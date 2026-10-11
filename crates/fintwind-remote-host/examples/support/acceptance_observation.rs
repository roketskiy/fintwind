//! Shared observation channel for the synthetic Windows process-ownership
//! black box. This module is **not** an example target: it lives under
//! `examples/support/` so Cargo's auto-discovery (`examples/*.rs` and
//! `examples/*/main.rs`) never picks it up, and both examples pull it in with
//! `#[path = "support/acceptance_observation.rs"] mod observation;`.
//!
//! What it exists to make possible, and nothing else: an outside observer
//! holding a **real handle** to every frozen descendant of the tested
//! supervisor, plus the supervisor's **exact job query handle**, so that
//! `IsProcessInJob(handle, exact_job)` and a bounded wait on each descendant
//! become evidence instead of inference.
//!
//! # Wire format
//!
//! One JSON object per line, terminated by `\n`. The writer appends the
//! newline; the reader never produces a message that did not end in one, so a
//! truncated write is held rather than guessed at. A buffer that grows past
//! `MAX_MESSAGE_BYTES` without a newline is a **protocol violation**: the
//! reader fails closed and reports it, it does not discard the bytes and
//! carry on.
//!
//! # Who may write where
//!
//! Every writer gets its **own pair** of anonymous pipes — one request
//! direction and one response direction — so two writers can never interleave
//! on one pipe. The supervisor writes on its pair; each observing fixture
//! writes on a pair the supervisor creates for it and registers with the
//! observer through a `FixtureChannel` hand-off.
//!
//! # Identity is structural, not claimed
//!
//! A message's writer is decided by which pipe it arrived on, never by
//! anything the message says about itself. The nonce correlates a round, it
//! does not authenticate a creator: a fixture that knows the nonce could
//! otherwise claim to be the supervisor. So every hand-off also carries the
//! writer's structural id and a per-writer monotonic transfer id, and
//! `verify_handoff` requires `GetProcessId(real_handle) == claimed_pid`.
//!
//! # Bounded IO
//!
//! Both directions run on a dedicated worker with an owned handle:
//!
//! * reads: a hard cap on the pending buffer, a bounded line queue, and a
//!   violation that fails closed;
//! * writes: a bounded inbox (`send_timeout`) plus a per-write completion
//!   rendezvous (`recv_timeout`). A message is at most `MAX_MESSAGE_BYTES`
//!   and at most one is outstanding per writer, so the only way a write can
//!   stall is a peer that stopped reading — and then the inbox fills and the
//!   send fails closed before any write is attempted.
//!
//! Handles are used through raw `ReadFile` / `WriteFile` and owned in exactly
//! one place each. There is no `File::from_raw_handle`, no `mem::forget`, and
//! therefore no path where a panic can close a handle twice.
//!
//! `finish` always runs: it stops the worker, waits a bounded time for the
//! worker to exit and close its own handle, and — only if that bound is
//! exceeded — closes the handle to cancel a stalled call and reports
//! `workerNotReaped`. Nothing is left dangling silently.
//!
//! Subject side (supervisor / fixture) — the API main integrates against:
//!
//! ```ignore
//! // once, at start-up, from the values the runner passed on the command line
//! let channel = observation::SubjectChannel::from_raw(request_write, response_read)?;
//! // per child, after AssignProcessToJobObject succeeded and BEFORE ResumeThread
//! let dup = observation::duplicate_for_observer(observer_process, child_process,
//!     observation::OBSERVER_PROCESS_ACCESS)?;
//! let ack = channel.handoff(&observation::Handoff {
//!     protocol: observation::PROTOCOL.to_owned(),
//!     nonce: nonce.clone(),
//!     source: observation::Source::Supervisor,
//!     fixture: None,
//!     transfer_id: next_id(),
//!     kind: observation::Kind::RootChild,
//!     claimed_pid: child_pid,
//!     process_handle: Some(dup as u64),
//!     job_handle: None,
//!     granted_access: "PROCESS_QUERY_LIMITED_INFORMATION|SYNCHRONIZE".to_owned(),
//! }, deadline)?;
//! if !ack.accepted { /* do NOT resume: terminate the child by its creation
//!                       handle, bounded-wait it, and report a startup failure */ }
//! ```
//!
//! Observer side — the API the black box's `--role observer` mode uses.

// Two binaries share this module and each exercises only its own half: the
// black box drives the observer end, the fixture drives the subject end, and
// main's supervisor integration will drive the subject end too. The unused
// half is therefore expected in every build, not dead weight.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::{
    CreateThread, GetCurrentProcess, GetProcessId, PROCESS_DUP_HANDLE,
    PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
};

use std::os::windows::io::FromRawHandle;

/// The pipe buffer this module asks for. One message is at most
/// `MAX_MESSAGE_BYTES`, and the protocol keeps at most one message outstanding
/// per direction per writer, so a write can never find the buffer full. The
/// size is requested explicitly rather than assumed.
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// Protocol tag. Version-independent on purpose: an unknown tag is rejected,
/// never silently reinterpreted.
pub const PROTOCOL: &str = "fintwind-ownership-observation/1";
/// A framed message, newline included, may not exceed this.
pub const MAX_MESSAGE_BYTES: usize = 2048;
/// How many complete lines a reader may hold before its consumer is considered
/// gone and the reader fails closed.
pub const MAX_QUEUED_LINES: usize = 64;
/// How many messages a writer may queue before its peer is considered gone.
pub const MAX_QUEUED_MESSAGES: usize = 4;
/// Nonce width in bytes. Never logged, never put in a handle list.
pub const NONCE_BYTES: usize = 16;
/// `SYNCHRONIZE` (winnt.h). windows-sys only exports it typed as
/// `FILE_ACCESS_RIGHTS` under the storage feature, so the documented value is
/// inlined here to keep the access mask a plain `u32`.
pub const SYNCHRONIZE: u32 = 0x0010_0000;
/// The access mask a handed-over process handle must carry: enough to query
/// the process and wait on it, and nothing that could end it.
pub const OBSERVER_PROCESS_ACCESS: u32 = PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE;
/// The access mask a handed-over observer handle must carry.
pub const OBSERVER_TARGET_ACCESS: u32 = PROCESS_DUP_HANDLE;

/// Which writer a message came from. Structural: it is decided by which pipe
/// the message arrived on, not by anything the writer says about itself.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Supervisor,
    Fixture,
}

/// The structural identity of one writer: the supervisor's own channel, or one
/// observing fixture's channel. The supervisor assigns the fixture id when it
/// creates that fixture's channel pair, so two fixtures never share a space.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WriterId {
    pub source: Source,
    /// `Some(id)` for a fixture channel, `None` for the supervisor's own.
    pub fixture: Option<u32>,
}

impl WriterId {
    pub fn supervisor() -> Self {
        Self {
            source: Source::Supervisor,
            fixture: None,
        }
    }

    pub fn fixture(id: u32) -> Self {
        Self {
            source: Source::Fixture,
            fixture: Some(id),
        }
    }
}

/// What a hand-off carries.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// A root child the supervisor created and contained.
    RootChild,
    /// A descendant an observing fixture created.
    Descendant,
    /// The supervisor's own job query handle.
    JobQuery,
    /// A channel pair the supervisor just created for one observing fixture.
    FixtureChannel,
    /// No further children will be created.
    TopologyFrozen,
}

/// Writer -> observer. Unknown fields are refused.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Handoff {
    pub protocol: String,
    pub nonce: String,
    pub source: Source,
    /// Which fixture channel this came from, for `Source::Fixture`.
    pub fixture: Option<u32>,
    /// Monotonic per writer. A repeat or a reorder is refused.
    pub transfer_id: u64,
    pub kind: Kind,
    /// The pid the writer believes the handle refers to. `verify_handoff`
    /// requires `GetProcessId(handle)` to equal it.
    pub claimed_pid: u32,
    /// A handle value valid in the **observer's** table.
    pub process_handle: Option<u64>,
    /// A `JOB_OBJECT_QUERY` handle value valid in the observer's table.
    pub job_handle: Option<u64>,
    /// The observer's own read end of one fixture's channel pair, valid in
    /// the observer's table. Labelled as a pipe end **explicitly**: a pipe
    /// has no pid, so it is never disguised as a process handle — the
    /// `GetProcessId` source check stays reserved for real process handles.
    #[serde(default)]
    pub pipe_request_read: Option<u64>,
    /// The observer's own write end for ACKs, same rules as above.
    #[serde(default)]
    pub pipe_response_write: Option<u64>,
    /// A label only. Never treated as proof of permissions.
    pub granted_access: String,
}

/// Observer -> writer.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ack {
    pub protocol: String,
    pub nonce: String,
    pub transfer_id: u64,
    pub accepted: bool,
    pub reason: String,
}

/// One pipe end, owned exactly once. Closing is RAII; `take` moves ownership
/// out (across a process boundary, or into a cancellation path).
#[derive(Default)]
pub struct End(Option<HANDLE>);

impl End {
    fn new(handle: HANDLE) -> Self {
        Self(Some(handle))
    }

    pub fn get(&self) -> Option<HANDLE> {
        self.0
    }

    pub fn take(&mut self) -> Option<HANDLE> {
        self.0.take()
    }
}

impl Drop for End {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            close_handle(handle);
        }
    }
}

fn close_handle(handle: HANDLE) {
    if !handle.is_null() {
        unsafe {
            let _ = CloseHandle(handle);
        }
    }
}

/// A raw handle is not `Send`. This wrapper lets the value cross into a worker
/// thread while ownership stays with the struct that spawned the thread, so
/// there is exactly one owner and exactly one close.
///
/// `into_inner` takes the whole wrapper, which is what forces a `move` closure
/// to capture the wrapper rather than its raw field: disjoint closure capture
/// would otherwise capture the bare `HANDLE`, which is not `Send`.
#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

impl SendHandle {
    fn into_inner(self) -> HANDLE {
        self.0
    }
}

fn remaining(deadline: Instant) -> Result<std::time::Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        bail!("the observation channel deadline passed");
    }
    Ok(left)
}

fn create_pipe() -> Result<(HANDLE, HANDLE)> {
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        windows_sys::Win32::System::Pipes::CreatePipe(&mut read, &mut write, std::ptr::null(), 0)
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not create an observation pipe");
    }
    Ok((read, write))
}

fn set_inheritable(handle: HANDLE) -> Result<()> {
    let ok = unsafe {
        windows_sys::Win32::Foundation::SetHandleInformation(
            handle,
            windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT,
            windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not mark an observation pipe end inheritable");
    }
    Ok(())
}

// ------------------------------------------------------------ bounded write

/// A worker thread with a real kernel handle, so its exit can be verified
/// against the kernel rather than inferred from a message.
///
/// The thread is created with `CreateThread`, which means this struct owns the
/// only handle that can wait on it, and `join_bounded` proves the thread has
/// left before anything it was using is touched again.
struct WorkerThread {
    handle: Option<HANDLE>,
    stop: Arc<AtomicBool>,
    finished: Receiver<()>,
}

extern "system" fn thread_entry(parameter: *mut core::ffi::c_void) -> u32 {
    // The closure was double-boxed so the pointer stays thin.
    let outer: Box<Box<dyn FnOnce() + Send>> = unsafe { Box::from_raw(parameter.cast()) };
    outer();
    0
}

impl WorkerThread {
    fn spawn<F>(stop: Arc<AtomicBool>, body: F) -> Result<Self>
    where
        F: FnOnce(Arc<AtomicBool>) + Send + 'static,
    {
        let (finished_tx, finished) = std::sync::mpsc::channel();
        let stop_for_thread = Arc::clone(&stop);
        let boxed: Box<dyn FnOnce() + Send> = Box::new(move || {
            body(stop_for_thread);
            let _ = finished_tx.send(());
        });
        let parameter = Box::into_raw(Box::new(boxed)) as *mut core::ffi::c_void;
        let handle = unsafe {
            CreateThread(
                std::ptr::null(),
                0,
                Some(thread_entry),
                parameter,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() {
            // The thread never started, so the box has to be reclaimed here.
            let _: Box<Box<dyn FnOnce() + Send>> = unsafe { Box::from_raw(parameter.cast()) };
            return Err(std::io::Error::last_os_error())
                .context("could not start an observation worker thread");
        }
        Ok(Self {
            handle: Some(handle),
            stop,
            finished,
        })
    }

    /// Ask the worker to stop, then prove it stopped.
    ///
    /// The proof is three-stage: the worker reports it has left its body;
    /// if it has not, a synchronous I/O call it may be stuck in — a write
    /// to a pipe nobody is draining — is cancelled through the **real
    /// thread handle** this struct owns, so the call returns and the worker
    /// can reach its own exit; and then the kernel confirms the thread
    /// object is signalled. Only after both do the caller and the worker
    /// itself let go of anything, so a handle value can never be reused
    /// while a thread is still inside a call on it.
    ///
    /// The cancellation never re-derives a handle from a thread id: the
    /// only handle used here is the one `CreateThread` returned. A worker
    /// that still has not left when the deadline passes is a reported
    /// failure, not a thread left behind silently.
    fn join_bounded(&mut self, deadline: Instant) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        self.cancel_stalled_io();
        if self.finished.recv_timeout(remaining(deadline)?).is_err() {
            // Stuck in a synchronous call. Cancel it; the worker's loop
            // then sees the failed call, reports the error, and leaves.
            self.cancel_stalled_io();
            if self.finished.recv_timeout(remaining(deadline)?).is_err() {
                bail!("an observation worker did not stop before the deadline");
            }
        }
        let handle = self
            .handle
            .take()
            .ok_or_else(|| anyhow::anyhow!("the observation worker handle is gone"))?;
        let wait = unsafe { WaitForSingleObject(handle, remaining(deadline)?.as_millis() as u32) };
        let exited = wait == WAIT_OBJECT_0;
        close_handle(handle);
        if !exited {
            bail!("the kernel did not confirm an observation worker exited");
        }
        Ok(())
    }

    /// Cancel a synchronous I/O call the worker is stuck in.
    ///
    /// Uses the thread handle `CreateThread` returned — the same one every
    /// wait in this struct uses — never a fresh handle opened from a thread
    /// id. For a thread that has already exited this is a harmless no-op;
    /// the exit itself is still verified afterwards.
    fn cancel_stalled_io(&self) {
        if let Some(handle) = self.handle {
            unsafe { CancelSynchronousIo(handle) };
        }
    }

    /// Whether the thread has already left its body.
    fn reported_finished(&self) -> bool {
        self.finished.try_recv().is_ok()
    }
}

impl Drop for WorkerThread {
    fn drop(&mut self) {
        // A worker that was never joined — a spawn-site error path, say —
        // still owns its I/O handle inside its own thread, and nothing here
        // closes anything the thread is using. Only the *thread* handle is
        // released, which gives up this struct's right to wait on the
        // thread without touching the thread itself: it is asked to stop
        // and reaches its own exit on its own.
        if let Some(handle) = self.handle.take() {
            self.stop.store(true, Ordering::SeqCst);
            unsafe { CancelSynchronousIo(handle) };
            close_handle(handle);
        }
    }
}

/// A write worker. The pipe end is owned by the thread that uses it, created
/// as a `File` inside that thread, so exactly one owner closes it exactly once.
struct WriteWorker {
    thread: WorkerThread,
    inbox: Option<SyncSender<Vec<u8>>>,
    progress: Receiver<Result<()>>,
}

impl WriteWorker {
    fn new(handle: HANDLE) -> Result<Self> {
        let (inbox, queue) = sync_channel::<Vec<u8>>(MAX_QUEUED_MESSAGES);
        let (progress_tx, progress) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        // The raw handle crosses into the thread inside the `SendHandle`
        // wrapper: ownership still moves exactly once, and the thread is the
        // only user of the value from here to its own exit.
        let handle = SendHandle(handle);
        let thread = WorkerThread::spawn(Arc::clone(&stop), move |stop| {
            let handle = handle.into_inner();
            // The worker owns the pipe end from here to thread exit.
            let mut stream = unsafe { std::fs::File::from_raw_handle(handle) };
            loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let bytes = match queue.recv_timeout(std::time::Duration::from_millis(20)) {
                    Ok(bytes) => bytes,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                let outcome = std::io::Write::write_all(&mut stream, &bytes)
                    .and_then(|_| std::io::Write::flush(&mut stream))
                    .context("could not write to the observation channel");
                let failed = outcome.is_err();
                let _ = progress_tx.send(outcome);
                if failed {
                    break;
                }
            }
            let _ = stop.load(Ordering::SeqCst);
        })?;
        Ok(Self {
            thread,
            inbox: Some(inbox),
            progress,
        })
    }

    /// Write one framed message, bounded end to end: the queue accepts it
    /// within the deadline, and the worker reports the real outcome within the
    /// deadline. A failed write is never reported as a success.
    fn write(&self, bytes: &[u8], deadline: Instant) -> Result<()> {
        let inbox = self
            .inbox
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("the observation write worker is finished"))?;
        send_with_deadline(
            inbox,
            bytes.to_vec(),
            deadline,
            "the observation write queue",
        )?;
        match self.progress.recv_timeout(remaining(deadline)?) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(RecvTimeoutError::Timeout) => {
                bail!("an observation write did not complete before the deadline")
            }
            Err(RecvTimeoutError::Disconnected) => {
                bail!("the observation write worker stopped before completing the write")
            }
        }
    }

    /// Stop the worker and prove it stopped. The worker owns the pipe end, so
    /// nothing is closed here until the kernel confirms the thread is gone.
    fn finish(mut self, deadline: Instant) -> Result<()> {
        self.inbox = None;
        self.thread.join_bounded(deadline)
    }
}

/// Bounded send into a sync channel. `SyncSender::send_timeout` is unstable, so
/// this polls `try_send` against a deadline and fails closed rather than
/// blocking forever on a consumer that is gone.
fn send_with_deadline<T>(
    sender: &SyncSender<T>,
    item: T,
    deadline: Instant,
    what: &str,
) -> Result<()> {
    let mut pending = item;
    loop {
        match sender.try_send(pending) {
            Ok(()) => return Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(returned)) => {
                if Instant::now() >= deadline {
                    bail!("{what} stopped draining before the deadline");
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
                pending = returned;
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                bail!("{what} has no consumer left");
            }
        }
    }
}

// ------------------------------------------------------------- bounded read

/// What a reader thread reports.
enum ReaderEvent {
    Line(String),
    /// A protocol violation: the reader has failed closed and stopped.
    Violation(String),
}

/// One event a reader produced, tagged with the writer whose pipe it came
/// from. The tag is set when the reader is spawned, from the pipe's structural
/// identity, never from anything a message says.
struct Tagged {
    id: WriterId,
    event: ReaderEvent,
}

/// How many bytes a reader may take from the pipe in one call. Small enough
/// that a single batch can never push the pending buffer past the cap in a way
/// that has to be undone, large enough to be efficient.
const READ_CHUNK_BYTES: usize = 512;

/// A read worker that never blocks.
///
/// The worker owns the pipe end, created as a `File` inside the thread. It
/// asks the kernel how many bytes are available with `PeekNamedPipe` and reads
/// at most that many, so a read can never wait for a peer that never writes.
/// The only loop that ever waits is the poll between checks, and that poll
/// watches a stop flag, so the thread leaves within a couple of milliseconds
/// of being asked.
struct LineReader {
    thread: WorkerThread,
}

impl LineReader {
    fn spawn(id: WriterId, handle: HANDLE, sender: SyncSender<Tagged>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = SendHandle(handle);
        let thread = WorkerThread::spawn(Arc::clone(&stop), move |stop| {
            let handle = handle.into_inner();
            let mut stream = unsafe { std::fs::File::from_raw_handle(handle) };
            let mut pending: Vec<u8> = Vec::new();
            loop {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let available = match peek_available(handle) {
                    Peek::Data(bytes) => bytes,
                    Peek::Closed => return,
                    Peek::Idle => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                };
                // Take at most the smaller of what is available and the chunk
                // size, and never more than the cap allows for, so a batch can
                // not momentarily carry the buffer past its limit.
                let room = MAX_MESSAGE_BYTES.saturating_sub(pending.len());
                if room == 0 {
                    let _ = send_with_deadline(
                        &sender,
                        Tagged {
                            id,
                            event: ReaderEvent::Violation(
                                "an observation writer sent more than the frame limit without a newline"
                                    .to_owned(),
                            ),
                        },
                        Instant::now() + std::time::Duration::from_secs(60),
                        "the observation read queue",
                    );
                    return;
                }
                let want = (available as usize).min(READ_CHUNK_BYTES).min(room);
                let mut buffer = [0_u8; READ_CHUNK_BYTES];
                let read = match std::io::Read::read(&mut stream, &mut buffer[..want]) {
                    Ok(0) => return,
                    Ok(read) => read,
                    Err(_) => return,
                };
                pending.extend_from_slice(&buffer[..read]);
                // Emit every complete line. A message that never ends in a
                // newline is never emitted.
                while let Some(position) = pending.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = pending.drain(..=position).collect();
                    let event = if line.len() > MAX_MESSAGE_BYTES {
                        ReaderEvent::Violation(
                            "an observation message exceeded the frame limit".to_owned(),
                        )
                    } else {
                        match String::from_utf8(line) {
                            Ok(text) => ReaderEvent::Line(text),
                            Err(_) => ReaderEvent::Violation(
                                "an observation message was not valid UTF-8".to_owned(),
                            ),
                        }
                    };
                    let violation = matches!(event, ReaderEvent::Violation(_));
                    let deadline = Instant::now() + std::time::Duration::from_secs(60);
                    if send_with_deadline(
                        &sender,
                        Tagged { id, event },
                        deadline,
                        "the observation read queue",
                    )
                    .is_err()
                    {
                        return;
                    }
                    if violation {
                        return;
                    }
                }
            }
        })?;
        Ok(Self { thread })
    }

    /// Stop the worker and prove it stopped before anything is closed.
    fn finish(mut self, deadline: Instant) -> Result<()> {
        self.thread.join_bounded(deadline)
    }
}

/// What a peek at a read end reports.
enum Peek {
    /// This many bytes are available, so a read of at most that many cannot
    /// block.
    Data(u32),
    /// No data yet.
    Idle,
    /// The writer let go of its end.
    Closed,
}

fn peek_available(handle: HANDLE) -> Peek {
    let mut available: u32 = 0;
    let ok = unsafe {
        PeekNamedPipe(
            handle,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        if error == ERROR_BROKEN_PIPE {
            return Peek::Closed;
        }
        return Peek::Closed;
    }
    if available == 0 {
        Peek::Idle
    } else {
        Peek::Data(available)
    }
}
// ---------------------------------------------------------------- channels

/// The four ends of one writer's channel pair, before they are split. Two
/// anonymous pipes: one request direction and one response direction, so an
/// ACK can travel back and a single unidirectional pipe never has to.
#[allow(dead_code)]
pub struct Channel {
    request_read: End,
    request_write: End,
    response_read: End,
    response_write: End,
}

impl Channel {
    /// Two anonymous pipes. Nothing here enters a named object namespace, so
    /// no other process in the session can join either direction.
    pub fn create() -> Result<Self> {
        let (request_read, request_write) = create_pipe()?;
        let (response_read, response_write) = match create_pipe() {
            Ok(pair) => pair,
            Err(error) => {
                close_handle(request_read);
                close_handle(request_write);
                return Err(error);
            }
        };
        Ok(Self {
            request_read: End::new(request_read),
            request_write: End::new(request_write),
            response_read: End::new(response_read),
            response_write: End::new(response_write),
        })
    }

    /// Mark the writer's two ends inheritable so they can be placed in a
    /// child's `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
    pub fn mark_subject_ends_inheritable(&self) -> Result<()> {
        if let Some(handle) = self.request_write.get() {
            set_inheritable(handle)?;
        }
        if let Some(handle) = self.response_read.get() {
            set_inheritable(handle)?;
        }
        Ok(())
    }

    /// Mark the observer's two ends inheritable.
    pub fn mark_observer_ends_inheritable(&self) -> Result<()> {
        if let Some(handle) = self.request_read.get() {
            set_inheritable(handle)?;
        }
        if let Some(handle) = self.response_write.get() {
            set_inheritable(handle)?;
        }
        Ok(())
    }

    pub fn request_write(&self) -> Option<HANDLE> {
        self.request_write.get()
    }

    pub fn response_read(&self) -> Option<HANDLE> {
        self.response_read.get()
    }

    pub fn request_read(&self) -> Option<HANDLE> {
        self.request_read.get()
    }

    pub fn response_write(&self) -> Option<HANDLE> {
        self.response_write.get()
    }
}

// ------------------------------------------------------------ subject side

/// What the writer (supervisor or observing fixture) keeps. This is the API
/// main integrates against; nothing in this repository calls it until that
/// lands, which is why it is marked as an intentional integration surface.
#[allow(dead_code)]
pub struct SubjectChannel {
    id: WriterId,
    writer: WriteWorker,
    reader: LineReader,
    inbox: Receiver<Tagged>,
}

#[allow(dead_code)]
impl SubjectChannel {
    /// Rebuild from values that arrived by inheritance. This is the only way
    /// the writer ever obtains them: nobody opens a pid to get them.
    pub fn from_raw(id: WriterId, request_write: HANDLE, response_read: HANDLE) -> Result<Self> {
        if request_write.is_null() || response_read.is_null() {
            bail!("an observation channel end is null");
        }
        let (sender, inbox) = sync_channel::<Tagged>(MAX_QUEUED_LINES);
        Ok(Self {
            id,
            writer: WriteWorker::new(request_write)?,
            reader: LineReader::spawn(id, response_read, sender)?,
            inbox,
        })
    }

    pub fn id(&self) -> WriterId {
        self.id
    }

    /// Write one hand-off and wait, bounded, for its ACK.
    pub fn handoff(&self, message: &Handoff, deadline: Instant) -> Result<Ack> {
        let line = frame(message)?;
        self.writer.write(&line, deadline)?;
        let text = self
            .next_ack_line(deadline)?
            .ok_or_else(|| anyhow::anyhow!("the observation response pipe closed"))?;
        let ack: Ack = parse(&text).context("the observation ACK is not a well-formed message")?;
        if ack.protocol != PROTOCOL {
            bail!("the observation ACK carries an unknown protocol tag");
        }
        if ack.nonce != message.nonce {
            bail!("the observation ACK does not carry this round's nonce");
        }
        if ack.transfer_id != message.transfer_id {
            bail!("the observation ACK does not answer this transfer");
        }
        Ok(ack)
    }

    /// Bounded wait for the next framed line on this writer's own response
    /// pipe. `Ok(None)` on a clean end of stream; `Err` on a violation or the
    /// deadline.
    fn next_ack_line(&self, deadline: Instant) -> Result<Option<String>> {
        match self.inbox.recv_timeout(remaining(deadline)?) {
            Ok(tagged) => {
                if tagged.id != self.id {
                    bail!("a response arrived on a channel that belongs to another writer");
                }
                match tagged.event {
                    ReaderEvent::Line(text) => Ok(Some(text)),
                    ReaderEvent::Violation(reason) => bail!("{reason}"),
                }
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    /// Stop both workers and reap them within a bound.
    pub fn finish(self, deadline: Instant) -> Result<()> {
        self.reader.finish(deadline)?;
        self.writer.finish(deadline)
    }
}

/// Frame one message as a single newline-terminated line.
fn frame<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let mut line =
        serde_json::to_vec(message).context("could not encode an observation message")?;
    if line.len() + 1 > MAX_MESSAGE_BYTES {
        bail!("an observation message exceeds the frame limit");
    }
    if line.contains(&b'\n') {
        bail!("an observation message must not contain a newline");
    }
    line.push(b'\n');
    Ok(line)
}

fn parse<T: for<'a> Deserialize<'a>>(text: &str) -> Result<T> {
    let text = text.trim_end_matches(['\n', '\r']);
    if text.len() > MAX_MESSAGE_BYTES {
        bail!("an observation message exceeds the frame limit");
    }
    serde_json::from_str(text).context("an observation message is not well-formed JSON")
}

// ----------------------------------------------------------- observer side

/// The observer's side of every channel at once.
///
/// Writers register dynamically: the supervisor registers its own pair, and
/// each observing fixture's pair is registered when the supervisor announces
/// it with a `FixtureChannel` hand-off. Every registration starts its own
/// reader, so a new descendant message is never left without one.
pub struct ObserverHub {
    inbox: Receiver<Tagged>,
    sender: SyncSender<Tagged>,
    readers: Vec<(WriterId, LineReader)>,
    responses: Vec<(WriterId, WriteWorker)>,
    /// Highest transfer id seen per writer, so a repeat or reorder is refused.
    seen: BTreeMap<WriterId, u64>,
    /// Job query handle copies, closed before the crash phase.
    job_copies: Vec<HANDLE>,
}

impl ObserverHub {
    pub fn new() -> Self {
        let (sender, inbox) = sync_channel::<Tagged>(MAX_QUEUED_LINES);
        Self {
            inbox,
            sender,
            readers: Vec::new(),
            responses: Vec::new(),
            seen: BTreeMap::new(),
            job_copies: Vec::new(),
        }
    }

    /// Register one writer's channel pair. The ends are the observer's own,
    /// obtained by inheritance or by duplication from the supervisor, and the
    /// id is the pipe's structural identity. Every registration starts its own
    /// reader, so a newly registered writer's messages are never left without
    /// one.
    ///
    /// Fails closed: a registration whose reader or writer cannot start is an
    /// error, never a half-registered channel that swallows hand-offs.
    pub fn register(
        &mut self,
        id: WriterId,
        request_read: HANDLE,
        response_write: HANDLE,
    ) -> Result<()> {
        let reader = LineReader::spawn(id, request_read, self.sender.clone())?;
        let writer = WriteWorker::new(response_write)?;
        self.readers.push((id, reader));
        self.responses.push((id, writer));
        Ok(())
    }

    /// Bounded wait for the next hand-off from any registered writer.
    pub fn next(&self, deadline: Instant) -> Result<Option<(WriterId, Handoff)>> {
        match self.inbox.recv_timeout(remaining(deadline)?) {
            Ok(tagged) => {
                let text = match tagged.event {
                    ReaderEvent::Line(text) => text,
                    ReaderEvent::Violation(reason) => bail!("{reason}"),
                };
                let message: Handoff =
                    parse(&text).context("a hand-off is not a well-formed message")?;
                if message.protocol != PROTOCOL {
                    bail!("a hand-off carries an unknown protocol tag");
                }
                let id = WriterId {
                    source: message.source,
                    fixture: if message.kind == Kind::FixtureChannel {
                        None
                    } else {
                        message.fixture
                    },
                };
                if id != tagged.id {
                    bail!("a hand-off arrived on a channel that belongs to another writer");
                }
                Ok(Some((id, message)))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    /// Record one job query handle copy so it can be closed before the crash.
    pub fn keep_job_copy(&mut self, handle: HANDLE) {
        if !handle.is_null() {
            self.job_copies.push(handle);
        }
    }

    /// Close every job query handle copy. Must happen, and be acknowledged,
    /// before the crash phase: holding one would delay the tested job's
    /// kill-on-close and invalidate the scenario.
    pub fn close_job_copies(&mut self) {
        for handle in self.job_copies.drain(..) {
            close_handle(handle);
        }
    }

    /// Write one ACK back to the writer that sent this hand-off.
    pub fn ack(
        &self,
        id: &WriterId,
        message: &Handoff,
        accepted: bool,
        reason: &str,
        deadline: Instant,
    ) -> Result<()> {
        let (_, writer) = self
            .responses
            .iter()
            .find(|(candidate, _)| candidate == id)
            .ok_or_else(|| anyhow::anyhow!("no response channel is registered for this writer"))?;
        writer.write(&frame(&ack(message, accepted, reason))?, deadline)
    }

    /// Stop every worker, close every job copy, and reap the threads within a
    /// bound. A job copy that survives here would delay the tested job's
    /// kill-on-close, so this is the last-resort close, not the normal path:
    /// the normal path is `close_job_copies` followed by an ACK, before the
    /// crash phase.
    pub fn finish(mut self, deadline: Instant) -> Result<()> {
        let mut first_error = None;
        let readers = std::mem::take(&mut self.readers);
        for (_, reader) in readers {
            if let Err(error) = reader.finish(deadline) {
                first_error.get_or_insert(error);
            }
        }
        let responses = std::mem::take(&mut self.responses);
        for (_, writer) in responses {
            if let Err(error) = writer.finish(deadline) {
                first_error.get_or_insert(error);
            }
        }
        self.close_job_copies();
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Default for ObserverHub {
    fn default() -> Self {
        Self::new()
    }
}

// -------------------------------------------------------------- primitives

/// The single auditable hand-off site. The access mask is enforced **here**,
/// in this file; no message can widen it and no message needs to prove it.
pub fn duplicate_for_observer(
    target_process: HANDLE,
    source: HANDLE,
    access: u32,
) -> Result<HANDLE> {
    if target_process.is_null() || source.is_null() {
        bail!("cannot duplicate a null handle for the observer");
    }
    let mut duplicated: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        windows_sys::Win32::Foundation::DuplicateHandle(
            GetCurrentProcess(),
            source,
            target_process,
            &mut duplicated,
            access,
            0,
            if access == 0 {
                windows_sys::Win32::Foundation::DUPLICATE_SAME_ACCESS
            } else {
                0
            },
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not duplicate a handle for the observer");
    }
    Ok(duplicated)
}

/// Duplicate a handle **within this process**, narrowing it to exactly
/// `access`. This is how the runner turns the observer's creation handle into
/// the `PROCESS_DUP_HANDLE`-only value the subject inherits.
pub fn dup_handle_with_access(source: HANDLE, access: u32) -> Result<HANDLE> {
    duplicate_for_observer(unsafe { GetCurrentProcess() }, source, access)
}

/// Duplicate one end of a channel pair for the observer.
///
/// The same single auditable hand-off site as every other duplication. A
/// pipe end needs no narrower mask — its access is whatever the source end
/// already carries — and the hand-off's explicit pipe fields are what keep
/// a pipe end from ever being confused with a process or job handle.
pub fn duplicate_pipe_end_for_observer(target_process: HANDLE, source: HANDLE) -> Result<HANDLE> {
    duplicate_for_observer(target_process, source, 0)
}

/// A per-round nonce. It is not a handle, never enters a `HANDLE_LIST`, and is
/// never written to any log or report.
pub fn nonce() -> String {
    let mut bytes = [0_u8; NONCE_BYTES];
    let mut state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0)
        ^ ((std::process::id() as u64) << 32);
    for byte in bytes.iter_mut() {
        // xorshift64*: enough entropy for a per-round correlation token, and
        // it keeps this module free of extra dependencies.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        *byte = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
    }
    let mut text = String::with_capacity(NONCE_BYTES * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// A hand-off the observer has accepted: the handle is real, its pid matches
/// the claim, and its transfer id is unique for that writer.
pub struct VerifiedHandoff {
    pub id: WriterId,
    pub kind: Kind,
    /// From `GetProcessId` on the real handle, never from the message.
    pub pid: u32,
    pub process: Option<HANDLE>,
    pub job: Option<HANDLE>,
    /// A label only. Never treated as proof of permissions.
    pub granted_access_label: String,
}

/// Verify provenance. A nonce alone does not authenticate a creator: a
/// fixture that knows the nonce could otherwise claim to be the supervisor.
/// The structural channel identity plus `GetProcessId` is what does.
pub fn verify_handoff(
    message: &Handoff,
    expected_nonce: &str,
    seen: &mut BTreeMap<WriterId, u64>,
) -> Result<VerifiedHandoff> {
    if message.nonce != expected_nonce {
        bail!("a hand-off does not carry this round's nonce");
    }
    let id = WriterId {
        source: message.source,
        fixture: if message.kind == Kind::FixtureChannel {
            None
        } else {
            message.fixture
        },
    };
    let last = seen.entry(id).or_insert(0);
    if message.transfer_id <= *last {
        bail!("a hand-off repeats or reorders a transfer id");
    }
    *last = message.transfer_id;

    let process = match message.process_handle {
        Some(value) => {
            let handle = value as HANDLE;
            if handle.is_null() {
                bail!("a hand-off carries a null process handle");
            }
            // The ownership proof: the pid the real handle reports must equal
            // the pid the writer claimed.
            let actual = unsafe { GetProcessId(handle) };
            if actual == 0 {
                bail!("a handed-over process handle does not name a process");
            }
            if actual != message.claimed_pid {
                bail!("a handed-over process handle names a different pid than claimed");
            }
            Some(handle)
        }
        None => None,
    };
    let job = match message.job_handle {
        Some(value) => {
            let handle = value as HANDLE;
            if handle.is_null() {
                bail!("a hand-off carries a null job handle");
            }
            Some(handle)
        }
        None => None,
    };
    Ok(VerifiedHandoff {
        id,
        kind: message.kind,
        pid: message.claimed_pid,
        process,
        job,
        granted_access_label: message.granted_access.clone(),
    })
}

/// Build an ACK for one transfer.
#[allow(dead_code)]
pub fn ack(message: &Handoff, accepted: bool, reason: impl Into<String>) -> Ack {
    Ack {
        protocol: PROTOCOL.to_owned(),
        nonce: message.nonce.clone(),
        transfer_id: message.transfer_id,
        accepted,
        reason: reason.into(),
    }
}

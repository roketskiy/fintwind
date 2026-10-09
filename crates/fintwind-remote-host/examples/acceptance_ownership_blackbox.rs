//! Synthetic Windows process-ownership black box for the acceptance supervisor.
//!
//! This is the containment check described in
//! `scripts/fixtures/remote-control/README.md` ("Synthetic process-ownership
//! black box") and designed in `scripts/fixtures/remote-control/process-ownership-plan.md`.
//! It runs **no provider, no daemon and no browser**.
//!
//! Scope, stated up front because it limits every claim this file can make:
//!
//! * the subject under test is the external binary `acceptance_supervisor`
//!   (`crates/fintwind-remote-host/examples/acceptance_supervisor.rs`). It is
//!   spawned, observed and torn down through the creation handle this runner
//!   owns. It is never modified, never placed inside one of this runner's jobs
//!   (that would let an outer job contain the tested subtree and mask the
//!   tested job's own behaviour), and never referenced as a service;
//! * every process this runner touches is one it created itself this round,
//!   held by a `CreateProcessW` handle. There is **no** `OpenProcess(pid)`, no
//!   `taskkill`, no kill by name / port / command line / directory, and no
//!   machine-wide scan. Cleanup has exactly two legal exits: `TerminateProcess`
//!   on a handle created this round, or `TerminateJobObject` on a job this
//!   round created and owns;
//! * a grandchild's handle can only be handed over by the process that created
//!   it, so the trusted fixture duplicates it into this runner (see
//!   `acceptance_ownership_fixture.rs`);
//! * raw child stdout/stderr is piped, counted in memory and discarded. Only
//!   structured facts are parsed, and nothing but synthetic identifiers ever
//!   reaches disk;
//! * `fullPlanVerified` is always `false`. Two checks are design-blocked and
//!   reported as such instead of being faked; see the plan document.
//!
//! The default observer is this same binary re-executed in `--role observer`
//! mode: a separate process, outside every job, that receives real handles by
//! inheritance and does its own kernel queries and bounded waits. A reviewer
//! who wants a different observer can run their own against the same facts
//! files; nothing here depends on the observer being this binary.
//!
//! Usage:
//!   acceptance_ownership_blackbox --supervisor <exe> --fixture <exe> \
//!     --workdir <dir> [--examples-dir <dir>] [--out <report.json>] \
//!     [--scenario NAME]... [--list]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context as _, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The shared observation channel. It lives under xamples/support/ so Cargo
/// never auto-discovers it as a runnable example.
#[cfg(windows)]
#[path = "support/acceptance_observation.rs"]
mod observation;

#[cfg(not(windows))]
fn main() {
    eprintln!("the process-ownership black box requires Windows job containment");
    std::process::exit(3);
}

/// Every Win32 call this file makes, and nothing that is not already enabled
/// in this crate's `windows-sys` feature set.
#[cfg(windows)]
mod win {
    use std::collections::BTreeMap;
    use std::os::windows::io::FromRawHandle as _;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use anyhow::{bail, Context as _, Result};

    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
        JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetCurrentProcessId,
        GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, PROCESS_DUP_HANDLE, ResumeThread,
        STARTUPINFOEXW, STARTF_USESTDHANDLES, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
        EXTENDED_STARTUPINFO_PRESENT, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject,
    };

    pub const WAIT_OBJECT_0: u32 = 0x0000_0000;
    pub const WAIT_TIMEOUT: u32 = 0x0000_0102;
    /// `STILL_ACTIVE` (winbase.h): the exit code of a process that has not
    /// exited. Inlined because windows-sys types it as `NTSTATUS`.
    pub const STILL_ACTIVE_CODE: u32 = 259;

    /// A handle this process owns. Every value of this type in this file comes
    /// from `CreateProcessW`, `CreateJobObjectW`, `DuplicateHandle` or
    /// inheritance at creation — never from `OpenProcess`.
    pub type Handle = HANDLE;

    pub fn current_process() -> HANDLE {
        unsafe { GetCurrentProcess() }
    }

    pub fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Windows command-line quoting: backslashes before a quote are doubled,
    /// quotes are escaped, and the argument is always wrapped in quotes.
    fn quote(argument: &str) -> String {
        let mut quoted = String::with_capacity(argument.len() + 3);
        quoted.push('"');
        let mut backslashes = 0_usize;
        for character in argument.chars() {
            if character == '\\' {
                backslashes += 1;
                quoted.push('\\');
            } else if character == '"' {
                for _ in 0..=backslashes {
                    quoted.push('\\');
                }
                quoted.push('"');
                backslashes = 0;
            } else {
                backslashes = 0;
                quoted.push(character);
            }
        }
        for _ in 0..backslashes {
            quoted.push('\\');
        }
        quoted.push('"');
        quoted
    }

    fn environment_block(env: &BTreeMap<String, String>) -> Vec<u16> {
        let mut block: Vec<u16> = Vec::new();
        for (name, value) in env {
            block.extend(format!("{name}={value}").encode_utf16());
            block.push(0);
        }
        block.push(0);
        block
    }

    // ---------------------------------------------------------------- jobs

    /// A job this round created and owns. Dropping it closes the handle, which
    /// is the kernel's own teardown for a `KILL_ON_JOB_CLOSE` job — but the
    /// runner always proves exit with explicit bounded waits first.
    pub struct Job {
        handle: HANDLE,
    }

    // SAFETY: job object handles are usable from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> Result<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(std::io::Error::last_os_error())
                        .context("could not create the runner's job object");
                }
                let job = Self { handle };
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job.handle,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error())
                        .context("could not configure the runner's job object");
                }
                Ok(job)
            }
        }

        /// The runner's own exclusive job for this round, containing the
        /// outsider. The observer receives this handle by inheritance.
        pub fn handle(&self) -> HANDLE {
            self.handle
        }

        /// Assign by a creation handle — never by opening a pid.
        pub fn assign(&self, process: HANDLE) -> Result<()> {
            unsafe {
                if AssignProcessToJobObject(self.handle, process) == 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("could not assign a controlled child to the runner's job");
                }
                Ok(())
            }
        }

        pub fn terminate(&self) -> Result<()> {
            unsafe {
                if TerminateJobObject(self.handle, 1) == 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("could not terminate the runner's job");
                }
                Ok(())
            }
        }

        /// The kernel's own membership list for this job. This is what makes a
        /// "same job" claim independent of any process's self-report.
        pub fn process_ids(&self) -> Result<Vec<u32>> {
            // 512 pid slots is far beyond anything this black box builds.
            let mut storage: Vec<usize> = vec![0; 8 + 512];
            let mut returned: u32 = 0;
            let ok = unsafe {
                QueryInformationJobObject(
                    self.handle,
                    JobObjectBasicProcessIdList,
                    storage.as_mut_ptr().cast(),
                    (storage.len() * std::mem::size_of::<usize>()) as u32,
                    &mut returned,
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("could not read the job's process id list");
            }
            let list = unsafe { &*(storage.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()) };
            let count = list.NumberOfProcessIdsInList as usize;
            let mut pids = Vec::with_capacity(count.min(512));
            for index in 0..count.min(512) {
                pids.push(unsafe { *list.ProcessIdList.as_ptr().add(index) } as u32);
            }
            Ok(pids)
        }

        /// True when the job really carries `KILL_ON_JOB_CLOSE`. Only readable
        /// by a handle owner, which is why the supervisor's own flag cannot be
        /// observed today.
        pub fn kill_on_job_close_set(&self) -> Result<bool> {
            unsafe {
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                let mut returned: u32 = 0;
                let ok = QueryInformationJobObject(
                    self.handle,
                    JobObjectExtendedLimitInformation,
                    &mut limits as *mut _ as *mut _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    &mut returned,
                );
                if ok == 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("could not read the job's extended limits");
                }
                Ok(limits.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                    != 0)
            }
        }

        pub fn active_processes_is_zero(&self) -> Result<bool> {
            unsafe {
                let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
                let ok = QueryInformationJobObject(
                    self.handle,
                    JobObjectBasicAccountingInformation,
                    &mut accounting as *mut _ as *mut _,
                    std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                );
                if ok == 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("could not read the job's accounting");
                }
                Ok(accounting.ActiveProcesses == 0)
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }

    // ------------------------------------------------------------- handles

    /// `close` guards a null, so a nulled end is skipped by `Pipes::drop`.
    pub fn close(handle: HANDLE) {
        if !handle.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }

    pub fn set_inheritable(handle: HANDLE) -> Result<()> {
        unsafe {
            if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("could not mark a handle inheritable");
            }
            Ok(())
        }
    }

    /// A real, inheritable handle to this process, so a child it creates can
    /// hand a handle back without anybody opening a pid.
    pub fn inheritable_self_handle() -> Result<HANDLE> {
        let mut handle: HANDLE = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                GetCurrentProcess(),
                GetCurrentProcess(),
                &mut handle,
                PROCESS_DUP_HANDLE,
                1,
                0,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not create an inheritable self handle");
        }
        Ok(handle)
    }

    pub fn is_in_job(process: HANDLE, job: HANDLE) -> Result<bool> {
        let mut result: i32 = 0;
        let ok = unsafe { IsProcessInJob(process, job, &mut result) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not query a process's job membership");
        }
        Ok(result != 0)
    }

    // -------------------------------------------------------- wait helpers

    /// Wait a bounded time on a handle this process owns; `None` on timeout.
    pub fn wait_handle(handle: HANDLE, timeout_ms: u32) -> Result<Option<u32>> {
        unsafe {
            let wait = WaitForSingleObject(handle, timeout_ms);
            if wait == WAIT_OBJECT_0 {
                Ok(Some(exit_code(handle)?))
            } else if wait == WAIT_TIMEOUT {
                Ok(None)
            } else {
                Err(std::io::Error::last_os_error()).context("could not wait on a handle")
            }
        }
    }

    pub fn exit_code(handle: HANDLE) -> Result<u32> {
        unsafe {
            let mut code: u32 = 0;
            if GetExitCodeProcess(handle, &mut code) == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("could not read an exit code");
            }
            Ok(code)
        }
    }

    /// Alive means "has not exited": `STILL_ACTIVE`, read from a handle this
    /// process owns. Never a heartbeat, never a name.
    pub fn still_active(handle: HANDLE) -> bool {
        exit_code(handle).map(|code| code == STILL_ACTIVE_CODE).unwrap_or(false)
    }

    pub fn current_process_id() -> u32 {
        unsafe { GetCurrentProcessId() }
    }

    /// The kernel's own membership list for a job handle this process holds.
    /// Used by the observer to cross-check a frozen topology against the real
    /// handles it was handed.
    pub fn job_process_ids(job: HANDLE) -> Result<Vec<u32>> {
        let mut storage: Vec<usize> = vec![0; 8 + 512];
        let mut returned: u32 = 0;
        let ok = unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicProcessIdList,
                storage.as_mut_ptr().cast(),
                (storage.len() * std::mem::size_of::<usize>()) as u32,
                &mut returned,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not read the job's process id list");
        }
        let list = unsafe { &*(storage.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()) };
        let count = list.NumberOfProcessIdsInList as usize;
        Ok((0..count.min(512))
            .map(|index| unsafe { *list.ProcessIdList.as_ptr().add(index) } as u32)
            .collect())
    }

    // ------------------------------------------------------------- spawning

    /// The opaque attribute list needs suitably aligned storage, alive through
    /// creation. Mirrors the supervisor's own construction.
    struct HandleAttributes {
        storage: Vec<usize>,
    }

    impl HandleAttributes {
        fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.storage.as_mut_ptr().cast()
        }

        fn new(handles: &mut [HANDLE]) -> Result<Self> {
            let mut size = 0_usize;
            unsafe {
                InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
            }
            if size == 0 {
                bail!("could not size the handle attribute list");
            }
            let mut list = Self {
                storage: vec![0; size.div_ceil(std::mem::size_of::<usize>())],
            };
            unsafe {
                if InitializeProcThreadAttributeList(list.pointer(), 1, 0, &mut size) == 0 {
                    list.storage.clear();
                    return Err(std::io::Error::last_os_error())
                        .context("could not initialize the handle attribute list");
                }
                if UpdateProcThreadAttribute(
                    list.pointer(),
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_mut_ptr().cast(),
                    std::mem::size_of_val(handles),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) == 0
                {
                    return Err(std::io::Error::last_os_error())
                        .context("could not restrict handle inheritance");
                }
            }
            Ok(list)
        }
    }

    impl Drop for HandleAttributes {
        fn drop(&mut self) {
            if !self.storage.is_empty() {
                unsafe { DeleteProcThreadAttributeList(self.pointer()) };
            }
        }
    }

    struct Pipes {
        /// The parent's ends.
        stdout_read: HANDLE,
        stderr_read: HANDLE,
        /// The child's ends, closed in the parent right after creation.
        stdin_read: HANDLE,
        stdout_write: HANDLE,
        stderr_write: HANDLE,
        /// Closed by the parent immediately: nothing here ever writes stdin.
        stdin_write: HANDLE,
    }

    impl Drop for Pipes {
        fn drop(&mut self) {
            // Every end still owned by this struct is closed exactly once.
            // Ends whose ownership has been handed over — closed outright by
            // the spawn path, or moved into a drain thread through
            // `File::from_raw_handle` — are nulled by the taker, so a
            // hand-off, an explicit close, and this drop can never fire on
            // the same value. The null guard is what stands between a
            // double close and a handle-value reuse: a value closed twice
            // can land on a freshly created thread or process handle, and
            // the second close then ends something alive that this runner
            // never meant to touch.
            for handle in [
                self.stdout_read,
                self.stderr_read,
                self.stdin_read,
                self.stdout_write,
                self.stderr_write,
                self.stdin_write,
            ] {
                close(handle);
            }
        }
    }

    fn create_pipes() -> Result<Pipes> {
        unsafe {
            let mut stdin_read: HANDLE = std::ptr::null_mut();
            let mut stdin_write: HANDLE = std::ptr::null_mut();
            let mut stdout_read: HANDLE = std::ptr::null_mut();
            let mut stdout_write: HANDLE = std::ptr::null_mut();
            let mut stderr_read: HANDLE = std::ptr::null_mut();
            let mut stderr_write: HANDLE = std::ptr::null_mut();
            if CreatePipe(&mut stdin_read, &mut stdin_write, std::ptr::null(), 0) == 0 {
                return Err(std::io::Error::last_os_error()).context("could not create a pipe");
            }
            if CreatePipe(&mut stdout_read, &mut stdout_write, std::ptr::null(), 0) == 0 {
                close(stdin_read);
                close(stdin_write);
                return Err(std::io::Error::last_os_error()).context("could not create a pipe");
            }
            if CreatePipe(&mut stderr_read, &mut stderr_write, std::ptr::null(), 0) == 0 {
                close(stdin_read);
                close(stdin_write);
                close(stdout_read);
                close(stdout_write);
                return Err(std::io::Error::last_os_error()).context("could not create a pipe");
            }
            for handle in [stdin_read, stdout_write, stderr_write] {
                if let Err(error) = set_inheritable(handle) {
                    for handle in [
                        stdin_read,
                        stdin_write,
                        stdout_read,
                        stdout_write,
                        stderr_read,
                        stderr_write,
                    ] {
                        close(handle);
                    }
                    return Err(error);
                }
            }
            Ok(Pipes {
                stdout_read,
                stderr_read,
                stdin_read,
                stdout_write,
                stderr_write,
                stdin_write,
            })
        }
    }

    /// One child this runner created, held for its whole lifetime. The handles
    /// come from `CreateProcessW` itself, so ownership never depends on a pid.
    pub struct Child {
        pub pid: u32,
        pub process: HANDLE,
        pub thread: HANDLE,
        pub stdout_bytes: Arc<AtomicUsize>,
        pub stderr_bytes: Arc<AtomicUsize>,
        drains: Vec<std::thread::JoinHandle<()>>,
    }

    impl Child {
        /// Bounded wait for the exit, then read the code. `None` on timeout.
        pub fn wait(&self, timeout_ms: u32) -> Result<Option<u32>> {
            wait_handle(self.process, timeout_ms)
        }

        /// Terminate through the creation handle — one of this runner's two
        /// legal cleanup exits — and nothing else. The caller owns the
        /// bounded wait that proves the exit.
        pub fn terminate(&self) -> Result<()> {
            unsafe {
                if TerminateProcess(self.process, 1) == 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("could not terminate a controlled child");
                }
                Ok(())
            }
        }

        /// Join the stdio drains that have actually finished.
        ///
        /// Only a thread that has already left (`is_finished`) is joined;
        /// one that has not is recorded as not reaped and deliberately
        /// **not joined** — there is no per-thread wait and no unbounded
        /// join, and the bounded waits that give this check meaning are the
        /// caller's: every caller reaches here only after its own shared
        /// cleanup deadline has been enforced on the process first. A
        /// detached-but-still-running reader exits on its own when its pipe
        /// reaches EOF; its exit is recorded as unverified, never claimed.
        ///
        /// The reaping itself is also contained: some Rust builds'
        /// `JoinHandle::join` **panics** (rather than returning `Err`) when
        /// the kernel wait fails, and that panic must not cost the rest of
        /// the round its cleanup and its report. A contained failure is
        /// returned as `false`, which every caller records as unverified
        /// cleanup — it is never swallowed into a silent pass.
        pub fn join_io(&mut self) -> bool {
            let mut reaped = true;
            for drain in self.drains.drain(..) {
                if !drain.is_finished() {
                    reaped = false;
                    // Dropping the handle detaches this thread; it ends on
                    // its own when its pipe reaches EOF. It is not joined
                    // and its exit is not claimed.
                    continue;
                }
                let joined =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drain.join()));
                if !matches!(joined, Ok(Ok(()))) {
                    reaped = false;
                    eprintln!("join_io: a stdio drain join failed");
                }
            }
            reaped
        }
    }

    impl Drop for Child {
        fn drop(&mut self) {
            // Closing a handle does not terminate anything; the runner must
            // have proven exit (or job teardown) before this runs.
            close(self.thread);
            close(self.process);
        }
    }

    fn drain(read: HANDLE) -> (Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
        let counter = Arc::new(AtomicUsize::new(0));
        let moved = Arc::clone(&counter);
        // The raw handle becomes an owned File before the thread starts, so
        // only a Send value crosses the thread boundary.
        let mut stream = unsafe { std::fs::File::from_raw_handle(read) };
        let handle = std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            loop {
                match std::io::Read::read(&mut stream, &mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        moved.fetch_add(read, Ordering::Relaxed);
                    }
                }
            }
        });
        (counter, handle)
    }

    /// All fields are `Copy`, so a request can be re-issued with one field
    /// changed (the suspended variant used by `spawn_contained`).
    #[derive(Clone, Copy)]
    pub struct SpawnRequest<'a> {
        pub program: &'a str,
        pub args: &'a [String],
        pub env: &'a BTreeMap<String, String>,
        pub cwd: Option<&'a Path>,
        /// Extra handles to hand over, on top of the child's stdio ends.
        pub inherit: &'a [HANDLE],
        pub suspended: bool,
    }

    /// Create a child with this run's pipes as its stdio and discard both
    /// streams in memory. Only `inherit` plus the three stdio ends are
    /// inheritable, so nothing else leaks across.
    pub fn spawn(request: &SpawnRequest<'_>) -> Result<Child> {
        let mut command_line = quote(request.program);
        for argument in request.args {
            command_line.push(' ');
            command_line.push_str(&quote(argument));
        }
        let mut command_line_wide = wide(&command_line);
        let mut application_wide = wide(request.program);
        let mut cwd_wide = request.cwd.map(|path| wide(&path.to_string_lossy()));
        let mut environment = environment_block(request.env);

        let mut pipes = create_pipes()?;
        let mut inherited: Vec<HANDLE> = request.inherit.to_vec();
        inherited.extend_from_slice(&[pipes.stdin_read, pipes.stdout_write, pipes.stderr_write]);

        let mut attributes = HandleAttributes::new(&mut inherited)?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = pipes.stdin_read;
        startup.StartupInfo.hStdOutput = pipes.stdout_write;
        startup.StartupInfo.hStdError = pipes.stderr_write;
        startup.lpAttributeList = attributes.pointer();

        let mut flags = CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT;
        if request.suspended {
            flags |= CREATE_SUSPENDED;
        }
        let mut information = PROCESS_INFORMATION::default();
        let created = unsafe {
            CreateProcessW(
                application_wide.as_mut_ptr(),
                command_line_wide.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                flags,
                environment.as_mut_ptr() as *mut _,
                cwd_wide
                    .as_mut()
                    .map(|wide| wide.as_mut_ptr())
                    .unwrap_or(std::ptr::null_mut()),
                &startup.StartupInfo,
                &mut information,
            )
        };
        if created == 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!("could not create the child process {:?}", request.program)
            });
        }

        // The parent never needs the child's pipe ends; holding them would
        // keep EOF from ever arriving. They are nulled here, not only
        // closed: `Pipes::drop` closes every end still owned, and a value
        // that was already closed must never be closed a second time.
        close(pipes.stdin_read);
        close(pipes.stdout_write);
        close(pipes.stderr_write);
        close(pipes.stdin_write);
        pipes.stdin_read = std::ptr::null_mut();
        pipes.stdout_write = std::ptr::null_mut();
        pipes.stderr_write = std::ptr::null_mut();
        pipes.stdin_write = std::ptr::null_mut();
        let (stdout_bytes, stdout_drain) = drain(pipes.stdout_read);
        let (stderr_bytes, stderr_drain) = drain(pipes.stderr_read);
        // The read ends' ownership moved into the drain threads; the struct
        // must not close them behind the threads' backs.
        pipes.stdout_read = std::ptr::null_mut();
        pipes.stderr_read = std::ptr::null_mut();

        Ok(Child {
            pid: information.dwProcessId,
            process: information.hProcess,
            thread: information.hThread,
            stdout_bytes,
            stderr_bytes,
            drains: vec![stdout_drain, stderr_drain],
        })
    }

    /// Create a child suspended, contain it in `job` by its creation handle,
    /// then resume. Mirrors the supervisor's own order exactly.
    pub fn spawn_contained(
        request: &SpawnRequest<'_>,
        job: &Job,
    ) -> Result<Child> {
        let request = SpawnRequest {
            suspended: true,
            ..*request
        };
        let child = spawn(&request)?;
        if let Err(error) = job.assign(child.process) {
            // It never ran an instruction and was never in the job: end it
            // through the creation handle and wait, verified.
            unsafe {
                let _ = TerminateProcess(child.process, 1);
            }
            let waited = wait_handle(child.process, 5_000).is_ok();
            if !waited {
                return Err(error.context("a controlled child did not exit after termination"));
            }
            return Err(error);
        }
        unsafe {
            if ResumeThread(child.thread) == u32::MAX {
                let _ = TerminateProcess(child.process, 1);
                let _ = wait_handle(child.process, 5_000);
                return Err(std::io::Error::last_os_error())
                    .context("could not resume a controlled child");
            }
        }
        Ok(child)
    }
}

// ------------------------------------------------------------------ report

#[cfg(windows)]
struct Checks {
    items: Vec<Value>,
}

#[cfg(windows)]
impl Checks {
    fn new() -> Self {
        Self { items: Vec::new() }
    }

    fn record(&mut self, name: &str, verdict: &str, detail: String) {
        self.items.push(json!({
            "name": name,
            "verdict": verdict,
            "detail": detail,
        }));
    }

    /// A fact that must hold. A failure here is a real failure of the run.
    fn verify(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        self.record(name, if ok { "verified" } else { "failed" }, detail.into());
    }

    /// Something this black box cannot decide. Never a pass, never a fail.
    fn not_covered(&mut self, name: &str, reason: impl Into<String>) {
        self.record(name, "notCovered", reason.into());
    }

    fn outcome(&self) -> &'static str {
        if self.items.iter().any(|item| item["verdict"] == "failed") {
            "fail"
        } else if self.items.iter().any(|item| item["verdict"] == "notCovered") {
            "blocked"
        } else {
            "pass"
        }
    }
}

#[cfg(windows)]
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut text = String::with_capacity(64);
    for byte in hasher.finalize().iter() {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

#[cfg(windows)]
fn sha256_file(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| sha256_hex(&bytes))
}

#[cfg(windows)]
fn utc_stamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("unix-{seconds}")
}

#[cfg(windows)]
fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

#[cfg(windows)]
fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[cfg(windows)]
fn u64_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

#[cfg(windows)]
fn final_event(events: &[Value]) -> Option<&Value> {
    events.iter().rev().find(|event| event["event"] == "final")
}

/// The containment verdict is only meaningful when every field it summarises
/// is itself true. Reading them apart is what stops a single boolean from
/// standing in for the whole teardown.
fn containment_consistent(event: &Value) -> bool {
    event["containmentVerified"].as_bool().unwrap_or(false)
        && event["terminated"].as_bool().unwrap_or(false)
        && event["allChildrenExited"].as_bool().unwrap_or(false)
        && event["jobActiveProcessesZero"].as_bool().unwrap_or(false)
        && event["startupCleanupVerified"].as_bool().unwrap_or(false)
        && event["ioWorkersExited"].as_bool().unwrap_or(false)
        && event["statusFileOk"].as_bool().unwrap_or(false)
}

/// The independent observer: a separate process, outside every job, that
/// receives real handles by inheritance and does its own kernel queries and
/// bounded waits. It writes only structured facts, never child text.
#[cfg(windows)]
mod observer {
    use std::path::{Path, PathBuf};

    use anyhow::{Context as _, Result};
    use serde_json::{json, Value};
    use windows_sys::Win32::System::JobObjects::{
        JobObjectBasicProcessIdList, JOBOBJECT_BASIC_PROCESS_ID_LIST, QueryInformationJobObject,
    };

    use super::{observation, str_field, win, Checks};
    use std::time::{Duration, Instant};

    pub fn main() -> Result<i32> {
        let mut arguments: Vec<String> = std::env::args().skip(1).collect();
        // A flag is removed with its value as soon as it is read, so the
        // parser below can never consume a value twice.
        let take = |name: &str, arguments: &mut Vec<String>| -> Option<String> {
            let position = arguments.iter().position(|argument| argument == name)?;
            let value = arguments.get(position + 1).cloned()?;
            arguments.drain(position..=position + 1);
            Some(value)
        };
        let phase = take("--phase", &mut arguments).unwrap_or_else(|| "control-before".to_owned());
        let facts = PathBuf::from(
            take("--facts", &mut arguments)
                .ok_or_else(|| anyhow::anyhow!("--facts is required"))?,
        );
        let job = take("--job", &mut arguments).and_then(|value| value.parse::<usize>().ok());
        let probe =
            take("--probe", &mut arguments).and_then(|value| value.parse::<usize>().ok());
        let outsider =
            take("--outsider", &mut arguments).and_then(|value| value.parse::<usize>().ok());
        let outsider_job =
            take("--outsider-job", &mut arguments).and_then(|value| value.parse::<usize>().ok());
        let probe_pid =
            take("--probe-pid", &mut arguments).and_then(|value| value.parse::<usize>().ok());
        let fixture_pid =
            take("--fixture-pid", &mut arguments).and_then(|value| value.parse::<usize>().ok());
        let outsider_pid =
            take("--outsider-pid", &mut arguments).and_then(|value| value.parse::<usize>().ok());

        let job = job.map(|value| value as win::Handle);
        let probe = probe.map(|value| value as win::Handle);
        let outsider = outsider.map(|value| value as win::Handle);
        let outsider_job = outsider_job.map(|value| value as win::Handle);

        let nonce = take("--nonce", &mut arguments).unwrap_or_default();
        let crash_boundary = take("--crash-boundary", &mut arguments).map(PathBuf::from);
        let mut handoffs_seen = 0_u64;
        let mut job_copies_closed_flag = false;
        let observation_request =
            take("--observation-request", &mut arguments).and_then(|value| value.parse::<usize>().ok());
        let observation_response =
            take("--observation-response", &mut arguments).and_then(|value| value.parse::<usize>().ok());
        let deadline_ms = take("--deadline-ms", &mut arguments).and_then(|value| value.parse::<u64>().ok());
        let started = Instant::now();
        let deadline = started + Duration::from_millis(deadline_ms.unwrap_or(60_000));

        let mut checks = Checks::new();
        match phase.as_str() {
            "observe" => observe(
                &mut checks,
                &mut handoffs_seen,
                &mut job_copies_closed_flag,
                nonce.as_str(),
                observation_request.map(|value| value as win::Handle),
                observation_response.map(|value| value as win::Handle),
                deadline,
                crash_boundary.as_deref(),
            ),
            "control-before" => control_before(
                &mut checks,
                job,
                probe,
                outsider,
                outsider_job,
                probe_pid,
                fixture_pid,
                outsider_pid,
            ),
            "control-after" => control_after(&mut checks, job, probe),
            other => {
                checks.not_covered("phase", format!("unknown observer phase {other:?}"));
            }
        }

        let report = json!({
            "marker": "fintwind-ownership-observer-v1",
            "observerPid": win::current_process_id(),
            "phase": phase,
            "handoffsReceived": handoffs_seen,
            "jobCopiesClosed": job_copies_closed_flag,
            "checks": checks.items,
            "outcome": checks.outcome(),
        });
        if let Some(parent) = facts.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&facts, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("could not write {}", facts.display()))?;
        Ok(0)
    }

    #[allow(clippy::too_many_arguments)]
    fn control_before(
        checks: &mut Checks,
        job: Option<win::Handle>,
        probe: Option<win::Handle>,
        outsider: Option<win::Handle>,
        outsider_job: Option<win::Handle>,
        probe_pid: Option<usize>,
        fixture_pid: Option<usize>,
        outsider_pid: Option<usize>,
    ) {
        let (Some(job), Some(probe), Some(outsider), Some(outsider_job)) =
            (job, probe, outsider, outsider_job)
        else {
            checks.not_covered(
                "handles",
                "the observer was not handed the handles this phase needs",
            );
            return;
        };
        // A null handle would make every query below fail for the wrong
        // reason, so it is reported as missing evidence, not as a failure.
        if job.is_null() || probe.is_null() || outsider.is_null() || outsider_job.is_null() {
            checks.not_covered(
                "handles",
                "one of the inherited handles is null, so the observer holds no evidence to report",
            );
            return;
        }
        let self_pid = win::current_process_id();

        // Same job, against a specific job handle — never "any job".
        checks.verify(
            "probe.inRunnerJob",
            win::is_in_job(probe, job).unwrap_or(false),
            "the control grandchild must sit in the runner's own job",
        );
        checks.verify(
            "probe.inSomeJob",
            win::is_in_job(probe, std::ptr::null_mut()).unwrap_or(false),
            "the control grandchild must inherit the job from its parent",
        );
        checks.verify(
            "outsider.notInRunnerJob",
            !win::is_in_job(outsider, job).unwrap_or(true),
            "the outsider lives in a different job, which is what a generic any-job check could not tell apart",
        );
        checks.verify(
            "outsider.inOutsiderJob",
            win::is_in_job(outsider, outsider_job).unwrap_or(false),
            "the outsider is contained by the runner's second, separate job",
        );
        checks.verify(
            "outsider.inSomeJob",
            win::is_in_job(outsider, std::ptr::null_mut()).unwrap_or(false),
            "the outsider is contained too, which is what makes the negative above meaningful",
        );

        // The kernel's own membership list, cross-checked against pids the
        // runner learned from its own CreateProcessW calls.
        match process_ids(job) {
            Ok(pids) => {
                checks.verify(
                    "job.containsFixture",
                    fixture_pid.is_some_and(|pid| pids.contains(&(pid as u32))),
                    "the runner's job accounting lists the control fixture pid",
                );
                checks.verify(
                    "job.containsGrandchild",
                    probe_pid.is_some_and(|pid| pids.contains(&(pid as u32))),
                    "the runner's job accounting lists the control grandchild pid",
                );
                checks.verify(
                    "job.excludesOutsider",
                    !outsider_pid.is_some_and(|pid| pids.contains(&(pid as u32))),
                    "the runner's job accounting must not list the outsider",
                );
                checks.verify(
                    "job.excludesObserver",
                    !pids.contains(&self_pid),
                    "the observer itself is outside every job",
                );
            }
            Err(error) => {
                checks.verify("job.processIdList", false, format!("{error:#}"));
            }
        }

        checks.verify(
            "probe.aliveBeforeTeardown",
            win::still_active(probe),
            "the control grandchild is still running before the teardown",
        );
    }

    /// The observation phase: read hand-offs on the supervisor's channel,
    /// verify each one, cross-check the frozen topology against the job's own
    /// pid list, then close every job copy and ACK so the tested job's
    /// kill-on-close is not delayed by anything this observer holds.
    ///
    /// Until main integrates the subject side, no hand-off ever arrives. That
    /// is reported as not covered; nothing here invents a substitute.
    #[allow(clippy::too_many_arguments)]
    fn observe(
        checks: &mut Checks,
        handoffs_seen: &mut u64,
        job_copies_closed_flag: &mut bool,
        nonce: &str,
        request_read: Option<win::Handle>,
        response_write: Option<win::Handle>,
        deadline: Instant,
        crash_boundary: Option<&Path>,
    ) {
        use std::collections::BTreeMap;
        let (Some(request_read), Some(response_write)) = (request_read, response_write) else {
            checks.not_covered(
                "observation.channel",
                "the observer was not handed the observation channel ends",
            );
            return;
        };
        let mut hub = observation::ObserverHub::new();
        if let Err(error) =
            hub.register(observation::WriterId::supervisor(), request_read, response_write)
        {
            // Without a working channel no hand-off can be verified, so this
            // observation cannot produce evidence; the failure is reported,
            // never swallowed into a silent empty hub.
            checks.not_covered(
                "observation.channel",
                &format!("the observer channel could not start: {error}"),
            );
            let _ = hub.finish(deadline);
            return;
        }

        let mut seen: BTreeMap<observation::WriterId, u64> = BTreeMap::new();
        let mut job: Option<win::Handle> = None;
        let mut processes: Vec<win::Handle> = Vec::new();
        let mut frozen = false;
        let mut channel_closed = false;
        let mut job_copies_closed = false;
        let mut unexplained: Vec<u32> = Vec::new();
        *job_copies_closed_flag = false;

        loop {
            if Instant::now() >= deadline {
                break;
            }
            match hub.next(deadline) {
                Ok(Some((id, message))) => {
                    *handoffs_seen += 1;
                    let verified = match observation::verify_handoff(&message, nonce, &mut seen) {
                        Ok(verified) => verified,
                        Err(error) => {
                            let _ = hub.ack(&id, &message, false, &format!("{error}"), deadline);
                            checks.verify(
                                "observation.handoff",
                                false,
                                format!("a hand-off was refused: {error}"),
                            );
                            continue;
                        }
                    };
                    match verified.kind {
                        observation::Kind::JobQuery => {
                            if let Some(handle) = verified.job {
                                job = Some(handle);
                                hub.keep_job_copy(handle);
                            }
                        }
                        observation::Kind::FixtureChannel => {
                            // The supervisor created a channel pair for one
                            // observing fixture and duplicated the observer's
                            // ends here, labelled explicitly as pipe ends. A
                            // pipe has no pid, so it never travels in the
                            // process-handle fields where `GetProcessId`
                            // would fail or, worse, be skipped; the two
                            // namespaces stay apart by construction.
                            let Some(fixture_id) = message.fixture else {
                                checks.verify(
                                    "observation.fixtureChannel",
                                    false,
                                    "a fixture channel was announced without a fixture id",
                                );
                                continue;
                            };
                            if message.process_handle.is_some() || message.job_handle.is_some() {
                                // A pipe announcement carrying process or job
                                // handles is a protocol misuse, not a channel.
                                checks.verify(
                                    "observation.fixtureChannel",
                                    false,
                                    "a fixture channel was announced with process or job handles in its pipe fields",
                                );
                                continue;
                            }
                            let request = message
                                .pipe_request_read
                                .map(|value| value as win::Handle);
                            let response = message
                                .pipe_response_write
                                .map(|value| value as win::Handle);
                            if let (Some(request), Some(response)) = (request, response) {
                                match hub.register(
                                    observation::WriterId::fixture(fixture_id),
                                    request,
                                    response,
                                ) {
                                    Ok(()) => checks.verify(
                                        "observation.fixtureChannel",
                                        true,
                                        format!(
                                            "registered the observation channel for fixture {fixture_id}"
                                        ),
                                    ),
                                    Err(error) => checks.verify(
                                        "observation.fixtureChannel",
                                        false,
                                        format!(
                                            "the observation channel for fixture {fixture_id} could not start: {error}"
                                        ),
                                    ),
                                }
                            } else {
                                checks.verify(
                                    "observation.fixtureChannel",
                                    false,
                                    "a fixture channel was announced without both ends",
                                );
                            }
                        }
                        observation::Kind::RootChild | observation::Kind::Descendant => {
                            if let Some(handle) = verified.process {
                                // Same job, against the exact job handle the
                                // supervisor handed over — never the null
                                // "some job" variant.
                                let same = match job {
                                    Some(job) => win::is_in_job(handle, job).unwrap_or(false),
                                    None => false,
                                };
                                checks.verify(
                                    "observation.inExactJob",
                                    same,
                                    format!(
                                        "pid {} (from GetProcessId on the real handle) sits in the supervisor's own job",
                                        verified.pid
                                    ),
                                );
                                processes.push(handle);
                            }
                        }
                        observation::Kind::TopologyFrozen => {
                            frozen = true;
                            // The kernel's own membership list must equal the
                            // set of pids the real handles name. Anything else
                            // is an unexplained member and the tree is not
                            // declared clean.
                            if let Some(job) = job {
                                let held: Vec<u32> = processes
                                    .iter()
                                    .map(|handle| {
                                        unsafe {
                                            windows_sys::Win32::System::Threading::GetProcessId(
                                                *handle,
                                            )
                                        }
                                    })
                                    .collect();
                                match win::job_process_ids(job) {
                                    Ok(members) => {
                                        let mut unexpected = members.clone();
                                        unexpected.retain(|pid| !held.contains(pid));
                                        unexplained = unexpected.clone();
                                        checks.verify(
                                            "observation.topologyMatchesHandles",
                                            unexpected.is_empty(),
                                            format!(
                                                "the job lists {} processes and the observer holds a real handle for {} of them; unexplained members: {:?}",
                                                members.len(),
                                                held.len(),
                                                unexpected
                                            ),
                                        );
                                    }
                                    Err(error) => checks.verify(
                                        "observation.jobProcessIdList",
                                        false,
                                        format!("{error:#}"),
                                    ),
                                }
                            } else {
                                checks.not_covered(
                                    "observation.topologyMatchesHandles",
                                    "no job query handle was handed over, so the frozen topology cannot be cross-checked",
                                );
                            }
                            // Close every job copy and say so. Holding one
                            // would delay the tested job's kill-on-close.
                            job = None;
                            hub.close_job_copies();
                            job_copies_closed = true;
                            *job_copies_closed_flag = true;
                            checks.verify(
                                "observation.jobCopiesClosed",
                                true,
                                "every job query handle copy was closed before the crash phase, so nothing this observer holds can delay the tested job's kill-on-close",
                            );
                        }
                                            }
                    if frozen {
                        if let Some(path) = crash_boundary {
                            // Withhold the final ACK in the observer, not the
                            // subject. This stops normal teardown while the
                            // runner crashes its own supervisor creation handle.
                            let live = processes.iter().filter(|handle| win::still_active(**handle)).count();
                            let ready = checks.outcome() == "pass" && job_copies_closed && live >= 2;
                            checks.verify("observation.crashBoundaryLive", ready,
                                "verified frozen topology, closed job copies and at least two live handed-over processes before the crash");
                            if std::fs::write(path, if ready { b"ready".as_slice() } else { b"failed".as_slice() }).is_err() {
                                checks.verify("observation.crashBoundaryPublished", false, "could not publish the isolated crash boundary");
                            }
                            break;
                        }
                    }
                    let _ = hub.ack(&id, &message, true, "accepted", deadline);
                    // Frozen topology is the protocol boundary. The runner
                    // still owns pipe copies, so EOF is not an exit signal.
                    if frozen { break; }
                }
                Ok(None) => {
                    // The request pipe reached end of stream: the subject let
                    // go of its end, which is what its death looks like from
                    // here. The runner knows whether that was a crash.
                    channel_closed = true;
                    break;
                }
                Err(error) => {
                    checks.verify("observation.read", false, format!("{error:#}"));
                    break;
                }
            }
        }

        if !frozen {
            checks.not_covered(
                "observation.topologyFrozen",
                "no topology-frozen event arrived: the supervisor does not implement the observation interface yet, so the observer cannot know the tree stopped growing",
            );
        }
        if processes.is_empty() {
            checks.not_covered(
                "observation.descendantHandles",
                "no descendant handle was handed over: the supervisor does not implement the observation interface yet, so there is no real handle to wait on",
            );
        }
        if channel_closed && !job_copies_closed {
            checks.not_covered(
                "observation.killOnClosePhase",
                "the supervisor exited while the observer still held a job query handle, so this crash is not a kill-on-close proof; only a crash after the job copies are closed and acknowledged is the formal scenario",
            );
        }

        // After the crash, wait on every real descendant handle. A timeout is
        // a failure, never a shrug.
        for handle in &processes {
            match win::wait_handle(*handle, 10_000) {
                Ok(Some(code)) => checks.verify(
                    "observation.descendantExited",
                    true,
                    format!("a handed-over descendant exited on its own (code {code})"),
                ),
                Ok(None) => checks.verify(
                    "observation.descendantExited",
                    false,
                    "a handed-over descendant was still running after the bounded wait",
                ),
                Err(error) => checks.verify(
                    "observation.descendantExited",
                    false,
                    format!("{error:#}"),
                ),
            }
        }
        if !unexplained.is_empty() {
            checks.verify(
                "observation.noUnexplainedMembers",
                false,
                format!(
                    "the job listed processes the observer holds no handle for: {:?}; the tree is not declared clean",
                    unexplained
                ),
            );
        }
        if let Err(error) = hub.finish(deadline + std::time::Duration::from_secs(10)) {
            checks.verify("observation.workersReaped", false, format!("{error:#}"));
        } else {
            checks.verify(
                "observation.workersReaped",
                true,
                "every observation worker was stopped and reaped within its bound",
            );
        }
    }

    fn control_after(checks: &mut Checks, job: Option<win::Handle>, probe: Option<win::Handle>) {
        let (Some(job), Some(probe)) = (job, probe) else {
            checks.not_covered(
                "handles",
                "the observer was not handed the handles this phase needs",
            );
            return;
        };
        if job.is_null() || probe.is_null() {
            checks.not_covered(
                "handles",
                "one of the inherited handles is null, so the observer holds no evidence to report",
            );
            return;
        }
        // A real handle, really waited: a timeout here is a failure, never a
        // "probably fine".
        match win::wait_handle(probe, 10_000) {
            Ok(Some(code)) => checks.verify(
                "probe.exitedAfterTeardown",
                true,
                format!("the control grandchild exited on its own job's teardown (code {code})"),
            ),
            Ok(None) => checks.verify(
                "probe.exitedAfterTeardown",
                false,
                "the control grandchild was still running after the teardown deadline",
            ),
            Err(error) => {
                checks.verify("probe.exitedAfterTeardown", false, format!("{error:#}"))
            }
        }
        checks.verify(
            "probe.notActiveAfterTeardown",
            !win::still_active(probe),
            "the control grandchild must no longer report STILL_ACTIVE",
        );
        match process_ids(job) {
            Ok(pids) => checks.verify(
                "job.emptyAfterTeardown",
                pids.is_empty(),
                format!(
                    "the runner's job accounting lists {} processes after the teardown",
                    pids.len()
                ),
            ),
            Err(error) => checks.verify("job.processIdList", false, format!("{error:#}")),
        }
    }

    /// Read the kernel's own membership list for a job handle this process
    /// holds, without touching any process handle.
    fn process_ids(job: win::Handle) -> Result<Vec<u32>> {
        let mut storage: Vec<usize> = vec![0; 8 + 512];
        let mut returned: u32 = 0;
        let ok = unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicProcessIdList,
                storage.as_mut_ptr().cast(),
                (storage.len() * std::mem::size_of::<usize>()) as u32,
                &mut returned,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not read the job's process id list");
        }
        let list = unsafe { &*(storage.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()) };
        let count = list.NumberOfProcessIdsInList as usize;
        Ok((0..count.min(512))
            .map(|index| unsafe { *list.ProcessIdList.as_ptr().add(index) } as u32)
            .collect())
    }

    /// The observer's own verdicts as a standalone `Checks` value.
    pub fn merge_checks(facts_path: &Path) -> Checks {
        let mut checks = Checks::new();
        merge(facts_path, &mut checks);
        checks
    }

    /// Fold the observer's own verdicts into the runner's, keeping them as the
    /// authoritative independent record.
    pub fn merge(facts_path: &Path, checks: &mut Checks) {
        let Ok(text) = std::fs::read_to_string(facts_path) else {
            checks.not_covered(
                "observer.facts",
                "the observer did not write a facts file, so nothing it saw can be reported",
            );
            return;
        };
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let items = value
            .get("checks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            checks.not_covered("observer.facts", "the observer facts file carried no checks");
            return;
        }
        for item in items {
            let name = str_field(&item, "name").unwrap_or_else(|| "unnamed".to_owned());
            let detail = str_field(&item, "detail").unwrap_or_default();
            match str_field(&item, "verdict").as_deref() {
                Some("verified") => checks.verify(&format!("observer.{name}"), true, detail),
                Some("notCovered") => checks.not_covered(&format!("observer.{name}"), detail),
                _ => checks.verify(&format!("observer.{name}"), false, detail),
            }
        }
    }
}

// ---------------------------------------------------------------- harness

/// One supervisor run's observation channel. Owns the pipe ends and the
/// observer handle the subject inherits, so a partial setup can never leak a
/// handle or leave a peer waiting forever. The observer **process** is not
/// held here: it lives in `Harness::observing` for the whole run, so a panic
/// unwinding out of a scenario still leaves its creation handles in the
/// harness's hands.
#[cfg(windows)]
struct Observation {
    nonce: String,
    facts: PathBuf,
    /// Keeps the four pipe ends alive for as long as the channel is in use.
    channel: observation::Channel,
    /// The observer's real handle, narrowed to `PROCESS_DUP_HANDLE`.
    observer_for_subject: win::Handle,
    subject_args: Vec<String>,
}

#[cfg(windows)]
impl Observation {
    /// The extra arguments the subject needs to use the channel. A subject
    /// that ignores them behaves exactly as it does today.
    fn subject_args(&self) -> Vec<String> {
        self.subject_args.clone()
    }

    /// The handles the subject must inherit, and only those.
    fn subject_inherit(&self) -> Vec<win::Handle> {
        vec![
            self.observer_for_subject,
            self.channel.request_write().unwrap_or(std::ptr::null_mut()),
            self.channel.response_read().unwrap_or(std::ptr::null_mut()),
        ]
    }

    /// Read the observer's facts into checks, once its process has been
    /// waited and reaped by the harness.
    fn merge_into_checks(&self, checks: &mut Checks, exited: Option<u32>) {
        let merged = observer::merge_checks(&self.facts);
        checks.items.extend(merged.items);
        let handoffs = std::fs::read_to_string(&self.facts)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| u64_field(&value, "handoffsReceived"))
            .unwrap_or(0);
        checks.verify(
            "observation.nonceWidth",
            self.nonce.len() == observation::NONCE_BYTES * 2,
            format!(
                "a {}-byte per-round nonce correlated every hand-off; the value itself is never logged",
                observation::NONCE_BYTES
            ),
        );
        checks.verify(
            "observation.observerBounded",
            exited.is_some(),
            format!(
                "the observer finished within its own deadline (exit {:?}); it is only ever ended through the runner's creation handle",
                exited
            ),
        );
        if handoffs == 0 {
            checks.not_covered(
                "observation.subjectIntegrated",
                "the runner created the channel, the observer and the inherited handles, but no hand-off arrived. This is awaiting a real run, not a pass, and the blocked checks below stay blocked until one lands.",
            );
        }
    }
}

#[cfg(windows)]
struct Harness {
    /// The one independent directory this round owns.
    dir: PathBuf,
    supervisor: PathBuf,
    fixture: PathBuf,
    outsider_job: win::Job,
    outsider: Option<win::Child>,
    outsider_final: Option<Value>,
    /// The supervised supervisor of the run in progress, held **here** —
    /// outside every scenario call frame — so that a panic unwinding out of
    /// a scenario cannot strip this runner of its creation handles before
    /// it has terminated the tree by handle and bounded-waited it. This is
    /// the runner's own creation handle on its own child; no outer job
    /// wraps the supervised tree and no pid is ever re-opened.
    supervised: Option<win::Child>,
    /// The observation-side child of the run in progress, held for the same
    /// reason: after a panic its handles are still this runner's, and its
    /// exit (or its own deadline) is bounded-waited, never assumed.
    observing: Option<win::Child>,
    /// `Some(true)` means the runner sits in a job, `Some(false)` means it
    /// verifiably does not, `None` means it could not be determined. The
    /// supervisor-tree deduction needs `Some(false)`.
    runner_in_job: Option<bool>,
    /// Same, for the supervisor, learned from the handle the runner holds.
    supervisor_in_job: Option<bool>,
    /// Raw child output, counted in memory and discarded, for the scenario
    /// currently running. It never reaches disk.
    discarded_stdout_bytes: usize,
    discarded_stderr_bytes: usize,
    scenarios: Vec<Value>,
    blocked: Vec<Value>,
    unverified_cleanup: Vec<String>,
}

#[cfg(windows)]
impl Harness {
    fn new(dir: PathBuf, supervisor: PathBuf, fixture: PathBuf) -> Result<Self> {
        let outsider_job = win::Job::new()?;
        let runner_in_job = Some(win::is_in_job(win::current_process(), std::ptr::null_mut())?);
        Ok(Self {
            dir,
            supervisor,
            fixture,
            outsider_job,
            outsider: None,
            outsider_final: None,
            supervised: None,
            observing: None,
            runner_in_job,
            supervisor_in_job: None,
            discarded_stdout_bytes: 0,
            discarded_stderr_bytes: 0,
            scenarios: Vec::new(),
            blocked: Vec::new(),
            unverified_cleanup: Vec::new(),
        })
    }

    /// End a run-in-progress's supervised supervisor and observer, by the
    /// creation handles this runner still owns, bounded.
    ///
    /// Used by the panic path: the scenario call frames are gone, but the
    /// handles live in this harness, so the ending stays handle-based —
    /// `TerminateProcess` on a handle created this round, a bounded wait on
    /// the same handle, and a contained reaping of the stdio drains. Every
    /// step that does not verify pushes into `unverified_cleanup`; nothing
    /// here claims an exit it did not wait for.
    fn terminate_run_in_progress(&mut self) {
        if let Some(child) = self.supervised.as_mut() {
            let terminated = child.terminate().is_ok();
            let exited = child.wait(10_000).ok().flatten();
            let reaped = child.join_io();
            if !terminated || exited.is_none() || !reaped {
                self.unverified_cleanup.push(
                    "the supervised supervisor of the panicking run was not verifiably ended"
                        .to_owned(),
                );
            }
        }
        if let Some(observer) = self.observing.as_mut() {
            let exited = observer.wait(10_000).ok().flatten();
            let reaped = observer.join_io();
            if exited.is_none() || !reaped {
                self.unverified_cleanup.push(
                    "the observer of the panicking run was not verifiably ended".to_owned(),
                );
            }
        }
        self.supervised = None;
        self.observing = None;
    }

    fn scenario_dir(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn path(&self, name: &str, file: &str) -> PathBuf {
        self.scenario_dir(name).join(file)
    }

    /// The environment every child gets: a whitelist by name, `PATH` narrowed
    /// to the system directory. Values are never recorded.
    fn whitelist_env(&self) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        for name in [
            "SystemRoot",
            "windir",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
            "COMSPEC",
            "PATHEXT",
        ] {
            if let Some(value) = std::env::var_os(name) {
                env.insert(name.to_owned(), value.to_string_lossy().into_owned());
            }
        }
        let system_root = env
            .get("SystemRoot")
            .cloned()
            .unwrap_or_else(|| "C:\\Windows".to_owned());
        env.insert("PATH".to_owned(), format!("{system_root}\\System32"));
        env
    }

    fn whitelist_names(&self) -> Vec<String> {
        self.whitelist_env().into_keys().collect()
    }

    fn record_blocked(&mut self, check: &str, reason: &str, suggestion: &str) {
        self.blocked.push(json!({
            "check": check,
            "reason": reason,
            "suggestion": suggestion,
        }));
    }

    /// Read the supervisor's status file. A trailing partial line is ignored:
    /// the crash path can kill the supervisor mid-write.
    fn read_status(path: &Path) -> (bool, Vec<Value>) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return (false, Vec::new());
        };
        let mut events = Vec::new();
        for line in text.split('\n') {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                events.push(value);
            }
        }
        (true, events)
    }

    /// Wait, bounded, for this run's observer to finish, reap its drains,
    /// and fold its facts into checks. The observer process lives in
    /// `self.observing` — the harness, not the call frame — and is cleared
    /// here once its exit has been waited.
    fn finish_observation(&mut self, observation: Observation) -> Checks {
        let mut checks = Checks::new();
        let mut exited = None;
        if let Some(observer) = self.observing.as_mut() {
            exited = observer.wait(30_000).ok().flatten();
            if exited.is_none() {
                self.unverified_cleanup.push("observer exceeded its exit deadline".to_owned());
                let _ = observer.terminate();
                exited = observer.wait(5_000).ok().flatten();
            }
            // A drain that fails to reap is an unverified cleanup, and
            // unverified cleanup never reads as green.
            checks.verify(
                "observation.observerIoReaped",
                observer.join_io(),
                "the observer's stdio drains were reaped within the run",
            );
        }
        observation.merge_into_checks(&mut checks, exited);
        self.observing = None;
        checks
    }

    fn spawn_outsider(&mut self) -> Result<()> {
        let facts = self.path("outsider", "outsider.facts.json");
        let env = self.whitelist_env();
        let args = vec![
            "--facts".to_owned(),
            facts.to_string_lossy().into_owned(),
            "--stay-alive".to_owned(),
        ];
        let request = win::SpawnRequest {
            program: &self.fixture.to_string_lossy(),
            args: &args,
            env: &env,
            cwd: Some(&self.dir),
            inherit: &[],
            suspended: true,
        };
        let child = win::spawn_contained(&request, &self.outsider_job)?;
        self.outsider = Some(child);
        Ok(())
    }

    /// The outsider must survive every scenario: it lives in a different job,
    /// so a supervisor that tore down anything outside its own job would show
    /// up here.
    fn outsider_alive(&self) -> bool {
        self.outsider
            .as_ref()
            .map(|child| win::still_active(child.process))
            .unwrap_or(false)
    }

    fn outsider_pid(&self) -> Option<u32> {
        self.outsider.as_ref().map(|child| child.pid)
    }

    fn outsider_handle(&self) -> win::Handle {
        self.outsider
            .as_ref()
            .map(|child| child.process)
            .unwrap_or(std::ptr::null_mut())
    }

    /// Set up the observation channel for one supervisor run.
    ///
    /// This is the runner half of the interface the supervisor implements on
    /// the subject side. The runner creates the observer, duplicates the
    /// observer's real handle down to exactly `PROCESS_DUP_HANDLE`, and hands
    /// the subject end of one anonymous pipe pair to the supervisor by
    /// explicit minimal inheritance. The observer stays alive for the whole
    /// run — its drains are joined by `Observation::finish`, never here.
    fn start_observation(&mut self, name: &str, deadline_ms: u64) -> Result<Observation> {
        let facts = self.path(name, "observer.observe.facts.json");
        let nonce = observation::nonce();

        let channel = observation::Channel::create()?;
        channel.mark_subject_ends_inheritable()?;
        channel.mark_observer_ends_inheritable()?;

        // The observer process first, so the runner holds its creation handle
        // and is the only party that can hand it to the supervisor.
        let mut observer_argv = vec![
            "--role".to_owned(),
            "observer".to_owned(),
            "--phase".to_owned(),
            "observe".to_owned(),
            "--facts".to_owned(),
            facts.to_string_lossy().into_owned(),
            "--nonce".to_owned(),
            nonce.clone(),
            "--deadline-ms".to_owned(),
            deadline_ms.to_string(),
            "--observation-request".to_owned(),
            channel
                .request_read()
                .map(|handle| (handle as usize).to_string())
                .unwrap_or_default(),
            "--observation-response".to_owned(),
            channel
                .response_write()
                .map(|handle| (handle as usize).to_string())
                .unwrap_or_default(),
        ];
        if name == "supervisor-crash" {
            observer_argv.extend(["--crash-boundary".to_owned(), self.path(name, "crash-boundary.txt").to_string_lossy().into_owned()]);
        }
        let env = self.whitelist_env();
        let request_read = channel.request_read().unwrap_or(std::ptr::null_mut());
        let response_write = channel.response_write().unwrap_or(std::ptr::null_mut());
        let executable = std::env::current_exe()
            .context("could not resolve the black box executable")?;
        let request = win::SpawnRequest {
            program: &executable.to_string_lossy(),
            args: &observer_argv,
            env: &env,
            cwd: Some(&self.dir),
            inherit: &[request_read, response_write],
            suspended: false,
        };
        let observer = win::spawn(&request)?;
        // The observer process is held in the harness — outside the scenario
        // call frame — so a panic unwinding out of a scenario still leaves
        // its creation handles in this runner's hands. Its stdio drains are
        // joined by `Harness::finish_observation`, after the observer has
        // actually run; joining them here would block until the observer
        // exits and leave no reader for any hand-off the supervisor sends.
        // The observer's real handle, narrowed to exactly what the subject
        // needs: enough to duplicate into, and nothing that could end it.
        // Narrowed to exactly PROCESS_DUP_HANDLE, then made inheritable so it
        // can enter the subject's HANDLE_LIST. Nothing wider is ever granted.
        let observer_for_subject =
            observation::dup_handle_with_access(observer.process, observation::OBSERVER_TARGET_ACCESS)?;
        win::set_inheritable(observer_for_subject)?;
        // The process handle is still needed after duplication — it is what
        // the harness holds for the bounded ending — so the child moves into
        // the harness only here, after every use of its fields.
        self.observing = Some(observer);

        let subject_args = vec![
            "--observer-handle".to_owned(),
            (observer_for_subject as usize).to_string(),
            "--observer-nonce".to_owned(),
            nonce.clone(),
            "--observation-request".to_owned(),
            channel
                .request_write()
                .map(|handle| (handle as usize).to_string())
                .unwrap_or_default(),
            "--observation-response".to_owned(),
            channel
                .response_read()
                .map(|handle| (handle as usize).to_string())
                .unwrap_or_default(),
        ];

        Ok(Observation {
            nonce,
            facts,
            channel,
            observer_for_subject,
            subject_args,
        })
    }

    fn spawn_observer(
        &self,
        name: &str,
        phase: &str,
        inherit: &[win::Handle],
        values: &[(&str, String)],
    ) -> Result<PathBuf> {
        struct Copies(Vec<win::Handle>);
        impl Drop for Copies {
            fn drop(&mut self) { for handle in self.0.drain(..) { win::close(handle); } }
        }
        let mut copies = Copies(Vec::new());
        let facts = self.path(name, &format!("observer.{phase}.facts.json"));
        let mut args: Vec<String> = vec![
            "--role".to_owned(),
            "observer".to_owned(),
            "--phase".to_owned(),
            phase.to_owned(),
            "--facts".to_owned(),
            facts.to_string_lossy().into_owned(),
        ];
        for (flag, value) in values {
            args.push((*flag).to_owned());
            if matches!(*flag, "--job" | "--outsider-job" | "--probe" | "--outsider") {
                let source = value.parse::<usize>()? as win::Handle;
                if !inherit.contains(&source) { bail!("observer handle is not in the explicit inheritance grant"); }
                let access = if matches!(*flag, "--job" | "--outsider-job") { 0x0004 } else { observation::OBSERVER_PROCESS_ACCESS };
                let copy = observation::dup_handle_with_access(source, access)?;
                copies.0.push(copy);
                win::set_inheritable(copy)?;
                args.push((copy as usize).to_string());
            } else {
                args.push(value.clone());
            }
        }
        let env = self.whitelist_env();
        let executable = std::env::current_exe()
            .context("could not resolve the black box executable")?;
        let request = win::SpawnRequest {
            program: &executable.to_string_lossy(),
            args: &args,
            env: &env,
            cwd: Some(&self.dir),
            inherit: &copies.0,
            suspended: false,
        };
        let mut child = win::spawn(&request)?;
        if child.wait(60_000)?.is_none() {
            child.terminate()?;
            if child.wait(5_000)?.is_none() { bail!("control observer failed to exit after owned-handle termination"); }
            bail!("control observer exceeded its deadline");
        }
        if !child.join_io() { bail!("control observer stdio workers were not reaped"); }
        Ok(facts)
    }

    /// End the round: the runner's own job, the runner's own handle, bounded.
    fn finish_outsider(&mut self) -> Value {
        if let Some(facts) = self.outsider_final.as_ref() {
            return facts.clone();
        }
        let pid = self.outsider_pid();
        let alive_before = self.outsider_alive();
        let mut facts = json!({
            "pid": pid,
            "aliveBeforeTeardown": alive_before,
            "job": "runner-exclusive-job",
        });
        if let Some(child) = self.outsider.as_mut() {
            let terminated = self.outsider_job.terminate().is_ok();
            let exited = child.wait(10_000).ok().flatten();
            if !child.join_io() {
                self.unverified_cleanup
                    .push("the outsider's stdio drains did not reap cleanly".to_owned());
            }
            facts["terminatedByRunnerJob"] = json!(terminated);
            facts["exitedWithinDeadline"] = json!(exited.is_some());
            facts["exitCode"] = json!(exited);
            if !terminated || exited.is_none() {
                self.unverified_cleanup
                    .push("the controlled outsider did not exit within the cleanup deadline"
                        .to_owned());
            }
        }
        self.outsider = None;
        self.outsider_final = Some(facts.clone());
        facts
    }
}

// ------------------------------------------------------- supervisor runner

#[cfg(windows)]
struct SupervisorRun {
    exit_code: Option<u32>,
    duration_ms: u128,
    plan_sha_before: String,
    plan_sha_after: String,
    status_present: bool,
    events: Vec<Value>,
    /// Read from the handle the runner holds: whether the supervisor sits in
    /// some job. The supervisor-tree deduction depends on this being false.
    supervisor_in_any_job: bool,
    /// The independent observer's verdicts for this run, when observation was
    /// requested.
    observation: Option<Checks>,
}

#[cfg(windows)]
impl Harness {
    /// Run the supervisor once against a frozen plan. Raw child output is
    /// piped, counted and discarded; only the status file is parsed.
    fn run_supervisor(
        &mut self,
        name: &str,
        plan_text: &str,
        deadline_ms: u64,
        extra_args: &[String],
        status_override: Option<&Path>,
        observe: bool,
    ) -> Result<SupervisorRun> {
        let dir = self.scenario_dir(name);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        let plan_path = dir.join("plan.json");
        let status_path = status_override
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.join("status.jsonl"));
        std::fs::write(&plan_path, plan_text)
            .with_context(|| format!("could not write {}", plan_path.display()))?;
        let plan_sha_before = sha256_hex(plan_text.as_bytes());

        let mut args: Vec<String> = vec![
            "--plan".to_owned(),
            plan_path.to_string_lossy().into_owned(),
            "--status".to_owned(),
            status_path.to_string_lossy().into_owned(),
            "--total-deadline-ms".to_owned(),
            deadline_ms.to_string(),
        ];
        args.extend(extra_args.iter().cloned());

        // The observation channel is created first so the observer exists
        // before the supervisor does: only the runner can hand the observer's
        // real handle to the subject.
        let observation = if observe {
            Some(self.start_observation(name, deadline_ms + 30_000)?)
        } else {
            None
        };
        let mut args = args;
        let inherit: Vec<win::Handle> = observation
            .as_ref()
            .map(|observation| observation.subject_inherit())
            .unwrap_or_default();
        if let Some(observation) = observation.as_ref() {
            args.extend(observation.subject_args());
        }

        let env = self.whitelist_env();
        let started = Instant::now();
        let request = win::SpawnRequest {
            program: &self.supervisor.to_string_lossy(),
            args: &args,
            env: &env,
            cwd: Some(&dir),
            inherit: &inherit,
            suspended: false,
        };
        let mut child = win::spawn(&request)?;
        let supervisor_in_any_job =
            win::is_in_job(child.process, std::ptr::null_mut()).unwrap_or(true);
        // The supervised supervisor is held in the harness — outside this
        // call frame — so a panic unwinding out of a scenario still leaves
        // its creation handle in this runner's hands for a handle-based,
        // bounded ending.
        self.supervised = Some(child);
        // The supervisor's own total deadline is the inner bound; this outer
        // bound only exists so a wedged run cannot hold the round open.
        let exit_code = match self.supervised.as_ref() {
            Some(child) => child
                .wait((deadline_ms + 20_000).min(u32::MAX as u64) as u32)
                .ok()
                .flatten(),
            None => None,
        };
        let reaped = match self.supervised.as_mut() {
            Some(child) => child.join_io(),
            None => true,
        };
        if !reaped {
            self.unverified_cleanup.push(
                "the supervisor's stdio drains did not reap cleanly".to_owned(),
            );
        }
        // The run is over and verified as far as it will be; the harness
        // releases its handle here, so the next run starts clean.
        let stdout_bytes = self
            .supervised
            .as_ref()
            .map(|child| child.stdout_bytes.load(Ordering::Relaxed))
            .unwrap_or(0);
        let stderr_bytes = self
            .supervised
            .as_ref()
            .map(|child| child.stderr_bytes.load(Ordering::Relaxed))
            .unwrap_or(0);
        self.supervised = None;
        let duration_ms = started.elapsed().as_millis();
        let (status_present, events) = Self::read_status(&status_path);
        let plan_sha_after = std::fs::read(&plan_path)
            .map(|bytes| sha256_hex(&bytes))
            .unwrap_or_else(|_| "unreadable".to_owned());

        self.discarded_stdout_bytes += stdout_bytes;
        self.discarded_stderr_bytes += stderr_bytes;

        let observation_checks = match observation {
            Some(observation) => Some(self.finish_observation(observation)),
            None => None,
        };

        Ok(SupervisorRun {
            exit_code,
            duration_ms,
            plan_sha_before,
            plan_sha_after,
            status_present,
            events,
            supervisor_in_any_job,
            observation: observation_checks,
        })
    }
}

// --------------------------------------------------------------- observer

#[cfg(windows)]
fn fixture_facts(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&text).ok()
}

#[cfg(windows)]
fn fixture_bool(path: &Path, key: &str) -> Option<bool> {
    bool_field(&fixture_facts(path)?, key)
}

#[cfg(windows)]
fn fixture_pid(path: &Path) -> Option<u32> {
    u64_field(&fixture_facts(path)?, "fixturePid").map(|value| value as u32)
}

/// One plan step, with the fields the supervisor's `deny_unknown_fields`
/// schema accepts and nothing else.
#[cfg(windows)]
fn step_json(
    tag: &str,
    mode: &str,
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &Value,
    extra: Value,
) -> Value {
    let mut value = json!({
        "tag": tag,
        "mode": mode,
        "program": program,
        "args": args,
        "cwd": cwd.to_string_lossy(),
        "env": env,
    });
    if let (Some(target), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        for (key, item) in extra {
            target.insert(key.clone(), item.clone());
        }
    }
    value
}

/// The "same job" deduction for the supervisor's own tree. It is a deduction,
/// not a handle comparison, and it is labelled that way in the report: the
/// supervisor's job is anonymous and its children inherit exactly three stdio
/// pipe handles, so no outside process can hold a job handle or a descendant
/// handle. See the plan document, section 5.3.
#[cfg(windows)]
fn record_tree_deduction(
    checks: &mut Checks,
    facts: &Path,
    runner_in_job: Option<bool>,
    supervisor_in_job: Option<bool>,
) {
    let value = fixture_facts(facts);
    let fixture_in = value.as_ref().and_then(|value| bool_field(value, "inAnyJob"));
    let grandchild_in = value
        .as_ref()
        .and_then(|value| bool_field(value, "grandchildInAnyJob"));
    checks.verify(
        "supervisorTree.stepInSomeJob",
        fixture_in == Some(true),
        "the supervised step, a process independent of the supervisor, reports that it sits in a job",
    );
    checks.verify(
        "supervisorTree.grandchildInSomeJob",
        grandchild_in == Some(true),
        "the grandchild the step created with its own creation handle reports that it sits in a job",
    );
    if runner_in_job == Some(false)
        && supervisor_in_job == Some(false)
        && fixture_in == Some(true)
        && grandchild_in == Some(true)
    {
        // A deduction is not a proof: it cannot distinguish jobs when more
        // than one exists, cannot read the job's limit flags, and rests on
        // the fixture's own report. The two kernel facts above carry the
        // weight they can carry; this does not become evidence.
        checks.not_covered(
            "supervisorTree.sameJob",
            "with the runner and the supervisor both verified outside every job, the supervisor's job is the only one on the ancestry chain, so the step and its grandchild most likely share it. This is a deduction from two processes' self-reports, not a job-handle comparison: it cannot distinguish jobs when more than one exists, cannot read the job's limit flags, and no third party holds a handle to cross-check. Only an observation interface that hands the observer a real job handle turns this into evidence.",
        );
    } else {
        checks.not_covered(
            "supervisorTree.sameJob",
            "the ancestry precondition is not verified (the runner or the supervisor could not be shown to sit outside every job), so membership in some job no longer implies membership in the supervisor's job",
        );
    }
}

/// The blocked supervisor-tree checks. Reported as not covered, with the exact
/// interface that would unblock them, instead of being faked.
#[cfg(windows)]
fn record_supervisor_tree_blocks(h: &mut Harness) {
    for name in ["normal", "supervisor-crash"] {
        if !h.scenarios.iter().any(|scenario| scenario["name"] == name && scenario["outcome"] == "pass") {
            h.record_blocked(name, "this round has no passing independent real-handle observation for this scenario",
                "run this scenario with the frozen observer, fixture and supervisor binaries");
        }
    }
    // Rejected on purpose: a named job object. The named job disappearing only
    // proves the last handle closed, never that each descendant exited on a
    // real handle, so it cannot stand in for a bounded wait. A production
    // supervisor also should not gain a test-only bypass.
    h.record_blocked(
        "assignmentFailure",
        "every child is created by the supervisor itself, suspended, and assigned before its first instruction, so no outside process can pre-place a tested child in a foreign job. The only remaining injection — placing the supervisor inside a non-breakaway outer job — is forbidden because the outer job would then contain the tested subtree and mask the tested job's own behaviour.",
        "no interface proposed on purpose: injecting this failure needs a fault switch inside the production supervisor, which is exactly the test-only bypass this design refuses to add. It stays NotCovered rather than being faked.",
    );
}

// -------------------------------------------------------------- scenarios

#[cfg(windows)]
const SCENARIOS: &[&str] = &[
    "preconditions",
    "control-tree",
    "normal",
    "exit-3",
    "no-ready",
    "malformed-readiness",
    "flood-over-cap",
    "stdin-unread",
    "total-deadline",
    "output-substitute-missing",
    "invalid-plan",
    "status-unwritable",
    "spawn-fail",
    "supervisor-crash",
    "outsider-final",
];

#[cfg(windows)]
fn run_scenario(h: &mut Harness, name: &str) -> Value {
    let started = Instant::now();
    h.discarded_stdout_bytes = 0;
    h.discarded_stderr_bytes = 0;
    let checks = match name {
        "preconditions" => scenario_preconditions(h),
        "control-tree" => scenario_control_tree(h),
        "normal" => scenario_normal(h),
        "exit-3" => scenario_exit_three(h),
        "no-ready" => scenario_no_ready(h),
        "malformed-readiness" => scenario_malformed_readiness(h),
        "flood-over-cap" => scenario_flood(h),
        "stdin-unread" => scenario_stdin_unread(h),
        "total-deadline" => scenario_total_deadline(h),
        "output-substitute-missing" => scenario_output_substitute(h),
        "invalid-plan" => scenario_invalid_plan(h),
        "status-unwritable" => scenario_status_unwritable(h),
        "spawn-fail" => scenario_spawn_fail(h),
        "supervisor-crash" => scenario_supervisor_crash(h),
        "outsider-final" => scenario_outsider_final(h),
        other => {
            let mut checks = Checks::new();
            checks.not_covered("scenario", format!("unknown scenario {other:?}"));
            checks
        }
    };
    let outcome = checks.outcome();
    let mut value = json!({
        "name": name,
        "durationMs": started.elapsed().as_millis(),
        "outcome": outcome,
        "checks": checks.items,
        "rawOutput": {
            "stdoutBytesDiscardedInMemory": h.discarded_stdout_bytes,
            "stderrBytesDiscardedInMemory": h.discarded_stderr_bytes,
            "writtenToDisk": false,
        },
    });
    // The outsider must outlive every scenario: it lives in a different job,
    // so a supervisor that reached outside its own job would show up here.
    if name != "outsider-final" && !h.outsider_alive() {
        let survived = json!({
            "name": "outsider.survivedScenario",
            "verdict": "failed",
            "detail": "the controlled outsider is no longer running after this scenario",
        });
        let outcome = if let Some(items) = value
            .get_mut("checks")
            .and_then(Value::as_array_mut)
        {
            items.push(survived);
            if items.iter().any(|item| item["verdict"] == "failed") {
                "fail"
            } else {
                "blocked"
            }
        } else {
            "fail"
        };
        if let Some(object) = value.as_object_mut() {
            object.insert("outcome".to_owned(), json!(outcome));
        }
    }
    value
}

/// A harness self-check: the runner must be outside every job, the supervisor
/// must be too, and the whole chain must be drivable at all.
#[cfg(windows)]
fn scenario_preconditions(h: &mut Harness) -> Checks {
    let mut checks = Checks::new();
    let name = "preconditions";

    checks.verify(
        "runner.notInAnyJob",
        h.runner_in_job == Some(false),
        "the runner sits in no job, which the supervisor-tree deduction depends on",
    );

    let env = json!(h.whitelist_env());
    let args = vec![
        "--facts".to_owned(),
        h.path(name, "smoke.facts.json").to_string_lossy().into_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];
    let plan = json!({"steps": [step_json(
        "smoke",
        "run",
        &h.fixture.to_string_lossy(),
        &args,
        &h.scenario_dir(name),
        &env,
        json!({}),
    )]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let run = h.run_supervisor(name, &plan_text, 20_000, &[], None, false);

    match run {
        Ok(run) => {
            h.supervisor_in_job = Some(run.supervisor_in_any_job);
            checks.verify(
                "supervisor.notInAnyJob",
                run.supervisor_in_any_job == false,
                "the supervisor was created by the runner and sits in no job, which the supervisor-tree deduction also depends on",
            );
            checks.verify(
                "supervisor.drivable",
                run.exit_code == Some(0),
                format!("a trivial one-step plan settles green (exit {:?})", run.exit_code),
            );
            checks.verify(
                "supervisor.containmentVerified",
                final_event(&run.events)
                    .map(|event| event["containmentVerified"].as_bool() == Some(true))
                    .unwrap_or(false),
                "the supervisor reports a verified teardown for the trivial plan",
            );
            checks.verify(
                "plan.frozen",
                run.plan_sha_before == run.plan_sha_after,
                "the frozen plan is byte-identical before and after the run",
            );
        }
        Err(error) => checks.verify("supervisor.drivable", false, format!("{error:#}")),
    }

    checks.verify(
        "outsider.alive",
        h.outsider_alive(),
        "the controlled outsider is running before any scenario",
    );
    checks.verify(
        "whitelist.names",
        h.whitelist_names().len() >= 4,
        format!(
            "the child environment keeps only these variable names: {}",
            h.whitelist_names().join(", ")
        ),
    );
    checks
}

/// Observer calibration on a tree the runner owns end to end. This proves the
/// observer can tell "same job" from "different job" with real handles; it
/// does not substitute for observing the supervisor's own tree.
#[cfg(windows)]
fn scenario_control_tree(h: &mut Harness) -> Checks {
    let name = "control-tree";
    let mut checks = Checks::new();
    if std::fs::create_dir_all(h.scenario_dir(name)).is_err() {
        checks.verify("controlTree.directory", false, "could not create the scenario directory");
        return checks;
    }

    let job = match win::Job::new() {
        Ok(job) => job,
        Err(error) => {
            checks.verify("controlTree.job", false, format!("{error:#}"));
            return checks;
        }
    };
    checks.verify(
        "controlTree.job.killOnJobClose",
        job.kill_on_job_close_set().unwrap_or(false),
        "the runner's own control job really carries KILL_ON_JOB_CLOSE, which is the mechanism the supervisor relies on",
    );

    // A real, inheritable handle to the runner, so the fixture can hand the
    // grandchild's creation handle back without anybody opening a pid.
    let self_handle = match win::inheritable_self_handle() {
        Ok(handle) => handle,
        Err(error) => {
            checks.verify("controlTree.selfHandle", false, format!("{error:#}"));
            return checks;
        }
    };

    let fixture_facts = h.path(name, "control.fixture.facts.json");
    let grandchild_facts = h.path(name, "control.grandchild.facts.json");
    let handle_out = h.path(name, "control.handle.txt");
    let args = vec![
        "--facts".to_owned(),
        fixture_facts.to_string_lossy().into_owned(),
        "--grandchild".to_owned(),
        "--grandchild-facts".to_owned(),
        grandchild_facts.to_string_lossy().into_owned(),
        "--parent-handle".to_owned(),
        (self_handle as usize).to_string(),
        "--handle-out".to_owned(),
        handle_out.to_string_lossy().into_owned(),
        "--stay-alive".to_owned(),
    ];
    let env = h.whitelist_env();
    let request = win::SpawnRequest {
        program: &h.fixture.to_string_lossy(),
        args: &args,
        env: &env,
        cwd: Some(&h.dir),
        inherit: &[self_handle],
        suspended: true,
    };
    let mut fixture_child = match win::spawn_contained(&request, &job) {
        Ok(child) => child,
        Err(error) => {
            win::close(self_handle);
            checks.verify("controlTree.fixture", false, format!("{error:#}"));
            return checks;
        }
    };

    // The grandchild handle arrives the only legitimate way: created by the
    // fixture, duplicated into the runner, handed back as a value that is
    // valid in the runner's own table.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut grandchild: Option<win::Handle> = None;
    let mut grandchild_pid: Option<u32> = None;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&handle_out) {
            let mut parts = text.split_whitespace();
            if let (Some(handle), Some(pid)) = (parts.next(), parts.next()) {
                if let (Ok(handle), Ok(pid)) = (handle.parse::<usize>(), pid.parse::<u32>()) {
                    grandchild = Some(handle as win::Handle);
                    grandchild_pid = Some(pid);
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let Some(grandchild) = grandchild else {
        fixture_child.join_io();
        win::close(self_handle);
        checks.not_covered(
            "controlTree.grandchildHandle",
            "the fixture never handed back the grandchild's creation handle, so nothing downstream can be verified with real handles",
        );
        return checks;
    };
    if win::set_inheritable(grandchild).is_err() {
        win::close(grandchild);
        fixture_child.join_io();
        win::close(self_handle);
        checks.verify(
            "controlTree.grandchildHandle",
            false,
            "could not mark the handed-back handle inheritable",
        );
        return checks;
    }

    let fixture_pid_value = fixture_pid(&fixture_facts);
    checks.verify(
        "controlTree.fixture.factsWritten",
        fixture_facts.is_file(),
        "the control fixture wrote its structured facts file",
    );
    checks.verify(
        "controlTree.fixture.inSomeJob",
        fixture_bool(&fixture_facts, "inAnyJob") == Some(true),
        "the control fixture reports it sits in the runner's job",
    );
    checks.verify(
        "controlTree.grandchild.factsWritten",
        grandchild_facts.is_file(),
        "the control grandchild wrote its own structured facts file",
    );
    checks.verify(
        "controlTree.grandchild.inSomeJob",
        fixture_bool(&grandchild_facts, "inAnyJob") == Some(true),
        "the grandchild inherited the job from its parent, with no assignment of its own",
    );

    // Observer phase one: same job against a specific job handle, and the
    // kernel's own pid list cross-checked against pids the runner learned
    // from its own CreateProcessW calls.
    let outsider_handle = h.outsider_handle();
    let outsider_pid_value = h.outsider_pid();
    let inherit = [
        job.handle(),
        grandchild,
        outsider_handle,
        h.outsider_job.handle(),
    ];
    let values: Vec<(&str, String)> = vec![
        ("--job", (job.handle() as usize).to_string()),
        ("--probe", (grandchild as usize).to_string()),
        ("--outsider", (outsider_handle as usize).to_string()),
        ("--outsider-job", (h.outsider_job.handle() as usize).to_string()),
        (
            "--probe-pid",
            grandchild_pid.map(|pid| pid.to_string()).unwrap_or_default(),
        ),
        (
            "--fixture-pid",
            fixture_pid_value.map(|pid| pid.to_string()).unwrap_or_default(),
        ),
        (
            "--outsider-pid",
            outsider_pid_value.map(|pid| pid.to_string()).unwrap_or_default(),
        ),
    ];
    match h.spawn_observer(name, "control-before", &inherit, &values) {
        Ok(facts) => observer::merge(&facts, &mut checks),
        Err(error) => checks.verify("observer.control-before", false, format!("{error:#}")),
    }

    // Teardown: the runner's own job, then real bounded waits on the handles
    // the runner holds. A timeout is a failure, never a shrug.
    checks.verify(
        "controlTree.job.terminated",
        job.terminate().is_ok(),
        "the runner terminated its own control job by handle",
    );
    let fixture_exit = fixture_child.wait(10_000).ok().flatten();
    let grandchild_exit = win::wait_handle(grandchild, 10_000).ok().flatten();
    checks.verify(
        "controlTree.fixture.exited",
        fixture_exit.is_some(),
        "the control fixture exited within the cleanup deadline",
    );
    checks.verify(
        "controlTree.grandchild.exited",
        grandchild_exit.is_some(),
        "the control grandchild exited within the cleanup deadline",
    );
    // The job's own accounting, read by the runner itself, is the same
    // criterion the supervisor uses for its own verdict.
    let mut active_zero = false;
    let accounting_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < accounting_deadline {
        match job.active_processes_is_zero() {
            Ok(true) => {
                active_zero = true;
                break;
            }
            Ok(false) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    checks.verify(
        "controlTree.job.accountingZero",
        active_zero,
        "the runner's own job reports zero active processes after the teardown",
    );
    match job.process_ids() {
        Ok(pids) => checks.verify(
            "controlTree.job.membershipEmpty",
            pids.is_empty(),
            format!(
                "the runner's own job lists {} processes after the teardown",
                pids.len()
            ),
        ),
        Err(error) => checks.verify("controlTree.job.processIdList", false, format!("{error:#}")),
    }
    fixture_child.join_io();

    let inherit = [job.handle(), grandchild];
    let values: Vec<(&str, String)> = vec![
        ("--job", (job.handle() as usize).to_string()),
        ("--probe", (grandchild as usize).to_string()),
    ];
    match h.spawn_observer(name, "control-after", &inherit, &values) {
        Ok(facts) => observer::merge(&facts, &mut checks),
        Err(error) => checks.verify("observer.control-after", false, format!("{error:#}")),
    }

    win::close(grandchild);
    win::close(self_handle);
    checks
}

#[cfg(windows)]
fn scenario_normal(h: &mut Harness) -> Checks {
    let name = "normal";
    let mut checks = Checks::new();
    let dir = h.scenario_dir(name);
    let env = json!(h.whitelist_env());
    let program = h.fixture.to_string_lossy().into_owned();

    let ready_args = vec![
        "--facts".to_owned(),
        h.path(name, "ready.facts.json").to_string_lossy().into_owned(),
        "--ready-mode".to_owned(),
        "correct".to_owned(),
        "--stay-alive".to_owned(),
    ];
    let seed_file = h.path(name, "seed.json").to_string_lossy().into_owned();
    let seed_args = vec![
        "--facts".to_owned(),
        h.path(name, "seed.facts.json").to_string_lossy().into_owned(),
        "--substitute-out".to_owned(),
        seed_file.clone(),
        "--substitute-field".to_owned(),
        "projectId".to_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];
    let consume_args = vec![
        "--facts".to_owned(),
        h.path(name, "consume.facts.json")
            .to_string_lossy()
            .into_owned(),
        "--echo-args".to_owned(),
        "{seedProjectId}".to_owned(),
        "{daemonAddress}".to_owned(),
    ];
    // The observing fixture: a trusted synthetic step that creates its own
    // descendant, hands the descendant's real creation handle to the
    // observer through its supervisor-granted channel, and exits. The
    // verified exit is the freeze handshake — after it, that fixture creates
    // no further process.
    let observe_args = vec![
        "--facts".to_owned(),
        h.path(name, "observe.facts.json").to_string_lossy().into_owned(),
        "--grandchild".to_owned(),
        "--grandchild-facts".to_owned(),
        h.path(name, "observe.grandchild.facts.json")
            .to_string_lossy()
            .into_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];

    let plan = json!({"steps": [
        step_json("ready", "spawn", &program, &ready_args, &dir, &env, json!({"readyFromLog": true})),
        step_json("observe", "run", &program, &observe_args, &dir, &env, json!({"observeDescendants": true})),
        step_json("seed", "run", &program, &seed_args, &dir, &env, json!({
            "outputSubstitute": {
                "file": seed_file,
                "placeholder": "seedProjectId",
                "field": "projectId",
            }
        })),
        step_json("consume", "run", &program, &consume_args, &dir, &env, json!({})),
    ]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let run = match h.run_supervisor(name, &plan_text, 30_000, &[], None, true) {
        Ok(run) => run,
        Err(error) => {
            checks.verify("supervisor.spawned", false, format!("{error:#}"));
            return checks;
        }
    };

    checks.verify(
        "supervisor.exitCode",
        run.exit_code == Some(0),
        format!("the all-green plan exits 0 (got {:?})", run.exit_code),
    );
    let final_event = final_event(&run.events).cloned();
    checks.verify(
        "status.finalEvent",
        final_event.is_some(),
        "the status file always ends with a final event",
    );
    if let Some(event) = final_event.as_ref() {
        checks.verify(
            "final.containmentVerified",
            event["containmentVerified"].as_bool() == Some(true),
            "the supervisor reports a verified teardown",
        );
        checks.verify(
            "final.containmentConsistent",
            containment_consistent(event),
            "containmentVerified is not standing in for the teardown: terminated, allChildrenExited, jobActiveProcessesZero, startupCleanupVerified, ioWorkersExited and statusFileOk are all true too",
        );
        checks.verify(
            "final.notFailed",
            event["failedStep"].is_null()
                && event["failureClass"].is_null()
                && event["totalDeadlineExceeded"].as_bool() == Some(false),
            "no step failed and the total deadline was not exceeded",
        );
    }
    checks.verify(
        "status.stepEvents",
        run.events
            .iter()
            .filter(|event| event["event"] == "step")
            .count()
            == 4,
        "every step is recorded: one spawn start, the observing run, and two run results",
    );
    checks.verify(
        "status.readinessEvent",
        run.events
            .iter()
            .any(|event| event["event"] == "readiness" && event["ready"] == json!(true)),
        "readiness is reported for the spawn step",
    );
    checks.verify(
        "plan.frozen",
        run.plan_sha_before == run.plan_sha_after,
        "the frozen plan is byte-identical before and after the run",
    );
    checks.verify(
        "substitution.fileWritten",
        h.path(name, "seed.json").is_file(),
        "the run step wrote the file the outputSubstitute names",
    );

    // Both placeholders must have resolved: the echoed argv carries the
    // substituted values, so a literal brace would prove it did not.
    let consume_facts = fixture_facts(&h.path(name, "consume.facts.json"));
    let echoed = consume_facts
        .as_ref()
        .and_then(|value| value.get("echoedArgs"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    checks.verify(
        "substitution.seedProjectIdResolved",
        echoed
            .iter()
            .any(|argument| argument == "synthetic-seed-project"),
        "the placeholder from the earlier step's outputSubstitute reached a later step's argv",
    );
    checks.verify(
        "substitution.daemonAddressResolved",
        echoed
            .iter()
            .any(|argument| argument.starts_with("127.0.0.1:")),
        "the address from the readiness line reached a later step's argv",
    );

    checks.verify(
        "outsider.alive",
        h.outsider_alive(),
        "the controlled outsider survived the run untouched",
    );
    if let Some(mut observation) = run.observation {
        checks.items.append(&mut observation.items);
    }

    // The same-job question is now answered with real handles, not the
    // deduction that used to stand in for it: the supervisor hands the
    // observer every root child's creation handle and the exact job query
    // handle, the observing fixture hands over its own descendant, and the
    // observer checks each against the job's own process-id list. Those
    // results are recorded under observation.inExactJob and
    // observation.topologyMatchesHandles when the channel ran.
    checks
}

#[cfg(windows)]
fn scenario_exit_three(h: &mut Harness) -> Checks {
    let name = "exit-3";
    let mut checks = Checks::new();
    let dir = h.scenario_dir(name);
    let env = json!(h.whitelist_env());
    let program = h.fixture.to_string_lossy().into_owned();

    let marker = h.path(name, "after.marker.json").to_string_lossy().into_owned();
    let exit_args = vec![
        "--facts".to_owned(),
        h.path(name, "exit3.facts.json").to_string_lossy().into_owned(),
        "--exit-code".to_owned(),
        "3".to_owned(),
    ];
    let after_args = vec![
        "--facts".to_owned(),
        h.path(name, "after.facts.json").to_string_lossy().into_owned(),
        "--substitute-out".to_owned(),
        marker.clone(),
        "--substitute-field".to_owned(),
        "marker".to_owned(),
    ];
    let plan = json!({"steps": [
        step_json("exit3", "run", &program, &exit_args, &dir, &env, json!({})),
        step_json("after", "run", &program, &after_args, &dir, &env, json!({})),
    ]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let run = match h.run_supervisor(name, &plan_text, 20_000, &[], None, false) {
        Ok(run) => run,
        Err(error) => {
            checks.verify("supervisor.spawned", false, format!("{error:#}"));
            return checks;
        }
    };

    checks.verify(
        "supervisor.exitCode",
        run.exit_code == Some(1),
        format!("a failing step makes the run non-green (exit {:?})", run.exit_code),
    );
    let final_event = final_event(&run.events).cloned();
    if let Some(event) = final_event.as_ref() {
        checks.verify(
            "final.failureClass",
            event["failureClass"] == json!("exit-3"),
            "the failing step names itself through its own exit code",
        );
        checks.verify(
            "final.failedStep",
            event["failedStep"] == json!("exit3"),
            "the failing step's tag is recorded",
        );
        checks.verify(
            "final.containmentConsistent",
            containment_consistent(event),
            "a failed run still tears down and verifies",
        );
    } else {
        checks.verify("status.finalEvent", false, "the status file has no final event");
    }
    checks.verify(
        "status.stepRecorded",
        run.events.iter().any(|event| {
            event["event"] == "step"
                && event["tag"] == json!("exit3")
                && event["exitCode"] == json!(3)
        }),
        "the failing step's exit code is recorded",
    );
    checks.verify(
        "sideEffect.notRun",
        !std::path::Path::new(&marker).exists(),
        "no side-effect step runs after a failed step",
    );
    checks.verify(
        "outsider.alive",
        h.outsider_alive(),
        "the controlled outsider survived the failed run untouched",
    );
    checks
}

#[cfg(windows)]
fn scenario_no_ready(h: &mut Harness) -> Checks {
    let name = "no-ready";
    let mut checks = Checks::new();
    let env = json!(h.whitelist_env());
    let program = h.fixture.to_string_lossy().into_owned();

    // A child that exits without ever printing a readiness line.
    let eof_args = vec![
        "--facts".to_owned(),
        h.path(name, "eof.facts.json").to_string_lossy().into_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];
    let plan = json!({"steps": [step_json(
        "no-ready",
        "spawn",
        &program,
        &eof_args,
        &h.scenario_dir(name),
        &env,
        json!({"readyFromLog": true}),
    )]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    if let Ok(run) = h.run_supervisor(name, &plan_text, 15_000, &[], None, false) {
        checks.verify(
            "eof.exitCode",
            run.exit_code == Some(1),
            format!("a spawn step that never becomes ready is non-green (exit {:?})", run.exit_code),
        );
        checks.verify(
            "eof.failureClass",
            final_event(&run.events)
                .map(|event| event["failureClass"] == json!("readiness-eof"))
                .unwrap_or(false),
            "the readiness failure is attributed to the end of the child's output",
        );
        checks.verify(
            "eof.containmentConsistent",
            final_event(&run.events)
                .map(containment_consistent)
                .unwrap_or(false),
            "a readiness failure still tears down and verifies",
        );
    } else {
        checks.verify("eof.ran", false, "the supervisor could not be run");
    }

    // A child that stays alive and silent: the total deadline has to fire.
    let live_args = vec![
        "--facts".to_owned(),
        h.path(name, "live.facts.json").to_string_lossy().into_owned(),
        "--stay-alive".to_owned(),
    ];
    let plan = json!({"steps": [step_json(
        "no-ready",
        "spawn",
        &program,
        &live_args,
        &h.scenario_dir(name),
        &env,
        json!({"readyFromLog": true}),
    )]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    if let Ok(run) = h.run_supervisor(name, &plan_text, 4_000, &[], None, false) {
        checks.verify(
            "timeout.exitCode",
            run.exit_code == Some(1),
            format!("a silent long-lived child is non-green (exit {:?})", run.exit_code),
        );
        checks.verify(
            "timeout.failureClass",
            final_event(&run.events)
                .map(|event| event["failureClass"] == json!("readiness"))
                .unwrap_or(false),
            "the readiness wait is bounded by the total deadline, not by a hang",
        );
        checks.verify(
            "timeout.deadlineExceeded",
            final_event(&run.events)
                .and_then(|event| bool_field(event, "totalDeadlineExceeded"))
                .unwrap_or(false),
            "the deadline path is reported as a deadline, not as a readiness success",
        );
    } else {
        checks.verify("timeout.ran", false, "the supervisor could not be run");
    }
    checks
}

#[cfg(windows)]
fn scenario_malformed_readiness(h: &mut Harness) -> Checks {
    let name = "malformed-readiness";
    let mut checks = Checks::new();
    let env = json!(h.whitelist_env());
    let program = h.fixture.to_string_lossy().into_owned();

    let cases: [(&str, Vec<String>, bool); 4] = [
        (
            "wrong-pid",
            vec![
                "--facts".to_owned(),
                h.path(name, "wrong-pid.facts.json").to_string_lossy().into_owned(),
                "--ready-mode".to_owned(),
                "wrong-pid".to_owned(),
                "--exit-code".to_owned(),
                "0".to_owned(),
            ],
            false,
        ),
        (
            "wrong-protocol",
            vec![
                "--facts".to_owned(),
                h.path(name, "wrong-protocol.facts.json")
                    .to_string_lossy()
                    .into_owned(),
                "--ready-mode".to_owned(),
                "wrong-protocol".to_owned(),
                "--exit-code".to_owned(),
                "0".to_owned(),
            ],
            false,
        ),
        (
            "wrong-address",
            vec![
                "--facts".to_owned(),
                h.path(name, "wrong-address.facts.json")
                    .to_string_lossy()
                    .into_owned(),
                "--ready-mode".to_owned(),
                "wrong-address".to_owned(),
                "--exit-code".to_owned(),
                "0".to_owned(),
            ],
            false,
        ),
        (
            "wrong-then-valid",
            vec![
                "--facts".to_owned(),
                h.path(name, "wrong-then-valid.facts.json")
                    .to_string_lossy()
                    .into_owned(),
                "--ready-mode".to_owned(),
                "wrong-then-correct".to_owned(),
                "--ready-delay-ms".to_owned(),
                "400".to_owned(),
                "--stay-alive".to_owned(),
            ],
            true,
        ),
    ];

    for (label, args, expect_green) in cases {
        let plan = json!({"steps": [step_json(
            label,
            "spawn",
            &program,
            &args,
            &h.scenario_dir(name),
            &env,
            json!({"readyFromLog": true}),
        )]});
        let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
        let Ok(run) = h.run_supervisor(name, &plan_text, 15_000, &[], None, false) else {
            checks.verify(&format!("{label}.ran"), false, "the supervisor could not be run");
            continue;
        };
        let green = run.exit_code == Some(0);
        checks.verify(
            &format!("{label}.verdict"),
            green == expect_green,
            format!(
                "a readiness line with a {} is {} (exit {:?})",
                if expect_green {
                    "rejected first line followed by a valid one, which is accepted"
                } else {
                    "bad field"
                },
                if expect_green { "accepted" } else { "refused" },
                run.exit_code
            ),
        );
        if expect_green {
            checks.verify(
                &format!("{label}.readinessReported"),
                run.events
                    .iter()
                    .any(|event| event["event"] == "readiness" && event["ready"] == json!(true)),
                "readiness is reported once a valid line arrives",
            );
        } else {
            checks.verify(
                &format!("{label}.notReady"),
                !run.events
                    .iter()
                    .any(|event| event["event"] == "readiness" && event["ready"] == json!(true)),
                "the malformed line is never reported as ready",
            );
        }
    }
    checks
}

#[cfg(windows)]
fn scenario_flood(h: &mut Harness) -> Checks {
    let name = "flood-over-cap";
    let mut checks = Checks::new();
    let env = json!(h.whitelist_env());
    let program = h.fixture.to_string_lossy().into_owned();
    let args = vec![
        "--facts".to_owned(),
        h.path(name, "flood.facts.json").to_string_lossy().into_owned(),
        "--junk-bytes".to_owned(),
        "70000".to_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];
    let plan = json!({"steps": [step_json(
        "flood",
        "spawn",
        &program,
        &args,
        &h.scenario_dir(name),
        &env,
        json!({"readyFromLog": true}),
    )]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let run = match h.run_supervisor(name, &plan_text, 15_000, &[], None, false) {
        Ok(run) => run,
        Err(error) => {
            checks.verify("supervisor.spawned", false, format!("{error:#}"));
            return checks;
        }
    };
    checks.verify(
        "supervisor.exitCode",
        run.exit_code == Some(1),
        format!("a flood before any valid line is non-green (exit {:?})", run.exit_code),
    );
    checks.verify(
        "final.failureClass",
        final_event(&run.events)
            .map(|event| event["failureClass"] == json!("readiness-cap"))
            .unwrap_or(false),
        "more than 64 KiB before the first outcome is reported as a cap, not as a wait",
    );
    checks
}

#[cfg(windows)]
fn scenario_stdin_unread(h: &mut Harness) -> Checks {
    let name = "stdin-unread";
    let mut checks = Checks::new();
    let env = json!(h.whitelist_env());
    let program = h.fixture.to_string_lossy().into_owned();
    // Far past the pipe buffer, so the write really blocks until something
    // breaks the pipe.
    let stdin_text = "s".repeat(256 * 1024);

    let exit_args = vec![
        "--facts".to_owned(),
        h.path(name, "exits.facts.json").to_string_lossy().into_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];
    let second_args = vec![
        "--facts".to_owned(),
        h.path(name, "exits.second.facts.json")
            .to_string_lossy()
            .into_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];
    let plan = json!({"steps": [
        step_json("never-reads", "run", &program, &exit_args, &h.scenario_dir(name), &env,
            json!({"stdinText": stdin_text})),
        step_json("after", "run", &program, &second_args, &h.scenario_dir(name), &env, json!({})),
    ]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    if let Ok(run) = h.run_supervisor(name, &plan_text, 20_000, &[], None, false) {
        checks.verify(
            "exits.exitCode",
            run.exit_code == Some(0),
            format!("a child that exits without reading stdin does not wedge the run (exit {:?})", run.exit_code),
        );
        checks.verify(
            "exits.ioWorkersExited",
            final_event(&run.events)
                .and_then(|event| bool_field(event, "ioWorkersExited"))
                .unwrap_or(false),
            "the blocked stdin write is released once the child is gone",
        );
        checks.verify(
            "exits.continued",
            run.events.iter().any(|event| {
                event["event"] == "step" && event["tag"] == json!("after")
            }),
            "the run continued past the step whose stdin was never read",
        );
    } else {
        checks.verify("exits.ran", false, "the supervisor could not be run");
    }

    // The harder shape: the child stays alive and never reads, so the write
    // only unblocks at teardown.
    let live_args = vec![
        "--facts".to_owned(),
        h.path(name, "live.facts.json").to_string_lossy().into_owned(),
        "--stay-alive".to_owned(),
    ];
    let plan = json!({"steps": [
        step_json("never-reads-live", "spawn", &program, &live_args, &h.scenario_dir(name), &env,
            json!({"stdinText": stdin_text})),
        step_json("after", "run", &program, &second_args, &h.scenario_dir(name), &env, json!({})),
    ]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    if let Ok(run) = h.run_supervisor(name, &plan_text, 20_000, &[], None, false) {
        checks.verify(
            "live.exitCode",
            run.exit_code == Some(0),
            format!("a live child that never reads stdin does not wedge the run (exit {:?})", run.exit_code),
        );
        checks.verify(
            "live.ioWorkersExited",
            final_event(&run.events)
                .and_then(|event| bool_field(event, "ioWorkersExited"))
                .unwrap_or(false),
            "the blocked stdin write is released by the teardown breaking the pipe, within the cleanup deadline",
        );
        checks.verify(
            "live.bounded",
            run.duration_ms < 60_000,
            format!("the run finished in {} ms rather than hanging", run.duration_ms),
        );
    } else {
        checks.verify("live.ran", false, "the supervisor could not be run");
    }
    checks
}

#[cfg(windows)]
fn scenario_total_deadline(h: &mut Harness) -> Checks {
    let name = "total-deadline";
    let mut checks = Checks::new();
    let env = json!(h.whitelist_env());
    let program = h.fixture.to_string_lossy().into_owned();
    let args = vec![
        "--facts".to_owned(),
        h.path(name, "sleep.facts.json").to_string_lossy().into_owned(),
        "--sleep-ms".to_owned(),
        "60000".to_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];
    let plan = json!({"steps": [step_json(
        "sleep",
        "run",
        &program,
        &args,
        &h.scenario_dir(name),
        &env,
        json!({}),
    )]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let run = match h.run_supervisor(name, &plan_text, 3_000, &[], None, false) {
        Ok(run) => run,
        Err(error) => {
            checks.verify("supervisor.spawned", false, format!("{error:#}"));
            return checks;
        }
    };
    checks.verify(
        "supervisor.exitCode",
        run.exit_code == Some(1),
        format!("a step that outruns the total deadline is non-green (exit {:?})", run.exit_code),
    );
    let final_event = final_event(&run.events).cloned();
    checks.verify(
        "status.finalEvent",
        final_event.is_some(),
        "the deadline path still writes a final event",
    );
    if let Some(event) = final_event.as_ref() {
        checks.verify(
            "final.failureClass",
            event["failureClass"] == json!("total-deadline"),
            "the deadline is attributed to the total deadline",
        );
        checks.verify(
            "final.deadlineExceeded",
            bool_field(event, "totalDeadlineExceeded").unwrap_or(false),
            "the deadline is reported as a deadline",
        );
        checks.verify(
            "final.failedStep",
            event["failedStep"] == json!("sleep"),
            "the step that outran the deadline names itself",
        );
        checks.verify(
            "final.containmentConsistent",
            containment_consistent(event),
            "the deadline path still tears down and verifies",
        );
    }
    checks.verify(
        "run.bounded",
        run.duration_ms < 60_000,
        format!("the run finished in {} ms rather than waiting for the sleeping child", run.duration_ms),
    );
    checks
}

#[cfg(windows)]
fn scenario_output_substitute(h: &mut Harness) -> Checks {
    let name = "output-substitute-missing";
    let mut checks = Checks::new();
    let env = json!(h.whitelist_env());
    let program = h.fixture.to_string_lossy().into_owned();
    let missing = h.path(name, "never-written.json").to_string_lossy().into_owned();
    let args = vec![
        "--facts".to_owned(),
        h.path(name, "nosub.facts.json").to_string_lossy().into_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];
    let plan = json!({"steps": [step_json(
        "nosub",
        "run",
        &program,
        &args,
        &h.scenario_dir(name),
        &env,
        json!({
            "outputSubstitute": {
                "file": missing,
                "placeholder": "seedProjectId",
                "field": "projectId",
            }
        }),
    )]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let run = match h.run_supervisor(name, &plan_text, 15_000, &[], None, false) {
        Ok(run) => run,
        Err(error) => {
            checks.verify("supervisor.spawned", false, format!("{error:#}"));
            return checks;
        }
    };
    checks.verify(
        "supervisor.exitCode",
        run.exit_code == Some(1),
        format!("a step that succeeds but produces no substitute is non-green (exit {:?})", run.exit_code),
    );
    checks.verify(
        "final.failureClass",
        final_event(&run.events)
            .map(|event| event["failureClass"] == json!("output-substitute"))
            .unwrap_or(false),
        "the missing substitution is attributed to the substitution, not to the exit code",
    );
    checks
}

#[cfg(windows)]
fn scenario_invalid_plan(h: &mut Harness) -> Checks {
    let name = "invalid-plan";
    let mut checks = Checks::new();
    let program = h.fixture.to_string_lossy().into_owned();
    let facts = h.path(name, "child.facts.json").to_string_lossy().into_owned();

    let cases: [(&str, String, bool); 7] = [
        ("bad-json", "{\"steps\": [".to_owned(), false),
        (
            "bad-mode",
            json!({"steps": [{"tag": "a", "mode": "exec", "program": program}]}).to_string(),
            false,
        ),
        (
            "duplicate-tag",
            json!({"steps": [
                {"tag": "a", "mode": "run", "program": program, "args": ["--facts", facts]},
                {"tag": "a", "mode": "run", "program": program, "args": ["--facts", facts]},
            ]})
            .to_string(),
            false,
        ),
        (
            "empty-tag",
            json!({"steps": [{"tag": "  ", "mode": "run", "program": program}]}).to_string(),
            false,
        ),
        (
            "ready-on-run",
            json!({"steps": [{"tag": "a", "mode": "run", "program": program, "readyFromLog": true}]})
                .to_string(),
            false,
        ),
        (
            "unknown-field",
            json!({"steps": [{"tag": "a", "mode": "run", "program": program, "bogus": 1}]}).to_string(),
            false,
        ),
        ("empty-steps", json!({"steps": []}).to_string(), true),
    ];

    for (label, plan_text, expect_green) in cases {
        let sub = format!("{name}-{label}");
        match h.run_supervisor(&sub, &plan_text, 10_000, &[], None, false) {
            Ok(run) => {
                let status = h.path(&sub, "status.jsonl");
                let green = run.exit_code == Some(0);
                if label == "empty-steps" {
                    // Main's ruling: an empty acceptance plan is a harness
                    // defect and must be refused. The supervisor has not been
                    // changed yet, so this records what it does today and
                    // states the ruling separately rather than pretending the
                    // refusal already happens.
                    checks.verify(
                        "empty-steps.recorded",
                        green || run.exit_code == Some(1),
                        format!(
                            "an empty plan was run and recorded: exit {:?}, status file {}, containmentVerified {:?}",
                            run.exit_code,
                            run.status_present,
                            final_event(&run.events)
                                .and_then(|event| bool_field(event, "containmentVerified"))
                        ),
                    );
                    if run.exit_code == Some(1) && !run.status_present {
                        checks.verify(
                            "empty-steps.refused",
                            true,
                            "main's ruling is in effect: an empty acceptance plan is refused before anything is spawned",
                        );
                    } else {
                        checks.not_covered(
                            "empty-steps.refused",
                            "main ruled that an empty acceptance plan must be refused, and main will change the supervisor to do it. The supervisor has not been changed yet, so this run records today's behaviour and cannot report the ruling as satisfied.",
                        );
                    }
                    continue;
                }
                checks.verify(
                    &format!("{label}.verdict"),
                    green == expect_green,
                    format!(
                        "{} (exit {:?})",
                        if expect_green {
                            "an empty plan settles green"
                        } else {
                            "prevalidation refuses the plan before anything is spawned"
                        },
                        run.exit_code
                    ),
                );
                if expect_green {
                    checks.verify(
                        &format!("{label}.statusWritten"),
                        run.status_present
                            && final_event(&run.events)
                                .map(|event| event["containmentVerified"].as_bool() == Some(true))
                                .unwrap_or(false),
                        "an empty plan still writes a verified teardown",
                    );
                } else {
                    checks.verify(
                        &format!("{label}.noStatusFile"),
                        !status.exists(),
                        "a refused plan produces no status file at all",
                    );
                    checks.verify(
                        &format!("{label}.nothingSpawned"),
                        !h.path(&sub, "child.facts.json").exists(),
                        "a refused plan spawns no child, so no fixture facts file appears",
                    );
                }
            }
            Err(error) => {
                checks.verify(&format!("{label}.ran"), false, format!("{error:#}"));
            }
        }
    }
    checks
}

#[cfg(windows)]
fn scenario_status_unwritable(h: &mut Harness) -> Checks {
    let name = "status-unwritable";
    let mut checks = Checks::new();
    let env = json!(h.whitelist_env());
    let program = h.fixture.to_string_lossy().into_owned();
    let args = vec![
        "--facts".to_owned(),
        h.path(name, "child.facts.json").to_string_lossy().into_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];
    let plan = json!({"steps": [step_json(
        "smoke",
        "run",
        &program,
        &args,
        &h.scenario_dir(name),
        &env,
        json!({}),
    )]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let status = h.path(name, "no-such-directory").join("status.jsonl");
    let run = match h.run_supervisor(name, &plan_text, 10_000, &[], Some(&status), false) {
        Ok(run) => run,
        Err(error) => {
            checks.verify("supervisor.spawned", false, format!("{error:#}"));
            return checks;
        }
    };
    checks.verify(
        "supervisor.exitCode",
        run.exit_code == Some(1),
        format!("a status file that cannot be created fails the run (exit {:?})", run.exit_code),
    );
    checks.verify(
        "status.notWritten",
        !status.exists(),
        "no status file appears at the unusable path",
    );
    checks.verify(
        "nothingSpawned",
        !h.path(name, "child.facts.json").exists(),
        "no child was created, so no fixture facts file appears",
    );
    checks.verify(
        "outsider.alive",
        h.outsider_alive(),
        "the controlled outsider survived untouched",
    );
    checks
}

#[cfg(windows)]
fn scenario_spawn_fail(h: &mut Harness) -> Checks {
    let name = "spawn-fail";
    let mut checks = Checks::new();
    let env = json!(h.whitelist_env());
    let missing = h
        .path(name, "does-not-exist.exe")
        .to_string_lossy()
        .into_owned();
    let plan = json!({"steps": [step_json(
        "nope",
        "run",
        &missing,
        &[],
        &h.scenario_dir(name),
        &env,
        json!({}),
    )]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let run = match h.run_supervisor(name, &plan_text, 15_000, &[], None, false) {
        Ok(run) => run,
        Err(error) => {
            checks.verify("supervisor.spawned", false, format!("{error:#}"));
            return checks;
        }
    };
    checks.verify(
        "supervisor.exitCode",
        run.exit_code == Some(1),
        format!("a step whose program cannot be created is non-green (exit {:?})", run.exit_code),
    );
    checks.verify(
        "status.spawnClass",
        run.events.iter().any(|event| {
            event["event"] == "step"
                && event["failed"] == json!(true)
                && event["class"] == json!("spawn")
                && event["tag"] == json!("nope")
        }),
        "the failed spawn is recorded on the step itself",
    );
    checks.verify(
        "final.failureClass",
        final_event(&run.events)
            .map(|event| event["failureClass"] == json!("spawn"))
            .unwrap_or(false),
        "the failure is attributed to the spawn",
    );
    checks.verify(
        "final.containmentConsistent",
        final_event(&run.events)
            .map(containment_consistent)
            .unwrap_or(false),
        "a spawn failure still tears down and verifies",
    );
    checks
}

/// The crash path. The runner terminates the supervisor on the creation handle
/// it owns, then proves what it can with the handles it owns and reports the
/// rest as blocked.
#[cfg(windows)]
fn scenario_supervisor_crash(h: &mut Harness) -> Checks {
    let name = "supervisor-crash";
    let mut checks = Checks::new();
    let dir = h.scenario_dir(name);
    if std::fs::create_dir_all(&dir).is_err() {
        checks.verify("directory", false, "could not create the scenario directory");
        return checks;
    }
    let env = json!(h.whitelist_env());
    let program = h.fixture.to_string_lossy().into_owned();
    let args = vec![
        "--facts".to_owned(),
        h.path(name, "live.facts.json").to_string_lossy().into_owned(),
        "--ready-mode".to_owned(),
        "correct".to_owned(),
        "--stay-alive".to_owned(),
    ];
    // The observing fixture hands its descendant's real creation handle to
    // the observer and exits; the supervisor's later crash then has to be
    // answered with a bounded wait on that descendant's handle, not with an
    // inference from the job disappearing.
    let observe_args = vec![
        "--facts".to_owned(),
        h.path(name, "observe.facts.json").to_string_lossy().into_owned(),
        "--grandchild".to_owned(),
        "--grandchild-facts".to_owned(),
        h.path(name, "observe.grandchild.facts.json")
            .to_string_lossy()
            .into_owned(),
        "--exit-code".to_owned(),
        "0".to_owned(),
    ];
    let plan = json!({"steps": [
        step_json("live", "spawn", &program, &args, &dir, &env, json!({"readyFromLog": true})),
        step_json("observe", "run", &program, &observe_args, &dir, &env, json!({"observeDescendants": true})),
    ]});
    let plan_text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let plan_sha_before = sha256_hex(plan_text.as_bytes());
    let plan_path = dir.join("plan.json");
    let status_path = dir.join("status.jsonl");
    if std::fs::write(&plan_path, &plan_text).is_err() {
        checks.verify("plan.written", false, "could not write the plan");
        return checks;
    }
    // The observation channel exists before the supervisor does, so the runner
    // is the only party that can hand the observer's real handle over.
    let observation = match h.start_observation(name, 90_000) {
        Ok(observation) => observation,
        Err(error) => {
            checks.verify("observation.channel", false, format!("{error:#}"));
            return checks;
        }
    };
    let mut supervisor_args = vec![
        "--plan".to_owned(),
        plan_path.to_string_lossy().into_owned(),
        "--status".to_owned(),
        status_path.to_string_lossy().into_owned(),
        "--total-deadline-ms".to_owned(),
        "60000".to_owned(),
    ];
    supervisor_args.extend(observation.subject_args());
    let child_env = h.whitelist_env();
    let inherit = observation.subject_inherit();
    let request = win::SpawnRequest {
        program: &h.supervisor.to_string_lossy(),
        args: &supervisor_args,
        env: &child_env,
        cwd: Some(&dir),
        inherit: &inherit,
        suspended: false,
    };
    let supervisor = match win::spawn(&request) {
        Ok(child) => child,
        Err(error) => {
            checks.verify("supervisor.spawned", false, format!("{error:#}"));
            return checks;
        }
    };
    h.supervised = Some(supervisor);

    // The observer withholds the final ACK after checking the frozen tree
    // and closing all job copies. Normal teardown cannot race this crash.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready = false;
    while Instant::now() < deadline {
        if let Ok(boundary) = std::fs::read(h.path(name, "crash-boundary.txt")) {
            ready = boundary == b"ready";
            break;
        }
        if !h.supervised.as_ref().map(|child| win::still_active(child.process)).unwrap_or(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    checks.verify(
        "supervisedTree.up",
        ready,
        "the independent observer verified the frozen live tree and closed all job copies before the crash",
    );
    checks.verify(
        "supervisedTree.stepInSomeJob",
        fixture_bool(&h.path(name, "live.facts.json"), "inAnyJob") == Some(true),
        "the supervised step, a process independent of the supervisor, reports that it sits in a job",
    );

    checks.verify(
        "outsider.aliveBeforeCrash",
        h.outsider_alive(),
        "the controlled outsider is running before the crash",
    );

    // The crash itself: TerminateProcess on the handle this round created.
    let supervisor = h.supervised.as_mut().expect("creation handle retained in harness");
    checks.verify("supervisor.aliveAtCrash", win::still_active(supervisor.process),
        "the subject is still alive and awaiting the withheld observation ACK");
    let crash_code = 1_u32;
    let terminated = unsafe {
        windows_sys::Win32::System::Threading::TerminateProcess(supervisor.process, crash_code)
    } != 0;
    checks.verify(
        "supervisor.terminated",
        terminated,
        "the supervisor was terminated through the runner's own creation handle",
    );
    let exit = supervisor.wait(20_000).ok().flatten();
    let io_reaped = supervisor.join_io();
    checks.verify("supervisor.ioReaped", io_reaped, "supervisor stdio threads exited within their bound");
    checks.verify(
        "supervisor.exited",
        exit.is_some(),
        format!("the supervisor exited within the bounded wait (code {:?})", exit),
    );

    let (status_present, events) = Harness::read_status(&status_path);
    checks.verify(
        "status.noFinalEvent",
        final_event(&events).is_none(),
        "the crash path cannot write a final event, so its absence is the expected shape",
    );
    checks.verify(
        "status.wellFormed",
        status_present && !events.is_empty(),
        "every complete line the supervisor did write is well-formed JSON",
    );
    let plan_sha_after = std::fs::read(&plan_path)
        .map(|bytes| sha256_hex(&bytes))
        .unwrap_or_else(|_| "unreadable".to_owned());
    checks.verify(
        "plan.frozen",
        plan_sha_before == plan_sha_after,
        "the frozen plan is byte-identical before and after the crash",
    );

    checks.verify(
        "outsider.aliveAfterCrash",
        h.outsider_alive(),
        "the controlled outsider survived the crash untouched",
    );

    let mut observed = h.finish_observation(observation);
    checks.items.append(&mut observed.items);
    h.supervised = None;
    checks
}

#[cfg(windows)]
fn scenario_outsider_final(h: &mut Harness) -> Checks {
    let mut checks = Checks::new();
    let facts = h.finish_outsider();
    checks.verify(
        "outsider.aliveBeforeTeardown",
        facts["aliveBeforeTeardown"].as_bool() == Some(true),
        "the controlled outsider outlived every scenario",
    );
    checks.verify(
        "outsider.terminatedByRunnerJob",
        facts["terminatedByRunnerJob"].as_bool() == Some(true),
        "the outsider was ended by the runner's own exclusive job, by handle",
    );
    checks.verify(
        "outsider.exitedWithinDeadline",
        facts["exitedWithinDeadline"].as_bool() == Some(true),
        "the outsider exited within the bounded cleanup wait",
    );
    checks
}

// ------------------------------------------------------------------- main

#[cfg(windows)]
fn runner_main(arguments: &[String]) -> Result<i32> {
    let mut supervisor: Option<PathBuf> = None;
    let mut fixture: Option<PathBuf> = None;
    let mut workdir: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut examples_dir: Option<PathBuf> = None;
    let mut selected: Vec<String> = Vec::new();
    let mut list = false;

    let mut index = 0_usize;
    while index < arguments.len() {
        let flag = arguments[index].clone();
        // `--list` takes no value, so it must not swallow the next argument.
        if flag == "--list" {
            list = true;
            index += 1;
            continue;
        }
        // Every other flag consumes the argument that follows it, if any.
        let mut value: Option<String> = None;
        if index + 1 < arguments.len() {
            index += 1;
            value = Some(arguments[index].clone());
        }
        match flag.as_str() {
            "--supervisor" => supervisor = value.map(PathBuf::from),
            "--fixture" => fixture = value.map(PathBuf::from),
            "--workdir" => workdir = value.map(PathBuf::from),
            "--out" => out = value.map(PathBuf::from),
            "--examples-dir" => examples_dir = value.map(PathBuf::from),
            "--scenario" => {
                if let Some(name) = value {
                    selected.push(name);
                }
            }
            "--list" => list = true,
            other => bail!("unknown argument {other:?}"),
        }
        index += 1;
    }

    if list {
        for name in SCENARIOS {
            println!("{name}");
        }
        return Ok(0);
    }

    let supervisor = supervisor.ok_or_else(|| anyhow::anyhow!("--supervisor is required"))?;
    let fixture = fixture.ok_or_else(|| anyhow::anyhow!("--fixture is required"))?;
    let workdir = workdir.ok_or_else(|| anyhow::anyhow!("--workdir is required"))?;
    for name in &selected {
        if !SCENARIOS.contains(&name.as_str()) {
            bail!("unknown scenario {name:?}");
        }
    }
    if !supervisor.is_file() {
        bail!("the supervisor binary {} does not exist", supervisor.display());
    }
    if !fixture.is_file() {
        bail!("the fixture binary {} does not exist", fixture.display());
    }

    // One independent directory for the whole round.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let dir = workdir.join(format!(
        "ownership-{stamp}-{}",
        win::current_process_id()
    ));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("could not create the run directory {}", dir.display()))?;

    let mut harness = Harness::new(dir.clone(), supervisor.clone(), fixture.clone())?;
    harness.spawn_outsider()?;

    let self_exe = std::env::current_exe()
        .context("could not resolve the black box executable")?;
    let mut binaries = vec![
        json!({"role": "supervisor", "path": supervisor.display().to_string(), "sha256": sha256_file(&supervisor)}),
        json!({"role": "fixture", "path": fixture.display().to_string(), "sha256": sha256_file(&fixture)}),
        json!({"role": "runner", "path": self_exe.display().to_string(), "sha256": sha256_file(&self_exe)}),
    ];
    let mut sources = Vec::new();
    if let Some(examples) = examples_dir.as_ref() {
        for name in [
            "acceptance_supervisor.rs",
            "acceptance_ownership_fixture.rs",
            "acceptance_ownership_blackbox.rs",
        ] {
            let path = examples.join(name);
            sources.push(json!({
                "file": name,
                "path": path.display().to_string(),
                "sha256": sha256_file(&path),
            }));
        }
    }

    // Everything from here owns processes and handles. A panic unwinding out
    // of the round must not skip the report or the round's own teardown: the
    // round is caught, the outsider is ended by the runner's own job, and a
    // **failure** report is written that claims nothing clean. The panic is
    // reported as what it is, never converted into a pass.
    let round = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i32> {
        for name in SCENARIOS {
            if !selected.is_empty() && !selected.iter().any(|selected| selected == name) {
                continue;
            }
            let scenario = run_scenario(&mut harness, name);
            harness.scenarios.push(scenario);
        }
        let outsider_facts = harness.finish_outsider();

        record_supervisor_tree_blocks(&mut harness);

        // The supervisor binary must not have changed under us mid-round.
        let supervisor_sha_after = sha256_file(&supervisor);
        if let Some(first) = binaries.first_mut() {
            if let Some(object) = first.as_object_mut() {
                object.insert("sha256AtRoundEnd".to_owned(), json!(supervisor_sha_after));
            }
        }

        let failed = harness
            .scenarios
            .iter()
            .filter(|scenario| scenario["outcome"] == "fail")
            .count();
        let blocked = harness
            .scenarios
            .iter()
            .filter(|scenario| scenario["outcome"] == "blocked")
            .count();
        let scenario_count = harness.scenarios.len();
        let unverified_cleanup = harness.unverified_cleanup.clone();
        let blocked_items = harness.blocked.clone();

        let report = json!({
            "artifact": "acceptance-ownership-blackbox",
            "scope": "synthetic-windows-process-ownership",
            "fullPlanVerified": false,
            "whyNotFull": "synthetic process ownership only; assignment failure is not injected, and daemon, provider, phone and public-network acceptance require separate evidence",
            "generatedAt": utc_stamp(),
            "runDir": dir.display().to_string(),
            "hold": "not lifted: the provider chain runner's incident gate stays in place until main reviews and independently runs this black box",
            "runnerPid": win::current_process_id(),
            "runnerInAnyJob": harness.runner_in_job,
            "environmentWhitelistNames": harness.whitelist_names(),
            "binaries": binaries,
            "sources": sources,
            "scenarios": harness.scenarios,
            "outsider": outsider_facts,
            "cleanup": {
                "unverified": unverified_cleanup,
                "legalExits": [
                    "TerminateProcess on a handle created this round",
                    "TerminateJobObject on a job created and owned this round",
                ],
                "forbidden": [
                    "OpenProcess by pid",
                    "taskkill",
                    "kill by name, port, command line or directory",
                    "machine-wide scans",
                    "adopting orphans",
                ],
            },
            "blocked": blocked_items,
            "summary": {
                "scenarios": scenario_count,
                "failed": failed,
                "blocked": blocked,
            },
        });

        let fallback_report = dir.join("report.json");
        let report_path = out.as_deref().unwrap_or(&fallback_report);
        if let Some(parent) = report_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(report_path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("could not write {}", report_path.display()))?;

        println!("run directory: {}", dir.display());
        println!("report:        {}", report_path.display());
        println!(
            "scenarios: {} (failed {}, blocked {})",
            scenario_count, failed, blocked
        );
        for item in &unverified_cleanup {
            eprintln!("cleanup not verified: {item}");
        }
        Ok(if failed == 0
            && blocked == 0
            && scenario_count > 0
            && unverified_cleanup.is_empty()
        {
            0
        } else {
            1
        })
    }));

    match round {
        Ok(inner) => inner,
        Err(payload) => {
            // Bounded, sanitized diagnostics: the class and a digit-masked
            // excerpt travel, never a verbatim payload whose contents this
            // synthetic runner does not control.
            let class = panic_class(payload.as_ref());
            let excerpt = panic_excerpt(payload.as_ref());
            eprintln!("black box: the round panicked (class: {class})");
            // The trees this runner still owns are ended by the handles it
            // still holds — the supervised supervisor and the observer of
            // the panicking run live in the harness, outside the call frame
            // that unwound — each through its creation handle with a
            // bounded wait and a contained reaping. A supervised tree whose
            // handle was already released may outlive the runner only until
            // its own total deadline; nothing is claimed about it, and it
            // is never adopted.
            harness.terminate_run_in_progress();
            let outsider_facts = harness.finish_outsider();
            let report = json!({
                "artifact": "acceptance-ownership-blackbox",
                "scope": "synthetic-windows-process-ownership",
                "fullPlanVerified": false,
                "whyNotFull": "the round panicked before it could finish; nothing was verified and no claim below reads as a pass",
                "outcome": "panic",
                "panicClass": class,
                "panicExcerpt": excerpt,
                "generatedAt": utc_stamp(),
                "runDir": dir.display().to_string(),
                "hold": "not lifted: the provider chain runner's incident gate stays in place",
                "runnerPid": win::current_process_id(),
                "outsider": outsider_facts,
                "cleanup": {
                    "unverified": [
                        "the round panicked; handles owned inside the round were released while unwinding or end by kill-on-close, and no exit was verified"
                    ],
                    "legalExits": [
                        "TerminateProcess on a handle created this round",
                        "TerminateJobObject on a job created and owned this round",
                    ],
                    "forbidden": [
                        "OpenProcess by pid",
                        "taskkill",
                        "kill by name, port, command line or directory",
                        "machine-wide scans",
                        "adopting orphans",
                    ],
                },
                "summary": {
                    "scenarios": 0,
                    "failed": 0,
                    "blocked": 0,
                },
            });
            let fallback_report = dir.join("report.json");
            let report_path = out.as_deref().unwrap_or(&fallback_report);
            if let Some(parent) = report_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let _ = std::fs::write(
                report_path,
                serde_json::to_string_pretty(&report).unwrap_or_default(),
            );
            println!("run directory: {}", dir.display());
            println!("report:        {} (panic)", report_path.display());
            eprintln!("cleanup is UNVERIFIED: the round panicked, and nothing here claims the owned trees clean");
            Ok(1)
        }
    }
}

/// The panic's class: a small, closed set of names for the panic kinds this
/// runner knows, and a generic name for everything else. No payload text
/// travels verbatim — its contents are not under this runner's control.
fn panic_class(payload: &(dyn std::any::Any + Send)) -> &'static str {
    let text = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    if text.contains("failed to join on thread") {
        "std-thread-join-failed"
    } else {
        "panic"
    }
}

/// A bounded, digit-masked excerpt of the panic payload, for the report.
/// Digits are masked so thread ids and handle values in a payload never
/// survive as claims; the excerpt is capped so an unforeseen payload cannot
/// grow the artifact.
fn panic_excerpt(payload: &(dyn std::any::Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".to_owned());
    let masked: String = text
        .chars()
        .map(|character| if character.is_ascii_digit() { '#' } else { character })
        .collect();
    masked.chars().take(200).collect()
}

#[cfg(windows)]
fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let code = if arguments.iter().any(|argument| argument == "--role") {
        match observer::main() {
            Ok(code) => code,
            Err(error) => {
                eprintln!("observer: {error:#}");
                2
            }
        }
    } else {
        match runner_main(&arguments) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("black box: {error:#}");
                2
            }
        }
    };
    std::process::exit(code);
}

//! One server-sent event connection per OpenCode server port.
//!
//! An OpenCode server broadcasts every session's events on `GET /api/event`,
//! so when several sessions share one server, per-driver connections each
//! receive the whole site-wide stream. The hub keeps a single SSE connection
//! per port and fans the parsed JSON events out to every subscriber;
//! SessionFamily filtering stays with the drivers.
//!
//! The reader owns the whole transport on that one connection: it validates
//! the HTTP handshake, decodes a chunked body, reassembles SSE frames the
//! network split anywhere, and reconnects with a capped backoff when the
//! stream breaks. A broken stream is a transport fault, not the end of the
//! server, so a subscription survives it; the connection generation is what
//! tells a consumer that a reconnect it never saw happened, so durable turn
//! state can be reconciled against the server instead of guessed.
//!
//! Reader threads hold only the server's port, never an `OpenCodeServer` or
//! `PooledServer` handle: a reader holding a handle would prevent the global
//! server's reclamation by `shutdown_all`, deadlocking the reader against
//! the process it is waiting to see exit.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use parking_lot::{Condvar, Mutex};
use serde_json::Value;

use crate::opencode_session::basic_authorization;

/// Every read on the stream polls this interval — during setup and for the
/// stream's whole life — so a teardown that shut the socket down is noticed
/// even when the network stack never wakes a blocked recv. Bytes that did
/// arrive stay buffered across an expiry, so continuing never loses a read.
const READ_POLL: Duration = Duration::from_millis(200);

/// How long establishing the TCP connection may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long writing the handshake request may take.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the response head may take to complete. A head that never
/// arrives is a hung endpoint, not a slow one.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a connection may deliver no bytes at all before it counts as
/// dead. Liveness is judged on raw bytes — heartbeat comments included — so
/// a session that is merely quiet, a long model turn with nothing to say,
/// never trips this, while a socket the server forgot about does.
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);

/// Reconnect backoff. It doubles from this floor and never passes this
/// ceiling, so a server that keeps dropping the stream is retried steadily
/// instead of hammered.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(250);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Caps that keep a broken or hostile stream from growing the reader's
/// buffers without bound: a head, a single SSE line, or an assembled event
/// past its budget ends the connection, and a chunk-size line past its
/// budget is a broken frame rather than a large one.
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_CHUNK_LINE_BYTES: usize = 1024;

/// How much one read pulls off the socket. It also bounds how much decoded
/// body can wait for the SSE layer, because the layer always drains first.
const READ_SIZE: usize = 16 * 1024;

/// One queued subscriber: a unique identity for cancellation plus the channel
/// the hub broadcasts parsed events through.
type Subscriber = (u64, Sender<Value>);

enum HubState {
    /// No reader is live; the next subscribing thread becomes the starter.
    Idle,
    /// A reader is connecting. Subscribers may already queue for it.
    Starting(Vec<Subscriber>),
    /// The reader is streaming; every subscriber receives every event.
    Running(Vec<Subscriber>),
    /// The last subscriber left. The reader is finishing its teardown and new
    /// subscribers wait for it to be gone, so two SSE connections never
    /// coexist on one port.
    Stopping,
}

struct Hub {
    port: u16,
    state: Mutex<HubState>,
    changed: Condvar,
    /// A clone of the reader's socket, so a teardown can shut the stream down
    /// even though the reader itself blocks inside a read.
    socket: Mutex<Option<TcpStream>>,
    /// Successful handshakes on this port: 0 before the first, one more for
    /// every connection after it — reconnects included. A consumer that
    /// watches it sees stream restarts its own receives could not reveal.
    generation: AtomicU64,
}

impl Hub {
    fn new(port: u16) -> Arc<Hub> {
        Arc::new(Hub {
            port,
            state: Mutex::new(HubState::Idle),
            changed: Condvar::new(),
            socket: Mutex::new(None),
            generation: AtomicU64::new(0),
        })
    }
}

fn hubs() -> &'static Mutex<HashMap<u16, Arc<Hub>>> {
    static HUBS: OnceLock<Mutex<HashMap<u16, Arc<Hub>>>> = OnceLock::new();
    HUBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_subscriber_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Subscribes to the server-wide event stream on `port`.
///
/// The first subscriber for a port opens the single SSE connection; later
/// ones join it. The connection attempt happens on the hub's reader thread,
/// so this returns before the stream is live and `recv` blocks until the
/// first event or the stream's end. Callers must already be off the UI
/// thread.
pub(crate) fn subscribe(port: u16) -> anyhow::Result<EventFeed> {
    let hub = hubs()
        .lock()
        .entry(port)
        .or_insert_with(|| Hub::new(port))
        .clone();
    let (tx, rx) = unbounded();
    let id = next_subscriber_id();
    let mut state = hub.state.lock();
    loop {
        match &mut *state {
            HubState::Idle => {
                *state = HubState::Starting(vec![(id, tx)]);
                drop(state);
                let reader = Arc::clone(&hub);
                if let Err(error) = thread::Builder::new()
                    .name("fintwind-opencode-event-hub".into())
                    .spawn(move || run_reader(reader))
                {
                    // No reader will ever run for this generation; release
                    // any subscriber that queued behind the start.
                    *hub.state.lock() = HubState::Idle;
                    hub.changed.notify_all();
                    return Err(anyhow::Error::new(error).context(format!(
                        "could not spawn the event hub reader for port {port}"
                    )));
                }
                return Ok(EventFeed {
                    hub,
                    rx,
                    id,
                    cancelled: AtomicBool::new(false),
                });
            }
            HubState::Starting(subscribers) | HubState::Running(subscribers) => {
                subscribers.push((id, tx));
                return Ok(EventFeed {
                    hub: Arc::clone(&hub),
                    rx,
                    id,
                    cancelled: AtomicBool::new(false),
                });
            }
            // The previous reader is still being torn down. Waiting here is
            // what keeps a second SSE connection from opening on this port.
            HubState::Stopping => hub.changed.wait(&mut state),
        }
    }
}

/// One subscription to a port's shared event stream.
///
/// Dropping the last feed for a port tears the SSE connection down.
pub(crate) struct EventFeed {
    hub: Arc<Hub>,
    rx: Receiver<Value>,
    id: u64,
    cancelled: AtomicBool,
}

impl EventFeed {
    /// Receives the next parsed JSON event.
    ///
    /// Blocks across transport failures — the hub reconnects in the
    /// background while any subscriber remains — so an error means the
    /// subscription itself is over: the last feed was cancelled or dropped,
    /// or the hub's reader stopped for good.
    ///
    /// Callers that need to watch anything else while waiting use
    /// `recv_timeout`; this blocking form stays part of the module's
    /// surface for callers that own their own wait, and the tests pin it.
    #[allow(dead_code)]
    pub(crate) fn recv(&self) -> anyhow::Result<Value> {
        self.rx
            .recv()
            .map_err(|_| anyhow!("opencode event stream on port {} has ended", self.hub.port))
    }

    /// Receives the next event, giving up after `timeout`.
    ///
    /// `Ok(None)` means nothing arrived in time. A quiet stream is not a
    /// failing one — a long turn with no output must not look like a dead
    /// subscription — so the caller simply polls again. `Err` means the
    /// subscription is over, exactly like `recv`'s error.
    pub(crate) fn recv_timeout(&self, timeout: Duration) -> anyhow::Result<Option<Value>> {
        match self.rx.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(anyhow!(
                "opencode event stream on port {} has ended",
                self.hub.port
            )),
        }
    }

    /// The hub's connection generation: 0 until the first handshake
    /// succeeds, then one more for every successful handshake after it,
    /// reconnects included. Comparing it across a quiet window is how a
    /// consumer notices the stream silently restarted and durable turn
    /// state needs reconciling.
    pub(crate) fn connection_generation(&self) -> u64 {
        self.hub.generation.load(Ordering::Acquire)
    }

    /// Whether parsed events are still waiting in this subscription's queue.
    ///
    /// The shared connection carries every session's events, so waiting for
    /// the whole connection to fall quiet would starve one session's
    /// reconciliation while another streams. Draining only this feed's own
    /// backlog is what makes an asynchronous result safe to apply; the check
    /// reads the queue and never touches the socket.
    pub(crate) fn has_pending_events(&self) -> bool {
        !self.rx.is_empty()
    }

    /// Unsubscribes. The last cancellation for a port shuts the SSE socket
    /// down and returns promptly; the reader thread finishes on its own —
    /// out of a reconnect backoff, which the wake below cuts short, and out
    /// of a panicking frame, which the reader's guard covers.
    pub(crate) fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let stop = {
            let mut state = self.hub.state.lock();
            match &mut *state {
                HubState::Starting(subscribers) | HubState::Running(subscribers) => {
                    subscribers.retain(|(id, _)| *id != self.id);
                    if subscribers.is_empty() {
                        *state = HubState::Stopping;
                        true
                    } else {
                        false
                    }
                }
                HubState::Idle | HubState::Stopping => false,
            }
        };
        if stop {
            // Wake the reader wherever it is — sleeping out a backoff or
            // polling a read — so it re-checks the state and stops.
            self.hub.changed.notify_all();
            if let Some(socket) = self.hub.socket.lock().take() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Drop for EventFeed {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Releases the hub's reader claim exactly once — including when the reader
/// unwinds.
///
/// Finishing the hub only on the happy path left a panic able to strand it
/// in `Running` with no reader behind it, after which every later subscriber
/// would wait forever on a stream that could never deliver. The guard makes
/// that state unreachable.
struct HubGuard<'a> {
    hub: &'a Hub,
}

impl<'a> HubGuard<'a> {
    fn new(hub: &'a Hub) -> Self {
        HubGuard { hub }
    }
}

impl Drop for HubGuard<'_> {
    fn drop(&mut self) {
        self.hub.reader_finished();
    }
}

/// Timeouts one connection attempt works against.
///
/// Production uses fixed budgets; tests shrink the idle one so a decision
/// that takes 45 seconds in the field can be observed in milliseconds.
#[derive(Clone, Copy)]
struct Budgets {
    handshake: Duration,
    idle: Duration,
}

impl Budgets {
    fn production() -> Self {
        Budgets {
            handshake: HANDSHAKE_TIMEOUT,
            idle: IDLE_TIMEOUT,
        }
    }
}

/// Why one connection attempt or live stream ended.
///
/// Every variant is a category or a status code — never anything the peer
/// sent — so the reader can record it safely. Only the fixed reason code
/// reaches the diagnostics log, never an error rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamFailure {
    /// The TCP connection could not be established.
    Connect,
    /// The handshake request could not be written.
    Write,
    /// The socket could not be registered or its timeouts set.
    Register(ErrorKind),
    /// The response head never completed, or was not an HTTP head at all.
    Handshake,
    /// The server answered with a status other than 200.
    Status(u16),
    /// The response was not `text/event-stream`.
    ContentType,
    /// The chunked body was malformed.
    Chunked,
    /// A head, line, or event grew past its budget.
    TooLarge,
    /// A read failed.
    Read(ErrorKind),
    /// The server closed the connection or finished the body.
    Eof,
    /// No bytes arrived within the idle budget.
    Idle,
    /// The hub tore the connection down while it was being set up.
    Cancelled,
}

impl StreamFailure {
    /// The fixed reason code recorded for this failure.
    fn reason(&self) -> &'static str {
        match self {
            StreamFailure::Connect => "connect_failed",
            StreamFailure::Write => "request_write_failed",
            StreamFailure::Register(_) => "socket_setup_failed",
            StreamFailure::Handshake => "handshake_incomplete",
            StreamFailure::Status(_) => "handshake_rejected_status",
            StreamFailure::ContentType => "handshake_rejected_content_type",
            StreamFailure::Chunked => "chunked_frame_rejected",
            StreamFailure::TooLarge => "buffer_limit_reached",
            StreamFailure::Read(_) => "read_error",
            StreamFailure::Eof => "server_closed_stream",
            StreamFailure::Idle => "heartbeat_expired",
            StreamFailure::Cancelled => "cancelled",
        }
    }
}

/// Why driving one connection ended.
enum DriveOutcome {
    /// The hub is tearing down; the reader must stop for good.
    Stopped,
    /// The connection ended on its own; reconnect unless torn down.
    Broke(StreamFailure),
}

impl Hub {
    /// Registers the reader's socket so a teardown can wake it. Fails when
    /// the hub started tearing down while the connection was being set up.
    fn attach(&self, stream: &TcpStream) -> Result<bool, StreamFailure> {
        let socket = stream
            .try_clone()
            .map_err(|error| StreamFailure::Register(error.kind()))?;
        // Lock order is state then socket everywhere; `reader_finished`
        // takes the same order, so holding `state` across the registration
        // cannot deadlock against a finishing reader.
        let state = self.state.lock();
        if matches!(*state, HubState::Stopping | HubState::Idle) {
            let _ = socket.shutdown(Shutdown::Both);
            return Ok(false);
        }
        *self.socket.lock() = Some(socket);
        Ok(true)
    }

    fn is_tearing_down(&self) -> bool {
        matches!(*self.state.lock(), HubState::Stopping | HubState::Idle)
    }

    /// Moves a successfully connected reader from `Starting` to `Running`,
    /// keeping whatever subscribers queued while it was connecting. A
    /// reconnect finds the hub already `Running` and needs no transition.
    fn promote(&self) {
        let mut state = self.state.lock();
        if let HubState::Starting(subscribers) = &mut *state {
            *state = HubState::Running(std::mem::take(subscribers));
        }
    }

    /// Sends one parsed event to every subscriber, dropping the ones whose
    /// feed is already gone.
    fn broadcast(&self, event: Value) {
        let mut state = self.state.lock();
        if let HubState::Running(subscribers) = &mut *state {
            subscribers.retain(|(_, sender)| sender.send(event.clone()).is_ok());
        }
    }

    /// Marks the reader as gone: queued and future subscribers learn the
    /// stream ended through their channel closing, and the next subscribe
    /// may start a fresh reader.
    ///
    /// The reader's guard calls this, so it also runs when a panic unwinds
    /// the reader thread instead of exiting it.
    fn reader_finished(&self) {
        // State before socket, like every other path. Clearing the socket
        // while still holding `state` also keeps a next-generation reader —
        // started once the notify below releases waiters — from registering
        // its own socket only to have this finishing one wipe it.
        let mut state = self.state.lock();
        *state = HubState::Idle;
        *self.socket.lock() = None;
        drop(state);
        self.changed.notify_all();
    }

    /// Waits out a reconnect backoff, returning `false` as soon as the hub
    /// starts tearing down, so a late cancellation never costs a full sleep.
    fn sleep_unless_teardown(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        let mut state = self.state.lock();
        loop {
            if matches!(*state, HubState::Stopping | HubState::Idle) {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            self.changed.wait_for(&mut state, deadline - now);
        }
    }
}

fn run_reader(hub: Arc<Hub>) {
    run_reader_with_budgets(hub, Budgets::production());
}

/// Drives one port's shared stream for as long as it has subscribers.
///
/// Reconnects with a capped backoff after any transport failure, so a
/// dropped stream never ends a subscription; the loop only exits when the
/// hub tears down.
fn run_reader_with_budgets(hub: Arc<Hub>, budgets: Budgets) {
    // The guard — not the happy path — is what ends this claim on the hub,
    // so a panicking reader still leaves the port subscribable.
    let _released = HubGuard::new(&hub);
    let mut backoff = RECONNECT_BACKOFF_MIN;
    loop {
        // No teardown pre-check here: a cancellation that lands before the
        // reader starts still opens and immediately closes its connection,
        // which is what leaves a peer's pending accept hanging up instead of
        // waiting forever. Every in-flight state re-checks teardown anyway
        // — `attach` refuses, the head read gives up, the drive loop stops,
        // and the backoff sleep is woken.
        match open_stream(&hub, &budgets) {
            Ok(mut connection) => {
                backoff = RECONNECT_BACKOFF_MIN;
                // Count the handshake before the connection's first
                // broadcast, so a consumer that reads the generation around
                // a receive always sees that the stream silently restarted.
                let generation = hub.generation.fetch_add(1, Ordering::AcqRel) + 1;
                crate::opencode_diagnostics::record("connected", hub.port, generation);
                hub.promote();
                match drive(&hub, &mut connection, &budgets) {
                    DriveOutcome::Stopped => break,
                    DriveOutcome::Broke(failure) => {
                        crate::opencode_diagnostics::record(failure.reason(), hub.port, generation);
                        if hub.is_tearing_down() {
                            break;
                        }
                    }
                }
            }
            Err(failure) => {
                // A cancellation is teardown doing its job, not a fault.
                if !matches!(failure, StreamFailure::Cancelled) {
                    crate::opencode_diagnostics::record(
                        failure.reason(),
                        hub.port,
                        hub.generation.load(Ordering::Acquire),
                    );
                }
                if hub.is_tearing_down() {
                    break;
                }
            }
        }
        if !hub.sleep_unless_teardown(backoff) {
            break;
        }
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
    }
}

/// Keeps one connection flowing until it ends or the hub tears down.
fn drive(hub: &Hub, connection: &mut Connection, budgets: &Budgets) -> DriveOutcome {
    loop {
        // Bytes already in hand — the response head's leftovers included —
        // are decoded before anything waits for the socket to say more.
        if let Err(failure) = connection.drain_buffered() {
            return end_connection(hub, connection, failure);
        }
        match connection.next_event() {
            Ok(Some(event)) => {
                hub.broadcast(event);
                // One TCP read can hold hundreds of complete frames. Drain
                // them before waiting on the socket again, otherwise every
                // already-buffered frame would pay another 200ms read poll.
                continue;
            }
            Ok(None) => {}
            Err(failure) => return end_connection(hub, connection, failure),
        }
        // A payload that is not JSON is skipped rather than fatal, and is
        // reported once per connection without the payload itself.
        if !connection.reported_invalid && connection.sse.take_invalid() > 0 {
            connection.reported_invalid = true;
            crate::opencode_diagnostics::record(
                "event_not_json",
                hub.port,
                hub.generation.load(Ordering::Acquire),
            );
        }
        match connection.pump() {
            Ok(true) => {}
            Ok(false) => {
                // The poll expired without new bytes. Liveness counts raw
                // bytes — heartbeat comments included — so a quiet session
                // keeps its connection while a silent socket loses it.
                if hub.is_tearing_down() {
                    return DriveOutcome::Stopped;
                }
                if connection.last_byte.elapsed() >= budgets.idle {
                    return end_connection(hub, connection, StreamFailure::Idle);
                }
            }
            Err(failure) => {
                // A teardown that shut the socket down looks like a read
                // error; ending the reader is the point, not a reconnect.
                if hub.is_tearing_down() {
                    return DriveOutcome::Stopped;
                }
                return end_connection(hub, connection, failure);
            }
        }
    }
}

/// Ends one connection, reconnecting is the reader's next step.
///
/// Everything this connection already assembled is broadcast first: a body
/// that finished, a socket that died, or a frame that outgrew its budget
/// must not swallow the events it carried before the end. A teardown needs
/// no flush — its subscribers are already gone.
fn end_connection(hub: &Hub, connection: &mut Connection, failure: StreamFailure) -> DriveOutcome {
    while let Ok(Some(event)) = connection.next_event() {
        hub.broadcast(event);
    }
    DriveOutcome::Broke(failure)
}

/// How the response body is delimited.
enum BodyFraming {
    /// `Transfer-Encoding: chunked`: size lines and their CRLFs are framing
    /// and must never reach the SSE decoder as data.
    Chunked,
    /// Close-delimited: every byte after the head is body.
    UntilClose,
}

/// Where the chunked decoder stands between reads.
enum ChunkState {
    /// Expecting a `"<hex>[;ext]"` size line.
    Size,
    /// Expecting this many bytes of chunk data.
    Data(u64),
    /// Expecting the CRLF that ends a chunk's data.
    DataEnd,
    /// Past the zero chunk: trailer lines until a blank one.
    Trailer,
    /// The body is complete.
    Done,
}

/// One live SSE connection: the socket plus the half-decoded bytes of its
/// response body.
struct Connection {
    stream: TcpStream,
    /// Raw socket bytes not yet decoded. They survive poll expiries and
    /// chunk boundaries, so a byte that arrives late is never lost.
    raw: Vec<u8>,
    /// How much of `raw` is already decoded.
    consumed: usize,
    framing: BodyFraming,
    chunks: ChunkState,
    /// Decoded body bytes waiting for the SSE layer. The layer always
    /// drains them before the next read, so this stays small.
    decoded: Vec<u8>,
    /// When a byte last arrived. Liveness is judged on raw bytes, not on
    /// business events, so a quiet session is never a dead connection.
    last_byte: Instant,
    sse: SseDecoder,
    /// Whether this connection already reported a payload that was not
    /// JSON, so a broken stream cannot flood the diagnostics log.
    reported_invalid: bool,
}

impl Connection {
    /// Takes the next fully assembled event, if one is ready.
    fn next_event(&mut self) -> Result<Option<Value>, StreamFailure> {
        if let Some(event) = self.sse.ready.pop_front() {
            return Ok(Some(event));
        }
        if !self.decoded.is_empty() {
            let pending = std::mem::take(&mut self.decoded);
            self.sse.push(&pending)?;
        }
        Ok(self.sse.ready.pop_front())
    }

    /// Reads the next group of socket bytes and decodes as much body as the
    /// framing allows. `Ok(false)` means the poll expired with nothing new;
    /// every byte that did arrive stays buffered for the next try.
    fn pump(&mut self) -> Result<bool, StreamFailure> {
        if matches!(self.chunks, ChunkState::Done) {
            // A chunked body that ended is a stream the server finished.
            return Err(StreamFailure::Eof);
        }
        let mut scratch = [0u8; READ_SIZE];
        match self.stream.read(&mut scratch) {
            // A close-delimited body ends with the socket; a chunked one
            // just stops mid-body. Both mean this connection is over.
            Ok(0) => return Err(StreamFailure::Eof),
            Ok(bytes) => {
                self.last_byte = Instant::now();
                self.raw.extend_from_slice(&scratch[..bytes]);
            }
            Err(error) if is_poll_expiry(&error) => return Ok(false),
            Err(error) => return Err(StreamFailure::Read(error.kind())),
        }
        self.drain_buffered()?;
        Ok(true)
    }

    /// Moves raw bytes into the decoded body, stripping chunk framing.
    fn decode_body(&mut self) -> Result<(), StreamFailure> {
        if let BodyFraming::UntilClose = self.framing {
            self.decoded.extend_from_slice(&self.raw[self.consumed..]);
            self.consumed = self.raw.len();
            return Ok(());
        }
        loop {
            match self.chunks {
                ChunkState::Done => return Ok(()),
                ChunkState::Size => {
                    let Some(newline) = find_newline(&self.raw[self.consumed..]) else {
                        // A size line past the budget is a broken stream,
                        // not a large chunk.
                        if self.raw.len() - self.consumed > MAX_CHUNK_LINE_BYTES {
                            return Err(StreamFailure::Chunked);
                        }
                        return Ok(());
                    };
                    let line = &self.raw[self.consumed..self.consumed + newline];
                    let size = parse_chunk_size(line).ok_or(StreamFailure::Chunked)?;
                    self.consumed += newline + 1;
                    self.chunks = if size == 0 {
                        ChunkState::Trailer
                    } else {
                        ChunkState::Data(size)
                    };
                }
                ChunkState::Data(remaining) => {
                    let available = self.raw.len() - self.consumed;
                    if available == 0 {
                        return Ok(());
                    }
                    let take = remaining.min(available as u64) as usize;
                    self.decoded
                        .extend_from_slice(&self.raw[self.consumed..self.consumed + take]);
                    self.consumed += take;
                    let remaining = remaining - take as u64;
                    self.chunks = if remaining == 0 {
                        ChunkState::DataEnd
                    } else {
                        ChunkState::Data(remaining)
                    };
                }
                ChunkState::DataEnd => match self.raw.get(self.consumed) {
                    None => return Ok(()),
                    Some(b'\n') => {
                        self.consumed += 1;
                        self.chunks = ChunkState::Size;
                    }
                    Some(b'\r') => match self.raw.get(self.consumed + 1) {
                        None => return Ok(()),
                        Some(b'\n') => {
                            self.consumed += 2;
                            self.chunks = ChunkState::Size;
                        }
                        Some(_) => return Err(StreamFailure::Chunked),
                    },
                    Some(_) => return Err(StreamFailure::Chunked),
                },
                ChunkState::Trailer => {
                    let Some(newline) = find_newline(&self.raw[self.consumed..]) else {
                        if self.raw.len() - self.consumed > MAX_CHUNK_LINE_BYTES {
                            return Err(StreamFailure::Chunked);
                        }
                        return Ok(());
                    };
                    // A blank line ends the trailer — and with CRLF endings
                    // that line is a bare `\r`, not an empty one.
                    let mut line = &self.raw[self.consumed..self.consumed + newline];
                    if line.last() == Some(&b'\r') {
                        line = &line[..line.len() - 1];
                    }
                    self.consumed += newline + 1;
                    if line.is_empty() {
                        self.chunks = ChunkState::Done;
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Drops the decoded prefix of `raw`, which stays bounded because every
    /// state either consumes all available bytes or hits its line budget.
    fn compact(&mut self) {
        if self.consumed > 0 {
            self.raw.drain(..self.consumed);
            self.consumed = 0;
        }
    }

    /// Decodes and compacts every byte already buffered.
    ///
    /// Called before waiting for more socket data, so body bytes that
    /// arrived behind the response head — or any state that stopped short of
    /// needing more — reach the SSE layer instead of waiting for a read that
    /// may never come.
    fn drain_buffered(&mut self) -> Result<(), StreamFailure> {
        self.decode_body()?;
        self.compact();
        Ok(())
    }
}

/// Assembles SSE events from decoded body bytes.
///
/// Only a complete line is interpreted, so a frame cut anywhere — mid-JSON,
/// mid-UTF-8, across a chunk, or across a read timeout — is reassembled
/// before anything looks at it.
#[derive(Default)]
struct SseDecoder {
    /// Bytes of the line being read.
    line: Vec<u8>,
    /// The event being assembled: one `data:` line appends, several join
    /// with newlines, and the blank line after them dispatches.
    data: Vec<u8>,
    /// Whether the current event has any `data:` line at all — an event
    /// with none, or a heartbeat comment, carries no payload.
    has_data: bool,
    /// Events completed by earlier pushes, in order.
    ready: VecDeque<Value>,
    /// Payloads that were not JSON, reported per connection as a count.
    invalid: usize,
}

impl SseDecoder {
    /// Consumes decoded body bytes, completing every event they finish.
    fn push(&mut self, bytes: &[u8]) -> Result<(), StreamFailure> {
        for &byte in bytes {
            if byte != b'\n' {
                self.line.push(byte);
                if self.line.len() > MAX_EVENT_BYTES {
                    return Err(StreamFailure::TooLarge);
                }
                continue;
            }
            let mut line = std::mem::take(&mut self.line);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.handle_line(&line)?;
        }
        Ok(())
    }

    /// Interprets one complete line.
    fn handle_line(&mut self, line: &[u8]) -> Result<(), StreamFailure> {
        if line.is_empty() {
            // The blank line dispatches the assembled event.
            if self.has_data {
                self.has_data = false;
                let payload = std::mem::take(&mut self.data);
                match parse_event_payload(&payload) {
                    Some(event) => self.ready.push_back(event),
                    None => self.invalid += 1,
                }
            }
            return Ok(());
        }
        // A line that starts with a colon is a comment: OpenCode's
        // heartbeats arrive this way, keeping the connection alive without
        // carrying an event.
        if line.first() == Some(&b':') {
            return Ok(());
        }
        if let Some(value) = line.strip_prefix(b"data:") {
            // One optional space after the colon belongs to the framing.
            let value = value.strip_prefix(b" ").unwrap_or(value);
            if !self.data.is_empty() {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(value);
            if self.data.len() > MAX_EVENT_BYTES {
                return Err(StreamFailure::TooLarge);
            }
            self.has_data = true;
        }
        // Every other field (`event:`, `id:`, `retry:`) carries no payload
        // for this consumer; the JSON in `data:` decides everything.
        Ok(())
    }

    /// Takes the count of payloads that were not JSON.
    fn take_invalid(&mut self) -> usize {
        std::mem::take(&mut self.invalid)
    }
}

/// Parses one assembled `data:` payload. Only complete events get here, so
/// a UTF-8 sequence split across reads is already whole; a payload that is
/// not JSON is skipped rather than ending the stream.
fn parse_event_payload(payload: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(payload).ok()?;
    serde_json::from_str(text).ok()
}

/// `HTTP/1.1 200 OK` becomes `200`; anything that is not a status line
/// leaves the handshake rejected.
fn parse_status_line(line: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(line).ok()?.trim_end();
    let (version, rest) = text.split_once(' ')?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let (code, _reason) = rest.split_once(' ').unwrap_or((rest, ""));
    code.parse().ok()
}

/// `"1a;name=value"` becomes `26`; anything else is a broken chunk header.
fn parse_chunk_size(line: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(line).ok()?;
    let digits = text.split(';').next().unwrap_or("").trim();
    if digits.is_empty() {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

/// Index of the next LF, or `None` while the buffer holds no complete line.
fn find_newline(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|&byte| byte == b'\n')
}

/// A read that hit the poll interval rather than the connection: not an
/// error, and not a reason to drop the bytes that did arrive.
fn is_poll_expiry(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

/// The parts of a response head the stream depends on.
#[derive(Default)]
struct ResponseHead {
    status: Option<u16>,
    content_type: Option<String>,
    transfer_encoding: Option<String>,
}

impl ResponseHead {
    fn read_header_line(&mut self, line: &[u8]) {
        let Ok(text) = std::str::from_utf8(line) else {
            return;
        };
        let Some((name, value)) = text.split_once(':') else {
            return;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "content-type" => self.content_type = Some(value.trim().to_owned()),
            "transfer-encoding" => self.transfer_encoding = Some(value.trim().to_owned()),
            _ => {}
        }
    }
}

/// Opens the port's server-sent event stream and leaves it open.
///
/// Validates the handshake — a 200 carrying `text/event-stream` — before
/// anything is decoded, and returns the connection with the body bytes that
/// already arrived behind the head still buffered: an event sharing the
/// head's TCP segment must survive into the stream loop.
fn open_stream(hub: &Hub, budgets: &Budgets) -> Result<Connection, StreamFailure> {
    let port = hub.port;
    let deadline = Instant::now() + budgets.handshake;
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        CONNECT_TIMEOUT,
    )
    .map_err(|_| StreamFailure::Connect)?;
    // Register before anything blocks, so a teardown that lands while the
    // handshake is in flight still finds a socket to shut down.
    if !hub.attach(&stream)? {
        return Err(StreamFailure::Cancelled);
    }
    stream
        .set_read_timeout(Some(READ_POLL))
        .map_err(|error| StreamFailure::Register(error.kind()))?;
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(|error| StreamFailure::Register(error.kind()))?;
    let mut request = format!(
        "GET /api/event HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n"
    );
    if let Some(authorization) = basic_authorization(port) {
        request.push_str(&authorization);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|_| StreamFailure::Write)?;
    stream.flush().map_err(|_| StreamFailure::Write)?;

    // Read the response head. Reads poll a short timeout instead of blocking
    // forever: on Windows a WFP/AV layer intercepting loopback can delay
    // shutdown's wake of a blocked recv indefinitely, and teardown must stay
    // responsive regardless of the network stack.
    let mut raw = Vec::with_capacity(READ_SIZE);
    let mut consumed = 0;
    let mut head = ResponseHead::default();
    let mut started_head = false;
    let mut head_done = false;
    let mut scratch = [0u8; READ_SIZE];
    while !head_done {
        while let Some(newline) = find_newline(&raw[consumed..]) {
            let line_end = consumed + newline;
            let mut line = &raw[consumed..line_end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            consumed = line_end + 1;
            if line.is_empty() {
                // A blank line before the status line is noise, not the end
                // of the head.
                if started_head {
                    head_done = true;
                    break;
                }
                continue;
            }
            if !started_head {
                started_head = true;
                head.status = parse_status_line(line);
            } else {
                head.read_header_line(line);
            }
        }
        if head_done {
            break;
        }
        if raw.len() > MAX_HEAD_BYTES {
            return Err(StreamFailure::TooLarge);
        }
        match stream.read(&mut scratch) {
            // The server hung up before finishing its head.
            Ok(0) => return Err(StreamFailure::Handshake),
            Ok(bytes) => raw.extend_from_slice(&scratch[..bytes]),
            Err(error) if is_poll_expiry(&error) => {
                if hub.is_tearing_down() {
                    return Err(StreamFailure::Cancelled);
                }
                if Instant::now() >= deadline {
                    return Err(StreamFailure::Handshake);
                }
            }
            Err(error) => return Err(StreamFailure::Read(error.kind())),
        }
    }
    let status = head.status.ok_or(StreamFailure::Handshake)?;
    if status != 200 {
        return Err(StreamFailure::Status(status));
    }
    let event_stream = head.content_type.as_deref().is_some_and(|content_type| {
        content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/event-stream")
    });
    if !event_stream {
        return Err(StreamFailure::ContentType);
    }
    let framing = if head
        .transfer_encoding
        .as_deref()
        .is_some_and(|encoding| encoding.to_ascii_lowercase().contains("chunked"))
    {
        BodyFraming::Chunked
    } else {
        BodyFraming::UntilClose
    };
    // Whatever body bytes arrived with the head stay in the buffer: this is
    // the reader that continues into the stream.
    raw.drain(..consumed);
    Ok(Connection {
        stream,
        raw,
        consumed: 0,
        framing,
        chunks: ChunkState::Size,
        decoded: Vec::new(),
        last_byte: Instant::now(),
        sse: SseDecoder::default(),
        reported_invalid: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    const EVENT_LINE: &str = "data: {\"type\":\"x\",\"data\":{}}\n\n";

    struct FakeServer {
        port: u16,
        accepted: Arc<AtomicUsize>,
        closed: Arc<AtomicUsize>,
    }

    /// An SSE endpoint that answers every connection with the same event
    /// lines, then stays open until the peer hangs up.
    fn spawn_fake_sse(broadcasts: usize) -> FakeServer {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let events: Arc<Vec<String>> = Arc::new(vec![EVENT_LINE.to_string(); broadcasts]);
        {
            let accepted = Arc::clone(&accepted);
            let closed = Arc::clone(&closed);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { return };
                    let events = Arc::clone(&events);
                    let accepted = Arc::clone(&accepted);
                    let closed = Arc::clone(&closed);
                    thread::spawn(move || {
                        serve_connection(stream, &events, accepted, closed);
                    });
                }
            });
        }
        FakeServer {
            port,
            accepted,
            closed,
        }
    }

    fn serve_connection(
        stream: TcpStream,
        events: &[String],
        accepted: Arc<AtomicUsize>,
        closed: Arc<AtomicUsize>,
    ) {
        accepted.fetch_add(1, Ordering::SeqCst);
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(_) if line.trim().is_empty() => break,
                Ok(_) => {}
                Err(_) => return,
            }
        }
        // The head and every event leave in ONE write: a first event that
        // shares the response head's TCP segment must still reach the hub
        // reader instead of being dropped with the handshake buffer.
        let mut payload =
            String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
        for event in events {
            payload.push_str(event);
        }
        let _ = writer.write_all(payload.as_bytes());
        let _ = writer.flush();
        // Stay open until the hub hangs up, then report the closure.
        let mut buf = [0u8; 256];
        while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
        closed.fetch_add(1, Ordering::SeqCst);
    }

    /// An SSE endpoint that accepts the connection and never answers the
    /// request, so the hub's response-head read stays blocked until
    /// teardown wakes it.
    fn spawn_silent_listener() -> FakeServer {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        {
            let accepted = Arc::clone(&accepted);
            let closed = Arc::clone(&closed);
            thread::spawn(move || {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                accepted.fetch_add(1, Ordering::SeqCst);
                // Hold the socket open without answering the request head;
                // drain until the hub hangs up.
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                while matches!(reader.read_line(&mut line), Ok(n) if n > 0) {}
                closed.fetch_add(1, Ordering::SeqCst);
            });
        }
        FakeServer {
            port,
            accepted,
            closed,
        }
    }

    fn wait_for(check: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(
                Instant::now() < deadline,
                "condition was not reached in time"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn expected_event() -> Value {
        serde_json::from_str(r#"{"type":"x","data":{}}"#).unwrap()
    }

    #[test]
    fn fans_one_connection_out_to_every_subscriber() {
        let server = spawn_fake_sse(1);
        let first = subscribe(server.port).unwrap();
        let second = subscribe(server.port).unwrap();
        let event = first.recv().unwrap();
        assert_eq!(second.recv().unwrap(), event);
        assert_eq!(event, expected_event());
        // One port, one SSE connection: both feeds were served by the same
        // accepted socket, not by two parallel streams.
        assert_eq!(server.accepted.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn closes_the_connection_and_starts_fresh_after_the_last_drop() {
        let server = spawn_fake_sse(1);
        let first = subscribe(server.port).unwrap();
        let second = subscribe(server.port).unwrap();
        let event = first.recv().unwrap();
        assert_eq!(second.recv().unwrap(), event);

        drop(first);
        drop(second);
        let accepted = server.accepted.load(Ordering::SeqCst);
        wait_for(|| server.closed.load(Ordering::SeqCst) == accepted);

        // The port's hub is reusable only through a brand-new connection.
        let again = subscribe(server.port).unwrap();
        assert_eq!(again.recv().unwrap(), event);
        assert_eq!(server.accepted.load(Ordering::SeqCst), accepted + 1);
    }

    #[test]
    fn last_drop_returns_promptly_without_waiting_for_the_process() {
        let server = spawn_fake_sse(1);
        let first = subscribe(server.port).unwrap();
        let second = subscribe(server.port).unwrap();
        let _ = first.recv().unwrap();
        let _ = second.recv().unwrap();

        let start = Instant::now();
        drop(first);
        drop(second);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the final drop must not block on the reader or the peer"
        );
        wait_for(|| server.closed.load(Ordering::SeqCst) == 1);
    }

    #[test]
    fn cancel_during_handshake_ends_the_feed_promptly() {
        let server = spawn_silent_listener();
        let feed = subscribe(server.port).unwrap();
        feed.cancel();
        // The handshake never completes, so cancellation must end the feed
        // — not leave `recv` blocked on a read the server never answers.
        let start = Instant::now();
        assert!(feed.recv().is_err());
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "cancellation must unblock a feed whose handshake never completed"
        );
        wait_for(|| server.closed.load(Ordering::SeqCst) == 1);
    }

    // ---- the transport itself, over real sockets ----
    //
    // The fake servers below speak real HTTP so the hub is exercised the way
    // OpenCode serves `/api/event`: chunked bodies, frames split anywhere,
    // heartbeat comments, wrong answers, and deliberate closes. Every valid
    // answer still carries `200` plus `Content-Type: text/event-stream`,
    // because the handshake validation is strict on purpose.

    /// A loopback endpoint that runs every accepted connection through the
    /// same script. Unlike `FakeServer` it can answer with anything, so a
    /// test can drive the hub's transport decisions directly.
    struct ScriptedServer {
        port: u16,
        accepted: Arc<AtomicUsize>,
    }

    impl ScriptedServer {
        /// Connections this endpoint has accepted so far.
        fn connections(&self) -> usize {
            self.accepted.load(Ordering::SeqCst)
        }
    }

    fn spawn_scripted_server<F>(handler: F) -> ScriptedServer
    where
        F: Fn(TcpStream) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(handler);
        {
            let accepted = Arc::clone(&accepted);
            let handler = Arc::clone(&handler);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { return };
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let handler = Arc::clone(&handler);
                    thread::spawn(move || handler(stream));
                }
            });
        }
        ScriptedServer { port, accepted }
    }

    /// Reads the hub's request head off one accepted connection, the way a
    /// real endpoint does before answering.
    fn read_request_head(stream: &TcpStream) {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) if line.trim().is_empty() => return,
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }

    /// Holds a served stream open the way a live SSE endpoint does, until
    /// the hub hangs up.
    fn hold_open_until_peer_hangs_up(stream: &TcpStream) {
        let mut reader = BufReader::new(stream);
        let mut buffer = [0u8; 256];
        while matches!(reader.read(&mut buffer), Ok(count) if count > 0) {}
    }

    /// A valid SSE response head. `chunked` adds the transfer encoding whose
    /// framing the hub must strip instead of decoding as events.
    fn sse_head(chunked: bool) -> Vec<u8> {
        let mut head = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n");
        if chunked {
            head.push_str("Transfer-Encoding: chunked\r\n");
        }
        head.push_str("\r\n");
        head.into_bytes()
    }

    /// One HTTP chunk around body bytes.
    fn chunk(bytes: &[u8]) -> Vec<u8> {
        let mut framed = format!("{:x}\r\n", bytes.len()).into_bytes();
        framed.extend_from_slice(bytes);
        framed.extend_from_slice(b"\r\n");
        framed
    }

    /// The zero-length chunk that ends a chunked body.
    fn final_chunk() -> Vec<u8> {
        b"0\r\n\r\n".to_vec()
    }

    /// One SSE event carried by a single `data:` line.
    fn sse_event(json: &str) -> Vec<u8> {
        format!("data: {json}\n\n").into_bytes()
    }

    /// The hub `subscribe` would use for `port`, without starting a reader,
    /// so a test can install its own subscriber or drive one itself.
    fn hub_for_port(port: u16) -> Arc<Hub> {
        hubs()
            .lock()
            .entry(port)
            .or_insert_with(|| Hub::new(port))
            .clone()
    }

    /// Installs one subscriber on a fresh hub and returns both, so a test
    /// can watch what a reader it drives delivers.
    fn hub_with_subscriber(port: u16) -> (Arc<Hub>, Receiver<Value>) {
        let hub = hub_for_port(port);
        let (tx, rx) = unbounded();
        *hub.state.lock() = HubState::Starting(vec![(1, tx)]);
        (hub, rx)
    }

    /// Ends a test-driven reader exactly the way the last `cancel` does.
    fn stop_test_reader(hub: &Hub) {
        *hub.state.lock() = HubState::Stopping;
        hub.changed.notify_all();
        if let Some(socket) = hub.socket.lock().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    /// Waits until a connection counter stops moving for a good while, so a
    /// reader that ignores its teardown cannot pass by retrying slower than
    /// the observation window.
    fn wait_until_settled(counter: &AtomicUsize) -> usize {
        let mut settled = counter.load(Ordering::SeqCst);
        let mut stable_since = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
            let current = counter.load(Ordering::SeqCst);
            if current != settled {
                settled = current;
                stable_since = Instant::now();
            } else if stable_since.elapsed() >= Duration::from_millis(700) {
                return settled;
            }
        }
        settled
    }

    #[test]
    fn reconnects_and_keeps_fanning_out_when_the_server_closes_the_stream() {
        let server = spawn_scripted_server(|stream| {
            read_request_head(&stream);
            let mut writer = stream.try_clone().unwrap();
            let _ = writer.write_all(&sse_head(false));
            let _ = writer.write_all(&sse_event(r#"{"type":"tick"}"#));
            let _ = writer.flush();
            // Dropping the stream here is the transport failure the hub has
            // to ride out: the server itself stays up for the next attempt.
        });
        let first = subscribe(server.port).unwrap();
        let second = subscribe(server.port).unwrap();
        assert_eq!(first.recv().unwrap()["type"], "tick");
        assert_eq!(second.recv().unwrap()["type"], "tick");

        // The closed connection is replaced, and both feeds keep receiving
        // from every generation of it.
        wait_for(|| server.connections() >= 3);
        assert_eq!(first.recv().unwrap()["type"], "tick");
        assert_eq!(second.recv().unwrap()["type"], "tick");
        wait_for(|| first.connection_generation() >= 3);
        drop(first);
        drop(second);
    }

    #[test]
    fn connection_generation_counts_every_successful_handshake() {
        let server = spawn_scripted_server(|stream| {
            read_request_head(&stream);
            // A slow head holds the first handshake open long enough to see
            // the generation before any connection has counted.
            thread::sleep(Duration::from_millis(500));
            let mut writer = stream.try_clone().unwrap();
            let _ = writer.write_all(&sse_head(false));
            let _ = writer.write_all(&sse_event(r#"{"type":"tick"}"#));
            let _ = writer.flush();
            // The close that follows is what makes the hub reconnect.
        });
        let feed = subscribe(server.port).unwrap();
        assert_eq!(feed.connection_generation(), 0);
        wait_for(|| feed.connection_generation() == 1);
        assert_eq!(feed.recv().unwrap()["type"], "tick");
        // Each successful handshake — this reconnect included — moves it.
        wait_for(|| feed.connection_generation() == 2);
        assert_eq!(feed.recv().unwrap()["type"], "tick");
        drop(feed);
    }

    #[test]
    fn recv_timeout_tells_a_quiet_stream_from_an_ended_one() {
        let server = spawn_scripted_server(|stream| {
            read_request_head(&stream);
            let mut writer = stream.try_clone().unwrap();
            let _ = writer.write_all(&sse_head(false));
            let _ = writer.flush();
            // Business silence: a healthy connection with nothing to report.
            thread::sleep(Duration::from_millis(600));
            let _ = writer.write_all(&sse_event(r#"{"type":"late"}"#));
            let _ = writer.flush();
            hold_open_until_peer_hangs_up(&stream);
        });
        let feed = subscribe(server.port).unwrap();
        let started = Instant::now();
        assert!(matches!(
            feed.recv_timeout(Duration::from_millis(200)),
            Ok(None)
        ));
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "the timeout must actually wait before giving up"
        );
        // A quiet stream is not an ended one: the subscription survives.
        let event = feed.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(event["type"], "late");
        assert!(matches!(
            feed.recv_timeout(Duration::from_millis(100)),
            Ok(None)
        ));
        assert_eq!(
            server.connections(),
            1,
            "silence must not recycle a live connection"
        );
        feed.cancel();
        assert!(matches!(
            feed.recv_timeout(Duration::from_millis(500)),
            Err(_)
        ));
    }

    #[test]
    fn chunked_bodies_and_split_frames_reassemble_before_parsing() {
        let server = spawn_scripted_server(|stream| {
            read_request_head(&stream);
            let mut writer = stream.try_clone().unwrap();
            let _ = writer.write_all(&sse_head(true));
            // One event cut across three chunks, the splits landing inside
            // the JSON. Only complete events are parsed, so chunk framing
            // must never reach the SSE decoder.
            let _ = writer.write_all(&chunk(br#"data: {"type":"a","text":"h"#));
            let _ = writer.write_all(&chunk(br#"ello"}"#));
            let _ = writer.write_all(&chunk(b"\n"));
            let _ = writer.write_all(&chunk(b"\n"));
            // Framing bytes and non-data lines are not events of their own.
            let _ = writer.write_all(&chunk(b": heartbeat\n\n"));
            let _ = writer.write_all(&chunk(b"event: ping\nid: 7\nretry: 1500\n\n"));
            // An event may carry its JSON over several `data:` lines, joined
            // by the blank line that ends it.
            let _ = writer.write_all(&chunk(b"data: {\"type\":\"b\",\n"));
            let _ = writer.write_all(&chunk(b"data: \"n\":2}\n\n"));
            let _ = writer.write_all(&final_chunk());
            let _ = writer.flush();
            hold_open_until_peer_hangs_up(&stream);
        });
        let feed = subscribe(server.port).unwrap();
        let first = feed.recv().unwrap();
        assert_eq!(first["type"], "a");
        assert_eq!(first["text"], "hello");
        let second = feed.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(second["n"], 2);
        // The finished chunked body ends that connection; the hub opens the
        // next one rather than treating it as a dead stream.
        wait_for(|| feed.connection_generation() >= 2);
        drop(feed);
    }

    #[test]
    fn frames_split_by_read_timeouts_and_utf8_bytes_decode_intact() {
        let server = spawn_scripted_server(|stream| {
            read_request_head(&stream);
            let mut writer = stream.try_clone().unwrap();
            let _ = writer.write_all(&sse_head(false));
            let _ = writer.flush();
            // Every gap here outlasts the reader's poll interval, so partial
            // bytes have to survive the timeout that ends each read.
            thread::sleep(Duration::from_millis(300));
            let _ = writer.write_all(br#"data: {"type":"u","text":"caf"#);
            let _ = writer.flush();
            // The split lands inside a two-byte UTF-8 character.
            thread::sleep(Duration::from_millis(300));
            let _ = writer.write_all(&[0xC3]);
            let _ = writer.flush();
            thread::sleep(Duration::from_millis(300));
            let _ = writer.write_all(&[0xA9, b'"', b'}', b'\n', b'\n']);
            let _ = writer.flush();
            hold_open_until_peer_hangs_up(&stream);
        });
        let feed = subscribe(server.port).unwrap();
        let event = feed.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        assert_eq!(event["type"], "u");
        assert_eq!(event["text"], "caf\u{00E9}");
        assert_eq!(server.connections(), 1);
        drop(feed);
    }

    #[test]
    fn heartbeats_keep_a_quiet_stream_alive_without_events() {
        let server = spawn_scripted_server(|stream| {
            read_request_head(&stream);
            let mut writer = stream.try_clone().unwrap();
            let _ = writer.write_all(&sse_head(false));
            let _ = writer.flush();
            // Only heartbeat comments for a while: bytes keep arriving while
            // nothing business-relevant does.
            for _ in 0..8 {
                thread::sleep(Duration::from_millis(100));
                let _ = writer.write_all(b": heartbeat\n\n");
                let _ = writer.flush();
            }
            let _ = writer.write_all(&sse_event(r#"{"type":"after_quiet"}"#));
            let _ = writer.flush();
            hold_open_until_peer_hangs_up(&stream);
        });
        let feed = subscribe(server.port).unwrap();
        for _ in 0..4 {
            assert!(matches!(
                feed.recv_timeout(Duration::from_millis(150)),
                Ok(None)
            ));
        }
        let event = feed.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(event["type"], "after_quiet");
        assert_eq!(server.connections(), 1);
        drop(feed);
    }

    #[test]
    fn a_socket_with_no_bytes_at_all_is_recycled_but_keeps_its_subscribers() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let server = {
            let attempts = Arc::clone(&attempts);
            spawn_scripted_server(move |stream| {
                // Number the connections: the first stays silent forever,
                // the ones after it answer with an event.
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                read_request_head(&stream);
                let mut writer = stream.try_clone().unwrap();
                let _ = writer.write_all(&sse_head(false));
                let _ = writer.flush();
                if attempt > 1 {
                    let _ = writer.write_all(&sse_event(r#"{"type":"recycled"}"#));
                    let _ = writer.flush();
                }
                hold_open_until_peer_hangs_up(&stream);
            })
        };
        let (hub, events) = hub_with_subscriber(server.port);
        let reader = Arc::clone(&hub);
        // Only the idle budget shrinks here, so a decision production takes
        // 45 seconds to reach can be observed inside a test.
        thread::spawn(move || {
            run_reader_with_budgets(
                reader,
                Budgets {
                    handshake: HANDSHAKE_TIMEOUT,
                    idle: Duration::from_millis(400),
                },
            )
        });
        // The silent connection is recycled even though nothing failed.
        wait_for(|| server.connections() >= 2);
        // The subscriber that waited through the dead connection is still
        // subscribed and receives from the replacement.
        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(event["type"], "recycled");
        assert!(hub.generation.load(Ordering::SeqCst) >= 2);
        stop_test_reader(&hub);
        wait_for(|| matches!(*hub.state.lock(), HubState::Idle));
    }

    #[test]
    fn non_200_answers_are_retried_never_delivered() {
        let server = spawn_scripted_server(|mut stream| {
            read_request_head(&stream);
            let _ = stream
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
            let _ = stream.flush();
        });
        let feed = subscribe(server.port).unwrap();
        wait_for(|| server.connections() >= 2);
        assert!(matches!(
            feed.recv_timeout(Duration::from_millis(300)),
            Ok(None)
        ));
        drop(feed);
    }

    #[test]
    fn non_event_stream_answers_are_retried_never_delivered() {
        let server = spawn_scripted_server(|mut stream| {
            read_request_head(&stream);
            let _ =
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{}");
            let _ = stream.flush();
        });
        let feed = subscribe(server.port).unwrap();
        wait_for(|| server.connections() >= 2);
        assert!(matches!(
            feed.recv_timeout(Duration::from_millis(300)),
            Ok(None)
        ));
        drop(feed);
    }

    #[test]
    fn cancelling_during_a_reconnect_backoff_stops_the_reader() {
        let server = spawn_scripted_server(|stream| {
            read_request_head(&stream);
            // Accept and hang up without a head: the transport fails at once
            // and the reader spends its time in backoff.
        });
        let feed = subscribe(server.port).unwrap();
        wait_for(|| server.connections() >= 2);
        feed.cancel();
        let before = server.connections();
        let settled = wait_until_settled(&server.accepted);
        // Only the attempt already in flight when the cancellation landed
        // can still complete; the sleep ends with it.
        assert!(
            settled <= before + 1,
            "the reader kept reconnecting after the last cancellation"
        );
        assert!(matches!(
            feed.recv_timeout(Duration::from_millis(500)),
            Err(_)
        ));
    }

    #[test]
    fn a_panicking_reader_releases_the_hub_instead_of_stranding_it() {
        let server = spawn_scripted_server(|mut stream| {
            read_request_head(&stream);
            let _ = stream.write_all(&sse_head(false));
            let _ = stream.write_all(&sse_event(r#"{"type":"alive"}"#));
            let _ = stream.flush();
            hold_open_until_peer_hangs_up(&stream);
        });
        let (hub, events) = hub_with_subscriber(server.port);
        {
            // A fault inside the reader's guarded scope unwinds past its
            // loop; the guard, not the happy path, has to release the hub.
            let _guard = HubGuard::new(&hub);
            let hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                panic!("reader fault injection");
            }));
            std::panic::set_hook(hook);
            assert!(panicked.is_err());
        }
        assert!(
            matches!(*hub.state.lock(), HubState::Idle),
            "a panic must not leave the hub claiming a reader"
        );
        assert!(
            events.recv().is_err(),
            "subscribers must see the stream end"
        );
        // The port still works: the next subscriber starts a real reader
        // instead of waiting on the one that panicked.
        let feed = subscribe(server.port).unwrap();
        let event = feed.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(event["type"], "alive");
        drop(feed);
    }

    #[test]
    fn oversized_heads_and_events_are_refused_not_buffered() {
        // A response head that never ends within its budget.
        let head_server = spawn_scripted_server(|stream| {
            read_request_head(&stream);
            let mut writer = stream.try_clone().unwrap();
            let _ = writer.write_all(b"HTTP/1.1 200 OK\r\n");
            let _ = writer.write_all(&vec![b'x'; 64 * 1024]);
            let _ = writer.flush();
            hold_open_until_peer_hangs_up(&stream);
        });
        let feed = subscribe(head_server.port).unwrap();
        wait_for(|| head_server.connections() >= 2);
        drop(feed);

        // An event line that never ends either.
        let event_server = spawn_scripted_server(|stream| {
            read_request_head(&stream);
            let mut writer = stream.try_clone().unwrap();
            let _ = writer.write_all(&sse_head(false));
            let _ = writer.write_all(b"data: ");
            let _ = writer.write_all(&vec![b'a'; 9 * 1024 * 1024]);
            let _ = writer.flush();
            hold_open_until_peer_hangs_up(&stream);
        });
        let feed = subscribe(event_server.port).unwrap();
        wait_for(|| event_server.connections() >= 2);
        // A refused frame recycles the connection; it is not an ending.
        assert!(matches!(
            feed.recv_timeout(Duration::from_millis(300)),
            Ok(None)
        ));
        drop(feed);
    }
}

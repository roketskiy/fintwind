//! One server-sent event connection per OpenCode server port.
//!
//! An OpenCode server broadcasts every session's events on `GET /api/event`,
//! so when several sessions share one server, per-driver connections each
//! receive the whole site-wide stream. The hub keeps a single SSE connection
//! per port and fans the parsed JSON events out to every subscriber;
//! SessionFamily filtering stays with the drivers.
//!
//! Reader threads hold only the server's port, never an `OpenCodeServer` or
//! `PooledServer` handle: a reader holding a handle would prevent the global
//! server's reclamation by `shutdown_all`, deadlocking the reader against
//! the process it is waiting to see exit.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{Context, anyhow};
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::{Condvar, Mutex};
use serde_json::Value;

use crate::opencode_session::basic_authorization;

/// Reads poll at this interval — during setup and for the stream's whole
/// life — so a teardown that shut the socket down is noticed even when the
/// network stack never wakes a blocked recv.
const HANDSHAKE_POLL: Duration = Duration::from_millis(200);

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
    /// even though the reader itself blocks inside `read_line`.
    socket: Mutex<Option<TcpStream>>,
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
        .or_insert_with(|| {
            Arc::new(Hub {
                port,
                state: Mutex::new(HubState::Idle),
                changed: Condvar::new(),
                socket: Mutex::new(None),
            })
        })
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
    /// Returns an error once the stream has ended — the server closed it, a
    /// read failed, or every feed was dropped. There is no reconnect: the
    /// caller decides what a dead stream means.
    pub(crate) fn recv(&self) -> anyhow::Result<Value> {
        self.rx
            .recv()
            .map_err(|_| anyhow!("opencode event stream on port {} has ended", self.hub.port))
    }

    /// Unsubscribes. The last cancellation for a port shuts the SSE socket
    /// down and returns promptly; the reader thread finishes on its own.
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

impl Hub {
    /// Registers the reader's socket so a teardown can wake it. Fails when
    /// the hub started tearing down while the connection was being set up.
    fn attach(&self, stream: &TcpStream) -> std::io::Result<bool> {
        let socket = stream.try_clone()?;
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
    /// keeping whatever subscribers queued while it was connecting.
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
}

fn run_reader(hub: Arc<Hub>) {
    // The reader that consumed the handshake continues into streaming: a
    // fresh `BufReader` over the raw socket would drop whatever payload
    // arrived buffered behind the response head in the same TCP segment.
    if let Ok(mut reader) = open_stream(&hub) {
        hub.promote();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                // The read keeps the short poll timeout for the stream's
                // whole life, so a teardown shuts the socket down into a
                // read that would otherwise block forever — on Windows the
                // network stack may never deliver shutdown's wake itself —
                // and each expiry re-checks teardown. Partial bytes stay
                // buffered, so continuing never loses a mid-line read.
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.kind() == ErrorKind::TimedOut =>
                {
                    if hub.is_tearing_down() {
                        break;
                    }
                    continue;
                }
                // Any read error ends the stream. There is no reconnect,
                // matching the driver treating a broken stream as exit; a
                // later subscribe starts a fresh connection.
                Err(_) => break,
                Ok(_) => {
                    if let Some(event) = parse_data_line(&line) {
                        hub.broadcast(event);
                    }
                }
            }
        }
    }
    hub.reader_finished();
}

/// Parses one SSE line. Only `data:` lines carry events; anything else
/// (comments, the blank line between events) is skipped, and a line that is
/// not valid JSON is dropped, both matching the driver's stream handling.
fn parse_data_line(line: &str) -> Option<Value> {
    let payload = line.strip_prefix("data:")?;
    serde_json::from_str(payload.trim()).ok()
}

/// Opens the port's server-sent event stream and leaves it open.
///
/// Returns the reader that consumed the response head, so the streaming
/// loop keeps reading from it — bytes that arrived in the same TCP segment
/// as the head stay buffered there instead of being lost. The shared
/// request helper reads a whole response before returning, which a stream
/// never finishes doing.
fn open_stream(hub: &Hub) -> anyhow::Result<BufReader<TcpStream>> {
    let port = hub.port;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("could not connect to OpenCode on local port {port}"))?;
    // Register before reading the response head too. If the last subscriber
    // cancels while setup is blocked, teardown can still close the socket and
    // wake the reader.
    if !hub.attach(&stream)? {
        return Err(anyhow!(
            "opencode event stream on port {port} was cancelled during setup"
        ));
    }
    let mut request = format!(
        "GET /api/event HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n"
    );
    if let Some(authorization) = basic_authorization(port) {
        request.push_str(&authorization);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    write!(stream, "{request}")?;
    stream.flush()?;
    // Skip the response head; every later line is stream payload. The head
    // read polls a short timeout instead of blocking forever: on Windows a
    // WFP/AV layer intercepting loopback can delay shutdown's wake of a
    // blocked recv indefinitely, and teardown must stay responsive
    // regardless of the network stack.
    stream.set_read_timeout(Some(HANDSHAKE_POLL))?;
    // This reader owns the socket from here on: the streaming loop must
    // receive the very same buffered reader, or a first event sharing the
    // head's TCP segment is lost inside the handshake buffer.
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Err(anyhow!("OpenCode closed the event stream during setup")),
            Ok(_) => {}
            // A timed-out read keeps its partial bytes buffered; keep polling
            // until the head completes or teardown wins.
            Err(error)
                if error.kind() == ErrorKind::WouldBlock || error.kind() == ErrorKind::TimedOut =>
            {
                if hub.is_tearing_down() {
                    let _ = reader.get_ref().shutdown(Shutdown::Both);
                    return Err(anyhow!(
                        "opencode event stream on port {port} was cancelled during setup"
                    ));
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        }
        if line.trim().is_empty() {
            break;
        }
    }
    // The short poll timeout stays on for the stream's whole life: the
    // streaming read polls too, so a teardown wakes it even on Windows,
    // where the network stack may never deliver shutdown's wake itself.
    Ok(reader)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
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
}

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use parking_lot::Mutex;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

use fintwind_protocol::MAX_WIRE_MESSAGE_BYTES;
use fintwind_protocol::browser::{
    BrowserAction, BrowserRequest, BrowserResult, BrowserScope, BrowserShare,
    MAX_BROWSER_PAGES_PER_CONNECTION, MAX_BROWSER_RESULT_BYTES, MAX_BROWSER_TITLE_BYTES,
};
use fintwind_protocol::{
    ClientMessage, Command, PROTOCOL_VERSION, ReplayCursor, Request, ResponseOutcome,
    ResponsePayload, RpcError, SequencedEvent, ServerMessage, WireDriverEvent,
};

const READ_POLL_INTERVAL: Duration = Duration::from_millis(25);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_BUFFERED_EVENTS_PER_RUNTIME: usize = 4096;
/// Browser notifications are delivered to a GUI consumer that must keep up.
/// A small bounded channel keeps a stuck or absent consumer from growing
/// daemon memory: an undeliverable request is answered with an immediate
/// error result, and an undeliverable cancel, rejection or revocation ends
/// the lease instead of being ignored.
const MAX_BUFFERED_BROWSER_NOTIFICATIONS: usize = 128;

/// Live-only browser traffic delivered to the GUI. There is deliberately no
/// replay: a reconnecting client starts with no pending requests, and a
/// request already delivered is never delivered twice.
pub enum BrowserNotification {
    /// The daemon routed one action to this client's published page.
    Request(BrowserRequest),
    /// The daemon gave up on a request (timeout or the invoker vanished).
    Cancel(Uuid),
    /// The daemon refused a publish. The listed scopes are the ones that did
    /// not take, so the GUI drops exactly those grants and shows the error.
    ShareRejected {
        scopes: Vec<BrowserScope>,
        message: String,
    },
    /// These scopes lost their grant because their session runtime ended or
    /// was replaced. A later re-share under a new grant is a different scope
    /// and is not listed here.
    ScopesRevoked(Vec<BrowserScope>),
    /// The connection ended. Pending page grants are gone with it.
    Disconnected,
}

/// A browser notification that cannot be delivered ends the lease. The GUI
/// can no longer be trusted to honor grants — a dropped cancel or revocation
/// could leave an action running — so the subscriber is closed, the client is
/// marked disconnected, and the caller must break the socket loop rather
/// than leave a live socket behind lingering grants.
fn browser_notification_undeliverable(inner: &ClientInner) {
    if let Some(taken) = inner.browser.lock().take() {
        let _ = taken.try_send(BrowserNotification::Disconnected);
    }
    inner.disconnected.store(true, Ordering::Release);
}

enum Outgoing {
    Message(ClientMessage),
    Disconnect,
    Shutdown,
}

struct ClientInner {
    outgoing: Sender<Outgoing>,
    pending: Mutex<HashMap<Uuid, Sender<Result<ResponsePayload, RpcError>>>>,
    sessions: Mutex<HashMap<(Uuid, Uuid), Sender<SequencedEvent>>>,
    pending_events: Mutex<HashMap<(Uuid, Uuid), VecDeque<SequencedEvent>>>,
    task_state_subscribers: Mutex<Vec<Sender<u64>>>,
    /// Single browser consumer. A later `subscribe_browser_requests` call
    /// replaces the previous receiver instead of fanning one request out to
    /// several consumers; requests already delivered are not replayed.
    browser: Mutex<Option<Sender<BrowserNotification>>>,
    last_sequences: Mutex<HashMap<(Uuid, Uuid), LastSequence>>,
    disconnected: AtomicBool,
}

#[derive(Clone, Copy)]
struct LastSequence {
    epoch: Uuid,
    sequence: u64,
}

#[derive(Clone)]
pub struct DaemonClient {
    inner: Arc<ClientInner>,
}

impl DaemonClient {
    pub fn connect(address: &str, token: String) -> anyhow::Result<Self> {
        Self::connect_with_resume(address, token, Vec::new())
    }

    pub fn connect_with_resume(
        address: &str,
        token: String,
        resume_from: Vec<ReplayCursor>,
    ) -> anyhow::Result<Self> {
        let last_sequences = resume_from
            .iter()
            .map(|cursor| {
                (
                    (cursor.session_id, cursor.runtime_id),
                    LastSequence {
                        epoch: cursor.epoch,
                        sequence: cursor.sequence,
                    },
                )
            })
            .collect();
        let url = daemon_url(address)?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_WIRE_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_WIRE_MESSAGE_BYTES));
        let (mut socket, _) =
            tungstenite::client::connect_with_config(url.as_str(), Some(config), 3)
                .context("could not connect to fintwind daemon")?;
        set_client_read_timeout(&mut socket, Some(Duration::from_secs(5)))?;
        write_json(
            &mut socket,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                token,
                client_id: Uuid::new_v4(),
                resume_from,
            },
        )?;
        let hello = read_server_message(&mut socket)?;
        match hello {
            ServerMessage::Hello {
                protocol_version, ..
            } if protocol_version == PROTOCOL_VERSION => {}
            ServerMessage::Hello {
                protocol_version, ..
            } => bail!(
                "daemon protocol {protocol_version} does not match desktop protocol {PROTOCOL_VERSION}"
            ),
            ServerMessage::Rejected { message } => bail!("daemon rejected connection: {message}"),
            other => bail!("daemon sent an invalid handshake response: {other:?}"),
        }
        set_client_read_timeout(&mut socket, Some(READ_POLL_INTERVAL))?;

        let (outgoing, outgoing_rx) = unbounded();
        let inner = Arc::new(ClientInner {
            outgoing,
            pending: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            pending_events: Mutex::new(HashMap::new()),
            task_state_subscribers: Mutex::new(Vec::new()),
            browser: Mutex::new(None),
            last_sequences: Mutex::new(last_sequences),
            disconnected: AtomicBool::new(false),
        });
        let thread_inner = inner.clone();
        std::thread::Builder::new()
            .name("fintwind-daemon-client".into())
            .spawn(move || run_client(socket, outgoing_rx, thread_inner))
            .context("could not start fintwind daemon client thread")?;
        Ok(Self { inner })
    }

    pub fn subscribe(&self, session_id: Uuid, runtime_id: Uuid) -> Receiver<SequencedEvent> {
        let (events, receiver) = unbounded();
        let key = (session_id, runtime_id);
        let mut sessions = self.inner.sessions.lock();
        sessions.insert(key, events.clone());
        // Keep the subscription lock while draining the pre-subscription
        // replay queue. The socket thread takes these locks in the same order,
        // so a new live event cannot overtake older replayed events here.
        if let Some(buffered) = self.inner.pending_events.lock().remove(&key) {
            for event in buffered {
                let _ = events.send(event);
            }
        }
        receiver
    }

    pub fn unsubscribe(&self, session_id: Uuid, runtime_id: Uuid) {
        self.inner.sessions.lock().remove(&(session_id, runtime_id));
    }

    pub fn subscribe_task_state(&self) -> Receiver<u64> {
        let (events, receiver) = unbounded();
        self.inner.task_state_subscribers.lock().push(events);
        receiver
    }

    /// Subscribe to live-only browser traffic. Only one consumer is expected:
    /// subscribing again replaces the previous receiver, and a request that
    /// was already delivered is not replayed to the new one. The replaced
    /// consumer is told the lease is over so it drops its page grants instead
    /// of acting on requests that now belong elsewhere.
    pub fn subscribe_browser_requests(&self) -> Receiver<BrowserNotification> {
        if self.inner.disconnected.load(Ordering::Acquire) {
            // A dead lease: the very first notification reports it.
            let (events, receiver) = bounded(1);
            let _ = events.send(BrowserNotification::Disconnected);
            return receiver;
        }
        let (events, receiver) = bounded(MAX_BUFFERED_BROWSER_NOTIFICATIONS);
        if let Some(previous) = self.inner.browser.lock().replace(events) {
            let _ = previous.try_send(BrowserNotification::Disconnected);
        }
        receiver
    }

    /// Share live browser pages with the daemon. The published set replaces
    /// this connection's previous set, so an empty list revokes every page.
    /// Bounds are checked before enqueueing so an oversized publish fails
    /// locally. The result reports whether the message was enqueued on a live
    /// connection; it never blocks on the socket.
    pub fn publish_browser_pages(&self, pages: Vec<BrowserShare>) -> anyhow::Result<()> {
        if pages.len() > MAX_BROWSER_PAGES_PER_CONNECTION {
            anyhow::bail!(
                "a connection may publish at most {MAX_BROWSER_PAGES_PER_CONNECTION} browser pages"
            );
        }
        for page in &pages {
            if !page.scope.is_well_formed() {
                anyhow::bail!("a published browser scope has nil session, runtime, page or grant");
            }
            if page.title.len() > MAX_BROWSER_TITLE_BYTES {
                anyhow::bail!("a published browser title is too long");
            }
            BrowserAction::validate_url(&page.url).map_err(anyhow::Error::msg)?;
        }
        self.send_browser_message(ClientMessage::BrowserPublish { pages })
    }

    /// Register or clear this connection's browser launcher capability for
    /// one live session runtime. `Some` installs (or replaces) the launcher;
    /// `None` clears it, which fails any open still routed to it and leaves
    /// this connection's page shares untouched. The scope's page and grant
    /// ids are launcher identities, never a real tab: they are not published
    /// as pages and cannot address a page action. Bounds are checked before
    /// enqueueing so a malformed registration fails locally; the result only
    /// reports whether the message was enqueued on a live connection.
    pub fn publish_browser_host(&self, scope: Option<BrowserScope>) -> anyhow::Result<()> {
        if let Some(scope) = &scope
            && !scope.is_well_formed()
        {
            anyhow::bail!("a browser launcher scope has nil session, runtime, page or grant");
        }
        self.send_browser_message(ClientMessage::BrowserHost { scope })
    }

    /// Answer a daemon-delivered browser request. Only the connection that
    /// published the page may answer; the daemon refuses anything else. An
    /// oversized result fails here instead of being silently truncated
    /// somewhere else on the wire.
    pub fn complete_browser_request(
        &self,
        request_id: Uuid,
        result: BrowserResult,
    ) -> anyhow::Result<()> {
        match serde_json::to_vec(&result) {
            Ok(bytes) if bytes.len() <= MAX_BROWSER_RESULT_BYTES => {}
            _ => anyhow::bail!("a browser result exceeds the size limit"),
        }
        self.send_browser_message(ClientMessage::BrowserResult { request_id, result })
    }

    /// Cancel a browser request this connection owns. Cancelling is not
    /// undoing an action that may already have reached the page.
    pub fn cancel_browser_request(&self, request_id: Uuid) -> anyhow::Result<()> {
        self.send_browser_message(ClientMessage::BrowserCancel { request_id })
    }

    fn send_browser_message(&self, message: ClientMessage) -> anyhow::Result<()> {
        if self.inner.disconnected.load(Ordering::Acquire) {
            bail!("fintwind daemon is disconnected");
        }
        // The outgoing queue is unbounded, so this never blocks the GUI
        // thread; a disconnected writer is the only failure, and it means
        // the message was not enqueued on a live lease.
        self.inner
            .outgoing
            .send(Outgoing::Message(message))
            .map_err(|_| anyhow!("fintwind daemon connection is closed"))
    }

    pub fn request(
        &self,
        session_id: Uuid,
        runtime_id: Uuid,
        command: Command,
    ) -> anyhow::Result<ResponsePayload> {
        self.request_with_timeout(session_id, runtime_id, command, REQUEST_TIMEOUT)
    }

    pub fn request_with_timeout(
        &self,
        session_id: Uuid,
        runtime_id: Uuid,
        command: Command,
        timeout: Duration,
    ) -> anyhow::Result<ResponsePayload> {
        if self.inner.disconnected.load(Ordering::Acquire) {
            bail!("fintwind daemon is disconnected");
        }
        let request_id = Uuid::new_v4();
        // A browser invoke acts on a real page. If this caller gives up
        // first, the GUI action must be cancelled too instead of running on
        // with nobody waiting; other commands have no GUI-side leg.
        let cancels_browser_action = matches!(command, Command::BrowserInvoke { .. });
        let (response, response_rx) = bounded(1);
        self.inner.pending.lock().insert(request_id, response);
        let message = ClientMessage::Request(Request {
            request_id,
            session_id,
            runtime_id,
            command,
        });
        if self
            .inner
            .outgoing
            .send(Outgoing::Message(message))
            .is_err()
        {
            self.inner.pending.lock().remove(&request_id);
            bail!("fintwind daemon connection is closed");
        }
        match response_rx.recv_timeout(timeout) {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(error)) => Err(anyhow!(error.message)),
            Err(error) => {
                self.inner.pending.lock().remove(&request_id);
                if cancels_browser_action {
                    // The server maps this back to the internal request id
                    // and tells the page owner to stop. Failure to enqueue
                    // the cancel is reported: a silent cancel would leave a
                    // browser action running.
                    self.send_browser_message(ClientMessage::BrowserCancel { request_id })?;
                }
                Err(anyhow!("timed out waiting for fintwind daemon: {error}"))
            }
        }
    }

    pub fn notify(
        &self,
        session_id: Uuid,
        runtime_id: Uuid,
        command: Command,
    ) -> anyhow::Result<()> {
        if self.inner.disconnected.load(Ordering::Acquire) {
            bail!("fintwind daemon is disconnected");
        }
        self.inner
            .outgoing
            .send(Outgoing::Message(ClientMessage::Request(Request {
                // The nil request id is reserved for fire-and-forget controls;
                // the daemon executes them in the runtime mailbox but does
                // not allocate or send a response.
                request_id: Uuid::nil(),
                session_id,
                runtime_id,
                command,
            })))
            .map_err(|_| anyhow!("fintwind daemon connection is closed"))
    }

    pub fn last_sequences(&self) -> Vec<ReplayCursor> {
        self.inner
            .last_sequences
            .lock()
            .iter()
            .map(|(&(session_id, runtime_id), cursor)| ReplayCursor {
                session_id,
                runtime_id,
                epoch: cursor.epoch,
                sequence: cursor.sequence,
            })
            .collect()
    }

    /// Whether the background socket thread has exited. The flag is terminal:
    /// a disconnected client never recovers on its own, so callers must
    /// obtain a replacement from the daemon supervisor.
    pub fn is_disconnected(&self) -> bool {
        self.inner.disconnected.load(Ordering::Acquire)
    }

    pub fn shutdown(&self) {
        let _ = self.inner.outgoing.send(Outgoing::Shutdown);
    }

    /// Close only this connection, not the daemon or its other clients.
    /// Its browser capabilities end with the socket and are not resumed.
    pub fn disconnect(&self) {
        let _ = self.inner.outgoing.send(Outgoing::Disconnect);
    }
}

fn daemon_url(address: &str) -> anyhow::Result<String> {
    let normalized = if address.starts_with("ws://") || address.starts_with("wss://") {
        address.to_owned()
    } else {
        format!("ws://{address}")
    };
    let mut url = url::Url::parse(&normalized).context("fintwind daemon address is invalid")?;
    url.set_path("/v1");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

fn run_client(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    outgoing: Receiver<Outgoing>,
    inner: Arc<ClientInner>,
) {
    // The loop exits through several branches and the reason is easy to lose
    // on the way (release builds have no stderr at all otherwise), so every
    // break records what happened before the flag goes terminal.
    let mut graceful = false;
    let mut reason = "the daemon closed the connection".to_owned();
    'connection: loop {
        while let Ok(message) = outgoing.try_recv() {
            match message {
                Outgoing::Message(message) => {
                    if let Err(error) = write_json(&mut socket, &message) {
                        reason = format!("a request could not be sent: {error:#}");
                        break 'connection;
                    }
                }
                Outgoing::Shutdown => {
                    let _ = write_json(&mut socket, &ClientMessage::Shutdown);
                    let _ = socket.flush();
                    graceful = true;
                    break 'connection;
                }
                Outgoing::Disconnect => {
                    let _ = socket.close(None);
                    graceful = true;
                    break 'connection;
                }
            }
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                let Ok(message) = serde_json::from_str::<ServerMessage>(text.as_ref()) else {
                    continue;
                };
                match message {
                    ServerMessage::Response {
                        request_id,
                        outcome,
                    } => {
                        if let Some(pending) = inner.pending.lock().remove(&request_id) {
                            let result = match outcome {
                                ResponseOutcome::Ok { payload } => Ok(payload),
                                ResponseOutcome::Error { error } => Err(error),
                            };
                            let _ = pending.send(result);
                        }
                    }
                    ServerMessage::Event(event) => {
                        let should_deliver = {
                            let mut sequences = inner.last_sequences.lock();
                            let previous = sequences
                                .entry((event.session_id, event.runtime_id))
                                .or_insert(LastSequence {
                                    epoch: event.epoch,
                                    sequence: 0,
                                });
                            if previous.epoch == event.epoch && event.sequence <= previous.sequence
                            {
                                false
                            } else {
                                previous.epoch = event.epoch;
                                previous.sequence = event.sequence;
                                true
                            }
                        };
                        if should_deliver {
                            let key = (event.session_id, event.runtime_id);
                            let sessions = inner.sessions.lock();
                            if let Some(events) = sessions.get(&key) {
                                let _ = events.send(event);
                            } else {
                                let mut pending = inner.pending_events.lock();
                                let buffered = pending.entry(key).or_default();
                                buffered.push_back(event);
                                while buffered.len() > MAX_BUFFERED_EVENTS_PER_RUNTIME {
                                    buffered.pop_front();
                                }
                            }
                        }
                    }
                    ServerMessage::TaskStateChanged { revision } => {
                        inner
                            .task_state_subscribers
                            .lock()
                            .retain(|subscriber| subscriber.send(revision).is_ok());
                    }
                    ServerMessage::BrowserRequest { request } => {
                        // Bounded delivery: a GUI that is not draining, has
                        // no subscriber, or dropped its receiver must not
                        // grow daemon-side queues and must not let the
                        // action run unobserved. The request is failed
                        // immediately on the socket thread instead.
                        let request_id = request.request_id;
                        let subscriber = inner.browser.lock().clone();
                        let delivered = match subscriber {
                            Some(events) => events
                                .try_send(BrowserNotification::Request(request))
                                .is_ok(),
                            None => false,
                        };
                        if !delivered {
                            let _ = inner.outgoing.send(Outgoing::Message(
                                ClientMessage::BrowserResult {
                                    request_id,
                                    result: BrowserResult::error(
                                        "the browser client could not accept the request",
                                    ),
                                },
                            ));
                        }
                    }
                    ServerMessage::BrowserCancel { request_id } => {
                        // A cancel must never be silently dropped: if it
                        // cannot be delivered the lease is broken, so the GUI
                        // clears its page grants and the socket loop exits.
                        let subscriber = inner.browser.lock().clone();
                        if let Some(events) = subscriber
                            && events
                                .try_send(BrowserNotification::Cancel(request_id))
                                .is_err()
                        {
                            browser_notification_undeliverable(&inner);
                            break 'connection;
                        }
                    }
                    ServerMessage::BrowserShareRejected { scopes, message } => {
                        let subscriber = inner.browser.lock().clone();
                        if let Some(events) = subscriber
                            && events
                                .try_send(BrowserNotification::ShareRejected { scopes, message })
                                .is_err()
                        {
                            browser_notification_undeliverable(&inner);
                            break 'connection;
                        }
                    }
                    ServerMessage::BrowserScopesRevoked { scopes } => {
                        let subscriber = inner.browser.lock().clone();
                        if let Some(events) = subscriber
                            && events
                                .try_send(BrowserNotification::ScopesRevoked(scopes))
                                .is_err()
                        {
                            browser_notification_undeliverable(&inner);
                            break 'connection;
                        }
                    }
                    ServerMessage::ShuttingDown => {
                        graceful = true;
                        break;
                    }
                    ServerMessage::Hello { .. } | ServerMessage::Rejected { .. } => {}
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_)) => {
                let _ = socket.flush();
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if retryable_io(&error) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => break,
            Err(error) => {
                reason = format!("the connection failed: {error}");
                break;
            }
        }
    }

    if !graceful {
        eprintln!("fintwind daemon connection lost: {reason}");
    }
    let pending = std::mem::take(&mut *inner.pending.lock());
    for (_, response) in pending {
        let _ = response.send(Err(RpcError {
            message: "fintwind daemon disconnected".into(),
        }));
    }
    let sessions = std::mem::take(&mut *inner.sessions.lock());
    for ((session_id, runtime_id), events) in sessions {
        // This event is synthesized locally and is not present in the
        // daemon's replay journal. Do not advance the replay cursor for it or
        // reconnecting to the same daemon would skip the next real event.
        let (epoch, sequence) = inner
            .last_sequences
            .lock()
            .get(&(session_id, runtime_id))
            .map(|cursor| (cursor.epoch, cursor.sequence))
            .unwrap_or((Uuid::nil(), 0));
        let _ = events.send(SequencedEvent {
            session_id,
            runtime_id,
            epoch,
            sequence,
            event: WireDriverEvent::new("processExited", serde_json::Value::Null),
        });
    }
    // Page grants died with the connection; tell the GUI before the terminal
    // flag so a consumer cannot observe "connected" and pending work at once.
    // try_send: a GUI that already stopped draining must not stall the exit
    // path; dropping the sender closes the channel, which the consumer sees
    // as the same lease end.
    if let Some(events) = inner.browser.lock().take() {
        let _ = events.try_send(BrowserNotification::Disconnected);
    }
    // Only now, with the synthetic session exits handed to their subscribers,
    // does the supervisor-visible flag go up: otherwise a supervisor poll
    // could replace this client while `is_disconnected()` still races the
    // delivery of those exits.
    inner.disconnected.store(true, Ordering::Release);
    inner.task_state_subscribers.lock().clear();
}

fn set_client_read_timeout(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Option<Duration>,
) -> io::Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(timeout),
        #[allow(unreachable_patterns)]
        _ => Ok(()),
    }
}

fn retryable_io(error: &io::Error) -> bool {
    retryable_error(error)
}

fn retryable_error(error: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(error) = error.downcast_ref::<io::Error>() {
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
        ) {
            return true;
        }
        #[cfg(unix)]
        if error.raw_os_error() == Some(libc::EAGAIN)
            || error.raw_os_error() == Some(libc::EWOULDBLOCK)
        {
            return true;
        }
    }
    error.source().is_some_and(retryable_error)
}

fn write_json<S: io::Read + io::Write, T: serde::Serialize>(
    socket: &mut WebSocket<S>,
    value: &T,
) -> anyhow::Result<()> {
    let payload = serde_json::to_string(value)?;
    socket.send(Message::Text(payload.into()))?;
    Ok(())
}

fn read_server_message(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
) -> anyhow::Result<ServerMessage> {
    loop {
        match socket.read()? {
            Message::Text(text) => return Ok(serde_json::from_str(text.as_ref())?),
            Message::Ping(_) => socket.flush()?,
            Message::Close(_) => bail!("fintwind daemon closed during handshake"),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_endpoint_accepts_addresses_and_secure_urls() {
        assert_eq!(
            daemon_url("127.0.0.1:4312").unwrap(),
            "ws://127.0.0.1:4312/v1"
        );
        assert_eq!(
            daemon_url("wss://fintwind.example.test/old?ignored=1").unwrap(),
            "wss://fintwind.example.test/v1"
        );
    }
}

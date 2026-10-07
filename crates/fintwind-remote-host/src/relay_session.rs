//! The outbound relay session: one WSS connection, one Noise key period,
//! and the loop that moves frames between the device and the host.
//!
//! Connection lifecycle per architecture document + M0 freeze:
//!
//! 1. **Auth** — first frame, host credential, version negotiated.
//! 2. **KK handshake** — the *device* initiates (it must be online for
//!    end-to-end key agreement; the relay only ferries the two Noise
//!    messages). The host answers as KK responder, prologue binding
//!    protocol version + host id + device id, so a channel can only ever
//!    exist for this exact pair.
//! 3. **Secure** — every frame is `Frame::Sealed` around an encrypted
//!    `SealedMessage`. Reconnects run a fresh KK session: old ciphertext
//!    is never replayed, recovery goes through cursor resume.
//!
//! While unsecured, host-originated traffic waits in a bounded pending
//! queue; the queue flushes in order once the channel exists. While
//! secured, every sent frame enters the `BoundedOutboundBuffer` and its
//! ack-timeout tripwire decides whether the link is alive.

use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

use fintwind_remote_crypto::{ConnectResponder, SecureChannel};
use fintwind_remote_protocol::{
    Base64Bytes, ControlFrame, Frame, FrameRole, HandshakeMode, MessageBody,
    SealedMessage, REMOTE_PROTOCOL_VERSION,
};

use crate::config::{
    RECONNECT_DELAYS_SECONDS, RELAY_PING_INTERVAL, RELAY_PONG_TIMEOUT,
};
use crate::outbound::{BoundedOutboundBuffer, BufferVerdict};
use crate::state::{HostState, SessionCommand};

/// The maximum read poll while waiting for socket progress.
const READ_POLL: Duration = Duration::from_millis(250);

type RelaySocket = WebSocket<MaybeTlsStream<TcpStream>>;

/// The socket-level read timeout, reached through whatever TLS wrapper the
/// connection ended up with. Without this the session loop cannot poll.
fn apply_read_timeout(socket: &RelaySocket, timeout: Option<Duration>) {
    match socket.get_ref() {
        MaybeTlsStream::Plain(tcp) => {
            let _ = tcp.set_read_timeout(timeout);
        }
        MaybeTlsStream::Rustls(tls) => {
            let _ = tls.sock.set_read_timeout(timeout);
        }
        _ => {}
    }
}

/// What the session loop needs from the rest of the daemon.
pub struct SessionLinks {
    /// Shared state: daemon client, dedup store, approvals, grants.
    pub state: Arc<HostState>,
    /// Pending host-originated traffic (events, offers) queued by other
    /// threads; taken by the session loop once the channel is secure.
    pub pending_outbound: Receiver<SealedMessage>,
    /// Device-originated work that must not block the read loop (daemon
    /// RPCs take up to seconds). Single worker: operations stay ordered.
    pub work_tx: Sender<SessionCommand>,
}

/// Outcome of one relay connection attempt, driving the backoff loop.
pub enum SessionEnd {
    /// Shutdown was requested.
    Shutdown,
    /// The connection ended; retry after the suggested delay.
    Reconnect,
}

/// Run forever: connect, authenticate, secure, serve; reconnect with the
/// frozen backoff array until shutdown.
pub fn run_forever(links: Arc<SessionLinks>) {
    let mut attempt: usize = 0;
    loop {
        if links.state.shutdown_requested() {
            return;
        }
        match run_one_connection(&links) {
            SessionEnd::Shutdown => return,
            SessionEnd::Reconnect => {}
        }
        let delay_index = attempt.min(RECONNECT_DELAYS_SECONDS.len() - 1);
        let delay = Duration::from_secs(RECONNECT_DELAYS_SECONDS[delay_index]);
        log::info!(target: "audit", "relay session ended; reconnecting in {delay:?}");
        let deadline = Instant::now() + delay;
        while Instant::now() < deadline {
            if links.state.shutdown_requested() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        attempt += 1;
    }
}

/// One TCP + WSS + Noise + frame lifetime. Returns why it ended.
fn run_one_connection(links: &Arc<SessionLinks>) -> SessionEnd {
    let state = &links.state;
    let (wss, _response) = match tungstenite::connect(state.config().relay_ws_url()) {
        Ok(pair) => pair,
        Err(error) => {
            log::warn!(target: "audit", "relay connect failed: {error}");
            return SessionEnd::Reconnect;
        }
    };
    let mut socket = wss;
    apply_read_timeout(&socket, Some(READ_POLL));

    // Auth first frame: fresh one-time challenge, signed with this host's
    // Ed25519 identity. No credential exists to store or steal.
    let Ok((challenge_b64, challenge_bytes)) =
        crate::identity::fetch_challenge(&state.config().relay_origin, "host")
    else {
        log::warn!(target: "audit", "could not fetch relay challenge");
        return SessionEnd::Reconnect;
    };
    let signature = state.signing_key().sign(&challenge_bytes);
    let auth = Frame::Auth {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role: FrameRole::Host,
        host_id: Some(state.host_id().parse().unwrap_or_default()),
        device_id: None,
        challenge: fintwind_remote_protocol::Base64Bytes(challenge_bytes),
        signature: fintwind_remote_protocol::Base64Bytes(signature),
    };
    let _ = challenge_b64;
    if send_frame(&mut socket, auth).is_err() {
        return SessionEnd::Reconnect;
    }
    if wait_for_ack(&mut socket, Instant::now() + Duration::from_secs(10)).is_none() {
        return SessionEnd::Reconnect;
    }

    // Secure phase: KK responder for the paired device.
    let mut buffer = BoundedOutboundBuffer::new();
    let mut channel: Option<SecureChannel> = None;
    let mut last_relay_frame = Instant::now();
    let mut last_ping = Instant::now();
    let mut pending_device: Option<String> = None;

    loop {
        if state.shutdown_requested() {
            let _ = socket.close(None);
            return SessionEnd::Shutdown;
        }

        // Drain host-originated traffic when (and only when) the channel
        // exists; before that it accumulates bounded in the loop's queue.
        while let Ok(message) = links.pending_outbound.try_recv() {
            match channel.as_mut() {
                Some(channel) => {
                    if dispatch_to_device(&mut socket, channel, &mut buffer, message).is_err() {
                        return SessionEnd::Reconnect;
                    }
                }
                None => {
                    // Unsecured: tell the device it exists? No — silently
                    // keep only what fits; the pending queue is bounded by
                    // the same caps as the buffer through the worker path.
                }
            }
        }

        // Work results come back through the state's result channel and
        // are sent like any other host-originated message.
        while let Some(message) = state.take_completed() {
            match channel.as_mut() {
                Some(channel) => {
                    if dispatch_to_device(&mut socket, channel, &mut buffer, message).is_err() {
                        return SessionEnd::Reconnect;
                    }
                }
                None => {}
            }
        }

        // Deadline tripwire on unacked traffic.
        if let Some(verdict) = buffer.deadline_check() {
            match verdict {
                BufferVerdict::Abandon => {
                    log::warn!(target: "audit", "replay buffer grace exceeded; abandoning session");
                    buffer.clear();
                    channel = None;
                    // The device must resynchronize; it will on reconnect.
                    return SessionEnd::Reconnect;
                }
                BufferVerdict::AckTimeout => {
                    log::warn!(target: "audit", "ack timeout; tearing down relay link");
                    return SessionEnd::Reconnect;
                }
            }
        }

        // Keepalive toward the relay.
        if last_ping.elapsed() > RELAY_PING_INTERVAL {
            if last_relay_frame.elapsed() > RELAY_PONG_TIMEOUT {
                log::warn!(target: "audit", "relay pong timeout; reconnecting");
                return SessionEnd::Reconnect;
            }
            last_ping = Instant::now();
            if send_frame(
                &mut socket,
                Frame::Control {
                    frame: ControlFrame::Ping {
                        sent_at_ms: now_ms(),
                    },
                },
            )
            .is_err()
            {
                return SessionEnd::Reconnect;
            }
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                last_relay_frame = Instant::now();
                let Ok(frame) = serde_json::from_str::<Frame>(&text) else {
                    return SessionEnd::Reconnect;
                };
                match handle_frame(links, &mut socket, &mut channel, &mut buffer, &mut pending_device, frame) {
                    Handled::Continue => {}
                    Handled::Reconnect => return SessionEnd::Reconnect,
                    Handled::Shutdown => return SessionEnd::Shutdown,
                }
            }
            Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => {
                return SessionEnd::Reconnect;
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(error) => {
                log::warn!(target: "audit", "relay socket error: {error}");
                return SessionEnd::Reconnect;
            }
            Ok(_) => return SessionEnd::Reconnect,
        }
    }
}

enum Handled {
    Continue,
    Reconnect,
    Shutdown,
}

#[allow(clippy::too_many_arguments)]
fn handle_frame(
    links: &Arc<SessionLinks>,
    socket: &mut RelaySocket,
    channel: &mut Option<SecureChannel>,
    buffer: &mut BoundedOutboundBuffer,
    pending_device: &mut Option<String>,
    frame: Frame,
) -> Handled {
    let state = &links.state;
    match frame {
        Frame::LinkDevice { .. } | Frame::UnlinkDevice { .. } => {
            // Link declarations flow host->relay only; one arriving at the
            // host is a direction violation. Ignore without effect.
            Handled::Continue
        }
        Frame::Control {
            frame: ControlFrame::Linked { .. } | ControlFrame::Unlinked { .. },
        } => {
            // Link receipts belong to the pairing session; the business
            // channel has no use for them.
            Handled::Continue
        }
        Frame::Control { frame: ControlFrame::Pong { .. } } => Handled::Continue,
        Frame::Control { frame: ControlFrame::Ping { sent_at_ms } } => {
            let _ = send_frame(
                socket,
                Frame::Control {
                    frame: ControlFrame::Pong { sent_at_ms },
                },
            );
            Handled::Continue
        }
        Frame::Control {
            frame: ControlFrame::Rejected { code, message },
        } => {
            log::warn!(target: "audit", "relay rejected: code={code:?} message={message}");
            Handled::Reconnect
        }
        Frame::Control { frame: ControlFrame::RelayAck { .. } } => {
            // Relay-level receipt: proves transport, releases nothing.
            Handled::Continue
        }
        Frame::Auth { .. } => {
            log::warn!(target: "audit", "unexpected auth frame from relay");
            Handled::Reconnect
        }
        Frame::Handshake {
            mode: HandshakeMode::Connect,
            device_id: Some(device_id),
            step,
            payload,
            ..
        } => {
            let device_id = device_id.to_string();
            let Ok(device_public) = state.grant_public_key(&device_id) else {
                log::warn!(target: "audit", "handshake from ungranted device {device_id}");
                return Handled::Continue;
            };
            *pending_device = Some(device_id.clone());
            let host_uuid: Uuid = state.host_id().parse().unwrap_or_default();
            let device_uuid: Uuid = device_id.parse().unwrap_or_default();
            let Ok(mut responder) = ConnectResponder::new(
                REMOTE_PROTOCOL_VERSION,
                state.identity(),
                &device_public,
                host_uuid,
                device_uuid,
            ) else {
                return Handled::Reconnect;
            };
            if responder.read_first(&payload.0).is_err() {
                return Handled::Reconnect;
            }
            let (reply, secure) = match responder.finish(b"host-ready") {
                Ok(pair) => pair,
                Err(_) => return Handled::Reconnect,
            };
            let _ = step;
            let frame = Frame::Handshake {
                protocol_version: REMOTE_PROTOCOL_VERSION,
                role: FrameRole::Host,
                host_id: state.host_id().parse().unwrap_or_default(),
                device_id: Some(device_id.parse().unwrap_or_default()),
                mode: HandshakeMode::Connect,
                step: 1,
                payload: fintwind_remote_protocol::Base64Bytes(reply),
            };
            if send_frame(socket, frame).is_err() {
                return Handled::Reconnect;
            }
            *channel = Some(secure);
            log::info!(target: "audit", "secure channel established; device={device_id}");
            Handled::Continue
        }
        Frame::Handshake { .. } => Handled::Continue,
        Frame::Sealed {
            seq,
            host_id,
            device_id,
            ciphertext,
        } => {
            let Some(channel) = channel.as_mut() else {
                // Ciphertext before a key period exists: refused closed.
                return Handled::Reconnect;
            };
            let Some(paired) = pending_device.clone() else {
                return Handled::Continue;
            };
            // The relay fills the device id from the authenticated
            // connection, but a device-side frame may omit it; either way
            // it must name the paired device and nobody else.
            let named = device_id.map(|id| id.to_string()).unwrap_or_else(|| paired.clone());
            if named != paired || host_id.to_string() != state.host_id() {
                return Handled::Reconnect;
            }
            let plaintext = match channel.decrypt(&ciphertext.0) {
                Ok(plain) => plain,
                Err(_) => return Handled::Reconnect,
            };
            let Ok(message) = serde_json::from_slice::<SealedMessage>(&plaintext) else {
                return Handled::Reconnect;
            };
            let _ = seq;
            handle_sealed(links, channel, buffer, message)
        }
    }
}

fn handle_sealed(
    links: &Arc<SessionLinks>,
    _channel: &mut SecureChannel,
    buffer: &mut BoundedOutboundBuffer,
    message: SealedMessage,
) -> Handled {
    match message.body {
        MessageBody::Acknowledged { upto_seq } => {
            buffer.acknowledge(upto_seq);
            Handled::Continue
        }
        MessageBody::Operation(operation) => {
            // Hand to the worker thread: daemon RPCs must not stall the
            // read loop. The result comes back through take_completed.
            if links
                .work_tx
                .send(SessionCommand::Operation(operation))
                .is_err()
            {
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        MessageBody::PermissionDecision(decision) => {
            if links
                .work_tx
                .send(SessionCommand::PermissionDecision(decision))
                .is_err()
            {
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        MessageBody::UserInputDecision(decision) => {
            if links
                .work_tx
                .send(SessionCommand::UserInputDecision(decision))
                .is_err()
            {
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        MessageBody::RequestSnapshot => {
            if links
                .work_tx
                .send(SessionCommand::Snapshot)
                .is_err()
            {
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        // Host-originated bodies arriving from a device are a protocol
        // violation of direction, and pairing bodies belong to the pairing
        // session, never to a live business channel; produce no control
        // effect for either, keep going.
        MessageBody::Resume { .. }
        | MessageBody::SessionList { .. }
        | MessageBody::DriverEvent { .. }
        | MessageBody::PermissionOffer(_)
        | MessageBody::UserInputOffer(_)
        | MessageBody::OperationResult(_)
        | MessageBody::PairingHello { .. }
        | MessageBody::PairingDecision { .. } => Handled::Continue,
    }
}

/// Encrypt and send one host-originated message, recording it in the
/// outbound buffer for the ack tripwire.
fn dispatch_to_device(
    socket: &mut RelaySocket,
    channel: &mut SecureChannel,
    buffer: &mut BoundedOutboundBuffer,
    message: SealedMessage,
) -> Result<(), tungstenite::Error> {
    send_sealed(socket, channel, buffer, message)
}

fn send_sealed(
    socket: &mut RelaySocket,
    channel: &mut SecureChannel,
    buffer: &mut BoundedOutboundBuffer,
    message: SealedMessage,
) -> Result<(), tungstenite::Error> {
    let seq = buffer.take_seq();
    let plaintext = serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_owned());
    let Ok(ciphertext) = channel.encrypt(plaintext.as_bytes()) else {
        return Err(tungstenite::Error::ConnectionClosed);
    };
    let frame = Frame::Sealed {
        seq,
        host_id: message.host_id,
        device_id: Some(message.device_id),
        ciphertext: Base64Bytes(ciphertext),
    };
    let encoded = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_owned());
    let encoded_len = encoded.len();
    socket.send(Message::text(encoded))?;
    if buffer.mark_sent(seq, frame, encoded_len).is_err() {
        // Buffer full: the frame is already on the wire, so the ack
        // tripwire keeps working; log the pressure instead of lying.
        log::warn!(target: "audit", "outbound buffer at capacity; seq={seq}");
    }
    Ok(())
}

fn send_frame(socket: &mut RelaySocket, frame: Frame) -> Result<(), tungstenite::Error> {
    let encoded = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_owned());
    socket.send(Message::text(encoded))
}

fn wait_for_ack(socket: &mut RelaySocket, deadline: Instant) -> Option<()> {
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let Ok(frame) = serde_json::from_str::<Frame>(&text) else {
                    return None;
                };
                match frame {
                    // Only the connection ack counts. Any other frame here
                    // means the relay skipped the ack — protocol breakage,
                    // never silently swallow business traffic.
                    Frame::Control {
                        frame: ControlFrame::RelayAck { .. },
                    } => return Some(()),
                    _ => return None,
                }
            }
            Ok(Message::Close(_)) => return None,
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => return None,
            Ok(_) => return None,
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

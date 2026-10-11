//! The outbound relay session: one WSS connection, and one Noise key
//! period **per device** that talks over it.
//!
//! Connection lifecycle per architecture document + M0 freeze:
//!
//! 1. **Auth** — first frame, host credential, version negotiated.
//! 2. **KK handshake** — each *device* initiates (it must be online for
//!    end-to-end key agreement; the relay only ferries the two Noise
//!    messages). The host answers as KK responder, prologue binding
//!    protocol version + host id + device id, so a channel can only ever
//!    exist for this exact pair.
//! 3. **Secure** — every frame is `Frame::Sealed` around an encrypted
//!    `SealedMessage`, demultiplexed by the device the relay authenticated
//!    it from. Reconnects run a fresh KK session per device: old ciphertext
//!    is never replayed, recovery goes through cursor resume.
//!
//! ## Isolation
//!
//! Nothing about one device's channel leaks into another's. A device that
//! disconnects, corrupts a frame, misses its acks or loses its grant loses
//! *its own* channel; every other device keeps its link, its delivery
//! sequence and its unacked buffer. A frame from a device this connection
//! has no channel for is dropped, never answered and never fatal.
//!
//! Outbound traffic waits in the host's bounded queue; the loop drains it
//! (results before events) whenever the target device has a channel, and
//! refuses to send when that device's sent-unacked buffer is full — a full
//! buffer means the device stopped acknowledging, and the honest response
//! is to rebuild *its* link, never to push untracked ciphertext. Anything a
//! device might have missed is re-derivable: events from the next snapshot
//! or a cursor resume, and operation results from the durable record
//! through `QueryOperationResult`.
//!
//! Two limits are enforced on the wire itself, not only on plaintext: the
//! socket is configured with `MAX_FRAME_BYTES` as its message and frame
//! ceiling, and every encoded frame is checked again before it is sent.
//! Inside the ciphertext, every sealed message repeats its frame's
//! delivery sequence as the authenticated `transport_seq`; the host stamps
//! it before encrypting and verifies it after decrypting, so a relay that
//! renumbers frames is detected and the channel closes. A protocol version
//! mismatch closes the link immediately; there is no downgrade path.

use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

use fintwind_remote_crypto::ConnectResponder;
use fintwind_remote_protocol::{
    Base64Bytes, ConnectionPurpose, ControlFrame, Frame, FrameRole, HandshakeMode, MessageBody,
    SealedMessage, MAX_FRAME_BYTES, MAX_SEALED_PLAINTEXT, REMOTE_PROTOCOL_VERSION,
};

use crate::config::{RECONNECT_DELAYS_SECONDS, RELAY_PING_INTERVAL, RELAY_PONG_TIMEOUT};
use crate::devices::{DeviceChannel, DeviceChannels};
use crate::outbound::{BoundedOutboundBuffer, BufferVerdict};
use crate::pending::{PendingKind, PendingMessage};
use crate::state::{HostState, SessionCommand};

/// The maximum read poll while waiting for socket progress.
const READ_POLL: Duration = Duration::from_millis(250);

/// How many queued messages one loop iteration may put on the wire.
const PENDING_DRAIN_BATCH: usize = 256;

/// How many batches one loop iteration may drain before yielding to the
/// socket read. Keeps a live event stream from starving the device's acks.
const MAX_DRAIN_BATCHES_PER_TICK: usize = 8;

/// How often the loop re-reads the grant file to notice a revocation or a
/// re-pairing. Cheap (one small file), and it is what makes a revocation
/// reach a live channel instead of waiting for the next reconnect.
const GRANT_RECHECK_INTERVAL: Duration = Duration::from_secs(5);

/// The host-wide ceiling on unacknowledged outbound ciphertext, across every
/// device channel at once.
///
/// Per-device caps alone are not a bound: with enough paired devices, each
/// individually under its own cap, the host could still hold an unbounded
/// total in memory. This is the second, global ceiling — when the total is
/// reached, nothing more is put on the wire until devices acknowledge, and
/// the honest response is to wait, never to push untracked ciphertext.
const MAX_TOTAL_UNACKED_BYTES: usize = 32 * 1024 * 1024;

type RelaySocket = WebSocket<MaybeTlsStream<TcpStream>>;

/// The largest raw chunk one fragment can carry. A fragment is itself a
/// sealed message, and its `data` field is base64 — a 4/3 expansion — inside
/// the `SealedMessage` + `Fragment` envelope. So the chunk must be a
/// fraction of the Noise plaintext limit, not the limit itself.
const FRAGMENT_CHUNK_BYTES: usize = MAX_SEALED_PLAINTEXT / 4 * 3 - 1_024;

/// What the session loop needs from the rest of the daemon.
pub struct SessionLinks {
    /// Shared state: daemon client, dedup store, approvals, grants, the
    /// bounded outbound queue.
    pub state: Arc<HostState>,
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
    // The wire limit is enforced by the protocol layer, not only by the
    // endpoint's own checks: an oversized frame is refused before it is
    // buffered, which is the difference between a limit and a log line.
    let socket_config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let (wss, _response) = match tungstenite::client::connect_with_config(
        state.config().relay_ws_url(),
        Some(socket_config),
        3,
    ) {
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
    let Ok((_challenge_b64, challenge_bytes)) =
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
        // The long-lived business session slot. Pairing rides its own
        // short-lived channel; the two never displace each other.
        purpose: ConnectionPurpose::Session,
        challenge: fintwind_remote_protocol::Base64Bytes(challenge_bytes),
        signature: fintwind_remote_protocol::Base64Bytes(signature),
    };
    if send_frame(&mut socket, auth).is_err() {
        return SessionEnd::Reconnect;
    }
    if wait_for_ack(&mut socket, Instant::now() + Duration::from_secs(10)).is_none() {
        return SessionEnd::Reconnect;
    }
    state.set_link_status(true);
    log::info!(target: "audit", "relay session established");

    // Every device channel this connection serves. Dropped together when
    // the connection ends: a device's channel cannot outlive the socket.
    let mut channels = DeviceChannels::default();
    let mut last_relay_frame = Instant::now();
    let mut last_ping = Instant::now();
    let mut last_grant_check = Instant::now();

    loop {
        if state.shutdown_requested() {
            let _ = socket.close(None);
            for device_id in channels.ids() {
                state.note_device_offline(device_id);
            }
            state.set_link_status(false);
            return SessionEnd::Shutdown;
        }

        // Drain host-originated traffic for the devices that have a
        // channel. Bounded per tick so a flood never starves the reads.
        {
            let mut ticks = 0;
            let mut batch = state.take_pending(PENDING_DRAIN_BATCH);
            while !batch.is_empty() && ticks < MAX_DRAIN_BATCHES_PER_TICK {
                let mut unsent: Vec<PendingMessage> = Vec::new();
                let mut deferred: Vec<PendingMessage> = Vec::new();
                let mut transport_dead = false;
                // The global ceiling, checked once per batch: per-device
                // caps do not bound the total across every device.
                if channels.total_unacked_bytes() >= MAX_TOTAL_UNACKED_BYTES {
                    state.requeue_pending_back(batch);
                    break;
                }
                for message in batch {
                    if transport_dead {
                        unsent.push(message);
                        continue;
                    }
                    let target = message.message.device_id;
                    let Some(device) = channels.get_mut(target) else {
                        // This device has no channel (offline, or its
                        // channel was just dropped). Its traffic waits
                        // without blocking anyone else's.
                        deferred.push(message);
                        continue;
                    };
                    let PendingMessage {
                        message: sealed,
                        bytes,
                        enqueued_at_ms,
                        kind,
                    } = message;
                    match dispatch_to_device(&mut socket, device, &sealed) {
                        Ok(()) => {}
                        Err(failure) => {
                            unsent.push(PendingMessage {
                                message: failure.message,
                                bytes,
                                enqueued_at_ms,
                                kind,
                            });
                            match failure_kind(&failure.error) {
                                DispatchScope::Link => {
                                    // The socket itself is dead: this
                                    // connection is over, for every device.
                                    log::warn!(
                                        target: "audit",
                                        "relay transport failed while sending; device={target}"
                                    );
                                    transport_dead = true;
                                }
                                DispatchScope::Device => {
                                    // Only this device's channel is the
                                    // problem; everyone else keeps theirs.
                                    log::warn!(
                                        target: "audit",
                                        "dropping device channel after a dispatch failure; device={target} error={}",
                                        failure.error
                                    );
                                    channels.remove(target);
                                    state.note_device_offline(target);
                                }
                            }
                        }
                    }
                }
                if !unsent.is_empty() {
                    // The failed message and everything after it go back to
                    // the head of the queue, in order.
                    state.requeue_pending_front(unsent);
                }
                if !deferred.is_empty() {
                    state.requeue_pending_back(deferred);
                }
                if transport_dead {
                    for device_id in channels.ids() {
                        state.note_device_offline(device_id);
                    }
                    state.set_link_status(false);
                    return SessionEnd::Reconnect;
                }
                ticks += 1;
                batch = state.take_pending(PENDING_DRAIN_BATCH);
            }
        }

        // Devices the relay refused a frame for. Their ciphertext is gone,
        // so they are told to resynchronize from a snapshot and a cursor —
        // per device, never by tearing the whole host link down.
        for device_id in state.take_desync_devices() {
            if !channels.contains(device_id) {
                continue;
            }
            log::warn!(
                target: "audit",
                "asking device to resynchronize after a refused frame; device={device_id}"
            );
            state.send_to_device(
                MessageBody::Resync {
                    session_id: None,
                    reason: "a frame addressed to this device was refused before delivery; \
                             rebuild from a snapshot"
                        .to_owned(),
                },
                device_id,
                PendingKind::Event,
            );
            // The authoritative session list rides along, so the device has
            // something to rebuild from the moment it is told to.
            crate::events::push_session_list(state, device_id, true);
        }

        // Per-device ack tripwires. Isolated on purpose: one device's dead
        // link never tears down another's live channel.
        for (device_id, verdict) in channels.deadline_verdicts() {
            match verdict {
                BufferVerdict::Abandon => {
                    log::warn!(
                        target: "audit",
                        "replay buffer grace exceeded; dropping device channel; device={device_id}"
                    );
                    channels.remove(device_id);
                    state.note_device_offline(device_id);
                }
                BufferVerdict::AckTimeout => {
                    log::warn!(
                        target: "audit",
                        "ack timeout; dropping device channel; device={device_id}"
                    );
                    channels.remove(device_id);
                    state.note_device_offline(device_id);
                }
            }
        }

        // Grant re-check: a revocation or a re-pairing must reach a live
        // channel, not wait for the device to reconnect.
        if last_grant_check.elapsed() > GRANT_RECHECK_INTERVAL {
            last_grant_check = Instant::now();
            for device_id in channels.ids() {
                let current = state.grant_version(device_id);
                let channel_version = channels.get(device_id).map(|c| c.grant_version);
                if current == 0 || Some(current) != channel_version {
                    log::warn!(
                        target: "audit",
                        "device grant changed or revoked; dropping channel; device={device_id}"
                    );
                    channels.remove(device_id);
                    state.note_device_offline(device_id);
                    state
                        .approvals()
                        .invalidate_device(device_id, "the device grant changed");
                    for dropped in state.queues().lock().drop_device(device_id) {
                        // The daemon's durable copy is released here too: a
                        // revoked device must not leave a prompt on this
                        // computer's books that a later re-pairing could
                        // adopt, and that a restart would restore.
                        state.cancel_remote_prompt_as_recorded(device_id, dropped.operation_id);
                        // A cancel that was waiting on this prompt is
                        // answered here: the host released the row itself, so
                        // the outcome is known without asking the daemon
                        // again — and nothing is re-sent.
                        if let Some(cancel_operation_id) = state
                            .queues()
                            .lock()
                            .take_cancel_request(dropped.session_id, dropped.operation_id)
                        {
                            state.send_to_device(
                                MessageBody::OperationResult(
                                    fintwind_remote_protocol::OperationOutcome {
                                        operation_id: cancel_operation_id,
                                        state: fintwind_remote_protocol::OperationState::Executed,
                                        detail: Some(
                                            serde_json::json!({
                                                "cancelled": true,
                                                "operationId": dropped.operation_id,
                                            })
                                            .to_string(),
                                        ),
                                    },
                                ),
                                device_id,
                                crate::pending::PendingKind::Result,
                            );
                        }
                        report_dropped_queue_item(state, &dropped, "the device grant changed");
                    }
                }
            }
        }

        // Keepalive toward the relay.
        if last_ping.elapsed() > RELAY_PING_INTERVAL {
            if last_relay_frame.elapsed() > RELAY_PONG_TIMEOUT {
                log::warn!(target: "audit", "relay pong timeout; reconnecting");
                for device_id in channels.ids() {
                    state.note_device_offline(device_id);
                }
                state.set_link_status(false);
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
                for device_id in channels.ids() {
                    state.note_device_offline(device_id);
                }
                state.set_link_status(false);
                return SessionEnd::Reconnect;
            }
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                last_relay_frame = Instant::now();
                // The socket is configured to refuse frames past the
                // limit; this is the second, explicit check on the bytes
                // that actually arrived.
                if text.len() > MAX_FRAME_BYTES {
                    log::warn!(
                        target: "audit",
                        "relay frame exceeds the wire limit; len={} limit={MAX_FRAME_BYTES}",
                        text.len()
                    );
                    for device_id in channels.ids() {
                        state.note_device_offline(device_id);
                    }
                    state.set_link_status(false);
                    return SessionEnd::Reconnect;
                }
                let Ok(frame) = serde_json::from_str::<Frame>(&text) else {
                    log::warn!(target: "audit", "relay frame failed to parse; len={}", text.len());
                    for device_id in channels.ids() {
                        state.note_device_offline(device_id);
                    }
                    state.set_link_status(false);
                    return SessionEnd::Reconnect;
                };
                match handle_frame(
                    links,
                    &mut socket,
                    &mut channels,
                    frame,
                ) {
                    Handled::Continue => {}
                    Handled::Reconnect => {
                        for device_id in channels.ids() {
                            state.note_device_offline(device_id);
                        }
                        state.set_link_status(false);
                        return SessionEnd::Reconnect;
                    }
                }
            }
            Ok(Message::Close(frame)) => {
                log::warn!(target: "audit", "relay closed the link; close_frame={frame:?}");
                for device_id in channels.ids() {
                    state.note_device_offline(device_id);
                }
                state.set_link_status(false);
                return SessionEnd::Reconnect;
            }
            Err(tungstenite::Error::ConnectionClosed) => {
                log::warn!(target: "audit", "relay closed the link (connection closed)");
                for device_id in channels.ids() {
                    state.note_device_offline(device_id);
                }
                state.set_link_status(false);
                return SessionEnd::Reconnect;
            }
            Ok(other) => {
                // A protocol-level frame (ping/pong/binary) from the relay
                // or the TLS proxy in front of it: name it, never guess.
                log::warn!(target: "audit", "relay link dropped on a non-text message: {other:?}");
                for device_id in channels.ids() {
                    state.note_device_offline(device_id);
                }
                state.set_link_status(false);
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
                for device_id in channels.ids() {
                    state.note_device_offline(device_id);
                }
                state.set_link_status(false);
                return SessionEnd::Reconnect;
            }
        }
    }
}

enum Handled {
    Continue,
    Reconnect,
}

fn handle_frame(
    links: &Arc<SessionLinks>,
    socket: &mut RelaySocket,
    channels: &mut DeviceChannels,
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
            frame: ControlFrame::Rejected {
                code,
                message,
                frame: refused,
            },
        } => {
            // A refused frame is a routing fact about one frame, never an
            // endpoint result: the frame did not reach its device, so
            // nothing was executed, accepted or acknowledged there.
            //
            // The structured reference is the only authority. The human
            // message is never parsed for a target — a sequence number
            // alone cannot name a frame, because several devices behind one
            // host number their own frames and may share a `seq`.
            match refused {
                Some(refused) if refused.host_id.to_string() == state.host_id() => {
                    if let Some(device_id) = refused.device_id {
                        // Only this device's recovery is touched. Another
                        // device's channel, buffer and delivery sequence are
                        // untouched, and the link itself stays up.
                        let released = channels
                            .get_mut(device_id)
                            .map(|device| device.buffer.release(refused.seq))
                            .unwrap_or(false);
                        if released {
                            log::warn!(
                                target: "audit",
                                "relay refused a frame; device={device_id} seq={} reason={:?}; \
                                 released that device's frame and asked it to resynchronize",
                                refused.seq,
                                refused.reason
                            );
                            // The ciphertext is dropped, not replayed: it
                            // belongs to a Noise period the device may have
                            // already left, and re-sending it in a new one
                            // is forbidden. Recovery re-derives from host
                            // state instead.
                            state.note_device_desync(device_id);
                        } else {
                            log::warn!(
                                target: "audit",
                                "relay refused a frame this host no longer holds; \
                                 device={device_id} seq={} reason={:?}",
                                refused.seq,
                                refused.reason
                            );
                        }
                    }
                }
                _ => {
                    log::warn!(
                        target: "audit",
                        "relay refused a frame it did not attribute; code={code:?} message={message}"
                    );
                }
            }
            Handled::Continue
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
            protocol_version,
            mode: HandshakeMode::Connect,
            device_id: Some(device_id),
            step,
            payload,
            ..
        } => {
            // Version before anything else: a peer that does not speak this
            // protocol is closed, never negotiated down to.
            if protocol_version != REMOTE_PROTOCOL_VERSION {
                log::warn!(
                    target: "audit",
                    "connect handshake protocol version {protocol_version} is not {REMOTE_PROTOCOL_VERSION}"
                );
                return Handled::Reconnect;
            }
            let Ok(device_public) = state.grant_public_key(device_id) else {
                log::warn!(target: "audit", "handshake from ungranted device {device_id}");
                return Handled::Continue;
            };
            let grant_version = state.grant_version(device_id);
            if grant_version == 0 {
                log::warn!(target: "audit", "handshake from device without a live grant {device_id}");
                return Handled::Continue;
            }
            let host_uuid: Uuid = state.host_id().parse().unwrap_or_default();
            let Ok(mut responder) = ConnectResponder::new(
                REMOTE_PROTOCOL_VERSION,
                state.identity(),
                &device_public,
                host_uuid,
                device_id,
            ) else {
                return Handled::Reconnect;
            };
            if responder.read_first(&payload.0).is_err() {
                // A broken handshake kills only this device's attempt; the
                // relay link and every other device's channel survive.
                log::warn!(target: "audit", "handshake message 1 failed; device={device_id}");
                return Handled::Continue;
            }
            let (reply, secure) = match responder.finish(b"host-ready") {
                Ok(pair) => pair,
                Err(error) => {
                    log::warn!(target: "audit", "handshake completion failed; device={device_id} error={error}");
                    return Handled::Continue;
                }
            };
            let _ = step;
            let frame = Frame::Handshake {
                protocol_version: REMOTE_PROTOCOL_VERSION,
                role: FrameRole::Host,
                host_id: state.host_id().parse().unwrap_or_default(),
                device_id: Some(device_id),
                mode: HandshakeMode::Connect,
                step: 1,
                payload: fintwind_remote_protocol::Base64Bytes(reply),
            };
            if send_frame(socket, frame).is_err() {
                return Handled::Reconnect;
            }
            // A re-handshake means a new key period: the device's previous
            // channel (and its fragment table) is replaced, never merged.
            channels.insert(DeviceChannel {
                device_id,
                channel: secure,
                buffer: BoundedOutboundBuffer::new(),
                fragments: Default::default(),
                grant_version,
            });
            state.note_device_online(device_id, grant_version);
            log::info!(target: "audit", "secure channel established; device={device_id}");
            // Offers raised while this device was away are still valid —
            // they were never tied to a connection. Send them now, before
            // the device asks, because a paused turn is waiting.
            crate::events::push_pending_offers(state, device_id);
            Handled::Continue
        }
        Frame::Handshake { protocol_version, .. } => {
            // Any other handshake shape on the business connection (a
            // pairing-mode frame, or one that names no device) is not
            // something this channel serves — but a version mismatch is
            // still a hard close.
            if protocol_version != REMOTE_PROTOCOL_VERSION {
                log::warn!(
                    target: "audit",
                    "handshake protocol version {protocol_version} is not {REMOTE_PROTOCOL_VERSION}"
                );
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        Frame::Sealed {
            seq,
            host_id,
            device_id,
            ciphertext,
        } => {
            // The relay fills the device id from the authenticated
            // connection; a frame that names no device cannot be routed.
            let Some(device_id) = device_id else {
                log::warn!(target: "audit", "sealed frame names no device; dropped");
                return Handled::Continue;
            };
            let Some(device) = channels.get_mut(device_id) else {
                // A device this connection has no channel for: its frames
                // are dropped, never answered and never fatal.
                log::warn!(target: "audit", "sealed frame for a device without a channel; device={device_id}; dropped");
                return Handled::Continue;
            };
            if host_id.to_string() != state.host_id() {
                log::warn!(
                    target: "audit",
                    "sealed frame claims another host; device={device_id}; dropped"
                );
                return Handled::Continue;
            }
            let plaintext = match device.channel.decrypt(&ciphertext.0) {
                Ok(plain) => plain,
                Err(error) => {
                    // A frame this device's key period cannot open: its
                    // channel is over. Only that device is affected.
                    log::warn!(
                        target: "audit",
                        "sealed frame decrypt failed; device={device_id} len={} err={error}; dropping that device's channel",
                        ciphertext.0.len()
                    );
                    channels.remove(device_id);
                    state.note_device_offline(device_id);
                    return Handled::Continue;
                }
            };
            let Ok(message) = serde_json::from_slice::<SealedMessage>(&plaintext) else {
                log::warn!(target: "audit", "sealed body failed to parse; device={device_id}");
                channels.remove(device_id);
                state.note_device_offline(device_id);
                return Handled::Continue;
            };
            // Authenticated-payload checks. Everything below was inside
            // the ciphertext, so it is the peer's own word — and it must
            // agree with what the relay claimed on the outside.
            if !payload_is_current(state, device_id, &message) {
                log::warn!(
                    target: "audit",
                    "sealed message failed the version or identity check; device={device_id}"
                );
                channels.remove(device_id);
                state.note_device_offline(device_id);
                return Handled::Continue;
            }
            // The authenticated delivery sequence must equal the frame's
            // relay-visible seq. A relay that renumbers frames is detected
            // here instead of corrupting ordering or the cumulative ack.
            // A fragment carries its own physical frame seq, exactly like
            // any other message, so it is checked too; the *logical*
            // message the fragments reassemble into is named by
            // `MessageBody::Fragment::transport_seq` and is not a frame seq.
            if message.transport_seq != seq {
                log::warn!(
                    target: "audit",
                    "sealed message transport_seq {} does not match frame seq {seq}; device={device_id}; dropping that device's channel",
                    message.transport_seq
                );
                channels.remove(device_id);
                state.note_device_offline(device_id);
                return Handled::Continue;
            }
            handle_sealed(links, channels, device_id, message)
        }
    }
}

/// The version and identity part of the authenticated-payload check,
/// shared by whole frames and reassembled fragment groups. The delivery
/// sequence is deliberately *not* checked here: only a whole frame's
/// envelope seq can be compared to its frame, and a reassembled message
/// carries the logical seq instead.
fn payload_is_current(state: &HostState, device_id: Uuid, message: &SealedMessage) -> bool {
    message.protocol_version == REMOTE_PROTOCOL_VERSION
        && message.host_id.to_string() == state.host_id()
        && message.device_id == device_id
}

fn handle_sealed(
    links: &Arc<SessionLinks>,
    channels: &mut DeviceChannels,
    device_id: Uuid,
    message: SealedMessage,
) -> Handled {
    match message.body {
        MessageBody::LivenessProbe { probe_id } => {
            links.state.send_to_device(MessageBody::LivenessReply { probe_id },
                device_id, PendingKind::Event);
            Handled::Continue
        }
        MessageBody::Acknowledged { upto_seq } => {
            // Only this device's end-to-end acknowledgement releases this
            // device's unacked frames — and an ack is never itself queued
            // for delivery, so there is no ack loop.
            if let Some(device) = channels.get_mut(device_id) {
                device.buffer.acknowledge(upto_seq);
            }
            Handled::Continue
        }
        MessageBody::Operation(operation) => {
            // Hand to the worker thread: daemon RPCs must not stall the
            // read loop. The result comes back through the pending queue.
            if links
                .work_tx
                .send(SessionCommand::Operation { device_id, operation })
                .is_err()
            {
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        MessageBody::PermissionDecision(decision) => {
            if links
                .work_tx
                .send(SessionCommand::PermissionDecision { device_id, decision })
                .is_err()
            {
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        MessageBody::UserInputDecision(decision) => {
            if links
                .work_tx
                .send(SessionCommand::UserInputDecision { device_id, decision })
                .is_err()
            {
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        MessageBody::RequestSnapshot => {
            if links
                .work_tx
                .send(SessionCommand::Snapshot { device_id })
                .is_err()
            {
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        MessageBody::QueryOperationResult { operation_id } => {
            // A result query executes nothing; the worker answers from the
            // durable record.
            if links
                .work_tx
                .send(SessionCommand::QueryResult {
                    device_id,
                    operation_id,
                })
                .is_err()
            {
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        MessageBody::Resume { cursors } => {
            if links
                .work_tx
                .send(SessionCommand::Resume { device_id, cursors })
                .is_err()
            {
                return Handled::Reconnect;
            }
            Handled::Continue
        }
        MessageBody::Fragment {
            transport_seq,
            index,
            of,
            data,
        } => {
            // A device-originated message that was too large for one Noise
            // frame. Reassemble and dispatch the body it carries, exactly
            // as if it had arrived whole.
            let Some(device) = channels.get_mut(device_id) else {
                return Handled::Continue;
            };
            if !device.fragments.insert(transport_seq, index, of, data.0) {
                log::warn!(
                    target: "audit",
                    "fragment refused; device={device_id} seq={transport_seq} index={index} of={of}"
                );
                return Handled::Continue;
            }
            if !device.fragments.is_complete(transport_seq) {
                return Handled::Continue;
            }
            let Some(plaintext) = device.fragments.take(transport_seq) else {
                return Handled::Continue;
            };
            let Ok(message) = serde_json::from_slice::<SealedMessage>(&plaintext) else {
                log::warn!(
                    target: "audit",
                    "fragment reassembly failed to parse; device={device_id} seq={transport_seq}"
                );
                return Handled::Continue;
            };
            // The reassembled payload was never on the wire as one frame,
            // so its envelope seq is the logical message's, not a frame seq
            // — the seq check does not apply here. The version and identity
            // checks do.
            if !payload_is_current(&links.state, device_id, &message) {
                log::warn!(
                    target: "audit",
                    "reassembled message failed the version or identity check; device={device_id}"
                );
                channels.remove(device_id);
                links.state.note_device_offline(device_id);
                return Handled::Continue;
            }
            handle_sealed(links, channels, device_id, message)
        }
        // Host-originated bodies arriving from a device are a protocol
        // violation of direction, and pairing bodies belong to the pairing
        // session, never to a live business channel; produce no control
        // effect for either, keep going.
        MessageBody::LivenessReply { .. }
        | MessageBody::SessionList { .. }
        | MessageBody::ProjectList { .. }
        | MessageBody::WorkspaceUnavailable { .. }
        | MessageBody::DriverEvent { .. }
        | MessageBody::Transcript { .. }
        | MessageBody::PermissionOffer(_)
        | MessageBody::UserInputOffer(_)
        | MessageBody::OperationResult(_)
        | MessageBody::SessionQueue { .. }
        | MessageBody::OptionsCatalog { .. }
        | MessageBody::Resync { .. }
        | MessageBody::PairingHello { .. }
        | MessageBody::PairingDecision { .. } => Handled::Continue,
    }
}

/// Why one host-originated message could not be put on the wire. The
/// transport and the crypto layer fail for unrelated reasons, and naming
/// them apart is what keeps a too-large message from being read as a dead
/// link: an encryption failure is a problem with that one message, not
/// with the connection.
enum DispatchError {
    /// The relay link itself failed; the session must reconnect.
    Transport(tungstenite::Error),
    /// The device's outbound buffer is full: it stopped acknowledging.
    /// Refused before the wire, so nothing untracked was ever sent.
    Backpressure,
    /// The encoded frame would exceed the wire limit. Cannot happen for a
    /// message the fragmenter sized correctly, so it is a bug guard.
    Oversized,
    /// Noise refused the message — in practice `Error::Input` because the
    /// sealed JSON exceeds `MAX_SEALED_PLAINTEXT`.
    Encrypt(fintwind_remote_crypto::CryptoError),
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::Backpressure => {
                write!(f, "outbound buffer is full; refusing to send untracked ciphertext")
            }
            Self::Oversized => write!(f, "encoded frame exceeds the wire limit"),
            Self::Encrypt(error) => write!(f, "encrypt refused the message: {error}"),
        }
    }
}

/// A dispatch that could not be completed, with the message handed back so
/// the caller can put it back in the queue.
struct DispatchFailure {
    error: DispatchError,
    message: SealedMessage,
}

/// What one dispatch failure costs: the whole link, or just the device it
/// was addressed to.
enum DispatchScope {
    /// The socket is dead; the connection is over for every device.
    Link,
    /// Only this device's channel is unusable; the others keep theirs.
    Device,
}

fn failure_kind(error: &DispatchError) -> DispatchScope {
    match error {
        DispatchError::Transport(_) => DispatchScope::Link,
        DispatchError::Backpressure
        | DispatchError::Oversized
        | DispatchError::Encrypt(_) => DispatchScope::Device,
    }
}

/// Encrypt and send one host-originated message to its device, recording
/// it in that device's buffer for the ack tripwire. A message whose
/// serialized form is too large for one Noise transport frame is split
/// into `MessageBody::Fragment`s, each riding its own seq.
fn dispatch_to_device(
    socket: &mut RelaySocket,
    device: &mut DeviceChannel,
    message: &SealedMessage,
) -> Result<(), DispatchFailure> {
    let seq = device.buffer.take_seq();
    // The authenticated delivery sequence: the payload repeats the frame's
    // relay-visible seq inside the integrity protection, so the peer can
    // tell a renumbered frame from an honest one. Stamped here, immediately
    // before encrypting, because the seq only exists once the frame does.
    let mut message = message.clone();
    message.transport_seq = seq;
    let plaintext = serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_owned());
    let failed = |error| DispatchFailure {
        error,
        message: message.clone(),
    };
    // Reserve room for the tag: snow counts `payload.len() + TAGLEN`, not
    // just the payload, against its limit.
    if plaintext.len() + 16 <= MAX_SEALED_PLAINTEXT {
        return send_sealed(socket, device, seq, &message, plaintext).map_err(failed);
    }

    // Fragment: the bytes are split raw, never re-encoded, so the receiver
    // concatenates `data` and parses the reassembled JSON once. Each
    // fragment is a complete sealed message with its own frame seq stamped
    // into its envelope; the logical message's seq rides in the fragment
    // body and is never a frame seq.
    let count = plaintext.len().div_ceil(FRAGMENT_CHUNK_BYTES);
    let chunks: Vec<&[u8]> = plaintext.as_bytes().chunks(FRAGMENT_CHUNK_BYTES).collect();
    debug_assert_eq!(chunks.len(), count);
    log::warn!(
        target: "audit",
        "sealed message too large for one frame; fragmenting device={} seq={seq} bytes={} of={count}",
        device.device_id,
        plaintext.len()
    );
    for (index, chunk) in chunks.into_iter().enumerate() {
        let fragment_seq = device.buffer.take_seq();
        let fragment = SealedMessage {
            protocol_version: message.protocol_version,
            transport_seq: fragment_seq,
            host_id: message.host_id,
            device_id: message.device_id,
            sent_at_ms: message.sent_at_ms,
            body: MessageBody::Fragment {
                transport_seq: seq,
                index: index as u32,
                of: count as u32,
                data: Base64Bytes(chunk.to_vec()),
            },
        };
        let fragment_json = serde_json::to_string(&fragment).unwrap_or_else(|_| "{}".to_owned());
        send_sealed(socket, device, fragment_seq, &fragment, fragment_json).map_err(|error| {
            DispatchFailure {
                error,
                message: message.clone(),
            }
        })?;
    }
    Ok(())
}

/// Serialize, encrypt, and send one whole sealed message under `seq`.
fn send_sealed(
    socket: &mut RelaySocket,
    device: &mut DeviceChannel,
    seq: u64,
    message: &SealedMessage,
    plaintext: String,
) -> Result<(), DispatchError> {
    let ciphertext = device
        .channel
        .encrypt(plaintext.as_bytes())
        .map_err(DispatchError::Encrypt)?;
    let frame = Frame::Sealed {
        seq,
        host_id: message.host_id,
        device_id: Some(message.device_id),
        ciphertext: Base64Bytes(ciphertext),
    };
    let encoded = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_owned());
    let encoded_len = encoded.len();
    // The wire limit, ciphertext frame included. The plaintext limit is
    // enforced by the fragmenter; this is the network-side ceiling.
    if encoded_len > MAX_FRAME_BYTES {
        return Err(DispatchError::Oversized);
    }
    // Backpressure before the wire. A full buffer means the device stopped
    // acknowledging end-to-end; pushing more ciphertext would put frames
    // on the socket that no ack tripwire covers. Refuse and let this
    // device's link rebuild — it resynchronizes from a snapshot, and every
    // operation result stays recoverable through QueryOperationResult.
    if !device.buffer.has_room(encoded_len) {
        return Err(DispatchError::Backpressure);
    }
    socket
        .send(Message::text(encoded))
        .map_err(DispatchError::Transport)?;
    if device.buffer.mark_sent(seq, frame, encoded_len).is_err() {
        // Unreachable after the has_room check above; kept so a future
        // accounting change fails loudly instead of silently untracked.
        log::warn!(target: "audit", "outbound buffer refused a frame it had room for; seq={seq}");
    }
    Ok(())
}

fn send_frame(socket: &mut RelaySocket, frame: Frame) -> Result<(), tungstenite::Error> {
    let encoded = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_owned());
    // Same wire ceiling as the sealed path, applied to the handshake and
    // control frames too: the limit is a property of the link, not of one
    // frame kind.
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(tungstenite::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds the wire limit",
        )));
    }
    socket.send(Message::text(encoded))
}

/// Tell one device its queued prompt was dropped (revoked, or its target
/// disappeared). The item never ran and never will.
fn report_dropped_queue_item(
    state: &HostState,
    item: &crate::queue::QueuedPrompt,
    reason: &str,
) {
    state.send_to_device(
        MessageBody::OperationResult(fintwind_remote_protocol::OperationOutcome {
            operation_id: item.operation_id,
            state: fintwind_remote_protocol::OperationState::Rejected {
                code: fintwind_remote_protocol::RejectCode::AuthFailed,
            },
            detail: Some(serde_json::json!({ "message": reason }).to_string()),
        }),
        item.device_id,
        crate::pending::PendingKind::Result,
    );
}

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

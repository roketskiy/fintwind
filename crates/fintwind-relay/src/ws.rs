//! One authenticated WebSocket session: identity admission, per-connection
//! rate limiting, ciphertext routing, and pairing-frame relay.
//!
//! Routing authorization is per message, not per connection: a device may
//! address only hosts owned by its own account, and a host may address
//! only devices of its own account. The connect-time admission is not
//! trusted to cover this — every frame re-checks its target against the
//! store (architecture: 统一路由层逐消息检查). A store error fails the
//! check closed: routing proceeds only on a positive answer.
//!
//! The loop is single-threaded with a short read timeout: it drains the
//! socket when frames arrive and the outbound queue when they do not. The
//! relay's phase-one traffic is tiny; correctness beats a split reader.

use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use uuid::Uuid;

use tungstenite::{Message, WebSocket};

use fintwind_remote_protocol::{
    ControlFrame, Frame, FrameRole, HandshakeMode, RejectCode, REMOTE_PROTOCOL_VERSION,
};

use crate::config::{AUTH_TIMEOUT, FRAME_RATE_LIMIT, FRAME_RATE_WINDOW, IDLE_TIMEOUT};
use crate::context::RelayContext;
use crate::ratelimit::RateLimiter;
use crate::registry::{ConnectionRole, RouteOutcome};
use crate::rest::ChallengeRole;
use crate::store::NonceOutcome;
use fintwind_remote_crypto::SigningKey;

/// Between outbound keepalives while the connection is quiet.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

pub fn run_session(mut websocket: WebSocket<TcpStream>, context: Arc<RelayContext>) {
    // Admission window: one Auth frame, nothing else, hard overall limit.
    let _ = websocket.get_ref().set_read_timeout(Some(AUTH_TIMEOUT));
    let first = match read_frame(&mut websocket, Instant::now() + AUTH_TIMEOUT) {
        Some(frame) => frame,
        None => return,
    };
    let role = match admit(&context, first) {
        Ok(role) => role,
        Err(code) => {
            let _ = send(
                &mut websocket,
                rejection(code, "authentication failed"),
            );
            log::warn!(target: "audit", "connection rejected; code={code:?}");
            return;
        }
    };

    // Register; a same-entity reconnect displaced an older session and
    // that session is told to leave.
    let (outbound_tx, outbound_rx) = crossbeam_channel::unbounded::<Frame>();
    let (connection_id, displaced) = context.registry.register(role.clone(), outbound_tx);
    if let Some(displaced) = displaced {
        let _ = displaced.outbound.send(rejection(
            RejectCode::AuthFailed,
            "replaced by a new connection",
        ));
    }

    // A device's queued ciphertext goes out before anything live, in
    // order, so snapshot/live traffic cannot overtake it.
    if let ConnectionRole::Device { device_id } = &role {
        for queued in context.registry.drain_queue(device_id) {
            let _ = send(&mut websocket, queued.frame);
        }
    }
    log::info!(target: "audit", "session established; subject={} id={connection_id}", role.subject_id());

    // Connection-level ack: tells the endpoint its identity was accepted
    // before any business frame moves. Sent here, on the session thread,
    // so it strictly precedes everything this connection relays.
    let _ = send(
        &mut websocket,
        Frame::Control {
            frame: ControlFrame::RelayAck { acked_seq: 0 },
        },
    );

    let _ = websocket
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(250)));
    let rate_limiter = RateLimiter::new(FRAME_RATE_LIMIT, FRAME_RATE_WINDOW);
    let mut last_frame = Instant::now();
    let mut last_keepalive = Instant::now();

    loop {
        if context.shutdown.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        // Outbound first: displaced sessions must see their rejection even
        // if no inbound frame ever arrives again.
        let mut outgoing_closed = false;
        while let Ok(frame) = outbound_rx.try_recv() {
            if send(&mut websocket, frame).is_err() {
                outgoing_closed = true;
            }
        }
        if outgoing_closed {
            break;
        }

        match websocket.read() {
            Ok(Message::Text(text)) => {
                last_frame = Instant::now();
                let parsed: Result<Frame, _> = serde_json::from_str(&text);
                let frame = match parsed {
                    Ok(frame) => frame,
                    Err(_) => {
                        let _ = send(
                            &mut websocket,
                            rejection(RejectCode::Malformed, "unparsable frame"),
                        );
                        continue;
                    }
                };
                if !rate_limiter.admit("conn") {
                    let _ = send(
                        &mut websocket,
                        rejection(RejectCode::RateLimited, "frame rate exceeded"),
                    );
                    continue;
                }
                match route_frame(&context, &role, frame) {
                    RouteReply::RelayAck(acked_seq) => {
                        let _ = send(
                            &mut websocket,
                            Frame::Control {
                                frame: ControlFrame::RelayAck { acked_seq },
                            },
                        );
                    }
                    RouteReply::RelayPong(sent_at_ms) => {
                        let _ = send(
                            &mut websocket,
                            Frame::Control {
                                frame: ControlFrame::Pong { sent_at_ms },
                            },
                        );
                    }
                    RouteReply::RelayControl(control) => {
                        let _ = send(&mut websocket, Frame::Control { frame: control });
                    }
                    RouteReply::Refused(code, message) => {
                        log::info!(target: "audit", "frame refused; subject={} code={code:?} message={message}", role.subject_id());
                        let _ = send(&mut websocket, rejection(code, &message));
                    }
                    RouteReply::Forwarded => {}
                }
            }
            Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => break,
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                if last_frame.elapsed() > IDLE_TIMEOUT {
                    log::info!(target: "audit", "session idle timeout; id={connection_id}");
                    break;
                }
                if last_keepalive.elapsed() > KEEPALIVE_INTERVAL {
                    last_keepalive = Instant::now();
                    let _ = send(
                        &mut websocket,
                        Frame::Control {
                            frame: ControlFrame::Ping {
                                sent_at_ms: now_ms(),
                            },
                        },
                    );
                }
            }
            Err(_) => break,
            Ok(_) => {
                // Binary frames: tungstenite answers protocol-level pings
                // itself; anything else is refused.
                let _ = send(
                    &mut websocket,
                    rejection(RejectCode::Malformed, "unsupported message"),
                );
            }
        }
    }
    context.registry.unregister(connection_id);
}

enum RouteReply {
    /// The relay accepted the frame; this is its delivery sequence.
    RelayAck(u64),
    /// The relay answers the endpoint's ping directly.
    RelayPong(u64),
    /// A relay-side control answer to send back verbatim (link results).
    RelayControl(ControlFrame),
    /// The relay refused the frame with a reason.
    Refused(RejectCode, String),
    /// Routed onward with no reply owed to this side.
    Forwarded,
}

fn refused(code: RejectCode, message: &str) -> RouteReply {
    RouteReply::Refused(code, message.to_owned())
}

/// The per-message routing decision: version, target membership, delivery.
fn route_frame(context: &RelayContext, role: &ConnectionRole, frame: Frame) -> RouteReply {
    match frame {
        Frame::Auth { .. } => refused(RejectCode::Malformed, "already authenticated"),
        Frame::Control { frame: control } => match control {
            // The relay answers endpoint pings itself. Endpoint control
            // semantics other than ping/pong do not exist: relay acks flow
            // from the relay only, and everything business rides sealed.
            ControlFrame::Ping { sent_at_ms } => RouteReply::RelayPong(sent_at_ms),
            ControlFrame::Pong { .. } => RouteReply::Forwarded,
            _ => refused(RejectCode::Malformed, "unexpected control frame"),
        },
        Frame::Sealed {
            seq,
            host_id,
            device_id,
            ciphertext,
        } => {
            let host_id = host_id.to_string();
            let device_id = device_id.map(|id| id.to_string());
            match role {
                ConnectionRole::Host { host_id: self_host } => {
                    if host_id != *self_host {
                        return refused(
                            RejectCode::AuthFailed,
                            "sealed frame claims another host identity",
                        );
                    }
                    let Some(device_id) = device_id else {
                        return refused(RejectCode::Malformed, "missing device target");
                    };
                    if !device_belongs(context, self_host, &device_id) {
                        // A pairing session in flight authorizes sealed
                        // traffic to exactly that device: the pairing
                        // decision must reach it before any link exists.
                        // The mapping dies with either connection, so the
                        // exemption cannot outlive the session.
                        let peer = context.registry.pairing_peer(self_host);
                        if peer.as_deref() != Some(device_id.as_str()) {
                            return refused(
                                RejectCode::UnknownRoute,
                                "device is not in this host's account",
                            );
                        }
                    }
                    let ciphertext_len = ciphertext.0.len();
                    let forward = Frame::Sealed {
                        seq,
                        host_id: Uuid::parse_str(&host_id).unwrap_or_default(),
                        device_id: Some(Uuid::parse_str(&device_id).unwrap_or_default()),
                        ciphertext,
                    };
                    let outcome =
                        context.registry.route_to_device(&device_id, self_host, seq, forward, ciphertext_len);
                    match outcome {
                        RouteOutcome::Delivered | RouteOutcome::Queued => RouteReply::RelayAck(seq),
                        RouteOutcome::QueueFull => {
                            refused(RejectCode::QueueFull, "device queue is full")
                        }
                        RouteOutcome::NoRoute => {
                            // The registered connection vanished between
                            // checks; the frame is gone, say so.
                            refused(RejectCode::UnknownRoute, "device not connected")
                        }
                    }
                }
                ConnectionRole::Device { device_id: self_device } => {
                    // The frame's host_id is the addressed host; membership
                    // in the device's account makes the target legal.
                    if !device_belongs(context, &host_id, self_device) {
                        return refused(
                            RejectCode::UnknownRoute,
                            "host is not in this device's account",
                        );
                    }
                    let forward = Frame::Sealed {
                        seq,
                        host_id: Uuid::parse_str(&host_id).unwrap_or_default(),
                        device_id: Some(Uuid::parse_str(self_device).unwrap_or_default()),
                        ciphertext,
                    };
                    match context.registry.route_to_host(&host_id, forward) {
                        RouteOutcome::Delivered => RouteReply::RelayAck(seq),
                        RouteOutcome::NoRoute => {
                            refused(RejectCode::UnknownRoute, "host not connected")
                        }
                        RouteOutcome::Queued | RouteOutcome::QueueFull => {
                            refused(RejectCode::QueueFull, "hosts are never queued")
                        }
                    }
                }
            }
        }
        Frame::Handshake {
            protocol_version,
            role: frame_role,
            host_id,
            device_id,
            mode,
            step,
            payload,
        } => {
            if protocol_version != REMOTE_PROTOCOL_VERSION {
                return refused(RejectCode::VersionMismatch, "protocol version");
            }
            let host_id = host_id.to_string();
            let device_id = device_id.map(|id| id.to_string());
            // Connect handshakes prove account membership before any key
            // material moves. Pairing is exactly the flow that establishes
            // membership, so it routes on presence alone: the invite
            // nonce, folded into the Noise prologue, is the real gate and
            // the host consumes it once.
            if mode == HandshakeMode::Connect {
                match &device_id {
                    Some(device_id) => {
                        if !device_belongs(context, &host_id, device_id) {
                            return refused(
                                RejectCode::AuthFailed,
                                "pairing pair does not share an account",
                            );
                        }
                    }
                    None => {
                        return refused(
                            RejectCode::Malformed,
                            "connect handshake must name the device",
                        );
                    }
                }
            }
            let payload_len = payload.0.len();
            let forward = Frame::Handshake {
                protocol_version,
                role: frame_role,
                host_id: Uuid::parse_str(&host_id).unwrap_or_default(),
                device_id: device_id
                    .as_deref()
                    .and_then(|id| Uuid::parse_str(id).ok()),
                mode,
                step,
                payload,
            };
            // Device-initiated handshakes go to the host; the host's
            // pairing reply goes back to whichever device opened the
            // pairing, and connect replies name their device outright.
            // Queued delivery is fine for the device direction and a miss
            // for the host one.
            let outcome = match (&device_id, &mode, role) {
                (Some(device_id), _, ConnectionRole::Host { .. }) => context
                    .registry
                    .route_to_device(device_id, &host_id, 0, forward, payload_len),
                (_, HandshakeMode::Connect, ConnectionRole::Device { .. }) => {
                    context.registry.route_to_host(&host_id, forward)
                }
                (None, HandshakeMode::Pairing, ConnectionRole::Device { device_id }) => {
                    log::info!(target: "audit", "pairing handshake routed to host; host={host_id} device={device_id}");
                    context.registry.note_pairing(&host_id, device_id);
                    context.registry.route_to_host(&host_id, forward)
                }
                (None, HandshakeMode::Pairing, ConnectionRole::Host { .. }) => {
                    return match context.registry.pairing_peer(&host_id) {
                        Some(device_id) => {
                            let outcome = context.registry.route_to_device(
                                &device_id,
                                &host_id,
                                0,
                                forward,
                                payload_len,
                            );
                            match outcome {
                                RouteOutcome::Delivered | RouteOutcome::Queued => {
                                    RouteReply::Forwarded
                                }
                                RouteOutcome::QueueFull => {
                                    refused(RejectCode::QueueFull, "device queue is full")
                                }
                                RouteOutcome::NoRoute => {
                                    refused(RejectCode::UnknownRoute, "peer not connected")
                                }
                            }
                        }
                        None => refused(RejectCode::UnknownRoute, "no pairing attempt in progress"),
                    };
                }
                _ => return refused(RejectCode::Malformed, "handshake does not name a device"),
            };
            match outcome {
                RouteOutcome::Delivered | RouteOutcome::Queued => RouteReply::Forwarded,
                RouteOutcome::QueueFull => refused(RejectCode::QueueFull, "device queue is full"),
                RouteOutcome::NoRoute => refused(RejectCode::UnknownRoute, "peer not connected"),
            }
        }
        Frame::LinkDevice {
            host_id,
            device_id,
            device_public_key,
            nonce,
            signature,
        } => handle_link(
            context,
            role,
            LinkMessage {
                host_id: host_id.to_string(),
                device_id: device_id.to_string(),
                device_public_key: device_public_key.0,
                nonce: nonce.0,
                signature: signature.0,
                unlink: false,
            },
        ),
        Frame::UnlinkDevice {
            host_id,
            device_id,
            nonce,
            signature,
        } => handle_link(
            context,
            role,
            LinkMessage {
                host_id: host_id.to_string(),
                device_id: device_id.to_string(),
                device_public_key: Vec::new(),
                nonce: nonce.0,
                signature: signature.0,
                unlink: true,
            },
        ),
    }
}

/// One `LinkDevice`/`UnlinkDevice` declaration, normalized for checking.
struct LinkMessage {
    host_id: String,
    device_id: String,
    device_public_key: Vec<u8>,
    nonce: Vec<u8>,
    signature: Vec<u8>,
    unlink: bool,
}

/// Validate and apply a host's link declaration. Order matters: role and
/// identity first, then the signature over the exact declaration, then
/// the one-time nonce, and only after all of that does the store change.
fn handle_link(context: &RelayContext, role: &ConnectionRole, message: LinkMessage) -> RouteReply {
    use fintwind_remote_crypto::SigningKey as Signing;

    let ConnectionRole::Host { host_id: self_host } = role else {
        return refused(RejectCode::AuthFailed, "only hosts may declare links");
    };
    if message.host_id != *self_host {
        return refused(RejectCode::AuthFailed, "link claims another host identity");
    }
    let Ok(device_uuid) = Uuid::parse_str(&message.device_id) else {
        return refused(RejectCode::Malformed, "device id is not a uuid");
    };

    // Signature over the exact declaration bytes with the host's
    // registered signing key: nothing a device sends can create a link.
    // Identity fields sign as raw 16-byte UUIDs, matching the host side.
    let (host_uuid, device_uuid) = match (
        Uuid::parse_str(&message.host_id),
        Uuid::parse_str(&message.device_id),
    ) {
        (Ok(host_uuid), Ok(device_uuid)) => (host_uuid, device_uuid),
        _ => return refused(RejectCode::Malformed, "ids are not uuids"),
    };
    let mut signed = host_uuid.as_bytes().to_vec();
    signed.extend_from_slice(device_uuid.as_bytes());
    if !message.unlink {
        signed.extend_from_slice(&message.device_public_key);
    }
    signed.extend_from_slice(&message.nonce);
    let Ok(Some(host_key)) = context.store.host_signing_key(&message.host_id) else {
        return refused(RejectCode::AuthFailed, "unknown host");
    };
    if !Signing::verify(&host_key, &signed, &message.signature) {
        return refused(RejectCode::AuthFailed, "link signature invalid");
    }

    // The nonce ties this declaration to one displayed invite. Exactly
    // one declaration ever consumes it.
    let nonce_text = BASE64_STANDARD.encode(&message.nonce);
    match context
        .store
        .consume_pairing_nonce(&message.host_id, &nonce_text)
    {
        Ok(NonceOutcome::Consumed) => {}
        Ok(NonceOutcome::Expired) => {
            return refused(RejectCode::AuthFailed, "pairing invite expired");
        }
        Ok(NonceOutcome::Unknown) => {
            return refused(RejectCode::AuthFailed, "pairing nonce unknown or spent");
        }
        Err(error) => {
            log::warn!(target: "audit", "nonce check failed: {error}");
            return refused(RejectCode::Malformed, "nonce check failed");
        }
    }

    // The declared device must exist at the relay; the routing id comes
    // from its own registration, never from the frame.
    match context.store.device_signing_key(&message.device_id) {
        Ok(Some(_)) => {}
        _ => return refused(RejectCode::UnknownRoute, "device is not registered"),
    }

    let result = if message.unlink {
        context
            .store
            .unlink_device(&message.host_id, &message.device_id)
            .map(|_| ControlFrame::Unlinked { device_id: device_uuid })
    } else {
        context
            .store
            .link_device(&message.host_id, &message.device_id)
            .map(|_| ControlFrame::Linked { device_id: device_uuid })
    };
    match result {
        Ok(control) => {
            log::info!(
                target: "audit",
                "{}; host={} device={}",
                if message.unlink { "device unlinked" } else { "device linked" },
                message.host_id,
                message.device_id
            );
            RouteReply::RelayControl(control)
        }
        Err(error) => {
            log::warn!(target: "audit", "link write failed: {error}");
            refused(RejectCode::Malformed, "link write failed")
        }
    }
}

/// Membership check that fails closed: a store error refuses the route.
/// "Paired" replaced the account model — only hosts that confirmed a
/// device accept its traffic (decision revision, M0 freeze document).
fn device_belongs(context: &RelayContext, host_id: &str, device_id: &str) -> bool {
    context
        .store
        .device_paired_with_host(host_id, device_id)
        .unwrap_or(false)
}

/// Admission: the first frame must authenticate the connection and fix
/// its role. Identity is a key pair — the frame carries a one-time relay
/// challenge plus its Ed25519 signature; the relay consumes the challenge
/// and verifies against the registered public key. No bearer credential
/// exists anywhere to steal or replay.
fn admit(context: &RelayContext, frame: Frame) -> Result<ConnectionRole, RejectCode> {
    let Frame::Auth {
        protocol_version,
        role,
        host_id,
        device_id,
        challenge,
        signature,
    } = frame
    else {
        return Err(RejectCode::Malformed);
    };
    if protocol_version != REMOTE_PROTOCOL_VERSION {
        return Err(RejectCode::VersionMismatch);
    }
    match (role, host_id, device_id) {
        (FrameRole::Host, Some(host_id), None) => {
            let host_id = host_id.to_string();
            if !context.auth_limiter.admit(&host_id) {
                return Err(RejectCode::RateLimited);
            }
            if !context.challenges.consume(&challenge.0, ChallengeRole::Host) {
                return Err(RejectCode::AuthFailed);
            }
            let registered = context
                .store
                .host_signing_key(&host_id)
                .unwrap_or(None);
            match registered {
                Some(public_key)
                    if SigningKey::verify(&public_key, &challenge.0, &signature.0) =>
                {
                    Ok(ConnectionRole::Host { host_id })
                }
                _ => Err(RejectCode::AuthFailed),
            }
        }
        (FrameRole::Device, None, Some(device_id)) => {
            let device_id = device_id.to_string();
            if !context.auth_limiter.admit(&device_id) {
                return Err(RejectCode::RateLimited);
            }
            if !context.challenges.consume(&challenge.0, ChallengeRole::Device) {
                return Err(RejectCode::AuthFailed);
            }
            let registered = context
                .store
                .device_signing_key(&device_id)
                .unwrap_or(None);
            match registered {
                Some(public_key)
                    if SigningKey::verify(&public_key, &challenge.0, &signature.0) =>
                {
                    Ok(ConnectionRole::Device { device_id })
                }
                _ => Err(RejectCode::AuthFailed),
            }
        }
        _ => Err(RejectCode::Malformed),
    }
}

fn rejection(code: RejectCode, message: &str) -> Frame {
    Frame::Control {
        frame: ControlFrame::Rejected {
            code,
            message: message.to_owned(),
        },
    }
}

fn send(websocket: &mut WebSocket<TcpStream>, frame: Frame) -> Result<(), tungstenite::Error> {
    let text = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_owned());
    websocket.send(Message::text(text))
}

fn read_frame(websocket: &mut WebSocket<TcpStream>, deadline: Instant) -> Option<Frame> {
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        match websocket.read() {
            Ok(Message::Text(text)) => {
                return serde_json::from_str(&text).ok();
            }
            Ok(Message::Close(_)) => return None,
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Still inside the admission window; keep waiting for the
                // Auth frame until the deadline ends the session.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keepalive_and_idle_windows_are_ordered() {
        assert!(KEEPALIVE_INTERVAL < IDLE_TIMEOUT);
        assert!(AUTH_TIMEOUT <= IDLE_TIMEOUT);
    }
}

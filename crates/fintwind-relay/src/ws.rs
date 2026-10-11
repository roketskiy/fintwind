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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use parking_lot::Mutex;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

use tungstenite::{Message, WebSocket};

use fintwind_remote_protocol::{
    ConnectionPurpose, ControlFrame, Frame, FrameRole, HandshakeMode, MAX_FRAME_BYTES,
    REMOTE_PROTOCOL_VERSION, RejectCode, RejectedFrame, RejectedFrameReason,
};

use crate::config::{FRAME_RATE_LIMIT, FRAME_RATE_WINDOW, IDLE_TIMEOUT, WRITE_TIMEOUT};
use crate::context::RelayContext;
use crate::http::AdmissionGuard;
use crate::ratelimit::RateLimiter;
use crate::registry::{ConnectionRole, PairingNote, RouteOutcome, encoded_len_hint};
use crate::rest::ChallengeRole;
use crate::store::NonceOutcome;
use fintwind_remote_crypto::SigningKey;

/// Between outbound keepalives while the connection is quiet.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// A TcpStream whose reads obey an absolute deadline. Tungstenite's
/// `read()` issues several blocking socket reads internally — one per
/// fragment a trickling peer delivers — and every success would restart
/// a timeout set outside it, so a per-read bound never becomes a total
/// one. Re-deriving the remaining time inside every `read` makes the
/// admission deadline hold at the Read boundary: once it has passed,
/// reads report a timeout no matter how eagerly the peer drips bytes.
pub(crate) struct DeadlineStream {
    stream: TcpStream,
    deadline: Arc<Mutex<Option<Instant>>>,
}

/// Shared handle that ends the admission enforcement once the
/// connection has authenticated; the session loop's 250 ms poll takes
/// over from there. One `Arc<Mutex<Option<Instant>>>` per connection,
/// no threads, no new dependencies.
#[derive(Clone)]
pub(crate) struct DeadlineHandle(Arc<Mutex<Option<Instant>>>);

impl DeadlineHandle {
    fn clear(&self) {
        *self.0.lock() = None;
    }
}

/// Splits the socket into the deadline-guarded stream tungstenite will
/// own, and the handle that lifts the guard after authentication.
pub(crate) fn deadline_stream(
    stream: TcpStream,
    deadline: Instant,
) -> (DeadlineStream, DeadlineHandle) {
    let shared = Arc::new(Mutex::new(Some(deadline)));
    (
        DeadlineStream {
            stream,
            deadline: Arc::clone(&shared),
        },
        DeadlineHandle(shared),
    )
}

impl DeadlineStream {
    /// The raw socket, for timeout plumbing and registry cloning.
    fn socket(&self) -> &TcpStream {
        &self.stream
    }
}

impl std::io::Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(deadline) = *self.deadline.lock() {
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "admission deadline passed",
                ));
            }
            // Windows rounds SO_RCVTIMEO down to whole milliseconds and
            // treats zero as "block forever"; keep at least 1 ms so the
            // bound can never regress to an unbounded wait.
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            let _ = self.stream.set_read_timeout(Some(remaining));
        }
        self.stream.read(buf)
    }
}

impl std::io::Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

pub fn run_session(
    mut websocket: WebSocket<DeadlineStream>,
    context: Arc<RelayContext>,
    admission: AdmissionGuard,
    admission_deadline: Instant,
    deadline: DeadlineHandle,
) {
    // A peer that stops reading must not hold this slot forever.
    let _ = websocket
        .get_ref()
        .socket()
        .set_write_timeout(Some(WRITE_TIMEOUT));
    // Admission window: one Auth frame, nothing else, hard overall limit.
    let first = match read_frame(&mut websocket, admission_deadline) {
        Some(frame) => frame,
        None => {
            // The window expired (or the peer vanished) before it
            // identified itself.
            context.metrics.timeout();
            return;
        }
    };
    let role = match admit(&context, first) {
        Ok(role) => role,
        Err(code) => {
            context.metrics.auth_rejected();
            let _ = send(&mut websocket, rejection(code, "authentication failed"));
            log::warn!(target: "audit", "connection rejected; code={code:?}");
            return;
        }
    };

    // Register; a same-entity reconnect displaced an older session and
    // that session is told to leave. From here the registry's
    // authenticated-connection cap governs, so the unauthenticated
    // admission slot is released exactly once.
    let (outbound_tx, outbound_rx) = crossbeam_channel::unbounded::<Frame>();
    let (connection_id, displaced) = match context.registry.register(role.clone(), outbound_tx) {
        Ok(registered) => registered,
        Err(_) => {
            context.metrics.rejection(RejectCode::RateLimited);
            let _ = send(
                &mut websocket,
                rejection(RejectCode::RateLimited, "too many connections"),
            );
            log::warn!(target: "audit", "connection refused at capacity; subject={}", role.subject_id());
            return;
        }
    };
    match websocket.get_ref().socket().try_clone() {
        Ok(socket) => context.registry.bind_socket(connection_id, socket),
        Err(_) => {
            context.registry.unregister(connection_id);
            return;
        }
    }
    admission.release();
    context.metrics.auth_accepted();
    // Authenticated: the admission deadline's job is done. Lift the
    // enforcement at the Read boundary — the session loop's 250 ms poll
    // and IDLE_TIMEOUT govern from here.
    deadline.clear();
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
        .socket()
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
            let connection_rejected = matches!(
                &frame,
                Frame::Control {
                    frame: ControlFrame::Rejected { .. }
                }
            );
            let frame_bytes = encoded_len_hint(&frame);
            if send(&mut websocket, frame).is_err() {
                // The peer stopped reading for good: a stuck write is a
                // dead link, and holding the slot helps nobody.
                log::info!(target: "audit", "link ended (write failed); id={connection_id}");
                outgoing_closed = true;
            } else {
                // The frame is on the wire: its buffer accounting is free
                // again, so a momentarily slow peer is not punished twice.
                context
                    .registry
                    .release_outbound(connection_id, frame_bytes);
            }
            if connection_rejected {
                outgoing_closed = true;
            }
            if outgoing_closed {
                break;
            }
        }
        if outgoing_closed {
            context.metrics.timeout();
            break;
        }

        match websocket.read() {
            Ok(Message::Text(text)) => {
                last_frame = Instant::now();
                // The WS layer already refuses frames past the cap; this
                // is the endpoint re-enforcing it before parsing, as the
                // frame contract requires.
                if text.len() > MAX_FRAME_BYTES {
                    let _ = send(
                        &mut websocket,
                        rejection(RejectCode::Malformed, "frame too large"),
                    );
                    continue;
                }
                // Rate limiting before parsing: an unparsable flood must
                // not be free.
                if !rate_limiter.admit("conn") {
                    context.metrics.rejection(RejectCode::RateLimited);
                    let _ = send(
                        &mut websocket,
                        rejection(RejectCode::RateLimited, "frame rate exceeded"),
                    );
                    continue;
                }
                context.metrics.frame_routed();
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
                    RouteReply::Refused(code, message, reference) => {
                        context.metrics.rejection(code);
                        log::info!(target: "audit", "frame refused; subject={} code={code:?} message={message}", role.subject_id());
                        let answer = match reference {
                            Some(reference) => rejection_frame(code, &message, reference),
                            None => rejection(code, &message),
                        };
                        let _ = send(&mut websocket, answer);
                    }
                    RouteReply::Forwarded => {}
                }
            }
            Ok(Message::Close(_)) => {
                log::info!(target: "audit", "link ended (peer close); subject={} id={connection_id}", role.subject_id());
                break;
            }
            Err(tungstenite::Error::ConnectionClosed) => {
                log::info!(target: "audit", "link ended (connection closed); subject={} id={connection_id}", role.subject_id());
                break;
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                if last_frame.elapsed() > IDLE_TIMEOUT {
                    log::info!(target: "audit", "session idle timeout; id={connection_id}");
                    context.metrics.timeout();
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
            Err(_) => {
                log::info!(target: "audit", "link ended (socket error); subject={} id={connection_id}", role.subject_id());
                break;
            }
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
    /// The relay refused the frame with a reason. The optional structured
    /// reference names the frame that was refused — host, device and the
    /// relay-visible sequence — so the sender can correlate the refusal
    /// without parsing the message, which is for logs only.
    Refused(RejectCode, String, Option<RejectedFrame>),
    /// Routed onward with no reply owed to this side.
    Forwarded,
}

fn refused(code: RejectCode, message: &str) -> RouteReply {
    RouteReply::Refused(code, message.to_owned(), None)
}

/// A refusal that can name the frame it refused. Only used where the
/// relay actually identified one: an unparsable frame has no reference
/// to give, and inventing one would be worse than omitting it.
fn refused_frame(code: RejectCode, message: &str, reference: Option<RejectedFrame>) -> RouteReply {
    RouteReply::Refused(code, message.to_owned(), reference)
}

/// The relay-visible reference for one frame the relay identified, used
/// when refusing it. The ciphertext is not part of it.
/// The relay-visible reference for one frame the relay identified, used
/// when refusing it. The ciphertext is not part of it. Returns `None`
/// when the ids are not uuids — a refusal without a reference is honest,
/// a wrong one is not.
fn reference(
    host_id: &str,
    device_id: Option<&str>,
    seq: u64,
    reason: RejectedFrameReason,
) -> Option<RejectedFrame> {
    Some(RejectedFrame {
        host_id: Uuid::parse_str(host_id).ok()?,
        device_id: device_id.and_then(|id| Uuid::parse_str(id).ok()),
        seq,
        reason,
    })
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
                ConnectionRole::Host { host_id: self_host }
                | ConnectionRole::HostPairing { host_id: self_host } => {
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
                            return refused_frame(
                                RejectCode::UnknownRoute,
                                "device is not in this host's account",
                                reference(
                                    self_host,
                                    Some(&device_id),
                                    seq,
                                    RejectedFrameReason::Unlinked,
                                ),
                            );
                        }
                    }
                    let forward = Frame::Sealed {
                        seq,
                        host_id: Uuid::parse_str(&host_id).unwrap_or_default(),
                        device_id: Some(Uuid::parse_str(&device_id).unwrap_or_default()),
                        ciphertext,
                    };
                    let outcome = context
                        .registry
                        .route_to_device(&device_id, self_host, seq, forward);
                    match outcome {
                        RouteOutcome::Delivered | RouteOutcome::Queued => RouteReply::RelayAck(seq),
                        RouteOutcome::QueueFull => refused_frame(
                            RejectCode::QueueFull,
                            &format!("device queue is full; seq={seq}"),
                            reference(
                                self_host,
                                Some(&device_id),
                                seq,
                                RejectedFrameReason::QueueFull,
                            ),
                        ),
                        RouteOutcome::NoRoute => {
                            // The registered connection vanished between
                            // checks; the frame is gone, say so.
                            refused_frame(
                                RejectCode::UnknownRoute,
                                &format!("device not connected; seq={seq}"),
                                reference(
                                    self_host,
                                    Some(&device_id),
                                    seq,
                                    RejectedFrameReason::EndpointOffline,
                                ),
                            )
                        }
                    }
                }
                ConnectionRole::Device {
                    device_id: self_device,
                } => {
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
                        RouteOutcome::NoRoute => refused_frame(
                            RejectCode::UnknownRoute,
                            "host not connected",
                            reference(
                                &host_id,
                                Some(self_device),
                                seq,
                                RejectedFrameReason::EndpointOffline,
                            ),
                        ),
                        RouteOutcome::Queued | RouteOutcome::QueueFull => refused_frame(
                            RejectCode::QueueFull,
                            "hosts are never queued",
                            reference(
                                &host_id,
                                Some(self_device),
                                seq,
                                RejectedFrameReason::QueueFull,
                            ),
                        ),
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
                match (&device_id, role) {
                    (
                        Some(device_id),
                        ConnectionRole::Device {
                            device_id: self_device,
                        },
                    ) => {
                        // The device that opens a business session must be
                        // the one this connection authenticated as. A frame
                        // naming another device of the same account is a
                        // routing attempt against someone else's channel:
                        // the relay is not the authority on that pairing,
                        // and the Noise handshake would only fail later,
                        // after the frame had already been delivered.
                        if device_id != self_device {
                            return refused_frame(
                                RejectCode::AuthFailed,
                                "connect handshake names another device",
                                reference(
                                    &host_id,
                                    Some(self_device),
                                    0,
                                    RejectedFrameReason::Other,
                                ),
                            );
                        }
                        if !device_belongs(context, &host_id, device_id) {
                            return refused_frame(
                                RejectCode::AuthFailed,
                                "pairing pair does not share an account",
                                reference(
                                    &host_id,
                                    Some(self_device),
                                    0,
                                    RejectedFrameReason::Unlinked,
                                ),
                            );
                        }
                    }
                    (Some(device_id), _) => {
                        if !device_belongs(context, &host_id, device_id) {
                            return refused(
                                RejectCode::AuthFailed,
                                "pairing pair does not share an account",
                            );
                        }
                    }
                    (None, _) => {
                        return refused(
                            RejectCode::Malformed,
                            "connect handshake must name the device",
                        );
                    }
                }
            }
            let forward = Frame::Handshake {
                protocol_version,
                role: frame_role,
                host_id: Uuid::parse_str(&host_id).unwrap_or_default(),
                device_id: device_id.as_deref().and_then(|id| Uuid::parse_str(id).ok()),
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
                (Some(device_id), _, ConnectionRole::Host { .. })
                | (Some(device_id), _, ConnectionRole::HostPairing { .. }) => {
                    // A host hands frames only to a device it paired
                    // with, or to the device currently pairing with it —
                    // the same authorization the sealed path enforces, so
                    // an unpaired device is never a route target and
                    // never a queue target either.
                    if !device_belongs(context, &host_id, device_id)
                        && context.registry.pairing_peer(&host_id).as_deref()
                            != Some(device_id.as_str())
                    {
                        return refused(
                            RejectCode::UnknownRoute,
                            "device is not in this host's account",
                        );
                    }
                    context
                        .registry
                        .route_to_device(device_id, &host_id, 0, forward)
                }
                (_, HandshakeMode::Connect, ConnectionRole::Device { device_id }) => {
                    // Business (re)connects address the session slot. The
                    // device it names is this connection's own device —
                    // checked above — so the reference is exact.
                    return match context.registry.route_to_host(&host_id, forward) {
                        RouteOutcome::Delivered => RouteReply::Forwarded,
                        RouteOutcome::NoRoute => refused_frame(
                            RejectCode::UnknownRoute,
                            "host not connected",
                            reference(
                                &host_id,
                                Some(device_id),
                                0,
                                RejectedFrameReason::EndpointOffline,
                            ),
                        ),
                        RouteOutcome::Queued | RouteOutcome::QueueFull => refused_frame(
                            RejectCode::QueueFull,
                            "hosts are never queued",
                            reference(&host_id, Some(device_id), 0, RejectedFrameReason::QueueFull),
                        ),
                    };
                }
                (None, HandshakeMode::Pairing, ConnectionRole::Device { device_id }) => {
                    // One invite routes to one device: a second, different
                    // device pairing with the same host is refused instead
                    // of silently displacing the first (it can rescan).
                    match context.registry.note_pairing(&host_id, device_id) {
                        PairingNote::Started | PairingNote::Retried => {
                            log::info!(target: "audit", "pairing handshake routed to host pairing channel; host={host_id} device={device_id}");
                            context.registry.route_to_host_pairing(&host_id, forward)
                        }
                        PairingNote::Conflict => {
                            return refused(
                                RejectCode::AuthFailed,
                                "another pairing is already in progress for this host",
                            );
                        }
                    }
                }
                (None, HandshakeMode::Pairing, ConnectionRole::Host { .. })
                | (None, HandshakeMode::Pairing, ConnectionRole::HostPairing { .. }) => {
                    return match context.registry.pairing_peer(&host_id) {
                        Some(device_id) => {
                            let outcome = context
                                .registry
                                .route_to_device(&device_id, &host_id, 0, forward);
                            match outcome {
                                RouteOutcome::Delivered | RouteOutcome::Queued => {
                                    RouteReply::Forwarded
                                }
                                RouteOutcome::QueueFull => refused_frame(
                                    RejectCode::QueueFull,
                                    "device queue is full",
                                    reference(
                                        &host_id,
                                        Some(&device_id),
                                        0,
                                        RejectedFrameReason::QueueFull,
                                    ),
                                ),
                                RouteOutcome::NoRoute => refused_frame(
                                    RejectCode::UnknownRoute,
                                    "peer not connected",
                                    reference(
                                        &host_id,
                                        Some(&device_id),
                                        0,
                                        RejectedFrameReason::EndpointOffline,
                                    ),
                                ),
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

    // The declaration rides whichever host slot is completing the pairing
    // (the pairing channel) or managing devices (the session).
    let Some(host_id) = role.host_id() else {
        return refused(RejectCode::AuthFailed, "only hosts may declare links");
    };
    let host_id = host_id.to_owned();
    if !role.is_host() {
        return refused(RejectCode::AuthFailed, "only hosts may declare links");
    }
    if message.host_id != host_id {
        return refused(RejectCode::AuthFailed, "link claims another host identity");
    }

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
        // The pairing is gone, so anything queued for that device has no
        // delivery target left. Dropping it here is what keeps an unlinked
        // device's ciphertext from sitting in the process until restart.
        context
            .store
            .unlink_device(&message.host_id, &message.device_id)
            .map(|_| {
                context
                    .registry
                    .drop_link_queue(&message.device_id, &message.host_id);
                ControlFrame::Unlinked {
                    device_id: device_uuid,
                }
            })
    } else {
        context
            .store
            .link_device(&message.host_id, &message.device_id)
            .map(|_| ControlFrame::Linked {
                device_id: device_uuid,
            })
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
        purpose,
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
            if !context
                .challenges
                .consume(&challenge.0, ChallengeRole::Host)
            {
                return Err(RejectCode::AuthFailed);
            }
            let registered = context.store.host_signing_key(&host_id).unwrap_or(None);
            match registered {
                Some(public_key) if SigningKey::verify(&public_key, &challenge.0, &signature.0) => {
                    match purpose {
                        ConnectionPurpose::Session => Ok(ConnectionRole::Host { host_id }),
                        ConnectionPurpose::Pairing => Ok(ConnectionRole::HostPairing { host_id }),
                    }
                }
                _ => Err(RejectCode::AuthFailed),
            }
        }
        (FrameRole::Device, None, Some(device_id)) => {
            let device_id = device_id.to_string();
            if !context.auth_limiter.admit(&device_id) {
                return Err(RejectCode::RateLimited);
            }
            if !context
                .challenges
                .consume(&challenge.0, ChallengeRole::Device)
            {
                return Err(RejectCode::AuthFailed);
            }
            let registered = context.store.device_signing_key(&device_id).unwrap_or(None);
            match registered {
                Some(public_key) if SigningKey::verify(&public_key, &challenge.0, &signature.0) => {
                    // A device has one connection regardless of purpose;
                    // the field is accepted for symmetry and ignored.
                    let _ = purpose;
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
            frame: None,
        },
    }
}

/// The same refusal, carrying the structured reference to the frame that
/// was refused. The message stays a human summary for logs; the
/// structured reference is what a sender correlates on.
fn rejection_frame(code: RejectCode, message: &str, reference: RejectedFrame) -> Frame {
    Frame::Control {
        frame: ControlFrame::Rejected {
            code,
            message: message.to_owned(),
            frame: Some(reference),
        },
    }
}

fn send(websocket: &mut WebSocket<DeadlineStream>, frame: Frame) -> Result<(), tungstenite::Error> {
    let text = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_owned());
    websocket.send(Message::text(text))
}

fn read_frame(websocket: &mut WebSocket<DeadlineStream>, deadline: Instant) -> Option<Frame> {
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        // The per-read timeout lives inside DeadlineStream: tungstenite
        // may issue several socket reads for one message, and the bound
        // must survive every one of them.
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
        use crate::config::AUTH_TIMEOUT;
        assert!(KEEPALIVE_INTERVAL < IDLE_TIMEOUT);
        assert!(AUTH_TIMEOUT <= IDLE_TIMEOUT);
    }
}

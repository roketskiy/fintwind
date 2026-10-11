//! Black-box E2E for the relay: the real binary, real REST, real WS.
//!
//! Nothing here links the crate's internals — the relay runs as the
//! shipped process and is driven over TCP exactly the way the host
//! daemon and the PWA drive it.
//!
//! Covered (see HARDENING.md for the failure list):
//! - S1: oversized WS frame and oversized HTTP head are refused, and
//!   the relay survives both.
//! - S14: exact-origin CORS on REST, OPTIONS and error answers; a
//!   cross-site WS upgrade is refused; a native (Origin-less) client
//!   still authenticates.
//! - identity: role is fixed after authentication; a connection cannot
//!   address another subject.
//! - S2: the authenticated-connection cap is atomic, is not bypassed by
//!   a same-entity reconnect, and is released on disconnect.
//! - S17: one pairing route per host; a second device is refused, not
//!   spliced; an unpaired device is neither a route nor a queue target;
//!   the offline queue is bounded and ordered.
//! - B1: a slow-reading peer hits the live outbound bound instead of
//!   growing the process, and normal devices keep working.

mod common;

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use common::{
    ALLOWED_ORIGIN, Admission, EVIL_ORIGIN, Identity, OVERSIZE_FRAME_BYTES, QUEUE_PROBE_FRAMES,
    Relay, Ws, arm_pairing_offer, connect_handshake, handshake, link_frame, sealed,
    sealed_with_payload, unlink_frame,
};
use fintwind_remote_protocol::{
    Base64Bytes, ConnectionPurpose, ControlFrame, Frame, FrameRole, REMOTE_PROTOCOL_VERSION,
    RejectCode,
};

use uuid::Uuid;

// ---------------------------------------------------------------- cases

/// S1: the wire frame cap and the head bound are enforced by the
/// endpoint, and refusing either does not take the relay down.
#[test]
fn oversized_frames_and_heads_are_refused_without_killing_the_relay() {
    let relay = Relay::start();
    let device = Identity::new("10.9.0.1");
    let device_id = device.register(&relay, "device");
    let session = device.device_session(&relay, &device_id);

    // One text message past the cap: tungstenite refuses it at the
    // protocol layer and the session ends.
    session.send_raw("a".repeat(OVERSIZE_FRAME_BYTES));
    assert!(
        session.next_frame(Duration::from_secs(15)).is_none(),
        "stage=oversize-frame the relay must not accept a frame past the cap"
    );

    // A head that never terminates must not grow the process.
    let mut stream = TcpStream::connect(("127.0.0.1", relay.port)).expect("head socket");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("write timeout");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    let mut flood = b"GET /api/health HTTP/1.1\r\n".to_vec();
    flood.extend(std::iter::repeat(b'X').take(200_000));
    let _ = stream.write_all(&flood);
    let mut drained = Vec::new();
    let _ = stream.read_to_end(&mut drained);
    drop(stream);

    // A body length past the REST cap is refused before any allocation.
    let mut stream = TcpStream::connect(("127.0.0.1", relay.port)).expect("body socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    let _ = stream.write_all(
        b"POST /api/challenge HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 100000000\r\n\r\n",
    );
    let mut drained = Vec::new();
    let _ = stream.read_to_end(&mut drained);
    drop(stream);

    assert!(relay.alive(), "stage=oversize the relay must still serve");
}

/// S14: exact-origin CORS everywhere, and a cross-site browser upgrade
/// is refused while a native client still authenticates.
#[test]
fn cors_is_exact_origin_and_cross_site_ws_is_refused() {
    let relay = Relay::start();

    // Allowed origin: reflected exactly, with Vary.
    let allowed = relay
        .request("GET", "/api/health", &[("origin", ALLOWED_ORIGIN)], None)
        .expect("allowed origin answered");
    assert_eq!(allowed.status, 200, "stage=cors-allowed status");
    assert_eq!(
        allowed.header("access-control-allow-origin"),
        Some(ALLOWED_ORIGIN)
    );
    assert_eq!(allowed.header("vary"), Some("Origin"));

    // Foreign origin: refused, and answered with no CORS header at all.
    let evil = relay
        .request("GET", "/api/health", &[("origin", EVIL_ORIGIN)], None)
        .expect("foreign origin answered");
    assert_eq!(evil.status, 403, "stage=cors-foreign status");
    assert_eq!(
        evil.header("access-control-allow-origin"),
        None,
        "stage=cors-foreign a wildcard or reflected header must not appear"
    );

    // Native client: no Origin, no CORS headers, still served.
    let native = relay
        .request("GET", "/api/health", &[], None)
        .expect("native answered");
    assert_eq!(native.status, 200, "stage=cors-native status");
    assert_eq!(native.header("access-control-allow-origin"), None);

    // Error answers carry the same origin policy as success.
    let missing = relay
        .request("GET", "/api/nope", &[("origin", ALLOWED_ORIGIN)], None)
        .expect("missing path answered");
    assert_eq!(missing.status, 404, "stage=cors-error status");
    assert_eq!(
        missing.header("access-control-allow-origin"),
        Some(ALLOWED_ORIGIN),
        "stage=cors-error an allowed origin must be reflected on errors too"
    );

    // Preflight: allowed origin gets the reflected header and methods.
    let preflight = relay
        .request(
            "OPTIONS",
            "/api/challenge",
            &[("origin", ALLOWED_ORIGIN)],
            None,
        )
        .expect("preflight answered");
    assert_eq!(preflight.status, 204, "stage=preflight status");
    assert_eq!(
        preflight.header("access-control-allow-origin"),
        Some(ALLOWED_ORIGIN)
    );
    assert!(
        preflight
            .header("access-control-allow-methods")
            .is_some_and(|methods| methods.contains("POST"))
    );

    // Preflight from a foreign origin is refused the same way.
    let evil_preflight = relay
        .request(
            "OPTIONS",
            "/api/challenge",
            &[("origin", EVIL_ORIGIN)],
            None,
        )
        .expect("foreign preflight answered");
    assert_eq!(evil_preflight.status, 403, "stage=preflight-foreign status");
    assert_eq!(evil_preflight.header("access-control-allow-origin"), None);

    // A cross-site browser upgrade gets no 101.
    assert!(
        relay.ws("/ws", Some(EVIL_ORIGIN)).is_none(),
        "stage=ws-foreign a cross-site upgrade must be refused"
    );
    // The allowed origin upgrades.
    assert!(
        relay.ws("/ws", Some(ALLOWED_ORIGIN)).is_some(),
        "stage=ws-allowed the allow-listed origin must upgrade"
    );
    // A native client without Origin upgrades and authenticates by
    // signature — CORS tightening must not touch that path.
    let host = Identity::new("10.9.0.2");
    let host_id = host.register(&relay, "host");
    let challenge = host.challenge(&relay, "host");
    let auth = Frame::Auth {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role: FrameRole::Host,
        host_id: Some(Uuid::parse_str(&host_id).expect("host uuid")),
        device_id: None,
        purpose: ConnectionPurpose::Session,
        challenge: Base64Bytes(challenge.clone()),
        signature: Base64Bytes(host.sign(&challenge)),
    };
    let native_session = relay
        .ws("/ws", None)
        .expect("stage=ws-native an Origin-less upgrade must be accepted");
    native_session.send_frame(&auth);
    assert!(
        matches!(
            native_session.next_frame(Duration::from_secs(10)),
            Some(Frame::Control {
                frame: ControlFrame::RelayAck { .. }
            })
        ),
        "stage=ws-native the native client must authenticate by signature"
    );
}

/// Identity is fixed at authentication: a session cannot re-authenticate
/// or address a subject it does not belong to.
#[test]
fn a_session_cannot_switch_identity_or_address_a_stranger() {
    let relay = Relay::start();
    let host = Identity::new("10.8.0.1");
    let host_id = host.register(&relay, "host");
    let stranger = Identity::new("10.8.0.2");
    let stranger_host_id = stranger.register(&relay, "host");
    let device = Identity::new("10.8.0.3");
    let device_id = device.register(&relay, "device");

    let host_session = host.host_session(&relay, &host_id);
    let device_session = device.device_session(&relay, &device_id);

    // A second Auth frame on an authenticated connection is refused.
    let challenge = host.challenge(&relay, "host");
    host_session.send_frame(&Frame::Auth {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role: FrameRole::Host,
        host_id: Some(Uuid::parse_str(&host_id).expect("host uuid")),
        device_id: None,
        purpose: ConnectionPurpose::Session,
        challenge: Base64Bytes(challenge.clone()),
        signature: Base64Bytes(host.sign(&challenge)),
    });
    let (code, _) = host_session.expect_reject(Duration::from_secs(10));
    assert_eq!(code, RejectCode::Malformed, "stage=reauth code");

    // A host cannot claim another host's identity on a sealed frame.
    host_session.send_frame(&sealed(1, &stranger_host_id, Some(&device_id)));
    let (code, _) = host_session.expect_reject(Duration::from_secs(10));
    assert_eq!(
        code,
        RejectCode::AuthFailed,
        "stage=host-impersonation code"
    );

    // An unpaired device cannot address a host at all.
    device_session.send_frame(&sealed(1, &host_id, None));
    let (code, _) = device_session.expect_reject(Duration::from_secs(10));
    assert_eq!(code, RejectCode::UnknownRoute, "stage=device-unpaired code");

    assert!(relay.alive(), "stage=identity the relay must still serve");
}

/// S2: the authenticated-connection cap is atomic, counts both host
/// slots and devices, is not consumed by a same-entity reconnect, and is
/// released when a session ends.
#[test]
fn authenticated_connection_cap_is_atomic_and_released() {
    let relay = Relay::start();

    let mut live: Vec<(String, Identity, Ws)> = Vec::new();
    let mut cap = 0usize;
    loop {
        let source = format!("10.7.{}.{}", live.len() / 256, live.len() % 256);
        let device = Identity::new(&source);
        let device_id = device.register(&relay, "device");
        match device.connect(
            &relay,
            FrameRole::Device,
            None,
            Some(&device_id),
            ConnectionPurpose::Session,
        ) {
            Admission::Live(session) => {
                live.push((device_id, device, session));
                cap += 1;
            }
            Admission::Refused(code) => {
                assert_eq!(
                    code,
                    RejectCode::RateLimited,
                    "stage=cap the first refusal must be the connection cap"
                );
                break;
            }
        }
        assert!(cap <= 256, "stage=cap the cap was never reached");
    }
    assert!(
        cap >= 8,
        "stage=cap the cap must be a real bound, saw {cap}"
    );

    // At capacity a same-entity reconnect still works: it displaces its
    // own stale connection instead of asking for a new slot. The fresh
    // session replaces the stale one in `live`, so the slot count is
    // unchanged by the exercise.
    let (first_id, first, _old_session) = &live[0];
    let reconnected = match first.connect(
        &relay,
        FrameRole::Device,
        None,
        Some(first_id),
        ConnectionPurpose::Session,
    ) {
        Admission::Live(session) => session,
        Admission::Refused(code) => panic!("stage=cap-reconnect refused code={code:?}"),
    };
    let stale = std::mem::replace(&mut live[0].2, reconnected);
    drop(stale);

    // A brand-new subject is still refused while the relay is full.
    let stranger = Identity::new("10.7.255.255");
    let stranger_id = stranger.register(&relay, "device");
    match stranger.connect(
        &relay,
        FrameRole::Device,
        None,
        Some(&stranger_id),
        ConnectionPurpose::Session,
    ) {
        Admission::Refused(code) => {
            assert_eq!(code, RejectCode::RateLimited, "stage=cap-full code")
        }
        Admission::Live(_) => panic!("stage=cap-full a new subject was admitted at capacity"),
    }

    // Releasing one slot admits exactly one new subject.
    let released = live.pop().expect("a session to release");
    drop(released);
    let fresh = Identity::new("10.7.255.254");
    let fresh_id = fresh.register(&relay, "device");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match fresh.connect(
            &relay,
            FrameRole::Device,
            None,
            Some(&fresh_id),
            ConnectionPurpose::Session,
        ) {
            Admission::Live(_) => break,
            Admission::Refused(code) => {
                assert!(Instant::now() < deadline, "stage=cap-release no slot freed");
                assert_eq!(code, RejectCode::RateLimited);
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    assert!(relay.alive(), "stage=cap the relay must still serve");
}

/// S17: one pairing route per host. A second device is refused instead
/// of being spliced into the first device's handshake, and an unpaired
/// device is neither a route target nor a queue target.
#[test]
fn one_pairing_route_per_host_and_no_unpaired_targets() {
    let relay = Relay::start();
    let host = Identity::new("10.6.0.1");
    let host_id = host.register(&relay, "host");
    let first = Identity::new("10.6.0.2");
    let first_id = first.register(&relay, "device");
    let second = Identity::new("10.6.0.3");
    let second_id = second.register(&relay, "device");

    // The host opens its pairing channel; the first device starts.
    let pairing = host.host_pairing(&relay, &host_id);
    let first_device = first.device_session(&relay, &first_id);
    first_device.send_frame(&handshake(0, &host_id, None, FrameRole::Device));
    assert!(
        pairing.next_frame(Duration::from_secs(10)).is_some(),
        "stage=pairing the first device's handshake must reach the host"
    );

    // The second device is refused, not spliced in.
    let second_device = second.device_session(&relay, &second_id);
    second_device.send_frame(&handshake(0, &host_id, None, FrameRole::Device));
    let (code, _) = second_device.expect_reject(Duration::from_secs(10));
    assert_eq!(
        code,
        RejectCode::AuthFailed,
        "stage=pairing-conflict a second concurrent pairing must be refused"
    );

    // The host's reply reaches only the first device.
    pairing.send_frame(&handshake(1, &host_id, None, FrameRole::Host));
    assert!(
        first_device.next_frame(Duration::from_secs(10)).is_some(),
        "stage=pairing-route the host reply must reach the pairing device"
    );
    second_device.expect_silence(Duration::from_millis(500));

    // The first device's next step still routes (idempotent, not a
    // conflict with itself).
    first_device.send_frame(&handshake(2, &host_id, None, FrameRole::Device));
    assert!(
        pairing.next_frame(Duration::from_secs(10)).is_some(),
        "stage=pairing-continue the pairing device's steps must keep routing"
    );

    // An unpaired device is not a route target for business traffic.
    let host_session = host.host_session(&relay, &host_id);
    host_session.send_frame(&sealed(1, &host_id, Some(&second_id)));
    let (code, _) = host_session.expect_reject(Duration::from_secs(10));
    assert_eq!(
        code,
        RejectCode::UnknownRoute,
        "stage=unpaired-route an unpaired device must not be addressable"
    );
    // ...and nothing was queued for it either.
    second_device.expect_silence(Duration::from_millis(500));

    // A fresh pairing channel starts a fresh pairing: the stale mapping
    // from the displaced channel must not keep refusing the new attempt.
    drop(pairing);
    let repaired = host.host_pairing(&relay, &host_id);
    second_device.send_frame(&handshake(0, &host_id, None, FrameRole::Device));
    assert!(
        repaired.next_frame(Duration::from_secs(10)).is_some(),
        "stage=pairing-restart a fresh pairing channel must accept a new device"
    );

    assert!(relay.alive(), "stage=pairing the relay must still serve");
}

/// B1: the live outbound bound is real, and a slow peer does not take
/// the relay — or another device's control path — down with it.
#[test]
fn a_slow_consumer_hits_the_live_outbound_bound() {
    let relay = Relay::start();
    let host = Identity::new("10.4.0.1");
    let host_id = host.register(&relay, "host");
    let stuck = Identity::new("10.4.0.2");
    let stuck_id = stuck.register(&relay, "device");
    let healthy = Identity::new("10.4.0.3");
    let healthy_id = healthy.register(&relay, "device");

    // Pair both devices so business traffic is routable.
    let nonce = arm_pairing_offer(&host, &relay, &host_id);
    let mut host_session = host.host_session(&relay, &host_id);
    for device_id in [&stuck_id, &healthy_id] {
        host_session.send_frame(&link_frame(
            &host_id,
            device_id,
            &[0x5a; 32],
            &nonce,
            &host.signing,
        ));
        // One nonce arms one link; the second declaration reuses the
        // same invite and is refused, which is fine — link the rest by
        // re-arming below.
        let _ = host_session.next_frame(Duration::from_secs(10));
    }
    // The single invite links one device; arm a second one for the other.
    let second_nonce = arm_pairing_offer(&host, &relay, &host_id);
    host_session.send_frame(&link_frame(
        &host_id,
        &healthy_id,
        &[0x5b; 32],
        &second_nonce,
        &host.signing,
    ));
    match host_session.next_frame(Duration::from_secs(10)) {
        Some(Frame::Control {
            frame: ControlFrame::Linked { .. },
        }) => {}
        other => panic!("stage=link-second unexpected answer: {other:?}"),
    }

    // The stuck device authenticates and then stops reading.
    let stuck_session = stuck.device_session(&relay, &stuck_id);
    stuck_session.stall();
    let healthy_session = healthy.device_session(&relay, &healthy_id);

    // Frames big enough that the peer's socket buffers fill first and
    // the relay's live outbound accounting (4 MiB) then becomes the
    // binding limit. The refusal point therefore depends on how much
    // the OS will buffer for a reader that never reads — what the test
    // pins down is that a refusal *does* come, that it names the frame,
    // and that nothing accepted is ever lost.
    let payload = 700_000;
    let mut delivered = 0usize;
    let mut refused = false;
    for seq in 1..=60u64 {
        host_session.send_frame(&sealed_with_payload(seq, &host_id, &stuck_id, payload));
        match host_session.next_frame(Duration::from_secs(60)) {
            Some(Frame::Control {
                frame: ControlFrame::RelayAck { .. },
            }) => delivered += 1,
            Some(Frame::Control {
                frame: ControlFrame::Rejected { code, message, .. },
            }) => {
                assert_eq!(code, RejectCode::QueueFull, "stage=slow-consumer code");
                assert!(
                    message.contains(&format!("seq={seq}")),
                    "stage=slow-consumer the refusal must name the frame, got: {message}"
                );
                refused = true;
                break;
            }
            other => panic!("stage=slow-consumer unexpected answer: {other:?}"),
        }
    }
    assert!(
        refused,
        "stage=slow-consumer the live outbound bound must refuse, delivered={delivered}"
    );
    // What filled must be the live buffer, not the offline queue: the
    // health answer's queue depth stays at zero while the peer holds
    // its session. (Aggregate counters only; they name no subject.)
    let health = relay.health_json();
    assert_eq!(
        health["queue"]["messages"].as_u64(),
        Some(0),
        "stage=slow-consumer a live peer must not fall back to the offline queue"
    );
    assert_eq!(
        health["sessions"]["authenticated"].as_u64(),
        Some(3),
        "stage=slow-consumer the stalled session must stay registered"
    );

    // The stall itself is holding: nothing reaches the test's reader,
    // and the session is still alive (silence is not a closed socket).
    stuck_session.expect_silence(Duration::from_millis(500));
    assert!(
        !stuck_session.is_closed(),
        "stage=slow-consumer the stalled session must still be open"
    );

    // The healthy device is still fully controllable while the stuck one
    // holds its buffer.
    host_session.send_frame(&sealed(90, &host_id, Some(&healthy_id)));
    assert!(
        matches!(
            host_session.next_frame(Duration::from_secs(10)),
            Some(Frame::Control {
                frame: ControlFrame::RelayAck { .. }
            })
        ),
        "stage=slow-consumer a healthy device must still be routable"
    );
    assert!(
        healthy_session
            .next_frame(Duration::from_secs(10))
            .is_some(),
        "stage=slow-consumer the healthy device must still receive"
    );
    healthy_session.send_frame(&sealed(91, &host_id, None));
    assert!(
        host_session.next_frame(Duration::from_secs(10)).is_some(),
        "stage=slow-consumer the healthy device must still send"
    );

    // Resuming the reader delivers exactly what was accepted — the bound
    // refused, it never silently dropped.
    stuck_session.unstall();
    let mut received = 0usize;
    while received < delivered {
        match stuck_session.next_frame(Duration::from_secs(20)) {
            Some(Frame::Sealed { .. }) => received += 1,
            Some(_) => continue,
            None => break,
        }
    }
    assert_eq!(
        received, delivered,
        "stage=slow-consumer everything accepted must arrive once the peer reads"
    );

    assert!(
        relay.alive(),
        "stage=slow-consumer the relay must still serve"
    );
}

/// The offline queue is bounded and ordered: a full queue refuses the
/// next frame, and a reconnecting device drains exactly what fit, in
/// order.
#[test]
fn offline_device_queue_is_bounded_and_ordered() {
    let relay = Relay::start();
    let host = Identity::new("10.5.0.1");
    let host_id = host.register(&relay, "host");
    let device = Identity::new("10.5.0.2");
    let device_id = device.register(&relay, "device");

    // Pair them the way the host does after the QR: arm a nonce, then
    // declare the link over the authenticated channel.
    let nonce = arm_pairing_offer(&host, &relay, &host_id);
    let host_session = host.host_session(&relay, &host_id);
    host_session.send_frame(&link_frame(
        &host_id,
        &device_id,
        &[0x5a; 32],
        &nonce,
        &host.signing,
    ));
    match host_session.next_frame(Duration::from_secs(10)) {
        Some(Frame::Control {
            frame: ControlFrame::Linked { .. },
        }) => {}
        other => panic!("stage=link unexpected answer: {other:?}"),
    }

    // The device is offline: the host's frames queue. The per-connection
    // frame budget (200/s, M0 resource table) is real, so the probe
    // paces itself and treats a refusal as "wait for the next window"
    // rather than a failure — what it is looking for is the queue's
    // message cap, which refuses with `QueueFull`.
    let mut acked: Vec<u64> = Vec::new();
    let mut refused = false;
    let mut seq = 1u64;
    'fill: while acked.len() < QUEUE_PROBE_FRAMES {
        let batch = 50.min(QUEUE_PROBE_FRAMES - acked.len());
        for _ in 0..batch {
            host_session.send_frame(&sealed(seq, &host_id, Some(&device_id)));
            seq += 1;
        }
        let mut answered = 0;
        while answered < batch {
            match host_session.next_frame(Duration::from_secs(30)) {
                Some(Frame::Control {
                    frame: ControlFrame::RelayAck { acked_seq },
                }) => {
                    assert!(
                        acked_seq > *acked.last().unwrap_or(&0),
                        "stage=queue-order acks must be strictly increasing"
                    );
                    acked.push(acked_seq);
                    answered += 1;
                }
                Some(Frame::Control {
                    frame: ControlFrame::Rejected { code, .. },
                }) => match code {
                    RejectCode::QueueFull => {
                        refused = true;
                        break 'fill;
                    }
                    RejectCode::RateLimited => {
                        // Frame budget spent: wait for the next window and
                        // keep going — this frame was refused, not queued.
                        std::thread::sleep(Duration::from_secs(1));
                        answered += 1;
                    }
                    other => panic!("stage=queue refused code={other:?}"),
                },
                Some(Frame::Control { .. }) => continue,
                other => panic!("stage=queue unexpected answer: {other:?}"),
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(refused, "stage=queue-full the message cap must refuse");
    assert!(
        acked.len() >= 64,
        "stage=queue the queue must hold a real backlog"
    );

    // The device reconnects and drains exactly what was acked, in order.
    // A reconnecting device's queued frames go out *before* the
    // connection-level ack (by design: backlog must not be overtaken by
    // live traffic), so the drain loop collects everything and skips
    // the control frames.
    let challenge = device.challenge(&relay, "device");
    let device_session = relay
        .ws("/ws", Some(ALLOWED_ORIGIN))
        .expect("stage=queue-reconnect the allowed origin must upgrade");
    device_session.send_frame(&Frame::Auth {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role: FrameRole::Device,
        host_id: None,
        device_id: Some(Uuid::parse_str(&device_id).expect("device uuid")),
        purpose: ConnectionPurpose::Session,
        challenge: Base64Bytes(challenge.clone()),
        signature: Base64Bytes(device.sign(&challenge)),
    });
    let mut received = VecDeque::new();
    let drain_deadline = Instant::now() + Duration::from_secs(120);
    while received.len() < acked.len() {
        match device_session.next_frame(Duration::from_secs(15)) {
            Some(Frame::Sealed { seq, .. }) => received.push_back(seq),
            Some(_) => continue,
            None => {
                // A transient stall is not a lost frame: keep waiting for
                // the rest of the backlog until the overall deadline.
                assert!(
                    Instant::now() < drain_deadline,
                    "stage=queue-drain the backlog stopped arriving: {}/{} frames",
                    received.len(),
                    acked.len()
                );
            }
        }
    }
    assert_eq!(
        received.len(),
        acked.len(),
        "stage=queue-drain the device must receive exactly what was acked: {}/{}",
        received.len(),
        acked.len()
    );
    for (index, seq) in received.iter().enumerate() {
        assert_eq!(
            *seq, acked[index],
            "stage=queue-order the queue must preserve delivery order"
        );
    }

    assert!(relay.alive(), "stage=queue the relay must still serve");
}

/// Arm an invite and link one device, consuming the Linked ack. The
/// shared preamble of the offline-budget cases: pairing is the only way
/// a device becomes a delivery target, and these cases want targets that
/// never come online.
fn link_one(relay: &Relay, host: &Identity, host_id: &str, host_session: &Ws, device_id: &str) {
    let nonce = arm_pairing_offer(host, relay, host_id);
    host_session.send_frame(&link_frame(
        host_id,
        device_id,
        &[0x5a; 32],
        &nonce,
        &host.signing,
    ));
    match host_session.next_frame(Duration::from_secs(15)) {
        Some(Frame::Control {
            frame: ControlFrame::Linked { .. },
        }) => {}
        other => panic!("stage=link unexpected answer: {other:?}"),
    }
}

/// C1: the offline budget is process-wide. Per-device caps alone would
/// let every offline device hold its own 32 MiB forever; the aggregate
/// cap is what keeps the total inside a small box's memory. Proven at
/// the refusal, not by trusting a constant: two devices spend the whole
/// budget, a third with an *empty* queue is refused, and unlinking one
/// device frees its share for the third immediately.
#[test]
fn the_offline_budget_is_process_wide_and_unlink_frees_it() {
    let relay = Relay::start();
    let host = Identity::new("10.7.0.1");
    let host_id = host.register(&relay, "host");
    let host_session = host.host_session(&relay, &host_id);

    let mut devices: Vec<String> = Vec::new();
    for n in 2..=4u8 {
        let device = Identity::new(&format!("10.7.0.{n}"));
        let device_id = device.register(&relay, "device");
        link_one(&relay, &host, &host_id, &host_session, &device_id);
        devices.push(device_id);
    }

    // One frame near the wire ceiling: 32 of these are one device's own
    // 32 MiB, and 64 are the process-wide budget.
    let payload = 786_000;
    let fill = |device_id: &str| -> usize {
        let mut accepted = 0usize;
        for seq in 1..=60u64 {
            host_session.send_frame(&sealed_with_payload(seq, &host_id, device_id, payload));
            match host_session.next_frame(Duration::from_secs(30)) {
                Some(Frame::Control {
                    frame: ControlFrame::RelayAck { .. },
                }) => accepted += 1,
                Some(Frame::Control {
                    frame: ControlFrame::Rejected { code, .. },
                }) => {
                    assert_eq!(
                        code,
                        RejectCode::QueueFull,
                        "stage=budget the fill must end in QueueFull"
                    );
                    break;
                }
                other => panic!("stage=budget unexpected answer: {other:?}"),
            }
        }
        accepted
    };

    let first = fill(&devices[0]);
    let second = fill(&devices[1]);
    assert!(
        first > 20 && second > 20,
        "stage=budget each device must hold a meaningful share, got {first}/{second}"
    );

    // The third device's queue is empty and stays empty: its refusal is
    // the aggregate speaking, and the refused frame was never queued.
    host_session.send_frame(&sealed_with_payload(1, &host_id, &devices[2], payload));
    let (code, message, reference) = host_session.expect_reject_full(Duration::from_secs(30));
    assert_eq!(code, RejectCode::QueueFull, "stage=aggregate code");
    assert!(
        reference.is_some(),
        "stage=aggregate the refusal must carry the structured frame reference, message={message}"
    );
    host_session.expect_silence(Duration::from_millis(400));

    // Unlink the first device: its ciphertext has no delivery target
    // left, so the relay drops the queue on the spot and the third
    // device's frame fits immediately — no reconnect, no TTL wait.
    let unlink_nonce = arm_pairing_offer(&host, &relay, &host_id);
    host_session.send_frame(&unlink_frame(
        &host_id,
        &devices[0],
        &unlink_nonce,
        &host.signing,
    ));
    match host_session.next_frame(Duration::from_secs(15)) {
        Some(Frame::Control {
            frame: ControlFrame::Unlinked { .. },
        }) => {}
        other => panic!("stage=unlink unexpected answer: {other:?}"),
    }

    host_session.send_frame(&sealed_with_payload(2, &host_id, &devices[2], payload));
    assert!(
        matches!(
            host_session.next_frame(Duration::from_secs(30)),
            Some(Frame::Control {
                frame: ControlFrame::RelayAck { .. }
            })
        ),
        "stage=aggregate freeing one device's share must admit the waiting frame"
    );
    let health = relay.health_json();
    assert!(
        health["queue"]["devices"].as_u64().unwrap_or(0) >= 2,
        "stage=aggregate the remaining queues must still be held"
    );
    assert!(relay.alive(), "stage=aggregate the relay must still serve");
}

/// C1 (expiry): undeliverable ciphertext leaves on a clock. With the
/// queue TTL overridden to seconds, a full queue frees itself and the
/// next frame is accepted — the accounting must leave with the frames,
/// which is what the health totals show.
#[test]
fn offline_frames_expire_and_release_their_budget() {
    let relay = Relay::start_with_env(&[
        ("FINTWIND_RELAY_QUEUE_TTL_SECS", "2"),
        // The reclaim below must be the sweeper's doing, not an
        // enqueue-triggered cleanup: shrink its cadence so the proof
        // lands in seconds while the window stays honest.
        ("FINTWIND_RELAY_SWEEP_INTERVAL_SECS", "2"),
    ]);
    let host = Identity::new("10.7.1.1");
    let host_id = host.register(&relay, "host");
    let host_session = host.host_session(&relay, &host_id);
    let device = Identity::new("10.7.1.2");
    let device_id = device.register(&relay, "device");
    link_one(&relay, &host, &host_id, &host_session, &device_id);

    let payload = 786_000;
    let mut accepted = 0usize;
    for seq in 1..=3u64 {
        host_session.send_frame(&sealed_with_payload(seq, &host_id, &device_id, payload));
        match host_session.next_frame(Duration::from_secs(30)) {
            Some(Frame::Control {
                frame: ControlFrame::RelayAck { .. },
            }) => accepted += 1,
            Some(Frame::Control {
                frame: ControlFrame::Rejected { code, .. },
            }) => {
                assert_eq!(code, RejectCode::QueueFull, "stage=expiry code");
                break;
            }
            other => panic!("stage=expiry unexpected answer: {other:?}"),
        }
    }
    assert_eq!(accepted, 3, "stage=expiry the queue must hold a backlog");

    // Past the TTL, the same frame is accepted: the expired frames left
    // first — on the sweeper's clock, with zero traffic — and their bytes
    // left the aggregate with them.
    let reclaimed_by = Instant::now() + Duration::from_secs(35);
    loop {
        let queue = relay.health_json()["queue"].clone();
        if queue["messages"].as_u64() == Some(0) {
            assert_eq!(
                queue["bytes"].as_u64(),
                Some(0),
                "stage=expiry byte reclaim"
            );
            assert_eq!(
                queue["devices"].as_u64(),
                Some(0),
                "stage=expiry queue reclaim"
            );
            break;
        }
        assert!(
            Instant::now() < reclaimed_by,
            "stage=expiry no traffic-independent reclaim"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    host_session.send_frame(&sealed_with_payload(500, &host_id, &device_id, payload));
    assert!(
        matches!(
            host_session.next_frame(Duration::from_secs(30)),
            Some(Frame::Control {
                frame: ControlFrame::RelayAck { .. }
            })
        ),
        "stage=expiry an expired queue must make room for the next frame"
    );
    let queue = relay.health_json()["queue"].clone();
    assert_eq!(
        queue["messages"].as_u64(),
        Some(1),
        "stage=expiry only the fresh frame may remain, got {queue}"
    );
}

/// C2: the admission window is total, not per read. A client that keeps
/// the head alive by sending one byte every few seconds used to be able
/// to hold one of the 64 unauthenticated slots forever; the deadline is
/// measured from accept time, so it loses the slot on schedule and a
/// normal client is admitted right after.
#[test]
fn a_dripping_client_loses_its_slot_at_the_deadline() {
    dripping_admission_case(
        b"POST /api/challenge HTTP/1.1\r\nHost: local\r\nX-Drip: ",
        b"x",
        false,
    );
}

fn dripping_admission_case(prefix: &[u8], byte: &[u8], delayed_prefix: bool) {
    let relay = Relay::start();
    let mut socket = TcpStream::connect(("127.0.0.1", relay.port)).expect("drip socket");
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("drip read timeout");
    socket
        .set_write_timeout(Some(Duration::from_millis(200)))
        .expect("drip write timeout");
    let started = Instant::now();
    if delayed_prefix {
        std::thread::sleep(Duration::from_secs(7));
    }
    socket.write_all(prefix).expect("drip prefix");
    let deadline = started + Duration::from_secs(13);
    let mut closed = false;
    while Instant::now() < deadline {
        if socket.write_all(byte).is_err() {
            closed = true;
            break;
        }
        let mut response = [0u8; 512];
        match socket.read(&mut response) {
            Ok(0) => {
                closed = true;
                break;
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                closed = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    assert!(
        closed,
        "stage=admission continuous drip exceeded the total deadline"
    );
    assert!(
        relay.alive(),
        "stage=admission normal traffic must remain reachable"
    );
}

#[test]
fn dripping_rest_body_obeys_the_same_total_deadline() {
    dripping_admission_case(
        b"POST /api/challenge HTTP/1.1\r\nHost: local\r\nContent-Length: 4096\r\n\r\n",
        b" ",
        false,
    );
}

#[test]
fn delayed_websocket_upgrade_does_not_reset_the_auth_deadline() {
    // A long incomplete masked text frame keeps the parser receiving bytes.
    // The HTTP phase consumes seven seconds of the original ten-second budget.
    dripping_admission_case(b"GET /ws HTTP/1.1\r\nHost: local\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: EREREREREREREREREREREQ==\r\nSec-WebSocket-Version: 13\r\n\r\n\x81\xfe\x10\x00\x00\x00\x00\x00", b" ", true);
}

/// C3: a device's business-session handshake names its own device. The
/// frame's device id used to be taken as routing truth — an
/// authenticated device could point a handshake at another device of the
/// same account and the relay would deliver it into that channel. The
/// Noise handshake would fail later, but delivering someone else's
/// handshake is not the relay's to do.
#[test]
fn a_connect_handshake_names_only_its_own_device() {
    let relay = Relay::start();
    let host = Identity::new("10.7.2.1");
    let host_id = host.register(&relay, "host");
    let first = Identity::new("10.7.2.2");
    let first_id = first.register(&relay, "device");
    let second = Identity::new("10.7.2.3");
    let second_id = second.register(&relay, "device");

    let host_session = host.host_session(&relay, &host_id);
    link_one(&relay, &host, &host_id, &host_session, &first_id);
    link_one(&relay, &host, &host_id, &host_session, &second_id);

    // Device one is authenticated and marks its handshake with device
    // two's id. Refused, and the host never sees it.
    let first_session = first.device_session(&relay, &first_id);
    first_session.send_frame(&connect_handshake(
        0,
        &host_id,
        &second_id,
        FrameRole::Device,
    ));
    let (code, message, _) = first_session.expect_reject_full(Duration::from_secs(15));
    assert_eq!(
        code,
        RejectCode::AuthFailed,
        "stage=foreign-handshake a foreign-marked connect must be refused"
    );
    let _ = message;
    host_session.expect_silence(Duration::from_millis(400));

    // The host session is unaffected: device two's own handshake flows.
    let second_session = second.device_session(&relay, &second_id);
    second_session.send_frame(&connect_handshake(
        0,
        &host_id,
        &second_id,
        FrameRole::Device,
    ));
    assert!(
        matches!(host_session.next_frame(Duration::from_secs(15)),
        Some(Frame::Handshake { host_id: actual_host, device_id: Some(actual_device), role: FrameRole::Device, .. })
        if actual_host.to_string() == host_id && actual_device.to_string() == second_id),
        "stage=foreign-handshake the exact legitimate device frame must route"
    );
    assert!(
        relay.alive(),
        "stage=foreign-handshake the relay must serve"
    );
}

/// Removing a link must not discard accepted ciphertext from another host.
#[test]
fn unlink_only_reclaims_the_selected_hosts_frames() {
    let relay = Relay::start();
    let first = Identity::new("10.7.4.1");
    let first_id = first.register(&relay, "host");
    let second = Identity::new("10.7.4.2");
    let second_id = second.register(&relay, "host");
    let device = Identity::new("10.7.4.3");
    let device_id = device.register(&relay, "device");
    let first_session = first.host_session(&relay, &first_id);
    let second_session = second.host_session(&relay, &second_id);
    for (host, id, session) in [
        (&first, &first_id, &first_session),
        (&second, &second_id, &second_session),
    ] {
        link_one(&relay, host, id, session, &device_id);
        session.send_frame(&sealed(1, id, Some(&device_id)));
        assert!(
            matches!(
                session.next_frame(Duration::from_secs(10)),
                Some(Frame::Control {
                    frame: ControlFrame::RelayAck { acked_seq: 1 }
                })
            ),
            "stage=multi-host queue did not accept frame"
        );
    }
    let nonce = arm_pairing_offer(&first, &relay, &first_id);
    first_session.send_frame(&unlink_frame(&first_id, &device_id, &nonce, &first.signing));
    assert!(
        matches!(
            first_session.next_frame(Duration::from_secs(10)),
            Some(Frame::Control {
                frame: ControlFrame::Unlinked { .. }
            })
        ),
        "stage=multi-host unlink missing"
    );
    assert_eq!(
        relay.health_json()["queue"]["messages"].as_u64(),
        Some(1),
        "stage=multi-host another host's frame was discarded"
    );
    let challenge = device.challenge(&relay, "device");
    let reconnect = relay.ws("/ws", Some(ALLOWED_ORIGIN)).expect("reconnect");
    reconnect.send_frame(&Frame::Auth {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role: FrameRole::Device,
        host_id: None,
        device_id: Some(Uuid::parse_str(&device_id).expect("device id")),
        purpose: ConnectionPurpose::Session,
        challenge: Base64Bytes(challenge.clone()),
        signature: Base64Bytes(device.sign(&challenge)),
    });
    assert!(
        matches!(reconnect.next_frame(Duration::from_secs(10)), Some(Frame::Sealed { host_id, seq: 1, .. }) if host_id.to_string() == second_id),
        "stage=multi-host remaining host's exact frame was lost"
    );
}

/// A departed slow consumer releases the aggregate budget, not just its slot.
#[test]
fn slow_consumer_exit_releases_global_outbound_accounting() {
    let relay = Relay::start();
    let host = Identity::new("10.7.5.1");
    let host_id = host.register(&relay, "host");
    let host_session = host.host_session(&relay, &host_id);
    let device = Identity::new("10.7.5.2");
    let device_id = device.register(&relay, "device");
    link_one(&relay, &host, &host_id, &host_session, &device_id);
    let slow = device.device_session(&relay, &device_id);
    slow.stall();
    let mut rejected = false;
    for seq in 1..=32 {
        host_session.send_frame(&sealed_with_payload(seq, &host_id, &device_id, 700_000));
        match host_session.next_frame(Duration::from_secs(15)) {
            Some(Frame::Control {
                frame:
                    ControlFrame::Rejected {
                        code: RejectCode::QueueFull,
                        ..
                    },
            }) => {
                rejected = true;
                break;
            }
            Some(Frame::Control {
                frame: ControlFrame::RelayAck { .. },
            }) => {}
            _ => panic!("stage=outbound-reclaim unexpected answer"),
        }
    }
    assert!(
        rejected,
        "stage=outbound-reclaim slow consumer did not fill budget"
    );
    assert!(
        relay.health_json()["outbound"]["bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0),
        "stage=outbound-reclaim no outstanding bytes observed"
    );
    drop(slow);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if relay.health_json()["outbound"]["bytes"].as_u64() == Some(0) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "stage=outbound-reclaim departed peer leaked aggregate budget"
        );
        std::thread::sleep(Duration::from_millis(150));
    }
}

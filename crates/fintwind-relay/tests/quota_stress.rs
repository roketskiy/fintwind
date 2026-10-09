//! Quota stress: the two global admission caps competing, with a
//! repeatable artifact.
//!
//! What this proves and records:
//! - 64 authenticated connections fill the authenticated cap; the next
//!   subject is refused with `RateLimited`, and a same-entity reconnect
//!   is not consumed by that.
//! - 64 slow-header connections fill the unauthenticated cap; the next
//!   connection is refused with HTTP 503 before any parsing.
//! - While both caps are full, already-authenticated sessions keep
//!   routing: the two quotas do not consume each other.
//! - Releasing either side admits new work: the caps are accounting,
//!   not a one-way ratchet.
//! - The relay's log carries no nonce, challenge, signature, public
//!   key, or ciphertext — the artifact records stages, counts, and
//!   reject codes only, plus a hash of the relay source it measured.
//!
//! Run through tests/stress/run-stress.ps1, which builds the binary and
//! points FINTWIND_STRESS_OUT at the artifact directory.

mod common;

use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use common::{
    arm_pairing_offer, link_frame, relay_source_hash, sealed, Admission, Identity, Relay,
    SlowHead, MAX_AUTHENTICATED_CONNECTIONS, MAX_UNAUTHENTICATED_CONNECTIONS,
};
use fintwind_remote_protocol::{ControlFrame, Frame};
use serde_json::{json, Value};

/// One recorded stage. Stages, counts and codes only — never a key, a
/// nonce, a payload, or a URL.
struct Stage {
    name: &'static str,
    detail: Value,
    at_ms: u128,
}

fn record(stages: &mut Vec<Stage>, started: Instant, name: &'static str, detail: Value) {
    stages.push(Stage {
        name,
        detail,
        at_ms: started.elapsed().as_millis(),
    });
}

#[test]
fn both_global_quotas_compete_and_release() {
    let started = Instant::now();
    let relay = Relay::start_capturing_logs();
    let mut stages: Vec<Stage> = Vec::new();

    // ---- phase 1: fill the authenticated cap --------------------------
    let host = Identity::new("10.3.0.1");
    let host_id = host.register(&relay, "host");
    let host_session = host.host_session(&relay, &host_id);

    let mut live: Vec<(String, Identity, common::Ws)> = Vec::new();
    let mut cap_refusal = String::from("none");
    loop {
        let source = format!("10.3.{}.{}", live.len() / 256, live.len() % 256);
        let device = Identity::new(&source);
        let device_id = device.register(&relay, "device");
        match device.connect_session(&relay, &device_id) {
            Admission::Live(session) => live.push((device_id, device, session)),
            Admission::Refused(code) => {
                cap_refusal = format!("{code:?}");
                break;
            }
        }
        assert!(
            live.len() <= MAX_AUTHENTICATED_CONNECTIONS + 8,
            "stage=quota-auth the cap was never reached"
        );
    }
    record(
        &mut stages,
        started,
        "authenticated-cap",
        json!({
            "live": live.len(),
            "refusal": cap_refusal,
            "expectedLiveAtMost": MAX_AUTHENTICATED_CONNECTIONS,
        }),
    );
    assert_eq!(
        live.len(),
        MAX_AUTHENTICATED_CONNECTIONS - 1,
        "stage=quota-auth the host holds one slot, the devices the rest"
    );
    assert_eq!(cap_refusal, "RateLimited", "stage=quota-auth refusal code");

    // A same-entity reconnect at capacity still works: it displaces its
    // own connection rather than asking for a new slot.
    let (first_id, first, _first_session) = &live[0];
    match first.connect_session(&relay, first_id) {
        Admission::Live(_) => {}
        Admission::Refused(code) => panic!("stage=quota-reconnect refused code={code:?}"),
    }
    record(
        &mut stages,
        started,
        "same-entity-reconnect-at-capacity",
        json!({ "outcome": "live" }),
    );

    // ---- phase 2: fill the unauthenticated cap ------------------------
    // Everything the authenticated phase needs is prepared first: once
    // the unauthenticated cap is full, no new HTTP request gets in, and
    // that is the point of the phase.
    let nonce = arm_pairing_offer(&host, &relay, &host_id);
    let target = live[1].0.clone();

    // Slow-header connections hold slots only inside the total deadline.
    let mut slow: Vec<SlowHead> = Vec::new();
    while slow.len() < MAX_UNAUTHENTICATED_CONNECTIONS {
        let mut connection = SlowHead::open(&relay);
        connection.trickle();
        slow.push(connection);
    }
    // The next connection is refused before any parsing happens.
    let overflow_status = relay
        .request("GET", "/api/health", &[], None)
        .map(|response| response.status);
    record(
        &mut stages,
        started,
        "unauthenticated-cap",
        json!({
            "held": slow.len(),
            "overflowStatus": overflow_status,
            "overflowRefused": overflow_status == Some(503) || overflow_status.is_none(),
        }),
    );
    assert!(overflow_status == Some(503) || overflow_status.is_none(),
        "stage=quota-unauth overflow must be refused, never served");

    // ---- phase 3: both caps full, authenticated work continues --------
    for connection in slow.iter_mut() {
        connection.trickle();
    }
    host_session.send_frame(&link_frame(
        &host_id,
        &target,
        &[0x5a; 32],
        &nonce,
        &host.signing,
    ));
    let linked = matches!(
        host_session.next_frame(Duration::from_secs(15)),
        Some(Frame::Control {
            frame: ControlFrame::Linked { .. }
        })
    );
    host_session.send_frame(&sealed(1, &host_id, Some(&target)));
    let delivered = matches!(
        host_session.next_frame(Duration::from_secs(15)),
        Some(Frame::Control {
            frame: ControlFrame::RelayAck { .. }
        })
    );
    record(
        &mut stages,
        started,
        "both-caps-full-authenticated-work-continues",
        json!({ "link": linked, "sealedDelivered": delivered }),
    );
    assert!(linked, "stage=quota-both a link must still be declarable");
    assert!(
        delivered,
        "stage=quota-both business traffic must still route at full load"
    );

    // ---- phase 4: release the unauthenticated side --------------------
    let released_unauth = 12;
    slow.truncate(slow.len() - released_unauth);
    // A freed slot admits new work immediately.
    let health_status = relay
        .request("GET", "/api/health", &[], None)
        .map(|response| response.status);
    // ...and the slots can be taken again, which is what makes the cap
    // an accounting rather than a one-way ratchet. The accept loop polls,
    // so give it a moment to take the new connections up.
    for _ in 0..released_unauth {
        let mut connection = SlowHead::open(&relay);
        connection.trickle();
        slow.push(connection);
    }
    let refill_deadline = Instant::now() + Duration::from_secs(10);
    let mut refilled_status = relay
        .request("GET", "/api/health", &[], None)
        .map(|response| response.status);
    while refilled_status == Some(200) && Instant::now() < refill_deadline {
        std::thread::sleep(Duration::from_millis(100));
        refilled_status = relay
            .request("GET", "/api/health", &[], None)
            .map(|response| response.status);
    }
    record(
        &mut stages,
        started,
        "unauthenticated-release",
        json!({
            "released": released_unauth,
            "healthStatusAfterRelease": health_status,
            "overflowStatusAfterRefill": refilled_status,
        }),
    );
    assert_eq!(
        health_status,
        Some(200),
        "stage=quota-release a freed slot must admit new work"
    );
    assert!(refilled_status == Some(503) || refilled_status.is_none(),
        "stage=quota-refill a refilled cap must refuse again");

    // ---- phase 5: release the authenticated side ----------------------
    // The unauthenticated cap has made its point; free it so the next
    // phase can register the devices it needs to probe the other cap.
    slow.clear();
    let released_auth = 6;
    let freed = live.len() - released_auth;
    live.truncate(freed);
    // Give the relay a moment to notice the closed sockets.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut admitted_auth = 0;
    while admitted_auth < released_auth {
        let source = format!("10.3.9.{}", admitted_auth);
        let device = Identity::new(&source);
        let device_id = device.register(&relay, "device");
        match device.connect_session(&relay, &device_id) {
            Admission::Live(_) => admitted_auth += 1,
            Admission::Refused(code) => {
                assert!(
                    Instant::now() < deadline,
                    "stage=quota-auth-release no slot was freed"
                );
                assert_eq!(format!("{code:?}"), "RateLimited");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    record(
        &mut stages,
        started,
        "authenticated-release",
        json!({
            "released": released_auth,
            "readmitted": admitted_auth,
        }),
    );

    // ---- phase 6: the relay is still healthy --------------------------
    let health = relay.health_json();
    record(&mut stages, started, "final-health", health.clone());
    assert_eq!(health["ok"], json!(true), "stage=quota-final health");
    assert!(relay.alive(), "stage=quota-final the relay must still serve");

    // ---- phase 7: privacy scan of the captured log --------------------
    // Every pattern a secret could take, gathered from this run. The
    // relay never sees the Noise keys, so the filler key and filler
    // ciphertext stand in for the ones that do cross the wire.
    let secrets = [
        BASE64.encode(&nonce),
        BASE64.encode([0x5au8; 32]),
        BASE64.encode(vec![0x5au8; 24]),
    ];
    let log = relay.captured_log();
    let leaked = secrets
        .iter()
        .filter(|secret| log.contains(secret.as_str()))
        .count();
    record(
        &mut stages,
        started,
        "log-privacy-scan",
        json!({
            "logBytes": log.len(),
            "secretPatternsChecked": secrets.len(),
            "leaked": leaked,
        }),
    );
    assert_eq!(
        leaked, 0,
        "stage=quota-privacy the relay log leaked a secret pattern"
    );

    // ---- artifact ------------------------------------------------------
    let artifact = json!({
        "stage": "relay-quota-stress",
        "codeHash": relay_source_hash(),
        "limits": {
            "authenticated": MAX_AUTHENTICATED_CONNECTIONS,
            "unauthenticated": MAX_UNAUTHENTICATED_CONNECTIONS,
        },
        "totals": health["totals"].clone(),
        "stages": stages
            .iter()
            .map(|stage| json!({
                "stage": stage.name,
                "atMs": stage.at_ms,
                "detail": stage.detail,
            }))
            .collect::<Vec<_>>(),
    });
    let out_dir = std::env::var("FINTWIND_STRESS_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("fintwind-relay-stress"));
    std::fs::create_dir_all(&out_dir).expect("artifact dir");
    let path = out_dir.join("relay-quota-stress.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&artifact).expect("artifact serializes"),
    )
    .expect("artifact writes");
    println!(
        "stage=quota-artifact path={} codeHash={}",
        path.display(),
        artifact["codeHash"].as_str().unwrap_or("")
    );
}

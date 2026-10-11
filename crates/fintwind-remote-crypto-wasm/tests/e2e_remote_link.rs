//! Black-box E2E for the wasm crypto bridge, over the real artifact.
//!
//! The device side is the actual wasm module the PWA loads, built by
//! wasm-pack and driven in Node exactly the way `crypto-bridge.ts`
//! drives it; the host peer (tests/common) speaks the same protocol
//! `fintwind-remote-host` speaks. Only the exported bridge API is
//! exercised — no internals — and the assertions are the security
//! semantics the hardening froze (HARDENING.md).
//!
//! Covered:
//! - F5: identity JSON that is not internally consistent is refused.
//! - F1b: a wrong-version, expired, empty-nonce, or malformed invite is
//!   refused before any handshake.
//! - F3: a pairing whose handshake host key differs from the scanned
//!   invite fails, and an honest pairing yields the same check code both
//!   ends compute.
//! - F1/F2/F4: inner version, addressing, and delivery-sequence checks;
//!   the first refusal poisons the channel for good.
//! - F7: a reconnect (KK) binds the dialed host key and exposes its
//!   fingerprint; the wrong host key fails closed on both ends.

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use common::{
    Base64Bytes, ConnectResponder, Device, HostPeer, IdentityKey, PairingResponder,
    REQUEST_SNAPSHOT, fingerprint, new_device_identity, now_ms, pair_ok, pairing_code,
};
use fintwind_remote_protocol::{MessageBody, REMOTE_PROTOCOL_VERSION, SealedMessage};
use serde_json::{Value, json};
use uuid::Uuid;

#[test]
fn identity_json_must_be_internally_consistent() {
    let mut device = Device::spawn();
    let made = device.call_ok(json!({ "cmd": "identity_new" }));
    let identity = made["identity"].as_str().expect("identity json").to_owned();
    let fingerprint = made["fingerprint"].as_str().expect("fingerprint");
    assert_eq!(fingerprint.len(), 32, "fingerprint is 16 bytes of hex");
    assert!(
        fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "fingerprint is lowercase hex"
    );

    // An honest identity loads, repeatedly — the consistency probe must
    // never refuse a keypair that matches.
    device.call_ok(json!({ "cmd": "identity_load", "id": "i1", "identity": identity }));
    device.call_ok(json!({ "cmd": "identity_load", "id": "i2", "identity": identity }));

    let mut value: Value = serde_json::from_str(&identity).expect("identity is json");

    // A noise public key that does not belong to the private key would
    // make this device display the wrong fingerprint and the wrong
    // pairing code forever; it must be refused at load.
    let mut swapped = BASE64
        .decode(value["noisePublic"].as_str().expect("noisePublic"))
        .expect("noisePublic decodes");
    swapped[0] ^= 0x01;
    value["noisePublic"] = json!(BASE64.encode(&swapped));
    let error = device.call_err(json!({
        "cmd": "identity_load",
        "id": "i3",
        "identity": value.to_string(),
    }));
    assert!(
        error.contains("noise public key does not match the private key"),
        "got: {error}"
    );

    // Same for the signing pair: the stored verifying key must belong to
    // the stored signing key instead of being silently re-derived.
    let mut value: Value = serde_json::from_str(&identity).expect("identity is json");
    let mut swapped = BASE64
        .decode(value["verifying"].as_str().expect("verifying"))
        .expect("verifying decodes");
    swapped[0] ^= 0x01;
    value["verifying"] = json!(BASE64.encode(&swapped));
    let error = device.call_err(json!({
        "cmd": "identity_load",
        "id": "i4",
        "identity": value.to_string(),
    }));
    assert!(
        error.contains("verifying key does not match the signing key"),
        "got: {error}"
    );
}

#[test]
fn pairing_start_refuses_unusable_invites() {
    let mut device = Device::spawn();
    let host = HostPeer::new();
    let identity = new_device_identity(&mut device);
    let nonce = host.fresh_nonce();
    let expires = now_ms() + 5 * 60_000;

    let mut wrong_version = host.invite(&host.noise.public, &nonce, expires);
    wrong_version = wrong_version.replace(
        &format!("\"protocolVersion\":{REMOTE_PROTOCOL_VERSION}"),
        "\"protocolVersion\":99",
    );
    let error = device.call_err(json!({
        "cmd": "pairing_start",
        "id": "p",
        "identity": identity,
        "invite": wrong_version,
    }));
    assert!(error.contains("invite protocol version"), "got: {error}");

    let error = device.call_err(json!({
        "cmd": "pairing_start",
        "id": "p",
        "identity": identity,
        "invite": host.invite(&host.noise.public, &nonce, now_ms() - 1),
    }));
    assert!(error.contains("invite has expired"), "got: {error}");

    let error = device.call_err(json!({
        "cmd": "pairing_start",
        "id": "p",
        "identity": identity,
        "invite": host.invite(&host.noise.public, &[], expires),
    }));
    assert!(error.contains("invite nonce is empty"), "got: {error}");

    let error = device.call_err(json!({
        "cmd": "pairing_start",
        "id": "p",
        "identity": identity,
        "invite": host.invite(&[0u8; 16], &nonce, expires),
    }));
    assert!(
        error.contains("invite host public key is not 32 bytes"),
        "got: {error}"
    );
}

#[test]
fn pairing_binds_the_invite_host_key_and_shows_one_check_code() {
    let mut device = Device::spawn();
    let host = HostPeer::new();
    let identity = new_device_identity(&mut device);
    let nonce = host.fresh_nonce();
    let expires = now_ms() + 5 * 60_000;

    // Honest pairing: the invite claims the host's real key.
    let pairing = pair_ok(
        &mut device,
        &host,
        &identity,
        &host.noise.public,
        &nonce,
        expires,
    );
    // Both ends derive the same six digits from the same two keys and
    // the same invite binding — the number the user compares.
    let expected = pairing_code(
        &host.noise.public,
        &pairing.device_public,
        &host.binding(&nonce, expires),
    );
    assert_eq!(pairing.check_code, expected);
    assert_eq!(pairing.check_code.len(), 6);
    assert_eq!(pairing.host_fingerprint, fingerprint(&host.noise.public));

    // Substitution: the QR claims a host key the handshake does not
    // present. The device must refuse instead of pairing to whoever
    // answered.
    let impostor = IdentityKey::generate().expect("impostor key");
    let substituted_nonce = host.fresh_nonce();
    let invite = host.invite(&impostor.public, &substituted_nonce, expires);
    let start = device.call_ok(json!({
        "cmd": "pairing_start",
        "id": "pairing2",
        "identity": identity,
        "invite": invite,
    }));
    let message1 = BASE64
        .decode(start["step0"].as_str().expect("step0"))
        .expect("step0 decodes");
    let binding = host.binding(&substituted_nonce, expires);
    let mut responder = PairingResponder::new(&binding, &host.noise).expect("responder builds");
    responder.read_first(&message1).expect("xx message 1");
    let (message2, _final_step) = responder.write_reply(b"").expect("xx message 2");
    let error = device.call_err(json!({
        "cmd": "pairing_finish",
        "id": "pairing2",
        "channel_id": "channel2",
        "reply": BASE64.encode(&message2),
        "device_id": host.device_id.to_string(),
        "device_name": "e2e-device",
    }));
    assert!(
        error.contains("does not match the scanned invite"),
        "a substituted invite host key must refuse the pairing, got: {error}"
    );
}

#[test]
fn sealed_frames_are_checked_inside_the_aead_and_fail_closed() {
    let mut device = Device::spawn();
    let host = HostPeer::new();
    let identity = new_device_identity(&mut device);
    let expires = now_ms() + 5 * 60_000;

    // Honest traffic in both directions on a fresh pairing.
    let mut pairing = pair_ok(
        &mut device,
        &host,
        &identity,
        &host.noise.public,
        &host.fresh_nonce(),
        expires,
    );
    let frame = device.call_ok(json!({
        "cmd": "channel_seal",
        "id": pairing.channel_id,
        "seq": 1,
        "body": REQUEST_SNAPSHOT,
    }))["frame"]
        .as_str()
        .expect("frame json")
        .to_owned();
    let opened = host.host_open(&mut pairing.host_channel, &frame);
    assert!(
        matches!(opened.body, MessageBody::RequestSnapshot),
        "honest device traffic must open on the host"
    );
    let reply = host.host_seal(
        &mut pairing.host_channel,
        2,
        &host.sealed_pairing(MessageBody::Acknowledged { upto_seq: 1 }),
    );
    let body = device.call_ok(json!({
        "cmd": "channel_open",
        "id": pairing.channel_id,
        "frame": reply,
    }))["body"]
        .as_str()
        .expect("body json")
        .to_owned();
    assert_eq!(body, r#"{"type":"acknowledged","uptoSeq":1}"#);

    // The pairing channel keeps working after the handshake: the host's
    // decision rides it with the pairing convention (no delivery
    // sequence) whatever seq the outer frame carries.
    let decision = host.host_seal(
        &mut pairing.host_channel,
        4_242,
        &host.sealed_pairing(MessageBody::PairingDecision {
            accepted: true,
            reason: None,
        }),
    );
    let body = device.call_ok(json!({
        "cmd": "channel_open",
        "id": pairing.channel_id,
        "frame": decision,
    }))["body"]
        .as_str()
        .expect("body json")
        .to_owned();
    assert_eq!(
        body,
        r#"{"type":"pairingDecision","accepted":true,"reason":null}"#
    );

    // A caller-side input error happens before any crypto state moves:
    // it must not poison the channel.
    let error = device.call_err(json!({
        "cmd": "channel_seal",
        "id": pairing.channel_id,
        "seq": 3,
        "body": "not json",
    }));
    assert!(error.contains("body json"), "got: {error}");
    device.call_ok(json!({
        "cmd": "channel_seal",
        "id": pairing.channel_id,
        "seq": 4,
        "body": REQUEST_SNAPSHOT,
    }));

    // Each inner-envelope attack gets a fresh channel: the first
    // refusal poisons it, which is the point of the mechanism.
    let attacks: [(&str, fn(&mut SealedMessage), &str); 3] = [
        (
            "inner protocol version",
            |sealed: &mut SealedMessage| sealed.protocol_version = 99,
            "sealed message protocol version mismatch",
        ),
        (
            "inner host id",
            |sealed: &mut SealedMessage| sealed.host_id = Uuid::nil(),
            "sealed message claims another host",
        ),
        (
            "inner device id",
            |sealed: &mut SealedMessage| sealed.device_id = Uuid::nil(),
            "sealed message claims another device",
        ),
    ];
    for (name, tamper, expected) in attacks {
        let mut pairing = pair_ok(
            &mut device,
            &host,
            &identity,
            &host.noise.public,
            &host.fresh_nonce(),
            expires,
        );
        let mut tampered = host.sealed_pairing(MessageBody::RequestSnapshot);
        tamper(&mut tampered);
        let wire = host.host_seal(&mut pairing.host_channel, 2, &tampered);
        let error = device.call_err(json!({
            "cmd": "channel_open",
            "id": pairing.channel_id,
            "frame": wire,
        }));
        assert!(
            error.contains(expected),
            "{name}: expected '{expected}', got: {error}"
        );
        // The channel is dead in both directions, even for a caller
        // that ignored the error.
        let sealed_again = device.call_err(json!({
            "cmd": "channel_seal",
            "id": pairing.channel_id,
            "seq": 3,
            "body": REQUEST_SNAPSHOT,
        }));
        assert!(
            sealed_again.contains("channel poisoned"),
            "{name}: a poisoned channel must refuse to seal, got: {sealed_again}"
        );
        let opened_again = device.call_err(json!({
            "cmd": "channel_open",
            "id": pairing.channel_id,
            "frame": r#"{"type":"sealed","seq":9,"hostId":"x"}"#,
        }));
        assert!(
            opened_again.contains("channel poisoned"),
            "{name}: a poisoned channel must refuse to open, got: {opened_again}"
        );
    }

    // An oversize frame is refused before parsing, and poisons too.
    let pairing = pair_ok(
        &mut device,
        &host,
        &identity,
        &host.noise.public,
        &host.fresh_nonce(),
        expires,
    );
    let oversize = "a".repeat(1_100_000);
    let error = device.call_err(json!({
        "cmd": "channel_open",
        "id": pairing.channel_id,
        "frame": oversize,
    }));
    assert!(
        error.contains("exceeds the maximum frame size"),
        "got: {error}"
    );
    let sealed_again = device.call_err(json!({
        "cmd": "channel_seal",
        "id": pairing.channel_id,
        "seq": 1,
        "body": REQUEST_SNAPSHOT,
    }));
    assert!(
        sealed_again.contains("channel poisoned"),
        "got: {sealed_again}"
    );
}

#[test]
fn reconnect_binds_the_dialed_host_key() {
    let mut device = Device::spawn();
    let host = HostPeer::new();
    let identity = new_device_identity(&mut device);
    let expires = now_ms() + 5 * 60_000;
    let pairing = pair_ok(
        &mut device,
        &host,
        &identity,
        &host.noise.public,
        &host.fresh_nonce(),
        expires,
    );
    let host_public_b64 = BASE64.encode(&host.noise.public);

    // Dialing the wrong host key: the host side refuses message 1.
    let impostor = IdentityKey::generate().expect("impostor key");
    let start = device.call_ok(json!({
        "cmd": "connect_start",
        "id": "wrong",
        "identity": identity,
        "host_public": BASE64.encode(&impostor.public),
        "host_id": host.host_id.to_string(),
        "device_id": host.device_id.to_string(),
    }));
    let message1 = BASE64
        .decode(start["step0"].as_str().expect("step0"))
        .expect("step0 decodes");
    let mut responder = ConnectResponder::new(
        REMOTE_PROTOCOL_VERSION,
        &host.noise,
        &pairing.device_public,
        host.host_id,
        host.device_id,
    )
    .expect("responder builds");
    assert!(
        responder.read_first(&message1).is_err(),
        "KK against the wrong host static key must fail closed"
    );

    // A garbage reply on the device side fails closed too.
    device.call_ok(json!({
        "cmd": "connect_start",
        "id": "bogus",
        "identity": identity,
        "host_public": host_public_b64,
        "host_id": host.host_id.to_string(),
        "device_id": host.device_id.to_string(),
    }));
    let error = device.call_err(json!({
        "cmd": "connect_finish",
        "id": "bogus",
        "reply": BASE64.encode(b"not a handshake message"),
    }));
    assert!(error.contains("connect message 2"), "got: {error}");

    // Honest reconnect: a fresh key period over the same host key.
    let start = device.call_ok(json!({
        "cmd": "connect_start",
        "id": "kk",
        "identity": identity,
        "host_public": host_public_b64,
        "host_id": host.host_id.to_string(),
        "device_id": host.device_id.to_string(),
    }));
    let message1 = BASE64
        .decode(start["step0"].as_str().expect("step0"))
        .expect("step0 decodes");
    let mut responder = ConnectResponder::new(
        REMOTE_PROTOCOL_VERSION,
        &host.noise,
        &pairing.device_public,
        host.host_id,
        host.device_id,
    )
    .expect("responder builds");
    responder.read_first(&message1).expect("kk message 1");
    let (reply, mut host_channel) = responder.finish(b"host-ready").expect("kk message 2");
    let done = device.call_ok(json!({
        "cmd": "connect_finish",
        "id": "kk",
        "channel_id": "kk-channel",
        "reply": BASE64.encode(&reply),
    }));
    assert_eq!(
        done["static_b64"].as_str(),
        Some(host_public_b64.as_str()),
        "the channel must bind the dialed host key"
    );
    assert_eq!(
        done["fingerprint"].as_str(),
        Some(fingerprint(&host.noise.public).as_str()),
        "the fingerprint must be the paired host's"
    );

    // Traffic flows on the new key period.
    let frame = device.call_ok(json!({
        "cmd": "channel_seal",
        "id": "kk-channel",
        "seq": 1,
        "body": REQUEST_SNAPSHOT,
    }))["frame"]
        .as_str()
        .expect("frame json")
        .to_owned();
    let opened = host.host_open(&mut host_channel, &frame);
    assert!(matches!(opened.body, MessageBody::RequestSnapshot));
    let reply = host.host_seal(
        &mut host_channel,
        2,
        &host.sealed(2, MessageBody::Acknowledged { upto_seq: 1 }),
    );
    device.call_ok(json!({
        "cmd": "channel_open",
        "id": "kk-channel",
        "frame": reply,
    }));

    // A fragment carries two sequences: the envelope's, which must equal
    // the frame's, and the fragment's own logical number, which is part
    // of the message and has nothing to do with the frame. Conflating
    // them would either drop or mis-deliver large messages, so this is
    // the case that must be *accepted* — and accepted with its logical
    // number intact.
    let fragmented = host.host_seal(
        &mut host_channel,
        7,
        &host.sealed(
            7,
            MessageBody::Fragment {
                transport_seq: 4_242,
                index: 0,
                of: 2,
                data: Base64Bytes(vec![0x5a; 32]),
            },
        ),
    );
    let body = device.call_ok(json!({
        "cmd": "channel_open",
        "id": "kk-channel",
        "frame": fragmented,
    }))["body"]
        .as_str()
        .expect("body json")
        .to_owned();
    let parsed: Value = serde_json::from_str(&body).expect("body parses");
    assert_eq!(
        parsed["transportSeq"].as_u64(),
        Some(4_242),
        "the fragment's logical sequence must survive untouched"
    );
    assert_eq!(parsed["of"].as_u64(), Some(2), "fragment metadata intact");

    // A relay that renumbers the frame in transit must be caught: the
    // authenticated copy of the delivery sequence no longer matches.
    let renumbered = host.host_seal(
        &mut host_channel,
        99,
        &host.sealed(2, MessageBody::Acknowledged { upto_seq: 2 }),
    );
    let error = device.call_err(json!({
        "cmd": "channel_open",
        "id": "kk-channel",
        "frame": renumbered,
    }));
    assert!(
        error.contains("transport_seq does not match the frame"),
        "a renumbered frame must be refused, got: {error}"
    );
    let sealed_again = device.call_err(json!({
        "cmd": "channel_seal",
        "id": "kk-channel",
        "seq": 3,
        "body": REQUEST_SNAPSHOT,
    }));
    assert!(
        sealed_again.contains("channel poisoned"),
        "a renumbered frame must poison the channel, got: {sealed_again}"
    );
}

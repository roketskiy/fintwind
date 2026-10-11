mod common;

use common::{
    Device, HostPeer, REQUEST_SNAPSHOT, fresh_bytes, new_device_identity, now_ms, pair_ok_as,
};
use fintwind_remote_protocol::MessageBody;
use serde_json::json;
use uuid::Uuid;

/// Replay, ciphertext from another key period or another device, a
/// version downgrade, and malformed input: every integrity failure
/// closes the channel for good. The one case that must be *accepted* —
/// a fragment whose logical sequence differs from its frame sequence —
/// is checked on the session channel in the reconnect test.
///
/// Every case runs on its own pairing, because the first failure closes
/// the channel it happened on — which is itself part of what is being
/// asserted.
#[test]
fn replay_foreign_and_malformed_frames_close_the_channel() {
    let mut device = Device::spawn();
    let host = HostPeer::new();
    let identity = new_device_identity(&mut device);
    let expires = now_ms() + 5 * 60_000;

    // ---- replay: the same ciphertext twice ----------------------------
    let mut pairing = pair_ok_as(
        &mut device,
        &host,
        &identity,
        &host.noise.public,
        &host.fresh_nonce(),
        expires,
        "replay",
    )
    .0;
    let wire = host.host_seal(
        &mut pairing.host_channel,
        1,
        &host.sealed_pairing(MessageBody::Acknowledged { upto_seq: 1 }),
    );
    device.call_ok(json!({
        "cmd": "channel_open",
        "id": pairing.channel_id,
        "frame": wire,
    }));
    // Delivered again: the transport nonce has moved on, so the replay
    // must fail and the channel must not survive it.
    let error = device.call_err(json!({
        "cmd": "channel_open",
        "id": pairing.channel_id,
        "frame": wire,
    }));
    assert!(
        error.contains("decrypt failed"),
        "a replayed frame must fail to decrypt, got: {error}"
    );
    let sealed_again = device.call_err(json!({
        "cmd": "channel_seal",
        "id": pairing.channel_id,
        "seq": 2,
        "body": REQUEST_SNAPSHOT,
    }));
    assert!(
        sealed_again.contains("channel poisoned"),
        "a replay must poison the channel, got: {sealed_again}"
    );

    // ---- another key period: same host, second pairing ----------------
    // A second pairing with the same host runs a fresh XX exchange, so
    // its transport keys differ. Ciphertext from it must not open on the
    // first channel even though the addressing is identical.
    let mut second = pair_ok_as(
        &mut device,
        &host,
        &identity,
        &host.noise.public,
        &host.fresh_nonce(),
        expires,
        "second",
    )
    .0;
    let foreign = host.host_seal(
        &mut second.host_channel,
        1,
        &host.sealed_pairing(MessageBody::Acknowledged { upto_seq: 1 }),
    );
    let error = device.call_err(json!({
        "cmd": "channel_open",
        "id": pairing.channel_id,
        "frame": foreign,
    }));
    assert!(
        error.contains("decrypt failed"),
        "ciphertext from another key period must not open, got: {error}"
    );

    // ---- another device: same host, different paired device ----------
    // A second device pairs with the same host; the host seals for it,
    // and a relay rewrites the outer addressing to this device. The
    // outer checks pass, the ciphertext does not.
    let other_peer = host.for_device(Uuid::from_bytes(fresh_bytes()));
    let other_identity = new_device_identity(&mut device);
    let mut other_pairing = pair_ok_as(
        &mut device,
        &other_peer,
        &other_identity,
        &host.noise.public,
        &host.fresh_nonce(),
        expires,
        "other-device",
    )
    .0;
    let cross_device = other_peer.host_seal_readdressed(
        &mut other_pairing.host_channel,
        3,
        &other_peer.sealed_pairing(MessageBody::Acknowledged { upto_seq: 3 }),
        host.host_id,
        Some(host.device_id),
    );
    let error = device.call_err(json!({
        "cmd": "channel_open",
        "id": pairing.channel_id,
        "frame": cross_device,
    }));
    assert!(
        error.contains("decrypt failed"),
        "ciphertext sealed for another device must not open, got: {error}"
    );

    // ---- version downgrade --------------------------------------------
    let mut third = pair_ok_as(
        &mut device,
        &host,
        &identity,
        &host.noise.public,
        &host.fresh_nonce(),
        expires,
        "downgrade",
    )
    .0;
    let mut downgraded = host.sealed_pairing(MessageBody::Acknowledged { upto_seq: 1 });
    downgraded.protocol_version = 1;
    let wire = host.host_seal(&mut third.host_channel, 1, &downgraded);
    let error = device.call_err(json!({
        "cmd": "channel_open",
        "id": third.channel_id,
        "frame": wire,
    }));
    assert!(
        error.contains("protocol version mismatch"),
        "a downgraded inner version must be refused, got: {error}"
    );

    // ---- malformed input closes the channel for good ------------------
    let mut fourth = pair_ok_as(
        &mut device,
        &host,
        &identity,
        &host.noise.public,
        &host.fresh_nonce(),
        expires,
        "malformed",
    )
    .0;
    let error = device.call_err(json!({
        "cmd": "channel_open",
        "id": fourth.channel_id,
        "frame": "this is not json",
    }));
    assert!(
        error.contains("frame json") || error.contains("channel poisoned"),
        "malformed input must be refused, got: {error}"
    );
    // A perfectly good frame on the same channel must not recover it.
    let good = host.host_seal(
        &mut fourth.host_channel,
        1,
        &host.sealed_pairing(MessageBody::Acknowledged { upto_seq: 1 }),
    );
    let error = device.call_err(json!({
        "cmd": "channel_open",
        "id": fourth.channel_id,
        "frame": good,
    }));
    assert!(
        error.contains("channel poisoned"),
        "a malformed frame must close the channel for good, got: {error}"
    );
}

//! Shared black-box harness for the wasm bridge E2E: the device side is
//! the real wasm artifact driven in Node, and this is the host peer that
//! speaks the same wire protocol `fintwind-remote-host` speaks.
//!
//! Assertions report stage names and reject codes only. No private key,
//! nonce, signature, or ciphertext ever reaches an assertion message.
#![allow(dead_code)]
//! Black-box E2E for the wasm crypto bridge, over the real artifact.
//!
//! The device side is the actual wasm module the PWA loads, built by
//! wasm-pack and driven in Node exactly the way `crypto-bridge.ts`
//! drives it; this test is the host peer, speaking the same wire
//! protocol `fintwind-remote-host` speaks. Only the exported bridge API
//! is exercised — no internals — and the assertions are the security
//! semantics the hardening froze (HARDENING.md):
//!
//! - identity JSON that is not internally consistent is refused;
//! - a pairing whose handshake host key differs from the scanned
//!   invite fails, and an honest pairing yields the same check code
//!   both ends compute;
//! - sealed frames whose authenticated inner version or addressing
//!   does not name this channel are refused, and the first refusal
//!   poisons the channel for good;
//! - a reconnect (KK) binds the dialed host key and exposes its
//!   fingerprint; the wrong host key fails closed on both ends.
//!
//! Run through tests/e2e/run-e2e.ps1, which builds the wasm artifact
//! first and points FINTWIND_E2E_WASM_DIR at it.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// What a test file reaches for. The unused re-exports are the ones only
/// some attack cases touch, and they keep every test file to one import.
#[allow(unused_imports)]
pub use fintwind_remote_crypto::{
    fingerprint, pairing_code, ConnectResponder, IdentityKey, PairingBinding, PairingResponder,
    SecureChannel,
};
pub use fintwind_remote_protocol::{Base64Bytes, Frame, MessageBody, SealedMessage};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fintwind_remote_protocol::{PairingInvite, REMOTE_PROTOCOL_VERSION};
use serde_json::{Value, json};
use uuid::Uuid;

const E2E_HINT: &str =
    "build the artifact first: pwsh -File crates/fintwind-remote-crypto-wasm/tests/e2e/run-e2e.ps1";

/// The device side: the real wasm bridge inside Node, one JSON line per
/// command in, one JSON line per result out.
pub struct Device {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Device {
    pub fn spawn() -> Self {
        let glue_dir =
            std::env::var("FINTWIND_E2E_WASM_DIR").unwrap_or_else(|_| panic!("{E2E_HINT}"));
        for name in ["fintwind-crypto.js", "fintwind-crypto_bg.wasm"] {
            let path = PathBuf::from(&glue_dir).join(name);
            assert!(path.is_file(), "missing {} — {E2E_HINT}", path.display());
        }
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/device.mjs");
        let mut child = Command::new("node")
            .arg(&script)
            .arg(&glue_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("node spawns; node must be on PATH");
        let stdin = child.stdin.take().expect("device stdin");
        let stdout = BufReader::new(child.stdout.take().expect("device stdout"));
        Self {
            _child: child,
            stdin,
            stdout,
        }
    }

    /// Bridge failures arrive as `ok: false` carrying the JS exception
    /// message — the same error the PWA's catch block would see.
    pub fn call(&mut self, command: Value) -> Value {
        let line = serde_json::to_string(&command).expect("command serializes");
        writeln!(self.stdin, "{line}").expect("write to device");
        self.stdin.flush().expect("flush to device");
        let mut response = String::new();
        self.stdout
            .read_line(&mut response)
            .expect("device responds");
        assert!(!response.is_empty(), "device closed the stream");
        serde_json::from_str(&response).expect("device responds with json")
    }

    pub fn call_ok(&mut self, command: Value) -> Value {
        let response = self.call(command.clone());
        assert_eq!(
            response["ok"],
            json!(true),
            "device refused {command}: {:?}",
            response["error"]
        );
        response
    }

    pub fn call_err(&mut self, command: Value) -> String {
        let response = self.call(command.clone());
        assert_eq!(
            response["ok"],
            json!(false),
            "device unexpectedly accepted {command}"
        );
        response["error"].as_str().unwrap_or_default().to_owned()
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_millis() as u64)
        .unwrap_or(0)
}

/// A routing identifier for this run. The nonce and ids only have to be
/// unique inside one pairing; they are public by design.
pub fn fresh_bytes() -> [u8; 16] {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_nanos())
        .unwrap_or(0);
    nanos.to_le_bytes()
}

/// The host peer: the role `fintwind-remote-host` plays in production.
pub struct HostPeer {
    pub noise: IdentityKey,
    pub host_id: Uuid,
    pub device_id: Uuid,
    pub relay_origin: String,
}

impl HostPeer {
    pub fn new() -> Self {
        Self {
            noise: IdentityKey::generate().expect("host noise key"),
            host_id: Uuid::from_bytes(fresh_bytes()),
            device_id: Uuid::from_bytes(fresh_bytes()),
            relay_origin: "https://relay.fintwind.xyz".to_owned(),
        }
    }

    /// The same host identity addressing a different device: what a
    /// second paired device looks like to this host.
    pub fn for_device(&self, device_id: Uuid) -> Self {
        Self {
            noise: self.noise.clone(),
            host_id: self.host_id,
            device_id,
            relay_origin: self.relay_origin.clone(),
        }
    }

    pub fn fresh_nonce(&self) -> [u8; 32] {
        let mut nonce = [0u8; 32];
        nonce[..16].copy_from_slice(self.host_id.as_bytes());
        nonce[16..].copy_from_slice(&fresh_bytes());
        nonce
    }

    pub fn binding(&self, nonce: &[u8], expires_at_ms: u64) -> PairingBinding {
        PairingBinding {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            host_id: self.host_id,
            nonce: nonce.to_vec(),
            expires_at_ms,
            relay_origin: self.relay_origin.clone(),
        }
    }

    /// The invite the host renders as a QR. `claimed_host_public` is
    /// what the QR text says; the substitution attack passes a key the
    /// host does not hold.
    pub fn invite(&self, claimed_host_public: &[u8], nonce: &[u8], expires_at_ms: u64) -> String {
        serde_json::to_string(&PairingInvite {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            relay_origin: self.relay_origin.clone(),
            host_id: self.host_id,
            host_public_key: BASE64.encode(claimed_host_public),
            nonce: BASE64.encode(nonce),
            expires_at_ms,
        })
        .expect("invite serializes")
    }

    pub fn host_seal(&self, channel: &mut SecureChannel, seq: u64, sealed: &SealedMessage) -> String {
        let plaintext = serde_json::to_vec(sealed).expect("sealed encodes");
        let ciphertext = channel.encrypt(&plaintext).expect("host encrypts");
        serde_json::to_string(&Frame::Sealed {
            seq,
            host_id: self.host_id,
            device_id: Some(self.device_id),
            ciphertext: Base64Bytes(ciphertext),
        })
        .expect("frame encodes")
    }

    /// Seal with outer routing metadata that differs from what the
    /// ciphertext was produced for — exactly what a relay rewriting
    /// addressing in transit looks like.
    pub fn host_seal_readdressed(
        &self,
        channel: &mut SecureChannel,
        seq: u64,
        sealed: &SealedMessage,
        outer_host_id: Uuid,
        outer_device_id: Option<Uuid>,
    ) -> String {
        let plaintext = serde_json::to_vec(sealed).expect("sealed encodes");
        let ciphertext = channel.encrypt(&plaintext).expect("host encrypts");
        serde_json::to_string(&Frame::Sealed {
            seq,
            host_id: outer_host_id,
            device_id: outer_device_id,
            ciphertext: Base64Bytes(ciphertext),
        })
        .expect("frame encodes")
    }

    pub fn host_open(&self, channel: &mut SecureChannel, frame_json: &str) -> SealedMessage {
        let frame: Frame = serde_json::from_str(frame_json).expect("device frame parses");
        let Frame::Sealed { ciphertext, .. } = frame else {
            panic!("device sent a non-sealed frame");
        };
        let plaintext = channel
            .decrypt(&ciphertext.0)
            .expect("device ciphertext opens");
        serde_json::from_slice(&plaintext).expect("device sealed message decodes")
    }

    pub fn sealed(&self, seq: u64, body: MessageBody) -> SealedMessage {
        SealedMessage {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            // The authenticated copy of the frame's delivery sequence.
            transport_seq: seq,
            host_id: self.host_id,
            device_id: self.device_id,
            sent_at_ms: now_ms(),
            body,
        }
    }

    /// A pairing payload: the pairing channel has no delivery sequence,
    /// so both ends use 0 by convention while the outer frame carries
    /// whatever seq the sender chose.
    pub fn sealed_pairing(&self, body: MessageBody) -> SealedMessage {
        SealedMessage {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            transport_seq: 0,
            host_id: self.host_id,
            device_id: self.device_id,
            sent_at_ms: now_ms(),
            body,
        }
    }
}

pub struct PairingOk {
    pub channel_id: String,
    pub check_code: String,
    pub host_fingerprint: String,
    pub host_channel: SecureChannel,
    pub device_public: Vec<u8>,
}

/// One full XX pairing between the device bridge and this host, the way
/// the relay carries it: invite, message 1, message 2, hello in message
/// 3, channel on both ends.
pub fn pair_ok(
    device: &mut Device,
    host: &HostPeer,
    identity_json: &str,
    claimed_host_public: &[u8],
    nonce: &[u8],
    expires_at_ms: u64,
) -> PairingOk {
    pair_ok_as(
        device,
        host,
        identity_json,
        claimed_host_public,
        nonce,
        expires_at_ms,
        "channel",
    )
    .0
}

/// The same pairing, but naming the channel id the harness hands back.
/// Every pairing registers a channel of its own, so several pairings can
/// coexist and be attacked independently.
pub fn pair_ok_as(
    device: &mut Device,
    host: &HostPeer,
    identity_json: &str,
    claimed_host_public: &[u8],
    nonce: &[u8],
    expires_at_ms: u64,
    channel_id: &str,
) -> (PairingOk, String) {
    let invite = host.invite(claimed_host_public, nonce, expires_at_ms);
    let start = device.call_ok(json!({
        "cmd": "pairing_start",
        "id": "pairing",
        "identity": identity_json,
        "invite": invite,
    }));
    assert_eq!(
        start["host_id"].as_str(),
        Some(host.host_id.to_string()).as_deref(),
        "pairing must bind the invite's host id"
    );
    let message1 = BASE64
        .decode(start["step0"].as_str().expect("step0 is base64"))
        .expect("step0 decodes");

    let binding = host.binding(nonce, expires_at_ms);
    let mut responder = PairingResponder::new(&binding, &host.noise).expect("responder builds");
    responder.read_first(&message1).expect("xx message 1");
    let (message2, final_step) = responder.write_reply(b"").expect("xx message 2");

    let finish = device.call_ok(json!({
        "cmd": "pairing_finish",
        "id": "pairing",
        "channel_id": channel_id,
        "reply": BASE64.encode(&message2),
        "device_id": host.device_id.to_string(),
        "device_name": "e2e-device",
    }));
    let message3 = BASE64
        .decode(finish["message3"].as_str().expect("message3 is base64"))
        .expect("message3 decodes");
    let (hello_bytes, host_channel) = final_step.finish(&message3).expect("xx message 3");
    let sealed: SealedMessage = serde_json::from_slice(&hello_bytes).expect("hello decodes");
    assert!(
        matches!(sealed.body, MessageBody::PairingHello { .. }),
        "the first sealed message must be the pairing hello"
    );
    let device_public = host_channel
        .remote_static()
        .expect("xx binds the peer static key")
        .to_vec();
    (
        PairingOk {
            channel_id: channel_id.to_owned(),
            check_code: finish["check_code"].as_str().unwrap_or_default().to_owned(),
            host_fingerprint: finish["host_fingerprint"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            host_channel,
            device_public,
        },
        channel_id.to_owned(),
    )
}

pub fn new_device_identity(device: &mut Device) -> String {
    device.call_ok(json!({ "cmd": "identity_new" }))["identity"]
        .as_str()
        .expect("identity json")
        .to_owned()
}

pub const REQUEST_SNAPSHOT: &str = r#"{"type":"requestSnapshot"}"#;

//! The wasm bridge for the PWA: the smallest surface that lets TypeScript
//! register a device, run the pairing and connect handshakes, and move
//! sealed JSON messages over the relay — without ever touching bytes.
//!
//! Everything here speaks base64 or full frame JSON; the protocol's serde
//! layer is the single encoder, so the PWA cannot drift from the wire
//! format the desktop speaks.
//!
//! Before any code, the failure modes this surface must refuse (full
//! list with triggers and residual risk: HARDENING.md):
//!
//! 1. A sealed message whose authenticated inner version, host or device
//!    is not this channel's must be refused — the outer routing fields
//!    are relay-visible and forgeable, the inner ones are not.
//! 2. A pairing whose handshake host static key differs from the scanned
//!    invite's must fail: a QR claiming one host key while the handshake
//!    presents another pairs the device to an impostor.
//! 3. An identity loaded from JSON must be internally consistent: the
//!    verifying key belongs to the signing key, and the noise public key
//!    belongs to the noise private key (proven by one throwaway KK
//!    exchange of the identity against itself — the frozen handshake is
//!    the verifier, no second crypto stack).
//! 4. After any integrity failure the channel is poisoned and every
//!    later call fails closed, so JavaScript that ignores the thrown
//!    error cannot keep using a compromised session. The only recovery
//!    is a fresh connect: a new key period.
//! 5. The authenticated delivery sequence must match the frame it rode
//!    in: the outer `seq` is relay-visible and tamperable, so the
//!    payload repeats it as `transport_seq` and a mismatch — including
//!    a relay that renumbers frames in transit — is refused. The
//!    pairing channel is the documented exception and uses 0.
//!
//! No hand-rolled cryptography and no JS-side Noise anywhere: every
//! primitive is `snow` through `fintwind-remote-crypto`, the same
//! audited code the host runs.

use base64::Engine as _;
use uuid::Uuid;
use wasm_bindgen::prelude::*;

use fintwind_remote_crypto::{
    ConnectInitiator, ConnectResponder, PairingBinding, PairingInitiator, SecureChannel,
    SigningKey, fingerprint, pairing_code,
};
use fintwind_remote_protocol::{
    Base64Bytes, Frame, MAX_FRAME_BYTES, MessageBody, PairingInvite, REMOTE_PROTOCOL_VERSION,
    SealedMessage,
};

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(value: &str) -> Result<Vec<u8>, JsError> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| JsError::new(&format!("bad base64: {error}")))
}

fn json_error(context: &str, error: impl std::fmt::Display) -> JsError {
    JsError::new(&format!("{context}: {error}"))
}

/// Raw length of an X25519 or Ed25519 public key.
const KEY_BYTES: usize = 32;

/// Prove a stored noise keypair is consistent by running one throwaway KK
/// exchange of the identity against itself. In KK message 1
/// (`-> e, es, s, ss`) the `es` token mixes the initiator's ephemeral
/// with the responder's static key, so a substituted public key fails
/// the MAC before anything completes; and the static the responder
/// learns from the handshake must equal the stored public key exactly.
/// The frozen Noise implementation is the verifier — no second crypto
/// stack, no new dependency. Any failure means "refuse this identity".
fn noise_pair_is_consistent(identity: &fintwind_remote_crypto::IdentityKey) -> bool {
    let build = || -> Option<(ConnectInitiator, ConnectResponder)> {
        let initiator = ConnectInitiator::new(
            REMOTE_PROTOCOL_VERSION,
            identity,
            &identity.public,
            Uuid::nil(),
            Uuid::nil(),
        )
        .ok()?;
        let responder = ConnectResponder::new(
            REMOTE_PROTOCOL_VERSION,
            identity,
            &identity.public,
            Uuid::nil(),
            Uuid::nil(),
        )
        .ok()?;
        Some((initiator, responder))
    };
    let Some((mut initiator, mut responder)) = build() else {
        return false;
    };
    let Ok(first) = initiator.write_first() else {
        return false;
    };
    if responder.read_first(&first).is_err() {
        return false;
    }
    let Ok((reply, responder_channel)) = responder.finish(b"") else {
        return false;
    };
    // The initiator only completes against the claimed public key; the
    // responder only completes against the true static the private key
    // belongs to.
    if initiator.finish(&reply).is_err() {
        return false;
    }
    responder_channel.remote_static() == Some(identity.public.as_slice())
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// Which delivery-sequence convention a channel speaks.
///
/// The outer `Frame::Sealed.seq` is relay-visible and therefore
/// tamperable, so the authenticated payload repeats it as
/// `SealedMessage.transport_seq` and the receiver checks the two against
/// each other. The pairing channel is the one exception, by convention
/// on both ends: it has no delivery sequence, so its payloads carry `0`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChannelKind {
    Pairing,
    Session,
}

impl ChannelKind {
    /// What `transport_seq` a received frame must carry.
    fn expected_transport_seq(self, frame_seq: u64) -> u64 {
        match self {
            Self::Pairing => 0,
            Self::Session => frame_seq,
        }
    }

    /// What `transport_seq` an outgoing body must carry.
    fn sealing_transport_seq(self, seq: u64) -> u64 {
        match self {
            Self::Pairing => 0,
            Self::Session => seq,
        }
    }
}

/// The device identity: an Ed25519 signing key (relay authentication) and
/// an X25519 Noise identity (end-to-end channel). Serialized as plain JSON
/// so the PWA can persist it; losing it means re-pairing, by design.
#[wasm_bindgen]
pub struct Identity {
    signing: SigningKey,
    noise: fintwind_remote_crypto::IdentityKey,
}

#[wasm_bindgen]
impl Identity {
    #[wasm_bindgen(constructor)]
    pub fn generate() -> Result<Identity, JsError> {
        Ok(Identity {
            signing: SigningKey::generate(),
            noise: fintwind_remote_crypto::IdentityKey::generate()
                .map_err(|error| json_error("key generation", error))?,
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::json!({
            "signing": b64(&self.signing.signing),
            "verifying": b64(&self.signing.verifying),
            "noisePublic": b64(&self.noise.public),
            "noiseSecret": b64(&self.noise.private),
        })
        .to_string()
    }

    pub fn from_json(json: &str) -> Result<Identity, JsError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|error| json_error("identity json", error))?;
        let field = |name: &str| -> Result<Vec<u8>, JsError> {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| JsError::new(&format!("identity missing {name}")))
                .and_then(|text| unb64(&text))
        };
        let signing_bytes = field("signing")?;
        let verifying_bytes = field("verifying")?;
        let signing = SigningKey::from_bytes(signing_bytes)
            .map_err(|error| json_error("signing key", error))?;
        // The stored verifying key is not decorative: if it does not
        // belong to the signing key the file was edited or corrupted,
        // and silently deriving the right one would hide that.
        if signing.verifying != verifying_bytes {
            return Err(JsError::new(
                "identity verifying key does not match the signing key",
            ));
        }
        let noise = fintwind_remote_crypto::IdentityKey::from_bytes(
            field("noisePublic")?,
            field("noiseSecret")?,
        )
        .map_err(|error| json_error("noise key", error))?;
        // A stored noise public key that does not belong to the stored
        // private key would make this device display the wrong
        // fingerprint and the wrong pairing check code forever.
        if !noise_pair_is_consistent(&noise) {
            return Err(JsError::new(
                "identity noise public key does not match the private key",
            ));
        }
        Ok(Identity { signing, noise })
    }

    pub fn signing_public_b64(&self) -> String {
        b64(&self.signing.verifying)
    }

    pub fn noise_public_b64(&self) -> String {
        b64(&self.noise.public)
    }

    /// This device's fingerprint: hex of the first 16 bytes of
    /// SHA-256(noise public key), the value the host shows when it
    /// confirms the grant. Read-only; never a secret.
    pub fn noise_fingerprint(&self) -> String {
        self.noise.fingerprint()
    }

    /// Sign a relay challenge (base64 in, base64 out).
    pub fn sign(&self, challenge_b64: &str) -> Result<String, JsError> {
        let challenge = unb64(challenge_b64)?;
        Ok(b64(&self.signing.sign(&challenge)))
    }
}

/// The pairing handshake on the device side: XX initiator, bound to the
/// full invite. `start` produces message 1; `finish` consumes the host's
/// reply, sends the hello, and moves out the live channel. After that the
/// pairing waits on the host user's decision, which arrives as a sealed
/// `pairingDecision`.
///
/// State-taking methods use `&mut self` + `Option::take` instead of
/// consuming `self`: wasm-bindgen 0.2.125+ changed the ABI for consuming
/// methods and the generated glue panics on those calls ("null pointer
/// passed to rust"). `&mut` borrows are unaffected.
#[wasm_bindgen]
pub struct Pairing {
    initiator: Option<PairingInitiator>,
    binding: PairingBinding,
    step0: Vec<u8>,
    /// The host static key the scanned invite claims. XX learns the real
    /// one from the handshake; `finish` refuses any mismatch, so a QR
    /// that claims one host while the handshake presents another cannot
    /// pair the device to an impostor.
    host_public: Vec<u8>,
    /// This device's noise public key, one input of the pairing check
    /// code both ends display.
    device_public: Vec<u8>,
}

#[wasm_bindgen]
impl Pairing {
    /// Build the initiator from the invite exactly as scanned. A
    /// malformed, wrong-version, expired or structurally invalid invite
    /// fails here; a tampered one fails later, at the first MACed
    /// handshake step.
    #[wasm_bindgen(constructor)]
    pub fn start(identity_json: &str, invite_json: &str) -> Result<Pairing, JsError> {
        let identity = Identity::from_json(identity_json)?;
        let invite: PairingInvite =
            serde_json::from_str(invite_json).map_err(|error| json_error("invite json", error))?;
        // Explicit version, no downgrade: a version mismatch closes the
        // pairing instead of running a handshake neither side speaks.
        if invite.protocol_version != REMOTE_PROTOCOL_VERSION {
            return Err(JsError::new(&format!(
                "invite protocol version {} is not supported (this build speaks {})",
                invite.protocol_version, REMOTE_PROTOCOL_VERSION
            )));
        }
        let nonce = unb64(&invite.nonce)?;
        if nonce.is_empty() {
            return Err(JsError::new("invite nonce is empty"));
        }
        // Early refusal only; the host stays the authority on nonce
        // expiry and consumes it atomically.
        if now_ms() > invite.expires_at_ms {
            return Err(JsError::new("invite has expired"));
        }
        let host_public = unb64(&invite.host_public_key)?;
        if host_public.len() != KEY_BYTES {
            return Err(JsError::new("invite host public key is not 32 bytes"));
        }
        let binding = PairingBinding {
            protocol_version: invite.protocol_version,
            host_id: invite.host_id,
            nonce,
            expires_at_ms: invite.expires_at_ms,
            relay_origin: invite.relay_origin.clone(),
        };
        let mut initiator = PairingInitiator::new(&binding, &identity.noise)
            .map_err(|error| json_error("pairing start", error))?;
        let step0 = initiator
            .write_first()
            .map_err(|error| json_error("pairing message 1", error))?;
        Ok(Pairing {
            initiator: Some(initiator),
            binding,
            step0,
            host_public,
            device_public: identity.noise.public.clone(),
        })
    }

    pub fn step0_b64(&self) -> String {
        b64(&self.step0)
    }

    pub fn host_id(&self) -> String {
        self.binding.host_id.to_string()
    }

    /// Consume the host's reply, send the device hello over the fresh
    /// channel, and hand the channel out. `&mut self` + take — see the
    /// note on the struct.
    pub fn finish(
        &mut self,
        reply_b64: &str,
        device_id: &str,
        device_name: &str,
    ) -> Result<PairingAccepted, JsError> {
        let mut initiator = self
            .initiator
            .take()
            .ok_or_else(|| JsError::new("pairing initiator already consumed"))?;
        let reply = unb64(reply_b64)?;
        let _offer = initiator
            .read_reply(&reply)
            .map_err(|error| json_error("pairing message 2 (invite binding mismatch?)", error))?;
        let device_uuid: uuid::Uuid = device_id
            .parse()
            .map_err(|error| json_error("device id", error))?;
        let hello = MessageBody::PairingHello {
            device_id: device_uuid,
            device_name: if device_name.is_empty() {
                None
            } else {
                Some(device_name.to_owned())
            },
        };
        let sealed = SealedMessage {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            // The pairing channel carries no delivery sequence; both
            // ends use 0 by convention.
            transport_seq: 0,
            host_id: self.binding.host_id,
            device_id: device_uuid,
            sent_at_ms: now_ms(),
            body: hello,
        };
        let hello_bytes =
            serde_json::to_vec(&sealed).map_err(|error| json_error("hello encode", error))?;
        let (message3, channel) = initiator
            .finish(&hello_bytes)
            .map_err(|error| json_error("pairing message 3", error))?;
        // The handshake — not the QR text — is what proves which host
        // this device just paired with. The invite's host key must be
        // exactly the static key the handshake bound; anything else is
        // an identity substitution and must not produce a channel.
        let host_static = channel
            .remote_static()
            .ok_or_else(|| JsError::new("pairing handshake did not bind the host static key"))?
            .to_vec();
        if host_static != self.host_public {
            return Err(JsError::new(
                "pairing handshake host key does not match the scanned invite",
            ));
        }
        // Both ends show this code; the user compares them on the two
        // screens. Computed only from the verified handshake key.
        let check_code = pairing_code(&host_static, &self.device_public, &self.binding);
        let host_fingerprint = fingerprint(&host_static);
        Ok(PairingAccepted {
            channel: Some(WasmChannel {
                inner: channel,
                host_id: self.binding.host_id,
                device_id: device_uuid,
                kind: ChannelKind::Pairing,
                poisoned: None,
            }),
            message3: b64(&message3),
            check_code,
            host_fingerprint,
        })
    }
}

/// The handshake's last message plus the channel it opened.
#[wasm_bindgen]
pub struct PairingAccepted {
    channel: Option<WasmChannel>,
    message3: String,
    check_code: String,
    host_fingerprint: String,
}

#[wasm_bindgen]
impl PairingAccepted {
    /// Message 3 for the host (base64): send it as `Handshake { step: 2 }`.
    pub fn message3_b64(&self) -> String {
        self.message3.clone()
    }

    /// Move the channel out. `&mut self` + take — see the note on
    /// `Pairing`. Returns `undefined` when already taken.
    pub fn take_channel(&mut self) -> Option<WasmChannel> {
        self.channel.take()
    }

    /// The six digits both ends display while the host user confirms.
    /// Same inputs on the host produce the same code; a spliced pairing
    /// produces a different one. Read it before dropping this object.
    pub fn pairing_check_code(&self) -> String {
        self.check_code.clone()
    }

    /// Fingerprint of the host this device just paired with, verified
    /// against the scanned invite. Compare it with the host screen.
    pub fn host_fingerprint(&self) -> String {
        self.host_fingerprint.clone()
    }
}

/// An established Noise session that speaks whole relay frames: `seal`
/// produces the exact JSON to put on the WebSocket, `open` unwraps a
/// `Sealed` frame into the body JSON. Anything else fails closed.
///
/// The first integrity failure poisons the channel: every later call
/// fails, so a caller that ignores the thrown error still cannot keep
/// using the session. Recovery is a fresh `connect` — a new key period,
/// never a retry on the poisoned one.
#[wasm_bindgen]
pub struct WasmChannel {
    inner: SecureChannel,
    host_id: uuid::Uuid,
    device_id: uuid::Uuid,
    /// Which delivery-sequence convention this channel speaks.
    kind: ChannelKind,
    /// Set by the first failure; `Some` means "fail closed forever".
    poisoned: Option<String>,
}

impl WasmChannel {
    fn check_usable(&self) -> Result<(), JsError> {
        match &self.poisoned {
            Some(reason) => Err(JsError::new(&format!("channel poisoned: {reason}"))),
            None => Ok(()),
        }
    }

    /// Record the failure and build its error. Used for every failure
    /// that touches channel integrity or crypto state.
    fn fail(&mut self, reason: &str) -> JsError {
        self.poisoned = Some(reason.to_owned());
        JsError::new(reason)
    }
}

#[wasm_bindgen]
impl WasmChannel {
    /// Build a channel from a KK connect handshake: the paired device's
    /// every reconnect starts here.
    pub fn connect(
        identity_json: &str,
        host_public_b64: &str,
        host_id: &str,
        device_id: &str,
    ) -> Result<Connect, JsError> {
        let identity = Identity::from_json(identity_json)?;
        let host_id_uuid: uuid::Uuid = host_id
            .parse()
            .map_err(|error| json_error("host id", error))?;
        let device_uuid: uuid::Uuid = device_id
            .parse()
            .map_err(|error| json_error("device id", error))?;
        let host_public = unb64(host_public_b64)?;
        if host_public.len() != KEY_BYTES {
            return Err(JsError::new("host public key is not 32 bytes"));
        }
        let mut initiator = fintwind_remote_crypto::ConnectInitiator::new(
            REMOTE_PROTOCOL_VERSION,
            &identity.noise,
            &host_public,
            host_id_uuid,
            device_uuid,
        )
        .map_err(|error| json_error("connect start", error))?;
        let step0 = initiator
            .write_first()
            .map_err(|error| json_error("connect message 1", error))?;
        Ok(Connect {
            initiator: Some(initiator),
            host_id: host_id_uuid,
            device_id: device_uuid,
            host_public,
            step0,
        })
    }

    pub fn seal_frame(&mut self, seq: u64, body_json: &str) -> Result<String, JsError> {
        self.check_usable()?;
        // Caller-side input errors happen before any crypto state
        // moves, so they do not poison the channel.
        let body: MessageBody =
            serde_json::from_str(body_json).map_err(|error| json_error("body json", error))?;
        let sealed = SealedMessage {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            // The authenticated copy of this frame's delivery sequence:
            // the outer seq is relay-visible, this one is not.
            transport_seq: self.kind.sealing_transport_seq(seq),
            host_id: self.host_id,
            device_id: self.device_id,
            sent_at_ms: now_ms(),
            body,
        };
        let plaintext =
            serde_json::to_vec(&sealed).map_err(|error| json_error("sealed encode", error))?;
        // Past this point the transport nonce has advanced: any failure
        // leaves the channel in a state the peer cannot follow, so it
        // fails closed.
        let ciphertext = self
            .inner
            .encrypt(&plaintext)
            .map_err(|error| self.fail(&format!("encrypt: {error}")))?;
        let frame = Frame::Sealed {
            seq,
            host_id: self.host_id,
            device_id: Some(self.device_id),
            ciphertext: Base64Bytes(ciphertext),
        };
        let text = serde_json::to_string(&frame)
            .map_err(|error| self.fail(&format!("frame encode: {error}")))?;
        if text.len() > MAX_FRAME_BYTES {
            return Err(self.fail("sealed frame exceeds the maximum frame size"));
        }
        Ok(text)
    }

    /// Unwrap a `Sealed` frame into its body JSON. Every failure —
    /// wrong frame type, oversize frame, decrypt failure, inner
    /// version or addressing mismatch — poisons the channel and the
    /// caller must reconnect with a fresh key period. Ignoring the
    /// error does not keep the channel alive.
    pub fn open_frame(&mut self, frame_json: &str) -> Result<String, JsError> {
        self.check_usable()?;
        // The relay enforces the frame cap before parsing; endpoints
        // enforce it again on anything they accept.
        if frame_json.len() > MAX_FRAME_BYTES {
            return Err(self.fail("frame exceeds the maximum frame size"));
        }
        let frame: Frame = serde_json::from_str(frame_json)
            .map_err(|error| self.fail(&format!("frame json: {error}")))?;
        let Frame::Sealed {
            seq,
            ciphertext,
            host_id,
            device_id,
            ..
        } = frame
        else {
            return Err(self.fail("not a sealed frame"));
        };
        // Outer routing fields are relay-visible and forgeable.
        if host_id != self.host_id {
            return Err(self.fail("sealed frame claims another host"));
        }
        if device_id.is_some_and(|id| id != self.device_id) {
            return Err(self.fail("sealed frame claims another device"));
        }
        let plaintext = self
            .inner
            .decrypt(&ciphertext.0)
            .map_err(|_| self.fail("decrypt failed"))?;
        let sealed: SealedMessage = serde_json::from_slice(&plaintext)
            .map_err(|error| self.fail(&format!("sealed decode: {error}")))?;
        // Inner fields rode inside the AEAD: they are the authenticated
        // copy. Version first — no downgrade — then the delivery
        // sequence the outer frame cannot be trusted for, then the
        // addressing, which must name this exact channel.
        if sealed.protocol_version != REMOTE_PROTOCOL_VERSION {
            return Err(self.fail("sealed message protocol version mismatch"));
        }
        if sealed.transport_seq != self.kind.expected_transport_seq(seq) {
            return Err(self.fail("sealed message transport_seq does not match the frame"));
        }
        if sealed.host_id != self.host_id {
            return Err(self.fail("sealed message claims another host"));
        }
        if sealed.device_id != self.device_id {
            return Err(self.fail("sealed message claims another device"));
        }
        serde_json::to_string(&sealed.body)
            .map_err(|error| self.fail(&format!("body encode: {error}")))
    }

    /// The peer static key this handshake bound, base64. For a paired
    /// device that is the host's Noise identity: persist it to dial the
    /// next connect. Empty only if the handshake never bound one, which
    /// a completed handshake always does.
    pub fn remote_static_b64(&self) -> String {
        match self.inner.remote_static() {
            Some(key) => b64(key),
            None => String::new(),
        }
    }

    /// Fingerprint of the peer static key: hex of the first 16 bytes of
    /// SHA-256. Compare it with the value shown at pairing time — a
    /// changed host fingerprint means a re-paired or replaced host and
    /// must never be accepted silently.
    pub fn remote_fingerprint(&self) -> String {
        match self.inner.remote_static() {
            Some(key) => fingerprint(key),
            None => String::new(),
        }
    }
}

/// The KK connect handshake: message 1 out, the host's message 2 in, and
/// the paired channel is back.
#[wasm_bindgen]
pub struct Connect {
    initiator: Option<fintwind_remote_crypto::ConnectInitiator>,
    host_id: uuid::Uuid,
    device_id: uuid::Uuid,
    /// The host static key this connect dialed; the finished channel
    /// must have bound exactly this one.
    host_public: Vec<u8>,
    step0: Vec<u8>,
}

#[wasm_bindgen]
impl Connect {
    pub fn step0_b64(&self) -> String {
        b64(&self.step0)
    }

    pub fn finish(&mut self, reply_b64: &str) -> Result<WasmChannel, JsError> {
        let initiator = self
            .initiator
            .take()
            .ok_or_else(|| JsError::new("connect initiator already consumed"))?;
        let reply = unb64(reply_b64)?;
        let inner = initiator
            .finish(&reply)
            .map_err(|error| json_error("connect message 2", error))?;
        // KK already fails against a host that does not hold the dialed
        // static key; asserting the bound key makes that guarantee
        // explicit instead of implied.
        let bound = inner
            .remote_static()
            .ok_or_else(|| JsError::new("connect handshake did not bind the host static key"))?;
        if bound != self.host_public.as_slice() {
            return Err(JsError::new("connect handshake bound a different host key"));
        }
        Ok(WasmChannel {
            inner,
            host_id: self.host_id,
            device_id: self.device_id,
            kind: ChannelKind::Session,
            poisoned: None,
        })
    }
}

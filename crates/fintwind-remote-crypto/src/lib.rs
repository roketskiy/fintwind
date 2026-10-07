//! Noise-based end-to-end encryption for Fintwind remote control.
//!
//! Frozen by [remote-control-m0-freeze.md] § 1: Curve25519 / ChaChaPoly /
//! SHA-256 via `snow`; **XX** once per pairing, **KK** for every
//! (re)connect — a reconnect is always a new key period, so ciphertext
//! from an old session can never be mistaken for a new operation. Static
//! identity keys belong to the host and to each paired device; private
//! keys never leave their machine (storage goes through the OS credential
//! store at the binary layer, not here).
//!
//! The whole crate compiles to wasm so the PWA uses this exact
//! implementation rather than a second JS crypto stack.
//!
//! Before any code, the failure modes this surface must refuse:
//!
//! 1. A KK handshake against the wrong peer static key must fail closed.
//! 2. Tampered transport ciphertext must fail to decrypt.
//! 3. Ciphertext from one key period must not decrypt in the next.
//! 4. Both pairing sides must derive the same six-digit code from the
//!    same public keys — and a different code from any other key.
//! 5. A truncated or reordered handshake must not yield a channel.
//!
//! [remote-control-m0-freeze.md]: ../../docs/remote-control-m0-freeze.md

use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use snow::params::NoiseParams;
use uuid::Uuid;

/// One-time pairing: `Noise_XX_25519_ChaChaPoly_SHA256`.
pub const PAIRING_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
/// Every (re)connect: `Noise_KK_25519_ChaChaPoly_SHA256`.
pub const CONNECT_PARAMS: &str = "Noise_KK_25519_ChaChaPoly_SHA256";

/// Prologue prefix for pairing handshakes; the full prologue also binds
/// the protocol version so a version downgrade cannot reuse old material.
pub const PAIRING_PROLOGUE_PREFIX: &[u8] = b"fintwind-remote-pairing-v";
/// Prologue prefix for connect handshakes.
pub const CONNECT_PROLOGUE_PREFIX: &[u8] = b"fintwind-remote-connect-v";

/// Domain separation for the pairing check code.
const PAIRING_CODE_INFO: &[u8] = b"fintwind-pairing-code-v1";

/// Digits shown to the user on both ends; same class of check as Bluetooth
/// numeric comparison. Never a substitute for unattended verification.
pub const PAIRING_CODE_DIGITS: usize = 6;

#[derive(Debug)]
pub enum CryptoError {
    /// A `snow` handshake, key, or transport failure. Every variant is
    /// terminal: callers close the channel, never retry half-done state.
    Snow(snow::Error),
    /// Transport ciphertext failed integrity or the key period is wrong.
    Decrypt,
    /// A static key or handshake input had the wrong length.
    InvalidLength,
    /// A handshake method was called out of order, or before completion.
    HandshakeIncomplete,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Snow(error) => write!(f, "noise handshake failed: {error}"),
            Self::Decrypt => write!(f, "transport ciphertext failed integrity"),
            Self::InvalidLength => write!(f, "key or handshake input has the wrong length"),
            Self::HandshakeIncomplete => write!(f, "handshake used out of order"),
        }
    }
}

impl std::error::Error for CryptoError {}

impl From<snow::Error> for CryptoError {
    fn from(error: snow::Error) -> Self {
        Self::Snow(error)
    }
}

/// A 32-byte static identity keypair. `private` is stored in the OS
/// credential store by the owning binary; this type only carries bytes.
#[derive(Clone, Debug)]
pub struct IdentityKey {
    pub public: Vec<u8>,
    pub private: Vec<u8>,
}

/// Ed25519 signing key for relay registration and per-connection
/// authentication: the relay knows only the public half, and every
/// connection is proven by signing a fresh relay-supplied challenge.
/// There are no passwords and no stored bearer credentials anywhere.
#[derive(Clone, Debug)]
pub struct SigningKey {
    pub verifying: Vec<u8>,
    pub signing: Vec<u8>,
}

impl SigningKey {
    pub fn generate() -> Self {
        let mut secret = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut secret);
        let extended = ed25519_dalek::SigningKey::from_bytes(&secret);
        Self {
            verifying: extended.verifying_key().to_bytes().to_vec(),
            signing: extended.to_bytes().to_vec(),
        }
    }

    pub fn from_bytes(signing: Vec<u8>) -> Result<Self, CryptoError> {
        if signing.len() != 32 {
            return Err(CryptoError::InvalidLength);
        }
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&signing);
        let extended = ed25519_dalek::SigningKey::from_bytes(&secret);
        Ok(Self {
            verifying: extended.verifying_key().to_bytes().to_vec(),
            signing: extended.to_bytes().to_vec(),
        })
    }

    /// Sign a relay challenge. The challenge is bound verbatim into the
    /// signature; the relay verifies against the registered public half.
    pub fn sign(&self, challenge: &[u8]) -> Vec<u8> {
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&self.signing);
        let extended = ed25519_dalek::SigningKey::from_bytes(&secret);
        use ed25519_dalek::Signer as _;
        extended.sign(challenge).to_bytes().to_vec()
    }

    /// Verify a challenge signature against a registered public key.
    /// The relay's only authentication primitive.
    pub fn verify(verifying: &[u8], challenge: &[u8], signature: &[u8]) -> bool {
        let (Ok(key_bytes), Ok(sig_bytes)) =
            (<[u8; 32]>::try_from(verifying), <[u8; 64]>::try_from(signature))
        else {
            return false;
        };
        let Ok(verifying_key) = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes) else {
            return false;
        };
        verifying_key
            .verify_strict(
                challenge,
                &ed25519_dalek::Signature::from_bytes(&sig_bytes),
            )
            .is_ok()
    }
}

impl IdentityKey {
    /// Generate a fresh Curve25519 identity.
    pub fn generate() -> Result<Self, CryptoError> {
        let params: NoiseParams = PAIRING_PARAMS.parse()?;
        let keypair = snow::Builder::new(params).generate_keypair()?;
        Ok(Self {
            public: keypair.public,
            private: keypair.private,
        })
    }

    /// Reassemble from stored bytes (both exactly 32 bytes).
    pub fn from_bytes(public: Vec<u8>, private: Vec<u8>) -> Result<Self, CryptoError> {
        if public.len() != 32 || private.len() != 32 {
            return Err(CryptoError::InvalidLength);
        }
        Ok(Self { public, private })
    }

    /// Hex of the first 16 bytes of SHA-256(public): the device指纹 bound
    /// into grants and shown on confirmations.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public)
    }
}

/// Hex of the first 16 bytes of SHA-256(`public`), 32 characters.
pub fn fingerprint(public: &[u8]) -> String {
    let digest = Sha256::digest(public);
    digest[..16].iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The full prologue for a pairing handshake: prefix + protocol version,
/// binding the version into the handshake transcript.
pub fn pairing_prologue(protocol_version: u32) -> Vec<u8> {
    let mut prologue = PAIRING_PROLOGUE_PREFIX.to_vec();
    prologue.extend_from_slice(protocol_version.to_be_bytes().as_slice());
    prologue
}

/// Everything a pairing handshake is pinned to. Both ends derive the same
/// bytes from the pairing invite (the QR payload): host identity, the
/// one-time nonce, expiry, relay origin, and protocol version. A mismatch
/// in any of them makes the handshake fail, so a tampered QR cannot pair
/// a device to anything but the exact invite the host displayed.
#[derive(Clone, Debug, PartialEq)]
pub struct PairingBinding {
    pub protocol_version: u32,
    pub host_id: Uuid,
    /// The one-time pairing nonce from the invite (host-generated).
    pub nonce: Vec<u8>,
    /// Invite expiry, milliseconds since the Unix epoch. The host refuses
    /// to consume a nonce past this moment.
    pub expires_at_ms: u64,
    /// The relay origin exactly as displayed (scheme + host[:port]).
    pub relay_origin: String,
}

/// The prologue bytes for a pairing handshake: every field of the invite
/// goes into the Noise transcript.
pub fn pairing_invite_prologue(binding: &PairingBinding) -> Vec<u8> {
    let mut prologue = PAIRING_PROLOGUE_PREFIX.to_vec();
    prologue.extend_from_slice(binding.protocol_version.to_be_bytes().as_slice());
    prologue.extend_from_slice(binding.host_id.as_bytes());
    prologue.extend_from_slice(&(binding.nonce.len() as u32).to_be_bytes());
    prologue.extend_from_slice(&binding.nonce);
    prologue.extend_from_slice(binding.expires_at_ms.to_be_bytes().as_slice());
    prologue.extend_from_slice(binding.relay_origin.as_bytes());
    prologue
}

/// The full prologue for a connect handshake: prefix + protocol version +
/// both identities, so a handshake can only ever complete for this exact
/// host-device pair at this protocol version.
pub fn connect_prologue(protocol_version: u32, host_id: Uuid, device_id: Uuid) -> Vec<u8> {
    let mut prologue = CONNECT_PROLOGUE_PREFIX.to_vec();
    prologue.extend_from_slice(protocol_version.to_be_bytes().as_slice());
    prologue.extend_from_slice(host_id.as_bytes());
    prologue.extend_from_slice(device_id.as_bytes());
    prologue
}

/// Six-digit pairing check code. Both ends compute it from the same two
/// static public keys plus the invite binding after the XX handshake, and
/// the user compares the displays — this is what catches a relay swapping
/// device keys or splicing a different pairing into the flow.
pub fn pairing_code(
    host_public: &[u8],
    device_public: &[u8],
    binding: &PairingBinding,
) -> String {
    let mut keys = [host_public, device_public];
    keys.sort_by_key(|key| key.to_vec());
    let mut ikm = keys[0].to_vec();
    ikm.extend_from_slice(keys[1]);

    let hk = Hkdf::<Sha256>::new(Some(&ikm), &pairing_invite_prologue(binding));
    let mut digits = [0u8; 4];
    hk.expand(PAIRING_CODE_INFO, &mut digits)
        .expect("32-byte output is always valid for HKDF-SHA256");
    let number = u32::from_be_bytes(digits) % 1_000_000;
    format!("{number:06}")
}

/// A completed Noise session. Encrypt/decrypt are strictly ordered per
/// direction (Noise transport nonces); any failure poisons the channel
/// and the only safe response is to close and reconnect with a new KK
/// session — a new key period.
pub struct SecureChannel {
    transport: snow::TransportState,
}

/// 16-byte GCM/Poly1305 tag appended to every transport message.
const TAG_BYTES: usize = 16;

impl SecureChannel {
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut buffer = vec![0u8; plaintext.len() + TAG_BYTES];
        let length = self.transport.write_message(plaintext, &mut buffer)?;
        buffer.truncate(length);
        Ok(buffer)
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if ciphertext.len() <= TAG_BYTES {
            return Err(CryptoError::Decrypt);
        }
        let mut buffer = vec![0u8; ciphertext.len()];
        let length = self
            .transport
            .read_message(ciphertext, &mut buffer)
            .map_err(|_| CryptoError::Decrypt)?;
        buffer.truncate(length);
        Ok(buffer)
    }

    /// The peer's static public key as bound by the handshake.
    pub fn remote_static(&self) -> Option<&[u8]> {
        self.transport.get_remote_static()
    }
}

/// The initiator half of the one-time XX pairing handshake.
pub struct PairingInitiator {
    state: snow::HandshakeState,
}

/// The responder half of the one-time XX pairing handshake.
pub struct PairingResponder {
    state: snow::HandshakeState,
}

/// The initiator half of a KK connect; sends the first message.
pub struct ConnectInitiator {
    state: snow::HandshakeState,
}

/// The responder half of a KK connect; answers the initiator.
pub struct ConnectResponder {
    state: snow::HandshakeState,
}

fn build_pairing_initiator(
    binding: &PairingBinding,
    identity: &IdentityKey,
) -> Result<snow::HandshakeState, CryptoError> {
    let params: NoiseParams = PAIRING_PARAMS.parse()?;
    let prologue = pairing_invite_prologue(binding);
    Ok(snow::Builder::new(params)
        .prologue(&prologue)?
        .local_private_key(&identity.private)?
        .build_initiator()?)
}

fn build_pairing_responder(
    binding: &PairingBinding,
    identity: &IdentityKey,
) -> Result<snow::HandshakeState, CryptoError> {
    let params: NoiseParams = PAIRING_PARAMS.parse()?;
    let prologue = pairing_invite_prologue(binding);
    Ok(snow::Builder::new(params)
        .prologue(&prologue)?
        .local_private_key(&identity.private)?
        .build_responder()?)
}

fn build_connect_initiator(
    protocol_version: u32,
    identity: &IdentityKey,
    host_public: &[u8],
    host_id: Uuid,
    device_id: Uuid,
) -> Result<snow::HandshakeState, CryptoError> {
    let params: NoiseParams = CONNECT_PARAMS.parse()?;
    let prologue = connect_prologue(protocol_version, host_id, device_id);
    Ok(snow::Builder::new(params)
        .prologue(&prologue)?
        .local_private_key(&identity.private)?
        .remote_public_key(host_public)?
        .build_initiator()?)
}

fn build_connect_responder(
    protocol_version: u32,
    identity: &IdentityKey,
    device_public: &[u8],
    host_id: Uuid,
    device_id: Uuid,
) -> Result<snow::HandshakeState, CryptoError> {
    let params: NoiseParams = CONNECT_PARAMS.parse()?;
    let prologue = connect_prologue(protocol_version, host_id, device_id);
    Ok(snow::Builder::new(params)
        .prologue(&prologue)?
        .local_private_key(&identity.private)?
        .remote_public_key(device_public)?
        .build_responder()?)
}

impl PairingInitiator {
    pub fn new(binding: &PairingBinding, identity: &IdentityKey) -> Result<Self, CryptoError> {
        Ok(Self {
            state: build_pairing_initiator(binding, identity)?,
        })
    }

    /// Message 1: `-> e`. No application payload on this step.
    pub fn write_first(&mut self) -> Result<Vec<u8>, CryptoError> {
        let mut buffer = vec![0u8; 1024];
        let length = self.state.write_message(&[], &mut buffer)?;
        buffer.truncate(length);
        Ok(buffer)
    }

    /// Message 2: `<- e, ee, s, es`. Returns the responder's application
    /// payload (pairing offer: relay address, host id, nonce, expiry).
    pub fn read_reply(&mut self, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut buffer = vec![0u8; 65535];
        let length = self.state.read_message(message, &mut buffer)?;
        buffer.truncate(length);
        Ok(buffer)
    }

    /// Message 3: `-> s, se`. Sends the device's pairing request payload
    /// (device public key, nonce echo, protocol binding) and completes
    /// the handshake. Returns the message-3 bytes for the responder and
    /// the ready channel.
    pub fn finish(mut self, payload: &[u8]) -> Result<(Vec<u8>, SecureChannel), CryptoError> {
        let mut buffer = vec![0u8; 65535];
        let length = self.state.write_message(payload, &mut buffer)?;
        let transport = self.state.into_transport_mode()?;
        buffer.truncate(length);
        Ok((buffer[..length].to_vec(), SecureChannel { transport }))
    }
}

impl PairingResponder {
    pub fn new(binding: &PairingBinding, identity: &IdentityKey) -> Result<Self, CryptoError> {
        Ok(Self {
            state: build_pairing_responder(binding, identity)?,
        })
    }

    /// Message 1: `-> e`. Returns the initiator's application payload.
    pub fn read_first(&mut self, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut buffer = vec![0u8; 65535];
        let length = self.state.read_message(message, &mut buffer)?;
        buffer.truncate(length);
        Ok(buffer)
    }

    /// Message 2: `<- e, ee, s, es`. Sends the host's pairing offer.
    pub fn write_reply(
        mut self,
        payload: &[u8],
    ) -> Result<(Vec<u8>, PairingResponderFinal), CryptoError> {
        let mut buffer = vec![0u8; 65535];
        let length = self.state.write_message(payload, &mut buffer)?;
        buffer.truncate(length);
        Ok((buffer[..length].to_vec(), PairingResponderFinal { state: self.state }))
    }
}

/// The responder's last handshake step, split out so each Noise message
/// lands on its own explicit method.
pub struct PairingResponderFinal {
    state: snow::HandshakeState,
}

impl PairingResponderFinal {
    /// Message 3: `-> s, se`. Returns the device's pairing request payload
    /// and the responder's ready channel.
    pub fn finish(mut self, message: &[u8]) -> Result<(Vec<u8>, SecureChannel), CryptoError> {
        let mut buffer = vec![0u8; 65535];
        let length = self.state.read_message(message, &mut buffer)?;
        let transport = self.state.into_transport_mode()?;
        buffer.truncate(length);
        Ok((buffer[..length].to_vec(), SecureChannel { transport }))
    }
}

impl ConnectInitiator {
    pub fn new(
        protocol_version: u32,
        identity: &IdentityKey,
        host_public: &[u8],
        host_id: Uuid,
        device_id: Uuid,
    ) -> Result<Self, CryptoError> {
        Ok(Self {
            state: build_connect_initiator(
                protocol_version,
                identity,
                host_public,
                host_id,
                device_id,
            )?,
        })
    }

    /// KK message 1: `-> e, es, s, ss`.
    pub fn write_first(&mut self) -> Result<Vec<u8>, CryptoError> {
        let mut buffer = vec![0u8; 1024];
        let length = self.state.write_message(&[], &mut buffer)?;
        buffer.truncate(length);
        Ok(buffer)
    }

    /// KK message 2: `<- e, ee, se`. Completes the handshake.
    pub fn finish(mut self, reply: &[u8]) -> Result<SecureChannel, CryptoError> {
        let mut buffer = vec![0u8; 1024];
        self.state.read_message(reply, &mut buffer)?;
        let transport = self.state.into_transport_mode()?;
        Ok(SecureChannel { transport })
    }
}

impl ConnectResponder {
    pub fn new(
        protocol_version: u32,
        identity: &IdentityKey,
        device_public: &[u8],
        host_id: Uuid,
        device_id: Uuid,
    ) -> Result<Self, CryptoError> {
        Ok(Self {
            state: build_connect_responder(
                protocol_version,
                identity,
                device_public,
                host_id,
                device_id,
            )?,
        })
    }

    /// KK message 1: `-> e, es, s, ss`.
    pub fn read_first(&mut self, message: &[u8]) -> Result<(), CryptoError> {
        let mut buffer = vec![0u8; 1024];
        self.state.read_message(message, &mut buffer)?;
        Ok(())
    }

    /// KK message 2: `<- e, ee, se`. Completes the handshake.
    pub fn finish(mut self, payload: &[u8]) -> Result<(Vec<u8>, SecureChannel), CryptoError> {
        let mut buffer = vec![0u8; 1024];
        let length = self.state.write_message(payload, &mut buffer)?;
        let transport = self.state.into_transport_mode()?;
        buffer.truncate(length);
        Ok((buffer[..length].to_vec(), SecureChannel { transport }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION: u32 = 1;
    const HOST_ID: Uuid = Uuid::nil();
    const DEVICE_ID: Uuid = Uuid::max();

    fn identity() -> IdentityKey {
        IdentityKey::generate().expect("key generation cannot fail")
    }

    fn binding() -> PairingBinding {
        PairingBinding {
            protocol_version: VERSION,
            host_id: HOST_ID,
            nonce: vec![7; 32],
            expires_at_ms: 1_700_000_000_000,
            relay_origin: "https://relay.fintwind.xyz".to_owned(),
        }
    }

    /// One full XX pairing exchange, returning both ready channels.
    fn run_pairing(host: &IdentityKey, device: &IdentityKey) -> (SecureChannel, SecureChannel) {
        run_pairing_bound(&binding(), host, device)
    }

    fn run_pairing_bound(
        binding: &PairingBinding,
        host: &IdentityKey,
        device: &IdentityKey,
    ) -> (SecureChannel, SecureChannel) {
        let mut initiator = PairingInitiator::new(binding, device).unwrap();
        let mut responder = PairingResponder::new(binding, host).unwrap();
        let message1 = initiator.write_first().unwrap();
        let _ = responder.read_first(&message1).unwrap();
        let (message2, final_step) = responder.write_reply(b"offer").unwrap();
        let _ = initiator.read_reply(&message2).unwrap();
        let (message3, device_channel) = initiator.finish(b"request").unwrap();
        let (_, host_channel) = final_step.finish(&message3).unwrap();
        (device_channel, host_channel)
    }

    /// Failure 4 from the module header: both sides must derive the same
    /// code from the same keys, and any different key must change it.
    #[test]
    fn pairing_code_is_symmetric_and_discriminating() {
        let host = identity();
        let device = identity();
        let invite = binding();
        let a = pairing_code(&host.public, &device.public, &invite);
        let b = pairing_code(&device.public, &host.public, &invite);
        assert_eq!(a, b);
        assert_eq!(a.len(), PAIRING_CODE_DIGITS);
        let other = identity();
        assert_ne!(pairing_code(&host.public, &other.public, &invite), a);
    }

    /// The code must also change when the invite changes: a spliced
    /// pairing (same keys, different nonce or relay) gets a different
    /// code, which the user's comparison catches.
    #[test]
    fn pairing_code_binds_the_invite() {
        let host = identity();
        let device = identity();
        let mut spliced = binding();
        spliced.nonce = vec![9; 32];
        assert_ne!(
            pairing_code(&host.public, &device.public, &binding()),
            pairing_code(&host.public, &device.public, &spliced),
        );
    }

    /// A prologue mismatch must break the handshake. Noise XX carries no
    /// proof in message 1 (`-> e` alone), so the mismatch surfaces when
    /// message 2 — the first MACed step — cannot be read; nothing after
    /// that can complete either.
    #[test]
    fn pairing_handshake_rejects_binding_mismatch() {
        let host = identity();
        let device = identity();
        let mut tampered = binding();
        tampered.relay_origin = "https://evil.example".to_owned();
        let mut initiator = PairingInitiator::new(&tampered, &device).unwrap();
        let mut responder = PairingResponder::new(&binding(), &host).unwrap();
        let message1 = initiator.write_first().unwrap();
        let _ = responder.read_first(&message1).unwrap();
        let (message2, _final_step) = responder.write_reply(b"offer").unwrap();
        assert!(
            initiator.read_reply(&message2).is_err(),
            "a tampered invite must fail the handshake at the first MACed step"
        );
        let _ = run_pairing_bound(&binding(), &host, &device);
    }

    /// Drive one full XX exchange end to end: both sides see the same
    /// application payloads, the handshake binds the peer's static key,
    /// and the resulting channels transport both ways.
    #[test]
    fn pairing_exchange_binds_remote_static_and_transports() {
        let host = identity();
        let device = identity();
        let (mut device_channel, mut host_channel) = run_pairing(&host, &device);

        assert_eq!(
            device_channel.remote_static().unwrap(),
            host.public.as_slice()
        );
        assert_eq!(
            host_channel.remote_static().unwrap(),
            device.public.as_slice()
        );

        let sealed = device_channel.encrypt(b"hello host").unwrap();
        assert_eq!(host_channel.decrypt(&sealed).unwrap(), b"hello host");
        let reply = host_channel.encrypt(b"hello device").unwrap();
        assert_eq!(device_channel.decrypt(&reply).unwrap(), b"hello device");
    }

    /// Failure 1: KK against the wrong peer static key must fail closed.
    #[test]
    fn connect_with_wrong_peer_key_is_rejected() {
        let host = identity();
        let device = identity();
        let impostor = identity();

        let mut initiator =
            ConnectInitiator::new(VERSION, &device, &host.public, HOST_ID, DEVICE_ID).unwrap();
        // Responder believes the peer is a different device.
        let mut responder =
            ConnectResponder::new(VERSION, &host, &impostor.public, HOST_ID, DEVICE_ID).unwrap();

        let first = initiator.write_first().unwrap();
        // Message 1 already carries the initiator's encrypted static key;
        // against the wrong expected peer key it fails closed right here.
        assert!(responder.read_first(&first).is_err());
    }

    /// Failures 2 and 3: tampered ciphertext and cross-period ciphertext
    /// must both fail to decrypt.
    #[test]
    fn transport_refuses_tampered_and_stale_ciphertext() {
        let (mut a, mut b) = {
            let host = identity();
            let device = identity();
            let mut initiator =
                ConnectInitiator::new(VERSION, &device, &host.public, HOST_ID, DEVICE_ID).unwrap();
            let mut responder =
                ConnectResponder::new(VERSION, &host, &device.public, HOST_ID, DEVICE_ID).unwrap();
            let first = initiator.write_first().unwrap();
            responder.read_first(&first).unwrap();
            let (reply, responder_channel) = responder.finish(b"welcome").unwrap();
            let initiator_channel = initiator.finish(&reply).unwrap();
            (initiator_channel, responder_channel)
        };

        // Tamper: one flipped bit must break authentication.
        let mut forged = a.encrypt(b"pay me").unwrap();
        let last = forged.len() - 1;
        forged[last] ^= 0x01;
        assert!(b.decrypt(&forged).is_err());

        // Stale: the same plaintext under period one must not open in a
        // fresh period two (a reconnect always runs a new KK session).
        let sealed_old = a.encrypt(b"old world").unwrap();
        let (mut a2, mut b2) = {
            let host = identity();
            let device = identity();
            let mut initiator =
                ConnectInitiator::new(VERSION, &device, &host.public, HOST_ID, DEVICE_ID).unwrap();
            let mut responder =
                ConnectResponder::new(VERSION, &host, &device.public, HOST_ID, DEVICE_ID).unwrap();
            let first = initiator.write_first().unwrap();
            responder.read_first(&first).unwrap();
            let (reply, responder_channel) = responder.finish(b"welcome").unwrap();
            let initiator_channel = initiator.finish(&reply).unwrap();
            (initiator_channel, responder_channel)
        };
        assert!(b2.decrypt(&sealed_old).is_err());

        // Sanity: honest transport still roundtrips.
        let sealed = a2.encrypt(b"still fine").unwrap();
        assert_eq!(b2.decrypt(&sealed).unwrap(), b"still fine");
    }

    /// Failure 5: reordered handshake messages must not yield a channel.
    #[test]
    fn reordered_pairing_handshake_fails() {
        let host = identity();
        let device = identity();
        let invite = binding();
        let mut initiator = PairingInitiator::new(&invite, &device).unwrap();
        let responder = PairingResponder::new(&invite, &host).unwrap();

        let message1 = initiator.write_first().unwrap();
        // Answering message 2 before consuming message 1 must fail closed;
        // the misordered call moves and poisons the responder state.
        assert!(responder.write_reply(b"offer").is_err());
        drop(message1);
    }
}

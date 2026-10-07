//! Outer wire frames — the only structures the relay ever sees.
//!
//! Architecture rule ([remote-control-relay.md], 铁律 2): the relay routes
//! ciphertext plus authentication and routing metadata, and nothing else.
//! These frames are designed so relay-side enforcement needs no business
//! knowledge: resource limits bind on [`MAX_FRAME_BYTES`] alone, delivery
//! bookkeeping reads only `seq` and the routing identifiers, and every
//! relay-observable semantic lives in [`ControlFrame`].
//!
//! Frames serialize as JSON over the WSS transport. Every handshake frame
//! and every sealed message carries the protocol version explicitly; a
//! mismatch closes the connection. Unknown variants and unknown fields are
//! rejected, so an unrecognized message never produces a control effect.
//!
//! [remote-control-relay.md]: ../../docs/remote-control-relay.md

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use schemars::JsonSchema;
use uuid::Uuid;

/// Explicit remote-control protocol version. Both ends must agree during
/// the handshake; there is no compatibility fallback.
pub const REMOTE_PROTOCOL_VERSION: u32 = 1;

/// Upper bound for one wire frame, ciphertext included (M0 resource-table
/// initial value). The relay enforces this before parsing; endpoints
/// enforce it again on anything they accept.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Bytes on the wire as standard-alphabet base64.
///
/// A newtype instead of a `#[serde(with)]` helper so `JsonSchema` derives
/// see a string, matching what the PWA actually parses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Base64Bytes(pub Vec<u8>);

impl From<Vec<u8>> for Base64Bytes {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl Serialize for Base64Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&BASE64_STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Base64Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        BASE64_STANDARD
            .decode(text.as_bytes())
            .map(Base64Bytes)
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Base64Bytes {
    fn schema_name() -> String {
        "Base64Bytes".to_owned()
    }

    fn json_schema(
        _generator: &mut schemars::r#gen::SchemaGenerator,
    ) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some("Standard-alphabet base64 bytes.".to_owned()),
                ..Default::default()
            }))
            .into(),
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            ..Default::default()
        }
        .into()
    }

    fn is_referenceable() -> bool {
        true
    }
}

/// Fixed connection role. Authentication binds the role to the connection;
/// a connection never switches roles mid-life (architecture rule 4).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FrameRole {
    Host,
    Device,
}

/// Which Noise handshake a `Frame::Handshake` belongs to. Pairing runs
/// XX once per device; every (re)connect runs KK as a fresh key period.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HandshakeMode {
    Pairing,
    Connect,
}

/// Relay-observable rejection reasons. These are the only error semantics
/// the relay may express; business outcomes travel sealed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "code")]
pub enum RejectCode {
    /// Authentication, pairing, or grant verification failed.
    AuthFailed,
    /// Protocol version mismatch; both ends must close.
    VersionMismatch,
    /// The addressed host or device is not in the connection's grant set.
    UnknownRoute,
    /// The sender exceeded a rate or connection limit.
    RateLimited,
    /// The per-device delivery queue is full; the frame is refused, not
    /// truncated or silently dropped.
    QueueFull,
    /// The frame violates a structural or size limit.
    Malformed,
}

/// One wire frame. Exactly one variant per WSS message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum Frame {
    /// First frame on every connection, before anything else. The relay
    /// validates it against its identity registry; until it is accepted
    /// the connection carries no other frame (architecture: an
    /// unauthenticated connection runs the auth flow only).
    ///
    /// Identity is a key pair, not a password: the client fetched a
    /// one-time `challenge` from the relay, signs it with its Ed25519
    /// signing key, and presents the signature next to the routing id it
    /// registered. The relay verifies against the registered public key;
    /// there is no stored bearer credential anywhere to leak.
    Auth {
        protocol_version: u32,
        role: FrameRole,
        /// Hosts authenticate as `host_id`; devices as `device_id`. The
        /// unset one must be `None`.
        host_id: Option<Uuid>,
        device_id: Option<Uuid>,
        /// The one-time challenge exactly as the relay issued it.
        challenge: Base64Bytes,
        /// Ed25519 signature over `challenge`.
        signature: Base64Bytes,
    },
    /// A Noise handshake message. `step` is the zero-based message index
    /// within `mode`; the receiver feeds `payload` to its handshake state
    /// verbatim. Handshake frames are unauthenticated until the pattern's
    /// final message, so endpoints accept business data only after the
    /// handshake completes (architecture: no business data before auth).
    Handshake {
        protocol_version: u32,
        role: FrameRole,
        host_id: Uuid,
        /// Required for `Connect` (routing); absent during pairing until
        /// the device identity is established.
        device_id: Option<Uuid>,
        mode: HandshakeMode,
        step: u8,
        payload: Base64Bytes,
    },
    /// One end-to-end encrypted message (`SealedMessage`, encrypted by the
    /// Noise transport of the current key period). The relay preserves
    /// per-direction `seq` order and acks it, but endpoints never trust
    /// `seq` for replay protection: that lives in the Noise session and
    /// in operation ids.
    Sealed {
        seq: u64,
        host_id: Uuid,
        /// Set on host-to-device frames (routing); device-to-host frames
        /// omit it because the authenticated connection already names the
        /// device.
        device_id: Option<Uuid>,
        ciphertext: Base64Bytes,
    },
    /// Host→relay pairing declaration: this host vouches for this device.
    /// The relay verifies the host's Ed25519 signature (registered signing
    /// key) against `device_id` + `device_public_key` + `nonce`, checks
    /// the device exists, and records the pairing link. This is the only
    /// path that ever creates a link; nothing a device sends can.
    LinkDevice {
        host_id: Uuid,
        device_id: Uuid,
        /// The device's Noise static public key (X25519, base64). Recorded
        /// so the host's grant file and the relay's link cannot drift.
        device_public_key: Base64Bytes,
        /// Fresh bytes from the host; the relay consumes each once.
        nonce: Base64Bytes,
        /// Ed25519 signature over `host_id || device_id ||
        /// device_public_key || nonce` with the host's registered key.
        signature: Base64Bytes,
    },
    /// Host→revocation: tear the pairing link down at the relay. Signed
    /// exactly like `LinkDevice` but over `host_id || device_id || nonce`.
    /// The local grant revocation is a separate host-side act; the relay
    /// link going away is what actually stops the traffic.
    UnlinkDevice {
        host_id: Uuid,
        device_id: Uuid,
        nonce: Base64Bytes,
        signature: Base64Bytes,
    },
    Control { frame: ControlFrame },
}

/// The pairing invite payload, rendered into the QR the host displays.
/// Everything here is public by design; the nonce's secrecy is irrelevant
/// (it is displayable), its *freshness* is what matters. Both handshake
/// ends fold every field into the Noise prologue, so a tampered QR pairs
/// to nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairingInvite {
    pub protocol_version: u32,
    /// Relay origin exactly as the device must dial it (scheme + host).
    pub relay_origin: String,
    pub host_id: Uuid,
    /// The host's Noise static public key (X25519, base64). The device
    /// needs it only to compute the check code; XX learns it from the
    /// handshake itself, and the prologue binding is what makes this copy
    /// non-forgeable.
    pub host_public_key: String,
    /// One-time nonce from the host (base64). The host consumes it once.
    pub nonce: String,
    /// Invite expiry, milliseconds since the Unix epoch.
    pub expires_at_ms: u64,
}

/// Relay-visible control semantics. A `RelayAck` proves the relay received
/// the frame and nothing more — it is never an endpoint confirmation and
/// never releases endpoint-side recovery records (architecture: layered
/// acks, 分层 ack).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum ControlFrame {
    RelayAck { acked_seq: u64 },
    Ping { sent_at_ms: u64 },
    Pong { sent_at_ms: u64 },
    Rejected { code: RejectCode, message: String },
    /// The relay recorded the host's `LinkDevice` declaration. The grant
    /// file on the host stays the source of trust; this only says the
    /// routing link now exists.
    Linked { device_id: Uuid },
    /// The relay tore the link down on the host's `UnlinkDevice`.
    Unlinked { device_id: Uuid },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ways this surface can fail, checked in order below: a roundtrip
    /// loses bytes, an unknown field slips through as a no-op, an unknown
    /// variant becomes a silent default, a bad base64 string parses, or a
    /// seal frame leaks its ciphertext into the JSON.
    #[test]
    fn frame_roundtrips_through_json() {
        let frame = Frame::Sealed {
            seq: 7,
            host_id: Uuid::nil(),
            device_id: Some(Uuid::max()),
            ciphertext: Base64Bytes(vec![0u8, 1, 2, 255]),
        };
        let text = serde_json::to_string(&frame).unwrap();
        let parsed: Frame = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, frame);
        // The relay sees base64 only; raw bytes never appear in the text.
        assert!(!text.contains("0,1,2,255"));
    }

    #[test]
    fn unknown_fields_and_variants_are_rejected() {
        // deny_unknown_fields: an unrecognized field must fail closed, not
        // ride along as an ignored no-op.
        let text = r#"{"type":"control","frame":{"type":"ping","sentAtMs":1,"extra":true}}"#;
        assert!(serde_json::from_str::<Frame>(text).is_err());
        // Unknown variants are rejected: unrecognized semantics produce no
        // control effect anywhere downstream.
        let text = r#"{"type":"teleport","hostId":"00000000-0000-0000-0000-000000000000"}"#;
        assert!(serde_json::from_str::<Frame>(text).is_err());
    }

    #[test]
    fn malformed_base64_is_rejected() {
        let text = r#"{"type":"sealed","seq":1,"hostId":"00000000-0000-0000-0000-000000000000","deviceId":null,"ciphertext":"!!!"}"#;
        assert!(serde_json::from_str::<Frame>(text).is_err());
    }
}

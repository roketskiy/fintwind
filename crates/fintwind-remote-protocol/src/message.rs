//! Sealed inner messages — the end-to-end payload, opaque to the relay.
//!
//! Semantics map one-to-one onto the architecture document's message-plane
//! rules ([remote-control-relay.md], 消息面协议语义):
//!
//! - **Bidirectional with host-initiated requests.** Permission and
//!   user-input offers are host-initiated and pause the turn awaiting the
//!   device; that is remote control's core interaction, not an extra.
//! - **Layered acks.** The relay ack lives in [`crate::ControlFrame`];
//!   end-to-end receive acknowledgement is
//!   [`MessageBody::Acknowledged`]; host acceptance and the execution
//!   outcome are separate states in [`OperationState`].
//! - **Cursor resume.** Event streams carry the same epoch/sequence
//!   semantics as the desktop wire (`ResumeCursor` mirrors
//!   `fintwind_protocol::ReplayCursor`), so snapshot-plus-increment never
//!   bridges a gap silently.
//! - **Approval binding.** Every offer binds one concrete underlying
//!   action with a digest, a TTL, and a grant version, and has exactly
//!   one terminal state. "Always"-style rules are never offered remotely.
//! - **Operations.** Retries reuse one `operation_id` bound to a frozen
//!   summary; the host keeps a bounded, restart-surviving record, and an
//!   unknown id means "query first", never "blindly retry".
//!
//! [remote-control-relay.md]: ../../docs/remote-control-relay.md

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One end-to-end encrypted message. Decrypting a `Frame::Sealed`
/// ciphertext yields exactly this JSON.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SealedMessage {
    pub protocol_version: u32,
    pub host_id: Uuid,
    pub device_id: Uuid,
    /// Sender clock in unix milliseconds. Advisory: TTL checks compare the
    /// receiving endpoint's clock against per-message `expiresAtMs`.
    pub sent_at_ms: u64,
    pub body: MessageBody,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum MessageBody {
    /// End-to-end receive acknowledgement for sealed messages up to the
    /// named transport `seq`. Only this releases the sender's outbound
    /// unacked buffer — never a `ControlFrame::RelayAck`.
    Acknowledged { upto_seq: u64 },
    /// Device asks for an authoritative state snapshot. Sent on first
    /// connect, when the relay generation changed, or when held cursors
    /// fall outside the host's retention window.
    RequestSnapshot,
    /// Device resumes event streams from cursors it already holds. The
    /// host replays from each cursor or answers with a fresh snapshot
    /// when the cursor can no longer be honored; it never bridges a gap
    /// silently.
    Resume { cursors: Vec<ResumeCursor> },
    /// Host pushes the authoritative session list snapshot. `revision`
    /// changes whenever any listed session changes.
    SessionList { revision: u64, sessions: Vec<SessionSummary> },
    /// Host streams one sequenced driver event (transcript deltas,
    /// status, background work). Replayable desktop events only; live-only
    /// terminal output never enters this channel.
    DriverEvent { event: DriverEventFrame },
    /// Host-initiated tool approval, bound to exactly one underlying
    /// request. Pauses the turn until decided or expired.
    PermissionOffer(PermissionOffer),
    /// Device answers a `PermissionOffer`. The host atomically consumes
    /// the offer: expired, already-consumed, or digest-mismatched
    /// decisions return an explicit failure and never execute.
    PermissionDecision(PermissionDecision),
    /// Host-initiated question set (single-select, multi-select, custom
    /// answers). Answers never double as execution approvals.
    UserInputOffer(UserInputOffer),
    /// Device answers a `UserInputOffer`; same single-terminal-state
    /// consumption rules as a permission decision.
    UserInputDecision(UserInputDecision),
    /// Device-initiated operation (send, stop, rename, delete, reads).
    Operation(OperationRequest),
    /// Host reports acceptance or outcome for one operation id.
    OperationResult(OperationOutcome),
    /// Device's self-introduction inside a fresh pairing channel: the
    /// first sealed message after XX completes. The device fingerprint
    /// rides in the channel's remote static key; the routing id comes
    /// from the device's own relay registration, and the host binds the
    /// grant to the pair of them.
    PairingHello {
        /// The device's own routing id at the relay.
        device_id: Uuid,
        /// What the host shows the confirming user, e.g. "Pixel 9".
        device_name: Option<String>,
    },
    /// Host's final answer to a pairing attempt, after the user compared
    /// the check codes and confirmed on the host. Rejection is explicit —
    /// silence is never an answer the device may interpret.
    PairingDecision {
        accepted: bool,
        /// Human-readable refusal reason when `accepted` is false.
        reason: Option<String>,
    },
}

/// Resume position for one session's event stream. Shape-identical to
/// `fintwind_protocol::ReplayCursor`: `epoch` identifies the daemon
/// process that assigned `sequence`, so a restarted host's numbers can
/// never be mistaken for an old stream's.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResumeCursor {
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    pub epoch: Uuid,
    pub sequence: u64,
}

impl From<&fintwind_protocol::ReplayCursor> for ResumeCursor {
    fn from(cursor: &fintwind_protocol::ReplayCursor) -> Self {
        Self {
            session_id: cursor.session_id,
            runtime_id: cursor.runtime_id,
            epoch: cursor.epoch,
            sequence: cursor.sequence,
        }
    }
}

impl From<ResumeCursor> for fintwind_protocol::ReplayCursor {
    fn from(cursor: ResumeCursor) -> Self {
        Self {
            session_id: cursor.session_id,
            runtime_id: cursor.runtime_id,
            epoch: cursor.epoch,
            sequence: cursor.sequence,
        }
    }
}

/// A sequenced desktop driver event, transported verbatim.
///
/// Owned wrapper because the foreign type cannot implement `JsonSchema`
/// here (orphan rule); serde representation is untouched, so the host
/// adapter converts at zero semantic cost.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriverEventFrame {
    pub event: fintwind_protocol::SequencedEvent,
}

impl From<fintwind_protocol::SequencedEvent> for DriverEventFrame {
    fn from(event: fintwind_protocol::SequencedEvent) -> Self {
        Self { event }
    }
}

impl JsonSchema for DriverEventFrame {
    fn schema_name() -> String {
        "DriverEventFrame".to_owned()
    }

    fn json_schema(
        _generator: &mut schemars::r#gen::SchemaGenerator,
    ) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "Exactly `fintwind_protocol::SequencedEvent` in its camelCase serde form: \
                     sessionId, runtimeId, epoch, sequence, event { kind, payload }."
                        .to_owned(),
                ),
                ..Default::default()
            }))
            .into(),
            instance_type: Some(schemars::schema::InstanceType::Object.into()),
            ..Default::default()
        }
        .into()
    }

    fn is_referenceable() -> bool {
        true
    }
}

/// One row of the authoritative session snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionSummary {
    pub session_id: Uuid,
    pub project_id: Option<Uuid>,
    pub title: String,
    /// The session status exactly as the desktop wire serializes it
    /// (`AgentSession::status`), so both ends share one vocabulary.
    pub status: String,
    pub updated_at_ms: u64,
    pub archived: bool,
}

/// A tool-approval offer. Fields and rules follow the architecture
/// document's 审批绑定: unique id, bound action digest, TTL, grant
/// version, one terminal state.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionOffer {
    pub approval_id: Uuid,
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    /// Underlying driver request id (the provider's own permission request
    /// id), for tracing and duplicate detection.
    pub request_ref: String,
    /// Full action summary. The device must render this completely;
    /// truncated text is never a valid approval basis.
    pub title: String,
    pub detail: String,
    /// Allowed answers. "Always"-style options never appear here.
    pub options: Vec<PermissionOption>,
    /// Unix ms after which the host refuses to consume this offer.
    pub expires_at_ms: u64,
    /// Digest over the exact action and parameters. A change invalidates
    /// any previously given decision.
    pub action_digest: String,
    /// Device-grant version the offer was issued under.
    pub grant_version: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionDecision {
    pub approval_id: Uuid,
    /// Must be one of the offer's option ids.
    pub option_id: String,
    pub decided_at_ms: u64,
}

/// A host-initiated question set, mirroring the desktop user-input flow.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserInputOffer {
    pub approval_id: Uuid,
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    pub request_ref: String,
    pub questions: Vec<UserInputQuestion>,
    pub expires_at_ms: u64,
    pub grant_version: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserInputQuestion {
    pub id: String,
    pub question: String,
    pub multi_select: bool,
    /// Custom free-text answers allowed when true.
    pub allow_custom: bool,
    pub options: Vec<PermissionOption>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserInputDecision {
    pub approval_id: Uuid,
    pub answers: Vec<UserInputAnswer>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserInputAnswer {
    pub question_id: String,
    pub selected_option_ids: Vec<String>,
    pub custom_text: Option<String>,
}

/// A device-initiated operation. The envelope carries everything the host
/// needs to accept safely; typed per-kind payloads land with the host
/// adapter (M2), until then `payload` holds the desktop wire's own
/// command JSON so the envelope stays forward-frozen.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationRequest {
    /// Idempotency id. Retries of one logical operation reuse it; the
    /// host binds its accept record to (id, summary, target, TTL), so a
    /// retry can never alter the operation or extend its life.
    pub operation_id: Uuid,
    pub kind: OperationKind,
    /// Frozen human summary of what this operation does. Both ends show
    /// it where confirmation matters; the host rejects a retry whose
    /// summary no longer matches its record.
    pub summary: String,
    /// Unix ms; the host rejects operations past its validity window.
    pub expires_at_ms: u64,
    pub session_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

/// First-phase operation vocabulary (the confirmed capability list). New
/// kinds are a protocol-version change, never a soft extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OperationKind {
    /// Ordinary send: queues behind a running turn (never an implicit
    /// steer). Explicit interruption is `CancelTurn`.
    SendPrompt,
    /// Explicit interruption, reusing the desktop cancel path.
    CancelTurn,
    CreateSession,
    AttachSession,
    RenameSession,
    ArchiveSession,
    RestoreArchivedSession,
    /// Permanent delete; payload carries the frozen, confirmed id list.
    DeleteSessions,
    ApplySessionOptions,
    ForkFromResponse,
    RewindToMessage,
    /// Read-only workspace reads; payload names project-scoped paths.
    ReadFile,
    ReadDiff,
    ReadTaskOutput,
}

/// Acceptance and outcome for one operation id — the "host accepted" and
/// "executed" layers of the ack stack.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationOutcome {
    pub operation_id: Uuid,
    pub state: OperationState,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum OperationState {
    /// The host persisted its accept record; execution follows. Distinct
    /// from execution: a crash after this point yields `Unknown`, and the
    /// device must query, not retry, side-effecting operations.
    Accepted,
    /// Executed to completion.
    Executed,
    /// Known failure; the operation did not take effect.
    Rejected { code: crate::RejectCode },
    /// The validity window elapsed before acceptance.
    Expired,
    /// Id unknown: never accepted, or its record aged out of the bounded
    /// window. Callers query or verify first; automatic retries are never
    /// implied by this state.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::REMOTE_PROTOCOL_VERSION;

    /// Ways this surface can fail, checked below: a sealed message
    /// roundtrip loses fields, an operation retry mutates its summary
    /// unnoticed, an expired offer still parses as decidable, or a
    /// cursor conversion drops the epoch guard.
    #[test]
    fn sealed_message_roundtrips() {
        let message = SealedMessage {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            host_id: Uuid::nil(),
            device_id: Uuid::max(),
            sent_at_ms: 1_000,
            body: MessageBody::Operation(OperationRequest {
                operation_id: Uuid::nil(),
                kind: OperationKind::SendPrompt,
                summary: "Send one prompt to session".to_owned(),
                expires_at_ms: 61_000,
                session_id: Some(Uuid::nil()),
                payload: serde_json::json!({ "prompt": "hi" }),
            }),
        };
        let text = serde_json::to_string(&message).unwrap();
        let parsed: SealedMessage = serde_json::from_str(&text).unwrap();
        match parsed.body {
            MessageBody::Operation(operation) => {
                assert_eq!(operation.kind, OperationKind::SendPrompt);
                assert_eq!(operation.summary, "Send one prompt to session");
                assert_eq!(operation.payload["prompt"], "hi");
            }
            other => panic!("unexpected body: {other:?}"),
        }
    }

    #[test]
    fn unknown_operation_variant_is_rejected() {
        let text = r#"{"type":"sendSms"}"#;
        assert!(serde_json::from_str::<MessageBody>(text).is_err());
    }

    #[test]
    fn cursor_conversion_keeps_epoch() {
        let native = fintwind_protocol::ReplayCursor {
            session_id: Uuid::nil(),
            runtime_id: Uuid::max(),
            epoch: Uuid::new_v4(),
            sequence: 4096,
        };
        let remote = ResumeCursor::from(&native);
        assert_eq!(remote.epoch, native.epoch);
        let roundtrip = fintwind_protocol::ReplayCursor::from(remote);
        assert_eq!(roundtrip.session_id, native.session_id);
        assert_eq!(roundtrip.runtime_id, native.runtime_id);
        assert_eq!(roundtrip.epoch, native.epoch);
        assert_eq!(roundtrip.sequence, native.sequence);
    }
}

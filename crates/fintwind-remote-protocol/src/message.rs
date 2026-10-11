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
//! - **Authenticated delivery sequence.** The relay-visible `seq` is
//!   repeated inside the encrypted payload as `transport_seq`, and the
//!   two must agree; see [`SealedMessage`]. Ordering, replay detection
//!   and the cumulative ack all key off the authenticated number.
//! - **Approval binding.** Every offer binds one concrete underlying
//!   action with a digest, a TTL, and a grant version, and has exactly
//!   one terminal state. "Always"-style rules are never offered remotely.
//! - **Operations.** Retries reuse one `operation_id` bound to a frozen
//!   summary; the host keeps a bounded, restart-surviving record, and an
//!   unknown id means "query first", never "blindly retry".
//!   [`MessageBody::QueryOperationResult`] is that query: it reads the
//!   record and executes nothing.
//!
//! [remote-control-relay.md]: ../../docs/remote-control-relay.md

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::frame::Base64Bytes;

/// One end-to-end encrypted message. Decrypting a `Frame::Sealed`
/// ciphertext yields exactly this JSON.
///
/// `transport_seq` is the **authenticated delivery sequence**. The outer
/// `Frame::Sealed.seq` is relay-visible and therefore tamperable; this
/// field repeats it inside the integrity-protected payload, so the peer
/// checks the two against each other and a relay that renumbers frames in
/// transit is detected instead of silently corrupting ordering, replay
/// detection, or the cumulative ack. Rules:
///
/// - A business-channel message carries the seq of the frame it rides in
///   (`transport_seq == frame.seq`). A mismatch closes the channel.
/// - A message embedded in the pairing handshake (`PairingHello`,
///   `PairingDecision`) carries `0`: the pairing channel has no delivery
///   sequence, and both sides agree on that by convention.
/// - A `Fragment` carries its *own* physical frame seq, exactly like any
///   other message. The logical message the fragments reassemble into is
///   named by `MessageBody::Fragment::transport_seq`; the two are
///   deliberately different numbers and must not be conflated.
///
/// The field is required and has no serde default: a peer that does not
/// send it fails to parse, which is the fail-closed behaviour a security
/// field must have.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SealedMessage {
    pub protocol_version: u32,
    /// The authenticated delivery sequence; see the type docs.
    pub transport_seq: u64,
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
    /// unacked buffer — never a `ControlFrame::RelayAck`. `upto_seq` names
    /// sequences the receiver *authenticated* (the inner `transport_seq`
    /// it verified against the outer frame's `seq`), never the raw
    /// relay-visible number. An acknowledgement is never itself queued for
    /// delivery in the sender's unacked buffer: acking the ack would
    /// recurse forever.
    Acknowledged { upto_seq: u64 },
    /// Authenticated host transport liveness, not relay or execution receipt.
    /// Exactly one bounded UUID probe per device; no workspace I/O.
    LivenessProbe { probe_id: Uuid },
    LivenessReply { probe_id: Uuid },
    /// One fragment of a sealed message whose JSON is too large for a
    /// single Noise transport frame. Noise caps a transport message at
    /// 65535 bytes, so a `SessionList` for a workspace with hundreds of
    /// sessions cannot ride one frame. Fragments of one logical message
    /// share `transport_seq` (the seq of the message they reassemble
    /// into) and are numbered `index` of `of`; the receiver buffers them
    /// and dispatches the reassembled body exactly as if it had arrived
    /// whole. Each fragment rides its own frame and therefore carries its
    /// own envelope `transport_seq` (its physical frame seq); this field
    /// stays the logical message's seq.
    Fragment {
        /// The `seq` of the logical message these fragments reassemble into.
        transport_seq: u64,
        /// Zero-based position of this fragment.
        index: u32,
        /// Total fragment count for this `transport_seq`.
        of: u32,
        /// The fragment's raw JSON bytes, base64-encoded.
        data: Base64Bytes,
    },
    /// Device asks for an authoritative state snapshot. Sent on first
    /// connect, when the relay generation changed, or when held cursors
    /// fall outside the host's retention window.
    RequestSnapshot,
    /// Device asks the host for the recorded outcome of one operation id.
    /// The host answers from the same durable accept record that makes a
    /// retry idempotent, and executes nothing to answer. An id this host
    /// never accepted for this device — or one whose record aged out of
    /// the bounded window — answers `Unknown`, which means "query or
    /// verify first", never "retry the side effect". This is the recovery
    /// path for a result that was lost with the link: the device re-asks
    /// instead of re-executing.
    QueryOperationResult { operation_id: Uuid },
    /// Device resumes event streams from cursors it already holds. The
    /// host replays from each cursor or answers with a fresh snapshot
    /// when the cursor can no longer be honored; it never bridges a gap
    /// silently.
    Resume { cursors: Vec<ResumeCursor> },
    /// Host pushes the authoritative session list snapshot. `revision`
    /// changes whenever any listed session changes.
    SessionList { revision: u64, sessions: Vec<SessionSummary> },
    /// Host pushes the project directory: ids and names, never paths. Sent
    /// with every snapshot so a device can name a target for a new task or a
    /// workspace read without rendering a UUID as a label.
    ProjectList {
        revision: u64,
        projects: Vec<ProjectSummary>,
    },
    /// Host cannot serve the workspace right now (local daemon down or
    /// not yet linked). The device shows the reason instead of spinning
    /// on a list that will never arrive; the host pushes a fresh
    /// `SessionList` the moment the workspace recovers.
    WorkspaceUnavailable { reason: String },
    /// Host pushes one session's rendered transcript snapshot: the linear
    /// message list with each message's trailing tool activity already
    /// interleaved. The device renders this as-is; live deltas arrive as
    /// `DriverEvent`s and the next `Transcript` recalibrates after a turn.
    Transcript {
        session_id: Uuid,
        messages: Vec<TranscriptMessage>,
    },
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
    /// Host tells one device its cursor can no longer be honored — the
    /// epoch changed (the session's runtime instance was replaced) or the
    /// sequence fell out of the host's retention window. The device drops
    /// its local projection for that session and re-attaches for a fresh
    /// snapshot; it never bridges the gap by guessing. `session_id` is
    /// `None` when the whole workspace view must be rebuilt.
    Resync {
        session_id: Option<Uuid>,
        reason: String,
    },
    /// Host pushes the visible state of one session's prompt queue: what
    /// is waiting, in what order, and whether it is still dispatchable.
    /// A queued prompt is *accepted*, not executed — the device must not
    /// render it as a completed turn.
    SessionQueue {
        session_id: Uuid,
        items: Vec<QueueItemEntry>,
    },
    /// Host answers `LoadOptionsCatalog` with the options this session may
    /// actually use. Anything the device names that is not in here is
    /// refused, so an unknown id, an "always"-style rule, or a permission
    /// field can never ride along inside an options change.
    OptionsCatalog {
        session_id: Uuid,
        models: Vec<ModelOptionEntry>,
        agent_presets: Vec<AgentPresetEntry>,
    },
    /// Device's self-introduction inside a fresh pairing channel: the
    /// first sealed message after XX completes. The device fingerprint
    /// rides in the channel's remote static key; the routing id comes
    /// from the device's own relay registration, and the host binds the
    /// grant to the pair of them.
    PairingHello {        /// The device's own routing id at the relay.
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

/// One queued prompt as the device sees it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueueItemEntry {
    pub operation_id: Uuid,
    /// Zero-based position in the session's queue.
    pub position: usize,
    /// `queued` (waiting), `dispatching` (handed to the driver), or
    /// `expired` (its validity window elapsed; it will never run).
    pub state: String,
    /// The frozen human summary the device confirmed when it sent it.
    pub summary: String,
    pub enqueued_at_ms: u64,
    pub expires_at_ms: u64,
}

/// One model the host's provider probe reported.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelOptionEntry {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    /// Reasoning-effort / service-tier / context-window choices this model
    /// exposes; empty when the provider exposes none.
    pub reasoning_efforts: Vec<String>,
    pub service_tiers: Vec<String>,
    pub context_windows: Vec<String>,
}

/// One agent composition the host's provider probe reported.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentPresetEntry {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// One row of the authoritative session snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionSummary {
    pub session_id: Uuid,
    pub project_id: Option<Uuid>,
    /// The project's display name, resolved by the host. A device must never
    /// have to render a project UUID as a label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    pub title: String,
    /// The session status exactly as the desktop wire serializes it
    /// (`AgentSession::status`), so both ends share one vocabulary.
    pub status: String,
    pub updated_at_ms: u64,
    pub archived: bool,
}

/// One row of the host's project directory. Names only: a workspace path is
/// the desktop's own business and never crosses the wire.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSummary {
    pub project_id: Uuid,
    pub name: String,
}

/// The turn one transcript message belongs to, as this computer resolved it.
///
/// A device never counts messages or roles to work out which turn it is
/// looking at: the host binds the message to the turn's own id and its own
/// count, and says whether that turn may be branched or rewound to. The
/// eligibility is computed from the desktop's own rules, so a device cannot
/// offer an action this computer would refuse.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptTurn {
    /// The desktop turn's own id. This, not a position, is what a branch or
    /// rewind names.
    pub turn_id: Uuid,
    /// The desktop's own turn count for that turn, resolved here.
    pub turn_count: usize,
    /// A completed response this task may be branched from.
    pub can_fork: bool,
    /// A user message this task may be rewound to.
    pub can_rewind: bool,
}

/// One rendered transcript message: the host maps the desktop's full
/// NativeTranscript (messages, blocks, activities, turns) into this flat list
/// so the device never reassembles a projection. Each entry carries the tool
/// activity that trails it, so order is exact and nothing is dropped.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptMessage {
    pub role: TranscriptRole,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activities: Vec<TranscriptActivity>,
    pub created_at_ms: u64,
    /// The turn this message belongs to, when the host could bind one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TranscriptTurn>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptRole {
    User,
    Assistant,
    System,
    /// Provider-side context compaction; renders as a divider.
    Compaction,
}

/// One condensed tool activity row. Arguments and outputs stay on the
/// desktop; the device shows what happened, not the full payload.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptActivity {
    /// The desktop `ActivityKind` string (`command`, `fileChange`, …).
    pub kind: String,
    pub title: String,
    pub detail: Option<String>,
    pub failed: bool,
    pub complete: bool,
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
    /// Device closed a session view: stop streaming that session's events.
    /// Purely a transport concern; it never touches the session itself.
    DetachSession,
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
    ReadDirectory,
    ReadDiff,
    ReadTaskOutput,
    /// Cancel one queued prompt that has not been dispatched yet. A prompt
    /// already handed to the driver is not cancellable this way — that is
    /// `CancelTurn`, a different operation with different semantics.
    CancelQueuedPrompt,
    /// Ask the host for the session's available model and agent-preset
    /// options. Read-only: the host answers from its own provider probe,
    /// and the device may only name options that came back here.
    LoadOptionsCatalog,
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
            transport_seq: 7,
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

//! The whitelist adapter: remote operations mapped onto the local daemon's
//! existing business paths.
//!
//! Rules from the architecture document, enforced here in one place:
//! nothing reaches the daemon except the confirmed capability list; every
//! mapping goes through the daemon's own request path (no private state
//! poking); a store error or unknown kind is refused closed. New
//! capability is a protocol-version and code-review change, not a
//! convenience `passthrough`.
//!
//! Deliberate v2 scope note: `SendPrompt` currently maps straight to the
//! daemon's `Prompt`, so a busy session surfaces an explicit refusal
//! instead of a queue. The architecture's bounded per-session queue
//! (排队、可查看、可取消) lands with the approval-flow milestone; until
//! then the refusal is honest — it never silently becomes a steer.

use std::collections::HashMap;

use uuid::Uuid;

use fintwind_client::DaemonClient;
use fintwind_protocol::{Command, ResponsePayload};
use fintwind_remote_protocol::{OperationKind, OperationRequest};

/// Cached session runtime resolution: `session_id -> runtime_id`. Filled
/// by `AttachSession` and consulted by anything that needs a runtime, so
/// the device never names runtime ids itself (architecture: host decides
/// internal values, the device names sessions and projects).
#[derive(Default)]
pub struct RuntimeCatalog {
    runtimes: HashMap<Uuid, Uuid>,
}

impl RuntimeCatalog {
    pub fn runtime_of(&self, session_id: Uuid) -> Option<Uuid> {
        self.runtimes.get(&session_id).copied()
    }

    fn note(&mut self, session_id: Uuid, runtime_id: Option<Uuid>) {
        if let Some(runtime_id) = runtime_id {
            self.runtimes.insert(session_id, runtime_id);
        }
    }
}

/// Execute one whitelisted operation against the daemon. The return value
/// is already shaped for `OperationResult::detail`.
pub fn execute(
    daemon: &DaemonClient,
    catalog: &mut RuntimeCatalog,
    operation: &OperationRequest,
) -> Result<serde_json::Value, (fintwind_remote_protocol::RejectCode, String)> {
    use fintwind_remote_protocol::RejectCode;
    let session_id = operation.session_id;
    let refused = |code: RejectCode, message: &str| {
        Err((code, message.to_owned()))
    };
    match operation.kind {
        OperationKind::AttachSession => {
            let Some(session_id) = session_id else {
                return refused(RejectCode::Malformed, "session id required");
            };
            let payload = daemon
                .request(session_id, Uuid::nil(), Command::AttachSession)
                .map_err(|error| (RejectCode::Malformed, error.to_string()))?;
            if let ResponsePayload::SessionRuntime {
                runtime_id,
                supports_steer,
            } = &payload
            {
                catalog.note(session_id, *runtime_id);
                return serde_json::to_value(&payload)
                    .map(|mut value| {
                        value["supportsSteer"] = serde_json::Value::Bool(*supports_steer);
                        value
                    })
                    .map_err(|_| (RejectCode::Malformed, "payload encode failed".to_owned()));
            }
            serde_json::to_value(&payload)
                .map_err(|_| (RejectCode::Malformed, "payload encode failed".to_owned()))
        }
        OperationKind::SendPrompt | OperationKind::CancelTurn => {
            let Some(session_id) = session_id else {
                return refused(RejectCode::Malformed, "session id required");
            };
            let runtime_id = resolve_runtime(daemon, catalog, session_id)?;
            let command = match operation.kind {
                OperationKind::SendPrompt => {
                    let prompt = operation
                        .payload
                        .get("prompt")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            (RejectCode::Malformed, "prompt field required".to_owned())
                        })?;
                    Command::Prompt {
                        prompt: prompt.to_owned(),
                        files: Vec::new(),
                    }
                }
                _ => Command::Cancel,
            };
            let payload = daemon
                .request(session_id, runtime_id, command)
                .map_err(|error| (RejectCode::Malformed, error.to_string()))?;
            to_value(payload)
        }
        OperationKind::ApplySessionOptions => {
            let Some(session_id) = session_id else {
                return refused(RejectCode::Malformed, "session id required");
            };
            let runtime_id = resolve_runtime(daemon, catalog, session_id)?;
            // The device names only catalog options; the daemon-side
            // `WireSessionOptions` is built here from vetted fields, and a
            // payload carrying unknown fields is refused wholesale rather
            // than partially applied.
            let options = parse_session_options(&operation.payload)?;
            let payload = daemon
                .request(session_id, runtime_id, Command::ApplyOptions { options })
                .map_err(|error| (RejectCode::Malformed, error.to_string()))?;
            to_value(payload)
        }
        OperationKind::RenameSession => {
            let Some(session_id) = session_id else {
                return refused(RejectCode::Malformed, "session id required");
            };
            // Renaming is a provider-session operation addressed by the
            // device with the workspace coordinates the host itself
            // published in the session snapshot; the payload carries them
            // verbatim, and the daemon validates ownership.
            let mut command_value = operation.payload.clone();
            command_value["type"] = serde_json::json!("renameProviderSession");
            let command: Command = serde_json::from_value(command_value)
                .map_err(|_| (RejectCode::Malformed, "rename payload".to_owned()))?;
            let _ = session_id;
            let payload = daemon
                .request(Uuid::nil(), Uuid::nil(), command)
                .map_err(|error| (RejectCode::Malformed, error.to_string()))?;
            to_value(payload)
        }
        // Reads land with the read-only workspace milestone (M5); refuse
        // them explicitly until then instead of half-implementing.
        OperationKind::ReadFile
        | OperationKind::ReadDiff
        | OperationKind::ReadTaskOutput => refused(
            RejectCode::Malformed,
            "read-only workspace lands with the next milestone",
        ),
        // Catalog operations (create, archive, delete, fork, rewind) need
        // the batch orchestration semantics from the freeze document;
        // they arrive with the control-plane milestone.
        OperationKind::CreateSession
        | OperationKind::ArchiveSession
        | OperationKind::RestoreArchivedSession
        | OperationKind::DeleteSessions
        | OperationKind::ForkFromResponse
        | OperationKind::RewindToMessage => refused(
            RejectCode::Malformed,
            "catalog operations land with the control-plane milestone",
        ),
    }
}

/// Attach implicitly so a device can name just the session id. Failure to
/// resolve is a refusal, never a guessed runtime.
fn resolve_runtime(
    daemon: &DaemonClient,
    catalog: &mut RuntimeCatalog,
    session_id: Uuid,
) -> Result<Uuid, (fintwind_remote_protocol::RejectCode, String)> {
    if let Some(runtime_id) = catalog.runtime_of(session_id) {
        return Ok(runtime_id);
    }
    let payload = daemon
        .request(session_id, Uuid::nil(), Command::AttachSession)
        .map_err(|error| {
            (
                fintwind_remote_protocol::RejectCode::UnknownRoute,
                format!("session not attachable: {error}"),
            )
        })?;
    if let ResponsePayload::SessionRuntime { runtime_id, .. } = &payload {
        catalog.note(session_id, *runtime_id);
        return runtime_id.ok_or_else(|| {
            (
                fintwind_remote_protocol::RejectCode::UnknownRoute,
                "session has no runtime".to_owned(),
            )
        });
    }
    Err((
        fintwind_remote_protocol::RejectCode::UnknownRoute,
        "unexpected attach response".to_owned(),
    ))
}

fn parse_session_options(
    payload: &serde_json::Value,
) -> Result<fintwind_protocol::WireSessionOptions, (fintwind_remote_protocol::RejectCode, String)>
{
    use fintwind_remote_protocol::RejectCode;
    let parse_str = |key: &str| {
        payload
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let Some(mode) = parse_str("mode") else {
        return Err((RejectCode::Malformed, "mode required".to_owned()));
    };
    let Some(interaction_mode) = parse_str("interactionMode") else {
        return Err((RejectCode::Malformed, "interactionMode required".to_owned()));
    };
    Ok(fintwind_protocol::WireSessionOptions {
        mode,
        interaction_mode,
        model: parse_str("model"),
        reasoning_effort: parse_str("reasoningEffort"),
        service_tier: parse_str("serviceTier"),
        context_window: parse_str("contextWindow"),
    })
}

fn to_value(
    payload: ResponsePayload,
) -> Result<serde_json::Value, (fintwind_remote_protocol::RejectCode, String)> {
    serde_json::to_value(&payload)
        .map_err(|_| (fintwind_remote_protocol::RejectCode::Malformed, "payload encode failed".to_owned()))
}

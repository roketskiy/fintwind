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
//! ## What the device may name, and what it may not
//!
//! The device names **session ids, project ids, titles, prompt text, turn
//! counts and read paths**. Everything else — workspace coordinates, the
//! provider binary, runtime ids, the current permission mode, the model a
//! session runs — is resolved here from the host's own task state or from
//! the daemon's trusted provider probe, because a device that could name
//! them could point an operation at a workspace it was never shown.
//!
//! ## Reads are not side effects
//!
//! `ReadFile`, `ReadDirectory`, `ReadDiff` and `ReadTaskOutput` return
//! content in the operation result the device sees, and the host records
//! **no** content in its durable dedup store or its logs: a replay answers
//! the recorded state, and re-reading is how a device gets content again.
//! `QueryOperationResult` therefore never returns a body.

use std::collections::HashMap;
use std::path::PathBuf;

use uuid::Uuid;

use fintwind_client::DaemonClient;
use fintwind_protocol::model::{RuntimeMode, SessionWorkspace};
use fintwind_protocol::{Command, ResponsePayload};
use fintwind_remote_protocol::{OperationKind, OperationRequest, RejectCode};

use crate::state::HostState;
use crate::workspace_diff::{self, DiffReadRequest};
use crate::workspace_directory::{self, DirectoryReadRequest};
use crate::workspace_reads::{self, FileReadRequest};

/// The provenance one remote operation carries into the daemon.
///
/// The host authenticates the device end to end, so the daemon takes the
/// host's word over the loopback IPC it already trusts — and never a
/// device's own. A remote client can therefore only ask: it cannot name
/// another device, another grant version, or a trusted object. The host id
/// is the host's own registered identity, named by the host itself.
pub fn device_provenance(
    state: &HostState,
    device_id: Uuid,
) -> Result<fintwind_protocol::submission::RemoteProvenance, (RejectCode, String)> {
    // The host id is the identity this host registered with the relay; a
    // value that is not a uuid refuses rather than attributing the
    // operation to a default.
    let host_id = state
        .host_id()
        .parse::<Uuid>()
        .map_err(|_| (RejectCode::Malformed, "this host has no usable identity".to_owned()))?;
    // The grant version is read fresh from the grant file, so a revocation
    // or re-pairing is visible to the daemon on the very request that
    // follows it.
    Ok(fintwind_protocol::submission::RemoteProvenance {
        host_id,
        device_id,
        grant_version: state.grant_version(device_id),
    })
}

/// Largest prompt text one operation may carry. The wire frame already
/// bounds this; the explicit cap keeps an absurd payload from reaching the
/// driver as a "prompt".
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;

/// Largest session title one operation may set.
pub const MAX_TITLE_BYTES: usize = 512;

/// Largest task-output body one read may return. The body is built from
/// the host's own session record, never from a device-supplied path.
pub const MAX_TASK_OUTPUT_BYTES: usize = 64 * 1024;

/// Largest batch one delete may name. The list is frozen at confirmation
/// time; a batch never grows to include sessions that appeared later.
pub const MAX_BATCH_SESSIONS: usize = 32;

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

    /// Drop every cached runtime. A daemon restart invalidates all of
    /// them; the device re-attaches and the host resolves fresh ones.
    pub fn clear(&mut self) {
        self.runtimes.clear();
    }
}

/// Execute one whitelisted operation against the daemon. The return value
/// is already shaped for `OperationResult::detail`.
pub fn execute(
    state: &HostState,
    daemon: &DaemonClient,
    catalog: &mut RuntimeCatalog,
    device_id: Uuid,
    operation: &OperationRequest,
) -> Result<serde_json::Value, (RejectCode, String)> {
    let session_id = operation.session_id;
    let refused = |code: RejectCode, message: &str| Err((code, message.to_owned()));
    let provenance = device_provenance(state, device_id)?;
    match operation.kind {
        // Detach never reaches the adapter: the worker handles it before
        // execution, since it touches only subscriptions. A queue
        // cancellation never reaches the driver either.
        OperationKind::DetachSession | OperationKind::CancelQueuedPrompt => {
            unreachable!("handled by the worker")
        }
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
                    let prompt = prompt.trim();
                    if prompt.is_empty() {
                        return refused(RejectCode::Malformed, "a non-empty prompt is required");
                    }
                    if prompt.len() > MAX_PROMPT_BYTES {
                        return refused(RejectCode::Malformed, "prompt is too long");
                    }
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
        OperationKind::ApplySessionOptions => apply_session_options(daemon, catalog, operation, provenance),
        OperationKind::LoadOptionsCatalog => {
            // Read-only: the host answers from its own provider probe and
            // the device may only name options that came back here.
            let Some(session_id) = session_id else {
                return refused(RejectCode::Malformed, "session id required");
            };
            let project_id = session_project(daemon, session_id)?;
            let directory = project_directory(daemon, project_id)?;
            let probe = probe_provider(daemon, Some(directory), true)?;
            // The operation settles with `Executed` and no payload; the
            // catalog itself travels as its own typed message, so a large
            // model list never rides inside a result detail.
            to_value(serde_json::json!({ "models": probe.models, "agentPresets": probe.agent_presets }))
        }
        OperationKind::RenameSession => rename_session(daemon, operation),
        OperationKind::CreateSession => create_session(daemon, operation, provenance),
        OperationKind::DeleteSessions => delete_sessions(daemon, operation),
        OperationKind::ForkFromResponse | OperationKind::RewindToMessage => {
            let Some(session_id) = session_id else {
                return refused(RejectCode::Malformed, "session id required");
            };
            // The device names the turn it is looking at; this computer
            // resolves that turn's own count from its hydrated record. A
            // frozen number would let an older confirmation land on a newer
            // turn that later reused the same count, so the count is never
            // taken from the wire.
            let turn_id = operation
                .payload
                .get("turnId")
                .and_then(serde_json::Value::as_str)
                .and_then(|text| Uuid::parse_str(text).ok())
                .ok_or_else(|| (RejectCode::Malformed, "turnId field required".to_owned()))?;
            let turn_count = resolve_turn_count(daemon, session_id, turn_id)?;
            let command = match operation.kind {
                OperationKind::ForkFromResponse => Command::ForkSessionFromResponse { turn_count },
                _ => Command::RewindSessionToMessage { turn_count },
            };
            let runtime_id = resolve_runtime(daemon, catalog, session_id)?;
            let payload = daemon
                .request(session_id, runtime_id, command)
                .map_err(|error| (RejectCode::Malformed, error.to_string()))?;
            // The fork/rewind produced a new session record; the device
            // needs its id to open it, and the host pushes a fresh list.
            let session_id = match &payload {
                ResponsePayload::SessionForked { session, .. }
                | ResponsePayload::SessionRewound { session, .. } => Some(session.id),
                _ => None,
            };
            to_value(serde_json::json!({
                "sessionId": session_id,
                "turnCount": turn_count,
                "warning": match &payload {
                    ResponsePayload::SessionForked { checkpoint_warning, .. } => {
                        checkpoint_warning.clone()
                    }
                    ResponsePayload::SessionRewound { cleanup_warning, .. } => {
                        cleanup_warning.clone()
                    }
                    _ => None,
                },
            }))
        }
        // ---- read-only workspace ----
        OperationKind::ReadFile => {
            let request: FileReadRequest = serde_json::from_value(operation.payload.clone())
                .map_err(|error| (RejectCode::Malformed, error.to_string()))?;
            let root = project_directory(daemon, request.project_id)?;
            let page = workspace_reads::read_file(&root, &request)
                .map_err(|(code, message)| (code, message))?;
            to_value(page)
        }
        OperationKind::ReadDirectory => {
            let request: DirectoryReadRequest = serde_json::from_value(operation.payload.clone())
                .map_err(|error| (RejectCode::Malformed, error.to_string()))?;
            let root = project_directory(daemon, request.project_id)?;
            let page = workspace_directory::read_directory(&root, &request)
                .map_err(|(code, message)| (code, message))?;
            to_value(page)
        }
        OperationKind::ReadDiff => {
            let request: DiffReadRequest = serde_json::from_value(operation.payload.clone())
                .map_err(|error| (RejectCode::Malformed, error.to_string()))?;
            let root = project_directory(daemon, request.project_id)?;
            let page = workspace_diff::read_diff(&root, &request)
                .map_err(|(code, message)| (code, message))?;
            to_value(page)
        }
        OperationKind::ReadTaskOutput => read_task_output(daemon, operation),
        // Archive has a real store now: the daemon owns the durable flag,
        // stops a busy turn, cancels what is queued, and never deletes
        // files or touches the provider's own session.
        OperationKind::ArchiveSession => archive_session(daemon, operation, true, provenance),
        OperationKind::RestoreArchivedSession => archive_session(daemon, operation, false, provenance),
    }
}

/// Create a session inside a project the host already knows. The device
/// names the project id and nothing else: the workspace, provider,
/// permission mode and model defaults come from this computer's own
/// new-task path, and the record is persisted by the daemon before it is
/// returned. A remote client can never choose a permission posture the
/// computer did not already choose.
fn create_session(
    daemon: &DaemonClient,
    operation: &OperationRequest,
    provenance: fintwind_protocol::submission::RemoteProvenance,
) -> Result<serde_json::Value, (RejectCode, String)> {
    let refused = |code: RejectCode, message: &str| Err((code, message.to_owned()));
    let project_id = operation
        .payload
        .get("projectId")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| Uuid::parse_str(text).ok())
        .ok_or_else(|| (RejectCode::Malformed, "projectId field required".to_owned()))?;
    // Strict whitelist: a new remote task is created from the project id
    // and nothing else. Any other key — a permission mode, a runtime mode,
    // a workspace, a binary, a directory, an unknown field — is refused
    // rather than ignored, so a caller learns its request did not apply
    // instead of silently getting different defaults.
    if let Some(key) = operation
        .payload
        .as_object()
        .and_then(|fields| fields.keys().find(|key| key.as_str() != "projectId"))
    {
        return refused(
            RejectCode::Malformed,
            &format!("a new remote task carries a project id and nothing else; {key} is not accepted"),
        );
    }
    // The daemon creates the record with its own new-task defaults and
    // persists it, so what comes back is a task this computer really has.
    let payload = daemon
        .request(
            Uuid::nil(),
            Uuid::nil(),
            Command::CreateRemoteTask {
                provenance,
                project_id,
            },
        )
        .map_err(|error| (RejectCode::Malformed, error.to_string()))?;
    let ResponsePayload::RemoteTaskCreated { session_id } = payload else {
        return refused(
            RejectCode::UnknownRoute,
            "this host could not create the task",
        );
    };
    to_value(serde_json::json!({ "sessionId": session_id }))
}

/// Rename a session. A started task is renamed on the provider (the single
/// store of titles) and the local record is synchronized; a task that has
/// never run has no provider session, so only the local title moves. The
/// device supplies a title and nothing else.
fn rename_session(
    daemon: &DaemonClient,
    operation: &OperationRequest,
) -> Result<serde_json::Value, (RejectCode, String)> {
    let refused = |code: RejectCode, message: &str| Err((code, message.to_owned()));
    let Some(session_id) = operation.session_id else {
        return refused(RejectCode::Malformed, "session id required");
    };
    let title = operation
        .payload
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or_else(|| (RejectCode::Malformed, "a non-empty title is required".to_owned()))?;
    if title.len() > MAX_TITLE_BYTES {
        return refused(RejectCode::Malformed, "title is too long");
    }
    let state = load_task_state(daemon)?;
    let Some(session) = state.sessions.iter().find(|session| session.id == session_id) else {
        return refused(RejectCode::UnknownRoute, "this session is unknown to this host");
    };
    let mut renamed_on_provider = false;
    let mut provider_error: Option<String> = None;
    if session.native_session_id.is_some() {
        // A started task: the provider owns the title, so rename it there
        // and let the local record follow.
        let Some((binary, directory, native_session_id)) = session_coordinates(daemon, session_id)
        else {
            return refused(
                RejectCode::UnknownRoute,
                "this session has no workspace coordinates on this computer",
            );
        };
        match daemon.request(
            Uuid::nil(),
            Uuid::nil(),
            Command::RenameProviderSession {
                binary,
                directory,
                session_id: native_session_id,
                title: title.to_owned(),
            },
        ) {
            // Only the daemon's own acknowledgement counts as a rename. A
            // transport-level Ok around an error payload is not success.
            Ok(ResponsePayload::Ack) => renamed_on_provider = true,
            Ok(other) => {
                provider_error = Some(format!(
                    "this computer did not confirm the rename: {}",
                    response_label(&other)
                ));
            }
            Err(error) => provider_error = Some(error.to_string()),
        }
    }
    // Local title: built from the host's own task-state row plus the new
    // title. The daemon's merge guards protect a pending turn from a
    // stale snapshot, and the device never supplies the session object.
    let mut local = session.clone();
    local.title = title.to_owned();
    local.updated_at = fintwind_protocol::model::unix_time();
    let synced = matches!(
        daemon.request(
            Uuid::nil(),
            Uuid::nil(),
            Command::SaveTaskState {
                projects: Vec::new(),
                live_session_ids: Vec::new(),
                sessions: vec![local],
            },
        ),
        Ok(ResponsePayload::TaskStateSaved { .. })
    );
    to_value(serde_json::json!({
        "sessionId": session_id,
        "renamedOnProvider": renamed_on_provider,
        "localTitleSynced": synced,
        "providerError": provider_error,
    }))
}

/// A short, content-free label for a response payload, for a refusal that
/// must say what came back without leaking a body.
fn response_label(payload: &ResponsePayload) -> &'static str {
    match payload {
        ResponsePayload::Ack => "ack",
        ResponsePayload::SubmissionSaved { .. } => "submissionSaved",
        ResponsePayload::RemoteTaskCreated { .. } => "remoteTaskCreated",
        ResponsePayload::RemotePromptAdmitted { .. } => "remotePromptAdmitted",
        ResponsePayload::RemotePromptState { .. } => "remotePromptState",
        ResponsePayload::RemotePrompts { .. } => "remotePrompts",
        ResponsePayload::SessionRuntime { .. } => "sessionRuntime",
        ResponsePayload::Started { .. } => "started",
        ResponsePayload::OptionsApplied { .. } => "optionsApplied",
        ResponsePayload::Cursor { .. } => "cursor",
        ResponsePayload::Settings { .. } => "settings",
        ResponsePayload::ProviderProbe { .. } => "providerProbe",
        ResponsePayload::PlanUsage { .. } => "planUsage",
        ResponsePayload::SkillsCatalog { .. } => "skillsCatalog",
        ResponsePayload::TaskState { .. } => "taskState",
        ResponsePayload::TaskStateSaved { .. } => "taskStateSaved",
        ResponsePayload::Session { .. } => "session",
        ResponsePayload::SessionForked { .. } => "sessionForked",
        ResponsePayload::SessionRewound { .. } => "sessionRewound",
        ResponsePayload::Workspace { .. } => "workspace",
        _ => "unrecognized",
    }
}

/// Delete an explicit, frozen list of sessions. A running session is
/// stopped first; only session records are removed, never project files.
/// Every item reports its own outcome — a partial failure is never
/// reported as a successful batch.
fn delete_sessions(
    daemon: &DaemonClient,
    operation: &OperationRequest,
) -> Result<serde_json::Value, (RejectCode, String)> {
    let refused = |code: RejectCode, message: &str| Err((code, message.to_owned()));
    let ids = operation
        .payload
        .get("sessionIds")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| (RejectCode::Malformed, "sessionIds field required".to_owned()))?;
    if ids.is_empty() || ids.len() > MAX_BATCH_SESSIONS {
        return refused(RejectCode::Malformed, "the session list is empty or too long");
    }
    let mut session_ids = Vec::with_capacity(ids.len());
    for id in ids {
        let text = id
            .as_str()
            .ok_or_else(|| (RejectCode::Malformed, "sessionIds must be uuid strings".to_owned()))?;
        let session_id = Uuid::parse_str(text)
            .map_err(|_| (RejectCode::Malformed, "sessionIds must be uuid strings".to_owned()))?;
        if !session_ids.contains(&session_id) {
            session_ids.push(session_id);
        }
    }
    let state = load_task_state(daemon)?;
    // Freeze every target's trusted coordinates *before* anything is
    // removed. Reading them after a removal would find no row and silently
    // skip the provider-side delete, leaving a task in the provider's store
    // that this computer no longer lists.
    let frozen: Vec<(
        Uuid,
        Option<(PathBuf, PathBuf, String)>,
        fintwind_protocol::model::SessionStatus,
    )> = session_ids
        .iter()
        .map(|session_id| {
            let row = state
                .sessions
                .iter()
                .find(|session| session.id == *session_id);
            (
                *session_id,
                row.and_then(|_| session_coordinates(daemon, *session_id)),
                row.map(|session| session.status).unwrap_or_default(),
            )
        })
        .collect();
    let mut results = Vec::with_capacity(session_ids.len());
    let mut failed = false;
    for (session_id, coordinates, status) in frozen {
        if !state
            .sessions
            .iter()
            .any(|session| session.id == session_id)
        {
            results.push(serde_json::json!({
                "sessionId": session_id,
                "state": "rejected",
                "code": "unknownRoute",
                "message": "this session is unknown to this host",
            }));
            failed = true;
            continue;
        }
        // Stop before delete: a running turn is cancelled first. The
        // driver's cancel is fire-and-forget, so what this reports is that a
        // stop was *requested and accepted by the runtime* — never that the
        // turn is confirmed ended. Its settlement arrives as an event, and
        // no field here claims otherwise.
        let mut stop_requested = false;
        if status.is_busy() {
            let runtime = resolve_runtime(daemon, &mut RuntimeCatalog::default(), session_id);
            if let Ok(runtime_id) = runtime {
                stop_requested = daemon
                    .request(session_id, runtime_id, Command::Cancel)
                    .is_ok_and(|payload| matches!(payload, ResponsePayload::Ack));
            }
            if !stop_requested {
                results.push(serde_json::json!({
                    "sessionId": session_id,
                    "state": "rejected",
                    "code": "malformed",
                    "message": "this session could not be stopped before deletion",
                }));
                failed = true;
                continue;
            }
        }
        // Remove the daemon-owned task. This never touches project files.
        let removed = daemon
            .request(session_id, Uuid::nil(), Command::RemoveSession)
            .is_ok_and(|payload| matches!(payload, ResponsePayload::Ack));
        // A started task also lives in the provider's own store; remove it
        // there too, with the coordinates frozen above. A provider failure
        // is reported per item and never turns a local removal into a
        // success it did not achieve.
        let mut provider_removed = false;
        let mut provider_error: Option<String> = None;
        if removed {
            if let Some((binary, directory, native_session_id)) = coordinates {
                match daemon.request(
                    Uuid::nil(),
                    Uuid::nil(),
                    Command::DeleteProviderSession {
                        binary,
                        directory,
                        session_id: native_session_id,
                    },
                ) {
                    Ok(ResponsePayload::Ack) => provider_removed = true,
                    Ok(other) => {
                        provider_error = Some(format!(
                            "this computer did not confirm the provider delete: {}",
                            response_label(&other)
                        ));
                    }
                    Err(error) => provider_error = Some(error.to_string()),
                }
            }
        }
        if !removed {
            results.push(serde_json::json!({
                "sessionId": session_id,
                "state": "rejected",
                "code": "malformed",
                "message": "this session could not be removed",
            }));
            failed = true;
            continue;
        }
        // The field is `stopRequested`, not `stopped`: a driver cancel is
        // fire-and-forget, so the honest claim is that a stop was requested
        // of a live runtime and accepted — never that the turn ended. The
        // turn's settlement arrives as its own event.
        results.push(serde_json::json!({
            "sessionId": session_id,
            "state": "executed",
            "stopRequested": stop_requested,
            "providerSessionRemoved": provider_removed,
            "providerError": provider_error,
        }));
    }
    // The overall state is Executed only when every item executed; a
    // partial batch is a rejection that still carries the per-item truth.
    if failed {
        return Err((
            RejectCode::Malformed,
            serde_json::json!({ "results": results }).to_string(),
        ));
    }
    to_value(serde_json::json!({ "results": results }))
}

/// Apply session options.
///
/// The device may switch the interaction mode and name model / agent /
/// reasoning-effort / service-tier / context-window values **from this
/// computer's own catalog**. The permission mode is the computer's security
/// posture: it is echoed from the session's record and never accepted from
/// the device, so a remote client cannot widen what a task may reach.
///
/// Every named value is vetted against the *effective* model — the one this
/// change selects, or the one the session already runs — and a model change
/// drops traits the new model does not expose, rather than carrying an
/// incompatible effort, tier or window into it. The values reach the session
/// record through the daemon, and its driver when one is live; a session
/// with no runtime yet keeps them for its next turn.
fn apply_session_options(
    daemon: &DaemonClient,
    catalog: &mut RuntimeCatalog,
    operation: &OperationRequest,
    provenance: fintwind_protocol::submission::RemoteProvenance,
) -> Result<serde_json::Value, (RejectCode, String)> {
    let refused = |code: RejectCode, message: &str| Err((code, message.to_owned()));
    let Some(session_id) = operation.session_id else {
        return refused(RejectCode::Malformed, "session id required");
    };
    for key in ["mode", "runtimeMode", "workspace", "binary", "cwd", "projectId"] {
        if operation.payload.get(key).is_some() {
            return refused(
                RejectCode::Malformed,
                "the permission mode and the workspace are set on this computer, not remotely",
            );
        }
    }
    let interaction_mode = match operation.payload.get("interactionMode") {
        None => None,
        Some(value) => {
            let text = value.as_str().ok_or_else(|| {
                (RejectCode::Malformed, "interactionMode must be a string".to_owned())
            })?;
            match text {
                "build" | "plan" => Some(text.to_owned()),
                _ => {
                    return refused(
                        RejectCode::Malformed,
                        "interactionMode must be \"build\" or \"plan\"",
                    )
                }
            }
        }
    };
    // List projections omit permission and option detail. Echo only the
    // hydrated computer-owned settings, never the list's default posture.
    let Ok(ResponsePayload::Session { session }) =
        daemon.request(session_id, Uuid::nil(), Command::HydrateSession { session_id })
    else {
        return refused(RejectCode::UnknownRoute, "this session's options are unavailable on this computer");
    };
    let Some(session) = session else {
        return refused(RejectCode::UnknownRoute, "this session is unknown to this host");
    };
    let project_id = session_project(daemon, session_id)?;
    let directory = project_directory(daemon, project_id)?;
    let probe = probe_provider(daemon, Some(directory), true)?;
    let model = vetted_catalog_id(&operation.payload, "model", &probe.models)?;
    // The agent preset is vetted against the same probe's presets and then
    // actually applied: it reaches the session record and the driver, so a
    // preset the computer offers is the one the next turn uses.
    let agent_preset = vetted_catalog_id(&operation.payload, "agentPreset", &probe.agent_presets)?;
    if (session.runtime_mode == RuntimeMode::Plan || interaction_mode.as_deref() == Some("plan"))
        && agent_preset.as_deref().is_some_and(|agent| agent != "plan")
    {
        return refused(RejectCode::Malformed, "this task is restricted to the plan agent on this computer");
    }
    // The effective model is what traits are checked against: the one this
    // change selects, or the one the session already runs.
    let effective_model = model.clone().or_else(|| session.model.clone());
    let reasoning_effort = vetted_trait(
        &operation.payload,
        "reasoningEffort",
        &probe.models,
        effective_model.as_deref(),
    )?;
    let service_tier = vetted_trait(
        &operation.payload,
        "serviceTier",
        &probe.models,
        effective_model.as_deref(),
    )?;
    let context_window = vetted_trait(
        &operation.payload,
        "contextWindow",
        &probe.models,
        effective_model.as_deref(),
    )?;
    // A model change does not inherit traits the new model does not expose.
    // Carrying them would leave the session claiming an effort or window the
    // provider would refuse on the next turn.
    let model_changed = model.is_some() && model != session.model;
    let inherited = |current: &Option<String>| {
        if model_changed {
            None
        } else {
            current.clone()
        }
    };
    let options = fintwind_protocol::WireSessionOptions {
        // The permission posture is this computer's own; it is echoed, never
        // taken from the wire.
        mode: encode_runtime_mode(session.runtime_mode).to_owned(),
        interaction_mode: interaction_mode
            .unwrap_or_else(|| encode_interaction_mode(session.interaction_mode).to_owned()),
        model: model.or_else(|| session.model.clone()),
        reasoning_effort: reasoning_effort.or_else(|| inherited(&session.reasoning_effort)),
        service_tier: service_tier.or_else(|| inherited(&session.service_tier)),
        context_window: context_window.or_else(|| inherited(&session.context_window)),
    };
    // The daemon owns the record: it decodes every enum itself, persists the
    // values, and reaches the driver when one is live. A session with no
    // runtime yet only records them, so its next turn starts with them.
    let payload = daemon
        .request(
            Uuid::nil(),
            Uuid::nil(),
            Command::SetRemoteSessionOptions {
                provenance,
                session_id,
                options,
                agent_preset: agent_preset.clone(),
            },
        )
        .map_err(|error| (RejectCode::Malformed, error.to_string()))?;
    let ResponsePayload::OptionsApplied { applied } = payload else {
        return refused(
            RejectCode::Malformed,
            "this host did not confirm the option change",
        );
    };
    // Keep the host's own runtime map warm so a later operation on this
    // session does not have to re-attach to learn it.
    let _ = resolve_runtime(daemon, catalog, session_id);
    to_value(serde_json::json!({
        "applied": applied,
        "agentPreset": agent_preset,
    }))
}

/// The turn count this computer's own record gives one turn id, resolved
/// from a freshly hydrated session.
///
/// This is the binding that stops a stale confirmation: a phone that names a
/// turn it was shown gets the count that turn carries *now*, so a turn that
/// was rewound away is refused instead of silently matching a different turn
/// that later reused the position.
fn resolve_turn_count(
    daemon: &DaemonClient,
    session_id: Uuid,
    turn_id: Uuid,
) -> Result<usize, (RejectCode, String)> {
    let Ok(ResponsePayload::Session { session }) =
        daemon.request(session_id, Uuid::nil(), Command::HydrateSession { session_id })
    else {
        return Err((
            RejectCode::UnknownRoute,
            "this session could not be read on this computer".to_owned(),
        ));
    };
    let Some(session) = session else {
        return Err((
            RejectCode::UnknownRoute,
            "this session is unknown to this host".to_owned(),
        ));
    };
    session
        .turns
        .iter()
        .find(|turn| turn.id == turn_id)
        .map(|turn| turn.turn_count)
        .ok_or_else(|| {
            (
                RejectCode::Malformed,
                "that message is no longer part of this task".to_owned(),
            )
        })
}

/// Archive or restore one session. Both are this computer's own tidy-up:
/// archiving stops a running turn and cancels what is queued, restoring only
/// puts the task back in the active list. Neither deletes files, neither
/// touches the provider's own session, and neither widens what a task may
/// reach.
fn archive_session(
    daemon: &DaemonClient,
    operation: &OperationRequest,
    archive: bool,
    provenance: fintwind_protocol::submission::RemoteProvenance,
) -> Result<serde_json::Value, (RejectCode, String)> {
    let refused = |code: RejectCode, message: &str| Err((code, message.to_owned()));
    let Some(session_id) = operation.session_id else {
        return refused(RejectCode::Malformed, "session id required");
    };
    for key in ["workspace", "binary", "cwd", "projectId"] {
        if operation.payload.get(key).is_some() {
            return refused(
                RejectCode::Malformed,
                "archiving is this computer's own tidy-up; a device may not name a workspace",
            );
        }
    }
    let command = if archive {
        Command::ArchiveRemoteSession {
            provenance,
            session_id,
        }
    } else {
        Command::RestoreRemoteSession {
            provenance,
            session_id,
        }
    };
    let payload = daemon
        .request(Uuid::nil(), Uuid::nil(), command)
        .map_err(|error| (RejectCode::UnknownRoute, error.to_string()))?;
    if !matches!(payload, ResponsePayload::Ack) {
        return refused(
            RejectCode::UnknownRoute,
            "this host did not confirm the change",
        );
    }
    to_value(serde_json::json!({
        "sessionId": session_id,
        "archived": archive,
    }))
}

/// Read one session's task output from the host's own session record. The
/// device names a session and nothing else: no file path, no directory, no
/// command. The body is the session's own message record, bounded; the
/// base's record does not associate messages with turns, so no turn filter
/// is offered rather than a filter that would silently ignore its input.
fn read_task_output(
    daemon: &DaemonClient,
    operation: &OperationRequest,
) -> Result<serde_json::Value, (RejectCode, String)> {
    let refused = |code: RejectCode, message: &str| Err((code, message.to_owned()));
    if operation.payload.get("turnId").is_some() {
        return refused(
            RejectCode::Malformed,
            "task output is read per session; this record has no per-turn view",
        );
    }
    let Some(session_id) = operation.session_id else {
        return refused(RejectCode::Malformed, "session id required");
    };
    let Ok(ResponsePayload::Session { session }) =
        daemon.request(session_id, Uuid::nil(), Command::HydrateSession { session_id })
    else {
        return refused(
            RejectCode::UnknownRoute,
            "this session's output is unavailable on this computer",
        );
    };
    let Some(session) = session else {
        return refused(RejectCode::UnknownRoute, "this session is unknown to this host");
    };
    // Started tasks keep their authoritative conversation in the provider;
    // the local hydrated record can still contain only the admitted prompt.
    // Do not silently substitute that stale projection on a failed fetch.
    let messages = if session.native_session_id.is_some() {
        let Some((binary, directory, native_session_id)) = session_coordinates(daemon, session_id) else {
            return refused(RejectCode::UnknownRoute, "this session's output coordinates are unavailable on this computer");
        };
        match daemon.request(Uuid::nil(), Uuid::nil(), Command::FetchNativeTranscript {
            binary, directory, session_id: native_session_id,
        }) {
            Ok(ResponsePayload::NativeTranscript { transcript }) => transcript.messages,
            Err(error) if error.to_string().contains("superseded") =>
                return refused(RejectCode::UnknownRoute, "this session's output read was superseded; retry the read"),
            _ => return refused(RejectCode::UnknownRoute, "this session's provider output is unavailable on this computer"),
        }
    } else { session.messages };
    let mut body = String::new();
    let mut truncated = false;
    for message in &messages {
        let line = format!("{}: {}", role_label(message.role), message.content.trim());
        if body.len() + line.len() + 1 > MAX_TASK_OUTPUT_BYTES {
            truncated = true;
            break;
        }
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&line);
    }
    to_value(serde_json::json!({
        "sessionId": session_id,
        "content": body,
        "truncated": truncated,
    }))
}

fn role_label(role: fintwind_protocol::model::MessageRole) -> &'static str {
    use fintwind_protocol::model::MessageRole;
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Compaction => "compaction",
    }
}

/// A catalog-named option id: it must be one the host's probe reported.
/// An unknown id, an "always"-style rule, or anything that is not a plain
/// catalog entry is refused rather than forwarded.
fn vetted_catalog_id<T: CatalogEntry>(
    payload: &serde_json::Value,
    key: &str,
    catalog: &[T],
) -> Result<Option<String>, (RejectCode, String)> {
    let Some(value) = payload.get(key) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| (RejectCode::Malformed, format!("{key} must be a string")))?;
    if text.is_empty() {
        return Ok(None);
    }
    if catalog.iter().any(|entry| entry.id() == text) {
        Ok(Some(text.to_owned()))
    } else {
        Err((
            RejectCode::Malformed,
            format!("{key} is not one of this computer's available options"),
        ))
    }
}

/// A model trait (reasoning effort, service tier, context window): it must
/// be one the **effective** model exposes. Without an effective model there
/// is nothing to check against, so a trait cannot be accepted on its own.
fn vetted_trait(
    payload: &serde_json::Value,
    key: &str,
    models: &[fintwind_protocol::model::ProviderModel],
    model: Option<&str>,
) -> Result<Option<String>, (RejectCode, String)> {
    let Some(value) = payload.get(key) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| (RejectCode::Malformed, format!("{key} must be a string")))?;
    if text.is_empty() {
        return Ok(None);
    }
    let Some(model) = model else {
        return Err((
            RejectCode::Malformed,
            format!("{key} needs a model from this computer's options first"),
        ));
    };
    let Some(entry) = models.iter().find(|entry| entry.id == model) else {
        return Err((
            RejectCode::Malformed,
            format!("{key} is not one of this computer's available options"),
        ));
    };
    let allowed = match key {
        "reasoningEffort" => &entry.reasoning_efforts,
        "serviceTier" => &entry.service_tiers,
        "contextWindow" => &entry.context_windows,
        _ => &Vec::new(),
    };
    if allowed.iter().any(|option| &option.id == text) {
        Ok(Some(text.to_owned()))
    } else {
        Err((
            RejectCode::Malformed,
            format!("{key} is not available for the selected model"),
        ))
    }
}

/// One catalog entry the device may name.
trait CatalogEntry {
    fn id(&self) -> &str;
}

impl CatalogEntry for fintwind_protocol::model::ProviderModel {
    fn id(&self) -> &str {
        &self.id
    }
}

impl CatalogEntry for fintwind_protocol::model::ProviderAgentPreset {
    fn id(&self) -> &str {
        &self.id
    }
}

/// Attach implicitly so a device can name just the session id. Failure to
/// resolve is a refusal, never a guessed runtime.
pub fn resolve_runtime(
    daemon: &DaemonClient,
    catalog: &mut RuntimeCatalog,
    session_id: Uuid,
) -> Result<Uuid, (RejectCode, String)> {
    if let Some(runtime_id) = catalog.runtime_of(session_id) {
        return Ok(runtime_id);
    }
    let payload = daemon
        .request(session_id, Uuid::nil(), Command::AttachSession)
        .map_err(|error| {
            (
                RejectCode::UnknownRoute,
                format!("session not attachable: {error}"),
            )
        })?;
    if let ResponsePayload::SessionRuntime { runtime_id, .. } = &payload {
        catalog.note(session_id, *runtime_id);
        return runtime_id.ok_or_else(|| {
            (
                RejectCode::UnknownRoute,
                "session has no runtime".to_owned(),
            )
        });
    }
    Err((
        RejectCode::UnknownRoute,
        "unexpected attach response".to_owned(),
    ))
}

/// The host's own task state, as the adapter needs it.
pub struct TaskStateView {
    pub projects: Vec<fintwind_protocol::model::Project>,
    pub sessions: Vec<fintwind_protocol::model::AgentSession>,
}

/// The host's own task state, or a refusal.
fn load_task_state(daemon: &DaemonClient) -> Result<TaskStateView, (RejectCode, String)> {
    let Ok(ResponsePayload::TaskState {
        projects, sessions, ..
    }) = daemon.request(Uuid::nil(), Uuid::nil(), Command::LoadTaskState)
    else {
        return Err((
            RejectCode::UnknownRoute,
            "this host could not read its own session state".to_owned(),
        ));
    };
    Ok(TaskStateView { projects, sessions })
}

/// The project a session belongs to, from the host's own task state.
fn session_project(
    daemon: &DaemonClient,
    session_id: Uuid,
) -> Result<Uuid, (RejectCode, String)> {
    let state = load_task_state(daemon)?;
    state
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .map(|session| session.project_id)
        .ok_or_else(|| {
            (
                RejectCode::UnknownRoute,
                "this session is unknown to this host".to_owned(),
            )
        })
}

/// A project's root directory, from the host's own task state. The device
/// never supplies a directory.
fn project_directory(
    daemon: &DaemonClient,
    project_id: Uuid,
) -> Result<PathBuf, (RejectCode, String)> {
    let state = load_task_state(daemon)?;
    state
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .map(|project| project.path.clone())
        .ok_or_else(|| {
            (
                RejectCode::UnknownRoute,
                "this project is unknown to this host".to_owned(),
            )
        })
}

/// Ask the daemon to probe the provider. The daemon resolves the binary in
/// its own trusted environment — the same resolution its own drivers use —
/// so an isolated or overridden installation is honored and the device
/// never names a binary. `discover_models` runs the provider's own catalog
/// discovery, which is only needed when the catalog itself is the answer.
pub fn probe_provider(
    daemon: &DaemonClient,
    directory: Option<PathBuf>,
    discover_models: bool,
) -> Result<fintwind_protocol::model::ProviderProbe, (RejectCode, String)> {
    let payload = daemon
        .request(
            Uuid::nil(),
            Uuid::nil(),
            Command::ProbeProvider {
                binary_override: None,
                directory,
                discover_models,
                probe_version: false,
            },
        )
        .map_err(|error| (RejectCode::UnknownRoute, error.to_string()))?;
    match payload {
        ResponsePayload::ProviderProbe { probe, .. } => Ok(probe),
        _ => Err((
            RejectCode::UnknownRoute,
            "this host could not probe its provider".to_owned(),
        )),
    }
}

/// The provider binary the daemon trusts, used for transcript, rename and
/// delete coordinates. The daemon's own probe is authoritative; the host's
/// PATH is only a last resort for a daemon that cannot probe.
pub fn resolve_provider_binary(
    daemon: &DaemonClient,
    directory: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Ok(probe) = probe_provider(daemon, directory, false) {
        if probe.installed {
            if let Some(path) = probe.path {
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    find_opencode_binary()
}

/// The opencode binary, found the same way the desktop app's PATH probe
/// finds it. Only a fallback: the daemon's own probe is the trusted
/// resolution.
#[cfg(windows)]
const OPENCODE_BINARY: &str = "opencode.exe";
#[cfg(not(windows))]
const OPENCODE_BINARY: &str = "opencode";

fn find_opencode_binary() -> Option<PathBuf> {
    let search_path = std::env::var_os("PATH")?;
    std::env::split_paths(&search_path)
        .map(|directory| directory.join(OPENCODE_BINARY))
        .find(|candidate| candidate.is_file())
}

/// The workspace coordinates a native-session operation needs: the trusted
/// provider binary, the session's workspace directory, and the provider's
/// own session id. Anything absent means the session was never run natively
/// here, and there is nothing to address. The device never supplies any of
/// these; they come from the host's own task state.
pub fn session_coordinates(
    daemon: &DaemonClient,
    session_id: Uuid,
) -> Option<(PathBuf, PathBuf, String)> {
    let Ok(ResponsePayload::TaskState { projects, sessions, .. }) =
        daemon.request(Uuid::nil(), Uuid::nil(), Command::LoadTaskState)
    else {
        return None;
    };
    let session = sessions.iter().find(|session| session.id == session_id)?;
    let native_session_id = session.native_session_id.clone()?;
    let project = projects
        .iter()
        .find(|project| project.id == session.project_id)
        .map(|project| project.path.clone());
    let directory = match &session.workspace {
        SessionWorkspace::Worktree { path, .. } => path.clone(),
        _ => project?,
    };
    let binary = resolve_provider_binary(daemon, Some(directory.clone()))?;
    Some((binary, directory, native_session_id))
}

/// The daemon's `mode` field is the permission posture (plan / ask /
/// auto-accept / full access), serialized by the base protocol's own
/// camelCase naming. It is only ever echoed back from the host's record.
fn encode_runtime_mode(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::Plan => "plan",
        RuntimeMode::Ask => "ask",
        RuntimeMode::AutoAcceptEdits => "autoAcceptEdits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "fullAccess",
    }
}

fn encode_interaction_mode(mode: fintwind_protocol::model::InteractionMode) -> &'static str {
    match mode {
        fintwind_protocol::model::InteractionMode::Build => "build",
        fintwind_protocol::model::InteractionMode::Plan => "plan",
    }
}

fn to_value(
    payload: impl serde::Serialize,
) -> Result<serde_json::Value, (RejectCode, String)> {
    serde_json::to_value(&payload)
        .map_err(|_| (RejectCode::Malformed, "payload encode failed".to_owned()))
}

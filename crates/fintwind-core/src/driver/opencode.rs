//! `opencode serve` is OpenCode's real API: one resident process serves
//! every Fintwind session started with the same opencode binary, streams
//! server-sent events, and answers permission requests the user can
//! actually be asked. Fintwind runs its own private global server per
//! binary via `opencode_pool` — never the user-level `opencode service`.
//! The process runs in a stable data directory, and each session's
//! workspace rides along as per-request location data (`location.directory`
//! when creating the session, `x-opencode-directory` / `?directory=` on
//! location-scoped routes). The server's lifetime is the daemon's:
//! `shutdown_all` reclaims it after the daemon drops its sessions. A prompt
//! posted into a busy session is folded into the running turn rather than
//! queued behind it, which is what makes steering a plain post.
//!
//! Routes and payload shapes here were read off a live `opencode` server's
//! `/api` protocol and event stream, not guessed. The v1 compatibility
//! surface (`/session/...`, `/event` with `properties`) is gone from current
//! releases — `POST /session` answers 405 — so everything below speaks the
//! `/api/*` protocol: prompts post `{text, files}`, forks take `{before}`
//! (an empty object copies the whole transcript; the older `boundary`
//! shape is only a fallback), messages come back as `{data:[...],cursor}`,
//! and events arrive as `{type,data}` lines on
//! `/api/event` — one shared connection per server port, delivered through
//! `opencode_events`, with each driver filtering its own session family.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use crossbeam_channel::{Sender, unbounded};
use fintwind_protocol::PromptFile;
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::activity;
use crate::driver::{
    DriverControl, DriverEventSender, DriverEventSink, DriverStartOptions, SessionOptions,
};
use crate::model::{
    ActivityItem, ActivityKind, BackgroundWorkEvent, BackgroundWorkItem, BackgroundWorkKey,
    BackgroundWorkKind, BackgroundWorkStatus, BackgroundWorkTranscriptEvent, CompactionState,
    CompactionStatus, DriverEvent, InteractionMode, PermissionOption, ProviderResumeCursor,
    RuntimeMode, TurnStats, UserInputAnswer, UserInputOption, UserInputQuestion, unix_time_millis,
};
use crate::opencode_events::EventFeed;
use crate::opencode_pool::PooledServer;
use crate::opencode_session::{
    encode_path_segment, fork_session_removing_turns_on_server, request_json_on_port_with_directory,
};

/// How often the permission poll scans the server's pending requests. The
/// endpoint answers instantly when nothing is pending and opencode does not
/// stream permission events, so this cadence bounds how long an approval
/// waits to reach the UI while costing next to nothing when idle.
const PERMISSION_POLL_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutoApprove {
    None,
    Edits,
    All,
}

impl From<bool> for AutoApprove {
    fn from(all: bool) -> Self {
        if all { Self::All } else { Self::None }
    }
}

impl From<RuntimeMode> for AutoApprove {
    fn from(mode: RuntimeMode) -> Self {
        match mode.access() {
            RuntimeMode::FullAccess => Self::All,
            RuntimeMode::AutoAcceptEdits => Self::Edits,
            _ => Self::None,
        }
    }
}

impl AutoApprove {
    fn allows(self, action: &str) -> bool {
        match self {
            Self::All => true,
            Self::Edits => is_edit_action(action),
            Self::None => false,
        }
    }
}

fn is_edit_action(action: &str) -> bool {
    matches!(action, "edit" | "write" | "patch" | "multiedit")
}

fn json_str_field<'a>(value: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .unwrap_or("")
}

fn json_str_list(value: &Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn opencode_session_permissions(mode: RuntimeMode, interaction_mode: InteractionMode) -> Value {
    let access = mode.access();
    if interaction_mode == InteractionMode::Plan {
        let effect = if access == RuntimeMode::FullAccess {
            "allow"
        } else {
            "ask"
        };
        return json!([{ "action": "shell", "resource": "*", "effect": effect }]);
    }
    match access {
        RuntimeMode::Ask => json!([
            { "action": "edit", "resource": "*", "effect": "ask" },
            { "action": "shell", "resource": "*", "effect": "ask" },
        ]),
        RuntimeMode::AutoAcceptEdits => json!([
            { "action": "edit", "resource": "*", "effect": "allow" },
            { "action": "shell", "resource": "*", "effect": "ask" },
        ]),
        _ => json!([{ "action": "*", "resource": "*", "effect": "allow" }]),
    }
}

fn apply_opencode_session_permissions(
    server: &crate::opencode_pool::PooledServer,
    session_id: &str,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
) {
    let path = format!("/api/session/{}", encode_path_segment(session_id));
    let _ = server.request(
        "PATCH",
        &path,
        Some(&json!({
            "permissions": opencode_session_permissions(mode, interaction_mode),
        })),
    );
}

enum CommandMessage {
    Prompt {
        text: String,
        files: Vec<PromptFile>,
    },
    Steer {
        text: String,
        files: Vec<PromptFile>,
    },
    Compact,
    Cancel,
    Respond {
        request_id: String,
        option_id: String,
        session_id: Option<String>,
    },
    RespondUserInput {
        request_id: String,
        answers: Vec<UserInputAnswer>,
        session_id: Option<String>,
    },
    Shutdown,
}

/// Parent session plus any descendant whose `parentID` chain reaches it.
/// Shared by the event thread and the permission poll so a child request is
/// neither dropped as someone else's session nor answered on the parent URL.
struct SessionFamily {
    root: String,
    members: Mutex<HashSet<String>>,
    rejected: Mutex<HashSet<String>>,
}

impl SessionFamily {
    fn new(root: String) -> Self {
        let mut members = HashSet::new();
        members.insert(root.clone());
        Self {
            root,
            members: Mutex::new(members),
            rejected: Mutex::new(HashSet::new()),
        }
    }

    fn remember(&self, session_id: impl Into<String>) {
        let session_id = session_id.into();
        if session_id.is_empty() {
            return;
        }
        self.members.lock().insert(session_id.clone());
        self.rejected.lock().remove(&session_id);
    }

    fn contains(&self, session_id: &str) -> bool {
        self.members.lock().contains(session_id)
    }

    fn belongs(&self, session_id: &str, port: u16) -> bool {
        self.belongs_with_probe(session_id, |id| probe_session_parent(port, id))
    }

    fn belongs_with_probe<F>(&self, session_id: &str, mut probe: F) -> bool
    where
        F: FnMut(&str) -> Result<Option<String>, SessionProbeError>,
    {
        if session_id.is_empty() {
            return false;
        }
        if self.contains(session_id) {
            return true;
        }
        if self.rejected.lock().contains(session_id) {
            return false;
        }

        let mut current = session_id.to_owned();
        let mut chain = vec![current.clone()];
        for _ in 0..8 {
            match probe(&current) {
                Err(SessionProbeError::Unavailable) => return false,
                Err(SessionProbeError::NotFound) | Ok(None) => {
                    reject_chain(&self.rejected, &self.root, &chain);
                    return false;
                }
                Ok(Some(parent)) => {
                    if parent.is_empty() {
                        reject_chain(&self.rejected, &self.root, &chain);
                        return false;
                    }
                    if self.contains(&parent) {
                        for id in chain {
                            self.remember(id);
                        }
                        return true;
                    }
                    if self.rejected.lock().contains(&parent) {
                        reject_chain(&self.rejected, &self.root, &chain);
                        return false;
                    }
                    if chain.iter().any(|id| *id == parent) {
                        return false;
                    }
                    current = parent;
                    chain.push(current.clone());
                }
            }
        }
        false
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionProbeError {
    NotFound,
    Unavailable,
}

fn reject_chain(rejected: &Mutex<HashSet<String>>, root: &str, chain: &[String]) {
    let mut rejected = rejected.lock();
    for id in chain {
        if id != root {
            rejected.insert(id.clone());
        }
    }
}

fn is_http_not_found(error: &anyhow::Error) -> bool {
    error.to_string().contains("HTTP 404")
}

fn is_http_bad_request(error: &anyhow::Error) -> bool {
    error.to_string().contains("HTTP 400")
}

/// Posts a permission decision in the v2.0.11+ shape, then the previous
/// `reply` field if that body is rejected.
///
/// Live 2.0.11 and 2.0.16 require `decision` (`once` / `always` / `reject`)
/// and answer `{"reply": ...}` with HTTP 400. A 404 from the legacy body is
/// returned as-is so [`post_owned_reply`] can still retarget the owning
/// session; any other legacy failure keeps the current-contract error, so a
/// real validation failure is not replaced by "missing decision".
fn post_permission_decision(
    mut post: impl FnMut(&Value) -> anyhow::Result<Value>,
    decision: &str,
) -> anyhow::Result<Value> {
    match post(&json!({"decision": decision})) {
        Ok(value) => Ok(value),
        Err(error) if is_http_bad_request(&error) => match post(&json!({"reply": decision})) {
            Ok(value) => Ok(value),
            Err(legacy) if is_http_not_found(&legacy) => Err(legacy),
            Err(_) => Err(error),
        },
        Err(error) => Err(error),
    }
}

fn permission_reply_path(session_id: &str, request_id: &str) -> String {
    format!(
        "/api/session/{}/permission/{}/reply",
        encode_path_segment(session_id),
        encode_path_segment(request_id)
    )
}

fn form_reply_path(session_id: &str, form_id: &str) -> String {
    format!(
        "/api/session/{}/form/{}/reply",
        encode_path_segment(session_id),
        encode_path_segment(form_id)
    )
}

fn value_session_id(value: &Value) -> Option<&str> {
    value
        .get("sessionID")
        .or_else(|| value.get("sessionId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

fn lookup_pending_item_session(pending: &Value, request_id: &str) -> Option<String> {
    pending
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|item| {
            (item.get("id").and_then(Value::as_str) == Some(request_id))
                .then(|| value_session_id(item).map(str::to_owned))
                .flatten()
        })
}

fn retry_session_after_not_found(
    attempted: &str,
    request_id: &str,
    pending: &Value,
) -> Option<String> {
    let found = lookup_pending_item_session(pending, request_id)?;
    (found != attempted).then_some(found)
}

fn session_parent_id_from_response(value: &Value) -> Option<String> {
    let info = value.get("data").unwrap_or(value);
    info.get("parentID")
        .or_else(|| info.get("parentId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn probe_session_parent(port: u16, session_id: &str) -> Result<Option<String>, SessionProbeError> {
    let path = format!("/api/session/{}", encode_path_segment(session_id));
    match crate::opencode_session::request_json_on_port(
        port,
        "GET",
        &path,
        None,
        Duration::from_secs(2),
    ) {
        Ok(value) => Ok(session_parent_id_from_response(&value)),
        Err(error) if is_http_not_found(&error) => Err(SessionProbeError::NotFound),
        Err(_) => Err(SessionProbeError::Unavailable),
    }
}

fn is_session_prompt_event(kind: &str) -> bool {
    kind.starts_with("permission.") || kind.starts_with("form.") || kind.starts_with("question.")
}

fn post_owned_reply(
    mut post: impl FnMut(&str) -> anyhow::Result<Value>,
    get: impl Fn(&str) -> anyhow::Result<Value>,
    mut reject: impl FnMut(&str) -> anyhow::Result<Value>,
    path_for: impl Fn(&str) -> String,
    preferred_session: &str,
    request_id: &str,
    list_path: &str,
) -> anyhow::Result<()> {
    match post(&path_for(preferred_session)) {
        Ok(_) => Ok(()),
        Err(error) if is_http_not_found(&error) => {
            let pending = get(list_path).ok();
            let retry_session = pending.as_ref().and_then(|pending| {
                retry_session_after_not_found(preferred_session, request_id, pending)
            });
            if let Some(retry_session) = retry_session {
                match post(&path_for(&retry_session)) {
                    Ok(_) => Ok(()),
                    Err(retry_error) => {
                        let _ = reject(&path_for(&retry_session));
                        Err(retry_error)
                    }
                }
            } else {
                let _ = reject(&path_for(preferred_session));
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

/// A prompt's attachment as the `files` array wants it: a `file:` URI of the
/// daemon's copy. A relative path cannot be a file URL, and dropping it would
/// send a prompt the model cannot see, so that fails the post instead.
fn file_uri(path: &Path) -> anyhow::Result<String> {
    url::Url::from_file_path(path)
        .map(|uri| uri.to_string())
        .map_err(|()| anyhow!("attachment path is not absolute: {}", path.display()))
}

/// The metadata a session create and every prompt carry: the app's identity
/// plus the task UUID that owns this conversation, so any client of the
/// OpenCode server can trace a session — or a single prompt — back to the
/// task that created it. `Session.Metadata` is a free-form object, and the
/// server echoes it on `GET /api/session/{id}` (verified on 2.0.11 and
/// 2.0.16; the field has been in the contract since 2.0.4).
fn fintwind_task_metadata(task_id: Option<&str>) -> Option<Value> {
    let task = task_id.map(str::trim).filter(|task| !task.is_empty())?;
    Some(json!({ "source": "fintwind", "task": task }))
}

/// The prompt bodies both turn starts and steers post: the current shape,
/// plus the pre-2.0 fallback that drops the newer fields. OpenCode keeps the
/// model on the session (set through `/api/session/{id}/model`). Attachment
/// chips ride `files` as `file:` URIs of the daemon copy; the typed text is
/// not rewritten with `@` paths. Plain prompts omit `files`. A relative path
/// cannot be a file URL, and dropping it would send a prompt the model cannot
/// see, so that fails the post instead. `metadata` records the owning task,
/// and `delivery: "steer"` states what every prompt from this app is: folded
/// into the running turn when there is one, a fresh turn when there is not —
/// the server's default, now named. Queued delivery stays a server-side
/// capability for a future inbox UI; the app's own follow-up queue already
/// covers that product behavior locally.
fn prompt_bodies(
    text: &str,
    files: &[PromptFile],
    task_id: Option<&str>,
) -> anyhow::Result<(Value, Value)> {
    let mut current = json!({"text": text, "delivery": "steer"});
    if let Some(metadata) = fintwind_task_metadata(task_id) {
        current["metadata"] = metadata;
    }
    let mut legacy = json!({"text": text});
    if !files.is_empty() {
        let mut encoded = Vec::with_capacity(files.len());
        for file in files {
            encoded.push(json!({
                "uri": file_uri(&file.path)?,
                "name": file.name,
            }));
        }
        current["files"] = Value::Array(encoded.clone());
        legacy["files"] = Value::Array(encoded);
    }
    Ok((current, legacy))
}

/// The `answer` object a form reply posts: every question's selections keyed
/// by its field key. A multiselect field must answer with an array — the
/// server rejects a bare string with `FormInvalidAnswerError` — so recorded
/// field shapes decide; without a recording (a driver restart between ask
/// and reply) the selection count guesses, and multi-select answered with a
/// single choice degrades to the rejected shape.
fn form_reply_answer(fields: &[(String, bool)], answers: &[UserInputAnswer]) -> Value {
    answers
        .iter()
        .map(|answer| {
            let multi = fields
                .iter()
                .any(|(key, multi)| *multi && *key == answer.question_id);
            let value = if multi || answer.answers.len() > 1 {
                json!(answer.answers)
            } else {
                json!(answer.answers.first().cloned().unwrap_or_default())
            };
            (answer.question_id.clone(), value)
        })
        .collect::<serde_json::Map<String, Value>>()
        .into()
}

/// The directory string a request names this task's workspace with. An
/// absolute cwd passes through untouched; a relative one is canonicalized so
/// the server sees one stable path regardless of where its process runs, and
/// a failed canonicalize falls back to the original value.
fn opencode_location_directory(cwd: &Path) -> String {
    if cwd.is_absolute() {
        return cwd.to_string_lossy().into_owned();
    }
    std::fs::canonicalize(cwd)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| cwd.to_string_lossy().into_owned())
}

/// A resume must stay inside the task's own workspace: on a server hosting
/// every directory, replaying a stale cursor would silently reopen another
/// project's conversation. Sessions recorded before locations existed carry
/// none and stay resumable. The recorded directory is compared against the
/// same normalized form the create call sent, as `Path`s so trailing
/// separators and separator style do not reject an identical directory.
fn verify_resume_location(session: &Value, cwd: &Path, session_id: &str) -> anyhow::Result<()> {
    let info = session.get("data").unwrap_or(session);
    let Some(recorded) = info
        .pointer("/location/directory")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|directory| !directory.is_empty())
    else {
        return Ok(());
    };
    if Path::new(recorded) != Path::new(&opencode_location_directory(cwd)) {
        bail!(
            "OpenCode session `{session_id}` lives in `{recorded}`, not this task's directory `{}`",
            cwd.display()
        );
    }
    Ok(())
}

pub struct OpenCodeDriver {
    // `Drop` releases this lease before waking the worker, guaranteeing that
    // final process teardown runs on the worker rather than the UI thread.
    server: Option<PooledServer>,
    session_id: String,
    /// This task's workspace: every location-scoped request names it
    /// explicitly, since the server can no longer be assumed to run here.
    cwd: PathBuf,
    events: DriverEventSender,
    background_refresh_generation: Arc<AtomicU64>,
    background_transcript_hydrations: Arc<Mutex<HashSet<String>>>,
    commands: Sender<CommandMessage>,
    permissions: Arc<Mutex<OpenCodePermissionState>>,
    forms: Arc<Mutex<OpenCodeFormState>>,
    event_feed: Arc<EventFeed>,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    model: Option<String>,
    reasoning_effort: Option<String>,
}

impl OpenCodeDriver {
    pub fn start(options: DriverStartOptions, events: DriverEventSender) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier: _,
            context_window: _,
            agent_preset: _,
            provider_cursor,
            task_id,
        } = options;
        let resume_session_id = match provider_cursor {
            Some(ProviderResumeCursor::OpenCode { session_id }) => {
                (!session_id.is_empty()).then_some(session_id)
            }
            None => None,
        };

        // OpenCode hosts many sessions per process, and a second
        // `opencode serve` in the same workspace contends with the live one.
        let server = crate::opencode_pool::acquire(&binary, &cwd)?;

        // Reuse the native session when resuming so the conversation, and the
        // cursor already persisted for it, stay the same.
        let location_directory = opencode_location_directory(&cwd);
        let session_id = match resume_session_id {
            Some(session_id) => {
                let path = format!("/api/session/{}", encode_path_segment(&session_id));
                let existing = server
                    .request("GET", &path, None)
                    .with_context(|| format!("could not resume OpenCode session `{session_id}`"))?;
                verify_resume_location(&existing, &cwd, &session_id)?;
                session_id
            }
            None => {
                // The session records which workspace it belongs to: a server
                // shared across directories resolves location-scoped routes
                // through it instead of its own process working directory.
                // The directory also rides the header, so an instance selected
                // by it stores the session under the same workspace the body
                // names. `metadata` ties the session back to the app task
                // that owns it; a pre-2.0 CLI that rejects the field gets the
                // plain location body instead (2.0.4+ accepts it — verified
                // on 2.0.11 and 2.0.16).
                let location_body = json!({ "location": { "directory": &location_directory } });
                let body = match fintwind_task_metadata(task_id.as_deref()) {
                    Some(metadata) => {
                        let mut body = location_body.clone();
                        body["metadata"] = metadata;
                        body
                    }
                    None => location_body.clone(),
                };
                let created = if body == location_body {
                    server
                        .request_for_directory_with_timeout(
                            &location_directory,
                            "POST",
                            "/api/session",
                            Some(&location_body),
                            Duration::from_secs(10),
                        )
                        .context("could not open an OpenCode session")?
                } else {
                    crate::opencode_session::post_current_or_legacy(
                        |body| {
                            server.request_for_directory_with_timeout(
                                &location_directory,
                                "POST",
                                "/api/session",
                                Some(body),
                                Duration::from_secs(10),
                            )
                        },
                        &body,
                        Some(&location_body),
                    )
                    .context("could not open an OpenCode session")?
                };
                created
                    .pointer("/data/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("OpenCode returned no session ID"))?
            }
        };
        let _ = events.send(DriverEvent::Connected {
            provider_cursor: Some(ProviderResumeCursor::OpenCode {
                session_id: session_id.clone(),
            }),
        });

        // The agent decides Plan versus Build, and it is per session: a
        // resumed session gets the agent re-posted and OpenCode persists it
        // with the session.
        let agent = if interaction_mode == InteractionMode::Plan || mode == RuntimeMode::Plan {
            "plan"
        } else {
            "build"
        };
        let _ = server.request(
            "POST",
            &format!("/api/session/{}/agent", encode_path_segment(&session_id)),
            Some(&json!({"agent": agent})),
        );
        apply_opencode_session_permissions(&server, &session_id, mode, interaction_mode);

        // opencode keeps the model on the session instead of on every
        // prompt. A startup model override switches it once; later switches
        // would ride the same endpoint. The model reference on this wire is
        // `{id, providerID}`, unlike v1's `{providerID, modelID}`.
        if let Some(model) = model.as_ref() {
            if let Some(model_ref) = opencode_model_ref(model, reasoning_effort.as_deref()) {
                server
                    .request(
                        "POST",
                        &format!("/api/session/{}/model", encode_path_segment(&session_id)),
                        Some(&json!({"model": model_ref})),
                    )
                    .context("switch OpenCode model/variant")?;
            }
        }

        let usage_metadata = Arc::new(OpenCodeUsageMetadata::default());
        let previous_usage_path = format!(
            "/api/session/{}/message?limit=200",
            encode_path_segment(&session_id)
        );
        let previous_usage = server.request("GET", &previous_usage_path, None).ok();
        let previous_session = server
            .request(
                "GET",
                &format!("/api/session/{}", encode_path_segment(&session_id)),
                None,
            )
            .ok();
        let previous_session_info = previous_session
            .as_ref()
            .and_then(|value| value.get("data"))
            .or(previous_session.as_ref());
        let previous_info = previous_usage
            .as_ref()
            .and_then(|messages| latest_opencode_usage_message(messages).cloned());
        if let Some(info) = previous_info.as_ref() {
            if let Some(model) = opencode_message_model_key(info) {
                *usage_metadata.last_model.lock() = Some(model);
            }
        } else if let Some(model) = model.as_ref() {
            *usage_metadata.last_model.lock() = Some(model.clone());
        }
        // A fresh driver republishes the newest stored step so the meter and
        // the totals row are populated before the first live step lands.
        // Session-row totals, when present, are the TUI's own source of the
        // cumulative numbers; the message tail is only a floor.
        let seeds = previous_usage
            .as_ref()
            .map(|messages| opencode_usage_seeds(messages, previous_session_info))
            .unwrap_or_else(|| opencode_usage_seeds(&Value::Null, previous_session_info));
        if seeds.total > 0 {
            *usage_metadata.session_total.lock() = seeds.total;
        }
        if seeds.prompt > 0 {
            *usage_metadata.session_cache_read.lock() = seeds.cache_read;
            *usage_metadata.session_prompt.lock() = seeds.prompt;
        }
        if seeds.newest.is_some() || seeds.total > 0 || seeds.prompt > 0 {
            let (cache_read, prompt_tokens) = usage_metadata.session_cache();
            let _ = events.send(DriverEvent::UsageUpdated {
                context_tokens: seeds.newest.map(|usage| usage.context),
                latest: seeds.newest.map(|usage| usage.latest),
                context_window: None,
                // The catalog has not been read yet. None here means "not
                // carried", not "this model has no window".
                context_window_resolved: false,
                session_total: (seeds.total > 0).then_some(seeds.total),
                cache_read,
                prompt_tokens,
            });
        }
        // A compaction is durable and can outlive this driver: republish the
        // newest native compaction record so a restarted session still shows
        // the last attempt's outcome — including the stored summary the TUI
        // renders as a Compaction section. One that was still running
        // re-announces itself through `session.compaction.delta` even without
        // `started`.
        if let Some(state) = previous_usage
            .as_ref()
            .and_then(|messages| latest_opencode_compaction(messages))
        {
            let _ = events.send(DriverEvent::CompactionUpdated(state));
        }

        // `/api/model` can be cold on the first server in a directory. Resolve
        // it off the driver-start path so a slow catalog never delays the
        // transcript or turns an otherwise healthy provider into a 0% meter.
        // The stream records the actual provider/model key in parallel; when
        // the catalog lands, publish the matching window as a separate merge.
        // The thread holds only the port: a handle would delay the pooled
        // server's teardown behind this request's timeout.
        let metadata_port = server.port;
        let metadata_directory = location_directory.clone();
        let metadata_events = events.clone();
        let background_usage_metadata = usage_metadata.clone();
        thread::Builder::new()
            .name("fintwind-opencode-usage-metadata".into())
            .spawn(move || {
                // `/api/model` answers with an empty catalog until the server
                // warms it up. The first session of a workspace starts a cold
                // server, so poll until models land or the budget runs out;
                // later sessions share an already-warm server.
                let started = std::time::Instant::now();
                let budget = Duration::from_secs(30);
                let response = loop {
                    let request = crate::opencode_session::request_json_on_port_with_directory(
                        metadata_port,
                        "GET",
                        "/api/model",
                        None,
                        Duration::from_secs(30),
                        Some(&metadata_directory),
                    );
                    let landed = request.as_ref().is_ok_and(|response| {
                        response
                            .pointer("/data")
                            .and_then(Value::as_array)
                            .is_some_and(|data| !data.is_empty())
                    });
                    if landed || started.elapsed() >= budget {
                        break request;
                    }
                    thread::sleep(Duration::from_secs(2));
                };
                let Ok(response) = response else {
                    return;
                };
                let windows = opencode_model_context_windows(&response);
                *background_usage_metadata.model_context_windows.lock() = windows;
                let (context_window, context_window_resolved) =
                    background_usage_metadata.resolved_context_window();
                let (cache_read, prompt_tokens) = background_usage_metadata.session_cache();
                let total = *background_usage_metadata.session_total.lock();
                // A resolved miss must still be published: it is how a cached
                // window from another provider's copy of the same id is cleared.
                if context_window_resolved || total > 0 || prompt_tokens.is_some() {
                    let _ = metadata_events.send(DriverEvent::UsageUpdated {
                        context_tokens: None,
                        latest: None,
                        context_window,
                        context_window_resolved,
                        session_total: (total > 0).then_some(total),
                        cache_read,
                        prompt_tokens,
                    });
                }
            })?;

        let auto_approve = AutoApprove::from(mode);
        let (commands, command_rx) = unbounded();
        let turn_active = Arc::new(Mutex::new(false));
        let permissions = Arc::new(Mutex::new(OpenCodePermissionState::default()));
        let forms = Arc::new(Mutex::new(OpenCodeFormState::default()));
        // One shared SSE connection per server port: the hub fans the
        // server-wide stream out to every driver on it, and this driver
        // filters its own session family. It returns before the stream is
        // live; `recv` blocks until the first event or the stream's end.
        let event_feed = Arc::new(crate::opencode_events::subscribe(server.port)?);
        let session_family = Arc::new(SessionFamily::new(session_id.clone()));

        // opencode answers permission requests through a polling endpoint
        // (`GET /api/permission/request`) instead of the event stream v1
        // used, so a dedicated thread scans it and routes requests through
        // the same approval path the event handler used. The request shape
        // maps straight onto the v1 event payload: `action` is the
        // permission, `resources` the patterns, `save` the always-rules.
        // Pending items belong to a session; accept this driver's family
        // (parent plus descendants) and skip everyone else on the shared
        // server.
        let permission_port = server.port;
        let permission_events = events.clone();
        let permission_commands = commands.clone();
        let permission_state = Arc::clone(&permissions);
        let poll_forms = Arc::clone(&forms);
        let permission_feed = Arc::clone(&event_feed);
        let poll_family = Arc::clone(&session_family);
        thread::Builder::new()
            .name("fintwind-opencode-permissions".into())
            .spawn(move || {
                while !permission_feed.is_cancelled() {
                    if let Ok(pending) = crate::opencode_session::request_json_on_port(
                        permission_port,
                        "GET",
                        "/api/permission/request",
                        None,
                        Duration::from_secs(2),
                    ) {
                        let Some(requests) = pending.get("data").and_then(Value::as_array) else {
                            thread::sleep(PERMISSION_POLL_INTERVAL);
                            continue;
                        };
                        for request in requests {
                            if request.get("id").and_then(Value::as_str).is_none() {
                                continue;
                            }
                            let Some(request_session) = value_session_id(request) else {
                                continue;
                            };
                            if !poll_family.belongs(request_session, permission_port) {
                                continue;
                            }
                            let adapted = json!({
                                "id": request.get("id").cloned().unwrap_or(Value::Null),
                                "sessionID": request.get("sessionID").cloned().unwrap_or(Value::Null),
                                "permission": request.get("action").cloned().unwrap_or(Value::Null),
                                "patterns": request.get("resources").cloned().unwrap_or(Value::Null),
                                "always": request.get("save").cloned().unwrap_or(Value::Null),
                            });
                            let _ = request_permission(
                                &adapted,
                                &permission_events,
                                &permission_commands,
                                auto_approve,
                                &permission_state,
                            );
                        }
                    }
                    // The question tool's prompt rides a form, and the
                    // dedicated question events current releases stream are
                    // not emitted, so the poll is the safety net for a form
                    // the event stream dropped (or that predates this
                    // driver). The event path dedups through the same
                    // `announced` set. v2.0.11+ lists forms at `/api/form`
                    // (`{location, data}`); earlier CLIs used `/api/form/request`.
                    if let Ok(pending) = crate::opencode_session::request_form_list(
                        permission_port,
                        |path| {
                            crate::opencode_session::request_json_on_port(
                                permission_port,
                                "GET",
                                path,
                                None,
                                Duration::from_secs(2),
                            )
                        },
                    ) {
                        for form in pending
                            .get("data")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            let Some(form_session) = value_session_id(form) else {
                                continue;
                            };
                            if !poll_family.belongs(form_session, permission_port) {
                                continue;
                            }
                            let _ = request_user_input_from_form(
                                &json!({"form": form}),
                                &poll_forms,
                                &permission_events,
                            );
                        }
                    }
                    thread::sleep(PERMISSION_POLL_INTERVAL);
                }
            })?;

        // The feed holds only the port, never a server handle: the hub's
        // stream ends exactly when the process exits, so a handle held here
        // would keep the pooled server from ever being killed.
        let stream_port = server.port;
        let stream_session = session_id.clone();
        let stream_events = events.clone();
        let stream_event_sink = stream_events.clone();
        let stream_commands = commands.clone();
        let stream_turn = turn_active.clone();
        let stream_usage_metadata = usage_metadata;
        let stream_permissions = Arc::clone(&permissions);
        let stream_forms = Arc::clone(&forms);
        let stream_feed = Arc::clone(&event_feed);
        let stream_family = Arc::clone(&session_family);
        thread::Builder::new()
            .name("fintwind-opencode-events".into())
            .spawn(move || {
                let mut state = OpenCodeStreamState {
                    usage_metadata: stream_usage_metadata,
                    permissions: stream_permissions,
                    forms: stream_forms,
                    ..OpenCodeStreamState::default()
                };
                // The hub delivers parsed JSON already. A stream end is the
                // server going away — there is no reconnect.
                loop {
                    if stream_feed.is_cancelled() {
                        break;
                    }
                    match stream_feed.recv() {
                        Ok(value) => dispatch_server_event(
                            &value,
                            &stream_session,
                            &stream_event_sink,
                            &stream_commands,
                            &stream_turn,
                            stream_port,
                            auto_approve,
                            &mut state,
                            &stream_family,
                        ),
                        Err(_) => break,
                    }
                }
                if !stream_feed.is_cancelled() {
                    let _ = stream_event_sink.send(DriverEvent::ProcessExited);
                }
            })?;

        let worker_server = server.clone();
        let worker_session = session_id.clone();
        let worker_task_id = task_id.clone();
        let worker_events = events;
        let worker_turn = turn_active;
        let worker_forms = Arc::clone(&forms);
        thread::Builder::new()
            .name("fintwind-opencode-driver".into())
            .spawn(move || {
                while let Ok(message) = command_rx.recv() {
                    match message {
                        CommandMessage::Prompt { text, files } => {
                            *worker_turn.lock() = true;
                            let _ = worker_events.send(DriverEvent::TurnStarted);
                            // `prompt` acknowledges as soon as the prompt
                            // is accepted; completion arrives as
                            // `session.execution.succeeded` (or `failed`) on
                            // the event stream. The blocking message route
                            // holds its response for the whole turn, which no
                            // sane read timeout survives — a turn longer than
                            // the HTTP timeout would be falsely failed.
                            let path = format!(
                                "/api/session/{}/prompt",
                                encode_path_segment(&worker_session)
                            );
                            let posted = prompt_bodies(&text, &files, worker_task_id.as_deref())
                                .and_then(|(current, legacy)| {
                                    crate::opencode_session::post_current_or_legacy(
                                        |body| worker_server.request("POST", &path, Some(body)),
                                        &current,
                                        Some(&legacy),
                                    )
                                });
                            if let Err(error) = posted {
                                let _ = worker_events.send(DriverEvent::Error(tr!(
                                    "errors.provider_rejected_prompt_detail",
                                    provider = "OpenCode",
                                    error = error
                                )));
                                // `session.execution.succeeded` never arrives
                                // for a turn that failed to start, so settle
                                // it here instead of hanging.
                                if std::mem::take(&mut *worker_turn.lock()) {
                                    let _ = worker_events.send(DriverEvent::TurnFinished {
                                        success: false,
                                        summary: Some(tr!(
                                            "errors.provider_start_turn",
                                            provider = "OpenCode"
                                        )),
                                    });
                                }
                            }
                        }
                        CommandMessage::Steer { text, files } => {
                            // A prompt posted into a busy session is a steer:
                            // the server folds it into the running turn and one
                            // `session.execution.succeeded` still settles
                            // everything. `prompt` acknowledges as soon as the
                            // prompt is accepted, unlike the message route,
                            // which blocks until the merged turn ends — which
                            // is what makes it the steer vehicle.
                            if !*worker_turn.lock() {
                                let _ = worker_events.send(DriverEvent::SteerRejected {
                                    message: text,
                                    reason: tr!(
                                        "errors.provider_no_active_turn",
                                        provider = "OpenCode"
                                    ),
                                });
                                continue;
                            }
                            let path = format!(
                                "/api/session/{}/prompt",
                                encode_path_segment(&worker_session)
                            );
                            let posted = prompt_bodies(&text, &files, worker_task_id.as_deref())
                                .and_then(|(current, legacy)| {
                                    crate::opencode_session::post_current_or_legacy(
                                        |body| worker_server.request("POST", &path, Some(body)),
                                        &current,
                                        Some(&legacy),
                                    )
                                });
                            match posted {
                                Ok(_) => {
                                    let _ = worker_events
                                        .send(DriverEvent::SteerAccepted { message: text });
                                }
                                Err(error) => {
                                    let _ = worker_events.send(DriverEvent::SteerRejected {
                                        message: text,
                                        reason: tr!(
                                            "errors.provider_rejected_steer",
                                            provider = "OpenCode",
                                            error = error
                                        ),
                                    });
                                }
                            }
                        }
                        CommandMessage::Compact => {
                            // Compaction admission is durable: steer delivery
                            // runs at the next safe step boundary of a busy
                            // session, an idle one starts immediately, and a
                            // repeat while one is pending coalesces
                            // server-side — so a double-submit cannot
                            // double-compact. Outcomes ride the event stream
                            // as `session.compaction.*`.
                            let path = format!(
                                "/api/session/{}/compact",
                                encode_path_segment(&worker_session)
                            );
                            if let Err(error) =
                                worker_server.request("POST", &path, Some(&json!({})))
                            {
                                // A 409 means a compaction is already
                                // admitted — that is success from the user's
                                // point of view, and the event stream owns
                                // the outcome, so stay quiet rather than
                                // reporting a failure that is really
                                // "already running".
                                if !error.to_string().contains("HTTP 409") {
                                    let _ = worker_events.send(DriverEvent::CompactionUpdated(
                                        CompactionState {
                                            status: CompactionStatus::Failed,
                                            reason: Some("manual".into()),
                                            model: None,
                                            error: Some(error.to_string()),
                                            summary: None,
                                        },
                                    ));
                                }
                            }
                        }
                        CommandMessage::Cancel => {
                            let path = format!(
                                "/api/session/{}/interrupt",
                                encode_path_segment(&worker_session)
                            );
                            if let Err(error) = worker_server.request("POST", &path, None) {
                                let _ = worker_events.send(DriverEvent::Error(tr!(
                                    "errors.stop_provider",
                                    provider = "OpenCode",
                                    error = error
                                )));
                            }
                        }
                        CommandMessage::Respond {
                            request_id,
                            option_id,
                            session_id,
                        } => {
                            let target = session_id
                                .filter(|id| !id.is_empty())
                                .unwrap_or_else(|| worker_session.clone());
                            if let Err(error) = post_owned_reply(
                                |path| {
                                    post_permission_decision(
                                        |body| worker_server.request("POST", path, Some(body)),
                                        &option_id,
                                    )
                                },
                                |path| worker_server.request("GET", path, None),
                                |path| {
                                    post_permission_decision(
                                        |body| worker_server.request("POST", path, Some(body)),
                                        "reject",
                                    )
                                },
                                |session| permission_reply_path(session, &request_id),
                                &target,
                                &request_id,
                                "/api/permission/request",
                            ) {
                                let _ = worker_events.send(DriverEvent::Error(tr!(
                                    "errors.answer_provider_permission",
                                    provider = "OpenCode",
                                    error = error
                                )));
                            }
                        }
                        CommandMessage::RespondUserInput {
                            request_id,
                            answers,
                            session_id,
                        } => {
                            // Current opencode routes the question tool's
                            // answers through the form that carried the
                            // prompt; the dedicated question route stays for
                            // releases that still publish `question.asked`.
                            if request_id.starts_with("frm_") {
                                let (fields, stored_session) = {
                                    let forms = worker_forms.lock();
                                    (
                                        forms.fields.get(&request_id).cloned().unwrap_or_default(),
                                        forms.sessions.get(&request_id).cloned(),
                                    )
                                };
                                let target = session_id
                                    .filter(|id| !id.is_empty())
                                    .or(stored_session)
                                    .unwrap_or_else(|| worker_session.clone());
                                let body = json!({"answer": form_reply_answer(&fields, &answers)});
                                match post_owned_reply(
                                    |path| worker_server.request("POST", path, Some(&body)),
                                    // The list path is probed inside the
                                    // closure: v2.0.11+ answers `/api/form`,
                                    // earlier CLIs still use `/api/form/request`.
                                    |_path| {
                                        crate::opencode_session::request_form_list(
                                            worker_server.port,
                                            |path| worker_server.request("GET", path, None),
                                        )
                                    },
                                    |_| Ok(Value::Null),
                                    |session| form_reply_path(session, &request_id),
                                    &target,
                                    &request_id,
                                    "/api/form",
                                ) {
                                    Ok(_) => {
                                        let mut forms = worker_forms.lock();
                                        forms.fields.remove(&request_id);
                                        forms.sessions.remove(&request_id);
                                    }
                                    Err(error) => {
                                        let _ = worker_events.send(DriverEvent::Error(tr!(
                                            "errors.answer_provider_question",
                                            provider = "OpenCode",
                                            error = error
                                        )));
                                    }
                                }
                            } else {
                                let path = format!(
                                    "/api/question/{}/reply",
                                    encode_path_segment(&request_id)
                                );
                                let body = json!({
                                    "answers": answers
                                        .iter()
                                        .map(|answer| json!(answer.answers))
                                        .collect::<Vec<_>>()
                                });
                                if let Err(error) =
                                    worker_server.request("POST", &path, Some(&body))
                                {
                                    let _ = worker_events.send(DriverEvent::Error(tr!(
                                        "errors.answer_provider_question",
                                        provider = "OpenCode",
                                        error = error
                                    )));
                                }
                            }
                        }
                        CommandMessage::Shutdown => break,
                    }
                }
            })
            .inspect_err(|_| {
                event_feed.cancel();
            })?;

        Ok(Self {
            server: Some(server),
            session_id,
            cwd,
            events: stream_events,
            background_refresh_generation: Arc::new(AtomicU64::new(0)),
            background_transcript_hydrations: Arc::new(Mutex::new(HashSet::new())),
            commands,
            permissions,
            forms,
            event_feed,
            mode,
            interaction_mode,
            model,
            reasoning_effort,
        })
    }
}

fn opencode_model_ref(model: &str, variant: Option<&str>) -> Option<Value> {
    let (provider_id, model_id) = model.split_once('/')?;
    let mut reference = json!({"id": model_id, "providerID": provider_id});
    if let Some(variant) = variant {
        reference["variant"] = json!(variant);
    }
    Some(reference)
}

impl DriverControl for OpenCodeDriver {
    fn prompt(&self, prompt: String, files: Vec<PromptFile>) {
        let _ = self.commands.send(CommandMessage::Prompt {
            text: prompt,
            files,
        });
    }

    fn compact(&self) {
        let _ = self.commands.send(CommandMessage::Compact);
    }

    fn refresh_background_work(&self) {
        let Some(server) = self.server.as_ref() else {
            return;
        };
        // The thread holds only the port: a handle would delay the pooled
        // server's teardown behind this request's timeout.
        let port = server.port;
        let generation = self
            .background_refresh_generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let parent_id = self.session_id.clone();
        let directory = opencode_location_directory(&self.cwd);
        let events = self.events.clone();
        let generation_guard = Arc::clone(&self.background_refresh_generation);
        let transcript_hydrations = Arc::clone(&self.background_transcript_hydrations);
        let _ = thread::Builder::new()
            .name("fintwind-opencode-subagents-refresh".into())
            .spawn(move || {
                // A server shared across workspaces must only see this
                // directory's sessions. New servers filter by parent before
                // paging; old ones may ignore or reject the parameter. Page
                // both cases: a 200-row first page is not the whole roster.
                let sessions = (|| {
                    let mut sessions = Vec::new();
                    let mut cursor: Option<String> = None;
                    let mut filter_parent = true;
                    for _ in 0..50 {
                        if generation_guard.load(Ordering::Acquire) != generation {
                            return None;
                        }
                        let mut base = format!(
                            "/api/session?directory={}&limit=200",
                            encode_path_segment(&directory)
                        );
                        if let Some(token) = &cursor {
                            base.push_str(&format!("&cursor={}", encode_path_segment(token)));
                        }
                        let path = if filter_parent {
                            format!("{base}&parentID={}", encode_path_segment(&parent_id))
                        } else {
                            base.clone()
                        };
                        let request = |path: &str| {
                            request_json_on_port_with_directory(
                                port,
                                "GET",
                                path,
                                None,
                                Duration::from_secs(10),
                                Some(&directory),
                            )
                        };
                        let response = match request(&path) {
                            Err(error) if filter_parent && is_http_bad_request(&error) => {
                                filter_parent = false;
                                request(&base).ok()?
                            }
                            result => result.ok()?,
                        };
                        let rows = response.get("data")?.as_array()?;
                        if rows.len() == 200 && response.get("cursor").is_none() {
                            // Without a cursor, a full page may omit live
                            // children. Keep the previous roster instead.
                            return None;
                        }
                        let empty = rows.is_empty();
                        sessions.extend(rows.iter().cloned());
                        let next = response.pointer("/cursor/next").and_then(Value::as_str);
                        match next {
                            Some(next) if !empty && cursor.as_deref() != Some(next) => {
                                cursor = Some(next.to_owned());
                            }
                            _ => return Some(sessions),
                        }
                    }
                    // A partial roster would mark omitted live children Lost.
                    None
                })();
                if generation_guard.load(Ordering::Acquire) != generation {
                    return;
                }
                // A failed/partial listing is not evidence of missing children.
                let Some(sessions) = sessions else {
                    return;
                };
                let items = sessions
                    .into_iter()
                    .filter_map(|payload| {
                        let child_id = payload.get("id").and_then(Value::as_str)?;
                        (payload
                            .get("parentID")
                            .or_else(|| payload.get("parentId"))
                            .and_then(Value::as_str)
                            == Some(parent_id.as_str()))
                        .then(|| {
                            let mut child =
                                OpenCodeChildSession::new(child_id, &parent_id, &payload);
                            let status = payload
                                .pointer("/status/type")
                                .or_else(|| payload.get("status"))
                                .and_then(Value::as_str);
                            child.item.status = match status {
                                Some("busy" | "running" | "starting") => {
                                    BackgroundWorkStatus::Running
                                }
                                Some("error" | "failed") => BackgroundWorkStatus::Failed,
                                Some("idle" | "completed" | "success") => {
                                    BackgroundWorkStatus::Completed
                                }
                                _ => BackgroundWorkStatus::Starting,
                            };
                            child.item.can_stop = child.item.status.is_stoppable();
                            child.item
                        })
                    })
                    .collect::<Vec<_>>();
                let child_ids = items
                    .iter()
                    .map(|item| item.key.provider_id.clone())
                    .collect::<Vec<_>>();
                let _ = events.send(DriverEvent::BackgroundWork(
                    BackgroundWorkEvent::ReconcileLive { items },
                ));
                for child_id in child_ids {
                    if !transcript_hydrations.lock().insert(child_id.clone()) {
                        continue;
                    }
                    match super::native::fetch_transcript_on_port(port, &child_id) {
                        Ok(transcript) => {
                            let _ = events.send(DriverEvent::BackgroundWork(
                                BackgroundWorkEvent::Transcript(
                                    BackgroundWorkTranscriptEvent::Snapshot {
                                        key: BackgroundWorkKey::new(
                                            BackgroundWorkKind::Subagent,
                                            child_id,
                                        ),
                                        transcript: crate::model::BackgroundWorkTranscript {
                                            messages: transcript.messages,
                                            transcript_blocks: transcript.blocks,
                                            turns: transcript.turns,
                                        },
                                    },
                                ),
                            ));
                        }
                        Err(_) => {
                            transcript_hydrations.lock().remove(&child_id);
                        }
                    }
                }
            });
    }

    fn stop_background_work(&self, key: BackgroundWorkKey, control_id: String) {
        if key.kind != BackgroundWorkKind::Subagent {
            return;
        }
        let Some(server) = self.server.as_ref() else {
            return;
        };
        let port = server.port;
        let events = self.events.clone();
        let _ = thread::Builder::new()
            .name("fintwind-opencode-subagent-stop".into())
            .spawn(move || {
                let path = format!(
                    "/api/session/{}/interrupt",
                    encode_path_segment(&control_id)
                );
                match crate::opencode_session::request_json_on_port(
                    port,
                    "POST",
                    &path,
                    None,
                    Duration::from_secs(10),
                ) {
                    Ok(_) => emit_stopped_subagent(&events, key),
                    Err(error) => {
                        let _ = events.send(DriverEvent::BackgroundWork(
                            BackgroundWorkEvent::StopFailed {
                                key,
                                message: error.to_string(),
                            },
                        ));
                    }
                }
            });
    }

    fn supports_steer(&self) -> bool {
        true
    }

    fn steer(&self, prompt: String, files: Vec<PromptFile>) {
        let _ = self.commands.send(CommandMessage::Steer {
            text: prompt,
            files,
        });
    }

    fn cancel(&self) {
        let _ = self.commands.send(CommandMessage::Cancel);
    }

    fn respond(&self, request_id: String, option_id: String) {
        for (request_id, option_id, session_id) in
            permission_responses(&self.permissions, &request_id, &option_id)
        {
            let _ = self.commands.send(CommandMessage::Respond {
                request_id,
                option_id,
                session_id,
            });
        }
    }

    fn respond_user_input(&self, request_id: String, answers: Vec<UserInputAnswer>) {
        let session_id = self.forms.lock().sessions.get(&request_id).cloned();
        let _ = self.commands.send(CommandMessage::RespondUserInput {
            request_id,
            answers,
            session_id,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        // Agent, model and variant are session-level settings. Changing any
        // of them requires a fresh driver to apply the new configuration.
        options.mode == self.mode
            && options.interaction_mode == self.interaction_mode
            && options.model == self.model
            && options.reasoning_effort == self.reasoning_effort
    }

    fn fork(&self, turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        let server = self
            .server
            .as_deref()
            .ok_or_else(|| anyhow!("OpenCode driver is shutting down"))?;
        fork_session_removing_turns_on_server(server, &self.session_id, turns_to_remove)
    }
}

impl Drop for OpenCodeDriver {
    fn drop(&mut self) {
        self.event_feed.cancel();
        // The worker owns the other server lease. Release the UI-owned lease
        // first, then wake the worker so any final terminate/wait happens there.
        drop(self.server.take());
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

/// A subagent tool call whose child session is not known yet. `target` names
/// the child a resume continues; `None` means the call spawns a new child.
struct PendingSubagent {
    activity_id: String,
    prompt: Option<String>,
    target: Option<String>,
}

#[derive(Default)]
struct OpenCodeStreamState {
    tools: HashMap<String, (ActivityKind, String)>,
    pending_subagents: VecDeque<PendingSubagent>,
    children: HashMap<String, OpenCodeChildSession>,
    usage_metadata: Arc<OpenCodeUsageMetadata>,
    permissions: Arc<Mutex<OpenCodePermissionState>>,
    forms: Arc<Mutex<OpenCodeFormState>>,
    /// A compaction is live on the server (`started` seen, no terminal event
    /// yet). The summary's `delta` events only re-announce the running state
    /// when this flag is down — a driver restarted mid-compaction never sees
    /// the durable `started`, and the first delta promotes the session
    /// instead of leaving it silent.
    compaction_live: bool,
    /// Footer statistics for the foreground turn in flight. Steps accumulate
    /// here until the turn settles, then leave as one `TurnStatsUpdated`;
    /// `None` between turns and until the first `session.step.started`.
    turn_stats: Option<OpenCodeTurnStats>,
}

/// The live accumulator behind [`TurnStats`]. The step events carry no
/// provider-side streaming time (verified against the v2 event schema: only
/// a publication `timestamp`), so each step's duration falls back to the
/// driver's own wall clock between `session.step.started` and the step's
/// settlement event — the same wall the app's own turn timing reads, so the
/// two stay comparable.
#[derive(Default)]
struct OpenCodeTurnStats {
    stats: TurnStats,
    /// Wall-clock (ms) of the in-flight step's `session.step.started`, or
    /// `None` when the step began before this driver attached.
    step_started_at: Option<u64>,
}

/// One resume call the parent bound to a child, awaiting the child's own
/// `session.execution.started`. The prompt it carries opens that turn.
struct PendingRevive {
    activity_id: String,
    prompt: Option<String>,
}

struct OpenCodeChildSession {
    item: BackgroundWorkItem,
    prompt: Option<String>,
    tools: HashMap<String, (ActivityKind, String)>,
    /// When the child's own execution began; the session row can exist
    /// (and be listed) noticeably earlier.
    execution_started_at_ms: Option<u64>,
    /// Resume calls the parent bound but whose `session.execution.started` has
    /// not arrived yet. Each is a legitimate reopen of a settled child rather
    /// than a redelivered straggler. A child can be resumed several times in
    /// one parent step (parallel `task` calls), so the binds queue instead of
    /// overwriting one another; each start consumes one, and its prompt opens
    /// that turn rather than echoing the child's original spawn prompt.
    pending_revives: VecDeque<PendingRevive>,
}

impl OpenCodeChildSession {
    fn new(session_id: &str, parent_id: &str, payload: &Value) -> Self {
        let now = unix_time_millis();
        let prompt = child_prompt(payload);
        let mut item = BackgroundWorkItem::new(
            BackgroundWorkKind::Subagent,
            session_id,
            child_title(payload).unwrap_or_else(|| tr!("background.subagent")),
            BackgroundWorkStatus::Starting,
        );
        item.background = true;
        item.can_stop = true;
        item.control_id = Some(session_id.to_owned());
        item.parent_id = Some(parent_id.to_owned());
        item.started_at_ms = now;
        item.updated_at_ms = now;
        item.role = child_role(payload);
        item.model = child_model(payload);
        item.command = prompt.clone();
        Self {
            item,
            prompt,
            tools: HashMap::new(),
            execution_started_at_ms: None,
            pending_revives: VecDeque::new(),
        }
    }
}

/// Take the pending call that belongs to `child_id`: an exact resume first,
/// then the oldest spawn (whose child id was unknown until now). Same-named
/// resumes that outlive their child stay queued instead of being consumed by
/// an unrelated `session.created`.
fn take_pending_subagent(
    pending: &mut VecDeque<PendingSubagent>,
    child_id: &str,
) -> Option<PendingSubagent> {
    let index = pending
        .iter()
        .position(|pending| pending.target.as_deref() == Some(child_id))
        .or_else(|| pending.iter().position(|pending| pending.target.is_none()))?;
    pending.remove(index)
}

fn is_subagent_tool(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "task" | "subagent"
    )
}

fn subagent_prompt(input: Option<&Value>) -> Option<String> {
    let input = input?;
    ["prompt", "message", "task", "description"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The child session a subagent tool call resumes, when it names one. A task
/// that carries `sessionID` continues an existing child instead of creating a
/// new one, so the call is bound to that child directly rather than queued for
/// the next `session.created`.
fn subagent_session_id(input: Option<&Value>) -> Option<String> {
    let input = input?;
    ["sessionID", "sessionId", "session_id"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Record one more transcript call as an entry point into a background item,
/// keeping the first occurrence's order and dropping duplicates. Returns
/// whether the call was new.
fn push_origin_activity(origins: &mut Vec<String>, activity_id: String) -> bool {
    if origins.iter().any(|existing| existing == &activity_id) {
        return false;
    }
    origins.push(activity_id);
    true
}

/// Pending question forms and whether they were already announced.
///
/// opencode delivers the `question` tool's prompt as a *form*: a
/// `form.created` event whose `metadata.kind` is `"question"` and whose
/// fields carry the questions (`title` = header, `description` = question
/// text, `type` `"multiselect"` for multi-select). The field shapes are kept
/// per form id because the reply route requires them: a multiselect field
/// rejects a bare string with `FormInvalidAnswerError`, so the answer value
/// must be an array exactly for those fields.
#[derive(Default)]
struct OpenCodeFormState {
    fields: HashMap<String, Vec<(String, bool)>>,
    announced: HashSet<String>,
    sessions: HashMap<String, String>,
}

#[derive(Default)]
struct OpenCodeUsageMetadata {
    model_context_windows: Mutex<HashMap<String, u64>>,
    last_model: Mutex<Option<String>>,
    /// Cumulative tokens the provider has processed for this session —
    /// every settled step's prompt + output, seeded from the stored session
    /// totals when present. Absolute values flow outward, so consumers never
    /// re-sum and replays cannot double-count.
    session_total: Mutex<u64>,
    /// Cached prompt tokens summed across every settled step.
    session_cache_read: Mutex<u64>,
    /// Full prompt tokens (cache + uncached input) summed across every
    /// settled step — the denominator of the session cache hit rate.
    session_prompt: Mutex<u64>,
    /// Whether `session.usage.updated` has delivered the session's own
    /// cumulative token row. Those values are authoritative — the row already
    /// includes every settled step — so settled steps then stop adding their
    /// own split on top.
    authoritative_totals: Mutex<bool>,
}

#[derive(Clone, Debug, Default)]
struct OpenCodePermissionRequest {
    permission: String,
    patterns: Vec<String>,
    always: Vec<String>,
    session_id: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct OpenCodePermissionRule {
    permission: String,
    pattern: String,
}

#[derive(Default)]
struct OpenCodePermissionState {
    pending: HashMap<String, OpenCodePermissionRequest>,
    approved: HashSet<OpenCodePermissionRule>,
    announced: HashSet<String>,
}

impl OpenCodePermissionState {
    fn is_approved(&self, request: &OpenCodePermissionRequest) -> bool {
        !request.patterns.is_empty()
            && request.patterns.iter().all(|pattern| {
                self.approved.iter().any(|rule| {
                    opencode_wildcard_matches(&request.permission, &rule.permission)
                        && opencode_wildcard_matches(pattern, &rule.pattern)
                })
            })
    }

    fn remember(&mut self, request: &OpenCodePermissionRequest) {
        // Mirror OpenCode's own `always` handling exactly: only provider-
        // supplied reusable patterns become rules. An empty list deliberately
        // resolves the current request without broadening future access.
        self.approved
            .extend(request.always.iter().map(|pattern| OpenCodePermissionRule {
                permission: request.permission.clone(),
                pattern: pattern.clone(),
            }));
    }
}

fn opencode_wildcard_matches(input: &str, pattern: &str) -> bool {
    let input = input.replace('\\', "/");
    let pattern = pattern.replace('\\', "/");
    if pattern
        .strip_suffix(" *")
        .is_some_and(|prefix| input == prefix)
    {
        return true;
    }

    let input = input.chars().collect::<Vec<_>>();
    let mut previous = vec![false; input.len() + 1];
    previous[0] = true;
    for token in pattern.chars() {
        let mut current = vec![false; input.len() + 1];
        if token == '*' {
            current[0] = previous[0];
        }
        for index in 1..=input.len() {
            current[index] = match token {
                '*' => previous[index] || current[index - 1],
                '?' => previous[index - 1],
                literal => previous[index - 1] && literal == input[index - 1],
            };
        }
        previous = current;
    }
    previous[input.len()]
}

impl OpenCodeUsageMetadata {
    /// `None` means the catalog or the live model is not known yet, so a
    /// stored window must be left alone. `Some` means the catalog answered:
    /// the inner `None` is an unambiguous miss and must clear a cached size.
    fn resolved_context_window(&self) -> (Option<u64>, bool) {
        let Some(model) = self.last_model.lock().clone() else {
            return (None, false);
        };
        let windows = self.model_context_windows.lock();
        if windows.is_empty() {
            return (None, false);
        }
        (opencode_lookup_context_window(&windows, &model), true)
    }

    fn session_cache(&self) -> (Option<u64>, Option<u64>) {
        let prompt = *self.session_prompt.lock();
        let cache = *self.session_cache_read.lock();
        (
            (prompt > 0).then_some(cache),
            (prompt > 0).then_some(prompt),
        )
    }
}

fn opencode_model_context_windows(response: &Value) -> HashMap<String, u64> {
    response
        .pointer("/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let provider = model.get("providerID").and_then(Value::as_str)?;
            let id = model.get("id").and_then(Value::as_str)?;
            let window = model
                .pointer("/limit/context")
                .and_then(Value::as_u64)
                .or_else(|| model.pointer("/limit/input").and_then(Value::as_u64))
                .or_else(|| model.get("contextWindow").and_then(Value::as_u64))
                .filter(|window| *window > 0)?;
            Some((format!("{provider}/{id}"), window))
        })
        .collect()
}

/// Match a live `provider/id` key against the catalog.
///
/// Casing may differ from the session key. An id-only fallback is safe only
/// when one catalog entry has that id: `fushengyunsuan/gpt-6-sol` and
/// `opencode/gpt-6-sol` are different windows, and guessing by id reports the
/// official 1.05M limit instead of the limit the user recorded on the custom
/// provider.
fn opencode_lookup_context_window(windows: &HashMap<String, u64>, model: &str) -> Option<u64> {
    if let Some(window) = windows.get(model).copied() {
        return Some(window);
    }
    if let Some(window) = windows
        .iter()
        .find_map(|(key, window)| key.eq_ignore_ascii_case(model).then_some(*window))
    {
        return Some(window);
    }
    let id = model.rsplit_once('/').map(|(_, id)| id).unwrap_or(model);
    let mut found = None;
    for (key, window) in windows {
        let catalog_id = key
            .rsplit_once('/')
            .map(|(_, id)| id)
            .unwrap_or(key.as_str());
        if !catalog_id.eq_ignore_ascii_case(id) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(*window);
    }
    found
}

/// The session-level token row carried by `session.usage.updated` — the same
/// shape the session record stores. `input` is the cache-excluded sum across
/// every settled call, so the object is throughput, not context: read as
/// occupancy it reported the whole session's processed input (545.7k on a
/// session whose live context was 147.3k, and 712.4k on the next turn).
struct SessionUsageRow {
    total: u64,
    cache_read: u64,
    prompt: u64,
}

fn opencode_session_row_usage(payload: &Value) -> Option<SessionUsageRow> {
    let tokens = payload.get("tokens")?;
    let input = tokens.get("input").and_then(Value::as_u64)?;
    let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0);
    let reasoning = tokens.get("reasoning").and_then(Value::as_u64).unwrap_or(0);
    let cache_read = tokens
        .pointer("/cache/read")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = tokens
        .pointer("/cache/write")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let prompt = input.saturating_add(cache_read).saturating_add(cache_write);
    let total = prompt.saturating_add(output).saturating_add(reasoning);
    (total > 0).then_some(SessionUsageRow {
        total,
        cache_read,
        prompt,
    })
}

/// One model call's usage in a single semantics: `prompt` is the full prompt
/// the call charged — cache read, cache write, and uncached input together —
/// and the denominator of the cache hit rate; `context` is prompt + output,
/// i.e. the occupancy number the meter already shows.
#[derive(Clone, Copy, Debug)]
struct UsageBreakdown {
    context: u64,
    prompt: u64,
    cache_read: u64,
    latest: crate::model::LatestCallUsage,
}

/// The normalized usage shape carried by an assistant message and by
/// `session.step.ended`: `input` excludes cached tokens, so the cache fields
/// are additions rather than subsets. `total`, when present, reports the
/// same context outright.
fn opencode_normalized_usage(message: &Value) -> Option<UsageBreakdown> {
    let tokens = message.get("tokens")?;
    let input = tokens.get("input").and_then(Value::as_u64).unwrap_or(0);
    let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0);
    let reasoning = tokens.get("reasoning").and_then(Value::as_u64).unwrap_or(0);
    let cache_read = tokens
        .pointer("/cache/read")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = tokens
        .pointer("/cache/write")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let prompt = input.saturating_add(cache_read).saturating_add(cache_write);
    let context = tokens
        .get("total")
        .and_then(Value::as_u64)
        .filter(|total| *total > 0)
        .unwrap_or_else(|| prompt.saturating_add(output).saturating_add(reasoning));
    (context > 0).then_some(UsageBreakdown {
        context,
        prompt,
        cache_read,
        latest: crate::model::LatestCallUsage {
            input,
            cache_read,
            cache_write,
            output,
            reasoning,
        },
    })
}

/// Session-wide usage seed: occupancy of the newest call, plus the
/// session's cumulative totals. Prefers the session row's stored totals
/// (the TUI's own source) when present; otherwise sums the fetched
/// assistant-message tail as a floor, never an over-count.
struct UsageSeed {
    newest: Option<UsageBreakdown>,
    total: u64,
    cache_read: u64,
    prompt: u64,
}

fn opencode_session_row_totals(session: &Value) -> Option<(u64, u64, u64)> {
    let tokens = session.get("tokens");
    let input = tokens
        .and_then(|tokens| tokens.get("input"))
        .and_then(Value::as_u64)
        .or_else(|| session.get("tokens_input").and_then(Value::as_u64))
        .unwrap_or(0);
    let output = tokens
        .and_then(|tokens| tokens.get("output"))
        .and_then(Value::as_u64)
        .or_else(|| session.get("tokens_output").and_then(Value::as_u64))
        .unwrap_or(0);
    let reasoning = tokens
        .and_then(|tokens| tokens.get("reasoning"))
        .and_then(Value::as_u64)
        .or_else(|| session.get("tokens_reasoning").and_then(Value::as_u64))
        .unwrap_or(0);
    let cache_read = tokens
        .and_then(|tokens| tokens.pointer("/cache/read"))
        .and_then(Value::as_u64)
        .or_else(|| session.get("tokens_cache_read").and_then(Value::as_u64))
        .unwrap_or(0);
    let cache_write = tokens
        .and_then(|tokens| tokens.pointer("/cache/write"))
        .and_then(Value::as_u64)
        .or_else(|| session.get("tokens_cache_write").and_then(Value::as_u64))
        .unwrap_or(0);
    let prompt = input.saturating_add(cache_read).saturating_add(cache_write);
    let total = prompt.saturating_add(output).saturating_add(reasoning);
    (total > 0 || prompt > 0).then_some((total, cache_read, prompt))
}

fn opencode_usage_seeds(messages: &Value, session: Option<&Value>) -> UsageSeed {
    let mut seed = UsageSeed {
        newest: None,
        total: 0,
        cache_read: 0,
        prompt: 0,
    };
    if let Some(data) = messages.pointer("/data").and_then(Value::as_array) {
        for message in data {
            if message.get("type").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(usage) = opencode_normalized_usage(message) else {
                continue;
            };
            seed.total = seed.total.saturating_add(usage.context);
            seed.cache_read = seed.cache_read.saturating_add(usage.cache_read);
            seed.prompt = seed.prompt.saturating_add(usage.prompt);
            seed.newest.get_or_insert(usage);
        }
    }
    if let Some((total, cache_read, prompt)) = session.and_then(opencode_session_row_totals) {
        seed.total = total;
        seed.cache_read = cache_read;
        seed.prompt = prompt;
    }
    seed
}

/// The model key (`provider/id`) of an opencode assistant message or
/// `session.step.started` payload, where `model` is an object.
fn opencode_message_model_key(message: &Value) -> Option<String> {
    let model = message.get("model")?;
    let provider = model.get("providerID").and_then(Value::as_str)?;
    let id = model.get("id").and_then(Value::as_str)?;
    Some(format!("{provider}/{id}"))
}

/// The reasoning fragment identity shared by `session.reasoning.started`,
/// `-delta`, and `-ended`: the (assistant message, ordinal) pair the stored
/// reasoning part is keyed by. The `reasoning:` prefix keeps the key out of
/// the tool call-id namespace that activity matching also uses. Fields that
/// a degraded stream omits fall back to stable defaults, so all three events
/// of one fragment still derive the same key.
fn reasoning_part_key(payload: &Value) -> String {
    let message = payload
        .get("assistantMessageID")
        .and_then(Value::as_str)
        .unwrap_or("msg");
    let ordinal = payload.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
    format!("reasoning:{message}:{ordinal}")
}

/// The text part identity shared by `session.text.started`, `-delta`, and
/// `-ended`. The `text:` prefix keeps it out of the reasoning and tool-call
/// namespaces. A degraded stream that omits the fields still derives one
/// stable key, matching [`reasoning_part_key`].
fn text_part_key(payload: &Value) -> String {
    let message = payload
        .get("assistantMessageID")
        .and_then(Value::as_str)
        .unwrap_or("msg");
    let ordinal = payload.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
    format!("text:{message}:{ordinal}")
}

/// The compaction summary's model, reported by `session.compaction.ended`.
/// The wire has carried both a bare `provider/id` string and the step
/// events' `{providerID, id}` object; accept either.
fn opencode_compaction_model(payload: &Value) -> Option<String> {
    match payload.get("model") {
        Some(Value::String(model)) => Some(model.clone()),
        // The step events' `{providerID, id}` object, read off the payload
        // itself — the key helper expects the enveloping object.
        Some(Value::Object(_)) => opencode_message_model_key(payload),
        _ => None,
    }
}

/// The newest native compaction record in a stored message list, as app
/// state. The endpoint answers newest-first, so the first compaction entry
/// is the latest attempt; records whose status is unknown are skipped
/// rather than guessed at.
fn opencode_compaction_summary(payload: &Value) -> Option<String> {
    payload
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let parts = payload.get("content").and_then(Value::as_array)?;
            let text = parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n");
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        })
}

fn fetch_latest_opencode_compaction(port: u16, session_id: &str) -> Option<CompactionState> {
    if port == 0 || session_id.is_empty() {
        return None;
    }
    let path = format!(
        "/api/session/{}/message?limit=20",
        encode_path_segment(session_id)
    );
    let messages = crate::opencode_session::request_json_on_port(
        port,
        "GET",
        &path,
        None,
        Duration::from_secs(10),
    )
    .ok()?;
    latest_opencode_compaction(&messages)
}

fn latest_opencode_compaction(messages: &Value) -> Option<CompactionState> {
    let data = messages.pointer("/data").and_then(Value::as_array)?;
    let record = data
        .iter()
        .find(|message| message.get("type").and_then(Value::as_str) == Some("compaction"))?;
    let status = match record.get("status").and_then(Value::as_str) {
        Some("completed") => CompactionStatus::Completed,
        Some("failed") => CompactionStatus::Failed,
        Some("running") | Some("pending") => CompactionStatus::Running,
        _ => return None,
    };
    Some(CompactionState {
        status,
        reason: record
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        model: opencode_compaction_model(record),
        error: record
            .get("error")
            .and_then(|error| {
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| error.as_str())
            })
            .map(str::to_owned),
        summary: opencode_compaction_summary(record),
    })
}

fn latest_opencode_usage_message(messages: &Value) -> Option<&Value> {
    let data = messages.pointer("/data").and_then(Value::as_array)?;
    // The native endpoint returns the newest message first.
    data.iter().find(|message| {
        message.get("type").and_then(Value::as_str) == Some("assistant")
            && message.get("tokens").is_some()
    })
}

fn event_payload(value: &Value) -> &Value {
    value
        .get("data")
        .or_else(|| value.get("properties"))
        .unwrap_or(&Value::Null)
}

fn event_session_id(value: &Value) -> Option<&str> {
    let payload = event_payload(value);
    payload
        .get("sessionID")
        .or_else(|| payload.get("sessionId"))
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/session/id").and_then(Value::as_str))
        .or_else(|| {
            payload
                .pointer("/session/sessionID")
                .and_then(Value::as_str)
        })
        .or_else(|| payload.pointer("/info/id").and_then(Value::as_str))
        .or_else(|| {
            payload.get("id").and_then(Value::as_str).filter(|_| {
                matches!(
                    value.get("type").and_then(Value::as_str),
                    Some("session.created" | "session.updated" | "session.deleted")
                )
            })
        })
        .or_else(|| payload.pointer("/form/sessionID").and_then(Value::as_str))
}

fn child_session_value(payload: &Value) -> &Value {
    payload
        .get("session")
        .or_else(|| payload.get("info"))
        .unwrap_or(payload)
}

fn child_parent_id(value: &Value) -> Option<String> {
    let payload = event_payload(value);
    let session = child_session_value(payload);
    session
        .get("parentID")
        .or_else(|| session.get("parentId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn child_prompt(payload: &Value) -> Option<String> {
    let session = child_session_value(payload);
    [
        session.get("prompt"),
        session.pointer("/prompt/text"),
        session.get("text"),
        session.pointer("/message/text"),
        session.pointer("/info/prompt"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(str::trim)
    .find(|text| !text.is_empty())
    .map(str::to_owned)
}

fn child_title(payload: &Value) -> Option<String> {
    let session = child_session_value(payload);
    session
        .get("title")
        .or_else(|| session.pointer("/info/title"))
        .or_else(|| session.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
}

fn child_role(payload: &Value) -> Option<String> {
    let session = child_session_value(payload);
    session
        .get("agent")
        .or_else(|| session.get("role"))
        .or_else(|| session.pointer("/info/agent"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn child_model(payload: &Value) -> Option<String> {
    let session = child_session_value(payload);
    let model = session
        .get("model")
        .or_else(|| session.pointer("/info/model"))?;
    if let Some(model) = model.as_str() {
        return Some(model.to_owned());
    }
    let provider = model.get("providerID").and_then(Value::as_str)?;
    let id = model.get("id").and_then(Value::as_str)?;
    Some(format!("{provider}/{id}"))
}

fn child_activity_event(
    key: &BackgroundWorkKey,
    activity: ActivityItem,
    events: &impl DriverEventSink,
) {
    let _ = events.send(DriverEvent::BackgroundWork(
        BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Activity {
            key: key.clone(),
            activity,
        }),
    ));
}

fn emit_stopped_subagent(events: &impl DriverEventSink, key: BackgroundWorkKey) {
    let mut item = BackgroundWorkItem::new(
        BackgroundWorkKind::Subagent,
        key.provider_id.clone(),
        String::new(),
        BackgroundWorkStatus::Stopped,
    );
    item.background = true;
    let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
        item,
    )));
    let _ = events.send(DriverEvent::BackgroundWork(
        BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Finished {
            key,
            success: false,
        }),
    ));
}

fn child_update(
    child: &mut OpenCodeChildSession,
    events: &impl DriverEventSink,
    status: Option<BackgroundWorkStatus>,
    detail: Option<String>,
) {
    if let Some(status) = status {
        child.item.status = status;
        child.item.can_stop = status.is_stoppable();
    }
    if detail.is_some() {
        child.item.detail = detail;
    }
    child.item.updated_at_ms = unix_time_millis();
    let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
        child.item.clone(),
    )));
}

fn dispatch_server_event(
    value: &Value,
    root: &str,
    events: &impl DriverEventSink,
    commands: &Sender<CommandMessage>,
    turn_active: &Mutex<bool>,
    port: u16,
    auto_approve: impl Into<AutoApprove>,
    state: &mut OpenCodeStreamState,
    family: &SessionFamily,
) {
    let auto_approve = auto_approve.into();
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let session = event_session_id(value);
    let lifecycle = matches!(
        kind,
        "session.created" | "session.updated" | "session.deleted"
    );

    if let Some(session) = session.filter(|session| *session != root) {
        let known_child = state
            .children
            .get(session)
            .is_some_and(|child| child.item.parent_id.as_deref() == Some(root));
        // Lifecycle events can announce a new child before its session id
        // is otherwise known. Route only sessions whose payload explicitly
        // points at this foreground session.
        let announced_child = matches!(kind, "session.created" | "session.updated")
            && child_parent_id(value).as_deref() == Some(root);
        if known_child
            || announced_child
            || (is_session_prompt_event(kind) && family.contains(session))
        {
            handle_child_event(value, root, events, commands, auto_approve, state);
            if state.children.contains_key(session) {
                family.remember(session.to_owned());
            }
            return;
        }
        // Other clients share this server; their sessions never touch this
        // transcript, but the sidebar still reconciles against the server's
        // roster when one appears or goes.
        if lifecycle {
            let _ = events.send(DriverEvent::NativeSessionsChanged);
        }
        return;
    }

    if !lifecycle && session.is_none() {
        return;
    }
    handle_event(
        value,
        events,
        commands,
        turn_active,
        port,
        root,
        auto_approve,
        state,
    );
}

fn handle_child_event(
    value: &Value,
    parent_id: &str,
    events: &impl DriverEventSink,
    commands: &Sender<CommandMessage>,
    auto_approve: impl Into<AutoApprove>,
    state: &mut OpenCodeStreamState,
) {
    let auto_approve = auto_approve.into();
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = event_payload(value);
    let Some(session_id) = event_session_id(value).map(str::to_owned) else {
        return;
    };
    // A subagent can request permissions or ask the user questions exactly
    // like the foreground session. Those prompts belong to the child
    // session and can race ahead of `session.created`, so they must not
    // wait for a child row.
    if kind.starts_with("permission.") {
        request_permission(payload, events, commands, auto_approve, &state.permissions);
        return;
    }
    match kind {
        "form.created" => {
            let _ = request_user_input_from_form(payload, &state.forms, events);
            return;
        }
        "form.replied" | "form.cancelled" => {
            let form = payload.get("form").unwrap_or(payload);
            if let Some(id) = form.get("id").and_then(Value::as_str) {
                let mut forms = state.forms.lock();
                forms.fields.remove(id);
                forms.sessions.remove(id);
                forms.announced.remove(id);
            }
            return;
        }
        _ => {}
    }
    if kind == "session.deleted" {
        // Remove the entry before settling it: late deltas and a replayed
        // `execution.started` must not revive a deleted child.
        if let Some(mut child) = state.children.remove(&session_id) {
            child_update(
                &mut child,
                events,
                Some(BackgroundWorkStatus::Stopped),
                Some(tr!("background.child_deleted")),
            );
            let _ = events.send(DriverEvent::BackgroundWork(
                BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Finished {
                    key: child.item.key.clone(),
                    success: false,
                }),
            ));
        }
        return;
    }
    if kind == "session.created" {
        if child_parent_id(value).as_deref() != Some(parent_id) {
            return;
        }
        let is_new = !state.children.contains_key(&session_id);
        let child = state
            .children
            .entry(session_id.clone())
            .or_insert_with(|| OpenCodeChildSession::new(&session_id, parent_id, payload));
        if is_new
            && let Some(pending) = take_pending_subagent(&mut state.pending_subagents, &session_id)
        {
            push_origin_activity(&mut child.item.origin_activity_ids, pending.activity_id);
            child.prompt = child.prompt.clone().or(pending.prompt);
        }
        child.prompt = child.prompt.clone().or_else(|| child_prompt(payload));
        if let Some(title) = child_title(payload) {
            child.item.title = title;
        }
        child.item.role = child.item.role.clone().or_else(|| child_role(payload));
        child.item.model = child.item.model.clone().or_else(|| child_model(payload));
        child.item.command = child.item.command.clone().or_else(|| child.prompt.clone());
        let key = child.item.key.clone();
        let prompt = child.prompt.clone();
        // Only a genuinely new (or still-live) child opens a transcript turn
        // here. A duplicate `session.created` for a settled child must stay
        // quiet: its resume turn belongs to the later `execution.started`.
        let starts_turn = is_new || child.item.status.is_live();
        child_update(child, events, None, None);
        if starts_turn {
            let _ = events.send(DriverEvent::BackgroundWork(
                BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Started {
                    key,
                    prompt,
                }),
            ));
        }
        return;
    }

    let Some(child) = state.children.get_mut(&session_id) else {
        return;
    };
    // A settled child's stragglers on the event stream must not reopen its
    // transcript; only metadata updates still apply. A `session.execution.
    // started` reopens it only when the parent just bound a new tool call to
    // this child — a redelivered start with no such binding is a straggler.
    if !child.item.status.is_live()
        && kind != "session.updated"
        && !(kind == "session.execution.started" && !child.pending_revives.is_empty())
    {
        return;
    }
    let key = child.item.key.clone();
    match kind {
        "session.updated" => {
            if let Some(title) = child_title(payload) {
                child.item.title = title;
            }
            child.item.role = child_role(payload).or_else(|| child.item.role.clone());
            child.item.model = child_model(payload).or_else(|| child.item.model.clone());
            child_update(child, events, None, None);
        }
        "session.execution.started" => {
            // A child that is already live can only be looking at a redelivered
            // start — executions do not overlap — so it must not consume a
            // resume that is still waiting for its own turn. Only a settled
            // child's start is the bound resume reopening it.
            let was_live = child.item.status.is_live();
            child.execution_started_at_ms = Some(unix_time_millis());
            // The resume has been consumed; a later redelivered start must not
            // reopen the child after it settles again.
            let prompt = if was_live {
                child.prompt.clone()
            } else {
                child
                    .pending_revives
                    .pop_front()
                    .and_then(|revive| revive.prompt)
                    .or_else(|| child.prompt.clone())
            };
            child_update(child, events, Some(BackgroundWorkStatus::Running), None);
            let _ = events.send(DriverEvent::BackgroundWork(
                BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Started {
                    key,
                    prompt,
                }),
            ));
        }
        "session.text.delta" => {
            if let Some(delta) = payload
                .get("delta")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
            {
                let _ = events.send(DriverEvent::BackgroundWork(
                    BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::TextDelta {
                        key,
                        delta: delta.to_owned(),
                    }),
                ));
            }
        }
        "session.reasoning.delta" => {
            if let Some(delta) = payload
                .get("delta")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
            {
                let _ = events.send(DriverEvent::BackgroundWork(
                    BackgroundWorkEvent::Transcript(
                        BackgroundWorkTranscriptEvent::ReasoningDelta {
                            key,
                            delta: delta.to_owned(),
                        },
                    ),
                ));
            }
        }
        "session.tool.input.started" => {
            if let (Some(id), Some(name)) = (
                payload.get("id").and_then(Value::as_str),
                payload.get("name").and_then(Value::as_str),
            ) {
                child.tools.insert(
                    id.to_owned(),
                    (super::support::classify_tool(name), name.to_owned()),
                );
                child_activity_event(&key, pending_tool_activity(id, name), events);
            }
        }
        "session.tool.called" => {
            if let Some(id) = payload.get("id").and_then(Value::as_str) {
                let stored = child.tools.get(id).cloned();
                let activity_kind = stored
                    .as_ref()
                    .map(|(kind, _)| *kind)
                    .unwrap_or(ActivityKind::Tool);
                let title = stored
                    .map(|(_, title)| title)
                    .unwrap_or_else(|| tr!("activity.tool"));
                let arguments = payload.get("input");
                let item = activity::tool_activity(
                    Some(id.to_owned()),
                    activity_kind,
                    title,
                    arguments,
                    None,
                    payload.get("input"),
                    false,
                    false,
                );
                child_activity_event(&key, item, events);
            }
        }
        "session.tool.progress" => {
            if let Some(id) = payload.get("id").and_then(Value::as_str)
                && let Some((kind, title)) = child.tools.get(id).cloned()
            {
                let item = activity::tool_activity(
                    Some(id.to_owned()),
                    kind,
                    title,
                    payload.get("input"),
                    None,
                    Some(payload),
                    false,
                    false,
                );
                child_activity_event(&key, item, events);
            }
        }
        "session.tool.success" | "session.tool.error" | "session.tool.failed" => {
            if let Some(id) = payload.get("id").and_then(Value::as_str) {
                let failed = kind != "session.tool.success";
                let stored = child.tools.remove(id);
                let activity_kind = stored
                    .as_ref()
                    .map(|(kind, _)| *kind)
                    .unwrap_or(ActivityKind::Tool);
                let title = stored
                    .map(|(_, title)| title)
                    .unwrap_or_else(|| tr!("activity.tool"));
                let output = failed
                    .then(|| payload.pointer("/error").unwrap_or(&Value::Null).clone())
                    .filter(|value| !value.is_null())
                    .or_else(|| payload.get("content").cloned());
                let item = activity::tool_activity(
                    Some(id.to_owned()),
                    activity_kind,
                    title,
                    payload.get("input"),
                    output.as_ref(),
                    Some(payload),
                    failed,
                    true,
                );
                child_activity_event(&key, item, events);
            }
        }
        "session.execution.succeeded" | "session.execution.failed" => {
            let success = kind.ends_with("succeeded");
            let detail = (!success)
                .then(|| {
                    payload
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .flatten();
            child.item.duration_ms = Some(
                unix_time_millis().saturating_sub(
                    child
                        .execution_started_at_ms
                        .unwrap_or(child.item.started_at_ms),
                ),
            );
            child_update(
                child,
                events,
                Some(if success {
                    BackgroundWorkStatus::Completed
                } else {
                    BackgroundWorkStatus::Failed
                }),
                detail,
            );
            let _ = events.send(DriverEvent::BackgroundWork(
                BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Finished {
                    key,
                    success,
                }),
            ));
        }
        _ => {}
    }
}
fn handle_event(
    value: &Value,
    events: &impl DriverEventSink,
    commands: &Sender<CommandMessage>,
    turn_active: &Mutex<bool>,
    port: u16,
    session_id: &str,
    auto_approve: impl Into<AutoApprove>,
    state: &mut OpenCodeStreamState,
) {
    let auto_approve = auto_approve.into();
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // opencode's `/api/event` payloads carry their fields under `data`; the
    // old v1 compatibility stream (still advertised by some forks) used
    // `properties`, tolerated here at no cost.
    let payload = value
        .get("data")
        .or_else(|| value.get("properties"))
        .unwrap_or(&Value::Null);

    match kind {
        "session.text.started" => {
            // Durable boundary of one persisted text part. The app reserves
            // the message here so a tool that lands before the batched tail
            // anchors after the sentence, not in the middle of it.
            let _ = events.send(DriverEvent::TextStarted {
                part: text_part_key(payload),
            });
        }
        "session.text.delta" => {
            let Some(delta) = payload.get("delta").and_then(Value::as_str) else {
                return;
            };
            if delta.is_empty() {
                return;
            }
            let _ = events.send(DriverEvent::TextDelta {
                part: text_part_key(payload),
                delta: delta.to_owned(),
            });
        }
        "session.text.ended" => {
            // `text` is the part's full value — the exact text the stored part
            // keeps. The live message is rewritten from it, the same way a
            // reasoning fragment settles.
            let _ = events.send(DriverEvent::TextEnded {
                part: text_part_key(payload),
                text: payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
        "session.reasoning.started" => {
            // Durable boundary of one persisted reasoning part. Forwarded so
            // the app keys its live block the way the stored part is keyed;
            // deltas may trail tool events, so position alone cannot group
            // them.
            let _ = events.send(DriverEvent::ReasoningStarted {
                part: reasoning_part_key(payload),
            });
        }
        "session.reasoning.delta" => {
            let Some(delta) = payload.get("delta").and_then(Value::as_str) else {
                return;
            };
            if delta.is_empty() {
                return;
            }
            let _ = events.send(DriverEvent::ReasoningDelta {
                part: reasoning_part_key(payload),
                delta: delta.to_owned(),
            });
        }
        "session.reasoning.ended" => {
            // Durable and authoritative: `text` is the fragment's full value —
            // the exact text the stored reasoning part keeps. Rewriting the
            // live block from it heals deltas lost to reordering or a
            // redelivered stream.
            let _ = events.send(DriverEvent::ReasoningEnded {
                part: reasoning_part_key(payload),
                text: payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
        "session.step.started" => {
            // The step announces the model that will run it; later usage
            // events carry tokens but no model. The same announcement feeds
            // the footer statistics: the footer reads the final step's
            // values, so each step overwrites the model/agent pair it
            // actually carries — an announcement missing either (a degraded
            // stream) keeps the earlier step's value instead of blanking it —
            // and arms the wall clock the step's duration falls back to.
            let model = opencode_message_model_key(payload);
            if let Some(model) = model.clone() {
                *state.usage_metadata.last_model.lock() = Some(model);
            }
            let agent = payload
                .get("agent")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .filter(|agent| !agent.trim().is_empty());
            // An accumulator left over from a turn this driver never saw
            // settle would bleed its tokens into the next turn, so a step
            // starting while no turn is active starts from zero.
            let stale = state.turn_stats.is_some() && !*turn_active.lock();
            let mut stats = match (state.turn_stats.take(), stale) {
                (Some(existing), false) => existing,
                _ => OpenCodeTurnStats::default(),
            };
            if model.is_some() {
                stats.stats.model = model;
            }
            if agent.is_some() {
                stats.stats.agent = agent;
            }
            stats.step_started_at = Some(unix_time_millis());
            state.turn_stats = Some(stats);
        }
        "session.usage.updated" => {
            // opencode publishes the session's cumulative token row here —
            // the same object the session record stores, `input` being the
            // cache-excluded sum across every settled call. Read as the
            // in-flight context it reported 545.7k against a 147.3k live
            // context (and 712.4k on the next turn), so occupancy never
            // comes from this event: it belongs to settled steps below.
            // The row is authoritative for the totals, so settled steps
            // stop adding their own split once it has landed.
            let Some(row) = opencode_session_row_usage(payload) else {
                return;
            };
            {
                let mut total = state.usage_metadata.session_total.lock();
                *total = row.total;
            }
            {
                let mut cache = state.usage_metadata.session_cache_read.lock();
                *cache = row.cache_read;
            }
            {
                let mut prompt = state.usage_metadata.session_prompt.lock();
                *prompt = row.prompt;
            }
            *state.usage_metadata.authoritative_totals.lock() = true;
            let (context_window, context_window_resolved) =
                state.usage_metadata.resolved_context_window();
            let (cache_read, prompt_tokens) = state.usage_metadata.session_cache();
            let _ = events.send(DriverEvent::UsageUpdated {
                context_tokens: None,
                latest: None,
                context_window,
                context_window_resolved,
                session_total: (row.total > 0).then_some(row.total),
                cache_read,
                prompt_tokens,
            });
        }
        "session.step.ended" => {
            // One settled model call: occupancy is this step's context. The
            // session totals grow here too — unless `session.usage.updated`
            // has already delivered the authoritative cumulative row, which
            // includes this very step.
            if let Some(usage) = opencode_normalized_usage(payload) {
                let (context_window, context_window_resolved) =
                    state.usage_metadata.resolved_context_window();
                let authoritative = *state.usage_metadata.authoritative_totals.lock();
                let total = if authoritative {
                    *state.usage_metadata.session_total.lock()
                } else {
                    let mut total = state.usage_metadata.session_total.lock();
                    *total = total.saturating_add(usage.context);
                    *total
                };
                if !authoritative && usage.prompt > 0 {
                    {
                        let mut cache = state.usage_metadata.session_cache_read.lock();
                        *cache = cache.saturating_add(usage.cache_read);
                    }
                    {
                        let mut prompt = state.usage_metadata.session_prompt.lock();
                        *prompt = prompt.saturating_add(usage.prompt);
                    }
                }
                let (cache_read, prompt_tokens) = state.usage_metadata.session_cache();
                let _ = events.send(DriverEvent::UsageUpdated {
                    context_tokens: Some(usage.context),
                    latest: Some(usage.latest),
                    context_window,
                    context_window_resolved,
                    session_total: (total > 0).then_some(total),
                    cache_read,
                    prompt_tokens,
                });
            }
            step_ended_turn_stats(payload, state);
        }
        "session.compaction.started" => {
            // Durable admission: the provider will summarize at its next
            // safe step boundary. `reason` distinguishes a user request
            // from the provider's own overflow preflight.
            state.compaction_live = true;
            let _ = events.send(DriverEvent::CompactionUpdated(CompactionState {
                status: CompactionStatus::Running,
                reason: payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                model: None,
                error: None,
                summary: None,
            }));
        }
        "session.compaction.delta" => {
            // The summary streams here, but the app shows progress rather
            // than the partial text, so re-announcing per delta would only
            // flood the wire. The first delta after a driver restart still
            // promotes the session: a compaction is durable, and one that
            // outlived this driver never replays its `started`.
            if !state.compaction_live {
                state.compaction_live = true;
                let _ = events.send(DriverEvent::CompactionUpdated(CompactionState {
                    status: CompactionStatus::Running,
                    reason: None,
                    model: None,
                    error: None,
                    summary: None,
                }));
            }
        }
        "session.compaction.ended" => {
            state.compaction_live = false;
            // The ended event often carries only status/model. The stored
            // summary lives on the compaction message, which is what the TUI
            // renders as the Compaction section — fetch it so the transcript
            // can show the same document immediately.
            let stored = fetch_latest_opencode_compaction(port, session_id);
            let _ = events.send(DriverEvent::CompactionUpdated(CompactionState {
                status: CompactionStatus::Completed,
                reason: payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| stored.as_ref().and_then(|state| state.reason.clone())),
                model: opencode_compaction_model(payload)
                    .or_else(|| stored.as_ref().and_then(|state| state.model.clone())),
                error: None,
                summary: opencode_compaction_summary(payload)
                    .or_else(|| stored.and_then(|state| state.summary)),
            }));
        }
        "session.compaction.failed" => {
            state.compaction_live = false;
            let error = payload.get("error");
            let aborted = error
                .and_then(|error| error.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("aborted"));
            if aborted {
                // An interrupt or provider abort withdraws the request; it
                // is not a failure to report.
                let _ = events.send(DriverEvent::CompactionUpdated(CompactionState {
                    status: CompactionStatus::Cancelled,
                    reason: payload
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    model: None,
                    error: None,
                    summary: None,
                }));
            } else {
                let _ = events.send(DriverEvent::CompactionUpdated(CompactionState {
                    status: CompactionStatus::Failed,
                    reason: payload
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    model: None,
                    error: error
                        .and_then(|error| {
                            error
                                .get("message")
                                .and_then(Value::as_str)
                                .or_else(|| error.as_str())
                        })
                        .map(str::to_owned),
                    summary: None,
                }));
            }
        }
        // The channel opencode actually publishes provider backoff on. Verified
        // against a real 2.0.3 server (a deliberately broken OpenAI-compatible
        // stream): the runner emits `session.step.failed` and this event for
        // every attempt, and emits **no** `session.status` at all over a whole
        // turn — including the retries. A driver that only understood
        // `session.status` therefore never saw a single retry.
        //
        // Payload: `{sessionID, assistantMessageID, attempt, at, error:{type,
        // message, status}}`, where `at` is the wall clock of the next attempt.
        // There is no upsell `action` on this path; `session.status`'s retry
        // variant carries one but is legacy.
        "session.retry.scheduled" => {
            let error = payload.get("error");
            let message = error
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .or_else(|| payload.get("message").and_then(Value::as_str))
                .unwrap_or_default()
                .to_owned();
            let _ = events.send(DriverEvent::ProviderRetry {
                attempt: payload
                    .get("attempt")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(u32::MAX as u64) as u32,
                message,
                action: None,
                next_at_ms: payload.get("at").and_then(Value::as_u64),
            });
        }
        // Legacy spelling of the same state, still emitted by older builds.
        // `status.retry` also carries the provider's optional upsell `action`,
        // which the modern event has no equivalent for, so both paths stay.
        "session.status" => {
            let status = payload.get("status");
            match status
                .and_then(|status| status.get("type"))
                .and_then(Value::as_str)
            {
                Some("busy") => {
                    let _ = events.send(DriverEvent::ProviderBusy);
                }
                Some("retry") => {
                    let _ = events.send(DriverEvent::ProviderRetry {
                        attempt: status
                            .and_then(|status| status.get("attempt"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            .min(u32::MAX as u64) as u32,
                        message: status
                            .and_then(|status| status.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        action: status
                            .and_then(|status| status.get("action"))
                            .filter(|action| action.is_object())
                            .and_then(|action| serde_json::from_value(action.clone()).ok()),
                        next_at_ms: status
                            .and_then(|status| status.get("next"))
                            .and_then(Value::as_u64),
                    });
                }
                // The runner's idle report may precede the execution event or
                // replace it entirely; either way a turn the app still holds
                // open must be settled (see `settle_on_idle_report`).
                Some("idle") => settle_on_idle_report(port, session_id, turn_active, state, events),
                _ => {}
            }
        }
        // Deprecated companion of `session.status idle` (both publish on the
        // same transition); the second settlement attempt is a no-op.
        "session.idle" => settle_on_idle_report(port, session_id, turn_active, state, events),
        // A background bash (or any server-initiated run) can start after
        // this client already settled. Child sessions reopen on this event;
        // the foreground session must too, or the continuation's deltas are
        // dropped (`accepts_turn_output` is false while Idle).
        "session.execution.started" => ensure_foreground_turn_active(turn_active, events),
        "session.execution.succeeded" => {
            clear_foreground_turn_state(state, events);
            if std::mem::take(&mut *turn_active.lock()) {
                let _ = events.send(DriverEvent::TurnFinished {
                    success: true,
                    summary: None,
                });
            }
        }
        "session.execution.failed" => {
            clear_foreground_turn_state(state, events);
            // The failure payload carries the provider error; surface it so
            // the transcript explains why the turn settled unsuccessfully.
            let message = payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(message) = message {
                let _ = events.send(DriverEvent::Error(message));
            }
            if std::mem::take(&mut *turn_active.lock()) {
                let _ = events.send(DriverEvent::TurnFinished {
                    success: false,
                    summary: Some(tr!("errors.provider_start_turn", provider = "OpenCode")),
                });
            }
        }
        "session.error" => {
            let message = payload
                .pointer("/error/message")
                .or_else(|| payload.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("OpenCode reported an error");
            let _ = events.send(DriverEvent::Error(message.to_owned()));
        }
        "session.renamed" => {
            let title = payload
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|title| !title.is_empty() && !title.starts_with("New session - "));
            if let Some(title) = title {
                let _ = events.send(DriverEvent::AutoTitleUpdated(Some(title.to_owned())));
            }
        }
        "session.tool.input.started" => {
            // The tool's name arrives on this event; `session.tool.called`
            // (which carries the arguments) does not repeat it.
            if let (Some(id), Some(name)) = (
                payload.get("id").and_then(Value::as_str),
                payload.get("name").and_then(Value::as_str),
            ) {
                let kind = super::support::classify_tool(name);
                state.tools.insert(id.to_owned(), (kind, name.to_owned()));
                let _ = events.send(DriverEvent::RichActivity(pending_tool_activity(id, name)));
            }
        }
        "session.tool.called" => {
            tool_called(payload, events, state);
        }
        "session.tool.progress" => tool_progress(payload, events, state),
        "session.tool.input.ended" => {}
        "session.created" | "session.updated" | "session.deleted" => {
            // Lifecycle news for the sidebar's reconciliation; debounced
            // app-side, so one send per event is fine.
            let _ = events.send(DriverEvent::NativeSessionsChanged);
        }
        "session.tool.success" => {
            tool_finished(payload, events, state, false);
        }
        "session.tool.error" | "session.tool.failed" => {
            tool_finished(payload, events, state, true);
        }
        _ if kind.starts_with("permission.") => {
            request_permission(payload, events, commands, auto_approve, &state.permissions);
        }
        "question.asked" | "question.v2.asked" => request_user_input(payload, events),
        "question.replied"
        | "question.rejected"
        | "question.v2.replied"
        | "question.v2.rejected" => {}
        // The `question` tool's prompt on current releases: a form whose
        // `metadata.kind` is `"question"`. Replied/cancelled only retire the
        // recorded field shapes; the app dismisses its own prompt when it
        // sends the reply.
        "form.created" => {
            let _ = request_user_input_from_form(payload, &state.forms, events);
        }
        "form.replied" | "form.cancelled" => {
            let form = payload.get("form").unwrap_or(payload);
            if let Some(id) = form.get("id").and_then(Value::as_str) {
                let mut forms = state.forms.lock();
                forms.fields.remove(id);
                forms.sessions.remove(id);
                forms.announced.remove(id);
            }
        }
        // `session.step.streamed`, `session.inbox.*`,
        // `session.instructions.updated`, `server.connected`, and the heartbeat
        // comment lines are not transcript content. The text and reasoning
        // started/ended/delta trios are handled above.
        _ => {}
    }
}

/// Marks the foreground turn live when the server starts an execution this
/// client did not prompt. A second start while the turn is already held is a
/// no-op, so a user-submitted prompt that already set the flag is unchanged.
fn ensure_foreground_turn_active(turn_active: &Mutex<bool>, events: &impl DriverEventSink) {
    {
        let mut active = turn_active.lock();
        if *active {
            return;
        }
        *active = true;
    }
    let _ = events.send(DriverEvent::TurnStarted);
}

/// Settles a still-active turn when the server's runner reports idle.
///
/// Turn settlement otherwise hangs on `session.execution.succeeded`/`failed`,
/// so a lost or reordered execution event would leave the task in Working
/// forever. The runner's idle report is the durable backstop: it always
/// publishes at the end of a run — including failures, where it precedes
/// `session.execution.failed` — so the outcome is resolved from the newest
/// assistant message (its `error` field records a failed run) instead of
/// assuming success. An unfetchable outcome stays untouched: the execution
/// event or the stream's own exit still has a chance to settle the turn.
fn settle_on_idle_report(
    port: u16,
    session_id: &str,
    turn_active: &Mutex<bool>,
    state: &mut OpenCodeStreamState,
    events: &impl DriverEventSink,
) {
    if !*turn_active.lock() {
        return;
    }
    let messages = crate::opencode_session::request_json_on_port(
        port,
        "GET",
        &format!(
            "/api/session/{}/message?limit=5",
            encode_path_segment(session_id)
        ),
        None,
        Duration::from_secs(3),
    )
    .ok();
    let Some(messages) = messages else {
        return;
    };
    let failure = newest_assistant_error(&messages);
    if std::mem::take(&mut *turn_active.lock()) {
        // The idle backstop replaces a lost execution event, so it must drop
        // the same per-turn bookkeeping: otherwise a stale pending spawn or
        // tool name leaks into the next turn and mis-binds a new child.
        clear_foreground_turn_state(state, events);
        let _ = events.send(DriverEvent::TurnFinished {
            success: failure.is_none(),
            summary: failure,
        });
    }
}

/// The provider error recorded on the newest assistant message, if any. The
/// message list is newest-first, and user or synthetic entries never carry a
/// live turn's error.
fn newest_assistant_error(messages: &Value) -> Option<String> {
    let data = messages.pointer("/data").and_then(Value::as_array)?;
    data.iter()
        .find(|message| message.get("type").and_then(Value::as_str) == Some("assistant"))
        .and_then(|message| message.get("error"))
        .filter(|error| !error.is_null())
        .and_then(|error| {
            error
                .get("message")
                .or_else(|| error.pointer("/data/message"))
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .map(str::to_owned)
        })
}

fn pending_tool_activity(id: &str, name: &str) -> ActivityItem {
    ActivityItem::new(
        Some(id.to_owned()),
        super::support::classify_tool(name),
        name.to_owned(),
        None,
        false,
    )
}

/// Enrich the activity opened by `session.tool.input.started` once its
/// arguments are complete. Large inputs can take minutes to generate.
fn tool_called(payload: &Value, events: &impl DriverEventSink, state: &mut OpenCodeStreamState) {
    let Some(id) = payload.get("id").and_then(Value::as_str) else {
        return;
    };
    let stored = state.tools.get(id).cloned();
    let kind = stored
        .as_ref()
        .map(|(kind, _)| *kind)
        .unwrap_or(ActivityKind::Tool);
    let title = stored
        .as_ref()
        .map(|(_, title)| title.clone())
        .unwrap_or_else(|| tr!("activity.tool"));
    let arguments = payload.get("input");
    if stored
        .as_ref()
        .is_some_and(|(_, name)| is_subagent_tool(name))
    {
        let prompt = subagent_prompt(arguments);
        let target = subagent_session_id(arguments);
        let resumed = target.as_deref().is_some_and(|child_id| {
            bind_subagent_resume(state, child_id, id, prompt.clone(), events)
        });
        if !resumed {
            state.pending_subagents.push_back(PendingSubagent {
                activity_id: id.to_owned(),
                prompt,
                target,
            });
        }
    }
    let item = activity::tool_activity(
        Some(id.to_owned()),
        kind,
        title,
        arguments,
        None,
        payload.get("input"),
        false,
        false,
    );
    let _ = events.send(DriverEvent::RichActivity(item));
}

fn tool_progress(payload: &Value, events: &impl DriverEventSink, state: &mut OpenCodeStreamState) {
    let Some(id) = payload.get("id").and_then(Value::as_str) else {
        return;
    };
    let Some((kind, title)) = state.tools.get(id).cloned() else {
        return;
    };
    let item = activity::tool_activity(
        Some(id.to_owned()),
        kind,
        title,
        payload.get("input"),
        None,
        Some(payload),
        false,
        false,
    );
    let _ = events.send(DriverEvent::RichActivity(item));
}

/// Bind a subagent tool call that names a child session to that child. Returns
/// false when the child is not known yet, leaving the caller to queue the call
/// for the next `session.created`.
fn bind_subagent_resume(
    state: &mut OpenCodeStreamState,
    child_id: &str,
    activity_id: &str,
    prompt: Option<String>,
    events: &impl DriverEventSink,
) -> bool {
    let Some(child) = state.children.get_mut(child_id) else {
        return false;
    };
    // Only a call the child has not already recorded is a new binding. A
    // replayed `tool.called`/`input.started` pair rewrites nothing and must not
    // re-arm a resume whose execution start was already consumed.
    if push_origin_activity(&mut child.item.origin_activity_ids, activity_id.to_owned()) {
        // The next execution start is an intentional resume and may reopen a
        // settled child, and its turn carries this prompt.
        child.pending_revives.push_back(PendingRevive {
            activity_id: activity_id.to_owned(),
            prompt,
        });
        child_update(child, events, None, None);
    }
    true
}

/// Clear every child's pending resumes. Called when the foreground turn
/// settles: a resume that never produced its own execution start (the tool
/// failed, or the turn was interrupted) must not stay armed.
fn disarm_child_resumes(state: &mut OpenCodeStreamState) {
    for child in state.children.values_mut() {
        child.pending_revives.clear();
    }
}

/// Drop the per-turn bookkeeping the foreground stream accumulated. Every path
/// that ends a foreground turn shares this — including the idle backstop, which
/// stands in when the execution event is lost — so a stale pending spawn or
/// tool name can never leak into the next turn and mis-bind a new child. The
/// statistics accumulated for the turn leave as one event on the same paths.
fn clear_foreground_turn_state(state: &mut OpenCodeStreamState, events: &impl DriverEventSink) {
    flush_turn_stats(state, events);
    state.permissions.lock().pending.clear();
    state.tools.clear();
    state.pending_subagents.clear();
    disarm_child_resumes(state);
}

/// Fold one settled step into the turn's footer statistics. The step events
/// carry no `time` object — the stored message rows have one, but they are
/// only visible on the message endpoint — so the step's streaming duration
/// prefers a payload that does grow `time.streamed`/`time.created` (a newer
/// server or a fork) and otherwise falls back to the wall clock its
/// `session.step.started` armed; a step whose start was never seen
/// contributes tokens but no time.
///
/// The numerator is the TUI footer's own: `tokens.output + tokens.reasoning`,
/// the tokens the provider actually produced, reasoning included. It only
/// accumulates, never flushes: a turn can carry several terminal `finish`
/// values (a steer or provider retry reopens the model loop, and `length`
/// continues past it), so an early flush would deliver a segment's totals and
/// lose whatever the earlier segments had already collected. The flush
/// happens exactly once, on the turn's terminal paths through
/// [`clear_foreground_turn_state`].
fn step_ended_turn_stats(payload: &Value, state: &mut OpenCodeStreamState) {
    let Some(accum) = state.turn_stats.as_mut() else {
        return;
    };
    let tokens = payload.get("tokens");
    let output = tokens
        .and_then(|tokens| tokens.get("output"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning = tokens
        .and_then(|tokens| tokens.get("reasoning"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    accum.stats.output_tokens = accum
        .stats
        .output_tokens
        .saturating_add(output)
        .saturating_add(reasoning);
    let duration = match (
        payload.pointer("/time/streamed").and_then(Value::as_u64),
        payload.pointer("/time/created").and_then(Value::as_u64),
    ) {
        (Some(streamed), Some(created)) => streamed.saturating_sub(created),
        _ => accum
            .step_started_at
            .map_or(0, |started| unix_time_millis().saturating_sub(started)),
    };
    accum.stats.stream_ms = accum.stats.stream_ms.saturating_add(duration);
    accum.step_started_at = None;
}

/// Send the accumulated turn statistics, if any, and retire the accumulator.
fn flush_turn_stats(state: &mut OpenCodeStreamState, events: &impl DriverEventSink) {
    if let Some(accum) = state.turn_stats.take() {
        let _ = events.send(DriverEvent::TurnStatsUpdated(accum.stats));
    }
}

/// `session.tool.success`/`session.tool.error` close a tool activity with its
/// output (or failure) and release the recorded name.
fn tool_finished(
    payload: &Value,
    events: &impl DriverEventSink,
    state: &mut OpenCodeStreamState,
    failed: bool,
) {
    let Some(id) = payload.get("id").and_then(Value::as_str) else {
        return;
    };
    let stored = state.tools.remove(id);
    if failed
        && stored
            .as_ref()
            .is_some_and(|(_, name)| is_subagent_tool(name))
    {
        state
            .pending_subagents
            .retain(|pending| pending.activity_id != id);
        // A resume that failed will never emit its own execution start, so it
        // must not stay armed against a later replay. Drop only this call's
        // bind so a parallel resume in the same turn stays queued.
        for child in state.children.values_mut() {
            child
                .pending_revives
                .retain(|revive| revive.activity_id != id);
        }
    }
    let kind = stored
        .as_ref()
        .map(|(kind, _)| *kind)
        .unwrap_or(ActivityKind::Tool);
    let title = stored
        .map(|(_, title)| title)
        .unwrap_or_else(|| tr!("activity.tool"));
    let output = failed
        .then(|| payload.pointer("/error").unwrap_or(&Value::Null).clone())
        .filter(|value| !value.is_null())
        .or_else(|| {
            payload
                .pointer("/content")
                .filter(|value| !value.is_null())
                .cloned()
        });
    let item = activity::tool_activity(
        Some(id.to_owned()),
        kind,
        title,
        payload.get("input"),
        output.as_ref(),
        Some(payload),
        failed,
        true,
    );
    let _ = events.send(DriverEvent::RichActivity(item));
}

fn request_user_input(properties: &Value, events: &impl DriverEventSink) {
    let Some(request_id) = properties.get("id").and_then(Value::as_str) else {
        return;
    };
    let questions = properties
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, question)| {
            let text = question.get("question").and_then(Value::as_str)?.trim();
            if text.is_empty() {
                return None;
            }
            let header = question
                .get("header")
                .and_then(Value::as_str)
                .filter(|header| !header.trim().is_empty())
                .unwrap_or("Question");
            let slug = header
                .trim()
                .to_ascii_lowercase()
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                        character
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            let slug = slug.trim_matches('-');
            let id = if slug.is_empty() {
                format!("question-{index}")
            } else {
                format!("question-{index}-{slug}")
            };
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| {
                    let label = option.get("label").and_then(Value::as_str)?.trim();
                    (!label.is_empty()).then(|| UserInputOption {
                        label: label.to_owned(),
                        description: option
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|description| !description.is_empty())
                            .map(str::to_owned),
                    })
                })
                .collect();
            Some(UserInputQuestion {
                id,
                header: header.to_owned(),
                question: text.to_owned(),
                options,
                multi_select: question
                    .get("multiple")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect::<Vec<_>>();
    if !questions.is_empty() {
        let _ = events.send(DriverEvent::UserInputRequested {
            request_id: request_id.to_owned(),
            questions,
        });
    }
}

/// The `question` tool on current opencode publishes its prompt as a form
/// (`form.created` with `metadata.kind == "question"`), not as the question
/// events the dedicated route still documents. A field maps back to a
/// question: `title` is the header, `description` the question text, each
/// option keeps `label`/`description`, and `type == "multiselect"` marks
/// multi-select. Other forms (if any ever appear) are ignored.
///
/// Records the field shapes under the form id so the reply can build the
/// answer object the route demands, and stays silent on a form the poll
/// already announced.
fn request_user_input_from_form(
    payload: &Value,
    forms: &Mutex<OpenCodeFormState>,
    events: &impl DriverEventSink,
) -> Option<()> {
    let form = payload.get("form").unwrap_or(payload);
    let form_id = form.get("id").and_then(Value::as_str)?;
    if value_session_id(form).is_none() {
        return None;
    }
    let is_question = form
        .pointer("/metadata/kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "question");
    let fields = form.get("fields").and_then(Value::as_array)?;

    let mut field_shapes = Vec::new();
    let mut questions = Vec::new();
    for field in fields {
        let Some(key) = field
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
        else {
            continue;
        };
        let multi_select = field.get("type").and_then(Value::as_str) == Some("multiselect");
        // The question text rides `description`; the form synthesis in the
        // server keeps it non-empty, but a missing one falls back to the
        // header so the card still asks something.
        let question = field
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .or_else(|| field.get("title").and_then(Value::as_str).map(str::trim))
            .filter(|text| !text.is_empty())?;
        let header = field
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|header| !header.is_empty())
            .unwrap_or("Question");
        let options = field
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|option| {
                let label = option.get("label").and_then(Value::as_str)?.trim();
                (!label.is_empty()).then(|| UserInputOption {
                    label: label.to_owned(),
                    description: option
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|description| !description.is_empty())
                        .map(str::to_owned),
                })
            })
            .collect::<Vec<_>>();
        field_shapes.push((key.to_owned(), multi_select));
        questions.push(UserInputQuestion {
            id: key.to_owned(),
            header: header.to_owned(),
            question: question.to_owned(),
            options,
            multi_select,
        });
    }
    if questions.is_empty() || !is_question {
        return None;
    }

    let mut state = forms.lock();
    if !state.announced.insert(form_id.to_owned()) {
        return None;
    }
    state.fields.insert(form_id.to_owned(), field_shapes);
    if let Some(session_id) = value_session_id(form) {
        state
            .sessions
            .insert(form_id.to_owned(), session_id.to_owned());
    }
    let _ = events.send(DriverEvent::UserInputRequested {
        request_id: form_id.to_owned(),
        questions,
    });
    Some(())
}

fn request_permission(
    properties: &Value,
    events: &impl DriverEventSink,
    commands: &Sender<CommandMessage>,
    auto_approve: AutoApprove,
    permissions: &Mutex<OpenCodePermissionState>,
) {
    // The request is either the properties themselves or nested under a key,
    // and it is identified by its `per`-prefixed ID.
    let request = ["permission", "request", "info"]
        .iter()
        .find_map(|key| properties.get(*key))
        .filter(|value| value.get("id").is_some())
        .unwrap_or(properties);
    let Some(request_id) = request.get("id").and_then(Value::as_str) else {
        return;
    };
    {
        let mut permissions = permissions.lock();
        if !permissions.announced.insert(request_id.to_owned()) {
            return;
        }
    }
    let session_id = value_session_id(request)
        .or_else(|| value_session_id(properties))
        .map(str::to_owned);
    let permission_request = OpenCodePermissionRequest {
        permission: json_str_field(request, &["permission", "action"]).to_owned(),
        patterns: json_str_list(request, &["patterns", "resources"]),
        always: json_str_list(request, &["always", "save"]),
        session_id: session_id.clone(),
    };

    // OpenCode's `always` response updates a process-wide approval cache. A
    // pooled Full Access task must never suppress prompts in a Supervised task,
    // so Fintwind sends only one-shot provider replies and retains durable choices
    // in this driver's session-local state.
    if auto_approve.allows(&permission_request.permission)
        || permissions.lock().is_approved(&permission_request)
    {
        let _ = commands.send(CommandMessage::Respond {
            request_id: request_id.to_owned(),
            option_id: "once".into(),
            session_id,
        });
        return;
    }

    permissions
        .lock()
        .pending
        .insert(request_id.to_owned(), permission_request.clone());

    let permission = if permission_request.permission.is_empty() {
        tr!("permission.run_a_tool_lower")
    } else {
        permission_request.permission.clone()
    };
    let patterns = (!permission_request.patterns.is_empty())
        .then(|| permission_request.patterns.join(", "))
        .filter(|patterns| !patterns.is_empty());
    let _ = events.send(DriverEvent::Permission {
        request_id: request_id.to_owned(),
        title: patterns.clone().unwrap_or_else(|| {
            tr!(
                "permission.allow_named_permission",
                permission = permission.as_str()
            )
        }),
        detail: match patterns {
            Some(_) => tr!(
                "permission.agent_asks_for_named_permission",
                permission = permission.as_str()
            ),
            None => tr!("permission.agent_asks_for_permission"),
        },
        options: vec![
            PermissionOption {
                id: "once".into(),
                label: tr!("permission.allow_once"),
                allow: true,
            },
            PermissionOption {
                id: "always".into(),
                label: tr!("permission.always_allow"),
                allow: true,
            },
            PermissionOption {
                id: "reject".into(),
                label: tr!("common.deny"),
                allow: false,
            },
        ],
    });
}

fn permission_responses(
    permissions: &Mutex<OpenCodePermissionState>,
    request_id: &str,
    option_id: &str,
) -> Vec<(String, String, Option<String>)> {
    let mut permissions = permissions.lock();
    let Some(request) = permissions.pending.remove(request_id) else {
        return Vec::new();
    };
    let session_id = request.session_id.clone();
    if option_id != "always" {
        return vec![(request_id.to_owned(), option_id.to_owned(), session_id)];
    }

    permissions.remember(&request);
    // OpenCode normally applies an `always` reply to other matching requests
    // already pending in the same session. Preserve that behavior locally,
    // but send every provider reply as one-shot so the shared server's cache
    // remains untouched.
    let additional = permissions
        .pending
        .iter()
        .filter(|(_, request)| permissions.is_approved(request))
        .map(|(request_id, request)| (request_id.clone(), request.session_id.clone()))
        .collect::<Vec<_>>();
    for (request_id, _) in &additional {
        permissions.pending.remove(request_id);
    }

    std::iter::once((request_id.to_owned(), "once".into(), session_id))
        .chain(
            additional
                .into_iter()
                .map(|(request_id, session_id)| (request_id, "once".into(), session_id)),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three resume rules a shared server needs: a cursor pointing at
    /// another workspace is refused instead of silently moving the session,
    /// a session recorded before locations existed stays resumable, and a
    /// trailing-separator difference is not a different workspace.
    #[test]
    fn resume_location_rules() {
        let cwd = Path::new("E:\\work\\fintwind");
        let recorded = json!({"data": {"location": {"directory": "E:\\work\\fintwind"}}});
        assert!(verify_resume_location(&recorded, cwd, "ses_1").is_ok());
        // A trailing separator is the same directory.
        let trailing = json!({"location": {"directory": "E:\\work\\fintwind\\"}});
        assert!(verify_resume_location(&trailing, cwd, "ses_1").is_ok());
        // No recorded location: an old session stays resumable.
        assert!(verify_resume_location(&json!({"data": {}}), cwd, "ses_1").is_ok());
        assert!(verify_resume_location(&json!({}), cwd, "ses_1").is_ok());
        // Another workspace's session must be refused outright.
        let other = json!({"data": {"location": {"directory": "E:\\work\\other"}}});
        let error = verify_resume_location(&other, cwd, "ses_1").unwrap_err();
        assert!(error.to_string().contains("E:\\work\\other"));
    }

    #[test]
    fn prompt_bodies_carry_delivery_metadata_and_legacy_fallback() {
        // The current body names its delivery and records the owning task.
        let (current, legacy) =
            prompt_bodies("hi", &[], Some("11111111-1111-1111-1111-111111111111")).unwrap();
        assert_eq!(
            current,
            json!({
                "text": "hi",
                "delivery": "steer",
                "metadata": {"source": "fintwind", "task": "11111111-1111-1111-1111-111111111111"},
            })
        );
        // The legacy body a pre-2.0 CLI gets instead: text only.
        assert_eq!(legacy, json!({"text": "hi"}));

        // Without a task id there is no metadata to record, and the legacy
        // body stays identical to the current one apart from delivery.
        let (current, legacy) = prompt_bodies("hi", &[], None).unwrap();
        assert_eq!(current, json!({"text": "hi", "delivery": "steer"}));
        assert_eq!(legacy, json!({"text": "hi"}));
        // A blank task id is treated as absent.
        let (current, _) = prompt_bodies("hi", &[], Some("  ")).unwrap();
        assert!(current.get("metadata").is_none());

        let path = std::env::temp_dir().join("notes.md");
        let (current, legacy) = prompt_bodies(
            "",
            &[PromptFile {
                path: path.clone(),
                name: "notes.md".into(),
            }],
            None,
        )
        .unwrap();
        for body in [&current, &legacy] {
            assert_eq!(body["text"], "");
            assert_eq!(body["files"][0]["name"], "notes.md");
            let uri = body["files"][0]["uri"].as_str().unwrap();
            assert!(uri.starts_with("file:"));
            assert!(!uri.contains('@'));
            assert_eq!(url::Url::parse(uri).unwrap().to_file_path().unwrap(), path);
        }

        let relative = prompt_bodies(
            "x",
            &[PromptFile {
                path: PathBuf::from("relative.md"),
                name: "relative.md".into(),
            }],
            None,
        );
        assert!(relative.is_err());
    }

    #[test]
    fn model_reference_carries_variant_and_preserves_nested_model_ids() {
        assert_eq!(
            opencode_model_ref("gateway/vendor/model", Some("high")),
            Some(json!({"id": "vendor/model", "providerID": "gateway", "variant": "high"}))
        );
        assert_eq!(
            opencode_model_ref("gateway/model", None),
            Some(json!({"id": "model", "providerID": "gateway"}))
        );
    }

    fn harness() -> (
        Sender<DriverEvent>,
        crossbeam_channel::Receiver<DriverEvent>,
        Sender<CommandMessage>,
        crossbeam_channel::Receiver<CommandMessage>,
        Mutex<bool>,
        OpenCodeStreamState,
    ) {
        let (events, event_rx) = unbounded();
        let (commands, command_rx) = unbounded();
        (
            events,
            event_rx,
            commands,
            command_rx,
            Mutex::new(true),
            OpenCodeStreamState::default(),
        )
    }

    /// One-shot loopback server answering a single GET with a canned JSON
    /// body, so the idle settlement's message fetch runs without an installed
    /// opencode.
    fn serve_one_message_response(body: String) -> u16 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 4096];
            let _ = std::io::Read::read(&mut stream, &mut buffer);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
        });
        port
    }

    #[test]
    fn session_status_busy_and_retry_surface_as_provider_signals() {
        let (events, event_rx, _commands, _command_rx, turn, mut state) = harness();
        handle_event(
            &json!({
                "type": "session.status",
                "data": {"sessionID": "ses_1", "status": {"type": "busy"}}
            }),
            &events,
            &_commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        assert!(matches!(event_rx.recv(), Ok(DriverEvent::ProviderBusy)));

        handle_event(
            &json!({
                "type": "session.status",
                "data": {
                    "sessionID": "ses_1",
                    "status": {
                        "type": "retry",
                        "attempt": 2,
                        "message": "429 Too Many Requests",
                        "next": 1_700_000_008_000_u64,
                        "action": {
                            "reason": "free_tier_limit",
                            "provider": "zen",
                            "title": "Free limit reached",
                            "message": "Subscribe to OpenCode Go",
                            "label": "subscribe",
                            "link": "https://opencode.ai/go"
                        }
                    }
                }
            }),
            &events,
            &_commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        match event_rx.recv() {
            Ok(DriverEvent::ProviderRetry {
                attempt,
                message,
                action,
                next_at_ms,
            }) => {
                assert_eq!(attempt, 2);
                assert_eq!(message, "429 Too Many Requests");
                let action = action.expect("the upsell action rides the retry");
                assert_eq!(action.reason, "free_tier_limit");
                assert_eq!(action.link.as_deref(), Some("https://opencode.ai/go"));
                assert_eq!(next_at_ms, Some(1_700_000_008_000));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    /// The modern retry event. `session.status` is never emitted by current
    /// builds — a whole turn, retries included, surfaced zero of them against a
    /// real 2.0.3 server — so this is the one that has to reach the app. Its
    /// field names differ from the legacy variant: the reason rides under
    /// `error.message` and the countdown under `at`.
    #[test]
    fn scheduled_retry_surfaces_as_a_provider_retry_with_its_own_field_names() {
        let (events, event_rx, _commands, _command_rx, turn, mut state) = harness();
        handle_event(
            &json!({
                "type": "session.retry.scheduled",
                "data": {
                    "sessionID": "ses_1",
                    "assistantMessageID": "msg_1",
                    "attempt": 4,
                    "at": 1_700_000_008_000_u64,
                    "error": {
                        "type": "provider.transport",
                        "message": "ECONNRESET: The socket connection was closed unexpectedly.",
                        "status": 200
                    }
                }
            }),
            &events,
            &_commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        match event_rx.recv() {
            Ok(DriverEvent::ProviderRetry {
                attempt,
                message,
                action,
                next_at_ms,
            }) => {
                assert_eq!(attempt, 4);
                assert!(message.contains("ECONNRESET"), "{message}");
                // This path carries no upsell action; the legacy one did.
                assert_eq!(action, None);
                assert_eq!(next_at_ms, Some(1_700_000_008_000));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn idle_report_settles_a_lost_turn_by_the_newest_assistant_error() {
        let (events, event_rx, _commands, _command_rx, turn, mut state) = harness();
        let port = serve_one_message_response(
            json!({"data": [
                {"type": "assistant", "id": "msg_2", "error": {"type": "APIError", "message": "rate limited"}},
                {"type": "assistant", "id": "msg_1"}
            ]})
            .to_string(),
        );
        handle_event(
            &json!({
                "type": "session.status",
                "data": {"sessionID": "ses_1", "status": {"type": "idle"}}
            }),
            &events,
            &_commands,
            &turn,
            port,
            "ses_1",
            false,
            &mut state,
        );
        match event_rx.recv() {
            Ok(DriverEvent::TurnFinished { success, summary }) => {
                assert!(!success);
                assert_eq!(summary.as_deref(), Some("rate limited"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        // The flag was consumed: a late execution event cannot double-settle.
        assert!(!*turn.lock());
    }

    #[test]
    fn idle_report_settles_successfully_and_ignores_inactive_turns() {
        // An idle report for a turn nobody holds open must stay silent.
        {
            let (events, event_rx, _commands, _command_rx, turn, mut state) = harness();
            *turn.lock() = false;
            handle_event(
                &json!({
                    "type": "session.status",
                    "data": {"sessionID": "ses_1", "status": {"type": "idle"}}
                }),
                &events,
                &_commands,
                &turn,
                0,
                "ses_1",
                false,
                &mut state,
            );
            assert!(event_rx.try_recv().is_err());
        }
        // A clean run settles successfully even when the execution event is
        // the one that went missing.
        {
            let (events, event_rx, _commands, _command_rx, turn, mut state) = harness();
            let port = serve_one_message_response(
                json!({"data": [{"type": "user"}, {"type": "assistant", "id": "msg_1"}]})
                    .to_string(),
            );
            handle_event(
                &json!({
                    "type": "session.idle",
                    "data": {"sessionID": "ses_1"}
                }),
                &events,
                &_commands,
                &turn,
                port,
                "ses_1",
                false,
                &mut state,
            );
            assert!(matches!(
                event_rx.recv(),
                Ok(DriverEvent::TurnFinished {
                    success: true,
                    summary: None
                })
            ));
        }
    }

    #[test]
    fn execution_started_reopens_an_inactive_foreground_turn() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        *turn.lock() = false;
        handle_event(
            &json!({
                "type": "session.execution.started",
                "data": {"sessionID": "ses_1"}
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        assert!(matches!(event_rx.recv(), Ok(DriverEvent::TurnStarted)));
        assert!(*turn.lock(), "the server-initiated run must arm the turn");

        // A start while the turn is already held is the user-prompt path;
        // it must not emit a second TurnStarted.
        handle_event(
            &json!({
                "type": "session.execution.started",
                "data": {"sessionID": "ses_1"}
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        assert!(event_rx.try_recv().is_err());
        assert!(*turn.lock());

        handle_event(
            &json!({
                "type": "session.text.delta",
                "data": {"sessionID": "ses_1", "delta": "compile finished"}
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        handle_event(
            &json!({
                "type": "session.execution.succeeded",
                "data": {"sessionID": "ses_1"}
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(seen.iter().any(
            |event| matches!(event, DriverEvent::TextDelta { delta, .. } if delta == "compile finished")
        ));
        assert!(
            seen.iter()
                .any(|event| matches!(event, DriverEvent::TurnFinished { success: true, .. }))
        );
        assert!(!*turn.lock(), "the continuation must settle exactly once");
    }

    #[test]
    fn an_unfetchable_outcome_leaves_the_turn_open() {
        let (events, event_rx, _commands, _command_rx, turn, mut state) = harness();
        // Port 0 refuses the connection: no outcome, no settlement.
        handle_event(
            &json!({
                "type": "session.status",
                "data": {"sessionID": "ses_1", "status": {"type": "idle"}}
            }),
            &events,
            &_commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        assert!(event_rx.try_recv().is_err());
        assert!(*turn.lock());
    }

    #[test]
    fn a_successful_subagent_interrupt_settles_as_stopped() {
        let (events, event_rx, _commands, _command_rx, _turn, _state) = harness();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_child");
        emit_stopped_subagent(&events, key.clone());
        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(seen.iter().any(|event| matches!(
            event,
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))
                if item.key == key && item.status == BackgroundWorkStatus::Stopped
        )));
        assert!(seen.iter().any(|event| matches!(
            event,
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(
                BackgroundWorkTranscriptEvent::Finished { success: false, .. }
            ))
        )));
    }

    #[test]
    fn child_session_events_stream_as_background_work_without_finishing_parent() {
        let (events, event_rx, _commands, _command_rx, turn, mut state) = harness();
        let parent = "ses_parent";
        let created = json!({
            "type": "session.created",
            "data": {"session": {"id": "ses_child", "parentID": parent, "title": "Research", "agent": "explore", "prompt": "Inspect the repository"}}
        });
        handle_child_event(&created, parent, &events, &_commands, false, &mut state);
        handle_child_event(
            &json!({"type":"session.execution.started","data":{"sessionID":"ses_child"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type":"session.reasoning.delta","data":{"sessionID":"ses_child","delta":"thinking"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type":"session.text.delta","data":{"sessionID":"ses_child","delta":"answer"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type":"session.tool.input.started","data":{"sessionID":"ses_child","id":"call_1","name":"read"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type":"session.tool.called","data":{"sessionID":"ses_child","id":"call_1","input":{"filePath":"README.md"}}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type":"session.tool.success","data":{"sessionID":"ses_child","id":"call_1","content":[{"type":"text","text":"ok"}]}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type":"session.execution.succeeded","data":{"sessionID":"ses_child"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );

        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(
            matches!(seen.first(), Some(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))) if item.key.provider_id == "ses_child" && item.status == BackgroundWorkStatus::Starting)
        );
        assert!(seen.iter().any(|event| matches!(event, DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item)) if item.status == BackgroundWorkStatus::Running)));
        assert!(seen.iter().any(|event| matches!(event, DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::ReasoningDelta { delta, .. })) if delta == "thinking")));
        assert!(seen.iter().any(|event| matches!(event, DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::TextDelta { delta, .. })) if delta == "answer")));
        assert!(seen.iter().any(|event| matches!(event, DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Activity { activity, .. })) if activity.complete)));
        assert!(seen.iter().any(|event| matches!(event, DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item)) if item.status == BackgroundWorkStatus::Completed)));
        assert!(seen.iter().any(|event| matches!(
            event,
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(
                BackgroundWorkTranscriptEvent::Finished { success: true, .. }
            ))
        )));
        assert!(
            *turn.lock(),
            "child execution must not settle the foreground turn"
        );
    }

    #[test]
    fn parent_subagent_tool_seeds_and_links_the_child_transcript() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        handle_event(
            &json!({
                "type": "session.tool.input.started",
                "data": {"id": "call_task", "name": "task"}
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        handle_event(
            &json!({
                "type": "session.tool.called",
                "data": {"id": "call_task", "input": {"prompt": "Inspect the repository"}}
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        handle_child_event(
            &json!({
                "type": "session.created",
                "data": {"session": {"id": "ses_child", "parentID": "ses_parent"}}
            }),
            "ses_parent",
            &events,
            &commands,
            false,
            &mut state,
        );

        let emitted = event_rx.try_iter().collect::<Vec<_>>();
        assert!(emitted.iter().any(|event| matches!(
            event,
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))
                if item.origin_activity_ids.iter().any(|id| id == "call_task")
        )));
        assert!(emitted.iter().any(|event| matches!(
            event,
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(
                BackgroundWorkTranscriptEvent::Started { prompt, .. }
            )) if prompt.as_deref() == Some("Inspect the repository")
        )));
    }

    #[test]
    fn resuming_a_settled_child_binds_the_new_call_and_revives_it() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let parent = "ses_parent";
        handle_child_event(
            &json!({"type": "session.created", "data": {"session": {"id": "ses_child", "parentID": parent}}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        while event_rx.try_recv().is_ok() {}

        // A task that names the child continues it: the new call is bound
        // directly, with no `session.created` to pop and no stale queue entry.
        handle_event(
            &json!({"type": "session.tool.input.started", "data": {"id": "call_resume", "name": "task"}}),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        handle_event(
            &json!({"type": "session.tool.called", "data": {"id": "call_resume", "input": {"prompt": "again", "sessionID": "ses_child"}}}),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        assert!(state.pending_subagents.is_empty());
        let bound = event_rx.try_iter().collect::<Vec<_>>();
        assert!(bound.iter().any(|event| matches!(
            event,
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))
                if item.origin_activity_ids.iter().any(|id| id == "call_resume")
        )));

        // The resumed execution reopens the settled child instead of being
        // discarded as a straggler.
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        let revived = event_rx.try_iter().collect::<Vec<_>>();
        assert!(revived.iter().any(|event| matches!(
            event,
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))
                if item.status == BackgroundWorkStatus::Running
        )));
        assert!(revived.iter().any(|event| matches!(
            event,
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(
                BackgroundWorkTranscriptEvent::Started { prompt, .. }
            )) if prompt.as_deref() == Some("again")
        )));
    }

    #[test]
    fn parallel_resumes_in_one_step_each_carry_their_own_prompt() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let parent = "ses_parent";
        handle_child_event(
            &json!({"type": "session.created", "data": {"session": {"id": "ses_child", "parentID": parent}}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        while event_rx.try_recv().is_ok() {}

        // Two `task` calls name the same child before either execution starts.
        // The binds queue instead of overwriting one another.
        for (id, prompt) in [("call_r1", "first resume"), ("call_r2", "second resume")] {
            handle_event(
                &json!({"type": "session.tool.input.started", "data": {"id": id, "name": "task"}}),
                &events,
                &commands,
                &turn,
                0,
                "ses_1",
                false,
                &mut state,
            );
            handle_event(
                &json!({"type": "session.tool.called", "data": {"id": id, "input": {"prompt": prompt, "sessionID": "ses_child"}}}),
                &events,
                &commands,
                &turn,
                0,
                "ses_1",
                false,
                &mut state,
            );
        }

        // Each resume runs to completion before the next begins, so the child
        // settles in between.
        let mut prompts = Vec::new();
        for _ in 0..2 {
            handle_child_event(
                &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
                parent,
                &events,
                &commands,
                false,
                &mut state,
            );
            prompts.extend(event_rx.try_iter().filter_map(|event| match event {
                DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(
                    BackgroundWorkTranscriptEvent::Started { prompt, .. },
                )) => prompt,
                _ => None,
            }));
            handle_child_event(
                &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_child"}}),
                parent,
                &events,
                &commands,
                false,
                &mut state,
            );
        }
        assert_eq!(
            prompts,
            vec!["first resume".to_owned(), "second resume".to_owned()],
            "each resume start carries its own prompt, in bind order"
        );
    }

    #[test]
    fn a_replayed_resume_call_does_not_rearm_the_child() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let parent = "ses_parent";
        handle_child_event(
            &json!({"type": "session.created", "data": {"session": {"id": "ses_child", "parentID": parent}}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        while event_rx.try_recv().is_ok() {}

        let bind = |state: &mut OpenCodeStreamState| {
            handle_event(
                &json!({"type": "session.tool.input.started", "data": {"id": "call_resume", "name": "task"}}),
                &events,
                &commands,
                &turn,
                0,
                "ses_1",
                false,
                state,
            );
            handle_event(
                &json!({"type": "session.tool.called", "data": {"id": "call_resume", "input": {"prompt": "again", "sessionID": "ses_child"}}}),
                &events,
                &commands,
                &turn,
                0,
                "ses_1",
                false,
                state,
            );
        };
        bind(&mut state);
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        while event_rx.try_recv().is_ok() {}

        // The same call id redelivered must not re-arm the resume, so a
        // straggler execution start stays ignored.
        bind(&mut state);
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(
            !seen.iter().any(|event| matches!(
                event,
                DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))
                    if item.status == BackgroundWorkStatus::Running
            )),
            "a replayed resume call must not revive a settled child"
        );
    }

    #[test]
    fn a_failed_resume_does_not_leave_the_child_armed() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let parent = "ses_parent";
        handle_child_event(
            &json!({"type": "session.created", "data": {"session": {"id": "ses_child", "parentID": parent}}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        while event_rx.try_recv().is_ok() {}

        handle_event(
            &json!({"type": "session.tool.input.started", "data": {"id": "call_resume", "name": "task"}}),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        handle_event(
            &json!({"type": "session.tool.called", "data": {"id": "call_resume", "input": {"prompt": "again", "sessionID": "ses_child"}}}),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        // The resume tool itself fails; its execution never starts.
        handle_event(
            &json!({"type": "session.tool.error", "data": {"id": "call_resume", "error": {"message": "could not resume"}}}),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        while event_rx.try_recv().is_ok() {}

        // A redelivered start must not reopen the child.
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(
            !seen.iter().any(|event| matches!(
                event,
                DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))
                    if item.status == BackgroundWorkStatus::Running
            )),
            "a failed resume must not leave the child armed"
        );
    }

    #[test]
    fn settled_children_stay_quiet_until_a_new_call_binds_them() {
        let (events, event_rx, commands, _command_rx, _turn, mut state) = harness();
        let parent = "ses_parent";
        let created = json!({
            "type": "session.created",
            "data": {"session": {"id": "ses_child", "parentID": parent, "title": "Research"}}
        });
        handle_child_event(&created, parent, &events, &commands, false, &mut state);
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        while event_rx.try_recv().is_ok() {}

        // A redelivered start with no bound call, and a duplicate created, must
        // both leave the settled child alone.
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &commands,
            false,
            &mut state,
        );
        handle_child_event(&created, parent, &events, &commands, false, &mut state);
        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(
            !seen.iter().any(|event| matches!(
                event,
                DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))
                    if item.status == BackgroundWorkStatus::Running
            )),
            "an unbidden start must not revive a settled child"
        );
        assert!(
            !seen.iter().any(|event| matches!(
                event,
                DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(
                    BackgroundWorkTranscriptEvent::Started { .. }
                ))
            )),
            "a duplicate created must not open a transcript turn"
        );
    }

    #[test]
    fn settled_children_ignore_late_stream_events_but_keep_metadata() {
        let (events, event_rx, _commands, _command_rx, _turn, mut state) = harness();
        let parent = "ses_parent";
        handle_child_event(
            &json!({
                "type": "session.created",
                "data": {"session": {"id": "ses_child", "parentID": parent, "title": "Research"}}
            }),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        while event_rx.try_recv().is_ok() {}

        // Deltas after the settlement are dropped, and a duplicate execution
        // event cannot finish the child a second time.
        handle_child_event(
            &json!({"type": "session.text.delta", "data": {"sessionID": "ses_child", "delta": "late"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.tool.called", "data": {"sessionID": "ses_child", "id": "call_1"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        assert!(event_rx.try_recv().is_err(), "settled children are quiet");

        // A rename still applies, carrying the settled status with it.
        handle_child_event(
            &json!({
                "type": "session.updated",
                "data": {"session": {"id": "ses_child", "parentID": parent, "title": "Research notes"}}
            }),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        let DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item)) =
            event_rx.try_recv().unwrap()
        else {
            panic!("the rename must still reach the panel");
        };
        assert_eq!(item.title, "Research notes");
        assert_eq!(item.status, BackgroundWorkStatus::Completed);
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn child_created_twice_keeps_its_title_and_deleted_children_cannot_revive() {
        let (events, event_rx, _commands, _command_rx, _turn, mut state) = harness();
        let parent = "ses_parent";
        let created = json!({
            "type": "session.created",
            "data": {"session": {"id": "ses_child", "parentID": parent, "title": "Research"}}
        });
        handle_child_event(&created, parent, &events, &_commands, false, &mut state);
        handle_child_event(&created, parent, &events, &_commands, false, &mut state);
        handle_child_event(
            &json!({"type": "session.deleted", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        // Everything after the deletion must be dropped for the removed
        // child: late deltas, a replayed execution start, even an update.
        handle_child_event(
            &json!({"type": "session.text.delta", "data": {"sessionID": "ses_child", "delta": "late"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({"type": "session.execution.started", "data": {"sessionID": "ses_child"}}),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );
        handle_child_event(
            &json!({
                "type": "session.updated",
                "data": {"session": {"id": "ses_child", "parentID": parent, "title": "Zombie"}}
            }),
            parent,
            &events,
            &_commands,
            false,
            &mut state,
        );

        assert!(
            state.children.is_empty(),
            "a deleted child must leave the routing table"
        );
        let seen = event_rx.try_iter().collect::<Vec<_>>();
        // One Finished from the deletion, and it is the last word: no event
        // after it may reopen the transcript or flip the status back.
        assert_eq!(
            seen.iter()
                .filter(|event| matches!(
                    event,
                    DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(
                        BackgroundWorkTranscriptEvent::Finished { .. }
                    ))
                ))
                .count(),
            1
        );
        assert!(matches!(
            seen.last(),
            Some(DriverEvent::BackgroundWork(
                BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Finished {
                    success: false,
                    ..
                })
            ))
        ));
        assert!(
            !seen.iter().any(|event| matches!(
                event,
                DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))
                    if item.status == BackgroundWorkStatus::Running
            )),
            "late events must not mark the deleted child running again"
        );
        let stopped = seen
            .iter()
            .find_map(|event| match event {
                DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))
                    if item.status == BackgroundWorkStatus::Stopped =>
                {
                    Some(item.title.clone())
                }
                _ => None,
            })
            .expect("the deletion must settle the child as stopped");
        assert_eq!(stopped, "Research");
    }

    #[test]
    fn child_permission_requests_surface_like_foreground_ones() {
        let (events, event_rx, commands, command_rx, _turn, mut state) = harness();
        handle_child_event(
            &json!({
                "type": "session.created",
                "data": {"session": {"id": "ses_child", "parentID": "ses_parent", "title": "Research"}}
            }),
            "ses_parent",
            &events,
            &commands,
            false,
            &mut state,
        );
        while event_rx.try_recv().is_ok() {}

        // Supervised mode: a subagent's permission ask must reach the user
        // through the same request plumbing as the foreground session's.
        handle_child_event(
            &json!({
                "type": "permission.requested",
                "data": {
                    "id": "per_child",
                    "sessionID": "ses_child",
                    "permission": "bash",
                    "patterns": ["rm -rf /tmp/fintwind-cache"]
                }
            }),
            "ses_parent",
            &events,
            &commands,
            false,
            &mut state,
        );
        let DriverEvent::Permission { request_id, .. } = event_rx.try_recv().unwrap() else {
            panic!("a child permission ask must surface to the user");
        };
        assert_eq!(request_id, "per_child");
        assert!(
            command_rx.try_recv().is_err(),
            "supervised asks wait for the user instead of auto-approving"
        );

        // Full access: the same ask is answered one-shot without prompting.
        handle_child_event(
            &json!({
                "type": "permission.requested",
                "data": {"id": "per_child_2", "sessionID": "ses_child", "permission": "bash"}
            }),
            "ses_parent",
            &events,
            &commands,
            true,
            &mut state,
        );
        let Ok(CommandMessage::Respond {
            request_id,
            session_id,
            ..
        }) = command_rx.try_recv()
        else {
            panic!("full access must answer a child permission ask");
        };
        assert_eq!(request_id, "per_child_2");
        assert_eq!(session_id.as_deref(), Some("ses_child"));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn auto_accept_edits_answers_edits_and_asks_for_shell() {
        let (events, event_rx, commands, command_rx, _turn, mut state) = harness();
        handle_child_event(
            &json!({
                "type": "permission.requested",
                "data": {
                    "id": "per_edit",
                    "sessionID": "ses_child",
                    "action": "edit",
                    "resources": ["src/main.rs"]
                }
            }),
            "ses_parent",
            &events,
            &commands,
            AutoApprove::Edits,
            &mut state,
        );
        let Ok(CommandMessage::Respond { request_id, .. }) = command_rx.try_recv() else {
            panic!("auto-accept edits must answer an edit ask");
        };
        assert_eq!(request_id, "per_edit");
        assert!(event_rx.try_recv().is_err());

        handle_child_event(
            &json!({
                "type": "permission.requested",
                "data": {
                    "id": "per_shell",
                    "sessionID": "ses_child",
                    "permission": "shell",
                    "patterns": ["git status"]
                }
            }),
            "ses_parent",
            &events,
            &commands,
            AutoApprove::Edits,
            &mut state,
        );
        assert!(
            command_rx.try_recv().is_err(),
            "auto-accept edits must still ask for shell"
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::Permission { request_id, .. } if request_id == "per_shell"
        ));
    }

    #[test]
    fn session_permission_rules_match_opencode_access_modes() {
        assert_eq!(
            opencode_session_permissions(RuntimeMode::Ask, InteractionMode::Build),
            json!([
                { "action": "edit", "resource": "*", "effect": "ask" },
                { "action": "shell", "resource": "*", "effect": "ask" },
            ])
        );
        assert_eq!(
            opencode_session_permissions(RuntimeMode::AutoAcceptEdits, InteractionMode::Build),
            json!([
                { "action": "edit", "resource": "*", "effect": "allow" },
                { "action": "shell", "resource": "*", "effect": "ask" },
            ])
        );
        assert_eq!(
            opencode_session_permissions(RuntimeMode::Auto, InteractionMode::Build),
            json!([{ "action": "*", "resource": "*", "effect": "allow" }])
        );
        assert_eq!(
            opencode_session_permissions(RuntimeMode::Ask, InteractionMode::Plan),
            json!([{ "action": "shell", "resource": "*", "effect": "ask" }])
        );
        assert_eq!(
            opencode_session_permissions(RuntimeMode::FullAccess, InteractionMode::Plan),
            json!([{ "action": "shell", "resource": "*", "effect": "allow" }])
        );
    }

    #[test]
    fn permission_decision_is_sent_before_the_legacy_reply_field() {
        let mut bodies = Vec::new();
        let result = post_permission_decision(
            |body| {
                bodies.push(body.clone());
                Ok(Value::Null)
            },
            "once",
        );
        assert!(result.is_ok());
        assert_eq!(bodies, [json!({"decision": "once"})]);
    }

    #[test]
    fn permission_decision_falls_back_to_reply_only_on_400() {
        let mut bodies = Vec::new();
        let result = post_permission_decision(
            |body| {
                bodies.push(body.clone());
                if body.get("decision").is_some() {
                    anyhow::bail!(
                        "OpenCode session request failed with HTTP 400: {{\"kind\":\"Payload\",\"message\":\"Missing key\"}}"
                    );
                }
                Ok(Value::Null)
            },
            "always",
        );
        assert!(result.is_ok());
        assert_eq!(
            bodies,
            [json!({"decision": "always"}), json!({"reply": "always"})]
        );

        let mut bodies = Vec::new();
        let missed = post_permission_decision(
            |body| {
                bodies.push(body.clone());
                anyhow::bail!("OpenCode session request failed with HTTP 404: not found");
            },
            "once",
        );
        assert!(missed.is_err());
        assert_eq!(
            bodies,
            [json!({"decision": "once"})],
            "a missing request is not a shape rejection"
        );
    }

    #[test]
    fn permission_shape_400_keeps_the_decision_error_when_reply_also_fails() {
        let result = post_permission_decision(
            |_body| {
                anyhow::bail!(
                    "OpenCode session request failed with HTTP 400: {{\"message\":\"Expected Permission.Reply\"}}"
                );
            },
            "nope",
        );
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("Expected Permission.Reply"),
            "the current-contract error must survive a failed legacy retry, got {error}"
        );
    }

    #[test]
    fn permission_reply_404_still_surfaces_so_ownership_can_retry() {
        let result = post_permission_decision(
            |body| {
                if body.get("decision").is_some() {
                    anyhow::bail!("OpenCode session request failed with HTTP 400: Missing key");
                }
                anyhow::bail!("OpenCode session request failed with HTTP 404: not found");
            },
            "once",
        );
        assert!(
            result.unwrap_err().to_string().contains("HTTP 404"),
            "a legacy 404 must stay a 404 so the owning session can be retried"
        );
    }

    #[test]
    fn child_permission_reply_targets_the_child_session() {
        assert_eq!(
            permission_reply_path("ses_child", "per_child"),
            "/api/session/ses_child/permission/per_child/reply"
        );
        assert!(
            !permission_reply_path("ses_child", "per_child").contains("ses_parent"),
            "a child reply must not be posted on the parent session"
        );
        assert_eq!(
            form_reply_path("ses_child", "frm_child"),
            "/api/session/ses_child/form/frm_child/reply"
        );
    }

    #[test]
    fn child_permission_before_session_created_still_surfaces() {
        let (events, event_rx, commands, command_rx, _turn, mut state) = harness();
        handle_child_event(
            &json!({
                "type": "permission.requested",
                "data": {
                    "id": "per_early",
                    "sessionID": "ses_child",
                    "permission": "external_directory",
                    "patterns": ["C:/Users/foo/.cargo/git/checkouts"]
                }
            }),
            "ses_parent",
            &events,
            &commands,
            false,
            &mut state,
        );
        let DriverEvent::Permission { request_id, .. } = event_rx.try_recv().unwrap() else {
            panic!("a child permission that races ahead of session.created must still surface");
        };
        assert_eq!(request_id, "per_early");
        assert!(command_rx.try_recv().is_err());
        assert_eq!(
            state
                .permissions
                .lock()
                .pending
                .get("per_early")
                .and_then(|request| request.session_id.as_deref()),
            Some("ses_child")
        );
    }

    #[test]
    fn session_family_accepts_descendants_and_rejects_strangers() {
        let family = SessionFamily::new("ses_parent".into());
        let probe = |id: &str| match id {
            "ses_child" => Ok(Some("ses_parent".into())),
            "ses_grand" => Ok(Some("ses_child".into())),
            "ses_other" => Ok(Some("ses_unrelated".into())),
            "ses_root" => Ok(None),
            "ses_missing" => Err(SessionProbeError::NotFound),
            _ => Err(SessionProbeError::Unavailable),
        };
        assert!(family.belongs_with_probe("ses_parent", probe));
        assert!(family.belongs_with_probe("ses_child", probe));
        assert!(family.belongs_with_probe("ses_grand", probe));
        assert!(!family.belongs_with_probe("ses_other", probe));
        assert!(!family.belongs_with_probe("ses_root", probe));
        assert!(!family.belongs_with_probe("ses_missing", probe));
        assert!(
            family.contains("ses_child") && family.contains("ses_grand"),
            "successful probes must be remembered for the poll and later events"
        );
        assert!(
            !family.contains("ses_other"),
            "a shared-server stranger must not join this session family"
        );
    }

    #[test]
    fn permission_404_retries_on_the_owning_session() {
        let pending = json!({
            "data": [{
                "id": "per_child",
                "sessionID": "ses_child"
            }]
        });
        assert_eq!(
            retry_session_after_not_found("ses_parent", "per_child", &pending).as_deref(),
            Some("ses_child")
        );
        assert_eq!(
            retry_session_after_not_found("ses_child", "per_child", &pending),
            None,
            "retrying the same session cannot recover a genuine miss"
        );

        let mut attempted = Vec::new();
        let mut rejected = Vec::new();
        let result = post_owned_reply(
            |path| {
                attempted.push(path.to_owned());
                if path.contains("ses_parent") {
                    anyhow::bail!("OpenCode session request failed with HTTP 404: not found");
                }
                Ok(Value::Null)
            },
            |_path| Ok(pending.clone()),
            |path| {
                rejected.push(path.to_owned());
                Ok(Value::Null)
            },
            |session| permission_reply_path(session, "per_child"),
            "ses_parent",
            "per_child",
            "/api/permission/request",
        );
        assert!(result.is_ok());
        assert_eq!(
            attempted,
            [
                permission_reply_path("ses_parent", "per_child"),
                permission_reply_path("ses_child", "per_child"),
            ]
        );
        assert!(rejected.is_empty());

        let mut attempted = Vec::new();
        let mut rejected = Vec::new();
        let result = post_owned_reply(
            |path| {
                attempted.push(path.to_owned());
                anyhow::bail!("OpenCode session request failed with HTTP 404: not found");
            },
            |_path| Ok(pending.clone()),
            |path| {
                rejected.push(path.to_owned());
                Ok(Value::Null)
            },
            |session| permission_reply_path(session, "per_child"),
            "ses_parent",
            "per_child",
            "/api/permission/request",
        );
        assert!(result.is_err());
        assert_eq!(
            rejected,
            [permission_reply_path("ses_child", "per_child")],
            "a failed retry must reject on the owning session so the tool does not stay running"
        );
    }

    #[test]
    fn dispatch_accepts_remembered_child_permission() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        let family = SessionFamily::new("ses_parent".into());
        family.remember("ses_child");
        dispatch_server_event(
            &json!({
                "type": "permission.requested",
                "data": {
                    "id": "per_child",
                    "sessionID": "ses_child",
                    "permission": "external_directory",
                    "patterns": ["C:/Users/foo/.cargo"]
                }
            }),
            "ses_parent",
            &events,
            &commands,
            &turn,
            0,
            false,
            &mut state,
            &family,
        );
        let DriverEvent::Permission { request_id, .. } = event_rx.try_recv().unwrap() else {
            panic!("the poll and event paths must accept a known child session");
        };
        assert_eq!(request_id, "per_child");
        assert_eq!(
            permission_responses(&state.permissions, "per_child", "once"),
            [("per_child".into(), "once".into(), Some("ses_child".into()))]
        );
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn unknown_child_permission_waits_for_family_membership() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        let family = SessionFamily::new("ses_parent".into());
        dispatch_server_event(
            &json!({
                "type": "permission.requested",
                "data": {
                    "id": "per_early",
                    "sessionID": "ses_child",
                    "permission": "external_directory"
                }
            }),
            "ses_parent",
            &events,
            &commands,
            &turn,
            0,
            false,
            &mut state,
            &family,
        );
        assert!(
            event_rx.try_recv().is_err(),
            "the event thread must not block on HTTP to classify an unknown session"
        );
        assert!(command_rx.try_recv().is_err());
        family.remember("ses_child");
        dispatch_server_event(
            &json!({
                "type": "permission.requested",
                "data": {
                    "id": "per_early",
                    "sessionID": "ses_child",
                    "permission": "external_directory"
                }
            }),
            "ses_parent",
            &events,
            &commands,
            &turn,
            0,
            false,
            &mut state,
            &family,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::Permission { request_id, .. } if request_id == "per_early"
        ));
    }

    #[test]
    fn missing_pending_permission_does_not_fall_back_to_the_parent() {
        let permissions = Mutex::new(OpenCodePermissionState::default());
        assert!(permission_responses(&permissions, "per_gone", "once").is_empty());
    }

    #[test]
    fn foreign_permission_events_do_not_surface_on_this_session() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        let family = SessionFamily::new("ses_parent".into());
        assert!(!family.belongs_with_probe("ses_stranger", |_| Ok(None)));
        dispatch_server_event(
            &json!({
                "type": "permission.requested",
                "data": {
                    "id": "per_other",
                    "sessionID": "ses_stranger",
                    "permission": "bash"
                }
            }),
            "ses_parent",
            &events,
            &commands,
            &turn,
            0,
            false,
            &mut state,
            &family,
        );
        assert!(
            event_rx.try_recv().is_err(),
            "pending asks on a shared server must not pop in this session"
        );
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn child_form_records_the_owning_session() {
        let (events, event_rx) = unbounded();
        let forms = Mutex::new(OpenCodeFormState::default());
        request_user_input_from_form(
            &json!({
                "form": {
                    "id": "frm_child",
                    "sessionID": "ses_child",
                    "metadata": {"kind": "question"},
                    "fields": [{
                        "key": "q0",
                        "title": "Color",
                        "description": "Which?",
                        "type": "string",
                        "options": [{"value": "Red", "label": "Red"}]
                    }]
                }
            }),
            &forms,
            &events,
        )
        .expect("a child question form must surface");
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::UserInputRequested { request_id, .. } if request_id == "frm_child"
        ));
        assert_eq!(
            forms.lock().sessions.get("frm_child").map(String::as_str),
            Some("ses_child")
        );
        assert_eq!(
            form_reply_path(forms.lock().sessions.get("frm_child").unwrap(), "frm_child"),
            "/api/session/ses_child/form/frm_child/reply"
        );
    }

    #[test]
    fn question_events_preserve_multiple_selection_and_option_copy() {
        let (events, event_rx) = unbounded();
        request_user_input(
            &json!({
                "id": "question-request",
                "sessionID": "session-1",
                "questions": [{
                    "header": "Files",
                    "question": "Which files should change?",
                    "multiple": true,
                    "options": [{
                        "label": "Source",
                        "description": "Update implementation files"
                    }]
                }]
            }),
            &events,
        );

        let DriverEvent::UserInputRequested {
            request_id,
            questions,
        } = event_rx.try_recv().unwrap()
        else {
            panic!("OpenCode question.asked must use the structured question event");
        };
        assert_eq!(request_id, "question-request");
        assert_eq!(questions[0].id, "question-0-files");
        assert!(questions[0].multi_select);
        assert_eq!(questions[0].options[0].label, "Source");
    }

    #[test]
    fn form_created_delivers_the_question_tool_prompt() {
        let (events, event_rx) = unbounded();
        let forms = Mutex::new(OpenCodeFormState::default());
        request_user_input_from_form(
            &json!({
                "form": {
                    "id": "frm_061c395b7001uJsmhrmjPH0tp4",
                    "sessionID": "ses_f9e4118feffeoIsNOAxowOF7u0",
                    "title": "Questions",
                    "metadata": {
                        "kind": "question",
                        "tool": {"messageID": "msg_1", "id": "call_1"}
                    },
                    "fields": [{
                        "key": "q0",
                        "title": "Favorite Color",
                        "description": "What is your favorite color?",
                        "type": "string",
                        "options": [
                            {"value": "Red", "label": "Red", "description": "A bold color."},
                            {"value": "Blue", "label": "Blue", "description": "A calm color."}
                        ],
                        "custom": true
                    }]
                }
            }),
            &forms,
            &events,
        )
        .expect("a question form must map onto the structured question event");

        let DriverEvent::UserInputRequested {
            request_id,
            questions,
        } = event_rx.try_recv().unwrap()
        else {
            panic!("form.created must deliver the structured question event");
        };
        assert_eq!(request_id, "frm_061c395b7001uJsmhrmjPH0tp4");
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].id, "q0");
        assert_eq!(questions[0].header, "Favorite Color");
        assert_eq!(questions[0].question, "What is your favorite color?");
        assert!(!questions[0].multi_select);
        assert_eq!(questions[0].options[1].label, "Blue");

        // The reply needs the recorded field shapes to type each answer.
        assert_eq!(
            forms.lock().fields.get(request_id.as_str()).cloned(),
            Some(vec![("q0".to_owned(), false)])
        );
    }

    #[test]
    fn form_question_asks_once_per_form_id() {
        let (events, event_rx) = unbounded();
        let forms = Mutex::new(OpenCodeFormState::default());
        let form = json!({
            "form": {
                "id": "frm_once",
                "sessionID": "ses_1",
                "metadata": {"kind": "question"},
                "fields": [{
                    "key": "q0", "title": "Color", "description": "Which?",
                    "type": "multiselect",
                    "options": [{"value": "Red", "label": "Red"}]
                }]
            }
        });
        request_user_input_from_form(&form, &forms, &events).expect("the first sight must ask");
        assert!(
            request_user_input_from_form(&form, &forms, &events).is_none(),
            "the poll and the event must not ask twice"
        );

        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::UserInputRequested { .. }
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn non_question_forms_are_ignored() {
        let (events, _event_rx) = unbounded();
        let forms = Mutex::new(OpenCodeFormState::default());
        let requested = request_user_input_from_form(
            &json!({
                "form": {
                    "id": "frm_other",
                    "sessionID": "ses_1",
                    "title": "Settings",
                    "fields": [{
                        "key": "theme", "title": "Theme", "description": "Pick one",
                        "type": "string",
                        "options": [{"value": "dark", "label": "Dark"}]
                    }]
                }
            }),
            &forms,
            &events,
        );
        assert!(requested.is_none());
        assert!(forms.lock().fields.is_empty());
    }

    #[test]
    fn form_reply_shapes_answers_to_their_fields() {
        let answers = vec![
            UserInputAnswer {
                question_id: "q0".into(),
                answers: vec!["Blue".into()],
            },
            UserInputAnswer {
                question_id: "q1".into(),
                answers: vec!["Red".into(), "Green".into()],
            },
        ];
        let shapes = vec![("q0".to_owned(), true), ("q1".to_owned(), false)];
        assert_eq!(
            form_reply_answer(&shapes, &answers),
            json!({"q0": ["Blue"], "q1": ["Red", "Green"]})
        );

        // Without recorded shapes (driver restarted mid-question) the count
        // guesses: one selection is a string, several an array.
        assert_eq!(
            form_reply_answer(&[], &answers),
            json!({"q0": "Blue", "q1": ["Red", "Green"]})
        );
    }

    /// Drives a real `opencode serve` through the actual driver. Ignored by
    /// default: needs the CLI installed, credentials, and the network. Run with
    /// `cargo test --bin fintwind opencode_session_against_a_real_server -- --ignored`.
    #[test]
    #[ignore = "requires an installed, authenticated opencode"]
    fn opencode_session_against_a_real_server() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        // The task link this test verifies round-trips through the server.
        let task_id = uuid::Uuid::new_v4().to_string();
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCodeDriver::start(
            DriverStartOptions {
                binary: binary.clone(),
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: Some("glmcoding/glm-5.3-flash".into()),
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,

                provider_cursor: None,
                task_id: Some(task_id.clone()),
            },
            events,
        )
        .expect("the server should start and open a session");

        let connected = event_rx
            .recv_timeout(std::time::Duration::from_secs(90))
            .expect("the server should report its session");
        let source_session_id = match connected {
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::OpenCode { session_id }),
            } => session_id,
            event => panic!("expected an OpenCode cursor, got {event:?}"),
        };

        // The task id rides the session's `metadata`, so any client of the
        // server — this app reconciling, the CLI, the TUI — can trace the
        // session back to the task that owns it.
        {
            let server = crate::opencode_pool::acquire(&binary, &std::env::temp_dir())
                .expect("the resident server should be reachable");
            let recorded = server
                .request("GET", &format!("/api/session/{source_session_id}"), None)
                .expect("the session should read back");
            assert_eq!(
                recorded
                    .pointer("/data/metadata/task")
                    .and_then(Value::as_str),
                Some(task_id.as_str()),
                "the session metadata should name the owning task, got {recorded}"
            );
            assert_eq!(
                recorded
                    .pointer("/data/metadata/source")
                    .and_then(Value::as_str),
                Some("fintwind"),
            );
        }

        driver.prompt(
            "Reply with exactly: OK. Do not use any tools.".into(),
            Vec::new(),
        );
        let mut text = String::new();
        let mut finished = None;
        let mut context_tokens = None;
        let mut context_window = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(5)) else {
                continue;
            };
            match event {
                DriverEvent::TextDelta { delta, .. } => text.push_str(&delta),
                DriverEvent::UsageUpdated {
                    context_tokens: tokens,
                    context_window: window,
                    ..
                } => {
                    context_tokens = tokens.or(context_tokens);
                    context_window = window.or(context_window);
                }
                DriverEvent::TurnFinished { success, .. } => {
                    finished = Some(success);
                }
                DriverEvent::Error(error) => panic!("the server reported: {error}"),
                _ => {}
            }
            if finished.is_some()
                && context_tokens.is_some_and(|tokens| tokens > 0)
                && context_window.is_some_and(|window| window > 0)
            {
                break;
            }
        }
        assert_eq!(finished, Some(true), "the turn should settle successfully");
        assert!(
            text.contains("OK"),
            "expected the reply to stream through, got {text:?}"
        );
        assert!(context_tokens.is_some_and(|tokens| tokens > 0));
        assert!(context_window.is_some_and(|window| window > 0));

        let ProviderResumeCursor::OpenCode {
            session_id: fork_session_id,
        } = driver
            .fork(1)
            .expect("the resident server should fork away the completed turn");
        assert_ne!(fork_session_id, source_session_id);
    }

    /// The provider status pipeline against a real `opencode`: a plain turn
    /// over `deepseek/deepseek-v4-flash` must surface the runner's
    /// `session.status busy` as `ProviderBusy` before the first delta and
    /// still settle through the execution event. Run with
    /// `cargo test -p fintwind-core provider_status_signals -- --ignored`.
    #[test]
    #[ignore = "requires an installed, authenticated opencode"]
    fn provider_status_signals_flow_against_a_real_server() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCodeDriver::start(
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: Some("deepseek/deepseek-v4-flash".into()),
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,

                provider_cursor: None,
                task_id: None,
            },
            events,
        )
        .expect("the server should start and open a session");
        let _connected = event_rx
            .recv_timeout(std::time::Duration::from_secs(90))
            .expect("the server should report its session");

        driver.prompt(
            "Reply with exactly: OK. Do not use any tools.".into(),
            Vec::new(),
        );
        let mut saw_busy = false;
        let mut retries = Vec::new();
        let mut text = String::new();
        let mut finished = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(5)) else {
                continue;
            };
            match event {
                DriverEvent::ProviderBusy => saw_busy = true,
                DriverEvent::ProviderRetry {
                    attempt, message, ..
                } => retries.push((attempt, message)),
                DriverEvent::TextDelta { delta, .. } => text.push_str(&delta),
                DriverEvent::TurnFinished { success, .. } => finished = Some(success),
                DriverEvent::Error(error) => panic!("the server reported: {error}"),
                _ => {}
            }
            if finished.is_some() {
                break;
            }
        }
        assert_eq!(finished, Some(true), "the turn should settle successfully");
        assert!(
            text.contains("OK"),
            "expected the reply to stream through, got {text:?}"
        );
        assert!(
            saw_busy,
            "the runner's busy status must surface as ProviderBusy"
        );
        // Retries only fire on provider failures, so their absence is fine —
        // but any that did fire must have carried a non-empty reason.
        assert!(
            retries.iter().all(|(_, message)| !message.is_empty()),
            "retry events must carry the provider's reason: {retries:?}"
        );
    }

    /// Drives the `question` tool against a real `opencode`: the prompt
    /// must arrive as a form (`form.created`, not the question events older
    /// docs describe), surface as a structured question request, and the
    /// reply must settle the form and the turn. Ignored by default: needs
    /// the CLI installed with working provider credentials. Run with
    /// `cargo test --bin fintwind question_form_against_a_real_server -- --ignored`.
    #[test]
    #[ignore = "requires an installed, authenticated opencode"]
    fn question_form_against_a_real_server() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCodeDriver::start(
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: Some("opencode-go/deepseek-v4-flash".into()),
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,

                provider_cursor: None,
                task_id: None,
            },
            events,
        )
        .expect("the server should start and open a session");

        match event_rx
            .recv_timeout(std::time::Duration::from_secs(90))
            .expect("the server should report its session")
        {
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::OpenCode { .. }),
            } => {}
            event => panic!("expected an OpenCode cursor, got {event:?}"),
        }

        driver.prompt(
            "Use the question tool to ask me exactly one question with three options: \
             what is my favorite color?"
                .into(),
            Vec::new(),
        );
        let request = loop {
            let event = event_rx
                .recv_timeout(std::time::Duration::from_secs(120))
                .expect("the question should arrive before the deadline");
            match event {
                DriverEvent::UserInputRequested {
                    request_id,
                    questions,
                } => break (request_id, questions),
                DriverEvent::Error(error) => panic!("the server reported: {error}"),
                _ => {}
            }
        };
        assert!(request.0.starts_with("frm_"), "got request {}", request.0);
        assert_eq!(request.1.len(), 1);
        assert_eq!(request.1[0].options.len(), 3);

        driver.respond_user_input(
            request.0.clone(),
            vec![UserInputAnswer {
                question_id: request.1[0].id.clone(),
                answers: vec![request.1[0].options[0].label.clone()],
            }],
        );
        let mut finished = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while finished.is_none() && std::time::Instant::now() < deadline {
            let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(5)) else {
                continue;
            };
            match event {
                DriverEvent::TurnFinished { success, .. } => finished = Some(success),
                DriverEvent::Error(error) => panic!("the server reported: {error}"),
                _ => {}
            }
        }
        assert_eq!(
            finished,
            Some(true),
            "the turn should settle after the reply"
        );
    }

    /// Proves steering through the actual driver: the message injected while
    /// the bash tool sleeps lands inside the same turn — one SteerAccepted,
    /// one TurnFinished, and a reply that honors both instructions. Ignored by
    /// default: needs the CLI installed, credentials, and the network.
    #[test]
    #[ignore = "requires an installed, authenticated opencode"]
    fn opencode_steering_folds_a_mid_turn_message_into_the_running_turn() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCodeDriver::start(
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: Some("glmcoding/glm-5.3-flash".into()),
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,

                provider_cursor: None,
                task_id: None,
            },
            events,
        )
        .expect("the server should start and open a session");

        driver.prompt(
            "Use the bash tool to run exactly `sleep 6` (nothing else). \
             After the command completes, reply with exactly: FIRST DONE"
                .into(),
            Vec::new(),
        );

        let mut text = String::new();
        let mut steered = false;
        let mut steer_accepted = false;
        let mut turns_finished = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(5)) else {
                // Quiet after the turn settled means no second turn is coming.
                if turns_finished == 1 {
                    break;
                }
                continue;
            };
            match event {
                DriverEvent::RichActivity(item) if !steered && !item.complete => {
                    // The tool is running: the turn is unambiguously live.
                    steered = true;
                    driver.steer(
                        "ADDITIONAL INSTRUCTION: end your very next reply \
                         with the word BANANA."
                            .into(),
                        Vec::new(),
                    );
                }
                DriverEvent::SteerAccepted { message } => {
                    assert!(message.contains("BANANA"));
                    steer_accepted = true;
                }
                DriverEvent::SteerRejected { reason, .. } => {
                    panic!("the steer should be accepted, got rejection: {reason}");
                }
                DriverEvent::TextDelta { delta, .. } => text.push_str(&delta),
                DriverEvent::TurnFinished { success, .. } => {
                    assert!(success, "the turn should settle successfully");
                    turns_finished += 1;
                }
                DriverEvent::Error(error) => panic!("the server reported: {error}"),
                _ => {}
            }
        }

        assert!(steered, "the probe never saw the tool start");
        assert!(steer_accepted, "the driver should acknowledge the steer");
        assert_eq!(
            turns_finished, 1,
            "a steered message must not settle a second turn"
        );
        assert!(
            text.contains("BANANA"),
            "the steered instruction should shape the same turn's reply, got {text:?}"
        );
    }

    #[test]
    fn streams_text_and_correlated_tools_and_settles_on_execution() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        // Payloads copied from a live `opencode serve` event stream.
        let wire = [
            json!({"type":"session.text.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"OK"}}),
            json!({"type":"session.reasoning.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"thinking"}}),
            json!({"type":"session.tool.input.started","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","id":"call_1","name":"read"}}),
            json!({"type":"session.tool.called","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","id":"call_1","input":{"filePath":"a.txt"},"executed":false}}),
            json!({"type":"session.tool.success","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","id":"call_1","content":[{"type":"text","text":"contents"}],"metadata":{"status":"completed"}}}),
            // Not transcript content.
            json!({"type":"session.inbox.enqueued","data":{"sessionID":"ses_1","inboxID":"msg_0","item":{"type":"user"}}}),
            json!({"type":"session.usage.updated","data":{"sessionID":"ses_1","cost":0,"tokens":{"input":1,"output":1,"cache":{"read":0,"write":0}}}}),
            json!({"type":"session.execution.succeeded","data":{"sessionID":"ses_1"}}),
        ];
        for event in wire {
            handle_event(
                &event, &events, &commands, &turn, 0, "ses_1", true, &mut state,
            );
        }

        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert!(
            matches!(&seen[0], DriverEvent::TextDelta { part, delta } if part == "text:msg_1:0" && delta == "OK")
        );
        assert!(
            matches!(&seen[1], DriverEvent::ReasoningDelta { delta: text, .. } if text == "thinking")
        );
        assert!(matches!(&seen[2], DriverEvent::RichActivity(item)
                if item.source_id.as_deref() == Some("call_1")
                    && item.kind == ActivityKind::FileRead && !item.complete
                    && item.arguments.is_none()));
        assert!(matches!(&seen[3], DriverEvent::RichActivity(item)
                if item.kind == ActivityKind::FileRead
                    && !item.complete
                    && item.display_target.as_deref() == Some("a.txt")));
        assert!(matches!(&seen[4], DriverEvent::RichActivity(item)
                if item.complete && item.title == "read"));
        assert!(matches!(
            &seen[5],
            DriverEvent::UsageUpdated {
                // The cumulative session row — totals only, never occupancy.
                context_tokens: None,
                context_window: None,
                session_total: Some(2),
                ..
            }
        ));
        assert!(matches!(
            &seen[6],
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert_eq!(seen.len(), 7, "non-transcript events leaked");
        assert!(!*turn.lock(), "the turn should be settled exactly once");
    }

    #[test]
    fn execute_progress_surfaces_codemode_nested_tool_calls() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let wire = [
            json!({"type":"session.tool.input.started","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","id":"call_ex","name":"execute"}}),
            json!({"type":"session.tool.called","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","id":"call_ex","input":{"code":"return await tools.context7.query_docs({ libraryId: '/opencode' })"}}}),
            json!({"type":"session.tool.progress","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","id":"call_ex","metadata":{"toolCalls":[{"tool":"context7.query_docs","status":"running"}]}}}),
            json!({"type":"session.tool.success","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","id":"call_ex","content":[{"type":"text","text":"ok"}],"metadata":{"toolCalls":[{"tool":"context7.query_docs","status":"completed"}]}}}),
        ];
        for event in wire {
            handle_event(
                &event, &events, &commands, &turn, 0, "ses_1", true, &mut state,
            );
        }

        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert!(matches!(&seen[0], DriverEvent::RichActivity(item)
            if item.source_id.as_deref() == Some("call_ex")
                && item.kind == ActivityKind::Tool
                && item.title == "execute"
                && !item.complete));
        assert!(matches!(&seen[1], DriverEvent::RichActivity(item)
            if item.kind == ActivityKind::Tool
                && item.display_target.as_deref()
                    == Some("return await tools.context7.query_docs({ libraryId: '/opencode' })")));
        assert!(matches!(&seen[2], DriverEvent::RichActivity(item)
            if item.display_target.as_deref() == Some("context7.query_docs") && !item.complete));
        assert!(matches!(&seen[3], DriverEvent::RichActivity(item)
            if item.complete
                && item.title == "execute"
                && item.display_target.as_deref() == Some("context7.query_docs")));
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn reasoning_fragments_key_their_deltas_and_settle_from_the_durable_end() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        // A thought's buffered tail can flush after the next tool's events
        // have already landed; started/ended carry the (message, ordinal)
        // identity the stored reasoning part keeps, and ended carries the
        // authoritative full text.
        let wire = [
            json!({"type":"session.reasoning.started","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0}}),
            json!({"type":"session.reasoning.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"let me look"}}),
            json!({"type":"session.tool.input.started","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","id":"call_1","name":"grep"}}),
            json!({"type":"session.reasoning.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"!"}}),
            json!({"type":"session.reasoning.ended","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"text":"let me look!"}}),
        ];
        for event in wire {
            handle_event(
                &event, &events, &commands, &turn, 0, "ses_1", true, &mut state,
            );
        }

        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert!(
            matches!(&seen[0], DriverEvent::ReasoningStarted { part } if part == "reasoning:msg_1:0")
        );
        assert!(
            matches!(&seen[1], DriverEvent::ReasoningDelta { part, delta }
                if part == "reasoning:msg_1:0" && delta == "let me look")
        );
        assert!(matches!(&seen[2], DriverEvent::RichActivity(_)));
        // The late tail keeps its fragment key even though a tool preceded it.
        assert!(
            matches!(&seen[3], DriverEvent::ReasoningDelta { part, delta }
                if part == "reasoning:msg_1:0" && delta == "!")
        );
        assert!(
            matches!(&seen[4], DriverEvent::ReasoningEnded { part, text }
                if part == "reasoning:msg_1:0" && text.as_deref() == Some("let me look!"))
        );
        assert_eq!(seen.len(), 5, "non-transcript events leaked");
    }

    #[test]
    #[ignore = "requires opencode, credentials and FINTWIND_TEST_MODEL"]
    fn tool_input_is_visible_before_execution_against_a_real_server() {
        let cwd =
            std::env::temp_dir().join(format!("fintwind-tool-stream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&cwd).unwrap();
        let (events, event_rx) = crate::driver::test_event_channel();
        let driver = OpenCodeDriver::start(
            DriverStartOptions {
                binary: crate::command_env::find_executable("opencode").unwrap(),
                cwd: cwd.clone(),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: Some(std::env::var("FINTWIND_TEST_MODEL").expect("set a test model")),
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,

                provider_cursor: None,
                task_id: None,
            },
            events,
        )
        .unwrap();
        assert!(matches!(
            event_rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap(),
            DriverEvent::Connected { .. }
        ));
        driver.prompt("Use the write tool to create probe.txt in the current directory with 200 numbered lines, each containing a different short sentence about software testing. Generate the full file in a single tool call. Do not use shell commands or read any other files. Then reply DONE.".into(), Vec::new());
        let began = std::time::Instant::now();
        let mut pending = HashMap::new();
        let mut enriched = HashSet::new();
        let mut completed = HashSet::new();
        let mut success = false;
        while began.elapsed() < std::time::Duration::from_secs(180) {
            let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(5)) else {
                continue;
            };
            match event {
                DriverEvent::RichActivity(item) => {
                    let Some(id) = item.source_id else {
                        continue;
                    };
                    if !item.complete && item.arguments.is_none() {
                        pending.insert(id, began.elapsed());
                    } else if !item.complete {
                        assert!(
                            pending.contains_key(&id),
                            "arguments arrived before the pending activity"
                        );
                        eprintln!(
                            "tool pending at {:?}, arguments at {:?}",
                            pending[&id],
                            began.elapsed()
                        );
                        enriched.insert(id);
                    } else {
                        assert!(!item.failed, "test tool failed");
                        completed.insert(id);
                    }
                }
                DriverEvent::TurnFinished {
                    success: finished, ..
                } => {
                    success = finished;
                    break;
                }
                DriverEvent::Error(error) => panic!("provider error: {error}"),
                _ => {}
            }
        }
        if !success {
            driver.cancel();
        }
        assert!(success, "test turn did not complete");
        assert!(!enriched.is_empty());
        assert!(enriched.iter().all(|id| completed.contains(id)));
        assert!(std::fs::metadata(cwd.join("probe.txt")).unwrap().len() > 1_000);
        std::fs::remove_file(cwd.join("probe.txt")).unwrap();
        // OpenCode may leave workspace metadata; do not remove unknown files.
        let _ = std::fs::remove_dir(cwd);
    }

    #[test]
    fn text_part_events_carry_the_stored_part_key() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let wire = [
            json!({"type":"session.text.started","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0}}),
            json!({"type":"session.text.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"了解"}}),
            json!({"type":"session.text.ended","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"text":"了解结构。"}}),
        ];
        for event in wire {
            handle_event(
                &event, &events, &commands, &turn, 0, "ses_1", true, &mut state,
            );
        }
        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(matches!(&seen[0], DriverEvent::TextStarted { part } if part == "text:msg_1:0"));
        assert!(matches!(
            &seen[1],
            DriverEvent::TextDelta { part, delta } if part == "text:msg_1:0" && delta == "了解"
        ));
        assert!(matches!(
            &seen[2],
            DriverEvent::TextEnded { part, text }
                if part == "text:msg_1:0" && text.as_deref() == Some("了解结构。")
        ));
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn v2_reasoning_and_text_flows_classify_by_their_own_events() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        // opencode separates the thought and answer streams into their own
        // events, so no part classification is needed.
        let wire = [
            json!({"type":"session.reasoning.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"thinking"}}),
            json!({"type":"session.text.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"answer"}}),
            json!({"type":"session.text.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_2","ordinal":0,"delta":" tail"}}),
            json!({"type":"session.execution.succeeded","data":{"sessionID":"ses_1"}}),
        ];
        for event in wire {
            handle_event(
                &event, &events, &commands, &turn, 0, "ses_1", true, &mut state,
            );
        }

        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(
            matches!(&seen[0], DriverEvent::ReasoningDelta { delta: text, .. } if text == "thinking")
        );
        assert!(
            matches!(&seen[1], DriverEvent::TextDelta { part, delta } if part == "text:msg_1:0" && delta == "answer")
        );
        assert!(
            matches!(&seen[2], DriverEvent::TextDelta { part, delta } if part == "text:msg_2:0" && delta == " tail")
        );
        assert!(matches!(
            &seen[3],
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn usage_events_feed_opencode_context_usage() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        state
            .usage_metadata
            .model_context_windows
            .lock()
            .insert("glmcoding/glm-5.3-flash".into(), 200_000);

        // The step announces the model. The usage event then carries the
        // session's cumulative row — totals only, never occupancy.
        handle_event(
            &json!({
                "type": "session.step.started",
                "data": {
                    "sessionID": "ses_1",
                    "agent": "build",
                    "model": {"id": "glm-5.3-flash", "providerID": "glmcoding"}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        handle_event(
            &json!({
                "type": "session.usage.updated",
                "data": {
                    "sessionID": "ses_1",
                    "cost": 0,
                    "tokens": {
                        "input": 13_399,
                        "output": 10,
                        "reasoning": 0,
                        "cache": {"read": 1_792, "write": 0}
                    }
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );

        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::UsageUpdated {
                context_tokens: None,
                context_window: Some(200_000),
                context_window_resolved: true,
                session_total: Some(15_201),
                cache_read: Some(1_792),
                prompt_tokens: Some(15_191),
                latest: None
            }
        ));

        // The settled step is what reports occupancy — and the authoritative
        // row keeps the totals from growing a second time.
        handle_event(
            &json!({
                "type": "session.step.ended",
                "data": {
                    "sessionID": "ses_1",
                    "tokens": {
                        "input": 11_607,
                        "output": 10,
                        "reasoning": 0,
                        "cache": {"read": 1_792, "write": 0}
                    }
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::UsageUpdated {
                context_tokens: Some(13_409),
                context_window: Some(200_000),
                context_window_resolved: true,
                session_total: Some(15_201),
                cache_read: Some(1_792),
                prompt_tokens: Some(15_191),
                latest: Some(_)
            }
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn step_ended_grows_the_session_total_exactly_once_per_step() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();

        // Two settled steps in the normalized shape (input excludes the
        // cached tokens); each contributes once to the cumulative total.
        for (input, output) in [(11_607, 10), (12_198, 25)] {
            handle_event(
                &json!({
                    "type": "session.step.ended",
                    "data": {
                        "sessionID": "ses_1",
                        "tokens": {
                            "input": input,
                            "output": output,
                            "reasoning": 0,
                            "cache": {"read": 1_792, "write": 0}
                        }
                    }
                }),
                &events,
                &commands,
                &turn,
                0,
                "ses_1",
                true,
                &mut state,
            );
        }

        let mut total = 0;
        let mut contexts = Vec::new();
        let mut caches = Vec::new();
        let mut prompts = Vec::new();
        while let Ok(DriverEvent::UsageUpdated {
            context_tokens,
            session_total: Some(session_total),
            cache_read,
            prompt_tokens,
            ..
        }) = event_rx.try_recv()
        {
            total = session_total;
            contexts.push(context_tokens);
            caches.push(cache_read);
            prompts.push(prompt_tokens);
        }
        assert_eq!(
            contexts,
            vec![Some(13_409), Some(14_015)],
            "each step should publish its own occupancy"
        );
        assert_eq!(total, 13_409 + 14_015, "steps should sum exactly once");
        assert_eq!(
            caches,
            vec![Some(1_792), Some(3_584)],
            "cache hits accumulate across every call"
        );
        assert_eq!(
            prompts,
            vec![Some(13_399), Some(27_389)],
            "prompt tokens accumulate across every call"
        );
    }

    #[test]
    fn step_events_accumulate_turn_statistics_and_flush_at_the_terminal_event() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();

        // The first step is a tool step: its tokens and streaming time fold
        // into the accumulator, which stays open regardless of `finish`.
        handle_event(
            &json!({
                "type": "session.step.started",
                "data": {
                    "sessionID": "ses_1",
                    "assistantMessageID": "msg_1",
                    "agent": "build",
                    "model": {"id": "glm-5.3-flash", "providerID": "glmcoding"}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        handle_event(
            &json!({
                "type": "session.step.ended",
                "data": {
                    "sessionID": "ses_1",
                    "assistantMessageID": "msg_1",
                    "finish": "tool-calls",
                    "tokens": {"input": 100, "output": 12, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                    "time": {"created": 1_000_u64, "streamed": 3_500_u64}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );

        // The final step names the agent and model the footer shows; it
        // carries no `time`, so its duration falls back to the wall clock
        // its `step.started` armed. Two milliseconds of separation keeps that
        // fallback measurably positive without making the test slow.
        std::thread::sleep(Duration::from_millis(2));
        handle_event(
            &json!({
                "type": "session.step.started",
                "data": {
                    "sessionID": "ses_1",
                    "assistantMessageID": "msg_2",
                    "agent": "explore",
                    "model": {"id": "glm-5.3", "providerID": "glmcoding"}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        handle_event(
            &json!({
                "type": "session.step.ended",
                "data": {
                    "sessionID": "ses_1",
                    "assistantMessageID": "msg_2",
                    "finish": "stop",
                    "tokens": {"input": 130, "output": 90, "reasoning": 8, "cache": {"read": 0, "write": 0}}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        // A terminal `finish` does not flush: the turn's terminal event owns
        // the single delivery, so nothing has left the accumulator yet.
        assert!(
            !event_rx
                .try_iter()
                .any(|event| matches!(event, DriverEvent::TurnStatsUpdated(_))),
            "statistics must not flush before the turn's terminal event"
        );

        handle_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_1"}}),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );

        let mut stats = Vec::new();
        let mut finished = false;
        for event in event_rx.try_iter() {
            match event {
                DriverEvent::TurnStatsUpdated(payload) => stats.push(payload),
                DriverEvent::TurnFinished { .. } => finished = true,
                _ => {}
            }
        }
        assert_eq!(
            stats.len(),
            1,
            "the flush happens once, at the terminal event"
        );
        // The TUI footer's numerator: output plus reasoning, summed across
        // the steps — 12 + (90 + 8).
        assert_eq!(stats[0].output_tokens, 110);
        // 2_500 from the tool step's payload time, plus a wall-clock
        // fallback that only has to be non-negative on the final step.
        assert!(stats[0].stream_ms >= 2_500);
        assert_eq!(stats[0].model.as_deref(), Some("glmcoding/glm-5.3"));
        assert_eq!(stats[0].agent.as_deref(), Some("explore"));
        assert!(finished, "the turn still settles normally");
    }

    /// A steer, provider retry, or `length` continuation reopens the model
    /// loop after a terminal `finish`, so the second terminal finish must add
    /// to — not replace — what the first segment collected. A degraded
    /// reopen that announces neither model nor agent (a blank agent is not a
    /// name) keeps the earlier step's values instead of blanking them.
    #[test]
    fn a_second_terminal_finish_in_one_turn_keeps_accumulating() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();

        handle_event(
            &json!({
                "type": "session.step.started",
                "data": {
                    "sessionID": "ses_1",
                    "assistantMessageID": "msg_1",
                    "agent": "plan",
                    "model": {"id": "glm-5.3", "providerID": "glmcoding"}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        handle_event(
            &json!({
                "type": "session.step.ended",
                "data": {
                    "sessionID": "ses_1",
                    "assistantMessageID": "msg_1",
                    "finish": "stop",
                    "tokens": {"input": 100, "output": 50, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                    "time": {"created": 1_000_u64, "streamed": 3_000_u64}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        // The reopened segment announces a blank agent and no model at all.
        handle_event(
            &json!({
                "type": "session.step.started",
                "data": {
                    "sessionID": "ses_1",
                    "assistantMessageID": "msg_2",
                    "agent": ""
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        handle_event(
            &json!({
                "type": "session.step.ended",
                "data": {
                    "sessionID": "ses_1",
                    "assistantMessageID": "msg_2",
                    "finish": "length",
                    "tokens": {"input": 130, "output": 30, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                    "time": {"created": 5_000_u64, "streamed": 6_500_u64}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        handle_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_1"}}),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );

        let stats = event_rx
            .try_iter()
            .filter_map(|event| match event {
                DriverEvent::TurnStatsUpdated(payload) => Some(payload),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stats.len(), 1, "both terminal finishes leave as one event");
        assert_eq!(stats[0].output_tokens, 80, "the segments sum, not replace");
        assert_eq!(stats[0].stream_ms, 3_500);
        assert_eq!(
            stats[0].model.as_deref(),
            Some("glmcoding/glm-5.3"),
            "a step without a model keeps the earlier step's"
        );
        assert_eq!(
            stats[0].agent.as_deref(),
            Some("plan"),
            "a blank agent is not a name and does not blank the captured one"
        );
    }

    #[test]
    fn turn_stats_flush_falls_back_to_the_terminal_execution_event() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();

        // A step that ends without a recognized finish — a degraded or
        // replayed stream — leaves the accumulator armed; the turn's
        // terminal event must still deliver what was collected.
        handle_event(
            &json!({
                "type": "session.step.started",
                "data": {
                    "sessionID": "ses_1",
                    "agent": "plan",
                    "model": {"id": "glm-5.3", "providerID": "glmcoding"}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        handle_event(
            &json!({
                "type": "session.step.ended",
                "data": {
                    "sessionID": "ses_1",
                    "tokens": {"input": 100, "output": 7, "reasoning": 0, "cache": {"read": 0, "write": 0}}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        handle_event(
            &json!({"type": "session.execution.succeeded", "data": {"sessionID": "ses_1"}}),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );

        let stats = event_rx
            .try_iter()
            .filter_map(|event| match event {
                DriverEvent::TurnStatsUpdated(stats) => Some(stats),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].output_tokens, 7);
        assert_eq!(stats[0].agent.as_deref(), Some("plan"));
    }

    #[test]
    fn compaction_events_report_the_full_lifecycle() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();

        handle_event(
            &json!({
                "type": "session.compaction.started",
                "data": {"sessionID": "ses_1", "reason": "manual"}
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::CompactionUpdated(CompactionState {
                status: CompactionStatus::Running,
                reason: Some(reason),
                ..
            }) if reason == "manual"
        ));

        handle_event(
            &json!({
                "type": "session.compaction.ended",
                "data": {
                    "sessionID": "ses_1",
                    "reason": "manual",
                    "model": {"id": "glm-5.3-flash", "providerID": "glmcoding"},
                    "summary": "## Objective\n- Compacted."
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::CompactionUpdated(CompactionState {
                status: CompactionStatus::Completed,
                model: Some(model),
                summary: Some(summary),
                ..
            }) if model == "glmcoding/glm-5.3-flash" && summary == "## Objective\n- Compacted."
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn compaction_deltas_promote_a_restarted_driver_exactly_once() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();

        // A compaction outlived this driver: `started` predates it, so the
        // first delta promotes the session, and later deltas stay silent.
        for _ in 0..2 {
            handle_event(
                &json!({
                    "type": "session.compaction.delta",
                    "data": {"sessionID": "ses_1", "delta": "summary so far"}
                }),
                &events,
                &commands,
                &turn,
                0,
                "ses_1",
                true,
                &mut state,
            );
        }
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::CompactionUpdated(CompactionState {
                status: CompactionStatus::Running,
                ..
            })
        ));
        assert!(event_rx.try_recv().is_err());

        // An interrupt withdraws the request instead of failing it, and the
        // live flag resets so a later attempt re-announces from `started`.
        handle_event(
            &json!({
                "type": "session.compaction.failed",
                "data": {"sessionID": "ses_1", "error": {"type": "aborted"}}
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::CompactionUpdated(CompactionState {
                status: CompactionStatus::Cancelled,
                ..
            })
        ));

        handle_event(
            &json!({
                "type": "session.compaction.started",
                "data": {"sessionID": "ses_1", "reason": "auto"}
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::CompactionUpdated(CompactionState {
                status: CompactionStatus::Running,
                reason: Some(reason),
                ..
            }) if reason == "auto"
        ));
    }

    #[test]
    fn compaction_failures_carry_the_provider_error() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();

        handle_event(
            &json!({
                "type": "session.compaction.failed",
                "data": {
                    "sessionID": "ses_1",
                    "reason": "manual",
                    "error": {"type": "CompactionError", "message": "model rejected the summary"}
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::CompactionUpdated(CompactionState {
                status: CompactionStatus::Failed,
                error: Some(error),
                ..
            }) if error == "model rejected the summary"
        ));
    }

    #[test]
    fn latest_opencode_compaction_seeds_the_newest_record() {
        // The message endpoint answers newest-first, so the first compaction
        // entry is the latest attempt and wins the seed.
        let messages = json!({
            "data": [
                {"id": "msg_3", "type": "user", "text": "next prompt"},
                {"id": "msg_2", "type": "compaction", "status": "failed",
                 "error": {"message": "provider 500"}},
                {"id": "msg_1", "type": "compaction", "status": "completed",
                 "model": "glmcoding/glm-5.3-flash",
                 "summary": "## Objective\n- Compacted."}
            ]
        });
        let seeded = latest_opencode_compaction(&messages).unwrap();
        assert_eq!(seeded.status, CompactionStatus::Failed);
        assert_eq!(seeded.error.as_deref(), Some("provider 500"));
        assert_eq!(seeded.summary, None);

        let completed = json!({
            "data": [
                {"id": "msg_1", "type": "compaction", "status": "completed",
                 "summary": "## Objective\n- Compacted."}
            ]
        });
        assert_eq!(
            latest_opencode_compaction(&completed)
                .unwrap()
                .summary
                .as_deref(),
            Some("## Objective\n- Compacted.")
        );

        // Conversations without a compaction seed nothing, and unknown
        // statuses are skipped rather than guessed at.
        assert!(
            latest_opencode_compaction(&json!({"data": [
                {"id": "msg_1", "type": "assistant", "tokens": {}}
            ]}))
            .is_none()
        );
        assert!(
            latest_opencode_compaction(&json!({"data": [
                {"id": "msg_1", "type": "compaction", "status": "mysterious"}
            ]}))
            .is_none()
        );
    }

    #[test]
    fn session_usage_row_is_cumulative_throughput_not_context() {
        // Live evidence: the session whose meter read 545.7k against a
        // 147.3k context carried exactly this row on `session.usage.updated`
        // — 525_421 + 20_321 = 545_742. `input` is the cache-excluded sum
        // across every settled call, so the object is the session record's
        // throughput, never the in-flight context.
        let payload = json!({
            "sessionID": "ses_1",
            "cost": 3.9,
            "tokens": {
                "input": 525_421,
                "output": 20_321,
                "reasoning": 82_280,
                "cache": {"read": 10_741_120, "write": 0}
            }
        });
        let row = opencode_session_row_usage(&payload).unwrap();
        assert_eq!(row.prompt, 525_421 + 10_741_120);
        assert_eq!(row.total, 525_421 + 10_741_120 + 20_321 + 82_280);
        assert_eq!(row.cache_read, 10_741_120);

        // An all-zero row (fresh session) carries nothing.
        assert!(
            opencode_session_row_usage(&json!({"tokens": {
                "input": 0, "output": 0, "reasoning": 0,
                "cache": {"read": 0, "write": 0}
            }}))
            .is_none()
        );
    }

    #[test]
    fn message_usage_splits_prompt_from_context_and_honors_total() {
        // The normalized message shape: cache and reasoning are separate from
        // input/output, so the disjoint fields sum to the context while the
        // prompt unifies the cache with the uncached input.
        let message = json!({"tokens": {
            "input": 246,
            "output": 83,
            "reasoning": 0,
            "cache": {"read": 78_592, "write": 0}
        }});
        let usage = opencode_normalized_usage(&message).unwrap();
        assert_eq!(usage.context, 78_921);
        assert_eq!(usage.prompt, 78_838);
        assert_eq!(usage.cache_read, 78_592);

        // `total`, when present, is that same context reported outright.
        let message = json!({"tokens": {
            "total": 78_921,
            "input": 300,
            "output": 83,
            "reasoning": 0,
            "cache": {"read": 78_592, "write": 0}
        }});
        assert_eq!(opencode_normalized_usage(&message).unwrap().context, 78_921);
    }

    #[test]
    fn usage_seeds_sum_the_stored_tail_and_take_the_newest_call() {
        // Newest first, exactly as the messages endpoint returns them.
        let messages = json!({
            "data": [
                {"type": "assistant", "id": "msg_2", "tokens": {
                    "input": 246, "output": 83, "reasoning": 0,
                    "cache": {"read": 78_592, "write": 0}
                }},
                {"type": "user", "id": "msg_1", "text": "hi"},
                {"type": "assistant", "id": "msg_0", "tokens": {
                    "input": 10, "output": 5, "reasoning": 0,
                    "cache": {"read": 0, "write": 0}
                }}
            ]
        });
        let seed = opencode_usage_seeds(&messages, None);
        assert_eq!(seed.total, 78_921 + 15);
        assert_eq!(seed.cache_read, 78_592);
        assert_eq!(seed.prompt, 78_838 + 10);
        let newest = seed.newest.expect("newest assistant usage");
        assert_eq!(newest.context, 78_921);
        assert_eq!(newest.prompt, 78_838);

        // The session row's stored totals win over a truncated message tail.
        let session = json!({"tokens": {
            "input": 525_421, "output": 20_321, "reasoning": 82_280,
            "cache": {"read": 10_741_120, "write": 0}
        }});
        let seed = opencode_usage_seeds(&messages, Some(&session));
        assert_eq!(seed.total, 525_421 + 10_741_120 + 20_321 + 82_280);
        assert_eq!(seed.cache_read, 10_741_120);
        assert_eq!(seed.prompt, 525_421 + 10_741_120);
        assert_eq!(seed.newest.unwrap().context, 78_921);

        // The session list/detail row stores the same totals as flattened
        // `tokens_*` columns rather than a nested `tokens` object.
        let session = json!({
            "tokens_input": 525_421,
            "tokens_output": 20_321,
            "tokens_reasoning": 82_280,
            "tokens_cache_read": 10_741_120,
            "tokens_cache_write": 0
        });
        let seed = opencode_usage_seeds(&Value::Null, Some(&session));
        assert_eq!(seed.total, 525_421 + 10_741_120 + 20_321 + 82_280);
        assert_eq!(seed.cache_read, 10_741_120);
        assert_eq!(seed.prompt, 525_421 + 10_741_120);
        assert!(seed.newest.is_none());
    }

    #[test]
    fn context_window_lookup_falls_back_across_casing_and_id() {
        let windows = opencode_model_context_windows(&json!({
            "data": [{
                "providerID": "rightcode",
                "id": "grok-4.6",
                "limit": {"context": 500_000}
            }]
        }));
        assert_eq!(
            opencode_lookup_context_window(&windows, "rightcode/grok-4.6"),
            Some(500_000)
        );
        assert_eq!(
            opencode_lookup_context_window(&windows, "RightCode/Grok-4.6"),
            Some(500_000)
        );
        assert_eq!(
            opencode_lookup_context_window(&windows, "other/grok-4.6"),
            Some(500_000)
        );

        // Two providers serving the same id must not lend each other a window.
        // HashMap order would otherwise make the hit depend on which entry is
        // visited first.
        let windows = opencode_model_context_windows(&json!({
            "data": [
                {
                    "providerID": "opencode",
                    "id": "gpt-6-sol",
                    "limit": {"context": 1_050_000}
                },
                {
                    "providerID": "fushengyunsuan",
                    "id": "gpt-6-sol",
                    "limit": {"context": 250_000}
                }
            ]
        }));
        assert_eq!(
            opencode_lookup_context_window(&windows, "fushengyunsuan/gpt-6-sol"),
            Some(250_000)
        );
        assert_eq!(
            opencode_lookup_context_window(&windows, "Fushengyunsuan/GPT-6-sol"),
            Some(250_000)
        );
        assert_eq!(opencode_lookup_context_window(&windows, "gpt-6-sol"), None);
    }

    #[test]
    fn model_metadata_and_last_message_restore_opencode_usage() {
        let models = json!({
            "data": [{
                "providerID": "glmcoding",
                "id": "glm-5.3-flash",
                "limit": {"context": 1_000_000, "output": 384_000}
            }]
        });
        let windows = opencode_model_context_windows(&models);
        let messages = json!({
            "data": [
                {"type": "assistant", "id": "msg_3", "finish": "stop", "model": {
                    "id": "glm-5.3-flash", "providerID": "glmcoding"
                }, "tokens": {
                    "input": 200, "output": 300, "reasoning": 0,
                    "cache": {"read": 0, "write": 0}
                }},
                {"type": "assistant", "id": "msg_2", "finish": "stop", "tokens": {
                    "input": 15_450, "output": 17, "reasoning": 0,
                    "cache": {"read": 0, "write": 0}
                }},
                {"type": "user", "id": "msg_1", "text": "hi"}
            ],
            "cursor": {"previous": null, "next": null}
        });

        let latest = latest_opencode_usage_message(&messages).expect("latest assistant usage");
        assert_eq!(
            opencode_normalized_usage(latest).map(|usage| usage.context),
            Some(500)
        );
        assert_eq!(
            opencode_message_model_key(latest).as_deref(),
            Some("glmcoding/glm-5.3-flash")
        );
        assert_eq!(
            opencode_message_model_key(latest)
                .as_ref()
                .and_then(|model| opencode_lookup_context_window(&windows, model)),
            Some(1_000_000)
        );
    }

    #[test]
    fn generated_session_titles_replace_the_local_fallback() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();

        // opencode emits the final generated title once through `session.renamed`.
        handle_event(
            &json!({
                "type": "session.renamed",
                "data": {
                    "sessionID": "ses_1",
                    "title": "Generated provider title"
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::AutoTitleUpdated(Some(title)) if title == "Generated provider title"
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn permission_approvals_stay_driver_local() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        // Shape from the server's OpenAPI PermissionRequest schema.
        let permission = json!({
            "type": "permission.requested",
            "properties": {
                "id": "per_abc",
                "sessionID": "ses_1",
                "permission": "bash",
                "patterns": ["rm -rf *"],
                "metadata": {},
                "always": ["rm -rf *"]
            }
        });

        handle_event(
            &permission,
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut state,
        );
        let DriverEvent::Permission {
            request_id,
            options,
            title,
            ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("Supervised mode must surface the request to the user");
        };
        assert_eq!(request_id, "per_abc");
        assert_eq!(title, "rm -rf *");
        assert_eq!(
            options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            ["once", "always", "reject"]
        );
        assert!(command_rx.try_recv().is_err());

        assert_eq!(
            permission_responses(&state.permissions, "per_abc", "always"),
            [("per_abc".into(), "once".into(), Some("ses_1".into()))],
            "provider-wide durable approval must be translated to one-shot"
        );
        let repeated = json!({
            "type": "permission.requested",
            "properties": {
                "id": "per_def",
                "sessionID": "ses_1",
                "permission": "bash",
                "patterns": ["rm -rf /tmp/fintwind-cache"],
                "metadata": {},
                "always": ["rm -rf *"]
            }
        });
        handle_event(
            &repeated, &events, &commands, &turn, 0, "ses_1", false, &mut state,
        );
        let Ok(CommandMessage::Respond { option_id, .. }) = command_rx.try_recv() else {
            panic!("the driver's remembered rule should answer without asking again");
        };
        assert_eq!(option_id, "once");
        assert!(event_rx.try_recv().is_err());

        let mut isolated = OpenCodeStreamState::default();
        handle_event(
            &repeated,
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            false,
            &mut isolated,
        );
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::Permission { request_id, .. } if request_id == "per_def"
        ));
        assert!(
            command_rx.try_recv().is_err(),
            "another driver must not inherit the approval"
        );
    }

    #[test]
    fn auto_modes_use_one_shot_provider_approval() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        handle_event(
            &json!({
                "type": "permission.requested",
                "properties": {
                    "id": "per_auto",
                    "sessionID": "ses_1",
                    "permission": "bash",
                    "patterns": ["cargo test"],
                    "always": ["cargo *"]
                }
            }),
            &events,
            &commands,
            &turn,
            0,
            "ses_1",
            true,
            &mut state,
        );
        let Ok(CommandMessage::Respond { option_id, .. }) = command_rx.try_recv() else {
            panic!("auto modes must answer without the user");
        };
        assert_eq!(option_id, "once");
        assert!(event_rx.try_recv().is_err());
        assert!(state.permissions.lock().approved.is_empty());
    }

    #[test]
    fn always_without_provider_rules_does_not_broaden_future_access() {
        let permissions = Mutex::new(OpenCodePermissionState::default());
        permissions.lock().pending.insert(
            "per_once".into(),
            OpenCodePermissionRequest {
                permission: "bash".into(),
                patterns: vec!["cargo test".into()],
                always: Vec::new(),
                session_id: None,
            },
        );

        assert_eq!(
            permission_responses(&permissions, "per_once", "always"),
            [("per_once".into(), "once".into(), None)]
        );
        assert!(permissions.lock().approved.is_empty());
    }

    #[test]
    fn always_resolves_matching_requests_that_are_already_pending() {
        let permissions = Mutex::new(OpenCodePermissionState::default());
        let request = |patterns: &[&str]| OpenCodePermissionRequest {
            permission: "bash".into(),
            patterns: patterns.iter().map(|pattern| (*pattern).into()).collect(),
            always: vec!["cargo *".into()],
            session_id: None,
        };
        permissions
            .lock()
            .pending
            .insert("per_first".into(), request(&["cargo test"]));
        permissions
            .lock()
            .pending
            .insert("per_matching".into(), request(&["cargo check"]));
        permissions
            .lock()
            .pending
            .insert("per_other".into(), request(&["git status"]));

        assert_eq!(
            permission_responses(&permissions, "per_first", "always"),
            [
                ("per_first".into(), "once".into(), None),
                ("per_matching".into(), "once".into(), None),
            ]
        );
        let permissions = permissions.lock();
        assert!(!permissions.pending.contains_key("per_matching"));
        assert!(permissions.pending.contains_key("per_other"));
    }
}

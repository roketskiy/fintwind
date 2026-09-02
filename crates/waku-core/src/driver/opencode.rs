//! `opencode2 serve` is OpenCode's real API: one resident process serves
//! every session in a workspace, streams server-sent events, and answers
//! permission requests the user can actually be asked. Waku already started
//! this server for a side-quest — forking a session — while running
//! conversations through one-shot `opencode2 run` invocations; this drives
//! everything through it, pooled per workspace via `opencode_pool` so
//! sessions share the process instead of starting one each. A prompt posted
//! into a busy session is folded into the running turn rather than queued
//! behind it, which is what makes steering a plain post.
//!
//! Routes and payload shapes here were read off a live `opencode2` server's
//! `/api` protocol and event stream, not guessed. The v1 compatibility
//! surface (`/session/...`, `/event` with `properties`) is gone from current
//! releases — `POST /session` answers 405 — so everything below speaks the
//! `/api/*` protocol: prompts post `{text}`, forks take
//! `{boundary:{type:"before"|"through",messageID}}`, messages come back as
//! `{data:[...],cursor}`, and events arrive as `{type,data}` lines on
//! `/api/event`.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::activity;
use crate::driver::{
    DriverControl, DriverEventSender, DriverEventSink, DriverStartOptions, SessionOptions,
};
use crate::model::{
    ActivityKind, DriverEvent, InteractionMode, PermissionOption, ProviderResumeCursor,
    RuntimeMode, UserInputAnswer, UserInputOption, UserInputQuestion,
};
use crate::opencode_pool::PooledServer;
use crate::opencode_session::{
    OpenCodeServer, basic_authorization, encode_path_segment, fork_session_removing_turns_on_server,
};

/// How often the permission poll scans the server's pending requests. The
/// endpoint answers instantly when nothing is pending and opencode2 does not
/// stream permission events, so this cadence bounds how long an approval
/// waits to reach the UI while costing next to nothing when idle.
const PERMISSION_POLL_INTERVAL: Duration = Duration::from_millis(400);

enum CommandMessage {
    Prompt(String),
    Steer(String),
    Cancel,
    Respond {
        request_id: String,
        option_id: String,
    },
    RespondUserInput {
        request_id: String,
        answers: Vec<UserInputAnswer>,
    },
    Shutdown,
}

/// The prompt body both turn starts and steers post; opencode2 keeps the
/// model on the session (set through `/api/session/{id}/model`), so prompts
/// carry only their text. The wire's default delivery (`steer`) folds a
/// prompt posted into a busy session into the running turn, matching v1.
fn prompt_body(text: &str) -> Value {
    json!({"text": text})
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

pub struct OpenCodeDriver {
    // `Drop` releases this lease before waking the worker, guaranteeing that
    // final process teardown runs on the worker rather than the UI thread.
    server: Option<PooledServer>,
    session_id: String,
    commands: Sender<CommandMessage>,
    permissions: Arc<Mutex<OpenCodePermissionState>>,
    event_stream: Arc<OpenCodeEventStreamControl>,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    model: Option<String>,
    computer_use: Option<super::support::HeadlessComputerUseRuntime>,
}

impl OpenCodeDriver {
    pub fn start(options: DriverStartOptions, events: DriverEventSender) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            interaction_mode,
            model,
            reasoning_effort: _,
            service_tier: _,
            context_window: _,
            agent_preset: _,
            computer_use_enabled,
            provider_cursor,
        } = options;
        let resume_session_id = match provider_cursor {
            Some(ProviderResumeCursor::OpenCode { session_id }) => {
                (!session_id.is_empty()).then_some(session_id)
            }
            None => None,
        };

        let computer_use = computer_use_enabled
            .then(|| {
                super::support::HeadlessComputerUseRuntime::start(events.clone())
            })
            .transpose()?;
        // The one-shot path handed Computer Use to OpenCode through the
        // environment; the resident server takes it exactly the same way.
        let environment = computer_use
            .as_ref()
            .map(|runtime| super::support::opencode_computer_use_environment(&runtime.config))
            .unwrap_or_default();
        // Computer Use bakes per-session configuration into the server's
        // environment, so it keeps a dedicated server. Every other session
        // shares the workspace's one resident server — OpenCode hosts many
        // sessions per process, and a second `opencode serve` in the same
        // workspace contends with the live one.
        let server = if computer_use.is_some() {
            PooledServer::dedicated(OpenCodeServer::start_with_env(&binary, &cwd, &environment)?)
        } else {
            crate::opencode_pool::acquire(&binary, &cwd)?
        };

        // Reuse the native session when resuming so the conversation, and the
        // cursor already persisted for it, stay the same.
        let session_id = match resume_session_id {
            Some(session_id) => {
                let path = format!("/api/session/{}", encode_path_segment(&session_id));
                server
                    .request("GET", &path, None)
                    .with_context(|| format!("could not resume OpenCode session `{session_id}`"))?;
                session_id
            }
            None => {
                let created = server
                    .request("POST", "/api/session", Some(&json!({})))
                    .context("could not open an OpenCode session")?;
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

        // opencode2 keeps the model on the session instead of on every
        // prompt. A startup model override switches it once; later switches
        // would ride the same endpoint. The model reference on this wire is
        // `{id, providerID}`, unlike v1's `{providerID, modelID}`.
        if let Some(model) = model.as_ref() {
            if let Some((provider_id, model_id)) = model.split_once('/') {
                let _ = server.request(
                    "POST",
                    &format!("/api/session/{}/model", encode_path_segment(&session_id)),
                    Some(&json!({"model": {"id": model_id, "providerID": provider_id}})),
                );
            }
        }

        let usage_metadata = Arc::new(OpenCodeUsageMetadata::default());
        let previous_usage_path = format!(
            "/api/session/{}/message?limit=20",
            encode_path_segment(&session_id)
        );
        let previous_info = server
            .request("GET", &previous_usage_path, None)
            .ok()
            .and_then(|messages| latest_opencode_usage_message(&messages).cloned());
        if let Some(info) = previous_info.as_ref() {
            if let Some(model) = opencode_message_model_key(info) {
                *usage_metadata.last_model.lock() = Some(model);
            }
            if let Some(tokens) = opencode_message_tokens(info) {
                let _ = events.send(DriverEvent::UsageUpdated {
                    context_tokens: Some(tokens),
                    context_window: None,
                });
            }
        } else if let Some(model) = model.as_ref() {
            *usage_metadata.last_model.lock() = Some(model.clone());
        }

        // `/api/model` can be cold on the first server in a directory. Resolve
        // it off the driver-start path so a slow catalog never delays the
        // transcript or turns an otherwise healthy provider into a 0% meter.
        // The stream records the actual provider/model key in parallel; when
        // the catalog lands, publish the matching window as a separate merge.
        // The thread holds only the port: a handle would delay the pooled
        // server's teardown behind this request's timeout.
        let metadata_port = server.port;
        let metadata_events = events.clone();
        let background_usage_metadata = usage_metadata.clone();
        thread::Builder::new()
            .name("waku-opencode-usage-metadata".into())
            .spawn(move || {
                // `/api/model` answers with an empty catalog until the server
                // warms it up. The first session of a workspace starts a cold
                // server, so poll until models land or the budget runs out;
                // later sessions share an already-warm server.
                let started = std::time::Instant::now();
                let budget = Duration::from_secs(30);
                let response = loop {
                    let request = crate::opencode_session::request_json_on_port(
                        metadata_port,
                        "GET",
                        "/api/model",
                        None,
                        Duration::from_secs(30),
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
                let window = background_usage_metadata.current_context_window();
                if let Some(window) = window {
                    let _ = metadata_events.send(DriverEvent::UsageUpdated {
                        context_tokens: None,
                        context_window: Some(window),
                    });
                }
            })?;

        let auto_approve = mode != RuntimeMode::Ask;
        let (commands, command_rx) = unbounded();
        let turn_active = Arc::new(Mutex::new(false));
        let permissions = Arc::new(Mutex::new(OpenCodePermissionState::default()));
        let forms = Arc::new(Mutex::new(OpenCodeFormState::default()));
        let event_stream = Arc::new(OpenCodeEventStreamControl::default());

        // opencode2 answers permission requests through a polling endpoint
        // (`GET /api/permission/request`) instead of the event stream v1
        // used, so a dedicated thread scans it and routes requests through
        // the same approval path the event handler used. The request shape
        // maps straight onto the v1 event payload: `action` is the
        // permission, `resources` the patterns, `save` the always-rules.
        let permission_port = server.port;
        let permission_session = session_id.clone();
        let permission_events = events.clone();
        let permission_commands = commands.clone();
        let permission_state = Arc::clone(&permissions);
        let poll_forms = Arc::clone(&forms);
        let permission_stream = Arc::clone(&event_stream);
        let permission_seen = Arc::new(Mutex::new(HashSet::new()));
        let poll_permission_seen = Arc::clone(&permission_seen);
        thread::Builder::new()
            .name("waku-opencode-permissions".into())
            .spawn(move || {
                while !permission_stream.is_cancelled() {
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
                            let Some(request_id) = request.get("id").and_then(Value::as_str) else {
                                continue;
                            };
                            if request.get("sessionID").and_then(Value::as_str)
                                != Some(permission_session.as_str())
                            {
                                continue;
                            }
                            if !poll_permission_seen.lock().insert(request_id.to_owned()) {
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
                    // `announced` set.
                    if let Ok(pending) = crate::opencode_session::request_json_on_port(
                        permission_port,
                        "GET",
                        "/api/form/request",
                        None,
                        Duration::from_secs(2),
                    ) {
                        for form in pending
                            .get("data")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            if form.get("sessionID").and_then(Value::as_str)
                                != Some(permission_session.as_str())
                            {
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

        // The reader holds only the port, never a server handle: the stream
        // closes exactly when the process exits, so a handle held here would
        // keep the pooled server from ever being killed.
        let stream_port = server.port;
        let stream_session = session_id.clone();
        let stream_events = events.clone();
        let stream_commands = commands.clone();
        let stream_turn = turn_active.clone();
        let stream_usage_metadata = usage_metadata;
        let stream_permissions = Arc::clone(&permissions);
        let stream_forms = Arc::clone(&forms);
        let stream_control = Arc::clone(&event_stream);
        thread::Builder::new()
            .name("waku-opencode-events".into())
            .spawn(move || {
                let mut state = OpenCodeStreamState {
                    usage_metadata: stream_usage_metadata,
                    permissions: stream_permissions,
                    forms: stream_forms,
                    ..OpenCodeStreamState::default()
                };
                match open_event_stream(stream_port, "/api/event", &stream_control) {
                    Ok(Some(stream)) => {
                        for line in BufReader::new(stream).lines().map_while(Result::ok) {
                            if stream_control.is_cancelled() {
                                break;
                            }
                            let Some(payload) = line.strip_prefix("data:") else {
                                continue;
                            };
                            let Ok(value) = serde_json::from_str::<Value>(payload.trim()) else {
                                continue;
                            };
                            // Another session's traffic must not reach this
                            // task's transcript — except session lifecycle
                            // news, which the app needs to keep its sidebar
                            // in sync with the server's whole session list.
                            let lifecycle = matches!(
                                value.get("type").and_then(Value::as_str),
                                Some("session.created" | "session.updated" | "session.deleted")
                            );
                            let session = value
                                .pointer("/data/sessionID")
                                .or_else(|| value.pointer("/properties/sessionID"))
                                // `form.created` nests the session under the
                                // form object.
                                .or_else(|| value.pointer("/data/form/sessionID"))
                                .and_then(Value::as_str);
                            if !lifecycle && session.is_some_and(|session| session != stream_session)
                            {
                                continue;
                            }
                            handle_event(
                                &value,
                                &stream_events,
                                &stream_commands,
                                &stream_turn,
                                auto_approve,
                                &mut state,
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if !stream_control.is_cancelled() {
                            let _ = stream_events.send(DriverEvent::Error(tr!(
                                "errors.read_provider_event_stream",
                                provider = "OpenCode",
                                error = error
                            )));
                        }
                    }
                }
                stream_control.clear();
                if !stream_control.is_cancelled() {
                    let _ = stream_events.send(DriverEvent::ProcessExited);
                }
            })?;

        let worker_server = server.clone();
        let worker_session = session_id.clone();
        let worker_events = events;
        let worker_turn = turn_active;
        let worker_permission_seen = Arc::clone(&permission_seen);
        let worker_forms = Arc::clone(&forms);
        thread::Builder::new()
            .name("waku-opencode-driver".into())
            .spawn(move || {
                while let Ok(message) = command_rx.recv() {
                    match message {
                        CommandMessage::Prompt(text) => {
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
                            let body = prompt_body(&text);
                            if let Err(error) = worker_server.request("POST", &path, Some(&body)) {
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
                        CommandMessage::Steer(text) => {
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
                            let body = prompt_body(&text);
                            match worker_server.request("POST", &path, Some(&body)) {
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
                        } => {
                            let path = format!(
                                "/api/session/{}/permission/{}/reply",
                                encode_path_segment(&worker_session),
                                encode_path_segment(&request_id)
                            );
                            if let Err(error) = worker_server.request(
                                "POST",
                                &path,
                                Some(&json!({"reply": option_id})),
                            ) {
                                worker_permission_seen.lock().remove(&request_id);
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
                        } => {
                            // Current opencode2 routes the question tool's
                            // answers through the form that carried the
                            // prompt; the dedicated question route stays for
                            // releases that still publish `question.asked`.
                            let (path, body) = if request_id.starts_with("frm_") {
                                let fields = worker_forms
                                    .lock()
                                    .fields
                                    .get(&request_id)
                                    .cloned()
                                    .unwrap_or_default();
                                (
                                    format!(
                                        "/api/session/{}/form/{}/reply",
                                        encode_path_segment(&worker_session),
                                        encode_path_segment(&request_id)
                                    ),
                                    json!({"answer": form_reply_answer(&fields, &answers)}),
                                )
                            } else {
                                (
                                    format!(
                                        "/api/question/{}/reply",
                                        encode_path_segment(&request_id)
                                    ),
                                    json!({
                                        "answers": answers
                                            .iter()
                                            .map(|answer| json!(answer.answers))
                                            .collect::<Vec<_>>()
                                    }),
                                )
                            };
                            match worker_server.request("POST", &path, Some(&body)) {
                                Ok(_) => {
                                    if request_id.starts_with("frm_") {
                                        worker_forms.lock().fields.remove(&request_id);
                                    }
                                }
                                Err(error) => {
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
                event_stream.cancel();
            })?;

        Ok(Self {
            server: Some(server),
            session_id,
            commands,
            permissions,
            event_stream,
            mode,
            interaction_mode,
            model,
            computer_use,
        })
    }
}

impl DriverControl for OpenCodeDriver {
    fn prompt(&self, prompt: String) {
        let _ = self.commands.send(CommandMessage::Prompt(prompt));
    }

    fn supports_steer(&self) -> bool {
        true
    }

    fn steer(&self, prompt: String) {
        let _ = self.commands.send(CommandMessage::Steer(prompt));
    }

    fn cancel(&self) {
        let _ = self.commands.send(CommandMessage::Cancel);
    }

    fn cancel_computer_use(&self) {
        if let Some(computer_use) = self.computer_use.as_ref() {
            computer_use.stop();
        }
    }

    fn respond(&self, request_id: String, option_id: String) {
        for (request_id, option_id) in
            permission_responses(&self.permissions, &request_id, &option_id)
        {
            let _ = self.commands.send(CommandMessage::Respond {
                request_id,
                option_id,
            });
        }
    }

    fn respond_user_input(&self, request_id: String, answers: Vec<UserInputAnswer>) {
        let _ = self.commands.send(CommandMessage::RespondUserInput {
            request_id,
            answers,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        // The agent and model are session-level settings, so either changing
        // one requires a fresh driver to apply the new session configuration.
        options.mode == self.mode
            && options.interaction_mode == self.interaction_mode
            && options.model == self.model
    }

    fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        if turns == 0 {
            return Ok(None);
        }
        self.fork(turns).map(Some)
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
        self.cancel_computer_use();
        self.event_stream.cancel();
        // The worker owns the other server lease. Release the UI-owned lease
        // first, then wake the worker so any final terminate/wait happens there.
        drop(self.server.take());
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

#[derive(Default)]
struct OpenCodeEventStreamControl {
    cancelled: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
}

impl OpenCodeEventStreamControl {
    fn attach(&self, stream: &TcpStream) -> std::io::Result<bool> {
        let socket = stream.try_clone()?;
        let mut active = self.socket.lock();
        if self.cancelled.load(Ordering::Acquire) {
            let _ = socket.shutdown(Shutdown::Both);
            return Ok(false);
        }
        *active = Some(socket);
        Ok(true)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(socket) = self.socket.lock().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    fn clear(&self) {
        self.socket.lock().take();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Opens the server-sent event stream and leaves it open.
///
/// The shared request helper reads a whole response before returning, which a
/// stream never finishes doing.
fn open_event_stream(
    port: u16,
    path: &str,
    control: &OpenCodeEventStreamControl,
) -> anyhow::Result<Option<TcpStream>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("could not connect to OpenCode on local port {port}"))?;
    // Register before reading the response head too. If this driver is dropped
    // while setup is blocked, cancellation can still close the socket and wake
    // the reader even though another pooled session keeps the server alive.
    if !control.attach(&stream)? {
        return Ok(None);
    }
    let mut request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n"
    );
    if let Some(authorization) = basic_authorization(port) {
        request.push_str(&authorization);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    write!(stream, "{request}")?;
    stream.flush()?;
    // Skip the response head; every later line is stream payload.
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(anyhow!("OpenCode closed the event stream during setup"));
        }
        if line.trim().is_empty() {
            break;
        }
    }
    stream.set_read_timeout(None)?;
    Ok(Some(stream))
}

#[derive(Default)]
struct OpenCodeStreamState {
    tools: HashMap<String, (ActivityKind, String)>,
    reasoning_parts: HashSet<String>,
    usage_metadata: Arc<OpenCodeUsageMetadata>,
    permissions: Arc<Mutex<OpenCodePermissionState>>,
    forms: Arc<Mutex<OpenCodeFormState>>,
}

/// Pending question forms and whether they were already announced.
///
/// opencode2 delivers the `question` tool's prompt as a *form*: a
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
}

#[derive(Default)]
struct OpenCodeUsageMetadata {
    model_context_windows: Mutex<HashMap<String, u64>>,
    last_model: Mutex<Option<String>>,
}

#[derive(Clone, Debug)]
struct OpenCodePermissionRequest {
    permission: String,
    patterns: Vec<String>,
    always: Vec<String>,
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
    fn current_context_window(&self) -> Option<u64> {
        let model = self.last_model.lock().clone()?;
        self.model_context_windows.lock().get(&model).copied()
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
                .filter(|window| *window > 0)?;
            Some((format!("{provider}/{id}"), window))
        })
        .collect()
}

/// The token total of an opencode2 usage payload: assistant messages carry
/// `tokens` at the top level and the event stream reports the same shape on
/// `session.usage.updated`. There is no `total`, so input/output plus cache
/// define the meter.
fn opencode_message_tokens(message: &Value) -> Option<u64> {
    let tokens = message.get("tokens")?;
    let total = [
        tokens.get("input"),
        tokens.get("output"),
        tokens.get("reasoning"),
        tokens.pointer("/cache/read"),
        tokens.pointer("/cache/write"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_u64)
    .fold(0_u64, u64::saturating_add);
    (total > 0).then_some(total)
}

/// The model key (`provider/id`) of an opencode2 assistant message or
/// `session.step.started` payload, where `model` is an object.
fn opencode_message_model_key(message: &Value) -> Option<String> {
    let model = message.get("model")?;
    let provider = model.get("providerID").and_then(Value::as_str)?;
    let id = model.get("id").and_then(Value::as_str)?;
    Some(format!("{provider}/{id}"))
}

fn latest_opencode_usage_message(messages: &Value) -> Option<&Value> {
    let data = messages.pointer("/data").and_then(Value::as_array)?;
    // The native endpoint returns the newest message first.
    data.iter().find(|message| {
        message.get("type").and_then(Value::as_str) == Some("assistant")
            && message.get("tokens").is_some()
    })
}

fn handle_event(
    value: &Value,
    events: &impl DriverEventSink,
    commands: &Sender<CommandMessage>,
    turn_active: &Mutex<bool>,
    auto_approve: bool,
    state: &mut OpenCodeStreamState,
) {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // opencode2's `/api/event` payloads carry their fields under `data`; the
    // old v1 compatibility stream (still advertised by some forks) used
    // `properties`, tolerated here at no cost.
    let payload = value
        .get("data")
        .or_else(|| value.get("properties"))
        .unwrap_or(&Value::Null);

    match kind {
        "session.text.delta" => {
            let Some(delta) = payload.get("delta").and_then(Value::as_str) else {
                return;
            };
            if delta.is_empty() {
                return;
            }
            let _ = events.send(DriverEvent::TextDelta(delta.to_owned()));
        }
        "session.reasoning.delta" => {
            let Some(delta) = payload.get("delta").and_then(Value::as_str) else {
                return;
            };
            if delta.is_empty() {
                return;
            }
            let _ = events.send(DriverEvent::ReasoningDelta(delta.to_owned()));
        }
        "session.step.started" => {
            // The step announces the model that will run it; later usage
            // events carry tokens but no model.
            if let Some(model) = opencode_message_model_key(payload) {
                *state.usage_metadata.last_model.lock() = Some(model);
            }
        }
        "session.usage.updated" => {
            // The payload carries tokens but no model; the window comes from
            // the model announced by `session.step.started`.
            let (context_tokens, context_window) = {
                let metadata = &state.usage_metadata;
                let tokens = opencode_message_tokens(payload);
                let window = metadata.current_context_window();
                (tokens, window)
            };
            if context_tokens.is_some() || context_window.is_some() {
                let _ = events.send(DriverEvent::UsageUpdated {
                    context_tokens,
                    context_window,
                });
            }
        }
        "session.execution.succeeded" => {
            state.reasoning_parts.clear();
            state.permissions.lock().pending.clear();
            state.tools.clear();
            if std::mem::take(&mut *turn_active.lock()) {
                let _ = events.send(DriverEvent::TurnFinished {
                    success: true,
                    summary: None,
                });
            }
        }
        "session.execution.failed" => {
            state.reasoning_parts.clear();
            state.permissions.lock().pending.clear();
            state.tools.clear();
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
            }
        }
        "session.tool.called" => {
            tool_called(payload, events, state);
        }
        "session.tool.progress" | "session.tool.input.ended" => {}
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
            request_permission(
                payload,
                events,
                commands,
                auto_approve,
                &state.permissions,
            );
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
                forms.announced.remove(id);
            }
        }
        // `session.text.started`/`ended`, `session.reasoning.started`/`ended`,
        // `session.step.streamed`, `session.inbox.*`, `session.execution.started`,
        // `session.instructions.updated`, `server.connected`, and the
        // heartbeat comment lines are not transcript content.
        _ => {}
    }
}

/// `session.tool.called` opens a tool activity with the arguments the tool
/// will run with; the name was recorded by `session.tool.input.started`.
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
    let display = activity::input_title(arguments);
    let item = activity::tool_activity(
        Some(id.to_owned()),
        kind,
        display.unwrap_or(title),
        arguments,
        None,
        payload.get("input"),
        false,
        false,
    );
    let _ = events.send(DriverEvent::RichActivity(item));
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

/// The `question` tool on current opencode2 publishes its prompt as a form
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
    if form.get("sessionID").and_then(Value::as_str).is_none() {
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
    auto_approve: bool,
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
    let permission_request = OpenCodePermissionRequest {
        permission: request
            .get("permission")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        patterns: request
            .get("patterns")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        always: request
            .get("always")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    };

    // OpenCode's `always` response updates a process-wide approval cache. A
    // pooled Full Access task must never suppress prompts in a Supervised task,
    // so Waku sends only one-shot provider replies and retains durable choices
    // in this driver's session-local state.
    if auto_approve || permissions.lock().is_approved(&permission_request) {
        let _ = commands.send(CommandMessage::Respond {
            request_id: request_id.to_owned(),
            option_id: "once".into(),
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
) -> Vec<(String, String)> {
    let mut permissions = permissions.lock();
    let request = permissions.pending.remove(request_id);
    if option_id != "always" {
        return vec![(request_id.to_owned(), option_id.to_owned())];
    }

    if let Some(request) = request.as_ref() {
        permissions.remember(request);
    }
    // OpenCode normally applies an `always` reply to other matching requests
    // already pending in the same session. Preserve that behavior locally,
    // but send every provider reply as one-shot so the shared server's cache
    // remains untouched.
    let additional = permissions
        .pending
        .iter()
        .filter(|(_, request)| permissions.is_approved(request))
        .map(|(request_id, _)| request_id.clone())
        .collect::<Vec<_>>();
    for request_id in &additional {
        permissions.pending.remove(request_id);
    }

    std::iter::once((request_id.to_owned(), "once".into()))
        .chain(
            additional
                .into_iter()
                .map(|request_id| (request_id, "once".into())),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        request_user_input_from_form(&form, &forms, &events)
            .expect("the first sight must ask");
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
        let shapes = vec![
            ("q0".to_owned(), true),
            ("q1".to_owned(), false),
        ];
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

    /// Drives a real `opencode2 serve` through the actual driver. Ignored by
    /// default: needs the CLI installed, credentials, and the network. Run with
    /// `cargo test --bin waku opencode_session_against_a_real_server -- --ignored`.
    #[test]
    #[ignore = "requires an installed, authenticated opencode2"]
    fn opencode_session_against_a_real_server() {
        let binary =
            crate::command_env::find_executable("opencode2").expect("opencode is not installed");
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
                computer_use_enabled: false,
                provider_cursor: None,
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

        driver.prompt("Reply with exactly: OK. Do not use any tools.".into());
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
                DriverEvent::TextDelta(delta) => text.push_str(&delta),
                DriverEvent::UsageUpdated {
                    context_tokens: tokens,
                    context_window: window,
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

    /// Drives the `question` tool against a real `opencode2`: the prompt
    /// must arrive as a form (`form.created`, not the question events older
    /// docs describe), surface as a structured question request, and the
    /// reply must settle the form and the turn. Ignored by default: needs
    /// the CLI installed with working provider credentials. Run with
    /// `cargo test --bin waku question_form_against_a_real_server -- --ignored`.
    #[test]
    #[ignore = "requires an installed, authenticated opencode2"]
    fn question_form_against_a_real_server() {
        let binary =
            crate::command_env::find_executable("opencode2").expect("opencode is not installed");
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
                computer_use_enabled: false,
                provider_cursor: None,
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
        assert_eq!(finished, Some(true), "the turn should settle after the reply");
    }

    /// Proves steering through the actual driver: the message injected while
    /// the bash tool sleeps lands inside the same turn — one SteerAccepted,
    /// one TurnFinished, and a reply that honors both instructions. Ignored by
    /// default: needs the CLI installed, credentials, and the network.
    #[test]
    #[ignore = "requires an installed, authenticated opencode2"]
    fn opencode_steering_folds_a_mid_turn_message_into_the_running_turn() {
        let binary =
            crate::command_env::find_executable("opencode2").expect("opencode is not installed");
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
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .expect("the server should start and open a session");

        driver.prompt(
            "Use the bash tool to run exactly `sleep 6` (nothing else). \
             After the command completes, reply with exactly: FIRST DONE"
                .into(),
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
                    );
                }
                DriverEvent::SteerAccepted { message } => {
                    assert!(message.contains("BANANA"));
                    steer_accepted = true;
                }
                DriverEvent::SteerRejected { reason, .. } => {
                    panic!("the steer should be accepted, got rejection: {reason}");
                }
                DriverEvent::TextDelta(delta) => text.push_str(&delta),
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
        // Payloads copied from a live `opencode2 serve` event stream.
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
            handle_event(&event, &events, &commands, &turn, true, &mut state);
        }

        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert!(matches!(&seen[0], DriverEvent::TextDelta(text) if text == "OK"));
        assert!(matches!(&seen[1], DriverEvent::ReasoningDelta(text) if text == "thinking"));
        assert!(matches!(&seen[2], DriverEvent::RichActivity(item)
                if item.kind == ActivityKind::FileRead
                    && !item.complete
                    && item.display_target.as_deref() == Some("a.txt")));
        assert!(matches!(&seen[3], DriverEvent::RichActivity(item)
                if item.complete && item.title == "read"));
        assert!(matches!(
            &seen[4],
            DriverEvent::UsageUpdated {
                context_tokens: Some(2),
                context_window: None
            }
        ));
        assert!(matches!(
            &seen[5],
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert_eq!(seen.len(), 6, "non-transcript events leaked");
        assert!(!*turn.lock(), "the turn should be settled exactly once");
    }

    #[test]
    fn v2_reasoning_and_text_flows_classify_by_their_own_events() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        // opencode2 separates the thought and answer streams into their own
        // events, so no part classification is needed.
        let wire = [
            json!({"type":"session.reasoning.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"thinking"}}),
            json!({"type":"session.text.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"answer"}}),
            json!({"type":"session.text.delta","data":{"sessionID":"ses_1","assistantMessageID":"msg_2","ordinal":0,"delta":" tail"}}),
            json!({"type":"session.execution.succeeded","data":{"sessionID":"ses_1"}}),
        ];
        for event in wire {
            handle_event(&event, &events, &commands, &turn, true, &mut state);
        }

        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(matches!(&seen[0], DriverEvent::ReasoningDelta(text) if text == "thinking"));
        assert!(matches!(&seen[1], DriverEvent::TextDelta(text) if text == "answer"));
        assert!(matches!(&seen[2], DriverEvent::TextDelta(text) if text == " tail"));
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

        // The step announces the model; the usage event then carries tokens.
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
                        "cache": {"read": 1792, "write": 0}
                    }
                }
            }),
            &events,
            &commands,
            &turn,
            true,
            &mut state,
        );

        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::UsageUpdated {
                context_tokens: Some(15_201),
                context_window: Some(200_000)
            }
        ));
        assert!(event_rx.try_recv().is_err());
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
        assert_eq!(opencode_message_tokens(latest), Some(500));
        assert_eq!(
            opencode_message_model_key(latest).as_deref(),
            Some("glmcoding/glm-5.3-flash")
        );
        assert_eq!(
            opencode_message_model_key(latest)
                .as_ref()
                .and_then(|model| windows.get(model))
                .copied(),
            Some(1_000_000)
        );
    }

    #[test]
    fn generated_session_titles_replace_the_local_fallback() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();

        // opencode2 emits the final generated title once through `session.renamed`.
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

        handle_event(&permission, &events, &commands, &turn, false, &mut state);
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
            [("per_abc".into(), "once".into())],
            "provider-wide durable approval must be translated to one-shot"
        );
        let repeated = json!({
            "type": "permission.requested",
            "properties": {
                "id": "per_def",
                "sessionID": "ses_1",
                "permission": "bash",
                "patterns": ["rm -rf /tmp/waku-cache"],
                "metadata": {},
                "always": ["rm -rf *"]
            }
        });
        handle_event(&repeated, &events, &commands, &turn, false, &mut state);
        let Ok(CommandMessage::Respond { option_id, .. }) = command_rx.try_recv() else {
            panic!("the driver's remembered rule should answer without asking again");
        };
        assert_eq!(option_id, "once");
        assert!(event_rx.try_recv().is_err());

        let mut isolated = OpenCodeStreamState::default();
        handle_event(&repeated, &events, &commands, &turn, false, &mut isolated);
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
            },
        );

        assert_eq!(
            permission_responses(&permissions, "per_once", "always"),
            [("per_once".into(), "once".into())]
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
                ("per_first".into(), "once".into()),
                ("per_matching".into(), "once".into()),
            ]
        );
        let permissions = permissions.lock();
        assert!(!permissions.pending.contains_key("per_matching"));
        assert!(permissions.pending.contains_key("per_other"));
    }

    #[test]
    fn cancelling_event_stream_unblocks_response_setup() {
        use std::net::TcpListener;
        use std::sync::mpsc;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let control = Arc::new(OpenCodeEventStreamControl::default());
        let reader_control = Arc::clone(&control);
        let (done, finished) = mpsc::channel();
        let reader = thread::spawn(move || {
            let _ = open_event_stream(port, "/event", &reader_control);
            done.send(()).unwrap();
        });
        let (_peer, _) = listener.accept().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while control.socket.lock().is_none() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(control.socket.lock().is_some());

        control.cancel();
        finished
            .recv_timeout(Duration::from_secs(1))
            .expect("cancellation should unblock the response-head read");
        reader.join().unwrap();
        assert!(control.is_cancelled());
        assert!(control.socket.lock().is_none());
    }
}

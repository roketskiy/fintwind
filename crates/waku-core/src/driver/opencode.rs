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

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    ActivityItem, ActivityKind, BackgroundWorkEvent, BackgroundWorkItem, BackgroundWorkKey,
    BackgroundWorkKind, BackgroundWorkStatus, BackgroundWorkTranscriptEvent, DriverEvent,
    InteractionMode, PermissionOption, ProviderResumeCursor, RuntimeMode, UserInputAnswer,
    UserInputOption, UserInputQuestion, unix_time_millis,
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
    events: DriverEventSender,
    background_refresh_generation: Arc<AtomicU64>,
    background_transcript_hydrations: Arc<Mutex<HashSet<String>>>,
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
            .then(|| super::support::HeadlessComputerUseRuntime::start(events.clone()))
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
        let stream_event_sink = stream_events.clone();
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
                            let session = event_session_id(&value);
                            let is_child = session
                                .filter(|session| *session != stream_session)
                                .is_some_and(|session| {
                                    state.children.get(session).is_some_and(|child| {
                                        child.item.parent_id.as_deref()
                                            == Some(stream_session.as_str())
                                    })
                                });
                            if is_child {
                                handle_child_event(
                                    &value,
                                    &stream_session,
                                    &stream_event_sink,
                                    &stream_commands,
                                    mode == RuntimeMode::FullAccess,
                                    &mut state,
                                );
                                continue;
                            }
                            // Lifecycle events can announce a new child before
                            // its session id is otherwise known. Route only
                            // sessions whose payload explicitly points at this
                            // foreground session; unrelated runtime traffic is
                            // discarded.
                            if matches!(
                                value.get("type").and_then(Value::as_str),
                                Some("session.created" | "session.updated")
                            ) && child_parent_id(&value).as_deref()
                                == Some(stream_session.as_str())
                            {
                                handle_child_event(
                                    &value,
                                    &stream_session,
                                    &stream_event_sink,
                                    &stream_commands,
                                    mode == RuntimeMode::FullAccess,
                                    &mut state,
                                );
                                continue;
                            }
                            let lifecycle = matches!(
                                value.get("type").and_then(Value::as_str),
                                Some("session.created" | "session.updated" | "session.deleted")
                            );
                            if session.is_some_and(|session| session != stream_session) {
                                // Other clients share this server; their
                                // sessions never touch this transcript, but
                                // the sidebar still reconciles against the
                                // server's roster when one appears or goes.
                                if lifecycle {
                                    let _ =
                                        stream_event_sink.send(DriverEvent::NativeSessionsChanged);
                                }
                                continue;
                            }
                            if !lifecycle && session.is_none() {
                                continue;
                            }
                            handle_event(
                                &value,
                                &stream_event_sink,
                                &stream_commands,
                                &stream_turn,
                                mode == RuntimeMode::FullAccess,
                                &mut state,
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if !stream_control.is_cancelled() {
                            let _ = stream_event_sink.send(DriverEvent::Error(tr!(
                                "errors.read_provider_event_stream",
                                provider = "OpenCode",
                                error = error
                            )));
                        }
                    }
                }
                stream_control.clear();
                if !stream_control.is_cancelled() {
                    let _ = stream_event_sink.send(DriverEvent::ProcessExited);
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
            events: stream_events,
            background_refresh_generation: Arc::new(AtomicU64::new(0)),
            background_transcript_hydrations: Arc::new(Mutex::new(HashSet::new())),
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

    fn refresh_background_work(&self) {
        let Some(server) = self.server.as_ref() else {
            return;
        };
        let server = server.clone();
        let generation = self
            .background_refresh_generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let port = server.port;
        let parent_id = self.session_id.clone();
        let events = self.events.clone();
        let generation_guard = Arc::clone(&self.background_refresh_generation);
        let transcript_hydrations = Arc::clone(&self.background_transcript_hydrations);
        let _ = thread::Builder::new()
            .name("waku-opencode-subagents-refresh".into())
            .spawn(move || {
                let path = "/api/session?limit=200";
                let response = crate::opencode_session::request_json_on_port(
                    port,
                    "GET",
                    path,
                    None,
                    Duration::from_secs(10),
                );
                if generation_guard.load(Ordering::Acquire) != generation {
                    return;
                }
                // A failed probe is not evidence that the children are gone:
                // sending an empty reconcile would mark live subagents Lost
                // until the next poll. Skip this round and keep the
                // event-driven state.
                let Some(sessions) = response
                    .ok()
                    .and_then(|value| value.get("data").and_then(Value::as_array).cloned())
                else {
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
                    match super::native::fetch_transcript(&server, &child_id) {
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
            .name("waku-opencode-subagent-stop".into())
            .spawn(move || {
                let path = format!(
                    "/api/session/{}/interrupt",
                    encode_path_segment(&control_id)
                );
                if let Err(error) = crate::opencode_session::request_json_on_port(
                    port,
                    "POST",
                    &path,
                    None,
                    Duration::from_secs(10),
                ) {
                    let _ = events.send(DriverEvent::BackgroundWork(
                        BackgroundWorkEvent::StopFailed {
                            key,
                            message: error.to_string(),
                        },
                    ));
                }
            });
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
    pending_subagents: VecDeque<(String, Option<String>)>,
    reasoning_parts: HashSet<String>,
    children: HashMap<String, OpenCodeChildSession>,
    usage_metadata: Arc<OpenCodeUsageMetadata>,
    permissions: Arc<Mutex<OpenCodePermissionState>>,
    forms: Arc<Mutex<OpenCodeFormState>>,
}

struct OpenCodeChildSession {
    item: BackgroundWorkItem,
    prompt: Option<String>,
    tools: HashMap<String, (ActivityKind, String)>,
    /// When the child's own execution began; the session row can exist
    /// (and be listed) noticeably earlier.
    execution_started_at_ms: Option<u64>,
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
        }
    }
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

/// The context size carried by an opencode2 assistant message: `tokens` on
/// the message info (and on `session.step.ended`) is the normalized shape —
/// `input` excludes cached tokens, `output` excludes reasoning, so the
/// disjoint fields sum to the context, and `total` reports the same number
/// outright when the provider sent it.
fn opencode_message_tokens(message: &Value) -> Option<u64> {
    let tokens = message.get("tokens")?;
    if let Some(total) = tokens.get("total").and_then(Value::as_u64).filter(|total| *total > 0) {
        return Some(total);
    }
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

/// The context size carried by a `session.usage.updated` payload. Unlike the
/// message shape above, this is the raw provider usage: AI SDK v6 normalizes
/// every provider's `inputTokens` to include cached tokens, and `outputTokens`
/// to include reasoning, so the cache and reasoning fields are subsets of
/// `input`/`output` rather than additions. Summing all five — as if normalized
/// — double-counts the cache and reads roughly twice the real context on a
/// cache-heavy turn. `total`, when present, already equals `input + output`.
fn opencode_session_usage_tokens(payload: &Value) -> Option<u64> {
    let tokens = payload.get("tokens")?;
    if let Some(total) = tokens.get("total").and_then(Value::as_u64).filter(|total| *total > 0) {
        return Some(total);
    }
    let total = [tokens.get("input"), tokens.get("output")]
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

fn handle_child_event(
    value: &Value,
    parent_id: &str,
    events: &impl DriverEventSink,
    commands: &Sender<CommandMessage>,
    auto_approve: bool,
    state: &mut OpenCodeStreamState,
) {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = event_payload(value);
    let Some(session_id) = event_session_id(value).map(str::to_owned) else {
        return;
    };
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
        if is_new && let Some((activity_id, prompt)) = state.pending_subagents.pop_front() {
            child.item.origin_activity_id = Some(activity_id);
            child.prompt = child.prompt.clone().or(prompt);
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
        child_update(child, events, None, None);
        let _ = events.send(DriverEvent::BackgroundWork(
            BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Started { key, prompt }),
        ));
        return;
    }

    let Some(child) = state.children.get_mut(&session_id) else {
        return;
    };
    // A settled child's stragglers on the event stream must not reopen its
    // transcript; only metadata updates still apply.
    if !child.item.status.is_live() && kind != "session.updated" {
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
            child.execution_started_at_ms = Some(unix_time_millis());
            child_update(child, events, Some(BackgroundWorkStatus::Running), None);
            let _ = events.send(DriverEvent::BackgroundWork(
                BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Started {
                    key,
                    prompt: child.prompt.clone(),
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
                    activity::input_title(arguments).unwrap_or(title),
                    arguments,
                    None,
                    payload.get("input"),
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
        // A subagent can request permissions or ask the user questions
        // exactly like the foreground session; the request ids are global,
        // so the parent's reply plumbing answers them unchanged. Without
        // this passthrough the child would stall on a prompt nobody sees.
        _ if kind.starts_with("permission.") => {
            request_permission(payload, events, commands, auto_approve, &state.permissions);
        }
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
        _ => {}
    }
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
            // The payload carries raw provider usage (its `input` already
            // includes cached tokens) but no model; the window comes from the
            // model announced by `session.step.started`.
            let (context_tokens, context_window) = {
                let metadata = &state.usage_metadata;
                let tokens = opencode_session_usage_tokens(payload);
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
            state.pending_subagents.clear();
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
            state.pending_subagents.clear();
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
    if stored
        .as_ref()
        .is_some_and(|(_, name)| is_subagent_tool(name))
    {
        state
            .pending_subagents
            .push_back((id.to_owned(), subagent_prompt(arguments)));
    }
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
    if failed
        && stored
            .as_ref()
            .is_some_and(|(_, name)| is_subagent_tool(name))
    {
        state
            .pending_subagents
            .retain(|(activity_id, _)| activity_id != id);
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
                if item.origin_activity_id.as_deref() == Some("call_task")
        )));
        assert!(emitted.iter().any(|event| matches!(
            event,
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(
                BackgroundWorkTranscriptEvent::Started { prompt, .. }
            )) if prompt.as_deref() == Some("Inspect the repository")
        )));
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
                    "patterns": ["rm -rf /tmp/waku-cache"]
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

        // Auto mode: the same ask is answered one-shot without prompting.
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
        let Ok(CommandMessage::Respond { request_id, .. }) = command_rx.try_recv() else {
            panic!("auto mode must answer a child permission ask");
        };
        assert_eq!(request_id, "per_child_2");
        assert!(event_rx.try_recv().is_err());
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

        // The step announces the model; the usage event then carries the raw
        // provider usage, whose `input` already includes the cached tokens.
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
                        "cache": {"read": 1_792, "write": 0}
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
                context_tokens: Some(13_409),
                context_window: Some(200_000)
            }
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn session_usage_events_do_not_double_count_the_cache() {
        // Shape captured live from opencode2: the same turn whose assistant
        // message reported `{input: 205, cache.read: 8_704, output: 3}` — a
        // ~8.9k context — published this raw usage payload. Summing all five
        // fields reads 19_339, about twice the real context; the raw shape
        // totals `input + output` because its cache entry is already inside
        // `input`.
        let payload = json!({
            "sessionID": "ses_1",
            "cost": 0.002_190_308,
            "tokens": {
                "input": 9_598,
                "output": 37,
                "reasoning": 0,
                "cache": {"read": 8_704, "write": 0}
            }
        });
        assert_eq!(opencode_session_usage_tokens(&payload), Some(9_635));

        // A `total`, when the provider reports one, is the context outright.
        let payload = json!({"tokens": {
            "total": 500,
            "input": 9_598,
            "output": 37,
            "reasoning": 0,
            "cache": {"read": 8_704, "write": 0}
        }});
        assert_eq!(opencode_session_usage_tokens(&payload), Some(500));
    }

    #[test]
    fn message_tokens_sum_disjoint_fields_and_honor_total() {
        // The normalized message shape: cache and reasoning are separate from
        // input/output, so the disjoint fields sum to the context.
        let message = json!({"tokens": {
            "input": 246,
            "output": 83,
            "reasoning": 0,
            "cache": {"read": 78_592, "write": 0}
        }});
        assert_eq!(opencode_message_tokens(&message), Some(78_921));

        // `total`, when present, is that same context reported outright.
        let message = json!({"tokens": {
            "total": 78_921,
            "input": 300,
            "output": 83,
            "reasoning": 0,
            "cache": {"read": 78_592, "write": 0}
        }});
        assert_eq!(opencode_message_tokens(&message), Some(78_921));
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

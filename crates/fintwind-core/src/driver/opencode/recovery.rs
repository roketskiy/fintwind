//! Reconcile a quiet live feed with durable turn boundaries, never with a
//! completed model step or the absence of process-local execution.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use crossbeam_channel::{Receiver, Sender, bounded};
use parking_lot::Mutex;
use serde_json::Value;

use crate::model::unix_time_millis;
use crate::opencode_events::EventFeed;
use crate::opencode_session::{encode_path_segment, request_json_on_port_bounded};
use fintwind_protocol::provider_session::NativeTranscript;

const QUIET_INTERVAL: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const WALK_BUDGET: Duration = Duration::from_secs(12);
const PAGE_LIMIT: usize = 200;
const MAX_PAGES: usize = 25;
const MAX_RECOVERY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
struct Boundary {
    generation: u64,
    started_at: u64,
    input_id: Option<String>,
    latest_input_id: Option<String>,
    unconfirmed_steers: Vec<(String, String)>,
    accepted: bool,
    server_initiated: bool,
    execution_id: Option<String>,
}

/// Shared only with the command worker. This is a submission fence, not an
/// execution lock: an unmodified external OpenCode process does not honor it.
#[derive(Default)]
pub(super) struct SubmissionFence {
    generation: u64,
    boundary: Option<Boundary>,
    durable_receipt: Option<(uuid::Uuid, Arc<crate::persistence::StateStore>)>,
}

pub(super) struct Steering {
    pub(super) generation: u64,
    pub(super) input_id: String,
    previous: Boundary,
}

fn input_id(timestamp: u64) -> String {
    fintwind_protocol::submission::new_input_id(timestamp)
}

impl SubmissionFence {
    pub(super) fn begin(&mut self) -> u64 {
        let started_at = unix_time_millis();
        let input_id = input_id(started_at);
        self.begin_with_input(input_id, started_at)
    }

    pub(super) fn begin_with_input(&mut self, input_id: String, started_at: u64) -> u64 {
        self.durable_receipt = None;
        self.generation = self.generation.wrapping_add(1);
        self.boundary = Some(Boundary {
            generation: self.generation,
            started_at,
            input_id: Some(input_id.clone()),
            latest_input_id: Some(input_id),
            unconfirmed_steers: Vec::new(),
            accepted: false,
            server_initiated: false,
            execution_id: None,
        });
        self.generation
    }

    pub(super) fn begin_steer(&mut self) -> Option<Steering> {
        let previous = self.boundary.clone()?;
        self.generation = self.generation.wrapping_add(1);
        let input_id = input_id(unix_time_millis());
        let boundary = self.boundary.as_mut()?;
        boundary.generation = self.generation;
        boundary.latest_input_id = Some(input_id.clone());
        boundary.accepted = false;
        Some(Steering {
            generation: self.generation,
            input_id,
            previous,
        })
    }

    pub(super) fn reject_steer(&mut self, mut steering: Steering) {
        if self
            .boundary
            .as_ref()
            .is_some_and(|boundary| boundary.generation == steering.generation)
        {
            steering.previous.generation = steering.generation;
            self.boundary = Some(steering.previous);
        }
    }

    pub(super) fn unconfirmed_steer(&mut self, steering: Steering, text: String) {
        self.unconfirmed(steering.generation);
        if let Some(boundary) = self.boundary.as_mut()
            && boundary.generation == steering.generation
        {
            boundary.unconfirmed_steers.push((steering.input_id, text));
        }
    }

    pub(super) fn input_id(&self) -> Option<String> {
        self.boundary
            .as_ref()
            .and_then(|boundary| boundary.input_id.clone())
    }

    pub(super) fn accepted(&mut self, generation: u64, response: &Value) {
        if let Some(boundary) = self.boundary.as_mut()
            && boundary.generation == generation
        {
            boundary.accepted = true;
            let info = response.get("data").unwrap_or(response);
            let first = boundary.input_id == boundary.latest_input_id;
            if let Some(id) = info.get("id").and_then(Value::as_str) {
                if first {
                    boundary.input_id = Some(id.to_owned());
                }
                boundary.latest_input_id = Some(id.to_owned());
            }
            if first && let Some(created) = info.pointer("/time/created").and_then(Value::as_u64) {
                boundary.started_at = created;
            }
        }
    }

    pub(super) fn unconfirmed(&mut self, generation: u64) {
        if let Some(boundary) = self.boundary.as_mut()
            && boundary.generation == generation
        {
            // A lost acknowledgement is not a rejection. Search for this
            // input in durable history; never resubmit automatically.
            boundary.accepted = true;
        }
    }

    fn server_started(&mut self, event: &Value) {
        if self.boundary.is_none() {
            self.begin();
            if let Some(boundary) = self.boundary.as_mut() {
                // Never use the time this consumer processed a buffered event.
                let native_started = event.get("created").and_then(Value::as_u64);
                let execution_id = event
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| id.strip_prefix("evt_"))
                    .map(|id| format!("msg_{id}"));
                boundary.accepted = native_started.is_some() && execution_id.is_some();
                boundary.started_at = native_started.unwrap_or_default();
                boundary.execution_id = execution_id;
                boundary.server_initiated = true;
                boundary.input_id = None;
                boundary.latest_input_id = None;
            }
        }
    }

    pub(super) fn matches(&self, generation: u64) -> bool {
        self.boundary
            .as_ref()
            .is_some_and(|boundary| boundary.generation == generation && boundary.accepted)
    }

    pub(super) fn clear(&mut self) {
        self.boundary = None;
        self.durable_receipt = None;
    }

    pub(super) fn bind_receipt(
        &mut self,
        id: uuid::Uuid,
        store: Arc<crate::persistence::StateStore>,
    ) {
        self.durable_receipt = Some((id, store));
    }

    /// Called only after the durable history walk proved this exact input.
    pub(super) fn confirm_receipt(&self, events: &impl crate::driver::DriverEventSink, port: u16) {
        use fintwind_protocol::submission::SubmissionState;
        let Some((id, store)) = &self.durable_receipt else {
            return;
        };
        let result = (|| -> std::io::Result<()> {
            let Some(saved) = store.submission(*id)? else {
                return Ok(());
            };
            if saved.receipt.state.is_unconfirmed()
                && let Some(saved) = store.transition_submission(
                    *id,
                    saved.receipt.state,
                    SubmissionState::Accepted,
                    None,
                )?
            {
                crate::opencode_diagnostics::submission(
                    "receipt_reconciled",
                    &saved,
                    port,
                    self.generation,
                );
                let _ = events.send(crate::model::DriverEvent::SubmissionUpdated(saved.receipt));
            }
            Ok(())
        })();
        if result.is_err() {
            crate::opencode_diagnostics::record(
                "receipt_reconcile_save_failed",
                port,
                self.generation,
            );
        }
    }
}

struct Probe {
    boundary: Boundary,
    revision: u64,
    connection: u64,
}

struct ProbeResult {
    probe: Probe,
    result: anyhow::Result<Option<RecoveredTurn>>,
}

pub(super) struct RecoveredTurn {
    pub(super) generation: u64,
    pub(super) transcript: NativeTranscript,
    pub(super) continuation: bool,
    pub(super) success: bool,
    pub(super) error: Option<String>,
    pub(super) acknowledged_steers: Vec<String>,
    pub(super) completion: Completion,
}

pub(super) struct Completion {
    created: u64,
    message_id: Option<String>,
}

/// One bounded, off-reader query at a time. Results are applied only by the
/// event consumer after newer queued events have been drained.
pub(super) struct Coordinator {
    pub(super) submissions: Arc<Mutex<SubmissionFence>>,
    requests: Sender<Probe>,
    results: Receiver<ProbeResult>,
    revision: u64,
    connection: u64,
    last_progress: Instant,
    last_probe: Instant,
    pending: bool,
    reconnect_due: bool,
    needs_history: bool,
    terminal_error: Option<(u64, String)>,
    completed: Option<Completion>,
    port: u16,
}

impl Coordinator {
    pub(super) fn new(port: u16, session: String, feed: Arc<EventFeed>) -> anyhow::Result<Self> {
        let (requests, queries) = bounded::<Probe>(1);
        let (replies, results) = bounded(1);
        thread::Builder::new()
            .name("fintwind-opencode-reconcile".into())
            .spawn(move || {
                while let Ok(probe) = queries.recv() {
                    if feed.is_cancelled() {
                        break;
                    }
                    let result =
                        fetch_turn(port, &session, &probe.boundary, || feed.is_cancelled());
                    if replies.send(ProbeResult { probe, result }).is_err() {
                        break;
                    }
                }
            })
            .context("could not start OpenCode reconciliation worker")?;
        Ok(Self {
            submissions: Arc::new(Mutex::new(SubmissionFence::default())),
            requests,
            results,
            revision: 0,
            connection: 0,
            last_progress: Instant::now(),
            last_probe: Instant::now(),
            pending: false,
            reconnect_due: false,
            needs_history: false,
            terminal_error: None,
            completed: None,
            port,
        })
    }

    pub(super) fn mark_completed(&mut self, completion: Completion) {
        self.completed = Some(completion);
    }

    /// A durable replacement is authoritative for events at/before its idle.
    /// Buffered text/tool/start events from that execution must not reopen or
    /// contaminate a later prompt. Use the native envelope, never receive time.
    pub(super) fn is_completed_event(&self, event: &Value, session: &str) -> bool {
        if super::event_session_id(event) != Some(session) {
            return false;
        }
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if ![
            "session.text.",
            "session.reasoning.",
            "session.tool.",
            "session.step.",
            "session.execution.",
        ]
        .iter()
        .any(|prefix| kind.starts_with(prefix))
        {
            return false;
        }
        let Some(completed) = &self.completed else {
            return false;
        };
        let Some(created) = event.get("created").and_then(Value::as_u64) else {
            return false;
        };
        created < completed.created
            || (created == completed.created
                && event
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| id.strip_prefix("evt_"))
                    .zip(
                        completed
                            .message_id
                            .as_deref()
                            .and_then(|id| id.strip_prefix("msg_")),
                    )
                    .is_some_and(|(event, idle)| event <= idle))
    }

    pub(super) fn observe(&mut self, event: &Value, session: &str, active: bool) {
        if super::event_session_id(event) != Some(session) {
            return;
        }
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(kind, "session.updated" | "session.renamed") {
            // Sidebar/metadata churn must not postpone a lost-turn probe or
            // invalidate a result containing unchanged execution facts.
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        self.last_progress = Instant::now();
        if kind == "session.execution.started" {
            self.submissions.lock().server_started(event);
        } else if !active {
            self.submissions.lock().clear();
        }
    }

    pub(super) fn observe_connection(&mut self, feed: &EventFeed, active: bool) {
        let connection = feed.connection_generation();
        if connection != self.connection {
            // A terminal arriving on the new live-only connection cannot
            // prove that we received the text and tools preceding it.
            self.needs_history |= self.connection != 0 || active;
            self.reconnect_due = self.needs_history;
            self.connection = connection;
        }
    }

    pub(super) fn defer_terminal(&mut self, event: &Value, session: &str) -> bool {
        let legacy_idle = event.get("type").and_then(Value::as_str) == Some("session.idle")
            || (event.get("type").and_then(Value::as_str) == Some("session.status")
                && event.pointer("/data/status/type").and_then(Value::as_str) == Some("idle"));
        let terminal = matches!(
            event.get("type").and_then(Value::as_str),
            Some(
                "session.execution.succeeded"
                    | "session.execution.failed"
                    | "session.execution.interrupted"
            )
        );
        // A successful HTTP ack admits an inbox item, not its delivery. The
        // latest input must be durable before any execution terminal settles.
        if (legacy_idle || terminal) && super::event_session_id(event) == Some(session) {
            self.reconnect_due = true;
            if let Some(message) = event.pointer("/data/error/message").and_then(Value::as_str)
                && let Some(boundary) = self.submissions.lock().boundary.as_ref()
                && event
                    .get("created")
                    .and_then(Value::as_u64)
                    .is_some_and(|created| created >= boundary.started_at)
            {
                self.terminal_error = Some((boundary.generation, message.to_owned()));
            }
            return true;
        }
        false
    }

    pub(super) fn poll(
        &mut self,
        feed: &EventFeed,
        active: bool,
        caught_up: bool,
    ) -> Option<RecoveredTurn> {
        self.observe_connection(feed, active);
        let connection = self.connection;
        if caught_up && let Ok(reply) = self.results.try_recv() {
            self.pending = false;
            let current = self.submissions.lock().boundary.clone();
            if active
                && reply.probe.revision == self.revision
                && reply.probe.connection == connection
                && current.as_ref().is_some_and(|boundary| {
                    boundary.accepted && boundary.generation == reply.probe.boundary.generation
                })
            {
                match reply.result {
                    Ok(Some(mut turn)) => {
                        if !turn.success && turn.error.is_none() {
                            turn.error = self
                                .terminal_error
                                .as_ref()
                                .filter(|(generation, _)| *generation == turn.generation)
                                .map(|(_, error)| error.clone());
                        }
                        self.needs_history = false;
                        self.terminal_error = None;
                        return Some(turn);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        crate::opencode_diagnostics::record(
                            "reconcile_unavailable",
                            self.port,
                            connection,
                        );
                    }
                }
            }
        }
        if !active || self.pending || feed.is_cancelled() {
            return None;
        }
        if !self.reconnect_due
            && (self.last_progress.elapsed() < QUIET_INTERVAL
                || self.last_probe.elapsed() < QUIET_INTERVAL)
        {
            return None;
        }
        let boundary = self.submissions.lock().boundary.clone()?;
        if !boundary.accepted || (boundary.server_initiated && boundary.execution_id.is_none()) {
            return None;
        }
        self.last_probe = Instant::now();
        self.reconnect_due = false;
        self.pending = self
            .requests
            .try_send(Probe {
                boundary,
                revision: self.revision,
                connection,
            })
            .is_ok();
        None
    }
}

fn fetch_turn(
    port: u16,
    session: &str,
    boundary: &Boundary,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<Option<RecoveredTurn>> {
    let path = format!("/api/session/{}", encode_path_segment(session));
    let info = request_json_on_port_bounded(port, &path, REQUEST_TIMEOUT, MAX_RECOVERY_BYTES)?;
    let info = info.get("data").unwrap_or(&info);
    if matches!(
        info.pointer("/status/type").and_then(Value::as_str),
        Some("busy" | "running" | "retry")
    ) {
        return Ok(None);
    }
    let idle_at = info.pointer("/time/idle").and_then(Value::as_u64);
    let mut terminal = idle_at
        .filter(|time| *time > boundary.started_at)
        .and_then(|time| {
            info.get("outcome")
                .and_then(Value::as_str)
                .map(|outcome| (time, outcome.to_owned()))
        });
    let started = Instant::now();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = std::collections::HashSet::new();
    let mut rows = Vec::new();
    let mut found_input = false;
    let mut full_window = false;
    let mut bytes = 0;
    for page in 0..MAX_PAGES {
        if cancelled() || started.elapsed() >= WALK_BUDGET {
            bail!("OpenCode reconciliation cancelled or exceeded its budget");
        }
        let mut query = format!("{path}/message?limit={PAGE_LIMIT}");
        if let Some(cursor) = &cursor {
            query.push_str(&format!("&cursor={}", encode_path_segment(cursor)));
        }
        let timeout = REQUEST_TIMEOUT.min(WALK_BUDGET.saturating_sub(started.elapsed()));
        if timeout.is_zero() {
            bail!("OpenCode reconciliation exceeded its pagination deadline");
        }
        let response = request_json_on_port_bounded(port, &query, timeout, MAX_RECOVERY_BYTES)?;
        let messages = response
            .get("data")
            .and_then(Value::as_array)
            .context("OpenCode reconciliation returned no message array")?;
        bytes += serde_json::to_vec(messages)?.len();
        if bytes > MAX_RECOVERY_BYTES {
            bail!("OpenCode reconciliation exceeded its memory budget");
        }
        let mut saw_older = false;
        for row in messages {
            let body = row.get("info").unwrap_or(row);
            let created = body.pointer("/time/created").and_then(Value::as_u64);
            let id = body.get("id").and_then(Value::as_str);
            let kind = body
                .get("type")
                .or_else(|| body.get("role"))
                .and_then(Value::as_str);
            if boundary.server_initiated
                && kind == Some("idle")
                && id
                    .zip(boundary.execution_id.as_deref())
                    .is_some_and(|(id, started)| id < started)
            {
                full_window = true;
                saw_older = true;
                break;
            }
            if !boundary.server_initiated && created.is_some_and(|time| time < boundary.started_at)
            {
                saw_older = true;
                break;
            }
            if terminal.is_none() && kind == Some("idle") {
                terminal = created
                    // A same-millisecond previous idle is ambiguous when the
                    // client supplied an ID; never treat it as a new boundary.
                    .filter(|time| *time > boundary.started_at)
                    .and_then(|time| {
                        body.get("outcome")
                            .and_then(Value::as_str)
                            .map(|outcome| (time, outcome.to_owned()))
                    });
            }
            rows.push(row.clone());
            if boundary
                .input_id
                .as_deref()
                .is_some_and(|input| id == Some(input))
                || (!boundary.server_initiated
                    && boundary.input_id.is_none()
                    && kind == Some("user")
                    && created.is_some_and(|time| time >= boundary.started_at))
            {
                found_input = true;
                break;
            }
        }
        if found_input || saw_older {
            break;
        }
        let next = response.pointer("/cursor/next").and_then(Value::as_str);
        let Some(next) = next.filter(|next| {
            !next.is_empty() && Some(*next) != cursor.as_deref() && !messages.is_empty()
        }) else {
            full_window = response
                .pointer("/cursor/next")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty);
            break;
        };
        if page + 1 == MAX_PAGES || !seen_cursors.insert(next.to_owned()) {
            bail!("OpenCode reconciliation exceeded its message window");
        }
        cursor = Some(next.to_owned());
    }
    if (!found_input && !boundary.server_initiated) || (boundary.server_initiated && !full_window) {
        return Ok(None);
    }
    if let Some(latest) = &boundary.latest_input_id
        && !rows
            .iter()
            .any(|row| row.get("id").and_then(Value::as_str) == Some(latest))
    {
        return Ok(None);
    }
    let Some((idle_at, outcome)) = terminal else {
        return Ok(None);
    };
    if !matches!(outcome.as_str(), "succeeded" | "failed" | "interrupted") {
        return Ok(None);
    }
    if let Some(latest) = boundary.latest_input_id.as_deref() {
        let latest_created = rows
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(latest))
            .and_then(|row| row.pointer("/time/created"))
            .and_then(Value::as_u64);
        let idle_follows_input = latest_created.is_some_and(|created| idle_at > created)
            || rows.iter().any(|row| {
                row.get("type").and_then(Value::as_str) == Some("idle")
                    && row.pointer("/time/created").and_then(Value::as_u64) == Some(idle_at)
                    && row
                        .get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id > latest)
            });
        if !idle_follows_input {
            // A previous execution's idle can share the input's millisecond.
            // Require native ordering proof, not timestamp equality alone.
            return Ok(None);
        }
    }
    // An idle marker from another writer cannot settle an execution this
    // private process still owns. Missing active entries alone prove nothing.
    let active = request_json_on_port_bounded(
        port,
        "/api/session/active",
        REQUEST_TIMEOUT,
        MAX_RECOVERY_BYTES,
    )?;
    let active = active
        .get("data")
        .and_then(Value::as_object)
        .context("OpenCode reconciliation returned no active-session map")?;
    if active.contains_key(session) {
        return Ok(None);
    }
    if rows.iter().any(|row| {
        row.pointer("/time/created")
            .and_then(Value::as_u64)
            .is_some_and(|time| time > idle_at)
            && matches!(
                row.get("type").and_then(Value::as_str),
                Some("user" | "synthetic" | "assistant")
            )
    }) {
        return Ok(None);
    }
    rows.reverse();
    let completion = Completion {
        created: idle_at,
        message_id: rows
            .iter()
            .rev()
            .find(|row| {
                row.get("type").and_then(Value::as_str) == Some("idle")
                    && row.pointer("/time/created").and_then(Value::as_u64) == Some(idle_at)
            })
            .and_then(|row| row.get("id").and_then(Value::as_str))
            .map(str::to_owned),
    };
    let error = (outcome != "succeeded")
        .then(|| {
            rows.iter()
                .rev()
                .find(|row| row.get("type").and_then(Value::as_str) == Some("assistant"))
                .and_then(|row| {
                    row.get("error")
                        .filter(|error| !error.is_null())
                        .and_then(|error| {
                            error
                                .get("message")
                                .and_then(Value::as_str)
                                .or_else(|| error.as_str())
                                .map(str::to_owned)
                        })
                })
        })
        .flatten();
    let acknowledged_steers = boundary
        .unconfirmed_steers
        .iter()
        .filter_map(|(id, text)| {
            rows.iter()
                .any(|row| row.get("id").and_then(Value::as_str) == Some(id))
                .then(|| text.clone())
        })
        .collect();
    let transcript = super::super::native::translate_recovered_rows(&rows);
    if transcript
        .messages
        .iter()
        .filter(|message| message.role == crate::model::MessageRole::User)
        .count()
        != rows
            .iter()
            .filter(|row| row.get("type").and_then(Value::as_str) == Some("user"))
            .count()
    {
        // A shifted or attachment-only shape the translator cannot materialize
        // must not be treated as a complete replacement of local inputs.
        return Ok(None);
    }
    Ok(Some(RecoveredTurn {
        generation: boundary.generation,
        transcript,
        continuation: boundary.server_initiated,
        success: outcome == "succeeded",
        error,
        acknowledged_steers,
        completion,
    }))
}

/// A cold receipt check uses the same exact-input terminal proof as live
/// recovery, without subscribing to events, restarting execution or POSTing.
pub(super) fn reconcile_saved(
    port: u16,
    session: &str,
    receipt: &fintwind_protocol::submission::SubmissionReceipt,
) -> anyhow::Result<Option<super::super::SavedSubmissionReconciliation>> {
    let boundary = Boundary {
        generation: 0,
        started_at: receipt.created_at,
        input_id: Some(receipt.input_id.clone()),
        latest_input_id: Some(receipt.input_id.clone()),
        unconfirmed_steers: Vec::new(),
        accepted: true,
        server_initiated: false,
        execution_id: None,
    };
    Ok(fetch_turn(port, session, &boundary, || false)?.map(|turn| {
        super::super::SavedSubmissionReconciliation {
            status: if turn.success {
                crate::model::TurnStatus::Completed
            } else {
                crate::model::TurnStatus::Failed
            },
            completed_at: turn.completion.created / 1000,
            transcript: turn.transcript,
        }
    }))
}

use crate::theme::{code_px, ui_px};

use super::*;

const MAX_BACKGROUND_OUTPUT_BYTES: usize = 512 * 1024;
const MAX_SETTLED_BACKGROUND_ITEMS: usize = 24;
const OUTPUT_CACHE_REFRESH_INTERVAL: Duration = Duration::from_millis(100);
const BACKGROUND_SUMMARY_MENU_ID: &str = "background-work-summary";

#[derive(Default)]
pub(super) struct BackgroundWorkRegistry {
    items: HashMap<BackgroundWorkKey, BackgroundWorkItem>,
    order: Vec<BackgroundWorkKey>,
    /// GPUI's shared text makes repainting a long log O(1). It is rebuilt only
    /// when provider output changes, never from the panel's render path.
    rendered_output: HashMap<BackgroundWorkKey, SharedString>,
    dirty_output: HashSet<BackgroundWorkKey>,
    /// Structured child-session transcript projections. Kept separate from
    /// terminal output so a subagent's live conversation can render through
    /// the same message/activity primitives as the main transcript.
    transcripts: HashMap<BackgroundWorkKey, BackgroundWorkTranscript>,
    last_output_cache_refresh: Option<Instant>,
    output_viewports: HashMap<BackgroundWorkKey, BackgroundOutputViewport>,
    selection: TranscriptSelection,
}

#[derive(Clone)]
struct BackgroundOutputViewport {
    scroll_handle: ScrollHandle,
    scrollbar: Rc<ScrollbarState>,
}

impl Default for BackgroundOutputViewport {
    fn default() -> Self {
        Self {
            scroll_handle: ScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
        }
    }
}

#[derive(Clone)]
struct BackgroundSummaryEntry {
    item: BackgroundWorkItem,
    row_focus: FocusHandle,
    stop_focus: FocusHandle,
}

#[derive(Clone)]
struct EnvironmentSummary {
    branch: Option<String>,
    additions: u64,
    deletions: u64,
    has_changes: bool,
    commit_status: Option<String>,
    changes_focus: FocusHandle,
    commit_focus: FocusHandle,
    compare_focus: FocusHandle,
}

/// Progress of one entry of the latest plan activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TodoEntryState {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl TodoEntryState {
    /// Providers spell plan statuses many ways; map the known families and
    /// leave anything else pending.
    fn from_status(status: &str) -> Self {
        match status
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "completed" | "complete" | "done" | "finished" | "success" | "succeeded" | "ok" => {
                Self::Completed
            }
            "in_progress" | "inprogress" | "active" | "running" | "current" | "started"
            | "working" => Self::InProgress,
            "failed" | "error" | "errored" | "aborted" | "cancelled" | "canceled" => Self::Failed,
            _ => Self::Pending,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Pending => tr!("todo.status.pending"),
            Self::InProgress => tr!("todo.status.in_progress"),
            Self::Completed => tr!("todo.status.completed"),
            Self::Failed => tr!("todo.status.failed"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TodoEntry {
    pub label: String,
    pub state: TodoEntryState,
}

/// Read-only display model for the capsule's Todo section: the newest
/// `ActivityKind::Plan` activity in the transcript. Built off the render
/// path — see [`Fintwind::rebuild_todo_summary`].
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct TodoSummary {
    pub entries: Vec<TodoEntry>,
    pub updating: bool,
    pub failed: bool,
}

/// Extracts the display model from a session's transcript blocks. Pure, so
/// callers can invoke it on plan updates, hydration, and selection changes
/// without ever walking blocks from a row builder.
pub(super) fn todo_summary_from_blocks(blocks: &[crate::model::TranscriptBlock]) -> TodoSummary {
    blocks
        .iter()
        .rev()
        .flat_map(|block| block.activities.iter().rev())
        .find(|activity| activity.kind == crate::model::ActivityKind::Plan)
        .map(todo_summary_from_activity)
        .unwrap_or_default()
}

fn todo_summary_from_activity(activity: &crate::model::ActivityItem) -> TodoSummary {
    let mut entries = todo_entries_from_json(activity.arguments.as_deref());
    if entries.is_empty() {
        entries = todo_entries_from_json(activity.output.as_deref());
    }
    if entries.is_empty() {
        let state = match (activity.failed, activity.complete) {
            (true, _) => TodoEntryState::Failed,
            (false, false) => TodoEntryState::InProgress,
            (false, true) => TodoEntryState::Completed,
        };
        entries.push(TodoEntry {
            label: fallback_todo_label(activity),
            state,
        });
    }
    TodoSummary {
        entries,
        updating: !activity.complete,
        failed: activity.failed,
    }
}

fn fallback_todo_label(activity: &crate::model::ActivityItem) -> String {
    if !crate::model::is_generic_activity_title(activity.kind, &activity.title) {
        return activity.title.clone();
    }
    if let Some(detail) = activity
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
    {
        return detail.to_owned();
    }
    tr!("activity.action_plan")
}

fn todo_entries_from_json(json: Option<&str>) -> Vec<TodoEntry> {
    let Some(text) = json
        .map(str::trim)
        .filter(|text| text.starts_with('[') || text.starts_with('{'))
    else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    todo_entries_from_value(&value)
}

fn todo_entries_from_value(value: &serde_json::Value) -> Vec<TodoEntry> {
    for key in ["todos", "todo", "plan", "items", "steps"] {
        if let Some(items) = value.get(key).and_then(|items| items.as_array()) {
            let entries = items
                .iter()
                .filter_map(todo_entry_from_item)
                .collect::<Vec<_>>();
            if !entries.is_empty() {
                return entries;
            }
        }
    }
    value
        .as_array()
        .map(|items| items.iter().filter_map(todo_entry_from_item).collect())
        .unwrap_or_default()
}

fn todo_entry_from_item(item: &serde_json::Value) -> Option<TodoEntry> {
    match item {
        serde_json::Value::String(text) => {
            let label = text.trim();
            (!label.is_empty()).then(|| TodoEntry {
                label: label.to_owned(),
                state: TodoEntryState::Pending,
            })
        }
        serde_json::Value::Object(map) => {
            let label = [
                "content",
                "text",
                "title",
                "description",
                "step",
                "label",
                "name",
            ]
            .iter()
            .find_map(|key| map.get(*key).and_then(|value| value.as_str()))
            .map(str::trim)
            .filter(|label| !label.is_empty())?;
            let state = ["status", "state"]
                .iter()
                .find_map(|key| map.get(*key).and_then(|value| value.as_str()))
                .map(TodoEntryState::from_status)
                .or_else(|| match map.get("completed") {
                    Some(serde_json::Value::Bool(true)) => Some(TodoEntryState::Completed),
                    _ => None,
                })
                .unwrap_or(TodoEntryState::Pending);
            Some(TodoEntry {
                label: label.to_owned(),
                state,
            })
        }
        _ => None,
    }
}

impl BackgroundWorkRegistry {
    fn from_snapshots(snapshots: &[BackgroundWorkSnapshot]) -> Self {
        let mut registry = Self::default();
        for snapshot in snapshots {
            registry.upsert(snapshot.item.clone());
            registry
                .transcripts
                .insert(snapshot.item.key.clone(), snapshot.transcript.clone());
        }
        registry
    }

    fn snapshot(&self, key: &BackgroundWorkKey) -> Option<BackgroundWorkSnapshot> {
        let item = self.items.get(key)?.clone();
        Some(BackgroundWorkSnapshot {
            item,
            transcript: self.transcripts.get(key)?.clone(),
        })
    }

    fn apply(&mut self, event: BackgroundWorkEvent) {
        match event {
            BackgroundWorkEvent::Upsert(item) => self.upsert(item),
            BackgroundWorkEvent::OutputDelta { key, delta } => self.append_output(&key, &delta),
            BackgroundWorkEvent::Transcript(event) => self.apply_transcript(event),
            BackgroundWorkEvent::ReconcileProcesses { items } => self.reconcile_processes(items),
            BackgroundWorkEvent::ReconcileLive { items } => self.reconcile_live(items),
            BackgroundWorkEvent::StopRequested(key) => {
                if let Some(item) = self.items.get_mut(&key) {
                    item.status = BackgroundWorkStatus::Stopping;
                    item.updated_at_ms = unix_time_millis();
                }
            }
            BackgroundWorkEvent::StopFailed { key, message } => {
                if let Some(item) = self
                    .items
                    .get_mut(&key)
                    .filter(|item| item.status.is_live())
                {
                    item.status = match item.key.kind {
                        BackgroundWorkKind::Monitor => BackgroundWorkStatus::Monitoring,
                        BackgroundWorkKind::Process | BackgroundWorkKind::Subagent => {
                            BackgroundWorkStatus::Running
                        }
                    };
                    item.detail = Some(message);
                    item.updated_at_ms = unix_time_millis();
                }
            }
        }
        self.trim_settled();
    }

    fn upsert(&mut self, mut incoming: BackgroundWorkItem) {
        let key = incoming.key.clone();
        // Short foreground commands belong in the transcript. Show them while
        // running, then remove them instead of turning this surface into a
        // duplicate command history.
        let already_background = self
            .items
            .get(&incoming.key)
            .is_some_and(|item| item.background);
        if !incoming.background
            && !already_background
            && !incoming.status.is_live()
            && matches!(
                incoming.key.kind,
                BackgroundWorkKind::Process | BackgroundWorkKind::Monitor
            )
        {
            self.remove(&incoming.key);
            return;
        }

        bound_output(&mut incoming);
        self.output_viewports.entry(key.clone()).or_default();
        let output_changed;
        if let Some(current) = self.items.get_mut(&incoming.key) {
            if incoming.title.is_empty() {
                incoming.title.clone_from(&current.title);
            }
            current.title = incoming.title;
            merge_option(&mut current.detail, incoming.detail);
            merge_option(&mut current.command, incoming.command);
            merge_option(&mut current.cwd, incoming.cwd);
            if let Some(output) = incoming.output {
                output_changed = current.output.as_ref() != Some(&output)
                    || current.output_truncated != incoming.output_truncated;
                current.output = Some(output);
                current.output_truncated = incoming.output_truncated;
            } else {
                output_changed = false;
            }
            merge_option(&mut current.duration_ms, incoming.duration_ms);
            merge_option(&mut current.exit_code, incoming.exit_code);
            merge_option(&mut current.control_id, incoming.control_id);
            for origin in incoming.origin_activity_ids {
                if !current.origin_activity_ids.contains(&origin) {
                    current.origin_activity_ids.push(origin);
                }
            }
            merge_option(&mut current.role, incoming.role);
            merge_option(&mut current.model, incoming.model);
            merge_option(&mut current.parent_id, incoming.parent_id);
            current.started_at_ms = current.started_at_ms.min(incoming.started_at_ms);
            current.updated_at_ms = current.updated_at_ms.max(incoming.updated_at_ms);
            current.background |= incoming.background;
            current.can_stop = if incoming.status.is_live() {
                current.can_stop || incoming.can_stop
            } else {
                false
            };
            current.status = merge_work_status(current.status, incoming.status);
        } else {
            output_changed = incoming.output.is_some();
            self.order.push(incoming.key.clone());
            self.items.insert(incoming.key.clone(), incoming);
        }
        if output_changed {
            self.dirty_output.insert(key);
        }
    }

    fn append_output(&mut self, key: &BackgroundWorkKey, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let Some(item) = self.items.get_mut(key) else {
            return;
        };
        self.output_viewports.entry(key.clone()).or_default();
        item.output.get_or_insert_with(String::new).push_str(delta);
        item.updated_at_ms = unix_time_millis();
        bound_output(item);
        self.dirty_output.insert(key.clone());
    }

    fn apply_transcript(&mut self, event: BackgroundWorkTranscriptEvent) {
        match event {
            BackgroundWorkTranscriptEvent::Started { key, prompt } => {
                let transcript = self.transcripts.entry(key).or_default();
                // Deltas can legitimately arrive before Started (the child
                // existed before the driver attached), and Started also
                // arrives again when the parent resumes a settled child. Open
                // a turn whenever the newest one is no longer running; the
                // prompt message only when it would still come first, so a
                // resume neither reopens the old turn nor repeats the prompt.
                let needs_turn = transcript
                    .turns
                    .last()
                    .is_none_or(|turn| turn.status != TurnStatus::Running);
                if needs_turn {
                    let turn_id = uuid::Uuid::new_v4();
                    let turn_count = transcript.turns.len() + 1;
                    transcript.turns.push(AgentTurn {
                        id: turn_id,
                        turn_count,
                        status: TurnStatus::Running,
                        provider_turn_started: true,
                        provider_resume_at: None,
                        started_at: unix_time(),
                        completed_at: None,
                        checkpoint: None,
                        stats: None,
                    });
                    // The prompt opens the new turn unless content already
                    // arrived before Started — a restored child whose deltas
                    // raced the announce — where inserting it would reorder
                    // the tail. A message with no turn id is exactly that
                    // pre-turn content.
                    let tail_is_prior_turn = transcript
                        .messages
                        .last()
                        .is_none_or(|message| message.turn_id.is_some());
                    if tail_is_prior_turn
                        && let Some(prompt) = prompt.filter(|prompt| !prompt.trim().is_empty())
                    {
                        transcript.messages.push(Message::new_for_turn(
                            MessageRole::User,
                            prompt,
                            turn_id,
                        ));
                    }
                }
            }
            BackgroundWorkTranscriptEvent::TextDelta { key, delta } => {
                Self::append_background_text(self.transcripts.entry(key).or_default(), &delta);
            }
            BackgroundWorkTranscriptEvent::ReasoningDelta { key, delta } => {
                Self::append_background_reasoning(self.transcripts.entry(key).or_default(), &delta);
            }
            BackgroundWorkTranscriptEvent::Activity { key, activity } => {
                Self::upsert_background_activity(
                    self.transcripts.entry(key).or_default(),
                    activity,
                );
            }
            BackgroundWorkTranscriptEvent::Snapshot { key, transcript } => {
                let current = self.transcripts.entry(key).or_default();
                if current.messages.is_empty() && current.transcript_blocks.is_empty() {
                    *current = transcript;
                }
            }
            BackgroundWorkTranscriptEvent::Finished { key, success } => {
                Self::finish_background_transcript(
                    self.transcripts.entry(key).or_default(),
                    success,
                );
            }
        }
    }

    fn append_background_text(transcript: &mut BackgroundWorkTranscript, delta: &str) {
        if delta.is_empty() {
            return;
        }
        Self::finish_live_background_reasoning(transcript);
        if let Some(message) = transcript
            .messages
            .last_mut()
            .filter(|message| message.role == MessageRole::Assistant && message.streaming)
        {
            message.content.push_str(delta);
            return;
        }
        let turn_id = transcript.turns.last().map(|turn| turn.id);
        let mut message = turn_id
            .map(|turn_id| Message::new_for_turn(MessageRole::Assistant, delta, turn_id))
            .unwrap_or_else(|| Message::new(MessageRole::Assistant, delta));
        message.streaming = true;
        transcript.messages.push(message);
    }

    fn append_background_reasoning(transcript: &mut BackgroundWorkTranscript, delta: &str) {
        if delta.is_empty() {
            return;
        }
        Self::finish_live_background_message(transcript);
        let after_message = transcript.messages.len();
        let turn_id = transcript.turns.last().map(|turn| turn.id);
        if let Some(activity) = transcript
            .transcript_blocks
            .last_mut()
            .filter(|block| block.after_message == after_message && block.turn_id == turn_id)
            .and_then(|block| block.activities.last_mut())
            .filter(|activity| activity.reasoning.is_some() && !activity.complete)
        {
            if let Some(reasoning) = activity.reasoning.as_mut() {
                reasoning.content.push_str(delta);
                reasoning.finished_at_ms = unix_time_millis();
            }
            return;
        }
        let mut activity = ActivityItem::from_reasoning(
            ReasoningBlock {
                content: delta.to_owned(),
                started_at_ms: unix_time_millis(),
                finished_at_ms: unix_time_millis(),
            },
            false,
        );
        activity.complete = false;
        if let Some(block) = transcript
            .transcript_blocks
            .last_mut()
            .filter(|block| block.after_message == after_message && block.turn_id == turn_id)
        {
            block.activities.push(activity);
        } else {
            transcript.transcript_blocks.push(TranscriptBlock {
                after_message,
                turn_id,
                activities: vec![activity],
            });
        }
    }

    fn upsert_background_activity(
        transcript: &mut BackgroundWorkTranscript,
        incoming: ActivityItem,
    ) {
        Self::finish_live_background_message(transcript);
        Self::finish_live_background_reasoning(transcript);
        let matching = transcript
            .transcript_blocks
            .iter_mut()
            .rev()
            .flat_map(|block| block.activities.iter_mut().rev())
            .find(|activity| {
                incoming
                    .source_id
                    .as_deref()
                    .is_some_and(|source| activity.source_id.as_deref() == Some(source))
            });
        if let Some(current) = matching {
            let id = current.id;
            *current = incoming;
            current.id = id;
            return;
        }
        let turn_id = transcript.turns.last().map(|turn| turn.id);
        let after_message = transcript.messages.len();
        if let Some(block) = transcript
            .transcript_blocks
            .last_mut()
            .filter(|block| block.after_message == after_message && block.turn_id == turn_id)
        {
            block.activities.push(incoming);
        } else {
            transcript.transcript_blocks.push(TranscriptBlock {
                after_message,
                turn_id,
                activities: vec![incoming],
            });
        }
    }

    fn finish_live_background_message(transcript: &mut BackgroundWorkTranscript) {
        if let Some(message) = transcript
            .messages
            .last_mut()
            .filter(|message| message.role == MessageRole::Assistant && message.streaming)
        {
            message.streaming = false;
        }
    }

    fn finish_live_background_reasoning(transcript: &mut BackgroundWorkTranscript) {
        if let Some(activity) = transcript
            .transcript_blocks
            .last_mut()
            .and_then(|block| block.activities.last_mut())
            .filter(|activity| activity.reasoning.is_some() && !activity.complete)
        {
            activity.complete = true;
        }
    }

    fn finish_background_transcript(transcript: &mut BackgroundWorkTranscript, success: bool) {
        for message in &mut transcript.messages {
            if message.role == MessageRole::Assistant {
                message.streaming = false;
            }
        }
        for block in &mut transcript.transcript_blocks {
            for activity in &mut block.activities {
                activity.complete = true;
                if activity.reasoning.is_some() {
                    activity.failed = !success;
                }
            }
        }
        if let Some(turn) = transcript
            .turns
            .last_mut()
            .filter(|turn| turn.status == TurnStatus::Running)
        {
            turn.status = if success {
                TurnStatus::Completed
            } else {
                TurnStatus::Failed
            };
            turn.completed_at = Some(unix_time());
        }
    }

    fn reconcile_processes(&mut self, items: Vec<BackgroundWorkItem>) {
        let present = items
            .iter()
            .map(|item| item.key.clone())
            .collect::<HashSet<_>>();
        let now = unix_time_millis();

        for item in self.items.values_mut() {
            if matches!(
                item.key.kind,
                BackgroundWorkKind::Process | BackgroundWorkKind::Monitor
            ) && item.background
                && item.status.is_live()
                && !present.contains(&item.key)
            {
                item.status = BackgroundWorkStatus::Lost;
                item.can_stop = false;
                item.updated_at_ms = now;
            }
        }
        for item in items {
            self.upsert(item);
        }
    }

    fn reconcile_live(&mut self, items: Vec<BackgroundWorkItem>) {
        let present = items
            .iter()
            .map(|item| item.key.clone())
            .collect::<HashSet<_>>();
        let now = unix_time_millis();
        for item in self.items.values_mut() {
            if item.background && item.status.is_live() && !present.contains(&item.key) {
                item.status = BackgroundWorkStatus::Lost;
                item.can_stop = false;
                item.updated_at_ms = now;
            }
        }
        for mut item in items {
            // The snapshot reflects the moment of the fetch: a child event
            // may have settled the run after it was taken, and a slow or
            // unknown server answer must not roll that state back. A snapshot
            // may only move a status forward; being listed again recovers a
            // previously Lost item.
            if let Some(current) = self.items.get(&item.key) {
                let recovered = current.status == BackgroundWorkStatus::Lost;
                if !recovered && status_progress(current.status) >= status_progress(item.status) {
                    item.status = current.status;
                }
            }
            self.upsert(item);
        }
    }

    fn remove(&mut self, key: &BackgroundWorkKey) {
        self.items.remove(key);
        self.transcripts.remove(key);
        self.rendered_output.remove(key);
        self.dirty_output.remove(key);
        self.output_viewports.remove(key);
        self.order.retain(|entry| entry != key);
    }

    fn trim_settled(&mut self) {
        let mut settled = self
            .order
            .iter()
            .filter_map(|key| self.items.get(key).map(|item| (key, item)))
            .filter(|(_, item)| !item.status.is_live())
            .count();
        if settled <= MAX_SETTLED_BACKGROUND_ITEMS {
            return;
        }
        let stale = self.order.clone();
        for key in stale {
            if settled <= MAX_SETTLED_BACKGROUND_ITEMS {
                break;
            }
            if self
                .items
                .get(&key)
                .is_some_and(|item| !item.status.is_live())
            {
                self.remove(&key);
                settled -= 1;
            }
        }
    }

    fn mark_live_lost(&mut self) {
        let now = unix_time_millis();
        for item in self.items.values_mut().filter(|item| item.status.is_live()) {
            item.status = BackgroundWorkStatus::Lost;
            item.can_stop = false;
            item.updated_at_ms = now;
        }
    }

    fn settle_foreground(&mut self, status: BackgroundWorkStatus) {
        let keys = self
            .items
            .values()
            .filter(|item| !item.background && item.status.is_live())
            .map(|item| item.key.clone())
            .collect::<Vec<_>>();
        let now = unix_time_millis();
        for key in keys {
            if key.kind == BackgroundWorkKind::Subagent {
                if let Some(item) = self.items.get_mut(&key) {
                    item.status = status;
                    item.can_stop = false;
                    item.updated_at_ms = now;
                }
            } else {
                self.remove(&key);
            }
        }
    }

    fn has_live(&self) -> bool {
        self.items.values().any(|item| item.status.is_live())
    }

    fn counts(&self) -> (usize, usize) {
        self.items
            .values()
            .filter(|item| item.status.is_live())
            .fold((0, 0), |(processes, agents), item| match item.key.kind {
                BackgroundWorkKind::Subagent => (processes, agents + 1),
                BackgroundWorkKind::Process | BackgroundWorkKind::Monitor => {
                    (processes + 1, agents)
                }
            })
    }

    fn ordered_items(&self) -> Vec<&BackgroundWorkItem> {
        self.order
            .iter()
            .rev()
            .filter_map(|key| self.items.get(key))
            .collect()
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        self.selection.selection.borrow().selected_text()
    }

    fn refresh_output_cache(&mut self) -> bool {
        if self.dirty_output.is_empty()
            || self
                .last_output_cache_refresh
                .is_some_and(|last| last.elapsed() < OUTPUT_CACHE_REFRESH_INTERVAL)
        {
            return false;
        }
        let dirty = std::mem::take(&mut self.dirty_output);
        for key in dirty {
            if let Some(output) = self.items.get(&key).and_then(|item| item.output.as_deref()) {
                self.rendered_output
                    .insert(key.clone(), SharedString::from(strip_ansi(output)));
                if let Some(viewport) = self.output_viewports.get(&key) {
                    viewport.scroll_handle.scroll_to_bottom();
                }
            }
        }
        self.last_output_cache_refresh = Some(Instant::now());
        true
    }

    fn output_refresh_delay(&self) -> Option<Duration> {
        (!self.dirty_output.is_empty()).then(|| {
            self.last_output_cache_refresh
                .map(|last| OUTPUT_CACHE_REFRESH_INTERVAL.saturating_sub(last.elapsed()))
                .unwrap_or_default()
        })
    }
}

fn merge_option<T>(target: &mut Option<T>, incoming: Option<T>) {
    if incoming.is_some() {
        *target = incoming;
    }
}

/// Lifecycle progress of a status. Live reconciles describe the past (the
/// moment of the fetch), so a snapshot may move a status forward but never
/// back over an event that already arrived.
fn status_progress(status: BackgroundWorkStatus) -> u8 {
    match status {
        BackgroundWorkStatus::Starting => 0,
        BackgroundWorkStatus::Running
        | BackgroundWorkStatus::Monitoring
        | BackgroundWorkStatus::Stopping => 1,
        BackgroundWorkStatus::Completed
        | BackgroundWorkStatus::Failed
        | BackgroundWorkStatus::Stopped
        | BackgroundWorkStatus::Lost => 2,
    }
}

fn merge_work_status(
    current: BackgroundWorkStatus,
    incoming: BackgroundWorkStatus,
) -> BackgroundWorkStatus {
    match current {
        BackgroundWorkStatus::Stopping if incoming.is_live() => BackgroundWorkStatus::Stopping,
        BackgroundWorkStatus::Stopping if incoming == BackgroundWorkStatus::Lost => {
            BackgroundWorkStatus::Lost
        }
        BackgroundWorkStatus::Stopping => BackgroundWorkStatus::Stopped,
        BackgroundWorkStatus::Stopped
            if !incoming.is_live() && incoming != BackgroundWorkStatus::Lost =>
        {
            BackgroundWorkStatus::Stopped
        }
        _ => incoming,
    }
}

fn recover_subagent_origin(session: &AgentSession, item: &mut BackgroundWorkItem) {
    if item.key.kind != BackgroundWorkKind::Subagent {
        return;
    }
    let marker = format!("sessionID=\"{}\"", item.key.provider_id);
    // Every task call that names this child is an entry point: the one that
    // spawned it and the ones that resumed it. The provider stamps each with
    // the same `sessionID` marker, so a scan of the transcript recovers all of
    // them — including a resume the driver linked live but a reloaded snapshot
    // only recorded the first half of.
    for activity in session
        .transcript_blocks
        .iter()
        .flat_map(|block| &block.activities)
        .filter(|activity| {
            is_subagent_activity(activity)
                && activity
                    .output
                    .as_deref()
                    .is_some_and(|output| output.contains(&marker))
        })
    {
        if let Some(source) = activity.source_id.clone()
            && !item.origin_activity_ids.contains(&source)
        {
            item.origin_activity_ids.push(source);
        }
    }
}

/// Whether a transcript activity is a subagent tool call. Recovery matches the
/// child's `sessionID` marker only on these, so an unrelated tool whose output
/// happens to quote the marker cannot claim the link.
fn is_subagent_activity(activity: &ActivityItem) -> bool {
    activity.kind == crate::model::ActivityKind::Tool
        && matches!(
            activity.title.trim().to_ascii_lowercase().as_str(),
            "task" | "subagent"
        )
}

fn bound_output(item: &mut BackgroundWorkItem) {
    let Some(output) = item.output.as_mut() else {
        return;
    };
    if output.len() <= MAX_BACKGROUND_OUTPUT_BYTES {
        return;
    }
    let mut cut = output.len() - MAX_BACKGROUND_OUTPUT_BYTES;
    while !output.is_char_boundary(cut) {
        cut += 1;
    }
    output.drain(..cut);
    item.output_truncated = true;
}

fn strip_ansi(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            clean.push(character);
        }
    }
    clean
}

pub(super) fn work_status_label(status: BackgroundWorkStatus) -> String {
    match status {
        BackgroundWorkStatus::Starting => tr!("background.status.starting"),
        BackgroundWorkStatus::Running => tr!("background.status.running"),
        BackgroundWorkStatus::Monitoring => tr!("background.status.monitoring"),
        BackgroundWorkStatus::Stopping => tr!("background.status.stopping"),
        BackgroundWorkStatus::Completed => tr!("background.status.completed"),
        BackgroundWorkStatus::Failed => tr!("background.status.failed"),
        BackgroundWorkStatus::Stopped => tr!("background.status.stopped"),
        BackgroundWorkStatus::Lost => tr!("background.status.lost"),
    }
}

fn work_status_icon(status: BackgroundWorkStatus) -> &'static str {
    match status {
        BackgroundWorkStatus::Starting
        | BackgroundWorkStatus::Running
        | BackgroundWorkStatus::Monitoring => "icons/loader-circle.svg",
        BackgroundWorkStatus::Stopping | BackgroundWorkStatus::Stopped => "icons/stop.svg",
        BackgroundWorkStatus::Completed => "icons/check.svg",
        BackgroundWorkStatus::Failed => "icons/x.svg",
        BackgroundWorkStatus::Lost => "icons/alert.svg",
    }
}

fn background_summary_process_status_icon(
    kind: BackgroundWorkKind,
    status: BackgroundWorkStatus,
) -> Option<&'static str> {
    if !matches!(
        kind,
        BackgroundWorkKind::Process | BackgroundWorkKind::Monitor
    ) {
        return None;
    }

    match status {
        BackgroundWorkStatus::Starting
        | BackgroundWorkStatus::Running
        | BackgroundWorkStatus::Monitoring
        | BackgroundWorkStatus::Completed
        | BackgroundWorkStatus::Failed => Some(work_status_icon(status)),
        _ => None,
    }
}

pub(super) fn rendered_work_status_icon(
    status: BackgroundWorkStatus,
    size: f32,
    color: Hsla,
) -> AnyElement {
    let icon = icon(work_status_icon(status), size, color);
    if matches!(
        status,
        BackgroundWorkStatus::Starting
            | BackgroundWorkStatus::Running
            | BackgroundWorkStatus::Monitoring
    ) {
        // Background work runs for minutes; don't price its pane at full rate.
        motion::spin_slow(icon)
    } else {
        icon.into_any_element()
    }
}

pub(super) fn work_status_color(status: BackgroundWorkStatus, theme: Theme) -> Hsla {
    match status {
        BackgroundWorkStatus::Starting
        | BackgroundWorkStatus::Running
        | BackgroundWorkStatus::Monitoring => theme.accent,
        BackgroundWorkStatus::Completed => theme.success,
        BackgroundWorkStatus::Failed | BackgroundWorkStatus::Lost => theme.danger,
        BackgroundWorkStatus::Stopping | BackgroundWorkStatus::Stopped => theme.text_tertiary,
    }
}

pub(super) fn work_kind_icon(kind: BackgroundWorkKind) -> &'static str {
    match kind {
        BackgroundWorkKind::Subagent => "icons/bot.svg",
        BackgroundWorkKind::Process | BackgroundWorkKind::Monitor => "icons/terminal-square.svg",
    }
}

fn work_elapsed(item: &BackgroundWorkItem) -> String {
    let duration_ms = item.duration_ms.unwrap_or_else(|| {
        let end = if item.status.is_live() {
            unix_time_millis()
        } else {
            item.updated_at_ms
        };
        end.saturating_sub(item.started_at_ms)
    });
    let seconds = duration_ms / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 60 * 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

impl Fintwind {
    pub(super) fn background_output_refresh_delay(&self) -> Option<Duration> {
        self.background_work
            .values()
            .filter_map(BackgroundWorkRegistry::output_refresh_delay)
            .min()
    }

    pub(super) fn observe_foreground_command_activity(
        &mut self,
        session_id: Uuid,
        activity: &ActivityItem,
    ) {
        if activity.kind != crate::model::ActivityKind::Command {
            return;
        }
        let provider_id = activity
            .source_id
            .clone()
            .unwrap_or_else(|| activity.id.to_string());
        let status = if !activity.complete {
            BackgroundWorkStatus::Running
        } else if activity.failed {
            BackgroundWorkStatus::Failed
        } else {
            BackgroundWorkStatus::Completed
        };
        let mut item = BackgroundWorkItem::new(
            BackgroundWorkKind::Process,
            provider_id.clone(),
            activity.title.clone(),
            status,
        );
        item.command = activity.display_target.clone();
        item.detail = activity.detail.clone();
        item.output = activity.output.clone();
        item.origin_activity_ids = vec![provider_id];
        self.handle_background_work_event(session_id, BackgroundWorkEvent::Upsert(item));
    }

    pub(super) fn handle_background_work_event(
        &mut self,
        session_id: Uuid,
        mut event: BackgroundWorkEvent,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        {
            match &mut event {
                // A live upsert from the driver already names every call that
                // bound the child, so only a card that arrived with no origin
                // at all needs the transcript scan. Reconciled items are rebuilt
                // fresh and empty, so they always scan; a settled snapshot is
                // repaired once by `restore_background_work`.
                BackgroundWorkEvent::Upsert(item) => {
                    if item.origin_activity_ids.is_empty() {
                        recover_subagent_origin(session, item);
                    }
                }
                BackgroundWorkEvent::ReconcileLive { items } => {
                    for item in items {
                        recover_subagent_origin(session, item);
                    }
                }
                _ => {}
            }
        }
        let snapshot_key = match &event {
            BackgroundWorkEvent::Transcript(
                BackgroundWorkTranscriptEvent::Finished { key, .. }
                | BackgroundWorkTranscriptEvent::Snapshot { key, .. },
            ) => Some(key.clone()),
            _ => None,
        };
        let registry = self.background_work.entry(session_id).or_default();
        registry.apply(event);
        let snapshot = snapshot_key.and_then(|key| registry.snapshot(&key));
        if let Some(snapshot) = snapshot
            && let Some(session) = self.state.session_mut(session_id)
        {
            session
                .background_work
                .retain(|stored| stored.item.key != snapshot.item.key);
            session.background_work.push(snapshot);
            let excess = session
                .background_work
                .len()
                .saturating_sub(MAX_SETTLED_BACKGROUND_ITEMS);
            session.background_work.drain(..excess);
        }
    }

    pub(super) fn restore_background_work(&mut self, session_id: Uuid) {
        if self.background_work.contains_key(&session_id) {
            return;
        }
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .filter(|session| !session.background_work.is_empty())
        else {
            return;
        };
        let mut registry = BackgroundWorkRegistry::from_snapshots(&session.background_work);
        // A snapshot written before the multi-call origin field, or against a
        // transcript that had dropped call ids, carries no way back to its
        // card. The stored transcript still names the child, so re-derive the
        // links now rather than leaving a settled subagent unopenable.
        for item in registry.items.values_mut() {
            recover_subagent_origin(session, item);
        }
        self.background_work.insert(session_id, registry);
    }

    pub(super) fn mark_background_work_lost(&mut self, session_id: Uuid) {
        if let Some(registry) = self.background_work.get_mut(&session_id) {
            registry.mark_live_lost();
        }
    }

    pub(super) fn settle_foreground_work(
        &mut self,
        session_id: Uuid,
        status: BackgroundWorkStatus,
    ) {
        if let Some(registry) = self.background_work.get_mut(&session_id) {
            registry.settle_foreground(status);
        }
    }

    pub(super) fn session_has_live_background_work(&self, session_id: Uuid) -> bool {
        self.background_work
            .get(&session_id)
            .is_some_and(BackgroundWorkRegistry::has_live)
    }

    pub(super) fn background_work_counts(&self, session_id: Uuid) -> (usize, usize) {
        self.background_work
            .get(&session_id)
            .map(BackgroundWorkRegistry::counts)
            .unwrap_or_default()
    }

    pub(super) fn background_work_for_activity(
        &self,
        session_id: Uuid,
        activity_id: &str,
    ) -> Option<&BackgroundWorkItem> {
        self.background_work
            .get(&session_id)?
            .items
            .values()
            .find(|item| item.origin_activity_ids.iter().any(|id| id == activity_id))
    }

    pub(super) fn maybe_refresh_background_work(&mut self, cx: &mut Context<Self>) {
        let mut output_changed = false;
        for registry in self.background_work.values_mut() {
            output_changed |= registry.refresh_output_cache();
        }
        if output_changed {
            cx.notify();
        }
        let selected = self.state.selected_session;
        for (session_id, runtime) in &mut self.runtimes {
            let should_refresh = selected == Some(*session_id)
                || self
                    .background_work
                    .get(session_id)
                    .is_some_and(BackgroundWorkRegistry::has_live);
            if should_refresh
                && runtime.last_background_refresh_at.elapsed() >= BACKGROUND_WORK_REFRESH_INTERVAL
            {
                runtime.last_background_refresh_at = Instant::now();
                runtime.driver.refresh_background_work();
            }
        }

        if self.last_background_work_tick.elapsed() >= BACKGROUND_WORK_TICK_INTERVAL
            && selected.is_some_and(|session_id| self.session_has_live_background_work(session_id))
        {
            self.last_background_work_tick = Instant::now();
            cx.notify();
        }
    }

    pub(super) fn stop_background_work(
        &mut self,
        session_id: Uuid,
        key: BackgroundWorkKey,
        cx: &mut Context<Self>,
    ) {
        let control_id = self
            .background_work
            .get(&session_id)
            .and_then(|registry| registry.items.get(&key))
            .filter(|item| item.status.is_stoppable() && item.can_stop)
            .and_then(|item| item.control_id.clone());
        let Some(control_id) = control_id else {
            return;
        };
        let Some(driver) = self
            .runtimes
            .get(&session_id)
            .map(|runtime| runtime.driver.clone())
        else {
            self.mark_background_work_lost(session_id);
            cx.notify();
            return;
        };
        self.handle_background_work_event(
            session_id,
            BackgroundWorkEvent::StopRequested(key.clone()),
        );
        driver.stop_background_work(key, control_id);
        cx.notify();
    }

    pub(super) fn open_background_work_surface(
        &mut self,
        session_id: Uuid,
        key: BackgroundWorkKey,
        cx: &mut Context<Self>,
    ) {
        let Some(title) = self
            .background_work
            .get(&session_id)
            .and_then(|registry| registry.items.get(&key))
            .map(|item| item.title.clone())
        else {
            return;
        };
        if self.state.selected_session != Some(session_id) {
            self.select_session(session_id, cx);
        }
        self.open_right_panel_surface(RightPanelSurface::BackgroundWork { key, title }, cx);
    }

    pub(super) fn todo_summary(&self, session_id: Option<Uuid>) -> Rc<TodoSummary> {
        session_id
            .and_then(|session_id| self.todo_summaries.borrow().get(&session_id).cloned())
            .unwrap_or_default()
    }

    /// Rebuilds the session's todo display model from its transcript blocks.
    /// Returns whether anything changed so event handlers can decide to
    /// repaint. Render must call [`Self::todo_summary`] instead — this walks
    /// the whole session.
    pub(super) fn rebuild_todo_summary(&mut self, session_id: Uuid) -> bool {
        let summary = Rc::new(
            self.state
                .session_mut(session_id)
                .map(|session| todo_summary_from_blocks(&session.transcript_blocks))
                .unwrap_or_default(),
        );
        let changed = self
            .todo_summaries
            .borrow()
            .get(&session_id)
            .is_none_or(|previous| **previous != *summary);
        self.todo_summaries.borrow_mut().insert(session_id, summary);
        changed
    }

    /// Floating tool capsule over the session pane: collapsed it shows the
    /// git change totals; expanded it lists Git tools, todos, subagents, and
    /// background work for the selected session.
    pub(super) fn render_task_capsule(&self, cx: &mut Context<Self>) -> AnyElement {
        let session = self.selected_session();
        let session_id = session.map(|session| session.id);
        let entries = session_id
            .and_then(|session_id| self.background_work.get(&session_id))
            .map(|registry| {
                registry
                    .ordered_items()
                    .into_iter()
                    .cloned()
                    .map(|item| {
                        let kind = item.key.kind as u8;
                        let provider_id = &item.key.provider_id;
                        BackgroundSummaryEntry {
                            row_focus: self.transcript_control_focus(
                                format!("background-summary-row-{provider_id}-{kind}"),
                                cx,
                            ),
                            stop_focus: self.transcript_control_focus(
                                format!("background-summary-stop-{provider_id}-{kind}"),
                                cx,
                            ),
                            item,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let workspace_path = session
            .and_then(|session| self.workspace_path_for_session(session))
            .or_else(|| {
                self.selected_project()
                    .map(|project| project.path.as_path())
            });
        let snapshot = workspace_path.and_then(|path| {
            self.visible_branch_snapshot
                .as_ref()
                .filter(|(snapshot_path, _)| snapshot_path == path)
                .map(|(_, snapshot)| snapshot)
        });
        let todo = self.todo_summary(session_id);
        let (additions, deletions) = snapshot
            .map(|snapshot| (snapshot.additions, snapshot.deletions))
            .unwrap_or_default();
        let has_changes = additions > 0 || deletions > 0;
        let environment = Some(EnvironmentSummary {
            branch: snapshot
                .and_then(|snapshot| snapshot.display_branch())
                .map(ToString::to_string),
            additions,
            deletions,
            has_changes,
            commit_status: self.commit_operation_status_label(),
            changes_focus: self.transcript_control_focus("environment-summary-changes", cx),
            commit_focus: self.transcript_control_focus("environment-summary-commit", cx),
            compare_focus: self.transcript_control_focus("environment-summary-compare", cx),
        });
        let (processes, agents) = session_id
            .map(|session_id| self.background_work_counts(session_id))
            .unwrap_or_default();
        let live_summary = background_work_count_summary(processes, agents);
        let mut tooltip_parts = Vec::new();
        if has_changes {
            tooltip_parts.push(tr!("environment.changes"));
        }
        if !live_summary.is_empty() {
            tooltip_parts.push(live_summary);
        }
        let tooltip = if tooltip_parts.is_empty() {
            tr!("capsule.tasks")
        } else {
            tooltip_parts.join(" · ")
        };
        let theme = Theme::current(cx);
        let refresh_weak = cx.entity().downgrade();
        let handle = self.menu_handle_with(BACKGROUND_SUMMARY_MENU_ID, cx, move |open, _, cx| {
            if open {
                let _ = refresh_weak.update(cx, |this, cx| {
                    this.refresh_selected_branch_snapshot(cx);
                });
            }
        });
        let trigger = div()
            .id("task-capsule-trigger")
            .h(px(28.0))
            .px(px(11.0))
            .rounded_full()
            .border_1()
            .border_color(if handle.is_open() {
                theme.accent
            } else {
                theme.border_strong
            })
            .bg(theme.raised)
            .shadow_xs()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(ui_px(12.0))
            .font_weight(FontWeight::MEDIUM)
            .focus_visible(|style| style.border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay))
            .active(|style| style.bg(theme.overlay_strong))
            .tooltip(Tooltip::text(tooltip))
            .child(icon(
                "icons/git-branch.svg",
                13.0,
                if handle.is_open() {
                    theme.accent
                } else {
                    theme.text_secondary
                },
            ))
            .when(additions > 0, |trigger| {
                trigger.child(
                    div()
                        .text_color(theme.success)
                        .child(format!("+{additions}")),
                )
            })
            .when(deletions > 0, |trigger| {
                trigger.child(
                    div()
                        .text_color(theme.danger)
                        .child(format!("-{deletions}")),
                )
            });
        let entries = Rc::new(entries);
        let weak = cx.entity().downgrade();
        div()
            .absolute()
            .top(px(56.0))
            .right(px(16.0))
            .child(popover(
                trigger,
                &handle,
                MenuAlign::BelowRight,
                move |handle, _, cx| {
                    render_task_capsule_card(
                        handle,
                        session_id.unwrap_or_else(Uuid::nil),
                        environment.clone(),
                        todo.clone(),
                        entries.clone(),
                        weak.clone(),
                        cx,
                    )
                },
            ))
            .into_any_element()
    }

    pub(super) fn render_background_work_surface(
        &self,
        key: &BackgroundWorkKey,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let session_id = self.state.selected_session;
        let registry = session_id.and_then(|session_id| self.background_work.get(&session_id));
        let item = registry.and_then(|registry| registry.items.get(key));
        let Some(item) = item else {
            return div()
                .id("background-work-surface")
                .tab_group()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(7.0))
                        .child(icon(work_kind_icon(key.kind), 22.0, theme.text_ghost))
                        .child(
                            div()
                                .text_size(ui_px(12.0))
                                .text_color(theme.text_secondary)
                                .child(tr!("background.no_work")),
                        ),
                );
        };
        let output = registry
            .and_then(|registry| registry.rendered_output.get(key))
            .cloned();
        let output_viewport = registry
            .and_then(|registry| registry.output_viewports.get(key))
            .cloned()
            .unwrap_or_default();
        let selection = registry
            .map(|registry| registry.selection.clone())
            .unwrap_or_default();
        let status_color = work_status_color(item.status, theme);
        let stop = session_id.and_then(|session_id| {
            (item.status.is_stoppable() && item.can_stop).then(|| {
                let focus = self.transcript_control_focus(
                    format!(
                        "background-surface-stop-{}-{}",
                        item.key.provider_id, item.key.kind as u8
                    ),
                    cx,
                );
                let click_key = item.key.clone();
                let click_weak = cx.entity().downgrade();
                let key_key = item.key.clone();
                let key_weak = cx.entity().downgrade();
                div()
                    .id(SharedString::from(format!(
                        "background-surface-stop-{}-{}",
                        item.key.provider_id, item.key.kind as u8
                    )))
                    .track_focus(&focus)
                    .tab_index(0)
                    .h(px(30.0))
                    .px(px(10.0))
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_default()
                    .text_size(ui_px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_secondary)
                    .hover(|style| style.bg(theme.danger.opacity(0.10)))
                    .active(|style| style.bg(theme.danger.opacity(0.16)))
                    .focus_visible(|style| style.border_color(theme.accent))
                    .tooltip(Tooltip::text(tr!("background.stop")))
                    .child(icon("icons/stop-filled.svg", 12.5, theme.danger))
                    .child(tr!("background.stop"))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        let _ = click_weak.update(cx, |this, cx| {
                            this.stop_background_work(session_id, click_key.clone(), cx);
                        });
                    })
                    .on_key_down(move |event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            let _ = key_weak.update(cx, |this, cx| {
                                this.stop_background_work(session_id, key_key.clone(), cx);
                            });
                            cx.stop_propagation();
                        }
                    })
            })
        });
        if item.key.kind == BackgroundWorkKind::Subagent {
            let transcript = registry.and_then(|registry| registry.transcripts.get(key));
            return div()
                .id("background-work-surface")
                .tab_group()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(theme.surface)
                .child(
                    div()
                        .h(px(52.0))
                        .px(px(14.0))
                        .flex_none()
                        .border_b_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .gap(px(9.0))
                        .child(icon("icons/bot.svg", 15.0, theme.text_secondary))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(ui_px(13.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.text)
                                        .child(item.title.clone()),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(5.0))
                                        .text_size(ui_px(10.5))
                                        .text_color(status_color)
                                        .child(rendered_work_status_icon(
                                            item.status,
                                            9.0,
                                            status_color,
                                        ))
                                        .child(work_status_label(item.status)),
                                ),
                        )
                        .when_some(stop, |header, stop| header.child(stop)),
                )
                .child(
                    div()
                        .id("background-transcript-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px(px(16.0))
                        .py(px(12.0))
                        .children(transcript.map(|transcript| {
                            self.render_background_transcript(
                                item,
                                transcript,
                                selection.clone(),
                                cx,
                            )
                        })),
                );
        }
        let card = div()
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(9.0))
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .bg(theme.surface)
            .child(
                div()
                    .min_h(px(54.0))
                    .px(px(11.0))
                    .py(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .child(icon(
                        work_kind_icon(item.key.kind),
                        15.0,
                        theme.text_secondary,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui_px(12.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(item.title.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .text_size(ui_px(10.0))
                                    .text_color(theme.text_tertiary)
                                    .child(rendered_work_status_icon(
                                        item.status,
                                        9.0,
                                        status_color,
                                    ))
                                    .child(work_status_label(item.status))
                                    .child("·")
                                    .child(work_elapsed(item)),
                            ),
                    )
                    .when_some(stop, |header, stop| header.child(stop)),
            )
            .child(self.render_background_work_detail(
                item,
                output,
                output_viewport,
                selection,
                cx,
            ));
        div()
            .id("background-work-surface")
            .tab_group()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(12.0))
            .child(card)
    }

    fn render_background_work_detail(
        &self,
        item: &BackgroundWorkItem,
        output: Option<SharedString>,
        output_viewport: BackgroundOutputViewport,
        selection: TranscriptSelection,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let mut detail = div().w_full().flex().flex_col().bg(theme.surface);
        let mut metadata = Vec::new();
        for (label, value) in [
            (tr!("background.command"), item.command.as_ref()),
            (tr!("background.cwd"), item.cwd.as_ref()),
            (tr!("background.role"), item.role.as_ref()),
            (tr!("background.model"), item.model.as_ref()),
            (tr!("background.latest_update"), item.detail.as_ref()),
        ] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                metadata.push((label, value.clone()));
            }
        }
        if let Some(exit_code) = item.exit_code {
            metadata.push((tr!("background.exit_code"), exit_code.to_string()));
        }
        for (label, value) in metadata {
            detail = detail.child(
                div()
                    .border_t_1()
                    .border_color(theme.border)
                    .px(px(10.0))
                    .py(px(7.0))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_size(ui_px(9.5))
                            .text_color(theme.text_tertiary)
                            .child(label),
                    )
                    .child(
                        div()
                            .text_size(code_px(10.5))
                            .font_family(md::render::mono_family())
                            .text_color(theme.text_secondary)
                            .child(value),
                    ),
            );
        }
        let output = output.unwrap_or_else(|| SharedString::from(tr!("background.no_output")));
        let output_flat = md::render::flatten_plain(
            output,
            md::render::mono_family(),
            FontWeight::NORMAL,
            theme.text_secondary,
        );
        let output_text = md::render::selectable_flat_text(
            &output_flat,
            crate::md::selection::TextKey::new(
                format!(
                    "background-output-{}-{}",
                    item.key.provider_id, item.key.kind as u8
                ),
                0,
            ),
            selection.clone(),
            theme.code_wash,
            theme.selection,
            false,
        );
        detail.child(
            div()
                .border_t_1()
                .border_color(theme.border)
                .p(px(10.0))
                .flex()
                .flex_col()
                .gap(px(5.0))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_size(ui_px(9.5))
                        .text_color(theme.text_tertiary)
                        .child(tr!("background.output"))
                        .when(item.output_truncated, |header| {
                            header.child(tr!("background.output_truncated"))
                        }),
                )
                .child(
                    div()
                        .relative()
                        .max_h(px(320.0))
                        .rounded(px(6.0))
                        .overflow_hidden()
                        .bg(theme.terminal)
                        .child(md::render::frame_reset(selection.clone()))
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "background-output-scroll-{}-{}",
                                    item.key.provider_id, item.key.kind as u8
                                )))
                                .max_h(px(320.0))
                                .overflow_y_scroll()
                                .track_scroll(&output_viewport.scroll_handle)
                                .on_scroll_wheel({
                                    let scroll = output_viewport.scroll_handle.clone();
                                    move |_, _, cx| contain_scroll(&scroll, cx)
                                })
                                .p(px(8.0))
                                .text_size(code_px(10.5))
                                .line_height(code_px(15.0))
                                .font_family(md::render::mono_family())
                                .text_color(theme.text_secondary)
                                .child(output_text),
                        )
                        .child(scrollbar::vertical(
                            &output_viewport.scroll_handle,
                            &output_viewport.scrollbar,
                        ))
                        .child(background_work_selection_input(selection)),
                ),
        )
    }

    fn render_background_transcript(
        &self,
        item: &BackgroundWorkItem,
        transcript: &BackgroundWorkTranscript,
        selection: TranscriptSelection,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let palette = MarkdownPalette::from_theme(&theme);
        let mut content = div().w_full().min_w_0().flex().flex_col().gap(px(8.0));
        let render_message_row = |message: &Message, cx: &mut Context<Self>| {
            let ctx = MarkdownCtx::new(
                format!("background-message-{}-{}", item.key.provider_id, message.id),
                &palette,
                if message.role == MessageRole::User {
                    MarkdownMetrics::user_message()
                } else {
                    MarkdownMetrics::body()
                },
                selection.clone(),
            )
            .with_link_handler(self.markdown_link_handler.clone())
            .with_streaming_animation(message.streaming && !cx.reduce_motion());
            let menu = self.menu_handle(format!("background-message-{}", message.id), cx);
            let mut markdown_views = self.message_markdown.borrow_mut();
            let view = markdown_views.entry(message.id).or_default();
            view.set_text(message.visible_content(), message.streaming);
            render_message(
                MessageRender {
                    theme: &theme,
                    message,
                    assistant_footer_copy_content: None,
                    assistant_footer_time: None,
                    assistant_before_footer: None,
                    assistant_turn_stats: None,
                    copied: false,
                    assistant_message_action: None,
                    user_message_rewind: UserMessageRewind::Hidden,
                    user_message_fill_width: true,
                    message_edit_input: None,
                    attachment_menus: Vec::new(),
                    attachment_images: Vec::new(),
                    attachments_can_reveal: false,
                    markdown: Some(view),
                    ctx: &ctx,
                    compaction_expanded: false,
                    compaction_focus: None,
                    menu,
                    fintwind: cx.entity().downgrade(),
                    composer: self.composer.clone(),
                },
                cx,
            )
        };
        // Group blocks by their anchor once per frame; the per-message filter
        // below then costs one lookup instead of a full block scan.
        let mut blocks_by_after: HashMap<usize, Vec<&TranscriptBlock>> = HashMap::new();
        for block in &transcript.transcript_blocks {
            blocks_by_after
                .entry(block.after_message)
                .or_default()
                .push(block);
        }
        for after_message in 0..=transcript.messages.len() {
            for block in blocks_by_after.get(&after_message).into_iter().flatten() {
                if block.activities.is_empty() {
                    continue;
                }
                let disclosure_id = block.activities[0].id;
                let expanded = self
                    .expanded_activity_items
                    .get(&disclosure_id)
                    .copied()
                    .unwrap_or(false);
                let click_weak = cx.entity().downgrade();
                let key_weak = cx.entity().downgrade();
                let focus = self
                    .transcript_control_focus(format!("background-activity-{disclosure_id}"), cx);
                let mut cluster = div().w_full().min_w_0().flex().flex_col().child(
                    div()
                        .id(SharedString::from(format!(
                            "background-activity-{disclosure_id}"
                        )))
                        .track_focus(&focus)
                        .tab_index(0)
                        .h(px(30.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .cursor_default()
                        .text_size(ui_px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_secondary)
                        .focus_visible(|style| style.text_color(theme.text))
                        .hover(|style| style.text_color(theme.text))
                        .child(activity_header_title(&block.activities, false, None))
                        .child(icon(
                            if expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            },
                            11.0,
                            theme.text_tertiary,
                        ))
                        .on_click(move |_, _, cx| {
                            let _ = click_weak.update(cx, |this, cx| {
                                this.expanded_activity_items
                                    .insert(disclosure_id, !expanded);
                                cx.notify();
                            });
                        })
                        .on_key_down(move |event: &KeyDownEvent, _, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                let _ = key_weak.update(cx, |this, cx| {
                                    this.expanded_activity_items
                                        .insert(disclosure_id, !expanded);
                                    cx.notify();
                                });
                                cx.stop_propagation();
                            }
                        }),
                );
                if expanded {
                    let mut activities = div()
                        .ml(px(6.0))
                        .pl(px(12.0))
                        .border_l_1()
                        .border_color(theme.border)
                        .flex()
                        .flex_col()
                        .gap(px(6.0));
                    for activity in &block.activities {
                        let color = if activity.failed {
                            theme.danger
                        } else if activity.complete {
                            theme.text_tertiary
                        } else {
                            theme.accent
                        };
                        let detail = activity_row_detail(activity, false);
                        activities = activities.child(
                            div()
                                .min_h(px(28.0))
                                .flex()
                                .items_center()
                                .gap(px(7.0))
                                .text_size(ui_px(11.5))
                                .text_color(theme.text_secondary)
                                .child(
                                    div()
                                        .flex_none()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(color)
                                        .child(activity_action_label(activity)),
                                )
                                .when(!detail.is_empty(), |row| {
                                    row.child(div().min_w_0().flex_1().truncate().child(detail))
                                })
                                .when(activity.failed, |row| {
                                    row.child(icon("icons/x.svg", 10.0, theme.danger))
                                })
                                .when(!activity.complete && !activity.failed, |row| {
                                    row.child(pulse_dot(5.0, theme.accent))
                                }),
                        );
                    }
                    cluster = cluster.child(activities);
                }
                content = content.child(cluster);
            }
            if let Some(message) = transcript.messages.get(after_message) {
                content = content.child(render_message_row(message, cx));
            }
        }
        content.child(background_work_selection_input(selection))
    }
}

fn background_work_selection_input(selection: TranscriptSelection) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, _, window, _| md::render::install_selection_input(window, &selection),
    )
    .absolute()
    .w(px(0.0))
    .h(px(0.0))
}

fn background_work_count_summary(processes: usize, agents: usize) -> String {
    let mut parts = Vec::new();
    if processes > 0 {
        parts.push(if processes == 1 {
            tr!("background.process_count_one")
        } else {
            tr!("background.process_count", count = processes)
        });
    }
    if agents > 0 {
        parts.push(if agents == 1 {
            tr!("background.agent_count_one")
        } else {
            tr!("background.agent_count", count = agents)
        });
    }
    parts.join(" · ")
}

fn render_task_capsule_card(
    handle: &ContextMenuHandle,
    session_id: Uuid,
    environment: Option<EnvironmentSummary>,
    todo: Rc<TodoSummary>,
    entries: Rc<Vec<BackgroundSummaryEntry>>,
    weak: WeakEntity<Fintwind>,
    cx: &mut App,
) -> AnyElement {
    let theme = Theme::current(cx);
    let processes = entries
        .iter()
        .filter(|entry| entry.item.key.kind != BackgroundWorkKind::Subagent)
        .cloned()
        .collect::<Vec<_>>();
    let agents = entries
        .iter()
        .filter(|entry| entry.item.key.kind == BackgroundWorkKind::Subagent)
        .cloned()
        .collect::<Vec<_>>();
    let separator = || div().mx(px(8.0)).h(px(1.0)).bg(theme.border);
    let git_tools = environment
        .map(|environment| {
            render_git_tools_section(environment, handle.clone(), weak.clone(), &theme)
        })
        .unwrap_or_else(|| {
            div()
                .h(px(26.0))
                .px(px(8.0))
                .flex()
                .items_center()
                .text_size(ui_px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text_tertiary)
                .child(tr!("capsule.git_tools"))
        });
    let content = div()
        .id("task-capsule-scroll")
        .max_h(px(480.0))
        .overflow_y_scroll()
        .p(px(8.0))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(git_tools)
        .child(separator())
        .child(render_todo_section(&todo, &theme))
        .child(separator())
        .child(render_background_summary_section(
            tr!("background.agents"),
            agents,
            session_id,
            handle.clone(),
            weak.clone(),
            &theme,
            tr!("background.no_agents"),
        ))
        .child(separator())
        .child(render_background_summary_section(
            tr!("background.title"),
            processes,
            session_id,
            handle.clone(),
            weak,
            &theme,
            tr!("background.no_work"),
        ));
    div()
        .id("task-capsule-card")
        .track_focus(handle.focus_handle())
        .w(px(300.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(theme.border_strong)
        .overflow_hidden()
        .bg(theme.raised)
        .shadow_lg()
        .child(content)
        .into_any_element()
}

fn render_capsule_section_header(id: &'static str, label: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(26.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .text_size(ui_px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text_tertiary)
        .child(label)
}

fn render_git_tools_section(
    environment: EnvironmentSummary,
    handle: ContextMenuHandle,
    weak: WeakEntity<Fintwind>,
    theme: &Theme,
) -> Div {
    let counts = (environment.has_changes).then(|| {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(ui_px(11.5))
            .font_weight(FontWeight::MEDIUM)
            .when(environment.additions > 0, |counts| {
                counts.child(
                    div()
                        .text_color(theme.success)
                        .child(format!("+{}", environment.additions)),
                )
            })
            .when(environment.deletions > 0, |counts| {
                counts.child(
                    div()
                        .text_color(theme.danger)
                        .child(format!("-{}", environment.deletions)),
                )
            })
            .into_any_element()
    });
    let changes_handle = handle.clone();
    let changes_weak = weak.clone();
    let changes = render_environment_action_row(
        "environment-summary-changes",
        &environment.changes_focus,
        "icons/file-diff.svg",
        tr!("environment.changes"),
        environment.has_changes,
        false,
        counts,
        theme,
        move |window, cx| {
            changes_handle.close(window, cx);
            window.refresh();
            let _ = changes_weak.update(cx, |this, cx| {
                this.set_right_panel_diff_source(ReviewDiffSource::Uncommitted, cx);
            });
        },
    );
    let branch = render_capsule_info_row(
        "environment-summary-branch",
        "icons/git-branch.svg",
        environment.branch.unwrap_or_else(|| "—".to_owned()),
        theme,
    );

    let commit_handle = handle.clone();
    let commit_weak = weak.clone();
    let commit_pending = environment.commit_status.is_some();
    let commit = render_environment_action_row(
        "environment-summary-commit",
        &environment.commit_focus,
        "icons/git-commit-horizontal.svg",
        environment
            .commit_status
            .unwrap_or_else(|| tr!("environment.commit_or_push")),
        !commit_pending,
        commit_pending,
        None,
        theme,
        move |window, cx| {
            commit_handle.close(window, cx);
            window.refresh();
            let _ = commit_weak.update(cx, |this, cx| {
                this.open_commit_dialog(window, cx);
            });
        },
    );

    let compare_handle = handle;
    let compare_weak = weak;
    let compare = render_environment_action_row(
        "environment-summary-compare",
        &environment.compare_focus,
        "icons/github.svg",
        tr!("environment.compare_branch"),
        true,
        false,
        Some(icon("icons/arrow-up-right.svg", 13.0, theme.text_tertiary).into_any_element()),
        theme,
        move |window, cx| {
            compare_handle.close(window, cx);
            window.refresh();
            let _ = compare_weak.update(cx, |this, cx| {
                this.set_right_panel_diff_source(ReviewDiffSource::Branch, cx);
            });
        },
    );

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_0()
        .child(render_capsule_section_header(
            "task-capsule-git-header",
            tr!("capsule.git_tools"),
            theme,
        ))
        .child(changes)
        .child(branch)
        .child(commit)
        .child(compare)
}

/// A read-only informational row: icon plus a single-line label.
fn render_capsule_info_row(
    id: &'static str,
    icon_path: &'static str,
    label: String,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .min_h(px(32.0))
        .w_full()
        .px(px(8.0))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(icon(icon_path, 14.0, theme.text_secondary))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_size(ui_px(13.5))
                .text_color(theme.text_secondary)
                .child(label),
        )
}

fn render_todo_section(todo: &TodoSummary, theme: &Theme) -> Div {
    let mut rows = div().w_full().flex().flex_col().gap(px(1.0));
    if todo.entries.is_empty() {
        rows = rows.child(
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(ui_px(12.0))
                .text_color(theme.text_tertiary)
                .child(tr!("capsule.todo_empty")),
        );
    } else {
        for (index, entry) in todo.entries.iter().enumerate() {
            rows = rows.child(render_todo_row(index, entry, theme));
        }
    }
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(
            div()
                .id("task-capsule-todo-header")
                .h(px(26.0))
                .px(px(8.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(ui_px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_tertiary)
                        .child(tr!("capsule.todo")),
                )
                .children(todo.updating.then(|| {
                    motion::spin_slow(icon("icons/loader-circle.svg", 10.0, theme.text_tertiary))
                })),
        )
        .child(rows)
}

fn render_todo_row(index: usize, entry: &TodoEntry, theme: &Theme) -> Stateful<Div> {
    let (marker, label_color) = match entry.state {
        TodoEntryState::Pending => (
            div()
                .size(px(10.0))
                .rounded_full()
                .border_1()
                .border_color(theme.border_strong)
                .into_any_element(),
            theme.text_secondary,
        ),
        TodoEntryState::InProgress => (
            motion::spin_slow(icon("icons/loader-circle.svg", 12.0, theme.accent)),
            theme.text,
        ),
        TodoEntryState::Completed => (
            icon("icons/check.svg", 12.0, theme.success).into_any_element(),
            theme.text_tertiary,
        ),
        TodoEntryState::Failed => (
            icon("icons/x.svg", 12.0, theme.danger).into_any_element(),
            theme.text_tertiary,
        ),
    };
    div()
        .id(SharedString::from(format!("task-capsule-todo-row-{index}")))
        .min_h(px(26.0))
        .w_full()
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .gap(px(9.0))
        .child(
            div()
                .size(px(12.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(marker),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .line_clamp(1)
                .text_ellipsis()
                .text_size(ui_px(12.5))
                .text_color(label_color)
                .child(entry.label.clone()),
        )
        .tooltip(Tooltip::text(entry.state.label()))
}

fn render_environment_action_row(
    id: &'static str,
    focus: &FocusHandle,
    icon_path: &'static str,
    label: String,
    enabled: bool,
    active: bool,
    trailing: Option<AnyElement>,
    theme: &Theme,
    action: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let foreground = if enabled {
        theme.text
    } else if active {
        theme.text_secondary
    } else {
        theme.text_ghost
    };
    let icon_foreground = if enabled || active {
        theme.text_secondary
    } else {
        theme.text_ghost
    };
    let indicator = if active {
        motion::spin_slow(icon("icons/loader-circle.svg", 14.0, theme.text_secondary))
    } else {
        icon(icon_path, 14.0, icon_foreground).into_any_element()
    };
    let action: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(action);
    let key_action = action.clone();
    div()
        .id(id)
        .track_focus(focus)
        .when(enabled, |row| row.tab_index(0))
        .min_h(px(32.0))
        .w_full()
        .px(px(8.0))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .cursor_default()
        .focus_visible(|style| style.border_1().border_color(theme.accent))
        .when(enabled, |row| {
            row.hover(|style| style.bg(theme.overlay_strong))
        })
        .child(indicator)
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_size(ui_px(13.5))
                .text_color(foreground)
                .child(label),
        )
        .children(trailing)
        .when(enabled, |row| {
            row.on_click(move |_, window, cx| action(window, cx))
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        key_action(window, cx);
                        cx.stop_propagation();
                    }
                })
        })
}

fn render_background_summary_section(
    label: String,
    entries: Vec<BackgroundSummaryEntry>,
    session_id: Uuid,
    handle: ContextMenuHandle,
    weak: WeakEntity<Fintwind>,
    theme: &Theme,
    empty_label: String,
) -> Div {
    let mut rows = div().w_full().flex().flex_col().gap(px(2.0));
    if entries.is_empty() {
        rows = rows.child(
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(ui_px(12.0))
                .text_color(theme.text_tertiary)
                .child(empty_label),
        );
    }
    for entry in entries {
        rows = rows.child(render_background_summary_row(
            entry,
            session_id,
            handle.clone(),
            weak.clone(),
            theme,
        ));
    }
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(
            div()
                .px(px(8.0))
                .text_size(ui_px(13.0))
                .text_color(theme.text_tertiary)
                .child(label),
        )
        .child(rows)
}

fn render_background_summary_row(
    entry: BackgroundSummaryEntry,
    session_id: Uuid,
    handle: ContextMenuHandle,
    weak: WeakEntity<Fintwind>,
    theme: &Theme,
) -> Stateful<Div> {
    let item = entry.item;
    let group_name = SharedString::from(format!(
        "background-summary-group-{}-{}",
        item.key.provider_id, item.key.kind as u8
    ));
    let status = background_summary_process_status_icon(item.key.kind, item.status).map(|_| {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .when(item.status.is_stoppable() && item.can_stop, |status| {
                status.group_hover(group_name.clone(), |style| style.invisible())
            })
            .child(rendered_work_status_icon(
                item.status,
                12.0,
                work_status_color(item.status, *theme),
            ))
    });
    let stop = (item.status.is_stoppable() && item.can_stop).then(|| {
        let click_key = item.key.clone();
        let click_weak = weak.clone();
        let key_key = item.key.clone();
        let key_weak = weak.clone();
        div()
            .id(SharedString::from(format!(
                "background-summary-stop-{}-{}",
                item.key.provider_id, item.key.kind as u8
            )))
            .track_focus(&entry.stop_focus)
            .tab_index(0)
            .size(px(24.0))
            .rounded(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .opacity(0.0)
            .group_hover(group_name.clone(), |style| style.opacity(1.0))
            .hover(|style| style.bg(theme.overlay_strong))
            .focus_visible(|style| {
                style
                    .opacity(1.0)
                    .bg(theme.raised)
                    .border_1()
                    .border_color(theme.accent)
            })
            .tooltip(Tooltip::text(tr!("background.stop")))
            .child(icon("icons/stop-filled.svg", 12.0, theme.text_tertiary))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                let _ = click_weak.update(cx, |this, cx| {
                    this.stop_background_work(session_id, click_key.clone(), cx);
                });
            })
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    let _ = key_weak.update(cx, |this, cx| {
                        this.stop_background_work(session_id, key_key.clone(), cx);
                    });
                    cx.stop_propagation();
                }
            })
    });
    let trailing = (status.is_some() || stop.is_some()).then(|| {
        div()
            .relative()
            .size(px(24.0))
            .flex_none()
            .children(status)
            .children(stop)
    });
    let is_process = item.key.kind != BackgroundWorkKind::Subagent;
    let open_key = item.key.clone();
    let key_key = open_key.clone();
    let click_handle = handle.clone();
    let click_weak = weak.clone();
    let key_handle = handle;
    let key_weak = weak;
    div()
        .id(SharedString::from(format!(
            "background-summary-row-{}-{}",
            item.key.provider_id, item.key.kind as u8
        )))
        .group(group_name)
        .track_focus(&entry.row_focus)
        .tab_index(0)
        .h(px(32.0))
        .w_full()
        .px(px(8.0))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .gap(px(9.0))
        .cursor_default()
        .focus_visible(|style| style.border_1().border_color(theme.accent))
        .hover(|style| style.bg(theme.overlay_strong))
        .child(icon(
            work_kind_icon(item.key.kind),
            14.0,
            theme.text_secondary,
        ))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .line_clamp(1)
                .text_ellipsis()
                .text_size(ui_px(if is_process { 12.0 } else { 13.5 }))
                .text_color(if is_process {
                    theme.text_secondary
                } else {
                    theme.text
                })
                .child(item.title.clone()),
        )
        .children(trailing)
        .on_click(move |_, window, cx| {
            click_handle.close(window, cx);
            window.refresh();
            let _ = click_weak.update(cx, |this, cx| {
                this.open_background_work_surface(session_id, open_key.clone(), cx);
            });
        })
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                key_handle.close(window, cx);
                window.refresh();
                let _ = key_weak.update(cx, |this, cx| {
                    this.open_background_work_surface(session_id, key_key.clone(), cx);
                });
                cx.stop_propagation();
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, status: BackgroundWorkStatus, background: bool) -> BackgroundWorkItem {
        let mut item = BackgroundWorkItem::new(
            BackgroundWorkKind::Process,
            id,
            format!("process {id}"),
            status,
        );
        item.background = background;
        item
    }

    #[test]
    fn info_popover_uses_distinct_process_status_icons() {
        assert_eq!(
            background_summary_process_status_icon(
                BackgroundWorkKind::Process,
                BackgroundWorkStatus::Completed,
            ),
            Some("icons/check.svg")
        );
        assert_eq!(
            background_summary_process_status_icon(
                BackgroundWorkKind::Monitor,
                BackgroundWorkStatus::Failed,
            ),
            Some("icons/x.svg")
        );
        assert_eq!(
            background_summary_process_status_icon(
                BackgroundWorkKind::Process,
                BackgroundWorkStatus::Running,
            ),
            Some("icons/loader-circle.svg")
        );
        assert_eq!(
            background_summary_process_status_icon(
                BackgroundWorkKind::Subagent,
                BackgroundWorkStatus::Completed,
            ),
            None
        );
    }

    #[test]
    fn settled_foreground_commands_leave_the_registry() {
        let mut registry = BackgroundWorkRegistry::default();
        registry.upsert(item("one", BackgroundWorkStatus::Running, false));
        assert!(registry.has_live());
        registry.upsert(item("one", BackgroundWorkStatus::Completed, false));
        assert!(registry.items.is_empty());
    }

    #[test]
    fn reconciliation_marks_disappeared_background_process_lost() {
        let mut registry = BackgroundWorkRegistry::default();
        registry.upsert(item("one", BackgroundWorkStatus::Running, true));
        registry.reconcile_processes(Vec::new());
        assert_eq!(
            registry.items[&BackgroundWorkKey::new(BackgroundWorkKind::Process, "one")].status,
            BackgroundWorkStatus::Lost
        );
    }

    #[test]
    fn polling_does_not_reopen_a_pending_stop() {
        let mut registry = BackgroundWorkRegistry::default();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Process, "one");
        registry.upsert(item("one", BackgroundWorkStatus::Running, true));
        registry.apply(BackgroundWorkEvent::StopRequested(key.clone()));
        registry.upsert(item("one", BackgroundWorkStatus::Running, true));
        assert_eq!(registry.items[&key].status, BackgroundWorkStatus::Stopping);
        assert!(!registry.items[&key].status.is_stoppable());
    }

    #[test]
    fn a_manual_stop_settles_as_stopped_even_if_the_provider_reports_completion() {
        let mut registry = BackgroundWorkRegistry::default();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_child");
        registry.upsert(subagent_item("ses_child", BackgroundWorkStatus::Running));
        registry.apply(BackgroundWorkEvent::StopRequested(key.clone()));
        registry.upsert(subagent_item("ses_child", BackgroundWorkStatus::Completed));
        assert_eq!(registry.items[&key].status, BackgroundWorkStatus::Stopped);
        registry.upsert(subagent_item("ses_child", BackgroundWorkStatus::Failed));
        assert_eq!(registry.items[&key].status, BackgroundWorkStatus::Stopped);
    }

    #[test]
    fn a_stopped_child_can_still_resume() {
        let mut registry = BackgroundWorkRegistry::default();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_child");
        registry.upsert(subagent_item("ses_child", BackgroundWorkStatus::Stopped));
        registry.upsert(subagent_item("ses_child", BackgroundWorkStatus::Running));
        assert_eq!(registry.items[&key].status, BackgroundWorkStatus::Running);
    }

    fn subagent_item(id: &str, status: BackgroundWorkStatus) -> BackgroundWorkItem {
        let mut item = BackgroundWorkItem::new(
            BackgroundWorkKind::Subagent,
            id,
            format!("subagent {id}"),
            status,
        );
        item.background = true;
        item
    }

    fn reasoning_activity(source: &str, complete: bool) -> ActivityItem {
        let mut activity = ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "thinking".into(),
                started_at_ms: 1,
                finished_at_ms: 2,
            },
            complete,
        );
        activity.source_id = Some(source.to_owned());
        activity
    }

    #[test]
    fn late_started_after_deltas_creates_the_turn_without_reordering_messages() {
        let mut registry = BackgroundWorkRegistry::default();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_child");
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::TextDelta {
                key: key.clone(),
                delta: "answer".into(),
            },
        ));
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::Started {
                key: key.clone(),
                prompt: Some("Inspect the repository".into()),
            },
        ));
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::Started {
                key: key.clone(),
                prompt: Some("Inspect the repository".into()),
            },
        ));
        let transcript = &registry.transcripts[&key];
        assert_eq!(transcript.turns.len(), 1, "Started is idempotent");
        assert_eq!(transcript.messages.len(), 1);
        // The prompt arrived after content, so it must not masquerade as
        // the first message of the conversation.
        assert_eq!(transcript.messages[0].role, MessageRole::Assistant);
    }

    #[test]
    fn a_resumed_child_opens_a_new_turn_with_its_own_prompt() {
        let mut registry = BackgroundWorkRegistry::default();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_child");
        let started = |prompt: &str| {
            BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Started {
                key: key.clone(),
                prompt: Some(prompt.to_owned()),
            })
        };
        registry.apply(started("Inspect the repository"));
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::TextDelta {
                key: key.clone(),
                delta: "first answer".into(),
            },
        ));
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::Finished {
                key: key.clone(),
                success: true,
            },
        ));
        let first_turn = registry.transcripts[&key].turns[0].id;

        registry.apply(started("Re-review the diff"));
        let transcript = &registry.transcripts[&key];
        assert_eq!(transcript.turns.len(), 2, "the resume opens a new turn");
        assert_eq!(transcript.turns[1].status, TurnStatus::Running);
        assert_ne!(transcript.turns[1].id, first_turn);
        assert_eq!(
            transcript
                .messages
                .iter()
                .filter(|message| message.role == MessageRole::User)
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["Inspect the repository", "Re-review the diff"],
            "the resume records its own prompt exactly once"
        );
    }

    #[test]
    fn activity_updates_match_source_ids_and_keep_the_original_entry() {
        let mut registry = BackgroundWorkRegistry::default();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_child");
        let running = reasoning_activity("call_1", false);
        let original_id = running.id;
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::Activity {
                key: key.clone(),
                activity: running,
            },
        ));
        let mut settled = reasoning_activity("call_1", true);
        settled.id = uuid::Uuid::new_v4();
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::Activity {
                key: key.clone(),
                activity: settled,
            },
        ));
        let transcript = &registry.transcripts[&key];
        let activities = transcript
            .transcript_blocks
            .iter()
            .flat_map(|block| block.activities.iter())
            .collect::<Vec<_>>();
        assert_eq!(activities.len(), 1, "the update replaces, not appends");
        assert!(activities[0].complete);
        assert_eq!(activities[0].id, original_id);
    }

    #[test]
    fn subagent_text_and_tools_keep_event_order() {
        let mut registry = BackgroundWorkRegistry::default();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_child");
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::Started {
                key: key.clone(),
                prompt: Some("Find the answer".into()),
            },
        ));
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::TextDelta {
                key: key.clone(),
                delta: "Before tool.".into(),
            },
        ));
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::Activity {
                key: key.clone(),
                activity: ActivityItem::new(
                    Some("call_1".into()),
                    ActivityKind::Search,
                    "Search",
                    None,
                    true,
                ),
            },
        ));
        registry.apply(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::TextDelta {
                key: key.clone(),
                delta: "After tool.".into(),
            },
        ));

        let transcript = &registry.transcripts[&key];
        assert_eq!(transcript.messages.len(), 3);
        assert_eq!(transcript.messages[1].content, "Before tool.");
        assert_eq!(transcript.messages[2].content, "After tool.");
        assert_eq!(transcript.transcript_blocks[0].after_message, 2);
    }

    #[test]
    fn settled_subagent_snapshot_survives_session_round_trip() {
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_child");
        let mut parent = AgentSession::new(uuid::Uuid::new_v4());
        let mut activity = ActivityItem::new(
            Some("call_parent".into()),
            ActivityKind::Tool,
            "Task",
            None,
            true,
        );
        activity.output = Some("<subagent sessionID=\"ses_child\">done</subagent>".into());
        parent.transcript_blocks.push(TranscriptBlock {
            after_message: 0,
            turn_id: None,
            activities: vec![activity],
        });

        let mut item = subagent_item("ses_child", BackgroundWorkStatus::Completed);
        recover_subagent_origin(&parent, &mut item);
        let mut registry = BackgroundWorkRegistry::default();
        registry.upsert(item);
        registry.apply_transcript(BackgroundWorkTranscriptEvent::Snapshot {
            key: key.clone(),
            transcript: BackgroundWorkTranscript {
                messages: vec![Message::new(MessageRole::Assistant, "child answer")],
                transcript_blocks: Vec::new(),
                turns: Vec::new(),
            },
        });
        parent
            .background_work
            .push(registry.snapshot(&key).unwrap());

        let stored = serde_json::to_string(&parent).unwrap();
        let restored_session: AgentSession = serde_json::from_str(&stored).unwrap();
        let mut restored =
            BackgroundWorkRegistry::from_snapshots(&restored_session.background_work);
        restored.apply_transcript(BackgroundWorkTranscriptEvent::Snapshot {
            key: key.clone(),
            transcript: BackgroundWorkTranscript::default(),
        });

        assert_eq!(
            restored.items[&key].origin_activity_ids,
            vec!["call_parent".to_owned()]
        );
        assert_eq!(
            restored.transcripts[&key].messages[0].content,
            "child answer"
        );
    }

    #[test]
    fn recovering_a_resumed_child_links_every_naming_call() {
        let mut parent = AgentSession::new(uuid::Uuid::new_v4());
        let mut spawn = ActivityItem::new(
            Some("call_spawn".into()),
            ActivityKind::Tool,
            "Task",
            None,
            true,
        );
        spawn.output =
            Some("<subagent sessionID=\"ses_child\" state=\"completed\">one</subagent>".into());
        let mut resume = ActivityItem::new(
            Some("call_resume".into()),
            ActivityKind::Tool,
            "Task",
            None,
            true,
        );
        resume.output =
            Some("<subagent sessionID=\"ses_child\" state=\"completed\">two</subagent>".into());
        parent.transcript_blocks.push(TranscriptBlock {
            after_message: 0,
            turn_id: None,
            activities: vec![spawn, resume],
        });

        let mut item = subagent_item("ses_child", BackgroundWorkStatus::Completed);
        recover_subagent_origin(&parent, &mut item);
        assert_eq!(
            item.origin_activity_ids,
            vec!["call_spawn".to_owned(), "call_resume".to_owned()]
        );
    }

    #[test]
    fn native_snapshot_is_persistable_before_status_is_known() {
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_child");
        let mut registry = BackgroundWorkRegistry::default();
        registry.upsert(subagent_item("ses_child", BackgroundWorkStatus::Starting));
        registry.apply_transcript(BackgroundWorkTranscriptEvent::Snapshot {
            key: key.clone(),
            transcript: BackgroundWorkTranscript {
                messages: vec![Message::new(MessageRole::Assistant, "child answer")],
                transcript_blocks: Vec::new(),
                turns: Vec::new(),
            },
        });

        assert_eq!(
            registry.snapshot(&key).unwrap().transcript.messages[0].content,
            "child answer"
        );
    }

    #[test]
    fn live_reconciliation_never_rolls_a_settled_child_back() {
        let mut registry = BackgroundWorkRegistry::default();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_child");
        registry.upsert(subagent_item("ses_child", BackgroundWorkStatus::Running));
        registry.upsert(subagent_item("ses_child", BackgroundWorkStatus::Completed));
        // A stale server snapshot taken before the completion event.
        registry.reconcile_live(vec![subagent_item(
            "ses_child",
            BackgroundWorkStatus::Starting,
        )]);
        assert_eq!(registry.items[&key].status, BackgroundWorkStatus::Completed);

        // A snapshot may still move a status forward…
        registry.upsert(subagent_item("ses_child", BackgroundWorkStatus::Running));
        registry.reconcile_live(vec![subagent_item(
            "ses_child",
            BackgroundWorkStatus::Failed,
        )]);
        assert_eq!(registry.items[&key].status, BackgroundWorkStatus::Failed);
        // …while a settled child is simply absent from a reconcile without
        // being re-marked: only live work can be Lost.
        registry.reconcile_live(Vec::new());
        assert_eq!(registry.items[&key].status, BackgroundWorkStatus::Failed);

        // A live child missing from the server goes Lost and recovers when
        // the server lists it again.
        let other = BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "ses_other");
        registry.upsert(subagent_item("ses_other", BackgroundWorkStatus::Running));
        registry.reconcile_live(Vec::new());
        assert_eq!(registry.items[&other].status, BackgroundWorkStatus::Lost);
        registry.reconcile_live(vec![subagent_item(
            "ses_other",
            BackgroundWorkStatus::Running,
        )]);
        assert_eq!(registry.items[&other].status, BackgroundWorkStatus::Running);
    }

    #[test]
    fn output_is_bounded_on_utf8_boundaries() {
        let mut registry = BackgroundWorkRegistry::default();
        registry.upsert(item("one", BackgroundWorkStatus::Running, true));
        registry.append_output(
            &BackgroundWorkKey::new(BackgroundWorkKind::Process, "one"),
            &"界".repeat(MAX_BACKGROUND_OUTPUT_BYTES),
        );
        let output = registry.items.values().next().unwrap();
        assert!(output.output.as_ref().unwrap().len() <= MAX_BACKGROUND_OUTPUT_BYTES);
        assert!(output.output_truncated);
    }

    #[test]
    fn output_cache_strips_split_ansi_sequences() {
        let mut registry = BackgroundWorkRegistry::default();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Process, "one");
        registry.upsert(item("one", BackgroundWorkStatus::Running, true));
        registry.append_output(&key, "\u{1b}");
        registry.append_output(&key, "[31mred\u{1b}[0m");
        assert!(registry.refresh_output_cache());
        assert_eq!(registry.rendered_output[&key].as_ref(), "red");
    }

    #[test]
    fn output_cache_requests_a_retry_only_while_dirty() {
        let mut registry = BackgroundWorkRegistry::default();
        let key = BackgroundWorkKey::new(BackgroundWorkKind::Process, "one");
        registry.upsert(item("one", BackgroundWorkStatus::Running, true));
        registry.append_output(&key, "first");
        assert_eq!(registry.output_refresh_delay(), Some(Duration::ZERO));
        assert!(registry.refresh_output_cache());
        assert_eq!(registry.output_refresh_delay(), None);

        registry.append_output(&key, " second");
        let delay = registry
            .output_refresh_delay()
            .expect("new output should request one cache refresh");
        assert!(delay <= OUTPUT_CACHE_REFRESH_INTERVAL);
        assert!(!registry.refresh_output_cache());
    }

    #[test]
    fn unchanged_process_snapshots_do_not_rebuild_output() {
        let mut registry = BackgroundWorkRegistry::default();
        let mut process = item("one", BackgroundWorkStatus::Running, true);
        process.output = Some("same output".to_owned());
        registry.upsert(process.clone());
        assert!(registry.refresh_output_cache());

        registry.upsert(process);
        assert_eq!(registry.output_refresh_delay(), None);
    }

    #[test]
    fn turn_settlement_keeps_detached_work_live() {
        let mut registry = BackgroundWorkRegistry::default();
        registry.upsert(item("foreground", BackgroundWorkStatus::Running, false));
        registry.upsert(item("background", BackgroundWorkStatus::Running, true));
        registry.settle_foreground(BackgroundWorkStatus::Completed);
        assert!(!registry.items.contains_key(&BackgroundWorkKey::new(
            BackgroundWorkKind::Process,
            "foreground"
        )));
        assert_eq!(
            registry.items[&BackgroundWorkKey::new(BackgroundWorkKind::Process, "background")]
                .status,
            BackgroundWorkStatus::Running
        );
    }

    fn plan_activity(
        title: &str,
        arguments: Option<String>,
        complete: bool,
        failed: bool,
    ) -> crate::model::ActivityItem {
        let mut activity = crate::model::ActivityItem::new(
            Some("plan-source".to_owned()),
            crate::model::ActivityKind::Plan,
            title,
            None,
            complete,
        );
        activity.failed = failed;
        activity.arguments = arguments;
        activity
    }

    fn block(activities: Vec<crate::model::ActivityItem>) -> crate::model::TranscriptBlock {
        crate::model::TranscriptBlock {
            after_message: 0,
            turn_id: None,
            activities,
        }
    }

    #[test]
    fn todo_summary_prefers_the_newest_plan_activity() {
        let older = plan_activity(
            "Plan",
            Some(r#"{"todos":[{"content":"old task","status":"pending"}]}"#.to_owned()),
            true,
            false,
        );
        let newer = plan_activity(
            "Plan",
            Some(r#"{"todos":[{"content":"new task","status":"in_progress"}]}"#.to_owned()),
            false,
            false,
        );
        let blocks = vec![
            block(vec![older]),
            block(vec![crate::model::ActivityItem::new(
                None,
                crate::model::ActivityKind::Command,
                "ls",
                None,
                true,
            )]),
            block(vec![newer]),
        ];
        let summary = todo_summary_from_blocks(&blocks);
        assert_eq!(summary.entries.len(), 1);
        assert_eq!(summary.entries[0].label, "new task");
        assert_eq!(summary.entries[0].state, TodoEntryState::InProgress);
        assert!(summary.updating);
    }

    #[test]
    fn todo_summary_without_plan_activities_is_empty() {
        let blocks = vec![block(vec![crate::model::ActivityItem::new(
            None,
            crate::model::ActivityKind::Command,
            "ls",
            None,
            true,
        )])];
        assert_eq!(todo_summary_from_blocks(&blocks), TodoSummary::default());
    }

    #[test]
    fn todo_summary_parses_todo_and_plan_payloads() {
        let claude = plan_activity(
            "Plan",
            Some(
                r#"{"todos":[
                    {"content":"done task","status":"completed"},
                    {"content":"running task","status":"in_progress"},
                    {"content":"queued task"}
                ]}"#
                .to_owned(),
            ),
            true,
            false,
        );
        let summary = todo_summary_from_activity(&claude);
        assert_eq!(
            summary
                .entries
                .iter()
                .map(|entry| entry.state)
                .collect::<Vec<_>>(),
            vec![
                TodoEntryState::Completed,
                TodoEntryState::InProgress,
                TodoEntryState::Pending
            ]
        );
        assert!(!summary.updating);

        let steps = plan_activity(
            "Plan",
            Some(r#"{"plan":[{"step":"write tests","status":"done"}]}"#.to_owned()),
            true,
            false,
        );
        let summary = todo_summary_from_activity(&steps);
        assert_eq!(summary.entries[0].label, "write tests");
        assert_eq!(summary.entries[0].state, TodoEntryState::Completed);

        let bare = plan_activity("Plan", None, true, false);
        let mut bare = bare;
        bare.output = Some(r#"["first", "second"]"#.to_owned());
        let summary = todo_summary_from_activity(&bare);
        assert_eq!(
            summary
                .entries
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert!(
            summary
                .entries
                .iter()
                .all(|entry| entry.state == TodoEntryState::Pending)
        );
    }

    #[test]
    fn todo_summary_falls_back_to_one_entry_without_parseable_payload() {
        let live = plan_activity("Ship the release", None, false, false);
        let summary = todo_summary_from_activity(&live);
        assert_eq!(summary.entries.len(), 1);
        assert_eq!(summary.entries[0].label, "Ship the release");
        assert_eq!(summary.entries[0].state, TodoEntryState::InProgress);

        let failed = plan_activity("Plan", Some("not json".to_owned()), true, true);
        let summary = todo_summary_from_activity(&failed);
        assert_eq!(summary.entries[0].state, TodoEntryState::Failed);
        assert!(summary.failed);
    }

    #[test]
    fn todo_status_normalizes_provider_spellings() {
        assert_eq!(
            TodoEntryState::from_status("Done"),
            TodoEntryState::Completed
        );
        assert_eq!(
            TodoEntryState::from_status("In-Progress"),
            TodoEntryState::InProgress
        );
        assert_eq!(
            TodoEntryState::from_status("cancelled"),
            TodoEntryState::Failed
        );
        assert_eq!(
            TodoEntryState::from_status("pending"),
            TodoEntryState::Pending
        );
        assert_eq!(
            TodoEntryState::from_status("some future status"),
            TodoEntryState::Pending
        );
    }
}

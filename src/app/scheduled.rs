//! Scheduled tasks: a workspace destination with a virtualized overview and
//! component-backed editing dialogs.
//!
//! One worker thread owns the daemon subscription and every RPC. It reports
//! back over a channel, and a foreground task applies the newest generation so
//! a slow reload can never overwrite a newer save. `render` reads memory only.

use crate::ui::icon;
use crate::ui::toggle_switch;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, SegmentedControl, ToggleButton, ToggleItem},
    forms::{Choice, Combobox, Input, InputEvent, Select, TextInput, TimePicker},
    overlays::Dialog,
    primitives::IconName,
    theme::ControlSize,
};

use fintwind_client::scheduled_task::{
    ClockTime, DispatchOutcome, Recurrence, ScheduledTask, ScheduledTaskDraft, ScheduledTaskEdit,
    ScheduledTaskId, SessionLaunch, TaskTarget, Weekdays,
};
use fintwind_client::{Command, DaemonClient, DaemonSupervisor, ResponsePayload};
use gpui::{ElementId, EventEmitter, Task};

use super::*;

mod editor;
mod overview;
use overview::command_button;

const ROW_HEIGHT: f32 = 112.0;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const PICKER_LIMIT: usize = 40;
const WEEKDAY_ORDER: [(u8, &str); 7] = [
    (1, "scheduled_tasks.weekday_mon"),
    (2, "scheduled_tasks.weekday_tue"),
    (3, "scheduled_tasks.weekday_wed"),
    (4, "scheduled_tasks.weekday_thu"),
    (5, "scheduled_tasks.weekday_fri"),
    (6, "scheduled_tasks.weekday_sat"),
    (0, "scheduled_tasks.weekday_sun"),
];

#[derive(Clone)]
pub(super) struct ScheduledSessionChoice {
    pub id: Uuid,
    pub title: String,
    pub project_id: Uuid,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ScheduledTasksEvent {
    OpenSession(Uuid),
}

enum Failure {
    Disconnected,
    Daemon(String),
    UnexpectedResponse,
}

enum ScheduledRequest {
    Reload,
    Save(ScheduledTaskEdit),
    Delete {
        id: ScheduledTaskId,
        expected_revision: u64,
    },
    RunNow(ScheduledTaskId),
}

enum ScheduledWake {
    List {
        generation: u64,
        result: Result<Vec<ScheduledTask>, Failure>,
    },
    Saved {
        generation: u64,
        result: Result<ScheduledTask, Failure>,
    },
    Deleted {
        generation: u64,
        result: Result<(), Failure>,
    },
}

impl ScheduledWake {
    fn generation(&self) -> u64 {
        match self {
            Self::List { generation, .. }
            | Self::Saved { generation, .. }
            | Self::Deleted { generation, .. } => *generation,
        }
    }

    fn with_generation(self, generation: u64) -> Self {
        match self {
            Self::List { result, .. } => Self::List { generation, result },
            Self::Saved { result, .. } => Self::Saved { generation, result },
            Self::Deleted { result, .. } => Self::Deleted { generation, result },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScheduleKind {
    AtTime,
    Every,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetKind {
    NewSession,
    ExistingSession,
}

#[derive(Clone, PartialEq)]
struct Editor {
    task_id: Option<ScheduledTaskId>,
    revision: u64,
    enabled: bool,
    title: String,
    prompt: String,
    schedule: ScheduleKind,
    time: String,
    weekdays: u8,
    interval_minutes: String,
    target: TargetKind,
    project_id: Option<Uuid>,
    session_id: Option<Uuid>,
    launch: SessionLaunch,
    workspace: SessionWorkspace,
}

impl Editor {
    fn for_task(task: &ScheduledTask) -> Self {
        let (schedule, time, weekdays, interval_minutes) = match task.schedule {
            Recurrence::AtTime { at, weekdays } => (
                ScheduleKind::AtTime,
                format_clock(at),
                weekdays.bits(),
                "60".to_owned(),
            ),
            Recurrence::Every { interval_secs } => (
                ScheduleKind::Every,
                "09:00".to_owned(),
                Weekdays::every_day().bits(),
                (interval_secs / 60).max(1).to_string(),
            ),
        };
        let (target, project_id, session_id, launch) = match &task.target {
            TaskTarget::NewSession {
                project_id, launch, ..
            } => (
                TargetKind::NewSession,
                Some(*project_id),
                None,
                launch.clone(),
            ),
            TaskTarget::ExistingSession { session_id } => (
                TargetKind::ExistingSession,
                None,
                Some(*session_id),
                SessionLaunch {
                    model: String::new(),
                    runtime_mode: RuntimeMode::Ask,
                    interaction_mode: InteractionMode::default(),
                    reasoning_effort: None,
                    service_tier: None,
                    context_window: None,
                    agent_preset: None,
                },
            ),
        };
        Self {
            task_id: Some(task.id),
            revision: task.revision,
            enabled: task.enabled,
            title: task.title.clone(),
            prompt: task.prompt.clone(),
            schedule,
            time,
            weekdays,
            interval_minutes,
            target,
            project_id,
            session_id,
            launch,
            workspace: match &task.target {
                TaskTarget::NewSession { workspace, .. } => workspace.clone(),
                TaskTarget::ExistingSession { .. } => SessionWorkspace::Local,
            },
        }
    }

    fn draft(&self) -> Result<ScheduledTaskDraft, String> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err(tr!("scheduled_tasks.error_title"));
        }
        let prompt = self.prompt.trim();
        if prompt.is_empty() {
            return Err(tr!("scheduled_tasks.error_prompt"));
        }
        let schedule = match self.schedule {
            ScheduleKind::AtTime => {
                let at =
                    parse_clock(&self.time).ok_or_else(|| tr!("scheduled_tasks.error_time"))?;
                let weekdays = Weekdays::new(self.weekdays)
                    .map_err(|_| tr!("scheduled_tasks.error_weekdays"))?;
                Recurrence::AtTime { at, weekdays }
            }
            ScheduleKind::Every => {
                let minutes: u32 = self
                    .interval_minutes
                    .trim()
                    .parse()
                    .map_err(|_| tr!("scheduled_tasks.error_interval"))?;
                let interval_secs = minutes
                    .checked_mul(60)
                    .filter(|secs| (60..=31_536_000).contains(secs))
                    .ok_or_else(|| tr!("scheduled_tasks.error_interval"))?;
                Recurrence::Every { interval_secs }
            }
        };
        let target = match self.target {
            TargetKind::NewSession => {
                let project_id = self
                    .project_id
                    .ok_or_else(|| tr!("scheduled_tasks.error_project"))?;
                let model = self.launch.model.trim();
                if model.is_empty() {
                    return Err(tr!("scheduled_tasks.error_model"));
                }
                TaskTarget::NewSession {
                    project_id,
                    launch: SessionLaunch {
                        model: model.to_owned(),
                        runtime_mode: self.launch.runtime_mode.access(),
                        ..self.launch.clone()
                    },
                    workspace: self.workspace.clone(),
                }
            }
            TargetKind::ExistingSession => {
                let session_id = self
                    .session_id
                    .ok_or_else(|| tr!("scheduled_tasks.error_session"))?;
                TaskTarget::ExistingSession { session_id }
            }
        };
        let draft = ScheduledTaskDraft {
            title: title.to_owned(),
            prompt: prompt.to_owned(),
            enabled: self.enabled,
            schedule,
            target,
        };
        draft
            .validate()
            .map_err(|detail| tr!("scheduled_tasks.error_detail", detail = detail))?;
        Ok(draft)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskState {
    Never,
    Queued,
    Preparing,
    Dispatched,
    Unconfirmed,
    NotSent,
    Rejected,
    Missed,
}

impl TaskState {
    fn of(task: &ScheduledTask) -> Self {
        if let Some(pending) = &task.pending {
            return if pending.preparing {
                Self::Preparing
            } else {
                Self::Queued
            };
        }
        match task.last.as_ref().map(|last| &last.outcome) {
            None => Self::Never,
            Some(DispatchOutcome::Accepted { .. }) => Self::Dispatched,
            Some(DispatchOutcome::Unconfirmed { .. }) => Self::Unconfirmed,
            Some(DispatchOutcome::NotSent { .. }) => Self::NotSent,
            Some(DispatchOutcome::Rejected { .. }) => Self::Rejected,
            Some(DispatchOutcome::Missed) => Self::Missed,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Never => tr!("scheduled_tasks.status_never"),
            Self::Queued => tr!("scheduled_tasks.status_queued"),
            Self::Preparing => tr!("scheduled_tasks.status_preparing"),
            Self::Dispatched => tr!("scheduled_tasks.status_dispatched"),
            Self::Unconfirmed => tr!("scheduled_tasks.status_unconfirmed"),
            Self::NotSent => tr!("scheduled_tasks.status_not_sent"),
            Self::Rejected => tr!("scheduled_tasks.status_rejected"),
            Self::Missed => tr!("scheduled_tasks.status_missed"),
        }
    }
}

fn last_session(task: &ScheduledTask) -> Option<Uuid> {
    if let Some(session_id) = task.pending.as_ref().and_then(|pending| pending.session_id) {
        return Some(session_id);
    }
    match task.last.as_ref().map(|last| &last.outcome) {
        Some(DispatchOutcome::Accepted { session_id, .. })
        | Some(DispatchOutcome::Unconfirmed { session_id, .. }) => Some(*session_id),
        Some(DispatchOutcome::NotSent {
            session_id: Some(session_id),
            ..
        }) => Some(*session_id),
        _ => None,
    }
}

fn outcome_reason(task: &ScheduledTask) -> Option<String> {
    match task.last.as_ref().map(|last| &last.outcome) {
        Some(DispatchOutcome::NotSent { reason, .. })
        | Some(DispatchOutcome::Rejected { reason }) => Some(reason.clone()),
        _ => None,
    }
}

fn parse_clock(text: &str) -> Option<ClockTime> {
    let text = text.trim();
    let (hour, minute) = text.split_once(':')?;
    let hour: u8 = hour.trim().parse().ok()?;
    let minute: u8 = minute.trim().parse().ok()?;
    ClockTime::new(hour, minute).ok()
}

fn format_clock(at: ClockTime) -> String {
    format!("{:02}:{:02}", at.hour(), at.minute())
}

fn format_timestamp(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
        .map(|time| time.with_timezone(&Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn weekday_labels(weekdays: Weekdays) -> String {
    if weekdays.bits() == Weekdays::every_day().bits() {
        return tr!("scheduled_tasks.every_day");
    }
    WEEKDAY_ORDER
        .iter()
        .filter(|&(bit, _)| weekdays.bits() & (1 << bit) != 0)
        .map(|&(_, key)| tr!(key))
        .collect::<Vec<_>>()
        .join(" ")
}

fn schedule_summary(schedule: Recurrence) -> String {
    match schedule {
        Recurrence::AtTime { at, weekdays } => tr!(
            "scheduled_tasks.at_time_summary",
            days = weekday_labels(weekdays),
            time = format_clock(at)
        ),
        Recurrence::Every { interval_secs } => {
            if interval_secs % 3600 == 0 {
                tr!("scheduled_tasks.every_hours", count = interval_secs / 3600)
            } else {
                tr!(
                    "scheduled_tasks.every_minutes",
                    count = (interval_secs / 60).max(1)
                )
            }
        }
    }
}

fn sort_tasks(tasks: &mut [ScheduledTask]) {
    tasks.sort_by(|a, b| {
        b.enabled
            .cmp(&a.enabled)
            .then_with(|| a.next_due_at.cmp(&b.next_due_at))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.0.cmp(&b.id.0))
    });
}

fn next_due_label(task: &ScheduledTask) -> String {
    match task.next_due_at {
        Some(at) => tr!("scheduled_tasks.next_run", time = format_timestamp(at)),
        None => tr!("scheduled_tasks.next_run_none"),
    }
}

fn failure_message(failure: Failure) -> String {
    match failure {
        Failure::Disconnected => tr!("scheduled_tasks.error_disconnected"),
        Failure::Daemon(detail) => tr!("scheduled_tasks.error_detail", detail = detail),
        Failure::UnexpectedResponse => tr!("scheduled_tasks.error_unexpected"),
    }
}

fn load_tasks(client: &DaemonClient) -> Result<Vec<ScheduledTask>, Failure> {
    match client.request(Uuid::nil(), Uuid::nil(), Command::ListScheduledTasks) {
        Ok(ResponsePayload::ScheduledTasks { tasks }) => Ok(tasks),
        Ok(_) => Err(Failure::UnexpectedResponse),
        Err(error) => Err(Failure::Daemon(error.to_string())),
    }
}

fn saved_task(client: &DaemonClient, command: Command) -> Result<ScheduledTask, Failure> {
    match client.request(Uuid::nil(), Uuid::nil(), command) {
        Ok(ResponsePayload::ScheduledTaskSaved { task }) => Ok(task),
        Ok(_) => Err(Failure::UnexpectedResponse),
        Err(error) => Err(Failure::Daemon(error.to_string())),
    }
}

fn perform_request(client: &DaemonClient, request: ScheduledRequest) -> ScheduledWake {
    match request {
        ScheduledRequest::Reload => ScheduledWake::List {
            generation: 0,
            result: load_tasks(client),
        },
        ScheduledRequest::Save(edit) => ScheduledWake::Saved {
            generation: 0,
            result: saved_task(client, Command::SaveScheduledTask { edit }),
        },
        ScheduledRequest::RunNow(id) => ScheduledWake::Saved {
            generation: 0,
            result: saved_task(client, Command::RunScheduledTaskNow { id }),
        },
        ScheduledRequest::Delete {
            id,
            expected_revision,
        } => ScheduledWake::Deleted {
            generation: 0,
            result: match client.request(
                Uuid::nil(),
                Uuid::nil(),
                Command::DeleteScheduledTask {
                    id,
                    expected_revision,
                },
            ) {
                Ok(ResponsePayload::Ack) => Ok(()),
                Ok(_) => Err(Failure::UnexpectedResponse),
                Err(error) => Err(Failure::Daemon(error.to_string())),
            },
        },
    }
}

fn refused_request(request: ScheduledRequest) -> ScheduledWake {
    match request {
        ScheduledRequest::Reload => ScheduledWake::List {
            generation: 0,
            result: Err(Failure::Disconnected),
        },
        ScheduledRequest::Save(_) | ScheduledRequest::RunNow(_) => ScheduledWake::Saved {
            generation: 0,
            result: Err(Failure::Disconnected),
        },
        ScheduledRequest::Delete { .. } => ScheduledWake::Deleted {
            generation: 0,
            result: Err(Failure::Disconnected),
        },
    }
}

fn run_scheduled_worker(
    clients: Receiver<DaemonClient>,
    requests: Receiver<ScheduledRequest>,
    wake: smol::channel::Sender<ScheduledWake>,
) {
    let Ok(mut client) = clients.recv() else {
        return;
    };
    let mut generation = 0u64;
    loop {
        let revisions = client.subscribe_scheduled_tasks();
        if !client.is_disconnected() {
            generation += 1;
            if wake
                .send_blocking(ScheduledWake::List {
                    generation,
                    result: load_tasks(&client),
                })
                .is_err()
            {
                return;
            }
        }
        let mut disconnected = client.is_disconnected();
        if !disconnected {
            loop {
                crossbeam_channel::select! {
                    recv(requests) -> request => {
                        let Ok(request) = request else { return };
                        if client.is_disconnected() {
                            generation += 1;
                            if wake.send_blocking(refused_request(request).with_generation(generation)).is_err() {
                                return;
                            }
                            disconnected = true;
                            break;
                        }
                        generation += 1;
                        let outcome = perform_request(&client, request);
                        if wake.send_blocking(outcome.with_generation(generation)).is_err() {
                            return;
                        }
                    }
                    recv(revisions) -> revision => {
                        if revision.is_err() {
                            disconnected = true;
                            break;
                        }
                        while revisions.try_recv().is_ok() {}
                        if client.is_disconnected() {
                            disconnected = true;
                            break;
                        }
                        generation += 1;
                        if wake.send_blocking(ScheduledWake::List {
                            generation,
                            result: load_tasks(&client),
                        }).is_err() {
                            return;
                        }
                    }
                    recv(clients) -> replacement => {
                        match replacement {
                            Ok(replacement) => client = replacement,
                            Err(_) => return,
                        }
                        break;
                    }
                    default(POLL_INTERVAL) => {
                        if client.is_disconnected() {
                            disconnected = true;
                            break;
                        }
                    }
                }
            }
        }
        if disconnected {
            loop {
                crossbeam_channel::select! {
                    recv(clients) -> replacement => {
                        match replacement {
                            Ok(replacement) => {
                                client = replacement;
                                break;
                            }
                            Err(_) => return,
                        }
                    }
                    recv(requests) -> request => {
                        let Ok(request) = request else { return };
                        generation += 1;
                        if wake.send_blocking(refused_request(request).with_generation(generation)).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
enum TaskFilter {
    #[default]
    All,
    Active,
    Paused,
}

struct TaskEditor {
    draft: Editor,
    original: Editor,
}

enum TaskDialog {
    Edit(TaskEditor),
    Discard(TaskEditor),
    Delete {
        id: ScheduledTaskId,
        revision: u64,
        title: String,
    },
}

enum Mutation {
    SaveEditor,
    UpdateTask,
    Delete(ScheduledTaskId),
}

pub(super) struct ScheduledTasksView {
    daemon: DaemonSupervisor,
    window: AnyWindowHandle,
    title_input: Entity<TextInput>,
    prompt_input: Entity<TextInput>,
    interval_input: Entity<TextInput>,
    model_input: Entity<TextInput>,
    session_input: Entity<TextInput>,
    search_input: Entity<TextInput>,
    list_state: ListState,
    row_height: Pixels,
    list_scrollbar: Rc<ScrollbarState>,
    projects: Vec<Project>,
    sessions: Vec<ScheduledSessionChoice>,
    session_ids: HashSet<Uuid>,
    models: Vec<Choice>,
    session_choices: Vec<Choice>,
    shown_models: Vec<Choice>,
    shown_sessions: Vec<Choice>,
    default_launch: Option<SessionLaunch>,
    tasks: Vec<ScheduledTask>,
    visible: Vec<usize>,
    task_keys: Vec<String>,
    task_targets: Vec<String>,
    filter: TaskFilter,
    query: String,
    dialog: Option<TaskDialog>,
    previous_focus: Option<FocusHandle>,
    confirm_focus: FocusHandle,
    loading: bool,
    mutation: Option<Mutation>,
    error: Option<SharedString>,
    applied_generation: u64,
    requests: Sender<ScheduledRequest>,
    _wake_task: Task<()>,
}

impl EventEmitter<ScheduledTasksEvent> for ScheduledTasksView {}

impl ScheduledTasksView {
    pub(super) fn new(
        daemon: DaemonSupervisor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title_input = cx.new(|cx| {
            TextInput::new(window, cx).placeholder(tr!("scheduled_tasks.name_placeholder"))
        });
        let prompt_input = cx.new(|cx| {
            TextInput::new(window, cx)
                .multi_line(4, 10)
                .placeholder(tr!("scheduled_tasks.prompt_placeholder"))
        });
        let interval_input = cx.new(|cx| TextInput::new(window, cx).placeholder("60"));
        let model_input =
            cx.new(|cx| TextInput::new(window, cx).placeholder(tr!("input.search_models")));
        let session_input = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder(tr!("scheduled_tasks.session_search_placeholder"))
        });
        let search_input =
            cx.new(|cx| TextInput::new(window, cx).placeholder(tr!("scheduled_tasks.search")));
        cx.subscribe(&search_input, |this: &mut Self, input, event, cx| {
            if *event == InputEvent::Changed {
                this.query = input.read(cx).text().trim().to_lowercase();
                this.refresh_visible();
                this.list_state.scroll_to(ListOffset::default());
                cx.notify();
            }
        })
        .detach();
        for input in [&model_input, &session_input] {
            cx.subscribe(input, |this: &mut Self, _, event, cx| {
                if *event == InputEvent::Changed {
                    this.refresh_picker_choices(cx);
                    cx.notify();
                }
            })
            .detach();
        }
        // The multiline component submits on Ctrl+Enter after IME handling.
        cx.subscribe(&prompt_input, |this: &mut Self, _, event, cx| {
            if *event == InputEvent::Submit {
                this.save(cx);
            }
        })
        .detach();
        let clients = daemon.subscribe_clients();
        let (requests, request_rx) = crossbeam_channel::unbounded();
        let (wake, wake_rx) = smol::channel::unbounded();
        std::thread::Builder::new()
            .name("fintwind-scheduled-tasks".into())
            .spawn(move || run_scheduled_worker(clients, request_rx, wake))
            .ok();
        let wake_task = cx.spawn(async move |this, cx| {
            while let Ok(wake) = wake_rx.recv().await {
                if this
                    .update(cx, |this, cx| this.apply_wake(wake, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            daemon,
            window: window.window_handle(),
            title_input,
            prompt_input,
            interval_input,
            model_input,
            session_input,
            search_input,
            list_state: ListState::new(0, ListAlignment::Top, px(320.0)),
            row_height: ui_px(ROW_HEIGHT),
            list_scrollbar: ScrollbarState::new(),
            projects: Vec::new(),
            sessions: Vec::new(),
            session_ids: HashSet::new(),
            models: Vec::new(),
            session_choices: Vec::new(),
            shown_models: Vec::new(),
            shown_sessions: Vec::new(),
            default_launch: None,
            tasks: Vec::new(),
            visible: Vec::new(),
            task_keys: Vec::new(),
            task_targets: Vec::new(),
            filter: TaskFilter::All,
            query: String::new(),
            dialog: None,
            previous_focus: None,
            confirm_focus: cx.focus_handle(),
            loading: true,
            mutation: None,
            error: None,
            applied_generation: 0,
            requests,
            _wake_task: wake_task,
        }
    }

    pub(super) fn set_catalog(
        &mut self,
        projects: Vec<Project>,
        sessions: Vec<ScheduledSessionChoice>,
        models: Vec<ProviderModel>,
        launch: Option<SessionLaunch>,
        cx: &mut Context<Self>,
    ) {
        self.projects = projects;
        self.sessions = sessions;
        self.session_ids = self.sessions.iter().map(|session| session.id).collect();
        self.models = models
            .into_iter()
            .map(|model| Choice::new(model.id.clone(), model.name).note(model.id))
            .collect();
        self.session_choices = self
            .sessions
            .iter()
            .map(|session| {
                let project = self
                    .projects
                    .iter()
                    .find(|p| p.id == session.project_id)
                    .map(|p| p.name.as_str())
                    .unwrap_or_default();
                Choice::new(
                    session.id.to_string(),
                    format!("{} · {project}", session.title),
                )
            })
            .collect();
        self.default_launch = launch.map(|launch| SessionLaunch {
            runtime_mode: launch.runtime_mode.access(),
            ..launch
        });
        self.refresh_picker_choices(cx);
        self.refresh_task_keys();
        cx.notify();
    }

    fn busy(&self) -> bool {
        self.mutation.is_some()
    }

    pub(super) fn refresh_localized_text(&self, cx: &mut Context<Self>) {
        for (input, key) in [
            (&self.title_input, "scheduled_tasks.name_placeholder"),
            (&self.prompt_input, "scheduled_tasks.prompt_placeholder"),
            (&self.model_input, "input.search_models"),
            (
                &self.session_input,
                "scheduled_tasks.session_search_placeholder",
            ),
            (&self.search_input, "scheduled_tasks.search"),
        ] {
            input.update(cx, |input, cx| input.set_placeholder(tr!(key), cx));
        }
        cx.notify();
    }

    fn refresh_picker_choices(&mut self, cx: &App) {
        let draft = match &self.dialog {
            Some(TaskDialog::Edit(editor) | TaskDialog::Discard(editor)) => Some(&editor.draft),
            _ => None,
        };
        let model = draft
            .map(|draft| draft.launch.model.as_str())
            .unwrap_or_default();
        let session = draft
            .and_then(|draft| draft.session_id)
            .map(|id| id.to_string())
            .unwrap_or_default();
        self.shown_models = picker_choices(&self.models, self.model_input.read(cx).text(), model);
        self.shown_sessions = picker_choices(
            &self.session_choices,
            self.session_input.read(cx).text(),
            &session,
        );
    }

    fn task(&self, id: ScheduledTaskId) -> Option<&ScheduledTask> {
        self.tasks.iter().find(|task| task.id == id)
    }

    fn target_label(&self, target: &TaskTarget) -> String {
        match target {
            TaskTarget::NewSession { project_id, .. } => self
                .projects
                .iter()
                .find(|p| p.id == *project_id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| tr!("scheduled_tasks.project_missing")),
            TaskTarget::ExistingSession { session_id } => self
                .sessions
                .iter()
                .find(|s| s.id == *session_id)
                .map(|s| s.title.clone())
                .unwrap_or_else(|| tr!("scheduled_tasks.session_missing")),
        }
    }

    fn refresh_task_keys(&mut self) {
        self.task_targets = self
            .tasks
            .iter()
            .map(|task| self.target_label(&task.target))
            .collect();
        self.task_keys = self
            .tasks
            .iter()
            .zip(&self.task_targets)
            .map(|(task, target)| format!("{} {} {target}", task.title, task.prompt).to_lowercase())
            .collect();
        self.refresh_visible();
    }

    fn refresh_visible(&mut self) {
        self.visible = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(index, task)| {
                (self.filter == TaskFilter::All
                    || task.enabled == (self.filter == TaskFilter::Active))
                    && self
                        .query
                        .split_whitespace()
                        .all(|word| self.task_keys[*index].contains(word))
            })
            .map(|(index, _)| index)
            .collect();
        let count = self.visible.len();
        if count != self.list_state.item_count() {
            let offset = self.list_state.logical_scroll_top();
            self.list_state
                .reset_with_uniform_height(count, self.row_height);
            self.list_state.scroll_to(offset);
        }
    }

    fn set_filter(&mut self, filter: TaskFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.refresh_visible();
        self.list_state.scroll_to(ListOffset::default());
        cx.notify();
    }

    fn apply_wake(&mut self, wake: ScheduledWake, cx: &mut Context<Self>) {
        if wake.generation() <= self.applied_generation {
            return;
        }
        self.applied_generation = wake.generation();
        match wake {
            ScheduledWake::List { result, .. } => {
                self.loading = false;
                match result {
                    Ok(mut tasks) => {
                        sort_tasks(&mut tasks);
                        self.tasks = tasks;
                        self.refresh_task_keys();
                        if self.dialog.is_none() && !self.busy() {
                            self.error = None;
                        }
                    }
                    Err(failure) => self.error = Some(failure_message(failure).into()),
                }
            }
            ScheduledWake::Saved { result, .. } => {
                let mutation = self.mutation.take();
                match result {
                    Ok(task) => {
                        if let Some(existing) =
                            self.tasks.iter_mut().find(|entry| entry.id == task.id)
                        {
                            *existing = task;
                        } else {
                            self.tasks.push(task);
                        }
                        sort_tasks(&mut self.tasks);
                        self.refresh_task_keys();
                        self.error = None;
                        if matches!(mutation, Some(Mutation::SaveEditor)) {
                            self.finish_dialog(cx);
                        }
                    }
                    Err(failure) => self.error = Some(failure_message(failure).into()),
                }
                self.set_inputs_disabled(false, cx);
            }
            ScheduledWake::Deleted { result, .. } => {
                let mutation = self.mutation.take();
                match result {
                    Ok(()) => {
                        if let Some(Mutation::Delete(id)) = mutation {
                            self.tasks.retain(|task| task.id != id);
                        }
                        self.refresh_task_keys();
                        self.error = None;
                        self.finish_dialog(cx);
                    }
                    Err(failure) => self.error = Some(failure_message(failure).into()),
                }
            }
        }
        cx.notify();
    }

    fn send(&mut self, request: ScheduledRequest, mutation: Mutation, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.error = None;
        if self.requests.send(request).is_err() {
            self.error = Some(tr!("scheduled_tasks.error_disconnected").into());
        } else {
            self.set_inputs_disabled(matches!(mutation, Mutation::SaveEditor), cx);
            self.mutation = Some(mutation);
        }
        cx.notify();
    }

    fn set_inputs_disabled(&self, disabled: bool, cx: &mut Context<Self>) {
        for input in [
            &self.title_input,
            &self.prompt_input,
            &self.interval_input,
            &self.model_input,
            &self.session_input,
        ] {
            input.update(cx, |input, cx| input.set_disabled(disabled, cx));
        }
    }

    fn set_enabled(&mut self, id: ScheduledTaskId, enabled: bool, cx: &mut Context<Self>) {
        let Some(task) = self.task(id) else {
            return;
        };
        let draft = ScheduledTaskDraft {
            title: task.title.clone(),
            prompt: task.prompt.clone(),
            enabled,
            schedule: task.schedule,
            target: task.target.clone(),
        };
        self.send(
            ScheduledRequest::Save(ScheduledTaskEdit::Update {
                id,
                expected_revision: task.revision,
                draft,
            }),
            Mutation::UpdateTask,
            cx,
        );
    }

    fn run_now(&mut self, id: ScheduledTaskId, cx: &mut Context<Self>) {
        self.send(ScheduledRequest::RunNow(id), Mutation::UpdateTask, cx);
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.loading = self.tasks.is_empty();
        if self.daemon.client().is_disconnected()
            || self.requests.send(ScheduledRequest::Reload).is_err()
        {
            self.loading = false;
            self.error = Some(tr!("scheduled_tasks.error_disconnected").into());
        }
        cx.notify();
    }
}

/// Bound Ely's popup rows before render; the complete catalog is searched only
/// when its input changes, and a saved selection stays resolvable off-screen.
fn picker_choices(all: &[Choice], text: &str, selected: &str) -> Vec<Choice> {
    let choice = all.iter().find(|choice| choice.value.as_ref() == selected);
    let query = if choice.is_some_and(|choice| choice.label.as_ref() == text) {
        String::new()
    } else {
        text.trim().to_lowercase()
    };
    let mut shown: Vec<_> = all
        .iter()
        .filter(|choice| {
            choice.label.to_lowercase().contains(&query)
                || choice
                    .note
                    .as_ref()
                    .is_some_and(|note| note.to_lowercase().contains(&query))
        })
        .take(PICKER_LIMIT)
        .cloned()
        .collect();
    if !selected.is_empty() && !shown.iter().any(|choice| choice.value.as_ref() == selected) {
        shown.push(
            choice
                .cloned()
                .unwrap_or_else(|| Choice::new(selected.to_owned(), selected.to_owned())),
        );
    }
    shown
}

impl Fintwind {
    pub(super) fn open_scheduled_tasks(&mut self, cx: &mut Context<Self>) {
        self.mode = WorkspaceMode::ScheduledTasks;
        self.refresh_scheduled_task_catalog(cx);
        cx.notify();
    }

    pub(super) fn refresh_scheduled_task_catalog(&self, cx: &mut Context<Self>) {
        use fintwind_protocol::scheduled_task::SessionLaunch;
        let projects = self.state.projects.clone();
        let sessions = self
            .state
            .sessions
            .iter()
            .map(|session| ScheduledSessionChoice {
                id: session.id,
                title: session.display_title().to_owned(),
                project_id: session.project_id,
            })
            .collect();
        let launch = self.selected_session().and_then(|session| {
            session
                .model
                .clone()
                .or_else(|| self.state.last_model.clone())
                .map(|model| SessionLaunch {
                    model,
                    runtime_mode: session.runtime_mode,
                    interaction_mode: session.interaction_mode,
                    reasoning_effort: session.reasoning_effort.clone(),
                    service_tier: session.service_tier.clone(),
                    context_window: session.context_window.clone(),
                    agent_preset: session.agent_preset.clone(),
                })
        });
        self.scheduled_tasks_view.update(cx, |view, cx| {
            let models = self
                .probes
                .iter()
                .filter(|probe| probe.installed)
                .flat_map(|probe| probe.models.iter().cloned())
                .collect();
            view.set_catalog(projects, sessions, models, launch, cx);
        });
        cx.notify();
    }
}

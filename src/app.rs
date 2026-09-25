use crate::theme::ui_px;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use crossbeam_channel::{Receiver, Sender, unbounded};
use gpui::{
    Animation, AnimationExt, AnyElement, App, Bounds, ClipboardEntry, ClipboardItem, Context, Div,
    Entity, ExternalPaths, FocusHandle, Focusable, FontWeight, Hsla, IntoElement, KeyDownEvent,
    ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, NavigationDirection, ObjectFit, PathPromptOptions, Pixels, Render, ScrollHandle,
    SharedString, Stateful, StyleRefinement, TextRun, WeakEntity, Window, WindowBounds, canvas,
    div, ease_out_quint, fill, font, img, linear_color_stop, linear_gradient, list, point,
    prelude::*, pulsating_between, px, rgb,
};
use uuid::Uuid;

use crate::app::background_work::TodoSummary;
use crate::checkpoint;
use crate::composer_complete::{FileEntry, SlashCommand};
use crate::driver::{self, DriverHandle, DriverStartOptions, SessionOptions};
use crate::git_branch::BranchSnapshot;
use crate::input::{ComposerAttachmentPaste, ComposerEvent, ComposerInput};
use crate::md;
use crate::model::{
    ActivityItem, ActivityKind, AgentSession, AgentTurn, BackgroundWorkEvent, BackgroundWorkItem,
    BackgroundWorkKey, BackgroundWorkKind, BackgroundWorkSnapshot, BackgroundWorkStatus,
    BackgroundWorkTranscript, BackgroundWorkTranscriptEvent, Checkpoint, CheckpointStatus,
    CompactionState, CompactionStatus, ContextUsage, DriverEvent, FavoriteModel, InteractionMode,
    Message, MessageAttachment, MessageRole, OPENCODE_PROVIDER, PendingPermission, Project,
    ProviderModel, ProviderProbe, ProviderResumeCursor, ProviderRetryAction, QueuedMessage,
    ReasoningBlock, RuntimeMode, SessionStatus, SessionWorkspace, TranscriptBlock, TurnStatus,
    UserInputAnswer, UserInputQuestion, compact_path, unix_time, unix_time_millis,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::md::render::{
    Ctx as MarkdownCtx, MarkdownView, Metrics as MarkdownMetrics, Palette as MarkdownPalette,
    TranscriptSelection,
};
use crate::ui::menu::{
    ConfirmEntry, ContextMenuHandle, DismissMenu, MenuAlign, MenuItem, SelectNextEntry,
    SelectNextTab, SelectPreviousEntry, SelectPreviousTab, context_menu, dropdown_menu, popover,
};
use crate::ui::scrollbar::{self, ScrollbarState};
use crate::ui::tooltip::Tooltip;

use crate::browser::BrowserView;
use crate::persistence::{
    ComposerDraftStore, ComposerDrafts, DEFAULT_RIGHT_PANEL_WIDTH, DEFAULT_SIDEBAR_WIDTH,
    PersistedState, PersistedWindowState, StateStore,
};
use crate::query::{Query, QueryCache};
use crate::review_diff::{Snapshot as ReviewDiffSnapshot, Source as ReviewDiffSource};
use crate::terminal::TerminalView;
use crate::theme::{TextSizePreset, Theme, ThemePreference};
use crate::ui::text_field::TextField;
use crate::ui::{
    MenuChip, ProjectNameSelector, activity_noun, contain_scroll, file_icon, icon, icon_button,
    model_icon, motion, provider_color, provider_icon, spin_halo, status_color, toggle_switch,
};
use crate::{
    CancelTurn, CloseFind, CloseWindow, CopySelection, FindNext, FindPrevious, FocusComposer,
    NavigateBack, NavigateForward, NewProject, NewSession, OpenFind, OpenFindReplace, OpenSettings,
    ReplaceAllMatches, SaveFile, ToggleCommandPalette, ToggleFindCaseSensitive, ToggleFindRegex,
    ToggleFindWholeWord, ToggleFpsCounter, ToggleModelPicker, ToggleRightPanel, ToggleSidebar,
    ToggleUsagePanel,
};

const TRAFFIC_LIGHT_CLEARANCE: f32 = 8.0;
const CONTENT_MAX_WIDTH: f32 = 720.0;
/// Menu-registry id of the composer's model picker, shared by its render site
/// and the primary-modifier `/` toggle action.
const MODEL_PICKER_MENU_ID: &str = "provider-model-picker";
const BRANCH_PICKER_MENU_ID: &str = "workspace-branch-picker";
const BRANCH_PICKER_ROW_HEIGHT: f32 = 30.0;
const SIDEBAR_MIN_WIDTH: f32 = 180.0;
const SIDEBAR_MAX_WIDTH: f32 = 420.0;
const RIGHT_PANEL_MIN_WIDTH: f32 = 280.0;
const RIGHT_PANEL_MAX_WIDTH: f32 = 1000.0;
const DEFAULT_FILE_TREE_WIDTH: f32 = 184.0;
const FILE_TREE_MIN_WIDTH: f32 = 140.0;
const FILE_TREE_MAX_WIDTH: f32 = 360.0;
const FILE_EDITOR_MIN_WIDTH: f32 = 140.0;
const FILE_EDITOR_INITIAL_WIDTH: f32 = 500.0;
const REVIEW_INITIAL_WIDTH: f32 = 820.0;
const MAIN_PANEL_MIN_WIDTH: f32 = 360.0;
const FOLLOWUP_TURN_TOP_GAP: f32 = 48.0;
const NAVIGATION_RAIL_WIDTH: f32 = 44.0;
const NAVIGATION_RAIL_LEFT: f32 = 16.0;
const NAVIGATION_RAIL_CONTENT_GAP: f32 = 16.0;
const NAVIGATION_RAIL_VIEWPORT_HEIGHT_RATIO: f32 = 0.80;
const NAVIGATION_RAIL_TICK_WIDTH: f32 = 32.0;
const NAVIGATION_RAIL_TICK_HEIGHT: f32 = 2.0;
const NAVIGATION_RAIL_TICK_GAP: f32 = 10.0;
const NAVIGATION_RAIL_INACTIVE_OPACITY: f32 = 0.45;
const NAVIGATION_RAIL_TURN_HEIGHT: f32 = NAVIGATION_RAIL_TICK_HEIGHT + NAVIGATION_RAIL_TICK_GAP;
const NAVIGATION_RAIL_FADE_HEIGHT: f32 = 20.0;
/// Hover on the navigation rail is high-frequency — scrubbing the cursor
/// re-arms the tween on every tick it passes — so the sweep stays at or under
/// the 150 ms motion-restraint bound despite the rail's larger travel.
const NAVIGATION_RAIL_ANIMATION_DURATION: Duration = Duration::from_millis(150);
const ESCAPE_STOP_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(3);
/// Presentation pacing only. The app sleeps until a provider or background
/// result wakes it, then uses this cadence while streamed chunks remain.
/// 120 ms matches Zeron's `STREAM_COMMIT_MS`: chunks queue for a full
/// interval and fold into one drain → one notify → one remeasure, so the
/// per-commit parse/flatten/highlight work runs at ~8 Hz regardless of the
/// provider's chunk rate, and the veil dissolve spans the gap so streamed
/// text still reads as continuous.
const STREAM_FRAME_INTERVAL: Duration = Duration::from_millis(120);
/// How long a session may sit untouched before its provider process is released.
/// Codex and Pi stay resident between turns, so without this an afternoon of
/// abandoned tasks is an afternoon of idle agent processes.
const IDLE_SESSION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const IDLE_SESSION_SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const BACKGROUND_WORK_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const BACKGROUND_WORK_TICK_INTERVAL: Duration = Duration::from_secs(1);
const STREAM_SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// Zed keeps status toasts on screen for ten seconds, pausing the countdown
/// while the pointer is over the toast so a long message remains readable.
const DEFAULT_TOAST_DURATION: Duration = Duration::from_secs(5);
const MINIMUM_TOAST_RESUME_DURATION: Duration = Duration::from_millis(800);
const TOAST_ANIMATION_DURATION: Duration = Duration::from_millis(150);
const TASK_NOTIFICATION_TAG_PREFIX: &str = "fintwind-task:";

pub(crate) fn task_notification_tag(session_id: Uuid) -> String {
    format!("{TASK_NOTIFICATION_TAG_PREFIX}{session_id}")
}

pub(crate) fn task_id_from_notification_tag(tag: &str) -> Option<Uuid> {
    tag.strip_prefix(TASK_NOTIFICATION_TAG_PREFIX)?.parse().ok()
}

fn signal_event_pump(wake: &smol::channel::Sender<()>) {
    let _ = wake.try_send(());
}

/// Source bytes of parsed messages kept across session switches.
///
/// Measured at ~17x expansion into parsed structures, plus flattened text and
/// shaped runs on top, so this is bounded by source size rather than entry
/// count — one long message costs far more than several short ones. 512 KB
/// holds several sessions' transcripts for a few MB of structures.
const MAX_CACHED_MESSAGE_SOURCE_BYTES: usize = 512 * 1024;
/// Projects whose workspace lookups are remembered — branch, diff listing,
/// working tree. A window rarely has more than a handful open, and the diff
/// and tree caches are invalidated on every refresh, so they hold one entry in
/// practice. 8 is generous and caps the tree cache, the only large one, at a
/// few hundred KB.
const MAX_CACHED_WORKSPACES: usize = 8;
const STREAM_REMEASURE_TAIL_ROWS: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamPhase {
    Text,
    Reasoning,
    Activity,
}

/// The provider runtime's own account of a running turn's quiet stretches
/// (opencode `session.status`). Deltas and tool activity speak for
/// themselves; this phase labels the waits where neither exists — a model
/// call in flight — so the working row never reads as a frozen app. Cleared
/// by any visible output.
#[derive(Clone, Debug, PartialEq)]
enum ProviderPhase {
    /// `session.status busy`: a model call is in flight.
    Responding { since: u64 },
}

/// The provider's retry backoff (`session.status retry`). Held by the app —
/// not by the session runtime, which `drain_driver_events` briefly takes out of
/// its map, and not by the turn, which the backoff routinely outlives.
///
/// A retry outlives the turn that started it: the provider keeps backing off
/// after Fintwind has already settled — or failed — that turn, and while the
/// session is in that state the app used to accept no provider output at all
/// and silently drop every retry report. App scope is what lets the retry card
/// stay on screen exactly when the reader most needs it, without the fold ever
/// asking a runtime that is not currently installed.
#[derive(Clone, Debug, PartialEq)]
struct ProviderRetry {
    attempt: u32,
    message: String,
    action: Option<ProviderRetryAction>,
    next_at_ms: Option<u64>,
    /// When the app recorded this report. Prices the card's own expiry when
    /// the provider stamped no next attempt, so an unretracted report cannot
    /// pin the card — and the pulse lease behind it — on screen forever.
    received_at_ms: u64,
}

impl ProviderRetry {
    /// The moment this report stops describing a backoff in progress.
    ///
    /// opencode stamps the next attempt; a report whose stamp is long past was
    /// superseded — by a successful attempt, or by an event stream that died
    /// with the report unretracted. Without a stamp there is nothing to price
    /// the wait, so the report's own age stands in.
    fn stale_at_ms(&self) -> u64 {
        const STALE_AFTER_MS: u64 = 60_000;
        match self.next_at_ms {
            Some(next_at_ms) => next_at_ms.saturating_add(STALE_AFTER_MS),
            None => self.received_at_ms.saturating_add(STALE_AFTER_MS),
        }
    }

    /// Whether this report still describes a backoff in progress.
    fn is_live(&self, now_ms: u64) -> bool {
        now_ms < self.stale_at_ms()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamDeltaKind {
    Text,
    Reasoning,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ModelPickerTab {
    Favorites,
    Provider(String),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum BranchPickerMode {
    #[default]
    Browse,
    Create,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BranchPickerAction {
    Checkout(String),
    Create,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingsPage {
    General,
    Providers,
    Skills,
    McpServers,
    McpMarket,
    Usage,
    Appearance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PanelResizeTarget {
    Sidebar,
    RightPanel,
    FileTree,
}

#[derive(Clone, Copy, Debug)]
struct PanelResizeDrag {
    target: PanelResizeTarget,
    start_mouse_x: f32,
    start_width: f32,
}

#[derive(Debug)]
struct ToastState {
    message: String,
    tone: ToastTone,
    id: u64,
    timer_generation: u64,
    duration_remaining: Duration,
    timer_started: Option<Instant>,
    hovered: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToastTone {
    Alert,
    Success,
}

fn paused_toast_duration(remaining: Duration, elapsed: Duration) -> Duration {
    remaining
        .saturating_sub(elapsed)
        .max(MINIMUM_TOAST_RESUME_DURATION)
}

/// A file dropped onto the composer, staged as a chip until the next
/// submission sends it on OpenCode's prompt `files` channel.
#[derive(Clone, Debug)]
struct ComposerAttachment {
    /// Materialized path on the daemon host. This is the only path sent to a
    /// provider or persisted with a task.
    path: PathBuf,
    /// Ephemeral decoded client image used only for an immediate preview after
    /// upload. It is never persisted or sent to the daemon.
    client_preview_image: Option<Arc<gpui::Image>>,
    /// Tooltip path. Not sent to the provider; the driver uses `path`.
    mention: String,
    /// Basename drawn on the chip.
    name: SharedString,
    is_dir: bool,
    /// Whether the chip shows a thumbnail. Decided by extension at drop time
    /// so render never touches the filesystem.
    is_image: bool,
    /// Daemon-issued durable reference retained by task persistence.
    blob_reference: Option<String>,
}

#[derive(Clone, Debug)]
enum RemoteImageState {
    Loading,
    Ready(Arc<gpui::Image>),
    Unavailable,
}

/// One accepted composer submission. `prompt` is the typed text the provider
/// receives. Attachments stay beside it and leave on the `files` channel;
/// `display_content` is only set for older rows that hid appended `@` paths.
#[derive(Clone, Debug)]
struct ComposerSubmission {
    prompt: String,
    display_content: Option<String>,
    attachments: Vec<MessageAttachment>,
}

impl ComposerSubmission {
    fn plain(prompt: String) -> Self {
        Self {
            prompt,
            display_content: None,
            attachments: Vec::new(),
        }
    }

    fn into_queued_message(self) -> QueuedMessage {
        QueuedMessage::with_presentation(self.prompt, self.display_content, self.attachments)
    }

    fn from_queued_message(message: QueuedMessage) -> Self {
        Self {
            prompt: message.content,
            display_content: message.display_content,
            attachments: message.attachments,
        }
    }

    /// Text posted to OpenCode. New submissions store that text in `prompt`.
    /// Older queued rows kept the typed text in `display_content` and appended
    /// `@` paths to `prompt`; those paths must not be sent again now that the
    /// same chips travel as `files`.
    fn provider_prompt(&self) -> String {
        if !self.attachments.is_empty()
            && let Some(visible) = &self.display_content
        {
            return visible.clone();
        }
        self.prompt.clone()
    }

    fn prompt_files(&self) -> Vec<fintwind_client::PromptFile> {
        self.attachments
            .iter()
            .map(|attachment| fintwind_client::PromptFile {
                path: attachment.path.clone(),
                name: attachment.name.clone(),
            })
            .collect()
    }

    /// Human-facing task text for titles and generated worktree names. An
    /// attachment-only submission uses basenames. Providers still receive
    /// `prompt` unchanged, with attachments on the `files` channel.
    fn human_prompt(&self) -> String {
        let visible = self
            .display_content
            .as_deref()
            .unwrap_or(&self.prompt)
            .trim();
        if !visible.is_empty() {
            return visible.to_owned();
        }
        if !self.attachments.is_empty() {
            return self
                .attachments
                .iter()
                .map(|attachment| attachment.name.as_str())
                .collect::<Vec<_>>()
                .join(" ");
        }
        self.prompt.trim().to_owned()
    }
}

/// Whether an untouched session's provider process may be released.
///
/// A session mid-turn is not idle however long it has been quiet: a slow tool
/// call, or an approval waiting on the user, must not have its agent pulled out
/// from under it.
fn session_is_reapable(
    session: Option<&AgentSession>,
    idle_for: Duration,
    has_live_background_work: bool,
) -> bool {
    !has_live_background_work
        && idle_for >= IDLE_SESSION_TIMEOUT
        && session.is_none_or(|session| {
            session.active_turn_id().is_none()
                && matches!(session.status, SessionStatus::Idle | SessionStatus::Failed)
        })
}

fn sanitize_panel_width(width: f32, default: f32, min: f32, max: f32) -> f32 {
    if width.is_finite() {
        width.clamp(min, max)
    } else {
        default
    }
}

fn persisted_window_state(
    bounds: Bounds<Pixels>,
    maximized: bool,
    display: Option<Uuid>,
) -> PersistedWindowState {
    PersistedWindowState {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
        maximized,
        display,
    }
}

fn fitted_file_tree_width(panel_width: f32, file_tree_width: f32) -> f32 {
    let maximum = FILE_TREE_MAX_WIDTH
        .min(panel_width - FILE_EDITOR_MIN_WIDTH)
        .max(FILE_TREE_MIN_WIDTH);
    sanitize_panel_width(
        file_tree_width,
        DEFAULT_FILE_TREE_WIDTH.clamp(FILE_TREE_MIN_WIDTH, maximum),
        FILE_TREE_MIN_WIDTH,
        maximum,
    )
}

fn widened_panel_width_for_file_editor(panel_width: f32, file_tree_width: f32) -> f32 {
    let panel_width = sanitize_panel_width(
        panel_width,
        DEFAULT_RIGHT_PANEL_WIDTH,
        RIGHT_PANEL_MIN_WIDTH,
        RIGHT_PANEL_MAX_WIDTH,
    );
    let file_tree_width = sanitize_panel_width(
        file_tree_width,
        DEFAULT_FILE_TREE_WIDTH,
        FILE_TREE_MIN_WIDTH,
        FILE_TREE_MAX_WIDTH,
    );
    panel_width
        .max(file_tree_width + FILE_EDITOR_INITIAL_WIDTH)
        .min(RIGHT_PANEL_MAX_WIDTH)
}

fn widened_panel_width_for_review(panel_width: f32) -> f32 {
    sanitize_panel_width(
        panel_width,
        DEFAULT_RIGHT_PANEL_WIDTH,
        RIGHT_PANEL_MIN_WIDTH,
        RIGHT_PANEL_MAX_WIDTH,
    )
    .max(REVIEW_INITIAL_WIDTH)
}

fn fitted_panel_widths(
    viewport_width: f32,
    sidebar_visible: bool,
    right_panel_visible: bool,
    sidebar_width: f32,
    right_panel_width: f32,
) -> (f32, f32) {
    let sidebar_min = if sidebar_visible {
        SIDEBAR_MIN_WIDTH
    } else {
        0.0
    };
    let right_panel_min = if right_panel_visible {
        RIGHT_PANEL_MIN_WIDTH
    } else {
        0.0
    };
    let mut sidebar = if sidebar_visible {
        sanitize_panel_width(
            sidebar_width,
            DEFAULT_SIDEBAR_WIDTH,
            SIDEBAR_MIN_WIDTH,
            SIDEBAR_MAX_WIDTH,
        )
    } else {
        0.0
    };
    let mut right_panel = if right_panel_visible {
        sanitize_panel_width(
            right_panel_width,
            DEFAULT_RIGHT_PANEL_WIDTH,
            RIGHT_PANEL_MIN_WIDTH,
            RIGHT_PANEL_MAX_WIDTH,
        )
    } else {
        0.0
    };

    let available = (viewport_width - MAIN_PANEL_MIN_WIDTH).max(0.0);
    let mut overflow = (sidebar + right_panel - available).max(0.0);
    let right_reduction = overflow.min((right_panel - right_panel_min).max(0.0));
    right_panel -= right_reduction;
    overflow -= right_reduction;
    let sidebar_reduction = overflow.min((sidebar - sidebar_min).max(0.0));
    sidebar -= sidebar_reduction;
    overflow -= sidebar_reduction;

    // The configured minimum window easily fits both panel minima. This final
    // fallback only protects layout if the host temporarily reports a smaller
    // viewport during a resize or display transition.
    if overflow > 0.0 {
        let right_reduction = overflow.min(right_panel);
        right_panel -= right_reduction;
        overflow -= right_reduction;
        sidebar = (sidebar - overflow).max(0.0);
    }

    (sidebar, right_panel)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RightPanelSurface {
    Context,
    Browser(Uuid),
    Terminal(Uuid),
    BackgroundWork {
        key: BackgroundWorkKey,
        title: String,
    },
    Files,
    Diff,
    File(String),
}

/// A turn whose checkpoint still has to be captured.
struct PendingCheckpointCapture {
    session_id: Uuid,
    turn_count: usize,
    project_path: PathBuf,
}

/// Sessions between accepting a submission and handing it to a provider.
///
/// Worktree creation and the pre-turn checkpoint both run off the UI thread,
/// but neither operation has a safe interrupt contract. Keeping this separate
/// from [`SessionStatus`] lets the composer distinguish that non-cancellable
/// preparation window from a connecting provider that can already be stopped.
struct PreparedSubmission {
    workspace: SessionWorkspace,
    checkpoint_warning: Option<String>,
    /// `None` reuses an already-live runtime. `Some` contains the result of a
    /// provider process start performed on the background executor.
    driver: Option<anyhow::Result<PreparedDriver>>,
}

/// Everything needed to start a provider process, captured while the session
/// is still on the UI thread. `cwd` is replaced with the materialized
/// worktree path by the background preparation task.
struct DriverStartRequest {
    session_id: Uuid,
    options: DriverStartOptions,
    event_wake: smol::channel::Sender<()>,
    daemon_client: fintwind_client::DaemonClient,
}

/// A provider process that has started off-thread but is not installed into
/// Fintwind's runtime map yet. Its event receiver safely buffers early events.
struct PreparedDriver {
    handle: DriverHandle,
    events: Receiver<DriverEvent>,
}

struct RemoteTaskStateSnapshot {
    projects: Vec<Project>,
    sessions: Vec<AgentSession>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EscapeStopTarget {
    session_id: Uuid,
    turn_id: Option<Uuid>,
}

impl EscapeStopTarget {
    fn for_session(session: &AgentSession) -> Self {
        Self {
            session_id: session.id,
            turn_id: session.active_turn_id(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EscapeStopPress {
    Arm(EscapeStopArm),
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EscapeStopArm {
    target: EscapeStopTarget,
    expires_at: Instant,
}

#[derive(Default)]
struct EscapeStopConfirmation {
    arm: Option<EscapeStopArm>,
}

impl EscapeStopConfirmation {
    fn press(&mut self, target: EscapeStopTarget, now: Instant) -> EscapeStopPress {
        if self
            .arm
            .is_some_and(|arm| arm.target == target && now < arm.expires_at)
        {
            self.arm = None;
            EscapeStopPress::Stop
        } else {
            let arm = EscapeStopArm {
                target,
                expires_at: now + ESCAPE_STOP_CONFIRMATION_TIMEOUT,
            };
            self.arm = Some(arm);
            EscapeStopPress::Arm(arm)
        }
    }

    fn is_armed_for(&self, target: EscapeStopTarget, now: Instant) -> bool {
        self.arm
            .is_some_and(|arm| arm.target == target && now < arm.expires_at)
    }

    fn expire(&mut self, arm: EscapeStopArm) -> bool {
        if self.arm != Some(arm) {
            return false;
        }
        self.arm = None;
        true
    }

    fn clear(&mut self) {
        self.arm = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventPumpSchedule {
    Idle,
    StreamFrame,
    BackgroundOutput(Duration),
}

/// One cached island of the root view: a region rendered by delegating back
/// into [`Fintwind`] under its own view identity.
///
/// All state stays on the root entity; what the island buys is scope for
/// gpui's cached-view machinery. The pulse clock and the streaming veil lease
/// `window.current_view()`, so their ~30 fps ticks dirty only the island
/// hosting the animation while every sibling island replays its cached
/// subtree instead of rebuilding. Observing the root preserves the old
/// invalidation semantics exactly — any root notify still re-renders every
/// island — so caching cannot show state the single-view architecture would
/// have repainted.
struct FintwindPane {
    fintwind: Option<WeakEntity<Fintwind>>,
    content: fn(&mut Fintwind, &mut Window, &mut Context<Fintwind>) -> AnyElement,
}

impl FintwindPane {
    fn new(
        content: fn(&mut Fintwind, &mut Window, &mut Context<Fintwind>) -> AnyElement,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|_| Self {
            fintwind: None,
            content,
        })
    }

    fn bind(&mut self, fintwind: &Entity<Fintwind>, cx: &mut Context<Self>) {
        self.fintwind = Some(fintwind.downgrade());
        cx.observe(fintwind, |_, fintwind, cx| {
            // A panel slide notifies the root at display rate for its 200ms,
            // and this fan-out would price every one of those ticks at a
            // three-island rebuild. Skipping it hands the decision to the
            // cached-view keys: the sliding panel (its clip moves) and the
            // transcript (its bounds move) miss their caches and re-render
            // with fresh state anyway, while the island nothing is moving
            // re-plays its cached subtree. Root-state changes it displays
            // can wait out the slide: updates born inside an island
            // (terminal output, pulse leases) dirty their ancestor pane
            // without this observer, and the slide's retirement notify
            // below re-runs the fan-out, so nothing outlasts the 200ms.
            if !fintwind.read(cx).panels_sliding() {
                cx.notify();
            }
        })
        .detach();
    }
}

impl Render for FintwindPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(fintwind) = self.fintwind.as_ref().and_then(WeakEntity::upgrade) else {
            return gpui::div().into_any_element();
        };
        let content = self.content;
        fintwind.update(cx, |fintwind, cx| content(fintwind, window, cx))
    }
}

struct RightPanelFileEditor {
    state: Entity<ComposerInput>,
    disk_content: String,
    writable: bool,
    dirty: bool,
    /// A read is in flight on the background executor. Set from the moment the
    /// editor is created, because `render` may not touch the filesystem: until
    /// the first read lands the editor is empty and locked, and that means
    /// "not read yet", never "empty file".
    reading: bool,
    /// Bumped whenever the editor's idea of the file changes, so a read that
    /// started earlier cannot apply over a newer truth — a save in particular,
    /// which makes any read already in flight describe the pre-save file.
    read_epoch: u64,
}

struct RightPanelSessionState {
    visible: bool,
    surfaces: Vec<RightPanelSurface>,
    active_surface: Option<usize>,
    tabs_scroll_handle: ScrollHandle,
    pending_tab_reveal: Option<usize>,
    expanded_paths: HashSet<PathBuf>,
    files_selected_path: Option<String>,
    file_tree_width: f32,
    file_editors: HashMap<String, RightPanelFileEditor>,
    diff_source: ReviewDiffSource,
    diff_snapshot: Option<Arc<ReviewDiffSnapshot>>,
    diff_selected_file: Option<usize>,
    diff_expanded_paths: HashSet<String>,
}

impl RightPanelSessionState {
    fn empty(visible: bool) -> Self {
        Self {
            visible,
            surfaces: Vec::new(),
            active_surface: None,
            tabs_scroll_handle: ScrollHandle::new(),
            pending_tab_reveal: None,
            expanded_paths: HashSet::new(),
            files_selected_path: None,
            file_tree_width: DEFAULT_FILE_TREE_WIDTH,
            file_editors: HashMap::new(),
            diff_source: ReviewDiffSource::default(),
            diff_snapshot: None,
            diff_selected_file: None,
            diff_expanded_paths: HashSet::new(),
        }
    }

    fn take_or_closed(states: &mut HashMap<Uuid, Self>, session_id: Uuid) -> Self {
        states
            .remove(&session_id)
            .unwrap_or_else(|| Self::empty(false))
    }
}

/// One choice in the model-traits menu: a label plus a badge marking the
/// provider's own default, so the current selection and the default read apart.
fn traits_choice(theme: Theme, label: String, is_default: bool, selected: bool) -> MenuItem {
    MenuItem::custom(move |_, _| {
        div()
            .w(px(190.0))
            .py(px(2.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_color(theme.text_secondary)
                    .child(label.clone()),
            )
            .when(is_default, |element| {
                element.child(
                    div()
                        .h(px(16.0))
                        .px(px(5.0))
                        .flex_none()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(theme.border_strong)
                        .bg(theme.overlay)
                        .flex()
                        .items_center()
                        .text_size(ui_px(9.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text_tertiary)
                        .child(tr!("common.default")),
                )
            })
            .when(selected, |element| {
                element.child(icon("icons/check.svg", 11.0, theme.text_tertiary))
            })
            .into_any_element()
    })
}

#[derive(Clone, Copy, Debug)]
struct UserMessageAction {
    session_id: Uuid,
    /// The prompt that opened the turn. A turn can hold several user messages
    /// once a steer lands in it, and only the opening one is a rewind
    /// boundary, so this is what the editor and the rewind both address.
    message_id: Uuid,
    turn_count: usize,
}

/// Why a settled user message cannot offer the rewind affordance. The
/// transcript keeps the affordance visible in these cases — disabled, with
/// the reason as its tooltip — so a missing control never reads as a
/// rendering bug.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RewindUnavailableReason {
    /// A steer joins an already-opened turn, and only the prompt that opened
    /// the turn is a rewind boundary.
    NotTurnOpening,
    /// The workspace is not a Git repository, so no turn snapshot can exist.
    NotGitRepository,
    /// The turn's checkpoint capture failed, leaving no baseline to restore.
    CheckpointError,
    /// The baseline snapshot has not landed yet — the capture or the ref
    /// prefetch is still in flight, and the affordance becomes available
    /// once the refs settle.
    SnapshotMissing,
    /// The local row lost its provider association, so the provider side of
    /// the rewind cannot be driven.
    ProviderLinkMissing,
}

/// What one user message's rewind affordance should render.
#[derive(Clone, Copy, Debug)]
enum UserMessageRewind {
    /// The rewind can start from this message.
    Ready(UserMessageAction),
    /// Render the affordance disabled with the reason as its tooltip.
    Blocked(RewindUnavailableReason),
    /// Not eligible at all — no affordance rendered.
    Hidden,
}

impl UserMessageRewind {
    /// The action when the rewind can actually start from this message.
    fn ready(self) -> Option<UserMessageAction> {
        match self {
            Self::Ready(action) => Some(action),
            Self::Blocked(_) | Self::Hidden => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct AssistantMessageAction {
    session_id: Uuid,
    turn_count: usize,
    enabled: bool,
    preparing: bool,
}

#[derive(Clone)]
struct MessageEdit {
    session_id: Uuid,
    /// Identifies the edited bubble outright. Matching by turn alone would
    /// open this one input on every user message the turn holds.
    message_id: Uuid,
    turn_count: usize,
    input: Entity<ComposerInput>,
    attachments: Vec<MessageAttachment>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TranscriptAnchor {
    session_id: Uuid,
    turn_id: Uuid,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NavigationRailVisualState {
    emphasized_turn: Option<Uuid>,
}

struct SessionRuntime {
    driver: DriverHandle,
    /// Invalidates stale ApplyOptions responses when settings change again or
    /// this runtime is replaced while the RPC is in flight.
    options_generation: u64,
    events: Receiver<DriverEvent>,
    pending_events: VecDeque<DriverEvent>,
    /// Presentation metadata for steering messages awaiting the provider's
    /// accepted/rejected acknowledgement, in transport order.
    pending_steers: VecDeque<ComposerSubmission>,
    stream_phase: Option<StreamPhase>,
    stream_remeasure_pending: bool,
    /// The provider's open reasoning fragments (opencode part keys) mapped to
    /// the transcript activity each one streams into. Deltas of a fragment
    /// can land after intervening tool events, so binding — not transcript
    /// position — decides where they append. Presentation state only: it
    /// never persists and is cleared with the phase at turn boundaries.
    open_reasoning: HashMap<String, Uuid>,
    /// Reasoning fragments that already settled from their authoritative end
    /// event. A tail delta can trail its own `ended` on the wire (the provider
    /// flushes buffered deltas asynchronously); the fragment's text is then
    /// already complete in its block, so further deltas for the key are
    /// dropped instead of opening a stray fragment. A fresh `started` reopens
    /// the key. Cleared with `open_reasoning`.
    settled_reasoning: HashSet<String>,
    /// Open text parts (`text:{message}:{ordinal}`) mapped to the assistant
    /// message each one streams into. A provider flushes a part's batched tail
    /// after the next tool's events have already landed; the key routes that
    /// tail back into its own message instead of opening a new one after the
    /// tool. Presentation only: it never persists, and it is cleared with the
    /// phase at turn boundaries.
    open_text: HashMap<String, Uuid>,
    /// Text parts that already settled from `session.text.ended`. A tail delta
    /// after the authoritative text is dropped. A fresh `started` reopens the
    /// key. Cleared with `open_text`.
    settled_text: HashSet<String>,
    /// The provider's busy report for the live turn, if any. Runtime
    /// presentation state: it never persists and always dies with the turn.
    provider_phase: Option<ProviderPhase>,
    pending_permission: Option<PendingPermission>,
    pending_user_input: Option<PendingUserInput>,
    last_driver_error: Option<String>,
    /// When this session last sent or received anything, for idle reaping.
    last_active_at: Instant,
    /// Background-process snapshots are provider IPC. Keep the polling clock
    /// on the runtime so switching tasks never creates duplicate probes.
    last_background_refresh_at: Instant,
}

#[derive(Clone)]
struct PendingUserInput {
    request_id: String,
    questions: Vec<UserInputQuestion>,
    question_index: usize,
    selections: HashMap<String, Vec<String>>,
    custom_answers: HashMap<String, String>,
    /// Whether the card body is folded to its header row. Runtime-only
    /// presentation state: a fresh request always opens.
    collapsed: bool,
}

impl PendingUserInput {
    fn new(request_id: String, questions: Vec<UserInputQuestion>) -> Self {
        Self {
            request_id,
            questions,
            question_index: 0,
            selections: HashMap::new(),
            custom_answers: HashMap::new(),
            collapsed: false,
        }
    }

    fn current_question(&self) -> Option<&UserInputQuestion> {
        self.questions.get(self.question_index)
    }

    fn answers(&self) -> Vec<UserInputAnswer> {
        self.questions
            .iter()
            .map(|question| {
                let custom = self
                    .custom_answers
                    .get(&question.id)
                    .map(|answer| answer.trim())
                    .filter(|answer| !answer.is_empty());
                UserInputAnswer {
                    question_id: question.id.clone(),
                    answers: custom.map_or_else(
                        || {
                            self.selections
                                .get(&question.id)
                                .cloned()
                                .unwrap_or_default()
                        },
                        |answer| vec![answer.to_owned()],
                    ),
                }
            })
            .collect()
    }
}

#[derive(Debug, Default)]
struct SessionNavigation {
    back: Vec<Uuid>,
    forward: Vec<Uuid>,
    /// The unstarted task behind the global New Task entry. Viewing another
    /// session must not make that entry forget the project chosen for it.
    new_task: Option<Uuid>,
}

impl SessionNavigation {
    fn visit(&mut self, current: Option<Uuid>, next: Uuid) {
        if let Some(current) = current.filter(|current| *current != next) {
            self.back.push(current);
            self.forward.clear();
        }
    }

    fn go_back(&mut self, current: Uuid) -> Option<Uuid> {
        let target = self.back.pop()?;
        self.forward.push(current);
        Some(target)
    }

    fn back_target(&self) -> Option<Uuid> {
        self.back.last().copied()
    }

    fn go_forward(&mut self, current: Uuid) -> Option<Uuid> {
        let target = self.forward.pop()?;
        self.back.push(current);
        Some(target)
    }

    fn forward_target(&self) -> Option<Uuid> {
        self.forward.last().copied()
    }

    fn remove(&mut self, session_id: Uuid) {
        self.back.retain(|entry| *entry != session_id);
        self.forward.retain(|entry| *entry != session_id);
        if self.new_task == Some(session_id) {
            self.new_task = None;
        }
    }

    fn remember_new_task(&mut self, session_id: Uuid) {
        self.new_task = Some(session_id);
    }

    fn remembered_new_task(&self, sessions: &[AgentSession]) -> Option<Uuid> {
        self.new_task.filter(|session_id| {
            sessions
                .iter()
                .any(|session| session.id == *session_id && !session.has_started())
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionActivationTransition {
    Visit,
    Back { from: Uuid },
    Forward { from: Uuid },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingSessionActivation {
    session_id: Uuid,
    transition: SessionActivationTransition,
}

#[derive(Clone)]
struct ActivityScrollViewport {
    scroll_handle: ScrollHandle,
    scrollbar: Rc<ScrollbarState>,
    follow_tail: Rc<Cell<bool>>,
    last_scrolled: Rc<Cell<Option<Pixels>>>,
    last_max_offset: Rc<Cell<Option<Pixels>>>,
}

impl Default for ActivityScrollViewport {
    fn default() -> Self {
        Self {
            scroll_handle: ScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
            follow_tail: Rc::new(Cell::new(true)),
            last_scrolled: Rc::new(Cell::new(None)),
            last_max_offset: Rc::new(Cell::new(None)),
        }
    }
}

pub struct Fintwind {
    /// Owns the headless provider process for exactly as long as the desktop
    /// app entity. Debug builds can replace it independently after a rebuild;
    /// all live driver handles below are lightweight RPC proxies.
    daemon: fintwind_client::DaemonSupervisor,
    /// Session details currently being fetched from the daemon. Sidebar rows
    /// stay usable while the selected transcript hydrates asynchronously.
    session_hydrations: HashSet<Uuid>,
    /// Selection is committed only after this target's transcript arrives, so
    /// the currently visible task stays intact during daemon latency.
    pending_session_activation: Option<PendingSessionActivation>,
    state: PersistedState,
    store: StateStore,
    /// Cached before rendering so path labels can abbreviate the home prefix
    /// without consulting the environment or account database in a frame.
    composer: Entity<ComposerInput>,
    user_input_answer: Entity<ComposerInput>,
    /// Drafts are independent of transcript persistence: started tasks key by
    /// session id, while blank New Task pages key by project id.
    composer_drafts: ComposerDrafts,
    composer_draft_store: ComposerDraftStore,
    composer_draft_save_generation: u64,
    command_palette: command_palette::CommandPaletteUi,
    model_search: Entity<ComposerInput>,
    settings_search: Entity<ComposerInput>,
    settings_focus: FocusHandle,
    latest_available: Option<String>,
    onboarding_add_project_focus: FocusHandle,
    onboarding_projectless_focus: FocusHandle,
    sidebar_add_project_focus: FocusHandle,
    probes: Vec<ProviderProbe>,
    provider_probe_tx: Sender<ProviderProbe>,
    provider_probe_events: Receiver<ProviderProbe>,
    provider_model_discoveries: HashSet<String>,
    provider_model_discoveries_pending: HashSet<String>,
    /// CLI version, probed off-thread. Missing key means the probe has not
    /// answered yet; `None` means it ran and found nothing.
    provider_versions: HashMap<String, Option<String>>,
    provider_version_tx: Sender<(String, Option<String>)>,
    provider_version_events: Receiver<(String, Option<String>)>,
    /// Providers with a version probe in flight, so a re-detect cannot stack
    /// a second subprocess on one that has not answered.
    provider_version_probes_pending: HashSet<String>,
    /// Fast provider detection results from the daemon, including its cached
    /// model catalog. Live discovery revalidates these probes afterward.
    provider_detection_tx: Sender<ProviderProbe>,
    provider_detection_events: Receiver<ProviderProbe>,
    /// Providers the running re-detection has not answered for yet; empty
    /// means no re-detection is in flight.
    provider_detection_remaining: usize,
    /// When provider detection last completed, for the page's "Checked" label.
    provider_detection_checked_at: Option<Instant>,
    model_picker_tab: ModelPickerTab,
    /// Keyboard cursor over the model picker's filtered rows. `None` means the
    /// keyboard has not moved yet, so `enter` takes the first row.
    model_picker_highlight: Option<usize>,
    model_picker_scroll: ScrollHandle,
    model_picker_scrollbar: Rc<ScrollbarState>,
    branch_search: Entity<ComposerInput>,
    branch_create_input: Entity<ComposerInput>,
    branch_picker_mode: BranchPickerMode,
    /// Keyboard cursor over the branch picker's enabled actions. Disabled
    /// rows remain visible but never enter this index.
    branch_picker_highlight: Option<usize>,
    branch_picker_list_state: ListState,
    branch_picker_row_cache: RefCell<Vec<crate::git_branch::BranchEntry>>,
    /// Git subprocess results per concrete workspace path. Render only reads
    /// this in-memory cache; misses are fulfilled on the background executor.
    branch_snapshots: QueryCache<PathBuf, Result<Option<BranchSnapshot>, String>>,
    /// The branch name drawn on each sidebar session card, per concrete
    /// workspace path. This is the lightweight branch query rather than the
    /// full [`Self::branch_snapshots`], and its capacity is larger because the
    /// sidebar asks for every visible project, not just the selected one.
    /// Failures are cached too, so a broken path is not retried every frame.
    sidebar_branches: QueryCache<PathBuf, Result<Option<String>, ()>>,
    /// Stale-while-revalidate value for the selected path, avoiding label
    /// flicker when app activation invalidates the query.
    visible_branch_snapshot: Option<(PathBuf, BranchSnapshot)>,
    branch_operation_pending: bool,
    /// Window-modal Git commit/push UI. Its repository snapshot is filled
    /// off-thread; frames only read this in-memory value.
    commit_dialog: Option<commit_dialog::CommitDialogState>,
    /// Commit-message generation and Git mutation outlive the modal that
    /// started them. Keeping the operation on the app also lets every
    /// Environment surface reflect and gate the same in-flight action.
    commit_operation: Option<commit_dialog::CommitOperationState>,
    /// Slash commands discovered per (provider, project root). Filesystem
    /// walks live on the background executor; frames read the index below.
    slash_commands: QueryCache<(String, PathBuf), Vec<SlashCommand>>,
    /// The merged command list the autocomplete popup draws, and the key it
    /// was built for — a stale key means "no commands", never another
    /// provider's list.
    slash_command_index: Rc<Vec<SlashCommand>>,
    slash_command_index_key: Option<(String, PathBuf)>,
    /// Workspace file index per project root, for `@` mentions.
    mention_files: QueryCache<PathBuf, Vec<FileEntry>>,
    mention_file_index: Rc<Vec<FileEntry>>,
    mention_file_index_path: Option<PathBuf>,
    /// Set when a driver reports its command registry mid-drain; the drain
    /// has no `Context` to rebuild the drawn index itself.
    composer_sources_stale: bool,
    composer_autocomplete: autocomplete::AutocompleteUi,
    /// Files dropped onto the composer, drawn as chips above the input and
    /// drained into the next submission.
    composer_attachments: Vec<ComposerAttachment>,
    /// Window-modal expansion of an image attachment. The path is already
    /// cached attachment metadata; render never probes the filesystem.
    image_preview: Option<image_preview::ImagePreviewState>,
    image_preview_generation: u64,
    /// In-memory GPUI images for daemon-owned bytes. A missing entry schedules
    /// one background fetch only when a visible row asks to render it; the
    /// desktop never creates another attachment file.
    remote_images: RefCell<HashMap<String, RemoteImageState>>,
    /// Coalesced edge trigger for provider and background result queues. The
    /// payloads stay in their typed channels; this channel only wakes the UI.
    event_wake_tx: smol::channel::Sender<()>,
    task_state_sync_tx: Sender<Result<RemoteTaskStateSnapshot, String>>,
    task_state_sync_events: Receiver<Result<RemoteTaskStateSnapshot, String>>,
    runtimes: HashMap<Uuid, SessionRuntime>,
    runtime_attach_pending: HashSet<Uuid>,
    runtime_attach_misses: HashMap<Uuid, u8>,
    /// Provider-neutral session work which may remain live after a turn ends.
    /// Runtime-only by design: providers reconcile their authoritative state
    /// when the resident transport reconnects.
    background_work: HashMap<Uuid, BackgroundWorkRegistry>,
    /// Latest plan (todo) display model per session, rebuilt off the render
    /// path when plan activities arrive, a session hydrates, or the selection
    /// changes. Render only reads this store.
    todo_summaries: RefCell<HashMap<Uuid, Rc<TodoSummary>>>,
    last_background_work_tick: Instant,
    /// Accepted submissions still creating their workspace/checkpoint, or an
    /// edited past message still rewinding its workspace and provider. The
    /// session is busy immediately, while the composer draws a spinner until
    /// the non-cancellable preparation is complete.
    submission_preparations: HashSet<Uuid>,
    /// First Escape press for the current turn. A matching second press stops
    /// the response; otherwise this returns to the ordinary Stop icon after a
    /// short timeout.
    escape_stop_confirmation: EscapeStopConfirmation,
    /// Response fork target per source session while its provider-native
    /// branch and Git checkpoint refs are being prepared off the UI thread.
    /// A source can have only one in flight because Pi temporarily changes
    /// its resident session while producing a branch.
    response_fork_preparations: HashMap<Uuid, usize>,
    /// Sessions whose just-settled turn should start the next queued
    /// follow-up. The request stays here until the ending checkpoint lands, so
    /// the next provider cannot edit the worktree while that snapshot is still
    /// being collected; it then reuses the runtime after the event drain has
    /// re-inserted it.
    pending_queue_drains: Vec<Uuid>,
    stream_state_dirty: bool,
    last_stream_save: Instant,
    /// User expansion overrides keyed by persisted transcript block index.
    activities_expanded: HashMap<usize, bool>,
    /// Per-item disclosure overrides. Reasoning starts open while live; tool
    /// details start closed, so the stored bool must preserve either choice.
    expanded_activity_items: HashMap<Uuid, bool>,
    /// Settled turns whose folded work the user has reopened.
    expanded_turns: HashSet<Uuid>,
    /// Per-response file cards the user expanded beyond their three-file
    /// preview. Runtime-only, like the other transcript disclosures.
    expanded_changed_files: HashSet<Uuid>,
    /// Settled compaction cards the user expanded; collapsed is the default,
    /// the summary belongs to the provider's bookkeeping more than to the
    /// reader. Runtime-only, like the other transcript disclosures.
    expanded_compactions: HashSet<Uuid>,
    /// Sessions whose provider-retry card the user expanded to read the full
    /// error. Collapsed is the default: the header already carries the attempt
    /// and countdown. Runtime-only.
    expanded_provider_retries: HashSet<Uuid>,
    /// The provider's live retry backoff per session, if any. App-level rather
    /// than runtime-level: `drain_driver_events` removes a runtime from map
    /// while it handles that session's events, and a fold read through the
    /// absent runtime would rebuild the rows without the retry card. Keyed by
    /// session so a backoff in a background task is remembered when the reader
    /// switches to it. Runtime-only, and pruned on expiry, submission, stop,
    /// and session removal.
    provider_retries: HashMap<Uuid, ProviderRetry>,
    /// Stable focus identities for controls inside virtualized transcript and
    /// diff rows. Recreating a handle on every row build would drop keyboard
    /// focus whenever GPUI re-renders the list.
    transcript_control_focuses: RefCell<HashMap<String, FocusHandle>>,
    session_navigation: SessionNavigation,
    /// Sidebar task currently showing its inline rename field.
    session_rename: Option<Uuid>,
    /// One stable field reused across sidebar rows so virtualization never
    /// replaces the focused editor while a rename is in progress.
    session_rename_input: Entity<ComposerInput>,
    /// Sidebar projects the user has unfolded, by project id. Inverted from a
    /// collapsed set so the default — an empty set — folds every project at
    /// launch; the selection's own project is revealed when the app opens and
    /// whenever the selected task changes. Intentionally runtime-only, like
    /// transcript disclosure state.
    sidebar_expanded_groups: HashSet<Uuid>,
    sidebar_visible: bool,
    sidebar_width: f32,
    right_panel_visible: bool,
    right_panel_width: f32,
    /// The show/hide slide each panel is in the middle of, if any. Driven by
    /// hand from `render` (see [`motion::WidthTween`]) because the width these
    /// produce is what the transcript column between them is laid out against.
    sidebar_slide: Option<motion::WidthTween>,
    right_panel_slide: Option<motion::WidthTween>,
    /// Width each panel actually occupied in the last frame — where a toggle
    /// starts its slide from, and what the transcript measures itself against
    /// while one is running.
    sidebar_rendered_width: f32,
    right_panel_rendered_width: f32,
    fps_counter_visible: bool,
    panel_resize_drag: Option<PanelResizeDrag>,
    right_panel_session_states: HashMap<Uuid, RightPanelSessionState>,
    context_summary_ids: HashMap<Uuid, Option<Uuid>>,
    right_panel_surfaces: Vec<RightPanelSurface>,
    right_panel_active_surface: Option<usize>,
    right_panel_tabs_scroll_handle: ScrollHandle,
    context_panel_focus: FocusHandle,
    context_meter_focus: FocusHandle,
    context_compact_focus: FocusHandle,
    context_summary_focus: FocusHandle,
    context_tab_focus: FocusHandle,
    context_tab_close_focus: FocusHandle,
    right_panel_files_scroll_handle: ScrollHandle,
    right_panel_files_scrollbar: Rc<ScrollbarState>,
    right_panel_diff_filter: Entity<ComposerInput>,
    /// Unified diff rows and changed-file tree rows are independently
    /// virtualized. Large generated patches stay proportional to what is on
    /// screen rather than the size of the repository change.
    right_panel_diff_list_state: ListState,
    right_panel_diff_scrollbar: Rc<ScrollbarState>,
    /// Selection spans and visible glyph geometry for the Review surface.
    /// Kept separate from the transcript because both surfaces paint at once.
    right_panel_diff_selection: TranscriptSelection,
    right_panel_diff_tree_list_state: ListState,
    right_panel_diff_tree_scrollbar: Rc<ScrollbarState>,
    right_panel_editor_scroll_handle: ScrollHandle,
    right_panel_editor_scrollbar: Rc<ScrollbarState>,
    right_panel_pending_tab_reveal: Option<usize>,
    right_panel_pending_terminal_focus: Option<Uuid>,
    right_panel_expanded_paths: HashSet<PathBuf>,
    right_panel_files_selected_path: Option<String>,
    right_panel_file_tree_width: f32,
    right_panel_file_editors: HashMap<String, RightPanelFileEditor>,
    /// Find-and-replace over the visible file editor. Created on first use of
    /// the primary find shortcut and kept for the window's lifetime so the
    /// query and toggles survive closing the bar; `open` says whether it shows.
    file_search: Option<file_search::FileSearch>,
    right_panel_diff_source: ReviewDiffSource,
    right_panel_diff_snapshot: Option<Arc<ReviewDiffSnapshot>>,
    right_panel_diff_loading: bool,
    right_panel_diff_error: Option<String>,
    right_panel_diff_generation: u64,
    right_panel_diff_selected_file: Option<usize>,
    right_panel_diff_expanded_paths: HashSet<String>,
    right_panel_diff_tree_rows: RefCell<Vec<right_panel::ReviewDiffTreeRow>>,
    right_panel_diff_tree_cursor: Option<usize>,
    /// The working tree as currently drawn. Held so a refresh can redraw the
    /// previous listing instead of blanking the panel.
    right_panel_working_tree: Vec<right_panel::WorkingTreeEntry>,
    /// Working tree per project path. Walking it is filesystem I/O and must
    /// never happen in a frame.
    working_trees: QueryCache<PathBuf, Vec<right_panel::WorkingTreeEntry>>,
    /// Set when a turn finishes; the drain loop drops the workspace queries,
    /// since the event handler has no `Context` to refresh them itself.
    workspace_queries_stale: bool,
    right_panel_terminals: HashMap<Uuid, Entity<TerminalView>>,
    right_panel_browsers: HashMap<Uuid, Entity<BrowserView>>,
    /// A Browser surface was just opened; the next right panel render moves
    /// focus into its address bar.
    right_panel_pending_browser_focus: Option<Uuid>,
    /// GPUI is compositing deferred draws on a plane above native views, so
    /// menus render over the live webview and no snapshot occlusion is needed.
    /// When the overlay could not be enabled, the browser falls back to
    /// swapping in frozen page pixels while an overlay is open.
    scene_overlay_enabled: bool,
    settings_page: Option<SettingsPage>,
    /// The Skills page's library snapshot, scanned off-thread. Frames read
    /// only this; `None` means the first scan has not landed yet.
    skills_catalog: Option<Rc<crate::skills::SkillsCatalog>>,
    /// Bumped per scan; a result from a superseded scan is discarded.
    skills_scan_generation: u64,
    skills_scan_pending: bool,
    /// When the current catalog landed, for the reopen-staleness check.
    skills_scanned_at: Option<Instant>,
    /// Filter query over the Skills page's rows.
    skills_search: Entity<ComposerInput>,
    /// Virtualized list over the filtered skill rows.
    skills_list_state: ListState,
    skills_scrollbar: Rc<ScrollbarState>,
    /// The rows the list currently draws — sections and catalog indices —
    /// refreshed once per frame rather than per row.
    skills_rows: RefCell<Vec<skills_page::SkillsRow>>,
    /// The skill directory the detail pane shows. `None` falls back to the
    /// first visible row, so the pane never opens empty.
    skills_selected: Option<PathBuf>,
    /// Parsed markdown for the selected skill's document, keyed by the skill
    /// directory it was built from. One entry: only one detail shows at once.
    skills_detail_markdown: RefCell<Option<(PathBuf, MarkdownView)>>,
    /// Text selection over the detail pane's rendered document. Its own
    /// registry, like the toast's, so it can never join a drag to another
    /// surface's text.
    skills_selection: TranscriptSelection,
    /// Scroll position of the detail pane, tracked so it can draw a
    /// scrollbar and land at the top when the selection moves.
    skills_detail_scroll: ScrollHandle,
    skills_detail_scrollbar: Rc<ScrollbarState>,
    /// Source the list is narrowed to; `None` shows every ecosystem.
    skills_source_filter: Option<crate::skills::SkillSource>,
    /// The skill directory whose delete button is armed for its confirming
    /// second click.
    skills_delete_arming: Option<PathBuf>,
    /// The Providers page's roster selection — a custom provider id, or the
    /// built-in provider key. `None` means the built-in provider.
    providers_selected: Option<String>,
    /// The Providers page is showing the add-provider form instead of a
    /// provider's detail.
    providers_adding: bool,
    /// The selected provider's name is being renamed inline.
    providers_renaming: bool,
    /// The selected provider's API key is shown in plain text.
    providers_api_key_revealed: bool,
    /// The selected custom provider whose delete button is armed for its
    /// confirming second click.
    providers_delete_arming: Option<String>,
    /// The inline model editor over the selected provider's model list.
    providers_model_editor: Option<providers_page::ProvidersModelEditor>,
    /// The inline model editor's input-modality selection, seeded when the
    /// editor opens and written to the model on confirm. Empty records none,
    /// leaving the row's badge to the models.dev table.
    providers_model_editor_modalities: Vec<String>,
    /// Generation token for the debounced commit of keystroke-level field
    /// edits; a newer edit supersedes the pending one.
    providers_commit_generation: u64,
    /// The provider roster, loaded from and committed back to OpenCode's own
    /// configuration file. The app keeps no separate store — this is a
    /// working copy that only lives between a config load and the next
    /// commit.
    providers_store: Vec<fintwind_client::custom_providers::CustomProvider>,
    /// Generation token for the background config load; a newer load
    /// supersedes an older one's result.
    providers_load_generation: usize,
    /// The models.dev provider roster: the built-in providers the add form's
    /// picker offers and the built-in pages' model lists. Loaded on page
    /// open, cached beside the model table.
    providers_builtin: Option<std::sync::Arc<fintwind_client::models_dev::ModelsDevProviders>>,
    /// The built-in provider roster's load is in flight.
    providers_builtin_loading: bool,
    /// The provider ids with a connected credential on the workspace's
    /// OpenCode server — the authorized set. The server's connection list,
    /// not any file, is the source of truth: it counts credentials made
    /// here, in the CLI, and in the TUI alike.
    providers_authorized: std::collections::HashSet<String>,
    /// The integration ids the server offers a plain API-key connect method
    /// for. Everything else in the roster authorizes through OAuth, an env
    /// var, or a command — never through this app's key form.
    providers_key_methods: std::collections::HashSet<String>,
    /// The integration fetch (authorization set included) is in flight.
    providers_auth_loading: bool,
    /// A connectivity probe of the selected built-in provider is in flight.
    providers_builtin_catalog_check: Option<providers_fetch::BuiltinCatalogState>,
    /// The id of the built-in provider the current connectivity probe names;
    /// a stale result for a different provider never renders.
    providers_builtin_catalog_check_id: Option<String>,
    /// Which provider the add form has picked — the picker page, the custom
    /// free-endpoint form, or a chosen built-in provider's key form.
    providers_form_stage: providers_page::ProviderFormStage,
    /// The models.dev metadata table from the latest successful fetch. It
    /// answers the Providers page's fetch-models action while fresh and
    /// serves as the offline fallback; render reads it only for the in-memory
    /// lookups behind the model rows' modality badges.
    models_dev_table: Option<std::sync::Arc<fintwind_client::models_dev::ModelsDevTable>>,
    /// The Providers page's fetch-models download is in flight.
    models_dev_fetching: bool,
    /// The Providers page's page-open catalog load is in flight.
    models_dev_loading: bool,
    /// The Providers page's connectivity probes, keyed by provider id. The
    /// page's one network-status memory; render reads only what a finished
    /// probe stored.
    provider_connectivity: HashMap<String, providers_fetch::ProviderConnectivityState>,
    /// The Providers page's first-token probes, keyed by (provider id,
    /// model id).
    model_latency: HashMap<(String, String), providers_fetch::ModelLatencyState>,
    /// Generation token for the debounced native-session reconcile.
    native_reconcile_generation: u64,
    /// Server transcript version (local fetch timestamp) already applied to
    /// each session that tracks a native one — imported or created here — so
    /// re-opening a session does not refetch unless the server moved.
    native_transcript_fetched: HashMap<Uuid, u64>,
    /// Native transcript fetches in flight, keyed by session id.
    native_transcript_fetches: HashSet<Uuid>,
    /// Sessions with a staged session-level undo. The value is the number of
    /// native user turns the conversation currently keeps, so repeated undos
    /// walk backwards and redo is available while the count sits below what
    /// the conversation had. Cleared when a new prompt deletes the
    /// staged-away turns server-side. Runtime-only.
    staged_undos: HashMap<Uuid, usize>,
    /// Session-level undo/redo daemon RPCs in flight, keyed by session id.
    undo_redo_preparations: HashSet<Uuid>,
    /// Last reconcile/fetch failure, shown next to the sidebar. `None` means
    /// the last attempt succeeded (or none ran yet).
    native_reconcile_error: Option<String>,
    /// The API format picked in the add-provider form.
    providers_form_format: fintwind_client::custom_providers::ProviderApiFormat,
    /// Model draft rows of the add-provider form: id and context-window
    /// fields plus the input modalities picked for each.
    providers_form_models: Vec<providers_page::ProviderFormModelDraft>,
    /// Model metadata the add form's fields cannot hold — the names and
    /// output limits a fetch's merge filled in — keyed by model id, so a
    /// submitted draft keeps what the fetch learned.
    providers_form_model_catalog:
        HashMap<String, fintwind_client::custom_providers::CustomProviderModel>,
    /// The add-provider form's connectivity probe: the form's own network
    /// verdict while the provider it describes does not exist yet.
    providers_form_connectivity: Option<providers_fetch::ProviderConnectivityState>,
    /// The add-provider form's model-list fetch is in flight.
    providers_form_fetching: bool,
    /// Generation tokens for the form's in-flight fetch and probe; leaving
    /// the form supersedes both, so a late result cannot land in a fresh one.
    providers_form_fetch_generation: usize,
    providers_form_probe_generation: usize,
    /// The selected provider's editable endpoint and key fields.
    provider_base_url_input: Entity<ComposerInput>,
    provider_api_key_input: Entity<ComposerInput>,
    provider_rename_input: Entity<ComposerInput>,
    provider_model_id_input: Entity<ComposerInput>,
    provider_model_context_input: Entity<ComposerInput>,
    provider_form_name: Entity<ComposerInput>,
    provider_form_base_url: Entity<ComposerInput>,
    provider_form_api_key: Entity<ComposerInput>,
    /// The add form's search over the built-in provider picker.
    provider_form_builtin_search: Entity<ComposerInput>,
    /// Scroll positions of the Providers page's panes, tracked so they can
    /// draw scrollbars and reset per selection.
    providers_list_scroll: ScrollHandle,
    providers_list_scrollbar: Rc<ScrollbarState>,
    providers_detail_scroll: ScrollHandle,
    providers_detail_scrollbar: Rc<ScrollbarState>,
    /// The MCP page's roster selection — a server name from OpenCode's
    /// `mcp` map. `None` falls back to the first visible row.
    mcp_selected: Option<String>,
    /// The MCP page is showing the add-server form instead of a server's
    /// detail.
    mcp_adding: bool,
    /// The selected server's name is being renamed inline.
    mcp_renaming: bool,
    /// The selected server whose delete button is armed for its confirming
    /// second click.
    mcp_delete_arming: Option<String>,
    /// The inline variable editor over the selected server's environment or
    /// headers table.
    mcp_variable_editor: Option<mcp_page::McpVariableEditor>,
    /// Generation token for the debounced commit of keystroke-level field
    /// edits; a newer edit supersedes the pending one.
    mcp_commit_generation: u64,
    /// The MCP server roster, loaded from and committed back to OpenCode's
    /// own configuration file. A working copy, exactly like the provider
    /// roster.
    mcp_servers: Vec<fintwind_client::opencode_config::McpServer>,
    /// Generation token for the background config load; a newer load
    /// supersedes an older one's result.
    mcp_load_generation: usize,
    /// The MCP servers' live connection statuses from the workspace's
    /// OpenCode server, keyed by server name. `None` means "not yet known"
    /// and renders nothing rather than a stale state.
    mcp_statuses: Option<HashMap<String, mcp_page::McpStatusEntry>>,
    mcp_status_generation: u64,
    /// The MCP list pane's search field.
    mcp_search: Entity<ComposerInput>,
    /// The selected server's editable connection fields, one per kind.
    mcp_command_input: Entity<ComposerInput>,
    mcp_url_input: Entity<ComposerInput>,
    mcp_oauth_client_id: Entity<ComposerInput>,
    mcp_oauth_client_secret: Entity<ComposerInput>,
    mcp_oauth_scope: Entity<ComposerInput>,
    /// Remote MCP OAuth currently waiting on the browser; `None` if idle.
    mcp_oauth_auth_name: Option<String>,
    mcp_oauth_auth_generation: u64,
    /// Set when the user presses the cancel button, so the login callback
    /// shows a neutral "cancelled" note instead of a failure.
    mcp_oauth_cancel_requested: bool,
    mcp_rename_input: Entity<ComposerInput>,
    /// Shared key and value fields of the inline variable editor.
    mcp_variable_key_input: Entity<ComposerInput>,
    mcp_variable_value_input: Entity<ComposerInput>,
    mcp_form_name: Entity<ComposerInput>,
    mcp_form_command: Entity<ComposerInput>,
    mcp_form_url: Entity<ComposerInput>,
    /// The kind picked in the add-server form.
    mcp_form_kind: fintwind_client::opencode_config::McpServerKind,
    /// Scroll positions of the MCP page's panes, tracked so they can draw
    /// scrollbars and reset per selection.
    mcp_list_scroll: ScrollHandle,
    mcp_list_scrollbar: Rc<ScrollbarState>,
    mcp_detail_scroll: ScrollHandle,
    mcp_detail_scrollbar: Rc<ScrollbarState>,
    mcp_market_search: Entity<ComposerInput>,
    mcp_market_category: mcp_market_page::McpMarketCategory,
    mcp_market_selected: Option<String>,
    /// The Usage page's scan snapshot, fetched off the UI thread. `None`
    /// until the first scan lands; frames never touch the entries inside.
    usage_stats: Option<Rc<fintwind_client::provider_session::UsageStats>>,
    /// The last scan's failure, when the page has nothing cached to show.
    usage_stats_error: Option<String>,
    /// Bumped per scan; a result from a superseded scan is discarded.
    usage_stats_generation: u64,
    usage_stats_pending: bool,
    usage_stats_loaded_at: Option<Instant>,
    /// Calendar day with full hourly attribution in the last scan.
    usage_detail_day: Option<chrono::NaiveDate>,
    /// Wall-clock reading of when the last successful scan landed, shown in
    /// the toolbar so a silent refresh is visible as an update rather than a
    /// page that never changes. `None` before the first scan.
    usage_loaded_label: Option<String>,
    /// Guards the page's auto-refresh loop: one at a time, so leaving and
    /// reopening the page quickly cannot stack several.
    usage_refresh_running: bool,
    /// What the Usage page's KPIs, daily bars, and model ranking aggregate
    /// over. The heatmap keeps its own fixed week window.
    usage_range: usage_page::UsageRange,
    /// Aggregated view models for the current range, rebuilt when a scan
    /// lands or the range changes. Frames read only this.
    usage_views: usage_page::UsageViews,
    /// Scroll position of the settings content column, tracked so the pane
    /// can draw a scrollbar and mark the titlebar boundary once content
    /// slides under it.
    settings_scroll: ScrollHandle,
    settings_scrollbar: Rc<ScrollbarState>,
    header_drag_armed: bool,
    toast: Option<ToastState>,
    toast_generation: u64,
    copied_control_feedback: HashMap<String, u64>,
    copied_control_generation: u64,
    copied_message_feedback: HashMap<Uuid, u64>,
    copied_message_generation: u64,
    copied_activity_feedback: HashMap<(Uuid, ActivityDisclosureSectionKind), u64>,
    copied_activity_generation: u64,
    message_edit: Option<MessageEdit>,
    transcript_rows: ListState,
    /// Active turns use top alignment so row remeasurement cannot invoke the
    /// bottom-aligned list's implicit pin and displace the sent-message anchor.
    anchored_transcript_rows: ListState,
    /// Virtualized list backing the sidebar session history, so only visible
    /// rows are built and laid out regardless of how many sessions exist.
    sidebar_list_state: ListState,
    sidebar_scrollbar: Rc<ScrollbarState>,
    /// Snapshot of the sidebar rows the list state currently corresponds to.
    sidebar_row_cache: RefCell<Vec<SidebarRow>>,
    /// Fingerprint + snapshot pair backing `sidebar_rows_cached`.
    sidebar_rows_fingerprint: Cell<Option<u64>>,
    sidebar_rows_snapshot: RefCell<Rc<Vec<SidebarRow>>>,
    /// Scroll state of the session list inside each unfolded sidebar group,
    /// keyed by project id, so a group scrolls on its own and keeps its
    /// position across folding it away and back.
    sidebar_group_scrolls: RefCell<HashMap<Uuid, SidebarGroupScroll>>,
    transcript_row_kinds: RefCell<Vec<TranscriptRowKind>>,
    /// Fingerprint of the transcript inputs `transcript_row_kinds` was folded
    /// from, so an unchanged transcript costs nothing on a frame. `None` until
    /// the first fold. See `transcript_rows_fingerprint`.
    transcript_row_kinds_fingerprint: Cell<Option<u64>>,
    /// The navigation rail's turn list, shared by `Rc` so a frame hands the
    /// rail a pointer instead of re-extracting every turn's snippets. Rebuilt
    /// by `navigation_turns` when the row-kinds fingerprint moves.
    transcript_navigation_turns: RefCell<Rc<Vec<TranscriptNavigationTurn>>>,
    /// The row-kinds fingerprint `transcript_navigation_turns` was built from.
    transcript_navigation_turns_fingerprint: Cell<Option<u64>>,
    /// Response-footer copy content and completion time per message index,
    /// rebuilt when the row-kinds fingerprint moves. The row builder asks for
    /// every visible row on every frame, and the underlying turn walk and
    /// answer join are O(session). Footers exist only for settled turns,
    /// whose parts are immutable, and settling moves the fingerprint.
    assistant_footer_cache: RefCell<HashMap<usize, (Option<SharedString>, Option<u64>)>>,
    /// The row-kinds fingerprint `assistant_footer_cache` was built under.
    assistant_footer_fingerprint: Cell<Option<u64>>,
    /// Settled-turn stats line per message index, rebuilt when the row-kinds
    /// fingerprint — mixed with the provider catalog's size, because the
    /// model segment resolves its display name through it — moves. See
    /// `assistant_turn_stats_cached`.
    assistant_turn_stats_cache: RefCell<HashMap<usize, Option<SharedString>>>,
    /// The fingerprint `assistant_turn_stats_cache` was built under.
    assistant_turn_stats_fingerprint: Cell<Option<u64>>,
    /// Checkpoint-ref existence per (session, retained turn count), filled by
    /// `prefetch_checkpoint_refs` on the background executor. Rows read only
    /// this cache: resolving a ref forks a `git` subprocess, which must stay
    /// off the frame path.
    checkpoint_ref_cache: RefCell<HashMap<(Uuid, usize), bool>>,
    /// Bumped whenever checkpoint refs may have changed. A prefetch launched
    /// under an older generation is stale and discarded on arrival.
    checkpoint_ref_generation: Cell<u64>,
    /// The (session, generation) the latest scheduled prefetch covers.
    checkpoint_ref_prefetch: Cell<Option<(Uuid, u64)>>,
    /// Turn checkpoints asked for but not started yet.
    ///
    /// `capture_turn` is upwards of ten `git` invocations, one of them a
    /// `git add -A` over the whole worktree, and the driver-event drain that
    /// asks for it shares the UI thread with rendering. Requests queue here and
    /// `start_pending_checkpoint_captures` runs them on the background executor.
    pending_checkpoint_captures: Vec<PendingCheckpointCapture>,
    /// The (session, turn) captures currently running, so a repeated request —
    /// a turn that finishes while its own capture is still going — does not
    /// fork a second `git add -A` over the same worktree.
    checkpoint_captures_in_flight: HashSet<(Uuid, usize)>,
    /// Clock for the idle-session sweep, so the check costs one comparison per
    /// frame instead of a scan.
    last_idle_session_sweep: Instant,
    transcript_anchor: Cell<Option<TranscriptAnchor>>,
    transcript_anchor_end_space: Rc<Cell<Pixels>>,
    transcript_anchor_following: Rc<Cell<bool>>,
    /// A wheel scroll has landed and where it came to rest is not classified
    /// yet. The first frame that can measure the tail consumes this and
    /// re-engages following when the reader scrolled back onto it; a frame that
    /// cannot measure the tail leaves it set, so a stream remeasure cannot
    /// swallow the re-engage.
    transcript_tail_recheck: Rc<Cell<bool>>,
    transcript_is_scrolled: Rc<Cell<bool>>,
    /// Last decided visibility of the scroll-to-tail affordance. The tail's
    /// position is unknowable on the frames a stream commit remeasures it, and
    /// those arrive at commit cadence — deciding "show" from that silence
    /// strobes the button against the frames in between.
    transcript_scroll_to_bottom_visible: Cell<bool>,
    /// Whether the transcript's scrollbar thumb was held at the last frame, so
    /// render can notice a drag starting and ending.
    transcript_scrollbar_dragging: Cell<bool>,
    transcript_layout_width: Cell<Pixels>,
    /// Parsed markdown per assistant message, keeping each response's
    /// incremental parse and flattened blocks alive across frames.
    message_markdown: RefCell<HashMap<Uuid, MarkdownView>>,
    /// Parsed markdown for reasoning activities, keyed by stable activity id.
    activity_markdown: RefCell<HashMap<Uuid, MarkdownView>>,
    /// Full scrollback for long thoughts, with only visible fragments laid out.
    reasoning_views: RefCell<HashMap<Uuid, Entity<md::virtualized::ReasoningView>>>,
    /// Independent capped viewports for expanded thoughts and command output.
    /// Keeping these stable preserves scroll position through virtualization.
    activity_scroll_viewports: RefCell<HashMap<Uuid, ActivityScrollViewport>>,
    /// Positioned, syntax-tokenized diff rows for expanded file-change
    /// activities. Built once when the activity is expanded and dropped when it
    /// collapses or its changes are replaced, so a frame only indexes rows.
    activity_diffs: RefCell<HashMap<Uuid, Rc<activity_diff::Diff>>>,
    /// Viewports for those diffs. Separate from `activity_scroll_viewports`
    /// because a failed edit shows both its diff and the error it returned.
    activity_diff_viewports: RefCell<HashMap<Uuid, ActivityScrollViewport>>,
    /// Viewports for individual disclosure sections (arguments, output,
    /// detail) inside an expanded tool card, keyed by activity and section so
    /// each keeps its own scroll position.
    activity_section_viewports:
        RefCell<HashMap<(Uuid, ActivityDisclosureSectionKind), ActivityScrollViewport>>,
    /// Viewport for a whole expanded detail card, capping the combined
    /// sections so no card can push the rest of the transcript around.
    activity_detail_viewports: RefCell<HashMap<Uuid, ActivityScrollViewport>>,
    /// One allocation for every transcript markdown context to share. The
    /// callback knows about the active workspace; the renderer deliberately
    /// does not.
    markdown_link_handler: md::render::LinkHandler,
    /// Transcript-wide text selection, spanning messages and tool output.
    transcript_selection: TranscriptSelection,
    /// Independent selection for the transient toast message. Keeping it out
    /// of the transcript registry prevents an overlay from joining a drag to
    /// whatever happens to be painted beneath it.
    toast_selection: TranscriptSelection,
    transcript_scrollbar: Rc<ScrollbarState>,
    /// Every menu site in the app, keyed by a stable id. Handles are created on
    /// first use and live as long as the window.
    menus: RefCell<HashMap<SharedString, ContextMenuHandle>>,
    navigation_rail: Entity<ConversationNavigationRail>,
    navigation_rail_reset_generation: Cell<u64>,
    /// Cached islands of the root view; see [`FintwindPane`].
    sidebar_pane: Entity<FintwindPane>,
    transcript_pane: Entity<FintwindPane>,
    right_panel_pane: Entity<FintwindPane>,
    /// The unix second the pending time-label wake-up targets, or `None` when
    /// none is armed. See `schedule_time_label_wake`.
    time_label_wake: Cell<Option<u64>>,
    /// Bumped per (re)arm so a superseded wake-up discards itself.
    time_label_wake_generation: Cell<u64>,
    /// Live frames-per-second measurement for the header counter.
    fps_last_frame: Instant,
    fps_frame_count: u64,
    fps_value: u32,
}

mod activity_diff;
mod autocomplete;
mod background_work;
mod branches;
mod command_palette;
mod commit_dialog;
mod components;
mod composer;
mod drafts;
mod file_search;
mod image_preview;
mod mcp_market_page;
mod mcp_page;
mod native_sessions;
mod providers_fetch;
mod providers_page;
mod render;
mod right_panel;
mod runtime;
mod sessions;
mod settings;
mod sidebar;
mod skills_page;
mod streaming;
mod transcript;
mod transcript_view;
mod usage_meter;
mod usage_page;
mod window_chrome;

pub use autocomplete::init as init_composer_autocomplete;
use background_work::{
    BackgroundWorkRegistry, work_kind_icon, work_status_color, work_status_label,
};
pub use command_palette::init as init_command_palette;
pub use commit_dialog::init as init_commit_dialog_keys;
use components::*;
pub use image_preview::init as init_image_preview_keys;
pub use mcp_market_page::init as init_mcp_market_keys;
pub use settings::init as init_settings_keys;
pub use sidebar::init as init_sidebar_keys;
use sidebar::{SidebarGroupScroll, SidebarRow};
pub use skills_page::init as init_skills_keys;
use streaming::*;
use transcript::*;
use transcript_view::ConversationNavigationRail;

/// Seconds until any session's time label next changes value, or `None` when
/// no label is on the clock at all. A running turn's elapsed counter moves
/// every second; a settled reply's "5m"/"3h"/"2d" moves only at its unit
/// boundary, so the wake-up this feeds gets rarer as the history ages.
pub(super) fn next_time_label_change(sessions: &[AgentSession], now: u64) -> Option<u64> {
    let mut next: Option<u64> = None;
    for session in sessions {
        if session.is_busy()
            && session
                .turns
                .last()
                .is_some_and(|turn| turn.status == TurnStatus::Running)
        {
            return Some(1);
        }
        if let Some(last_reply_at) = session.last_reply_at {
            let elapsed = now.saturating_sub(last_reply_at);
            let step = match elapsed {
                0..=3_599 => 60,
                3_600..=86_399 => 3_600,
                _ => 86_400,
            };
            let remaining = (step - elapsed % step).max(1);
            next = Some(next.map_or(remaining, |next| next.min(remaining)));
        }
    }
    next
}

fn migrate_legacy_projectless_projects(
    state: &mut PersistedState,
    workspace: &fintwind_client::WorkspaceClient,
) -> (bool, Option<anyhow::Error>) {
    let legacy_indices = state
        .projects
        .iter()
        .enumerate()
        .filter_map(|(index, project)| {
            crate::projectless::needs_migration(&project.path).then_some(index)
        })
        .collect::<Vec<_>>();
    if legacy_indices.is_empty() {
        return (false, None);
    }

    let mut changed = false;
    for index in legacy_indices {
        let path = state.projects[index].path.clone();
        let response = workspace
            .request(fintwind_client::WorkspaceOperation::MigrateProjectlessWorkspace { path });
        let cwd = match response {
            Ok(fintwind_client::WorkspaceResult::ProjectlessWorkspace { cwd }) => cwd,
            Ok(_) => {
                return (
                    changed,
                    Some(anyhow::anyhow!(
                        "the daemon returned an invalid projectless response"
                    )),
                );
            }
            Err(error) => return (changed, Some(error)),
        };
        state.projects[index].name = Project::PROJECTLESS_NAME.to_owned();
        state.projects[index].path = cwd;
        changed = true;
    }
    (changed, None)
}

impl Fintwind {
    pub(super) fn show_toast(&mut self, message: impl Into<String>) {
        self.show_toast_with_tone(message, ToastTone::Alert);
    }

    pub(super) fn show_success_toast(&mut self, message: impl Into<String>) {
        self.show_toast_with_tone(message, ToastTone::Success);
    }

    /// Opens a daemon-host path with its default application. A remote
    /// session's paths exist only on the daemon host, so every such
    /// affordance explains that instead of pointing this machine's desktop
    /// at a path that was never here.
    pub(super) fn open_host_path(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if self.daemon.is_remote() {
            self.show_toast(tr!("errors.remote_host_path"));
            cx.notify();
        } else {
            crate::platform::open_with_default_app(path, cx);
        }
    }

    /// Like [`Fintwind::open_host_path`], but selects `path` in the file
    /// manager rather than opening it.
    pub(super) fn reveal_host_path(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if self.daemon.is_remote() {
            self.show_toast(tr!("errors.remote_host_path"));
            cx.notify();
        } else {
            crate::platform::reveal_in_file_manager(path, cx);
        }
    }

    fn check_for_update(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let latest = cx
                .background_executor()
                .spawn(async { crate::update::fetch_newer_release() })
                .await;
            let Some(version) = latest else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.latest_available = Some(version);
                cx.notify();
            });
        })
        .detach();
    }

    fn show_toast_with_tone(&mut self, message: impl Into<String>, tone: ToastTone) {
        self.toast_selection.selection.borrow_mut().clear();
        self.toast_selection.registry.borrow_mut().clear();
        self.toast_generation = self.toast_generation.wrapping_add(1);
        self.toast = Some(ToastState {
            message: message.into(),
            tone,
            id: self.toast_generation,
            timer_generation: self.toast_generation,
            duration_remaining: DEFAULT_TOAST_DURATION,
            timer_started: None,
            hovered: false,
        });
    }

    /// Arm one wake-up for the moment a time-derived label next changes —
    /// the sidebar's relative reply times and every "Working for Ns" elapsed.
    ///
    /// There is deliberately no standing timer. Render calls this each frame;
    /// while the scheduled instant is unchanged it is a `Cell` comparison and
    /// nothing spawns. The timer fires exactly when a visible label rolls to
    /// its next value, notifies once, and the frame that draws the new value
    /// arms the next boundary. An idle window with hour-old sessions wakes
    /// once an hour; with nothing to show it wakes never. (T3 Code's
    /// equivalent is one minute-aligned interval gated on subscribers; label
    /// boundaries make even that unnecessary.) A busy session pins the chain
    /// to one-second steps — that is what keeps its elapsed counters moving
    /// under reduce-motion, where the pulse animations that normally drive
    /// frames are suppressed, and while a background turn sits between
    /// stream events.
    fn schedule_time_label_wake(&self, cx: &mut Context<Self>) {
        let now = unix_time();
        let target = next_time_label_change(&self.state.sessions, now).map(|seconds| now + seconds);
        if self.time_label_wake.get() == target {
            return;
        }
        self.time_label_wake.set(target);
        let generation = self.time_label_wake_generation.get().wrapping_add(1);
        self.time_label_wake_generation.set(generation);
        let Some(target) = target else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let delay = target.saturating_sub(unix_time()).max(1);
            cx.background_executor()
                .timer(Duration::from_secs(delay))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.time_label_wake_generation.get() != generation {
                    return;
                }
                // Consumed: the notified frame re-arms the next boundary.
                this.time_label_wake.set(None);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn hide_toast(&mut self) {
        if self.toast.take().is_some() {
            self.toast_selection.selection.borrow_mut().clear();
            self.toast_selection.registry.borrow_mut().clear();
            // Detached timers are deliberately cheap, but their generation
            // must stop them from dismissing a newer toast.
            self.toast_generation = self.toast_generation.wrapping_add(1);
        }
    }

    fn start_toast_dismiss_timer(&mut self, cx: &mut Context<Self>) {
        let Some(toast) = self.toast.as_mut() else {
            return;
        };
        if toast.hovered || toast.timer_started.is_some() {
            return;
        }

        let duration = toast.duration_remaining;
        let generation = toast.timer_generation;
        toast.timer_started = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .toast
                    .as_ref()
                    .is_some_and(|toast| toast.timer_generation == generation && !toast.hovered)
                {
                    this.hide_toast();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_toast_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        let Some(toast) = self.toast.as_ref() else {
            return;
        };
        if toast.hovered == hovered {
            return;
        }

        self.toast_generation = self.toast_generation.wrapping_add(1);
        let generation = self.toast_generation;
        let toast = self.toast.as_mut().expect("toast checked above");
        toast.timer_generation = generation;
        toast.hovered = hovered;
        if hovered {
            if let Some(started) = toast.timer_started.take() {
                toast.duration_remaining =
                    paused_toast_duration(toast.duration_remaining, started.elapsed());
            }
        } else {
            toast.timer_started = None;
            self.start_toast_dismiss_timer(cx);
        }
    }

    pub fn new(
        window: &mut Window,
        cx: &mut App,
        daemon: fintwind_client::DaemonSupervisor,
    ) -> Entity<Self> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let store = StateStore::remote(daemon.clone());
        let composer_draft_store = ComposerDraftStore::remote(daemon.clone());
        let composer_drafts = composer_draft_store.load().unwrap_or_default();
        let mut state = store.load_or_fresh(cwd);
        state.apply_daemon_settings(daemon.settings());
        if let Err(error) = daemon.update_settings(state.daemon_settings()) {
            eprintln!("could not normalize daemon settings after migration: {error:#}");
        }
        crate::i18n::set_language(state.language);
        crate::theme::set_ui_text_scale(state.ui_text_scale);
        crate::theme::set_code_text_scale(state.code_text_scale);
        crate::theme::set_ui_font_family(state.ui_font_family.clone());
        crate::theme::set_code_font_family(state.code_font_family.clone());

        let composer = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .padding_x(px(14.0))
                .submit_empty()
        });
        let user_input_answer = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("user_input.other_placeholder"))
        });
        let command_palette_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("command_palette.placeholder"))
        });
        let model_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("input.search_models"))
        });
        let branch_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("input.search_branches"))
        });
        let branch_create_input = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("input.new_branch_name"))
        });
        let settings_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("settings.search"))
        });
        let skills_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("skills.search"))
        });
        let provider_base_url_input = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("providers.base_url_placeholder"))
        });
        let provider_api_key_input = cx.new(|cx| ComposerInput::new(window, cx).search_field());
        let provider_rename_input = cx.new(|cx| ComposerInput::new(window, cx).search_field());
        let provider_model_id_input = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("providers.model_id_placeholder"))
        });
        let provider_model_context_input = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("providers.context_window_placeholder"))
        });
        let provider_form_name = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("providers.name_placeholder"))
        });
        let provider_form_base_url = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("providers.base_url_placeholder"))
        });
        let provider_form_api_key = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("providers.api_key_placeholder"))
        });
        let provider_form_builtin_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("providers.builtin_search_placeholder"))
        });
        let mcp_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.search_placeholder"))
        });
        let mcp_market_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp_market.search_placeholder"))
        });
        let mcp_command_input = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.command_placeholder"))
        });
        let mcp_url_input = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.url_placeholder"))
        });
        let mcp_oauth_client_id = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.oauth_client_id_placeholder"))
        });
        let mcp_oauth_client_secret = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.oauth_client_secret_placeholder"))
        });
        let mcp_oauth_scope = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.oauth_scope_placeholder"))
        });
        let mcp_rename_input = cx.new(|cx| ComposerInput::new(window, cx).search_field());
        let mcp_variable_key_input = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.variable_key_placeholder"))
        });
        let mcp_variable_value_input = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.variable_value_placeholder"))
        });
        let mcp_form_name = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.name_placeholder"))
        });
        let mcp_form_command = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.command_placeholder"))
        });
        let mcp_form_url = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("mcp.url_placeholder"))
        });
        let session_rename_input = cx.new(|cx| ComposerInput::new(window, cx).search_field());
        let right_panel_diff_filter = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("diff.filter_files"))
        });
        let navigation_rail = cx.new(|_| ConversationNavigationRail::new());
        let sidebar_pane = FintwindPane::new(Fintwind::sidebar_pane_content, cx);
        let transcript_pane = FintwindPane::new(Fintwind::transcript_pane_content, cx);
        let right_panel_pane = FintwindPane::new(Fintwind::right_panel_pane_content, cx);
        let workspace_client = fintwind_client::WorkspaceClient::new(daemon.client());
        let (projectless_migrated, projectless_migration_error) =
            migrate_legacy_projectless_projects(&mut state, &workspace_client);
        let projectless_save_error = projectless_migrated
            .then(|| store.save(&mut state).err())
            .flatten();
        let startup_toast = projectless_migration_error
            .map(|error| tr!("errors.move_projectless_task", error = error))
            .or_else(|| {
                projectless_save_error
                    .map(|error| tr!("errors.save_projectless_migration", error = error))
            });
        let sidebar_visible = state.sidebar_visible;
        let right_panel_visible = state.right_panel_visible;
        let sidebar_width = sanitize_panel_width(
            state.sidebar_width,
            DEFAULT_SIDEBAR_WIDTH,
            SIDEBAR_MIN_WIDTH,
            SIDEBAR_MAX_WIDTH,
        );
        let right_panel_width = sanitize_panel_width(
            state.right_panel_width,
            DEFAULT_RIGHT_PANEL_WIDTH,
            RIGHT_PANEL_MIN_WIDTH,
            RIGHT_PANEL_MAX_WIDTH,
        );
        state.sidebar_width = sidebar_width;
        state.right_panel_width = right_panel_width;
        // First launch has no persisted frame yet; seed from the freshly
        // opened window so an immediate zoom or fullscreen still has a
        // floating frame to restore to. The bounds observer keeps it current
        // from here.
        if state.window_state.is_none() {
            state.window_state = Some(persisted_window_state(
                window.bounds(),
                false,
                window.display(cx).and_then(|display| display.uuid().ok()),
            ));
        }
        crate::theme::apply_theme_preference(state.theme, window, cx);
        crate::platform::set_sidebar_material_width(window, sidebar_width);
        let project_paths = state
            .projects
            .iter()
            .map(|project| (project.id, project.path.clone()))
            .collect::<HashMap<_, _>>();
        let mut startup_live_session_ids = state
            .sessions
            .iter()
            .filter(|session| session.status.is_busy())
            .map(|session| session.id)
            .collect::<Vec<_>>();
        if let Some(selected) = state.selected_session
            && state
                .sessions
                .iter()
                .find(|session| session.id == selected)
                .is_some_and(AgentSession::has_started)
            && !startup_live_session_ids.contains(&selected)
        {
            startup_live_session_ids.push(selected);
        }
        let mut interrupted_turn_checkpoints = Vec::new();
        for session in &mut state.sessions {
            session.migrate_legacy_state();
            // A provider runtime belongs to the daemon and may still be
            // streaming after this desktop process restarted. Leave its
            // persisted projection intact until the background attachment
            // check proves there is no live runtime to resume.
            if session.status.is_busy() {
                continue;
            }
            if session.status != SessionStatus::Idle {
                session.status = SessionStatus::Idle;
            }
            let interrupted_turn = if let Some(turn) = session
                .turns
                .last_mut()
                .filter(|turn| turn.status == TurnStatus::Running)
            {
                turn.status = TurnStatus::Interrupted;
                turn.completed_at = Some(unix_time());
                Some(turn.turn_count)
            } else {
                None
            };
            // A crash mid-turn leaves work in the tree worth checkpointing, but
            // one `capture_turn` per interrupted session is upwards of ten
            // `git` invocations each — paid here, before the window has drawn
            // once. Queue them and let the first frames go out first.
            if let Some(turn_count) = interrupted_turn
                && let Some(project_path) = session
                    .workspace
                    .path()
                    .map(std::path::Path::to_path_buf)
                    .or_else(|| project_paths.get(&session.project_id).cloned())
            {
                interrupted_turn_checkpoints.push(PendingCheckpointCapture {
                    session_id: session.id,
                    turn_count,
                    project_path,
                });
            }
            for message in &mut session.messages {
                message.streaming = false;
            }
            for block in &mut session.transcript_blocks {
                block.activities.retain(|activity| {
                    activity
                        .reasoning
                        .as_ref()
                        .is_none_or(|reasoning| !reasoning.content.trim().is_empty())
                });
                for activity in &mut block.activities {
                    activity.complete = true;
                }
            }
            session
                .transcript_blocks
                .retain(|block| !block.activities.is_empty());
        }
        let initial_composer_draft = state
            .selected_session
            .and_then(|selected| state.sessions.iter().find(|session| session.id == selected))
            .and_then(|session| composer_drafts.get_for(session))
            .cloned()
            .unwrap_or_default();
        let crate::persistence::ComposerDraft {
            text: initial_composer_text,
            attachments: initial_composer_attachments,
        } = initial_composer_draft;
        if !initial_composer_text.is_empty() {
            composer.update(cx, |input, cx| input.set_content(initial_composer_text, cx));
        }
        let composer_attachments = initial_composer_attachments
            .into_iter()
            .map(ComposerAttachment::from)
            .collect();
        let probes = vec![ProviderProbe {
            installed: false,
            path: None,
            models: crate::model_catalog::fallback_models(),
            agent_presets: crate::model_catalog::fallback_agent_presets(),
        }];
        let (provider_probe_tx, provider_probe_events) = unbounded();
        let (provider_version_tx, provider_version_events) = unbounded();
        let (provider_detection_tx, provider_detection_events) = unbounded();
        let (event_wake_tx, event_wake_events) = smol::channel::bounded(1);
        let (task_state_sync_tx, task_state_sync_events) = unbounded();
        let model_picker_tab = ModelPickerTab::Provider("OpenCode".into());
        let mut session_navigation = SessionNavigation::default();
        if let Some(session_id) = state.selected_session.filter(|session_id| {
            state
                .sessions
                .iter()
                .any(|session| session.id == *session_id && !session.has_started())
        }) {
            session_navigation.remember_new_task(session_id);
        }
        // Measure visible rows only, with a generous overdraw — the same shape
        // Zed's own agent chat uses. `measure_all` lays out every row in the
        // session on the first frame and again after any structural splice,
        // which a long transcript cannot afford.
        let transcript_rows = ListState::new(0, ListAlignment::Bottom, px(2048.0));
        let anchored_transcript_rows = ListState::new(0, ListAlignment::Top, px(2048.0));
        let sidebar_list_state = ListState::new(0, ListAlignment::Top, px(256.0));
        let branch_picker_list_state = ListState::new(0, ListAlignment::Top, px(152.0));
        let transcript_is_scrolled = Rc::new(Cell::new(false));
        let transcript_anchor_following = Rc::new(Cell::new(false));
        let transcript_tail_recheck = Rc::new(Cell::new(false));
        // A wheel scroll drops tail following and asks the next measured frame
        // whether it landed back on the tail. GPUI re-engages its own tail pin
        // when a bottom-aligned list reaches the end — it represents that end as
        // no logical offset — but a turn renders through the top-aligned
        // anchored list, whose end is an ordinary offset, so only this can.
        transcript_rows.set_scroll_handler({
            let transcript_is_scrolled = transcript_is_scrolled.clone();
            let transcript_anchor_following = transcript_anchor_following.clone();
            let transcript_tail_recheck = transcript_tail_recheck.clone();
            move |event, window, _| {
                transcript_is_scrolled.set(event.is_scrolled);
                transcript_anchor_following.set(false);
                transcript_tail_recheck.set(true);
                window.refresh();
            }
        });
        anchored_transcript_rows.set_scroll_handler({
            let transcript_is_scrolled = transcript_is_scrolled.clone();
            let transcript_anchor_following = transcript_anchor_following.clone();
            let transcript_tail_recheck = transcript_tail_recheck.clone();
            move |event, window, _| {
                transcript_is_scrolled.set(event.is_scrolled);
                transcript_anchor_following.set(false);
                transcript_tail_recheck.set(true);
                window.refresh();
            }
        });
        // Enable GPUI's experimental overlay plane so deferred draws (menus,
        // tooltips, popovers) composite above native content — without it the
        // browser surface would cover them.
        //
        // Both backends of the pinned fork implement it, and both browser
        // hosts render somewhere it can reach: a sibling NSView below GPUI's
        // overlay layer on macOS, a DirectComposition visual between GPUI's
        // base and overlay planes on Windows. When it is unavailable the
        // surface falls back to freezing the page to a bitmap while an
        // overlay is open.
        let scene_overlay_enabled = window.enable_scene_overlay().is_ok();
        let entity = cx.new(|cx| {
            let settings_focus = cx.focus_handle();
            let onboarding_add_project_focus = cx.focus_handle();
            let onboarding_projectless_focus = cx.focus_handle();
            let sidebar_add_project_focus = cx.focus_handle();
            let context_panel_focus = cx.focus_handle();
            let context_meter_focus = cx.focus_handle();
            let context_compact_focus = cx.focus_handle();
            let context_summary_focus = cx.focus_handle();
            let context_tab_focus = cx.focus_handle();
            let context_tab_close_focus = cx.focus_handle();

            cx.observe_window_appearance(window, |this: &mut Self, window, cx| {
                if this.state.theme == ThemePreference::System {
                    crate::theme::apply_theme_preference(this.state.theme, window, cx);
                    cx.notify();
                }
            })
            .detach();

            cx.observe_window_bounds(window, |this: &mut Self, window, cx| {
                this.capture_window_state(window, cx);
            })
            .detach();

            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                if window.is_window_active() {
                    this.reload_clean_right_panel_file_editors(cx);
                    // The working tree and branch may have moved while another
                    // app had focus — a checkout in a terminal, an edit in an
                    // editor. Coming back is the moment to re-check.
                    this.invalidate_workspace_queries(cx);
                    // Skill files are routinely edited in another app; coming
                    // back to the window is the moment to re-read them.
                    if this.settings_page == Some(SettingsPage::Skills) {
                        this.ensure_skills_catalog(true, cx);
                    }
                    // And the OpenCode server may have gained, renamed, or
                    // dropped sessions while another app (the TUI) had
                    // focus — reconcile the sidebar.
                    this.schedule_native_session_reconcile(cx);
                }
            })
            .detach();

            // A closed surface can take the window's focus down with it —
            // closing a browser tab drops the focused address input — and
            // with nothing focused, action availability walks only the root
            // dispatch node, so every app menu item greys out. When focus
            // dies with its element, send it home to the composer, the way
            // Zed's workspace refocuses itself.
            cx.on_focus_lost(window, |this: &mut Self, window, cx| {
                let focus = this.composer_focus(cx);
                window.focus(&focus, cx);
            })
            .detach();

            // Edits, not raw notifies: a field also notifies for caret blinks
            // and selection changes, and none of the app chrome depends on
            // those — re-rendering the window twice a second for a blinking
            // caret is exactly what the Performance guidance forbids.
            cx.subscribe(
                &composer,
                |this: &mut Self, _, event: &ComposerEvent, cx| match event {
                    ComposerEvent::Submit(prompt) => {
                        if let Some(session_id) = this.selected_session().and_then(|session| {
                            this.response_fork_preparations
                                .contains_key(&session.id)
                                .then_some(session.id)
                        }) {
                            this.defer_restore_composer_after_fork(session_id, prompt.clone(), cx);
                        } else if let Some(submission) =
                            this.submission_with_attachments(prompt, cx)
                        {
                            this.submit_composer_submission(submission, cx);
                        }
                    }
                    ComposerEvent::SubmitSteer(prompt) => {
                        if let Some(session_id) = this.selected_session().and_then(|session| {
                            this.response_fork_preparations
                                .contains_key(&session.id)
                                .then_some(session.id)
                        }) {
                            this.defer_restore_composer_after_fork(session_id, prompt.clone(), cx);
                        } else if let Some(submission) =
                            this.submission_with_attachments(prompt, cx)
                        {
                            this.steer_composer_submission(submission, cx);
                        }
                    }
                    ComposerEvent::Edited => {
                        this.schedule_composer_draft_save(cx);
                        cx.notify();
                    }
                    ComposerEvent::Focus => {}
                    ComposerEvent::BackspaceOnEmpty => {
                        if this.composer_attachments.pop().is_some() {
                            this.schedule_composer_draft_save(cx);
                            cx.notify();
                        }
                    }
                },
            )
            .detach();

            cx.subscribe(
                &user_input_answer,
                |this: &mut Self, input, event: &ComposerEvent, cx| match event {
                    ComposerEvent::Submit(answer) | ComposerEvent::SubmitSteer(answer) => {
                        this.submit_user_input_custom_answer(answer.clone(), cx);
                    }
                    ComposerEvent::Edited => {
                        let answer = input.read(cx).content().to_owned();
                        this.update_user_input_custom_answer(answer, cx);
                    }
                    ComposerEvent::Focus | ComposerEvent::BackspaceOnEmpty => {}
                },
            )
            .detach();

            // Clipboard images and Finder file copies are attachment payloads,
            // not text paths. The input owns representation priority; Fintwind
            // owns durable staging and composer/session state.
            cx.subscribe(
                &composer,
                |this: &mut Self, _, event: &ComposerAttachmentPaste, cx| {
                    this.stage_pasted_attachments(event.0.clone(), cx);
                },
            )
            .detach();

            // Closing the window can leave the app running in the tray. On a
            // true app quit, persist the final state before stopping the
            // desktop-owned daemon and its OpenCode backend processes.
            cx.on_app_quit(|this, cx| {
                this.capture_current_composer_draft(cx);
                this.composer_draft_save_generation =
                    this.composer_draft_save_generation.saturating_add(1);
                let generation = this.composer_draft_save_generation;
                let store = this.composer_draft_store.clone();
                let drafts = this.composer_drafts.clone();
                let daemon = this.daemon.clone();
                this.save();
                let quit = cx.background_executor().spawn(async move {
                    let _ = store.save(drafts, generation);
                    daemon.shutdown();
                });
                async move {
                    let _ = quit.await;
                }
            })
            .detach();

            // A changed query re-filters the picker rows and renumbers them,
            // so the drawn selection cannot carry over. While a filter is
            // active the cursor lands on the first match so `enter` has a
            // visible target; clearing the query returns to the opening
            // state — nothing highlighted, the current model's row in view.
            cx.subscribe(
                &model_search,
                |this: &mut Self, search, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        if search.read(cx).content().trim().is_empty() {
                            this.model_picker_highlight = None;
                            this.reveal_selected_picker_model();
                        } else {
                            this.model_picker_highlight = Some(0);
                            this.model_picker_scroll.scroll_to_item(0);
                        }
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(
                &command_palette_search,
                |this: &mut Self, search, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        let query = search.read(cx).content().to_owned();
                        this.command_palette_query_edited(&query, cx);
                    }
                },
            )
            .detach();
            cx.subscribe(
                &branch_search,
                |this: &mut Self, search, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited)
                        && this.branch_picker_mode == BranchPickerMode::Browse
                    {
                        if search.read(cx).content().trim().is_empty() {
                            this.branch_picker_highlight = None;
                        } else {
                            this.branch_picker_highlight = Some(0);
                            this.branch_picker_list_state.scroll_to_reveal_item(0);
                        }
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(
                &branch_create_input,
                |_: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(
                &settings_search,
                |_: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(
                &skills_search,
                |_: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(
                &session_rename_input,
                |this: &mut Self, _, event: &ComposerEvent, cx| match event {
                    ComposerEvent::Submit(_) => this.commit_session_rename(cx),
                    ComposerEvent::Edited if this.session_rename.is_some() => cx.notify(),
                    _ => {}
                },
            )
            .detach();
            cx.subscribe(
                &provider_base_url_input,
                |this: &mut Self, input, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        this.provider_field_edited(
                            providers_page::ProviderField::BaseUrl,
                            input.read(cx).content().to_owned(),
                            cx,
                        );
                    }
                },
            )
            .detach();
            cx.subscribe(
                &provider_api_key_input,
                |this: &mut Self, input, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        this.provider_field_edited(
                            providers_page::ProviderField::ApiKey,
                            input.read(cx).content().to_owned(),
                            cx,
                        );
                    }
                },
            )
            .detach();
            cx.subscribe(
                &provider_rename_input,
                |this: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Submit(_)) {
                        this.confirm_rename(cx);
                    }
                },
            )
            .detach();
            for model_editor_input in [&provider_model_id_input, &provider_model_context_input] {
                cx.subscribe(model_editor_input, |this, _, event, cx| match event {
                    ComposerEvent::Submit(_) => this.confirm_model_editor(cx),
                    ComposerEvent::Edited if this.providers_model_editor.is_some() => cx.notify(),
                    _ => {}
                })
                .detach();
            }
            for form_input in [
                &provider_form_name,
                &provider_form_base_url,
                &provider_form_api_key,
            ] {
                cx.subscribe(form_input, |this, input, event, cx| match event {
                    // The add form submits only when its fields validate, so
                    // Return in any field is an honest attempt to save.
                    ComposerEvent::Submit(_) => this.submit_provider_form(cx),
                    ComposerEvent::Edited if this.providers_adding => {
                        // An endpoint or key edit voids the form's probe
                        // verdict; it no longer describes these fields.
                        if input == this.provider_form_base_url
                            || input == this.provider_form_api_key
                        {
                            this.providers_form_connectivity = None;
                        }
                        cx.notify();
                    }
                    _ => {}
                })
                .detach();
            }
            cx.subscribe(
                &provider_form_builtin_search,
                |_: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(&mcp_search, |_: &mut Self, _, event: &ComposerEvent, cx| {
                if matches!(event, ComposerEvent::Edited) {
                    cx.notify();
                }
            })
            .detach();
            cx.subscribe(
                &mcp_market_search,
                |_: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        cx.notify();
                    }
                },
            )
            .detach();
            for (input, field) in [
                (&mcp_command_input, mcp_page::McpField::Command),
                (&mcp_url_input, mcp_page::McpField::Url),
                (&mcp_oauth_client_id, mcp_page::McpField::OAuthClientId),
                (
                    &mcp_oauth_client_secret,
                    mcp_page::McpField::OAuthClientSecret,
                ),
                (&mcp_oauth_scope, mcp_page::McpField::OAuthScope),
            ] {
                cx.subscribe(input, move |this, input, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        this.mcp_field_edited(field, input.read(cx).content().to_owned(), cx);
                    }
                })
                .detach();
            }
            cx.subscribe(
                &mcp_rename_input,
                |this: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Submit(_)) {
                        this.confirm_mcp_rename(cx);
                    }
                },
            )
            .detach();
            for variable_input in [&mcp_variable_key_input, &mcp_variable_value_input] {
                cx.subscribe(variable_input, |this, _, event, cx| match event {
                    ComposerEvent::Submit(_) => this.confirm_mcp_variable_editor(cx),
                    ComposerEvent::Edited if this.mcp_variable_editor.is_some() => cx.notify(),
                    _ => {}
                })
                .detach();
            }
            for form_input in [&mcp_form_name, &mcp_form_command, &mcp_form_url] {
                cx.subscribe(form_input, |this, _, event, cx| match event {
                    // The add form submits only when its fields validate, so
                    // Return in any field is an honest attempt to save.
                    ComposerEvent::Submit(_) => this.submit_mcp_form(cx),
                    ComposerEvent::Edited if this.mcp_adding => cx.notify(),
                    _ => {}
                })
                .detach();
            }
            cx.subscribe(
                &right_panel_diff_filter,
                |this: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        this.sync_right_panel_diff_tree_rows(cx);
                        cx.notify();
                    }
                },
            )
            .detach();

            // Like T3 Code's adapter subscriptions feeding its ingestion
            // worker, provider threads push an edge into this bounded wake
            // channel. The UI does no standing scan: the short follow-up tick
            // exists only to remeasure Markdown after a changed text frame.
            cx.spawn(async move |this, cx| {
                while event_wake_events.recv().await.is_ok() {
                    loop {
                        // The typed queues are drained below, so all wake edges
                        // already represented by those payloads can coalesce.
                        while event_wake_events.try_recv().is_ok() {}
                        let schedule = match this.update(cx, |this, cx| this.drain_event_pump(cx)) {
                            Ok(schedule) => schedule,
                            Err(_) => return,
                        };
                        match schedule {
                            EventPumpSchedule::Idle => break,
                            EventPumpSchedule::StreamFrame => {
                                // Deliberately not raced against the wake
                                // channel: waking per chunk made the notify
                                // rate equal the provider's chunk rate, and
                                // every notify is a full re-render. Chunks
                                // queue during the sleep and fold into the
                                // next drain's single batch.
                                cx.background_executor().timer(STREAM_FRAME_INTERVAL).await;
                            }
                            EventPumpSchedule::BackgroundOutput(delay) => {
                                // A log cache has its own 100 ms batching
                                // cadence. A new provider edge interrupts that
                                // wait; it must not wait behind log rendering.
                                futures_lite::future::race(
                                    async {
                                        let _ = event_wake_events.recv().await;
                                    },
                                    async {
                                        cx.background_executor().timer(delay).await;
                                    },
                                )
                                .await;
                            }
                        }
                    }
                }
            })
            .detach();

            // Maintenance clocks are intentionally independent of provider
            // ingestion and run at the slowest cadence their UI requires.
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(BACKGROUND_WORK_TICK_INTERVAL)
                        .await;
                    if this
                        .update(cx, |this, cx| this.maybe_refresh_background_work(cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();

            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(IDLE_SESSION_SWEEP_INTERVAL)
                        .await;
                    if this
                        .update(cx, |this, _| this.reap_idle_sessions())
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();

            let markdown_link_handler: md::render::LinkHandler = {
                let fintwind = cx.entity().downgrade();
                Rc::new(move |target, _, cx| {
                    let handled = fintwind
                        .update(cx, |fintwind, cx| fintwind.open_transcript_link(target, cx))
                        .unwrap_or(false);
                    if !handled {
                        cx.open_url(target);
                    }
                })
            };

            // Groups start folded; only the restored selection's project is
            // revealed so the user can see where they left off.
            let sidebar_expanded_groups = state
                .selected_session
                .and_then(|session_id| {
                    state
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .map(|session| session.project_id)
                })
                .into_iter()
                .collect::<HashSet<Uuid>>();

            Self {
                daemon,
                session_hydrations: HashSet::new(),
                pending_session_activation: None,
                state,
                store,
                composer,
                user_input_answer,
                composer_drafts,
                composer_draft_store,
                composer_draft_save_generation: 0,
                command_palette: command_palette::CommandPaletteUi::new(command_palette_search),
                model_search,
                branch_search,
                branch_create_input,
                settings_search,
                settings_focus,
                latest_available: None,
                onboarding_add_project_focus,
                onboarding_projectless_focus,
                sidebar_add_project_focus,
                probes,
                provider_probe_tx,
                provider_probe_events,
                provider_model_discoveries: HashSet::new(),
                provider_model_discoveries_pending: HashSet::new(),
                provider_versions: HashMap::new(),
                provider_version_tx,
                provider_version_events,
                provider_version_probes_pending: HashSet::new(),
                provider_detection_tx,
                provider_detection_events,
                provider_detection_remaining: 0,
                provider_detection_checked_at: None,
                model_picker_tab,
                model_picker_highlight: None,
                model_picker_scroll: ScrollHandle::new(),
                model_picker_scrollbar: ScrollbarState::new(),
                branch_picker_mode: BranchPickerMode::Browse,
                branch_picker_highlight: None,
                branch_picker_list_state,
                branch_picker_row_cache: RefCell::new(Vec::new()),
                branch_snapshots: QueryCache::new(MAX_CACHED_WORKSPACES),
                sidebar_branches: QueryCache::new(4 * MAX_CACHED_WORKSPACES),
                visible_branch_snapshot: None,
                branch_operation_pending: false,
                commit_dialog: None,
                commit_operation: None,
                // Providers × workspaces; both scans are small, the cache
                // only exists to keep them off the frame path.
                slash_commands: QueryCache::new(2 * MAX_CACHED_WORKSPACES),
                slash_command_index: Rc::new(Vec::new()),
                slash_command_index_key: None,
                mention_files: QueryCache::new(MAX_CACHED_WORKSPACES),
                mention_file_index: Rc::new(Vec::new()),
                mention_file_index_path: None,
                composer_sources_stale: false,
                composer_autocomplete: autocomplete::AutocompleteUi::new(),
                composer_attachments,
                image_preview: None,
                image_preview_generation: 0,
                remote_images: RefCell::new(HashMap::new()),
                event_wake_tx,
                task_state_sync_tx,
                task_state_sync_events,
                runtimes: HashMap::new(),
                runtime_attach_pending: HashSet::new(),
                runtime_attach_misses: HashMap::new(),
                background_work: HashMap::new(),
                todo_summaries: RefCell::new(HashMap::new()),
                last_background_work_tick: Instant::now(),
                submission_preparations: HashSet::new(),
                escape_stop_confirmation: EscapeStopConfirmation::default(),
                response_fork_preparations: HashMap::new(),
                pending_queue_drains: Vec::new(),
                stream_state_dirty: false,
                last_stream_save: Instant::now(),
                activities_expanded: HashMap::new(),
                expanded_activity_items: HashMap::new(),
                expanded_turns: HashSet::new(),
                expanded_changed_files: HashSet::new(),
                expanded_compactions: HashSet::new(),
                expanded_provider_retries: HashSet::new(),
                provider_retries: HashMap::new(),
                transcript_control_focuses: RefCell::new(HashMap::new()),
                session_navigation,
                session_rename: None,
                session_rename_input,
                sidebar_expanded_groups,
                sidebar_visible,
                sidebar_width,
                right_panel_visible,
                right_panel_width,
                sidebar_slide: None,
                right_panel_slide: None,
                sidebar_rendered_width: if sidebar_visible { sidebar_width } else { 0.0 },
                right_panel_rendered_width: if right_panel_visible {
                    right_panel_width
                } else {
                    0.0
                },
                fps_counter_visible: false,
                panel_resize_drag: None,
                right_panel_session_states: HashMap::new(),
                context_summary_ids: HashMap::new(),
                right_panel_surfaces: Vec::new(),
                right_panel_active_surface: None,
                right_panel_tabs_scroll_handle: ScrollHandle::new(),
                context_panel_focus,
                context_meter_focus,
                context_compact_focus,
                context_summary_focus,
                context_tab_focus,
                context_tab_close_focus,
                right_panel_files_scroll_handle: ScrollHandle::new(),
                right_panel_files_scrollbar: ScrollbarState::new(),
                right_panel_diff_filter,
                right_panel_diff_list_state: ListState::new(0, ListAlignment::Top, px(512.0)),
                right_panel_diff_scrollbar: ScrollbarState::new(),
                right_panel_diff_selection: TranscriptSelection::default(),
                right_panel_diff_tree_list_state: ListState::new(0, ListAlignment::Top, px(180.0))
                    .with_uniform_item_height(px(30.0)),
                right_panel_diff_tree_scrollbar: ScrollbarState::new(),
                right_panel_editor_scroll_handle: ScrollHandle::new(),
                right_panel_editor_scrollbar: ScrollbarState::new(),
                right_panel_pending_tab_reveal: None,
                right_panel_pending_terminal_focus: None,
                right_panel_expanded_paths: HashSet::new(),
                right_panel_files_selected_path: None,
                right_panel_file_tree_width: DEFAULT_FILE_TREE_WIDTH,
                right_panel_file_editors: HashMap::new(),
                file_search: None,
                right_panel_diff_source: ReviewDiffSource::default(),
                right_panel_diff_snapshot: None,
                right_panel_diff_loading: false,
                right_panel_diff_error: None,
                right_panel_diff_generation: 0,
                right_panel_diff_selected_file: None,
                right_panel_diff_expanded_paths: HashSet::new(),
                right_panel_diff_tree_rows: RefCell::new(Vec::new()),
                right_panel_diff_tree_cursor: None,
                right_panel_working_tree: Vec::new(),
                working_trees: QueryCache::new(MAX_CACHED_WORKSPACES),
                workspace_queries_stale: false,
                right_panel_terminals: HashMap::new(),
                right_panel_browsers: HashMap::new(),
                right_panel_pending_browser_focus: None,
                scene_overlay_enabled,
                settings_page: None,
                skills_catalog: None,
                skills_scan_generation: 0,
                skills_scan_pending: false,
                skills_scanned_at: None,
                skills_search,
                skills_list_state: ListState::new(0, ListAlignment::Top, px(512.0)),
                skills_scrollbar: ScrollbarState::new(),
                skills_rows: RefCell::new(Vec::new()),
                skills_selected: None,
                skills_detail_markdown: RefCell::new(None),
                skills_selection: TranscriptSelection::default(),
                skills_detail_scroll: ScrollHandle::new(),
                skills_detail_scrollbar: ScrollbarState::new(),
                skills_source_filter: None,
                skills_delete_arming: None,
                providers_selected: None,
                providers_adding: false,
                providers_renaming: false,
                providers_api_key_revealed: false,
                providers_delete_arming: None,
                providers_model_editor: None,
                providers_model_editor_modalities: Vec::new(),
                providers_commit_generation: 0,
                providers_store: Vec::new(),
                providers_load_generation: 0,
                providers_builtin: None,
                providers_builtin_loading: false,
                providers_authorized: HashSet::new(),
                providers_key_methods: HashSet::new(),
                providers_auth_loading: false,
                providers_builtin_catalog_check: None,
                providers_builtin_catalog_check_id: None,
                providers_form_stage: Default::default(),
                models_dev_table: None,
                models_dev_fetching: false,
                models_dev_loading: false,
                provider_connectivity: HashMap::new(),
                model_latency: HashMap::new(),
                native_reconcile_generation: 0,
                native_transcript_fetched: HashMap::new(),
                native_transcript_fetches: HashSet::new(),
                staged_undos: HashMap::new(),
                undo_redo_preparations: HashSet::new(),
                native_reconcile_error: None,
                providers_form_format: Default::default(),
                providers_form_models: Vec::new(),
                providers_form_model_catalog: HashMap::new(),
                providers_form_connectivity: None,
                providers_form_fetching: false,
                providers_form_fetch_generation: 0,
                providers_form_probe_generation: 0,
                provider_base_url_input,
                provider_api_key_input,
                provider_rename_input,
                provider_model_id_input,
                provider_model_context_input,
                provider_form_name,
                provider_form_base_url,
                provider_form_api_key,
                provider_form_builtin_search,
                providers_list_scroll: ScrollHandle::new(),
                providers_list_scrollbar: ScrollbarState::new(),
                providers_detail_scroll: ScrollHandle::new(),
                providers_detail_scrollbar: ScrollbarState::new(),
                mcp_selected: None,
                mcp_adding: false,
                mcp_renaming: false,
                mcp_delete_arming: None,
                mcp_variable_editor: None,
                mcp_commit_generation: 0,
                mcp_servers: Vec::new(),
                mcp_load_generation: 0,
                mcp_statuses: None,
                mcp_status_generation: 0,
                mcp_search,
                mcp_command_input,
                mcp_url_input,
                mcp_oauth_client_id,
                mcp_oauth_client_secret,
                mcp_oauth_scope,
                mcp_oauth_auth_name: None,
                mcp_oauth_auth_generation: 0,
                mcp_oauth_cancel_requested: false,
                mcp_rename_input,
                mcp_variable_key_input,
                mcp_variable_value_input,
                mcp_form_name,
                mcp_form_command,
                mcp_form_url,
                mcp_form_kind: Default::default(),
                mcp_list_scroll: ScrollHandle::new(),
                mcp_list_scrollbar: ScrollbarState::new(),
                mcp_detail_scroll: ScrollHandle::new(),
                mcp_detail_scrollbar: ScrollbarState::new(),
                mcp_market_search,
                mcp_market_category: Default::default(),
                mcp_market_selected: None,
                usage_stats: None,
                usage_stats_error: None,
                usage_stats_generation: 0,
                usage_stats_pending: false,
                usage_stats_loaded_at: None,
                usage_detail_day: None,
                usage_loaded_label: None,
                usage_refresh_running: false,
                usage_range: Default::default(),
                usage_views: Default::default(),
                settings_scroll: ScrollHandle::new(),
                settings_scrollbar: ScrollbarState::new(),
                header_drag_armed: false,
                toast: startup_toast.map(|message| ToastState {
                    message,
                    tone: ToastTone::Alert,
                    id: 0,
                    timer_generation: 0,
                    duration_remaining: DEFAULT_TOAST_DURATION,
                    timer_started: None,
                    hovered: false,
                }),
                toast_generation: 0,
                copied_control_feedback: HashMap::new(),
                copied_control_generation: 0,
                copied_message_feedback: HashMap::new(),
                copied_message_generation: 0,
                copied_activity_feedback: HashMap::new(),
                copied_activity_generation: 0,
                message_edit: None,
                transcript_rows,
                anchored_transcript_rows,
                sidebar_list_state,
                sidebar_scrollbar: ScrollbarState::new(),
                sidebar_row_cache: RefCell::new(Vec::new()),
                sidebar_rows_fingerprint: Cell::new(None),
                sidebar_rows_snapshot: RefCell::new(Rc::new(Vec::new())),
                sidebar_group_scrolls: RefCell::new(HashMap::new()),
                transcript_row_kinds: RefCell::new(Vec::new()),
                transcript_row_kinds_fingerprint: Cell::new(None),
                transcript_navigation_turns: RefCell::new(Rc::new(Vec::new())),
                transcript_navigation_turns_fingerprint: Cell::new(None),
                assistant_footer_cache: RefCell::new(HashMap::new()),
                assistant_footer_fingerprint: Cell::new(None),
                assistant_turn_stats_cache: RefCell::new(HashMap::new()),
                assistant_turn_stats_fingerprint: Cell::new(None),
                checkpoint_ref_cache: RefCell::new(HashMap::new()),
                checkpoint_ref_generation: Cell::new(0),
                checkpoint_ref_prefetch: Cell::new(None),
                pending_checkpoint_captures: interrupted_turn_checkpoints,
                checkpoint_captures_in_flight: HashSet::new(),
                last_idle_session_sweep: Instant::now(),
                transcript_anchor: Cell::new(None),
                transcript_anchor_end_space: Rc::new(Cell::new(Pixels::ZERO)),
                transcript_anchor_following,
                transcript_tail_recheck,
                transcript_is_scrolled,
                transcript_scroll_to_bottom_visible: Cell::new(false),
                transcript_scrollbar_dragging: Cell::new(false),
                transcript_layout_width: Cell::new(Pixels::ZERO),
                message_markdown: RefCell::new(HashMap::new()),
                activity_markdown: RefCell::new(HashMap::new()),
                reasoning_views: RefCell::new(HashMap::new()),
                activity_scroll_viewports: RefCell::new(HashMap::new()),
                activity_diffs: RefCell::new(HashMap::new()),
                activity_diff_viewports: RefCell::new(HashMap::new()),
                activity_section_viewports: RefCell::new(HashMap::new()),
                activity_detail_viewports: RefCell::new(HashMap::new()),
                markdown_link_handler,
                transcript_selection: TranscriptSelection::default(),
                toast_selection: TranscriptSelection::default(),
                transcript_scrollbar: ScrollbarState::new(),
                menus: RefCell::new(HashMap::new()),
                navigation_rail: navigation_rail.clone(),
                navigation_rail_reset_generation: Cell::new(0),
                sidebar_pane: sidebar_pane.clone(),
                transcript_pane: transcript_pane.clone(),
                right_panel_pane: right_panel_pane.clone(),
                time_label_wake: Cell::new(None),
                time_label_wake_generation: Cell::new(0),
                fps_last_frame: Instant::now(),
                fps_frame_count: 0,
                fps_value: 0,
            }
        });
        navigation_rail.update(cx, |rail, _| rail.set_fintwind(entity.downgrade()));
        for pane in [&sidebar_pane, &transcript_pane, &right_panel_pane] {
            pane.update(cx, |pane, cx| pane.bind(&entity, cx));
        }
        let initial_row_count = entity.read(cx).transcript_row_count();
        entity.read(cx).reset_transcript_rows(initial_row_count);
        // Everything launch needs from `git` or the filesystem, started now
        // that there is an entity to notify and deliberately not before the
        // first frame.
        entity.update(cx, |this, cx| {
            if let Some(session_id) = this.state.selected_session {
                this.restore_background_work(session_id);
                if this
                    .selected_session()
                    .is_some_and(|session| session.detail_loaded)
                {
                    this.refresh_context_summary_id(session_id);
                }
            }
            this.restart_task_state_sync();
            for session_id in startup_live_session_ids {
                this.start_runtime_attachment(session_id, cx);
            }
            this.start_pending_checkpoint_captures(cx);
            // The autocomplete indexes prefetch alongside, so typing `/` or
            // `@` into the very first prompt already has data to draw.
            this.refresh_composer_sources(cx);
            // Re-detect the provider CLI after resolving the user's login-shell
            // environment off-thread. Detection then starts model and version
            // discovery for what it finds, including nvm/fnm-managed
            // installs.
            this.refresh_provider_detection();
            // The skill library too: the Skills settings page must open onto
            // data, not a scan.
            this.ensure_skills_catalog(false, cx);
            // And the provider roster from OpenCode's own configuration, so
            // the Providers page opens onto the CLI's providers — after the
            // one-time migration of the old app-managed mirror file.
            this.load_providers_from_config(cx);
            // The MCP roster too, same file, same contract.
            this.load_mcp_servers_from_config(cx);
            // The session roster too: the server is the single store, so the
            // sidebar must reconcile with what the CLI and TUI left there.
            // It retries once the provider probe finds the binary.
            this.schedule_native_session_reconcile(cx);
            this.check_for_update(cx);
        });
        entity
    }
}

#[cfg(test)]
mod tests;

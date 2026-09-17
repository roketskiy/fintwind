use crate::theme::ui_px;

use gpui::{KeyBinding, actions};

use super::*;

actions!(fintwind_sidebar, [CancelSessionRename]);

const SESSION_RENAME_PARENT_CONTEXT: &str = "SessionRename";
const SESSION_RENAME_FIELD_CONTEXT: &str = "SessionRename > ComposerInput";

/// Keep Escape inside the focused inline editor so it cancels the rename,
/// rather than falling through to the window-wide Stop action.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "escape",
        CancelSessionRename,
        Some(SESSION_RENAME_FIELD_CONTEXT),
    )]);
}

/// One project group in the sidebar: its header, plus the sessions listed
/// beneath it while it is unfolded. Whether it is unfolded belongs to the row
/// rather than to a lookup at paint time: a group's height depends on it, so
/// the row value has to move with it for the virtualized list to notice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SidebarGroup {
    pub(super) project_id: Uuid,
    /// Shared so a frame clones a pointer instead of every session id.
    pub(super) sessions: Rc<Vec<Uuid>>,
    pub(super) unfolded: bool,
}

/// The scroll state behind one unfolded group's body. Held per project so a
/// group keeps its scroll position when it is folded and unfolded again.
#[derive(Clone)]
pub(super) struct SidebarGroupScroll {
    pub(super) list: ListState,
    pub(super) scrollbar: Rc<ScrollbarState>,
}

/// Height of a session card plus the separation reserved beneath it in the
/// virtualized sidebar list. Keep the gap inside the list row so measured and
/// estimated heights stay identical for off-screen sessions.
const SIDEBAR_SESSION_CARD_HEIGHT: f32 = 52.0;
const SESSION_AVATAR_SIZE: f32 = 36.0;
const SESSION_AVATAR_INNER: f32 = 26.0;
const SESSION_AVATAR_ICON: f32 = 14.0;
const SIDEBAR_SESSION_ROW_GAP: f32 = 2.0;
const SIDEBAR_SESSION_ROW_HEIGHT: f32 = SIDEBAR_SESSION_CARD_HEIGHT + SIDEBAR_SESSION_ROW_GAP;
const SIDEBAR_ACTION_ROW_HEIGHT: f32 = 32.0;
const SIDEBAR_SEARCH_BOTTOM_GAP: f32 = 10.0;
/// Project group header: a bordered card that is slightly taller than the
/// old text-only row so the trailing new-session control has a usable hit area.
const SIDEBAR_PROJECT_CARD_HEIGHT: f32 = 34.0;
const SIDEBAR_PROJECT_CARD_BOTTOM_GAP: f32 = 6.0;
/// An unfolded group's body stops growing here — six session cards — and the
/// rest scrolls inside the group, so one busy project cannot push every other
/// project off screen.
const SIDEBAR_GROUP_BODY_MAX_HEIGHT: f32 = SIDEBAR_SESSION_ROW_HEIGHT * 6.0;
/// How far a group list renders above and below its viewport.
const SIDEBAR_GROUP_OVERDRAW: f32 = SIDEBAR_SESSION_ROW_HEIGHT * 2.0;

/// Height of an unfolded group's body: its session rows, capped at
/// [`SIDEBAR_GROUP_BODY_MAX_HEIGHT`] so the remainder scrolls in place. Rows
/// are a fixed height, so the cap falls on a row boundary and no card is ever
/// cut in half.
fn sidebar_group_body_height(sessions: usize) -> f32 {
    (sessions as f32 * SIDEBAR_SESSION_ROW_HEIGHT).min(SIDEBAR_GROUP_BODY_MAX_HEIGHT)
}

/// The session row's trailing time: how long the live turn has been working,
/// or how long ago the agent last replied. A session that has never replied
/// shows nothing.
pub(super) fn session_time_label(session: &AgentSession, now: u64) -> Option<String> {
    if session.is_busy()
        && let Some(turn) = session
            .turns
            .last()
            .filter(|turn| turn.status == TurnStatus::Running)
    {
        return Some(tr!(
            "sidebar.working",
            elapsed = format_working_elapsed(now.saturating_sub(turn.started_at))
        ));
    }
    session
        .last_reply_at
        .map(|last_reply_at| format_time_ago(now.saturating_sub(last_reply_at)))
}

/// Recency for sidebar ordering and date groups. A submitted turn promotes the
/// task immediately, while metadata edits such as a rename do not; a task with
/// no turns stays anchored to when it was created.
fn sidebar_session_timestamp(session: &AgentSession) -> u64 {
    session.last_reply_at.unwrap_or(session.created_at)
}

/// Compact "how long ago" for the sidebar: "just now", then one coarse unit —
/// "5m", "3h", "420d". Days are the largest unit so a glance still reads as a
/// count rather than a date.
pub(super) fn format_time_ago(seconds: u64) -> String {
    match seconds {
        0..=59 => tr!("sidebar.just_now"),
        60..=3_599 => tr!("sidebar.minutes_ago", count = seconds / 60),
        3_600..=86_399 => tr!("sidebar.hours_ago", count = seconds / 3_600),
        _ => tr!("sidebar.days_ago", count = seconds / 86_400),
    }
}

/// Started sessions grouped by project. Groups are ordered by each project's
/// most recent activity — a project with no started sessions falls back to its
/// own `created_at` — and sessions are newest first within a group, so the
/// global recency order is preserved inside the grouping. Every non-projectless
/// project keeps a group even before its first task starts, so a freshly added
/// project is visible in the sidebar right away; sessions whose project record
/// is gone still group under their project id and fall back to a generic label
/// at render time.
fn sidebar_project_groups(
    sessions: &[AgentSession],
    projects: &[Project],
) -> Vec<(Uuid, Vec<Uuid>)> {
    let mut sorted_sessions = sessions
        .iter()
        .filter(|session| session.has_started())
        .collect::<Vec<_>>();
    sorted_sessions.sort_by_key(|session| std::cmp::Reverse(sidebar_session_timestamp(session)));

    let mut groups: Vec<(Uuid, u64, Vec<Uuid>)> = Vec::new();
    for session in sorted_sessions {
        match groups
            .iter_mut()
            .find(|(project_id, _, _)| *project_id == session.project_id)
        {
            // Sessions arrive newest first, so the group's recency is already
            // its most recent session's.
            Some((_, _, group)) => group.push(session.id),
            None => groups.push((
                session.project_id,
                sidebar_session_timestamp(session),
                vec![session.id],
            )),
        }
    }
    for project in projects.iter().filter(|project| !project.is_projectless()) {
        if !groups
            .iter()
            .any(|(project_id, _, _)| *project_id == project.id)
        {
            groups.push((project.id, project.created_at, Vec::new()));
        }
    }
    groups.sort_by_key(|&(_, recency, _)| std::cmp::Reverse(recency));
    groups
        .into_iter()
        .map(|(project_id, _, sessions)| (project_id, sessions))
        .collect()
}

/// One row of the virtualized sidebar session history. A project contributes a
/// single row — its header and, while unfolded, its own capped session list —
/// so a group's height and its internal scroll belong to one list item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SidebarRow {
    /// Opens the window-wide command palette and scrolls with history.
    Search,
    /// A project group carrying its id and its started sessions.
    Group(Rc<SidebarGroup>),
    /// Spacing between project groups.
    GroupSpacer,
}

impl Fintwind {
    pub(super) fn window_drag_region(
        &self,
        region: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        // Windows drags from the hit test, not from a mouse-move handler:
        // `DefWindowProc` moves the window once the region reports itself as
        // caption, and performs the user's configured double-click action.
        #[cfg(target_os = "windows")]
        let region = region.window_control_area(gpui::WindowControlArea::Drag);

        region
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    crate::platform::titlebar_double_click(window);
                }
            })
            .on_mouse_down_out(cx.listener(|this, _, _, _| {
                this.header_drag_armed = false;
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.header_drag_armed = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.header_drag_armed = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.header_drag_armed {
                    this.header_drag_armed = false;
                    crate::platform::start_window_move(window);
                }
            }))
    }
    // ── Sidebar ────────────────────────────────────────────────────────────

    fn render_fps_counter(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let fps = self.fps_value;
        let dot = if fps == 0 {
            theme.text_ghost
        } else if fps >= 55 {
            theme.success
        } else if fps >= 30 {
            theme.warning
        } else {
            theme.danger
        };
        div()
            .flex_none()
            .h(px(26.0))
            .px(px(6.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .text_size(ui_px(11.0))
            .line_height(ui_px(0.0))
            .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(dot))
            .child(
                div()
                    .text_color(theme.text_tertiary)
                    .font_family(crate::md::render::MONO_FAMILY)
                    .child(SharedString::from(format!("{fps} FPS"))),
            )
    }

    fn render_sidebar_toggle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id("toggle-sidebar")
            .w(px(28.0))
            .h(px(28.0))
            .flex_none()
            .rounded(px(7.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon("icons/panel-left.svg", 15.0, theme.text_tertiary))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.set_sidebar_visible(!this.sidebar_visible, cx);
            }))
    }

    pub(super) fn render_history_button(
        &self,
        id: &'static str,
        icon_path: &'static str,
        enabled: bool,
        navigate_back: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id(id)
            .w(px(28.0))
            .h(px(28.0))
            .flex_none()
            .rounded(px(7.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .when(!enabled, |element| element.opacity(0.35))
            .when(enabled, |element| {
                element
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if navigate_back {
                            this.navigate_back_action(&NavigateBack, window, cx);
                        } else {
                            this.navigate_forward_action(&NavigateForward, window, cx);
                        }
                    }))
            })
            .child(icon(icon_path, 15.0, theme.text_tertiary))
    }

    /// The right-side header button that shows the selected session's project
    /// folder in the desktop file manager. Absent while no session is
    /// selected — there is no folder to offer before the first task exists.
    fn render_reveal_project_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.selected_workspace_path()?;
        let theme = Theme::current(cx);
        let focus = cx.focus_handle();
        Some(
            div()
                .id("reveal-project-folder")
                .track_focus(&focus)
                .tab_index(0)
                .tab_stop(true)
                .w(px(28.0))
                .h(px(28.0))
                .flex_none()
                .rounded(px(7.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .hover(|element| element.bg(theme.overlay))
                .active(|element| element.bg(theme.overlay_strong))
                .child(icon("icons/folder.svg", 15.0, theme.text_tertiary))
                .tooltip(|window, cx| {
                    Tooltip::new(tr!("session.reveal_project_folder")).build(window, cx)
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.reveal_selected_project_folder(cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        this.reveal_selected_project_folder(cx);
                        cx.stop_propagation();
                    }
                }))
                .into_any_element(),
        )
    }

    /// Opens the selected session's workspace folder on the desktop.
    fn reveal_selected_project_folder(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self
            .selected_workspace_path()
            .map(|path| path.to_path_buf())
        {
            self.open_host_path(&path, cx);
        }
    }

    fn render_sidebar_titlebar(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("sidebar-titlebar")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .children(self.render_client_window_controls(
                super::window_chrome::WindowControlSide::Left,
                window,
                cx,
            ))
            .child(
                self.window_drag_region(
                    div()
                        .id("sidebar-traffic-light-drag-region")
                        .w(px(TRAFFIC_LIGHT_CLEARANCE))
                        .h_full()
                        .flex_none(),
                    cx,
                ),
            )
            .child(self.render_sidebar_toggle(cx))
            .child(
                div()
                    .ml(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(self.render_history_button(
                        "navigate-back",
                        "icons/arrow-left.svg",
                        !self.session_navigation.back.is_empty(),
                        true,
                        cx,
                    ))
                    .child(self.render_history_button(
                        "navigate-forward",
                        "icons/arrow-right.svg",
                        !self.session_navigation.forward.is_empty(),
                        false,
                        cx,
                    )),
            )
            .child(self.window_drag_region(
                div().id("sidebar-titlebar-drag-region").h_full().flex_1(),
                cx,
            ))
            .child(self.render_sidebar_project_action(cx))
    }

    /// The sidebar titlebar's "add project" button. It sits at the trailing
    /// edge of the top row so opening a project stays reachable whether or not
    /// any history exists.
    fn render_sidebar_project_action(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        div().mr(px(10.0)).flex().items_center().child(
            div()
                .id("add-project")
                .track_focus(&self.sidebar_add_project_focus)
                .tab_index(0)
                .w(px(28.0))
                .h(px(28.0))
                .flex_none()
                .rounded(px(7.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .tooltip(Tooltip::text(tr_cow!("project.add_project")))
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .hover(|element| element.bg(theme.overlay))
                .active(|element| element.bg(theme.overlay_strong))
                .child(icon("icons/folder-new.svg", 15.0, theme.text_tertiary))
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .on_click(cx.listener(|this, _, _, cx| this.add_project(cx)))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        this.add_project(cx);
                        cx.stop_propagation();
                    }
                })),
        )
    }

    fn render_sidebar_action_row(
        &self,
        id: &'static str,
        icon_path: &'static str,
        label: String,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id(id)
            .tab_index(0)
            .w_full()
            .h(px(SIDEBAR_ACTION_ROW_HEIGHT))
            .flex_none()
            .px(px(4.0))
            .rounded(px(7.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|element| element.bg(theme.sidebar_item_background))
            .active(|element| element.bg(theme.overlay_strong))
            .child(
                div()
                    .size(px(20.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(icon_path, 16.0, theme.text_secondary)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(ui_px(13.0))
                    .text_color(theme.text_secondary)
                    .child(label),
            )
    }

    fn render_sidebar_new_session(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        self.render_sidebar_action_row(
            "sidebar-new-session",
            "icons/compose.svg",
            tr!("menu.new_task"),
            cx,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.new_session_action(&NewSession, window, cx);
        }))
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                this.new_session_action(&NewSession, window, cx);
                cx.stop_propagation();
            }
        }))
    }

    fn render_sidebar_search(&self, cx: &mut Context<Self>) -> Div {
        let search = self
            .render_sidebar_action_row(
                "sidebar-search",
                "icons/search.svg",
                tr!("sidebar.search"),
                cx,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_command_palette_action(&ToggleCommandPalette, window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.toggle_command_palette_action(&ToggleCommandPalette, window, cx);
                    cx.stop_propagation();
                }
            }));
        div()
            .w_full()
            .h(px(SIDEBAR_ACTION_ROW_HEIGHT + SIDEBAR_SEARCH_BOTTOM_GAP))
            .flex_none()
            .child(search)
    }

    fn render_sidebar_footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        div()
            .flex_none()
            .h(px(40.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(
                div()
                    .id("open-settings")
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .h(px(32.0))
                    .px(px(10.0))
                    .flex_none()
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .text_size(ui_px(13.0))
                    .text_color(theme.text_secondary)
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .tooltip(Tooltip::text(tr_cow!("common.settings")))
                    .child(icon("icons/settings.svg", 15.0, theme.text_tertiary))
                    .child(tr_cow!("common.settings"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_settings_action(&OpenSettings, window, cx);
                    })),
            )
            .when_some(self.latest_available.clone(), |footer, version| {
                footer.child(
                    div()
                        .id("open-update")
                        .tab_index(0)
                        .focus_visible(|style| style.border_1().border_color(theme.accent))
                        .h(px(32.0))
                        .px(px(10.0))
                        .flex_none()
                        .rounded(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .text_size(ui_px(13.0))
                        .text_color(theme.text_secondary)
                        .cursor_default()
                        .hover(|element| element.bg(theme.overlay))
                        .active(|element| element.bg(theme.overlay_strong))
                        .tooltip(Tooltip::text(tr!(
                            "sidebar.update_tooltip",
                            version = version
                        )))
                        .child(icon("icons/download.svg", 15.0, theme.text_tertiary))
                        .child(tr_cow!("sidebar.update"))
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.open_url(crate::update::RELEASES_LATEST_URL);
                        }))
                        .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                cx.open_url(crate::update::RELEASES_LATEST_URL);
                                cx.stop_propagation();
                            }
                        })),
                )
            })
            .child(div().flex_1())
    }

    pub(super) fn render_sidebar(
        &self,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let is_resizing = self
            .panel_resize_drag
            .is_some_and(|drag| drag.target == PanelResizeTarget::Sidebar);

        let rows = self.sidebar_rows_cached();
        self.sync_sidebar_rows(&rows);
        let history_scrolled =
            self.sidebar_list_state.scroll_px_offset_for_scrollbar().y < px(-0.5);
        let entity = cx.entity().downgrade();

        div()
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(if is_resizing {
                theme.sidebar_drag_background
            } else {
                theme.sidebar
            })
            .child(self.render_sidebar_titlebar(window, cx))
            .child(
                div()
                    .flex_none()
                    .px(px(10.0))
                    .child(self.render_sidebar_new_session(cx)),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div().px(px(10.0)).size_full().child(
                            list(
                                self.sidebar_list_state.clone(),
                                move |index, _window, cx| {
                                    entity
                                        .upgrade()
                                        .map(|entity| {
                                            entity.update(cx, |this, cx| {
                                                this.sidebar_row(index, &rows, cx)
                                            })
                                        })
                                        .unwrap_or_else(|| div().into_any_element())
                                },
                            )
                            .size_full(),
                        ),
                    )
                    .child(scrollbar::vertical(
                        &self.sidebar_list_state,
                        &self.sidebar_scrollbar,
                    ))
                    .when(history_scrolled, |scroll| {
                        scroll.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .w_full()
                                .h(px(1.0))
                                .bg(theme.border),
                        )
                    }),
            )
            .child(self.render_sidebar_footer(cx))
    }

    /// The sidebar row snapshot, rebuilt only when its inputs move.
    ///
    /// The sidebar re-renders at pulse cadence whenever one of its session
    /// rows shows a working spinner, and rebuilding the snapshot sorts every
    /// started session — far too much per tick for values that move at most
    /// once per stream commit. The fingerprint is an allocation-free scan of
    /// exactly what [`Self::sidebar_rows`] reads: started sessions with their
    /// recency timestamps and project, the projects that seed sessionless
    /// groups with their `created_at` recency, the expanded-project set, and
    /// the language the headers are localized in.
    fn sidebar_rows_cached(&self) -> Rc<Vec<SidebarRow>> {
        let mut fingerprint = mix(0x51de_ba5e_5eed_c0de, self.state.language as u64);
        for session in &self.state.sessions {
            if !session.has_started() {
                continue;
            }
            fingerprint = mix_uuid(fingerprint, session.id);
            fingerprint = mix(fingerprint, sidebar_session_timestamp(session));
            fingerprint = mix_uuid(fingerprint, session.project_id);
        }
        for project in &self.state.projects {
            fingerprint = mix_uuid(fingerprint, project.id);
            fingerprint = mix(fingerprint, project.created_at);
        }
        // A set has no stable iteration order; combine order-independently.
        let expanded = self
            .sidebar_expanded_groups
            .iter()
            .fold(0u64, |combined, project| {
                combined.wrapping_add(mix_uuid(0, *project))
            });
        fingerprint = mix(
            mix(fingerprint, self.sidebar_expanded_groups.len() as u64),
            expanded,
        );
        if self.sidebar_rows_fingerprint.get() != Some(fingerprint) {
            *self.sidebar_rows_snapshot.borrow_mut() = Rc::new(self.sidebar_rows());
            self.sidebar_rows_fingerprint.set(Some(fingerprint));
        }
        self.sidebar_rows_snapshot.borrow().clone()
    }

    /// Snapshot the session history as a flat list of lightweight rows,
    /// grouped by project. The most recently active project comes first, and
    /// sessions inside a project keep global recency order, newest first.
    /// Projects without history keep a bare header so they stay reachable.
    fn sidebar_rows(&self) -> Vec<SidebarRow> {
        let mut rows = vec![SidebarRow::Search];
        for (project_id, sessions) in
            sidebar_project_groups(&self.state.sessions, &self.state.projects)
        {
            rows.push(SidebarRow::Group(Rc::new(SidebarGroup {
                project_id,
                sessions: Rc::new(sessions),
                unfolded: self.sidebar_expanded_groups.contains(&project_id),
            })));
            rows.push(SidebarRow::GroupSpacer);
        }
        rows
    }

    /// Keep the virtualized list in sync with the current row snapshot.
    /// Rows are cheap values, so only the minimal changed suffix is spliced,
    /// preserving scroll position and measured heights across unrelated churn
    /// (e.g. the active session's `updated_at` bumping on every stream tick).
    fn sync_sidebar_rows(&self, rows: &[SidebarRow]) {
        let mut cached = self.sidebar_row_cache.borrow_mut();
        if cached.as_slice() == rows {
            return;
        }
        let prefix = cached
            .iter()
            .zip(rows.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let old_count = cached.len();
        *cached = rows.to_vec();
        if old_count == 0 {
            self.sidebar_list_state
                .reset_with_uniform_height(rows.len(), px(SIDEBAR_SESSION_ROW_HEIGHT));
        } else {
            self.sidebar_list_state
                .splice(prefix..old_count, rows.len() - prefix);
            // Newly inserted rows have no measured height yet; give them the
            // uniform hint so the scrollbar keeps a correct total height.
            self.sidebar_list_state
                .clone()
                .with_uniform_item_height(px(SIDEBAR_SESSION_ROW_HEIGHT));
        }
        // A group row is several times taller than the session hint, and the
        // list's scroll extent comes from those hints until a row is measured.
        // Measure every row once per snapshot instead: there is one per
        // project, and a body's height is fixed, so this lays out the headers
        // and bodies — never the session rows inside them.
        let _ = self.sidebar_list_state.clone().measure_all();
    }

    fn sidebar_row(
        &mut self,
        index: usize,
        rows: &[SidebarRow],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = rows.get(index) else {
            return div().into_any_element();
        };
        match row {
            SidebarRow::Search => self.render_sidebar_search(cx).into_any_element(),
            SidebarRow::Group(group) => self
                .render_sidebar_group(group, cx)
                .into_any_element(),
            SidebarRow::GroupSpacer => div().w_full().h(px(10.0)).into_any_element(),
        }
    }

    /// One project group: the header card, and — while the group is unfolded —
    /// its sessions in a capped, virtualized list of their own, so a project
    /// with hundreds of tasks scrolls inside its own group instead of pushing
    /// the rest of the sidebar off screen.
    fn render_sidebar_group(&self, group: &SidebarGroup, cx: &mut Context<Self>) -> Div {
        let container = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(self.render_sidebar_group_header(group.project_id, !group.unfolded, cx));
        if !group.unfolded || group.sessions.is_empty() {
            return container;
        }
        let scroll = self.sidebar_group_scroll(group.project_id, group.sessions.len());
        let sessions = group.sessions.clone();
        let entity = cx.entity().downgrade();
        let wheel_list = scroll.list.clone();
        container.child(
            div()
                .id(SharedString::from(format!(
                    "sidebar-group-body-{}",
                    group.project_id
                )))
                .w_full()
                .min_w_0()
                .h(px(sidebar_group_body_height(group.sessions.len())))
                .relative()
                // The group list is nested in the sidebar list, so keep the
                // wheel inside it while it has overflow of its own; a group
                // that fits keeps chaining to the sidebar as before.
                .on_scroll_wheel(move |_, _, cx| contain_scroll(&wheel_list, cx))
                .child(
                    list(scroll.list.clone(), move |index, _window, cx| {
                        let Some(session_id) = sessions.get(index).copied() else {
                            return div().into_any_element();
                        };
                        entity
                            .upgrade()
                            .map(|entity| {
                                entity.update(cx, |this, cx| {
                                    this.render_sidebar_session_item(session_id, cx)
                                })
                            })
                            .unwrap_or_else(|| div().into_any_element())
                    })
                    .size_full(),
                )
                .child(scrollbar::vertical(&scroll.list, &scroll.scrollbar)),
        )
    }

    /// The scroll state behind one unfolded group's body, created on first use
    /// and kept for the window's lifetime so the group's scroll position
    /// survives folding it away and unfolding it again.
    fn sidebar_group_scroll(&self, project_id: Uuid, sessions: usize) -> SidebarGroupScroll {
        let mut scrolls = self.sidebar_group_scrolls.borrow_mut();
        let scroll = scrolls
            .entry(project_id)
            .or_insert_with(|| SidebarGroupScroll {
                list: ListState::new(0, ListAlignment::Top, px(SIDEBAR_GROUP_OVERDRAW)),
                scrollbar: ScrollbarState::new(),
            });
        if scroll.list.item_count() != sessions {
            // Splicing the whole range drops the list to the top, so hold the
            // user's place across a task being added or removed. A task added
            // at the top is revealed by `reveal_sidebar_session_project`.
            let anchor = scroll.list.logical_scroll_top();
            scroll.list.splice(0..scroll.list.item_count(), sessions);
            // Session rows are a fixed height, and the body is sized from the
            // same constant, so the hint makes the group's extent exact before
            // any of its rows have been measured.
            let _ = scroll
                .list
                .clone()
                .with_uniform_item_height(px(SIDEBAR_SESSION_ROW_HEIGHT));
            if anchor.item_ix < sessions {
                scroll.list.scroll_to(anchor);
            }
        }
        scroll.clone()
    }

    fn render_sidebar_group_header(
        &self,
        project_id: Uuid,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let project_name = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(Project::display_name)
            .unwrap_or_else(|| tr!("sidebar.unknown_project"));
        let chevron = icon("icons/chevron-down.svg", 11.0, theme.text_ghost)
            .when(collapsed, |icon| {
                icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.75)))
            });
        let fintwind = cx.entity().downgrade();
        let menu = self.menu_handle(format!("project-{project_id}"), cx);
        let keyboard_menu = menu.clone();
        let toggle = div()
            .id(SharedString::from(format!(
                "sidebar-group-toggle-{project_id}"
            )))
            .tab_index(0)
            .h_full()
            .flex_1()
            .min_w_0()
            .px(px(4.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .child(icon("icons/folder.svg", 12.0, theme.text_ghost))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(project_name)),
            )
            .child(chevron)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_sidebar_group(project_id, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "enter" | "space" => {
                        this.toggle_sidebar_group(project_id, cx);
                        cx.stop_propagation();
                    }
                    "left" if !collapsed => {
                        this.set_sidebar_group_collapsed(project_id, true, cx);
                        cx.stop_propagation();
                    }
                    "right" if collapsed => {
                        this.set_sidebar_group_collapsed(project_id, false, cx);
                        cx.stop_propagation();
                    }
                    "f10" if event.keystroke.modifiers.shift => {
                        keyboard_menu.open_context_menu(window, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }));
        let new_session = div()
            .id(SharedString::from(format!(
                "sidebar-group-new-session-{project_id}"
            )))
            .tab_index(0)
            .size(px(28.0))
            .flex_none()
            .rounded(px(7.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .tooltip(Tooltip::text(tr_cow!("menu.new_task")))
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon("icons/compose.svg", 14.0, theme.text_tertiary))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.start_session_for_project(project_id, window, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    this.start_session_for_project(project_id, window, cx);
                    cx.stop_propagation();
                }
            }));
        div()
            .w_full()
            .min_w_0()
            .pb(px(SIDEBAR_PROJECT_CARD_BOTTOM_GAP))
            .child(context_menu(
                div()
                    // The card carries the unified hover highlight so the
                    // whole header lights up as one surface, the way the
                    // session cards do — a hover on the inner toggle would
                    // otherwise paint a detached blob that stops short of the
                    // new-session control. The toggle unfolds the group rather
                    // than selecting anything, so only focus is drawn on it;
                    // the new-session button keeps its own press highlight.
                    .id(SharedString::from(format!(
                        "sidebar-project-card-{project_id}"
                    )))
                    .w_full()
                    .min_w_0()
                    .h(px(SIDEBAR_PROJECT_CARD_HEIGHT))
                    .px(px(4.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .cursor_default()
                    .hover(|element| element.bg(theme.sidebar_item_background))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .text_size(ui_px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_tertiary)
                    .child(toggle)
                    .child(new_session),
                SharedString::from(format!("project-menu-{project_id}")),
                &menu,
                move |_| {
                    let remove_fintwind = fintwind.clone();
                    vec![MenuItem::new(tr!("project.remove"), move |_, cx| {
                        let _ = remove_fintwind.update(cx, |fintwind, cx| {
                            fintwind.remove_project(project_id, cx);
                        });
                    })]
                },
            ))
    }

    fn start_session_for_project(
        &mut self,
        project_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_page = None;
        self.select_project(project_id, cx);
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
    }

    fn toggle_sidebar_group(&mut self, project_id: Uuid, cx: &mut Context<Self>) {
        let collapsed = self.sidebar_expanded_groups.contains(&project_id);
        self.set_sidebar_group_collapsed(project_id, collapsed, cx);
    }

    fn set_sidebar_group_collapsed(
        &mut self,
        project_id: Uuid,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if collapsed {
            self.sidebar_expanded_groups.remove(&project_id)
        } else {
            self.sidebar_expanded_groups.insert(project_id)
        };
        if changed {
            cx.notify();
        }
    }

    /// Reveals the group that owns `session_id` so a task the user just
    /// switched to (or created) is visible even though groups start folded.
    /// The group's body is capped, so unfolding it is not enough on its own:
    /// the task is also brought into the group's own viewport.
    pub(super) fn reveal_sidebar_session_project(
        &mut self,
        session_id: Uuid,
        cx: &mut Context<Self>,
    ) {
        let Some(project_id) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| session.project_id)
        else {
            return;
        };
        if self.sidebar_expanded_groups.insert(project_id) {
            cx.notify();
        }
        let rows = self.sidebar_rows_cached();
        let Some((sessions, index)) = rows.iter().find_map(|row| match row {
            SidebarRow::Group(group) if group.project_id == project_id => group
                .sessions
                .iter()
                .position(|session| *session == session_id)
                .map(|index| (group.sessions.len(), index)),
            _ => None,
        }) else {
            return;
        };
        self.reveal_sidebar_group_session(project_id, sessions, index);
    }

    /// Bring session `index` of a group into view inside the group's capped
    /// body, leaving the group's scroll alone when the row is already visible.
    ///
    /// Row and body heights are both fixed constants, so the visible window is
    /// known before the body has ever been laid out — which is the case that
    /// matters here, since the group is usually unfolded by this very switch.
    fn reveal_sidebar_group_session(&self, project_id: Uuid, sessions: usize, index: usize) {
        let scroll = self.sidebar_group_scroll(project_id, sessions);
        let row_height = SIDEBAR_SESSION_ROW_HEIGHT;
        let row_top = index as f32 * row_height;
        let viewport = sidebar_group_body_height(sessions);
        let scrolled = -f32::from(scroll.list.scroll_px_offset_for_scrollbar().y);
        if row_top >= scrolled && row_top + row_height <= scrolled + viewport {
            return;
        }
        // Park the row at the bottom of the window: the rows above it are the
        // group's newer tasks, and revealing an old one should not push those
        // off screen.
        let target = (row_top + row_height - viewport).max(0.0);
        let item_ix = (target / row_height).floor() as usize;
        scroll.list.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(target - item_ix as f32 * row_height),
        });
    }

    fn begin_session_rename(
        &mut self,
        session_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(title) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(localized_session_title)
        else {
            return;
        };

        self.session_rename = Some(session_id);
        self.session_rename_input.update(cx, |input, cx| {
            input.set_content(title, cx);
            input.select_all_text(cx);
        });
        let focus = self.session_rename_input.read(cx).focus();
        window.on_next_frame(move |window, cx| window.focus(&focus, cx));
        cx.notify();
    }

    pub(super) fn commit_session_rename(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_rename.take() else {
            return;
        };
        let title = self
            .session_rename_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        let should_update = !title.is_empty()
            && self
                .state
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| session.title != title);
        if should_update
            && self
                .state
                .session_mut(session_id)
                .is_some_and(|session| session.set_title(&title))
        {
            self.save();
        }
        // A session backed by a native OpenCode session carries the rename to
        // the server, so the CLI and TUI see the same title.
        self.rename_native_session(session_id, &title, cx);
        cx.notify();
    }

    fn cancel_session_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_rename.take().is_none() {
            return;
        }
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn render_sidebar_session_item(
        &mut self,
        session_id: Uuid,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        // Everything the card draws is read off the session before the branch
        // cache is consulted, because its read needs `&mut self`.
        let (
            status,
            working,
            time_label,
            title_text,
            stored_branch,
            workspace_path,
            model_id,
            model_name,
            provider,
        ) = {
            let Some(session) = self
                .state
                .sessions
                .iter()
                .find(|session| session.id == session_id)
            else {
                return div().into_any_element();
            };
            // A materialized worktree already stores its branch. A local
            // checkout reads the lightweight per-workspace branch cache, which
            // answers from memory and only schedules background work on a miss.
            let (stored_branch, workspace_path) = match &session.workspace {
                SessionWorkspace::Worktree { branch, .. } => (Some(branch.clone()), None),
                SessionWorkspace::Local | SessionWorkspace::NewWorktree { .. } => (
                    None,
                    self.workspace_path_for_session(session)
                        .map(std::path::Path::to_path_buf),
                ),
            };
            let model_id = self.model_for_session(session).map(str::to_owned);
            let model_name = self.model_display_name(model_id.as_deref());
            (
                session.status,
                matches!(
                    session.status,
                    SessionStatus::Connecting | SessionStatus::Working
                ),
                session_time_label(session, unix_time()),
                localized_session_title(session),
                stored_branch,
                workspace_path,
                model_id,
                model_name,
                session.provider.clone(),
            )
        };
        let branch = if let Some(branch) = stored_branch {
            Some(branch)
        } else if let Some(path) = workspace_path.as_deref() {
            let cached = self.sidebar_branch_for_workspace(path, cx);
            cached.or_else(|| {
                // Stale-while-revalidate: invalidation (a command finishing,
                // switching sessions, app reactivation) drops the cached name,
                // so keep drawing the selected workspace's last known branch
                // until the fresh one lands instead of flickering the row.
                self.visible_branch_snapshot
                    .as_ref()
                    .filter(|(cached, _)| cached == path)
                    .and_then(|(_, snapshot)| snapshot.display_branch().map(str::to_owned))
            })
        } else {
            None
        };
        let selected = sidebar_session_selected(
            self.state.selected_session,
            self.pending_session_activation
                .map(|pending| pending.session_id),
            session_id,
        );
        let rename_input =
            (self.session_rename == Some(session_id)).then(|| self.session_rename_input.clone());
        let renaming = rename_input.is_some();
        let title = if let Some(rename_input) = rename_input {
            div()
                .id(SharedString::from(format!(
                    "session-rename-field-{session_id}"
                )))
                .key_context(SESSION_RENAME_PARENT_CONTEXT)
                .on_action(cx.listener(|this, _: &CancelSessionRename, window, cx| {
                    this.cancel_session_rename(window, cx);
                }))
                .h(px(20.0))
                .flex_1()
                .min_w_0()
                .px(px(4.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.accent)
                .bg(theme.inset)
                .flex()
                .items_center()
                .text_size(ui_px(13.5))
                .text_color(theme.text)
                .child(rename_input)
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(ui_px(13.5))
                .text_color(theme.text)
                .child(SharedString::from(title_text))
                .into_any_element()
        };
        let avatar = session_model_avatar(
            model_icon(model_id.as_deref().unwrap_or(""), &model_name, &provider),
            provider_color(&theme, &provider),
            working,
            &theme,
        );
        let metadata_time = time_label.map(|label| {
            div()
                .flex_none()
                .text_size(ui_px(11.0))
                .text_color(if status.is_busy() {
                    theme.text_tertiary
                } else {
                    theme.text_ghost
                })
                .child(SharedString::from(label))
        });
        let title_row = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(title)
            .when(status == SessionStatus::Waiting, |element| {
                element.child(icon("icons/alert.svg", 12.0, status_color(&theme, status)))
            })
            .when(status == SessionStatus::Failed, |element| {
                element.child(icon("icons/x.svg", 12.0, status_color(&theme, status)))
            });
        let has_meta = metadata_time.is_some() || branch.is_some();
        let meta_row = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.0))
            .when_some(metadata_time, |element, time| element.child(time))
            .when_some(branch, |element, branch| {
                element.child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .child(icon("icons/git-branch.svg", 11.0, theme.text_ghost))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(ui_px(11.0))
                                .text_color(theme.text_ghost)
                                .child(SharedString::from(branch)),
                        ),
                )
            });

        let fintwind = cx.entity().downgrade();
        let menu = self.menu_handle(format!("session-{session_id}"), cx);
        let row_focus = menu.trigger_focus_handle().clone();
        let keyboard_menu = menu.clone();
        let row = div()
            .id(SharedString::from(format!("session-{session_id}")))
            .w_full()
            .min_w_0()
            .h(px(SIDEBAR_SESSION_CARD_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .rounded(px(8.0))
            .cursor_default()
            .when(selected, |element| {
                element.bg(theme.sidebar_item_background)
            })
            .hover(|element| element.bg(theme.sidebar_item_background))
            .active(|element| element.bg(theme.overlay_strong))
            .child(avatar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(2.0))
                    .child(title_row)
                    .when(has_meta, |element| element.child(meta_row)),
            )
            .when(!renaming, |element| {
                element
                    .track_focus(&row_focus)
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        let key = event.keystroke.key.as_str();
                        if matches!(key, "enter" | "space") {
                            this.select_session(session_id, cx);
                            cx.stop_propagation();
                        } else if key == "f10" && event.keystroke.modifiers.shift {
                            keyboard_menu.open_context_menu(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_session(session_id, cx);
                    }))
            });
        let row = if renaming {
            div()
                .w_full()
                .child(row)
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    if this.session_rename == Some(session_id) {
                        this.commit_session_rename(cx);
                    }
                }))
                .into_any_element()
        } else {
            context_menu(
                div().w_full().child(row),
                SharedString::from(format!("session-menu-{session_id}")),
                &menu,
                move |_| {
                    let rename_fintwind = fintwind.clone();
                    let remove_fintwind = fintwind.clone();
                    vec![
                        MenuItem::new(tr!("common.rename"), move |window, cx| {
                            let _ = rename_fintwind.update(cx, |fintwind, cx| {
                                fintwind.begin_session_rename(session_id, window, cx);
                            });
                        }),
                        MenuItem::Separator,
                        MenuItem::new(tr!("common.remove"), move |_, cx| {
                            let _ = remove_fintwind
                                .update(cx, |fintwind, cx| fintwind.remove_session(session_id, cx));
                        }),
                    ]
                },
            )
        };

        div()
            .w_full()
            .pb(px(SIDEBAR_SESSION_ROW_GAP))
            .child(row)
            .into_any_element()
    }

    // ── Header ─────────────────────────────────────────────────────────────

    pub(super) fn render_header(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let title = session
            .map(localized_session_title)
            .unwrap_or_else(|| tr!("session.new_task"));
        let left_window_controls = (!self.sidebar_visible)
            .then(|| {
                self.render_client_window_controls(
                    super::window_chrome::WindowControlSide::Left,
                    window,
                    cx,
                )
            })
            .flatten();
        let right_window_controls = (!self.right_panel_visible)
            .then(|| {
                self.render_client_window_controls(
                    super::window_chrome::WindowControlSide::Right,
                    window,
                    cx,
                )
            })
            .flatten();
        div()
            .id("window-header")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .children(left_window_controls)
            // The header starts where the sidebar ends, so until the sidebar
            // is wide enough to host the traffic lights itself the header has
            // to clear them. Steady state with the sidebar open adds nothing;
            // a sidebar sliding in shrinks the inset as it takes the lights
            // over, which is what keeps the title from passing under them.
            .pl(if self.sidebar_visible {
                px(14.0 + (TRAFFIC_LIGHT_CLEARANCE - self.sidebar_rendered_width).max(0.0))
            } else {
                px(0.0)
            })
            // Caption buttons sit on the window edge; Win11 chrome has no inset.
            .pr(if right_window_controls.is_some() {
                px(0.0)
            } else {
                px(14.0)
            })
            .when(!self.sidebar_visible, |element| {
                element
                    .child(
                        self.window_drag_region(
                            div()
                                .id("header-traffic-light-drag-region")
                                .w(px(TRAFFIC_LIGHT_CLEARANCE - 8.0))
                                .h_full()
                                .flex_none(),
                            cx,
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(self.render_sidebar_toggle(cx))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.0))
                                    .child(self.render_history_button(
                                        "navigate-back",
                                        "icons/arrow-left.svg",
                                        !self.session_navigation.back.is_empty(),
                                        true,
                                        cx,
                                    ))
                                    .child(self.render_history_button(
                                        "navigate-forward",
                                        "icons/arrow-right.svg",
                                        !self.session_navigation.forward.is_empty(),
                                        false,
                                        cx,
                                    )),
                            ),
                    )
            })
            .child(
                self.window_drag_region(
                    div()
                        .id("header-title-drag-region")
                        .h_full()
                        .min_w_0()
                        .flex_shrink(1.0)
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(ui_px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(SharedString::from(title)),
                        ),
                    cx,
                ),
            )
            .child(
                self.window_drag_region(
                    div().id("header-center-drag-region").h_full().flex_1(),
                    cx,
                ),
            )
            .children(self.render_reveal_project_button(cx))
            .when(!self.right_panel_visible, |element| {
                element
                    .when(self.fps_counter_visible, |element| {
                        element.child(self.render_fps_counter(cx))
                    })
                    .child(self.render_right_panel_toggle(cx))
            })
            .children(right_window_controls)
    }

    // ── Empty states ───────────────────────────────────────────────────────

    pub(super) fn render_empty_state(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        if self.selected_project().is_none() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .px_8()
                .pb(px(46.0))
                .child(icon("icons/sparkle.svg", 24.0, theme.accent))
                .child(
                    div()
                        .mt(px(16.0))
                        .text_size(ui_px(20.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(tr_cow!("onboarding.open_project_to_begin")),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .max_w(px(380.0))
                        .text_center()
                        .text_size(ui_px(12.5))
                        .line_height(ui_px(19.0))
                        .text_color(theme.text_tertiary)
                        .child(tr_cow!("onboarding.description")),
                )
                .child(
                    div()
                        .mt(px(20.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.0))
                        .tab_index(0)
                        .tab_group()
                        .tab_stop(false)
                        .child(
                            div()
                                .id("onboarding-add-project")
                                .track_focus(&self.onboarding_add_project_focus)
                                .tab_index(0)
                                .focus_visible(|style| style.border_1().border_color(theme.accent))
                                .h(px(32.0))
                                .px(px(14.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .cursor_default()
                                .bg(theme.inverse)
                                .text_color(theme.on_inverse)
                                .text_size(ui_px(12.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .hover(|element| element.opacity(0.9))
                                .active(|element| element.opacity(0.8))
                                .child(tr_cow!("onboarding.open_project_folder"))
                                .on_click(cx.listener(|this, _, _, cx| this.add_project(cx)))
                                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                        this.add_project(cx);
                                        cx.stop_propagation();
                                    }
                                })),
                        )
                        .child(
                            div()
                                .id("onboarding-projectless")
                                .track_focus(&self.onboarding_projectless_focus)
                                .tab_index(1)
                                .focus_visible(|style| style.border_1().border_color(theme.accent))
                                .h(px(30.0))
                                .px(px(12.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .cursor_default()
                                .text_color(theme.text_secondary)
                                .text_size(ui_px(12.0))
                                .hover(|element| element.bg(theme.overlay))
                                .active(|element| element.bg(theme.overlay_strong))
                                .child(icon("icons/x.svg", 11.0, theme.text_tertiary))
                                .child(tr_cow!("project.no_project"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.create_projectless_session(cx);
                                }))
                                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                        this.create_projectless_session(cx);
                                        cx.stop_propagation();
                                    }
                                })),
                        ),
                );
        }
        let selected_project_id = self.state.selected_project;
        let projectless_selected = self.selected_project().is_some_and(Project::is_projectless);
        let project_name = self
            .selected_project()
            .map(|project| {
                if project.is_projectless() {
                    tr!("project.without_a_project")
                } else {
                    project.display_name()
                }
            })
            .unwrap_or_else(|| tr!("project.your_project"));
        let project_options = self
            .state
            .projects
            .iter()
            .filter(|project| !project.is_projectless())
            .filter(|project| Some(project.id) == selected_project_id)
            .chain(
                self.state
                    .projects
                    .iter()
                    .filter(|project| !project.is_projectless())
                    .filter(|project| Some(project.id) != selected_project_id),
            )
            .map(|project| (project.id, project.display_name()))
            .collect::<Vec<_>>();
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("empty-state-project", cx);
        let project_selector = dropdown_menu(
            ProjectNameSelector::new("empty-state-project", project_name)
                .selected(handle.is_open()),
            "empty-state-project-menu",
            &handle,
            MenuAlign::BelowLeft,
            move |_| {
                let mut items = project_options
                    .clone()
                    .into_iter()
                    .map(|(project_id, project_name)| {
                        let weak = weak.clone();
                        MenuItem::new(project_name, move |_, cx| {
                            if Some(project_id) == selected_project_id {
                                return;
                            }
                            let _ = weak.update(cx, |this, cx| this.select_project(project_id, cx));
                        })
                        .selected(Some(project_id) == selected_project_id)
                    })
                    .collect::<Vec<_>>();
                if !items.is_empty() {
                    items.push(MenuItem::Separator);
                }
                let add_project_weak = weak.clone();
                items.push(
                    MenuItem::new(tr!("project.new_project"), move |_, cx| {
                        let _ = add_project_weak.update(cx, |this, cx| this.add_project(cx));
                    })
                    .icon("icons/folder-new.svg"),
                );
                let projectless_weak = weak.clone();
                items.push(
                    MenuItem::new(tr!("project.no_project"), move |_, cx| {
                        let _ = projectless_weak.update(cx, |this, cx| {
                            if !this.selected_project().is_some_and(Project::is_projectless) {
                                this.create_projectless_session(cx);
                            }
                        });
                    })
                    .icon("icons/x.svg")
                    .selected(projectless_selected),
                );
                items
            },
        );
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_8()
            .pb(px(52.0))
            .child(icon("icons/sparkle.svg", 20.0, theme.accent))
            .child(
                div()
                    .mt(px(14.0))
                    .flex()
                    .items_baseline()
                    .text_size(ui_px(20.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .when(projectless_selected, |element| {
                        element.child(tr_cow!("onboarding.what_should_we_build"))
                    })
                    .when(!projectless_selected, |element| {
                        element
                            .child(tr_cow!("onboarding.what_should_we_build_in"))
                            .child(project_selector)
                            .child(tr_cow!("onboarding.question_mark"))
                    }),
            )
    }
}

fn session_model_avatar(
    icon_path: &'static str,
    icon_color: Hsla,
    working: bool,
    theme: &Theme,
) -> AnyElement {
    div()
        .size(px(SESSION_AVATAR_SIZE))
        .flex_none()
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .when(working, |element| {
            element.child(
                div()
                    .absolute()
                    .inset_0()
                    .child(spin_halo(theme.accent, SESSION_AVATAR_SIZE)),
            )
        })
        .child(
            div()
                .size(px(SESSION_AVATAR_INNER))
                .rounded_full()
                .bg(theme.raised)
                .border_1()
                .border_color(theme.border_strong)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icon_path, SESSION_AVATAR_ICON, icon_color)),
        )
        .into_any_element()
}

fn localized_session_title(session: &AgentSession) -> String {
    let title = session.display_title();
    if title == AgentSession::DEFAULT_TITLE {
        tr!("session.new_task")
    } else {
        title.to_owned()
    }
}

fn sidebar_session_selected(
    selected_session: Option<Uuid>,
    pending_session: Option<Uuid>,
    session_id: Uuid,
) -> bool {
    pending_session.map_or(selected_session == Some(session_id), |pending| {
        pending == session_id
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_started_at(project_id: Uuid, replied_at: u64) -> AgentSession {
        let mut session = AgentSession::new(project_id);
        session.begin_turn("Start it");
        // `begin_turn` stamps `last_reply_at` with the wall clock; the
        // fixture's recency is what the ordering must follow.
        session.created_at = replied_at;
        session.last_reply_at = Some(replied_at);
        session
    }

    #[test]
    fn sessions_are_grouped_by_project_with_the_newest_project_first() {
        let project_a = Uuid::new_v4();
        let project_b = Uuid::new_v4();
        let a_old = session_started_at(project_a, 10);
        let a_new = session_started_at(project_a, 100);
        let b_only = session_started_at(project_b, 50);

        let groups = sidebar_project_groups(&[a_old.clone(), b_only.clone(), a_new.clone()], &[]);
        assert_eq!(
            groups,
            vec![
                (project_a, vec![a_new.id, a_old.id]),
                (project_b, vec![b_only.id]),
            ]
        );
    }

    #[test]
    fn unstarted_sessions_stay_out_of_the_sidebar() {
        let project = Uuid::new_v4();
        let draft = AgentSession::new(project);
        assert!(sidebar_project_groups(&[draft], &[]).is_empty());
    }

    #[test]
    fn projects_without_sessions_keep_a_group() {
        let active = Uuid::new_v4();
        let session = session_started_at(active, 50);
        let mut stale = Project::from_path("C:\\stale".into());
        stale.created_at = 10;
        let mut fresh = Project::from_path("C:\\fresh".into());
        fresh.created_at = 100;

        let groups = sidebar_project_groups(&[session.clone()], &[stale.clone(), fresh.clone()]);
        assert_eq!(
            groups,
            vec![
                (fresh.id, vec![]),
                (active, vec![session.id]),
                (stale.id, vec![])
            ]
        );
    }

    #[test]
    fn projectless_projects_wait_for_a_started_session() {
        // `is_projectless` classifies by path, so anchor the pseudo-project
        // under a dedicated workspace root for the duration of the check.
        let root = std::env::temp_dir().join("fintwind-sidebar-projectless-test");
        fintwind_protocol::projectless::set_workspace_root(Some(root.clone()));
        let projectless = Project::from_path(root.join("scratch"));
        assert!(projectless.is_projectless());
        assert!(sidebar_project_groups(&[], &[projectless]).is_empty());
    }

    #[test]
    fn an_unfolded_group_caps_its_body_height() {
        // A short group is exactly as tall as its rows; a long one stops at the
        // cap, so the remainder scrolls inside the group.
        assert_eq!(sidebar_group_body_height(0), 0.0);
        assert_eq!(
            sidebar_group_body_height(2),
            SIDEBAR_SESSION_ROW_HEIGHT * 2.0
        );
        assert_eq!(sidebar_group_body_height(6), SIDEBAR_GROUP_BODY_MAX_HEIGHT);
        assert_eq!(
            sidebar_group_body_height(200),
            SIDEBAR_GROUP_BODY_MAX_HEIGHT
        );
    }

    #[test]
    fn sidebar_recency_uses_last_reply_with_creation_fallback() {
        let project_id = Uuid::new_v4();
        let mut renamed_old_session = AgentSession::new(project_id);
        renamed_old_session.created_at = 10;
        renamed_old_session.last_reply_at = Some(20);
        renamed_old_session.updated_at = 1_000;

        let mut newer_unanswered_session = AgentSession::new(project_id);
        newer_unanswered_session.created_at = 30;
        newer_unanswered_session.last_reply_at = None;
        newer_unanswered_session.updated_at = 30;

        assert_eq!(sidebar_session_timestamp(&renamed_old_session), 20);
        assert_eq!(sidebar_session_timestamp(&newer_unanswered_session), 30);

        let mut sessions = [&renamed_old_session, &newer_unanswered_session];
        sessions.sort_by_key(|session| std::cmp::Reverse(sidebar_session_timestamp(session)));
        assert_eq!(sessions[0].id, newer_unanswered_session.id);
    }

    #[test]
    fn pending_session_replaces_sidebar_selection_immediately() {
        let current = Uuid::from_u128(1);
        let pending = Uuid::from_u128(2);

        assert!(!sidebar_session_selected(
            Some(current),
            Some(pending),
            current
        ));
        assert!(sidebar_session_selected(
            Some(current),
            Some(pending),
            pending
        ));
        assert!(sidebar_session_selected(Some(current), None, current));
    }
}

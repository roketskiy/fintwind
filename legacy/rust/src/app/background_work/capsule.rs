use super::*;
use crate::ui::menu::{popover_covering_trigger, toggle_popover};
use ely_gpui_component::motion::Spinner;
use ely_gpui_component::theme::IconSize;
use ely_gpui_component::typography::tabular;
use fintwind_protocol::pull_request::{
    PullRequest, PullRequestChecks, PullRequestSnapshot, PullRequestState, PullRequestUnavailable,
};
use gpui::Size;

use super::todo::{TodoSnapshot, TodoStatus};

const CAPSULE_TABS_HEIGHT: f32 = 32.0;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(in crate::app) enum CapsuleTab {
    Changes,
    Todo,
    Tasks,
    PullRequests,
}

impl CapsuleTab {
    const ALL: [Self; 4] = [Self::Changes, Self::Todo, Self::Tasks, Self::PullRequests];

    fn id(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Todo => "todo",
            Self::Tasks => "tasks",
            Self::PullRequests => "pull-requests",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Changes => "icons/file-diff.svg",
            Self::Todo => "icons/list.svg",
            Self::Tasks => "icons/bot.svg",
            Self::PullRequests => "icons/git-branch.svg",
        }
    }

    fn label(self) -> String {
        match self {
            Self::Changes => tr!("environment.changes"),
            Self::Todo => tr!("capsule.todo"),
            Self::Tasks => tr!("capsule.task_list"),
            Self::PullRequests => tr!("capsule.pull_requests"),
        }
    }

    fn visible(self, todo_enabled: bool) -> bool {
        self != Self::Todo || todo_enabled
    }

    fn visible_tabs(todo_enabled: bool) -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(move |tab| tab.visible(todo_enabled))
    }

    fn navigate(self, key: &str, todo_enabled: bool) -> Option<Self> {
        let tabs: Vec<Self> = Self::visible_tabs(todo_enabled).collect();
        let index = tabs.iter().position(|tab| *tab == self)?;
        let next = match key {
            "left" => (index + tabs.len() - 1) % tabs.len(),
            "right" => (index + 1) % tabs.len(),
            "home" => 0,
            "end" => tabs.len() - 1,
            _ => return None,
        };
        tabs.get(next).copied()
    }
}

impl Fintwind {
    pub(in crate::app) fn render_task_capsule(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.selected_workspace_path().is_none() {
            return div().into_any_element();
        }
        let weak = cx.entity().downgrade();
        let toggle_weak = weak.clone();
        let previous_focus = Rc::new(RefCell::new(None::<FocusHandle>));
        let handle =
            self.menu_handle_with(BACKGROUND_SUMMARY_MENU_ID, cx, move |open, window, cx| {
                if open {
                    *previous_focus.borrow_mut() = window.focused(cx);
                    let tab = toggle_weak.update(cx, |this, cx| {
                        this.refresh_selected_branch_snapshot(cx);
                        this.refresh_capsule_tab(cx);
                        this.task_capsule_tab
                    });
                    if let Ok(tab) = tab {
                        focus_open_tab(toggle_weak.clone(), tab, window, cx);
                    }
                } else if let Some(focus) = previous_focus.borrow_mut().take() {
                    window.focus(&focus, cx);
                }
            });
        let expanded = handle.is_open();
        let theme = Theme::current(cx);
        // The changes tab widens into a diff stat while the worktree is dirty,
        // so the trigger has to be able to grow past its icon-only width.
        let trigger = div()
            .id("task-capsule-trigger")
            .min_w(px(108.0))
            .h(px(CAPSULE_TABS_HEIGHT + 2.0))
            .child(
                div()
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.raised)
                    .shadow_xs()
                    .when(expanded, |bar| bar.invisible())
                    .child(self.render_capsule_tabs(&handle, false, cx)),
            );
        popover_covering_trigger(trigger, &handle, move |handle, available, _, cx| {
            weak.update(cx, |this, cx| {
                this.render_capsule_card(handle, available, cx)
            })
            .unwrap_or_else(|_| div().into_any_element())
        })
    }

    fn render_capsule_tabs(
        &self,
        handle: &ContextMenuHandle,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let mut row = div()
            .h(px(CAPSULE_TABS_HEIGHT))
            .px(px(9.0))
            .flex()
            .items_center()
            .justify_end()
            .gap(px(2.0));
        let (processes, agents) = self
            .state
            .selected_session
            .map(|id| self.background_work_counts(id))
            .unwrap_or_default();
        // Read the same cached branch snapshot the card's changes row uses, so
        // the collapsed capsule and the open card never disagree. While the
        // card is open its own changes row already carries the totals, so only
        // the collapsed trigger widens.
        let changes = self
            .cached_branch_snapshot()
            .map(|snapshot| (snapshot.additions, snapshot.deletions))
            .unwrap_or_default();
        let todo_enabled = self.state.todo_enabled;
        let open_todos = if todo_enabled {
            self.capsule_todos()
                .map(|snapshot| snapshot.open)
                .unwrap_or_default()
        } else {
            0
        };
        for tab in CapsuleTab::visible_tabs(todo_enabled) {
            let selected = expanded && self.task_capsule_tab == tab;
            let diff = (!expanded && tab == CapsuleTab::Changes)
                .then_some(changes)
                .filter(|(additions, deletions)| *additions > 0 || *deletions > 0);
            let weak = cx.entity().downgrade();
            let click_weak = weak.clone();
            let click_handle = handle.clone();
            let key_handle = handle.clone();
            let focus = self.transcript_control_focus(
                format!(
                    "capsule-{}-{}",
                    if expanded { "open" } else { "closed" },
                    tab.id()
                ),
                cx,
            );
            let count = match tab {
                CapsuleTab::Todo => open_todos,
                CapsuleTab::Tasks => processes + agents,
                _ => 0,
            };
            let button = div()
                .id(SharedString::from(format!(
                    "capsule-tab-{}-{}",
                    expanded,
                    tab.id()
                )))
                .track_focus(&focus)
                .tab_index(0)
                .relative()
                .h(px(28.0))
                .when(diff.is_none(), |button| button.w(px(28.0)))
                .when_some(diff, |button, _| button.px(px(8.0)))
                .rounded(px(7.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.0))
                .cursor_default()
                .when(selected, |button| button.bg(theme.overlay_strong))
                .hover(|button| button.bg(theme.overlay))
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .tooltip(Tooltip::text(tab.label()))
                .child(icon(
                    tab.icon(),
                    16.0,
                    if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    },
                ))
                .when_some(diff, |button, (additions, deletions)| {
                    button.child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .text_size(ui_px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .when(additions > 0, |counts| {
                                counts.child(
                                    div()
                                        .text_color(theme.success_text)
                                        .child(format!("+{additions}")),
                                )
                            })
                            .when(deletions > 0, |counts| {
                                counts.child(
                                    div()
                                        .text_color(theme.danger_text)
                                        .child(format!("-{deletions}")),
                                )
                            }),
                    )
                })
                .when(count > 0, |button| {
                    button.child(
                        div()
                            .absolute()
                            .top(px(0.0))
                            .right(px(0.0))
                            .min_w(px(14.0))
                            .h(px(14.0))
                            .px(px(3.0))
                            .rounded_full()
                            .bg(theme.accent_fill)
                            .text_color(theme.on_accent)
                            .text_size(ui_px(9.0))
                            .line_height(px(14.0))
                            .text_center()
                            .child(count.to_string()),
                    )
                })
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    activate_tab(&click_weak, &click_handle, tab, true, window, cx);
                })
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        activate_tab(&weak, &key_handle, tab, true, window, cx);
                    } else if let Some(next) = tab.navigate(&event.keystroke.key, todo_enabled) {
                        activate_tab(&weak, &key_handle, next, false, window, cx);
                    } else {
                        return;
                    }
                    cx.stop_propagation();
                });
            row = row.child(button);
        }
        row
    }

    fn refresh_capsule_tab(&mut self, cx: &mut Context<Self>) {
        match self.task_capsule_tab {
            CapsuleTab::PullRequests => self.refresh_capsule_pull_requests(cx),
            CapsuleTab::Changes | CapsuleTab::Todo | CapsuleTab::Tasks => {}
        }
    }

    fn refresh_capsule_pull_requests(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        self.capsule_pull_requests.invalidate(&path);
        let Query::Missing(token) = self.capsule_pull_requests.read(&path) else {
            return;
        };
        let workspace = fintwind_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match workspace.request(
                        fintwind_client::WorkspaceOperation::InspectPullRequests { cwd: path },
                    ) {
                        Ok(fintwind_client::WorkspaceResult::PullRequests { snapshot }) => {
                            Ok(snapshot)
                        }
                        Ok(_) => Err("Invalid pull request response".to_owned()),
                        Err(error) => Err(error.to_string()),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.capsule_pull_requests.fulfill(token, result) {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn render_capsule_card(
        &self,
        handle: &ContextMenuHandle,
        available: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let tab = if self.task_capsule_tab.visible(self.state.todo_enabled) {
            self.task_capsule_tab
        } else {
            CapsuleTab::Changes
        };
        let body = match tab {
            CapsuleTab::Changes => self.render_capsule_changes(handle, cx),
            CapsuleTab::Todo => self.render_capsule_todo(cx),
            CapsuleTab::Tasks => self.render_capsule_tasks(handle, cx),
            CapsuleTab::PullRequests => self.render_capsule_pull_requests(handle, cx),
        };
        div()
            .id("task-capsule-card")
            .track_focus(handle.focus_handle())
            .tab_group()
            .tab_stop(false)
            .w(px(420.0).min(available.width))
            .max_h(available.height)
            .rounded(px(16.0))
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .bg(theme.raised)
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div()
                    .id(SharedString::from(format!(
                        "task-capsule-scroll-{}",
                        tab.id()
                    )))
                    .w_full()
                    .max_h(
                        px(440.0)
                            .min((available.height - px(CAPSULE_TABS_HEIGHT + 3.0)).max(px(0.0))),
                    )
                    .overflow_y_scroll()
                    .p(px(12.0))
                    .child(body),
            )
            .child(div().h(px(1.0)).flex_none().bg(theme.border))
            .child(self.render_capsule_tabs(handle, true, cx).flex_none())
            .into_any_element()
    }

    /// The cached branch snapshot for the selected workspace. Rendering reads
    /// this slot only; another workspace's snapshot never answers for it.
    fn cached_branch_snapshot(&self) -> Option<&BranchSnapshot> {
        let path = self.selected_workspace_path()?;
        self.visible_branch_snapshot
            .as_ref()
            .filter(|(cached, _)| cached == path)
            .map(|(_, snapshot)| snapshot)
    }

    fn render_capsule_changes(&self, handle: &ContextMenuHandle, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let snapshot = self.cached_branch_snapshot();
        let (additions, deletions) = snapshot
            .map(|s| (s.additions, s.deletions))
            .unwrap_or_default();
        let environment = EnvironmentSummary {
            branch: snapshot.and_then(|s| s.display_branch()).map(str::to_owned),
            additions,
            deletions,
            has_changes: additions > 0 || deletions > 0,
            commit_status: self.commit_operation_status_label(),
            changes_focus: self.transcript_control_focus("environment-summary-changes", cx),
            commit_focus: self.transcript_control_focus("environment-summary-commit", cx),
            compare_focus: self.transcript_control_focus("environment-summary-compare", cx),
            history_focus: self.transcript_control_focus("environment-summary-history", cx),
        };
        render_git_tools_section(environment, handle.clone(), cx.entity().downgrade(), &theme)
    }

    /// The selected session's newest task list, parsed once and shared by the
    /// collapsed badge and the open card. A cache miss is one transcript scan,
    /// never a scan per frame.
    fn capsule_todos(&self) -> Option<Rc<TodoSnapshot>> {
        let session = self.selected_session()?;
        let session_id = session.id;
        if let Some((cached_id, snapshot)) = self.capsule_todos.borrow().as_ref()
            && *cached_id == session_id
        {
            return snapshot.clone();
        }
        let snapshot = super::todo::latest_snapshot(session).map(Rc::new);
        self.capsule_todos
            .replace(Some((session_id, snapshot.clone())));
        snapshot
    }

    /// The plan activity changed or history was replaced: reparse the list on
    /// the next frame instead of showing a stale one.
    pub(in crate::app) fn invalidate_capsule_todos(&self) {
        self.capsule_todos.replace(None);
    }

    fn render_capsule_todo(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let Some(snapshot) = self.capsule_todos() else {
            return empty_todo_section(&theme);
        };
        if snapshot.items.is_empty() {
            return empty_todo_section(&theme);
        }
        render_todo_plan(&snapshot, &theme)
    }

    fn render_capsule_tasks(&self, handle: &ContextMenuHandle, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let session_id = self.state.selected_session.unwrap_or_else(Uuid::nil);
        let items = self
            .background_work
            .get(&session_id)
            .map(|r| r.ordered_items())
            .unwrap_or_default();
        let mut agents = Vec::new();
        let mut processes = Vec::new();
        for item in items {
            let provider_id = &item.key.provider_id;
            let kind = item.key.kind as u8;
            let entry = BackgroundSummaryEntry {
                key: item.key.clone(),
                title: item.title.clone(),
                status: item.status,
                can_stop: item.can_stop,
                row_focus: self.transcript_control_focus(
                    format!("background-summary-row-{provider_id}-{kind}"),
                    cx,
                ),
                stop_focus: self.transcript_control_focus(
                    format!("background-summary-stop-{provider_id}-{kind}"),
                    cx,
                ),
            };
            if item.key.kind == BackgroundWorkKind::Subagent {
                agents.push(entry);
            } else {
                processes.push(entry);
            }
        }
        let weak = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(render_background_summary_section(
                tr!("background.agents"),
                agents,
                session_id,
                handle.clone(),
                weak.clone(),
                &theme,
                tr!("background.no_agents"),
            ))
            .child(render_background_summary_section(
                tr!("background.title"),
                processes,
                session_id,
                handle.clone(),
                weak,
                &theme,
                tr!("background.no_work"),
            ))
    }

    fn render_capsule_pull_requests(
        &self,
        handle: &ContextMenuHandle,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let result = self
            .selected_workspace_path()
            .and_then(|path| self.capsule_pull_requests.peek(&path.to_path_buf()));
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(capsule_heading(tr!("capsule.pull_requests"), &theme));
        body = match result.as_deref() {
            None => body.child(capsule_message(tr!("capsule.loading"), &theme)),
            Some(Err(error)) => body.child(
                capsule_message(tr!("capsule.pr_failed"), &theme)
                    .id("capsule-pr-error")
                    .tooltip(Tooltip::text(error.clone())),
            ),
            Some(Ok(PullRequestSnapshot::Unavailable { reason })) => body.child(capsule_message(
                match reason {
                    PullRequestUnavailable::NotRepository => tr!("capsule.not_repository"),
                    PullRequestUnavailable::DetachedHead => tr!("capsule.detached_head"),
                    PullRequestUnavailable::NoGithubRemote => tr!("capsule.no_github_remote"),
                    PullRequestUnavailable::CliMissing => tr!("capsule.gh_missing"),
                    PullRequestUnavailable::AuthenticationRequired => {
                        tr!("capsule.gh_auth_required")
                    }
                },
                &theme,
            )),
            Some(Ok(PullRequestSnapshot::Ready {
                repository,
                branch,
                requests,
            })) => {
                let mut content = body
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon("icons/github.svg", 16.0, theme.text_secondary))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_px(13.0))
                                    .text_color(theme.text)
                                    .child(repository.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(icon("icons/git-branch.svg", 13.0, theme.text_tertiary))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_px(12.0))
                                    .text_color(theme.text_secondary)
                                    .child(branch.clone()),
                            ),
                    );
                if requests.is_empty() {
                    content =
                        content.child(capsule_message(tr!("capsule.no_pull_requests"), &theme));
                }
                for request in requests {
                    let focus =
                        self.transcript_control_focus(format!("capsule-pr-{}", request.number), cx);
                    content = content.child(render_pull_request_card(request, &focus, &theme));
                }
                content
            }
        };
        let weak = cx.entity().downgrade();
        let focus = self.transcript_control_focus("capsule-pr-refresh", cx);
        body.child(render_environment_action_row(
            "capsule-pr-refresh",
            &focus,
            "icons/rotate-cw.svg",
            tr!("common.refresh"),
            result.is_some(),
            false,
            None,
            &theme,
            move |_, cx| {
                let _ = weak.update(cx, |this, cx| this.refresh_capsule_pull_requests(cx));
            },
        ))
        .child(self.capsule_review_action(handle, cx))
    }

    fn capsule_review_action(
        &self,
        handle: &ContextMenuHandle,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let handle = handle.clone();
        let weak = cx.entity().downgrade();
        let focus = self.transcript_control_focus("capsule-review-branch", cx);
        render_environment_action_row(
            "capsule-review-branch",
            &focus,
            "icons/file-diff.svg",
            tr!("environment.compare_branch"),
            true,
            false,
            None,
            &theme,
            move |window, cx| {
                handle.close(window, cx);
                let _ = weak.update(cx, |this, cx| {
                    this.open_review_page(ReviewDiffSource::Branch, cx)
                });
            },
        )
    }
}

fn activate_tab(
    weak: &WeakEntity<Fintwind>,
    handle: &ContextMenuHandle,
    tab: CapsuleTab,
    toggle_same: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let open = handle.is_open();
    let toggle = weak.update(cx, |this, cx| {
        let same = this.task_capsule_tab == tab;
        this.task_capsule_tab = tab;
        if open && !same {
            this.refresh_capsule_tab(cx);
        }
        cx.notify();
        !open || (same && toggle_same)
    });
    if matches!(toggle, Ok(true)) {
        toggle_popover(handle, MenuAlign::AboveRight, window, cx);
    } else if open {
        focus_open_tab(weak.clone(), tab, window, cx);
    }
}

fn focus_open_tab(weak: WeakEntity<Fintwind>, tab: CapsuleTab, window: &mut Window, _: &mut App) {
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, cx| {
            let focus = weak
                .update(cx, |this, cx| {
                    this.menus
                        .borrow()
                        .get(BACKGROUND_SUMMARY_MENU_ID)
                        .filter(|handle| handle.is_open())
                        .filter(|_| this.task_capsule_tab == tab)
                        .map(|_| {
                            this.transcript_control_focus(format!("capsule-open-{}", tab.id()), cx)
                        })
                })
                .ok()
                .flatten();
            if let Some(focus) = focus {
                window.focus(&focus, cx);
            }
        });
    });
}

fn capsule_heading(text: String, theme: &Theme) -> Div {
    div()
        .px(px(8.0))
        .py(px(5.0))
        .text_size(ui_px(14.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .child(text)
}

fn capsule_message(text: String, theme: &Theme) -> Div {
    div()
        .px(px(8.0))
        .py(px(8.0))
        .text_size(ui_px(12.5))
        .text_color(theme.text_secondary)
        .child(text)
}

fn empty_todo_section(theme: &Theme) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .child(capsule_heading(tr!("capsule.todo"), theme))
        .child(capsule_message(tr!("capsule.todo_empty"), theme))
}

/// The agent's plan, laid out like Ely's `AgentPlan`: a title with how many
/// items are done, then one marked row per item. The count is composed here
/// so it follows the UI language.
fn render_todo_plan(snapshot: &TodoSnapshot, theme: &Theme) -> Div {
    let count = tabular(
        div()
            .flex_none()
            .text_size(ui_px(11.0))
            .text_color(theme.text_tertiary),
    )
    .child(format!("{}/{}", snapshot.completed, snapshot.items.len()));
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(
            div()
                .w_full()
                .px(px(8.0))
                .py(px(5.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(ui_px(14.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(tr!("capsule.todo")),
                )
                .child(count),
        )
        .children(snapshot.items.iter().enumerate().map(|(index, item)| {
            let text_color = match item.status {
                TodoStatus::Completed | TodoStatus::Cancelled => theme.text_tertiary,
                TodoStatus::InProgress => theme.text,
                TodoStatus::Pending => theme.text_secondary,
            };
            div()
                .w_full()
                .flex()
                .items_start()
                .gap(px(8.0))
                .px(px(8.0))
                .child(todo_mark(item.status, index, theme))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(ui_px(12.5))
                        .line_height(ui_px(17.0))
                        .text_color(text_color)
                        .when(item.status == TodoStatus::Completed, |text| {
                            text.line_through()
                        })
                        .child(item.content.clone()),
                )
        }))
}

fn todo_mark(status: TodoStatus, index: usize, theme: &Theme) -> AnyElement {
    let mark = match status {
        TodoStatus::Completed => {
            icon("icons/check.svg", 14.0, theme.success_text).into_any_element()
        }
        TodoStatus::InProgress => Spinner::new(("capsule-todo-mark", index))
            .size(IconSize::Sm)
            .color(theme.accent)
            .into_any_element(),
        TodoStatus::Cancelled => icon("icons/x.svg", 13.0, theme.text_tertiary).into_any_element(),
        TodoStatus::Pending => div()
            .size(px(6.0))
            .rounded_full()
            .bg(theme.text_tertiary)
            .into_any_element(),
    };
    div()
        .flex_none()
        .w(px(16.0))
        .h(px(17.0))
        .flex()
        .items_center()
        .justify_center()
        .child(mark)
        .into_any_element()
}

fn render_pull_request_card(
    request: &PullRequest,
    focus: &FocusHandle,
    theme: &Theme,
) -> Stateful<Div> {
    let (state, color) = match request.state {
        PullRequestState::Open => (tr!("capsule.pr_open"), theme.success_text),
        PullRequestState::Draft => (tr!("capsule.pr_draft"), theme.text_secondary),
        PullRequestState::Merged => (tr!("capsule.pr_merged"), theme.accent),
        PullRequestState::Closed => (tr!("capsule.pr_closed"), theme.danger_text),
    };
    let (check_icon, check_label, check_color) = match request.checks {
        PullRequestChecks::None => (
            "icons/ellipsis.svg",
            tr!("capsule.checks_none"),
            theme.text_tertiary,
        ),
        PullRequestChecks::Pending => (
            "icons/clock.svg",
            tr!("capsule.checks_pending"),
            theme.text_secondary,
        ),
        PullRequestChecks::Passed => (
            "icons/check.svg",
            tr!("capsule.checks_passed"),
            theme.success_text,
        ),
        PullRequestChecks::Failed => (
            "icons/x.svg",
            tr!("capsule.checks_failed"),
            theme.danger_text,
        ),
    };
    let url = request.url.clone();
    let key_url = url.clone();
    div()
        .id(SharedString::from(format!(
            "capsule-pr-card-{}",
            request.number
        )))
        .track_focus(focus)
        .tab_index(0)
        .rounded(px(12.0))
        .border_1()
        .border_color(theme.border)
        .p(px(12.0))
        .flex()
        .flex_col()
        .gap(px(10.0))
        .cursor_default()
        .hover(|style| style.bg(theme.overlay))
        .focus_visible(|style| style.border_color(theme.accent))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .rounded_full()
                        .px(px(8.0))
                        .py(px(3.0))
                        .bg(theme.raised.blend(color.opacity(0.12)))
                        .text_size(ui_px(11.5))
                        .text_color(color)
                        .child(state),
                )
                .child(
                    div()
                        .text_size(ui_px(12.0))
                        .text_color(theme.text_secondary)
                        .child(format!("#{}", request.number)),
                )
                .child(div().flex_1())
                .when_some(
                    DateTime::parse_from_rfc3339(&request.updated_at).ok(),
                    |row, updated| {
                        row.child(
                            div()
                                .text_size(ui_px(11.0))
                                .text_color(theme.text_tertiary)
                                .child(updated.format("%Y-%m-%d").to_string()),
                        )
                    },
                )
                .child(icon("icons/arrow-up-right.svg", 14.0, theme.text_tertiary)),
        )
        .child(
            div()
                .text_size(ui_px(14.0))
                .line_height(ui_px(20.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(request.title.clone()),
        )
        .child(
            div()
                .text_size(ui_px(11.5))
                .text_color(theme.text_secondary)
                .child(format!("{} → {}", request.head_branch, request.base_branch)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(px(8.0))
                .text_size(ui_px(11.5))
                .child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(request.author.clone()),
                )
                .child(div().text_color(theme.text_secondary).child(format!(
                    "{} {}",
                    request.comments,
                    tr!("capsule.comments")
                )))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .text_color(check_color)
                        .child(icon(check_icon, 13.0, check_color))
                        .child(check_label),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_color(theme.success_text)
                        .child(format!("+{}", request.additions)),
                )
                .child(
                    div()
                        .text_color(theme.danger_text)
                        .child(format!("-{}", request.deletions)),
                )
                .child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(tr!("capsule.file_count", count = request.changed_files)),
                ),
        )
        .on_click(move |_, _, cx| cx.open_url(&url))
        .on_key_down(move |event: &KeyDownEvent, _, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                cx.open_url(&key_url);
                cx.stop_propagation();
            }
        })
}

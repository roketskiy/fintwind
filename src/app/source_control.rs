use crate::theme::{code_px, ui_px};

use gpui::{Context, Div, IntoElement, Window, div, prelude::*, px};

use super::*;
use super::right_panel::file_icon_for_path;
use super::sidebar::localized_session_title;

/// The source-control mode: the standalone review page that used to live in
/// the right panel, plus its second column of workspace context. The mode is
/// reached from the first column's rail, or from a turn's "review changes"
/// entry, which loads that turn's historical diff as a read-only snapshot.
///
/// The diff state itself (`right_panel_diff_*` fields on [`Fintwind`]) is
/// app-level, not tied to a panel: one source, one snapshot, one selection.
/// The rendering below only ever reads that in-memory snapshot — Git, patch
/// parsing, and tokenization happen in [`Fintwind::refresh_review_diff`].
impl Fintwind {
    /// Enter the source-control mode from the rail. Whatever source and file
    /// selection the user last looked at stays; only a data refresh is
    /// requested, so returning picks the review up where it left off.
    pub(super) fn open_source_control(&mut self, cx: &mut Context<Self>) {
        self.mode = WorkspaceMode::SourceControl;
        self.refresh_source_control_status(cx);
        self.refresh_review_diff(cx);
        cx.notify();
    }

    /// Show `source` in the source-control mode. Every entry point funnels
    /// here: the toolbar's source menu, the turn review entry, and the
    /// workspace actions elsewhere in the app.
    pub(super) fn open_review_page(
        &mut self,
        source: ReviewDiffSource,
        cx: &mut Context<Self>,
    ) {
        if self.right_panel_diff_source != source {
            self.reset_review_diff_state(source);
        }
        self.mode = WorkspaceMode::SourceControl;
        if !self.sidebar_visible {
            self.set_sidebar_visible(true, cx);
        }
        self.refresh_source_control_status(cx);
        self.refresh_review_diff(cx);
        cx.notify();
    }

    /// Whether the review page is currently showing a checkpoint-based
    /// historical snapshot. Such a view is read-only: staging, discarding,
    /// and commit actions must stay disabled against it, and the banner says
    /// so explicitly.
    pub(super) fn review_is_history_snapshot(&self) -> bool {
        matches!(
            self.right_panel_diff_source,
            ReviewDiffSource::LastTurn { .. }
        )
    }

    /// Whether the review page tracks the selected session's workspace. A
    /// pinned turn snapshot belongs to its own session; session switches and
    /// workspace invalidations must not overwrite it with live data.
    pub(super) fn review_follows_selected_session(&self) -> bool {
        !self.review_is_history_snapshot()
    }

    /// Clear everything the review page draws and aim it at `source`. The
    /// scroll states reset too, so a new source starts at its top.
    fn reset_review_diff_state(&mut self, source: ReviewDiffSource) {
        self.right_panel_diff_selection.clear();
        self.right_panel_diff_source = source;
        self.right_panel_diff_snapshot = None;
        self.right_panel_diff_error = None;
        self.right_panel_diff_selected_file = None;
        *self.source_control_diff_rows.borrow_mut() = Vec::new();
        self.right_panel_diff_list_state.reset(0);
    }

    /// The transcript's "review this turn's changes" entry. The turn's diff
    /// comes from its checkpoint refs — an immutable snapshot of the
    /// workspace pair — so the review page renders it read-only and says so.
    pub(super) fn open_turn_diff(&mut self, turn_id: Uuid, cx: &mut Context<Self>) {
        let Some((session_id, turn_count)) = self.selected_session().and_then(|session| {
            session
                .turns
                .iter()
                .find(|turn| turn.id == turn_id)
                .map(|turn| (session.id, turn.turn_count))
        }) else {
            return;
        };
        self.open_review_page(
            ReviewDiffSource::LastTurn {
                session_id,
                turn_id,
                turn_count,
            },
            cx,
        );
    }

    /// The workspace the source-control mode is pointed at: the user's
    /// manual choice when one is pinned, otherwise the selected session's
    /// actual directory — including its worktree.
    pub(super) fn review_workspace_path(&self) -> Option<&std::path::Path> {
        self.review_workspace_override
            .as_deref()
            .or_else(|| self.selected_workspace_path())
    }

    /// Fetches the pinned-or-followed workspace's live status in the
    /// background. One generation-guarded snapshot feeds the branch line and
    /// the staged/unstaged groups; a stale response never lands.
    pub(super) fn refresh_source_control_status(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            self.source_control_status = None;
            self.source_control_status_loading = false;
            self.source_control_status_error = None;
            self.sync_source_control_rows(cx);
            cx.notify();
            return;
        };

        self.source_control_status_generation =
            self.source_control_status_generation.wrapping_add(1);
        let generation = self.source_control_status_generation;
        self.source_control_status_loading = true;
        self.source_control_status_error = None;
        cx.notify();

        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |fintwind, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let client = client.clone();
                    let workspace = workspace.clone();
                    async move {
                        client.request(fintwind_client::WorkspaceOperation::InspectStatus {
                            cwd: workspace,
                        })
                    }
                })
                .await;
            fintwind
                .update(cx, |fintwind, cx| {
                    let still_current = fintwind.source_control_status_generation == generation
                        && fintwind
                            .review_workspace_path()
                            .is_some_and(|path| path == workspace);
                    if !still_current {
                        return;
                    }

                    fintwind.source_control_status_loading = false;
                    match result {
                        Ok(fintwind_client::WorkspaceResult::WorktreeStatus { status }) => {
                            fintwind.source_control_status = status.map(Arc::new);
                            fintwind.source_control_status_error = None;
                        }
                        Ok(_) => {
                            fintwind.source_control_status_error =
                                Some(tr!("source_control.status_failed"));
                        }
                        Err(error) => {
                            fintwind.source_control_status_error = Some(error.to_string());
                        }
                    }
                    fintwind.sync_source_control_rows(cx);
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Pins the source-control mode to a recorded directory — a project root
    /// or a session worktree — picked from the workspace menu. The pin
    /// survives session switches until "follow session" is used, and dies
    /// with the app run so a stale path can never come back.
    pub(super) fn pin_source_control_workspace(
        &mut self,
        target: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.review_workspace_override == target {
            return;
        }
        self.review_workspace_override = target;
        self.review_pending_file_focus = None;
        // A pinned turn snapshot belongs to its session's repository; once
        // the user repoints the workspace, the live view is the honest thing
        // to show.
        if self.review_is_history_snapshot() {
            self.reset_review_diff_state(ReviewDiffSource::Uncommitted);
        }
        self.refresh_source_control_status(cx);
        self.refresh_review_diff(cx);
        cx.notify();
    }

    /// The directories the workspace menu offers: the selected session's
    /// actual directory first (the follow target), then every recorded
    /// project root, then every worktree a session has materialized.
    fn source_control_workspace_targets(&self) -> Vec<(Option<PathBuf>, String)> {
        let mut targets = vec![(None, tr!("source_control.follow_session"))];
        for project in &self.state.projects {
            if project.is_projectless() {
                continue;
            }
            targets.push((Some(project.path.clone()), project.display_name()));
        }
        let mut seen: Vec<PathBuf> = self
            .state
            .projects
            .iter()
            .filter(|project| !project.is_projectless())
            .map(|project| project.path.clone())
            .collect();
        for session in &self.state.sessions {
            let fintwind_protocol::model::SessionWorkspace::Worktree { path, branch } =
                &session.workspace
            else {
                continue;
            };
            if seen.iter().any(|seen| seen == path) {
                continue;
            }
            seen.push(path.clone());
            let leaf = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            targets.push((Some(path.clone()), format!("{branch} · {leaf}")));
        }
        targets
    }

    /// Drops the manual pin so the mode follows the selected session's
    /// workspace again.
    pub(super) fn follow_source_control_session(&mut self, cx: &mut Context<Self>) {
        if self.review_workspace_override.take().is_none() {
            return;
        }
        self.review_pending_file_focus = None;
        self.refresh_source_control_status(cx);
        self.refresh_review_diff(cx);
        cx.notify();
    }

    /// Flattens the cached status into the virtualized list's rows: one
    /// header per non-empty group, then that group's files. The filter
    /// input narrows both groups by path. Called whenever the status
    /// snapshot or the filter changes.
    pub(super) fn sync_source_control_rows(&mut self, cx: &App) {
        let filter = self.right_panel_diff_filter.read(cx).content().to_owned();
        let filter = filter.trim().to_ascii_lowercase();
        let matches = |entry: &fintwind_client::git::WorktreeStatusEntry| {
            filter.is_empty()
                || entry.path.to_ascii_lowercase().contains(&filter)
                || entry
                    .origin_path
                    .as_deref()
                    .is_some_and(|origin| origin.to_ascii_lowercase().contains(&filter))
        };
        let mut rows = Vec::new();
        if let Some(status) = self.source_control_status.as_ref() {
            let staged: Vec<&fintwind_client::git::WorktreeStatusEntry> = status
                .entries
                .iter()
                .filter(|entry| source_control_entry_is_staged(entry) && matches(entry))
                .collect();
            if !staged.is_empty() {
                rows.push(SourceControlRow::Header {
                    staged: true,
                    count: staged.len(),
                });
                rows.extend(
                    staged
                        .iter()
                        .map(|entry| SourceControlRow::file(true, entry)),
                );
            }
            let unstaged: Vec<&fintwind_client::git::WorktreeStatusEntry> = status
                .entries
                .iter()
                .filter(|entry| source_control_entry_is_unstaged(entry) && matches(entry))
                .collect();
            if !unstaged.is_empty() {
                rows.push(SourceControlRow::Header {
                    staged: false,
                    count: unstaged.len(),
                });
                rows.extend(
                    unstaged
                        .iter()
                        .map(|entry| SourceControlRow::file(false, entry)),
                );
            }
        }
        let row_count = rows.len();
        *self.source_control_rows.borrow_mut() = rows;
        self.source_control_list_state
            .reset_with_uniform_height(row_count, px(28.0));
    }

    /// Opens a file from one of the second column's groups. Clicking a file
    /// under the staged group shows the staged diff, under the unstaged
    /// group the worktree diff — switching the source if needed, then
    /// landing on that file once its snapshot arrives.
    pub(super) fn open_source_control_file(
        &mut self,
        staged: bool,
        path: String,
        cx: &mut Context<Self>,
    ) {
        let source = if staged {
            ReviewDiffSource::Staged
        } else {
            ReviewDiffSource::Unstaged
        };
        if self.right_panel_diff_source == source
            && let Some(snapshot) = self.right_panel_diff_snapshot.as_ref()
            && let Some(index) = snapshot.files.iter().position(|file| file.path == path)
        {
            self.right_panel_diff_selected_file = Some(index);
            self.review_pending_file_focus = None;
            self.sync_source_control_diff_rows();
            cx.notify();
            return;
        }
        self.review_pending_file_focus = Some(path);
        self.set_review_diff_source(source, cx);
    }

    /// Rebuilds the single-file diff rows from the cached snapshot, the
    /// selected file, and the layout. Called whenever any of the three
    /// changes; render reads only the flattened result.
    pub(super) fn sync_source_control_diff_rows(&mut self) {
        let rows = self
            .right_panel_diff_snapshot
            .as_ref()
            .zip(self.right_panel_diff_selected_file)
            .map(|(snapshot, file_index)| {
                let range = source_control_file_line_range(snapshot, file_index);
                if self.source_control_diff_split {
                    source_control_split_rows(snapshot, range)
                } else {
                    (range.start..range.end)
                        .map(SourceControlDiffRow::Line)
                        .collect()
                }
            })
            .unwrap_or_default();
        let row_count = rows.len();
        *self.source_control_diff_rows.borrow_mut() = rows;
        self.right_panel_diff_list_state.reset(row_count);
    }

    /// Flips the diff reader between unified rows and side-by-side pairs.
    pub(super) fn toggle_source_control_diff_layout(&mut self, cx: &mut Context<Self>) {
        self.source_control_diff_split = !self.source_control_diff_split;
        self.sync_source_control_diff_rows();
        cx.notify();
    }

    /// Moves the reader to the previous or next changed file, clamped to the
    /// snapshot's files. The list resets to the top, as a file switch should.
    pub(super) fn step_source_control_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(snapshot) = self.right_panel_diff_snapshot.as_ref() else {
            return;
        };
        let count = snapshot.files.len();
        if count == 0 {
            return;
        }
        let current = self.right_panel_diff_selected_file.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, count as isize - 1) as usize;
        if Some(next) == self.right_panel_diff_selected_file {
            return;
        }
        self.right_panel_diff_selected_file = Some(next);
        self.sync_source_control_diff_rows();
        cx.notify();
    }

    /// The mode's second column: the workspace the review is pointed at, and
    /// its staged/unstaged file groups. Showing the project, the concrete
    /// directory (a session worktree can differ from the project root), and
    /// the branch guards against running Git operations in the wrong place.
    pub(super) fn render_source_control_secondary(
        &mut self,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        div()
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .child(self.render_source_control_workspace_block(cx))
            .child(self.render_source_control_file_list(cx))
    }

    /// The workspace block: project, directory, branch, and the workspace
    /// picker that doubles as the follow/pin indicator.
    fn render_source_control_workspace_block(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let workspace = self.review_workspace_path().map(Path::to_path_buf);
        let status = self.source_control_status.as_ref();
        let project_name = self.source_control_project_name();
        let branch_label = match status.map(|status| status.branch.as_deref()) {
            Some(Some(branch)) => branch.to_owned(),
            Some(None) => tr!("source_control.detached_head"),
            None => String::new(),
        };

        div()
            .px(px(12.0))
            .pt(px(12.0))
            .pb(px(10.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .border_b_1()
            .border_color(theme.sidebar_border)
            // Title row with the manual refresh control.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(ui_px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_tertiary)
                            .child(tr!("source_control.workspace")),
                    )
                    .child(
                        div()
                            .id("source-control-refresh")
                            .tab_index(0)
                            .w(px(22.0))
                            .h(px(22.0))
                            .flex_none()
                            .rounded(px(6.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_default()
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.bg(theme.overlay_strong))
                            .focus_visible(|element| {
                                element.border_1().border_color(theme.accent_focus)
                            })
                            .tooltip(Tooltip::text(tr!("source_control.refresh")))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.refresh_source_control_status(cx);
                                this.refresh_review_diff(cx);
                            }))
                            .child(icon("icons/rotate-cw.svg", 13.0, theme.text_tertiary)),
                    ),
            )
            // Loading, error, and "not a repository" states keep the block
            // honest instead of showing a stale branch over a dead path.
            .when_some(self.source_control_status_error.clone(), |column, error| {
                column.child(
                    div()
                        .text_size(ui_px(11.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .when(self.source_control_status_loading, |column| {
                column.child(
                    div()
                        .text_size(ui_px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(tr!("source_control.loading")),
                )
            })
            .child(
                div()
                    .text_size(ui_px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(project_name),
            )
            .when_some(workspace.as_deref(), |column, path| {
                column.child(
                    div()
                        .id("source-control-workspace-path")
                        .text_size(ui_px(11.0))
                        .text_color(theme.text_tertiary)
                        .tooltip(Tooltip::text(path.to_string_lossy().to_string()))
                        .child(compact_path(path)),
                )
            })
            .when(status.is_some(), |column| {
                column.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .child(icon("icons/git-branch.svg", 12.0, theme.text_tertiary))
                        .child(
                            div()
                                .text_size(ui_px(11.5))
                                .text_color(theme.text_secondary)
                                .child(branch_label),
                        ),
                )
            })
            // The workspace picker doubles as the follow/pin indicator: its
            // label states the mode, so no separate status row is needed.
            .child(self.render_source_control_workspace_menu(cx))
    }

    /// The workspace picker: a menu of Fintwind's recorded directories —
    /// the followed session first, then project roots and session worktrees.
    fn render_source_control_workspace_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let handle = self.menu_handle("source-control-workspace", cx);
        let current = self
            .review_workspace_path()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| tr!("source_control.no_directory"));
        let pinned = self.review_workspace_override.is_some();
        let targets = self.source_control_workspace_targets();
        let pinned_path = self.review_workspace_override.clone();
        let weak = cx.entity().downgrade();
        dropdown_menu(
            MenuChip::new("source-control-workspace")
                .icon(
                    "icons/folder.svg",
                    if pinned {
                        theme.warning_text
                    } else {
                        theme.text_tertiary
                    },
                )
                .label(format!(
                    "{} · {}",
                    if pinned {
                        tr!("source_control.pinned")
                    } else {
                        tr!("source_control.follow_session")
                    },
                    current
                ))
                .max_label_width(160.0)
                .height(px(24.0))
                .background(theme.sidebar)
                .selected(handle.is_open()),
            "source-control-workspace-menu",
            &handle,
            MenuAlign::BelowLeft,
            move |_| {
                targets
                    .iter()
                    .map(|(target, label)| {
                        let (target, weak) = (target.clone(), weak.clone());
                        let selected = match target.as_ref() {
                            None => !pinned,
                            Some(path) => pinned_path.as_deref() == Some(path.as_path()),
                        };
                        MenuItem::new(label.clone(), move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.pin_source_control_workspace(target.clone(), cx)
                            });
                        })
                        .selected(selected)
                    })
                    .collect::<Vec<_>>()
            },
        )
    }

    /// The staged/unstaged file groups, or one of the honest empty states:
    /// no workspace, unreadable status, not a repository, or a clean tree.
    fn render_source_control_file_list(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let entity = cx.entity().downgrade();
        let has_workspace = self.review_workspace_path().is_some();
        let status = self.source_control_status.as_ref();
        let placeholder = if !has_workspace {
            Some(tr!("source_control.no_directory"))
        } else if self.source_control_status_error.is_some() {
            None
        } else if self.source_control_status_loading && status.is_none() {
            Some(tr!("source_control.loading"))
        } else if let Some(status) = status {
            if status.entries.is_empty() {
                Some(tr!("source_control.clean"))
            } else {
                None
            }
        } else {
            Some(tr!("source_control.not_a_repository"))
        };
        let row_count = self.source_control_rows.borrow().len();

        div()
            .flex_1()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            // The path filter narrows both groups. It reuses the old diff
            // tree's filter input, which the standalone page no longer has.
            .when(has_workspace && status.is_some(), |column| {
                column.child(
                    div().px(px(10.0)).pb(px(6.0)).child(
                        TextField::new(
                            "source-control-filter",
                            self.right_panel_diff_filter.clone(),
                        )
                        .icon("icons/search.svg", 12.0)
                        .w_full(),
                    ),
                )
            })
            .children(placeholder.map(|label| {
                div()
                    .px(px(12.0))
                    .pt(px(10.0))
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_tertiary)
                    .child(label)
                    .into_any_element()
            }))
            .when(row_count > 0, |column| {
                column.child(
                    div()
                        .id("source-control-file-list")
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(
                            list(
                                self.source_control_list_state.clone(),
                                move |index, _window, cx| {
                                    entity
                                        .upgrade()
                                        .map(|entity| {
                                            entity.update(cx, |this, cx| {
                                                this.render_source_control_row(index, cx)
                                            })
                                        })
                                        .unwrap_or_else(|| div().into_any_element())
                                },
                            )
                            .size_full()
                            .py(px(4.0)),
                        )
                        .child(scrollbar::vertical(
                            &self.source_control_list_state,
                            &self.source_control_scrollbar,
                        )),
                )
            })
    }

    /// One flattened row: a group header, or a clickable file row with its
    /// colored status letter.
    fn render_source_control_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(row) = self.source_control_rows.borrow().get(index).cloned() else {
            return div().into_any_element();
        };
        match row {
            SourceControlRow::Header { staged, count } => div()
                .h(px(28.0))
                .flex_none()
                .px(px(12.0))
                .flex()
                .items_center()
                .text_size(ui_px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_tertiary)
                .child(if staged {
                    tr!("source_control.staged", count = count)
                } else {
                    tr!("source_control.unstaged", count = count)
                })
                .into_any_element(),
            SourceControlRow::File {
                staged,
                path,
                title,
                letter,
            } => {
                let element_id = SharedString::from(format!(
                    "source-control-file-{}-{path}",
                    if staged { "staged" } else { "unstaged" }
                ));
                let title_id =
                    SharedString::from(format!("source-control-file-title-{staged}-{path}"));
                let row_path = path.clone();
                div()
                    .id(element_id)
                    .tab_index(0)
                    .h(px(28.0))
                    .flex_none()
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .focus_visible(|element| element.border_1().border_color(theme.accent_focus))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_source_control_file(staged, row_path.clone(), cx);
                    }))
                    .child(
                        div()
                            .w(px(12.0))
                            .flex_none()
                            .text_size(ui_px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(status_letter_color(letter, &theme))
                            .child(letter.to_string()),
                    )
                    .child(
                        div()
                            .id(title_id)
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(12.0))
                            .text_color(theme.text_secondary)
                            .tooltip(Tooltip::text(title.clone()))
                            .child(title),
                    )
                    .into_any_element()
            }
        }
    }

    /// The name above the directory: the pinned workspace's project when one
    /// matches, its folder name otherwise, or the followed session's project.
    fn source_control_project_name(&self) -> String {
        if let Some(override_path) = self.review_workspace_override.as_deref() {
            if let Some(project) = self
                .state
                .projects
                .iter()
                .find(|project| project.path == override_path)
            {
                if project.is_projectless() {
                    return tr!("project.without_a_project");
                }
                return project.display_name().to_string();
            }
            return override_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| tr!("sidebar.unknown_project"));
        }
        self.selected_project()
            .map(|project| {
                if project.is_projectless() {
                    tr!("project.without_a_project")
                } else {
                    project.display_name().to_string()
                }
            })
            .unwrap_or_else(|| tr!("sidebar.unknown_project"))
    }

    /// The mode's main area: the read-only history banner when a turn diff
    /// is loaded, then the diff reader. The unified top bar owns the window
    /// chrome and the drag region, so the page starts directly with content.
    pub(super) fn render_source_control_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let banner = self.render_review_history_banner(cx);

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_l_1()
            .border_color(theme.sidebar_border)
            .children(banner)
            .child(
                div().flex_1().min_h_0().flex().child(
                    div()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(self.render_review_diff_body(
                            self.chat_viewport_width(window),
                            window,
                            cx,
                        )),
                ),
            )
    }

    /// The read-only banner for a checkpoint-based turn diff: where the
    /// snapshot came from, that it cannot be mutated, and the two ways out —
    /// the live workspace, or back to the session.
    fn render_review_history_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ReviewDiffSource::LastTurn {
            session_id,
            turn_count,
            ..
        } = self.right_panel_diff_source
        else {
            return None;
        };
        let theme = Theme::current(cx);
        let session_title = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(localized_session_title);
        let turn_label = if turn_count > 0 {
            tr!("review.turn_label", turn = turn_count)
        } else {
            tr!("diff.source_last_turn")
        };

        let focus = self.transcript_control_focus("review-banner-back", cx);
        let back = div()
            .id("review-banner-back")
            .track_focus(&focus)
            .tab_index(0)
            .h(px(24.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_default()
            .text_size(ui_px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.text_secondary)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay).text_color(theme.text))
            .active(|style| style.bg(theme.overlay_strong))
            .child(icon("icons/arrow-left.svg", 11.0, theme.text_tertiary))
            .child(tr!("review.back_to_session"))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.return_to_sessions(window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.return_to_sessions(window, cx);
                    cx.stop_propagation();
                }
            }));

        let focus = self.transcript_control_focus("review-banner-live", cx);
        let live = div()
            .id("review-banner-live")
            .track_focus(&focus)
            .tab_index(0)
            .h(px(24.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_default()
            .text_size(ui_px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.text_secondary)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay).text_color(theme.text))
            .active(|style| style.bg(theme.overlay_strong))
            .child(icon("icons/rotate-cw.svg", 11.0, theme.text_tertiary))
            .child(tr!("review.view_workspace"))
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.set_review_diff_source(ReviewDiffSource::Uncommitted, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.set_review_diff_source(ReviewDiffSource::Uncommitted, cx);
                    cx.stop_propagation();
                }
            }));

        Some(
            div()
                .flex_none()
                .px(px(12.0))
                .py(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.overlay)
                .child(icon("icons/history.svg", 13.0, theme.text_tertiary))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(ui_px(11.5))
                        .text_color(theme.text_secondary)
                        .child(match session_title {
                            Some(title) => tr!(
                                "review.snapshot_from_session",
                                title = title,
                                turn = turn_label
                            ),
                            None => tr!("review.snapshot", turn = turn_label),
                        }),
                )
                .child(
                    div()
                        .h(px(16.0))
                        .px(px(5.0))
                        .flex_none()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(theme.border_strong)
                        .flex()
                        .items_center()
                        .text_size(ui_px(9.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text_tertiary)
                        .child(tr!("review.readonly")),
                )
                .child(div().flex_1())
                .child(live)
                .child(back)
                .into_any_element(),
        )
    }

    /// The review page's diff reader: the source toolbar above the snapshot
    /// (or its status), with the changed-file tree beside the diff.
    fn render_review_diff_body(
        &mut self,
        _page_width: f32,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let navbar = self.render_review_diff_toolbar(cx);
        let content = match self.right_panel_diff_snapshot.clone() {
            Some(snapshot) => div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .child(self.render_source_control_file_header(snapshot.clone(), cx))
                .child(self.render_review_diff(snapshot, cx))
                .into_any_element(),
            None if self.right_panel_diff_loading => self
                .render_right_panel_empty_message(
                    tr!("diff.loading"),
                    tr!("diff.loading_description"),
                    cx,
                )
                .into_any_element(),
            None if self.right_panel_diff_error.is_some() => self
                .render_right_panel_empty_message(
                    tr!("diff.unavailable"),
                    self.right_panel_diff_error.clone().unwrap_or_default(),
                    cx,
                )
                .into_any_element(),
            None => self
                .render_right_panel_empty_message(
                    tr!("diff.no_changes"),
                    tr!("diff.no_changes_description"),
                    cx,
                )
                .into_any_element(),
        };

        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .relative()
            .flex()
            .flex_col()
            .child(md::render::frame_reset(
                self.right_panel_diff_selection.clone(),
            ))
            .child(navbar)
            .child(content)
            .child(self.review_diff_selection_input())
    }

    /// The fixed header above the reader: the current file's folder-dimmed
    /// path, its status letter, its diff stat, and the layout toggle.
    fn render_source_control_file_header(
        &mut self,
        snapshot: Arc<ReviewDiffSnapshot>,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let file = self
            .right_panel_diff_selected_file
            .and_then(|index| snapshot.files.get(index));
        let (path, additions, deletions, letter) = match file {
            Some(file) => {
                let letter = match file.status {
                    crate::review_diff::FileStatus::Added => 'A',
                    crate::review_diff::FileStatus::Deleted => 'D',
                    crate::review_diff::FileStatus::Binary => 'B',
                    crate::review_diff::FileStatus::Modified => 'M',
                };
                (file.path.clone(), file.additions, file.deletions, letter)
            }
            None => (String::new(), 0, 0, ' '),
        };
        let (folder, name) = match path.rsplit_once('/') {
            Some((folder, name)) => (Some(folder.to_owned()), name.to_owned()),
            None => (None, path.clone()),
        };

        div()
            .h(px(36.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .child(file_icon(file_icon_for_path(&path), 14.0))
            .when_some(folder, |header, folder| {
                header.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(ui_px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(format!("{folder}/")),
                )
            })
            .child(
                div()
                    .id("source-control-file-header-path")
                    .min_w_0()
                    .truncate()
                    .text_size(ui_px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_secondary)
                    .tooltip(Tooltip::text(path))
                    .child(name),
            )
            .when(letter != ' ', |header| {
                header.child(
                    div()
                        .flex_none()
                        .text_size(ui_px(10.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(status_letter_color(letter, &theme))
                        .child(letter.to_string()),
                )
            })
            .child(render_diff_stat(additions, deletions, &theme))
            .child(div().flex_1())
            .child(self.render_source_control_layout_toggle(cx))
    }

    /// The unified/side-by-side switch, drawn as two small adjoining
    /// buttons with the active one filled.
    fn render_source_control_layout_toggle(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let split = self.source_control_diff_split;
        let segment = |id: &'static str, label: String, active: bool| {
            div()
                .id(id)
                .tab_index(0)
                .h(px(22.0))
                .px(px(8.0))
                .flex_none()
                .flex()
                .items_center()
                .cursor_default()
                .text_size(ui_px(10.5))
                .font_weight(FontWeight::MEDIUM)
                .when(active, |segment| {
                    segment
                        .rounded(px(5.0))
                        .bg(theme.overlay_strong)
                        .text_color(theme.text)
                })
                .when(!active, |segment| {
                    segment
                        .rounded(px(5.0))
                        .text_color(theme.text_tertiary)
                        .hover(|segment| segment.bg(theme.overlay).text_color(theme.text_secondary))
                })
                .focus_visible(|segment| segment.border_1().border_color(theme.accent_focus))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if this.source_control_diff_split != split {
                        return;
                    }
                    this.toggle_source_control_diff_layout(cx);
                }))
                .child(label)
        };
        div()
            .flex_none()
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .p(px(1.0))
            .flex()
            .items_center()
            .gap(px(1.0))
            .child(segment(
                "source-control-layout-unified",
                tr!("diff.view_unified"),
                !split,
            ))
            .child(segment(
                "source-control-layout-split",
                tr!("diff.view_split"),
                split,
            ))
    }

    fn render_review_diff_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let selected = self.right_panel_diff_source;
        let latest_turn = self.latest_review_turn_source();
        let source_label = self.review_diff_source_label(selected);
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("review-diff-source", cx);
        let source = dropdown_menu(
            MenuChip::new("review-diff-source")
                .label(source_label)
                .height(px(28.0))
                .background(theme.surface)
                .selected(handle.is_open()),
            "review-diff-source-menu",
            &handle,
            MenuAlign::BelowLeft,
            move |_| {
                let mut items = Vec::new();
                let last_turn_source = latest_turn.unwrap_or_default();
                let last_turn_weak = weak.clone();
                items.push(
                    MenuItem::new(tr!("diff.source_last_turn"), move |_, cx| {
                        let _ = last_turn_weak.update(cx, |this, cx| {
                            this.set_review_diff_source(last_turn_source, cx)
                        });
                    })
                    .selected(latest_turn == Some(selected))
                    .disabled(latest_turn.is_none()),
                );
                items.push(MenuItem::Separator);
                for (choice, label) in [
                    (
                        ReviewDiffSource::Uncommitted,
                        tr!("diff.source_uncommitted"),
                    ),
                    (ReviewDiffSource::Unstaged, tr!("diff.source_unstaged")),
                    (ReviewDiffSource::Staged, tr!("diff.source_staged")),
                ] {
                    let choice_weak = weak.clone();
                    items.push(
                        MenuItem::new(label, move |_, cx| {
                            let _ = choice_weak.update(cx, |this, cx| {
                                this.set_review_diff_source(choice, cx)
                            });
                        })
                        .selected(choice == selected),
                    );
                }
                items.push(MenuItem::Separator);
                for (choice, label) in [
                    (ReviewDiffSource::Committed, tr!("diff.source_committed")),
                    (ReviewDiffSource::Branch, tr!("diff.source_branch")),
                ] {
                    let choice_weak = weak.clone();
                    items.push(
                        MenuItem::new(label, move |_, cx| {
                            let _ = choice_weak.update(cx, |this, cx| {
                                this.set_review_diff_source(choice, cx)
                            });
                        })
                        .selected(choice == selected),
                    );
                }
                items
            },
        );

        let (additions, deletions, truncated) = self
            .right_panel_diff_snapshot
            .as_ref()
            .map_or((0, 0, false), |snapshot| {
                (snapshot.additions, snapshot.deletions, snapshot.truncated)
            });
        let refresh_focus = self.transcript_control_focus("review-diff-refresh", cx);
        let refresh_icon: AnyElement = if self.right_panel_diff_loading {
            motion::spin(icon("icons/loader-circle.svg", 12.0, theme.text_tertiary))
        } else {
            icon("icons/rotate-cw.svg", 12.0, theme.text_tertiary).into_any_element()
        };
        let refresh = div()
            .id("review-diff-refresh")
            .track_focus(&refresh_focus)
            .tab_index(0)
            .size(px(28.0))
            .rounded(px(7.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay))
            .active(|style| style.bg(theme.overlay_strong))
            .child(refresh_icon)
            .tooltip(|window, cx| Tooltip::new(tr!("diff.refresh")).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.refresh_review_diff(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.refresh_review_diff(cx);
                    cx.stop_propagation();
                }
            }));

        // File navigation: previous / next, and the position readout. Each
        // side disables at the end of the file list, like GitHub's PR bar.
        let total = self
            .right_panel_diff_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.files.len());
        let current = self.right_panel_diff_selected_file.unwrap_or(0);

        div()
            .h(px(44.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .child(source)
            .child(
                div()
                    .text_size(ui_px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.success_text)
                    .child(format!("+{additions}")),
            )
            .child(
                div()
                    .text_size(ui_px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.danger_text)
                    .child(format!("-{deletions}")),
            )
            .when(truncated, |row| {
                row.child(
                    div()
                        .text_size(ui_px(10.5))
                        .text_color(theme.warning_text)
                        .child(tr!("diff.truncated")),
                )
            })
            .child(div().flex_1())
            .when(total > 0, |bar| {
                bar.child(self.render_source_control_nav_button(
                    "review-diff-prev-file",
                    "icons/chevron-up.svg",
                    -1,
                    current == 0,
                    cx,
                ))
                .child(
                    div()
                        .flex_none()
                        .min_w(px(52.0))
                        .text_center()
                        .text_size(ui_px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(tr!(
                            "review.file_position",
                            index = current + 1,
                            total = total
                        )),
                )
                .child(self.render_source_control_nav_button(
                    "review-diff-next-file",
                    "icons/chevron-down.svg",
                    1,
                    current + 1 >= total,
                    cx,
                ))
            })
            .child(refresh)
            .into_any_element()
    }

    /// One file-navigation button of the navbar. `delta` is -1 for the
    /// previous file, 1 for the next; the disabled end stays focusable-looking
    /// but inert.
    fn render_source_control_nav_button(
        &self,
        id: &'static str,
        icon_path: &'static str,
        delta: isize,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let focus = self.transcript_control_focus(id, cx);
        div()
            .id(id)
            .track_focus(&focus)
            .tab_index(0)
            .size(px(24.0))
            .rounded(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .when(disabled, |button| button.opacity(0.4))
            .when(!disabled, |button| {
                button
                    .cursor_default()
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .hover(|style| style.bg(theme.overlay))
                    .active(|style| style.bg(theme.overlay_strong))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.step_source_control_file(delta, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.step_source_control_file(delta, cx);
                            cx.stop_propagation();
                        }
                    }))
            })
            .child(icon(icon_path, 12.0, theme.text_tertiary))
    }

    /// The reader: the flattened single-file rows in one virtualized,
    /// shared-scroll list.
    fn render_review_diff(
        &self,
        snapshot: Arc<ReviewDiffSnapshot>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if snapshot.files.is_empty() {
            return self
                .render_right_panel_empty_message(
                    tr!("diff.no_changes"),
                    tr!("diff.no_changes_description"),
                    cx,
                )
                .into_any_element();
        }
        let entity = cx.entity().downgrade();
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .relative()
            .child(
                list(
                    self.right_panel_diff_list_state.clone(),
                    move |index, _window, cx| {
                        entity
                            .upgrade()
                            .map(|entity| {
                                entity.update(cx, |this, cx| {
                                    this.render_source_control_diff_row(index, cx)
                                })
                            })
                            .unwrap_or_else(|| div().into_any_element())
                    },
                )
                .size_full(),
            )
            .child(scrollbar::vertical(
                &self.right_panel_diff_list_state,
                &self.right_panel_diff_scrollbar,
            ))
            .into_any_element()
    }

    /// One reader row: a full-width unified line, or a side-by-side pair.
    fn render_source_control_diff_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.source_control_diff_rows.borrow().get(index).cloned() else {
            return div().into_any_element();
        };
        match row {
            SourceControlDiffRow::Line(line_index) => self.render_review_diff_line(line_index, cx),
            SourceControlDiffRow::Pair(left, right) => {
                self.render_source_control_split_row(left, right, cx)
            }
        }
    }

    /// One side-by-side row: the old side and the new side of one aligned
    /// change, sharing one row height. A missing side stays blank, and
    /// full-width rows inside the split layout (hunk headers, gaps) span
    /// both sides.
    fn render_source_control_split_row(
        &self,
        left: Option<usize>,
        right: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let side = |line_index: Option<usize>, old_side: bool| -> AnyElement {
            let Some(line_index) = line_index else {
                return div().w(px(1.0)).flex_1().self_stretch().into_any_element();
            };
            let Some(snapshot) = self.right_panel_diff_snapshot.as_ref() else {
                return div().into_any_element();
            };
            let Some(line) = snapshot.lines.get(line_index) else {
                return div().into_any_element();
            };
            let number = if old_side {
                line.old_line
            } else {
                line.new_line
            };
            let (body_background, gutter_background, edge) = match &line.kind {
                crate::review_diff::LineKind::Addition => (
                    Some(theme.success.opacity(0.20)),
                    Some(theme.success.opacity(0.15)),
                    Some(theme.success),
                ),
                crate::review_diff::LineKind::Deletion => (
                    Some(theme.danger.opacity(0.20)),
                    Some(theme.danger.opacity(0.15)),
                    Some(theme.danger),
                ),
                _ => (None, None, None),
            };
            let flat = review_diff_flat_text(line, &theme);
            let selectable = md::render::selectable_flat_text(
                &flat,
                crate::md::selection::TextKey::new(
                    diff_row_selection_key(
                        if old_side {
                            "src-sc-left"
                        } else {
                            "src-sc-right"
                        },
                        line,
                        line_index,
                    ),
                    0,
                ),
                self.right_panel_diff_selection.clone(),
                theme.code_wash,
                theme.selection,
                false,
            );
            div()
                .flex_1()
                .min_w_0()
                .min_h(px(20.0))
                .self_stretch()
                .flex()
                .items_stretch()
                .when_some(body_background, |side, background| side.bg(background))
                .child(
                    div()
                        .w(px(44.0))
                        .self_stretch()
                        .flex_none()
                        .pr(px(8.0))
                        .flex()
                        .items_start()
                        .justify_end()
                        .border_r_1()
                        .border_color(theme.border)
                        .text_color(theme.text_tertiary)
                        .when_some(gutter_background, |gutter, background| {
                            gutter.bg(background)
                        })
                        .children(number.map(|number| number.to_string())),
                )
                .when_some(edge, |side, edge| side.border_l_2().border_color(edge))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .pl(px(10.0))
                        .flex()
                        .items_start()
                        .overflow_hidden()
                        .whitespace_normal()
                        .child(selectable),
                )
                .into_any_element()
        };
        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .items_stretch()
            .font_family(md::render::mono_family())
            .text_size(code_px(10.5))
            .line_height(code_px(20.0))
            .child(side(left, true))
            .child(div().w(px(1.0)).flex_none().self_stretch().bg(theme.border))
            .child(side(right, false))
            .into_any_element()
    }

    fn render_review_diff_line(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(snapshot) = self.right_panel_diff_snapshot.as_ref() else {
            return div().into_any_element();
        };
        let Some(line) = snapshot.lines.get(index) else {
            return div().into_any_element();
        };
        let theme = Theme::current(cx);

        match &line.kind {
            // The file header lives in the fixed header above the list; a
            // row pointing at one is a stale row cache.
            crate::review_diff::LineKind::FileHeader => div().into_any_element(),
            crate::review_diff::LineKind::Gap(gap) => {
                let expandable = gap.is_expandable();
                let chunked = gap.count() > crate::review_diff::DEFAULT_EXPANSION_LINE_COUNT as u32;
                let directions = review_diff_gap_directions(gap.position, chunked);
                let two_directions = directions.len() > 1;
                let gutter = div()
                    .w(px(52.0))
                    .h_full()
                    .flex_none()
                    .flex()
                    .when(two_directions, |gutter| gutter.flex_col())
                    .border_r_1()
                    .border_color(theme.border)
                    .bg(theme.overlay)
                    .when(expandable, |mut gutter| {
                        for (button_index, direction) in directions.iter().copied().enumerate() {
                            gutter = gutter.child(self.render_review_diff_gap_action(
                                index,
                                gap.id,
                                direction,
                                review_diff_gap_icon_path(direction),
                                review_diff_gap_tooltip(direction),
                                two_directions,
                                two_directions && button_index == 0,
                                cx,
                            ));
                        }
                        gutter
                    });
                let label_focus = self
                    .transcript_control_focus(format!("review-diff-gap-{}-label", gap.id), cx);
                let label = div()
                    .id(SharedString::from(format!(
                        "review-diff-gap-{}-label",
                        gap.id
                    )))
                    .track_focus(&label_focus)
                    .h_full()
                    .min_w_0()
                    .flex_1()
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .bg(theme.overlay)
                    .child(tr!("diff.unmodified_lines", count = gap.count()))
                    .when(expandable, |label| {
                        label
                            .tab_index(0)
                            .cursor_default()
                            .focus_visible(|style| style.border_1().border_color(theme.accent))
                            .hover(|style| {
                                style
                                    .bg(theme.overlay_strong)
                                    .text_color(theme.text_secondary)
                            })
                            .active(|style| style.bg(theme.overlay))
                            .tooltip(Tooltip::text(tr!("diff.expand_context")))
                            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                                let direction = if event.modifiers().shift {
                                    crate::review_diff::ExpansionDirection::All
                                } else {
                                    crate::review_diff::ExpansionDirection::Both
                                };
                                this.expand_review_diff_gap(index, direction, cx);
                                cx.stop_propagation();
                            }))
                            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    let direction = if event.keystroke.modifiers.shift {
                                        crate::review_diff::ExpansionDirection::All
                                    } else {
                                        crate::review_diff::ExpansionDirection::Both
                                    };
                                    this.expand_review_diff_gap(index, direction, cx);
                                    cx.stop_propagation();
                                }
                            }))
                    });
                div()
                    .h(px(32.0))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .text_size(ui_px(10.5))
                    .text_color(theme.text_tertiary)
                    .child(gutter)
                    .child(label)
                    .into_any_element()
            }
            crate::review_diff::LineKind::HunkHeader => div()
                .min_h(px(24.0))
                .w_full()
                .min_w_0()
                .flex()
                .items_stretch()
                .font_family(md::render::mono_family())
                .text_size(code_px(10.0))
                .line_height(code_px(16.0))
                .text_color(theme.text_tertiary)
                .child(
                    div()
                        .w(px(52.0))
                        .min_h(px(24.0))
                        .self_stretch()
                        .flex_none()
                        .border_r_1()
                        .border_color(theme.border)
                        .bg(theme.overlay),
                )
                .child(
                    div()
                        .min_h(px(24.0))
                        .min_w_0()
                        .flex_1()
                        .px(px(12.0))
                        .py(px(4.0))
                        .flex()
                        .items_start()
                        .overflow_hidden()
                        .whitespace_normal()
                        .bg(theme.overlay)
                        .child(line.content.clone()),
                )
                .into_any_element(),
            crate::review_diff::LineKind::Meta => div()
                .min_h(px(24.0))
                .w_full()
                .min_w_0()
                .flex()
                .items_stretch()
                .font_family(md::render::mono_family())
                .text_size(code_px(10.5))
                .line_height(code_px(16.0))
                .text_color(theme.text_tertiary)
                .child(div().w(px(52.0)).min_h(px(24.0)).self_stretch().flex_none())
                .child(
                    div()
                        .min_h(px(24.0))
                        .min_w_0()
                        .flex_1()
                        .py(px(4.0))
                        .overflow_hidden()
                        .whitespace_normal()
                        .pr(px(10.0))
                        .child(line.content.clone()),
                )
                .into_any_element(),
            crate::review_diff::LineKind::Context
            | crate::review_diff::LineKind::Addition
            | crate::review_diff::LineKind::Deletion => render_diff_code_row(
                line,
                index,
                "review-diff",
                &self.right_panel_diff_selection,
                DiffRowStyle::REVIEW,
                &theme,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_review_diff_gap_action(
        &self,
        line_index: usize,
        gap_id: u64,
        direction: crate::review_diff::ExpansionDirection,
        icon_path: &'static str,
        tooltip: String,
        compact_half: bool,
        border_bottom: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let direction_name = match direction {
            crate::review_diff::ExpansionDirection::Start => "start",
            crate::review_diff::ExpansionDirection::End => "end",
            crate::review_diff::ExpansionDirection::Both => "both",
            crate::review_diff::ExpansionDirection::All => "all",
        };
        let focus = self
            .transcript_control_focus(format!("review-diff-gap-{gap_id}-button-{direction_name}"), cx);
        div()
            .id(SharedString::from(format!(
                "review-diff-gap-{gap_id}-button-{direction_name}"
            )))
            .track_focus(&focus)
            .tab_index(0)
            .w_full()
            .h_full()
            .min_w_0()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .when(compact_half, |button| button.h(px(16.0)).flex_none())
            .when(border_bottom, |button| {
                button.border_b_1().border_color(theme.border)
            })
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay))
            .active(|style| style.bg(theme.overlay_strong))
            .tooltip(Tooltip::text(tooltip))
            .child(icon(icon_path, 11.0, theme.text_tertiary))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                let direction = if event.modifiers().shift {
                    crate::review_diff::ExpansionDirection::All
                } else {
                    direction
                };
                this.expand_review_diff_gap(line_index, direction, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    let direction = if event.keystroke.modifiers.shift {
                        crate::review_diff::ExpansionDirection::All
                    } else {
                        direction
                    };
                    this.expand_review_diff_gap(line_index, direction, cx);
                    cx.stop_propagation();
                }
            }))
    }

    fn expand_review_diff_gap(
        &mut self,
        line_index: usize,
        direction: crate::review_diff::ExpansionDirection,
        cx: &mut Context<Self>,
    ) {
        let expansion = self
            .right_panel_diff_snapshot
            .as_mut()
            .and_then(|snapshot| Arc::make_mut(snapshot).expand_gap(line_index, direction));
        let Some(expansion) = expansion else {
            return;
        };
        // The revealed context rows replace the gap row in the flattened
        // reader too, at the same position, so the scroll anchor holds.
        let position = self
            .source_control_diff_rows
            .borrow()
            .iter()
            .position(|row| *row == SourceControlDiffRow::Line(line_index));
        if let Some(position) = position {
            let revealed: Vec<SourceControlDiffRow> = (1..=expansion.replacement_count)
                .map(|offset| {
                    let index = line_index + offset;
                    let is_context = self
                        .right_panel_diff_snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.lines.get(index))
                        .is_some_and(|line| {
                            matches!(line.kind, crate::review_diff::LineKind::Context)
                        });
                    if is_context && self.source_control_diff_split {
                        SourceControlDiffRow::Pair(Some(index), Some(index))
                    } else {
                        SourceControlDiffRow::Line(index)
                    }
                })
                .collect();
            let count = expansion.replacement_count;
            self.source_control_diff_rows
                .borrow_mut()
                .splice(position..position + 1, revealed);
            self.right_panel_diff_list_state
                .splice(position..position + 1, count);
        } else {
            self.sync_source_control_diff_rows();
        }
        cx.notify();
    }

    /// One listener set covers every selectable code line registered while
    /// the virtualized review list paints this frame. Skipped while the
    /// update card is open: the window-level dispatch cannot see the card's
    /// occlusion, and on a narrow window the card can overlap this page.
    fn review_diff_selection_input(&self) -> AnyElement {
        if self.update_card_visible() {
            return div().into_any_element();
        }
        let selection = self.right_panel_diff_selection.clone();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| md::render::install_selection_input(window, &selection),
        )
        .absolute()
        .w(px(0.0))
        .h(px(0.0))
        .into_any_element()
    }

    fn latest_review_turn_source(&self) -> Option<ReviewDiffSource> {
        let session = self.selected_session()?;
        session
            .turns
            .iter()
            .rev()
            .find(|turn| {
                turn.turn_count > 0
                    && turn
                        .checkpoint
                        .as_ref()
                        .is_some_and(|checkpoint| checkpoint.status == CheckpointStatus::Ready)
            })
            .map(|turn| ReviewDiffSource::LastTurn {
                session_id: session.id,
                turn_id: turn.id,
                turn_count: turn.turn_count,
            })
    }

    fn review_diff_source_label(&self, source: ReviewDiffSource) -> String {
        match source {
            ReviewDiffSource::LastTurn { .. }
                if self.latest_review_turn_source() == Some(source) =>
            {
                tr!("diff.source_last_turn")
            }
            ReviewDiffSource::LastTurn { turn_count, .. } => {
                tr!("diff.source_turn", turn = turn_count)
            }
            ReviewDiffSource::Uncommitted => tr!("diff.source_uncommitted"),
            ReviewDiffSource::Unstaged => tr!("diff.source_unstaged"),
            ReviewDiffSource::Staged => tr!("diff.source_staged"),
            ReviewDiffSource::Committed => tr!("diff.source_committed"),
            ReviewDiffSource::Branch => tr!("diff.source_branch"),
        }
    }

    /// The toolbar's source menu. The review page is already on screen, so
    /// this only retargets and refreshes; entering the mode itself goes
    /// through [`Fintwind::open_review_page`].
    pub(super) fn set_review_diff_source(
        &mut self,
        source: ReviewDiffSource,
        cx: &mut Context<Self>,
    ) {
        if self.right_panel_diff_source != source {
            self.reset_review_diff_state(source);
        }
        self.refresh_review_diff(cx);
        cx.notify();
    }

    /// Captures one stable Git range and turns it into render-ready rows. Git,
    /// patch parsing, and syntax tokenization all stay off the UI thread; the
    /// generation check prevents an old source or session from landing late.
    pub(super) fn refresh_review_diff(&mut self, cx: &mut Context<Self>) {
        // A pinned workspace can be reviewed without any session open, so
        // the session is not required — only a workspace path is.
        let session_id = self.state.selected_session;
        let Some(project_path) = self
            .review_workspace_path()
            .map(std::path::Path::to_path_buf)
        else {
            self.right_panel_diff_selection.clear();
            self.right_panel_diff_snapshot = None;
            self.right_panel_diff_loading = false;
            self.right_panel_diff_error = Some(tr!("diff.unavailable"));
            return;
        };

        self.right_panel_diff_generation = self.right_panel_diff_generation.wrapping_add(1);
        let generation = self.right_panel_diff_generation;
        let source = self.right_panel_diff_source;
        let selected_path = self.right_panel_diff_selected_file.and_then(|index| {
            self.right_panel_diff_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.files.get(index))
                .map(|file| file.path.clone())
        });
        self.right_panel_diff_loading = true;
        self.right_panel_diff_error = None;
        cx.notify();

        let workspace = fintwind_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |fintwind, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let project_path = project_path.clone();
                    async move {
                        match workspace.request(
                            fintwind_client::WorkspaceOperation::CollectReviewDiff {
                                cwd: project_path,
                                source: crate::review_diff::wire_source(source),
                            },
                        )? {
                            fintwind_client::WorkspaceResult::ReviewDiff { data } => {
                                Ok(crate::review_diff::parse_collected(
                                    source,
                                    &data.numstat,
                                    &data.patch,
                                    data.complete_context,
                                ))
                            }
                            _ => anyhow::bail!("the daemon returned an invalid diff response"),
                        }
                    }
                })
                .await;
            fintwind
                .update(cx, |fintwind, cx| {
                    let still_current = fintwind.state.selected_session == session_id
                        && fintwind.right_panel_diff_generation == generation
                        && fintwind.right_panel_diff_source == source
                        && fintwind
                            .review_workspace_path()
                            .is_some_and(|path| path == project_path);
                    if !still_current {
                        return;
                    }

                    fintwind.right_panel_diff_loading = false;
                    match result {
                        Ok(snapshot) => {
                            fintwind.right_panel_diff_selection.clear();
                            fintwind.right_panel_diff_selected_file = selected_path
                                .as_deref()
                                .and_then(|path| {
                                    snapshot.files.iter().position(|file| file.path == path)
                                })
                                .or_else(|| (!snapshot.files.is_empty()).then_some(0));
                            // A click from the second column's groups lands
                            // here: once the requested source's snapshot is
                            // in, put the clicked file under the reader.
                            if let Some(target) = fintwind.review_pending_file_focus.take()
                                && let Some(index) =
                                    snapshot.files.iter().position(|file| file.path == target)
                            {
                                fintwind.right_panel_diff_selected_file = Some(index);
                            }
                            fintwind.right_panel_diff_snapshot = Some(Arc::new(snapshot));
                            fintwind.right_panel_diff_error = None;
                            fintwind.sync_source_control_diff_rows();
                        }
                        Err(error) => {
                            let message = error.to_string();
                            if fintwind.right_panel_diff_snapshot.is_some() {
                                fintwind.show_toast(tr!("diff.refresh_failed", error = message));
                            } else {
                                fintwind.right_panel_diff_error = Some(message);
                            }
                        }
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

}


/// One rendered row of the source-control page's single-file diff reader.
/// Rows are flattened out of the snapshot ahead of time so the virtualized
/// list never re-derives pairing per frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SourceControlDiffRow {
    /// A full-width row mapping to one snapshot line: a hunk header, a folded
    /// gap, a meta note, or a code line in the unified layout.
    Line(usize),
    /// A side-by-side row: the left (old) and right (new) snapshot line
    /// indexes. `None` leaves that side blank.
    Pair(Option<usize>, Option<usize>),
}

/// The snapshot line range of one file's diff: from after its header row to
/// before the next file's header, or the end of the snapshot.
fn source_control_file_line_range(
    snapshot: &crate::review_diff::Snapshot,
    file_index: usize,
) -> std::ops::Range<usize> {
    let start = snapshot
        .lines
        .iter()
        .position(|line| line.file_index == file_index)
        .map(|start| {
            // The file's header row moves to the fixed file header above the
            // list; the rows start with the first hunk.
            start + 1
        })
        .unwrap_or(snapshot.lines.len());
    let end = snapshot
        .lines
        .iter()
        .skip(start)
        .position(|line| line.file_index != file_index)
        .map(|offset| start + offset)
        .unwrap_or(snapshot.lines.len());
    start..end
}

/// Side-by-side rows for one file's line range: runs of deletions pair with
/// the additions that follow them, context carries both sides, and hunk
/// headers, gaps, and meta notes stay full width.
fn source_control_split_rows(
    snapshot: &crate::review_diff::Snapshot,
    range: std::ops::Range<usize>,
) -> Vec<SourceControlDiffRow> {
    use crate::review_diff::LineKind;

    let lines = &snapshot.lines[range.clone()];
    let mut rows = Vec::new();
    let mut at = range.start;
    while at < range.end {
        let index = at - range.start;
        match lines[index].kind {
            LineKind::Context => {
                rows.push(SourceControlDiffRow::Pair(Some(at), Some(at)));
                at += 1;
            }
            LineKind::Deletion | LineKind::Addition => {
                let deletions = lines[index..]
                    .iter()
                    .take_while(|line| line.kind == LineKind::Deletion)
                    .count();
                let additions = lines[index + deletions..]
                    .iter()
                    .take_while(|line| line.kind == LineKind::Addition)
                    .count();
                let pairs = deletions.max(additions);
                for offset in 0..pairs {
                    rows.push(SourceControlDiffRow::Pair(
                        (offset < deletions).then_some(at + offset),
                        (offset < additions).then_some(at + deletions + offset),
                    ));
                }
                at += deletions + additions;
            }
            LineKind::FileHeader
            | LineKind::HunkHeader
            | LineKind::Gap(_)
            | LineKind::Meta => {
                rows.push(SourceControlDiffRow::Line(at));
                at += 1;
            }
        }
    }
    rows
}

/// One flattened row of the source-control second column: a group header, or
/// a file from the staged/unstaged side. `title` carries the rename form
/// `origin → path` when Git detected a rename.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SourceControlRow {
    Header {
        staged: bool,
        count: usize,
    },
    File {
        staged: bool,
        path: String,
        title: String,
        letter: char,
    },
}

impl SourceControlRow {
    fn file(staged: bool, entry: &fintwind_client::git::WorktreeStatusEntry) -> Self {
        let letter = if staged {
            entry.index_status
        } else {
            entry.worktree_status
        };
        let title = match entry.origin_path.as_deref() {
            Some(origin) => format!("{origin} → {}", entry.path),
            None => entry.path.clone(),
        };
        Self::File {
            staged,
            path: entry.path.clone(),
            title,
            letter,
        }
    }
}

/// The staged side of a porcelain entry: a real index change, not untracked.
fn source_control_entry_is_staged(entry: &fintwind_client::git::WorktreeStatusEntry) -> bool {
    !matches!(entry.index_status, ' ' | '?' | '!')
}

/// The unstaged side: a worktree change or an untracked file. Untracked
/// files belong here so they can be seen (and later staged) like anywhere
/// else in a Git UI.
fn source_control_entry_is_unstaged(entry: &fintwind_client::git::WorktreeStatusEntry) -> bool {
    !matches!(entry.worktree_status, ' ' | '!')
}

/// Status-letter color following the Git convention Fintwind uses elsewhere:
/// green for additions, red for deletions, amber for everything modified,
/// moved, or conflicting, muted for untracked.
fn status_letter_color(letter: char, theme: &Theme) -> gpui::Hsla {
    match letter {
        'A' | 'C' => theme.success_text,
        'D' => theme.danger,
        'M' | 'R' | 'T' | 'U' => theme.warning_text,
        '?' => theme.text_tertiary,
        _ => theme.text_secondary,
    }
}

/// How many of the stat squares read as added; the rest read as removed.
fn diff_stat_split(added: usize, removed: usize) -> Option<usize> {
    let total = added + removed;
    (total > 0).then(|| (added * 5 + total / 2) / total)
}

/// A file's line-change stat: the two counts plus five squares shared
/// between them, after Ely's `DiffStat` badge.
fn render_diff_stat(added: u64, removed: u64, theme: &Theme) -> Div {
    let added = added as usize;
    let removed = removed as usize;
    let green = diff_stat_split(added, removed);
    let square = |index: usize| {
        let color = match green {
            Some(green) if index < green => theme.success,
            Some(_) => theme.danger,
            None => theme.border,
        };
        div().size(px(5.0)).rounded(px(1.5)).bg(color)
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.0))
        .child(
            div()
                .text_size(ui_px(11.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.success_text)
                .child(format!("+{added}")),
        )
        .child(
            div()
                .text_size(ui_px(11.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.danger_text)
                .child(format!("\u{2212}{removed}")),
        )
        .child(div().flex().gap(px(2.0)).children((0..5).map(square)))
}

fn review_diff_gap_icon_path(direction: crate::review_diff::ExpansionDirection) -> &'static str {
    match direction {
        // Pierre's direction attributes and rendered chevrons are inverted by
        // CSS. Fintwind names the data operation directly, so encode the resulting
        // visual here: reveal-from-start points down; reveal-from-end points up.
        crate::review_diff::ExpansionDirection::Start => "icons/chevron-down.svg",
        crate::review_diff::ExpansionDirection::End => "icons/chevron-up.svg",
        crate::review_diff::ExpansionDirection::Both
        | crate::review_diff::ExpansionDirection::All => "icons/chevrons-up-down.svg",
    }
}

fn review_diff_gap_tooltip(direction: crate::review_diff::ExpansionDirection) -> String {
    match direction {
        crate::review_diff::ExpansionDirection::Start => tr!("diff.expand_context_below"),
        crate::review_diff::ExpansionDirection::End => tr!("diff.expand_context_above"),
        crate::review_diff::ExpansionDirection::Both => tr!("diff.expand_context"),
        crate::review_diff::ExpansionDirection::All => tr!("diff.expand_all_context"),
    }
}

fn review_diff_gap_directions(
    position: crate::review_diff::GapPosition,
    chunked: bool,
) -> &'static [crate::review_diff::ExpansionDirection] {
    use crate::review_diff::{ExpansionDirection, GapPosition};

    match (position, chunked) {
        (GapPosition::Leading, _) => &[ExpansionDirection::End],
        (GapPosition::Trailing, _) => &[ExpansionDirection::Start],
        (GapPosition::Between, false) => &[ExpansionDirection::Both],
        (GapPosition::Between, true) => &[ExpansionDirection::Start, ExpansionDirection::End],
    }
}

#[derive(Clone, Copy)]
pub(super) struct DiffRowStyle {
    gutter_width: f32,
    row_height: f32,
    /// What to put in the gutter of a row that has no line number. Git always
    /// reports positions, so this only comes up on a diff synthesized from a
    /// provider's before/after text: there the `+`/`-` marker stands in, which
    /// keeps the gutter from going blank and the meaning off color alone.
    marker_fallback: bool,
}

impl DiffRowStyle {
    pub(super) const REVIEW: Self = Self {
        gutter_width: 52.0,
        row_height: 20.0,
        marker_fallback: false,
    };
    /// The same rows the review page draws, so an edit reads the same
    /// wherever it is opened.
    pub(super) const ACTIVITY: Self = Self {
        marker_fallback: true,
        ..Self::REVIEW
    };
}

/// Selection identity for one diff code row. Selection resolves a drag by
/// looking rows up by key, so every row must have its own.
///
/// Rows with line numbers key on them: they survive the review page's gap
/// expansion, where a revealed gap shifts every later row's index. Rows
/// without them 鈥?a diff synthesized from a provider's before/after text 鈥?/// key on the row index instead, which is stable there because an activity
/// diff is only ever rebuilt whole. Keying those on their (absent) numbers
/// gave every added row the same key, and a drag resolved against whichever
/// duplicate registered first: selections jumped rows, skipped wrapped lines,
/// and collapsed when the head crossed into context.
fn diff_row_selection_key(
    key_prefix: &str,
    line: &crate::review_diff::Line,
    index: usize,
) -> String {
    let kind = match &line.kind {
        crate::review_diff::LineKind::Context => "context",
        crate::review_diff::LineKind::Addition => "addition",
        crate::review_diff::LineKind::Deletion => "deletion",
        _ => "other",
    };
    match (line.old_line, line.new_line) {
        (None, None) => format!("{key_prefix}-line-{}-{kind}-i{index}", line.file_index),
        (old, new) => format!(
            "{key_prefix}-line-{}-{kind}-{}-{}",
            line.file_index,
            old.unwrap_or(0),
            new.unwrap_or(0),
        ),
    }
}

/// One context, addition, or deletion row, shared by the review page and the
/// diff inside an expanded file-change activity so the two never drift.
pub(super) fn render_diff_code_row(
    line: &crate::review_diff::Line,
    index: usize,
    key_prefix: &str,
    selection: &TranscriptSelection,
    style: DiffRowStyle,
    theme: &Theme,
) -> AnyElement {
    let semantic_body_opacity = if theme.is_dark { 0.20 } else { 0.12 };
    let semantic_gutter_opacity = if theme.is_dark { 0.15 } else { 0.09 };
    let (marker, body_background, gutter_background, edge, number_color) = match &line.kind {
        crate::review_diff::LineKind::Addition => (
            "+",
            Some(theme.success.opacity(semantic_body_opacity)),
            Some(theme.success.opacity(semantic_gutter_opacity)),
            Some(theme.success),
            theme.success,
        ),
        crate::review_diff::LineKind::Deletion => (
            "-",
            Some(theme.danger.opacity(semantic_body_opacity)),
            Some(theme.danger.opacity(semantic_gutter_opacity)),
            Some(theme.danger),
            theme.danger,
        ),
        _ => (" ", None, None, None, theme.text_tertiary),
    };
    let shown_line = line.new_line.or(line.old_line);
    let flat = review_diff_flat_text(line, theme);
    let selectable = md::render::selectable_flat_text(
        &flat,
        crate::md::selection::TextKey::new(diff_row_selection_key(key_prefix, line, index), 0),
        selection.clone(),
        theme.code_wash,
        theme.selection,
        false,
    );
    let gutter = div()
        .w(px(style.gutter_width))
        .min_h(px(style.row_height))
        .self_stretch()
        .flex_none()
        .pr(px(9.0))
        .flex()
        .items_start()
        .justify_end()
        .border_r_1()
        .border_color(theme.border)
        .text_color(number_color)
        .when_some(gutter_background, |gutter, background| {
            gutter.bg(background)
        })
        .child(
            shown_line
                .map(|line| line.to_string())
                .or_else(|| style.marker_fallback.then(|| marker.to_owned()))
                .unwrap_or_default(),
        );
    let body = div()
        .min_h(px(style.row_height))
        .self_stretch()
        .min_w_0()
        .flex_1()
        .pl(px(12.0))
        .flex()
        .items_start()
        .when_some(body_background, |body, background| body.bg(background))
        .child(
            div()
                .id(SharedString::from(format!(
                    "{key_prefix}-line-content-{index}"
                )))
                .min_h(px(style.row_height))
                .min_w_0()
                .flex_1()
                .pr(px(10.0))
                .flex()
                .items_start()
                .overflow_hidden()
                .whitespace_normal()
                .child(selectable),
        );
    div()
        .id(SharedString::from(format!("{key_prefix}-row-{index}")))
        .w_full()
        .min_w_0()
        .min_h(px(style.row_height))
        // A wrapped line makes the row taller than one line. Stacked in a
        // scrolling column, a shrinkable row would be squeezed back to one
        // and paint its overflow over the row beneath it.
        .flex_none()
        .flex()
        .items_stretch()
        .font_family(md::render::mono_family())
        .text_size(code_px(10.5))
        .line_height(code_px(style.row_height))
        .when_some(edge, |row, edge| row.border_l_2().border_color(edge))
        .child(gutter)
        .child(body)
        .into_any_element()
}

fn review_diff_flat_text(line: &crate::review_diff::Line, theme: &Theme) -> md::render::FlatText {
    let text = line.content.clone();
    let palette = MarkdownPalette::from_theme(theme);
    let code_font = font(md::render::mono_family());
    let mut runs = Vec::with_capacity(line.tokens.len() * 2 + 1);
    let mut offset = 0;
    let mut push = |len: usize, color: Hsla| {
        if len > 0 {
            runs.push(TextRun {
                len,
                font: code_font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    };
    for token in &line.tokens {
        if token.range.start > offset {
            push(token.range.start - offset, theme.text_secondary);
        }
        push(token.range.len(), palette.token(token.class));
        offset = token.range.end;
    }
    if offset < text.len() {
        push(text.len() - offset, theme.text_secondary);
    }
    md::render::FlatText {
        text: text.into(),
        runs,
        links: Vec::new(),
        code_ranges: Vec::new(),
        math: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review_file(path: &str) -> crate::review_diff::File {
        crate::review_diff::File {
            path: path.into(),
            additions: 1,
            deletions: 0,
            status: crate::review_diff::FileStatus::Modified,
            diff_line: None,
        }
    }

    fn review_files() -> Vec<crate::review_diff::File> {
        [
            "README.md",
            "src/app/runtime.rs",
            "src/app/view.rs",
            "src/lib.rs",
            "tests/review.rs",
        ]
        .into_iter()
        .map(review_file)
        .collect()
    }

    fn split_test_line(
        file_index: usize,
        kind: crate::review_diff::LineKind,
    ) -> crate::review_diff::Line {
        crate::review_diff::Line {
            file_index,
            old_line: None,
            new_line: None,
            kind,
            content: String::new(),
            tokens: Vec::new(),
        }
    }

    fn split_test_snapshot(
        lines: Vec<crate::review_diff::Line>,
    ) -> crate::review_diff::Snapshot {
        crate::review_diff::Snapshot {
            source: crate::review_diff::Source::default(),
            files: vec![review_file("a.rs")],
            lines,
            additions: 0,
            deletions: 0,
            truncated: false,
        }
    }

    #[test]
    fn split_rows_pair_deletions_with_the_additions_that_follow() {
        use crate::review_diff::LineKind::{Addition, Context, Deletion, FileHeader, HunkHeader};

        let snapshot = split_test_snapshot(vec![
            split_test_line(0, FileHeader),
            split_test_line(0, HunkHeader),
            split_test_line(0, Context),
            split_test_line(0, Deletion),
            split_test_line(0, Deletion),
            split_test_line(0, Addition),
            split_test_line(0, Context),
            split_test_line(0, Addition),
        ]);
        let rows = source_control_split_rows(
            &snapshot,
            source_control_file_line_range(&snapshot, 0),
        );
        assert_eq!(
            rows,
            vec![
                // The hunk header stays full width; the file header is not
                // part of the range at all.
                SourceControlDiffRow::Line(1),
                SourceControlDiffRow::Pair(Some(2), Some(2)),
                SourceControlDiffRow::Pair(Some(3), Some(5)),
                SourceControlDiffRow::Pair(Some(4), None),
                SourceControlDiffRow::Pair(Some(6), Some(6)),
                SourceControlDiffRow::Pair(None, Some(7)),
            ]
        );
    }

    #[test]
    fn the_file_range_skips_its_header_and_stops_at_the_next_file() {
        use crate::review_diff::LineKind::{Context, FileHeader};

        let mut snapshot = split_test_snapshot(vec![
            split_test_line(0, FileHeader),
            split_test_line(0, Context),
            split_test_line(1, FileHeader),
            split_test_line(1, Context),
        ]);
        snapshot.files.push(review_file("b.rs"));
        assert_eq!(source_control_file_line_range(&snapshot, 0), 1..2);
        assert_eq!(source_control_file_line_range(&snapshot, 1), 3..4);
        assert_eq!(source_control_file_line_range(&snapshot, 9), 4..4);
    }

    #[test]
    fn review_gap_expansion_icons_match_pierre_visual_directions() {
        use crate::review_diff::{ExpansionDirection, GapPosition};

        assert_eq!(
            review_diff_gap_directions(GapPosition::Leading, true),
            &[ExpansionDirection::End]
        );
        assert_eq!(
            review_diff_gap_directions(GapPosition::Trailing, true),
            &[ExpansionDirection::Start]
        );
        assert_eq!(
            review_diff_gap_directions(GapPosition::Between, false),
            &[ExpansionDirection::Both]
        );
        assert_eq!(
            review_diff_gap_directions(GapPosition::Between, true),
            &[ExpansionDirection::Start, ExpansionDirection::End]
        );

        assert_eq!(
            review_diff_gap_icon_path(ExpansionDirection::Start),
            "icons/chevron-down.svg"
        );
        assert_eq!(
            review_diff_gap_icon_path(ExpansionDirection::End),
            "icons/chevron-up.svg"
        );
        assert_eq!(
            review_diff_gap_icon_path(ExpansionDirection::Both),
            "icons/chevrons-up-down.svg"
        );
    }

    #[test]
    fn review_render_path_only_reads_the_in_memory_snapshot() {        let source = include_str!("source_control.rs");
        let start = source
            .find("\n    fn render_review_diff_body(")
            .expect("review render fn");
        let body = &source[start + 1..];
        let end = body
            .find("\n    fn render_review_diff_toolbar(")
            .expect("review render end");
        let body = &body[..end];

        for forbidden in [
            "Command::new",
            "std::fs::",
            "review_diff::collect",
            "capture_worktree_commit",
        ] {
            assert!(
                !body.contains(forbidden),
                "Review rendering must not call `{forbidden}`; prepare it in refresh_review_diff"
            );
        }
    }

    /// A wrapped diff line must grow its row rather than be clipped by it.
    /// Both the review page's own rows and the shared code row have to hold
    /// this, and the shared one is also what the transcript's diff paints
    /// with.
    #[test]
    fn diff_text_rows_soft_wrap() {
        let source = include_str!("source_control.rs");
        let panel = source
            .split_once("\n    fn render_review_diff_line(")
            .expect("review diff line renderer")
            .1
            .split_once("\n    #[allow(clippy::too_many_arguments)]")
            .expect("review diff line renderer end")
            .0;
        let shared = source
            .split_once("\npub(super) fn render_diff_code_row(")
            .expect("shared diff code row")
            .1
            .split_once("\nfn review_diff_flat_text(")
            .expect("shared diff code row end")
            .0;

        for body in [panel, shared] {
            assert!(!body.contains(".whitespace_nowrap()"));
        }
        assert!(panel.matches(".whitespace_normal()").count() >= 2);
        assert!(shared.contains(".whitespace_normal()"));
        assert!(shared.contains(".min_h(px(style.row_height))"));
        assert!(!shared.contains(".h(px(style.row_height))"));
    }
}

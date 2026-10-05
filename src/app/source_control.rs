use crate::theme::{code_px, ui_px};

use gpui::{Context, Div, IntoElement, Window, div, px, prelude::*};

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
        self.right_panel_diff_expanded_paths.clear();
        self.right_panel_diff_tree_cursor = None;
        self.right_panel_diff_tree_rows.borrow_mut().clear();
        self.right_panel_diff_tree_list_state.reset(0);
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

    /// The mode's second column: the workspace the review is pointed at.
    /// Showing the project, the concrete directory (a session worktree can
    /// differ from the project root), and the branch guards against running
    /// Git operations in the wrong place.
    pub(super) fn render_source_control_secondary(
        &mut self,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let workspace = self
            .selected_workspace_path()
            .map(|path| path.to_path_buf());
        let branch = workspace
            .as_deref()
            .and_then(|path| self.sidebar_branch_for_workspace(path, cx));
        let project_name = self
            .selected_project()
            .map(|project| {
                if project.is_projectless() {
                    tr!("project.without_a_project")
                } else {
                    project.display_name()
                }
            })
            .unwrap_or_else(|| tr!("sidebar.unknown_project"));

        div()
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .child(
                div()
                    .px(px(12.0))
                    .pt(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(ui_px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_tertiary)
                            .child(tr!("source_control.workspace")),
                    )
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
                    .when_some(branch, |column, branch| {
                        column.child(
                            div()
                                .mt(px(2.0))
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .child(icon("icons/git-branch.svg", 12.0, theme.text_tertiary))
                                .child(
                                    div()
                                        .text_size(ui_px(11.5))
                                        .text_color(theme.text_secondary)
                                        .child(branch),
                                ),
                        )
                    }),
            )
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
                        .child(
                            self.render_review_diff_body(
                                self.chat_viewport_width(window),
                                window,
                                cx,
                            ),
                        ),
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
        page_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let toolbar = self.render_review_diff_toolbar(cx);
        let content = match self.right_panel_diff_snapshot.clone() {
            Some(snapshot) => {
                let tree_width = fitted_file_tree_width(
                    page_width,
                    self.right_panel_file_tree_width.max(220.0),
                );
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .child(self.render_review_diff(snapshot.clone(), cx))
                    .child(
                        div()
                            .w(px(tree_width))
                            .min_w(px(FILE_TREE_MIN_WIDTH))
                            .h_full()
                            .flex_none()
                            .relative()
                            .border_l_1()
                            .border_color(theme.border_strong)
                            .child(self.render_review_diff_tree(window, cx))
                            .child(self.render_panel_resize_handle(
                                "review-diff-tree-resize-handle",
                                PanelResizeTarget::FileTree,
                                cx,
                            )),
                    )
                    .into_any_element()
            }
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
            .child(toolbar)
            .child(content)
            .child(self.review_diff_selection_input())
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
            .child(refresh)
            .into_any_element()
    }

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
                                    this.render_review_diff_line(index, cx)
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

    fn render_review_diff_line(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(snapshot) = self.right_panel_diff_snapshot.as_ref() else {
            return div().into_any_element();
        };
        let Some(line) = snapshot.lines.get(index) else {
            return div().into_any_element();
        };
        let Some(file) = snapshot.files.get(line.file_index) else {
            return div().into_any_element();
        };
        let theme = Theme::current(cx);

        match &line.kind {
            crate::review_diff::LineKind::FileHeader => div()
                .id(SharedString::from(format!("review-diff-file-{index}")))
                .w_full()
                .min_w_0()
                .h(px(36.0))
                .px(px(12.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.surface)
                .child(file_icon(file_icon_for_path(&file.path), 14.0))
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "review-diff-file-path-{index}"
                        )))
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(ui_px(11.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_secondary)
                        .tooltip(Tooltip::text(file.path.clone()))
                        .child(file.path.clone()),
                )
                .child(
                    div()
                        .text_size(ui_px(10.5))
                        .text_color(theme.success_text)
                        .child(format!("+{}", file.additions)),
                )
                .child(
                    div()
                        .text_size(ui_px(10.5))
                        .text_color(theme.danger_text)
                        .child(format!("-{}", file.deletions)),
                )
                .into_any_element(),
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
        self.right_panel_diff_list_state
            .splice(line_index..line_index + 1, expansion.replacement_count);
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

    fn render_review_diff_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let focus = self.transcript_control_focus("review-diff-tree", cx);
        let tree_focused = focus.is_focused(window);
        let entity = cx.entity().downgrade();
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(44.0))
                    .flex_none()
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        TextField::new(
                            "review-diff-filter",
                            self.right_panel_diff_filter.clone(),
                        )
                        .icon("icons/search.svg", 13.0)
                        .w_full(),
                    ),
            )
            .child(
                div()
                    .id("review-diff-tree")
                    .track_focus(&focus)
                    .tab_index(0)
                    .key_context("ReviewDiffTree")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        this.review_diff_tree_key_down(event, window, cx)
                    }))
                    .child(
                        list(
                            self.right_panel_diff_tree_list_state.clone(),
                            move |index, _window, cx| {
                                entity
                                    .upgrade()
                                    .map(|entity| {
                                        entity.update(cx, |this, cx| {
                                            this.render_review_diff_tree_row(
                                                index,
                                                tree_focused,
                                                cx,
                                            )
                                        })
                                    })
                                    .unwrap_or_else(|| div().into_any_element())
                            },
                        )
                        .size_full()
                        .py(px(4.0)),
                    )
                    .child(scrollbar::vertical(
                        &self.right_panel_diff_tree_list_state,
                        &self.right_panel_diff_tree_scrollbar,
                    )),
            )
    }

    fn render_review_diff_tree_row(
        &self,
        index: usize,
        tree_focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.right_panel_diff_tree_rows.borrow().get(index).cloned() else {
            return div().h(px(30.0)).into_any_element();
        };
        let theme = Theme::current(cx);
        let cursor = tree_focused && self.right_panel_diff_tree_cursor == Some(index);
        match row {
            ReviewDiffTreeRow::Directory {
                path,
                name,
                depth,
                expanded,
            } => div()
                .w_full()
                .h(px(30.0))
                .px(px(6.0))
                .flex()
                .items_center()
                .child(
                    div()
                        .id(SharedString::from(format!("review-diff-directory-{path}")))
                        .h(px(26.0))
                        .flex_1()
                        .min_w_0()
                        .pl(px(7.0 + depth as f32 * 14.0))
                        .pr(px(7.0))
                        .rounded(px(5.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .cursor_default()
                        .when(cursor, |row| row.bg(theme.overlay_strong))
                        .when(!cursor, |row| {
                            row.hover(|row| row.bg(theme.overlay))
                                .active(|row| row.bg(theme.overlay_strong))
                        })
                        .child(icon(
                            if expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            },
                            10.0,
                            theme.text_ghost,
                        ))
                        .child(icon("icons/folder.svg", 13.0, theme.text_tertiary))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_size(ui_px(11.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text_secondary)
                                .child(name),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let focus = this.transcript_control_focus("review-diff-tree", cx);
                            focus.focus(window, cx);
                            this.right_panel_diff_tree_cursor = Some(index);
                            this.toggle_review_diff_directory(path.clone(), cx);
                        })),
                )
                .into_any_element(),
            ReviewDiffTreeRow::File { file_index, depth } => {
                let Some(snapshot) = self.right_panel_diff_snapshot.as_ref() else {
                    return div().h(px(30.0)).into_any_element();
                };
                let Some(file) = snapshot.files.get(file_index) else {
                    return div().h(px(30.0)).into_any_element();
                };
                let path = file.path.clone();
                let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
                let selected = self.right_panel_diff_selected_file == Some(file_index);
                let (status, status_color) = match file.status {
                    crate::review_diff::FileStatus::Added => ("A", theme.success),
                    crate::review_diff::FileStatus::Deleted => ("D", theme.danger),
                    crate::review_diff::FileStatus::Binary => ("B", theme.warning),
                    crate::review_diff::FileStatus::Modified => ("M", theme.warning),
                };
                div()
                    .w_full()
                    .h(px(30.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "review-diff-tree-file-{path}"
                            )))
                            .h(px(26.0))
                            .flex_1()
                            .min_w_0()
                            .pl(px(23.0 + depth as f32 * 14.0))
                            .pr(px(7.0))
                            .rounded(px(5.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .cursor_default()
                            .when(selected && cursor, |row| row.bg(theme.overlay_strong))
                            .when(selected ^ cursor, |row| row.bg(theme.overlay))
                            .when(!selected && !cursor, |row| {
                                row.hover(|row| row.bg(theme.overlay))
                                    .active(|row| row.bg(theme.overlay_strong))
                            })
                            .child(file_icon(file_icon_for_path(&path), 13.0))
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "review-diff-tree-file-path-{file_index}"
                                    )))
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_size(ui_px(11.0))
                                    .text_color(if selected {
                                        theme.text
                                    } else {
                                        theme.text_secondary
                                    })
                                    .tooltip(Tooltip::text(path.clone()))
                                    .child(name),
                            )
                            .child(
                                div()
                                    .w(px(16.0))
                                    .h(px(16.0))
                                    .flex_none()
                                    .rounded(px(4.0))
                                    .border_1()
                                    .border_color(status_color.opacity(0.65))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(ui_px(9.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(status_color)
                                    .child(status),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let focus =
                                    this.transcript_control_focus("review-diff-tree", cx);
                                focus.focus(window, cx);
                                this.right_panel_diff_tree_cursor = Some(index);
                                this.select_review_diff_file(file_index, cx);
                            })),
                    )
                    .into_any_element()
            }
        }
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
        let Some(session_id) = self.state.selected_session else {
            self.right_panel_diff_selection.clear();
            self.right_panel_diff_snapshot = None;
            self.right_panel_diff_loading = false;
            self.right_panel_diff_error = Some(tr!("diff.unavailable"));
            return;
        };
        let Some(project_path) = self
            .selected_workspace_path()
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
        let had_snapshot = self.right_panel_diff_snapshot.is_some();
        let previous_directories = self
            .right_panel_diff_snapshot
            .as_ref()
            .map_or_else(HashSet::new, |snapshot| {
                review_diff_directory_paths(&snapshot.files)
            });
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
                    let still_current = fintwind.state.selected_session == Some(session_id)
                        && fintwind.right_panel_diff_generation == generation
                        && fintwind.right_panel_diff_source == source
                        && fintwind
                            .selected_workspace_path()
                            .is_some_and(|path| path == project_path);
                    if !still_current {
                        return;
                    }

                    fintwind.right_panel_diff_loading = false;
                    match result {
                        Ok(snapshot) => {
                            fintwind.right_panel_diff_selection.clear();
                            let directories = review_diff_directory_paths(&snapshot.files);
                            if had_snapshot {
                                fintwind
                                    .right_panel_diff_expanded_paths
                                    .retain(|path| directories.contains(path));
                                fintwind
                                    .right_panel_diff_expanded_paths
                                    .extend(directories.difference(&previous_directories).cloned());
                            } else {
                                fintwind.right_panel_diff_expanded_paths = directories;
                            }
                            fintwind.right_panel_diff_selected_file = selected_path
                                .as_deref()
                                .and_then(|path| {
                                    snapshot.files.iter().position(|file| file.path == path)
                                })
                                .or_else(|| (!snapshot.files.is_empty()).then_some(0));
                            let line_count = snapshot.lines.len();
                            fintwind.right_panel_diff_snapshot = Some(Arc::new(snapshot));
                            fintwind.right_panel_diff_error = None;
                            fintwind.right_panel_diff_list_state.reset(line_count);
                            fintwind.sync_review_diff_tree_rows(cx);
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

    pub(super) fn sync_review_diff_tree_rows(&mut self, cx: &mut Context<Self>) {
        let filter = self.right_panel_diff_filter.read(cx).content().to_owned();
        let previous_cursor_row = self
            .right_panel_diff_tree_cursor
            .and_then(|index| self.right_panel_diff_tree_rows.borrow().get(index).cloned());
        let rows = self
            .right_panel_diff_snapshot
            .as_ref()
            .map_or_else(Vec::new, |snapshot| {
                review_diff_tree_rows(
                    &snapshot.files,
                    &self.right_panel_diff_expanded_paths,
                    &filter,
                )
            });
        let cursor = previous_cursor_row
            .as_ref()
            .and_then(|previous| {
                rows.iter().position(|row| match (previous, row) {
                    (
                        ReviewDiffTreeRow::Directory { path: left, .. },
                        ReviewDiffTreeRow::Directory { path: right, .. },
                    ) => left == right,
                    (
                        ReviewDiffTreeRow::File {
                            file_index: left, ..
                        },
                        ReviewDiffTreeRow::File {
                            file_index: right, ..
                        },
                    ) => left == right,
                    _ => false,
                })
            })
            .or_else(|| {
                self.right_panel_diff_selected_file.and_then(|selected| {
                    rows.iter().position(|row| {
                        matches!(
                            row,
                            ReviewDiffTreeRow::File { file_index, .. }
                                if *file_index == selected
                        )
                    })
                })
            })
            .or_else(|| (!rows.is_empty()).then_some(0));
        let row_count = rows.len();
        *self.right_panel_diff_tree_rows.borrow_mut() = rows;
        self.right_panel_diff_tree_cursor = cursor;
        self.right_panel_diff_tree_list_state
            .reset_with_uniform_height(row_count, px(30.0));
    }

    fn toggle_review_diff_directory(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.right_panel_diff_expanded_paths.remove(&path) {
            self.right_panel_diff_expanded_paths.insert(path);
        }
        self.sync_review_diff_tree_rows(cx);
        cx.notify();
    }

    fn select_review_diff_file(&mut self, file_index: usize, cx: &mut Context<Self>) {
        self.right_panel_diff_selected_file = Some(file_index);
        if let Some(line) = self
            .right_panel_diff_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.files.get(file_index))
            .and_then(|file| file.diff_line)
        {
            // `scroll_to_reveal_item` bottom-aligns targets below the viewport,
            // which can reveal only the file header and leave its diff body
            // off-screen. A tree selection is an explicit jump, so top-anchor
            // the header and expose the content immediately below it.
            self.right_panel_diff_list_state
                .scroll_to(gpui::ListOffset {
                    item_ix: line,
                    offset_in_item: px(0.0),
                });
        }
        cx.notify();
    }

    fn review_diff_tree_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.right_panel_diff_tree_rows.borrow().clone();
        if rows.is_empty() {
            return;
        }
        let current = self
            .right_panel_diff_tree_cursor
            .filter(|index| *index < rows.len())
            .unwrap_or(0);
        let key = event.keystroke.key.as_str();
        let target = match key {
            "up" => Some(current.saturating_sub(1)),
            "down" => Some((current + 1).min(rows.len() - 1)),
            "home" => Some(0),
            "end" => Some(rows.len() - 1),
            "left" => match &rows[current] {
                ReviewDiffTreeRow::Directory {
                    path,
                    expanded: true,
                    ..
                } => {
                    self.toggle_review_diff_directory(path.clone(), cx);
                    None
                }
                ReviewDiffTreeRow::Directory { depth, .. }
                | ReviewDiffTreeRow::File { depth, .. } => {
                    rows[..current].iter().rposition(|row| {
                        matches!(
                            row,
                            ReviewDiffTreeRow::Directory {
                                depth: parent_depth,
                                ..
                            } if *parent_depth < *depth
                        )
                    })
                }
            },
            "right" => match &rows[current] {
                ReviewDiffTreeRow::Directory {
                    path,
                    expanded: false,
                    ..
                } => {
                    self.toggle_review_diff_directory(path.clone(), cx);
                    None
                }
                ReviewDiffTreeRow::Directory { depth, .. }
                    if rows.get(current + 1).is_some_and(|row| match row {
                        ReviewDiffTreeRow::Directory {
                            depth: child_depth, ..
                        }
                        | ReviewDiffTreeRow::File {
                            depth: child_depth, ..
                        } => child_depth > depth,
                    }) =>
                {
                    Some(current + 1)
                }
                _ => None,
            },
            "enter" | "space" => {
                match &rows[current] {
                    ReviewDiffTreeRow::Directory { path, .. } => {
                        self.toggle_review_diff_directory(path.clone(), cx)
                    }
                    ReviewDiffTreeRow::File { file_index, .. } => {
                        self.select_review_diff_file(*file_index, cx)
                    }
                }
                None
            }
            _ => return,
        };
        if let Some(target) = target {
            self.right_panel_diff_tree_cursor = Some(target);
            self.right_panel_diff_tree_list_state
                .scroll_to_reveal_item(target);
            cx.notify();
        }
        cx.stop_propagation();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ReviewDiffTreeRow {
    Directory {
        path: String,
        name: String,
        depth: usize,
        expanded: bool,
    },
    File {
        file_index: usize,
        depth: usize,
    },
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

fn review_diff_directory_paths(files: &[crate::review_diff::File]) -> HashSet<String> {
    let mut paths = HashSet::new();
    for file in files {
        let parts = file.path.split('/').collect::<Vec<_>>();
        let mut path = String::new();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            paths.insert(path.clone());
        }
    }
    paths
}

fn review_diff_tree_rows(
    files: &[crate::review_diff::File],
    expanded_paths: &HashSet<String>,
    filter: &str,
) -> Vec<ReviewDiffTreeRow> {
    let filter = filter.trim().to_ascii_lowercase();
    let filtering = !filter.is_empty();
    let mut indexes = files
        .iter()
        .enumerate()
        .filter(|(_, file)| {
            filtering
                .then(|| file.path.to_ascii_lowercase().contains(&filter))
                .unwrap_or(true)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    indexes.sort_by_key(|index| files[*index].path.to_ascii_lowercase());

    let mut rows = Vec::new();
    let mut emitted_directories = HashSet::new();
    for file_index in indexes {
        let parts = files[file_index].path.split('/').collect::<Vec<_>>();
        let mut directory = String::new();
        let mut visible = true;
        for (depth, part) in parts.iter().take(parts.len().saturating_sub(1)).enumerate() {
            if !directory.is_empty() {
                directory.push('/');
            }
            directory.push_str(part);
            let expanded = filtering || expanded_paths.contains(&directory);
            if emitted_directories.insert(directory.clone()) && visible {
                rows.push(ReviewDiffTreeRow::Directory {
                    path: directory.clone(),
                    name: (*part).to_owned(),
                    depth,
                    expanded,
                });
            }
            if !expanded {
                visible = false;
                break;
            }
        }
        if visible {
            rows.push(ReviewDiffTreeRow::File {
                file_index,
                depth: parts.len().saturating_sub(1),
            });
        }
    }
    rows
}

/// How wide and tall a diff row is drawn. The review page is a reading
/// surface; the copy embedded in a transcript activity is a summary and gives
/// its space back to the code.
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
/// without them — a diff synthesized from a provider's before/after text —
/// key on the row index instead, which is stable there because an activity
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
    fn review_tree_builds_shared_directories_once() {
        let files = review_files();
        let expanded = review_diff_directory_paths(&files);
        assert_eq!(
            review_diff_tree_rows(&files, &expanded, ""),
            vec![
                ReviewDiffTreeRow::File {
                    file_index: 0,
                    depth: 0,
                },
                ReviewDiffTreeRow::Directory {
                    path: "src".into(),
                    name: "src".into(),
                    depth: 0,
                    expanded: true,
                },
                ReviewDiffTreeRow::Directory {
                    path: "src/app".into(),
                    name: "app".into(),
                    depth: 1,
                    expanded: true,
                },
                ReviewDiffTreeRow::File {
                    file_index: 1,
                    depth: 2,
                },
                ReviewDiffTreeRow::File {
                    file_index: 2,
                    depth: 2,
                },
                ReviewDiffTreeRow::File {
                    file_index: 3,
                    depth: 1,
                },
                ReviewDiffTreeRow::Directory {
                    path: "tests".into(),
                    name: "tests".into(),
                    depth: 0,
                    expanded: true,
                },
                ReviewDiffTreeRow::File {
                    file_index: 4,
                    depth: 1,
                },
            ]
        );
    }

    #[test]
    fn review_tree_collapse_hides_only_descendants() {
        let files = review_files();
        let expanded = HashSet::from(["src".to_owned()]);
        let rows = review_diff_tree_rows(&files, &expanded, "");

        assert!(rows.contains(&ReviewDiffTreeRow::Directory {
            path: "src/app".into(),
            name: "app".into(),
            depth: 1,
            expanded: false,
        }));
        assert!(rows.contains(&ReviewDiffTreeRow::File {
            file_index: 3,
            depth: 1,
        }));
        assert!(!rows.iter().any(|row| {
            matches!(
                row,
                ReviewDiffTreeRow::File { file_index, .. } if *file_index == 1 || *file_index == 2
            )
        }));
    }

    #[test]
    fn review_tree_filter_reveals_matching_path_and_ancestors() {
        let rows = review_diff_tree_rows(&review_files(), &HashSet::new(), "RUNTIME");
        assert_eq!(
            rows,
            vec![
                ReviewDiffTreeRow::Directory {
                    path: "src".into(),
                    name: "src".into(),
                    depth: 0,
                    expanded: true,
                },
                ReviewDiffTreeRow::Directory {
                    path: "src/app".into(),
                    name: "app".into(),
                    depth: 1,
                    expanded: true,
                },
                ReviewDiffTreeRow::File {
                    file_index: 1,
                    depth: 2,
                },
            ]
        );
    }

    /// Selection resolves rows by key, so a repeated key makes a drag jump
    /// between the duplicates. Numbered rows keep their number-derived keys
    /// (stable across the review page's gap expansion); rows a provider never
    /// positioned fall back to the row index.
    #[test]
    fn diff_row_selection_keys_are_unique_even_without_line_numbers() {
        let positionless =
            crate::review_diff::from_file_changes(&[crate::model::ActivityFileChange {
                path: "a.md".into(),
                additions: Some(2),
                deletions: Some(0),
                status: None,
                diff: Some("@@\n+one\n+two\n \n+three\n".into()),
            }]);
        let keys = positionless
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                matches!(
                    line.kind,
                    crate::review_diff::LineKind::Context
                        | crate::review_diff::LineKind::Addition
                        | crate::review_diff::LineKind::Deletion
                )
            })
            .map(|(index, line)| diff_row_selection_key("activity", line, index))
            .collect::<Vec<_>>();
        let unique = keys.iter().collect::<HashSet<_>>();
        assert_eq!(unique.len(), keys.len(), "{keys:?}");

        let numbered = crate::review_diff::Line {
            file_index: 0,
            old_line: Some(4),
            new_line: Some(6),
            kind: crate::review_diff::LineKind::Context,
            content: "kept".into(),
            tokens: Vec::new(),
        };
        assert_eq!(
            diff_row_selection_key("review-diff", &numbered, 9),
            "review-diff-line-0-context-4-6",
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

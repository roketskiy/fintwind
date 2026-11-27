use crate::git_history::CommitEntry;
use crate::theme::ui_px;
use crate::ui::ActivationExt;
use crate::ui::loading::{LoadingDelay, LoadingPhase};
use gpui::{point, relative};

use std::path::Path as StdPath;

use super::*;

pub(super) use fintwind_scm_ui::history::*;


impl Fintwind {
    fn sync_history_rows(&mut self, key: HistoryKey, rows: &Arc<HistoryRows>) {
        let height = ui_px(HISTORY_ROW_HEIGHT);
        let same_key = self.history_row_key.as_ref() == Some(&key);
        let same_rows = Arc::ptr_eq(&self.history_row_cache, rows);
        if same_key && same_rows && self.history_row_height == height {
            return;
        }
        if same_key && same_rows {
            self.history_row_height = height;
            self.history_list_state.remeasure();
            return;
        }
        let mut anchor = self.history_list_state.logical_scroll_top();
        if same_key {
            // O(1) lookup into indexes prepared on the worker. New commits at
            // the top must not move the commit currently under the reader.
            if let Some(index) = self
                .history_row_cache
                .commits
                .get(anchor.item_ix)
                .and_then(|commit| rows.positions.get(&commit.hash))
            {
                anchor.item_ix = *index;
            }
            anchor.offset_in_item *=
                f32::from(height) / f32::from(self.history_row_height).max(1.0);
            self.history_highlight = self
                .history_highlight
                .and_then(|index| self.history_row_cache.commits.get(index))
                .and_then(|commit| rows.positions.get(&commit.hash))
                .copied();
        } else {
            self.history_highlight = None;
            self.history_details_scroll
                .set_offset(point(px(0.0), px(0.0)));
        }
        self.history_row_cache = rows.clone();
        self.history_row_key = Some(key);
        self.history_row_height = height;
        self.history_list_state
            .reset_with_uniform_height(rows.commits.len(), height);
        if same_key && !rows.commits.is_empty() {
            anchor.item_ix = anchor.item_ix.min(rows.commits.len() - 1);
            self.history_list_state.scroll_to(anchor);
        }
    }

    /// Read the selected workspace's cached Git commit history under the
    /// current branch filter, starting one background fetch on a miss. Render
    /// only reads the in-memory cache; a miss claims a token so a second
    /// reader never starts duplicate work.
    fn commit_log_for_workspace(
        &mut self,
        workspace_path: &StdPath,
        cx: &mut Context<Self>,
    ) -> HistoryFetch {
        let key = (workspace_path.to_path_buf(), self.history_branch.clone());
        match self.commit_log.read(&key) {
            Query::Ready(result) => match result.as_ref() {
                Ok(Some(commits)) => {
                    self.sync_history_rows(key, commits);
                    HistoryFetch::Ready
                }
                Ok(None) => HistoryFetch::NotRepository,
                Err(error) => HistoryFetch::Error(error.clone()),
            },
            Query::Pending => {
                if self.history_row_key.as_ref() == Some(&key) {
                    HistoryFetch::Refreshing
                } else {
                    HistoryFetch::Loading
                }
            }
            Query::Missing(token) => {
                let fetch_path = workspace_path.to_path_buf();
                let fetch_branch = self.history_branch.clone();
                let previous = (self.history_row_key.as_ref() == Some(&key))
                    .then(|| self.history_row_cache.clone());
                let workspace = fintwind_client::WorkspaceClient::new(self.daemon.client());
                cx.spawn(async move |fintwind, cx| {
                    let result = cx
                        .background_executor()
                        .spawn({
                            let fetch_path = fetch_path.clone();
                            let fetch_branch = fetch_branch.clone();
                            async move {
                                match workspace.request(
                                    fintwind_client::WorkspaceOperation::ListCommits {
                                        cwd: fetch_path,
                                        limit: HISTORY_COMMIT_LIMIT,
                                        branch: fetch_branch,
                                    },
                                ) {
                                    Ok(fintwind_client::WorkspaceResult::Commits { commits }) => {
                                        Ok(commits.map(|commits| {
                                            // Compare only once, off-thread. An unchanged refresh
                                            // keeps both the snapshot identity and scroll state.
                                            if let Some(previous) = previous
                                                .filter(|previous| previous.commits == commits)
                                            {
                                                previous
                                            } else {
                                                Arc::new(HistoryRows::new(commits))
                                            }
                                        }))
                                    }
                                    Ok(_) => Err("the daemon returned an invalid history response"
                                        .to_owned()),
                                    Err(error) => Err(error.to_string()),
                                }
                            }
                        })
                        .await;
                    let _ = fintwind.update(cx, |fintwind, cx| {
                        if !fintwind.commit_log.fulfill(token, result) {
                            return;
                        }
                        // Only the visible panel rebuilds; an answer for
                        // another workspace or filter waits until shown.
                        let selected = fintwind
                            .selected_workspace_path()
                            .is_some_and(|path| path == fetch_path)
                            && fintwind.history_branch == fetch_branch;
                        if selected {
                            cx.notify();
                        }
                    });
                })
                .detach();
                if self.history_row_key.as_ref() == Some(&key) {
                    HistoryFetch::Refreshing
                } else {
                    HistoryFetch::Loading
                }
            }
        }
    }

    pub(super) fn refresh_history_panel(&mut self, cx: &mut Context<Self>) {
        // Only the visible (workspace, branch) answer is stale; other cached
        // histories keep serving until their own panel asks again.
        if let Some(path) = self.selected_workspace_path().map(StdPath::to_path_buf) {
            self.commit_log
                .invalidate(&(path, self.history_branch.clone()));
        }
        // Retry failed detail reads too; cached failures otherwise stay quiet
        // instead of spawning a request on every frame.
        self.history_stats.clear();
        cx.notify();
    }

    pub(super) fn set_history_branch_filter(
        &mut self,
        branch: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.history_branch == branch {
            return;
        }
        self.history_branch = branch;
        self.history_highlight = None;
        self.commit_log.clear();
        cx.notify();
    }

    fn move_history_highlight(&mut self, key: &str, cx: &mut Context<Self>) {
        let rows = self.history_row_cache.commits.len();
        if rows == 0 {
            return;
        }
        let current = self.history_highlight.filter(|index| *index < rows);
        let next = match (key, current) {
            ("up", Some(0)) => rows - 1,
            ("up", Some(index)) => index - 1,
            ("up", None) => rows - 1,
            ("home", _) => 0,
            ("end", _) => rows - 1,
            (_, Some(index)) => (index + 1) % rows,
            (_, None) => 0,
        };
        self.history_highlight = Some(next);
        self.history_details_scroll
            .set_offset(point(px(0.0), px(0.0)));
        self.history_list_state.scroll_to_reveal_item(next);
        cx.notify();
    }

    /// Only the explicit copy control writes to the clipboard.
    fn copy_history_hash(&mut self, hash: &str, short_hash: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(hash.to_owned()));
        self.show_success_toast(tr!("git_history.hash_copied", hash = short_hash));
        cx.notify();
    }

    fn selected_commit_stats(
        &mut self,
        hash: &str,
        cx: &mut Context<Self>,
    ) -> Option<Result<fintwind_protocol::git::CommitStats, String>> {
        let path = self.selected_workspace_path()?.to_path_buf();
        let key = (path.clone(), hash.to_owned());
        match self.history_stats.read(&key) {
            Query::Ready(result) => Some(result.as_ref().clone()),
            Query::Pending => None,
            Query::Missing(token) => {
                let workspace = fintwind_client::WorkspaceClient::new(self.daemon.client());
                let hash = hash.to_owned();
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            match workspace.request(
                                fintwind_client::WorkspaceOperation::ReadCommitStats {
                                    cwd: path,
                                    hash,
                                },
                            ) {
                                Ok(fintwind_client::WorkspaceResult::CommitStats { stats }) => {
                                    Ok(stats)
                                }
                                Ok(_) => {
                                    Err("the daemon returned an invalid commit stats response"
                                        .to_owned())
                                }
                                Err(error) => Err(error.to_string()),
                            }
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this.history_stats.fulfill(token, result) {
                            cx.notify();
                        }
                    });
                })
                .detach();
                None
            }
        }
    }

    pub(super) fn render_right_panel_history(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let workspace_path = self.selected_workspace_path().map(StdPath::to_path_buf);
        let branches: Vec<(String, bool)> = workspace_path
            .as_deref()
            .and_then(|path| {
                self.visible_branch_snapshot
                    .as_ref()
                    .filter(|(snapshot_path, _)| snapshot_path == path)
                    .map(|(_, snapshot)| snapshot)
            })
            .map(|snapshot| {
                snapshot
                    .branches
                    .iter()
                    .map(|branch| (branch.name.clone(), branch.checked_out_elsewhere))
                    .collect()
            })
            .unwrap_or_default();
        let branch_filter = self.history_branch.clone();
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("history-branch-filter", cx);
        let filter_label = branch_filter
            .clone()
            .unwrap_or_else(|| tr!("git_history.all_refs"));
        let menu_weak = weak.clone();
        let filter = dropdown_menu(
            MenuChip::new("history-branch-filter")
                .label(filter_label)
                .icon("icons/git-branch.svg", theme.text_tertiary)
                .height(px(28.0))
                .background(theme.surface)
                .selected(handle.is_open()),
            "history-branch-filter-menu",
            &handle,
            MenuAlign::BelowLeft,
            move |_| {
                let mut items = Vec::new();
                let head_weak = menu_weak.clone();
                items.push(
                    MenuItem::new(tr!("git_history.all_refs"), move |_, cx| {
                        let _ = head_weak.update(cx, |this, cx| {
                            this.set_history_branch_filter(None, cx);
                        });
                    })
                    .selected(branch_filter.is_none()),
                );
                items.push(MenuItem::Separator);
                // Reading another worktree's branch history needs no
                // checkout, so every branch stays selectable here.
                for (name, _checked_out_elsewhere) in branches.iter() {
                    let choice_weak = menu_weak.clone();
                    let choice = Some(name.clone());
                    items.push(
                        MenuItem::new(name.clone(), move |_, cx| {
                            let _ = choice_weak.update(cx, |this, cx| {
                                this.set_history_branch_filter(choice.clone(), cx);
                            });
                        })
                        .selected(branch_filter.as_deref() == Some(name.as_str())),
                    );
                }
                items
            },
        );

        let mut fetch = match workspace_path.as_deref() {
            Some(path) => self.commit_log_for_workspace(path, cx),
            None => HistoryFetch::NotRepository,
        };
        let refresh_error = match (&fetch, workspace_path.as_ref()) {
            (HistoryFetch::Error(error), Some(path))
                if self.history_row_key.as_ref()
                    == Some(&(path.clone(), self.history_branch.clone())) =>
            {
                Some(error.clone())
            }
            _ => None,
        };
        if refresh_error.is_some() {
            fetch = HistoryFetch::Ready;
        }
        let request_running = matches!(fetch, HistoryFetch::Loading | HistoryFetch::Refreshing);
        let loading_phase = if matches!(fetch, HistoryFetch::Error(_)) || refresh_error.is_some() {
            self.history_loading = None;
            LoadingPhase::Done
        } else if let Some(path) = workspace_path.as_ref() {
            let (phase, wake) = LoadingDelay::observe(
                &mut self.history_loading,
                (path.clone(), self.history_branch.clone()),
                matches!(fetch, HistoryFetch::Loading),
            );
            if let Some(wake) = wake {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wake.delay()).await;
                    wake.fired();
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }
            phase
        } else {
            self.history_loading = None;
            LoadingPhase::Done
        };
        match loading_phase {
            LoadingPhase::Hidden => fetch = HistoryFetch::Waiting,
            LoadingPhase::Visible => fetch = HistoryFetch::Placeholder,
            LoadingPhase::Done => {}
        }
        let refreshing = matches!(fetch, HistoryFetch::Refreshing);
        if self.history_refresh_spin.as_mut().is_some_and(|spin| {
            (!request_running && cx.reduce_motion()) || !spin.observe(request_running)
        }) {
            self.history_refresh_spin = None;
        }
        let refresh_focus = self.transcript_control_focus("history-refresh", cx);
        let refresh = div()
            .id("history-refresh")
            .track_focus(&refresh_focus)
            .tab_index(0)
            .w(px(28.0))
            .h(px(28.0))
            .flex_none()
            .rounded(px(7.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay))
            .active(|style| style.bg(theme.overlay_strong))
            .child(if let Some(spin) = &self.history_refresh_spin {
                if cx.reduce_motion() {
                    icon("icons/rotate-cw.svg", 14.0, theme.text_secondary).into_any_element()
                } else {
                    spin.render(icon("icons/rotate-cw.svg", 14.0, theme.text_secondary))
                }
            } else if refreshing {
                motion::spin_slow(icon("icons/rotate-cw.svg", 14.0, theme.text_secondary))
            } else {
                icon("icons/rotate-cw.svg", 14.0, theme.text_secondary).into_any_element()
            })
            .tooltip(Tooltip::text(tr!("git_history.refresh")))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_activation(cx, move |this, _, cx| {
                if !request_running {
                    this.history_refresh_spin = Some(motion::ActionSpin::start());
                    this.refresh_history_panel(cx);
                }
            });

        let toolbar = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(8.0))
            .child(filter)
            .child(div().flex_1())
            .child(refresh);

        let highlight = self
            .history_highlight
            .filter(|index| *index < self.history_row_cache.commits.len());

        let content = match fetch {
            HistoryFetch::Waiting | HistoryFetch::Loading => {
                div().flex_1().min_h_0().into_any_element()
            }
            HistoryFetch::Placeholder => {
                let block = || div().rounded(px(3.0)).bg(theme.overlay_strong);
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .children((0..5).map(|index| {
                        div()
                            .h(ui_px(HISTORY_ROW_HEIGHT))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(block().size(px(12.0)))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap(px(7.0))
                                    .child(block().h(px(10.0)).w(relative(if index % 2 == 0 {
                                        0.7
                                    } else {
                                        0.55
                                    })))
                                    .child(block().h(px(8.0)).w(relative(0.35))),
                            )
                    }))
                    .into_any_element()
            }
            HistoryFetch::Ready | HistoryFetch::Refreshing
                if self.history_row_cache.commits.is_empty() =>
            {
                self.render_right_panel_empty_message(
                    tr!("git_history.empty"),
                    tr!("git_history.empty_description"),
                    cx,
                )
                .into_any_element()
            }
            HistoryFetch::Ready | HistoryFetch::Refreshing => {
                let list_weak = weak;
                let list_highlight = highlight;
                let rows = self.history_row_cache.clone();
                let row_height = self.history_row_height;
                let now = unix_time();
                let details = highlight
                    .and_then(|index| self.history_row_cache.commits.get(index))
                    .cloned();
                let details = self.render_history_selection(details.as_ref(), cx);
                div()
                    .id("history-list")
                    .track_focus(&self.transcript_control_focus("history-list", cx))
                    .tab_index(0)
                    .key_context("GitHistoryList")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.modifiers.modified() {
                            return;
                        }
                        match event.keystroke.key.as_str() {
                            "up" | "down" | "home" | "end" => {
                                this.move_history_highlight(event.keystroke.key.as_str(), cx);
                                cx.stop_propagation();
                            }
                            _ => {}
                        }
                    }))
                    .child(
                        list(
                            self.history_list_state.clone(),
                            move |index, _window, _cx| {
                                let Some(commit) = rows.commits.get(index) else {
                                    return div().into_any_element();
                                };
                                let selected = list_highlight == Some(index);
                                div()
                                    .id(SharedString::from(format!("history-row-{index}")))
                                    .h(row_height)
                                    .w_full()
                                    .overflow_hidden()
                                    .pl(px(8.0))
                                    .pr(px(12.0))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .cursor_default()
                                    .when(selected, |row| row.bg(theme.overlay_strong))
                                    .when(!selected, |row| {
                                        row.hover(|style| style.bg(theme.overlay))
                                    })
                                    .child(graph_cell(
                                        rows.graph.rows[index].clone(),
                                        rows.graph.width,
                                        row_height,
                                        commit.is_head,
                                        commit.parents.len() > 1,
                                        selected,
                                        theme,
                                    ))
                                    .child(commit_details(commit, index, now, theme))
                                    .on_click({
                                        let weak = list_weak.clone();
                                        move |_, _, cx| {
                                            cx.stop_propagation();
                                            let _ = weak.update(cx, |this, cx| {
                                                // The clicked row becomes the
                                                // keyboard cursor, so mouse and
                                                // arrow-key selection share one
                                                // state.
                                                this.history_highlight = Some(index);
                                                this.history_details_scroll
                                                    .set_offset(point(px(0.0), px(0.0)));
                                                cx.notify();
                                            });
                                        }
                                    })
                                    .into_any_element()
                            },
                        )
                        .flex_1()
                        .min_h_0()
                        .w_full(),
                    )
                    .child(details)
                    .into_any_element()
            }
            HistoryFetch::NotRepository => self
                .render_right_panel_empty_message(
                    tr!("git_history.not_a_repository"),
                    tr!("git_history.not_a_repository_description"),
                    cx,
                )
                .into_any_element(),
            HistoryFetch::Error(error) => {
                let retry_focus = self.transcript_control_focus("history-error-retry", cx);
                self.render_right_panel_empty_message(tr!("git_history.error"), error, cx)
                    .child(
                        div()
                            .id("history-error-retry")
                            .track_focus(&retry_focus)
                            .tab_index(0)
                            .mt(px(12.0))
                            .px(px(12.0))
                            .h(px(28.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(px(6.0))
                            .cursor_pointer()
                            .text_size(ui_px(12.0))
                            .text_color(theme.text_secondary)
                            .border_1()
                            .border_color(theme.border)
                            .hover(|style| style.bg(theme.overlay))
                            .active(|style| style.bg(theme.overlay_strong))
                            .focus_visible(|style| style.border_color(theme.accent))
                            .child(icon("icons/rotate-cw.svg", 14.0, theme.text_secondary))
                            .child(tr!("git_history.refresh"))
                            .tooltip(Tooltip::text(tr!("git_history.refresh")))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_activation(cx, move |this, _, cx| {
                                this.history_refresh_spin =
                                    Some(motion::ActionSpin::start());
                                this.refresh_history_panel(cx);
                            }),
                    )
                    .into_any_element()
            }
        };

        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .when_some(refresh_error, |content, error| {
                content.child(
                    div()
                        .px(px(10.0))
                        .pb(px(6.0))
                        .text_size(ui_px(11.0))
                        .text_color(theme.warning_text)
                        .child(error),
                )
            })
            .child(content)
    }

    /// Full names remain readable without hover. Tab from the list to this
    /// scrollable detail area; arrows scroll, Escape returns to the list.
    fn render_history_selection(
        &mut self,
        commit: Option<&CommitEntry>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let focus = self.transcript_control_focus("history-selected-details", cx);
        let stats = commit.and_then(|commit| self.selected_commit_stats(&commit.hash, cx));
        let copy_button = commit.map(|commit| {
            let hash = commit.hash.clone();
            let short_hash = commit.short_hash.clone();
            let focus = self.transcript_control_focus("history-copy-sha", cx);
            crate::ui::icon_button("history-copy-sha", "icons/copy.svg", theme)
                .flex_none()
                .track_focus(&focus)
                .tab_index(0)
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .tooltip(Tooltip::text(tr!("git_history.copy_hash")))
                .on_activation(cx, move |this, _, cx| {
                    this.copy_history_hash(&hash, &short_hash, cx);
                })
        });
        let review_button = commit.map(|commit| {
            let hash = commit.hash.clone();
            let focus = self.transcript_control_focus("history-review-commit", cx);
            crate::ui::icon_button("history-review-commit", "icons/file-diff.svg", theme)
                .flex_none()
                .track_focus(&focus)
                .tab_index(0)
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .tooltip(Tooltip::text(tr!("git_history.view_changes")))
                .on_activation(cx, move |this, _, cx| {
                    this.open_commit_review(hash.clone(), cx);
                })
        });
        div()
            .id("history-selected-details")
            .track_focus(&focus)
            .tab_index(0)
            .flex_none()
            .min_h_0()
            // Reserve the same viewport before and after first selection:
            // End/Up reveal uses the last measured list bounds.
            .h(ui_px(140.0))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .overflow_x_scroll()
            .track_scroll(&self.history_details_scroll)
            .border_t_1()
            .border_color(theme.border)
            .focus_visible(|style| style.border_color(theme.accent))
            .px(px(10.0))
            .py(px(8.0))
            .text_size(ui_px(11.0))
            .line_height(ui_px(18.0))
            .text_color(theme.text_secondary)
            .when(commit.is_none(), |details| {
                details.child(div().flex_none().child(tr!("git_history.select_commit")))
            })
            .when_some(commit, |details, commit| {
                details
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.text)
                            .child(commit.subject.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(crate::theme::code_font_family())
                                    .child(commit.hash.clone()),
                            )
                            .children(copy_button)
                            .children(review_button),
                    )
                    .child(match stats {
                        Some(Ok(stats)) => div()
                            .flex_none()
                            .flex()
                            .gap(px(8.0))
                            .child(tr!("git_history.files_changed", count = stats.files))
                            .child(
                                div()
                                    .text_color(theme.success)
                                    .child(format!("+{}", stats.additions)),
                            )
                            .child(
                                div()
                                    .text_color(theme.danger_text)
                                    .child(format!("−{}", stats.deletions)),
                            )
                            .into_any_element(),
                        Some(Err(error)) => div()
                            .id("history-stats-error")
                            .flex_none()
                            .text_color(theme.warning_text)
                            .tooltip(Tooltip::text(error))
                            .child(tr!("git_history.stats_error"))
                            .into_any_element(),
                        None => div()
                            .flex_none()
                            .h(ui_px(18.0))
                            .into_any_element(),
                    })
                    .child(div().flex_none().child(commit.author.clone()))
                    .children(
                        commit
                            .refs
                            .iter()
                            .map(|commit_ref| div().flex_none().child(commit_ref.label.clone())),
                    )
                    .when(!commit.pushed, |details| {
                        details.child(div().flex_none().child(tr!("git_history.unpushed")))
                    })
            })
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let scroll = &this.history_details_scroll;
                let max = scroll.max_offset();
                let mut offset = scroll.offset();
                let step = ui_px(24.0);
                match event.keystroke.key.as_str() {
                    "up" => offset.y += step,
                    "down" => offset.y -= step,
                    "left" => offset.x += step,
                    "right" => offset.x -= step,
                    "home" => offset = point(px(0.0), px(0.0)),
                    "end" => offset.y = -max.y,
                    "escape" => {
                        window.focus(&this.transcript_control_focus("history-list", cx), cx);
                        cx.stop_propagation();
                        return;
                    }
                    _ => return,
                }
                scroll.set_offset(point(
                    offset.x.clamp(-max.x, px(0.0)),
                    offset.y.clamp(-max.y, px(0.0)),
                ));
                cx.stop_propagation();
                cx.notify();
            }))
    }
}

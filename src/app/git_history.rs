use crate::git_history::{CommitEntry, CommitGraph, GraphHalf};
use crate::theme::{code_px, ui_px};
use crate::ui::ActivationExt;
use gpui::{PathBuilder, relative, size};

use std::path::Path as StdPath;

use super::sidebar::format_time_ago;
use super::*;

const HISTORY_ROW_HEIGHT: f32 = 60.0;
/// How many commits one history read asks the daemon for.
const HISTORY_COMMIT_LIMIT: usize = 500;

pub(super) type HistoryKey = (PathBuf, Option<String>);
pub(super) type HistoryResult = Result<Option<Arc<HistoryRows>>, String>;

/// Prepared off-thread once per fetch. Frames clone an Arc, never traverse the
/// complete history or recompute its topology.
#[derive(Default)]
pub(super) struct HistoryRows {
    commits: Vec<CommitEntry>,
    graph: CommitGraph,
    positions: HashMap<String, usize>,
}

impl HistoryRows {
    fn new(commits: Vec<CommitEntry>) -> Self {
        let graph = CommitGraph::new(&commits);
        let positions = commits
            .iter()
            .enumerate()
            .map(|(index, commit)| (commit.hash.clone(), index))
            .collect();
        Self {
            commits,
            graph,
            positions,
        }
    }
}

/// What the history panel has to draw for the selected workspace: either the
/// synced row cache (the commit list) or one of the empty states that must
/// stay distinguishable (not a repository / daemon error / fetch in flight).
/// Keyboard and click handlers read the same prepared snapshot as the list.
enum HistoryFetch {
    Ready,
    NotRepository,
    Error(String),
    Loading,
}

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
            Query::Pending => HistoryFetch::Loading,
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
                HistoryFetch::Loading
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

    /// Copies one row's full hash and confirms with a toast. Both the mouse
    /// click and the keyboard path land here, reading the synced row cache so
    /// neither path needs to carry the list around.
    fn copy_history_hash(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(commit) = self.history_row_cache.commits.get(index) else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(commit.hash.clone()));
        self.show_success_toast(tr!(
            "git_history.hash_copied",
            hash = commit.short_hash.clone()
        ));
        cx.notify();
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
            .unwrap_or_else(|| tr!("git_history.head"));
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
                    MenuItem::new(tr!("git_history.head"), move |_, cx| {
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
            .child(icon("icons/rotate-cw.svg", 14.0, theme.text_secondary))
            .tooltip(Tooltip::text(tr!("git_history.refresh")))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_activation(cx, |this, _, cx| this.refresh_history_panel(cx));

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

        let fetch = match workspace_path.as_deref() {
            Some(path) => self.commit_log_for_workspace(path, cx),
            None => HistoryFetch::NotRepository,
        };
        let highlight = self
            .history_highlight
            .filter(|index| *index < self.history_row_cache.commits.len());

        let content = match fetch {
            HistoryFetch::Ready if self.history_row_cache.commits.is_empty() => self
                .render_right_panel_empty_message(
                    tr!("git_history.empty"),
                    tr!("git_history.empty_description"),
                    cx,
                )
                .into_any_element(),
            HistoryFetch::Ready => {
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
                            "enter" | "space" => {
                                if let Some(index) = this.history_highlight {
                                    this.copy_history_hash(index, cx);
                                }
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
                                                this.copy_history_hash(index, cx)
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
            HistoryFetch::Error(error) => self
                .render_right_panel_empty_message(tr!("git_history.error"), error, cx)
                .into_any_element(),
            HistoryFetch::Loading => self
                .render_right_panel_empty_message(
                    tr!("git_history.loading"),
                    tr!("git_history.loading_description"),
                    cx,
                )
                .into_any_element(),
        };

        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(toolbar)
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
        div()
            .id("history-selected-details")
            .track_focus(&focus)
            .tab_index(0)
            .flex_none()
            .min_h_0()
            // Reserve the same viewport before and after first selection:
            // End/Up reveal uses the last measured list bounds.
            .h(ui_px(112.0))
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
                            .font_family(crate::theme::code_font_family())
                            .child(commit.hash.clone()),
                    )
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

/// Ely-style lane cell, painted independently for each virtualized row. The
/// shared span preserves connections even when neighboring rows are unmounted.
fn graph_cell(
    row: crate::git_history::GraphRow,
    span: usize,
    height: Pixels,
    is_head: bool,
    merge: bool,
    selected: bool,
    theme: Theme,
) -> AnyElement {
    let palette = [
        theme.gauge,
        theme.success,
        theme.code_keyword,
        theme.warning,
        theme.link,
        theme.accent,
    ];
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let step = bounds.size.width / span.max(1) as f32;
            let x = |lane: usize| bounds.origin.x + step * (lane as f32 + 0.5);
            let top = bounds.origin.y;
            let middle = top + bounds.size.height / 2.0;
            let bottom = top + bounds.size.height;
            for stroke in &row.strokes {
                let (from, to) = match stroke.half {
                    GraphHalf::Top => (point(x(stroke.from), top), point(x(stroke.to), middle)),
                    GraphHalf::Bottom => {
                        (point(x(stroke.from), middle), point(x(stroke.to), bottom))
                    }
                    GraphHalf::Through => (point(x(stroke.from), top), point(x(stroke.to), bottom)),
                };
                let mut path = PathBuilder::stroke(px(1.5));
                path.move_to(from);
                path.line_to(to);
                if let Ok(path) = path.build() {
                    window.paint_path(path, palette[stroke.from.max(stroke.to) % palette.len()]);
                }
            }
            let center = point(x(row.lane), middle);
            let dot = step.min(px(16.0)) * 0.45;
            let color = palette[row.lane % palette.len()];
            let paint_dot = |window: &mut Window, diameter: Pixels, color: Hsla| {
                window.paint_quad(
                    fill(
                        Bounds::new(
                            center - point(diameter / 2.0, diameter / 2.0),
                            size(diameter, diameter),
                        ),
                        color,
                    )
                    .corner_radii(diameter / 2.0),
                );
            };
            if is_head || selected {
                paint_dot(window, dot + px(5.0), color.opacity(0.22));
            }
            paint_dot(window, dot, color);
            if merge {
                paint_dot(window, dot * 0.45, theme.surface);
            }
        },
    )
    .flex_none()
    .w(px(16.0 * span.max(1) as f32))
    // Reserve room for the subject in a narrow panel; compress lanes together
    // instead of clipping edges or silently discarding branches.
    .max_w(relative(0.35))
    .h(height)
    .into_any_element()
}

fn commit_details(commit: &CommitEntry, index: usize, now: u64, theme: Theme) -> Div {
    let mut chips = div()
        .flex_none()
        .max_w(relative(0.5))
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(4.0));
    let shown = commit.refs.len().min(2);
    for (ref_index, commit_ref) in commit.refs.iter().take(shown).enumerate() {
        chips = chips.child(ref_chip(
            format!("history-ref-{index}-{ref_index}"),
            commit_ref.label.clone(),
            commit_ref.remote,
            commit_ref.head,
            &theme,
            None,
        ));
    }
    if commit.refs.len() > shown {
        chips = chips.child(ref_chip(
            format!("history-ref-{index}-overflow"),
            format!("+{}", commit.refs.len() - shown),
            false,
            false,
            &theme,
            Some(
                commit
                    .refs
                    .iter()
                    .skip(shown)
                    .map(|r| r.label.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        ));
    }
    let title = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .min_w_0()
        .line_height(ui_px(20.0))
        .child(
            div()
                .id(SharedString::from(format!("history-subject-{index}")))
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(ui_px(12.5))
                .font_weight(if commit.is_head {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(theme.text)
                .tooltip(Tooltip::text(commit.subject.clone()))
                .child(SharedString::from(commit.subject.clone())),
        )
        .when(!commit.refs.is_empty(), |title| title.child(chips))
        .when(!commit.pushed, |title| {
            title.child(
                div()
                    .id(SharedString::from(format!("history-unpushed-{index}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .tooltip(Tooltip::text(tr!("git_history.unpushed")))
                    .child(icon("icons/cloud-upload.svg", 12.0, theme.warning)),
            )
        });
    let metadata = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .min_w_0()
        .line_height(ui_px(18.0))
        .text_size(ui_px(10.5))
        .text_color(theme.text_tertiary)
        .child(
            div()
                .flex_none()
                .text_size(code_px(10.5))
                .font_family(crate::theme::code_font_family())
                .text_color(if commit.is_head {
                    theme.accent_text
                } else {
                    theme.text_secondary
                })
                .child(SharedString::from(commit.short_hash.clone())),
        )
        .child(
            div()
                .id(SharedString::from(format!("history-author-{index}")))
                .flex_1()
                .min_w_0()
                .truncate()
                .tooltip(Tooltip::text(commit.author.clone()))
                .child(SharedString::from(commit.author.clone())),
        )
        .child(
            div()
                .flex_none()
                .child(format_time_ago(now.saturating_sub(commit.timestamp))),
        );
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(title)
        .child(metadata)
}

/// Ref decorations stay bounded so long branch names do not consume a row.
fn ref_chip(
    id: String,
    label: String,
    remote: bool,
    is_head: bool,
    theme: &Theme,
    tooltip: Option<String>,
) -> Stateful<Div> {
    let color = if is_head {
        theme.accent
    } else {
        theme.text_tertiary
    };
    div()
        .id(SharedString::from(id))
        .flex_none()
        .max_w(ui_px(104.0))
        .overflow_hidden()
        .px(px(5.0))
        .h(ui_px(16.0))
        .rounded(px(4.0))
        .flex()
        .items_center()
        .when(remote, |chip| {
            chip.gap(px(3.0)).child(icon("icons/globe.svg", 9.0, color))
        })
        .tooltip(Tooltip::text(tooltip.unwrap_or_else(|| {
            if remote {
                format!("{label}\n{}", tr!("git_history.remote_ref"))
            } else {
                label.clone()
            }
        })))
        .text_size(ui_px(10.0))
        .text_color(color)
        .bg(theme.overlay)
        .child(div().min_w_0().truncate().child(SharedString::from(label)))
}

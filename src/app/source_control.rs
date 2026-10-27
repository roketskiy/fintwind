use crate::theme::{code_px, ui_px};
use crate::ui::ActivationExt;

use gpui::{Context, Div, IntoElement, Window, div, prelude::*, px};

use super::right_panel::file_icon_for_path;
use super::sidebar::localized_session_title;
use super::*;

pub(super) struct PendingGitQuery {
    id: Uuid,
    client: fintwind_client::WorkspaceClient,
}

/// Line totals for every file one review source covers. The toolbar capsule
/// shows this; the diff snapshot beside it covers only the selected file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ReviewAreaStats {
    pub additions: u64,
    pub deletions: u64,
    pub files: u64,
    /// True when the daemon stopped its untracked scan at a budget, so the
    /// totals under-report.
    pub truncated: bool,
}

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
    pub(super) fn open_review_page(&mut self, source: ReviewDiffSource, cx: &mut Context<Self>) {
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
        self.source_control_diff_context = 3;
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
        self.cancel_source_control_file_diff(cx);
        let status_request = self.source_control_status_request.take();
        self.cancel_source_control_query(status_request, cx);
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            self.cancel_nested_repository_queries(cx);
            let stats_request = self.source_control_stats_request.take();
            self.cancel_source_control_query(stats_request, cx);
            self.source_control_stats_generation =
                self.source_control_stats_generation.wrapping_add(1);
            self.source_control_stats = None;
            self.source_control_stats_key = None;
            self.source_control_stats_dirty = false;
            let untracked_request = self.source_control_untracked_request.take();
            let ignored_request = self.source_control_ignored_request.take();
            self.cancel_source_control_query(untracked_request, cx);
            self.cancel_source_control_query(ignored_request, cx);
            self.source_control_status = None;
            self.source_control_status_workspace = None;
            self.source_control_branch_generation =
                self.source_control_branch_generation.wrapping_add(1);
            self.source_control_branches = None;
            self.source_control_status_inflight = None;
            self.source_control_status_pending = false;
            self.source_control_nested_generation =
                self.source_control_nested_generation.wrapping_add(1);
            self.source_control_nested_candidates = None;
            self.source_control_nested_complete = false;
            self.source_control_nested_refreshed_at = None;
            self.source_control_nested.clear();
            self.source_control_review_files = SourceControlReviewFiles::default();
            self.source_control_untracked_generation =
                self.source_control_untracked_generation.wrapping_add(1);
            self.source_control_untracked_loading = false;
            self.source_control_untracked_complete = false;
            self.source_control_untracked_ready = false;
            self.source_control_untracked_loaded.clear();
            self.source_control_untracked_next.clear();
            self.source_control_untracked_incomplete.clear();
            self.source_control_pending_untracked_directories.clear();
            self.source_control_ignored_generation =
                self.source_control_ignored_generation.wrapping_add(1);
            self.source_control_ignored_loading = false;
            self.source_control_ignored = None;
            self.source_control_ignored_loaded.clear();
            self.source_control_ignored_next.clear();
            self.source_control_ignored_incomplete.clear();
            self.source_control_status_loading = false;
            self.source_control_status_error = None;
            self.sync_source_control_rows(cx);
            cx.notify();
            return;
        };

        if self
            .source_control_status_inflight
            .as_ref()
            .is_some_and(|(path, _)| path == &workspace)
        {
            self.source_control_status_pending = true;
            self.right_panel_diff_generation = self.right_panel_diff_generation.wrapping_add(1);
            self.refresh_review_stats(cx);
            return;
        }
        self.refresh_review_stats(cx);
        self.source_control_status_pending = false;
        self.source_control_nested_candidates = None;
        let untracked_request = self.source_control_untracked_request.take();
        let ignored_request = self.source_control_ignored_request.take();
        self.cancel_source_control_query(untracked_request, cx);
        self.cancel_source_control_query(ignored_request, cx);
        let nested_list_request = self.source_control_nested_list_request.take();
        if nested_list_request.is_some() {
            self.source_control_nested_list_generation =
                self.source_control_nested_list_generation.wrapping_add(1);
        }
        self.cancel_source_control_query(nested_list_request, cx);

        if self.source_control_status_workspace.as_deref() == Some(workspace.as_path()) {
            let mut requests = Vec::new();
            invalidate_nested_requests(&mut self.source_control_nested, &mut requests);
            for request in requests {
                self.cancel_source_control_query(Some(request), cx);
            }
            let folded = self.source_control_folded_directories.borrow();
            let mut directories = folded
                .iter()
                .filter_map(|key| key.strip_prefix("untracked:").map(str::to_owned))
                .filter(|directory| {
                    directory.match_indices('/').all(|(index, _)| {
                        folded.contains(&source_control_directory_key(
                            SourceControlGroup::Untracked,
                            &directory[..index],
                        ))
                    })
                })
                .collect::<Vec<_>>();
            drop(folded);
            directories.sort_by(|a, b| {
                a.matches('/')
                    .count()
                    .cmp(&b.matches('/').count())
                    .then_with(|| a.cmp(b))
            });
            self.source_control_pending_untracked_directories = directories.into();
        } else {
            self.source_control_pending_untracked_directories.clear();
        }

        if self.source_control_status_workspace.as_deref() != Some(workspace.as_path()) {
            self.cancel_nested_repository_queries(cx);
            self.source_control_status = None;
            self.source_control_branch_generation =
                self.source_control_branch_generation.wrapping_add(1);
            self.source_control_branches = None;
            self.source_control_review_files = SourceControlReviewFiles::default();
            self.source_control_nested_generation =
                self.source_control_nested_generation.wrapping_add(1);
            self.source_control_nested_list_generation =
                self.source_control_nested_list_generation.wrapping_add(1);
            self.source_control_nested_complete = false;
            self.source_control_nested_refreshed_at = None;
            self.source_control_nested.clear();
            self.right_panel_diff_generation = self.right_panel_diff_generation.wrapping_add(1);
            self.right_panel_diff_snapshot = None;
            self.right_panel_diff_selected_file = None;
            self.review_pending_file_focus = None;
            self.source_control_ignored_open = false;
            self.source_control_diff_context = 3;
        } else if self
            .source_control_review_files
            .for_source(self.right_panel_diff_source)
            .is_some()
        {
            if self.review_pending_file_focus.is_none() {
                self.review_pending_file_focus =
                    self.right_panel_diff_selected_file.and_then(|index| {
                        self.right_panel_diff_snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.files.get(index))
                            .map(|file| file.path.clone())
                    });
            }
            self.right_panel_diff_generation = self.right_panel_diff_generation.wrapping_add(1);
        }
        self.source_control_status_workspace = Some(workspace.clone());

        self.source_control_untracked_generation =
            self.source_control_untracked_generation.wrapping_add(1);
        self.source_control_untracked_loading = false;
        self.source_control_untracked_complete = false;
        self.source_control_untracked_ready = false;
        self.source_control_untracked_error = None;
        self.source_control_untracked_loaded.clear();
        self.source_control_untracked_next.clear();
        self.source_control_untracked_incomplete.clear();

        self.source_control_ignored_generation =
            self.source_control_ignored_generation.wrapping_add(1);
        self.source_control_ignored = None;
        self.source_control_ignored_loaded.clear();
        self.source_control_ignored_next.clear();
        self.source_control_ignored_incomplete.clear();
        self.source_control_ignored_loading = false;
        self.source_control_ignored_error = None;
        if self.source_control_ignored_open {
            self.refresh_source_control_ignored(None, None, cx);
        }

        self.source_control_status_generation =
            self.source_control_status_generation.wrapping_add(1);
        let generation = self.source_control_status_generation;
        let query_id = Uuid::new_v4();
        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        self.source_control_status_loading = true;
        self.source_control_status_inflight = Some((workspace.clone(), generation));
        self.source_control_status_request = Some(PendingGitQuery {
            id: query_id,
            client: client.clone(),
        });
        self.source_control_status_error = None;
        cx.notify();

        cx.spawn(async move |fintwind, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let client = client.clone();
                    let workspace = workspace.clone();
                    async move {
                        client.request(fintwind_client::WorkspaceOperation::InspectTrackedStatus {
                            cwd: workspace,
                            query_id: Some(query_id),
                        })
                    }
                })
                .await;
            fintwind
                .update(cx, |fintwind, cx| {
                    if fintwind
                        .source_control_status_request
                        .as_ref()
                        .is_some_and(|request| request.id == query_id)
                    {
                        let request = fintwind.source_control_status_request.take();
                        if result.is_err() {
                            fintwind.cancel_source_control_query(request, cx);
                        }
                    }
                    let still_current = fintwind.source_control_status_generation == generation
                        && fintwind
                            .review_workspace_path()
                            .is_some_and(|path| path == workspace);
                    if !still_current {
                        return;
                    }

                    fintwind.source_control_status_inflight = None;
                    if fintwind.source_control_status_pending {
                        fintwind.source_control_status_pending = false;
                        fintwind.source_control_status_loading = false;
                        fintwind.refresh_source_control_status(cx);
                        return;
                    }
                    fintwind.source_control_status_loading = false;
                    match result {
                        Ok(fintwind_client::WorkspaceResult::WorktreeStatus { status }) => {
                            fintwind.source_control_review_files =
                                SourceControlReviewFiles::from_status(status.as_ref());
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
                    fintwind.refresh_source_control_branches(cx);
                    if fintwind.source_control_nested.is_empty() {
                        fintwind.refresh_nested_repositories(cx);
                    }
                    if fintwind.source_control_status.is_some()
                        && fintwind.source_control_status_error.is_none()
                    {
                        fintwind.refresh_source_control_untracked(None, None, cx);
                    }
                    if fintwind.source_control_status_error.is_none()
                        && fintwind
                            .source_control_review_files
                            .for_source(fintwind.right_panel_diff_source)
                            .is_some()
                    {
                        fintwind.refresh_review_diff(cx);
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn refresh_source_control_untracked(
        &mut self,
        directory: Option<String>,
        after: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.source_control_untracked_loading {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        let generation = self.source_control_untracked_generation;
        let query_id = Uuid::new_v4();
        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        self.source_control_untracked_request = Some(PendingGitQuery {
            id: query_id,
            client: client.clone(),
        });
        self.source_control_untracked_loading = true;
        self.source_control_untracked_error = None;
        cx.spawn(async move |this, cx| {
            let request_workspace = workspace.clone();
            let request_directory = directory.clone();
            let request_after = after.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.request(fintwind_client::WorkspaceOperation::ListUntracked {
                        cwd: request_workspace,
                        directory: request_directory,
                        after: request_after,
                        query_id: Some(query_id),
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                if this
                    .source_control_untracked_request
                    .as_ref()
                    .is_some_and(|request| request.id == query_id)
                {
                    let request = this.source_control_untracked_request.take();
                    if result.is_err() {
                        this.cancel_source_control_query(request, cx);
                    }
                }
                if this.source_control_untracked_generation != generation
                    || !this
                        .review_workspace_path()
                        .is_some_and(|path| path == workspace)
                {
                    return;
                }
                this.source_control_untracked_loading = false;
                match result {
                    Ok(fintwind_client::WorkspaceResult::UntrackedFiles {
                        entries,
                        next,
                        incomplete,
                    }) => {
                        if let Some(status) = this.source_control_status.as_mut() {
                            let status = Arc::make_mut(status);
                            if directory.is_none() && after.is_none() {
                                status.entries.retain(|entry| entry.worktree_status != '?');
                            }
                            let mut seen: std::collections::HashSet<_> = status
                                .entries
                                .iter()
                                .map(|entry| entry.path.clone())
                                .collect();
                            status.entries.extend(
                                entries
                                    .into_iter()
                                    .filter(|entry| seen.insert(entry.path.clone())),
                            );
                            this.source_control_review_files =
                                SourceControlReviewFiles::from_status(Some(status));
                        }
                        this.apply_untracked_page(directory, next, incomplete);
                        if (this.review_pending_file_focus.is_some()
                            || this.right_panel_diff_snapshot.is_none())
                            && this
                                .source_control_review_files
                                .for_source(this.right_panel_diff_source)
                                .is_some()
                        {
                            this.refresh_review_diff(cx);
                        }
                        this.load_next_pending_untracked(cx);
                    }
                    Ok(_) => {
                        this.source_control_untracked_error =
                            Some(tr!("source_control.status_failed"))
                    }
                    Err(error) => this.source_control_untracked_error = Some(error.to_string()),
                }
                if let Some(error) = this.source_control_untracked_error.as_ref()
                    && this.review_pending_file_focus.is_some()
                {
                    this.right_panel_diff_loading = false;
                    if this.right_panel_diff_snapshot.is_none() {
                        this.right_panel_diff_error = Some(error.clone());
                    }
                }
                if this.source_control_untracked_error.is_some() {
                    // A failed page leaves the queue unable to advance; the
                    // next status refresh enqueues the expanded directories
                    // again, so drop the stale entries instead of retrying a
                    // failing query in a loop.
                    this.source_control_pending_untracked_directories.clear();
                    this.reload_stale_nested_repositories(cx);
                }
                this.sync_source_control_rows(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        self.sync_source_control_rows(cx);
    }

    fn refresh_source_control_ignored(
        &mut self,
        directory: Option<String>,
        after: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.source_control_ignored_loading {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        self.source_control_ignored_generation =
            self.source_control_ignored_generation.wrapping_add(1);
        let generation = self.source_control_ignored_generation;
        let query_id = Uuid::new_v4();
        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        self.source_control_ignored_request = Some(PendingGitQuery {
            id: query_id,
            client: client.clone(),
        });
        self.source_control_ignored_loading = true;
        self.source_control_ignored_error = None;
        cx.spawn(async move |this, cx| {
            let request_workspace = workspace.clone();
            let request_directory = directory.clone();
            let request_after = after.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.request(fintwind_client::WorkspaceOperation::ListIgnored {
                        cwd: request_workspace,
                        directory: request_directory,
                        after: request_after,
                        query_id: Some(query_id),
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                if this
                    .source_control_ignored_request
                    .as_ref()
                    .is_some_and(|request| request.id == query_id)
                {
                    let request = this.source_control_ignored_request.take();
                    if result.is_err() {
                        this.cancel_source_control_query(request, cx);
                    }
                }
                if this.source_control_ignored_generation != generation
                    || !this
                        .review_workspace_path()
                        .is_some_and(|path| path == workspace)
                {
                    return;
                }
                this.source_control_ignored_loading = false;
                match result {
                    Ok(fintwind_client::WorkspaceResult::IgnoredFiles {
                        entries,
                        next,
                        incomplete,
                    }) => {
                        if directory.is_none() && after.is_none() {
                            this.source_control_ignored = Some(entries);
                        } else {
                            let existing = this.source_control_ignored.get_or_insert_with(Vec::new);
                            let mut seen: std::collections::HashSet<String> =
                                existing.iter().map(|entry| entry.path.clone()).collect();
                            existing.extend(
                                entries
                                    .into_iter()
                                    .filter(|entry| seen.insert(entry.path.clone())),
                            );
                        }
                        this.apply_ignored_page(directory, next, incomplete);
                    }
                    Ok(_) => {
                        this.source_control_ignored_error =
                            Some(tr!("source_control.status_failed"))
                    }
                    Err(error) => this.source_control_ignored_error = Some(error.to_string()),
                }
                this.sync_source_control_rows(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_source_control_ignored(&mut self, cx: &mut Context<Self>) {
        self.source_control_ignored_open = !self.source_control_ignored_open;
        if !self.source_control_ignored_open {
            let request = self.source_control_ignored_request.take();
            self.cancel_source_control_query(request, cx);
            self.source_control_ignored_generation =
                self.source_control_ignored_generation.wrapping_add(1);
            self.source_control_ignored_loading = false;
        }
        if self.source_control_ignored_open && self.source_control_ignored.is_none() {
            self.refresh_source_control_ignored(None, None, cx);
        }
        self.sync_source_control_rows(cx);
        cx.notify();
    }

    fn refresh_nested_repositories(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        let Some(status) = self.source_control_status.as_ref() else {
            self.cancel_nested_repository_queries(cx);
            self.source_control_nested_list_generation =
                self.source_control_nested_list_generation.wrapping_add(1);
            self.source_control_nested_candidates = None;
            self.source_control_nested_complete = false;
            self.source_control_nested_refreshed_at = None;
            self.source_control_nested.clear();
            return;
        };
        let candidates = nested_repository_candidates(status);
        let candidates_unchanged =
            self.source_control_nested_candidates.as_ref() == Some(&candidates);
        // A scan in flight already covers the whole worktree; candidates only
        // decide whether a finished scan is worth repeating.
        if self.source_control_nested_list_request.is_some() {
            return;
        }
        // A scan that hit its budget is retried after the same quiet period, so
        // a large workspace cannot re-walk its tree on every status refresh.
        let scanned_recently = self
            .source_control_nested_refreshed_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(60));
        if candidates_unchanged && scanned_recently {
            return;
        }
        self.source_control_nested_candidates = Some(candidates.clone());
        let previous = self.source_control_nested_list_request.take();
        self.cancel_source_control_query(previous, cx);
        self.source_control_nested_list_generation =
            self.source_control_nested_list_generation.wrapping_add(1);
        let generation = self.source_control_nested_list_generation;
        let workspace_generation = self.source_control_nested_generation;
        self.source_control_nested_error = None;
        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        let query_id = Uuid::new_v4();
        self.source_control_nested_list_request = Some(PendingGitQuery {
            id: query_id,
            client: client.clone(),
        });
        cx.spawn(async move |this, cx| {
            let request_workspace = workspace.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.request(
                        fintwind_client::WorkspaceOperation::DiscoverNestedRepositories {
                            cwd: request_workspace,
                            candidates,
                            query_id: Some(query_id),
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this
                    .source_control_nested_list_request
                    .as_ref()
                    .is_some_and(|request| request.id == query_id)
                {
                    let request = this.source_control_nested_list_request.take();
                    if result.is_err() {
                        this.cancel_source_control_query(request, cx);
                    }
                }
                if this.source_control_nested_list_generation != generation
                    || this.source_control_nested_generation != workspace_generation
                    || !this
                        .review_workspace_path()
                        .is_some_and(|path| path == workspace)
                {
                    return;
                }
                let mut removed_requests = Vec::new();
                match result {
                    Ok(fintwind_client::WorkspaceResult::NestedRepositories {
                        repositories,
                        complete,
                    }) => {
                        let old = std::mem::take(&mut this.source_control_nested);
                        this.source_control_nested =
                            merge_nested_repositories(old, repositories, &mut removed_requests);
                        this.source_control_nested_complete = complete;
                        this.source_control_nested_refreshed_at = Some(Instant::now());
                    }
                    Ok(_) => {
                        this.source_control_nested_candidates = None;
                        this.source_control_nested_complete = false;
                        this.source_control_nested_error = Some(tr!("source_control.status_failed"))
                    }
                    Err(error) => {
                        this.source_control_nested_candidates = None;
                        this.source_control_nested_complete = false;
                        this.source_control_nested_error = Some(error.to_string());
                    }
                }
                for request in removed_requests {
                    this.cancel_source_control_query(Some(request), cx);
                }
                this.reload_stale_nested_repositories(cx);
                this.sync_source_control_rows(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_nested_repository(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(node) = find_nested_repository_mut(&mut self.source_control_nested, &path) else {
            return;
        };
        if !node.repository.initialized {
            return;
        }
        node.open = !node.open;
        if !node.open {
            let mut requests = Vec::new();
            take_nested_requests(std::slice::from_mut(node), &mut requests);
            for request in requests {
                self.cancel_source_control_query(Some(request), cx);
            }
            self.sync_source_control_rows(cx);
            cx.notify();
            return;
        }
        self.load_nested_repository(path, cx);
        self.sync_source_control_rows(cx);
        cx.notify();
    }

    fn load_nested_repository(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(root) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        let Some(node) = find_nested_repository_mut(&mut self.source_control_nested, &path) else {
            return;
        };
        if !node.open
            || !node.repository.initialized
            || node.loading
            || (node.status.is_some() && !node.stale)
        {
            return;
        }
        node.loading = true;
        node.error = None;
        let generation = self.source_control_nested_generation;
        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        let query_id = Uuid::new_v4();
        node.request = Some(PendingGitQuery {
            id: query_id,
            client: client.clone(),
        });
        let request_path = path.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut status = match client.request(
                        fintwind_client::WorkspaceOperation::InspectTrackedStatus {
                            cwd: request_path.clone(),
                            query_id: Some(query_id),
                        },
                    )? {
                        fintwind_client::WorkspaceResult::WorktreeStatus {
                            status: Some(status),
                        } => status,
                        _ => anyhow::bail!("nested repository is unavailable"),
                    };
                    let (untracked_complete, load_error) =
                        match client.request(fintwind_client::WorkspaceOperation::ListUntracked {
                            cwd: request_path.clone(),
                            directory: None,
                            after: None,
                            query_id: Some(query_id),
                        }) {
                            Ok(fintwind_client::WorkspaceResult::UntrackedFiles {
                                entries,
                                next,
                                incomplete,
                            }) => {
                                status.entries.extend(entries);
                                (next.is_none() && !incomplete, None)
                            }
                            Ok(_) => anyhow::bail!("nested repository status is unavailable"),
                            Err(error) => return Err(error),
                        };
                    let mut load_error = load_error;
                    let candidates = nested_repository_candidates(&status);
                    let children = match client.request(
                        fintwind_client::WorkspaceOperation::DiscoverNestedRepositories {
                            cwd: request_path,
                            candidates,
                            query_id: Some(query_id),
                        },
                    ) {
                        Ok(fintwind_client::WorkspaceResult::NestedRepositories {
                            repositories,
                            ..
                        }) => Some(repositories),
                        Ok(_) => {
                            load_error.get_or_insert_with(|| tr!("source_control.status_failed"));
                            None
                        }
                        Err(error) => {
                            load_error.get_or_insert_with(|| error.to_string());
                            None
                        }
                    };
                    Ok::<_, anyhow::Error>((status, untracked_complete, children, load_error))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.source_control_nested_generation != generation
                    || !this
                        .review_workspace_path()
                        .is_some_and(|current| current == root)
                {
                    return;
                }
                let Some(node) = find_nested_repository_mut(&mut this.source_control_nested, &path)
                else {
                    return;
                };
                if !node
                    .request
                    .as_ref()
                    .is_some_and(|request| request.id == query_id)
                {
                    return;
                }
                let request = node.request.take();
                node.loading = false;
                let failed = result.is_err();
                let mut removed_requests = Vec::new();
                match result {
                    Ok((status, untracked_complete, children, load_error)) => {
                        node.status = Some(status);
                        node.untracked_complete = untracked_complete;
                        node.stale = false;
                        if let Some(children) = children {
                            node.children = merge_nested_repositories(
                                std::mem::take(&mut node.children),
                                children,
                                &mut removed_requests,
                            );
                        }
                        node.error = load_error;
                    }
                    Err(error) => node.error = Some(error.to_string()),
                }
                if failed {
                    this.cancel_source_control_query(request, cx);
                }
                for request in removed_requests {
                    this.cancel_source_control_query(Some(request), cx);
                }
                this.sync_source_control_rows(cx);
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
        self.source_control_pending_untracked_directories.clear();
        self.right_panel_diff_generation = self.right_panel_diff_generation.wrapping_add(1);
        self.source_control_status = None;
        self.source_control_review_files = SourceControlReviewFiles::default();
        self.right_panel_diff_snapshot = None;
        self.right_panel_diff_selected_file = None;
        self.source_control_diff_context = 3;
        self.source_control_untracked_generation =
            self.source_control_untracked_generation.wrapping_add(1);
        self.source_control_untracked_loading = false;
        self.source_control_untracked_complete = false;
        self.source_control_untracked_ready = false;
        self.source_control_untracked_error = None;
        self.source_control_untracked_loaded.clear();
        self.source_control_untracked_next.clear();
        self.source_control_untracked_incomplete.clear();
        self.source_control_ignored_generation =
            self.source_control_ignored_generation.wrapping_add(1);
        self.source_control_ignored = None;
        self.source_control_ignored_open = false;
        self.source_control_ignored_loading = false;
        self.source_control_ignored_error = None;
        self.source_control_ignored_loaded.clear();
        self.source_control_ignored_next.clear();
        self.source_control_ignored_incomplete.clear();
        self.source_control_nested_generation =
            self.source_control_nested_generation.wrapping_add(1);
        self.source_control_nested_list_generation =
            self.source_control_nested_list_generation.wrapping_add(1);
        self.cancel_nested_repository_queries(cx);
        self.source_control_nested.clear();
        self.source_control_nested_error = None;
        self.source_control_nested_complete = false;
        self.source_control_nested_refreshed_at = None;
        self.source_control_folded_directories.borrow_mut().clear();
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

    /// Whether Git write actions may run right now: a history snapshot is
    /// read-only, and one action runs at a time.
    fn source_control_allows_writes(&self) -> bool {
        !self.review_is_history_snapshot() && self.source_control_busy.is_none()
    }

    /// Runs one source-control write against the workspace it names. One
    /// action at a time; landing refreshes the cached status and the open
    /// diff, and failures surface as a toast with the Git message.
    fn run_source_control_mutation(
        &mut self,
        label: SharedString,
        operation: fintwind_client::WorkspaceOperation,
        success_toast: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.source_control_busy.is_some() {
            return;
        }
        self.source_control_busy = Some(label);
        cx.notify();
        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client.request(operation) })
                .await;
            this.update(cx, |this, cx| {
                this.source_control_busy = None;
                match result {
                    Ok(fintwind_client::WorkspaceResult::Ack) => {
                        if let Some(message) = success_toast {
                            this.show_toast(message);
                        }
                    }
                    Ok(_) => this.show_toast(tr!("source_control.action_failed")),
                    Err(error) => this.show_toast(error.to_string()),
                }
                this.refresh_source_control_status(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The paths of the cached status that carry an unstaged side, including
    /// untracked files. This is the commit dialog's "stage all and commit"
    /// scope; a group header's own stage action stays inside that group.
    fn source_control_unstaged_paths(&self) -> Vec<String> {
        self.source_control_status
            .as_ref()
            .map(|status| {
                status
                    .entries
                    .iter()
                    .filter(|entry| source_control_entry_is_unstaged(entry))
                    .map(|entry| entry.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn source_control_untracked_fully_loaded(&self) -> bool {
        self.source_control_untracked_ready
    }

    /// Records what one untracked page read. A directory counts as loaded only
    /// when its last page arrived with no cursor and no truncation, which is
    /// what batch operations require before touching the whole group.
    fn apply_untracked_page(
        &mut self,
        directory: Option<String>,
        next: Option<String>,
        incomplete: bool,
    ) {
        let key = directory.unwrap_or_default();
        if let Some(next) = next {
            self.source_control_untracked_next.insert(key.clone(), next);
            self.source_control_untracked_incomplete.remove(&key);
            return;
        }
        self.source_control_untracked_next.remove(&key);
        if incomplete {
            self.source_control_untracked_incomplete.insert(key);
            return;
        }
        self.source_control_untracked_incomplete.remove(&key);
        if key.is_empty() {
            self.source_control_untracked_complete = true;
        } else {
            self.source_control_untracked_loaded.insert(key);
        }
    }

    fn apply_ignored_page(
        &mut self,
        directory: Option<String>,
        next: Option<String>,
        incomplete: bool,
    ) {
        let key = directory.unwrap_or_default();
        if let Some(next) = next {
            self.source_control_ignored_next.insert(key.clone(), next);
            self.source_control_ignored_incomplete.remove(&key);
            return;
        }
        self.source_control_ignored_next.remove(&key);
        if incomplete {
            self.source_control_ignored_incomplete.insert(key);
            return;
        }
        self.source_control_ignored_incomplete.remove(&key);
        if !key.is_empty() {
            self.source_control_ignored_loaded.insert(key);
        }
    }

    fn calculate_source_control_untracked_ready(&self) -> bool {
        if !self.source_control_untracked_complete
            || self.source_control_untracked_loading
            || self.source_control_untracked_error.is_some()
            || !self.source_control_untracked_next.is_empty()
            || !self.source_control_untracked_incomplete.is_empty()
        {
            return false;
        }
        let Some(status) = self.source_control_status.as_ref() else {
            return false;
        };
        status
            .entries
            .iter()
            .filter(|entry| entry.worktree_status == '?' && entry.path.ends_with('/'))
            .all(|entry| {
                let directory = entry.path.trim_end_matches('/');
                self.source_control_nested
                    .iter()
                    .any(|node| node.repository.relative_path == directory)
                    || self.source_control_untracked_loaded.contains(directory)
            })
    }

    /// The paths of the cached status that carry a staged side: everything
    /// one "unstage all" would reset.
    fn source_control_staged_paths(&self) -> Vec<String> {
        self.source_control_status
            .as_ref()
            .map(|status| {
                status
                    .entries
                    .iter()
                    .filter(|entry| source_control_entry_is_staged(entry))
                    .map(|entry| entry.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn stage_source_control_paths(
        &mut self,
        mut paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if !self.source_control_allows_writes() || paths.is_empty() {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        if let Some(status) = self.source_control_status.as_ref() {
            let selected: std::collections::HashSet<_> = paths.iter().cloned().collect();
            for entry in &status.entries {
                if selected.contains(&entry.path)
                    && matches!(entry.worktree_status, 'R' | 'C')
                    && let Some(origin) = entry.origin_path.as_ref()
                    && !selected.contains(origin)
                {
                    paths.push(origin.clone());
                }
            }
        }
        self.run_source_control_mutation(
            tr!("source_control.staging").into(),
            fintwind_client::WorkspaceOperation::StagePaths {
                cwd: workspace,
                paths,
            },
            None,
            cx,
        );
    }

    pub(super) fn unstage_source_control_paths(
        &mut self,
        mut paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if !self.source_control_allows_writes() || paths.is_empty() {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        if let Some(status) = self.source_control_status.as_ref() {
            let selected: std::collections::HashSet<_> = paths.iter().cloned().collect();
            for entry in &status.entries {
                if selected.contains(&entry.path)
                    && let Some(origin) = entry.origin_path.as_ref()
                    && !selected.contains(origin)
                {
                    paths.push(origin.clone());
                }
            }
        }
        self.run_source_control_mutation(
            tr!("source_control.unstaging").into(),
            fintwind_client::WorkspaceOperation::UnstagePaths {
                cwd: workspace,
                paths,
            },
            None,
            cx,
        );
    }

    /// Opens the discard confirmation for one file's unstaged side. An
    /// untracked file gets the deletion warning; a tracked one restores from
    /// the index and keeps its staged side.
    pub(super) fn request_discard_source_control_file(
        &mut self,
        path: String,
        cx: &mut Context<Self>,
    ) {
        if !self.source_control_allows_writes() {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        let Some(entry) = self
            .source_control_status
            .as_ref()
            .and_then(|status| status.entries.iter().find(|entry| entry.path == path))
        else {
            return;
        };
        let untracked = entry.worktree_status == '?';
        self.source_control_discard = Some(DiscardRequest {
            workspace,
            tracked: if untracked {
                Vec::new()
            } else {
                vec![path.clone()]
            },
            untracked: if untracked { vec![path] } else { Vec::new() },
        });
        cx.notify();
    }

    pub(super) fn confirm_source_control_discard(&mut self, cx: &mut Context<Self>) {
        if self.source_control_busy.is_some() {
            return;
        }
        let Some(request) = self.source_control_discard.take() else {
            return;
        };
        self.run_source_control_mutation(
            tr!("source_control.discarding").into(),
            fintwind_client::WorkspaceOperation::DiscardWorktreePaths {
                cwd: request.workspace,
                tracked: request.tracked,
                untracked: request.untracked,
            },
            None,
            cx,
        );
    }

    pub(super) fn cancel_source_control_discard(&mut self, cx: &mut Context<Self>) {
        if self.source_control_discard.take().is_some() {
            cx.notify();
        }
    }

    /// Opens the discard confirmation for the selected worktree group.
    fn request_discard_source_control_group(
        &mut self,
        group: SourceControlGroup,
        cx: &mut Context<Self>,
    ) {
        if !self.source_control_allows_writes() {
            return;
        }
        if group == SourceControlGroup::Untracked && !self.source_control_untracked_fully_loaded() {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        let mut tracked = Vec::new();
        let mut untracked = Vec::new();
        if let Some(status) = self.source_control_status.as_ref() {
            for entry in &status.entries {
                if !group.matches(entry) || entry.path.ends_with('/') {
                    continue;
                }
                if entry.worktree_status == '?' {
                    untracked.push(entry.path.clone());
                } else {
                    tracked.push(entry.path.clone());
                }
            }
        }
        if tracked.is_empty() && untracked.is_empty() {
            return;
        }
        self.source_control_discard = Some(DiscardRequest {
            workspace,
            tracked,
            untracked,
        });
        cx.notify();
    }

    pub(super) fn open_source_control_commit_dialog(&mut self, cx: &mut Context<Self>) {
        if self.source_control_commit_dialog {
            return;
        }
        self.source_control_commit_dialog = true;
        cx.notify();
    }

    pub(super) fn close_source_control_commit_dialog(&mut self, cx: &mut Context<Self>) {
        if self.source_control_commit_dialog {
            self.source_control_commit_dialog = false;
            cx.notify();
        }
    }

    pub(super) fn fetch_source_control(&mut self, cx: &mut Context<Self>) {
        if !self.source_control_allows_writes() {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        self.run_source_control_mutation(
            tr!("source_control.fetching").into(),
            fintwind_client::WorkspaceOperation::FetchRemote { cwd: workspace },
            Some(tr!("source_control.fetch_done")),
            cx,
        );
    }

    pub(super) fn pull_source_control(&mut self, cx: &mut Context<Self>) {
        if !self.source_control_allows_writes() {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        self.run_source_control_mutation(
            tr!("source_control.pulling").into(),
            fintwind_client::WorkspaceOperation::PullFastForward { cwd: workspace },
            Some(tr!("source_control.pull_done")),
            cx,
        );
    }

    pub(super) fn push_source_control(&mut self, cx: &mut Context<Self>) {
        if !self.source_control_allows_writes() {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        self.run_source_control_mutation(
            tr!("source_control.pushing").into(),
            fintwind_client::WorkspaceOperation::Push { cwd: workspace },
            Some(tr!("source_control.push_done")),
            cx,
        );
    }

    /// Switches the pinned-or-followed workspace to `branch`, creating it
    /// first when asked. Git's own guard against switching with conflicting
    /// uncommitted changes is never bypassed.
    pub(super) fn checkout_source_control_branch(
        &mut self,
        branch: String,
        create: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.source_control_allows_writes() {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        let label = tr!("source_control.switching", branch = branch.clone());
        self.run_source_control_mutation(
            label.into(),
            fintwind_client::WorkspaceOperation::CheckoutBranch {
                cwd: workspace,
                branch,
                create,
            },
            None,
            cx,
        );
    }

    /// Commits the staged content of the pinned-or-followed workspace. The
    /// staged set is the scope — nothing unstaged rides along — except the
    /// explicit "stage all and commit" shortcut, which stages first.
    pub(super) fn commit_source_control(
        &mut self,
        stage_all: bool,
        push: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.source_control_allows_writes() {
            return;
        }
        if stage_all && !self.source_control_untracked_fully_loaded() {
            self.show_toast(tr!("source_control.load_untracked_first"));
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        let message = self
            .source_control_commit_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        if message.is_empty() {
            self.show_toast(tr!("source_control.commit_empty_message"));
            return;
        }
        let staged_paths = self.source_control_staged_paths();
        if !stage_all && staged_paths.is_empty() {
            self.show_toast(tr!("source_control.commit_nothing_staged"));
            return;
        }
        let unstaged_paths = if stage_all {
            self.source_control_unstaged_paths()
        } else {
            Vec::new()
        };
        self.source_control_busy = Some(
            if push {
                tr!("source_control.committing_and_pushing")
            } else {
                tr!("source_control.committing")
            }
            .into(),
        );
        cx.notify();
        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if !unstaged_paths.is_empty() {
                        client.request(fintwind_client::WorkspaceOperation::StagePaths {
                            cwd: workspace.clone(),
                            paths: unstaged_paths,
                        })?;
                    }
                    client.request(fintwind_client::WorkspaceOperation::Commit {
                        cwd: workspace,
                        message,
                        include_unstaged: false,
                        push,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.source_control_busy = None;
                match result {
                    Ok(_) => {
                        this.source_control_commit_dialog = false;
                        this.source_control_commit_input.update(cx, |input, cx| {
                            input.clear(cx);
                        });
                        this.show_toast(if push {
                            tr!("source_control.commit_push_done")
                        } else {
                            tr!("source_control.commit_done")
                        });
                    }
                    Err(error) => this.show_toast(error.to_string()),
                }
                this.refresh_source_control_status(cx);
                this.refresh_review_diff(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Fills the commit input from the staged diff via the session's coding
    /// agent. Needs a selected session — the invocation is its model setup.
    pub(super) fn generate_source_control_commit_message(&mut self, cx: &mut Context<Self>) {
        if !self.source_control_allows_writes() {
            return;
        }
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            return;
        };
        if self.source_control_staged_paths().is_empty() {
            self.show_toast(tr!("source_control.commit_nothing_staged"));
            return;
        }
        let Some(session) = self.selected_session() else {
            self.show_toast(tr!("commit.no_task"));
            return;
        };
        let Some(invocation) = self
            .provider_probe()
            .and_then(|probe| probe.path.clone())
            .map(|binary| crate::git_commit::AgentInvocation {
                binary,
                model: self.model_for_session(session).map(str::to_owned),
                reasoning_effort: session.reasoning_effort.clone(),
            })
        else {
            self.show_toast(tr!("commit.no_provider"));
            return;
        };
        self.source_control_busy = Some(tr!("source_control.generating_message").into());
        cx.notify();
        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.request(fintwind_client::WorkspaceOperation::GenerateCommitMessage {
                        cwd: workspace,
                        include_unstaged: false,
                        invocation,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.source_control_busy = None;
                match result {
                    Ok(fintwind_client::WorkspaceResult::CommitMessage { message }) => {
                        this.source_control_commit_input.update(cx, |input, cx| {
                            input.set_content(message, cx);
                        });
                    }
                    Ok(_) => this.show_toast(tr!("source_control.action_failed")),
                    Err(error) => this.show_toast(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Fetches the pinned-or-followed workspace's local branches for the
    /// branch menu, in the background alongside the status query.
    pub(super) fn refresh_source_control_branches(&mut self, cx: &mut Context<Self>) {
        self.source_control_branch_generation =
            self.source_control_branch_generation.wrapping_add(1);
        let generation = self.source_control_branch_generation;
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            self.source_control_branches = None;
            return;
        };
        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |this, cx| {
            let request_workspace = workspace.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.request(fintwind_client::WorkspaceOperation::InspectBranchRefs {
                        cwd: request_workspace,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                if this.source_control_branch_generation != generation
                    || !this
                        .review_workspace_path()
                        .is_some_and(|path| path == workspace)
                {
                    return;
                }
                match result {
                    Ok(fintwind_client::WorkspaceResult::Branches { snapshot }) => {
                        this.source_control_branches = snapshot.map(Arc::new);
                    }
                    _ => this.source_control_branches = None,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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

    /// Flattens the cached status into the virtualized list's rows: one
    /// header per non-empty group, then that group's files arranged as a
    /// directory tree. The filter input narrows both groups by path, and a
    /// filter always opens every directory so its matches are visible.
    /// Called whenever the status snapshot, the filter, or a fold changes.
    pub(super) fn sync_source_control_rows(&mut self, cx: &App) {
        self.source_control_untracked_ready = self.calculate_source_control_untracked_ready();
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
        let force_open = !filter.is_empty();
        let folded = self.source_control_folded_directories.borrow();
        let nested_paths: std::collections::HashSet<&str> = self
            .source_control_nested
            .iter()
            .map(|node| node.repository.relative_path.as_str())
            .collect();
        let mut rows = Vec::new();
        let no_pages = HashMap::new();
        let no_incomplete = HashSet::new();
        if let Some(status) = self.source_control_status.as_ref() {
            for group in SourceControlGroup::ALL {
                let (next_pages, incomplete) = match group {
                    SourceControlGroup::Untracked => (
                        &self.source_control_untracked_next,
                        &self.source_control_untracked_incomplete,
                    ),
                    SourceControlGroup::Ignored => (
                        &self.source_control_ignored_next,
                        &self.source_control_ignored_incomplete,
                    ),
                    _ => (&no_pages, &no_incomplete),
                };
                let entries: Vec<_> = status
                    .entries
                    .iter()
                    .filter(|entry| {
                        group.matches(entry)
                            && matches(entry)
                            && !(group == SourceControlGroup::Untracked
                                && entry.path.ends_with('/')
                                && nested_paths.contains(entry.path.trim_end_matches('/')))
                    })
                    .collect();
                let pending_root = next_pages.contains_key("") || incomplete.contains("");
                if !entries.is_empty() || pending_root {
                    rows.push(SourceControlRow::Header {
                        group,
                        count: entries
                            .iter()
                            .filter(|entry| !entry.path.ends_with('/'))
                            .count(),
                        incomplete: group == SourceControlGroup::Untracked
                            && !self.source_control_untracked_ready,
                    });
                    source_control_tree_rows(
                        &mut rows, group, &entries, &folded, force_open, next_pages, incomplete,
                    );
                }
            }
            if self.source_control_untracked_loading
                || (!self.source_control_untracked_complete
                    && self.source_control_untracked_error.is_none())
            {
                rows.push(SourceControlRow::UntrackedLoading);
            }
            if let Some(message) = self.source_control_untracked_error.as_ref() {
                rows.push(SourceControlRow::IgnoredError {
                    message: message.clone(),
                });
            }
            source_control_nested_rows(&mut rows, &self.source_control_nested, 0, &filter);
            if let Some(message) = self.source_control_nested_error.as_ref() {
                rows.push(SourceControlRow::IgnoredError {
                    message: message.clone(),
                });
            } else if !self.source_control_nested_complete
                && self.source_control_nested_refreshed_at.is_some()
            {
                rows.push(SourceControlRow::IgnoredError {
                    message: tr!("source_control.repositories_incomplete"),
                });
            }
            rows.push(SourceControlRow::IgnoredHeader {
                open: self.source_control_ignored_open,
                loading: self.source_control_ignored_loading,
            });
            if self.source_control_ignored_open {
                if let Some(error) = self.source_control_ignored_error.as_ref() {
                    rows.push(SourceControlRow::IgnoredError {
                        message: error.clone(),
                    });
                }
                let entries: Vec<_> = self
                    .source_control_ignored
                    .as_ref()
                    .map(|entries| entries.iter().filter(|entry| matches(entry)).collect())
                    .unwrap_or_default();
                let pending_root = self.source_control_ignored_next.contains_key("")
                    || self.source_control_ignored_incomplete.contains("");
                if entries.is_empty()
                    && !pending_root
                    && filter.is_empty()
                    && !self.source_control_ignored_loading
                    && self.source_control_ignored_error.is_none()
                {
                    rows.push(SourceControlRow::IgnoredEmpty);
                }
                if !entries.is_empty() || pending_root {
                    source_control_tree_rows(
                        &mut rows,
                        SourceControlGroup::Ignored,
                        &entries,
                        &folded,
                        force_open,
                        &self.source_control_ignored_next,
                        &self.source_control_ignored_incomplete,
                    );
                }
            }
        }
        let row_count = rows.len();
        let mut cached = self.source_control_rows.borrow_mut();
        if cached.as_slice() == rows.as_slice() {
            return;
        }
        // Fold changes rewrite one subtree; splicing only the changed suffix
        // keeps the scroll position and every measured row above it.
        let prefix = cached
            .iter()
            .zip(rows.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let old_count = cached.len();
        *cached = rows;
        drop(cached);
        let height = ui_px(SOURCE_CONTROL_ROW_HEIGHT);
        if old_count == 0 {
            self.source_control_list_state
                .reset_with_uniform_height(row_count, height);
        } else {
            self.source_control_list_state
                .splice(prefix..old_count, row_count - prefix);
            self.source_control_list_state
                .clone()
                .with_uniform_item_height(height);
        }
    }

    /// Opens or folds one directory of the change tree. Directories start
    /// open, so the first press folds; the folded set is what persists.
    fn toggle_source_control_directory(
        &mut self,
        group: SourceControlGroup,
        path: String,
        cx: &mut Context<Self>,
    ) {
        let key = source_control_directory_key(group, &path);
        let mut folded = self.source_control_folded_directories.borrow_mut();
        if !folded.remove(&key) {
            folded.insert(key);
        }
        let open = if matches!(
            group,
            SourceControlGroup::Ignored | SourceControlGroup::Untracked
        ) {
            folded.contains(&source_control_directory_key(group, &path))
        } else {
            !folded.contains(&source_control_directory_key(group, &path))
        };
        drop(folded);
        if group == SourceControlGroup::Ignored
            && open
            && !self.source_control_ignored_loaded.contains(&path)
            && !self.source_control_ignored_next.contains_key(&path)
            && self.source_control_ignored.as_ref().is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.path.strip_suffix('/') == Some(path.as_str()))
            })
        {
            self.refresh_source_control_ignored(Some(path), None, cx);
        } else if group == SourceControlGroup::Untracked {
            if open {
                self.enqueue_untracked_directory(&path);
                self.load_next_pending_untracked(cx);
            } else {
                self.source_control_pending_untracked_directories
                    .retain(|pending| {
                        pending != &path
                            && !pending
                                .strip_prefix(&path)
                                .is_some_and(|rest| rest.starts_with('/'))
                    });
            }
        }
        self.sync_source_control_rows(cx);
        cx.notify();
    }

    /// Opens every directory above `path` on both sides of the tree, so a
    /// file the reader moves to is reachable in the list beside it. Reports
    /// whether any fold actually changed.
    fn reveal_source_control_file(&mut self, path: &str) -> bool {
        let Some((directory, _)) = path.rsplit_once('/') else {
            return false;
        };
        let mut folded = self.source_control_folded_directories.borrow_mut();
        let mut changed = false;
        for (index, _) in directory.match_indices('/') {
            let ancestor = &directory[..index];
            for group in SourceControlGroup::ALL {
                let key = source_control_directory_key(group, ancestor);
                changed |= if group == SourceControlGroup::Untracked {
                    folded.insert(key)
                } else {
                    folded.remove(&key)
                };
            }
        }
        for group in SourceControlGroup::ALL {
            let key = source_control_directory_key(group, directory);
            changed |= if group == SourceControlGroup::Untracked {
                folded.insert(key)
            } else {
                folded.remove(&key)
            };
        }
        changed
    }

    fn reveal_source_control_untracked_directory(&mut self, directory: &str) {
        let mut folded = self.source_control_folded_directories.borrow_mut();
        for (index, _) in directory.match_indices('/') {
            folded.insert(source_control_directory_key(
                SourceControlGroup::Untracked,
                &directory[..index],
            ));
        }
        folded.insert(source_control_directory_key(
            SourceControlGroup::Untracked,
            directory,
        ));
    }

    fn enqueue_untracked_directory(&mut self, directory: &str) {
        self.reveal_source_control_untracked_directory(directory);
        for path in directory
            .match_indices('/')
            .map(|(index, _)| &directory[..index])
            .chain(std::iter::once(directory))
        {
            // A directory that already has a cursor is mid-listing; expanding
            // it must continue that page, not restart it.
            if !self.source_control_untracked_loaded.contains(path)
                && !self.source_control_untracked_next.contains_key(path)
                && !self
                    .source_control_pending_untracked_directories
                    .iter()
                    .any(|pending| pending == path)
            {
                self.source_control_pending_untracked_directories
                    .push_back(path.to_owned());
            }
        }
    }

    /// Reads the next page of one group's directory listing. `directory` is
    /// `None` for the worktree root.
    fn load_more_source_control_group(
        &mut self,
        group: SourceControlGroup,
        directory: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let key = directory.clone().unwrap_or_default();
        let next = match group {
            SourceControlGroup::Untracked => self.source_control_untracked_next.get(&key).cloned(),
            SourceControlGroup::Ignored => self.source_control_ignored_next.get(&key).cloned(),
            _ => None,
        };
        let Some(next) = next else {
            return;
        };
        match group {
            SourceControlGroup::Untracked => {
                self.refresh_source_control_untracked(directory, Some(next), cx)
            }
            SourceControlGroup::Ignored => {
                self.refresh_source_control_ignored(directory, Some(next), cx)
            }
            _ => {}
        }
    }

    fn load_next_pending_untracked(&mut self, cx: &mut Context<Self>) {
        if self.source_control_untracked_loading || !self.source_control_untracked_complete {
            return;
        }
        while let Some(directory) = self
            .source_control_pending_untracked_directories
            .pop_front()
        {
            if self.source_control_untracked_loaded.contains(&directory) {
                continue;
            }
            let mut candidate = directory.as_str();
            let mut target = None;
            loop {
                if self.source_control_nested.iter().any(|node| {
                    candidate == node.repository.relative_path
                        || candidate
                            .strip_prefix(&node.repository.relative_path)
                            .is_some_and(|rest| rest.starts_with('/'))
                }) {
                    break;
                }
                let marker = format!("{candidate}/");
                if !self.source_control_untracked_loaded.contains(candidate)
                    && !self.source_control_untracked_next.contains_key(candidate)
                    && self
                        .source_control_review_files
                        .entry(self.source_control_status.as_deref(), &marker)
                        .is_some_and(|entry| entry.worktree_status == '?')
                {
                    target = Some(candidate.to_owned());
                    break;
                }
                let Some((parent, _)) = candidate.rsplit_once('/') else {
                    break;
                };
                candidate = parent;
            }
            if let Some(target) = target {
                if target != directory {
                    self.source_control_pending_untracked_directories
                        .push_front(directory);
                }
                self.refresh_source_control_untracked(Some(target), None, cx);
                return;
            }
        }
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
        if self.reveal_source_control_file(&path) {
            self.sync_source_control_rows(cx);
        }
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
        let switched_source = self.right_panel_diff_source != source;
        self.reset_review_diff_state(source);
        self.review_pending_file_focus = Some(path);
        self.refresh_review_diff(cx);
        if switched_source {
            self.refresh_review_stats(cx);
        }
    }

    /// Rebuilds the single-file diff rows from the cached snapshot, the
    /// selected file, and the layout. Called whenever any of the three
    /// changes; render reads only the flattened result.
    pub(super) fn sync_source_control_diff_rows(&mut self) {
        let range = self
            .right_panel_diff_snapshot
            .as_ref()
            .zip(self.right_panel_diff_selected_file)
            .map(|(snapshot, file_index)| source_control_file_line_range(snapshot, file_index));
        self.source_control_number_width = self
            .right_panel_diff_snapshot
            .as_ref()
            .zip(range.clone())
            .map_or_else(
                || review_diff_number_width(2),
                |(snapshot, range)| review_diff_number_width(review_diff_digits(snapshot, range)),
            );
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
        if let Some((files, current)) = self.source_control_live_selection() {
            let next = (current as isize + delta).clamp(0, files.paths.len() as isize - 1) as usize;
            if next != current {
                let path = files.paths[next].clone();
                let source = self.right_panel_diff_source;
                self.reset_review_diff_state(source);
                if self.reveal_source_control_file(&path) {
                    self.sync_source_control_rows(cx);
                }
                self.review_pending_file_focus = Some(path);
                self.refresh_review_diff(cx);
            }
            return;
        }
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
        if let Some(path) = self
            .right_panel_diff_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.files.get(next))
            .map(|file| file.path.clone())
            && self.reveal_source_control_file(&path)
        {
            self.sync_source_control_rows(cx);
        }
        self.sync_source_control_diff_rows();
        cx.notify();
    }

    /// The mode's second column: the workspace the review is pointed at, and
    /// its staged/unstaged change tree. Showing the project, the concrete
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
            .child(self.render_source_control_commit_bar(cx))
    }

    /// The action bar below the file groups: the commit button, which opens
    /// the commit dialog. The dialog decides what is possible from there —
    /// plain commit needs staged content, "stage all and commit" only needs
    /// a dirty tree — so the bar itself only gates on writes in flight.
    fn render_source_control_commit_bar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let writable = !self.review_is_history_snapshot();
        let ready = writable && self.source_control_busy.is_none();
        div()
            .flex_none()
            .px(px(12.0))
            .py(px(10.0))
            .border_t_1()
            .border_color(theme.sidebar_border)
            .flex()
            .items_center()
            .child(
                div()
                    .id("source-control-commit-open")
                    .tab_index(0)
                    .h(px(28.0))
                    .w_full()
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(5.0))
                    .text_size(ui_px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .focus_visible(|button| button.border_1().border_color(theme.accent_focus))
                    .when(ready, |button| {
                        button
                            .bg(theme.inverse)
                            .text_color(theme.on_inverse)
                            .cursor_default()
                            .hover(|button| button.opacity(0.9))
                            .on_activation(cx, |this, _, cx| {
                                this.open_source_control_commit_dialog(cx);
                            })
                    })
                    .when(!ready, |button| {
                        button.bg(theme.overlay_strong).text_color(theme.text_ghost)
                    })
                    .child(icon(
                        "icons/git-commit-horizontal.svg",
                        13.0,
                        theme.on_inverse,
                    ))
                    .child(tr!("source_control.commit")),
            )
    }

    /// The workspace block: the workspace picker (which shows the current
    /// directory and opens the switcher) above the branch row.
    fn render_source_control_workspace_block(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let status = self.source_control_status.as_ref();
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
            .gap(px(4.0))
            .child(self.render_source_control_workspace_menu(cx))
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
            .when(status.is_some(), |column| {
                column.child(self.render_source_control_branch_row(branch_label, cx))
            })
    }

    /// Branch selection and refresh stay visible; remote operations live in the menu.
    fn render_source_control_branch_row(
        &self,
        branch_label: String,
        cx: &mut Context<Self>,
    ) -> Div {
        let enabled = !self.review_is_history_snapshot() && self.source_control_busy.is_none();

        div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.render_source_control_branch_menu(branch_label, enabled, cx)),
            )
            .child(self.render_source_control_action_button(
                "source-control-refresh",
                "icons/rotate-cw.svg",
                tr!("source_control.refresh"),
                enabled,
                |this, cx| {
                    this.refresh_source_control_status(cx);
                    this.refresh_review_diff(cx);
                },
                cx,
            ))
            .child(self.render_source_control_repository_menu(enabled, cx))
    }

    fn render_source_control_repository_menu(
        &self,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let handle = self.menu_handle("source-control-repository-actions", cx);
        let weak = cx.entity().downgrade();
        dropdown_menu(
            MenuChip::new("source-control-repository-actions")
                .icon("icons/ellipsis.svg", theme.text_tertiary)
                .icon_only(true)
                .height(px(26.0))
                .tooltip(tr!("source_control.repository_actions"))
                .selected(handle.is_open()),
            "source-control-repository-actions-menu",
            &handle,
            MenuAlign::BelowRight,
            move |_| {
                let actions: [(_, _, fn(&mut Self, &mut Context<Self>)); 3] = [
                    (
                        tr!("source_control.fetch"),
                        "icons/download.svg",
                        Self::fetch_source_control,
                    ),
                    (
                        tr!("source_control.pull"),
                        "icons/arrow-down.svg",
                        Self::pull_source_control,
                    ),
                    (
                        tr!("source_control.push"),
                        "icons/arrow-up.svg",
                        Self::push_source_control,
                    ),
                ];
                actions
                    .into_iter()
                    .map(|(label, icon, action)| {
                        let weak = weak.clone();
                        MenuItem::new(label, move |_, cx| {
                            let _ = weak.update(cx, |this, cx| action(this, cx));
                        })
                        .icon(icon)
                        .disabled(!enabled)
                    })
                    .collect()
            },
        )
    }

    /// One small icon button of the branch row. `action` is the plain
    /// method it fires.
    fn render_source_control_action_button(
        &self,
        id: &'static str,
        path: &'static str,
        label: String,
        enabled: bool,
        action: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id(id)
            .tab_index(0)
            .size(px(26.0))
            .rounded(px(5.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .when(enabled, |button| {
                button
                    .cursor_default()
                    .hover(|button| button.bg(theme.overlay))
                    .active(|button| button.bg(theme.overlay_strong))
                    .on_activation(cx, move |this, _, cx| {
                        action(this, cx);
                    })
            })
            .when(!enabled, |button| button.opacity(0.4))
            .focus_visible(|button| button.border_1().border_color(theme.accent_focus))
            .tooltip(Tooltip::text(label))
            .child(icon(path, 12.0, theme.text_tertiary))
    }

    /// The branch menu: the current branch on the chip, the local branches
    /// beneath it, and a create row (type a name, press Enter) that
    /// switches to a fresh branch.
    fn render_source_control_branch_menu(
        &self,
        branch_label: String,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = Theme::current(cx);
        let handle = self.menu_handle("source-control-branch", cx);
        let snapshot = self.source_control_branches.clone();
        let weak = cx.entity().downgrade();
        let create_input = self.source_control_branch_create_input.clone();
        dropdown_menu(
            MenuChip::new("source-control-branch")
                .icon("icons/git-branch.svg", theme.text_tertiary)
                .label(branch_label)
                .max_label_width(140.0)
                .height(px(26.0))
                .w_full()
                .min_w_0()
                .background(theme.sidebar)
                .caret(true)
                .disabled(!enabled)
                .selected(enabled && handle.is_open()),
            "source-control-branch-menu",
            &handle,
            MenuAlign::BelowLeft,
            move |_| {
                let mut items = Vec::new();
                // The create row: the name goes in, Enter switches. The
                // Submit subscription on the input runs the checkout.
                items.push(MenuItem::custom({
                    let input = create_input.clone();
                    move |_window, _cx| {
                        let input = input.clone();
                        div()
                            .px(px(8.0))
                            .py(px(6.0))
                            .child(
                                TextField::new("source-control-branch-create", input)
                                    .icon("icons/plus.svg", 11.0)
                                    .w_full(),
                            )
                            .into_any_element()
                    }
                }));
                items.push(MenuItem::Separator);
                if let Some(snapshot) = snapshot.as_ref() {
                    for branch in &snapshot.branches {
                        let weak = weak.clone();
                        let name = branch.name.clone();
                        items.push(
                            MenuItem::new(name.clone(), move |_, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.checkout_source_control_branch(name.clone(), false, cx)
                                });
                            })
                            .selected(snapshot.current.as_deref() == Some(branch.name.as_str())),
                        );
                    }
                }
                items
            },
        )
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
                .label(current)
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
            if status.entries.is_empty()
                && self.source_control_untracked_complete
                && !self.source_control_untracked_loading
            {
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
            // The path filter narrows the change tree and always opens the
            // directories it matches.
            .when(has_workspace && status.is_some(), |column| {
                column.child(
                    div().px(px(12.0)).pb(px(8.0)).child(
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

    /// One flattened row: a group header with its group-wide actions, a
    /// foldable directory, or a clickable file with its status letter.
    /// Per-file actions live beside the file name in the diff reader's
    /// header, not here.
    fn render_source_control_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(row) = self.source_control_rows.borrow().get(index).cloned() else {
            return div().into_any_element();
        };
        let writable = !self.review_is_history_snapshot();
        match row {
            SourceControlRow::Header {
                group,
                count,
                incomplete,
            } => div()
                .w_full()
                .min_w_0()
                .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                .flex_none()
                .pl(px(12.0))
                .pr(px(8.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(ui_px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_tertiary)
                .child(match group {
                    SourceControlGroup::Staged => tr!("source_control.staged", count = count),
                    SourceControlGroup::Unstaged => tr!("source_control.unstaged", count = count),
                    SourceControlGroup::Untracked if incomplete => {
                        tr!("source_control.untracked_incomplete")
                    }
                    SourceControlGroup::Untracked => tr!("source_control.untracked", count = count),
                    SourceControlGroup::Ignored => tr!("source_control.ignored"),
                })
                .child(div().flex_1())
                // One click moves the whole group across the staging line;
                // discard exists on the unstaged side only and confirms first.
                .when(writable, |header| {
                    if group == SourceControlGroup::Staged {
                        header.child(self.render_source_control_text_action(
                            "source-control-unstage-all",
                            tr!("source_control.unstage_all"),
                            move |this, cx| {
                                let paths = this.source_control_staged_paths();
                                this.unstage_source_control_paths(paths, cx);
                            },
                            cx,
                        ))
                    } else if group == SourceControlGroup::Untracked
                        && !self.source_control_untracked_fully_loaded()
                    {
                        header
                    } else {
                        header
                            .child(self.render_source_control_text_action(
                                match group {
                                    SourceControlGroup::Untracked => {
                                        "source-control-stage-untracked"
                                    }
                                    _ => "source-control-stage-all",
                                },
                                tr!("source_control.stage_all"),
                                move |this, cx| {
                                    let paths = this
                                        .source_control_status
                                        .as_ref()
                                        .map(|status| {
                                            status
                                                .entries
                                                .iter()
                                                .filter(|entry| {
                                                    group.matches(entry)
                                                        && !entry.path.ends_with('/')
                                                })
                                                .map(|entry| entry.path.clone())
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    this.stage_source_control_paths(paths, cx);
                                },
                                cx,
                            ))
                            .child(self.render_source_control_group_menu(group, cx))
                    }
                })
                .into_any_element(),
            SourceControlRow::IgnoredHeader { open, loading } => div()
                .id("source-control-ignored")
                .tab_index(0)
                .w_full()
                .min_w_0()
                .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                .flex_none()
                .pl(px(12.0))
                .pr(px(8.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(ui_px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_tertiary)
                .cursor_default()
                .focus_visible(|element| element.border_1().border_color(theme.accent_focus))
                .hover(|element| element.bg(theme.overlay))
                .child(icon(
                    if open {
                        "icons/chevron-down.svg"
                    } else {
                        "icons/chevron-right.svg"
                    },
                    10.0,
                    theme.text_ghost,
                ))
                .child(tr!("source_control.ignored"))
                .when(loading, |row| row.child(tr!("source_control.loading")))
                .on_activation(cx, |this, _, cx| this.toggle_source_control_ignored(cx))
                .into_any_element(),
            SourceControlRow::IgnoredError { message } => div()
                .w_full()
                .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                .px(px(12.0))
                .text_size(ui_px(11.0))
                .text_color(theme.danger)
                .truncate()
                .child(message)
                .into_any_element(),
            SourceControlRow::IgnoredEmpty => div()
                .w_full()
                .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                .px(px(12.0))
                .text_size(ui_px(11.0))
                .text_color(theme.text_tertiary)
                .child(tr!("source_control.ignored_empty"))
                .into_any_element(),
            SourceControlRow::UntrackedLoading => div()
                .w_full()
                .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                .px(px(12.0))
                .text_size(ui_px(11.0))
                .text_color(theme.text_tertiary)
                .child(tr!("source_control.loading"))
                .into_any_element(),
            SourceControlRow::LoadMore {
                group,
                directory,
                depth,
            } => div()
                .id(SharedString::from(format!(
                    "source-control-load-more-{}-{}",
                    group.key(),
                    directory.as_deref().unwrap_or("")
                )))
                .aria_label(tr!("source_control.load_more"))
                .tab_index(0)
                .w_full()
                .min_w_0()
                .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                .flex_none()
                .pl(px(12.0 + depth as f32 * SOURCE_CONTROL_TREE_INDENT))
                .pr(px(12.0))
                .flex()
                .items_center()
                .rounded(px(5.0))
                .cursor_default()
                .hover(|element| element.bg(theme.overlay))
                .focus_visible(|element| element.border_1().border_color(theme.accent_focus))
                .on_activation(cx, move |this, _, cx| {
                    this.load_more_source_control_group(group, directory.clone(), cx)
                })
                .child(
                    div()
                        .text_size(ui_px(11.0))
                        .text_color(theme.text_secondary)
                        .child(tr!("source_control.load_more")),
                )
                .into_any_element(),
            SourceControlRow::ListingIncomplete { depth, .. } => div()
                .w_full()
                .min_w_0()
                .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                .flex_none()
                .pl(px(12.0 + depth as f32 * SOURCE_CONTROL_TREE_INDENT))
                .pr(px(12.0))
                .flex()
                .items_center()
                .text_size(ui_px(11.0))
                .text_color(theme.text_tertiary)
                .truncate()
                .child(tr!("source_control.listing_incomplete"))
                .into_any_element(),
            SourceControlRow::Repository {
                path,
                name,
                depth,
                open,
                loading,
                initialized,
            } => {
                let element_id =
                    SharedString::from(format!("source-control-repository-{}", path.display()));
                div()
                    .id(element_id)
                    .aria_label(name.clone())
                    .w_full()
                    .min_w_0()
                    .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                    .flex_none()
                    .pl(px(12.0 + depth as f32 * SOURCE_CONTROL_TREE_INDENT))
                    .pr(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(ui_px(11.5))
                    .text_color(theme.text_secondary)
                    .focus_visible(|element| element.border_1().border_color(theme.accent_focus))
                    .when(initialized, |element| {
                        element
                            .tab_index(0)
                            .cursor_default()
                            .hover(|element| element.bg(theme.overlay))
                            .on_activation(cx, move |this, _, cx| {
                                this.toggle_nested_repository(path.clone(), cx)
                            })
                    })
                    .child(icon(
                        if open {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        10.0,
                        theme.text_ghost,
                    ))
                    .child(icon("icons/git-branch.svg", 13.0, theme.text_tertiary))
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .when(loading, |element| {
                        element.child(tr!("source_control.loading"))
                    })
                    .when(!initialized, |element| {
                        element.child(tr!("source_control.repository_uninitialized"))
                    })
                    .into_any_element()
            }
            SourceControlRow::NestedHeader {
                group,
                count,
                depth,
                incomplete,
            } => div()
                .w_full()
                .min_w_0()
                .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                .flex_none()
                .pl(px(12.0 + depth as f32 * SOURCE_CONTROL_TREE_INDENT))
                .flex()
                .items_center()
                .text_size(ui_px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_tertiary)
                .child(match group {
                    SourceControlGroup::Staged => tr!("source_control.staged", count = count),
                    SourceControlGroup::Unstaged => tr!("source_control.unstaged", count = count),
                    SourceControlGroup::Untracked if incomplete => {
                        tr!("source_control.untracked_incomplete")
                    }
                    SourceControlGroup::Untracked => tr!("source_control.untracked", count = count),
                    SourceControlGroup::Ignored => tr!("source_control.ignored"),
                })
                .into_any_element(),
            SourceControlRow::NestedDirectory {
                repository,
                path,
                depth,
            } => {
                let id = SharedString::from(format!(
                    "source-control-nested-directory-{}-{path}",
                    repository.display()
                ));
                let directory = path.trim_end_matches('/').to_owned();
                div()
                    .id(id)
                    .aria_label(directory.clone())
                    .tab_index(0)
                    .w_full()
                    .min_w_0()
                    .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                    .flex_none()
                    .pl(px(12.0 + depth as f32 * SOURCE_CONTROL_TREE_INDENT))
                    .pr(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(5.0))
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .focus_visible(|element| element.border_1().border_color(theme.accent_focus))
                    .on_activation(cx, move |this, _, cx| {
                        this.pin_source_control_workspace(Some(repository.clone()), cx);
                        this.enqueue_untracked_directory(&directory);
                        this.load_next_pending_untracked(cx);
                    })
                    .child(icon("icons/folder.svg", 13.0, theme.text_ghost))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(11.5))
                            .text_color(theme.text_secondary)
                            .child(path),
                    )
                    .into_any_element()
            }
            SourceControlRow::NestedFile {
                repository,
                group,
                path,
                depth,
                letter,
            } => {
                let id = SharedString::from(format!(
                    "source-control-nested-file-{}-{}-{path}",
                    repository.display(),
                    group.key()
                ));
                let row_path = path.clone();
                div()
                    .id(id)
                    .aria_label(path.clone())
                    .tab_index(0)
                    .w_full()
                    .min_w_0()
                    .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                    .flex_none()
                    .pl(px(12.0 + depth as f32 * SOURCE_CONTROL_TREE_INDENT))
                    .pr(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(5.0))
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .focus_visible(|element| element.border_1().border_color(theme.accent_focus))
                    .on_activation(cx, move |this, _, cx| {
                        this.pin_source_control_workspace(Some(repository.clone()), cx);
                        this.open_source_control_file(
                            group == SourceControlGroup::Staged,
                            row_path.clone(),
                            cx,
                        );
                    })
                    .child(file_icon(file_icon_for_path(&path), 13.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(11.5))
                            .text_color(theme.text_secondary)
                            .child(path),
                    )
                    .child(
                        div()
                            .w(px(12.0))
                            .text_right()
                            .text_size(ui_px(10.5))
                            .text_color(status_letter_color(letter, &theme))
                            .child(letter.to_string()),
                    )
                    .into_any_element()
            }
            SourceControlRow::Directory {
                group,
                path,
                name,
                depth,
                open,
            } => {
                let element_id =
                    SharedString::from(format!("source-control-directory-{}-{path}", group.key()));
                let row_path = path.clone();
                div()
                    .id(element_id)
                    .aria_label(path.clone())
                    .tab_index(0)
                    .relative()
                    .w_full()
                    .min_w_0()
                    .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                    .flex_none()
                    .pl(px(12.0 + depth as f32 * SOURCE_CONTROL_TREE_INDENT))
                    .pr(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(5.0))
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .focus_visible(|element| element.border_1().border_color(theme.accent_focus))
                    .child(source_control_guides(depth, &theme))
                    .child(icon(
                        if open {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        10.0,
                        theme.text_ghost,
                    ))
                    .child(icon("icons/folder.svg", 13.0, theme.text_ghost))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(11.5))
                            .text_color(theme.text_tertiary)
                            .child(name),
                    )
                    .on_activation(cx, move |this, _, cx| {
                        this.toggle_source_control_directory(group, row_path.clone(), cx);
                    })
                    .into_any_element()
            }
            SourceControlRow::File {
                group,
                path,
                title,
                name,
                depth,
                letter,
                ..
            } => {
                let element_id =
                    SharedString::from(format!("source-control-file-{}-{path}", group.key()));
                let staged = group == SourceControlGroup::Staged;
                let title_id =
                    SharedString::from(format!("source-control-file-title-{}-{path}", group.key()));
                let row_path = path.clone();
                let selected_source = self.right_panel_diff_source
                    == if staged {
                        ReviewDiffSource::Staged
                    } else {
                        ReviewDiffSource::Unstaged
                    };
                let selected_path = self.review_pending_file_focus.as_deref().or_else(|| {
                    self.right_panel_diff_selected_file.and_then(|index| {
                        self.right_panel_diff_snapshot
                            .as_ref()?
                            .files
                            .get(index)
                            .map(|file| file.path.as_str())
                    })
                });
                let selected = selected_source && selected_path == Some(path.as_str());
                div()
                    .id(element_id)
                    .aria_label(title.clone())
                    .relative()
                    .w_full()
                    .min_w_0()
                    .h(ui_px(SOURCE_CONTROL_ROW_HEIGHT))
                    .flex_none()
                    .pl(px(12.0 + depth as f32 * SOURCE_CONTROL_TREE_INDENT))
                    .pr(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(5.0))
                    .cursor_default()
                    .when(selected, |element| element.bg(theme.overlay_strong))
                    .when(!selected, |element| {
                        element.hover(|element| element.bg(theme.overlay))
                    })
                    .focus_visible(|element| element.border_1().border_color(theme.accent_focus))
                    .when(group != SourceControlGroup::Ignored, |element| {
                        element.tab_index(0).on_activation(cx, move |this, _, cx| {
                            this.open_source_control_file(staged, row_path.clone(), cx);
                        })
                    })
                    .child(source_control_guides(depth, &theme))
                    // The chevron slot keeps file names aligned with the
                    // directory names beside them.
                    .child(div().w(px(10.0)).flex_none())
                    .child(file_icon(file_icon_for_path(&path), 13.0))
                    .child(
                        div()
                            .id(title_id)
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_ellipsis_middle()
                            .text_size(ui_px(11.5))
                            .font_weight(if selected {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(if selected {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .tooltip(Tooltip::text(title.clone()))
                            .child(name),
                    )
                    .child(
                        div()
                            .w(px(12.0))
                            .flex_none()
                            .text_right()
                            .text_size(ui_px(10.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(status_letter_color(letter, &theme).opacity(0.9))
                            .child(letter.to_string()),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_source_control_group_menu(
        &self,
        group: SourceControlGroup,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let menu_id = if group == SourceControlGroup::Untracked {
            "source-control-untracked-actions"
        } else {
            "source-control-group-actions"
        };
        let handle = self.menu_handle(menu_id, cx);
        let weak = cx.entity().downgrade();
        let enabled = self.source_control_allows_writes();
        dropdown_menu(
            MenuChip::new(menu_id)
                .icon("icons/ellipsis.svg", theme.text_tertiary)
                .icon_only(true)
                .height(px(24.0))
                .tooltip(tr!("source_control.group_actions"))
                .selected(handle.is_open()),
            menu_id,
            &handle,
            MenuAlign::BelowRight,
            move |_| {
                vec![
                    MenuItem::new(tr!("source_control.discard_all"), {
                        let weak = weak.clone();
                        move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.request_discard_source_control_group(group, cx)
                            });
                        }
                    })
                    .disabled(!enabled),
                ]
            },
        )
    }

    /// One small text action of a group header. Muted until hovered, so the
    /// header stays readable.
    fn render_source_control_text_action(
        &self,
        id: &'static str,
        label: String,
        activate: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id(id)
            .tab_index(0)
            .h(px(24.0))
            .px(px(4.0))
            .rounded(px(4.0))
            .flex_none()
            .flex()
            .items_center()
            .font_weight(FontWeight::MEDIUM)
            .cursor_default()
            .text_size(ui_px(11.0))
            .text_color(theme.text_tertiary)
            .hover(|button| {
                button
                    .bg(theme.overlay_strong)
                    .text_color(theme.text_secondary)
            })
            .focus_visible(|button| button.border_1().border_color(theme.accent_focus))
            .on_activation(cx, move |this, _, cx| activate(this, cx))
            .child(label)
    }

    /// The commit dialog: the message input, the AI shortcut, and the two
    /// commit scopes — staged only, or stage everything first. Esc or the
    /// scrim closes it.
    pub(super) fn render_source_control_commit_dialog(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.source_control_commit_dialog {
            return None;
        }
        let theme = Theme::current(cx);
        let busy = self.source_control_busy.is_some();
        let branch = self
            .source_control_status
            .as_ref()
            .and_then(|status| status.branch.clone())
            .unwrap_or_else(|| tr!("source_control.detached_head"));
        let staged = !self.source_control_staged_paths().is_empty();
        let has_message = !self
            .source_control_commit_input
            .read(cx)
            .content()
            .trim()
            .is_empty();
        let has_session = self.selected_session().is_some();
        let has_provider = self.provider_probe().is_some();
        let can_generate = !busy && staged && has_session && has_provider;
        let can_commit = !busy && staged && has_message;
        // "Stage all and commit" brings its own changes, so it only needs a
        // dirty tree — staged or not — plus a message.
        let can_stage_all_commit = !busy
            && has_message
            && self.source_control_untracked_fully_loaded()
            && (staged || !self.source_control_unstaged_paths().is_empty());
        let weak = cx.entity().downgrade();
        let message = self.source_control_commit_input.clone();

        let action_row = |id: &'static str,
                          label: String,
                          primary: bool,
                          enabled: bool,
                          action: fn(&mut Self, &mut Context<Self>)|
         -> Stateful<Div> {
            let weak = weak.clone();
            div()
                .id(id)
                .tab_index(0)
                .h(px(28.0))
                .px(px(12.0))
                .rounded(px(6.0))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.0))
                .text_size(ui_px(11.5))
                .font_weight(FontWeight::MEDIUM)
                .when(enabled, |button| {
                    button.cursor_default().on_click(move |_, _, cx| {
                        let _ = weak.update(cx, action);
                    })
                })
                .when(primary && enabled, |button| {
                    button
                        .bg(theme.inverse)
                        .text_color(theme.on_inverse)
                        .hover(|button| button.opacity(0.9))
                })
                .when(!primary && enabled, |button| {
                    button
                        .text_color(theme.text_secondary)
                        .hover(|button| button.bg(theme.overlay))
                })
                .when(!enabled, |button| {
                    button.bg(theme.overlay_strong).text_color(theme.text_ghost)
                })
                .focus_visible(|button| button.border_1().border_color(theme.accent_focus))
                .child(label)
        };

        let card = div()
            .w(px(420.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface)
            .shadow_lg()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px(px(16.0))
                    .pt(px(14.0))
                    .pb(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(icon("icons/git-commit-horizontal.svg", 14.0, theme.text))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(tr!("source_control.commit_to", branch = branch)),
                    ),
            )
            .child(
                div()
                    .mx(px(8.0))
                    .h(px(96.0))
                    .overflow_hidden()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.composer)
                    // Code-mode editors inherit the caller's text metrics;
                    // pin them to the app's standard so the message does not
                    // render at the root default size.
                    .text_size(ui_px(13.5))
                    .line_height(ui_px(22.0))
                    .child(message),
            )
            .child(
                div()
                    .p(px(8.0))
                    .pt(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id("source-control-commit-generate")
                            .tab_index(0)
                            .size(px(28.0))
                            .rounded(px(6.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(can_generate, |button| {
                                button
                                    .cursor_default()
                                    .hover(|button| button.bg(theme.overlay))
                            })
                            .when(!can_generate, |button| button.opacity(0.4))
                            .tooltip(Tooltip::text(tr!("source_control.generate_message")))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.generate_source_control_commit_message(cx);
                            }))
                            .child(icon("icons/sparkle.svg", 13.0, theme.text_tertiary)),
                    )
                    .child(div().flex_1())
                    .child(action_row(
                        "source-control-commit-stage-all-run",
                        tr!("source_control.stage_all_commit"),
                        false,
                        can_stage_all_commit,
                        |this, cx| {
                            this.close_source_control_commit_dialog(cx);
                            this.commit_source_control(true, false, cx);
                        },
                    ))
                    .child(action_row(
                        "source-control-commit-run",
                        tr!("source_control.commit"),
                        true,
                        can_commit,
                        |this, cx| {
                            this.close_source_control_commit_dialog(cx);
                            this.commit_source_control(false, false, cx);
                        },
                    )),
            );
        let scrim = if theme.is_dark {
            gpui::hsla(0.0, 0.0, 0.0, 0.34)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        };
        let layer = div()
            .id("source-control-commit-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(scrim)
            .p(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_source_control_commit_dialog(cx)),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close_source_control_commit_dialog(cx);
                    cx.stop_propagation();
                }
            }))
            .child(card);
        Some(gpui::deferred(layer).with_priority(4).into_any_element())
    }

    /// The discard confirmation: what would be restored, and — for
    /// untracked files — the warning that Git cannot bring them back.
    pub(super) fn render_source_control_discard_dialog(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let request = self.source_control_discard.as_ref()?;
        let theme = Theme::current(cx);
        let total = request.total();
        let busy = self.source_control_busy.is_some();
        let weak = cx.entity().downgrade();
        let cancel = {
            let weak = weak.clone();
            div()
                .id("source-control-discard-cancel")
                .tab_index(0)
                .h(px(28.0))
                .px(px(12.0))
                .rounded(px(6.0))
                .flex_none()
                .flex()
                .items_center()
                .cursor_default()
                .text_size(ui_px(11.5))
                .text_color(theme.text_secondary)
                .hover(|button| button.bg(theme.overlay))
                .focus_visible(|button| button.border_1().border_color(theme.accent_focus))
                .child(tr!("source_control.discard_cancel"))
                .on_click(move |_, _, cx| {
                    let _ = weak.update(cx, |this, cx| this.cancel_source_control_discard(cx));
                })
        };
        let confirm = {
            let weak = weak.clone();
            div()
                .id("source-control-discard-confirm")
                .tab_index(0)
                .h(px(28.0))
                .px(px(12.0))
                .rounded(px(6.0))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.0))
                .when(!busy, |button| {
                    button
                        .cursor_default()
                        .bg(theme.danger)
                        .text_color(theme.on_inverse)
                        .hover(|button| button.opacity(0.9))
                })
                .when(busy, |button| button.opacity(0.5))
                .text_size(ui_px(11.5))
                .font_weight(FontWeight::MEDIUM)
                .focus_visible(|button| button.border_1().border_color(theme.accent_focus))
                .child(tr!("source_control.discard_confirm", count = total))
                .on_click(move |_, _, cx| {
                    let _ = weak.update(cx, |this, cx| this.confirm_source_control_discard(cx));
                })
        };
        let card = div()
            .w(px(360.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface)
            .shadow_lg()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px(px(16.0))
                    .pt(px(14.0))
                    .pb(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(icon("icons/alert.svg", 14.0, theme.danger))
                    .child(
                        div()
                            .text_size(ui_px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(tr!("source_control.discard_title")),
                    ),
            )
            .child(
                div()
                    .px(px(16.0))
                    .pb(px(10.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .when(!request.tracked.is_empty(), |column| {
                        column.child(
                            div()
                                .text_size(ui_px(11.5))
                                .line_height(ui_px(16.0))
                                .text_color(theme.text_secondary)
                                .child(tr!(
                                    "source_control.discard_tracked",
                                    count = request.tracked.len()
                                )),
                        )
                    })
                    .when(!request.untracked.is_empty(), |column| {
                        column.child(
                            div()
                                .text_size(ui_px(11.5))
                                .line_height(ui_px(16.0))
                                .text_color(theme.danger_text)
                                .child(tr!(
                                    "source_control.discard_untracked_note",
                                    count = request.untracked.len()
                                )),
                        )
                    }),
            )
            .child(div().mx(px(8.0)).h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .p(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .justify_end()
                    .child(cancel)
                    .child(confirm),
            );
        let scrim = if theme.is_dark {
            gpui::hsla(0.0, 0.0, 0.0, 0.34)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        };
        let layer = div()
            .id("source-control-discard-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(scrim)
            .p(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.cancel_source_control_discard(cx)),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_source_control_discard(cx);
                    cx.stop_propagation();
                }
            }))
            .child(card);
        Some(gpui::deferred(layer).with_priority(4).into_any_element())
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
        // Per-file actions sit beside the file name: stage and discard exist
        // only for files with an unstaged side, unstage only for files with
        // a staged one. A history snapshot is read-only, so none render.
        let entry = self
            .source_control_review_files
            .entry(self.source_control_status.as_deref(), &path);
        let has_unstaged = entry.is_some_and(|entry| entry.worktree_status != ' ');
        let has_staged = entry.is_some_and(source_control_entry_is_staged);
        let writable = !self.review_is_history_snapshot() && self.source_control_busy.is_none();
        let can_stage = writable && has_unstaged;
        let can_discard = writable && has_unstaged;
        let can_unstage = writable && has_staged;
        let can_expand_context = !self.right_panel_diff_loading
            && self.source_control_diff_context < 100_000
            && self
                .source_control_review_files
                .for_source(self.right_panel_diff_source)
                .is_some()
            && file.is_some_and(|file| file.status == crate::review_diff::FileStatus::Modified);

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
            .when(can_expand_context, |header| {
                header.child(self.render_source_control_text_action(
                    "source-control-more-context",
                    tr!("diff.more_context"),
                    |this, cx| {
                        this.source_control_diff_context = this
                            .source_control_diff_context
                            .saturating_add(100)
                            .min(100_000);
                        this.refresh_review_diff(cx);
                    },
                    cx,
                ))
            })
            .when(can_stage, |header| {
                header.child(self.render_source_control_file_action(
                    "source-control-file-header-stage",
                    tr!("source_control.stage"),
                    cx.listener(|this, _, _, cx| {
                        let path = this.source_control_selected_path();
                        if let Some(path) = path {
                            this.stage_source_control_paths(vec![path], cx);
                        }
                    }),
                    cx,
                ))
            })
            .when(can_discard, |header| {
                header.child(self.render_source_control_file_action(
                    "source-control-file-header-discard",
                    tr!("source_control.discard"),
                    cx.listener(|this, _, _, cx| {
                        let path = this.source_control_selected_path();
                        if let Some(path) = path {
                            this.request_discard_source_control_file(path, cx);
                        }
                    }),
                    cx,
                ))
            })
            .when(can_unstage, |header| {
                header.child(self.render_source_control_file_action(
                    "source-control-file-header-unstage",
                    tr!("source_control.unstage"),
                    cx.listener(|this, _, _, cx| {
                        let path = this.source_control_selected_path();
                        if let Some(path) = path {
                            this.unstage_source_control_paths(vec![path], cx);
                        }
                    }),
                    cx,
                ))
            })
            .child(self.render_source_control_layout_toggle(cx))
    }

    /// The path of the file the reader is focused on, from the cached
    /// snapshot.
    fn source_control_selected_path(&self) -> Option<String> {
        self.right_panel_diff_selected_file
            .and_then(|index| {
                self.right_panel_diff_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.files.get(index))
            })
            .map(|file| file.path.clone())
    }

    /// One bordered text action beside the file name in the reader's header.
    fn render_source_control_file_action(
        &self,
        id: &'static str,
        label: String,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id(id)
            .tab_index(0)
            .h(px(22.0))
            .px(px(8.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(theme.border)
            .flex_none()
            .flex()
            .items_center()
            .cursor_default()
            .text_size(ui_px(10.5))
            .text_color(theme.text_secondary)
            .hover(|button| button.bg(theme.overlay).text_color(theme.text))
            .focus_visible(|button| button.border_1().border_color(theme.accent_focus))
            .on_click(on_click)
            .child(label)
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
        let stats = self
            .source_control_stats_key
            .as_ref()
            .filter(|(workspace, source)| {
                *source == selected
                    && self
                        .review_workspace_path()
                        .is_some_and(|current| current == workspace)
            })
            .and(self.source_control_stats)
            .filter(|stats| stats.additions > 0 || stats.deletions > 0);
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("review-diff-source", cx);
        let open = handle.is_open();
        let trigger = div()
            .id("review-diff-source")
            .flex_none()
            .h(px(26.0))
            .pl(px(10.0))
            .pr(px(8.0))
            .rounded_full()
            .border_1()
            .border_color(if open {
                theme.border_strong
            } else {
                theme.border
            })
            .bg(if open { theme.overlay } else { theme.surface })
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(ui_px(11.5))
            .line_height(ui_px(16.0))
            .hover(|pill| pill.bg(theme.overlay))
            .focus_visible(|pill| pill.border_color(theme.accent))
            .when_some(stats, |pill, stats| {
                let mut tooltip = tr!("diff.file_count", count = stats.files);
                if stats.truncated {
                    tooltip.push_str(" · ");
                    tooltip.push_str(&tr!("diff.stats_partial"));
                }
                pill.tooltip(Tooltip::text(tooltip))
            })
            .child(
                div()
                    .min_w_0()
                    .max_w(px(160.0))
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_secondary)
                    .child(source_label),
            )
            .child(icon("icons/chevron-down.svg", 10.0, theme.text_ghost))
            .when_some(stats, |pill, stats| {
                pill.child(div().flex_none().h(px(10.0)).w(px(1.0)).bg(theme.border))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.success_text)
                                    .child(format!("+{}", grouped_count(stats.additions))),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.danger_text)
                                    .child(format!("\u{2212}{}", grouped_count(stats.deletions))),
                            )
                            .when(stats.truncated, |group| {
                                group.child(div().text_color(theme.text_tertiary).child("\u{2026}"))
                            }),
                    )
            });
        let source = dropdown_menu(
            trigger,
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
                            let _ = choice_weak
                                .update(cx, |this, cx| this.set_review_diff_source(choice, cx));
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
                            let _ = choice_weak
                                .update(cx, |this, cx| this.set_review_diff_source(choice, cx));
                        })
                        .selected(choice == selected),
                    );
                }
                items
            },
        );

        let truncated = self
            .right_panel_diff_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.truncated);
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
            .on_click(cx.listener(|this, _, _, cx| this.refresh_review_surface(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.refresh_review_surface(cx);
                    cx.stop_propagation();
                }
            }));

        // File navigation: previous / next, and the position readout. Each
        // side disables at the end of the file list, like GitHub's PR bar.
        let (total, current) = self
            .source_control_live_selection()
            .map(|(files, current)| (files.paths.len(), current))
            .unwrap_or_else(|| {
                (
                    self.right_panel_diff_snapshot
                        .as_ref()
                        .map_or(0, |snapshot| snapshot.files.len()),
                    self.right_panel_diff_selected_file.unwrap_or(0),
                )
            });

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
        let selection = self.right_panel_diff_selection.clone();
        let focus = self.right_panel_diff_menu.trigger_focus_handle().clone();
        let mouse_focus = focus.clone();
        let keyboard_menu = self.right_panel_diff_menu.clone();
        let theme = Theme::current(cx);
        let reader = div()
            .id("review-diff-reader")
            .track_focus(&focus)
            .tab_index(0)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&mouse_focus, cx);
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "menu"
                    || (event.keystroke.key == "f10" && event.keystroke.modifiers.shift)
                {
                    keyboard_menu.open_context_menu(window, cx);
                    cx.stop_propagation();
                }
            })
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
            ));
        context_menu(
            reader,
            "review-diff-menu",
            &self.right_panel_diff_menu,
            move |_| {
                let selected = selection.selection.borrow().selected_text();
                let disabled = selected.is_none();
                vec![
                    MenuItem::new(tr!("common.copy_selection"), move |_, cx| {
                        if let Some(text) = selected.as_ref() {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        }
                    })
                    .disabled(disabled),
                ]
            },
        )
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
        let number_width = self.source_control_number_width;
        let side = |line_index: Option<usize>, old_side: bool| -> AnyElement {
            let Some(line_index) = line_index else {
                return div()
                    .flex_1()
                    .min_w_0()
                    .self_stretch()
                    .pl(px(8.0))
                    .bg(theme.overlay)
                    .into_any_element();
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
            let (marker, body_background, sign_color) = match &line.kind {
                crate::review_diff::LineKind::Addition => (
                    "+",
                    Some(theme.success.opacity(DIFF_ROW_WASH)),
                    theme.success_text,
                ),
                crate::review_diff::LineKind::Deletion => (
                    "\u{2212}",
                    Some(theme.danger.opacity(DIFF_ROW_WASH)),
                    theme.danger_text,
                ),
                _ => (" ", None, theme.text_ghost),
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
                .pl(px(8.0))
                .flex()
                .items_stretch()
                .when_some(body_background, |side, background| side.bg(background))
                .child(
                    div()
                        .w(px(number_width))
                        .flex_none()
                        .pr(px(8.0))
                        .text_right()
                        .text_color(theme.text_tertiary)
                        .children(number.map(|number| number.to_string())),
                )
                .child(
                    div()
                        .w(px(10.0))
                        .flex_none()
                        .text_color(sign_color)
                        .child(marker),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .pl(px(2.0))
                        .pr(px(8.0))
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
            .child(div().flex_1().min_w_0().flex().child(side(left, true)))
            .child(div().w(px(1.0)).flex_none().self_stretch().bg(theme.border))
            .child(div().flex_1().min_w_0().flex().child(side(right, false)))
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
                let expandable = gap.is_expandable()
                    || (gap.count() > 0
                        && self
                            .source_control_review_files
                            .for_source(self.right_panel_diff_source)
                            .is_some()
                        && self.source_control_diff_context < 100_000);
                let chunked = gap.count() > crate::review_diff::DEFAULT_EXPANSION_LINE_COUNT as u32;
                let directions = review_diff_gap_directions(gap.position, chunked);
                let two_directions = directions.len() > 1;
                let gutter = div()
                    .w(px(self.source_control_number_width * 2.0 + 24.0))
                    .h_full()
                    .flex_none()
                    .flex()
                    .when(two_directions, |gutter| gutter.flex_col())
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
                let label_focus =
                    self.transcript_control_focus(format!("review-diff-gap-{}-label", gap.id), cx);
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
                    .justify_center()
                    .bg(theme.overlay)
                    .child(format!(
                        "\u{22ef} {}",
                        tr!("diff.unmodified_lines", count = gap.count())
                    ))
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
                .items_center()
                .font_family(md::render::mono_family())
                .text_size(code_px(10.0))
                .line_height(code_px(16.0))
                .text_color(theme.text_tertiary)
                .bg(theme.overlay)
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
                        .child(line.content.clone()),
                )
                .into_any_element(),
            crate::review_diff::LineKind::Meta => div()
                .min_h(px(24.0))
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .font_family(md::render::mono_family())
                .text_size(code_px(10.5))
                .line_height(code_px(16.0))
                .text_color(theme.text_tertiary)
                .child(
                    div()
                        .min_h(px(24.0))
                        .min_w_0()
                        .flex_1()
                        .px(px(12.0))
                        .py(px(4.0))
                        .overflow_hidden()
                        .whitespace_normal()
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
                DiffRowStyle::review(self.source_control_number_width),
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
        let focus = self.transcript_control_focus(
            format!("review-diff-gap-{gap_id}-button-{direction_name}"),
            cx,
        );
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
        let compact = self.right_panel_diff_snapshot.as_ref()
            .and_then(|snapshot| snapshot.lines.get(line_index))
            .is_some_and(|line| matches!(&line.kind, crate::review_diff::LineKind::Gap(gap) if !gap.is_expandable()));
        if compact
            && self
                .source_control_review_files
                .for_source(self.right_panel_diff_source)
                .is_some()
        {
            self.source_control_diff_context = match direction {
                crate::review_diff::ExpansionDirection::All => 100_000,
                _ => self
                    .source_control_diff_context
                    .saturating_add(100)
                    .min(100_000),
            };
            self.refresh_review_diff(cx);
            return;
        }
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

    fn source_control_live_selection(&self) -> Option<(&SourceControlFileList, usize)> {
        let files = self
            .source_control_review_files
            .for_source(self.right_panel_diff_source)?;
        let path = self.review_pending_file_focus.as_deref().or_else(|| {
            self.right_panel_diff_selected_file.and_then(|index| {
                self.right_panel_diff_snapshot
                    .as_ref()?
                    .files
                    .get(index)
                    .map(|file| file.path.as_str())
            })
        })?;
        Some((files, *files.indices.get(path)?))
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
        self.refresh_review_stats(cx);
        cx.notify();
    }

    /// Fetches the whole source's line totals for the toolbar capsule. This is
    /// deliberately separate from the file diff: the toolbar needs one figure
    /// for the area, and the reader still only loads the file it shows.
    ///
    /// A request already in flight for the same workspace and source wins; a
    /// call that arrives meanwhile only marks that answer stale, and the
    /// result asks for one more pass.
    pub(super) fn refresh_review_stats(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.review_workspace_path().map(Path::to_path_buf) else {
            let request = self.source_control_stats_request.take();
            self.cancel_source_control_query(request, cx);
            self.source_control_stats_generation =
                self.source_control_stats_generation.wrapping_add(1);
            self.source_control_stats = None;
            self.source_control_stats_key = None;
            self.source_control_stats_dirty = false;
            return;
        };
        let source = self.right_panel_diff_source;
        let key = (workspace.clone(), source);
        let same_key = self.source_control_stats_key.as_ref() == Some(&key);
        if same_key && self.source_control_stats_request.is_some() {
            // The answer in flight predates this state change. Ask again once
            // it lands rather than firing a second Git call now.
            self.source_control_stats_dirty = true;
            return;
        }
        if !same_key {
            self.source_control_stats = None;
        }
        self.source_control_stats_key = Some(key);
        self.source_control_stats_dirty = false;
        self.source_control_stats_generation = self.source_control_stats_generation.wrapping_add(1);
        let generation = self.source_control_stats_generation;
        let previous = self.source_control_stats_request.take();
        self.cancel_source_control_query(previous, cx);
        let query_id = Uuid::new_v4();
        let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        self.source_control_stats_request = Some(PendingGitQuery {
            id: query_id,
            client: client.clone(),
        });
        cx.spawn(async move |fintwind, cx| {
            let request_workspace = workspace.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.request(fintwind_client::WorkspaceOperation::CollectReviewStats {
                        cwd: request_workspace,
                        source: crate::review_diff::wire_source(source),
                        query_id: Some(query_id),
                    })
                })
                .await;
            fintwind
                .update(cx, |this, cx| {
                    if this.source_control_stats_generation != generation {
                        return;
                    }
                    this.source_control_stats_request = None;
                    this.source_control_stats = match result {
                        Ok(fintwind_client::WorkspaceResult::ReviewStats {
                            additions,
                            deletions,
                            files,
                            truncated,
                        }) => Some(ReviewAreaStats {
                            additions,
                            deletions,
                            files,
                            truncated,
                        }),
                        _ => None,
                    };
                    if std::mem::take(&mut this.source_control_stats_dirty) {
                        this.refresh_review_stats(cx);
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// The toolbar's refresh: both the file the reader shows and the area
    /// totals beside its name, so the number never lags the file.
    fn refresh_review_surface(&mut self, cx: &mut Context<Self>) {
        self.refresh_review_diff(cx);
        self.refresh_review_stats(cx);
    }

    /// Captures one stable Git range and turns it into render-ready rows. Git,
    /// patch parsing, and syntax tokenization all stay off the UI thread; the
    /// generation check prevents an old source or session from landing late.
    pub(super) fn refresh_review_diff(&mut self, cx: &mut Context<Self>) {
        self.cancel_source_control_file_diff(cx);
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
        let previous_path = self.right_panel_diff_selected_file.and_then(|index| {
            self.right_panel_diff_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.files.get(index))
                .map(|file| file.path.clone())
        });
        if let Some(pending) = self.review_pending_file_focus.clone()
            && let Some(files) = self.source_control_review_files.for_source(source)
            && !files.indices.contains_key(&pending)
        {
            let waiting_for_status = self.source_control_status_loading
                || (source != ReviewDiffSource::Staged
                    && !self.source_control_untracked_complete
                    && self.source_control_untracked_error.is_none());
            if waiting_for_status {
                self.right_panel_diff_loading = true;
                cx.notify();
                return;
            }
            let directory = (source != ReviewDiffSource::Staged)
                .then(|| {
                    self.source_control_status.as_ref().and_then(|status| {
                        status
                            .entries
                            .iter()
                            .filter(|entry| {
                                entry.worktree_status == '?'
                                    && entry.path.ends_with('/')
                                    && pending.starts_with(&entry.path)
                            })
                            .map(|entry| entry.path.trim_end_matches('/'))
                            .filter(|path| {
                                !self.source_control_untracked_loaded.contains(*path)
                                    && !self.source_control_untracked_incomplete.contains(*path)
                            })
                            .max_by_key(|path| path.len())
                            .map(str::to_owned)
                    })
                })
                .flatten();
            if let Some(directory) = directory {
                self.reveal_source_control_file(&pending);
                // A partly listed directory continues from its cursor, so
                // waiting for a file on a later page terminates.
                let after = self.source_control_untracked_next.get(&directory).cloned();
                self.refresh_source_control_untracked(Some(directory), after, cx);
                self.right_panel_diff_loading = true;
                cx.notify();
                return;
            }
            self.review_pending_file_focus = None;
        }
        let mut selected_path = self.review_pending_file_focus.clone().or(previous_path);
        let file_request = if let Some(files) = self.source_control_review_files.for_source(source)
        {
            if selected_path
                .as_deref()
                .is_none_or(|path| !files.indices.contains_key(path))
            {
                selected_path = files.paths.first().cloned();
            }
            let Some(path) = selected_path.as_ref() else {
                self.right_panel_diff_snapshot = None;
                self.right_panel_diff_selected_file = None;
                self.right_panel_diff_loading = false;
                self.right_panel_diff_error = None;
                self.sync_source_control_diff_rows();
                cx.notify();
                return;
            };
            let Some(entry) = self
                .source_control_review_files
                .entry(self.source_control_status.as_deref(), path)
            else {
                self.right_panel_diff_loading = false;
                return;
            };
            Some((
                entry.path.clone(),
                entry.origin_path.clone(),
                entry.worktree_status == '?',
            ))
        } else {
            None
        };
        let context_lines = self.source_control_diff_context;
        let query_id = file_request.as_ref().map(|_| Uuid::new_v4());
        let workspace = fintwind_client::WorkspaceClient::new(self.daemon.client());
        self.source_control_diff_request = query_id.map(|id| PendingGitQuery {
            id,
            client: workspace.clone(),
        });
        self.right_panel_diff_loading = true;
        self.right_panel_diff_error = None;
        cx.notify();

        cx.spawn(async move |fintwind, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let project_path = project_path.clone();
                    async move {
                        let operation = if let Some((path, origin_path, untracked)) = file_request {
                            fintwind_client::WorkspaceOperation::CollectReviewFileDiff {
                                cwd: project_path,
                                source: crate::review_diff::wire_source(source),
                                path,
                                origin_path,
                                untracked,
                                context_lines,
                                query_id,
                            }
                        } else {
                            fintwind_client::WorkspaceOperation::CollectReviewDiff {
                                cwd: project_path,
                                source: crate::review_diff::wire_source(source),
                            }
                        };
                        match workspace.request(operation)? {
                            fintwind_client::WorkspaceResult::ReviewDiff { data } => {
                                Ok(crate::review_diff::parse_collected(
                                    source,
                                    &data.numstat,
                                    &data.patch,
                                    data.complete_context,
                                    data.truncated,
                                    data.file.as_ref(),
                                ))
                            }
                            _ => anyhow::bail!("the daemon returned an invalid diff response"),
                        }
                    }
                })
                .await;
            fintwind
                .update(cx, |fintwind, cx| {
                    if query_id.is_some()
                        && fintwind
                            .source_control_diff_request
                            .as_ref()
                            .is_some_and(|request| Some(request.id) == query_id)
                    {
                        if result.is_err() {
                            fintwind.cancel_source_control_file_diff(cx);
                        } else {
                            fintwind.source_control_diff_request = None;
                        }
                    }
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

    fn cancel_source_control_file_diff(&mut self, cx: &mut Context<Self>) {
        let query_id = self.source_control_diff_request.take();
        self.cancel_source_control_query(query_id, cx);
    }

    fn cancel_nested_repository_queries(&mut self, cx: &mut Context<Self>) {
        let list_request = self.source_control_nested_list_request.take();
        self.cancel_source_control_query(list_request, cx);
        let mut requests = Vec::new();
        take_nested_requests(&mut self.source_control_nested, &mut requests);
        for request in requests {
            self.cancel_source_control_query(Some(request), cx);
        }
    }

    fn reload_stale_nested_repositories(&mut self, cx: &mut Context<Self>) {
        let mut paths = Vec::new();
        collect_stale_nested_paths(&self.source_control_nested, &mut paths);
        for path in paths {
            self.load_nested_repository(path, cx);
        }
    }

    fn cancel_source_control_query(
        &self,
        request: Option<PendingGitQuery>,
        cx: &mut Context<Self>,
    ) {
        let Some(PendingGitQuery {
            id: query_id,
            client,
        }) = request
        else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let _ = client
                    .request(fintwind_client::WorkspaceOperation::CancelGitQuery { query_id });
            })
            .detach();
    }
}

/// A discard awaiting the user's confirmation: the workspace it targets and
/// the tracked/untracked split the daemon expects. The dialog shows both
/// sides, because deleting untracked files cannot be undone by Git.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DiscardRequest {
    pub(super) workspace: PathBuf,
    /// Tracked files that restore from the index; the staged side survives.
    pub(super) tracked: Vec<String>,
    /// Untracked files deletion would erase — Git cannot bring them back.
    pub(super) untracked: Vec<String>,
}

impl DiscardRequest {
    fn total(&self) -> usize {
        self.tracked.len() + self.untracked.len()
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
            LineKind::FileHeader | LineKind::HunkHeader | LineKind::Gap(_) | LineKind::Meta => {
                rows.push(SourceControlDiffRow::Line(at));
                at += 1;
            }
        }
    }
    rows
}

/// One flattened row of the source-control second column: a group header, a
/// directory of the change tree, or a file from one of its groups. `title`
/// carries the rename form `origin → path` when Git detected a rename;
/// `untracked` marks an entry Git has no record of, whose discard deletes
/// the file.
#[derive(Default)]
pub(super) struct SourceControlReviewFiles {
    uncommitted: SourceControlFileList,
    unstaged: SourceControlFileList,
    staged: SourceControlFileList,
    status_indices: std::collections::HashMap<String, usize>,
}

pub(super) struct NestedRepositoryState {
    repository: fintwind_client::git::NestedRepository,
    open: bool,
    loading: bool,
    stale: bool,
    request: Option<PendingGitQuery>,
    status: Option<fintwind_client::git::WorktreeStatus>,
    /// Whether the nested repository's untracked listing reached its end.
    untracked_complete: bool,
    error: Option<String>,
    children: Vec<NestedRepositoryState>,
}

fn take_nested_requests(nodes: &mut [NestedRepositoryState], requests: &mut Vec<PendingGitQuery>) {
    for node in nodes {
        if let Some(request) = node.request.take() {
            requests.push(request);
        }
        node.loading = false;
        node.open = false;
        node.stale = false;
        node.status = None;
        node.untracked_complete = false;
        node.error = None;
        take_nested_requests(&mut node.children, requests);
    }
}

fn invalidate_nested_requests(
    nodes: &mut [NestedRepositoryState],
    requests: &mut Vec<PendingGitQuery>,
) {
    for node in nodes {
        if let Some(request) = node.request.take() {
            requests.push(request);
        }
        node.loading = false;
        node.stale = true;
        invalidate_nested_requests(&mut node.children, requests);
    }
}

fn collect_stale_nested_paths(nodes: &[NestedRepositoryState], paths: &mut Vec<PathBuf>) {
    for node in nodes {
        if !node.open {
            continue;
        }
        if node.stale && !node.loading && node.repository.initialized {
            paths.push(node.repository.path.clone());
        }
        collect_stale_nested_paths(&node.children, paths);
    }
}

fn merge_nested_repositories(
    previous: Vec<NestedRepositoryState>,
    repositories: Vec<fintwind_client::git::NestedRepository>,
    removed_requests: &mut Vec<PendingGitQuery>,
) -> Vec<NestedRepositoryState> {
    let mut previous: std::collections::HashMap<_, _> = previous
        .into_iter()
        .map(|node| (node.repository.path.clone(), node))
        .collect();
    let merged = repositories
        .into_iter()
        .map(|repository| {
            if let Some(mut node) = previous.remove(&repository.path) {
                node.repository = repository;
                node
            } else {
                NestedRepositoryState {
                    repository,
                    open: false,
                    loading: false,
                    stale: false,
                    request: None,
                    status: None,
                    untracked_complete: false,
                    error: None,
                    children: Vec::new(),
                }
            }
        })
        .collect();
    for (_, mut removed) in previous {
        take_nested_requests(std::slice::from_mut(&mut removed), removed_requests);
    }
    merged
}

fn find_nested_repository_mut<'a>(
    nodes: &'a mut [NestedRepositoryState],
    path: &Path,
) -> Option<&'a mut NestedRepositoryState> {
    for node in nodes {
        if node.repository.path == path {
            return Some(node);
        }
        if let Some(found) = find_nested_repository_mut(&mut node.children, path) {
            return Some(found);
        }
    }
    None
}

fn nested_repository_candidates(status: &fintwind_client::git::WorktreeStatus) -> Vec<String> {
    status
        .entries
        .iter()
        .filter(|entry| entry.worktree_status == '?' && entry.path.ends_with('/'))
        .map(|entry| entry.path.clone())
        .collect()
}

#[derive(Default)]
struct SourceControlFileList {
    paths: Vec<String>,
    indices: std::collections::HashMap<String, usize>,
}

impl SourceControlFileList {
    fn push(&mut self, path: &str) {
        self.indices.insert(path.to_owned(), self.paths.len());
        self.paths.push(path.to_owned());
    }
}

impl SourceControlReviewFiles {
    fn from_status(status: Option<&fintwind_client::git::WorktreeStatus>) -> Self {
        let mut files = Self::default();
        if let Some(status) = status {
            for (index, entry) in status.entries.iter().enumerate() {
                files.status_indices.insert(entry.path.clone(), index);
                let staged = source_control_entry_is_staged(entry);
                let unstaged = source_control_entry_is_unstaged(entry);
                if staged || unstaged {
                    files.uncommitted.push(&entry.path);
                }
                if staged {
                    files.staged.push(&entry.path);
                }
                if unstaged {
                    files.unstaged.push(&entry.path);
                }
            }
        }
        files
    }

    fn for_source(&self, source: ReviewDiffSource) -> Option<&SourceControlFileList> {
        match source {
            ReviewDiffSource::Uncommitted => Some(&self.uncommitted),
            ReviewDiffSource::Unstaged => Some(&self.unstaged),
            ReviewDiffSource::Staged => Some(&self.staged),
            _ => None,
        }
    }

    fn entry<'a>(
        &self,
        status: Option<&'a fintwind_client::git::WorktreeStatus>,
        path: &str,
    ) -> Option<&'a fintwind_client::git::WorktreeStatusEntry> {
        status?.entries.get(*self.status_indices.get(path)?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceControlGroup {
    Staged,
    Unstaged,
    Untracked,
    Ignored,
}

impl SourceControlGroup {
    const ALL: [Self; 3] = [Self::Staged, Self::Unstaged, Self::Untracked];

    fn key(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Untracked => "untracked",
            Self::Ignored => "ignored",
        }
    }

    fn matches(self, entry: &fintwind_client::git::WorktreeStatusEntry) -> bool {
        match self {
            Self::Staged => source_control_entry_is_staged(entry),
            Self::Unstaged => {
                source_control_entry_is_unstaged(entry) && entry.worktree_status != '?'
            }
            Self::Untracked => entry.worktree_status == '?',
            Self::Ignored => entry.worktree_status == '!',
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SourceControlRow {
    Header {
        group: SourceControlGroup,
        count: usize,
        incomplete: bool,
    },
    IgnoredHeader {
        open: bool,
        loading: bool,
    },
    IgnoredError {
        message: String,
    },
    IgnoredEmpty,
    UntrackedLoading,
    /// The next page of a directory listing. `directory` is `None` for the
    /// worktree root.
    LoadMore {
        group: SourceControlGroup,
        directory: Option<String>,
        depth: usize,
    },
    /// The listing hit its budget; only a narrower directory can show more.
    ListingIncomplete {
        group: SourceControlGroup,
        directory: Option<String>,
        depth: usize,
    },
    Repository {
        path: PathBuf,
        name: String,
        depth: usize,
        open: bool,
        loading: bool,
        initialized: bool,
    },
    NestedHeader {
        group: SourceControlGroup,
        count: usize,
        depth: usize,
        incomplete: bool,
    },
    NestedDirectory {
        repository: PathBuf,
        path: String,
        depth: usize,
    },
    NestedFile {
        repository: PathBuf,
        group: SourceControlGroup,
        path: String,
        depth: usize,
        letter: char,
    },
    Directory {
        group: SourceControlGroup,
        path: String,
        name: String,
        depth: usize,
        open: bool,
    },
    File {
        group: SourceControlGroup,
        path: String,
        title: String,
        name: String,
        depth: usize,
        letter: char,
        untracked: bool,
    },
}

fn source_control_nested_rows(
    rows: &mut Vec<SourceControlRow>,
    nodes: &[NestedRepositoryState],
    depth: usize,
    filter: &str,
) {
    for node in nodes {
        rows.push(SourceControlRow::Repository {
            path: node.repository.path.clone(),
            name: node.repository.relative_path.clone(),
            depth,
            open: node.open,
            loading: node.loading,
            initialized: node.repository.initialized,
        });
        if !node.open {
            continue;
        }
        if let Some(error) = node.error.as_ref() {
            rows.push(SourceControlRow::IgnoredError {
                message: error.clone(),
            });
        }
        if let Some(status) = node.status.as_ref() {
            let nested_paths: std::collections::HashSet<&str> = node
                .children
                .iter()
                .map(|child| child.repository.relative_path.as_str())
                .collect();
            for group in SourceControlGroup::ALL {
                let entries: Vec<_> = status
                    .entries
                    .iter()
                    .filter(|entry| {
                        group.matches(entry)
                            && !(group == SourceControlGroup::Untracked
                                && entry.path.ends_with('/')
                                && nested_paths.contains(entry.path.trim_end_matches('/')))
                            && (filter.is_empty()
                                || entry.path.to_ascii_lowercase().contains(filter))
                    })
                    .collect();
                if entries.is_empty() {
                    continue;
                }
                rows.push(SourceControlRow::NestedHeader {
                    group,
                    count: entries
                        .iter()
                        .filter(|entry| !entry.path.ends_with('/'))
                        .count(),
                    depth: depth + 1,
                    incomplete: group == SourceControlGroup::Untracked
                        && (node.error.is_some() || !node.untracked_complete),
                });
                for entry in entries {
                    if entry.path.ends_with('/') {
                        rows.push(SourceControlRow::NestedDirectory {
                            repository: node.repository.path.clone(),
                            path: entry.path.clone(),
                            depth: depth + 1,
                        });
                        continue;
                    }
                    rows.push(SourceControlRow::NestedFile {
                        repository: node.repository.path.clone(),
                        group,
                        path: entry.path.clone(),
                        depth: depth + 1,
                        letter: if group == SourceControlGroup::Staged {
                            entry.index_status
                        } else {
                            entry.worktree_status
                        },
                    });
                }
            }
            if !node.untracked_complete {
                rows.push(SourceControlRow::ListingIncomplete {
                    group: SourceControlGroup::Untracked,
                    directory: None,
                    depth: depth + 1,
                });
            }
        }
        source_control_nested_rows(rows, &node.children, depth + 1, filter);
    }
}

impl SourceControlRow {
    fn file(
        group: SourceControlGroup,
        depth: usize,
        entry: &fintwind_client::git::WorktreeStatusEntry,
    ) -> Self {
        let letter = if group == SourceControlGroup::Staged {
            entry.index_status
        } else {
            entry.worktree_status
        };
        let title = match entry.origin_path.as_deref() {
            Some(origin) => format!("{origin} → {}", entry.path),
            None => entry.path.clone(),
        };
        let name = entry
            .path
            .rsplit_once('/')
            .map_or(entry.path.as_str(), |(_, name)| name);
        Self::File {
            group,
            path: entry.path.clone(),
            title,
            name: name.to_owned(),
            depth,
            letter,
            untracked: entry.worktree_status == '?',
        }
    }
}

/// The source-control change tree's row height. Shared by the virtualized
/// list and every row so a uniform height stays exact.
const SOURCE_CONTROL_ROW_HEIGHT: f32 = 28.0;

/// One directory level's indent, matching Ely's tree list.
const SOURCE_CONTROL_TREE_INDENT: f32 = 16.0;

/// One directory while a group's change tree is built: subdirectories and
/// the files directly inside it, both keyed by name so the walk is stable.
#[derive(Default)]
struct SourceControlTreeNode<'a> {
    directories: std::collections::BTreeMap<String, SourceControlTreeNode<'a>>,
    files: std::collections::BTreeMap<&'a str, &'a fintwind_client::git::WorktreeStatusEntry>,
}

/// The fold set's key for one directory on one side of the tree.
fn source_control_directory_key(group: SourceControlGroup, path: &str) -> String {
    format!("{}:{path}", group.key())
}

/// Flattens one group's changed files into directory rows and file rows,
/// depth first. Directories keep their children behind a fold; files keep a
/// stable name order inside each directory.
fn source_control_tree_rows<'a>(
    rows: &mut Vec<SourceControlRow>,
    group: SourceControlGroup,
    entries: &[&'a fintwind_client::git::WorktreeStatusEntry],
    folded: &std::collections::HashSet<String>,
    force_open: bool,
    next_pages: &HashMap<String, String>,
    incomplete: &HashSet<String>,
) {
    let mut root = SourceControlTreeNode::<'a>::default();
    for entry in entries {
        let mut node = &mut root;
        let directory_entry = entry.path.ends_with('/');
        let mut segments = entry.path.trim_end_matches('/').split('/').peekable();
        while let Some(segment) = segments.next() {
            if segments.peek().is_none() {
                if directory_entry {
                    node.directories.entry(segment.to_owned()).or_default();
                } else {
                    node.files.insert(segment, *entry);
                }
                break;
            }
            node = node.directories.entry(segment.to_owned()).or_default();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk<'a>(
        node: &SourceControlTreeNode<'a>,
        group: SourceControlGroup,
        prefix: &str,
        depth: usize,
        folded: &std::collections::HashSet<String>,
        force_open: bool,
        next_pages: &HashMap<String, String>,
        incomplete: &HashSet<String>,
        rows: &mut Vec<SourceControlRow>,
    ) {
        for (name, child) in &node.directories {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let key = source_control_directory_key(group, &path);
            let open = force_open
                || if matches!(
                    group,
                    SourceControlGroup::Ignored | SourceControlGroup::Untracked
                ) {
                    folded.contains(&key)
                } else {
                    !folded.contains(&key)
                };
            rows.push(SourceControlRow::Directory {
                group,
                path: path.clone(),
                name: name.clone(),
                depth,
                open,
            });
            if open {
                walk(
                    child,
                    group,
                    &path,
                    depth + 1,
                    folded,
                    force_open,
                    next_pages,
                    incomplete,
                    rows,
                );
                if next_pages.contains_key(&path) {
                    rows.push(SourceControlRow::LoadMore {
                        group,
                        directory: Some(path.clone()),
                        depth: depth + 1,
                    });
                }
                if incomplete.contains(&path) {
                    rows.push(SourceControlRow::ListingIncomplete {
                        group,
                        directory: Some(path.clone()),
                        depth: depth + 1,
                    });
                }
            }
        }
        for entry in node.files.values() {
            rows.push(SourceControlRow::file(group, depth, entry));
        }
    }

    walk(
        &root, group, "", 0, folded, force_open, next_pages, incomplete, rows,
    );
    if next_pages.contains_key("") {
        rows.push(SourceControlRow::LoadMore {
            group,
            directory: None,
            depth: 0,
        });
    }
    if incomplete.contains("") {
        rows.push(SourceControlRow::ListingIncomplete {
            group,
            directory: None,
            depth: 0,
        });
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
        && !(entry.worktree_status == '?' && entry.path.ends_with('/'))
}

/// Faint vertical guides down each level a tree row sits under, after Ely's
/// tree list. A hairline only: the tree's shape should read without more ink.
fn source_control_guides(depth: usize, theme: &Theme) -> Div {
    div()
        .absolute()
        .inset_0()
        .pl(px(12.0))
        .flex()
        .children((0..depth).map(|_| {
            div()
                .w(px(SOURCE_CONTROL_TREE_INDENT))
                .h_full()
                .pl(px(5.0))
                .child(div().h_full().border_l_1().border_color(theme.border))
        }))
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

/// A count grouped for reading, so a five-figure total stays legible in a
/// narrow capsule.
fn grouped_count(value: u64) -> String {
    let text = value.to_string();
    let mut grouped = String::with_capacity(text.len() + text.len() / 3);
    for (index, digit) in text.chars().enumerate() {
        if index > 0 && (text.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
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
    /// One line-number column's width, sized to the widest number the diff
    /// shows so the code column never shifts between rows.
    number_width: f32,
    row_height: f32,
    /// Unified rows carry the old and the new number side by side.
    two_numbers: bool,
}

impl DiffRowStyle {
    /// The review page's rows: one file's diff, both sides numbered.
    pub(super) fn review(number_width: f32) -> Self {
        Self {
            number_width,
            row_height: 20.0,
            two_numbers: true,
        }
    }

    /// A change card's rows: the same code, one number column, because a
    /// provider's synthesised patch often has no positions at all.
    pub(super) fn activity(number_width: f32) -> Self {
        Self {
            number_width,
            row_height: 20.0,
            two_numbers: false,
        }
    }
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

/// The widest line number in `range`, at least two digits.
pub(super) fn review_diff_digits(
    snapshot: &crate::review_diff::Snapshot,
    range: Range<usize>,
) -> usize {
    snapshot
        .lines
        .get(range)
        .unwrap_or_default()
        .iter()
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .max()
        .unwrap_or(1)
        .to_string()
        .len()
        .max(2)
}

/// One number column's width for a given digit count, so the code column
/// keeps the same place on every row.
pub(super) fn review_diff_number_width(digits: usize) -> f32 {
    let size: f32 = code_px(10.5).into();
    (size * (digits as f32 * 0.62 + 0.8)).max(size * 1.6)
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
    let (marker, body_background, sign_color) = match &line.kind {
        crate::review_diff::LineKind::Addition => (
            "+",
            Some(theme.success.opacity(DIFF_ROW_WASH)),
            theme.success_text,
        ),
        crate::review_diff::LineKind::Deletion => (
            "\u{2212}",
            Some(theme.danger.opacity(DIFF_ROW_WASH)),
            theme.danger_text,
        ),
        _ => (" ", None, theme.text_ghost),
    };
    let flat = review_diff_flat_text(line, theme);
    let selectable = md::render::selectable_flat_text(
        &flat,
        crate::md::selection::TextKey::new(diff_row_selection_key(key_prefix, line, index), 0),
        selection.clone(),
        theme.code_wash,
        theme.selection,
        false,
    );
    let number = |value: Option<u32>| {
        div()
            .w(px(style.number_width))
            .flex_none()
            .text_right()
            .children(value.map(|value| value.to_string()))
    };
    let mut gutter = div()
        .min_h(px(style.row_height))
        .self_stretch()
        .flex_none()
        .pl(px(8.0))
        .pr(px(8.0))
        .flex()
        .gap(px(6.0))
        .items_start()
        .text_color(theme.text_tertiary);
    if style.two_numbers {
        gutter = gutter
            .child(number(line.old_line))
            .child(number(line.new_line));
    } else {
        gutter = gutter.child(number(line.new_line.or(line.old_line)));
    }
    let body = div()
        .min_h(px(style.row_height))
        .self_stretch()
        .min_w_0()
        .flex_1()
        .flex()
        .items_start()
        .child(
            div()
                .id(SharedString::from(format!("{key_prefix}-sign-{index}")))
                .w(px(10.0))
                .min_h(px(style.row_height))
                .flex_none()
                .text_color(sign_color)
                .child(marker),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "{key_prefix}-line-content-{index}"
                )))
                .min_h(px(style.row_height))
                .min_w_0()
                .flex_1()
                .pl(px(2.0))
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
        .when_some(body_background, |row, background| row.bg(background))
        .font_family(md::render::mono_family())
        .text_size(code_px(10.5))
        .line_height(code_px(style.row_height))
        .child(gutter)
        .child(body)
        .into_any_element()
}

/// The faint row wash the review reader uses. The changed words inside the
/// row are washed harder, so an edit reads without either half shouting.
const DIFF_ROW_WASH: f32 = 0.08;

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
    if !line.words.is_empty() {
        let wash = match line.kind {
            crate::review_diff::LineKind::Addition => Some(theme.success.opacity(WORD_WASH)),
            crate::review_diff::LineKind::Deletion => Some(theme.danger.opacity(WORD_WASH)),
            _ => None,
        };
        if let Some(wash) = wash {
            runs = wash_word_runs(runs, &line.words, wash);
        }
    }
    md::render::FlatText {
        text: text.into(),
        runs,
        links: Vec::new(),
        code_ranges: Vec::new(),
        active_link: Rc::default(),
        math: None,
        references: Vec::new(),
        allow_references: false,
    }
}

/// The wash behind the words that changed, matching the diff viewer this
/// surface follows: the row stays faint, the edit itself reads at full
/// strength.
const WORD_WASH: f32 = 0.25;

/// Splits syntax runs so the changed words carry a background. Every input
/// run becomes one or more runs with the same total length, so the text still
/// lays out exactly as it did. Ranges are sorted and non-overlapping by
/// construction; clamping to the cursor keeps that a hard guarantee rather
/// than an assumption.
fn wash_word_runs(runs: Vec<TextRun>, words: &[Range<usize>], wash: Hsla) -> Vec<TextRun> {
    let mut output = Vec::with_capacity(runs.len() + words.len() * 2);
    let mut offset = 0;
    for run in runs {
        let start = offset;
        let end = offset + run.len;
        offset = end;
        let mut cursor = start;
        for word in words {
            let word_start = word.start.max(start).max(cursor);
            let word_end = word.end.min(end);
            if word_start >= word_end {
                continue;
            }
            if cursor < word_start {
                output.push(run_segment(&run, cursor - start, word_start - start, None));
            }
            output.push(run_segment(
                &run,
                word_start - start,
                word_end - start,
                Some(wash),
            ));
            cursor = word_end;
        }
        if cursor < end {
            output.push(run_segment(&run, cursor - start, end - start, None));
        }
    }
    output
}

fn run_segment(run: &TextRun, start: usize, end: usize, background: Option<Hsla>) -> TextRun {
    TextRun {
        len: end - start,
        font: run.font.clone(),
        color: run.color,
        background_color: background.or(run.background_color),
        underline: run.underline,
        strikethrough: run.strikethrough,
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
            words: Vec::new(),
        }
    }

    fn split_test_snapshot(lines: Vec<crate::review_diff::Line>) -> crate::review_diff::Snapshot {
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
        let rows =
            source_control_split_rows(&snapshot, source_control_file_line_range(&snapshot, 0));
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
    fn review_render_path_only_reads_the_in_memory_snapshot() {
        let source = include_str!("source_control.rs");
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

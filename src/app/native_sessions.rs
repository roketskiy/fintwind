//! Reconciliation between the app's sidebar and OpenCode's own session store.
//!
//! OpenCode is the single source of truth for sessions: the CLI, the TUI, and
//! this app all write into the same server. A background reconcile lists the
//! current workspace's native sessions, imports the ones the app has never
//! seen, refreshes titles and timestamps of the ones it has, and drops local
//! entries the server no longer holds. When a session that tracks a native
//! one is opened — imported, or created here and continued elsewhere — its
//! server-side transcript is fetched and translated into the ordinary
//! message/block model, so it renders exactly like a session created here.
//!
//! Reconcile runs at startup, when the window regains focus (the classic
//! "I just used the TUI" moment), and whenever a live driver relays a session
//! lifecycle event. All server I/O stays off the UI thread.

use super::*;

use fintwind_client::provider_session::NativeSessionSummary;

/// Debounce for reconcile requests, so a burst of lifecycle events costs one
/// server round trip.
const RECONCILE_DEBOUNCE: Duration = Duration::from_millis(600);

/// Local rows and server summaries stamp their times with different clocks
/// (the app's, or a remote OpenCode server's). A pair within this window is
/// the same conversation; beyond it, a title collision is too likely.
const TWIN_CLAIM_WINDOW_SECS: u64 = 5 * 60;

/// What one roster summary should do to the local roster.
#[derive(Debug, Eq, PartialEq)]
enum RosterTarget {
    /// Refresh the row already tracking this native session.
    Update(Uuid),
    /// Hand the native id to a local row that describes the same conversation
    /// but never recorded it, instead of importing a twin.
    Claim(Uuid),
    /// A legacy twin pair: an imported skeleton tracks the session while an
    /// untracked local row describes the same conversation. Keep the local
    /// row and drop the skeleton.
    HealTwin { claim: Uuid, remove: Uuid },
    /// No local row describes this native session; import one.
    Import,
}

/// Does a local row already track this native session? Skeletons carry
/// the native id in its list column, live sessions in their cursor.
fn finds_native_row(sessions: &[AgentSession], native_id: &str) -> Option<Uuid> {
    sessions
        .iter()
        .find(|session| {
            session.native_session_id.as_deref() == Some(native_id)
                || session
                    .provider_cursor
                    .as_ref()
                    .is_some_and(|cursor| cursor.native_id() == native_id)
        })
        .map(|session| session.id)
}

/// The local side of a legacy twin pair: a started session that never
/// recorded its native id — an older build, or the window between the server
/// creating the session and `Connected` landing locally — describing the same
/// conversation as `summary`: same project, no id or cursor of its own, a
/// matching title, and a recency within [`TWIN_CLAIM_WINDOW_SECS`].
fn untracked_twin_row(
    sessions: &[AgentSession],
    project_id: Uuid,
    summary: &NativeSessionSummary,
) -> Option<Uuid> {
    let title = summary.title.as_deref()?;
    sessions
        .iter()
        .find(|session| {
            session.project_id == project_id
                && session.has_started()
                && !session.imported
                && session.native_session_id.is_none()
                && session.provider_cursor.is_none()
                && ((session.title != AgentSession::DEFAULT_TITLE && session.title == title)
                    || session.auto_title.as_deref() == Some(title))
                && session
                    .last_reply_at
                    .unwrap_or(session.created_at)
                    .abs_diff(summary.updated_at)
                    <= TWIN_CLAIM_WINDOW_SECS
        })
        .map(|session| session.id)
}

/// Decide what `summary` should do to the roster.
fn resolve_roster_target(
    sessions: &[AgentSession],
    project_id: Uuid,
    summary: &NativeSessionSummary,
) -> RosterTarget {
    let tracked = finds_native_row(sessions, &summary.session_id);
    let claimable = untracked_twin_row(sessions, project_id, summary);
    match (tracked, claimable) {
        (Some(tracked), Some(claim)) if tracked != claim => {
            // Only a skeleton is droppable: an app-created row that already
            // tracks the session may hold state the claimant lacks, and the
            // pair may be two genuinely distinct conversations.
            let tracked_is_skeleton = sessions
                .iter()
                .find(|session| session.id == tracked)
                .is_some_and(|session| session.imported);
            if tracked_is_skeleton {
                RosterTarget::HealTwin {
                    claim,
                    remove: tracked,
                }
            } else {
                RosterTarget::Update(tracked)
            }
        }
        (Some(tracked), _) => RosterTarget::Update(tracked),
        (None, Some(claim)) => RosterTarget::Claim(claim),
        (None, None) => RosterTarget::Import,
    }
}

/// Imported rows of one project the server no longer holds. Only that
/// project's rows are judged: the listing belongs to its directory, so
/// another project's imports must survive a reconcile they were never part
/// of — sweeping them made sessions vanish whenever the selection moved,
/// until some later reconcile happened to re-import them.
fn stale_imported_rows(
    sessions: &[AgentSession],
    project_id: Uuid,
    known: &std::collections::HashSet<&str>,
) -> Vec<Uuid> {
    sessions
        .iter()
        .filter(|session| session.imported && session.project_id == project_id)
        .filter(|session| {
            session
                .native_session_id
                .as_deref()
                .is_some_and(|native| !known.contains(native))
        })
        .map(|session| session.id)
        .collect()
}

/// Rows sharing a native id are one conversation; keep the app-created row
/// when there is one (it may hold hydrated state the skeleton lacks) and
/// drop the rest. Legacy stores can already contain such pairs.
fn duplicate_native_rows(sessions: &[AgentSession]) -> Vec<Uuid> {
    let mut keepers: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut remove: Vec<Uuid> = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        let Some(native) = session.native_session_id.as_deref() else {
            continue;
        };
        match keepers.get(native) {
            Some(&keeper) => {
                if sessions[keeper].imported && !session.imported {
                    remove.push(sessions[keeper].id);
                    keepers.insert(native, index);
                } else {
                    remove.push(session.id);
                }
            }
            None => {
                keepers.insert(native, index);
            }
        }
    }
    remove
}

/// A connecting session has had its native session created on the server but
/// has not recorded the id locally yet — `Connected` has not arrived.
/// Applying a roster in that window imports a twin for it.
fn has_starting_session(sessions: &[AgentSession]) -> bool {
    sessions
        .iter()
        .any(|session| session.status == SessionStatus::Connecting)
}

/// Should this session's transcript be pulled from the OpenCode server?
///
/// Every session that tracks a native one qualifies — imported sessions on
/// first open, and app-created ones whenever the server's copy moved past
/// what they hold: the TUI and the CLI write into the same store while this
/// app is closed or focused elsewhere. A session this app is streaming does
/// not — its transcript is being produced here, and a server pull would
/// clobber live turn state.
fn native_transcript_refresh_due(
    session: &AgentSession,
    runtime_attached: bool,
    fetched_at: u64,
) -> bool {
    if session.native_session_id.is_none() {
        return false;
    }
    if session.status.is_busy() || runtime_attached {
        return false;
    }
    let has_content = session.detail_loaded && !session.messages.is_empty();
    !(has_content && fetched_at >= session.last_reply_at.unwrap_or(0))
}

impl Fintwind {
    /// Request a reconcile after a short debounce. Safe to call often.
    pub(super) fn schedule_native_session_reconcile(&mut self, cx: &mut Context<Self>) {
        self.native_reconcile_generation += 1;
        let generation = self.native_reconcile_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RECONCILE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                if this.native_reconcile_generation == generation {
                    this.reconcile_native_sessions(cx);
                }
            });
        })
        .detach();
    }

    /// The OpenCode binary the provider probe found, if any.
    fn native_binary_path(&self) -> Option<PathBuf> {
        self.probes
            .first()
            .and_then(|probe| probe.path.clone())
            .filter(|path| path.is_file())
    }

    /// The workspace directory whose native surface a session talks to: its
    /// own project's checkout, not whichever project happens to be selected.
    fn native_session_directory(&self, session_id: Uuid) -> Option<PathBuf> {
        let project_id = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)?
            .project_id;
        self.state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
    }

    /// List the server's sessions for the selected project's workspace and
    /// merge them into the local roster. The listing RPC is blocking, so it
    /// runs on the background executor.
    pub(super) fn reconcile_native_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(binary) = self.native_binary_path() else {
            return;
        };
        // The listing belongs to one project: capture it together with the
        // directory, so a late apply cannot pair an old directory's roster
        // with a newly selected project.
        let Some(project_id) = self.state.selected_project else {
            return;
        };
        let Some(directory) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
        else {
            return;
        };
        // A connecting session would be twinned by this roster; wait for its
        // `Connected` to land. The next lifecycle event or window focus
        // re-requests the reconcile.
        if has_starting_session(&self.state.sessions) {
            self.schedule_native_session_reconcile(cx);
            return;
        }
        self.native_reconcile_generation += 1;
        let generation = self.native_reconcile_generation;
        let daemon = self.daemon.clone();
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move {
                    fintwind_client::persistence::StateStore::remote(daemon)
                        .list_provider_sessions(binary, directory)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A newer reconcile superseded this listing; applying it
                // would re-import under a stale project pairing.
                if this.native_reconcile_generation != generation {
                    return;
                }
                match listed {
                    Ok(sessions) => this.apply_native_session_roster(project_id, sessions, cx),
                    Err(error) => {
                        // Reconciliation is best-effort: a server that cannot
                        // be reached leaves the current roster in place. Only
                        // the first failure after a success toasts, so a
                        // dead server does not nag on every window focus.
                        let first_failure = this.native_reconcile_error.is_none();
                        this.native_reconcile_error = Some(error.to_string());
                        if first_failure {
                            this.show_toast(tr!("sessions.sync_failed", error = error.to_string()));
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Merge one server listing into the local roster. The listing belongs
    /// to `project_id`'s workspace; imports are homed there and only that
    /// project's rows are ever dropped for missing from it.
    fn apply_native_session_roster(
        &mut self,
        project_id: Uuid,
        summaries: Vec<fintwind_client::provider_session::NativeSessionSummary>,
        cx: &mut Context<Self>,
    ) {
        self.native_reconcile_error = None;
        let mut changed = false;

        for summary in &summaries {
            match resolve_roster_target(&self.state.sessions, project_id, summary) {
                RosterTarget::Update(session_id) => {
                    changed |= self.update_session_from_summary(session_id, summary);
                }
                RosterTarget::Claim(session_id) => {
                    self.claim_native_session(session_id, summary);
                    self.update_session_from_summary(session_id, summary);
                    changed = true;
                }
                RosterTarget::HealTwin { claim, remove } => {
                    // The user may be looking at the twin; keep the
                    // conversation open on the surviving row.
                    if self.state.selected_session == Some(remove) {
                        self.state.selected_session = Some(claim);
                    }
                    self.drop_roster_row(remove);
                    self.claim_native_session(claim, summary);
                    self.update_session_from_summary(claim, summary);
                    changed = true;
                }
                RosterTarget::Import => {
                    let mut session = AgentSession::new(project_id);
                    session.imported = true;
                    session.native_session_id = Some(summary.session_id.clone());
                    session.provider_cursor = Some(ProviderResumeCursor::from_session_id(
                        summary.session_id.clone(),
                    ));
                    session.auto_title = summary.title.clone();
                    session.model = summary.model.clone();
                    // The server's own timestamps are authoritative for
                    // imported sessions; zero would sort them wrong.
                    session.created_at = summary.created_at.max(1);
                    session.updated_at = summary.updated_at.max(1);
                    session.last_reply_at = Some(summary.updated_at.max(1));
                    session.detail_loaded = false;
                    self.state.push_session(session);
                    changed = true;
                }
            }
        }

        // Legacy stores can already hold two rows for one native session.
        for session_id in duplicate_native_rows(&self.state.sessions) {
            self.drop_roster_row(session_id);
            changed = true;
        }

        // Imported sessions the server no longer holds are removed locally.
        let known: std::collections::HashSet<&str> = summaries
            .iter()
            .map(|summary| summary.session_id.as_str())
            .collect();
        for session_id in stale_imported_rows(&self.state.sessions, project_id, &known) {
            self.drop_roster_row(session_id);
            changed = true;
        }

        if changed {
            self.save();
            cx.notify();
        }

        // A roster is the app's one reliable signal that the server moved —
        // the "I just used the TUI" moment. If that merge invalidated the
        // selected session's synced transcript, pull it now instead of
        // waiting for a re-activation. Sessions this run has driven keep
        // their runtime attached and are skipped: their transcript echoes
        // this app's own turns, and their next activation refreshes.
        if let Some(selected) = self.state.selected_session
            && !self.runtimes.contains_key(&selected)
        {
            self.ensure_native_transcript(selected, cx);
        }
    }

    /// Point an untracked local row at its native session so later rosters
    /// match it instead of importing a twin.
    fn claim_native_session(&mut self, session_id: Uuid, summary: &NativeSessionSummary) {
        if let Some(session) = self.state.session_mut(session_id) {
            session.native_session_id = Some(summary.session_id.clone());
            if session.auto_title.is_none() {
                session.auto_title = summary.title.clone();
            }
        }
    }

    /// Refresh one tracked row from a summary. Returns whether anything
    /// moved: a no-op reconcile (the common case on every window focus) must
    /// not dirty the roster and trigger a save.
    fn update_session_from_summary(
        &mut self,
        session_id: Uuid,
        summary: &NativeSessionSummary,
    ) -> bool {
        let Some(before) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| {
                (
                    session.imported,
                    session.auto_title.clone(),
                    session.model.clone(),
                    session.last_reply_at,
                    session.updated_at,
                )
            })
        else {
            return false;
        };
        let (imported, old_title, old_model, old_last_reply, old_updated) = before;
        let new_title = if imported {
            summary.title.clone()
        } else {
            old_title.clone()
        };
        let new_model = match &old_model {
            Some(_) => old_model.clone(),
            None => summary.model.clone(),
        };
        let new_last_reply = Some(old_last_reply.unwrap_or(0).max(summary.updated_at));
        let new_updated = old_updated.max(summary.updated_at);
        let unchanged = old_title == new_title
            && old_model == new_model
            && old_last_reply == new_last_reply
            && old_updated == new_updated;
        if unchanged {
            return false;
        }
        if let Some(session) = self.state.session_mut(session_id) {
            if imported {
                // The server's view of title wins for sessions
                // the app did not create; the title rides in
                // `auto_title` so a user-owned name still
                // outranks it in the UI.
                session.auto_title = new_title;
                session.model = new_model;
            } else if session.model.is_none() {
                session.model = new_model;
            }
            // Freshness: a server update newer than what the
            // local transcript reflects invalidates the fetched
            // snapshot.
            session.last_reply_at = new_last_reply;
            session.updated_at = new_updated;
        }
        if old_last_reply.unwrap_or(0) < summary.updated_at {
            self.native_transcript_fetched.remove(&session_id);
        }
        true
    }

    /// Remove one roster row and its satellite state. Unlike
    /// [`Self::remove_session`] this is reconciliation bookkeeping: the
    /// native session may still exist on the server, and runtimes, stored
    /// rows, drafts, and navigation history are not touched.
    fn drop_roster_row(&mut self, session_id: Uuid) {
        self.state
            .sessions
            .retain(|session| session.id != session_id);
        self.remove_right_panel_session_state(session_id);
        self.native_transcript_fetched.remove(&session_id);
        self.state.selected_session = self
            .state
            .selected_session
            .filter(|selected| *selected != session_id);
    }

    /// Make sure a session that tracks a native OpenCode session shows the
    /// server's transcript. Called when a session is activated (after local
    /// hydration) and after a roster merge that may have observed the server
    /// moving underneath.
    ///
    /// App-created sessions need this as much as imported ones: the TUI and
    /// the CLI write into the same server store, so a conversation continued
    /// elsewhere must refresh exactly like one imported from there.
    pub(super) fn ensure_native_transcript(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let Some((native_session_id, binary, directory)) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                if !native_transcript_refresh_due(
                    session,
                    self.runtimes.contains_key(&session_id),
                    self.native_transcript_fetched
                        .get(&session_id)
                        .copied()
                        .unwrap_or(0),
                ) {
                    return None;
                }
                Some((
                    session.native_session_id.clone()?,
                    self.native_binary_path()?,
                    self.native_session_directory(session_id)?,
                ))
            })
        else {
            return;
        };
        if !self.native_transcript_fetches.insert(session_id) {
            return;
        }
        let daemon = self.daemon.clone();
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    fintwind_client::persistence::StateStore::remote(daemon)
                        .fetch_native_transcript(binary, directory, native_session_id)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.native_transcript_fetches.remove(&session_id);
                match fetched {
                    Ok(transcript) => {
                        // The fetch raced the session coming back to life: a
                        // turn may have started between dispatch and apply,
                        // and replacing the transcript then would clobber
                        // live streaming state. Leave the synced marker
                        // unset so the next activation retries.
                        let live = this
                            .state
                            .sessions
                            .iter()
                            .find(|session| session.id == session_id)
                            .is_some_and(|session| session.status.is_busy())
                            || this.runtimes.contains_key(&session_id)
                            || this.submission_preparations.contains(&session_id);
                        if !live
                            && let Some(session) = this.state.session_mut(session_id)
                        {
                            session.messages = transcript.messages;
                            session.transcript_blocks = transcript.blocks;
                            session.turns = transcript.turns;
                            session.detail_loaded = true;
                            // Not the wall clock: at least the server stamp
                            // the pull reflects, so a server clock ahead of
                            // this machine's cannot pin every future check
                            // into refetching.
                            this.native_transcript_fetched.insert(
                                session_id,
                                unix_time().max(session.last_reply_at.unwrap_or(0)),
                            );
                            this.state.mark_session_dirty(session_id);
                            // The pulled blocks may carry plan activities the
                            // replaced state never saw.
                            this.rebuild_todo_summary(session_id);
                            if this.state.selected_session == Some(session_id) {
                                this.reset_visible_state();
                                this.reset_transcript_rows(this.transcript_row_count());
                            }
                            this.save();
                        }
                        cx.notify();
                    }
                    Err(error) => {
                        this.native_reconcile_error = Some(error.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Carry a title edit to the OpenCode server when the session is backed
    /// by a native session. Best-effort: a failure leaves the local title.
    pub(super) fn rename_native_session(
        &mut self,
        session_id: Uuid,
        title: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(native_session_id) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .filter(|session| !session.status.is_busy())
            .and_then(|session| session.native_session_id.clone())
        else {
            return;
        };
        let Some(binary) = self.native_binary_path() else {
            return;
        };
        let Some(directory) = self.native_session_directory(session_id) else {
            return;
        };
        let title = title.trim().to_owned();
        if title.is_empty() {
            return;
        }
        let daemon = self.daemon.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = fintwind_client::persistence::StateStore::remote(daemon)
                    .rename_provider_session(binary, directory, native_session_id, title);
            })
            .detach();
    }

    /// Delete the backing native session on the OpenCode server. Called when
    /// a session with a native id is removed from the app — the server is the
    /// single store, so deleting here deletes there too. Best-effort: the
    /// local removal proceeds regardless.
    pub(super) fn delete_native_session(
        &mut self,
        session_id: Uuid,
        project_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(native_session_id) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.native_session_id.clone())
        else {
            return;
        };
        let Some(project_path) = project_path.or_else(|| self.native_session_directory(session_id))
        else {
            return;
        };
        let Some(binary) = self.native_binary_path() else {
            return;
        };
        let daemon = self.daemon.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = fintwind_client::persistence::StateStore::remote(daemon)
                    .delete_provider_session(binary, project_path, native_session_id);
            })
            .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_summary(session_id: &str, title: &str, updated_at: u64) -> NativeSessionSummary {
        NativeSessionSummary {
            session_id: session_id.to_owned(),
            title: Some(title.to_owned()),
            created_at: updated_at - 60,
            updated_at,
            model: None,
        }
    }

    fn started_session(project_id: Uuid, title: &str, replied_at: u64) -> AgentSession {
        let mut session = AgentSession::new(project_id);
        session.begin_turn("Start it");
        session.set_title(title);
        // `begin_turn` stamps the wall clock; the fixture's recency is what
        // the decision must follow.
        session.created_at = replied_at - 60;
        session.last_reply_at = Some(replied_at);
        session
    }

    fn imported_session(
        project_id: Uuid,
        native_id: &str,
        title: &str,
        updated_at: u64,
    ) -> AgentSession {
        let mut session = AgentSession::new(project_id);
        session.imported = true;
        session.native_session_id = Some(native_id.to_owned());
        session.provider_cursor = Some(ProviderResumeCursor::from_session_id(native_id.to_owned()));
        session.auto_title = Some(title.to_owned());
        session.created_at = updated_at - 60;
        session.updated_at = updated_at;
        session.last_reply_at = Some(updated_at);
        session.detail_loaded = false;
        session
    }

    #[test]
    fn stale_imported_rows_only_judge_the_reconciled_project() {
        let project_a = Uuid::new_v4();
        let project_b = Uuid::new_v4();
        let stale = imported_session(project_a, "ses_a", "A", 100);
        let other_project = imported_session(project_b, "ses_b", "B", 100);
        let still_listed = imported_session(project_a, "ses_c", "C", 100);
        let stale_id = stale.id;
        let known: std::collections::HashSet<&str> = ["ses_c"].into_iter().collect();

        assert_eq!(
            stale_imported_rows(&[stale, other_project, still_listed], project_a, &known),
            vec![stale_id]
        );
    }

    #[test]
    fn an_untracked_local_row_is_claimed_instead_of_imported() {
        let project = Uuid::new_v4();
        let local = started_session(project, "问候交流", 1_000);
        let summary = native_summary("ses_1", "问候交流", 1_030);

        assert_eq!(
            resolve_roster_target(std::slice::from_ref(&local), project, &summary),
            RosterTarget::Claim(local.id)
        );
    }

    #[test]
    fn an_imported_skeleton_twin_is_healed_onto_the_local_row() {
        let project = Uuid::new_v4();
        let local = started_session(project, "问候交流", 1_000);
        let twin = imported_session(project, "ses_1", "问候交流", 1_030);
        let local_id = local.id;
        let twin_id = twin.id;
        let summary = native_summary("ses_1", "问候交流", 1_030);

        assert_eq!(
            resolve_roster_target(&[local, twin], project, &summary),
            RosterTarget::HealTwin {
                claim: local_id,
                remove: twin_id,
            }
        );
    }

    #[test]
    fn a_claim_rejects_other_titles_projects_and_stale_recency() {
        let project = Uuid::new_v4();
        let other_project = Uuid::new_v4();
        let local = started_session(project, "问候交流", 1_000);

        let other_title = native_summary("ses_1", "打招呼", 1_030);
        assert_eq!(
            resolve_roster_target(std::slice::from_ref(&local), project, &other_title),
            RosterTarget::Import
        );

        let other_project_summary = native_summary("ses_1", "问候交流", 1_030);
        assert_eq!(
            resolve_roster_target(
                std::slice::from_ref(&local),
                other_project,
                &other_project_summary
            ),
            RosterTarget::Import
        );

        let long_ago = native_summary("ses_1", "问候交流", 1_000 + 60 * 60);
        assert_eq!(
            resolve_roster_target(std::slice::from_ref(&local), project, &long_ago),
            RosterTarget::Import
        );
    }

    #[test]
    fn duplicate_native_rows_keep_the_app_created_row() {
        let project = Uuid::new_v4();
        let mut local = started_session(project, "问候交流", 1_000);
        local.native_session_id = Some("ses_1".to_owned());
        let twin = imported_session(project, "ses_1", "问候交流", 1_030);
        let twin_id = twin.id;

        assert_eq!(
            duplicate_native_rows(&[local.clone(), twin.clone()]),
            vec![twin_id]
        );
        assert_eq!(duplicate_native_rows(&[twin, local]), vec![twin_id]);
        assert!(duplicate_native_rows(&[]).is_empty());
    }

    #[test]
    fn a_connecting_session_defers_the_roster() {
        let mut session = started_session(Uuid::new_v4(), "问候交流", 1_000);
        assert!(!has_starting_session(std::slice::from_ref(&session)));
        session.status = SessionStatus::Connecting;
        assert!(has_starting_session(std::slice::from_ref(&session)));
        assert!(!has_starting_session(&[]));
    }

    /// The reported shape: a session created here, continued in the TUI
    /// while the app was closed. The row is not `imported`, but it tracks a
    /// native session whose transcript moved past the local copy, so it must
    /// refresh exactly like an imported one.
    #[test]
    fn an_app_created_session_tracking_a_native_one_refreshes_too() {
        let mut session = started_session(Uuid::new_v4(), "GPU 探讨", 1_000);
        session.native_session_id = Some("ses_1".to_owned());
        session.detail_loaded = true;
        session.push_message(MessageRole::User, "本地已有的最后一条");

        // Never synced this run, or the server moved past the synced stamp.
        assert!(native_transcript_refresh_due(&session, false, 0));
        assert!(native_transcript_refresh_due(&session, false, 999));
        // Already synced through this version of the server transcript.
        assert!(!native_transcript_refresh_due(&session, false, 2_000));
        // A live runtime owns the transcript; a server pull would clobber it.
        assert!(!native_transcript_refresh_due(&session, true, 0));
        // A busy session streams through its runtime too.
        session.status = SessionStatus::Working;
        assert!(!native_transcript_refresh_due(&session, false, 0));
    }

    #[test]
    fn a_local_only_session_never_refreshes() {
        let mut session = started_session(Uuid::new_v4(), "本地会话", 1_000);
        session.detail_loaded = true;
        session.push_message(MessageRole::User, "你好");
        assert!(!native_transcript_refresh_due(&session, false, 0));
    }

    #[test]
    fn an_imported_skeleton_still_refreshes_on_first_open() {
        let session = imported_session(Uuid::new_v4(), "ses_1", "问候交流", 1_000);
        assert!(native_transcript_refresh_due(&session, false, 0));
        assert!(!native_transcript_refresh_due(&session, true, 0));
    }
}

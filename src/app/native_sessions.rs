//! Reconciliation between the app's sidebar and OpenCode's own session store.
//!
//! OpenCode is the single source of truth for sessions: the CLI, the TUI, and
//! this app all write into the same server. A background reconcile lists the
//! current workspace's native sessions, imports the ones the app has never
//! seen, refreshes titles and timestamps of the ones it has, and drops local
//! entries the server no longer holds. When an imported session is opened,
//! its server-side transcript is fetched and translated into the ordinary
//! message/block model, so it renders exactly like a session created here.
//!
//! Reconcile runs at startup, when the window regains focus (the classic
//! "I just used the TUI" moment), and whenever a live driver relays a session
//! lifecycle event. All server I/O stays off the UI thread.

use super::*;

/// Debounce for reconcile requests, so a burst of lifecycle events costs one
/// server round trip.
const RECONCILE_DEBOUNCE: Duration = Duration::from_millis(600);

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

    /// The workspace directory whose sessions the sidebar reconciles: the
    /// selected project's checkout.
    fn native_reconcile_directory(&self) -> Option<PathBuf> {
        let project_id = self.state.selected_project?;
        self.state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
    }

    /// List the server's sessions for the current workspace and merge them
    /// into the local roster. The listing RPC is blocking, so it runs on the
    /// background executor.
    pub(super) fn reconcile_native_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(binary) = self.native_binary_path() else {
            return;
        };
        let Some(directory) = self.native_reconcile_directory() else {
            return;
        };
        self.native_reconcile_generation += 1;
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
                match listed {
                    Ok(sessions) => this.apply_native_session_roster(sessions, cx),
                    Err(error) => {
                        // Reconciliation is best-effort: a server that cannot
                        // be reached leaves the current roster in place. Only
                        // the first failure after a success toasts, so a
                        // dead server does not nag on every window focus.
                        let first_failure = this.native_reconcile_error.is_none();
                        this.native_reconcile_error = Some(error.to_string());
                        if first_failure {
                            this.show_toast(tr!(
                                "sessions.sync_failed",
                                error = error.to_string()
                            ));
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Does a local row already track this native session? Skeletons carry
    /// the native id in its list column, live sessions in their cursor.
    fn finds_native_session(&self, native_id: &str) -> Option<Uuid> {
        self.state
            .sessions
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

    /// Merge one server listing into the local roster.
    fn apply_native_session_roster(
        &mut self,
        summaries: Vec<fintwind_client::provider_session::NativeSessionSummary>,
        cx: &mut Context<Self>,
    ) {
        self.native_reconcile_error = None;
        let Some(project_id) = self.state.selected_project else {
            return;
        };
        let mut changed = false;

        for summary in &summaries {
            match self.finds_native_session(&summary.session_id) {
                Some(session_id) => {
                    // Compare first: a no-op reconcile (the common case on
                    // every window focus) must not dirty the roster and
                    // trigger a save.
                    let before = self
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
                        });
                    let Some((imported, old_title, old_model, old_last_reply, old_updated)) =
                        before
                    else {
                        continue;
                    };
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
                        continue;
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
                        self.imported_transcript_fetched.remove(&session_id);
                    }
                    changed = true;
                }
                None => {
                    let mut session = AgentSession::new(project_id);
                    session.imported = true;
                    session.native_session_id = Some(summary.session_id.clone());
                    session.provider_cursor =
                        Some(ProviderResumeCursor::from_session_id(summary.session_id.clone()));
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

        // Imported sessions the server no longer holds are removed locally.
        let known: std::collections::HashSet<&str> = summaries
            .iter()
            .map(|summary| summary.session_id.as_str())
            .collect();
        for session_id in self
            .state
            .sessions
            .iter()
            .filter(|session| session.imported)
            .filter(|session| {
                session
                    .native_session_id
                    .as_deref()
                    .is_some_and(|native| !known.contains(native))
            })
            .map(|session| session.id)
            .collect::<Vec<_>>()
        {
            self.state.sessions.retain(|session| session.id != session_id);
            self.remove_right_panel_session_state(session_id);
            self.imported_transcript_fetched.remove(&session_id);
            self.state.selected_session = self
                .state
                .selected_session
                .filter(|selected| *selected != session_id);
            changed = true;
        }

        if changed {
            self.save();
            cx.notify();
        }
    }

    /// Make sure an imported session shows its server-side transcript.
    /// Called when a session is activated (after local hydration).
    pub(super) fn ensure_imported_transcript(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let Some((native_session_id, binary, directory)) =
            self.state.sessions.iter().find(|s| s.id == session_id).and_then(|session| {
                if !session.imported {
                    return None;
                }
                // Busy or attached sessions stream through the runtime;
                // replacing their transcript from the server would clobber
                // live state.
                if session.status.is_busy() || self.runtimes.contains_key(&session_id) {
                    return None;
                }
                // Already fetched for this version of the server transcript?
                let fetched_at = self
                    .imported_transcript_fetched
                    .get(&session_id)
                    .copied()
                    .unwrap_or(0);
                let has_content = session.detail_loaded && !session.messages.is_empty();
                if has_content && fetched_at >= session.last_reply_at.unwrap_or(0) {
                    return None;
                }
                Some((
                    session.native_session_id.clone()?,
                    self.native_binary_path()?,
                    self.native_reconcile_directory()?,
                ))
            })
        else {
            return;
        };
        if !self.imported_transcript_fetches.insert(session_id) {
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
                this.imported_transcript_fetches.remove(&session_id);
                match fetched {
                    Ok(transcript) => {
                        if let Some(session) = this.state.session_mut(session_id) {
                            session.messages = transcript.messages;
                            session.transcript_blocks = transcript.blocks;
                            session.turns = transcript.turns;
                            session.detail_loaded = true;
                            this.imported_transcript_fetched
                                .insert(session_id, unix_time());
                            this.state.mark_session_dirty(session_id);
                        }
                        if this.state.selected_session == Some(session_id) {
                            this.reset_visible_state();
                            this.reset_transcript_rows(this.transcript_row_count());
                        }
                        this.save();
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
        let Some(directory) = self.native_reconcile_directory() else {
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
        let Some(project_path) = project_path.or_else(|| self.native_reconcile_directory())
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

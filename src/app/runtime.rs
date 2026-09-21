use super::*;

fn workspace_ack(
    workspace: &fintwind_client::WorkspaceClient,
    operation: fintwind_client::WorkspaceOperation,
) -> anyhow::Result<()> {
    match workspace.request(operation)? {
        fintwind_client::WorkspaceResult::Ack => Ok(()),
        _ => anyhow::bail!("the daemon returned an invalid workspace response"),
    }
}

fn start_driver(mut request: DriverStartRequest, cwd: PathBuf) -> anyhow::Result<PreparedDriver> {
    request.options.cwd = cwd;
    let (event_tx, events) = driver::event_channel(request.event_wake);
    let handle = driver::start_remote(
        request.daemon_client,
        request.session_id,
        request.options,
        event_tx,
    )?;
    Ok(PreparedDriver { handle, events })
}

fn attach_driver(
    daemon: fintwind_client::DaemonSupervisor,
    session_id: Uuid,
    event_wake: smol::channel::Sender<()>,
) -> anyhow::Result<Option<(AgentSession, PreparedDriver)>> {
    let Some(session) = fintwind_client::persistence::hydrate_session(&daemon, session_id)? else {
        return Ok(None);
    };
    let response = daemon.client().request(
        session_id,
        Uuid::nil(),
        fintwind_client::Command::AttachSession,
    )?;
    let fintwind_client::ResponsePayload::SessionRuntime {
        runtime_id,
        supports_steer,
    } = response
    else {
        anyhow::bail!("fintwind daemon returned an invalid runtime attachment response");
    };
    let Some(runtime_id) = runtime_id else {
        return Ok(None);
    };
    let (event_tx, events) = driver::event_channel(event_wake);
    let handle = driver::attach_remote(
        daemon.client(),
        session_id,
        runtime_id,
        supports_steer,
        session.runtime_event_cursor,
        event_tx,
    )?;
    Ok(Some((session, PreparedDriver { handle, events })))
}

fn load_remote_task_state(
    client: &fintwind_client::DaemonClient,
) -> anyhow::Result<RemoteTaskStateSnapshot> {
    let response = client.request(
        Uuid::nil(),
        Uuid::nil(),
        fintwind_client::Command::LoadTaskState,
    )?;
    let fintwind_client::ResponsePayload::TaskState {
        projects,
        mut sessions,
        ..
    } = response
    else {
        anyhow::bail!("fintwind daemon returned an invalid task-state response");
    };
    for session in &mut sessions {
        session.detail_loaded = false;
    }
    Ok(RemoteTaskStateSnapshot { projects, sessions })
}

/// Merge the daemon's list-only session projection into the desktop catalog.
///
/// Existing rows may already contain a hydrated transcript, so only list
/// metadata is copied from the projection. A locally attached runtime remains
/// authoritative for transient status and timestamps until its own events are
/// drained.
pub(super) fn merge_remote_session_catalog(
    local: &mut Vec<AgentSession>,
    remote: Vec<AgentSession>,
    has_local_runtime: impl Fn(Uuid) -> bool,
) -> Vec<Uuid> {
    let remote_ids = remote
        .iter()
        .map(|session| session.id)
        .collect::<HashSet<_>>();
    let removed = local
        .iter()
        .filter(|session| session.has_started() && !remote_ids.contains(&session.id))
        .map(|session| session.id)
        .collect::<Vec<_>>();
    local.retain(|session| !session.has_started() || remote_ids.contains(&session.id));

    for remote in remote {
        if let Some(local) = local.iter_mut().find(|session| session.id == remote.id) {
            local.title = remote.title;
            local.auto_title = remote.auto_title;
            local.project_id = remote.project_id;
            local.provider = remote.provider;
            local.model = remote.model;
            local.created_at = remote.created_at;
            local.last_reply_at = remote.last_reply_at;
            if !has_local_runtime(local.id) {
                local.status = remote.status;
                local.updated_at = remote.updated_at;
            }
        } else {
            local.push(remote);
        }
    }

    removed
}

/// Perform every blocking operation between accepting a submission and
/// starting its provider. This function is called only from the background
/// executor; the UI thread owns applying the returned workspace afterward.
fn prepare_submission(
    workspace_client: fintwind_client::WorkspaceClient,
    project: Project,
    workspace: SessionWorkspace,
    driver_start: Option<anyhow::Result<DriverStartRequest>>,
    session_id: Uuid,
    prompt: &str,
    turn_count: usize,
) -> anyhow::Result<PreparedSubmission> {
    let workspace = match workspace {
        SessionWorkspace::NewWorktree { base_branch } => {
            if project.is_projectless() {
                anyhow::bail!("a projectless task cannot create a Git worktree");
            }
            let created = match workspace_client.request(
                fintwind_client::WorkspaceOperation::CreateWorktree {
                    project_path: project.path.clone(),
                    project_id: project.id,
                    session_id,
                    prompt: prompt.to_owned(),
                    base_branch,
                },
            )? {
                fintwind_client::WorkspaceResult::WorktreeCreated { worktree } => worktree,
                _ => anyhow::bail!("the daemon returned an invalid worktree response"),
            };
            SessionWorkspace::Worktree {
                path: created.path,
                branch: created.branch,
            }
        }
        workspace => workspace,
    };
    let project_path = workspace.path().unwrap_or(&project.path);

    // Every turn gets its own immutable starting snapshot. Reusing the prior
    // response's ending ref would attribute branch switches or terminal edits
    // made between turns to the next response.
    let checkpoint_warning = workspace_ack(
        &workspace_client,
        fintwind_client::WorkspaceOperation::CaptureTurnStart {
            cwd: project_path.to_path_buf(),
            session_id,
            turn_count,
        },
    )
    .err()
    .map(|error| tr!("errors.capture_pre_turn_checkpoint", error = error));

    // Process startup can synchronously resolve executables, bind sockets,
    // and spawn children. It belongs behind the same animated preparation
    // boundary as Git work, otherwise the last spinner frame visibly freezes
    // just before Stop appears.
    let driver = driver_start.map(|request| {
        request.and_then(|request| start_driver(request, project_path.to_path_buf()))
    });

    Ok(PreparedSubmission {
        workspace,
        checkpoint_warning,
        driver,
    })
}

/// Everything a past-message resend needs after the UI accepts it.
///
/// The request owns only thread-safe snapshots. Git, provider RPCs, process
/// startup, and native transcript reads all happen in
/// [`perform_message_rewind`] on the background executor.
struct MessageRewindRequest {
    workspace_client: fintwind_client::WorkspaceClient,
    project_path: PathBuf,
    rollback_turns: usize,
    provider_turn_count: usize,
    provider_cursor: Option<ProviderResumeCursor>,
    binary: Option<PathBuf>,
}

fn perform_message_rewind(mut request: MessageRewindRequest) -> Result<(), String> {
    perform_provider_rewind(&mut request).map_err(|error| error.to_string())
}

fn perform_provider_rewind(request: &mut MessageRewindRequest) -> anyhow::Result<()> {
    if request.rollback_turns == 0 {
        return Ok(());
    }
    // A cold rewind (no live driver) asks the daemon to drive OpenCode's own
    // revert: the server marks the boundary, restores its snapshot, and keeps
    // the session id. A live driver's native session is the same store, so
    // both paths run the identical server-side revert.
    let Some(ProviderResumeCursor::OpenCode {
        session_id: native_session_id,
    }) = request.provider_cursor.as_ref()
    else {
        anyhow::bail!(tr!(
            "errors.provider_native_cursor_unavailable",
            provider = "OpenCode"
        ));
    };
    let binary = request
        .binary
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!(tr!("errors.provider_not_found", provider = "OpenCode")))?;
    request.workspace_client.fork_provider_session(
        fintwind_client::provider_session::ProviderSessionForkRequest::OpenCodeRevert {
            binary: binary.to_owned(),
            cwd: request.project_path.clone(),
            session_id: native_session_id.clone(),
            turn_count: request.provider_turn_count,
        },
    )?;
    Ok(())
}

/// Everything a response fork needs after the click has been accepted.
///
/// The session is a point-in-time snapshot: provider branching may take long
/// enough for the user to navigate elsewhere, but the resulting task must
/// still end at the response they chose. Provider RPCs, process startup,
/// native transcript I/O, and Git ref copying are all performed by
/// [`perform_response_fork`] on the background executor.
struct ResponseForkRequest {
    workspace_client: fintwind_client::WorkspaceClient,
    source: AgentSession,
    source_workspace_path: PathBuf,
    fork_title: String,
    turn_count: usize,
    provider_turn_count: usize,
    binary: Option<PathBuf>,
}

fn numbered_title_suffix(title: &str) -> Option<(&str, usize)> {
    let (base, suffix) = title.rsplit_once(" (")?;
    let number = suffix.strip_suffix(')')?.parse().ok()?;
    (!base.is_empty() && number >= 2).then_some((base, number))
}

fn next_response_fork_title<'a>(
    source_title: &str,
    existing_titles: impl IntoIterator<Item = &'a str>,
) -> String {
    let existing_titles = existing_titles.into_iter().collect::<Vec<_>>();
    let base = numbered_title_suffix(source_title)
        .filter(|(base, _)| existing_titles.iter().any(|title| title == base))
        .map_or(source_title, |(base, _)| base);
    let highest_number = existing_titles
        .iter()
        .filter_map(|title| {
            if *title == base {
                Some(1)
            } else {
                numbered_title_suffix(title)
                    .filter(|(candidate_base, _)| *candidate_base == base)
                    .map(|(_, number)| number)
            }
        })
        .max()
        .unwrap_or(1);
    format!("{base} ({})", highest_number.saturating_add(1).max(2))
}

struct PreparedResponseFork {
    forked: AgentSession,
    prepared_driver: Option<PreparedDriver>,
    checkpoint_warning: Option<String>,
}

type ProviderForkResult = (
    ProviderResumeCursor,
    Option<HashMap<String, String>>,
    Option<PreparedDriver>,
);

fn perform_response_fork(request: ResponseForkRequest) -> Result<PreparedResponseFork, String> {
    let native_fork = (|| -> anyhow::Result<ProviderForkResult> {
        let Some(ProviderResumeCursor::OpenCode {
            session_id: native_session_id,
        }) = request.source.provider_cursor.as_ref()
        else {
            anyhow::bail!(tr!(
                "errors.provider_native_session_unavailable",
                provider = "OpenCode"
            ));
        };
        let binary = request.binary.as_deref().ok_or_else(|| {
            anyhow::anyhow!(tr!("errors.provider_not_installed", provider = "OpenCode"))
        })?;
        Ok((
            request
                .workspace_client
                .fork_provider_session(
                    fintwind_client::provider_session::ProviderSessionForkRequest::OpenCode {
                        binary: binary.to_owned(),
                        cwd: request.source_workspace_path.clone(),
                        session_id: native_session_id.clone(),
                        turn_count: request.provider_turn_count,
                    },
                )?
                .cursor,
            None,
            None,
        ))
    })();

    let (provider_cursor, claude_message_ids, prepared_driver) =
        native_fork.map_err(|error| tr!("errors.fork_task", error = error))?;
    let Some(mut forked) =
        request
            .source
            .fork_through_turn(request.turn_count, provider_cursor, &request.fork_title)
    else {
        return Err(tr!("session.response_cannot_copy"));
    };
    if let Some(message_ids) = claude_message_ids {
        for turn in &mut forked.turns {
            if let Some(message_id) = turn.provider_resume_at.as_mut()
                && let Some(remapped) = message_ids.get(message_id)
            {
                *message_id = remapped.clone();
            }
        }
    }

    let fork_id = forked.id;
    for turn in &mut forked.turns {
        if let Some(checkpoint) = turn.checkpoint.as_mut() {
            checkpoint.git_ref = checkpoint::checkpoint_ref(fork_id, checkpoint.turn_count);
        }
    }
    let checkpoint_warning = workspace_ack(
        &request.workspace_client,
        fintwind_client::WorkspaceOperation::CopySessionRefs {
            cwd: request.source_workspace_path.clone(),
            source_session_id: request.source.id,
            target_session_id: fork_id,
            through_turn_count: request.turn_count,
        },
    )
    .err()
    .map(|error| error.to_string());

    Ok(PreparedResponseFork {
        forked,
        prepared_driver,
        checkpoint_warning,
    })
}

impl Fintwind {
    pub(super) fn restart_task_state_sync(&self) {
        let clients = self.daemon.subscribe_clients();
        let results = self.task_state_sync_tx.clone();
        let event_wake = self.event_wake_tx.clone();
        std::thread::Builder::new()
            .name("fintwind-task-state-sync".into())
            .spawn(move || {
                let Ok(mut client) = clients.recv() else {
                    return;
                };
                loop {
                    while let Ok(newer) = clients.try_recv() {
                        client = newer;
                    }
                    let revisions = client.subscribe_task_state();
                    let result = load_remote_task_state(&client).map_err(|error| error.to_string());
                    if results.send(result).is_err() {
                        return;
                    }
                    signal_event_pump(&event_wake);
                    client = loop {
                        crossbeam_channel::select! {
                            recv(clients) -> replacement => {
                                let Ok(mut replacement) = replacement else {
                                    return;
                                };
                                while let Ok(newer) = clients.try_recv() {
                                    replacement = newer;
                                }
                                break replacement;
                            }
                            recv(revisions) -> revision => {
                                if revision.is_err() {
                                    // Managed replacement publishes the new
                                    // client after the old socket closes. Wait
                                    // for that publication instead of exiting
                                    // the task-state sync worker permanently.
                                    let Ok(replacement) = clients.recv() else {
                                        return;
                                    };
                                    break replacement;
                                }
                                while revisions.try_recv().is_ok() {}
                                let result = load_remote_task_state(&client)
                                    .map_err(|error| error.to_string());
                                if results.send(result).is_err() {
                                    return;
                                }
                                signal_event_pump(&event_wake);
                            }
                        }
                    };
                }
            })
            .ok();
    }

    fn drain_task_state_sync_events(&mut self, cx: &mut Context<Self>) -> bool {
        let mut latest = None;
        while let Ok(result) = self.task_state_sync_events.try_recv() {
            latest = Some(result);
        }
        let Some(result) = latest else {
            return false;
        };
        match result {
            Ok(snapshot) => {
                self.apply_remote_task_state(snapshot, cx);
                true
            }
            Err(error) => {
                eprintln!("could not refresh daemon task state: {error}");
                false
            }
        }
    }

    fn apply_remote_task_state(
        &mut self,
        snapshot: RemoteTaskStateSnapshot,
        cx: &mut Context<Self>,
    ) {
        let runtime_ids = self.runtimes.keys().copied().collect::<HashSet<_>>();
        let removed = merge_remote_session_catalog(
            &mut self.state.sessions,
            snapshot.sessions,
            |session_id| runtime_ids.contains(&session_id),
        );
        for session_id in &removed {
            self.runtime_attach_pending.remove(session_id);
            self.runtime_attach_misses.remove(session_id);
            self.runtimes.remove(session_id);
            self.background_work.remove(session_id);
            self.remove_right_panel_session_state(*session_id);
        }
        self.state.projects = snapshot.projects;

        let attach = self
            .state
            .sessions
            .iter()
            .filter(|session| {
                session.status.is_busy()
                    || (self.state.selected_session == Some(session.id) && session.has_started())
            })
            .map(|session| session.id)
            .collect::<Vec<_>>();
        for session_id in attach {
            self.start_runtime_attachment(session_id, cx);
        }

        if self.state.selected_session.is_some_and(|selected| {
            !self
                .state
                .sessions
                .iter()
                .any(|session| session.id == selected)
        }) {
            let previous_project = self.state.selected_project;
            self.state.selected_session = None;
            let next = self
                .state
                .sessions
                .iter()
                .filter(|session| {
                    previous_project.is_none_or(|project| session.project_id == project)
                })
                .max_by_key(|session| session.updated_at)
                .map(|session| session.id)
                .or_else(|| {
                    self.state
                        .sessions
                        .iter()
                        .max_by_key(|session| session.updated_at)
                        .map(|session| session.id)
                });
            if let Some(next) = next {
                self.select_session(next, cx);
            } else if let Some(project_id) = self
                .state
                .selected_project
                .filter(|project_id| {
                    self.state
                        .projects
                        .iter()
                        .any(|project| project.id == *project_id)
                })
                .or_else(|| self.state.projects.first().map(|project| project.id))
            {
                self.state.selected_project = Some(project_id);
                self.create_session_for(project_id, cx);
            }
        } else if let Some(selected) = self.state.selected_session {
            // The merge may have relocated the selected session to another
            // project; keep its group unfolded so the active row stays visible.
            self.reveal_sidebar_session_project(selected, cx);
        }
    }

    pub(super) fn start_runtime_attachment(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        if self.runtimes.contains_key(&session_id)
            || !self.runtime_attach_pending.insert(session_id)
        {
            return;
        }
        let daemon = self.daemon.clone();
        let event_wake = self.event_wake_tx.clone();
        cx.spawn(async move |fintwind, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { attach_driver(daemon, session_id, event_wake) })
                .await;
            let _ = fintwind.update(cx, move |fintwind, cx| {
                fintwind.finish_runtime_attachment(session_id, result, cx);
            });
        })
        .detach();
    }

    fn finish_runtime_attachment(
        &mut self,
        session_id: Uuid,
        result: anyhow::Result<Option<(AgentSession, PreparedDriver)>>,
        cx: &mut Context<Self>,
    ) {
        if !self.runtime_attach_pending.remove(&session_id) {
            return;
        }
        match result {
            Ok(Some((session, prepared))) => {
                self.runtime_attach_misses.remove(&session_id);
                let Some(index) = self
                    .state
                    .sessions
                    .iter()
                    .position(|candidate| candidate.id == session_id)
                else {
                    return;
                };
                if !self.runtimes.contains_key(&session_id) {
                    self.state.sessions[index] = session;
                    self.install_prepared_driver(session_id, prepared);
                    if self.state.selected_session == Some(session_id) {
                        self.reset_visible_state();
                        self.reset_transcript_rows(self.transcript_row_count());
                    }
                    cx.notify();
                }
            }
            Ok(None) => {
                let busy = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .is_some_and(|session| session.status.is_busy());
                if !busy {
                    self.runtime_attach_misses.remove(&session_id);
                    return;
                }
                let misses = self.runtime_attach_misses.entry(session_id).or_default();
                *misses = misses.saturating_add(1);
                if *misses < 4 {
                    cx.spawn(async move |fintwind, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(250))
                            .await;
                        let _ = fintwind.update(cx, |fintwind, cx| {
                            fintwind.start_runtime_attachment(session_id, cx);
                        });
                    })
                    .detach();
                } else {
                    self.runtime_attach_misses.remove(&session_id);
                    self.interrupt_orphaned_runtime(session_id, cx);
                }
            }
            Err(error) => {
                eprintln!("could not attach desktop to daemon session {session_id}: {error:#}");
            }
        }
    }

    fn interrupt_orphaned_runtime(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let project_paths = self
            .state
            .projects
            .iter()
            .map(|project| (project.id, project.path.clone()))
            .collect::<HashMap<_, _>>();
        let mut checkpoint = None;
        if let Some(session) = self.state.session_mut(session_id) {
            if !session.status.is_busy() {
                return;
            }
            session.status = SessionStatus::Idle;
            let interrupted_turn_count = session
                .turns
                .last_mut()
                .filter(|turn| turn.status == TurnStatus::Running)
                .map(|turn| {
                    turn.status = TurnStatus::Interrupted;
                    turn.completed_at = Some(unix_time());
                    turn.turn_count
                });
            if let Some(turn_count) = interrupted_turn_count {
                let project_path = session
                    .workspace
                    .path()
                    .map(Path::to_path_buf)
                    .or_else(|| project_paths.get(&session.project_id).cloned());
                checkpoint = project_path.map(|project_path| PendingCheckpointCapture {
                    session_id,
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
        if let Some(checkpoint) = checkpoint {
            self.pending_checkpoint_captures.push(checkpoint);
            self.start_pending_checkpoint_captures(cx);
        }
        if self.state.selected_session == Some(session_id) {
            self.reset_visible_state();
            self.reset_transcript_rows(self.transcript_row_count());
        }
        self.save();
        cx.notify();
    }

    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus()
    }

    pub(super) fn selected_project(&self) -> Option<&Project> {
        let id = self.state.selected_project?;
        self.state.projects.iter().find(|project| project.id == id)
    }

    pub(super) fn selected_session(&self) -> Option<&AgentSession> {
        let id = self.state.selected_session?;
        self.state.sessions.iter().find(|session| session.id == id)
    }

    /// Completes a persisted turn exactly once. All production
    /// turn-settlement paths go through this seam.
    pub(super) fn finish_active_turn(
        &mut self,
        session_id: Uuid,
        status: TurnStatus,
    ) -> Option<(Uuid, usize)> {
        self.state
            .session_mut(session_id)?
            .finish_active_turn(status)
    }

    /// The directory every filesystem and provider operation for `session`
    /// must use. A not-yet-materialized worktree draft deliberately reads the
    /// local checkout until its first submission creates the isolated copy.
    pub(super) fn workspace_path_for_session<'a>(
        &'a self,
        session: &'a AgentSession,
    ) -> Option<&'a std::path::Path> {
        let project = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)?;
        Some(session.workspace.path().unwrap_or(&project.path))
    }

    pub(super) fn selected_workspace_path(&self) -> Option<&std::path::Path> {
        let session = self.selected_session()?;
        self.workspace_path_for_session(session)
    }

    /// Marks the session for the next save; see `PersistedState::session_mut`.
    pub(super) fn selected_session_mut(&mut self) -> Option<&mut AgentSession> {
        let id = self.state.selected_session?;
        self.state.session_mut(id)
    }

    pub(super) fn selected_runtime(&self) -> Option<&SessionRuntime> {
        self.runtimes.get(&self.state.selected_session?)
    }

    pub(super) fn provider_probe(&self) -> Option<&ProviderProbe> {
        self.probes.first()
    }

    pub(super) fn request_provider_model_discovery(&mut self) {
        let provider = OPENCODE_PROVIDER.to_owned();
        if self.provider_model_discoveries.contains(&provider) {
            return;
        }
        let Some(probe) = self
            .provider_probe()
            .filter(|probe| probe.installed)
            .cloned()
        else {
            return;
        };
        self.provider_model_discoveries.insert(provider.clone());
        self.provider_model_discoveries_pending
            .insert(provider.clone());
        let provider_probe_tx = self.provider_probe_tx.clone();
        let event_wake = self.event_wake_tx.clone();
        let daemon = self.daemon.client();
        if std::thread::Builder::new()
            .name("fintwind-opencode-model-discovery".into())
            .spawn(move || {
                let discovered = match daemon.request(
                    Uuid::nil(),
                    Uuid::nil(),
                    fintwind_client::Command::ProbeProvider {
                        binary_override: None,
                        discover_models: true,
                        probe_version: false,
                    },
                ) {
                    Ok(fintwind_client::ResponsePayload::ProviderProbe { probe, .. }) => probe,
                    _ => probe,
                };
                if provider_probe_tx.send(discovered).is_ok() {
                    signal_event_pump(&event_wake);
                }
            })
            .is_err()
        {
            self.provider_model_discoveries.remove(&provider);
            self.provider_model_discoveries_pending.remove(&provider);
        }
    }

    /// Re-run OpenCode's model-owned catalog discovery, for selectors whose
    /// contents can change while Fintwind stays open — models the user just
    /// authored in OpenCode's config. The stale catalog stays on screen until
    /// the fresh probe lands, so an open menu never blanks into a loading
    /// state while it refreshes.
    pub(super) fn refresh_provider_model_discovery(&mut self) {
        let provider = OPENCODE_PROVIDER.to_owned();
        if self.provider_model_discoveries_pending.contains(&provider) {
            return;
        }
        self.provider_model_discoveries.remove(&provider);
        self.request_provider_model_discovery();
    }

    /// Ask the installed CLI for its version, one short-lived subprocess on
    /// its own thread. The answer lands in `provider_versions` through the
    /// drain loop; render reads only that map.
    pub(super) fn request_provider_version_probes(&mut self) {
        let provider = OPENCODE_PROVIDER.to_owned();
        if !self.probes.first().is_some_and(|probe| probe.installed)
            || !self
                .provider_version_probes_pending
                .insert(provider.clone())
        {
            return;
        }
        let provider_version_tx = self.provider_version_tx.clone();
        let event_wake = self.event_wake_tx.clone();
        let daemon = self.daemon.client();
        if std::thread::Builder::new()
            .name("fintwind-opencode-version-probe".into())
            .spawn(move || {
                let version = match daemon.request(
                    Uuid::nil(),
                    Uuid::nil(),
                    fintwind_client::Command::ProbeProvider {
                        binary_override: None,
                        discover_models: false,
                        probe_version: true,
                    },
                ) {
                    Ok(fintwind_client::ResponsePayload::ProviderProbe { version, .. }) => version,
                    _ => None,
                };
                let _ = provider_version_tx.send((provider, version));
                signal_event_pump(&event_wake);
            })
            .is_err()
        {
            self.provider_version_probes_pending
                .remove(OPENCODE_PROVIDER);
        }
    }

    pub(super) fn drain_provider_version_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok((provider, version)) = self.provider_version_events.try_recv() {
            self.provider_version_probes_pending.remove(&provider);
            self.provider_versions.insert(provider, version);
            changed = true;
        }
        changed
    }

    /// Re-detect the provider CLI off-thread — for the Providers page's
    /// refresh, or when its binary path just changed. Also re-runs model
    /// discovery and the version probe for whatever detection finds installed.
    pub(super) fn refresh_provider_detection(&mut self) {
        if self.provider_detection_remaining > 0 {
            return;
        }
        self.provider_detection_remaining = 1;
        let provider_detection_tx = self.provider_detection_tx.clone();
        let event_wake = self.event_wake_tx.clone();
        let daemon = self.daemon.client();
        if std::thread::Builder::new()
            .name("fintwind-provider-detection".into())
            .spawn(move || {
                let response = daemon.request(
                    Uuid::nil(),
                    Uuid::nil(),
                    fintwind_client::Command::ProbeProvider {
                        binary_override: None,
                        discover_models: false,
                        probe_version: false,
                    },
                );
                let probe = match response {
                    Ok(fintwind_client::ResponsePayload::ProviderProbe { probe, .. }) => probe,
                    _ => ProviderProbe {
                        installed: false,
                        path: None,
                        models: crate::model_catalog::fallback_models(),
                        agent_presets: crate::model_catalog::fallback_agent_presets(),
                    },
                };
                if provider_detection_tx.send(probe).is_ok() {
                    signal_event_pump(&event_wake);
                }
            })
            .is_err()
        {
            self.provider_detection_remaining = 0;
            return;
        }
        // A refresh means "re-check everything": clearing the per-launch guard
        // lets catalog discovery run again as its detection lands below.
        self.provider_model_discoveries.remove(OPENCODE_PROVIDER);
    }

    pub(super) fn drain_provider_detection_events(&mut self) -> bool {
        let mut changed = false;
        let mut installed = false;
        while let Ok(probe) = self.provider_detection_events.try_recv() {
            installed = probe.installed;
            self.provider_detection_remaining = self.provider_detection_remaining.saturating_sub(1);
            if self.provider_detection_remaining == 0 {
                self.provider_detection_checked_at = Some(Instant::now());
            }
            if let Some(existing) = self.probes.first_mut() {
                if self
                    .provider_model_discoveries_pending
                    .contains(OPENCODE_PROVIDER)
                {
                    // A manual refresh may overlap an older live discovery.
                    // Keep that newer catalog while still accepting PATH
                    // detection from this response.
                    existing.installed = probe.installed;
                    existing.path = probe.path;
                } else {
                    *existing = probe;
                }
            } else {
                self.probes.push(probe);
            }
            if !installed {
                self.provider_versions.remove(OPENCODE_PROVIDER);
            }
            changed = true;
        }
        if installed {
            self.request_provider_model_discovery();
        }
        if changed {
            self.request_provider_version_probes();
        }
        changed
    }

    pub(super) fn model_for_session<'a>(&'a self, session: &'a AgentSession) -> Option<&'a str> {
        session.model.as_deref().or_else(|| {
            self.provider_probe()
                .and_then(ProviderProbe::preferred_model)
                .map(|model| model.id.as_str())
        })
    }

    pub(super) fn model_display_name(&self, model: Option<&str>) -> String {
        let Some(model) = model else {
            return "OpenCode".to_owned();
        };
        self.provider_probe()
            .and_then(|probe| probe.models.iter().find(|candidate| candidate.id == model))
            .map(|candidate| candidate.name.clone())
            .unwrap_or_else(|| model.to_owned())
    }

    pub(super) fn model_metadata_for_session(
        &self,
        session: &AgentSession,
    ) -> Option<&ProviderModel> {
        let model = self.model_for_session(session)?;
        self.provider_probe()?
            .models
            .iter()
            .find(|candidate| candidate.id == model)
    }

    pub(super) fn selected_transcript_blocks(&self) -> &[TranscriptBlock] {
        self.selected_session()
            .map(|session| session.transcript_blocks.as_slice())
            .unwrap_or(&[])
    }

    pub(super) fn save(&mut self) {
        self.last_stream_save = Instant::now();
        let daemon_error = self
            .daemon
            .update_settings(self.state.daemon_settings())
            .err()
            .map(|error| error.to_string());
        let app_error = self
            .store
            .save(&mut self.state)
            .err()
            .map(|error| error.to_string());
        if let Some(error) = daemon_error.or(app_error) {
            self.show_toast(tr!("errors.save_local_state", error = error));
        } else {
            self.stream_state_dirty = false;
        }
    }

    fn checkpoint_capture_pending(&self, session_id: Uuid, turn_count: usize) -> bool {
        self.checkpoint_captures_in_flight
            .contains(&(session_id, turn_count))
            || self
                .pending_checkpoint_captures
                .iter()
                .any(|capture| capture.session_id == session_id && capture.turn_count == turn_count)
    }

    fn ending_checkpoint_pending(&self, session_id: Uuid) -> bool {
        self.state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.turns.last())
            .filter(|turn| turn.status != TurnStatus::Running)
            .is_some_and(|turn| self.checkpoint_capture_pending(session_id, turn.turn_count))
    }

    fn defer_queue_drain(&mut self, session_id: Uuid) {
        if !self.pending_queue_drains.contains(&session_id) {
            self.pending_queue_drains.push(session_id);
        }
    }

    /// Queues the newest finished turn's checkpoint for capture.
    ///
    /// Bookkeeping only. The capture itself is upwards of ten `git`
    /// invocations, one of them a `git add -A` over the whole worktree, and the
    /// hottest caller is the driver-event drain that shares the UI thread with
    /// rendering — so the work belongs to
    /// [`Self::start_pending_checkpoint_captures`], which every caller that
    /// holds a `Context` runs straight after queueing.
    pub(super) fn capture_latest_turn_checkpoint_for(&mut self, session_id: Uuid) {
        let Some((session, turn_count)) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .turns
                    .last()
                    .filter(|turn| turn.status != TurnStatus::Running)
                    .map(|turn| (session, turn.turn_count))
            })
        else {
            return;
        };
        if self.checkpoint_capture_pending(session_id, turn_count) {
            return;
        }
        let Some(project_path) = self
            .workspace_path_for_session(session)
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        self.pending_checkpoint_captures
            .push(PendingCheckpointCapture {
                session_id,
                turn_count,
                project_path,
            });
    }

    /// Runs queued turn checkpoints on the background executor.
    ///
    /// A capture lands a frame or many later, and the turn it belongs to may be
    /// gone by then, so the result is matched back by turn count rather than
    /// position. Nothing on screen waits for it: the transcript's rewind
    /// affordance appears when `invalidate_checkpoint_refs` prompts the next
    /// prefetch to notice the new ref.
    pub(super) fn start_pending_checkpoint_captures(&mut self, cx: &mut Context<Self>) {
        for request in std::mem::take(&mut self.pending_checkpoint_captures) {
            let PendingCheckpointCapture {
                session_id,
                turn_count,
                project_path,
            } = request;
            if !self
                .checkpoint_captures_in_flight
                .insert((session_id, turn_count))
            {
                continue;
            }
            let workspace = fintwind_client::WorkspaceClient::new(self.daemon.client());
            cx.spawn(async move |fintwind, cx| {
                let captured = cx
                    .background_executor()
                    .spawn({
                        let project_path = project_path.clone();
                        async move {
                            match workspace.request(
                                fintwind_client::WorkspaceOperation::CaptureTurn {
                                    cwd: project_path,
                                    session_id,
                                    turn_count,
                                },
                            )? {
                                fintwind_client::WorkspaceResult::Checkpoint { checkpoint } => {
                                    Ok(checkpoint)
                                }
                                _ => anyhow::bail!(
                                    "the daemon returned an invalid checkpoint response"
                                ),
                            }
                        }
                    })
                    .await;
                fintwind
                    .update(cx, |fintwind, cx| {
                        fintwind
                            .checkpoint_captures_in_flight
                            .remove(&(session_id, turn_count));
                        let selected = fintwind.state.selected_session == Some(session_id);
                        if selected {
                            fintwind.sync_transcript_rows();
                        }
                        let previous_kinds = if selected {
                            fintwind.transcript_row_kinds.borrow().clone()
                        } else {
                            Vec::new()
                        };
                        let checkpoint = match captured {
                            Ok(checkpoint) => checkpoint,
                            Err(error) => {
                                fintwind.show_toast(tr!(
                                    "errors.capture_turn_checkpoint",
                                    error = error
                                ));
                                Checkpoint {
                                    turn_count,
                                    git_ref: checkpoint::checkpoint_ref(session_id, turn_count),
                                    status: CheckpointStatus::Error,
                                    files: Vec::new(),
                                    additions: 0,
                                    deletions: 0,
                                    created_at: unix_time(),
                                }
                            }
                        };
                        fintwind.invalidate_checkpoint_refs();
                        let mut attached_turn_id = None;
                        if let Some(session) = fintwind.state.session_mut(session_id)
                            && let Some(turn) = session
                                .turns
                                .iter_mut()
                                .find(|turn| turn.turn_count == turn_count)
                        {
                            turn.checkpoint = Some(checkpoint);
                            attached_turn_id = Some(turn.id);
                        }
                        if let Some(turn_id) = attached_turn_id
                            && selected
                        {
                            // Reconcile a standalone card by row identity, then
                            // remeasure the terminal response when the card is
                            // hosted inline before its footer.
                            fintwind
                                .splice_transcript_rows_after_visibility_change(&previous_kinds);
                            fintwind.remeasure_changed_files(turn_id);
                        }
                        let resume_queue = fintwind.pending_queue_drains.contains(&session_id);
                        if resume_queue {
                            fintwind.pending_queue_drains.retain(|id| *id != session_id);
                            fintwind.drain_queued_message(session_id, cx);
                        }
                        cx.notify();
                        if attached_turn_id.is_some() {
                            // Let the new transcript row paint before SQLite work.
                            // Without this save, a checkpoint that lands after the
                            // turn's final stream save can disappear on relaunch.
                            cx.spawn(async move |fintwind, cx| {
                                cx.background_executor().timer(STREAM_FRAME_INTERVAL).await;
                                let _ = fintwind.update(cx, |fintwind, _| fintwind.save());
                            })
                            .detach();
                        }
                    })
                    .ok();
            })
            .detach();
        }
    }

    pub(super) fn fork_session_from_response(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        cx: &mut Context<Self>,
    ) {
        if self.response_fork_preparations.contains_key(&session_id)
            || self.submission_preparations.contains(&session_id)
        {
            self.show_toast(tr!("session.response_cannot_fork"));
            cx.notify();
            return;
        }
        let Some(source) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
        else {
            self.show_toast(tr!("session.response_unavailable"));
            cx.notify();
            return;
        };
        if self.state.selected_session != Some(session_id)
            || !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed)
            || source
                .turns
                .get(turn_count.saturating_sub(1))
                .is_none_or(|turn| turn.turn_count != turn_count || !turn.provider_turn_started)
        {
            self.show_toast(tr!("session.response_cannot_fork"));
            cx.notify();
            return;
        }
        let Some(source_workspace_path) = self
            .workspace_path_for_session(&source)
            .map(std::path::Path::to_path_buf)
        else {
            self.show_toast(tr!("errors.task_project_not_found"));
            cx.notify();
            return;
        };

        let project_id = source.project_id;
        let fork_title = next_response_fork_title(
            source.display_title(),
            self.state
                .sessions
                .iter()
                .filter(|session| session.project_id == project_id)
                .map(AgentSession::display_title),
        );
        let provider_turn_count = source
            .turns
            .iter()
            .take(turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count();
        let binary = self.probes.first().and_then(|probe| probe.path.clone());
        if binary.is_none() {
            self.show_toast(tr!("errors.provider_not_installed", provider = "OpenCode"));
            cx.notify();
            return;
        }
        let request = ResponseForkRequest {
            workspace_client: fintwind_client::WorkspaceClient::new(self.daemon.client()),
            source,
            source_workspace_path,
            fork_title,
            turn_count,
            provider_turn_count,
            binary,
        };

        self.response_fork_preparations
            .insert(session_id, turn_count);
        self.hide_toast();
        cx.notify();

        cx.spawn(async move |fintwind, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { perform_response_fork(request) })
                .await;
            let _ = fintwind.update(cx, move |fintwind, cx| {
                fintwind.finish_response_fork(session_id, turn_count, result, cx);
            });
        })
        .detach();
    }

    fn finish_response_fork(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        result: Result<PreparedResponseFork, String>,
        cx: &mut Context<Self>,
    ) {
        if self.response_fork_preparations.get(&session_id) != Some(&turn_count) {
            return;
        }
        self.response_fork_preparations.remove(&session_id);

        let PreparedResponseFork {
            forked,
            prepared_driver,
            checkpoint_warning,
        } = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.drain_queued_message(session_id, cx);
                self.show_toast(error);
                cx.notify();
                return;
            }
        };

        if let Some(prepared) = prepared_driver
            && !self.runtimes.contains_key(&session_id)
        {
            self.install_prepared_driver(session_id, prepared);
        }
        self.invalidate_checkpoint_refs();

        let fork_id = forked.id;
        self.state.push_session(forked);
        self.select_session(fork_id, cx);
        self.drain_queued_message(session_id, cx);
        match checkpoint_warning {
            Some(error) => {
                self.show_toast(tr!("session.forked_with_checkpoint_warning", error = error))
            }
            None => self.show_success_toast(tr!("session.forked_from_response")),
        }
        cx.notify();
    }

    /// Composer Enter clears the field after emitting its event. A response
    /// fork temporarily owns the source provider, so restore a keyboard
    /// submission on the next task turn instead of racing it against the fork.
    pub(super) fn defer_restore_composer_after_fork(
        &self,
        session_id: Uuid,
        prompt: String,
        cx: &mut Context<Self>,
    ) {
        let composer = self.composer.clone();
        cx.spawn(async move |fintwind, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1))
                .await;
            let _ = fintwind.update(cx, |fintwind, cx| {
                if fintwind.state.selected_session == Some(session_id) {
                    composer.update(cx, |input, cx| {
                        if input.content().is_empty() {
                            input.set_content(prompt, cx);
                        }
                    });
                }
            });
        })
        .detach();
    }

    pub(super) fn begin_message_edit(
        &mut self,
        action: UserMessageAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let UserMessageAction {
            session_id,
            message_id,
            turn_count,
        } = action;
        let Some((message_index, initial_message, attachments)) = self
            .state
            .sessions
            .iter()
            .find(|session| {
                session.id == session_id
                    && matches!(session.status, SessionStatus::Idle | SessionStatus::Failed)
            })
            .and_then(|session| {
                let turn = session
                    .turns
                    .iter()
                    .find(|turn| turn.turn_count == turn_count)?;
                session
                    .messages
                    .iter()
                    .enumerate()
                    .find_map(|(index, message)| {
                        (message.id == message_id
                            && message.turn_id == Some(turn.id)
                            && message.role == MessageRole::User)
                            .then(|| {
                                (
                                    index,
                                    message.visible_content().to_owned(),
                                    message.attachments.clone(),
                                )
                            })
                    })
            })
        else {
            self.show_toast(tr!("session.message_not_editable"));
            cx.notify();
            return;
        };

        let input = cx.new(|cx| ComposerInput::new(window, cx).padding_x(px(12.0)));
        input.update(cx, |input, cx| input.set_content(initial_message, cx));
        cx.subscribe(
            &input,
            |this: &mut Self, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::Submit(prompt) => {
                    this.submit_message_edit_prompt(prompt.clone(), cx)
                }
                // An edited past message resubmits from that point; there is
                // no running turn for it to steer.
                ComposerEvent::SubmitSteer(prompt) => {
                    this.submit_message_edit_prompt(prompt.clone(), cx)
                }
                ComposerEvent::Edited => cx.notify(),
                ComposerEvent::Focus => {}
                ComposerEvent::BackspaceOnEmpty => {}
            },
        )
        .detach();
        self.message_edit = Some(MessageEdit {
            session_id,
            message_id,
            turn_count,
            input: input.clone(),
            attachments,
        });
        self.hide_toast();
        self.remeasure_transcript_message(message_index);
        let focus_handle = input.read(cx).focus();
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn cancel_message_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .message_edit
            .as_ref()
            .is_some_and(|edit| self.submission_preparations.contains(&edit.session_id))
        {
            return;
        }
        let Some(edit) = self.message_edit.take() else {
            return;
        };
        let message_index = self.selected_session().and_then(|session| {
            session
                .messages
                .iter()
                .position(|message| message.id == edit.message_id)
        });
        if let Some(message_index) = message_index {
            self.remeasure_transcript_message(message_index);
        }
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn submit_message_edit(&mut self, cx: &mut Context<Self>) {
        let prompt = self
            .message_edit
            .as_ref()
            .map(|edit| edit.input.read(cx).content().to_owned())
            .unwrap_or_default();
        self.submit_message_edit_prompt(prompt, cx);
    }

    fn submit_message_edit_prompt(&mut self, prompt: String, cx: &mut Context<Self>) {
        let Some(edit) = self.message_edit.clone() else {
            return;
        };
        if self.submission_preparations.contains(&edit.session_id) {
            return;
        }
        // Keyboard submission clears ComposerInput after emitting its event.
        // Use the event's captured value rather than rereading the field; the
        // button path enters here with its own pre-clear content as well.
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() && edit.attachments.is_empty() {
            self.show_toast(tr!("session.edited_message_empty"));
            cx.notify();
            return;
        }
        let mentions = edit
            .attachments
            .iter()
            .map(|attachment| attachment.mention.clone())
            .collect::<Vec<_>>();
        let provider_prompt = composer::merged_submission(&prompt, &mentions)
            .expect("edited text or retained attachments always form a submission");
        let display_content = (!edit.attachments.is_empty()).then_some(prompt);
        self.start_message_rewind(
            edit.clone(),
            ComposerSubmission {
                prompt: provider_prompt,
                display_content,
                attachments: edit.attachments,
            },
            cx,
        );
    }

    fn start_message_rewind(
        &mut self,
        edit: MessageEdit,
        submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        let session_id = edit.session_id;
        let turn_count = edit.turn_count;
        let retained_turn_count = turn_count.saturating_sub(1);
        let Some(source) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .filter(|session| {
                session
                    .turns
                    .iter()
                    .any(|turn| turn.turn_count == turn_count)
            })
        else {
            self.show_toast(tr!("session.message_unavailable"));
            cx.notify();
            return;
        };
        if self.state.selected_session != Some(session_id) {
            self.show_toast(tr!("session.select_before_rewind"));
            cx.notify();
            return;
        }
        if !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed) {
            self.show_toast(tr!("session.stop_before_rewind"));
            cx.notify();
            return;
        }
        // The rewind and a pending undo/redo both rewrite the server's revert
        // boundary; racing them would commit a boundary the other side never
        // saw. The undo/redo finish path restores Idle, so this is momentary.
        if self.undo_redo_preparations.contains(&session_id) {
            self.show_toast(tr!("session.rewind_during_undo"));
            cx.notify();
            return;
        }
        let rollback_turns = source.provider_turns_after(retained_turn_count);
        if rollback_turns > 0 && source.provider_cursor.is_none() {
            self.show_toast(tr!("session.provider_cannot_rewind", provider = "OpenCode"));
            cx.notify();
            return;
        }
        let Some(project_path) = self
            .workspace_path_for_session(&source)
            .map(std::path::Path::to_path_buf)
        else {
            self.show_toast(tr!("errors.task_project_not_found"));
            cx.notify();
            return;
        };
        // OpenCode's revert marks a native user-message boundary, so the
        // index counts only turns that reached the provider — a locally
        // failed turn has no native message to retain.
        let provider_turn_count = source
            .turns
            .iter()
            .take(retained_turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count();
        // The daemon drives OpenCode's own revert through the workspace's
        // resident server, so the provider binary must be resolvable up
        // front — a rewind that cannot reach the session must fail before
        // the UI leaves edit mode.
        let binary = self.probes.first().and_then(|probe| probe.path.clone());
        if rollback_turns > 0 && binary.is_none() {
            self.show_toast(tr!("errors.provider_not_found", provider = "OpenCode"));
            cx.notify();
            return;
        }
        let previous_status = source.status;
        let provider_cursor = source.provider_cursor.clone();
        let edited_message_id = edit.message_id;
        let Some(edited_message_index) = source
            .turns
            .iter()
            .find(|turn| turn.turn_count == turn_count)
            .and_then(|turn| {
                source.messages.iter().position(|message| {
                    message.id == edited_message_id
                        && message.turn_id == Some(turn.id)
                        && message.role == MessageRole::User
                })
            })
        else {
            self.show_toast(tr!("session.message_unavailable"));
            cx.notify();
            return;
        };
        let request = MessageRewindRequest {
            workspace_client: fintwind_client::WorkspaceClient::new(self.daemon.client()),
            provider_cursor,
            project_path,
            rollback_turns,
            provider_turn_count,
            binary,
        };

        // Optimistically leave edit mode and show the replacement bubble at
        // accept time. The main composer switches to its non-cancellable
        // spinner while every Git, process, native transcript, and provider
        // operation runs off the UI thread. Failure restores both the original
        // bubble and this edit input.
        let original_message = self.state.session_mut(session_id).and_then(|session| {
            let message = session
                .messages
                .iter_mut()
                .find(|message| message.id == edited_message_id)?;
            let original = message.clone();
            message.content = submission.prompt.clone();
            message.display_content = submission.display_content.clone();
            message.attachments = submission.attachments.clone();
            session.status = SessionStatus::Connecting;
            session.updated_at = unix_time();
            Some(original)
        });
        let Some(original_message) = original_message else {
            self.show_toast(tr!("session.message_unavailable"));
            cx.notify();
            return;
        };
        self.message_edit = None;
        self.submission_preparations.insert(session_id);
        self.hide_toast();
        self.remeasure_transcript_message(edited_message_index);
        cx.notify();

        cx.spawn(async move |fintwind, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { perform_message_rewind(request) })
                .await;
            let _ = fintwind.update(cx, move |fintwind, cx| {
                fintwind.finish_message_rewind(
                    edit,
                    submission,
                    edited_message_id,
                    original_message,
                    previous_status,
                    result,
                    cx,
                );
            });
        })
        .detach();
    }

    fn finish_message_rewind(
        &mut self,
        edit: MessageEdit,
        submission: ComposerSubmission,
        edited_message_id: Uuid,
        original_message: Message,
        previous_status: SessionStatus,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let session_id = edit.session_id;
        let turn_count = edit.turn_count;
        if !self.submission_preparations.remove(&session_id) {
            return;
        }
        let selected = self.state.selected_session == Some(session_id);
        if let Err(error) = result {
            if let Some(session) = self.state.session_mut(session_id) {
                if let Some(message) = session
                    .messages
                    .iter_mut()
                    .find(|message| message.id == edited_message_id)
                {
                    *message = original_message;
                }
                if session.status == SessionStatus::Connecting {
                    session.status = previous_status;
                }
            }
            if selected && self.message_edit.is_none() {
                self.message_edit = Some(edit.clone());
            }
            if selected
                && let Some(message_index) = self.selected_session().and_then(|session| {
                    session
                        .messages
                        .iter()
                        .position(|message| message.id == edited_message_id)
                })
            {
                self.remeasure_transcript_message(message_index);
            }
            self.show_toast(error);
            cx.notify();
            return;
        }
        let retained_turn_count = turn_count.saturating_sub(1);
        if !self
            .state
            .sessions
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }
        if selected {
            self.sync_transcript_rows();
        }
        let previous_kinds = if selected {
            self.transcript_row_kinds.borrow().clone()
        } else {
            Vec::new()
        };
        if let Some(session) = self.state.session_mut(session_id) {
            // OpenCode's revert keeps the native session id, so the stored
            // cursor keeps working: nothing to rewrite here.
            session.truncate_after_turn(retained_turn_count);
            session.status = SessionStatus::Idle;
        }
        // The rewind's committed revert deleted the staged-away turns along
        // with everything after the edited message, so any pending redo
        // ends here.
        self.clear_staged_undo(session_id);

        if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime
                .pending_events
                .retain(|event| matches!(event, DriverEvent::BackgroundWork(_)));
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.open_reasoning.clear();
            runtime.settled_reasoning.clear();
            runtime.pending_permission = None;
            runtime.pending_user_input = None;
        }
        self.invalidate_checkpoint_refs();
        if self
            .message_edit
            .as_ref()
            .is_some_and(|current| current.session_id == session_id)
        {
            self.message_edit = None;
        }
        if selected {
            self.activities_expanded.clear();
            self.expanded_activity_items.clear();
            self.expanded_turns.clear();
            self.expanded_changed_files.clear();
            self.expanded_compactions.clear();
            self.expanded_provider_retries.clear();
            self.transcript_control_focuses.borrow_mut().clear();
            self.splice_transcript_rows_after_visibility_change(&previous_kinds);
            self.show_toast(tr!("session.rewound", turn = turn_count));
        }
        cx.notify();
        self.submit_submission_for_session(session_id, submission, cx);
    }

    /// Resolves the turn options a driver should run with, dropping a reasoning
    /// effort or service tier the resolved model does not offer. Driver start
    /// and in-session option changes both go through this so they cannot
    /// disagree about what the session is currently set to.
    pub(super) fn session_options(&self, session: &AgentSession) -> SessionOptions {
        let model = session.model.clone().or_else(|| {
            self.provider_probe()
                .and_then(ProviderProbe::preferred_model)
                .map(|model| model.id.clone())
        });
        let model_metadata = self.model_metadata_for_session(session);
        let reasoning_effort = session
            .reasoning_effort
            .clone()
            .filter(|effort| {
                model_metadata.is_some_and(|model| {
                    model
                        .reasoning_efforts
                        .iter()
                        .any(|option| option.id == *effort)
                })
            })
            .or_else(|| {
                model_metadata.and_then(|model| {
                    model.default_reasoning_effort.clone().or_else(|| {
                        model
                            .reasoning_efforts
                            .first()
                            .map(|option| option.id.clone())
                    })
                })
            });
        let service_tier = session.service_tier.clone().filter(|tier| {
            tier == "default"
                || model_metadata.is_some_and(|model| {
                    model.service_tiers.iter().any(|option| option.id == *tier)
                })
        });
        let context_window = session.context_window.clone().filter(|window| {
            model_metadata.is_some_and(|model| {
                model
                    .context_windows
                    .iter()
                    .any(|option| option.id == *window)
            })
        });
        SessionOptions {
            mode: session.runtime_mode,
            interaction_mode: session.interaction_mode,
            model,
            reasoning_effort,
            service_tier,
            context_window,
        }
    }

    pub(super) fn agent_preset_for_session(&self, session: &AgentSession) -> Option<String> {
        session.agent_preset.clone().or_else(|| {
            self.provider_probe()
                .and_then(ProviderProbe::preferred_agent_preset)
                .map(|preset| preset.id.clone())
        })
    }

    /// Releases provider processes for sessions nobody has touched in a while.
    ///
    /// Codex and Pi keep a process resident between turns, so an abandoned task
    /// otherwise holds an agent for as long as the app runs. Recreating a
    /// runtime is exactly the
    /// work the next prompt already does after Stop, and the resume cursor is
    /// persisted, so the conversation survives.
    pub(super) fn reap_idle_sessions(&mut self) {
        if self.last_idle_session_sweep.elapsed() < IDLE_SESSION_SWEEP_INTERVAL {
            return;
        }
        self.last_idle_session_sweep = Instant::now();
        let idle = self
            .runtimes
            .iter()
            .filter(|(session_id, runtime)| {
                let session = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == **session_id);
                session_is_reapable(
                    session,
                    runtime.last_active_at.elapsed(),
                    self.session_has_live_background_work(**session_id),
                )
            })
            .map(|(session_id, _)| *session_id)
            .collect::<Vec<_>>();
        for session_id in idle {
            // Idle reaping is an explicit daemon-runtime release. Merely
            // dropping a client attachment must not stop work observed by a
            // second desktop or browser client.
            if let Some(runtime) = self.runtimes.remove(&session_id) {
                runtime.driver.close();
            }
        }
    }

    /// Applies a changed model, effort, tier, or mode to a session. Transports
    /// that carry these per turn absorb the change and keep running; the rest
    /// are torn down so the next prompt starts with the new options.
    pub(super) fn apply_session_options(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let Some(options) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| self.session_options(session))
        else {
            return;
        };
        let Some(runtime) = self.runtimes.get_mut(&session_id) else {
            return;
        };
        runtime.options_generation = runtime.options_generation.wrapping_add(1);
        let generation = runtime.options_generation;
        let driver = runtime.driver.clone();
        cx.spawn(async move |fintwind, cx| {
            let applied = cx
                .background_executor()
                .spawn(async move { driver.apply_options(options) })
                .await;
            let _ = fintwind.update(cx, |fintwind, cx| {
                let is_current = fintwind
                    .runtimes
                    .get(&session_id)
                    .is_some_and(|runtime| runtime.options_generation == generation);
                if is_current && !applied {
                    fintwind.reset_session_runtime(session_id);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn driver_start_request_for_session(
        &self,
        session: &AgentSession,
        cwd: PathBuf,
    ) -> anyhow::Result<DriverStartRequest> {
        let binary = self
            .probes
            .first()
            .and_then(|probe| probe.path.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(tr!("errors.provider_not_found", provider = "OpenCode"))
            })?;
        let agent_preset = self.agent_preset_for_session(session);
        let SessionOptions {
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier,
            context_window,
        } = self.session_options(&session);
        Ok(DriverStartRequest {
            session_id: session.id,
            options: DriverStartOptions {
                binary,
                cwd,
                mode,
                interaction_mode,
                model,
                reasoning_effort,
                service_tier,
                context_window,
                agent_preset,
                provider_cursor: session.provider_cursor.clone(),
            },
            event_wake: self.event_wake_tx.clone(),
            daemon_client: self.daemon.client(),
        })
    }

    fn install_prepared_driver(
        &mut self,
        session_id: Uuid,
        prepared: PreparedDriver,
    ) -> DriverHandle {
        let handle = prepared.handle.clone();
        self.runtimes.insert(
            session_id,
            SessionRuntime {
                driver: prepared.handle,
                options_generation: 0,
                events: prepared.events,
                pending_events: VecDeque::new(),
                pending_steers: VecDeque::new(),
                stream_phase: None,
                stream_remeasure_pending: false,
                open_reasoning: HashMap::new(),
                settled_reasoning: HashSet::new(),
                provider_phase: None,
                pending_permission: None,
                pending_user_input: None,
                last_driver_error: None,
                last_active_at: Instant::now(),
                last_background_refresh_at: Instant::now()
                    .checked_sub(BACKGROUND_WORK_REFRESH_INTERVAL)
                    .unwrap_or_else(Instant::now),
            },
        );
        // Startup can emit before the background task hands this receiver to
        // the runtime map. Wake once after installation so those buffered
        // events cannot be stranded behind an already-consumed edge.
        signal_event_pump(&self.event_wake_tx);
        handle
    }

    /// Whether the composer text is the compaction request rather than a
    /// prompt. `/compact` is a UI-intercepted command: Fintwind asks the
    /// provider to summarize the context, so the text must never reach the
    /// model — not even steered into a running turn.
    pub(super) fn is_compact_submission(prompt: &str) -> bool {
        prompt.trim().split_whitespace().next() == Some("/compact")
    }

    /// Ask the provider to compact the session's context. The request is
    /// durable — a busy session compacts at its next safe step boundary, an
    /// idle one starts immediately — and every outcome arrives as
    /// `DriverEvent::CompactionUpdated`.
    pub(super) fn request_context_compaction(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let already_running = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.compaction.as_ref().map(|c| c.status))
            == Some(CompactionStatus::Running);
        if already_running {
            // The provider coalesces repeats while one is pending; the
            // indicator is already up, so stay quiet.
            return;
        }
        if let Some(runtime) = self.runtimes.get(&session_id) {
            runtime.driver.compact();
            if self.state.selected_session == Some(session_id) {
                self.show_toast(tr!("session.compaction_requested"));
            }
        } else if self.state.selected_session == Some(session_id) {
            self.show_toast(tr!("session.compaction_unavailable"));
        }
        cx.notify();
    }

    pub(super) fn submit_composer_submission(
        &mut self,
        submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.selected_session() else {
            return;
        };
        if self.response_fork_preparations.contains_key(&session.id) {
            return;
        }
        // An undo/redo RPC is rewriting the server's revert boundary; a prompt
        // landing mid-flight would delete the staged-away turns (or race the
        // redo's clear) while the finish path still truncates against the old
        // transcript. Queue the message until the preparation settles.
        if self.undo_redo_preparations.contains(&session.id) {
            self.enqueue_follow_up_submission(session.id, submission, cx);
            return;
        }
        if Self::is_compact_submission(&submission.prompt) {
            self.request_context_compaction(session.id, cx);
            return;
        }
        if session.is_busy() {
            // While the agent is working, Enter queues a follow-up instead of
            // refusing the message. The queue drains once the turn settles.
            self.enqueue_follow_up_submission(session.id, submission, cx);
            return;
        }
        self.submit_submission_for_session(session.id, submission, cx);
    }

    /// Deliver a steering message into the running turn. Providers without a
    /// live-turn transport (or a session that is not actively working) fall
    /// back to queueing a follow-up.
    pub(super) fn steer_composer_submission(
        &mut self,
        submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        if Self::is_compact_submission(&submission.prompt) {
            // Steer delivery is exactly what the compaction request wants of
            // a busy session: it runs at the next safe step boundary, ahead
            // of any queued or steered prompts.
            self.request_context_compaction(session.id, cx);
            return;
        }
        if !session.is_busy() {
            self.submit_composer_submission(submission, cx);
            return;
        }
        // A turn that has not reached the provider yet cannot be steered; the
        // driver reports the outcome asynchronously via SteerAccepted or
        // SteerRejected once it is handed off.
        let steerable = session.status != SessionStatus::Connecting
            && self
                .runtimes
                .get(&session.id)
                .is_some_and(|runtime| runtime.driver.supports_steer());
        if !steerable {
            self.enqueue_follow_up_submission(session.id, submission, cx);
            return;
        }
        if let Some(runtime) = self.runtimes.get_mut(&session.id) {
            runtime.driver.steer(submission.prompt.clone());
            runtime.pending_steers.push_back(submission);
        } else {
            self.enqueue_follow_up_submission(session.id, submission, cx);
        }
        cx.notify();
    }

    pub(super) fn enqueue_follow_up_submission(
        &mut self,
        session_id: Uuid,
        mut submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        submission.prompt = submission.prompt.trim().to_owned();
        if submission.prompt.is_empty() {
            return;
        }
        if let Some(session) = self.state.session_mut(session_id) {
            session
                .queued_messages
                .push(submission.into_queued_message());
            session.updated_at = unix_time();
        }
        self.save();
        cx.notify();
    }

    pub(super) fn remove_queued_message(
        &mut self,
        session_id: Uuid,
        message_id: Uuid,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.state.session_mut(session_id) {
            session
                .queued_messages
                .retain(|message| message.id != message_id);
        }
        self.save();
        cx.notify();
    }

    /// Pop a queued message back into the composer so the user can edit and
    /// resubmit it.
    pub(super) fn edit_queued_message(
        &mut self,
        session_id: Uuid,
        message_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.state.session_mut(session_id).and_then(|session| {
            let index = session
                .queued_messages
                .iter()
                .position(|message| message.id == message_id)?;
            Some(session.queued_messages.remove(index))
        }) else {
            return;
        };
        self.restore_composer_submission(ComposerSubmission::from_queued_message(message), cx);
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
        self.save();
        cx.notify();
    }

    /// Deliver a queued follow-up into the running turn right away instead of
    /// waiting for the turn to settle. Falls through the same paths as a
    /// composer steer: an idle session starts a fresh turn, an unsteerable
    /// one re-queues the message.
    pub(super) fn steer_queued_message(
        &mut self,
        session_id: Uuid,
        message_id: Uuid,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.state.session_mut(session_id).and_then(|session| {
            let index = session
                .queued_messages
                .iter()
                .position(|message| message.id == message_id)?;
            Some(session.queued_messages.remove(index))
        }) else {
            return;
        };
        self.save();
        self.steer_composer_submission(ComposerSubmission::from_queued_message(message), cx);
    }

    /// Start the next queued follow-up as a fresh turn. Only called once a
    /// settled turn has been fully closed, so the session is Idle.
    pub(super) fn drain_queued_message(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        if self.response_fork_preparations.contains_key(&session_id) {
            return;
        }
        // Undo/redo finish paths drain the queue themselves once the server's
        // revert boundary is settled; draining earlier would send a prompt
        // into a staged revert and delete the staged-away turns.
        if self.undo_redo_preparations.contains(&session_id) {
            return;
        }
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return;
        };
        if session.is_busy()
            || session.queued_messages.is_empty()
            || self.ending_checkpoint_pending(session_id)
        {
            return;
        }
        let Some(message) = self
            .state
            .session_mut(session_id)
            .map(|session| session.queued_messages.remove(0))
        else {
            return;
        };
        self.submit_submission_for_session(
            session_id,
            ComposerSubmission::from_queued_message(message),
            cx,
        );
    }

    fn submit_submission_for_session(
        &mut self,
        session_id: Uuid,
        submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        if self.response_fork_preparations.contains_key(&session_id) {
            return;
        }
        let selected = self.state.selected_session == Some(session_id);
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return;
        };
        if self.ending_checkpoint_pending(session_id) {
            self.enqueue_follow_up_submission(session_id, submission, cx);
            self.defer_queue_drain(session_id);
            return;
        }
        if session.status.is_busy() {
            self.enqueue_follow_up_submission(session_id, submission, cx);
            return;
        }
        let prompt = submission.prompt.clone();
        let human_prompt = submission.human_prompt();
        let next_turn_count = session.turns.len() + 1;
        let project_id = session.project_id;
        let workspace = session.workspace.clone();
        let driver_start = (!self.runtimes.contains_key(&session_id)).then(|| {
            let provisional_cwd = self
                .workspace_path_for_session(session)
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default();
            self.driver_start_request_for_session(session, provisional_cwd)
        });
        let Some(project) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .cloned()
        else {
            if selected {
                self.restore_composer_submission(submission, cx);
                self.show_toast(tr!("errors.prepare_task_project_not_found"));
            }
            cx.notify();
            return;
        };
        // Busy is visible before any Git work begins. The separate transient
        // set keeps this non-cancellable phase visually distinct from a
        // connecting provider, whose runtime already has a working Stop path.
        //
        // The turn also begins now, not once preparation settles: the sent
        // message and its working indicator belong in the transcript the
        // moment the submission is accepted — a first prompt otherwise leaves
        // the empty state on screen for as long as a `git add -A` takes.
        // Preparation failure unwinds the turn and restores the prompt.
        if selected {
            self.sync_transcript_rows();
        }
        let previous_kinds = if selected {
            self.transcript_row_kinds.borrow().clone()
        } else {
            Vec::new()
        };
        let transcript_anchor = if let Some(session) = self.state.session_mut(session_id) {
            session.set_title_from_prompt(&human_prompt);
            let turn_id = session.begin_turn_with_presentation(
                &prompt,
                submission.display_content.clone(),
                submission.attachments.clone(),
            );
            session.status = SessionStatus::Connecting;
            session.updated_at = unix_time();
            selected.then_some(TranscriptAnchor {
                session_id,
                turn_id,
            })
        } else {
            None
        };
        self.submission_preparations.insert(session_id);
        if selected {
            self.activities_expanded.clear();
            self.expanded_activity_items.clear();
            self.expanded_turns.clear();
            self.expanded_changed_files.clear();
            self.expanded_compactions.clear();
            self.expanded_provider_retries.clear();
            self.transcript_control_focuses.borrow_mut().clear();
            self.message_edit = None;
            self.hide_toast();
            self.transcript_anchor.set(transcript_anchor);
            // Provisional reservation: the anchored list has no measured
            // bounds until its first paint, and a zero end space cannot hold
            // the sent row at the viewport top — without scroll room past the
            // tail, the list clamps to its end and the prompt paints a frame
            // at the bottom before the first measured frame lifts it. Seed a
            // full viewport of end space instead; the overshoot is invisible
            // under the top anchor and the first measured frame trues it up.
            let mut provisional = self.transcript_rows.viewport_bounds().size.height;
            if provisional <= Pixels::ZERO {
                provisional = self.anchored_transcript_rows.viewport_bounds().size.height;
            }
            self.transcript_anchor_end_space.set(provisional);
            self.transcript_anchor_following.set(true);
            self.splice_transcript_rows_after_visibility_change(&previous_kinds);
            self.scroll_transcript_to_anchor();
        }
        cx.notify();

        let preparation_prompt = human_prompt;
        let workspace_client = fintwind_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |fintwind, cx| {
            let prepared = cx
                .background_executor()
                .spawn(async move {
                    prepare_submission(
                        workspace_client,
                        project,
                        workspace,
                        driver_start,
                        session_id,
                        &preparation_prompt,
                        next_turn_count,
                    )
                })
                .await;
            let _ = fintwind.update(cx, move |fintwind, cx| {
                fintwind.finish_submission_preparation(session_id, submission, prepared, cx);
            });
        })
        .detach();
    }

    fn finish_submission_preparation(
        &mut self,
        session_id: Uuid,
        submission: ComposerSubmission,
        prepared: anyhow::Result<PreparedSubmission>,
        cx: &mut Context<Self>,
    ) {
        if !self.submission_preparations.contains(&session_id) {
            return;
        }
        let selected = self.state.selected_session == Some(session_id);
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.submission_preparations.remove(&session_id);
                if selected {
                    self.sync_transcript_rows();
                }
                let previous_kinds = if selected {
                    self.transcript_row_kinds.borrow().clone()
                } else {
                    Vec::new()
                };
                if let Some(session) = self.state.session_mut(session_id)
                    && session.status == SessionStatus::Connecting
                {
                    // The submission never reached a provider and its prompt
                    // returns to the composer, so the eagerly-begun turn and
                    // its message leave the transcript with it.
                    if let Some(turn_id) = session.active_turn_id() {
                        session.unwind_unstarted_turn(turn_id);
                    }
                    session.status = SessionStatus::Idle;
                }
                if selected {
                    if self
                        .transcript_anchor
                        .get()
                        .is_some_and(|anchor| anchor.session_id == session_id)
                    {
                        self.transcript_anchor.set(None);
                        self.transcript_anchor_following.set(false);
                    }
                    self.splice_transcript_rows_after_visibility_change(&previous_kinds);
                    self.restore_composer_submission(submission, cx);
                    self.show_toast(tr!("errors.create_worktree", error = error));
                }
                cx.notify();
                return;
            }
        };
        let PreparedSubmission {
            workspace,
            checkpoint_warning,
            driver: prepared_driver,
        } = prepared;
        // The turn began at accept time; it must still be the untouched one
        // this preparation belongs to. Cancellation is blocked while the
        // preparation set holds the session, so a mismatch means the session
        // was replaced under the preparation rather than a user action.
        let can_start = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| {
                session.status == SessionStatus::Connecting
                    && session.turns.last().is_some_and(|turn| {
                        turn.status == TurnStatus::Running && !turn.provider_turn_started
                    })
            });
        if !can_start {
            self.submission_preparations.remove(&session_id);
            cx.notify();
            return;
        }

        let workspace_changed = self.state.session_mut(session_id).is_some_and(|session| {
            let changed = session.workspace != workspace;
            session.workspace = workspace;
            changed
        });
        if selected && workspace_changed {
            self.invalidate_workspace_queries(cx);
            self.reload_clean_right_panel_file_editors(cx);
            self.ensure_right_panel_terminals(cx);
        }
        let driver = match prepared_driver {
            None => self
                .runtimes
                .get(&session_id)
                .map(|runtime| runtime.driver.clone())
                .ok_or_else(|| anyhow::anyhow!(tr!("errors.prepared_runtime_unavailable"))),
            Some(Ok(prepared)) => Ok(self.install_prepared_driver(session_id, prepared)),
            Some(Err(error)) => Err(error),
        };
        self.invalidate_checkpoint_refs();
        if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime
                .pending_events
                .retain(|event| matches!(event, DriverEvent::BackgroundWork(_)));
            runtime.pending_steers.clear();
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.open_reasoning.clear();
            runtime.settled_reasoning.clear();
            runtime.provider_phase = None;
            // The new submission supersedes any backoff from the previous turn.
            self.provider_retries.remove(&session_id);
            runtime.pending_permission = None;
            runtime.pending_user_input = None;
            runtime.last_active_at = Instant::now();
        }
        // The transcript already shows the turn — the prompt message, its
        // anchor, and the working indicator all landed at accept time. Only
        // preparation's own output surfaces here.
        if selected && let Some(warning) = checkpoint_warning {
            self.show_toast(warning);
        }
        // Template commands expand here, at the seam between the transcript
        // and the transport: the user message keeps the typed `/name …` —
        // the same echo the CLIs show — while the provider receives the
        // rendered prompt. Claude's commands pass through untouched; its CLI
        // owns their expansion.
        let prompt = submission.prompt;
        let driver_prompt =
            crate::composer_complete::expanded_submission(&prompt, &self.slash_command_index)
                .unwrap_or(prompt);
        let mut failed_to_start = false;
        match driver {
            Ok(driver) => {
                // OpenCode deletes the staged-away turns when this prompt
                // lands, so redo stops being possible from here. A failed
                // preparation never reaches the server and keeps the marker.
                self.clear_staged_undo(session_id);
                driver.prompt(driver_prompt);
            }
            Err(error) => {
                failed_to_start = true;
                let message = tr!("errors.start_agent", error = error);
                if let Some(session) = self.state.session_mut(session_id) {
                    session.status = SessionStatus::Failed;
                    session.push_message(MessageRole::Assistant, message);
                }
                self.finish_active_turn(session_id, TurnStatus::Failed);
            }
        }
        // From this point onward `cancel_turn` has either a live driver to
        // cancel or a settled startup failure. The next frame must therefore
        // show Stop (or Send after failure), never the preparation spinner.
        self.submission_preparations.remove(&session_id);
        if failed_to_start {
            self.capture_latest_turn_checkpoint_for(session_id);
            self.start_pending_checkpoint_captures(cx);
        }
        cx.notify();
        // Persist on the next frame boundary. Saving is intentionally after
        // the spinner-to-Stop paint: SQLite or blob externalization must not
        // hold the final preparation frame motionless.
        cx.spawn(async move |fintwind, cx| {
            cx.background_executor().timer(STREAM_FRAME_INTERVAL).await;
            let _ = fintwind.update(cx, |fintwind, _| fintwind.save());
        })
        .detach();
    }

    pub(super) fn collect_runtime_events(runtime: &mut SessionRuntime) {
        while let Ok(event) = runtime.events.try_recv() {
            runtime.pending_events.push_back(event);
        }
    }

    pub(super) fn drain_event_pump(&mut self, cx: &mut Context<Self>) -> EventPumpSchedule {
        // `|` on purpose: a busy provider must not starve the other result
        // queues just because its own drain reported a change first.
        let detection_changed = self.drain_provider_detection_events();
        if self.drain_driver_events(cx)
            | self.drain_provider_probe_events()
            | self.drain_provider_version_events()
            | detection_changed
            | self.drain_task_state_sync_events(cx)
        {
            cx.notify();
        }
        if detection_changed {
            // Provider detection just resolved the CLI binary; the startup
            // reconcile that skipped for lack of one can run now.
            self.schedule_native_session_reconcile(cx);
        }
        if std::mem::take(&mut self.workspace_queries_stale) {
            self.invalidate_workspace_queries(cx);
        }
        if std::mem::take(&mut self.composer_sources_stale) {
            self.refresh_composer_sources(cx);
        }
        self.maybe_refresh_background_work(cx);
        // A finished turn asks for a checkpoint from a handler with no
        // `Context`; this is where that `git` work leaves the UI thread.
        self.start_pending_checkpoint_captures(cx);

        if self
            .runtimes
            .values()
            .any(|runtime| !runtime.pending_events.is_empty() || runtime.stream_remeasure_pending)
        {
            EventPumpSchedule::StreamFrame
        } else if let Some(delay) = self.background_output_refresh_delay() {
            EventPumpSchedule::BackgroundOutput(delay)
        } else {
            EventPumpSchedule::Idle
        }
    }

    pub(super) fn drain_provider_probe_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(probe) = self.provider_probe_events.try_recv() {
            self.provider_model_discoveries_pending
                .remove(OPENCODE_PROVIDER);
            if let Some(existing) = self.probes.first_mut() {
                *existing = probe;
            } else {
                self.probes.push(probe);
            }
            changed = true;
        }
        changed
    }

    pub(super) fn drain_driver_events(&mut self, cx: &mut Context<Self>) -> bool {
        let session_ids = self.runtimes.keys().copied().collect::<Vec<_>>();
        let mut changed = false;
        let mut persisted_state_changed = false;
        let mut force_save = false;
        let mut selected_changed = false;
        for session_id in session_ids {
            let Some(mut runtime) = self.runtimes.remove(&session_id) else {
                continue;
            };
            let follow_up_remeasure = std::mem::take(&mut runtime.stream_remeasure_pending);
            Self::collect_runtime_events(&mut runtime);
            let mut runtime_changed = false;
            let mut background_changed = false;
            let mut background_persisted = false;
            let mut markdown_changed = false;
            let mut keep_runtime = true;
            while let Some(event) = runtime.pending_events.front() {
                let kind = stream_delta_kind(event);
                let event = if let Some(kind) = kind {
                    pop_stream_batch(&mut runtime.pending_events, kind)
                } else {
                    runtime.pending_events.pop_front()
                };
                let Some(event) = event else {
                    break;
                };
                let background_event = matches!(event, DriverEvent::BackgroundWork(_));
                let background_output_delta = matches!(
                    event,
                    DriverEvent::BackgroundWork(BackgroundWorkEvent::OutputDelta { .. })
                );
                background_persisted |= matches!(
                    &event,
                    DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(
                        BackgroundWorkTranscriptEvent::Finished { .. }
                            | BackgroundWorkTranscriptEvent::Snapshot { .. }
                    ))
                );
                force_save |= matches!(
                    event,
                    DriverEvent::Connected { .. }
                        | DriverEvent::AgentPresetSelected(_)
                        | DriverEvent::AutoTitleUpdated(_)
                        | DriverEvent::Permission { .. }
                        | DriverEvent::SteerAccepted { .. }
                        | DriverEvent::SteerRejected { .. }
                        | DriverEvent::TurnFinished { .. }
                        | DriverEvent::Error(_)
                        | DriverEvent::ProcessExited
                );
                // Reasoning is markdown too (the live peek renders it), and
                // this flag is also what routes the pump onto the coalesced
                // `StreamFrame` cadence: without it a reasoning-only drain
                // reported Idle, so every fast thinking chunk woke the pump
                // for an immediate drain-and-notify — 40+ full re-renders a
                // second, sailing straight past the 120 ms commit floor.
                markdown_changed |= matches!(
                    event,
                    DriverEvent::TextDelta(_)
                        | DriverEvent::ReasoningDelta { .. }
                        // The authoritative fragment text rewrites the live
                        // block's markdown in one pass.
                        | DriverEvent::ReasoningEnded { .. }
                );
                if background_output_delta {
                    // The registry batches log text into SharedString at 10Hz;
                    // repainting and saving for every provider chunk would
                    // turn a noisy command into UI-thread work.
                } else if background_event {
                    background_changed = true;
                } else {
                    runtime_changed = true;
                }
                keep_runtime &= self.handle_driver_event(session_id, &mut runtime, event, true, cx);
                if !keep_runtime {
                    break;
                }
            }
            runtime.stream_remeasure_pending = markdown_changed;
            if keep_runtime {
                self.runtimes.insert(session_id, runtime);
            }
            changed |= runtime_changed || background_changed;
            persisted_state_changed |= runtime_changed || background_persisted;
            force_save |= background_persisted;
            if self.state.selected_session == Some(session_id)
                && (runtime_changed || follow_up_remeasure)
            {
                selected_changed = true;
            }
        }

        if !self.pending_queue_drains.is_empty() {
            let drains = std::mem::take(&mut self.pending_queue_drains);
            for session_id in drains {
                if self.ending_checkpoint_pending(session_id) {
                    self.defer_queue_drain(session_id);
                } else {
                    self.drain_queued_message(session_id, cx);
                }
            }
            changed = true;
        }

        if persisted_state_changed {
            self.stream_state_dirty = true;
        }
        if selected_changed {
            self.remeasure_transcript_tail();
        }
        if self.stream_state_dirty
            && (force_save || self.last_stream_save.elapsed() >= STREAM_SAVE_INTERVAL)
        {
            self.save();
        }
        changed || selected_changed
    }
}

#[cfg(test)]
mod response_fork_title_tests {
    use super::next_response_fork_title;

    #[test]
    fn response_fork_titles_advance_one_numbered_sequence() {
        assert_eq!(
            next_response_fork_title("Fix the bug", ["Fix the bug"]),
            "Fix the bug (2)"
        );
        assert_eq!(
            next_response_fork_title(
                "Fix the bug",
                ["Fix the bug", "Fix the bug (2)", "Fix the bug (4)"]
            ),
            "Fix the bug (5)"
        );
        assert_eq!(
            next_response_fork_title("Fix the bug (2)", ["Fix the bug", "Fix the bug (2)"]),
            "Fix the bug (3)"
        );
        assert_eq!(
            next_response_fork_title("Plan (2026)", ["Plan (2026)"]),
            "Plan (2026) (2)"
        );
    }
}

#[cfg(test)]
mod version_tests {
    use crate::model::parse_cli_version;

    #[test]
    fn parses_common_cli_version_banners() {
        assert_eq!(
            parse_cli_version("codex-cli 0.45.0\n"),
            Some("0.45.0".to_owned())
        );
        assert_eq!(
            parse_cli_version("2.1.24 (Claude Code)\n"),
            Some("2.1.24".to_owned())
        );
        assert_eq!(
            parse_cli_version("v1.3.0-beta.2"),
            Some("1.3.0-beta.2".to_owned())
        );
        assert_eq!(
            parse_cli_version("\nAmp CLI version 0.9.12\n"),
            Some("0.9.12".to_owned())
        );
        assert_eq!(parse_cli_version("not a version"), None);
        assert_eq!(parse_cli_version(""), None);
    }

    #[test]
    fn version_requires_a_dotted_number_not_a_bare_digit() {
        // "2024" alone or a hash must not read as a version.
        assert_eq!(parse_cli_version("build 2024 f3a9c1"), None);
        assert_eq!(
            parse_cli_version("cursor-agent 2025.09.12-4f8d8e2"),
            Some("2025.09.12-4f8d8e2".to_owned())
        );
    }
}

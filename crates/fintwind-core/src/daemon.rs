//! Provider backend and driver-event wire translation for `fintwind-daemon`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::{
    Backend, Command, EventSink, Request, ResponsePayload, WireDriverEvent, WorkspaceOperation,
    WorkspaceResult,
};
use anyhow::{Context as _, anyhow, bail};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::attachments::AttachmentStore;
use crate::driver::{self, DriverHandle, DriverStartOptions, SessionOptions};
use crate::model::{
    ActivityKind, AgentSession, Checkpoint, CheckpointStatus, DriverEvent, PermissionOption,
    Project, ProviderResumeCursor, ProviderRetryAction, SessionStatus,
};
use crate::persistence::{ComposerDraftStore, PersistedState, StateStore};
use crate::settings::DaemonSettingsStore;
use fintwind_protocol::provider_session::{ProviderSessionFork, ProviderSessionForkRequest};

pub struct FintwindBackend {
    sessions: Mutex<HashMap<Uuid, (Uuid, DriverHandle)>>,
    terminals: Mutex<HashMap<Uuid, (Uuid, crate::terminal::DaemonTerminal)>>,
    settings: DaemonSettingsStore,
    task_store: StateStore,
    task_state: Mutex<PersistedState>,
    removed_session_ids: Mutex<HashSet<Uuid>>,
    removed_project_ids: Mutex<HashSet<Uuid>>,
    composer_drafts: ComposerDraftStore,
    attachments: AttachmentStore,
    checkpoint_capture_locks: Mutex<HashMap<(PathBuf, Uuid, usize), Arc<Mutex<()>>>>,
    default_cwd: std::path::PathBuf,
}

impl FintwindBackend {
    pub fn new(settings: DaemonSettingsStore, task_store: StateStore) -> anyhow::Result<Self> {
        let mut task_state = task_store
            .load()
            .context("could not load fintwind task database")?;
        migrate_projectless_state(&task_store, &mut task_state)?;
        let composer_drafts = ComposerDraftStore::for_state_path(task_store.path());
        let attachments = AttachmentStore::new(
            task_store
                .path()
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("attachments"),
        );
        Ok(Self {
            sessions: Mutex::new(HashMap::new()),
            terminals: Mutex::new(HashMap::new()),
            settings,
            task_store,
            task_state: Mutex::new(task_state),
            removed_session_ids: Mutex::new(HashSet::new()),
            removed_project_ids: Mutex::new(HashSet::new()),
            composer_drafts,
            attachments,
            checkpoint_capture_locks: Mutex::new(HashMap::new()),
            default_cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        })
    }

    /// Capture and persist one ending checkpoint exactly once per daemon.
    /// Desktop and Web may observe the same turn completion concurrently; a
    /// per-turn lock prevents both clients from running the expensive Git
    /// snapshot while leaving unrelated tasks independent.
    fn capture_turn_checkpoint(
        &self,
        cwd: PathBuf,
        session_id: Uuid,
        turn_count: usize,
    ) -> anyhow::Result<Checkpoint> {
        let key = (cwd.clone(), session_id, turn_count);
        let capture_lock = self
            .checkpoint_capture_locks
            .lock()
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _capture = capture_lock.lock();

        {
            let mut state = self.task_state.lock();
            if let Some(index) = state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
            {
                self.task_store.hydrate(&mut state.sessions[index])?;
                if let Some(checkpoint) = state.sessions[index]
                    .turns
                    .iter()
                    .find(|turn| turn.turn_count == turn_count)
                    .and_then(|turn| turn.checkpoint.as_ref())
                    .filter(|checkpoint| {
                        matches!(
                            checkpoint.status,
                            CheckpointStatus::Ready | CheckpointStatus::Unavailable
                        )
                    })
                {
                    return Ok(checkpoint.clone());
                }
            }
        }

        let checkpoint = crate::checkpoint::capture_turn(&cwd, session_id, turn_count)?;
        let mut state = self.task_state.lock();
        if let Some(index) = state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        {
            self.task_store.hydrate(&mut state.sessions[index])?;
            if let Some(turn) = state.sessions[index]
                .turns
                .iter_mut()
                .find(|turn| turn.turn_count == turn_count)
            {
                turn.checkpoint = Some(checkpoint.clone());
                state.mark_session_dirty(session_id);
                self.task_store.save(&mut state)?;
            }
        }
        Ok(checkpoint)
    }

    /// Drop a project and its local session rows from the daemon catalog.
    /// The project folder and OpenCode-native sessions are left untouched.
    fn remove_project_from_catalog(&self, project_id: Uuid) -> anyhow::Result<()> {
        let session_ids = {
            let mut state = self.task_state.lock();
            let mut removed_session_ids = self.removed_session_ids.lock();
            let mut removed_project_ids = self.removed_project_ids.lock();
            removed_project_ids.insert(project_id);
            let session_ids = state
                .sessions
                .iter()
                .filter(|session| session.project_id == project_id)
                .map(|session| session.id)
                .collect::<Vec<_>>();
            for session_id in &session_ids {
                removed_session_ids.insert(*session_id);
            }
            drop(removed_session_ids);
            drop(removed_project_ids);
            state
                .sessions
                .retain(|session| session.project_id != project_id);
            state.projects.retain(|project| project.id != project_id);
            self.task_store.save(&mut state)?;
            session_ids
        };
        let mut sessions = self.sessions.lock();
        for session_id in session_ids {
            drop(sessions.remove(&session_id));
        }
        Ok(())
    }
}

/// Storage-layout migrations belong to the daemon because both the database
/// rows and the directories name paths on its host. Persist after each move
/// so a later failure cannot leave an earlier project pointing at its old
/// location in SQLite.
fn migrate_projectless_state(
    task_store: &StateStore,
    task_state: &mut PersistedState,
) -> anyhow::Result<()> {
    let indices = task_state
        .projects
        .iter()
        .enumerate()
        .filter_map(|(index, project)| {
            crate::projectless::needs_migration(&project.path).then_some(index)
        })
        .collect::<Vec<_>>();
    for index in indices {
        let old_path = task_state.projects[index].path.clone();
        let workspace = crate::projectless::migrate_workspace(&old_path).with_context(|| {
            format!(
                "could not move projectless workspace {} under ~/.fintwind/projects",
                old_path.display()
            )
        })?;
        task_state.projects[index].name = crate::model::Project::PROJECTLESS_NAME.to_owned();
        task_state.projects[index].path = workspace.cwd;
        task_store
            .save(task_state)
            .context("could not persist migrated projectless workspace")?;
    }
    Ok(())
}

impl Backend for FintwindBackend {
    fn handle(&self, request: Request, events: EventSink) -> anyhow::Result<ResponsePayload> {
        let session_id = request.session_id;
        let runtime_id = request.runtime_id;
        match request.command {
            Command::AttachSession => {
                let sessions = self.sessions.lock();
                let Some((runtime_id, driver)) = sessions.get(&session_id) else {
                    return Ok(ResponsePayload::SessionRuntime {
                        runtime_id: None,
                        supports_steer: false,
                    });
                };
                Ok(ResponsePayload::SessionRuntime {
                    runtime_id: Some(*runtime_id),
                    supports_steer: driver.supports_steer(),
                })
            }
            Command::GetSettings => Ok(ResponsePayload::Settings {
                settings: self.settings.get(),
            }),
            Command::UpdateSettings { settings } => {
                self.settings.replace(settings)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ProbeProvider {
                binary_override,
                discover_models,
                probe_version,
            } => {
                ensure_shell_environment();
                let mut probe = match binary_override.as_deref() {
                    override_value if discover_models || probe_version => {
                        crate::model::provider_probe(override_value)
                    }
                    override_value => crate::model::cached_provider_probe(override_value),
                };
                let version = probe_version
                    .then(|| {
                        probe
                            .path
                            .as_deref()
                            .and_then(crate::model::probe_provider_version)
                    })
                    .flatten();
                if discover_models {
                    probe = crate::model::discover_provider_models(probe);
                }
                Ok(ResponsePayload::ProviderProbe { probe, version })
            }
            Command::FetchPlanUsage {
                binary_override: _,
                cli_version: _,
            } => {
                let usage = crate::usage::fetch_opencode_go_plan_usage()?;
                Ok(ResponsePayload::PlanUsage { usage })
            }
            Command::LoadSkills { projects } => {
                let locations = crate::skills::skill_locations(&projects);
                Ok(ResponsePayload::SkillsCatalog {
                    catalog: crate::skills::scan_skills(&locations),
                })
            }
            Command::SetSkillsEnabled { dirs, enabled } => {
                for dir in dirs {
                    crate::skills::set_skill_enabled(&dir, enabled)
                        .map_err(|error| anyhow!(error))?;
                }
                Ok(ResponsePayload::Ack)
            }
            Command::TrashSkills { dirs } => {
                crate::skills::trash_skills(&dirs).map_err(|error| anyhow!(error))?;
                Ok(ResponsePayload::Ack)
            }
            Command::LoadTaskState => {
                let state = self.task_state.lock();
                Ok(ResponsePayload::TaskState {
                    projects: state.projects.clone(),
                    sessions: state
                        .sessions
                        .iter()
                        .map(AgentSession::list_projection)
                        .collect(),
                    default_cwd: self.default_cwd.clone(),
                    projectless_root: crate::projectless::workspace_root(),
                })
            }
            Command::SaveTaskState {
                projects,
                live_session_ids: _,
                sessions,
            } => {
                let active_runtimes = self
                    .sessions
                    .lock()
                    .iter()
                    .map(|(session_id, (runtime_id, _))| (*session_id, *runtime_id))
                    .collect::<HashMap<_, _>>();
                let mut state = self.task_state.lock();
                let removed_session_ids = self.removed_session_ids.lock();
                let removed_project_ids = self.removed_project_ids.lock();
                for project in projects {
                    if removed_project_ids.contains(&project.id) {
                        continue;
                    }
                    if let Some(existing) = state
                        .projects
                        .iter_mut()
                        .find(|existing| existing.id == project.id)
                    {
                        *existing = project;
                    } else {
                        state.projects.push(project);
                    }
                }
                let sessions = sessions
                    .into_iter()
                    .filter(|session| {
                        !removed_session_ids.contains(&session.id)
                            && !removed_project_ids.contains(&session.project_id)
                    })
                    .collect::<Vec<_>>();
                drop(removed_session_ids);
                drop(removed_project_ids);
                let saved_ids = sessions
                    .iter()
                    .map(|session| session.id)
                    .collect::<Vec<_>>();
                for mut session in sessions {
                    if let Some(existing) = state
                        .sessions
                        .iter_mut()
                        .find(|existing| existing.id == session.id)
                    {
                        if session_projection_precedes(
                            existing,
                            &session,
                            active_runtimes.get(&session.id).copied(),
                        ) {
                            merge_stale_session_metadata(existing, session);
                        } else {
                            preserve_daemon_checkpoints(existing, &mut session);
                            *existing = session;
                        }
                    } else {
                        state.sessions.push(session);
                    }
                }
                let used_project_ids = state
                    .sessions
                    .iter()
                    .map(|session| session.project_id)
                    .collect::<std::collections::HashSet<_>>();
                state.projects.retain(|project| {
                    !project.is_projectless() || used_project_ids.contains(&project.id)
                });
                for session_id in &saved_ids {
                    state.mark_session_dirty(*session_id);
                }
                self.task_store.save(&mut state)?;
                let sessions = saved_ids
                    .into_iter()
                    .filter_map(|session_id| {
                        state
                            .sessions
                            .iter()
                            .find(|session| session.id == session_id)
                            .cloned()
                    })
                    .collect();
                Ok(ResponsePayload::TaskStateSaved { sessions })
            }
            Command::RemoveProject { project_id } => {
                self.remove_project_from_catalog(project_id)?;
                Ok(ResponsePayload::Ack)
            }
            Command::RemoveSession => {
                {
                    let mut state = self.task_state.lock();
                    self.removed_session_ids.lock().insert(session_id);
                    let project_id = state
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .map(|session| session.project_id);
                    state.sessions.retain(|session| session.id != session_id);
                    if let Some(project_id) = project_id {
                        let remove_project = state
                            .projects
                            .iter()
                            .find(|project| project.id == project_id)
                            .is_some_and(Project::is_projectless)
                            && !state
                                .sessions
                                .iter()
                                .any(|session| session.project_id == project_id);
                        if remove_project {
                            state.projects.retain(|project| project.id != project_id);
                        }
                    }
                    self.task_store.save(&mut state)?;
                }
                let removed = self.sessions.lock().remove(&session_id);
                drop(removed);
                Ok(ResponsePayload::Ack)
            }
            Command::HydrateSession { session_id } => {
                let mut state = self.task_state.lock();
                let session = if let Some(session) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    self.task_store.hydrate(session)?;
                    Some(session.clone())
                } else {
                    None
                };
                Ok(ResponsePayload::Session { session })
            }
            Command::SearchSessionMessages { query, limit } => {
                let matches = self.task_store.session_message_search(query, limit)()?;
                Ok(ResponsePayload::SessionMessageMatches { matches })
            }
            Command::LoadComposerDrafts => Ok(ResponsePayload::ComposerDrafts {
                drafts: self.composer_drafts.load()?,
            }),
            Command::SaveComposerDrafts { drafts, generation } => {
                self.composer_drafts.save(drafts, generation)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ApplyComposerDraftChanges { changes } => {
                self.composer_drafts.apply_changes(changes)?;
                Ok(ResponsePayload::Ack)
            }
            Command::StoreBlob { mime_type, bytes } => {
                if bytes.len() > crate::attachments::MAX_PROMPT_FILE_BYTES {
                    bail!("attachment is larger than 20 MB");
                }
                let reference = self
                    .task_store
                    .blobs()
                    .store_image_bytes(&mime_type, &bytes)?;
                let path = self
                    .task_store
                    .blobs()
                    .path_for(&reference)
                    .ok_or_else(|| anyhow!("stored blob has no daemon path"))?;
                Ok(ResponsePayload::BlobStored { reference, path })
            }
            Command::ImportAttachment { name, upload } => Ok(ResponsePayload::AttachmentStored {
                attachment: self.attachments.import(&name, upload)?,
            }),
            Command::ImportPathAttachment { path } => Ok(ResponsePayload::AttachmentStored {
                attachment: self.attachments.import_path(&path)?,
            }),
            Command::ReadBlob { reference } => {
                let path = self
                    .task_store
                    .blobs()
                    .path_for(&reference)
                    .ok_or_else(|| anyhow!("invalid blob reference"))?;
                Ok(ResponsePayload::BlobData {
                    bytes: std::fs::read(path)?,
                })
            }
            Command::ReadAttachment { reference, path } => Ok(ResponsePayload::BlobData {
                bytes: self.attachments.read_file(&reference, &path)?,
            }),
            Command::SweepBlobs => {
                self.task_store.blob_sweep()();
                Ok(ResponsePayload::Ack)
            }
            Command::ForkSessionFromResponse { turn_count } => {
                let (session, checkpoint_warning) =
                    self.fork_session_from_response(session_id, turn_count)?;
                Ok(ResponsePayload::SessionForked {
                    session,
                    checkpoint_warning,
                })
            }
            Command::RewindSessionToMessage { turn_count } => {
                let (session, cleanup_warning) =
                    self.rewind_session_to_message(session_id, turn_count)?;
                Ok(ResponsePayload::SessionRewound {
                    session,
                    cleanup_warning,
                })
            }
            Command::ForkProviderSession { request } => {
                Ok(ResponsePayload::ProviderSessionForked {
                    result: fork_provider_session(request)?,
                })
            }
            Command::ListProviderSessions { binary, directory } => {
                // Goes through the binary's private global server; the
                // directory rides along as per-request location data.
                // Blocking I/O, so this runs on the request thread.
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let sessions =
                    crate::driver::native::list_sessions(&server, &directory.to_string_lossy())?;
                Ok(ResponsePayload::ProviderSessions { sessions })
            }
            Command::FetchNativeTranscript {
                binary,
                directory,
                session_id,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let transcript = crate::driver::native::fetch_transcript(&server, &session_id)?;
                Ok(ResponsePayload::NativeTranscript { transcript })
            }
            Command::FetchUsageStats {
                binary,
                directory,
                detailed_day,
            } => {
                // One pass over the whole session store; `directory` only
                // scopes the request's location. Blocking I/O, so
                // this runs on the request thread like the other
                // sessionless OpenCode reads.
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let stats = crate::driver::native::fetch_usage_stats(&server, detailed_day)?;
                Ok(ResponsePayload::UsageStats { stats })
            }
            Command::RenameProviderSession {
                binary,
                directory,
                session_id,
                title,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                crate::driver::native::rename_session(&server, &session_id, &title)?;
                Ok(ResponsePayload::Ack)
            }
            Command::DeleteProviderSession {
                binary,
                directory,
                session_id,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                crate::driver::native::delete_session(&server, &session_id)?;
                Ok(ResponsePayload::Ack)
            }
            Command::FetchIntegrations { binary, directory } => {
                // One round trip to the private global OpenCode server; the
                // connection list is the authorization truth. Blocking I/O,
                // so this runs on the request thread like the other
                // sessionless OpenCode reads.
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let integrations = crate::driver::native::list_integrations(&server)?;
                Ok(ResponsePayload::Integrations { integrations })
            }
            Command::AuthorizeProvider {
                binary,
                directory,
                provider_id,
                key,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                crate::driver::native::authorize_integration(&server, &provider_id, &key)?;
                Ok(ResponsePayload::Ack)
            }
            Command::LogoutProvider {
                binary,
                directory,
                provider_id,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                crate::driver::native::logout_integration(&server, &provider_id)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ProbeBuiltinProvider {
                binary,
                directory,
                provider_id,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let models = crate::driver::native::probe_provider_models(
                    &server,
                    &directory.to_string_lossy(),
                    &provider_id,
                )?;
                Ok(ResponsePayload::BuiltinProviderProbed { models })
            }
            Command::AuthenticateMcpServer {
                binary,
                directory,
                name,
            } => {
                crate::mcp_auth::authenticate(&binary, &directory, &name)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ListMcpServerStatuses { binary, directory } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let statuses = crate::driver::native::list_mcp_statuses(
                    &server,
                    &directory.to_string_lossy(),
                )?;
                Ok(ResponsePayload::McpServerStatuses { statuses })
            }
            Command::CancelAuthenticateMcpServer { name } => {
                crate::mcp_auth::cancel(&name);
                Ok(ResponsePayload::Ack)
            }
            Command::Workspace {
                operation:
                    WorkspaceOperation::CaptureTurn {
                        cwd,
                        session_id,
                        turn_count,
                    },
            } => Ok(ResponsePayload::Workspace {
                result: WorkspaceResult::Checkpoint {
                    checkpoint: self.capture_turn_checkpoint(cwd, session_id, turn_count)?,
                },
            }),
            Command::Workspace { operation } => Ok(ResponsePayload::Workspace {
                result: crate::workspace::execute(operation)?,
            }),
            Command::OpenTerminal { cwd, cols, rows } => {
                ensure_shell_environment();
                let terminal = crate::terminal::DaemonTerminal::open(&cwd, cols, rows, events)?;
                let previous = self
                    .terminals
                    .lock()
                    .insert(session_id, (runtime_id, terminal));
                drop(previous);
                Ok(ResponsePayload::Ack)
            }
            Command::WriteTerminal { data } => {
                let terminals = self.terminals.lock();
                let (active_runtime_id, terminal) = terminals
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("daemon terminal {session_id} is not running"))?;
                if *active_runtime_id != runtime_id {
                    bail!(
                        "daemon terminal {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                    );
                }
                terminal.write(data)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ResizeTerminal { cols, rows } => {
                let terminals = self.terminals.lock();
                let (active_runtime_id, terminal) = terminals
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("daemon terminal {session_id} is not running"))?;
                if *active_runtime_id != runtime_id {
                    bail!(
                        "daemon terminal {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                    );
                }
                terminal.resize(cols, rows);
                Ok(ResponsePayload::Ack)
            }
            Command::CloseTerminal => {
                let removed = {
                    let mut terminals = self.terminals.lock();
                    if let Some((active_runtime_id, _)) = terminals.get(&session_id) {
                        if *active_runtime_id != runtime_id {
                            bail!(
                                "daemon terminal {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                            );
                        }
                    }
                    terminals.remove(&session_id)
                };
                drop(removed);
                Ok(ResponsePayload::Ack)
            }
            Command::Start { options } => {
                let previous = self.sessions.lock().remove(&session_id);
                drop(previous);
                let options = DriverStartOptions {
                    binary: options.binary,
                    cwd: options.cwd,
                    mode: decode_enum(&options.mode)?,
                    interaction_mode: decode_enum(&options.interaction_mode)?,
                    model: options.model,
                    reasoning_effort: options.reasoning_effort,
                    service_tier: options.service_tier,
                    context_window: options.context_window,
                    agent_preset: options.agent_preset,
                    provider_cursor: options
                        .provider_cursor
                        .map(serde_json::from_value)
                        .transpose()
                        .context("daemon received an invalid provider cursor")?,
                };
                let (wake, _wake_events) = smol::channel::bounded(1);
                let (event_sender, event_receiver) = driver::event_channel(wake);
                let handle = driver::start_local(options, event_sender)?;
                let supports_steer = handle.supports_steer();
                std::thread::Builder::new()
                    .name(format!("fintwind-daemon-events-{session_id}"))
                    .spawn(move || {
                        while let Ok(event) = event_receiver.recv() {
                            let wire = event_to_wire(event).unwrap_or_else(|error| {
                                WireDriverEvent::new(
                                    "error",
                                    Value::String(format!(
                                        "could not encode daemon event: {error}"
                                    )),
                                )
                            });
                            if events.send(wire).is_err() {
                                break;
                            }
                        }
                    })
                    .context("could not start daemon event forwarding thread")?;
                self.sessions
                    .lock()
                    .insert(session_id, (runtime_id, handle));
                Ok(ResponsePayload::Started { supports_steer })
            }
            Command::CloseSession => {
                let removed = {
                    let mut sessions = self.sessions.lock();
                    sessions
                        .get(&session_id)
                        .is_some_and(|(active_runtime_id, _)| *active_runtime_id == runtime_id)
                        .then(|| sessions.remove(&session_id))
                        .flatten()
                };
                drop(removed);
                Ok(ResponsePayload::Ack)
            }
            command => {
                let driver = {
                    let sessions = self.sessions.lock();
                    let (active_runtime_id, driver) = sessions
                        .get(&session_id)
                        .ok_or_else(|| anyhow!("daemon session {session_id} is not running"))?;
                    if *active_runtime_id != runtime_id {
                        bail!(
                            "daemon session {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                        );
                    }
                    driver.clone()
                };
                handle_driver_command(&driver, command)
            }
        }
    }

    fn shutdown(&self) {
        let sessions = std::mem::take(&mut *self.sessions.lock());
        drop(sessions);
        let terminals = std::mem::take(&mut *self.terminals.lock());
        drop(terminals);
        crate::opencode_pool::shutdown_all();
    }
}

fn session_projection_precedes(
    existing: &AgentSession,
    incoming: &AgentSession,
    active_runtime_id: Option<Uuid>,
) -> bool {
    let existing_cursor = existing.runtime_event_cursor;
    let incoming_cursor = incoming.runtime_event_cursor;
    if let Some(active_runtime_id) = active_runtime_id {
        let existing_is_active =
            existing_cursor.is_some_and(|cursor| cursor.runtime_id == active_runtime_id);
        let incoming_is_active =
            incoming_cursor.is_some_and(|cursor| cursor.runtime_id == active_runtime_id);
        if existing_is_active != incoming_is_active {
            return existing_is_active;
        }
    }
    match (existing_cursor, incoming_cursor) {
        (Some(existing), Some(incoming))
            if existing.runtime_id == incoming.runtime_id && existing.epoch == incoming.epoch =>
        {
            incoming.sequence < existing.sequence
        }
        (Some(_), None) if existing.status.is_busy() => true,
        _ => incoming.updated_at < existing.updated_at,
    }
}

fn merge_stale_session_metadata(existing: &mut AgentSession, incoming: AgentSession) {
    if incoming.updated_at >= existing.updated_at {
        existing.title = incoming.title;
        existing.project_id = incoming.project_id;
        existing.workspace = incoming.workspace;
        existing.provider = incoming.provider;
        existing.model = incoming.model;
        existing.runtime_mode = incoming.runtime_mode;
        existing.interaction_mode = incoming.interaction_mode;
        existing.reasoning_effort = incoming.reasoning_effort;
        existing.service_tier = incoming.service_tier;
        existing.context_window = incoming.context_window;
        existing.agent_preset = incoming.agent_preset;
        existing.updated_at = incoming.updated_at;
        existing.last_reply_at = incoming.last_reply_at.or(existing.last_reply_at);
    }
    for queued in incoming.queued_messages {
        if !existing
            .queued_messages
            .iter()
            .any(|candidate| candidate.id == queued.id)
        {
            existing.queued_messages.push(queued);
        }
    }
}

/// Ending checkpoints are produced and stored by the daemon. A second client
/// may still save a projection created just before capture completed; never
/// let that stale projection erase the canonical Git snapshot.
fn preserve_daemon_checkpoints(existing: &AgentSession, incoming: &mut AgentSession) {
    for turn in &mut incoming.turns {
        let Some(checkpoint) = existing
            .turns
            .iter()
            .find(|candidate| candidate.turn_count == turn.turn_count)
            .and_then(|candidate| candidate.checkpoint.as_ref())
            .filter(|checkpoint| {
                matches!(
                    checkpoint.status,
                    CheckpointStatus::Ready | CheckpointStatus::Unavailable
                )
            })
        else {
            continue;
        };
        turn.checkpoint = Some(checkpoint.clone());
    }
}

impl FintwindBackend {
    /// Fork a response using only daemon-host state.
    ///
    /// A browser must never reconstruct or persist this operation itself:
    /// provider-native sessions, checkpoint refs, and the task database all
    /// belong to the daemon and may be on another machine.
    fn fork_session_from_response(
        &self,
        session_id: Uuid,
        turn_count: usize,
    ) -> anyhow::Result<(AgentSession, Option<String>)> {
        let (source, cwd, fork_title) = {
            let mut state = self.task_state.lock();
            let source_index = state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
                .ok_or_else(|| anyhow!("the source task is unavailable"))?;
            self.task_store
                .hydrate(&mut state.sessions[source_index])
                .context("could not load the source task")?;
            let source = state.sessions[source_index].clone();
            let project = state
                .projects
                .iter()
                .find(|project| project.id == source.project_id)
                .ok_or_else(|| anyhow!("the source task project is unavailable"))?;
            let cwd = source.workspace.path().unwrap_or(&project.path).to_owned();
            let fork_title = next_response_fork_title(
                source.display_title(),
                state
                    .sessions
                    .iter()
                    .filter(|session| session.project_id == source.project_id)
                    .map(AgentSession::display_title),
            );
            (source, cwd, fork_title)
        };

        validate_response_fork(&source, turn_count)?;
        let provider_turn_count = source
            .turns
            .iter()
            .take(turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count();
        let (provider_cursor, message_ids) =
            self.fork_provider_response(&source, &cwd, provider_turn_count)?;
        let mut forked = source
            .fork_through_turn(turn_count, provider_cursor, &fork_title)
            .ok_or_else(|| anyhow!("the selected response cannot be copied"))?;
        if !message_ids.is_empty() {
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
                checkpoint.git_ref =
                    crate::checkpoint::checkpoint_ref(fork_id, checkpoint.turn_count);
            }
        }
        let checkpoint_warning =
            crate::checkpoint::copy_session_refs(&cwd, source.id, fork_id, turn_count)
                .err()
                .map(|error| error.to_string());

        let mut state = self.task_state.lock();
        state.push_session(forked.clone());
        if let Err(error) = self.task_store.save(&mut state) {
            state.sessions.retain(|session| session.id != fork_id);
            let _ = crate::checkpoint::delete_all_session_refs(&cwd, fork_id);
            return Err(error).context("could not save the forked task");
        }
        Ok((forked, checkpoint_warning))
    }

    /// Restore the daemon-host worktree, provider conversation, and stored
    /// transcript to immediately before one user turn.
    fn rewind_session_to_message(
        &self,
        session_id: Uuid,
        turn_count: usize,
    ) -> anyhow::Result<(AgentSession, Option<String>)> {
        let (source, cwd) = {
            let mut state = self.task_state.lock();
            let source_index = state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
                .ok_or_else(|| anyhow!("the task is unavailable"))?;
            self.task_store
                .hydrate(&mut state.sessions[source_index])
                .context("could not load the task")?;
            let source = state.sessions[source_index].clone();
            let project = state
                .projects
                .iter()
                .find(|project| project.id == source.project_id)
                .ok_or_else(|| anyhow!("the task project is unavailable"))?;
            let cwd = source.workspace.path().unwrap_or(&project.path).to_owned();
            (source, cwd)
        };
        validate_message_rewind(&source, turn_count)?;

        let retained_turn_count = turn_count.saturating_sub(1);
        let rollback_turns = source.provider_turns_after(retained_turn_count);
        if rollback_turns > 0 {
            // OpenCode's revert marks a native user-message boundary, so the
            // index must count only turns that reached the provider — a
            // locally failed turn has no native message to retain.
            let provider_turn_count = source
                .turns
                .iter()
                .take(retained_turn_count)
                .filter(|turn| turn.provider_turn_started)
                .count();
            // OpenCode's revert rewrites the worktree with its own snapshot,
            // so no fintwind git checkpoint is captured or restored here. The
            // marker is the work: it hides the dropped turns from the next
            // model call and rolls their file changes back.
            let binary = self.provider_binary()?;
            self.rewind_provider_response(
                &source,
                &cwd,
                &binary,
                provider_turn_count,
                rollback_turns,
            )?;
        }
        // The native session id survives OpenCode's revert, so the stored
        // cursor keeps working no matter how many turns were dropped.
        let provider_cursor = source.provider_cursor.clone();

        // Dropping a resident source driver prevents its late events from
        // racing the rewound transcript; the next prompt starts fresh and
        // OpenCode's pending cleanup physically removes the dropped turns.
        let removed = self.sessions.lock().remove(&session_id);
        drop(removed);

        let mut rewound = source.clone();
        rewound.provider_cursor = provider_cursor;
        rewound.truncate_after_turn(retained_turn_count);
        rewound.status = SessionStatus::Idle;

        let mut state = self.task_state.lock();
        let existing = state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("the task was removed while it was being rewound"))?;
        *existing = rewound.clone();
        state.mark_session_dirty(session_id);
        self.task_store
            .save(&mut state)
            .context("could not save the rewound task")?;
        Ok((rewound, None))
    }

    fn fork_provider_response(
        &self,
        source: &AgentSession,
        cwd: &Path,
        provider_turn_count: usize,
    ) -> anyhow::Result<(ProviderResumeCursor, HashMap<String, String>)> {
        let Some(ProviderResumeCursor::OpenCode { session_id }) = source.provider_cursor.as_ref()
        else {
            bail!("OpenCode's native session is unavailable");
        };
        let fork = fork_provider_session(ProviderSessionForkRequest::OpenCode {
            binary: self.provider_binary()?,
            cwd: cwd.to_owned(),
            session_id: session_id.clone(),
            turn_count: provider_turn_count,
        })?;
        Ok((fork.cursor, HashMap::new()))
    }

    fn rewind_provider_response(
        &self,
        source: &AgentSession,
        cwd: &Path,
        binary: &Path,
        provider_turn_count: usize,
        rollback_turns: usize,
    ) -> anyhow::Result<()> {
        if rollback_turns == 0 {
            return Ok(());
        }
        let Some(ProviderResumeCursor::OpenCode { session_id }) = source.provider_cursor.as_ref()
        else {
            bail!("OpenCode's native session is unavailable");
        };
        let _ = fork_provider_session(ProviderSessionForkRequest::OpenCodeRevert {
            binary: binary.to_owned(),
            cwd: cwd.to_owned(),
            session_id: session_id.clone(),
            turn_count: provider_turn_count,
        })?;
        Ok(())
    }

    fn provider_binary(&self) -> anyhow::Result<PathBuf> {
        ensure_shell_environment();
        crate::model::provider_probe(None)
            .path
            .ok_or_else(|| anyhow!("opencode is not installed on the daemon"))
    }
}

fn validate_message_rewind(source: &AgentSession, turn_count: usize) -> anyhow::Result<()> {
    if !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed) {
        bail!("stop the task before editing a prior message");
    }
    let Some(turn) = source
        .turns
        .iter()
        .find(|turn| turn.turn_count == turn_count)
    else {
        bail!("the selected message is unavailable");
    };
    if !source.messages.iter().any(|message| {
        message.turn_id == Some(turn.id) && message.role == crate::model::MessageRole::User
    }) {
        bail!("the selected user message is unavailable");
    }
    let rollback_turns = source.provider_turns_after(turn_count.saturating_sub(1));
    if rollback_turns > 0 && source.provider_cursor.is_none() {
        bail!("the provider conversation is unavailable");
    }
    Ok(())
}

fn validate_response_fork(source: &AgentSession, turn_count: usize) -> anyhow::Result<()> {
    if !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed) {
        bail!("stop the task before forking a response");
    }
    let cursor = source
        .provider_cursor
        .as_ref()
        .ok_or_else(|| anyhow!("the provider conversation is unavailable"))?;
    if cursor.provider() != source.provider {
        bail!("the provider conversation does not match this task");
    }
    if source
        .turns
        .get(turn_count.saturating_sub(1))
        .is_none_or(|turn| turn.turn_count != turn_count || !turn.provider_turn_started)
    {
        bail!("the selected response cannot be forked");
    }
    Ok(())
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

fn fork_provider_session(
    request: ProviderSessionForkRequest,
) -> anyhow::Result<ProviderSessionFork> {
    let (cursor, message_ids, source_resume_at) = match request {
        ProviderSessionForkRequest::OpenCode {
            binary,
            cwd,
            session_id,
            turn_count,
        } => (
            crate::opencode_session::fork_session_at_turn(&binary, &cwd, &session_id, turn_count)?,
            HashMap::new(),
            None,
        ),
        // The revert keeps the session id, so the fork's cursor field just
        // carries the same session onward.
        ProviderSessionForkRequest::OpenCodeRevert {
            binary,
            cwd,
            session_id,
            turn_count,
        } => {
            let server = crate::opencode_pool::acquire(&binary, &cwd)?;
            crate::opencode_session::revert_session_at_message(&server, &session_id, turn_count)?;
            (
                ProviderResumeCursor::OpenCode {
                    session_id: session_id.clone(),
                },
                HashMap::new(),
                None,
            )
        }
        // Undo and redo never change the conversation identity either: a
        // staged boundary only decides what the next model call sees, and a
        // cleared one restores what staging hid.
        ProviderSessionForkRequest::OpenCodeUndoTurn {
            binary,
            cwd,
            session_id,
        } => {
            let server = crate::opencode_pool::acquire(&binary, &cwd)?;
            crate::opencode_session::undo_last_turn_on_server(&server, &session_id)?;
            (
                ProviderResumeCursor::OpenCode {
                    session_id: session_id.clone(),
                },
                HashMap::new(),
                None,
            )
        }
        ProviderSessionForkRequest::OpenCodeRedoTurn {
            binary,
            cwd,
            session_id,
        } => {
            let server = crate::opencode_pool::acquire(&binary, &cwd)?;
            crate::opencode_session::redo_turn_on_server(&server, &session_id)?;
            (
                ProviderResumeCursor::OpenCode {
                    session_id: session_id.clone(),
                },
                HashMap::new(),
                None,
            )
        }
    };
    Ok(ProviderSessionFork {
        cursor,
        message_ids,
        source_resume_at,
    })
}

fn handle_driver_command(
    driver: &DriverHandle,
    command: Command,
) -> anyhow::Result<ResponsePayload> {
    match command {
        Command::Prompt { prompt, files } => driver.prompt(prompt, files),
        Command::Steer { prompt, files } => driver.steer(prompt, files),
        Command::CompactSession => driver.compact(),
        Command::Cancel => driver.cancel(),
        Command::RefreshBackgroundWork => driver.refresh_background_work(),
        Command::StopBackgroundWork { key, control_id } => {
            driver.stop_background_work(
                serde_json::from_value(key).context("invalid background-work key")?,
                control_id,
            );
        }
        Command::Respond {
            request_id,
            option_id,
        } => driver.respond(request_id, option_id),
        Command::RespondUserInput {
            request_id,
            answers,
        } => driver.respond_user_input(request_id, answers),
        Command::ApplyOptions { options } => {
            return Ok(ResponsePayload::OptionsApplied {
                applied: driver.apply_options(SessionOptions {
                    mode: decode_enum(&options.mode)?,
                    interaction_mode: decode_enum(&options.interaction_mode)?,
                    model: options.model,
                    reasoning_effort: options.reasoning_effort,
                    service_tier: options.service_tier,
                    context_window: options.context_window,
                }),
            });
        }
        Command::Fork { turns_to_remove } => {
            let cursor = Some(serde_json::to_value(driver.fork(turns_to_remove)?)?);
            return Ok(ResponsePayload::Cursor { cursor });
        }
        Command::AttachSession
        | Command::Start { .. }
        | Command::GetSettings
        | Command::UpdateSettings { .. }
        | Command::ProbeProvider { .. }
        | Command::FetchPlanUsage { .. }
        | Command::LoadSkills { .. }
        | Command::SetSkillsEnabled { .. }
        | Command::TrashSkills { .. }
        | Command::LoadTaskState
        | Command::SaveTaskState { .. }
        | Command::RemoveSession
        | Command::RemoveProject { .. }
        | Command::HydrateSession { .. }
        | Command::SearchSessionMessages { .. }
        | Command::LoadComposerDrafts
        | Command::SaveComposerDrafts { .. }
        | Command::ApplyComposerDraftChanges { .. }
        | Command::StoreBlob { .. }
        | Command::ImportAttachment { .. }
        | Command::ImportPathAttachment { .. }
        | Command::ReadBlob { .. }
        | Command::ReadAttachment { .. }
        | Command::SweepBlobs
        | Command::ForkSessionFromResponse { .. }
        | Command::RewindSessionToMessage { .. }
        | Command::ForkProviderSession { .. }
        | Command::ListProviderSessions { .. }
        | Command::FetchNativeTranscript { .. }
        | Command::FetchUsageStats { .. }
        | Command::RenameProviderSession { .. }
        | Command::DeleteProviderSession { .. }
        | Command::FetchIntegrations { .. }
        | Command::AuthorizeProvider { .. }
        | Command::LogoutProvider { .. }
        | Command::ProbeBuiltinProvider { .. }
        | Command::AuthenticateMcpServer { .. }
        | Command::ListMcpServerStatuses { .. }
        | Command::CancelAuthenticateMcpServer { .. }
        | Command::Workspace { .. }
        | Command::OpenTerminal { .. }
        | Command::WriteTerminal { .. }
        | Command::ResizeTerminal { .. }
        | Command::CloseTerminal
        | Command::CloseSession => {
            bail!("daemon received a command in the wrong dispatch path")
        }
    }
    Ok(ResponsePayload::Ack)
}

fn ensure_shell_environment() {
    static REFRESHED: OnceLock<()> = OnceLock::new();
    REFRESHED.get_or_init(|| {
        crate::command_env::refresh_from_default_shell();
    });
}

fn decode_enum<T: DeserializeOwned>(value: &str) -> anyhow::Result<T> {
    serde_json::from_value(Value::String(value.to_owned()))
        .with_context(|| format!("invalid protocol enum value {value:?}"))
}

pub fn encode_enum<T: Serialize>(value: T) -> anyhow::Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("protocol enum did not serialize as a string"))
}

fn event_to_wire(event: DriverEvent) -> anyhow::Result<WireDriverEvent> {
    let (kind, payload) = match event {
        DriverEvent::RuntimeEventCursorAdvanced(_) => {
            bail!("client-only runtime cursors cannot be sent by the daemon")
        }
        DriverEvent::Connected { provider_cursor } => {
            ("connected", serde_json::to_value(provider_cursor)?)
        }
        DriverEvent::AgentPresetSelected(preset) => {
            ("agentPresetSelected", serde_json::to_value(preset)?)
        }
        DriverEvent::AutoTitleUpdated(title) => ("autoTitleUpdated", serde_json::to_value(title)?),
        DriverEvent::AvailableCommands(commands) => {
            ("availableCommands", serde_json::to_value(commands)?)
        }
        DriverEvent::TurnStarted => ("turnStarted", Value::Null),
        DriverEvent::NativeSessionsChanged => ("nativeSessionsChanged", Value::Null),
        DriverEvent::TextStarted { part } => ("textStarted", json!({ "part": part })),
        DriverEvent::TextDelta { part, delta } => {
            ("textDelta", json!({ "part": part, "delta": delta }))
        }
        DriverEvent::TextEnded { part, text } => {
            ("textEnded", json!({ "part": part, "text": text }))
        }
        DriverEvent::ReasoningStarted { part } => ("reasoningStarted", json!({ "part": part })),
        DriverEvent::ReasoningDelta { part, delta } => {
            ("reasoningDelta", json!({ "part": part, "delta": delta }))
        }
        DriverEvent::ReasoningEnded { part, text } => {
            ("reasoningEnded", json!({ "part": part, "text": text }))
        }
        DriverEvent::Activity {
            id,
            kind,
            title,
            detail,
            complete,
        } => (
            "activity",
            json!({
                "id": id,
                "kind": kind,
                "title": title,
                "detail": detail,
                "complete": complete,
            }),
        ),
        DriverEvent::RichActivity(activity) => ("richActivity", serde_json::to_value(activity)?),
        DriverEvent::BackgroundWork(work) => ("backgroundWork", serde_json::to_value(work)?),
        DriverEvent::Permission {
            request_id,
            title,
            detail,
            options,
        } => (
            "permission",
            json!({
                "requestId": request_id,
                "title": title,
                "detail": detail,
                "options": options,
            }),
        ),
        DriverEvent::UserInputRequested {
            request_id,
            questions,
        } => (
            "userInputRequested",
            json!({
                "requestId": request_id,
                "questions": questions,
            }),
        ),
        DriverEvent::SteerAccepted { message } => ("steerAccepted", json!({ "message": message })),
        DriverEvent::SteerRejected { message, reason } => (
            "steerRejected",
            json!({ "message": message, "reason": reason }),
        ),
        DriverEvent::UsageUpdated {
            context_tokens,
            context_window,
            session_total,
            cache_read,
            prompt_tokens,
            latest,
        } => (
            "usageUpdated",
            json!({
                "contextTokens": context_tokens,
                "contextWindow": context_window,
                "sessionTotal": session_total,
                "cacheRead": cache_read,
                "promptTokens": prompt_tokens,
                "latest": latest,
            }),
        ),
        DriverEvent::PlanUsageUpdated(usage) => ("planUsageUpdated", serde_json::to_value(usage)?),
        DriverEvent::TurnStatsUpdated(stats) => ("turnStatsUpdated", serde_json::to_value(stats)?),
        DriverEvent::CompactionUpdated(state) => {
            ("compactionUpdated", serde_json::to_value(state)?)
        }
        DriverEvent::ProviderBusy => ("providerBusy", Value::Null),
        DriverEvent::ProviderRetry {
            attempt,
            message,
            action,
            next_at_ms,
        } => (
            "providerRetry",
            json!({
                "attempt": attempt,
                "message": message,
                "action": action,
                "nextAtMs": next_at_ms,
            }),
        ),
        DriverEvent::TurnFinished { success, summary } => (
            "turnFinished",
            json!({ "success": success, "summary": summary }),
        ),
        DriverEvent::Error(error) => ("error", Value::String(error)),
        DriverEvent::ProcessExited => ("processExited", Value::Null),
    };
    Ok(WireDriverEvent::new(kind, payload))
}

/// The reasoning fragment identity a wire payload carries, if any. An absent
/// key (a pre-keying peer) decodes to an empty string, which the app maps to
/// its phase-based fallback instead of part-bound reasoning.
fn reasoning_part_from_wire(payload: &Value) -> String {
    payload
        .get("part")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

pub fn event_from_wire(event: WireDriverEvent) -> anyhow::Result<DriverEvent> {
    let payload = event.payload;
    Ok(match event.kind.as_str() {
        "connected" => DriverEvent::Connected {
            provider_cursor: serde_json::from_value(payload)?,
        },
        "agentPresetSelected" => DriverEvent::AgentPresetSelected(serde_json::from_value(payload)?),
        "autoTitleUpdated" => DriverEvent::AutoTitleUpdated(serde_json::from_value(payload)?),
        "availableCommands" => DriverEvent::AvailableCommands(serde_json::from_value(payload)?),
        "turnStarted" => DriverEvent::TurnStarted,
        "nativeSessionsChanged" => DriverEvent::NativeSessionsChanged,
        "textStarted" => DriverEvent::TextStarted {
            part: reasoning_part_from_wire(&payload),
        },
        // A bare string payload is a pre-keying peer: the delta keeps the
        // phase-based fallback path on an empty part key.
        "textDelta" => {
            let (part, delta) = match payload {
                Value::String(delta) => (String::new(), delta),
                payload => (
                    reasoning_part_from_wire(&payload),
                    payload
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ),
            };
            DriverEvent::TextDelta { part, delta }
        }
        "textEnded" => DriverEvent::TextEnded {
            part: reasoning_part_from_wire(&payload),
            text: payload
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        // A bare string payload is a pre-keying peer: the delta keeps the
        // phase-based fallback path on an empty part key.
        "reasoningStarted" => DriverEvent::ReasoningStarted {
            part: reasoning_part_from_wire(&payload),
        },
        "reasoningDelta" => {
            let (part, delta) = match payload {
                Value::String(delta) => (String::new(), delta),
                payload => (
                    reasoning_part_from_wire(&payload),
                    payload
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ),
            };
            DriverEvent::ReasoningDelta { part, delta }
        }
        "reasoningEnded" => DriverEvent::ReasoningEnded {
            part: reasoning_part_from_wire(&payload),
            text: payload
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        "activity" => {
            let activity: ActivityWire = serde_json::from_value(payload)?;
            DriverEvent::Activity {
                id: activity.id,
                kind: activity.kind,
                title: activity.title,
                detail: activity.detail,
                complete: activity.complete,
            }
        }
        "richActivity" => DriverEvent::RichActivity(serde_json::from_value(payload)?),
        "backgroundWork" => DriverEvent::BackgroundWork(serde_json::from_value(payload)?),
        "permission" => {
            let permission: PermissionWire = serde_json::from_value(payload)?;
            DriverEvent::Permission {
                request_id: permission.request_id,
                title: permission.title,
                detail: permission.detail,
                options: permission.options,
            }
        }
        "userInputRequested" => {
            let request: UserInputWire = serde_json::from_value(payload)?;
            DriverEvent::UserInputRequested {
                request_id: request.request_id,
                questions: request.questions,
            }
        }
        "steerAccepted" => {
            let steer: AcceptedSteerWire = serde_json::from_value(payload)?;
            DriverEvent::SteerAccepted {
                message: steer.message,
            }
        }
        "steerRejected" => {
            let steer: RejectedSteerWire = serde_json::from_value(payload)?;
            DriverEvent::SteerRejected {
                message: steer.message,
                reason: steer.reason,
            }
        }
        "usageUpdated" => {
            let usage: UsageWire = serde_json::from_value(payload)?;
            DriverEvent::UsageUpdated {
                context_tokens: usage.context_tokens,
                context_window: usage.context_window,
                session_total: usage.session_total,
                cache_read: usage.cache_read,
                prompt_tokens: usage.prompt_tokens,
                latest: usage.latest,
            }
        }
        "planUsageUpdated" => DriverEvent::PlanUsageUpdated(serde_json::from_value(payload)?),
        "turnStatsUpdated" => DriverEvent::TurnStatsUpdated(serde_json::from_value(payload)?),
        "compactionUpdated" => DriverEvent::CompactionUpdated(serde_json::from_value(payload)?),
        "providerBusy" => DriverEvent::ProviderBusy,
        "providerRetry" => {
            let retry: ProviderRetryWire = serde_json::from_value(payload)?;
            DriverEvent::ProviderRetry {
                attempt: retry.attempt,
                message: retry.message,
                action: retry.action,
                next_at_ms: retry.next_at_ms,
            }
        }
        "turnFinished" => {
            let finished: TurnFinishedWire = serde_json::from_value(payload)?;
            DriverEvent::TurnFinished {
                success: finished.success,
                summary: finished.summary,
            }
        }
        "error" => DriverEvent::Error(serde_json::from_value(payload)?),
        "processExited" => DriverEvent::ProcessExited,
        kind => bail!("daemon sent an unsupported driver event {kind:?}"),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityWire {
    id: Option<String>,
    kind: ActivityKind,
    title: String,
    detail: Option<String>,
    complete: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PermissionWire {
    request_id: String,
    title: String,
    detail: String,
    options: Vec<PermissionOption>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInputWire {
    request_id: String,
    questions: Vec<crate::model::UserInputQuestion>,
}

#[derive(Deserialize)]
struct AcceptedSteerWire {
    message: String,
}

#[derive(Deserialize)]
struct RejectedSteerWire {
    message: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageWire {
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    #[serde(default)]
    session_total: Option<u64>,
    #[serde(default)]
    cache_read: Option<u64>,
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    latest: Option<fintwind_protocol::model::LatestCallUsage>,
}

#[derive(Deserialize)]
struct TurnFinishedWire {
    success: bool,
    summary: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderRetryWire {
    attempt: u32,
    message: String,
    action: Option<ProviderRetryAction>,
    next_at_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_runtime_projection_keeps_newer_transcript_cursor() {
        let runtime_id = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        let mut existing = AgentSession::new(Uuid::new_v4());
        existing.status = SessionStatus::Working;
        existing.runtime_event_cursor = Some(crate::model::RuntimeEventCursor {
            runtime_id,
            epoch,
            sequence: 10,
        });
        existing.push_message(crate::model::MessageRole::Assistant, "complete so far");

        let mut stale = existing.clone();
        stale.title = "Renamed elsewhere".into();
        stale.messages.clear();
        stale.runtime_event_cursor = Some(crate::model::RuntimeEventCursor {
            runtime_id,
            epoch,
            sequence: 7,
        });

        assert!(session_projection_precedes(
            &existing,
            &stale,
            Some(runtime_id)
        ));
        merge_stale_session_metadata(&mut existing, stale);
        assert_eq!(existing.title, "Renamed elsewhere");
        assert_eq!(existing.messages.len(), 1);
        assert_eq!(existing.runtime_event_cursor.unwrap().sequence, 10);
    }

    #[test]
    fn client_projection_cannot_replace_a_daemon_checkpoint() {
        let mut existing = AgentSession::new(Uuid::new_v4());
        existing.begin_turn("change it");
        existing.finish_active_turn(crate::model::TurnStatus::Completed);
        let checkpoint = Checkpoint {
            turn_count: 1,
            git_ref: "refs/fintwind/canonical".into(),
            status: CheckpointStatus::Ready,
            files: Vec::new(),
            additions: 0,
            deletions: 0,
            created_at: 1,
        };
        existing.turns[0].checkpoint = Some(checkpoint.clone());

        let mut incoming = existing.clone();
        incoming.turns[0].checkpoint = Some(Checkpoint {
            git_ref: "refs/fintwind/stale-client".into(),
            ..checkpoint.clone()
        });
        preserve_daemon_checkpoints(&existing, &mut incoming);

        assert_eq!(incoming.turns[0].checkpoint.as_ref(), Some(&checkpoint));
    }

    #[test]
    fn response_fork_titles_follow_one_numbered_sequence() {
        assert_eq!(
            next_response_fork_title("Fix the bug", ["Fix the bug"]),
            "Fix the bug (2)"
        );
        assert_eq!(
            next_response_fork_title(
                "Fix the bug (2)",
                ["Fix the bug", "Fix the bug (2)", "Fix the bug (4)"]
            ),
            "Fix the bug (5)"
        );
        assert_eq!(
            next_response_fork_title("Plan (2026)", ["Plan (2026)"]),
            "Plan (2026) (2)"
        );
    }

    #[test]
    fn message_rewind_requires_a_settled_user_turn_and_provider_cursor() {
        let mut session = AgentSession::new(Uuid::new_v4());
        session.begin_turn("change it");
        session.mark_active_turn_provider_started();
        session.provider_cursor = Some(ProviderResumeCursor::OpenCode {
            session_id: "session".into(),
        });
        session.finish_active_turn(crate::model::TurnStatus::Completed);

        assert!(validate_message_rewind(&session, 1).is_ok());

        let mut busy = session.clone();
        busy.status = SessionStatus::Working;
        assert!(validate_message_rewind(&busy, 1).is_err());

        let mut missing_cursor = session.clone();
        missing_cursor.provider_cursor = None;
        assert!(validate_message_rewind(&missing_cursor, 1).is_err());

        let mut missing_message = session;
        missing_message.messages.clear();
        assert!(validate_message_rewind(&missing_message, 1).is_err());
    }

    #[test]
    fn wire_event_round_trip_preserves_ordered_delta_payload() {
        let wire = event_to_wire(DriverEvent::TextDelta {
            part: "text:msg_1:0".into(),
            delta: "hello".into(),
        })
        .unwrap();
        assert_eq!(wire.kind, "textDelta");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::TextDelta { part, delta } if part == "text:msg_1:0" && delta == "hello"
        ));

        let legacy = WireDriverEvent::new("textDelta", Value::String("hello".into()));
        assert!(matches!(
            event_from_wire(legacy).unwrap(),
            DriverEvent::TextDelta { part, delta } if part.is_empty() && delta == "hello"
        ));
    }

    #[test]
    fn wire_event_round_trip_preserves_keyed_reasoning_fragments() {
        let wire = event_to_wire(DriverEvent::ReasoningStarted {
            part: "reasoning:msg_1:0".into(),
        })
        .unwrap();
        assert_eq!(wire.kind, "reasoningStarted");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ReasoningStarted { part } if part == "reasoning:msg_1:0"
        ));

        let wire = event_to_wire(DriverEvent::ReasoningDelta {
            part: "reasoning:msg_1:0".into(),
            delta: "thinking".into(),
        })
        .unwrap();
        assert_eq!(wire.kind, "reasoningDelta");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ReasoningDelta { part, delta }
                if part == "reasoning:msg_1:0" && delta == "thinking"
        ));

        let wire = event_to_wire(DriverEvent::ReasoningEnded {
            part: "reasoning:msg_1:0".into(),
            text: Some("thinking!".into()),
        })
        .unwrap();
        assert_eq!(wire.kind, "reasoningEnded");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ReasoningEnded { part, text }
                if part == "reasoning:msg_1:0" && text.as_deref() == Some("thinking!")
        ));
    }

    #[test]
    fn a_pre_keying_reasoning_delta_degrades_to_the_unkeyed_path() {
        // An older daemon serializes reasoning deltas as bare strings; the
        // decoded event carries an empty part key, which the app maps to its
        // phase-based fallback instead of failing the stream.
        let wire = WireDriverEvent::new(
            "reasoningDelta".to_owned(),
            serde_json::Value::String("thinking".to_owned()),
        );
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ReasoningDelta { part, delta }
                if part.is_empty() && delta == "thinking"
        ));
    }

    #[test]
    fn wire_event_round_trip_preserves_provider_status_signals() {
        let wire = event_to_wire(DriverEvent::ProviderBusy).unwrap();
        assert_eq!(wire.kind, "providerBusy");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ProviderBusy
        ));

        let action = ProviderRetryAction {
            reason: "free_tier_limit".into(),
            provider: "zen".into(),
            title: "Free limit reached".into(),
            message: "Subscribe to OpenCode Go".into(),
            label: "subscribe".into(),
            link: Some("https://opencode.ai/go".into()),
        };
        let retry = DriverEvent::ProviderRetry {
            attempt: 3,
            message: "429 Too Many Requests".into(),
            action: Some(action),
            next_at_ms: Some(1_700_000_008_000),
        };
        let wire = event_to_wire(retry).unwrap();
        assert_eq!(wire.kind, "providerRetry");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ProviderRetry {
                attempt: 3,
                message,
                action: Some(action),
                next_at_ms: Some(1_700_000_008_000),
            } if message == "429 Too Many Requests"
                && action.reason == "free_tier_limit"
                && action.link.as_deref() == Some("https://opencode.ai/go")
        ));
    }

    // The daemon keeps its own wire copy; this pins the compaction encoding
    // here so the duplicate cannot drift from the protocol crate's.
    #[test]
    fn wire_event_round_trip_preserves_compaction_snapshots() {
        let state = fintwind_protocol::model::CompactionState {
            status: fintwind_protocol::model::CompactionStatus::Failed,
            reason: Some("manual".into()),
            model: None,
            error: Some("provider rejected the summary".into()),
            summary: None,
        };
        let wire = event_to_wire(DriverEvent::CompactionUpdated(state.clone())).unwrap();
        assert_eq!(wire.kind, "compactionUpdated");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::CompactionUpdated(round) if round == state
        ));
    }

    fn catalog_backend() -> (FintwindBackend, PathBuf) {
        let root = std::env::temp_dir().join(format!("fintwind-remove-project-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let backend = FintwindBackend::new(
            crate::settings::DaemonSettingsStore::open(root.join("settings.json")).unwrap(),
            StateStore::daemon(root.join("app.db")),
        )
        .unwrap();
        (backend, root)
    }

    fn catalog_request(command: Command) -> Request {
        Request {
            request_id: Uuid::nil(),
            session_id: Uuid::nil(),
            runtime_id: Uuid::nil(),
            command,
        }
    }

    #[test]
    fn removing_a_project_hides_it_from_the_catalog_without_resurrecting_on_save() {
        let (backend, root) = catalog_backend();
        let project = Project::from_path(root.join("repo"));
        let mut session = AgentSession::new(project.id);
        session.begin_turn("keep the files");
        backend
            .handle(
                catalog_request(Command::SaveTaskState {
                    projects: vec![project.clone()],
                    live_session_ids: vec![session.id],
                    sessions: vec![session.clone()],
                }),
                EventSink::discarded(),
            )
            .unwrap();

        backend
            .handle(
                catalog_request(Command::RemoveProject {
                    project_id: project.id,
                }),
                EventSink::discarded(),
            )
            .unwrap();

        let ResponsePayload::TaskState {
            projects, sessions, ..
        } = backend
            .handle(
                catalog_request(Command::LoadTaskState),
                EventSink::discarded(),
            )
            .unwrap()
        else {
            panic!("expected task state");
        };
        assert!(projects.is_empty(), "the project leaves the app catalog");
        assert!(sessions.is_empty(), "its local session rows leave with it");

        backend
            .handle(
                catalog_request(Command::SaveTaskState {
                    projects: vec![project],
                    live_session_ids: vec![session.id],
                    sessions: vec![session],
                }),
                EventSink::discarded(),
            )
            .unwrap();
        let ResponsePayload::TaskState {
            projects, sessions, ..
        } = backend
            .handle(
                catalog_request(Command::LoadTaskState),
                EventSink::discarded(),
            )
            .unwrap()
        else {
            panic!("expected task state");
        };
        assert!(
            projects.is_empty(),
            "a stale save cannot restore the project"
        );
        assert!(
            sessions.is_empty(),
            "a stale save cannot restore its sessions"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

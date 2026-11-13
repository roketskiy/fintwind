//! General workspace settings through the real daemon RPC and SQLite store.
//! Uses an isolated directory and loopback threads; launches no child processes.

use anyhow::{Context, Result, ensure};
use fintwind_client::{DaemonClient, WorkspaceClient, persistence::PersistedState as ClientState};
use fintwind_core::{DaemonSettingsStore, daemon::FintwindBackend, persistence::StateStore};
use fintwind_protocol::{
    Command, ResponsePayload, WorkspaceOperation, WorkspaceResult,
    model::{AgentSession, Message, MessageRole, Project},
    settings::{DaemonSettings, GENERAL_WORKSPACE_PATH_KEY},
};
use serde_json::json;
use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};
use uuid::Uuid;

struct Server {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
    client: DaemonClient,
    address: String,
    token: String,
    backend: std::sync::Weak<FintwindBackend>,
}

impl Server {
    fn start(directory: &Path) -> Result<Self> {
        let backend = Arc::new(FintwindBackend::new(
            DaemonSettingsStore::open(directory.join("daemon.json"))?,
            StateStore::daemon(directory.join("app.db")),
        )?);
        let weak_backend = Arc::downgrade(&backend);
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?.to_string();
        let token = Uuid::new_v4().to_string();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server_shutdown = shutdown.clone();
        let server_token = token.clone();
        let thread = std::thread::spawn(move || {
            fintwind_core::serve(
                listener,
                server_token,
                backend,
                server_shutdown,
                Default::default(),
            )
        });
        let client = match DaemonClient::connect(&address, token.clone()) {
            Ok(client) => client,
            Err(error) => {
                shutdown.store(true, Ordering::Release);
                match thread.join() {
                    Ok(Err(startup)) => return Err(startup.context("black-box server startup")),
                    _ => return Err(error),
                }
            }
        };
        Ok(Self {
            shutdown,
            thread: Some(thread),
            client,
            address,
            token,
            backend: weak_backend,
        })
    }

    fn request(&self, command: Command) -> Result<ResponsePayload> {
        self.client.request(Uuid::nil(), Uuid::nil(), command)
    }

    fn settings(&self) -> Result<DaemonSettings> {
        match self.request(Command::GetSettings)? {
            ResponsePayload::Settings { settings } => Ok(settings),
            other => anyhow::bail!("unexpected settings response: {other:?}"),
        }
    }

    fn root(&self) -> Result<PathBuf> {
        match self.request(Command::LoadTaskState)? {
            ResponsePayload::TaskState {
                projectless_root: Some(root),
                ..
            } => Ok(root),
            other => anyhow::bail!("unexpected task-state response: {other:?}"),
        }
    }

    fn create(&self) -> Result<PathBuf> {
        match WorkspaceClient::new(self.client.clone()).request(
            WorkspaceOperation::CreateProjectlessWorkspace {
                prompt: Some("new workspace".into()),
            },
        )? {
            WorkspaceResult::ProjectlessWorkspace { cwd } => Ok(cwd),
            other => anyhow::bail!("unexpected workspace response: {other:?}"),
        }
    }

    fn desktop_state(&self, directory: &Path) -> Result<ClientState> {
        let daemon = fintwind_client::DaemonSupervisor::connect(&self.address, self.token.clone())?;
        let store = fintwind_client::persistence::StateStore::remote_in(daemon.clone(), directory);
        let state = store.load_or_fresh(directory.to_owned());
        drop(store);
        daemon.shutdown();
        daemon.client().disconnect();
        Ok(state)
    }

    fn stop(mut self) -> Result<()> {
        self.client.disconnect();
        self.shutdown.store(true, Ordering::Release);
        self.thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow::anyhow!("server panicked"))??;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while self.backend.strong_count() > 0 {
            ensure!(
                std::time::Instant::now() < deadline,
                "owned server connections did not close"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.client.disconnect();
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn main() -> Result<()> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("pass an artifact directory")?,
    )
    .join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    let data = output.join("data");
    std::fs::create_dir_all(&data)?;
    let legacy_root = output.join("home/.fintwind");
    fintwind_protocol::projectless::set_legacy_root(Some(legacy_root.clone()));
    let legacy_cwd = legacy_root.join("projects/2026-08-08/old-chat");
    std::fs::create_dir_all(&legacy_cwd)?;
    std::fs::write(legacy_cwd.join("keep.txt"), "existing work")?;
    let legacy = Project::from_path(legacy_cwd.clone());
    let mut stored = fintwind_core::persistence::PersistedState::empty();
    stored.projects.push(legacy.clone());
    let mut legacy_session = AgentSession::new(legacy.id);
    legacy_session
        .messages
        .push(Message::new(MessageRole::User, "existing history"));
    stored.sessions.push(legacy_session);
    StateStore::daemon(data.join("app.db")).save(&mut stored)?;

    let mut checks = Vec::new();
    let server = Server::start(&data)?;
    let default_root = data.join("projects");
    ensure!(server.root()? == default_root && default_root.is_dir());
    let first = server.create()?;
    let second = server.create()?;
    ensure!(first == default_root && second == default_root && first.is_dir());
    ensure!(std::fs::read_dir(&default_root)?.next().is_none());
    checks.push("startup creates one fixed directory; repeated sessions reuse it without date or task subdirectories");

    let mut desktop = server.desktop_state(&output.join("desktop"))?;
    let selected = desktop
        .selected_session
        .context("startup has no draft session")?;
    let session = desktop
        .sessions
        .iter()
        .find(|session| session.id == selected)
        .context("startup draft is missing")?;
    let project = desktop
        .projects
        .iter()
        .find(|project| project.id == session.project_id)
        .context("startup workspace is missing")?;
    ensure!(project.path == default_root && project.is_projectless() && !session.has_started());
    let project_id = project.id;
    ensure!(desktop.ensure_general_workspace_project(default_root.clone()) == project_id);
    let first_draft = desktop.new_session(project_id);
    let second_draft = desktop.new_session(project_id);
    ensure!(first_draft.id != second_draft.id && first_draft.project_id == second_draft.project_id);
    match server.request(Command::LoadTaskState)? {
        ResponsePayload::TaskState { default_cwd, .. } => ensure!(default_cwd == default_root),
        other => anyhow::bail!("unexpected task-state response: {other:?}"),
    }
    checks.push("desktop cold start without a selected project has a draft in the prepared shared directory; sessions share its project identity");

    let custom = output.join("通用 工作区");
    let mut client = ClientState::empty();
    client.apply_daemon_settings(server.settings()?);
    client.set_general_workspace_path(Some(custom.to_str().context("UTF-8 path")?.into()));
    let mut settings = client.daemon_settings();
    settings.extra.insert("future_setting".into(), json!(42));
    ensure!(matches!(
        server.request(Command::UpdateSettings { settings })?,
        ResponsePayload::Ack
    ));
    ensure!(server.root()? == custom && custom.is_dir());
    let custom_cwd = server.create()?;
    ensure!(custom_cwd == custom && custom_cwd.is_dir());
    std::fs::write(custom_cwd.join("keep.txt"), "custom work")?;
    let mut project = Project::from_path(custom_cwd.clone());
    project.name = Project::PROJECTLESS_NAME.into();
    let mut custom_session = AgentSession::new(project.id);
    custom_session
        .messages
        .push(Message::new(MessageRole::User, "custom history"));
    server.request(Command::SaveTaskState {
        projects: vec![project.clone()],
        live_session_ids: vec![],
        sessions: vec![custom_session],
    })?;
    checks.push("folder with Chinese and spaces is created and used by the real workspace RPC");

    let before = std::fs::read(data.join("daemon.json"))?;
    let file = output.join("a-file");
    std::fs::write(&file, "not a directory")?;
    for invalid in [
        PathBuf::from("relative-workspace"),
        file.clone(),
        file.join("child"),
    ] {
        let mut settings = server.settings()?;
        settings
            .extra
            .insert(GENERAL_WORKSPACE_PATH_KEY.into(), json!(invalid));
        ensure!(
            server
                .request(Command::UpdateSettings { settings })
                .is_err()
        );
        ensure!(std::fs::read(data.join("daemon.json"))? == before);
        ensure!(server.root()? == custom);
    }
    ensure!(!Project::from_path(output.join("2026-10-26/ordinary-project")).is_projectless());
    checks.push("invalid paths preserve saved and active settings; custom-root siblings remain ordinary projects");
    server.stop()?;

    let server = Server::start(&data)?;
    client.apply_daemon_settings(server.settings()?);
    ensure!(client.general_workspace_path() == custom.to_str());
    ensure!(server.root()? == custom && server.create()?.starts_with(&custom));
    ensure!(server.settings()?.extra["future_setting"] == 42);
    checks.push("daemon restart restores the chosen root and preserves unrelated settings");

    client.set_general_workspace_path(None);
    server.request(Command::UpdateSettings {
        settings: client.daemon_settings(),
    })?;
    ensure!(server.root()? == default_root && server.create()?.starts_with(&default_root));
    match server.request(Command::LoadTaskState)? {
        ResponsePayload::TaskState { projects, .. } => {
            let old = projects
                .iter()
                .find(|row| row.id == legacy.id)
                .context("legacy project missing")?;
            let custom = projects
                .iter()
                .find(|row| row.id == project.id)
                .context("custom project missing")?;
            ensure!(old.path == legacy_cwd && old.is_projectless());
            ensure!(custom.path == custom_cwd && custom.is_projectless());
        }
        other => anyhow::bail!("unexpected task-state response: {other:?}"),
    }
    ensure!(std::fs::read_to_string(legacy_cwd.join("keep.txt"))? == "existing work");
    ensure!(std::fs::read_to_string(custom_cwd.join("keep.txt"))? == "custom work");
    checks.push("reset affects new sessions while old default and custom workspaces retain identity, paths and files");
    server.stop()?;

    let server = Server::start(&data)?;
    ensure!(server.settings()?.general_workspace_path().is_none());
    ensure!(server.root()? == default_root);
    let unavailable = output.join("unavailable");
    let mut settings = server.settings()?;
    settings
        .extra
        .insert(GENERAL_WORKSPACE_PATH_KEY.into(), json!(unavailable));
    server.request(Command::UpdateSettings { settings })?;
    server.stop()?;
    checks.push("reset persists across daemon restart");

    std::fs::remove_dir(&unavailable)?;
    let server = Server::start(&data)?;
    ensure!(unavailable.is_dir());
    ensure!(server.root()? == unavailable && server.create()? == unavailable);
    server.stop()?;
    checks.push("a missing custom folder is recreated at startup before any session is created");

    std::fs::remove_dir(&unavailable)?;
    std::fs::write(&unavailable, "directory is no longer available")?;
    let server = Server::start(&data)?;
    ensure!(server.root()? == unavailable);
    ensure!(server.create().is_err());
    let mut settings = server.settings()?;
    settings.extra.insert("future_setting".into(), json!(43));
    server.request(Command::UpdateSettings {
        settings: settings.clone(),
    })?;
    settings.extra.remove(GENERAL_WORKSPACE_PATH_KEY);
    server.request(Command::UpdateSettings { settings })?;
    ensure!(server.root()? == default_root && server.create()?.is_dir());
    server.stop()?;
    checks.push("an unavailable custom folder does not block startup, unrelated settings or restoring the default");

    std::fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&json!({
            "passed": true, "checks": checks, "processes_launched": 0,
        }))?,
    )?;
    println!("{} checks passed; {}", checks.len(), output.display());
    Ok(())
}

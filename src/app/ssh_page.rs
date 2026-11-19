use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    overlays::Dialog,
    primitives::{Icon, IconName},
    theme::ControlSize,
};
use gpui::ElementId;
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, load_secret_key};
use serde::{Deserialize, Serialize};

use super::providers_page::labeled_field;
use super::*;
use crate::ui::ActivationExt;
use crate::ui::loading::{LoadingDelay, LoadingPhase};

use fintwind_client::ssh as tunnel;
mod deploy;
mod mcp;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Profile {
    id: Uuid,
    name: String,
    host: String,
    port: u16,
    user: String,
    key: PathBuf,
    fingerprint: Option<String>,
    #[serde(default)]
    daemon_port: Option<u16>,
    #[serde(default)]
    token_file: String,
}

#[derive(PartialEq, Serialize, Deserialize)]
struct CatalogHost {
    host: String,
    port: u16,
    user: String,
    fingerprint: Option<String>,
}

type LoadedSshWorkspace = (StateStore, LoadedWorkspace);

impl From<&Profile> for CatalogHost {
    fn from(profile: &Profile) -> Self {
        Self {
            host: profile.host.clone(),
            port: profile.port,
            user: profile.user.clone(),
            fingerprint: profile.fingerprint.clone(),
        }
    }
}

struct Draft {
    id: Uuid,
    fields: [Entity<ComposerInput>; 7],
    manual_backend: bool,
}

enum Probe {
    Snapshot(Vec<String>, Option<Deployment>),
    BackendUnavailable(Vec<String>, String, Option<Deployment>),
    Ready(Vec<String>, tunnel::Connection),
}

#[derive(Clone, Copy)]
enum Deployment {
    Upload,
    Start,
}

#[derive(Default)]
pub(super) struct SshPage {
    mcp: mcp::RemoteMcp,
    pub(super) provider_workspace: Option<(PathBuf, PathBuf)>,
    pub(super) providers: Vec<fintwind_client::custom_providers::CustomProvider>,
    pub(super) providers_loaded: bool,
    pub(super) mcp_loaded: bool,
    pub(super) provider_integrations: Vec<fintwind_protocol::provider_session::IntegrationSummary>,
    general_edit: Option<(Uuid, tunnel::GeneralSetting, Entity<ComposerInput>)>,
    general_pending: bool,
    general_loading: RefCell<Option<LoadingDelay<(Uuid, bool)>>>,
    general_error: Option<(Uuid, bool, String)>,
    #[cfg(test)]
    test_location_picker: bool,
    project_connection: Option<tunnel::Connection>,
    project_machine: Option<String>,
    project_profile: Option<Profile>,
    loaded: bool,
    busy: bool,
    connection_action: Option<(Uuid, SshConnectionAction)>,
    connection_loading: RefCell<Option<LoadingDelay<(Uuid, SshConnectionAction)>>>,
    dialog_loading: RefCell<Option<LoadingDelay<(Uuid, u8)>>>,
    profiles: Vec<Profile>,
    selected: Option<Uuid>,
    draft: Option<Draft>,
    last_read: HashMap<Uuid, Vec<String>>,
    search: Option<Entity<ComposerInput>>,
    row_focus: HashMap<Uuid, FocusHandle>,
    backend_errors: HashMap<Uuid, String>,
    connection_errors: HashMap<Uuid, String>,
    deployments: HashMap<Uuid, Deployment>,
    opencode_installed: HashMap<Uuid, bool>,
    connections: HashMap<Uuid, Arc<tunnel::Connection>>,
    catalogs: HashMap<Uuid, RemoteTaskStateSnapshot>,
    catalog_tasks: HashMap<Uuid, gpui::Task<()>>,
    workspaces: HashMap<Uuid, Entity<Fintwind>>,
    loaded_workspaces: HashMap<Uuid, LoadedSshWorkspace>,
    active_workspace: Option<Uuid>,
    restoring_workspace: Option<Uuid>,
    workspace_owner: Option<gpui::WeakEntity<Fintwind>>,
    ready: HashSet<Uuid>,
    watching: bool,
    machine: Option<Uuid>,
    remote_settings: HashMap<Uuid, fintwind_protocol::settings::DaemonSettings>,
    error: Option<String>,
    remove: Option<Uuid>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SshConnectionAction {
    Connect,
    Deploy,
}

impl SshPage {
    fn connection_status(&self, id: Uuid, theme: Theme) -> (String, Hsla) {
        if self.ready.contains(&id) {
            (tr!("ssh.ready"), theme.success_text)
        } else if self.connection_errors.contains_key(&id) {
            (tr!("ssh.connect_failed"), theme.danger_text)
        } else if self.backend_errors.contains_key(&id) {
            (tr!("ssh.backend_unavailable"), theme.warning_text)
        } else if self.connections.contains_key(&id) {
            (tr!("ssh.offline"), theme.warning_text)
        } else if self.deployments.contains_key(&id) {
            (tr!("ssh.connected"), theme.success_text)
        } else {
            (tr!("ssh.not_connected"), theme.text_secondary)
        }
    }

    pub(super) fn with_search(search: Entity<ComposerInput>, cx: &mut Context<Fintwind>) -> Self {
        cx.subscribe(&search, |_: &mut Fintwind, _, event: &ComposerEvent, cx| {
            if matches!(event, ComposerEvent::Edited) {
                cx.notify();
            }
        })
        .detach();
        Self {
            search: Some(search),
            ..Self::default()
        }
    }
}

fn profiles_path() -> Result<PathBuf> {
    if let Some(root) = fintwind_client::persistence::debug_data_directory() {
        return Ok(root.join("ssh-profiles.json"));
    }
    let root = dirs::data_local_dir().context("local data directory unavailable")?;
    Ok(root
        .join(if cfg!(debug_assertions) {
            "Fintwind Debug"
        } else {
            "Fintwind"
        })
        .join("ssh-profiles.json"))
}

fn read_profiles() -> Result<Vec<Profile>> {
    match std::fs::read(profiles_path()?) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn write_profiles(profiles: &[Profile]) -> Result<()> {
    write_ssh_state(profiles_path()?, &serde_json::to_vec_pretty(profiles)?)
}

fn catalog_path(id: Uuid) -> Result<PathBuf> {
    Ok(profiles_path()?.with_file_name("ssh-catalogs").join(format!("{id}.json")))
}

fn load_ssh_workspace(
    host: Uuid,
    daemon: &fintwind_client::DaemonSupervisor,
) -> Result<LoadedSshWorkspace> {
    let directory = profiles_path()?.with_file_name("ssh-workspaces").join(host.to_string());
    let store = StateStore::ssh_workspace(daemon.clone(), &directory);
    let loaded = Fintwind::load_workspace(daemon, &store);
    Ok((store, loaded))
}

pub(super) fn save_ssh_navigation(host: Option<Uuid>) -> Result<()> {
    write_ssh_state(
        profiles_path()?.with_file_name("ssh-navigation.json"),
        &serde_json::to_vec(&host)?,
    )
}

fn read_ssh_navigation() -> Option<Uuid> {
    let path = profiles_path().ok()?.with_file_name("ssh-navigation.json");
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok().flatten()
}

fn write_ssh_state(path: PathBuf, content: &[u8]) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("missing profile directory")?)?;
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    std::fs::write(&temporary, content)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(temporary);
        return Err(error.into());
    }
    Ok(())
}

struct HostKey {
    expected: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
}

impl client::Handler for HostKey {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        let matches = self
            .expected
            .as_ref()
            .is_none_or(|expected| expected == &fingerprint);
        *self.observed.lock().expect("SSH host key mutex") = Some(fingerprint);
        Ok(matches)
    }
}

async fn authenticate(profile: &Profile) -> Result<(client::Handle<HostKey>, String)> {
    let observed = Arc::new(Mutex::new(None));
    let mut session = match client::connect(
        Arc::new(client::Config {
            keepalive_interval: Some(Duration::from_secs(10)),
            keepalive_max: 2,
            ..Default::default()
        }),
        (profile.host.as_str(), profile.port),
        HostKey {
            expected: profile.fingerprint.clone(),
            observed: observed.clone(),
        },
    )
    .await
    {
        Ok(session) => session,
        Err(error) => match (
            profile.fingerprint.as_ref(),
            observed.lock().expect("SSH host key mutex").take(),
        ) {
            (Some(expected), Some(current)) if expected != &current => {
                anyhow::bail!("{}", tr!("ssh.key_changed"))
            }
            _ => return Err(error.context(tr!("ssh.connect_failed"))),
        },
    };
    let key = load_secret_key(&profile.key, None).context(tr!("ssh.key_failed"))?;
    let hash = session.best_supported_rsa_hash().await?.flatten();
    ensure!(
        session
            .authenticate_publickey(
                &profile.user,
                PrivateKeyWithHashAlg::new(Arc::new(key), hash)
            )
            .await?
            .success(),
        "{}",
        tr!("ssh.auth_failed")
    );
    let fingerprint = observed
        .lock()
        .expect("SSH host key mutex")
        .take()
        .context("SSH host key was not recorded")?;
    Ok((session, fingerprint))
}

async fn inspect(profile: &mut Profile, action: Option<SshConnectionAction>) -> Result<Probe> {
    let (session, fingerprint) = authenticate(profile).await?;
    let session = Arc::new(session);
    profile.fingerprint = Some(fingerprint);
    let mut channel = session.channel_open_session().await?;
    channel.exec(true, "export LC_ALL=C; printf '%s\\n' \"$(opencode --version 2>/dev/null | head -n1)\" \"$(uname -sm)\" \"$(getconf _NPROCESSORS_ONLN)\" \"$(awk '/^MemTotal:/ {print $2 \" KiB\"}' /proc/meminfo)\" \"$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | paste -sd, -)\" \"$(df -hP . | awk 'NR==2 {print $4}')\"").await?;
    let mut bytes = Vec::new();
    let mut success = false;
    while let Some(message) = channel.wait().await {
        match message {
            russh::ChannelMsg::Data { data } => {
                ensure!(
                    bytes.len() + data.len() <= 8192,
                    "SSH environment response too large"
                );
                bytes.extend_from_slice(&data);
            }
            russh::ChannelMsg::ExitStatus { exit_status } => success = exit_status == 0,
            _ => {}
        }
    }
    ensure!(success, "{}", tr!("ssh.inspect_failed"));
    let output = String::from_utf8_lossy(&bytes);
    let values: Vec<String> = output
        .lines()
        .take(6)
        .map(|line| line.chars().filter(|c| !c.is_control()).collect::<String>())
        .collect();
    let mut deployment = None;
    let mut endpoint = profile
        .daemon_port
        .map(|port| (port, profile.token_file.clone()));
    if endpoint.is_none()
        && let Some(target) = values
            .get(1)
            .and_then(|system| deploy::LinuxTarget::from_system(system))
    {
        let detected = deploy::detect(&session).await?;
        if let deploy::Detection::Running { port, token_file } = &detected
            && action != Some(SshConnectionAction::Deploy)
        {
            endpoint = Some((*port, token_file.clone()));
        } else if action.is_some() {
            if let deploy::Detection::Running { port, token_file } = &detected {
                let connection = tunnel::Connection::open_with_reconnect(
                    session.clone(),
                    *port,
                    token_file,
                    None::<fn() -> std::future::Ready<Result<client::Handle<HostKey>>>>,
                ).await?;
                let client = connection.client();
                tokio::task::spawn_blocking(move || {
                    require_idle_daemon(&client)
                }).await??;
            }
            let allow_existing_without_asset = !matches!(detected, deploy::Detection::Running { .. });
            match deploy::ensure_current(&session, target, allow_existing_without_asset).await {
                Ok(address) => endpoint = Some(address),
                Err(error) => {
                    return Ok(Probe::BackendUnavailable(
                        values,
                        error.to_string(),
                        Some(Deployment::Upload),
                    ));
                }
            }
        } else {
            match detected {
                deploy::Detection::Missing => deployment = Some(Deployment::Upload),
                deploy::Detection::Stopped => deployment = Some(Deployment::Start),
                deploy::Detection::Running { port, token_file } => {
                    endpoint = Some((port, token_file))
                }
            }
        }
    }
    if let Some((port, token_file)) = endpoint {
        let profile = profile.clone();
        let connection = tunnel::Connection::open_with_reconnect(
            session,
            port,
            &token_file,
            Some(move || {
                let profile = profile.clone();
                async move { authenticate(&profile).await.map(|(session, _)| session) }
            }),
        )
        .await;
        return Ok(match connection {
            Ok(connection) => Probe::Ready(values, connection),
            Err(error) => Probe::BackendUnavailable(values, error.to_string(), deployment),
        });
    }
    Ok(Probe::Snapshot(values, deployment))
}

fn probe_opencode(client: &fintwind_client::DaemonClient) -> Option<(bool, Option<String>)> {
    let nil = Uuid::nil();
    match client
        .request_with_timeout(
            nil,
            nil,
            fintwind_client::Command::ProbeProvider {
                binary_override: None,
                directory: None,
                discover_models: false,
                probe_version: true,
            },
            Duration::from_secs(8),
        )
        .ok()?
    {
        fintwind_client::ResponsePayload::ProviderProbe { probe, version } => {
            Some((probe.installed, version))
        }
        _ => None,
    }
}

fn require_idle_daemon(client: &fintwind_client::DaemonClient) -> Result<()> {
    let snapshot = super::runtime::load_remote_task_state(client)?;
    ensure!(
        !snapshot.sessions.iter().any(|session| session.status.is_busy()),
        "{}", tr!("ssh.update_busy"),
    );
    Ok(())
}

pub(super) fn settings_opencode_workspace(
    daemon: &fintwind_client::DaemonSupervisor,
) -> std::io::Result<(PathBuf, PathBuf)> {
    tunnel::opencode_workspace(&daemon.client()).map_err(std::io::Error::other)
}

pub(crate) struct ProjectLaunch {
    profile: Profile,
    pub machine: String,
    project: Project,
    pub state_dir: PathBuf,
    pub connection: tunnel::Connection,
}

pub(crate) fn remote_project_directory(
    client: fintwind_client::DaemonClient,
    path: String,
) -> Result<PathBuf> {
    ensure!(path.starts_with('/'), "{}", tr!("project.remote_path"));
    match client.request_with_timeout(
        Uuid::nil(),
        Uuid::nil(),
        fintwind_client::Command::Workspace {
            operation: fintwind_client::WorkspaceOperation::BrowseDirectory {
                path: Some(path.into()),
            },
        },
        Duration::from_secs(10),
    )? {
        fintwind_client::ResponsePayload::Workspace {
            result: fintwind_client::WorkspaceResult::Directory { path, .. },
        } => Ok(path),
        _ => anyhow::bail!("invalid remote directory response"),
    }
}

pub(crate) fn project_launch() -> Result<Option<ProjectLaunch>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--ssh-project")) {
        return Ok(None);
    }
    let id: Uuid = args
        .next()
        .context("missing SSH profile")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("invalid SSH profile"))?
        .parse()?;
    let path = args
        .next()
        .context("missing remote directory")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("invalid remote directory"))?;
    let name = args
        .next()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let profile = read_profiles()?
        .into_iter()
        .find(|profile| profile.id == id)
        .context("SSH profile no longer exists")?;
    let state_dir = profiles_path()?
        .with_file_name("ssh-workspaces")
        .join(id.to_string());
    prepare_project_launch(profile, path, name, state_dir).map(Some)
}

fn prepare_project_launch(
    mut profile: Profile,
    path: String,
    name: String,
    state_dir: PathBuf,
) -> Result<ProjectLaunch> {
    let machine = profile.name.clone();
    let runtime = tunnel::RUNTIME
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let probe = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), inspect(&mut profile, None)).await
    })??;
    let connection = match probe {
        Probe::Ready(_, connection) => connection,
        Probe::BackendUnavailable(_, error, _) => anyhow::bail!("{error}"),
        _ => anyhow::bail!("SSH profile is not ready"),
    };
    let directory = remote_project_directory(connection.client(), path)?;
    let project = register_remote_project(&connection.client(), directory, name)?;
    Ok(ProjectLaunch {
        profile,
        machine,
        project,
        state_dir,
        connection,
    })
}

pub(super) fn register_remote_project(
    client: &fintwind_client::DaemonClient,
    directory: PathBuf,
    name: String,
) -> Result<Project> {
    let request = |command| {
        client.request_with_timeout(Uuid::nil(), Uuid::nil(), command, Duration::from_secs(10))
    };
    let fintwind_client::ResponsePayload::TaskState { projects, .. } =
        request(fintwind_client::Command::LoadTaskState)?
    else {
        anyhow::bail!("invalid remote project catalog response");
    };
    if let Some(project) = projects.into_iter().find(|project| project.path == directory) {
        return Ok(project);
    }
    let mut project = Project::from_path(directory);
    if !name.is_empty() {
        project.name = name;
    }
    let response = request(fintwind_client::Command::SaveTaskStateCheckpoint {
        projects: vec![project.clone()],
        skeleton_session_ids: Vec::new(),
        sessions: Vec::new(),
    })?;
    ensure!(
        matches!(
            response,
            fintwind_client::ResponsePayload::TaskStateSaved { .. }
        ),
        "remote daemon did not confirm project registration"
    );
    Ok(project)
}

#[cfg(test)]
mod e2e;

fn action(
    cx: &mut Context<Fintwind>,
    _theme: &Theme,
    id: impl Into<ElementId>,
    label: String,
    run: impl Fn(&mut Fintwind, &mut Context<Fintwind>) + Clone + 'static,
) -> Button {
    window_action(cx, id, label, move |this, _, cx| run(this, cx))
}

fn window_action(
    cx: &mut Context<Fintwind>,
    id: impl Into<ElementId>,
    label: String,
    run: impl Fn(&mut Fintwind, &mut Window, &mut Context<Fintwind>) + 'static,
) -> Button {
    Button::new(id, label)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Sm)
        .on_click(cx.listener(move |this, _, window, cx| run(this, window, cx)))
}

pub(super) fn ssh_wait_button(
    cx: &mut Context<Fintwind>,
    theme: Theme,
    id: &'static str,
    focus: &FocusHandle,
    label: String,
    variant: ButtonVariant,
    enabled: bool,
    active: bool,
    show_spinner: bool,
    run: impl Fn(&mut Fintwind, &mut Context<Fintwind>) + 'static,
) -> Stateful<Div> {
    let filled = matches!(variant, ButtonVariant::Primary | ButtonVariant::Danger);
    let foreground = if filled { theme.on_accent } else { theme.text };
    let background = if matches!(variant, ButtonVariant::Danger) {
        theme.danger
    } else {
        theme.accent_fill
    };
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label.clone())
        .track_focus(focus)
        .when(enabled || active, |button| button.tab_index(0))
        .h(px(24.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(if matches!(variant, ButtonVariant::Ghost) {
            theme.border_strong.opacity(0.0)
        } else if filled {
            background
        } else {
            theme.border_strong
        })
        .when(filled, |button| button.bg(background))
        .when(matches!(variant, ButtonVariant::Secondary), |button| {
            button.bg(theme.surface)
        })
        .flex()
        .items_center()
        .justify_center()
        .text_size(ui_px(12.0))
        .text_color(foreground)
        .font_weight(FontWeight::MEDIUM)
        .when(!enabled && !active, |button| button.opacity(0.45))
        .when(enabled, |button| {
            button.hover(|style| {
                if filled {
                    style.opacity(0.9)
                } else {
                    style.bg(theme.overlay)
                }
            })
        })
        .focus_visible(|style| style.border_color(theme.accent_focus))
        .child(crate::ui::loading_label(label, show_spinner, foreground))
        .when(enabled, |button| {
            button.on_activation(cx, move |app, _, cx| run(app, cx))
        })
}

pub(super) fn ssh_wait_phase<K: PartialEq>(
    state: &RefCell<Option<LoadingDelay<K>>>,
    key: K,
    active: bool,
    cx: &mut Context<Fintwind>,
) -> LoadingPhase {
    let (phase, wake) = LoadingDelay::observe(&mut state.borrow_mut(), key, active);
    if let Some(wake) = wake {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wake.delay()).await;
            wake.fired();
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }
    phase
}

fn ssh_status(label: String, color: Hsla) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(ui_px(11.0))
        .text_color(color)
        .child(div().size(px(6.0)).flex_none().rounded_full().bg(color))
        .child(label)
}

fn ssh_info(label: String, value: impl IntoElement, theme: Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .py(px(8.0))
        .text_size(ui_px(13.0))
        .border_t_1()
        .border_color(theme.border)
        .child(
            div()
                .w(px(132.0))
                .flex_none()
                .text_color(theme.text_secondary)
                .child(label),
        )
        .child(div().min_w_0().flex_1().child(value))
}

impl Fintwind {
    pub(super) fn ssh_connection_for_quit(&mut self, cx: &App) -> Option<Arc<tunnel::Connection>> {
        if let Some(connection) = self.ssh.project_connection.take() {
            return Some(Arc::new(connection));
        }
        let owner = self.ssh_workspace_owner()?;
        let host = self.ssh.project_profile.as_ref()?.id;
        owner.read(cx).ssh.connections.get(&host).cloned()
    }
    pub(super) fn active_ssh_workspace(&self) -> Option<Entity<Self>> {
        self.ssh.workspaces.get(&self.ssh.active_workspace?).cloned()
    }

    pub(super) fn has_workspace_owner(&self) -> bool {
        self.ssh.workspace_owner.is_some()
    }

    pub(super) fn ssh_workspace_owner(&self) -> Option<Entity<Self>> {
        self.ssh.workspace_owner.as_ref()?.upgrade()
    }

    pub(super) fn active_ssh_host(&self) -> Option<Uuid> {
        self.ssh.active_workspace
    }

    pub(super) fn ssh_host_for_next_launch(&self) -> Option<Uuid> {
        self.ssh.active_workspace.or(self.ssh.restoring_workspace)
    }

    pub(super) fn restore_ssh_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.ssh.restoring_workspace else {
            return;
        };
        if self.mode != WorkspaceMode::Sessions || self.ssh_client_for(host).is_none() {
            return;
        }
        let entity = cx.entity().downgrade();
        window.defer(cx, move |window, cx| {
            let Some(entity) = entity.upgrade() else { return; };
            entity.update(cx, |app, cx| {
                if app.ssh.restoring_workspace == Some(host)
                    && app.ssh.active_workspace.is_none()
                    && app.mode == WorkspaceMode::Sessions
                    && let Some(workspace) = app.open_ssh_workspace(host, window, cx)
                {
                    window.focus(&workspace.read(cx).composer_focus(cx), cx);
                }
            });
        });
    }

    pub(super) fn cancel_ssh_navigation_restore(&mut self) {
        self.ssh.restoring_workspace = None;
    }

    pub(super) fn route_sidebar_host_action(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        action: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if let Some(owner) = self.ssh_workspace_owner() {
            self.hide_workspace_webviews(cx);
            window.defer(cx, move |window, cx| owner.update(cx, |app, cx| action(app, window, cx)));
        } else {
            action(self, window, cx);
        }
    }

    pub(super) fn return_to_local_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(owner) = self.ssh.workspace_owner.as_ref().and_then(|owner| owner.upgrade()) {
            self.hide_workspace_webviews(cx);
            owner.update(cx, |app, cx| {
                app.ssh.active_workspace = None;
                app.ssh.restoring_workspace = None;
                window.focus(&app.composer_focus(cx), cx);
                cx.notify();
            });
        }
    }

    pub(super) fn activate_local_sidebar_target(
        &mut self,
        id: Uuid,
        is_session: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ssh.active_workspace = None;
        if is_session {
            self.select_session(id, cx);
        } else {
            self.select_project(id, cx);
        }
        window.focus(&self.composer_focus(cx), cx);
        cx.notify();
    }

    pub(super) fn select_ssh_workspace_project(
        &mut self,
        host: Uuid,
        project_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.ssh.catalogs.get(&host)
            .and_then(|catalog| catalog.projects.iter().find(|project| project.id == project_id))
            .cloned() else { return; };
        self.open_registered_ssh_project(host, project, window, cx);
    }

    pub(super) fn select_ssh_workspace_session(
        &mut self,
        host: Uuid,
        project_id: Uuid,
        session_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_open_ssh_session(host, session_id, cx) {
            return;
        }
        let Some(catalog) = self.ssh.catalogs.get(&host) else { return; };
        let Some(project) = catalog.projects.iter().find(|project| project.id == project_id).cloned() else { return; };
        let Some(session) = catalog.sessions.iter().find(|session| session.id == session_id).cloned() else { return; };
        if let Some(workspace) = self.open_ssh_workspace(host, window, cx) {
            workspace.update(cx, |app, cx| {
                if !app.state.projects.iter().any(|existing| existing.id == project_id) {
                    app.state.projects.push(project);
                }
                if !app.state.sessions.iter().any(|existing| existing.id == session_id) {
                    app.state.sessions.push(session);
                }
                app.select_session(session_id, cx);
                window.focus(&app.composer_focus(cx), cx);
            });
        }
    }

    pub(super) fn can_open_ssh_session(&self, host: Uuid, session_id: Uuid, cx: &App) -> bool {
        self.ssh_client_for(host).is_some()
            || self.ssh.workspaces.get(&host).is_some_and(|workspace| {
                workspace.read(cx).state.sessions.iter().any(|session| {
                    session.id == session_id && session.detail_loaded
                })
            })
    }

    pub(super) fn open_registered_ssh_project(
        &mut self,
        host: Uuid,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project_id = project.id;
        let Some(workspace) = self.open_ssh_workspace(host, window, cx) else { return false; };
        workspace.update(cx, |app, cx| {
            if !app.state.projects.iter().any(|existing| existing.id == project_id) {
                app.state.projects.push(project);
            }
            app.select_project(project_id, cx);
            window.focus(&app.composer_focus(cx), cx);
        });
        true
    }

    fn open_ssh_workspace(
        &mut self,
        host: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Self>> {
        let workspace = if let Some(workspace) = self.ssh.workspaces.get(&host) {
            workspace.clone()
        } else {
            let connection = self.ssh.connections.get(&host).filter(|connection| connection.is_ready())?;
            let profile = self.ssh.profiles.iter().find(|profile| profile.id == host)?.clone();
            let daemon = connection.supervisor();
            let (store, loaded) = self.ssh.loaded_workspaces.remove(&host)?;
            let workspace = Self::new_workspace_view(window, cx, daemon, store, loaded, true);
            let owner = cx.entity().downgrade();
            workspace.update(cx, |app, _| {
                app.ssh.workspace_owner = Some(owner);
                app.ssh.project_machine = Some(profile.name.clone());
                app.ssh.project_profile = Some(profile);
            });
            self.ssh.workspaces.insert(host, workspace.clone());
            workspace
        };
        self.hide_workspace_webviews(cx);
        self.ssh.restoring_workspace = None;
        self.ssh.active_workspace = Some(host);
        cx.notify();
        Some(workspace)
    }

    pub(super) fn hide_workspace_webviews(&self, cx: &mut Context<Self>) {
        for browser in self.right_panel_browsers.values() {
            browser.update(cx, |view, cx| view.sync_native_state(false, false, cx));
        }
    }

    pub(super) fn open_workspace_settings(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if self.ssh.machine != self.ssh.active_workspace {
            self.ssh.machine = self.ssh.active_workspace;
            self.reset_provider_auth_target(cx);
            self.reset_skills_machine(cx);
        }
        self.open_settings_page(page, cx);
        cx.notify();
    }
    #[cfg(test)]
    pub(super) fn ssh_test_location_picker(&self) -> bool {
        self.ssh.test_location_picker
    }

    pub(crate) fn open_ssh_project(&mut self, remote: ProjectLaunch, cx: &mut Context<Self>) {
        self.ssh.project_connection = Some(remote.connection);
        self.ssh.project_machine = Some(remote.machine);
        self.ssh.project_profile = Some(remote.profile);
        let project_id = remote.project.id;
        if self.state.projects.iter().any(|project| project.id == project_id) {
            self.select_project(project_id, cx);
        } else {
            self.state.projects.push(remote.project);
            self.create_session_for(project_id, cx);
        }
        cx.notify();
    }

    pub(super) fn ssh_project_machine(&self) -> Option<&str> {
        self.ssh.project_machine.as_deref()
    }

    pub(super) fn ssh_project_creator(
        &self,
        server: Option<Uuid>,
    ) -> Option<impl FnOnce(String, String, Option<String>, super::add_project_dialog::CloneProgress) -> Result<String> + use<>> {
        let profile = match server {
            Some(id) => self.ssh.profiles.iter().find(|p| p.id == id),
            None => self.ssh.project_profile.as_ref(),
        }?
        .clone();
        Some(
            move |parent: String, name: String, repository: Option<String>, progress: super::add_project_dialog::CloneProgress| {
                let runtime = tunnel::RUNTIME
                    .as_ref()
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                let result = runtime.block_on(async {
                    let (session, _) =
                        tokio::time::timeout(Duration::from_secs(20), authenticate(&profile))
                            .await??;
                    #[cfg(test)]
                    if name == "interrupted-clone" && std::env::var_os("FINTWIND_SSH_E2E_CLONE_DISCONNECT").is_some() && {
                        static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                        !INTERRUPTED.swap(true, std::sync::atomic::Ordering::Relaxed)
                    } {
                        let (signal, started) = tokio::sync::oneshot::channel();
                        let mut signal = Some(signal);
                        let operation = tunnel::create_project(&session, &parent, &name, repository.as_deref(), |update| {
                            progress.publish(update);
                            if let Some(signal) = signal.take() {
                                let _ = signal.send(());
                            }
                        });
                        tokio::pin!(operation);
                        return tokio::select! {
                            biased;
                            Ok(()) = started => {
                                session.disconnect(russh::Disconnect::ByApplication, "SSH clone test", "en").await?;
                                operation.await
                            }
                            result = &mut operation => result,
                        };
                    }
                    tunnel::create_project(&session, &parent, &name, repository.as_deref(), |update| progress.publish(update)).await
                })?;
                match result {
                    tunnel::ProjectCreation::Created(path) => Ok(path),
                    tunnel::ProjectCreation::Exists => {
                        anyhow::bail!("{}", tr!("project.target_exists"))
                    }
                    tunnel::ProjectCreation::Failed => {
                        anyhow::bail!("{}", tr!("project.creation_failed"))
                    }
                    tunnel::ProjectCreation::Unconfirmed => {
                        anyhow::bail!("{}", tr!("project.creation_unknown"))
                    }
                }
            },
        )
    }

    pub(super) fn saved_ssh_hosts(&self) -> Vec<(Uuid, String)> {
        self.ssh
            .profiles
            .iter()
            .map(|p| (p.id, p.name.clone()))
            .collect()
    }

    pub(super) fn project_host_connection(
        &self,
        host: Option<Uuid>,
    ) -> (bool, bool, Option<String>) {
        let Some(id) = host.filter(|id| self.ssh_client_for(*id).is_none()) else {
            return (false, false, None);
        };
        let connecting = self.ssh.connection_action.is_some_and(|(host, _)| host == id);
        let error = if connecting {
            None
        } else {
            self.ssh
                .connection_errors
                .get(&id)
                .or_else(|| self.ssh.backend_errors.get(&id))
                .cloned()
        };
        (connecting, !self.ssh.busy, error)
    }

    pub(super) fn connect_project_host(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if !self.ssh.busy
            && let Some(profile) = self
                .ssh
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
        {
            self.ssh.selected = Some(id);
            self.ssh_probe(profile, cx);
        }
    }

    pub(super) fn ssh_client_for(&self, id: Uuid) -> Option<fintwind_client::DaemonClient> {
        self.ssh
            .connections
            .get(&id)
            .filter(|connection| connection.is_ready())
            .map(|connection| connection.client())
    }

    pub(super) fn load_ssh_profiles(&mut self, cx: &mut Context<Self>) {
        self.load_ssh_profiles_with_navigation(!self.daemon.is_remote(), cx);
    }

    fn load_ssh_profiles_with_navigation(&mut self, restore_navigation: bool, cx: &mut Context<Self>) {
        if self.ssh.loaded || self.ssh.busy {
            return;
        }
        self.ssh.busy = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let profiles = read_profiles()?;
                    let catalogs = profiles.iter().filter_map(|profile| {
                        let bytes = std::fs::read(catalog_path(profile.id).ok()?).ok()?;
                        let (host, snapshot): (CatalogHost, RemoteTaskStateSnapshot) =
                            serde_json::from_slice(&bytes).ok()?;
                        (host == CatalogHost::from(profile)).then_some((profile.id, snapshot))
                    }).collect::<HashMap<_, _>>();
                    let last_host = restore_navigation.then(read_ssh_navigation).flatten();
                    Ok::<_, anyhow::Error>((profiles, catalogs, last_host))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.ssh.busy = false;
                match result {
                    Ok((profiles, catalogs, last_host)) => {
                        this.ssh.catalogs = catalogs;
                        this.ssh.row_focus = profiles
                            .iter()
                            .map(|profile| (profile.id, cx.focus_handle().tab_stop(true)))
                            .collect();
                        this.ssh.selected = profiles.first().map(|p| p.id);
                        this.ssh.profiles = profiles;
                        this.sidebar_rows_fingerprint.set(None);
                        this.ssh
                            .remote_settings
                            .retain(|id, _| this.ssh.profiles.iter().any(|p| p.id == *id));
                        if this
                            .ssh
                            .machine
                            .is_some_and(|id| !this.ssh.profiles.iter().any(|p| p.id == id))
                        {
                            this.ssh.machine = None;
                            this.reset_skills_machine(cx);
                        }
                        this.ssh.loaded = true;
                        this.ssh.error = None;
                        if let Some(host) = last_host.filter(|host| {
                            this.ssh.profiles.iter().any(|profile| profile.id == *host)
                        }) {
                            this.ssh.restoring_workspace = Some(host);
                            this.connect_project_host(host, cx);
                        }
                    }
                    Err(error) => this.ssh.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn persist_ssh_profiles(
        &mut self,
        profiles: Vec<Profile>,
        selected: Option<Uuid>,
        connect: bool,
        cx: &mut Context<Self>,
    ) {
        if self.ssh.busy || !self.ssh.loaded {
            return;
        }
        self.ssh.busy = true;
        self.ssh.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (profiles, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = write_profiles(&profiles);
                    (profiles, result)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.ssh.busy = false;
                match result {
                    Ok(()) => {
                        this.ssh
                            .row_focus
                            .retain(|id, _| profiles.iter().any(|profile| profile.id == *id));
                        for profile in &profiles {
                            this.ssh
                                .row_focus
                                .entry(profile.id)
                                .or_insert_with(|| cx.focus_handle().tab_stop(true));
                        }
                        this.ssh
                            .last_read
                            .retain(|id, _| profiles.iter().any(|p| p.id == *id));
                        let previous = &this.ssh.profiles;
                        this.ssh.deployments.retain(|id, _| {
                            previous.iter().find(|p| p.id == *id)
                                == profiles.iter().find(|p| p.id == *id)
                                && profiles.iter().any(|p| p.id == *id)
                        });
                        this.ssh.connections.retain(|id, _| {
                            previous.iter().find(|p| p.id == *id).map(CatalogHost::from)
                                == profiles.iter().find(|p| p.id == *id).map(CatalogHost::from)
                        });
                        this.ssh.workspaces.retain(|id, _| this.ssh.connections.contains_key(id));
                        this.ssh.active_workspace = this.ssh.active_workspace.filter(|id| this.ssh.workspaces.contains_key(id));
                        for profile in &profiles {
                            if let Some(workspace) = this.ssh.workspaces.get(&profile.id) {
                                workspace.update(cx, |app, cx| {
                                    app.ssh.project_machine = Some(profile.name.clone());
                                    app.ssh.project_profile = Some(profile.clone());
                                    cx.notify();
                                });
                            }
                        }
                        this.ssh
                            .ready
                            .retain(|id| this.ssh.connections.contains_key(id));
                        this.ssh
                            .opencode_installed
                            .retain(|id, _| this.ssh.connections.contains_key(id));
                        this.ssh.remote_settings.retain(|id, _| {
                            previous.iter().find(|p| p.id == *id)
                                == profiles.iter().find(|p| p.id == *id)
                        });
                        let changed_machine = this.ssh.machine.is_some_and(|id| {
                            previous.iter().find(|p| p.id == id)
                                != profiles.iter().find(|p| p.id == id)
                        });
                        if this
                            .ssh
                            .machine
                            .is_some_and(|id| !profiles.iter().any(|p| p.id == id))
                        {
                            this.ssh.machine = None;
                        }
                        this.ssh.catalogs.retain(|id, _| {
                            let old = previous.iter().find(|profile| profile.id == *id);
                            let new = profiles.iter().find(|profile| profile.id == *id);
                            new.is_some() && old.map(CatalogHost::from) == new.map(CatalogHost::from)
                        });
                        this.ssh.profiles = profiles;
                        this.sidebar_rows_fingerprint.set(None);
                        this.ssh.catalog_tasks.retain(|id, _| this.ssh.connections.contains_key(id));
                        this.ssh.loaded_workspaces.retain(|id, _| this.ssh.connections.contains_key(id));
                        this.ssh.restoring_workspace = this.ssh.restoring_workspace.filter(|id| {
                            this.ssh.profiles.iter().any(|profile| profile.id == *id)
                        });
                        this.ssh.backend_errors.clear();
                        this.ssh.connection_errors.clear();
                        if changed_machine {
                            this.ssh.mcp = Default::default();
                            this.ssh.providers_loaded = false;
                            this.ssh.mcp_loaded = false;
                            this.reset_provider_auth_target(cx);
                            this.reset_skills_machine(cx);
                        }
                        this.ssh.selected = selected;
                        this.ssh.draft = None;
                        if connect
                            && let Some(profile) = this
                                .ssh
                                .profiles
                                .iter()
                                .find(|p| Some(p.id) == selected)
                                .cloned()
                        {
                            this.ssh_probe(profile, cx);
                        }
                        this.ssh.remove = None;
                    }
                    Err(error) => this.ssh.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ssh_edit(&mut self, profile: Option<Profile>, window: &mut Window, cx: &mut Context<Self>) {
        let (id, values) = profile.map_or_else(
            || {
                (
                    Uuid::new_v4(),
                    [
                        String::new(),
                        String::new(),
                        "22".into(),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ],
                )
            },
            |p| {
                (
                    p.id,
                    [
                        p.name,
                        p.host,
                        p.port.to_string(),
                        p.user,
                        p.key.to_string_lossy().into_owned(),
                        p.daemon_port
                            .map(|port| port.to_string())
                            .unwrap_or_default(),
                        p.token_file,
                    ],
                )
            },
        );
        let manual_backend = !values[5].is_empty();
        let fields = values.map(|value| {
            cx.new(|cx| {
                let mut field = ComposerInput::new(window, cx)
                    .search_field()
                    .placeholder("");
                field.set_content(value, cx);
                field
            })
        });
        self.ssh.remove = None;
        self.ssh.error = None;
        self.ssh.draft = Some(Draft {
            id,
            fields,
            manual_backend,
        });
        cx.notify();
    }

    fn ssh_save_draft(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = &self.ssh.draft else {
            return;
        };
        let [name, host, port, user, key, daemon_port, token_file] = draft
            .fields
            .each_ref()
            .map(|input| input.read(cx).content().trim().to_owned());
        let id = draft.id;
        let key = PathBuf::from(key);
        let Ok(port) = port.parse::<u16>() else {
            self.ssh.error = Some(tr!("ssh.invalid_fields"));
            cx.notify();
            return;
        };
        if name.is_empty() || host.is_empty() || port == 0 || user.is_empty() || !key.is_absolute()
        {
            self.ssh.error = Some(tr!("ssh.invalid_fields"));
            cx.notify();
            return;
        }
        let mut profiles = self.ssh.profiles.clone();
        let daemon_port = match (daemon_port.as_str(), token_file.as_str()) {
            ("", "") => None,
            (port, path) if path.starts_with('/') && port.parse::<u16>().is_ok_and(|n| n > 0) => {
                port.parse().ok()
            }
            _ => {
                self.ssh.error = Some(tr!("ssh.invalid_daemon"));
                cx.notify();
                return;
            }
        };
        let new = !profiles.iter().any(|profile| profile.id == id);
        let mut profile = Profile {
            id,
            name,
            host,
            port,
            user,
            key,
            fingerprint: None,
            daemon_port,
            token_file,
        };
        if let Some(existing) = profiles.iter_mut().find(|p| p.id == id) {
            if existing.host == profile.host && existing.port == profile.port {
                profile.fingerprint = existing.fingerprint.clone();
            }
            self.ssh.last_read.remove(&id);
            *existing = profile;
        } else {
            profiles.push(profile);
        }
        self.persist_ssh_profiles(profiles, Some(id), new, cx);
    }

    fn ssh_probe(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.ssh_connect(profile, SshConnectionAction::Connect, cx);
    }

    fn ssh_connect(
        &mut self,
        mut profile: Profile,
        action: SshConnectionAction,
        cx: &mut Context<Self>,
    ) {
        if self.ssh.busy {
            return;
        }
        self.ssh.busy = true;
        self.ssh.error = None;
        self.ssh.remove = None;
        let id = profile.id;
        let mut profiles = profile
            .fingerprint
            .is_none()
            .then(|| self.ssh.profiles.clone());
        let preload = !self.ssh.workspaces.contains_key(&id);
        *self.ssh.connection_loading.get_mut() = None;
        self.ssh.connection_action = Some((id, action));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (result, fingerprint) = cx
                .background_executor()
                .spawn(async move {
                    let result = tunnel::RUNTIME
                        .as_ref()
                        .map_err(|error| anyhow::anyhow!("{error}"))
                        .and_then(|runtime| runtime.block_on(async {
                            tokio::time::timeout(Duration::from_secs(210), inspect(&mut profile, Some(action)))
                                .await
                                .context(tr!("ssh.timeout"))?
                        }));
                    let fingerprint = if let Some(profiles) = profiles.as_mut()
                        && let Some(fingerprint) = profile.fingerprint.as_ref()
                        && let Some(saved) = profiles.iter_mut().find(|saved| saved.id == id)
                    {
                        saved.fingerprint = Some(fingerprint.clone());
                        if let Err(error) = write_profiles(profiles) {
                            return (Err(error), None);
                        }
                        Some(fingerprint.clone())
                    } else {
                        None
                    };
                    let mut result = match result {
                        Ok(probe) => probe,
                        Err(error) => return (Err(error), fingerprint),
                    };
                    let opencode = match &mut result {
                        Probe::Ready(values, connection) => probe_opencode(&connection.client())
                            .map(|(installed, version)| {
                                if let (Some(value), Some(version)) = (values.first_mut(), version)
                                {
                                    *value = version;
                                }
                                installed
                            }),
                        _ => None,
                    };
                    let loaded = match &result {
                        Probe::Ready(_, connection) if preload => {
                            match load_ssh_workspace(id, &connection.supervisor()) {
                                Ok(loaded) => Some(loaded),
                                Err(error) => return (Err(error), fingerprint),
                            }
                        }
                        _ => None,
                    };
                    (Ok((result, opencode, loaded)), fingerprint)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.ssh.busy = false;
                this.ssh.connection_action = None;
                if let Some(fingerprint) = fingerprint
                    && let Some(profile) = this.ssh.profiles.iter_mut().find(|profile| profile.id == id)
                {
                    profile.fingerprint = Some(fingerprint);
                }
                match result {
                    Ok((Probe::Snapshot(values, deployment), _, _)) => {
                        this.ssh.backend_errors.remove(&id);
                        this.ssh.connection_errors.remove(&id);
                        this.ssh.last_read.insert(id, values);
                        this.ssh.deployments.remove(&id);
                        if let Some(deployment) = deployment {
                            this.ssh.deployments.insert(id, deployment);
                        }
                    }
                    Ok((Probe::BackendUnavailable(values, error, deployment), _, _)) => {
                        this.ssh.connection_errors.remove(&id);
                        this.ssh.deployments.remove(&id);
                        if let Some(deployment) = deployment {
                            this.ssh.deployments.insert(id, deployment);
                        }
                        this.ssh.last_read.insert(id, values);
                        this.ssh.backend_errors.insert(id, error);
                    }
                    Ok((Probe::Ready(values, connection), opencode, loaded)) => {
                        if let Some(loaded) = loaded {
                            this.ssh.loaded_workspaces.insert(id, loaded);
                        }
                        this.ssh.backend_errors.remove(&id);
                        this.ssh.connection_errors.remove(&id);
                        this.ssh.last_read.insert(id, values);
                        this.ssh.deployments.remove(&id);
                        this.ssh.opencode_installed.remove(&id);
                        if let Some(installed) = opencode {
                            this.ssh.opencode_installed.insert(id, installed);
                        }
                        this.ssh.remote_settings.insert(id, connection.settings());
                        if let Some(current) = this.ssh.connections.get_mut(&id)
                            && this.ssh.workspaces.contains_key(&id)
                        {
                            Arc::get_mut(current).expect("SSH connection shared only during quit").replace_tunnel(connection);
                        } else {
                            this.ssh.connections.insert(id, Arc::new(connection));
                        }
                        this.watch_ssh_catalog(id, cx);
                        if this.ssh_client_for(id).is_some() {
                            this.ssh.ready.insert(id);
                        } else {
                            this.ssh.ready.remove(&id);
                        }
                        if this.ssh.machine == Some(id)
                            && this.mode == WorkspaceMode::Settings
                            && this.settings_page == SettingsPage::Skills
                        {
                            this.ensure_skills_catalog(true, cx);
                        }
                        this.watch_ssh_connections(cx);
                        if this.ssh.machine == Some(id)
                            && this.mode == WorkspaceMode::Settings
                            && matches!(
                                this.settings_page,
                                SettingsPage::McpServers | SettingsPage::McpMarket
                            )
                        {
                            this.reset_mcp_page(cx);
                            this.load_ssh_mcp(cx);
                        }
                        if this.ssh.machine == Some(id)
                            && this.mode == WorkspaceMode::Settings
                            && this.settings_page == SettingsPage::Providers
                        {
                            this.reset_providers_page(cx);
                        }
                    }
                    Err(error) => {
                        this.ssh.deployments.remove(&id);
                        this.ssh.backend_errors.remove(&id);
                        if action == SshConnectionAction::Deploy {
                            this.show_toast_with_tone(error.to_string(), ToastTone::Alert);
                        }
                        this.ssh.connection_errors.insert(id, error.to_string());
                    }
                }
                if this.add_project_targets_host(id)
                    && let (_, _, Some(error)) = this.project_host_connection(Some(id))
                {
                    this.report_add_project_error(error, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn settings_machine_selected(&self) -> bool {
        self.ssh.machine.is_some()
    }

    pub(super) fn settings_daemon(&self) -> Option<fintwind_client::DaemonSupervisor> {
        if self.settings_machine_selected() {
            self.settings_ssh_supervisor()
        } else {
            Some(self.daemon.clone())
        }
    }

    pub(super) fn settings_request(
        daemon: &fintwind_client::DaemonSupervisor,
        command: fintwind_client::Command,
    ) -> std::io::Result<fintwind_client::ResponsePayload> {
        daemon
            .client()
            .request_with_timeout(Uuid::nil(), Uuid::nil(), command, Duration::from_secs(10))
            .map_err(std::io::Error::other)
    }

    pub(super) fn settings_json<T: serde::de::DeserializeOwned>(
        daemon: &fintwind_client::DaemonSupervisor,
        command: fintwind_client::Command,
    ) -> std::io::Result<T> {
        match Self::settings_request(daemon, command)? {
            fintwind_client::ResponsePayload::Json { value } => {
                serde_json::from_value(value).map_err(std::io::Error::other)
            }
            _ => Err(std::io::Error::other("invalid settings response")),
        }
    }

    pub(super) fn settings_ssh_host(&self) -> Option<Uuid> {
        self.ssh.machine
    }

    pub(super) fn settings_ssh_supervisor(&self) -> Option<fintwind_client::DaemonSupervisor> {
        self.ssh
            .connections
            .get(&self.ssh.machine?)
            .filter(|connection| connection.is_ready())
            .map(|connection| connection.supervisor())
    }

    pub(super) fn settings_ssh_host_ready(&self) -> bool {
        self.ssh
            .machine
            .is_none_or(|id| self.ssh.ready.contains(&id))
    }

    fn watch_ssh_connections(&mut self, cx: &mut Context<Self>) {
        if self.ssh.watching {
            return;
        }
        self.ssh.watching = true;
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_secs(1)).await;
                let keep = this
                    .update(cx, |app, cx| {
                        let ready = app
                            .ssh
                            .connections
                            .iter()
                            .filter(|(_, c)| c.is_ready())
                            .map(|(id, _)| *id)
                            .collect::<HashSet<_>>();
                        if ready != app.ssh.ready {
                            for (id, workspace) in &app.ssh.workspaces {
                                if ready.contains(id) != app.ssh.ready.contains(id) {
                                    workspace.update(cx, |_, cx| cx.notify());
                                }
                            }
                            let restored = app.ssh.machine.is_some_and(|id| {
                                ready.contains(&id) && !app.ssh.ready.contains(&id)
                            });
                            app.ssh.ready = ready;
                            if restored
                                && app.mode == WorkspaceMode::Settings
                                && app.settings_page == SettingsPage::Skills
                            {
                                app.ensure_skills_catalog(true, cx);
                            }
                            if restored
                                && app.mode == WorkspaceMode::Settings
                                && app.settings_page == SettingsPage::Providers
                            {
                                app.reset_providers_page(cx);
                            }
                            if restored
                                && app.mode == WorkspaceMode::Settings
                                && app.settings_page == SettingsPage::McpServers
                            {
                                app.reset_mcp_page(cx);
                                app.load_ssh_mcp(cx);
                            }
                            cx.notify();
                        }
                        app.ssh.watching = !app.ssh.connections.is_empty();
                        app.ssh.watching
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    fn watch_ssh_catalog(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(connection) = self.ssh.connections.get(&id) else {
            return;
        };
        let Some(profile) = self.ssh.profiles.iter().find(|profile| profile.id == id) else {
            return;
        };
        let host = Arc::new(CatalogHost::from(profile));
        let clients = connection.supervisor().subscribe_clients();
        let task = cx.spawn(async move |this, cx| {
            let mut subscription = None;
            loop {
                let refresh = if let Some(client) = clients.try_iter().last() {
                    subscription = Some((client.subscribe_task_state(), client));
                    true
                } else {
                    subscription.as_ref().is_some_and(|(revisions, _)| {
                        revisions.try_iter().last().is_some()
                    })
                };
                if refresh && let Some((_, client)) = &subscription {
                    let client = client.clone();
                    let host = host.clone();
                    let result = cx.background_executor().spawn(async move {
                        let snapshot = super::runtime::load_remote_task_state(&client)?;
                        let persisted = catalog_path(id).and_then(|path| {
                            write_ssh_state(path, &serde_json::to_vec(&(&*host, &snapshot))?)
                        });
                        if let Err(error) = persisted {
                            eprintln!("could not cache SSH catalog for {id}: {error:#}");
                        }
                        Ok::<_, anyhow::Error>(snapshot)
                    }).await;
                    if let Ok(snapshot) = result
                        && this.update(cx, |app, cx| {
                            app.ssh.catalogs.insert(id, snapshot);
                            app.sidebar_rows_fingerprint.set(None);
                            cx.notify();
                        }).is_err()
                    {
                        break;
                    }
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        });
        self.ssh.catalog_tasks.insert(id, task);
    }

    pub(super) fn ssh_project_catalog(&self, id: Uuid) -> Option<&RemoteTaskStateSnapshot> {
        self.ssh.catalogs.get(&id)
    }

    pub(super) fn render_settings_machine_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let selected = self.ssh.machine;
        let machines = std::iter::once((None, tr!("ssh.local")))
            .chain(
                self.ssh
                    .profiles
                    .iter()
                    .map(|p| (Some(p.id), p.name.clone())),
            )
            .collect::<Vec<_>>();
        let label = machines
            .iter()
            .find(|(id, _)| *id == selected)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| tr!("ssh.local"));
        let handle = self.menu_handle("settings-machine", cx);
        let weak = cx.entity().downgrade();
        div()
            .px(px(12.0))
            .py(px(12.0))
            .border_t_1()
            .border_color(theme.sidebar_border)
            .child(dropdown_menu(
                MenuChip::new("settings-machine")
                    .label(label)
                    .outlined()
                    .selected(handle.is_open())
                    .w_full()
                    .justify_between(),
                "settings-machine-menu",
                &handle,
                MenuAlign::AboveLeft,
                move |_| {
                    machines
                        .iter()
                        .map(|(id, name)| {
                            let weak = weak.clone();
                            let id = *id;
                            MenuItem::new(name.clone(), move |_, cx| {
                                let _ = weak.update(cx, |app, cx| {
                                    if app.ssh.machine == id {
                                        return;
                                    }
                                    if app.providers_save_in_flight
                                        || app.providers_commit_generation != 0
                                        || app.models_dev_fetching
                                        || app.providers_auth_loading
                                    {
                                        return;
                                    }
                                    app.reset_provider_auth_target(cx);
                                    app.ssh.mcp = Default::default();
                                    app.reset_settings_rosters(cx);
                                    app.ssh.machine = id;
                                    app.reset_skills_machine(cx);
                                    app.open_settings_page(app.settings_page, cx);
                                    cx.notify();
                                });
                            })
                            .selected(id == selected)
                        })
                        .collect()
                },
            ))
            .into_any_element()
    }

    fn save_ssh_general(&mut self, cx: &mut Context<Self>) {
        let Some((id, field, input)) = &self.ssh.general_edit else {
            return;
        };
        let (id, field) = (*id, *field);
        if self.ssh.general_pending || self.ssh.machine != Some(id) {
            return;
        }
        let Some(client) = self.ssh_client_for(id) else {
            self.ssh.general_error = Some((id, true, tr!("ssh.offline")));
            cx.notify();
            return;
        };
        let value = input.read(cx).content().trim().to_owned();
        let profile = self
            .ssh
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned();
        self.ssh.general_pending = true;
        self.ssh.general_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let settings =
                        tunnel::save_general_setting(Some(client.clone()), field, &value)?;
                    let opencode = (field == tunnel::GeneralSetting::OpenCodePath)
                        .then(|| probe_opencode(&client));
                    Ok::<_, anyhow::Error>((settings, opencode))
                })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.ssh.general_pending = false;
                if app.ssh.profiles.iter().find(|p| p.id == id) == profile.as_ref() {
                    match result {
                        Ok((settings, opencode)) => {
                            app.ssh.remote_settings.insert(id, settings);
                            if let Some(opencode) = opencode {
                                app.ssh.opencode_installed.remove(&id);
                                if let Some((installed, version)) = opencode {
                                    app.ssh.opencode_installed.insert(id, installed);
                                    if let Some(value) = app
                                        .ssh
                                        .last_read
                                        .get_mut(&id)
                                        .and_then(|values| values.first_mut())
                                    {
                                        *value = version.unwrap_or_default();
                                    }
                                }
                            }
                            app.ssh.general_edit = None;
                        }
                        Err(error) => {
                            app.ssh.general_error = Some((id, true, error.to_string()));
                        }
                    }
                } else {
                    app.ssh.general_edit = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn install_ssh_opencode(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.ssh.general_pending
            || self.ssh.machine != Some(id)
            || self.ssh.opencode_installed.get(&id) != Some(&false)
        {
            return;
        }
        let Some(client) = self.ssh_client_for(id) else {
            self.ssh.general_error = Some((id, false, tr!("ssh.offline")));
            cx.notify();
            return;
        };
        let profile = self
            .ssh
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned();
        self.ssh.general_pending = true;
        self.ssh.general_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let nil = Uuid::nil();
                    let fintwind_client::ResponsePayload::OpenCodeInstalled { settings } = client
                        .request_with_timeout(
                        nil,
                        nil,
                        fintwind_client::Command::InstallOpenCode,
                        Duration::from_secs(330),
                    )?
                    else {
                        anyhow::bail!("invalid daemon response");
                    };
                    Ok::<_, anyhow::Error>((
                        settings,
                        probe_opencode(&client).and_then(|(_, version)| version),
                    ))
                })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.ssh.general_pending = false;
                if app.ssh.profiles.iter().find(|profile| profile.id == id) == profile.as_ref() {
                    match result {
                        Ok((settings, version)) => {
                            app.ssh.remote_settings.insert(id, settings);
                            app.ssh.opencode_installed.insert(id, true);
                            if let (Some(value), Some(version)) = (
                                app.ssh
                                    .last_read
                                    .get_mut(&id)
                                    .and_then(|values| values.first_mut()),
                                version,
                            ) {
                                *value = version;
                            }
                            app.show_toast(tr!("settings.opencode_installed"));
                        }
                        Err(error) => {
                            app.ssh.general_error = Some((
                                id,
                                false,
                                tr!("settings.opencode_operation_failed", error = error),
                            ))
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_ssh_general(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(id) = self.ssh.machine else {
            return div().into_any_element();
        };
        let Some(profile) = self.ssh.profiles.iter().find(|p| p.id == id) else {
            return div().into_any_element();
        };
        let ready = self.ssh.ready.contains(&id);
        let saving = self.ssh.general_edit.is_some();
        let phase = ssh_wait_phase(
            &self.ssh.general_loading,
            (id, saving),
            self.ssh.general_pending,
            cx,
        );
        let settings = self.ssh.remote_settings.get(&id);
        let mut body = div().mt(px(8.0)).flex().flex_col().gap(px(12.0)).child(
            div()
                .text_size(ui_px(12.0))
                .text_color(theme.text_secondary)
                .child(profile.name.clone()),
        );
        if !ready {
            body = body.child(
                div()
                    .text_size(ui_px(12.0))
                    .text_color(theme.warning_text)
                    .child(if settings.is_some() {
                        tr!("ssh.settings_cached")
                    } else {
                        tr!("ssh.offline")
                    }),
            );
        }
        if let Some(settings) = settings {
            use super::settings::{
                settings_card, settings_group_title, settings_row, settings_separator,
            };
            for (field, key, glyph, group, label, value) in [
                (
                    tunnel::GeneralSetting::OpenCodePath,
                    "ssh-opencode",
                    "icons/terminal.svg",
                    tr!("settings.group_environment"),
                    tr!("settings.opencode_binary"),
                    settings.opencode_binary_path(),
                ),
                (
                    tunnel::GeneralSetting::Workspace,
                    "ssh-workspace",
                    "icons/folder.svg",
                    tr!("settings.group_data"),
                    tr!("settings.general_workspace"),
                    settings.general_workspace_path(),
                ),
            ] {
                let mut row = settings_row(glyph, label, value.map(str::to_owned), theme);
                let mut card = settings_card(theme);
                let editing = self
                    .ssh
                    .general_edit
                    .as_ref()
                    .filter(|(host, current, _)| *host == id && *current == field);
                if field == tunnel::GeneralSetting::OpenCodePath {
                    let version = self
                        .ssh
                        .last_read
                        .get(&id)
                        .and_then(|values| values.first())
                        .filter(|version| !version.is_empty())
                        .cloned();
                    let missing = self.ssh.opencode_installed.get(&id) == Some(&false);
                    card = card
                        .child(
                            settings_row(
                                "icons/provider-opencode.svg",
                                "OpenCode".into(),
                                if missing {
                                    Some(tr!("settings.opencode_not_installed"))
                                } else {
                                    version
                                },
                                theme,
                            )
                            .when(missing, |row| {
                                let active = !saving
                                    && (self.ssh.general_pending || phase == LoadingPhase::Visible);
                                let focus =
                                    self.transcript_control_focus("ssh-opencode-install", cx);
                                row.child(ssh_wait_button(
                                    cx,
                                    theme,
                                    "ssh-opencode-install",
                                    &focus,
                                    tr!("settings.opencode_install"),
                                    ButtonVariant::Outline,
                                    ready && !active,
                                    active,
                                    !saving && phase == LoadingPhase::Visible,
                                    move |app, cx| app.install_ssh_opencode(id, cx),
                                ))
                            }),
                        )
                        .children(self.ssh.general_error.iter().filter_map(
                            |(host, editing, error)| {
                                (*host == id && !*editing).then(|| {
                                    div()
                                        .px(px(20.0))
                                        .pb(px(10.0))
                                        .text_size(ui_px(12.0))
                                        .text_color(theme.danger_text)
                                        .child(error.clone())
                                })
                            },
                        ))
                        .child(settings_separator(theme));
                }
                if ready && editing.is_none() {
                    let value = value.unwrap_or_default().to_owned();
                    row = row.child(
                        window_action(cx, key, tr!("ssh.edit"), move |app, window, cx| {
                            if app.ssh.general_pending
                                || app.ssh.machine != Some(id)
                                || app.ssh_client_for(id).is_none()
                            {
                                return;
                            }
                            let value = value.clone();
                            let input = cx.new(|cx| {
                                let mut input = ComposerInput::new(window, cx).search_field();
                                input.set_content(value, cx);
                                input
                            });
                            app.ssh.general_edit = Some((id, field, input));
                            app.ssh.general_error = None;
                            cx.notify();
                        })
                        .disabled(self.ssh.general_pending),
                    );
                }
                body = body.child(settings_group_title(group, theme)).child(
                    card.child(row)
                        .children(editing.map(|(_, _, input)| {
                            let pending = self.ssh.general_pending;
                            let showing = phase == LoadingPhase::Visible;
                            let focus = self.transcript_control_focus("ssh-general-save", cx);
                            div()
                                .px(px(20.0))
                                .pb(px(14.0))
                                .mt(px(10.0))
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .when(ready, |row| {
                                    row.child(div().flex_1().child(
                                        TextField::new("ssh-general-path", input.clone()).w_full(),
                                    ))
                                })
                                .child(
                                    action(
                                        cx,
                                        &theme,
                                        "ssh-general-cancel",
                                        tr!("common.cancel"),
                                        |app, cx| {
                                            app.ssh.general_edit = None;
                                            app.ssh.general_error = None;
                                            cx.notify();
                                        },
                                    )
                                    .disabled(pending),
                                )
                                .child(ssh_wait_button(
                                    cx,
                                    theme,
                                    "ssh-general-save",
                                    &focus,
                                    tr!("mcp.save"),
                                    ButtonVariant::Outline,
                                    ready && !pending && !showing,
                                    pending || showing,
                                    showing,
                                    Self::save_ssh_general,
                                ))
                        }))
                        .children(self.ssh.general_error.iter().filter_map(
                            |(host, is_editing, error)| {
                                (*host == id && *is_editing && editing.is_some()).then(|| {
                                    div()
                                        .px(px(20.0))
                                        .pb(px(10.0))
                                        .text_size(ui_px(12.0))
                                        .text_color(theme.danger_text)
                                        .child(error.clone())
                                })
                            },
                        )),
                );
            }
        }
        body.into_any_element()
    }

    pub(super) fn render_ssh_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let busy = self.ssh.busy;
        let mut root = div().pt(px(6.0)).flex().flex_col().gap(px(12.0)).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(ui_px(20.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(tr!("settings.ssh")),
                        )
                        .child(
                            div()
                                .text_size(ui_px(12.0))
                                .text_color(theme.text_secondary)
                                .child(tr!("ssh.local_only")),
                        ),
                )
                .child(
                    window_action(cx, "ssh-new", tr!("ssh.new"), |this, window, cx| {
                        this.ssh_edit(None, window, cx)
                    })
                    .primary()
                    .disabled(busy || !self.ssh.loaded || self.ssh.draft.is_some()),
                ),
        );
        if let Some(error) = &self.ssh.error {
            root = root.child(
                div()
                    .text_size(ui_px(12.0))
                    .text_color(theme.warning_text)
                    .child(error.clone()),
            );
        }
        if !self.ssh.loaded {
            if !busy {
                root = root.child(action(
                    cx,
                    &theme,
                    "ssh-retry",
                    tr!("ssh.retry"),
                    Self::load_ssh_profiles,
                ));
            }
            return root.into_any_element();
        }
        root.child(self.render_ssh_cards(cx, theme, busy))
            .into_any_element()
    }

    pub(super) fn render_ssh_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.mode != WorkspaceMode::Settings || self.settings_page != SettingsPage::Ssh {
            return None;
        }
        let theme = Theme::current(cx);
        let busy = self.ssh.busy;
        let dialog_key = self
            .ssh
            .draft
            .as_ref()
            .map(|draft| (draft.id, 0))
            .or_else(|| self.ssh.remove.map(|id| (id, 2)));
        let (loading_phase, wake) = if let Some(key) = dialog_key {
            LoadingDelay::observe(&mut self.ssh.dialog_loading.borrow_mut(), key, busy)
        } else {
            *self.ssh.dialog_loading.borrow_mut() = None;
            (LoadingPhase::Done, None)
        };
        let showing = loading_phase == LoadingPhase::Visible;
        if let Some(wake) = wake {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wake.delay()).await;
                wake.fired();
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        let weak = cx.entity().downgrade();
        let close = move |_: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |app, cx| {
                if !app.ssh.busy {
                    app.ssh.draft = None;
                    app.ssh.remove = None;
                    app.ssh.error = None;
                    cx.notify();
                }
            });
        };
        if let Some(draft) = &self.ssh.draft {
            let editing = self
                .ssh
                .profiles
                .iter()
                .any(|profile| profile.id == draft.id);
            let mut form = div().flex().flex_col().gap(px(10.0)).max_w(px(480.0));
            let mut host = None;
            for (index, (label, input)) in [
                tr!("ssh.name"),
                tr!("ssh.host"),
                tr!("ssh.port"),
                tr!("ssh.user"),
                tr!("ssh.key"),
                tr!("ssh.daemon_port"),
                tr!("ssh.token_file"),
            ]
            .into_iter()
            .zip(&draft.fields)
            .enumerate()
            {
                if index >= 5 && !draft.manual_backend {
                    continue;
                }
                let field = TextField::new(
                    SharedString::from(format!("ssh-field-{index}")),
                    input.clone(),
                )
                .w_full();
                let field = if index == 4 {
                    let key = input.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().flex_1().min_w_0().child(field))
                        .child(
                            action(
                                cx,
                                &theme,
                                "ssh-select-key",
                                tr!("ssh.choose_key"),
                                move |_, cx| {
                                    let receiver = cx.prompt_for_paths(PathPromptOptions {
                                        files: true,
                                        directories: false,
                                        multiple: false,
                                        prompt: Some(tr!("ssh.choose_key").into()),
                                    });
                                    let key = key.clone();
                                    cx.spawn(async move |_, cx| {
                                        if let Ok(Ok(Some(paths))) = receiver.await
                                            && let Some(path) = paths.first()
                                        {
                                            key.update(cx, |input, cx| {
                                                input.set_content(
                                                    path.to_string_lossy().into_owned(),
                                                    cx,
                                                )
                                            });
                                        }
                                    })
                                    .detach();
                                },
                            )
                            .disabled(busy),
                        )
                        .into_any_element()
                } else {
                    field.into_any_element()
                };
                let field = labeled_field(&theme, label, field);
                if index == 1 {
                    host = Some(field);
                } else if index == 2 {
                    form = form.child(
                        div()
                            .flex()
                            .gap(px(12.0))
                            .child(div().flex_1().min_w_0().children(host.take()))
                            .child(div().w(px(104.0)).flex_none().child(field)),
                    );
                } else {
                    form = form.child(field);
                }
            }
            form = form.child(
                div().child(
                    action(
                        cx,
                        &theme,
                        "ssh-manual-backend",
                        tr!("ssh.manual_backend"),
                        |app, cx| {
                            if let Some(draft) = &mut app.ssh.draft {
                                draft.manual_backend = !draft.manual_backend;
                                cx.notify();
                            }
                        },
                    )
                    .variant(ButtonVariant::Ghost)
                    .disabled(busy),
                ),
            );
            if let Some(error) = &self.ssh.error {
                form = form.child(
                    div()
                        .text_size(ui_px(12.0))
                        .text_color(theme.danger_text)
                        .child(error.clone()),
                );
            }
            let focus = self.transcript_control_focus("ssh-save", cx);
            let save = ssh_wait_button(
                cx,
                theme,
                "ssh-save",
                &focus,
                if editing {
                    tr!("ssh.save_edit")
                } else {
                    tr!("ssh.save_connect")
                },
                ButtonVariant::Primary,
                !busy && !showing,
                busy || showing,
                showing,
                Self::ssh_save_draft,
            );
            return Some(
                Dialog::new(
                    "ssh-editor",
                    if editing {
                        tr!("ssh.edit_title")
                    } else {
                        tr!("ssh.new_title")
                    },
                    close,
                )
                .child(form)
                .action(move |close| {
                    Button::new("ssh-cancel", tr!("common.cancel"))
                        .variant(ButtonVariant::Outline)
                        .disabled(busy)
                        .on_click(move |_, window, cx| close(window, cx))
                })
                .action(move |_| save)
                .into_any_element(),
            );
        }
        if let Some(id) = self.ssh.remove
            && let Some(profile) = self.ssh.profiles.iter().find(|profile| profile.id == id)
        {
            let focus = self.transcript_control_focus("ssh-confirm-remove", cx);
            let remove = ssh_wait_button(
                cx,
                theme,
                "ssh-confirm-remove",
                &focus,
                tr!("ssh.remove_connection"),
                ButtonVariant::Danger,
                !busy && !showing,
                busy || showing,
                showing,
                move |app, cx| {
                    let profiles: Vec<_> = app
                        .ssh
                        .profiles
                        .iter()
                        .filter(|profile| profile.id != id)
                        .cloned()
                        .collect();
                    let selected = profiles.first().map(|profile| profile.id);
                    app.persist_ssh_profiles(profiles, selected, false, cx);
                },
            );
            return Some(
                Dialog::new("ssh-remove-dialog", tr!("ssh.remove_title"), close)
                    .detail(tr!("ssh.remove_body", name = profile.name))
                    .action(move |close| {
                        Button::new("ssh-remove-cancel", tr!("common.cancel"))
                            .variant(ButtonVariant::Outline)
                            .disabled(busy)
                            .on_click(move |_, window, cx| close(window, cx))
                    })
                    .action(move |_| remove)
                    .into_any_element(),
            );
        }
        None
    }

    fn render_ssh_cards(&self, cx: &mut Context<Self>, theme: Theme, busy: bool) -> Div {
        let query = self
            .ssh
            .search
            .as_ref()
            .map(|input| input.read(cx).content().trim().to_lowercase())
            .unwrap_or_default();
        let address =
            |profile: &Profile| format!("{}@{}:{}", profile.user, profile.host, profile.port);
        let mut list = div()
            .w(px(276.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(14.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.raised)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(ui_px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr!("ssh.saved")),
                    )
                    .child(
                        div()
                            .text_size(ui_px(11.0))
                            .text_color(theme.text_secondary)
                            .child(self.ssh.profiles.len().to_string()),
                    ),
            )
            .children(
                self.ssh
                    .search
                    .as_ref()
                    .map(|input| TextField::new("ssh-search", input.clone()).w_full()),
            );
        let mut matches = 0;
        for profile in &self.ssh.profiles {
            let endpoint = address(profile);
            if !profile.name.to_lowercase().contains(&query)
                && !endpoint.to_lowercase().contains(&query)
            {
                continue;
            }
            matches += 1;
            let id = profile.id;
            let selected = self.ssh.selected == Some(id);
            let (status, color) = self.ssh.connection_status(id, theme);
            let select = move |this: &mut Self, cx: &mut Context<Self>| {
                if !this.ssh.busy {
                    this.ssh.selected = Some(id);
                    this.ssh.error = None;
                    this.ssh.remove = None;
                    cx.notify();
                }
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!("ssh-{id}")))
                    .when_some(self.ssh.row_focus.get(&id), |row, handle| {
                        row.track_focus(handle)
                    })
                    .focus_visible(|row| row.border_color(theme.accent_focus))
                    .border_l(px(2.0))
                    .border_color(if selected {
                        theme.accent_fill
                    } else {
                        theme.raised
                    })
                    .rounded(px(8.0))
                    .px(px(10.0))
                    .py(px(7.0))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .when(selected, |row| row.bg(theme.sidebar_item_background))
                    .when(!selected, |row| {
                        row.hover(|row| row.bg(theme.sidebar_item_background))
                    })
                    .child(
                        div()
                            .flex_none()
                            .child(Icon::new(IconName::Monitor).color(theme.text)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(ui_px(13.0))
                                    .child(profile.name.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_family(crate::theme::code_font_family())
                                    .text_size(ui_px(11.0))
                                    .text_color(theme.text_secondary)
                                    .child(endpoint),
                            )
                            .child(ssh_status(status, color)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| select(this, cx)))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            select(this, cx);
                            cx.stop_propagation();
                        }
                    })),
            );
        }
        if matches == 0 {
            list = list.child(
                div()
                    .py(px(8.0))
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_secondary)
                    .child(if query.is_empty() {
                        tr!("ssh.empty")
                    } else {
                        tr!("ssh.no_search_results")
                    }),
            );
        }
        let mut detail = div()
            .min_w_0()
            .flex_1()
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.raised)
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(10.0));
        if let Some(profile) = self
            .ssh
            .profiles
            .iter()
            .find(|p| Some(p.id) == self.ssh.selected)
        {
            let id = profile.id;
            let ready = self.ssh.ready.contains(&id);
            let deployment = self.ssh.deployments.get(&id).copied();
            let connection_error = self.ssh.connection_errors.get(&id);
            let backend_error = self.ssh.backend_errors.get(&id);
            let loading_key = self
                .ssh
                .connection_action
                .filter(|(host, _)| *host == id)
                .or_else(|| {
                    self.ssh
                        .connection_loading
                        .borrow()
                        .as_ref()
                        .filter(|loading| loading.key().0 == id)
                        .map(|loading| *loading.key())
                });
            let (loading_phase, wake) = if let Some(key) = loading_key {
                LoadingDelay::observe(
                    &mut self.ssh.connection_loading.borrow_mut(),
                    key,
                    self.ssh.connection_action == Some(key),
                )
            } else {
                (LoadingPhase::Done, None)
            };
            if let Some(wake) = wake {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wake.delay()).await;
                    wake.fired();
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }
            let (status, color) = self.ssh.connection_status(id, theme);
            let ssh_state = if ready || backend_error.is_some() || deployment.is_some() {
                tr!("ssh.connected")
            } else {
                status.clone()
            };
            let backend_state = if ready {
                tr!("ssh.ready")
            } else if backend_error.is_some() {
                tr!("ssh.backend_unavailable")
            } else if matches!(deployment, Some(Deployment::Upload)) {
                tr!("ssh.deploy_missing")
            } else {
                tr!("ssh.not_connected")
            };
            detail = detail
                .child(
                    div()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap(px(12.0))
                        .mb(px(4.0))
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .text_size(ui_px(17.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(profile.name.clone()),
                                )
                                .child(
                                    div()
                                        .font_family(crate::theme::code_font_family())
                                        .text_size(ui_px(11.0))
                                        .text_color(theme.text_secondary)
                                        .child(address(profile)),
                                ),
                        )
                        .child(div().flex_none().child(ssh_status(status, color))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(ssh_info("SSH".into(), div().child(ssh_state), theme))
                        .child(ssh_info(
                            tr!("ssh.backend"),
                            div().child(backend_state),
                            theme,
                        ))
                        .child(ssh_info(
                            tr!("ssh.authentication"),
                            div()
                                .flex()
                                .gap(px(6.0))
                                .min_w_0()
                                .child(div().flex_none().child(tr!("ssh.key")))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(ui_px(11.0))
                                        .text_color(theme.text_secondary)
                                        .child(profile.key.to_string_lossy().into_owned()),
                                ),
                            theme,
                        )),
                )
                .children(connection_error.or(backend_error).map(|error| {
                    div()
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .bg(theme.danger_soft)
                        .text_size(ui_px(12.0))
                        .text_color(theme.danger_text)
                        .child(error.clone())
                }));
            if let Some(snapshot) = self.ssh.last_read.get(&id) {
                let metrics = [
                    "OpenCode".to_owned(),
                    tr!("ssh.system"),
                    "CPU".to_owned(),
                    tr!("ssh.memory"),
                    "GPU".to_owned(),
                    tr!("ssh.disk"),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, label)| {
                    let value = snapshot
                        .get(index)
                        .filter(|value| !value.is_empty())
                        .cloned()
                        .unwrap_or_else(|| "—".into());
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(ui_px(11.0))
                                .text_color(theme.text_secondary)
                                .child(label),
                        )
                        .child(div().text_size(ui_px(13.0)).child(value))
                })
                .collect::<Vec<_>>();
                let mut environment = div()
                    .border_t_1()
                    .border_color(theme.border)
                    .py(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(ui_px(13.0))
                            .child(tr!("ssh.environment")),
                    );
                let mut metrics = metrics.into_iter();
                while let (Some(left), Some(right)) = (metrics.next(), metrics.next()) {
                    environment =
                        environment.child(div().flex().gap(px(16.0)).child(left).child(right));
                }
                detail = detail.child(environment);
            }
            {
                let probe = profile.clone();
                let deploy_profile = profile.clone();
                let edit = profile.clone();
                let connect_active =
                    self.ssh.connection_action == Some((id, SshConnectionAction::Connect));
                let deploy_active = self.ssh.connection_action == Some((id, SshConnectionAction::Deploy));
                let connect_showing = loading_phase == LoadingPhase::Visible
                    && loading_key == Some((id, SshConnectionAction::Connect));
                let deploy_showing = loading_phase == LoadingPhase::Visible
                    && loading_key == Some((id, SshConnectionAction::Deploy));
                let connect_focus = self.transcript_control_focus("ssh-connect", cx);
                let deploy_focus = self.transcript_control_focus("ssh-deploy", cx);
                detail = detail
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.0))
                            .child(ssh_wait_button(
                                cx,
                                theme,
                                "ssh-connect",
                                &connect_focus,
                                if ready {
                                    tr!("ssh.disconnect")
                                } else {
                                    tr!("ssh.connect")
                                },
                                ButtonVariant::Outline,
                                !busy && !connect_showing,
                                connect_active || connect_showing,
                                connect_showing,
                                move |this, cx| {
                                    if ready {
                                        this.ssh.connections.remove(&id);
                                        this.ssh.workspaces.remove(&id);
                                        this.ssh.loaded_workspaces.remove(&id);
                                        if this.ssh.active_workspace == Some(id) {
                                            this.ssh.active_workspace = None;
                                        }
                                        this.ssh.catalog_tasks.remove(&id);
                                        this.ssh.ready.remove(&id);
                                        this.ssh.opencode_installed.remove(&id);
                                        cx.notify();
                                    } else {
                                        this.ssh_probe(probe.clone(), cx);
                                    }
                                },
                            ))
                            .children(deployment.filter(|_| !ready).or_else(|| {
                                (ready && deploy_profile.daemon_port.is_none()).then_some(Deployment::Upload)
                            }).map(|deployment| {
                                ssh_wait_button(
                                    cx,
                                    theme,
                                    "ssh-deploy",
                                    &deploy_focus,
                                    match deployment {
                                        Deployment::Upload if ready => tr!("ssh.update_daemon"),
                                        Deployment::Upload => tr!("ssh.deploy_daemon"),
                                        Deployment::Start => tr!("ssh.start_daemon"),
                                    },
                                    ButtonVariant::Outline,
                                    !busy && !deploy_showing,
                                    deploy_active || deploy_showing,
                                    deploy_showing,
                                    move |this, cx| {
                                        this.ssh_connect(
                                            deploy_profile.clone(),
                                            SshConnectionAction::Deploy,
                                            cx,
                                        )
                                    },
                                )
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_t_1()
                            .border_color(theme.border)
                            .pt(px(12.0))
                            .child(
                                window_action(
                                    cx,
                                    "ssh-edit",
                                    tr!("ssh.edit_connection"),
                                    move |this, window, cx| {
                                        this.ssh_edit(Some(edit.clone()), window, cx)
                                    },
                                )
                                .variant(ButtonVariant::Ghost)
                                .disabled(busy),
                            )
                            .child(
                                action(
                                    cx,
                                    &theme,
                                    "ssh-remove",
                                    tr!("ssh.remove_connection"),
                                    move |this, cx| {
                                        this.ssh.remove = Some(id);
                                        cx.notify();
                                    },
                                )
                                .variant(ButtonVariant::Ghost)
                                .disabled(busy),
                            ),
                    );
            }
        } else {
            detail = detail.child(
                div()
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_secondary)
                    .child(tr!("ssh.empty")),
            );
        }
        div()
            .flex()
            .items_start()
            .gap(px(12.0))
            .child(list)
            .child(detail)
    }
}

pub(crate) const SSH_PROJECT_READY: &str = "fintwind-ssh-project-ready";

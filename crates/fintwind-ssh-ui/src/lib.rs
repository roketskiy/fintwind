//! Pure SSH connection, host-key verification, remote daemon probing,
//! profile/catalog persistence, Linux deployment, and lightweight status UI helpers.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use fintwind_client::ssh as tunnel;
use fintwind_protocol::model::Project;
use fintwind_protocol::tr;
use fintwind_ui::theme::{Theme, ui_px};
use gpui::{Div, Hsla, IntoElement, ParentElement, Styled, div, px};
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, load_secret_key};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod deploy;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key: PathBuf,
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub daemon_port: Option<u16>,
    #[serde(default)]
    pub token_file: String,
}

#[derive(PartialEq, Serialize, Deserialize)]
pub struct CatalogHost {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub fingerprint: Option<String>,
}

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

pub enum Probe {
    Snapshot(Vec<String>, Option<Deployment>),
    BackendUnavailable(Vec<String>, String, Option<Deployment>),
    Ready(Vec<String>, tunnel::Connection),
}

#[derive(Clone, Copy)]
pub enum Deployment {
    Upload,
    Start,
}

pub enum RemoteOpenCodeResult {
    Checked {
        installed: bool,
        version: Option<String>,
        update: Option<(PathBuf, String)>,
        error: Option<String>,
    },
    Changed {
        settings: fintwind_protocol::settings::DaemonSettings,
        version: Option<String>,
    },
}


#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SshConnectionAction {
    Connect,
    Deploy,
}


pub fn profiles_path() -> Result<PathBuf> {
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

pub fn read_profiles() -> Result<Vec<Profile>> {
    match std::fs::read(profiles_path()?) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

pub fn write_profiles(profiles: &[Profile]) -> Result<()> {
    write_ssh_state(profiles_path()?, &serde_json::to_vec_pretty(profiles)?)
}

pub fn catalog_path(id: Uuid) -> Result<PathBuf> {
    Ok(profiles_path()?.with_file_name("ssh-catalogs").join(format!("{id}.json")))
}

pub fn save_ssh_navigation(host: Option<Uuid>) -> Result<()> {
    write_ssh_state(
        profiles_path()?.with_file_name("ssh-navigation.json"),
        &serde_json::to_vec(&host)?,
    )
}

pub fn read_ssh_navigation() -> Option<Uuid> {
    let path = profiles_path().ok()?.with_file_name("ssh-navigation.json");
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok().flatten()
}

pub fn write_ssh_state(path: PathBuf, content: &[u8]) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("missing profile directory")?)?;
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    std::fs::write(&temporary, content)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(temporary);
        return Err(error.into());
    }
    Ok(())
}

pub struct HostKey {
    pub expected: Option<String>,
    pub observed: Arc<Mutex<Option<String>>>,
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

pub async fn authenticate(profile: &Profile) -> Result<(client::Handle<HostKey>, String)> {
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

pub async fn inspect(profile: &mut Profile, action: Option<SshConnectionAction>) -> Result<Probe> {
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
        if let Some(action) = action {
            match deploy::ensure_current(&session, target, &detected, action).await {
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

pub fn probe_opencode(client: &fintwind_client::DaemonClient) -> Option<(bool, Option<String>)> {
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

pub fn remote_daemon_busy(client: &fintwind_client::DaemonClient) -> Result<bool> {
    let response = client.request(
        Uuid::nil(),
        Uuid::nil(),
        fintwind_client::Command::LoadTaskState,
    )?;
    let fintwind_client::ResponsePayload::TaskState { sessions, .. } = response else {
        anyhow::bail!("fintwind daemon returned an invalid task-state response");
    };
    Ok(sessions.iter().any(|session| session.status.is_busy()))
}

pub fn settings_opencode_workspace(
    daemon: &fintwind_client::DaemonSupervisor,
) -> std::io::Result<(PathBuf, PathBuf)> {
    tunnel::opencode_workspace(&daemon.client()).map_err(std::io::Error::other)
}

pub struct ProjectLaunch {
    pub profile: Profile,
    pub machine: String,
    pub project: Project,
    pub state_dir: PathBuf,
    pub connection: tunnel::Connection,
}

pub fn remote_project_directory(
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

pub fn project_launch() -> Result<Option<ProjectLaunch>> {
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

pub fn prepare_project_launch(
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

pub fn register_remote_project(
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


pub fn ssh_status(label: String, color: Hsla) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(ui_px(11.0))
        .text_color(color)
        .child(div().size(px(6.0)).flex_none().rounded_full().bg(color))
        .child(label)
}

pub fn ssh_info(label: String, value: impl IntoElement, theme: Theme) -> Div {
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


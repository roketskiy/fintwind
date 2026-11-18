use std::sync::{Arc, LazyLock};
use std::time::Duration;

use crate::DaemonSupervisor;
use anyhow::{Context, Result, ensure};
use fintwind_protocol::settings::{
    DaemonSettings, GENERAL_WORKSPACE_PATH_KEY, OPENCODE_BINARY_PATH_KEY,
};
use russh::{ChannelMsg, client};
use tokio::{net::TcpListener, task::JoinSet};

mod mcp_oauth;
pub use mcp_oauth::authenticate_mcp;

pub fn provider_integrations(
    daemon: DaemonSupervisor,
) -> Result<(
    (std::path::PathBuf, std::path::PathBuf),
    Vec<fintwind_protocol::provider_session::IntegrationSummary>,
)> {
    let (binary, directory) = opencode_workspace(&daemon.client())?;
    let rows = crate::persistence::StateStore::remote(daemon)
        .fetch_integrations(binary.clone(), directory.clone())?;
    Ok(((binary, directory), rows))
}

pub fn opencode_workspace(
    client: &crate::DaemonClient,
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    use crate::{Command, ResponsePayload};
    let nil = uuid::Uuid::nil();
    let ResponsePayload::TaskState { default_cwd, .. } =
        client.request(nil, nil, Command::LoadTaskState)?
    else {
        anyhow::bail!("invalid remote workspace response");
    };
    let directory = default_cwd;
    let ResponsePayload::ProviderProbe { probe, .. } = client.request(
        nil,
        nil,
        Command::ProbeProvider {
            binary_override: None,
            directory: Some(directory.clone()),
            discover_models: false,
            probe_version: false,
        },
    )?
    else {
        anyhow::bail!("invalid remote OpenCode probe");
    };
    let binary = probe
        .path
        .context("OpenCode was not found on the selected host")?;
    Ok((binary, directory))
}

pub fn load_skills(
    client: &crate::DaemonClient,
) -> Result<fintwind_protocol::skills::SkillsCatalog> {
    use crate::{Command, ResponsePayload};
    let nil = uuid::Uuid::nil();
    let ResponsePayload::TaskState { projects, .. } =
        client.request(nil, nil, Command::LoadTaskState)?
    else {
        anyhow::bail!("invalid remote project catalog");
    };
    let projects = projects
        .into_iter()
        .filter(|p| !p.is_projectless())
        .map(|p| (p.display_name(), p.path))
        .collect();
    match client.request(nil, nil, Command::LoadSkills { projects })? {
        ResponsePayload::SkillsCatalog { catalog } => Ok(catalog),
        _ => anyhow::bail!("invalid remote skills catalog"),
    }
}

pub fn change_skill(
    client: Option<&crate::DaemonClient>,
    dirs: Vec<std::path::PathBuf>,
    enabled: Option<bool>,
) -> Result<()> {
    use crate::{Command, ResponsePayload};
    let client = client
        .filter(|c| !c.is_disconnected())
        .context("Skills daemon is offline")?;
    let command = match enabled {
        Some(enabled) => Command::SetSkillsEnabled { dirs, enabled },
        None => Command::TrashSkills { dirs },
    };
    ensure!(
        matches!(
            client.request(uuid::Uuid::nil(), uuid::Uuid::nil(), command)?,
            ResponsePayload::Ack
        ),
        "skill change was not acknowledged"
    );
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GeneralSetting {
    OpenCodePath,
    Workspace,
}

pub fn save_general_setting(
    client: Option<crate::DaemonClient>,
    field: GeneralSetting,
    value: &str,
) -> Result<DaemonSettings> {
    use crate::{Command, ResponsePayload};
    let client = client
        .filter(|client| !client.is_disconnected())
        .context("SSH daemon is offline")?;
    ensure!(
        value.is_empty() || (value.starts_with('/') && !value.chars().any(char::is_control)),
        "expected an absolute Linux path"
    );
    let key = match field {
        GeneralSetting::OpenCodePath => OPENCODE_BINARY_PATH_KEY,
        GeneralSetting::Workspace => GENERAL_WORKSPACE_PATH_KEY,
    };
    let request = |command| {
        client.request_with_timeout(
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
            command,
            Duration::from_secs(15),
        )
    };
    let read = || match request(Command::GetSettings)? {
        ResponsePayload::Settings { settings } => Ok(settings),
        _ => anyhow::bail!("invalid settings response"),
    };
    let mut settings = read()?;
    if value.is_empty() {
        settings.extra.remove(key);
    } else {
        settings.extra.insert(key.into(), value.into());
    }
    let expected = settings.extra.get(key).cloned();
    ensure!(
        matches!(
            request(Command::UpdateSettings { settings })?,
            ResponsePayload::Ack
        ),
        "settings save was not acknowledged"
    );
    let actual = read()?;
    ensure!(
        actual.extra.get(key) == expected.as_ref(),
        "settings were changed before confirmation"
    );
    Ok(actual)
}

#[derive(Debug)]
pub enum ProjectCreation {
    Created(String),
    Exists,
    Failed,
    Unconfirmed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GitCloneStage {
    Counting,
    Compressing,
    Receiving,
    Resolving,
    Updating,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GitCloneProgress {
    pub stage: GitCloneStage,
    pub percent: u8,
}

#[derive(Default)]
pub struct GitCloneProgressReader {
    line: Vec<u8>,
}

impl GitCloneProgressReader {
    pub fn push(&mut self, bytes: &[u8], mut report: impl FnMut(GitCloneProgress)) {
        for &byte in bytes {
            if matches!(byte, b'\r' | b'\n') {
                if let Some(progress) = Self::parse(&self.line) {
                    report(progress);
                }
                self.line.clear();
            } else if self.line.len() < 1024 {
                self.line.push(byte);
            }
        }
    }

    fn parse(line: &[u8]) -> Option<GitCloneProgress> {
        let line = std::str::from_utf8(line).ok()?;
        let line = line.trim_start_matches("remote: ").trim_start();
        let (stage, value) = line.split_once(':')?;
        let stage = match stage {
            "Counting objects" => GitCloneStage::Counting,
            "Compressing objects" => GitCloneStage::Compressing,
            "Receiving objects" => GitCloneStage::Receiving,
            "Resolving deltas" => GitCloneStage::Resolving,
            "Updating files" => GitCloneStage::Updating,
            _ => return None,
        };
        let percent = value.split_once('%')?.0.trim().parse().ok()?;
        (percent <= 100).then_some(GitCloneProgress { stage, percent })
    }
}

pub async fn create_project<H: client::Handler>(
    session: &client::Handle<H>,
    parent: &str,
    name: &str,
    repository: Option<&str>,
    mut progress: impl FnMut(GitCloneProgress),
) -> Result<ProjectCreation> {
    ensure!(
        parent.starts_with('/') && !parent.chars().any(char::is_control),
        "expected an absolute Linux parent directory"
    );
    ensure!(
        !name.is_empty()
            && !matches!(name, "." | "..")
            && !name.contains('/')
            && !name.chars().any(char::is_control),
        "expected a single folder name"
    );
    let repo = repository.map(github_repository).transpose()?;
    let clone = repo.map_or(String::new(), |url| {
        format!(
            "LC_ALL=C GIT_TERMINAL_PROMPT=0 git clone --progress -- {} . >/dev/null || exit 13; ",
            shell_arg(&url)
        )
    });
    let script = format!(
        "unset CDPATH; cd -P -- {} || exit 11; mkdir -- {} 2>/dev/null || {{ if [ -e {} ] || [ -L {} ]; then exit 12; fi; exit 11; }}; cd -P -- {} || exit 13; {clone}pwd -P",
        shell_arg(parent),
        shell_arg(name),
        shell_arg(name),
        shell_arg(name),
        shell_arg(name),
    );
    let mut channel = session.channel_open_session().await?;
    let operation = async {
        channel
            .exec(
                true,
                format!("exec timeout -k 5s 120s /bin/sh -c {}", shell_arg(&script)),
            )
            .await?;
        let mut bytes = Vec::new();
        let mut progress_reader = GitCloneProgressReader::default();
        let mut status = None;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => {
                    ensure!(bytes.len() + data.len() <= 8192, "project path too long");
                    bytes.extend_from_slice(&data);
                }
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                ChannelMsg::ExtendedData { data, .. } => progress_reader.push(&data, &mut progress),
                _ => {}
            }
        }
        Ok::<_, anyhow::Error>(match status {
            Some(0) => ProjectCreation::Created(
                String::from_utf8(bytes)?
                    .strip_suffix('\n')
                    .context("missing project path")?
                    .to_owned(),
            ),
            Some(12) => ProjectCreation::Exists,
            Some(11 | 13) => ProjectCreation::Failed,
            _ => ProjectCreation::Unconfirmed,
        })
    };
    Ok(
        match tokio::time::timeout(Duration::from_secs(130), operation).await {
            Ok(Ok(result)) => result,
            _ => ProjectCreation::Unconfirmed,
        },
    )
}

fn github_repository(value: &str) -> Result<String> {
    let path = value
        .strip_prefix("https://github.com/")
        .context("expected a GitHub HTTPS repository URL")?;
    let (owner, repo) = path
        .split_once('/')
        .context("expected a GitHub owner and repository")?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    ensure!(
        [owner, repo].into_iter().all(|part| !part.is_empty()
            && !matches!(part, "." | "..")
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))),
        "invalid GitHub owner or repository"
    );
    Ok(format!("https://github.com/{owner}/{repo}.git"))
}

fn shell_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub static RUNTIME: LazyLock<std::io::Result<tokio::runtime::Runtime>> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
});

pub struct Connection {
    supervisor: DaemonSupervisor,
    relay: Relay,
    retired_client: Option<crate::DaemonClient>,
}

struct Relay(tokio::task::JoinHandle<()>);

impl Drop for Relay {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Connection {
    pub fn settings(&self) -> fintwind_protocol::settings::DaemonSettings {
        self.supervisor.settings()
    }

    pub fn client(&self) -> crate::DaemonClient {
        self.supervisor.client()
    }

    pub fn supervisor(&self) -> DaemonSupervisor {
        self.supervisor.clone()
    }

    pub fn replace_tunnel(&mut self, mut replacement: Self) {
        self.retired_client = Some(self.client());
        std::mem::swap(&mut self.relay, &mut replacement.relay);
        self.supervisor.reconnect_through(&replacement.supervisor);
    }

    pub async fn open<H: client::Handler + Sync + 'static>(
        session: client::Handle<H>,
        port: u16,
        token_file: &str,
    ) -> Result<Self> {
        Self::open_with_reconnect(
            Arc::new(session),
            port,
            token_file,
            None::<fn() -> std::future::Ready<Result<client::Handle<H>>>>,
        )
        .await
    }

    pub async fn open_with_reconnect<H, F, Fut>(
        session: Arc<client::Handle<H>>,
        port: u16,
        token_file: &str,
        reconnect: Option<F>,
    ) -> Result<Self>
    where
        H: client::Handler + Sync + 'static,
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<client::Handle<H>>> + Send,
    {
        let token = read_token(&session, token_file).await?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?.to_string();
        let mut relay = Relay(tokio::spawn(async move {
            let mut session = session;
            loop {
                if !forward(&listener, session, port).await {
                    break;
                }
                let Some(reconnect) = &reconnect else {
                    break;
                };
                let mut delay = Duration::from_secs(1);
                session = loop {
                    let attempt = async move {
                        tokio::time::sleep(delay).await;
                        tokio::time::timeout(Duration::from_secs(12), reconnect()).await
                    };
                    tokio::pin!(attempt);
                    let result = loop {
                        tokio::select! {
                            biased;
                            result = &mut attempt => break result,
                            socket = listener.accept() => if socket.is_err() { return; },
                        }
                    };
                    if let Ok(Ok(session)) = result {
                        break Arc::new(session);
                    }
                    delay = (delay * 2).min(Duration::from_secs(15));
                };
            }
        }));
        let mut attempt =
            tokio::task::spawn_blocking(move || DaemonSupervisor::connect(&address, token));
        let result = tokio::select! {
            result = &mut attempt => result.context("daemon connection worker stopped").and_then(|result| result),
            _ = &mut relay.0 => anyhow::bail!("SSH tunnel closed before daemon connected"),
        };
        match result {
            Ok(supervisor) => Ok(Self { supervisor, relay, retired_client: None }),
            Err(error) => Err(error),
        }
    }

    pub fn is_ready(&self) -> bool {
        let client = self.client();
        !self.relay.0.is_finished()
            && !client.is_disconnected()
            && self.retired_client.as_ref().is_none_or(|retired| !retired.same_connection(&client))
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.supervisor.client().disconnect();
    }
}

async fn read_token<H: client::Handler>(session: &client::Handle<H>, path: &str) -> Result<String> {
    let path = format!("'{}'", path.replace('\'', "'\"'\"'"));
    let mut channel = session.channel_open_session().await?;
    channel
        .exec(true, format!("exec head -c 4097 -- {path}"))
        .await?;
    let mut bytes = Vec::new();
    let mut ok = false;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => {
                ensure!(
                    bytes.len() + data.len() <= 4096,
                    "invalid daemon token file"
                );
                bytes.extend_from_slice(&data);
            }
            ChannelMsg::ExitStatus { exit_status } => ok = exit_status == 0,
            _ => {}
        }
    }
    ensure!(ok, "could not read remote daemon token file");
    let token = String::from_utf8(bytes).context("invalid daemon token file")?;
    let token = token.trim();
    ensure!(
        !token.is_empty() && !token.chars().any(char::is_whitespace),
        "invalid daemon token file"
    );
    Ok(token.to_owned())
}

async fn forward<H: client::Handler + Sync + 'static>(
    listener: &TcpListener,
    session: Arc<client::Handle<H>>,
    port: u16,
) -> bool {
    let mut streams = JoinSet::new();
    let mut health = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = health.tick() => if session.is_closed() { return true; },
            _ = streams.join_next(), if !streams.is_empty() => {},
            socket = listener.accept(), if streams.len() < 16 => {
                let Ok((mut socket, peer)) = socket else { return false; };
                let session = session.clone();
                streams.spawn(async move {
                    let opened = tokio::time::timeout(Duration::from_secs(6), session.channel_open_direct_tcpip(
                        "127.0.0.1", u32::from(port), "127.0.0.1", u32::from(peer.port()),
                    )).await;
                    if let Ok(Ok(channel)) = opened {
                        let _ = tokio::io::copy_bidirectional(&mut socket, &mut channel.into_stream()).await;
                    }
                });
            }
        }
    }
}

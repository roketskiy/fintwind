mod crypto;
mod session;
mod signal;
mod snapshot;
mod transport;

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use smol::channel;
use uuid::Uuid;

const EVENT_CAPACITY: usize = 32;
const MAX_MUTATION_IDS: usize = 65_536;
pub(crate) const SIGNAL_ORIGIN: &str = "https://fintwind-mobile.roketskiy.top:18761";

/// The signaling endpoint. Direct connections need no server-side identity:
/// the pairing key protects the signaling exchange end to end.
pub struct RelayConfig {
    relay_url: url::Url,
}

impl RelayConfig {
    /// Loads the user-selected signaling config. Call only when remote control
    /// starts. A missing file is the normal case: the fixed public endpoint is
    /// used. Legacy registration keys in older files are parsed and ignored.
    pub fn load_default() -> anyhow::Result<Self> {
        let path = dirs_path()?.join(".fintwind").join("remote-control.json");
        let mut relay_url =
            url::Url::parse(SIGNAL_ORIGIN).context("invalid built-in signaling URL")?;
        match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > 16 * 1024 {
                    bail!("remote-control config is too large");
                }
                let raw: RawConfig =
                    serde_json::from_slice(&bytes).context("invalid remote-control config")?;
                if let Some(value) = raw.relay_url.as_deref() {
                    let configured = url::Url::parse(value).context("invalid relay URL")?;
                    if configured.scheme() != "https"
                        || configured.host_str().is_none()
                        || !configured.username().is_empty()
                        || configured.password().is_some()
                        || configured.query().is_some()
                        || configured.fragment().is_some()
                        || !matches!(configured.path(), "" | "/")
                    {
                        bail!("remote-control config is not allowed");
                    }
                    if configured.host_str() == Some("fintwind-mobile.roketskiy.top")
                        && configured.port().is_none()
                    {
                        relay_url = url::Url::parse(SIGNAL_ORIGIN)?;
                    } else {
                        relay_url = configured;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(
                    anyhow::Error::new(error).context("could not read remote-control config")
                );
            }
        }
        Ok(Self { relay_url })
    }

    pub(crate) fn signal_url(&self) -> anyhow::Result<String> {
        let mut url = self.relay_url.clone();
        url.set_scheme("wss")
            .map_err(|_| anyhow::anyhow!("invalid signaling URL"))?;
        url.set_path("/signal/v2");
        Ok(url.into())
    }

    pub(crate) fn stun_url(&self, port: Option<u16>) -> anyhow::Result<String> {
        let host = self.relay_url.host_str().context("signaling host missing")?;
        Ok(format!("stun:{host}:{}", port.unwrap_or(18762)))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    relay_url: Option<String>,
    // Older installs wrote a shared registration key here. It is parsed so the
    // file still loads, and never used to register.
    #[serde(default, rename = "registration_key")]
    _registration_key: Option<String>,
}

fn dirs_path() -> anyhow::Result<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .context("user home directory is unavailable")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteStatus {
    Connecting,
    Waiting,
    Connected,
    Failed(String),
}

pub enum RemoteEvent {
    Status(RemoteStatus),
    Request(DesktopRequest),
    WatchSession {
        session_id: Uuid,
        snapshots: channel::Sender<Result<Value, String>>,
    },
}

pub struct DesktopRequest {
    pub id: Uuid,
    pub command: RemoteCommand,
    pub respond: channel::Sender<Result<Value, String>>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RemoteCommand {
    ListWorkspaces,
    GetTaskActivity,
    ListSessions {
        project_id: Uuid,
    },
    GetSession {
        session_id: Uuid,
    },
    WatchSession {
        session_id: Uuid,
        watch_id: Uuid,
        enabled: bool,
    },
    GetComposer {
        session_id: Uuid,
    },
    ConfigureSession {
        session_id: Uuid,
        model: Option<String>,
        interaction_mode: Option<String>,
        reasoning_effort: Option<String>,
        runtime_mode: Option<String>,
    },
    CreateSession {
        project_id: Uuid,
    },
    SendPrompt {
        session_id: Uuid,
        text: String,
        #[serde(default)]
        attachment_ids: Vec<Uuid>,
    },
    SteerPrompt {
        session_id: Uuid,
        text: String,
        #[serde(default)]
        attachment_ids: Vec<Uuid>,
    },
    UploadChunk {
        session_id: Uuid,
        upload_id: Uuid,
        name: String,
        offset: usize,
        data_base64: String,
        complete: bool,
    },
    Stop {
        session_id: Uuid,
    },
    RespondPermission {
        session_id: Uuid,
        request_id: String,
        option_id: String,
    },
    AnswerQuestions {
        session_id: Uuid,
        request_id: String,
        answers: Vec<RemoteAnswer>,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteAnswer {
    pub question_id: String,
    pub answers: Vec<String>,
}

impl RemoteCommand {
    fn is_mutation(&self) -> bool {
        !matches!(
            self,
            Self::ListWorkspaces
                | Self::GetTaskActivity
                | Self::ListSessions { .. }
                | Self::GetSession { .. }
                | Self::WatchSession { .. }
                | Self::GetComposer { .. }
        )
    }

    fn within_limits(&self) -> bool {
        let ok = |s: &str, max: usize| !s.is_empty() && s.len() <= max;
        match self {
            Self::ListWorkspaces | Self::GetTaskActivity => true,
            Self::WatchSession {
                session_id,
                watch_id,
                ..
            } => !session_id.is_nil() && !watch_id.is_nil(),
            Self::ListSessions { project_id } | Self::CreateSession { project_id } => {
                !project_id.is_nil()
            }
            Self::GetSession { session_id }
            | Self::GetComposer { session_id }
            | Self::Stop { session_id } => !session_id.is_nil(),
            Self::ConfigureSession {
                session_id,
                model,
                interaction_mode,
                reasoning_effort,
                runtime_mode,
            } => {
                !session_id.is_nil()
                    && model.as_deref().is_none_or(|value| ok(value, 512))
                    && interaction_mode
                        .as_deref()
                        .is_none_or(|value| matches!(value, "build" | "plan"))
                    && reasoning_effort
                        .as_deref()
                        .is_none_or(|value| ok(value, 128))
                    && runtime_mode.as_deref().is_none_or(|value| {
                        matches!(value, "ask" | "autoAcceptEdits" | "fullAccess")
                    })
            }
            Self::SendPrompt {
                session_id,
                text,
                attachment_ids,
            }
            | Self::SteerPrompt {
                session_id,
                text,
                attachment_ids,
            } => {
                !session_id.is_nil()
                    && (!text.trim().is_empty() || !attachment_ids.is_empty())
                    && text.len() <= 32 * 1024
                    && attachment_ids.len() <= 8
                    && attachment_ids.iter().all(|id| !id.is_nil())
            }
            Self::UploadChunk {
                session_id,
                upload_id,
                name,
                offset,
                data_base64,
                ..
            } => {
                !session_id.is_nil()
                    && !upload_id.is_nil()
                    && ok(name, 128)
                    && *offset <= 2_796_204
                    && ok(data_base64, 64 * 1024)
            }
            Self::RespondPermission {
                session_id,
                request_id,
                option_id,
            } => !session_id.is_nil() && ok(request_id, 256) && ok(option_id, 256),
            Self::AnswerQuestions {
                session_id,
                request_id,
                answers,
            } => {
                !session_id.is_nil()
                    && ok(request_id, 256)
                    && answers.len() <= 32
                    && answers.iter().all(|a| {
                        ok(&a.question_id, 256)
                            && a.answers.len() <= 32
                            && a.answers.iter().all(|s| s.len() <= 4096)
                    })
            }
        }
    }
}

pub struct RemoteControl {
    pairing_url: String,
    cancelled: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
}

impl RemoteControl {
    pub fn start(config: RelayConfig) -> anyhow::Result<(Self, channel::Receiver<RemoteEvent>)> {
        Self::start_with_options(config, None, None)
    }

    /// Starts remote control with test-only transport overrides: an alternate
    /// TCP connect address for the signaling WebSocket (TLS still authenticates
    /// the configured domain) and an alternate STUN port. Production callers
    /// use `start`, which passes neither.
    pub fn start_with_options(
        config: RelayConfig,
        signal_connect_addr: Option<String>,
        stun_port: Option<u16>,
    ) -> anyhow::Result<(Self, channel::Receiver<RemoteEvent>)> {
        let route = crypto::random_32()?;
        let secret = crypto::random_32()?;
        let route_text = crypto::encode(&route);
        let secret_text = crypto::encode(&secret);
        let pairing_url = format!(
            "{}#v=2&route={route_text}&key={secret_text}",
            config.relay_url.as_str().trim_end_matches('/')
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(tokio::sync::Notify::new());
        let transport_wake = wake.clone();
        let (events_tx, events_rx) = channel::bounded(EVENT_CAPACITY);
        let stop_flag = cancelled.clone();
        std::thread::Builder::new()
            .name("fintwind-remote-transport".into())
            .spawn(move || {
                // The transport runs on Tokio; the app consumes smol channels.
                // A dedicated bridge thread keeps the two executors apart.
                let (inner_tx, mut inner_rx) = tokio::sync::mpsc::channel(EVENT_CAPACITY);
                let bridge_cancelled = stop_flag.clone();
                let failure_tx = events_tx.clone();
                let bridge_result = std::thread::Builder::new()
                    .name("fintwind-remote-events".into())
                    .spawn(move || {
                        while let Some(mut event) = inner_rx.blocking_recv() {
                            loop {
                                if bridge_cancelled.load(Ordering::Acquire) {
                                    return;
                                }
                                match events_tx.try_send(event) {
                                    Ok(()) => break,
                                    Err(smol::channel::TrySendError::Closed(_)) => return,
                                    Err(smol::channel::TrySendError::Full(pending)) => {
                                        event = pending;
                                        std::thread::sleep(std::time::Duration::from_millis(20));
                                    }
                                }
                            }
                        }
                    })
                    .context("could not start remote event bridge");
                match bridge_result {
                    Ok(_bridge_thread) => {
                        transport::run(config, route, secret, inner_tx, stop_flag, signal_connect_addr, stun_port, transport_wake);
                    }
                    Err(_error) => {
                        let _ = failure_tx.try_send(RemoteEvent::Status(RemoteStatus::Failed(
                            "无法启动远控传输".into(),
                        )));
                    }
                }
            })
            .context("could not start remote transport worker")?;
        Ok((
            Self {
                pairing_url,
                cancelled,
                wake,
            },
            events_rx,
        ))
    }

    pub fn pairing_url(&self) -> &str {
        &self.pairing_url
    }

    pub fn stop(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}

impl Drop for RemoteControl {
    fn drop(&mut self) {
        self.stop();
    }
}

fn reserve_mutation(ids: &mut HashSet<Uuid>, id: Uuid) -> bool {
    if ids.contains(&id) || ids.len() >= MAX_MUTATION_IDS {
        return false;
    }
    ids.insert(id)
}

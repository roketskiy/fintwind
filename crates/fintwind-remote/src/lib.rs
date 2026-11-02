mod crypto;
mod enrollment;
mod identity;
mod snapshot;
mod transport;

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use smol::channel;
use uuid::Uuid;

const EVENT_CAPACITY: usize = 32;
const MAX_MUTATION_IDS: usize = 65_536;

/// Registration credentials remain private and are never formatted or logged.
pub struct RelayConfig {
    relay_url: url::Url,
    credentials: enrollment::CredentialCache,
}

impl RelayConfig {
    /// Loads the user-selected relay config. Call only when remote control starts.
    /// A missing file is the normal case: the fixed public relay is used, and an
    /// existing file is validated but its legacy registration key is ignored.
    pub fn load_default() -> anyhow::Result<Self> {
        let path = dirs_path()?.join(".fintwind").join("remote-control.json");
        match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > 16 * 1024 {
                    bail!("remote-control config is too large");
                }
                let raw: RawConfig =
                    serde_json::from_slice(&bytes).context("invalid remote-control config")?;
                if let Some(value) = raw.relay_url.as_deref() {
                    let relay_url = url::Url::parse(value).context("invalid relay URL")?;
                    if relay_url.scheme() != "https"
                        || relay_url.host_str() != Some("fintwind-mobile.roketskiy.top")
                        || relay_url.port().is_some()
                        || !relay_url.username().is_empty()
                        || relay_url.password().is_some()
                        || relay_url.query().is_some()
                        || relay_url.fragment().is_some()
                        || !matches!(relay_url.path(), "" | "/")
                    {
                        bail!("remote-control config is not allowed");
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
        Ok(Self {
            relay_url: url::Url::parse(enrollment::RELAY_ORIGIN)
                .context("invalid built-in relay URL")?,
            credentials: enrollment::CredentialCache::new(),
        })
    }
}

/// Fixed failure contexts safe to show. No raw error text or endpoint detail.
#[derive(Debug)]
pub(crate) enum RemoteFailure {
    IdentityUnavailable,
    Enrollment,
    RateLimited { retry_after: Duration },
    Revoked,
    CredentialExpired,
}

impl std::fmt::Display for RemoteFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::IdentityUnavailable => "relay installation identity is unavailable",
            Self::Enrollment => "relay admission failed",
            Self::RateLimited { .. } => "relay admission is rate limited",
            Self::Revoked => "relay installation identity was rejected",
            Self::CredentialExpired => "relay registration expired",
        })
    }
}

impl std::error::Error for RemoteFailure {}

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
    ListSessions {
        project_id: Uuid,
    },
    GetSession {
        session_id: Uuid,
    },
    CreateSession {
        project_id: Uuid,
    },
    SendPrompt {
        session_id: Uuid,
        text: String,
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
            Self::ListWorkspaces | Self::ListSessions { .. } | Self::GetSession { .. }
        )
    }

    fn within_limits(&self) -> bool {
        let ok = |s: &str, max: usize| !s.is_empty() && s.len() <= max;
        match self {
            Self::ListWorkspaces => true,
            Self::ListSessions { project_id } | Self::CreateSession { project_id } => {
                !project_id.is_nil()
            }
            Self::GetSession { session_id } | Self::Stop { session_id } => !session_id.is_nil(),
            Self::SendPrompt { session_id, text } => {
                !session_id.is_nil() && !text.trim().is_empty() && text.len() <= 32 * 1024
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
}

impl RemoteControl {
    pub fn start(config: RelayConfig) -> anyhow::Result<(Self, channel::Receiver<RemoteEvent>)> {
        let route = crypto::random_32()?;
        let secret = crypto::random_32()?;
        let route_text = crypto::encode(&route);
        let secret_text = crypto::encode(&secret);
        let pairing_url = format!(
            "https://fintwind-mobile.roketskiy.top/#v=1&route={route_text}&key={secret_text}"
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let (events_tx, events_rx) = channel::bounded(EVENT_CAPACITY);
        let stop_flag = cancelled.clone();
        std::thread::Builder::new()
            .name("fintwind-remote-transport".into())
            .spawn(move || transport::run(config, route, secret, events_tx, stop_flag))
            .context("could not start remote transport worker")?;
        Ok((
            Self {
                pairing_url,
                cancelled,
            },
            events_rx,
        ))
    }

    pub fn pairing_url(&self) -> &str {
        &self.pairing_url
    }

    pub fn stop(&self) {
        self.cancelled.store(true, Ordering::Release);
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

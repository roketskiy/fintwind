//! Desktop ownership of the Fintwind daemon process, and of the
//! remote-control host that runs beside it.
//!
//! The host daemon (`fintwind-remote-host`) is spawned here with the
//! loopback address and one-time token of the daemon this app owns. Both
//! values change whenever the supervisor replaces the daemon — a managed
//! restart binds a fresh ephemeral port with a fresh token — so a
//! supervisor thread watches the daemon's client updates and restarts the
//! host against the new endpoint. The token is passed to the child through
//! its environment only: it never appears in an argument list, a log line,
//! or a status string.
//!
//! What the devices page shows comes from [`remote_host_status`], a cached
//! snapshot the supervisor thread maintains. Render reads that snapshot
//! and nothing else: no process handles, no file reads, no sockets.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, anyhow, bail};

/// Where the remote-control host keeps its identity and grant files. Must
/// agree between this app's pairing page and the daemon binary: both talk
/// to the same relay identity and trust the same grants.
pub fn remote_data_dir() -> PathBuf {
    #[cfg(debug_assertions)]
    {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("temp")
            .join("remote-host")
    }
    #[cfg(not(debug_assertions))]
    {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("remote-host")
    }
}

/// The host's self-reported link state. `Unknown` is a real answer: the
/// host is running but has not said (recently) whether its relay session
/// is up, and the page must not guess either way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkState {
    Connected,
    Disconnected,
    Unknown,
}

/// What the desktop knows about the remote host process. Read by the
/// devices page; written only by the supervisor thread.
#[derive(Clone, Debug)]
pub enum RemoteHostStatus {
    /// Spawn in flight.
    Starting,
    /// The process is alive. `link` is its self-reported relay state, or
    /// [`LinkState::Unknown`] when it has not reported.
    Running { link: LinkState },
    /// The process exited or could not be spawned.
    Failed { reason: String },
    /// Never started: no executable, or the daemon is externally managed.
    NotStarted { reason: String },
}

/// How long a host status record stays fresh. The host rewrites it on
/// every relay transition and at least every 30 seconds; three missed
/// windows mean the writer is gone, which is "unknown", not "down".
const LINK_STALE_AFTER: Duration = Duration::from_secs(90);

/// The host's status record, read by the supervisor thread. Contract
/// (written by `fintwind-remote-host` next to its identity files):
///
/// ```json
/// {"relayConnected": true, "updatedAtMs": 1760000000000}
/// ```
///
/// `relayConnected` is true when the host's outbound WSS session is
/// authenticated at the relay. The file is rewritten on every transition
/// and refreshed at least every 30 seconds, so staleness is detectable.
/// A missing file means the host has not reported yet — never "down".
#[derive(serde::Deserialize)]
struct HostLinkRecord {
    #[serde(rename = "relayConnected")]
    relay_connected: bool,
    #[serde(rename = "updatedAtMs")]
    updated_at_ms: u64,
}

struct RemoteHostProcess {
    child: Option<Child>,
    /// The daemon endpoint this host was started against. A change means
    /// the supervisor replaced the daemon and the host must follow.
    endpoint: Option<(String, String)>,
    status: RemoteHostStatus,
    link: LinkState,
    /// Whether the host has ever written a status record. Until it has,
    /// an absent file is silence, not an unknown link.
    link_reported: bool,
}

static REMOTE_HOST: Mutex<Option<RemoteHostProcess>> = Mutex::new(None);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// The cached snapshot. Cheap enough for every frame: it clones a small
/// enum, and only the failure variants allocate.
pub fn remote_host_status() -> RemoteHostStatus {
    let guard = REMOTE_HOST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(process) => process.status.clone(),
        None => RemoteHostStatus::NotStarted {
            reason: "remote control has not started yet".to_owned(),
        },
    }
}

fn with_process<T>(action: impl FnOnce(&mut RemoteHostProcess) -> T) -> Option<T> {
    let mut guard = REMOTE_HOST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.as_mut().map(action)
}

fn set_status(status: RemoteHostStatus) {
    with_process(|process| process.status = status);
}

fn set_link(link: LinkState, reported: bool) {
    with_process(|process| {
        process.link = link;
        process.link_reported = reported;
    });
}

pub fn start_process() -> anyhow::Result<fintwind_client::DaemonSupervisor> {
    let address = std::env::var(fintwind_client::DAEMON_ADDRESS_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let token = std::env::var(fintwind_client::DAEMON_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty());
    match (address, token) {
        (Some(address), Some(token)) => {
            return fintwind_client::DaemonSupervisor::connect(address.trim(), token);
        }
        (Some(_), None) => bail!(
            "{} is set but {} is missing",
            fintwind_client::DAEMON_ADDRESS_ENV,
            fintwind_client::DAEMON_TOKEN_ENV
        ),
        (None, Some(_)) => bail!(
            "{} is set but {} is missing",
            fintwind_client::DAEMON_ADDRESS_ENV,
            fintwind_client::DAEMON_TOKEN_ENV
        ),
        (None, None) => {}
    }
    fintwind_client::DaemonSupervisor::spawn(&daemon_executable_path()?, cfg!(debug_assertions))
}

fn daemon_executable_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("FINTWIND_DAEMON_PATH").filter(|path| !path.is_empty()) {
        return Ok(path.into());
    }
    let executable = format!("fintwind-daemon{}", std::env::consts::EXE_SUFFIX);
    let current = std::env::current_exe().context("could not locate the fintwind executable")?;

    // Development keeps the daemon beside Cargo's debug artifacts rather than
    // inside the debug app bundle. The supervisor watches this file and swaps only
    // the daemon when the development watcher relinks it.
    #[cfg(debug_assertions)]
    if let Some(debug_directory) = current
        .ancestors()
        .find(|candidate| candidate.file_name().is_some_and(|name| name == "debug"))
    {
        let external = debug_directory.join(&executable);
        if external.is_file() {
            return Ok(external);
        }
    }

    let sibling = current
        .parent()
        .map(|directory| directory.join(&executable))
        .ok_or_else(|| anyhow!("fintwind executable has no parent directory"))?;
    if sibling.is_file() {
        return Ok(sibling);
    }
    #[cfg(debug_assertions)]
    bail!(
        "fintwind daemon was not found in Cargo's debug directory or next to the app executable: {}",
        sibling.display()
    );
    #[cfg(not(debug_assertions))]
    bail!(
        "fintwind daemon is missing next to the app executable: {}",
        sibling.display()
    )
}

/// Spawn the remote-control host beside the daemon and keep it honest.
///
/// The host re-reads the grant file on every handshake, so pairing or
/// revoking while it runs takes effect immediately; with no grant file it
/// simply sits invisible on the relay — the correct pre-pairing posture.
/// Failure to start it is not fatal: the desktop works fully without
/// remote control, and the devices page says so.
pub fn start_remote_host(supervisor: &fintwind_client::DaemonSupervisor) {
    // Defense in depth: `run` only calls this on a channel that ships the
    // remote-control surface, but the guard lives here too so no future call
    // site or environment tweak can start the host from a stable build.
    // Stable bundles no `fintwind-remote-host.exe` and shows no devices page.
    if !crate::REMOTE_CONTROL_ENABLED {
        return;
    }
    let executable = match remote_host_executable_path() {
        Ok(path) => path,
        Err(error) => {
            install_placeholder();
            set_status(RemoteHostStatus::NotStarted {
                reason: error.to_string(),
            });
            return;
        }
    };
    install_placeholder();
    let supervisor = supervisor.clone();
    if std::thread::Builder::new()
        .name("fintwind-remote-host-supervisor".into())
        .spawn(move || supervise_remote_host(supervisor, executable))
        .is_err()
    {
        set_status(RemoteHostStatus::Failed {
            reason: "could not start the remote host supervisor thread".to_owned(),
        });
    }
}

/// Create the status slot before any thread can read it, so a page opened
/// during startup sees a defined state rather than "not started yet".
fn install_placeholder() {
    let mut guard = REMOTE_HOST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        *guard = Some(RemoteHostProcess {
            child: None,
            endpoint: None,
            status: RemoteHostStatus::Starting,
            link: LinkState::Unknown,
            link_reported: false,
        });
    }
}

/// The supervisor loop: keep one host process running against the current
/// daemon endpoint, watch its lifetime, and mirror its self-reported link
/// state into the cached snapshot.
fn supervise_remote_host(supervisor: fintwind_client::DaemonSupervisor, executable: PathBuf) {
    let data_dir = remote_data_dir();
    let clients = supervisor.subscribe_clients();
    let mut endpoint = supervisor.local_endpoint();
    match &endpoint {
        Some(_) => spawn_host(&executable, endpoint.as_ref()),
        None => set_status(RemoteHostStatus::NotStarted {
            reason: "the daemon is managed outside fintwind; remote control needs its address"
                .to_owned(),
        }),
    }

    loop {
        // A new daemon client wakes this immediately (a managed restart
        // publishes one); otherwise the tick drives liveness and link
        // polling.
        match clients.recv_timeout(Duration::from_secs(2)) {
            Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }

        // The daemon was replaced: its address and token changed, so the
        // host must be restarted against the new pair. An in-place
        // reconnect publishes a client too, but the endpoint is identical
        // and nothing happens.
        let current = supervisor.local_endpoint();
        if current != endpoint {
            kill_host();
            endpoint = current.clone();
            match &endpoint {
                Some(_) => spawn_host(&executable, endpoint.as_ref()),
                None => set_status(RemoteHostStatus::NotStarted {
                    reason: "the daemon is managed outside fintwind; remote control needs its address"
                        .to_owned(),
                }),
            }
            continue;
        }

        // Process lifetime: an exited host is a visible failure, never a
        // silent hole in the page.
        if let Some(reason) = host_exit_reason() {
            set_status(RemoteHostStatus::Failed { reason });
            continue;
        }

        // The host's own link report. Absent before the first report is
        // silence; stale after one is "unknown", which is what the page
        // shows.
        let reported = with_process(|process| process.link_reported).unwrap_or(false);
        match read_host_link(&data_dir) {
            Some(record) => {
                let fresh = now_ms().saturating_sub(record.updated_at_ms)
                    < LINK_STALE_AFTER.as_millis() as u64;
                let link = if !fresh {
                    LinkState::Unknown
                } else if record.relay_connected {
                    LinkState::Connected
                } else {
                    LinkState::Disconnected
                };
                set_link(link, true);
            }
            None => {
                if reported {
                    set_link(LinkState::Unknown, true);
                }
            }
        }
    }
}

/// Spawn one host process against `endpoint` and record it. The daemon
/// token travels in the child's environment only.
fn spawn_host(executable: &PathBuf, endpoint: Option<&(String, String)>) {
    let Some((daemon_address, daemon_token)) = endpoint else {
        set_status(RemoteHostStatus::NotStarted {
            reason: "the daemon is managed outside fintwind; remote control needs its address"
                .to_owned(),
        });
        return;
    };
    let token_env = "FINTWIND_REMOTE_HOST_DAEMON_TOKEN";
    let relay_origin =
        std::env::var("FINTWIND_RELAY_ORIGIN").unwrap_or_else(|_| "https://relay.fintwind.xyz".to_owned());
    let data_dir = remote_data_dir();

    let mut command = Command::new(executable);
    command
        .arg("--relay-origin")
        .arg(&relay_origin)
        .arg("--daemon-address")
        .arg(daemon_address)
        .arg("--daemon-token-env")
        .arg(token_env)
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .env(token_env, daemon_token)
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    match command.spawn() {
        Ok(child) => {
            with_process(|process| {
                process.child = Some(child);
                process.endpoint = Some((daemon_address.clone(), daemon_token.clone()));
                process.status = RemoteHostStatus::Running {
                    link: LinkState::Unknown,
                };
                process.link = LinkState::Unknown;
                process.link_reported = false;
            });
            log_line(&format!(
                "[remote-host] running; relay={relay_origin} data={}",
                data_dir.display()
            ));
        }
        Err(error) => {
            set_status(RemoteHostStatus::Failed {
                reason: format!("could not launch {}: {error}", executable.display()),
            });
        }
    }
}

/// Stop the current host process, if any. A restart always kills first:
/// two hosts on one identity would displace each other at the relay.
fn kill_host() {
    if let Some(mut child) = with_process(|process| process.child.take()).flatten() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Why the host process is no longer running, when it is not.
fn host_exit_reason() -> Option<String> {
    with_process(|process| {
        let child = process.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => Some(format!("the remote host exited with {status}")),
            Ok(None) => None,
            Err(error) => Some(format!("the remote host could not be checked: {error}")),
        }
    })
    .flatten()
}

/// Read the host's link record. A missing, unreadable, or malformed file
/// is `None` — the supervisor decides what silence means.
fn read_host_link(data_dir: &PathBuf) -> Option<HostLinkRecord> {
    let text = std::fs::read_to_string(data_dir.join("host-status.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// The app is a GUI-subsystem binary on Windows: a console write goes
/// nowhere, so operational lines go to stderr like the daemon spawn does,
/// and the user-facing truth lives on the devices page instead.
fn log_line(line: &str) {
    eprintln!("{line}");
}

fn remote_host_executable_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("FINTWIND_REMOTE_HOST_PATH")
        .filter(|path| !path.is_empty())
    {
        return Ok(path.into());
    }
    let executable = format!("fintwind-remote-host{}", std::env::consts::EXE_SUFFIX);
    let current = std::env::current_exe().context("could not locate the fintwind executable")?;

    #[cfg(debug_assertions)]
    if let Some(debug_directory) = current
        .ancestors()
        .find(|candidate| candidate.file_name().is_some_and(|name| name == "debug"))
    {
        let external = debug_directory.join(&executable);
        if external.is_file() {
            return Ok(external);
        }
    }

    let sibling = current
        .parent()
        .map(|directory| directory.join(&executable))
        .ok_or_else(|| anyhow!("fintwind executable has no parent directory"))?;
    if sibling.is_file() {
        return Ok(sibling);
    }
    bail!(
        "fintwind-remote-host was not found in Cargo's debug directory or next to the app executable: {}",
        sibling.display()
    )
}

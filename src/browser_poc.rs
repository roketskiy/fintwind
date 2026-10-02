//! Isolated browser PoC host: three composition-hosted WebView2 surfaces
//! driven by an external Playwright runner over CDP.
//!
//! This is stage one of the browser AI fusion plan — a *harness*, not a product
//! surface. It reuses the production `BrowserView` (same composition hosting,
//! same input forwarding, and in phase two the same collaboration grants and
//! approvals) but starts nothing else: no daemon, no session store, no restored
//! window geometry, and never the daily WebView2 profile. It is compiled only
//! behind the `browser-poc` feature, and the `--browser-poc` flag is
//! intercepted in `main` before the application boots, so a normal build can
//! neither reach this path nor inherit its behavior.
//!
//! # Failure modes, and what this host does about each
//!
//! - **Bad arguments**: unknown flags, duplicates, a zero port, a non-loopback
//!   fixture origin, a relative artifact directory, a malformed run id, or a
//!   half-configured bridge all abort before a window opens. Nothing is
//!   created on a bad parse.
//! - **Inherited WebView2 environment**: `WEBVIEW2_USER_DATA_FOLDER` or
//!   `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` in the environment would silently
//!   redirect the profile or add browser flags. They are rejected up front, and
//!   the environment is never mutated.
//! - **Profile reuse**: the WebView2 profile directory is created exclusively
//!   (`create_dir` fails when it exists). A reused profile would mean driving
//!   the daily profile — or a previous run's — so that is a hard error, not a
//!   fallback.
//! - **Asynchronous host startup**: the composition controller completes on a
//!   posted message. Until it lands, a navigation request parks as the view's
//!   pending URL and replays from `webview_ready`. A page that never becomes
//!   ready within the startup deadline is a failed run: the error is recorded
//!   in `pages[].error` plus `fatal`, the final state is written, and the host
//!   exits non-zero so the runner cannot mistake a hang for success.
//! - **Half-written control.json**: a read that yields nothing parseable is
//!   retried on the next tick; the runner rewrites the file. Every request id
//!   is applied exactly once and acknowledged through `lastControlId`.
//! - **Half-written host-state.json**: state is serialized to a unique
//!   temporary file in the same directory and renamed into place. The
//!   destination is never truncated first — a polling runner sees the previous
//!   state or the new one. Rust's rename maps to `MoveFileExW` with
//!   `MOVEFILE_REPLACE_EXISTING` on Windows, so replacing an existing state
//!   file is supported; if a reader holds it open the rename fails, the error
//!   is logged, and the next tick retries.
//! - **Re-entrancy**: the poll loop never holds GPUI state across an await.
//!   Entity updates go through `WeakEntity::update_in`, and a root that is
//!   already gone ends the loop instead of panicking.
//! - **Leaked loop**: the poll task is stored on the root entity (never
//!   detached), so closing the window cancels it rather than leaving a 250 ms
//!   timer spinning forever.
//! - **Masked focus behavior**: nothing here force-restores focus to hide what
//!   Playwright actually did to the native first responder. The one sanctioned
//!   restore is the explicit `focus-gpui` control command: it reclaims the
//!   native keyboard from every page and moves GPUI focus onto the sentinel.
//!   Because the native reclaim is deferred by the view, the confirmation
//!   lands in a later `host-state.json`, never in the one written alongside
//!   the command.
//! - **Render-path I/O**: `render` reads only entity-owned memory. All file
//!   work runs on the background executor inside the poll loop, and the loop
//!   notifies only when a control request actually changed state — the 250 ms
//!   tick is a poll, never a render clock.
//! - **Bridge disconnect**: a daemon socket that ends drops every lease this
//!   host published and answers its in-flight requests with an error. Nothing
//!   is republished on its own, and a reconnect starts with no grants at all,
//!   so no lease can be inherited by a second connection.
//! - **Spoofed answers**: a completion is only accepted from the connection
//!   that published the page. A second client's `BrowserResult` for the same
//!   request id is refused by the daemon, not merely ignored here.
//! - **Explicit takeover**: `focus-page` drives ordinary native focus without
//!   withdrawing sharing. `take-over-page` calls the same stop command as the
//!   browser toolbar. `share-automatic-page`
//!   exercises only the native adapter; it does not simulate a product
//!   launcher, FullAccess picker, or complete application acceptance.
//!
//! # Contract with the runner
//!
//! The runner owns `control.json` and polls `host-state.json` in the artifact
//! directory it created:
//!
//! ```json
//! {"requestId": "uuid", "action": "focus-gpui" | "close-page" | "shutdown"
//!  | "share-page" | "revoke-page" | "approve-browser" | "reject-browser"
//!  | "cancel-browser" | "bridge-disconnect" | "bridge-connect"
//!  | "share-automatic-page" | "manual-browser-mode" | "focus-page",
//!  "pageId": "alpha", "grantId": "uuid", "browserRequestId": "uuid"}
//! ```
//!
//! Phase two adds the four bridge flags, which are all-or-nothing:
//! `--bridge-address`, `--bridge-token`, `--bridge-session` and
//! `--bridge-runtime`. Without them the host behaves exactly like phase one.
//!
//! ```json
//! {"runId": "...", "pid": 0, "startedAtUnixMs": 0, "profile": "...", "cdpPort": 0,
//!  "fixtureOrigin": "...", "artifactDir": "...", "lastControlId": "...",
//!  "gpuiFocused": false, "fatal": null,
//!  "browserPages": [{"page": "alpha", "pageId": "uuid"}],
//!  "browserShares": [{"page": "alpha", "scope": {...}, "url": "...", "title": "..."}],
//!  "pendingBrowserRequests": [{"page": "alpha", "requestId": "uuid", "action": "click", "detail": "#count"}],
//!  "bridge": {"configured": true, "connected": true, "sessionId": "uuid", "runtimeId": "uuid", "error": null},
//!  "pages": [{"id": "alpha", "url": null, "title": null, "loading": false,
//!             "ready": false, "error": null, "nativeFocused": false,
//!             "nativeFocusGains": 0, "nativeVisible": false, "nativeBounds": null}]}
//! ```

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use crossbeam_channel::Receiver;
use gpui::{
    App, AppContext, AsyncApp, Bounds, Context, Entity, FocusHandle, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, WeakEntity, Window, WindowBounds, WindowOptions, canvas, div, point, px,
    rgb, size,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::assets::{self, Assets};
use crate::browser::{
    BrowserCollaborationEvent, BrowserPocEnvironment, BrowserPocPageState, BrowserView,
};
use fintwind_client::{BrowserNotification, DaemonClient};
use fintwind_protocol::browser::{BrowserAction, BrowserScope, BrowserShare};

/// The three page ids, fixed for the whole run so a runner can address them
/// without discovery.
const PAGE_IDS: [&str; 3] = ["alpha", "beta", "gamma"];

/// One poll of the control protocol per tick. This is deliberately slow: the
/// runner writes control and polls state, so nothing here is latency-critical.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long a page may take to produce its composition host before the run is
/// declared failed. WebView2 controller creation normally lands within a few
/// frames; twenty seconds is generous without hanging the runner.
const HOST_STARTUP_DEADLINE: Duration = Duration::from_secs(20);
/// Also expire a host whose runner was forcibly terminated and cannot clean up.
const HOST_RUN_DEADLINE: Duration = Duration::from_secs(600);

/// The WebView2 profile lives inside the runner-owned artifact directory, one
/// fresh directory per run. It is never configurable and never shared.
const PROFILE_DIRECTORY: &str = "profile";

/// One bridge poll per tick. Browser traffic is live-only and low-rate, so a
/// 150 ms poll is a transfer latency, never a render clock.
const BRIDGE_POLL_INTERVAL: Duration = Duration::from_millis(150);

const CONTROL_FILE: &str = "control.json";
const HOST_STATE_FILE: &str = "host-state.json";
const SENTINEL_LABEL: &str = "GPUI focus sentinel";

const WINDOW_WIDTH: f32 = 1200.0;
const WINDOW_HEIGHT: f32 = 800.0;

/// Distinguishes one state write's temporary file from every other's, so a
/// leftover tmp from a crashed write is never mistaken for the current one.
static HOST_STATE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Boot the isolated PoC host. `args` are the flags *after* `--browser-poc`.
pub fn try_run_browser_poc(args: &[String]) -> Result<()> {
    reject_inherited_webview2_environment()?;
    let host = BrowserPocHost::from_args(args)?;
    // The bridge connects before any window exists: it is a plain socket with
    // no render or GPUI involvement, so a bad address or token fails the run
    // here instead of surfacing later as a missing share.
    if let Some(bridge) = host.bridge.as_ref() {
        bridge.connect();
        if !bridge.connected.load(Ordering::SeqCst) {
            let error = bridge
                .error
                .lock()
                .clone()
                .unwrap_or_else(|| "the bridge did not connect".to_owned());
            bail!("could not connect the browser bridge: {error}");
        }
    }
    let failure = host.failure.clone();

    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            // The same minimal startup the application performs, minus every
            // persistent key binding: this host owns no shortcuts.
            assets::register_fonts(cx).expect("failed to register bundled fonts");
            crate::theme::init(cx);
            crate::platform::init_reduce_motion(cx);
            crate::input::init(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(140.0), px(80.0)),
                            size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
                        ))),
                        ..Default::default()
                    },
                    move |window, cx| cx.new(|cx| BrowserPocRoot::new(host, window, cx)),
                )
                .expect("failed to open the browser poc window");
            window
                .update(cx, |_, window, cx| {
                    window.activate_window();
                    cx.activate(true);
                })
                .ok();
        });

    if failure.load(Ordering::SeqCst) {
        bail!("the browser poc host failed; see host-state.json and host.stderr.log");
    }
    Ok(())
}

/// Everything the host needs for one run, shared by reference between the root
/// entity and the poll loop.
#[derive(Clone)]
struct BrowserPocHost {
    environment: BrowserPocEnvironment,
    fixture_origin: String,
    artifact_dir: PathBuf,
    run_id: String,
    started_at: Instant,
    started_at_unix_ms: u64,
    /// Set when a run must fail the process; read after the app exits.
    failure: Arc<AtomicBool>,
    /// The daemon bridge, when the runner asked for phase two. `None` keeps
    /// phase one exactly as it was.
    bridge: Option<Arc<BrowserBridge>>,
}

/// The daemon side of the phase-two bridge.
///
/// The client owns its socket thread, so this handle is shared across the
/// GPUI thread and the root's background poll task. Everything that can be
/// replaced (the client, its notification receiver) is behind a mutex, so a
/// reconnect creates a fresh subscription instead of a second one.
struct BrowserBridge {
    address: String,
    token: String,
    session_id: Uuid,
    runtime_id: Mutex<Uuid>,
    client: Mutex<Option<DaemonClient>>,
    notifications: Mutex<Option<Receiver<BrowserNotification>>>,
    connected: AtomicBool,
    connecting: AtomicBool,
    error: Mutex<Option<String>>,
    pending: Mutex<HashMap<Uuid, DaemonClient>>,
}

impl BrowserBridge {
    fn connect(self: &Arc<Self>) {
        if self.connected.load(Ordering::SeqCst)
            || self
                .connecting
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return;
        }
        match DaemonClient::connect(&self.address, self.token.clone()) {
            Ok(client) => {
                // One consumer only: a second subscribe replaces the first
                // instead of fanning the same request out twice.
                let receiver = client.subscribe_browser_requests();
                *self.client.lock() = Some(client);
                *self.notifications.lock() = Some(receiver);
                self.connected.store(true, Ordering::SeqCst);
                *self.error.lock() = None;
                eprintln!("[browser-poc] bridge connected to {}", self.address);
            }
            Err(error) => {
                self.connected.store(false, Ordering::SeqCst);
                *self.error.lock() = Some(error.to_string());
                eprintln!("[browser-poc] bridge connect failed: {error:#}");
            }
        }
        self.connecting.store(false, Ordering::SeqCst);
    }

    fn disconnect(&self) {
        if let Some(client) = self.client.lock().take() {
            client.disconnect();
        }
        *self.notifications.lock() = None;
        self.connected.store(false, Ordering::SeqCst);
    }

    fn publish(&self, pages: Vec<BrowserShare>) -> bool {
        let Some(client) = self.client.lock().clone() else {
            // Nothing is connected, so nothing is shared: say so rather than
            // leaving a page claiming a grant nobody holds.
            return false;
        };
        if let Err(error) = client.publish_browser_pages(pages) {
            // The error carries the daemon's reason only; the bridge token
            // never appears in this log.
            eprintln!("[browser-poc] publish refused by the daemon: {error:#}");
            *self.error.lock() = Some(error.to_string());
            return false;
        }
        true
    }

    fn complete(&self, request_id: Uuid, result: fintwind_protocol::browser::BrowserResult) {
        let Some(client) = self.pending.lock().remove(&request_id) else {
            return;
        };
        if let Err(error) = client.complete_browser_request(request_id, result) {
            eprintln!("[browser-poc] bridge completion failed: {error:#}");
            *self.error.lock() = Some(error.to_string());
        }
    }

    fn cancel(&self, request_id: Uuid) {
        let Some(client) = self.client.lock().clone() else {
            return;
        };
        if let Err(error) = client.cancel_browser_request(request_id) {
            eprintln!("[browser-poc] bridge cancel failed: {error:#}");
            *self.error.lock() = Some(error.to_string());
        }
    }

    /// Take one pending notification, if any. Called only from the root's
    /// background task, so the single-consumer receiver stays single.
    fn take_notification(&self) -> Option<BrowserNotification> {
        let receiver = self.notifications.lock().clone()?;
        match receiver.try_recv() {
            Ok(notification) => Some(notification),
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.notifications.lock().take();
                Some(BrowserNotification::Disconnected)
            }
        }
    }
}

impl BrowserPocHost {
    fn from_args(args: &[String]) -> Result<Self> {
        let (cdp_port, fixture_origin, artifact_dir, run_id, bridge) = parse_arguments(args)?;
        let profile = create_isolated_profile(&artifact_dir)?;
        let started_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since_epoch| since_epoch.as_millis() as u64)
            .unwrap_or_default();
        Ok(Self {
            environment: BrowserPocEnvironment { profile, cdp_port },
            fixture_origin,
            artifact_dir,
            run_id,
            started_at: Instant::now(),
            started_at_unix_ms,
            failure: Arc::new(AtomicBool::new(false)),
            bridge,
        })
    }
}

/// Reject inherited WebView2 settings: `WEBVIEW2_USER_DATA_FOLDER` and
/// `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` (among others) would override the
/// isolated profile or inject browser flags, and neither can be un-set safely
/// from inside the process. The check is case-insensitive because Windows
/// environment variables are.
fn reject_inherited_webview2_environment() -> Result<()> {
    const PREFIX: &str = "WEBVIEW2_";
    for (key, _value) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key
            .get(..PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
        {
            bail!(
                "the {key} environment variable is set: inherited WebView2 \
                 settings could redirect the isolated profile or add browser \
                 flags, so this host refuses to start"
            );
        }
    }
    Ok(())
}

/// Accept the four phase-one flags plus the phase-two bridge flags. Every flag
/// is optional on its own except the original four, and the bridge flags are
/// all-or-nothing: a half-configured bridge is refused rather than half-used.
fn parse_arguments(
    args: &[String],
) -> Result<(u16, String, PathBuf, String, Option<Arc<BrowserBridge>>)> {
    let mut cdp_port = None;
    let mut fixture_origin = None;
    let mut artifact_dir = None;
    let mut run_id = None;
    let mut bridge_address = None;
    let mut bridge_token = None;
    let mut bridge_session = None;
    let mut bridge_runtime = None;

    for arg in args {
        let Some((flag, value)) = arg.split_once('=') else {
            bail!("unexpected argument {arg:?}: every flag needs an =value");
        };
        match flag {
            "--cdp-port" => {
                if cdp_port.is_some() {
                    bail!("duplicate --cdp-port");
                }
                let port: u16 = value
                    .parse()
                    .map_err(|_| anyhow!("--cdp-port wants a port number, got {value:?}"))?;
                if port == 0 {
                    bail!("--cdp-port must be nonzero");
                }
                cdp_port = Some(port);
            }
            "--fixture-origin" => {
                if fixture_origin.is_some() {
                    bail!("duplicate --fixture-origin");
                }
                fixture_origin = Some(parse_fixture_origin(value)?);
            }
            "--artifact-dir" => {
                if artifact_dir.is_some() {
                    bail!("duplicate --artifact-dir");
                }
                artifact_dir = Some(parse_artifact_dir(value)?);
            }
            "--run-id" => {
                if run_id.is_some() {
                    bail!("duplicate --run-id");
                }
                run_id = Some(parse_run_id(value)?);
            }
            // Phase two. The bridge is only reachable in this build, only over
            // loopback, and only with the token the daemon printed for this
            // run; nothing here can select the user's daemon or the service.
            "--bridge-address" => {
                if bridge_address.is_some() {
                    bail!("duplicate --bridge-address");
                }
                bridge_address = Some(parse_bridge_address(value)?);
            }
            "--bridge-token" => {
                if bridge_token.is_some() {
                    bail!("duplicate --bridge-token");
                }
                if value.trim().is_empty() {
                    bail!("--bridge-token must not be empty");
                }
                bridge_token = Some(value.to_owned());
            }
            "--bridge-session" => {
                if bridge_session.is_some() {
                    bail!("duplicate --bridge-session");
                }
                bridge_session = Some(parse_uuid_flag(value, "--bridge-session")?);
            }
            "--bridge-runtime" => {
                if bridge_runtime.is_some() {
                    bail!("duplicate --bridge-runtime");
                }
                bridge_runtime = Some(parse_uuid_flag(value, "--bridge-runtime")?);
            }
            _ => bail!("unknown flag {flag:?}"),
        }
    }

    let (Some(cdp_port), Some(fixture_origin), Some(artifact_dir), Some(run_id)) =
        (cdp_port, fixture_origin, artifact_dir, run_id)
    else {
        bail!(
            "missing required flags: --cdp-port, --fixture-origin, \
             --artifact-dir and --run-id are all required"
        );
    };
    let bridge = match (bridge_address, bridge_token, bridge_session, bridge_runtime) {
        (None, None, None, None) => None,
        (Some(address), Some(token), Some(session_id), Some(runtime_id)) => {
            Some(Arc::new(BrowserBridge {
                address,
                token,
                session_id,
                runtime_id: Mutex::new(runtime_id),
                client: Mutex::new(None),
                notifications: Mutex::new(None),
                connected: AtomicBool::new(false),
                connecting: AtomicBool::new(false),
                error: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
            }))
        }
        _ => bail!(
            "the bridge flags are all-or-nothing: --bridge-address, --bridge-token, \
             --bridge-session and --bridge-runtime must be given together"
        ),
    };
    Ok((cdp_port, fixture_origin, artifact_dir, run_id, bridge))
}

/// The bridge address must be a plain loopback HTTP address: no credentials,
/// no path, no query, no fragment, and never a non-loopback host.
fn parse_bridge_address(raw: &str) -> Result<String> {
    let url =
        Url::parse(raw).with_context(|| format!("--bridge-address {raw:?} is not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "ws") {
        bail!(
            "--bridge-address must be http or ws, got scheme {:?}",
            url.scheme()
        );
    }
    if !matches!(url.host_str(), Some("127.0.0.1") | Some("localhost")) {
        bail!(
            "--bridge-address must be loopback, got host {:?}",
            url.host_str()
        );
    }
    if url.port().is_none() {
        bail!("--bridge-address needs an explicit port");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("--bridge-address must not carry credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("--bridge-address must not carry a query or a fragment");
    }
    if url.path() != "/" && !url.path().is_empty() {
        bail!(
            "--bridge-address must not carry a path, got {:?}",
            url.path()
        );
    }
    let port = url
        .port()
        .expect("the bridge address was checked for a port");
    Ok(format!("ws://127.0.0.1:{port}"))
}

fn parse_uuid_flag(raw: &str, flag: &str) -> Result<Uuid> {
    let id = Uuid::parse_str(raw).with_context(|| format!("{flag} {raw:?} is not a uuid"))?;
    if id.is_nil() {
        bail!("{flag} must not be nil");
    }
    Ok(id)
}

/// The fixture origin must be plain HTTP on loopback with nothing but a port:
/// no credentials, no path, no query, no fragment, and no other host.
fn parse_fixture_origin(raw: &str) -> Result<String> {
    let url =
        Url::parse(raw).with_context(|| format!("--fixture-origin {raw:?} is not a valid URL"))?;
    if url.scheme() != "http" {
        bail!(
            "--fixture-origin must be http, got scheme {:?}",
            url.scheme()
        );
    }
    if url.host_str() != Some("127.0.0.1") {
        bail!(
            "--fixture-origin must be 127.0.0.1, got host {:?}",
            url.host_str()
        );
    }
    if url.port().is_none() {
        bail!("--fixture-origin needs an explicit port");
    }
    let path = url.path();
    if !path.is_empty() && path != "/" {
        bail!("--fixture-origin must not carry a path, got {path:?}");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("--fixture-origin must not carry a query or a fragment");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("--fixture-origin must not carry credentials");
    }
    Ok(url.origin().ascii_serialization())
}

/// The runner creates the artifact directory before launching the host; the
/// host only verifies it and canonicalizes it so the profile path and the
/// state file paths are stable, absolute paths.
fn parse_artifact_dir(raw: &str) -> Result<PathBuf> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        bail!("--artifact-dir must be an absolute path, got {raw:?}");
    }
    let canonical = std::fs::canonicalize(&path)
        .with_context(|| format!("--artifact-dir {} must already exist", path.display()))?;
    if !canonical.is_dir() {
        bail!("--artifact-dir {} is not a directory", canonical.display());
    }
    Ok(canonical)
}

fn parse_run_id(raw: &str) -> Result<String> {
    Uuid::parse_str(raw).with_context(|| format!("--run-id {raw:?} is not a uuid"))?;
    Ok(raw.to_owned())
}

/// Create this run's WebView2 profile exclusively. `create_dir` (not
/// `create_dir_all`) is the whole point: an existing directory is an error, so
/// a run can never join the daily profile or a previous run's.
fn create_isolated_profile(artifact_dir: &Path) -> Result<PathBuf> {
    let profile = artifact_dir.join(PROFILE_DIRECTORY);
    match std::fs::create_dir(&profile) {
        Ok(()) => Ok(profile),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => bail!(
            "the profile directory {} already exists: every run needs a fresh \
             artifact directory",
            profile.display()
        ),
        Err(error) => Err(anyhow!("could not create {}: {error}", profile.display())),
    }
}

/// The one window's root: three always-visible pages plus a focus sentinel.
struct BrowserPocRoot {
    host: BrowserPocHost,
    pages: Vec<PocPage>,
    focus_handle: FocusHandle,
    /// Whether the sentinel holds GPUI focus right now, read for the state file.
    gpui_focused: bool,
    /// The last control request id applied; it is also the runner's ack.
    last_control_id: Option<String>,
    /// Set when the startup deadline failed; written as `fatal`.
    deadline_failure: Option<String>,
    quit_requested: bool,
    render_count: u64,
    /// Set while a refused publish is being revoked, so the cascade of
    /// `ShareChanged` events cannot publish again into the same refusal.
    publish_settling: bool,
    _page_observers: Vec<Subscription>,
    /// Browser collaboration subscriptions, one per page. Held for the same
    /// reason as the poll loop: dropping the root cancels them.
    _browser_subscriptions: Vec<Subscription>,
    /// Held, never detached: dropping the root cancels the loop.
    _task: Task<()>,
    /// The bridge notification task, present only when a bridge was given.
    _bridge_task: Option<Task<()>>,
}

struct PocPage {
    id: &'static str,
    element_id: SharedString,
    entity: Entity<BrowserView>,
    /// Last polled summary, read by render only. Never read from the view.
    last_status: String,
    layout_bounds: Rc<Cell<Option<[f32; 4]>>>,
    /// The stable identity the daemon addresses this page by. Derived from
    /// the run id, so the runner can read it from the state file instead of
    /// guessing one.
    page_id: Uuid,
}

impl BrowserPocRoot {
    fn new(host: BrowserPocHost, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let mut pages = Vec::with_capacity(PAGE_IDS.len());
        let mut page_observers = Vec::with_capacity(PAGE_IDS.len());
        let mut browser_subscriptions = Vec::with_capacity(PAGE_IDS.len());
        for id in PAGE_IDS {
            let url = format!("{}/page/{id}?run={}", host.fixture_origin, host.run_id);
            let entity = cx.new(|cx| {
                // The composition host is asynchronous, so the navigation
                // parks as the view's pending URL until `webview_ready`
                // replays it. Sharing one profile across the three views is
                // the point: they are one browser with three windows on it.
                let mut view = BrowserView::new_for_poc(host.environment.clone(), window, cx);
                view.navigate_to_url(url, cx);
                view
            });
            // A controller may become ready after the initial root render.
            // Its notification must revisit the parent's native visibility
            // push too; a periodic state read is deliberately not a redraw.
            page_observers.push(cx.observe(&entity, |_, _, cx| cx.notify()));
            // Every share change and every finished operation is published to
            // the daemon from the root, so the daemon's view of the world is
            // always exactly what these three views hold.
            browser_subscriptions.push(cx.subscribe(
                &entity,
                move |root: &mut Self, _page, event: &BrowserCollaborationEvent, cx| {
                    root.on_browser_event(event, cx);
                },
            ));
            pages.push(PocPage {
                id,
                element_id: SharedString::from(format!("browser-poc-page-{id}")),
                entity,
                last_status: "starting host".to_owned(),
                layout_bounds: Rc::new(Cell::new(None)),
                page_id: deterministic_uuid(&format!(
                    "fintwind/browser-collaboration/{}/{}",
                    host.run_id, id
                )),
            });
        }

        // `spawn` (not `spawn_in`): the loop's file work needs the background
        // executor, which `AsyncWindowContext` does not expose.
        let loop_host = host.clone();
        let task = cx.spawn(async move |this, cx| {
            Self::control_loop(this, cx, loop_host).await;
        });

        // The bridge task only exists when a bridge was configured, and it is
        // held by the root like the control loop: closing the window cancels
        // it instead of leaving a timer spinning behind a dead entity.
        let bridge_task = host.bridge.as_ref().map(|bridge| {
            let bridge = Arc::clone(bridge);
            cx.spawn(async move |this, cx| {
                Self::bridge_loop(this, cx, bridge).await;
            })
        });

        Self {
            host,
            pages,
            focus_handle,
            gpui_focused: false,
            last_control_id: None,
            deadline_failure: None,
            quit_requested: false,
            render_count: 0,
            publish_settling: false,
            _page_observers: page_observers,
            _browser_subscriptions: browser_subscriptions,
            _task: task,
            _bridge_task: bridge_task,
        }
    }

    /// One batch of daemon notifications. A request is routed to the
    /// page whose share matches its scope exactly; a mismatch or a missing
    /// page answers with an error instead of guessing another page.
    async fn bridge_loop(this: WeakEntity<Self>, cx: &mut AsyncApp, bridge: Arc<BrowserBridge>) {
        loop {
            cx.background_executor().timer(BRIDGE_POLL_INTERVAL).await;
            let mut drained = Vec::new();
            for _ in 0..128 {
                match bridge.take_notification() {
                    Some(notification) => drained.push(notification),
                    None => break,
                }
            }
            if drained.is_empty() {
                continue;
            }
            let bridge = Arc::clone(&bridge);
            let applied = this.update_in(cx, |root, _window, cx| {
                root.apply_bridge_notifications(drained, &bridge, cx)
            });
            if applied.is_err() {
                // The root entity is gone: its window closed, so nothing
                // serves these requests anymore.
                break;
            }
        }
    }

    /// One poll per tick: read control in the background, apply and collect
    /// state in one foreground update, write state in the background.
    async fn control_loop(this: WeakEntity<Self>, cx: &mut AsyncApp, host: BrowserPocHost) {
        loop {
            cx.background_executor().timer(POLL_INTERVAL).await;

            let read_directory = host.artifact_dir.clone();
            let control = cx
                .background_executor()
                .spawn(async move { read_control(&read_directory) })
                .await;

            let Ok((state, shutdown)) = this.update_in(cx, |root, window, cx| {
                root.apply_control(control, window, cx)
            }) else {
                // The root entity is gone: its window closed, so nothing
                // drives the pages anymore.
                break;
            };

            // Written every tick, control or not: the runner polls state and
            // must never have to wait for a change to see one.
            let write_directory = host.artifact_dir.clone();
            if let Err(error) = cx
                .background_executor()
                .spawn(async move { write_host_state(&write_directory, &state) })
                .await
            {
                eprintln!("[browser-poc] could not write {HOST_STATE_FILE}: {error}");
            }

            if shutdown {
                cx.update(|cx| cx.quit());
                break;
            }
        }
    }

    /// Apply the newest control request (exactly once per request id), then
    /// collect the state the runner polls. Notifies only when a request
    /// actually changed something.
    fn apply_control(
        &mut self,
        control: Option<ControlFile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (HostState, bool) {
        let mut changed = false;
        if let Some(control) = control {
            let is_new = self.last_control_id.as_deref() != Some(control.request_id.as_str());
            if is_new {
                // Acknowledge first: an action runs at most once even when it
                // fails halfway (an unknown page id still consumes its id).
                self.last_control_id = Some(control.request_id.clone());
                changed = true;
                self.run_control_action(&control, window, cx);
            }
        }

        let mut pages = Vec::with_capacity(self.pages.len());
        for index in 0..self.pages.len() {
            let id = self.pages[index].id;
            let entity = self.pages[index].entity.clone();
            let state = entity.read_with(cx, |view, _| view.poc_state(id));
            self.pages[index].last_status = summarize_page(&state);
            pages.push(state);
        }

        self.gpui_focused = window.focused(cx).as_ref() == Some(&self.focus_handle);

        let mut shutdown = self.quit_requested;
        if self.deadline_failure.is_none() && self.host.started_at.elapsed() >= HOST_RUN_DEADLINE {
            self.deadline_failure =
                Some("isolated browser run exceeded its ten-minute lifetime".into());
            self.host.failure.store(true, Ordering::SeqCst);
            shutdown = true;
        }
        if self.deadline_failure.is_none()
            && self.host.started_at.elapsed() >= HOST_STARTUP_DEADLINE
            && pages.iter().any(|page| !page.ready)
        {
            let unready: Vec<&str> = pages
                .iter()
                .filter(|page| !page.ready)
                .map(|page| page.id)
                .collect();
            let message = format!(
                "pages [{}] never became ready within {}s",
                unready.join(", "),
                HOST_STARTUP_DEADLINE.as_secs()
            );
            eprintln!("[browser-poc] {message}");
            self.deadline_failure = Some(message);
            self.host.failure.store(true, Ordering::SeqCst);
            shutdown = true;
        }

        if changed {
            cx.notify();
        }

        let state = HostState {
            run_id: self.host.run_id.clone(),
            pid: std::process::id(),
            started_at_unix_ms: self.host.started_at_unix_ms,
            profile: self.host.environment.profile.clone(),
            cdp_port: self.host.environment.cdp_port,
            fixture_origin: self.host.fixture_origin.clone(),
            artifact_dir: self.host.artifact_dir.clone(),
            last_control_id: self.last_control_id.clone(),
            gpui_focused: self.gpui_focused,
            fatal: self.deadline_failure.clone(),
            render_count: self.render_count,
            layout_bounds: self
                .pages
                .iter()
                .map(|page| (page.id, page.layout_bounds.get()))
                .collect(),
            pages,
            browser_pages: self
                .pages
                .iter()
                .map(|page| BrowserPageIdentity {
                    page: page.id,
                    page_id: page.page_id,
                })
                .collect(),
            browser_shares: self.collect_browser_shares(cx),
            pending_browser_requests: self.collect_pending_browser_requests(cx),
            bridge: self.host.bridge.as_ref().map(|bridge| HostBridgeState {
                configured: true,
                connected: bridge.connected.load(Ordering::SeqCst),
                session_id: Some(bridge.session_id),
                runtime_id: Some(*bridge.runtime_id.lock()),
                error: bridge.error.lock().clone(),
            }),
        };
        (state, shutdown)
    }

    /// Exactly what the live views hold, labelled with the page it came from.
    /// This is the same data the host publishes to the daemon, so a runner can
    /// see a disagreement between the two as a failure rather than infer it.
    fn collect_browser_shares(&self, cx: &Context<Self>) -> Vec<HostBrowserShare> {
        self.pages
            .iter()
            .filter_map(|page| {
                let share = page.entity.read_with(cx, |view, _| view.browser_share())?;
                Some(HostBrowserShare {
                    page: page.id,
                    scope: share.scope,
                    url: share.url,
                    title: share.title,
                })
            })
            .collect()
    }

    fn collect_pending_browser_requests(
        &self,
        cx: &Context<Self>,
    ) -> Vec<HostPendingBrowserRequest> {
        self.pages
            .iter()
            .filter_map(|page| {
                let request = page
                    .entity
                    .read_with(cx, |view, _| view.pending_browser_request())?;
                let (action, detail) = match &request.action {
                    BrowserAction::Snapshot => ("snapshot".to_owned(), String::new()),
                    BrowserAction::Click { selector } => ("click".to_owned(), selector.clone()),
                    BrowserAction::Fill { selector, text } => (
                        "fill".to_owned(),
                        format!("{selector} -> {} chars", text.chars().count()),
                    ),
                    BrowserAction::Navigate { url } => ("navigate".to_owned(), url.clone()),
                    BrowserAction::Open { url } => ("open".to_owned(), url.clone()),
                    BrowserAction::Scroll { delta_y } => ("scroll".to_owned(), delta_y.to_string()),
                    BrowserAction::Screenshot { full_page } => {
                        ("screenshot".to_owned(), full_page.to_string())
                    }
                    BrowserAction::Evaluate { expression } => {
                        ("evaluate".to_owned(), truncate_detail(expression))
                    }
                    BrowserAction::ClickAt { x, y } => ("clickAt".to_owned(), format!("{x},{y}")),
                    BrowserAction::DoubleClick { selector } => {
                        ("doubleClick".to_owned(), selector.clone())
                    }
                    BrowserAction::Press { selector, key } => {
                        ("press".to_owned(), format!("{selector} {key}"))
                    }
                    BrowserAction::Hover { selector } => ("hover".to_owned(), selector.clone()),
                    BrowserAction::Select { selector, value } => {
                        ("select".to_owned(), format!("{selector} {value}"))
                    }
                    BrowserAction::Drag { from, to } => ("drag".to_owned(), format!("{from} {to}")),
                    BrowserAction::Close => ("close".to_owned(), String::new()),
                };
                Some(HostPendingBrowserRequest {
                    page: page.id,
                    request_id: request.request_id,
                    action,
                    detail,
                })
            })
            .collect()
    }

    /// Apply one batch of daemon notifications. A request is routed to the
    /// page whose share matches its scope exactly; a mismatch or a missing
    /// page answers with an error instead of guessing another page.
    fn apply_bridge_notifications(
        &mut self,
        notifications: Vec<BrowserNotification>,
        bridge: &Arc<BrowserBridge>,
        cx: &mut Context<Self>,
    ) {
        for notification in notifications {
            match notification {
                BrowserNotification::Request(request) => {
                    if let Some(client) = bridge.client.lock().clone() {
                        bridge.pending.lock().insert(request.request_id, client);
                    }
                    let target = self.pages.iter().find(|page| {
                        page.entity.read_with(cx, |view, _| {
                            view.browser_share()
                                .is_some_and(|share| share.scope == request.scope)
                        })
                    });
                    match target {
                        Some(page) => {
                            let entity = page.entity.clone();
                            entity.update(cx, |view, cx| view.handle_browser_request(request, cx));
                        }
                        None => bridge.complete(
                            request.request_id,
                            fintwind_protocol::browser::BrowserResult::error(
                                "this host has no live page shared for that scope",
                            ),
                        ),
                    }
                }
                BrowserNotification::Cancel(request_id) => {
                    bridge.pending.lock().remove(&request_id);
                    for page in &self.pages {
                        let entity = page.entity.clone();
                        entity.update(cx, |view, cx| view.cancel_browser_request(request_id, cx));
                    }
                }
                // The daemon refused this publish (over the page limit, a page
                // another connection already owns, or a runtime that is not
                // live). The pages it named are not shared, so the UI must not
                // keep claiming they are.
                BrowserNotification::ShareRejected { scopes, message } => {
                    eprintln!("[browser-poc] publish refused by the daemon: {message}");
                    // Only the daemon's message is logged; never the token.
                    self.revoke_matching(&scopes, cx);
                }
                // The daemon dropped these grants: the runtime was replaced, or
                // the session's work moved elsewhere. Match each scope to the
                // page that currently holds it, so a page that was re-shared
                // under a new grant is not revoked by an older one.
                BrowserNotification::ScopesRevoked(scopes) => {
                    self.revoke_matching(&scopes, cx);
                }
                BrowserNotification::Disconnected => {
                    // Every grant died with the connection. Nothing is
                    // republished until the user shares a page again.
                    bridge.disconnect();
                    for page in &self.pages {
                        let entity = page.entity.clone();
                        entity.update(cx, |view, cx| view.revoke_browser_share(cx));
                    }
                    bridge.pending.lock().clear();
                }
            }
        }
        cx.notify();
    }

    /// Revoke only the pages whose *current* share matches one of `scopes`.
    /// A page that has since been re-shared under a new grant keeps it.
    fn revoke_matching(&mut self, scopes: &[BrowserScope], cx: &mut Context<Self>) {
        for index in 0..self.pages.len() {
            let entity = self.pages[index].entity.clone();
            let matches = entity.read_with(cx, |view, _| {
                view.browser_share()
                    .is_some_and(|share| scopes.contains(&share.scope))
            });
            if matches {
                entity.update(cx, |view, cx| view.revoke_browser_share(cx));
            }
        }
    }

    /// Publish exactly the shares the live views currently hold. Publishing
    /// replaces this connection's whole set, so closing a page or revoking a
    /// grant reaches the daemon on the next call rather than lingering.
    ///
    /// A refused publish is not a silent no-op: the pages it named are revoked
    /// here too, so the UI never claims a share the daemon does not hold. The
    /// revocation itself emits `ShareChanged`, so a flag keeps that from
    /// republishing into the same refusal and looping.
    fn publish_browser_shares(&mut self, cx: &mut Context<Self>) {
        let Some(bridge) = self.host.bridge.as_ref() else {
            return;
        };
        if !bridge.connected.load(Ordering::SeqCst) || self.publish_settling {
            return;
        }
        let shares: Vec<BrowserShare> = self
            .pages
            .iter()
            .filter_map(|page| page.entity.read_with(cx, |view, _| view.browser_share()))
            .collect();
        if bridge.publish(shares) {
            return;
        }
        // The daemon did not accept this set. Revoke the matching pages while
        // the settling flag stops the cascade, then let the next user action
        // publish again.
        let scopes: Vec<BrowserScope> = self
            .pages
            .iter()
            .filter_map(|page| {
                page.entity
                    .read_with(cx, |view, _| view.browser_share().map(|share| share.scope))
            })
            .collect();
        if scopes.is_empty() {
            return;
        }
        self.publish_settling = true;
        self.revoke_matching(&scopes, cx);
        self.publish_settling = false;
    }

    /// One event per page: a share changed, or one operation finished.
    fn on_browser_event(&mut self, event: &BrowserCollaborationEvent, cx: &mut Context<Self>) {
        match event {
            BrowserCollaborationEvent::ShareRequested => {}
            BrowserCollaborationEvent::ShareChanged => self.publish_browser_shares(cx),
            BrowserCollaborationEvent::Finished { request_id, result } => {
                // The completion rides back on the same connection, so a
                // second client cannot answer for this page.
                if let Some(bridge) = self.host.bridge.as_ref() {
                    bridge.complete(*request_id, result.clone());
                }
                self.publish_browser_shares(cx);
            }
        }
        cx.notify();
    }

    fn run_control_action(
        &mut self,
        control: &ControlFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match control.action.as_str() {
            "focus-gpui" => {
                // The one sanctioned focus restore: reclaim the native
                // keyboard from every page, then hand GPUI focus to the
                // sentinel. The native reclaim is deferred by the view, so
                // the confirmation lands in a later state write.
                for page in &self.pages {
                    let entity = page.entity.clone();
                    entity.update(cx, |view, cx| view.poc_reclaim_keyboard(cx));
                }
                window.focus(&self.focus_handle, cx);
            }
            "focus-page" => {
                if let Some(index) = self.page_index(control.page_id.as_deref()) {
                    // Exercise ordinary native focus, not a direct grant
                    // revocation or a synthesized DOM focus event.
                    self.pages[index].entity.clone().update(cx, |view, cx| {
                        view.focus_default(window, cx);
                    });
                }
            }
            "close-page" => match control.page_id.as_deref() {
                Some(page_id) => {
                    if let Some(index) = self.pages.iter().position(|page| page.id == page_id) {
                        // Dropping the root's only strong reference releases
                        // the view, and the host's `Drop` closes the native
                        // controller. No raw CDP page close is sent.
                        self.pages.remove(index);
                        // The closed page's grant is gone from this
                        // connection; the daemon must learn that now rather
                        // than routing into a page nobody owns.
                        self.publish_browser_shares(cx);
                    } else {
                        eprintln!("[browser-poc] close-page: no page {page_id:?}");
                    }
                }
                None => eprintln!("[browser-poc] close-page: the control has no pageId"),
            },
            // Phase two: an explicit human share of one page. The grant id is
            // chosen by the runner, so "share again" is a genuinely new lease
            // and the previous one cannot be reused.
            "share-page" | "share-automatic-page" => {
                let Some(grant_id) = control
                    .grant_id
                    .as_deref()
                    .and_then(|grant_id| Uuid::parse_str(grant_id).ok())
                else {
                    eprintln!("[browser-poc] share-page: the control needs a uuid grantId");
                    return;
                };
                let Some(bridge) = self.host.bridge.as_ref() else {
                    eprintln!("[browser-poc] share-page: no bridge was configured");
                    return;
                };
                let Some(index) = self.page_index(control.page_id.as_deref()) else {
                    eprintln!("[browser-poc] share-page: no page {:?}", control.page_id);
                    return;
                };
                if let Some(runtime_id) = control.runtime_id.as_deref() {
                    let Ok(runtime_id) = parse_uuid_flag(runtime_id, "runtimeId") else {
                        return;
                    };
                    *bridge.runtime_id.lock() = runtime_id;
                }
                let scope = BrowserScope {
                    session_id: bridge.session_id,
                    runtime_id: *bridge.runtime_id.lock(),
                    page_id: self.pages[index].page_id,
                    grant_id,
                };
                let entity = self.pages[index].entity.clone();
                entity.update(cx, |view, cx| {
                    if control.action == "share-automatic-page" {
                        view.begin_browser_automation(scope, cx);
                    } else {
                        view.begin_browser_share(scope, cx);
                    }
                });
            }
            "manual-browser-mode" => {
                if let Some(index) = self.page_index(control.page_id.as_deref()) {
                    self.pages[index]
                        .entity
                        .clone()
                        .update(cx, |view, cx| view.set_browser_automatic(false, cx));
                }
            }
            "take-over-page" | "revoke-page" => {
                let Some(index) = self.page_index(control.page_id.as_deref()) else {
                    eprintln!("[browser-poc] revoke-page: no page {:?}", control.page_id);
                    return;
                };
                let entity = self.pages[index].entity.clone();
                entity.update(cx, |view, cx| {
                    if control.action == "take-over-page" {
                        view.take_over_browser(cx);
                    } else {
                        view.revoke_browser_share(cx);
                    }
                });
                self.publish_browser_shares(cx);
            }
            // Approvals are ordinary control commands driven through the same
            // public view API the approval bar uses, so no test-only approval
            // back door exists.
            "approve-browser" => self.answer_browser_request(control, true, cx),
            "reject-browser" => self.answer_browser_request(control, false, cx),
            "cancel-browser" => match control
                .request_id_field
                .as_deref()
                .and_then(|id| Uuid::parse_str(id).ok())
            {
                Some(request_id) => {
                    if let Some(bridge) = self.host.bridge.as_ref() {
                        bridge.cancel(request_id);
                    }
                    for page in &self.pages {
                        let entity = page.entity.clone();
                        entity.update(cx, |view, cx| view.cancel_browser_request(request_id, cx));
                    }
                }
                None => eprintln!("[browser-poc] cancel-browser: the control has no requestId"),
            },
            "bridge-disconnect" => {
                // Close this host's real socket; never send daemon Shutdown.
                if let Some(bridge) = self.host.bridge.as_ref() {
                    bridge.disconnect();
                }
                for page in &self.pages {
                    let entity = page.entity.clone();
                    entity.update(cx, |view, cx| view.revoke_browser_share(cx));
                }
            }
            "bridge-connect" => {
                if let Some(bridge) = self.host.bridge.as_ref() {
                    let bridge = bridge.clone();
                    cx.background_executor()
                        .spawn(async move {
                            bridge.connect();
                        })
                        .detach();
                }
            }
            "shutdown" => self.quit_requested = true,
            other => eprintln!("[browser-poc] ignoring unknown control action {other:?}"),
        }
    }

    /// The index of the page with this id, or `None` when the control names a
    /// page that is not live (already closed, or never existed).
    fn page_index(&self, page_id: Option<&str>) -> Option<usize> {
        let page_id = page_id?;
        self.pages.iter().position(|page| page.id == page_id)
    }

    /// Approve or reject the pending request the runner names. Runs through
    /// the same view API the collaboration bar calls.
    fn answer_browser_request(
        &mut self,
        control: &ControlFile,
        approve: bool,
        cx: &mut Context<Self>,
    ) {
        let action = control.action.clone();
        let Some(request_id) = control
            .request_id_field
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok())
        else {
            eprintln!("[browser-poc] {action}: the control has no requestId");
            return;
        };
        for page in &self.pages {
            let entity = page.entity.clone();
            entity.update(cx, |view, cx| {
                if approve {
                    view.approve_browser_request(request_id, cx);
                } else {
                    view.reject_browser_request(request_id, cx);
                }
            });
        }
    }
}

impl Render for BrowserPocRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_count += 1;
        // Every page is always on screen: push the native views visible once
        // per frame from the top of render, exactly like the application does.
        for page in &self.pages {
            let entity = page.entity.clone();
            entity.update(cx, |view, cx| view.sync_native_state(true, false, cx));
        }

        div()
            .id("browser-poc-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x0f1115))
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .id("browser-poc-pages")
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .children(self.pages.iter().map(|page| {
                        let layout_bounds = page.layout_bounds.clone();
                        div()
                            .id(page.element_id.clone())
                            .flex_1()
                            .min_w_0()
                            .size_full()
                            .flex()
                            .flex_col()
                            .relative()
                            .border_r_1()
                            .border_color(rgb(0x272c36))
                            .child(
                                div()
                                    .px(px(8.0))
                                    .py(px(4.0))
                                    .text_size(px(11.0))
                                    .text_color(rgb(0x9aa3b2))
                                    .truncate()
                                    .child(format!("{} - {}", page.id, page.last_status)),
                            )
                            .child(page.entity.clone())
                            .child(
                                canvas(
                                    move |bounds, _, _| {
                                        layout_bounds.set(Some([
                                            f32::from(bounds.origin.x),
                                            f32::from(bounds.origin.y),
                                            f32::from(bounds.size.width),
                                            f32::from(bounds.size.height),
                                        ]));
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                    })),
            )
    }
}

impl BrowserPocRoot {
    /// The run strip: the focus sentinel first so `Tab` reaches it before the
    /// pages, then the run identity in memory.
    fn render_toolbar(&self, cx: &Context<Self>) -> gpui::Stateful<gpui::Div> {
        let last_control = self.last_control_id.as_deref().unwrap_or("none");
        div()
            .id("browser-poc-toolbar")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(rgb(0x272c36))
            .child(
                div()
                    .id("browser-poc-focus-sentinel")
                    .track_focus(&self.focus_handle)
                    .tab_index(0)
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(if self.gpui_focused {
                        rgb(0x4f8cff)
                    } else {
                        rgb(0x3a4150)
                    })
                    .bg(if self.gpui_focused {
                        rgb(0x1b2434)
                    } else {
                        rgb(0x161a22)
                    })
                    .text_size(px(12.0))
                    .text_color(rgb(0xe1e6ef))
                    .focus_visible(|style| style.border_color(rgb(0x7aa9ff)))
                    .cursor_pointer()
                    .child(SENTINEL_LABEL)
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.focus(&this.focus_handle, cx);
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(11.0))
                    .text_color(rgb(0x9aa3b2))
                    .truncate()
                    .child(format!(
                        "run {} - pid {} - cdp {} - profile {} - last control {}",
                        self.host.run_id,
                        std::process::id(),
                        self.host.environment.cdp_port,
                        self.host.environment.profile.display(),
                        last_control,
                    )),
            )
    }
}

fn summarize_page(state: &BrowserPocPageState) -> String {
    if let Some(error) = &state.error {
        return format!("error: {error}");
    }
    if !state.ready {
        return "starting host".to_owned();
    }
    match state.url.as_deref() {
        Some(url) if state.loading => format!("loading {url}"),
        Some(url) => url.to_owned(),
        None => "ready".to_owned(),
    }
}

/// Keep a PoC detail small: an `evaluate` expression can be large, so the
/// state file carries only its first 120 characters plus an ellipsis when
/// it was longer.
fn truncate_detail(text: &str) -> String {
    const MAX_DETAIL_CHARS: usize = 120;
    if text.chars().count() <= MAX_DETAIL_CHARS {
        return text.to_owned();
    }
    let mut detail: String = text.chars().take(MAX_DETAIL_CHARS).collect();
    detail.push('…');
    detail
}

/// The control request, parsed as far as JSON allows. An unknown action or a
/// missing page id is rejected by the caller, not here: they must consume
/// their request id exactly like a valid one.
#[derive(Debug, Deserialize)]
struct ControlFile {
    #[serde(rename = "requestId")]
    request_id: String,
    action: String,
    #[serde(rename = "pageId")]
    page_id: Option<String>,
    /// Phase two: the lease the runner minted for this share, so "share again"
    /// is a new capability rather than a renewal of the old one.
    #[serde(rename = "grantId")]
    grant_id: Option<String>,
    /// Phase two: the browser request an approval, rejection or cancel names.
    #[serde(rename = "browserRequestId")]
    request_id_field: Option<String>,
    #[serde(rename = "runtimeId")]
    runtime_id: Option<String>,
}

/// Read `control.json` from a background thread. A missing or unparseable file
/// is not an error: the runner may be mid-write, and the next tick retries.
fn read_control(artifact_dir: &Path) -> Option<ControlFile> {
    let path = artifact_dir.join(CONTROL_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("[browser-poc] could not read {}: {error}", path.display());
            return None;
        }
    };
    if text.trim().is_empty() {
        return None;
    }
    match serde_json::from_str::<ControlFile>(&text) {
        Ok(control) if !control.request_id.trim().is_empty() => Some(control),
        Ok(_) => None,
        Err(error) => {
            eprintln!(
                "[browser-poc] ignoring unreadable {}: {error}",
                path.display()
            );
            None
        }
    }
}

/// What the runner sees. `fatal` is set only when the run failed; `pages`
/// mirrors the live page entities, so a closed page simply disappears.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostState {
    run_id: String,
    pid: u32,
    started_at_unix_ms: u64,
    profile: PathBuf,
    cdp_port: u16,
    fixture_origin: String,
    artifact_dir: PathBuf,
    last_control_id: Option<String>,
    gpui_focused: bool,
    fatal: Option<String>,
    render_count: u64,
    layout_bounds: Vec<(&'static str, Option<[f32; 4]>)>,
    pages: Vec<BrowserPocPageState>,
    /// The stable identity each page is addressed by. Derived from the run id,
    /// so the runner reads it instead of guessing one.
    browser_pages: Vec<BrowserPageIdentity>,
    /// The pages currently shared with the daemon, exactly as published.
    browser_shares: Vec<HostBrowserShare>,
    /// The requests waiting for a human decision right now.
    pending_browser_requests: Vec<HostPendingBrowserRequest>,
    /// Phase-two bridge state. `None` when no bridge was configured.
    bridge: Option<HostBridgeState>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserPageIdentity {
    page: &'static str,
    page_id: Uuid,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostBrowserShare {
    page: &'static str,
    scope: BrowserScope,
    url: String,
    title: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostPendingBrowserRequest {
    page: &'static str,
    request_id: Uuid,
    action: String,
    /// The selector or URL the action names, so a runner can tell two
    /// different requests apart without trusting the page.
    detail: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostBridgeState {
    configured: bool,
    connected: bool,
    session_id: Option<Uuid>,
    runtime_id: Option<Uuid>,
    error: Option<String>,
}

/// A v4-shaped uuid derived from a label, so a run's page ids are reproducible
/// from the run id without adding a uuid feature to the crate.
fn deterministic_uuid(label: &str) -> Uuid {
    // FNV-1a per four-byte lane with a round seed. Not cryptographic: it only
    // needs to be stable across the labels one run uses.
    let mut bytes = [0u8; 16];
    for (round, slot) in bytes.chunks_mut(4).enumerate() {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325 ^ (round as u64).wrapping_mul(0x9e37_79b9);
        for byte in label.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        slot.copy_from_slice(&hash.to_le_bytes()[..4]);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

/// Write `host-state.json` atomically: unique temporary file in the same
/// directory, then rename over the destination.
fn write_host_state(artifact_dir: &Path, state: &HostState) -> std::io::Result<()> {
    let final_path = artifact_dir.join(HOST_STATE_FILE);
    let tmp_path = artifact_dir.join(format!(
        "{HOST_STATE_FILE}.tmp.{}",
        HOST_STATE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(&tmp_path, bytes)?;
    // Rust's std rename maps to MoveFileExW with MOVEFILE_REPLACE_EXISTING on
    // Windows, so an existing destination is replaced without ever being
    // truncated first: a polling reader sees the previous file or the new one.
    if let Err(error) = std::fs::rename(&tmp_path, &final_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error);
    }
    Ok(())
}

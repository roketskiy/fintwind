//! Phase-three behavioral E2E for the browser-only tool endpoint.
//!
//! This runs the production `fintwind_core::server::serve` on a loopback port
//! exactly as `fintwind-daemon` does, with the real [`fintwind_core::browser_tools::BrowserTools`]
//! registry attached to the real [`fintwind_core::browser_broker::BrowserBroker`].
//! A private-process stand-in issues its own credential through
//! `BrowserTools::issue_server`, binds one Fintwind runtime per OpenCode
//! session id, and a native WebSocket client speaks the real
//! `fintwind_protocol::browser_tools` wire against `/v1/browser-tools`.
//!
//! # What is real and what is faked (read this before trusting a pass)
//!
//! Real, production code paths exercised here:
//!
//! - the daemon listener, the handshake, the versioned `/v1` endpoint and the
//!   restricted `/v1/browser-tools` endpoint (`browser_tools_transport`);
//! - the browser registry's credential issue / bind / retire / revoke logic
//!   and its lock order with the broker;
//! - the broker's scope matching, grant currency, request-id tombstoning,
//!   result bounds, revocation on runtime replacement and disconnect cleanup;
//! - the broker's launcher registration, exact-match open routing,
//!   ambiguity refusal and launcher revocation, driven from the real plugin
//!   message grammar (`open`) and the real desktop message grammar
//!   (`browserHost`);
//! - the desktop protocol, driven through the real `fintwind_client::DaemonClient`;
//! - a retired caller's transport ending: once a binding is replaced or the
//!   capability is revoked, the daemon stops serving that connection instead
//! of continuing to answer it.
//!
//! Faked (in-process stand-ins, named honestly):
//!
//! - the GUI **page owner** is a `DaemonClient` plus a thread that answers
//!   browser notifications. There is no WebView2, no CDP, no browser, and no
//!   real page: a "snapshot" is a canned JSON value and an "approved click"
//!   is a recorded action, nothing is rendered;
//! - the GUI **launcher** registers a synthetic scope (its page and grant
//!   ids are launcher identities, never a tab) and answers an open with a
//!   canned page value. The real GUI's tab creation, navigation and
//!   full-access approval decision are not exercised;
//! - the provider runtime is `Command::Start` returning `Started` without
//!   spawning OpenCode: no private `opencode` process, no plugin binary, no
//!   model request, no agent tool call;
//! - the OpenCode session ids (`ses_e2e_...`) are strings generated here; no
//!   real OpenCode session exists.
//!
//! Therefore a pass here proves the daemon-side browser-tool contract only.
//! It does **not** verify real plugin loading, real tool execution, real page
//! behavior or a real provider model call.
//!
//! # Failure modes this host is built to surface
//!
//! Taken from `docs/browser-tools.md`, in the order they bite:
//!
//! - two tasks on one private service crossing pages, because the tool
//!   parameters could name a Fintwind session or runtime;
//! - an unknown or child OpenCode session inheriting the parent's page,
//!   instead of being refused with no "current page" fallback;
//! - a stale grant, a replaced runtime or a superseded mapping still answering;
//! - the plugin credential authenticating the desktop endpoint, or the daemon
//!   credential authenticating the browser endpoint;
//! - browser-origin traffic reaching the private endpoint at all;
//! - the browser endpoint running general task RPCs, publishing pages,
//!   answering (forging) results or shutting the daemon down;
//! - a cancel that arrived before dispatch being lost, a cancel during
//!   approval leaving the GUI acting, or a late answer resurrecting a request;
//! - a disconnect or a dropped binding leaving a caller or page acting alone;
//! - an unbounded result crossing the bridge, or unknown fields widening the
//!   message grammar.
//!
//! Plus the launcher contract from `docs/browser-automation-fixes.md`:
//!
//! - an open with no registered launcher succeeding, or being routed to a
//!   page grant instead of to the one launcher of the live session runtime;
//! - two launchers for one session runtime with the broker guessing one, or
//!   a launcher of another session or a stale runtime being used;
//! - a wrong connection answering an open, a launcher unregistered or
//!   replaced mid-flight still completing, or a page publish silently
//!   cancelling a pending open;
//! - the launcher's identity leaking into a page list, or a scroll outside
//!   its bound (including the real `{"kind":"scroll","deltaY":…}` wire form)
//!   being accepted.
//!
//! Two properties this host insists on rather than assumes:
//!
//! - when a capability is retired, an in-flight call must end with either a
//!   terminal refusal or a *proven* connection close (`ToolConnectionClosed`).
//!   A read timeout, an unreadable frame or a fixture fault is a failure of
//!   the run, never evidence that the call ended;
//! - a reply that arrives for another request while one is awaited is kept
//!   whole and still answers its own request, so nothing is lost or re-read;
//! - a list of legal pages whose serialized form dwarfs the bridge's result
//!   bound is answered with a refusal envelope, never a payload, and the page
//!   URLs do not survive inside that refusal.
//!
//! # Contract with a human running it
//!
//! One line on stdout once the daemon is bound (no credential on it):
//!
//! ```json
//! {"kind":"browser-tools-e2e","address":"127.0.0.1:PORT","reportDir":"…","pid":N}
//! ```
//!
//! One line at the end:
//!
//! ```text
//! [PASS] 30/30 checks; report: <dir>/report.json
//! ```
//!
//! Everything else, including per-check progress, goes to stderr. The report
//! never contains either credential.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use chrono::Utc;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, unbounded};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket, client::connect_with_config};
use uuid::Uuid;

use fintwind_client::DaemonClient;
use fintwind_core::browser_tools::{
    BrowserToolBinding, BrowserToolRuntime, BrowserToolServer, BrowserTools,
};
use fintwind_core::{Backend, EventSink, Request, ResponsePayload, ServerOptions, serve};
use fintwind_protocol::browser::{
    BrowserAction, BrowserRequest, BrowserResult, BrowserScope, BrowserShare,
    MAX_BROWSER_PAGES_PER_CONNECTION, MAX_BROWSER_RESULT_BYTES,
};
use fintwind_protocol::browser_tools::{
    BROWSER_TOOL_ENDPOINT, BROWSER_TOOL_VERSION, BrowserToolMessage, BrowserToolReply,
    MAX_BROWSER_TOOL_MESSAGE_BYTES,
};
use fintwind_protocol::{
    ClientMessage, Command, PROTOCOL_VERSION, ReplayCursor, ResponseOutcome, ServerMessage,
};

/// One pending tool call cannot outlive the daemon's own 30 s browser timeout
/// by much: a stall is an E2E failure, not something to wait out.
const INVOKE_TIMEOUT: Duration = Duration::from_secs(12);
const LIST_TIMEOUT: Duration = Duration::from_secs(8);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Absolute ceiling for the whole run, so a forgotten run cannot linger.
const WATCHDOG_SECONDS: u64 = 420;
/// The behavioral checks this run performs; a run that executes fewer is a
/// failure, not a partial pass.
const EXPECTED_CHECKS: usize = 30;

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CheckEntry {
    name: String,
    status: String,
    details: String,
    #[serde(rename = "durationMs")]
    duration_ms: u128,
}

#[derive(Serialize)]
struct StepEntry {
    step: String,
    status: String,
    details: String,
}

#[derive(Serialize)]
struct Report {
    #[serde(rename = "runId")]
    run_id: String,
    status: String,
    #[serde(rename = "startedAt")]
    started_at: String,
    #[serde(rename = "finishedAt")]
    finished_at: String,
    versions: Value,
    checks: Vec<CheckEntry>,
    errors: Vec<String>,
    startup: Vec<StepEntry>,
    cleanup: Vec<StepEntry>,
    artifacts: Value,
    notes: Value,
}

impl Report {
    fn new(run_id: &str) -> Self {
        Self {
            run_id: run_id.to_owned(),
            status: "running".to_owned(),
            started_at: Utc::now().to_rfc3339(),
            finished_at: String::new(),
            versions: json!({
                "protocolVersion": PROTOCOL_VERSION,
                "browserToolVersion": BROWSER_TOOL_VERSION,
                "coreVersion": env!("CARGO_PKG_VERSION"),
                "targetOs": std::env::consts::OS,
                "targetArch": std::env::consts::ARCH,
                "exampleSourceSha256": String::new(),
                "exampleExe": String::new(),
                "exampleExeSha256": String::new(),
            }),
            checks: Vec::new(),
            errors: Vec::new(),
            startup: Vec::new(),
            cleanup: Vec::new(),
            artifacts: Value::Null,
            notes: json!({
                "scope": "daemon-side browser tools contract only",
                "realComponents": [
                    "fintwind_core::server::serve on a loopback listener",
                    "the /v1 desktop endpoint and the /v1/browser-tools endpoint",
                    "browser_tools::BrowserTools registry (issue, bind, retire, revoke)",
                    "browser_tools_transport restricted reader",
                    "browser_broker::BrowserBroker scope matching, launcher registration and revocation",
                    "fintwind_client::DaemonClient as the desktop connection",
                ],
                "fakedComponents": [
                    "the GUI page owner answers from an in-process thread; there is no WebView2, no CDP and no real page",
                    "the GUI launcher registers a synthetic scope and answers the open with a canned value; no real tab is opened",
                    "the provider runtime is a Command::Start stub; no OpenCode process, plugin or model is started",
                    "the OpenCode session ids are locally generated strings, not real OpenCode sessions",
                ],
                "notVerified": [
                    "real OpenCode plugin loading and tool execution inside a private process",
                    "a real WebView2 page action (trusted click, fill, navigation, snapshot)",
                    "a real WebView2 tab open under full access (the GUI owns that decision)",
                    "a real provider model call or agent turn",
                ],
            }),
        }
    }

    fn write(&self, dir: &PathBuf) -> anyhow::Result<()> {
        let text = serde_json::to_string_pretty(self).context("could not serialize the report")?;
        fs::write(dir.join("report.json"), format!("{text}\n"))
            .context("could not write the report")
    }

    fn startup_ok(&mut self, step: &str, details: String) {
        eprintln!("[OK] startup {step}");
        self.startup.push(StepEntry {
            step: step.to_owned(),
            status: "ok".to_owned(),
            details,
        });
    }

    fn startup_failed(&mut self, step: &str, details: String) {
        eprintln!("[X] startup {step}: {details}");
        self.startup.push(StepEntry {
            step: step.to_owned(),
            status: "failed".to_owned(),
            details: details.clone(),
        });
        self.errors.push(format!("startup {step}: {details}"));
    }

    fn cleanup_ok(&mut self, step: &str) {
        eprintln!("[OK] cleanup {step}");
        self.cleanup.push(StepEntry {
            step: step.to_owned(),
            status: "ok".to_owned(),
            details: String::new(),
        });
    }

    fn cleanup_ok_with(&mut self, step: &str, details: String) {
        eprintln!("[OK] cleanup {step}: {details}");
        self.cleanup.push(StepEntry {
            step: step.to_owned(),
            status: "ok".to_owned(),
            details,
        });
    }

    fn cleanup_failed(&mut self, step: &str, details: String) {
        eprintln!("[X] cleanup {step}: {details}");
        self.cleanup.push(StepEntry {
            step: step.to_owned(),
            status: "failed".to_owned(),
            details: details.clone(),
        });
        self.errors.push(format!("cleanup {step}: {details}"));
    }
}

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

fn retryable(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
    )
}

/// tungstenite reports a peer that dropped its socket without a close frame as
/// a protocol error rather than as a clean close, so classify it here.
fn peer_is_gone(error: &tungstenite::Error) -> bool {
    let text = error.to_string().to_lowercase();
    text.contains("connection reset") || text.contains("without closing handshake")
}

fn wait_until<T>(
    label: &str,
    timeout: Duration,
    mut read: impl FnMut() -> Option<T>,
) -> anyhow::Result<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = read() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            bail!("{label} did not happen within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn refused_message(result: &BrowserResult) -> anyhow::Result<String> {
    match result {
        BrowserResult::Ok { value } => bail!("expected a refusal, got the success {value}"),
        BrowserResult::Error { message } => Ok(message.clone()),
    }
}

fn expect_refused(result: &BrowserResult, patterns: &[&str]) -> anyhow::Result<String> {
    let message = refused_message(result)?;
    let lowered = message.to_lowercase();
    if !patterns
        .iter()
        .any(|pattern| lowered.contains(&pattern.to_lowercase()))
    {
        bail!("the refusal `{message}` matched none of {patterns:?}");
    }
    Ok(message)
}

fn expect_ok(result: &BrowserResult) -> anyhow::Result<Value> {
    match result {
        BrowserResult::Ok { value } => Ok(value.clone()),
        BrowserResult::Error { message } => {
            bail!("expected a success, got the refusal `{message}`")
        }
    }
}

/// The one property a retired capability must have: the caller's call never
/// succeeds. Two outcomes prove it, and only two:
///
/// - the daemon answers with a terminal refusal, or
/// - the capability is gone, so the transport ends and the caller sees a
///   proven connection close (`ToolConnectionClosed`).
///
/// Anything else — a timeout, an unreadable frame, a fixture fault — is a
/// failure of this run, never evidence that the call ended.
fn terminal_outcome(result: anyhow::Result<BrowserResult>) -> anyhow::Result<String> {
    match result {
        Ok(BrowserResult::Ok { value }) => {
            bail!("expected a terminal outcome, got the success {value}")
        }
        Ok(BrowserResult::Error { message }) => Ok(message),
        Err(error) => match error.downcast_ref::<ToolConnectionClosed>() {
            Some(closed) => Ok(closed.to_string()),
            None => bail!("the call failed, but not with a proven connection close: {error:#}"),
        },
    }
}

/// Compare a JSON field that holds an id against the id itself, so a check
/// never depends on how a UUID was spelled on the wire.
fn json_id(value: &Value, id: &Uuid) -> bool {
    value.as_str() == Some(&id.to_string())
}

/// The internal request id the owner log recorded for one delivered
/// request: the id the daemon addressed the GUI with, not the caller's.
fn internal_request_id(event: &Value) -> anyhow::Result<Uuid> {
    let raw = event["requestId"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("the owner log carried no internal request id"))?;
    Uuid::parse_str(raw).context("the owner log carried an unreadable request id")
}

/// Approve one request the owner is holding. The `awaitingApproval` log
/// event is the signal that the owner thread has put the request in its
/// `held` map, so approving after it can never race the insert and get
/// ignored.
fn approve_held(owner: &FakeOwner, request_id: Uuid) -> anyhow::Result<()> {
    owner.wait_event(
        |event| {
            event["event"] == "awaitingApproval" && event["requestId"] == request_id.to_string()
        },
        Duration::from_secs(5),
    )?;
    owner.approve(request_id);
    Ok(())
}

fn action_label(action: &BrowserAction) -> &'static str {
    match action {
        BrowserAction::Snapshot => "snapshot",
        BrowserAction::Click { .. } => "click",
        BrowserAction::Fill { .. } => "fill",
        BrowserAction::Navigate { .. } => "navigate",
        BrowserAction::Open { .. } => "open",
        BrowserAction::Scroll { .. } => "scroll",
        BrowserAction::Screenshot { .. } => "screenshot",
        BrowserAction::Evaluate { .. } => "evaluate",
        BrowserAction::ClickAt { .. } => "clickAt",
        BrowserAction::DoubleClick { .. } => "doubleClick",
        BrowserAction::Press { .. } => "press",
        BrowserAction::Hover { .. } => "hover",
        BrowserAction::Select { .. } => "select",
        BrowserAction::Drag { .. } => "drag",
        BrowserAction::Close => "close",
    }
}

/// SHA-256 over bytes, so the report can carry a real build hash. The
/// digest comes from the crate, not from an algorithm written here.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn sha256_file(path: &PathBuf) -> anyhow::Result<String> {
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    Ok(sha256_hex(&bytes))
}

// ---------------------------------------------------------------------------
// WebSocket helpers shared by the tool client and the probes
// ---------------------------------------------------------------------------

enum ReadOutcome {
    Text(String),
    Timeout,
    Closed,
}

fn set_stream_read_timeout(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Option<Duration>,
) -> std::io::Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(timeout),
        #[allow(unreachable_patterns)]
        _ => Ok(()),
    }
}

fn send_value(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    value: &Value,
) -> anyhow::Result<()> {
    let text = serde_json::to_string(value).context("could not serialize a probe message")?;
    socket
        .send(Message::Text(text.into()))
        .context("could not send a probe message")
}

fn read_text(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Duration,
) -> anyhow::Result<ReadOutcome> {
    set_stream_read_timeout(socket, Some(timeout))?;
    loop {
        return match socket.read() {
            Ok(Message::Text(text)) => Ok(ReadOutcome::Text(text.to_string())),
            Ok(Message::Ping(_)) => {
                socket.flush().ok();
                continue;
            }
            Ok(Message::Close(_)) => Ok(ReadOutcome::Closed),
            Ok(_) => continue,
            Err(tungstenite::Error::Io(error)) if retryable(&error) => Ok(ReadOutcome::Timeout),
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                // The endpoint ended the connection without a WebSocket close
                // frame, which is what a retired caller's transport does.
                Ok(ReadOutcome::Closed)
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Ok(ReadOutcome::Closed)
            }
            Err(error) if peer_is_gone(&error) => Ok(ReadOutcome::Closed),
            Err(error) => Err(error).context("reading from a WebSocket"),
        };
    }
}

/// The first text frame a freshly connected endpoint answers with.
fn first_reply_text(url: &str, hello: &Value, timeout: Duration) -> anyhow::Result<String> {
    let (mut socket, _) =
        connect_with_config(url, None, 3).with_context(|| format!("could not open {url}"))?;
    send_value(&mut socket, hello)?;
    match read_text(&mut socket, timeout)? {
        ReadOutcome::Text(text) => Ok(text),
        ReadOutcome::Timeout => bail!("{url} did not answer the hello"),
        ReadOutcome::Closed => bail!("{url} closed before answering the hello"),
    }
}

// ---------------------------------------------------------------------------
// The native browser-tool client (the plugin side of the wire)
// ---------------------------------------------------------------------------

/// One reply that arrived for a request id the caller is not currently
/// awaiting. The frame is kept whole — id, result and raw text — so the await
/// that does want it still finds it.
enum HeldFrame {
    Result {
        request_id: Uuid,
        result: BrowserResult,
        raw: String,
    },
    /// Anything the endpoint sends outside an answer, kept for diagnostics.
    Other { raw: String },
}

/// The daemon retired this caller's transport, so the socket ended before an
/// answer could be written. Only this error may stand for "the call ended":
/// a read timeout, a parse failure or a fixture fault is a failure, never a
/// terminal outcome.
#[derive(Debug)]
struct ToolConnectionClosed {
    reason: &'static str,
}

impl std::fmt::Display for ToolConnectionClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the browser tool connection closed before it could answer ({})",
            self.reason
        )
    }
}

impl std::error::Error for ToolConnectionClosed {}

/// A native WebSocket client for the restricted browser endpoint. A pump
/// thread owns the socket so a pending invoke never blocks the test thread,
/// which is how a real plugin would await an approval.
struct ToolClient {
    outgoing: Sender<String>,
    frames: Receiver<String>,
    /// Replies that arrived for another request id, in arrival order.
    stash: Mutex<VecDeque<HeldFrame>>,
}

fn pump_tool_socket(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    outgoing: Receiver<String>,
    frames: Sender<String>,
) {
    if set_stream_read_timeout(&mut socket, Some(Duration::from_millis(25))).is_err() {
        return;
    }
    loop {
        // A dropped sender means the caller is gone: close the socket so the
        // daemon-side transport sees the disconnect instead of lingering.
        match outgoing.try_recv() {
            Ok(text) => {
                if socket.send(Message::Text(text.into())).is_err() {
                    return;
                }
            }
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                if frames.send(text.to_string()).is_err() {
                    return;
                }
            }
            Ok(Message::Ping(_)) => {
                let _ = socket.flush();
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if retryable(&error) => {}
            Err(_) => return,
        }
    }
}

impl ToolClient {
    fn connect(url: &str, token: &str) -> anyhow::Result<Self> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_BROWSER_TOOL_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_BROWSER_TOOL_MESSAGE_BYTES));
        let (mut socket, _) = connect_with_config(url, Some(config), 3)
            .with_context(|| format!("could not open a browser tool connection to {url}"))?;
        send_value(
            &mut socket,
            &serde_json::to_value(&BrowserToolMessage::Hello {
                version: BROWSER_TOOL_VERSION,
                token: token.to_owned(),
            })?,
        )?;
        let hello = match read_text(&mut socket, Duration::from_secs(10))? {
            ReadOutcome::Text(text) => serde_json::from_str::<BrowserToolReply>(&text)
                .with_context(|| format!("unreadable browser tool hello reply: {text}"))?,
            ReadOutcome::Timeout => bail!("the browser tool endpoint did not answer the hello"),
            ReadOutcome::Closed => bail!("the browser tool endpoint closed before the hello reply"),
        };
        match hello {
            BrowserToolReply::Hello { version } if version == BROWSER_TOOL_VERSION => {}
            other => bail!("the browser tool endpoint refused the connection: {other:?}"),
        }
        let (outgoing_tx, outgoing_rx) = unbounded();
        let (frames_tx, frames_rx) = unbounded();
        std::thread::Builder::new()
            .name("browser-tools-e2e-tool-pump".into())
            .spawn(move || pump_tool_socket(socket, outgoing_rx, frames_tx))
            .context("could not start the browser tool pump thread")?;
        Ok(Self {
            outgoing: outgoing_tx,
            frames: frames_rx,
            stash: Mutex::new(VecDeque::new()),
        })
    }

    fn send(&self, message: &BrowserToolMessage) -> anyhow::Result<()> {
        let text = serde_json::to_string(message)?;
        self.outgoing.send(text).map_err(|_| ToolConnectionClosed {
            reason: "the pump thread ended, so the socket is gone",
        })?;
        Ok(())
    }

    /// One pass over the frames already held: remove the match, keep the rest
    /// in order. This never blocks, so it cannot loop back onto itself.
    fn take_held(&self, request_id: Uuid) -> Option<(BrowserResult, String)> {
        let mut stash = self.stash.lock();
        let index = stash.iter().position(|held| {
            matches!(
                held,
                HeldFrame::Result {
                    request_id: id,
                    ..
                } if *id == request_id
            )
        })?;
        match stash.remove(index) {
            Some(HeldFrame::Result { result, raw, .. }) => Some((result, raw)),
            _ => None,
        }
    }

    /// Wait for the result of one request id: scan what is already held once,
    /// then read exactly one incoming frame, keeping every other request's
    /// reply for the await that wants it.
    fn result_frame(
        &self,
        request_id: Uuid,
        timeout: Duration,
    ) -> anyhow::Result<(BrowserResult, String)> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(held) = self.take_held(request_id) {
                return Ok(held);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("the browser tool endpoint did not answer {request_id}");
            }
            match self.frames.recv_timeout(left) {
                Ok(text) => match serde_json::from_str::<BrowserToolReply>(&text) {
                    Ok(BrowserToolReply::Result {
                        request_id: answered,
                        result,
                    }) if answered == request_id => return Ok((result, text)),
                    Ok(BrowserToolReply::Result {
                        request_id: answered,
                        result,
                    }) => self.stash.lock().push_back(HeldFrame::Result {
                        request_id: answered,
                        result,
                        raw: text,
                    }),
                    Ok(_) => self.stash.lock().push_back(HeldFrame::Other { raw: text }),
                    Err(_) => bail!("the browser tool endpoint sent an unreadable frame: {text}"),
                },
                Err(RecvTimeoutError::Timeout) => {
                    // A timeout is a failure of this run, so say what was
                    // already held: an answer for another request, or a frame
                    // no request id can claim.
                    let held: Vec<String> = self
                        .stash
                        .lock()
                        .iter()
                        .map(|held| match held {
                            HeldFrame::Result { request_id, .. } => request_id.to_string(),
                            HeldFrame::Other { raw } => format!("non-result: {raw}"),
                        })
                        .collect();
                    bail!(
                        "the browser tool endpoint did not answer {request_id} in time; held frames: {held:?}"
                    );
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // The pump ended, so the socket is gone: the only way a
                    // retired caller's call can "end" rather than fail.
                    return Err(ToolConnectionClosed {
                        reason: "the pump thread ended",
                    }
                    .into());
                }
            }
        }
    }

    fn list(&self, request_id: Uuid, session: &str) -> anyhow::Result<BrowserResult> {
        self.send(&BrowserToolMessage::List {
            request_id,
            session_id: session.to_owned(),
        })?;
        self.result_frame(request_id, LIST_TIMEOUT)
            .map(|(result, _)| result)
    }

    fn list_frame(
        &self,
        request_id: Uuid,
        session: &str,
    ) -> anyhow::Result<(BrowserResult, String)> {
        self.send(&BrowserToolMessage::List {
            request_id,
            session_id: session.to_owned(),
        })?;
        self.result_frame(request_id, LIST_TIMEOUT)
    }

    fn invoke(
        &self,
        request_id: Uuid,
        session: &str,
        page_id: Uuid,
        grant_id: Uuid,
        action: BrowserAction,
    ) -> anyhow::Result<BrowserResult> {
        self.send(&BrowserToolMessage::Invoke {
            request_id,
            session_id: session.to_owned(),
            page_id,
            grant_id,
            action,
        })?;
        self.result_frame(request_id, INVOKE_TIMEOUT)
            .map(|(result, _)| result)
    }

    fn invoke_frame(
        &self,
        request_id: Uuid,
        session: &str,
        page_id: Uuid,
        grant_id: Uuid,
        action: BrowserAction,
    ) -> anyhow::Result<(BrowserResult, String)> {
        self.send(&BrowserToolMessage::Invoke {
            request_id,
            session_id: session.to_owned(),
            page_id,
            grant_id,
            action,
        })?;
        self.result_frame(request_id, INVOKE_TIMEOUT)
    }

    /// Ask the daemon to open a new tab for this connection's session. The
    /// caller names no page: the daemon resolves the live session runtime
    /// from the binding and routes to its registered launcher.
    fn open(&self, request_id: Uuid, session: &str, url: &str) -> anyhow::Result<BrowserResult> {
        self.send(&BrowserToolMessage::Open {
            request_id,
            session_id: session.to_owned(),
            url: url.to_owned(),
        })?;
        self.result_frame(request_id, INVOKE_TIMEOUT)
            .map(|(result, _)| result)
    }

    /// Send one hand-written JSON frame, byte for byte. Used to prove the
    /// wire grammar itself — a real plugin's field spelling, not the Rust
    /// enum's — still routes and still answers on this connection.
    fn send_raw(&self, message: &Value) -> anyhow::Result<()> {
        let text = serde_json::to_string(message)?;
        self.outgoing.send(text).map_err(|_| ToolConnectionClosed {
            reason: "the pump thread ended, so the socket is gone",
        })?;
        Ok(())
    }

    fn cancel(&self, request_id: Uuid) -> anyhow::Result<()> {
        self.send(&BrowserToolMessage::Cancel { request_id })
    }
}

/// One exchange on the browser endpoint with a fresh connection: a valid
/// hello, then the given messages, collecting every reply frame.
fn tool_probe(url: &str, token: &str, messages: &[Value]) -> anyhow::Result<(Vec<String>, bool)> {
    let (mut socket, _) = connect_with_config(url, None, 3)
        .with_context(|| format!("could not open a browser tool probe to {url}"))?;
    send_value(
        &mut socket,
        &serde_json::to_value(&BrowserToolMessage::Hello {
            version: BROWSER_TOOL_VERSION,
            token: token.to_owned(),
        })?,
    )?;
    let mut frames = Vec::new();
    match read_text(&mut socket, Duration::from_secs(10))? {
        ReadOutcome::Text(text) => {
            let reply: BrowserToolReply = serde_json::from_str(&text)
                .with_context(|| format!("unreadable browser tool hello reply: {text}"))?;
            if !matches!(reply, BrowserToolReply::Hello { .. }) {
                bail!("the browser tool endpoint refused the probe hello: {reply:?}");
            }
        }
        ReadOutcome::Timeout => bail!("the browser tool endpoint did not answer the probe hello"),
        ReadOutcome::Closed => bail!("the browser tool endpoint closed before the probe hello"),
    }
    let mut closed = false;
    for message in messages {
        send_value(&mut socket, message)?;
        match read_text(&mut socket, PROBE_TIMEOUT)? {
            ReadOutcome::Text(text) => frames.push(text),
            ReadOutcome::Timeout => {}
            ReadOutcome::Closed => {
                closed = true;
                break;
            }
        }
    }
    if !closed {
        if let Ok(ReadOutcome::Closed) = read_text(&mut socket, Duration::from_millis(500)) {
            closed = true;
        }
    }
    Ok((frames, closed))
}

/// A raw hand-written HTTP upgrade, used only to prove the endpoint refuses a
/// browser `Origin` before any WebSocket frame exists.
fn probe_origin(address: &str) -> anyhow::Result<(String, String)> {
    let mut stream =
        TcpStream::connect(address).with_context(|| format!("could not reach {address}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let request = format!(
        "GET {BROWSER_TOOL_ENDPOINT} HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: https://attacker.example\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .context("could not send the origin probe")?;
    let mut response: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let deadline = Instant::now() + Duration::from_secs(5);
    while !response.windows(4).any(|window| window == b"\r\n\r\n") {
        if Instant::now() >= deadline || response.len() > 8 * 1024 {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error) if retryable(&error) => continue,
            Err(error) => return Err(error).context("reading the origin probe response"),
        }
    }
    let text = String::from_utf8_lossy(&response).to_string();
    let status = text.lines().next().unwrap_or_default().to_owned();
    Ok((status, text))
}

// ---------------------------------------------------------------------------
// The fake GUI page owner
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum OwnerCommand {
    Approve(Uuid),
}

fn owner_note(log: &Mutex<Vec<Value>>, mut event: Value) {
    let mut log = log.lock();
    let sequence = log.len() as u64 + 1;
    if let Some(object) = event.as_object_mut() {
        object.insert("seq".into(), json!(sequence));
        object.insert("at".into(), json!(Utc::now().to_rfc3339()));
    }
    log.push(event);
}

fn fake_snapshot(request: &BrowserRequest) -> Value {
    // A canned value. This is not a page: it proves the daemon routed the
    // action to the owner of the mapped scope and that the owner's answer
    // came back to the caller unchanged.
    json!({
        "url": "https://fake-owner.invalid/browser-tools-e2e",
        "title": "fake owner page (not WebView2)",
        "text": "fake-owner-snapshot",
        "controls": [],
        "truncated": false,
        "requestedAction": action_label(&request.action),
        "pageId": request.scope.page_id,
        "grantId": request.scope.grant_id,
    })
}

fn owner_loop(
    client: DaemonClient,
    commands: Receiver<OwnerCommand>,
    notifications: Receiver<fintwind_client::BrowserNotification>,
    log: Arc<Mutex<Vec<Value>>>,
    performed: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
) {
    let mut held: HashMap<Uuid, BrowserRequest> = HashMap::new();
    while !stop.load(Ordering::Acquire) {
        while let Ok(command) = commands.try_recv() {
            match command {
                OwnerCommand::Approve(request_id) => {
                    let Some(request) = held.remove(&request_id) else {
                        owner_note(
                            &log,
                            json!({"event": "approveIgnored", "requestId": request_id}),
                        );
                        continue;
                    };
                    owner_note(
                        &log,
                        json!({"event": "performed", "requestId": request_id, "action": action_label(&request.action)}),
                    );
                    performed.lock().push(json!({
                        "requestId": request_id,
                        "action": action_label(&request.action),
                        "scope": request.scope,
                    }));
                    let _ = client.complete_browser_request(
                        request_id,
                        BrowserResult::Ok {
                            value: json!({"issued": true, "fakeOwner": true}),
                        },
                    );
                }
            }
        }
        match notifications.recv_timeout(Duration::from_millis(25)) {
            Ok(fintwind_client::BrowserNotification::Request(request)) => {
                owner_note(
                    &log,
                    json!({"event": "browserRequest", "requestId": request.request_id, "scope": request.scope, "action": request.action}),
                );
                if matches!(request.action, BrowserAction::Snapshot) {
                    // Observation is the owner's default permission; a mutation
                    // waits for the same approval a real page would need.
                    let _ = client.complete_browser_request(
                        request.request_id,
                        BrowserResult::Ok {
                            value: fake_snapshot(&request),
                        },
                    );
                    owner_note(
                        &log,
                        json!({"event": "autoAnswered", "requestId": request.request_id}),
                    );
                } else {
                    // Held for approval, and the hold is logged: a check may
                    // only approve once this event exists, so it can never
                    // approve a request the owner thread has not put in
                    // `held` yet.
                    owner_note(
                        &log,
                        json!({"event": "awaitingApproval", "requestId": request.request_id}),
                    );
                    held.insert(request.request_id, request);
                }
            }
            Ok(fintwind_client::BrowserNotification::Cancel(request_id)) => {
                owner_note(
                    &log,
                    json!({"event": "browserCancel", "requestId": request_id}),
                );
                held.remove(&request_id);
            }
            Ok(fintwind_client::BrowserNotification::ShareRejected { scopes, message }) => {
                owner_note(
                    &log,
                    json!({"event": "shareRejected", "scopes": scopes, "message": message}),
                );
            }
            Ok(fintwind_client::BrowserNotification::ScopesRevoked(scopes)) => {
                owner_note(&log, json!({"event": "scopesRevoked", "scopes": scopes}));
            }
            Ok(fintwind_client::BrowserNotification::Disconnected) => {
                owner_note(&log, json!({"event": "disconnected"}));
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// The desktop side of the bridge: a real `DaemonClient` whose browser
/// notifications are answered by an in-process thread instead of by WebView2.
struct FakeOwner {
    client: DaemonClient,
    log: Arc<Mutex<Vec<Value>>>,
    performed: Arc<Mutex<Vec<Value>>>,
    commands: Sender<OwnerCommand>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeOwner {
    fn connect(label: &str, address: &str, token: String) -> anyhow::Result<Self> {
        let client = DaemonClient::connect(address, token)
            .with_context(|| format!("the fake {label} owner could not authenticate"))?;
        // Subscribe before anything can be published, so a request is never
        // refused for a missing consumer.
        let notifications = client.subscribe_browser_requests();
        let log = Arc::new(Mutex::new(Vec::new()));
        let performed = Arc::new(Mutex::new(Vec::new()));
        let (commands_tx, commands_rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let loop_client = client.clone();
        let loop_log = log.clone();
        let loop_performed = performed.clone();
        let loop_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name(format!("browser-tools-e2e-owner-{label}"))
            .spawn(move || {
                owner_loop(
                    loop_client,
                    commands_rx,
                    notifications,
                    loop_log,
                    loop_performed,
                    loop_stop,
                )
            })
            .context("could not start the fake owner thread")?;
        Ok(Self {
            client,
            log,
            performed,
            commands: commands_tx,
            stop,
            thread: Some(thread),
        })
    }

    fn start_runtime(&self, session: Uuid, runtime: Uuid) -> anyhow::Result<()> {
        let payload = self
            .client
            .request(
                session,
                runtime,
                Command::Start {
                    options: wire_options(),
                },
            )
            .context("the daemon refused the runtime start")?;
        match payload {
            ResponsePayload::Started { .. } => Ok(()),
            other => bail!("the runtime start returned an unexpected payload: {other:?}"),
        }
    }

    fn share(&self, scope: BrowserScope, url: String, title: String) -> anyhow::Result<()> {
        self.client
            .publish_browser_pages(vec![BrowserShare { scope, url, title }])
    }

    /// Share several pages at once, which is how a real owner publishes the
    /// most a connection may hold.
    fn share_many(&self, pages: Vec<BrowserShare>) -> anyhow::Result<()> {
        self.client.publish_browser_pages(pages)
    }

    /// Register this connection's launcher for one live session runtime.
    /// The scope's page and grant ids are launcher identities: they are not
    /// a shared page and may not address a page action.
    fn register_host(&self, scope: &BrowserScope) -> anyhow::Result<()> {
        self.client.publish_browser_host(Some(scope.clone()))?;
        // Publish only enqueues on the desktop socket. A reply on that same
        // FIFO connection proves registration was processed before another
        // connection sends an open; a sleep or a tool-side list cannot do so.
        self.shares(scope.session_id, scope.runtime_id)?;
        Ok(())
    }

    /// Clear this connection's launcher. Pending opens routed to it fail;
    /// page shares are untouched.
    fn clear_host(&self) -> anyhow::Result<()> {
        self.client.publish_browser_host(None)
    }

    fn shares(&self, session: Uuid, runtime: Uuid) -> anyhow::Result<Vec<BrowserShare>> {
        match self
            .client
            .request(session, runtime, Command::BrowserList)?
        {
            ResponsePayload::Json { value } => serde_json::from_value(value)
                .context("the daemon returned an unreadable share list"),
            other => bail!("the daemon answered the browser list with {other:?}"),
        }
    }

    fn events(&self) -> Vec<Value> {
        self.log.lock().clone()
    }

    fn events_of(&self, kind: &str) -> Vec<Value> {
        self.log
            .lock()
            .iter()
            .filter(|event| event["event"] == kind)
            .cloned()
            .collect()
    }

    fn performed(&self) -> Vec<Value> {
        self.performed.lock().clone()
    }

    fn wait_event(
        &self,
        predicate: impl Fn(&Value) -> bool,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        wait_until(
            "the fake owner observed the expected event",
            timeout,
            || {
                self.log
                    .lock()
                    .iter()
                    .rev()
                    .find(|event| predicate(event))
                    .cloned()
            },
        )
    }

    fn approve(&self, request_id: Uuid) {
        let _ = self.commands.send(OwnerCommand::Approve(request_id));
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for FakeOwner {
    fn drop(&mut self) {
        self.client.disconnect();
        self.shutdown();
    }
}

/// A second GUI owner on a raw socket, used only for the result-size check:
/// `DaemonClient` refuses an oversized answer locally, so the oversized value
/// has to reach the daemon to prove the daemon-side bound.
struct RawOwner {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
}

impl RawOwner {
    fn connect(address: &str, token: &str) -> anyhow::Result<Self> {
        let url = format!("ws://{address}/v1");
        let (mut socket, _) = connect_with_config(&url, None, 3)
            .with_context(|| format!("the raw owner could not open {url}"))?;
        send_value(
            &mut socket,
            &serde_json::to_value(&ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                token: token.to_owned(),
                client_id: Uuid::new_v4(),
                resume_from: Vec::<ReplayCursor>::new(),
            })?,
        )?;
        let reply = match read_text(&mut socket, Duration::from_secs(10))? {
            ReadOutcome::Text(text) => text,
            ReadOutcome::Timeout => {
                bail!("the desktop endpoint did not answer the raw owner's hello")
            }
            ReadOutcome::Closed => {
                bail!("the desktop endpoint closed before the raw owner's hello")
            }
        };
        let message: ServerMessage = serde_json::from_str(&reply)
            .with_context(|| format!("the desktop endpoint answered {reply}"))?;
        match message {
            ServerMessage::Hello {
                protocol_version, ..
            } if protocol_version == PROTOCOL_VERSION => {}
            other => bail!("the raw owner could not authenticate: {other:?}"),
        }
        Ok(Self { socket })
    }

    fn send(&mut self, message: &ClientMessage) -> anyhow::Result<()> {
        send_value(&mut self.socket, &serde_json::to_value(message)?)
    }

    /// Read one server message, whatever it is. Used for refusals the
    /// `DaemonClient` drops silently (a generic `Rejected`), so a raw owner
    /// can prove the daemon refused its forged answer or its duplicate
    /// launcher registration.
    fn next_message(&mut self, timeout: Duration) -> anyhow::Result<ServerMessage> {
        match read_text(&mut self.socket, timeout)? {
            ReadOutcome::Text(text) => serde_json::from_str(&text)
                .with_context(|| format!("the desktop endpoint answered {text}")),
            ReadOutcome::Timeout => bail!("the desktop endpoint did not answer the raw owner"),
            ReadOutcome::Closed => bail!("the raw owner's connection closed"),
        }
    }

    fn publish(&mut self, pages: Vec<BrowserShare>) -> anyhow::Result<()> {
        self.send(&ClientMessage::BrowserPublish { pages })
    }

    fn complete(&mut self, request_id: Uuid, result: BrowserResult) -> anyhow::Result<()> {
        self.send(&ClientMessage::BrowserResult { request_id, result })
    }

    fn shares(&mut self, session: Uuid, runtime: Uuid) -> anyhow::Result<Vec<BrowserShare>> {
        let request_id = Uuid::new_v4();
        self.send(&ClientMessage::Request(Request {
            request_id,
            session_id: session,
            runtime_id: runtime,
            command: Command::BrowserList,
        }))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match read_text(&mut self.socket, PROBE_TIMEOUT)? {
                ReadOutcome::Text(text) => {
                    let message: ServerMessage = serde_json::from_str(&text)
                        .with_context(|| format!("the desktop endpoint answered {text}"))?;
                    if let ServerMessage::Response {
                        request_id: answered,
                        outcome:
                            ResponseOutcome::Ok {
                                payload: ResponsePayload::Json { value },
                            },
                    } = message
                        && answered == request_id
                    {
                        return serde_json::from_value(value)
                            .context("the daemon returned an unreadable share list");
                    }
                }
                ReadOutcome::Timeout => bail!("the daemon did not answer the raw owner's list"),
                ReadOutcome::Closed => bail!("the raw owner's connection closed"),
            }
        }
        bail!("the raw owner's list timed out")
    }

    fn wait_browser_request(
        &mut self,
        scope: &BrowserScope,
        timeout: Duration,
    ) -> anyhow::Result<BrowserRequest> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match read_text(&mut self.socket, PROBE_TIMEOUT)? {
                ReadOutcome::Text(text) => {
                    let message: ServerMessage = serde_json::from_str(&text)
                        .with_context(|| format!("the desktop endpoint answered {text}"))?;
                    if let ServerMessage::BrowserRequest { request } = message
                        && &request.scope == scope
                    {
                        return Ok(request);
                    }
                }
                ReadOutcome::Timeout => {}
                ReadOutcome::Closed => {
                    bail!("the raw owner's connection closed before the request")
                }
            }
        }
        bail!("the daemon did not route the action to the raw owner")
    }
}

fn wire_options() -> fintwind_protocol::WireDriverStartOptions {
    fintwind_protocol::WireDriverStartOptions {
        binary: PathBuf::from("opencode"),
        cwd: std::env::temp_dir(),
        mode: "default".to_owned(),
        interaction_mode: "default".to_owned(),
        model: None,
        reasoning_effort: None,
        service_tier: None,
        context_window: None,
        agent_preset: None,
        provider_cursor: None,
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    dir: PathBuf,
    address: String,
    tools_url: String,
    daemon_token: String,
    browser_token: String,
    registry: Arc<BrowserTools>,
    server: Arc<BrowserToolServer>,
    session: Uuid,
    session_b: Uuid,
    runtime_b: Uuid,
    native_session: String,
    native_session_b: String,
    /// A third native session, deliberately mapped to a runtime that never
    /// started. Nothing about it is live, so it proves a stale mapping
    /// fails closed even while a valid launcher exists for the active
    /// runtime.
    native_session_c: String,
    /// The Fintwind runtime the broker currently answers for `session`.
    active_runtime: Uuid,
    binding: Option<BrowserToolBinding>,
    /// The other Fintwind session's guard, for the whole run.
    binding_b: Option<BrowserToolBinding>,
    /// The stale mapping's guard, for the whole run.
    binding_c: Option<BrowserToolBinding>,
    /// The guard a replacement binding superseded; dropping it must not remove
    /// the replacement.
    superseded: Option<BrowserToolBinding>,
    owner: FakeOwner,
    owner_b: FakeOwner,
    tool: Option<ToolClient>,
    tool_b: Option<ToolClient>,
    tool_c: Option<ToolClient>,
    raw_owner: Option<RawOwner>,
    shutdown: Arc<AtomicBool>,
    serve_rx: Receiver<String>,
    serve_thread: Option<JoinHandle<()>>,
}

impl Harness {
    fn tool(&self) -> anyhow::Result<&ToolClient> {
        self.tool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("the browser tool connection is not live"))
    }

    fn tool_c(&self) -> anyhow::Result<&ToolClient> {
        self.tool_c
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("the stale-mapping tool connection is not live"))
    }

    fn take_tool(&mut self) -> Option<ToolClient> {
        self.tool.take()
    }

    fn put_tool(&mut self, tool: ToolClient) {
        self.tool = Some(tool);
    }

    fn native_runtime(&self, session: Uuid, runtime: Uuid) -> BrowserToolRuntime {
        BrowserToolRuntime {
            registry: self.registry.clone(),
            session_id: session,
            runtime_id: runtime,
        }
    }

    /// Publish one page through `owner` and wait until the daemon lists it.
    fn share(
        &self,
        owner: &FakeOwner,
        session: Uuid,
        runtime: Uuid,
    ) -> anyhow::Result<BrowserScope> {
        let scope = BrowserScope {
            session_id: session,
            runtime_id: runtime,
            page_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
        };
        owner.share(
            scope.clone(),
            format!("https://fake-owner.invalid/page/{}", scope.page_id),
            "fake owner page (not WebView2)".to_owned(),
        )?;
        let wanted = scope.clone();
        wait_until(
            "the daemon accepted the explicit share",
            Duration::from_secs(10),
            || {
                owner
                    .shares(session, runtime)
                    .ok()
                    .and_then(|pages| pages.iter().any(|page| page.scope == wanted).then_some(()))
            },
        )?;
        Ok(scope)
    }

    /// Replace the binding for the test's own native session, keeping the
    /// superseded guard so its late drop can be checked separately.
    fn rebind(&mut self, session: Uuid, runtime: Uuid) -> anyhow::Result<()> {
        let guard = self.server.bind(
            self.native_session.clone(),
            &self.native_runtime(session, runtime),
        )?;
        if let Some(previous) = self.binding.replace(guard) {
            self.superseded = Some(previous);
        }
        self.active_runtime = runtime;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// The one backend the E2E needs. Browser commands never reach it — the
/// server answers those through the broker — so it only has to activate a
/// runtime without starting a provider, and hand `serve` the registry it
/// shares with this test.
struct E2EBackend {
    tools: Arc<BrowserTools>,
}

impl Backend for E2EBackend {
    fn browser_tools(&self) -> Option<Arc<BrowserTools>> {
        Some(self.tools.clone())
    }

    fn handle(&self, request: Request, _events: EventSink) -> anyhow::Result<ResponsePayload> {
        match &request.command {
            Command::Start { .. } => Ok(ResponsePayload::Started {
                supports_steer: false,
            }),
            other => bail!("the browser tools e2e backend does not implement {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

struct E2E {
    dir: PathBuf,
    report: Report,
    harness: Harness,
}

fn spawn_watchdog(finished: Arc<AtomicBool>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("browser-tools-e2e-watchdog".into())
        .spawn(move || {
            let step = Duration::from_millis(250);
            let mut waited = Duration::ZERO;
            while waited < Duration::from_secs(WATCHDOG_SECONDS) {
                if finished.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(step);
                waited += step;
            }
            eprintln!(
                "[browser-tools-e2e] the {WATCHDOG_SECONDS}s watchdog deadline reached; forcing exit so no process is left behind"
            );
            std::process::exit(3);
        })
        .expect("could not start the browser tools e2e watchdog thread")
}

impl E2E {
    fn check(&mut self, name: &str, body: impl FnOnce(&mut Harness) -> anyhow::Result<String>) {
        let started = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| body(&mut self.harness)));
        let (status, details) = match outcome {
            Ok(Ok(details)) => ("passed".to_owned(), details),
            Ok(Err(error)) => ("failed".to_owned(), format!("{error:#}")),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                    .unwrap_or_else(|| "the check panicked".to_owned());
                ("failed".to_owned(), format!("panic: {message}"))
            }
        };
        let entry = CheckEntry {
            name: name.to_owned(),
            status: status.clone(),
            details,
            duration_ms: started.elapsed().as_millis(),
        };
        if entry.status == "passed" {
            eprintln!("[OK] {} ({} ms)", entry.name, entry.duration_ms);
        } else {
            eprintln!("[X] {}: {}", entry.name, entry.details);
            self.report
                .errors
                .push(format!("{}: {}", entry.name, entry.details));
        }
        self.report.checks.push(entry);
        if let Err(error) = self.report.write(&self.dir) {
            eprintln!("[X] could not persist the report: {error:#}");
        }
    }
}

/// Build the harness: the real daemon, a real desktop owner, a real browser
/// tool server and the tool connections a plugin would open.
fn build_harness(report: &mut Report, dir: &PathBuf) -> anyhow::Result<Harness> {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").context("could not bind the e2e listener")?;
    let address = listener
        .local_addr()
        .context("could not read the e2e listener address")?
        .to_string();
    let daemon_token = Uuid::new_v4().simple().to_string();
    let registry: Arc<BrowserTools> = Arc::new(BrowserTools::default());
    let backend: Arc<dyn Backend> = Arc::new(E2EBackend {
        tools: registry.clone(),
    });
    let shutdown = Arc::new(AtomicBool::new(false));
    let (serve_tx, serve_rx) = unbounded();
    let serve_token = daemon_token.clone();
    let serve_shutdown = shutdown.clone();
    let serve_thread = std::thread::Builder::new()
        .name("browser-tools-e2e-serve".into())
        .spawn(move || {
            let result = serve(
                listener,
                serve_token,
                backend,
                serve_shutdown,
                ServerOptions {
                    allow_shutdown: true,
                },
            );
            let summary = match result {
                Ok(()) => "serve returned Ok".to_owned(),
                Err(error) => format!("serve failed: {error:#}"),
            };
            let _ = serve_tx.send(summary);
        })
        .context("could not start the e2e serve thread")?;
    report.startup_ok(
        "daemon listener",
        format!("{address} (loopback, protocol {PROTOCOL_VERSION})"),
    );

    // The registry is attached inside `serve`, so retry until it is live.
    let server = wait_until(
        "the browser tools registry attached to the daemon",
        Duration::from_secs(15),
        || registry.issue_server().ok(),
    )?;
    let browser_token = server.token().to_owned();
    let tools_url = server.address().to_owned();
    report.startup_ok(
        "browser tool server issued",
        format!(
            "{tools_url} with a {} character credential",
            browser_token.len()
        ),
    );

    let owner = match FakeOwner::connect("primary", &address, daemon_token.clone()) {
        Ok(owner) => owner,
        Err(error) => {
            report.startup_failed("authenticated desktop owner", format!("{error:#}"));
            return Err(error);
        }
    };
    report.startup_ok(
        "authenticated desktop owner",
        "DaemonClient handshake and hello accepted".to_owned(),
    );
    let session = Uuid::new_v4();
    let runtime = Uuid::new_v4();
    if let Err(error) = owner.start_runtime(session, runtime) {
        report.startup_failed("first runtime start", format!("{error:#}"));
        return Err(error);
    }
    report.startup_ok("first runtime start", format!("{session} / {runtime}"));
    let owner_b = match FakeOwner::connect("secondary", &address, daemon_token.clone()) {
        Ok(owner) => owner,
        Err(error) => {
            report.startup_failed("second desktop owner", format!("{error:#}"));
            return Err(error);
        }
    };
    let session_b = Uuid::new_v4();
    let runtime_b = Uuid::new_v4();
    if let Err(error) = owner_b.start_runtime(session_b, runtime_b) {
        report.startup_failed("second runtime start", format!("{error:#}"));
        return Err(error);
    }
    report.startup_ok("second runtime start", format!("{session_b} / {runtime_b}"));

    // One private process owns one credential; its native sessions are bound
    // to the Fintwind runtimes that opened them.
    let native_session = format!("ses_e2e_{}", Uuid::new_v4().simple());
    let native_session_b = format!("ses_e2e_{}", Uuid::new_v4().simple());
    let binding = server.bind(
        native_session.clone(),
        &BrowserToolRuntime {
            registry: registry.clone(),
            session_id: session,
            runtime_id: runtime,
        },
    )?;
    // The second binding must outlive the harness build: dropping it retires
    // its own connection, so it lives exactly as long as the run does.
    let binding_b = server.bind(
        native_session_b.clone(),
        &BrowserToolRuntime {
            registry: registry.clone(),
            session_id: session_b,
            runtime_id: runtime_b,
        },
    )?;
    // The third binding maps its native session to a runtime that was never
    // started. Nothing about it is live; the stale-mapping check proves an
    // open through it is refused even while a valid launcher exists for the
    // active runtime.
    let native_session_c = format!("ses_e2e_{}", Uuid::new_v4().simple());
    let binding_c = server.bind(
        native_session_c.clone(),
        &BrowserToolRuntime {
            registry: registry.clone(),
            session_id: session,
            runtime_id: Uuid::new_v4(),
        },
    )?;
    report.startup_ok(
        "native session bindings",
        format!("{native_session} -> {session}, {native_session_b} -> {session_b}"),
    );

    let tool = ToolClient::connect(&tools_url, &browser_token)?;
    let tool_b = ToolClient::connect(&tools_url, &browser_token)?;
    let tool_c = ToolClient::connect(&tools_url, &browser_token)?;
    // Setup smoke test: each native session must resolve through its own
    // connection before any check runs, so a later refusal is meaningful.
    for (label, client, native) in [
        ("first", &tool, native_session.as_str()),
        ("second", &tool_b, native_session_b.as_str()),
        ("third", &tool_c, native_session_c.as_str()),
    ] {
        let pages = expect_ok(&client.list(Uuid::new_v4(), native)?)?
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the {label} tool connection answered a non-array"))?;
        if !pages.is_empty() {
            bail!("a freshly bound native session already shared pages: {pages:?}");
        }
    }
    report.startup_ok(
        "browser tool connections",
        "two native WebSocket clients authenticated, and each native session resolves through its own connection".to_owned(),
    );

    report.versions["daemonAddress"] = json!(address);
    report.versions["browserToolEndpoint"] = json!(BROWSER_TOOL_ENDPOINT);
    report.versions["browserTokenLength"] = json!(browser_token.len());
    report.versions["nativeSessionCount"] = json!(2);

    Ok(Harness {
        dir: dir.clone(),
        address,
        tools_url,
        daemon_token,
        browser_token,
        registry,
        server,
        session,
        session_b,
        runtime_b,
        native_session,
        native_session_b,
        native_session_c,
        active_runtime: runtime,
        binding: Some(binding),
        binding_b: Some(binding_b),
        binding_c: Some(binding_c),
        superseded: None,
        owner,
        owner_b,
        tool: Some(tool),
        tool_b: Some(tool_b),
        tool_c: Some(tool_c),
        raw_owner: None,
        shutdown,
        serve_rx,
        serve_thread: Some(serve_thread),
    })
}

fn run_checks(e2e: &mut E2E) {
    // -- 1 -------------------------------------------------------------------
    e2e.check(
        "an-unshared-mapping-lists-nothing-and-refuses-an-action",
        |h| {
            let (result, frame) = h.tool()?.list_frame(Uuid::new_v4(), &h.native_session)?;
            let pages = expect_ok(&result)?
                .as_array()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("the list answer was not an array"))?;
            if !pages.is_empty() {
                bail!("a session with no shared page listed {pages:?}");
            }
            if frame.contains(&h.daemon_token) || frame.contains(&h.browser_token) {
                bail!("the list reply leaked a credential: {frame}");
            }
            let outcome = h.tool()?.invoke(
                Uuid::new_v4(),
                &h.native_session,
                Uuid::new_v4(),
                Uuid::new_v4(),
                BrowserAction::Snapshot,
            )?;
            let message = expect_refused(&outcome, &["no live browser page", "not well formed"])?;
            // Nothing was offered to the owner: there is no "current page".
            if !h.owner.events_of("browserRequest").is_empty() {
                bail!("an unshared page still reached the fake owner");
            }
            Ok(format!(
                "the mapped session listed nothing and its first action was refused: {message}"
            ))
        },
    );

    // -- 2 -------------------------------------------------------------------
    e2e.check("an-unknown-opencode-session-is-refused", |h| {
        let unknown = format!("ses_unknown_{}", Uuid::new_v4().simple());
        let empty = String::new();
        // Far beyond any documented session-id bound, so it is refused as an
        // identity rather than as an unknown mapping.
        let oversized = "s".repeat(4096);
        let requests_before = h.owner.events_of("browserRequest").len();
        for session in [unknown.as_str(), empty.as_str(), oversized.as_str()] {
            let listed = h.tool()?.list(Uuid::new_v4(), session)?;
            expect_refused(&listed, &["no fintwind browser binding", "invalid opencode session"])?;
            let invoked = h.tool()?.invoke(
                Uuid::new_v4(),
                session,
                Uuid::new_v4(),
                Uuid::new_v4(),
                BrowserAction::Snapshot,
            )?;
            expect_refused(&invoked, &["no fintwind browser binding", "invalid opencode session"])?;
        }
        if h.owner.events_of("browserRequest").len() != requests_before {
            bail!("an unmapped session still reached the fake owner");
        }
        Ok("three unmapped identities (unknown, empty, over-long) were refused for list and invoke, and none reached a page owner".to_owned())
    });

    // -- 3 -------------------------------------------------------------------
    e2e.check("a-child-session-does-not-inherit-and-a-connection-cannot-switch", |h| {
        let child = format!("ses_child_{}", Uuid::new_v4().simple());
        let inherited = h.tool()?.list(Uuid::new_v4(), &child)?;
        expect_refused(&inherited, &["no fintwind browser binding"])?;
        // An explicit binding is what makes a child session work at all.
        let child_guard = h.server.bind(child.clone(), &h.native_runtime(h.session, h.active_runtime))?;
        let child_tool = ToolClient::connect(&h.tools_url, &h.browser_token)?;
        let (result, frame) = child_tool.list_frame(Uuid::new_v4(), &child)?;
        let value = expect_ok(&result)?;
        if !value.as_array().is_some_and(|pages| pages.is_empty()) {
            bail!("an explicitly bound child session did not resolve: {value}");
        }
        if frame.contains(&h.session.to_string()) || frame.contains(&h.active_runtime.to_string()) {
            bail!("the child session list named the Fintwind session or runtime: {frame}");
        }
        // One connection serves exactly one OpenCode session.
        let switched = h.tool()?.list(Uuid::new_v4(), &h.native_session_b)?;
        expect_refused(&switched, &["cannot change session binding"])?;
        // The same connection still serves its own session afterwards.
        expect_ok(&h.tool()?.list(Uuid::new_v4(), &h.native_session)?)?;
        drop(child_guard);
        drop(child_tool);
        Ok("an unmapped child is refused, an explicitly bound child resolves, and a connection cannot switch to another session".to_owned())
    });

    // -- 4 -------------------------------------------------------------------
    e2e.check("the-mapped-session-invokes-exactly-its-own-page", |h| {
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        let (listed, frame) = h.tool()?.list_frame(Uuid::new_v4(), &h.native_session)?;
        let pages = expect_ok(&listed)?
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the list answer was not an array"))?;
        if pages.len() != 1 {
            bail!("the mapped session listed {} pages: {pages:?}", pages.len());
        }
        let page = &pages[0];
        if !json_id(&page["pageId"], &scope.page_id) || !json_id(&page["grantId"], &scope.grant_id) {
            bail!("the listed page was not the shared one: {page}");
        }
        if !page["url"].as_str().is_some_and(|url| url.starts_with("https://fake-owner.invalid/page/")) {
            bail!("the listed page carried an unexpected url: {page}");
        }
        if frame.contains(&h.session.to_string()) || frame.contains(&h.active_runtime.to_string()) {
            bail!("the list reply named the Fintwind session or runtime: {frame}");
        }
        let outcome = h.tool()?.invoke(
            Uuid::new_v4(),
            &h.native_session,
            scope.page_id,
            scope.grant_id,
            BrowserAction::Snapshot,
        )?;
        let value = expect_ok(&outcome)?;
        if value["text"] != "fake-owner-snapshot" {
            bail!("the owner's answer did not come back unchanged: {value}");
        }
        // The daemon delivered the owner's own scope, not a guessed one.
        let delivered = h.owner.wait_event(
            |event| {
                event["event"] == "browserRequest"
                    && json_id(&event["scope"]["pageId"], &scope.page_id)
                    && json_id(&event["scope"]["grantId"], &scope.grant_id)
                    && json_id(&event["scope"]["sessionId"], &h.session)
                    && json_id(&event["scope"]["runtimeId"], &h.active_runtime)
            },
            Duration::from_secs(5),
        )?;
        if delivered["action"] != json!(BrowserAction::Snapshot) {
            bail!("the owner saw a different action: {delivered}");
        }
        if !h.owner.performed().is_empty() {
            bail!("a snapshot is observation, so nothing may be recorded as performed");
        }
        Ok("the mapped session listed only its own page without Fintwind ids, and its snapshot routed through the exact scope and back".to_owned())
    });

    // -- 5 -------------------------------------------------------------------
    e2e.check("another-fintwind-sessions-page-is-invisible", |h| {
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        let scope_b = h.share(&h.owner_b, h.session_b, h.runtime_b)?;
        let own = expect_ok(&h.tool()?.list(Uuid::new_v4(), &h.native_session)?)?;
        let own = own
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the list answer was not an array"))?;
        if own.len() != 1 || !json_id(&own[0]["pageId"], &scope.page_id) {
            bail!("the mapped session saw more than its own page: {own:?}");
        }
        let other = expect_ok(&h.tool_b.as_ref().unwrap().list(Uuid::new_v4(), &h.native_session_b)?)?;
        let other = other
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the list answer was not an array"))?;
        if other.len() != 1 || !json_id(&other[0]["pageId"], &scope_b.page_id) {
            bail!("the other session's connection saw more than its own page: {other:?}");
        }
        // The tool cannot widen its own scope by naming another task's page.
        let cross = h.tool()?.invoke(
            Uuid::new_v4(),
            &h.native_session,
            scope_b.page_id,
            scope_b.grant_id,
            BrowserAction::Snapshot,
        )?;
        expect_refused(&cross, &["no live browser page", "runtime"])?;
        let cross_click = h.tool()?.invoke(
            Uuid::new_v4(),
            &h.native_session,
            scope_b.page_id,
            scope_b.grant_id,
            BrowserAction::Click { selector: "#count".to_owned() },
        )?;
        expect_refused(&cross_click, &["no live browser page", "runtime"])?;
        // The other owner is reachable through its own mapping only.
        let legitimate = h.tool_b.as_ref().unwrap().invoke(
            Uuid::new_v4(),
            &h.native_session_b,
            scope_b.page_id,
            scope_b.grant_id,
            BrowserAction::Snapshot,
        )?;
        expect_ok(&legitimate)?;
        let delivered = h.owner_b.wait_event(
            |event| event["event"] == "browserRequest" && json_id(&event["scope"]["pageId"], &scope_b.page_id),
            Duration::from_secs(5),
        )?;
        if !json_id(&delivered["scope"]["sessionId"], &h.session_b) {
            bail!("the other owner was addressed with the wrong session: {delivered}");
        }        let leaked = h
            .owner
            .events_of("browserRequest")
            .into_iter()
            .filter(|event| json_id(&event["scope"]["pageId"], &scope_b.page_id))
            .count();
        if leaked != 0 {
            bail!("the first owner received {leaked} requests addressed to the other session's page");
        }
        Ok("each mapping listed and reached only its own page; naming the other session's page from this mapping was refused".to_owned())
    });

    // -- 6 -------------------------------------------------------------------
    e2e.check("a-wrong-or-superseded-grant-is-refused", |h| {
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        let cases = [
            (scope.page_id, Uuid::new_v4(), "a fresh, never-issued grant"),
            (Uuid::new_v4(), scope.grant_id, "an unknown page under a live grant"),
            (Uuid::nil(), scope.grant_id, "a nil page id"),
            (scope.page_id, Uuid::nil(), "a nil grant id"),
        ];
        for (page_id, grant_id, _label) in cases {
            let outcome = h.tool()?.invoke(
                Uuid::new_v4(),
                &h.native_session,
                page_id,
                grant_id,
                BrowserAction::Snapshot,
            )?;
            expect_refused(&outcome, &["no live browser page", "not well formed", "runtime"])?;
        }
        // A republished lease replaces the old grant for the same page.
        let stale = h.share(&h.owner, h.session, h.active_runtime)?;
        let stale_grant = scope.grant_id;
        let replaced = stale.page_id;
        let stale_call = h.tool()?.invoke(
            Uuid::new_v4(),
            &h.native_session,
            replaced,
            Uuid::new_v4(),
            BrowserAction::Snapshot,
        )?;
        expect_refused(&stale_call, &["no live browser page", "runtime"])?;
        let live = h.tool()?.invoke(
            Uuid::new_v4(),
            &h.native_session,
            replaced,
            stale.grant_id,
            BrowserAction::Snapshot,
        )?;
        expect_ok(&live)?;
        if stale_grant == stale.grant_id {
            bail!("the republished grant was not a fresh capability");
        }
        if !h.owner.performed().is_empty() {
            bail!("a refused scope still recorded a performed action");
        }
        Ok("wrong page, wrong grant, nil ids and an unknown grant were all refused, while the current lease still worked".to_owned())
    });

    // -- 7 -------------------------------------------------------------------
    e2e.check("the-browser-token-cannot-authenticate-the-desktop-endpoint", |h| {
        let hello = serde_json::to_value(&ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: h.browser_token.clone(),
            client_id: Uuid::new_v4(),
            resume_from: Vec::<ReplayCursor>::new(),
        })?;
        let reply = first_reply_text(&format!("ws://{}/v1", h.address), &hello, PROBE_TIMEOUT)?;
        let message: ServerMessage = serde_json::from_str(&reply)
            .with_context(|| format!("the desktop endpoint answered {reply}"))?;
        match message {
            ServerMessage::Rejected { message } => {
                if !message.to_lowercase().contains("authentication") {
                    bail!("the desktop endpoint refused with an unexpected reason: {message}");
                }
            }
            other => bail!("the desktop endpoint accepted the browser credential: {other:?}"),
        }
        if h.browser_token == h.daemon_token {
            bail!("the browser credential is the daemon credential");
        }
        Ok(format!(
            "the desktop endpoint rejected the browser credential; the two credentials are independent ({} and {} characters)",
            h.browser_token.len(),
            h.daemon_token.len()
        ))
    });

    // -- 8 -------------------------------------------------------------------
    e2e.check(
        "the-daemon-token-cannot-authenticate-the-browser-endpoint",
        |h| {
            let hello = serde_json::to_value(&BrowserToolMessage::Hello {
                version: BROWSER_TOOL_VERSION,
                token: h.daemon_token.clone(),
            })?;
            let reply = first_reply_text(&h.tools_url, &hello, PROBE_TIMEOUT)?;
            let message: BrowserToolReply = serde_json::from_str(&reply)
                .with_context(|| format!("the browser endpoint answered {reply}"))?;
            match message {
                BrowserToolReply::Rejected { message } => {
                    if !message.to_lowercase().contains("authentication") {
                        bail!("the browser endpoint refused with an unexpected reason: {message}");
                    }
                }
                other => bail!("the browser endpoint accepted the daemon credential: {other:?}"),
            }
            Ok("the browser endpoint rejected the daemon credential at the hello".to_owned())
        },
    );

    // -- 9 -------------------------------------------------------------------
    e2e.check("an-origin-header-is-refused-before-any-frame", |h| {
        let (status, body) = probe_origin(&h.address)?;
        if !status.contains("403") {
            bail!("the browser endpoint answered the origin handshake with `{status}`");
        }
        if !body.to_lowercase().contains("origin") {
            bail!("the origin refusal did not explain itself: {body}");
        }
        Ok(format!(
            "a browser-origin handshake was refused with {status}"
        ))
    });

    // -- 10 ------------------------------------------------------------------
    e2e.check("the-browser-endpoint-refuses-desktop-rpc-publish-and-shutdown", |h| {
        let session = h.session;
        let runtime = h.active_runtime;
        let denied: Vec<Value> = vec![
            serde_json::to_value(&ClientMessage::Request(Request {
                request_id: Uuid::new_v4(),
                session_id: session,
                runtime_id: runtime,
                command: Command::GetSettings,
            }))?,
            serde_json::to_value(&ClientMessage::BrowserPublish {
                pages: vec![BrowserShare {
                    scope: BrowserScope {
                        session_id: session,
                        runtime_id: runtime,
                        page_id: Uuid::new_v4(),
                        grant_id: Uuid::new_v4(),
                    },
                    url: "https://fake-owner.invalid/spoofed".to_owned(),
                    title: "spoofed".to_owned(),
                }],
            })?,
            serde_json::to_value(&ClientMessage::BrowserResult {
                request_id: Uuid::new_v4(),
                result: BrowserResult::Ok { value: json!({"issued": true, "forged": true}) },
            })?,
            serde_json::to_value(&ClientMessage::Shutdown)?,
        ];
        for message in &denied {
            let (frames, _closed) = tool_probe(&h.tools_url, &h.browser_token, std::slice::from_ref(message))?;
            let refused = frames.iter().find_map(|frame| {
                match serde_json::from_str::<BrowserToolReply>(frame) {
                    Ok(BrowserToolReply::Rejected { message }) => Some(message),
                    _ => None,
                }
            });
            match refused {
                Some(message) => {
                    if !message
                        .to_lowercase()
                        .contains("only browser list, open, invoke and cancel")
                    {
                        bail!("the browser endpoint refused a desktop message with `{message}`");
                    }
                }
                None => bail!("the browser endpoint did not refuse {message}"),
            }
        }
        // The daemon survived: the desktop owner still works and a fresh
        // browser connection still authenticates.
        h.owner.shares(session, runtime)?;
        ToolClient::connect(&h.tools_url, &h.browser_token)?;
        Ok("task RPC, page publish, a forged result and shutdown were each refused on the browser endpoint, and the daemon kept running".to_owned())
    });

    // -- 11 ------------------------------------------------------------------
    e2e.check("a-replaced-runtime-purges-the-old-mapping", |h| {
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        expect_ok(&h.tool()?.invoke(
            Uuid::new_v4(),
            &h.native_session,
            scope.page_id,
            scope.grant_id,
            BrowserAction::Snapshot,
        )?)?;
        // Replace the session runtime through the real daemon path.
        let replacement = Uuid::new_v4();
        h.owner.start_runtime(h.session, replacement)?;
        h.owner.wait_event(|event| event["event"] == "scopesRevoked", Duration::from_secs(10))?;
        // The old mapping still names the replaced runtime.
        expect_refused(
            &h.tool()?.invoke(
                Uuid::new_v4(),
                &h.native_session,
                scope.page_id,
                scope.grant_id,
                BrowserAction::Snapshot,
            )?,
            &["no live browser page", "runtime"],
        )?;
        // A native session already owned by this Fintwind session cannot be
        // pointed at another one.
        let stolen = h.server.bind(
            h.native_session.clone(),
            &h.native_runtime(h.session_b, h.runtime_b),
        );
        match stolen {
            Ok(_) => bail!("a native session was rebound to another Fintwind session"),
            Err(error) => {
                let message = error.to_string();
                if !message.contains("already belongs to another Fintwind session") {
                    bail!("the cross-task rebind was refused with an unexpected reason: {message}");
                }
            }
        }
        // Rebind to the replacement runtime. The connection that resolved the
        // old binding is retired, so its transport ends instead of answering.
        h.rebind(h.session, replacement)?;
        terminal_outcome(h.tool()?.list(Uuid::new_v4(), &h.native_session))?;
        let old_tool = h.take_tool();
        let tool = ToolClient::connect(&h.tools_url, &h.browser_token)?;
        let fresh = h.share(&h.owner, h.session, h.active_runtime)?;
        let pages = expect_ok(&tool.list(Uuid::new_v4(), &h.native_session)?)?
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the list answer was not an array"))?;
        if pages.len() != 1 || !json_id(&pages[0]["pageId"], &fresh.page_id) {
            bail!("the replacement binding listed the wrong pages: {pages:?}");
        }
        expect_ok(&tool.invoke(
            Uuid::new_v4(),
            &h.native_session,
            fresh.page_id,
            fresh.grant_id,
            BrowserAction::Snapshot,
        )?)?;
        h.put_tool(tool);
        drop(old_tool);
        Ok("the replaced runtime revoked the old grant, the stale connection was retired, and the rebind served only the new runtime".to_owned())
    });

    // -- 12 ------------------------------------------------------------------
    e2e.check("dropping-the-superseded-binding-keeps-the-replacement", |h| {
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        let guard = h
            .superseded
            .take()
            .ok_or_else(|| anyhow::anyhow!("no superseded binding was kept for this check"))?;
        drop(guard);
        let pages = expect_ok(&h.tool()?.list(Uuid::new_v4(), &h.native_session)?)?
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the list answer was not an array"))?;
        if pages.len() != 1 || !json_id(&pages[0]["pageId"], &scope.page_id) {
            bail!("dropping the old binding removed the replacement: {pages:?}");
        }
        expect_ok(&h.tool()?.invoke(
            Uuid::new_v4(),
            &h.native_session,
            scope.page_id,
            scope.grant_id,
            BrowserAction::Snapshot,
        )?)?;
        Ok("the late drop of the superseded guard left the replacement binding and its page intact".to_owned())
    });

    // -- 13 ------------------------------------------------------------------
    e2e.check("a-cancel-before-dispatch-refuses-the-late-invoke", |h| {
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        let requests_before = h.owner.events_of("browserRequest").len();
        let request_id = Uuid::new_v4();
        // The caller gives up before its own invoke is dispatched, the way a
        // signal channel aborting a tool call would.
        h.tool()?.cancel(request_id)?;
        let outcome = h.tool()?.invoke(
            request_id,
            &h.native_session,
            scope.page_id,
            scope.grant_id,
            BrowserAction::Click { selector: "#count".to_owned() },
        )?;
        expect_refused(&outcome, &["cancelled"])?;
        if h.owner.events_of("browserRequest").len() != requests_before {
            bail!("a cancel that arrived first still offered the action to the owner");
        }
        if !h.owner.performed().is_empty() {
            bail!("a cancelled-before-start request still ran");
        }
        Ok("the early cancel was remembered, so the late invoke was refused and never reached the page owner".to_owned())
    });

    // -- 14 ------------------------------------------------------------------
    e2e.check("a-cancel-during-approval-stops-the-owner", |h| {
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        let performed_before = h.owner.performed().len();
        let request_id = Uuid::new_v4();
        let tool = h.tool()?;
        let owner = &h.owner;
        let native_session = h.native_session.clone();
        let (delivered, result) = std::thread::scope(|scope_thread| {
            let pending = scope_thread.spawn(move || {
                tool.invoke(
                    request_id,
                    &native_session,
                    scope.page_id,
                    scope.grant_id,
                    BrowserAction::Click { selector: "#count".to_owned() },
                )
            });
            let delivered = owner.wait_event(
                |event| {
                    event["event"] == "browserRequest" && json_id(&event["scope"]["pageId"], &scope.page_id)
                },
                INVOKE_TIMEOUT,
            )?;
            // The caller aborts while the owner is still waiting for approval.
            tool.cancel(request_id)?;
            let result = pending
                .join()
                .map_err(|_| anyhow::anyhow!("the invoke thread panicked"))??;
            Ok::<_, anyhow::Error>((delivered, result))
        })?;
        expect_refused(&result, &["cancelled"])?;
        // The daemon must also tell the owner to stop acting.
        let internal = delivered["requestId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("the owner log carried no internal request id"))?
            .to_owned();
        let internal_id = Uuid::parse_str(&internal).context("the owner log carried an unreadable request id")?;
        h.owner.wait_event(
            |event| event["event"] == "browserCancel" && event["requestId"] == internal,
            Duration::from_secs(5),
        )?;
        // A late approval for a cancelled request changes nothing.
        h.owner.approve(internal_id);
        std::thread::sleep(Duration::from_millis(250));
        if h.owner.performed().len() != performed_before {
            bail!("a late approval still performed the cancelled action");
        }
        Ok("the caller's cancel failed the call, told the owner to stop, and a late approval had no effect".to_owned())
    });

    // -- 15 ------------------------------------------------------------------
    e2e.check("a-repeated-request-id-is-refused", |h| {
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        let request_id = Uuid::new_v4();
        expect_ok(&h.tool()?.invoke(
            request_id,
            &h.native_session,
            scope.page_id,
            scope.grant_id,
            BrowserAction::Snapshot,
        )?)?;
        let delivered_before = h
            .owner
            .events_of("browserRequest")
            .into_iter()
            .filter(|event| json_id(&event["scope"]["pageId"], &scope.page_id))
            .count();
        let repeat = h.tool()?.invoke(
            request_id,
            &h.native_session,
            scope.page_id,
            scope.grant_id,
            BrowserAction::Click { selector: "#count".to_owned() },
        )?;
        expect_refused(&repeat, &["already attempted", "retry"])?;
        let delivered_after = h
            .owner
            .events_of("browserRequest")
            .into_iter()
            .filter(|event| json_id(&event["scope"]["pageId"], &scope.page_id))
            .count();
        if delivered_after != delivered_before {
            bail!("a repeated request id was offered to the owner again");
        }
        if !h.owner.performed().is_empty() {
            bail!("a repeated request id performed an action");
        }
        Ok("the spent request id was refused with an observe-again error and reached no owner a second time".to_owned())
    });

    // -- 16 ------------------------------------------------------------------
    e2e.check("an-oversized-owner-result-is-capped", |h| {
        // A second GUI owner on a raw socket answers with an oversized value:
        // DaemonClient refuses that locally, so this is the only way to prove
        // the daemon-side bound.
        let mut raw = RawOwner::connect(&h.address, &h.daemon_token)?;
        let raw_scope = BrowserScope {
            session_id: h.session,
            runtime_id: h.active_runtime,
            page_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
        };
        raw.publish(vec![BrowserShare {
            scope: raw_scope.clone(),
            url: format!("https://fake-owner.invalid/page/{}", raw_scope.page_id),
            title: "raw owner page".to_owned(),
        }])?;
        wait_until("the raw owner's share is live", Duration::from_secs(10), || {
            raw.shares(h.session, h.active_runtime)
                .ok()
                .and_then(|shares| shares.iter().any(|share| share.scope == raw_scope).then_some(()))
        })?;
        let marker = format!("oversized-{}", "z".repeat(40_000));
        let tool = h.tool()?;
        let native_session = h.native_session.clone();
        let (result, frame) = std::thread::scope(|scope_thread| {
            let pending = scope_thread.spawn(move || {
                tool.invoke_frame(
                    Uuid::new_v4(),
                    &native_session,
                    raw_scope.page_id,
                    raw_scope.grant_id,
                    BrowserAction::Snapshot,
                )
            });
            let request = raw.wait_browser_request(&raw_scope, Duration::from_secs(10))?;
            raw.complete(
                request.request_id,
                BrowserResult::Ok { value: Value::String(marker.clone()) },
            )?;
            let result = pending
                .join()
                .map_err(|_| anyhow::anyhow!("the invoke thread panicked"))??;
            Ok::<_, anyhow::Error>(result)
        })?;
        expect_refused(&result, &["size limit", "exceeds"])?;
        if frame.contains("oversized") {
            bail!("the oversized payload crossed the browser endpoint: {} bytes", frame.len());
        }
        if frame.len() > 8 * 1024 {
            bail!("the capped reply was still {} bytes", frame.len());
        }
        // The raw owner's page was this check's own fixture; revoke it so later
        // checks see a clean share set.
        raw.publish(Vec::new())?;
        h.raw_owner = Some(raw);
        Ok("an owner answer above the wire bound was replaced by a size-limit error, and no payload crossed".to_owned())
    });

    // -- 17 ------------------------------------------------------------------
    e2e.check("unknown-fields-nil-ids-and-repeated-hello-are-refused", |h| {
        // Unknown fields widen the grammar; they are refused, not ignored.
        let unknown_field = json!({
            "type": "list",
            "requestId": Uuid::new_v4(),
            "sessionId": h.native_session,
            "surprise": true,
        });
        let (frames, _closed) =
            tool_probe(&h.tools_url, &h.browser_token, std::slice::from_ref(&unknown_field))?;
        let refused = frames
            .iter()
            .filter_map(|frame| serde_json::from_str::<BrowserToolReply>(frame).ok())
            .find_map(|reply| match reply {
                BrowserToolReply::Rejected { message } => Some(message),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("the unknown field was not refused: {frames:?}"))?;
        if !refused
            .to_lowercase()
            .contains("only browser list, open, invoke and cancel")
        {
            bail!("the unknown field was refused with `{refused}`");
        }
        // A nil request id cannot address a response.
        let nil_id = json!({
            "type": "list",
            "requestId": Uuid::nil(),
            "sessionId": h.native_session,
        });
        let (frames, _closed) = tool_probe(&h.tools_url, &h.browser_token, std::slice::from_ref(&nil_id))?;
        let answered = frames
            .iter()
            .filter_map(|frame| serde_json::from_str::<BrowserToolReply>(frame).ok())
            .find_map(|reply| match reply {
                BrowserToolReply::Result { result, .. } => Some(result),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("the nil request id was not answered: {frames:?}"))?;
        expect_refused(&answered, &["request id"])?;
        // An incompatible hello version is refused at the door.
        let wrong_version = json!({
            "type": "hello",
            "version": BROWSER_TOOL_VERSION + 1,
            "token": h.browser_token,
        });
        let reply = first_reply_text(&h.tools_url, &wrong_version, PROBE_TIMEOUT)?;
        let replied: BrowserToolReply = serde_json::from_str(&reply)?;
        match replied {
            BrowserToolReply::Rejected { message } => {
                if !message.to_lowercase().contains("authentication") {
                    bail!("a wrong hello version was refused with `{message}`");
                }
            }
            other => bail!("a wrong hello version was not refused: {other:?}"),
        }
        // A second hello on an established connection is a protocol error: the
        // connection ends without an answer.
        let (frames, closed) = tool_probe(
            &h.tools_url,
            &h.browser_token,
            &[serde_json::to_value(&BrowserToolMessage::Hello {
                version: BROWSER_TOOL_VERSION,
                token: h.browser_token.clone(),
            })?],
        )?;
        if !closed {
            bail!("a repeated hello did not end the connection");
        }
        if !frames.is_empty() {
            bail!("a repeated hello was answered instead of refused: {frames:?}");
        }
        // The endpoint still works afterwards.
        expect_ok(&h.tool()?.list(Uuid::new_v4(), &h.native_session)?)?;
        Ok("unknown fields, a nil request id, an incompatible version and a repeated hello were each refused, and the endpoint kept serving".to_owned())
    });

    // -- 18 ------------------------------------------------------------------
    e2e.check("a-large-list-is-refused-without-leaking-its-pages", |h| {
        // Sixteen legal pages is the most one connection may share, and each
        // URL is long but inside MAX_BROWSER_URL_BYTES. Serialized, that list
        // is an order of magnitude past the bridge's result bound, so the
        // endpoint must answer with a refusal envelope instead of a payload —
        // and must not let a page URL through in that refusal.
        let prefix = "https://fake-owner.invalid/";
        let url = format!("{prefix}{}", "p".repeat(4090 - prefix.len()));
        let pages: Vec<BrowserShare> = (0..MAX_BROWSER_PAGES_PER_CONNECTION)
            .map(|index| BrowserShare {
                scope: BrowserScope {
                    session_id: h.session,
                    runtime_id: h.active_runtime,
                    page_id: Uuid::new_v4(),
                    grant_id: Uuid::new_v4(),
                },
                url: url.clone(),
                title: format!("large list page {index}"),
            })
            .collect();
        let payload_estimate: usize = pages.iter().map(|page| page.url.len() + 256).sum();
        if payload_estimate <= MAX_BROWSER_RESULT_BYTES {
            bail!("the fixture no longer clears the result bound ({payload_estimate} bytes)");
        }
        h.owner.share_many(pages.clone())?;
        // The broker has to really hold all sixteen scopes before the list.
        let wanted: Vec<Uuid> = pages.iter().map(|page| page.scope.page_id).collect();
        wait_until(
            "the broker accepted all sixteen shares",
            Duration::from_secs(10),
            || {
                h.owner.shares(h.session, h.active_runtime).ok().and_then(|shares| {
                    (shares.len() == wanted.len()
                        && wanted
                            .iter()
                            .all(|id| shares.iter().any(|share| &share.scope.page_id == id)))
                    .then_some(())
                })
            },
        )?;
        let (listed, frame) = h.tool()?.list_frame(Uuid::new_v4(), &h.native_session)?;
        let message = expect_refused(&listed, &["size limit", "exceeds"])?;
        // An answer, never a timeout (a timeout fails this run), and an
        // envelope rather than a payload.
        if frame.len() > MAX_BROWSER_RESULT_BYTES {
            bail!("the refusal frame itself exceeded the result bound: {} bytes", frame.len());
        }
        if frame.len() * 8 > payload_estimate {
            bail!(
                "the refusal frame is {} bytes against a {payload_estimate}-byte list; it looks like payload, not a refusal",
                frame.len()
            );
        }
        for page in &pages {
            if frame.contains(&page.url) {
                bail!("the refused list still carried a page URL ({} bytes)", page.url.len());
            }
        }
        // The share set returns to one page, and the endpoint still serves it.
        let restored = h.share(&h.owner, h.session, h.active_runtime)?;
        let (again, _) = h.tool()?.list_frame(Uuid::new_v4(), &h.native_session)?;
        let restored_pages = expect_ok(&again)?
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the restored list was not an array"))?;
        if restored_pages.len() != 1 || !json_id(&restored_pages[0]["pageId"], &restored.page_id) {
            bail!("after the refused list the share set was not restored: {restored_pages:?}");
        }
        Ok(format!(
            "a {payload_estimate}-byte list of sixteen legal pages was refused with `{message}`; the {}-byte reply carried no page URL",
            frame.len()
        ))
    });

    // -- 19 ------------------------------------------------------------------
    e2e.check("a-reply-for-another-request-survives-while-one-awaits", |h| {
        // Two calls in flight on one connection: a mutation the owner has not
        // approved, and a list the endpoint answers right away. The list's
        // reply is written first, so awaiting the mutation has to hold that
        // frame instead of dropping it, re-reading it until the deadline, or
        // losing it for the await that wants it.
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        let tool = h.tool()?;
        let native_session = h.native_session.clone();
        let mutation_id = Uuid::new_v4();
        let list_id = Uuid::new_v4();
        let late_id = Uuid::new_v4();
        tool.send(&BrowserToolMessage::Invoke {
            request_id: mutation_id,
            session_id: native_session.clone(),
            page_id: scope.page_id,
            grant_id: scope.grant_id,
            action: BrowserAction::Click { selector: "#count".to_owned() },
        })?;
        let delivered = h.owner.wait_event(
            |event| {
                event["event"] == "browserRequest"
                    && json_id(&event["scope"]["pageId"], &scope.page_id)
            },
            INVOKE_TIMEOUT,
        )?;
        // The list is answered while the mutation is still pending; the cancel
        // for the mutation is the frame after it.
        tool.send(&BrowserToolMessage::List {
            request_id: list_id,
            session_id: native_session.clone(),
        })?;
        tool.cancel(mutation_id)?;
        // Awaiting the later frame must not consume the earlier one.
        let (cancelled, _) = tool.result_frame(mutation_id, INVOKE_TIMEOUT)?;
        expect_refused(&cancelled, &["cancelled"])?;
        // The held frame still answers its own request, whole.
        let (listed, raw) = tool.result_frame(list_id, LIST_TIMEOUT)?;
        let pages = expect_ok(&listed)?
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the held reply was not a list"))?;
        if !pages.iter().any(|page| json_id(&page["pageId"], &scope.page_id)) {
            bail!("the held reply lost its page: {raw}");
        }
        if !raw.contains(&scope.page_id.to_string()) {
            bail!("the held reply did not carry its own page id: {raw}");
        }
        // A fresh answer for the same connection must agree with the held one.
        let fresh = expect_ok(&tool.list(late_id, &native_session)?)?
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the fresh reply was not a list"))?;
        let held_ids: Vec<String> = pages
            .iter()
            .filter_map(|page| page["pageId"].as_str().map(str::to_owned))
            .collect();
        let fresh_ids: Vec<String> = fresh
            .iter()
            .filter_map(|page| page["pageId"].as_str().map(str::to_owned))
            .collect();
        if held_ids != fresh_ids {
            bail!("the held reply {held_ids:?} disagrees with the fresh reply {fresh_ids:?}");
        }
        // The connection keeps serving after a frame was held.
        expect_ok(&tool.list(late_id, &native_session)?)?;
        let internal = delivered["requestId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("the owner log carried no internal request id"))?
            .to_owned();
        h.owner.wait_event(
            |event| event["event"] == "browserCancel" && event["requestId"] == internal,
            Duration::from_secs(5),
        )?;
        if !h.owner.performed().is_empty() {
            bail!("the cancelled mutation still performed its action");
        }
        Ok("a reply for another request in flight was held whole and answered its own request, while the connection kept serving".to_owned())
    });

    // -- 20 ------------------------------------------------------------------
    // The launcher contract: an open is routed only to the one launcher the
    // GUI registered for the live session runtime. A page grant is never a
    // launcher, another session's launcher is never visible, two launchers
    // for one session runtime are refused instead of guessed, and a
    // launcher that disappears mid-flight fails its pending open.
    e2e.check("an-open-without-a-launcher-is-refused", |h| {
        // No launcher is registered for this session runtime, and a shared
        // page must never stand in for one: opening is not a page action.
        let page = h.share(&h.owner, h.session, h.active_runtime)?;
        let requests_before = h.owner.events_of("browserRequest").len();
        let performed_before = h.owner.performed().len();
        for url in [
            "https://example.invalid/never-opened",
            "ftp://example.invalid/not-http",
            "https://user:password@example.invalid/credentials",
            "::::not a url at all",
        ] {
            let outcome = h.tool()?.open(Uuid::new_v4(), &h.native_session, url)?;
            expect_refused(
                &outcome,
                &["launcher", "http and https", "credentials", "size limit"],
            )?;
        }
        if h.owner.events_of("browserRequest").len() != requests_before {
            bail!("an open with no registered launcher still reached the page owner");
        }
        if h.owner.performed().len() != performed_before {
            bail!("an open without a launcher still ran");
        }
        Ok(format!(
            "an open with no launcher was refused alike for a missing launcher, a non-HTTP scheme, embedded credentials and a malformed URL; page {} was never used as one",
            page.page_id
        ))
    });

    // -- 21 ------------------------------------------------------------------
    e2e.check("a-registered-launcher-receives-and-answers-the-open", |h| {
        // The GUI registers one launcher for the live session runtime. Its
        // page and grant ids are launcher identities, not a tab: they must
        // not appear in the page list.
        let host_scope = BrowserScope {
            session_id: h.session,
            runtime_id: h.active_runtime,
            page_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
        };
        h.owner.register_host(&host_scope)?;
        let listed = expect_ok(&h.tool()?.list(Uuid::new_v4(), &h.native_session)?)?
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the list answer was not an array"))?;
        if listed
            .iter()
            .any(|page| json_id(&page["pageId"], &host_scope.page_id))
        {
            bail!("the launcher's identity appeared in the page list: {listed:?}");
        }
        // An open is not the default-permission observation, so the owner
        // holds it for approval — the point where a real GUI's full-access
        // decision happens — and only then answers it.
        let url = "https://example.invalid/opened-by-launcher";
        let request_id = Uuid::new_v4();
        let tool = h.tool()?;
        let owner = &h.owner;
        let native_session = h.native_session.clone();
        let (delivered, result) = std::thread::scope(|scope_thread| {
            let pending = scope_thread.spawn(move || tool.open(request_id, &native_session, url));
            let delivered = owner.wait_event(
                |event| {
                    event["event"] == "browserRequest"
                        && json_id(&event["scope"]["pageId"], &host_scope.page_id)
                },
                INVOKE_TIMEOUT,
            )?;
            approve_held(owner, internal_request_id(&delivered)?)?;
            let result = pending
                .join()
                .map_err(|_| anyhow::anyhow!("the open thread panicked"))??;
            Ok::<_, anyhow::Error>((delivered, result))
        })?;
        let value = expect_ok(&result)?;
        if delivered["scope"] != json!(host_scope) {
            bail!(
                "the open was not routed to the launcher's exact scope: {}",
                delivered["scope"]
            );
        }
        if delivered["action"]["url"] != json!(url) {
            bail!("the open carried a different URL: {}", delivered["action"]);
        }
        if value["issued"] != json!(true) || value["fakeOwner"] != json!(true) {
            bail!("the launcher's answer did not survive the round trip: {value}");
        }
        h.owner.clear_host()?;
        Ok("the registered launcher received the open on its exact scope with the exact URL, held it for approval, and its answer came back unchanged".to_owned())
    });

    // -- 22 ------------------------------------------------------------------
    e2e.check("only-the-launcher-owner-may-answer-an-open", |h| {
        let host_scope = BrowserScope {
            session_id: h.session,
            runtime_id: h.active_runtime,
            page_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
        };
        h.owner.register_host(&host_scope)?;
        // A second desktop connection, so the forged answer comes from a
        // subscriber that is not the launcher's owner.
        let mut raw = RawOwner::connect(&h.address, &h.daemon_token)?;
        let request_id = Uuid::new_v4();
        let tool = h.tool()?;
        let owner = &h.owner;
        let native_session = h.native_session.clone();
        let (delivered, result) = std::thread::scope(|scope_thread| {
            let pending = scope_thread.spawn(move || {
                tool.open(
                    request_id,
                    &native_session,
                    "https://example.invalid/opened-by-launcher",
                )
            });
            let delivered = owner.wait_event(
                |event| {
                    event["event"] == "browserRequest"
                        && json_id(&event["scope"]["pageId"], &host_scope.page_id)
                },
                INVOKE_TIMEOUT,
            )?;
            let internal_id = internal_request_id(&delivered)?;
            // The other connection forges an answer for the open.
            raw.complete(
                internal_id,
                BrowserResult::Ok {
                    value: json!({"issued": true, "forged": true}),
                },
            )?;
            let refused = match raw.next_message(PROBE_TIMEOUT)? {
                ServerMessage::Rejected { message } => message,
                other => bail!("the daemon let another connection answer an open: {other:?}"),
            };
            if !refused
                .to_lowercase()
                .contains("only the gui connection that owns")
            {
                bail!("the forged answer was refused with `{refused}`");
            }
            // The launcher owner's own answer is the one that counts.
            approve_held(owner, internal_id)?;
            let result = pending
                .join()
                .map_err(|_| anyhow::anyhow!("the open thread panicked"))??;
            Ok::<_, anyhow::Error>((delivered, result))
        })?;
        let value = expect_ok(&result)?;
        if value.get("forged").is_some() {
            bail!("a forged answer from another connection completed the open: {value}");
        }
        if value["issued"] != json!(true) || value["fakeOwner"] != json!(true) {
            bail!("the owner's own answer did not complete the open: {value}");
        }
        if delivered["scope"] != json!(host_scope) {
            bail!(
                "the open was not routed to the launcher's exact scope: {}",
                delivered["scope"]
            );
        }
        h.owner.clear_host()?;
        Ok("another connection's forged answer was refused, and only the launcher owner's answer completed the open".to_owned())
    });

    // -- 23 ------------------------------------------------------------------
    e2e.check("an-open-cancelled-before-dispatch-is-refused", |h| {
        let host_scope = BrowserScope {
            session_id: h.session,
            runtime_id: h.active_runtime,
            page_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
        };
        h.owner.register_host(&host_scope)?;
        let requests_before = h.owner.events_of("browserRequest").len();
        let performed_before = h.owner.performed().len();
        let request_id = Uuid::new_v4();
        // The caller gives up before its own open is dispatched, the way a
        // signal channel aborting a tool call would.
        h.tool()?.cancel(request_id)?;
        let outcome = h.tool()?.open(
            request_id,
            &h.native_session,
            "https://example.invalid/never-opened",
        )?;
        expect_refused(&outcome, &["cancelled"])?;
        if h.owner.events_of("browserRequest").len() != requests_before {
            bail!("a cancelled-before-start open still reached the launcher");
        }
        if h.owner.performed().len() != performed_before {
            bail!("a cancelled-before-start open still ran");
        }
        h.owner.clear_host()?;
        Ok("the early cancel was remembered, so the late open was refused and never reached the launcher".to_owned())
    });

    // -- 24 ------------------------------------------------------------------
    e2e.check(
        "unregistering-the-launcher-fails-a-pending-open-but-keeps-pages",
        |h| {
            let moved = h.share(&h.owner, h.session, h.active_runtime)?;
            let host_scope = BrowserScope {
                session_id: h.session,
                runtime_id: h.active_runtime,
                page_id: Uuid::new_v4(),
                grant_id: Uuid::new_v4(),
            };
            h.owner.register_host(&host_scope)?;
            let performed_before = h.owner.performed().len();
            let request_id = Uuid::new_v4();
            let tool = h.tool()?;
            let owner = &h.owner;
            let native_session = h.native_session.clone();
            let url = "https://example.invalid/opened-then-revoked";
            let outcome = std::thread::scope(|scope_thread| {
                let pending =
                    scope_thread.spawn(move || tool.open(request_id, &native_session, url));
                let delivered = owner.wait_event(
                    |event| {
                        event["event"] == "browserRequest"
                            && json_id(&event["scope"]["pageId"], &host_scope.page_id)
                    },
                    INVOKE_TIMEOUT,
                )?;
                // Publishing a different page set must not cancel a pending
                // open: the connection's launcher scope stays covered, so
                // the open survives the publish.
                let republished = h.share(owner, h.session, h.active_runtime)?;
                // The GUI withdraws the launcher while the open is still
                // pending.
                owner.clear_host()?;
                let result = pending
                    .join()
                    .map_err(|_| anyhow::anyhow!("the open thread panicked"))??;
                Ok::<_, anyhow::Error>((delivered, result, republished))
            });
            // Even a failed observation must not leave a launcher that changes
            // the next check's cross-session precondition. Flush its removal
            // on the owner socket before propagating the original failure.
            owner.clear_host()?;
            owner.shares(h.session, h.active_runtime)?;
            let (delivered, result, republished) = outcome?;
            // The refusal names the launcher, not the page publish: the
            // publish left the pending open alone and the withdrawal ended
            // it.
            expect_refused(&result, &["launcher was unregistered"])?;
            let internal_id = internal_request_id(&delivered)?;
            h.owner.wait_event(
                |event| {
                    event["event"] == "browserCancel" && event["requestId"] == internal_id.to_string()
                },
                Duration::from_secs(5),
            )?;
            // The page shares survived the launcher revocation.
            let shares = h.owner.shares(h.session, h.active_runtime)?;
            if !shares.iter().any(|share| share.scope == republished) {
                bail!("the launcher revocation also took the page shares: {shares:?}");
            }
            if shares.iter().any(|share| share.scope == moved) {
                bail!("the republished page set still carried the replaced page: {shares:?}");
            }
            // A late approval cannot complete the revoked open.
            h.owner.approve(internal_id);
            std::thread::sleep(Duration::from_millis(250));
            if h.owner.performed().len() != performed_before {
                bail!("a late approval still performed the revoked open");
            }
            Ok("a page publish left the pending open alive, withdrawing the launcher failed it and told the owner to stop, and the page shares survived".to_owned())
        },
    );

    // -- 25 ------------------------------------------------------------------
    e2e.check(
        "a-duplicate-launcher-is-refused-and-another-session-launcher-is-invisible",
        |h| {
            // The other session's launcher is not this session's: an open
            // here is refused instead of reaching another session's
            // capability.
            let foreign = BrowserScope {
                session_id: h.session_b,
                runtime_id: h.runtime_b,
                page_id: Uuid::new_v4(),
                grant_id: Uuid::new_v4(),
            };
            h.owner_b.register_host(&foreign)?;
            let foreign_requests_before = h.owner_b.events_of("browserRequest").len();
            let outcome = h.tool()?.open(
                Uuid::new_v4(),
                &h.native_session,
                "https://example.invalid/cross-session",
            )?;
            expect_refused(&outcome, &["launcher"])?;
            if h.owner_b.events_of("browserRequest").len() != foreign_requests_before {
                bail!("another session's launcher received this session's open");
            }
            // A second connection registering a launcher for the same session
            // runtime would make the open target ambiguous, so it is
            // refused rather than one being guessed.
            let mut raw = RawOwner::connect(&h.address, &h.daemon_token)?;
            raw.send(&ClientMessage::BrowserHost {
                scope: Some(foreign.clone()),
            })?;
            match raw.next_message(PROBE_TIMEOUT)? {
                ServerMessage::BrowserShareRejected { scopes, message } => {
                    if scopes != vec![foreign.clone()] {
                        bail!("the refused registration carried unexpected scopes: {scopes:?}");
                    }
                    if !message.to_lowercase().contains("launcher") {
                        bail!("the duplicate launcher was refused with `{message}`");
                    }
                }
                other => bail!(
                    "the daemon accepted a second launcher for one session runtime: {other:?}"
                ),
            }
            // The refused registration changed nothing: the session's one
            // launcher still receives and answers its own open.
            let tool_b = h
                .tool_b
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("the second tool connection is not live"))?;
            let request_id = Uuid::new_v4();
            let owner_b = &h.owner_b;
            let native_session_b = h.native_session_b.clone();
            let (delivered, result) = std::thread::scope(|scope_thread| {
                let pending = scope_thread.spawn(move || {
                    tool_b.open(
                        request_id,
                        &native_session_b,
                        "https://example.invalid/opened-on-session-b",
                    )
                });
                let delivered = owner_b.wait_event(
                    |event| {
                        event["event"] == "browserRequest"
                            && json_id(&event["scope"]["pageId"], &foreign.page_id)
                    },
                    INVOKE_TIMEOUT,
                )?;
                approve_held(owner_b, internal_request_id(&delivered)?)?;
                let result = pending
                    .join()
                    .map_err(|_| anyhow::anyhow!("the open thread panicked"))??;
                Ok::<_, anyhow::Error>((delivered, result))
            })?;
            expect_ok(&result)?;
            if delivered["scope"] != json!(foreign) {
                bail!(
                    "the open was not routed to the registered launcher's exact scope: {}",
                    delivered["scope"]
                );
            }
            h.owner_b.clear_host()?;
            Ok("another session's launcher was invisible, a second launcher for one session runtime was refused, and the single launcher still served its own open".to_owned())
        },
    );

    // -- 26 ------------------------------------------------------------------
    e2e.check("a-stale-runtime-mapping-cannot-open", |h| {
        // A valid launcher exists for the active runtime...
        let host_scope = BrowserScope {
            session_id: h.session,
            runtime_id: h.active_runtime,
            page_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
        };
        h.owner.register_host(&host_scope)?;
        let requests_before = h.owner.events_of("browserRequest").len();
        // ...but the third native session is mapped to a runtime that never
        // started, so its open is refused before any GUI work, and the live
        // launcher is not a fallback.
        let outcome = h.tool_c()?.open(
            Uuid::new_v4(),
            &h.native_session_c,
            "https://example.invalid/stale-runtime",
        )?;
        expect_refused(&outcome, &["runtime"])?;
        if h.owner.events_of("browserRequest").len() != requests_before {
            bail!("a stale runtime mapping still reached the launcher");
        }
        h.owner.clear_host()?;
        Ok("a mapping to a never-started runtime was refused with no GUI work, even while a valid launcher existed for the active runtime".to_owned())
    });

    // -- 27 ------------------------------------------------------------------
    e2e.check(
        "a-page-grant-cannot-open-and-a-launcher-cannot-scroll",
        |h| {
            let page = h.share(&h.owner, h.session, h.active_runtime)?;
            let host_scope = BrowserScope {
                session_id: h.session,
                runtime_id: h.active_runtime,
                page_id: Uuid::new_v4(),
                grant_id: Uuid::new_v4(),
            };
            h.owner.register_host(&host_scope)?;
            // A page grant never authorizes an open: the action is routed
            // only to launchers.
            let outcome = h.tool()?.invoke(
                Uuid::new_v4(),
                &h.native_session,
                page.page_id,
                page.grant_id,
                BrowserAction::Open {
                    url: "https://example.invalid/via-page-grant".to_owned(),
                },
            )?;
            expect_refused(&outcome, &["launcher"])?;
            // A launcher never authorizes a page action.
            let outcome = h.tool()?.invoke(
                Uuid::new_v4(),
                &h.native_session,
                host_scope.page_id,
                host_scope.grant_id,
                BrowserAction::Scroll { delta_y: 700 },
            )?;
            expect_refused(&outcome, &["no live browser page"])?;
            // Scroll bounds: a zero delta and anything past the limit are
            // refused instead of being treated as a scroll.
            for delta in [0, 2_001, -2_001, i32::MAX, i32::MIN] {
                let outcome = h.tool()?.invoke(
                    Uuid::new_v4(),
                    &h.native_session,
                    page.page_id,
                    page.grant_id,
                    BrowserAction::Scroll { delta_y: delta },
                )?;
                expect_refused(&outcome, &["scroll"])?;
            }
            // The real wire form: a plugin spells the field `deltaY`, and
            // that exact JSON has to route as a scroll rather than fail to
            // parse.
            let wire_scroll = json!({"kind": "scroll", "deltaY": 700});
            if serde_json::to_value(BrowserAction::Scroll { delta_y: 700 })? != wire_scroll {
                bail!("the protocol does not spell a scroll as {wire_scroll}");
            }
            let request_id = Uuid::new_v4();
            h.tool()?.send_raw(&json!({
                "type": "invoke",
                "requestId": request_id,
                "sessionId": h.native_session,
                "pageId": page.page_id,
                "grantId": page.grant_id,
                "action": wire_scroll,
            }))?;
            let delivered = h.owner.wait_event(
                |event| {
                    event["event"] == "browserRequest"
                        && event["action"] == wire_scroll
                        && json_id(&event["scope"]["pageId"], &page.page_id)
                },
                INVOKE_TIMEOUT,
            )?;
            approve_held(&h.owner, internal_request_id(&delivered)?)?;
            let (result, _frame) = h.tool()?.result_frame(request_id, INVOKE_TIMEOUT)?;
            let value = expect_ok(&result)?;
            if value["issued"] != json!(true) {
                bail!("the scroll's answer did not survive the round trip: {value}");
            }
            h.owner.clear_host()?;
            Ok("a page grant could not open, a launcher could not scroll, out-of-bound deltas were refused, and the real `deltaY` wire scroll routed and answered".to_owned())
        },
    );

    // -- 28 ------------------------------------------------------------------
    e2e.check("dropping-the-binding-cancels-a-pending-invoke", |h| {
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        let performed_before = h.owner.performed().len();
        let request_id = Uuid::new_v4();
        let guard = h
            .binding
            .take()
            .ok_or_else(|| anyhow::anyhow!("no live binding was kept for this check"))?;
        let tool = h.tool()?;
        let owner = &h.owner;
        let native_session = h.native_session.clone();
        let (delivered, result) = std::thread::scope(|scope_thread| {
            let pending = scope_thread.spawn(move || {
                tool.invoke(
                    request_id,
                    &native_session,
                    scope.page_id,
                    scope.grant_id,
                    BrowserAction::Click { selector: "#count".to_owned() },
                )
            });
            let delivered = owner.wait_event(
                |event| {
                    event["event"] == "browserRequest" && json_id(&event["scope"]["pageId"], &scope.page_id)
                },
                INVOKE_TIMEOUT,
            )?;
            // The driver that owned the native session goes away.
            drop(guard);
            let result = match pending.join() {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("the invoke thread panicked")),
            };
            Ok::<_, anyhow::Error>((delivered, result))
        })?;
        terminal_outcome(result)?;
        let internal = delivered["requestId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("the owner log carried no internal request id"))?
            .to_owned();
        h.owner.wait_event(
            |event| event["event"] == "browserCancel" && event["requestId"] == internal,
            Duration::from_secs(5),
        )?;
        if h.owner.performed().len() != performed_before {
            bail!("the pending action still ran after the binding was dropped");
        }
        // The retired caller's transport ends; it cannot keep using the old
        // mapping through a socket the daemon no longer serves.
        terminal_outcome(h.tool()?.list(Uuid::new_v4(), &h.native_session))?;
        Ok("dropping the runtime binding ended the pending call, told the owner to stop, and left no usable connection".to_owned())
    });

    // -- 29 ------------------------------------------------------------------
    e2e.check("revoking-the-server-stops-new-connections-and-fails-pending", |h| {
        let scope = h.share(&h.owner, h.session, h.active_runtime)?;
        h.rebind(h.session, h.active_runtime)?;
        let performed_before = h.owner.performed().len();
        let old_tool = h.take_tool();
        let tool = ToolClient::connect(&h.tools_url, &h.browser_token)?;
        h.put_tool(tool);
        let request_id = Uuid::new_v4();
        let tool = h.tool()?;
        let owner = &h.owner;
        let native_session = h.native_session.clone();
        let (delivered, result) = std::thread::scope(|scope_thread| {
            let pending = scope_thread.spawn(move || {
                tool.invoke(
                    request_id,
                    &native_session,
                    scope.page_id,
                    scope.grant_id,
                    BrowserAction::Click { selector: "#count".to_owned() },
                )
            });
            let delivered = owner.wait_event(
                |event| {
                    event["event"] == "browserRequest" && json_id(&event["scope"]["pageId"], &scope.page_id)
                },
                INVOKE_TIMEOUT,
            )?;
            // The private process that owned this capability exits.
            h.server.revoke();
            let result = match pending.join() {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("the invoke thread panicked")),
            };
            Ok::<_, anyhow::Error>((delivered, result))
        })?;
        terminal_outcome(result)?;
        let internal = delivered["requestId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("the owner log carried no internal request id"))?
            .to_owned();
        h.owner.wait_event(
            |event| event["event"] == "browserCancel" && event["requestId"] == internal,
            Duration::from_secs(5),
        )?;
        if h.owner.performed().len() != performed_before {
            bail!("the revoked server's pending action still ran");
        }
        // No new connection can use the revoked credential.
        let hello = serde_json::to_value(&BrowserToolMessage::Hello {
            version: BROWSER_TOOL_VERSION,
            token: h.browser_token.clone(),
        })?;
        let reply = first_reply_text(&h.tools_url, &hello, PROBE_TIMEOUT)?;
        let replied: BrowserToolReply = serde_json::from_str(&reply)?;
        match replied {
            BrowserToolReply::Rejected { .. } => {}
            other => bail!("a revoked credential still authenticated: {other:?}"),
        }
        terminal_outcome(h.tool()?.list(Uuid::new_v4(), &h.native_session))?;
        drop(old_tool);
        Ok("revoking the capability ended the pending call, told the owner to stop, and refused every new connection".to_owned())
    });

    // -- 30 ------------------------------------------------------------------
    e2e.check("the-report-holds-no-credentials", |h| {
        let owner_events = h.dir.join("owner-events.json");
        let mut combined = h.owner.events();
        combined.append(&mut h.owner_b.events());
        let text =
            serde_json::to_string_pretty(&combined).context("could not serialize the owner log")?;
        fs::write(&owner_events, format!("{text}\n")).context("could not write the owner log")?;
        let report_text =
            fs::read_to_string(h.dir.join("report.json")).context("could not read the report")?;
        let owner_text =
            fs::read_to_string(&owner_events).context("could not read the owner log")?;
        if report_text.contains(&h.daemon_token) || owner_text.contains(&h.daemon_token) {
            bail!("a persisted artifact holds the daemon token");
        }
        if report_text.contains(&h.browser_token) || owner_text.contains(&h.browser_token) {
            bail!("a persisted artifact holds the browser token");
        }
        // Every check this run defined must have been recorded.
        let recorded = report_text.matches("\"name\":").count();
        if recorded != EXPECTED_CHECKS - 1 {
            bail!(
                "expected {} recorded checks before the last one, found {recorded}",
                EXPECTED_CHECKS - 1
            );
        }
        Ok(
            "no credential appears in the report or the owner log, and every defined check ran"
                .to_owned(),
        )
    });
}

// ---------------------------------------------------------------------------
// Cleanup and entry point
// ---------------------------------------------------------------------------

/// Tear every owned transport down in dependency order. Each step is recorded,
/// so a run that leaked a socket, a thread or a process is visible in the
/// report instead of hidden by a forced exit.
fn cleanup(e2e: &mut E2E) {
    let harness = &mut e2e.harness;
    // Dropping a tool client drops its outgoing sender, which ends its pump
    // thread and closes the socket.
    harness.tool = None;
    harness.tool_b = None;
    harness.tool_c = None;
    e2e.report.cleanup_ok("browser tool connections closed");

    harness.raw_owner = None;
    e2e.report.cleanup_ok("raw owner socket closed");

    // Drop the live guard first, so a late drop of the superseded one is what
    // a real driver would produce.
    harness.binding = None;
    harness.binding_b = None;
    harness.binding_c = None;
    harness.superseded = None;
    e2e.report.cleanup_ok("runtime binding guards dropped");

    harness.server.revoke();
    e2e.report.cleanup_ok("browser tool server revoked");

    harness.owner.client.disconnect();
    harness.owner_b.client.disconnect();
    e2e.report.cleanup_ok("desktop owners disconnected");
    harness.owner.shutdown();
    harness.owner_b.shutdown();
    e2e.report.cleanup_ok("owner threads joined");

    harness.shutdown.store(true, Ordering::Release);
    match harness.serve_rx.recv_timeout(Duration::from_secs(15)) {
        Ok(summary) => {
            let joined = harness.serve_thread.take().map(|thread| {
                thread
                    .join()
                    .map(|()| summary)
                    .map_err(|_| "the serve thread panicked".to_owned())
            });
            match joined {
                Some(Ok(summary)) => e2e.report.cleanup_ok_with("daemon shut down", summary),
                Some(Err(error)) => e2e.report.cleanup_failed("daemon serve thread", error),
                None => e2e.report.cleanup_failed(
                    "daemon serve thread",
                    "the serve thread handle was missing".to_owned(),
                ),
            }
        }
        Err(_) => e2e.report.cleanup_failed(
            "daemon shutdown",
            "the serve thread did not exit within 15s".to_owned(),
        ),
    }
}

fn run() -> anyhow::Result<bool> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| anyhow::anyhow!("could not resolve the repository root from {manifest:?}"))?
        .to_path_buf();
    let run_id = Uuid::new_v4().to_string();
    let dir = root
        .join("target")
        .join("browser-tools-e2e")
        .join("runs")
        .join(&run_id);
    fs::create_dir_all(&dir).context("could not create the e2e report directory")?;

    let mut report = Report::new(&run_id);
    report.artifacts = json!({
        "report": dir.join("report.json").display().to_string(),
        "ownerEvents": dir.join("owner-events.json").display().to_string(),
    });
    let source = manifest.join("examples").join("browser_tools_e2e.rs");
    report.versions["exampleSource"] = json!(source.display().to_string());
    report.versions["exampleSourceSha256"] = json!(sha256_file(&source)?);
    if let Ok(exe) = std::env::current_exe() {
        report.versions["exampleExe"] = json!(exe.display().to_string());
        report.versions["exampleExeSha256"] = json!(sha256_file(&exe)?);
    }
    // The report's build hash is only evidence if the helper is correct.
    let known = sha256_hex(b"abc");
    if known != "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" {
        report.errors.push(format!(
            "the report hash helper produced {known} for a known digest"
        ));
    }
    report
        .write(&dir)
        .context("could not write the initial report")?;

    let finished = Arc::new(AtomicBool::new(false));
    let watchdog = spawn_watchdog(finished.clone());

    let harness = match build_harness(&mut report, &dir) {
        Ok(harness) => harness,
        Err(error) => {
            report.status = "failed".to_owned();
            report.finished_at = Utc::now().to_rfc3339();
            let _ = report.write(&dir);
            finished.store(true, Ordering::Release);
            let _ = watchdog.join();
            return Err(error);
        }
    };
    let address = harness.address.clone();
    let mut e2e = E2E {
        dir: dir.clone(),
        report,
        harness,
    };

    // The ready line carries the endpoint and the report location only; the
    // credentials stay in this process.
    println!(
        "{}",
        serde_json::to_string(&json!({
            "kind": "browser-tools-e2e",
            "address": address,
            "reportDir": dir.display().to_string(),
            "pid": std::process::id(),
        }))
        .context("could not serialize the ready line")?
    );
    std::io::stdout().flush().ok();

    run_checks(&mut e2e);
    cleanup(&mut e2e);

    let passed_checks = e2e
        .report
        .checks
        .iter()
        .filter(|check| check.status == "passed")
        .count();
    if e2e.report.checks.len() != EXPECTED_CHECKS {
        e2e.report.errors.push(format!(
            "expected {EXPECTED_CHECKS} behavior checks; ran {}",
            e2e.report.checks.len()
        ));
    }
    let healthy = e2e.report.errors.is_empty()
        && e2e
            .report
            .checks
            .iter()
            .all(|check| check.status == "passed")
        && e2e.report.cleanup.iter().all(|step| step.status == "ok");
    e2e.report.status = if healthy { "passed" } else { "failed" }.to_owned();
    e2e.report.finished_at = Utc::now().to_rfc3339();
    if let Err(error) = e2e.report.write(&dir) {
        eprintln!("[X] could not write the final report: {error:#}");
    }
    println!(
        "[{}] {passed_checks}/{} checks; report: {}",
        if healthy { "PASS" } else { "FAIL" },
        e2e.report.checks.len(),
        dir.join("report.json").display()
    );
    std::io::stdout().flush().ok();
    finished.store(true, Ordering::Release);
    let _ = watchdog.join();
    Ok(healthy)
}

fn main() {
    let code = match run() {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => {
            eprintln!("[X] the browser tools e2e could not complete: {error:#}");
            2
        }
    };
    std::process::exit(code);
}

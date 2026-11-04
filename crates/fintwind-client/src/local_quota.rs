//! Desktop-local subscription sources: the Claude Code and Codex sign-ins on
//! the machine running the desktop app. Credential files are
//! parsed in memory and never logged or sent over remote IPC; subprocesses are the
//! CLIs' own non-model queries run with a bounded timeout in a directory we
//! own. Renewal stays the CLIs' own job: an expired Codex token is reported
//! as needing Codex, never refreshed or written back here. No remote IPC
//! carries credentials, whatever workspace or
//! remote daemon is selected.
//!
//! Claude `/usage` parsing and the Codex `auth.json`/`wham/usage` shapes are
//! adapted from Magpie (MIT, see docs/provider-quota-upstream-license.txt).
//! Newer Claude Code versions answer `-p /usage` with only the session cost.
//! Their subscription windows come from the interactive `/usage` panel,
//! following Magpie's `claudetui` helper.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

use base64::Engine as _;
use chrono::Datelike as _;
use regex::Regex;
use sha2::{Digest, Sha256};

use fintwind_protocol::provider_quota::{
    ProviderQuota, QuotaBalance, QuotaStatus, QuotaWindow, QuotaWindowKind, is_local_quota_source,
};
use serde_json::Value;

/// Desktop-local transport seam. Deliberately not Debug: contains secrets.
pub struct QuotaRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<String>,
    pub body: Option<String>,
}

pub struct QuotaResponse {
    pub status: u16,
    pub body: String,
    pub retry_after: Option<u64>,
}

const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
/// Magpie gives the initial prompt 45 seconds and the usage panel 20 seconds.
const CLAUDE_TUI_TIMEOUT: Duration = Duration::from_secs(65);
const WHAM_TIMEOUT_SECS: u64 = 15;
const CODEX_BASE: &str = "https://chatgpt.com/backend-api";

/// File, CLI and HTTP access for a machine-local subscription query.
pub trait LocalBackend {
    fn read_file(&self, path: &Path) -> Option<Vec<u8>>;
    fn home_dir(&self) -> Option<PathBuf>;
    fn env_var(&self, name: &str) -> Option<String>;
    fn find_binary(&self, name: &str) -> Option<PathBuf>;
    fn now(&self) -> i64;
    fn run_capture(
        &self,
        program: &Path,
        args: &[&str],
        remove_env: &[&str],
        extra_env: &[(&str, &str)],
        workdir: &Path,
        timeout: Duration,
    ) -> Option<CommandOutput>;
    /// Run the interactive CLI in an exclusive pseudo-terminal and read one
    /// `/usage` panel from its screen. The default skips the PTY stage;
    /// fixtures script it.
    fn run_tui(
        &self,
        _program: &Path,
        _args: &[&str],
        _remove_env: &[&str],
        _extra_env: &[(&str, &str)],
        _workdir: &Path,
        _timeout: Duration,
    ) -> Option<crate::claude_pty::TuiTranscript> {
        None
    }
    fn http(&self, request: &QuotaRequest) -> anyhow::Result<QuotaResponse>;
    fn temp_dir(&self) -> PathBuf;
}

#[derive(Clone, Debug)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// The desktop machine: files, processes and HTTPS in this process.
/// Never a remote daemon: the caller runs on the desktop itself.
pub struct SystemBackend;

fn plain_command(program: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let candidate = dir.join(format!("{name}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn collect_output(
    mut child: std::process::Child,
    timeout: Duration,
) -> Option<std::process::Output> {
    use std::sync::mpsc;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let (stdout_tx, stdout_rx) = mpsc::channel();
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.read_to_end(&mut bytes);
        let _ = stdout_tx.send(result.map(|_| bytes));
    });
    let (stderr_tx, stderr_rx) = mpsc::channel();
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr.read_to_end(&mut bytes);
        let _ = stderr_tx.send(result.map(|_| bytes));
    });
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let remaining = || timeout.saturating_sub(started.elapsed());
    let stdout = stdout_rx.recv_timeout(remaining()).ok()?.ok()?;
    let stderr = stderr_rx.recv_timeout(remaining()).ok()?.ok()?;
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    Some(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

impl LocalBackend for SystemBackend {
    fn read_file(&self, path: &Path) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    fn home_dir(&self) -> Option<PathBuf> {
        dirs::home_dir()
    }

    fn env_var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn find_binary(&self, name: &str) -> Option<PathBuf> {
        find_executable(name)
    }

    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn run_capture(
        &self,
        program: &Path,
        args: &[&str],
        remove_env: &[&str],
        extra_env: &[(&str, &str)],
        workdir: &Path,
        timeout: Duration,
    ) -> Option<CommandOutput> {
        let mut command = plain_command(program);
        command.args(args.iter().copied());
        command.current_dir(workdir);
        command.stdin(std::process::Stdio::null());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        for name in remove_env {
            command.env_remove(name);
        }
        for (name, value) in extra_env {
            command.env(name, value);
        }
        if let Ok(child) = command.spawn() {
            let output = collect_output(child, timeout)?;
            return Some(CommandOutput {
                success: output.status.success(),
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        None
    }

    fn http(&self, request: &QuotaRequest) -> anyhow::Result<QuotaResponse> {
        let (status, body, retry_after) = fintwind_protocol::http::http_json(
            request.method,
            &request.url,
            &request.headers,
            request.body.as_deref(),
            WHAM_TIMEOUT_SECS,
        )?;
        Ok(QuotaResponse {
            status,
            body,
            retry_after,
        })
    }

    fn run_tui(
        &self,
        program: &Path,
        args: &[&str],
        remove_env: &[&str],
        extra_env: &[(&str, &str)],
        workdir: &Path,
        timeout: Duration,
    ) -> Option<crate::claude_pty::TuiTranscript> {
        crate::claude_pty::spawn_tui_session(&crate::claude_pty::TuiRequest {
            program: program.to_owned(),
            args: args.iter().map(|s| s.to_string()).collect(),
            remove_env: remove_env.iter().map(|s| s.to_string()).collect(),
            extra_env: extra_env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            workdir: workdir.to_owned(),
            timeout,
        })
    }

    fn temp_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
}

/// Caller-owned desktop quota cache: account isolation, in-flight
/// de-duplication across threads, failure cooldowns with Retry-After, and
/// force semantics that never cross a 429 floor. Repeated board opens and
/// concurrent same-source reads share one CLI run and one vendor request;
/// an account change or logout clears the old reading instead of reusing
/// it. Blocking; call off the UI thread.
#[derive(Default)]
pub struct LocalQuotaCache {
    slots: Mutex<HashMap<String, Arc<Slot>>>,
}

struct Slot {
    entry: Mutex<CachedLocal>,
    in_flight: Mutex<bool>,
    changed: Condvar,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            entry: Mutex::new(CachedLocal::default()),
            in_flight: Mutex::new(false),
            changed: Condvar::new(),
        }
    }
}

#[derive(Default)]
struct CachedLocal {
    identity: Option<[u8; 32]>,
    file_fingerprint: Option<[u8; 32]>,
    last: Option<ProviderQuota>,
    good: Option<ProviderQuota>,
    next_manual: i64,
    next_auto: i64,
}

impl LocalQuotaCache {
    /// Answer `anthropic`/`openai` from the desktop machine's own sign-in.
    /// Every other ID is unsupported here: server providers keep flowing
    /// through the workspace server or daemon.
    pub fn fetch(&self, provider: &str, force: bool) -> ProviderQuota {
        self.fetch_with(&SystemBackend, provider, force)
    }

    /// Same as [`LocalQuotaCache::fetch`] against an explicit backend; the
    /// black-box harness drives it with fixtures.
    pub fn fetch_with(
        &self,
        backend: &impl LocalBackend,
        provider: &str,
        force: bool,
    ) -> ProviderQuota {
        if !is_local_quota_source(provider) {
            return ProviderQuota::empty(QuotaStatus::Unsupported);
        }
        let slot = self
            .slots
            .lock()
            .unwrap()
            .entry(provider.to_owned())
            .or_default()
            .clone();
        // Fast path: a fresh cooldown with unchanged credential files runs
        // no CLI at all. Otherwise join the single flight, or become it.
        loop {
            let now = backend.now();
            let entry = slot.entry.lock().unwrap();
            let floor = if force {
                entry.next_manual
            } else {
                entry.next_auto
            };
            if now < floor
                && entry.identity.is_some()
                && let Some(last) = entry.last.clone()
                && login_files_fingerprint(backend, provider) == entry.file_fingerprint
            {
                return last;
            }
            drop(entry);
            let mut in_flight = slot.in_flight.lock().unwrap();
            if !*in_flight {
                *in_flight = true;
                break;
            }
            let _guard = slot.changed.wait(in_flight).unwrap();
            drop(_guard);
        }
        let reading = self.query_fresh(&slot, backend, provider, force);
        *slot.in_flight.lock().unwrap() = false;
        slot.changed.notify_all();
        reading
    }

    /// Full login plus query path. Runs with no cache locks held.
    fn query_fresh(
        &self,
        slot: &Arc<Slot>,
        backend: &impl LocalBackend,
        provider: &str,
        force: bool,
    ) -> ProviderQuota {
        let now = backend.now();
        let login = match login_for(backend, provider, now) {
            Ok(login) => login,
            Err(status) => {
                let mut entry = slot.entry.lock().unwrap();
                if matches!(
                    status,
                    QuotaStatus::NeedsAuth
                        | QuotaStatus::AccessDenied
                        | QuotaStatus::NoCredential
                        | QuotaStatus::Unsupported
                ) {
                    *entry = CachedLocal::default();
                } else {
                    // An unreadable login cannot authorize a cached Ready
                    // result on the next fast path. Keep good data only for
                    // a later successful re-identification of this account.
                    entry.last = None;
                    entry.file_fingerprint = None;
                }
                return ProviderQuota::empty(status);
            }
        };
        let identity: [u8; 32] =
            Sha256::digest(format!("{provider}\0{}\0{}", login.id, login.secret).as_bytes()).into();
        {
            let mut entry = slot.entry.lock().unwrap();
            if entry.identity != Some(identity) {
                *entry = CachedLocal {
                    identity: Some(identity),
                    ..Default::default()
                };
            }
            let now = backend.now();
            if now
                < if force {
                    entry.next_manual
                } else {
                    entry.next_auto
                }
            {
                if let Some(last) = entry.last.clone() {
                    return last;
                }
            }
        }
        let (mut reading, retry_after) = query_login(backend, provider, &login, now);
        reading.service = Some(login.service.clone());
        reading.account = Some(login.account.clone());
        // The account may have changed while the query ran. Never publish the
        // reading under the new account. The recheck is file-only: no second
        // CLI run, no network.
        if !login_identity_unchanged(backend, provider, &login, now) {
            *slot.entry.lock().unwrap() = CachedLocal::default();
            return ProviderQuota::empty(QuotaStatus::Unavailable);
        }
        let now = backend.now();
        let cooldown = if reading.status == QuotaStatus::RateLimited {
            retry_after.unwrap_or(60).max(60).min(i64::MAX as u64) as i64
        } else {
            15
        };
        let mut entry = slot.entry.lock().unwrap();
        entry.next_manual = now.saturating_add(cooldown);
        entry.next_auto = now.saturating_add(cooldown.max(60));
        entry.file_fingerprint = login_files_fingerprint(backend, provider);
        if reading.status == QuotaStatus::Ready {
            reading.measured_at = Some(now);
            entry.good = Some(reading.clone());
        } else if matches!(
            reading.status,
            QuotaStatus::Unavailable | QuotaStatus::RateLimited
        ) {
            if let Some(good) = entry.good.as_ref().filter(|g| {
                g.measured_at
                    .is_some_and(|at| now.saturating_sub(at) < 86400)
            }) {
                let status = reading.status;
                reading = good.clone();
                reading.status = status;
            }
        } else {
            entry.good = None;
        }
        reading.retry_at = (reading.status != QuotaStatus::Ready).then_some(entry.next_auto);
        entry.last = Some(reading.clone());
        reading
    }
}

/// Fingerprint of the credential files a login reads, without spawning a
/// CLI: the fast path only trusts a fresh cooldown while these bytes are
/// unchanged. `None` when no file exists.
fn login_files_fingerprint(backend: &impl LocalBackend, provider: &str) -> Option<[u8; 32]> {
    let path = match provider {
        "anthropic" => backend
            .home_dir()?
            .join(".claude")
            .join(".credentials.json"),
        "openai" => codex_home(backend)?.join("auth.json"),
        _ => return None,
    };
    Some(Sha256::digest(backend.read_file(&path)?).into())
}

/// A resolved machine-local login: enough to query, nothing secret retained
/// past the query. `id` isolates the cache per account; `secret` feeds only
/// the identity hash and the request itself.
struct LocalLogin {
    id: String,
    account: String,
    service: String,
    plan: Option<String>,
    secret: String,
    account_id: String,
}

fn login_for(
    backend: &impl LocalBackend,
    provider: &str,
    now: i64,
) -> Result<LocalLogin, QuotaStatus> {
    match provider {
        "anthropic" => claude_login(backend),
        "openai" => codex_login(backend, now),
        _ => Err(QuotaStatus::Unsupported),
    }
}

fn query_login(
    backend: &impl LocalBackend,
    provider: &str,
    login: &LocalLogin,
    now: i64,
) -> (ProviderQuota, Option<u64>) {
    match provider {
        "anthropic" => (claude_usage(backend, login, now), None),
        "openai" => codex_usage(backend, login, now),
        _ => (ProviderQuota::empty(QuotaStatus::Unsupported), None),
    }
}

fn login_identity_unchanged(
    backend: &impl LocalBackend,
    provider: &str,
    login: &LocalLogin,
    now: i64,
) -> bool {
    match provider {
        "anthropic" => claude_identity_string(backend, &login.account) == login.id,
        "openai" => matches!(codex_login(backend, now), Ok(next) if next.id == login.id),
        _ => false,
    }
}

/// The cache identity for a Claude account: the account plus a fingerprint
/// of the credential file, so a refresh or a different sign-in invalidates,
/// never reuses, the reading. File-only, safe to recheck after a query.
fn claude_identity_string(backend: &impl LocalBackend, account: &str) -> String {
    let mut id = format!("claude:{account}");
    if let Some(home) = backend.home_dir()
        && let Some(raw) = backend.read_file(&home.join(".claude").join(".credentials.json"))
    {
        let fingerprint: [u8; 32] = Sha256::digest(&raw).into();
        id = format!("claude:{account}:{}", hex32(&fingerprint));
    }
    id
}

// ---- Claude Code -----------------------------------------------------------

fn claude_binary(backend: &impl LocalBackend) -> Option<PathBuf> {
    backend.find_binary("claude")
}

fn claude_login(backend: &impl LocalBackend) -> Result<LocalLogin, QuotaStatus> {
    let binary = claude_binary(backend).ok_or(QuotaStatus::NoCredential)?;
    let output = backend
        .run_capture(
            &binary,
            &["auth", "status", "--json"],
            &[
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_AUTH_TOKEN",
                "ANTHROPIC_API_KEY",
                "CLAUDE_CODE_OAUTH_TOKEN",
            ],
            &[
                ("DISABLE_TELEMETRY", "1"),
                ("DISABLE_ERROR_REPORTING", "1"),
                ("DISABLE_AUTOUPDATER", "1"),
            ],
            &backend.temp_dir(),
            STATUS_TIMEOUT,
        )
        .ok_or(QuotaStatus::Unavailable)?;
    let status: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| QuotaStatus::Unavailable)?;
    // Signed out still prints JSON (exit 1); only a missing/null flag is an
    // unreadable answer, i.e. a temporary failure.
    let logged_in = status["loggedIn"]
        .as_bool()
        .ok_or(QuotaStatus::Unavailable)?;
    if !logged_in {
        return Err(QuotaStatus::NoCredential);
    }
    let email = status["email"].as_str().unwrap_or_default().trim();
    let plan = status["subscriptionType"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let account = if email.is_empty() {
        match plan.as_deref() {
            Some(kind) => {
                let mut name = String::from("Claude ");
                let mut chars = kind.chars();
                if let Some(first) = chars.next() {
                    name.extend(first.to_uppercase());
                    name.push_str(chars.as_str());
                }
                name
            }
            None => "Claude account".to_owned(),
        }
    } else {
        email.to_owned()
    };
    // The identity binds the cache to the exact credential file: a refresh
    // or a different sign-in invalidates, never reuses, the reading.
    let id = claude_identity_string(backend, &account);
    Ok(LocalLogin {
        id,
        account,
        service: "Claude Code".to_owned(),
        plan,
        secret: String::new(),
        account_id: String::new(),
    })
}

fn claude_usage(backend: &impl LocalBackend, login: &LocalLogin, now: i64) -> ProviderQuota {
    claude_usage_tui(backend, login, now)
        .unwrap_or_else(|| ProviderQuota::empty(QuotaStatus::Unavailable))
}

/// Stable run-owned PTY directory: one trust entry covers every run.
fn tui_workdir(backend: &impl LocalBackend) -> PathBuf {
    backend
        .temp_dir()
        .join(crate::claude_pty::TUI_WORK_DIR_NAME)
}

/// Magpie's default interactive `/usage` read:
/// the query typed into the real interactive CLI, its panel parsed by the
/// same reader. An unresolved panel remains unknown; sign-in is left to
/// Claude Code's user.
fn claude_usage_tui(
    backend: &impl LocalBackend,
    login: &LocalLogin,
    now: i64,
) -> Option<ProviderQuota> {
    let binary = claude_binary(backend)?;
    let workdir = tui_workdir(backend);
    let output = backend.run_tui(
        &binary,
        &[
            "--setting-sources",
            "",
            "--settings",
            "{\"disableDeepLinkRegistration\":\"disable\"}",
            "--strict-mcp-config",
            "--tools",
            "",
            "--permission-mode",
            "default",
            "--allowedTools",
            "",
            "--no-chrome",
        ],
        &[
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ],
        &[
            ("DISABLE_TELEMETRY", "1"),
            ("DISABLE_ERROR_REPORTING", "1"),
            ("DISABLE_AUTOUPDATER", "1"),
        ],
        &workdir,
        CLAUDE_TUI_TIMEOUT,
    )?;
    if output.saw_denied {
        let mut denied = ProviderQuota::empty(QuotaStatus::NeedsAuth);
        denied.service = Some(login.service.clone());
        denied.account = Some(login.account.clone());
        return Some(denied);
    }
    match parse_claude_usage(&crate::claude_pty::usage_text(&output.text), now) {
        Ok(windows) => {
            let mut reading = ProviderQuota::empty(QuotaStatus::Ready);
            reading.plan = login.plan.clone();
            reading.windows = windows;
            Some(reading)
        }
        Err(status) => Some(ProviderQuota::empty(status)),
    }
}

/// Whether the output names an account problem rather than a temporary
/// failure. Magpie's `claudeUsageDenied` pattern is used unchanged.
fn claude_denied(output: &[u8]) -> bool {
    static DENIED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(401|403)\b|not (logged|signed) in|signed out|sign-in (has )?expired|unauthorized|forbidden|authentication (failed|required)|invalid (access )?token|(session|usage) limit|hit your limit").unwrap()
    });
    DENIED.is_match(&String::from_utf8_lossy(output))
}

/// Parse `/usage` lines. Ports Magpie's parseClaudeUsage: account errors
/// take precedence; unknown panels are temporary failures, never zero.
fn parse_claude_usage(text: &str, now: i64) -> Result<Vec<QuotaWindow>, QuotaStatus> {
    static LINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^Current (session|week(?: \(([^)]+)\))?):\s*([0-9.]+)% used(?:\s*·\s*resets (.+))?$",
        )
        .unwrap()
    });
    static OVERAGES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)using your overages").unwrap());
    let stripped = strip_ansi(text);
    let lines: Vec<&str> = stripped.lines().map(str::trim).collect();
    if lines.iter().any(|line| claude_denied(line.as_bytes())) {
        return Err(QuotaStatus::NeedsAuth);
    }
    let mut out: Vec<QuotaWindow> = Vec::new();
    for line in &lines {
        let Some(parts) = LINE.captures(line) else {
            continue;
        };
        let Ok(used) = parts[3].parse::<f64>() else {
            continue;
        };
        let weekly = &parts[1] != "session";
        let scope = parts
            .get(2)
            .map(|scope| {
                let scope = scope.as_str().trim();
                let lower = scope.to_lowercase();
                if let Some(short) = lower.strip_suffix(" only").filter(|s| !s.is_empty()) {
                    scope[..short.len()].trim()
                } else {
                    scope
                }
            })
            .filter(|scope| !scope.is_empty() && !scope.eq_ignore_ascii_case("all models"));
        let (span_secs, kind) = if weekly {
            (7 * 86400, QuotaWindowKind::Weekly)
        } else {
            (5 * 3600, QuotaWindowKind::Rolling)
        };
        let resets = parts
            .get(4)
            .and_then(|when| claude_reset_time(when.as_str(), now, span_secs));
        let id = if let Some(model) = scope {
            format!("week:{}", model.to_lowercase())
        } else if weekly {
            "weekly".to_owned()
        } else {
            "session".to_owned()
        };
        if out.iter().any(|w: &QuotaWindow| w.id == id) {
            continue;
        }
        out.push(QuotaWindow {
            id,
            kind,
            model: scope.map(str::to_owned),
            span_seconds: Some(span_secs),
            used_percent: Some(used),
            remaining: None,
            limit: None,
            resets_at: resets,
            unlimited: false,
        });
    }
    if lines.iter().any(|line| OVERAGES.is_match(line)) {
        let resets_at = out
            .iter()
            .filter(|window| window.used_percent.is_some_and(|used| used >= 100.0))
            .filter_map(|window| window.resets_at)
            .max();
        out.push(QuotaWindow {
            id: "extra-usage".into(),
            kind: QuotaWindowKind::ExtraUsage,
            model: None,
            span_seconds: None,
            used_percent: Some(100.0),
            remaining: None,
            limit: None,
            resets_at,
            unlimited: false,
        });
    }
    if out.is_empty() {
        // Billed through the subscription but nothing metered to show, or a
        // newer CLI that only prints the session cost: unavailable, not zero.
        return Err(QuotaStatus::Unavailable);
    }
    Ok(out)
}

fn strip_ansi(text: &str) -> String {
    static ANSI: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[P^_][^\x1b]*\x1b\\|\x1b.").unwrap()
    });
    ANSI.replace_all(text, "").into_owned()
}

/// Read a `/usage` reset stamp ("Oct 1 at 3:30pm", "Oct 9, 2:59pm",
/// "3pm") in the named IANA zone, else local time, as in Magpie.
/// An unparseable stamp leaves the window without a
/// reset rather than inventing one.
fn claude_reset_time(text: &str, now: i64, span_secs: u64) -> Option<i64> {
    let mut text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut zone = None;
    if let Some(open) = text.rfind('(')
        && text.ends_with(')')
    {
        zone = text[open + 1..text.len() - 1].parse::<chrono_tz::Tz>().ok();
        text = text[..open].trim();
    }
    let text = text.replace("AM", "am").replace("PM", "pm");
    match zone {
        Some(zone) => claude_reset_in_zone(&text, now, span_secs, zone),
        None => claude_reset_in_zone(&text, now, span_secs, chrono::Local),
    }
}

/// Magpie's `claudeResetTime` and `nextReading`, using Chrono's explicit
/// local-time candidates for the repeated hour when daylight saving ends.
fn claude_reset_in_zone<Tz: chrono::TimeZone>(
    text: &str,
    now: i64,
    span_secs: u64,
    zone: Tz,
) -> Option<i64> {
    // Chrono needs minutes; Go's hour-only layouts supply zero implicitly.
    static HOUR_ONLY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(^|[ ,])([0-9]{1,2})(am|pm)$").unwrap());
    let text = HOUR_ONLY.replace(text, "${1}${2}:00${3}");
    let text = text.as_ref();
    let reference = zone.timestamp_opt(now, 0).single()?;
    let latest = now + span_secs as i64 + 2 * 3600;
    for (format, has_year) in [
        ("%b %e at %I:%M%P", false),
        ("%b %e, %Y at %I:%M%P", true),
        ("%b %e, %I:%M%P", false),
        ("%b %e, %Y, %I:%M%P", true),
    ] {
        // A leap reference year lets the no-year layouts retain February 29.
        let parsed = if has_year {
            chrono::NaiveDateTime::parse_from_str(text, format)
        } else {
            chrono::NaiveDateTime::parse_from_str(&format!("{text} 2000"), &format!("{format} %Y"))
        };
        let Ok(parsed) = parsed else { continue };
        if has_year {
            return zone
                .from_local_datetime(&parsed)
                .earliest()
                .map(|at| at.timestamp());
        }
        let mut past = None;
        for year in reference.year() - 1..=reference.year() + 1 {
            let date = chrono::NaiveDate::from_ymd_opt(year, parsed.month(), 1)?
                .checked_add_days(chrono::Days::new(parsed.day() as u64 - 1))?;
            let local = zone.from_local_datetime(&date.and_time(parsed.time()));
            if let Some(at) = next_reading(local.clone(), now, now - 86400)
                && at <= latest
            {
                return Some(at);
            }
            if let Some(at) = local.earliest().filter(|at| at.timestamp() < now) {
                past = Some(at.timestamp());
            }
        }
        return past;
    }
    let clock = chrono::NaiveTime::parse_from_str(text, "%I:%M%P").ok()?;
    for day in 0..=2 {
        let date = reference
            .date_naive()
            .checked_add_days(chrono::Days::new(day))?;
        if let Some(at) = next_reading(zone.from_local_datetime(&date.and_time(clock)), now, now) {
            if at <= latest {
                return Some(at);
            }
            break;
        }
    }
    for day in 0..=2 {
        let date = reference
            .date_naive()
            .checked_sub_days(chrono::Days::new(day))?;
        if let Some(at) = zone
            .from_local_datetime(&date.and_time(clock))
            .earliest()
            .filter(|at| at.timestamp() < now)
        {
            return Some(at.timestamp());
        }
    }
    None
}

fn next_reading<Tz: chrono::TimeZone>(
    local: chrono::LocalResult<chrono::DateTime<Tz>>,
    reference: i64,
    limit: i64,
) -> Option<i64> {
    let first = local.clone().earliest()?.timestamp();
    let last = local.latest()?.timestamp();
    for from in [reference, limit] {
        for at in [first, last] {
            if at >= from {
                return Some(at);
            }
        }
    }
    None
}

// ---- Codex -----------------------------------------------------------------

fn codex_home(backend: &impl LocalBackend) -> Option<PathBuf> {
    if let Some(home) = backend
        .env_var("CODEX_HOME")
        .filter(|s| !s.trim().is_empty())
    {
        return Some(PathBuf::from(home));
    }
    backend.home_dir().map(|home| home.join(".codex"))
}

/// Which credential store the Codex CLI itself would use. Read-only parse
/// of `config.toml`: "file" (or absent) keeps `auth.json`; "keyring",
/// "auto" without a file, and "ephemeral" keep no file this module may
/// read. Only the `cli_auth_credentials_store` scalar is inspected, never
/// secrets. Unknown text reads as the CLI default ("file"), so a garbled
/// config never masks a plain logged-out state.
fn codex_store_mode(backend: &impl LocalBackend, home: &Path) -> String {
    let raw = backend
        .read_file(&home.join("config.toml"))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((key, mut value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "cli_auth_credentials_store" {
            continue;
        }
        value = value.trim();
        value = value.trim_matches(|c| c == '"' || c == '\'');
        // Strip an inline comment after the scalar ("keyring # comment").
        value = value.split('#').next().unwrap_or(value).trim();
        value = value.trim_matches(|c| c == '"' || c == '\'');
        let mode = value.to_ascii_lowercase();
        return match mode.as_str() {
            "file" | "keyring" | "auto" | "ephemeral" => mode,
            _ => "file".to_owned(),
        };
    }
    "file".to_owned()
}

fn codex_login(backend: &impl LocalBackend, now: i64) -> Result<LocalLogin, QuotaStatus> {
    let home = codex_home(backend).ok_or(QuotaStatus::NoCredential)?;
    // "file" is the default store and "auto" falls back to it; a keyring or
    // ephemeral store keeps no file this module may read. Outcome-first: an
    // auth.json Codex itself would fall back to is used, otherwise there is
    // no readable login. A keyring/auto/ephemeral store without auth.json
    // is an unreadable encrypted login (Unavailable, keep last good), never
    // evidence of no sign-in.
    let raw = match backend.read_file(&home.join("auth.json")) {
        Some(raw) => raw,
        None => {
            return Err(match codex_store_mode(backend, &home).as_str() {
                "file" => QuotaStatus::NoCredential,
                _ => QuotaStatus::Unavailable,
            });
        }
    };
    let auth: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| QuotaStatus::NoCredential)?;
    if auth["auth_mode"].as_str() == Some("apikey") {
        return Err(QuotaStatus::Unsupported);
    }
    let tokens = &auth["tokens"];
    let access = tokens["access_token"].as_str().unwrap_or_default();
    if access.is_empty() {
        return Err(QuotaStatus::NoCredential);
    }
    if token_expired(access, now) {
        // Codex renews its own sign-in as it runs; never refreshed here.
        return Err(QuotaStatus::NeedsAuth);
    }
    let account_id = tokens["account_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| jwt_nested(access, "https://api.openai.com/auth", "chatgpt_account_id"))
        .unwrap_or_default();
    let email = tokens["id_token"]
        .as_str()
        .and_then(|token| jwt_top(token, "email"))
        .or_else(|| jwt_top(access, "email"))
        .filter(|email| !email.trim().is_empty());
    let plan = jwt_nested(access, "https://api.openai.com/auth", "chatgpt_plan_type");
    let account = email.unwrap_or_else(|| "ChatGPT".to_owned());
    let fingerprint: [u8; 32] = Sha256::digest(access.as_bytes()).into();
    Ok(LocalLogin {
        id: format!("codex:{account_id}:{}", hex32(&fingerprint)),
        account,
        service: "Codex".to_owned(),
        plan,
        secret: access.to_owned(),
        account_id,
    })
}

fn codex_usage(
    backend: &impl LocalBackend,
    login: &LocalLogin,
    now: i64,
) -> (ProviderQuota, Option<u64>) {
    let request = QuotaRequest {
        method: "GET",
        url: format!("{CODEX_BASE}/wham/usage"),
        headers: vec![
            format!("Authorization: Bearer {}", login.secret),
            format!("chatgpt-account-id: {}", login.account_id),
            "Accept: application/json".into(),
            "User-Agent: fintwind".into(),
        ],
        body: None,
    };
    // Header values ride a curl config line: reject control bytes rather
    // than interpreting them, exactly like the server path.
    if request
        .headers
        .iter()
        .any(|h| h.chars().any(|c| c.is_control() || matches!(c, '"' | '\\')))
    {
        return (ProviderQuota::empty(QuotaStatus::NeedsAuth), None);
    }
    let response = match backend.http(&request) {
        Ok(response) => response,
        Err(_) => return (ProviderQuota::empty(QuotaStatus::Unavailable), None),
    };
    if !(200..300).contains(&response.status) {
        let body = response.body.as_bytes();
        let status = match response.status {
            401 if codex_expired_words(body) || codex_revoked_words(body) => QuotaStatus::NeedsAuth,
            // Any other 401 is unknown (Magpie: never a re-login on its own).
            401 => QuotaStatus::Unavailable,
            403 => QuotaStatus::AccessDenied,
            429 => QuotaStatus::RateLimited,
            _ => QuotaStatus::Unavailable,
        };
        return (ProviderQuota::empty(status), response.retry_after);
    }
    let mut reading = match serde_json::from_str::<serde_json::Value>(&response.body) {
        Ok(body) => {
            let mut q = ProviderQuota::empty(QuotaStatus::Ready);
            parse_codex_wham(&mut q, &body, now);
            q
        }
        Err(_) => ProviderQuota::empty(QuotaStatus::Unavailable),
    };
    if reading.status == QuotaStatus::Ready {
        reading.plan = reading.plan.or_else(|| login.plan.clone());
    }
    (reading, response.retry_after)
}

/// Whether the access token's `exp` already passed. A token without an
/// expiry reads as usable; only Codex itself renews it.
fn token_expired(access: &str, now: i64) -> bool {
    let Some(exp) = jwt_payload(access).and_then(|claims| claims["exp"].as_i64()) else {
        return false;
    };
    exp != 0 && now >= exp
}

/// Decode a JWT payload without verifying: the token proves itself to the
/// vendor; here it only sources labels and expiry.
fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let (_header, payload, _sig) = (parts.next()?, parts.next()?, parts.next()?);
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(payload))
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn jwt_top(token: &str, key: &str) -> Option<String> {
    jwt_payload(token)?.get(key)?.as_str().map(str::to_owned)
}

fn jwt_nested(token: &str, key: &str, nested: &str) -> Option<String> {
    jwt_payload(token)?
        .get(key)?
        .get(nested)?
        .as_str()
        .map(str::to_owned)
}

/// Magpie's expiredWords: a 401 saying the access token expired.
fn codex_expired_words(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body).to_lowercase();
    text.contains("token_expired")
        || text.contains("authentication token is expired")
        || text.contains("authentication token has expired")
        || text.contains("please try refreshing it")
}

/// The sign-in itself is gone (Magpie's revokedWords: token_revoked).
fn codex_revoked_words(body: &[u8]) -> bool {
    String::from_utf8_lossy(body)
        .to_lowercase()
        .contains("token_revoked")
}

fn hex32(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str()?.parse::<f64>().ok())
        .filter(|n| n.is_finite())
}

fn percent(v: &Value) -> Option<f64> {
    number(v).filter(|n| *n >= 0.0).map(|n| n.min(100.0))
}

fn money(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let digits = s.strip_prefix('-').unwrap_or(&s);
    if digits.is_empty()
        || digits.matches('.').count() > 1
        || !digits.bytes().all(|c| c.is_ascii_digit() || c == b'.')
        || !digits.bytes().any(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(s)
}

fn timestamp(v: &Value) -> Option<i64> {
    if let Some(s) = v.as_str()
        && let Ok(t) = chrono::DateTime::parse_from_rfc3339(s)
    {
        return Some(t.timestamp());
    }
    let n = v.as_i64().or_else(|| v.as_str()?.parse::<i64>().ok())?;
    (n > 0).then_some(if n >= 1_000_000_000_000 { n / 1000 } else { n })
}

fn window(
    id: &str,
    kind: QuotaWindowKind,
    used: Option<f64>,
    reset: Option<i64>,
    span: Option<u64>,
) -> QuotaWindow {
    QuotaWindow {
        id: id.into(),
        kind,
        model: None,
        span_seconds: span,
        used_percent: used,
        remaining: None,
        limit: None,
        resets_at: reset,
        unlimited: false,
    }
}

/// Shared ChatGPT backend wham/usage shape: plan, primary/secondary windows
/// and credits. Adapted from Magpie's codexWindows parsing (MIT, see
/// docs/provider-quota-upstream-license.txt).
fn parse_codex_wham(q: &mut ProviderQuota, body: &Value, now: i64) {
    q.plan = body["plan_type"].as_str().map(str::to_owned);
    for id in ["primary_window", "secondary_window"] {
        let v = &body["rate_limit"][id];
        if let Some(used) = percent(&v["used_percent"]) {
            let span = v["limit_window_seconds"].as_u64();
            let kind = if span == Some(604800) {
                QuotaWindowKind::Weekly
            } else {
                QuotaWindowKind::Rolling
            };
            q.windows.push(window(
                id,
                kind,
                Some(used),
                timestamp(&v["reset_at"]).or_else(|| {
                    v["reset_after_seconds"]
                        .as_i64()
                        .filter(|s| *s > 0)
                        .map(|s| now.saturating_add(s))
                }),
                span,
            ));
        }
    }
    let c = &body["credits"];
    if c["unlimited"].as_bool() == Some(true) {
        q.balances.push(QuotaBalance {
            unit: "credits".into(),
            remaining: String::new(),
            unlimited: true,
        });
    } else if c["has_credits"].as_bool() == Some(true)
        && let Some(amount) = money(&c["balance"])
    {
        q.balances.push(QuotaBalance {
            unit: "credits".into(),
            remaining: amount,
            unlimited: false,
        });
    }
}

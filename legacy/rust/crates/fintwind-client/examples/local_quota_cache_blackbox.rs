//! Desktop local-quota cache black box: no user files, no real CLIs, no
//! network. The harness drives `LocalQuotaCache` with fixtures and asserts
//! caching, single-flight de-duplication, 429 backoff and account-change
//! clearing from the outside. Not unit tests: every vector runs the real
//! fetch path end to end.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::Result;
use fintwind_client::claude_pty::TuiTranscript;
use fintwind_client::local_quota::{
    CommandOutput, LocalBackend, LocalQuotaCache, QuotaRequest, QuotaResponse,
};
use fintwind_protocol::provider_quota::QuotaStatus;
use serde_json::{Value, json};

struct Fixture {
    files: Mutex<HashMap<PathBuf, Vec<u8>>>,
    binaries: HashMap<String, PathBuf>,
    runs: Mutex<VecDeque<CommandOutput>>,
    tui: Mutex<VecDeque<TuiTranscript>>,
    tui_ran: Mutex<Vec<(String, PathBuf)>>,
    ran: Mutex<usize>,
    https: Mutex<VecDeque<Result<QuotaResponse, String>>>,
    requested: Mutex<Vec<(String, String)>>,
    home: PathBuf,
    now: AtomicI64,
}

impl Fixture {
    fn new(
        files: Vec<(PathBuf, Vec<u8>)>,
        binaries: Vec<(&str, &str)>,
        https: Vec<Result<QuotaResponse, String>>,
    ) -> Self {
        Self {
            files: Mutex::new(files.into_iter().collect()),
            binaries: binaries
                .into_iter()
                .map(|(k, v)| (k.to_owned(), PathBuf::from(v)))
                .collect(),
            runs: Mutex::new(VecDeque::new()),
            tui: Mutex::new(VecDeque::new()),
            tui_ran: Mutex::new(Vec::new()),
            ran: Mutex::new(0),
            https: Mutex::new(https.into_iter().collect()),
            requested: Mutex::new(Vec::new()),
            home: PathBuf::from("/fixture/home"),
            now: AtomicI64::new(1900000000),
        }
    }

    fn codex_access(exp: i64) -> String {
        let payload = json!({"exp":exp,"email":"fixture@example.invalid",
            "https://api.openai.com/auth":{"chatgpt_account_id":"fixture-acct","chatgpt_plan_type":"plus"}});
        format!(
            "synthetic.{}.synthetic",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    }

    fn codex_auth(home: &Path, access: &str) -> (PathBuf, Vec<u8>) {
        (
            home.join(".codex").join("auth.json"),
            json!({"auth_mode":"chatgpt","tokens":{
                "access_token":access,"id_token":access,
                "refresh_token":"synthetic-refresh-do-not-leak","account_id":"fixture-acct"}})
            .to_string()
            .into_bytes(),
        )
    }

    fn wham(status: u16, body: Value) -> Result<QuotaResponse, String> {
        Ok(QuotaResponse {
            status,
            body: body.to_string(),
            retry_after: None,
        })
    }
}

impl LocalBackend for Fixture {
    fn read_file(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(path).cloned()
    }
    fn home_dir(&self) -> Option<PathBuf> {
        Some(self.home.clone())
    }
    fn env_var(&self, _: &str) -> Option<String> {
        None
    }
    fn find_binary(&self, name: &str) -> Option<PathBuf> {
        self.binaries.get(name).cloned()
    }
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
    fn run_capture(
        &self,
        _program: &Path,
        args: &[&str],
        _: &[&str],
        _: &[(&str, &str)],
        _: &Path,
        _: Duration,
    ) -> Option<CommandOutput> {
        *self.ran.lock().unwrap() += 1;
        let _ = args;
        self.runs.lock().unwrap().pop_front()
    }
    fn run_tui(
        &self,
        program: &Path,
        args: &[&str],
        _: &[&str],
        _: &[(&str, &str)],
        workdir: &Path,
        _: Duration,
    ) -> Option<TuiTranscript> {
        let _ = args;
        self.tui_ran
            .lock()
            .unwrap()
            .push((program.to_string_lossy().into_owned(), workdir.to_owned()));
        self.tui.lock().unwrap().pop_front()
    }
    fn http(&self, request: &QuotaRequest) -> Result<QuotaResponse> {
        self.requested
            .lock()
            .unwrap()
            .push((request.method.into(), request.url.clone()));
        assert!(!request.url.contains("responses"));
        self.https
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("unexpected local vendor call"))?
            .map_err(|e| anyhow::anyhow!(e))
    }
    fn temp_dir(&self) -> PathBuf {
        PathBuf::from("/fixture/tmp")
    }
}

use base64::Engine as _;

fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .expect("provide a new artifact.json path");
    let mut passed: Vec<String> = Vec::new();
    let home = PathBuf::from("/fixture/home");
    let ready_body = || {
        json!({"plan_type":"plus","rate_limit":{
            "primary_window":{"used_percent":30,"limit_window_seconds":18000,"reset_at":1900000001},
            "secondary_window":{"limit_window_seconds":604800,"reset_at":1900100000}},
            "credits":{"has_credits":true,"unlimited":false,"balance":"12.50"}})
    };

    // Repeated opens share one CLI run and one vendor request per cooldown.
    let backend = Fixture::new(
        vec![Fixture::codex_auth(
            &home,
            &Fixture::codex_access(4102444800),
        )],
        vec![],
        vec![
            Fixture::wham(200, ready_body()),
            Fixture::wham(200, ready_body()),
        ],
    );
    let cache = LocalQuotaCache::default();
    let first = cache.fetch_with(&backend, "openai", false);
    assert_eq!(first.status, QuotaStatus::Ready);
    let second = cache.fetch_with(&backend, "openai", false);
    assert_eq!(second.measured_at, first.measured_at);
    assert_eq!(
        backend.requested.lock().unwrap().len(),
        1,
        "a second open inside the cooldown issues no vendor request"
    );
    backend.now.fetch_add(61, Ordering::SeqCst);
    let third = cache.fetch_with(&backend, "openai", false);
    assert!(third.measured_at.unwrap() > first.measured_at.unwrap());
    passed.push("repeated opens share one vendor request per cooldown".into());

    // Concurrent same-source reads run one vendor request between them.
    let backend = Fixture::new(
        vec![Fixture::codex_auth(
            &home,
            &Fixture::codex_access(4102444800),
        )],
        vec![],
        vec![Fixture::wham(200, ready_body())],
    );
    let cache = LocalQuotaCache::default();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| cache.fetch_with(&backend, "openai", false)))
            .collect();
        let mut stamps = Vec::new();
        for handle in handles {
            let reading = handle.join().unwrap();
            assert_eq!(reading.status, QuotaStatus::Ready);
            stamps.push(reading.measured_at);
        }
        assert!(
            stamps.windows(2).all(|w| w[0] == w[1]),
            "concurrent reads converge on one reading"
        );
    });
    assert_eq!(
        backend.requested.lock().unwrap().len(),
        1,
        "eight concurrent reads issue one vendor request"
    );
    passed.push("concurrent same-source reads de-duplicate to one request".into());

    // A forced refresh cannot cross a 429 floor.
    let backend = Fixture::new(
        vec![Fixture::codex_auth(
            &home,
            &Fixture::codex_access(4102444800),
        )],
        vec![],
        vec![
            Fixture::wham(200, ready_body()),
            Ok(QuotaResponse {
                status: 429,
                body: json!({}).to_string(),
                retry_after: Some(600),
            }),
        ],
    );
    let cache = LocalQuotaCache::default();
    let _ = cache.fetch_with(&backend, "openai", false);
    backend.now.fetch_add(61, Ordering::SeqCst);
    let limited = cache.fetch_with(&backend, "openai", false);
    assert_eq!(limited.status, QuotaStatus::RateLimited);
    assert_eq!(limited.windows.len(), 1, "the dated reading is retained");
    let calls = backend.requested.lock().unwrap().len();
    let forced = cache.fetch_with(&backend, "openai", true);
    assert_eq!(forced.status, QuotaStatus::RateLimited);
    assert_eq!(
        backend.requested.lock().unwrap().len(),
        calls,
        "force during the 429 floor issues no vendor request"
    );
    passed.push("forced refresh never crosses a 429 floor".into());

    // An account change or logout clears the old reading.
    let backend = Fixture::new(
        vec![Fixture::codex_auth(
            &home,
            &Fixture::codex_access(4102444800),
        )],
        vec![],
        vec![
            Fixture::wham(200, ready_body()),
            Fixture::wham(200, ready_body()),
        ],
    );
    let cache = LocalQuotaCache::default();
    let before = cache.fetch_with(&backend, "openai", false);
    *backend.files.lock().unwrap() = vec![Fixture::codex_auth(
        &home,
        &Fixture::codex_access(4102444900),
    )]
    .into_iter()
    .collect();
    backend.now.fetch_add(61, Ordering::SeqCst);
    let after = cache.fetch_with(&backend, "openai", false);
    assert_ne!(
        before.measured_at, after.measured_at,
        "a changed account refetches instead of reusing"
    );
    backend.files.lock().unwrap().clear();
    let logged_out = cache.fetch_with(&backend, "openai", true);
    assert_eq!(logged_out.status, QuotaStatus::NoCredential);
    assert!(logged_out.windows.is_empty());
    passed.push("account change refetches; logout clears the reading".into());

    // The Claude fast path runs no CLI while files and cooldown are fresh.
    let status =
        json!({"loggedIn":true,"email":"fixture@example.invalid","subscriptionType":"pro"});
    let mut backend = Fixture::new(vec![], vec![("claude", "/fixture/claude")], vec![]);
    backend.runs = Mutex::new(
        vec![CommandOutput {
            success: true,
            stdout: status.to_string().into_bytes(),
            stderr: Vec::new(),
        }]
        .into(),
    );
    backend.tui = Mutex::new(
        vec![TuiTranscript {
            text: "Current session: 13% used · Resets Oct 1 at 3:30pm (UTC)".into(),
            saw_denied: false,
        }]
        .into(),
    );
    let cache = LocalQuotaCache::default();
    let first = cache.fetch_with(&backend, "anthropic", false);
    assert_eq!(first.status, QuotaStatus::Ready);
    assert_eq!(*backend.ran.lock().unwrap(), 1);
    let second = cache.fetch_with(&backend, "anthropic", false);
    assert_eq!(second.measured_at, first.measured_at);
    assert_eq!(
        *backend.ran.lock().unwrap(),
        1,
        "a second open inside the cooldown spawns no CLI"
    );
    passed.push("claude fast path spawns no CLI while files are unchanged".into());

    // The exclusive PTY stage through the desktop cache: typed panel from
    // its own console, then cached without a second spawn.
    let status =
        json!({"loggedIn":true,"email":"fixture@example.invalid","subscriptionType":"pro"});
    let mut backend = Fixture::new(vec![], vec![("claude", "/fixture/claude")], vec![]);
    backend.runs = Mutex::new(
        vec![CommandOutput {
            success: true,
            stdout: status.to_string().into_bytes(),
            stderr: Vec::new(),
        }]
        .into(),
    );
    backend.tui = Mutex::new(
        vec![TuiTranscript {
            text: "Current session: 13% used · Resets Oct 1 at 3:30pm (UTC)\nCurrent week (all models): 4% used · Resets Oct 3 at 2pm (UTC)".to_owned(),
            saw_denied: false,
        }]
        .into(),
    );
    let cache = LocalQuotaCache::default();
    let first = cache.fetch_with(&backend, "anthropic", false);
    assert_eq!(first.status, QuotaStatus::Ready);
    assert_eq!(first.windows.len(), 2);
    let tui_ran = backend.tui_ran.lock().unwrap();
    assert_eq!(tui_ran.len(), 1, "the panel came from the PTY, not -p");
    assert_eq!(
        tui_ran[0].1,
        PathBuf::from("/fixture/tmp/fintwind-claude-work"),
        "the PTY runs in the stable directory"
    );
    drop(tui_ran);
    let runs_after_first = *backend.ran.lock().unwrap();
    let second = cache.fetch_with(&backend, "anthropic", false);
    assert_eq!(second.measured_at, first.measured_at);
    assert_eq!(
        backend.tui_ran.lock().unwrap().len(),
        1,
        "a second open repeats no PTY session"
    );
    assert_eq!(
        *backend.ran.lock().unwrap(),
        runs_after_first,
        "a second open spawns no CLI either"
    );
    passed.push("cached PTY panel reuses one exclusive session".into());

    // Unsupported IDs never touch a backend.
    let backend = Fixture::new(vec![], vec![], vec![]);
    let cache = LocalQuotaCache::default();
    let q = cache.fetch_with(&backend, "deepseek", false);
    assert_eq!(q.status, QuotaStatus::Unsupported);
    assert_eq!(*backend.ran.lock().unwrap(), 0);
    assert!(backend.requested.lock().unwrap().is_empty());
    passed.push("non-local IDs stay unsupported without backend contact".into());

    let artifact = json!({
        "suite": "local-quota-cache-blackbox",
        "passed": passed.len(),
        "checks": passed,
    });
    let text = serde_json::to_string_pretty(&artifact)? + "\n";
    assert!(!text.contains("synthetic-"), "secret boundary holds");
    std::fs::write(&path, text)?;
    println!(
        "{}",
        serde_json::json!({"suite":"local-quota-cache-blackbox","passed":passed.len(),"checks":passed})
    );
    Ok(())
}

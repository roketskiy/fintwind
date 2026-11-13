//! Module black-box checks; failure cases are specified in docs/provider-quota.md.
//! No user files, real credentials, OpenCode services, or external network calls.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::Result;
use fintwind_core::provider_quota::{QuotaRequest, QuotaResponse, QuotaService, QuotaSource};
use fintwind_protocol::provider_quota::QuotaStatus;
use serde_json::{Value, json};

struct Source {
    credentials: Mutex<Value>,
    replies: Mutex<VecDeque<QuotaResponse>>,
    requests: Mutex<Vec<(String, String)>>,
    now: AtomicI64,
}

impl Source {
    fn new(provider: &str, value: Value, body: Value) -> Self {
        Self {
            credentials: Mutex::new(json!({"data":[{
                "id":"fixture-account-a", "integrationID":provider, "active":true,
                "label":"fixture", "value":value
            }]})),
            replies: Mutex::new(VecDeque::from([QuotaResponse {
                status: 200,
                body: body.to_string(),
                retry_after: None,
            }])),
            requests: Mutex::new(Vec::new()),
            now: AtomicI64::new(1900000000),
        }
    }
}

impl QuotaSource for Source {
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
    fn credentials(&self) -> Result<Value> {
        Ok(self.credentials.lock().unwrap().clone())
    }
    fn request(&self, request: &QuotaRequest) -> Result<QuotaResponse> {
        assert!(
            !request.url.contains("responses"),
            "query must not generate text"
        );
        self.requests
            .lock()
            .unwrap()
            .push((request.method.into(), request.url.clone()));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("unexpected vendor call"))
    }
}

fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .expect("provide a new artifact.json path");
    let mut passed = Vec::new();
    let key = || json!({"type":"key","key":"synthetic-secret-do-not-leak"});
    let oauth = |method: &str, metadata: Value| {
        json!({
            "type":"oauth", "methodID":method, "access":"synthetic-secret-do-not-leak",
            "refresh":"synthetic-refresh-do-not-leak", "expires":4102444800000_i64,
            "metadata":metadata
        })
    };
    let cases = vec![
        (
            "github-copilot",
            oauth("device", json!({})),
            json!({
                "copilot_plan":"individual", "quota_reset_date":"2033-01-01",
                "quota_snapshots":{"premium_interactions":{"entitlement":"300","quota_remaining":"180"},
                    "chat":{"unlimited":true}}
            }),
            2,
            0,
            "https://api.github.com/copilot_internal/user",
        ),
        (
            "google",
            oauth("gemini-cli", json!({"projectId":"fixture-project"})),
            json!({
                "buckets":[{"modelId":"gemini-pro","remainingFraction":0.6,"resetTime":"2033-01-01T00:00:00Z"}]
            }),
            1,
            0,
            "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota",
        ),
        (
            "zhipuai-coding-plan",
            key(),
            json!({"success":true,"data":{"level":"pro","limits":[
                {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":12,"nextResetTime":2000000000000_i64},
                {"type":"TIME_LIMIT","unit":5,"number":1,"percentage":3}]
            }}),
            2,
            0,
            "https://open.bigmodel.cn/api/monitor/usage/quota/limit",
        ),
        (
            "kimi-code-plan-cn",
            key(),
            json!({"usage":{"limit":"100","used":"25","resetTime":"2033-01-01T00:00:00Z"},
                "limits":[{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},"detail":{"limit":"100","remaining":"90"}}]
            }),
            2,
            0,
            "https://api.kimi.com/coding/v1/usages",
        ),
        (
            "minimax-coding-plan",
            key(),
            json!({"base_resp":{"status_code":0},"model_remains":[{
                "model_name":"general","current_interval_remaining_percent":80,"current_interval_status":1,
                "start_time":2000000000000_i64,"end_time":2000018000000_i64,
                "current_weekly_remaining_percent":90,"current_weekly_status":2,"weekly_end_time":2000100000
            }]}),
            2,
            0,
            "https://api.minimax.io/v1/token_plan/remains",
        ),
        (
            "deepseek",
            key(),
            json!({"is_available":false,"balance_infos":[
                {"currency":"CNY","total_balance":"0.00"}, {"currency":"USD","total_balance":"1.23456789"}]
            }),
            0,
            2,
            "https://api.deepseek.com/user/balance",
        ),
        (
            "stepfun",
            key(),
            json!({"object":"account","type":"prepaid","balance":0,"total_voucher_balance":26}),
            0,
            1,
            "https://api.stepfun.com/v1/accounts",
        ),
        (
            "commandcode",
            key(),
            json!({"credits":{"planId":"individual-go-monthly","monthlyCredits":10,
                    "purchasedCredits":2.5,"freeCredits":0},
                "windowLimits":{"fiveHour":{"used":3.1,"cap":10,"resetAt":2000000000000_i64},
                    "weekly":{"used":12,"cap":40,"resetAt":2000600000000_i64}}}),
            2,
            1,
            "https://api.commandcode.ai/alpha/billing/credits",
        ),
        (
            "ollama-cloud",
            key(),
            json!({"included":{"balance_usd":72.5,"allowance_usd":100,
                    "period":{"from":"2033-01-01T00:00:00Z","until":"2033-01-31T00:00:00Z"},
                    "weekly":{"remaining_percent":40,"resets_at":"2033-01-05T00:00:00Z"}},
                "purchased":{"balance_usd":25}}),
            2,
            1,
            "https://ollama.com/api/balance",
        ),
        (
            "opencode-go",
            key(),
            json!({"usage":{
                "rolling":{"percent":37,"resetsAt":"2033-01-01T14:12:03Z"},
                "weekly":{"percent":12,"resetsAt":"2033-01-08T00:00:00Z"},
                "monthly":{"percent":0,"resetsAt":"2033-02-01T00:00:00Z"}}}),
            2,
            0,
            "https://opencode.ai/zen/go/v1/usage",
        ),
        (
            "openrouter",
            key(),
            json!({"data":{"total_credits":20,"total_usage":3.5}}),
            0,
            1,
            "https://openrouter.ai/api/v1/credits",
        ),
        (
            "siliconflow",
            key(),
            json!({"data":{"totalBalance":"88.88"}}),
            0,
            1,
            "https://api.siliconflow.com/v1/user/info",
        ),
        (
            "siliconflow-cn",
            key(),
            json!({"data":{"totalBalance":"18.08"}}),
            0,
            1,
            "https://api.siliconflow.cn/v1/user/info",
        ),
        (
            "moonshotai",
            key(),
            json!({"data":{"available_balance":49.58}}),
            0,
            1,
            "https://api.moonshot.ai/v1/users/me/balance",
        ),
        (
            "moonshotai-cn",
            key(),
            json!({"data":{"available_balance":"10.00"}}),
            0,
            1,
            "https://api.moonshot.cn/v1/users/me/balance",
        ),
        (
            "zhipuai",
            key(),
            json!({"code":200,"success":true,"data":{"availableBalance":"0.00",
                "balance":0,"rechargeAmount":0,"giveAmount":0}}),
            0,
            1,
            "https://open.bigmodel.cn/api/biz/account/query-customer-account-report",
        ),
        (
            "zai",
            key(),
            json!({"code":200,"success":true,"data":{"availableBalance":5.5}}),
            0,
            1,
            "https://api.z.ai/api/biz/account/query-customer-account-report",
        ),
        (
            "minimax",
            key(),
            json!({"base_resp":{"status_code":0},"model_remains":[{
                "model_name":"general","current_interval_remaining_percent":80,"current_interval_status":1,
                "start_time":2000000000000_i64,"end_time":2000018000000_i64,
                "current_weekly_remaining_percent":90,"current_weekly_status":2,"weekly_end_time":2000100000
            }]}),
            2,
            0,
            "https://api.minimax.io/v1/token_plan/remains",
        ),
        (
            "minimax-cn",
            key(),
            json!({"base_resp":{"status_code":0},"model_remains":[{
                "model_name":"general","current_interval_remaining_percent":80,"current_interval_status":1,
                "start_time":2000000000000_i64,"end_time":2000018000000_i64
            }]}),
            1,
            0,
            "https://api.minimax.cn/v1/token_plan/remains",
        ),
        (
            "stepfun-ai-step-plan",
            key(),
            json!({"object":"account","type":"prepaid","balance":26}),
            0,
            1,
            "https://api.stepfun.ai/v1/accounts",
        ),
    ];
    for (provider, auth, body, windows, balances, url) in cases {
        let service = QuotaService::default();
        let source = Source::new(provider, auth, body);
        let q = service.fetch(&source, "fixture-scope", provider, false);
        assert_eq!(q.status, QuotaStatus::Ready, "{provider}");
        assert_eq!(q.windows.len(), windows, "{provider}");
        assert_eq!(q.balances.len(), balances, "{provider}");
        assert_eq!(source.requests.lock().unwrap()[0].1, url);
        if provider == "google" {
            assert_eq!(source.requests.lock().unwrap()[0].0, "POST");
        }
        if provider == "deepseek" {
            assert_eq!(q.balances[1].remaining, "1.23456789");
        }
        if provider == "minimax-coding-plan" || provider == "minimax" {
            assert_eq!(q.windows[1].used_percent, Some(100.0));
        }
        if provider == "commandcode" {
            assert_eq!(q.plan.as_deref(), Some("Go"));
            assert_eq!(q.balances[0].remaining, "12.50");
            assert_eq!(q.balances[0].unit, "USD");
            assert_eq!(q.windows[0].used_percent, Some(31.0));
        }
        if provider == "ollama-cloud" {
            assert_eq!(q.balances[0].unit, "USD");
            assert_eq!(q.balances[0].remaining, "25.00");
        }
        if provider == "openrouter" {
            assert_eq!(q.balances[0].remaining, "16.50");
            assert_eq!(q.balances[0].unit, "USD");
        }
        // A stored OAuth connection works for the ported adapters too: the
        // access token rides Bearer, with the same expiry rule as Google.
        if provider == "commandcode" {
            let oauth_source = Source::new(
                "commandcode",
                oauth("device", json!({})),
                json!({"credits":{"planId":"individual-pro-monthly","monthlyCredits":30,
                        "purchasedCredits":0,"freeCredits":0},
                    "windowLimits":{"fiveHour":{"used":1,"cap":10,"resetAt":2000000000000_i64},
                        "weekly":{"used":2,"cap":40,"resetAt":2000600000000_i64}}}),
            );
            let oauth_quota =
                QuotaService::default().fetch(&oauth_source, "scope", "commandcode", false);
            assert_eq!(oauth_quota.status, QuotaStatus::Ready);
            assert_eq!(oauth_quota.plan.as_deref(), Some("Pro"));
            assert_eq!(
                oauth_source.requests.lock().unwrap()[0].1,
                "https://api.commandcode.ai/alpha/billing/credits"
            );
        }
        if provider == "opencode-go" {
            assert_eq!(q.windows.len(), 2, "a 0% window names no reset");
        }
        if provider == "zhipuai" {
            assert_eq!(q.balances[0].unit, "CNY");
            assert_eq!(q.balances[0].remaining, "0.00");
        }
        let wire = serde_json::to_string(&q)?;
        assert!(!wire.contains("synthetic-"));
        let cached = service.fetch(&source, "fixture-scope", provider, false);
        assert_eq!(cached.measured_at, q.measured_at);
        let _ = service.fetch(&source, "fixture-scope", provider, true);
        assert_eq!(
            source.requests.lock().unwrap().len(),
            1,
            "manual refresh floor"
        );
        passed.push(format!(
            "{provider}: adapter, cache, refresh floor, secret boundary"
        ));
    }
    for (provider, auth) in [
        ("openai", key()),
        (
            "openai",
            oauth("chatgpt-token-sharing", json!({"accountID":"fixture"})),
        ),
        ("anthropic", key()),
        ("anthropic", oauth("oauth", json!({}))),
        ("google", oauth("gemini-cli", json!({}))),
        (
            "github-copilot",
            oauth("device", json!({"enterpriseUrl":"example.org"})),
        ),
        ("opencode", key()),
        ("commandcode", json!({"type":"external","methodID":"file"})),
        (
            "stepfun-step-plan",
            json!({"type":"external","methodID":"file"}),
        ),
        ("deepseek", json!({"type":"external","methodID":"file"})),
        // Tencent Token Plan and Volcengine Ark plans: their account usage
        // APIs need an AccessKey pair OpenCode connections cannot express
        // (single models.dev env key, single-key /connect, sanitized
        // key_accounts), so no account call can be signed: Unsupported.
        ("tencent-coding-plan", key()),
        ("tencent-token-plan", key()),
        ("tencent-tokenhub", key()),
        ("volcengine", key()),
        ("volcengine-coding-plan", key()),
    ] {
        let source = Source::new(provider, auth, json!({}));
        let q = QuotaService::default().fetch(&source, "scope", provider, false);
        assert_eq!(q.status, QuotaStatus::Unsupported, "{provider}");
        assert!(source.requests.lock().unwrap().is_empty());
        passed.push(format!(
            "{provider}: incompatible credential does not call vendor"
        ));
    }
    let source = Source::new(
        "google",
        json!({"type":"oauth","methodID":"gemini-cli",
        "access":"synthetic-expired","expires":1,"metadata":{"projectId":"fixture"}}),
        json!({}),
    );
    assert_eq!(
        QuotaService::default()
            .fetch(&source, "scope", "google", false)
            .status,
        QuotaStatus::NeedsAuth
    );
    assert!(source.requests.lock().unwrap().is_empty());
    passed.push("expired OAuth never refreshed or sent".into());

    for (status, expected) in [
        (401, QuotaStatus::NeedsAuth),
        (403, QuotaStatus::AccessDenied),
        (429, QuotaStatus::RateLimited),
        (503, QuotaStatus::Unavailable),
    ] {
        let source = Source::new("deepseek", key(), json!({}));
        source.replies.lock().unwrap()[0] = QuotaResponse {
            status,
            body: "synthetic-secret-do-not-leak".into(),
            retry_after: Some(600),
        };
        let service = QuotaService::default();
        let q = service.fetch(&source, "scope", "deepseek", false);
        assert_eq!(q.status, expected);
        assert!(q.balances.is_empty());
        assert!(!serde_json::to_string(&q)?.contains("synthetic-"));
        let _ = service.fetch(&source, "scope", "deepseek", true);
        assert_eq!(source.requests.lock().unwrap().len(), 1);
        passed.push(format!("HTTP {status}: safe failure and cooldown"));
    }
    for body in [
        json!({}),
        json!({"balance_infos":[{"currency":"CNY"}]}),
        json!({"balance_infos":[{"currency":"CNY","total_balance":"NaN"}]}),
    ] {
        let source = Source::new("deepseek", key(), body);
        let q = QuotaService::default().fetch(&source, "scope", "deepseek", false);
        assert_eq!(q.status, QuotaStatus::NoData);
        assert!(q.balances.is_empty());
    }
    passed.push("missing or invalid balance is never zero".into());
    let source = Source::new(
        "deepseek",
        key(),
        json!({"balance_infos":[{"currency":"CNY","total_balance":"8.00"}]}),
    );
    let service = QuotaService::default();
    let _ = service.fetch(&source, "scope", "deepseek", false);
    source.credentials.lock().unwrap()["data"][0]["id"] = json!("fixture-account-b");
    source.replies.lock().unwrap().push_back(QuotaResponse {
        status: 503,
        body: String::new(),
        retry_after: None,
    });
    let q = service.fetch(&source, "scope", "deepseek", false);
    assert_eq!(q.status, QuotaStatus::Unavailable);
    assert!(
        q.balances.is_empty(),
        "new account must not see old balance"
    );
    *source.credentials.lock().unwrap() = json!({"data":[]});
    let q = service.fetch(&source, "scope", "deepseek", false);
    assert_eq!(q.status, QuotaStatus::NoCredential);
    assert!(q.balances.is_empty());
    passed.push("account replacement and logout clear previous snapshot".into());

    let source = Source::new(
        "deepseek",
        key(),
        json!({"balance_infos":[{"currency":"CNY","total_balance":"8.00"}]}),
    );
    let service = QuotaService::default();
    let original = service.fetch(&source, "scope", "deepseek", false);
    source.now.fetch_add(61, Ordering::SeqCst);
    source.replies.lock().unwrap().push_back(QuotaResponse {
        status: 429,
        body: String::new(),
        retry_after: Some(600),
    });
    let stale = service.fetch(&source, "scope", "deepseek", false);
    assert_eq!(stale.status, QuotaStatus::RateLimited);
    assert_eq!(stale.measured_at, original.measured_at);
    assert_eq!(stale.balances[0].remaining, "8.00");
    source.now.fetch_add(120, Ordering::SeqCst);
    let _ = service.fetch(&source, "scope", "deepseek", true);
    assert_eq!(
        source.requests.lock().unwrap().len(),
        2,
        "Retry-After applies to manual refresh"
    );
    source.now.fetch_add(481, Ordering::SeqCst);
    source.replies.lock().unwrap().push_back(QuotaResponse {
        status: 401,
        body: String::new(),
        retry_after: None,
    });
    let rejected = service.fetch(&source, "scope", "deepseek", true);
    assert_eq!(rejected.status, QuotaStatus::NeedsAuth);
    assert!(rejected.balances.is_empty());
    passed.push("same-account temporary failure keeps dated snapshot; 401 clears it".into());
    passed.push("Retry-After blocks manual refresh until cooldown ends".into());

    // The ChatGPT wham shape, answered from the machine's own Codex sign-in.
    use base64::Engine as _;
    use fintwind_client::claude_pty::TuiTranscript;
    use fintwind_core::local_quota::{CommandOutput, LocalBackend};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    struct FixtureLocal {
        files: Mutex<HashMap<PathBuf, Vec<u8>>>,
        reads: Mutex<usize>,
        flip_after: Option<usize>,
        flipped: Mutex<HashMap<PathBuf, Vec<u8>>>,
        binaries: HashMap<String, PathBuf>,
        runs: Mutex<VecDeque<CommandOutput>>,
        tui: Mutex<VecDeque<TuiTranscript>>,
        tui_ran: Mutex<Vec<(String, PathBuf)>>,
        ran: Mutex<Vec<(Vec<String>, Vec<String>, Vec<(String, String)>)>>,
        https: Mutex<VecDeque<Result<QuotaResponse, String>>>,
        requested: Mutex<Vec<(String, String)>>,
        home: PathBuf,
        now: AtomicI64,
    }
    impl FixtureLocal {
        fn codex(
            files: Vec<(PathBuf, Vec<u8>)>,
            https: Vec<Result<QuotaResponse, String>>,
        ) -> Self {
            Self {
                files: Mutex::new(files.into_iter().collect()),
                reads: Mutex::new(0),
                flip_after: None,
                flipped: Mutex::new(HashMap::new()),
                binaries: HashMap::new(),
                runs: Mutex::new(VecDeque::new()),
                tui: Mutex::new(VecDeque::new()),
                tui_ran: Mutex::new(Vec::new()),
                ran: Mutex::new(Vec::new()),
                https: Mutex::new(https.into_iter().collect()),
                requested: Mutex::new(Vec::new()),
                home: PathBuf::from("/fixture/home"),
                now: AtomicI64::new(1900000000),
            }
        }
    }
    impl LocalBackend for FixtureLocal {
        fn read_file(&self, path: &Path) -> Option<Vec<u8>> {
            let mut reads = self.reads.lock().unwrap();
            *reads += 1;
            if self.flip_after.is_some_and(|n| *reads > n) {
                return self.flipped.lock().unwrap().get(path).cloned();
            }
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
        fn run_capture(
            &self,
            program: &Path,
            args: &[&str],
            remove_env: &[&str],
            extra_env: &[(&str, &str)],
            _: &Path,
            _: Duration,
        ) -> Option<CommandOutput> {
            self.ran.lock().unwrap().push((
                std::iter::once(program.to_string_lossy().into_owned())
                    .chain(args.iter().map(|s| s.to_string()))
                    .collect(),
                remove_env.iter().map(|s| s.to_string()).collect(),
                extra_env
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ));
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
        fn now(&self) -> i64 {
            self.now.load(Ordering::SeqCst)
        }
    }
    fn codex_access(exp: i64) -> String {
        let payload = serde_json::json!({"exp":exp,"email":"fixture@example.invalid",
            "https://api.openai.com/auth":{"chatgpt_account_id":"fixture-acct","chatgpt_plan_type":"plus"}});
        format!(
            "synthetic.{}.synthetic",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    }
    fn codex_auth(home: &Path, access: &str) -> (PathBuf, Vec<u8>) {
        (
            home.join(".codex").join("auth.json"),
            serde_json::json!({"auth_mode":"chatgpt","tokens":{
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
    let home = PathBuf::from("/fixture/home");
    let ready_body = json!({"plan_type":"plus","rate_limit":{
        "primary_window":{"used_percent":30,"limit_window_seconds":18000,"reset_at":1900000001},
        "secondary_window":{"limit_window_seconds":604800,"reset_at":1900100000}},
        "credits":{"has_credits":true,"unlimited":false,"balance":"12.50"}});
    let backend = FixtureLocal::codex(
        vec![codex_auth(&home, &codex_access(4102444800))],
        vec![wham(200, ready_body)],
    );
    let service = QuotaService::default();
    let first = service.fetch_local_with(&backend, "openai", false);
    assert_eq!(first.status, QuotaStatus::Ready);
    assert_eq!(
        first.windows.len(),
        1,
        "missing usage must not become an unused window"
    );
    assert_eq!(first.service.as_deref(), Some("Codex"));
    assert_eq!(first.account.as_deref(), Some("fixture@example.invalid"));
    assert_eq!(first.plan.as_deref(), Some("plus"));
    assert_eq!(first.balances.len(), 1);
    assert_eq!(
        backend.requested.lock().unwrap()[0].1,
        "https://chatgpt.com/backend-api/wham/usage"
    );
    assert!(!serde_json::to_string(&first)?.contains("synthetic-"));
    service.fetch_local_with(&backend, "openai", false);
    assert_eq!(
        backend.https.lock().unwrap().len(),
        0,
        "local cache serves the second read"
    );
    passed.push(
        "Codex wham shape from the machine sign-in; missing window is unknown; secret boundary holds"
            .into(),
    );

    // A passed reset is not a fresh zero reading: 503 keeps the dated windows.
    let backend = FixtureLocal::codex(
        vec![codex_auth(&home, &codex_access(4102444800))],
        vec![
            wham(
                200,
                json!({"rate_limit":{"primary_window":{"used_percent":30,
                    "limit_window_seconds":18000,"reset_at":1900000001}}}),
            ),
            wham(503, json!({})),
        ],
    );
    // Force past the cooldown via the fixture clock so the second read requeries.
    let service = QuotaService::default();
    let first = service.fetch_local_with(&backend, "openai", false);
    assert_eq!(first.windows[0].used_percent, Some(30.0));
    backend.now.fetch_add(61, Ordering::SeqCst);
    let stale = service.fetch_local_with(&backend, "openai", false);
    assert_eq!(stale.status, QuotaStatus::Unavailable);
    assert_eq!(
        stale.windows[0].used_percent,
        Some(30.0),
        "passed reset is not a fresh zero reading"
    );
    passed.push("Codex temporary failure keeps the dated snapshot".into());

    // Expired, revoked and unknown 401s.
    for (status, body, expected, keeps) in [
        (
            401,
            json!({"detail":"Provided authentication token is expired. Please try refreshing it."}),
            QuotaStatus::NeedsAuth,
            false,
        ),
        (
            401,
            json!({"detail":"token_revoked"}),
            QuotaStatus::NeedsAuth,
            false,
        ),
        (
            401,
            json!({"detail":"some other refusal"}),
            QuotaStatus::Unavailable,
            true,
        ),
        (403, json!({}), QuotaStatus::AccessDenied, false),
        (429, json!({}), QuotaStatus::RateLimited, true),
    ] {
        let backend = FixtureLocal::codex(
            vec![codex_auth(&home, &codex_access(4102444800))],
            vec![
                wham(
                    200,
                    json!({"rate_limit":{"primary_window":{"used_percent":30,
                    "limit_window_seconds":18000,"reset_at":1900000001}}}),
                ),
                wham(status, body),
            ],
        );
        let service = QuotaService::default();
        let _ = service.fetch_local_with(&backend, "openai", false);
        backend.now.fetch_add(61, Ordering::SeqCst);
        let q = service.fetch_local_with(&backend, "openai", false);
        assert_eq!(q.status, expected);
        assert_eq!(q.windows.is_empty(), !keeps);
    }
    passed
        .push("Codex expiry and revocation re-login; unknown 401 and 429 keep the reading".into());

    // Login shapes that never reach the vendor.
    for (files, expected) in [
        (vec![], QuotaStatus::NoCredential),
        (
            vec![(home.join(".codex").join("auth.json"), b"not json".to_vec())],
            QuotaStatus::NoCredential,
        ),
        (
            vec![codex_auth(&home, &codex_access(1))],
            QuotaStatus::NeedsAuth,
        ),
    ] {
        let backend = FixtureLocal::codex(files, vec![]);
        let q = QuotaService::default().fetch_local_with(&backend, "openai", true);
        assert_eq!(q.status, expected);
        assert!(backend.requested.lock().unwrap().is_empty());
    }
    let (auth_path, _) = codex_auth(&home, &codex_access(4102444800));
    let mut apikey = std::collections::HashMap::new();
    apikey.insert(
        auth_path,
        serde_json::json!({"auth_mode":"apikey","tokens":{"access_token":"synthetic-secret-do-not-leak"}})
            .to_string()
            .into_bytes(),
    );
    let mut backend = FixtureLocal::codex(vec![], vec![]);
    backend.files = Mutex::new(apikey);
    let q = QuotaService::default().fetch_local_with(&backend, "openai", true);
    assert_eq!(q.status, QuotaStatus::Unsupported);
    assert!(backend.requested.lock().unwrap().is_empty());
    passed.push("Codex apikey mode, missing or expired sign-in never query".into());

    // An encrypted store without auth.json is unreadable, not logged out.
    for mode in ["keyring", "auto", "ephemeral"] {
        let mut files = HashMap::new();
        files.insert(
            home.join(".codex").join("config.toml"),
            format!("cli_auth_credentials_store = \"{mode}\"\n").into_bytes(),
        );
        let mut backend = FixtureLocal::codex(vec![], vec![]);
        backend.files = Mutex::new(files);
        let q = QuotaService::default().fetch_local_with(&backend, "openai", true);
        assert_eq!(q.status, QuotaStatus::Unavailable, "{mode}");
        assert!(backend.requested.lock().unwrap().is_empty());
    }
    // A file store without auth.json is still plain logged-out.
    let mut files = HashMap::new();
    files.insert(
        home.join(".codex").join("config.toml"),
        b"cli_auth_credentials_store = \"file\"\n".to_vec(),
    );
    let mut backend = FixtureLocal::codex(vec![], vec![]);
    backend.files = Mutex::new(files);
    let q = QuotaService::default().fetch_local_with(&backend, "openai", true);
    assert_eq!(q.status, QuotaStatus::NoCredential);
    passed.push(
        "Codex encrypted stores without auth.json stay Unavailable, never NoCredential".into(),
    );

    // The account changing mid-query discards the response.
    let mut backend = FixtureLocal::codex(
        vec![codex_auth(&home, &codex_access(4102444800))],
        vec![wham(
            200,
            json!({"rate_limit":{"primary_window":{"used_percent":30,
                "limit_window_seconds":18000,"reset_at":1900000001}}}),
        )],
    );
    backend.flip_after = Some(1);
    backend.flipped = Mutex::new(
        vec![codex_auth(&home, &codex_access(4102444900))]
            .into_iter()
            .collect(),
    );
    let q = QuotaService::default().fetch_local_with(&backend, "openai", true);
    assert_eq!(q.status, QuotaStatus::Unavailable);
    assert!(q.windows.is_empty());
    passed.push("Codex account change during HTTP discards the response".into());

    // Claude Code: status identity plus /usage windows from the local CLI.
    fn claude_backend(status: Value, usage_stdout: Vec<u8>) -> FixtureLocal {
        let mut backend = FixtureLocal::codex(vec![], vec![]);
        backend
            .binaries
            .insert("claude".into(), PathBuf::from("/fixture/claude"));
        backend.runs = Mutex::new(
            vec![CommandOutput {
                success: true,
                stdout: status.to_string().into_bytes(),
                stderr: Vec::new(),
            }]
            .into(),
        );
        let text = serde_json::from_slice::<Value>(&usage_stdout)
            .ok()
            .and_then(|value| value["result"].as_str().map(str::to_owned))
            .unwrap_or_else(|| String::from_utf8_lossy(&usage_stdout).into_owned());
        backend.tui = Mutex::new(
            vec![TuiTranscript {
                text: text.replace("resets", "Resets"),
                saw_denied: false,
            }]
            .into(),
        );
        backend
    }
    fn usage_envelope(result: &str) -> Vec<u8> {
        serde_json::json!({"type":"result","is_error":false,"result":result})
            .to_string()
            .into_bytes()
    }
    let backend = claude_backend(
        json!({"loggedIn":true,"email":"fixture@example.invalid","subscriptionType":"pro"}),
        usage_envelope(
            "Current session: 13% used \u{b7} resets Oct 1 at 3:30pm (UTC)\nCurrent week (all models): 4% used \u{b7} resets Oct 3 at 2pm (UTC)",
        ),
    );
    let q = QuotaService::default().fetch_local_with(&backend, "anthropic", false);
    assert_eq!(q.status, QuotaStatus::Ready);
    assert_eq!(q.windows.len(), 2);
    assert_eq!(q.windows[0].used_percent, Some(13.0));
    assert_eq!(q.windows[1].used_percent, Some(4.0));
    assert_eq!(q.plan.as_deref(), Some("pro"));
    assert_eq!(q.service.as_deref(), Some("Claude Code"));
    assert_eq!(q.account.as_deref(), Some("fixture@example.invalid"));
    assert!(!serde_json::to_string(&q)?.contains("synthetic-"));
    passed.push("Claude status identity and interactive /usage windows".into());

    // Claude sign-out, denied and unreadable answers.
    let backend = claude_backend(json!({"loggedIn":false}), usage_envelope(""));
    let q = QuotaService::default().fetch_local_with(&backend, "anthropic", false);
    assert_eq!(q.status, QuotaStatus::NoCredential);
    assert_eq!(
        backend.ran.lock().unwrap().len(),
        1,
        "signed out runs no usage"
    );
    let backend = claude_backend(
        json!({"loggedIn":true,"email":"fixture@example.invalid"}),
        serde_json::json!({"type":"result","is_error":true,
            "result":"Claude Code: not logged in. Run claude auth login."})
        .to_string()
        .into_bytes(),
    );
    let q = QuotaService::default().fetch_local_with(&backend, "anthropic", false);
    assert_eq!(q.status, QuotaStatus::NeedsAuth);
    for stdout in [
        b"not json at all".to_vec(),
        usage_envelope(
            "You are currently using your subscription to power your Claude Code usage.",
        ),
    ] {
        let backend = claude_backend(
            json!({"loggedIn":true,"email":"fixture@example.invalid"}),
            stdout,
        );
        let q = QuotaService::default().fetch_local_with(&backend, "anthropic", false);
        assert_eq!(
            q.status,
            QuotaStatus::Unavailable,
            "temporary or newer-CLI output is never zero"
        );
        assert!(q.windows.is_empty());
    }
    // A window without a reset stamp is still a reading, just one whose
    // renewal is unknown: kept, not invented.
    let backend = claude_backend(
        json!({"loggedIn":true,"email":"fixture@example.invalid"}),
        usage_envelope("Current session: 3% used"),
    );
    let q = QuotaService::default().fetch_local_with(&backend, "anthropic", false);
    assert_eq!(q.status, QuotaStatus::Ready);
    assert_eq!(q.windows.len(), 1);
    assert_eq!(q.windows[0].resets_at, None);
    let backend = FixtureLocal::codex(vec![], vec![]);
    let q = QuotaService::default().fetch_local_with(&backend, "anthropic", false);
    assert_eq!(q.status, QuotaStatus::NoCredential);
    passed
        .push("Claude sign-out, denial, unreadable and newer-CLI panels classify honestly".into());

    // The exclusive PTY stage reads the interactive CLI's captured screen.
    let tui_panel = |text: &str| TuiTranscript {
        text: text.to_owned(),
        saw_denied: false,
    };
    // The PTY exposes screen cells, not the raw ANSI byte stream.
    let mut backend = claude_backend(
        json!({"loggedIn":true,"email":"fixture@example.invalid","subscriptionType":"pro"}),
        usage_envelope(
            "You are currently using your subscription to power your Claude Code usage.",
        ),
    );
    backend.tui = Mutex::new(
        vec![tui_panel("Current session: 13% used · Resets Oct 1 at 3:30pm (UTC)\nCurrent week (all models): 4% used · Resets Oct 3 at 2pm (UTC)")]
            .into(),
    );
    let q = QuotaService::default().fetch_local_with(&backend, "anthropic", false);
    assert_eq!(q.status, QuotaStatus::Ready);
    assert_eq!(q.windows.len(), 2);
    assert_eq!(q.windows[0].used_percent, Some(13.0));
    let tui_ran = backend.tui_ran.lock().unwrap();
    assert_eq!(tui_ran.len(), 1, "the panel came from the PTY, not -p");
    assert!(tui_ran[0].0.contains("claude"), "the PTY owns its own CLI");
    assert_eq!(
        tui_ran[0].1,
        PathBuf::from("/fixture/tmp/fintwind-claude-work"),
        "the PTY runs in the stable directory"
    );
    drop(tui_ran);
    passed.push("Claude PTY types /usage into its own console and parses the panel".into());

    // A denied PTY panel re-logs in with the account attached.
    let mut backend = claude_backend(
        json!({"loggedIn":true,"email":"fixture@example.invalid"}),
        usage_envelope(
            "You are currently using your subscription to power your Claude Code usage.",
        ),
    );
    backend.tui = Mutex::new(
        vec![TuiTranscript {
            text: "Claude Code: not logged in. Run claude auth login.".to_owned(),
            saw_denied: true,
        }]
        .into(),
    );
    let q = QuotaService::default().fetch_local_with(&backend, "anthropic", false);
    assert_eq!(q.status, QuotaStatus::NeedsAuth);
    assert_eq!(q.service.as_deref(), Some("Claude Code"));
    assert_eq!(q.account.as_deref(), Some("fixture@example.invalid"));
    passed.push("Claude PTY denial re-logs in with the account attached".into());

    // Zhipu/Z.ai account report: refused keys re-login, other failures stay temporary.
    for (body, expected) in [
        (
            json!({"code":1000,"msg":"synthetic-secret-do-not-leak","success":false}),
            QuotaStatus::NeedsAuth,
        ),
        (
            json!({"error":{"code":"1002","message":"synthetic-secret-do-not-leak"}}),
            QuotaStatus::NeedsAuth,
        ),
        (
            json!({"code":500,"msg":"busy","success":false}),
            QuotaStatus::Unavailable,
        ),
        (
            json!({"code":200,"success":true,"data":{"availableBalance":"NaN"}}),
            QuotaStatus::NoData,
        ),
    ] {
        let source = Source::new("zhipuai", key(), body);
        let service = QuotaService::default();
        let q = service.fetch(&source, "scope", "zhipuai", false);
        assert_eq!(q.status, expected);
        assert!(q.balances.is_empty());
        assert!(!serde_json::to_string(&q)?.contains("synthetic-"));
        let _ = service.fetch(&source, "scope", "zhipuai", true);
        assert_eq!(
            source.requests.lock().unwrap().len(),
            1,
            "manual refresh floor"
        );
    }
    passed.push(
        "Zhipu refusal re-login; malformed balance is never zero; secret boundary holds".into(),
    );

    let source = Source::new(
        "minimax-coding-plan",
        key(),
        json!({"base_resp":{"status_code":1004,"status_msg":"synthetic-secret-do-not-leak"}}),
    );
    let q = QuotaService::default().fetch(&source, "scope", "minimax-coding-plan", false);
    assert_eq!(q.status, QuotaStatus::NeedsAuth);
    assert!(!serde_json::to_string(&q)?.contains("synthetic-"));
    passed.push("MiniMax HTTP 200 credential failure is not success or zero allowance".into());

    let source = Source::new("deepseek", key(), json!({}));
    source.replies.lock().unwrap()[0].body = "not JSON: synthetic-secret-do-not-leak".into();
    let q = QuotaService::default().fetch(&source, "scope", "deepseek", false);
    assert_eq!(q.status, QuotaStatus::Unavailable);
    assert!(!serde_json::to_string(&q)?.contains("synthetic-"));
    passed.push("malformed upstream data stays behind the secret boundary".into());

    let source = Source::new(
        "deepseek",
        key(),
        json!({"balance_infos":[{"currency":"CNY","total_balance":"2.00"}]}),
    );
    let service = QuotaService::default();
    std::thread::scope(|scope| {
        let a = scope.spawn(|| service.fetch(&source, "scope", "deepseek", false));
        let b = scope.spawn(|| service.fetch(&source, "scope", "deepseek", false));
        assert_eq!(a.join().unwrap().status, QuotaStatus::Ready);
        assert_eq!(b.join().unwrap().status, QuotaStatus::Ready);
    });
    assert_eq!(source.requests.lock().unwrap().len(), 1);
    passed.push("concurrent reads share one vendor request".into());

    struct Switching(Source);
    impl QuotaSource for Switching {
        fn now(&self) -> i64 {
            self.0.now()
        }
        fn credentials(&self) -> Result<Value> {
            self.0.credentials()
        }
        fn request(&self, r: &QuotaRequest) -> Result<QuotaResponse> {
            let reply = self.0.request(r)?;
            self.0.credentials.lock().unwrap()["data"][0]["id"] =
                json!("new-account-during-request");
            Ok(reply)
        }
    }
    let source = Switching(Source::new(
        "deepseek",
        key(),
        json!({"balance_infos":[{"currency":"CNY","total_balance":"2.00"}]}),
    ));
    let q = QuotaService::default().fetch(&source, "scope", "deepseek", false);
    assert_eq!(q.status, QuotaStatus::Unavailable);
    assert!(q.balances.is_empty());
    passed.push("account change during HTTP discards the response".into());

    // Actual 2026-10-15 Zhipu response shape: Credit-based plan, not TOKENS_LIMIT.
    let source = Source::new(
        "zhipuai-coding-plan",
        key(),
        json!({"success":true,"data":{"limits":[
            {"type":"CREDIT_LIMIT","unit":3,"number":5,"percentage":12},
            {"type":"CREDIT_LIMIT","unit":6,"number":1,"percentage":31,"nextResetTime":2000000000000_i64}
        ]}}),
    );
    let quota = QuotaService::default().fetch(&source, "scope", "zhipuai-coding-plan", false);
    assert_eq!(quota.status, QuotaStatus::Ready);
    assert_eq!(quota.windows.len(), 2);
    assert_eq!(quota.windows[0].used_percent, Some(12.0));
    assert_eq!(quota.windows[0].resets_at, None);
    assert_eq!(quota.windows[1].resets_at, Some(2000000000));
    passed.push(
        "Zhipu CREDIT_LIMIT keeps rolling/weekly windows without inventing reset times".into(),
    );

    struct EnvSource {
        inner: Source,
        key: Mutex<Option<String>>,
    }
    impl QuotaSource for EnvSource {
        fn credentials(&self) -> Result<Value> {
            Ok(json!({"data":[]}))
        }
        fn environment_credential(&self, provider: &str) -> Result<Option<Value>> {
            Ok(
                fintwind_core::provider_quota::environment_credential_from_connection(
                    provider,
                    &json!({"data":{"id":"deepseek","connections":[{"type":"env","name":"DEEPSEEK_API_KEY"}]}}),
                    |_| self.key.lock().unwrap().clone(),
                ),
            )
        }
        fn request(&self, request: &QuotaRequest) -> Result<QuotaResponse> {
            self.inner.request(request)
        }
        fn now(&self) -> i64 {
            self.inner.now()
        }
    }
    let source = EnvSource {
        inner: Source::new(
            "deepseek",
            key(),
            json!({"balance_infos":[{"currency":"CNY","total_balance":"3.21"}]}),
        ),
        key: Mutex::new(Some("synthetic-env-key-do-not-leak".into())),
    };
    let service = QuotaService::default();
    let quota = service.fetch(&source, "scope", "deepseek", false);
    assert_eq!(quota.status, QuotaStatus::Ready);
    assert_eq!(quota.balances[0].remaining, "3.21");
    assert!(!serde_json::to_string(&quota)?.contains("synthetic-"));
    *source.key.lock().unwrap() = None;
    let quota = service.fetch(&source, "scope", "deepseek", false);
    assert_eq!(quota.status, QuotaStatus::NoCredential);
    assert!(quota.balances.is_empty());
    passed.push(
        "DeepSeek registered environment connection resolves balance and invalidates on removal"
            .into(),
    );
    for integration in [
        json!({"data":{"id":"deepseek","connections":[{"type":"env","name":"OTHER_SECRET"}]}}),
        json!({"data":{"id":"other","connections":[{"type":"env","name":"DEEPSEEK_API_KEY"}]}}),
        json!({"data":{"id":"deepseek","connections":[{"type":"credential","id":"existing"},{"type":"env","name":"DEEPSEEK_API_KEY"}]}}),
    ] {
        let credential = fintwind_core::provider_quota::environment_credential_from_connection(
            "deepseek",
            &integration,
            |_| panic!("must not read unselected/unregistered environment"),
        );
        assert!(credential.is_none());
    }
    passed.push("environment resolution refuses foreign names, providers, and credential precedence conflicts".into());

    // Contract cases adapted from CodexBar KimiRatioPoolTests at 278e9056.
    let source = Source::new(
        "kimi-code-plan-global",
        key(),
        json!({"usages":{
            "limit_5h":{"used_ratio":0.625},
            "limit_7d":{"used_ratio":0.125,"reset_time":"2033-01-01T00:00:00Z"},
            "limit_month_total":{"used_ratio":1.05}
        }}),
    );
    let quota = QuotaService::default().fetch(&source, "scope", "kimi-code-plan-global", false);
    assert_eq!(quota.status, QuotaStatus::Ready);
    assert_eq!(quota.windows.len(), 3);
    assert_eq!(quota.windows[0].used_percent, Some(62.5));
    assert_eq!(quota.windows[1].used_percent, Some(12.5));
    assert_eq!(quota.windows[2].used_percent, Some(100.0));
    passed.push("Kimi ratio pools keep session, weekly and monthly allowance apart".into());
    for (count_reset, expected) in [
        ("2033-01-01T00:00:01Z", 19.0),
        ("2033-01-08T00:00:00Z", 0.0),
    ] {
        let source = Source::new(
            "kimi-code-plan-cn",
            key(),
            json!({
                "usage":{"limit":"100","used":"19","remaining":"90","resetTime":count_reset},
                "usages":{"limit_7d":{"used_ratio":0,"reset_time":"2033-01-01T00:00:00Z"}}
            }),
        );
        let quota = QuotaService::default().fetch(&source, "scope", "kimi-code-plan-cn", false);
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].used_percent, Some(expected));
    }
    passed.push("Kimi matching resets take tighter reliable counters; different periods keep ratio authoritative".into());
    let source = Source::new(
        "kimi-code-plan-cn",
        key(),
        json!({
            "usage":{"limit":"100"},"usages":{"limit_month_total":{"used_ratio":0.0313}}
        }),
    );
    let quota = QuotaService::default().fetch(&source, "scope", "kimi-code-plan-cn", false);
    assert_eq!(quota.windows.len(), 1);
    assert_eq!(
        quota.windows[0].kind,
        fintwind_protocol::provider_quota::QuotaWindowKind::Monthly
    );
    passed.push("Kimi incomplete counters never fabricate a zero weekly window".into());
    let source = Source::new(
        "minimax-coding-plan",
        key(),
        json!({"data":{
            "base_resp":{"status_code":0},"plan_name":"fixture-plan","model_remains":[{
                "model_name":"general","current_interval_remaining_percent":80,
                "start_time":2000000000000_i64,"end_time":2000018000000_i64,
                "current_weekly_remaining_percent":100,"current_weekly_status":3
            }]
        }}),
    );
    let quota = QuotaService::default().fetch(&source, "scope", "minimax-coding-plan", false);
    assert_eq!(quota.status, QuotaStatus::Ready);
    assert_eq!(quota.windows.len(), 1);
    assert_eq!(quota.windows[0].used_percent, Some(20.0));
    assert_eq!(quota.plan.as_deref(), Some("fixture-plan"));
    passed.push(
        "MiniMax wrapped payload, missing optional status and absent weekly allowance".into(),
    );
    for (provider, name) in [
        ("zai-coding-plan", "ZHIPU_API_KEY"),
        ("kimi-code-plan-cn", "KIMI_API_KEY"),
        ("minimax-cn-coding-plan", "MINIMAX_API_KEY"),
        ("stepfun", "STEPFUN_API_KEY"),
        ("openrouter", "OPENROUTER_API_KEY"),
        ("siliconflow", "SILICONFLOW_API_KEY"),
        ("siliconflow-cn", "SILICONFLOW_CN_API_KEY"),
        ("moonshotai", "MOONSHOT_API_KEY"),
        ("moonshotai-cn", "MOONSHOT_API_KEY"),
        ("opencode-go", "OPENCODE_API_KEY"),
        ("commandcode", "CMD_API_KEY"),
        ("ollama-cloud", "OLLAMA_API_KEY"),
        ("zhipuai", "ZHIPU_API_KEY"),
        ("minimax", "MINIMAX_API_KEY"),
    ] {
        let credential = fintwind_core::provider_quota::environment_credential_from_connection(
            provider,
            &json!({"data":{"id":provider,"connections":[{"type":"env","name":name}]}}),
            |asked| {
                assert_eq!(asked, name);
                Some("synthetic-key".into())
            },
        );
        assert!(credential.is_some());
    }
    passed.push("coding-plan keys resolve only exact registered environment names".into());

    let credentials = json!({"data":[
        {"id":"older","integrationID":"deepseek","active":false,"value":{"type":"key","key":"synthetic-older"}},
        {"id":"newer","integrationID":"deepseek","active":false,"value":{"type":"key","key":"synthetic-newer"}}
    ]});
    let integration = json!({"data":{"id":"deepseek","connections":[{"type":"credential","id":"newer"},{"type":"credential","id":"older"}]}});
    let selected = fintwind_core::provider_quota::credentials_for_connection(
        "deepseek",
        &credentials,
        &integration,
    );
    assert_eq!(selected["data"][0]["id"].as_str(), Some("newer"));
    assert_eq!(selected["data"][0]["active"].as_bool(), Some(true));
    let integration =
        json!({"data":{"id":"deepseek","connections":[{"type":"env","name":"DEEPSEEK_API_KEY"}]}});
    let selected = fintwind_core::provider_quota::credentials_for_connection(
        "deepseek",
        &credentials,
        &integration,
    );
    assert_eq!(selected["data"].as_array().unwrap().len(), 0);
    passed.push(
        "server connection order selects legacy credentials without guessing active flags".into(),
    );

    let report = json!({"suite":"provider-quota-blackbox","passed":passed.len(),"checks":passed});
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(serde_json::to_string_pretty(&report)?.as_bytes())?;
    println!("{}", report);
    Ok(())
}

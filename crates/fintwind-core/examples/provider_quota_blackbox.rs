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
            "openai",
            oauth("chatgpt-headless", json!({"accountID":"fixture"})),
            json!({
                "plan_type":"plus", "rate_limit":{
                    "primary_window":{"used_percent":23,"limit_window_seconds":18000,"reset_at":2000000000},
                    "secondary_window":{"used_percent":41,"limit_window_seconds":604800,"reset_at":2000100000}},
                "credits":{"has_credits":true,"unlimited":false,"balance":"12.50"}
            }),
            2,
            1,
            "https://chatgpt.com/backend-api/wham/usage",
        ),
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
        if provider == "minimax-coding-plan" {
            assert_eq!(q.windows[1].used_percent, Some(100.0));
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
        ("google", oauth("gemini-cli", json!({}))),
        (
            "github-copilot",
            oauth("device", json!({"enterpriseUrl":"example.org"})),
        ),
        ("stepfun-step-plan", key()),
        ("deepseek", json!({"type":"external","methodID":"file"})),
    ] {
        let source = Source::new(provider, auth, json!({}));
        let q = QuotaService::default().fetch(&source, "scope", provider, false);
        assert_eq!(q.status, QuotaStatus::Unsupported);
        assert!(source.requests.lock().unwrap().is_empty());
        passed.push(format!(
            "{provider}: incompatible credential does not call vendor"
        ));
    }
    let source = Source::new(
        "openai",
        json!({"type":"oauth","methodID":"chatgpt-headless",
        "access":"synthetic-expired","expires":1,"metadata":{"accountID":"fixture"}}),
        json!({}),
    );
    assert_eq!(
        QuotaService::default()
            .fetch(&source, "scope", "openai", false)
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

    let source = Source::new(
        "openai",
        oauth("chatgpt-headless", json!({"accountID":"fixture"})),
        json!({"rate_limit":{
            "primary_window":{"used_percent":30,"limit_window_seconds":18000,"reset_at":1900000001},
            "secondary_window":{"limit_window_seconds":604800,"reset_at":1900100000}}
        }),
    );
    let service = QuotaService::default();
    let first = service.fetch(&source, "scope", "openai", false);
    assert_eq!(
        first.windows.len(),
        1,
        "missing usage must not become an unused window"
    );
    source.now.fetch_add(61, Ordering::SeqCst);
    source.replies.lock().unwrap().push_back(QuotaResponse {
        status: 503,
        body: String::new(),
        retry_after: None,
    });
    let stale = service.fetch(&source, "scope", "openai", false);
    assert_eq!(
        stale.windows[0].used_percent,
        Some(30.0),
        "passed reset is not a fresh zero reading"
    );
    passed
        .push("missing window is unknown; a passed reset does not invent fresh zero usage".into());

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

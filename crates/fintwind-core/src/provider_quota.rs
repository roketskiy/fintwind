//! Read-only vendor allowance adapters. All I/O stays on the daemon request thread.
//! OpenCode owns credentials and their refresh. No discovery, inference, or writes.
//! Vendor response handling follows Magpie's 43849d51 snapshot. See
//! docs/provider-quota-upstream-license.txt for their MIT license notices.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use fintwind_protocol::provider_quota::{
    ProviderQuota, QuotaBalance, QuotaStatus, QuotaWindow, QuotaWindowKind, is_local_quota_source,
    supports_provider_quota,
};

pub use fintwind_client::local_quota::{QuotaRequest, QuotaResponse};

pub trait QuotaSource {
    fn credentials(&self) -> anyhow::Result<Value>;
    fn credentials_for(&self, _provider: &str) -> anyhow::Result<Value> {
        self.credentials()
    }
    /// Only registered, selected environment connections may be resolved. The
    /// returned secret is daemon-local, just like `/api/credential` material.
    fn environment_credential(&self, _provider: &str) -> anyhow::Result<Option<Value>> {
        Ok(None)
    }
    fn request(&self, request: &QuotaRequest) -> anyhow::Result<QuotaResponse>;
    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}

struct ServerSource<'a>(&'a crate::opencode_session::OpenCodeServer);

impl QuotaSource for ServerSource<'_> {
    fn credentials(&self) -> anyhow::Result<Value> {
        self.0.request("GET", "/api/credential", None)
    }

    fn credentials_for(&self, provider: &str) -> anyhow::Result<Value> {
        let credentials = self.credentials()?;
        let integration = self
            .0
            .request("GET", &format!("/api/integration/{provider}"), None)?;
        Ok(credentials_for_connection(
            provider,
            &credentials,
            &integration,
        ))
    }

    fn environment_credential(&self, provider: &str) -> anyhow::Result<Option<Value>> {
        if quota_environment_name(provider).is_none() {
            return Ok(None);
        }
        let integration = self
            .0
            .request("GET", &format!("/api/integration/{provider}"), None)?;
        Ok(environment_credential_from_connection(
            provider,
            &integration,
            |name| {
                // Match the captured environment passed to the resident OpenCode
                // server; on Windows it inherits the daemon's process environment.
                crate::command_env::shell_environment()
                    .into_iter()
                    .find(|(key, _)| key == name)
                    .and_then(|(_, value)| value.into_string().ok())
                    .or_else(|| std::env::var(name).ok())
            },
        ))
    }

    fn request(&self, request: &QuotaRequest) -> anyhow::Result<QuotaResponse> {
        let (status, body, retry_after) = fintwind_protocol::http::http_json(
            request.method,
            &request.url,
            &request.headers,
            request.body.as_deref(),
            15,
        )?;
        Ok(QuotaResponse {
            status,
            body,
            retry_after,
        })
    }
}

#[derive(Default)]
struct Cached {
    identity: Option<[u8; 32]>,
    last: Option<ProviderQuota>,
    good: Option<ProviderQuota>,
    next_auto: i64,
    next_manual: i64,
}

/// Per-server/provider caches serialize duplicate reads without blocking other vendors.
/// Neither credential material nor raw responses are retained in this cache.
#[derive(Default)]
pub struct QuotaService {
    entries: Mutex<HashMap<(String, String), Arc<Mutex<Cached>>>>,
    pub(crate) local: fintwind_client::local_quota::LocalQuotaCache,
}

impl QuotaService {
    pub(crate) fn fetch_from_server(
        &self,
        server: &crate::opencode_session::OpenCodeServer,
        provider: &str,
        force: bool,
    ) -> ProviderQuota {
        if provider == "opencode" {
            return self.fetch_opencode_limits(server, force);
        }
        self.fetch(
            &ServerSource(server),
            &server.port.to_string(),
            provider,
            force,
        )
    }

    /// Console OAuth belongs to OpenCode, not the `opencode-go` inference
    /// provider. Ask the running server to resolve its own membership and
    /// workspace; do not extract its OAuth token or require a second API key.
    fn fetch_opencode_limits(
        &self,
        server: &crate::opencode_session::OpenCodeServer,
        force: bool,
    ) -> ProviderQuota {
        let entry = self
            .entries
            .lock()
            .entry((server.port.to_string(), "opencode".into()))
            .or_default()
            .clone();
        let mut cache = entry.lock();
        let connection_identity = || -> anyhow::Result<Option<[u8; 32]>> {
            let integration = server.request("GET", "/api/integration/opencode", None)?;
            let integration = integration.get("data").unwrap_or(&integration);
            Ok(integration["connections"]
                .as_array()
                .filter(|rows| !rows.is_empty())
                .map(|rows| Sha256::digest(serde_json::to_vec(rows).unwrap_or_default()).into()))
        };
        let identity = match connection_identity() {
            Ok(Some(identity)) => identity,
            Ok(None) => {
                *cache = Cached::default();
                return ProviderQuota::empty(QuotaStatus::NoCredential);
            }
            Err(_) => return ProviderQuota::empty(QuotaStatus::Unavailable),
        };
        if cache.identity != Some(identity) {
            *cache = Cached {
                identity: Some(identity),
                ..Default::default()
            };
        }
        let now = chrono::Utc::now().timestamp();
        if now
            < if force {
                cache.next_manual
            } else {
                cache.next_auto
            }
            && let Some(last) = &cache.last
        {
            return last.clone();
        }
        let mut reading = match server.request("GET", "/api/experimental/usage/limits", None) {
            Ok(body) => {
                let data = body.get("data").unwrap_or(&body);
                let mut reading = ProviderQuota::empty(QuotaStatus::Ready);
                if data["go"].is_object() {
                    reading.service = Some("OpenCode Go".into());
                    reading.plan = Some("Go".into());
                    for (key, kind, span) in [
                        ("rolling", QuotaWindowKind::Rolling, Some(18000)),
                        ("weekly", QuotaWindowKind::Weekly, Some(604800)),
                        ("monthly", QuotaWindowKind::Monthly, None),
                    ] {
                        let meter = &data["go"][key];
                        if let Some(used) = percent(&meter["percent"]) {
                            reading.windows.push(window(
                                key,
                                kind,
                                Some(used),
                                timestamp(&meter["resetsAt"]),
                                span,
                            ));
                        }
                    }
                }
                if reading.windows.is_empty() {
                    reading.status = QuotaStatus::NoData;
                }
                reading
            }
            Err(_) => ProviderQuota::empty(QuotaStatus::Unavailable),
        };
        if connection_identity().ok().flatten() != Some(identity) {
            *cache = Cached::default();
            return ProviderQuota::empty(QuotaStatus::Unavailable);
        }
        let now = chrono::Utc::now().timestamp();
        cache.next_manual = now.saturating_add(15);
        cache.next_auto = now.saturating_add(60);
        if reading.status == QuotaStatus::Ready {
            reading.measured_at = Some(now);
            cache.good = Some(reading.clone());
        } else if reading.status == QuotaStatus::Unavailable {
            if let Some(good) = cache.good.as_ref().filter(|good| {
                good.measured_at
                    .is_some_and(|at| now.saturating_sub(at) < 86400)
            }) {
                reading = good.clone();
                reading.status = QuotaStatus::Unavailable;
            }
        } else {
            cache.good = None;
        }
        reading.retry_at = (reading.status != QuotaStatus::Ready).then_some(cache.next_auto);
        cache.last = Some(reading.clone());
        reading
    }

    pub fn fetch(
        &self,
        source: &impl QuotaSource,
        scope: &str,
        provider: &str,
        force: bool,
    ) -> ProviderQuota {
        if !supports_provider_quota(provider) {
            return ProviderQuota::empty(QuotaStatus::Unsupported);
        }
        let entry = self
            .entries
            .lock()
            .entry((scope.into(), provider.into()))
            .or_default()
            .clone();
        let mut cache = entry.lock();
        let now = source.now();
        // Read auth even on a cache hit: logout/account selection is authoritative.
        let credentials = match source.credentials_for(provider) {
            Ok(v) => v,
            Err(_) => {
                *cache = Cached::default();
                return ProviderQuota::empty(QuotaStatus::Unavailable);
            }
        };
        let credential = match selected_credential(&credentials, provider) {
            Some(credential) => Some(credential.clone()),
            None => match source.environment_credential(provider) {
                Ok(credential) => credential,
                Err(_) => {
                    *cache = Cached::default();
                    return ProviderQuota::empty(QuotaStatus::Unavailable);
                }
            },
        };
        let Some(credential) = credential else {
            *cache = Cached::default();
            return ProviderQuota::empty(QuotaStatus::NoCredential);
        };
        let identity: [u8; 32] = Sha256::digest(credential.to_string().as_bytes()).into();
        if cache.identity != Some(identity) {
            *cache = Cached {
                identity: Some(identity),
                ..Default::default()
            };
        }
        let request = match request_for(provider, &credential, now) {
            Ok(request) => request,
            Err(status) => {
                cache.good = None;
                cache.last = None;
                return ProviderQuota::empty(status);
            }
        };
        if now
            < if force {
                cache.next_manual
            } else {
                cache.next_auto
            }
        {
            if let Some(last) = &cache.last {
                return last.clone();
            }
        }
        let response = source.request(&request);
        let mut q = match &response {
            Ok(r) if (200..300).contains(&r.status) => serde_json::from_str::<Value>(&r.body)
                .ok()
                .map(|body| parse(provider, &body))
                .unwrap_or_else(|| ProviderQuota::empty(QuotaStatus::Unavailable)),
            Ok(r) => ProviderQuota::empty(match r.status {
                401 => QuotaStatus::NeedsAuth,
                403 => QuotaStatus::AccessDenied,
                429 => QuotaStatus::RateLimited,
                _ => QuotaStatus::Unavailable,
            }),
            Err(_) => ProviderQuota::empty(QuotaStatus::Unavailable),
        };
        // The account may have changed while HTTP was in flight. Never publish it
        // under the new account, even if the UI has not heard of the change yet.
        let unchanged = source.credentials_for(provider).ok().and_then(|v| {
            selected_credential(&v, provider)
                .cloned()
                .or_else(|| source.environment_credential(provider).ok().flatten())
                .map(|c| <[u8; 32]>::from(Sha256::digest(c.to_string().as_bytes())))
        }) == Some(identity);
        if !unchanged {
            *cache = Cached::default();
            return ProviderQuota::empty(QuotaStatus::Unavailable);
        }
        let now = source.now();
        let cooldown = if q.status == QuotaStatus::RateLimited {
            response
                .as_ref()
                .ok()
                .and_then(|r| r.retry_after)
                .unwrap_or(60)
                .max(60)
                .min(i64::MAX as u64) as i64
        } else {
            15
        };
        cache.next_manual = now.saturating_add(cooldown);
        cache.next_auto = now.saturating_add(cooldown.max(60));
        if q.status == QuotaStatus::Ready {
            q.measured_at = Some(now);
            cache.good = Some(q.clone());
        } else if matches!(
            q.status,
            QuotaStatus::Unavailable | QuotaStatus::RateLimited
        ) {
            if let Some(good) = cache.good.as_ref().filter(|g| {
                g.measured_at
                    .is_some_and(|at| now.saturating_sub(at) < 86400)
            }) {
                let status = q.status;
                q = good.clone();
                q.status = status;
            }
        } else {
            cache.good = None;
        }
        q.retry_at = (q.status != QuotaStatus::Ready).then_some(cache.next_auto);
        cache.last = Some(q.clone());
        q
    }
}

fn quota_environment_name(provider: &str) -> Option<&'static str> {
    match provider {
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        // Exact variable names from the models.dev catalog; matched against
        // the server's env connection, never interpolated into a request.
        "zhipuai" | "zhipuai-coding-plan" | "zai" | "zai-coding-plan" => Some("ZHIPU_API_KEY"),
        "kimi-code-plan-cn" | "kimi-code-plan-global" => Some("KIMI_API_KEY"),
        "minimax" | "minimax-cn" | "minimax-coding-plan" | "minimax-cn-coding-plan" => {
            Some("MINIMAX_API_KEY")
        }
        "stepfun" | "stepfun-ai" | "stepfun-ai-step-plan" | "stepfun-step-plan" => {
            Some("STEPFUN_API_KEY")
        }
        // Exact variable names from the models.dev catalog; matched against
        // the server's env connection, never interpolated into a request.
        "ollama-cloud" => Some("OLLAMA_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "siliconflow" => Some("SILICONFLOW_API_KEY"),
        "siliconflow-cn" => Some("SILICONFLOW_CN_API_KEY"),
        "moonshotai" | "moonshotai-cn" => Some("MOONSHOT_API_KEY"),
        "opencode-go" => Some("OPENCODE_API_KEY"),
        "commandcode" => Some("CMD_API_KEY"),
        _ => None,
    }
}

/// Keep only the stored credential selected by OpenCode's ordered connection
/// list. All-zero legacy active flags must not override this authoritative ID.
/// A missing selected record fails closed rather than querying another account.
pub fn credentials_for_connection(provider: &str, stored: &Value, integration: &Value) -> Value {
    let integration = integration.get("data").unwrap_or(integration);
    let connection = integration["connections"]
        .as_array()
        .and_then(|rows| rows.first());
    let selected = connection
        .filter(|c| c["type"].as_str() == Some("credential"))
        .and_then(|c| c["id"].as_str())
        .filter(|_| integration["id"].as_str() == Some(provider))
        .and_then(|id| {
            stored["data"].as_array()?.iter().find(|row| {
                row["id"].as_str() == Some(id) && row["integrationID"].as_str() == Some(provider)
            })
        })
        .map(|row| {
            let mut row = row.clone();
            row["active"] = json!(true);
            row
        });
    json!({"data":selected.into_iter().collect::<Vec<_>>()})
}

/// Resolve a provider's advertised environment connection without scanning auth
/// files or reading arbitrary variable names supplied by a server/plugin.
/// Stored connections take precedence, matching OpenCode's connection order.
pub fn environment_credential_from_connection(
    provider: &str,
    integration: &Value,
    lookup: impl FnOnce(&str) -> Option<String>,
) -> Option<Value> {
    let name = quota_environment_name(provider)?;
    let integration = integration.get("data").unwrap_or(integration);
    if integration["id"].as_str() != Some(provider) {
        return None;
    }
    let connection = integration["connections"].as_array()?.first()?;
    if connection["type"].as_str() != Some("env") || connection["name"].as_str() != Some(name) {
        return None;
    }
    let key = lookup(name).filter(|key| !key.trim().is_empty())?;
    Some(json!({
        "id":format!("env:{name}"), "integrationID":provider, "active":true,
        "value":{"type":"key","key":key}
    }))
}

fn selected_credential<'a>(body: &'a Value, provider: &str) -> Option<&'a Value> {
    let rows = body.get("data")?.as_array()?;
    let mut matching = rows
        .iter()
        .filter(|c| c.get("integrationID").and_then(Value::as_str) == Some(provider));
    let first = matching.next()?;
    let rest: Vec<_> = matching.collect();
    std::iter::once(first)
        .chain(rest.iter().copied())
        .find(|c| c.get("active").and_then(Value::as_bool) == Some(true))
        .or_else(|| rest.is_empty().then_some(first))
}

fn request_for(provider: &str, credential: &Value, now: i64) -> Result<QuotaRequest, QuotaStatus> {
    let value = &credential["value"];
    let kind = value["type"].as_str().unwrap_or_default();
    // Local subscription sources never query through the workspace server;
    // the daemon answers them from the machine's own sign-in.
    if is_local_quota_source(provider) {
        return Err(QuotaStatus::Unsupported);
    }
    // The long-standing adapters keep their exact credential shape: OAuth
    // for the first-party logins, a key for the plan/balance vendors.
    // Adapters added with the Magpie port accept either a stored key or an
    // OAuth access token, whichever the OpenCode connection holds.
    let flexible_oauth = matches!(
        provider,
        "commandcode"
            | "ollama-cloud"
            | "openrouter"
            | "siliconflow"
            | "siliconflow-cn"
            | "moonshotai"
            | "moonshotai-cn"
            | "opencode-go"
            | "zhipuai"
            | "zai"
            | "minimax"
            | "minimax-cn"
            | "stepfun-ai"
            | "stepfun-ai-step-plan"
            | "stepfun-step-plan"
    );
    let oauth =
        matches!(provider, "google" | "github-copilot") || (flexible_oauth && kind == "oauth");
    // Tencent Token Plan and Volcengine Ark plans: their account usage APIs
    // need an AccessKey ID + Secret pair (TC3 / Volc HMAC), never the plan's
    // own inference key. OpenCode's connection info for these IDs expresses
    // exactly one secret per key connection (a single models.dev env key, a
    // single-key /connect flow, sanitized key_accounts with id/label only),
    // so the pair is not expressible and no account call can be signed.
    // The bundled models.dev endpoints confirm the plan mapping
    // (api.lkeap.../coding and /plan, tokenhub.tencentmaas.com/v1,
    // ark.cn-beijing.volces.com/api/coding/v3 vs /api/v3 pay-as-you-go),
    // but without an expressible key pair every one of these IDs is
    // unreachable: Unsupported, never a fabricated zero.
    if matches!(
        provider,
        "tencent-coding-plan"
            | "tencent-token-plan"
            | "tencent-tokenhub"
            | "volcengine"
            | "volcengine-coding-plan"
    ) {
        return Err(QuotaStatus::Unsupported);
    }
    if !flexible_oauth {
        if kind != if oauth { "oauth" } else { "key" } {
            return Err(QuotaStatus::Unsupported);
        }
    } else if kind != "key" && kind != "oauth" {
        return Err(QuotaStatus::Unsupported);
    }
    if oauth && provider != "github-copilot" {
        let expires = value["expires"].as_i64().ok_or(QuotaStatus::NeedsAuth)?;
        if expires <= now.saturating_mul(1000) {
            return Err(QuotaStatus::NeedsAuth);
        }
    }
    let token = value[if oauth { "access" } else { "key" }]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or(QuotaStatus::NeedsAuth)?;
    let metadata = &value["metadata"];
    let mut req = QuotaRequest {
        method: "GET",
        url: String::new(),
        headers: vec![
            format!("Authorization: Bearer {token}"),
            "Accept: application/json".into(),
            "User-Agent: fintwind".into(),
        ],
        body: None,
    };
    req.url = match provider {
        "github-copilot" => {
            if metadata.get("enterpriseUrl").is_some() {
                return Err(QuotaStatus::Unsupported);
            }
            req.headers.push("X-GitHub-Api-Version: 2025-04-01".into());
            "https://api.github.com/copilot_internal/user"
        }
        "google" => {
            let project = ["projectId", "projectID", "project"]
                .iter()
                .find_map(|k| metadata[*k].as_str())
                .filter(|s| !s.is_empty())
                .ok_or(QuotaStatus::Unsupported)?;
            req.method = "POST";
            req.headers.push("Content-Type: application/json".into());
            req.body = Some(json!({"project": project}).to_string());
            "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota"
        }
        "zhipuai-coding-plan" => "https://open.bigmodel.cn/api/monitor/usage/quota/limit",
        "zai-coding-plan" => "https://api.z.ai/api/monitor/usage/quota/limit",
        // The base (non-coding) Zhipu/Z.ai provider reads its account report
        // instead of plan windows, as Magpie does: a Coding Plan key stays
        // on the *-coding-plan ID above.
        "zhipuai" => "https://open.bigmodel.cn/api/biz/account/query-customer-account-report",
        "zai" => "https://api.z.ai/api/biz/account/query-customer-account-report",
        "kimi-code-plan-cn" => "https://api.kimi.com/coding/v1/usages",
        "kimi-code-plan-global" => "https://api.kimi.ai/coding/v1/usages",
        "commandcode" => "https://api.commandcode.ai/alpha/billing/credits",
        "ollama-cloud" => "https://ollama.com/api/balance",
        "openrouter" => "https://openrouter.ai/api/v1/credits",
        "siliconflow" => "https://api.siliconflow.com/v1/user/info",
        "siliconflow-cn" => "https://api.siliconflow.cn/v1/user/info",
        "moonshotai" => "https://api.moonshot.ai/v1/users/me/balance",
        "moonshotai-cn" => "https://api.moonshot.cn/v1/users/me/balance",
        "opencode-go" => "https://opencode.ai/zen/go/v1/usage",
        "minimax-coding-plan" => "https://api.minimax.io/v1/token_plan/remains",
        "minimax-cn-coding-plan" => "https://api.minimaxi.com/v1/token_plan/remains",
        // Base MiniMax IDs share the Token Plan query; a pay-as-you-go key
        // answers without windows and becomes NoData, never a zero card.
        "minimax" => "https://api.minimax.io/v1/token_plan/remains",
        "minimax-cn" => "https://api.minimax.cn/v1/token_plan/remains",
        "deepseek" => "https://api.deepseek.com/user/balance",
        "stepfun" | "stepfun-step-plan" => "https://api.stepfun.com/v1/accounts",
        "stepfun-ai" | "stepfun-ai-step-plan" => "https://api.stepfun.ai/v1/accounts",
        _ => return Err(QuotaStatus::Unsupported),
    }
    .into();
    if matches!(provider, "zhipuai-coding-plan" | "zai-coding-plan") {
        req.headers[0] = format!("Authorization: {token}");
    }
    // Also protect injected transports, not only curl's config parser.
    if req
        .headers
        .iter()
        .any(|h| h.chars().any(|c| c.is_control() || matches!(c, '"' | '\\')))
    {
        return Err(QuotaStatus::NeedsAuth);
    }
    Ok(req)
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
    // Keep vendor decimal precision; do not parse money through a float.
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
    if let Some(s) = v.as_str() {
        if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
            return Some(t.timestamp());
        }
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
fn add_count(w: &mut QuotaWindow, limit: &Value, used: &Value, remaining: &Value) {
    if let Some(total) = number(limit).filter(|n| *n > 0.0) {
        if let Some(left) = number(remaining).or_else(|| number(used).map(|n| total - n)) {
            w.used_percent = Some(((total - left) / total * 100.0).clamp(0.0, 100.0));
            w.remaining = Some(format!("{}", left.max(0.0)));
            w.limit = money(limit);
        }
    }
}

fn parse(provider: &str, body: &Value) -> ProviderQuota {
    let mut q = ProviderQuota::empty(QuotaStatus::Ready);
    match provider {
        "commandcode" => parse_commandcode(body, &mut q),
        "ollama-cloud" => parse_ollama(body, &mut q),
        "opencode-go" => parse_opencode_go(body, &mut q),
        "openrouter" => parse_dollar_balance(body, &mut q, "USD"),
        "siliconflow" => parse_total_balance(body, &mut q, "USD"),
        "siliconflow-cn" => parse_total_balance(body, &mut q, "CNY"),
        "moonshotai" => parse_available_balance(body, &mut q, "USD"),
        "moonshotai-cn" => parse_available_balance(body, &mut q, "CNY"),
        "zhipuai" => parse_zhipu_balance(body, &mut q, "CNY"),
        "zai" => parse_zhipu_balance(body, &mut q, "USD"),
        "minimax" | "minimax-cn" | "minimax-coding-plan" | "minimax-cn-coding-plan" => {
            parse_minimax(body, &mut q)
        }
        "google" => {
            for v in body["buckets"].as_array().into_iter().flatten() {
                let (Some(model), Some(left)) = (
                    v["modelId"].as_str(),
                    number(&v["remainingFraction"]).filter(|n| (0.0..=1.0).contains(n)),
                ) else {
                    continue;
                };
                let id = format!("{model}:{}", v["tokenType"].as_str().unwrap_or_default());
                let mut w = window(
                    &id,
                    QuotaWindowKind::Model,
                    Some((1.0 - left) * 100.0),
                    timestamp(&v["resetTime"]),
                    None,
                );
                w.model = Some(model.into());
                w.remaining = money(&v["remainingAmount"]);
                q.windows.push(w);
            }
        }
        "github-copilot" => parse_copilot(body, &mut q),
        "zhipuai-coding-plan" | "zai-coding-plan" => {
            if body["success"].as_bool() == Some(false) {
                return ProviderQuota::empty(QuotaStatus::NoData);
            }
            q.plan = body["data"]["level"].as_str().map(str::to_owned);
            for (i, v) in body["data"]["limits"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                let Some(used) = percent(&v["percentage"]) else {
                    continue;
                };
                let (kind, span) = if v["type"].as_str() == Some("TIME_LIMIT") {
                    (QuotaWindowKind::ToolCalls, None)
                } else if matches!(v["type"].as_str(), Some("TOKENS_LIMIT" | "CREDIT_LIMIT"))
                    && v["unit"].as_u64() == Some(6)
                {
                    (QuotaWindowKind::Weekly, Some(604800))
                } else if matches!(v["type"].as_str(), Some("TOKENS_LIMIT" | "CREDIT_LIMIT"))
                    && v["unit"].as_u64() == Some(3)
                {
                    (
                        QuotaWindowKind::Rolling,
                        Some(
                            v["number"]
                                .as_u64()
                                .unwrap_or(5)
                                .max(1)
                                .saturating_mul(3600),
                        ),
                    )
                } else {
                    continue;
                };
                q.windows.push(window(
                    &format!("limit-{i}"),
                    kind,
                    Some(used),
                    timestamp(&v["nextResetTime"]),
                    span,
                ));
            }
        }
        "kimi-code-plan-cn" | "kimi-code-plan-global" => parse_kimi(body, &mut q),
        "deepseek" => {
            for v in body["balance_infos"].as_array().into_iter().flatten() {
                let (Some(unit), Some(remaining)) =
                    (v["currency"].as_str(), money(&v["total_balance"]))
                else {
                    continue;
                };
                if matches!(unit, "CNY" | "USD") {
                    q.balances.push(QuotaBalance {
                        unit: unit.into(),
                        remaining,
                        unlimited: false,
                    });
                }
            }
        }
        "stepfun" | "stepfun-ai" | "stepfun-ai-step-plan" | "stepfun-step-plan" => {
            if body["object"].as_str() == Some("account") {
                if let Some(remaining) = money(&body["balance"]) {
                    q.balances.push(QuotaBalance {
                        unit: String::new(),
                        remaining,
                        unlimited: false,
                    });
                }
            }
        }
        _ => return ProviderQuota::empty(QuotaStatus::Unsupported),
    }
    if q.status == QuotaStatus::Ready && q.windows.is_empty() && q.balances.is_empty() {
        q.status = QuotaStatus::NoData;
    }
    q
}

/// Magpie's money without the sign: two decimals, as the vendors print them.
fn format_amount(v: f64) -> String {
    format!("{v:.2}")
}

/// Command Code's credits reply: the plan's 5-hour and weekly windows plus
/// the dollars left on the account. Shapes from Magpie's commandcode_plan.go
/// (MIT): `used/cap` counters, `resetAt` in seconds or milliseconds, and the
/// CLI's own plan-name table.
fn parse_commandcode(body: &Value, q: &mut ProviderQuota) {
    const PLANS: &[(&str, &str)] = &[
        ("individual-provider", "Provider"),
        ("individual-pro-v1", "Pro"),
        ("individual-goat", "GOAT"),
        ("individual-ultra", "Ultra"),
        ("individual-max", "Max"),
        ("individual-pro", "Pro"),
        ("individual-go", "Go"),
        ("teams-pro", "Teams Pro"),
    ];
    let credits = &body["credits"];
    q.plan = credits["planId"].as_str().map(|id| {
        let normalized = id.to_lowercase().replace('_', "-");
        PLANS
            .iter()
            .find(|(prefix, _)| normalized.starts_with(prefix))
            .map(|(_, name)| (*name).to_owned())
            .unwrap_or_else(|| id.to_owned())
    });
    let limits = &body["windowLimits"];
    for (key, id, kind, span) in [
        (
            "fiveHour",
            "fiveHour",
            QuotaWindowKind::Rolling,
            Some(18000),
        ),
        ("weekly", "weekly", QuotaWindowKind::Weekly, Some(604800)),
    ] {
        let w = &limits[key];
        if let (Some(used), Some(cap)) = (number(&w["used"]), number(&w["cap"]))
            && cap > 0.0
        {
            q.windows.push(window(
                id,
                kind,
                Some((used / cap * 100.0).clamp(0.0, 100.0)),
                timestamp(&w["resetAt"]),
                span,
            ));
        }
    }
    let mut left = 0.0;
    let mut any = false;
    for key in ["monthlyCredits", "purchasedCredits", "freeCredits"] {
        if let Some(v) = number(&credits[key]) {
            left += v.max(0.0);
            any = true;
        }
    }
    if any {
        q.balances.push(QuotaBalance {
            unit: "USD".into(),
            remaining: format_amount(left),
            unlimited: false,
        });
    }
}

/// Ollama Cloud's balance reply: the plan's included allowance (dollars or
/// legacy session/weekly limits) plus purchased credits. Shapes from Magpie's
/// ollama_usage.go (MIT).
fn parse_ollama(body: &Value, q: &mut ProviderQuota) {
    let included = &body["included"];
    for (key, id, kind, span) in [
        ("session", "session", QuotaWindowKind::Rolling, None),
        ("weekly", "weekly", QuotaWindowKind::Weekly, Some(604800)),
    ] {
        let limit = &included[key];
        if let Some(left) = number(&limit["remaining_percent"]) {
            q.windows.push(window(
                id,
                kind,
                Some((100.0 - left).clamp(0.0, 100.0)),
                timestamp(&limit["resets_at"]),
                span,
            ));
        }
    }
    if let (Some(allowance), Some(left)) = (
        number(&included["allowance_usd"]),
        number(&included["balance_usd"]),
    ) && allowance > 0.0
    {
        let used = (allowance - left).clamp(0.0, allowance) / allowance * 100.0;
        let mut w = window(
            "allowance",
            QuotaWindowKind::Rolling,
            Some(used),
            timestamp(&included["period"]["until"]),
            None,
        );
        if let (Some(from), Some(until)) = (
            timestamp(&included["period"]["from"]),
            timestamp(&included["period"]["until"]),
        ) && until > from
        {
            w.span_seconds = Some((until - from).max(0) as u64);
            let days = (until - from) as f64 / 86400.0;
            if (27.0..=32.0).contains(&days) {
                w.id = "month".into();
                w.kind = QuotaWindowKind::Monthly;
            }
        }
        w.remaining = Some(format_amount(left.max(0.0)));
        w.limit = Some(format_amount(allowance));
        q.windows.push(w);
    }
    if let Some(purchased) = number(&body["purchased"]["balance_usd"]) {
        q.balances.push(QuotaBalance {
            unit: "USD".into(),
            remaining: format_amount(purchased),
            unlimited: false,
        });
    }
}

/// OpenCode Go's usage reply: rolling, weekly and monthly windows.
/// From Magpie's readOpenCodeGo (MIT). A 0% window names no reset.
fn parse_opencode_go(body: &Value, q: &mut ProviderQuota) {
    for (key, id, kind, span) in [
        ("rolling", "rolling", QuotaWindowKind::Rolling, Some(18000)),
        ("weekly", "weekly", QuotaWindowKind::Weekly, Some(604800)),
        ("monthly", "monthly", QuotaWindowKind::Monthly, None),
    ] {
        let w = &body["usage"][key];
        // A 0% window names a reset nothing happens at; Magpie leaves it
        // out rather than showing an idle gauge.
        if let Some(used) = percent(&w["percent"]).filter(|p| *p > 0.0) {
            q.windows.push(window(
                id,
                kind,
                Some(used),
                timestamp(&w["resetsAt"]),
                span,
            ));
        }
    }
}

/// OpenRouter's credits reply: what is left of the dollars bought.
/// From Magpie's readOpenRouter (MIT).
fn parse_dollar_balance(body: &Value, q: &mut ProviderQuota, unit: &str) {
    if let (Some(credits), Some(used)) = (
        number(&body["data"]["total_credits"]),
        number(&body["data"]["total_usage"]),
    ) {
        q.balances.push(QuotaBalance {
            unit: unit.into(),
            remaining: format_amount(credits - used),
            unlimited: false,
        });
    }
}

/// SiliconFlow's user-info reply. From Magpie's readSiliconFlow (MIT).
fn parse_total_balance(body: &Value, q: &mut ProviderQuota, unit: &str) {
    if let Some(total) = number(&body["data"]["totalBalance"]) {
        q.balances.push(QuotaBalance {
            unit: unit.into(),
            remaining: format_amount(total),
            unlimited: false,
        });
    }
}

/// Moonshot's balance reply. From Magpie's readMoonshot (MIT).
fn parse_available_balance(body: &Value, q: &mut ProviderQuota, unit: &str) {
    if let Some(available) = number(&body["data"]["available_balance"]) {
        q.balances.push(QuotaBalance {
            unit: unit.into(),
            remaining: format_amount(available),
            unlimited: false,
        });
    }
}

/// Whether a Zhipu/Z.ai account-report envelope refuses the key
/// (codes 1000-1005: wrong, missing, expired or single-sign-on keys).
/// From Magpie's zhipu_envelope.go (MIT).
fn zhipu_key_refused(body: &Value) -> bool {
    let code = body["code"]
        .as_i64()
        .or_else(|| body["error"]["code"].as_str()?.parse::<i64>().ok());
    code.is_some_and(|code| (1000..=1005).contains(&code))
}

fn zhipu_envelope_ok(body: &Value) -> bool {
    if body["success"].as_bool() == Some(false) {
        return false;
    }
    match body["code"].as_i64() {
        Some(200) | None if body["error"].is_null() => true,
        _ => false,
    }
}

/// Zhipu/Z.ai base account report: the cash left to spend. A refused key is
/// NeedsAuth; any other envelope failure is Unavailable, never a zero.
/// From Magpie's readZhipuBalance (MIT), pay-as-you-go semantics: a zero
/// balance is a real reading on the base endpoint.
fn parse_zhipu_balance(body: &Value, q: &mut ProviderQuota, unit: &str) {
    if !zhipu_envelope_ok(body) {
        q.status = if zhipu_key_refused(body) {
            QuotaStatus::NeedsAuth
        } else {
            QuotaStatus::Unavailable
        };
        return;
    }
    if let Some(balance) = money(&body["data"]["availableBalance"]) {
        q.balances.push(QuotaBalance {
            unit: unit.into(),
            remaining: balance,
            unlimited: false,
        });
    }
}

fn parse_copilot(body: &Value, q: &mut ProviderQuota) {
    q.plan = body["copilot_plan"].as_str().map(str::to_owned);
    let reset = timestamp(&body["quota_reset_date_utc"]).or_else(|| {
        chrono::NaiveDate::parse_from_str(body["quota_reset_date"].as_str()?, "%Y-%m-%d")
            .ok()?
            .and_hms_opt(0, 0, 0)
            .map(|d| d.and_utc().timestamp())
    });
    for (id, kind) in [
        ("chat", QuotaWindowKind::Chat),
        ("completions", QuotaWindowKind::Completions),
        ("premium_interactions", QuotaWindowKind::Premium),
    ] {
        let v = &body["quota_snapshots"][id];
        if !v.is_object() {
            continue;
        }
        let mut w = window(
            id,
            kind,
            None,
            timestamp(&v["quota_reset_at"]).or(reset),
            None,
        );
        if v["unlimited"].as_bool() == Some(true) {
            if id == "premium_interactions" && v["has_quota"].as_bool() == Some(false) {
                w.used_percent = Some(100.0);
            } else {
                w.unlimited = true;
            }
        } else {
            // An explicit zero entitlement means there is no included allowance.
            if number(&v["entitlement"]).is_some_and(|n| n <= 0.0) {
                continue;
            }
            add_count(
                &mut w,
                &v["entitlement"],
                &Value::Null,
                &v["quota_remaining"],
            );
            if w.used_percent.is_none() {
                w.used_percent = percent(&v["percent_remaining"]).map(|n| 100.0 - n);
            }
        }
        if w.unlimited || w.used_percent.is_some() {
            q.windows.push(w);
        }
    }
}

fn parse_minimax(body: &Value, q: &mut ProviderQuota) {
    let payload = body.get("data").filter(|v| v.is_object()).unwrap_or(body);
    let verdict = payload.get("base_resp").or_else(|| body.get("base_resp"));
    if verdict.is_none() {
        q.status = QuotaStatus::Unavailable;
        return;
    }
    if let Some(code) = verdict
        .and_then(|v| v["status_code"].as_i64())
        .filter(|n| *n != 0)
    {
        q.status = match code {
            1004 => QuotaStatus::NeedsAuth,
            _ => QuotaStatus::Unavailable,
        };
        return;
    }
    q.plan = [
        "current_subscribe_title",
        "plan_name",
        "combo_title",
        "current_plan_title",
    ]
    .iter()
    .find_map(|key| payload[*key].as_str().filter(|s| !s.trim().is_empty()))
    .map(str::to_owned);
    for bucket in payload["model_remains"].as_array().into_iter().flatten() {
        let Some(model) = bucket["model_name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        for (id, kind, start, end, left, status) in [
            (
                "rolling",
                QuotaWindowKind::Rolling,
                "start_time",
                "end_time",
                "current_interval_remaining_percent",
                "current_interval_status",
            ),
            (
                "weekly",
                QuotaWindowKind::Weekly,
                "weekly_start_time",
                "weekly_end_time",
                "current_weekly_remaining_percent",
                "current_weekly_status",
            ),
        ] {
            let state = bucket[status].as_i64();
            // Percent-bearing legacy replies may omit status; status 3 is not
            // a metered allowance. Never fabricate a window from an absent value.
            if state == Some(3) {
                continue;
            }
            let used = if state == Some(2) {
                Some(100.0)
            } else {
                number(&bucket[left]).map(|n| (100.0 - n).clamp(0.0, 100.0))
            };
            if used.is_none() {
                continue;
            }
            let reset = timestamp(&bucket[end]);
            let span = timestamp(&bucket[start])
                .zip(reset)
                .and_then(|(a, b)| (b > a).then_some((b - a) as u64))
                .or((kind == QuotaWindowKind::Weekly).then_some(604800));
            let mut w = window(&format!("{model}:{id}"), kind, used, reset, span);
            if !model.eq_ignore_ascii_case("general") {
                w.model = Some(model.into());
            }
            q.windows.push(w);
        }
    }
}

fn kimi_reset(value: &Value) -> Option<i64> {
    ["resetTime", "resetAt", "reset_time", "reset_at"]
        .iter()
        .find_map(|key| {
            timestamp(&value[*key]).or_else(|| {
                let n = number(&value[*key]).filter(|n| *n > 0.0)?;
                Some(if n > 1e11 {
                    (n / 1000.0) as i64
                } else {
                    n as i64
                })
            })
        })
}

fn kimi_counts(id: &str, kind: QuotaWindowKind, span: Option<u64>, value: &Value) -> QuotaWindow {
    let mut w = window(id, kind, None, kimi_reset(value), span);
    let Some(total) = number(&value["limit"]).filter(|n| *n > 0.0) else {
        return w;
    };
    let used =
        number(&value["used"]).or_else(|| number(&value["remaining"]).map(|left| total - left));
    if let Some(used) = used {
        w.used_percent = Some((used / total * 100.0).clamp(0.0, 100.0));
        w.remaining = Some((total - used).max(0.0).to_string());
        w.limit = money(&value["limit"]);
    }
    w
}

fn parse_kimi(body: &Value, q: &mut ProviderQuota) {
    let mut counts = Vec::new();
    for (i, v) in body["limits"].as_array().into_iter().flatten().enumerate() {
        let duration = number(&v["window"]["duration"]).unwrap_or(0.0);
        let unit = v["window"]["timeUnit"].as_str().unwrap_or_default();
        let seconds = if unit.contains("MINUTE") {
            60.0
        } else if unit.contains("HOUR") {
            3600.0
        } else if unit.contains("DAY") {
            86400.0
        } else if unit.contains("WEEK") {
            604800.0
        } else if unit.contains("MONTH") {
            2592000.0
        } else {
            0.0
        };
        let span = (duration * seconds > 0.0).then_some((duration * seconds) as u64);
        let d = if v["detail"].is_object() {
            &v["detail"]
        } else {
            &v["window"]
        };
        let kind = if span == Some(2592000) {
            QuotaWindowKind::Monthly
        } else if span == Some(604800) {
            QuotaWindowKind::Weekly
        } else {
            QuotaWindowKind::Rolling
        };
        let w = kimi_counts(&format!("limit-{i}"), kind, span, d);
        if w.used_percent.is_some() {
            counts.push(w);
        }
    }
    let weekly = kimi_counts(
        "weekly",
        QuotaWindowKind::Weekly,
        Some(604800),
        &body["usage"],
    );
    if weekly.used_percent.is_some() {
        counts.push(weekly);
    }
    let mut shares = Vec::new();
    for (id, camel, kind, span) in [
        ("limit_5h", "limit5h", QuotaWindowKind::Rolling, Some(18000)),
        ("limit_7d", "limit7d", QuotaWindowKind::Weekly, Some(604800)),
        (
            "limit_month_total",
            "limitMonthTotal",
            QuotaWindowKind::Monthly,
            Some(2592000),
        ),
        (
            "limit_month_code",
            "limitMonthCode",
            QuotaWindowKind::Monthly,
            Some(2592000),
        ),
    ] {
        let pools = &body["usages"];
        let pool = pools
            .get(id)
            .filter(|v| v.is_object())
            .unwrap_or(&pools[camel]);
        let Some(ratio) = number(&pool["used_ratio"]).or_else(|| number(&pool["usedRatio"])) else {
            continue;
        };
        if shares.iter().any(|w: &QuotaWindow| w.span_seconds == span) {
            continue;
        }
        let w = window(
            id,
            kind,
            Some((ratio * 100.0).clamp(0.0, 100.0)),
            kimi_reset(pool),
            span,
        );
        shares.push(w);
    }
    if !shares.is_empty() {
        counts.retain(|w| {
            !shares
                .iter()
                .any(|share| share.span_seconds == w.span_seconds)
        });
        counts.extend(shares);
        counts.sort_by_key(|w| w.span_seconds.unwrap_or(u64::MAX));
    }
    if counts.is_empty() {
        q.status = QuotaStatus::Unavailable;
    }
    q.windows.extend(counts);
}

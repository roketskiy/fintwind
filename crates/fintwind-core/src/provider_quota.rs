//! Read-only vendor allowance adapters. All I/O stays on the daemon request thread.
//! OpenCode owns credentials and their refresh. No discovery, inference, or writes.
//! Kimi ratio reconciliation is adapted from CodexBar (278e9056), and MiniMax
//! response handling follows CodexBar and Magpie (8d2a4a7). See
//! docs/provider-quota-upstream-license.txt for their MIT license notices.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use fintwind_protocol::provider_quota::{
    ProviderQuota, QuotaBalance, QuotaStatus, QuotaWindow, QuotaWindowKind, supports_provider_quota,
};

/// Daemon-only transport seam. Deliberately not Debug/Serialize: contains secrets.
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
        // Leave OpenAI's existing handling unchanged as requested. For the other
        // adapters the connection list owns selection, including legacy rows
        // without an active flag. Do not infer order from /api/credential.
        if provider == "openai" {
            return Ok(credentials);
        }
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
}

impl QuotaService {
    pub(crate) fn fetch_from_server(
        &self,
        server: &crate::opencode_session::OpenCodeServer,
        provider: &str,
        force: bool,
    ) -> ProviderQuota {
        self.fetch(
            &ServerSource(server),
            &server.port.to_string(),
            provider,
            force,
        )
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
                .map(|body| parse(provider, &body, now))
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
        "zhipuai-coding-plan" | "zai-coding-plan" => Some("ZHIPU_API_KEY"),
        "kimi-code-plan-cn" | "kimi-code-plan-global" => Some("KIMI_API_KEY"),
        "minimax-coding-plan" | "minimax-cn-coding-plan" => Some("MINIMAX_API_KEY"),
        "stepfun" => Some("STEPFUN_API_KEY"),
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
    let oauth = matches!(provider, "openai" | "google" | "github-copilot");
    if kind != if oauth { "oauth" } else { "key" } {
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
        "openai" => {
            if !matches!(
                value["methodID"].as_str(),
                Some("chatgpt-browser" | "chatgpt-headless")
            ) {
                return Err(QuotaStatus::Unsupported);
            }
            let account = metadata["accountID"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or(QuotaStatus::Unsupported)?;
            req.headers.push(format!("chatgpt-account-id: {account}"));
            "https://chatgpt.com/backend-api/wham/usage"
        }
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
        "kimi-code-plan-cn" => "https://api.kimi.com/coding/v1/usages",
        "kimi-code-plan-global" => "https://api.kimi.ai/coding/v1/usages",
        "minimax-coding-plan" => "https://api.minimax.io/v1/token_plan/remains",
        "minimax-cn-coding-plan" => "https://api.minimaxi.com/v1/token_plan/remains",
        "deepseek" => "https://api.deepseek.com/user/balance",
        "stepfun" => "https://api.stepfun.com/v1/accounts",
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

fn parse(provider: &str, body: &Value, now: i64) -> ProviderQuota {
    let mut q = ProviderQuota::empty(QuotaStatus::Ready);
    match provider {
        "openai" => {
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
            } else if c["has_credits"].as_bool() == Some(true) {
                if let Some(amount) = money(&c["balance"]) {
                    q.balances.push(QuotaBalance {
                        unit: "credits".into(),
                        remaining: amount,
                        unlimited: false,
                    });
                }
            }
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
        "minimax-coding-plan" | "minimax-cn-coding-plan" => parse_minimax(body, &mut q),
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
        "stepfun" => {
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
        let Some(model) = bucket["model_name"].as_str() else {
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
            if state.is_some_and(|state| !matches!(state, 1 | 2)) {
                continue;
            }
            let used = if state == Some(2) {
                Some(100.0)
            } else {
                percent(&bucket[left]).map(|n| 100.0 - n)
            };
            if used.is_none() {
                continue;
            }
            let reset = timestamp(&bucket[end]);
            let span = timestamp(&bucket[start])
                .zip(reset)
                .and_then(|(a, b)| (b > a).then_some((b - a) as u64));
            let mut w = window(&format!("{model}:{id}"), kind, used, reset, span);
            if model != "general" {
                w.model = Some(model.into());
            }
            q.windows.push(w);
        }
    }
}

fn kimi_reset(value: &Value) -> Option<i64> {
    ["resetTime", "resetAt", "reset_time", "reset_at"]
        .iter()
        .find_map(|key| timestamp(&value[*key]))
}

fn kimi_counts(id: &str, kind: QuotaWindowKind, span: Option<u64>, value: &Value) -> QuotaWindow {
    let mut w = window(id, kind, None, kimi_reset(value), span);
    let Some(total) = number(&value["limit"]).filter(|n| *n > 0.0) else {
        return w;
    };
    // CodexBar: used is authoritative if remaining contradicts it. Missing or
    // invalid counters remain unknown rather than becoming a zero-percent gauge.
    let left = number(&value["used"])
        .filter(|n| *n >= 0.0)
        .map(|used| (total - used).max(0.0))
        .or_else(|| number(&value["remaining"]).filter(|n| *n >= 0.0 && *n <= total));
    if let Some(left) = left {
        w.used_percent = Some(((total - left) / total * 100.0).clamp(0.0, 100.0));
        w.remaining = Some(left.to_string());
        w.limit = money(&value["limit"]);
    }
    w
}

fn parse_kimi(body: &Value, q: &mut ProviderQuota) {
    let mut counts = Vec::new();
    for (i, v) in body["limits"].as_array().into_iter().flatten().enumerate() {
        let duration = v["window"]["duration"].as_u64().filter(|n| *n > 0);
        let span = match v["window"]["timeUnit"].as_str() {
            Some("TIME_UNIT_MINUTE") => duration.and_then(|n| n.checked_mul(60)),
            Some("TIME_UNIT_HOUR") => duration.and_then(|n| n.checked_mul(3600)),
            Some("TIME_UNIT_SECOND") => duration,
            Some("TIME_UNIT_DAY") => duration.and_then(|n| n.checked_mul(86400)),
            _ => None,
        };
        let d = if v["detail"].is_object() {
            &v["detail"]
        } else {
            &v["window"]
        };
        let w = kimi_counts(&format!("limit-{i}"), QuotaWindowKind::Rolling, span, d);
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
    for (id, kind, span) in [
        ("limit_5h", QuotaWindowKind::Rolling, Some(18000)),
        ("limit_7d", QuotaWindowKind::Weekly, Some(604800)),
        ("limit_month_total", QuotaWindowKind::Monthly, None),
    ] {
        let pool = &body["usages"][id];
        let Some(ratio) = number(&pool["used_ratio"]).filter(|n| *n >= 0.0) else {
            continue;
        };
        let mut w = window(
            id,
            kind,
            Some(ratio.min(1.0) * 100.0),
            kimi_reset(pool),
            span,
        );
        if kind != QuotaWindowKind::Monthly {
            // Count and ratio clocks differing by <=2 seconds describe one
            // quota. Keep the tighter value only for matching windows/resets;
            // counters from another period must not override a new zero pool.
            for count in counts
                .iter()
                .filter(|c| c.span_seconds == span && c.kind == kind)
            {
                if count
                    .resets_at
                    .zip(w.resets_at)
                    .is_some_and(|(a, b)| a.abs_diff(b) <= 2)
                    && count.used_percent > w.used_percent
                {
                    w.used_percent = count.used_percent;
                    w.resets_at = count.resets_at;
                    w.remaining = count.remaining.clone();
                    w.limit = count.limit.clone();
                }
            }
            counts.retain(|c| c.kind != kind || c.span_seconds != span);
        }
        q.windows.push(w);
    }
    q.windows.extend(counts);
}

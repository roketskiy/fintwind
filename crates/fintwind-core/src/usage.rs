//! Account plan-usage limits for OpenCode Go, read the way CodexBar reads
//! them: OpenCode Go's API key calls `opencode.ai/zen/go/v1/usage`. Payload
//! shapes were verified against live responses, not guessed.
//!
//! Everything in this module blocks on subprocesses and the network and must
//! run on the background executor. The parsed snapshot only travels the wire
//! protocol; the desktop front end retired its plan-usage panel.

use anyhow::{Context as _, anyhow};
use serde_json::Value;

const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";

pub use fintwind_protocol::usage::{
    PlanUsage, PlanWindow, cache_hit_percent, format_percent, format_tokens, reset_label,
};

/// Fetch OpenCode Go's rolling, weekly, and monthly subscription limits.
/// `None` means OpenCode has no Go credential, which is normal for people
/// using Zen or any of OpenCode's many other providers. Blocking.
pub fn fetch_opencode_go_plan_usage() -> anyhow::Result<Option<PlanUsage>> {
    let Some(api_key) = opencode_go_api_key() else {
        return Ok(None);
    };
    let (status, body) = http_get(
        OPENCODE_GO_USAGE_URL,
        &[
            format!("Authorization: Bearer {api_key}"),
            "Accept: application/json".to_owned(),
            "User-Agent: fintwind".to_owned(),
        ],
    )?;
    match status {
        200 => {}
        401 | 403 => {
            return Err(anyhow!(tr!(
                "usage_error.opencode_go_key_rejected",
                status = status
            )));
        }
        429 => return Err(anyhow!(tr!("usage_error.rate_limited"))),
        other => return Err(anyhow!(tr!("usage_error.http_status", status = other))),
    }
    let body: Value = serde_json::from_str(&body).context(tr!("usage_error.invalid_json"))?;
    parse_opencode_go_plan_usage(&body)
        .map(Some)
        .ok_or_else(|| anyhow!(tr!("usage_error.no_rate_limit_windows")))
}

/// Match OpenCode's credential precedence closely enough for its Go provider:
/// `OPENCODE_AUTH_CONTENT` replaces auth.json, a provider entry overrides the
/// catalog environment key, and `OPENCODE_API_KEY` remains the fallback.
fn opencode_go_api_key() -> Option<String> {
    let auth = std::env::var("OPENCODE_AUTH_CONTENT")
        .ok()
        .and_then(|payload| serde_json::from_str::<Value>(&payload).ok())
        .or_else(|| {
            let data_home = std::env::var_os("XDG_DATA_HOME")
                .filter(|path| !path.is_empty())
                .map(std::path::PathBuf::from)
                .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))?;
            let payload = std::fs::read_to_string(data_home.join("opencode/auth.json")).ok()?;
            serde_json::from_str(&payload).ok()
        });
    opencode_go_api_key_from_auth(auth.as_ref()).or_else(|| {
        std::env::var("OPENCODE_API_KEY")
            .ok()
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty())
    })
}

fn opencode_go_api_key_from_auth(auth: Option<&Value>) -> Option<String> {
    let entry = auth?.get("opencode-go")?;
    if entry.get("type").and_then(Value::as_str) != Some("api") {
        return None;
    }
    entry
        .get("key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
}

/// Live shape verified on 2026-08-12. Standard Zen has no corresponding
/// `/zen/v1/usage` route, so these rows deliberately represent Go only.
fn parse_opencode_go_plan_usage(body: &Value) -> Option<PlanUsage> {
    let usage = body.get("usage")?;
    let windows = [
        ("rolling", tr!("usage.hour_limit", count = 5)),
        ("weekly", tr!("usage.weekly_limit")),
        ("monthly", tr!("usage.monthly_limit")),
    ]
    .into_iter()
    .filter_map(|(key, label)| {
        let window = usage.get(key)?;
        Some(PlanWindow {
            label,
            percent: window
                .get("percent")
                .and_then(Value::as_f64)?
                .clamp(0.0, 100.0),
            resets_at: window
                .get("resetsAt")
                .and_then(Value::as_str)
                .and_then(|reset| chrono::DateTime::parse_from_rfc3339(reset).ok())
                .map(|date| date.timestamp()),
        })
    })
    .collect::<Vec<_>>();
    if windows.is_empty() {
        return None;
    }
    Some(PlanUsage {
        plan_label: Some("Go".to_owned()),
        windows,
    })
}

/// The request goes through the workspace's one curl helper
/// (`fintwind_protocol::http`); its plain-English failure details are wrapped
/// in the usage page's localized wording here.
fn http_get(url: &str, headers: &[String]) -> anyhow::Result<(u16, String)> {
    fintwind_protocol::http::http_get(url, headers, 15)
        .map_err(|error| anyhow!(tr!("usage_error.curl_failed", error = error.to_string())))
}

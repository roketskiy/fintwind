//! Account plan-usage limits for OpenCode Go, read the way CodexBar reads
//! them: OpenCode Go's API key calls `opencode.ai/zen/go/v1/usage`. Payload
//! shapes were verified against live responses, not guessed.
//!
//! Everything in this module blocks on subprocesses and the network and must
//! run on the background executor. Render reads only the parsed snapshot the
//! app entity stores.

use std::io::Write as _;
use std::process::Stdio;

use anyhow::{Context as _, anyhow};
use serde_json::Value;

const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";

/// The absolute path keeps a shadowed `curl` on `PATH` out of the credential
/// exchange. Windows 10 build 17063 and later ship the same tool in System32.
#[cfg(not(windows))]
const CURL_PATH: &str = "/usr/bin/curl";
#[cfg(windows)]
const CURL_PATH: &str = r"C:\Windows\System32\curl.exe";

pub use fintwind_protocol::usage::{PlanUsage, PlanWindow, format_tokens, reset_label};

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

/// `curl`-based HTTPS GET with the headers passed over curl's own config
/// mechanism, so credentials never appear in a process list. `-D -` prefixes
/// the body with the response headers; [`split_status_and_body`] reads the
/// status line and returns the body.
pub fn http_get(url: &str, headers: &[String]) -> anyhow::Result<(u16, String)> {
    let mut child = crate::command_env::plain_command(CURL_PATH)
        .args(["-sS", "--max-time", "15", "-D", "-", "-K", "-", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context(tr!("usage_error.run_curl"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!(tr!("usage_error.curl_stdin_unavailable")))?;
        for header in headers {
            writeln!(stdin, "header = \"{header}\"").context(tr!("usage_error.configure_curl"))?;
        }
    }
    let output = child
        .wait_with_output()
        .context(tr!("usage_error.curl_did_not_finish"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let error = stderr
            .lines()
            .last()
            .map(str::trim)
            .filter(|error| !error.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| tr!("usage_error.unknown_error"));
        return Err(anyhow!(tr!("usage_error.curl_failed", error = error)));
    }
    split_status_and_body(&String::from_utf8_lossy(&output.stdout))
}

/// `-D -` prefixes the body with the response headers; the status code is on
/// the first line and the body follows the blank separator line.
fn split_status_and_body(raw: &str) -> anyhow::Result<(u16, String)> {
    let status = raw
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow!(tr!("usage_error.curl_no_status")))?;
    let body = raw
        .split_once("\r\n\r\n")
        .or_else(|| raw.split_once("\n\n"))
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    Ok((status, body))
}

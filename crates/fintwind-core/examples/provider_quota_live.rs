//! Read-only diagnostic of the real quota service and HTTP transport for the
//! user's DeepSeek and Zhipu connections. Not a desktop UI E2E test.
//! Uses an already-running OpenCode service; never starts the app/daemon, edits
//! credentials, generates text, or terminates any process. Artifact omits secrets,
//! account IDs, balances, and percentages. Requires explicit binary/output paths.

use std::path::PathBuf;

use anyhow::{Result, bail, ensure};
use fintwind_core::provider_quota::{
    QuotaRequest, QuotaResponse, QuotaService, QuotaSource, credentials_for_connection,
    environment_credential_from_connection,
};
use fintwind_protocol::provider_quota::QuotaStatus;
use serde_json::{Value, json};

struct LiveSource {
    binary: PathBuf,
}

impl LiveSource {
    fn api(&self, path: &str) -> Result<Value> {
        let output = fintwind_core::command_env::output(
            fintwind_core::command_env::plain_command(&self.binary).args(["api", "get", path]),
        )?;
        ensure!(output.status.success(), "OpenCode API read failed");
        // Never include raw CLI output in an error; credential responses contain secrets.
        serde_json::from_slice(&output.stdout)
            .map_err(|_| anyhow::anyhow!("Invalid OpenCode API JSON"))
    }
}

impl QuotaSource for LiveSource {
    fn credentials(&self) -> Result<Value> {
        self.api("/api/credential")
    }
    fn credentials_for(&self, provider: &str) -> Result<Value> {
        let stored = self.credentials()?;
        let integration = self.api(&format!("/api/integration/{provider}"))?;
        Ok(credentials_for_connection(provider, &stored, &integration))
    }
    fn environment_credential(&self, provider: &str) -> Result<Option<Value>> {
        if provider != "deepseek" {
            return Ok(None);
        }
        let integration = self.api("/api/integration/deepseek")?;
        Ok(environment_credential_from_connection(
            provider,
            &integration,
            |name| std::env::var(name).ok(),
        ))
    }
    fn request(&self, request: &QuotaRequest) -> Result<QuotaResponse> {
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

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let binary = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("provide OpenCode binary"))?,
    );
    let artifact = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("provide new artifact path"))?,
    );
    let status = fintwind_core::command_env::output(
        fintwind_core::command_env::plain_command(&binary).args(["service", "status"]),
    )?;
    ensure!(
        status.status.success()
            && String::from_utf8_lossy(&status.stdout)
                .trim()
                .starts_with("http://127.0.0.1:"),
        "An existing local OpenCode service is required"
    );
    // Explicit proxy for vendor requests; loopback CLI discovery stays separate.
    fintwind_protocol::http::set_active_proxy(Some("http://127.0.0.1:7897".into()));
    let source = LiveSource { binary };
    let service = QuotaService::default();
    let models = source.api("/api/model")?;
    let mut readings = Vec::new();
    for provider in ["deepseek", "zhipuai-coding-plan"] {
        let quota = service.fetch(&source, "existing-opencode-service", provider, false);
        let model_count = models["data"].as_array().map(|rows| {
            rows.iter()
                .filter(|v| v["providerID"].as_str() == Some(provider))
                .count()
        });
        readings.push(json!({
            "provider":provider, "status":quota.status, "windows":quota.windows.len(),
            "balances":quota.balances.len(), "measuredAt":quota.measured_at,
            "serverModelCount":model_count,
            "passed":quota.status == QuotaStatus::Ready && match provider {
                "deepseek" => !quota.balances.is_empty(),
                _ => quota.windows.len() >= 2,
            },
        }));
    }
    let passed = readings.iter().all(|r| r["passed"].as_bool() == Some(true));
    let report = json!({"check":"live-provider-quota","desktopUiVerified":false,"passed":passed,"readings":readings});
    use std::io::Write as _;
    let mut output = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(artifact)?;
    output.write_all(serde_json::to_string_pretty(&report)?.as_bytes())?;
    println!("{}", report);
    if !passed {
        bail!("Live quota check failed; see sanitized artifact");
    }
    Ok(())
}

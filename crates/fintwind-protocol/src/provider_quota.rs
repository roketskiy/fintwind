//! Sanitized account readings. Secrets and raw upstream errors never cross this boundary.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    Ready,
    Unsupported,
    NoCredential,
    NeedsAuth,
    AccessDenied,
    RateLimited,
    Unavailable,
    NoData,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderQuota {
    pub status: QuotaStatus,
    /// Vendor-reported plan name, when the endpoint names one.
    pub plan: Option<String>,
    /// Local agent or service the reading belongs to (e.g. "Claude Code",
    /// "Codex"). Only set for machine-local subscription sources.
    #[serde(default)]
    pub service: Option<String>,
    /// Sanitized account label (email or login name). Never a secret.
    #[serde(default)]
    pub account: Option<String>,
    pub windows: Vec<QuotaWindow>,
    pub balances: Vec<QuotaBalance>,
    /// Unix seconds of the successful reading, retained on temporary failure.
    pub measured_at: Option<i64>,
    pub retry_at: Option<i64>,
}

impl ProviderQuota {
    pub fn empty(status: QuotaStatus) -> Self {
        Self {
            status,
            plan: None,
            service: None,
            account: None,
            windows: Vec::new(),
            balances: Vec::new(),
            measured_at: None,
            retry_at: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaWindowKind {
    Rolling,
    Weekly,
    Monthly,
    ToolCalls,
    Chat,
    Completions,
    Premium,
    Model,
    ExtraUsage,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    pub id: String,
    pub kind: QuotaWindowKind,
    /// Only model/bucket names supplied by the vendor; other labels are localized by the UI.
    pub model: Option<String>,
    pub span_seconds: Option<u64>,
    pub used_percent: Option<f64>,
    pub remaining: Option<String>,
    pub limit: Option<String>,
    pub resets_at: Option<i64>,
    pub unlimited: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaBalance {
    /// ISO currency, `credits`, or empty when the endpoint does not specify a unit.
    pub unit: String,
    pub remaining: String,
    pub unlimited: bool,
}

/// Exact catalog IDs, not substring matches against user-supplied endpoints.
/// Local subscription sources (`anthropic`, `openai`) read the machine's own
/// Claude/Codex sign-in on the client host; every other ID resolves its
/// credential from the selected OpenCode environment connection.
pub fn supports_provider_quota(id: &str) -> bool {
    matches!(
        id,
        "anthropic"
            | "openai"
            | "github-copilot"
            | "google"
            | "zhipuai"
            | "zhipuai-coding-plan"
            | "zai"
            | "zai-coding-plan"
            | "kimi-code-plan-cn"
            | "kimi-code-plan-global"
            | "minimax"
            | "minimax-cn"
            | "minimax-coding-plan"
            | "minimax-cn-coding-plan"
            | "commandcode"
            | "ollama-cloud"
            | "openrouter"
            | "siliconflow"
            | "siliconflow-cn"
            | "moonshotai"
            | "moonshotai-cn"
            | "opencode-go"
            | "opencode"
            | "deepseek"
            | "stepfun"
            | "stepfun-ai"
            | "stepfun-ai-step-plan"
            | "stepfun-step-plan"
    )
}

/// IDs answered from the client's own sign-in instead of the
/// selected workspace's OpenCode server. Credentials never cross remote IPC.
pub fn is_local_quota_source(id: &str) -> bool {
    matches!(id, "anthropic" | "openai")
}

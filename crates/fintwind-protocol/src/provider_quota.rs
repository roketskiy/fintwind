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
    pub plan: Option<String>,
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
pub fn supports_provider_quota(id: &str) -> bool {
    matches!(
        id,
        "openai"
            | "github-copilot"
            | "google"
            | "zhipuai-coding-plan"
            | "zai-coding-plan"
            | "kimi-code-plan-cn"
            | "kimi-code-plan-global"
            | "minimax-coding-plan"
            | "minimax-cn-coding-plan"
            | "deepseek"
            | "stepfun"
    )
}

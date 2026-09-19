use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::{AgentTurn, Message, ProviderResumeCursor, TranscriptBlock};

/// Daemon-host native-session operation used when no live driver can act.
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(tag = "provider", rename_all = "camelCase")]
pub enum ProviderSessionForkRequest {
    OpenCode {
        binary: PathBuf,
        cwd: PathBuf,
        session_id: String,
        turn_count: usize,
    },
    /// Rewinds the native conversation with OpenCode's own revert: the
    /// server marks the boundary, restores its snapshot, and keeps the
    /// session id. `turn_count` is the number of native user turns kept.
    OpenCodeRevert {
        binary: PathBuf,
        cwd: PathBuf,
        session_id: String,
        turn_count: usize,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionFork {
    pub cursor: ProviderResumeCursor,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub message_ids: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_resume_at: Option<String>,
}

/// One session on the OpenCode server, as the session list reports it. This
/// is the reconciliation unit between the app's sidebar and sessions created
/// outside the app.
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeSessionSummary {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Unix seconds.
    pub created_at: u64,
    /// Unix seconds — the server's notion of "last touched", which drives
    /// sidebar ordering and transcript freshness checks.
    pub updated_at: u64,
    /// `<providerID>/<modelID>` when the server recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One MCP server's live connection status as the OpenCode server reports
/// it — the state of the process or HTTP connection itself, independent of
/// any session.
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub name: String,
    pub status: McpConnectionState,
    /// Server-side failure detail for `failed` and `needs_auth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionState {
    Connected,
    Pending,
    Disabled,
    Failed,
    NeedsAuth,
}

/// A native session's transcript translated into the app's rendering model:
/// the same shapes a live session persists, so an imported session renders
/// through the ordinary transcript pipeline.
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeTranscript {
    pub messages: Vec<Message>,
    pub blocks: Vec<TranscriptBlock>,
    pub turns: Vec<AgentTurn>,
}

/// One session's cumulative usage, as the OpenCode server's session list
/// reports it. The usage page aggregates these into whatever view it draws,
/// so a range change never re-fetches.
#[derive(Clone, Debug, Deserialize, Serialize, TS, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
    /// When the session was last active, in unix seconds — the stamp its
    /// tokens are bucketed under.
    pub timestamp: u64,
    /// `<providerID>/<modelID>` when the server recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The working directory the session ran in, for the per-project
    /// ranking. Absent on rows the server did not localize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// The server-side cost estimate, when the provider reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl UsageEntry {
    /// Input + output + reasoning + both cache lanes — the full traffic the
    /// provider reported for the session.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.reasoning_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }
}

/// The whole OpenCode store's usage scan: one entry per top-level session,
/// collected in a single pass over the session list. The client aggregates
/// these into whatever view it draws, so a range change never re-fetches.
#[derive(Clone, Debug, Default, Deserialize, Serialize, TS, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    /// Sessions, oldest first.
    pub entries: Vec<UsageEntry>,
}

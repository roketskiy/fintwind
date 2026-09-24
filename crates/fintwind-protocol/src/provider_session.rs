use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{AgentTurn, Message, ProviderResumeCursor, TranscriptBlock};

/// Daemon-host native-session operation used when no live driver can act.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "provider", rename_all = "camelCase")]
pub enum ProviderSessionForkRequest {
    OpenCode {
        binary: PathBuf,
        cwd: PathBuf,
        session_id: String,
        turn_count: usize,
    },
    /// Rewinds the native conversation with OpenCode's own revert: the
    /// server stages a boundary, restores its snapshot, and keeps the
    /// session id. `turn_count` is the number of native user turns kept.
    OpenCodeRevert {
        binary: PathBuf,
        cwd: PathBuf,
        session_id: String,
        turn_count: usize,
    },
    /// Undoes the newest native user message: stages a revert boundary on it,
    /// or moves an existing boundary one user message back. The staged-away
    /// messages stay in storage until the next prompt (which deletes them) or
    /// a redo (which restores them). The daemon resolves the boundary from
    /// the server's own transcript.
    OpenCodeUndoTurn {
        binary: PathBuf,
        cwd: PathBuf,
        session_id: String,
    },
    /// Redoes a previously undone turn: clears the staged revert so the
    /// staged-away turns return to the conversation.
    OpenCodeRedoTurn {
        binary: PathBuf,
        cwd: PathBuf,
        session_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub name: String,
    pub status: McpConnectionState,
    /// Server-side failure detail for `failed` and `needs_auth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One connectable provider integration as the OpenCode server reports it.
/// The server owns the credential store, so its connection list — not any
/// file — is the source of truth for whether a provider is authorized.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationSummary {
    pub id: String,
    pub name: String,
    /// Whether the server offers a plain API-key connect method for this
    /// integration; OAuth- or env-only integrations cannot take a key.
    pub supports_key: bool,
    /// Whether at least one credential is connected for it.
    pub connected: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTranscript {
    pub messages: Vec<Message>,
    pub blocks: Vec<TranscriptBlock>,
    pub turns: Vec<AgentTurn>,
}

/// One session's cumulative usage, as the OpenCode server's session list
/// reports it. The usage page aggregates these into whatever view it draws,
/// so a range change never re-fetches.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
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
    /// The server-side cost estimate, kept exactly as reported. A `Some(0.0)`
    /// means the provider answered "no charge", which is not the same as
    /// answering nothing — the two are told apart by `costed_sessions` on
    /// [`UsageTotals`] rather than by discarding the row here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Sub-agent sessions whose usage this entry absorbed. A sub-agent is its
    /// own session with its own token accounting, and its parent's aggregate
    /// does not include it, so folding it in here is the only way the totals
    /// are honest. Forked sessions are *not* folded in: their history is a
    /// copy of the parent's, and counting it again would double it.
    #[serde(default)]
    pub subagent_sessions: u32,
    /// Tokens contributed by those sub-agents, for the page's subtitle.
    #[serde(default)]
    pub subagent_tokens: u64,
    /// The non-cache part of `subagent_tokens`. The day chart plots non-cache
    /// usage only, so a folded amount needs to be split the same way rather
    /// than pushing its whole total through a channel labelled "excludes
    /// cache".
    #[serde(default)]
    pub subagent_direct: u64,
    /// Per-message day split, present only when the session's activity
    /// provably spans more than one calendar day. `None` means the session
    /// was never refined and every token belongs to `timestamp`'s day, which
    /// is the case for the overwhelming majority of sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<Vec<UsageDayShare>>,
    /// Per-model split, present only for the same refined sessions. A
    /// session's own `model` is whichever model it ended on, so a session
    /// that switched models mid-way would otherwise attribute all of its
    /// usage to the last one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_lanes: Vec<UsageModelLane>,
}

/// One local day's share of a session's usage. `timestamp` is the unix second
/// of a message that landed on that day, so the consumer maps it back through
/// the same local-calendar helper it already uses for whole sessions.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageDayShare {
    pub timestamp: u64,
    /// Input + output + reasoning — what the daily chart encodes.
    pub direct: u64,
    /// Both cache lanes included, for the tooltip's honest total.
    pub total: u64,
}

/// One model's share of a refined session. Only produced when the session's
/// messages were walked, so sessions that never switched models keep the
/// cheaper session-level attribution.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageModelLane {
    /// `<providerID>/<modelID>` as the session's messages recorded it.
    pub model: String,
    pub total: u64,
    /// Sum of the per-message costs, which only exist when the provider
    /// reported them.
    pub cost: f64,
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
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    /// Sessions, oldest first.
    pub entries: Vec<UsageEntry>,
    /// The scan hit its page cap, so the entries cover only the newest portion
    /// of the store. Reported so the page can say so instead of quietly
    /// under-reporting.
    #[serde(default)]
    pub truncated: bool,
    /// How many session rows the scan actually saw.
    #[serde(default)]
    pub sessions_scanned: usize,
}

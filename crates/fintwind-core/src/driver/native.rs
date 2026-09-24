//! OpenCode native session surface outside a live driver runtime.
//!
//! Sessions exist on the OpenCode server whether or not the app created them:
//! the CLI and TUI write into the same store. This module lists a workspace's
//! sessions, translates a native transcript into the app's message/block
//! model (the exact shapes a live session persists, so an imported session
//! renders through the ordinary transcript pipeline), and applies title and
//! deletion edits back to the server.
//!
//! Wire shapes verified against `opencode` 0.0.0-beta-18743:
//! - `GET /api/session?directory=…` → `{data: [session…], cursor}` newest
//!   first, `time.created/updated` in milliseconds;
//! - `GET /api/session/{id}/message` → `{data: [message…], cursor}` newest
//!   first, each message flat. Assistant rows inline their parts under
//!   `content` (`{type: "reasoning"|"tool"|"text"…}`) — the SDK 1.4.6
//!   `{info, parts}` envelope does not match this beta. Tool parts carry the
//!   tool name as `name` (legacy: `tool`) and keep the result text in
//!   `state.content`, with no `state.title`. User rows carry the prompt as a
//!   top-level `text` string with attached files in a `files` array and no
//!   `content` at all.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value;

use fintwind_protocol::model::unix_time;
use fintwind_protocol::provider_session::{
    IntegrationSummary, McpConnectionState, McpServerStatus, NativeSessionSummary,
    NativeTranscript, UsageDayShare, UsageEntry, UsageModelLane, UsageStats,
};

use crate::model::{
    ActivityItem, AgentTurn, Message, MessageRole, ReasoningBlock, TranscriptBlock, TurnStats,
    TurnStatus,
};
use crate::opencode_session::{OpenCodeServer, encode_path_segment, request_json_on_port};

/// Page size for paged lists; matches the live transcript reader.
const PAGE_LIMIT: usize = 200;
/// Page size for the global session listing, whose rows are small.
const SESSION_LIST_LIMIT: usize = 500;
/// Sessions per workspace are bounded in practice; the cap keeps a pathological
/// store from paging forever. At 500 per page this covers 25 000 sessions,
/// and the scan reports when it is hit rather than stopping quietly.
const MAX_SESSION_PAGES: usize = 50;
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
/// How far back a session's activity may reach and still be worth splitting
/// per message. The page's charts read at most 26 weeks, so an older session's
/// day attribution cannot change any mark it draws — only its own total,
/// which the session row already carries.
const DAY_SPLIT_HORIZON_DAYS: u64 = 26 * 7;
/// Refinement cache ceiling. One entry per session seen, so this bounds the
/// daemon's memory for a store far larger than any real one.
const USAGE_CACHE_CEILING: usize = 20_000;
/// Message pages one day split may walk. At 200 messages a page this covers
/// 10 000 messages, past which a session is treated as unattributable rather
/// than allowed to spend the whole scan on itself.
const MAX_MESSAGE_PAGES: usize = 50;

/// List a workspace's native sessions, oldest first, with sub-sessions
/// (fork/compaction children) left out — the sidebar models one linear
/// conversation per row, exactly like the app's own sessions.
pub(crate) fn list_sessions(
    server: &OpenCodeServer,
    directory: &str,
) -> anyhow::Result<Vec<NativeSessionSummary>> {
    let mut summaries = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_SESSION_PAGES {
        let mut path = format!(
            "/api/session?directory={}&limit={}",
            encode_path_segment(directory),
            PAGE_LIMIT
        );
        if let Some(token) = &cursor {
            path.push_str(&format!("&cursor={}", encode_path_segment(token)));
        }
        let response = server.request_with_timeout("GET", &path, None, HTTP_TIMEOUT)?;
        let rows = response
            .pointer("/data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let empty_after_rows = rows.is_empty();
        for row in rows {
            if is_child_session(&row) {
                continue;
            }
            if let Some(summary) = summary_from_row(&row) {
                summaries.push(summary);
            }
        }
        // The next cursor repeats when the list is exhausted; stop then.
        match response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            Some(next) if cursor.as_deref() != Some(next.as_str()) && !empty_after_rows => {
                cursor = Some(next);
            }
            _ => break,
        }
    }
    summaries.reverse();
    Ok(summaries)
}

/// Ask the workspace's server for its MCP servers' live connection statuses.
/// The server owns the MCP connections, so this answers the same whether or
/// not any session exists. Verified against the V2 OpenAPI `mcp.list`
/// response: `{data: [{name, status: {status: "connected"|"pending"|
/// "disabled"|"failed"|"needs_auth", error?}}]}`.
pub(crate) fn list_mcp_statuses(
    server: &OpenCodeServer,
    directory: &str,
) -> anyhow::Result<Vec<McpServerStatus>> {
    let path = format!("/api/mcp?directory={}", encode_path_segment(directory));
    let response = server.request_with_timeout("GET", &path, None, HTTP_TIMEOUT)?;
    let mut statuses = Vec::new();
    for row in response
        .pointer("/data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let Some(name) = row.get("name").and_then(Value::as_str) else {
            continue;
        };
        let state = row
            .pointer("/status/status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let error = row
            .pointer("/status/error")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let status = match state {
            "connected" => McpConnectionState::Connected,
            "pending" => McpConnectionState::Pending,
            "disabled" => McpConnectionState::Disabled,
            "failed" => McpConnectionState::Failed,
            "needs_auth" => McpConnectionState::NeedsAuth,
            other => {
                eprintln!("unknown MCP status {other:?} for server {name:?}");
                continue;
            }
        };
        statuses.push(McpServerStatus {
            name: name.to_owned(),
            status,
            error,
        });
    }
    Ok(statuses)
}

/// What a session row is, in the only distinction that matters for usage.
///
/// `parentID` alone conflates two very different things, and treating them
/// the same either double-counts or drops real spend:
///
/// - a **fork** (`forkSessionID` set) replays the parent's history into a new
///   session. OpenCode zeroes the cloned `step-finish` parts on fork
///   (#31136/#31138) precisely so its token row stays its own post-fork
///   spend, which *is* additive — so a fork counts as its own entry.
/// - a **sub-agent** (`parentID` set, no fork marker) is a separate session
///   with its own token accounting, and its parent's aggregate does *not*
///   include it. Measured on a real store: 103 sub-agent sessions holding
///   128.8M tokens that a `parentID`-only filter discards — 8.6% of the
///   store's total.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionKind {
    TopLevel,
    Fork,
    SubAgent,
}

fn session_kind(row: &Value) -> SessionKind {
    if row
        .get("forkSessionID")
        .or_else(|| row.get("fork_session_id"))
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return SessionKind::Fork;
    }
    if row
        .get("parentID")
        .or_else(|| row.get("parent_id"))
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return SessionKind::SubAgent;
    }
    SessionKind::TopLevel
}

/// The sidebar models one linear conversation per row: forks and sub-agent
/// children are both left out of it.
fn is_child_session(row: &Value) -> bool {
    !matches!(session_kind(row), SessionKind::TopLevel)
}

fn summary_from_row(row: &Value) -> Option<NativeSessionSummary> {
    let session_id = row.get("id").and_then(Value::as_str)?;
    let time = row.get("time")?;
    Some(NativeSessionSummary {
        session_id: session_id.to_owned(),
        title: row
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|title| !title.trim().is_empty()),
        created_at: ms_to_seconds(time.get("created")).unwrap_or_default(),
        updated_at: ms_to_seconds(time.get("updated")).unwrap_or_default(),
        model: row.get("model").and_then(|model| {
            let provider = model.get("providerID").and_then(Value::as_str)?;
            let id = model.get("id").and_then(Value::as_str)?;
            Some(format!("{provider}/{id}"))
        }),
    })
}

fn ms_to_seconds(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .map(|milliseconds| milliseconds / 1_000)
}

/// Fetch and translate one native transcript. Pages arrive newest first and
/// are reversed before translation so turn numbering follows conversation
/// order.
pub(crate) fn fetch_transcript(
    server: &OpenCodeServer,
    session_id: &str,
) -> anyhow::Result<NativeTranscript> {
    fetch_transcript_on_port(server.port, session_id)
}

/// [`fetch_transcript`] against a bare port, for background threads that must
/// not hold a server handle: a handle would delay the pooled server's
/// teardown behind this request's timeout. Session-id routes need no
/// directory.
pub(crate) fn fetch_transcript_on_port(
    port: u16,
    session_id: &str,
) -> anyhow::Result<NativeTranscript> {
    let mut pages: Vec<Vec<Value>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut path = format!(
            "/api/session/{}/message?limit={}",
            encode_path_segment(session_id),
            PAGE_LIMIT
        );
        if let Some(token) = &cursor {
            path.push_str(&format!("&cursor={}", encode_path_segment(token)));
        }
        let response = request_json_on_port(port, "GET", &path, None, HTTP_TIMEOUT)?;
        let rows = response
            .pointer("/data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let exhausted = rows.is_empty();
        match response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            Some(next) if cursor.as_deref() != Some(next.as_str()) && !exhausted => {
                cursor = Some(next);
                pages.push(rows);
            }
            _ => {
                pages.push(rows);
                break;
            }
        }
    }
    let mut rows = pages.into_iter().flatten().collect::<Vec<_>>();
    rows.reverse();
    Ok(translate_rows(&rows))
}

/// Walk the whole OpenCode store's session list in one pass and collect
/// every session's cumulative usage for the usage statistics page, including
/// the sub-agents those sessions spawned. The list itself is global — no
/// `directory` filter — so sessions from every project the CLI, TUI, or any
/// client ever used are covered.
///
/// Pass one walks the session list: the rows already carry tokens, cost,
/// model, and timestamps, so the common case costs one request per page.
/// Pass two is the exception — only the handful of sessions whose activity
/// provably spans more than one calendar day are opened message by message,
/// because a session row's `time.updated` would otherwise dump a week's spend
/// onto its last day.
pub(crate) fn fetch_usage_stats(server: &OpenCodeServer) -> anyhow::Result<UsageStats> {
    let mut stats = UsageStats::default();
    let mut cursor: Option<String> = None;
    let mut rows_by_id: HashMap<String, RowFacts> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for page in 0..MAX_SESSION_PAGES {
        let mut path = format!("/api/session?limit={SESSION_LIST_LIMIT}");
        if let Some(token) = &cursor {
            path.push_str(&format!("&cursor={}", encode_path_segment(token)));
        }
        let response = server.request_with_timeout("GET", &path, None, HTTP_TIMEOUT)?;
        let rows = response
            .pointer("/data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let exhausted = rows.is_empty();
        let has_more = response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .is_some_and(|next| Some(next) != cursor.as_deref());
        stats.sessions_scanned += rows.len();
        for row in &rows {
            let Some((id, facts)) = row_facts(row) else {
                continue;
            };
            seen.insert(id.clone());
            rows_by_id.insert(id, facts);
        }
        if exhausted || !has_more {
            break;
        }
        cursor = response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .map(str::to_owned);
        // Ran out of pages with more still listed. Say so: a silently short
        // scan is indistinguishable from a quiet month.
        if page + 1 == MAX_SESSION_PAGES {
            stats.truncated = true;
            eprintln!(
                "usage scan hit the {MAX_SESSION_PAGES}-page session cap; \
                 statistics cover only the newest portion"
            );
        }
    }

    let mut entries = Vec::with_capacity(rows_by_id.len());
    // Parents before children: a sub-agent folds into its parent's entry, and
    // a HashMap's visit order cannot be relied on to bring them together. The
    // partition is also what makes the result deterministic — the same store
    // has to produce the same totals twice.
    let (subagents, parents): (Vec<_>, Vec<_>) = rows_by_id
        .into_iter()
        .partition(|(_, facts)| facts.kind == SessionKind::SubAgent);
    let mut entry_index: HashMap<String, usize> = HashMap::new();
    for (id, facts) in parents.into_iter().chain(subagents) {
        let Some(mut entry) = usage_entry_from_row(&facts) else {
            continue;
        };
        if facts.kind == SessionKind::SubAgent {
            if let Some(&parent_index) = facts.parent.as_deref().and_then(|p| entry_index.get(p)) {
                fold_subagent_into(&mut entries[parent_index], &entry);
                // A sub-agent is not an entry of its own; the parent's model,
                // directory, and timestamp already describe the work.
                continue;
            }
            // An orphaned sub-agent (its parent was deleted or aged out of the
            // listing) is still real spend. Keep it rather than drop it.
        }
        entry_index.insert(id.clone(), entries.len());
        let split = split_entry_days(server, &id, &entry, facts.created, facts.updated);
        if let Some(split) = split {
            entry.days = Some(split.days);
            entry.model_lanes = split.models;
        }
        entries.push(entry);
    }

    // Retire entries for sessions the listing no longer shows, so the cache
    // tracks the store rather than growing forever.
    usage_day_cache().lock().retain(|id, _| seen.contains(id));

    stats.entries = entries;
    stats.entries.sort_by_key(|entry| entry.timestamp);
    Ok(stats)
}

/// Fold a sub-agent's usage into its parent.
///
/// A sub-agent books its tokens into its own session row and its parent's
/// aggregate does not include them, so the parent's total is short by exactly
/// this amount until the fold. Every lane is added, not just the headline
/// number: folding only `total` would leave the input/output breakdown the
/// KPI card prints disagreeing with the figures above it.
///
/// The cost joins too, and it is merged rather than replaced: a sub-agent's
/// estimate is additive spend, and throwing it away would make the page's
/// "cost" KPI quietly low on exactly the sessions that delegate the most.
fn fold_subagent_into(parent: &mut UsageEntry, child: &UsageEntry) {
    parent.input_tokens = parent.input_tokens.saturating_add(child.input_tokens);
    parent.output_tokens = parent.output_tokens.saturating_add(child.output_tokens);
    parent.reasoning_tokens = parent
        .reasoning_tokens
        .saturating_add(child.reasoning_tokens);
    parent.cache_read_tokens = parent
        .cache_read_tokens
        .saturating_add(child.cache_read_tokens);
    parent.cache_write_tokens = parent
        .cache_write_tokens
        .saturating_add(child.cache_write_tokens);
    parent.cost = match (parent.cost, child.cost) {
        (Some(parent_cost), Some(child_cost)) => Some(parent_cost + child_cost),
        (parent_cost, child_cost) => parent_cost.or(child_cost),
    };
    parent.subagent_sessions = parent.subagent_sessions.saturating_add(1);
    parent.subagent_tokens = parent.subagent_tokens.saturating_add(child.total_tokens());
    parent.subagent_direct = parent.subagent_direct.saturating_add(
        child
            .input_tokens
            .saturating_add(child.output_tokens)
            .saturating_add(child.reasoning_tokens),
    );
}

/// The parts of a session row the scan needs. Only these are kept, so the
/// scan never holds the session list's JSON alive past one page.
struct RowFacts {
    kind: SessionKind,
    /// `parentID` for a sub-agent; `None` otherwise.
    parent: Option<String>,
    /// Unix seconds.
    created: u64,
    updated: u64,
    model: Option<String>,
    directory: Option<String>,
    cost: Option<f64>,
    tokens: [u64; 5],
}

fn row_facts(row: &Value) -> Option<(String, RowFacts)> {
    let id = row.get("id").and_then(Value::as_str)?.to_owned();
    let time = row.get("time")?;
    let created =
        ms_to_seconds(time.get("created")).or_else(|| ms_to_seconds(time.get("updated")))?;
    let updated = ms_to_seconds(time.get("updated")).unwrap_or(created);
    let kind = session_kind(row);
    let parent = (kind == SessionKind::SubAgent)
        .then(|| {
            row.get("parentID")
                .or_else(|| row.get("parent_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .flatten();
    // Kept exactly as reported, zero included: a provider answering "free"
    // is not the same as answering nothing, and the page reports how many
    // sessions carried a cost at all.
    let cost = row.get("cost").and_then(Value::as_f64);
    let empty_tokens = Value::Null;
    let tokens = row.get("tokens").unwrap_or(&empty_tokens);
    let lane = |pointer: &str| tokens.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    Some((
        id,
        RowFacts {
            kind,
            parent,
            created,
            updated,
            model: row.get("model").and_then(|model| {
                let provider = model.get("providerID").and_then(Value::as_str)?;
                let id = model
                    .get("id")
                    .or_else(|| model.get("modelID"))
                    .and_then(Value::as_str)?;
                Some(format!("{provider}/{id}"))
            }),
            directory: row
                .pointer("/location/directory")
                .and_then(Value::as_str)
                .map(str::to_owned),
            cost,
            tokens: [
                lane("/input"),
                lane("/output"),
                lane("/reasoning"),
                lane("/cache/read"),
                lane("/cache/write"),
            ],
        },
    ))
}

/// One session row's contribution to the usage scan. Rows without a usable
/// timestamp fold out: they cannot land on a day, and the totals the page
/// draws are all day-bucketed. A row without `tokens` still counts as a
/// session — it just adds zero tokens.
fn usage_entry_from_row(facts: &RowFacts) -> Option<UsageEntry> {
    // Last activity is when the session's tokens were spent, as far as a
    // day bucket can tell.
    let timestamp = facts.updated.max(facts.created);
    Some(UsageEntry {
        timestamp,
        model: facts.model.clone(),
        directory: facts.directory.clone(),
        cost: facts.cost,
        input_tokens: facts.tokens[0],
        output_tokens: facts.tokens[1],
        reasoning_tokens: facts.tokens[2],
        cache_read_tokens: facts.tokens[3],
        cache_write_tokens: facts.tokens[4],
        subagent_sessions: 0,
        subagent_tokens: 0,
        subagent_direct: 0,
        days: None,
        model_lanes: Vec::new(),
    })
}

/// What one message walk yields for a session: the day split that repairs a
/// multi-day session's timeline, and the model split that repairs a
/// model-switching one's ranking. Both come from the same walk, so neither
/// costs an extra request.
#[derive(Clone)]
struct SessionSplit {
    days: Vec<UsageDayShare>,
    models: Vec<UsageModelLane>,
}

/// A session's per-day split, or `None` when every token already belongs to
/// `entry.timestamp`'s day.
///
/// A session row reports one `time.updated`, so a session that ran from Monday
/// into Wednesday has all of its spend land on Wednesday — a real distortion
/// on a daily chart, and one a user can see. Splitting by message repairs it,
/// but only a small minority of sessions need it: on a real store 486 of 494
/// were opened and last touched on the same day, so the message walk is run
/// only for the rest.
///
/// Two further gates keep the walk narrow. Sessions older than the chart's
/// horizon cannot move any mark the page draws, and sessions with no tokens
/// have nothing to attribute. Everything else is `None`: no request, no
/// payload, no change to the drawing.
fn split_entry_days(
    server: &OpenCodeServer,
    session_id: &str,
    entry: &UsageEntry,
    created: u64,
    updated: u64,
) -> Option<SessionSplit> {
    if entry.total_tokens() == 0 {
        return None;
    }
    // Opened and last touched on one day ⇒ nothing to redistribute.
    if local_day(created) == local_day(updated) {
        return None;
    }
    // Beyond the 26-week heatmap the day split changes no mark, and a session
    // that ended long ago will not gain activity.
    let horizon = unix_time().saturating_sub(DAY_SPLIT_HORIZON_DAYS * 86_400);
    if updated < horizon && created < horizon {
        return None;
    }

    // The fingerprint is last-touched seconds: a session that gained activity
    // re-splits, an untouched one answers from cache without any request.
    let cached = {
        let cache = usage_day_cache().lock();
        cache
            .get(session_id)
            .and_then(|(cached_at, split)| (*cached_at == updated).then(|| split.clone()))
    };
    if let Some(split) = cached {
        return split;
    }

    // A failed walk is deliberately not cached. Caching it would make a
    // transient server error stick for as long as the session stayed
    // untouched, and the page would show a stale single-day bucket with
    // nothing to say why.
    let split = session_day_split(server, session_id)?;
    let mut cache = usage_day_cache().lock();
    if cache.len() >= USAGE_CACHE_CEILING {
        cache.clear();
    }
    cache.insert(session_id.to_owned(), (updated, Some(split.clone())));
    Some(split)
}

/// Walk one session's assistant messages, bucketing their tokens by local day
/// and by model. `None` when the walk fails or yields nothing, which leaves
/// the session on its single-day, single-model bucket — a degraded but still
/// honest answer.
fn session_day_split(server: &OpenCodeServer, session_id: &str) -> Option<SessionSplit> {
    let mut cursor: Option<String> = None;
    let mut days: Vec<UsageDayShare> = Vec::new();
    let mut index_of_day: HashMap<u64, usize> = HashMap::new();
    let mut models: Vec<UsageModelLane> = Vec::new();
    let mut index_of_model: HashMap<String, usize> = HashMap::new();
    for _ in 0..MAX_MESSAGE_PAGES {
        let mut path = format!(
            "/api/session/{}/message?limit={PAGE_LIMIT}",
            encode_path_segment(session_id)
        );
        if let Some(token) = &cursor {
            path.push_str(&format!("&cursor={}", encode_path_segment(token)));
        }
        let response = match server.request_with_timeout("GET", &path, None, HTTP_TIMEOUT) {
            Ok(response) => response,
            // A session that cannot be read still has its session-level total;
            // only the attribution is lost.
            Err(error) => {
                eprintln!("usage day split failed for {session_id}: {error}");
                return None;
            }
        };
        let rows = response
            .pointer("/data")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let has_more = response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .is_some_and(|next| Some(next) != cursor.as_deref());

        for row in rows {
            let Some(usage) = message_usage(row) else {
                continue;
            };
            match index_of_day.get(&usage.timestamp) {
                Some(&index) => {
                    let existing = &mut days[index];
                    existing.direct = existing.direct.saturating_add(usage.direct);
                    existing.total = existing.total.saturating_add(usage.total);
                }
                None => {
                    index_of_day.insert(usage.timestamp, days.len());
                    days.push(UsageDayShare {
                        timestamp: usage.timestamp,
                        direct: usage.direct,
                        total: usage.total,
                    });
                }
            }
            let Some(model) = usage.model else {
                continue;
            };
            match index_of_model.get(&model) {
                Some(&index) => {
                    let existing = &mut models[index];
                    existing.total = existing.total.saturating_add(usage.total);
                    existing.cost += usage.cost;
                }
                None => {
                    index_of_model.insert(model.clone(), models.len());
                    models.push(UsageModelLane {
                        model,
                        total: usage.total,
                        cost: usage.cost,
                    });
                }
            }
        }

        if rows.is_empty() || !has_more {
            break;
        }
        cursor = response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }

    days.sort_by_key(|share| share.timestamp);
    models.sort_by(|a, b| b.total.cmp(&a.total));
    (!days.is_empty()).then_some(SessionSplit { days, models })
}

/// The message body inside a `/session/:id/message` row. Newer servers wrap
/// each message as `{info, parts}`; the beta fintwind's transcript reader was
/// written against answers flat. Accept both.
fn message_body(row: &Value) -> &Value {
    row.get("info").unwrap_or(row)
}

/// One assistant message's tokens, the model that produced them, and its
/// reported cost. User messages carry none, and an aborted turn's all-zero
/// row would otherwise book a day with no spend, so both are filtered here.
struct MessageUsage {
    timestamp: u64,
    direct: u64,
    total: u64,
    model: Option<String>,
    cost: f64,
}

fn message_usage(row: &Value) -> Option<MessageUsage> {
    let body = message_body(row);
    // `type` on the flat shape, `role` inside the info envelope.
    if body
        .get("type")
        .or_else(|| body.get("role"))
        .and_then(Value::as_str)
        .is_some_and(|role| role != "assistant")
    {
        return None;
    }
    let timestamp = ms_to_seconds(body.pointer("/time/created"))?;
    let empty = Value::Null;
    let tokens = body.get("tokens").unwrap_or(&empty);
    let lane = |pointer: &str| tokens.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let (input, output, reasoning) = (lane("/input"), lane("/output"), lane("/reasoning"));
    let (cache_read, cache_write) = (lane("/cache/read"), lane("/cache/write"));
    let total = input
        .saturating_add(output)
        .saturating_add(reasoning)
        .saturating_add(cache_read)
        .saturating_add(cache_write);
    if total == 0 {
        return None;
    }
    Some(MessageUsage {
        timestamp,
        direct: input.saturating_add(output).saturating_add(reasoning),
        total,
        // The message's own model, not the session's: a session that switched
        // models has both in its stream, and the session row names only the
        // model it ended on.
        model: message_model(body),
        cost: body.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

/// `<providerID>/<modelID>` for one message. The two shapes key the id
/// differently — the flat row nests it under `model`, the info envelope may
/// use either spelling — so both are tried before giving up.
fn message_model(body: &Value) -> Option<String> {
    let model = body.get("model")?;
    let provider = model.get("providerID").and_then(Value::as_str)?;
    let id = model
        .get("modelID")
        .or_else(|| model.get("id"))
        .and_then(Value::as_str)?;
    Some(format!("{provider}/{id}"))
}

/// The local calendar day a unix second falls on. Shared with the client so
/// the scan and the aggregation cannot drift apart on DST.
fn local_day(unix_seconds: u64) -> i64 {
    fintwind_protocol::model::local_day(unix_seconds)
}

/// Session id → (last-touched seconds, its split). Keyed by session id
/// alone because OpenCode's store is one store: ids are unique across
/// projects, channels, and builds. Cleared wholesale rather than evicted by
/// age — a full clear is cheaper than the bookkeeping and only costs one
/// re-split of the spanning sessions.
fn usage_day_cache() -> &'static Mutex<HashMap<String, (u64, Option<SessionSplit>)>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, (u64, Option<SessionSplit>)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Rename a native session on the server. The GA endpoint table (verified
/// against v2.0.11 and v2.0.15) has no `/rename` route: retitle through
/// `PATCH /api/session/{id}` with `{"title": …}`, which answers 204. Pre-GA
/// servers answer 404 to that PATCH and only expose rename as its own beta
/// route, so fall back to it there before failing.
pub(crate) fn rename_session(
    server: &OpenCodeServer,
    session_id: &str,
    title: &str,
) -> anyhow::Result<()> {
    let path = format!("/api/session/{}", encode_path_segment(session_id));
    let body = serde_json::json!({"title": title});
    match server.request("PATCH", &path, Some(&body)) {
        Ok(_) => Ok(()),
        Err(patch_error) if is_missing_route(&patch_error) => {
            let legacy = format!("{path}/rename");
            server
                .request("POST", &legacy, Some(&body))
                .map(|_| ())
                .or(Err(patch_error))
        }
        Err(error) => Err(error),
    }
}

/// Whether an OpenCode HTTP failure is a 404 — the GA rename lives on the
/// session route itself, so a pre-GA server reports the PATCH as missing
/// rather than saying anything about the session.
fn is_missing_route(error: &anyhow::Error) -> bool {
    error.to_string().contains("HTTP 404")
}

/// Delete a native session on the server, transcript and all.
pub(crate) fn delete_session(server: &OpenCodeServer, session_id: &str) -> anyhow::Result<()> {
    let path = format!("/api/session/{}", encode_path_segment(session_id));
    server.request("DELETE", &path, None)?;
    Ok(())
}

/// List the server's provider integrations: the connectable roster with
/// each entry's key-method support and live credential connections.
pub(crate) fn list_integrations(
    server: &OpenCodeServer,
) -> anyhow::Result<Vec<IntegrationSummary>> {
    // connect-key does not wait for plugin activation; this list does.
    let response = server.request_with_timeout("GET", "/api/integration", None, HTTP_TIMEOUT)?;
    let rows = integration_rows(&response);
    Ok(rows
        .iter()
        .map(|row| IntegrationSummary {
            id: row
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            name: row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            supports_key: row
                .get("methods")
                .and_then(Value::as_array)
                .is_some_and(|methods| {
                    methods
                        .iter()
                        .any(|method| method.get("type").and_then(Value::as_str) == Some("key"))
                }),
            connected: row
                .get("connections")
                .and_then(Value::as_array)
                .is_some_and(|connections| !connections.is_empty()),
        })
        .collect())
}

/// Store a key for `provider_id` on the server: its connect-key API. The
/// running server adopts the credential immediately and persists it in its
/// own store, so sessions — and the CLI and TUI against this server — can
/// use it without a restart.
pub(crate) fn authorize_integration(
    server: &OpenCodeServer,
    provider_id: &str,
    key: &str,
) -> anyhow::Result<()> {
    // OpenCode's connect-key handler looks the id up immediately and does
    // not wait for plugin activation. GET /api/integration does
    // (`awaitActivation`), so a freshly started server answers /api/info —
    // then 404s connect — until that list has returned once.
    let integrations = list_integrations(server)?;
    if !integrations
        .iter()
        .any(|integration| integration.id == provider_id)
    {
        anyhow::bail!("OpenCode does not provide the integration `{provider_id}`");
    }
    let path = format!(
        "/api/integration/{}/connect/key",
        encode_path_segment(provider_id)
    );
    server.request("POST", &path, Some(&serde_json::json!({ "key": key })))?;
    Ok(())
}

/// Remove `provider_id`'s connected credentials from the server — the
/// logout. The credential ids live in the integration's connection list, so
/// this looks them up there and removes every credential connection; a
/// provider without one is already logged out and answers Ok, keeping the
/// action idempotent.
pub(crate) fn logout_integration(server: &OpenCodeServer, provider_id: &str) -> anyhow::Result<()> {
    let response = server.request("GET", "/api/integration", None)?;
    let credential_ids: Vec<String> = integration_rows(&response)
        .iter()
        .filter(|row| row.get("id").and_then(Value::as_str) == Some(provider_id))
        .flat_map(|row| {
            row.get("connections")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter(|connection| {
                    connection.get("type").and_then(Value::as_str) == Some("credential")
                })
                .filter_map(|connection| connection.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    for credential_id in credential_ids {
        let path = format!("/api/credential/{}", encode_path_segment(&credential_id));
        server.request("DELETE", &path, None)?;
    }
    Ok(())
}

/// Count the models the server currently exposes for `provider_id`, timed.
/// Models appear only when the server holds a credential it accepts, so the
/// count doubles as a connectivity verdict for the authorized provider.
/// The catalog is location-scoped: the request names the workspace so a
/// server shared across directories answers this one's provider list.
pub(crate) fn probe_provider_models(
    server: &OpenCodeServer,
    directory: &str,
    provider_id: &str,
) -> anyhow::Result<(usize, u64)> {
    let started = std::time::Instant::now();
    let response = server.request_for_directory_with_timeout(
        directory,
        "GET",
        "/api/model",
        None,
        HTTP_TIMEOUT,
    )?;
    let models = response
        .pointer("/data")
        .or_else(|| response.pointer("/models"))
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter(|row| row.get("providerID").and_then(Value::as_str) == Some(provider_id))
                .count()
        })
        .unwrap_or_default();
    Ok((models, started.elapsed().as_millis() as u64))
}

/// The integration array inside a `/api/integration` response: under
/// `data` when enveloped, a bare array otherwise.
fn integration_rows(response: &Value) -> &[Value] {
    response
        .pointer("/data")
        .and_then(Value::as_array)
        .or_else(|| response.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Translate the native message rows (oldest first) into the app's model.
fn translate_rows(rows: &[Value]) -> NativeTranscript {
    let mut transcript = NativeTranscript {
        messages: Vec::new(),
        blocks: Vec::new(),
        turns: Vec::new(),
    };

    for row in rows {
        let created_at =
            ms_to_seconds(row.get("time").and_then(|time| time.get("created"))).unwrap_or_default();
        match row.get("type").and_then(Value::as_str) {
            Some("compaction") => {
                // opencode stores a completed compaction as its own message
                // type with a top-level `summary` (and sometimes text parts).
                // The TUI renders it as a Compaction divider; skipping the
                // type dropped every summary from imported transcripts.
                let mut text = row
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if text.trim().is_empty() {
                    text = parts_text(row.get("content"));
                }
                if text.trim().is_empty() {
                    continue;
                }
                transcript.messages.push(Message {
                    id: uuid::Uuid::new_v4(),
                    turn_id: None,
                    role: MessageRole::Compaction,
                    content: text,
                    display_content: None,
                    attachments: Vec::new(),
                    created_at,
                    streaming: false,
                });
            }
            Some("user") => {
                // Verified against 0.0.0-beta-18743: a user row carries its
                // prompt as a top-level `text` string and has no `content`
                // parts at all — unlike assistant rows. Attached files ride
                // in a separate `files` array (inline base64 blobs, no
                // daemon blob reference yet), so nothing to translate there.
                // Parts are still accepted so a shifted shape degrades to a
                // fallback rather than a lost prompt.
                let mut text = row
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if text.trim().is_empty() {
                    text = parts_text(row.get("content"));
                }
                if text.trim().is_empty() {
                    continue;
                }
                let turn = AgentTurn {
                    id: uuid::Uuid::new_v4(),
                    turn_count: transcript.turns.len() + 1,
                    status: TurnStatus::Completed,
                    provider_turn_started: true,
                    provider_resume_at: None,
                    started_at: created_at,
                    completed_at: Some(created_at),
                    checkpoint: None,
                    stats: None,
                };
                transcript.messages.push(Message {
                    id: uuid::Uuid::new_v4(),
                    turn_id: Some(turn.id),
                    role: MessageRole::User,
                    content: text,
                    display_content: None,
                    attachments: Vec::new(),
                    created_at,
                    streaming: false,
                });
                transcript.turns.push(turn);
            }
            Some("assistant") => {
                let parts = row.get("content").and_then(Value::as_array);
                // Every assistant row is one model step of the turn above it,
                // and the fold happens before the content gate: a row with no
                // visible text still ran a model step whose tokens and
                // streaming time belong to the turn's footer statistics.
                if let Some(step) = turn_stats_step(row)
                    && let Some(turn) = transcript.turns.last_mut()
                {
                    let stats = turn.stats.get_or_insert_with(TurnStats::default);
                    if step.model.is_some() {
                        stats.model = step.model;
                    }
                    if step.agent.is_some() {
                        stats.agent = step.agent;
                    }
                    stats.output_tokens = stats.output_tokens.saturating_add(step.output_tokens);
                    stats.stream_ms = stats.stream_ms.saturating_add(step.stream_ms);
                }
                if !assistant_parts_visible(parts) {
                    continue;
                }
                // A settled assistant message carries its completion time;
                // that is when the turn ended.
                let completed_at =
                    ms_to_seconds(row.pointer("/time/completed")).unwrap_or(created_at);
                // The response belongs to the last open turn; an assistant
                // message without a user turn above it (a pre-title or
                // synthetic open) models its own turn so block/turn
                // references stay consistent.
                let turn_id = match transcript.turns.last_mut() {
                    Some(turn) => {
                        turn.completed_at = Some(completed_at.max(turn.completed_at.unwrap_or(0)));
                        turn.id
                    }
                    None => {
                        let turn = AgentTurn {
                            id: uuid::Uuid::new_v4(),
                            turn_count: transcript.turns.len() + 1,
                            status: TurnStatus::Completed,
                            provider_turn_started: true,
                            provider_resume_at: None,
                            started_at: created_at,
                            completed_at: Some(completed_at),
                            checkpoint: None,
                            stats: None,
                        };
                        let id = turn.id;
                        transcript.turns.push(turn);
                        id
                    }
                };
                // Parts stay in stored order. Text that a tool sits between is
                // two messages with the tool block between them; consecutive
                // text parts with nothing between them still join. Hoisting
                // every tool above every text part is what made a restart hide
                // a live split instead of reproducing the part order.
                append_assistant_parts(&mut transcript, parts, turn_id, created_at);
            }
            _ => {}
        }
    }

    transcript
}

/// One assistant row's contribution to its turn's footer statistics. The
/// stored rows — unlike the live step events, which carry no times — keep the
/// `time.streamed`/`time.created` pair the TUI's own footer divides by, so
/// the import path can reproduce the exact numbers a live turn accumulates.
/// A row that reports nothing measurable folds in as `None`.
fn turn_stats_step(row: &Value) -> Option<TurnStats> {
    let model = row.get("model").and_then(|model| {
        let provider = model.get("providerID").and_then(Value::as_str)?;
        let id = model.get("id").and_then(Value::as_str)?;
        Some(format!("{provider}/{id}"))
    });
    // A blank agent id is a degraded row, not a name to titlecase: normalizing
    // it away keeps the footer from rendering an empty segment.
    let agent = row
        .get("agent")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|agent| !agent.trim().is_empty());
    let output = row
        .pointer("/tokens/output")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // The TUI's footer numerator is output + reasoning — the tokens the
    // provider actually produced — so the import path reproduces that exact
    // sum rather than the text alone.
    let reasoning = row
        .pointer("/tokens/reasoning")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = output.saturating_add(reasoning);
    let stream_ms = match (
        row.pointer("/time/streamed").and_then(Value::as_u64),
        row.pointer("/time/created").and_then(Value::as_u64),
    ) {
        (Some(streamed), Some(created)) => streamed.saturating_sub(created),
        _ => 0,
    };
    (model.is_some() || agent.is_some() || output > 0 || stream_ms > 0).then_some(TurnStats {
        model,
        agent,
        output_tokens: output,
        stream_ms,
    })
}

fn assistant_parts_visible(parts: Option<&Vec<Value>>) -> bool {
    parts
        .into_iter()
        .flatten()
        .any(|part| match part.get("type").and_then(Value::as_str) {
            Some("tool") => true,
            Some("reasoning") => reasoning_item(part).is_some(),
            Some("text") => part
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty()),
            _ => false,
        })
}

/// Emit one assistant row's parts in stored order. Activity runs become one
/// block at the message count where they occur; adjacent text parts join with
/// a blank line, but a tool or thought between them keeps the texts apart.
fn append_assistant_parts(
    transcript: &mut NativeTranscript,
    parts: Option<&Vec<Value>>,
    turn_id: uuid::Uuid,
    created_at: u64,
) {
    let mut pending_text: Vec<&str> = Vec::new();
    let mut pending_activities: Vec<ActivityItem> = Vec::new();
    for part in parts.into_iter().flatten() {
        match part.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                // An empty reasoning part is not an activity. Flushing the
                // text before knowing that would split two text parts that
                // should stay one message.
                if let Some(item) = reasoning_item(part) {
                    flush_assistant_text(transcript, &mut pending_text, turn_id, created_at);
                    pending_activities.push(item);
                }
            }
            Some("tool") => {
                flush_assistant_text(transcript, &mut pending_text, turn_id, created_at);
                pending_activities.push(tool_item(part));
            }
            Some("text") => {
                let Some(text) = part.get("text").and_then(Value::as_str) else {
                    continue;
                };
                if text.trim().is_empty() {
                    continue;
                }
                flush_assistant_activities(transcript, &mut pending_activities, turn_id);
                pending_text.push(text);
            }
            _ => {}
        }
    }
    flush_assistant_activities(transcript, &mut pending_activities, turn_id);
    flush_assistant_text(transcript, &mut pending_text, turn_id, created_at);
}

fn flush_assistant_text(
    transcript: &mut NativeTranscript,
    pending_text: &mut Vec<&str>,
    turn_id: uuid::Uuid,
    created_at: u64,
) {
    if pending_text.is_empty() {
        return;
    }
    let text = pending_text.join("\n\n");
    pending_text.clear();
    if text.trim().is_empty() {
        return;
    }
    transcript.messages.push(Message {
        id: uuid::Uuid::new_v4(),
        turn_id: Some(turn_id),
        role: MessageRole::Assistant,
        content: text,
        display_content: None,
        attachments: Vec::new(),
        created_at,
        streaming: false,
    });
}

fn flush_assistant_activities(
    transcript: &mut NativeTranscript,
    pending: &mut Vec<ActivityItem>,
    turn_id: uuid::Uuid,
) {
    if pending.is_empty() {
        return;
    }
    let activities = std::mem::take(pending);
    let after_message = transcript.messages.len();
    // Consecutive activities at the same position merge into one block — the
    // same shape `push_transcript_activity` produces for a live stream.
    match transcript.blocks.last_mut() {
        Some(block) if block.after_message == after_message && block.turn_id == Some(turn_id) => {
            block.activities.extend(activities);
        }
        _ => transcript.blocks.push(TranscriptBlock {
            after_message,
            turn_id: Some(turn_id),
            activities,
        }),
    }
}

/// Concatenated text of a message's text parts.
fn parts_text(content: Option<&Value>) -> String {
    let Some(parts) = content.and_then(Value::as_array) else {
        return String::new();
    };
    parts
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn reasoning_item(part: &Value) -> Option<ActivityItem> {
    let text = part.get("text").and_then(Value::as_str)?;
    if text.trim().is_empty() {
        return None;
    }
    let time = part.get("time");
    let started = time
        .and_then(|time| time.get("created"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let finished = time
        .and_then(|time| time.get("completed"))
        .and_then(Value::as_u64)
        .unwrap_or(started);
    Some(ActivityItem::from_reasoning(
        ReasoningBlock {
            content: text.to_owned(),
            started_at_ms: started,
            finished_at_ms: finished,
        },
        true,
    ))
}

fn tool_item(part: &Value) -> ActivityItem {
    // The tool state carries what the live events deliver, so the same
    // normalization applies: kind from the tool name, display target and
    // output prepared once here instead of per frame.
    //
    // opencode persists `name`/`id` on the part and keeps the result text in
    // `state.content` with no `state.title`; the legacy shape used
    // `tool`/`callID` with `state.output` and a server-authored title.
    // Accept both so a restored transcript keeps the identity a live stream
    // gave the same tool call.
    let state = part.get("state").unwrap_or(part);
    // The provider-call id is what links a restored activity back to live
    // background work (a subagent's card to its child session). opencode
    // keeps it on the part's `id`; the legacy shape used `callID`.
    let source_id = part
        .get("id")
        .or_else(|| part.get("callID"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let name = part
        .get("name")
        .or_else(|| part.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let kind = crate::model::ActivityKind::from_tool_name(name);
    let failed = state.get("status").and_then(Value::as_str) == Some("error");
    let output: Option<Value> = state
        .get("output")
        .filter(|output| !output.is_null())
        .cloned()
        .or_else(|| {
            failed
                .then(|| state.get("error").cloned())
                .flatten()
                .filter(|error| !error.is_null())
        })
        .or_else(|| {
            state
                .get("content")
                .filter(|content| !content.is_null())
                .cloned()
        });
    let mut item = super::activity::tool_activity(
        source_id,
        kind,
        name.to_owned(),
        state.get("input"),
        output.as_ref(),
        state.get("metadata"),
        failed,
        true,
    );
    if item.display_target.is_none()
        && let Some(state_title) = state
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty() && *title != name)
    {
        item.display_target = Some(state_title.to_owned());
    }
    item
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sessions_list_parses_rows_and_skips_children() {
        let server_row = json!({
            "id": "ses_1",
            "title": "迁移会话",
            "time": {"created": 1_788_253_280_101_u64, "updated": 1_788_259_280_101_u64},
            "model": {"id": "deepseek-v4-flash", "providerID": "opencode-go"},
            "location": {"directory": "E:\\work\\x"}
        });
        let child_row = json!({
            "id": "ses_2",
            "parentID": "ses_1",
            "title": "child",
            "time": {"created": 1, "updated": 1}
        });

        let summary = summary_from_row(&server_row).unwrap();
        assert_eq!(summary.session_id, "ses_1");
        assert_eq!(summary.title.as_deref(), Some("迁移会话"));
        assert_eq!(summary.created_at, 1_788_253_280_u64);
        assert_eq!(
            summary.model.as_deref(),
            Some("opencode-go/deepseek-v4-flash")
        );
        // The child carries a parentID; `list_sessions` filters it out.
        assert!(is_child_session(&child_row));
        assert!(!is_child_session(&server_row));
    }

    #[test]
    fn transcript_translation_groups_turns_and_blocks() {
        let rows = vec![
            // The real user shape: a top-level `text` prompt, no `content`.
            json!({
                "id": "msg_1", "type": "user",
                "time": {"created": 1_000_u64},
                "text": "帮我看看日志",
                "agents": [], "files": []
            }),
            json!({
                "id": "msg_2", "type": "assistant",
                "time": {"created": 2_000_u64, "completed": 5_000_u64},
                "model": {"id": "deepseek-v4-flash", "providerID": "opencode-go"},
                "content": [
                    {"type": "reasoning", "text": "先读文件", "time": {"created": 2_100_u64, "completed": 2_500_u64}},
                    {"type": "tool", "tool": "read", "state": {
                        "status": "completed", "input": {"filePath": "log.txt"},
                        "title": "Read log.txt", "output": "line 1\nline 2"
                    }},
                    {"type": "text", "text": "日志显示一切正常。"}
                ]
            }),
            // A parts-shaped user row still translates (fallback).
            json!({
                "id": "msg_3", "type": "user",
                "time": {"created": 6_000_u64},
                "content": [{"type": "text", "text": "那修复它"}]
            }),
            json!({
                "id": "msg_4", "type": "assistant",
                "time": {"created": 7_000_u64, "completed": 9_000_u64},
                "content": [{"type": "text", "text": "已修复。"}]
            }),
            // Non-conversation rows must be ignored.
            json!({"id": "msg_5", "type": "synthetic", "time": {"created": 9_500_u64}, "content": []}),
        ];

        let transcript = translate_rows(&rows);
        // Two user prompts, two assistant replies.
        assert_eq!(transcript.messages.len(), 4);
        assert_eq!(transcript.messages[0].role, MessageRole::User);
        assert_eq!(transcript.messages[0].content, "帮我看看日志");
        assert_eq!(transcript.messages[1].role, MessageRole::Assistant);
        assert_eq!(transcript.messages[1].content, "日志显示一切正常。");
        assert_eq!(transcript.messages[1].created_at, 2);
        // One turn per user prompt, in order.
        assert_eq!(transcript.turns.len(), 2);
        assert_eq!(transcript.turns[0].turn_count, 1);
        assert_eq!(transcript.turns[0].status, TurnStatus::Completed);
        assert_eq!(transcript.turns[0].completed_at, Some(5));
        assert_eq!(transcript.turns[1].turn_count, 2);
        // The reasoning + tool pair sit in one merged block after the first
        // user message (before the first assistant text).
        assert_eq!(transcript.blocks.len(), 1);
        assert_eq!(transcript.blocks[0].after_message, 1);
        assert_eq!(transcript.blocks[0].activities.len(), 2);
        assert_eq!(transcript.blocks[0].turn_id, transcript.messages[1].turn_id);
        // Every message references its turn.
        for message in &transcript.messages {
            assert!(
                transcript
                    .turns
                    .iter()
                    .any(|turn| Some(turn.id) == message.turn_id)
            );
        }
    }

    #[test]
    fn transcript_translation_keeps_a_tool_between_the_text_parts_around_it() {
        let rows = vec![
            json!({
                "id": "msg_1", "type": "user",
                "time": {"created": 1_000_u64},
                "text": "看一下"
            }),
            json!({
                "id": "msg_2", "type": "assistant",
                "time": {"created": 2_000_u64, "completed": 5_000_u64},
                "content": [
                    {"type": "text", "text": "了解"},
                    {"type": "text", "text": ""},
                    {"type": "tool", "tool": "read", "state": {
                        "status": "completed", "input": {"filePath": "composer.rs"},
                        "title": "Read composer.rs", "output": "fn main"
                    }},
                    {"type": "text", "text": "结构。"},
                    {"type": "reasoning", "text": "  "},
                    {"type": "text", "text": "下一句。"}
                ]
            }),
        ];

        let transcript = translate_rows(&rows);
        assert_eq!(transcript.messages.len(), 3);
        assert_eq!(transcript.messages[1].content, "了解");
        assert_eq!(transcript.messages[2].content, "结构。\n\n下一句。");
        assert_eq!(transcript.blocks.len(), 1);
        assert_eq!(transcript.blocks[0].after_message, 2);
        assert_eq!(transcript.blocks[0].activities.len(), 1);
        assert_eq!(transcript.blocks[0].activities[0].title, "read");
    }

    /// Assistant rows keep the tokens, model, and streaming time the live
    /// step events lack, so the import path can fold them into the same
    /// footer statistics a live turn accumulates — summed across a turn's
    /// steps, with the final step's model and agent winning.
    #[test]
    fn transcript_translation_folds_turn_statistics() {
        let rows = vec![
            json!({
                "id": "msg_1", "type": "user",
                "time": {"created": 1_000_u64},
                "text": "跑一下测试", "agents": [], "files": []
            }),
            // Tool step of the same turn with no visible content at all: it
            // is dropped from the transcript but its output tokens and
            // payload-sourced streaming duration still fold into the turn's
            // statistics. Its model/agent lead until the final step
            // overwrites them.
            json!({
                "id": "msg_2", "type": "assistant",
                "time": {"created": 2_000_u64, "streamed": 4_600_u64, "completed": 5_000_u64},
                "agent": "build",
                "model": {"id": "glm-5.3-flash", "providerID": "glmcoding"},
                "tokens": {"input": 100, "output": 12, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "content": []
            }),
            json!({
                "id": "msg_3", "type": "assistant",
                "time": {"created": 6_000_u64, "completed": 9_000_u64},
                "agent": "explore",
                "model": {"id": "glm-5.3", "providerID": "glmcoding"},
                "tokens": {"input": 130, "output": 90, "reasoning": 8, "cache": {"read": 0, "write": 0}},
                "content": [{"type": "text", "text": "全部通过。"}]
            }),
            // A second turn whose only step lacks `time.streamed`: its
            // duration stays out of the denominator, tokens still count, and
            // its blank agent is normalized away rather than folded in.
            json!({
                "id": "msg_4", "type": "user",
                "time": {"created": 20_000_u64},
                "text": "再来一次", "agents": [], "files": []
            }),
            json!({
                "id": "msg_5", "type": "assistant",
                "time": {"created": 21_000_u64, "completed": 22_000_u64},
                "agent": "",
                "model": {"id": "glm-5.3", "providerID": "glmcoding"},
                "tokens": {"input": 50, "output": 3, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "content": [{"type": "text", "text": "完成。"}]
            }),
        ];

        let transcript = translate_rows(&rows);
        assert_eq!(transcript.turns.len(), 2);
        assert_eq!(
            transcript.turns[0].stats,
            Some(TurnStats {
                model: Some("glmcoding/glm-5.3".into()),
                agent: Some("explore".into()),
                // The TUI footer's numerator: output plus reasoning, summed
                // across the steps — 12 + (90 + 8).
                output_tokens: 110,
                stream_ms: 2_600,
            })
        );
        // The second turn's step streamed no measurable time, so its stats
        // carry the tokens and model but no duration to divide by.
        assert_eq!(
            transcript.turns[1].stats,
            Some(TurnStats {
                model: Some("glmcoding/glm-5.3".into()),
                agent: None,
                output_tokens: 3,
                stream_ms: 0,
            })
        );
    }

    /// The exact row shape `0.0.0-beta-18743` serves for user turns: the
    /// prompt is a top-level `text` string, attachments live in `files`, and
    /// there is no `content` array. A translator that only reads `content`
    /// parts drops every prompt from imported transcripts.
    #[test]
    fn user_prompts_translate_from_the_live_row_shape() {
        let rows = vec![
            json!({
                "id": "msg_1", "type": "user",
                "time": {"created": 1_000_u64},
                "text": "这个目前只能应用在mac和Linux，适配windows的困难程度如何？",
                "agents": [], "files": []
            }),
            json!({
                "id": "msg_2", "type": "assistant",
                "time": {"created": 2_000_u64, "completed": 3_000_u64},
                "content": [{"type": "text", "text": "困难不大。"}]
            }),
        ];
        let transcript = translate_rows(&rows);
        assert_eq!(transcript.messages.len(), 2);
        assert_eq!(transcript.messages[0].role, MessageRole::User);
        assert_eq!(
            transcript.messages[0].content,
            "这个目前只能应用在mac和Linux，适配windows的困难程度如何？"
        );
        assert_eq!(transcript.messages[0].created_at, 1);
        assert_eq!(transcript.turns.len(), 1);
    }

    #[test]
    fn tool_parts_normalize_kind_output_and_failure() {
        let part = json!({
            "type": "tool", "tool": "bash", "callID": "call_1",
            "state": {
                "status": "completed",
                "input": {"command": "cargo test", "description": "Run the tests"},
                "title": "Run the tests",
                "output": "test result: ok",
                "metadata": {"exit": 0}
            }
        });
        let item = tool_item(&part);
        assert_eq!(item.kind, crate::model::ActivityKind::Command);
        assert_eq!(item.title, "bash");
        assert_eq!(item.display_target.as_deref(), Some("cargo test"));
        assert_eq!(item.source_id.as_deref(), Some("call_1"));
        assert_eq!(item.output.as_deref(), Some("test result: ok"));
        assert!(!item.failed);
        assert!(item.complete);

        let failed = json!({
            "type": "tool", "tool": "write",
            "state": {"status": "error", "input": {"filePath": "x"}, "error": "permission denied"}
        });
        let item = tool_item(&failed);
        assert!(item.failed);
        assert!(item.complete);
    }

    /// A sub-agent books its tokens into its own row and the parent's
    /// aggregate does not include them, so the fold is the only thing that
    /// makes the parent's total honest. Measured on a real store this was
    /// worth 8.6% of the whole total.
    #[test]
    fn a_subagents_usage_folds_into_its_parent() {
        let mut parent = UsageEntry {
            timestamp: 10,
            model: Some("p/m".to_owned()),
            directory: None,
            cost: Some(0.5),
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 5,
            cache_read_tokens: 1_000,
            cache_write_tokens: 50,
            subagent_sessions: 0,
            subagent_tokens: 0,
            subagent_direct: 0,
            days: None,
            model_lanes: Vec::new(),
        };
        let child = UsageEntry {
            timestamp: 11,
            model: Some("p/m".to_owned()),
            directory: None,
            cost: Some(0.5),
            input_tokens: 7,
            output_tokens: 3,
            reasoning_tokens: 1,
            cache_read_tokens: 40,
            cache_write_tokens: 2,
            subagent_sessions: 0,
            subagent_tokens: 0,
            subagent_direct: 0,
            days: None,
            model_lanes: Vec::new(),
        };

        fold_subagent_into(&mut parent, &child);

        // Every lane moved, so the breakdown agrees with the total.
        assert_eq!(parent.total_tokens(), 1_175 + 53);
        assert_eq!(parent.input_tokens, 107);
        assert_eq!(parent.output_tokens, 23);
        assert_eq!(parent.cache_read_tokens, 1_040);
        assert_eq!(parent.cost, Some(1.0));
        // The subtitle numbers describe the folded-in part only, so the page
        // can say how much of the total came from sub-agents.
        assert_eq!(parent.subagent_sessions, 1);
        assert_eq!(parent.subagent_tokens, 53);
        // Split the same way the day chart splits it: non-cache first, then
        // the whole amount, so the timeline cannot push cache through a
        // channel labelled "excludes cache".
        assert_eq!(parent.subagent_direct, 11);
        // The parent keeps its own timestamp and model: the fold moves tokens,
        // not identity.
        assert_eq!(parent.timestamp, 10);
    }

    /// The session-row shape the global usage scan reduces: five token
    /// lanes, the model pair, the project directory, and `time.updated` as
    /// the preferred stamp.
    #[test]
    fn usage_entry_parses_the_session_row_shape() {
        let row = json!({
            "id": "ses_1", "title": "迁移会话",
            "time": {"created": 1_788_253_280_101_u64, "updated": 1_788_253_300_500_u64},
            "model": {"id": "glm-5.3", "providerID": "glmcoding"},
            "tokens": {"input": 130, "output": 90, "reasoning": 7, "cache": {"read": 1_000, "write": 10}},
            "cost": 1.25,
            "location": {"directory": "E:\\work\\x"}
        });
        let (_, facts) = row_facts(&row).unwrap();
        let entry = usage_entry_from_row(&facts).unwrap();
        assert_eq!(entry.timestamp, 1_788_253_300);
        assert_eq!(entry.model.as_deref(), Some("glmcoding/glm-5.3"));
        assert_eq!(entry.directory.as_deref(), Some("E:\\work\\x"));
        assert_eq!(entry.cost, Some(1.25));
        assert_eq!(entry.input_tokens, 130);
        assert_eq!(entry.output_tokens, 90);
        assert_eq!(entry.reasoning_tokens, 7);
        assert_eq!(entry.cache_read_tokens, 1_000);
        assert_eq!(entry.cache_write_tokens, 10);
        assert_eq!(entry.total_tokens(), 1_237);

        // A reported zero cost stays a reported zero: "the provider chose not
        // to charge" is an answer, and the page counts how many sessions
        // answered at all rather than filtering the zero away here.
        let free = json!({
            "id": "ses_2", "time": {"updated": 2_000_u64},
            "tokens": {"input": 5, "output": 1}, "cost": 0
        });
        let (_, facts) = row_facts(&free).unwrap();
        let entry = usage_entry_from_row(&facts).unwrap();
        assert_eq!(entry.cost, Some(0.0));
        assert_eq!(entry.timestamp, 2);
        assert_eq!(entry.total_tokens(), 6);

        // A session that never reported tokens still counts as a session,
        // just with zero tokens.
        let tokenless = json!({"id": "ses_4", "time": {"updated": 3_000_u64}});
        let (_, facts) = row_facts(&tokenless).unwrap();
        let entry = usage_entry_from_row(&facts).unwrap();
        assert_eq!(entry.timestamp, 3);
        assert_eq!(entry.total_tokens(), 0);

        // A timeless row cannot land on a day and folds out here.
        let timeless = json!({"id": "ses_3", "tokens": {"input": 1}});
        assert!(row_facts(&timeless).is_none());
    }

    /// A fork inherits the parent's history, so its session row carries the
    /// parent's tokens *and* `forkSessionID`; a sub-agent's tokens are its own
    /// and its parent's aggregate does not include them. Reading only
    /// `parentID` conflates the two: discarding a sub-agent as a fork drops
    /// real spend.
    #[test]
    fn session_kind_tells_a_fork_from_a_subagent() {
        let parent = json!({"id": "ses_p", "time": {"updated": 1}});
        let fork = json!({
            "id": "ses_f", "forkSessionID": "ses_p", "time": {"updated": 2}
        });
        let subagent = json!({
            "id": "ses_c", "parentID": "ses_p", "time": {"updated": 3}
        });

        assert_eq!(session_kind(&parent), SessionKind::TopLevel);
        assert_eq!(session_kind(&fork), SessionKind::Fork);
        assert_eq!(session_kind(&subagent), SessionKind::SubAgent);

        // Both are excluded from the sidebar's linear-conversation list.
        assert!(is_child_session(&fork));
        assert!(is_child_session(&subagent));
        assert!(!is_child_session(&parent));

        // Only a sub-agent folds into a parent: a fork's tokens already
        // belong to the parent's history, and folding it would count the same
        // messages twice.
        assert_eq!(session_kind(&subagent), SessionKind::SubAgent);
        assert_ne!(session_kind(&fork), SessionKind::SubAgent);
    }

    #[test]
    fn opencode_tool_parts_keep_their_identity() {
        // Stored shape of the current beta (`session_message` rows): the name
        // lives on the part, the result text rides `state.content`, and no
        // `state.title` exists. Before this was handled, every restored tool
        // row degraded to the generic "工具" label with no target or output.
        let part = json!({
            "type": "tool", "id": "call_01", "name": "read", "executed": true,
            "state": {
                "status": "completed",
                "input": {"path": "src/workspace.rs", "limit": 200},
                "content": [{"type": "text", "text": "Read src/workspace.rs"}],
                "metadata": {"truncated": false}
            },
            "time": {"created": 1_788_962_111_526_u64, "completed": 1_788_962_111_943_u64}
        });
        let item = tool_item(&part);
        assert_eq!(item.kind, crate::model::ActivityKind::FileRead);
        assert_eq!(item.title, "read");
        assert_eq!(item.display_target.as_deref(), Some("src/workspace.rs"));
        // The restored activity keeps the provider call id, which is what
        // re-links it to a live background item (e.g. a subagent's card).
        assert_eq!(item.source_id.as_deref(), Some("call_01"));
        assert_eq!(item.output.as_deref(), Some("Read src/workspace.rs"));
        assert!(!item.failed);

        let patch = json!({
            "type": "tool", "id": "call_02", "name": "patch",
            "state": {
                "status": "error",
                "input": {"patchText": "*** Begin Patch\n*** Update File: a.rs\n@@\n-x\n+y\n*** End Patch"},
                "error": {"message": "patch did not apply"}
            }
        });
        let item = tool_item(&patch);
        assert_eq!(item.kind, crate::model::ActivityKind::FileChange);
        assert_eq!(item.source_id.as_deref(), Some("call_02"));
        assert!(item.failed);
        assert!(!item.file_changes.is_empty());
        assert!(
            item.output
                .as_deref()
                .is_some_and(|output| output.contains("patch did not apply"))
        );

        let execute = json!({
            "type": "tool", "id": "call_ex", "name": "execute",
            "state": {
                "status": "completed",
                "input": {"code": "return await tools.context7.query_docs({ libraryId: '/opencode' })"},
                "content": [{"type": "text", "text": "ok"}],
                "metadata": {
                    "toolCalls": [
                        {"tool": "context7.query_docs", "status": "completed"}
                    ]
                }
            }
        });
        let item = tool_item(&execute);
        assert_eq!(item.kind, crate::model::ActivityKind::Tool);
        assert_eq!(item.title, "execute");
        assert_eq!(item.display_target.as_deref(), Some("context7.query_docs"));

        let titled = json!({
            "type": "tool", "id": "call_js", "name": "Js",
            "state": {
                "status": "completed",
                "title": "Inspect Helium browser",
                "input": {"code": "sky.get_app_state()"},
                "content": [{"type": "text", "text": "ok"}]
            }
        });
        let item = tool_item(&titled);
        assert_eq!(item.title, "Js");
        assert_eq!(item.display_target.as_deref(), Some("sky.get_app_state()"));
    }

    #[test]
    fn empty_reasoning_and_blank_text_are_dropped() {
        let rows = vec![
            json!({
                "id": "msg_1", "type": "user", "time": {"created": 1_000_u64},
                "content": [{"type": "text", "text": "  "}]
            }),
            json!({
                "id": "msg_2", "type": "assistant", "time": {"created": 2_000_u64},
                "content": [{"type": "reasoning", "text": ""}]
            }),
        ];
        let transcript = translate_rows(&rows);
        assert!(transcript.messages.is_empty());
        assert!(transcript.blocks.is_empty());
        assert!(transcript.turns.is_empty());
    }

    #[test]
    fn compaction_rows_become_transcript_dividers() {
        let rows = vec![
            json!({
                "id": "msg_1", "type": "user",
                "time": {"created": 1_000_u64},
                "text": "先设计"
            }),
            json!({
                "id": "msg_2", "type": "assistant",
                "time": {"created": 2_000_u64, "completed": 3_000_u64},
                "content": [{"type": "text", "text": "好。"}]
            }),
            json!({
                "id": "msg_3", "type": "compaction",
                "time": {"created": 4_000_u64},
                "status": "completed",
                "summary": "## Objective\n- Compacted."
            }),
            json!({
                "id": "msg_4", "type": "user",
                "time": {"created": 5_000_u64},
                "text": "继续"
            }),
        ];
        let transcript = translate_rows(&rows);
        assert_eq!(transcript.messages.len(), 4);
        assert_eq!(transcript.messages[2].role, MessageRole::Compaction);
        assert_eq!(transcript.messages[2].content, "## Objective\n- Compacted.");
        assert_eq!(transcript.messages[2].turn_id, None);
        assert_eq!(transcript.turns.len(), 2);
    }
}

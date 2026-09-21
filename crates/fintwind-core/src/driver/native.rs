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

use std::time::Duration;

use serde_json::Value;

use fintwind_protocol::provider_session::{
    IntegrationSummary, McpConnectionState, McpServerStatus, NativeSessionSummary,
    NativeTranscript, UsageEntry, UsageStats,
};

use crate::model::{
    ActivityItem, AgentTurn, Message, MessageRole, ReasoningBlock, TurnStats, TurnStatus,
};
use crate::opencode_session::{OpenCodeServer, encode_path_segment};

/// Page size for paged lists; matches the live transcript reader.
const PAGE_LIMIT: usize = 200;
/// Page size for the global session listing, whose rows are small.
const SESSION_LIST_LIMIT: usize = 500;
/// Sessions per workspace are bounded in practice; the cap keeps a pathological
/// store from paging forever. At 500 per page this covers 12 500 sessions.
const MAX_SESSION_PAGES: usize = 25;
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

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

/// Fork/compaction children model the same conversation as their parent, so
/// the sidebar only lists top-level sessions.
fn is_child_session(row: &Value) -> bool {
    row.get("parentID").and_then(Value::as_str).is_some()
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
        let response = server.request_with_timeout("GET", &path, None, HTTP_TIMEOUT)?;
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
/// every top-level session's cumulative usage for the usage statistics page.
/// The list itself is global — no `directory` filter — so sessions from
/// every project the CLI, TUI, or any client ever used are covered. One
/// request per page of sessions, nothing per session: the rows already
/// carry the tokens, cost, model, and timestamps.
pub(crate) fn fetch_usage_stats(server: &OpenCodeServer) -> anyhow::Result<UsageStats> {
    let mut stats = UsageStats::default();
    let mut cursor: Option<String> = None;
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
        for row in &rows {
            if is_child_session(row) {
                continue;
            }
            if let Some(entry) = usage_entry_from_row(row) {
                stats.entries.push(entry);
            }
        }
        // The next cursor repeats when the list is exhausted; stop then.
        match response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            Some(next) if cursor.as_deref() != Some(next.as_str()) && !exhausted => {
                cursor = Some(next);
            }
            _ => {
                if !exhausted
                    && page + 1 == MAX_SESSION_PAGES
                    && response
                        .pointer("/cursor/next")
                        .and_then(Value::as_str)
                        .is_some()
                {
                    eprintln!(
                        "usage scan hit the {MAX_SESSION_PAGES}-page session cap; \
                         statistics cover only the newest portion"
                    );
                }
                break;
            }
        }
    }
    stats.entries.sort_by_key(|entry| entry.timestamp);
    Ok(stats)
}

/// One session row's contribution to the usage scan. Rows without a usable
/// timestamp fold out: they cannot land on a day, and the totals the page
/// draws are all day-bucketed. A row without `tokens` still counts as a
/// session — it just adds zero tokens.
fn usage_entry_from_row(row: &Value) -> Option<UsageEntry> {
    let time = row.get("time")?;
    // Last activity is when the session's tokens were spent, as far as a
    // day bucket can tell.
    let timestamp =
        ms_to_seconds(time.get("updated")).or_else(|| ms_to_seconds(time.get("created")))?;
    let empty_tokens = Value::Null;
    let tokens = row.get("tokens").unwrap_or(&empty_tokens);
    let lane = |pointer: &str| tokens.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    Some(UsageEntry {
        timestamp,
        model: row.get("model").and_then(|model| {
            let provider = model.get("providerID").and_then(Value::as_str)?;
            let id = model.get("id").and_then(Value::as_str)?;
            Some(format!("{provider}/{id}"))
        }),
        directory: row
            .pointer("/location/directory")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cost: row
            .get("cost")
            .and_then(Value::as_f64)
            .filter(|cost| *cost > 0.0),
        input_tokens: lane("/input"),
        output_tokens: lane("/output"),
        reasoning_tokens: lane("/reasoning"),
        cache_read_tokens: lane("/cache/read"),
        cache_write_tokens: lane("/cache/write"),
    })
}

/// Rename a native session on the server. This beta exposes rename as
/// `POST /api/session/{id}/rename` (a `PATCH /api/session/{id}` answers 404,
/// verified against 0.0.0-beta-18743).
pub(crate) fn rename_session(
    server: &OpenCodeServer,
    session_id: &str,
    title: &str,
) -> anyhow::Result<()> {
    let path = format!("/api/session/{}/rename", encode_path_segment(session_id));
    server.request("POST", &path, Some(&serde_json::json!({"title": title})))?;
    Ok(())
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
    let response =
        server.request_with_timeout("GET", "/api/integration", None, HTTP_TIMEOUT)?;
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
    server.request(
        "POST",
        &path,
        Some(&serde_json::json!({ "key": key })),
    )?;
    Ok(())
}

/// Remove `provider_id`'s connected credentials from the server — the
/// logout. The credential ids live in the integration's connection list, so
/// this looks them up there and removes every credential connection; a
/// provider without one is already logged out and answers Ok, keeping the
/// action idempotent.
pub(crate) fn logout_integration(
    server: &OpenCodeServer,
    provider_id: &str,
) -> anyhow::Result<()> {
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
pub(crate) fn probe_provider_models(
    server: &OpenCodeServer,
    provider_id: &str,
) -> anyhow::Result<(usize, u64)> {
    let started = std::time::Instant::now();
    let response = server.request("GET", "/api/model", None)?;
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
                let content = row.get("content").and_then(Value::as_array);
                // Activity parts render as transcript blocks before the
                // assistant's text, the same order a live stream produces.
                let mut activities: Vec<ActivityItem> = Vec::new();
                for part in content.into_iter().flatten() {
                    match part.get("type").and_then(Value::as_str) {
                        Some("reasoning") => {
                            if let Some(item) = reasoning_item(part) {
                                activities.push(item);
                            }
                        }
                        Some("tool") => {
                            activities.push(tool_item(part));
                        }
                        _ => {}
                    }
                }
                let text = parts_text(row.get("content"));
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
                if activities.is_empty() && text.trim().is_empty() {
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
                if !activities.is_empty() {
                    let after_message = transcript.messages.len();
                    // Consecutive activities at the same position merge into
                    // one block — the same shape `push_transcript_activity`
                    // produces for a live stream.
                    match transcript.blocks.last_mut() {
                        Some(block)
                            if block.after_message == after_message
                                && block.turn_id == Some(turn_id) =>
                        {
                            block.activities.extend(activities);
                        }
                        _ => transcript
                            .blocks
                            .push(fintwind_protocol::model::TranscriptBlock {
                                after_message,
                                turn_id: Some(turn_id),
                                activities,
                            }),
                    }
                }
                if !text.trim().is_empty() {
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
                "tokens": {"input": 130, "output": 90, "reasoning": 0, "cache": {"read": 0, "write": 0}},
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
                output_tokens: 102,
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
        let entry = usage_entry_from_row(&row).unwrap();
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

        // A zero cost folds out to `None`; a row that only stamped
        // `time.created` still lands on it.
        let free = json!({
            "id": "ses_2", "time": {"updated": 2_000_u64},
            "tokens": {"input": 5, "output": 1}, "cost": 0
        });
        let entry = usage_entry_from_row(&free).unwrap();
        assert_eq!(entry.cost, None);
        assert_eq!(entry.timestamp, 2);
        assert_eq!(entry.total_tokens(), 6);

        // A session that never reported tokens still counts as a session,
        // just with zero tokens.
        let tokenless = json!({"id": "ses_4", "time": {"updated": 3_000_u64}});
        let entry = usage_entry_from_row(&tokenless).unwrap();
        assert_eq!(entry.timestamp, 3);
        assert_eq!(entry.total_tokens(), 0);

        // A child session is skipped by the scan itself; a timeless row
        // cannot land on a day and folds out here.
        let timeless = json!({"id": "ses_3", "tokens": {"input": 1}});
        assert!(usage_entry_from_row(&timeless).is_none());
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

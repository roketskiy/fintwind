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
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use serde_json::Value;

use fintwind_protocol::provider_session::{
    IntegrationFormField, IntegrationFormFieldCondition, IntegrationFormFieldOption,
    IntegrationOAuthAttempt, IntegrationOAuthMethod, IntegrationOAuthMode, IntegrationOAuthPoll,
    IntegrationSummary, McpConnectionState, McpServerStatus, NativeSessionSummary,
    NativeTranscript,
};

use fintwind_storage::blob_store::BlobStore;
use crate::model::{
    ActivityItem, AgentTurn, Message, MessageAttachment, MessageRole, ReasoningBlock,
    TranscriptBlock, TurnStats, TurnStatus,
};
use crate::opencode_session::{OpenCodeServer, encode_path_segment, request_json_on_port};

/// Page size for paged lists; matches the live transcript reader.
const PAGE_LIMIT: usize = 200;
/// Sessions per workspace are bounded in practice; the cap keeps a pathological
/// store from paging forever. At 500 per page this covers 25 000 sessions,
/// and the scan reports when it is hit rather than stopping quietly.
const MAX_SESSION_PAGES: usize = 50;
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound the work and disk writes of opening a remote transcript. Attachments
/// beyond the limit still appear as unavailable image tiles.
const MAX_TRANSCRIPT_IMAGE_BYTES: usize = 128 * 1024 * 1024;

/// List a workspace's native sessions, oldest first, with sub-sessions
/// (fork/compaction children) left out — the sidebar models one linear
/// conversation per row, exactly like the app's own sessions.
pub fn list_sessions(
    server: &OpenCodeServer,
    directory: &str,
) -> anyhow::Result<Vec<NativeSessionSummary>> {
    let directory = crate::opencode_session::resolve_request_directory(Path::new(directory));
    let mut summaries = Vec::new();
    let mut cursor: Option<String> = None;
    // Newer servers can exclude child sessions before paging; old servers
    // either ignore the parameter (the client still filters) or reject it.
    let mut server_filters_roots = true;
    for page in 0..MAX_SESSION_PAGES {
        let mut base = format!(
            "/api/session?directory={}&limit={}",
            encode_path_segment(&directory),
            PAGE_LIMIT
        );
        if let Some(token) = &cursor {
            base.push_str(&format!("&cursor={}", encode_path_segment(token)));
        }
        let mut path = base.clone();
        if server_filters_roots {
            path.push_str("&parentID=null");
        }
        let response = match server.request_with_timeout("GET", &path, None, HTTP_TIMEOUT) {
            Err(error) if server_filters_roots && error.to_string().contains("HTTP 400") => {
                server_filters_roots = false;
                server.request_with_timeout("GET", &base, None, HTTP_TIMEOUT)?
            }
            result => result?,
        };
        let rows = response
            .pointer("/data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if rows.len() == PAGE_LIMIT && response.get("cursor").is_none() {
            anyhow::bail!("OpenCode session listing returned a full page without a cursor");
        }
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
                if page + 1 == MAX_SESSION_PAGES {
                    anyhow::bail!("OpenCode session listing exceeded {MAX_SESSION_PAGES} pages");
                }
                cursor = Some(next);
            }
            _ => break,
        }
    }
    summaries.reverse();
    Ok(summaries)
}

/// A directory roster alone cannot distinguish a move from deletion. Only
/// missing tracked IDs need an additional request; a failed lookup must not
/// silently remove local rows. Resolve Git metadata once per distinct location.
pub fn list_sessions_with_tracked(
    server: &OpenCodeServer,
    directory: &str,
    tracked: &[String],
) -> anyhow::Result<Vec<NativeSessionSummary>> {
    let mut summaries = list_sessions(server, directory)?;
    let mut known: HashSet<String> = summaries.iter().map(|row| row.session_id.clone()).collect();
    for id in tracked {
        if !known.insert(id.clone()) {
            continue;
        }
        let path = format!("/api/session/{}", encode_path_segment(id));
        let response = match server.request_with_timeout("GET", &path, None, HTTP_TIMEOUT) {
            Err(error) if error.to_string().contains("HTTP 404") => continue,
            result => result?,
        };
        let row = response.get("data").unwrap_or(&response);
        // Treat malformed success as an error, not evidence of deletion.
        let summary = summary_from_row(row)
            .ok_or_else(|| anyhow::anyhow!("OpenCode returned an invalid session {id}"))?;
        summaries.push(summary);
    }
    let mut workspaces = HashMap::new();
    for summary in &mut summaries {
        if let Some(directory) = summary.directory.as_ref() {
            summary.workspace = Some(
                workspaces
                    .entry(directory.clone())
                    .or_insert_with(|| fintwind_git::worktree::inspect_workspace(directory))
                    .clone(),
            );
        }
    }
    Ok(summaries)
}

/// Ask the workspace's server for its MCP servers' live connection statuses.
/// The server owns the MCP connections, so this answers the same whether or
/// not any session exists. Verified against the V2 OpenAPI `mcp.list`
/// response: `{data: [{name, status: {status: "connected"|"pending"|
/// "disabled"|"failed"|"needs_auth", error?}}]}`.
pub fn list_mcp_statuses(
    server: &OpenCodeServer,
    directory: &str,
) -> anyhow::Result<Vec<McpServerStatus>> {
    let directory = crate::opencode_session::resolve_request_directory(Path::new(directory));
    let path = format!("/api/mcp?directory={}", encode_path_segment(&directory));
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

/// Ask the server to hot-reload in place through its native `location.reload`
/// route: every loaded location is shut down and rebuilt, so edited
/// configuration takes effect without restarting the server process. The
/// route answers 204 once all replacement builds settle; running sessions
/// continue with fresh services at their next step boundary. Reload acts on
/// every loaded location and never boots the caller's location first, so no
/// directory header is sent.
pub fn reload_server(server: &OpenCodeServer) -> anyhow::Result<()> {
    server.request_with_timeout("POST", "/api/location/reload", None, HTTP_TIMEOUT)?;
    Ok(())
}

use fintwind_quota::usage_scan::{is_child_session, ms_to_seconds};

fn summary_from_row(row: &Value) -> Option<NativeSessionSummary> {
    let session_id = row.get("id").and_then(Value::as_str)?;
    let time = row.get("time")?;
    Some(NativeSessionSummary {
        session_id: session_id.to_owned(),
        directory: row
            .pointer("/location/directory")
            .and_then(Value::as_str)
            .filter(|directory| !directory.trim().is_empty())
            .map(std::path::PathBuf::from),
        workspace: None,
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
        task: row
            .pointer("/metadata/task")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|task| !task.is_empty())
            .map(str::to_owned),
    })
}


/// Fetch and translate one native transcript. Pages arrive newest first and
/// are reversed before translation so turn numbering follows conversation
/// order.
pub fn fetch_transcript(
    server: &OpenCodeServer,
    session_id: &str,
    blobs: &BlobStore,
    input_ids: &HashSet<String>,
) -> anyhow::Result<NativeTranscript> {
    // Keep admitted file-only inputs before presentation filtering erases IDs.
    fetch_transcript_from_port(server.port, session_id, Some(blobs), Some(input_ids))
}

/// [`fetch_transcript`] against a bare port, for background threads that must
/// not hold a server handle: a handle would delay the pooled server's
/// teardown behind this request's timeout. Session-id routes need no
/// directory.
pub fn fetch_transcript_on_port(
    port: u16,
    session_id: &str,
) -> anyhow::Result<NativeTranscript> {
    fetch_transcript_from_port(port, session_id, None, None)
}

fn fetch_transcript_from_port(
    port: u16,
    session_id: &str,
    blobs: Option<&BlobStore>,
    retained_inputs: Option<&HashSet<String>>,
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
    restore_history_output(port, session_id, &mut rows);
    Ok(match retained_inputs {
        Some(inputs) => translate_native_rows(&rows, blobs, false, Some(inputs)),
        None => translate_rows_with_blobs(&rows, blobs),
    })
}

pub(super) fn restore_history_output(port: u16, session_id: &str, rows: &mut [Value]) {
    let mut fields = Vec::new();
    for row in rows {
        if row.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        for part in row.get_mut("content").and_then(Value::as_array_mut).into_iter().flatten() {
            match part.get("type").and_then(Value::as_str) {
                Some("text" | "reasoning") => fields.extend(part.get_mut("text")),
                Some("tool") => {
                    let state = if part.get("state").is_some() {
                        part.get_mut("state").expect("checked tool state")
                    } else {
                        part
                    };
                    if let Some(object) = state.as_object_mut() {
                        fields.extend(object.iter_mut().filter_map(|(key, value)| {
                            matches!(key.as_str(), "output" | "content" | "error").then_some(value)
                        }));
                    }
                }
                _ => {}
            }
        }
    }
    crate::context_plugin::restore_output(port, None, session_id, fields);
}

/// Read only the durable assistant steps of a settled turn. Never substitute
/// step-end or local wall-clock time for a missing provider stream boundary.
pub fn fetch_turn_stats(
    server: &OpenCodeServer,
    session_id: &str,
    step_ids: &[String],
) -> anyhow::Result<Option<TurnStats>> {
    if step_ids.is_empty() || step_ids.len() > 256 {
        return Ok(None);
    }
    let mut remaining: std::collections::HashSet<&str> =
        step_ids.iter().map(String::as_str).collect();
    if remaining.len() != step_ids.len() {
        return Ok(None);
    }
    let mut found = std::collections::HashMap::new();
    let mut cursor: Option<String> = None;
    // Reconciliation is best-effort; never scan an unbounded historical store
    // for a step that the server has not persisted yet.
    for _ in 0..10 {
        let mut path = format!(
            "/api/session/{}/message?limit={PAGE_LIMIT}",
            encode_path_segment(session_id)
        );
        if let Some(token) = &cursor {
            path.push_str(&format!("&cursor={}", encode_path_segment(token)));
        }
        let response = server.request_with_timeout("GET", &path, None, HTTP_TIMEOUT)?;
        let rows = response.pointer("/data").and_then(Value::as_array);
        let Some(rows) = rows else { return Ok(None) };
        for row in rows {
            if let Some(id) = row.get("id").and_then(Value::as_str)
                && remaining.remove(id)
            {
                found.insert(id.to_owned(), row.clone());
            }
        }
        if remaining.is_empty() {
            break;
        }
        let next = response.pointer("/cursor/next").and_then(Value::as_str);
        match next {
            Some(next) if !rows.is_empty() && cursor.as_deref() != Some(next) => {
                cursor = Some(next.to_owned())
            }
            _ => break,
        }
    }
    if !remaining.is_empty() {
        return Ok(None);
    }
    let mut stats = TurnStats::default();
    for (index, id) in step_ids.iter().enumerate() {
        let row = &found[id];
        if row.get("type").and_then(Value::as_str) != Some("assistant")
            || row
                .pointer("/time/created")
                .and_then(Value::as_u64)
                .is_none()
            || row
                .pointer("/time/streamed")
                .and_then(Value::as_u64)
                .is_none()
        {
            return Ok(None);
        }
        let Some(step) = turn_stats_step(row) else {
            return Ok(None);
        };
        if step.model.is_some() {
            stats.model = step.model;
            stats.reasoning_effort = step.reasoning_effort;
        }
        if step.agent.is_some() {
            stats.agent = step.agent;
        }
        stats.output_tokens = stats.output_tokens.saturating_add(step.output_tokens);
        stats.stream_ms = stats.stream_ms.saturating_add(step.stream_ms);
        // Only the opening step's first-token boundary describes the wait
        // the user experienced; later steps answer tool continuations.
        if index == 0 {
            stats.first_token_ms = step.first_token_ms;
        }
    }
    stats.stream_verified = true;
    Ok(Some(stats))
}

/// Walk the whole OpenCode store's session list in one pass and collect
/// every session's cumulative usage for the usage statistics page, including
/// the sub-agents those sessions spawned. The list itself is global — no
/// `directory` filter — so sessions from every project the CLI, TUI, or any
/// Rename a native session on the server. The GA endpoint table (verified
/// against v2.0.11 and v2.0.15) has no `/rename` route: retitle through
/// `PATCH /api/session/{id}` with `{"title": …}`, which answers 204. Pre-GA
/// servers answer 404 to that PATCH and only expose rename as its own beta
/// route, so fall back to it there before failing.
pub fn rename_session(
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
pub fn delete_session(server: &OpenCodeServer, session_id: &str) -> anyhow::Result<()> {
    let path = format!("/api/session/{}", encode_path_segment(session_id));
    server.request("DELETE", &path, None)?;
    Ok(())
}

/// List the server's provider integrations: the connectable roster with
/// each entry's key-method support and live credential connections.
pub fn list_image_models(server: &OpenCodeServer) -> anyhow::Result<Value> {
    let response =
        server.request_with_timeout("GET", "/api/model?types=image", None, HTTP_TIMEOUT)?;
    let rows = response
        .get("data")
        .unwrap_or(&response)
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Invalid OpenCode image model inventory"))?;
    let mut models = Vec::new();
    for row in rows {
        if row.get("type").and_then(Value::as_str) != Some("image") {
            continue;
        }
        let Some(provider) = row.get("providerID").and_then(Value::as_str) else {
            continue;
        };
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        models.push(serde_json::json!({"provider": provider, "id": format!("{provider}/{id}"), "label": row.get("name").and_then(Value::as_str).unwrap_or(id)}));
    }
    Ok(Value::Array(models))
}

pub fn list_integrations(
    server: &OpenCodeServer,
) -> anyhow::Result<Vec<IntegrationSummary>> {
    // connect-key does not wait for plugin activation; this list does.
    let response = server.request_with_timeout("GET", "/api/integration", None, HTTP_TIMEOUT)?;
    let rows = integration_rows(&response);
    Ok(rows
        .iter()
        .map(|row| {
            let connections: &[Value] = row
                .get("connections")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            // OpenCode v2.0.20+ tags a connection whose credential was
            // rejected with `status: {status: "needs_auth", ...}`. The
            // integration is only unusable when *every* connection failed —
            // one healthy credential still answers requests.
            let auth_states = connections
                .iter()
                .filter_map(|connection| {
                    connection
                        .get("status")
                        .and_then(|status| status.get("status"))
                        .and_then(Value::as_str)
                })
                .collect::<Vec<_>>();
            // An env connection carries the variable's name (`{"type": "env",
            // "name": "DEEPSEEK_API_KEY"}`). These credentials are not stored
            // in the server's credential store, so the page can badge them and
            // its logout can explain why they survive it.
            let env_names: Vec<String> = connections
                .iter()
                .filter(|connection| connection.get("type").and_then(Value::as_str) == Some("env"))
                .filter_map(|connection| connection.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect();
            // `Integration.OAuthMethod` rows: the sign-in methods the in-app
            // OAuth flow connects with. The id names the connect route's
            // method, the label is the server's own display text, and the
            // form carries the method's string fields — visible ones render
            // as inputs, hidden ones contribute their defaults silently.
            let oauth_methods: Vec<IntegrationOAuthMethod> = row
                .get("methods")
                .and_then(Value::as_array)
                .map(|methods| {
                    methods
                        .iter()
                        .filter(|method| {
                            method.get("type").and_then(Value::as_str) == Some("oauth")
                        })
                        .filter_map(|method| {
                            let form = method
                                .get("form")
                                .and_then(Value::as_array)
                                .map(|fields| {
                                    fields
                                        .iter()
                                        .filter(|field| {
                                            field.get("type").and_then(Value::as_str)
                                                == Some("string")
                                        })
                                        .filter_map(|field| {
                                            Some(IntegrationFormField {
                                                key: field.get("key")?.as_str()?.to_owned(),
                                                title: field
                                                    .get("title")
                                                    .and_then(Value::as_str)
                                                    .map(str::to_owned),
                                                description: field
                                                    .get("description")
                                                    .and_then(Value::as_str)
                                                    .map(str::to_owned),
                                                required: field
                                                    .get("required")
                                                    .and_then(Value::as_bool)
                                                    .unwrap_or(false),
                                                hidden: field
                                                    .get("hidden")
                                                    .and_then(Value::as_bool)
                                                    .unwrap_or(false),
                                                options: field
                                                    .get("options")
                                                    .and_then(Value::as_array)
                                                    .map(|options| {
                                                        options
                                                            .iter()
                                                            .filter_map(|option| {
                                                                Some(IntegrationFormFieldOption {
                                                                    value: option
                                                                        .get("value")?
                                                                        .as_str()?
                                                                        .to_owned(),
                                                                    label: option
                                                                        .get("label")
                                                                        .and_then(Value::as_str)
                                                                        .unwrap_or_default()
                                                                        .to_owned(),
                                                                })
                                                            })
                                                            .collect()
                                                    })
                                                    .unwrap_or_default(),
                                                when: field
                                                    .get("when")
                                                    .and_then(Value::as_array)
                                                    .map(|conditions| {
                                                        conditions
                                                            .iter()
                                                            .filter_map(|condition| {
                                                                Some(
                                                                    IntegrationFormFieldCondition {
                                                                        key: condition
                                                                            .get("key")?
                                                                            .as_str()?
                                                                            .to_owned(),
                                                                        op: condition
                                                                            .get("op")
                                                                            .and_then(Value::as_str)
                                                                            .unwrap_or("eq")
                                                                            .to_owned(),
                                                                        value: condition
                                                                            .get("value")
                                                                            .cloned()
                                                                            .unwrap_or(Value::Null),
                                                                    },
                                                                )
                                                            })
                                                            .collect()
                                                    })
                                                    .unwrap_or_default(),
                                                placeholder: field
                                                    .get("placeholder")
                                                    .and_then(Value::as_str)
                                                    .map(str::to_owned),
                                                default: field
                                                    .get("default")
                                                    .and_then(Value::as_str)
                                                    .map(str::to_owned),
                                                field_type: "string".to_owned(),
                                            })
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            Some(IntegrationOAuthMethod {
                                id: method.get("id")?.as_str()?.to_owned(),
                                label: method
                                    .get("label")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                form,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            IntegrationSummary {
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
                connected: !connections.is_empty(),
                needs_auth: !connections.is_empty()
                    && auth_states.len() == connections.len()
                    && auth_states.iter().all(|state| *state == "needs_auth"),
                env_names,
                key_accounts: connections
                    .iter()
                    .enumerate()
                    .filter_map(|(index, connection)| {
                        if connection["type"].as_str() != Some("credential")
                            || connection["method"].as_str() != Some("key")
                        {
                            return None;
                        }
                        Some(fintwind_protocol::provider_session::IntegrationKeyAccount {
                            id: connection["id"].as_str()?.to_owned(),
                            label: connection["label"].as_str().unwrap_or_default().to_owned(),
                            active: index == 0,
                        })
                    })
                    .collect(),
                oauth_methods,
                auth_url: connections
                    .iter()
                    .filter(|connection| {
                        connection
                            .get("status")
                            .and_then(|status| status.get("status"))
                            .and_then(Value::as_str)
                            == Some("needs_auth")
                    })
                    .find_map(|connection| {
                        connection
                            .get("status")
                            .and_then(|status| status.get("url"))
                            .and_then(Value::as_str)
                    })
                    .map(str::to_owned),
            }
        })
        .collect())
}

/// Store a key for `provider_id` on the server: its connect-key API. The
/// running server adopts the credential immediately and persists it in its
/// own store, so sessions — and the CLI and TUI against this server — can
/// use it without a restart.
pub fn authorize_integration(
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
pub fn logout_integration(server: &OpenCodeServer, provider_id: &str) -> anyhow::Result<()> {
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

/// Start one OAuth attempt for `provider_id` — the same path the CLI's
/// `auth login` and the TUI's /connect take. Like the key connect, the
/// OAuth connect route 404s until the integration list has activated the
/// server's plugins once, so the roster is fetched first; it also doubles
/// as the check that `method_id` really is one of the integration's OAuth
/// methods. `answers` carries the visible form fields' answers; the
/// method's hidden fields contribute their defaults on top, exactly like
/// the OpenCode desktop app's form handling.
pub fn start_integration_oauth(
    server: &OpenCodeServer,
    provider_id: &str,
    method_id: &str,
    answers: &Value,
) -> anyhow::Result<IntegrationOAuthAttempt> {
    let integrations = list_integrations(server)?;
    let integration = integrations
        .iter()
        .find(|integration| integration.id == provider_id);
    let Some(integration) = integration else {
        anyhow::bail!("OpenCode does not provide the integration `{provider_id}`");
    };
    let method = integration
        .oauth_methods
        .iter()
        .find(|method| method.id == method_id);
    let Some(method) = method else {
        anyhow::bail!(
            "OpenCode does not provide the OAuth method `{method_id}` for `{provider_id}`"
        );
    };
    // Fold the declared hidden fields' defaults into the answers, so the
    // request matches what the CLI and desktop app send for methods whose
    // forms hide server-side defaults (e.g. the Console URL).
    let mut answer = serde_json::Map::new();
    for field in &method.form {
        if field.hidden
            && let Some(default) = &field.default {
                answer.insert(field.key.clone(), Value::String(default.clone()));
            }
    }
    if let Some(answers) = answers.as_object() {
        for (key, value) in answers {
            answer.insert(key.clone(), value.clone());
        }
    }
    let path = format!(
        "/api/integration/{}/connect/oauth",
        encode_path_segment(provider_id)
    );
    let mut payload = serde_json::json!({ "methodID": method_id });
    if !answer.is_empty() {
        payload["answer"] = Value::Object(answer);
    }
    let response = server.request("POST", &path, Some(&payload))?;
    parse_oauth_attempt(&response)
}

/// Poll one OAuth attempt: pending while the user signs in, then complete,
/// failed, or expired. A vanished attempt reports expired rather than
/// failing — the flow's wait is simply over.
pub fn poll_integration_oauth(
    server: &OpenCodeServer,
    provider_id: &str,
    attempt_id: &str,
) -> anyhow::Result<IntegrationOAuthPoll> {
    let path = format!(
        "/api/integration/{}/connect/oauth/{}",
        encode_path_segment(provider_id),
        encode_path_segment(attempt_id)
    );
    let response = server.request("GET", &path, None)?;
    let status = response.pointer("/data").unwrap_or(&response);
    let state = status
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match state {
        "pending" => Ok(IntegrationOAuthPoll::Pending),
        "complete" => Ok(IntegrationOAuthPoll::Complete),
        "expired" => Ok(IntegrationOAuthPoll::Expired),
        "failed" => Ok(IntegrationOAuthPoll::Failed {
            message: status
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("authorization failed")
                .to_owned(),
        }),
        other => anyhow::bail!("OpenCode reported the OAuth attempt as `{other}`"),
    }
}

/// Finish a code-mode OAuth attempt with the code the user pasted. The
/// server stores the resulting credential, so the connection goes live
/// without anything else.
pub fn complete_integration_oauth(
    server: &OpenCodeServer,
    provider_id: &str,
    attempt_id: &str,
    code: Option<&str>,
) -> anyhow::Result<()> {
    let path = format!(
        "/api/integration/{}/connect/oauth/{}/complete",
        encode_path_segment(provider_id),
        encode_path_segment(attempt_id)
    );
    server.request("POST", &path, Some(&serde_json::json!({ "code": code })))?;
    Ok(())
}

/// Cancel one OAuth attempt and release its server-side resources. A
/// cancelled or already-finished attempt answers 404 — the goal is met
/// either way, so that is not an error.
pub fn cancel_integration_oauth(
    server: &OpenCodeServer,
    provider_id: &str,
    attempt_id: &str,
) -> anyhow::Result<()> {
    let path = format!(
        "/api/integration/{}/connect/oauth/{}",
        encode_path_segment(provider_id),
        encode_path_segment(attempt_id)
    );
    match server.request("DELETE", &path, None) {
        Ok(_) => Ok(()),
        Err(error) if error.to_string().contains("HTTP 404") => Ok(()),
        Err(error) => Err(error),
    }
}

/// `Integration.Attempt` inside a connect-oauth response: under `data` when
/// enveloped, at the root otherwise.
fn parse_oauth_attempt(response: &Value) -> anyhow::Result<IntegrationOAuthAttempt> {
    let attempt = response.pointer("/data").unwrap_or(response);
    let attempt_id = attempt
        .get("attemptID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let url = attempt
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if attempt_id.is_empty() || url.is_empty() {
        anyhow::bail!("OpenCode returned an incomplete OAuth attempt");
    }
    let mode = match attempt.get("mode").and_then(Value::as_str) {
        Some("code") => IntegrationOAuthMode::Code,
        _ => IntegrationOAuthMode::Auto,
    };
    Ok(IntegrationOAuthAttempt {
        attempt_id: attempt_id.to_owned(),
        url: url.to_owned(),
        instructions: attempt
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        mode,
    })
}

/// Count the models the local OpenCode server currently exposes for
/// `provider_id`. This reads the server's model catalog; it does not
/// test connectivity to the provider's remote endpoint. The catalog is
/// location-scoped: the request names the workspace so a server shared across
/// directories answers this one's provider list.
pub fn probe_provider_models(
    server: &OpenCodeServer,
    directory: &str,
    provider_id: &str,
) -> anyhow::Result<usize> {
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
    Ok(models)
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
pub fn translate_rows_with_blobs(
    rows: &[Value],
    blobs: Option<&BlobStore>,
) -> NativeTranscript {
    translate_native_rows(rows, blobs, false, None)
}

/// Recovery has already validated the admitted input IDs. Even an empty
/// user row owns a turn when it carries non-image files; its client restores
/// the original attachment metadata instead of importing images here.
pub fn translate_recovered_rows(rows: &[Value]) -> NativeTranscript {
    translate_native_rows(rows, None, true, None)
}

/// OpenCode keeps the goal kickoff template in native history. Show only the
/// user-provided objective, never the provider's private control instructions.
fn native_goal_objective(row: &Value) -> Option<String> {
    if row.pointer("/metadata/source").and_then(Value::as_str) != Some("goal") {
        return None;
    }
    let text = row.get("text").and_then(Value::as_str).unwrap_or_default();
    let objective = text
        .split_once("<untrusted_objective>")
        .and_then(|(_, text)| text.split_once("</untrusted_objective>"))
        .map(|(objective, _)| objective.trim())
        .or_else(|| {
            row.get("description")
                .and_then(Value::as_str)
                .and_then(|description| description.strip_prefix("Goal: "))
        })
        .unwrap_or_default();
    Some(
        objective
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

pub fn is_native_goal_kickoff(row: &Value) -> bool {
    row.get("type").and_then(Value::as_str) == Some("synthetic")
        && row.pointer("/metadata/source").and_then(Value::as_str) == Some("goal")
        && row.get("description").and_then(Value::as_str).is_some()
}

fn translate_native_rows(
    rows: &[Value],
    blobs: Option<&BlobStore>,
    retain_empty_users: bool,
    retained_inputs: Option<&HashSet<String>>,
) -> NativeTranscript {
    let mut transcript = NativeTranscript {
        messages: Vec::new(),
        blocks: Vec::new(),
        turns: Vec::new(),
    };
    let mut image_bytes_left = MAX_TRANSCRIPT_IMAGE_BYTES;
    // Idle closes an execution, including prompts steered in while busy.
    // Older histories without markers retain their per-prompt boundaries;
    // recovery rows are already scoped to one admitted execution.
    let explicit_boundaries = retain_empty_users
        || rows
            .iter()
            .any(|row| row.get("type").and_then(Value::as_str) == Some("idle"));
    let mut turn_closed = true;

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
                    turn_id: transcript
                        .turns
                        .last()
                        .filter(|_| explicit_boundaries && !turn_closed)
                        .map(|turn| turn.id),
                    role: MessageRole::Compaction,
                    content: text,
                    display_content: None,
                    attachments: Vec::new(),
                    created_at,
                    streaming: false,
                });
            }
            Some("user") | Some("synthetic")
                if row.get("type").and_then(Value::as_str) == Some("user")
                    || is_native_goal_kickoff(row) =>
            {
                // Verified against 0.0.0-beta-18743: a user row carries its
                // prompt as a top-level `text` string and has no `content`
                // parts at all — unlike assistant rows. Attached files ride
                // in a separate `files` array. Materialize image bytes in
                // daemon-owned storage before the history replaces live rows.
                // Parts are still accepted so a shifted shape degrades to a
                // fallback rather than a lost prompt.
                let native_goal = native_goal_objective(row);
                let mut text = native_goal.clone().unwrap_or_else(|| {
                    row.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                });
                if text.trim().is_empty() && native_goal.is_none() {
                    text = parts_text(row.get("content"));
                }
                let attachments = row
                    .get("files")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|file| native_image_attachment(file, blobs, &mut image_bytes_left))
                    .collect::<Vec<_>>();
                let owned_input = row
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| retained_inputs.is_some_and(|inputs| inputs.contains(id)));
                if !retain_empty_users
                    && !owned_input
                    && text.trim().is_empty()
                    && attachments.is_empty()
                {
                    continue;
                }
                if turn_closed || !explicit_boundaries {
                    let turn = AgentTurn {
                        id: uuid::Uuid::new_v4(),
                        submission: None,
                        turn_count: transcript.turns.len() + 1,
                        status: TurnStatus::Completed,
                        provider_turn_started: true,
                        provider_resume_at: row
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        provider_prompt: None,
                        goal_objective: native_goal.is_some()
                            || row
                                .pointer("/metadata/fintwindGoal")
                                .and_then(Value::as_bool)
                                == Some(true),
                        started_at: created_at,
                        completed_at: Some(created_at),
                        checkpoint: None,
                        stats: None,
                    };
                    transcript.turns.push(turn);
                    turn_closed = false;
                }
                let turn = transcript.turns.last_mut().expect("user turn owner");
                // A goal submitted while a turn is already running is a later
                // input, not the turn's opening user message. Do not label that
                // earlier message as the goal in the restored transcript.
                turn.goal_objective |= row
                    .pointer("/metadata/fintwindGoal")
                    .and_then(Value::as_bool)
                    == Some(true);
                transcript.messages.push(Message {
                    id: uuid::Uuid::new_v4(),
                    turn_id: Some(turn.id),
                    role: MessageRole::User,
                    content: text,
                    display_content: None,
                    attachments,
                    created_at,
                    streaming: false,
                });
            }
            Some("assistant") => {
                let parts = row.get("content").and_then(Value::as_array);
                let step = turn_stats_step(row);
                let visible = assistant_parts_visible(parts);
                // A synthetic continuation has no visible user row. Establish
                // its owner before folding the first model step's statistics,
                // but only when the row actually contributes: a blank row
                // must not leave an orphan turn without any message.
                if (step.is_some() || visible) && turn_closed {
                    transcript.turns.push(AgentTurn {
                        id: uuid::Uuid::new_v4(),
                        submission: None,
                        turn_count: transcript.turns.len() + 1,
                        status: TurnStatus::Completed,
                        provider_turn_started: true,
                        provider_resume_at: None,
                        provider_prompt: None,
                        goal_objective: false,
                        started_at: created_at,
                        completed_at: Some(created_at),
                        checkpoint: None,
                        stats: None,
                    });
                    turn_closed = false;
                }
                // Every assistant row is one model step of the turn above it,
                // and the fold happens before the content gate: a row with no
                // visible text still ran a model step whose tokens and
                // streaming time belong to the turn's footer statistics.
                if let Some(step) = step
                    && let Some(turn) = transcript.turns.last_mut()
                {
                    let stats = turn.stats.get_or_insert_with(TurnStats::default);
                    if step.model.is_some() {
                        stats.model = step.model;
                        stats.reasoning_effort = step.reasoning_effort;
                    }
                    if step.agent.is_some() {
                        stats.agent = step.agent;
                    }
                    stats.output_tokens = stats.output_tokens.saturating_add(step.output_tokens);
                    stats.stream_ms = stats.stream_ms.saturating_add(step.stream_ms);
                    // The opening step owns the turn's first-token latency;
                    // a later step's boundary answers a tool continuation.
                    stats.first_token_ms = stats.first_token_ms.or(step.first_token_ms);
                    // One step carrying both timestamps measures the turn's
                    // streaming duration, so a later step that never streams
                    // must not withdraw what an earlier one established —
                    // the turn's stats fold every step, not just the last.
                    // The `as_u64` gate keeps this in step with where the
                    // duration itself is read (`turn_stats_step`): a
                    // non-numeric timestamp would measure nothing.
                    if row
                        .pointer("/time/created")
                        .and_then(Value::as_u64)
                        .is_some()
                        && row
                            .pointer("/time/streamed")
                            .and_then(Value::as_u64)
                            .is_some()
                    {
                        stats.stream_verified = true;
                    }
                }
                if !visible {
                    continue;
                }
                // Step completion is the fallback until the execution idle marker arrives.
                let completed_at =
                    ms_to_seconds(row.pointer("/time/completed")).unwrap_or(created_at);
                let turn = transcript.turns.last_mut().expect("assistant turn owner");
                turn.completed_at = Some(completed_at.max(turn.completed_at.unwrap_or(0)));
                let turn_id = turn.id;
                // Parts stay in stored order. Text that a tool sits between is
                // two messages with the tool block between them; consecutive
                // text parts with nothing between them still join. Hoisting
                // every tool above every text part is what made a restart hide
                // a live split instead of reproducing the part order.
                append_assistant_parts(&mut transcript, parts, turn_id, created_at);
            }
            Some("idle") => {
                if !turn_closed && let Some(turn) = transcript.turns.last_mut() {
                    turn.completed_at = Some(created_at.max(turn.completed_at.unwrap_or(0)));
                    turn.status = match row.get("outcome").and_then(Value::as_str) {
                        Some("failed") => TurnStatus::Failed,
                        Some("interrupted") => TurnStatus::Interrupted,
                        _ => TurnStatus::Completed,
                    };
                }
                turn_closed = true;
            }
            _ => {}
        }
    }

    transcript
}

/// OpenCode stores user images as base64 in `files`, even when the input came
/// from a file URI. Keep only the compact reference in Fintwind's transcript.
fn native_image_attachment(
    file: &Value,
    blobs: Option<&BlobStore>,
    image_bytes_left: &mut usize,
) -> Option<MessageAttachment> {
    let mime = file.get("mime")?.as_str()?;
    let extension = fintwind_storage::blob_store::extension_for_mime(mime);
    if extension == "bin" {
        return None;
    }
    let name = file
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| {
            std::path::Path::new(name)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|suffix| {
                    suffix.eq_ignore_ascii_case(extension)
                        || (extension == "jpg" && suffix.eq_ignore_ascii_case("jpeg"))
                        || (extension == "tiff" && suffix.eq_ignore_ascii_case("tif"))
                })
        })
        .map(str::to_owned)
        .unwrap_or_else(|| format!("image.{extension}"));
    let stored = (|| {
        let blobs = blobs?;
        let data = file.get("data")?.as_str()?;
        // Provider data is untrusted; bound both individual decodes and the
        // whole transcript before allocating or writing image bytes.
        let max_bytes = fintwind_protocol::attachments::MAX_PROMPT_FILE_BYTES;
        if data.len() > max_bytes.div_ceil(3) * 4
            || data.len().div_ceil(4).saturating_mul(3).saturating_sub(2) > *image_bytes_left
        {
            return None;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?;
        if bytes.is_empty() || bytes.len() > max_bytes || bytes.len() > *image_bytes_left {
            return None;
        }
        let reference = blobs.store_image_bytes(mime, &bytes).ok()?;
        let path = blobs.path_for(&reference)?;
        *image_bytes_left -= bytes.len();
        Some((path, reference))
    })();
    let (path, reference) = match stored {
        Some((path, reference)) => (path, Some(reference)),
        None => (Default::default(), None),
    };
    Some(MessageAttachment {
        mention: path.to_string_lossy().into_owned(),
        path,
        name,
        is_dir: false,
        is_image: true,
        blob_reference: reference,
    })
}

#[cfg(test)]
fn translate_rows(rows: &[Value]) -> NativeTranscript {
    translate_rows_with_blobs(rows, None)
}

/// One assistant row's contribution to its turn's footer statistics. The
/// stored rows — unlike the live step events, which carry no times — keep the
/// `time.streamed`/`time.created` pair the TUI's own footer divides by, so
/// the import path can reproduce the exact numbers a live turn accumulates.
/// `time.firstToken` (opencode 2.3+) marks the first output token; against
/// `time.created` it measures this step's first-token latency. A row that
/// reports nothing measurable folds in as `None`.
fn turn_stats_step(row: &Value) -> Option<TurnStats> {
    let reasoning_effort = row
        .pointer("/model/variant")
        .and_then(Value::as_str)
        .filter(|variant| !variant.trim().is_empty())
        .map(str::to_owned);
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
    let created = row.pointer("/time/created").and_then(Value::as_u64);
    let stream_ms = match (
        row.pointer("/time/streamed").and_then(Value::as_u64),
        created,
    ) {
        (Some(streamed), Some(created)) => streamed.saturating_sub(created),
        _ => 0,
    };
    let first_token_ms = match (
        row.pointer("/time/firstToken").and_then(Value::as_u64),
        created,
    ) {
        (Some(first_token), Some(created)) => Some(first_token.saturating_sub(created)),
        _ => None,
    };
    (model.is_some() || agent.is_some() || output > 0 || stream_ms > 0 || first_token_ms.is_some())
        .then_some(TurnStats {
            model,
            reasoning_effort,
            agent,
            output_tokens: output,
            stream_ms,
            first_token_ms,
            ..TurnStats::default()
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
        super::activity::ToolPayloads {
            arguments: state.get("input"),
            output: output.as_ref(),
            image_source: state.get("metadata"),
        },
        failed,
        true,
    );
    // Native replay must use provider timestamps, never the time of loading
    // the transcript. The legacy tool shape kept these under state.time.
    item.started_at_ms = part
        .pointer("/time/created")
        .or_else(|| state.pointer("/time/start"))
        .and_then(Value::as_u64)
        .filter(|time| *time > 0);
    item.finished_at_ms = part
        .pointer("/time/completed")
        .or_else(|| state.pointer("/time/end"))
        .and_then(Value::as_u64)
        .filter(|time| *time > 0);
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
        // A session created outside this app carries no task metadata.
        assert_eq!(summary.task, None);
        // The child carries a parentID; `list_sessions` filters it out.
        assert!(is_child_session(&child_row));
        assert!(!is_child_session(&server_row));
    }

    #[test]
    fn sessions_list_reads_the_task_link_from_session_metadata() {
        // The live create response (2.0.11 and 2.0.16): `metadata` carries
        // what this app wrote at create time.
        let app_row = json!({
            "id": "ses_app",
            "title": "Fintwind 任务",
            "time": {"created": 1_000_u64, "updated": 2_000_u64},
            "metadata": {"source": "fintwind", "task": "11111111-1111-1111-1111-111111111111"}
        });
        let summary = summary_from_row(&app_row).unwrap();
        assert_eq!(
            summary.task.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );

        // Sessions without a task link — older builds of this app, the CLI,
        // the TUI — and blank links stay unclaimed.
        for row in [
            json!({"id": "ses_a", "time": {"created": 1_u64, "updated": 1_u64}}),
            json!({
                "id": "ses_b",
                "time": {"created": 1_u64, "updated": 1_u64},
                "metadata": {"source": "tui"}
            }),
            json!({
                "id": "ses_c",
                "time": {"created": 1_u64, "updated": 1_u64},
                "metadata": {"source": "fintwind", "task": "  "}
            }),
        ] {
            assert_eq!(summary_from_row(&row).unwrap().task, None);
        }
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
                stream_verified: true,
                ..TurnStats::default()
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
                ..TurnStats::default()
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

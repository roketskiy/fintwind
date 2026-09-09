//! OpenCode native session surface outside a live driver runtime.
//!
//! Sessions exist on the OpenCode server whether or not the app created them:
//! the CLI and TUI write into the same store. This module lists a workspace's
//! sessions, translates a native transcript into the app's message/block
//! model (the exact shapes a live session persists, so an imported session
//! renders through the ordinary transcript pipeline), and applies title and
//! deletion edits back to the server.
//!
//! Wire shapes verified against `opencode2` 0.0.0-beta-18743:
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

use fintwind_protocol::provider_session::{NativeSessionSummary, NativeTranscript};

use crate::model::{ActivityItem, AgentTurn, Message, MessageRole, ReasoningBlock, TurnStatus};
use crate::opencode_session::{OpenCodeServer, encode_path_segment};

/// Page size for paged lists; matches the live transcript reader.
const PAGE_LIMIT: usize = 200;
/// Sessions per workspace are bounded in practice; the cap keeps a pathological
/// store from paging forever.
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

/// Rename a native session on the server. This beta exposes rename as
/// `POST /api/session/{id}/rename` (a `PATCH /api/session/{id}` answers 404,
/// verified against 0.0.0-beta-18743).
pub(crate) fn rename_session(
    server: &OpenCodeServer,
    session_id: &str,
    title: &str,
) -> anyhow::Result<()> {
    let path = format!(
        "/api/session/{}/rename",
        encode_path_segment(session_id)
    );
    server.request(
        "POST",
        &path,
        Some(&serde_json::json!({"title": title})),
    )?;
    Ok(())
}

/// Delete a native session on the server, transcript and all.
pub(crate) fn delete_session(server: &OpenCodeServer, session_id: &str) -> anyhow::Result<()> {
    let path = format!("/api/session/{}", encode_path_segment(session_id));
    server.request("DELETE", &path, None)?;
    Ok(())
}

/// Translate the native message rows (oldest first) into the app's model.
fn translate_rows(rows: &[Value]) -> NativeTranscript {
    let mut transcript = NativeTranscript {
        messages: Vec::new(),
        blocks: Vec::new(),
        turns: Vec::new(),
    };

    for row in rows {
        let created_at = ms_to_seconds(row.get("time").and_then(|time| time.get("created")))
            .unwrap_or_default();
        match row.get("type").and_then(Value::as_str) {
            Some("compaction") => {
                // opencode2 stores a completed compaction as its own message
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
                if activities.is_empty() && text.trim().is_empty() {
                    continue;
                }
                // A settled assistant message carries its completion time;
                // that is when the turn ended.
                let completed_at = ms_to_seconds(row.pointer("/time/completed"))
                    .unwrap_or(created_at);
                // The response belongs to the last open turn; an assistant
                // message without a user turn above it (a pre-title or
                // synthetic open) models its own turn so block/turn
                // references stay consistent.
                let turn_id = match transcript.turns.last_mut() {
                    Some(turn) => {
                        turn.completed_at =
                            Some(completed_at.max(turn.completed_at.unwrap_or(0)));
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
                        _ => transcript.blocks.push(fintwind_protocol::model::TranscriptBlock {
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
    // opencode2 persists `name`/`id` on the part and keeps the result text in
    // `state.content` with no `state.title`; the legacy shape used
    // `tool`/`callID` with `state.output` and a server-authored title.
    // Accept both so a restored transcript keeps the identity a live stream
    // gave the same tool call.
    let state = part.get("state").unwrap_or(part);
    let name = part
        .get("name")
        .or_else(|| part.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let kind = crate::model::ActivityKind::from_tool_name(name);
    let title = state
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| name.to_owned());
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
    super::activity::tool_activity(
        None,
        kind,
        title,
        state.get("input"),
        output.as_ref(),
        state.get("metadata"),
        failed,
        true,
    )
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
        assert_eq!(summary.model.as_deref(), Some("opencode-go/deepseek-v4-flash"));
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
            assert!(transcript.turns.iter().any(|turn| Some(turn.id) == message.turn_id));
        }
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
        assert_eq!(item.title, "Run the tests");
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
    fn opencode2_tool_parts_keep_their_identity() {
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
        assert_eq!(item.display_target.as_deref(), Some("src/workspace.rs"));
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
        assert!(item.failed);
        assert!(!item.file_changes.is_empty());
        assert!(item.output.as_deref().is_some_and(|output| output.contains("patch did not apply")));
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

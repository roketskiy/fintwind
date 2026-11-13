//! Adapter for OpenCode's built-in Goal plugin (2.2.1 and later).

use anyhow::Context as _;
use fintwind_protocol::goal::{GoalSnapshot, GoalStatus};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeGoal {
    objective: String,
    status: NativeGoalStatus,
    #[serde(default)]
    reason: Option<String>,
    rounds: u32,
    max_rounds: u32,
    updated_at: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum NativeGoalStatus {
    Active,
    Paused,
    Complete,
}

/// Translate the native snapshot into the existing desktop/daemon wire shape.
/// OpenCode does not expose tool counts, elapsed time, or a separate result.
pub(crate) fn snapshot(value: Value, session_id: &str) -> anyhow::Result<Option<GoalSnapshot>> {
    if value.is_null() {
        return Ok(None);
    }
    let goal: NativeGoal = serde_json::from_value(value).context("invalid OpenCode goal")?;
    let status = match goal.status {
        NativeGoalStatus::Active => GoalStatus::Active,
        NativeGoalStatus::Paused => GoalStatus::Paused,
        NativeGoalStatus::Complete => GoalStatus::Done,
    };
    Ok(Some(GoalSnapshot {
        session_id: session_id.to_owned(),
        objective: goal.objective,
        status,
        turns: goal.rounds,
        max_turns: goal.max_rounds,
        reason: goal.reason.unwrap_or_default(),
        revision: goal.updated_at,
        result: None,
        model: None,
        tools: None,
        started_at: None,
        completed_at: (status == GoalStatus::Done).then_some(goal.updated_at / 1000),
        elapsed_seconds: None,
        active_since: None,
    }))
}

pub(crate) fn get(
    server: &crate::opencode_session::OpenCodeServer,
    directory: &str,
    session_id: &str,
) -> anyhow::Result<Option<GoalSnapshot>> {
    let response = server.request_for_directory_with_timeout(
        directory,
        "POST",
        "/api/rpc/opencode.goal/get",
        Some(&json!({ "input": { "sessionID": session_id } })),
        Duration::from_secs(10),
    )?;
    snapshot(
        response.get("output").cloned().unwrap_or(response),
        session_id,
    )
}

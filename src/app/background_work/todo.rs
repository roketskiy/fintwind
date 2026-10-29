//! The agent's own task list, read from the latest plan tool call in a
//! session. OpenCode's `todowrite` sends the complete list as
//! `{ "todos": [{ "content", "status" }] }` with every call, so the newest
//! plan activity is the whole state; older calls only preserve history.

use fintwind_protocol::model::{ActivityKind, AgentSession};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl TodoStatus {
    fn from_wire(status: &str) -> Self {
        let normalized = status.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        match normalized.as_str() {
            "completed" | "complete" | "done" | "success" => Self::Completed,
            "in_progress" | "running" | "active" => Self::InProgress,
            "cancelled" | "canceled" | "skipped" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::app) struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Clone, Debug)]
pub(in crate::app) struct TodoSnapshot {
    pub items: Vec<TodoItem>,
    /// Finished items, out of the total.
    pub completed: usize,
    /// Items still waiting or running; cancelled work is not outstanding.
    pub open: usize,
}

impl TodoSnapshot {
    fn new(items: Vec<TodoItem>) -> Self {
        let completed = items
            .iter()
            .filter(|item| item.status == TodoStatus::Completed)
            .count();
        let open = items
            .iter()
            .filter(|item| matches!(item.status, TodoStatus::Pending | TodoStatus::InProgress))
            .count();
        Self {
            items,
            completed,
            open,
        }
    }
}

/// The newest plan tool call in the loaded transcript, if it carries a list.
pub(in crate::app) fn latest_snapshot(session: &AgentSession) -> Option<TodoSnapshot> {
    for block in session.transcript_blocks.iter().rev() {
        for activity in block.activities.iter().rev() {
            if activity.kind != ActivityKind::Plan {
                continue;
            }
            if let Some(items) = activity.arguments.as_deref().and_then(parse_arguments) {
                return Some(TodoSnapshot::new(items));
            }
        }
    }
    None
}

fn parse_arguments(arguments: &str) -> Option<Vec<TodoItem>> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    // Some providers wrap the tool input as a JSON string.
    let value = match value {
        Value::String(text) => serde_json::from_str(&text).ok()?,
        value => value,
    };
    let list = value
        .get("todos")
        .or_else(|| value.get("plan"))
        .or_else(|| value.get("items"))?
        .as_array()?;
    let items = list.iter().filter_map(parse_item).collect::<Vec<_>>();
    // An empty array is a cleared list; a non-empty array whose entries all
    // failed to parse is not a todo list, so keep looking further back.
    if list.is_empty() {
        Some(items)
    } else {
        (!items.is_empty()).then_some(items)
    }
}

fn parse_item(value: &Value) -> Option<TodoItem> {
    let content = value
        .get("content")
        .or_else(|| value.get("step"))
        .or_else(|| value.get("title"))
        .or_else(|| value.get("text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|content| !content.is_empty())?;
    let status = match value.get("status").and_then(Value::as_str) {
        Some(status) => TodoStatus::from_wire(status),
        None if value.get("completed").and_then(Value::as_bool) == Some(true) => {
            TodoStatus::Completed
        }
        None => TodoStatus::Pending,
    };
    Some(TodoItem {
        content: content.to_owned(),
        status,
    })
}

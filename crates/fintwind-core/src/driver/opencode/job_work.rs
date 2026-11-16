//! The experimental job roster supplements, rather than replaces, session and
//! shell history: jobs disappear as soon as their completion is consumed.

use std::time::Duration;

use serde_json::Value;

use crate::model::{BackgroundWorkItem, BackgroundWorkKind, BackgroundWorkStatus};
use crate::opencode_session::{encode_path_segment, request_json_on_port_with_directory};

pub(super) struct Jobs {
    pub shells: Vec<BackgroundWorkItem>,
    pub children: Vec<BackgroundWorkItem>,
    pub images: Vec<BackgroundWorkItem>,
}

pub(super) fn list(port: u16, directory: &str, session_id: &str) -> Option<Jobs> {
    let path = format!(
        "/api/experimental/job?sessionID={}&descendants=true",
        encode_path_segment(session_id)
    );
    let response = request_json_on_port_with_directory(
        port,
        "GET",
        &path,
        None,
        Duration::from_secs(2),
        Some(directory),
    )
    .ok()?;
    let rows = response.get("data")?.as_array()?;
    let mut jobs = Jobs {
        shells: Vec::new(),
        children: Vec::new(),
        images: Vec::new(),
    };
    for row in rows {
        let status = match row.get("status")?.as_str()? {
            "running" => BackgroundWorkStatus::Running,
            "completed" => BackgroundWorkStatus::Completed,
            "error" => BackgroundWorkStatus::Failed,
            "cancelled" => BackgroundWorkStatus::Stopped,
            _ => return None,
        };
        let owner = row.get("sessionID")?.as_str()?;
        match row.get("kind")?.as_str()? {
            "shell" => {
                let id = row.get("shellID")?.as_str()?;
                if status.is_live() {
                    let command = row.get("command")?.as_str()?;
                    let mut item =
                        BackgroundWorkItem::new(BackgroundWorkKind::Process, id, command, status);
                    item.command = Some(command.to_owned());
                    item.background = true;
                    item.can_stop = true;
                    item.control_id = Some(id.to_owned());
                    item.parent_id = Some(owner.to_owned());
                    set_time(&mut item, row);
                    jobs.shells.push(item);
                }
            }
            "subagent" => {
                let id = row.get("childSessionID")?.as_str()?;
                let title = row
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| row.get("description").and_then(Value::as_str))
                    .unwrap_or_default();
                let mut item =
                    BackgroundWorkItem::new(BackgroundWorkKind::Subagent, id, title, status);
                item.background = true;
                item.can_stop = status.is_stoppable();
                item.control_id = Some(id.to_owned());
                item.parent_id = Some(owner.to_owned());
                item.role = row.get("agent").and_then(Value::as_str).map(str::to_owned);
                item.command = row
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                set_time(&mut item, row);
                jobs.children.push(item);
            }
            "image" => {
                let id = row.get("id")?.as_str()?;
                let title = row
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|title| !title.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| tr!("background.image"));
                let mut item =
                    BackgroundWorkItem::new(BackgroundWorkKind::Image, id, title, status);
                item.background = true;
                item.parent_id = Some(owner.to_owned());
                item.model = row.get("model").and_then(Value::as_str).map(str::to_owned);
                item.detail = row.get("path").and_then(Value::as_str).map(str::to_owned);
                set_time(&mut item, row);
                jobs.images.push(item);
            }
            _ => return None,
        }
    }
    Some(jobs)
}

fn set_time(item: &mut BackgroundWorkItem, row: &Value) {
    if let Some(started) = row.pointer("/time/started").and_then(Value::as_u64) {
        item.started_at_ms = started;
        item.updated_at_ms = started;
    }
    if let Some(completed) = row.pointer("/time/completed").and_then(Value::as_u64) {
        item.updated_at_ms = completed;
        item.duration_ms = Some(completed.saturating_sub(item.started_at_ms));
    }
}

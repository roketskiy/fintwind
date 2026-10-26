use std::sync::{Arc, Weak};

use anyhow::{Context as _, bail};
use chrono::Local;
use fintwind_protocol::scheduled_task::{
    ClockTime, MAX_INTERVAL_SECS, MIN_INTERVAL_SECS, Recurrence, ScheduledTask, ScheduledTaskDraft,
    TaskTarget, Weekdays,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::daemon::FintwindBackend;

#[derive(Clone)]
pub(crate) struct Runtime {
    pub registry: Arc<super::mcp::Registry>,
    pub backend: Weak<FintwindBackend>,
    pub session_id: Uuid,
    pub runtime_id: Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateTask {
    title: String,
    prompt: String,
    enabled: bool,
    schedule: Schedule,
    pub target: Target,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Target {
    CurrentSession,
    NewSession,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Schedule {
    Every { interval_seconds: u32 },
    AtTime { time: String, weekdays: Vec<Day> },
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Day {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl CreateTask {
    pub(crate) fn draft(self, target: TaskTarget) -> anyhow::Result<ScheduledTaskDraft> {
        let schedule = match self.schedule {
            Schedule::Every { interval_seconds } => Recurrence::Every {
                interval_secs: interval_seconds,
            },
            Schedule::AtTime { time, weekdays } => {
                let (hour, minute) = time.split_once(':').context("time must use HH:MM")?;
                if time.len() != 5
                    || hour.len() != 2
                    || minute.len() != 2
                    || !hour
                        .bytes()
                        .chain(minute.bytes())
                        .all(|byte| byte.is_ascii_digit())
                {
                    bail!("time must use HH:MM");
                }
                let at =
                    ClockTime::new(hour.parse()?, minute.parse()?).map_err(anyhow::Error::msg)?;
                if weekdays.is_empty() || weekdays.len() > 7 {
                    bail!("Select between one and seven weekdays");
                }
                let mut bits = 0;
                for day in weekdays {
                    let bit = 1
                        << match day {
                            Day::Sunday => 0,
                            Day::Monday => 1,
                            Day::Tuesday => 2,
                            Day::Wednesday => 3,
                            Day::Thursday => 4,
                            Day::Friday => 5,
                            Day::Saturday => 6,
                        };
                    if bits & bit != 0 {
                        bail!("Weekdays must not contain duplicates");
                    }
                    bits |= bit;
                }
                Recurrence::AtTime {
                    at,
                    weekdays: Weekdays::new(bits).map_err(anyhow::Error::msg)?,
                }
            }
        };
        let draft = ScheduledTaskDraft {
            title: self.title,
            prompt: self.prompt,
            enabled: self.enabled,
            schedule,
            target,
        };
        draft.validate().map_err(anyhow::Error::msg)?;
        Ok(draft)
    }
}

pub(crate) fn definition() -> Value {
    json!({
        "name": "create",
        "description": format!(
            "Create a recurring task in Fintwind's scheduled tasks module. Ask the user to resolve any unspecified execution target, schedule, time zone, or task instructions before calling; never invent defaults. \
             target=currentSession continues this conversation; target=newSession starts a conversation in the same workspace, copying the current model and permission settings. \
             Use details already specified by the user; ask only for missing choices. AtTime schedules use the daemon's local time zone (currently {}). \
             Every schedules run first after the interval. Tasks run while the Fintwind daemon is running. \
             Return the created task ID and next run time to the user. A lost response may mean creation succeeded; do not automatically repeat a create.",
            Local::now().format("%Z %:z")
        ),
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "properties": {
                "title": {"type": "string", "minLength": 1, "maxLength": 512},
                "prompt": {"type": "string", "minLength": 1, "maxLength": 1048576,
                    "description": "Instructions sent to the agent on each scheduled run."},
                "enabled": {"type": "boolean", "description": "Whether the user wants the task active immediately."},
                "target": {"type": "string", "enum": ["currentSession", "newSession"],
                    "description": "Execution target confirmed with the user. Required; there is no default."},
                "schedule": {"oneOf": [
                    {"type": "object", "additionalProperties": false,
                        "properties": {"kind": {"type": "string", "const": "every"},
                            "intervalSeconds": {"type": "integer", "minimum": MIN_INTERVAL_SECS, "maximum": MAX_INTERVAL_SECS}},
                        "required": ["kind", "intervalSeconds"]},
                    {"type": "object", "additionalProperties": false,
                        "properties": {"kind": {"type": "string", "const": "atTime"},
                            "time": {"type": "string", "pattern": "^([01][0-9]|2[0-3]):[0-5][0-9]$", "description": "HH:MM in the daemon's local time zone."},
                            "weekdays": {"type": "array", "minItems": 1, "maxItems": 7, "uniqueItems": true,
                                "items": {"type": "string", "enum": ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"]}}},
                        "required": ["kind", "time", "weekdays"]}
                ]}
            },
            "required": ["title", "prompt", "enabled", "target", "schedule"]
        },
        "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false}
    })
}

impl Runtime {
    pub(crate) fn create(&self, input: Value) -> anyhow::Result<Value> {
        let input: CreateTask = serde_json::from_value(input).context(
            "Invalid task arguments. Ask the user for missing details before calling again",
        )?;
        let backend = self
            .backend
            .upgrade()
            .context("Fintwind daemon has stopped")?;
        let task =
            backend.create_scheduled_task_from_agent(self.session_id, self.runtime_id, input)?;
        Ok(created(&task))
    }
}

fn created(task: &ScheduledTask) -> Value {
    json!({
        "id": task.id, "title": task.title, "enabled": task.enabled,
        "schedule": task.schedule, "target": task.target,
        "nextDueAt": task.next_due_at,
        "nextRunLocal": task.next_due_at.and_then(|seconds| {
            chrono::DateTime::from_timestamp(i64::try_from(seconds).ok()?, 0)
                .map(|time| time.with_timezone(&Local).to_rfc3339())
        }),
    })
}

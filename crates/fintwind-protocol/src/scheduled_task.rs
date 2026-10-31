use chrono::{Datelike, Local, NaiveDate, TimeZone, Weekday};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use uuid::Uuid;

use crate::model::{AgentTurn, InteractionMode, Message, RuntimeMode, SessionWorkspace};

pub const MIN_INTERVAL_SECS: u32 = 60;
pub const MAX_INTERVAL_SECS: u32 = 365 * 24 * 60 * 60;
pub const FIXED_TIME_GRACE_SECS: u64 = 600;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockTime(u16);

impl ClockTime {
    pub fn new(hour: u8, minute: u8) -> Result<Self, String> {
        if hour > 23 || minute > 59 {
            return Err("Time must be between 00:00 and 23:59".into());
        }
        Ok(Self(u16::from(hour) * 60 + u16::from(minute)))
    }

    pub fn hour(self) -> u8 {
        (self.0 / 60) as u8
    }
    pub fn minute(self) -> u8 {
        (self.0 % 60) as u8
    }
    pub fn minutes(self) -> u16 {
        self.0
    }
}

impl Serialize for ClockTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u16(self.0)
    }
}

impl<'de> Deserialize<'de> for ClockTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let minutes = u16::deserialize(deserializer)?;
        if minutes >= 1440 {
            return Err(D::Error::custom("Time must be between 00:00 and 23:59"));
        }
        Ok(Self(minutes))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Weekdays(u8);

impl Weekdays {
    pub fn new(bits: u8) -> Result<Self, String> {
        if bits == 0 || bits > 127 {
            return Err("Select at least one valid weekday".into());
        }
        Ok(Self(bits))
    }

    pub fn every_day() -> Self {
        Self(127)
    }
    pub fn bits(self) -> u8 {
        self.0
    }
    pub fn contains(self, day: Weekday) -> bool {
        self.0 & (1 << day.num_days_from_sunday()) != 0
    }
}

impl Serialize for Weekdays {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for Weekdays {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(u8::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Recurrence {
    Every { interval_secs: u32 },
    AtTime { at: ClockTime, weekdays: Weekdays },
    Once { at: u64 },
}

impl<'de> Deserialize<'de> for Recurrence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(
            tag = "kind",
            rename_all = "camelCase",
            rename_all_fields = "camelCase"
        )]
        enum Wire {
            Every { interval_secs: u32 },
            AtTime { at: ClockTime, weekdays: Weekdays },
            Once { at: u64 },
        }
        let recurrence = match Wire::deserialize(deserializer)? {
            Wire::Every { interval_secs } => Self::Every { interval_secs },
            Wire::AtTime { at, weekdays } => Self::AtTime { at, weekdays },
            Wire::Once { at } => Self::Once { at },
        };
        recurrence.validate().map_err(D::Error::custom)?;
        Ok(recurrence)
    }
}

impl Recurrence {
    pub fn is_once(&self) -> bool {
        matches!(self, Self::Once { .. })
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Self::Every { interval_secs } = self
            && !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(interval_secs)
        {
            return Err("Interval must be between one minute and 365 days".into());
        }
        if let Self::Once { at } = self {
            if *at == 0 {
                return Err("One-time run time is invalid".into());
            }
            if i64::try_from(*at).is_err() {
                return Err("One-time run time is out of range".into());
            }
        }
        Ok(())
    }

    pub fn next_due(&self, after: u64) -> Result<u64, String> {
        self.validate()?;
        if let Self::Every { interval_secs } = self {
            return after
                .checked_add(u64::from(*interval_secs))
                .ok_or_else(|| "Next run exceeds the timestamp range".into());
        }
        if let Self::Once { at } = self {
            if *at > after {
                return Ok(*at);
            }
            return Err("One-time run is in the past".into());
        }
        let Self::AtTime { at, weekdays } = self else {
            unreachable!()
        };
        let timestamp = i64::try_from(after).map_err(|_| "Timestamp is out of range")?;
        let mut date = Local
            .timestamp_opt(timestamp, 0)
            .earliest()
            .ok_or("Timestamp has no local representation")?
            .date_naive();
        for _ in 0..15 {
            if weekdays.contains(date.weekday())
                && let Some(next) = occurrence_on(date, *at)
                && next > after
            {
                return Ok(next);
            }
            date = date.succ_opt().ok_or("Calendar date exceeds its range")?;
        }
        Err("No valid scheduled day was found".into())
    }
}

fn occurrence_on(date: NaiveDate, at: ClockTime) -> Option<u64> {
    for minute in at.minutes()..1440 {
        let local = date.and_hms_opt(u32::from(minute / 60), u32::from(minute % 60), 0)?;
        // A repeated hour has one occurrence. A missing minute advances on the same day.
        if let Some(instant) = Local.from_local_datetime(&local).earliest()
            && let Ok(seconds) = u64::try_from(instant.timestamp())
        {
            return Some(seconds);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScheduledTaskId(pub Uuid);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLaunch {
    pub model: String,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub context_window: Option<String>,
    pub agent_preset: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TaskTarget {
    NewSession {
        project_id: Uuid,
        launch: SessionLaunch,
        workspace: SessionWorkspace,
    },
    ExistingSession {
        session_id: Uuid,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskDraft {
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: Recurrence,
    pub target: TaskTarget,
}

impl ScheduledTaskDraft {
    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() || self.title.len() > 512 {
            return Err("Task title must contain between 1 and 512 characters".into());
        }
        if self.prompt.trim().is_empty() || self.prompt.len() > 1024 * 1024 {
            return Err("Task prompt is empty or too long".into());
        }
        self.schedule.validate()?;
        if let TaskTarget::NewSession {
            launch, workspace, ..
        } = &self.target
        {
            if launch.model.trim().is_empty() {
                return Err("Select a model for the new session".into());
            }
            if matches!(workspace, SessionWorkspace::NewWorktree { .. }) {
                return Err("Scheduled tasks require an existing workspace".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ScheduledTaskEdit {
    Create {
        draft: ScheduledTaskDraft,
    },
    Update {
        id: ScheduledTaskId,
        expected_revision: u64,
        draft: ScheduledTaskDraft,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DispatchOutcome {
    Accepted {
        session_id: Uuid,
        submission_id: Uuid,
    },
    Unconfirmed {
        session_id: Uuid,
        submission_id: Uuid,
    },
    NotSent {
        session_id: Option<Uuid>,
        submission_id: Option<Uuid>,
        reason: String,
    },
    Rejected {
        reason: String,
    },
    Missed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingOccurrence {
    pub id: Uuid,
    pub due_at: u64,
    pub session_id: Option<Uuid>,
    pub preparing: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchRecord {
    pub due_at: u64,
    pub at: u64,
    pub outcome: DispatchOutcome,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTask {
    pub id: ScheduledTaskId,
    pub revision: u64,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: Recurrence,
    pub target: TaskTarget,
    pub next_due_at: Option<u64>,
    pub created_at: u64,
    pub updated_at: u64,
    pub pending: Option<PendingOccurrence>,
    pub last: Option<DispatchRecord>,
    pub run_count: u64,
}

/// The turn a scheduled run admitted, plus its opening user message. Carried by
/// `DriverEvent::ScheduledTurnAdmitted` on the same ordered per-runtime stream
/// as the provider events that follow, so an attached client can hydrate the
/// turn before `TurnStarted` and its deltas arrive. The message's `turn_id` and
/// the turn's `submission.id` (when present) must equal the turn's `id`; the
/// daemon only builds it from a turn opened with `begin_turn_with_id`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTurnAdmission {
    pub turn: AgentTurn,
    pub message: Message,
}

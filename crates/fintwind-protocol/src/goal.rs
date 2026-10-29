use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GoalStatus {
    Active,
    Paused,
    Done,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSnapshot {
    pub session_id: String,
    pub objective: String,
    pub status: GoalStatus,
    pub turns: u32,
    pub max_turns: u32,
    pub reason: String,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
    /// Active time accumulated at the last provider checkpoint, excluding pauses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_since: Option<u64>,
}

impl GoalSnapshot {
    pub fn elapsed_seconds_at(&self, now: u64) -> Option<u64> {
        if let Some(elapsed) = self.elapsed_seconds {
            let running = if self.status == GoalStatus::Active {
                self.active_since
                    .map(|since| now.saturating_sub(since))
                    .unwrap_or(0)
            } else {
                0
            };
            Some(elapsed.saturating_add(running))
        } else {
            self.started_at
                .map(|started| self.completed_at.unwrap_or(now).saturating_sub(started))
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum GoalAction {
    Get,
    Start {
        objective: String,
        #[serde(default)]
        attachments: Vec<crate::model::MessageAttachment>,
    },
    Pause,
    Resume,
    Clear,
}

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub enum RetentionPeriod {
    Days30,
    #[default]
    Days90,
    Days180,
}

impl RetentionPeriod {
    pub const ALL: [Self; 3] = [Self::Days30, Self::Days90, Self::Days180];

    pub fn days(self) -> u64 {
        match self {
            Self::Days30 => 30,
            Self::Days90 => 90,
            Self::Days180 => 180,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionCleanupPreview {
    pub before: u64,
    pub session_ids: Vec<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum SessionCleanupOutcome {
    Deleted { session_ids: Vec<Uuid> },
    Skipped,
}

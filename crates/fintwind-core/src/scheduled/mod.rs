pub(crate) mod mcp;
pub(crate) mod runner;
pub(crate) mod store;
pub(crate) mod tools;

use fintwind_protocol::scheduled_task::{ScheduledTaskDraft, ScheduledTaskId};
use fintwind_protocol::submission::SubmissionRecord;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) use store::ScheduledStore;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum OccurrenceState {
    Queued,
    Preparing,
    Settled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Occurrence {
    pub id: Uuid,
    pub task_id: ScheduledTaskId,
    pub due_at: u64,
    pub manual: bool,
    pub owner: Uuid,
    pub draft: ScheduledTaskDraft,
    pub state: OccurrenceState,
    pub session_id: Option<Uuid>,
    pub runtime_id: Option<Uuid>,
    pub submission: Option<SubmissionRecord>,
}

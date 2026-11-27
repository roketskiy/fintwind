//! Scheduled task persistence, runner loop, and OpenCode client-hosted MCP.

pub mod mcp;
pub mod runner;
pub mod store;
pub mod tools;

use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Weak};

use fintwind_protocol::scheduled_task::{
    DispatchOutcome, ScheduledTask, ScheduledTaskDraft, ScheduledTaskId, ScheduledTurnAdmission,
};
use fintwind_protocol::submission::SubmissionRecord;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use store::ScheduledStore;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OccurrenceState {
    Queued,
    Preparing,
    Settled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
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

pub struct ScheduledPreparation {
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    pub submission: SubmissionRecord,
    pub admission: ScheduledTurnAdmission,
}

pub enum ScheduledAdmission {
    Deferred,
    Rejected(String),
    Interrupted {
        session_id: Uuid,
        submission_id: Uuid,
        reason: String,
    },
    Admitted(Box<ScheduledPreparation>),
}

pub trait ScheduledCreateHost: Send + Sync + 'static {
    fn create_scheduled_task_from_agent(
        &self,
        session_id: Uuid,
        runtime_id: Uuid,
        input: tools::CreateTask,
    ) -> anyhow::Result<ScheduledTask>;
}

pub trait ScheduledBackend: Send + Sync + 'static {
    type Channel: Send + Sync + 'static;

    fn scheduled_store(&self) -> &Arc<ScheduledStore>;
    fn scheduled_runner(&self) -> &ScheduledRunner;
    fn scheduled_owner(&self) -> Uuid;
    fn scheduled_channel(&self) -> Option<Self::Channel>;
    fn scheduled_notify_tasks(&self);
    fn scheduled_notify_catalog(&self);
    fn scheduled_preparation_is_live(&self, submission_id: Uuid) -> bool;
    fn scheduled_admit(
        &self,
        channel: &Self::Channel,
        occurrence: &Occurrence,
        owner: Weak<()>,
    ) -> anyhow::Result<ScheduledAdmission>;
    fn scheduled_launch(
        &self,
        channel: &Self::Channel,
        preparation: &ScheduledPreparation,
        owner: Weak<()>,
    ) -> anyhow::Result<()>;
    fn scheduled_dispatch(
        &self,
        channel: &Self::Channel,
        preparation: &ScheduledPreparation,
        owner: Weak<()>,
    ) -> DispatchOutcome;
    fn scheduled_abandon(
        &self,
        channel: &Self::Channel,
        preparation: &ScheduledPreparation,
        owner: Weak<()>,
    );
    fn scheduled_resolve_interrupted(&self, submission: &SubmissionRecord) -> DispatchOutcome;
}

pub struct ScheduledRunner {
    pub stop: Arc<AtomicBool>,
    pub thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    pub done: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    pub cursor: AtomicUsize,
}

impl Default for ScheduledRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduledRunner {
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
            done: Mutex::new(None),
            cursor: AtomicUsize::new(0),
        }
    }

    pub fn is_stopping(&self) -> bool {
        self.stop.load(std::sync::atomic::Ordering::SeqCst)
    }
}

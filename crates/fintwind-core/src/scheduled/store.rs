use std::sync::Arc;

use anyhow::{Context, Result, bail};
use fintwind_protocol::scheduled_task::{
    DispatchOutcome, DispatchRecord, FIXED_TIME_GRACE_SECS, PendingOccurrence, Recurrence,
    ScheduledTask, ScheduledTaskDraft, ScheduledTaskEdit, ScheduledTaskId, TaskTarget,
};
use fintwind_protocol::submission::SubmissionRecord;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Occurrence, OccurrenceState};
use crate::persistence::StateStore;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredTask {
    title: String,
    prompt: String,
    schedule: Recurrence,
    target: TaskTarget,
    created_at: u64,
    updated_at: u64,
    last: Option<DispatchRecord>,
    run_count: u64,
}

impl StoredTask {
    fn draft(&self, enabled: bool) -> ScheduledTaskDraft {
        ScheduledTaskDraft {
            title: self.title.clone(),
            prompt: self.prompt.clone(),
            enabled,
            schedule: self.schedule,
            target: self.target.clone(),
        }
    }
}

pub(crate) struct ScheduledStore {
    state: Arc<StateStore>,
}

impl ScheduledStore {
    pub(crate) fn new(state: Arc<StateStore>) -> Self {
        Self { state }
    }

    fn connection(&self) -> Result<Connection> {
        let connection = self.state.open()?;
        connection.execute_batch("PRAGMA busy_timeout = 5000;")?;
        Ok(connection)
    }

    pub(crate) fn list(&self) -> Result<Vec<ScheduledTask>> {
        let connection = self.connection()?;
        let mut pending = self.open_pending(&connection)?;
        let mut statement = connection.prepare(
            "SELECT id, revision, enabled, next_due_at, data FROM scheduled_tasks ORDER BY id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut tasks = Vec::with_capacity(rows.len());
        for (id, revision, enabled, next_due_at, data) in rows {
            let id = parse_id(&id)?;
            tasks.push(decode_task(
                id,
                revision,
                enabled,
                next_due_at,
                &data,
                pending.remove(&id),
            )?);
        }
        tasks.sort_by_key(|task| (task.created_at, task.id.0));
        Ok(tasks)
    }

    pub(crate) fn get(&self, id: ScheduledTaskId) -> Result<Option<ScheduledTask>> {
        let connection = self.connection()?;
        self.read_task(&connection, id)
    }

    pub(crate) fn save(&self, edit: ScheduledTaskEdit, now: u64) -> Result<ScheduledTask> {
        match edit {
            ScheduledTaskEdit::Create { draft } => {
                draft.validate().map_err(anyhow::Error::msg)?;
                let next_due_at = next_due(&draft, now)?;
                let stored = StoredTask {
                    title: draft.title.clone(),
                    prompt: draft.prompt.clone(),
                    schedule: draft.schedule,
                    target: draft.target.clone(),
                    created_at: now,
                    updated_at: now,
                    last: None,
                    run_count: 0,
                };
                let data = serde_json::to_string(&stored)?;
                let id = ScheduledTaskId(Uuid::new_v4());
                let connection = self.connection()?;
                connection.execute(
                    "INSERT INTO scheduled_tasks(id, revision, enabled, next_due_at, data)
                     VALUES(?1, 0, ?2, ?3, ?4)",
                    params![
                        id.0.to_string(),
                        flag(draft.enabled),
                        optional_time(next_due_at)?,
                        data
                    ],
                )?;
                self.created(&connection, id)
            }
            ScheduledTaskEdit::Update {
                id,
                expected_revision,
                draft,
            } => {
                draft.validate().map_err(anyhow::Error::msg)?;
                let mut connection = self.connection()?;
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let Some((revision, enabled, next_due_at, mut stored)) =
                    self.read_task_columns(&transaction, id)?
                else {
                    bail!("This scheduled task no longer exists");
                };
                if revision != expected_revision {
                    bail!("This scheduled task was changed by another client");
                }
                let next_due_at = if !draft.enabled {
                    None
                } else if stored.schedule != draft.schedule || !enabled {
                    next_due(&draft, now)?
                } else {
                    next_due_at
                };
                stored.title = draft.title.clone();
                stored.prompt = draft.prompt.clone();
                stored.schedule = draft.schedule;
                stored.target = draft.target.clone();
                stored.updated_at = now;
                let changed = transaction.execute(
                    "UPDATE scheduled_tasks
                     SET revision = ?2, enabled = ?3, next_due_at = ?4, data = ?5
                     WHERE id = ?1 AND revision = ?6",
                    params![
                        id.0.to_string(),
                        revision as i64 + 1,
                        flag(draft.enabled),
                        optional_time(next_due_at)?,
                        serde_json::to_string(&stored)?,
                        revision
                    ],
                )?;
                if changed != 1 {
                    bail!("This scheduled task was changed by another client");
                }
                if !draft.enabled {
                    cancel_queued(&transaction, id)?;
                }
                let task = self
                    .read_task(&transaction, id)?
                    .context("the saved scheduled task disappeared")?;
                transaction.commit()?;
                Ok(task)
            }
        }
    }

    pub(crate) fn delete(&self, id: ScheduledTaskId, expected_revision: u64) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "DELETE FROM scheduled_tasks WHERE id = ?1 AND revision = ?2",
            params![id.0.to_string(), expected_revision],
        )?;
        if changed != 1 {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM scheduled_tasks WHERE id = ?1)",
                params![id.0.to_string()],
                |row| row.get(0),
            )?;
            if exists {
                bail!("This scheduled task was changed by another client");
            }
            bail!("This scheduled task no longer exists");
        }
        transaction.execute(
            "DELETE FROM scheduled_occurrences WHERE task_id = ?1 AND state <> 'preparing'",
            params![id.0.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn claim_due(&self, now: u64, owner: Uuid, limit: usize) -> Result<Vec<Occurrence>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT id, next_due_at, data FROM scheduled_tasks
             WHERE enabled = 1 AND next_due_at IS NOT NULL AND next_due_at <= ?1
             ORDER BY next_due_at",
        )?;
        let rows = statement
            .query_map(params![time(now)?], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut claimed = Vec::new();
        for (id, due_at, data) in rows {
            if claimed.len() == limit {
                break;
            }
            let id = parse_id(&id)?;
            if has_open_occurrence(&transaction, id)? {
                continue;
            }
            let mut stored: StoredTask = serde_json::from_str(&data)?;
            let due_at = seconds(due_at)?;
            if missed(now, due_at, stored.schedule) {
                stored.last = Some(DispatchRecord {
                    due_at,
                    at: now,
                    outcome: DispatchOutcome::Missed,
                });
                let one_shot = stored.schedule.is_once();
                let next = if one_shot {
                    None
                } else {
                    stored.schedule.next_due(now).ok()
                };
                update_task_state(&transaction, id, !one_shot, next, &stored)?;
                continue;
            }
            let occurrence = Occurrence {
                id: Uuid::new_v4(),
                task_id: id,
                due_at,
                manual: false,
                owner,
                draft: stored.draft(true),
                state: OccurrenceState::Queued,
                session_id: None,
                runtime_id: None,
                submission: None,
            };
            insert_occurrence(&transaction, &occurrence)?;
            claimed.push(occurrence);
        }
        transaction.commit()?;
        Ok(claimed)
    }

    pub(crate) fn claim_manual(
        &self,
        id: ScheduledTaskId,
        now: u64,
        owner: Uuid,
    ) -> Result<Occurrence> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((enabled, _, stored)) = self.read_task_view(&transaction, id)? else {
            bail!("This scheduled task no longer exists");
        };
        if has_open_occurrence(&transaction, id)? {
            bail!("This scheduled task already has a pending run");
        }
        let occurrence = Occurrence {
            id: Uuid::new_v4(),
            task_id: id,
            due_at: now,
            manual: true,
            owner,
            draft: stored.draft(enabled),
            state: OccurrenceState::Queued,
            session_id: None,
            runtime_id: None,
            submission: None,
        };
        insert_occurrence(&transaction, &occurrence)?;
        transaction.commit()?;
        Ok(occurrence)
    }

    pub(crate) fn open_occurrences(&self) -> Result<Vec<Occurrence>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT data FROM scheduled_occurrences WHERE state <> 'settled' ORDER BY due_at",
        )?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.iter()
            .map(|data| Ok(serde_json::from_str(data)?))
            .collect()
    }

    pub(crate) fn save_preparation(
        &self,
        id: Uuid,
        session_id: Uuid,
        runtime_id: Uuid,
        submission: SubmissionRecord,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut occurrence) = read_occurrence(&transaction, id)? else {
            bail!("This scheduled run no longer exists");
        };
        if occurrence.state == OccurrenceState::Preparing
            && occurrence.submission.as_ref().is_some_and(|saved| {
                saved.receipt.id == submission.receipt.id
                    && saved.session_id == submission.session_id
            })
        {
            return Ok(());
        }
        if occurrence.state != OccurrenceState::Queued {
            bail!("This scheduled run was already started");
        }
        if submission.session_id != session_id {
            bail!("The admitted submission belongs to another session");
        }
        occurrence.state = OccurrenceState::Preparing;
        occurrence.session_id = Some(session_id);
        occurrence.runtime_id = Some(runtime_id);
        occurrence.submission = Some(submission);
        let changed = transaction.execute(
            "UPDATE scheduled_occurrences SET state = 'preparing', data = ?2
             WHERE id = ?1 AND state = 'queued'",
            params![id.to_string(), serde_json::to_string(&occurrence)?],
        )?;
        if changed != 1 {
            bail!("This scheduled run was already started");
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn settle(&self, id: Uuid, outcome: DispatchOutcome, now: u64) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut occurrence) = read_occurrence(&transaction, id)? else {
            return Ok(());
        };
        if occurrence.state == OccurrenceState::Settled {
            return Ok(());
        }
        let task = self.read_task_view(&transaction, occurrence.task_id)?;
        occurrence.state = OccurrenceState::Settled;
        transaction.execute(
            "UPDATE scheduled_occurrences SET state = 'settled', data = ?2
             WHERE id = ?1 AND state <> 'settled'",
            params![id.to_string(), serde_json::to_string(&occurrence)?],
        )?;
        match task {
            Some((enabled, current_due, mut stored)) => {
                if !matches!(outcome, DispatchOutcome::Missed) {
                    stored.run_count = stored.run_count.saturating_add(1);
                }
                stored.last = Some(DispatchRecord {
                    due_at: occurrence.due_at,
                    at: now,
                    outcome,
                });
                let next_due_at = if occurrence.manual {
                    current_due
                } else if enabled {
                    let one_shot = stored.schedule.is_once();
                    if one_shot {
                        None
                    } else {
                        stored.schedule.next_due(now).ok()
                    }
                } else {
                    None
                };
                let enabled_next = enabled && (occurrence.manual || !stored.schedule.is_once());
                update_task_state(
                    &transaction,
                    occurrence.task_id,
                    enabled_next,
                    next_due_at,
                    &stored,
                )?;
            }
            None => {
                transaction.execute(
                    "DELETE FROM scheduled_occurrences WHERE id = ?1",
                    params![id.to_string()],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    fn created(&self, connection: &Connection, id: ScheduledTaskId) -> Result<ScheduledTask> {
        self.read_task(connection, id)?
            .context("the created scheduled task disappeared")
    }

    fn read_task(
        &self,
        connection: &Connection,
        id: ScheduledTaskId,
    ) -> Result<Option<ScheduledTask>> {
        let pending = pending_for(connection, id)?;
        let row = connection
            .query_row(
                "SELECT revision, enabled, next_due_at, data FROM scheduled_tasks WHERE id = ?1",
                params![id.0.to_string()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(revision, enabled, next_due_at, data)| {
            decode_task(id, revision, enabled, next_due_at, &data, pending)
        })
        .transpose()
    }

    fn read_task_columns(
        &self,
        connection: &Connection,
        id: ScheduledTaskId,
    ) -> Result<Option<(u64, bool, Option<u64>, StoredTask)>> {
        let row = connection
            .query_row(
                "SELECT revision, enabled, next_due_at, data FROM scheduled_tasks WHERE id = ?1",
                params![id.0.to_string()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(revision, enabled, next_due_at, data)| {
            Ok((
                revision as u64,
                enabled != 0,
                next_due_at.map(seconds).transpose()?,
                serde_json::from_str(&data)?,
            ))
        })
        .transpose()
    }

    fn read_task_view(
        &self,
        connection: &Connection,
        id: ScheduledTaskId,
    ) -> Result<Option<(bool, Option<u64>, StoredTask)>> {
        Ok(self
            .read_task_columns(connection, id)?
            .map(|(_, enabled, next_due_at, stored)| (enabled, next_due_at, stored)))
    }

    fn open_pending(
        &self,
        connection: &Connection,
    ) -> Result<std::collections::HashMap<ScheduledTaskId, PendingOccurrence>> {
        let mut statement = connection.prepare(
            "SELECT id, task_id, due_at, state, json_extract(data, '$.sessionId')
             FROM scheduled_occurrences WHERE state <> 'settled'",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut pending = std::collections::HashMap::new();
        for (id, task_id, due_at, state, session_id) in rows {
            pending.insert(
                parse_id(&task_id)?,
                PendingOccurrence {
                    id: Uuid::parse_str(&id)?,
                    due_at: seconds(due_at)?,
                    session_id: session_id.map(|id| Uuid::parse_str(&id)).transpose()?,
                    preparing: state == "preparing",
                },
            );
        }
        Ok(pending)
    }
}

fn decode_task(
    id: ScheduledTaskId,
    revision: i64,
    enabled: i64,
    next_due_at: Option<i64>,
    data: &str,
    pending: Option<PendingOccurrence>,
) -> Result<ScheduledTask> {
    let stored: StoredTask = serde_json::from_str(data)?;
    Ok(ScheduledTask {
        id,
        revision: revision as u64,
        title: stored.title,
        prompt: stored.prompt,
        enabled: enabled != 0,
        schedule: stored.schedule,
        target: stored.target,
        next_due_at: next_due_at.map(seconds).transpose()?,
        created_at: stored.created_at,
        updated_at: stored.updated_at,
        pending,
        last: stored.last,
        run_count: stored.run_count,
    })
}

fn pending_for(connection: &Connection, id: ScheduledTaskId) -> Result<Option<PendingOccurrence>> {
    let row = connection
        .query_row(
            "SELECT id, due_at, state, json_extract(data, '$.sessionId')
             FROM scheduled_occurrences WHERE task_id = ?1 AND state <> 'settled'",
            params![id.0.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    row.map(|(id, due_at, state, session_id)| {
        Ok(PendingOccurrence {
            id: Uuid::parse_str(&id)?,
            due_at: seconds(due_at)?,
            session_id: session_id.map(|id| Uuid::parse_str(&id)).transpose()?,
            preparing: state == "preparing",
        })
    })
    .transpose()
}

fn read_occurrence(connection: &Connection, id: Uuid) -> Result<Option<Occurrence>> {
    let data: Option<String> = connection
        .query_row(
            "SELECT data FROM scheduled_occurrences WHERE id = ?1",
            params![id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    data.map(|data| serde_json::from_str::<Occurrence>(&data))
        .transpose()
        .map_err(Into::into)
}

fn has_open_occurrence(connection: &Connection, id: ScheduledTaskId) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM scheduled_occurrences WHERE task_id = ?1 AND state <> 'settled')",
        params![id.0.to_string()],
        |row| row.get(0),
    )?)
}

fn insert_occurrence(connection: &Connection, occurrence: &Occurrence) -> Result<()> {
    connection.execute(
        "INSERT INTO scheduled_occurrences(id, task_id, due_at, state, manual, data)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            occurrence.id.to_string(),
            occurrence.task_id.0.to_string(),
            time(occurrence.due_at)?,
            state_name(occurrence.state),
            flag(occurrence.manual),
            serde_json::to_string(occurrence)?,
        ],
    )?;
    Ok(())
}

fn update_task_state(
    connection: &Connection,
    id: ScheduledTaskId,
    enabled: bool,
    next_due_at: Option<u64>,
    stored: &StoredTask,
) -> Result<()> {
    connection.execute(
        "UPDATE scheduled_tasks SET enabled = ?2, next_due_at = ?3, data = ?4 WHERE id = ?1",
        params![
            id.0.to_string(),
            flag(enabled),
            optional_time(next_due_at)?,
            serde_json::to_string(stored)?
        ],
    )?;
    Ok(())
}

fn cancel_queued(connection: &Connection, id: ScheduledTaskId) -> Result<()> {
    connection.execute(
        "DELETE FROM scheduled_occurrences WHERE task_id = ?1 AND state = 'queued'",
        params![id.0.to_string()],
    )?;
    Ok(())
}

fn next_due(draft: &ScheduledTaskDraft, now: u64) -> Result<Option<u64>> {
    if draft.enabled {
        let due = draft.schedule.next_due(now).map_err(anyhow::Error::msg)?;
        Ok(Some(due))
    } else {
        Ok(None)
    }
}

fn missed(now: u64, due_at: u64, schedule: Recurrence) -> bool {
    matches!(
        schedule,
        Recurrence::AtTime { .. } | Recurrence::Once { .. }
    ) && due_at.saturating_add(FIXED_TIME_GRACE_SECS) < now
}

fn state_name(state: OccurrenceState) -> &'static str {
    match state {
        OccurrenceState::Queued => "queued",
        OccurrenceState::Preparing => "preparing",
        OccurrenceState::Settled => "settled",
    }
}

fn flag(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn time(value: u64) -> Result<i64> {
    i64::try_from(value).context("timestamp is outside the database range")
}

fn optional_time(value: Option<u64>) -> Result<Option<i64>> {
    value.map(time).transpose()
}

fn seconds(value: i64) -> Result<u64> {
    u64::try_from(value).context("stored timestamp is negative")
}

fn parse_id(value: &str) -> Result<ScheduledTaskId> {
    Ok(ScheduledTaskId(Uuid::parse_str(value)?))
}

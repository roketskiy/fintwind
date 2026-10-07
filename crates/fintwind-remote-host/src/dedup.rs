//! Durable operation acceptance records (architecture document: 接受记录
//! 先持久化，再执行副作用).
//!
//! One small SQLite table beside the host identity key. For every remote
//! `operation_id` the host has ever accepted it remembers the frozen
//! summary, the target, the TTL window, and the final state — so a device
//! retry after a crash gets the recorded answer instead of a re-executed
//! side effect, and a retry that tries to change the summary is refused.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

use fintwind_remote_protocol::{OperationState, RejectCode};

/// How long an accepted record outlives its request window (M0: the
/// dedup record covers the full request validity, never aged out early
/// while a retry could still arrive).
pub const RECORD_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub struct OperationStore {
    /// `rusqlite::Connection` is not `Sync`; the mutex is the honest
    /// boundary between the worker thread and the sweeper.
    connection: Mutex<Connection>,
}

pub struct AcceptCheck {
    pub state: OperationState,
    /// Some when the operation was already accepted and the caller should
    /// answer from this record instead of executing again.
    pub replay: bool,
}

impl OperationStore {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS operations (
                operation_id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                summary TEXT NOT NULL,
                device_id TEXT NOT NULL,
                session_id TEXT,
                accepted_at_ms INTEGER NOT NULL,
                expires_at_ms INTEGER NOT NULL,
                state TEXT NOT NULL,
                reject_code TEXT,
                detail TEXT
            ) STRICT;
            CREATE INDEX IF NOT EXISTS operations_expiry ON operations(accepted_at_ms);",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// The admission gate for a remote operation. Returns the recorded
    /// outcome for a known id (idempotent replay), an explicit refusal
    /// for a summary-changing retry or an expired window, or admission
    /// for a fresh id — **after** which the caller persists the acceptance
    /// and only then performs the side effect.
    pub fn check(
        &self,
        operation_id: &str,
        kind: &str,
        summary: &str,
        device_id: &str,
        expires_at_ms: u64,
    ) -> anyhow::Result<AcceptCheck> {
        let connection = &self.connection.lock();
        let now = now_ms();
        if expires_at_ms < now {
            return Ok(AcceptCheck {
                state: OperationState::Expired,
                replay: false,
            });
        }
        let row: Option<(String, String, String, Option<String>, String)> = connection
            .query_row(
                "SELECT kind, summary, state, reject_code, device_id FROM operations WHERE operation_id = ?1",
                params![operation_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        match row {
            Some((stored_kind, stored_summary, stored_state, stored_reject_code, stored_device)) => {
                if stored_kind != kind || stored_summary != summary {
                    // A retry must never mutate the operation it retries.
                    return Ok(AcceptCheck {
                        state: OperationState::Rejected {
                            code: RejectCode::Malformed,
                        },
                        replay: true,
                    });
                }
                if stored_device != device_id {
                    // An operation belongs to the device that raised it;
                    // a different device replaying the id learns nothing.
                    return Ok(AcceptCheck {
                        state: OperationState::Rejected {
                            code: RejectCode::AuthFailed,
                        },
                        replay: true,
                    });
                }
                let state = match stored_state.as_str() {
                    "accepted" => OperationState::Accepted,
                    "executed" => OperationState::Executed,
                    "failed" => OperationState::Rejected {
                        code: stored_reject_code
                            .as_deref()
                            .and_then(|code| serde_json::from_str(code).ok())
                            .unwrap_or(RejectCode::Malformed),
                    },
                    _ => OperationState::Unknown,
                };
                Ok(AcceptCheck {
                    state,
                    replay: true,
                })
            }
            None => Ok(AcceptCheck {
                state: OperationState::Accepted,
                replay: false,
            }),
        }
    }

    /// Persist acceptance **before** any side effect. A failure here
    /// refuses the operation outright: without the durable record the
    /// host cannot answer a retry honestly.
    pub fn record_accepted(
        &self,
        operation_id: &str,
        kind: &str,
        summary: &str,
        device_id: &str,
        session_id: Option<&str>,
        expires_at_ms: u64,
    ) -> anyhow::Result<()> {
        self.connection.lock().execute(
            "INSERT INTO operations
                 (operation_id, kind, summary, device_id, session_id,
                  accepted_at_ms, expires_at_ms, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'accepted')",
            params![
                operation_id,
                kind,
                summary,
                device_id,
                session_id,
                now_ms() as i64,
                expires_at_ms as i64,
            ],
        )?;
        Ok(())
    }

    /// Freeze the final answer. The full state is passed so a rejection
    /// keeps its code: the replay must answer exactly what was answered
    /// the first time, not a flattened "failed".
    pub fn record_finished(
        &self,
        operation_id: &str,
        state: &OperationState,
        detail: Option<&str>,
    ) -> anyhow::Result<()> {
        let (state_text, reject_code) = match state {
            OperationState::Executed => ("executed", None),
            OperationState::Rejected { code } => (
                "failed",
                Some(serde_json::to_string(code).expect("reject code serializes")),
            ),
            OperationState::Accepted => ("accepted", None),
            OperationState::Unknown => ("unknown", None),
            OperationState::Expired => ("expired", None),
        };
        self.connection.lock().execute(
            "UPDATE operations SET state = ?2, reject_code = ?3, detail = ?4 WHERE operation_id = ?1",
            params![operation_id, state_text, reject_code, detail],
        )?;
        Ok(())
    }

    /// Drop records past their retention window. A retry arriving after
    /// this gets `Unknown`, which the protocol defines as "query first,
    /// never blindly retry" — the honest answer at that distance.
    pub fn sweep(&self) -> anyhow::Result<()> {
        self.connection.lock().execute(
            "DELETE FROM operations WHERE accepted_at_ms < ?1",
            params![(now_ms() as i64) - (RECORD_RETENTION.as_millis() as i64)],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> OperationStore {
        let path = std::env::temp_dir().join(format!(
            "relay-dedup-test-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        OperationStore::open(&path).expect("dedup store opens")
    }

    fn fresh_expiry() -> u64 {
        now_ms() + 60_000
    }

    /// Ways this store can fail: a crash between accept and execute
    /// resurrects the side effect on retry, a retry changes the summary
    /// undetected, an expired request slips through, or an executed
    /// operation executes twice.
    #[test]
    fn retry_after_accept_replays_not_reexecutes() {
        let store = store();
        let check = store
            .check("op-1", "SendPrompt", "s", "d", fresh_expiry())
            .unwrap();
        assert!(!check.replay);
        store.record_accepted("op-1", "SendPrompt", "s", "d", None, fresh_expiry()).unwrap();
        // Crash before record_finished: the retry must see Accepted and
        // must not be admitted for execution again.
        let replay = store
            .check("op-1", "SendPrompt", "s", "d", fresh_expiry())
            .unwrap();
        assert!(replay.replay);
        assert!(matches!(replay.state, OperationState::Accepted));
    }

    #[test]
    fn summary_changing_retry_is_refused() {
        let store = store();
        store
            .check("op-1", "SendPrompt", "prompt A", "d", fresh_expiry())
            .unwrap();
        store
            .record_accepted("op-1", "SendPrompt", "prompt A", "d", None, fresh_expiry())
            .unwrap();
        let tampered = store
            .check("op-1", "SendPrompt", "prompt B", "d", fresh_expiry())
            .unwrap();
        assert!(tampered.replay);
        assert!(matches!(
            tampered.state,
            OperationState::Rejected {
                code: RejectCode::Malformed
            }
        ));
    }

    #[test]
    fn expired_request_is_rejected_fresh() {
        let store = store();
        let check = store
            .check("op-2", "CancelTurn", "s", "d", now_ms() - 1)
            .unwrap();
        assert!(!check.replay);
        assert!(matches!(check.state, OperationState::Expired));
    }

    #[test]
    fn executed_operation_reports_executed() {
        let store = store();
        store
            .check("op-3", "RenameSession", "s", "d", fresh_expiry())
            .unwrap();
        store
            .record_accepted("op-3", "RenameSession", "s", "d", None, fresh_expiry())
            .unwrap();
        store
            .record_finished("op-3", &OperationState::Executed, None)
            .unwrap();
        let replay = store
            .check("op-3", "RenameSession", "s", "d", fresh_expiry())
            .unwrap();
        assert!(replay.replay);
        assert!(matches!(replay.state, OperationState::Executed));
    }

    #[test]
    fn rejection_keeps_its_code_across_replays() {
        let store = store();
        store
            .check("op-4", "AttachSession", "s", "d", fresh_expiry())
            .unwrap();
        store
            .record_accepted("op-4", "AttachSession", "s", "d", None, fresh_expiry())
            .unwrap();
        store
            .record_finished(
                "op-4",
                &OperationState::Rejected {
                    code: RejectCode::UnknownRoute,
                },
                Some("workspace unreachable"),
            )
            .unwrap();
        let replay = store
            .check("op-4", "AttachSession", "s", "d", fresh_expiry())
            .unwrap();
        assert!(replay.replay);
        match replay.state {
            OperationState::Rejected { code } => {
                assert_eq!(
                    code,
                    RejectCode::UnknownRoute,
                    "the replayed answer must carry the original code"
                );
            }
            other => panic!("expected the recorded rejection, got {other:?}"),
        }
    }
}

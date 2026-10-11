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
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use fintwind_remote_protocol::{OperationState, RejectCode};

/// How long an accepted record outlives its request window (M0: the
/// dedup record covers the full request validity, never aged out early
/// while a retry could still arrive).
pub const RECORD_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// The host's own bound on how long a remote operation may stay
/// retryable. The device proposes a window in `expires_at_ms`; the host
/// refuses anything longer, so a retry can never extend an operation's
/// life by waiting (architecture: the id binds the TTL, a retry may not
/// change it). Retention is an order of magnitude past this, so a
/// recorded answer always outlives every window it could be replayed
/// under.
pub const MAX_OPERATION_TTL_MS: u64 = 10 * 60 * 1000;

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

/// One stored operation record, as a query reads it.
#[derive(Clone, Debug)]
pub struct OperationRecord {
    pub kind: String,
    pub summary: String,
    /// The device that raised the operation. A query from any other
    /// device is answered `Unknown` — the record's existence is not
    /// another device's business.
    pub device_id: String,
    pub state: OperationState,
    pub detail: Option<String>,
}

/// An accepted cancellation whose ending the host still owes a device.
#[derive(Clone, Copy, Debug)]
pub struct PendingCancelRecord {
    /// The cancel operation the device raised, and that this host accepted.
    pub cancel_operation_id: Uuid,
    /// The prompt that cancellation is about.
    pub prompt_operation_id: Uuid,
    /// The device that raised it — the only one its ending may go to.
    pub device_id: Uuid,
    /// The grant under which the cancel was accepted. It remains usable for
    /// recovery queries after the device is re-paired.
    pub grant_version: Option<u64>,
}

/// Decode a stored state row into the wire state. Shared by the replay
/// path and the query path so both answer identically.
fn decode_state(state: &str, reject_code: Option<&str>) -> OperationState {
    match state {
        "accepted" => OperationState::Accepted,
        "executed" => OperationState::Executed,
        "failed" => OperationState::Rejected {
            code: reject_code
                .and_then(|code| serde_json::from_str(code).ok())
                .unwrap_or(RejectCode::Malformed),
        },
        "expired" => OperationState::Expired,
        _ => OperationState::Unknown,
    }
}

/// A stable hash of one prompt's text, for binding an operation record to
/// its body.
///
/// The content hash one prompt-carrying operation is bound to.
///
/// A digest, never the text: the store binds an operation id to the prompt
/// it carried so a retry with different text is refused rather than
/// silently adopting the first body, and the prompt itself is never
/// persisted, logged, or read back out.
pub fn prompt_hash(prompt: &str) -> String {
    // SHA-256 over the trimmed prompt, hex-encoded. The comparison is
    // equality of digests; no plaintext ever reaches the store.
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    let digest = hasher.finalize();
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// Add any missing operation-table column, once, idempotently.
///
/// The same check-then-alter under an `IMMEDIATE` transaction that
/// `content_hash` introduced: the probe runs before the lock is taken and
/// again after it is held, so concurrent openers resolve the race without
/// ever surfacing a duplicate-column error. Existing rows keep every value
/// they had; a new column reads back `NULL` for them, which is exactly
/// what "this record predates the column" means.
fn ensure_columns(connection: &mut Connection, names: &[&str]) -> anyhow::Result<()> {
    if has_columns(connection, names)? {
        return Ok(());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if has_columns(&transaction, names)? {
        // Another opener committed the columns between the first probe and
        // this transaction's lock; nothing to add.
        transaction.commit()?;
        return Ok(());
    }
    for name in names {
        if has_columns(&transaction, &[name])? {
            continue;
        }
        // Column names come from this file's own constants, never from
        // stored data, so formatting them into the statement is safe.
        transaction.execute_batch(&format!("ALTER TABLE operations ADD COLUMN {name} TEXT"))?;
    }
    transaction.commit()?;
    Ok(())
}

fn has_columns(connection: &Connection, names: &[&str]) -> anyhow::Result<bool> {
    let mut statement = connection.prepare("PRAGMA table_info(operations)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    for column in columns {
        present.insert(column?);
    }
    Ok(names.iter().all(|name| present.contains(*name)))
}

impl OperationStore {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut connection = Connection::open(path)?;
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
                detail TEXT,
                content_hash TEXT,
                cancel_target TEXT,
                cancel_grant_version INTEGER
            ) STRICT;
            CREATE INDEX IF NOT EXISTS operations_expiry ON operations(accepted_at_ms);",
        )?;
        // A database created before content binding exists has the table
        // without the column, and `IF NOT EXISTS` above will not add it —
        // after which every read and write that names the column fails. The
        // columns are therefore added idempotently, in a transaction,
        // without touching a single existing row.
        ensure_columns(
            &mut connection,
            // `cancel_target`: the prompt a `CancelQueuedPrompt` operation is
            // about. An accepted cancel whose ending was never delivered must
            // survive this host's restart, and the target is what lets the
            // restarted host find its truth without the device replaying the
            // cancellation.
            &["content_hash", "cancel_target", "cancel_grant_version"],
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
        content_hash: Option<&str>,
    ) -> anyhow::Result<AcceptCheck> {
        let connection = &self.connection.lock();
        let now = now_ms();
        if expires_at_ms < now {
            return Ok(AcceptCheck {
                state: OperationState::Expired,
                replay: false,
            });
        }
        let row: Option<(String, String, String, Option<String>, String, Option<String>)> =
            connection
                .query_row(
                    "SELECT kind, summary, state, reject_code, device_id, content_hash FROM operations WHERE operation_id = ?1",
                    params![operation_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .optional()?;
        match row {
            Some((
                stored_kind,
                stored_summary,
                stored_state,
                stored_reject_code,
                stored_device,
                stored_hash,
            )) => {
                if stored_kind != kind || stored_summary != summary {
                    // A retry must never mutate the operation it retries.
                    return Ok(AcceptCheck {
                        state: OperationState::Rejected {
                            code: RejectCode::Malformed,
                        },
                        replay: true,
                    });
                }
                // Content binding. A prompt-carrying operation is bound to
                // its prompt's hash, so a retry that reuses the id with
                // different text is refused instead of silently adopting the
                // first body. The hash travels, never the prompt.
                //
                // Fail closed, in both directions. A stored hash that differs
                // from the request's is a different body. A stored row with
                // *no* hash predates content binding: its body cannot be
                // verified, so a prompt-carrying retry is refused rather than
                // assumed to match — the lenient reading would reinstate the
                // silent adoption this binding exists to prevent. An operation
                // that carries no prompt at all has no hash on either side and
                // is unaffected.
                let hash_matches = match (stored_hash.as_deref(), content_hash) {
                    (Some(stored), Some(incoming)) => stored == incoming,
                    (None, None) => true,
                    _ => false,
                };
                if !hash_matches {
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
                let state = decode_state(&stored_state, stored_reject_code.as_deref());
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
        content_hash: Option<&str>,
    ) -> anyhow::Result<()> {
        self.connection.lock().execute(
            "INSERT INTO operations
                 (operation_id, kind, summary, device_id, session_id,
                  accepted_at_ms, expires_at_ms, state, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'accepted', ?8)",
            params![
                operation_id,
                kind,
                summary,
                device_id,
                session_id,
                now_ms() as i64,
                expires_at_ms as i64,
                content_hash,
            ],
        )?;
        Ok(())
    }

    /// Bind an accepted `CancelQueuedPrompt` to the prompt it cancels.
    ///
    /// The cancel was accepted — the device was told so — and the host now
    /// owes it an ending even if this process dies first. The binding is
    /// what lets a restarted host find the still-accepted cancel and resolve
    /// it against the daemon's record, instead of leaving the device to
    /// replay its cancellation just to be told what happened. Returns
    /// `false` when no such operation row exists, which the caller reports
    /// as an unknown outcome rather than pretending the promise is stored.
    pub fn set_cancel_target(
        &self,
        operation_id: &str,
        target: &str,
        grant_version: u64,
    ) -> anyhow::Result<bool> {
        let changed = self.connection.lock().execute(
            "UPDATE operations SET cancel_target = ?2, cancel_grant_version = ?3
              WHERE operation_id = ?1 AND state = 'accepted'",
            params![operation_id, target, grant_version as i64],
        )?;
        Ok(changed == 1)
    }

    /// Every accepted cancellation this host has not finished delivering.
    ///
    /// These are operations recorded as `accepted` that carry a cancel
    /// target: the side effect happened (the daemon was asked), but no
    /// ending has reached the device. A host restarts with this list and
    /// resolves each entry by query — never by re-sending the cancellation.
    pub fn pending_cancel_targets(&self) -> anyhow::Result<Vec<PendingCancelRecord>> {
        let connection = self.connection.lock();
        let mut rows = connection.prepare(
            "SELECT operation_id, cancel_target, device_id, cancel_grant_version FROM operations
               WHERE kind = 'CancelQueuedPrompt'
                 AND state = 'accepted'
                 AND cancel_target IS NOT NULL",
        )?;
        let mut pending = Vec::new();
        let mut rows = rows.query([])?;
        while let Some(row) = rows.next()? {
            let cancel_id: String = row.get(0)?;
            let target: String = row.get(1)?;
            let device: String = row.get(2)?;
            let grant_version = row
                .get::<_, Option<i64>>(3)?
                .and_then(|value| u64::try_from(value).ok());
            let (Some(cancel_operation_id), Some(prompt_operation_id), Ok(device_id)) = (
                Uuid::parse_str(&cancel_id).ok(),
                Uuid::parse_str(&target).ok(),
                Uuid::parse_str(&device),
            ) else {
                // A malformed record is never silently adopted; skipping it
                // keeps the restart path alive for the well-formed ones.
                continue;
            };
            pending.push(PendingCancelRecord {
                cancel_operation_id,
                prompt_operation_id,
                device_id,
                grant_version,
            });
        }
        Ok(pending)
    }

    /// The grant version recorded with one accepted cancellation, if this
    /// host version stored it. Older accepted records return `None` and are
    /// fail-closed during recovery because their original provenance cannot
    /// be reconstructed safely.
    pub fn cancel_grant_version(&self, operation_id: &str) -> Option<u64> {
        self.connection
            .lock()
            .query_row(
                "SELECT cancel_grant_version FROM operations WHERE operation_id = ?1",
                params![operation_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten()
            .and_then(|value| u64::try_from(value).ok())
    }

    /// Read the prompt target frozen with an accepted cancellation. A replay
    /// must not be allowed to reuse the cancel operation id for a different
    /// prompt.
    pub fn cancel_target(&self, operation_id: &str) -> Option<String> {
        self.connection
            .lock()
            .query_row(
                "SELECT cancel_target FROM operations WHERE operation_id = ?1",
                params![operation_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten()
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

    /// Settle an operation only while its durable record is still accepted.
    ///
    /// Recovery and replay can race with the worker that first observed a
    /// terminal daemon state. The compare-and-set prevents the losing path
    /// from overwriting the first answer or sending the same result again.
    pub fn record_finished_if_accepted(
        &self,
        operation_id: &str,
        state: &OperationState,
        detail: Option<&str>,
    ) -> anyhow::Result<bool> {
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
        let changed = self.connection.lock().execute(
            "UPDATE operations
                SET state = ?2, reject_code = ?3, detail = ?4
              WHERE operation_id = ?1 AND state = 'accepted'",
            params![operation_id, state_text, reject_code, detail],
        )?;
        Ok(changed == 1)
    }

    /// Read one operation's record for a device's result query. Executes
    /// nothing: the stored answer is the answer, including `Accepted` for
    /// an operation whose execution never finished (a crash between
    /// acceptance and completion), which tells the device to verify rather
    /// than retry.
    pub fn lookup(&self, operation_id: &str) -> Option<OperationRecord> {
        let connection = self.connection.lock();
        let row: Option<(String, String, String, String, Option<String>)> = connection
            .query_row(
                "SELECT kind, summary, device_id, state, detail FROM operations WHERE operation_id = ?1",
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
            .optional()
            .ok()?;
        let (kind, summary, device_id, state, detail) = row?;
        let reject_code: Option<String> = connection
            .query_row(
                "SELECT reject_code FROM operations WHERE operation_id = ?1",
                params![operation_id],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();
        Some(OperationRecord {
            kind,
            summary,
            device_id,
            state: decode_state(&state, reject_code.as_deref()),
            detail,
        })
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
        let path =
            std::env::temp_dir().join(format!("relay-dedup-test-{}.sqlite3", uuid::Uuid::new_v4()));
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
            .check("op-1", "SendPrompt", "s", "d", fresh_expiry(), None)
            .unwrap();
        assert!(!check.replay);
        store
            .record_accepted("op-1", "SendPrompt", "s", "d", None, fresh_expiry(), None)
            .unwrap();
        // Crash before record_finished: the retry must see Accepted and
        // must not be admitted for execution again.
        let replay = store
            .check("op-1", "SendPrompt", "s", "d", fresh_expiry(), None)
            .unwrap();
        assert!(replay.replay);
        assert!(matches!(replay.state, OperationState::Accepted));
    }

    #[test]
    fn summary_changing_retry_is_refused() {
        let store = store();
        store
            .check(
                "op-1",
                "SendPrompt",
                "prompt A",
                "d",
                fresh_expiry(),
                Some(&prompt_hash("prompt A")),
            )
            .unwrap();
        store
            .record_accepted(
                "op-1",
                "SendPrompt",
                "prompt A",
                "d",
                None,
                fresh_expiry(),
                Some(&prompt_hash("prompt A")),
            )
            .unwrap();
        let tampered = store
            .check(
                "op-1",
                "SendPrompt",
                "prompt B",
                "d",
                fresh_expiry(),
                Some(&prompt_hash("prompt B")),
            )
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
            .check("op-2", "CancelTurn", "s", "d", now_ms() - 1, None)
            .unwrap();
        assert!(!check.replay);
        assert!(matches!(check.state, OperationState::Expired));
    }

    #[test]
    fn executed_operation_reports_executed() {
        let store = store();
        store
            .check("op-3", "RenameSession", "s", "d", fresh_expiry(), None)
            .unwrap();
        store
            .record_accepted(
                "op-3",
                "RenameSession",
                "s",
                "d",
                None,
                fresh_expiry(),
                None,
            )
            .unwrap();
        store
            .record_finished("op-3", &OperationState::Executed, None)
            .unwrap();
        let replay = store
            .check("op-3", "RenameSession", "s", "d", fresh_expiry(), None)
            .unwrap();
        assert!(replay.replay);
        assert!(matches!(replay.state, OperationState::Executed));
    }

    #[test]
    fn rejection_keeps_its_code_across_replays() {
        let store = store();
        store
            .check("op-4", "AttachSession", "s", "d", fresh_expiry(), None)
            .unwrap();
        store
            .record_accepted(
                "op-4",
                "AttachSession",
                "s",
                "d",
                None,
                fresh_expiry(),
                None,
            )
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
            .check("op-4", "AttachSession", "s", "d", fresh_expiry(), None)
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

//! Durable intent journal for daemon-owned filesystem writes.
//!
//! Design §§7.3–7.5, 13, 15 require that a repository clone (F04) or an
//! empty-project directory creation (F05) is recorded in the daemon's SQLite
//! database **before** any write or subprocess starts, keyed by a
//! caller-stable operation UUID plus an explicit environment identity. This
//! module is that record-keeping base: registering intent, guarding state
//! transitions, and refusing to overstate what is known. It performs no
//! filesystem writes, spawns no subprocesses, opens no network, exposes no
//! RPC endpoint, and renders no UI; the actual clone/mkdir execution, owner
//! liveness verification, and any recovery UI are wired up separately.
//!
//! Why a new table instead of reusing `submissions` or `scheduled_*`:
//! submissions are turn admissions bound to a session row (and deleted with
//! the session), so they cannot represent an intent that exists before any
//! project or session row does; scheduled occurrences are bound to a task
//! with at most one open row per task. The project list itself is rewritten
//! wholesale on save, so it cannot give per-operation atomic registration.
//! `filesystem_operations` is therefore a minimal independent table whose
//! primary key gives concurrent same-ID registration a single winner.
//!
//! What this table does NOT do (explicit non-goals):
//! - It does not verify environments. [`OperationEnvironment`] is currently
//!   constructible only as local; a UUID that parses is not a known or
//!   authorized environment, and this layer never authorizes routing.
//! - It does not implement automatic recovery. Crash/restart handling is a
//!   future controlled operation manager with owner/lease proof; unfinished
//!   rows stay unfinished and read back as needing reconciliation.
//! - It stores no credentials and no external text: no Git URLs or URL
//!   fragments (a token can hide in the path part where no parser can prove
//!   it absent), no stdout/stderr, no exception text, no passwords, no key
//!   paths, no proxy commands. Failures keep only a fixed small category
//!   plus an integer exit code, both safe to persist and to display.
//!
//! Invariants, enforced here rather than by callers:
//! - The environment is an explicit [`OperationEnvironment`], currently
//!   local-only. Registration rejects anything else; reads never default a
//!   missing or foreign identity to local.
//! - State moves only forward through
//!   `preparing -> running -> {succeeded, failed, unknown, cancelled}`,
//!   guarded by the registering owner plus a `(state, revision)`
//!   compare-and-swap, so a late result cannot overwrite a newer settlement.
//!   Terminal states never leave; in particular there is no path from
//!   `unknown` to a success without a future evidence-carrying reconciler,
//!   which does not exist yet.
//! - A duplicate operation ID is always rejected, even with identical
//!   contents: a retry after reconciling the old row mints a fresh ID. An ID
//!   stored under a different environment is refused without revealing
//!   whether that environment exists.
//! - Rows carry routing facts only. `target_path` is required because the
//!   design routes by it, and it is stored and returned byte-for-byte:
//!   `@`, `?`, `#`, CJK, spaces and other special characters are ordinary
//!   path data, never a signal of secrecy, so there is no character
//!   blacklist. Only what the underlying layer cannot represent is
//!   rejected — empty paths, non-absolute paths (the routing contract
//!   names a canonical final path), NUL bytes, paths that are not valid
//!   Unicode and therefore cannot round-trip through TEXT storage, and
//!   overlong values — each with an explicit error, never by silently
//!   normalizing or replacing anything. Record types deliberately
//!   implement no `Debug`, so paths cannot leak through generic logging.
//!   Callers must not pass authentication URLs or credentials as project
//!   paths; that is a caller contract, not something this layer sniffs
//!   out of legitimate filenames. There is deliberately no pid column,
//!   since a pid alone proves nothing about resource ownership after a
//!   restart.
//! - Growth is bounded by refusing new registrations past
//!   [`MAX_OPERATIONS`], inside the same transaction as the count. Nothing
//!   is ever silently evicted: `unknown` and other unfinished rows are
//!   evidence for recovery and manual reconciliation, and no delete path
//!   exists in this module. Cleanup never touches user files.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use crate::persistence::StateStore;

/// Directory creation (F05).
const KIND_MKDIR: &str = "mkdir";
/// Repository clone (F04).
const KIND_CLONE: &str = "clone";

const STATE_PREPARING: &str = "preparing";
const STATE_RUNNING: &str = "running";
const STATE_SUCCEEDED: &str = "succeeded";
const STATE_FAILED: &str = "failed";
const STATE_UNKNOWN: &str = "unknown";
const STATE_CANCELLED: &str = "cancelled";

/// Hard cap on journal rows. Past this, registration fails closed instead
/// of evicting unfinished evidence. Raising it is a deliberate decision;
/// deleting rows is a future verified manager's job, not this module's.
const MAX_OPERATIONS: u64 = 10_000;
/// Longest target path accepted.
const MAX_TARGET_PATH_LEN: usize = 32768;

/// What the operation intends to create.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationKind {
    Mkdir,
    Clone,
}

/// Lifecycle of one journaled operation.
///
/// `Preparing` is the intent row written before any effect. `Running` means
/// the owning executor started. The four terminal states never leave;
/// `Unknown` means no confirmed result exists and the effect, if any, must
/// be reconciled by inspecting the filesystem — never by re-running the
/// write, and never by an automatic transition to success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationState {
    Preparing,
    Running,
    Succeeded,
    Failed,
    Unknown,
    Cancelled,
}

/// Fixed, small, safe failure categories. Free-form external text (command
/// output, exception messages, paths) must never be persisted, so callers
/// map their failure to one of these and optionally attach the process exit
/// code. The set grows only by deliberate review, never per-incident.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureKind {
    TargetExists,
    ParentMissing,
    ParentNotWritable,
    GitNotAvailable,
    AuthRequired,
    NetworkUnreachable,
    RemoteFailed,
    Interrupted,
    Internal,
}

/// Machine-readable failure evidence: a fixed category plus an optional
/// process exit code. Nothing else.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationFailure {
    pub kind: FailureKind,
    pub exit_code: Option<i32>,
}

/// Environment that owns the target path.
///
/// Currently constructible only as local ([`OperationEnvironment::local`]):
/// this daemon's own machine. A UUID that parses is NOT a known or
/// authorized environment, and this type authorizes nothing — it only scopes
/// rows so a future verified layer can refuse to route across environments.
/// There is deliberately no constructor from network input; adding remote
/// environments requires an authenticated environment context that does not
/// exist yet. Record types implement no `Debug` so environment identities
/// cannot leak through generic logging either.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct OperationEnvironment(Uuid);

impl OperationEnvironment {
    /// This machine. The only constructor available until an authenticated
    /// remote-environment context exists.
    pub fn local() -> Self {
        Self(fintwind_protocol::model::LOCAL_ENVIRONMENT_ID)
    }

    /// Rebuilds the exact stored identity when reading a row. This preserves
    /// the bytes; it does not authorize anything and must never be fed from
    /// network input.
    fn from_stored_row(id: Uuid) -> Self {
        Self(id)
    }

    /// True only for this machine's explicit identity. Anything else —
    /// including empty or unparsable input, which cannot even reach this
    /// type — is refused, never defaulted to local.
    pub fn is_local(&self) -> bool {
        self.0 == fintwind_protocol::model::LOCAL_ENVIRONMENT_ID
    }

    pub fn id(&self) -> Uuid {
        self.0
    }
}

/// Caller-supplied intent. The ID must be stable for the caller's attempt;
/// a retry after reconciling the old row mints a fresh one. `owner` names
/// the registering instance; only that owner may transition the row, but
/// owner equality is not a liveness proof — lease verification belongs to
/// the future controlled operation manager.
pub struct NewOperation {
    pub id: Uuid,
    pub environment: OperationEnvironment,
    pub owner: Uuid,
    pub kind: OperationKind,
    pub target_path: PathBuf,
}

/// One durable journal row. No `Debug`: `target_path` is a routing fact for
/// the database, not for logs.
pub struct OperationRecord {
    pub id: Uuid,
    pub environment: OperationEnvironment,
    pub owner: Uuid,
    /// Journal schema fields: persisted and read back for row fidelity, but
    /// no consumer reads them yet; reconciliation tooling is future work.
    #[allow(dead_code)]
    pub kind: OperationKind,
    pub target_path: PathBuf,
    pub state: OperationState,
    /// Bumped on every transition; stale results lose the CAS.
    pub revision: u64,
    #[allow(dead_code)]
    pub created_at: u64,
    #[allow(dead_code)]
    pub updated_at: u64,
    pub failure: Option<OperationFailure>,
}

/// Evidence-carrying state move requested by the owning instance.
pub struct OperationTransition {
    pub owner: Uuid,
    pub id: Uuid,
    pub expected: OperationState,
    pub next: OperationState,
    pub expected_revision: u64,
    pub failure: Option<OperationFailure>,
}

/// How a reader must interpret a row. Non-terminal rows never report work
/// as running or complete: without owner/lease proof, liveness is unknown
/// and the row needs reconciliation by whoever can inspect the filesystem.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum OperationReadStatus {
    Settled(OperationState),
    NeedsReconciliation { owner: Uuid, state: OperationState },
}

/// Maps a stored row to its honest reading. Terminal states pass through;
/// anything else needs reconciliation regardless of who reads it.
pub fn read_status(record: &OperationRecord) -> OperationReadStatus {
    match record.state {
        OperationState::Preparing | OperationState::Running => {
            OperationReadStatus::NeedsReconciliation {
                owner: record.owner,
                state: record.state,
            }
        }
        settled => OperationReadStatus::Settled(settled),
    }
}

/// Registers an intent row before any write or subprocess starts.
///
/// The count check and the insert share one immediate transaction, so
/// concurrent registrations serialize: at most one winner per ID, and the
/// table never grows past [`MAX_OPERATIONS`]. Fails closed when the ID is
/// already registered (same or different contents), when the environment is
/// not local, or when inputs are invalid. Never overwrites, merges,
/// rebinds, or evicts an existing row.
pub fn register(
    store: &StateStore,
    operation: &NewOperation,
) -> anyhow::Result<OperationRecord> {
    if !operation.environment.is_local() {
        anyhow::bail!("only the local environment is supported");
    }
    let target_path = validate_target_path(&operation.target_path)?;
    // Validated above to be lossless Unicode; read it back exactly as the
    // row to store, with no lossy replacement on any platform.
    let target_text = target_path
        .to_str()
        .context("the target path cannot be stored losslessly")?;
    let now = fintwind_protocol::model::unix_time();
    let mut connection = connection(store)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM filesystem_operations", [], |row| {
            row.get(0)
        })?;
    let count = u64::try_from(count).context("the operation journal count is negative")?;
    if count >= MAX_OPERATIONS {
        anyhow::bail!("the operation journal is full; no automatic eviction exists");
    }
    let outcome = transaction.execute(
        "INSERT INTO filesystem_operations(
             id, environment_id, owner_id, kind, target_path,
             state, revision, created_at, updated_at, failure_kind, exit_code
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, NULL, NULL)",
        params![
            operation.id.to_string(),
            operation.environment.id().to_string(),
            operation.owner.to_string(),
            kind_tag(operation.kind),
            target_text,
            STATE_PREPARING,
            now as i64,
            now as i64,
        ],
    );
    match outcome {
        Ok(_) => {
            transaction.commit()?;
            Ok(OperationRecord {
                id: operation.id,
                environment: operation.environment,
                owner: operation.owner,
                kind: operation.kind,
                target_path,
                state: OperationState::Preparing,
                revision: 0,
                created_at: now,
                updated_at: now,
                failure: None,
            })
        }
        Err(error) if is_constraint_violation(&error) => {
            // The primary key made concurrent same-ID registration a single
            // winner. Diagnose without leaking: the same message whether the
            // row exists under another environment or not.
            anyhow::bail!("the operation ID is already registered")
        }
        Err(error) => Err(error.into()),
    }
}

/// Reads one row by environment and operation ID.
///
/// A missing row and a row stored under a different environment are
/// indistinguishable (`None`): cross-environment queries must not reveal
/// whether another environment's ID exists.
pub fn get(
    store: &StateStore,
    environment: &OperationEnvironment,
    id: Uuid,
) -> anyhow::Result<Option<OperationRecord>> {
    let connection = connection(store)?;
    match read(&connection, &id)? {
        Some(record) if record.environment.id() != environment.id() => Ok(None),
        record => Ok(record),
    }
}

/// Moves a row forward with owner proof plus a compare-and-swap on
/// `(state, revision)`.
///
/// The caller proves the basis it acted on (`expected`, `expected_revision`)
/// and its ownership (`owner`); any concurrent settlement wins and this call
/// fails so the loser rereads instead of overwriting. Missing and
/// foreign-environment rows share one message; a wrong owner is reported
/// distinctly so the real owner can distinguish confusion from a race.
/// Terminal states have no outgoing transition. Failure evidence is fixed
/// categories plus an exit code, kept only for failure-like outcomes.
pub fn transition(
    store: &StateStore,
    environment: &OperationEnvironment,
    request: &OperationTransition,
) -> anyhow::Result<OperationRecord> {
    if !is_allowed(request.expected, request.next) {
        anyhow::bail!(
            "the operation cannot move from {} to {}",
            state_tag(request.expected),
            state_tag(request.next)
        );
    }
    if !matches!(
        (request.next, request.failure),
        (
            OperationState::Failed | OperationState::Cancelled | OperationState::Unknown,
            _
        ) | (
            OperationState::Preparing | OperationState::Running | OperationState::Succeeded,
            None
        )
    ) {
        anyhow::bail!("only a failure-like outcome carries failure evidence");
    }
    let now = fintwind_protocol::model::unix_time();
    let connection = connection(store)?;
    let (failure_kind, exit_code) = match request.failure {
        Some(failure) => (Some(failure_kind_tag(failure.kind)), failure.exit_code),
        None => (None, None),
    };
    // The single atomic statement is the whole gate: id, environment,
    // owner, state and revision must all match, or nothing is rewritten
    // and no success is reported. Owner mismatch and concurrent conflicts
    // therefore cannot rewrite the row or be mistaken for success, and the
    // diagnosis below reveals no external values.
    let changed = connection.execute(
        "UPDATE filesystem_operations
            SET state = ?2, revision = revision + 1, updated_at = ?3,
                failure_kind = ?4, exit_code = ?5
          WHERE id = ?1 AND environment_id = ?6 AND owner_id = ?7
            AND state = ?8 AND revision = ?9",
        params![
            request.id.to_string(),
            state_tag(request.next),
            now as i64,
            failure_kind,
            exit_code,
            environment.id().to_string(),
            request.owner.to_string(),
            state_tag(request.expected),
            request.expected_revision as i64,
        ],
    )?;
    if changed == 1 {
        return read(&connection, &request.id)?.context("the operation disappeared");
    }
    let current = read(&connection, &request.id)?;
    let Some(current) = current.filter(|record| record.environment.id() == environment.id()) else {
        anyhow::bail!("the operation is not registered");
    };
    if current.owner != request.owner {
        anyhow::bail!("the operation is owned by another instance");
    }
    if current.state != request.expected {
        anyhow::bail!(
            "the operation is {}, not {}",
            state_tag(current.state),
            state_tag(request.expected)
        );
    }
    if current.revision != request.expected_revision {
        anyhow::bail!("the operation was settled by a newer result; reread it");
    }
    anyhow::bail!("the operation was changed by another writer; reread it");
}

fn validate_target_path(path: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(!path.as_os_str().is_empty(), "the target path is empty");
    anyhow::ensure!(
        path.is_absolute(),
        "the target path must be absolute on its environment"
    );
    // TEXT storage round-trips Unicode exactly; anything else would need
    // silent lossy replacement, so it is rejected explicitly instead.
    // `@`, `?`, `#`, CJK, spaces and other special characters are all
    // valid here: they are path data, not evidence of secrets.
    let text = path
        .to_str()
        .context("the target path is not valid Unicode and cannot be stored losslessly")?;
    anyhow::ensure!(!text.contains('\0'), "the target path contains NUL");
    anyhow::ensure!(
        text.len() <= MAX_TARGET_PATH_LEN,
        "the target path is too long"
    );
    Ok(path.to_path_buf())
}

fn is_allowed(from: OperationState, next: OperationState) -> bool {
    matches!(
        (from, next),
        (OperationState::Preparing, OperationState::Running)
            | (OperationState::Preparing, OperationState::Failed)
            | (OperationState::Preparing, OperationState::Unknown)
            | (OperationState::Preparing, OperationState::Cancelled)
            | (OperationState::Running, OperationState::Succeeded)
            | (OperationState::Running, OperationState::Failed)
            | (OperationState::Running, OperationState::Unknown)
            | (OperationState::Running, OperationState::Cancelled)
    )
}

fn kind_tag(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Mkdir => KIND_MKDIR,
        OperationKind::Clone => KIND_CLONE,
    }
}

fn parse_kind(tag: &str) -> anyhow::Result<OperationKind> {
    match tag {
        KIND_MKDIR => Ok(OperationKind::Mkdir),
        KIND_CLONE => Ok(OperationKind::Clone),
        _ => anyhow::bail!("the stored operation kind is unknown"),
    }
}

fn state_tag(state: OperationState) -> &'static str {
    match state {
        OperationState::Preparing => STATE_PREPARING,
        OperationState::Running => STATE_RUNNING,
        OperationState::Succeeded => STATE_SUCCEEDED,
        OperationState::Failed => STATE_FAILED,
        OperationState::Unknown => STATE_UNKNOWN,
        OperationState::Cancelled => STATE_CANCELLED,
    }
}

fn parse_state(tag: &str) -> anyhow::Result<OperationState> {
    match tag {
        STATE_PREPARING => Ok(OperationState::Preparing),
        STATE_RUNNING => Ok(OperationState::Running),
        STATE_SUCCEEDED => Ok(OperationState::Succeeded),
        STATE_FAILED => Ok(OperationState::Failed),
        STATE_UNKNOWN => Ok(OperationState::Unknown),
        STATE_CANCELLED => Ok(OperationState::Cancelled),
        _ => anyhow::bail!("the stored operation state is unknown"),
    }
}

fn failure_kind_tag(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::TargetExists => "target_exists",
        FailureKind::ParentMissing => "parent_missing",
        FailureKind::ParentNotWritable => "parent_not_writable",
        FailureKind::GitNotAvailable => "git_not_available",
        FailureKind::AuthRequired => "auth_required",
        FailureKind::NetworkUnreachable => "network_unreachable",
        FailureKind::RemoteFailed => "remote_failed",
        FailureKind::Interrupted => "interrupted",
        FailureKind::Internal => "internal",
    }
}

fn parse_failure_kind(tag: &str) -> anyhow::Result<FailureKind> {
    match tag {
        "target_exists" => Ok(FailureKind::TargetExists),
        "parent_missing" => Ok(FailureKind::ParentMissing),
        "parent_not_writable" => Ok(FailureKind::ParentNotWritable),
        "git_not_available" => Ok(FailureKind::GitNotAvailable),
        "auth_required" => Ok(FailureKind::AuthRequired),
        "network_unreachable" => Ok(FailureKind::NetworkUnreachable),
        "remote_failed" => Ok(FailureKind::RemoteFailed),
        "interrupted" => Ok(FailureKind::Interrupted),
        "internal" => Ok(FailureKind::Internal),
        _ => anyhow::bail!("the stored operation failure kind is unknown"),
    }
}

fn is_constraint_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn connection(store: &StateStore) -> anyhow::Result<Connection> {
    let connection = store.open()?;
    // Intent rows must survive a system crash, not just a process crash:
    // the executor is only allowed to act after this row is durable. Same
    // FULL escalation as submission admission. Each call opens its own
    // short-lived connection, so nothing needs restoring afterwards.
    connection.execute_batch("PRAGMA busy_timeout = 5000; PRAGMA synchronous = FULL;")?;
    Ok(connection)
}

#[allow(clippy::type_complexity)]
fn read(connection: &Connection, id: &Uuid) -> anyhow::Result<Option<OperationRecord>> {
    let mut statement = connection.prepare(
        "SELECT id, environment_id, owner_id, kind, target_path, state,
                revision, created_at, updated_at, failure_kind, exit_code
           FROM filesystem_operations WHERE id = ?1",
    )?;
    let row = statement
        .query_row(params![id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<i64>>(10)?,
            ))
        })
        .optional()?;
    row.map(decode).transpose()
}

#[allow(clippy::type_complexity)]
fn decode(
    row: (
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
        i64,
        Option<String>,
        Option<i64>,
    ),
) -> anyhow::Result<OperationRecord> {
    let (
        id,
        environment_id,
        owner_id,
        kind,
        target_path,
        state,
        revision,
        created_at,
        updated_at,
        failure_kind,
        exit_code,
    ) = row;
    let failure = match failure_kind {
        Some(tag) => Some(OperationFailure {
            kind: parse_failure_kind(&tag)?,
            exit_code: exit_code
                .map(|code| {
                    i32::try_from(code).context("the stored operation exit code is out of range")
                })
                .transpose()?,
        }),
        None => {
            if exit_code.is_some() {
                anyhow::bail!("the stored operation has an exit code without a failure kind");
            }
            None
        }
    };
    Ok(OperationRecord {
        id: Uuid::parse_str(&id).context("the stored operation id is not a UUID")?,
        environment: OperationEnvironment::from_stored_row(
            Uuid::parse_str(&environment_id)
                .context("the stored operation environment is not a UUID")?,
        ),
        owner: Uuid::parse_str(&owner_id).context("the stored operation owner is not a UUID")?,
        kind: parse_kind(&kind)?,
        // Stored rows pass the same gate as registration input: a corrupt
        // or externally written relative, empty, NUL-bearing or
        // non-Unicode target fails the read instead of being repaired,
        // narrowed, or fallen back to the local working directory.
        // Legal names — CJK, spaces, `@`, `?`, `#` and other special
        // characters — pass through unchanged.
        target_path: validate_target_path(Path::new(&target_path))
            .context("the stored operation target is invalid")?,
        state: parse_state(&state)?,
        revision: u64::try_from(revision).context("the stored operation revision is negative")?,
        created_at: u64::try_from(created_at).context("the stored operation time is negative")?,
        updated_at: u64::try_from(updated_at).context("the stored operation time is negative")?,
        failure,
    })
}

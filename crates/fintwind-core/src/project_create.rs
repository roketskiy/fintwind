//! Journaled creation of one empty project directory.
//!
//! Persist intent in the existing [`crate::operation_journal`] SQLite table
//! **before** touching the filesystem, then atomically create exactly one
//! empty directory.
//!
//! What this module does:
//! - Verifies the parent directory on this machine first
//!   (`canonicalize` + `metadata.is_dir`, no shell, no recursive creation).
//! - Re-confirms the parent read-only immediately before the single write;
//!   a missing, non-directory, or differently-resolving parent ends the
//!   attempt as a recorded parent failure with no file written.
//! - Accepts only a single path segment as the project name: empty, `.`,
//!   `..`, embedded `/`, `\`, and NUL are rejected before anything is
//!   persisted or written. CJK, spaces, `@`, `#`, and other characters the
//!   target platform accepts pass through as path data; the operating system
//!   is the final arbiter of legality and its refusal becomes a recorded
//!   failure, never a silent rename.
//! - Registers `preparing` in the existing journal keyed by the caller's
//!   stable operation UUID plus an explicit local environment and a trusted
//!   owner instance, advances to `running` with owner + `(state, revision)`
//!   compare-and-swap, and only then calls `create_dir` once.
//! - Reports the confirmed journal row: `Created` (terminal success),
//!   `Failed` (terminal failure with a fixed category), or `Unknown` (nothing
//!   is proven; reconcile, never blindly re-run). Failures never delete or
//!   clean any directory.
//!
//! What this module deliberately does NOT do (explicit non-goals, all left
//! for later work; this slice is not a complete empty-project flow):
//! - No Git initialization. There is no `init_git` parameter on purpose: a
//!   caller that wants a repository must use a separate, future journaled
//!   step. Nothing here may look like a button or API that claims to
//!   initialize Git.
//! - No remote environments. Only [`OperationEnvironment::local`] is
//!   accepted; anything else is refused, never defaulted to local. There is
//!   no network entry, no new RPC variant, and no "current machine" global.
//! - No automatic recovery, retry, or cleanup. `Unknown` rows stay in the
//!   journal as evidence; no delete path exists here. A dropped response, an
//!   interrupted process, or a restart must reconcile via
//!   [`get_project_operation`] plus a filesystem inspection — never by
//!   re-issuing the same operation ID (duplicates are rejected by the
//!   journal) and never by treating a pre-existing path as this call's
//!   success.
//! - No UI and no render-thread work. This function blocks on SQLite and the
//!   filesystem, so only a daemon background or request thread may call it;
//!   it must never run synchronously inside render.
//!
//! Current callers: none. The future daemon empty-project handler
//! (background/request thread) will own the single call site; until it
//! exists this module is compiled but uncalled, exactly like the journal
//! layer it builds on.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::bail;
use uuid::Uuid;

use crate::operation_journal::{
    self, FailureKind, NewOperation, OperationEnvironment, OperationFailure, OperationKind,
    OperationRecord, OperationState, OperationTransition,
};
use crate::persistence::StateStore;

/// Honest outcome of one journaled directory creation.
///
/// `Created` and `Failed` carry terminal journal rows whose state proves what
/// happened. `Unknown` means the effect, the record, or both cannot be
/// proven: the caller must reconcile with [`get_project_operation`] and a
/// filesystem inspection, must not retry the write, and must surface
/// "result needs verification" instead of success or failure.
pub(crate) enum ProjectCreateOutcome {
    Created(OperationRecord),
    Failed(OperationRecord),
    Unknown {
        record: OperationRecord,
        detail: &'static str,
    },
}

/// Read-only lookup of one journaled creation by environment and operation
/// ID. Reuses the existing journal; rows under another environment read back
/// as absent, and nothing is ever deleted here.
pub(crate) fn get_project_operation(
    store: &StateStore,
    environment: &OperationEnvironment,
    id: Uuid,
) -> anyhow::Result<Option<OperationRecord>> {
    operation_journal::get(store, environment, id)
}

/// Creates one empty project directory under an already-existing parent.
///
/// The caller supplies a stable operation UUID for this attempt, the explicit
/// local environment, and the trusted owner instance that will own the row.
/// A retry after reconciling the old row mints a fresh UUID; reusing an ID is
/// rejected by the journal before any filesystem write.
///
/// Order of effects: validate inputs, verify the parent, register
/// `preparing`, advance to `running`, re-confirm the parent read-only, then
/// exactly one `create_dir`, then settle terminally. A parent that no longer
/// resolves to the journaled directory ends the attempt as a recorded
/// parent failure with no file written. Any state that cannot be proven —
/// including a failed verification read — becomes
/// [`ProjectCreateOutcome::Unknown`] with the last record this call
/// provably held; nothing is removed either way, and `Unknown` never
/// retries the write or deletes anything.
pub(crate) fn create_empty_project(
    store: &StateStore,
    environment: &OperationEnvironment,
    owner: Uuid,
    operation_id: Uuid,
    parent_dir: &Path,
    project_name: &str,
) -> anyhow::Result<ProjectCreateOutcome> {
    if !environment.is_local() {
        bail!("only the local environment is supported");
    }
    let name = validate_project_name(project_name)?;
    let canonical_parent = verify_parent_dir(parent_dir)?;
    let target = canonical_parent.join(name);
    // `name` is one normal segment, so the join cannot escape the verified
    // parent; prove it lexically rather than trusting construction.
    if target.parent() != Some(canonical_parent.as_path()) {
        bail!("the project target escapes its verified parent");
    }

    // Intent first: a duplicate ID or a full journal fails here, before any
    // directory is created.
    let preparing = operation_journal::register(
        store,
        &NewOperation {
            id: operation_id,
            environment: *environment,
            owner,
            kind: OperationKind::Mkdir,
            target_path: target.clone(),
        },
    )?;
    let running = match operation_journal::transition(
        store,
        environment,
        &OperationTransition {
            owner,
            id: operation_id,
            expected: OperationState::Preparing,
            next: OperationState::Running,
            expected_revision: preparing.revision,
            failure: None,
        },
    ) {
        Ok(running) => running,
        Err(_) => {
            // No filesystem effect happened yet; the row stays non-terminal
            // and reads back as needing reconciliation.
            return Ok(unconfirmed(
                store,
                environment,
                operation_id,
                preparing,
                "running_unconfirmed",
            ));
        }
    };

    // The parent may have vanished, become a non-directory, or re-resolved
    // to a different identity since it was verified and journaled. Confirm
    // it again read-only immediately before the single write; any drift
    // ends the attempt as a recorded parent failure with no file written.
    // No recursive creation, no retry, no cleanup.
    match confirm_parent_dir(parent_dir) {
        Ok(rechecked) if rechecked == canonical_parent => {}
        Ok(_) => {
            return Ok(fail_before_write(
                store,
                environment,
                owner,
                operation_id,
                running,
                FailureKind::Internal,
            ));
        }
        Err(problem) => {
            return Ok(fail_before_write(
                store,
                environment,
                owner,
                operation_id,
                running,
                problem.failure_kind(),
            ));
        }
    }

    match fs::create_dir(&target) {
        Ok(()) => {
            // Exactly one winner reaches here; the directory below is the
            // one this call created. If the settlement below cannot be
            // proven, that fact is reported instead of success.
            match settle(
                store,
                environment,
                owner,
                operation_id,
                running.revision,
                OperationState::Succeeded,
                None,
            ) {
                Ok(done) => Ok(ProjectCreateOutcome::Created(done)),
                Err(_) => Ok(unconfirmed(
                    store,
                    environment,
                    operation_id,
                    running,
                    "settlement_unconfirmed",
                )),
            }
        }
        Err(error) => {
            let kind = match error.kind() {
                // Someone else owns this path: record it, change nothing.
                std::io::ErrorKind::AlreadyExists => FailureKind::TargetExists,
                // The parent vanished between the pre-write recheck and
                // creation; never fall back to creating parents.
                std::io::ErrorKind::NotFound => FailureKind::ParentMissing,
                std::io::ErrorKind::PermissionDenied => FailureKind::ParentNotWritable,
                std::io::ErrorKind::Interrupted => FailureKind::Interrupted,
                // Includes platform-illegal names (for example `?` on
                // Windows): the OS refused, so the failure is recorded
                // instead of renaming or repairing the name.
                _ => FailureKind::Internal,
            };
            match settle(
                store,
                environment,
                owner,
                operation_id,
                running.revision,
                OperationState::Failed,
                Some(OperationFailure {
                    kind,
                    exit_code: None,
                }),
            ) {
                Ok(done) => Ok(ProjectCreateOutcome::Failed(done)),
                Err(_) => Ok(unconfirmed(
                    store,
                    environment,
                    operation_id,
                    running,
                    "settlement_unconfirmed",
                )),
            }
        }
    }
}

/// Moves a `running` row to its terminal state with owner + revision proof.
///
/// Failure evidence travels only as the journal's fixed categories; no
/// external text, paths, or credentials are persisted.
fn settle(
    store: &StateStore,
    environment: &OperationEnvironment,
    owner: Uuid,
    id: Uuid,
    expected_revision: u64,
    next: OperationState,
    failure: Option<OperationFailure>,
) -> anyhow::Result<OperationRecord> {
    operation_journal::transition(
        store,
        environment,
        &OperationTransition {
            owner,
            id,
            expected: OperationState::Running,
            next,
            expected_revision,
            failure,
        },
    )
}

/// Records a pre-write abort as a terminal parent failure.
///
/// The filesystem was not touched, but the terminal write itself may still
/// fail to persist; that case is `Unknown`, never a plain error, so a
/// future caller cannot mistake it for "nothing happened" and retry.
fn fail_before_write(
    store: &StateStore,
    environment: &OperationEnvironment,
    owner: Uuid,
    id: Uuid,
    running: OperationRecord,
    kind: FailureKind,
) -> ProjectCreateOutcome {
    let revision = running.revision;
    match settle(
        store,
        environment,
        owner,
        id,
        revision,
        OperationState::Failed,
        Some(OperationFailure {
            kind,
            exit_code: None,
        }),
    ) {
        Ok(done) => ProjectCreateOutcome::Failed(done),
        Err(_) => unconfirmed(store, environment, id, running, "settlement_unconfirmed"),
    }
}

/// Builds the `Unknown` outcome when a journal write after registration
/// could not be proven — including when the verification read itself fails.
///
/// A failed `get` must never surface as a plain error here: past this
/// point the filesystem effect may already have happened, so any error is
/// indistinguishable from "unknown". Returning `Err` could mislead a future
/// caller into treating it as a pre-write failure and retrying, which the
/// journal's duplicate-ID rule and the no-retry contract forbid. The last
/// record this call provably held is reused instead; success and failure
/// are never fabricated.
fn unconfirmed(
    store: &StateStore,
    environment: &OperationEnvironment,
    id: Uuid,
    fallback: OperationRecord,
    detail: &'static str,
) -> ProjectCreateOutcome {
    let record = match operation_journal::get(store, environment, id) {
        Ok(Some(current)) => current,
        Ok(None) | Err(_) => fallback,
    };
    ProjectCreateOutcome::Unknown { record, detail }
}

/// Read-only parent problem for the pre-write recheck; carries no caller
/// input and no paths.
enum ParentProblem {
    Missing,
    NotWritable,
    /// The parent became a non-directory or failed in any other way. The
    /// journal has no narrower stored category for these, so they share
    /// `Internal`; user-visible guidance is the future handler's job.
    Internal,
}

impl ParentProblem {
    fn failure_kind(&self) -> FailureKind {
        match self {
            ParentProblem::Missing => FailureKind::ParentMissing,
            ParentProblem::NotWritable => FailureKind::ParentNotWritable,
            ParentProblem::Internal => FailureKind::Internal,
        }
    }
}

/// Re-confirms the parent read-only immediately before the single write.
///
/// Returns the freshly canonicalized directory so the caller can prove it
/// still resolves to the journaled identity. Missing, unreadable, and
/// non-directory parents — plus any other I/O failure — come back as a
/// fixed problem, never with reflected input. Only `canonicalize` and
/// `metadata` run here; nothing is created, renamed, or removed.
fn confirm_parent_dir(parent: &Path) -> Result<PathBuf, ParentProblem> {
    if parent.as_os_str().is_empty() {
        return Err(ParentProblem::Internal);
    }
    let resolved = fs::canonicalize(parent).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => ParentProblem::Missing,
        std::io::ErrorKind::PermissionDenied => ParentProblem::NotWritable,
        _ => ParentProblem::Internal,
    })?;
    match fs::metadata(&resolved) {
        Ok(metadata) if metadata.is_dir() => Ok(resolved),
        Ok(_) => Err(ParentProblem::Internal),
        Err(error) => Err(match error.kind() {
            std::io::ErrorKind::NotFound => ParentProblem::Missing,
            std::io::ErrorKind::PermissionDenied => ParentProblem::NotWritable,
            _ => ParentProblem::Internal,
        }),
    }
}
///
/// Accepts exactly one relative path segment.
///
/// Rejects the empty string, `.`, `..`, any `/` or `\` separator, and NUL, so
/// the name cannot escape the verified parent through the join. Everything
/// else the platform accepts — CJK, spaces, `@`, `#`, and similar — passes
/// through untouched; platform-illegal spellings are refused later by the
/// operating system and recorded as failures, not repaired. A trailing space
/// or dot is rejected because Windows strips it, which would otherwise make
/// the journaled target differ from the directory actually created. Error
/// messages name no caller input, so a credential-shaped string can never be
/// reflected into a log through this path.
fn validate_project_name(name: &str) -> anyhow::Result<&str> {
    if name.is_empty() {
        bail!("the project name is empty");
    }
    if name.contains('\0') {
        bail!("the project name contains NUL");
    }
    if name.contains('/') || name.contains('\\') {
        bail!("the project name must be a single path segment");
    }
    if name == "." || name == ".." {
        bail!("the project name must be a single path segment");
    }
    let path = Path::new(name);
    match (path.components().next(), path.components().nth(1)) {
        (Some(Component::Normal(_)), None) => {}
        _ => bail!("the project name must be a single path segment"),
    }
    if name.ends_with(' ') || name.ends_with('.') {
        bail!("the project name must not end with a space or dot");
    }
    Ok(name)
}

/// Resolves the parent on the executing machine and proves it is a directory.
///
/// Only `canonicalize`, `metadata`, and no shell run here, mirroring the
/// read-only browse path. Missing, unreadable, and non-directory parents fail
/// with fixed categories and no reflected input; parents are never created.
fn verify_parent_dir(parent: &Path) -> anyhow::Result<PathBuf> {
    if parent.as_os_str().is_empty() {
        bail!("the parent directory is empty");
    }
    let resolved = fs::canonicalize(parent).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            anyhow::anyhow!("parent_missing: the parent directory does not exist")
        }
        std::io::ErrorKind::PermissionDenied => {
            anyhow::anyhow!("parent_not_writable: the parent directory cannot be read")
        }
        _ => anyhow::Error::new(error).context("could not resolve the parent directory"),
    })?;
    let is_dir = fs::metadata(&resolved)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                anyhow::anyhow!("parent_missing: the parent directory does not exist")
            }
            std::io::ErrorKind::PermissionDenied => {
                anyhow::anyhow!("parent_not_writable: the parent directory cannot be read")
            }
            _ => anyhow::Error::new(error).context("could not inspect the parent directory"),
        })?
        .is_dir();
    if !is_dir {
        bail!("not_a_directory: the parent path is not a directory");
    }
    Ok(resolved)
}

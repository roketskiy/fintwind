//! Provider backend and driver-event wire translation for `fintwind-daemon`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::{
    Backend, Command, EventSink, Request, ResponsePayload, WireDriverEvent, WorkspaceOperation,
    WorkspaceResult,
};
use anyhow::{Context as _, anyhow, bail};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::attachments::AttachmentStore;
use crate::driver::{self, DriverHandle, DriverStartOptions, SessionOptions};
use crate::model::{
    ActivityKind, AgentSession, Checkpoint, CheckpointStatus, DriverEvent, PermissionOption,
    Project, ProviderResumeCursor, ProviderRetryAction, SessionStatus,
};
use crate::persistence::{ComposerDraftStore, PersistedState, StateStore};
use crate::settings::DaemonSettingsStore;
use fintwind_protocol::provider_session::{ProviderSessionFork, ProviderSessionForkRequest};
use fintwind_protocol::submission::SubmissionState;

pub struct FintwindBackend {
    browser_tools: Arc<crate::browser_tools::BrowserTools>,
    sessions: Arc<Mutex<HashMap<Uuid, (Uuid, DriverHandle)>>>,
    terminals: Mutex<HashMap<Uuid, (Uuid, crate::terminal::DaemonTerminal)>>,
    settings: DaemonSettingsStore,
    task_store: Arc<StateStore>,
    task_state: Arc<Mutex<PersistedState>>,
    removed_session_ids: Mutex<HashSet<Uuid>>,
    removed_project_ids: Mutex<HashSet<Uuid>>,
    /// Prompts whose dispatch this process currently holds a claim on.
    ///
    /// A queued row can also be `dispatching` because a crash left it there,
    /// and a cancel arriving at that row must be able to tell the two apart:
    /// converging a cancel against a dispatch that is still in flight would
    /// cancel a live request. An operation is inserted when its compare-and-
    /// swap claim succeeds and removed when this call returns, by whichever
    /// exit it takes, so membership is proof of "another connection is
    /// driving this right now" and absence is proof of "no one is".
    live_dispatches: Mutex<HashSet<Uuid>>,
    active_preparations: Mutex<HashMap<Uuid, EventSink>>,
    native_transcript_generations: Mutex<HashMap<String, u64>>,
    side_generations: crate::session_generation::SessionGenerations,
    composer_drafts: ComposerDraftStore,
    attachments: AttachmentStore,
    checkpoint_capture_locks: Mutex<HashMap<(PathBuf, Uuid, usize), Arc<Mutex<()>>>>,
    default_cwd: std::path::PathBuf,
}

/// Whether the turn one admitted prompt runs in could be opened.
///
/// The three answers are deliberately not one error: a prompt that found
/// the session busy is still queued and is retried, while one whose session
/// disappeared is answered from the record and never retried. Collapsing
/// them would report a merely-busy session as an unknown outcome.
enum MaterializedTurn {
    /// The turn is open; the value is its ordinal.
    Opened(usize),
    /// Another submission opened a turn first. This prompt did not run and
    /// stays queued.
    Busy,
    /// The session was removed or archived while this ran, and its rows
    /// were settled by that path.
    Gone,
}

/// Removes a dispatch's live marking on every exit path, including `?`.
///
/// Membership in `live_dispatches` is the proof that another connection is
/// driving this prompt right now; the guard is what keeps that proof true
/// no matter how the dispatch ends.
struct LiveDispatchGuard<'a> {
    live: &'a Mutex<HashSet<Uuid>>,
    operation_id: Uuid,
}

impl Drop for LiveDispatchGuard<'_> {
    fn drop(&mut self) {
        self.live.lock().remove(&self.operation_id);
    }
}

impl FintwindBackend {
    pub fn new(settings: DaemonSettingsStore, task_store: StateStore) -> anyhow::Result<Self> {
        let mut task_state = task_store
            .load()
            .context("could not load fintwind task database")?;
        migrate_projectless_state(&task_store, &mut task_state)?;
        let composer_drafts = ComposerDraftStore::for_state_path(task_store.path());
        let attachments = AttachmentStore::new(
            task_store
                .path()
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("attachments"),
        );
        let browser_tools = Arc::new(crate::browser_tools::BrowserTools::default());
        // Seed from the persisted setting so a daemon that comes up before
        // the app connects already exposes the right state to plugins.
        browser_tools.set_enabled(settings.get().browser_tools_enabled());
        Ok(Self {
            browser_tools,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            terminals: Mutex::new(HashMap::new()),
            settings,
            task_store: Arc::new(task_store),
            task_state: Arc::new(Mutex::new(task_state)),
            removed_session_ids: Mutex::new(HashSet::new()),
            removed_project_ids: Mutex::new(HashSet::new()),
            live_dispatches: Mutex::new(HashSet::new()),
            active_preparations: Mutex::new(HashMap::new()),
            native_transcript_generations: Mutex::new(HashMap::new()),
            side_generations: Default::default(),
            composer_drafts,
            attachments,
            checkpoint_capture_locks: Mutex::new(HashMap::new()),
            default_cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        })
    }

    /// Recover admission facts only. No runtime is not proof of execution idle.
    fn hydrate_submission_session(&self, session: &mut AgentSession) -> anyhow::Result<()> {
        self.task_store.hydrate(session)?;
        self.task_store.hydrate_submission_receipts(session)?;
        let runtime_attached = self.sessions.lock().contains_key(&session.id);
        let mut settled_unsent = false;
        for turn in &mut session.turns {
            let Some(receipt) = turn.submission.as_mut() else {
                continue;
            };
            if session.provider_cursor.is_none()
                && let Some(native) = receipt.native_session_id.as_ref()
            {
                session.provider_cursor =
                    Some(ProviderResumeCursor::from_session_id(native.clone()));
                session.native_session_id = Some(native.clone());
            }
            let preparing = self
                .active_preparations
                .lock()
                .get(&receipt.id)
                .is_some_and(EventSink::source_is_connected);
            if preparing {
                continue;
            }
            if !runtime_attached || receipt.state == SubmissionState::Preparing {
                self.active_preparations.lock().remove(&receipt.id);
                let target = match receipt.state {
                    SubmissionState::Preparing => Some(SubmissionState::NotSent),
                    SubmissionState::Dispatching => Some(SubmissionState::Unknown),
                    _ => None,
                };
                if let Some(target) = target {
                    let updated = self.task_store.transition_submission(
                        receipt.id,
                        receipt.state,
                        target,
                        None,
                    )?;
                    let updated = match updated {
                        Some(saved) => Some(saved),
                        None => self.task_store.submission(receipt.id)?,
                    };
                    if let Some(saved) = updated {
                        *receipt = saved.receipt;
                    }
                }
            }
            if matches!(
                receipt.state,
                SubmissionState::NotSent | SubmissionState::Rejected
            ) && turn.status == crate::model::TurnStatus::Running
            {
                turn.status = crate::model::TurnStatus::Interrupted;
                turn.completed_at = Some(crate::model::unix_time());
                settled_unsent = true;
            }
        }
        if settled_unsent {
            session.status = if fintwind_protocol::submission::has_unsettled_submissions(session) {
                SessionStatus::Working
            } else {
                SessionStatus::Idle
            };
        }
        Ok(())
    }

    /// Read-only provider reconciliation. Admission, history and execution
    /// proof remain separate; a superseded read never returns a stale projection.
    fn fetch_native_transcript(
        &self,
        binary: &Path,
        directory: &Path,
        native_id: &str,
    ) -> anyhow::Result<fintwind_protocol::provider_session::NativeTranscript> {
        use fintwind_protocol::submission::{
            has_unsettled_submissions, merge_native_transcript, replace_native_window,
        };
        let (local, generation, expected_projection) = {
            let mut state = self.task_state.lock();
            let local = state.sessions.iter_mut().find(|session| {
                session.native_session_id.as_deref() == Some(native_id)
                    || session
                        .provider_cursor
                        .as_ref()
                        .is_some_and(|cursor| cursor.native_id() == native_id)
            });
            let local = if let Some(local) = local {
                self.hydrate_submission_session(local)?;
                Some(local.clone())
            } else {
                None
            };
            let mut generations = self.native_transcript_generations.lock();
            let generation = generations.entry(native_id.to_owned()).or_default();
            *generation = generation.wrapping_add(1);
            let expected_projection = local.as_ref().map(serde_json::to_vec).transpose()?;
            (local, *generation, expected_projection)
        };
        let inputs = local
            .as_ref()
            .map(|session| {
                session
                    .turns
                    .iter()
                    .filter_map(|turn| {
                        turn.submission
                            .as_ref()
                            .map(|receipt| receipt.input_id.clone())
                    })
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let server = crate::opencode_pool::acquire(binary, directory)?;
        let mut transcript = driver::native::fetch_transcript(
            &server,
            native_id,
            &self.task_store.blobs(),
            &inputs,
        )?;
        let Some(mut local) = local else {
            if self.native_transcript_generations.lock().get(native_id) != Some(&generation) {
                bail!("native history read was superseded");
            }
            return Ok(transcript);
        };
        for turn in &local.turns {
            if let Some(receipt) = turn.submission.as_ref()
                && receipt.state.is_unconfirmed()
                && transcript
                    .turns
                    .iter()
                    .any(|native| native.provider_resume_at.as_deref() == Some(&receipt.input_id))
                && let Some(saved) = self.task_store.transition_submission(
                    receipt.id,
                    receipt.state,
                    SubmissionState::Accepted,
                    Some(native_id),
                )?
            {
                crate::opencode_diagnostics::submission(
                    "cold_receipt_reconciled",
                    &saved,
                    server.port,
                    generation,
                );
            }
        }
        // A failed CAS may mean a concurrent reader already confirmed receipt.
        // Always reload the authority, never retain the old Unknown copy.
        self.task_store.hydrate_submission_receipts(&mut local)?;
        let terminal = local
            .turns
            .iter()
            .find(|turn| {
                turn.status == crate::model::TurnStatus::Running
                    && turn
                        .submission
                        .as_ref()
                        .is_some_and(|receipt| receipt.state == SubmissionState::Accepted)
            })
            .and_then(|turn| turn.submission.as_ref())
            .and_then(|receipt| {
                driver::reconcile_saved_submission(server.port, native_id, receipt)
                    .ok()
                    .flatten()
                    .map(|proof| (receipt, proof))
            })
            .and_then(|(receipt, proof)| {
                // The terminal cannot stamp an earlier partial history read.
                replace_native_window(&mut transcript, proof.transcript, &receipt.input_id)
                    .then_some((receipt.id, proof.status, proof.completed_at))
            });

        let mut state = self.task_state.lock();
        if self.native_transcript_generations.lock().get(native_id) != Some(&generation) {
            bail!("native history read was superseded");
        }
        let runtimes = self.sessions.lock();
        if runtimes.contains_key(&local.id) {
            bail!("native history read raced an attached runtime");
        }
        let existing = state
            .sessions
            .iter_mut()
            .find(|session| session.id == local.id)
            .ok_or_else(|| anyhow!("native history target was removed"))?;
        if Some(serde_json::to_vec(existing)?) != expected_projection
            || existing.turns.last().map(|turn| turn.id) != local.turns.last().map(|turn| turn.id)
            || existing.native_session_id != local.native_session_id
            || existing.turns.iter().any(|turn| {
                self.active_preparations
                    .lock()
                    .get(&turn.id)
                    .is_some_and(EventSink::source_is_connected)
            })
        {
            bail!("native history target changed during reconciliation");
        }
        self.task_store.hydrate_submission_receipts(existing)?;
        // Merge against the latest projection, so same-turn settlement,
        // presentation and checkpoints published during I/O cannot roll back.
        let mut transcript = merge_native_transcript(existing, transcript);
        if let Some((id, status, completed_at)) = terminal
            && let Some(turn) = transcript.turns.iter_mut().find(|turn| turn.id == id)
            && turn.status == crate::model::TurnStatus::Running
            && turn
                .submission
                .as_ref()
                .is_some_and(|receipt| receipt.state == SubmissionState::Accepted)
        {
            turn.status = status;
            turn.completed_at = Some(completed_at);
        }
        existing.messages = transcript.messages.clone();
        existing.transcript_blocks = transcript.blocks.clone();
        existing.turns = transcript.turns.clone();
        existing.status = if has_unsettled_submissions(existing) {
            SessionStatus::Working
        } else {
            SessionStatus::Idle
        };
        state.mark_session_dirty(local.id);
        self.task_store.save(&mut state)?;
        Ok(transcript)
    }

    /// Capture and persist one ending checkpoint exactly once per daemon.
    /// Desktop and Web may observe the same turn completion concurrently; a
    /// per-turn lock prevents both clients from running the expensive Git
    /// snapshot while leaving unrelated tasks independent.
    fn capture_turn_checkpoint(
        &self,
        cwd: PathBuf,
        session_id: Uuid,
        turn_count: usize,
    ) -> anyhow::Result<Checkpoint> {
        let key = (cwd.clone(), session_id, turn_count);
        let capture_lock = self
            .checkpoint_capture_locks
            .lock()
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _capture = capture_lock.lock();

        {
            let mut state = self.task_state.lock();
            if let Some(index) = state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
            {
                self.task_store.hydrate(&mut state.sessions[index])?;
                if let Some(checkpoint) = state.sessions[index]
                    .turns
                    .iter()
                    .find(|turn| turn.turn_count == turn_count)
                    .and_then(|turn| turn.checkpoint.as_ref())
                    .filter(|checkpoint| {
                        matches!(
                            checkpoint.status,
                            CheckpointStatus::Ready | CheckpointStatus::Unavailable
                        )
                    })
                {
                    return Ok(checkpoint.clone());
                }
            }
        }

        let checkpoint = crate::checkpoint::capture_turn(&cwd, session_id, turn_count)?;
        let mut state = self.task_state.lock();
        if let Some(index) = state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        {
            self.task_store.hydrate(&mut state.sessions[index])?;
            if let Some(turn) = state.sessions[index]
                .turns
                .iter_mut()
                .find(|turn| turn.turn_count == turn_count)
            {
                turn.checkpoint = Some(checkpoint.clone());
                state.mark_session_dirty(session_id);
                self.task_store.save(&mut state)?;
            }
        }
        Ok(checkpoint)
    }

    /// Drop a project and its local session rows from the daemon catalog.
    /// The project folder and OpenCode-native sessions are left untouched.
    fn remove_project_from_catalog(&self, project_id: Uuid) -> anyhow::Result<()> {
        let session_ids = {
            let mut state = self.task_state.lock();
            let mut removed_session_ids = self.removed_session_ids.lock();
            let mut removed_project_ids = self.removed_project_ids.lock();
            removed_project_ids.insert(project_id);
            let session_ids = state
                .sessions
                .iter()
                .filter(|session| session.project_id == project_id)
                .map(|session| session.id)
                .collect::<Vec<_>>();
            for session_id in &session_ids {
                removed_session_ids.insert(*session_id);
            }
            drop(removed_session_ids);
            drop(removed_project_ids);
            state
                .sessions
                .retain(|session| session.project_id != project_id);
            state.projects.retain(|project| project.id != project_id);
            self.task_store.save(&mut state)?;
            session_ids
        };
        let mut sessions = self.sessions.lock();
        for session_id in session_ids {
            drop(sessions.remove(&session_id));
        }
        Ok(())
    }
}

/// Storage-layout migrations belong to the daemon because both the database
/// rows and the directories name paths on its host. Persist after each move
/// so a later failure cannot leave an earlier project pointing at its old
/// location in SQLite.
fn migrate_projectless_state(
    task_store: &StateStore,
    task_state: &mut PersistedState,
) -> anyhow::Result<()> {
    let indices = task_state
        .projects
        .iter()
        .enumerate()
        .filter_map(|(index, project)| {
            crate::projectless::needs_migration(&project.path).then_some(index)
        })
        .collect::<Vec<_>>();
    for index in indices {
        let old_path = task_state.projects[index].path.clone();
        let workspace = crate::projectless::migrate_workspace(&old_path).with_context(|| {
            format!(
                "could not move projectless workspace {} under ~/.fintwind/projects",
                old_path.display()
            )
        })?;
        task_state.projects[index].name = crate::model::Project::PROJECTLESS_NAME.to_owned();
        task_state.projects[index].path = workspace.cwd;
        task_store
            .save(task_state)
            .context("could not persist migrated projectless workspace")?;
    }
    Ok(())
}

impl FintwindBackend {
    // ---- remote control: daemon-owned task, queue and tidy state ----

    /// Admit one remote prompt: write its submission record and its queue
    /// row durably, and open no turn.
    ///
    /// Lock discipline is the point of the shape below. `task_state` is a
    /// non-reentrant mutex and every helper this path needs takes it, so
    /// the live guard is released before any of them runs: the target is
    /// validated under one short lock, and the commit takes its own.
    ///
    /// Admission deliberately does **not** open a turn. A queued prompt is
    /// a record, not a claim on the transcript: opening one here would hand
    /// a busy session a second running turn, and the turn that actually ran
    /// would then never settle. The turn is opened at dispatch, by
    /// `materialize_remote_turn`, once, and only when the session is idle.
    /// A refused admission therefore leaves the live session exactly as it
    /// was — busy or idle — with nothing to roll back.
    ///
    /// A prompt whose operation id is already on the books is answered from
    /// that row instead of admitting a second one: an identical retry gets
    /// the recorded truth, and a *different* request reusing the id is
    /// refused rather than silently replacing what was already accepted.
    fn admit_remote_prompt(
        &self,
        provenance: fintwind_protocol::submission::RemoteProvenance,
        session_id: Uuid,
        operation_id: Uuid,
        prompt: &str,
        expires_at_ms: u64,
        summary: &str,
    ) -> anyhow::Result<ResponsePayload> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            bail!("a remote prompt may not be empty");
        }
        // Idempotency by the caller's own operation id, before anything is
        // reserved: a retry must never create a second turn.
        //
        // Every field of the admitted request is part of the identity, the
        // prompt text included. Two requests sharing an operation id are the
        // same request only when they are identical; a different prompt under
        // the same id is refused rather than silently answered from the first
        // one's record — otherwise a retry could learn the state of a prompt
        // it did not send. The host's own dedup answers a retry before it
        // reaches here, so this is the backstop for a host that lost its
        // record, and the daemon compares what it durably holds.
        if let Some(existing) = self
            .task_store
            .remote_prompts()?
            .into_iter()
            .find(|row| row.operation_id == operation_id)
        {
            if existing.session_id != session_id
                || existing.device_id != provenance.device_id
                || existing.host_id != provenance.host_id.to_string()
                || existing.grant_version != provenance.grant_version
                || existing.summary != summary
                || existing.prompt != prompt
            {
                bail!("operation id is already used by a different request");
            }
            return Ok(ResponsePayload::RemotePromptState {
                operation_id,
                submission_id: existing.submission_id,
                state: existing.state,
            });
        }

        // ---- short lock: validate the target, reserve nothing ----
        //
        // No turn is opened here. A queued prompt is a durable record, not
        // a claim on the transcript: opening a turn at admission would give
        // a busy session a second running turn, and the event forwarder
        // settles the *last* turn — the queued one — leaving the turn that
        // actually ran unsettled and the session busy forever. The turn is
        // opened at dispatch, by `materialize_remote_turn`, exactly once
        // and only when the session is idle.
        let project = {
            let mut state = self.task_state.lock();
            if self.removed_session_ids.lock().contains(&session_id) {
                bail!("remote prompt target session was removed");
            }
            let Some(session) = state
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
            else {
                bail!("remote prompt target session is unknown");
            };
            self.hydrate_submission_session(session)?;
            if session.archived {
                bail!("remote prompt target session is archived");
            }
            let project_id = session.project_id;
            let project = state
                .projects
                .iter()
                .find(|project| project.id == project_id)
                .cloned()
                .ok_or_else(|| anyhow!("remote prompt target project is unknown"))?;
            project
        };
        // The submission id is chosen here and bound to the operation id in
        // the same durable write, so a retry answers from the row rather
        // than creating a second prompt.
        let submission_id = Uuid::new_v4();
        let record = fintwind_protocol::submission::SubmissionRecord::new(
            session_id,
            submission_id,
            prompt.to_owned(),
            Vec::new(),
        );
        let row = crate::persistence::RemotePromptRow {
            operation_id,
            submission_id,
            session_id,
            device_id: provenance.device_id,
            state: fintwind_protocol::submission::RemotePromptState::Queued
                .label()
                .to_owned(),
            summary: summary.to_owned(),
            prompt: prompt.to_owned(),
            enqueued_at_ms: crate::model::unix_time_millis(),
            expires_at_ms,
            host_id: provenance.host_id.to_string(),
            grant_version: provenance.grant_version,
            // A freshly admitted prompt has no cancel waiting on it yet.
            cancel_operation_id: None,
            cancel_state: None,
        };
        let response =
            self.commit_remote_admission(&project, &record, &row, operation_id)?;
        crate::opencode_diagnostics::record("remote_prompt_admitted", 0, 0);
        Ok(response)
    }

    /// Write one admitted prompt durably: its submission record and its
    /// remote-queue row, in one transaction, without touching the session's
    /// transcript.
    ///
    /// The session itself is not rewritten — admission changes nothing a
    /// reader of the transcript can see, so the commit carries the two rows
    /// that do change and leaves the live state alone. A refused admission
    /// therefore cannot leave a half-opened turn behind, on an idle or a
    /// busy session alike.
    fn commit_remote_admission(
        &self,
        project: &Project,
        submission: &fintwind_protocol::submission::SubmissionRecord,
        remote: &crate::persistence::RemotePromptRow,
        operation_id: Uuid,
    ) -> anyhow::Result<ResponsePayload> {
        let session_id = submission.session_id;
        if submission.receipt.state != SubmissionState::Preparing {
            bail!("invalid submission identity");
        }
        let mut state = self.task_state.lock();
        if self.removed_session_ids.lock().contains(&session_id)
            || self.removed_project_ids.lock().contains(&project.id)
        {
            bail!("submission target was removed");
        }
        if !state
            .sessions
            .iter()
            .any(|candidate| candidate.id == session_id)
        {
            bail!("remote prompt target session is unknown");
        }
        // A submission id that is already on the books is the same prompt
        // arriving twice: the saved record is the answer, and a conflicting
        // one is refused rather than replaced.
        if let Some(saved) = self.task_store.submission(submission.receipt.id)? {
            if saved.session_id != session_id
                || saved.receipt.input_id != submission.receipt.input_id
                || saved.prompt != submission.prompt
                || serde_json::to_vec(&saved.files)? != serde_json::to_vec(&submission.files)?
            {
                bail!("submission ID conflicts with a saved input");
            }
            return Ok(ResponsePayload::RemotePromptAdmitted {
                operation_id,
                submission_id: saved.receipt.id,
                state: serde_json::to_value(saved.receipt.state)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".to_owned()),
            });
        }
        let mut next = state.clone();
        if !next
            .projects
            .iter()
            .any(|candidate| candidate.id == project.id)
        {
            next.projects.push(project.clone());
        }
        self.task_store
            .save_with_submission_and_remote(&mut next, Some(submission), Some(remote))?;
        *state = next;
        // The daemon owns this preparation: no client going away may
        // invalidate it, so the sink is detached from any subscriber.
        self.active_preparations
            .lock()
            .insert(submission.receipt.id, EventSink::detached(session_id, Uuid::nil()));
        crate::opencode_diagnostics::submission("saved", submission, 0, 0);
        Ok(ResponsePayload::RemotePromptAdmitted {
            operation_id,
            submission_id: submission.receipt.id,
            state: fintwind_protocol::submission::RemotePromptState::Queued
                .label()
                .to_owned(),
        })
    }

    /// Prepare and dispatch one admitted prompt.
    ///
    /// Preparation is the daemon's own: the workspace is resolved from the
    /// session's record, the turn-start checkpoint is captured here, and the
    /// driver is started from the session's own options with a binary this
    /// computer resolved. Only then is the submission claimed. A crash
    /// anywhere before the claim leaves the prompt `NotSent` or `Unknown`,
    /// and neither is ever resent automatically.
    fn dispatch_remote_prompt(
        &self,
        provenance: fintwind_protocol::submission::RemoteProvenance,
        operation_id: Uuid,
        events: EventSink,
    ) -> anyhow::Result<ResponsePayload> {
        let row = self
            .task_store
            .remote_prompts()?
            .into_iter()
            .find(|row| row.operation_id == operation_id)
            .ok_or_else(|| anyhow!("no queued prompt carries that operation id"))?;
        // Provenance: only the device that asked, on the host it asked from,
        // under the grant it held, may drive this prompt. A restored queue is
        // re-checked rather than trusted.
        //
        // The grant version must match exactly, not merely be no older: a
        // re-pairing gives the device a *newer* version, and `newer passes`
        // would let it adopt and execute a prompt admitted under a grant it
        // no longer holds. The host's cleanup paths are a different matter —
        // they quote the version the row itself carries, so a revoked or
        // re-paired device's rows are still released; see
        // `HostState::cancel_remote_prompt_as_recorded`.
        if row.device_id != provenance.device_id
            || row.host_id != provenance.host_id.to_string()
            || provenance.grant_version != row.grant_version
        {
            bail!("that prompt belongs to another device");
        }        let state_now = fintwind_protocol::submission::RemotePromptState::parse(&row.state)
            .unwrap_or(fintwind_protocol::submission::RemotePromptState::Unknown);
        if !state_now.is_pending() {
            // Already dispatched, cancelled or expired: the recorded state
            // is the answer, and re-running is never attempted.
            return Ok(ResponsePayload::RemotePromptState {
                operation_id,
                submission_id: row.submission_id,
                state: row.state,
            });
        }
        if row.expires_at_ms <= crate::model::unix_time_millis() {
            self.task_store.delete_remote_prompt(operation_id)?;
            return Ok(ResponsePayload::RemotePromptState {
                operation_id,
                submission_id: row.submission_id,
                state: fintwind_protocol::submission::RemotePromptState::Expired
                    .label()
                    .to_owned(),
            });
        }
        let session_id = row.session_id;
        // A submission that already settled is answered from its record. A
        // crash mid-dispatch leaves the row `dispatching`, and whether the
        // driver ever received the prompt is a fact in the submission store,
        // not a guess: only a prompt still `Preparing` — provably never
        // handed to a driver — may be dispatched. Anything else is reported
        // exactly as it stands and never re-run.
        //
        // This is also the authority a waiting cancel is measured against,
        // and it is read *before* the cancel is settled. A row left
        // `dispatching` by a crash that happened after `driver.submit` has a
        // submission in `Dispatching`: the prompt ran, so cancelling it now
        // would claim a cancellation for something that executed. A missing
        // submission record is itself unknown evidence, never a pass.
        let submission_state = match self.task_store.submission(row.submission_id)? {
            Some(saved) => saved.receipt.state,
            None => SubmissionState::Unknown,
        };
        // A cancel waiting on this prompt is settled before anything is
        // dispatched. What it settles *to* is the submission's own truth, so
        // a prompt that already reached the driver is reported as having
        // run, and only one that provably never did is cancelled.
        if self.task_store.cancel_intent(operation_id)?.is_some() {
            // A dispatch this process still holds a claim on is deciding
            // the prompt's fate right now. Converging the cancel against it
            // could cancel a live request — the submission may still read
            // `Preparing` while the driver call is under way — so the row
            // is reported exactly as it stands. The in-flight dispatch
            // settles the waiting cancel itself when its outcome lands.
            if !self.live_dispatches.lock().contains(&operation_id) {
                return self.converge_pending_cancel(
                    operation_id,
                    row.submission_id,
                    session_id,
                    submission_state,
                );
            }
            return Ok(ResponsePayload::RemotePromptState {
                operation_id,
                submission_id: row.submission_id,
                state: row.state.clone(),
            });
        }
        if submission_state != SubmissionState::Preparing {
            // Already handed to a driver, or its fate is otherwise decided:
            // the recorded state is the answer, and re-running is never
            // attempted.
            return Ok(ResponsePayload::RemotePromptState {
                operation_id,
                submission_id: row.submission_id,
                state: row.state,
            });
        }
        // ---- short lock: snapshot, then release. Nothing below takes it. ----
        let (project, workspace) = {
            let mut state = self.task_state.lock();
            if self.removed_session_ids.lock().contains(&session_id) {
                self.task_store.delete_remote_prompt(operation_id)?;
                bail!("the prompt's session was deleted");
            }
            let project_id = {
                let Some(session) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                else {
                    self.task_store.delete_remote_prompt(operation_id)?;
                    bail!("the prompt's session no longer exists");
                };
                self.hydrate_submission_session(session)?;
                if session.archived {
                    self.task_store.delete_remote_prompt(operation_id)?;
                    bail!("the prompt's session was archived");
                }
                // A busy session never receives a queued prompt. The host's
                // view of idle is not proof — it is learned from events — so
                // the daemon checks its own state here. The row stays queued
                // and a later tick takes it: an ordinary prompt waits its
                // turn, and never steers the one already running.
                if session.status.is_busy()
                    || fintwind_protocol::submission::has_unsettled_submissions(session)
                {
                    return Ok(ResponsePayload::RemotePromptState {
                        operation_id,
                        submission_id: row.submission_id,
                        state: fintwind_protocol::submission::RemotePromptState::Queued
                            .label()
                            .to_owned(),
                    });
                }
                session.project_id
            };
            let project = state
                .projects
                .iter()
                .find(|project| project.id == project_id)
                .cloned()
                .ok_or_else(|| anyhow!("the prompt's project is unknown"))?;
            let workspace = {
                let session = state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .ok_or_else(|| anyhow!("the prompt's session no longer exists"))?;
                session.workspace.clone()
            };
            (project, workspace)
        };

        // Claim the dispatch before any side effect, so a crash mid-
        // preparation is visible as `dispatching` rather than still-queued.
        // The compare-and-swap is what stops a second caller from also
        // claiming it.
        let claimed = {
            // Serialize the durable claim and its in-memory live marker
            // with cancellation. No caller can observe a claimed-but-not-live
            // operation and settle it as an abandoned request.
            let mut live = self.live_dispatches.lock();
            let claimed = self.task_store.transition_remote_prompt(
            operation_id,
            fintwind_protocol::submission::RemotePromptState::Queued.label(),
            fintwind_protocol::submission::RemotePromptState::Dispatching.label(),
            ).context("remote-dispatch-claim")?;
            if claimed { live.insert(operation_id); }
            claimed
        };
        if !claimed {
            let row = self
                .task_store
                .remote_prompts()?
                .into_iter()
                .find(|row| row.operation_id == operation_id)
                .ok_or_else(|| anyhow!("the queued prompt disappeared"))?;
            return Ok(ResponsePayload::RemotePromptState {
                operation_id,
                submission_id: row.submission_id,
                state: row.state,
            });
        }
        // The claim is ours. From here until this call returns — by success,
        // by every early return, and by every `?` — this operation counts
        // as live: a cancel arriving on the row must not converge against a
        // dispatch that is still being decided. The guard removes the
        // marking on every exit path.
        let _live = LiveDispatchGuard {
            live: &self.live_dispatches,
            operation_id,
        };

        // Re-check the target after the claim: a session deleted, archived or
        // expired while this ran must never receive the prompt.
        if self.remote_target_still_valid(session_id, operation_id).context("remote-dispatch-target")? {
            // The turn opens first, so the checkpoint below addresses the
            // turn it belongs to rather than the one before it. Opening it
            // is also where the busy question is answered authoritatively:
            // the local GUI can have committed a turn between the snapshot
            // above and this point, and a queued prompt must never become a
            // second running turn.
            match self.materialize_remote_turn(session_id, row.submission_id, &row.prompt).context("remote-dispatch-materialize")? {
                MaterializedTurn::Opened(turn_count) => {
                    if let Ok(workspace) =
                        self.prepare_remote_turn(&project, session_id, &workspace, turn_count)
                    {
                        let outcome = (|| -> anyhow::Result<ResponsePayload> {
                            self.ensure_remote_runtime(session_id, &events)?;
                            self.dispatch_submission_inner(
                                session_id,
                                row.submission_id,
                                workspace,
                            )
                        })();
                        if let Ok(ResponsePayload::SubmissionSaved { receipt }) = outcome
                            && matches!(receipt.state, SubmissionState::Dispatching | SubmissionState::Accepted)
                        {
                            self.task_store.transition_remote_prompt(
                                operation_id,
                                fintwind_protocol::submission::RemotePromptState::Dispatching
                                    .label(),
                                fintwind_protocol::submission::RemotePromptState::Dispatched
                                    .label(),
                            ).context("remote-dispatch-commit")?;
                            // The prompt reached the driver. A cancel that
                            // arrived while this dispatch was in flight came
                            // too late: it is settled as `dispatched`, never
                            // as a cancellation, so no device is told its
                            // prompt was stopped when it ran. The ending is
                            // written to the durable term record so a query
                            // after a crash still has it.
                            self.settle_cancel_term(operation_id, "dispatched").context("remote-dispatch-cancel-settlement")?;
                            crate::opencode_diagnostics::record(
                                "remote_prompt_dispatched",
                                0,
                                0,
                            );
                            return Ok(ResponsePayload::RemotePromptState {
                                operation_id,
                                submission_id: row.submission_id,
                                state: fintwind_protocol::submission::RemotePromptState::Dispatched.label().to_owned(),
                            });
                        }
                    }
                }
                MaterializedTurn::Busy => {
                    // The session became busy after this dispatch's snapshot.
                    // The prompt did not run and is not lost: hand the claim
                    // back so a later tick takes it. The original operation
                    // id is kept, so a query still answers from this row
                    // rather than looking like a new request.
                    let reclaimed = self.task_store.transition_remote_prompt(
                        operation_id,
                        fintwind_protocol::submission::RemotePromptState::Dispatching.label(),
                        fintwind_protocol::submission::RemotePromptState::Queued.label(),
                    )?;
                    if !reclaimed {
                        // Someone else settled it while the claim was ours;
                        // their recorded state is the answer.
                        let settled = self
                            .task_store
                            .remote_prompts()?
                            .into_iter()
                            .find(|row| row.operation_id == operation_id);
                        return Ok(ResponsePayload::RemotePromptState {
                            operation_id,
                            submission_id: row.submission_id,
                            state: settled
                                .map(|row| row.state)
                                .unwrap_or_else(|| row.state.clone()),
                        });
                    }
                    // The row is back to `queued`, so a cancel waiting on it
                    // is settled right now rather than waiting for another
                    // tick to come around. The submission is still
                    // `Preparing` here — the driver was never reached — so
                    // the cancellation holds.
                    if self.task_store.cancel_intent(operation_id)?.is_some() {
                        return self.converge_pending_cancel(
                            operation_id,
                            row.submission_id,
                            session_id,
                            SubmissionState::Preparing,
                        );
                    }
                    return Ok(ResponsePayload::RemotePromptState {
                        operation_id,
                        submission_id: row.submission_id,
                        state: fintwind_protocol::submission::RemotePromptState::Queued
                            .label()
                            .to_owned(),
                    });
                }
                MaterializedTurn::Gone => {
                    // The session was removed or archived while this ran, and
                    // that path already settled this prompt's row. Nothing ran
                    // and nothing will; the submission store holds the truth.
                    let _ = self.abandon_remote_prompt(session_id, row.submission_id);
                    self.delete_prompt_settling_cancel(operation_id, row.submission_id)?;
                    return Ok(ResponsePayload::RemotePromptState {
                        operation_id,
                        submission_id: row.submission_id,
                        state: fintwind_protocol::submission::RemotePromptState::Unknown
                            .label()
                            .to_owned(),
                    });
                }
            }
        }
        // Nothing ran. The submission is abandoned and the row is dropped, so
        // no later tick can retry it — an unknown outcome is only ever
        // answered by an explicit query, never by a second send.
        let _ = self.abandon_remote_prompt(session_id, row.submission_id);
        self.delete_prompt_settling_cancel(operation_id, row.submission_id)?;
        Ok(ResponsePayload::RemotePromptState {
            operation_id,
            submission_id: row.submission_id,
            state: fintwind_protocol::submission::RemotePromptState::Unknown
                .label()
                .to_owned(),
        })
    }

    /// Delete one claimed prompt's row, settling a waiting cancel first.
    ///
    /// A cancel that was bound while this dispatch was in flight must not
    /// disappear with the row: the device was told the promise was durable,
    /// and a deleted row with no recorded ending would leave it waiting
    /// forever. The caller has just settled the submission, so the
    /// submission store — never this row — says what the ending is: never
    /// sent means the cancellation holds, already run means it came too
    /// late, and exhausted evidence stays `unknown`. Reading the row's
    /// binding, writing the ending, and deleting the row are one
    /// transaction, and a row with no binding just goes away.
    fn delete_prompt_settling_cancel(
        &self,
        operation_id: Uuid,
        submission_id: Uuid,
    ) -> anyhow::Result<()> {
        let state = match self.task_store.submission(submission_id)? {
            Some(saved) => match saved.receipt.state {
                SubmissionState::NotSent | SubmissionState::Rejected => "cancelled",
                SubmissionState::Dispatching | SubmissionState::Accepted => "dispatched",
                // Still preparing after the caller's abandon, or evidence
                // exhausted: the ending is not provable, so it is unknown —
                // never upgraded into a cancellation.
                SubmissionState::Preparing | SubmissionState::Unknown => "unknown",
            },
            None => "unknown",
        };
        let _ = self
            .task_store
            .settle_cancel_and_delete_row(operation_id, state)?;
        Ok(())
    }

    /// Record the ending of a cancel that was waiting on this prompt, and
    /// clear the binding.
    ///
    /// Used at the points where the prompt's own fate becomes known without
    /// a cancel being the cause — a dispatch that reached the driver. The
    /// ending lands in the durable term record, so a device asking after a
    /// crash gets the fact rather than a guess, and the row can go away
    /// without taking the answer with it.
    ///
    /// The term and the cleared binding are one transaction: a crash
    /// between them used to leave a term that a later resolve could
    /// overwrite, or a binding that looked still-waiting on a prompt that
    /// had already run.
    fn settle_cancel_term(&self, operation_id: Uuid, state: &str) -> anyhow::Result<()> {
        self.task_store
            .settle_cancel_term_and_clear(operation_id, state)?;
        Ok(())
    }

    /// Settle a cancel that is waiting on this prompt, using the submission
    /// store as the authority on whether the prompt ever ran.
    ///
    /// `submission_state` is `Preparing` only when the driver provably never
    /// received the prompt — the one case in which cancelling is truthful.
    /// Every other state means the prompt's fate is already decided, and the
    /// cancel settles to that truth rather than to a cancellation.
    ///
    /// The ending is written to the durable cancel-term record in the same
    /// breath, so a device can ask again after a crash and get the recorded
    /// fact. Nothing is ever re-sent to reach it.
    fn converge_pending_cancel(
        &self,
        operation_id: Uuid,
        submission_id: Uuid,
        session_id: Uuid,
        submission_state: SubmissionState,
    ) -> anyhow::Result<ResponsePayload> {
        // The ending, decided by the submission's own truth — never by
        // whether a row happens to still exist.
        let (state, ran) = match submission_state {
            // Provably never handed to a driver: the cancellation holds.
            SubmissionState::Preparing => (
                fintwind_protocol::submission::RemotePromptState::Cancelled
                    .label()
                    .to_owned(),
                false,
            ),
            // The driver took it, or confirmed it. It ran, so the cancel came
            // too late and is reported as such rather than as a cancellation.
            SubmissionState::Dispatching | SubmissionState::Accepted => (
                fintwind_protocol::submission::RemotePromptState::Dispatched
                    .label()
                    .to_owned(),
                true,
            ),
            // Refused, or never sent: the prompt did not run.
            SubmissionState::NotSent | SubmissionState::Rejected => (
                fintwind_protocol::submission::RemotePromptState::Cancelled
                    .label()
                    .to_owned(),
                false,
            ),
            // The evidence ran out. `unknown` is the honest ending and is
            // never upgraded to a success by inference.
            SubmissionState::Unknown => (
                fintwind_protocol::submission::RemotePromptState::Unknown
                    .label()
                    .to_owned(),
                false,
            ),
        };
        // The submission is settled first, before any cancel bookkeeping:
        // its own truth is what decided `state`, and the cancel's records
        // never outrank it. Prompts that provably did not run are released
        // here; prompts that ran keep the submission their driver holds.
        if !ran {
            let _ = self.abandon_remote_prompt(session_id, submission_id);
        }
        // The ending, the cleared binding, and the row's deletion are one
        // transaction. They used to be three, and a crash between them
        // left a middle state every later reader had to guess about — a
        // term with a still-pending binding, or a deleted row whose ending
        // had never been written.
        let _ = self
            .task_store
            .settle_cancel_and_delete_row(operation_id, &state)?;
        crate::opencode_diagnostics::record("remote_cancel_converged", 0, 0);
        Ok(ResponsePayload::RemotePromptState {
            operation_id,
            submission_id,
            state,
        })
    }

    /// Whether one claimed prompt may still be dispatched: the session must
    /// still exist, not be removed or archived, and the row must still be
    /// the one this caller claimed.
    fn remote_target_still_valid(
        &self,
        session_id: Uuid,
        operation_id: Uuid,
    ) -> anyhow::Result<bool> {
        {
            let state = self.task_state.lock();
            if self.removed_session_ids.lock().contains(&session_id) {
                return Ok(false);
            }
            let Some(session) = state
                .sessions
                .iter()
                .find(|session| session.id == session_id)
            else {
                return Ok(false);
            };
            if session.archived {
                return Ok(false);
            }
        }
        let row = self
            .task_store
            .remote_prompts()?
            .into_iter()
            .find(|row| row.operation_id == operation_id);
        Ok(row.is_some_and(|row| {
            fintwind_protocol::submission::RemotePromptState::parse(&row.state)
                .is_some_and(|state| state.is_pending())
        }))
    }

    /// Resolve the workspace a turn runs in, creating a worktree when the
    /// session asked for one, and capture its turn-start checkpoint.
    ///
    /// This is the daemon's own copy of the GUI's preparation step: same
    /// workspace operation, same checkpoint, no client in the middle.
    fn prepare_remote_turn(
        &self,
        project: &Project,
        session_id: Uuid,
        workspace: &crate::model::SessionWorkspace,
        turn_count: usize,
    ) -> anyhow::Result<crate::model::SessionWorkspace> {
        use crate::{WorkspaceOperation, WorkspaceResult};
        let workspace = match workspace {
            crate::model::SessionWorkspace::NewWorktree { base_branch } => {
                if project.is_projectless() {
                    anyhow::bail!("a projectless task cannot create a Git worktree");
                }
                let created = crate::workspace::execute(WorkspaceOperation::CreateWorktree {
                    project_path: project.path.clone(),
                    project_id: project.id,
                    session_id,
                    prompt: project.name.clone(),
                    base_branch: base_branch.clone(),
                })?;
                let WorkspaceResult::WorktreeCreated { worktree } = created else {
                    anyhow::bail!("the daemon returned an invalid worktree response");
                };
                crate::model::SessionWorkspace::Worktree {
                    path: worktree.path,
                    branch: worktree.branch,
                }
            }
            workspace => workspace.clone(),
        };
        let project_path = workspace
            .path()
            .unwrap_or(&project.path)
            .to_path_buf();
        // A checkpoint failure is reported, never fatal: the turn still runs,
        // and the warning travels with the session's own record.
        if let Err(error) = self.capture_turn_checkpoint(project_path, session_id, turn_count) {
            eprintln!("remote turn checkpoint failed; session={session_id} error={error}");
        }
        Ok(workspace)
    }

    /// Start the session's driver when it has none, from the session's own
    /// record and a binary this computer resolved.
    ///
    /// The binary comes from the daemon's trusted provider probe — the same
    /// resolution its own drivers use — so an isolated or overridden
    /// installation is honored and a remote caller can never name one.
    ///
    /// The requesting sink is carried through so the runtime this starts can
    /// publish to the same hub a client-started runtime publishes to: the
    /// request that asked for the dispatch cannot name the runtime it is
    /// about to create, so the sink is rebound to the runtime's own
    /// coordinates and that runtime is opened on the hub here.
    fn ensure_remote_runtime(
        &self,
        session_id: Uuid,
        publisher: &EventSink,
    ) -> anyhow::Result<Uuid> {
        if let Some((runtime_id, _)) = self.sessions.lock().get(&session_id) {
            return Ok(*runtime_id);
        }
        let runtime_id = Uuid::new_v4();
        let state = self.task_state.lock();
        let session = state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("the prompt's session no longer exists"))?;
        let project = state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .ok_or_else(|| anyhow!("the prompt's project is unknown"))?;
        let workspace_path = session
            .workspace
            .path()
            .unwrap_or(&project.path)
            .to_path_buf();
        ensure_shell_environment();
        let probe = crate::model::cached_provider_probe(None, Some(&workspace_path));
        let binary = probe
            .path
            .clone()
            .filter(|path| path.is_file())
            .ok_or_else(|| anyhow!("this computer has no opencode binary to run the task"))?;
        let options = crate::driver::DriverStartOptions {
            binary,
            cwd: workspace_path,
            mode: session.runtime_mode,
            interaction_mode: session.interaction_mode,
            model: session.model.clone(),
            reasoning_effort: session.reasoning_effort.clone(),
            service_tier: session.service_tier.clone(),
            context_window: session.context_window.clone(),
            agent_preset: session.agent_preset.clone(),
            provider_cursor: session.provider_cursor.clone(),
            // The daemon keys runtimes by the task's own UUID, exactly as
            // the GUI's start path does, so metadata written by the driver
            // lands on the right task.
            task_id: Some(session_id.to_string()),
        };
        drop(state);
        // The runtime is opened on the hub before its first event exists, so
        // the hub accepts what it publishes instead of dropping it.
        let publisher = publisher.rebound(session_id, runtime_id);
        publisher.open_runtime();
        let (wake, _wake_events) = smol::channel::bounded(1);
        let (event_sender, event_receiver) = crate::driver::event_channel(wake);
        let handle = crate::driver::start_local(
            options,
            event_sender,
            crate::browser_tools::BrowserToolRuntime {
                registry: self.browser_tools.clone(),
                session_id,
                runtime_id,
            },
        )?;
        self.spawn_remote_event_forwarder(session_id, runtime_id, event_receiver, publisher);
        self.sessions
            .lock()
            .insert(session_id, (runtime_id, handle));
        Ok(runtime_id)
    }

    /// Open the turn one admitted prompt runs in, once, with the id its
    /// submission was bound to at admission, and return that turn's ordinal.
    ///
    /// The prompt's text travels from the daemon's own row, so a device
    /// never supplies it twice and a retry after a crash finds the turn
    /// already open and adds nothing. The receipt travels with the turn, so
    /// a crash before the driver is reached settles the turn as never-sent
    /// instead of leaving a running turn nobody can finish.
    ///
    /// The busy re-check is the point of the critical section. Between the
    /// dispatch snapshot and here, this computer's own GUI may have
    /// committed a running turn — its `AcceptSubmission` opens a turn in
    /// the same `task_state` critical section it commits in, so a check
    /// performed at the append point is authoritative: whatever opened a
    /// turn before this lock is visible here, and whatever opens one after
    /// sees the turn this call opened. The remote row's compare-and-swap
    /// cannot provide this, because the local submission never touches the
    /// remote row at all.
    fn materialize_remote_turn(
        &self,
        session_id: Uuid,
        submission_id: Uuid,
        prompt: &str,
    ) -> anyhow::Result<MaterializedTurn> {
        let mut state = self.task_state.lock();
        let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return Ok(MaterializedTurn::Gone);
        };
        if session.archived {
            return Ok(MaterializedTurn::Gone);
        }
        if let Some(turn) = session
            .turns
            .iter()
            .find(|turn| turn.id == submission_id)
        {
            return Ok(MaterializedTurn::Opened(turn.turn_count));
        }
        self.hydrate_submission_session(session)?;
        if self.removed_session_ids.lock().contains(&session_id) {
            return Ok(MaterializedTurn::Gone);
        }
        // The authoritative re-check, at the append point: any turn already
        // running, or any submission still unsettled, means this session is
        // not free. Appending anyway would leave the session with two
        // running turns, and the forwarder settles only the last.
        if session.status.is_busy()
            || session
                .turns
                .iter()
                .any(|turn| turn.status == crate::model::TurnStatus::Running)
            || fintwind_protocol::submission::has_unsettled_submissions(session)
        {
            return Ok(MaterializedTurn::Busy);
        }
        session.begin_turn_with_id(submission_id, prompt);
        let receipt = self
            .task_store
            .submission(submission_id)?
            .map(|saved| saved.receipt);
        let Some(turn) = session
            .turns
            .iter_mut()
            .find(|turn| turn.id == submission_id)
        else {
            bail!("the opened turn is missing");
        };
        turn.submission = receipt;
        let turn_count = turn.turn_count;
        state.mark_session_dirty(session_id);
        self.task_store.save(&mut state)?;
        Ok(MaterializedTurn::Opened(turn_count))
    }

    /// The dispatch half of the submission path, shared with the GUI's
    /// `DispatchSubmission`: claim the saved submission once, then hand it
    /// to the driver. A driver refusal settles the submission as `NotSent`
    /// and never retries.
    fn dispatch_submission_inner(
        &self,
        session_id: Uuid,
        submission_id: Uuid,
        workspace: crate::model::SessionWorkspace,
    ) -> anyhow::Result<ResponsePayload> {
        let runtime_id = self
            .sessions
            .lock()
            .get(&session_id)
            .map(|(runtime_id, _)| *runtime_id)
            .ok_or_else(|| anyhow!("submission runtime is unavailable"))?;
        let mut state = self.task_state.lock();
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            self.hydrate_submission_session(session)?;
        }
        let saved = self
            .task_store
            .submission(submission_id)?
            .ok_or_else(|| anyhow!("submission is missing"))?;
        if saved.session_id != session_id {
            bail!("submission belongs to another session");
        }
        if saved.receipt.state != SubmissionState::Preparing {
            return Ok(ResponsePayload::SubmissionSaved {
                receipt: saved.receipt,
            });
        }
        let driver = {
            let sessions = self.sessions.lock();
            let (active_runtime_id, driver) = sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("submission runtime is unavailable"))?;
            if *active_runtime_id != runtime_id {
                bail!("submission runtime was replaced");
            }
            driver.clone()
        };
        let session = state
            .sessions
            .iter_mut()
            .find(|candidate| candidate.id == session_id)
            .ok_or_else(|| anyhow!("submission session is missing"))?;
        if !session
            .turns
            .last()
            .is_some_and(|turn| turn.id == submission_id)
        {
            bail!("submission turn was replaced");
        }
        session.workspace = workspace;
        state.mark_session_dirty(session_id);
        self.task_store.save(&mut state)?;
        let Some(saved) = self.task_store.transition_submission(
            submission_id,
            SubmissionState::Preparing,
            SubmissionState::Dispatching,
            None,
        )?
        else {
            let saved = self
                .task_store
                .submission(submission_id)?
                .ok_or_else(|| anyhow!("submission is missing"))?;
            return Ok(ResponsePayload::SubmissionSaved {
                receipt: saved.receipt,
            });
        };
        crate::opencode_diagnostics::submission("dispatching", &saved, 0, 0);
        self.active_preparations.lock().remove(&submission_id);
        if let Some(turn) = state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .and_then(|session| session.turns.last_mut())
        {
            turn.submission = Some(saved.receipt.clone());
        }
        if let Err(error) = driver.submit(saved.clone(), self.task_store.clone()) {
            if let Some(saved) = self.task_store.transition_submission(
                submission_id,
                SubmissionState::Dispatching,
                SubmissionState::NotSent,
                None,
            )? {
                if let Some(session) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    if let Some(turn) = session.turns.last_mut() {
                        turn.submission = Some(saved.receipt.clone());
                    }
                    session.finish_active_turn(crate::model::TurnStatus::Failed);
                    session.status = SessionStatus::Idle;
                }
                state.mark_session_dirty(session_id);
                self.task_store.save(&mut state)?;
                return Ok(ResponsePayload::SubmissionSaved {
                    receipt: saved.receipt,
                });
            }
            return Err(error);
        }
        Ok(ResponsePayload::SubmissionSaved {
            receipt: saved.receipt,
        })
    }

    /// Forward one daemon-owned driver's events: onto the task state, and
    /// out to the hub, exactly the way the GUI's start path does.
    ///
    /// The two halves are both required. Without the task-state half the
    /// turn never settles in the daemon's own record; without the hub half
    /// no client ever sees the turn, so a host cannot learn the session went
    /// busy and would dispatch the next queued prompt into a running turn.
    fn spawn_remote_event_forwarder(
        &self,
        session_id: Uuid,
        runtime_id: Uuid,
        event_receiver: crossbeam_channel::Receiver<crate::model::DriverEvent>,
        publisher: EventSink,
    ) {
        let task_state = self.task_state.clone();
        let task_store = self.task_store.clone();
        let active_runtimes = self.sessions.clone();
        std::thread::Builder::new()
            .name(format!("fintwind-daemon-remote-events-{session_id}"))
            .spawn(move || {
                while let Ok(event) = event_receiver.recv() {
                    if let crate::model::DriverEvent::SubmissionUpdated(receipt) = &event {
                        let mut state = task_state.lock();
                        let current_runtime = active_runtimes
                            .lock()
                            .get(&session_id)
                            .is_some_and(|(active, _)| *active == runtime_id);
                        if let Some(turn) = state
                            .sessions
                            .iter_mut()
                            .find(|session| session.id == session_id && current_runtime)
                            .and_then(|session| {
                                session.turns.iter_mut().find(|turn| turn.id == receipt.id)
                            })
                            && turn
                                .submission
                                .as_ref()
                                .is_none_or(|previous| receipt.supersedes(previous))
                        {
                            turn.submission = Some(receipt.clone());
                        }
                    }
                    if let crate::model::DriverEvent::TurnFinished { success, .. } = &event {
                        let mut state = task_state.lock();
                        let current_runtime = active_runtimes
                            .lock()
                            .get(&session_id)
                            .is_some_and(|(active, _)| *active == runtime_id);
                        if let Some(session) = state
                            .sessions
                            .iter_mut()
                            .find(|session| session.id == session_id && current_runtime)
                            && session
                                .turns
                                .last()
                                .and_then(|turn| turn.submission.as_ref())
                                .is_some_and(|receipt| {
                                    matches!(
                                        receipt.state,
                                        SubmissionState::Accepted
                                            | SubmissionState::Dispatching
                                            | SubmissionState::Unknown
                                            | SubmissionState::Rejected
                                    )
                                })
                        {
                            session.finish_active_turn(if *success {
                                crate::model::TurnStatus::Completed
                            } else {
                                crate::model::TurnStatus::Failed
                            });
                            session.status = if *success {
                                SessionStatus::Idle
                            } else {
                                SessionStatus::Failed
                            };
                            state.mark_session_dirty(session_id);
                            let _ = task_store.save(&mut state);
                        }
                    }
                    // Publish the verified boundary to subscribers before the
                    // loop can take the next event, so a client never sees a
                    // turn finish after its successor started.
                    let wire = event_to_wire(event).unwrap_or_else(|error| {
                        WireDriverEvent::new(
                            "error",
                            Value::String(format!("could not encode daemon event: {error}")),
                        )
                    });
                    if publisher.send(wire).is_err() {
                        break;
                    }
                }
            })
            .ok();
    }

    /// Abandon one admitted prompt's submission, so a failed preparation
    /// leaves the turn settled rather than hanging as Running.
    ///
    /// A queued prompt usually has no turn yet — it is admitted as a record
    /// — so this normally only settles the submission. When a turn was
    /// already opened by a failed dispatch, that turn is settled as failed.
    /// Either way the session's status is recomputed from what is actually
    /// still unsettled: abandoning one queued prompt must never declare a
    /// session idle while another turn is still running.
    fn abandon_remote_prompt(
        &self,
        session_id: Uuid,
        submission_id: Uuid,
    ) -> anyhow::Result<()> {
        self.active_preparations.lock().remove(&submission_id);
        let mut state = self.task_state.lock();
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if let Some(turn) = session
                .turns
                .iter_mut()
                .find(|turn| turn.id == submission_id)
            {
                turn.submission = None;
                if turn.status == crate::model::TurnStatus::Running {
                    turn.status = crate::model::TurnStatus::Failed;
                    turn.completed_at = Some(crate::model::unix_time());
                }
            }
            session.status = if fintwind_protocol::submission::has_unsettled_submissions(session) {
                SessionStatus::Working
            } else {
                SessionStatus::Idle
            };
            state.mark_session_dirty(session_id);
            self.task_store.save(&mut state)?;
        }
        let _ = self
            .task_store
            .transition_submission(
                submission_id,
                SubmissionState::Preparing,
                SubmissionState::NotSent,
                None,
            )
            .ok();
        Ok(())
    }

    /// Cancel one admitted prompt that has not been dispatched yet.
    ///
    /// `cancel_operation_id` is the device's own cancel operation, and it is
    /// what makes the promise durable: when the prompt is mid-dispatch the
    /// cancel cannot be honoured yet, so it is recorded on the row under
    /// this id, and the dispatch resolves it. Without it the host would have
    /// nothing durable to answer a restart with.
    fn cancel_remote_prompt(
        &self,
        provenance: &fintwind_protocol::submission::RemoteProvenance,
        operation_id: Uuid,
        cancel_operation_id: Option<Uuid>,
    ) -> anyhow::Result<ResponsePayload> {
        let _claim_gate = self.live_dispatches.lock();
        let row = self
            .task_store
            .remote_prompts()?
            .into_iter()
            .find(|row| row.operation_id == operation_id)
            .ok_or_else(|| anyhow!("no queued prompt carries that operation id"))?;
        if row.device_id != provenance.device_id
            || row.host_id != provenance.host_id.to_string()
            || provenance.grant_version != row.grant_version
        {
            bail!("that prompt belongs to another device");
        }
        let state_now = fintwind_protocol::submission::RemotePromptState::parse(&row.state)
            .unwrap_or(fintwind_protocol::submission::RemotePromptState::Unknown);
        if !state_now.is_pending() {
            return Ok(ResponsePayload::RemotePromptState {
                operation_id,
                submission_id: row.submission_id,
                state: row.state,
            });
        }
        // A prompt whose dispatch is being decided right now is not
        // cancelled: claiming a cancellation for it would contradict the
        // dispatch that is already under way. Instead the cancel is recorded
        // durably on the row, so the promise survives a host crash, and the
        // dispatch resolves it the moment its outcome is known.
        if state_now == fintwind_protocol::submission::RemotePromptState::Dispatching {
            match self
                .task_store
                .record_cancel_intent(operation_id, cancel_operation_id)?
            {
                crate::persistence::CancelIntentOutcome::Recorded => {
                    crate::opencode_diagnostics::record("remote_cancel_pending", 0, 0);
                }
                crate::persistence::CancelIntentOutcome::Conflict => {
                    // Another device's cancel already holds this prompt. Two
                    // cancellations cannot both be the one that counted.
                    bail!("another cancellation is already waiting on this prompt");
                }
                crate::persistence::CancelIntentOutcome::RowMissing => {
                    bail!("no queued prompt carries that operation id");
                }
            }
            return Ok(ResponsePayload::RemotePromptState {
                operation_id,
                submission_id: row.submission_id,
                state: row.state,
            });
        }
        match self.task_store.record_cancel_intent(operation_id, cancel_operation_id)? {
            crate::persistence::CancelIntentOutcome::Recorded => {}
            crate::persistence::CancelIntentOutcome::Conflict => bail!("another cancellation is already waiting on this prompt"),
            crate::persistence::CancelIntentOutcome::RowMissing => bail!("no queued prompt carries that operation id"),
        }
        self.abandon_remote_prompt(row.session_id, row.submission_id)?;
        self.task_store.settle_cancel_and_delete_row(operation_id, "cancelled")?;
        crate::opencode_diagnostics::record("remote_prompt_cancelled", 0, 0);
        Ok(ResponsePayload::RemotePromptState {
            operation_id,
            submission_id: row.submission_id,
            state: fintwind_protocol::submission::RemotePromptState::Cancelled
                .label()
                .to_owned(),
        })
    }

    /// Archive or restore one session.
    ///
    /// Archiving stops a running turn first, cancels every prompt still
    /// queued for the session, and writes the flag through its own
    /// statement. Restoring only clears the flag: it never starts a turn and
    /// never widens what the session may reach.
    ///
    /// Nothing here holds the state guard across a driver call or across a
    /// helper that takes the guard itself: the driver's threads take it, and
    /// `abandon_remote_prompt` takes it, so both run with the guard
    /// released.
    fn archive_remote_session(
        &self,
        provenance: &fintwind_protocol::submission::RemoteProvenance,
        session_id: Uuid,
        archived: bool,
    ) -> anyhow::Result<ResponsePayload> {
        if archived {
            // A busy task is stopped before it is tidied away: an archive
            // that raced a running turn would hide a turn nobody can see.
            // A stop that cannot even be attempted refuses the archive
            // instead of recording a tidy state the turn then contradicts.
            if !self.stop_remote_session_turn(session_id)? {
                bail!("the task could not be stopped before archiving");
            }
            // Every prompt still waiting for this session is cancelled: an
            // archived task never runs anything. The rows are read first,
            // because each abandon takes the state guard itself.
            let waiting: Vec<(Uuid, Uuid, String)> = self
                .task_store
                .remote_prompts()?
                .into_iter()
                .filter(|row| row.session_id == session_id)
                .map(|row| (row.operation_id, row.submission_id, row.state))
                .collect();
            for (operation_id, submission_id, state) in waiting {
                if fintwind_protocol::submission::RemotePromptState::parse(&state)
                    .is_some_and(|state| state.is_pending())
                {
                    self.abandon_remote_prompt(session_id, submission_id)?;
                }
                self.task_store.delete_remote_prompt(operation_id)?;
            }
        }
        let mut state = self.task_state.lock();
        let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            bail!("the session is unknown to this computer");
        };
        self.hydrate_submission_session(session)?;
        if session.archived == archived {
            return Ok(ResponsePayload::Ack);
        }
        session.archived = archived;
        state.mark_session_dirty(session_id);
        self.task_store.save(&mut state)?;
        drop(state);
        // The column is written separately so a later client save that never
        // mentioned the field cannot drop it.
        self.task_store.set_session_archived(session_id, archived)?;
        crate::opencode_diagnostics::record(
            if archived {
                "remote_session_archived"
            } else {
                "remote_session_restored"
            },
            0,
            0,
        );
        let _ = provenance;
        Ok(ResponsePayload::Ack)
    }

    /// Stop one session's running turn before it is archived.
    ///
    /// Returns whether the session is safe to archive: `false` when a turn
    /// is running and this daemon holds no runtime that could stop it.
    ///
    /// The driver call runs with no state guard held — the driver's own
    /// threads take that guard, so waiting on one while holding it is how a
    /// daemon deadlocks. A cancel is fire-and-forget, so the turn is settled
    /// by its own event rather than by this call returning; what this call
    /// guarantees is that a stop was actually attempted on a runtime that
    /// exists, never that one was confirmed.
    fn stop_remote_session_turn(&self, session_id: Uuid) -> anyhow::Result<bool> {
        let running = {
            let mut state = self.task_state.lock();
            let Some(session) = state
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
            else {
                bail!("the session is unknown to this computer");
            };
            self.hydrate_submission_session(session)?;
            session.status.is_busy()
                || session
                    .turns
                    .last()
                    .is_some_and(|turn| turn.status == crate::model::TurnStatus::Running)
        };
        if !running {
            return Ok(true);
        }
        let Some((_, driver)) = self.sessions.lock().get(&session_id).cloned() else {
            // A running turn with no runtime cannot be stopped from here.
            return Ok(false);
        };
        driver.cancel();
        Ok(true)
    }

    /// Persist one session's options on this computer.
    ///
    /// Every enum is decoded here, so a value the daemon does not know is
    /// refused instead of being stored as if it applied. A live session also
    /// gets the values through its driver; a session with no runtime yet
    /// only records them, so its next turn starts with them.
    fn set_remote_session_options(
        &self,
        provenance: &fintwind_protocol::submission::RemoteProvenance,
        session_id: Uuid,
        options: crate::WireSessionOptions,
    ) -> anyhow::Result<ResponsePayload> {
        let mode = decode_enum(&options.mode)?;
        let interaction_mode = decode_enum(&options.interaction_mode)?;
        let mut state = self.task_state.lock();
        let recorded = {
            let Some(session) = state
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
            else {
                bail!("the session is unknown to this computer");
            };
            self.hydrate_submission_session(session)?;
            session.runtime_mode = mode;
            session.interaction_mode = interaction_mode;
            session.model = options.model;
            session.reasoning_effort = options.reasoning_effort;
            session.service_tier = options.service_tier;
            session.context_window = options.context_window;
            crate::driver::SessionOptions {
                mode,
                interaction_mode,
                model: session.model.clone(),
                reasoning_effort: session.reasoning_effort.clone(),
                service_tier: session.service_tier.clone(),
                context_window: session.context_window.clone(),
            }
        };
        state.mark_session_dirty(session_id);
        self.task_store.save(&mut state)?;
        drop(state);
        let applied = self
            .sessions
            .lock()
            .get(&session_id)
            .map(|(_, driver)| driver.apply_options(recorded))
            .unwrap_or(false);
        crate::opencode_diagnostics::record("remote_session_options_set", 0, 0);
        let _ = provenance;
        Ok(ResponsePayload::OptionsApplied { applied })
    }
}

impl Backend for FintwindBackend {
    fn browser_tools(&self) -> Option<Arc<crate::browser_tools::BrowserTools>> {
        Some(self.browser_tools.clone())
    }
    fn handle(&self, request: Request, events: EventSink) -> anyhow::Result<ResponsePayload> {
        let session_id = request.session_id;
        let runtime_id = request.runtime_id;
        let checkpoint_skeletons = match &request.command {
            Command::SaveTaskStateCheckpoint { skeleton_session_ids, .. } => {
                Some(skeleton_session_ids.clone())
            }
            _ => None,
        };
        match request.command {
            Command::AcceptSubmission {
                project,
                session,
                submission,
            } => {
                let session = *session;
                let submission = *submission;
                if session.id != session_id
                    || submission.session_id != session_id
                    || session.project_id != project.id
                    || submission.receipt.state != SubmissionState::Preparing
                    || !session.detail_loaded
                    || !session.turns.last().is_some_and(|turn| {
                        turn.id == submission.receipt.id
                            && turn.status == crate::model::TurnStatus::Running
                    })
                {
                    bail!("invalid submission identity");
                }
                let mut state = self.task_state.lock();
                if let Some(existing) = state
                    .sessions
                    .iter_mut()
                    .find(|candidate| candidate.id == session_id)
                {
                    self.hydrate_submission_session(existing)?;
                }
                if self.removed_session_ids.lock().contains(&session_id)
                    || self.removed_project_ids.lock().contains(&project.id)
                {
                    bail!("submission target was removed");
                }
                if let Some(saved) = self.task_store.submission(submission.receipt.id)? {
                    if saved.session_id != session_id
                        || saved.receipt.input_id != submission.receipt.input_id
                        || saved.prompt != submission.prompt
                        || serde_json::to_value(&saved.files)?
                            != serde_json::to_value(&submission.files)?
                    {
                        bail!("submission ID conflicts with a saved input");
                    }
                    return Ok(ResponsePayload::SubmissionSaved {
                        receipt: saved.receipt,
                    });
                }
                if state
                    .sessions
                    .iter()
                    .find(|candidate| candidate.id == session_id)
                    .is_some_and(|existing| {
                        (existing.status.is_busy()
                            || fintwind_protocol::submission::has_unsettled_submissions(existing))
                            && existing
                                .turns
                                .last()
                                .is_none_or(|turn| turn.id != submission.receipt.id)
                    })
                {
                    bail!("session already has an active submission");
                }
                // Publish to daemon memory only after the database commit.
                let mut next = state.clone();
                if !next
                    .projects
                    .iter()
                    .any(|candidate| candidate.id == project.id)
                {
                    next.projects.push(project);
                }
                if let Some(existing) = next
                    .sessions
                    .iter_mut()
                    .find(|candidate| candidate.id == session_id)
                {
                    *existing = session;
                } else {
                    next.sessions.push(session);
                }
                next.mark_session_dirty(session_id);
                self.task_store
                    .save_with_submission(&mut next, Some(&submission))?;
                *state = next;
                self.active_preparations
                    .lock()
                    .insert(submission.receipt.id, events.clone());
                crate::opencode_diagnostics::submission("saved", &submission, 0, 0);
                Ok(ResponsePayload::SubmissionSaved {
                    receipt: submission.receipt,
                })
            }
            Command::AbandonSubmission { submission_id } => {
                let mut state = self.task_state.lock();
                let saved = self
                    .task_store
                    .submission(submission_id)?
                    .ok_or_else(|| anyhow!("submission is missing"))?;
                if saved.session_id != session_id {
                    bail!("submission belongs to another session");
                }
                self.active_preparations.lock().remove(&submission_id);
                if let Some(saved) = self.task_store.transition_submission(
                    submission_id,
                    SubmissionState::Preparing,
                    SubmissionState::NotSent,
                    None,
                )? {
                    crate::opencode_diagnostics::submission("preparation_failed", &saved, 0, 0);
                    if let Some(session) = state
                        .sessions
                        .iter_mut()
                        .find(|session| session.id == session_id)
                    {
                        self.task_store.hydrate(session)?;
                        if let Some(turn) = session
                            .turns
                            .iter_mut()
                            .find(|turn| turn.id == submission_id)
                        {
                            turn.submission = Some(saved.receipt);
                            if !turn.provider_turn_started
                                && turn.status == crate::model::TurnStatus::Running
                            {
                                turn.status = crate::model::TurnStatus::Failed;
                                turn.completed_at = Some(crate::model::unix_time());
                                session.status = SessionStatus::Idle;
                            }
                        }
                        state.mark_session_dirty(session_id);
                        self.task_store.save(&mut state)?;
                    }
                }
                let saved = self
                    .task_store
                    .submission(submission_id)?
                    .ok_or_else(|| anyhow!("submission is missing"))?;
                Ok(ResponsePayload::SubmissionSaved {
                    receipt: saved.receipt,
                })
            }
            Command::DispatchSubmission {
                submission_id,
                workspace,
            } => {
                let mut state = self.task_state.lock();
                if let Some(session) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    self.hydrate_submission_session(session)?;
                }
                let saved = self
                    .task_store
                    .submission(submission_id)?
                    .ok_or_else(|| anyhow!("submission is missing"))?;
                if saved.session_id != session_id {
                    bail!("submission belongs to another session");
                }
                if saved.receipt.state != SubmissionState::Preparing {
                    return Ok(ResponsePayload::SubmissionSaved {
                        receipt: saved.receipt,
                    });
                }
                let driver = {
                    let sessions = self.sessions.lock();
                    let (active_runtime_id, driver) = sessions
                        .get(&session_id)
                        .ok_or_else(|| anyhow!("submission runtime is unavailable"))?;
                    if *active_runtime_id != runtime_id {
                        bail!("submission runtime was replaced");
                    }
                    driver.clone()
                };
                let session = state
                    .sessions
                    .iter_mut()
                    .find(|candidate| candidate.id == session_id)
                    .ok_or_else(|| anyhow!("submission session is missing"))?;
                if !session
                    .turns
                    .last()
                    .is_some_and(|turn| turn.id == submission_id)
                {
                    bail!("submission turn was replaced");
                }
                session.workspace = workspace;
                state.mark_session_dirty(session_id);
                self.task_store.save(&mut state)?;
                let Some(saved) = self.task_store.transition_submission(
                    submission_id,
                    SubmissionState::Preparing,
                    SubmissionState::Dispatching,
                    None,
                )?
                else {
                    let saved = self
                        .task_store
                        .submission(submission_id)?
                        .ok_or_else(|| anyhow!("submission is missing"))?;
                    return Ok(ResponsePayload::SubmissionSaved {
                        receipt: saved.receipt,
                    });
                };
                crate::opencode_diagnostics::submission("dispatching", &saved, 0, 0);
                self.active_preparations.lock().remove(&submission_id);
                if let Some(turn) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                    .and_then(|session| session.turns.last_mut())
                {
                    turn.submission = Some(saved.receipt.clone());
                }
                if let Err(error) = driver.submit(saved.clone(), self.task_store.clone()) {
                    if let Some(saved) = self.task_store.transition_submission(
                        submission_id,
                        SubmissionState::Dispatching,
                        SubmissionState::NotSent,
                        None,
                    )? {
                        if let Some(session) = state
                            .sessions
                            .iter_mut()
                            .find(|session| session.id == session_id)
                        {
                            if let Some(turn) = session.turns.last_mut() {
                                turn.submission = Some(saved.receipt.clone());
                            }
                            session.finish_active_turn(crate::model::TurnStatus::Failed);
                            session.status = SessionStatus::Idle;
                        }
                        state.mark_session_dirty(session_id);
                        self.task_store.save(&mut state)?;
                        return Ok(ResponsePayload::SubmissionSaved {
                            receipt: saved.receipt,
                        });
                    }
                    return Err(error);
                }
                Ok(ResponsePayload::SubmissionSaved {
                    receipt: saved.receipt,
                })
            }
            Command::AttachSession => {
                let sessions = self.sessions.lock();
                let Some((runtime_id, driver)) = sessions.get(&session_id) else {
                    return Ok(ResponsePayload::SessionRuntime {
                        runtime_id: None,
                        supports_steer: false,
                    });
                };
                Ok(ResponsePayload::SessionRuntime {
                    runtime_id: Some(*runtime_id),
                    supports_steer: driver.supports_steer(),
                })
            }
            Command::GetSettings => Ok(ResponsePayload::Settings {
                settings: self.settings.get(),
            }),
            Command::UpdateSettings { settings } => {
                self.settings.replace(settings)?;
                // Persisting alone keeps the plugin on its previous state;
                // apply it so the change reaches models on the next request.
                self.browser_tools
                    .set_enabled(self.settings.get().browser_tools_enabled());
                Ok(ResponsePayload::Ack)
            }
            Command::ProbeProvider {
                binary_override,
                directory,
                discover_models,
                probe_version,
            } => {
                ensure_shell_environment();
                let mut probe = match binary_override.as_deref() {
                    override_value if discover_models || probe_version => {
                        crate::model::provider_probe(override_value)
                    }
                    override_value => {
                        crate::model::cached_provider_probe(override_value, directory.as_deref())
                    }
                };
                let version = probe_version
                    .then(|| {
                        probe
                            .path
                            .as_deref()
                            .and_then(crate::model::probe_provider_version)
                    })
                    .flatten();
                if discover_models {
                    probe = crate::model::discover_provider_models(probe, directory.as_deref());
                }
                Ok(ResponsePayload::ProviderProbe { probe, version })
            }
            Command::FetchPlanUsage {
                binary_override: _,
                cli_version: _,
            } => {
                let usage = crate::usage::fetch_opencode_go_plan_usage()?;
                Ok(ResponsePayload::PlanUsage { usage })
            }
            Command::LoadSkills { projects } => {
                let locations = crate::skills::skill_locations(&projects);
                Ok(ResponsePayload::SkillsCatalog {
                    catalog: crate::skills::scan_skills(&locations),
                })
            }
            Command::SetSkillsEnabled { dirs, enabled } => {
                for dir in dirs {
                    crate::skills::set_skill_enabled(&dir, enabled)
                        .map_err(|error| anyhow!(error))?;
                }
                Ok(ResponsePayload::Ack)
            }
            Command::TrashSkills { dirs } => {
                crate::skills::trash_skills(&dirs).map_err(|error| anyhow!(error))?;
                Ok(ResponsePayload::Ack)
            }
            Command::LoadTaskState => {
                let state = self.task_state.lock();
                Ok(ResponsePayload::TaskState {
                    projects: state.projects.clone(),
                    sessions: state
                        .sessions
                        .iter()
                        .map(AgentSession::list_projection)
                        .collect(),
                    default_cwd: self.default_cwd.clone(),
                    projectless_root: crate::projectless::workspace_root(),
                })
            }
            Command::SaveTaskState {
                projects,
                live_session_ids: _,
                sessions,
            }
            | Command::SaveTaskStateCheckpoint {
                projects,
                sessions,
                ..
            } => {
                let active_runtimes = self
                    .sessions
                    .lock()
                    .iter()
                    .map(|(session_id, (runtime_id, _))| (*session_id, *runtime_id))
                    .collect::<HashMap<_, _>>();
                let mut state = self.task_state.lock();
                let removed_session_ids = self.removed_session_ids.lock();
                let removed_project_ids = self.removed_project_ids.lock();
                for project in projects {
                    if removed_project_ids.contains(&project.id) {
                        continue;
                    }
                    if let Some(existing) = state
                        .projects
                        .iter_mut()
                        .find(|existing| existing.id == project.id)
                    {
                        *existing = project;
                    } else {
                        state.projects.push(project);
                    }
                }
                let sessions = sessions
                    .into_iter()
                    .filter(|session| {
                        !removed_session_ids.contains(&session.id)
                            && !removed_project_ids.contains(&session.project_id)
                    })
                    .collect::<Vec<_>>();
                drop(removed_session_ids);
                drop(removed_project_ids);
                let saved_ids = sessions
                    .iter()
                    .map(|session| session.id)
                    .collect::<Vec<_>>();
                for mut session in sessions {
                    if checkpoint_skeletons
                        .as_ref()
                        .is_some_and(|ids| ids.contains(&session.id))
                    {
                        // The process-local hydration marker is not serialized.
                        // A list-only edit must not erase the stored transcript.
                        session.detail_loaded = false;
                    }
                    if let Some(existing) = state
                        .sessions
                        .iter_mut()
                        .find(|existing| existing.id == session.id)
                    {
                        let preparing = existing.turns.iter().any(|turn| {
                            turn.submission
                                .as_ref()
                                .is_some_and(|receipt| receipt.state == SubmissionState::Preparing)
                        });
                        let missing_pending_turn = existing.turns.iter().any(|turn| {
                            turn.submission.as_ref().is_some_and(|receipt| {
                                receipt.state.is_unconfirmed()
                                    || turn.status == crate::model::TurnStatus::Running
                            }) && !session.turns.iter().any(|incoming| incoming.id == turn.id)
                        });
                        let reopens_settled = existing.turns.iter().any(|turn| {
                            turn.submission.is_some()
                                && turn.status != crate::model::TurnStatus::Running
                                && session.turns.iter().any(|incoming| {
                                    incoming.id == turn.id
                                        && incoming.status == crate::model::TurnStatus::Running
                                })
                        });
                        if !session.detail_loaded {
                            if session.updated_at >= existing.updated_at {
                                existing.title = session.title;
                                existing.auto_title = session.auto_title;
                                existing.model = session.model;
                                existing.updated_at = session.updated_at;
                            }
                        } else if preparing {
                            // Do not allow another client's workspace/options
                            // snapshot to change the already accepted input.
                            for queued in session.queued_messages {
                                if !existing
                                    .queued_messages
                                    .iter()
                                    .any(|item| item.id == queued.id)
                                {
                                    existing.queued_messages.push(queued);
                                }
                            }
                        } else if missing_pending_turn
                            || reopens_settled
                            || session_projection_precedes(
                                existing,
                                &session,
                                active_runtimes.get(&session.id).copied(),
                            )
                        {
                            merge_stale_session_metadata(existing, session);
                        } else {
                            self.task_store.hydrate_submission_receipts(&mut session)?;
                            preserve_daemon_checkpoints(existing, &mut session);
                            for turn in &mut session.turns {
                                if let Some(receipt) = existing
                                    .turns
                                    .iter()
                                    .find(|saved| saved.id == turn.id)
                                    .and_then(|saved| saved.submission.as_ref())
                                    && turn
                                        .submission
                                        .as_ref()
                                        .is_none_or(|incoming| !incoming.supersedes(receipt))
                                {
                                    turn.submission = Some(receipt.clone());
                                }
                            }
                            *existing = session;
                            if fintwind_protocol::submission::has_unsettled_submissions(existing)
                                && !existing.status.is_busy()
                            {
                                existing.status = SessionStatus::Working;
                            }
                        }
                    } else {
                        state.sessions.push(session);
                    }
                }
                let used_project_ids = state
                    .sessions
                    .iter()
                    .map(|session| session.project_id)
                    .collect::<std::collections::HashSet<_>>();
                state.projects.retain(|project| {
                    !project.is_projectless() || used_project_ids.contains(&project.id)
                });
                for session_id in &saved_ids {
                    state.mark_session_dirty(*session_id);
                }
                self.task_store.save(&mut state)?;
                let sessions = saved_ids
                    .into_iter()
                    .filter_map(|session_id| {
                        state
                            .sessions
                            .iter()
                            .find(|session| session.id == session_id)
                            .map(|session| {
                                if checkpoint_skeletons.is_some() {
                                    session.list_projection()
                                } else {
                                    session.clone()
                                }
                            })
                    })
                    .collect();
                Ok(ResponsePayload::TaskStateSaved { sessions })
            }
            Command::RemoveProject { project_id } => {
                self.remove_project_from_catalog(project_id)?;
                Ok(ResponsePayload::Ack)
            }
            Command::RemoveSession => {
                {
                    let mut state = self.task_state.lock();
                    self.removed_session_ids.lock().insert(session_id);
                    let project_id = state
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .map(|session| session.project_id);
                    state.sessions.retain(|session| session.id != session_id);
                    if let Some(project_id) = project_id {
                        let remove_project = state
                            .projects
                            .iter()
                            .find(|project| project.id == project_id)
                            .is_some_and(Project::is_projectless)
                            && !state
                                .sessions
                                .iter()
                                .any(|session| session.project_id == project_id);
                        if remove_project {
                            state.projects.retain(|project| project.id != project_id);
                        }
                    }
                    self.task_store.save(&mut state)?;
                }
                let removed = self.sessions.lock().remove(&session_id);
                drop(removed);
                Ok(ResponsePayload::Ack)
            }
            // ---- remote control: daemon-owned task, queue and tidy state ----
            //
            // Every command below is addressed by ids this daemon already
            // owns. None of them accepts a project, a session object, a
            // driver-start payload, a binary or a working directory from the
            // caller: the daemon resolves those from its own task state and
            // its own provider probe, so a remote client can only ever name
            // work this computer already has.
            Command::CreateRemoteTask {
                provenance,
                project_id,
            } => {
                let mut state = self.task_state.lock();
                if self.removed_project_ids.lock().contains(&project_id) {
                    bail!("remote task target project was removed");
                }
                if !state
                    .projects
                    .iter()
                    .any(|project| project.id == project_id)
                {
                    bail!("remote task target project is unknown");
                }
                // The computer's own new-task defaults, not a structural
                // default chosen here: the remembered model and its traits,
                // the same record a locally created task starts from.
                let session = state.new_session(project_id);
                let session_id = session.id;
                state.sessions.push(session);
                state.mark_session_dirty(session_id);
                self.task_store.save(&mut state)?;
                crate::opencode_diagnostics::record("remote_task_created", 0, 0);
                let _ = provenance;
                Ok(ResponsePayload::RemoteTaskCreated { session_id })
            }
            Command::AdmitRemotePrompt {
                provenance,
                session_id,
                operation_id,
                prompt,
                expires_at_ms,
                summary,
            } => self.admit_remote_prompt(
                provenance,
                session_id,
                operation_id,
                &prompt,
                expires_at_ms,
                &summary,
            ),
            Command::DispatchRemotePrompt {
                provenance,
                operation_id,
            } => self.dispatch_remote_prompt(provenance, operation_id, events.clone()),
            Command::CancelRemotePrompt {
                provenance,
                operation_id,
                cancel_operation_id,
            } => self.cancel_remote_prompt(&provenance, operation_id, cancel_operation_id),            Command::ListRemotePrompts { provenance } => {
                let prompts = self
                    .task_store
                    .remote_prompts()?
                    .into_iter()
                    .map(|row| fintwind_protocol::submission::RemotePromptRecord {
                        operation_id: row.operation_id,
                        submission_id: row.submission_id,
                        session_id: row.session_id,
                        device_id: row.device_id,
                        state: row.state,
                        summary: row.summary,
                        enqueued_at_ms: row.enqueued_at_ms,
                        expires_at_ms: row.expires_at_ms,
                        grant_version: row.grant_version,
                        // The durable cancel binding, so a host that restarts
                        // knows a cancellation was promised instead of
                        // dispatching the prompt.
                        cancel_operation_id: row.cancel_operation_id,
                        cancel_state: row.cancel_state,
                    })
                    .collect();
                let _ = provenance;
                Ok(ResponsePayload::RemotePrompts { prompts })
            }
            Command::QueryRemoteCancel {
                provenance,
                operation_id,
                cancel_operation_id,
            } => {
                // The authoritative answer to "did my cancellation hold?".
                // The prompt's own operation id is what is asked about — no
                // new id is minted — and the daemon reports what it durably
                // recorded rather than inferring an ending from whatever
                // happens to still exist.
                //
                // A missing prompt row proves nothing on its own: rows are
                // deleted when a session goes away and when a queue is
                // swept, so reading `absent` as a cancellation would be a
                // guess. The cancel-term record is the only thing that may
                // supply an ending once the row is gone, and without one the
                // honest answer is `unknown`.
                let row = self
                    .task_store
                    .remote_prompts()?
                    .into_iter()
                    .find(|row| row.operation_id == operation_id);
                if let Some(row) = row.as_ref()
                    && (row.device_id != provenance.device_id
                        || row.host_id != provenance.host_id.to_string()
                        || provenance.grant_version != row.grant_version)
                {
                    bail!("that prompt belongs to another device");
                }
                // Naming a cancellation asks about *that* one. One prompt may
                // carry more than one cancellation over its life, so the
                // unnamed query reports whichever ended last — this one must
                // never be answered with another's fate. The lookup is
                // purely a read: nothing is settled, re-sent, or released by
                // asking.
                if let Some(asked) = cancel_operation_id {
                    // Still waiting on the live row under exactly this id?
                    if let Some((bound, bound_state)) =
                        self.task_store.cancel_intent(operation_id)?
                        && bound == Some(asked)
                    {
                        return Ok(ResponsePayload::RemoteCancelState {
                            operation_id,
                            submission_id: row.as_ref().map(|row| row.submission_id),
                            cancel_operation_id: Some(asked),
                            cancel_state: bound_state,
                            host_id: row
                                .as_ref()
                                .and_then(|row| Uuid::parse_str(&row.host_id).ok()),
                            device_id: row.as_ref().map(|row| row.device_id),
                            grant_version: row.as_ref().map(|row| row.grant_version),
                        });
                    }
                    // Otherwise its recorded ending — but only if the record
                    // is about this prompt and belongs to the device asking,
                    // under the grant it was admitted under. Another
                    // device's ending is not this caller's business, and a
                    // re-paired device's new grant does not own the old
                    // cancellation either.
                    let term =
                        self.task_store
                            .cancel_term_by_id(asked, operation_id)?;
                    if let Some(term) = term.as_ref()
                        && (term.device_id != provenance.device_id
                            || term.host_id != provenance.host_id.to_string()
                            || provenance.grant_version != term.grant_version)
                    {
                        return Ok(ResponsePayload::RemoteCancelState {
                            operation_id,
                            submission_id: row.as_ref().map(|row| row.submission_id),
                            cancel_operation_id: Some(asked),
                            cancel_state: "unknown".to_owned(),
                            host_id: None,
                            device_id: None,
                            grant_version: None,
                        });
                    }
                    if let Some(term) = term {
                        return Ok(ResponsePayload::RemoteCancelState {
                            operation_id,
                            submission_id: Some(term.submission_id),
                            cancel_operation_id: Some(term.cancel_operation_id),
                            cancel_state: term.state,
                            host_id: Uuid::parse_str(&term.host_id).ok(),
                            device_id: Some(term.device_id),
                            grant_version: Some(term.grant_version),
                        });
                    }
                    // No binding and no recorded ending for this id. The
                    // honest answer is `unknown`; a missing record never
                    // becomes a cancellation.
                    return Ok(ResponsePayload::RemoteCancelState {
                        operation_id,
                        submission_id: row.as_ref().map(|row| row.submission_id),
                        cancel_operation_id: Some(asked),
                        cancel_state: "unknown".to_owned(),
                        host_id: row
                            .as_ref()
                            .and_then(|row| Uuid::parse_str(&row.host_id).ok()),
                        device_id: row.as_ref().map(|row| row.device_id),
                        grant_version: row.as_ref().map(|row| row.grant_version),
                    });
                }
                match self.task_store.cancel_intent(operation_id)? {
                    Some((cancel_operation_id, state)) => Ok(ResponsePayload::RemoteCancelState {
                        operation_id,
                        submission_id: row.as_ref().map(|row| row.submission_id),
                        cancel_operation_id,
                        cancel_state: state,
                        // Still waiting, so the provenance is the live row's.
                        host_id: row
                            .as_ref()
                            .and_then(|row| Uuid::parse_str(&row.host_id).ok()),
                        device_id: row.as_ref().map(|row| row.device_id),
                        grant_version: row.as_ref().map(|row| row.grant_version),
                    }),
                    None => {
                        // Nothing is waiting on a live row. If this prompt's
                        // cancellation already ended, that recorded ending is
                        // the answer — provenance included, so the host can
                        // confirm it still belongs to the device asking;
                        // otherwise the row's own state is the truth about the
                        // prompt, reported as such rather than dressed up as a
                        // cancellation.
                        let term = self.task_store.cancel_term_by_prompt(operation_id)?;
                        let cancel_state = match term.as_ref() {
                            Some(term) => term.state.clone(),
                            None => row
                                .as_ref()
                                .map(|row| row.state.clone())
                                .unwrap_or_else(|| "unknown".to_owned()),
                        };
                        Ok(ResponsePayload::RemoteCancelState {
                            operation_id,
                            submission_id: row
                                .as_ref()
                                .map(|row| row.submission_id)
                                .or(term.as_ref().map(|term| term.submission_id)),
                            cancel_operation_id: term
                                .as_ref()
                                .map(|term| term.cancel_operation_id),
                            cancel_state,
                            host_id: term
                                .as_ref()
                                .and_then(|term| Uuid::parse_str(&term.host_id).ok())
                                .or_else(|| {
                                    row.as_ref()
                                        .and_then(|row| Uuid::parse_str(&row.host_id).ok())
                                }),
                            device_id: term
                                .as_ref()
                                .map(|term| term.device_id)
                                .or_else(|| row.as_ref().map(|row| row.device_id)),
                            grant_version: term
                                .as_ref()
                                .map(|term| term.grant_version)
                                .or_else(|| row.as_ref().map(|row| row.grant_version)),
                        })
                    }
                }
            }
            Command::ArchiveRemoteSession {
                provenance,
                session_id,
            } => self.archive_remote_session(&provenance, session_id, true),
            Command::RestoreRemoteSession {
                provenance,
                session_id,
            } => self.archive_remote_session(&provenance, session_id, false),
            Command::SetRemoteSessionOptions {
                provenance,
                session_id,
                options,
            } => self.set_remote_session_options(&provenance, session_id, options),
            Command::HydrateSession { session_id } => {
                let mut state = self.task_state.lock();
                let session = if let Some(session) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    self.task_store.hydrate(session)?;
                    Some(session.clone())
                } else {
                    None
                };
                Ok(ResponsePayload::Session { session })
            }
            Command::SearchSessionMessages { query, limit } => {
                let matches = self.task_store.session_message_search(query, limit)()?;
                Ok(ResponsePayload::SessionMessageMatches { matches })
            }
            Command::LoadComposerDrafts => Ok(ResponsePayload::ComposerDrafts {
                drafts: self.composer_drafts.load()?,
            }),
            Command::SaveComposerDrafts { drafts, generation } => {
                self.composer_drafts.save(drafts, generation)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ApplyComposerDraftChanges { changes } => {
                self.composer_drafts.apply_changes(changes)?;
                Ok(ResponsePayload::Ack)
            }
            Command::StoreBlob { mime_type, bytes } => {
                if bytes.len() > crate::attachments::MAX_PROMPT_FILE_BYTES {
                    bail!("attachment is larger than 20 MB");
                }
                let reference = self
                    .task_store
                    .blobs()
                    .store_image_bytes(&mime_type, &bytes)?;
                let path = self
                    .task_store
                    .blobs()
                    .path_for(&reference)
                    .ok_or_else(|| anyhow!("stored blob has no daemon path"))?;
                Ok(ResponsePayload::BlobStored { reference, path })
            }
            Command::ImportAttachment { name, upload } => Ok(ResponsePayload::AttachmentStored {
                attachment: self.attachments.import(&name, upload)?,
            }),
            Command::ImportPathAttachment { path } => Ok(ResponsePayload::AttachmentStored {
                attachment: self.attachments.import_path(&path)?,
            }),
            Command::ReadBlob { reference } => {
                let path = self
                    .task_store
                    .blobs()
                    .path_for(&reference)
                    .ok_or_else(|| anyhow!("invalid blob reference"))?;
                Ok(ResponsePayload::BlobData {
                    bytes: std::fs::read(path)?,
                })
            }
            Command::ReadAttachment { reference, path } => Ok(ResponsePayload::BlobData {
                bytes: self.attachments.read_file(&reference, &path)?,
            }),
            Command::SweepBlobs => {
                self.task_store.blob_sweep()();
                Ok(ResponsePayload::Ack)
            }
            Command::ForkSessionFromResponse { turn_count } => {
                let (session, checkpoint_warning) =
                    self.fork_session_from_response(session_id, turn_count)?;
                Ok(ResponsePayload::SessionForked {
                    session,
                    checkpoint_warning,
                })
            }
            Command::RewindSessionToMessage { turn_count } => {
                let (session, cleanup_warning) =
                    self.rewind_session_to_message(session_id, turn_count)?;
                Ok(ResponsePayload::SessionRewound {
                    session,
                    cleanup_warning,
                })
            }
            Command::ForkProviderSession { request } => {
                Ok(ResponsePayload::ProviderSessionForked {
                    result: fork_provider_session(request)?,
                })
            }
            Command::ListProviderSessions {
                binary,
                directory,
                tracked_session_ids,
            } => {
                // Goes through the binary's private global server; the
                // directory rides along as per-request location data.
                // Blocking I/O, so this runs on the request thread.
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let sessions = crate::driver::native::list_sessions_with_tracked(
                    &server,
                    &directory.to_string_lossy(),
                    &tracked_session_ids,
                )?;
                Ok(ResponsePayload::ProviderSessions { sessions })
            }
            Command::FetchNativeTranscript {
                binary,
                directory,
                session_id,
            } => {
                let transcript = self.fetch_native_transcript(&binary, &directory, &session_id)?;
                Ok(ResponsePayload::NativeTranscript { transcript })
            }
            Command::FetchNativeTurnStats {
                binary,
                directory,
                session_id,
                step_ids,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let stats =
                    crate::driver::native::fetch_turn_stats(&server, &session_id, &step_ids)?;
                Ok(ResponsePayload::NativeTurnStats { stats })
            }
            Command::GenerateSessionText {
                generation_id,
                binary,
                directory,
                session_id,
                question,
                model,
            } => {
                let text = self.side_generations.generate(
                    generation_id,
                    &binary,
                    &directory,
                    &session_id,
                    &question,
                    model.as_ref(),
                )?;
                Ok(match text {
                    Some(text) => ResponsePayload::SessionTextGenerated { text },
                    None => ResponsePayload::SessionGenerationModelNotSynced,
                })
            }
            Command::CancelSessionGeneration { generation_id } => {
                self.side_generations.cancel(generation_id);
                Ok(ResponsePayload::Ack)
            }
            Command::FetchUsageStats {
                binary,
                directory,
                detailed_day,
            } => {
                // One pass over the whole session store; `directory` only
                // scopes the request's location. Blocking I/O, so
                // this runs on the request thread like the other
                // sessionless OpenCode reads.
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let stats = crate::driver::native::fetch_usage_stats(&server, detailed_day)?;
                Ok(ResponsePayload::UsageStats { stats })
            }
            Command::RenameProviderSession {
                binary,
                directory,
                session_id,
                title,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                crate::driver::native::rename_session(&server, &session_id, &title)?;
                Ok(ResponsePayload::Ack)
            }
            Command::DeleteProviderSession {
                binary,
                directory,
                session_id,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                crate::driver::native::delete_session(&server, &session_id)?;
                Ok(ResponsePayload::Ack)
            }
            Command::FetchIntegrations { binary, directory } => {
                // One round trip to the private global OpenCode server; the
                // connection list is the authorization truth. Blocking I/O,
                // so this runs on the request thread like the other
                // sessionless OpenCode reads.
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let integrations = crate::driver::native::list_integrations(&server)?;
                Ok(ResponsePayload::Integrations { integrations })
            }
            Command::AuthorizeProvider {
                binary,
                directory,
                provider_id,
                key,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                crate::driver::native::authorize_integration(&server, &provider_id, &key)?;
                Ok(ResponsePayload::Ack)
            }
            Command::LogoutProvider {
                binary,
                directory,
                provider_id,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                crate::driver::native::logout_integration(&server, &provider_id)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ProbeBuiltinProvider {
                binary,
                directory,
                provider_id,
            } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let models = crate::driver::native::probe_provider_models(
                    &server,
                    &directory.to_string_lossy(),
                    &provider_id,
                )?;
                Ok(ResponsePayload::BuiltinProviderProbed { models })
            }
            Command::AuthenticateMcpServer {
                binary,
                directory,
                name,
            } => {
                crate::mcp_auth::authenticate(&binary, &directory, &name)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ListMcpServerStatuses { binary, directory } => {
                let server = crate::opencode_pool::acquire(&binary, &directory)?;
                let statuses = crate::driver::native::list_mcp_statuses(
                    &server,
                    &directory.to_string_lossy(),
                )?;
                Ok(ResponsePayload::McpServerStatuses { statuses })
            }
            Command::CancelAuthenticateMcpServer { name } => {
                crate::mcp_auth::cancel(&name);
                Ok(ResponsePayload::Ack)
            }
            Command::Workspace {
                operation:
                    WorkspaceOperation::CaptureTurn {
                        cwd,
                        session_id,
                        turn_count,
                    },
            } => Ok(ResponsePayload::Workspace {
                result: WorkspaceResult::Checkpoint {
                    checkpoint: self.capture_turn_checkpoint(cwd, session_id, turn_count)?,
                },
            }),
            Command::Workspace { operation } => Ok(ResponsePayload::Workspace {
                result: crate::workspace::execute(operation)?,
            }),
            Command::OpenTerminal { cwd, cols, rows } => {
                ensure_shell_environment();
                let terminal = crate::terminal::DaemonTerminal::open(&cwd, cols, rows, events)?;
                let previous = self
                    .terminals
                    .lock()
                    .insert(session_id, (runtime_id, terminal));
                drop(previous);
                Ok(ResponsePayload::Ack)
            }
            Command::WriteTerminal { data } => {
                let terminals = self.terminals.lock();
                let (active_runtime_id, terminal) = terminals
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("daemon terminal {session_id} is not running"))?;
                if *active_runtime_id != runtime_id {
                    bail!(
                        "daemon terminal {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                    );
                }
                terminal.write(data)?;
                Ok(ResponsePayload::Ack)
            }
            Command::ResizeTerminal { cols, rows } => {
                let terminals = self.terminals.lock();
                let (active_runtime_id, terminal) = terminals
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("daemon terminal {session_id} is not running"))?;
                if *active_runtime_id != runtime_id {
                    bail!(
                        "daemon terminal {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                    );
                }
                terminal.resize(cols, rows);
                Ok(ResponsePayload::Ack)
            }
            Command::CloseTerminal => {
                let removed = {
                    let mut terminals = self.terminals.lock();
                    if let Some((active_runtime_id, _)) = terminals.get(&session_id) {
                        if *active_runtime_id != runtime_id {
                            bail!(
                                "daemon terminal {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                            );
                        }
                    }
                    terminals.remove(&session_id)
                };
                drop(removed);
                Ok(ResponsePayload::Ack)
            }
            Command::Start { options } => {
                let previous = self.sessions.lock().remove(&session_id);
                drop(previous);
                let options = DriverStartOptions {
                    binary: options.binary,
                    cwd: options.cwd,
                    mode: decode_enum(&options.mode)?,
                    interaction_mode: decode_enum(&options.interaction_mode)?,
                    model: options.model,
                    reasoning_effort: options.reasoning_effort,
                    service_tier: options.service_tier,
                    context_window: options.context_window,
                    agent_preset: options.agent_preset,
                    provider_cursor: options
                        .provider_cursor
                        .map(serde_json::from_value)
                        .transpose()
                        .context("daemon received an invalid provider cursor")?,
                    // The daemon keys runtimes by the task's own UUID, so the
                    // session metadata can record it without the wire
                    // carrying a field the UI would only copy through.
                    task_id: Some(session_id.to_string()),
                };
                let (wake, _wake_events) = smol::channel::bounded(1);
                let (event_sender, event_receiver) = driver::event_channel(wake);
                let handle = driver::start_local(
                    options,
                    event_sender,
                    crate::browser_tools::BrowserToolRuntime {
                        registry: self.browser_tools.clone(),
                        session_id,
                        runtime_id,
                    },
                )?;
                let supports_steer = handle.supports_steer();
                let task_state = self.task_state.clone();
                let task_store = self.task_store.clone();
                let active_runtimes = self.sessions.clone();
                std::thread::Builder::new()
                    .name(format!("fintwind-daemon-events-{session_id}"))
                    .spawn(move || {
                        while let Ok(event) = event_receiver.recv() {
                            if let DriverEvent::SubmissionUpdated(receipt) = &event {
                                let mut state = task_state.lock();
                                let current_runtime = active_runtimes
                                    .lock()
                                    .get(&session_id)
                                    .is_some_and(|(active, _)| *active == runtime_id);
                                if let Some(turn) = state
                                    .sessions
                                    .iter_mut()
                                    .find(|session| session.id == session_id && current_runtime)
                                    .and_then(|session| {
                                        session.turns.iter_mut().find(|turn| turn.id == receipt.id)
                                    })
                                {
                                    if turn
                                        .submission
                                        .as_ref()
                                        .is_none_or(|previous| receipt.supersedes(previous))
                                    {
                                        turn.submission = Some(receipt.clone());
                                    }
                                }
                            }
                            if let DriverEvent::TurnFinished { success, .. } = &event {
                                let mut state = task_state.lock();
                                let current_runtime = active_runtimes
                                    .lock()
                                    .get(&session_id)
                                    .is_some_and(|(active, _)| *active == runtime_id);
                                if let Some(session) = state
                                    .sessions
                                    .iter_mut()
                                    .find(|session| session.id == session_id && current_runtime)
                                    && session
                                        .turns
                                        .last()
                                        .and_then(|turn| turn.submission.as_ref())
                                        .is_some_and(|receipt| {
                                            matches!(
                                                receipt.state,
                                                SubmissionState::Accepted
                                                    | SubmissionState::Dispatching
                                                    | SubmissionState::Unknown
                                                    | SubmissionState::Rejected
                                            )
                                        })
                                {
                                    session.finish_active_turn(if *success {
                                        crate::model::TurnStatus::Completed
                                    } else {
                                        crate::model::TurnStatus::Failed
                                    });
                                    session.status = if *success {
                                        SessionStatus::Idle
                                    } else {
                                        SessionStatus::Failed
                                    };
                                    state.mark_session_dirty(session_id);
                                    // Publish the verified execution boundary
                                    // before a client can submit its successor.
                                    let _ = task_store.save(&mut state);
                                }
                            }
                            let wire = event_to_wire(event).unwrap_or_else(|error| {
                                WireDriverEvent::new(
                                    "error",
                                    Value::String(format!(
                                        "could not encode daemon event: {error}"
                                    )),
                                )
                            });
                            if events.send(wire).is_err() {
                                break;
                            }
                        }
                    })
                    .context("could not start daemon event forwarding thread")?;
                self.sessions
                    .lock()
                    .insert(session_id, (runtime_id, handle));
                Ok(ResponsePayload::Started { supports_steer })
            }
            Command::CloseSession => {
                let removed = {
                    let mut sessions = self.sessions.lock();
                    sessions
                        .get(&session_id)
                        .is_some_and(|(active_runtime_id, _)| *active_runtime_id == runtime_id)
                        .then(|| sessions.remove(&session_id))
                        .flatten()
                };
                drop(removed);
                Ok(ResponsePayload::Ack)
            }
            command => {
                let driver = {
                    let sessions = self.sessions.lock();
                    let (active_runtime_id, driver) = sessions
                        .get(&session_id)
                        .ok_or_else(|| anyhow!("daemon session {session_id} is not running"))?;
                    if *active_runtime_id != runtime_id {
                        bail!(
                            "daemon session {session_id} belongs to runtime {active_runtime_id}, not {runtime_id}"
                        );
                    }
                    driver.clone()
                };
                handle_driver_command(&driver, command)
            }
        }
    }

    fn shutdown(&self) {
        let sessions = std::mem::take(&mut *self.sessions.lock());
        drop(sessions);
        let terminals = std::mem::take(&mut *self.terminals.lock());
        drop(terminals);
        crate::opencode_pool::shutdown_all();
    }
}

fn session_projection_precedes(
    existing: &AgentSession,
    incoming: &AgentSession,
    active_runtime_id: Option<Uuid>,
) -> bool {
    let existing_cursor = existing.runtime_event_cursor;
    let incoming_cursor = incoming.runtime_event_cursor;
    if let Some(active_runtime_id) = active_runtime_id {
        let existing_is_active =
            existing_cursor.is_some_and(|cursor| cursor.runtime_id == active_runtime_id);
        let incoming_is_active =
            incoming_cursor.is_some_and(|cursor| cursor.runtime_id == active_runtime_id);
        if existing_is_active != incoming_is_active {
            return existing_is_active;
        }
    }
    match (existing_cursor, incoming_cursor) {
        (Some(existing), Some(incoming))
            if existing.runtime_id == incoming.runtime_id && existing.epoch == incoming.epoch =>
        {
            incoming.sequence < existing.sequence
        }
        (Some(_), None) if existing.status.is_busy() => true,
        _ => incoming.updated_at < existing.updated_at,
    }
}

fn merge_stale_session_metadata(existing: &mut AgentSession, incoming: AgentSession) {
    if incoming.updated_at >= existing.updated_at {
        existing.title = incoming.title;
        existing.project_id = incoming.project_id;
        existing.workspace = incoming.workspace;
        existing.provider = incoming.provider;
        existing.model = incoming.model;
        existing.runtime_mode = incoming.runtime_mode;
        existing.interaction_mode = incoming.interaction_mode;
        existing.reasoning_effort = incoming.reasoning_effort;
        existing.service_tier = incoming.service_tier;
        existing.context_window = incoming.context_window;
        existing.agent_preset = incoming.agent_preset;
        existing.updated_at = incoming.updated_at;
        existing.last_reply_at = incoming.last_reply_at.or(existing.last_reply_at);
    }
    for queued in incoming.queued_messages {
        if !existing
            .queued_messages
            .iter()
            .any(|candidate| candidate.id == queued.id)
        {
            existing.queued_messages.push(queued);
        }
    }
}

/// Ending checkpoints are produced and stored by the daemon. A second client
/// may still save a projection created just before capture completed; never
/// let that stale projection erase the canonical Git snapshot.
fn preserve_daemon_checkpoints(existing: &AgentSession, incoming: &mut AgentSession) {
    for turn in &mut incoming.turns {
        let Some(checkpoint) = existing
            .turns
            .iter()
            .find(|candidate| candidate.turn_count == turn.turn_count)
            .and_then(|candidate| candidate.checkpoint.as_ref())
            .filter(|checkpoint| {
                matches!(
                    checkpoint.status,
                    CheckpointStatus::Ready | CheckpointStatus::Unavailable
                )
            })
        else {
            continue;
        };
        turn.checkpoint = Some(checkpoint.clone());
    }
}

impl FintwindBackend {
    /// Fork a response using only daemon-host state.
    ///
    /// A browser must never reconstruct or persist this operation itself:
    /// provider-native sessions, checkpoint refs, and the task database all
    /// belong to the daemon and may be on another machine.
    fn fork_session_from_response(
        &self,
        session_id: Uuid,
        turn_count: usize,
    ) -> anyhow::Result<(AgentSession, Option<String>)> {
        let (source, cwd, fork_title) = {
            let mut state = self.task_state.lock();
            let source_index = state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
                .ok_or_else(|| anyhow!("the source task is unavailable"))?;
            self.task_store
                .hydrate(&mut state.sessions[source_index])
                .context("could not load the source task")?;
            let source = state.sessions[source_index].clone();
            let project = state
                .projects
                .iter()
                .find(|project| project.id == source.project_id)
                .ok_or_else(|| anyhow!("the source task project is unavailable"))?;
            let cwd = source.workspace.path().unwrap_or(&project.path).to_owned();
            let fork_title = next_response_fork_title(
                source.display_title(),
                state
                    .sessions
                    .iter()
                    .filter(|session| session.project_id == source.project_id)
                    .map(AgentSession::display_title),
            );
            (source, cwd, fork_title)
        };

        validate_response_fork(&source, turn_count)?;
        let provider_turn_count = source
            .turns
            .iter()
            .take(turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count();
        let (provider_cursor, message_ids) =
            self.fork_provider_response(&source, &cwd, provider_turn_count)?;
        let mut forked = source
            .fork_through_turn(turn_count, provider_cursor, &fork_title)
            .ok_or_else(|| anyhow!("the selected response cannot be copied"))?;
        if !message_ids.is_empty() {
            for turn in &mut forked.turns {
                if let Some(message_id) = turn.provider_resume_at.as_mut()
                    && let Some(remapped) = message_ids.get(message_id)
                {
                    *message_id = remapped.clone();
                }
            }
        }

        let fork_id = forked.id;
        for turn in &mut forked.turns {
            if let Some(checkpoint) = turn.checkpoint.as_mut() {
                checkpoint.git_ref =
                    crate::checkpoint::checkpoint_ref(fork_id, checkpoint.turn_count);
            }
        }
        let checkpoint_warning =
            crate::checkpoint::copy_session_refs(&cwd, source.id, fork_id, turn_count)
                .err()
                .map(|error| error.to_string());

        let mut state = self.task_state.lock();
        state.push_session(forked.clone());
        if let Err(error) = self.task_store.save(&mut state) {
            state.sessions.retain(|session| session.id != fork_id);
            let _ = crate::checkpoint::delete_all_session_refs(&cwd, fork_id);
            return Err(error).context("could not save the forked task");
        }
        Ok((forked, checkpoint_warning))
    }

    /// Restore the daemon-host worktree, provider conversation, and stored
    /// transcript to immediately before one user turn.
    fn rewind_session_to_message(
        &self,
        session_id: Uuid,
        turn_count: usize,
    ) -> anyhow::Result<(AgentSession, Option<String>)> {
        let (source, cwd) = {
            let mut state = self.task_state.lock();
            let source_index = state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
                .ok_or_else(|| anyhow!("the task is unavailable"))?;
            self.task_store
                .hydrate(&mut state.sessions[source_index])
                .context("could not load the task")?;
            let source = state.sessions[source_index].clone();
            let project = state
                .projects
                .iter()
                .find(|project| project.id == source.project_id)
                .ok_or_else(|| anyhow!("the task project is unavailable"))?;
            let cwd = source.workspace.path().unwrap_or(&project.path).to_owned();
            (source, cwd)
        };
        validate_message_rewind(&source, turn_count)?;

        let retained_turn_count = turn_count.saturating_sub(1);
        let rollback_turns = source.provider_turns_after(retained_turn_count);
        if rollback_turns > 0 {
            // OpenCode's revert marks a native user-message boundary, so the
            // index must count only turns that reached the provider — a
            // locally failed turn has no native message to retain.
            let provider_turn_count = source
                .turns
                .iter()
                .take(retained_turn_count)
                .filter(|turn| turn.provider_turn_started)
                .count();
            // OpenCode's revert rewrites the worktree with its own snapshot,
            // so no fintwind git checkpoint is captured or restored here. The
            // marker is the work: it hides the dropped turns from the next
            // model call and rolls their file changes back.
            let binary = self.provider_binary()?;
            self.rewind_provider_response(
                &source,
                &cwd,
                &binary,
                provider_turn_count,
                rollback_turns,
            )?;
        }
        // The native session id survives OpenCode's revert, so the stored
        // cursor keeps working no matter how many turns were dropped.
        let provider_cursor = source.provider_cursor.clone();

        // Dropping a resident source driver prevents its late events from
        // racing the rewound transcript; the next prompt starts fresh and
        // OpenCode's pending cleanup physically removes the dropped turns.
        let removed = self.sessions.lock().remove(&session_id);
        drop(removed);

        let mut rewound = source.clone();
        rewound.provider_cursor = provider_cursor;
        rewound.truncate_after_turn(retained_turn_count);
        rewound.status = SessionStatus::Idle;

        let mut state = self.task_state.lock();
        let existing = state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("the task was removed while it was being rewound"))?;
        *existing = rewound.clone();
        state.mark_session_dirty(session_id);
        self.task_store
            .save(&mut state)
            .context("could not save the rewound task")?;
        Ok((rewound, None))
    }

    fn fork_provider_response(
        &self,
        source: &AgentSession,
        cwd: &Path,
        provider_turn_count: usize,
    ) -> anyhow::Result<(ProviderResumeCursor, HashMap<String, String>)> {
        let Some(ProviderResumeCursor::OpenCode { session_id }) = source.provider_cursor.as_ref()
        else {
            bail!("OpenCode's native session is unavailable");
        };
        let fork = fork_provider_session(ProviderSessionForkRequest::OpenCode {
            binary: self.provider_binary()?,
            cwd: cwd.to_owned(),
            session_id: session_id.clone(),
            turn_count: provider_turn_count,
        })?;
        Ok((fork.cursor, HashMap::new()))
    }

    fn rewind_provider_response(
        &self,
        source: &AgentSession,
        cwd: &Path,
        binary: &Path,
        provider_turn_count: usize,
        rollback_turns: usize,
    ) -> anyhow::Result<()> {
        if rollback_turns == 0 {
            return Ok(());
        }
        let Some(ProviderResumeCursor::OpenCode { session_id }) = source.provider_cursor.as_ref()
        else {
            bail!("OpenCode's native session is unavailable");
        };
        let _ = fork_provider_session(ProviderSessionForkRequest::OpenCodeRevert {
            binary: binary.to_owned(),
            cwd: cwd.to_owned(),
            session_id: session_id.clone(),
            turn_count: provider_turn_count,
        })?;
        Ok(())
    }

    fn provider_binary(&self) -> anyhow::Result<PathBuf> {
        ensure_shell_environment();
        crate::model::provider_probe(None)
            .path
            .ok_or_else(|| anyhow!("opencode is not installed on the daemon"))
    }
}

fn validate_message_rewind(source: &AgentSession, turn_count: usize) -> anyhow::Result<()> {
    if !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed) {
        bail!("stop the task before editing a prior message");
    }
    let Some(turn) = source
        .turns
        .iter()
        .find(|turn| turn.turn_count == turn_count)
    else {
        bail!("the selected message is unavailable");
    };
    if !source.messages.iter().any(|message| {
        message.turn_id == Some(turn.id) && message.role == crate::model::MessageRole::User
    }) {
        bail!("the selected user message is unavailable");
    }
    let rollback_turns = source.provider_turns_after(turn_count.saturating_sub(1));
    if rollback_turns > 0 && source.provider_cursor.is_none() {
        bail!("the provider conversation is unavailable");
    }
    Ok(())
}

fn validate_response_fork(source: &AgentSession, turn_count: usize) -> anyhow::Result<()> {
    if !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed) {
        bail!("stop the task before forking a response");
    }
    let cursor = source
        .provider_cursor
        .as_ref()
        .ok_or_else(|| anyhow!("the provider conversation is unavailable"))?;
    if cursor.provider() != source.provider {
        bail!("the provider conversation does not match this task");
    }
    if source
        .turns
        .get(turn_count.saturating_sub(1))
        .is_none_or(|turn| turn.turn_count != turn_count || !turn.provider_turn_started)
    {
        bail!("the selected response cannot be forked");
    }
    Ok(())
}

fn numbered_title_suffix(title: &str) -> Option<(&str, usize)> {
    let (base, suffix) = title.rsplit_once(" (")?;
    let number = suffix.strip_suffix(')')?.parse().ok()?;
    (!base.is_empty() && number >= 2).then_some((base, number))
}

fn next_response_fork_title<'a>(
    source_title: &str,
    existing_titles: impl IntoIterator<Item = &'a str>,
) -> String {
    let existing_titles = existing_titles.into_iter().collect::<Vec<_>>();
    let base = numbered_title_suffix(source_title)
        .filter(|(base, _)| existing_titles.iter().any(|title| title == base))
        .map_or(source_title, |(base, _)| base);
    let highest_number = existing_titles
        .iter()
        .filter_map(|title| {
            if *title == base {
                Some(1)
            } else {
                numbered_title_suffix(title)
                    .filter(|(candidate_base, _)| *candidate_base == base)
                    .map(|(_, number)| number)
            }
        })
        .max()
        .unwrap_or(1);
    format!("{base} ({})", highest_number.saturating_add(1).max(2))
}

fn fork_provider_session(
    request: ProviderSessionForkRequest,
) -> anyhow::Result<ProviderSessionFork> {
    let (cursor, message_ids, source_resume_at) = match request {
        ProviderSessionForkRequest::OpenCode {
            binary,
            cwd,
            session_id,
            turn_count,
        } => (
            crate::opencode_session::fork_session_at_turn(&binary, &cwd, &session_id, turn_count)?,
            HashMap::new(),
            None,
        ),
        // The revert keeps the session id, so the fork's cursor field just
        // carries the same session onward.
        ProviderSessionForkRequest::OpenCodeRevert {
            binary,
            cwd,
            session_id,
            turn_count,
        } => {
            let server = crate::opencode_pool::acquire(&binary, &cwd)?;
            crate::opencode_session::revert_session_at_message(&server, &session_id, turn_count)?;
            (
                ProviderResumeCursor::OpenCode {
                    session_id: session_id.clone(),
                },
                HashMap::new(),
                None,
            )
        }
        // Undo and redo never change the conversation identity either: a
        // staged boundary only decides what the next model call sees, and a
        // cleared one restores what staging hid.
        ProviderSessionForkRequest::OpenCodeUndoTurn {
            binary,
            cwd,
            session_id,
        } => {
            let server = crate::opencode_pool::acquire(&binary, &cwd)?;
            crate::opencode_session::undo_last_turn_on_server(&server, &session_id)?;
            (
                ProviderResumeCursor::OpenCode {
                    session_id: session_id.clone(),
                },
                HashMap::new(),
                None,
            )
        }
        ProviderSessionForkRequest::OpenCodeRedoTurn {
            binary,
            cwd,
            session_id,
        } => {
            let server = crate::opencode_pool::acquire(&binary, &cwd)?;
            crate::opencode_session::redo_turn_on_server(&server, &session_id)?;
            (
                ProviderResumeCursor::OpenCode {
                    session_id: session_id.clone(),
                },
                HashMap::new(),
                None,
            )
        }
    };
    Ok(ProviderSessionFork {
        cursor,
        message_ids,
        source_resume_at,
    })
}

fn handle_driver_command(
    driver: &DriverHandle,
    command: Command,
) -> anyhow::Result<ResponsePayload> {
    match command {
        Command::Prompt { prompt, files } => driver.prompt(prompt, files),
        Command::Steer { prompt, files } => driver.steer(prompt, files),
        Command::CompactSession => driver.compact(),
        Command::Cancel => driver.cancel(),
        Command::RefreshBackgroundWork => driver.refresh_background_work(),
        Command::StopBackgroundWork { key, control_id } => {
            driver.stop_background_work(
                serde_json::from_value(key).context("invalid background-work key")?,
                control_id,
            );
        }
        Command::Respond {
            request_id,
            option_id,
        } => driver.respond(request_id, option_id),
        Command::RespondUserInput {
            request_id,
            answers,
        } => driver.respond_user_input(request_id, answers),
        Command::ApplyOptions { options } => {
            return Ok(ResponsePayload::OptionsApplied {
                applied: driver.apply_options(SessionOptions {
                    mode: decode_enum(&options.mode)?,
                    interaction_mode: decode_enum(&options.interaction_mode)?,
                    model: options.model,
                    reasoning_effort: options.reasoning_effort,
                    service_tier: options.service_tier,
                    context_window: options.context_window,
                }),
            });
        }
        Command::Fork { turns_to_remove } => {
            let cursor = Some(serde_json::to_value(driver.fork(turns_to_remove)?)?);
            return Ok(ResponsePayload::Cursor { cursor });
        }
        Command::AttachSession
        | Command::AcceptSubmission { .. }
        | Command::DispatchSubmission { .. }
        | Command::AbandonSubmission { .. }
        | Command::Start { .. }
        | Command::GetSettings
        | Command::UpdateSettings { .. }
        | Command::ProbeProvider { .. }
        | Command::FetchPlanUsage { .. }
        | Command::LoadSkills { .. }
        | Command::SetSkillsEnabled { .. }
        | Command::TrashSkills { .. }
        | Command::LoadTaskState
        | Command::SaveTaskState { .. }
        | Command::SaveTaskStateCheckpoint { .. }
        | Command::RemoveSession
        | Command::RemoveProject { .. }
        | Command::HydrateSession { .. }
        | Command::SearchSessionMessages { .. }
        | Command::LoadComposerDrafts
        | Command::SaveComposerDrafts { .. }
        | Command::ApplyComposerDraftChanges { .. }
        | Command::StoreBlob { .. }
        | Command::ImportAttachment { .. }
        | Command::ImportPathAttachment { .. }
        | Command::ReadBlob { .. }
        | Command::ReadAttachment { .. }
        | Command::SweepBlobs
        | Command::ForkSessionFromResponse { .. }
        | Command::RewindSessionToMessage { .. }
        | Command::ForkProviderSession { .. }
        | Command::ListProviderSessions { .. }
        | Command::FetchNativeTranscript { .. }
        | Command::GenerateSessionText { .. }
        | Command::CancelSessionGeneration { .. }
        | Command::FetchNativeTurnStats { .. }
        | Command::FetchUsageStats { .. }
        | Command::RenameProviderSession { .. }
        | Command::DeleteProviderSession { .. }
        | Command::FetchIntegrations { .. }
        | Command::AuthorizeProvider { .. }
        | Command::LogoutProvider { .. }
        | Command::ProbeBuiltinProvider { .. }
        | Command::AuthenticateMcpServer { .. }
        | Command::ListMcpServerStatuses { .. }
        | Command::CancelAuthenticateMcpServer { .. }
        | Command::Workspace { .. }
        | Command::OpenTerminal { .. }
        | Command::WriteTerminal { .. }
        | Command::ResizeTerminal { .. }
        | Command::CloseTerminal
        | Command::CloseSession
        | Command::CreateRemoteTask { .. }
        | Command::AdmitRemotePrompt { .. }
        | Command::DispatchRemotePrompt { .. }
        | Command::CancelRemotePrompt { .. }
        | Command::ListRemotePrompts { .. }
        | Command::ArchiveRemoteSession { .. }
        | Command::RestoreRemoteSession { .. }
        | Command::SetRemoteSessionOptions { .. }
        | Command::QueryRemoteCancel { .. }
        | Command::BrowserList
        | Command::BrowserInvoke { .. } => {
            bail!("daemon received a command in the wrong dispatch path")
        }
    }
    Ok(ResponsePayload::Ack)
}

fn ensure_shell_environment() {
    static REFRESHED: OnceLock<()> = OnceLock::new();
    REFRESHED.get_or_init(|| {
        crate::command_env::refresh_from_default_shell();
    });
}

fn decode_enum<T: DeserializeOwned>(value: &str) -> anyhow::Result<T> {
    serde_json::from_value(Value::String(value.to_owned()))
        .with_context(|| format!("invalid protocol enum value {value:?}"))
}

pub fn encode_enum<T: Serialize>(value: T) -> anyhow::Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("protocol enum did not serialize as a string"))
}

fn event_to_wire(event: DriverEvent) -> anyhow::Result<WireDriverEvent> {
    let (kind, payload) = match event {
        DriverEvent::SubmissionUpdated(receipt) => {
            ("submissionUpdated", serde_json::to_value(receipt)?)
        }
        DriverEvent::RuntimeEventCursorAdvanced(_) => {
            bail!("client-only runtime cursors cannot be sent by the daemon")
        }
        DriverEvent::Connected { provider_cursor } => {
            ("connected", serde_json::to_value(provider_cursor)?)
        }
        DriverEvent::AgentPresetSelected(preset) => {
            ("agentPresetSelected", serde_json::to_value(preset)?)
        }
        DriverEvent::AutoTitleUpdated(title) => ("autoTitleUpdated", serde_json::to_value(title)?),
        DriverEvent::AvailableCommands(commands) => {
            ("availableCommands", serde_json::to_value(commands)?)
        }
        DriverEvent::TurnStarted => ("turnStarted", Value::Null),
        DriverEvent::TurnTranscriptReconciled {
            transcript,
            continuation,
        } => (
            "turnTranscriptReconciled",
            json!({ "transcript": transcript, "continuation": continuation }),
        ),
        DriverEvent::NativeSessionsChanged => ("nativeSessionsChanged", Value::Null),
        DriverEvent::NativeSessionMoved { native_session_id } => (
            "nativeSessionMoved",
            json!({ "nativeSessionId": native_session_id }),
        ),
        DriverEvent::TextStarted { part } => ("textStarted", json!({ "part": part })),
        DriverEvent::TextDelta { part, delta } => {
            ("textDelta", json!({ "part": part, "delta": delta }))
        }
        DriverEvent::TextEnded { part, text } => {
            ("textEnded", json!({ "part": part, "text": text }))
        }
        DriverEvent::ReasoningStarted { part } => ("reasoningStarted", json!({ "part": part })),
        DriverEvent::ReasoningDelta { part, delta } => {
            ("reasoningDelta", json!({ "part": part, "delta": delta }))
        }
        DriverEvent::ReasoningEnded { part, text } => {
            ("reasoningEnded", json!({ "part": part, "text": text }))
        }
        DriverEvent::Activity {
            id,
            kind,
            title,
            detail,
            complete,
        } => (
            "activity",
            json!({
                "id": id,
                "kind": kind,
                "title": title,
                "detail": detail,
                "complete": complete,
            }),
        ),
        DriverEvent::RichActivity(activity) => ("richActivity", serde_json::to_value(activity)?),
        DriverEvent::BackgroundWork(work) => ("backgroundWork", serde_json::to_value(work)?),
        DriverEvent::Permission {
            request_id,
            title,
            detail,
            options,
        } => (
            "permission",
            json!({
                "requestId": request_id,
                "title": title,
                "detail": detail,
                "options": options,
            }),
        ),
        DriverEvent::UserInputRequested {
            request_id,
            questions,
        } => (
            "userInputRequested",
            json!({
                "requestId": request_id,
                "questions": questions,
            }),
        ),
        DriverEvent::SteerAccepted { message } => ("steerAccepted", json!({ "message": message })),
        DriverEvent::SteerRejected { message, reason } => (
            "steerRejected",
            json!({ "message": message, "reason": reason }),
        ),
        DriverEvent::UsageUpdated {
            context_tokens,
            context_window,
            context_window_resolved,
            session_total,
            cache_read,
            prompt_tokens,
            latest,
        } => (
            "usageUpdated",
            json!({
                "contextTokens": context_tokens,
                "contextWindow": context_window,
                "contextWindowResolved": context_window_resolved,
                "sessionTotal": session_total,
                "cacheRead": cache_read,
                "promptTokens": prompt_tokens,
                "latest": latest,
            }),
        ),
        DriverEvent::PlanUsageUpdated(usage) => ("planUsageUpdated", serde_json::to_value(usage)?),
        DriverEvent::TurnStatsUpdated(stats) => ("turnStatsUpdated", serde_json::to_value(stats)?),
        DriverEvent::CompactionUpdated(state) => {
            ("compactionUpdated", serde_json::to_value(state)?)
        }
        DriverEvent::ProviderBusy => ("providerBusy", Value::Null),
        DriverEvent::ProviderRetry {
            attempt,
            message,
            action,
            next_at_ms,
        } => (
            "providerRetry",
            json!({
                "attempt": attempt,
                "message": message,
                "action": action,
                "nextAtMs": next_at_ms,
            }),
        ),
        DriverEvent::TurnFinished { success, summary } => (
            "turnFinished",
            json!({ "success": success, "summary": summary }),
        ),
        DriverEvent::Error(error) => ("error", Value::String(error)),
        DriverEvent::ProcessExited => ("processExited", Value::Null),
    };
    Ok(WireDriverEvent::new(kind, payload))
}

/// The reasoning fragment identity a wire payload carries, if any. An absent
/// key (a pre-keying peer) decodes to an empty string, which the app maps to
/// its phase-based fallback instead of part-bound reasoning.
fn reasoning_part_from_wire(payload: &Value) -> String {
    payload
        .get("part")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

pub fn event_from_wire(event: WireDriverEvent) -> anyhow::Result<DriverEvent> {
    let payload = event.payload;
    Ok(match event.kind.as_str() {
        "submissionUpdated" => DriverEvent::SubmissionUpdated(serde_json::from_value(payload)?),
        "connected" => DriverEvent::Connected {
            provider_cursor: serde_json::from_value(payload)?,
        },
        "agentPresetSelected" => DriverEvent::AgentPresetSelected(serde_json::from_value(payload)?),
        "autoTitleUpdated" => DriverEvent::AutoTitleUpdated(serde_json::from_value(payload)?),
        "availableCommands" => DriverEvent::AvailableCommands(serde_json::from_value(payload)?),
        "turnStarted" => DriverEvent::TurnStarted,
        "turnTranscriptReconciled" => DriverEvent::TurnTranscriptReconciled {
            transcript: serde_json::from_value(
                payload
                    .get("transcript")
                    .cloned()
                    .context("missing turn transcript")?,
            )?,
            continuation: payload
                .get("continuation")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "nativeSessionsChanged" => DriverEvent::NativeSessionsChanged,
        "nativeSessionMoved" => DriverEvent::NativeSessionMoved {
            native_session_id: payload
                .get("nativeSessionId")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missing moved native session id"))?
                .to_owned(),
        },
        "textStarted" => DriverEvent::TextStarted {
            part: reasoning_part_from_wire(&payload),
        },
        // A bare string payload is a pre-keying peer: the delta keeps the
        // phase-based fallback path on an empty part key.
        "textDelta" => {
            let (part, delta) = match payload {
                Value::String(delta) => (String::new(), delta),
                payload => (
                    reasoning_part_from_wire(&payload),
                    payload
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ),
            };
            DriverEvent::TextDelta { part, delta }
        }
        "textEnded" => DriverEvent::TextEnded {
            part: reasoning_part_from_wire(&payload),
            text: payload
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        // A bare string payload is a pre-keying peer: the delta keeps the
        // phase-based fallback path on an empty part key.
        "reasoningStarted" => DriverEvent::ReasoningStarted {
            part: reasoning_part_from_wire(&payload),
        },
        "reasoningDelta" => {
            let (part, delta) = match payload {
                Value::String(delta) => (String::new(), delta),
                payload => (
                    reasoning_part_from_wire(&payload),
                    payload
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ),
            };
            DriverEvent::ReasoningDelta { part, delta }
        }
        "reasoningEnded" => DriverEvent::ReasoningEnded {
            part: reasoning_part_from_wire(&payload),
            text: payload
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        "activity" => {
            let activity: ActivityWire = serde_json::from_value(payload)?;
            DriverEvent::Activity {
                id: activity.id,
                kind: activity.kind,
                title: activity.title,
                detail: activity.detail,
                complete: activity.complete,
            }
        }
        "richActivity" => DriverEvent::RichActivity(serde_json::from_value(payload)?),
        "backgroundWork" => DriverEvent::BackgroundWork(serde_json::from_value(payload)?),
        "permission" => {
            let permission: PermissionWire = serde_json::from_value(payload)?;
            DriverEvent::Permission {
                request_id: permission.request_id,
                title: permission.title,
                detail: permission.detail,
                options: permission.options,
            }
        }
        "userInputRequested" => {
            let request: UserInputWire = serde_json::from_value(payload)?;
            DriverEvent::UserInputRequested {
                request_id: request.request_id,
                questions: request.questions,
            }
        }
        "steerAccepted" => {
            let steer: AcceptedSteerWire = serde_json::from_value(payload)?;
            DriverEvent::SteerAccepted {
                message: steer.message,
            }
        }
        "steerRejected" => {
            let steer: RejectedSteerWire = serde_json::from_value(payload)?;
            DriverEvent::SteerRejected {
                message: steer.message,
                reason: steer.reason,
            }
        }
        "usageUpdated" => {
            let usage: UsageWire = serde_json::from_value(payload)?;
            DriverEvent::UsageUpdated {
                context_tokens: usage.context_tokens,
                context_window: usage.context_window,
                context_window_resolved: usage.context_window_resolved,
                session_total: usage.session_total,
                cache_read: usage.cache_read,
                prompt_tokens: usage.prompt_tokens,
                latest: usage.latest,
            }
        }
        "planUsageUpdated" => DriverEvent::PlanUsageUpdated(serde_json::from_value(payload)?),
        "turnStatsUpdated" => DriverEvent::TurnStatsUpdated(serde_json::from_value(payload)?),
        "compactionUpdated" => DriverEvent::CompactionUpdated(serde_json::from_value(payload)?),
        "providerBusy" => DriverEvent::ProviderBusy,
        "providerRetry" => {
            let retry: ProviderRetryWire = serde_json::from_value(payload)?;
            DriverEvent::ProviderRetry {
                attempt: retry.attempt,
                message: retry.message,
                action: retry.action,
                next_at_ms: retry.next_at_ms,
            }
        }
        "turnFinished" => {
            let finished: TurnFinishedWire = serde_json::from_value(payload)?;
            DriverEvent::TurnFinished {
                success: finished.success,
                summary: finished.summary,
            }
        }
        "error" => DriverEvent::Error(serde_json::from_value(payload)?),
        "processExited" => DriverEvent::ProcessExited,
        kind => bail!("daemon sent an unsupported driver event {kind:?}"),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityWire {
    id: Option<String>,
    kind: ActivityKind,
    title: String,
    detail: Option<String>,
    complete: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PermissionWire {
    request_id: String,
    title: String,
    detail: String,
    options: Vec<PermissionOption>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInputWire {
    request_id: String,
    questions: Vec<crate::model::UserInputQuestion>,
}

#[derive(Deserialize)]
struct AcceptedSteerWire {
    message: String,
}

#[derive(Deserialize)]
struct RejectedSteerWire {
    message: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageWire {
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    #[serde(default)]
    context_window_resolved: bool,
    #[serde(default)]
    session_total: Option<u64>,
    #[serde(default)]
    cache_read: Option<u64>,
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    latest: Option<fintwind_protocol::model::LatestCallUsage>,
}

#[derive(Deserialize)]
struct TurnFinishedWire {
    success: bool,
    summary: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderRetryWire {
    attempt: u32,
    message: String,
    action: Option<ProviderRetryAction>,
    next_at_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_runtime_projection_keeps_newer_transcript_cursor() {
        let runtime_id = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        let mut existing = AgentSession::new(Uuid::new_v4());
        existing.status = SessionStatus::Working;
        existing.runtime_event_cursor = Some(crate::model::RuntimeEventCursor {
            runtime_id,
            epoch,
            sequence: 10,
        });
        existing.push_message(crate::model::MessageRole::Assistant, "complete so far");

        let mut stale = existing.clone();
        stale.title = "Renamed elsewhere".into();
        stale.messages.clear();
        stale.runtime_event_cursor = Some(crate::model::RuntimeEventCursor {
            runtime_id,
            epoch,
            sequence: 7,
        });

        assert!(session_projection_precedes(
            &existing,
            &stale,
            Some(runtime_id)
        ));
        merge_stale_session_metadata(&mut existing, stale);
        assert_eq!(existing.title, "Renamed elsewhere");
        assert_eq!(existing.messages.len(), 1);
        assert_eq!(existing.runtime_event_cursor.unwrap().sequence, 10);
    }

    #[test]
    fn client_projection_cannot_replace_a_daemon_checkpoint() {
        let mut existing = AgentSession::new(Uuid::new_v4());
        existing.begin_turn("change it");
        existing.finish_active_turn(crate::model::TurnStatus::Completed);
        let checkpoint = Checkpoint {
            turn_count: 1,
            git_ref: "refs/fintwind/canonical".into(),
            status: CheckpointStatus::Ready,
            files: Vec::new(),
            additions: 0,
            deletions: 0,
            created_at: 1,
        };
        existing.turns[0].checkpoint = Some(checkpoint.clone());

        let mut incoming = existing.clone();
        incoming.turns[0].checkpoint = Some(Checkpoint {
            git_ref: "refs/fintwind/stale-client".into(),
            ..checkpoint.clone()
        });
        preserve_daemon_checkpoints(&existing, &mut incoming);

        assert_eq!(incoming.turns[0].checkpoint.as_ref(), Some(&checkpoint));
    }

    #[test]
    fn response_fork_titles_follow_one_numbered_sequence() {
        assert_eq!(
            next_response_fork_title("Fix the bug", ["Fix the bug"]),
            "Fix the bug (2)"
        );
        assert_eq!(
            next_response_fork_title(
                "Fix the bug (2)",
                ["Fix the bug", "Fix the bug (2)", "Fix the bug (4)"]
            ),
            "Fix the bug (5)"
        );
        assert_eq!(
            next_response_fork_title("Plan (2026)", ["Plan (2026)"]),
            "Plan (2026) (2)"
        );
    }

    #[test]
    fn message_rewind_requires_a_settled_user_turn_and_provider_cursor() {
        let mut session = AgentSession::new(Uuid::new_v4());
        session.begin_turn("change it");
        session.mark_active_turn_provider_started();
        session.provider_cursor = Some(ProviderResumeCursor::OpenCode {
            session_id: "session".into(),
        });
        session.finish_active_turn(crate::model::TurnStatus::Completed);

        assert!(validate_message_rewind(&session, 1).is_ok());

        let mut busy = session.clone();
        busy.status = SessionStatus::Working;
        assert!(validate_message_rewind(&busy, 1).is_err());

        let mut missing_cursor = session.clone();
        missing_cursor.provider_cursor = None;
        assert!(validate_message_rewind(&missing_cursor, 1).is_err());

        let mut missing_message = session;
        missing_message.messages.clear();
        assert!(validate_message_rewind(&missing_message, 1).is_err());
    }

    #[test]
    fn wire_event_round_trip_preserves_ordered_delta_payload() {
        let wire = event_to_wire(DriverEvent::TextDelta {
            part: "text:msg_1:0".into(),
            delta: "hello".into(),
        })
        .unwrap();
        assert_eq!(wire.kind, "textDelta");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::TextDelta { part, delta } if part == "text:msg_1:0" && delta == "hello"
        ));

        let legacy = WireDriverEvent::new("textDelta", Value::String("hello".into()));
        assert!(matches!(
            event_from_wire(legacy).unwrap(),
            DriverEvent::TextDelta { part, delta } if part.is_empty() && delta == "hello"
        ));
    }

    #[test]
    fn wire_event_round_trip_preserves_keyed_reasoning_fragments() {
        let wire = event_to_wire(DriverEvent::ReasoningStarted {
            part: "reasoning:msg_1:0".into(),
        })
        .unwrap();
        assert_eq!(wire.kind, "reasoningStarted");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ReasoningStarted { part } if part == "reasoning:msg_1:0"
        ));

        let wire = event_to_wire(DriverEvent::ReasoningDelta {
            part: "reasoning:msg_1:0".into(),
            delta: "thinking".into(),
        })
        .unwrap();
        assert_eq!(wire.kind, "reasoningDelta");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ReasoningDelta { part, delta }
                if part == "reasoning:msg_1:0" && delta == "thinking"
        ));

        let wire = event_to_wire(DriverEvent::ReasoningEnded {
            part: "reasoning:msg_1:0".into(),
            text: Some("thinking!".into()),
        })
        .unwrap();
        assert_eq!(wire.kind, "reasoningEnded");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ReasoningEnded { part, text }
                if part == "reasoning:msg_1:0" && text.as_deref() == Some("thinking!")
        ));
    }

    #[test]
    fn a_pre_keying_reasoning_delta_degrades_to_the_unkeyed_path() {
        // An older daemon serializes reasoning deltas as bare strings; the
        // decoded event carries an empty part key, which the app maps to its
        // phase-based fallback instead of failing the stream.
        let wire = WireDriverEvent::new(
            "reasoningDelta".to_owned(),
            serde_json::Value::String("thinking".to_owned()),
        );
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ReasoningDelta { part, delta }
                if part.is_empty() && delta == "thinking"
        ));
    }

    #[test]
    fn wire_event_round_trip_preserves_provider_status_signals() {
        let wire = event_to_wire(DriverEvent::ProviderBusy).unwrap();
        assert_eq!(wire.kind, "providerBusy");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ProviderBusy
        ));

        let action = ProviderRetryAction {
            reason: "free_tier_limit".into(),
            provider: "zen".into(),
            title: "Free limit reached".into(),
            message: "Subscribe to OpenCode Go".into(),
            label: "subscribe".into(),
            link: Some("https://opencode.ai/go".into()),
        };
        let retry = DriverEvent::ProviderRetry {
            attempt: 3,
            message: "429 Too Many Requests".into(),
            action: Some(action),
            next_at_ms: Some(1_700_000_008_000),
        };
        let wire = event_to_wire(retry).unwrap();
        assert_eq!(wire.kind, "providerRetry");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ProviderRetry {
                attempt: 3,
                message,
                action: Some(action),
                next_at_ms: Some(1_700_000_008_000),
            } if message == "429 Too Many Requests"
                && action.reason == "free_tier_limit"
                && action.link.as_deref() == Some("https://opencode.ai/go")
        ));
    }

    // The daemon keeps its own wire copy; this pins the compaction encoding
    // here so the duplicate cannot drift from the protocol crate's.
    #[test]
    fn wire_event_round_trip_preserves_compaction_snapshots() {
        let state = fintwind_protocol::model::CompactionState {
            status: fintwind_protocol::model::CompactionStatus::Failed,
            reason: Some("manual".into()),
            model: None,
            error: Some("provider rejected the summary".into()),
            summary: None,
        };
        let wire = event_to_wire(DriverEvent::CompactionUpdated(state.clone())).unwrap();
        assert_eq!(wire.kind, "compactionUpdated");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::CompactionUpdated(round) if round == state
        ));
    }

    fn catalog_backend() -> (FintwindBackend, PathBuf) {
        let root = std::env::temp_dir().join(format!("fintwind-remove-project-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let backend = FintwindBackend::new(
            crate::settings::DaemonSettingsStore::open(root.join("settings.json")).unwrap(),
            StateStore::daemon(root.join("app.db")),
        )
        .unwrap();
        (backend, root)
    }

    fn catalog_request(command: Command) -> Request {
        Request {
            request_id: Uuid::nil(),
            session_id: Uuid::nil(),
            runtime_id: Uuid::nil(),
            command,
        }
    }

    #[test]
    fn removing_a_project_hides_it_from_the_catalog_without_resurrecting_on_save() {
        let (backend, root) = catalog_backend();
        let project = Project::from_path(root.join("repo"));
        let mut session = AgentSession::new(project.id);
        session.begin_turn("keep the files");
        backend
            .handle(
                catalog_request(Command::SaveTaskState {
                    projects: vec![project.clone()],
                    live_session_ids: vec![session.id],
                    sessions: vec![session.clone()],
                }),
                EventSink::discarded(),
            )
            .unwrap();

        backend
            .handle(
                catalog_request(Command::RemoveProject {
                    project_id: project.id,
                }),
                EventSink::discarded(),
            )
            .unwrap();

        let ResponsePayload::TaskState {
            projects, sessions, ..
        } = backend
            .handle(
                catalog_request(Command::LoadTaskState),
                EventSink::discarded(),
            )
            .unwrap()
        else {
            panic!("expected task state");
        };
        assert!(projects.is_empty(), "the project leaves the app catalog");
        assert!(sessions.is_empty(), "its local session rows leave with it");

        backend
            .handle(
                catalog_request(Command::SaveTaskState {
                    projects: vec![project],
                    live_session_ids: vec![session.id],
                    sessions: vec![session],
                }),
                EventSink::discarded(),
            )
            .unwrap();
        let ResponsePayload::TaskState {
            projects, sessions, ..
        } = backend
            .handle(
                catalog_request(Command::LoadTaskState),
                EventSink::discarded(),
            )
            .unwrap()
        else {
            panic!("expected task state");
        };
        assert!(
            projects.is_empty(),
            "a stale save cannot restore the project"
        );
        assert!(
            sessions.is_empty(),
            "a stale save cannot restore its sessions"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

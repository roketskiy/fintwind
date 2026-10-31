use std::sync::atomic::Ordering;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context as _, bail};
use fintwind_protocol::scheduled_task::{DispatchOutcome, FIXED_TIME_GRACE_SECS, Recurrence};
use uuid::Uuid;

use crate::daemon::{FintwindBackend, ScheduledAdmission};
use crate::model::unix_time;
use crate::scheduled::{Occurrence, OccurrenceState, ScheduledStore};

const TICK_INTERVAL: Duration = Duration::from_secs(5);
const STOP_SLICE: Duration = Duration::from_millis(100);
const TICK_BATCH: usize = 4;
const TICK_SCAN: usize = 16;
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

pub(crate) fn begin(backend: &Arc<FintwindBackend>) -> anyhow::Result<()> {
    let mut slot = backend.scheduled_thread.lock();
    if slot.is_some() {
        bail!("the scheduled task runner is already active");
    }
    reconcile(backend, unix_time());
    backend.scheduled_stop.store(false, Ordering::SeqCst);
    backend.scheduled_cursor.store(0, Ordering::SeqCst);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    *backend.scheduled_done.lock() = Some(done_rx);
    let weak = Arc::downgrade(backend);
    let stop = backend.scheduled_stop.clone();
    let handle = std::thread::Builder::new()
        .name("fintwind-scheduled-tasks".into())
        .spawn(move || {
            let _done = DoneSignal(done_tx);
            run(weak, stop);
        })
        .context("could not start the scheduled task runner")?;
    *slot = Some(handle);
    Ok(())
}

struct DoneSignal(std::sync::mpsc::Sender<()>);

impl Drop for DoneSignal {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

pub(crate) fn stop(backend: &FintwindBackend) {
    backend.scheduled_stop.store(true, Ordering::SeqCst);
    let handle = backend.scheduled_thread.lock().take();
    let done = backend.scheduled_done.lock().take();
    let Some(handle) = handle else {
        return;
    };
    if handle.thread().id() == std::thread::current().id() {
        return;
    }
    let Some(done) = done else {
        return;
    };
    match done.recv_timeout(SHUTDOWN_WAIT) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
        }
        Err(RecvTimeoutError::Timeout) => eprintln!(
            "fintwind scheduled tasks: the ticker is still busy after {}s; detaching it",
            SHUTDOWN_WAIT.as_secs()
        ),
    }
}

fn run(weak: Weak<FintwindBackend>, stop: Arc<std::sync::atomic::AtomicBool>) {
    loop {
        let mut remaining = TICK_INTERVAL;
        while !remaining.is_zero() {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let slice = remaining.min(STOP_SLICE);
            std::thread::sleep(slice);
            remaining -= slice;
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Some(backend) = weak.upgrade() else {
            return;
        };
        let tick = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tick(&backend)));
        drop(backend);
        if tick.is_err() {
            eprintln!("fintwind scheduled tasks: a delivery attempt failed unexpectedly");
        }
    }
}

fn tick(backend: &Arc<FintwindBackend>) {
    let now = unix_time();
    if reconcile(backend, now) {
        backend.scheduled_notify_tasks();
    }
    if backend.scheduled_is_stopping() {
        return;
    }
    let store = backend.scheduled_store.clone();
    let owner = backend.scheduled_owner;
    if store.claim_due(now, owner, TICK_BATCH).is_err() {
        eprintln!("fintwind scheduled tasks: could not claim due occurrences");
        return;
    }
    let open = match store.open_occurrences() {
        Ok(open) => open,
        Err(_) => {
            eprintln!("fintwind scheduled tasks: could not read open occurrences");
            return;
        }
    };
    let queued: Vec<Occurrence> = open
        .into_iter()
        .filter(|occurrence| matches!(occurrence.state, OccurrenceState::Queued))
        .collect();
    let total = queued.len();
    if total == 0 {
        return;
    }
    let start = backend.scheduled_cursor.load(Ordering::SeqCst) % total;
    let window = total.min(TICK_SCAN);
    let mut changed = false;
    let mut attempted = 0usize;
    let mut working = 0usize;
    for offset in 0..window {
        if backend.scheduled_is_stopping() || working == TICK_BATCH {
            break;
        }
        let occurrence = queued[(start + offset) % total].clone();
        attempted += 1;
        match deliver(backend, occurrence) {
            Delivery::Settled => changed = true,
            Delivery::Attempted => {
                working += 1;
                changed = true;
            }
            Delivery::Deferred => {}
        }
    }
    if attempted > 0 {
        backend
            .scheduled_cursor
            .store((start + attempted) % total, Ordering::SeqCst);
    }
    if changed {
        backend.scheduled_notify_tasks();
    }
}

enum Delivery {
    Settled,
    Attempted,
    Deferred,
}

fn deliver(backend: &Arc<FintwindBackend>, occurrence: Occurrence) -> Delivery {
    let store = backend.scheduled_store.clone();
    let now = unix_time();
    if missed_window(&occurrence, now) {
        return settled(&store, occurrence.id, DispatchOutcome::Missed, now);
    }
    if backend.scheduled_is_stopping() {
        return Delivery::Deferred;
    }
    let Some(channel) = backend.scheduled_channel() else {
        return Delivery::Deferred;
    };
    let guard = Arc::new(());
    let owner = Arc::downgrade(&guard);
    let preparation = match backend.scheduled_admit(&channel, &occurrence, owner.clone()) {
        Ok(ScheduledAdmission::Admitted(preparation)) => preparation,
        Ok(ScheduledAdmission::Deferred) => return Delivery::Deferred,
        Ok(ScheduledAdmission::Rejected(reason)) => {
            return settled(
                &store,
                occurrence.id,
                DispatchOutcome::Rejected { reason },
                now,
            );
        }
        Ok(ScheduledAdmission::Interrupted {
            session_id,
            submission_id,
            reason,
        }) => {
            return settled(
                &store,
                occurrence.id,
                DispatchOutcome::NotSent {
                    session_id: Some(session_id),
                    submission_id: Some(submission_id),
                    reason,
                },
                now,
            );
        }
        Err(error) => {
            return settled(
                &store,
                occurrence.id,
                DispatchOutcome::NotSent {
                    session_id: None,
                    submission_id: None,
                    reason: format!("{error:#}"),
                },
                unix_time(),
            );
        }
    };
    if backend.scheduled_is_stopping() {
        backend.scheduled_abandon(&channel, &preparation, owner);
        return settled(
            &store,
            occurrence.id,
            DispatchOutcome::NotSent {
                session_id: Some(preparation.session_id),
                submission_id: Some(preparation.submission.receipt.id),
                reason: "The daemon shut down before dispatch".into(),
            },
            unix_time(),
        );
    }
    let outcome = match backend.scheduled_launch(&channel, &preparation, owner.clone()) {
        Ok(()) => {
            let outcome = backend.scheduled_dispatch(&channel, &preparation, owner);
            backend.scheduled_notify_catalog();
            outcome
        }
        Err(error) => {
            backend.scheduled_abandon(&channel, &preparation, owner);
            DispatchOutcome::NotSent {
                session_id: Some(preparation.session_id),
                submission_id: Some(preparation.submission.receipt.id),
                reason: format!("{error:#}"),
            }
        }
    };
    let settled = settled(&store, occurrence.id, outcome, unix_time());
    drop(guard);
    match settled {
        Delivery::Settled => Delivery::Attempted,
        other => other,
    }
}

fn settled(store: &ScheduledStore, id: Uuid, outcome: DispatchOutcome, now: u64) -> Delivery {
    match store.settle(id, outcome, now) {
        Ok(()) => Delivery::Settled,
        Err(error) => {
            eprintln!("fintwind scheduled tasks: could not settle an occurrence: {error:#}");
            Delivery::Deferred
        }
    }
}

fn reconcile(backend: &Arc<FintwindBackend>, now: u64) -> bool {
    let store = backend.scheduled_store.clone();
    let open = match store.open_occurrences() {
        Ok(open) => open,
        Err(_) => {
            eprintln!("fintwind scheduled tasks: could not read occurrences to reconcile");
            return false;
        }
    };
    let mut changed = false;
    for occurrence in open {
        let settled = match occurrence.state {
            OccurrenceState::Settled => false,
            OccurrenceState::Queued => reconcile_queued(&store, &occurrence, now),
            OccurrenceState::Preparing => reconcile_preparing(backend, &store, &occurrence, now),
        };
        changed |= settled;
    }
    changed
}

fn reconcile_queued(store: &ScheduledStore, occurrence: &Occurrence, now: u64) -> bool {
    let task = store.get(occurrence.task_id).ok().flatten();
    let cancelled = match task {
        None => Some("The scheduled task was deleted".to_owned()),
        Some(task) if !task.enabled => Some("The scheduled task is paused".to_owned()),
        Some(_) => None,
    };
    if let Some(reason) = cancelled {
        return store
            .settle(
                occurrence.id,
                DispatchOutcome::NotSent {
                    session_id: None,
                    submission_id: None,
                    reason,
                },
                now,
            )
            .is_ok();
    }
    if missed_window(occurrence, now) {
        return store
            .settle(occurrence.id, DispatchOutcome::Missed, now)
            .is_ok();
    }
    false
}

fn reconcile_preparing(
    backend: &Arc<FintwindBackend>,
    store: &ScheduledStore,
    occurrence: &Occurrence,
    now: u64,
) -> bool {
    let Some(submission) = occurrence.submission.as_ref() else {
        return store
            .settle(
                occurrence.id,
                DispatchOutcome::NotSent {
                    session_id: None,
                    submission_id: None,
                    reason: "Preparation stopped before any input was saved".into(),
                },
                now,
            )
            .is_ok();
    };
    if backend.scheduled_preparation_is_live(submission.receipt.id) {
        return false;
    }
    let outcome = backend.scheduled_resolve_interrupted(submission);
    store.settle(occurrence.id, outcome, now).is_ok()
}

fn missed_window(occurrence: &Occurrence, now: u64) -> bool {
    !occurrence.manual
        && matches!(
            occurrence.draft.schedule,
            Recurrence::AtTime { .. } | Recurrence::Once { .. }
        )
        && now.saturating_sub(occurrence.due_at) > FIXED_TIME_GRACE_SECS
}

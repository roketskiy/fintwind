//! The daemon's global Git process budget.
//!
//! A large workspace can ask for status, directory pages, single-file diffs,
//! branch metadata, and history at the same time, and each request runs on its
//! own mailbox thread. Starting one Git child per request lets a handful of
//! concurrent views saturate the machine. Every Git subprocess therefore takes
//! a permit here first, so at most [`MAX_CONCURRENT`] run at once.
//!
//! User-facing queries are [`Priority::Interactive`]: they take a slot ahead of
//! waiting background work, so selecting a file never queues behind an
//! automatic history scan. A permit is released when the guard drops, which
//! happens after the child this module started has been waited on.

use std::process::{Child, Command, Output};
use std::sync::{
    Condvar, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};

/// Git children allowed to run at once across the whole daemon.
const MAX_CONCURRENT: usize = 4;
/// How long a request waits for a slot before giving up. A slot is held only
/// for one command, so this covers a busy daemon, not a hung repository.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
/// An execution ceiling for the commands this module collects output from.
/// Fetch and pull on a failing network would otherwise hold a shared slot
/// forever and stall every other Git query behind it.
const EXECUTION_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Priority {
    /// The user is waiting on this result, directly or through a selected row.
    Interactive,
    /// History, discovery, and other work that can yield to interactivity.
    Background,
}

#[derive(Default)]
struct Gate {
    active: usize,
    interactive_waiters: usize,
}

static GATE: OnceLock<(Mutex<Gate>, Condvar)> = OnceLock::new();

/// One occupied slot. Dropping it hands the slot to a waiter.
pub(crate) struct Permit;

impl Permit {
    pub(crate) fn acquire(
        priority: Priority,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<Self> {
        let (lock, changed) = GATE.get_or_init(Default::default);
        let mut gate = lock
            .lock()
            .map_err(|_| anyhow!("Git process gate unavailable"))?;
        let interactive = priority == Priority::Interactive;
        if interactive {
            gate.interactive_waiters += 1;
        }
        let started = Instant::now();
        loop {
            let was_cancelled = cancelled.is_some_and(|flag| flag.load(Ordering::Acquire));
            if was_cancelled || started.elapsed() >= ACQUIRE_TIMEOUT {
                if interactive {
                    gate.interactive_waiters -= 1;
                }
                changed.notify_all();
                if was_cancelled {
                    anyhow::bail!("Git query cancelled");
                }
                anyhow::bail!("waiting for a Git process timed out");
            }
            // Background work stands aside as soon as an interactive request
            // is waiting, so the slot it would take goes to the user's query.
            if gate.active < MAX_CONCURRENT && (interactive || gate.interactive_waiters == 0) {
                gate.active += 1;
                if interactive {
                    gate.interactive_waiters -= 1;
                }
                return Ok(Self);
            }
            gate = changed
                .wait_timeout(gate, Duration::from_millis(25))
                .map_err(|_| anyhow!("Git process gate unavailable"))?
                .0;
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some((lock, changed)) = GATE.get()
            && let Ok(mut gate) = lock.lock()
        {
            gate.active -= 1;
            changed.notify_all();
        }
    }
}

/// Runs one Git command inside the global budget and collects its output.
pub(crate) fn output(command: &mut Command, priority: Priority) -> anyhow::Result<Output> {
    let _permit = Permit::acquire(priority, None)?;
    match crate::command_env::output_with_timeout(command, EXECUTION_TIMEOUT) {
        Ok(Some(output)) => Ok(output),
        Ok(None) => anyhow::bail!("git command timed out"),
        Err(error) => Err(error).context("failed to execute git"),
    }
}

/// Starts one Git command inside the global budget. The permit is held for as
/// long as the returned child is alive, so callers must wait on it.
pub(crate) fn spawn(command: &mut Command, priority: Priority) -> anyhow::Result<GitChild> {
    let permit = Permit::acquire(priority, None)?;
    let child = crate::command_env::spawn(command).context("failed to execute git")?;
    Ok(GitChild {
        child,
        _permit: permit,
    })
}

pub(crate) struct GitChild {
    child: Child,
    _permit: Permit,
}

impl GitChild {
    /// Waits for the child and collects its output, holding the slot until the
    /// streams close.
    pub(crate) fn wait_with_output(self) -> std::io::Result<Output> {
        self.child.wait_with_output()
    }
}

impl std::ops::Deref for GitChild {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.child
    }
}

impl std::ops::DerefMut for GitChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

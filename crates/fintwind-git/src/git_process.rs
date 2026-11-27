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
pub enum Priority {
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
pub struct Permit;

impl Permit {
    pub fn acquire(
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
pub fn output(command: &mut Command, priority: Priority) -> anyhow::Result<Output> {
    let _permit = Permit::acquire(priority, None)?;
    match fintwind_env::output_with_timeout(command, EXECUTION_TIMEOUT) {
        Ok(Some(output)) => Ok(output),
        Ok(None) => anyhow::bail!("git command timed out"),
        Err(error) => Err(error).context("failed to execute git"),
    }
}

/// Starts one Git command inside the global budget. The permit is held for as
/// long as the returned child is alive, so callers must wait on it.
pub fn spawn(command: &mut Command, priority: Priority) -> anyhow::Result<GitChild> {
    let permit = Permit::acquire(priority, None)?;
    let child = fintwind_env::spawn(command).context("failed to execute git")?;
    Ok(GitChild {
        child,
        _permit: permit,
    })
}

pub struct GitChild {
    child: Child,
    _permit: Permit,
}

impl GitChild {
    /// Waits for the child and collects its output, holding the slot until the
    /// streams close.
    pub fn wait_with_output(self) -> std::io::Result<Output> {
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

pub fn check_git_cancelled(cancelled: Option<&AtomicBool>) -> anyhow::Result<()> {
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
        anyhow::bail!("Git query cancelled");
    }
    Ok(())
}

/// Bound the output while Git is running. Only the child started here is stopped.
pub fn bounded_git_output_cancellable(
    command: &mut std::process::Command,
    no_index: bool,
    cancelled: Option<&AtomicBool>,
    interactive: bool,
) -> anyhow::Result<Vec<u8>> {
    let exit_mode = if no_index {
        GitExitMode::Diff
    } else {
        GitExitMode::Success
    };
    let (output, complete) =
        bounded_git_output_with_mode(command, exit_mode, cancelled, interactive)?;
    if !complete {
        anyhow::bail!("the Git output exceeds the size limit");
    }
    Ok(output)
}

/// A listing may use whatever the output budget allowed and report itself
/// incomplete, instead of failing the whole request. Listings are user-facing,
/// so they take the interactive slot priority.
pub fn bounded_git_output_partial(
    command: &mut std::process::Command,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<(Vec<u8>, bool)> {
    bounded_git_output_with_mode(command, GitExitMode::Success, cancelled, true)
}

pub fn bounded_git_probe(
    command: &mut std::process::Command,
    cancelled: Option<&AtomicBool>,
    interactive: bool,
) -> anyhow::Result<bool> {
    let (output, _) =
        bounded_git_output_with_mode(command, GitExitMode::Probe, cancelled, interactive)?;
    Ok(!output.is_empty())
}

#[derive(Clone, Copy)]
enum GitExitMode {
    Success,
    Diff,
    Probe,
}

fn bounded_git_output_with_mode(
    command: &mut std::process::Command,
    exit_mode: GitExitMode,
    cancelled: Option<&AtomicBool>,
    interactive: bool,
) -> anyhow::Result<(Vec<u8>, bool)> {
    bounded_git_output_full(command, exit_mode, cancelled, interactive, None)
}

/// A command fed through stdin, such as `git check-ignore --stdin`. The writer
/// runs on its own thread so a large path list cannot deadlock against the
/// output pipes, and the same cancel, timeout, and size budgets apply.
pub fn bounded_git_output_with_stdin(
    command: &mut std::process::Command,
    input: &[u8],
    cancelled: Option<&AtomicBool>,
    interactive: bool,
) -> anyhow::Result<Vec<u8>> {
    let (output, complete) = bounded_git_output_full(
        command,
        GitExitMode::Diff,
        cancelled,
        interactive,
        Some(input.to_vec()),
    )?;
    if !complete {
        anyhow::bail!("the Git output exceeds the size limit");
    }
    Ok(output)
}

fn bounded_git_output_full(
    command: &mut std::process::Command,
    exit_mode: GitExitMode,
    cancelled: Option<&AtomicBool>,
    interactive: bool,
    stdin: Option<Vec<u8>>,
) -> anyhow::Result<(Vec<u8>, bool)> {
    use std::io::Read as _;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const MAX_OUTPUT: usize = 8 * 1024 * 1024;
    const MAX_ERROR: usize = 64 * 1024;
    const TIMEOUT: Duration = Duration::from_secs(30);
    enum Message {
        Data(bool, Vec<u8>),
        End(std::io::Result<()>),
    }
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
        anyhow::bail!("Git query cancelled");
    }
    let _permit = crate::git_process::Permit::acquire(
        if interactive {
            crate::git_process::Priority::Interactive
        } else {
            crate::git_process::Priority::Background
        },
        cancelled,
    )?;
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = fintwind_env::spawn(command).context("failed to start Git command")?;
    if let Some(input) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        std::thread::spawn(move || {
            use std::io::Write as _;
            let _ = pipe.write_all(&input);
        });
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (sender, receiver) = mpsc::sync_channel(16);
    for (mut stream, is_stdout) in [
        (Box::new(stdout) as Box<dyn std::io::Read + Send>, true),
        (Box::new(stderr) as Box<dyn std::io::Read + Send>, false),
    ] {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut buffer = [0; 8192];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => {
                        let _ = sender.send(Message::End(Ok(())));
                        break;
                    }
                    Ok(length) => {
                        if sender
                            .send(Message::Data(is_stdout, buffer[..length].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Message::End(Err(error)));
                        break;
                    }
                }
            }
        });
    }
    drop(sender);
    let started = Instant::now();
    let mut output = Vec::new();
    let mut error_output = Vec::new();
    let mut closed = 0;
    let mut status = None;
    loop {
        if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Git query cancelled");
        }
        if started.elapsed() >= TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("git command timed out");
        }
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(Message::Data(true, chunk)) => {
                if output.len().saturating_add(chunk.len()) > MAX_OUTPUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Return what the budget allowed; the caller decides
                    // whether an incomplete result is usable.
                    return Ok((output, false));
                }
                output.extend_from_slice(&chunk);
            }
            Ok(Message::Data(false, chunk)) => {
                let remaining = MAX_ERROR.saturating_sub(error_output.len());
                error_output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            Ok(Message::End(result)) => {
                if let Err(error) = result {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error).context("failed to read Git output");
                }
                closed += 1;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                closed = 2;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if status.is_none() {
            status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error).context("failed to wait for Git command");
                }
            };
        }
        if let Some(status) = status
            && closed == 2
        {
            if status.success()
                || (matches!(exit_mode, GitExitMode::Diff) && status.code() == Some(1))
            {
                return Ok((output, true));
            }
            if matches!(exit_mode, GitExitMode::Probe) {
                return Ok((Vec::new(), true));
            }
            anyhow::bail!(
                "Git command failed: {}",
                String::from_utf8_lossy(&error_output).trim()
            );
        }
    }
}


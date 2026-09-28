//! Daemon-owned Git commit history reads.
//!
//! Every function in this module performs process I/O. Callers must run them
//! from the background executor; render paths consume only the cached
//! [`CommitEntry`] values they return.

use std::path::Path;
use std::process::Output;

use anyhow::{Context as _, bail};

pub use fintwind_protocol::git::CommitEntry;

/// The commit history of a workspace, newest first. `branch` `None` reads
/// `HEAD`; `Some` names a branch or any revision Git accepts. `Ok(None)`
/// means `cwd` is not inside a Git repository.
pub fn list(
    cwd: &Path,
    limit: usize,
    branch: Option<&str>,
) -> anyhow::Result<Option<Vec<CommitEntry>>> {
    let repository_output = crate::command_env::plain_command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .context("failed to execute git")?;
    if !repository_output.status.success() {
        return Ok(None);
    }

    // `%x1f` is the unit separator between fields and `%x1e` the record
    // separator between commits, so subjects containing newlines, spaces, or
    // commas cannot corrupt the parsing below.
    let mut command = crate::command_env::plain_command("git");
    command.args(["log", "-n", &limit.to_string()]);
    if let Some(branch) = branch {
        command.arg(branch);
    }
    let output = command
        .arg("--pretty=format:%H%x1f%h%x1f%s%x1f%an%x1f%at%x1f%D%x1e")
        .current_dir(cwd)
        .output()
        .context("failed to execute git log")?;
    if !output.status.success() {
        // A repository whose HEAD branch has no commits yet is a valid empty
        // history, not an error; Git exits 128 with this fatal message.
        if String::from_utf8_lossy(&output.stderr).contains("does not have any commits yet") {
            return Ok(Some(Vec::new()));
        }
        bail!("{}", command_error(&output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();
    for record in stdout.split('\u{1e}') {
        // Git emits a newline between `format` records; trim it so the
        // trailing empty record and inter-record gaps can simply be skipped.
        let record = record.trim_matches('\n');
        if record.is_empty() {
            continue;
        }
        let fields = record.split('\u{1f}').collect::<Vec<_>>();
        let [hash, short_hash, subject, author, timestamp, refs] = fields[..] else {
            continue;
        };
        let Ok(timestamp) = timestamp.parse::<u64>() else {
            continue;
        };
        // `%D` decorates with a comma and a space, e.g. `HEAD -> main,
        // origin/main, tag: v1.2.0`.
        let refs = refs
            .split(", ")
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let is_head = refs
            .iter()
            .any(|label| label == "HEAD" || label.starts_with("HEAD ->"));
        commits.push(CommitEntry {
            hash: hash.to_owned(),
            short_hash: short_hash.to_owned(),
            subject: subject.to_owned(),
            author: author.to_owned(),
            timestamp,
            refs,
            is_head,
        });
    }
    Ok(Some(commits))
}

fn command_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("git exited with {}", output.status)
    } else {
        stderr
    }
}

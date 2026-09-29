//! Daemon-owned Git commit history reads.
//!
//! Every function in this module performs process I/O. Callers must run them
//! from the background executor; render paths consume only the cached
//! [`CommitEntry`] values they return.

use std::collections::HashSet;
use std::path::Path;
use std::process::Output;

use anyhow::{Context as _, bail};

pub use fintwind_protocol::git::{CommitEntry, CommitRef};

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

    // Remote names decide which `%D` decorations point at remote-tracking
    // refs. A repository without remotes is common, so a failing `git
    // remote` simply yields an empty list rather than an error.
    let remotes_output = crate::command_env::plain_command("git")
        .arg("remote")
        .current_dir(cwd)
        .output()
        .context("failed to execute git remote")?;
    let remotes = if remotes_output.status.success() {
        String::from_utf8_lossy(&remotes_output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

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

    // Commits reachable from the revision but from no remote ref are
    // unpushed. `-n limit` matches the `git log` limit: `git log` is a
    // porcelain front for the same newest-first walk `git rev-list`
    // performs, so the unpushed hashes inside the returned window are
    // always among the first `limit` of the full unpushed list, and the
    // truncation cannot miss one. (Unpushed and pushed commits may
    // interleave in merge histories; the guarantee is the shared traversal
    // order, not their separation.) Without remotes, `--remotes` expands
    // to nothing and every commit counts as unpushed, which matches the
    // semantics.
    let rev = branch.unwrap_or("HEAD");
    let rev_list_output = crate::command_env::plain_command("git")
        .args([
            "rev-list",
            "-n",
            &limit.to_string(),
            rev,
            "--not",
            "--remotes",
        ])
        .current_dir(cwd)
        .output()
        .context("failed to execute git rev-list")?;
    if !rev_list_output.status.success() {
        bail!("{}", command_error(&rev_list_output));
    }
    let unpushed = String::from_utf8_lossy(&rev_list_output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<HashSet<_>>();

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
            .map(|label| CommitRef {
                head: label == "HEAD" || label.starts_with("HEAD ->"),
                remote: remotes
                    .iter()
                    .any(|remote| label.starts_with(&format!("{remote}/"))),
                label: label.to_owned(),
            })
            .collect::<Vec<_>>();
        let is_head = refs.iter().any(|commit_ref| commit_ref.head);
        commits.push(CommitEntry {
            hash: hash.to_owned(),
            short_hash: short_hash.to_owned(),
            subject: subject.to_owned(),
            author: author.to_owned(),
            timestamp,
            refs,
            is_head,
            pushed: !unpushed.contains(hash),
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

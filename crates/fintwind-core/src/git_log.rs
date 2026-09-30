//! Daemon-owned Git commit history reads.
//!
//! Every function in this module performs process I/O. Callers must run them
//! from the background executor; render paths consume only the cached
//! [`CommitEntry`] values they return.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;
use std::process::{Output, Stdio};

use anyhow::{Context as _, bail};

pub use fintwind_protocol::git::{CommitEntry, CommitRef};

/// The commit history of a workspace, children before parents. `branch` `None` reads
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
    command.args(["log", "--topo-order", "-n", &limit.to_string()]);
    if let Some(branch) = branch {
        command.arg(branch);
    }
    let output = command
        .arg("--pretty=format:%H%x1f%h%x1f%s%x1f%an%x1f%at%x1f%D%x1f%P%x1e")
        .arg("--")
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
        let [hash, short_hash, subject, author, timestamp, refs, parents] = fields[..] else {
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
            parents: parents.split_whitespace().map(str::to_owned).collect(),
            short_hash: short_hash.to_owned(),
            subject: subject.to_owned(),
            author: author.to_owned(),
            timestamp,
            refs,
            is_head,
            pushed: false,
        });
    }
    if commits.is_empty() {
        return Ok(Some(commits));
    }

    // Classify exactly the displayed window, not a separately truncated walk
    // whose order can change when remote ancestors are pruned. Excluding all
    // parents outside this topological prefix bounds the walk to this window,
    // even in a large repository with no remotes. stdin avoids Windows' command
    // line length limit. Command-line --not applies to --remotes, while stdin's
    // explicit ^ prefixes mark only the boundary parents as excluded.
    let visible: HashSet<_> = commits.iter().map(|commit| commit.hash.as_str()).collect();
    let boundary: HashSet<_> = commits
        .iter()
        .flat_map(|commit| &commit.parents)
        .map(String::as_str)
        .filter(|parent| !visible.contains(parent))
        .collect();
    let mut input = String::new();
    for commit in &commits {
        input.push_str(&commit.hash);
        input.push('\n');
    }
    for parent in boundary {
        input.push('^');
        input.push_str(parent);
        input.push('\n');
    }
    let mut child = crate::command_env::plain_command("git")
        .args(["rev-list", "--stdin", "--not", "--remotes"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to execute git rev-list")?;
    let write = child
        .stdin
        .take()
        .context("git rev-list stdin unavailable")?
        .write_all(input.as_bytes());
    let output = child
        .wait_with_output()
        .context("failed to wait for git rev-list")?;
    write.context("failed to write git rev-list revisions")?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    let unpushed: HashSet<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    for commit in &mut commits {
        commit.pushed = !unpushed.contains(&commit.hash);
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

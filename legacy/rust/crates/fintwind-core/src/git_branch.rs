//! Daemon-owned Git branch discovery and checkout operations.
//!
//! Every function in this module performs process I/O. Callers must run them
//! from the background executor; render paths consume only the cached
//! [`BranchSnapshot`] values they return.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Output;

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;

use anyhow::{anyhow, bail};
const MAX_UNTRACKED_FILES: usize = 2_048;
const MAX_UNTRACKED_FILE_BYTES: u64 = 8 * 1_024 * 1_024;
const MAX_UNTRACKED_TOTAL_BYTES: u64 = 32 * 1_024 * 1_024;
const BINARY_PROBE_BYTES: usize = 8_000;

pub use fintwind_protocol::git::{BranchEntry, BranchSnapshot};

/// Inspect local branches and which worktree, if any, currently owns each.
/// `Ok(None)` means `cwd` is not inside a Git repository.
pub fn inspect(cwd: &Path) -> anyhow::Result<Option<BranchSnapshot>> {
    inspect_inner(cwd, true)
}

/// Branch metadata for menus that do not display worktree line counts.
pub fn inspect_refs(cwd: &Path) -> anyhow::Result<Option<BranchSnapshot>> {
    inspect_inner(cwd, false)
}

fn inspect_inner(cwd: &Path, include_line_counts: bool) -> anyhow::Result<Option<BranchSnapshot>> {
    let mut command = crate::command_env::plain_command("git");
    command
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd);
    let repository_output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
    if !repository_output.status.success() {
        return Ok(None);
    }
    let repository = PathBuf::from(
        String::from_utf8_lossy(&repository_output.stdout)
            .trim()
            .to_owned(),
    );
    let repository = fs::canonicalize(&repository).unwrap_or(repository);

    let current =
        optional_stdout(cwd, &["branch", "--show-current"])?.filter(|branch| !branch.is_empty());
    let detached_head = if current.is_none() {
        Some(git_stdout(cwd, &["rev-parse", "--short", "HEAD"])?).filter(|head| !head.is_empty())
    } else {
        None
    };

    // `%(worktreepath)` is empty for an available branch and points at the
    // owning checkout otherwise. A NUL separates it from the branch name so
    // paths containing spaces need no shell-style decoding.
    let refs = git_stdout(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(worktreepath)",
            "refs/heads",
        ],
    )?;
    let mut branches = refs
        .lines()
        .filter_map(|line| {
            let (name, worktree_path) = line.split_once('\0')?;
            if name.is_empty() {
                return None;
            }
            let checked_out_elsewhere = if worktree_path.is_empty() {
                false
            } else {
                let worktree_path = PathBuf::from(worktree_path);
                let worktree_path = fs::canonicalize(&worktree_path).unwrap_or(worktree_path);
                worktree_path != repository
            };
            Some(BranchEntry {
                name: name.to_owned(),
                checked_out_elsewhere,
            })
        })
        .collect::<Vec<_>>();

    branches.sort_by(|left, right| {
        let left_current = current.as_deref() == Some(left.name.as_str());
        let right_current = current.as_deref() == Some(right.name.as_str());
        right_current
            .cmp(&left_current)
            .then_with(|| left.name.cmp(&right.name))
    });

    let remote_default = optional_stdout(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )?;
    let default_branch = remote_default
        .as_deref()
        .and_then(|branch| branch.strip_prefix("origin/"))
        .filter(|branch| branches.iter().any(|entry| entry.name == *branch))
        .map(str::to_owned)
        .or_else(|| current.clone());
    let (additions, deletions) = if include_line_counts {
        worktree_line_counts(&repository)
    } else {
        (0, 0)
    };

    Ok(Some(BranchSnapshot {
        repository,
        current,
        detached_head,
        default_branch,
        branches,
        additions,
        deletions,
    }))
}

/// The workspace's checked-out branch, or `None` when `cwd` is not inside a
/// Git repository. A detached HEAD reports its short commit instead.
///
/// This is deliberately much cheaper than [`inspect`]: it never enumerates
/// refs or measures pending changes, so list surfaces can afford to ask once
/// per project rather than only for the selected workspace.
pub fn current(cwd: &Path) -> anyhow::Result<Option<String>> {
    let mut command = crate::command_env::plain_command("git");
    command
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd);
    let repository =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
    if !repository.status.success() {
        return Ok(None);
    }
    if let Some(branch) =
        optional_stdout(cwd, &["branch", "--show-current"])?.filter(|branch| !branch.is_empty())
    {
        return Ok(Some(branch));
    }
    Ok(optional_stdout(cwd, &["rev-parse", "--short", "HEAD"])?.filter(|head| !head.is_empty()))
}

fn worktree_line_counts(cwd: &Path) -> (u64, u64) {
    let mut command = crate::command_env::plain_command("git");
    command
        .args(["diff", "--numstat", "HEAD", "--"])
        .current_dir(cwd);
    let tracked =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)
            .ok()
            .filter(|output| output.status.success())
            .map_or((0, 0), |output| numstat_line_counts(&output.stdout));
    (
        tracked
            .0
            .saturating_add(untracked_line_stats(cwd).additions),
        tracked.1,
    )
}

pub(crate) fn numstat_line_counts(output: &[u8]) -> (u64, u64) {
    numstat_totals(output).0
}

/// Added lines, removed lines, and file count of one `--numstat` stream.
/// Binary files are reported as `-\t-`; they count as a file but have no line
/// counts.
pub(crate) fn numstat_totals(output: &[u8]) -> ((u64, u64), u64) {
    String::from_utf8_lossy(output).lines().fold(
        ((0, 0), 0),
        |((additions, deletions), files), line| {
            if line.is_empty() {
                return ((additions, deletions), files);
            }
            let mut fields = line.splitn(3, '\t');
            let added = fields.next().and_then(|value| value.parse::<u64>().ok());
            let deleted = fields.next().and_then(|value| value.parse::<u64>().ok());
            match (added, deleted) {
                (Some(added), Some(deleted)) => (
                    (
                        additions.saturating_add(added),
                        deletions.saturating_add(deleted),
                    ),
                    files + 1,
                ),
                // Binary files are reported as `-\t-` and have no line count.
                _ => ((additions, deletions), files + 1),
            }
        },
    )
}

/// `git diff --numstat` deliberately omits untracked files. Count their text
/// lines as additions so the compact status agrees with the Uncommitted review
/// source that opens when it is clicked. Bounds keep generated trees from
/// turning a background metadata refresh into unbounded work; the flag says
/// whether a bound stopped the scan, so a caller that reports the number can
/// say it under-counts.
pub(crate) fn untracked_line_stats(repository: &Path) -> UntrackedStats {
    let mut command = crate::command_env::plain_command("git");
    command
        .args([
            "ls-files",
            "--others",
            "--exclude-standard",
            "--full-name",
            "-z",
            "--",
        ])
        .current_dir(repository);
    let Ok(output) =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)
    else {
        return UntrackedStats::unavailable();
    };
    if !output.status.success() {
        return UntrackedStats::unavailable();
    }

    let mut remaining_bytes = MAX_UNTRACKED_TOTAL_BYTES;
    let mut stats = UntrackedStats::default();
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        if remaining_bytes == 0 || stats.files >= MAX_UNTRACKED_FILES as u64 {
            stats.complete = false;
            break;
        }
        stats.files += 1;
        let path = repository.join(path_from_git_bytes(path));
        let (lines, bytes_read, complete) =
            untracked_text_line_count(&path, remaining_bytes.min(MAX_UNTRACKED_FILE_BYTES));
        stats.additions = stats.additions.saturating_add(lines);
        stats.complete &= complete;
        remaining_bytes = remaining_bytes.saturating_sub(bytes_read);
    }
    stats
}

/// Untracked files' contribution to a line total.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UntrackedStats {
    pub additions: u64,
    pub files: u64,
    /// False when a file, byte, or count bound stopped the scan, so the counts
    /// are a lower bound rather than the whole answer.
    pub complete: bool,
}

impl Default for UntrackedStats {
    fn default() -> Self {
        Self {
            additions: 0,
            files: 0,
            complete: true,
        }
    }
}

impl UntrackedStats {
    fn unavailable() -> Self {
        Self {
            complete: false,
            ..Self::default()
        }
    }
}

/// Text lines in one untracked file, the bytes it read, and whether the answer
/// covers the whole file. A file too large for the budget counts as unknown so
/// the caller can say its total is partial.
fn untracked_text_line_count(path: &Path, maximum_bytes: u64) -> (u64, u64, bool) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return (0, 0, false);
    };
    if metadata.file_type().is_symlink() {
        return (1, 0, true);
    }
    if !metadata.is_file() {
        return (0, 0, true);
    }
    if metadata.len() > maximum_bytes {
        return (0, 0, false);
    }

    let Ok(file) = fs::File::open(path) else {
        return (0, 0, false);
    };
    let mut content = Vec::with_capacity(metadata.len() as usize);
    if file
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut content)
        .is_err()
    {
        return (0, 0, false);
    }
    let bytes_read = content.len() as u64;
    if bytes_read > maximum_bytes {
        return (0, bytes_read, false);
    }
    if content[..content.len().min(BINARY_PROBE_BYTES)].contains(&0) {
        // Binary files have no line count at all, so nothing was skipped.
        return (0, bytes_read, true);
    }
    if content.is_empty() {
        return (0, 0, true);
    }
    let newline_count = content.iter().filter(|byte| **byte == b'\n').count() as u64;
    let trailing_line = u64::from(content.last() != Some(&b'\n'));
    (
        newline_count.saturating_add(trailing_line),
        bytes_read,
        true,
    )
}

#[cfg(unix)]
fn path_from_git_bytes(path: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(path.to_vec()))
}

#[cfg(not(unix))]
fn path_from_git_bytes(path: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(path).into_owned())
}

pub fn checkout(cwd: &Path, branch: &str) -> anyhow::Result<BranchSnapshot> {
    let mut command = crate::command_env::plain_command("git");
    command.args(["switch", "--"]).arg(branch).current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    inspect(cwd)?.ok_or_else(|| anyhow!("the workspace is no longer a Git repository"))
}

pub fn create_and_checkout(cwd: &Path, branch: &str) -> anyhow::Result<BranchSnapshot> {
    let branch = branch.trim();
    if branch.is_empty() {
        bail!("enter a branch name");
    }
    let mut command = crate::command_env::plain_command("git");
    command
        .args(["check-ref-format", "--branch"])
        .arg(branch)
        .current_dir(cwd);
    let validation =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
    if !validation.status.success() {
        bail!("{}", command_error(&validation));
    }
    let mut command = crate::command_env::plain_command("git");
    command.args(["switch", "-c"]).arg(branch).current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    inspect(cwd)?.ok_or_else(|| anyhow!("the workspace is no longer a Git repository"))
}

fn git_stdout(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let mut command = crate::command_env::plain_command("git");
    command.args(args).current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn optional_stdout(cwd: &Path, args: &[&str]) -> anyhow::Result<Option<String>> {
    let mut command = crate::command_env::plain_command("git");
    command.args(args).current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    bail!("{}", command_error(&output))
}

fn command_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("git exited with {}", output.status)
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = crate::command_env::plain_command("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", command_error(&output));
    }

    fn repository() -> PathBuf {
        let root = std::env::temp_dir().join(format!("fintwind-branch-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-b", "main"]);
        fs::write(root.join("README.md"), "main\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=Fintwind Tests",
                "-c",
                "user.email=fintwind@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );
        run_git(&root, &["branch", "feature"]);
        root
    }

    #[test]
    fn discovers_current_and_worktree_owned_branches() {
        let repository = repository();
        let other = repository.with_extension("other-worktree");
        run_git(
            &repository,
            &["worktree", "add", "-b", "occupied", other.to_str().unwrap()],
        );

        let snapshot = inspect(&repository).unwrap().unwrap();
        assert_eq!(snapshot.current.as_deref(), Some("main"));
        assert_eq!(snapshot.branches[0].name, "main");
        assert!(
            snapshot
                .branches
                .iter()
                .find(|branch| branch.name == "occupied")
                .unwrap()
                .checked_out_elsewhere
        );
        assert!(
            !snapshot
                .branches
                .iter()
                .find(|branch| branch.name == "feature")
                .unwrap()
                .checked_out_elsewhere
        );
    }

    #[test]
    fn reports_the_current_branch_without_inspecting_the_tree() {
        let repository = repository();
        assert_eq!(current(&repository).unwrap().as_deref(), Some("main"));

        let outside = repository.with_extension("not-a-repository");
        fs::create_dir_all(&outside).unwrap();
        assert_eq!(current(&outside).unwrap(), None);
    }

    #[test]
    fn switches_and_creates_branches() {
        let repository = repository();
        let switched = checkout(&repository, "feature").unwrap();
        assert_eq!(switched.current.as_deref(), Some("feature"));

        let created = create_and_checkout(&repository, "topic/new-picker").unwrap();
        assert_eq!(created.current.as_deref(), Some("topic/new-picker"));
        assert!(
            created
                .branches
                .iter()
                .any(|branch| branch.name == "topic/new-picker")
        );
    }

    #[test]
    fn counts_tracked_worktree_changes() {
        let repository = repository();
        fs::write(repository.join("README.md"), "main\nsecond\n").unwrap();

        let snapshot = inspect(&repository).unwrap().unwrap();
        assert_eq!((snapshot.additions, snapshot.deletions), (1, 0));
    }

    #[test]
    fn counts_tracked_and_untracked_changes_in_a_linked_worktree() {
        let repository = repository();
        let worktree = repository.with_extension("linked-worktree");
        run_git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "linked-worktree",
                worktree.to_str().unwrap(),
            ],
        );
        fs::write(worktree.join("README.md"), "main\nsecond\n").unwrap();
        fs::write(worktree.join("new file.txt"), "first\nsecond").unwrap();
        fs::write(worktree.join("binary.dat"), b"binary\0content\n").unwrap();

        let snapshot = inspect(&worktree).unwrap().unwrap();
        assert_eq!((snapshot.additions, snapshot.deletions), (3, 0));
    }
}

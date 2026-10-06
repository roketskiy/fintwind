//! Daemon-owned live worktree status reads.
//!
//! The module turns one `git status --porcelain=v1 -z` run into the
//! structured [`WorktreeStatus`] the source-control column draws. Every
//! function performs process I/O; callers must run them from the background
//! executor, and render paths consume only the cached value they return.

use std::path::Path;

use anyhow::{Context as _, bail};

use fintwind_protocol::git::{WorktreeStatus, WorktreeStatusEntry};

/// The live worktree status of a workspace. `Ok(None)` means `cwd` is not
/// inside a Git repository, mirroring [`crate::git_log::list`].
pub fn inspect(cwd: &Path) -> anyhow::Result<Option<WorktreeStatus>> {
    let repository_output = crate::command_env::plain_command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .context("failed to execute git")?;
    if !repository_output.status.success() {
        return Ok(None);
    }

    // `-z` terminates records with NUL, so path names containing spaces,
    // quotes, or line breaks cannot corrupt the parsing below, and renames
    // arrive as two NUL-separated fields (new path, then origin).
    let output = crate::command_env::plain_command("git")
        .args([
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--branch",
            "--untracked-files=all",
        ])
        .current_dir(cwd)
        .output()
        .context("failed to execute git status")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!("git status failed: {stderr}");
    }
    parse(String::from_utf8_lossy(&output.stdout).as_ref()).map(Some)
}

fn parse(stdout: &str) -> anyhow::Result<WorktreeStatus> {
    let mut status = WorktreeStatus {
        branch: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        entries: Vec::new(),
    };
    let mut records = stdout.split('\0');
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        if let Some(header) = record.strip_prefix("## ") {
            (status.branch, status.upstream, status.ahead, status.behind) =
                parse_branch_header(header);
            continue;
        }
        let bytes = record.as_bytes();
        if bytes.len() < 4 || bytes[2] != b' ' {
            bail!("git status produced an unreadable record: {record:?}");
        }
        let index_status = bytes[0] as char;
        let worktree_status = bytes[1] as char;
        let path = record[3..].to_owned();
        // Rename and copy records carry their origin path as the next
        // NUL-separated field.
        let origin_path = if matches!(index_status, 'R' | 'C') {
            records.next().map(str::to_owned)
        } else {
            None
        };
        status.entries.push(WorktreeStatusEntry {
            path,
            origin_path,
            index_status,
            worktree_status,
        });
    }
    Ok(status)
}

/// `## main...origin/main [ahead 1, behind 2]`, `## main`,
/// `## No commits yet on main`, or `## HEAD (no branch)`.
fn parse_branch_header(header: &str) -> (Option<String>, Option<String>, u64, u64) {
    let (mut ahead, mut behind) = (0, 0);
    let header = match header.find(" [") {
        Some(index) => {
            let (base, info) = header.split_at(index);
            let info = info.trim_start_matches(" [").trim_end_matches(']');
            for part in info.split(',') {
                let mut fields = part.trim().splitn(2, ' ');
                let name = fields.next().unwrap_or_default();
                let value = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                match name {
                    "ahead" => ahead = value,
                    "behind" => behind = value,
                    _ => {}
                }
            }
            base
        }
        None => header,
    };
    if let Some(branch) = header.strip_prefix("No commits yet on ") {
        return (Some(branch.trim().to_owned()), None, 0, 0);
    }
    if header == "HEAD (no branch)" {
        return (None, None, ahead, behind);
    }
    let mut parts = header.splitn(2, "...");
    let branch = parts.next().unwrap_or_default().trim();
    let upstream = parts.next().map(|value| value.trim().to_owned());
    let branch = (!branch.is_empty()).then(|| branch.to_owned());
    (branch, upstream, ahead, behind)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use uuid::Uuid;

    fn git_ok(cwd: &Path, args: &[&str]) {
        let output = crate::command_env::plain_command("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
    }

    fn repository() -> PathBuf {
        let root = std::env::temp_dir().join(format!("fintwind-status-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        git_ok(&root, &["init", "-b", "main"]);
        fs::write(root.join("tracked.txt"), "baseline\n").unwrap();
        git_ok(&root, &["add", "."]);
        git_ok(
            &root,
            &[
                "-c",
                "user.name=Fintwind Tests",
                "-c",
                "user.email=fintwind@example.com",
                "commit",
                "-m",
                "baseline",
            ],
        );
        root
    }

    fn inspect_ok(root: &Path) -> WorktreeStatus {
        inspect(root).unwrap().expect("expected a repository")
    }

    fn entry<'a>(status: &'a WorktreeStatus, path: &str) -> &'a WorktreeStatusEntry {
        status
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("missing entry for {path}"))
    }

    #[test]
    fn clean_worktree_reports_branch_without_entries() {
        let root = repository();
        let status = inspect_ok(&root);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(status.entries.is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn staged_unstaged_and_untracked_files_keep_their_sides() {
        let root = repository();
        fs::write(root.join("staged.txt"), "staged\n").unwrap();
        git_ok(&root, &["add", "staged.txt"]);
        fs::write(root.join("tracked.txt"), "baseline\nedited\n").unwrap();
        fs::write(root.join("untracked.txt"), "new\n").unwrap();

        let status = inspect_ok(&root);
        let staged = entry(&status, "staged.txt");
        assert_eq!(staged.index_status, 'A');
        assert_eq!(staged.worktree_status, ' ');
        let edited = entry(&status, "tracked.txt");
        assert_eq!(edited.index_status, ' ');
        assert_eq!(edited.worktree_status, 'M');
        let untracked = entry(&status, "untracked.txt");
        assert_eq!(untracked.index_status, '?');
        assert_eq!(untracked.worktree_status, '?');
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rename_records_carry_their_origin_path() {
        let root = repository();
        git_ok(&root, &["mv", "tracked.txt", "renamed.txt"]);
        let status = inspect_ok(&root);
        let renamed = entry(&status, "renamed.txt");
        assert_eq!(renamed.index_status, 'R');
        assert_eq!(renamed.origin_path.as_deref(), Some("tracked.txt"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn detached_head_has_no_branch_name() {
        let root = repository();
        let head = String::from_utf8_lossy(
            &crate::command_env::plain_command("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&root)
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_owned();
        git_ok(&root, &["checkout", "--detach", &head]);
        let status = inspect_ok(&root);
        assert!(status.branch.is_none());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn outside_a_repository_the_status_is_none() {
        let root = std::env::temp_dir().join(format!("fintwind-status-none-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        assert!(inspect(&root).unwrap().is_none());
        fs::remove_dir_all(root).ok();
    }

    /// The same fixture through the workspace protocol entry, mirroring how
    /// the daemon dispatches `InspectStatus`.
    #[test]
    fn inspect_status_operation_dispatches_through_the_workspace_entry() {
        use crate::workspace::{WorkspaceOperation, WorkspaceResult, execute};

        let root = repository();
        fs::write(root.join("tracked.txt"), "baseline\nedited\n").unwrap();
        let WorkspaceResult::WorktreeStatus { status } =
            execute(WorkspaceOperation::InspectStatus { cwd: root.clone() }).unwrap()
        else {
            panic!("unexpected workspace response")
        };
        let status = status.expect("expected a repository");
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.entries.len(), 1);
        assert_eq!(status.entries[0].path, "tracked.txt");
        assert_eq!(status.entries[0].worktree_status, 'M');
        fs::remove_dir_all(root).ok();
    }
}

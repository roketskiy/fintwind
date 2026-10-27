//! Blackbox check of the review toolbar's area totals.
//!
//! Runs the same `workspace::execute` entry point the daemon calls, against a
//! real Git repository built here. Each review source must report its own
//! range: staged changes must not include the worktree, unstaged changes must
//! not include the index, and untracked files must count as additions for the
//! sources that show them.
//!
//! Run: `cargo run -p fintwind-core --example git_review_stats_blackbox`

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::bail;
use fintwind_core::workspace::{self, WorkspaceOperation, WorkspaceResult};
use fintwind_protocol::workspace::ReviewDiffSource;
use uuid::Uuid;

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents).expect("write fixture");
}

fn stats(cwd: &Path, source: ReviewDiffSource) -> (u64, u64, u64, bool) {
    match workspace::execute(WorkspaceOperation::CollectReviewStats {
        cwd: cwd.to_path_buf(),
        source,
        query_id: None,
    })
    .expect("review stats")
    {
        WorkspaceResult::ReviewStats {
            additions,
            deletions,
            files,
            truncated,
        } => (additions, deletions, files, truncated),
        other => panic!("unexpected result {other:?}"),
    }
}

fn repository() -> PathBuf {
    let root = std::env::temp_dir().join(format!("fintwind-stats-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create repository");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "core.autocrlf", "false"]);
    write(&root.join("tracked.txt"), "one\ntwo\nthree\n");
    git(&root, &["add", "."]);
    git(
        &root,
        &[
            "-c",
            "user.name=Fintwind Check",
            "-c",
            "user.email=fintwind@example.com",
            "commit",
            "-q",
            "-m",
            "baseline",
        ],
    );
    root
}

fn main() -> anyhow::Result<()> {
    let check = |name: &str, ok: bool| -> anyhow::Result<()> {
        if ok {
            println!("[OK] {name}");
            Ok(())
        } else {
            bail!("{name}")
        }
    };

    let root = repository();
    // Worktree only: one line replaced, one line added.
    write(&root.join("tracked.txt"), "one\ntwo changed\nthree\nfour\n");
    // Index only: a new two-line file.
    write(&root.join("staged.txt"), "alpha\nbeta\n");
    git(&root, &["add", "staged.txt"]);
    // Neither: an untracked three-line file.
    write(&root.join("loose.txt"), "a\nb\nc\n");

    let (additions, deletions, files, truncated) = stats(&root, ReviewDiffSource::Unstaged);
    check(
        "unstaged totals cover the worktree plus untracked files",
        (additions, deletions, files) == (5, 1, 2) && !truncated,
    )?;

    let (additions, deletions, files, truncated) = stats(&root, ReviewDiffSource::Staged);
    check(
        "staged totals cover the index only",
        (additions, deletions, files) == (2, 0, 1) && !truncated,
    )?;

    let (additions, deletions, files, truncated) = stats(&root, ReviewDiffSource::Uncommitted);
    check(
        "uncommitted totals cover index, worktree, and untracked files",
        (additions, deletions, files) == (7, 1, 3) && !truncated,
    )?;

    // A file beyond the per-file budget cannot be counted, and the answer must
    // say so rather than pass a lower bound off as the total.
    write(&root.join("huge.log"), &"x".repeat(9 * 1024 * 1024));
    let (additions, deletions, files, truncated) = stats(&root, ReviewDiffSource::Unstaged);
    check(
        "an oversized untracked file marks the totals partial",
        (additions, deletions, files) == (5, 1, 3) && truncated,
    )?;
    std::fs::remove_file(root.join("huge.log"))?;

    // Committing removes the live totals without changing the committed one.
    git(&root, &["add", "-A"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Fintwind Check",
            "-c",
            "user.email=fintwind@example.com",
            "commit",
            "-q",
            "-m",
            "second",
        ],
    );
    let (additions, deletions, files, truncated) = stats(&root, ReviewDiffSource::Uncommitted);
    check(
        "a clean worktree reports nothing",
        (additions, deletions, files) == (0, 0, 0) && !truncated,
    )?;

    let _ = std::fs::remove_dir_all(&root);
    println!("git review stats blackbox passed");
    Ok(())
}

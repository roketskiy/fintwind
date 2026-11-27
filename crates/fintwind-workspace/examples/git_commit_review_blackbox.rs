//! Blackbox check of the commit history's "view changes" review source.
//!
//! Runs the same `workspace::execute` entry point the daemon calls, against a
//! real Git repository built here. A commit's review must show only that
//! commit's own changes: against its first parent, against the empty tree for
//! a root commit, and never the live worktree or index. Its totals must agree
//! with the history panel's commit stats.
//!
//! Run: `cargo run -p fintwind-core --example git_commit_review_blackbox`

use std::path::Path;
use std::process::Command;

use anyhow::bail;
use fintwind_workspace::workspace::{self, WorkspaceOperation, WorkspaceResult};
use fintwind_protocol::workspace::{ReviewDiffData, ReviewDiffSource};
use uuid::Uuid;

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout)
        .expect("git output is utf-8")
        .trim()
        .to_owned()
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents).expect("write fixture");
}

fn commit(cwd: &Path, message: &str) -> String {
    git(cwd, &["add", "-A"]);
    git(
        cwd,
        &[
            "-c",
            "user.name=Fintwind Check",
            "-c",
            "user.email=fintwind@example.com",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
    git_stdout(cwd, &["rev-parse", "HEAD"])
}

fn diff(cwd: &Path, hash: &str) -> ReviewDiffData {
    match workspace::execute(WorkspaceOperation::CollectReviewDiff {
        cwd: cwd.to_path_buf(),
        source: ReviewDiffSource::Commit {
            hash: hash.to_owned(),
        },
    })
    .expect("commit review diff")
    {
        WorkspaceResult::ReviewDiff { data } => data,
        other => panic!("unexpected result {other:?}"),
    }
}

fn stats(cwd: &Path, hash: &str) -> (u64, u64, u64, bool) {
    match workspace::execute(WorkspaceOperation::CollectReviewStats {
        cwd: cwd.to_path_buf(),
        source: ReviewDiffSource::Commit {
            hash: hash.to_owned(),
        },
        query_id: None,
    })
    .expect("commit review stats")
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

fn main() -> anyhow::Result<()> {
    let check = |name: &str, ok: bool| -> anyhow::Result<()> {
        if ok {
            println!("[OK] {name}");
            Ok(())
        } else {
            bail!("{name}")
        }
    };

    let root = std::env::temp_dir().join(format!("fintwind-commit-review-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create repository");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "core.autocrlf", "false"]);

    // A root commit, a normal commit, and a --no-ff merge with two parents.
    write(&root.join("base.txt"), "one\ntwo\nthree\n");
    let root_hash = commit(&root, "baseline");
    write(&root.join("base.txt"), "one\ntwo changed\nthree\nfour\n");
    write(&root.join("added.txt"), "alpha\nbeta\n");
    let second_hash = commit(&root, "second");
    git(&root, &["checkout", "-q", "-b", "side", &root_hash]);
    write(&root.join("side.txt"), "side\n");
    let side_hash = commit(&root, "side work");
    git(&root, &["checkout", "-q", "main"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Fintwind Check",
            "-c",
            "user.email=fintwind@example.com",
            "merge",
            "-q",
            "--no-ff",
            "--no-edit",
            "-m",
            "merge side",
            "side",
        ],
    );
    let merge_hash = git_stdout(&root, &["rev-parse", "HEAD"]);

    let second = diff(&root, &second_hash);
    check(
        "a commit's numstat covers only its own changes",
        second.numstat == "2\t0\tadded.txt\n2\t1\tbase.txt\n",
    )?;
    check(
        "a commit's patch carries its changed lines and nothing later",
        second.patch.contains("+two changed")
            && second.patch.contains("+four")
            && second.patch.contains("+alpha")
            && !second.patch.contains("side.txt"),
    )?;
    check(
        "a commit's full context is retained",
        second.complete_context && !second.truncated,
    )?;
    let (additions, deletions, files, truncated) = stats(&root, &second_hash);
    check(
        "a commit's toolbar totals match its diff",
        (additions, deletions, files) == (4, 1, 2) && !truncated,
    )?;
    let history = fintwind_git::git_log::stats(&root, &second_hash)?;
    check(
        "review totals agree with the history panel's commit stats",
        (history.additions, history.deletions, history.files) == (additions, deletions, files),
    )?;

    let root_diff = diff(&root, &root_hash);
    check(
        "a root commit shows every file as added against the empty tree",
        root_diff.numstat == "3\t0\tbase.txt\n" && root_diff.patch.contains("new file mode"),
    )?;

    let merge_diff = diff(&root, &merge_hash);
    check(
        "a merge commit follows its first parent",
        merge_diff.numstat == "1\t0\tside.txt\n" && !merge_diff.patch.contains("base.txt"),
    )?;

    // Live state must not leak into a commit's review. The side commit is
    // diffed for the first time here, so no cached answer can mask a leak.
    write(&root.join("base.txt"), "dirty\n");
    write(&root.join("staged.txt"), "staged\n");
    git(&root, &["add", "staged.txt"]);
    write(&root.join("loose.txt"), "untracked\n");
    let side_diff = diff(&root, &side_hash);
    check(
        "a dirty worktree and index do not leak into a commit's review",
        side_diff.numstat == "1\t0\tside.txt\n",
    )?;
    let second_again = diff(&root, &second_hash);
    check(
        "a commit's review is stable across worktree changes",
        second_again.numstat == second.numstat && second_again.patch == second.patch,
    )?;

    let missing = "0".repeat(40);
    check(
        "an unknown hash is rejected",
        workspace::execute(WorkspaceOperation::CollectReviewDiff {
            cwd: root.clone(),
            source: ReviewDiffSource::Commit { hash: missing },
        })
        .is_err(),
    )?;
    check(
        "a malformed hash is rejected",
        workspace::execute(WorkspaceOperation::CollectReviewDiff {
            cwd: root.clone(),
            source: ReviewDiffSource::Commit {
                hash: "HEAD".to_owned(),
            },
        })
        .is_err(),
    )?;

    let _ = std::fs::remove_dir_all(&root);
    println!("git commit review blackbox passed");
    Ok(())
}

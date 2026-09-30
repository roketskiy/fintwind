//! Non-visual Git-to-graph E2E check. Creates only a disposable fixture repo.
//! Run: cargo run --locked -p fintwind-core --example verify_commit_graph -- <output-dir>
//! Failures covered: lost/reordered parents, clock-skew ordering, broken merge
//! lanes, roots, truncated history, branch filters, refs/unpushed state, empty
//! repositories, non-repositories, invalid revisions and wire round trips.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, ensure};
use fintwind_client::git_history::{CommitEntry, CommitGraph, GraphHalf};
use fintwind_core::{WorkspaceOperation, WorkspaceResult};
use serde_json::json;

fn git(cwd: &Path, args: &[&str], date: Option<&str>) -> anyhow::Result<String> {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);
    if let Some(date) = date {
        command
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date);
    }
    let output = command.output()?;
    ensure!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn commit(repo: &Path, subject: &str, date: &str) -> anyhow::Result<String> {
    git(
        repo,
        &["commit", "--allow-empty", "-m", subject],
        Some(date),
    )?;
    git(repo, &["rev-parse", "HEAD"], None)
}

fn fetch(
    repo: &Path,
    limit: usize,
    branch: Option<&str>,
) -> anyhow::Result<Option<Vec<CommitEntry>>> {
    let operation = WorkspaceOperation::ListCommits {
        cwd: repo.to_path_buf(),
        limit,
        branch: branch.map(str::to_owned),
    };
    let operation = serde_json::from_slice(&serde_json::to_vec(&operation)?)?;
    let result = fintwind_core::workspace::execute(operation)?;
    let result = serde_json::from_slice(&serde_json::to_vec(&result)?)?;
    match result {
        WorkspaceResult::Commits { commits } => Ok(commits),
        _ => anyhow::bail!("unexpected workspace response"),
    }
}

fn verify_edges(commits: &[CommitEntry], graph: &CommitGraph) -> anyhow::Result<()> {
    ensure!(
        commits.len() == graph.rows.len(),
        "graph/commit count mismatch"
    );
    let positions: HashMap<_, _> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.hash.as_str(), i))
        .collect();
    for (index, commit) in commits.iter().enumerate() {
        let row = &graph.rows[index];
        let outgoing: Vec<_> = row
            .strokes
            .iter()
            .filter(|s| s.half == GraphHalf::Bottom)
            .collect();
        ensure!(outgoing.len() == commit.parents.len(), "lost parent edge");
        for (parent, edge) in commit.parents.iter().zip(outgoing) {
            ensure!(edge.from == row.lane, "edge does not leave commit node");
            let Some(&parent_index) = positions.get(parent.as_str()) else {
                continue;
            };
            ensure!(parent_index > index, "parent precedes child");
            let mut lane = edge.to;
            for next in &graph.rows[index + 1..=parent_index] {
                let end = std::ptr::eq(next, &graph.rows[parent_index]);
                let continuation = next.strokes.iter().find(|s| {
                    s.from == lane
                        && s.half
                            == if end {
                                GraphHalf::Top
                            } else {
                                GraphHalf::Through
                            }
                });
                let continuation = continuation.context("discontinuous parent edge")?;
                lane = continuation.to;
            }
            ensure!(
                lane == graph.rows[parent_index].lane,
                "edge reaches wrong node"
            );
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("missing output directory")?;
    std::fs::create_dir_all(&output)?;
    let repo = output.join(format!("fixture-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"], None)?;
    git(&repo, &["config", "user.name", "Graph Fixture"], None)?;
    git(
        &repo,
        &["config", "user.email", "graph@example.invalid"],
        None,
    )?;
    git(&repo, &["config", "commit.gpgsign", "false"], None)?;
    git(
        &repo,
        &["config", "core.hooksPath", ".disabled-hooks"],
        None,
    )?;
    ensure!(
        fetch(&repo, 100, None)?.is_some_and(|c| c.is_empty()),
        "unborn repository is not empty"
    );
    let root = commit(&repo, "Root", "2026-01-01T12:00:00Z")?;
    git(&repo, &["branch", "feature"], None)?;
    // Deliberately skew dates; topology, not timestamps, must define row order.
    let main = commit(&repo, "Main change", "2026-01-05T12:00:00Z")?;
    git(&repo, &["checkout", "feature"], None)?;
    let feature = commit(&repo, "Feature change", "2026-01-03T12:00:00Z")?;
    git(&repo, &["checkout", "main"], None)?;
    git(
        &repo,
        &["merge", "--no-ff", "feature", "-m", "Merge feature"],
        Some("2026-01-02T12:00:00Z"),
    )?;
    let merge = git(&repo, &["rev-parse", "HEAD"], None)?;
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/fixture.git",
        ],
        None,
    )?;
    git(
        &repo,
        &["update-ref", "refs/remotes/origin/main", &merge],
        None,
    )?;
    git(&repo, &["tag", "v1.0.0", &merge], None)?;
    let head = commit(&repo, "Unpushed tip", "2026-01-06T12:00:00Z")?;
    let commits = fetch(&repo, 100, None)?.context("fixture not recognized as repo")?;
    ensure!(
        commits.len() == 5 && commits[0].hash == head,
        "wrong HEAD history"
    );
    let merge_entry = commits
        .iter()
        .find(|c| c.hash == merge)
        .context("merge missing")?;
    ensure!(
        merge_entry.parents == [main.clone(), feature.clone()],
        "merge parent order changed"
    );
    ensure!(
        merge_entry.pushed && !commits[0].pushed && commits[0].is_head,
        "pushed/HEAD state lost"
    );
    ensure!(
        merge_entry.refs.iter().any(|r| r.remote)
            && merge_entry.refs.iter().any(|r| r.label == "tag: v1.0.0"),
        "refs lost"
    );
    ensure!(
        commits
            .last()
            .is_some_and(|c| c.hash == root && c.parents.is_empty()),
        "root has fabricated parents"
    );
    let graph = CommitGraph::new(&commits);
    verify_edges(&commits, &graph)?;
    ensure!(graph.width >= 2, "merge did not open a lane");
    let branch = fetch(&repo, 100, Some("feature"))?.context("branch missing")?;
    ensure!(
        branch.len() == 2 && branch[0].hash == feature && branch[1].hash == root,
        "branch filter changed"
    );
    verify_edges(&branch, &CommitGraph::new(&branch))?;
    let limited = fetch(&repo, 2, None)?.context("limited history missing")?;
    ensure!(
        limited.len() == 2 && limited[1].parents.len() == 2,
        "limit lost outgoing parents"
    );
    verify_edges(&limited, &CommitGraph::new(&limited))?;
    ensure!(
        fetch(&repo, 100, Some("missing-branch")).is_err(),
        "invalid branch is not an error"
    );
    ensure!(
        fetch(&output, 100, None)?.is_none(),
        "non-repository returned commits"
    );
    let rows: Vec<_> = graph
        .rows
        .iter()
        .zip(&commits)
        .map(|(row, commit)| {
            json!({
                "hash": commit.hash, "subject": commit.subject, "parents": commit.parents,
                "lane": row.lane, "strokes": row.strokes.iter().map(|s| json!({
                    "from": s.from, "to": s.to, "half": format!("{:?}", s.half),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let report = output.join("commit-graph-report.json");
    std::fs::write(
        &report,
        serde_json::to_vec_pretty(&json!({
            "status": "passed", "fixture": repo, "width": graph.width, "rows": rows,
            "checks": ["wire round trip", "empty repository", "clock-skew topology", "merge edges", "root", "refs and unpushed", "branch filter", "truncation", "invalid revision", "non-repository"],
        }))?,
    )?;
    println!("[OK] Git-to-graph E2E passed: {}", report.display());
    Ok(())
}

//! Non-visual Git-to-graph E2E check. Creates only a disposable fixture repo.
//! Run: cargo run --locked -p fintwind-core --example verify_commit_graph -- <output-dir>
//! Failures covered: lost/reordered parents, clock-skew ordering, broken merge
//! lanes, roots, truncated history, branch filters, refs/unpushed state, empty
//! repositories, non-repositories, invalid revisions and wire round trips.
//! Also checks phantom strokes, boundary/color continuity, premature joins,
//! nested/octopus merges, disconnected tips and real repository windows.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, ensure};
use fintwind_client::git_history::{CommitEntry, CommitGraph, GraphHalf, MAIN_COLOR};
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
    let mut previous_bottom = BTreeSet::new();
    for (index, commit) in commits.iter().enumerate() {
        let row = &graph.rows[index];
        let mut top = BTreeSet::new();
        let mut bottom = BTreeSet::new();
        for stroke in &row.strokes {
            ensure!(
                stroke.from < graph.width && stroke.to < graph.width,
                "stroke outside graph at {}",
                commit.subject
            );
            if stroke.half != GraphHalf::Bottom {
                ensure!(
                    top.insert((stroke.from, stroke.color)),
                    "overlapping incoming strokes"
                );
            }
            if stroke.half != GraphHalf::Top {
                // An extra parent can fork into an already active edge, so
                // several paths may share one outgoing boundary endpoint.
                bottom.insert((stroke.to, stroke.color));
            }
            match stroke.half {
                GraphHalf::Top => ensure!(stroke.to == row.lane, "arrival misses node"),
                GraphHalf::Bottom => ensure!(stroke.from == row.lane, "parent misses node"),
                GraphHalf::Through => ensure!(
                    stroke.from == stroke.to && stroke.from != row.lane,
                    "unrelated lane bends or crosses node at {}",
                    commit.subject
                ),
            }
        }
        ensure!(
            top == previous_bottom,
            "phantom/discontinuous/color-changing boundary at {}: above={previous_bottom:?}, below={top:?}",
            commit.subject
        );
        previous_bottom = bottom;
        let outgoing: Vec<_> = row
            .strokes
            .iter()
            .filter(|s| s.half == GraphHalf::Bottom)
            .collect();
        ensure!(outgoing.len() == commit.parents.len(), "lost parent edge");
        if let Some(first_parent) = outgoing.first() {
            ensure!(
                first_parent.to == row.lane && first_parent.color == row.color,
                "first-parent branch bends or changes color before its ancestor at {}",
                commit.subject
            );
        }
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
                ensure!(
                    continuation.color == edge.color,
                    "parent edge changes color"
                );
                lane = continuation.to;
            }
            ensure!(
                lane == graph.rows[parent_index].lane,
                "edge reaches wrong node"
            );
        }
    }
    if commits
        .iter()
        .flat_map(|commit| &commit.parents)
        .all(|parent| positions.contains_key(parent.as_str()))
    {
        ensure!(
            previous_bottom.is_empty(),
            "complete history has dangling edges"
        );
    }
    let mut main = commits
        .iter()
        .find(|commit| commit.is_head)
        .or(commits.first());
    while let Some(commit) = main {
        let row = &graph.rows[positions[commit.hash.as_str()]];
        ensure!(
            row.lane == 0 && row.color == MAIN_COLOR,
            "mainline moved or changed color"
        );
        main = commit
            .parents
            .first()
            .and_then(|parent| positions.get(parent.as_str()))
            .map(|index| &commits[*index]);
    }
    Ok(())
}

fn graph_rows(commits: &[CommitEntry], graph: &CommitGraph) -> serde_json::Value {
    json!({
        "width": graph.width,
        "rows": graph.rows.iter().zip(commits).map(|(row, commit)| json!({
            "hash": commit.hash, "subject": commit.subject, "parents": commit.parents,
            "lane": row.lane, "color": row.color,
            "strokes": row.strokes.iter().map(|stroke| json!({
                "from": stroke.from, "to": stroke.to, "half": format!("{:?}", stroke.half),
                "color": stroke.color,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn verify_window(
    repo: &Path,
    limit: usize,
    branch: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let commits = fetch(repo, limit, branch)?.context("repository missing")?;
    let graph = CommitGraph::new(&commits);
    verify_edges(&commits, &graph)?;
    Ok(json!({ "limit": limit, "branch": branch, "graph": graph_rows(&commits, &graph) }))
}

// Real Git objects and refs exercise ListCommits ordering and serialization too.
fn graph_commit(
    repo: &Path,
    tree: &str,
    subject: &str,
    parents: &[&str],
) -> anyhow::Result<String> {
    let mut args = vec!["commit-tree", tree, "-m", subject];
    for parent in parents {
        args.extend(["-p", parent]);
    }
    git(repo, &args, Some("2026-02-01T12:00:00Z"))
}

fn verify_complex_history(repo: &Path, base: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    let tree = git(repo, &["rev-parse", "HEAD^{tree}"], None)?;
    let left = graph_commit(repo, &tree, "Left", &[base])?;
    let left_tip = graph_commit(repo, &tree, "Left tip", &[&left])?;
    let right = graph_commit(repo, &tree, "Right", &[base])?;
    let nested = graph_commit(repo, &tree, "Nested merge", &[&left_tip, &right])?;
    let main = graph_commit(repo, &tree, "Main before merge", &[base])?;
    let merge = graph_commit(repo, &tree, "Outer merge", &[&main, &nested])?;
    let other = graph_commit(repo, &tree, "Other tip", &[base])?;
    let octopus = graph_commit(repo, &tree, "Octopus merge", &[&merge, &other, &right])?;
    let independent = graph_commit(repo, &tree, "Independent root", &[])?;
    git(repo, &["update-ref", "refs/heads/main", &octopus], None)?;
    git(
        repo,
        &["update-ref", "refs/heads/unmerged", &left_tip],
        None,
    )?;
    git(
        repo,
        &["update-ref", "refs/heads/independent", &independent],
        None,
    )?;
    let mut windows = Vec::new();
    for limit in [1, 2, 3, 5, 100] {
        windows.push(verify_window(repo, limit, None)?);
    }
    windows.push(verify_window(repo, 100, Some("unmerged"))?);
    windows.push(verify_window(repo, 100, Some("independent"))?);
    // An unmerged tip can appear above HEAD. Reserving lane zero must not
    // create a mainline stroke above HEAD or let that tip steal lane zero.
    let future = graph_commit(repo, &tree, "Unmerged above HEAD", &[&octopus])?;
    git(repo, &["update-ref", "refs/heads/future", &future], None)?;
    windows.push(verify_window(repo, 100, None)?);
    Ok(windows)
}

// Exercise the daemon entry and wire format, not a duplicate of its parser.
fn verify_stats(
    repo: &Path,
    hash: &str,
    expected: (u64, u64, u64),
) -> anyhow::Result<serde_json::Value> {
    let operation = WorkspaceOperation::ReadCommitStats {
        cwd: repo.to_path_buf(),
        hash: hash.to_owned(),
    };
    let operation = serde_json::from_slice(&serde_json::to_vec(&operation)?)?;
    let result = fintwind_core::workspace::execute(operation)?;
    let result = serde_json::from_slice(&serde_json::to_vec(&result)?)?;
    let WorkspaceResult::CommitStats { stats } = result else {
        anyhow::bail!("unexpected commit stats response");
    };
    ensure!(
        (stats.files, stats.additions, stats.deletions) == expected,
        "incorrect stats for {hash}: {stats:?}, expected {expected:?}"
    );
    Ok(json!({ "hash": hash, "stats": stats }))
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
    std::fs::write(repo.join("root.txt"), "one\ntwo\n")?;
    git(&repo, &["add", "root.txt"], None)?;
    let root = commit(&repo, "Root", "2026-01-01T12:00:00Z")?;
    let root_stats = verify_stats(&repo, &root, (1, 2, 0))?;
    git(&repo, &["branch", "feature"], None)?;
    // Deliberately skew dates; topology, not timestamps, must define row order.
    std::fs::write(repo.join("root.txt"), "one\nthree\nfour\n")?;
    git(&repo, &["add", "root.txt"], None)?;
    let main = commit(&repo, "Main change", "2026-01-05T12:00:00Z")?;
    let main_stats = verify_stats(&repo, &main, (1, 2, 1))?;
    git(&repo, &["checkout", "feature"], None)?;
    std::fs::write(repo.join("binary.dat"), [0, 1, 2, 0, 3])?;
    git(&repo, &["add", "binary.dat"], None)?;
    let feature = commit(&repo, "Feature change", "2026-01-03T12:00:00Z")?;
    let binary_stats = verify_stats(&repo, &feature, (1, 0, 0))?;
    git(&repo, &["checkout", "main"], None)?;
    git(
        &repo,
        &["merge", "--no-ff", "feature", "-m", "Merge feature"],
        Some("2026-01-02T12:00:00Z"),
    )?;
    let merge = git(&repo, &["rev-parse", "HEAD"], None)?;
    let merge_stats = verify_stats(&repo, &merge, (1, 0, 0))?;
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
    let empty_stats = verify_stats(&repo, &head, (0, 0, 0))?;
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
    // The side branch reaches the common ancestor before the first-parent
    // chain does in this topological order. It must not steal the main lane.
    for hash in [&head, &merge, &main, &root] {
        let index = commits
            .iter()
            .position(|commit| &commit.hash == hash)
            .context("main commit missing")?;
        ensure!(
            graph.rows[index].lane == 0,
            "first-parent chain changed lanes at {hash}"
        );
    }
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
    git(&repo, &["mv", "root.txt", "renamed.txt"], None)?;
    let renamed = commit(&repo, "Rename", "2026-01-07T12:00:00Z")?;
    let rename_stats = verify_stats(&repo, &renamed, (1, 0, 0))?;
    git(&repo, &["rm", "renamed.txt"], None)?;
    let deleted = commit(&repo, "Delete", "2026-01-08T12:00:00Z")?;
    let delete_stats = verify_stats(&repo, &deleted, (1, 0, 3))?;
    ensure!(
        fintwind_core::workspace::execute(WorkspaceOperation::ReadCommitStats {
            cwd: repo.clone(),
            hash: "--output=unexpected".to_owned(),
        })
        .is_err(),
        "option-shaped hash was accepted"
    );
    ensure!(
        fintwind_core::workspace::execute(WorkspaceOperation::ReadCommitStats {
            cwd: repo.clone(),
            hash: "a".repeat(40),
        })
        .is_err(),
        "missing commit was not an error"
    );
    let complex = verify_complex_history(&repo, &deleted)?;
    let real_repository = std::env::args_os().nth(2).map(PathBuf::from);
    let mut real_windows = Vec::new();
    if let Some(repository) = &real_repository {
        for limit in [1, 2, 10, 100, 500] {
            real_windows.push(verify_window(repository, limit, None)?);
        }
    }
    let report = output.join("commit-graph-report.json");
    std::fs::write(
        &report,
        serde_json::to_vec_pretty(&json!({
            "status": "passed", "fixture": repo, "graph": graph_rows(&commits, &graph),
            "complex_windows": complex, "real_repository": real_repository, "real_windows": real_windows,
            "stats_checks": ["root", "text additions and deletions", "binary", "first-parent merge", "empty commit", "rename", "delete", "invalid hash", "missing commit"],
            "stats_results": [root_stats, main_stats, binary_stats, merge_stats, empty_stats, rename_stats, delete_stats],
            "checks": ["wire round trip", "empty repository", "clock-skew topology", "merge edges", "stable first-parent lane and color", "boundary continuity without phantom strokes", "no premature joins", "nested and octopus merges", "disconnected and unmerged tips", "root", "refs and unpushed", "branch filter", "truncation", "invalid revision", "non-repository"],
        }))?,
    )?;
    println!("[OK] Git-to-graph E2E passed: {}", report.display());
    Ok(())
}

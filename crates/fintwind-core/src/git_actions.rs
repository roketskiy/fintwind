//! Daemon-owned Git write operations behind the source-control page:
//! staging, unstaging, discarding worktree changes, fetch, and pull. Every
//! entry point performs process I/O and must run on the background
//! executor; the UI refreshes its cached status snapshot afterwards.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, bail};

/// Stages files by literal repository-relative path. `--` separates the
/// paths from the subcommand, so leading dashes and pathspec magic stay
/// inert; Git treats the rest as plain names.
pub fn stage(cwd: &Path, paths: &[String]) -> anyhow::Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let root = repository_root(cwd)?;
    run(
        &root,
        &with_paths(&["--literal-pathspecs", "add", "--"], paths),
    )
}

/// Unstages files without touching the worktree. A repository without any
/// commit has no `HEAD` to reset to, so a failed reset falls back to
/// dropping the entries from the index.
pub fn unstage(cwd: &Path, paths: &[String]) -> anyhow::Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let root = repository_root(cwd)?;
    if run_ok(
        &root,
        &["rev-parse".into(), "--verify".into(), "HEAD".into()],
    ) {
        run(
            &root,
            &with_paths(&["--literal-pathspecs", "reset", "-q", "HEAD", "--"], paths),
        )
    } else {
        run(
            &root,
            &with_paths(
                &["--literal-pathspecs", "rm", "--cached", "-r", "-q", "--"],
                paths,
            ),
        )
    }
}

/// Discards unstaged worktree changes. Tracked files restore from the
/// index, so the staged side survives; untracked files are deleted from
/// disk, which Git cannot bring back — the UI confirms before calling.
pub fn discard_worktree(
    cwd: &Path,
    tracked: &[String],
    untracked: &[String],
) -> anyhow::Result<()> {
    let root = repository_root(cwd)?;
    let canonical_root = std::fs::canonicalize(&root)?;
    let mut targets = Vec::new();
    let mut checked_parents = std::collections::HashSet::new();
    let current_untracked = if untracked.is_empty() {
        std::collections::HashSet::new()
    } else {
        let mut command = crate::command_env::plain_command("git");
        command
            .args(with_paths(
                &[
                    "--literal-pathspecs",
                    "ls-files",
                    "--others",
                    "--exclude-standard",
                    "-z",
                    "--",
                ],
                untracked,
            ))
            .current_dir(&root);
        let output =
            crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
        if !output.status.success() {
            bail!(
                "failed to inspect untracked files: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect()
    };
    for path in untracked {
        if Path::new(path).is_absolute()
            || Path::new(path)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            bail!("refusing to delete outside the worktree: {path}");
        }
        let target = root.join(path);
        if target.is_file() {
            if !current_untracked.contains(path) {
                bail!("file is no longer untracked: {path}");
            }
            let parent = target.parent().context("file has no parent directory")?;
            if !std::fs::canonicalize(parent)?.starts_with(&canonical_root) {
                bail!("refusing to delete outside the worktree: {path}");
            }
            let mut ancestor = root.clone();
            for component in Path::new(path)
                .parent()
                .into_iter()
                .flat_map(Path::components)
            {
                let Component::Normal(name) = component else {
                    bail!("invalid worktree path: {path}");
                };
                ancestor.push(name);
                if checked_parents.insert(ancestor.clone()) {
                    if !std::fs::canonicalize(&ancestor)?.starts_with(&canonical_root) {
                        bail!("refusing to delete outside the worktree: {path}");
                    }
                    if ancestor.join(".git").try_exists()? {
                        bail!("refusing to delete inside a separate Git repository: {path}");
                    }
                }
            }
            targets.push((target, path));
        }
    }
    if !tracked.is_empty() {
        run(
            &root,
            &with_paths(&["--literal-pathspecs", "restore", "--"], tracked),
        )?;
    }
    for (target, path) in targets {
        std::fs::remove_file(&target).with_context(|| format!("failed to delete {path}"))?;
    }
    Ok(())
}

fn repository_root(cwd: &Path) -> anyhow::Result<PathBuf> {
    let mut command = crate::command_env::plain_command("git");
    command
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
    if !output.status.success() {
        bail!("this directory is not a Git repository");
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim_end_matches(['\r', '\n']),
    ))
}

/// Fetches every remote and prunes stale tracking refs.
pub fn fetch(cwd: &Path) -> anyhow::Result<()> {
    ensure_repository(cwd)?;
    run(cwd, &["fetch".to_owned(), "--prune".to_owned()])
}

/// Pulls with fast-forward only. A diverged branch is an error the UI
/// surfaces — never an automatic merge, rebase, or stash.
pub fn pull_fast_forward(cwd: &Path) -> anyhow::Result<()> {
    ensure_repository(cwd)?;
    run(cwd, &["pull".to_owned(), "--ff-only".to_owned()])
}

fn ensure_repository(cwd: &Path) -> anyhow::Result<()> {
    if run_ok(cwd, &["rev-parse".to_owned(), "--show-toplevel".to_owned()]) {
        return Ok(());
    }
    bail!("this directory is not a Git repository")
}

fn with_paths(prefix: &[&str], paths: &[String]) -> Vec<String> {
    prefix
        .iter()
        .map(|argument| argument.to_string())
        .chain(paths.iter().cloned())
        .collect()
}

fn run(cwd: &Path, args: &[String]) -> anyhow::Result<()> {
    let mut command = crate::command_env::plain_command("git");
    command.args(args).current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    bail!("git failed: {stderr}");
}

fn run_ok(cwd: &Path, args: &[String]) -> bool {
    let mut command = crate::command_env::plain_command("git");
    command.args(args).current_dir(cwd);
    crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)
        .is_ok_and(|output| output.status.success())
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

    fn git_text(cwd: &Path, args: &[&str]) -> String {
        String::from_utf8_lossy(
            &crate::command_env::plain_command("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap()
                .stdout,
        )
        .into_owned()
    }

    /// A committed repository with one tracked file at a known baseline.
    fn repository() -> PathBuf {
        let root = std::env::temp_dir().join(format!("fintwind-actions-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        git_ok(&root, &["init", "-b", "main"]);
        git_ok(&root, &["config", "core.autocrlf", "false"]);
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

    fn status_line(root: &Path) -> String {
        let text = git_text(root, &["status", "--porcelain"]);
        // Porcelain lines start with their status columns; only trailing
        // whitespace is safe to trim.
        text.lines()
            .next()
            .map(|line| line.trim_end().to_owned())
            .unwrap_or_default()
    }

    #[test]
    fn stage_unstage_and_discard_round_trip_one_file() {
        let root = repository();
        fs::write(root.join("tracked.txt"), "edited\n").unwrap();

        stage(&root, &["tracked.txt".to_owned()]).unwrap();
        assert_eq!(status_line(&root), "M  tracked.txt");

        unstage(&root, &["tracked.txt".to_owned()]).unwrap();
        assert_eq!(status_line(&root), " M tracked.txt");

        discard_worktree(&root, &["tracked.txt".to_owned()], &[]).unwrap();
        assert_eq!(status_line(&root), "");
        assert_eq!(
            fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "baseline\n"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discard_keeps_the_staged_side_and_deletes_untracked_files() {
        let root = repository();
        fs::write(root.join("tracked.txt"), "staged then worktree edit\n").unwrap();
        git_ok(&root, &["add", "tracked.txt"]);
        fs::write(
            root.join("tracked.txt"),
            "staged then worktree edit\nmore\n",
        )
        .unwrap();
        fs::write(root.join("untracked.txt"), "new\n").unwrap();

        discard_worktree(
            &root,
            &["tracked.txt".to_owned()],
            &["untracked.txt".to_owned()],
        )
        .unwrap();

        assert_eq!(status_line(&root), "M  tracked.txt");
        assert_eq!(
            fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "staged then worktree edit\n"
        );
        assert!(!root.join("untracked.txt").exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unstaging_without_a_head_falls_back_to_the_index() {
        let root = std::env::temp_dir().join(format!("fintwind-actions-nohead-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        git_ok(&root, &["init", "-b", "main"]);
        fs::write(root.join("new.txt"), "added\n").unwrap();
        git_ok(&root, &["add", "new.txt"]);
        assert_eq!(status_line(&root), "A  new.txt");

        unstage(&root, &["new.txt".to_owned()]).unwrap();
        assert_eq!(status_line(&root), "?? new.txt");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn deletion_and_rename_paths_stay_literal() {
        let root = repository();
        fs::remove_file(root.join("tracked.txt")).unwrap();
        discard_worktree(&root, &["tracked.txt".to_owned()], &[]).unwrap();
        assert_eq!(status_line(&root), "");
        assert!(root.join("tracked.txt").exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn untracked_deletion_refuses_paths_outside_the_worktree() {
        let root = repository();
        let result = discard_worktree(&root, &[], &["../outside.txt".to_owned()]);
        assert!(result.is_err());
        fs::remove_dir_all(root).ok();
    }
}

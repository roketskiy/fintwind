//! Blackbox check of the paginated source-control listings.
//!
//! Runs the same `workspace::execute` entry point the daemon calls, against a
//! real Git repository built here. Verifies page cursors, the final page, the
//! collapse of untracked-only directories, ignored listings, and that a
//! tracked directory's untracked file keeps its full path.
//!
//! Run: `cargo run -p fintwind-core --example git_listing_blackbox`

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::bail;
use fintwind_core::workspace::{self, WorkspaceOperation, WorkspaceResult};
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

fn execute(operation: WorkspaceOperation) -> WorkspaceResult {
    workspace::execute(operation).expect("workspace operation")
}

struct Page {
    entries: Vec<String>,
    next: Option<String>,
    incomplete: bool,
}

fn untracked(cwd: &Path, directory: Option<&str>, after: Option<&str>) -> Page {
    match execute(WorkspaceOperation::ListUntracked {
        cwd: cwd.to_path_buf(),
        directory: directory.map(str::to_owned),
        after: after.map(str::to_owned),
        query_id: None,
    }) {
        WorkspaceResult::UntrackedFiles {
            entries,
            next,
            incomplete,
        } => Page {
            entries: entries.into_iter().map(|entry| entry.path).collect(),
            next,
            incomplete,
        },
        other => panic!("unexpected result {other:?}"),
    }
}

fn ignored(cwd: &Path, directory: Option<&str>, after: Option<&str>) -> Page {
    match execute(WorkspaceOperation::ListIgnored {
        cwd: cwd.to_path_buf(),
        directory: directory.map(str::to_owned),
        after: after.map(str::to_owned),
        query_id: None,
    }) {
        WorkspaceResult::IgnoredFiles {
            entries,
            next,
            incomplete,
        } => Page {
            entries: entries.into_iter().map(|entry| entry.path).collect(),
            next,
            incomplete,
        },
        other => panic!("unexpected result {other:?}"),
    }
}

fn discover(cwd: &Path) -> (Vec<(String, bool)>, bool) {
    match execute(WorkspaceOperation::DiscoverNestedRepositories {
        cwd: cwd.to_path_buf(),
        candidates: Vec::new(),
        query_id: None,
    }) {
        WorkspaceResult::NestedRepositories {
            repositories,
            complete,
        } => (
            repositories
                .into_iter()
                .map(|repository| (repository.relative_path, repository.initialized))
                .collect(),
            complete,
        ),
        other => panic!("unexpected result {other:?}"),
    }
}

fn repository() -> PathBuf {
    let root = std::env::temp_dir().join(format!("fintwind-listing-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create repository");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "core.autocrlf", "false"]);
    write(&root.join("tracked/keep.txt"), "tracked\n");
    write(&root.join("tracked/sub/tracked.txt"), "tracked\n");
    write(
        &root.join(".gitignore"),
        "ignored-dir/\ndeep-ignored/\nforce-tracked/\n",
    );
    write(
        &root.join(".gitmodules"),
        "[submodule \"vendor/lib\"]\n\tpath = vendor/lib\n\turl = https://example.com/lib.git\n",
    );
    write(
        &root.join("force-tracked/keep.log"),
        "tracked despite ignore\n",
    );
    git(&root, &["add", "."]);
    git(&root, &["add", "-f", "force-tracked/keep.log"]);
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
    let root = repository();
    let check = |name: &str, ok: bool| -> anyhow::Result<()> {
        if ok {
            println!("[OK] {name}");
            Ok(())
        } else {
            bail!("{name}")
        }
    };

    // A tracked directory keeps its untracked file at full depth instead of
    // collapsing to a directory row.
    write(&root.join("tracked/sub/loose.txt"), "loose\n");
    // A directory holding only untracked files collapses to one row until it
    // is expanded.
    write(&root.join("pure/deep/file.txt"), "deep\n");
    // Enough direct children to require a second page when expanded.
    for index in 0..600 {
        write(&root.join(format!("bulk/{index:04}.txt")), "x");
    }
    for index in 0..600 {
        write(&root.join(format!("ignored-dir/{index:04}.log")), "x");
    }

    // A nested repository inside a tracked directory is invisible to status.
    let nested = root.join("tracked/nested");
    std::fs::create_dir_all(&nested)?;
    git(&nested, &["init", "-q", "-b", "main"]);
    // A repository inside an ignored directory stays out of the default scan,
    // including one nested below the worktree root.
    let ignored_nested = root.join("ignored-dir/nested");
    std::fs::create_dir_all(&ignored_nested)?;
    git(&ignored_nested, &["init", "-q", "-b", "main"]);
    let deeply_ignored_nested = root.join("tracked/deep-ignored/nested");
    std::fs::create_dir_all(&deeply_ignored_nested)?;
    git(&deeply_ignored_nested, &["init", "-q", "-b", "main"]);

    let root_page = untracked(&root, None, None);
    check(
        "root listing is one complete page",
        root_page.next.is_none() && !root_page.incomplete,
    )?;
    check(
        "collapsed untracked-only directory is one row",
        root_page.entries.iter().any(|path| path == "pure/")
            && root_page.entries.iter().any(|path| path == "bulk/"),
    )?;
    check(
        "tracked directory's untracked file keeps its path",
        root_page
            .entries
            .iter()
            .any(|path| path == "tracked/sub/loose.txt"),
    )?;

    let first = untracked(&root, Some("bulk"), None);
    check(
        "first directory page respects the page budget",
        first.next.is_some() && !first.incomplete && first.entries.len() == 512,
    )?;

    let mut seen = first.entries.clone();
    let mut cursor = first.next.clone();
    let mut pages = 1;
    while let Some(after) = cursor {
        let page = untracked(&root, Some("bulk"), Some(&after));
        check(
            "page starts after its cursor",
            page.entries
                .iter()
                .all(|path| path.as_str() > after.as_str()),
        )?;
        pages += 1;
        seen.extend(page.entries.clone());
        cursor = page.next.clone();
        if pages > 20 {
            bail!("pagination did not terminate");
        }
    }
    check(
        "all directory pages cover every child exactly once",
        pages == 2
            && seen.len() == 600
            && seen.iter().collect::<std::collections::HashSet<_>>().len() == 600
            && seen.iter().any(|path| path == "bulk/0599.txt"),
    )?;

    let inner = untracked(&root, Some("pure"), None);
    check(
        "expanding a collapsed directory lists its child",
        inner.entries == vec!["pure/deep/".to_owned()] && inner.next.is_none(),
    )?;
    let deeper = untracked(&root, Some("pure/deep"), None);
    check(
        "second level lists the file",
        deeper.entries == vec!["pure/deep/file.txt".to_owned()],
    )?;

    let ignored_root = ignored(&root, None, None);
    check(
        "ignored root collapses each ignored directory",
        ignored_root
            .entries
            .iter()
            .any(|path| path == "ignored-dir/")
            && ignored_root.next.is_none()
            && !ignored_root.incomplete,
    )?;
    let mut ignored_seen = Vec::new();
    let mut ignored_cursor = None;
    loop {
        let page = ignored(&root, Some("ignored-dir"), ignored_cursor.as_deref());
        ignored_seen.extend(page.entries.clone());
        match page.next {
            Some(next) => ignored_cursor = Some(next),
            None => {
                check("ignored paging ends complete", !page.incomplete)?;
                break;
            }
        }
    }
    let ignored_logs = ignored_seen
        .iter()
        .filter(|path| path.ends_with(".log"))
        .count();
    check(
        "ignored pages cover the whole ignored directory",
        ignored_logs == 600
            && ignored_seen
                .iter()
                .any(|path| path == "ignored-dir/0599.log"),
    )?;
    check(
        "a tracked file inside an ignored directory stays out of the ignored group",
        {
            let forced = ignored(&root, Some("force-tracked"), None);
            forced.entries.is_empty() && !forced.incomplete
        },
    )?;
    check(
        "expanding an ignored directory lists one layer only",
        ignored_seen
            .iter()
            .all(|path| path.trim_end_matches('/').matches('/').count() == 1),
    )?;
    let one_layer = ignored(&root, Some("tracked/deep-ignored"), None);
    check(
        "an ignored directory below the root expands one layer",
        one_layer.entries == vec!["tracked/deep-ignored/nested/".to_owned()],
    )?;

    let (repositories, complete) = discover(&root);
    check(
        "declared but uninitialized submodule is listed",
        repositories
            .iter()
            .any(|(path, initialized)| path == "vendor/lib" && !initialized),
    )?;
    check(
        "nested repository inside a tracked directory is found",
        repositories
            .iter()
            .any(|(path, initialized)| path == "tracked/nested" && *initialized),
    )?;
    check(
        "ignored directories stay out of the default scan",
        !repositories
            .iter()
            .any(|(path, _)| path == "ignored-dir/nested" || path == "tracked/deep-ignored/nested"),
    )?;
    check("small workspace scan is complete", complete)?;

    let _ = std::fs::remove_dir_all(&root);
    println!("git listing blackbox passed");
    Ok(())
}

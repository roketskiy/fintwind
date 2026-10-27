//! Daemon-owned workspace filesystem and Git API.
//!
//! Paths in this module always name resources on the daemon host. A client
//! may display them, but must never reinterpret them against its own machine.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};

const MAX_HYDRATED_PATCH_BYTES: usize = 32 * 1024 * 1024;
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const MAX_GIT_REQUESTS: usize = 512;

struct GitRequestEntry {
    cancelled: Arc<AtomicBool>,
    started: bool,
    created: Instant,
}

static GIT_REQUESTS: OnceLock<Mutex<std::collections::HashMap<uuid::Uuid, GitRequestEntry>>> =
    OnceLock::new();

fn git_requests() -> &'static Mutex<std::collections::HashMap<uuid::Uuid, GitRequestEntry>> {
    GIT_REQUESTS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn make_git_request_room(
    requests: &mut std::collections::HashMap<uuid::Uuid, GitRequestEntry>,
) -> anyhow::Result<()> {
    requests.retain(|_, entry| entry.started || entry.created.elapsed() < Duration::from_secs(120));
    if requests.len() >= MAX_GIT_REQUESTS {
        let oldest_pending = requests
            .iter()
            .filter(|(_, entry)| !entry.started)
            .min_by_key(|(_, entry)| entry.created)
            .map(|(id, _)| *id);
        if let Some(id) = oldest_pending {
            requests.remove(&id);
        } else {
            bail!("too many active Git queries");
        }
    }
    Ok(())
}

struct GitRequestGuard {
    id: uuid::Uuid,
    cancelled: Arc<AtomicBool>,
}

impl Drop for GitRequestGuard {
    fn drop(&mut self) {
        if let Ok(mut requests) = git_requests().lock()
            && requests
                .get(&self.id)
                .is_some_and(|entry| Arc::ptr_eq(&entry.cancelled, &self.cancelled))
        {
            requests.remove(&self.id);
        }
    }
}

fn start_git_request(id: uuid::Uuid) -> anyhow::Result<GitRequestGuard> {
    let mut requests = git_requests()
        .lock()
        .map_err(|_| anyhow!("Git request registry unavailable"))?;
    let cancelled = if let Some(entry) = requests.get_mut(&id) {
        if entry.started {
            bail!("duplicate Git query");
        }
        entry.started = true;
        entry.cancelled.clone()
    } else {
        make_git_request_room(&mut requests)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        requests.insert(
            id,
            GitRequestEntry {
                cancelled: cancelled.clone(),
                started: true,
                created: Instant::now(),
            },
        );
        cancelled
    };
    Ok(GitRequestGuard { id, cancelled })
}

fn cancel_git_request(id: uuid::Uuid) -> anyhow::Result<()> {
    let mut requests = git_requests()
        .lock()
        .map_err(|_| anyhow!("Git request registry unavailable"))?;
    if let Some(entry) = requests.get(&id) {
        entry.cancelled.store(true, Ordering::Release);
    } else {
        make_git_request_room(&mut requests)?;
        requests.insert(
            id,
            GitRequestEntry {
                cancelled: Arc::new(AtomicBool::new(true)),
                started: false,
                created: Instant::now(),
            },
        );
    }
    Ok(())
}

fn with_git_request<T>(
    query_id: Option<uuid::Uuid>,
    run: impl FnOnce(Option<&AtomicBool>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let request = query_id.map(start_git_request).transpose()?;
    let cancelled = request.as_ref().map(|request| request.cancelled.as_ref());
    check_git_cancelled(cancelled)?;
    run(cancelled)
}

pub(crate) fn check_git_cancelled(cancelled: Option<&AtomicBool>) -> anyhow::Result<()> {
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
        bail!("Git query cancelled");
    }
    Ok(())
}

pub use fintwind_protocol::workspace::{
    ReviewDiffData, ReviewDiffFile, ReviewDiffSource, WorkingTreeEntry, WorkspaceOperation,
    WorkspaceResult,
};

pub fn execute(operation: WorkspaceOperation) -> anyhow::Result<WorkspaceResult> {
    Ok(match operation {
        WorkspaceOperation::ListTree {
            root,
            expanded_paths,
        } => WorkspaceResult::WorkingTree {
            entries: list_tree(&root, &expanded_paths.into_iter().collect()),
        },
        WorkspaceOperation::BrowseDirectory { path } => {
            let home = dirs::home_dir().ok_or_else(|| anyhow!("home directory is unavailable"))?;
            let path = fs::canonicalize(path.as_deref().unwrap_or(&home)).with_context(|| {
                format!(
                    "could not open directory {}",
                    path.as_deref().unwrap_or(&home).display()
                )
            })?;
            if !fs::metadata(&path)?.is_dir() {
                bail!("not a directory: {}", path.display());
            }
            let filesystem_root = path
                .ancestors()
                .last()
                .map(Path::to_owned)
                .unwrap_or_else(|| path.clone());
            WorkspaceResult::Directory {
                parent: path.parent().map(Path::to_owned),
                entries: list_directory(&path)?,
                path,
                home,
                filesystem_root,
            }
        }
        WorkspaceOperation::ReadTextFile {
            root,
            relative_path,
        } => WorkspaceResult::TextFile {
            content: fs::read_to_string(resolve_workspace_path(&root, &relative_path)?)?,
        },
        WorkspaceOperation::WriteTextFile {
            root,
            relative_path,
            content,
        } => {
            fs::write(resolve_workspace_path(&root, &relative_path)?, content)?;
            WorkspaceResult::Ack
        }
        WorkspaceOperation::ListProjectFiles { root, cap } => WorkspaceResult::ProjectFiles {
            entries: crate::composer_complete::list_project_files(&root, cap),
        },
        WorkspaceOperation::DiscoverSlashCommands {
            project_root,
            binary,
        } => {
            let mut commands = crate::composer_complete::discover_slash_commands(&project_root);
            // The server's registry names config and plugin commands no
            // filesystem scan can see; its entries never carry a template, so
            // a discovered one always keeps its own.
            if let Some(binary) = binary {
                let reported = crate::composer_complete::server_commands(&binary, &project_root);
                commands = crate::composer_complete::merge_reported_commands(&commands, &reported);
                if let Ok(skills) = crate::skill_prompt::discover_commands(&binary, &project_root) {
                    // Filesystem discovery cannot reproduce V2 IDs, config
                    // sources, overrides or built-in skills. The live registry
                    // is authoritative; retain only non-skill local commands.
                    commands.retain(|command| {
                        command.scope != crate::composer_complete::CommandScope::Skill
                    });
                    commands.extend(skills);
                    commands.sort_by(|a, b| {
                        (a.scope.display_rank(), &a.name).cmp(&(b.scope.display_rank(), &b.name))
                    });
                }
            }
            WorkspaceResult::SlashCommands { commands }
        }
        WorkspaceOperation::CreateProjectlessWorkspace { prompt } => {
            WorkspaceResult::ProjectlessWorkspace {
                cwd: crate::projectless::create_workspace(prompt.as_deref())?.cwd,
            }
        }
        WorkspaceOperation::MigrateProjectlessWorkspace { path } => {
            WorkspaceResult::ProjectlessWorkspace {
                cwd: crate::projectless::migrate_workspace(&path)?.cwd,
            }
        }
        WorkspaceOperation::InspectBranches { cwd } => WorkspaceResult::Branches {
            snapshot: crate::git_branch::inspect(&cwd)?,
        },
        WorkspaceOperation::InspectBranchRefs { cwd } => WorkspaceResult::Branches {
            snapshot: crate::git_branch::inspect_refs(&cwd)?,
        },
        WorkspaceOperation::InspectPullRequests { cwd } => WorkspaceResult::PullRequests {
            snapshot: crate::pull_requests::inspect(&cwd)?,
        },
        WorkspaceOperation::ListCommits { cwd, limit, branch } => WorkspaceResult::Commits {
            commits: crate::git_log::list(&cwd, limit, branch.as_deref())?,
        },
        WorkspaceOperation::ReadCommitStats { cwd, hash } => {
            let key = crate::git_cache::CacheKey::CommitStats {
                worktree: crate::git_cache::worktree_identity(&cwd),
                hash: hash.clone(),
            };
            if let Some(cached) = crate::git_cache::get(&key, Duration::from_secs(600))
                && let crate::git_cache::Cached::CommitStats(stats) = cached.as_ref()
            {
                return Ok(WorkspaceResult::CommitStats {
                    stats: stats.clone(),
                });
            }
            let stats = crate::git_log::stats(&cwd, &hash)?;
            crate::git_cache::insert(key, crate::git_cache::Cached::CommitStats(stats.clone()));
            WorkspaceResult::CommitStats { stats }
        }
        WorkspaceOperation::CurrentBranch { cwd } => WorkspaceResult::CurrentBranch {
            branch: crate::git_branch::current(&cwd)?,
        },
        WorkspaceOperation::CheckoutBranch {
            cwd,
            branch,
            create,
        } => WorkspaceResult::BranchChanged {
            snapshot: if create {
                crate::git_branch::create_and_checkout(&cwd, &branch)?
            } else {
                crate::git_branch::checkout(&cwd, &branch)?
            },
        },
        WorkspaceOperation::CreateWorktree {
            project_path,
            project_id,
            session_id,
            prompt,
            base_branch,
        } => WorkspaceResult::WorktreeCreated {
            worktree: crate::worktree::create(
                &project_path,
                project_id,
                session_id,
                &prompt,
                base_branch.as_deref(),
            )?,
        },
        WorkspaceOperation::InspectCommit { cwd } => WorkspaceResult::CommitSnapshot {
            snapshot: crate::git_commit::inspect(&cwd)?,
        },
        WorkspaceOperation::InspectStatus { cwd } => WorkspaceResult::WorktreeStatus {
            status: crate::git_status::inspect(&cwd)?,
        },
        WorkspaceOperation::InspectTrackedStatus { cwd, query_id } => {
            WorkspaceResult::WorktreeStatus {
                status: with_git_request(query_id, |cancelled| {
                    crate::git_status::inspect_tracked(&cwd, cancelled)
                })?,
            }
        }
        WorkspaceOperation::ListUntracked {
            cwd,
            directory,
            after,
            query_id,
        } => {
            let page = with_git_request(query_id, |cancelled| {
                list_untracked(&cwd, directory.as_deref(), after.as_deref(), cancelled)
            })?;
            WorkspaceResult::UntrackedFiles {
                entries: page.entries,
                next: page.next,
                incomplete: page.incomplete,
            }
        }
        WorkspaceOperation::ListIgnored {
            cwd,
            directory,
            after,
            query_id,
        } => {
            let page = with_git_request(query_id, |cancelled| {
                list_ignored(&cwd, directory.as_deref(), after.as_deref(), cancelled)
            })?;
            WorkspaceResult::IgnoredFiles {
                entries: page.entries,
                next: page.next,
                incomplete: page.incomplete,
            }
        }
        WorkspaceOperation::DiscoverNestedRepositories {
            cwd,
            candidates,
            query_id,
        } => {
            let discovery = with_git_request(query_id, |cancelled| {
                discover_nested_repositories(&cwd, &candidates, cancelled)
            })?;
            WorkspaceResult::NestedRepositories {
                repositories: discovery.repositories,
                complete: discovery.complete,
            }
        }
        WorkspaceOperation::StagePaths { cwd, paths } => {
            crate::git_actions::stage(&cwd, &paths)?;
            WorkspaceResult::Ack
        }
        WorkspaceOperation::UnstagePaths { cwd, paths } => {
            crate::git_actions::unstage(&cwd, &paths)?;
            WorkspaceResult::Ack
        }
        WorkspaceOperation::DiscardWorktreePaths {
            cwd,
            tracked,
            untracked,
        } => {
            crate::git_actions::discard_worktree(&cwd, &tracked, &untracked)?;
            WorkspaceResult::Ack
        }
        WorkspaceOperation::FetchRemote { cwd } => {
            crate::git_actions::fetch(&cwd)?;
            WorkspaceResult::Ack
        }
        WorkspaceOperation::PullFastForward { cwd } => {
            crate::git_actions::pull_fast_forward(&cwd)?;
            WorkspaceResult::Ack
        }
        WorkspaceOperation::GenerateCommitMessage {
            cwd,
            include_unstaged,
            invocation,
        } => WorkspaceResult::CommitMessage {
            message: crate::git_commit::generate_message(&cwd, include_unstaged, &invocation)?,
        },
        WorkspaceOperation::Commit {
            cwd,
            message,
            include_unstaged,
            push,
        } => {
            crate::git_commit::commit(&cwd, &message, include_unstaged)?;
            if push {
                crate::git_commit::push(&cwd)?;
            }
            WorkspaceResult::Ack
        }
        WorkspaceOperation::Push { cwd } => {
            crate::git_commit::push(&cwd)?;
            WorkspaceResult::Ack
        }
        WorkspaceOperation::CaptureTurnStart {
            cwd,
            session_id,
            turn_count,
        } => {
            crate::checkpoint::capture_turn_start(&cwd, session_id, turn_count)?;
            WorkspaceResult::Ack
        }
        WorkspaceOperation::CaptureTurn {
            cwd,
            session_id,
            turn_count,
        } => WorkspaceResult::Checkpoint {
            checkpoint: crate::checkpoint::capture_turn(&cwd, session_id, turn_count)?,
        },
        WorkspaceOperation::CaptureRef { cwd, git_ref } => {
            crate::checkpoint::capture_ref(&cwd, &git_ref)?;
            WorkspaceResult::Ack
        }
        WorkspaceOperation::RestoreRef { cwd, git_ref } => {
            crate::checkpoint::restore_ref(&cwd, &git_ref)?;
            crate::git_cache::invalidate_worktree(&cwd);
            WorkspaceResult::Ack
        }
        WorkspaceOperation::HasRef { cwd, git_ref } => WorkspaceResult::Bool {
            value: crate::checkpoint::has_ref(&cwd, &git_ref),
        },
        WorkspaceOperation::SessionTurnRefs { cwd, session_id } => {
            let mut turn_counts = crate::checkpoint::session_turn_refs(&cwd, session_id)
                .into_iter()
                .collect::<Vec<_>>();
            turn_counts.sort_unstable();
            WorkspaceResult::TurnRefs { turn_counts }
        }
        WorkspaceOperation::DeleteRef { cwd, git_ref } => {
            crate::checkpoint::delete_ref(&cwd, &git_ref)?;
            crate::git_cache::invalidate_worktree(&cwd);
            WorkspaceResult::Ack
        }
        WorkspaceOperation::DeleteTurnRefsAfter {
            cwd,
            session_id,
            retained_turn_count,
            previous_turn_count,
        } => {
            crate::checkpoint::delete_turn_refs_after(
                &cwd,
                session_id,
                retained_turn_count,
                previous_turn_count,
            )?;
            crate::git_cache::invalidate_worktree(&cwd);
            WorkspaceResult::Ack
        }
        WorkspaceOperation::DeleteSessionRefs { cwd, session_id } => {
            crate::checkpoint::delete_all_session_refs(&cwd, session_id)?;
            crate::git_cache::invalidate_worktree(&cwd);
            WorkspaceResult::Ack
        }
        WorkspaceOperation::CopySessionRefs {
            cwd,
            source_session_id,
            target_session_id,
            through_turn_count,
        } => {
            crate::checkpoint::copy_session_refs(
                &cwd,
                source_session_id,
                target_session_id,
                through_turn_count,
            )?;
            WorkspaceResult::Ack
        }
        WorkspaceOperation::CollectReviewDiff { cwd, source } => {
            // A completed turn's refs are fixed, so its full diff is worth
            // keeping; a live source is not cached at all.
            let key = matches!(source, ReviewDiffSource::LastTurn { .. }).then(|| {
                crate::git_cache::CacheKey::TurnDiff {
                    worktree: crate::git_cache::worktree_identity(&cwd),
                    source: serde_json::to_string(&source).unwrap_or_default(),
                }
            });
            if let Some(key) = key.as_ref()
                && let Some(cached) = crate::git_cache::get(key, Duration::from_secs(300))
                && let crate::git_cache::Cached::ReviewDiff(data) = cached.as_ref()
            {
                return Ok(WorkspaceResult::ReviewDiff { data: data.clone() });
            }
            let data = collect_review_diff(&cwd, source)?;
            if let Some(key) = key {
                crate::git_cache::insert(key, crate::git_cache::Cached::ReviewDiff(data.clone()));
            }
            WorkspaceResult::ReviewDiff { data }
        }
        WorkspaceOperation::CollectReviewFileDiff {
            cwd,
            source,
            path,
            origin_path,
            untracked,
            context_lines,
            query_id,
        } => WorkspaceResult::ReviewDiff {
            data: collect_review_file_diff(
                &cwd,
                source,
                &path,
                origin_path.as_deref(),
                untracked,
                context_lines,
                query_id,
            )?,
        },
        WorkspaceOperation::CancelGitQuery { query_id } => {
            cancel_git_request(query_id)?;
            WorkspaceResult::Ack
        }
    })
}

fn resolve_workspace_path(root: &Path, relative: &Path) -> anyhow::Result<PathBuf> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("workspace path must be a non-empty relative path");
    }
    Ok(root.join(relative))
}

fn list_tree(root: &Path, expanded_paths: &HashSet<PathBuf>) -> Vec<WorkingTreeEntry> {
    fn visit(
        directory: &Path,
        relative_directory: &Path,
        depth: usize,
        expanded_paths: &HashSet<PathBuf>,
        output: &mut Vec<WorkingTreeEntry>,
    ) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        let mut children = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".git" {
                    return None;
                }
                let is_dir = entry.file_type().ok()?.is_dir();
                Some((entry.path(), name, is_dir))
            })
            .collect::<Vec<_>>();
        children.sort_by_key(|(_, name, is_dir)| (!*is_dir, name.to_lowercase()));
        for (absolute_path, name, is_dir) in children {
            let relative_path = relative_directory.join(&name);
            let expanded = is_dir && expanded_paths.contains(&absolute_path);
            output.push(WorkingTreeEntry {
                relative_path: relative_path.to_string_lossy().into_owned(),
                absolute_path: absolute_path.clone(),
                name,
                is_dir,
                expanded,
                depth,
            });
            if expanded {
                visit(
                    &absolute_path,
                    &relative_path,
                    depth + 1,
                    expanded_paths,
                    output,
                );
            }
        }
    }
    let mut output = Vec::new();
    visit(root, Path::new(""), 0, expanded_paths, &mut output);
    output
}

fn list_directory(directory: &Path) -> anyhow::Result<Vec<WorkingTreeEntry>> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("could not read directory {}", directory.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = fs::metadata(entry.path()).ok()?.is_dir();
            Some(WorkingTreeEntry {
                relative_path: name.clone(),
                absolute_path: entry.path(),
                name,
                is_dir,
                expanded: false,
                depth: 0,
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
    Ok(entries)
}

#[derive(Clone, Debug)]
struct DiffRange {
    from: String,
    to: String,
}

fn collect_review_diff(cwd: &Path, source: ReviewDiffSource) -> anyhow::Result<ReviewDiffData> {
    ensure_repository(cwd)?;
    let range = resolve_diff_range(cwd, source)?;
    let numstat = diff_output(cwd, &range, &["--numstat"])?;
    let hydrated = diff_output(cwd, &range, &["--unified=2147483647"])?;
    let (patch, complete_context) = if hydrated.len() <= MAX_HYDRATED_PATCH_BYTES {
        (hydrated, true)
    } else {
        (diff_output(cwd, &range, &["--unified=3"])?, false)
    };
    Ok(ReviewDiffData {
        source,
        numstat,
        patch,
        complete_context,
        truncated: !complete_context,
        file: None,
    })
}

const MAX_DISCOVERY_DIRECTORIES: usize = 4_000;
const MAX_DISCOVERY_REPOSITORIES: usize = 256;
const MAX_DISCOVERY_DEPTH: usize = 32;
const MAX_DISCOVERY_DURATION: Duration = Duration::from_secs(10);

struct NestedRepositoryDiscovery {
    repositories: Vec<fintwind_protocol::git::NestedRepository>,
    /// False when a budget or an unreadable directory left the scan partial.
    complete: bool,
}

fn discover_nested_repositories(
    cwd: &Path,
    candidates: &[String],
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<NestedRepositoryDiscovery> {
    let root = git_query_root(cwd, cancelled, false)?;
    let canonical_root = fs::canonicalize(&root)?;
    let mut paths = std::collections::BTreeSet::new();
    let modules = root.join(".gitmodules");
    if modules.is_file() {
        let mut command = crate::command_env::plain_command("git");
        command.current_dir(&root).args([
            "config",
            "--null",
            "--file",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.path$",
        ]);
        let output = bounded_git_output_cancellable(&mut command, true, cancelled, false)?;
        for record in String::from_utf8_lossy(&output).split('\0') {
            if let Some((_, path)) = record.split_once('\n') {
                paths.insert(path.trim_end_matches('/').to_owned());
            }
        }
    }
    for candidate in candidates {
        check_git_cancelled(cancelled)?;
        let candidate = candidate.trim_end_matches('/');
        if let Ok(path) = resolve_workspace_path(&root, Path::new(candidate))
            && path.join(".git").exists()
        {
            paths.insert(candidate.to_owned());
        }
    }

    // A bounded breadth-first walk finds repositories Git's status cannot
    // report: those nested inside tracked directories. Ignored directories are
    // skipped by default, as are links and reparse points, so a loop cannot
    // pull the scan out of the worktree.
    let mut complete = true;
    let started = Instant::now();
    let mut visited = 0;
    let mut queue = std::collections::VecDeque::from([(root.clone(), 0_usize)]);
    'scan: while let Some((directory, depth)) = queue.pop_front() {
        check_git_cancelled(cancelled)?;
        if visited >= MAX_DISCOVERY_DIRECTORIES || started.elapsed() >= MAX_DISCOVERY_DURATION {
            complete = false;
            break;
        }
        visited += 1;
        if depth >= MAX_DISCOVERY_DEPTH {
            complete = false;
            continue;
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                complete = false;
                continue;
            }
        };
        let mut children = Vec::new();
        for entry in entries {
            check_git_cancelled(cancelled)?;
            let Ok(entry) = entry else {
                complete = false;
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                complete = false;
                continue;
            };
            if file_type.is_symlink() || is_reparse_point(&entry.path()) {
                continue;
            }
            if !file_type.is_dir() || entry.file_name() == ".git" {
                continue;
            }
            children.push(entry.path());
        }
        let ignored = ignored_directories(&root, &children, cancelled)?;
        for child in children {
            let relative = child
                .strip_prefix(&root)
                .unwrap_or(&child)
                .to_string_lossy()
                .replace('\\', "/");
            if ignored.contains(&relative) {
                continue;
            }
            if child.join(".git").exists() {
                if paths.len() >= MAX_DISCOVERY_REPOSITORIES {
                    complete = false;
                    break 'scan;
                }
                paths.insert(relative);
                continue;
            }
            queue.push_back((child, depth + 1));
        }
    }
    if paths.len() > MAX_DISCOVERY_REPOSITORIES {
        complete = false;
    }

    let mut repositories = Vec::new();
    for relative_path in paths.into_iter().take(MAX_DISCOVERY_REPOSITORIES) {
        check_git_cancelled(cancelled)?;
        let Ok(path) = resolve_workspace_path(&root, Path::new(&relative_path)) else {
            continue;
        };
        let initialized = if path.exists() {
            let canonical = fs::canonicalize(&path)?;
            if !canonical.starts_with(&canonical_root) {
                continue;
            }
            path.join(".git").exists()
        } else {
            false
        };
        repositories.push(fintwind_protocol::git::NestedRepository {
            path,
            relative_path,
            initialized,
        });
    }
    Ok(NestedRepositoryDiscovery {
        repositories,
        complete,
    })
}

/// Marks junctions as well as symlinks. A junction is a directory to Rust but
/// still a link that can lead outside the worktree or form a cycle.
fn is_reparse_point(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        fs::symlink_metadata(path)
            .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            .unwrap_or(true)
    }
    #[cfg(not(windows))]
    {
        fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(true)
    }
}

/// One `git check-ignore` call per batch of relative paths. Git exits 1 when
/// nothing matches, which is an empty answer rather than a failure.
fn ignored_directories(
    root: &Path,
    directories: &[PathBuf],
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<std::collections::HashSet<String>> {
    let paths = directories
        .iter()
        .filter_map(|directory| directory.strip_prefix(root).ok())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect::<Vec<_>>();
    check_ignore_paths(root, &paths, cancelled, false)
}

/// Reports which of these repository-relative paths match an ignore rule.
fn check_ignore_paths(
    root: &Path,
    paths: &[String],
    cancelled: Option<&AtomicBool>,
    interactive: bool,
) -> anyhow::Result<std::collections::HashSet<String>> {
    let mut ignored = std::collections::HashSet::new();
    let mut start = 0;
    while start < paths.len() {
        check_git_cancelled(cancelled)?;
        let mut end = start;
        let mut units = 0;
        while end < paths.len() {
            let next = paths[end].encode_utf16().count() + 3;
            if end > start && units + next > MAX_DIRECTORY_ARGUMENT_UNITS {
                break;
            }
            units += next;
            end += 1;
        }
        let mut input = Vec::new();
        for path in &paths[start..end] {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        let mut command = crate::command_env::plain_command("git");
        command
            .current_dir(root)
            .args(["check-ignore", "-z", "--stdin"]);
        let output = bounded_git_output_with_stdin(&mut command, &input, cancelled, interactive)?;
        for path in String::from_utf8_lossy(&output)
            .split('\0')
            .filter(|path| !path.is_empty())
        {
            ignored.insert(path.replace('\\', "/"));
        }
        start = end;
    }
    Ok(ignored)
}

/// One page of an untracked or ignored directory listing.
struct DirectoryPage {
    entries: Vec<fintwind_protocol::git::WorktreeStatusEntry>,
    /// Last direct child this page read. `Some` means the client can ask for
    /// the next page after it.
    next: Option<String>,
    /// The listing could not finish inside its budget; narrowing the directory
    /// is the only way to see the rest.
    incomplete: bool,
}

/// Direct children read per page. A page stays small enough that one Git call
/// returns it well inside the output budget.
const MAX_DIRECTORY_CHILDREN_PER_PAGE: usize = 512;
/// A single directory is enumerated at most this deep. Beyond it the listing
/// reports itself incomplete rather than growing without bound.
const MAX_DIRECTORY_CHILDREN: usize = 20_000;
const MAX_DIRECTORY_ARGUMENT_UNITS: usize = 8_192;
/// Empty pages are skipped by reading further batches, so a page is only
/// returned without entries when the directory really has none left.
const MAX_EMPTY_PAGE_ROUNDS: usize = 8;

fn list_untracked(
    cwd: &Path,
    directory: Option<&str>,
    after: Option<&str>,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<DirectoryPage> {
    let root = git_query_root(cwd, cancelled, true)?;
    let canonical_root = fs::canonicalize(&root)?;
    let mut boundaries = std::collections::HashMap::new();
    if let Some(directory) = directory {
        let target = resolve_workspace_path(&root, Path::new(directory))?;
        if !fs::canonicalize(&target)?.starts_with(&canonical_root) {
            bail!("the directory is outside the worktree");
        }
        if untracked_repository_boundary(&root, &canonical_root, directory, true, &mut boundaries)?
            .is_some()
        {
            bail!("the directory is a separate Git repository");
        }
    }
    let page = list_git_directory(&root, directory, false, after, cancelled)?;
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::new();
    for path in page.paths {
        check_git_cancelled(cancelled)?;
        let path = match untracked_repository_boundary(
            &root,
            &canonical_root,
            &path,
            path.ends_with('/'),
            &mut boundaries,
        )? {
            Some(repository) => format!("{repository}/"),
            None => path,
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        entries.push(fintwind_protocol::git::WorktreeStatusEntry {
            path,
            origin_path: None,
            index_status: '?',
            worktree_status: '?',
        });
    }
    Ok(DirectoryPage {
        entries,
        next: page.next,
        incomplete: page.incomplete,
    })
}

fn git_query_root(
    cwd: &Path,
    cancelled: Option<&AtomicBool>,
    interactive: bool,
) -> anyhow::Result<PathBuf> {
    let mut command = crate::command_env::plain_command("git");
    command
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd);
    let output = bounded_git_output_cancellable(&mut command, false, cancelled, interactive)?;
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output).trim_end_matches(['\r', '\n']),
    ))
}

struct GitDirectoryPage {
    paths: Vec<String>,
    next: Option<String>,
    incomplete: bool,
}

fn list_git_directory(
    root: &Path,
    directory: Option<&str>,
    ignored: bool,
    after: Option<&str>,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<GitDirectoryPage> {
    let (children, complete) = directory_children(root, directory, cancelled)?;
    let prefix = directory.map_or_else(String::new, |directory| format!("{directory}/"));
    let mut cursor = match after {
        Some(after) => children.partition_point(|child| child.as_str() <= after),
        None => 0,
    };
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut truncated = false;
    let mut rounds = 0;
    while cursor < children.len() {
        check_git_cancelled(cancelled)?;
        let end = (cursor + MAX_DIRECTORY_CHILDREN_PER_PAGE).min(children.len());
        let (batch, complete_output) =
            list_git_children(root, &children[cursor..end], &prefix, ignored, cancelled)?;
        for path in batch {
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
        cursor = end;
        rounds += 1;
        if !complete_output {
            // A partial batch cannot name a safe cursor, since children inside
            // it were never read. Report the truncation instead of skipping.
            return Ok(GitDirectoryPage {
                paths,
                next: None,
                incomplete: true,
            });
        }
        if !paths.is_empty() || rounds >= MAX_EMPTY_PAGE_ROUNDS {
            break;
        }
    }
    let next = (cursor < children.len()).then(|| children[cursor - 1].clone());
    if next.is_none() && !complete {
        truncated = true;
    }
    Ok(GitDirectoryPage {
        paths,
        next,
        incomplete: truncated,
    })
}

/// The direct children of one directory, sorted so a page cursor stays stable
/// across requests.
fn directory_children(
    root: &Path,
    directory: Option<&str>,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<(Vec<String>, bool)> {
    let (target, prefix) = match directory {
        Some(directory) => (
            resolve_workspace_path(root, Path::new(directory))?,
            format!("{directory}/"),
        ),
        None => (root.to_path_buf(), String::new()),
    };
    let mut children = Vec::new();
    let mut complete = true;
    let entries = fs::read_dir(&target)
        .with_context(|| format!("could not read directory {}", target.display()))?;
    for entry in entries {
        check_git_cancelled(cancelled)?;
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().context("a file name is not valid Unicode")?;
        children.push(format!("{prefix}{name}"));
        if children.len() > MAX_DIRECTORY_CHILDREN {
            complete = false;
            break;
        }
    }
    children.sort_unstable();
    children.truncate(MAX_DIRECTORY_CHILDREN);
    Ok((children, complete))
}

/// Asks Git which of these children are untracked (or ignored). Git collapses
/// a directory that holds nothing but such entries to a trailing-slash path,
/// which is exactly the row the tree shows.
fn list_git_children(
    root: &Path,
    children: &[String],
    prefix: &str,
    ignored: bool,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<(Vec<String>, bool)> {
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut start = 0;
    while start < children.len() {
        check_git_cancelled(cancelled)?;
        let mut end = start;
        let mut units = 0;
        while end < children.len() {
            let next = children[end].encode_utf16().count() + 3;
            if end > start && units + next > MAX_DIRECTORY_ARGUMENT_UNITS {
                break;
            }
            units += next;
            end += 1;
        }
        let mut command = crate::command_env::plain_command("git");
        command.current_dir(root).args([
            "--no-pager",
            "--literal-pathspecs",
            "-c",
            "core.quotePath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
            "--directory",
            "--no-empty-directory",
            "-z",
        ]);
        if ignored {
            command.arg("--ignored");
        }
        command.arg("--").args(&children[start..end]);
        let (output, complete) = bounded_git_output_partial(&mut command, cancelled)?;
        let text = String::from_utf8_lossy(&output);
        let mut records = text
            .split('\0')
            .filter(|path| !path.is_empty())
            .collect::<Vec<_>>();
        if !complete && !output.ends_with(b"\0") {
            // A size-capped read can stop mid-record; that record names no
            // path this request may hand back.
            records.pop();
        }
        for path in records {
            if !path.starts_with(prefix) {
                bail!("Git returned a path outside the requested directory");
            }
            if seen.insert(path.to_owned()) {
                paths.push(path.to_owned());
            }
        }
        if !complete {
            return Ok((paths, false));
        }
        start = end;
    }
    Ok((paths, true))
}

fn untracked_repository_boundary(
    root: &Path,
    canonical_root: &Path,
    path: &str,
    include_last: bool,
    cache: &mut std::collections::HashMap<PathBuf, bool>,
) -> anyhow::Result<Option<String>> {
    use std::path::Component;

    let components: Vec<_> = Path::new(path).components().collect();
    let count = components.len().saturating_sub(usize::from(!include_last));
    let mut relative = PathBuf::new();
    for component in components.into_iter().take(count) {
        let Component::Normal(part) = component else {
            bail!("invalid untracked path");
        };
        relative.push(part);
        let is_repository = if let Some(is_repository) = cache.get(&relative) {
            *is_repository
        } else {
            let absolute = root.join(&relative);
            if !fs::canonicalize(&absolute)?.starts_with(canonical_root) {
                bail!("an untracked directory is outside the worktree");
            }
            let is_repository = absolute.join(".git").try_exists()?;
            cache.insert(relative.clone(), is_repository);
            is_repository
        };
        if is_repository {
            return Ok(Some(relative.to_string_lossy().replace('\\', "/")));
        }
    }
    Ok(None)
}

fn list_ignored(
    cwd: &Path,
    directory: Option<&str>,
    after: Option<&str>,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<DirectoryPage> {
    let root = git_query_root(cwd, cancelled, true)?;
    let canonical_root = fs::canonicalize(&root)?;
    let Some(directory) = directory else {
        let page = list_git_directory(&root, None, true, after, cancelled)?;
        return Ok(DirectoryPage {
            entries: page.paths.into_iter().map(ignored_entry).collect(),
            next: page.next,
            incomplete: page.incomplete,
        });
    };
    let target = resolve_workspace_path(&root, Path::new(directory))?;
    if !fs::canonicalize(&target)?.starts_with(&canonical_root) {
        bail!("the directory is outside the worktree");
    }
    if untracked_repository_boundary(
        &root,
        &canonical_root,
        directory,
        true,
        &mut std::collections::HashMap::new(),
    )?
    .is_some()
    {
        bail!("the directory is a separate Git repository");
    }
    // Git reports an ignored directory as one row even when asked for children
    // inside it, so one layer of children comes from the directory itself and
    // from check-ignore, never from a recursive file walk.
    let (children, complete) = directory_children(&root, Some(directory), cancelled)?;
    let start = match after {
        Some(after) => children.partition_point(|child| child.as_str() <= after),
        None => 0,
    };
    let end = (start + MAX_DIRECTORY_CHILDREN_PER_PAGE).min(children.len());
    let batch = &children[start..end];
    let ignored = check_ignore_paths(&root, batch, cancelled, true)?;
    let tracked = tracked_children(&root, directory, batch, cancelled)?;
    let mut entries = Vec::new();
    for child in batch {
        if !ignored.contains(child) || tracked.contains(child) {
            continue;
        }
        let is_directory = fs::symlink_metadata(root.join(child))
            .map(|metadata| metadata.file_type().is_dir())
            .unwrap_or(false);
        entries.push(ignored_entry(if is_directory {
            format!("{child}/")
        } else {
            child.clone()
        }));
    }
    let next = (end < children.len()).then(|| children[end - 1].clone());
    let incomplete = next.is_none() && !complete;
    Ok(DirectoryPage {
        entries,
        next,
        incomplete,
    })
}

fn ignored_entry(path: String) -> fintwind_protocol::git::WorktreeStatusEntry {
    fintwind_protocol::git::WorktreeStatusEntry {
        path,
        origin_path: None,
        index_status: '!',
        worktree_status: '!',
    }
}

/// The direct children of `directory` that Git tracks. A tracked file stays
/// visible even when its parent directory matches an ignore rule.
fn tracked_children(
    root: &Path,
    directory: &str,
    children: &[String],
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<std::collections::HashSet<String>> {
    let mut tracked = std::collections::HashSet::new();
    if children.is_empty() {
        return Ok(tracked);
    }
    let prefix = format!("{directory}/");
    let mut command = crate::command_env::plain_command("git");
    command.current_dir(root).args([
        "--no-pager",
        "--literal-pathspecs",
        "-c",
        "core.quotePath=false",
        "ls-files",
        "-z",
        "--",
    ]);
    command.args(children);
    let output = bounded_git_output_cancellable(&mut command, false, cancelled, true)?;
    for path in String::from_utf8_lossy(&output)
        .split('\0')
        .filter(|path| !path.is_empty())
    {
        let Some(rest) = path.strip_prefix(&prefix) else {
            continue;
        };
        match rest.split_once('/') {
            Some((name, _)) => {
                tracked.insert(format!("{prefix}{name}"));
            }
            None => {
                tracked.insert(path.to_owned());
            }
        }
    }
    Ok(tracked)
}

fn collect_review_file_diff(
    cwd: &Path,
    source: ReviewDiffSource,
    path: &str,
    origin_path: Option<&str>,
    untracked: bool,
    context_lines: u32,
    query_id: Option<uuid::Uuid>,
) -> anyhow::Result<ReviewDiffData> {
    let request = query_id.map(start_git_request).transpose()?;
    let cancelled = request.as_ref().map(|request| request.cancelled.as_ref());
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
        bail!("Git query cancelled");
    }
    if !matches!(
        source,
        ReviewDiffSource::Uncommitted | ReviewDiffSource::Unstaged | ReviewDiffSource::Staged
    ) {
        bail!("a single-file worktree diff requires a live source");
    }
    let root =
        PathBuf::from(git(cwd, ["rev-parse", "--show-toplevel"])?.trim_end_matches(['\r', '\n']));
    let file = resolve_workspace_path(&root, Path::new(path))?;
    if let Some(origin) = origin_path {
        resolve_workspace_path(&root, Path::new(origin))?;
    }
    let head = (source == ReviewDiffSource::Uncommitted)
        .then(|| resolve(&root, "HEAD"))
        .flatten();
    let unborn_worktree = source == ReviewDiffSource::Uncommitted && head.is_none();
    let file_exists = match fs::symlink_metadata(&file) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("could not inspect worktree file"),
    };
    if unborn_worktree && !file_exists {
        return Ok(ReviewDiffData {
            source,
            numstat: String::new(),
            patch: String::new(),
            complete_context: false,
            truncated: false,
            file: Some(ReviewDiffFile {
                path: path.to_owned(),
                additions: 0,
                deletions: 0,
                binary: false,
            }),
        });
    }
    let no_index = (untracked && source != ReviewDiffSource::Staged) || unborn_worktree;
    if no_index {
        let root = fs::canonicalize(&root)?;
        let target = fs::canonicalize(&file).context("could not open untracked file")?;
        if !target.starts_with(&root) {
            bail!("the file is outside the worktree");
        }
    }

    let run = |mode: &[&str]| -> anyhow::Result<Vec<u8>> {
        let mut command = crate::command_env::plain_command("git");
        command
            .args([
                "--no-pager",
                "--literal-pathspecs",
                "-c",
                "core.quotePath=false",
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--find-renames",
            ])
            .args(mode)
            .current_dir(&root);
        if no_index {
            command.args(["--no-index", "--", "/dev/null", path]);
        } else {
            match source {
                ReviewDiffSource::Staged => {
                    command.arg("--cached");
                }
                ReviewDiffSource::Uncommitted => {
                    command.arg(head.as_deref().expect("resolved HEAD for worktree diff"));
                }
                _ => {}
            }
            command.arg("--");
            if let Some(origin) = origin_path {
                command.arg(origin);
            }
            command.arg(path);
        }
        bounded_git_output_cancellable(&mut command, no_index, cancelled, true)
    };

    let numstat = run(&["--numstat", "-z"])?;
    let mut columns = numstat
        .split(|byte| *byte == 0)
        .next()
        .unwrap_or_default()
        .splitn(3, |byte| *byte == b'\t');
    let additions = columns.next().unwrap_or_default();
    let deletions = columns.next().unwrap_or_default();
    let number = |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    };
    let file = ReviewDiffFile {
        path: path.to_owned(),
        additions: number(additions),
        deletions: number(deletions),
        binary: additions == b"-" || deletions == b"-",
    };
    let context = format!("--unified={}", context_lines.clamp(3, 100_000));
    let patch = run(&[&context])?;
    Ok(ReviewDiffData {
        source,
        numstat: String::new(),
        patch: String::from_utf8_lossy(&patch).into_owned(),
        complete_context: false,
        truncated: false,
        file: Some(file),
    })
}

/// Bound the output while Git is running. Only the child started here is stopped.
pub(crate) fn bounded_git_output_cancellable(
    command: &mut std::process::Command,
    no_index: bool,
    cancelled: Option<&AtomicBool>,
    interactive: bool,
) -> anyhow::Result<Vec<u8>> {
    let exit_mode = if no_index {
        GitExitMode::Diff
    } else {
        GitExitMode::Success
    };
    let (output, complete) =
        bounded_git_output_with_mode(command, exit_mode, cancelled, interactive)?;
    if !complete {
        bail!("the Git output exceeds the size limit");
    }
    Ok(output)
}

/// A listing may use whatever the output budget allowed and report itself
/// incomplete, instead of failing the whole request. Listings are user-facing,
/// so they take the interactive slot priority.
pub(crate) fn bounded_git_output_partial(
    command: &mut std::process::Command,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<(Vec<u8>, bool)> {
    bounded_git_output_with_mode(command, GitExitMode::Success, cancelled, true)
}

pub(crate) fn bounded_git_probe(
    command: &mut std::process::Command,
    cancelled: Option<&AtomicBool>,
    interactive: bool,
) -> anyhow::Result<bool> {
    let (output, _) =
        bounded_git_output_with_mode(command, GitExitMode::Probe, cancelled, interactive)?;
    Ok(!output.is_empty())
}

#[derive(Clone, Copy)]
enum GitExitMode {
    Success,
    Diff,
    Probe,
}

fn bounded_git_output_with_mode(
    command: &mut std::process::Command,
    exit_mode: GitExitMode,
    cancelled: Option<&AtomicBool>,
    interactive: bool,
) -> anyhow::Result<(Vec<u8>, bool)> {
    bounded_git_output_full(command, exit_mode, cancelled, interactive, None)
}

/// A command fed through stdin, such as `git check-ignore --stdin`. The writer
/// runs on its own thread so a large path list cannot deadlock against the
/// output pipes, and the same cancel, timeout, and size budgets apply.
fn bounded_git_output_with_stdin(
    command: &mut std::process::Command,
    input: &[u8],
    cancelled: Option<&AtomicBool>,
    interactive: bool,
) -> anyhow::Result<Vec<u8>> {
    let (output, complete) = bounded_git_output_full(
        command,
        GitExitMode::Diff,
        cancelled,
        interactive,
        Some(input.to_vec()),
    )?;
    if !complete {
        bail!("the Git output exceeds the size limit");
    }
    Ok(output)
}

fn bounded_git_output_full(
    command: &mut std::process::Command,
    exit_mode: GitExitMode,
    cancelled: Option<&AtomicBool>,
    interactive: bool,
    stdin: Option<Vec<u8>>,
) -> anyhow::Result<(Vec<u8>, bool)> {
    use std::io::Read as _;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const MAX_OUTPUT: usize = 8 * 1024 * 1024;
    const MAX_ERROR: usize = 64 * 1024;
    const TIMEOUT: Duration = Duration::from_secs(30);
    enum Message {
        Data(bool, Vec<u8>),
        End(std::io::Result<()>),
    }
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
        bail!("Git query cancelled");
    }
    let _permit = crate::git_process::Permit::acquire(
        if interactive {
            crate::git_process::Priority::Interactive
        } else {
            crate::git_process::Priority::Background
        },
        cancelled,
    )?;
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::command_env::spawn(command).context("failed to start Git command")?;
    if let Some(input) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        std::thread::spawn(move || {
            use std::io::Write as _;
            let _ = pipe.write_all(&input);
        });
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (sender, receiver) = mpsc::sync_channel(16);
    for (mut stream, is_stdout) in [
        (Box::new(stdout) as Box<dyn std::io::Read + Send>, true),
        (Box::new(stderr) as Box<dyn std::io::Read + Send>, false),
    ] {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut buffer = [0; 8192];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => {
                        let _ = sender.send(Message::End(Ok(())));
                        break;
                    }
                    Ok(length) => {
                        if sender
                            .send(Message::Data(is_stdout, buffer[..length].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Message::End(Err(error)));
                        break;
                    }
                }
            }
        });
    }
    drop(sender);
    let started = Instant::now();
    let mut output = Vec::new();
    let mut error_output = Vec::new();
    let mut closed = 0;
    let mut status = None;
    loop {
        if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Git query cancelled");
        }
        if started.elapsed() >= TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("git command timed out");
        }
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(Message::Data(true, chunk)) => {
                if output.len().saturating_add(chunk.len()) > MAX_OUTPUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Return what the budget allowed; the caller decides
                    // whether an incomplete result is usable.
                    return Ok((output, false));
                }
                output.extend_from_slice(&chunk);
            }
            Ok(Message::Data(false, chunk)) => {
                let remaining = MAX_ERROR.saturating_sub(error_output.len());
                error_output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            Ok(Message::End(result)) => {
                if let Err(error) = result {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error).context("failed to read Git output");
                }
                closed += 1;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                closed = 2;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if status.is_none() {
            status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error).context("failed to wait for Git command");
                }
            };
        }
        if let Some(status) = status
            && closed == 2
        {
            if status.success()
                || (matches!(exit_mode, GitExitMode::Diff) && status.code() == Some(1))
            {
                return Ok((output, true));
            }
            if matches!(exit_mode, GitExitMode::Probe) {
                return Ok((Vec::new(), true));
            }
            bail!(
                "Git command failed: {}",
                String::from_utf8_lossy(&error_output).trim()
            );
        }
    }
}

fn resolve_diff_range(cwd: &Path, source: ReviewDiffSource) -> anyhow::Result<DiffRange> {
    let head = resolve(cwd, "HEAD").unwrap_or_else(|| EMPTY_TREE.to_owned());
    Ok(match source {
        ReviewDiffSource::LastTurn {
            session_id,
            turn_count,
            ..
        } => {
            if turn_count == 0 {
                bail!("the first checkpoint is a baseline, not a completed turn");
            }
            let diff_base_ref = crate::checkpoint::turn_diff_base_ref(session_id, turn_count);
            let start_ref = crate::checkpoint::turn_start_ref(session_id, turn_count);
            let legacy_ref = crate::checkpoint::checkpoint_ref(session_id, turn_count - 1);
            let to_ref = crate::checkpoint::checkpoint_ref(session_id, turn_count);
            DiffRange {
                from: resolve(cwd, &diff_base_ref)
                    .or_else(|| resolve(cwd, &start_ref))
                    .or_else(|| resolve(cwd, &legacy_ref))
                    .ok_or_else(|| anyhow!("the turn's starting checkpoint is unavailable"))?,
                to: resolve(cwd, &to_ref)
                    .ok_or_else(|| anyhow!("the turn's ending checkpoint is unavailable"))?,
            }
        }
        ReviewDiffSource::Uncommitted => DiffRange {
            from: head,
            to: crate::checkpoint::capture_worktree_commit(cwd)?,
        },
        ReviewDiffSource::Unstaged => DiffRange {
            from: index_tree(cwd)?,
            to: crate::checkpoint::capture_worktree_commit(cwd)?,
        },
        ReviewDiffSource::Staged => DiffRange {
            from: head,
            to: index_tree(cwd)?,
        },
        ReviewDiffSource::Committed => DiffRange {
            from: branch_base(cwd)?,
            to: head,
        },
        ReviewDiffSource::Branch => DiffRange {
            from: branch_base(cwd)?,
            to: crate::checkpoint::capture_worktree_commit(cwd)?,
        },
    })
}

fn branch_base(cwd: &Path) -> anyhow::Result<String> {
    let Some(snapshot) = crate::git_branch::inspect_refs(cwd)? else {
        bail!("the workspace is not a Git repository");
    };
    let Some(head) = resolve(cwd, "HEAD") else {
        return Ok(EMPTY_TREE.to_owned());
    };
    let current = snapshot.current.as_deref();
    let default_branch = snapshot
        .default_branch
        .filter(|branch| current != Some(branch.as_str()))
        .or_else(|| {
            ["main", "master"]
                .into_iter()
                .find(|candidate| {
                    current != Some(*candidate)
                        && snapshot
                            .branches
                            .iter()
                            .any(|branch| branch.name == *candidate)
                })
                .map(str::to_owned)
        });
    let Some(default_branch) = default_branch else {
        return Ok(head);
    };
    let output = git(cwd, ["merge-base", "HEAD", default_branch.as_str()])?;
    let base = output.trim();
    Ok(if base.is_empty() {
        head
    } else {
        base.to_owned()
    })
}

fn index_tree(cwd: &Path) -> anyhow::Result<String> {
    let mut command = crate::command_env::plain_command("git");
    command.args(["write-tree"]).current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)
            .context("failed to snapshot the Git index")?;
    if output.status.success() {
        let tree = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !tree.is_empty() {
            return Ok(tree);
        }
    }
    if resolve(cwd, "HEAD").is_none() {
        Ok(EMPTY_TREE.to_owned())
    } else {
        bail!("{}", command_error(&output))
    }
}

fn diff_output(cwd: &Path, range: &DiffRange, modes: &[&str]) -> anyhow::Result<String> {
    let mut command = crate::command_env::plain_command("git");
    command
        .args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-ext-diff",
            "--no-color",
        ])
        .args(modes)
        .arg("--no-renames")
        .arg(&range.from)
        .arg(&range.to)
        .args(["--", "."])
        .current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)
            .context("failed to generate Git diff")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        bail!("{}", command_error(&output))
    }
}

fn ensure_repository(cwd: &Path) -> anyhow::Result<()> {
    let mut command = crate::command_env::plain_command("git");
    command
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)
            .context("failed to inspect Git workspace")?;
    if output.status.success() {
        Ok(())
    } else {
        bail!("the workspace is not a Git repository")
    }
}

fn resolve(cwd: &Path, revision: &str) -> Option<String> {
    let mut command = crate::command_env::plain_command("git");
    command
        .args(["rev-parse", "--verify", &format!("{revision}^{{commit}}")])
        .current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn git<I, S>(cwd: &Path, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = crate::command_env::plain_command("git");
    command.args(args).current_dir(cwd);
    let output =
        crate::git_process::output(&mut command, crate::git_process::Priority::Interactive)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        bail!("{}", command_error(&output))
    }
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
    use std::fs;

    use super::*;
    use uuid::Uuid;

    fn git_ok(cwd: &Path, args: &[&str]) {
        let output = crate::command_env::plain_command("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", command_error(&output));
    }

    fn repository() -> PathBuf {
        let root = std::env::temp_dir().join(format!("fintwind-workspace-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        git_ok(&root, &["init", "-b", "main"]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn baseline() {}\n").unwrap();
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

    fn collect(root: &Path, source: ReviewDiffSource) -> ReviewDiffData {
        let WorkspaceResult::ReviewDiff { data } = execute(WorkspaceOperation::CollectReviewDiff {
            cwd: root.to_path_buf(),
            source,
        })
        .unwrap() else {
            panic!("unexpected workspace response")
        };
        data
    }

    fn numstat_summary(numstat: &str) -> (usize, u64, u64) {
        let mut files = 0;
        let mut additions = 0;
        let mut deletions = 0;
        for line in numstat.lines().filter(|line| !line.is_empty()) {
            let mut fields = line.splitn(3, '\t');
            additions += fields
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            deletions += fields
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            assert!(fields.next().is_some(), "numstat row is missing its path");
            files += 1;
        }
        (files, additions, deletions)
    }

    #[test]
    fn directory_browser_lists_an_arbitrary_daemon_directory() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-directory-browser-{}", Uuid::new_v4()));
        fs::create_dir_all(directory.join("folder")).unwrap();
        fs::create_dir_all(directory.join(".git")).unwrap();
        fs::write(directory.join("notes.txt"), "notes").unwrap();

        let WorkspaceResult::Directory {
            path,
            parent,
            entries,
            ..
        } = execute(WorkspaceOperation::BrowseDirectory {
            path: Some(directory.clone()),
        })
        .unwrap()
        else {
            panic!("unexpected workspace response")
        };

        assert_eq!(path, fs::canonicalize(&directory).unwrap());
        assert_eq!(parent, path.parent().map(Path::to_owned));
        assert_eq!(
            entries
                .iter()
                .map(|entry| (&*entry.name, entry.is_dir))
                .collect::<Vec<_>>(),
            [(".git", true), ("folder", true), ("notes.txt", false)]
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn file_reader_keeps_the_complete_disk_content() {
        let root = std::env::temp_dir().join(format!("fintwind-editor-file-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let content = "line\n".repeat(10_000);
        fs::write(root.join("large.txt"), &content).unwrap();

        let WorkspaceResult::TextFile { content: restored } =
            execute(WorkspaceOperation::ReadTextFile {
                root: root.clone(),
                relative_path: PathBuf::from("large.txt"),
            })
            .unwrap()
        else {
            panic!("unexpected workspace response")
        };
        assert_eq!(restored, content);
        assert!(
            execute(WorkspaceOperation::ReadTextFile {
                root: root.clone(),
                relative_path: PathBuf::from("missing.txt"),
            })
            .is_err()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_modes_compare_consistent_git_snapshots() {
        let root = repository();
        git_ok(&root, &["switch", "-c", "feature"]);
        fs::write(root.join("src/lib.rs"), "fn committed() {}\n").unwrap();
        git_ok(&root, &["add", "src/lib.rs"]);
        git_ok(
            &root,
            &[
                "-c",
                "user.name=Fintwind Tests",
                "-c",
                "user.email=fintwind@example.com",
                "commit",
                "-m",
                "feature",
            ],
        );
        fs::write(
            root.join("src/lib.rs"),
            "fn committed() {}\nfn staged() {}\n",
        )
        .unwrap();
        git_ok(&root, &["add", "src/lib.rs"]);
        fs::write(
            root.join("src/lib.rs"),
            "fn committed() {}\nfn staged() {}\nfn unstaged() {}\n",
        )
        .unwrap();
        fs::write(root.join("new file.txt"), "untracked\n").unwrap();

        let committed = collect(&root, ReviewDiffSource::Committed);
        let staged = collect(&root, ReviewDiffSource::Staged);
        let unstaged = collect(&root, ReviewDiffSource::Unstaged);
        let uncommitted = collect(&root, ReviewDiffSource::Uncommitted);
        let branch = collect(&root, ReviewDiffSource::Branch);

        assert_eq!(numstat_summary(&committed.numstat), (1, 1, 1));
        assert_eq!(numstat_summary(&staged.numstat), (1, 1, 0));
        assert_eq!(
            numstat_summary(&unstaged.numstat),
            (2, 2, 0),
            "unstaged includes untracked files"
        );
        assert_eq!(numstat_summary(&uncommitted.numstat), (2, 3, 0));
        assert_eq!(numstat_summary(&branch.numstat), (2, 4, 1));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn last_turn_uses_captured_checkpoints_not_the_live_worktree() {
        let root = repository();
        let session_id = Uuid::new_v4();
        crate::checkpoint::capture_turn(&root, session_id, 0).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "fn baseline() {}\nfn from_turn() {}\n",
        )
        .unwrap();
        crate::checkpoint::capture_turn(&root, session_id, 1).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "fn baseline() {}\nfn from_turn() {}\nfn after_turn() {}\n",
        )
        .unwrap();

        let data = collect(
            &root,
            ReviewDiffSource::LastTurn {
                session_id,
                turn_id: Uuid::new_v4(),
                turn_count: 1,
            },
        );
        assert_eq!(numstat_summary(&data.numstat), (1, 1, 0));
        assert!(data.patch.contains("from_turn"));
        assert!(!data.patch.contains("after_turn"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn last_turn_review_uses_the_branch_aware_diff_base() {
        let root = repository();
        git_ok(&root, &["switch", "-c", "feature"]);
        fs::write(root.join("feature-only.rs"), "fn feature() {}\n").unwrap();
        git_ok(&root, &["add", "feature-only.rs"]);
        git_ok(
            &root,
            &[
                "-c",
                "user.name=Fintwind Tests",
                "-c",
                "user.email=fintwind@example.com",
                "commit",
                "-m",
                "feature baseline",
            ],
        );
        git_ok(&root, &["switch", "main"]);
        fs::write(root.join("main-only.rs"), "fn main_only() {}\n").unwrap();
        git_ok(&root, &["add", "main-only.rs"]);
        git_ok(
            &root,
            &[
                "-c",
                "user.name=Fintwind Tests",
                "-c",
                "user.email=fintwind@example.com",
                "commit",
                "-m",
                "main baseline",
            ],
        );

        let session_id = Uuid::new_v4();
        crate::checkpoint::capture_turn_start(&root, session_id, 1).unwrap();
        git_ok(&root, &["switch", "feature"]);
        fs::write(
            root.join("src/lib.rs"),
            "fn baseline() {}\nfn from_turn() {}\n",
        )
        .unwrap();
        crate::checkpoint::capture_turn(&root, session_id, 1).unwrap();

        let data = collect(
            &root,
            ReviewDiffSource::LastTurn {
                session_id,
                turn_id: Uuid::new_v4(),
                turn_count: 1,
            },
        );
        assert_eq!(numstat_summary(&data.numstat), (1, 1, 0));
        assert!(data.numstat.ends_with("\tsrc/lib.rs\n"));
        fs::remove_dir_all(root).ok();
    }
}

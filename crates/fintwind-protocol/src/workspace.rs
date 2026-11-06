use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::composer::{FileEntry, SlashCommand};
use crate::git::{
    AgentInvocation, BranchSnapshot, CommitEntry, CommitSnapshot, CommitStats, CreatedWorktree,
};
use crate::model::Checkpoint;

/// Stable failure kinds for [`WorkspaceOperation::BrowseDirectories`].
///
/// They travel as the prefix of the daemon's error message
/// (`"<kind>: <detail>"`), because `RpcError` carries only a message.
/// `detail` names the daemon-side path and carries no credentials.
pub mod directory_browse {
    /// The resolved directory does not exist.
    pub const NOT_FOUND: &str = "directory_not_found";
    /// The directory cannot be read with the daemon's permissions.
    pub const NOT_READABLE: &str = "directory_not_readable";
    /// The resolved path exists but is not a directory.
    pub const NOT_DIRECTORY: &str = "not_a_directory";
    /// A relative path arrived without an already-verified base directory.
    pub const RELATIVE_WITHOUT_BASE: &str = "relative_without_base";
    /// The daemon user's home directory is unavailable.
    pub const HOME_UNAVAILABLE: &str = "home_unavailable";

    const ALL: &[&str] = &[
        NOT_FOUND,
        NOT_READABLE,
        NOT_DIRECTORY,
        RELATIVE_WITHOUT_BASE,
        HOME_UNAVAILABLE,
    ];

    /// Extract the failure kind from a daemon error message, if it carries one.
    pub fn kind_of(message: &str) -> Option<&str> {
        let (kind, _) = message.split_once(": ")?;
        ALL.iter().copied().find(|known| *known == kind)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDiffSource {
    LastTurn {
        session_id: Uuid,
        turn_id: Uuid,
        turn_count: usize,
    },
    Uncommitted,
    Unstaged,
    Staged,
    Committed,
    Branch,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffData {
    pub source: ReviewDiffSource,
    pub numstat: String,
    pub patch: String,
    pub complete_context: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub file: Option<ReviewDiffFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingTreeEntry {
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub depth: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkspaceOperation {
    ListTree {
        root: PathBuf,
        expanded_paths: Vec<PathBuf>,
    },
    BrowseDirectory {
        path: Option<PathBuf>,
    },
    /// Read-only directory picker for the SSH "add project" flow.
    ///
    /// The daemon resolves `path` on its own host and returns only
    /// directories (including enterable symlinked directories), never files.
    /// `None` selects the daemon user's home directory. An absolute `path`
    /// is used verbatim; a leading `~` segment expands to the daemon HOME;
    /// a relative `path` resolves against the already-verified absolute
    /// `base` directory and is rejected without one; a non-absolute base is
    /// rejected rather than resolved against the daemon's working directory. Raw bytes travel as path data;
    /// no shell is involved, so spaces, non-UTF8-host names and other
    /// special characters are never interpreted. Filtering text and request
    /// generations stay on the caller: the daemon treats every relative
    /// `path` as navigation, never as a filter. Callers must scope the
    /// connection before sending this; the daemon itself performs no
    /// authorization and creates, renames, or writes nothing.
    /// Failures travel as transport errors whose message starts with one of
    /// [`directory_browse`] kinds (`"<kind>: <detail>"`), because `RpcError`
    /// carries only a message.
    BrowseDirectories {
        path: Option<PathBuf>,
        /// Already-verified absolute current directory. Required when `path` is
        /// relative; ignored otherwise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<PathBuf>,
        /// Include dot-directories such as `.config`. Defaults to hidden.
        #[serde(default)]
        include_hidden: bool,
    },
    ReadTextFile {
        root: PathBuf,
        relative_path: PathBuf,
    },
    WriteTextFile {
        root: PathBuf,
        relative_path: PathBuf,
        content: String,
    },
    ListProjectFiles {
        root: PathBuf,
        cap: usize,
    },
    DiscoverSlashCommands {
        project_root: PathBuf,
        /// The OpenCode binary the client probed, when it has one. The daemon
        /// folds the server's own command registry — `opencode.json` entries
        /// and everything plugins registered — into the filesystem discovery
        /// through it. `None` keeps discovery filesystem-only.
        binary: Option<PathBuf>,
    },
    CreateProjectlessWorkspace {
        prompt: Option<String>,
    },
    MigrateProjectlessWorkspace {
        path: PathBuf,
    },
    InspectBranches {
        cwd: PathBuf,
    },
    /// Branch menu metadata without scanning file contents for line counts.
    InspectBranchRefs {
        cwd: PathBuf,
    },
    InspectPullRequests {
        cwd: PathBuf,
    },
    /// The commit history of a workspace, newest first. `branch` `None` reads
    /// `HEAD`; `Some` names a branch or any revision Git accepts.
    ListCommits {
        cwd: PathBuf,
        limit: usize,
        branch: Option<String>,
    },
    ReadCommitStats {
        cwd: PathBuf,
        hash: String,
    },
    /// The currently checked-out branch of a workspace. Far cheaper than
    /// [`Self::InspectBranches`], which enumerates refs and measures the
    /// working tree; list surfaces that only draw the branch name ask for this.
    CurrentBranch {
        cwd: PathBuf,
    },
    CheckoutBranch {
        cwd: PathBuf,
        branch: String,
        create: bool,
    },
    CreateWorktree {
        project_path: PathBuf,
        project_id: Uuid,
        session_id: Uuid,
        prompt: String,
        base_branch: Option<String>,
    },
    InspectCommit {
        cwd: PathBuf,
    },
    /// The live worktree status of a workspace: branch plus every staged,
    /// unstaged, and untracked file. `WorkspaceResult::WorktreeStatus` with
    /// `None` means `cwd` is not inside a Git repository.
    InspectStatus {
        cwd: PathBuf,
    },
    InspectTrackedStatus {
        cwd: PathBuf,
        #[serde(default)]
        query_id: Option<Uuid>,
    },
    ListUntracked {
        cwd: PathBuf,
        directory: Option<String>,
        /// Last direct child of the previous page; the next page starts after
        /// it. `None` starts at the beginning of the directory.
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        query_id: Option<Uuid>,
    },
    ListIgnored {
        cwd: PathBuf,
        directory: Option<String>,
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        query_id: Option<Uuid>,
    },
    DiscoverNestedRepositories {
        cwd: PathBuf,
        candidates: Vec<String>,
        #[serde(default)]
        query_id: Option<Uuid>,
    },
    /// Stages files by literal repository-relative path.
    StagePaths {
        cwd: PathBuf,
        paths: Vec<String>,
    },
    /// Unstages files without touching the worktree.
    UnstagePaths {
        cwd: PathBuf,
        paths: Vec<String>,
    },
    /// Discards unstaged worktree changes: tracked files restore from the
    /// index, untracked files are deleted from disk (irrecoverable — the UI
    /// confirms first).
    DiscardWorktreePaths {
        cwd: PathBuf,
        tracked: Vec<String>,
        untracked: Vec<String>,
    },
    /// Fetches every remote and prunes stale tracking refs.
    FetchRemote {
        cwd: PathBuf,
    },
    /// Pulls with fast-forward only; a diverged branch is an error, never an
    /// automatic merge.
    PullFastForward {
        cwd: PathBuf,
    },
    GenerateCommitMessage {
        cwd: PathBuf,
        include_unstaged: bool,
        invocation: AgentInvocation,
    },
    Commit {
        cwd: PathBuf,
        message: String,
        include_unstaged: bool,
        push: bool,
    },
    Push {
        cwd: PathBuf,
    },
    CaptureTurnStart {
        cwd: PathBuf,
        session_id: Uuid,
        turn_count: usize,
    },
    CaptureTurn {
        cwd: PathBuf,
        session_id: Uuid,
        turn_count: usize,
    },
    CaptureRef {
        cwd: PathBuf,
        git_ref: String,
    },
    RestoreRef {
        cwd: PathBuf,
        git_ref: String,
    },
    HasRef {
        cwd: PathBuf,
        git_ref: String,
    },
    SessionTurnRefs {
        cwd: PathBuf,
        session_id: Uuid,
    },
    DeleteRef {
        cwd: PathBuf,
        git_ref: String,
    },
    DeleteTurnRefsAfter {
        cwd: PathBuf,
        session_id: Uuid,
        retained_turn_count: usize,
        previous_turn_count: usize,
    },
    DeleteSessionRefs {
        cwd: PathBuf,
        session_id: Uuid,
    },
    CopySessionRefs {
        cwd: PathBuf,
        source_session_id: Uuid,
        target_session_id: Uuid,
        through_turn_count: usize,
    },
    CollectReviewDiff {
        cwd: PathBuf,
        source: ReviewDiffSource,
    },
    CollectReviewFileDiff {
        cwd: PathBuf,
        source: ReviewDiffSource,
        path: String,
        origin_path: Option<String>,
        untracked: bool,
        context_lines: u32,
        #[serde(default)]
        query_id: Option<Uuid>,
    },
    /// Line totals for every file in one review source. The review toolbar
    /// shows one figure for the whole area, which no single file's diff can
    /// supply; the caller fetches this beside the file it renders.
    CollectReviewStats {
        cwd: PathBuf,
        source: ReviewDiffSource,
        #[serde(default)]
        query_id: Option<Uuid>,
    },
    #[serde(alias = "cancelReviewFileDiff")]
    CancelGitQuery {
        query_id: Uuid,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkspaceResult {
    /// A mutation that only reports success; failures arrive as transport or
    /// daemon errors carrying the Git message.
    Ack,
    WorkingTree {
        entries: Vec<WorkingTreeEntry>,
    },
    Directory {
        path: PathBuf,
        parent: Option<PathBuf>,
        home: PathBuf,
        filesystem_root: PathBuf,
        entries: Vec<WorkingTreeEntry>,
    },
    TextFile {
        content: String,
    },
    ProjectFiles {
        entries: Vec<FileEntry>,
    },
    SlashCommands {
        commands: Vec<SlashCommand>,
    },
    ProjectlessWorkspace {
        cwd: PathBuf,
    },
    Branches {
        snapshot: Option<BranchSnapshot>,
    },
    PullRequests {
        snapshot: crate::pull_request::PullRequestSnapshot,
    },
    Commits {
        /// `None` means `cwd` is not inside a Git repository.
        commits: Option<Vec<CommitEntry>>,
    },
    CommitStats {
        stats: CommitStats,
    },
    CurrentBranch {
        branch: Option<String>,
    },
    BranchChanged {
        snapshot: BranchSnapshot,
    },
    WorktreeCreated {
        worktree: CreatedWorktree,
    },
    CommitSnapshot {
        snapshot: CommitSnapshot,
    },
    WorktreeStatus {
        /// `None` means `cwd` is not inside a Git repository.
        status: Option<crate::git::WorktreeStatus>,
    },
    IgnoredFiles {
        entries: Vec<crate::git::WorktreeStatusEntry>,
        /// Pass back as `after` to read the next page. `None` means this
        /// directory is fully listed, or the page could not be continued.
        #[serde(default)]
        next: Option<String>,
        /// The output budget stopped this page before every child was read, so
        /// it must be narrowed instead of paged.
        #[serde(default)]
        incomplete: bool,
    },
    UntrackedFiles {
        entries: Vec<crate::git::WorktreeStatusEntry>,
        #[serde(default)]
        next: Option<String>,
        #[serde(default)]
        incomplete: bool,
    },
    NestedRepositories {
        repositories: Vec<crate::git::NestedRepository>,
        /// False when the scan hit its directory, time, or count budget, so
        /// the list is a partial view of the workspace.
        #[serde(default)]
        complete: bool,
    },
    CommitMessage {
        message: String,
    },
    Checkpoint {
        checkpoint: Checkpoint,
    },
    Bool {
        value: bool,
    },
    TurnRefs {
        turn_counts: Vec<usize>,
    },
    ReviewDiff {
        data: ReviewDiffData,
    },
    ReviewStats {
        additions: u64,
        deletions: u64,
        files: u64,
        /// True when the untracked scan stopped at a budget, so the totals
        /// cover only the files it read.
        #[serde(default)]
        truncated: bool,
    },
}

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

/// Stable rejection and failure kinds for the local empty-project journal
/// RPC ([`WorkspaceOperation::CreateEmptyProject`] and
/// [`WorkspaceOperation::GetProjectOperation`]).
///
/// They travel as the prefix of the daemon's error message
/// (`"<kind>: <detail>"`), because `RpcError` carries only a message, or as
/// the `failure_kind` of a settled failure, because that row is terminal.
/// `detail` is fixed daemon text and never reflects caller input, daemon
/// paths, raw OS errors, subprocess output, or credentials. The terminal
/// failure tags mirror the journal's fixed categories byte-for-byte; the
/// `local` in their meaning is that this daemon's own machine executed the
/// write, never a remote environment.
pub mod project_operation {
    /// The project name is not a single valid path segment.
    pub const INVALID_NAME: &str = "invalid_name";
    /// The parent directory cannot be used (empty, unresolvable, or not
    /// storable as text). Missing and unreadable parents carry the narrower
    /// [`PARENT_MISSING`] and [`PARENT_NOT_WRITABLE`] kinds instead.
    pub const INVALID_PARENT: &str = "invalid_parent";
    /// Parent plus name do not form a storable target path.
    pub const INVALID_TARGET: &str = "invalid_target";
    /// The operation ID is already registered. Nothing was executed; the
    /// caller must reconcile the old row and retry with a fresh ID, never by
    /// reusing this one.
    pub const DUPLICATE_OPERATION: &str = "duplicate_operation";
    /// The journal is full or otherwise unavailable. Nothing was executed.
    pub const JOURNAL_UNAVAILABLE: &str = "journal_unavailable";
    /// No operation with this ID is registered locally. Missing rows and rows
    /// under any other environment share this one answer, so a query reveals
    /// nothing about other environments.
    pub const NOT_REGISTERED: &str = "operation_not_registered";
    /// The final path already exists. Recorded as a terminal failure; the
    /// directory is left untouched, never overwritten or adopted as success.
    pub const TARGET_EXISTS: &str = "target_exists";
    /// The parent directory does not exist. Parents are never created.
    pub const PARENT_MISSING: &str = "parent_missing";
    /// The parent directory cannot be read.
    pub const PARENT_NOT_WRITABLE: &str = "parent_not_writable";
    /// A Git executable the operation needed is not available.
    pub const GIT_NOT_AVAILABLE: &str = "git_not_available";
    /// The operation needs an interaction the caller did not authorize.
    pub const AUTH_REQUIRED: &str = "auth_required";
    /// The network the operation needed is unreachable.
    pub const NETWORK_UNREACHABLE: &str = "network_unreachable";
    /// A remote step the operation needed failed.
    pub const REMOTE_FAILED: &str = "remote_failed";
    /// The write was interrupted before its effect could be proven.
    pub const INTERRUPTED: &str = "interrupted";
    /// The daemon cannot classify the failure more narrowly.
    pub const INTERNAL: &str = "internal";

    const ALL: &[&str] = &[
        INVALID_NAME,
        INVALID_PARENT,
        INVALID_TARGET,
        DUPLICATE_OPERATION,
        JOURNAL_UNAVAILABLE,
        NOT_REGISTERED,
        TARGET_EXISTS,
        PARENT_MISSING,
        PARENT_NOT_WRITABLE,
        GIT_NOT_AVAILABLE,
        AUTH_REQUIRED,
        NETWORK_UNREACHABLE,
        REMOTE_FAILED,
        INTERRUPTED,
        INTERNAL,
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
    /// Journaled creation of one empty project directory on this daemon's own
    /// machine (F05, local only).
    ///
    /// The caller mints a stable `operation_id` for its attempt; after the
    /// caller reconciles a finished or unknown row, a retry mints a fresh ID
    /// because the journal rejects ID reuse before any filesystem write.
    /// `parent_dir` names an explicit already-existing parent on the daemon
    /// host and `project_name` is one path segment with the same rules as the
    /// local directory creator (no `.`/`..`, no separators or NUL, no
    /// trailing space or dot; CJK, spaces, `@` and `#` pass through as path
    /// data). There is deliberately no environment field: the daemon always
    /// runs this as its local environment and never treats a caller string,
    /// alias, or path as an environment. There is no `init_git` field on
    /// purpose: Git initialization is a separate future journaled step, and
    /// nothing here may look like it.
    ///
    /// The daemon answers with [`WorkspaceResult::ProjectOperation`]:
    /// `succeeded` proves this call created the directory, `failed` is a
    /// terminal failure whose directory (when present) is retained and must
    /// be inspected rather than deleted or overwritten, and
    /// `needsReconciliation`/`unknown` means the effect cannot be proven, so
    /// the caller must query [`Self::GetProjectOperation`] and inspect the
    /// filesystem instead of retrying the write or treating a pre-existing
    /// directory as this call's success. Errors before any write carry a
    /// [`project_operation`] kind and mean nothing was executed.
    CreateEmptyProject {
        operation_id: Uuid,
        parent_dir: PathBuf,
        project_name: String,
    },
    /// Read-only point-in-time reading of one journaled project operation on
    /// this daemon's own machine.
    ///
    /// Always scoped to the daemon's local environment; an ID registered
    /// nowhere locally reads back as [`project_operation::NOT_REGISTERED`]
    /// without revealing anything about other environments. The query
    /// performs no write, cleanup, or retry.
    GetProjectOperation {
        operation_id: Uuid,
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
    /// Point-in-time reading of one journaled local empty-project operation.
    ///
    /// `path` is the journaled canonical target: for `succeeded` the
    /// directory this call created; for `failed` the directory that may
    /// already exist or have been left behind, which the caller must inspect
    /// (offering the existing-directory flow) rather than delete, overwrite,
    /// or adopt as success; for `needsReconciliation` or `unknown` the last
    /// journaled target to reconcile with [`WorkspaceOperation::GetProjectOperation`]
    /// plus a filesystem inspection, never with a blind rewrite.
    /// `failure_kind` carries a [`project_operation`] kind for `failed` rows
    /// and is absent otherwise.
    ProjectOperation {
        operation_id: Uuid,
        state: ProjectOperationState,
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_kind: Option<String>,
    },
}

/// Honest lifecycle position of one journaled local project operation.
///
/// The first four are settled journal states and never leave; `Unknown` is a
/// terminal row whose effect could not be proven, not a missing row.
/// `NeedsReconciliation` means the row is not settled and, without owner or
/// lease proof, no liveness may be claimed: whoever can inspect the
/// filesystem must reconcile before deciding anything.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectOperationState {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
    NeedsReconciliation,
}

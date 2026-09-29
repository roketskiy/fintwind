use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BranchEntry {
    pub name: String,
    pub checked_out_elsewhere: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BranchSnapshot {
    pub repository: PathBuf,
    pub current: Option<String>,
    pub detached_head: Option<String>,
    pub default_branch: Option<String>,
    pub branches: Vec<BranchEntry>,
    pub additions: u64,
    pub deletions: u64,
}

impl BranchSnapshot {
    pub fn display_branch(&self) -> Option<&str> {
        self.current.as_deref().or(self.detached_head.as_deref())
    }
}

/// One `%D` decoration of a commit, split into its own label. `remote` marks
/// decorations that point at a remote-tracking ref, e.g. `origin/main`;
/// `head` marks the decoration `HEAD` points at (`HEAD` itself when
/// detached, `HEAD -> branch` otherwise).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommitRef {
    /// The label as Git prints it, e.g. `HEAD -> main`, `origin/main`, or
    /// `tag: v1.2.0`.
    pub label: String,
    /// Whether this decoration points at a remote ref, i.e. starts with one
    /// of the repository's remote names followed by `/`.
    pub remote: bool,
    /// Whether this decoration is where `HEAD` points.
    pub head: bool,
}

/// One entry in a workspace's commit history, as drawn by the Git history
/// panel. `refs` are Git's `%D` decorations split into separate labels, e.g.
/// `HEAD -> main`, `origin/main`, `tag: v1.2.0`; `is_head` marks the commit
/// `HEAD` points at, including a detached HEAD; `pushed` tells whether the
/// commit is reachable from any remote ref.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommitEntry {
    pub hash: String,
    pub short_hash: String,
    pub subject: String,
    pub author: String,
    /// Author date, in Unix seconds.
    pub timestamp: u64,
    pub refs: Vec<CommitRef>,
    pub is_head: bool,
    pub pushed: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommitSnapshot {
    pub branch: String,
    pub additions: u64,
    pub deletions: u64,
    pub staged_additions: u64,
    pub staged_deletions: u64,
    pub has_staged: bool,
    pub has_unstaged: bool,
    pub can_push: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentInvocation {
    pub binary: PathBuf,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreatedWorktree {
    pub path: PathBuf,
    pub branch: String,
}

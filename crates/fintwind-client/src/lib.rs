//! Rust transport and lifecycle support for clients of `fintwind-daemon`.
//!
//! This crate depends only on [`fintwind_protocol`] plus lightweight helper
//! crates (`fintwind-env`, `fintwind-http`). It never touches daemon-owned
//! filesystem, Git, databases, or workspace servers.

mod client;
pub mod command_env;
pub mod composer_complete;
pub mod driver;
pub mod persistence;
mod process;
#[cfg(feature = "ssh")]
pub mod ssh;
mod workspace_client;

pub use client::DaemonClient;
pub use fintwind_protocol::*;
pub use process::{DaemonProcess, DaemonSupervisor};
pub use workspace_client::WorkspaceClient;

pub mod git_branch {
    pub use fintwind_protocol::git::{BranchEntry, BranchSnapshot};
}

pub mod git_commit {
    pub use fintwind_protocol::git::AgentInvocation;
    pub use fintwind_protocol::git::CommitSnapshot as Snapshot;
}

pub mod worktree {
    pub use fintwind_protocol::git::CreatedWorktree;
}

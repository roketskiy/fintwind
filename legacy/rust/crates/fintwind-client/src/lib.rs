//! Rust transport and lifecycle support for clients of `fintwind-daemon`.
//!
//! This crate depends only on [`fintwind_protocol`] plus read-only access to
//! the desktop machine's own Claude/Codex sign-in (see [`local_quota`]) for
//! subscription reads that must never ride remote IPC. It never touches
//! daemon-owned filesystem, Git, databases, or workspace servers.

mod client;
pub mod claude_pty;
pub mod command_env;
pub mod composer_complete;
pub mod custom_providers;
pub mod driver;
pub mod git_history;
pub mod local_quota;
pub mod model_protocol;
pub mod models_dev;
pub mod opencode_config;
pub mod opencode_image_generate;
pub mod opencode_shell;
pub mod opencode_todo;
pub mod persistence;
mod process;
mod provider_thinking;
mod workspace_client;

pub use client::{BrowserNotification, DaemonClient};
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

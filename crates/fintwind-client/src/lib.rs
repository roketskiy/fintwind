//! Rust transport and lifecycle support for clients of `fintwind-daemon`.
//!
//! This crate intentionally depends only on [`fintwind_protocol`], so GUI and CLI
//! clients cannot accidentally reach daemon-owned filesystem, Git, database,
//! or provider implementations.

mod client;
pub mod command_env;
pub mod composer_complete;
pub mod computer_use;
pub mod custom_providers;
pub mod driver;
pub mod models_dev;
pub mod opencode_config;
pub mod persistence;
mod process;
mod workspace_client;

pub use client::DaemonClient;
pub use process::{
    DEFAULT_EXPOSED_DAEMON_PORT, DaemonExposureSettings, DaemonProcess, DaemonSupervisor,
    parse_allowed_origins,
};
pub use fintwind_protocol::*;
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

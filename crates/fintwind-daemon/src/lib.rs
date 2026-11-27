#![recursion_limit = "256"]

//! Headless Fintwind daemon backend and authenticated WebSocket server.

pub use fintwind_driver::{DaemonSettings, DaemonSettingsStore, mermaid_check_environment, model, settings};
pub use fintwind_git::{checkpoint, git_actions, git_branch, git_commit, pull_requests, worktree};
pub use fintwind_storage::{attachments, blob_store, persistence};
pub use fintwind_workspace::{composer_complete, projectless, skills, terminal, workspace};
pub use workspace::{WorkspaceOperation, WorkspaceResult};

pub mod daemon;
mod protocol;
mod server;

pub use protocol::{
    APP_EXECUTABLE_ENV, ClientMessage, Command, DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, DaemonReady,
    PROTOCOL_VERSION, PromptFile, ReplayCursor, Request, ResponseOutcome, ResponsePayload,
    RpcError, SequencedEvent, ServerMessage, WireDriverEvent, WireDriverStartOptions,
    WireSessionOptions,
};
pub use server::{Backend, EventSink, ServerOptions, SystemChannel, serve};

#![recursion_limit = "256"]

//! OpenCode and native AI provider drivers, server pool, and session lifecycle.

macro_rules! tr {
    ($($args:tt)*) => {
        fintwind_protocol::tr!($($args)*)
    };
}

pub mod context_plugin;
pub mod custom_provider_credentials;
pub mod driver;
pub mod mcp_auth;
pub mod mermaid_check;
pub use mermaid_check::environment as mermaid_check_environment;
pub mod model;
pub mod model_catalog;
pub mod native_goal;
pub mod opencode_diagnostics;
pub mod opencode_events;
pub mod opencode_management;
pub mod opencode_pool;
pub mod opencode_session;
pub mod session_cleanup;
pub mod session_generation;
pub mod settings;
pub mod skill_prompt;

pub use settings::{DaemonSettings, DaemonSettingsStore};

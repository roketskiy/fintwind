//! Library half of `fintwind-remote-host`: pairing sessions, grants, host
//! identity, and the relay session loop, shared between the daemon binary
//! and the desktop app's device-management page.

pub mod adapter;
pub mod approvals;
pub mod config;
pub mod dedup;
pub mod devices;
pub mod events;
pub mod grants;
pub mod identity;
pub mod outbound;
pub mod pair;
pub mod pending;
pub mod queue;
pub mod relay_session;
pub mod state;
pub mod workspace_diff;
pub mod workspace_directory;
pub mod workspace_reads;

pub use config::HostConfig;
pub use grants::{DeviceGrant, GRANT_SCOPE_TEXT, GrantFile};
pub use identity::HostIdentity;

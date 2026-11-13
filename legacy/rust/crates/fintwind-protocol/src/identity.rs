//! Shared application identity used by the daemon and desktop client.

#[cfg(debug_assertions)]
pub const APP_NAME: &str = "fintwind Dev";
#[cfg(not(debug_assertions))]
pub const APP_NAME: &str = "fintwind";

#[cfg(debug_assertions)]
pub const APP_ID: &str = "sh.fintwind.dev";
#[cfg(not(debug_assertions))]
pub const APP_ID: &str = "sh.fintwind";

#[cfg(debug_assertions)]
// Keep the existing development directory name so current agent data is not
// orphaned when the channel label changes from Debug to Dev.
pub const DATA_DIRECTORY_NAME: &str = "Fintwind Debug";
#[cfg(not(debug_assertions))]
pub const DATA_DIRECTORY_NAME: &str = "Fintwind";

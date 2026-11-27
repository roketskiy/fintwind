//! Shared application identity used by the daemon and desktop client.

#[cfg(debug_assertions)]
pub const APP_NAME: &str = "fintwind Dev";
#[cfg(not(debug_assertions))]
pub const APP_NAME: &str = "fintwind";

#[cfg(debug_assertions)]
pub const APP_ID: &str = "sh.fintwind.dev";
#[cfg(not(debug_assertions))]
pub const APP_ID: &str = "sh.fintwind";

use std::path::{Path, PathBuf};

#[cfg(debug_assertions)]
// Keep the existing development directory name so current agent data is not
// orphaned when the channel label changes from Debug to Dev.
pub const DATA_DIRECTORY_NAME: &str = "Fintwind Debug";
#[cfg(not(debug_assertions))]
pub const DATA_DIRECTORY_NAME: &str = "Fintwind";

pub fn debug_data_directory() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var_os("FINTWIND_DEBUG_DATA_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub fn default_data_directory() -> PathBuf {
    if cfg!(debug_assertions) {
        if let Some(root) = debug_data_directory() {
            return root;
        }
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
            .join("temp")
    } else {
        dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(DATA_DIRECTORY_NAME)
    }
}

pub fn default_state_db_path() -> PathBuf {
    default_data_directory().join("app.db")
}

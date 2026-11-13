//! The launcher agent profile must not fall back to Windows Known Folders.
//! `dirs` deliberately ignores HOME/LOCALAPPDATA on Windows. All other
//! callers retain the platform's normal path resolution.
use std::path::PathBuf;

fn isolated() -> bool {
    std::env::var("FINTWIND_MYGO_PROFILE").as_deref() == Ok("agent")
}

/// Explicit launcher profiles override checkout-wide debug cache/work dirs.
pub fn launcher_profile() -> bool {
    matches!(
        std::env::var("FINTWIND_MYGO_PROFILE").as_deref(),
        Ok("agent" | "daily")
    )
}

fn explicit(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub fn home_dir() -> Option<PathBuf> {
    if isolated() {
        explicit("USERPROFILE").or_else(|| explicit("HOME"))
    } else {
        dirs::home_dir()
    }
}

pub fn data_local_dir() -> Option<PathBuf> {
    if isolated() {
        explicit("LOCALAPPDATA")
    } else {
        dirs::data_local_dir()
    }
}

pub fn cache_dir() -> Option<PathBuf> {
    if isolated() {
        explicit("XDG_CACHE_HOME")
    } else {
        dirs::cache_dir()
    }
}

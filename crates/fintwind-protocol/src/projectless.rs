//! Pure daemon-path classification shared by clients and the daemon.

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

struct WorkspacePaths {
    root: Option<PathBuf>,
    legacy_root: Option<PathBuf>,
}

fn workspace_root_slot() -> &'static RwLock<WorkspacePaths> {
    static ROOT: OnceLock<RwLock<WorkspacePaths>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let legacy_root = dirs::home_dir().map(|home| home.join(".fintwind"));
        RwLock::new(WorkspacePaths {
            root: legacy_root.as_ref().map(|root| root.join("projects")),
            legacy_root,
        })
    })
}

pub fn set_workspace_root(root: Option<PathBuf>) {
    if let Ok(mut current) = workspace_root_slot().write() {
        current.root = root;
    }
}

pub fn workspace_root() -> Option<PathBuf> {
    workspace_root_slot().read().ok()?.root.clone()
}

pub fn set_legacy_root(root: Option<PathBuf>) {
    if let Ok(mut current) = workspace_root_slot().write() {
        current.legacy_root = root;
    }
}

pub fn legacy_root() -> Option<PathBuf> {
    workspace_root_slot().read().ok()?.legacy_root.clone()
}

pub fn home_directory() -> Option<PathBuf> {
    legacy_root()?.parent().map(Path::to_path_buf)
}

pub fn is_projectless_path(path: &Path) -> bool {
    let Ok(paths) = workspace_root_slot().read() else {
        return false;
    };
    paths
        .root
        .as_ref()
        .is_some_and(|root| path.starts_with(root))
        || paths.legacy_root.as_ref().is_some_and(|root| {
            path.starts_with(root.join("projects")) || is_legacy_workspace_path(path, root)
        })
}

pub fn needs_migration(path: &Path) -> bool {
    let Ok(paths) = workspace_root_slot().read() else {
        return false;
    };
    paths.legacy_root.as_ref().is_some_and(|legacy| {
        path == legacy
            || (!paths
                .root
                .as_ref()
                .is_some_and(|root| path.starts_with(root))
                && is_legacy_workspace_path(path, legacy))
    })
}

pub fn is_legacy_root_path(path: &Path) -> bool {
    legacy_root().is_some_and(|root| path == root)
}

fn is_legacy_workspace_path(path: &Path, legacy_root: &Path) -> bool {
    if path == legacy_root {
        return true;
    }
    let Some(date) = path
        .strip_prefix(legacy_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|component| component.as_os_str().to_str())
    else {
        return false;
    };
    is_date_component(date)
}

fn is_date_component(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

//! One shared working directory for tasks not attached to a user project.
//! The daemon prepares it at startup; historical session directories stay put.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MAX_NUMBERED_CANDIDATES: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    pub cwd: PathBuf,
    pub workspace_root: PathBuf,
}

pub use fintwind_protocol::projectless::{
    home_directory, is_legacy_root_path, is_projectless_path, legacy_root, needs_migration,
    set_workspace_root, workspace_root,
};

pub fn configured_workspace_root(
    settings: &crate::DaemonSettings,
    data_directory: &Path,
) -> io::Result<PathBuf> {
    let root = settings
        .general_workspace_path()
        .map(PathBuf::from)
        .unwrap_or_else(|| data_directory.join("projects"));
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "general workspace must be an absolute directory path",
        ));
    }
    Ok(root)
}

pub fn prepare_workspace_root(
    settings: &crate::DaemonSettings,
    data_directory: &Path,
) -> io::Result<PathBuf> {
    let root = configured_workspace_root(settings, data_directory)?;
    ensure_real_directory(&root)?;
    Ok(root)
}

pub fn create_workspace(_prompt: Option<&str>) -> io::Result<Workspace> {
    let root = workspace_root().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "general workspace is unavailable")
    })?;
    ensure_workspace_in(&root)
}

fn ensure_workspace_in(root: &Path) -> io::Result<Workspace> {
    ensure_real_directory(root)?;
    Ok(Workspace {
        cwd: root.to_owned(),
        workspace_root: root.to_owned(),
    })
}

/// Move one old dated workspace from `~/.fintwind/<date>/<slug>` into
/// `~/.fintwind/projects/<date>/<slug>` without copying its contents through the
/// client. The oldest layout used `~/.fintwind` itself; that path contains Fintwind's
/// configuration now, so it receives the shared workspace instead of
/// moving the configuration directory.
pub fn migrate_workspace(path: &Path) -> io::Result<Workspace> {
    let root = legacy_root()
        .map(|root| root.join("projects"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not locate the home directory for ~/.fintwind/projects",
            )
        })?;
    migrate_workspace_in(&root, path)
}

fn migrate_workspace_in(root: &Path, path: &Path) -> io::Result<Workspace> {
    if path.starts_with(root) {
        validate_real_directory(path)?;
        return Ok(Workspace {
            cwd: path.to_owned(),
            workspace_root: root.to_owned(),
        });
    }
    let legacy_root = root.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid projectless workspace root",
        )
    })?;
    if path == legacy_root {
        return ensure_workspace_in(root);
    }

    let relative = path.strip_prefix(legacy_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a legacy projectless workspace",
        )
    })?;
    let components = relative.components().collect::<Vec<_>>();
    if components.len() != 2
        || components
            .iter()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || !is_date_component(components[0].as_os_str().to_string_lossy().as_ref())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a dated legacy projectless workspace",
        ));
    }
    validate_real_directory(path)?;
    ensure_real_directory(root)?;
    let date_directory = root.join(components[0].as_os_str());
    ensure_real_directory(&date_directory)?;
    let original_name = components[1].as_os_str().to_string_lossy();
    for index in 0..MAX_NUMBERED_CANDIDATES {
        let name = if index == 0 {
            original_name.to_string()
        } else {
            format!("{original_name}-{}", index + 1)
        };
        let destination = date_directory.join(name);
        if destination.exists() {
            continue;
        }
        match fs::rename(path, &destination) {
            Ok(()) => {
                return Ok(Workspace {
                    cwd: destination,
                    workspace_root: root.to_owned(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate a destination for the legacy projectless workspace",
    ))
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

fn ensure_real_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_real_directory(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            validate_real_directory(path)
        }
        Err(error) => Err(error),
    }
}

fn validate_real_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "projectless task directory must be a real directory",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_root() -> PathBuf {
        std::env::temp_dir().join(format!("fintwind-projectless-{}", Uuid::new_v4()))
    }

    #[test]
    fn migrates_legacy_workspace_with_contents_under_projects() {
        let legacy_root = test_root();
        let root = legacy_root.join("projects");
        let legacy = legacy_root.join("2026-08-08/fix-projectless-sessions");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("notes.txt"), "kept").unwrap();

        let migrated = migrate_workspace_in(&root, &legacy).unwrap();

        assert_eq!(
            migrated.cwd,
            root.join("2026-08-08/fix-projectless-sessions")
        );
        assert_eq!(
            fs::read_to_string(migrated.cwd.join("notes.txt")).unwrap(),
            "kept"
        );
        assert!(!legacy.exists());
        fs::remove_dir_all(&legacy_root).ok();
    }
}

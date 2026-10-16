//! Global OpenCode shell preference. All discovery and file access runs off-thread.
use std::{
    io,
    path::{Path, PathBuf},
};

use jsonc_parser::{
    ParseOptions,
    cst::{CstInputValue, CstRootNode},
};

#[derive(Clone, Debug)]
pub struct ShellOption {
    pub label: String,
    pub path: String,
}

/// JSONC has higher precedence than JSON in OpenCode's global directory.
pub fn config_path() -> PathBuf {
    let directory = std::env::var_os("OPENCODE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .filter(|value| !value.is_empty())
                .map(|value| PathBuf::from(value).join("opencode"))
        })
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join(".config/opencode")
        });
    let jsonc = directory.join("opencode.jsonc");
    if jsonc.exists() {
        jsonc
    } else {
        directory.join("opencode.json")
    }
}

fn read_document(path: &Path) -> io::Result<CstRootNode> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            "{\n  \"$schema\": \"https://opencode.ai/config.json\"\n}\n".into()
        }
        Err(error) => return Err(error),
    };
    let root = CstRootNode::parse(&text, &ParseOptions::default())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if root.object_value().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OpenCode config must be an object",
        ));
    }
    Ok(root)
}

pub fn load() -> io::Result<Option<String>> {
    let path = config_path();
    let selected = load_at(&path)?;
    if selected.is_none()
        && path
            .extension()
            .is_some_and(|extension| extension == "jsonc")
    {
        load_at(&path.with_extension("json"))
    } else {
        Ok(selected)
    }
}

pub fn save(shell: Option<&str>) -> io::Result<()> {
    let path = config_path();
    if shell.is_none()
        && path
            .extension()
            .is_some_and(|extension| extension == "jsonc")
    {
        let json = path.with_extension("json");
        if json.exists() {
            save_at(&json, None)?;
        }
    }
    save_at(&path, shell)
}

pub fn load_at(path: &Path) -> io::Result<Option<String>> {
    let root = read_document(path)?;
    Ok(root.to_serde_value().and_then(|value| {
        value
            .get("shell")
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    }))
}

/// Change only `shell`, retaining comments and unrelated formatting.
pub fn save_at(path: &Path, shell: Option<&str>) -> io::Result<()> {
    let root = read_document(path)?;
    let object = root.object_value().unwrap();
    // Remove all duplicate keys so an earlier value cannot reappear on reset.
    while let Some(property) = object.get("shell") {
        property.remove();
    }
    if let Some(shell) = shell {
        object.append("shell", CstInputValue::String(shell.to_owned()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, root.to_string())?;
    let result = std::fs::rename(&temporary, path);
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// A bounded list, not a disk scan. Never execute a candidate just to detect it.
pub fn detect() -> Vec<ShellOption> {
    let search = crate::command_env::executable_search_path();
    let mut directories = search
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    #[cfg(windows)]
    {
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(variable) {
                let base = PathBuf::from(base);
                directories.extend([
                    base.join("PowerShell/7"),
                    base.join("Git/bin"),
                    base.join("Programs/Git/bin"),
                ]);
            }
        }
    }
    #[cfg(windows)]
    let names = [
        ("PowerShell 7", "pwsh.exe"),
        ("Windows PowerShell", "powershell.exe"),
        ("CMD", "cmd.exe"),
        ("Git Bash", "bash.exe"),
    ];
    #[cfg(not(windows))]
    let names = [
        ("Zsh", "zsh"),
        ("Bash", "bash"),
        ("Fish", "fish"),
        ("PowerShell", "pwsh"),
    ];
    names
        .into_iter()
        .filter_map(|(label, name)| {
            directories
                .iter()
                .map(|directory| directory.join(name))
                .find(|path| {
                    if !path.is_file() {
                        return false;
                    }
                    // Windows' System32/bash.exe is a legacy WSL launcher, not Git Bash.
                    #[cfg(windows)]
                    if name == "bash.exe"
                        && !path
                            .components()
                            .any(|part| part.as_os_str().eq_ignore_ascii_case("Git"))
                    {
                        return false;
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if !std::fs::metadata(path)
                            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
                        {
                            return false;
                        }
                    }
                    true
                })
                .map(|path| ShellOption {
                    label: label.into(),
                    path: path.to_string_lossy().into_owned(),
                })
        })
        .collect()
}

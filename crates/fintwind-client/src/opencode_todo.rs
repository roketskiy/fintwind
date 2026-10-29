//! OpenCode's experimental todo tool switch (`experimental.todo`). While on,
//! agents get the `todowrite` tool and the session capsule can show the list
//! they write. File access follows [`crate::opencode_shell`]: JSONC wins over
//! JSON, and writes keep comments and unrelated formatting.
use std::{
    io,
    path::{Path, PathBuf},
};

use jsonc_parser::{
    ParseOptions,
    cst::{CstInputValue, CstRootNode},
};
use serde_json::Value;

pub fn config_path() -> PathBuf {
    crate::opencode_shell::config_path()
}

pub fn load() -> io::Result<bool> {
    load_at(&config_path())
}

pub fn save(enabled: bool) -> io::Result<()> {
    save_at(&config_path(), enabled)
}

pub fn load_at(path: &Path) -> io::Result<bool> {
    let root = read_document(path)?;
    Ok(root
        .to_serde_value()
        .as_ref()
        .and_then(|document| document.get("experimental"))
        .and_then(|experimental| experimental.get("todo"))
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

pub fn save_at(path: &Path, enabled: bool) -> io::Result<()> {
    let root = read_document(path)?;
    let object = root.object_value().unwrap();
    let experimental = object.object_value_or_set("experimental");
    // Remove every duplicate so an earlier value cannot reappear when the
    // new one is appended.
    while let Some(property) = experimental.get("todo") {
        property.remove();
    }
    experimental.append("todo", CstInputValue::Bool(enabled));
    write_document(path, &root)
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

fn write_document(path: &Path, root: &CstRootNode) -> io::Result<()> {
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

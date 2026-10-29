//! OpenCode's experimental image generation switch
//! (`experimental.image_generate`). The key accepts `true` or an object with
//! a default `model`; the UI stores `false` while off. File access follows
//! [`crate::opencode_shell`]: JSONC wins over JSON, and writes keep comments
//! and unrelated formatting.
use std::{
    io,
    path::{Path, PathBuf},
};

use jsonc_parser::{
    ParseOptions,
    cst::{CstInputValue, CstRootNode},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The switch and its optional default model, as OpenCode reads them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageGenerateConfig {
    pub enabled: bool,
    /// `provider/model`, optionally with `#variant`. `None` lets OpenCode
    /// choose an available image model per request.
    pub model: Option<String>,
}

impl ImageGenerateConfig {
    pub fn enabled(model: Option<String>) -> Self {
        Self {
            enabled: true,
            model: model.filter(|model| !model.trim().is_empty()),
        }
    }
}

pub fn config_path() -> PathBuf {
    crate::opencode_shell::config_path()
}

pub fn load() -> io::Result<ImageGenerateConfig> {
    load_at(&config_path())
}

pub fn save(config: &ImageGenerateConfig) -> io::Result<()> {
    save_at(&config_path(), config)
}

pub fn load_at(path: &Path) -> io::Result<ImageGenerateConfig> {
    let root = read_document(path)?;
    Ok(parse_setting(root.to_serde_value().as_ref()))
}

pub fn save_at(path: &Path, config: &ImageGenerateConfig) -> io::Result<()> {
    let root = read_document(path)?;
    let object = root.object_value().unwrap();
    let experimental = object.object_value_or_set("experimental");
    // Remove every duplicate so an earlier value cannot reappear when the
    // new one is appended.
    while let Some(property) = experimental.get("image_generate") {
        property.remove();
    }
    let value = match config.model.as_deref().map(str::trim) {
        Some(model) if config.enabled && !model.is_empty() => CstInputValue::Object(vec![(
            "model".into(),
            CstInputValue::String(model.to_owned()),
        )]),
        _ => CstInputValue::Bool(config.enabled),
    };
    experimental.append("image_generate", value);
    write_document(path, &root)
}

fn parse_setting(document: Option<&Value>) -> ImageGenerateConfig {
    let Some(entry) = document
        .and_then(|document| document.get("experimental"))
        .and_then(|experimental| experimental.get("image_generate"))
    else {
        return ImageGenerateConfig::default();
    };
    match entry {
        Value::Bool(true) => ImageGenerateConfig::enabled(None),
        Value::Object(object) => ImageGenerateConfig::enabled(
            object
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ),
        // `false`, `null`, and anything OpenCode would not accept read as off.
        _ => ImageGenerateConfig::default(),
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

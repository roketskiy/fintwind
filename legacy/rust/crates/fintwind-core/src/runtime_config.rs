use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, bail};
use fintwind_protocol::{RuntimeConfigDraft, RuntimeConfigScope, RuntimeConfigSnapshot};
use jsonc_parser::{
    ParseOptions,
    cst::{CstInputValue, CstObject, CstRootNode},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const IMAGE_DRAFT: &str = ".fintwind-image-generate.jsonc";
const TRANSACTION: &str = ".fintwind-runtime-transaction.json";

#[derive(Default)]
pub struct RuntimeConfigStore {
    gate: Mutex<()>,
}

impl RuntimeConfigStore {
    pub fn load(
        &self,
        scope: RuntimeConfigScope,
        directory: &Path,
    ) -> anyhow::Result<RuntimeConfigSnapshot> {
        let _guard = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        recover(directory)?;
        ConfigDocuments::read(directory)?.snapshot(scope)
    }

    pub fn save(
        &self,
        draft: &RuntimeConfigDraft,
        directory: &Path,
    ) -> anyhow::Result<(RuntimeConfigSnapshot, bool)> {
        let _guard = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        recover(directory)?;
        let docs = ConfigDocuments::read(directory)?;
        let current = docs.snapshot(draft.scope.clone())?;
        if current.revision != draft.expected_revision {
            return Ok((current, true));
        }
        validate_draft(draft)?;
        let (name, text) = docs.primary();
        let root = document(text)?;
        let object = root.object_value().unwrap();
        set_property(
            &object,
            "shell",
            draft
                .shell
                .as_ref()
                .map(|s| CstInputValue::String(s.clone())),
        );
        let experimental = object.object_value_or_set("experimental");
        set_property(
            &experimental,
            "todo",
            Some(CstInputValue::Bool(draft.todo_enabled)),
        );

        // OpenCode treats any image object as enabled. Disabled settings live
        // separately so toggling off does not destroy custom object fields.
        let image_root = if let Some(image) = experimental.object_value("image_generate") {
            document(Some(&image.to_string()))?
        } else {
            document(Some(docs.image.as_deref().unwrap_or("{}")))?
        };
        let image_object = image_root.object_value().unwrap();
        set_property(
            &image_object,
            "model",
            draft
                .image_model
                .as_ref()
                .map(|s| CstInputValue::String(s.clone())),
        );
        let image_text = image_root.to_string();
        let marker = uuid::Uuid::new_v4().to_string();
        if draft.image_generate_enabled {
            // A unique CST string slot lets us transplant the saved object's
            // exact syntax, including its comments and unknown number tokens.
            set_property(
                &experimental,
                "image_generate",
                Some(CstInputValue::String(marker.clone())),
            );
        } else {
            set_property(
                &experimental,
                "image_generate",
                Some(CstInputValue::Bool(false)),
            );
        }
        let mut primary_text = root.to_string();
        if draft.image_generate_enabled {
            primary_text = primary_text.replacen(&serde_json::to_string(&marker)?, &image_text, 1);
        }
        document(Some(&primary_text))?;
        let mut edits = Vec::new();
        if draft.shell.is_none() && name == "opencode.jsonc" {
            if let Some(json) = docs.json.as_deref() {
                let lower = document(Some(json))?;
                let lower_object = lower.object_value().unwrap();
                set_property(&lower_object, "shell", None);
                edits.push(FileEdit {
                    name: "opencode.json".into(),
                    before: Some(json.into()),
                    after: lower.to_string(),
                });
            }
        }
        edits.push(FileEdit {
            name: IMAGE_DRAFT.into(),
            before: docs.image.clone(),
            after: image_text,
        });
        edits.push(FileEdit {
            name: name.into(),
            before: text.map(str::to_owned),
            after: primary_text,
        });
        edits.retain(|edit| edit.before.as_deref() != Some(edit.after.as_str()));
        // External editors do not share this lock. Refuse a changed base before
        // recording the recoverable transaction instead of knowingly overwriting it.
        if ConfigDocuments::read(directory)?.revision() != current.revision {
            return Ok((
                ConfigDocuments::read(directory)?.snapshot(draft.scope.clone())?,
                true,
            ));
        }
        commit(directory, &edits)?;
        Ok((
            ConfigDocuments::read(directory)?.snapshot(draft.scope.clone())?,
            false,
        ))
    }
}

struct ConfigDocuments {
    json: Option<String>,
    jsonc: Option<String>,
    image: Option<String>,
}

impl ConfigDocuments {
    fn read(directory: &Path) -> anyhow::Result<Self> {
        let docs = Self {
            json: read_optional(&directory.join("opencode.json"))?,
            jsonc: read_optional(&directory.join("opencode.jsonc"))?,
            image: read_optional(&directory.join(IMAGE_DRAFT))?,
        };
        for text in [&docs.json, &docs.jsonc, &docs.image].into_iter().flatten() {
            document(Some(text))?;
        }
        Ok(docs)
    }
    fn primary(&self) -> (&str, Option<&str>) {
        if let Some(text) = self.jsonc.as_deref() {
            ("opencode.jsonc", Some(text))
        } else if let Some(text) = self.json.as_deref() {
            ("opencode.json", Some(text))
        } else {
            ("opencode.jsonc", None)
        }
    }
    fn revision(&self) -> String {
        let mut hash = Sha256::new();
        for text in [&self.json, &self.jsonc, &self.image] {
            hash.update([u8::from(text.is_some())]);
            if let Some(text) = text {
                hash.update((text.len() as u64).to_be_bytes());
                hash.update(text.as_bytes());
            }
        }
        format!("{:x}", hash.finalize())
    }
    fn snapshot(&self, scope: RuntimeConfigScope) -> anyhow::Result<RuntimeConfigSnapshot> {
        let (_, text) = self.primary();
        let value = document(text)?
            .to_serde_value()
            .context("invalid OpenCode configuration")?;
        let lower = document(self.json.as_deref())?
            .to_serde_value()
            .context("invalid fallback configuration")?;
        let shell = value
            .get("shell")
            .and_then(Value::as_str)
            .or_else(|| lower.get("shell").and_then(Value::as_str))
            .map(str::to_owned);
        let image = value.pointer("/experimental/image_generate");
        let enabled = image.is_some_and(|entry| entry == &Value::Bool(true) || entry.is_object());
        let saved = document(self.image.as_deref())?
            .to_serde_value()
            .context("invalid saved image configuration")?;
        let model = if enabled {
            image.and_then(|entry| entry.get("model"))
        } else {
            saved.get("model")
        }
        .and_then(Value::as_str)
        .map(str::to_owned);
        Ok(RuntimeConfigSnapshot {
            scope,
            revision: self.revision(),
            shell,
            todo_enabled: value
                .pointer("/experimental/todo")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            image_generate_enabled: enabled,
            image_model: model,
            image_models: Vec::new(),
        })
    }
}

fn document(text: Option<&str>) -> anyhow::Result<CstRootNode> {
    let root = CstRootNode::parse(
        text.unwrap_or("{\n  \"$schema\": \"https://opencode.ai/config.json\"\n}\n"),
        &ParseOptions::default(),
    )
    .context("invalid OpenCode JSONC configuration")?;
    let value = root
        .to_serde_value()
        .context("invalid OpenCode configuration")?;
    if !value.is_object() {
        bail!("OpenCode config must be an object");
    }
    if value.get("shell").is_some_and(|v| !v.is_string()) {
        bail!("invalid OpenCode shell");
    }
    if value.get("experimental").is_some_and(|v| !v.is_object()) {
        bail!("invalid OpenCode experimental settings");
    }
    if value
        .pointer("/experimental/todo")
        .is_some_and(|v| !v.is_boolean())
    {
        bail!("invalid OpenCode todo setting");
    }
    if value
        .pointer("/experimental/image_generate")
        .is_some_and(|v| !v.is_boolean() && !v.is_object())
    {
        bail!("invalid OpenCode image setting");
    }
    Ok(root)
}

fn validate_draft(draft: &RuntimeConfigDraft) -> anyhow::Result<()> {
    if let Some(shell) = &draft.shell {
        if shell.trim().is_empty() || shell.len() > 32768 || shell.contains(['\0', '\r', '\n']) {
            bail!("invalid shell executable path");
        }
    }
    if let Some(model) = &draft.image_model {
        let (provider, name) = model
            .split_once('/')
            .context("image model must use provider/model")?;
        if provider.is_empty()
            || name.is_empty()
            || model.len() > 4096
            || model.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            bail!("invalid image model");
        }
    }
    Ok(())
}

fn set_property(object: &CstObject, key: &str, value: Option<CstInputValue>) {
    while let Some(property) = object.get(key) {
        property.remove();
    }
    if let Some(value) = value {
        object.append(key, value);
    }
}

fn read_optional(path: &Path) -> anyhow::Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("OpenCode configuration must be a regular file")
        }
        Ok(metadata) if metadata.len() > 8 * 1024 * 1024 => {
            bail!("OpenCode configuration is too large")
        }
        Ok(_) => Ok(Some(
            fs::read_to_string(path).context("could not read OpenCode configuration")?,
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("could not inspect OpenCode configuration"),
    }
}

#[derive(Serialize, Deserialize)]
struct FileEdit {
    name: String,
    before: Option<String>,
    after: String,
}

fn commit(directory: &Path, edits: &[FileEdit]) -> anyhow::Result<()> {
    if edits.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(directory)?;
    atomic_write(&directory.join(TRANSACTION), &serde_json::to_vec(edits)?)?;
    recover(directory)
}

fn recover(directory: &Path) -> anyhow::Result<()> {
    let journal = directory.join(TRANSACTION);
    let Some(text) = read_optional(&journal)? else {
        return Ok(());
    };
    let edits: Vec<FileEdit> = serde_json::from_str(&text)
        .context("invalid pending OpenCode configuration transaction")?;
    if edits.len() > 3 {
        bail!("invalid pending OpenCode configuration transaction");
    }
    for edit in &edits {
        if !["opencode.json", "opencode.jsonc", IMAGE_DRAFT].contains(&edit.name.as_str()) {
            bail!("invalid pending OpenCode configuration target");
        }
        let current = read_optional(&directory.join(&edit.name))?;
        if current != edit.before && current.as_deref() != Some(edit.after.as_str()) {
            bail!(
                "OpenCode configuration changed during a pending save; resolve the pending transaction before editing"
            );
        }
    }
    for edit in edits {
        let path = directory.join(&edit.name);
        if read_optional(&path)?.as_deref() != Some(edit.after.as_str()) {
            atomic_write(&path, edit.after.as_bytes())?;
        }
    }
    fs::remove_file(journal).context("could not finalize OpenCode configuration save")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let temporary: PathBuf = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        if let Ok(metadata) = fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.context("could not atomically replace OpenCode configuration")
}

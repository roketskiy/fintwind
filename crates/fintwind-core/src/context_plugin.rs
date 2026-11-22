use anyhow::Context as _;
use fintwind_protocol::model::ContextComposition;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub(crate) struct ContextPlugin {
    directory: PathBuf,
}

const PRIVACY_PLUGIN: &str = "prompt-privacy";
const PRIVACY_RULES: &str = "prompt-privacy-rules.json";
const PRIVACY_DETECT: &str = "prompt-privacy-detect.ts";

impl ContextPlugin {
    pub(crate) fn create(parent: &Path) -> anyhow::Result<Self> {
        let directory = parent.join(format!("context-plugin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let write = (|| -> std::io::Result<()> {
            std::fs::write(
                directory.join("index.ts"),
                include_str!("../../../resources/opencode-context-plugin.ts"),
            )?;
            std::fs::create_dir(directory.join(PRIVACY_PLUGIN))?;
            std::fs::write(
                directory.join(PRIVACY_PLUGIN).join("index.ts"),
                include_str!("../../../resources/opencode-prompt-privacy-plugin.ts"),
            )?;
            std::fs::write(
                directory.join(PRIVACY_PLUGIN).join(PRIVACY_RULES),
                include_str!("../../../resources/prompt-privacy-rules.json"),
            )?;
            std::fs::write(
                directory.join(PRIVACY_PLUGIN).join(PRIVACY_DETECT),
                include_str!("../../../resources/prompt-privacy-detect.ts"),
            )
        })();
        if let Err(error) = write {
            let _ = std::fs::remove_file(directory.join("index.ts"));
            let _ = std::fs::remove_file(directory.join(PRIVACY_PLUGIN).join("index.ts"));
            let _ = std::fs::remove_file(directory.join(PRIVACY_PLUGIN).join(PRIVACY_RULES));
            let _ = std::fs::remove_file(directory.join(PRIVACY_PLUGIN).join(PRIVACY_DETECT));
            let _ = std::fs::remove_dir(directory.join(PRIVACY_PLUGIN));
            let _ = std::fs::remove_dir(&directory);
            return Err(error.into());
        }
        Ok(Self { directory })
    }

    pub(crate) fn config(&self, inherited: Option<&str>) -> anyhow::Result<String> {
        let inherited = inherited
            .map(str::to_owned)
            .or_else(|| std::env::var("OPENCODE_CONFIG_CONTENT").ok());
        let mut config: Value = match inherited.filter(|text| !text.trim().is_empty()) {
            Some(text) => serde_json::from_str(&text).context("invalid inline OpenCode config")?,
            None => json!({}),
        };
        let object = config
            .as_object_mut()
            .context("inline OpenCode config must be an object")?;
        let plugins = object
            .entry("plugins")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("inline OpenCode plugins must be an array")?;
        plugins.push(json!("-opencode.browser"));
        plugins.push(json!(self.directory));
        plugins.push(json!(self.directory.join(PRIVACY_PLUGIN)));
        // The check is read-only compilation with no side effects, so sitting
        // behind a per-call approval would only stall diagram turns.
        match object.entry("permission") {
            serde_json::map::Entry::Vacant(entry) => {
                entry.insert(json!({ "fintwind_mermaid_check": "allow" }));
            }
            serde_json::map::Entry::Occupied(mut entry) => {
                if let Some(permissions) = entry.get_mut().as_object_mut() {
                    permissions
                        .entry("fintwind_mermaid_check".to_owned())
                        .or_insert_with(|| json!("allow"));
                }
            }
        }
        Ok(serde_json::to_string(&config)?)
    }
}

impl Drop for ContextPlugin {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.directory.join("index.ts"));
        let _ = std::fs::remove_file(self.directory.join(PRIVACY_PLUGIN).join("index.ts"));
        let _ = std::fs::remove_file(self.directory.join(PRIVACY_PLUGIN).join(PRIVACY_RULES));
        let _ = std::fs::remove_file(self.directory.join(PRIVACY_PLUGIN).join(PRIVACY_DETECT));
        let _ = std::fs::remove_dir(self.directory.join(PRIVACY_PLUGIN));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

pub(crate) fn get(
    server: &crate::opencode_session::OpenCodeServer,
    directory: &str,
    session_id: &str,
) -> anyhow::Result<Option<ContextComposition>> {
    let response = server.request_for_directory_with_timeout(
        directory,
        "POST",
        "/api/rpc/fintwind.context/get",
        Some(&json!({ "input": { "sessionID": session_id } })),
        std::time::Duration::from_secs(5),
    )?;
    let output = response.get("output").cloned().unwrap_or(response);
    if output.is_null() {
        return Ok(None);
    }
    let composition: ContextComposition =
        serde_json::from_value(output).context("invalid context composition snapshot")?;
    Ok(Some(composition))
}

pub(crate) fn get_on_port(
    port: u16,
    directory: &str,
    session_id: &str,
) -> anyhow::Result<Option<ContextComposition>> {
    let response = crate::opencode_session::request_json_on_port_with_directory(
        port,
        "POST",
        "/api/rpc/fintwind.context/get",
        Some(&json!({ "input": { "sessionID": session_id } })),
        std::time::Duration::from_secs(5),
        Some(directory),
    )?;
    let output = response.get("output").cloned().unwrap_or(response);
    if output.is_null() {
        return Ok(None);
    }
    let composition =
        serde_json::from_value(output).context("invalid context composition snapshot")?;
    Ok(Some(composition))
}

fn private_references(text: &str) -> impl Iterator<Item = (usize, &str)> {
    const PREFIX: &str = "FINTWIND_SECRET_";
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    text.match_indices(PREFIX).filter_map(move |(start, _)| {
        let end = start + PREFIX.len() + 24;
        let suffix = text.as_bytes().get(start + PREFIX.len()..end)?;
        if start.checked_sub(1).is_some_and(|i| word(text.as_bytes()[i]))
            || text.as_bytes().get(end).copied().is_some_and(word)
            || !suffix.iter().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b))
        {
            return None;
        }
        Some((start, &text[start..end]))
    })
}

fn visit_output_text(value: &mut Value, visit: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(text) => visit(text),
        Value::Array(items) => items.iter_mut().for_each(|item| visit_output_text(item, visit)),
        Value::Object(fields) => fields.values_mut().for_each(|item| visit_output_text(item, visit)),
        _ => {}
    }
}

/// Restore presentation copies only. Never rewrite OpenCode history or call
/// this from a render path or for individual streaming deltas.
pub(crate) fn restore_output<'a>(
    port: u16,
    directory: Option<&str>,
    session_id: &str,
    fields: impl IntoIterator<Item = &'a mut Value>,
) {
    let mut fields = fields.into_iter().collect::<Vec<_>>();
    let mut references = BTreeSet::new();
    for field in &mut fields {
        visit_output_text(field, &mut |text| {
            references.extend(private_references(text).map(|(_, reference)| reference.to_owned()));
        });
    }
    if references.is_empty() {
        return;
    }
    let Ok(response) = crate::opencode_session::request_json_on_port_with_directory(
        port,
        "POST",
        "/api/rpc/fintwind.privacy/resolve",
        Some(&json!({ "input": { "sessionID": session_id, "references": references } })),
        std::time::Duration::from_millis(500),
        directory,
    ) else {
        return;
    };
    let output = response.get("output").cloned().unwrap_or(response);
    let Ok(originals) = serde_json::from_value::<HashMap<String, String>>(output) else {
        return;
    };
    if originals.is_empty() {
        return;
    }
    for field in fields {
        visit_output_text(field, &mut |text| {
            let mut cursor = 0;
            let mut restored = String::new();
            for (start, reference) in private_references(text) {
                if let Some(original) = originals.get(reference) {
                    restored.push_str(&text[cursor..start]);
                    restored.push_str(original);
                    cursor = start + reference.len();
                }
            }
            if cursor != 0 {
                restored.push_str(&text[cursor..]);
                *text = restored;
            }
        });
    }
}

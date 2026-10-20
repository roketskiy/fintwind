use anyhow::Context as _;
use fintwind_protocol::model::ContextComposition;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(crate) struct ContextPlugin {
    directory: PathBuf,
}

impl ContextPlugin {
    pub(crate) fn create(parent: &Path) -> anyhow::Result<Self> {
        let directory = parent.join(format!("context-plugin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        if let Err(error) = std::fs::write(
            directory.join("index.ts"),
            include_str!("../../../resources/opencode-context-plugin.ts"),
        ) {
            let _ = std::fs::remove_file(directory.join("index.ts"));
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
        object
            .entry("plugins")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("inline OpenCode plugins must be an array")?
            .push(json!(self.directory));
        Ok(serde_json::to_string(&config)?)
    }
}

impl Drop for ContextPlugin {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.directory.join("index.ts"));
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

/// Consume the plugin's server-side first-token latency for `session_id`, in
/// milliseconds. The plugin measures it between its `http.request` and
/// `http.response` hooks and answers `null` when the provider path never
/// produced a sample (WebSocket streaming, or hooks missing on this build).
pub(crate) fn first_token_on_port(
    port: u16,
    directory: &str,
    session_id: &str,
) -> anyhow::Result<Option<u64>> {
    let response = crate::opencode_session::request_json_on_port_with_directory(
        port,
        "POST",
        "/api/rpc/fintwind.context/ttft",
        Some(&json!({ "input": { "sessionID": session_id } })),
        std::time::Duration::from_secs(5),
        Some(directory),
    )?;
    let output = response.get("output").cloned().unwrap_or(response);
    if output.is_null() {
        return Ok(None);
    }
    let latency = output
        .as_u64()
        .context("invalid first-token latency sample")?;
    Ok(Some(latency))
}

//! Materialize the embedded plugin next to Fintwind's private serve data, not
//! inside a user workspace. The one owned file contains no credentials.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

const PLUGIN_SOURCE: &str = include_str!("../../../resources/opencode-browser-plugin.ts");
pub(crate) const ADDRESS_ENV: &str = "FINTWIND_BROWSER_TOOL_ADDRESS";
pub(crate) const TOKEN_ENV: &str = "FINTWIND_BROWSER_TOOL_TOKEN";

/// The public plugin list is a state query, not a blocking activation call.
/// Match this exact plugin in this location; an empty list is still loading.
pub(crate) fn wait_until_active(
    server: &crate::opencode_session::OpenCodeServer,
    directory: &str,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("private browser plugin activation timed out");
        }
        let response = server.request_for_directory_with_timeout(
            directory,
            "GET",
            "/api/plugin",
            None,
            remaining,
        )?;
        let plugins = response
            .get("data")
            .and_then(Value::as_array)
            .context("OpenCode returned an incompatible plugin state response")?;
        if let Some(plugin) = plugins
            .iter()
            .find(|plugin| plugin.get("id").and_then(Value::as_str) == Some("fintwind.browser"))
        {
            match plugin.pointer("/state/status").and_then(Value::as_str) {
                Some("active") => return Ok(()),
                Some("failed") => bail!("private browser plugin setup failed"),
                _ => {}
            }
        }
        // Background driver startup only; no polling reaches the UI thread.
        std::thread::sleep(
            Duration::from_millis(60).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

pub(crate) struct BrowserPlugin {
    directory: PathBuf,
    path: PathBuf,
}

impl BrowserPlugin {
    pub(crate) fn create(parent: &Path) -> anyhow::Result<Self> {
        let directory = parent.join(format!("browser-plugin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)
            .context("could not create the private browser plugin directory")?;
        let plugin = Self {
            path: directory.join("index.ts"),
            directory,
        };
        std::fs::write(&plugin.path, PLUGIN_SOURCE)
            .context("could not materialize the embedded browser plugin")?;
        Ok(plugin)
    }

    pub(crate) fn config(&self) -> anyhow::Result<String> {
        // Preserve an inherited JSON inline config. User/project files are
        // still loaded by OpenCode's normal config merge; never rewrite them.
        let mut config: Value = match std::env::var("OPENCODE_CONFIG_CONTENT") {
            Ok(content) if !content.trim().is_empty() => serde_json::from_str(&content)
                .context("inherited OPENCODE_CONFIG_CONTENT is not valid JSON")?,
            _ => json!({}),
        };
        let config = config
            .as_object_mut()
            .context("inline OpenCode config must be an object")?;
        let plugins = config.entry("plugins").or_insert_with(|| json!([]));
        let plugins = plugins
            .as_array_mut()
            .context("inline OpenCode plugins must be an array")?;
        if plugins.len() > 256 {
            bail!("inline OpenCode config has too many plugins");
        }
        // v2.0.16 accepts configured plugin directories, not raw .ts paths.
        // Automatic .opencode/plugins discovery accepts files, but this is
        // deliberately outside every user's project/config discovery root.
        plugins.push(json!(self.directory));
        Ok(serde_json::to_string(config)?)
    }
}

impl Drop for BrowserPlugin {
    fn drop(&mut self) {
        // Only the two paths created above, never a recursive user directory.
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

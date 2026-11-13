//! Materialize the suppression-only plugin next to Fintwind's private serve
//! data, not inside a user workspace. The one owned file registers no tools
//! and carries no credentials: it only removes OpenCode's built-in desktop
//! browser tools and repeats the in-app browser instruction when the browser
//! MCP is present.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

const PLUGIN_SOURCE: &str = include_str!("../../../resources/opencode-browser-suppress-plugin.ts");

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

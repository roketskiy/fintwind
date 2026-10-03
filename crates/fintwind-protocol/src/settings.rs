use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct DaemonSettings {
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            extra: BTreeMap::new(),
        }
    }
}

impl DaemonSettings {
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(".fintwind")
            .join("settings.json")
    }

    pub fn discard_legacy_app_keys(&mut self) {
        for key in [
            "analytics_enabled",
            "favorite_models",
            "theme",
            "language",
            "computer_use_enabled",
            "computer_use_allowed_apps",
        ] {
            self.extra.remove(key);
        }
    }

    /// Whether the daemon lets the browser plugin expose its tools to models.
    /// The default is off: browser tools cost model context, so enabling them
    /// is an explicit act. A missing or non-boolean value reads as off.
    pub fn browser_tools_enabled(&self) -> bool {
        self.extra
            .get(BROWSER_TOOLS_ENABLED_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

pub const BROWSER_TOOLS_ENABLED_KEY: &str = "browser_tools_enabled";

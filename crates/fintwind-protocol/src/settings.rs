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
    pub fn general_workspace_path(&self) -> Option<&str> {
        self.extra
            .get(GENERAL_WORKSPACE_PATH_KEY)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
    }

    pub fn opencode_binary_path(&self) -> Option<&str> {
        self.extra
            .get(OPENCODE_BINARY_PATH_KEY)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
    }

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
            "auto_compact_threshold_tokens",
        ] {
            self.extra.remove(key);
        }
    }

    pub fn scheduled_tools_enabled(&self) -> bool {
        self.extra
            .get(SCHEDULED_TOOLS_ENABLED_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(false)
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

    /// The HTTP proxy address (for example `http://127.0.0.1:7897`) that
    /// Fintwind's own fetches and `opencode serve` children ride through.
    /// A missing, empty, or non-string value reads as "no proxy".
    pub fn network_proxy_url(&self) -> Option<&str> {
        self.extra
            .get(NETWORK_PROXY_URL_KEY)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
    }

    pub fn auto_compact_enabled(&self) -> bool {
        self.extra
            .get(AUTO_COMPACT_ENABLED_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    pub fn background_compact_enabled(&self) -> bool {
        self.extra
            .get(BACKGROUND_COMPACT_ENABLED_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    pub fn watchdog_enabled(&self) -> bool {
        self.extra
            .get(WATCHDOG_ENABLED_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    pub fn auto_compact_threshold_percent(&self) -> u8 {
        self.extra
            .get(AUTO_COMPACT_THRESHOLD_PERCENT_KEY)
            .and_then(Value::as_u64)
            .filter(|percent| (1..=99).contains(percent))
            .map(|percent| percent as u8)
            .unwrap_or(DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT)
    }
}

pub const SCHEDULED_TOOLS_ENABLED_KEY: &str = "scheduled_tools_enabled";
pub const OPENCODE_BINARY_PATH_KEY: &str = "opencode_binary_path";
pub const GENERAL_WORKSPACE_PATH_KEY: &str = "general_workspace_path";
pub const BROWSER_TOOLS_ENABLED_KEY: &str = "browser_tools_enabled";
pub const NETWORK_PROXY_URL_KEY: &str = "network_proxy_url";
pub const AUTO_COMPACT_ENABLED_KEY: &str = "auto_compact_enabled";
pub const WATCHDOG_ENABLED_KEY: &str = "watchdog_enabled";
pub const BACKGROUND_COMPACT_ENABLED_KEY: &str = "background_compact_enabled";
pub const AUTO_COMPACT_THRESHOLD_PERCENT_KEY: &str = "auto_compact_threshold_percent";
pub const DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT: u8 = 85;

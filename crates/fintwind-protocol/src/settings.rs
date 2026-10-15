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

    pub fn goal_enabled(&self) -> bool {
        self.extra
            .get(GOAL_ENABLED_KEY)
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

    /// Whether the client compacts a session whose live context occupancy
    /// reaches [`AUTO_COMPACT_THRESHOLD_TOKENS_DEFAULT`]. On by default: the
    /// provider's own near-limit auto compaction stays armed as the net, so
    /// this only moves compression earlier.
    pub fn auto_compact_enabled(&self) -> bool {
        self.extra
            .get(AUTO_COMPACT_ENABLED_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// Live context tokens at which the client requests a compaction, for
    /// the foreground session and every live subagent alike. Absolute, not a
    /// percentage: windows smaller than this never reach it and stay on the
    /// provider's own auto compaction. Clamped to a sane range; a missing or
    /// non-numeric value reads as the default.
    pub fn auto_compact_threshold_tokens(&self) -> u64 {
        self.extra
            .get(AUTO_COMPACT_THRESHOLD_TOKENS_KEY)
            .and_then(Value::as_u64)
            .map(|tokens| {
                tokens.clamp(
                    AUTO_COMPACT_THRESHOLD_TOKENS_MIN,
                    AUTO_COMPACT_THRESHOLD_TOKENS_MAX,
                )
            })
            .unwrap_or(AUTO_COMPACT_THRESHOLD_TOKENS_DEFAULT)
    }
}

pub const GOAL_ENABLED_KEY: &str = "goal_enabled";
pub const BROWSER_TOOLS_ENABLED_KEY: &str = "browser_tools_enabled";
pub const NETWORK_PROXY_URL_KEY: &str = "network_proxy_url";
pub const AUTO_COMPACT_ENABLED_KEY: &str = "auto_compact_enabled";
pub const AUTO_COMPACT_THRESHOLD_TOKENS_KEY: &str = "auto_compact_threshold_tokens";
/// Default trigger: half of a 1M window. Smaller windows never reach it and
/// stay on the provider's own near-limit auto compaction, which Fintwind
/// never reconfigures.
pub const AUTO_COMPACT_THRESHOLD_TOKENS_DEFAULT: u64 = 500_000;
pub const AUTO_COMPACT_THRESHOLD_TOKENS_MIN: u64 = 10_000;
pub const AUTO_COMPACT_THRESHOLD_TOKENS_MAX: u64 = 100_000_000;

//! User-configured model providers.
//!
//! A custom provider is an entry in OpenCode's own configuration file
//! (`provider.<key>` — base URL, API format, key, and model list) managed
//! from the Providers settings page. The app keeps no parallel store: the
//! working roster is loaded from, and committed back to, OpenCode's
//! configuration by [`crate::opencode_config`], so entries created with the
//! CLI and entries created here are the same data.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::persistence::StateStore;

/// Wire protocol a custom provider speaks. Selects the OpenCode SDK package
/// that adapts the endpoint.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProviderApiFormat {
    #[default]
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
}

impl ProviderApiFormat {
    pub const ALL: [ProviderApiFormat; 2] = [Self::OpenAi, Self::Anthropic];

    /// The OpenCode provider package that adapts this protocol.
    pub fn npm_package(self) -> &'static str {
        match self {
            Self::OpenAi => "@ai-sdk/openai-compatible",
            Self::Anthropic => "@ai-sdk/anthropic",
        }
    }
}

/// One model of a [`CustomProvider`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomProviderModel {
    pub id: String,
    /// Context window in tokens, when the user recorded one. Purely
    /// informational in the UI; stored on the model as `limit.context`.
    pub context_window: Option<u64>,
}

impl Default for CustomProviderModel {
    fn default() -> Self {
        Self {
            id: String::new(),
            context_window: None,
        }
    }
}

/// A user-defined provider: a custom API endpoint and its models.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomProvider {
    /// The stable OpenCode configuration key. Also the UI's id, so sessions
    /// naming `<key>/<model-id>` survive renames and edits.
    pub id: String,
    /// Same as [`Self::id`]; kept so older call sites read naturally.
    pub slug: String,
    pub name: String,
    pub base_url: String,
    pub api_format: ProviderApiFormat,
    pub api_key: String,
    pub enabled: bool,
    pub models: Vec<CustomProviderModel>,
    /// The provider entry exactly as read from opencode.json. A save starts
    /// from it and rewrites only the keys the UI owns, so unknown fields
    /// (cost tables, `whitelist`, `options.setCacheKey`, …) survive.
    #[serde(skip)]
    pub raw: Value,
    /// Set when the user explicitly picked an API format. Only then does a
    /// save rewrite `npm`; otherwise an unrecognized original package (or a
    /// deliberately absent one, like models.dev providers) is left as-is.
    #[serde(skip)]
    pub npm_touched: bool,
}

impl CustomProvider {
    pub fn model(&self, id: &str) -> Option<&CustomProviderModel> {
        self.models.iter().find(|model| model.id == id)
    }

    /// A freshly created entry: everything the add form collected, with an
    /// empty raw record so the first save writes the standard fields.
    pub fn new(slug: String, name: String, base_url: String, api_format: ProviderApiFormat, api_key: String, models: Vec<CustomProviderModel>) -> Self {
        Self {
            id: slug.clone(),
            slug,
            name,
            base_url,
            api_format,
            api_key,
            enabled: true,
            models,
            raw: Value::Null,
            npm_touched: true,
        }
    }
}

/// Where the pre-sync design mirrored its provider roster (beside the app
/// state database). Startup migrates that file into OpenCode's real
/// configuration once, then removes it; see
/// [`crate::opencode_config::migrate_legacy_override_file`].
pub fn legacy_override_path() -> PathBuf {
    StateStore::default_path().with_file_name("opencode-providers.json")
}

/// Derive a stable OpenCode configuration key from a provider name:
/// lowercased ASCII alphanumerics joined by dashes, trimmed to a readable
/// length. An empty name still yields a valid key.
pub fn provider_slug(name: &str) -> String {
    let slug: String = name
        .chars()
        .filter_map(|char| {
            if char.is_ascii_alphanumeric() {
                Some(char.to_ascii_lowercase())
            } else if char.is_whitespace() || char == '-' || char == '_' || char == '.' {
                Some('-')
            } else {
                None
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_owned();
    let mut slug: String = slug.chars().take(40).collect();
    if slug.trim_matches('-').is_empty() {
        slug.clear();
        slug.push('p');
    } else if slug.ends_with('-') {
        slug.truncate(slug.trim_end_matches('-').len());
    }
    slug
}

/// Make `slug` unique within `taken` by appending `-2`, `-3`, …
pub fn unique_provider_slug(name: &str, taken: &[String]) -> String {
    let base = provider_slug(name);
    if !taken.iter().any(|slug| slug == &base) {
        return base;
    }
    for index in 2.. {
        let candidate = format!("{base}-{index}");
        if !taken.iter().any(|slug| slug == &candidate) {
            return candidate;
        }
    }
    unreachable!("an infinite candidate stream always finds a free slug")
}

/// `"1000000"`, `"1M"`, `"128k"` → tokens. Plain numbers are accepted with or
/// without thousands separators; suffixes are case-insensitive.
pub fn parse_context_window(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (number, multiplier) = match text.chars().last()? {
        digit if digit.is_ascii_digit() => (text, 1),
        'k' | 'K' => (&text[..text.len() - 1], 1_000),
        'm' | 'M' => (&text[..text.len() - 1], 1_000_000),
        _ => return None,
    };
    let number: String = number.chars().filter(|char| *char != ',').collect();
    number.parse::<u64>().ok().map(|value| value * multiplier)
}

/// Compact badge text: whole millions as `N M`, whole thousands as `N K`,
/// anything else verbatim.
pub fn format_context_window(tokens: u64) -> String {
    if tokens >= 1_000_000 && tokens % 1_000_000 == 0 {
        format!("{}M", tokens / 1_000_000)
    } else if tokens >= 1_000 && tokens % 1_000 == 0 {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_flat_and_unique() {
        assert_eq!(provider_slug("DeepSeek 官方"), "deepseek");
        assert_eq!(provider_slug("  -- My_Provider.v2  "), "my-provider-v2");
        assert_eq!(provider_slug("+++"), "p");

        let taken = vec!["deepseek".to_owned()];
        assert_eq!(unique_provider_slug("DeepSeek", &taken), "deepseek-2");
        let mut taken = taken;
        taken.push("deepseek-2".to_owned());
        assert_eq!(unique_provider_slug("DeepSeek", &taken), "deepseek-3");
        assert_eq!(unique_provider_slug("Other", &taken), "other");
    }

    #[test]
    fn context_windows_parse_and_format() {
        assert_eq!(parse_context_window("1000000"), Some(1_000_000));
        assert_eq!(parse_context_window("1,000,000"), Some(1_000_000));
        assert_eq!(parse_context_window("1M"), Some(1_000_000));
        assert_eq!(parse_context_window("128k"), Some(128_000));
        assert_eq!(parse_context_window(" 2m "), Some(2_000_000));
        assert_eq!(parse_context_window(""), None);
        assert_eq!(parse_context_window("abc"), None);
        assert_eq!(parse_context_window("1T"), None);

        assert_eq!(format_context_window(1_000_000), "1M");
        assert_eq!(format_context_window(2_000_000), "2M");
        assert_eq!(format_context_window(128_000), "128K");
        assert_eq!(format_context_window(262_144), "262144");
        assert_eq!(format_context_window(500), "500");
    }
}

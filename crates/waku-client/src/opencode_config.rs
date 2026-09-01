//! OpenCode's own configuration file is the single source of truth for model
//! providers.
//!
//! The Providers settings page loads its working roster from, and commits
//! edits straight back to, OpenCode's global configuration —
//! `~/.config/opencode/opencode.json` (the same file the CLI and TUI read).
//! The app keeps no parallel store, so a provider added with `opencode2
//! auth`-managed credentials or by hand in an editor shows up here and vice
//! versa. OpenCode watches the file and hot-reloads, so a commit reaches
//! running `opencode2 serve` processes without a restart.
//!
//! Saves are deliberately conservative: the document is read fresh, only the
//! `provider` map is regenerated from the working roster, the top-level
//! `disabled_providers` list is reconciled with the roster's enabled flags,
//! and per-provider unknown fields (`$schema`, `mcp`, `skills`, cost tables,
//! `options.setCacheKey`, …) are carried over untouched. The one lossy case
//! is a provider entry edited in an editor between a page load and a commit
//! — the roster read at page open wins.
//!
//! Verified against `opencode2` 0.0.0-beta-18743:
//! - a model `limit` without `output` fails validation and silently drops
//!   the WHOLE provider from the catalog, so a fresh `limit` is always
//!   written with both keys (output defaults to OpenCode's own 32 000);
//! - `limit.context` is what `/api/model` reports for context windows;
//! - `disabled_providers` hides a config-defined provider's models.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::custom_providers::{CustomProvider, CustomProviderModel, ProviderApiFormat};

/// OpenCode's output-token default, applied when a fresh `limit` is written.
/// Matches what OpenCode itself reports for models without a configured
/// limit (verified via `/api/model`).
const DEFAULT_OUTPUT_LIMIT: u64 = 32_000;

/// Where OpenCode's global configuration lives. `OPENCODE_CONFIG_DIR` wins
/// when set, mirroring the CLI; otherwise the XDG-style path OpenCode uses
/// on every platform, including Windows.
pub fn config_path() -> PathBuf {
    if let Some(directory) = std::env::var_os("OPENCODE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|directory| !directory.as_os_str().is_empty())
    {
        return directory.join("opencode.json");
    }
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".config")
        .join("opencode")
        .join("opencode.json")
}

/// Load the user-configured providers from OpenCode's configuration. Every
/// entry under `provider` is returned — the file holds no built-in catalog,
/// so its entries are exactly what the UI presents as editable providers.
pub fn load_providers() -> io::Result<Vec<CustomProvider>> {
    load_providers_at(&config_path())
}

pub fn load_providers_at(path: &Path) -> io::Result<Vec<CustomProvider>> {
    let bytes = std::fs::read(path)?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let disabled: HashSet<String> = document
        .get("disabled_providers")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let providers = document
        .get("provider")
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .map(|(key, entry)| provider_from_config(key, entry, &disabled))
                .collect()
        })
        .unwrap_or_default();
    Ok(providers)
}

fn provider_from_config(key: &str, entry: &Value, disabled: &HashSet<String>) -> CustomProvider {
    let npm = entry.get("npm").and_then(Value::as_str);
    let api_format = match npm {
        Some("@ai-sdk/anthropic") => ProviderApiFormat::Anthropic,
        _ => ProviderApiFormat::OpenAi,
    };
    let options = entry.get("options");
    let models = entry
        .get("models")
        .and_then(Value::as_object)
        .map(|models| {
            models
                .iter()
                .map(|(id, spec)| CustomProviderModel {
                    id: id.clone(),
                    context_window: spec
                        .pointer("/limit/context")
                        .and_then(Value::as_u64)
                        .or_else(|| spec.get("contextWindow").and_then(Value::as_u64)),
                })
                .collect()
        })
        .unwrap_or_default();
    CustomProvider {
        id: key.to_owned(),
        slug: key.to_owned(),
        name: entry
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or(key)
            .to_owned(),
        base_url: options
            .and_then(|options| options.get("baseURL"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        api_format,
        api_key: options
            .and_then(|options| options.get("apiKey"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        enabled: !disabled.contains(key),
        models,
        raw: entry.clone(),
        npm_touched: false,
    }
}

/// Commit the working roster to OpenCode's configuration. The `provider` map
/// is rebuilt from `providers` (per-entry unknown fields ride along in
/// [`CustomProvider::raw`]); everything outside it is preserved, and the
/// `disabled_providers` list keeps entries that name providers outside the
/// roster (the CLI can disable catalog providers the UI never manages).
pub fn save_providers(providers: &[CustomProvider]) -> io::Result<()> {
    save_providers_at(&config_path(), providers)
}

pub fn save_providers_at(path: &Path, providers: &[CustomProvider]) -> io::Result<()> {
    let mut document: Map<String, Value> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        // No file yet: start a minimal document with OpenCode's schema pin.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut document = Map::new();
            document.insert(
                "$schema".into(),
                Value::String("https://opencode.ai/config.json".into()),
            );
            document
        }
        Err(error) => return Err(error),
    };

    let managed: HashSet<&str> = providers.iter().map(|provider| provider.slug.as_str()).collect();

    let mut provider_map = Map::new();
    for provider in providers {
        provider_map.insert(provider.slug.clone(), provider_entry(provider));
    }
    document.insert("provider".into(), Value::Object(provider_map));

    // Reconcile `disabled_providers`: entries outside the roster survive
    // (catalog providers disabled via the CLI), roster entries follow their
    // enabled flag, and an empty list drops the key entirely.
    let mut disabled: Vec<String> = document
        .get("disabled_providers")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .filter(|key| !managed.contains(key))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    for provider in providers {
        if !provider.enabled {
            disabled.push(provider.slug.clone());
        }
    }
    if disabled.is_empty() {
        document.remove("disabled_providers");
    } else {
        document.insert("disabled_providers".into(), Value::from(disabled));
    }

    write_json_atomically(path, &Value::Object(document))
}

/// The provider entry as it should sit in the configuration: the original
/// record with only the UI-owned keys rewritten.
fn provider_entry(provider: &CustomProvider) -> Value {
    let mut entry = match provider.raw.as_object() {
        Some(raw) => raw.clone(),
        None => Map::new(),
    };

    if provider.name.is_empty() || provider.name == provider.slug {
        // A display name equal to the key carries no information; dropping
        // it lets OpenCode fall back to the key the same way the loader did.
        entry.remove("name");
    } else {
        entry.insert("name".into(), Value::String(provider.name.clone()));
    }

    // `npm` is only rewritten when the user explicitly picked a format.
    // An untouched entry keeps its original package — including the
    // deliberate absence of one (models.dev-style providers resolve without
    // a package and must not gain one behind their back).
    if provider.npm_touched {
        entry.insert(
            "npm".into(),
            Value::String(provider.api_format.npm_package().to_owned()),
        );
    }

    let mut options = entry
        .get("options")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    set_or_remove_string(&mut options, "baseURL", &provider.base_url);
    set_or_remove_string(&mut options, "apiKey", &provider.api_key);
    if options.is_empty() {
        entry.remove("options");
    } else {
        entry.insert("options".into(), Value::Object(options));
    }

    let raw_models = provider
        .raw
        .get("models")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut models = Map::new();
    for model in &provider.models {
        let mut spec = raw_models
            .get(&model.id)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        // OpenCode's model display name; entries recorded without one show
        // the model id, so only fill it in when missing.
        spec.entry("name".to_owned())
            .or_insert_with(|| Value::String(model.id.clone()));
        // `limit.context` carries the recorded context window. OpenCode's
        // schema requires `output` beside it — a context-only `limit`
        // invalidates the whole provider (verified: the entry silently
        // vanishes from the catalog) — so a fresh limit is written complete.
        match model.context_window {
            Some(window) => {
                let mut limit = spec
                    .get("limit")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                limit.insert("context".into(), Value::from(window));
                limit
                    .entry("output".to_owned())
                    .or_insert_with(|| Value::from(DEFAULT_OUTPUT_LIMIT));
                spec.insert("limit".into(), Value::Object(limit));
            }
            None => {
                if let Some(limit) = spec
                    .get("limit")
                    .and_then(Value::as_object)
                    .cloned()
                    .map(|mut limit| {
                        limit.remove("context");
                        limit
                    })
                {
                    if limit.is_empty() {
                        spec.remove("limit");
                    } else {
                        spec.insert("limit".into(), Value::Object(limit));
                    }
                }
            }
        }
        // The pre-OpenCode-config mirror wrote `contextWindow`; superseded
        // by `limit.context` and never read back once `limit` exists.
        spec.remove("contextWindow");
        models.insert(model.id.clone(), Value::Object(spec));
    }
    entry.insert("models".into(), Value::Object(models));

    Value::Object(entry)
}

fn set_or_remove_string(map: &mut Map<String, Value>, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        map.remove(key);
    } else {
        map.insert(key.into(), Value::String(value.to_owned()));
    }
}

fn write_json_atomically(path: &Path, document: &Value) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(document)?;
    bytes.push(b'\n');
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

/// One-time migration from the pre-sync design: the app used to mirror its
/// own provider roster into an `opencode-providers.json` file beside the app
/// state database and point `OPENCODE_CONFIG` at it. Those entries are
/// upserted into OpenCode's real configuration (by key), after which the
/// mirror file is removed. Idempotent: a missing or unreadable mirror is a
/// no-op.
pub fn migrate_legacy_override_file() -> bool {
    let legacy = crate::custom_providers::legacy_override_path();
    let Ok(bytes) = std::fs::read(&legacy) else {
        return false;
    };
    let migrated = serde_json::from_slice::<Value>(&bytes).ok().and_then(|document| {
        let entries = document.get("provider")?.as_object()?.clone();
        if entries.is_empty() {
            return Some(Vec::new());
        }
        let providers: Vec<CustomProvider> = entries
            .iter()
            .map(|(key, entry)| provider_from_config(key, entry, &HashSet::new()))
            .collect();
        Some(providers)
    });
    let migrated = match migrated {
        Some(providers) => save_providers_at(&config_path(), &providers).is_ok(),
        None => false,
    };
    if migrated {
        let _ = std::fs::remove_file(&legacy);
    }
    migrated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom_providers::{format_context_window, parse_context_window};

    fn sample_document() -> Value {
        serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": {"context7": {"type": "remote", "url": "https://mcp.example.com", "enabled": true}},
            "skills": {"paths": ["C:/Users/example/.config/opencode/skills"]},
            "disabled_providers": ["catalog-provider"],
            "provider": {
                "deepseek": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "DeepSeek",
                    "whitelist": ["deepseek-chat"],
                    "options": {"apiKey": "sk-test", "baseURL": "https://api.deepseek.com", "setCacheKey": true},
                    "models": {
                        "deepseek-chat": {"name": "DeepSeek Chat", "limit": {"context": 128000, "output": 8192}},
                        "deepseek-reasoner": {"name": "DeepSeek Reasoner", "limit": {"context": 128000, "output": 8192}, "cost": {"input": 0.5, "output": 2.0}}
                    }
                },
                "openrouter": {
                    "options": {"apiKey": "sk-or"},
                    "models": {"vendor/model": {"name": "Vendor Model"}}
                },
                "catalog-provider": {
                    "models": {"some-model": {"name": "Some Model"}}
                }
            }
        })
    }

    fn write_fixture(directory: &Path, document: &Value) -> PathBuf {
        std::fs::create_dir_all(directory).unwrap();
        let path = directory.join("opencode.json");
        std::fs::write(&path, serde_json::to_vec_pretty(document).unwrap()).unwrap();
        path
    }

    #[test]
    fn load_reads_roster_with_enabled_flags_and_context_windows() {
        let directory =
            std::env::temp_dir().join(format!("waku-oc-cfg-load-{}", std::process::id()));
        let path = write_fixture(&directory, &sample_document());

        let providers = load_providers_at(&path).unwrap();
        assert_eq!(providers.len(), 3);

        let deepseek = providers.iter().find(|p| p.slug == "deepseek").unwrap();
        assert_eq!(deepseek.name, "DeepSeek");
        assert_eq!(deepseek.base_url, "https://api.deepseek.com");
        assert_eq!(deepseek.api_key, "sk-test");
        assert_eq!(deepseek.api_format, ProviderApiFormat::OpenAi);
        assert!(deepseek.enabled);
        assert_eq!(deepseek.models.len(), 2);
        assert_eq!(
            deepseek.model("deepseek-chat").unwrap().context_window,
            Some(128000)
        );
        assert_eq!(deepseek.npm_touched, false);

        // An entry without npm loads as OpenAI-compatible without touching
        // the original package choice.
        let openrouter = providers.iter().find(|p| p.slug == "openrouter").unwrap();
        assert_eq!(openrouter.api_format, ProviderApiFormat::OpenAi);
        assert_eq!(openrouter.base_url, "");
        assert!(openrouter.enabled);

        // `disabled_providers` marks the entry disabled.
        let disabled = providers
            .iter()
            .find(|p| p.slug == "catalog-provider")
            .unwrap();
        assert!(!disabled.enabled);

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn save_preserves_unknown_keys_and_reconciles_disabled_providers() {
        let directory =
            std::env::temp_dir().join(format!("waku-oc-cfg-save-{}", std::process::id()));
        let path = write_fixture(&directory, &sample_document());
        let mut providers = load_providers_at(&path).unwrap();

        // Rename + disable deepseek, drop one model, edit a context window.
        let deepseek = providers
            .iter_mut()
            .find(|provider| provider.slug == "deepseek")
            .unwrap();
        deepseek.name = "DeepSeek 官方".into();
        deepseek.enabled = false;
        deepseek.models.remove(1);
        deepseek.models[0].context_window = Some(1_000_000);
        // Delete openrouter entirely, add a fresh provider.
        providers.retain(|provider| provider.slug != "openrouter");
        providers.push(CustomProvider::new(
            "my-relay".into(),
            "My Relay".into(),
            "https://relay.example.com/v1".into(),
            ProviderApiFormat::Anthropic,
            "sk-relay".into(),
            vec![CustomProviderModel {
                id: "relay-large".into(),
                context_window: Some(200_000),
            }],
        ));

        save_providers_at(&path, &providers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

        // Unrelated top-level keys survive verbatim.
        assert_eq!(saved["mcp"]["context7"]["url"], "https://mcp.example.com");
        assert_eq!(saved["skills"]["paths"][0], "C:/Users/example/.config/opencode/skills");
        // The disabled catalog provider outside the roster stays listed.
        assert_eq!(saved["disabled_providers"], serde_json::json!(["catalog-provider", "deepseek"]));

        // The renamed, disabled, edited entry kept its unknown fields.
        let deepseek = &saved["provider"]["deepseek"];
        assert_eq!(deepseek["name"], "DeepSeek 官方");
        assert_eq!(deepseek["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(deepseek["whitelist"], serde_json::json!(["deepseek-chat"]));
        assert_eq!(deepseek["options"]["setCacheKey"], true);
        assert_eq!(deepseek["options"]["apiKey"], "sk-test");
        assert_eq!(
            deepseek["models"]["deepseek-chat"]["limit"],
            serde_json::json!({"context": 1000000, "output": 8192})
        );
        assert!(deepseek["models"].get("deepseek-reasoner").is_none());

        // The deleted entry is gone.
        assert!(saved["provider"].get("openrouter").is_none());

        // The fresh entry carries a complete npm/options/models shape, and a
        // fresh `limit` is written with an output beside the context.
        let relay = &saved["provider"]["my-relay"];
        assert_eq!(relay["npm"], "@ai-sdk/anthropic");
        assert_eq!(relay["options"]["baseURL"], "https://relay.example.com/v1");
        assert_eq!(relay["options"]["apiKey"], "sk-relay");
        assert_eq!(
            relay["models"]["relay-large"]["limit"],
            serde_json::json!({"context": 200000, "output": 32000})
        );

        // Round-trip: the saved file loads back to the same roster — the two
        // edited/added entries plus the roster's remaining catalog entry.
        let reloaded = load_providers_at(&path).unwrap();
        assert_eq!(reloaded.len(), 3);
        let deepseek = reloaded.iter().find(|p| p.slug == "deepseek").unwrap();
        assert!(!deepseek.enabled);
        assert_eq!(deepseek.name, "DeepSeek 官方");

        // Re-enabling the roster's own entries (leaving the catalog entry
        // disabled) re-enables deepseek and drops an empty roster disable
        // set; the outside entry survives alone in the list.
        let mut providers = reloaded;
        for provider in &mut providers {
            if provider.slug != "catalog-provider" {
                provider.enabled = true;
            }
        }
        save_providers_at(&path, &providers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // The roster entry left the list; the catalog entry survives alone.
        assert_eq!(saved["disabled_providers"], serde_json::json!(["catalog-provider"]));

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn save_preserves_absent_npm_and_clearing_a_context_window_keeps_output() {
        let directory =
            std::env::temp_dir().join(format!("waku-oc-cfg-npm-{}", std::process::id()));
        let path = write_fixture(&directory, &sample_document());
        let mut providers = load_providers_at(&path).unwrap();

        // Edit the models.dev-style entry without touching its format: npm
        // stays absent after the save.
        let openrouter = providers
            .iter_mut()
            .find(|provider| provider.slug == "openrouter")
            .unwrap();
        openrouter.models[0].context_window = Some(1_000_000);
        save_providers_at(&path, &providers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(saved["provider"]["openrouter"].get("npm").is_none());
        assert_eq!(saved["provider"]["openrouter"]["options"]["apiKey"], "sk-or");
        assert_eq!(
            saved["provider"]["openrouter"]["models"]["vendor/model"]["limit"],
            serde_json::json!({"context": 1000000, "output": 32000})
        );

        // Clearing the window strips `context` but keeps the other limit
        // keys a hand-written config may carry.
        let mut providers = load_providers_at(&path).unwrap();
        let openrouter = providers
            .iter_mut()
            .find(|provider| provider.slug == "openrouter")
            .unwrap();
        openrouter.models[0].context_window = None;
        save_providers_at(&path, &providers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let limit = &saved["provider"]["openrouter"]["models"]["vendor/model"]["limit"];
        assert!(limit.get("context").is_none());

        // Explicitly switching the format rewrites npm.
        let mut providers = load_providers_at(&path).unwrap();
        let openrouter = providers
            .iter_mut()
            .find(|provider| provider.slug == "openrouter")
            .unwrap();
        openrouter.api_format = ProviderApiFormat::Anthropic;
        openrouter.npm_touched = true;
        save_providers_at(&path, &providers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["provider"]["openrouter"]["npm"], "@ai-sdk/anthropic");

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn context_window_helpers_agree_with_the_schema() {
        // The stored value round-trips through the UI's free-text field.
        let window = parse_context_window(&format_context_window(1_000_000)).unwrap();
        assert_eq!(window, 1_000_000);
    }

    #[test]
    fn legacy_override_file_migrates_into_the_real_config() {
        let directory =
            std::env::temp_dir().join(format!("waku-oc-cfg-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();

        // Point both paths into the fixture directory.
        let config = directory.join("opencode.json");
        let legacy = directory.join("opencode-providers.json");
        let document = serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "provider": {
                "old-relay": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "Old Relay",
                    "options": {"baseURL": "https://old.example.com/v1", "apiKey": "sk-old"},
                    "models": {"old-model": {"name": "old-model", "contextWindow": 65536}}
                }
            }
        });
        std::fs::write(&legacy, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

        let migrated = migrate_legacy_override_file_in(&config, &legacy);
        assert!(migrated);
        assert!(!legacy.exists());

        let providers = load_providers_at(&config).unwrap();
        assert_eq!(providers.len(), 1);
        let relay = &providers[0];
        assert_eq!(relay.slug, "old-relay");
        assert_eq!(relay.name, "Old Relay");
        assert_eq!(relay.models[0].context_window, Some(65536));

        // Running again is a no-op.
        assert!(!migrate_legacy_override_file_in(&config, &legacy));

        let _ = std::fs::remove_dir_all(directory);
    }

    // The test seam mirrors the public functions but takes both paths, so
    // the migration never touches the developer's real OpenCode config.
    fn migrate_legacy_override_file_in(config: &Path, legacy: &Path) -> bool {
        let Ok(bytes) = std::fs::read(legacy) else {
            return false;
        };
        let migrated = serde_json::from_slice::<Value>(&bytes).ok().and_then(|document| {
            let entries = document.get("provider")?.as_object()?.clone();
            if entries.is_empty() {
                return Some(Vec::new());
            }
            Some(
                entries
                    .iter()
                    .map(|(key, entry)| provider_from_config(key, entry, &HashSet::new()))
                    .collect::<Vec<CustomProvider>>(),
            )
        });
        let migrated = match migrated {
            Some(providers) => save_providers_at(config, &providers).is_ok(),
            None => false,
        };
        if migrated {
            let _ = std::fs::remove_file(legacy);
        }
        migrated
    }
}

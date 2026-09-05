//! OpenCode model discovery.

use std::path::{Path, PathBuf};

use crate::model::{ProviderAgentPreset, ProviderModel, ProviderModelOption};

pub fn fallback_models() -> Vec<ProviderModel> {
    // OpenCode's catalog depends on the user's configured providers. An
    // invented fallback would make unavailable models look selectable.
    Vec::new()
}

pub fn fallback_agent_presets() -> Vec<ProviderAgentPreset> {
    Vec::new()
}

/// Discovers models from the installed OpenCode CLI (`opencode models`).
pub fn discover_catalog(binary: &Path) -> (Vec<ProviderModel>, Vec<ProviderAgentPreset>) {
    let discovered = discover_opencode_models(binary);
    let models = if discovered.is_empty() {
        // A failed or empty probe keeps the last successful discovery over
        // the hardcoded catalog, so one bad CLI run can't shrink the picker.
        cached_models().unwrap_or_else(fallback_models)
    } else {
        let models = deduplicate(discovered);
        write_cached_models(&models);
        models
    };
    (models, Vec::new())
}

/// Where OpenCode's last discovered catalog is cached. Debug builds keep it
/// in the checkout's gitignored `temp/` beside the debug database, so
/// development never touches the installed app's cache.
fn model_cache_path() -> PathBuf {
    let directory = if cfg!(debug_assertions) {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("temp")
            .join("model-cache")
    } else {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(crate::identity::DATA_DIRECTORY_NAME)
            .join("models")
    };
    directory.join("opencode.json")
}

/// The catalog cached by the last successful discovery, or `None` when no run
/// has cached one or the file no longer parses. Reads the filesystem, so call
/// it from the discovery thread, never from render.
pub fn cached_models() -> Option<Vec<ProviderModel>> {
    read_models_file(&model_cache_path())
}

fn read_models_file(path: &Path) -> Option<Vec<ProviderModel>> {
    let contents = std::fs::read(path).ok()?;
    let models = serde_json::from_slice::<Vec<ProviderModel>>(&contents).ok()?;
    (!models.is_empty()).then_some(models)
}

/// Best-effort: a cache that fails to write only costs the next launch its
/// head start.
fn write_cached_models(models: &[ProviderModel]) {
    let _ = write_models_file(&model_cache_path(), models);
}

fn write_models_file(path: &Path, models: &[ProviderModel]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write-then-rename so a crash mid-write can't leave a torn file for the
    // next launch to trip over.
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec(models)?)?;
    std::fs::rename(temporary, path)
}

fn discover_opencode_models(binary: &Path) -> Vec<ProviderModel> {
    // The plain `models` listing discards variants. Query the V2 catalog
    // through the CLI so discovery shares its authentication/service context.
    let query = std::env::current_dir()
        .ok()
        .map(|directory| {
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("location[directory]", &directory.to_string_lossy())
                .finish()
        })
        .unwrap_or_default();
    let mut activation_command = crate::command_env::command(binary);
    activation_command.args([
        "api",
        "post",
        &format!("/api/plugin/await-activation?{query}"),
    ]);
    let _ = crate::command_env::output(&mut activation_command);
    let mut catalog_command = crate::command_env::command(binary);
    catalog_command.args(["api", "get", &format!("/api/model?{query}")]);
    if let Ok(output) = crate::command_env::output(&mut catalog_command)
        && output.status.success()
        && let Ok(value) = serde_json::from_slice(&output.stdout)
    {
        let models = parse_opencode_catalog(&value);
        if !models.is_empty() {
            return models;
        }
    }
    let mut command = crate::command_env::command(binary);
    command.arg("models");
    let Ok(output) = crate::command_env::output(&mut command) else {
        return Vec::new();
    };
    let models = parse_opencode_models(&String::from_utf8_lossy(&output.stdout));
    let cached: std::collections::HashMap<_, _> = cached_models()
        .unwrap_or_default()
        .into_iter()
        .map(|model| (model.id.clone(), model))
        .collect();
    models
        .into_iter()
        .map(|model| cached.get(&model.id).cloned().unwrap_or(model))
        .collect()
}

fn parse_opencode_catalog(value: &serde_json::Value) -> Vec<ProviderModel> {
    use serde_json::Value;
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("enabled").and_then(Value::as_bool) != Some(false))
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?;
            let provider = entry.get("providerID")?.as_str()?;
            let name = entry.get("name").and_then(Value::as_str).unwrap_or(id);
            let reference = fintwind_protocol::thinking_modes::find(id, Some(name));
            let mut model = ProviderModel::new(format!("{provider}/{id}"), name)
                .sub_provider(display_name_from_slug(provider));
            model.reasoning_efforts = entry
                .get("variants")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|variant| {
                    let id = variant.get("id")?.as_str()?;
                    let label = reference
                        .and_then(|model| {
                            model.thinking_modes.iter().find(|mode| mode.mode_key == id)
                        })
                        .map(|mode| mode.name_en.as_str())
                        .unwrap_or(id);
                    Some(ProviderModelOption::new(id, label))
                })
                .collect();
            // Only select defaults that actually exist in this server's catalog.
            let configured_default = entry
                .get("variants")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|variant| {
                    variant
                        .get("settings")
                        .and_then(Value::as_object)
                        .is_some_and(|settings| {
                            !settings.is_empty()
                                && settings.iter().all(|(key, value)| {
                                    entry.get("settings").and_then(|settings| settings.get(key))
                                        == Some(value)
                                })
                        })
                })
                .and_then(|variant| variant.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            model.default_reasoning_effort = configured_default
                .or_else(|| {
                    reference
                        .and_then(|model| model.thinking_modes.iter().find(|mode| mode.is_default))
                        .map(|mode| mode.mode_key.clone())
                })
                .filter(|id| {
                    model
                        .reasoning_efforts
                        .iter()
                        .any(|option| &option.id == id)
                });
            Some(model)
        })
        .collect()
}

fn parse_opencode_models(output: &str) -> Vec<ProviderModel> {
    output
        .lines()
        .filter_map(|line| {
            let id = strip_ansi(line).trim().to_owned();
            if id.is_empty() || id.split_whitespace().count() != 1 || !id.contains('/') {
                return None;
            }
            let (provider, model) = id.split_once('/')?;
            if provider.is_empty() || model.is_empty() {
                return None;
            }
            Some(
                ProviderModel::new(id.clone(), display_name_from_slug(model))
                    .sub_provider(display_name_from_slug(provider)),
            )
        })
        .collect()
}

fn display_name_from_slug(slug: &str) -> String {
    let words = slug
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "gpt" => "GPT".to_owned(),
            "ai" => "AI".to_owned(),
            "xai" => "xAI".to_owned(),
            _ if part
                .chars()
                .all(|char| char.is_ascii_digit() || char == '.') =>
            {
                part.to_owned()
            }
            _ => {
                let mut chars = part.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            }
        })
        .collect::<Vec<_>>();
    if words.first().is_some_and(|word| word == "GPT") {
        words.join("-")
    } else {
        words.join(" ")
    }
}

fn strip_ansi(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(char) = chars.next() {
        if char == '\u{1b}' {
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            output.push(char);
        }
    }
    output
}

fn deduplicate(models: Vec<ProviderModel>) -> Vec<ProviderModel> {
    let mut seen = std::collections::HashSet::new();
    models
        .into_iter()
        .filter(|model| seen.insert(model.id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_keeps_server_variants_and_filters_unavailable_models() {
        let models = parse_opencode_catalog(&serde_json::json!({"data": [
            {"id": "gpt-5.5", "providerID": "gateway", "enabled": true,
             "name": "GPT-5.5", "variants": [{"id": "low"}, {"id": "xhigh"}]},
            {"id": "hidden", "providerID": "gateway", "enabled": false},
            {"id": "custom", "providerID": "gateway", "variants": [{"id": "my-budget"}]}
        ]}));
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].reasoning_efforts.len(), 2);
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(models[1].reasoning_efforts[0].id, "my-budget");
        assert!(models[1].default_reasoning_effort.is_none());
    }

    #[test]
    fn model_cache_round_trips_and_rejects_empty_or_invalid_files() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-model-cache-test-{}", std::process::id()));
        let path = directory.join("opencode.json");
        let models = vec![ProviderModel::new("opencode/big-pickle", "Big Pickle").default()];

        assert_eq!(read_models_file(&path), None);
        write_models_file(&path, &models).unwrap();
        assert_eq!(read_models_file(&path), Some(models));

        write_models_file(&path, &[]).unwrap();
        assert_eq!(read_models_file(&path), None);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read_models_file(&path), None);

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn parses_opencode_provider_qualified_models() {
        let models = parse_opencode_models(
            "opencode/big-pickle\n\u{1b}[32mgithub-copilot/gpt-5.4\u{1b}[0m\nnoise here\n",
        );
        assert_eq!(models.len(), 2);
        assert_eq!(models[1].id, "github-copilot/gpt-5.4");
        assert_eq!(models[1].name, "GPT-5.4");
        assert_eq!(models[1].sub_provider.as_deref(), Some("Github Copilot"));
    }
}

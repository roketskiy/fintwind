//! OpenCode model discovery.

use std::path::{Path, PathBuf};

use crate::model::{ProviderAgentPreset, ProviderModel};

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
    let mut command = crate::command_env::command(binary);
    let command = command.arg("models");
    let Ok(output) = crate::command_env::output(command) else {
        return Vec::new();
    };
    parse_opencode_models(&String::from_utf8_lossy(&output.stdout))
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
    fn model_cache_round_trips_and_rejects_empty_or_invalid_files() {
        let directory =
            std::env::temp_dir().join(format!("waku-model-cache-test-{}", std::process::id()));
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

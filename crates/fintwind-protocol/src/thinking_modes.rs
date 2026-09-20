//! Bundled reference metadata. Parsed once; no filesystem access at runtime.
use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ThinkingModel {
    pub name: String,
    pub thinking_modes: Vec<ThinkingMode>,
}

#[derive(Debug, Deserialize)]
pub struct ThinkingMode {
    pub mode_key: String,
    pub name_en: String,
    pub name_zh: String,
    pub thinking_budget_tokens: Option<u64>,
    pub is_default: bool,
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn find(id: &str, name: Option<&str>) -> Option<&'static ThinkingModel> {
    static MODELS: OnceLock<HashMap<String, ThinkingModel>> = OnceLock::new();
    let models = MODELS.get_or_init(|| {
        let entries: Vec<ThinkingModel> =
            serde_json::from_str(include_str!("../../../models_thinking_modes.json"))
                .expect("bundled thinking modes must be valid JSON");
        entries
            .into_iter()
            .map(|entry| (normalize(&entry.name), entry))
            .collect()
    });
    // Exact normalized matches only: never confuse Pro, Flash or dated releases.
    let leaf = id.rsplit('/').next().unwrap_or(id);
    let normalized = normalize(leaf);
    models
        .get(&normalized)
        .or_else(|| name.and_then(|name| models.get(&normalize(name))))
        .or_else(|| {
            normalized
                .strip_prefix("claude")
                .and_then(|id| models.get(id))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_names_and_qualified_ids_without_family_guessing() {
        let model = find("gateway/gpt-5.5", None).unwrap();
        assert_eq!(model.name, "GPT-5.5");
        assert!(model.thinking_modes.iter().any(|mode| mode.is_default));
        assert_eq!(find("claude-opus-4-7", None).unwrap().name, "Opus 4.7");
        assert_eq!(
            find("stepfun/step-5-preview", None).unwrap().name,
            "Step 5 Preview"
        );
        let step_flash = find("step-3.7-flash", None).unwrap();
        assert_eq!(step_flash.name, "Step 3.7 Flash");
        assert!(
            step_flash
                .thinking_modes
                .iter()
                .any(|mode| mode.mode_key == "medium" && mode.is_default)
        );
        assert_eq!(
            find("step-3.5-flash-2603", None).unwrap().name,
            "Step 3.5 Flash 2603"
        );
        assert_eq!(find("step-3.5-flash", None).unwrap().name, "Step 3.5 Flash");
        assert!(find("gpt-5.5-unknown", None).is_none());
    }
}

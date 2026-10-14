//! Protocol mappings from a model's thinking modes to the OpenCode provider
//! format the roster writes: variant objects contain package options
//! directly (not V2 `settings`).
//!
//! The modes come from the models.dev catalog ([`crate::models_dev`]) — the
//! same download that fills names and windows. A model the catalog does not
//! describe still offers the standard effort ladder rather than nothing, so
//! an undiscovered model keeps the common settings selectable.
use serde_json::{Map, Value, json};

use crate::custom_providers::ProviderApiFormat;
use crate::models_dev::{ModelsDevThinking, ModelsDevThinkingMode};

/// The levels offered for a model whose catalog entry states no modes: the
/// standard effort ladder, `none` spelled the way the wire mapping keys a
/// disabled mode.
const FALLBACK_MODES: [&str; 6] = ["nothinking", "low", "medium", "high", "xhigh", "max"];

pub(crate) fn fill_model_variants(
    spec: &mut Map<String, Value>,
    thinking: Option<&ModelsDevThinking>,
    name: Option<&str>,
    format: ProviderApiFormat,
) {
    // A catalog that could not be had says nothing: leave the spec alone
    // rather than second-guess the levels an earlier write may have based
    // on real data. A catalog that answers but lists no modes for the
    // model is an authoritative silence — the ladder fills in.
    let Some(thinking) = thinking else {
        return;
    };
    let fallback: Vec<ModelsDevThinkingMode>;
    let modes: &[ModelsDevThinkingMode] = if thinking.modes.is_empty() {
        fallback = FALLBACK_MODES
            .iter()
            .map(|mode_key| ModelsDevThinkingMode {
                mode_key: (*mode_key).to_owned(),
                thinking_budget_tokens: None,
            })
            .collect();
        &fallback
    } else {
        &thinking.modes
    };
    let variants = spec.entry("variants").or_insert_with(|| json!({}));
    let Some(variants) = variants.as_object_mut() else {
        return;
    };
    // Modes the catalog no longer lists are an earlier write's leftovers —
    // often a relay's re-listing that an older build accepted. Drop them so
    // a stale level cannot masquerade as the model's own; variants the
    // catalog still lists keep their values, hand-edits included.
    variants.retain(|key, _| modes.iter().any(|mode| mode.mode_key == *key));
    for mode in modes {
        // Explicit user settings, including disabled variants, always win.
        variants
            .entry(mode.mode_key.clone())
            .or_insert_with(|| mode_options(mode, name, format));
    }
    let Some(default_key) = crate::models_dev::default_mode_of(modes) else {
        return;
    };
    if let Some(mode) = modes.iter().find(|mode| mode.mode_key == default_key) {
        let defaults = mode_options(mode, name, format);
        let options = spec.entry("options").or_insert_with(|| json!({}));
        if let Some(options) = options.as_object_mut() {
            for (key, value) in defaults.as_object().expect("mode options are objects") {
                options.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }
}

fn mode_options(
    mode: &ModelsDevThinkingMode,
    name: Option<&str>,
    format: ProviderApiFormat,
) -> Value {
    let key = mode.mode_key.as_str();
    let name = name.unwrap_or_default().to_ascii_lowercase();
    let off = key == "nothinking";
    let on_toggle = key == "thinking";
    let effort = match key {
        "nothinking" => "none",
        // The toggle's on state carries no effort level of its own; gateways
        // that still want one read the model's highest supported level.
        "thinking" => "high",
        key => key,
    };
    if matches!(format, ProviderApiFormat::Anthropic) {
        if name.starts_with("claude opus 5.5") {
            return json!({"effort": key});
        }
        if off {
            return json!({"thinking": {"type": "disabled"}});
        }
        if on_toggle {
            return json!({"thinking": {"type": "enabled"}});
        }
        // Budget-shaped models expose token budgets rather than effort
        // levels. The catalog's floor-raised budget wins; the expansion
        // already raised each preset above the model's own minimum. A
        // catalog-silent level (the effort-ladder fallback) still needs a
        // budget on this wire, so the bounded presets answer per level.
        if let Some(budget) = mode.thinking_budget_tokens {
            return json!({"thinking": {"type": "enabled", "budgetTokens": budget}});
        }
        let budget = match key {
            "low" => 1024,
            "medium" => 8192,
            "max" | "xhigh" | "high" => 24576,
            _ => 16384,
        };
        return json!({"thinking": {"type": "enabled", "budgetTokens": budget}});
    }
    // The Google wire keys thinkingConfig regardless of the model's name;
    // a gemini-named model behind an OpenAI-compatible relay keeps the same
    // mapping, since relays forward the option name verbatim.
    if matches!(format, ProviderApiFormat::Google) || name.starts_with("gemini") {
        if off {
            return json!({"thinkingConfig": {"thinkingBudget": 0, "includeThoughts": false}});
        }
        if let Some(budget) = mode.thinking_budget_tokens {
            return json!({"thinkingConfig": {"thinkingBudget": budget, "includeThoughts": true}});
        }
        if on_toggle {
            return json!({"thinkingConfig": {"includeThoughts": true}});
        }
        return json!({"thinkingConfig": {"thinkingLevel": effort, "includeThoughts": true}});
    }
    if name.starts_with("gpt")
        || name.starts_with("o1")
        || name.starts_with("o3")
        || name.starts_with("o4")
        || matches!(format, ProviderApiFormat::OpenAiResponses)
    {
        return json!({"reasoningEffort": effort});
    }
    // Compatible gateways differ here. These are inferred request options,
    // not claims about the underlying provider's capabilities.
    let mut options = if matches!(key, "thinking" | "nothinking") {
        json!({})
    } else {
        json!({"reasoningEffort": effort})
    };
    if name.starts_with("deepseek") || name.starts_with("glm") || name.starts_with("kimi") {
        options["thinking"] = json!({"type": if off { "disabled" } else { "enabled" }});
    } else {
        // openai-compatible forwards nonstandard option names verbatim.
        options["enable_thinking"] = json!(!off);
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models_dev::ModelsDevReasoningOption;

    fn thinking(options: &[ModelsDevReasoningOption]) -> ModelsDevThinking {
        crate::models_dev::reconcile_reasoning_options(options)
    }

    fn effort(values: &[&str]) -> ModelsDevReasoningOption {
        ModelsDevReasoningOption::Effort {
            values: values.iter().map(|value| (*value).to_owned()).collect(),
        }
    }

    #[test]
    fn fills_catalog_variants_and_default_without_overwriting_edits() {
        let mut spec = json!({"variants": {"high": {"reasoningEffort": "custom"}},
            "options": {"temperature": 0.4}})
        .as_object()
        .unwrap()
        .clone();
        fill_model_variants(
            &mut spec,
            Some(&thinking(&[
                effort(&["none", "low", "medium", "high"]),
                ModelsDevReasoningOption::Toggle,
            ])),
            None,
            ProviderApiFormat::OpenAiResponses,
        );
        assert_eq!(spec["variants"]["high"]["reasoningEffort"], "custom");
        assert_eq!(spec["variants"]["nothinking"]["reasoningEffort"], "none");
        assert_eq!(spec["variants"]["low"]["reasoningEffort"], "low");
        assert_eq!(spec["options"]["reasoningEffort"], "medium");
        assert_eq!(spec["options"]["temperature"], 0.4);
        let previous = spec.clone();
        fill_model_variants(
            &mut spec,
            Some(&thinking(&[
                effort(&["none", "low", "medium", "high"]),
                ModelsDevReasoningOption::Toggle,
            ])),
            None,
            ProviderApiFormat::OpenAiResponses,
        );
        assert_eq!(spec, previous);
    }

    #[test]
    fn an_undescribed_model_still_offers_the_effort_ladder() {
        let mut spec = Map::new();
        fill_model_variants(
            &mut spec,
            Some(&ModelsDevThinking::default()),
            Some("gpt-5.5"),
            ProviderApiFormat::OpenAi,
        );
        // The standard ladder, none through max, defaulting to medium.
        let keys: Vec<_> = spec["variants"].as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec!["nothinking", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(spec["options"]["reasoningEffort"], "medium");

        // On Anthropic's wire the ladder levels carry bounded budgets.
        let mut anthropic = Map::new();
        fill_model_variants(
            &mut anthropic,
            Some(&ModelsDevThinking::default()),
            Some("claude sonnet"),
            ProviderApiFormat::Anthropic,
        );
        assert_eq!(
            anthropic["variants"]["low"]["thinking"]["budgetTokens"],
            1024
        );
        assert_eq!(
            anthropic["variants"]["high"]["thinking"]["budgetTokens"],
            24576
        );
        assert_eq!(
            anthropic["variants"]["nothinking"]["thinking"]["type"],
            "disabled"
        );
    }

    #[test]
    fn google_format_keys_the_ladder_through_thinking_config() {
        // On Google's wire an undescribed model keys the ladder through
        // thinkingLevel rather than a budget, whatever the model is named.
        let mut google = Map::new();
        fill_model_variants(
            &mut google,
            Some(&ModelsDevThinking::default()),
            Some("custom-named-model"),
            ProviderApiFormat::Google,
        );
        assert_eq!(
            google["variants"]["nothinking"]["thinkingConfig"]["thinkingBudget"],
            0
        );
        assert_eq!(
            google["variants"]["low"]["thinkingConfig"]["thinkingLevel"],
            "low"
        );
        assert_eq!(
            google["variants"]["high"]["thinkingConfig"]["thinkingLevel"],
            "high"
        );
    }

    #[test]
    fn compatible_toggles_use_wire_keys_without_inventing_an_effort() {
        let mut spec = Map::new();
        fill_model_variants(
            &mut spec,
            Some(&thinking(&[ModelsDevReasoningOption::Toggle])),
            None,
            ProviderApiFormat::OpenAi,
        );
        assert_eq!(
            spec["variants"]["thinking"],
            json!({"enable_thinking": true})
        );
        assert_eq!(
            spec["variants"]["nothinking"],
            json!({"enable_thinking": false})
        );
    }

    #[test]
    fn budget_models_raise_each_preset_above_the_catalog_minimum() {
        let mut spec = Map::new();
        fill_model_variants(
            &mut spec,
            Some(&thinking(&[ModelsDevReasoningOption::BudgetTokens {
                min: Some(2048),
            }])),
            None,
            ProviderApiFormat::Anthropic,
        );
        assert_eq!(spec["variants"]["low"]["thinking"]["budgetTokens"], 2048);
        assert_eq!(spec["variants"]["medium"]["thinking"]["budgetTokens"], 8192);
        assert_eq!(spec["variants"]["max"]["thinking"]["budgetTokens"], 24576);
        assert_eq!(
            spec["variants"]["nothinking"]["thinking"]["type"],
            "disabled"
        );
        assert_eq!(spec["options"]["thinking"]["budgetTokens"], 8192);
    }

    #[test]
    fn stepfun_effort_levels_keep_the_catalog_values() {
        let mut spec = Map::new();
        fill_model_variants(
            &mut spec,
            Some(&thinking(&[effort(&["low", "medium", "high"])])),
            Some("Step 3.7 Flash"),
            ProviderApiFormat::OpenAi,
        );
        assert_eq!(spec["variants"]["low"]["reasoningEffort"], "low");
        assert_eq!(spec["variants"]["medium"]["reasoningEffort"], "medium");
        assert_eq!(spec["variants"]["high"]["reasoningEffort"], "high");
        assert_eq!(spec["options"]["reasoningEffort"], "medium");
        assert!(spec["variants"].get("nothinking").is_none());

        spec.clear();
        fill_model_variants(
            &mut spec,
            Some(&thinking(&[effort(&["low", "high"])])),
            Some("Step 3.5 Flash 2603"),
            ProviderApiFormat::OpenAi,
        );
        assert_eq!(spec["variants"]["low"]["reasoningEffort"], "low");
        assert_eq!(spec["variants"]["high"]["reasoningEffort"], "high");
        // No medium on the list: the upper-middle level defaults.
        assert_eq!(spec["options"]["reasoningEffort"], "high");
    }

    #[test]
    fn opus_55_uses_anthropic_effort_and_minimal_passes_through() {
        let mut spec = Map::new();
        fill_model_variants(
            &mut spec,
            Some(&thinking(&[effort(&["low", "medium", "max"])])),
            Some("Claude Opus 5.5"),
            ProviderApiFormat::Anthropic,
        );
        assert_eq!(spec["variants"]["low"]["effort"], "low");
        assert_eq!(spec["variants"]["medium"]["effort"], "medium");
        assert_eq!(spec["variants"]["max"]["effort"], "max");
        assert!(spec["variants"].get("nothinking").is_none());
        assert_eq!(spec["options"]["effort"], "medium");

        spec.clear();
        fill_model_variants(
            &mut spec,
            Some(&thinking(&[effort(&["minimal", "high"])])),
            Some("Gemini 3 Pro"),
            ProviderApiFormat::OpenAi,
        );
        assert_eq!(
            spec["variants"]["minimal"]["thinkingConfig"]["thinkingLevel"],
            "minimal"
        );
    }
}

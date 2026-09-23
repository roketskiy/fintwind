//! Best-effort protocol mappings for the bundled thinking-mode reference.
//! The roster currently writes OpenCode's supported legacy provider format:
//! variant objects contain package options directly (not V2 `settings`).
use serde_json::{Map, Value, json};

use crate::custom_providers::ProviderApiFormat;
use fintwind_protocol::thinking_modes::{self, ThinkingMode};

pub(crate) fn fill_model_variants(
    spec: &mut Map<String, Value>,
    id: &str,
    name: Option<&str>,
    format: ProviderApiFormat,
) {
    let Some(reference) = thinking_modes::find(id, name) else {
        return;
    };
    let variants = spec.entry("variants").or_insert_with(|| json!({}));
    let Some(variants) = variants.as_object_mut() else {
        return;
    };
    for mode in &reference.thinking_modes {
        // Explicit user settings, including disabled variants, always win.
        variants
            .entry(mode.mode_key.clone())
            .or_insert_with(|| mode_options(mode, &reference.name, format));
    }
    if let Some(mode) = reference.thinking_modes.iter().find(|mode| mode.is_default) {
        let defaults = mode_options(mode, &reference.name, format);
        let options = spec.entry("options").or_insert_with(|| json!({}));
        if let Some(options) = options.as_object_mut() {
            for (key, value) in defaults.as_object().expect("mode options are objects") {
                options.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }
}

fn mode_options(mode: &ThinkingMode, name: &str, format: ProviderApiFormat) -> Value {
    let key = mode.mode_key.as_str();
    let name = name.to_ascii_lowercase();
    let off = key == "nothinking";
    let effort = match key {
        "nothinking" => "none",
        "thinking" | "adaptive" | "extended" | "deep" => "high",
        key => key,
    };
    if matches!(format, ProviderApiFormat::Anthropic) {
        if name.starts_with("claude opus 5.5") {
            return json!({"effort": key});
        }
        if off {
            return json!({"thinking": {"type": "disabled"}});
        }
        if key == "adaptive" {
            return json!({"thinking": {"type": "adaptive"}});
        }
        // Older Claude models expose token budgets rather than adaptive effort.
        // The reference's explicit budget wins; otherwise use a bounded preset.
        let budget = mode.thinking_budget_tokens.unwrap_or(match key {
            "low" => 1024,
            "medium" => 8192,
            "max" | "xhigh" | "extended" => 24576,
            _ => 16384,
        });
        return json!({"thinking": {"type": "enabled", "budgetTokens": budget}});
    }
    if name.starts_with("gemini") {
        if off {
            return json!({"thinkingConfig": {"thinkingBudget": 0, "includeThoughts": false}});
        }
        if let Some(budget) = mode.thinking_budget_tokens {
            return json!({"thinkingConfig": {"thinkingBudget": budget, "includeThoughts": true}});
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

    #[test]
    fn fills_reference_variants_and_default_without_overwriting_edits() {
        let mut spec = json!({"variants": {"high": {"reasoningEffort": "custom"}},
            "options": {"temperature": 0.4}})
        .as_object()
        .unwrap()
        .clone();
        fill_model_variants(
            &mut spec,
            "gpt-5.5",
            None,
            ProviderApiFormat::OpenAiResponses,
        );
        assert_eq!(spec["variants"]["high"]["reasoningEffort"], "custom");
        assert_eq!(spec["variants"]["nothinking"]["reasoningEffort"], "none");
        assert_eq!(spec["options"]["reasoningEffort"], "medium");
        assert_eq!(spec["options"]["temperature"], 0.4);
        let previous = spec.clone();
        fill_model_variants(
            &mut spec,
            "gpt-5.5",
            None,
            ProviderApiFormat::OpenAiResponses,
        );
        assert_eq!(spec, previous);
    }

    #[test]
    fn compatible_toggles_use_wire_keys_without_inventing_an_effort() {
        let mut spec = Map::new();
        fill_model_variants(&mut spec, "qwen3.6-27b", None, ProviderApiFormat::OpenAi);
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
    fn unknown_models_are_untouched_and_anthropic_uses_budgets() {
        let mut spec = Map::new();
        fill_model_variants(&mut spec, "unknown-model", None, ProviderApiFormat::OpenAi);
        assert!(spec.is_empty());
        fill_model_variants(
            &mut spec,
            "claude-opus-4-7",
            None,
            ProviderApiFormat::Anthropic,
        );
        assert_eq!(spec["variants"]["low"]["thinking"]["budgetTokens"], 1024);
        assert_eq!(spec["variants"]["max"]["thinking"]["budgetTokens"], 24576);
    }

    #[test]
    fn stepfun_flash_uses_effort_levels_with_medium_default() {
        let mut spec = Map::new();
        fill_model_variants(
            &mut spec,
            "step-3.7-flash",
            Some("Step 3.7 Flash"),
            ProviderApiFormat::OpenAi,
        );
        assert_eq!(spec["variants"]["low"]["reasoningEffort"], "low");
        assert_eq!(spec["variants"]["medium"]["reasoningEffort"], "medium");
        assert_eq!(spec["variants"]["high"]["reasoningEffort"], "high");
        assert_eq!(spec["options"]["reasoningEffort"], "medium");
        assert!(spec["variants"].get("nothinking").is_none());
        assert!(spec["variants"].get("deep").is_none());

        spec.clear();
        fill_model_variants(&mut spec, "step-3.5-flash", None, ProviderApiFormat::OpenAi);
        assert_eq!(
            spec["variants"]["thinking"],
            json!({"enable_thinking": true})
        );
        assert!(spec["variants"].get("low").is_none());
        assert_eq!(spec["options"]["enable_thinking"], true);

        spec.clear();
        fill_model_variants(
            &mut spec,
            "step-3.5-flash-2603",
            None,
            ProviderApiFormat::OpenAi,
        );
        assert_eq!(spec["variants"]["low"]["reasoningEffort"], "low");
        assert_eq!(spec["variants"]["high"]["reasoningEffort"], "high");
        assert!(spec["variants"].get("medium").is_none());
        assert_eq!(spec["options"]["reasoningEffort"], "high");
    }

    #[test]
    fn opus_55_uses_anthropic_effort_without_disabling_adaptive_thinking() {
        let mut spec = Map::new();
        fill_model_variants(
            &mut spec,
            "claude-opus-5-5",
            None,
            ProviderApiFormat::Anthropic,
        );

        assert_eq!(spec["variants"]["low"]["effort"], "low");
        assert_eq!(spec["variants"]["medium"]["effort"], "medium");
        assert_eq!(spec["variants"]["max"]["effort"], "max");
        assert!(spec["variants"].get("nothinking").is_none());
        assert_eq!(spec["options"]["effort"], "medium");
    }
}

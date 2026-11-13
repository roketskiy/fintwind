//! Native V2 model protocols and the bounded legacy-provider conversion.
use serde_json::{Map, Value, json};

use crate::custom_providers::ProviderApiFormat;

pub const OPENAI_IMAGES: &str = "@opencode/ai/providers/openai/images";
pub const GOOGLE_IMAGES: &str = "@opencode/ai/providers/google/images";
pub const PACKAGES: [&str; 6] = [
    "@opencode/ai/providers/openai-compatible",
    "@opencode/ai/providers/openai",
    "@opencode/ai/providers/anthropic",
    "@opencode/ai/providers/google",
    OPENAI_IMAGES,
    GOOGLE_IMAGES,
];

pub fn is_image(package: &str) -> bool {
    package.ends_with("/images")
}

pub fn image_package(package: &str) -> Option<&'static str> {
    match package {
        OPENAI_IMAGES => return Some(OPENAI_IMAGES),
        GOOGLE_IMAGES => return Some(GOOGLE_IMAGES),
        _ => {}
    }
    match ProviderApiFormat::from_package(package)? {
        ProviderApiFormat::OpenAi | ProviderApiFormat::OpenAiResponses => Some(OPENAI_IMAGES),
        ProviderApiFormat::Google => Some(GOOGLE_IMAGES),
        ProviderApiFormat::Anthropic => None,
    }
}

fn package(value: Value) -> Value {
    match value.as_str() {
        Some(value) if value.starts_with('@') && !value.starts_with("@opencode/") => {
            Value::String(format!("aisdk:{value}"))
        }
        _ => value,
    }
}

fn rename(object: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = object.remove(from) {
        object.insert(to.to_owned(), value);
    }
}

// Matches OpenCode v2.2.1's v1/config/provider-options.ts: compatible model
// options not consumed by the runtime are request-body fields, not settings.
fn model_options(spec: &mut Map<String, Value>, mut options: Map<String, Value>, compatible: bool) {
    let mut unbound = false;
    for key in ["thinking", "reasoningConfig"] {
        if let Some(thinking) = options.get_mut(key).and_then(Value::as_object_mut)
            && thinking.get("blockBinding").and_then(Value::as_bool) == Some(false)
        {
            thinking.remove("blockBinding");
            unbound = true;
            if thinking.is_empty() {
                options.remove(key);
            }
        }
    }
    if unbound {
        spec.insert(
            "compatibility".into(),
            json!({"supportsThinkingBlockBinding": false}),
        );
    }
    let mut settings = Map::new();
    let mut body = Map::new();
    for (key, value) in options {
        if !compatible
            || matches!(
                key.as_str(),
                "apiKey"
                    | "baseURL"
                    | "headers"
                    | "body"
                    | "extraBody"
                    | "timeout"
                    | "chunkTimeout"
                    | "headerTimeout"
                    | "disabled"
                    | "reasoningEffort"
                    | "textVerbosity"
                    | "user"
                    | "strictJsonSchema"
            )
        {
            settings.insert(key, value);
        } else {
            body.insert(key, value);
        }
    }
    if !settings.is_empty() {
        spec.insert("settings".into(), Value::Object(settings));
    }
    if !body.is_empty() {
        spec.insert("body".into(), Value::Object(body));
    }
}

pub(crate) fn native_provider(value: &Value) -> Value {
    let mut provider = value.as_object().cloned().unwrap_or_default();
    let npm = provider.remove("npm");
    if let Some(npm) = npm.clone() {
        provider.insert("package".into(), package(npm));
    }
    let mut settings = provider
        .remove("options")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for key in ["headers", "body"] {
        if let Some(value) = settings.remove(key) {
            provider.insert(key.into(), value);
        }
    }
    if let Some(value) = settings.remove("setCacheKey") {
        provider.insert(
            "compatibility".into(),
            json!({"supportsPromptCacheKey": value}),
        );
    }
    if let Some(api) = provider.remove("api") {
        settings.insert("baseURL".into(), api);
    }
    if !settings.is_empty() {
        provider.insert("settings".into(), Value::Object(settings));
    }
    if let Some(models) = provider.get_mut("models").and_then(Value::as_object_mut) {
        for spec in models.values_mut().filter_map(Value::as_object_mut) {
            let model_provider = spec.remove("provider").unwrap_or(Value::Null);
            let model_npm = model_provider
                .get("npm")
                .or(npm.as_ref())
                .and_then(Value::as_str);
            let compatible = model_npm == Some("@ai-sdk/openai-compatible");
            if let Some(npm) = model_provider.get("npm") {
                spec.insert("package".into(), package(npm.clone()));
            }
            if let Some(options) = spec.remove("options").and_then(|v| v.as_object().cloned()) {
                model_options(spec, options, compatible);
            }
            if let Some(api) = model_provider.get("api") {
                spec.entry("settings")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .unwrap()
                    .insert("baseURL".into(), api.clone());
            }
            rename(spec, "id", "modelID");
            rename(spec, "release_date", "releaseDate");
            let modalities = spec.remove("modalities").unwrap_or(Value::Null);
            let tools = spec.remove("tool_call");
            if !modalities.is_null() || tools.is_some() {
                spec.insert("capabilities".into(), json!({
                    "tools": tools.unwrap_or(json!(true)),
                    "input": modalities.get("input").cloned().unwrap_or(json!(["text", "image"])),
                    "output": modalities.get("output").cloned().unwrap_or(json!(["text"]))
                }));
            }
            if let Some(variants) = spec.remove("variants").and_then(|v| v.as_object().cloned()) {
                let variants: Vec<Value> = variants
                    .into_iter()
                    .filter_map(|(id, options)| {
                        if options.get("disabled").and_then(Value::as_bool) == Some(true) {
                            return None;
                        }
                        let mut variant = Map::from_iter([("id".into(), json!(id))]);
                        let mut options = options.as_object().cloned().unwrap_or_default();
                        options.remove("disabled");
                        model_options(&mut variant, options, compatible);
                        Some(Value::Object(variant))
                    })
                    .collect();
                spec.insert("variants".into(), Value::Array(variants));
            }
            if let Some(cost) = spec.get("cost").filter(|v| v.is_object()).cloned() {
                let tier = |cost: &Value| json!({"input": cost.get("input").cloned().unwrap_or(json!(0)), "output": cost.get("output").cloned().unwrap_or(json!(0)), "cache": {"read": cost.get("cache_read").cloned().unwrap_or(json!(0)), "write": cost.get("cache_write").cloned().unwrap_or(json!(0))}});
                let mut tiers = vec![tier(&cost)];
                if let Some(large) = cost.get("context_over_200k") {
                    let mut large = tier(large);
                    large["tier"] = json!({"type": "context", "size": 200000});
                    tiers.push(large);
                }
                spec.insert("cost".into(), json!(tiers));
            }
            if spec.get("status").and_then(Value::as_str) == Some("deprecated") {
                spec.insert("disabled".into(), json!(true));
            }
            if let Some(interleaved) = spec.remove("interleaved") {
                if let Some(field) = interleaved
                    .as_str()
                    .or_else(|| interleaved.get("field").and_then(Value::as_str))
                {
                    let compatibility = spec.entry("compatibility").or_insert_with(|| json!({}));
                    if let Some(compatibility) = compatibility.as_object_mut() {
                        compatibility.insert("reasoningField".into(), json!(field));
                    }
                }
            }
        }
    }
    Value::Object(provider)
}

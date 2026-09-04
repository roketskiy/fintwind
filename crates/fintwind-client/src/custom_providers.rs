//! User-configured model providers.
//!
//! A custom provider is an entry in OpenCode's own configuration file
//! (`provider.<key>` — base URL, API format, key, and model list) managed
//! from the Providers settings page. The app keeps no parallel store: the
//! working roster is loaded from, and committed back to, OpenCode's
//! configuration by [`crate::opencode_config`], so entries created with the
//! CLI and entries created here are the same data.

use std::path::PathBuf;
use std::time::{Duration, Instant};

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
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic")]
    Anthropic,
}

impl ProviderApiFormat {
    pub const ALL: [ProviderApiFormat; 3] = [Self::OpenAi, Self::OpenAiResponses, Self::Anthropic];

    /// The OpenCode provider package that adapts this protocol. OpenCode
    /// instantiates `@ai-sdk/openai` with `.responses(...)`, so this package
    /// is what selects the Responses API for a custom endpoint.
    pub fn npm_package(self) -> &'static str {
        match self {
            Self::OpenAi => "@ai-sdk/openai-compatible",
            Self::OpenAiResponses => "@ai-sdk/openai",
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
    /// Display name, typically filled from the models.dev catalog. Stored on
    /// the model as `name`; `None` keeps whatever the entry already carries.
    pub name: Option<String>,
    /// Output limit in tokens, typically filled from the models.dev catalog.
    /// Stored on the model as `limit.output` when the entry has none.
    pub output_limit: Option<u64>,
}

impl Default for CustomProviderModel {
    fn default() -> Self {
        Self {
            id: String::new(),
            context_window: None,
            name: None,
            output_limit: None,
        }
    }
}

impl CustomProviderModel {
    /// The name worth showing: `None` when absent or merely a copy of the id
    /// — a name equal to the id carries no information. The one authority on
    /// that question; load and merge may store whatever a source said, and
    /// read sites ask this instead of re-filtering.
    pub fn display_name(&self) -> Option<&str> {
        self.name
            .as_deref()
            .filter(|name| !name.is_empty() && *name != self.id)
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

/// One model from a provider's own model-list API response: the id it serves
/// under, an optional display name (Anthropic and Gemini answer with one;
/// OpenAI-compatible endpoints answer with bare ids), and optional limits
/// (Gemini reports token limits; OpenAI-compatible endpoints do not).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderApiModel {
    pub id: String,
    pub name: Option<String>,
    pub context_window: Option<u64>,
    pub output_limit: Option<u64>,
}

/// Why a model-list request failed, in provider-agnostic terms. The UI maps
/// each kind to localized wording; `Display` carries an English detail for
/// logs and callers that do not localize.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiListError {
    /// The Base URL is not an http(s) address.
    InvalidBaseUrl,
    /// The endpoint answered but rejected the credentials.
    AuthRejected(u16),
    /// The model-list path does not exist for this Base URL.
    ListMissing,
    /// Any other HTTP status.
    HttpStatus(u16),
    /// No HTTP answer: DNS, TLS, connection refused, curl itself.
    Unreachable(String),
    /// A 200 whose body carries no usable model list.
    NoModelList(String),
}

impl std::fmt::Display for ApiListError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBaseUrl => write!(formatter, "the Base URL is not an http(s) address"),
            Self::AuthRejected(status) => write!(formatter, "HTTP {status} (check the API key)"),
            Self::ListMissing => write!(formatter, "HTTP 404 (check the Base URL)"),
            Self::HttpStatus(status) => write!(formatter, "HTTP {status}"),
            Self::Unreachable(error) => write!(formatter, "{error}"),
            Self::NoModelList(error) => write!(formatter, "{error}"),
        }
    }
}

/// Ask the provider's endpoint for its model list: `GET {base_url}/models`
/// in the API format's authentication style. The network request the
/// Providers page's "fetch models" action performs; blocking, so call it from
/// the background executor.
pub fn fetch_api_model_list(
    provider: &CustomProvider,
) -> Result<Vec<ProviderApiModel>, ApiListError> {
    fetch_api_model_list_with_timeout(provider, 120)
}

fn fetch_api_model_list_with_timeout(
    provider: &CustomProvider,
    max_time_secs: u64,
) -> Result<Vec<ProviderApiModel>, ApiListError> {
    if !base_url_valid(provider.base_url.trim()) {
        return Err(ApiListError::InvalidBaseUrl);
    }
    let (url, headers) = api_list_request(provider);
    let (status, body) = fintwind_protocol::http::http_get(&url, &headers, max_time_secs)
        .map_err(|error| ApiListError::Unreachable(error.to_string()))?;
    match status {
        200 => parse_api_model_list(&body).map_err(|error| ApiListError::NoModelList(error.to_string())),
        401 | 403 => Err(ApiListError::AuthRejected(status)),
        404 => Err(ApiListError::ListMissing),
        status => Err(ApiListError::HttpStatus(status)),
    }
}

/// What one connectivity probe of a provider's endpoint found: the model-list
/// request is the whole test — endpoint reachable, credentials accepted, and
/// the endpoint speaks a model list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectivityOutcome {
    Reachable { models: usize, latency: Duration },
    Failed { error: ApiListError },
}

/// Probe a provider's endpoint for connectivity: one model-list request with
/// a short deadline, timed end to end. Blocking; call from the background
/// executor.
pub fn probe_connectivity(provider: &CustomProvider) -> ConnectivityOutcome {
    let started = Instant::now();
    match fetch_api_model_list_with_timeout(provider, CONNECTIVITY_TIMEOUT_SECS) {
        Ok(models) => ConnectivityOutcome::Reachable {
            models: models.len(),
            latency: started.elapsed(),
        },
        Err(error) => ConnectivityOutcome::Failed { error },
    }
}

/// How long a first-token probe waits for a model to start answering. Slow
/// reasoning models think for tens of seconds before their first token.
pub const FIRST_TOKEN_TIMEOUT_SECS: u64 = 30;

/// How long a connectivity probe waits for the model list.
pub const CONNECTIVITY_TIMEOUT_SECS: u64 = 20;

/// Why a first-token probe failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FirstTokenError {
    /// The Base URL is not an http(s) address.
    InvalidBaseUrl,
    /// The endpoint answered with this status; `message` is the provider's
    /// own error sentence when it sent one.
    HttpStatus { status: u16, message: Option<String> },
    /// No HTTP answer at all: DNS, TLS, connection refused, curl itself.
    Unreachable(String),
    /// A 200 that never streamed a token, with no error document explaining
    /// why.
    NoStreamData { message: Option<String> },
    /// The deadline passed before any token arrived.
    Timeout,
}

/// Measure a model's first-token latency: POST a minimal streaming chat
/// request and time how long the first SSE data line takes to arrive —
/// connection setup, queueing, and model thinking all count, which is what a
/// user experiences as "how long until words appear". The connection is torn
/// down at the first token, so the probe costs roughly one token. Blocking;
/// call from the background executor.
pub fn first_token_latency(
    provider: &CustomProvider,
    model_id: &str,
) -> Result<Duration, FirstTokenError> {
    if !base_url_valid(provider.base_url.trim()) {
        return Err(FirstTokenError::InvalidBaseUrl);
    }
    let (url, headers, body) = first_token_request(provider, model_id);
    let posted = fintwind_protocol::http::http_post_stream(
        &url,
        &headers,
        &body,
        FIRST_TOKEN_TIMEOUT_SECS,
    )
    .map_err(|error| FirstTokenError::Unreachable(error.to_string()))?;
    if let Some((latency, _)) = posted.first_data {
        return Ok(latency);
    }
    if posted.timed_out {
        return Err(FirstTokenError::Timeout);
    }
    let message = || error_message(&posted.error_body);
    match posted.status {
        200 => Err(FirstTokenError::NoStreamData { message: message() }),
        0 => Err(FirstTokenError::Unreachable(posted.error_body)),
        status => Err(FirstTokenError::HttpStatus {
            status,
            message: message(),
        }),
    }
}

/// The chat request a first-token probe sends, per API format: the same
/// authentication as the model list, a one-line prompt, streaming on, and the
/// smallest legal reply budget — the probe stops reading after the first
/// token, so nothing more is generated. Anthropic requires `max_tokens`;
/// OpenAI-compatible endpoints are left to their default because the newer
/// OpenAI models reject `max_tokens` in favor of `max_completion_tokens`, and
/// the Responses format likewise reads no budget (its `max_output_tokens`
/// floor of 16 varies across relays).
fn first_token_request(provider: &CustomProvider, model_id: &str) -> (String, Vec<String>, String) {
    let base = provider.base_url.trim().trim_end_matches('/');
    let url = match provider.api_format {
        ProviderApiFormat::OpenAi => format!("{base}/chat/completions"),
        ProviderApiFormat::OpenAiResponses => format!("{base}/responses"),
        ProviderApiFormat::Anthropic => format!("{}/messages", versioned_root(base)),
    };
    let mut headers = auth_headers(provider);
    headers.push("Content-Type: application/json".to_owned());
    headers.push("Accept: text/event-stream".to_owned());
    let body = match provider.api_format {
        // `escape_json_string` contributes the quotes around the id.
        ProviderApiFormat::OpenAi => format!(
            r#"{{"model":{},"stream":true,"messages":[{{"role":"user","content":"Hi"}}]}}"#,
            escape_json_string(model_id)
        ),
        ProviderApiFormat::OpenAiResponses => format!(
            r#"{{"model":{},"input":"Hi","stream":true}}"#,
            escape_json_string(model_id)
        ),
        ProviderApiFormat::Anthropic => format!(
            r#"{{"model":{},"max_tokens":1,"stream":true,"messages":[{{"role":"user","content":"Hi"}}]}}"#,
            escape_json_string(model_id)
        ),
    };
    (url, headers, body)
}

/// The one escape a JSON string in a hand-built body needs: the model id can
/// contain quotes or backslashes.
fn escape_json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

/// The provider's own error sentence from an error body, when one parses out:
/// OpenAI and Anthropic nest it under `error.message`, relays often use a
/// bare `message` or `error` string. Capped so a verbose provider cannot
/// balloon a tooltip.
fn error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let message = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .or_else(|| value.get("error").and_then(Value::as_str))
        .filter(|message| !message.trim().is_empty())?;
    Some(message.trim().chars().take(300).collect())
}

/// The list endpoint and its request headers per API format: OpenAI-compatible
/// SDK roots already carry their version path (`/v1`), while the Anthropic SDK
/// root omits one and the SDK adds it. The Responses format lists models
/// through the same OpenAI-style endpoint.
fn api_list_request(provider: &CustomProvider) -> (String, Vec<String>) {
    let base = provider.base_url.trim().trim_end_matches('/');
    let url = match provider.api_format {
        ProviderApiFormat::OpenAi | ProviderApiFormat::OpenAiResponses => {
            format!("{base}/models")
        }
        ProviderApiFormat::Anthropic => format!("{}/models", versioned_root(base)),
    };
    let mut headers = auth_headers(provider);
    headers.push("Accept: application/json".to_owned());
    (url, headers)
}

/// The Anthropic SDK root omits the version path; requests add it. A root
/// that already carries `/v1` is not versioned twice.
fn versioned_root(base: &str) -> String {
    if base.ends_with("/v1") {
        base.to_owned()
    } else {
        format!("{base}/v1")
    }
}

/// The headers every request to the provider carries, in the API format's
/// authentication style.
fn auth_headers(provider: &CustomProvider) -> Vec<String> {
    let mut headers = vec!["User-Agent: fintwind".to_owned()];
    match provider.api_format {
        ProviderApiFormat::OpenAi | ProviderApiFormat::OpenAiResponses => {
            if !provider.api_key.trim().is_empty() {
                headers.push(format!("Authorization: Bearer {}", provider.api_key.trim()));
            }
        }
        ProviderApiFormat::Anthropic => {
            if !provider.api_key.trim().is_empty() {
                headers.push(format!("x-api-key: {}", provider.api_key.trim()));
            }
            headers.push("anthropic-version: 2023-06-01".to_owned());
        }
    }
    headers
}

/// Parse a model-list response body. Every major shape is accepted: the
/// OpenAI/Anthropic `{"data": [...]}`, Gemini's `{"models": [...]}`, a bare
/// array, and entries as bare id strings. Gemini nests ids under `name`
/// (`models/gemini-2.0-flash`); display names ride `displayName`/`display_name`.
pub fn parse_api_model_list(body: &str) -> anyhow::Result<Vec<ProviderApiModel>> {
    let document: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| anyhow::anyhow!("the response is not valid JSON: {error}"))?;
    let entries = document
        .get("data")
        .and_then(serde_json::Value::as_array)
        .or_else(|| document.get("models").and_then(serde_json::Value::as_array))
        .or_else(|| document.as_array())
        .ok_or_else(|| anyhow::anyhow!("the response carries no model list"))?;
    let mut models = Vec::with_capacity(entries.len());
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        let (id, name, context_window, output_limit) = match entry {
            serde_json::Value::String(id) => (id.clone(), None, None, None),
            serde_json::Value::Object(entry) => {
                let raw_id = entry
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| entry.get("name").and_then(serde_json::Value::as_str));
                let Some(raw_id) = raw_id else {
                    continue;
                };
                // Gemini names ids "models/gemini-2.0-flash"; the bare id is
                // what sessions and OpenCode's config use.
                let id = raw_id.strip_prefix("models/").unwrap_or(raw_id);
                let name = ["display_name", "displayName", "name"]
                    .iter()
                    .filter_map(|key| entry.get(*key).and_then(serde_json::Value::as_str))
                    .find(|name| !name.is_empty() && *name != id && *name != raw_id)
                    .map(str::to_owned);
                let context_window = ["context_window", "contextWindow", "inputTokenLimit"]
                    .iter()
                    .find_map(|key| entry.get(*key).and_then(serde_json::Value::as_u64));
                let output_limit = ["output_limit", "outputLimit", "outputTokenLimit"]
                    .iter()
                    .find_map(|key| entry.get(*key).and_then(serde_json::Value::as_u64));
                (id.to_owned(), name, context_window, output_limit)
            }
            _ => continue,
        };
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        models.push(ProviderApiModel {
            id,
            name,
            context_window,
            output_limit,
        });
    }
    if models.is_empty() {
        return Err(anyhow::anyhow!(
            "the response carries no model list entries"
        ));
    }
    Ok(models)
}

/// `http://` or `https://`, the minimum a fetchable endpoint needs.
pub fn base_url_valid(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
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

    fn provider(format: ProviderApiFormat, base_url: &str, api_key: &str) -> CustomProvider {
        CustomProvider::new(
            "relay".into(),
            "Relay".into(),
            base_url.into(),
            format,
            api_key.into(),
            Vec::new(),
        )
    }

    #[test]
    fn display_name_skips_absent_empty_and_id_copies() {
        let mut model = CustomProviderModel {
            id: "deepseek-chat".into(),
            ..Default::default()
        };
        assert_eq!(model.display_name(), None);
        model.name = Some(String::new());
        assert_eq!(model.display_name(), None);
        model.name = Some("deepseek-chat".into());
        assert_eq!(model.display_name(), None, "a name equal to the id carries no information");
        model.name = Some("DeepSeek Chat".into());
        assert_eq!(model.display_name(), Some("DeepSeek Chat"));
    }

    #[test]
    fn list_request_targets_the_formats_endpoint_with_its_auth() {
        let (url, headers) = api_list_request(&provider(
            ProviderApiFormat::OpenAi,
            "https://api.example.com/v1/",
            "sk-test",
        ));
        assert_eq!(url, "https://api.example.com/v1/models");
        assert!(headers
            .iter()
            .any(|header| header == "Authorization: Bearer sk-test"));

        // The Anthropic SDK root omits the version path; the request adds it.
        let (url, headers) = api_list_request(&provider(
            ProviderApiFormat::Anthropic,
            "https://api.anthropic.com",
            "sk-ant",
        ));
        assert_eq!(url, "https://api.anthropic.com/v1/models");
        assert!(headers.iter().any(|header| header == "x-api-key: sk-ant"));
        assert!(headers
            .iter()
            .any(|header| header == "anthropic-version: 2023-06-01"));

        // A root that already carries /v1 is not versioned twice.
        let (url, _) = api_list_request(&provider(
            ProviderApiFormat::Anthropic,
            "https://relay.example.com/v1",
            "",
        ));
        assert_eq!(url, "https://relay.example.com/v1/models");

        // An empty key sends no credential header at all.
        let (_, headers) = api_list_request(&provider(
            ProviderApiFormat::OpenAi,
            "https://api.example.com",
            "  ",
        ));
        assert!(!headers
            .iter()
            .any(|header| header.starts_with("Authorization")));
    }

    #[test]
    fn parses_openai_anthropic_and_gemini_list_shapes() {
        // OpenAI-compatible: bare ids under data.
        let models = parse_api_model_list(
            r#"{"object":"list","data":[{"id":"deepseek-chat","object":"model"},{"id":"deepseek-reasoner","object":"model"}]}"#,
        )
        .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "deepseek-chat");
        assert_eq!(models[0].name, None);

        // Anthropic: display_name beside the id.
        let models = parse_api_model_list(
            r#"{"data":[{"type":"model","id":"claude-sonnet-4-5","display_name":"Claude Sonnet 4.5"}]}"#,
        )
        .unwrap();
        assert_eq!(models[0].id, "claude-sonnet-4-5");
        assert_eq!(models[0].name.as_deref(), Some("Claude Sonnet 4.5"));

        // Gemini: models/named ids with token limits and a displayName.
        let models = parse_api_model_list(
            r#"{"models":[{"name":"models/gemini-2.0-flash","displayName":"Gemini 2.0 Flash","inputTokenLimit":1048576,"outputTokenLimit":8192}]}"#,
        )
        .unwrap();
        assert_eq!(models[0].id, "gemini-2.0-flash");
        assert_eq!(models[0].name.as_deref(), Some("Gemini 2.0 Flash"));
        assert_eq!(models[0].context_window, Some(1_048_576));
        assert_eq!(models[0].output_limit, Some(8_192));

        // A bare array of id strings also parses; duplicates collapse.
        let models = parse_api_model_list(r#"["a-model","a-model","b-model"]"#).unwrap();
        assert_eq!(
            models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(),
            vec!["a-model", "b-model"]
        );

        // Entries without an id are skipped; an empty list is an error so the
        // action reports something instead of silently doing nothing.
        assert!(parse_api_model_list(r#"{"data":[{"object":"model"}]}"#).is_err());
        assert!(parse_api_model_list(r#"{"error":{"message":"nope"}}"#).is_err());
        assert!(parse_api_model_list("not json").is_err());
    }

    #[test]
    fn npm_packages_map_the_three_formats() {
        assert_eq!(ProviderApiFormat::OpenAi.npm_package(), "@ai-sdk/openai-compatible");
        assert_eq!(ProviderApiFormat::OpenAiResponses.npm_package(), "@ai-sdk/openai");
        assert_eq!(ProviderApiFormat::Anthropic.npm_package(), "@ai-sdk/anthropic");
        // The round trip is what keeps a saved entry loading as it saved:
        // OpenCode reads the package and picks the API shape from it.
        for format in ProviderApiFormat::ALL {
            let round = serde_json::to_value(format).unwrap();
            assert_eq!(round.as_str().unwrap(), match format {
                ProviderApiFormat::OpenAi => "openai",
                ProviderApiFormat::OpenAiResponses => "openai-responses",
                ProviderApiFormat::Anthropic => "anthropic",
            });
        }
    }

    #[test]
    fn first_token_request_targets_the_formats_chat_endpoint() {
        let (url, headers, body) = first_token_request(
            &provider(ProviderApiFormat::OpenAi, "https://api.example.com/v1/", "sk-test"),
            "deepseek-chat",
        );
        assert_eq!(url, "https://api.example.com/v1/chat/completions");
        assert!(headers
            .iter()
            .any(|header| header == "Authorization: Bearer sk-test"));
        assert!(headers.iter().any(|header| header == "Content-Type: application/json"));
        assert!(headers.iter().any(|header| header == "Accept: text/event-stream"));
        assert!(body.contains(r#""model":"deepseek-chat""#));
        assert!(body.contains(r#""stream":true"#));
        assert!(!body.contains("max_tokens"), "the newer OpenAI models reject max_tokens");

        // Anthropic: the versioned messages endpoint with its minimum
        // required output budget.
        let (url, headers, body) = first_token_request(
            &provider(ProviderApiFormat::Anthropic, "https://relay.example.com", ""),
            "claude-sonnet-4-5",
        );
        assert_eq!(url, "https://relay.example.com/v1/messages");
        assert!(headers.iter().any(|header| header == "anthropic-version: 2023-06-01"));
        assert!(body.contains(r#""max_tokens":1"#));
        assert!(body.contains(r#""stream":true"#));

        // A model id with a quote is escaped, not spliced into the JSON.
        let (_, _, body) = first_token_request(
            &provider(ProviderApiFormat::OpenAi, "https://api.example.com", ""),
            "we\"ird\\model",
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["model"],
            "we\"ird\\model"
        );

        // The Responses format posts to the responses path with the Responses
        // input shape, and carries no token budget (its floor varies).
        let (url, headers, body) = first_token_request(
            &provider(ProviderApiFormat::OpenAiResponses, "https://api.example.com/v1/", "sk-test"),
            "gpt-5.4",
        );
        assert_eq!(url, "https://api.example.com/v1/responses");
        assert!(headers
            .iter()
            .any(|header| header == "Authorization: Bearer sk-test"));
        assert!(body.contains(r#""model":"gpt-5.4""#));
        assert!(body.contains(r#""input":"Hi""#));
        assert!(body.contains(r#""stream":true"#));
        assert!(!body.contains("output_tokens"));

        // The Responses format lists models through the OpenAI-style list
        // endpoint with Bearer authentication.
        let (url, headers) = api_list_request(&provider(
            ProviderApiFormat::OpenAiResponses,
            "https://relay.example.com",
            "sk-test",
        ));
        assert_eq!(url, "https://relay.example.com/models");
        assert!(headers
            .iter()
            .any(|header| header == "Authorization: Bearer sk-test"));
    }

    #[test]
    fn first_token_latency_rejects_an_invalid_base_url_without_network() {
        assert_eq!(
            first_token_latency(&provider(ProviderApiFormat::OpenAi, "ftp://nope", ""), "m"),
            Err(FirstTokenError::InvalidBaseUrl)
        );
    }

    #[test]
    fn error_messages_surface_from_the_common_shapes() {
        assert_eq!(
            error_message(r#"{"error":{"message":"bad key","type":"auth"}}"#).as_deref(),
            Some("bad key")
        );
        assert_eq!(
            error_message(r#"{"type":"error","error":{"type":"x","message":"nope"}}"#).as_deref(),
            Some("nope")
        );
        assert_eq!(
            error_message(r#"{"message":"relay says no"}"#).as_deref(),
            Some("relay says no")
        );
        assert_eq!(error_message(r#"{"error":"plain string error"}"#).as_deref(), Some("plain string error"));
        assert_eq!(error_message("not json"), None);
        assert_eq!(error_message(r#"{"error":{"message":"  "}}"#), None);
    }

    #[test]
    fn api_list_errors_display_their_english_details() {
        assert_eq!(
            ApiListError::AuthRejected(401).to_string(),
            "HTTP 401 (check the API key)"
        );
        assert_eq!(
            ApiListError::ListMissing.to_string(),
            "HTTP 404 (check the Base URL)"
        );
        assert_eq!(ApiListError::HttpStatus(502).to_string(), "HTTP 502");
    }
}

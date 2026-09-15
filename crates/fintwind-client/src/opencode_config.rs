//! OpenCode's own configuration file is the single source of truth for model
//! providers.
//!
//! The Providers settings page loads its working roster from, and commits
//! edits straight back to, OpenCode's global configuration —
//! `~/.config/opencode/opencode.json` (the same file the CLI and TUI read).
//! The app keeps no parallel store, so a provider added with `opencode
//! auth`-managed credentials or by hand in an editor shows up here and vice
//! versa. OpenCode watches the file and hot-reloads, so a commit reaches
//! running `opencode serve` processes without a restart.
//!
//! Saves are deliberately conservative: the document is read fresh, only the
//! `provider` map is regenerated from the working roster, the top-level
//! `disabled_providers` list is reconciled with the roster's enabled flags,
//! and per-provider unknown fields (`$schema`, `mcp`, `skills`, cost tables,
//! `options.setCacheKey`, …) are carried over untouched. The one lossy case
//! is a provider entry edited in an editor between a page load and a commit
//! — the roster read at page open wins.
//!
//! Map iteration order is a build property, not a data property. serde_json
//! iterates maps in insertion order when the `preserve_order` feature is in
//! the dependency graph (gpui pulls it in, so app builds see the file's own
//! order) and in sorted key order without it. Lists this module writes back
//! or asserts on — `disabled_providers`, MCP `environment`/`headers` pairs —
//! are sorted explicitly so they cannot shuffle with the build; loaded entity
//! lists (providers, models, servers) keep the map's order, and no test
//! asserts it.
//!
//! Verified against `opencode` 0.0.0-beta-18743:
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
    // OpenCode speaks the Responses API to whatever carries the
    // `@ai-sdk/openai` package and Chat Completions to
    // `@ai-sdk/openai-compatible`; the UI's format mirrors that choice.
    let api_format = match npm {
        Some("@ai-sdk/anthropic") => ProviderApiFormat::Anthropic,
        Some("@ai-sdk/openai") => ProviderApiFormat::OpenAiResponses,
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
                    // Stored as recorded; whether a name is worth showing is
                    // `CustomProviderModel::display_name`'s one decision.
                    name: spec.get("name").and_then(Value::as_str).map(str::to_owned),
                    output_limit: spec.pointer("/limit/output").and_then(Value::as_u64),
                    input_modalities: spec
                        .pointer("/modalities/input")
                        .and_then(Value::as_array)
                        .map(|modalities| {
                            modalities
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
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

    let managed: HashSet<&str> = providers
        .iter()
        .map(|provider| provider.slug.as_str())
        .collect();

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
    // This list is written back to disk; sort it so it cannot shuffle with
    // the build's map iteration order (see the module note).
    disabled.sort();
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
        // OpenCode's model display name. A catalog-filled name is the
        // roster's value and is written over any stale one; an entry recorded
        // without a real name keeps what it has and otherwise shows the id.
        match model.display_name() {
            Some(name) => {
                spec.insert("name".into(), Value::String(name.to_owned()));
            }
            None => {
                spec.entry("name".to_owned())
                    .or_insert_with(|| Value::String(model.id.clone()));
            }
        }
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
                // A fetched output limit fills the missing slot; an output the
                // entry already carries is never overwritten.
                limit.entry("output".to_owned()).or_insert_with(|| {
                    Value::from(model.output_limit.unwrap_or(DEFAULT_OUTPUT_LIMIT))
                });
                spec.insert("limit".into(), Value::Object(limit));
            }
            None => {
                if let Some(limit) =
                    spec.get("limit")
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
        // `modalities.input` carries the recorded input modalities. An unset
        // list drops just that key, leaving any other `modalities` keys
        // (`output`, or anything a newer schema adds) exactly as recorded.
        if model.input_modalities.is_empty() {
            let mut modalities = spec.get("modalities").and_then(Value::as_object).cloned();
            if let Some(modalities) = modalities.as_mut() {
                modalities.remove("input");
            }
            match modalities {
                Some(modalities) if modalities.is_empty() => {
                    spec.remove("modalities");
                }
                Some(modalities) => {
                    spec.insert("modalities".into(), Value::Object(modalities));
                }
                None => {}
            }
        } else {
            let mut modalities = spec
                .get("modalities")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            modalities.insert(
                "input".into(),
                Value::Array(
                    model
                        .input_modalities
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
            spec.insert("modalities".into(), Value::Object(modalities));
        }
        // The pre-OpenCode-config mirror wrote `contextWindow`; superseded
        // by `limit.context` and never read back once `limit` exists.
        spec.remove("contextWindow");
        crate::provider_thinking::fill_model_variants(
            &mut spec,
            &model.id,
            model.display_name(),
            provider.api_format,
        );
        models.insert(model.id.clone(), Value::Object(spec));
    }
    entry.insert("models".into(), Value::Object(models));

    Value::Object(entry)
}

fn write_mcp_oauth(entry: &mut Map<String, Value>, oauth: &McpOAuth) {
    match oauth.mode {
        McpOAuthMode::Automatic => {
            entry.remove("oauth");
        }
        McpOAuthMode::Disabled => {
            entry.insert("oauth".into(), Value::Bool(false));
        }
        McpOAuthMode::Custom => {
            let mut map = Map::new();
            set_or_remove_string(&mut map, "clientId", &oauth.client_id);
            set_or_remove_string(&mut map, "clientSecret", &oauth.client_secret);
            set_or_remove_string(&mut map, "scope", &oauth.scope);
            entry.insert("oauth".into(), Value::Object(map));
        }
    }
}

fn set_or_remove_string(map: &mut Map<String, Value>, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        map.remove(key);
    } else {
        map.insert(key.into(), Value::String(value.to_owned()));
    }
}

/// One MCP server under opencode.json's top-level `mcp` map. Verified
/// against opencode's published schema: a `local` server carries `command`
/// (an argv vector) plus `environment`, a `remote` server carries `url`
/// plus `headers` and optional `oauth`; both may carry `enabled` and unknown
/// fields (`cwd`, `timeout`, …) which ride along in [`McpServer::raw`].
#[derive(Clone, Debug, PartialEq)]
pub struct McpServer {
    /// The entry's key in the `mcp` map — also its display name.
    pub name: String,
    pub kind: McpServerKind,
    /// `local` only: the argv vector the server is launched with.
    pub command: Vec<String>,
    /// `remote` only: the server endpoint.
    pub url: String,
    /// The entry's `environment` object, as ordered pairs. Parsed whatever
    /// the kind, so re-typing a server never silently turns its environment
    /// into headers or loses it. Entries with empty keys are dropped on
    /// save.
    pub environment: Vec<(String, String)>,
    /// The entry's `headers` object, as ordered pairs.
    pub headers: Vec<(String, String)>,
    /// `remote` only: OpenCode OAuth. Parsed whatever the kind so re-typing
    /// never drops a custom client. Written only while the server is remote.
    pub oauth: McpOAuth,
    pub enabled: bool,
    /// The original record, preserved so unknown fields ride along on save.
    pub raw: Value,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum McpServerKind {
    #[default]
    Local,
    Remote,
}

/// How a remote MCP server authenticates over OAuth.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum McpOAuthMode {
    /// Omit `oauth` so OpenCode auto-detects a 401 and runs the flow.
    #[default]
    Automatic,
    /// `oauth: false` — API keys / headers only.
    Disabled,
    /// Pre-registered client credentials.
    Custom,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpOAuth {
    pub mode: McpOAuthMode,
    pub client_id: String,
    pub client_secret: String,
    pub scope: String,
}

impl McpServerKind {
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }
}

/// Load the user-configured MCP servers from OpenCode's configuration. The
/// file holds exactly what the UI presents as editable servers.
pub fn load_mcp_servers() -> io::Result<Vec<McpServer>> {
    load_mcp_servers_at(&config_path())
}

pub fn load_mcp_servers_at(path: &Path) -> io::Result<Vec<McpServer>> {
    let bytes = std::fs::read(path)?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let servers = document
        .get("mcp")
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .map(|(key, entry)| mcp_server_from_config(key, entry))
                .collect()
        })
        .unwrap_or_default();
    Ok(servers)
}

fn mcp_server_from_config(key: &str, entry: &Value) -> McpServer {
    let kind = match entry.get("type").and_then(Value::as_str) {
        Some("remote") => McpServerKind::Remote,
        // OpenCode's schema requires an explicit type; a hand-written entry
        // without one — or with an unknown one, which OpenCode's validation
        // would reject — still loads as local so it stays visible and
        // fixable instead of vanishing.
        _ => McpServerKind::Local,
    };
    let string_pairs = |table_key: &str| {
        entry
            .get(table_key)
            .and_then(Value::as_object)
            .map(|entries| {
                let mut pairs: Vec<(String, String)> = entries
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_owned()))
                    })
                    .collect();
                // Sort by key — see the module note on map iteration order.
                pairs.sort_by(|a, b| a.0.cmp(&b.0));
                pairs
            })
            .unwrap_or_default()
    };
    McpServer {
        name: key.to_owned(),
        kind,
        command: entry
            .get("command")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        url: entry
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        environment: string_pairs("environment"),
        headers: string_pairs("headers"),
        oauth: mcp_oauth_from_config(entry),
        // OpenCode runs servers whose entries omit `enabled`, so absence
        // loads as on and the UI's toggle then writes the flag explicitly.
        enabled: entry
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        raw: entry.clone(),
    }
}

fn mcp_oauth_from_config(entry: &Value) -> McpOAuth {
    match entry.get("oauth") {
        Some(Value::Bool(false)) => McpOAuth {
            mode: McpOAuthMode::Disabled,
            ..McpOAuth::default()
        },
        Some(Value::Object(map)) => {
            let client_id = map
                .get("clientId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let client_secret = map
                .get("clientSecret")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let scope = map
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let custom = !client_id.trim().is_empty()
                || !client_secret.trim().is_empty()
                || !scope.trim().is_empty();
            McpOAuth {
                mode: if custom {
                    McpOAuthMode::Custom
                } else {
                    McpOAuthMode::Automatic
                },
                client_id,
                client_secret,
                scope,
            }
        }
        _ => McpOAuth::default(),
    }
}

/// Commit the working roster to OpenCode's configuration. The `mcp` map is
/// rebuilt from `servers` (per-entry unknown fields ride along in
/// [`McpServer::raw`]); everything outside it is preserved. An empty roster
/// drops the key entirely.
pub fn save_mcp_servers(servers: &[McpServer]) -> io::Result<()> {
    save_mcp_servers_at(&config_path(), servers)
}

pub fn save_mcp_servers_at(path: &Path, servers: &[McpServer]) -> io::Result<()> {
    let mut document: Map<String, Value> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
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

    if servers.is_empty() {
        document.remove("mcp");
    } else {
        let mut map = Map::new();
        for server in servers {
            map.insert(server.name.clone(), mcp_server_entry(server));
        }
        document.insert("mcp".into(), Value::Object(map));
    }

    write_json_atomically(path, &Value::Object(document))
}

/// The server entry as it should sit in the configuration: the original
/// record with only the UI-owned keys rewritten, and the keys of the other
/// kind dropped so switching a server's type cannot leave a hybrid entry
/// OpenCode's schema rejects.
fn mcp_server_entry(server: &McpServer) -> Value {
    let mut entry = match server.raw.as_object() {
        Some(raw) => raw.clone(),
        None => Map::new(),
    };
    entry.insert(
        "type".into(),
        Value::String(server.kind.as_config().to_owned()),
    );

    // Only the kind's own table is written (and only when non-empty); the
    // other kind's key is dropped. The inactive table stays on the working
    // roster, so flipping a server's type back and forth in the UI never
    // loses what the user typed — it just is not in the file while inactive.
    match server.kind {
        McpServerKind::Local => {
            set_or_remove_string_table(&mut entry, "environment", &server.environment);
            entry.remove("headers");
            entry.remove("oauth");
            entry.insert(
                "command".into(),
                Value::Array(
                    server
                        .command
                        .iter()
                        .map(|part| Value::String(part.clone()))
                        .collect(),
                ),
            );
            entry.remove("url");
        }
        McpServerKind::Remote => {
            set_or_remove_string_table(&mut entry, "headers", &server.headers);
            entry.remove("environment");
            set_or_remove_string(&mut entry, "url", &server.url);
            entry.remove("command");
            write_mcp_oauth(&mut entry, &server.oauth);
        }
    }

    entry.insert("enabled".into(), Value::from(server.enabled));
    Value::Object(entry)
}

/// Insert `pairs` as an object under `key`, dropping blank keys and removing
/// the key entirely when nothing remains.
fn set_or_remove_string_table(
    entry: &mut Map<String, Value>,
    key: &str,
    pairs: &[(String, String)],
) {
    let mut table = Map::new();
    for (name, value) in pairs {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        table.insert(name.to_owned(), Value::String(value.clone()));
    }
    if table.is_empty() {
        entry.remove(key);
    } else {
        entry.insert(key.into(), Value::Object(table));
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
    let migrated = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|document| {
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
    fn an_entry_with_the_openai_package_loads_as_the_responses_format() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-oc-cfg-resp-{}", std::process::id()));
        let document = serde_json::json!({
            "provider": {
                "my-relay": {
                    "npm": "@ai-sdk/openai",
                    "name": "My Relay",
                    "options": {"apiKey": "sk-relay", "baseURL": "https://relay.example.com/v1"},
                    "models": {"gpt-5.4": {"name": "GPT 5.4"}}
                }
            }
        });
        let path = write_fixture(&directory, &document);

        let providers = load_providers_at(&path).unwrap();
        let relay = providers.iter().find(|p| p.slug == "my-relay").unwrap();
        // OpenCode instantiates this package with `.responses(...)`, so the
        // roster shows the Responses format without rewriting the entry.
        assert_eq!(relay.api_format, ProviderApiFormat::OpenAiResponses);
        assert_eq!(relay.npm_touched, false);

        // Saving without a format change keeps the original package.
        // Path-scoped save: the global `save_providers` would rewrite the
        // user's real configuration from this fixture's roster.
        save_providers_at(&path, &providers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["provider"]["my-relay"]["npm"], "@ai-sdk/openai");

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn saving_a_new_provider_writes_thinking_modes_and_survives_reload() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-thinking-config-{}", uuid::Uuid::new_v4()));
        let path = write_fixture(
            &directory,
            &serde_json::json!({"instructions": ["keep.md"]}),
        );
        let providers = vec![CustomProvider::new(
            "thinking-test".into(),
            "Thinking test".into(),
            "https://example.invalid/v1".into(),
            ProviderApiFormat::OpenAiResponses,
            "test-key".into(),
            vec![CustomProviderModel {
                id: "gpt-5.5".into(),
                ..Default::default()
            }],
        )];
        save_providers_at(&path, &providers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let model = &saved["provider"]["thinking-test"]["models"]["gpt-5.5"];
        assert_eq!(model["variants"].as_object().unwrap().len(), 6);
        assert_eq!(model["options"]["reasoningEffort"], "xhigh");
        assert_eq!(saved["instructions"], serde_json::json!(["keep.md"]));
        let loaded = load_providers_at(&path).unwrap();
        save_providers_at(&path, &loaded).unwrap();
        let reloaded: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved, reloaded);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn load_reads_roster_with_enabled_flags_and_context_windows() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-oc-cfg-load-{}", std::process::id()));
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
            std::env::temp_dir().join(format!("fintwind-oc-cfg-save-{}", std::process::id()));
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
                ..Default::default()
            }],
        ));

        save_providers_at(&path, &providers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

        // Unrelated top-level keys survive verbatim.
        assert_eq!(saved["mcp"]["context7"]["url"], "https://mcp.example.com");
        assert_eq!(
            saved["skills"]["paths"][0],
            "C:/Users/example/.config/opencode/skills"
        );
        // The disabled catalog provider outside the roster stays listed.
        assert_eq!(
            saved["disabled_providers"],
            serde_json::json!(["catalog-provider", "deepseek"])
        );

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
        assert_eq!(
            saved["disabled_providers"],
            serde_json::json!(["catalog-provider"])
        );

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn save_preserves_absent_npm_and_clearing_a_context_window_keeps_output() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-oc-cfg-npm-{}", std::process::id()));
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
        assert_eq!(
            saved["provider"]["openrouter"]["options"]["apiKey"],
            "sk-or"
        );
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
    fn input_modalities_round_trip_and_leave_other_modality_keys_alone() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-oc-cfg-modalities-{}", std::process::id()));
        let document = serde_json::json!({
            "provider": {
                "relay": {
                    "options": {"baseURL": "https://relay.example.com/v1"},
                    "models": {
                        "vision-model": {
                            "name": "Vision Model",
                            "cost": {"input": 0.5, "output": 1.0},
                            "modalities": {
                                "input": ["text", "image"],
                                "output": ["text"],
                                "custom": {"future": true}
                            }
                        },
                        "plain-model": {"name": "Plain Model"}
                    }
                }
            }
        });
        let path = write_fixture(&directory, &document);
        let mut providers = load_providers_at(&path).unwrap();

        // The recorded list loads as recorded.
        let relay = providers.iter_mut().find(|p| p.slug == "relay").unwrap();
        let vision = relay.model("vision-model").unwrap();
        assert_eq!(vision.input_modalities, vec!["text", "image"]);
        assert!(
            relay
                .model("plain-model")
                .unwrap()
                .input_modalities
                .is_empty()
        );

        // A fresh selection replaces `input` and leaves `output` in place.
        let relay = providers.iter_mut().find(|p| p.slug == "relay").unwrap();
        let vision = relay
            .models
            .iter_mut()
            .find(|model| model.id == "vision-model")
            .unwrap();
        vision.input_modalities = vec!["text".into(), "pdf".into()];
        let plain = relay
            .models
            .iter_mut()
            .find(|model| model.id == "plain-model")
            .unwrap();
        plain.input_modalities = vec!["text".into()];
        save_providers_at(&path, &providers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let models = &saved["provider"]["relay"]["models"];
        assert_eq!(
            models["vision-model"]["modalities"],
            serde_json::json!({"input": ["text", "pdf"], "output": ["text"], "custom": {"future": true}})
        );
        // Unknown model-spec keys ride along through a modalities edit.
        assert_eq!(
            models["vision-model"]["cost"],
            serde_json::json!({"input": 0.5, "output": 1.0})
        );
        assert_eq!(
            models["plain-model"]["modalities"],
            serde_json::json!({"input": ["text"]})
        );

        // Clearing the selection drops `input` alone; the emptied object the
        // removal would leave behind is dropped too, and the `modalities`
        // object's unknown siblings survive both edits.
        let mut providers = load_providers_at(&path).unwrap();
        let relay = providers.iter_mut().find(|p| p.slug == "relay").unwrap();
        for model in &mut relay.models {
            model.input_modalities.clear();
        }
        save_providers_at(&path, &providers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let models = &saved["provider"]["relay"]["models"];
        assert_eq!(
            models["vision-model"]["modalities"],
            serde_json::json!({"output": ["text"], "custom": {"future": true}})
        );
        assert!(models["plain-model"].get("modalities").is_none());

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn context_window_helpers_agree_with_the_schema() {
        // The stored value round-trips through the UI's free-text field.
        let window = parse_context_window(&format_context_window(1_000_000)).unwrap();
        assert_eq!(window, 1_000_000);
    }

    fn sample_mcp_document() -> Value {
        serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "provider": {"deepseek": {"models": {"deepseek-chat": {"name": "DeepSeek Chat"}}}},
            "mcp": {
                "charts": {
                    "type": "local",
                    "command": ["npx", "-y", "@antv/mcp-server-chart"],
                    "environment": {"NODE_ENV": "production", "DEBUG": "1"},
                    "timeout": 9000,
                    "enabled": false
                },
                "context7": {"type": "remote", "url": "https://mcp.example.com", "headers": {"Authorization": "Bearer sk-test"}, "enabled": true},
                "no-type": {"command": ["bun", "x", "some-server"]},
                "org-remote": {"type": "remote", "url": "https://org.example.com/mcp", "oauth": false}
            }
        })
    }

    #[test]
    fn load_mcp_reads_kinds_variables_and_enabled_flags() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-oc-mcp-load-{}", std::process::id()));
        let path = write_fixture(&directory, &sample_mcp_document());

        let servers = load_mcp_servers_at(&path).unwrap();
        assert_eq!(servers.len(), 4);

        let charts = servers.iter().find(|s| s.name == "charts").unwrap();
        assert_eq!(charts.kind, McpServerKind::Local);
        assert_eq!(charts.command, vec!["npx", "-y", "@antv/mcp-server-chart"]);
        assert_eq!(
            // Sorted by `mcp_server_from_config`; see the module note on map
            // iteration order.
            charts.environment,
            vec![
                ("DEBUG".to_owned(), "1".to_owned()),
                ("NODE_ENV".to_owned(), "production".to_owned())
            ]
        );
        assert!(charts.headers.is_empty());
        assert!(!charts.enabled);

        let context7 = servers.iter().find(|s| s.name == "context7").unwrap();
        assert_eq!(context7.kind, McpServerKind::Remote);
        assert_eq!(context7.url, "https://mcp.example.com");
        assert_eq!(
            context7.headers,
            vec![("Authorization".to_owned(), "Bearer sk-test".to_owned())]
        );
        assert!(context7.enabled);

        // An entry without a type still loads, from the command it carries.
        let no_type = servers.iter().find(|s| s.name == "no-type").unwrap();
        assert_eq!(no_type.kind, McpServerKind::Local);
        assert!(no_type.enabled);

        // An org remote without `enabled` loads as on, matching OpenCode.
        let org = servers.iter().find(|s| s.name == "org-remote").unwrap();
        assert_eq!(org.kind, McpServerKind::Remote);
        assert!(org.enabled);
        assert_eq!(org.oauth.mode, McpOAuthMode::Disabled);

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn save_mcp_writes_remote_oauth_modes() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-oc-mcp-oauth-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("opencode.json");

        let automatic = McpServer {
            name: "auto".into(),
            kind: McpServerKind::Remote,
            command: Vec::new(),
            url: "https://auto.example.com/mcp".into(),
            environment: Vec::new(),
            headers: Vec::new(),
            oauth: McpOAuth::default(),
            enabled: true,
            raw: Value::Null,
        };
        let custom = McpServer {
            name: "custom".into(),
            kind: McpServerKind::Remote,
            command: Vec::new(),
            url: "https://custom.example.com/mcp".into(),
            environment: Vec::new(),
            headers: Vec::new(),
            oauth: McpOAuth {
                mode: McpOAuthMode::Custom,
                client_id: "id".into(),
                client_secret: String::new(),
                scope: "tools:read".into(),
            },
            enabled: true,
            raw: Value::Null,
        };
        save_mcp_servers_at(&path, &[automatic, custom]).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(saved["mcp"]["auto"].get("oauth").is_none());
        assert_eq!(saved["mcp"]["custom"]["oauth"]["clientId"], "id");
        assert!(
            saved["mcp"]["custom"]["oauth"]
                .get("clientSecret")
                .is_none()
        );
        assert_eq!(saved["mcp"]["custom"]["oauth"]["scope"], "tools:read");

        let loaded = load_mcp_servers_at(&path).unwrap();
        let custom = loaded
            .iter()
            .find(|server| server.name == "custom")
            .unwrap();
        assert_eq!(custom.oauth.mode, McpOAuthMode::Custom);
        assert_eq!(custom.oauth.client_id, "id");
        assert_eq!(custom.oauth.scope, "tools:read");

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn save_mcp_preserves_unknown_keys_and_drops_cross_kind_fields() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-oc-mcp-save-{}", std::process::id()));
        let path = write_fixture(&directory, &sample_mcp_document());
        let mut servers = load_mcp_servers_at(&path).unwrap();

        // Re-type the local charts server as remote with a URL; its command
        // and environment must not survive into the entry.
        let charts = servers.iter_mut().find(|s| s.name == "charts").unwrap();
        charts.kind = McpServerKind::Remote;
        charts.url = "https://charts.example.com/mcp".into();
        let context7 = servers.iter_mut().find(|s| s.name == "context7").unwrap();
        context7.enabled = false;
        context7.headers.clear();
        // Drop no-type entirely; keep org-remote untouched.
        servers.retain(|server| server.name != "no-type");

        save_mcp_servers_at(&path, &servers).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

        // Unrelated top-level keys survive verbatim.
        assert_eq!(
            saved["provider"]["deepseek"]["models"]["deepseek-chat"]["name"],
            "DeepSeek Chat"
        );

        let charts = &saved["mcp"]["charts"];
        assert_eq!(charts["type"], "remote");
        assert_eq!(charts["url"], "https://charts.example.com/mcp");
        assert!(charts.get("command").is_none());
        assert!(charts.get("environment").is_none());
        assert_eq!(charts["timeout"], 9000);

        let context7 = &saved["mcp"]["context7"];
        assert_eq!(context7["enabled"], false);
        assert!(context7.get("headers").is_none());
        assert_eq!(context7["url"], "https://mcp.example.com");

        // The dropped entry is gone; the untouched one kept its unknowns.
        assert!(saved["mcp"].get("no-type").is_none());
        assert_eq!(saved["mcp"]["org-remote"]["oauth"], false);

        // Round-trip: the saved file loads back to the same roster.
        let reloaded = load_mcp_servers_at(&path).unwrap();
        assert_eq!(reloaded.len(), 3);
        let charts = reloaded.iter().find(|s| s.name == "charts").unwrap();
        assert_eq!(charts.kind, McpServerKind::Remote);

        // An empty roster drops the key; the rest of the document survives.
        save_mcp_servers_at(&path, &[]).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(saved.get("mcp").is_none());
        assert!(saved.get("provider").is_some());

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn save_mcp_drops_blank_variables_and_writes_fresh_documents() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-oc-mcp-fresh-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("opencode.json");

        let server = McpServer {
            name: "fresh-server".into(),
            kind: McpServerKind::Local,
            command: vec!["bun".into(), "x".into(), "some-server".into()],
            url: String::new(),
            environment: vec![
                ("KEY".to_owned(), "value".to_owned()),
                (String::new(), "dropped".to_owned()),
                ("   ".to_owned(), "dropped".to_owned()),
            ],
            headers: Vec::new(),
            oauth: McpOAuth::default(),
            enabled: true,
            raw: Value::Null,
        };
        save_mcp_servers_at(&path, &[server]).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

        // A fresh file gains OpenCode's schema pin alongside the entry.
        assert_eq!(saved["$schema"], "https://opencode.ai/config.json");
        let entry = &saved["mcp"]["fresh-server"];
        assert_eq!(entry["type"], "local");
        assert_eq!(
            entry["command"],
            serde_json::json!(["bun", "x", "some-server"])
        );
        assert_eq!(entry["environment"], serde_json::json!({"KEY": "value"}));
        assert_eq!(entry["enabled"], true);

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn legacy_override_file_migrates_into_the_real_config() {
        let directory =
            std::env::temp_dir().join(format!("fintwind-oc-cfg-migrate-{}", std::process::id()));
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
        let migrated = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|document| {
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

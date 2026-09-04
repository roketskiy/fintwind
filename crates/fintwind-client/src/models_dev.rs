//! The models.dev catalog as Fintwind's model metadata table.
//!
//! [models.dev](https://models.dev/models/) publishes every provider and
//! model it tracks as one JSON document (`api.json`). This module downloads
//! that document and flattens it into a table of model records grouped by
//! normalized id, so one lookup answers any provider's copy of a model. The
//! Providers page's "fetch models" action matches the models the provider's
//! own API listed against the table and fills in basic data — display names
//! and token limits — without clobbering anything the user recorded by hand.
//!
//! Everything here blocks on the network or the filesystem and must run on
//! the background executor; render reads only what the app entity stores.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::custom_providers::{CustomProvider, CustomProviderModel};

const MODELS_DEV_CATALOG_URL: &str = "https://models.dev/api.json";

/// A fresh in-memory table answers the fetch button without another
/// download — enough to cover stepping through a few providers in a row.
pub const TABLE_REUSE_WINDOW: Duration = Duration::from_secs(600);

/// The api.json document is a few megabytes and grows; a runaway response is
/// a network fault, not a catalog.
const MAX_CATALOG_BYTES: usize = 64 * 1024 * 1024;

/// One record of the catalog. The catalog describes models with far more
/// (costs, dates, flags); these fields are the ones anything reads, so only
/// these are kept — the cache stays small, and a future reader adds its field
/// back as one serde-default line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsDevModel {
    pub id: String,
    pub name: Option<String>,
    pub context_window: Option<u64>,
    pub output_limit: Option<u64>,
    /// The modalities the model accepts, e.g. `["text", "image"]`. Empty when
    /// the catalog does not say.
    #[serde(default)]
    pub input_modalities: Vec<String>,
    /// The modalities the model emits. Empty when the catalog does not say.
    #[serde(default)]
    pub output_modalities: Vec<String>,
}

/// The fill-in basics one model id resolves to in the metadata table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordBasics {
    pub name: Option<String>,
    pub context_window: Option<u64>,
    pub output_limit: Option<u64>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
}

/// The metadata table: the catalog flattened into records grouped by
/// normalized model id, plus when it was fetched. The grouping is built at
/// parse time so a lookup never rescans the catalog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelsDevTable {
    /// Unix milliseconds of the successful fetch that produced this table.
    pub fetched_at: u64,
    /// Records grouped by normalized id (case and separators removed); each
    /// group keeps the exact spellings so an exact match can be preferred
    /// over a separator-insensitive one.
    models: BTreeMap<String, Vec<ModelsDevModel>>,
}

impl ModelsDevTable {
    pub fn age(&self) -> Duration {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since_epoch| since_epoch.as_millis() as u64)
            .unwrap_or(self.fetched_at);
        Duration::from_millis(now.saturating_sub(self.fetched_at))
    }

    /// The input modalities one model id resolves to, same matching as
    /// [`resolve_model_basics`]. Separate from it because the modality badge
    /// runs per visible row per frame and should not pay for the fields it
    /// does not read.
    pub fn resolve_input_modalities(&self, id: &str) -> Vec<String> {
        let Some(group) = self.models.get(&normalize_model_id(id)) else {
            return Vec::new();
        };
        let exact: Vec<&ModelsDevModel> = group.iter().filter(|model| model.id == id).collect();
        let matches: Vec<&ModelsDevModel> = if exact.is_empty() {
            group.iter().collect()
        } else {
            exact
        };
        mode(matches.iter().map(|model| {
            (!model.input_modalities.is_empty()).then(|| model.input_modalities.clone())
        }))
        .unwrap_or_default()
    }

    /// The basics every catalog copy of one model id agrees on: exact ids
    /// first, then separator-insensitive ones; each field is the majority
    /// value across the matches (ties take the later value in sorted-table
    /// order, which is deterministic).
    pub fn resolve_model_basics(&self, id: &str) -> RecordBasics {
        let Some(group) = self.models.get(&normalize_model_id(id)) else {
            return RecordBasics::default();
        };
        let exact: Vec<&ModelsDevModel> = group.iter().filter(|model| model.id == id).collect();
        let matches: Vec<&ModelsDevModel> = if exact.is_empty() {
            group.iter().collect()
        } else {
            exact
        };
        RecordBasics {
            name: mode(matches.iter().map(|model| model.name.clone())),
            context_window: mode(matches.iter().map(|model| model.context_window)),
            output_limit: mode(matches.iter().map(|model| model.output_limit)),
            // Models without modality information do not vote for "empty":
            // the majority is taken over the copies that say.
            input_modalities: mode(
                matches
                    .iter()
                    .map(|model| (!model.input_modalities.is_empty()).then(|| model.input_modalities.clone())),
            )
            .unwrap_or_default(),
            output_modalities: mode(
                matches
                    .iter()
                    .map(|model| (!model.output_modalities.is_empty()).then(|| model.output_modalities.clone())),
            )
            .unwrap_or_default(),
        }
    }
}

/// What one merge pass changed on a provider.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModelsDevApplyOutcome {
    /// Models from the API list the roster did not hold yet.
    pub added: usize,
    /// Existing models that gained a name or window from the table.
    pub filled: usize,
}

/// Merge the models the provider's own API listed into the roster, filling in
/// basic data from the metadata table by model id alone: every catalog
/// provider serving the same id contributes, and the majority value wins, so
/// a model keeps its record no matter which endpoint listed it. Existing
/// models keep their roster order and never lose recorded values; API models
/// the roster lacks are appended in API order. Blocking catalog work must
/// have happened already — the table here is read-only and cheap to query.
pub fn merge_api_models(
    provider: &mut CustomProvider,
    api_models: &[crate::custom_providers::ProviderApiModel],
    table: Option<&ModelsDevTable>,
) -> ModelsDevApplyOutcome {
    let mut outcome = ModelsDevApplyOutcome::default();
    let mut seen: Vec<String> = Vec::with_capacity(api_models.len());
    for model in &mut provider.models {
        // The API speaks for what exists: a roster id the list no longer
        // names is left alone, but one that returns under a respelled id is
        // still recognized so re-fetching never duplicates it.
        if let Some(api_model) = api_models
            .iter()
            .find(|api_model| same_model_id(&api_model.id, &model.id))
        {
            seen.push(api_model.id.clone());
            let basics = table
                .map(|table| table.resolve_model_basics(&model.id))
                .unwrap_or_default();
            let mut touched = false;
            if model.context_window.is_none()
                && let Some(window) = api_model.context_window.or(basics.context_window)
            {
                model.context_window = Some(window);
                touched = true;
            }
            if model.output_limit.is_none()
                && let Some(limit) = api_model.output_limit.or(basics.output_limit)
            {
                model.output_limit = Some(limit);
                touched = true;
            }
            if model.display_name().is_none() {
                // The table's curated name wins; the API's own display name
                // (Anthropic, Gemini) is the fallback when the table holds
                // nothing for this id.
                if let Some(name) = basics.name.or_else(|| api_model.name.clone()) {
                    model.name = Some(name);
                    touched = true;
                }
            }
            if touched {
                outcome.filled += 1;
            }
        }
    }
    for api_model in api_models {
        if seen.iter().any(|id| same_model_id(id, &api_model.id)) {
            continue;
        }
        seen.push(api_model.id.clone());
        let basics = table
            .map(|table| table.resolve_model_basics(&api_model.id))
            .unwrap_or_default();
        provider.models.push(CustomProviderModel {
            id: api_model.id.clone(),
            name: basics.name.or_else(|| api_model.name.clone()),
            context_window: api_model.context_window.or(basics.context_window),
            output_limit: api_model.output_limit.or(basics.output_limit),
        });
        outcome.added += 1;
    }
    outcome
}

/// The most frequent value; a tie keeps the later arrival, and an all-`None`
/// stream yields `None`.
fn mode<T: Clone + PartialEq>(values: impl Iterator<Item = Option<T>>) -> Option<T> {
    let mut counted: Vec<(T, usize)> = Vec::new();
    for value in values.flatten() {
        match counted
            .iter_mut()
            .find(|(candidate, _)| candidate == &value)
        {
            Some((_, count)) => *count += 1,
            None => counted.push((value, 1)),
        }
    }
    counted
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(value, _)| value)
}

fn same_model_id(left: &str, right: &str) -> bool {
    left == right || normalize_model_id(left) == normalize_model_id(right)
}

fn normalize_model_id(id: &str) -> String {
    id.chars()
        .filter(|char| char.is_ascii_alphanumeric())
        .map(|char| char.to_ascii_lowercase())
        .collect()
}

/// Fetch the catalog over the network, parse it, and refresh the on-disk
/// cache. Blocking; call from the background executor.
pub fn fetch_catalog() -> anyhow::Result<ModelsDevTable> {
    let (status, body) = http_get(MODELS_DEV_CATALOG_URL)?;
    if status != 200 {
        return Err(anyhow!("HTTP {status}"));
    }
    if body.len() > MAX_CATALOG_BYTES {
        return Err(anyhow!("response of {} bytes is not a catalog", body.len()));
    }
    let table = parse_catalog(&body)
        .map_err(|error| anyhow!("the response is not a models.dev catalog: {error}"))?;
    write_cached_catalog(&table);
    Ok(table)
}

/// Fetch the catalog, falling back to the newest known copy — the table this
/// session already holds, then the on-disk cache — when the network fails.
/// `None` means no copy could be had at all.
pub fn fetch_catalog_with_fallback(
    session_table: Option<&ModelsDevTable>,
) -> Option<ModelsDevTable> {
    match fetch_catalog() {
        Ok(table) => Some(table),
        Err(_) => session_table.cloned().or_else(cached_catalog),
    }
}

/// Where the fetched table is cached, following the OpenCode catalog cache:
/// debug builds keep it in the checkout's gitignored `temp/` beside the debug
/// database, so development never touches the installed app's cache.
fn catalog_cache_path() -> PathBuf {
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
    directory.join("models-dev.json")
}

/// The table cached by the last successful fetch, or `None` when no fetch has
/// cached one or the file no longer parses. Reads the filesystem, so call it
/// from the background executor, never from render.
pub fn cached_catalog() -> Option<ModelsDevTable> {
    let contents = std::fs::read(catalog_cache_path()).ok()?;
    serde_json::from_slice(&contents).ok()
}

/// Best-effort: a cache that fails to write only costs the next launch its
/// head start.
fn write_cached_catalog(table: &ModelsDevTable) {
    let path = catalog_cache_path();
    let Ok(bytes) = serde_json::to_vec(table) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, bytes).is_ok() {
        let _ = std::fs::rename(&temporary, &path);
    }
}

/// Flatten the models.dev document into the metadata table. serde_json's
/// maps iterate keys in sorted order, so the table is key-sorted too —
/// look entries up by id, never by position.
pub fn parse_catalog(body: &str) -> anyhow::Result<ModelsDevTable> {
    let document: Value = serde_json::from_str(body).context("the response is not valid JSON")?;
    let entries = document
        .as_object()
        .context("the catalog is not an object of providers")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since_epoch| since_epoch.as_millis() as u64)
        .unwrap_or_default();
    let mut models: BTreeMap<String, Vec<ModelsDevModel>> = BTreeMap::new();
    for entry in entries.values() {
        let Some(specs) = entry.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (id, spec) in specs {
            if let Some(record) = parse_model(id, spec) {
                models
                    .entry(normalize_model_id(&record.id))
                    .or_default()
                    .push(record);
            }
        }
    }
    Ok(ModelsDevTable {
        fetched_at: now,
        models,
    })
}

fn parse_model(id: &str, spec: &Value) -> Option<ModelsDevModel> {
    let spec = spec.as_object()?;
    let limit = |key: &str| {
        spec.get("limit")
            .and_then(|limit| limit.get(key))
            .and_then(Value::as_u64)
    };
    let modalities = |key: &str| {
        spec.get("modalities")
            .and_then(|modalities| modalities.get(key))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(ModelsDevModel {
        id: spec
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_owned(),
        name: string_field(spec, "name"),
        context_window: limit("context"),
        output_limit: limit("output"),
        input_modalities: modalities("input"),
        output_modalities: modalities("output"),
    })
}

fn string_field(entry: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// One HTTPS GET through the shared curl helper. Blocking.
fn http_get(url: &str) -> anyhow::Result<(u16, String)> {
    fintwind_protocol::http::http_get(
        url,
        &[
            "User-Agent: fintwind".to_owned(),
            "Accept: application/json".to_owned(),
        ],
        120,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom_providers::{ProviderApiFormat, ProviderApiModel};

    fn provider() -> CustomProvider {
        CustomProvider::new(
            "deepseek".into(),
            "DeepSeek".into(),
            "https://api.deepseek.com/v1".into(),
            ProviderApiFormat::OpenAi,
            String::new(),
            vec![CustomProviderModel {
                id: "deepseek-chat".into(),
                ..Default::default()
            }],
        )
    }

    fn api_model(id: &str, name: Option<&str>) -> ProviderApiModel {
        ProviderApiModel {
            id: id.into(),
            name: name.map(str::to_owned),
            context_window: None,
            output_limit: None,
        }
    }

    fn sample_catalog() -> &'static str {
        r#"{
            "deepseek": {
                "id": "deepseek",
                "name": "DeepSeek",
                "api": "https://api.deepseek.com",
                "npm": "@ai-sdk/openai-compatible",
                "env": ["DEEPSEEK_API_KEY"],
                "models": {
                    "deepseek-chat": {
                        "id": "deepseek-chat",
                        "name": "DeepSeek Chat",
                        "reasoning": false,
                        "tool_call": true,
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 128000, "output": 8192},
                        "cost": {"input": 0.5, "output": 2.0, "cache_read": 0.1}
                    },
                    "deepseek-reasoner": {
                        "id": "deepseek-reasoner",
                        "name": "DeepSeek Reasoner",
                        "reasoning": true,
                        "tool_call": true,
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 128000},
                        "release_date": "2025-05-28"
                    }
                }
            },
            "anthropic": {
                "id": "anthropic",
                "name": "Anthropic",
                "api": "https://api.anthropic.com",
                "models": {
                    "claude-sonnet-4-5": {
                        "id": "claude-sonnet-4-5",
                        "name": "Claude Sonnet 4.5",
                        "limit": {"context": 200000, "output": 64000}
                    }
                }
            },
            "broken": {"id": "broken", "name": "Broken", "models": {"ghost": "not an object"}}
        }"#
    }

    #[test]
    fn parses_catalog_into_flattened_table() {
        let table = parse_catalog(sample_catalog()).unwrap();
        // Only the fields anything reads are kept, grouped by normalized
        // id: look entries up by id rather than position.
        assert_eq!(
            table.resolve_model_basics("deepseek-chat"),
            RecordBasics {
                name: Some("DeepSeek Chat".into()),
                context_window: Some(128_000),
                output_limit: Some(8_192),
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
            }
        );
        assert_eq!(
            table.resolve_model_basics("deepseek-reasoner").output_limit,
            None
        );
        assert_eq!(
            table
                .resolve_model_basics("claude-sonnet-4-5")
                .context_window,
            Some(200_000)
        );
        // A model spec that is not an object is skipped, not fatal.
        assert_eq!(table.resolve_model_basics("ghost"), RecordBasics::default());
        // The fetched-at stamp parses back through the same field.
        let round_tripped: ModelsDevTable =
            serde_json::from_slice(&serde_json::to_vec(&table).unwrap()).unwrap();
        assert_eq!(round_tripped, table);
    }

    #[test]
    fn input_modalities_resolve_without_voting_for_empty() {
        let document = r#"{
            "a": {"id": "a", "models": {"m": {"id": "m", "modalities": {"input": ["text", "image"], "output": ["text"]}}}},
            "b": {"id": "b", "models": {"m": {"id": "m"}}}
        }"#;
        let table = parse_catalog(document).unwrap();
        // The copy without modality information does not outvote the one
        // that says.
        assert_eq!(
            table.resolve_input_modalities("m"),
            vec!["text".to_owned(), "image".to_owned()]
        );
        // Nothing known about the id: empty.
        assert_eq!(table.resolve_input_modalities("other"), Vec::<String>::new());
    }

    #[test]
    fn resolves_a_model_id_across_every_provider_by_majority() {        // Multiple catalog providers serve the same id with slightly
        // different metadata; the majority value wins per field.
        let document = r#"{
            "openai": {
                "id": "openai",
                "name": "OpenAI",
                "models": {
                    "gpt-test": {"id": "gpt-test", "name": "GPT-Test", "limit": {"context": 400000, "output": 128000}, "modalities": {"input": ["text", "image"], "output": ["text"]}},
                    "shared": {"id": "shared", "name": "Shared", "limit": {"context": 1000000}, "modalities": {"input": ["text"], "output": ["text"]}}
                }
            },
            "relay-a": {
                "id": "relay-a",
                "name": "Relay A",
                "models": {
                    "shared": {"id": "shared", "name": "Shared", "limit": {"context": 1000000}, "modalities": {"input": ["text"], "output": ["text"]}},
                    "shared.alt": {"name": "Shared Alt", "limit": {"context": 500000}}
                }
            },
            "relay-b": {
                "id": "relay-b",
                "name": "Relay B",
                "models": {
                    "shared": {"id": "shared", "name": "Shared Renamed", "limit": {"context": 200000}, "modalities": {"input": ["text", "image"], "output": ["text"]}}
                }
            }
        }"#;
        let table = parse_catalog(document).unwrap();

        // Majority name (2 of 3) and majority window (2 of 3).
        let shared = table.resolve_model_basics("shared");
        assert_eq!(shared.name.as_deref(), Some("Shared"));
        assert_eq!(shared.context_window, Some(1_000_000));
        // Majority modalities (2 of 3 say text-only; one says vision).
        assert_eq!(shared.input_modalities, vec!["text".to_owned()]);
        assert_eq!(shared.output_modalities, vec!["text".to_owned()]);
        // A model the catalog describes as multimodal carries its lists.
        assert_eq!(
            table.resolve_model_basics("gpt-test").input_modalities,
            vec!["text".to_owned(), "image".to_owned()]
        );

        // No catalog provider carries the id: nothing to fill.
        assert_eq!(
            table.resolve_model_basics("unknown"),
            RecordBasics::default()
        );

        // Separator-insensitive fallback: `shared_alt` finds `shared.alt`.
        let alt = table.resolve_model_basics("shared_alt");
        assert_eq!(alt.name.as_deref(), Some("Shared Alt"));
        assert_eq!(alt.context_window, Some(500_000));

        // An exact id in the group beats its separator-spelled siblings even
        // when they are the majority.
        let cased = r#"{
            "a": {"id": "a", "models": {"Test-Model": {"name": "Exact Spelling", "limit": {"context": 1000}}}},
            "b": {"id": "b", "models": {
                "test_model": {"name": "One", "limit": {"context": 1}},
                "testmodel": {"name": "Two", "limit": {"context": 2}}
            }}
        }"#;
        let table = parse_catalog(cased).unwrap();
        let basics = table.resolve_model_basics("Test-Model");
        assert_eq!(basics.name.as_deref(), Some("Exact Spelling"));
        assert_eq!(basics.context_window, Some(1_000));
    }

    #[test]
    fn merge_fills_from_table_and_appends_api_models() {
        let table = parse_catalog(sample_catalog()).unwrap();
        let mut roster = provider();
        // Pre-record a window the user typed; it must survive untouched.
        roster.models[0].context_window = Some(1_000_000);
        let api_models = vec![
            api_model("deepseek-chat", None),
            api_model("deepseek-reasoner", None),
            // The API offers something the table has never heard of: it is
            // still added, just without metadata.
            api_model("brand-new-model", Some("Brand New Model")),
        ];

        let outcome = merge_api_models(&mut roster, &api_models, Some(&table));
        assert_eq!(
            outcome,
            ModelsDevApplyOutcome {
                added: 2,
                filled: 1,
            }
        );
        assert_eq!(roster.models[0].context_window, Some(1_000_000));
        assert_eq!(roster.models[0].name.as_deref(), Some("DeepSeek Chat"));
        assert_eq!(roster.models[0].output_limit, Some(8_192));
        assert_eq!(roster.models[1].id, "deepseek-reasoner");
        assert_eq!(roster.models[1].name.as_deref(), Some("DeepSeek Reasoner"));
        assert_eq!(roster.models[1].context_window, Some(128_000));
        assert_eq!(roster.models[2].id, "brand-new-model");
        assert_eq!(roster.models[2].name.as_deref(), Some("Brand New Model"));
        assert_eq!(roster.models[2].context_window, None);

        // Merging again changes nothing: everything is already recorded.
        let outcome = merge_api_models(&mut roster, &api_models, Some(&table));
        assert_eq!(
            outcome,
            ModelsDevApplyOutcome {
                added: 0,
                filled: 0,
            }
        );
    }

    #[test]
    fn merge_matches_ids_across_separator_spelling() {
        let table = parse_catalog(sample_catalog()).unwrap();
        let mut roster = CustomProvider::new(
            "claude".into(),
            "Claude".into(),
            "https://api.anthropic.com".into(),
            ProviderApiFormat::Anthropic,
            String::new(),
            vec![CustomProviderModel {
                id: "claude.sonnet.4.5".into(),
                ..Default::default()
            }],
        );
        let api_models = vec![api_model("claude-sonnet-4-5", None)];
        let outcome = merge_api_models(&mut roster, &api_models, Some(&table));
        assert_eq!(outcome.added, 0, "the respelled roster id is recognized");
        assert_eq!(outcome.filled, 1);
        assert_eq!(roster.models[0].name.as_deref(), Some("Claude Sonnet 4.5"));
        assert_eq!(roster.models[0].context_window, Some(200_000));
    }

    #[test]
    fn merge_without_table_still_adds_the_api_list() {
        let mut roster = provider();
        let api_models = vec![api_model("deepseek-chat", Some("DeepSeek Chat"))];
        let outcome = merge_api_models(&mut roster, &api_models, None);
        assert_eq!(outcome.added, 0);
        assert_eq!(outcome.filled, 1, "the API's display name still fills in");
        assert_eq!(roster.models[0].name.as_deref(), Some("DeepSeek Chat"));
    }
}

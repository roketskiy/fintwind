//! Provider fallback choices used before daemon-side discovery completes.

use serde::{Deserialize, Serialize};

use crate::model::{ProviderAgentPreset, ProviderModel};

/// One image-capable model entry returned by the runtime inventory or catalog.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageModelChoice {
    /// The catalog provider id — the first half of [`Self::id`].
    pub provider: String,
    /// `provider/model`, the value OpenCode's config stores.
    pub id: String,
    /// The model's display name, or the model id when the catalog has none.
    pub label: String,
}

/// OpenCode discovers its catalog from the running server, so there is no
/// static fallback list. Kept as a function so callers share one shape.
pub fn fallback_models() -> Vec<ProviderModel> {
    Vec::new()
}

pub fn fallback_agent_presets() -> Vec<ProviderAgentPreset> {
    Vec::new()
}

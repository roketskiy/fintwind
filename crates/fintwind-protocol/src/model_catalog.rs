//! Provider fallback choices used before daemon-side discovery completes.

use crate::model::{ProviderAgentPreset, ProviderModel};

/// OpenCode discovers its catalog from the running server, so there is no
/// static fallback list. Kept as a function so callers share one shape.
pub fn fallback_models() -> Vec<ProviderModel> {
    Vec::new()
}

pub fn fallback_agent_presets() -> Vec<ProviderAgentPreset> {
    Vec::new()
}

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "scope",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RuntimeConfigScope {
    Global,
    Project { project_id: Uuid },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeImageModel {
    pub provider: String,
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfigSnapshot {
    pub scope: RuntimeConfigScope,
    /// Opaque content revision. The daemon issues and validates this value.
    pub revision: String,
    pub shell: Option<String>,
    pub todo_enabled: bool,
    pub image_generate_enabled: bool,
    /// OpenCode's `provider/model`, optionally followed by `#variant`.
    pub image_model: Option<String>,
    pub image_models: Vec<RuntimeImageModel>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfigDraft {
    pub scope: RuntimeConfigScope,
    pub expected_revision: String,
    pub shell: Option<String>,
    pub todo_enabled: bool,
    pub image_generate_enabled: bool,
    pub image_model: Option<String>,
}

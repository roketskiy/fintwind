//! Mermaid-check plugin transport. Like the browser-tool wire, this is not
//! the desktop daemon protocol: it carries one read-only validation call for
//! the private OpenCode plugin and nothing else.
//!
//! The grammar is additive: a newer daemon accepts messages an older client
//! never sends, and a newer client sending an unknown message to an older
//! daemon is refused instead of being executed.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MERMAID_CHECK_VERSION: u32 = 1;
pub const MERMAID_CHECK_ENDPOINT: &str = "/v1/mermaid-check";
/// One 32 KiB source plus its JSON envelope and identity fields.
pub const MAX_MERMAID_CHECK_MESSAGE_BYTES: usize = 40 * 1024;
/// A cached failure or a tool reply stays one bounded line.
pub const MAX_MERMAID_CHECK_ERROR_BYTES: usize = 1024;

#[derive(Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MermaidCheckMessage {
    Hello {
        version: u32,
        token: String,
    },
    Check {
        request_id: Uuid,
        source: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum MermaidCheckReply {
    Hello {
        version: u32,
    },
    Rejected {
        message: String,
    },
    Result {
        request_id: Uuid,
        ok: bool,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        width: Option<f64>,
        #[serde(default)]
        height: Option<f64>,
    },
}

//! Browser-only plugin transport. This is not the desktop daemon protocol:
//! it cannot name a Fintwind session, publish pages, or execute general RPCs.
//!
//! The grammar is additive: a newer daemon accepts messages an older client
//! never sends (an older client simply lacks the capability), and a newer
//! client sending an unknown message to an older daemon is refused instead of
//! being executed. That is why adding [`BrowserToolMessage::Open`] keeps
//! [`BROWSER_TOOL_VERSION`] at one.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::browser::{BrowserAction, BrowserResult};

pub const BROWSER_TOOL_VERSION: u32 = 1;
pub const BROWSER_TOOL_ENDPOINT: &str = "/v1/browser-tools";
pub const MAX_BROWSER_TOOL_MESSAGE_BYTES: usize = 32 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BrowserToolMessage {
    Hello {
        version: u32,
        token: String,
    },
    List {
        request_id: Uuid,
        session_id: String,
    },
    Invoke {
        request_id: Uuid,
        session_id: String,
        page_id: Uuid,
        grant_id: Uuid,
        action: BrowserAction,
    },
    /// Ask the GUI that registered this session runtime's launcher to open a
    /// new tab. The caller never names a page: the daemon resolves the live
    /// session runtime from the connection's own binding and refuses when no
    /// unique launcher is registered for it.
    Open {
        request_id: Uuid,
        session_id: String,
        url: String,
    },
    Cancel {
        request_id: Uuid,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserToolPage {
    pub page_id: Uuid,
    pub grant_id: Uuid,
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BrowserToolReply {
    Hello {
        version: u32,
    },
    Rejected {
        message: String,
    },
    Result {
        request_id: Uuid,
        result: BrowserResult,
    },
    /// The daemon-side browser-tools setting changed (or is being stated for
    /// a fresh connection). While disabled the plugin must keep its tools and
    /// instruction out of model context entirely.
    ToolsState {
        enabled: bool,
    },
}

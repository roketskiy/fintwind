//! Ephemeral browser collaboration messages. Never persist or replay these.
//!
//! The messages here form a live-only bridge between a daemon-owned session
//! runtime and the GUI connection that currently holds the browser page.
//! Authorization is issued per page by the GUI and bound to one session,
//! runtime, page and grant; the daemon never guesses a "current page". Every
//! design decision favors failing closed: an unknown scope answers with an
//! error instead of falling back to another page, and no message on this
//! channel may carry raw scripts, arbitrary CDP methods, cookies or local
//! file contents.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// How long the daemon waits for a GUI answer before cancelling the request
/// and reporting an error. A timeout is terminal: the side effect may or may
/// not have happened, so the caller re-observes instead of retrying.
pub const BROWSER_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// Upper bound for a GUI result payload. Snapshots are untrusted page text
/// and are truncated by the GUI adapter as well; this is the wire-level cap
/// so a hostile or broken page cannot push unbounded data into the daemon.
pub const MAX_BROWSER_RESULT_BYTES: usize = 32 * 1024;

pub const MAX_BROWSER_SELECTOR_BYTES: usize = 512;
pub const MAX_BROWSER_TEXT_BYTES: usize = 8 * 1024;
pub const MAX_BROWSER_URL_BYTES: usize = 4096;
pub const MAX_BROWSER_TITLE_BYTES: usize = 1024;
/// Pages one connection may publish at once. Mirrored by the daemon broker
/// and pre-checked by the client so an oversized publish fails locally.
pub const MAX_BROWSER_PAGES_PER_CONNECTION: usize = 16;

/// A fresh, GUI-issued capability for one page and one live session runtime.
/// The transport additionally binds it to the publishing GUI connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserScope {
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    pub page_id: Uuid,
    pub grant_id: Uuid,
}

impl BrowserScope {
    /// Every id must be present: a nil session, runtime, page or grant is a
    /// malformed scope and is refused rather than interpreted.
    pub fn is_well_formed(&self) -> bool {
        !self.session_id.is_nil()
            && !self.runtime_id.is_nil()
            && !self.page_id.is_nil()
            && !self.grant_id.is_nil()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserShare {
    pub scope: BrowserScope,
    pub url: String,
    pub title: String,
}

/// No raw scripts, arbitrary CDP methods, cookies or file access. Selector
/// and text bounds keep a broken page from steering unbounded payloads, and
/// navigation is restricted to scheme-checked HTTP(S) URLs without embedded
/// credentials.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum BrowserAction {
    Snapshot,
    Click { selector: String },
    Fill { selector: String, text: String },
    Navigate { url: String },
}

impl BrowserAction {
    /// Observation is the default permission; mutations need per-request
    /// confirmation by the page owner.
    pub fn requires_approval(&self) -> bool {
        !matches!(self, Self::Snapshot)
    }

    /// Scheme and credential rules shared by navigation actions and by page
    /// publication, so a shared page's URL is held to the same standard as a
    /// URL the daemon asks the GUI to open.
    pub fn validate_url(url: &str) -> Result<(), String> {
        if url.len() > MAX_BROWSER_URL_BYTES {
            return Err("browser URL exceeds the size limit".into());
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("browser navigation only supports HTTP and HTTPS".into());
        }
        // `https://user:password@host/` would carry credentials through
        // the bridge, so any authority with userinfo is refused.
        let authority = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
        let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
        if authority.contains('@') {
            return Err("browser navigation must not carry credentials".into());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        let selector = match self {
            Self::Click { selector } | Self::Fill { selector, .. } => Some(selector),
            _ => None,
        };
        if selector.is_some_and(|selector| {
            selector.trim().is_empty() || selector.len() > MAX_BROWSER_SELECTOR_BYTES
        }) {
            return Err("browser selector is empty or too long".into());
        }
        if let Self::Fill { text, .. } = self
            && text.len() > MAX_BROWSER_TEXT_BYTES
        {
            return Err("browser input exceeds the size limit".into());
        }
        if let Self::Navigate { url } = self {
            Self::validate_url(url)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserRequest {
    pub request_id: Uuid,
    pub scope: BrowserScope,
    pub action: BrowserAction,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BrowserResult {
    Ok { value: Value },
    Error { message: String },
}

impl BrowserResult {
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }
}

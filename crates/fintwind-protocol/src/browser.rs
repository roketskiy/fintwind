//! Ephemeral browser collaboration messages. Never persist or replay these.
//!
//! The messages here form a live-only bridge between a daemon-owned session
//! runtime and the GUI connection that currently holds the browser page.
//! Authorization is issued per page by the GUI and bound to one session,
//! runtime, page and grant; the daemon never guesses a "current page". Every
//! design decision favors failing closed: an unknown scope answers with an
//! error instead of falling back to another page. No message on this channel
//! carries arbitrary CDP method names, cookies or local file contents. One
//! raw capability is deliberate: [`BrowserAction::Evaluate`] carries a single
//! bounded JavaScript expression — it is authorized exactly like every other
//! mutation (per-action approval on a manual share, continuous under a
//! full-access share), runs in the page's isolated world, and its output is
//! untrusted page data, never instructions.
//!
//! A GUI connection may additionally register one *launcher* scope per live
//! session runtime. The launcher's page and grant ids are capability
//! identities, not a real tab: they never appear in any page list, and only
//! [`BrowserAction::Open`] is ever routed to them. Exactly one launcher per
//! session runtime may exist at a time; a second registration is refused
//! instead of guessing which one to use.

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
/// Upper bound for one `Evaluate` expression, in bytes. The expression is the
/// one raw script body the bridge carries; everything else about it is
/// authorization, isolation and output bounding.
pub const MAX_BROWSER_EXPRESSION_BYTES: usize = 32 * 1024;
/// Upper bound for one embedded media payload, in decoded bytes. A screenshot
/// is bounded by construction — the GUI controls capture format and quality —
/// so a hostile page cannot push unbounded pixels through this channel.
pub const MAX_BROWSER_MEDIA_BYTES: usize = 3 * 1024 * 1024;
/// Upper bound for a serialized [`BrowserResult::Media`] on the wire, JSON
/// envelope and base64 expansion included.
pub const MAX_BROWSER_MEDIA_RESULT_BYTES: usize = 5 * 1024 * 1024;
/// Upper bound for one viewport coordinate in CSS pixels. The live viewport
/// is checked again at execution; this only refuses absurd values outright.
pub const MAX_BROWSER_COORDINATE: i32 = 8192;
/// Upper bound for one `Press` key-combination string, in bytes.
pub const MAX_BROWSER_KEY_BYTES: usize = 32;
/// Upper bound for one scroll request, in pixels. A zero delta is refused
/// separately, so the only accepted range is `-MAX..=MAX` without zero.
pub const MAX_BROWSER_SCROLL_DELTA: u64 = 2_000;
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

/// No arbitrary CDP method names, cookies or file access. Selector and text
/// bounds keep a broken page from steering unbounded payloads, and navigation
/// is restricted to scheme-checked HTTP(S) URLs without embedded credentials.
///
/// [`Self::Open`] is the one launcher-side action: only a registered browser
/// launcher may ever be routed an `Open`. [`Self::Scroll`] is an ordinary
/// page action and is served only by a page publication, like the rest.
///
/// [`Self::Evaluate`] is the deliberate exception to "no raw scripts": one
/// bounded expression, authorized like every other mutation, executed in the
/// page's isolated world with an untrusted, size-capped result. See the
/// module documentation. [`Self::Screenshot`] answers as
/// [`BrowserResult::Media`], bounded by capture parameters rather than trust.
/// [`Self::Close`] is launcher-side like `Open`: the app host closes the
/// surface, and only a page this session opened automatically qualifies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BrowserAction {
    Snapshot,
    Click {
        selector: String,
    },
    Fill {
        selector: String,
        text: String,
    },
    Navigate {
        url: String,
    },
    /// Ask the GUI that registered the session runtime's launcher to open a
    /// new tab. The answered value is the new page's identity, and the page
    /// is then shared under a fresh scope of its own.
    Open {
        url: String,
    },
    /// Scroll a shared page by a bounded delta. A zero delta is refused
    /// rather than treated as a harmless no-op, and the bound keeps a
    /// broken or hostile caller from asking for an unbounded scroll.
    Scroll {
        delta_y: i32,
    },
    /// Capture what the page visually shows. The result travels as
    /// [`BrowserResult::Media`] and is bounded by construction: the GUI
    /// captures, re-encodes smaller when over budget, and refuses rather
    /// than sending unbounded pixels.
    Screenshot {
        /// Capture the full scrollable document instead of the viewport.
        full_page: bool,
    },
    /// Run one JavaScript expression in the page's isolated world, awaiting
    /// promises. The expression body is untrusted input to the page, not to
    /// this protocol; its result is untrusted page data.
    Evaluate {
        expression: String,
    },
    /// Click at viewport-relative CSS coordinates. The live viewport is
    /// checked at execution and an out-of-range point is refused rather than
    /// clipped into a different target.
    ClickAt {
        x: i32,
        y: i32,
    },
    DoubleClick {
        selector: String,
    },
    /// Press one key combination on the focused control the selector names.
    /// The key grammar is an allowlist, not a mapping best-effort.
    Press {
        selector: String,
        key: String,
    },
    Hover {
        selector: String,
    },
    /// Select one option of a `<select>` by its value.
    Select {
        selector: String,
        value: String,
    },
    /// Drag from one element to another through interpolated pointer moves.
    Drag {
        from: String,
        to: String,
    },
    /// Close the page this grant names. Launcher-side like [`Self::Open`]:
    /// only the app host can close the surface, and only a page this session
    /// opened automatically qualifies; a manually shared tab is the user's.
    Close,
}

impl BrowserAction {
    /// Whether this action is outside the default observation permission.
    /// It does **not** mean the daemon demands a per-request approval: the
    /// page owner decides, and an owner in full-access mode answers these
    /// without prompting. Snapshot is the only action an owner may treat as
    /// always permitted.
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

    /// The key combinations [`Self::Press`] accepts: one named functional
    /// key or one printable ASCII character, optionally preceded by
    /// `Control+`, `Shift+` and/or `Alt+`. Anything else is refused rather
    /// than mapped by guesswork — an unknown key name reaching the native
    /// side must be a protocol error, not a fallback.
    pub fn validate_key(key: &str) -> Result<(), String> {
        if key.is_empty() || key.len() > MAX_BROWSER_KEY_BYTES {
            return Err("browser key is empty or too long".into());
        }
        let mut main = key;
        for _ in 0..3 {
            match main.split_once('+') {
                // A `+` the main key itself contains (`Shift++`) must survive.
                Some((head, tail))
                    if matches!(head, "Control" | "Shift" | "Alt") && !tail.is_empty() =>
                {
                    main = tail;
                }
                _ => break,
            }
        }
        let ok = matches!(
            main,
            "Enter"
                | "Tab"
                | "Escape"
                | "Backspace"
                | "Delete"
                | "Insert"
                | "Home"
                | "End"
                | "PageUp"
                | "PageDown"
                | "ArrowUp"
                | "ArrowDown"
                | "ArrowLeft"
                | "ArrowRight"
                | "Space"
        ) || (main.len() == 1 && main.as_bytes()[0].is_ascii_graphic());
        if !ok {
            return Err("browser key is not in the supported key set".into());
        }
        Ok(())
    }

    fn validate_selector(selector: &str) -> Result<(), String> {
        if selector.trim().is_empty() || selector.len() > MAX_BROWSER_SELECTOR_BYTES {
            return Err("browser selector is empty or too long".into());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Click { selector }
            | Self::Fill { selector, .. }
            | Self::DoubleClick { selector }
            | Self::Press { selector, .. }
            | Self::Hover { selector }
            | Self::Select { selector, .. } => Self::validate_selector(selector)?,
            Self::Drag { from, to } => {
                Self::validate_selector(from)?;
                Self::validate_selector(to)?;
            }
            _ => {}
        }
        if let Self::Fill { text, .. } = self
            && text.len() > MAX_BROWSER_TEXT_BYTES
        {
            return Err("browser input exceeds the size limit".into());
        }
        if let Self::Select { value, .. } = self
            && value.len() > MAX_BROWSER_TEXT_BYTES
        {
            return Err("browser input exceeds the size limit".into());
        }
        if let Self::Evaluate { expression } = self
            && (expression.trim().is_empty()
                || expression.len() > MAX_BROWSER_EXPRESSION_BYTES)
        {
            return Err("browser expression is empty or too long".into());
        }
        if let Self::ClickAt { x, y } = self
            && (!(0..=MAX_BROWSER_COORDINATE).contains(x)
                || !(0..=MAX_BROWSER_COORDINATE).contains(y))
        {
            return Err("browser coordinates are outside the supported range".into());
        }
        if let Self::Press { key, .. } = self {
            Self::validate_key(key)?;
        }
        if let Self::Navigate { url } | Self::Open { url } = self {
            Self::validate_url(url)?;
        }
        if let Self::Scroll { delta_y } = self
            && (*delta_y == 0 || delta_y.unsigned_abs() as u64 > MAX_BROWSER_SCROLL_DELTA)
        {
            return Err("browser scroll delta must be a non-zero amount within the limit".into());
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
    /// One bounded binary payload — a screenshot — as standard base64. Wire
    /// caps treat this variant separately: [`Self::wire_budget`] gives it a
    /// larger budget than the JSON results, still fixed and enforced on both
    /// sides of the bridge.
    Media { mime: String, data: String },
}

impl BrowserResult {
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    /// Build a media result, allowing only the capture formats the GUI
    /// produces and only base64 that decodes within the media byte budget.
    pub fn media(mime: &str, data: String) -> Result<Self, String> {
        if !matches!(mime, "image/png" | "image/jpeg") {
            return Err("unsupported browser media type".into());
        }
        // Standard base64 with padding encodes 3 bytes into 4 characters.
        let max_encoded = 4 * MAX_BROWSER_MEDIA_BYTES.div_ceil(3);
        if data.len() > max_encoded {
            return Err("browser media exceeds the size limit".into());
        }
        Ok(Self::Media {
            mime: mime.to_owned(),
            data,
        })
    }

    /// The serialized-size budget for this result on the wire. JSON results
    /// share the small cap; media carries its own.
    pub fn wire_budget(&self) -> usize {
        match self {
            Self::Ok { .. } | Self::Error { .. } => MAX_BROWSER_RESULT_BYTES,
            Self::Media { .. } => MAX_BROWSER_MEDIA_RESULT_BYTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_combinations_follow_the_allowlist() {
        for key in [
            "Enter",
            "Tab",
            "Escape",
            "Space",
            "ArrowDown",
            "PageUp",
            "a",
            "A",
            "1",
            "+",
            "Control+A",
            "Control+Shift+Tab",
            "Alt+ArrowLeft",
            "Shift++",
        ] {
            assert_eq!(BrowserAction::validate_key(key), Ok(()), "{key}");
        }
        for key in [
            "",
            "Control+",
            "Enter+Enter",
            "Control+Shift+Alt+Meta+A",
            "Meta+A",
            "ctrl+a",
            "Enter ",
            "F5",
            "中",
            "Control+Enter+X",
        ] {
            assert!(BrowserAction::validate_key(key).is_err(), "{key}");
        }
    }

    #[test]
    fn new_actions_validate_their_bounds() {
        assert!(BrowserAction::Evaluate {
            expression: "1 + 1".into()
        }
        .validate()
        .is_ok());
        assert!(BrowserAction::Evaluate {
            expression: String::new()
        }
        .validate()
        .is_err());
        assert!(BrowserAction::Evaluate {
            expression: "x".repeat(MAX_BROWSER_EXPRESSION_BYTES + 1)
        }
        .validate()
        .is_err());

        assert!(BrowserAction::ClickAt { x: 0, y: 8192 }.validate().is_ok());
        assert!(BrowserAction::ClickAt { x: -1, y: 10 }.validate().is_err());
        assert!(BrowserAction::ClickAt { x: 10, y: 8193 }.validate().is_err());

        assert!(BrowserAction::Press {
            selector: "#q".into(),
            key: "Enter".into()
        }
        .validate()
        .is_ok());
        assert!(BrowserAction::Press {
            selector: "#q".into(),
            key: "Meta+A".into()
        }
        .validate()
        .is_err());
        assert!(BrowserAction::Press {
            selector: " ".into(),
            key: "Enter".into()
        }
        .validate()
        .is_err());

        assert!(BrowserAction::Drag {
            from: "#a".into(),
            to: "#b".into()
        }
        .validate()
        .is_ok());
        assert!(BrowserAction::Drag {
            from: "#a".into(),
            to: " ".into()
        }
        .validate()
        .is_err());

        assert!(BrowserAction::Select {
            selector: "#s".into(),
            value: String::new()
        }
        .validate()
        .is_ok());
        assert!(BrowserAction::Select {
            selector: "#s".into(),
            value: "v".repeat(MAX_BROWSER_TEXT_BYTES + 1)
        }
        .validate()
        .is_err());

        assert!(BrowserAction::Screenshot { full_page: true }.validate().is_ok());
        assert!(BrowserAction::Close.validate().is_ok());
    }

    #[test]
    fn every_action_validates_as_a_whole() {
        // The public entry a request passes through before any routing.
        let all = [
            BrowserAction::Snapshot,
            BrowserAction::Click {
                selector: "#a".into(),
            },
            BrowserAction::Fill {
                selector: "#a".into(),
                text: "hello".into(),
            },
            BrowserAction::Navigate {
                url: "https://example.com/".into(),
            },
            BrowserAction::Open {
                url: "https://example.com/".into(),
            },
            BrowserAction::Scroll { delta_y: -40 },
            BrowserAction::Screenshot { full_page: false },
            BrowserAction::Evaluate {
                expression: "document.title".into(),
            },
            BrowserAction::ClickAt { x: 12, y: 34 },
            BrowserAction::DoubleClick {
                selector: "#a".into(),
            },
            BrowserAction::Press {
                selector: "#a".into(),
                key: "Enter".into(),
            },
            BrowserAction::Hover {
                selector: "#a".into(),
            },
            BrowserAction::Select {
                selector: "#a".into(),
                value: "1".into(),
            },
            BrowserAction::Drag {
                from: "#a".into(),
                to: "#b".into(),
            },
            BrowserAction::Close,
        ];
        for action in &all {
            assert!(action.validate().is_ok(), "{action:?}");
        }
        // Every action except the pure observation requires approval.
        for action in &all {
            assert_eq!(
                action.requires_approval(),
                !matches!(action, BrowserAction::Snapshot),
                "{action:?}"
            );
        }
    }

    #[test]
    fn media_results_carry_their_own_wire_budget() {
        let json = BrowserResult::Ok {
            value: Value::Null,
        };
        assert_eq!(json.wire_budget(), MAX_BROWSER_RESULT_BYTES);
        let media = BrowserResult::media("image/png", "AAAA".into()).unwrap();
        assert_eq!(media.wire_budget(), MAX_BROWSER_MEDIA_RESULT_BYTES);

        assert!(BrowserResult::media("image/webp", "AAAA".into()).is_err());
        assert!(BrowserResult::media("image/png", String::new()).is_ok());
        let max_encoded = 4 * MAX_BROWSER_MEDIA_BYTES.div_ceil(3);
        assert!(BrowserResult::media("image/png", "A".repeat(max_encoded)).is_ok());
        assert!(BrowserResult::media("image/png", "A".repeat(max_encoded + 4)).is_err());
    }

    #[test]
    fn new_actions_round_trip_over_the_wire() {
        // The plugin sends camelCase tags and fields; deny_unknown_fields
        // makes an unknown kind a refusal, so the exact shapes matter.
        let actions = [
            (
                serde_json::json!({"kind": "screenshot", "fullPage": true}),
                BrowserAction::Screenshot { full_page: true },
            ),
            (
                serde_json::json!({"kind": "evaluate", "expression": "1+1"}),
                BrowserAction::Evaluate {
                    expression: "1+1".into(),
                },
            ),
            (
                serde_json::json!({"kind": "clickAt", "x": 5, "y": 6}),
                BrowserAction::ClickAt { x: 5, y: 6 },
            ),
            (
                serde_json::json!({"kind": "doubleClick", "selector": "#a"}),
                BrowserAction::DoubleClick {
                    selector: "#a".into(),
                },
            ),
            (
                serde_json::json!({"kind": "press", "selector": "#a", "key": "Control+A"}),
                BrowserAction::Press {
                    selector: "#a".into(),
                    key: "Control+A".into(),
                },
            ),
            (
                serde_json::json!({"kind": "hover", "selector": "#a"}),
                BrowserAction::Hover {
                    selector: "#a".into(),
                },
            ),
            (
                serde_json::json!({"kind": "select", "selector": "#a", "value": "1"}),
                BrowserAction::Select {
                    selector: "#a".into(),
                    value: "1".into(),
                },
            ),
            (
                serde_json::json!({"kind": "drag", "from": "#a", "to": "#b"}),
                BrowserAction::Drag {
                    from: "#a".into(),
                    to: "#b".into(),
                },
            ),
            (serde_json::json!({"kind": "close"}), BrowserAction::Close),
        ];
        for (wire, action) in actions {
            assert_eq!(
                serde_json::from_value::<BrowserAction>(wire.clone()).unwrap(),
                action,
                "{wire}"
            );
            assert_eq!(serde_json::to_value(&action).unwrap(), wire, "{wire}");
        }
        assert!(serde_json::from_value::<BrowserAction>(serde_json::json!({
            "kind": "evaluate"
        }))
        .is_err());
        assert!(serde_json::from_value::<BrowserResult>(serde_json::json!({
            "kind": "media", "mime": "image/png", "data": "AAAA"
        }))
        .is_ok());
    }
}

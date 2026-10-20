//! The daemon-side mermaid check endpoint.
//!
//! The context plugin exposes `fintwind_mermaid_check` to the model; this
//! module answers it. Validation calls the one compiler in
//! `fintwind_diagram::diagram_compile`, so a check result can never drift
//! from what the app's renderer does with the same source.
//!
//! The transport mirrors the browser-tool wire: a dedicated endpoint on the
//! daemon listener, a per-daemon random token (never the main daemon token),
//! no `Origin` header, and structurally bounded messages. One connection
//! serves any number of checks; each answer is a single bounded reply, so
//! the connection thread can compile inline without a worker pool.

use std::net::TcpStream;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tungstenite::{Message, WebSocket};

use fintwind_protocol::mermaid::{
    MAX_MERMAID_CHECK_ERROR_BYTES, MAX_MERMAID_CHECK_MESSAGE_BYTES, MERMAID_CHECK_VERSION,
    MermaidCheckMessage, MermaidCheckReply,
};

pub(crate) const ADDRESS_ENV: &str = "FINTWIND_MERMAID_CHECK_ADDRESS";
pub(crate) const TOKEN_ENV: &str = "FINTWIND_MERMAID_CHECK_TOKEN";

struct Endpoint {
    address: String,
    token: String,
}

/// Set once when the daemon listener starts; the plugin reads the same values
/// through the environment. A missing entry means no capability: the plugin
/// stays inert and no tool reaches the model.
static ENDPOINT: OnceLock<Endpoint> = OnceLock::new();

/// Publishes the endpoint address and generates this daemon's token. Fails
/// only when called twice, which no daemon lifetime does.
pub(crate) fn install(address: String) -> anyhow::Result<()> {
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    ENDPOINT
        .set(Endpoint { address, token })
        .map_err(|_| anyhow::anyhow!("mermaid check endpoint is already installed"))
}

/// The capability environment for a private OpenCode process, or `None` when
/// the daemon never installed the endpoint. Re-exported at the crate root for
/// the harness that stands in for that process.
pub fn environment() -> Vec<(&'static str, String)> {
    let Some(endpoint) = ENDPOINT.get() else {
        return Vec::new();
    };
    vec![
        (ADDRESS_ENV, endpoint.address.clone()),
        (TOKEN_ENV, endpoint.token.clone()),
    ]
}

fn token_matches(candidate: &str) -> bool {
    ENDPOINT
        .get()
        .is_some_and(|endpoint| !candidate.is_empty() && candidate == endpoint.token)
}

/// The theme the check compiles against. Success and failure do not depend on
/// the colors — every value here is a legal sRGB triplet — so a neutral light
/// theme keeps the answer independent of the reader's appearance.
const CHECK_THEME: fintwind_diagram::diagram_compile::Theme =
    fintwind_diagram::diagram_compile::Theme {
        background: [255, 255, 255],
        foreground: [31, 35, 40],
        muted: [87, 96, 106],
        border: [208, 215, 222],
        accent: [9, 105, 218],
    };

/// One validation result: `ok` with the drawn size, or the compiler's error.
fn check_outcome(source: &str) -> (bool, Option<String>, Option<f64>, Option<f64>) {
    if source.len() > fintwind_diagram::diagram_compile::MAX_SOURCE_BYTES {
        let error = format!(
            "diagram source is {} bytes; the limit is {} bytes",
            source.len(),
            fintwind_diagram::diagram_compile::MAX_SOURCE_BYTES
        );
        return (false, Some(error), None, None);
    }
    // The same "today once" shape the renderer's Source captures, so a Gantt
    // compiles against the same reference day it will be drawn with.
    let now_ms = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400
        * 86_400
        * 1000) as f64;
    match fintwind_diagram::diagram_compile::compile(source, CHECK_THEME, now_ms) {
        Ok(diagram) => (true, None, Some(diagram.width), Some(diagram.height)),
        Err(error) => {
            let mut text = format!("{error:#}");
            if text.len() > MAX_MERMAID_CHECK_ERROR_BYTES {
                let mut end = MAX_MERMAID_CHECK_ERROR_BYTES;
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
            }
            (false, Some(text), None, None)
        }
    }
}

/// Serves one plugin connection until it closes or the daemon shuts down.
pub(crate) fn handle(
    mut socket: WebSocket<TcpStream>,
    shutdown: std::sync::Arc<AtomicBool>,
) -> anyhow::Result<()> {
    socket.set_config(|config| {
        config.max_message_size = Some(MAX_MERMAID_CHECK_MESSAGE_BYTES);
        config.max_frame_size = Some(MAX_MERMAID_CHECK_MESSAGE_BYTES);
    });
    let first = socket.read()?;
    let authenticated = match &first {
        Message::Text(text) => {
            matches!(
                serde_json::from_str::<MermaidCheckMessage>(text),
                Ok(MermaidCheckMessage::Hello { version, token })
                    if version == MERMAID_CHECK_VERSION && token_matches(&token)
            )
        }
        _ => false,
    };
    if !authenticated {
        write(
            &mut socket,
            &MermaidCheckReply::Rejected {
                message: "mermaid check authentication failed".into(),
            },
        )?;
        return Ok(());
    }
    write(
        &mut socket,
        &MermaidCheckReply::Hello {
            version: MERMAID_CHECK_VERSION,
        },
    )?;
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(25)))?;
    socket
        .get_mut()
        .set_write_timeout(Some(Duration::from_secs(3)))?;
    loop {
        if shutdown.load(Ordering::Acquire) {
            return Ok(());
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                if text.len() > MAX_MERMAID_CHECK_MESSAGE_BYTES {
                    write(
                        &mut socket,
                        &MermaidCheckReply::Rejected {
                            message: "mermaid check message exceeds the size limit".into(),
                        },
                    )?;
                    return Ok(());
                }
                let Ok(MermaidCheckMessage::Check {
                    request_id,
                    source,
                }) = serde_json::from_str::<MermaidCheckMessage>(&text)
                else {
                    write(
                        &mut socket,
                        &MermaidCheckReply::Rejected {
                            message: "only a hello and check requests are permitted".into(),
                        },
                    )?;
                    return Ok(());
                };
                // One compile is milliseconds on a bounded source, so the
                // connection thread answers inline; the daemon's connection
                // cap bounds the total concurrency.
                let (ok, error, width, height) = check_outcome(&source);
                write(
                    &mut socket,
                    &MermaidCheckReply::Result {
                        request_id,
                        ok,
                        error,
                        width,
                        height,
                    },
                )?;
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(Message::Ping(_)) => {
                socket.flush()?;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn write(socket: &mut WebSocket<TcpStream>, reply: &MermaidCheckReply) -> anyhow::Result<()> {
    socket.send(Message::Text(serde_json::to_string(reply)?.into()))?;
    Ok(())
}

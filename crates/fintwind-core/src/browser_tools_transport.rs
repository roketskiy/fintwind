//! Restricted WebSocket reader for the browser plugin. It never subscribes
//! to the event hub, calls Backend::handle, or uses the shared response cache.

use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::bail;
use crossbeam_channel::unbounded;
use tungstenite::{Message, WebSocket};

use crate::browser_tools::BrowserTools;
use fintwind_protocol::browser::BrowserResult;
use fintwind_protocol::browser_tools::{
    BROWSER_TOOL_VERSION, BrowserToolMessage, BrowserToolReply, MAX_BROWSER_TOOL_MESSAGE_BYTES,
};

struct ConnectionGuard {
    tools: Arc<BrowserTools>,
    id: u64,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.tools.disconnect(self.id);
    }
}

struct WorkPermit(Arc<AtomicUsize>);
impl Drop for WorkPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn handle(
    mut socket: WebSocket<TcpStream>,
    tools: Arc<BrowserTools>,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    socket.set_config(|config| {
        // Large enough for a media reply (a screenshot) in either direction.
        // Plugin -> daemon messages stay structurally small — the grammar is
        // deny_unknown_fields with per-field bounds — so the read-side
        // exposure is bounded by message shape, not just by this limit.
        config.max_message_size = Some(fintwind_protocol::browser::MAX_BROWSER_MEDIA_RESULT_BYTES);
        config.max_frame_size = Some(fintwind_protocol::browser::MAX_BROWSER_MEDIA_RESULT_BYTES);
    });
    let first = socket.read()?;
    let id = match first {
        Message::Text(text) => match serde_json::from_str::<BrowserToolMessage>(&text) {
            Ok(BrowserToolMessage::Hello { version, token }) if version == BROWSER_TOOL_VERSION => {
                tools.connect(&token)
            }
            _ => Err(anyhow::anyhow!(
                "browser tool hello is invalid or incompatible"
            )),
        },
        _ => Err(anyhow::anyhow!("browser tool connection requires a hello")),
    };
    let id = match id {
        Ok(id) => id,
        Err(_) => {
            write(
                &mut socket,
                &BrowserToolReply::Rejected {
                    message: "browser tool authentication failed".into(),
                },
            )?;
            return Ok(());
        }
    };
    let _guard = ConnectionGuard {
        tools: tools.clone(),
        id,
    };
    write(
        &mut socket,
        &BrowserToolReply::Hello {
            version: BROWSER_TOOL_VERSION,
        },
    )?;
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(25)))?;
    socket
        .get_mut()
        .set_write_timeout(Some(Duration::from_secs(3)))?;
    let (outgoing, inbox) = unbounded();
    // Registration also states the current setting, so the plugin knows
    // whether it may expose tools before its first request; later changes
    // arrive through this channel and leave on the existing drain below.
    tools.register_outgoing(id, outgoing.clone());
    let active = Arc::new(AtomicUsize::new(0));
    while !shutdown.load(Ordering::Acquire) && tools.is_connected(id) {
        while let Ok(reply) = inbox.try_recv() {
            write(&mut socket, &reply)?;
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                // Commands are structurally small; a bigger one is not a
                // browser tool message. (The socket frame limit is larger
                // only so media replies can be written back.)
                if text.len() > MAX_BROWSER_TOOL_MESSAGE_BYTES {
                    write(
                        &mut socket,
                        &BrowserToolReply::Rejected {
                            message: "browser tool message exceeds the size limit".into(),
                        },
                    )?;
                    break;
                }
                let command = match serde_json::from_str::<BrowserToolMessage>(&text) {
                    Ok(command) => command,
                    Err(_) => {
                        write(
                            &mut socket,
                            &BrowserToolReply::Rejected {
                                message: "only browser list, open, invoke and cancel are permitted"
                                    .into(),
                            },
                        )?;
                        break;
                    }
                };
                match command {
                    BrowserToolMessage::List {
                        request_id,
                        session_id,
                    } => {
                        let result = if request_id.is_nil() {
                            BrowserResult::error("browser tool request id must not be nil")
                        } else {
                            match tools.list(id, &session_id) {
                                Ok(pages) => BrowserResult::Ok {
                                    value: serde_json::to_value(pages)?,
                                },
                                Err(error) => BrowserResult::error(error.to_string()),
                            }
                        };
                        write(
                            &mut socket,
                            &BrowserToolReply::Result { request_id, result },
                        )?;
                    }
                    BrowserToolMessage::Invoke {
                        request_id,
                        session_id,
                        page_id,
                        grant_id,
                        action,
                    } => {
                        // Bound worker threads on this connection, including
                        // malformed/busy scopes. The production plugin opens
                        // one connection per call; the shared server connection
                        // cap and broker pending cap also bound that pattern.
                        if active
                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                (n < 4).then_some(n + 1)
                            })
                            .is_err()
                        {
                            write(
                                &mut socket,
                                &BrowserToolReply::Result {
                                    request_id,
                                    result: BrowserResult::error(
                                        "too many pending browser tool calls",
                                    ),
                                },
                            )?;
                            continue;
                        }
                        let permit = WorkPermit(active.clone());
                        let tools = tools.clone();
                        let outgoing = outgoing.clone();
                        std::thread::Builder::new()
                            .name("fintwind-browser-tool".into())
                            .spawn(move || {
                                let _permit = permit;
                                let result = tools.invoke(
                                    id,
                                    &session_id,
                                    request_id,
                                    page_id,
                                    grant_id,
                                    action,
                                );
                                let _ =
                                    outgoing.send(BrowserToolReply::Result { request_id, result });
                            })?;
                    }
                    BrowserToolMessage::Cancel { request_id } => {
                        if tools.cancel(id, request_id).is_err() {
                            break;
                        }
                    }
                    BrowserToolMessage::Open {
                        request_id,
                        session_id,
                        url,
                    } => {
                        // Same bounded-worker rule as invoke: an open waits
                        // for the GUI round trip, including the owner's
                        // approval, so it must not occupy this reader thread.
                        if active
                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                (n < 4).then_some(n + 1)
                            })
                            .is_err()
                        {
                            write(
                                &mut socket,
                                &BrowserToolReply::Result {
                                    request_id,
                                    result: BrowserResult::error(
                                        "too many pending browser tool calls",
                                    ),
                                },
                            )?;
                            continue;
                        }
                        let permit = WorkPermit(active.clone());
                        let tools = tools.clone();
                        let outgoing = outgoing.clone();
                        std::thread::Builder::new()
                            .name("fintwind-browser-tool".into())
                            .spawn(move || {
                                let _permit = permit;
                                let result = tools.open(id, &session_id, request_id, &url);
                                let _ =
                                    outgoing.send(BrowserToolReply::Result { request_id, result });
                            })?;
                    }
                    BrowserToolMessage::Hello { .. } => {
                        bail!("browser tool hello cannot be repeated")
                    }
                }
            }
            Ok(Message::Close(_)) => break,
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
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn write(socket: &mut WebSocket<TcpStream>, reply: &BrowserToolReply) -> anyhow::Result<()> {
    let reply = match reply {
        BrowserToolReply::Result { request_id, result }
            if serde_json::to_vec(result)?.len() > result.wire_budget() =>
        {
            BrowserToolReply::Result {
                request_id: *request_id,
                result: BrowserResult::error("browser tool result exceeds the size limit"),
            }
        }
        other => other.clone(),
    };
    socket.send(Message::Text(serde_json::to_string(&reply)?.into()))?;
    Ok(())
}

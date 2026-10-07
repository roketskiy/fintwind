//! One accept loop, one thread per connection, and the split between the
//! REST endpoints and WebSocket upgrades.
//!
//! Handwritten HTTP keeps the dependency surface where the rest of the
//! workspace already is: Fintwind's own daemon and its opencode transport
//! both speak hand-rolled HTTP/1.1. The relay needs so little — fixed
//! paths, Content-Length bodies, one upgrade — that a framework would
//! only add review surface to a public process.
//!
//! Connection admission lives here: the head read carries the
//! authentication timeout, and the unauthenticated-connection cap is
//! checked before any per-connection thread does real work.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tungstenite::handshake::derive_accept_key;
use tungstenite::{protocol::Role, WebSocket};

use crate::config::AUTH_TIMEOUT;
use crate::context::RelayContext;
use crate::rest;
use crate::ws;

/// Largest REST body the relay will read. Registration payloads carry
/// public keys and signatures, not files.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// One parsed request head; the body is read separately by REST.
pub struct RequestHead {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub content_length: usize,
}

impl RequestHead {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Serve until the shutdown flag flips. Each connection gets its own
/// thread; the relay's phase-one scale is a handful of live entities, so
/// thread-per-connection stays simpler than any polling scheme.
pub fn serve(listener: TcpListener, context: Arc<RelayContext>) -> anyhow::Result<()> {
    listener
        .set_nonblocking(true)
        .context("relay listener could not go non-blocking")?;
    log::info!(target: "audit", "relay listening on {}", listener.local_addr()?);
    loop {
        if context.shutdown.load(Ordering::Acquire) {
            return Ok(());
        }
        match listener.accept() {
            Ok((stream, _peer)) => {
                let context = Arc::clone(&context);
                if std::thread::Builder::new()
                    .name("fintwind-relay-conn".into())
                    .spawn(move || handle_connection(stream, context))
                    .is_err()
                {
                    log::warn!(target: "audit", "connection thread spawn failed; connection dropped");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::ConnectionReset
                    || error.kind() == std::io::ErrorKind::ConnectionAborted =>
            {
                // Windows surfaces a client-side reset of an *earlier*
                // connection as an error on the next accept call. It is
                // noise about a connection this loop never saw; skip it
                // and keep serving (killing the relay here would drop
                // every live session).
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                // Unknown accept failures must not take a public process
                // down; log, back off briefly, and keep the listener.
                log::warn!(target: "audit", "relay accept error: {error}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn handle_connection(stream: TcpStream, context: Arc<RelayContext>) {
    // Admission bookkeeping covers the whole connection lifetime: the
    // unauthenticated pile-up is exactly what the cap defends against,
    // and a connection that authenticates stops counting only when the
    // registry takes it over.
    if !context.has_admission_slot() {
        let _ = write_response(&stream, 503, r#"{"error":"at capacity"}"#);
        return;
    }
    context
        .pending_connections
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    handle_connection_inner(stream, &context);
    context
        .pending_connections
        .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
}

fn handle_connection_inner(mut stream: TcpStream, context: &Arc<RelayContext>) {
    let _ = stream.set_nodelay(true);
    // The head read shares the authentication timeout: an unauthenticated
    // connection gets exactly this long to identify itself.
    let _ = stream.set_read_timeout(Some(AUTH_TIMEOUT));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let head = match read_head(&mut reader) {
        Ok(Some(head)) => head,
        Ok(None) | Err(_) => return,
    };

    let origin = head.header("origin").map(str::to_owned);
    let is_upgrade = head.method == "GET"
        && head
            .header("upgrade")
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let is_rest = head.path.starts_with("/api/");

    if !is_upgrade && !is_rest {
        let _ = write_response(
            &stream,
            404,
            r#"{"error":"not found"}"#,
        );
        return;
    }
    if !context.origin_allowed(origin.as_deref()) {
        let _ = write_response(&stream, 403, r#"{"error":"origin rejected"}"#);
        return;
    }

    if is_upgrade {
        // From here the socket belongs to the WebSocket session. The
        // request head was already consumed by `read_head`, so the 101
        // answer is written by hand (derive_accept_key implements the
        // RFC 6455 accept computation) instead of handing the raw stream
        // to `accept_hdr`, which would wait forever for a request that no
        // longer exists in the stream. BufReader holds no lookahead at
        // this point: the client sends nothing further until it sees the
        // 101.
        let Some(key) = head.header("sec-websocket-key").map(str::to_owned) else {
            let _ = write_response(&stream, 400, r#"{"error":"missing ws key"}"#);
            return;
        };
        let accept = derive_accept_key(key.as_bytes());
        let switching = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept}\r\n\
             \r\n"
        );
        if stream.write_all(switching.as_bytes()).is_err() || stream.flush().is_err() {
            return;
        }
        let _ = stream.set_read_timeout(None);
        let websocket = WebSocket::from_raw_socket(stream, Role::Server, None);
        ws::run_session(websocket, Arc::clone(context));
    } else {
        // REST: read the body inside the same auth timeout, answer, close.
        let body = match read_body(&mut reader, head.content_length) {
            Ok(body) => body,
            Err(_) => return,
        };
        let response = rest::handle(&context, &head, &body);
        let _ = write_response(
            &stream,
            response.status,
            &response.body,
        );
    }
}

/// Read one request head. `Ok(None)` marks a closed or malformed socket.
fn read_head(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<RequestHead>> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_owned();
            let value = value.trim().to_owned();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(usize::MAX);
            }
            headers.push((name, value));
        }
    }
    Ok(Some(RequestHead {
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        content_length,
    }))
}

fn read_body(reader: &mut BufReader<TcpStream>, content_length: usize) -> std::io::Result<Vec<u8>> {
    if content_length > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body too large",
        ));
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    Ok(body)
}

fn write_response(mut stream: &TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

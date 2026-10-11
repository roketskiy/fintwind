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

use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use tungstenite::WebSocket;
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::{Role, WebSocketConfig};

use crate::config::{
    AUTH_TIMEOUT, MAX_BODY_BYTES, MAX_FRAME_BYTES, MAX_HEAD_BYTES, MAX_HEADER_COUNT,
    MAX_REQUEST_LINE_BYTES,
};
use crate::context::RelayContext;
use crate::rest;
use crate::ws;

/// One admission slot for the unauthenticated phase of a connection.
///
/// Held from accept until either the connection authenticates (the
/// registry's cap takes over and this guard is released exactly once)
/// or the connection ends — REST answer, failed auth, timeout, or
/// socket error. A guard left alive for a whole WS session would let
/// authenticated connections consume the unauthenticated cap, which is
/// how the two limits used to blur into one.
pub struct AdmissionGuard {
    context: Arc<RelayContext>,
    released: bool,
}

impl AdmissionGuard {
    /// Take a slot if one is free; `None` means the process is at its
    /// unauthenticated cap and the connection must be refused.
    pub fn acquire(context: &Arc<RelayContext>) -> Option<Self> {
        context
            .pending_connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < crate::config::MAX_UNAUTHENTICATED_CONNECTIONS).then_some(count + 1)
            })
            .ok()?;
        Some(Self {
            context: Arc::clone(context),
            released: false,
        })
    }

    /// Release early: the registry now governs this connection.
    pub fn release(mut self) {
        self.released = true;
        self.context
            .pending_connections
            .fetch_sub(1, Ordering::AcqRel);
    }
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        if !self.released {
            self.context
                .pending_connections
                .fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// One parsed request head; the body is read separately by REST.
pub struct RequestHead {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub content_length: usize,
    /// The direct peer's address. The relay binds loopback by
    /// configuration, so a loopback peer is the one case where a
    /// forwarding proxy (Caddy) can be speaking for someone else.
    pub peer_ip: Option<String>,
    /// Whether `X-Forwarded-For` may be believed for this request. Only
    /// a loopback peer can be the trusted proxy; every other client
    /// writing that header is just writing a header.
    pub trusted_proxy: bool,
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
                let Some(admission) = AdmissionGuard::acquire(&context) else {
                    context.metrics.unauthenticated_refused();
                    // Capacity refusal must not create another thread or
                    // block accept while draining an untrusted socket.
                    drop(stream);
                    continue;
                };
                let context = Arc::clone(&context);
                // The admission clock starts here, at accept time, and is
                // carried into the connection thread: a client that drips
                // bytes must not be able to hold one of the unauthenticated
                // slots forever by resetting a per-read timeout.
                let accepted_at = Instant::now();
                if std::thread::Builder::new()
                    .name("fintwind-relay-conn".into())
                    .spawn(move || {
                        handle_connection_inner(stream, &context, admission, accepted_at)
                    })
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

fn handle_connection_inner(
    mut stream: TcpStream,
    context: &Arc<RelayContext>,
    admission: AdmissionGuard,
    accepted_at: Instant,
) {
    // Windows: a socket accepted from a non-blocking listener inherits
    // the non-blocking flag. That would make every write to a full
    // buffer fail immediately (WSAEWOULDBLOCK) and kill the session,
    // which is exactly the slow-peer case the write timeout and the
    // live outbound bound exist to handle. Put the socket back to
    // blocking; the read and write timeouts below stay in force.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    // One hard admission deadline from the moment the connection was
    // accepted, not a fresh window per read: a client that sends one
    // byte every few seconds must not hold an unauthenticated slot
    // forever, because that is how legitimate reconnects get starved.
    let admission_deadline = accepted_at + AUTH_TIMEOUT;
    let _ = stream.set_write_timeout(Some(remaining_timeout(admission_deadline)));
    let _ = stream.set_read_timeout(Some(remaining_timeout(admission_deadline)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let head = match read_head(&mut reader, admission_deadline) {
        Ok(Some(head)) => head,
        Ok(None) | Err(_) => return,
    };
    // Only the local trusted proxy may speak for another address. A
    // loopback peer is Caddy in the intended deployment; anything else
    // writing X-Forwarded-For is a client with a header.
    let peer_ip = stream.peer_addr().ok().map(|peer| peer.ip().to_string());
    let trusted_proxy = stream.peer_addr().is_ok_and(|peer| peer.ip().is_loopback());
    let mut head = head;
    head.peer_ip = peer_ip;
    head.trusted_proxy = trusted_proxy;

    // A present Origin is a browser promise: it must be on the exact
    // allow-list, and the answer reflects that one origin — never a
    // wildcard. An absent Origin is a native client, which authenticates
    // by signature and gets no CORS headers at all.
    let allowed_origin: Option<String> = match head.header("origin") {
        None => None,
        Some(origin) if context.origin_allowed(Some(origin)) => Some(origin.to_owned()),
        Some(_) => {
            let _ = write_response(&stream, 403, r#"{"error":"origin rejected"}"#, None, false);
            return;
        }
    };

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
            allowed_origin.as_deref(),
            false,
        );
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
            let _ = write_response(
                &stream,
                400,
                r#"{"error":"missing ws key"}"#,
                allowed_origin.as_deref(),
                false,
            );
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
        // The frame cap is an endpoint duty, not a relay default: the
        // wire bound is enforced here, at the protocol layer, before a
        // single oversized frame is buffered or parsed. `WebSocketConfig`
        // is non-exhaustive, so start from its default and set the two
        // caps explicitly.
        let mut config = WebSocketConfig::default();
        config.max_frame_size = Some(MAX_FRAME_BYTES);
        config.max_message_size = Some(MAX_FRAME_BYTES);
        let _ = stream.set_read_timeout(None);
        // The WS boundary enforces the admission deadline itself:
        // tungstenite issues several socket reads per message, so a
        // per-read timeout set outside cannot become a total one.
        let (deadline_stream, deadline_handle) = ws::deadline_stream(stream, admission_deadline);
        let websocket = WebSocket::from_raw_socket(deadline_stream, Role::Server, Some(config));
        ws::run_session(
            websocket,
            Arc::clone(context),
            admission,
            admission_deadline,
            deadline_handle,
        );
    } else {
        // REST: read the body inside the same auth timeout, answer, close.
        let body = match read_body(&mut reader, head.content_length, admission_deadline) {
            Ok(body) => body,
            Err(_) => return,
        };
        let response = rest::handle(&context, &head, &body);
        let _ = write_response(
            &stream,
            response.status,
            &response.body,
            allowed_origin.as_deref(),
            head.method == "OPTIONS",
        );
    }
}

/// How long a read may still block before the admission window closes.
/// Never zero: a deadline that has already passed is reported by the
/// caller as an error, not by a zero-length timeout.
fn remaining_timeout(deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1))
}

/// Read one request head under a hard byte bound. A client that never
/// sends the terminator must not be able to grow this process; the same
/// bound caps the request line and the header count.
///
/// `deadline` is the whole admission window, measured from accept time.
/// Each read is bounded by what is left of it, so a client that keeps
/// the connection alive by sending one byte at a time still loses the
/// slot on schedule instead of resetting the clock forever.
fn read_head(
    reader: &mut BufReader<TcpStream>,
    deadline: Instant,
) -> std::io::Result<Option<RequestHead>> {
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    loop {
        if head.len() > MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "admission deadline passed",
            ));
        }
        // Never wait longer than the window that is left.
        let _ = reader
            .get_ref()
            .set_read_timeout(Some(remaining_timeout(deadline)));
        match reader.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
                    break;
                }
            }
            Err(error) => return Err(error),
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split('\n');
    let request_line = lines.next().unwrap_or("").trim_end();
    if request_line.len() > MAX_REQUEST_LINE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request line too large",
        ));
    }
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if headers.len() >= MAX_HEADER_COUNT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "too many headers",
            ));
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_owned();
        let value = value.trim().to_owned();
        if name.eq_ignore_ascii_case("transfer-encoding") {
            // Chunked bodies are not supported; refusing is unambiguous,
            // and every answer closes the connection anyway.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "transfer-encoding not supported",
            ));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let parsed: usize = value.parse().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "bad content-length")
            })?;
            content_length = parsed;
        }
        headers.push((name, value));
    }
    Ok(Some(RequestHead {
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        content_length,
        peer_ip: None,
        trusted_proxy: false,
    }))
}

fn read_body(
    reader: &mut BufReader<TcpStream>,
    content_length: usize,
    deadline: Instant,
) -> std::io::Result<Vec<u8>> {
    if content_length > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body too large",
        ));
    }
    let mut body = vec![0u8; content_length];
    let mut offset = 0;
    while offset < body.len() {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "admission deadline passed",
            ));
        }
        reader
            .get_ref()
            .set_read_timeout(Some(remaining_timeout(deadline)))?;
        let read = reader.read(&mut body[offset..])?;
        if read == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        offset += read;
    }
    Ok(body)
}

fn write_response(
    mut stream: &TcpStream,
    status: u16,
    body: &str,
    allowed_origin: Option<&str>,
    preflight: bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    // Exact-origin reflection, never a wildcard: a site that is not on
    // the allow-list cannot read a relay answer. `Vary: Origin` keeps
    // caches from serving one origin's answer to another.
    let cors = match allowed_origin {
        Some(origin) => format!(
            "Access-Control-Allow-Origin: {origin}\r\n\
             Vary: Origin\r\n"
        ),
        None => String::new(),
    };
    let preflight_headers = if preflight {
        "Access-Control-Allow-Methods: POST, GET, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type\r\n\
         Access-Control-Max-Age: 600\r\n"
    } else {
        ""
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         {cors}{preflight_headers}\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

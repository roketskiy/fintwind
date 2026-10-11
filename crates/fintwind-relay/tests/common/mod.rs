//! Shared black-box harness: the relay as a real process, a hand-written
//! HTTP client, and a hand-rolled WebSocket client. Zero new
//! dependencies on purpose — the oversized-frame and malformed-head
//! cases need raw socket control a client library would hide.
//!
//! Assertions report stage names and reject codes only. No private key,
//! nonce, signature, or ciphertext ever reaches an assertion message.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fintwind_remote_crypto::SigningKey;
use fintwind_remote_protocol::{
    Base64Bytes, ConnectionPurpose, ControlFrame, Frame, FrameRole, HandshakeMode,
    REMOTE_PROTOCOL_VERSION, RejectCode, RejectedFrame,
};
use serde_json::{Value, json};
use tungstenite::protocol::Role;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

/// The one browser origin the test relay allows.
pub const ALLOWED_ORIGIN: &str = "https://pwa.test";
pub const EVIL_ORIGIN: &str = "https://evil.example";

/// One wire frame past the cap, as a text message.
pub const OVERSIZE_FRAME_BYTES: usize = 1_100_000;

/// How many sealed frames the queue probe pushes before giving up. The
/// per-device message cap (M0 resource table) trips somewhere below it.
pub const QUEUE_PROBE_FRAMES: usize = 4200;

/// The two process-wide admission caps, mirrored here so the quota
/// stress artifact can name what it measured. The relay's own values
/// live in `config.rs`; a drift shows up as a failed assertion, which
/// is the point.
pub const MAX_AUTHENTICATED_CONNECTIONS: usize = 64;
pub const MAX_UNAUTHENTICATED_CONNECTIONS: usize = 64;

// ---------------------------------------------------------------- relay

pub struct Relay {
    child: Child,
    pub port: u16,
    dir: PathBuf,
    /// Captured relay log (stderr), when this instance was started with
    /// `start_capturing_logs`. Used to prove no private material is
    /// logged, never to assert on content.
    pub log_path: Option<PathBuf>,
}

impl Relay {
    pub fn start() -> Self {
        Self::spawn(false)
    }

    /// Start the relay with its log captured to a file inside the
    /// instance directory. `RUST_LOG=info` so the audit lines are in
    /// scope for the privacy scan.
    pub fn start_capturing_logs() -> Self {
        Self::spawn(true)
    }

    /// Start the relay with extra environment overrides — the queue TTL
    /// override exists so a test can prove expiry by waiting seconds.
    pub fn start_with_env(overrides: &[(&str, &str)]) -> Self {
        Self::spawn_configured(true, overrides)
    }

    fn spawn(capture_logs: bool) -> Self {
        Self::spawn_configured(capture_logs, &[])
    }

    fn spawn_configured(capture_logs: bool, overrides: &[(&str, &str)]) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
        let port = listener.local_addr().expect("port").port();
        drop(listener);
        let base = std::env::var_os("FINTWIND_RELAY_BLACKBOX_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if cfg!(windows) {
                    PathBuf::from("C:/Users/Public/Temp/opencode")
                } else {
                    std::env::temp_dir()
                }
            });
        let dir = base.join(format!("fintwind-relay-e2e-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let log_path = capture_logs.then(|| dir.join("relay.log"));
        let binary = std::env::var_os("FINTWIND_RELAY_BLACKBOX_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_fintwind-relay")));
        let mut command = Command::new(binary);
        command.env_clear();
        for name in [
            "PATH",
            "PATHEXT",
            "SystemRoot",
            "WINDIR",
            "ComSpec",
            "SYSTEMDRIVE",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let home = dir.join("home");
        std::fs::create_dir_all(&home).expect("isolated home");
        for name in [
            "HOME",
            "USERPROFILE",
            "XDG_CONFIG_HOME",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
            "TMPDIR",
        ] {
            command.env(name, &home);
        }
        command.current_dir(&dir);
        for (name, value) in overrides {
            command.env(name, value);
        }
        command
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--db")
            .arg(dir.join("relay.sqlite3"))
            .arg("--public-url")
            .arg(format!("http://127.0.0.1:{port}"))
            .env("FINTWIND_RELAY_ORIGINS", ALLOWED_ORIGIN)
            .env("RUST_LOG", if capture_logs { "info" } else { "error" })
            .stdout(Stdio::null());
        if let Some(path) = &log_path {
            let file = std::fs::File::create(path).expect("log file");
            command.stderr(Stdio::from(file));
        } else {
            command.stderr(Stdio::null());
        }
        let child = command.spawn().expect("relay binary spawns");
        let relay = Self {
            child,
            port,
            dir,
            log_path,
        };
        relay
            .write_lifecycle(false)
            .expect("initial process artifact");
        relay.wait_healthy();
        relay
    }

    /// The captured log text. Only meaningful for an instance started
    /// with `start_capturing_logs`.
    pub fn captured_log(&self) -> String {
        match &self.log_path {
            Some(path) => std::fs::read_to_string(path).unwrap_or_default(),
            None => String::new(),
        }
    }

    fn write_lifecycle(&self, stopped: bool) -> std::io::Result<()> {
        let report = json!({ "processId": self.child.id(), "allOwnedProcessesStopped": stopped });
        std::fs::write(self.dir.join("lifecycle.json"), report.to_string())
    }

    pub fn wait_healthy(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some(response) = self.request("GET", "/api/health", &[], None) {
                if response.status == 200 {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("stage=health relay never became healthy");
    }

    /// The health answer as JSON, for the aggregate counters.
    pub fn health_json(&self) -> Value {
        let response = self
            .request("GET", "/api/health", &[], None)
            .expect("health answered");
        response.json()
    }

    /// Still serving: the process survived whatever the last case did.
    pub fn alive(&self) -> bool {
        self.request("GET", "/api/health", &[], None)
            .is_some_and(|response| response.status == 200)
    }

    pub fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Option<HttpResponse> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).ok()?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .expect("write timeout");
        let payload = body.unwrap_or("");
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: {}\r\n",
            self.port,
            payload.len()
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(payload);
        stream.write_all(request.as_bytes()).ok()?;
        stream.flush().ok()?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).ok()?;
        HttpResponse::parse(&raw)
    }

    pub fn post_json(
        &self,
        path: &str,
        body: &Value,
        headers: &[(&str, &str)],
    ) -> Option<HttpResponse> {
        self.request(
            "POST",
            path,
            headers,
            Some(&serde_json::to_string(body).expect("body serializes")),
        )
    }

    /// Open a WebSocket. `None` means the upgrade was refused (bad
    /// origin, malformed request) — the relay never answered 101.
    pub fn ws(&self, path: &str, origin: Option<&str>) -> Option<Ws> {
        Ws::connect(self.port, path, origin)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let stopped = self.child.wait().is_ok();
        let _ = self.write_lifecycle(stopped);
        // Retain synthetic process evidence in its unique directory.
    }
}

pub struct HttpResponse {
    pub status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    pub fn parse(raw: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(raw);
        let (head, body) = text.split_once("\r\n\r\n")?;
        let mut lines = head.split("\r\n");
        let status = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        Some(Self {
            status,
            headers,
            body: body.to_owned(),
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

// ------------------------------------------------------------------- ws

enum WsEvent {
    Text(String),
    Closed,
}

enum WsCommand {
    Text(String),
    Close,
}

/// One WebSocket session. The owning thread reads continuously and
/// writes on command, so a test can send and receive without either
/// direction blocking the other (the relay's own queues are bounded, so
/// a one-way pump would deadlock on volume).
///
/// `stall` parks the reader: the socket stops being drained while the
/// writer keeps working, which is exactly the slow-consumer shape the
/// relay's live outbound bound has to survive.
pub struct Ws {
    commands: Sender<WsCommand>,
    incoming: Receiver<WsEvent>,
    handle: Option<JoinHandle<()>>,
    stalled: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

impl Ws {
    pub fn connect(port: u16, path: &str, origin: Option<&str>) -> Option<Self> {
        let stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
        // The handshake gets a generous timeout; the session loop below
        // switches to a short poll so commands are picked up promptly.
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .expect("write timeout");
        // Fixed key: RFC 6455 only requires the accept value to match,
        // and nothing in this suite replays a handshake.
        let key = BASE64.encode([0x11u8; 16]);
        let mut request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n"
        );
        if let Some(origin) = origin {
            request.push_str(&format!("Origin: {origin}\r\n"));
        }
        request.push_str("\r\n");
        let mut writer = stream.try_clone().ok()?;
        writer.write_all(request.as_bytes()).ok()?;
        writer.flush().ok()?;
        // The head must be consumed exactly: handing a buffered stream
        // to tungstenite would swallow the first frame.
        let mut reader = stream;
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match reader.read(&mut byte) {
                Ok(0) => return None,
                Ok(_) => {
                    head.push(byte[0]);
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => return None,
            }
        }
        if !String::from_utf8_lossy(&head).starts_with("HTTP/1.1 101") {
            return None;
        }
        reader
            .set_read_timeout(Some(Duration::from_millis(10)))
            .expect("poll timeout");
        let socket = WebSocket::from_raw_socket(reader, Role::Client, None);
        let (commands_tx, commands_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        let stalled = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let reader_stalled = Arc::clone(&stalled);
        let reader_closed = Arc::clone(&closed);
        let handle = std::thread::Builder::new()
            .name("e2e-ws".into())
            .spawn(move || {
                let mut socket = socket;
                loop {
                    while let Ok(command) = commands_rx.try_recv() {
                        match command {
                            WsCommand::Text(text) => {
                                if socket.send(Message::text(text)).is_err() {
                                    reader_closed.store(true, Ordering::Release);
                                    let _ = events_tx.send(WsEvent::Closed);
                                    return;
                                }
                            }
                            WsCommand::Close => {
                                let _ = socket.close(None);
                                reader_closed.store(true, Ordering::Release);
                                let _ = events_tx.send(WsEvent::Closed);
                                return;
                            }
                        }
                    }
                    if reader_stalled.load(Ordering::Acquire) {
                        // Parked reader: commands still flow out, but
                        // nothing is drained from the socket.
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    match socket.read() {
                        Ok(Message::Text(text)) => {
                            if events_tx.send(WsEvent::Text(text.to_string())).is_err() {
                                reader_closed.store(true, Ordering::Release);
                                return;
                            }
                        }
                        Ok(Message::Close(_)) => {
                            reader_closed.store(true, Ordering::Release);
                            let _ = events_tx.send(WsEvent::Closed);
                            return;
                        }
                        Err(tungstenite::Error::Io(_)) => {}
                        Err(_) => {
                            reader_closed.store(true, Ordering::Release);
                            let _ = events_tx.send(WsEvent::Closed);
                            return;
                        }
                        Ok(_) => {}
                    }
                }
            })
            .expect("ws thread spawns");
        Some(Self {
            commands: commands_tx,
            incoming: events_rx,
            handle: Some(handle),
            stalled,
            closed,
        })
    }

    /// Park the reader: the peer stops draining its socket while it can
    /// still send. This is the slow-consumer shape.
    pub fn stall(&self) {
        self.stalled.store(true, Ordering::Release);
    }

    /// Whether the session's reader has stopped (peer closed, socket
    /// error). Distinguishes "silent because stalled" from "silent
    /// because the connection is gone".
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Resume draining.
    pub fn unstall(&self) {
        self.stalled.store(false, Ordering::Release);
    }

    pub fn send_frame(&self, frame: &Frame) {
        let text = serde_json::to_string(frame).expect("frame serializes");
        let _ = self.commands.send(WsCommand::Text(text));
    }

    pub fn send_raw(&self, text: String) {
        let _ = self.commands.send(WsCommand::Text(text));
    }

    pub fn next_frame(&self, timeout: Duration) -> Option<Frame> {
        match self.incoming.recv_timeout(timeout) {
            Ok(WsEvent::Text(text)) => serde_json::from_str(&text).ok(),
            Ok(WsEvent::Closed) | Err(_) => None,
        }
    }

    /// Wait for a relay rejection; anything else first is a failure.
    pub fn expect_reject(&self, timeout: Duration) -> (RejectCode, String) {
        let (code, message, _) = self.expect_reject_full(timeout);
        (code, message)
    }

    /// The same refusal with the structured frame reference, for the
    /// cases that assert the sender can correlate what was refused.
    pub fn expect_reject_full(
        &self,
        timeout: Duration,
    ) -> (RejectCode, String, Option<RejectedFrame>) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.next_frame(deadline.saturating_duration_since(Instant::now())) {
                Some(Frame::Control {
                    frame:
                        ControlFrame::Rejected {
                            code,
                            message,
                            frame,
                        },
                }) => return (code, message, frame),
                Some(_) => continue,
                None => break,
            }
        }
        (
            RejectCode::Malformed,
            "no rejection arrived".to_owned(),
            None,
        )
    }

    /// Nothing may arrive: used to prove a frame was not spliced to a
    /// second device.
    pub fn expect_silence(&self, timeout: Duration) {
        assert!(
            self.next_frame(timeout).is_none(),
            "stage=pairing-isolation an unexpected frame reached the wrong device"
        );
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        let _ = self.commands.send(WsCommand::Close);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// ------------------------------------------------------------- identity

/// One registered-or-registerable identity. `source` is the client
/// address the relay sees (Caddy-supplied X-Forwarded-For in
/// production), which is what the per-source rate budgets key on.
pub struct Identity {
    pub signing: SigningKey,
    source: String,
}

/// What an authentication attempt produced: a live session, or the
/// relay's own rejection.
pub enum Admission {
    Live(Ws),
    Refused(RejectCode),
}

impl Identity {
    pub fn new(source: &str) -> Self {
        Self {
            signing: SigningKey::generate(),
            source: source.to_owned(),
        }
    }

    pub fn public_b64(&self) -> String {
        BASE64.encode(&self.signing.verifying)
    }

    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.signing.sign(message)
    }

    pub fn challenge(&self, relay: &Relay, role: &str) -> Vec<u8> {
        let response = relay
            .post_json(
                "/api/challenge",
                &json!({ "role": role }),
                &[("x-forwarded-for", self.source.as_str())],
            )
            .expect("challenge stage answered");
        assert_eq!(
            response.status, 200,
            "stage=challenge status={}",
            response.status
        );
        BASE64
            .decode(
                response.json()["challenge"]
                    .as_str()
                    .expect("challenge field"),
            )
            .expect("challenge base64")
    }

    pub fn register(&self, relay: &Relay, role: &str) -> String {
        let challenge = self.challenge(relay, role);
        let signature = self.sign(&challenge);
        let response = relay
            .post_json(
                "/api/register",
                &json!({
                    "role": role,
                    "signingPublicKey": self.public_b64(),
                    "challenge": BASE64.encode(&challenge),
                    "signature": BASE64.encode(&signature),
                }),
                &[("x-forwarded-for", self.source.as_str())],
            )
            .expect("register stage answered");
        assert_eq!(
            response.status, 200,
            "stage=register status={}",
            response.status
        );
        let key = if role == "host" { "hostId" } else { "deviceId" };
        response.json()[key].as_str().expect(key).to_owned()
    }

    pub fn connect(
        &self,
        relay: &Relay,
        role: FrameRole,
        host_id: Option<&str>,
        device_id: Option<&str>,
        purpose: ConnectionPurpose,
    ) -> Admission {
        let role_name = match role {
            FrameRole::Host => "host",
            FrameRole::Device => "device",
        };
        let challenge = self.challenge(relay, role_name);
        let signature = self.sign(&challenge);
        let auth = Frame::Auth {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            role,
            host_id: host_id.and_then(|id| Uuid::parse_str(id).ok()),
            device_id: device_id.and_then(|id| Uuid::parse_str(id).ok()),
            purpose,
            challenge: Base64Bytes(challenge),
            signature: Base64Bytes(signature),
        };
        let session = relay
            .ws("/ws", Some(ALLOWED_ORIGIN))
            .expect("stage=ws-upgrade the allowed origin must upgrade");
        session.send_frame(&auth);
        // A successful authentication is acknowledged before any business
        // frame; a refusal is the relay's own rejection.
        match session.next_frame(Duration::from_secs(15)) {
            Some(Frame::Control {
                frame: ControlFrame::RelayAck { .. },
            }) => Admission::Live(session),
            Some(Frame::Control {
                frame: ControlFrame::Rejected { code, .. },
            }) => Admission::Refused(code),
            other => panic!("stage=ws-auth unexpected first frame: {other:?}"),
        }
    }

    pub fn host_session(&self, relay: &Relay, host_id: &str) -> Ws {
        match self.connect(
            relay,
            FrameRole::Host,
            Some(host_id),
            None,
            ConnectionPurpose::Session,
        ) {
            Admission::Live(session) => session,
            Admission::Refused(code) => panic!("stage=host-session refused code={code:?}"),
        }
    }

    pub fn device_session(&self, relay: &Relay, device_id: &str) -> Ws {
        match self.connect(
            relay,
            FrameRole::Device,
            None,
            Some(device_id),
            ConnectionPurpose::Session,
        ) {
            Admission::Live(session) => session,
            Admission::Refused(code) => panic!("stage=device-session refused code={code:?}"),
        }
    }

    /// Authenticate a device session and report the relay's own answer,
    /// for quota tests that must observe a refusal instead of panicking.
    pub fn connect_session(&self, relay: &Relay, device_id: &str) -> Admission {
        self.connect(
            relay,
            FrameRole::Device,
            None,
            Some(device_id),
            ConnectionPurpose::Session,
        )
    }

    pub fn host_pairing(&self, relay: &Relay, host_id: &str) -> Ws {
        match self.connect(
            relay,
            FrameRole::Host,
            Some(host_id),
            None,
            ConnectionPurpose::Pairing,
        ) {
            Admission::Live(session) => session,
            Admission::Refused(code) => panic!("stage=host-pairing refused code={code:?}"),
        }
    }
}

pub fn sealed(seq: u64, host_id: &str, device_id: Option<&str>) -> Frame {
    Frame::Sealed {
        seq,
        host_id: Uuid::parse_str(host_id).expect("host uuid"),
        device_id: device_id.map(|id| Uuid::parse_str(id).expect("device uuid")),
        // Opaque to the relay: a fixed non-secret filler keeps the frame
        // small and the byte budget out of the way of the message cap.
        ciphertext: Base64Bytes(vec![0x5a; 24]),
    }
}

/// A sealed frame with a chosen ciphertext length, for exercising byte
/// budgets. The filler stays non-secret and is never asserted on.
pub fn sealed_with_payload(
    seq: u64,
    host_id: &str,
    device_id: &str,
    payload_bytes: usize,
) -> Frame {
    Frame::Sealed {
        seq,
        host_id: Uuid::parse_str(host_id).expect("host uuid"),
        device_id: Some(Uuid::parse_str(device_id).expect("device uuid")),
        ciphertext: Base64Bytes(vec![0x5a; payload_bytes]),
    }
}

/// SHA-256 over the relay's sources, so a stress artifact can name the
/// exact code it measured. Hashes content only; no path or secret is
/// included.
pub fn relay_source_hash() -> String {
    use sha2::{Digest, Sha256};
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&src)
        .expect("src dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();
    let mut hasher = Sha256::new();
    for path in files {
        hasher.update(
            path.file_name()
                .expect("file name")
                .to_string_lossy()
                .as_bytes(),
        );
        hasher.update(std::fs::read(&path).unwrap_or_default());
    }
    format!("{:x}", hasher.finalize())
}

/// One unauthenticated connection that holds its admission slot without
/// ever completing its head: the request line is sent once, then header
/// lines trickle in. Every read returns data, so the relay's
/// authentication timeout never fires and the slot stays taken — the
/// slowloris shape the unauthenticated cap exists for.
pub struct SlowHead {
    stream: TcpStream,
}

impl SlowHead {
    pub fn open(relay: &Relay) -> Self {
        let mut stream = TcpStream::connect(("127.0.0.1", relay.port)).expect("slow head connects");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("write timeout");
        stream
            .write_all(b"GET /api/health HTTP/1.1\r\n")
            .expect("request line");
        stream.flush().expect("flush");
        Self { stream }
    }

    /// Send one more header line, keeping the read side alive.
    pub fn trickle(&mut self) {
        let _ = self.stream.write_all(b"X-Pad: 1\r\n");
        let _ = self.stream.flush();
    }

    /// Whether the relay has closed its side. A slow head never receives
    /// data — the relay answers nothing while the head is incomplete — so
    /// a readable byte is an EOF, and a read timeout means still open.
    /// Short timeout so a probe never adds measurable wait of its own.
    pub fn is_closed(&mut self) -> bool {
        let _ = self
            .stream
            .set_read_timeout(Some(Duration::from_millis(300)));
        let mut byte = [0u8; 1];
        match self.stream.read(&mut byte) {
            Ok(0) => true,
            Ok(_) => false,
            Err(_) => false, // timed out: the peer is still holding on
        }
    }
}

pub fn handshake(step: u8, host_id: &str, device_id: Option<&str>, role: FrameRole) -> Frame {
    Frame::Handshake {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role,
        host_id: Uuid::parse_str(host_id).expect("host uuid"),
        device_id: device_id.map(|id| Uuid::parse_str(id).expect("device uuid")),
        mode: HandshakeMode::Pairing,
        step,
        payload: Base64Bytes(vec![0x5a; 16]),
    }
}

/// A device's business-session handshake. `mode` is Connect — the path
/// the foreign-device binding (C3) lives on; pairing handshakes name no
/// device and route on the invite nonce instead.
pub fn connect_handshake(step: u8, host_id: &str, device_id: &str, role: FrameRole) -> Frame {
    Frame::Handshake {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role,
        host_id: Uuid::parse_str(host_id).expect("host uuid"),
        device_id: Some(Uuid::parse_str(device_id).expect("device uuid")),
        mode: HandshakeMode::Connect,
        step,
        payload: Base64Bytes(vec![0x5a; 16]),
    }
}

pub fn link_frame(
    host_id: &str,
    device_id: &str,
    device_public_key: &[u8],
    nonce: &[u8],
    signing: &SigningKey,
) -> Frame {
    // The relay verifies over raw 16-byte UUIDs, matching the host side.
    let mut signed = Uuid::parse_str(host_id)
        .expect("host uuid")
        .as_bytes()
        .to_vec();
    signed.extend_from_slice(Uuid::parse_str(device_id).expect("device uuid").as_bytes());
    signed.extend_from_slice(device_public_key);
    signed.extend_from_slice(nonce);
    Frame::LinkDevice {
        host_id: Uuid::parse_str(host_id).expect("host uuid"),
        device_id: Uuid::parse_str(device_id).expect("device uuid"),
        device_public_key: Base64Bytes(device_public_key.to_vec()),
        nonce: Base64Bytes(nonce.to_vec()),
        signature: Base64Bytes(signing.sign(&signed)),
    }
}

/// The teardown declaration. Signs `host || device || nonce` — no key on
/// an unlink, matching the relay's verification order.
pub fn unlink_frame(host_id: &str, device_id: &str, nonce: &[u8], signing: &SigningKey) -> Frame {
    let mut signed = Uuid::parse_str(host_id)
        .expect("host uuid")
        .as_bytes()
        .to_vec();
    signed.extend_from_slice(Uuid::parse_str(device_id).expect("device uuid").as_bytes());
    signed.extend_from_slice(nonce);
    Frame::UnlinkDevice {
        host_id: Uuid::parse_str(host_id).expect("host uuid"),
        device_id: Uuid::parse_str(device_id).expect("device uuid"),
        nonce: Base64Bytes(nonce.to_vec()),
        signature: Base64Bytes(signing.sign(&signed)),
    }
}

/// Arm a pairing invite at the relay the way the host does before
/// showing the QR, and return the nonce it stored. Each call mints a
/// distinct nonce: the relay stores it by hash, so a repeat would be a
/// primary-key conflict rather than a fresh invite.
pub fn arm_pairing_offer(host: &Identity, relay: &Relay, host_id: &str) -> Vec<u8> {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let distinct = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut nonce = Vec::with_capacity(32);
    nonce.extend_from_slice(&distinct.to_le_bytes());
    nonce.extend_from_slice(&[0x5au8; 28]);
    let challenge = host.challenge(relay, "host");
    // The REST offer signs the host id as text plus the nonce.
    let mut message = host_id.as_bytes().to_vec();
    message.extend_from_slice(&nonce);
    let response = relay
        .post_json(
            "/api/pairing/offer",
            &json!({
                "hostId": host_id,
                "nonce": BASE64.encode(&nonce),
                "challenge": BASE64.encode(&challenge),
                "signature": BASE64.encode(host.sign(&message)),
            }),
            &[("x-forwarded-for", host.source.as_str())],
        )
        .expect("pairing offer stage answered");
    assert_eq!(
        response.status, 200,
        "stage=pairing-offer status={}",
        response.status
    );
    nonce
}

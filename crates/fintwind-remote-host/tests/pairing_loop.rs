//! End-to-end pairing loop against a real relay and a real `--pair`
//! host process, entirely on loopback.
//!
//! The test plays the device: it registers at the relay, reads the invite
//! the host printed, opens the XX handshake through the relay, sends its
//! hello, and — after piping `y` into the host's confirmation prompt —
//! waits for the acceptance decision and the relay's link receipt. The
//! pass condition is threefold: the decision says accepted, the relay
//! confirms the link, and the host's grant file now carries the device
//! with exactly the public key the handshake bound.

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

use fintwind_remote_crypto::{pairing_code, PairingBinding, PairingInitiator, SigningKey};
use fintwind_remote_protocol::{
    Base64Bytes, ControlFrame, Frame, FrameRole, HandshakeMode, MessageBody, PairingInvite,
    REMOTE_PROTOCOL_VERSION, SealedMessage,
};

type DeviceSocket = WebSocket<MaybeTlsStream<TcpStream>>;

/// The sibling relay binary lives in the shared workspace target dir.
fn relay_exe() -> PathBuf {
    #[cfg(windows)]
    const NAME: &str = "fintwind-relay.exe";
    #[cfg(not(windows))]
    const NAME: &str = "fintwind-relay";
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(NAME)
}

struct Child2(std::process::Child);

impl Drop for Child2 {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn pairing_loop_confirms_grants_and_links() {
    let port = free_port();
    let origin = format!("http://127.0.0.1:{port}");
    let ws_url = format!("ws://127.0.0.1:{port}/ws");
    let relay_db = temp_dir("pair-relay").join("relay.sqlite3");
    let host_data = temp_dir("pair-host");
    std::fs::create_dir_all(&host_data).expect("host dir");

    // ---- relay up ----
    let relay = Command::new(relay_exe())
        .args([
            "--bind",
            &format!("127.0.0.1:{port}"),
            "--db",
            relay_db.to_str().expect("db path"),
        ])
        .stderr(Stdio::from(
            std::fs::File::create(relay_db.with_extension("relay.log")).expect("relay log"),
        ))
        .spawn()
        .expect("relay process");
    let _relay = Child2(relay);
    wait_http_ok(&origin);

    // ---- host --pair: interactive, stdout parsed, stdin piped ----
    let mut host = Command::new(env!("CARGO_BIN_EXE_fintwind-remote-host"))
        .args([
            "--pair",
            "--relay-origin",
            &origin,
            "--data-dir",
            host_data.to_str().expect("host dir"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(
            std::fs::File::create(host_data.join("pair.log")).expect("pair log"),
        ))
        .spawn()
        .expect("host --pair process");

    // ---- device identity + registration ----
    let device_signing = SigningKey::generate();
    let device_noise = fintwind_remote_crypto::IdentityKey::generate().expect("device noise key");
    let device_id = register_identity(&origin, &device_signing);

    // ---- read the invite the host printed ----
    let stdout = host.stdout.take().expect("host stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let invite = wait_for_invite(&mut reader);
    let binding = PairingBinding {
        protocol_version: invite.protocol_version,
        host_id: invite.host_id,
        nonce: BASE64_STANDARD.decode(&invite.nonce).expect("invite nonce"),
        expires_at_ms: invite.expires_at_ms,
        relay_origin: invite.relay_origin.clone(),
    };
    let host_public = BASE64_STANDARD.decode(&invite.host_public_key).expect("host key");

    // ---- connect, authenticate, open the XX pairing handshake ----
    let (mut socket, _) = tungstenite::connect(&ws_url).expect("device ws connect");
    apply_read_timeout(&socket, Some(Duration::from_millis(250)));
    device_auth(&mut socket, &device_id, &device_signing);

    let mut initiator =
        PairingInitiator::new(&binding, &device_noise).expect("xx initiator");
    let message1 = initiator.write_first().expect("xx message 1");
    send_frame(
        &mut socket,
        Frame::Handshake {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            role: FrameRole::Device,
            host_id: invite.host_id,
            device_id: None,
            mode: HandshakeMode::Pairing,
            step: 0,
            payload: Base64Bytes(message1),
        },
    );

    // step 1 from the host, then message 3 with our hello inside.
    let mut channel = loop {
        let frame = read_frame(&mut socket);
        match frame {
            Frame::Control { .. } => continue, // auth ack, pings, receipts
            Frame::Handshake {
                mode: HandshakeMode::Pairing,
                step: 1,
                payload,
                ..
            } => {
                let _offer = initiator.read_reply(&payload.0).expect("xx message 2");
                let hello = SealedMessage {
                    protocol_version: REMOTE_PROTOCOL_VERSION,
                    host_id: invite.host_id,
                    device_id: device_id.parse().expect("device uuid"),
                    sent_at_ms: now_ms(),
                    body: MessageBody::PairingHello {
                        device_id: device_id.parse().expect("device uuid"),
                        device_name: Some("loop test phone".to_owned()),
                    },
                };
                let hello_bytes = serde_json::to_string(&hello).expect("hello json");
                let (message3, channel) =
                    initiator.finish(hello_bytes.as_bytes()).expect("xx message 3");
                send_frame(
                    &mut socket,
                    Frame::Handshake {
                        protocol_version: REMOTE_PROTOCOL_VERSION,
                        role: FrameRole::Device,
                        host_id: invite.host_id,
                        device_id: None,
                        mode: HandshakeMode::Pairing,
                        step: 2,
                        payload: Base64Bytes(message3),
                    },
                );
                break channel;
            }
            Frame::Control {
                frame: ControlFrame::Rejected { code, message },
            } => panic!("relay refused the pairing handshake: {code:?} {message}"),
            other => panic!("unexpected frame during pairing: {other:?}"),
        }
    };

    // ---- both ends must derive the same check code ----
    let our_code = pairing_code(&host_public, &device_noise.public, &binding);
    let printed_code = wait_for_line(&mut reader, "check code:");
    let printed = printed_code
        .split("check code:")
        .nth(1)
        .map(str::trim)
        .expect("host printed a check code")
        .to_owned();
    assert_eq!(
        our_code, printed,
        "both ends must derive the same check code from the same handshake"
    );

    // ---- confirm on the host side (piping y into its stdin) ----
    println!("test: sending confirmation");
    host.stdin
        .take()
        .expect("host stdin")
        .write_all(b"y\n")
        .expect("confirmation send");

    // ---- the acceptance decision, sealed, arrives over the pairing ----
    // exemption before any link exists. The relay's Linked receipt goes to
    // the host only; the device side of "linked" is the grant file plus
    // the relay store, both asserted below.
    let deadline = Instant::now() + Duration::from_secs(20);
    let decided = loop {
        assert!(Instant::now() < deadline, "pairing never completed");
        match socket.read() {
            Ok(Message::Text(text)) => {
                let frame: Frame = serde_json::from_str(&text).expect("frame json");
                match frame {
                    Frame::Sealed { ciphertext, .. } => {
                        let plaintext = channel.decrypt(&ciphertext.0).expect("decrypt");
                        let sealed: SealedMessage =
                            serde_json::from_slice(&plaintext).expect("sealed json");
                        match sealed.body {
                            MessageBody::PairingDecision { accepted, .. } => {
                                println!("device saw the pairing decision: accepted={accepted}");
                                assert!(accepted, "the host must accept the piped y");
                                break true;
                            }
                            other => panic!("unexpected sealed body: {other:?}"),
                        }
                    }
                    Frame::Control {
                        frame: ControlFrame::Ping { sent_at_ms },
                    } => {
                        send_frame(
                            &mut socket,
                            Frame::Control {
                                frame: ControlFrame::Pong { sent_at_ms },
                            },
                        );
                    }
                    _ => {}
                }
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(error) => panic!("socket error: {error}"),
            Ok(_) => {}
        }
    };
    assert!(decided);

    // ---- the grant file carries the device under its handshake-bound key ----
    let grant_text = std::fs::read_to_string(host_data.join("paired-device.json"))
        .expect("grant file after pairing");
    let grant: serde_json::Value = serde_json::from_str(&grant_text).expect("grant json");
    let grants = grant
        .get("grants")
        .and_then(serde_json::Value::as_array)
        .expect("grant array");
    assert_eq!(grants.len(), 1, "exactly one grant after the first pairing");
    assert_eq!(
        grants[0].get("deviceId").and_then(serde_json::Value::as_str),
        Some(device_id.as_str())
    );
    assert_eq!(
        grants[0].get("devicePublicKey").and_then(serde_json::Value::as_str),
        Some(BASE64_STANDARD.encode(&device_noise.public).as_str()),
        "the grant must pin the public key the XX handshake bound"
    );

    // The host process exits cleanly after pairing, and the relay store
    // carries the live link the host declared.
    let status = host.wait().expect("host exit");
    println!("host exited with {status}");
    assert!(status.success(), "host --pair must exit cleanly: {status}");
    let link = rusqlite::Connection::open(&relay_db)
        .expect("relay db opens")
        .query_row(
            "SELECT unlinked_at_ms FROM pairing_links WHERE host_id = ?1 AND device_id = ?2",
            rusqlite::params![invite.host_id.to_string(), device_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .expect("link row queried");
    assert!(link.is_none(), "the pairing link must be live, not unlinked");
}

fn host_tool_idle() {
    // Placeholder to keep CARGO_BIN_EXE linkage honest if helpers change.
    let _ = env!("CARGO_BIN_EXE_fintwind-remote-host");
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("local addr")
        .port()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{name}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn apply_read_timeout(socket: &DeviceSocket, timeout: Option<Duration>) {
    if let MaybeTlsStream::Plain(tcp) = socket.get_ref() {
        let _ = tcp.set_read_timeout(timeout);
    }
}

fn send_frame(socket: &mut DeviceSocket, frame: Frame) {
    socket
        .send(Message::text(serde_json::to_string(&frame).expect("json")))
        .expect("frame send");
}

fn read_frame(socket: &mut DeviceSocket) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "frame never arrived");
        match socket.read() {
            Ok(Message::Text(text)) => {
                return serde_json::from_str(&text).expect("frame json");
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(error) => panic!("socket error: {error}"),
            Ok(_) => {}
        }
    }
}

fn wait_http_ok(origin: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(Instant::now() < deadline, "relay health never came up");
        if let Ok(mut stream) = TcpStream::connect(origin.trim_start_matches("http://")) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let request = format!(
                "GET /api/health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                origin.trim_start_matches("http://")
            );
            if stream.write_all(request.as_bytes()).is_ok() {
                let mut raw = Vec::new();
                if stream.read_to_end(&mut raw).is_ok()
                    && String::from_utf8_lossy(&raw).contains(" 200 ")
                {
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn post_json(origin: &str, path: &str, body: &serde_json::Value) -> serde_json::Value {
    let body = serde_json::to_string(body).expect("json");
    let authority = origin.trim_start_matches("http://");
    let mut stream = TcpStream::connect(authority).expect("relay connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("write timeout");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("send request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let text = String::from_utf8_lossy(&raw);
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let payload = parts.next().unwrap_or_default();
    let status = head.lines().next().unwrap_or_default();
    assert!(status.contains(" 200 "), "relay refused {path}: {status} {payload}");
    serde_json::from_str(payload).expect("relay json response")
}

fn register_identity(origin: &str, signing: &SigningKey) -> String {
    let challenge = post_json(origin, "/api/challenge", &serde_json::json!({"role": "device"}));
    let challenge_b64 = challenge
        .get("challenge")
        .and_then(serde_json::Value::as_str)
        .expect("challenge")
        .to_owned();
    let challenge_bytes = BASE64_STANDARD.decode(&challenge_b64).expect("challenge");
    let signature = signing.sign(&challenge_bytes);
    let registered = post_json(
        origin,
        "/api/register",
        &serde_json::json!({
            "role": "device",
            "signingPublicKey": BASE64_STANDARD.encode(&signing.verifying),
            "challenge": challenge_b64,
            "signature": BASE64_STANDARD.encode(signature),
        }),
    );
    registered
        .get("deviceId")
        .and_then(serde_json::Value::as_str)
        .expect("assigned device id")
        .to_owned()
}

fn device_auth(socket: &mut DeviceSocket, device_id: &str, signing: &SigningKey) {
    let authority = match socket.get_ref() {
        MaybeTlsStream::Plain(tcp) => tcp.peer_addr().expect("peer addr").to_string(),
        _ => panic!("plain ws expected in tests"),
    };
    let origin = format!("http://{authority}");
    let challenge = post_json(&origin, "/api/challenge", &serde_json::json!({"role": "device"}));
    let challenge_b64 = challenge
        .get("challenge")
        .and_then(serde_json::Value::as_str)
        .expect("challenge")
        .to_owned();
    let challenge_bytes = BASE64_STANDARD.decode(&challenge_b64).expect("challenge");
    let signature = signing.sign(&challenge_bytes);
    send_frame(
        socket,
        Frame::Auth {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            role: FrameRole::Device,
            host_id: None,
            device_id: Some(device_id.parse().expect("device uuid")),
            challenge: Base64Bytes(challenge_bytes),
            signature: Base64Bytes(signature),
        },
    );
}

/// Pull host stdout until the invite line (a JSON object) appears.
fn wait_for_invite(reader: &mut impl std::io::BufRead) -> PairingInvite {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "invite never printed");
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => panic!("host closed stdout before the invite"),
            Ok(_) => {}
            Err(error) => panic!("read failed: {error}"),
        }
        let trimmed = line.trim();
        if trimmed.starts_with('{') && trimmed.contains("nonce") {
            return serde_json::from_str(trimmed).expect("invite json");
        }
    }
}

/// Pull host stdout until a line containing `needle` appears.
fn wait_for_line(reader: &mut impl std::io::BufRead, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "line {needle:?} never printed");
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => panic!("host closed stdout before {needle:?}"),
            Ok(_) => {}
            Err(error) => panic!("read failed: {error}"),
        }
        if line.contains(needle) {
            return line;
        }
    }
}

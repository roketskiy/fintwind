//! Manual smoke test: the real host daemon, a real local `fintwind-daemon`,
//! and the production relay. Unlike `device_loop.rs` this exercises the
//! daemon leg of the adapter — the one path a loopback test cannot reach.
//!
//! Run with: cargo test -p fintwind-remote-host --test daemon_smoke -- --ignored --nocapture
//!
//! What it does:
//! 1. starts a local `fintwind-daemon` (debug build; its state store lands
//!    in crates/temp, never in real user data),
//! 2. mints a host identity and a device identity, registers both with the
//!    production relay over HTTPS/WSS,
//! 3. injects the pairing link on the relay host over SSH (harness for the
//!    M3 pairing flow),
//! 4. runs the KK handshake through the relay and issues one AttachSession
//!    for a random session id.
//!
//! The pass condition is an `Executed` outcome whose detail carries
//! `supportsSteer` — a payload only the real daemon can produce. Before
//! the daemon leg existed this exact call came back
//! `Rejected { UnknownRoute, "workspace unreachable" }`.

use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

use fintwind_remote_crypto::{ConnectInitiator, IdentityKey, SecureChannel, SigningKey};
use fintwind_remote_protocol::{
    Base64Bytes, ControlFrame, Frame, FrameRole, HandshakeMode, MessageBody, OperationKind,
    OperationRequest, SealedMessage, REMOTE_PROTOCOL_VERSION,
};

type DeviceSocket = WebSocket<MaybeTlsStream<TcpStream>>;

const RELAY_ORIGIN: &str = "https://relay.fintwind.xyz";
const RELAY_HOST: &str = "relay.fintwind.xyz";
const SSH_USER_AT_HOST: &str = "root@165.154.64.159";

fn ssh_key() -> String {
    std::env::var("RELAY_SSH_KEY")
        .unwrap_or_else(|_| r"C:\Users\潘雷\.ssh\ucloud_root_ed25519".to_owned())
}

fn tool_exe(name: &str) -> PathBuf {
    #[cfg(windows)]
    let name = format!("{name}.exe");
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(name)
}

struct Child2(Child);

impl Drop for Child2 {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "manual smoke: needs the production relay and SSH access"]
fn daemon_smoke_reaches_a_real_fintwind_daemon() {
    let work = temp_dir("host-smoke");
    let host_data = work.join("host");
    std::fs::create_dir_all(&host_data).expect("host data dir");

    // ---- local daemon up (debug build: state store stays in crates/temp) --
    let daemon = Command::new(tool_exe("fintwind-daemon"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .env("FINTWIND_DAEMON_TOKEN", "smoke-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fintwind-daemon; build it with cargo build -p fintwind-daemon");
    let mut daemon = Child2(daemon);
    let stdout = daemon.0.stdout.take().expect("daemon stdout");
    let ready_line = BufReader::new(stdout)
        .lines()
        .next()
        .expect("daemon prints a ready line")
        .expect("daemon ready line");
    let ready: serde_json::Value = serde_json::from_str(&ready_line).expect("daemon ready json");
    let daemon_address = ready
        .get("address")
        .and_then(serde_json::Value::as_str)
        .expect("daemon address")
        .to_owned();
    println!("daemon ready at {daemon_address}");

    // ---- host identity (keys only; the daemon registers on start) ----
    let identity_json = host_tool(&[
        "--print-identity",
        "--data-dir",
        host_data.to_str().expect("host dir"),
    ]);
    let host_noise_public = decode_b64(
        identity_json
            .get("noisePublicKey")
            .and_then(serde_json::Value::as_str)
            .expect("host noise key"),
    );

    // ---- device identity registered at the production relay ----
    let device_signing = SigningKey::generate();
    let device_noise = IdentityKey::generate().expect("device noise key");
    let device_id = register_device(&device_signing);
    println!("device registered: {device_id}");

    // ---- grant file: the host trusts exactly this device ----
    std::fs::write(
        host_data.join("paired-device.json"),
        serde_json::json!({
            "grants": [{
                "deviceId": device_id,
                "devicePublicKey": BASE64_STANDARD.encode(&device_noise.public),
                "grantVersion": 1,
                "grantedAtMs": 0,
            }]
        })
        .to_string(),
    )
    .expect("grant file");

    // ---- host daemon up: registers itself, connects to the relay ----
    let host = Command::new(tool_exe("fintwind-remote-host"))
        .args([
            "--relay-origin",
            RELAY_ORIGIN,
            "--daemon-address",
            &daemon_address,
            "--daemon-token-env",
            "SMOKE_DAEMON_TOKEN",
            "--data-dir",
            host_data.to_str().expect("host dir"),
        ])
        .env("SMOKE_DAEMON_TOKEN", "smoke-token")
        .stderr(Stdio::from(
            std::fs::File::create(host_data.join("host-daemon.log")).expect("host log"),
        ))
        .spawn()
        .expect("spawn fintwind-remote-host");
    let host = Child2(host);

    let host_id = wait_for_file(&host_data.join("host-id.txt"));
    println!("host registered: {host_id}");

    // ---- pairing link injected on the relay host (M3 stands in here) ----
    inject_link_on_relay(&host_id, &device_id);

    // ---- device connects over WSS and runs KK ----
    let (mut socket, _) = tungstenite::connect(format!("wss://{RELAY_HOST}/ws")).expect("wss");
    apply_read_timeout(&socket, Some(Duration::from_millis(300)));
    device_auth(&mut socket, &device_id, &device_signing);
    let mut channel =
        device_handshake(&mut socket, &host_noise_public, &host_id, &device_id, &device_noise);

    // ---- the probe: AttachSession through to the real daemon ----
    let operation = OperationRequest {
        operation_id: Uuid::new_v4(),
        kind: OperationKind::AttachSession,
        summary: "smoke: attach through the production relay".to_owned(),
        expires_at_ms: now_ms() + 120_000,
        session_id: Some(Uuid::new_v4()),
        payload: serde_json::json!({}),
    };
    send_sealed(
        &mut socket,
        &mut channel,
        &host_id,
        &device_id,
        MessageBody::Operation(operation.clone()),
    );
    let outcome = wait_for_operation_result(&mut socket, &mut channel, operation.operation_id);
    println!("outcome: {}", serde_json::to_string_pretty(&outcome).expect("json"));
    match outcome.state {
        fintwind_remote_protocol::OperationState::Executed => {}
        other => panic!("expected Executed from the real daemon, got {other:?}"),
    }
    let detail = outcome.detail.unwrap_or_default();
    assert!(
        detail.contains("supportsSteer"),
        "the daemon's AttachSession receipt must name supportsSteer: {detail}"
    );
    println!("[OK] production relay -> host -> daemon round trip proven.");

    // Both children die with the scope; Child2's Drop kills them.
    drop(host);
    drop(daemon);
}

fn host_tool(arguments: &[&str]) -> serde_json::Value {
    let output = Command::new(tool_exe("fintwind-remote-host"))
        .args(arguments)
        .output()
        .expect("host tool runs");
    assert!(
        output.status.success(),
        "host tool failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("host tool json")
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

fn decode_b64(value: &str) -> Vec<u8> {
    BASE64_STANDARD.decode(value).expect("base64 value")
}

fn apply_read_timeout(socket: &DeviceSocket, timeout: Option<Duration>) {
    match socket.get_ref() {
        MaybeTlsStream::Plain(tcp) => {
            let _ = tcp.set_read_timeout(timeout);
        }
        MaybeTlsStream::Rustls(tls) => {
            let _ = tls.sock.set_read_timeout(timeout);
        }
        _ => {}
    }
}

fn wait_for_file(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "host id never appeared");
        if let Ok(text) = std::fs::read_to_string(path) {
            let id = text.trim().to_owned();
            if !id.is_empty() {
                return id;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// HTTPS REST call via the system curl (Schannel trust store). A manual
/// smoke may lean on curl; the CI loop test speaks plain HTTP natively.
fn post_json_https(path: &str, body: &serde_json::Value) -> serde_json::Value {
    let output = Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "15",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            &serde_json::to_string(body).expect("json"),
            &format!("{RELAY_ORIGIN}{path}"),
        ])
        .output()
        .expect("curl runs");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| {
            panic!(
                "relay response unreadable for {path}: {error}; body={:?}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
    assert!(
        parsed.get("error").is_none(),
        "relay refused {path}: {parsed}"
    );
    parsed
}

fn register_device(signing: &SigningKey) -> String {
    let challenge = post_json_https("/api/challenge", &serde_json::json!({ "role": "device" }));
    let challenge_b64 = challenge
        .get("challenge")
        .and_then(serde_json::Value::as_str)
        .expect("challenge")
        .to_owned();
    let challenge_bytes = decode_b64(&challenge_b64);
    let signature = signing.sign(&challenge_bytes);
    let registered = post_json_https(
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

/// Feed a tiny python script to the relay host over SSH; it writes the
/// pairing link straight into the production store. This stands in for
/// the M3 pairing flow and is the only mutating harness step here.
fn inject_link_on_relay(host_id: &str, device_id: &str) {
    let script = format!(
        "import sqlite3\n\
         c = sqlite3.connect('/var/lib/fintwind-relay/relay.sqlite3')\n\
         c.execute('INSERT INTO pairing_links (host_id, device_id, linked_at_ms) VALUES (?,?,?) \
                    ON CONFLICT(host_id, device_id) DO UPDATE SET unlinked_at_ms = NULL',\n\
                    ('{host_id}', '{device_id}', {ms}))\n\
         c.commit()\n\
         print('linked')\n",
        host_id = host_id,
        device_id = device_id,
        ms = now_ms(),
    );
    let script_path = temp_dir("smoke-link").join("link.py");
    std::fs::write(&script_path, script).expect("link script");
    let output = Command::new("ssh")
        .args([
            "-i",
            &ssh_key(),
            "-o",
            "ConnectTimeout=10",
            SSH_USER_AT_HOST,
            "python3 -",
        ])
        .stdin(Stdio::from(std::fs::File::open(&script_path).expect("link script opens")))
        .output()
        .expect("ssh runs");
    assert!(
        output.status.success(),
        "link injection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("linked"), "unexpected link output: {text}");
    println!("pairing link injected on the relay");
}

fn device_auth(socket: &mut DeviceSocket, device_id: &str, signing: &SigningKey) {
    let challenge = post_json_https("/api/challenge", &serde_json::json!({"role": "device"}));
    let challenge_b64 = challenge
        .get("challenge")
        .and_then(serde_json::Value::as_str)
        .expect("challenge")
        .to_owned();
    let challenge_bytes = decode_b64(&challenge_b64);
    let signature = signing.sign(&challenge_bytes);
    let frame = Frame::Auth {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role: FrameRole::Device,
        host_id: None,
        device_id: Some(device_id.parse().expect("device uuid")),
        challenge: Base64Bytes(challenge_bytes),
        signature: Base64Bytes(signature),
    };
    socket
        .send(Message::text(serde_json::to_string(&frame).expect("json")))
        .expect("auth send");
}

fn device_handshake(
    socket: &mut DeviceSocket,
    host_noise_public: &[u8],
    host_id: &str,
    device_id: &str,
    device_noise: &IdentityKey,
) -> SecureChannel {
    let host_uuid: Uuid = host_id.parse().expect("host uuid");
    let device_uuid: Uuid = device_id.parse().expect("device uuid");
    let mut initiator = ConnectInitiator::new(
        REMOTE_PROTOCOL_VERSION,
        device_noise,
        host_noise_public,
        host_uuid,
        device_uuid,
    )
    .expect("kk initiator");
    let first = initiator.write_first().expect("kk message 1");
    let frame = Frame::Handshake {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role: FrameRole::Device,
        host_id: host_uuid,
        device_id: Some(device_uuid),
        mode: HandshakeMode::Connect,
        step: 0,
        payload: Base64Bytes(first),
    };
    socket
        .send(Message::text(serde_json::to_string(&frame).expect("json")))
        .expect("handshake send");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "kk reply never arrived");
        match socket.read() {
            Ok(Message::Text(text)) => {
                let frame: Frame = serde_json::from_str(&text).expect("frame json");
                match frame {
                    Frame::Handshake {
                        mode: HandshakeMode::Connect,
                        step: 1,
                        payload,
                        ..
                    } => return initiator.finish(&payload.0).expect("kk completes"),
                    Frame::Control {
                        frame: ControlFrame::Rejected { code, message },
                    } => panic!("relay rejected handshake: {code:?} {message}"),
                    _ => continue,
                }
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(error) => panic!("socket error during handshake: {error}"),
            Ok(_) => {}
        }
    }
}

fn send_sealed(
    socket: &mut DeviceSocket,
    channel: &mut SecureChannel,
    host_id: &str,
    device_id: &str,
    body: MessageBody,
) {
    let sealed = SealedMessage {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        host_id: host_id.parse().expect("host uuid"),
        device_id: device_id.parse().expect("device uuid"),
        sent_at_ms: now_ms(),
        body,
    };
    let plaintext = serde_json::to_string(&sealed).expect("sealed json");
    let ciphertext = channel.encrypt(plaintext.as_bytes()).expect("encrypt");
    let frame = Frame::Sealed {
        seq: now_ms(),
        host_id: host_id.parse().expect("host uuid"),
        device_id: None,
        ciphertext: Base64Bytes(ciphertext),
    };
    socket
        .send(Message::text(serde_json::to_string(&frame).expect("json")))
        .expect("sealed send");
}

fn wait_for_operation_result(
    socket: &mut DeviceSocket,
    channel: &mut SecureChannel,
    operation_id: Uuid,
) -> fintwind_remote_protocol::OperationOutcome {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "operation result never arrived");
        match socket.read() {
            Ok(Message::Text(text)) => {
                let frame: Frame = serde_json::from_str(&text).expect("frame json");
                match frame {
                    Frame::Sealed { ciphertext, .. } => {
                        let plaintext = channel.decrypt(&ciphertext.0).expect("decrypt");
                        let sealed: SealedMessage =
                            serde_json::from_slice(&plaintext).expect("sealed json");
                        if let MessageBody::OperationResult(outcome) = sealed.body {
                            if outcome.operation_id == operation_id {
                                return outcome;
                            }
                        }
                    }
                    Frame::Control {
                        frame: ControlFrame::Ping { sent_at_ms },
                    } => {
                        let _ = socket.send(Message::text(
                            serde_json::to_string(&Frame::Control {
                                frame: ControlFrame::Pong { sent_at_ms },
                            })
                            .expect("json"),
                        ));
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
            Err(error) => panic!("socket error waiting for result: {error}"),
            Ok(_) => {}
        }
    }
}

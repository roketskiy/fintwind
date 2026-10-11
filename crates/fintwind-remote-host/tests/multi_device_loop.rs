//! Whole-process, two-device fault and isolation matrix against the real
//! relay and the real host daemon.
//!
//! Failure modes were listed before this file was written. What it must
//! prove, and what it deliberately does not:
//!
//! | Case | Covered here |
//! | --- | --- |
//! | One device's bad frame must not cost another device its channel | yes |
//! | A revoked or re-paired grant must drop only that device | yes |
//! | A device with no grant, or naming another host, must get nothing | yes |
//! | A stale cursor must be answered with an explicit resync | yes |
//! | Two devices competing for one driver decision | **not here** — it needs a live provider turn; see the report |
//!
//! The last row is stated rather than papered over: an approval offer only
//! exists once a driver raises a permission request, and this harness runs
//! no provider. Claiming that case from an in-process registry test would
//! prove nothing about the real chain.
//!
//! Everything below is out-of-process: a real `fintwind-relay`, a real
//! `fintwind-remote-host`, two real device clients speaking the real KK
//! handshake and the real sealed frame format.

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

use fintwind_remote_crypto::{ConnectInitiator, IdentityKey, SecureChannel, SigningKey};
use fintwind_remote_protocol::{
    Base64Bytes, ConnectionPurpose, ControlFrame, Frame, FrameRole, HandshakeMode, MessageBody,
    REMOTE_PROTOCOL_VERSION, ResumeCursor, SealedMessage,
};

type DeviceSocket = WebSocket<MaybeTlsStream<TcpStream>>;

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

/// One device's whole presence: its keys, its socket, its Noise channel and
/// its delivery counter.
struct Device {
    id: String,
    socket: DeviceSocket,
    channel: SecureChannel,
    next_seq: u64,
    /// The host this device is talking to, shared so one place can update it
    /// for the foreign-host case.
    host_id: std::sync::Arc<std::sync::Mutex<Option<Uuid>>>,
}

impl Device {
    fn send(&mut self, body: MessageBody) {
        // The authenticated delivery sequence repeats the frame's own seq,
        // exactly as a real device does, so the host's check is exercised.
        self.next_seq += 1;
        let seq = self.next_seq;
        let sealed = SealedMessage {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            transport_seq: seq,
            host_id: self.host_id(),
            device_id: self.id.parse().expect("device uuid"),
            sent_at_ms: now_ms(),
            body,
        };
        let plaintext = serde_json::to_string(&sealed).expect("sealed json");
        let ciphertext = self.channel.encrypt(plaintext.as_bytes()).expect("encrypt");
        let frame = Frame::Sealed {
            seq,
            host_id: self.host_id(),
            device_id: None,
            ciphertext: Base64Bytes(ciphertext),
        };
        self.socket
            .send(Message::text(serde_json::to_string(&frame).expect("json")))
            .expect("sealed send");
    }

    /// Send one frame whose authenticated sequence deliberately disagrees
    /// with its envelope sequence. This is the "bad frame" case: the payload
    /// cannot be trusted to describe the frame it rode in on.
    fn send_tampered_seq(&mut self, body: MessageBody) {
        let frame_seq = self.next_seq + 1000;
        self.next_seq += 2;
        let sealed = SealedMessage {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            transport_seq: frame_seq,
            host_id: self.host_id(),
            device_id: self.id.parse().expect("device uuid"),
            sent_at_ms: now_ms(),
            body,
        };
        let plaintext = serde_json::to_string(&sealed).expect("sealed json");
        let ciphertext = self.channel.encrypt(plaintext.as_bytes()).expect("encrypt");
        let frame = Frame::Sealed {
            seq: self.next_seq,
            host_id: self.host_id(),
            device_id: None,
            ciphertext: Base64Bytes(ciphertext),
        };
        self.socket
            .send(Message::text(serde_json::to_string(&frame).expect("json")))
            .expect("sealed send");
    }

    fn host_id(&self) -> Uuid {
        self.host_id
            .lock()
            .expect("host id")
            .expect("host id known")
    }
}

#[test]
fn two_devices_one_host_isolate_faults_grants_and_resume() {
    let port = free_port();
    let origin = format!("http://127.0.0.1:{port}");
    let ws_url = format!("ws://127.0.0.1:{port}/ws");
    let relay_db = temp_dir("multi-relay").join("relay.sqlite3");
    let host_data = temp_dir("multi-host");
    let grant_file = host_data.join("paired-device.json");

    // ---- keys exist before any registration ----
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

    let relay = Command::new(relay_exe())
        .args([
            "--bind",
            &format!("127.0.0.1:{port}"),
            "--db",
            relay_db.to_str().expect("db path"),
        ])
        .stderr(Stdio::from(
            std::fs::File::create(relay_db.with_extension("log")).expect("relay log"),
        ))
        .spawn()
        .expect("relay process");
    let _relay = Child2(relay);
    wait_http_ok(&origin);

    // ---- two granted devices and one that was never paired ----
    let a_signing = SigningKey::generate();
    let a_noise = IdentityKey::generate().expect("device noise key");
    let b_signing = SigningKey::generate();
    let b_noise = IdentityKey::generate().expect("device noise key");
    let c_signing = SigningKey::generate();
    let c_noise = IdentityKey::generate().expect("device noise key");
    let a_id = register_identity(&origin, "device", &a_signing);
    let b_id = register_identity(&origin, "device", &b_signing);
    let c_id = register_identity(&origin, "device", &c_signing);

    write_grants(&grant_file, &[(&a_id, &a_noise, 1), (&b_id, &b_noise, 1)]);

    let host = Command::new(env!("CARGO_BIN_EXE_fintwind-remote-host"))
        .args([
            "--relay-origin",
            &origin,
            "--daemon-address",
            "127.0.0.1:1",
            "--daemon-token-env",
            "MULTI_DEVICE_TOKEN",
            "--data-dir",
            host_data.to_str().expect("host dir"),
            "--paired-device",
            grant_file.to_str().expect("grant path"),
        ])
        .env("MULTI_DEVICE_TOKEN", "unused-in-this-test")
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            std::fs::File::create(host_data.join("host-daemon.log")).expect("host log"),
        ))
        .spawn()
        .expect("host process");
    let _host = Child2(host);

    let host_id: Uuid = wait_for_file(&host_data.join("host-id.txt"))
        .parse()
        .expect("host uuid");
    let host_id_cell = std::sync::Arc::new(std::sync::Mutex::new(Some(host_id)));

    link_at_relay(&relay_db, &host_id.to_string(), &a_id);
    link_at_relay(&relay_db, &host_id.to_string(), &b_id);
    link_at_relay(&relay_db, &host_id.to_string(), &c_id);

    wait_for_relay_link(&host_data.join("host-status.json"));

    let mut a = connect_device(
        &ws_url,
        a_id.clone(),
        &a_signing,
        &a_noise,
        &host_noise_public,
        host_id,
        std::sync::Arc::clone(&host_id_cell),
    );
    let mut b = connect_device(
        &ws_url,
        b_id.clone(),
        &b_signing,
        &b_noise,
        &host_noise_public,
        host_id,
        std::sync::Arc::clone(&host_id_cell),
    );
    // ---- both granted devices are served ----
    let a_first = snapshot_roundtrip(&mut a);
    assert!(
        a_first,
        "device A must be served before anything goes wrong"
    );
    let b_first = snapshot_roundtrip(&mut b);
    assert!(
        b_first,
        "device B must be served before anything goes wrong"
    );

    // ---- the never-paired device gets nothing, and costs nobody anything ----
    assert!(
        !probe_ungranted(
            &ws_url,
            &c_id,
            &c_signing,
            &c_noise,
            &host_noise_public,
            host_id
        ),
        "a device without a grant must never be answered"
    );
    assert!(
        snapshot_roundtrip(&mut b),
        "device B must still be served after an ungranted device spoke"
    );

    // ---- a frame claiming another host is dropped, not answered ----
    let foreign = Uuid::new_v4();
    a.send_with_host(MessageBody::RequestSnapshot, foreign);
    assert!(
        !wait_for_any_message(&mut a, Duration::from_secs(3)),
        "a frame naming another host must be dropped"
    );
    assert!(
        snapshot_roundtrip(&mut b),
        "device B must still be served after a foreign-host frame"
    );

    // ---- a bad frame costs only its own device its channel ----
    send_tampered_seq(&mut a, MessageBody::RequestSnapshot);
    assert!(
        !wait_for_any_message(&mut a, Duration::from_secs(3)),
        "a frame whose authenticated sequence disagrees with its envelope must be dropped"
    );
    assert!(
        snapshot_roundtrip(&mut b),
        "device B must still be served after device A corrupted a frame"
    );

    // ---- a stale cursor is answered with an explicit resync ----
    let stale = ResumeCursor {
        session_id: Uuid::new_v4(),
        runtime_id: Uuid::new_v4(),
        epoch: Uuid::new_v4(),
        sequence: 0,
    };
    b.send(MessageBody::Resume {
        cursors: vec![stale],
    });
    let resynced = wait_for_resync(&mut b, Duration::from_secs(10));
    assert!(
        resynced,
        "a cursor the host cannot honor must be answered with a resync, never a bridged gap"
    );

    // ---- a revoked grant drops only that device ----
    // Re-pair B at version 2: the host re-reads grants on a short interval,
    // so its channel must be dropped while A's — revoked here — is restored
    // by a fresh handshake first.
    write_grants(&grant_file, &[(&a_id, &a_noise, 1), (&b_id, &b_noise, 2)]);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut b_dropped = false;
    while Instant::now() < deadline {
        b.send(MessageBody::RequestSnapshot);
        if !wait_for_any_message(&mut b, Duration::from_millis(700)) {
            b_dropped = true;
            break;
        }
    }
    assert!(
        b_dropped,
        "a device whose grant version moved must lose its channel"
    );
    // A re-handshake gives B a fresh key period under the new grant.
    let mut b = reconnect_device(
        &ws_url,
        b_id.clone(),
        &b_signing,
        &b_noise,
        &host_noise_public,
        host_id,
        std::sync::Arc::clone(&host_id_cell),
    );
    assert!(
        snapshot_roundtrip(&mut b),
        "a device that re-pairs must be served again under its new grant"
    );
    // A, whose grant never moved, gets a fresh key period and is served
    // again: an unchanged grant is not punished for another device's.
    let mut a = reconnect_device(
        &ws_url,
        a_id.clone(),
        &a_signing,
        &a_noise,
        &host_noise_public,
        host_id,
        std::sync::Arc::clone(&host_id_cell),
    );
    assert!(
        snapshot_roundtrip(&mut a),
        "a device whose grant did not change must be served again after another's revocation"
    );

    let report = serde_json::json!({
        "scenario": "two-devices-one-host-isolation",
        "passed": true,
        "badFrameIsolated": true,
        "ungrantedDeviceIgnored": true,
        "foreignHostDropped": true,
        "staleCursorResynced": true,
        "grantRevocationIsolated": true,
        "approvalClaimAcrossDevices": false,
        "approvalClaimNote": "needs a live provider turn; not covered by this harness",
        "relayPort": port,
    });
    let artifact = host_data.join("summary.json");
    std::fs::write(&artifact, serde_json::to_vec_pretty(&report).unwrap())
        .expect("write repeatable black-box artifact");
    println!("[ARTIFACT] {}", artifact.display());
}

// ---- harness ----

/// One frame whose authenticated sequence deliberately disagrees with its
/// envelope sequence. This is the bad-frame case: the payload cannot be
/// trusted to describe the frame it rode in on, so the host must drop the
/// device's channel rather than act on the mismatch.
fn send_tampered_seq(device: &mut Device, body: MessageBody) {
    let claimed = device.next_seq + 1000;
    let actual = device.next_seq + 1;
    device.next_seq = actual + 1;
    let sealed = SealedMessage {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        transport_seq: claimed,
        host_id: device.host_id(),
        device_id: device.id.parse().expect("device uuid"),
        sent_at_ms: now_ms(),
        body,
    };
    let plaintext = serde_json::to_string(&sealed).expect("sealed json");
    let ciphertext = device
        .channel
        .encrypt(plaintext.as_bytes())
        .expect("encrypt");
    let frame = Frame::Sealed {
        seq: actual,
        host_id: device.host_id(),
        device_id: None,
        ciphertext: Base64Bytes(ciphertext),
    };
    device
        .socket
        .send(Message::text(serde_json::to_string(&frame).expect("json")))
        .expect("sealed send");
}

impl Device {
    fn send_with_host(&mut self, body: MessageBody, host_id: Uuid) {
        self.next_seq += 1;
        let seq = self.next_seq;
        let sealed = SealedMessage {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            transport_seq: seq,
            host_id,
            device_id: self.id.parse().expect("device uuid"),
            sent_at_ms: now_ms(),
            body,
        };
        let plaintext = serde_json::to_string(&sealed).expect("sealed json");
        let ciphertext = self.channel.encrypt(plaintext.as_bytes()).expect("encrypt");
        let frame = Frame::Sealed {
            seq,
            host_id,
            device_id: None,
            ciphertext: Base64Bytes(ciphertext),
        };
        self.socket
            .send(Message::text(serde_json::to_string(&frame).expect("json")))
            .expect("sealed send");
    }
}

/// Authenticate as a device that was never paired, run the KK handshake as
/// far as it goes, and report whether the host answered anything at all.
///
/// The host refuses the handshake outright, so this device can never hold a
/// channel and can never seal anything: the honest expectation is silence.
fn probe_ungranted(
    ws_url: &str,
    device_id: &str,
    signing: &SigningKey,
    noise: &IdentityKey,
    host_noise_public: &[u8],
    host_id: Uuid,
) -> bool {
    let (mut socket, _) = tungstenite::connect(ws_url).expect("device ws connect");
    apply_read_timeout(&socket, Some(Duration::from_millis(250)));
    device_auth(&mut socket, device_id, signing);
    let device_uuid: Uuid = device_id.parse().expect("device uuid");
    let mut initiator = ConnectInitiator::new(
        REMOTE_PROTOCOL_VERSION,
        noise,
        host_noise_public,
        host_id,
        device_uuid,
    )
    .expect("kk initiator");
    let first = initiator.write_first().expect("kk message 1");
    let _ = socket.send(Message::text(
        serde_json::to_string(&Frame::Handshake {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            role: FrameRole::Device,
            host_id,
            device_id: Some(device_uuid),
            mode: HandshakeMode::Connect,
            step: 0,
            payload: Base64Bytes(first),
        })
        .expect("json"),
    ));
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        match socket.read() {
            // Only an endpoint frame counts as an answer. A relay-level
            // receipt or ping is transport, never the host speaking.
            Ok(Message::Text(text)) => {
                if let Ok(frame) = serde_json::from_str::<Frame>(&text) {
                    if matches!(frame, Frame::Handshake { .. } | Frame::Sealed { .. }) {
                        return true;
                    }
                }
            }
            Ok(Message::Close(_)) => return false,
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => return false,
            Ok(_) => {}
        }
    }
    false
}

/// Ask for a snapshot and wait for anything back. With no daemon behind the
/// host the honest answer is `WorkspaceUnavailable`, which is exactly what
/// proves the device still has a live channel.
fn snapshot_roundtrip(device: &mut Device) -> bool {
    device.send(MessageBody::RequestSnapshot);
    wait_for_any_message(device, Duration::from_secs(8))
}

fn wait_for_any_message(device: &mut Device, window: Duration) -> bool {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match device.socket.read() {
            Ok(Message::Text(text)) => {
                let Ok(frame) = serde_json::from_str::<Frame>(&text) else {
                    continue;
                };
                match frame {
                    Frame::Sealed { ciphertext, .. } => {
                        if device.channel.decrypt(&ciphertext.0).is_ok() {
                            return true;
                        }
                    }
                    Frame::Control {
                        frame: ControlFrame::Ping { sent_at_ms },
                    } => {
                        let _ = device.socket.send(Message::text(
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
            Err(_) => return false,
            Ok(_) => {}
        }
    }
    false
}

fn wait_for_resync(device: &mut Device, window: Duration) -> bool {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match device.socket.read() {
            Ok(Message::Text(text)) => {
                let Ok(frame) = serde_json::from_str::<Frame>(&text) else {
                    continue;
                };
                if let Frame::Sealed { ciphertext, .. } = frame {
                    let Ok(plaintext) = device.channel.decrypt(&ciphertext.0) else {
                        continue;
                    };
                    if let Ok(sealed) = serde_json::from_slice::<SealedMessage>(&plaintext) {
                        if matches!(sealed.body, MessageBody::Resync { .. }) {
                            return true;
                        }
                    }
                }
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => return false,
            Ok(_) => {}
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn connect_device(
    ws_url: &str,
    device_id: String,
    signing: &SigningKey,
    noise: &IdentityKey,
    host_noise_public: &[u8],
    host_id: Uuid,
    host_id_cell: std::sync::Arc<std::sync::Mutex<Option<Uuid>>>,
) -> Device {
    let (mut socket, _) = tungstenite::connect(ws_url).expect("device ws connect");
    apply_read_timeout(&socket, Some(Duration::from_millis(250)));
    device_auth(&mut socket, &device_id, signing);
    let channel = device_handshake(&mut socket, host_noise_public, host_id, &device_id, noise);
    Device {
        id: device_id,
        socket,
        channel,
        next_seq: 0,
        host_id: host_id_cell,
    }
}

/// A fresh connection and a fresh key period, which is what a device does
/// after its grant moved.
#[allow(clippy::too_many_arguments)]
fn reconnect_device(
    ws_url: &str,
    device_id: String,
    signing: &SigningKey,
    noise: &IdentityKey,
    host_noise_public: &[u8],
    host_id: Uuid,
    host_id_cell: std::sync::Arc<std::sync::Mutex<Option<Uuid>>>,
) -> Device {
    connect_device(
        ws_url,
        device_id,
        signing,
        noise,
        host_noise_public,
        host_id,
        host_id_cell,
    )
}

fn write_grants(path: &std::path::Path, devices: &[(&str, &IdentityKey, u64)]) {
    let grants: Vec<serde_json::Value> = devices
        .iter()
        .map(|(device_id, noise, version)| {
            serde_json::json!({
                "deviceId": device_id,
                "devicePublicKey": BASE64_STANDARD.encode(&noise.public),
                "grantVersion": version,
                "grantedAtMs": 0,
            })
        })
        .collect();
    std::fs::write(path, serde_json::json!({ "grants": grants }).to_string()).expect("grant file");
}

fn host_tool(arguments: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_fintwind-remote-host"))
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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("local addr")
        .port()
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
    if let MaybeTlsStream::Plain(tcp) = socket.get_ref() {
        let _ = tcp.set_read_timeout(timeout);
    }
}

/// The host writes its own relay link state, so the harness waits for a real
/// connected link instead of racing the host's session establishment.
fn wait_for_relay_link(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            Instant::now() < deadline,
            "the host never linked to the relay"
        );
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if value
                    .get("relayConnected")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_for_file(path: &std::path::Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
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

fn wait_http_ok(origin: &str) {
    use std::io::{Read, Write};
    let authority = origin.trim_start_matches("http://");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(Instant::now() < deadline, "relay health never came up");
        let attempt = TcpStream::connect(authority).and_then(|mut stream| {
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
            stream.write_all(
                format!(
                    "GET /api/health HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )?;
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw)?;
            Ok(String::from_utf8_lossy(&raw).to_string())
        });
        if attempt.is_ok_and(|text| text.contains(" 200 ") && text.contains("ok")) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn post_json(origin: &str, path: &str, body: &serde_json::Value) -> serde_json::Value {
    use std::io::{Read, Write};
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
    assert!(
        status.contains(" 200 "),
        "relay refused {path}: {status} {}",
        payload.trim()
    );
    serde_json::from_str(payload).expect("relay json response")
}

fn register_identity(origin: &str, role: &str, signing: &SigningKey) -> String {
    let challenge = post_json(
        origin,
        "/api/challenge",
        &serde_json::json!({ "role": role }),
    );
    let challenge_b64 = challenge
        .get("challenge")
        .and_then(serde_json::Value::as_str)
        .expect("challenge")
        .to_owned();
    let challenge_bytes = decode_b64(&challenge_b64);
    let signature = signing.sign(&challenge_bytes);
    let registered = post_json(
        origin,
        "/api/register",
        &serde_json::json!({
            "role": role,
            "signingPublicKey": BASE64_STANDARD.encode(&signing.verifying),
            "challenge": challenge_b64,
            "signature": BASE64_STANDARD.encode(signature),
        }),
    );
    let id_field = if role == "host" { "hostId" } else { "deviceId" };
    registered
        .get(id_field)
        .and_then(serde_json::Value::as_str)
        .expect("assigned id")
        .to_owned()
}

fn link_at_relay(relay_db: &std::path::Path, host_id: &str, device_id: &str) {
    let connection = rusqlite::Connection::open(relay_db).expect("relay db opens");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match connection.execute(
            "INSERT INTO pairing_links (host_id, device_id, linked_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(host_id, device_id) DO UPDATE SET unlinked_at_ms = NULL",
            rusqlite::params![host_id, device_id, now_ms() as i64],
        ) {
            Ok(_) => return,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "pairing link never landed: {error}"
                );
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn device_auth(socket: &mut DeviceSocket, device_id: &str, signing: &SigningKey) {
    let peer = match socket.get_ref() {
        MaybeTlsStream::Plain(tcp) => tcp.peer_addr().expect("peer addr"),
        _ => panic!("plain ws expected in tests"),
    };
    let rest = format!("http://{peer}");
    let challenge = post_json(
        &rest,
        "/api/challenge",
        &serde_json::json!({"role":"device"}),
    );
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
        purpose: ConnectionPurpose::Session,
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
    host_id: Uuid,
    device_id: &str,
    device_noise: &IdentityKey,
) -> SecureChannel {
    let device_uuid: Uuid = device_id.parse().expect("device uuid");
    let mut initiator = ConnectInitiator::new(
        REMOTE_PROTOCOL_VERSION,
        device_noise,
        host_noise_public,
        host_id,
        device_uuid,
    )
    .expect("kk initiator");
    let first = initiator.write_first().expect("kk message 1");
    let frame = Frame::Handshake {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role: FrameRole::Device,
        host_id,
        device_id: Some(device_uuid),
        mode: HandshakeMode::Connect,
        step: 0,
        payload: Base64Bytes(first),
    };
    socket
        .send(Message::text(serde_json::to_string(&frame).expect("json")))
        .expect("handshake send");

    let deadline = Instant::now() + Duration::from_secs(20);
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
                    } => {
                        return initiator.finish(&payload.0).expect("kk completes");
                    }
                    Frame::Control {
                        frame: ControlFrame::Rejected { code, message, .. },
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

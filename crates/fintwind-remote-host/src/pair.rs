//! The pairing session: invite, XX handshake, human confirmation, grant.
//!
//! Flow (architecture document, 配对流程, adapted to the no-account model):
//! the host mints a one-time nonce, arms it at the relay (signed), shows
//! the invite, then waits for a device to open a `Pairing` handshake. XX
//! binds every invite field into the Noise transcript; after the handshake
//! both ends derive the same six-digit check code and the user compares.
//! Only after an explicit local confirmation does the host write the grant
//! and declare the link at the relay.
//!
//! Confirmation runs on stdin for now; the desktop dialog replaces the
//! prompt without touching this state machine.

use std::io::{BufRead, Write as _};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use tungstenite::Message;
use uuid::Uuid;

use fintwind_remote_crypto::{pairing_code, PairingBinding, PairingResponder, PairingResponderFinal};
use fintwind_remote_protocol::{
    Base64Bytes, ControlFrame, Frame, FrameRole, HandshakeMode, MessageBody, PairingInvite,
    REMOTE_PROTOCOL_VERSION, SealedMessage,
};

use crate::config::HostConfig;
use crate::grants::{DeviceGrant, GrantFile, GRANT_SCOPE_TEXT};
use crate::identity;

/// How long an invite lives, shown to the user and enforced twice: the
/// relay's stored nonce carries the same TTL, and the host refuses to
/// confirm past expiry.
const INVITE_TTL: Duration = Duration::from_secs(5 * 60);

/// Mint an invite, arm it at the relay, and print it. Returns the payload
/// the UI layer will render as a QR code.
fn arm_invite(config: &HostConfig, host: &identity::HostIdentity) -> anyhow::Result<PairingInvite> {
    let mut nonce = [0u8; 32];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut nonce);
    let expires_at_ms = now_ms() + INVITE_TTL.as_millis() as u64;

    // Arm the nonce at the relay: a fresh challenge proves freshness, and
    // the offer payload `host_id || nonce` is signed with the host's
    // registered key. The relay consumes both before storing the nonce.
    let (challenge_b64, challenge_bytes) = identity::fetch_challenge(&config.relay_origin, "host")?;
    let mut signed = host.host_id.as_bytes().to_vec();
    signed.extend_from_slice(&nonce);
    let signature = host.signing.sign(&signed);
    identity::post_json(
        &format!("{}/api/pairing/offer", config.relay_origin.trim_end_matches('/')),
        &serde_json::json!({
            "hostId": host.host_id,
            "nonce": BASE64_STANDARD.encode(nonce),
            "challenge": challenge_b64,
            "signature": BASE64_STANDARD.encode(signature),
        }),
    )?;
    let _ = challenge_bytes;

    Ok(PairingInvite {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        relay_origin: config.relay_origin.trim_end_matches('/').to_owned(),
        host_id: host.host_id.parse().expect("stored host id is a uuid"),
        host_public_key: BASE64_STANDARD.encode(&host.noise.public),
        nonce: BASE64_STANDARD.encode(nonce),
        expires_at_ms,
    })
}

fn binding_from(invite: &PairingInvite) -> PairingBinding {
    PairingBinding {
        protocol_version: invite.protocol_version,
        host_id: invite.host_id,
        nonce: decode_b64(&invite.nonce),
        expires_at_ms: invite.expires_at_ms,
        relay_origin: invite.relay_origin.clone(),
    }
}

fn decode_b64(value: &str) -> Vec<u8> {
    BASE64_STANDARD.decode(value).expect("invite base64")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

type HostSocket = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

/// Short read timeouts drive the poll loop; wss splits the stream, so
/// match the variants instead of assuming plain TCP.
fn apply_read_timeout(socket: &HostSocket, timeout: Option<Duration>) {
    use tungstenite::stream::MaybeTlsStream;
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

/// The full interactive pairing session. Prints progress to stdout and
/// reads the confirmation from stdin; exits non-zero on refusal.
pub fn run_pair_session(config: &HostConfig) -> anyhow::Result<()> {
    let mut host = identity::load_or_create(&config.data_dir)?;
    if host.host_id.is_empty() {
        // First contact: mint the routing identity on the spot. Pairing is
        // exactly the moment a fresh host meets the relay.
        let host_id = identity::register_with_relay(&config.relay_origin, &host.signing)?;
        identity::store_host_id(&config.data_dir, &host_id)?;
        host.host_id = host_id;
    }
    let invite = arm_invite(config, &host)?;

    println!("Scan this pairing invite with the device:");
    println!(
        "{}",
        serde_json::to_string(&invite).expect("invite serializes")
    );
    println!(
        "Host fingerprint: {}",
        host.noise.fingerprint()
    );
    println!("The invite expires at {}.", invite.expires_at_ms);
    println!("Waiting for a device...\n");

    let mut socket = connect_and_auth(config, &host)?;
    apply_read_timeout(&socket, Some(Duration::from_millis(250)));

    let binding = binding_from(&invite);
    let deadline = Instant::now() + INVITE_TTL;
    let mut responder: Option<PairingResponderFinal> = None;

    // ---- pairing handshake, then the confirmation gate ----
    let (device_id, device_name, device_public, code, mut channel) = loop {
        if Instant::now() > deadline {
            anyhow::bail!("no device arrived before the invite expired");
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let Ok(frame) = serde_json::from_str::<Frame>(&text) else {
                    anyhow::bail!("relay sent an unparsable frame");
                };
                match frame {
                    Frame::Handshake {
                        mode: HandshakeMode::Pairing,
                        step: 0,
                        payload,
                        ..
                    } => {
                        let mut state = PairingResponder::new(&binding, &host.noise)
                            .expect("responder builds");
                        // read_first verifies the prologue binding: any
                        // tampered invite field fails right here.
                        let _ = state.read_first(&payload.0)?;
                        let (reply, final_step) = state.write_reply(b"")?;
                        send_frame(
                            &mut socket,
                            Frame::Handshake {
                                protocol_version: REMOTE_PROTOCOL_VERSION,
                                role: FrameRole::Host,
                                host_id: invite.host_id,
                                device_id: None,
                                mode: HandshakeMode::Pairing,
                                step: 1,
                                payload: Base64Bytes(reply),
                            },
                        )?;
                        responder = Some(final_step);
                    }
                    Frame::Handshake {
                        mode: HandshakeMode::Pairing,
                        step: 2,
                        payload,
                        ..
                    } => {
                        let final_step = responder.take().expect("pairing step 2 after step 0");
                        let (hello_bytes, channel) = final_step.finish(&payload.0)?;
                        let sealed: SealedMessage = serde_json::from_slice(&hello_bytes)?;
                        let MessageBody::PairingHello {
                            device_id,
                            device_name,
                        } = sealed.body
                        else {
                            anyhow::bail!("pairing channel opened with a non-hello message");
                        };
                        let device_public: Vec<u8> = channel
                            .remote_static()
                            .expect("XX binds the peer static key")
                            .to_vec();
                        let code =
                            pairing_code(&host.noise.public, &device_public, &binding);
                        break (device_id, device_name, device_public, code, channel);
                    }
                    Frame::Control {
                        frame: ControlFrame::Rejected { code, message },
                    } => anyhow::bail!("relay refused the pairing: {code:?} {message}"),
                    Frame::Control {
                        frame: ControlFrame::Ping { sent_at_ms },
                    } => {
                        send_frame(
                            &mut socket,
                            Frame::Control {
                                frame: ControlFrame::Pong { sent_at_ms },
                            },
                        )?;
                    }
                    _ => {}
                }
            }
            Ok(Message::Close(_)) => anyhow::bail!("relay closed the connection"),
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
    };

    // ---- the human gate ----
    let shown_name = device_name.unwrap_or_else(|| "unnamed device".to_owned());
    println!("Device requesting pairing:");
    println!("  name:        {shown_name}");
    println!("  id:          {device_id}");
    println!(
        "  fingerprint: {}",
        fintwind_remote_crypto::fingerprint(device_public.as_slice())
    );
    println!("  check code:  {code}");
    println!("\nThis device will be granted: {GRANT_SCOPE_TEXT}");
    print!("Confirm pairing? [y/N] ");
    std::io::stdout().flush()?;
    let accepted = read_confirmation(deadline)?;

    // Persist the grant BEFORE notifying the device: once the decision is
    // out, a freshly paired device may connect immediately, and the KK
    // handshake needs this file already on disk.
    if accepted {
        let grant = DeviceGrant {
            device_id: device_id.to_string(),
            device_public_key: BASE64_STANDARD.encode(&device_public),
            grant_version: now_ms(),
            granted_at_ms: now_ms(),
            revoked_at_ms: None,
        };
        let mut file =
            GrantFile::load(&config.paired_device_file)?.unwrap_or(GrantFile { grants: vec![] });
        file.upsert(grant);
        std::fs::write(
            &config.paired_device_file,
            serde_json::to_string_pretty(&file)?,
        )?;
        log::info!(target: "audit", "device granted; device={device_id}");
    }

    let decision = MessageBody::PairingDecision {
        accepted,
        reason: if accepted { None } else { Some("rejected on the host".to_owned()) },
    };
    send_sealed(&mut socket, &mut channel, &invite.host_id, device_id, decision)?;
    if !accepted {
        println!("Pairing rejected.");
        return Ok(());
    }

    // Declare the link at the relay, signed like the offer was.
    let mut signed = invite.host_id.as_bytes().to_vec();
    signed.extend_from_slice(device_id.as_bytes());
    signed.extend_from_slice(&device_public);
    signed.extend_from_slice(&decode_b64(&invite.nonce));
    let link_signature = host.signing.sign(&signed);
    send_frame(
        &mut socket,
        Frame::LinkDevice {
            host_id: invite.host_id,
            device_id,
            device_public_key: Base64Bytes(device_public.clone()),
            nonce: Base64Bytes(decode_b64(&invite.nonce)),
            signature: Base64Bytes(link_signature),
        },
    )?;

    wait_for_control(&mut socket, &invite.host_id, device_id, true)?;
    println!("[OK] device {device_id} is paired and linked.");
    Ok(())
}

/// Revoke a device: the local grant is marked revoked and the relay link
/// is torn down with a signed declaration. Both ends fail closed while
/// either revocation is still in flight.
pub fn run_revoke_device(config: &HostConfig, device_id: &str) -> anyhow::Result<()> {
    let host = identity::load_or_create(&config.data_dir)?;
    if host.host_id.is_empty() {
        anyhow::bail!("this host is not registered yet");
    }
    let mut file = GrantFile::load(&config.paired_device_file)?.unwrap_or(GrantFile { grants: vec![] });
    if !file.revoke(device_id, now_ms()) {
        anyhow::bail!("no active grant for device {device_id}");
    }
    std::fs::write(
        &config.paired_device_file,
        serde_json::to_string_pretty(&file)?,
    )?;

    // Arm a nonce for the unlink declaration, then send it.
    let mut nonce = [0u8; 32];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut nonce);
    let (challenge_bytes, _) = identity::fetch_challenge(&config.relay_origin, "host")?;
    let mut signed = host.host_id.as_bytes().to_vec();
    signed.extend_from_slice(&nonce);
    let offer_signature = host.signing.sign(&signed);
    identity::post_json(
        &format!("{}/api/pairing/offer", config.relay_origin.trim_end_matches('/')),
        &serde_json::json!({
            "hostId": host.host_id,
            "nonce": BASE64_STANDARD.encode(nonce),
            "challenge": BASE64_STANDARD.encode(challenge_bytes),
            "signature": BASE64_STANDARD.encode(offer_signature),
        }),
    )?;

    let mut socket = connect_and_auth(config, &host)?;
    apply_read_timeout(&socket, Some(Duration::from_millis(250)));
    let device_uuid: Uuid = device_id.parse().map_err(|error| {
        anyhow::anyhow!("device id is not a uuid: {error}")
    })?;
    let mut signed = host.host_id.as_bytes().to_vec();
    signed.extend_from_slice(device_id.as_bytes());
    signed.extend_from_slice(&nonce);
    let signature = host.signing.sign(&signed);
    send_frame(
        &mut socket,
        Frame::UnlinkDevice {
            host_id: host.host_id.parse().expect("stored host id is a uuid"),
            device_id: device_uuid,
            nonce: Base64Bytes(nonce.to_vec()),
            signature: Base64Bytes(signature),
        },
    )?;
    wait_for_control(&mut socket, &host.host_id.parse().expect("uuid"), device_uuid, false)?;
    println!("[OK] device {device_id} is revoked and unlinked.");
    Ok(())
}

/// Read y/n from stdin with the invite deadline enforced.
fn read_confirmation(deadline: Instant) -> anyhow::Result<bool> {
    let stdin = std::io::stdin();
    loop {
        if Instant::now() > deadline {
            anyhow::bail!("confirmation window closed with the invite");
        }
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => anyhow::bail!("stdin closed before confirmation"),
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" | "" => return Ok(false),
            _ => {
                print!("Please answer y or n: ");
                std::io::stdout().flush()?;
            }
        }
    }
}

fn send_frame(socket: &mut HostSocket, frame: Frame) -> anyhow::Result<()> {
    socket
        .send(Message::text(serde_json::to_string(&frame)?))
        .map_err(|error| anyhow::anyhow!("relay send failed: {error}"))
}

fn send_sealed(
    socket: &mut HostSocket,
    channel: &mut fintwind_remote_crypto::SecureChannel,
    host_id: &Uuid,
    device_id: Uuid,
    body: MessageBody,
) -> anyhow::Result<()> {
    let sealed = SealedMessage {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        host_id: *host_id,
        device_id,
        sent_at_ms: now_ms(),
        body,
    };
    let plaintext = serde_json::to_string(&sealed)?;
    let ciphertext = channel.encrypt(plaintext.as_bytes())?;
    send_frame(
        socket,
        Frame::Sealed {
            seq: now_ms(),
            host_id: *host_id,
            device_id: Some(device_id),
            ciphertext: Base64Bytes(ciphertext),
        },
    )
}

/// Wait for the relay's Linked/Unlinked receipt for one device.
fn wait_for_control(
    socket: &mut HostSocket,
    host_id: &Uuid,
    device_id: Uuid,
    linked: bool,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if Instant::now() > deadline {
            anyhow::bail!("relay never confirmed the link change");
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let Ok(frame) = serde_json::from_str::<Frame>(&text) else {
                    continue;
                };
                match frame {
                    Frame::Control {
                        frame:
                            ControlFrame::Linked { device_id: got }
                            | ControlFrame::Unlinked { device_id: got },
                    } if got == device_id => return Ok(()),
                    Frame::Control {
                        frame: ControlFrame::Rejected { code, message },
                    } => anyhow::bail!("relay refused the link change: {code:?} {message}"),
                    Frame::Control {
                        frame: ControlFrame::Ping { sent_at_ms },
                    } => {
                        send_frame(
                            socket,
                            Frame::Control {
                                frame: ControlFrame::Pong { sent_at_ms },
                            },
                        )?;
                    }
                    _ => {}
                }
            }
            Ok(Message::Close(_)) => anyhow::bail!("relay closed the connection"),
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
    }
}

/// Connect to the relay and authenticate as the host (same signed
/// challenge flow the daemon session uses).
fn connect_and_auth(config: &HostConfig, host: &identity::HostIdentity) -> anyhow::Result<HostSocket> {
    let ws_url = format!(
        "{}/ws",
        config
            .relay_origin
            .trim_end_matches('/')
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1)
    );
    let (mut socket, _) = tungstenite::connect(&ws_url)?;
    let (challenge_b64, challenge_bytes) = identity::fetch_challenge(&config.relay_origin, "host")?;
    let signature = host.signing.sign(&challenge_bytes);
    let auth = Frame::Auth {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        role: FrameRole::Host,
        host_id: Some(host.host_id.parse().expect("stored host id is a uuid")),
        device_id: None,
        challenge: Base64Bytes(challenge_bytes),
        signature: Base64Bytes(signature),
    };
    send_frame(&mut socket, auth)?;
    let _ = challenge_b64;

    // The relay answers auth with a RelayAck; nothing else may precede it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() > deadline {
            anyhow::bail!("relay never acknowledged the host auth");
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let frame: Frame = serde_json::from_str(&text)?;
                match frame {
                    Frame::Control {
                        frame: ControlFrame::RelayAck { .. },
                    } => return Ok(socket),
                    Frame::Control {
                        frame: ControlFrame::Rejected { code, message },
                    } => anyhow::bail!("relay refused host auth: {code:?} {message}"),
                    _ => anyhow::bail!("unexpected frame before auth ack"),
                }
            }
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!("unexpected non-text frame before auth ack"),
        }
    }
}

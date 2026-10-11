//! The pairing session: invite, XX handshake, human confirmation, grant.
//!
//! Flow (architecture document, 配对流程, adapted to the no-account model):
//! the host mints a one-time nonce, arms it at the relay (signed), shows
//! the invite, then waits for a device to open a `Pairing` handshake. XX
//! binds every invite field into the Noise transcript, and the invite
//! itself travels from the desktop screen to the phone's camera, so the
//! channel is trusted without a manual comparison step. Only after an
//! explicit local confirmation does the host write the grant
//! and declare the link at the relay.
//!
//! Confirmation runs on stdin for now; the desktop dialog replaces the
//! prompt without touching this state machine.

use std::io::{BufRead, Write as _};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use tungstenite::Message;
use uuid::Uuid;

use fintwind_remote_crypto::{
    PairingBinding, PairingResponder, PairingResponderFinal, fingerprint as device_fingerprint,
    pairing_code,
};
use fintwind_remote_protocol::{
    Base64Bytes, ConnectionPurpose, ControlFrame, Frame, FrameRole, HandshakeMode, MessageBody,
    PairingInvite, REMOTE_PROTOCOL_VERSION, SealedMessage,
};

use crate::config::HostConfig;
use crate::grants::{DeviceGrant, GRANT_SCOPE_TEXT, GrantFile};
use crate::identity;

/// How long an invite lives, shown to the user and enforced twice: the
/// relay's stored nonce carries the same TTL, and the host refuses to
/// confirm past expiry.
const INVITE_TTL: Duration = Duration::from_secs(5 * 60);

/// Where the control PWA lives — the origin the pairing QR points at.
///
/// The QR never points at the relay: the relay is a ciphertext forwarder
/// with no UI, and the phone needs the PWA to turn a scanned invite into a
/// paired device. The invite still binds `relay_origin` (where the
/// handshake runs); this origin only says where the invite is opened.
///
/// Cloudflare Pages serves the PWA. The custom `remote.fintwind.xyz`
/// domain is not active yet, so the pages.dev origin is the shipped
/// default.
pub const DEFAULT_PWA_ORIGIN: &str = "https://fintwind-remote.pages.dev";

/// Override for a locally hosted or test PWA. Must be an absolute http(s)
/// origin with no path, query, fragment, or userinfo; a plain-http origin
/// is accepted only for a loopback host, so the override can never put a
/// production invite on the wire unprotected. An unusable value is ignored
/// (with a warning) rather than silently producing a wrong QR.
pub const PWA_ORIGIN_ENV: &str = "FINTWIND_PWA_ORIGIN";

/// The PWA origin to build pairing URLs with: the environment override
/// when it is a usable origin, the shipped default otherwise.
pub fn pwa_origin() -> String {
    match std::env::var(PWA_ORIGIN_ENV) {
        Ok(value) => match validated_origin(&value) {
            Some(origin) => origin,
            None => {
                log::warn!(
                    target: "audit",
                    "ignoring {PWA_ORIGIN_ENV}: expected an http(s) origin without a path"
                );
                DEFAULT_PWA_ORIGIN.to_owned()
            }
        },
        Err(_) => DEFAULT_PWA_ORIGIN.to_owned(),
    }
}

/// Normalize a candidate origin, or reject it. Loopback plain http is a
/// test affordance; every other plain-http origin is refused.
fn validated_origin(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    let (scheme, rest) = trimmed.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if rest.is_empty() || rest.contains(['/', '?', '#', '@']) {
        return None;
    }
    let host = rest.split(':').next().unwrap_or(rest);
    let loopback = matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]");
    match scheme.as_str() {
        "https" => {}
        "http" if loopback => {}
        _ => return None,
    }
    Some(format!("{scheme}://{rest}"))
}

/// The URL the QR encodes: the PWA's pair route with the invite in the
/// fragment. The fragment never reaches any server (browsers do not send
/// it), so the invite — nonce included — stays out of every access log.
pub fn pairing_url(invite: &PairingInvite) -> String {
    use base64::Engine as _;
    let invite_json = serde_json::to_string(invite).unwrap_or_default();
    let fragment = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(invite_json.as_bytes());
    format!("{}/pair#{fragment}", pwa_origin().trim_end_matches('/'))
}

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
        &format!(
            "{}/api/pairing/offer",
            config.relay_origin.trim_end_matches('/')
        ),
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

/// What a pairing session reports to whoever drives it (the CLI prints
/// these lines; the desktop app renders them as UI state).
#[derive(Clone, Debug)]
pub enum PairingEvent {
    /// The invite is armed at the relay and ready to display.
    InviteReady(PairingInvite),
    /// A device completed the XX handshake; the user must decide. The
    /// session blocks until it receives the decision on the confirmation
    /// channel.
    ///
    /// `fingerprint` and `check_code` are the human verification pair:
    /// the fingerprint names the exact static key the handshake bound
    /// (not the name the device self-reported), and the check code is the
    /// six digits both ends derive from the same two keys and the same
    /// invite. A driver that cannot show both must not offer a way to
    /// accept.
    DevicePresented {
        device_id: Uuid,
        device_name: Option<String>,
        fingerprint: String,
        check_code: String,
    },
    /// The link is live at the relay; pairing is complete.
    Linked { device_id: Uuid },
    /// The session ended without a pairing (refusal, expiry, error).
    Failed(String),
}

/// How a revocation ended. The local grant and the relay link are two
/// separate actions with two separate failure modes (architecture
/// document, 撤销语义): the local revocation is the reliable boundary, and
/// an unconfirmed relay side is reported as pending — never as done.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RevokeOutcome {
    /// The local grant is revoked and the relay confirmed the unlink.
    Revoked,
    /// The local grant is revoked; the relay unlink did not confirm. The
    /// device is already cut off from this host; the relay-side state is
    /// unknown and shown as such.
    RelayPending { reason: String },
}

/// The full pairing session, event-driven. Blocks the calling thread
/// until the pairing completes or fails; reports progress on `events`
/// and waits for exactly one decision on `confirm` when a device shows
/// up. `confirm` receiving `false` — or nothing before expiry — rejects
/// the pairing.
pub fn run_pairing_session_events(
    config: &HostConfig,
    events: &crossbeam_channel::Sender<PairingEvent>,
    confirm: &crossbeam_channel::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut host = identity::load_or_create(&config.data_dir)?;
    if host.host_id.is_empty() {
        // First contact: mint the routing identity on the spot. Pairing is
        // exactly the moment a fresh host meets the relay.
        let host_id = identity::register_with_relay(&config.relay_origin, &host.signing)?;
        identity::store_host_id(&config.data_dir, &host_id)?;
        host.host_id = host_id;
    }
    let invite = arm_invite(config, &host)?;
    let _ = events.send(PairingEvent::InviteReady(invite.clone()));

    let mut socket = connect_and_auth(config, &host)?;
    apply_read_timeout(&socket, Some(Duration::from_millis(250)));

    let binding = binding_from(&invite);
    let deadline = Instant::now() + INVITE_TTL;
    let mut responder: Option<PairingResponderFinal> = None;

    // ---- pairing handshake, then the confirmation gate ----
    let (device_id, device_name, device_public, mut channel) = loop {
        if Instant::now() > deadline {
            let _ = events.send(PairingEvent::Failed("invite expired".to_owned()));
            anyhow::bail!("no device arrived before the invite expired");
        }
        // A cancel during the wait (the operator closed the QR view)
        // unwinds the session right here; the UI has already moved on.
        if let Ok(false) = confirm.try_recv() {
            let _ = events.send(PairingEvent::Failed("pairing cancelled".to_owned()));
            anyhow::bail!("pairing cancelled by the operator");
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
                        let mut state =
                            PairingResponder::new(&binding, &host.noise).expect("responder builds");
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
                        break (device_id, device_name, device_public, channel);
                    }
                    Frame::Control {
                        frame:
                            ControlFrame::Rejected {
                                code,
                                message,
                                frame: _,
                            },
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
    // The verification pair is derived from the key the handshake actually
    // bound (the channel's remote static), not from anything the device or
    // the relay claimed: a spliced pairing shows a different code.
    let check_code = pairing_code(&host.noise.public, &device_public, &binding);
    let fingerprint = device_fingerprint(&device_public);
    let _ = events.send(PairingEvent::DevicePresented {
        device_id,
        device_name: device_name.clone(),
        fingerprint,
        check_code,
    });
    let accepted = match confirm.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(accepted) => accepted,
        Err(_) => {
            let _ = events.send(PairingEvent::Failed(
                "confirmation window closed".to_owned(),
            ));
            anyhow::bail!("confirmation window closed with the invite");
        }
    };

    // Persist the grant BEFORE notifying the device: once the decision is
    // out, a freshly paired device may connect immediately, and the KK
    // handshake needs this file already on disk.
    if accepted {
        let grant = DeviceGrant {
            device_id: device_id.to_string(),
            // The label the operator saw, so the device list names the
            // device instead of a UUID. Self-reported, never trusted for
            // authorization — the fingerprint and public key are.
            device_name: device_name.clone(),
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
        reason: if accepted {
            None
        } else {
            Some("rejected on the host".to_owned())
        },
    };
    send_sealed(
        &mut socket,
        &mut channel,
        &invite.host_id,
        device_id,
        decision,
    )?;
    if !accepted {
        let _ = events.send(PairingEvent::Failed("rejected on the host".to_owned()));
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
    let _ = events.send(PairingEvent::Linked { device_id });
    println!("[OK] device {device_id} is paired and linked.");
    Ok(())
}

/// The CLI wrapper: same session, events printed to stdout, the decision
/// read from stdin. Exits non-zero on refusal.
pub fn run_pair_session(config: &HostConfig) -> anyhow::Result<()> {
    let (event_tx, event_rx) = crossbeam_channel::unbounded::<PairingEvent>();
    let (confirm_tx, confirm_rx) = crossbeam_channel::bounded::<bool>(1);
    let config = config.clone();
    let worker = std::thread::Builder::new()
        .name("pairing-session".into())
        .spawn(move || run_pairing_session_events(&config, &event_tx, &confirm_rx))
        .expect("pairing thread spawns");

    let invite_deadline = Instant::now() + INVITE_TTL;
    let mut confirming = false;
    let mut decision_sent = false;
    let outcome = loop {
        match event_rx.recv_timeout(invite_deadline.saturating_duration_since(Instant::now())) {
            Ok(PairingEvent::InviteReady(invite)) => {
                println!("Scan this pairing invite with the device:");
                println!("{}", pairing_url(&invite));
                println!(
                    "{}",
                    serde_json::to_string(&invite).expect("invite serializes")
                );
                println!("Waiting for a device...\n");
            }
            Ok(PairingEvent::DevicePresented {
                device_name,
                fingerprint,
                check_code,
                ..
            }) => {
                confirming = true;
                let shown_name = device_name.unwrap_or_else(|| "unnamed device".to_owned());
                println!("Device requesting pairing:");
                println!("  name:        {shown_name}");
                println!("  fingerprint: {fingerprint}");
                println!("  check code:  {check_code}");
                println!("Compare the check code with the one shown on the device.");
                println!("This device will be granted: {GRANT_SCOPE_TEXT}");
                print!("Confirm pairing? [y/N] ");
                std::io::stdout().flush()?;
            }
            Ok(PairingEvent::Linked { device_id }) => {
                println!("[OK] device {device_id} is paired and linked.");
                break Ok(());
            }
            Ok(PairingEvent::Failed(reason)) => break Err(anyhow::anyhow!(reason)),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                break Err(anyhow::anyhow!("timed out waiting for the pairing session"));
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                break worker.join().expect("pairing thread panics are surfaced");
            }
        }
        if confirming && !decision_sent {
            // Read the decision from stdin exactly once.
            match read_confirmation(Instant::now() + Duration::from_secs(600)) {
                Ok(answer) => {
                    let _ = confirm_tx.send(answer);
                    decision_sent = true;
                }
                Err(error) => break Err(error),
            }
            confirming = false;
        }
    };
    outcome
}

/// Revoke a device: the local grant is marked revoked and the relay link
/// is torn down with a signed declaration. Both ends fail closed while
/// either revocation is still in flight.
///
/// The order is the point: the local grant is revoked and persisted first,
/// so a host that cannot reach the relay has still cut the device off. The
/// relay-side unlink is then reported honestly — see [`RevokeOutcome`].
pub fn revoke_device_with_outcome(
    config: &HostConfig,
    device_id: &str,
) -> anyhow::Result<RevokeOutcome> {
    let host = identity::load_or_create(&config.data_dir)?;
    if host.host_id.is_empty() {
        anyhow::bail!("this host is not registered yet");
    }
    let mut file =
        GrantFile::load(&config.paired_device_file)?.unwrap_or(GrantFile { grants: vec![] });
    if !file.revoke(device_id, now_ms()) {
        anyhow::bail!("no active grant for device {device_id}");
    }
    std::fs::write(
        &config.paired_device_file,
        serde_json::to_string_pretty(&file)?,
    )?;
    log::info!(target: "audit", "device grant revoked locally; device={device_id}");

    // Everything below is the relay-side unlink. A failure here leaves the
    // local revocation intact and is reported as pending, never swallowed.
    let device_uuid: Uuid = match device_id.parse() {
        Ok(uuid) => uuid,
        Err(error) => {
            return Ok(RevokeOutcome::RelayPending {
                reason: format!("device id is not a uuid: {error}"),
            });
        }
    };
    let unlink = (|| -> anyhow::Result<()> {
        // Arm a nonce for the unlink declaration, then send it.
        let mut nonce = [0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut nonce);
        // fetch_challenge returns (base64 text, decoded bytes); the offer
        // body sends the decoded bytes re-encoded — taking the wrong tuple
        // slot here double-encodes and the relay sees a 44-byte
        // "challenge".
        let (_challenge_b64, challenge_bytes) =
            identity::fetch_challenge(&config.relay_origin, "host")?;
        let mut signed = host.host_id.as_bytes().to_vec();
        signed.extend_from_slice(&nonce);
        let offer_signature = host.signing.sign(&signed);
        identity::post_json(
            &format!(
                "{}/api/pairing/offer",
                config.relay_origin.trim_end_matches('/')
            ),
            &serde_json::json!({
                "hostId": host.host_id,
                "nonce": BASE64_STANDARD.encode(nonce),
                "challenge": BASE64_STANDARD.encode(challenge_bytes),
                "signature": BASE64_STANDARD.encode(offer_signature),
            }),
        )?;

        let mut socket = connect_and_auth(config, &host)?;
        apply_read_timeout(&socket, Some(Duration::from_millis(250)));
        // The relay verifies link declarations over the raw 16-byte UUIDs
        // (see relay ws.rs handle_link); signing the textual form is
        // exactly the bug that made every revoke fail relay-side.
        let host_uuid: Uuid = host.host_id.parse().expect("stored host id is a uuid");
        let mut signed = host_uuid.as_bytes().to_vec();
        signed.extend_from_slice(device_uuid.as_bytes());
        signed.extend_from_slice(&nonce);
        let signature = host.signing.sign(&signed);
        send_frame(
            &mut socket,
            Frame::UnlinkDevice {
                host_id: host_uuid,
                device_id: device_uuid,
                nonce: Base64Bytes(nonce.to_vec()),
                signature: Base64Bytes(signature),
            },
        )?;
        wait_for_control(&mut socket, &host_uuid, device_uuid, false)?;
        Ok(())
    })();
    match unlink {
        Ok(()) => {
            println!("[OK] device {device_id} is revoked and unlinked.");
            Ok(RevokeOutcome::Revoked)
        }
        Err(error) => {
            log::warn!(
                target: "audit",
                "relay unlink unconfirmed; device={device_id} error={error:#}"
            );
            Ok(RevokeOutcome::RelayPending {
                reason: error.to_string(),
            })
        }
    }
}

/// Revoke a device for the CLI. A pending relay unlink is an error here:
/// the operator asked for a full revocation and must see that the relay
/// side did not confirm.
pub fn run_revoke_device(config: &HostConfig, device_id: &str) -> anyhow::Result<()> {
    match revoke_device_with_outcome(config, device_id)? {
        RevokeOutcome::Revoked => Ok(()),
        RevokeOutcome::RelayPending { reason } => {
            anyhow::bail!("revoked on this host, but the relay unlink did not confirm: {reason}")
        }
    }
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
    // The pairing channel has no delivery sequence: the frame's `seq` is
    // relay bookkeeping, and the sealed payload says so with a zero
    // transport_seq (protocol: pairing messages carry 0 by convention).
    let sealed = SealedMessage {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        transport_seq: 0,
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
    _host_id: &Uuid,
    device_id: Uuid,
    _linked: bool,
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
                        frame:
                            ControlFrame::Rejected {
                                code,
                                message,
                                frame: _,
                            },
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
fn connect_and_auth(
    config: &HostConfig,
    host: &identity::HostIdentity,
) -> anyhow::Result<HostSocket> {
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
        // The pairing channel occupies its own relay slot: displacing it
        // never touches the business session, and vice versa.
        purpose: ConnectionPurpose::Pairing,
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
                        frame:
                            ControlFrame::Rejected {
                                code,
                                message,
                                frame: _,
                            },
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

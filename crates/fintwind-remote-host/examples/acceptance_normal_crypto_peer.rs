//! Normal exported-bridge black box peer, inside the owned acceptance tree.
//! No relay, model, child process, saved key, or production configuration.
//! Failure contract: docs/remote-control-normal-suite.md (WASM measurement).
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use fintwind_remote_crypto::{
    ConnectResponder, IdentityKey, PairingBinding, PairingResponder, SecureChannel, fingerprint,
    pairing_code,
};
use fintwind_remote_protocol::{
    Base64Bytes, Frame, MessageBody, PairingInvite, REMOTE_PROTOCOL_VERSION, SealedMessage,
};
use serde_json::{Value, json};
use uuid::Uuid;

fn emit(value: Value) -> anyhow::Result<()> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, &value)?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}

fn step(request: &Value) -> anyhow::Result<Vec<u8>> {
    Ok(BASE64.decode(request["step"].as_str().context("step missing")?)?)
}

fn run() -> anyhow::Result<()> {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("isolated workspace required")?,
    );
    ensure!(
        root.is_absolute() && root.join(".fintwind-e2e-fixture").is_file(),
        "workspace marker missing"
    );
    let identity = IdentityKey::generate()?;
    let host_id = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
    let binding = PairingBinding {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        host_id,
        nonce: Uuid::new_v4().as_bytes().to_vec(),
        expires_at_ms: now + 60_000,
        relay_origin: "http://127.0.0.1".to_owned(),
    };
    let invite = PairingInvite {
        protocol_version: binding.protocol_version,
        relay_origin: binding.relay_origin.clone(),
        host_id,
        host_public_key: BASE64.encode(&identity.public),
        nonce: BASE64.encode(&binding.nonce),
        expires_at_ms: binding.expires_at_ms,
    };
    emit(
        json!({"type": "cryptoReady", "synthetic": true, "invite": invite,
        "hostPublic": BASE64.encode(&identity.public), "hostId": host_id,
        "deviceId": device_id, "sessionId": session_id}),
    )?;
    let mut first = Some(PairingResponder::new(&binding, &identity)?);
    let mut final_step = None;
    let mut device_public = None;
    let mut channel: Option<SecureChannel> = None;
    let mut frames = 0_u64;
    let mut bytes = 0_usize;
    let mut logical_messages = 0;
    let mut logical = Vec::with_capacity(1_048_576);
    for line in io::stdin().lock().lines() {
        let line = line?;
        ensure!(line.len() <= 256 * 1024, "request size exceeded");
        let request: Value = serde_json::from_str(&line)?;
        match request["type"].as_str().context("request type missing")? {
            "pairFirst" => {
                let mut responder = first.take().context("pairing already started")?;
                ensure!(
                    responder.read_first(&step(&request)?)?.is_empty(),
                    "pairing payload differs"
                );
                let (reply, last) = responder.write_reply(&[])?;
                final_step = Some(last);
                emit(json!({"type": "pairFirst", "step": BASE64.encode(reply)}))?;
            }
            "pairFinish" => {
                let (hello, paired) = final_step
                    .take()
                    .context("pairing final missing")?
                    .finish(&step(&request)?)?;
                let hello: SealedMessage = serde_json::from_slice(&hello)?;
                ensure!(
                    hello.protocol_version == REMOTE_PROTOCOL_VERSION
                        && hello.host_id == host_id
                        && hello.device_id == device_id
                        && hello.transport_seq == 0,
                    "pairing binding differs"
                );
                ensure!(
                    matches!(hello.body, MessageBody::PairingHello { device_id: id, .. } if id == device_id),
                    "pairing hello differs"
                );
                let remote = paired
                    .remote_static()
                    .context("device key missing")?
                    .to_vec();
                emit(
                    json!({"type": "pairFinish", "code": pairing_code(&identity.public, &remote, &binding),
                    "fingerprint": fingerprint(&identity.public)}),
                )?;
                device_public = Some(remote);
            }
            "connectFirst" => {
                ensure!(channel.is_none() && frames == 0, "connect repeated");
                let mut responder = ConnectResponder::new(
                    REMOTE_PROTOCOL_VERSION,
                    &identity,
                    device_public.as_deref().context("device not paired")?,
                    host_id,
                    device_id,
                )?;
                responder.read_first(&step(&request)?)?;
                let (reply, ready) = responder.finish(&[])?;
                channel = Some(ready);
                emit(json!({"type": "connectFirst", "step": BASE64.encode(reply)}))?;
            }
            "roundTrip" => {
                ensure!(frames < 128, "frame count exceeded");
                let seq = request["seq"].as_u64().context("sequence missing")?;
                ensure!(seq == frames + 1, "sequence differs");
                let frame: Frame =
                    serde_json::from_str(request["frame"].as_str().context("frame missing")?)?;
                let Frame::Sealed {
                    seq: outer_seq,
                    host_id: outer_host,
                    device_id: outer_device,
                    ciphertext,
                } = frame
                else {
                    bail!("sealed frame required");
                };
                ensure!(
                    outer_seq == seq && outer_host == host_id && outer_device == Some(device_id),
                    "frame binding differs"
                );
                let channel = channel.as_mut().context("connect missing")?;
                let envelope: SealedMessage =
                    serde_json::from_slice(&channel.decrypt(&ciphertext.0)?)?;
                ensure!(
                    envelope.protocol_version == REMOTE_PROTOCOL_VERSION
                        && envelope.transport_seq == seq
                        && envelope.host_id == host_id
                        && envelope.device_id == device_id,
                    "envelope binding differs"
                );
                let MessageBody::Fragment {
                    transport_seq,
                    index,
                    of,
                    ref data,
                } = envelope.body
                else {
                    bail!("fragment required");
                };
                ensure!(
                    transport_seq == 10_001
                        && u64::from(index) == frames % 32
                        && of == 32
                        && request["index"].as_u64() == Some(u64::from(index))
                        && request["of"].as_u64() == Some(32),
                    "fragment order differs"
                );
                let chunk = &data.0;
                ensure!(chunk.len() == 32_768, "fragment size differs");
                logical.extend_from_slice(chunk);
                bytes += chunk.len();
                if index == 31 {
                    ensure!(logical.len() == 1_048_576, "logical size differs");
                    let message: SealedMessage = serde_json::from_slice(&logical)?;
                    ensure!(
                        message.protocol_version == REMOTE_PROTOCOL_VERSION
                            && message.host_id == host_id
                            && message.device_id == device_id
                            && message.transport_seq == 10_001,
                        "logical binding differs"
                    );
                    let MessageBody::Transcript {
                        session_id: id,
                        messages,
                    } = message.body
                    else {
                        bail!("logical transcript missing");
                    };
                    ensure!(
                        id == session_id
                            && messages.len() == 1
                            && messages[0].text.bytes().all(|byte| byte == b'x')
                            && messages[0].text.len() > 1_048_000,
                        "logical content differs"
                    );
                    logical.clear();
                    logical_messages += 1;
                }
                // Return the same validated fragment under the host's direction.
                let sealed = SealedMessage {
                    protocol_version: REMOTE_PROTOCOL_VERSION,
                    transport_seq: seq,
                    host_id,
                    device_id,
                    sent_at_ms: 0,
                    body: envelope.body,
                };
                let response = Frame::Sealed {
                    seq,
                    host_id,
                    device_id: Some(device_id),
                    ciphertext: Base64Bytes(channel.encrypt(&serde_json::to_vec(&sealed)?)?),
                };
                frames += 1;
                emit(
                    json!({"type": "roundTrip", "verified": true, "frame": serde_json::to_string(&response)?}),
                )?;
            }
            "finish" => {
                ensure!(
                    frames == 128
                        && bytes == 4 * 1_048_576
                        && logical_messages == 4
                        && logical.is_empty(),
                    "final counters differ"
                );
                emit(
                    json!({"type": "finish", "framesPerDirection": frames, "bytesPerDirection": bytes,
                    "logicalMessagesVerified": logical_messages}),
                )?;
                return Ok(());
            }
            _ => bail!("unknown normal request"),
        }
    }
    bail!("crypto peer ended before finish")
}

fn main() -> anyhow::Result<()> {
    run().map_err(|_| anyhow::anyhow!("normal crypto peer verification failed"))
}

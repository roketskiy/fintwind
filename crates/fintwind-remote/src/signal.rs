//! The v2 signaling WebSocket. The server only matches the two roles of one
//! route and forwards opaque sealed frames; every meaningful byte is encrypted
//! with the pairing key before it reaches this socket.

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use crate::crypto::{self, SignalDirection};

pub(crate) const MAX_SIGNAL_PLAIN: usize = 48 * 1024;
const MAX_SIGNAL_FRAME: usize = 64 * 1024;

/// One verified offer from the phone, with the directions needed to answer.
pub(crate) struct Offer {
    pub attempt: [u8; 32],
    pub attempt_text: String,
    pub sdp: String,
    pub outbound: SignalDirection,
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum Incoming {
    Joined,
    PeerOnline(bool),
    Offer(Offer),
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub(crate) struct Signal {
    socket: Socket,
    route_hash: String,
    secret: [u8; 32],
    joined: bool,
}

impl Signal {
    pub(crate) async fn connect(
        url: &str,
        route_hash: String,
        secret: [u8; 32],
        connect_addr: Option<&str>,
    ) -> anyhow::Result<Self> {
        let socket = match connect_addr {
            // Test deployments may forward the signaling TCP stream through a
            // local port. TLS still authenticates the URL's domain.
            Some(addr) => {
                let stream = tokio::net::TcpStream::connect(addr)
                    .await
                    .context("signaling connection failed")?;
                let (socket, _) = tokio_tungstenite::client_async_tls_with_config(
                    url, stream, None, None,
                )
                .await
                .context("signaling connection failed")?;
                socket
            }
            None => {
                let (socket, _) = tokio_tungstenite::connect_async(url)
                    .await
                    .context("signaling connection failed")?;
                socket
            }
        };
        Ok(Self {
            socket,
            route_hash,
            secret,
            joined: false,
        })
    }

    pub(crate) async fn join(&mut self) -> anyhow::Result<()> {
        let frame = json!({
            "type": "join",
            "version": 2,
            "routeHash": self.route_hash,
            "role": "desktop",
        });
        self.socket
            .send(Message::text(frame.to_string()))
            .await
            .context("signaling join failed")?;
        Ok(())
    }

    /// Reads and authenticates the next meaningful frame. The transport
    /// rejects previously accepted attempts before replacing a connection.
    pub(crate) async fn next(&mut self) -> anyhow::Result<Option<Incoming>> {
        loop {
            let Some(message) = self.socket.next().await else {
                bail!("signaling connection closed");
            };
            let message = message.context("signaling read failed")?;
            let text = match message {
                Message::Text(text) => text,
                Message::Close(_) => bail!("signaling connection closed"),
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
                Message::Binary(_) => bail!("unexpected signaling frame"),
            };
            if text.len() > MAX_SIGNAL_FRAME {
                bail!("signaling frame is too large");
            }
            let frame: Value =
                serde_json::from_str(&text).context("invalid signaling frame")?;
            match frame.get("type").and_then(Value::as_str) {
                Some("joined") => {
                    if frame.get("version").and_then(Value::as_u64) != Some(2) {
                        bail!("unsupported signaling version");
                    }
                    self.joined = true;
                    return Ok(Some(Incoming::Joined));
                }
                Some("peer") => {
                    if frame.get("role").and_then(Value::as_str) != Some("phone") {
                        continue;
                    }
                    let online = frame
                        .get("online")
                        .and_then(Value::as_bool)
                        .context("invalid peer frame")?;
                    return Ok(Some(Incoming::PeerOnline(online)));
                }
                Some("signal") => {
                    if frame.get("sender").and_then(Value::as_str) != Some("phone") {
                        bail!("unexpected signaling sender");
                    }
                    if frame.get("version").and_then(Value::as_u64) != Some(2) {
                        bail!("unsupported signaling version");
                    }
                    return Ok(Some(Incoming::Offer(self.open_offer(&frame)?)));
                }
                _ => bail!("unexpected signaling frame"),
            }
        }
    }

    fn open_offer(
        &self,
        frame: &Value,
    ) -> anyhow::Result<Offer> {
        let attempt_text = frame
            .get("attempt")
            .and_then(Value::as_str)
            .context("missing attempt")?;
        let attempt = decode_32(attempt_text).context("invalid attempt")?;
        let seq = frame
            .get("seq")
            .and_then(Value::as_u64)
            .context("invalid signaling sequence")?;
        let data = frame
            .get("data")
            .and_then(Value::as_str)
            .context("missing signaling ciphertext")?;
        let ciphertext = decode_bytes(data).context("invalid signaling ciphertext")?;
        if ciphertext.len() > MAX_SIGNAL_PLAIN + 16 {
            bail!("signaling ciphertext is too large");
        }
        let (outbound, mut inbound) =
            crypto::signal_directions(&self.secret, &self.route_hash, &attempt, attempt_text)?;
        let plain = inbound.open(seq, &ciphertext)?;
        if plain.len() > MAX_SIGNAL_PLAIN {
            bail!("signaling payload is too large");
        }
        let payload: Value = serde_json::from_slice(&plain).context("invalid offer payload")?;
        if payload.get("kind").and_then(Value::as_str) != Some("offer") {
            bail!("unexpected signaling payload");
        }
        let sdp = payload
            .get("sdp")
            .and_then(Value::as_str)
            .context("missing offer SDP")?;
        if sdp.is_empty() || sdp.len() > MAX_SIGNAL_PLAIN {
            bail!("offer SDP is too large");
        }
        Ok(Offer {
            attempt,
            attempt_text: attempt_text.to_owned(),
            sdp: sdp.to_owned(),
            outbound,
        })
    }

    pub(crate) async fn send_answer(
        &mut self,
        offer: &mut Offer,
        sdp: &str,
    ) -> anyhow::Result<()> {
        if !self.joined {
            bail!("signaling socket is not joined");
        }
        let bytes = serde_json::to_vec(&json!({"kind": "answer", "sdp": sdp}))?;
        if bytes.len() > MAX_SIGNAL_PLAIN {
            bail!("answer SDP is too large");
        }
        let (seq, encrypted) = offer.outbound.seal(&bytes)?;
        if encrypted.len() > MAX_SIGNAL_PLAIN {
            bail!("answer ciphertext is too large");
        }
        let frame = json!({
            "type": "signal",
            "version": 2,
            "attempt": offer.attempt_text,
            "seq": seq,
            "data": crypto::encode(&encrypted),
        });
        self.socket
            .send(Message::text(frame.to_string()))
            .await
            .context("signaling answer send failed")?;
        Ok(())
    }

    pub(crate) async fn close(&mut self) {
        let _ = self.socket.close(None).await;
    }
}

pub(crate) fn decode_32(value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = decode_bytes(value)?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid 32-byte value"))
}

pub(crate) fn decode_bytes(value: &str) -> anyhow::Result<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD.decode(value).context("invalid base64url")?;
    if crypto::encode(&bytes) != value {
        bail!("invalid base64url encoding");
    }
    Ok(bytes)
}

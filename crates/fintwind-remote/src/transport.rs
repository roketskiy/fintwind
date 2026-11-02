use std::{
    collections::HashSet,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::digest;
use serde_json::{Value, json};
use smol::channel;
use tungstenite::{
    Message, client::IntoClientRequest, client_tls_with_config, protocol::WebSocketConfig,
    stream::MaybeTlsStream,
};
use uuid::Uuid;

use crate::{
    DesktopRequest, RelayConfig, RemoteCommand, RemoteEvent, RemoteFailure, RemoteStatus, crypto,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_TIMEOUT: Duration = Duration::from_millis(200);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const INITIAL_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(30);
const MAX_FRAME: usize = 1024 * 1024;
const MAX_RESPONSE: usize = 512 * 1024;
const VIRTUAL_PATH: &str = "/__fintwind_remote__";

type Socket = tungstenite::WebSocket<MaybeTlsStream<DeadlineStream>>;

async fn timeout<F: std::future::Future>(
    duration: Duration,
    future: F,
) -> std::io::Result<F::Output> {
    smol::future::or(async move { Ok(future.await) }, async move {
        smol::Timer::after(duration).await;
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "operation timed out",
        ))
    })
    .await
}

struct DeadlineStream {
    stream: TcpStream,
    handshake_deadline: Option<Instant>,
}

impl DeadlineStream {
    fn timeout(&self, regular: Duration) -> std::io::Result<Duration> {
        let remaining = self
            .handshake_deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(regular);
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "network operation timed out",
            ));
        }
        Ok(remaining)
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.stream
            .set_read_timeout(Some(self.timeout(POLL_TIMEOUT)?))?;
        self.stream.read(buffer)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.stream
            .set_write_timeout(Some(self.timeout(CONNECT_TIMEOUT)?))?;
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream
            .set_write_timeout(Some(self.timeout(CONNECT_TIMEOUT)?))?;
        self.stream.flush()
    }
}

pub(crate) fn run(
    mut config: RelayConfig,
    route: [u8; 32],
    secret: [u8; 32],
    events: channel::Sender<RemoteEvent>,
    cancelled: Arc<AtomicBool>,
) {
    let mut mutations = HashSet::new();
    let mut retry_delay = INITIAL_RETRY;
    while !cancelled.load(Ordering::Acquire) {
        send_status(&events, RemoteStatus::Connecting);
        let result = connect_and_run(
            &mut config,
            &route,
            &secret,
            &events,
            &cancelled,
            &mut mutations,
            &mut retry_delay,
        );
        let Err(error) = result else {
            continue;
        };
        if error
            .downcast_ref::<RemoteFailure>()
            .is_some_and(|failure| matches!(failure, RemoteFailure::CredentialExpired))
        {
            config.credentials.invalidate();
        }
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        send_status(&events, RemoteStatus::Failed(safe_failure_message(&error)));
        let delay = retry_after(&error).unwrap_or(retry_delay);
        sleep_until(delay, &cancelled);
        retry_delay = (retry_delay * 2).min(MAX_RETRY);
    }
}

fn retry_after(error: &anyhow::Error) -> Option<Duration> {
    error
        .chain()
        .find_map(|cause| match cause.downcast_ref::<RemoteFailure>() {
            Some(RemoteFailure::RateLimited { retry_after }) => Some(*retry_after),
            _ => None,
        })
}

fn sleep_until(delay: Duration, cancelled: &AtomicBool) {
    let deadline = Instant::now() + delay;
    while !cancelled.load(Ordering::Acquire) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(POLL_TIMEOUT.min(remaining));
    }
}

fn safe_failure_message(error: &anyhow::Error) -> String {
    for cause in error.chain() {
        if let Some(failure) = cause.downcast_ref::<RemoteFailure>() {
            return match failure {
                RemoteFailure::IdentityUnavailable => "远控安装身份不可用".into(),
                RemoteFailure::Enrollment => "远控注册失败".into(),
                RemoteFailure::RateLimited { .. } => "远控注册过于频繁，请稍后再试".into(),
                RemoteFailure::Revoked => "远控安装身份已被拒绝".into(),
                RemoteFailure::CredentialExpired => "远控登记已过期，正在重新注册".into(),
            };
        }
    }
    for cause in error.chain() {
        if let Some(tungstenite::Error::Http(response)) = cause.downcast_ref::<tungstenite::Error>()
        {
            let status = response.status().as_u16();
            if status == 401 || status == 403 {
                return "远控注册被拒绝".into();
            }
            if status != 0 {
                return format!("远控握手失败（HTTP {status}）");
            }
            return "远控握手失败".into();
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::TimedOut => return "远控连接超时".into(),
                std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::NotConnected => return "远控连接被拒绝".into(),
                std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::NetworkUnreachable => {
                    return "远控网络不可达".into();
                }
                _ => {}
            }
        }
    }
    for cause in error.chain() {
        let text = cause.to_string();
        if text.contains("relay address lookup") {
            return "远控域名解析失败".into();
        }
        if text.contains("relay connection timed out") {
            return "远控连接超时".into();
        }
        if text.contains("relay handshake failed") {
            return "远控安全握手失败".into();
        }
        if text.contains("relay registration refused") {
            return "远控注册被拒绝".into();
        }
        if text.contains("invalid relay frame")
            || text.contains("unexpected relay frame")
            || text.contains("relay frame is too large")
            || text.contains("unsupported relay frame")
            || text.contains("missing virtual socket id")
            || text.contains("invalid virtual socket request")
        {
            return "远控协议异常".into();
        }
    }
    "远控服务暂不可用".into()
}

fn connect_and_run(
    config: &mut RelayConfig,
    route: &[u8; 32],
    secret: &[u8; 32],
    events: &channel::Sender<RemoteEvent>,
    cancelled: &AtomicBool,
    mutations: &mut HashSet<Uuid>,
    retry_delay: &mut Duration,
) -> anyhow::Result<()> {
    // Registration now needs a short-lived admission token. Acquire it before
    // any network work so a refused admission costs no connection attempt.
    let token = config.credentials.token()?.to_owned();
    let host = config.relay_url.host_str().context("relay host missing")?;
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let addresses = (host, 443)
        .to_socket_addrs()
        .context("relay address lookup failed")?;
    let mut stream = None;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        if let Ok(connected) = TcpStream::connect_timeout(&address, remaining) {
            stream = Some(connected);
            break;
        }
    }
    let stream = stream.context("relay connection timed out")?;
    let route_text = crypto::encode(route);
    let route_hash = digest::digest(&digest::SHA256, route_text.as_bytes());
    let token_hash = route_hash
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let request = format!("wss://{host}/agent?th={token_hash}")
        .into_client_request()
        .context("invalid relay request")?;
    let mut ws_config = WebSocketConfig::default();
    ws_config.max_frame_size = Some(MAX_FRAME);
    ws_config.max_message_size = Some(MAX_FRAME);
    let stream = DeadlineStream {
        stream,
        handshake_deadline: Some(Instant::now() + CONNECT_TIMEOUT),
    };
    let (mut socket, _) = client_tls_with_config(request, stream, Some(ws_config), None)
        .context("relay handshake failed")?;
    match socket.get_mut() {
        MaybeTlsStream::Rustls(stream) => {
            stream.sock.handshake_deadline = None;
        }
        _ => bail!("relay TLS stream was not created"),
    }
    send_json(
        &mut socket,
        &json!({"t":"register","tokenHash":token_hash,"name":"Fintwind","proto":1,"key":token}),
    )?;
    let registration = recv_json_until(&mut socket, Instant::now() + CONNECT_TIMEOUT, cancelled)?;
    if registration.get("t").and_then(Value::as_str) != Some("registered") {
        // The relay sends an error frame before closing on a rejected token.
        // Re-enroll on the next attempt instead of retrying the same token.
        config.credentials.invalidate();
        bail!("relay registration refused");
    }
    *retry_delay = INITIAL_RETRY;
    send_status(events, RemoteStatus::Waiting);
    loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = socket.close(None);
            return Ok(());
        }
        match read_message(&mut socket)? {
            Some(Message::Text(text)) => {
                if text.len() > MAX_FRAME {
                    bail!("relay frame is too large");
                }
                let frame: Value =
                    serde_json::from_str(text.as_str()).context("invalid relay frame")?;
                match frame.get("t").and_then(Value::as_str) {
                    Some("wopen") => {
                        let id = frame
                            .get("id")
                            .and_then(Value::as_str)
                            .context("missing virtual socket id")?;
                        if id.is_empty()
                            || id.len() > 255
                            || !has_exact_keys(&frame, &["t", "id", "path"])
                        {
                            bail!("invalid virtual socket request");
                        }
                        if frame.get("path").and_then(Value::as_str) != Some(VIRTUAL_PATH) {
                            send_json(
                                &mut socket,
                                &json!({"t":"werr","id":id,"message":"Unsupported socket path"}),
                            )?;
                            continue;
                        }
                        match serve_phone(&mut socket, id, secret, events, cancelled, mutations) {
                            Ok(()) => {}
                            Err(error) => {
                                if error
                                    .downcast_ref::<RemoteFailure>()
                                    .is_some_and(|failure| {
                                        matches!(failure, RemoteFailure::CredentialExpired)
                                    })
                                {
                                    config.credentials.invalidate();
                                    return Err(error);
                                }
                                send_json(&mut socket, &json!({"t":"wclose","id":id,"code":1008}))?;
                            }
                        }
                        send_status(events, RemoteStatus::Waiting);
                    }
                    Some("hreq") => {
                        reject_http_request(&mut socket, &frame)?;
                    }
                    Some("hcancel" | "push" | "pushed") => bail!("unsupported relay frame"),
                    _ => bail!("unexpected relay frame"),
                }
            }
            Some(Message::Close(frame)) => {
                if frame.is_some_and(|frame| u16::from(frame.code) == 4401) {
                    config.credentials.invalidate();
                    return Err(RemoteFailure::CredentialExpired.into());
                }
                bail!("relay connection closed");
            }
            Some(_) => bail!("unexpected relay frame"),
            None => {}
        }
    }
}

struct DeferredResponse {
    id: Uuid,
    receiver: channel::Receiver<Result<Value, String>>,
    deadline: Instant,
    read_only: bool,
    if_revision: Option<String>,
}

fn serve_phone(
    socket: &mut Socket,
    socket_id: &str,
    secret: &[u8; 32],
    events: &channel::Sender<RemoteEvent>,
    cancelled: &AtomicBool,
    mutations: &mut HashSet<Uuid>,
) -> anyhow::Result<()> {
    send_json(socket, &json!({"t":"wopened","id":socket_id}))?;
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let hello_text = recv_tunnel_text(socket, socket_id, deadline, cancelled)?;
    let hello: Value = serde_json::from_str(&hello_text).context("invalid phone hello")?;
    if !has_exact_keys(&hello, &["type", "version", "nonce"])
        || hello.get("type").and_then(Value::as_str) != Some("hello")
        || hello.get("version").and_then(Value::as_u64) != Some(1)
    {
        bail!("invalid phone handshake");
    }
    let phone_nonce = decode_nonce(
        hello
            .get("nonce")
            .and_then(Value::as_str)
            .context("missing nonce")?,
    )?;
    let desktop_nonce = crypto::random_32()?;
    send_tunnel_text(
        socket,
        socket_id,
        &json!({"type":"hello","version":1,"nonce":crypto::encode(&desktop_nonce)}).to_string(),
    )?;
    let (mut outbound, mut inbound) = crypto::directions(secret, &phone_nonce, &desktop_nonce)?;
    send_sealed(socket, socket_id, &mut outbound, &json!({"type":"ready"}))?;
    let mut authenticated = false;
    let mut deferred: Vec<DeferredResponse> = Vec::new();
    loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = send_json(socket, &json!({"t":"wclose","id":socket_id,"code":1000}));
            return Ok(());
        }
        let mut index = 0;
        while index < deferred.len() {
            let answer = match deferred[index].receiver.try_recv() {
                Ok(answer) => Some(answer),
                Err(channel::TryRecvError::Closed) => {
                    Some(Err("Desktop request handler stopped".into()))
                }
                Err(channel::TryRecvError::Empty) if Instant::now() >= deferred[index].deadline => {
                    Some(Err("操作结果待确认，请勿重复提交。".into()))
                }
                Err(channel::TryRecvError::Empty) => None,
            };
            if let Some(answer) = answer {
                let item = deferred.swap_remove(index);
                let answer = if item.read_only {
                    answer.map(|value| {
                        crate::snapshot::conditional_response(value, item.if_revision.as_deref())
                    })
                } else {
                    answer
                };
                reply(socket, socket_id, &mut outbound, item.id, answer)?;
            } else {
                index += 1;
            }
        }
        match read_message(socket)? {
            Some(Message::Text(text)) => {
                if text.len() > MAX_FRAME {
                    bail!("phone frame is too large");
                }
                let relay_frame: Value =
                    serde_json::from_str(text.as_str()).context("invalid relay frame")?;
                if relay_frame.get("t").and_then(Value::as_str) == Some("hreq") {
                    reject_http_request(socket, &relay_frame)?;
                    continue;
                }
                if relay_frame.get("t").and_then(Value::as_str) == Some("wopen") {
                    let other_id = relay_frame
                        .get("id")
                        .and_then(Value::as_str)
                        .context("missing virtual socket id")?;
                    if other_id.is_empty() || other_id.len() > 255 {
                        bail!("invalid virtual socket id");
                    }
                    if other_id != socket_id {
                        send_json(
                            socket,
                            &json!({"t":"werr","id":other_id,"message":"A phone is already connected"}),
                        )?;
                        continue;
                    }
                    bail!("duplicate virtual socket open");
                }
                if relay_frame.get("id").and_then(Value::as_str) != Some(socket_id) {
                    bail!("unexpected virtual socket frame");
                }
                match relay_frame.get("t").and_then(Value::as_str) {
                    Some("wclose") => return Ok(()),
                    Some("wmsg") if has_exact_keys(&relay_frame, &["t", "id", "s"]) => {}
                    _ => bail!("unexpected virtual socket frame"),
                }
                let phone_text = relay_frame
                    .get("s")
                    .and_then(Value::as_str)
                    .context("missing virtual socket text")?;
                let envelope: Value =
                    serde_json::from_str(phone_text).context("invalid phone envelope")?;
                if !has_exact_keys(&envelope, &["type", "seq", "data"])
                    || envelope.get("type").and_then(Value::as_str) != Some("sealed")
                {
                    bail!("invalid phone envelope");
                }
                let seq = envelope
                    .get("seq")
                    .and_then(Value::as_u64)
                    .context("invalid sequence")?;
                let encoded = envelope
                    .get("data")
                    .and_then(Value::as_str)
                    .context("missing ciphertext")?;
                let ciphertext = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .context("invalid ciphertext")?;
                if crypto::encode(&ciphertext) != encoded {
                    bail!("invalid ciphertext encoding");
                }
                let plain = inbound.open(seq, &ciphertext)?;
                if !authenticated {
                    authenticated = true;
                    send_status(events, RemoteStatus::Connected);
                }
                if plain.len() > MAX_FRAME {
                    bail!("decrypted frame is too large");
                }
                let request: IncomingRequest =
                    serde_json::from_slice(&plain).context("invalid remote request")?;
                if !request.command.within_limits() {
                    reply(
                        socket,
                        socket_id,
                        &mut outbound,
                        request.id,
                        Err("Invalid request".into()),
                    )?;
                    continue;
                }
                let read_only = !request.command.is_mutation();
                let allow_deferred = matches!(
                    &request.command,
                    RemoteCommand::GetComposer { .. }
                        | RemoteCommand::UploadChunk { complete: true, .. }
                );
                if allow_deferred && deferred.len() >= 8 {
                    reply(
                        socket,
                        socket_id,
                        &mut outbound,
                        request.id,
                        Err("正在处理其他请求，请稍后重试。".into()),
                    )?;
                    continue;
                }
                if request.if_revision.as_deref().is_some_and(|revision| {
                    !read_only || !crate::snapshot::valid_revision(revision)
                }) {
                    reply(
                        socket,
                        socket_id,
                        &mut outbound,
                        request.id,
                        Err("Invalid snapshot version".into()),
                    )?;
                    continue;
                }
                if request.command.is_mutation() && !crate::reserve_mutation(mutations, request.id)
                {
                    reply(
                        socket,
                        socket_id,
                        &mut outbound,
                        request.id,
                        Err("Request was already handled or the activation is full".into()),
                    )?;
                    continue;
                }
                let (response_tx, response_rx) = channel::bounded(1);
                let event = RemoteEvent::Request(DesktopRequest {
                    id: request.id,
                    command: request.command,
                    respond: response_tx,
                });
                smol::block_on(timeout(Duration::from_secs(15), events.send(event)))
                    .context("desktop request queue timed out")?
                    .context("desktop request handler stopped")?;
                let deadline = Instant::now() + REQUEST_TIMEOUT;
                if allow_deferred {
                    deferred.push(DeferredResponse {
                        id: request.id,
                        receiver: response_rx,
                        deadline,
                        read_only,
                        if_revision: request.if_revision,
                    });
                    continue;
                }
                let answer = loop {
                    if cancelled.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    if Instant::now() >= deadline {
                        break Err("操作结果待确认，请勿重复提交。".to_string());
                    }
                    match smol::block_on(timeout(POLL_TIMEOUT, response_rx.recv())) {
                        Ok(Ok(Ok(value))) => break Ok(value),
                        Ok(Ok(Err(message))) => break Err(message),
                        Ok(Err(_)) => break Err("The desktop request handler stopped".to_string()),
                        Err(_) => {}
                    }
                };
                let answer = if read_only {
                    answer.map(|value| {
                        crate::snapshot::conditional_response(value, request.if_revision.as_deref())
                    })
                } else {
                    answer
                };
                reply(socket, socket_id, &mut outbound, request.id, answer)?;
            }
            Some(Message::Close(frame)) => {
                if frame.is_some_and(|frame| u16::from(frame.code) == 4401) {
                    return Err(RemoteFailure::CredentialExpired.into());
                }
                bail!("relay connection closed");
            }
            Some(_) => bail!("invalid phone frame"),
            None => {}
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IncomingRequest {
    id: Uuid,
    command: RemoteCommand,
    #[serde(default)]
    if_revision: Option<String>,
}

fn decode_nonce(value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD.decode(value).context("invalid nonce")?;
    let nonce: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid nonce length"))?;
    if crypto::encode(&nonce) != value {
        bail!("invalid nonce encoding");
    }
    Ok(nonce)
}

fn has_exact_keys(value: &Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    })
}

fn reply(
    socket: &mut Socket,
    socket_id: &str,
    outbound: &mut crypto::Direction,
    id: Uuid,
    answer: Result<Value, String>,
) -> anyhow::Result<()> {
    let response = match answer {
        Ok(value)
            if serde_json::to_vec(&value)
                .map(|bytes| bytes.len() <= MAX_RESPONSE)
                .unwrap_or(false) =>
        {
            json!({"id":id,"ok":true,"data":value})
        }
        Ok(_) => json!({"id":id,"ok":false,"error":"Response exceeds the allowed size"}),
        Err(message) => {
            json!({"id":id,"ok":false,"error":message.chars().take(256).collect::<String>()})
        }
    };
    send_sealed(socket, socket_id, outbound, &response)
}

fn send_sealed(
    socket: &mut Socket,
    socket_id: &str,
    direction: &mut crypto::Direction,
    value: &Value,
) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_RESPONSE {
        bail!("outgoing payload exceeds the allowed size");
    }
    let (seq, encrypted) = direction.seal(&bytes)?;
    send_tunnel_text(
        socket,
        socket_id,
        &json!({"type":"sealed","seq":seq,"data":URL_SAFE_NO_PAD.encode(encrypted)}).to_string(),
    )
}

fn send_tunnel_text(socket: &mut Socket, socket_id: &str, text: &str) -> anyhow::Result<()> {
    send_json(socket, &json!({"t":"wmsg","id":socket_id,"s":text}))
}

fn send_json(socket: &mut Socket, value: &Value) -> anyhow::Result<()> {
    let text = serde_json::to_string(value)?;
    if text.len() > MAX_FRAME {
        bail!("outgoing relay frame is too large");
    }
    socket
        .send(Message::Text(text.into()))
        .context("relay write failed")?;
    Ok(())
}

fn reject_http_request(socket: &mut Socket, frame: &Value) -> anyhow::Result<()> {
    let id = frame
        .get("id")
        .and_then(Value::as_str)
        .context("missing HTTP request id")?;
    if id.is_empty() || id.len() > 255 {
        bail!("invalid HTTP request id");
    }
    send_json(
        socket,
        &json!({"t":"hres","id":id,"status":403,"headers":{"content-length":"0"}}),
    )?;
    send_json(socket, &json!({"t":"hend","id":id}))
}

fn recv_json_until(
    socket: &mut Socket,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> anyhow::Result<Value> {
    loop {
        if cancelled.load(Ordering::Acquire) {
            bail!("remote control stopped");
        }
        if Instant::now() >= deadline {
            bail!("relay registration timed out");
        }
        match read_message(socket)? {
            Some(Message::Text(text)) => {
                if text.len() > MAX_FRAME {
                    bail!("relay frame is too large");
                }
                return serde_json::from_str(text.as_str()).context("invalid relay frame");
            }
            Some(Message::Close(frame)) => {
                if frame.is_some_and(|frame| u16::from(frame.code) == 4401) {
                    return Err(RemoteFailure::CredentialExpired.into());
                }
                bail!("relay registration refused");
            }
            Some(_) => bail!("invalid registration frame"),
            None => {}
        }
    }
}

fn recv_tunnel_text(
    socket: &mut Socket,
    socket_id: &str,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> anyhow::Result<String> {
    loop {
        if cancelled.load(Ordering::Acquire) {
            bail!("remote control stopped");
        }
        if Instant::now() >= deadline {
            bail!("phone handshake timed out");
        }
        let Some(Message::Text(text)) = read_message(socket)? else {
            continue;
        };
        if text.len() > MAX_FRAME {
            bail!("phone frame is too large");
        }
        let frame: Value =
            serde_json::from_str(text.as_str()).context("invalid virtual socket frame")?;
        if frame.get("t").and_then(Value::as_str) != Some("wmsg")
            || frame.get("id").and_then(Value::as_str) != Some(socket_id)
            || !has_exact_keys(&frame, &["t", "id", "s"])
        {
            bail!("unexpected virtual socket frame");
        }
        return Ok(frame
            .get("s")
            .and_then(Value::as_str)
            .context("missing virtual socket text")?
            .to_owned());
    }
}

fn read_message(socket: &mut Socket) -> anyhow::Result<Option<Message>> {
    match socket.read() {
        Ok(Message::Ping(_) | Message::Pong(_)) => {
            socket.flush()?;
            Ok(None)
        }
        Ok(message) => Ok(Some(message)),
        Err(tungstenite::Error::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn send_status(events: &channel::Sender<RemoteEvent>, status: RemoteStatus) {
    let _ = smol::block_on(timeout(
        Duration::from_millis(100),
        events.send(RemoteEvent::Status(status)),
    ));
}

//! One direct connection attempt: chunk framing, the mutual authentication
//! handshake, and the existing remote-control business loop on a WebRTC data
//! channel. The transport owns the peer connection and signaling; this module
//! owns everything that travels over the channel once it is open.

use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, bail};
use futures::{FutureExt, future::{BoxFuture, select_all}};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;
use webrtc::data_channel::{DataChannel, DataChannelEvent};

use crate::{
    DesktopRequest, RemoteCommand, RemoteEvent, RemoteStatus, crypto, signal, snapshot,
};

const CHUNK_BYTES: usize = 8192;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE: usize = 512 * 1024;
const REASSEMBLY_TIMEOUT: Duration = Duration::from_secs(15);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const EVENT_SEND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_QUEUED_MESSAGES: usize = 64;
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;
const MAX_BACKLOG: usize = 8;
const MAX_DEFERRED: usize = 8;
const SEND_LOW_THRESHOLD: u32 = 32 * 1024;

pub(crate) struct Session {
    secret: [u8; 32],
    events: mpsc::Sender<RemoteEvent>,
    cancelled: Arc<AtomicBool>,
    mutations: Arc<Mutex<HashSet<Uuid>>>,
}

impl Session {
    pub(crate) fn new(
        secret: [u8; 32],
        events: mpsc::Sender<RemoteEvent>,
        cancelled: Arc<AtomicBool>,
        mutations: Arc<Mutex<HashSet<Uuid>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            secret,
            events,
            cancelled,
            mutations,
        })
    }

    /// Runs until the channel closes, the activation is cancelled, or a
    /// protocol violation forces the channel down. The caller closes the peer
    /// connection afterwards; this method closes the data channel itself.
    pub(crate) async fn run(
        self: Arc<Self>,
        channel: Arc<dyn DataChannel>,
        attempt_text: String,
        offer_hash: String,
        answer_hash: String,
    ) -> anyhow::Result<()> {
        let result = self
            .drive(&channel, &attempt_text, &offer_hash, &answer_hash)
            .await;
        let _ = channel.close().await;
        result
    }

    async fn drive(
        &self,
        channel: &Arc<dyn DataChannel>,
        attempt_text: &str,
        offer_hash: &str,
        answer_hash: &str,
    ) -> anyhow::Result<()> {
        let _ = channel
            .set_buffered_amount_low_threshold(SEND_LOW_THRESHOLD)
            .await;
        let mut state = Connection {
            channel: channel.clone(),
            phase: Phase::WaitHello,
            directions: None,
            reassembly: None,
            queue: VecDeque::new(),
            queued_bytes: 0,
            sending: VecDeque::new(),
            paused: false,
            busy: None,
            backlog: VecDeque::new(),
            deferred: Vec::new(),
            watch: None,
            handshake_deadline: tokio::time::Instant::now() + HANDSHAKE_TIMEOUT,
        };
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return Ok(());
            }
            state.pump_outbound().await?;
            state.pump_watch()?;
            state.pump_answers().await?;
            while state.busy.is_none() {
                let Some(next) = state.backlog.pop_front() else {
                    break;
                };
                self.submit_queued(&mut state, next).await?;
            }
            state.check_deadlines()?;
            if !state.paused && (!state.queue.is_empty() || !state.sending.is_empty()) {
                continue;
            }
            enum Wake {
                Channel(Option<DataChannelEvent>),
                Internal(InternalWake),
            }
            let wake = tokio::select! {
                event = channel.poll() => Wake::Channel(event),
                event = state.wait_internal() => Wake::Internal(event),
            };
            match wake {
                Wake::Channel(event) => {
                    match event {
                        Some(DataChannelEvent::OnMessage(message)) => {
                            if !message.is_string {
                                bail!("unexpected binary channel message");
                            }
                            let text = std::str::from_utf8(&message.data)
                                .context("invalid channel text")?;
                            if let Some(plain) = state.feed_chunk(text)? {
                                self.handle_message(&mut state, plain, attempt_text, offer_hash, answer_hash).await?;
                            }
                        }
                        Some(DataChannelEvent::OnBufferedAmountLow) => {
                            state.paused = false;
                        }
                        Some(DataChannelEvent::OnClose) | None => return Ok(()),
                        Some(DataChannelEvent::OnClosing) => return Ok(()),
                        Some(_) => {}
                    }
                }
                Wake::Internal(event) => state.handle_internal(event)?,
            }
        }
    }

    async fn handle_message(
        &self,
        state: &mut Connection,
        plain: Vec<u8>,
        attempt_text: &str,
        offer_hash: &str,
        answer_hash: &str,
    ) -> anyhow::Result<()> {
        if plain.len() > MAX_MESSAGE_BYTES {
            bail!("channel message is too large");
        }
        let value: Value = serde_json::from_slice(&plain).context("invalid channel message")?;
        match state.phase {
            Phase::WaitHello => {
                if !has_exact_keys(&value, &["type", "version", "nonce"])
                    || value.get("type").and_then(Value::as_str) != Some("hello")
                    || value.get("version").and_then(Value::as_u64) != Some(2)
                {
                    bail!("invalid phone hello");
                }
                let phone_nonce = signal::decode_32(
                    value
                        .get("nonce")
                        .and_then(Value::as_str)
                        .context("missing nonce")?,
                )?;
                let desktop_nonce = crypto::random_32()?;
                state.enqueue(serde_json::to_vec(&json!({
                    "type": "hello",
                    "version": 2,
                    "nonce": crypto::encode(&desktop_nonce),
                }))?)?;
                let (outbound, inbound) =
                    crypto::directions(&self.secret, &phone_nonce, &desktop_nonce)?;
                state.directions = Some((outbound, inbound));
                state.send_sealed(&json!({
                    "type": "auth",
                    "role": "desktop",
                    "attempt": attempt_text,
                    "offerHash": offer_hash,
                    "answerHash": answer_hash,
                }))?;
                state.phase = Phase::WaitPhoneAuth;
                Ok(())
            }
            Phase::WaitPhoneAuth => {
                let payload = state.open_sealed(&value)?;
                let auth: Value =
                    serde_json::from_slice(&payload).context("invalid phone auth")?;
                if !has_exact_keys(
                    &auth,
                    &["type", "role", "attempt", "offerHash", "answerHash"],
                ) || auth.get("type").and_then(Value::as_str) != Some("auth")
                    || auth.get("role").and_then(Value::as_str) != Some("phone")
                    || auth.get("attempt").and_then(Value::as_str) != Some(attempt_text)
                    || auth.get("offerHash").and_then(Value::as_str) != Some(offer_hash)
                    || auth.get("answerHash").and_then(Value::as_str) != Some(answer_hash)
                {
                    bail!("phone authentication failed");
                }
                state.send_sealed(&json!({"type": "ready", "attempt": attempt_text}))?;
                state.phase = Phase::Business;
                self.send_status(RemoteStatus::Connected).await;
                Ok(())
            }
            Phase::Business => {
                let payload = state.open_sealed(&value)?;
                let request: IncomingRequest =
                    serde_json::from_slice(&payload).context("invalid remote request")?;
                self.dispatch(state, request).await
            }
        }
    }

    async fn dispatch(
        &self,
        state: &mut Connection,
        request: IncomingRequest,
    ) -> anyhow::Result<()> {
        if !request.command.within_limits() {
            return state.reply(request.id, Err("Invalid request".into()));
        }
        if let RemoteCommand::WatchSession {
            session_id,
            watch_id,
            enabled,
        } = &request.command
        {
            if request.if_revision.is_some() {
                return state.reply(
                    request.id,
                    Err("Subscriptions do not accept snapshot versions".into()),
                );
            }
            if *enabled {
                let (snapshots, receiver) = smol::channel::bounded(1);
                let event = RemoteEvent::WatchSession {
                    session_id: *session_id,
                    snapshots,
                };
                self.send_event(event).await?;
                state.watch = Some(SessionWatch {
                    id: *watch_id,
                    snapshots: receiver,
                    previous: None,
                    ready: None,
                });
            } else if state
                .watch
                .as_ref()
                .is_some_and(|watch| watch.id == *watch_id)
            {
                state.watch = None;
            }
            return state.reply(request.id, Ok(json!({"accepted": true})));
        }
        if request
            .if_revision
            .as_deref()
            .is_some_and(|revision| {
                request.command.is_mutation() || !snapshot::valid_revision(revision)
            })
        {
            return state.reply(request.id, Err("Invalid snapshot version".into()));
        }
        // Submissions are serialized so a mutation's dedup reservation always
        // precedes the desktop seeing the request, even under deferred answers.
        if state.busy.is_some() {
            if state.backlog.len() >= MAX_BACKLOG {
                return state.reply(
                    request.id,
                    Err("正在处理其他请求，请稍后重试。".into()),
                );
            }
            state.backlog.push_back(request);
            return Ok(());
        }
        self.submit_queued(state, request).await
    }

    /// Dedup reservation happens here, immediately before the desktop sees
    /// the request, so a queued mutation cannot be executed twice.
    async fn submit_queued(
        &self,
        state: &mut Connection,
        request: IncomingRequest,
    ) -> anyhow::Result<()> {
        if request.command.is_mutation() {
            let mut mutations = self.mutations.lock().await;
            if !crate::reserve_mutation(&mut mutations, request.id) {
                return state.reply(
                    request.id,
                    Err("Request was already handled or the activation is full".into()),
                );
            }
        }
        let read_only = !request.command.is_mutation();
        let generation_request = matches!(&request.command, RemoteCommand::GenerateBtw { .. });
        let allow_deferred = matches!(
            &request.command,
        RemoteCommand::GetComposer { .. }
            | RemoteCommand::SearchFiles { .. }
            | RemoteCommand::GenerateBtw { .. }
                | RemoteCommand::SteerPrompt { .. }
                | RemoteCommand::UploadChunk { complete: true, .. }
        );
        if allow_deferred {
            if state.deferred.len() >= MAX_DEFERRED {
                return state.reply(
                    request.id,
                    Err("正在处理其他请求，请稍后重试。".into()),
                );
            }
            let receiver = self.submit(request.id, request.command).await?;
            state.deferred.push(Pending {
                id: request.id,
                receiver,
                deadline: tokio::time::Instant::now() + if generation_request { Duration::from_secs(150) } else { REQUEST_TIMEOUT },
                read_only,
                if_revision: request.if_revision,
            });
            return Ok(());
        }
        self.start_blocking(state, request).await
    }

    async fn start_blocking(
        &self,
        state: &mut Connection,
        request: IncomingRequest,
    ) -> anyhow::Result<()> {
        let read_only = !request.command.is_mutation();
        let receiver = self.submit(request.id, request.command).await?;
        state.busy = Some(Pending {
            id: request.id,
            receiver,
            deadline: tokio::time::Instant::now() + REQUEST_TIMEOUT,
            read_only,
            if_revision: request.if_revision,
        });
        Ok(())
    }

    async fn submit(
        &self,
        id: Uuid,
        command: RemoteCommand,
    ) -> anyhow::Result<smol::channel::Receiver<Result<Value, String>>> {
        let (response_tx, response_rx) = smol::channel::bounded(1);
        self.send_event(RemoteEvent::Request(DesktopRequest {
            id,
            command,
            respond: response_tx,
        }))
        .await?;
        Ok(response_rx)
    }

    async fn send_event(&self, event: RemoteEvent) -> anyhow::Result<()> {
        tokio::time::timeout(EVENT_SEND_TIMEOUT, self.events.send(event))
            .await
            .context("desktop request queue timed out")?
            .context("desktop request handler stopped")
    }

    async fn send_status(&self, status: RemoteStatus) {
        let _ = tokio::time::timeout(
            Duration::from_millis(100),
            self.events.send(RemoteEvent::Status(status)),
        )
        .await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    WaitHello,
    WaitPhoneAuth,
    Business,
}

struct Pending {
    id: Uuid,
    receiver: smol::channel::Receiver<Result<Value, String>>,
    deadline: tokio::time::Instant,
    read_only: bool,
    if_revision: Option<String>,
}

struct SessionWatch {
    id: Uuid,
    snapshots: smol::channel::Receiver<Result<Value, String>>,
    previous: Option<Value>,
    ready: Option<Result<Value, String>>,
}

enum InternalWake {
    Watch(Uuid, Result<Result<Value, String>, smol::channel::RecvError>),
    Answer(Uuid, Result<Result<Value, String>, smol::channel::RecvError>),
    Deadline,
}

struct Reassembly {
    id: Uuid,
    total: u64,
    next: u64,
    bytes: Vec<u8>,
    deadline: tokio::time::Instant,
}

struct Connection {
    channel: Arc<dyn DataChannel>,
    phase: Phase,
    directions: Option<(crypto::Direction, crypto::Direction)>,
    reassembly: Option<Reassembly>,
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    sending: VecDeque<String>,
    paused: bool,
    busy: Option<Pending>,
    backlog: VecDeque<IncomingRequest>,
    deferred: Vec<Pending>,
    watch: Option<SessionWatch>,
    handshake_deadline: tokio::time::Instant,
}

impl Connection {
    async fn wait_internal(&self) -> InternalWake {
        let mut waits: Vec<BoxFuture<'static, InternalWake>> = Vec::new();
        if self.queue.len() <= MAX_DEFERRED
            && let Some(watch) = &self.watch
        {
            let receiver = watch.snapshots.clone();
            let id = watch.id;
            waits.push(async move { InternalWake::Watch(id, receiver.recv().await) }.boxed());
        }
        for pending in self.busy.iter().chain(self.deferred.iter()) {
            let receiver = pending.receiver.clone();
            let id = pending.id;
            waits.push(async move { InternalWake::Answer(id, receiver.recv().await) }.boxed());
        }
        let mut deadline = if self.phase == Phase::Business { None } else { Some(self.handshake_deadline) };
        for at in self.reassembly.iter().map(|item| item.deadline)
            .chain(self.busy.iter().chain(self.deferred.iter()).map(|item| item.deadline))
        {
            deadline = Some(deadline.map_or(at, |current| current.min(at)));
        }
        if let Some(at) = deadline {
            waits.push(async move { tokio::time::sleep_until(at).await; InternalWake::Deadline }.boxed());
        }
        if waits.is_empty() {
            return std::future::pending().await;
        }
        select_all(waits).await.0
    }

    fn handle_internal(&mut self, event: InternalWake) -> anyhow::Result<()> {
        match event {
            InternalWake::Watch(id, result) => {
                if self.watch.as_ref().is_some_and(|watch| watch.id == id) {
                    match result {
                        Ok(value) => self.watch.as_mut().expect("watch exists").ready = Some(value),
                        Err(_) => {
                            self.watch = None;
                            self.send_sealed(&json!({"type": "sessionUpdate", "watchId": id, "error": "会话订阅已结束"}))?;
                        }
                    }
                }
            }
            InternalWake::Answer(id, result) => {
                let item = if self.busy.as_ref().is_some_and(|item| item.id == id) {
                    self.busy.take()
                } else {
                    self.deferred.iter().position(|item| item.id == id).map(|index| self.deferred.swap_remove(index))
                };
                if let Some(item) = item {
                    let answer = result.unwrap_or_else(|_| Err("Desktop request handler stopped".into()));
                    self.reply(item.id, finalize(item.read_only, item.if_revision.as_deref(), answer))?;
                }
            }
            InternalWake::Deadline => {}
        }
        Ok(())
    }
    fn enqueue(&mut self, bytes: Vec<u8>) -> anyhow::Result<()> {
        if self.queue.len() >= MAX_QUEUED_MESSAGES
            || self.queued_bytes + bytes.len() > MAX_QUEUED_BYTES
        {
            bail!("outgoing queue is full");
        }
        self.queued_bytes += bytes.len();
        self.queue.push_back(bytes);
        Ok(())
    }

    fn send_sealed(&mut self, value: &Value) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > MAX_RESPONSE {
            bail!("outgoing payload exceeds the allowed size");
        }
        let (outbound, _) = self
            .directions
            .as_mut()
            .context("channel keys are not established")?;
        let (seq, encrypted) = outbound.seal(&bytes)?;
        self.enqueue(serde_json::to_vec(&json!({
            "type": "sealed",
            "seq": seq,
            "data": crypto::encode(&encrypted),
        }))?)
    }

    fn open_sealed(&mut self, value: &Value) -> anyhow::Result<Vec<u8>> {
        if !has_exact_keys(value, &["type", "seq", "data"])
            || value.get("type").and_then(Value::as_str) != Some("sealed")
        {
            bail!("invalid phone envelope");
        }
        let seq = value
            .get("seq")
            .and_then(Value::as_u64)
            .context("invalid sequence")?;
        let encoded = value
            .get("data")
            .and_then(Value::as_str)
            .context("missing ciphertext")?;
        let ciphertext = signal::decode_bytes(encoded).context("invalid ciphertext")?;
        if ciphertext.len() > MAX_MESSAGE_BYTES {
            bail!("ciphertext is too large");
        }
        let (_, inbound) = self
            .directions
            .as_mut()
            .context("channel keys are not established")?;
        inbound.open(seq, &ciphertext)
    }

    fn reply(&mut self, id: Uuid, answer: Result<Value, String>) -> anyhow::Result<()> {
        let response = match answer {
            Ok(value)
                if serde_json::to_vec(&value)
                    .map(|bytes| bytes.len() <= MAX_RESPONSE)
                    .unwrap_or(false) =>
            {
                json!({"id": id, "ok": true, "data": value})
            }
            Ok(_) => json!({"id": id, "ok": false, "error": "Response exceeds the allowed size"}),
            Err(message) => {
                json!({"id": id, "ok": false, "error": message.chars().take(256).collect::<String>()})
            }
        };
        self.send_sealed(&response)
    }

    /// Accepts one chunk frame; returns the reassembled message when complete.
    fn feed_chunk(&mut self, text: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let frame: Value = serde_json::from_str(text).context("invalid chunk frame")?;
        if !has_exact_keys(&frame, &["type", "id", "index", "total", "data"])
            || frame.get("type").and_then(Value::as_str) != Some("chunk")
        {
            bail!("invalid chunk frame");
        }
        let id = frame
            .get("id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .context("invalid chunk id")?;
        let index = frame
            .get("index")
            .and_then(Value::as_u64)
            .context("invalid chunk index")?;
        let total = frame
            .get("total")
            .and_then(Value::as_u64)
            .context("invalid chunk total")?;
        if total == 0 || total > (MAX_MESSAGE_BYTES / CHUNK_BYTES + 1) as u64 {
            bail!("invalid chunk total");
        }
        let data = frame
            .get("data")
            .and_then(Value::as_str)
            .context("missing chunk data")?;
        let bytes = signal::decode_bytes(data).context("invalid chunk data")?;
        if bytes.len() > CHUNK_BYTES {
            bail!("chunk is too large");
        }
        match &mut self.reassembly {
            None => {
                if index != 0 {
                    bail!("chunk sequence starts mid-message");
                }
                self.reassembly = Some(Reassembly {
                    id,
                    total,
                    next: 0,
                    bytes: Vec::new(),
                    deadline: tokio::time::Instant::now() + REASSEMBLY_TIMEOUT,
                });
            }
            Some(current) => {
                if current.id != id || current.total != total {
                    bail!("interleaved channel messages");
                }
            }
        }
        let current = self.reassembly.as_mut().expect("reassembly exists");
        if index != current.next {
            bail!("out-of-order chunk");
        }
        if current.bytes.len() + bytes.len() > MAX_MESSAGE_BYTES {
            bail!("reassembled message is too large");
        }
        current.bytes.extend_from_slice(&bytes);
        current.next += 1;
        if current.next == current.total {
            let done = self.reassembly.take().expect("reassembly exists");
            return Ok(Some(done.bytes));
        }
        Ok(None)
    }

    async fn pump_outbound(&mut self) -> anyhow::Result<()> {
        while !self.paused {
            if self.sending.is_empty() {
                let Some(bytes) = self.queue.pop_front() else {
                    return Ok(());
                };
                self.queued_bytes -= bytes.len();
                let id = Uuid::new_v4().to_string();
                let total = bytes.len().div_ceil(CHUNK_BYTES).max(1);
                for index in 0..total {
                    let start = index * CHUNK_BYTES;
                    let end = (start + CHUNK_BYTES).min(bytes.len());
                    let chunk = json!({
                        "type": "chunk",
                        "id": id,
                        "index": index,
                        "total": total,
                        "data": crypto::encode(&bytes[start..end]),
                    });
                    self.sending.push_back(chunk.to_string());
                }
            }
            let Some(chunk) = self.sending.front() else {
                continue;
            };
            match self.channel.try_send_text(chunk).await {
                Ok(()) => {
                    self.sending.pop_front();
                }
                Err(webrtc::error::Error::ErrSendBufferFull) => {
                    self.paused = true;
                    return Ok(());
                }
                Err(error) => return Err(error).context("channel send failed"),
            }
        }
        Ok(())
    }

    fn pump_watch(&mut self) -> anyhow::Result<()> {
        // A slow consumer coalesces updates instead of growing the queue.
        if self.queue.len() > MAX_DEFERRED {
            return Ok(());
        }
        // Compute the update under the watch borrow, then release it before
        // sealing so the send path can touch the rest of the connection.
        let prepared = {
            let Some(watch) = &mut self.watch else {
                return Ok(());
            };
            let result = watch.ready.take().map(Ok).unwrap_or_else(|| watch.snapshots.try_recv());
            match result {
                Ok(Ok(value)) => {
                    let next = snapshot::conditional_response(value, None);
                    if watch.previous.as_ref() == Some(&next) {
                        None
                    } else {
                        let full = json!({"type": "sessionUpdate", "watchId": watch.id, "snapshot": next});
                        let update = if let Some(previous) = &watch.previous {
                            let edits = snapshot::delta(previous, &next);
                            let delta = json!({"type": "sessionUpdate", "watchId": watch.id,
                                "baseRevision": previous["revision"], "revision": next["revision"], "edits": edits});
                            if serde_json::to_vec(&delta)?.len() < serde_json::to_vec(&full)?.len() {
                                delta
                            } else {
                                full
                            }
                        } else {
                            full
                        };
                        Some((update, Some(next)))
                    }
                }
                Ok(Err(error)) => Some((
                    json!({
                        "type": "sessionUpdate",
                        "watchId": watch.id,
                        "error": error.chars().take(512).collect::<String>(),
                    }),
                    None,
                )),
                Err(_) => None,
            }
        };
        if let Some((update, next)) = prepared {
            self.send_sealed(&update)?;
            if let Some(next) = next
                && let Some(watch) = &mut self.watch
            {
                watch.previous = Some(next);
            }
        }
        Ok(())
    }

    async fn pump_answers(&mut self) -> anyhow::Result<()> {
        let mut index = 0;
        while index < self.deferred.len() {
            let answer = poll_pending(&mut self.deferred[index]);
            if let Some(answer) = answer {
                let item = self.deferred.swap_remove(index);
                let answer = finalize(item.read_only, item.if_revision.as_deref(), answer);
                self.reply(item.id, answer)?;
            } else {
                index += 1;
            }
        }
        let busy_answer = self.busy.as_mut().and_then(poll_pending);
        if let Some(answer) = busy_answer {
            let item = self.busy.take().expect("busy slot exists");
            let answer = finalize(item.read_only, item.if_revision.as_deref(), answer);
            self.reply(item.id, answer)?;
        }
        Ok(())
    }

    fn check_deadlines(&self) -> anyhow::Result<()> {
        let now = tokio::time::Instant::now();
        if self.phase != Phase::Business && now >= self.handshake_deadline {
            bail!("channel handshake timed out");
        }
        if let Some(reassembly) = &self.reassembly
            && now >= reassembly.deadline
        {
            bail!("chunk reassembly timed out");
        }
        Ok(())
    }
}

fn poll_pending(pending: &mut Pending) -> Option<Result<Value, String>> {
    match pending.receiver.try_recv() {
        Ok(answer) => Some(answer),
        Err(smol::channel::TryRecvError::Closed) => {
            Some(Err("Desktop request handler stopped".into()))
        }
        Err(smol::channel::TryRecvError::Empty)
            if tokio::time::Instant::now() >= pending.deadline =>
        {
            Some(Err("操作结果待确认，请勿重复提交。".into()))
        }
        Err(smol::channel::TryRecvError::Empty) => None,
    }
}

fn finalize(
    read_only: bool,
    if_revision: Option<&str>,
    answer: Result<Value, String>,
) -> Result<Value, String> {
    if read_only {
        answer.map(|value| snapshot::conditional_response(value, if_revision))
    } else {
        answer
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

fn has_exact_keys(value: &Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    })
}

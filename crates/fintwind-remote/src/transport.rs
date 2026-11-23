use std::{
    collections::HashSet,
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    time::Duration,
};

use anyhow::{Context, bail};
use tokio::sync::{Mutex, Notify, mpsc, watch};
use uuid::Uuid;
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    peer_connection::{
        MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
        RTCConfigurationBuilder, RTCIceGatheringState, RTCIceServer, RTCPeerConnectionState,
        RTCSessionDescription, Registry, SettingEngineBuilder, register_default_interceptors,
    },
    runtime::TokioRuntime,
};

use crate::{
    RelayConfig, RemoteEvent, RemoteStatus, crypto,
    session::Session,
    signal::{Incoming, Offer, Signal},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const NEGOTIATE_TIMEOUT: Duration = Duration::from_secs(30);
const CHANNEL_TIMEOUT: Duration = Duration::from_secs(60);
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(20);
const INITIAL_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(30);
const SEND_BUFFER_LIMIT: usize = 256 * 1024;

#[derive(Clone)]
struct ConnectionHandler {
    channels: mpsc::Sender<Arc<dyn DataChannel>>,
    gathered: watch::Sender<bool>,
    states: mpsc::Sender<RTCPeerConnectionState>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for ConnectionHandler {
    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        match channel.label().await {
            Ok(label) if label == "fintwind.remote.v2" => {}
            _ => {
                let _ = channel.close().await;
                return;
            }
        }
        let _ = self.channels.try_send(channel);
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.send_replace(true);
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let _ = self.states.try_send(state);
    }
}

struct Peer {
    connection: Arc<dyn PeerConnection>,
    channels: Option<mpsc::Receiver<Arc<dyn DataChannel>>>,
    gathered: watch::Receiver<bool>,
    states: Option<mpsc::Receiver<RTCPeerConnectionState>>,
}

impl Peer {
    async fn new(stun_url: String) -> anyhow::Result<Self> {
        let (channels_tx, channels) = mpsc::channel(1);
        let (gathered_tx, gathered) = watch::channel(false);
        let (states_tx, states) = mpsc::channel(8);
        let mut media = MediaEngine::default();
        media.register_default_codecs()?;
        let registry = register_default_interceptors(Registry::new(), &mut media)?;
        let connection = PeerConnectionBuilder::new()
            .with_configuration(RTCConfigurationBuilder::new().with_ice_servers(vec![RTCIceServer {
                urls: vec![stun_url],
                ..Default::default()
            }]).build())
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_setting_engine(SettingEngineBuilder::new().build())
            .with_handler(Arc::new(ConnectionHandler {
                channels: channels_tx,
                gathered: gathered_tx,
                states: states_tx,
            }))
            .with_runtime(Arc::new(TokioRuntime))
            .with_udp_addrs(vec!["0.0.0.0:0"])
            .with_data_channel_send_buffer_limit(SEND_BUFFER_LIMIT)
            .build().await.context("could not create direct connection")?;
        Ok(Self {
            connection: Arc::new(connection),
            channels: Some(channels),
            gathered,
            states: Some(states),
        })
    }

    async fn answer(&mut self, offer: &Offer) -> anyhow::Result<(String, String, String)> {
        let offer_hash = crypto::sha256_hex(offer.sdp.as_bytes());
        let description: RTCSessionDescription = serde_json::from_value(serde_json::json!({
            "type": "offer", "sdp": offer.sdp,
        }))?;
        self.connection.set_remote_description(description).await?;
        let answer = self.connection.create_answer(None).await?;
        self.connection.set_local_description(answer).await?;
        let mut gathered = self.gathered.clone();
        while !*gathered.borrow_and_update() {
            gathered.changed().await.context("ICE gathering stopped")?;
        }
        let answer = self.connection.local_description().await.context("answer unavailable")?;
        Ok((answer.sdp.clone(), offer_hash, crypto::sha256_hex(answer.sdp.as_bytes())))
    }

    async fn close(&self) {
        let _ = self.connection.close().await;
    }
}

struct ActiveConnection {
    connection: Arc<dyn PeerConnection>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl ActiveConnection {
    async fn close(self) -> anyhow::Result<()> {
        let _ = self.connection.close().await;
        self.task.await.context("remote session task stopped")?
    }
}

#[allow(clippy::large_enum_variant)]
enum Link {
    Offline,
    Connecting(tokio::task::JoinHandle<anyhow::Result<Signal>>),
    Ready {
        signal: Option<Signal>,
        #[allow(clippy::type_complexity)]
        reading: Option<tokio::task::JoinHandle<anyhow::Result<(Signal, Option<Incoming>)>>>,
    },
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    config: RelayConfig,
    route: [u8; 32],
    secret: [u8; 32],
    events: mpsc::Sender<RemoteEvent>,
    cancelled: Arc<AtomicBool>,
    signal_connect_addr: Option<String>,
    stun_port: Option<u16>,
    wake: Arc<Notify>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = events.blocking_send(RemoteEvent::Status(RemoteStatus::Failed(format!("无法启动远控传输：{error}"))));
            return;
        }
    };
    let route_hash = crypto::sha256_hex(crypto::encode(&route).as_bytes());
    let mutations = Arc::new(Mutex::new(HashSet::<Uuid>::new()));
    runtime.block_on(run_transport(config, route_hash, secret, events, cancelled, mutations, signal_connect_addr, stun_port, wake));
}

#[allow(clippy::too_many_arguments)]
async fn run_transport(
    config: RelayConfig,
    route_hash: String,
    secret: [u8; 32],
    events: mpsc::Sender<RemoteEvent>,
    cancelled: Arc<AtomicBool>,
    mutations: Arc<Mutex<HashSet<Uuid>>>,
    signal_connect_addr: Option<String>,
    stun_port: Option<u16>,
    wake: Arc<Notify>,
) {
    let (session_events, mut session_receiver) = mpsc::channel(32);
    let session = Session::new(secret, session_events, cancelled.clone(), mutations);
    let mut accepted = HashSet::<[u8; 32]>::new();
    let mut active: Option<ActiveConnection> = None;
    let mut link = Link::Offline;
    let mut retry_at = tokio::time::Instant::now();
    let mut retry_delay = INITIAL_RETRY;
    let mut joined = false;
    let mut reported = RemoteStatus::Connecting;
    let mut terminal_error = None;
    let mut cancelled_error = None;
    let urls = match config.signal_url().and_then(|signal| config.stun_url(stun_port).map(|stun| (signal, stun))) {
        Ok(urls) => urls,
        Err(error) => {
            publish(&events, &mut reported, RemoteStatus::Failed(safe_failure(&error))).await;
            return;
        }
    };
    publish(&events, &mut reported, RemoteStatus::Connecting).await;
    while !cancelled.load(Ordering::Acquire) {
        while let Ok(event) = session_receiver.try_recv() {
            if matches!(event, RemoteEvent::Status(RemoteStatus::Connected))
                && active.is_some()
            {
                reported = RemoteStatus::Connected;
            }
            if events.send(event).await.is_err() {
                cancelled.store(true, Ordering::Release);
                break;
            }
        }
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        if active.as_ref().is_some_and(|connection| connection.task.is_finished()) {
            let connection = active.take().unwrap();
            let result = connection.close().await;
            while let Ok(event) = session_receiver.try_recv() {
                if matches!(event, RemoteEvent::Status(RemoteStatus::Connected)) {
                    reported = RemoteStatus::Connected;
                }
                if events.send(event).await.is_err() {
                    cancelled.store(true, Ordering::Release);
                    break;
                }
            }
            match result {
                Ok(()) => publish(&events, &mut reported, RemoteStatus::Waiting).await,
                Err(error) => {
                    if !cancelled.load(Ordering::Acquire) {
                        publish(&events, &mut reported, RemoteStatus::Failed(safe_failure(&error))).await;
                    }
                }
            }
            if cancelled.load(Ordering::Acquire) {
                break;
            }
        }
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        match &mut link {
            Link::Offline if tokio::time::Instant::now() >= retry_at => {
                let url = urls.0.clone();
                let hash = route_hash.clone();
                let stop = cancelled.clone();
                let connect_addr = signal_connect_addr.clone();
                let completed = wake.clone();
                let task = tokio::spawn(async move {
                    let result = async move {
                    let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
                    let mut operation = Box::pin(async {
                        let mut signal =
                            Signal::connect(&url, hash, secret, connect_addr.as_deref()).await?;
                        signal.join().await?;
                        Ok(signal)
                    });
                    loop {
                        if stop.load(Ordering::Acquire) { bail!("remote control stopped"); }
                        tokio::select! {
                            result = &mut operation => return result,
                            _ = tokio::time::sleep_until(deadline) => bail!("signaling connection timed out"),
                            _ = tokio::time::sleep(TICK) => {}
                        }
                    }
                    }.await;
                    completed.notify_one();
                    result
                });
                link = Link::Connecting(task);
            }
            Link::Connecting(task) if task.is_finished() => {
                let result = task.await.context("signaling task stopped").and_then(|result| result);
                match result {
                    Ok(signal) => {
                        link = Link::Ready { signal: Some(signal), reading: None };
                        retry_delay = INITIAL_RETRY;
                        joined = false;
                    }
                    Err(error) => {
                        link = Link::Offline;
                        retry_at = tokio::time::Instant::now() + retry_delay;
                        retry_delay = (retry_delay * 2).min(MAX_RETRY);
                        publish(&events, &mut reported, RemoteStatus::Failed(safe_failure(&error))).await;
                    }
                }
            }
            Link::Ready { signal, reading } => {
                if reading.is_none() {
                    let Some(mut owned) = signal.take() else {
                        terminal_error = Some(anyhow::anyhow!("signaling socket unavailable"));
                        break;
                    };
                    let completed = wake.clone();
                    let task = tokio::spawn(async move {
                        let result = owned.next().await.map(|incoming| (owned, incoming));
                        completed.notify_one();
                        result
                    });
                    *reading = Some(task);
                }
                if reading.as_ref().is_some_and(|task| task.is_finished()) {
                    let task = reading.take().unwrap();
                    let result = task.await.context("signaling read task stopped").and_then(|result| result);
                    match result {
                        Ok((returned_signal, incoming)) => {
                            *signal = Some(returned_signal);
                            match incoming {
                                Some(Incoming::Joined) => {
                                    joined = true;
                                    if active.is_none() { publish(&events, &mut reported, RemoteStatus::Waiting).await; }
                                }
                                Some(Incoming::PeerOnline(online)) => {
                                    if joined && online && active.is_none() { publish(&events, &mut reported, RemoteStatus::Waiting).await; }
                                }
                                Some(Incoming::Offer(offer)) => {
                                    if accepted.contains(&offer.attempt) { continue; }
                                    accepted.insert(offer.attempt);
                                    if let Some(previous) = active.take() {
                                        let _ = previous.close().await;
                                    }
                                    let Some(open_signal) = signal.as_mut() else {
                                        terminal_error = Some(anyhow::anyhow!("signaling socket unavailable"));
                                        break;
                                    };
                                    match start_peer(open_signal, &urls.1, offer, session.clone(), cancelled.clone(), wake.clone()).await {
                                        Ok(connection) => {
                                            active = Some(connection);
                                            publish(&events, &mut reported, RemoteStatus::Connecting).await;
                                        }
                                        Err(error) => {
                                            if cancelled.load(Ordering::Acquire) {
                                                cancelled_error = Some(error);
                                            } else {
                                                publish(&events, &mut reported, RemoteStatus::Failed(safe_failure(&error))).await;
                                            }
                                            if cancelled.load(Ordering::Acquire) { break; }
                                        }
                                    }
                                }
                                None => {}
                            }
                        }
                        Err(error) => {
                            link = Link::Offline;
                            retry_at = tokio::time::Instant::now() + retry_delay;
                            retry_delay = (retry_delay * 2).min(MAX_RETRY);
                            if active.is_none() { publish(&events, &mut reported, RemoteStatus::Failed(safe_failure(&error))).await; }
                        }
                    }
                }
            }
            _ => {}
        }
        if matches!(&link, Link::Ready { signal: Some(_), reading: None }) {
            continue;
        }
        tokio::select! {
            _ = wake.notified() => {},
            event = session_receiver.recv() => {
                if let Some(event) = event {
                    if matches!(event, RemoteEvent::Status(RemoteStatus::Connected)) && active.is_some() {
                        reported = RemoteStatus::Connected;
                    }
                    if events.send(event).await.is_err() {
                        cancelled.store(true, Ordering::Release);
                    }
                }
            },
            _ = async {
                if matches!(link, Link::Offline) {
                    tokio::time::sleep_until(retry_at).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {},
        }
    }
    if let Some(connection) = active.take() { let _ = connection.close().await; }
    while let Ok(event) = session_receiver.try_recv() {
        if events.send(event).await.is_err() { break; }
    }
    match link {
        Link::Connecting(task) => task.abort(),
        Link::Ready { signal, reading } => {
            if let Some(task) = reading { task.abort(); }
            if let Some(mut signal) = signal { signal.close().await; }
        }
        Link::Offline => {}
    }
    drop(session);
    while let Some(event) = session_receiver.recv().await {
        if events.send(event).await.is_err() { break; }
    }
    if let Some(error) = terminal_error.or(cancelled_error) {
        publish(&events, &mut reported, RemoteStatus::Failed(safe_failure(&error))).await;
    }
}

async fn start_peer(
    signal: &mut Signal,
    stun_url: &str,
    mut offer: Offer,
    session: Arc<Session>,
    cancelled: Arc<AtomicBool>,
    wake: Arc<Notify>,
) -> anyhow::Result<ActiveConnection> {
    let mut peer = Peer::new(stun_url.to_owned()).await?;
    if cancelled.load(Ordering::Acquire) {
        peer.close().await;
        bail!("remote control stopped");
    }
    let answer = match await_cancelled(&cancelled, NEGOTIATE_TIMEOUT, peer.answer(&offer)).await {
        Ok(Some(answer)) => answer,
        Ok(None) => { peer.close().await; bail!("remote control stopped"); }
        Err(error) => { peer.close().await; return Err(error); }
    };
    let sent = match await_cancelled(&cancelled, CONNECT_TIMEOUT, signal.send_answer(&mut offer, &answer.0)).await {
        Ok(result) => result,
        Err(error) => { peer.close().await; return Err(error); }
    };
    if !matches!(sent, Some(())) {
        peer.close().await;
        bail!("signaling answer failed");
    }
    let channels = peer.channels.take().context("channel receiver unavailable")?;
    let states = peer.states.take().context("state receiver unavailable")?;
    let connection = peer.connection.clone();
    let session_peer = connection.clone();
    let attempt_text = offer.attempt_text;
    let task = tokio::spawn(async move {
        let result = run_session(session, channels, states, session_peer, cancelled, attempt_text, answer.1, answer.2).await;
        wake.notify_one();
        result
    });
    Ok(ActiveConnection { connection, task })
}

async fn await_cancelled<T>(
    cancelled: &AtomicBool,
    timeout: Duration,
    operation: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<Option<T>> {
    let mut operation = Box::pin(operation);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cancelled.load(Ordering::Acquire) { return Ok(None); }
        tokio::select! {
            result = &mut operation => return result.map(Some),
            _ = tokio::time::sleep_until(deadline) => bail!("remote operation timed out"),
            _ = tokio::time::sleep(TICK) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    session: Arc<Session>,
    mut channels: mpsc::Receiver<Arc<dyn DataChannel>>,
    mut states: mpsc::Receiver<RTCPeerConnectionState>,
    peer: Arc<dyn PeerConnection>,
    cancelled: Arc<AtomicBool>,
    attempt_text: String,
    offer_hash: String,
    answer_hash: String,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + CHANNEL_TIMEOUT;
    let channel = loop {
        if cancelled.load(Ordering::Acquire) { return Ok(()); }
        tokio::select! {
            channel = channels.recv() => match channel {
                Some(channel) => break channel,
                None => bail!("data channel unavailable"),
            },
            _ = tokio::time::sleep_until(deadline) => bail!("data channel timed out"),
            _ = tokio::time::sleep(TICK) => {}
        }
    };
    let mut opening = Box::pin(async {
        loop {
            match channel.poll().await {
                Some(DataChannelEvent::OnOpen) => return Ok(()),
                Some(DataChannelEvent::OnClose) | None => bail!("data channel closed before opening"),
                _ => {}
            }
        }
    });
    let mut disconnected_since = None;
    let opened = loop {
        if cancelled.load(Ordering::Acquire) { let _ = channel.close().await; return Ok(()); }
        let mut state_failure = None;
        while let Ok(state) = states.try_recv() {
            match state {
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                    state_failure = Some(anyhow::anyhow!("direct connection failed before opening"));
                    break;
                }
                RTCPeerConnectionState::Disconnected => { disconnected_since.get_or_insert_with(tokio::time::Instant::now); }
                RTCPeerConnectionState::Connected => disconnected_since = None,
                _ => {}
            }
        }
        if let Some(error) = state_failure {
            break Err(error);
        }
        if disconnected_since.is_some_and(|since| since.elapsed() >= DISCONNECT_TIMEOUT) {
            break Err(anyhow::anyhow!("direct connection remained disconnected"));
        }
        tokio::select! {
            result = &mut opening => break result,
            _ = tokio::time::sleep_until(deadline) => break Err(anyhow::anyhow!("data channel open timed out")),
            _ = tokio::time::sleep(TICK) => {}
        }
    };
    if let Err(error) = opened {
        let _ = channel.close().await;
        return Err(error);
    }
    drop(opening);
    let health_channel = channel.clone();
    let health = tokio::spawn(async move {
        let mut disconnected_since = None;
        loop {
            match states.recv().await {
                Some(RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) => {
                    let _ = health_channel.close().await; let _ = peer.close().await; break;
                }
                Some(RTCPeerConnectionState::Disconnected) => {
                    let since = *disconnected_since.get_or_insert_with(tokio::time::Instant::now);
                    tokio::select! {
                        _ = tokio::time::sleep_until(since + DISCONNECT_TIMEOUT) => {
                            let _ = health_channel.close().await; let _ = peer.close().await; break;
                        }
                        state = states.recv() => {
                            match state {
                                Some(RTCPeerConnectionState::Connected) => disconnected_since = None,
                                Some(RTCPeerConnectionState::Failed) | Some(RTCPeerConnectionState::Closed) | None => {
                                    let _ = health_channel.close().await;
                                    let _ = peer.close().await;
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Some(RTCPeerConnectionState::Connected) => disconnected_since = None,
                Some(_) => {}
                None => break,
            }
        }
    });
    let result = session.run(channel, attempt_text, offer_hash, answer_hash).await;
    health.abort();
    result
}

async fn publish(events: &mpsc::Sender<RemoteEvent>, reported: &mut RemoteStatus, status: RemoteStatus) {
    if *reported != status && events.send(RemoteEvent::Status(status.clone())).await.is_ok() {
        *reported = status;
    }
}

fn safe_failure(error: &anyhow::Error) -> String {
    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<std::io::Error>() {
            return match error.kind() {
                std::io::ErrorKind::TimedOut => "远控连接超时".into(),
                std::io::ErrorKind::ConnectionRefused => "远控服务拒绝连接".into(),
                std::io::ErrorKind::NetworkUnreachable => "远控网络不可达".into(),
                _ => "远控连接失败".into(),
            };
        }
        let text = cause.to_string();
        if text.contains("signaling") { return "信令连接失败，正在重试".into(); }
        if text.contains("ICE") || text.contains("direct connection") || text.contains("data channel") {
            return "无法建立直连，请切换网络后重试".into();
        }
    }
    "远控连接已断开".into()
}

//! OpenCode 2.1.4 client-hosted MCP, scoped to one resident server location.
//! The server authenticates the outbound socket and supplies the native session
//! in each call's metadata. Only live driver bindings can create tasks.

use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use parking_lot::{Condvar, Mutex};
use serde::Deserialize;
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

use super::tools::{self, Runtime};
use crate::opencode_pool::{PooledServer, ServerLiveness};

const NAME: &str = "fintwind_scheduled_tasks";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

struct State {
    stopped: AtomicBool,
    bindings: Mutex<HashMap<String, (Uuid, Runtime)>>,
    registry: Arc<Registry>,
    connection: Mutex<ConnectionState>,
    changed: Condvar,
}

enum ConnectionState {
    Disconnected,
    Ready,
    Failed(String),
}

pub(crate) struct Registry {
    enabled: AtomicBool,
    generation: AtomicU64,
    hosts: Mutex<Vec<Weak<Host>>>,
}

impl Registry {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            generation: AtomicU64::new(0),
            hosts: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        if self.enabled.swap(enabled, Ordering::AcqRel) == enabled {
            return;
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        let hosts = {
            let mut hosts = self.hosts.lock();
            hosts.retain(|host| host.strong_count() != 0);
            hosts.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
        };
        for host in &hosts {
            host.state.update_connection(ConnectionState::Disconnected);
            host.worker.thread().unpark();
        }
        if enabled {
            let deadline = Instant::now() + CONNECT_TIMEOUT;
            for host in hosts {
                if let Err(error) = host.wait_until_ready(deadline) {
                    eprintln!("Fintwind scheduled tasks MCP unavailable: {error:#}");
                }
            }
        }
    }
}

impl State {
    fn update_connection(&self, connection: ConnectionState) {
        *self.connection.lock() = connection;
        self.changed.notify_all();
    }
}

pub(crate) struct Host {
    state: Arc<State>,
    worker: JoinHandle<()>,
}

pub(crate) struct Binding {
    host: Arc<Host>,
    native_session: String,
    id: Uuid,
}

impl Binding {
    pub(crate) fn attach(
        server: &PooledServer,
        directory: &str,
        native_session: &str,
        runtime: Runtime,
    ) -> anyhow::Result<Self> {
        let mut hosts = server.scheduled_mcp.lock();
        hosts.retain(|_, host| host.strong_count() != 0);
        let host = match hosts.get(directory).and_then(Weak::upgrade) {
            Some(host) => host,
            None => {
                let host = Host::start(server, directory, runtime.registry.clone())?;
                hosts.insert(directory.to_owned(), Arc::downgrade(&host));
                host
            }
        };
        let id = Uuid::new_v4();
        host.state
            .bindings
            .lock()
            .insert(native_session.to_owned(), (id, runtime));
        Ok(Self {
            host,
            native_session: native_session.to_owned(),
            id,
        })
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        let mut bindings = self.host.state.bindings.lock();
        if bindings
            .get(&self.native_session)
            .is_some_and(|(id, _)| *id == self.id)
        {
            bindings.remove(&self.native_session);
        }
    }
}

impl Host {
    fn start(
        server: &PooledServer,
        directory: &str,
        registry: Arc<Registry>,
    ) -> anyhow::Result<Arc<Self>> {
        let state = Arc::new(State {
            stopped: AtomicBool::new(false),
            bindings: Mutex::new(HashMap::new()),
            registry: registry.clone(),
            connection: Mutex::new(ConnectionState::Disconnected),
            changed: Condvar::new(),
        });
        let mut url = url::Url::parse(&format!(
            "ws://127.0.0.1:{}/api/experimental/client-mcp",
            server.port
        ))?;
        url.query_pairs_mut()
            .append_pair("name", NAME)
            .append_pair("location[directory]", directory);
        let authorization = crate::opencode_session::basic_authorization(server.port)
            .context("private OpenCode server has no credentials")?;
        let authorization = authorization
            .strip_prefix("Authorization: ")
            .context("invalid private OpenCode authorization header")?
            .to_owned();
        let liveness = server.liveness();
        let worker_state = state.clone();
        let port = server.port;
        let worker = thread::Builder::new()
            .name("fintwind-scheduled-mcp".into())
            .spawn(move || run(worker_state, liveness, port, url, authorization))?;
        let host = Arc::new(Self { state, worker });
        registry.hosts.lock().push(Arc::downgrade(&host));
        host.worker.thread().unpark();
        if registry.is_enabled() {
            host.wait_until_ready(Instant::now() + CONNECT_TIMEOUT)?;
        }
        Ok(host)
    }

    fn wait_until_ready(&self, deadline: Instant) -> anyhow::Result<()> {
        let mut connection = self.state.connection.lock();
        while self.state.registry.is_enabled() {
            match &*connection {
                ConnectionState::Ready => return Ok(()),
                ConnectionState::Failed(message) => bail!("{message}"),
                ConnectionState::Disconnected => {}
            }
            if self
                .state
                .changed
                .wait_until(&mut connection, deadline)
                .timed_out()
            {
                bail!("OpenCode client-hosted MCP initialization timed out (requires 2.1.4+)");
            }
        }
        Ok(())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
        self.worker.thread().unpark();
    }
}

fn connect(port: u16, url: &url::Url, authorization: &str) -> anyhow::Result<WebSocket<TcpStream>> {
    let stream = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", authorization.parse()?);
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let (socket, _) = tungstenite::client::client_with_config(request, stream, Some(config))
        .context("OpenCode client-hosted MCP connection failed (requires 2.1.4+)")?;
    socket.get_ref().set_read_timeout(Some(POLL_INTERVAL))?;
    Ok(socket)
}

fn run(
    state: Arc<State>,
    liveness: ServerLiveness,
    port: u16,
    url: url::Url,
    authorization: String,
) {
    let mut delay = Duration::from_millis(500);
    while !state.stopped.load(Ordering::Acquire) && liveness.is_alive() {
        if !state.registry.is_enabled() {
            thread::park();
            delay = Duration::from_millis(500);
            continue;
        }
        let generation = state.registry.generation.load(Ordering::Acquire);
        match connect(port, &url, &authorization) {
            Ok(mut socket) => {
                if let Err(error) = serve(&mut socket, &state, &liveness, &mut delay, generation)
                    && !state.stopped.load(Ordering::Acquire)
                {
                    eprintln!("scheduled MCP connection ended: {error:#}");
                }
                let _ = socket.close(None);
                state.update_connection(ConnectionState::Disconnected);
            }
            Err(error) => {
                state.update_connection(ConnectionState::Failed(format!("{error:#}")));
            }
        }
        if !state.stopped.load(Ordering::Acquire)
            && state.registry.generation.load(Ordering::Acquire) == generation
        {
            thread::park_timeout(delay);
            delay = (delay * 2).min(Duration::from_secs(10));
        }
    }
}

#[derive(Deserialize)]
struct Request {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
struct Call {
    name: String,
    arguments: Value,
    #[serde(rename = "_meta")]
    meta: Meta,
}

#[derive(Deserialize)]
struct Meta {
    #[serde(rename = "ai.opencode/sessionID")]
    session_id: String,
}

fn serve(
    socket: &mut WebSocket<TcpStream>,
    state: &State,
    liveness: &ServerLiveness,
    delay: &mut Duration,
    generation: u64,
) -> anyhow::Result<()> {
    while !state.stopped.load(Ordering::Acquire)
        && state.registry.is_enabled()
        && state.registry.generation.load(Ordering::Acquire) == generation
        && liveness.is_alive()
    {
        let text = match socket.read() {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => continue,
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let Ok(request) = serde_json::from_str::<Request>(&text) else {
            continue;
        };
        if request.jsonrpc != "2.0" {
            continue;
        }
        if request.method == "notifications/initialized" {
            *delay = Duration::from_millis(500);
        }
        let Some(id) = request.id.filter(|id| id.is_string() || id.is_number()) else {
            continue;
        };
        let reply = match request.method.as_str() {
            "initialize" => json!({"result": {
                "protocolVersion": request.params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-11-25"),
                "capabilities": {"tools": {}}, "serverInfo": {"name": NAME, "version": "1.0.0"},
                "instructions": "Use fintwind_scheduled_tasks.create to create recurring tasks in Fintwind. Before calling, ask the user to resolve any unspecified target, schedule, time zone, or task instructions. Do not invent defaults. The tool requires an explicit target and schedule; new sessions copy the current workspace, model and permission settings."
            }}),
            "ping" => json!({"result": {}}),
            "tools/list" => json!({"result": {"tools": [tools::definition()]}}),
            "tools/call" => match call(state, request.params) {
                Ok(value) => {
                    json!({"result": {"content": [{"type": "text", "text": serde_json::to_string(&value)?}], "structuredContent": value}})
                }
                Err(error) => {
                    json!({"result": {"isError": true, "content": [{"type": "text", "text": format!("{error:#}")}]}})
                }
            },
            _ => json!({"error": {"code": -32601, "message": "Method not found"}}),
        };
        let mut reply = reply;
        reply["jsonrpc"] = json!("2.0");
        reply["id"] = id;
        socket.send(Message::Text(serde_json::to_string(&reply)?.into()))?;
        if request.method == "tools/list"
            && state.registry.generation.load(Ordering::Acquire) == generation
        {
            state.update_connection(ConnectionState::Ready);
        }
    }
    Ok(())
}

fn call(state: &State, params: Value) -> anyhow::Result<Value> {
    let call: Call = serde_json::from_value(params).context(
        "A scheduled task call requires an OpenCode session and explicit task arguments",
    )?;
    if call.name != "create" {
        bail!("Unknown scheduled task tool");
    }
    let runtime = state
        .bindings
        .lock()
        .get(&call.meta.session_id)
        .map(|(_, runtime)| runtime.clone())
        .context("This OpenCode session has no active Fintwind task binding")?;
    runtime.create(call.arguments)
}

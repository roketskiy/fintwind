//! Browser-only capabilities for Fintwind-owned OpenCode processes.
//!
//! Failures to guard against: cwd/session confusion, a model selecting another
//! runtime, stale process credentials, child-session inheritance, late work
//! after disconnect, and credentials escaping through Debug or persisted state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use anyhow::{Context as _, bail};
use parking_lot::Mutex;
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::browser_broker::BrowserBroker;
use fintwind_protocol::browser::{BrowserAction, BrowserResult, BrowserScope};
use fintwind_protocol::browser_tools::BrowserToolPage;

const MAX_TOOL_SERVERS: usize = 32;
const MAX_TOOL_SESSIONS: usize = 1024;
const MAX_SESSION_ID_BYTES: usize = 128;
// Desktop subscribers count up from zero. Plugin callers use the upper half,
// so closing one transport can never retire another transport's capabilities.
static NEXT_TOOL_CONNECTION: AtomicU64 = AtomicU64::new(1 << 63);

#[derive(Default)]
struct State {
    address: Option<String>,
    broker: Weak<BrowserBroker>,
    servers: HashMap<Uuid, Server>,
    connections: HashMap<u64, Connection>,
}

struct Connection {
    server_id: Uuid,
    binding_id: Option<Uuid>,
}

struct Server {
    activated: bool,
    token: String,
    sessions: HashMap<String, Mapping>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Mapping {
    session_id: Uuid,
    runtime_id: Uuid,
    binding_id: Uuid,
}

/// One registry per daemon. It is never written to a database or shared with
/// the GUI; only the backend and its owned OpenCode processes issue mappings.
#[derive(Default)]
pub struct BrowserTools {
    state: Mutex<State>,
}

/// A private process owns this capability for exactly its lifetime.
pub struct BrowserToolServer {
    registry: Arc<BrowserTools>,
    id: Uuid,
    address: String,
    token: String,
}

/// Driver-local context; unlike cwd this names the actual Fintwind runtime.
#[derive(Clone)]
pub struct BrowserToolRuntime {
    pub registry: Arc<BrowserTools>,
    pub session_id: Uuid,
    pub runtime_id: Uuid,
}

/// Held while a driver owns a native OpenCode session. Late drop of an old
/// binding cannot remove a replacement binding for the same native session.
pub struct BrowserToolBinding {
    server: Arc<BrowserToolServer>,
    native_session_id: String,
    mapping: Mapping,
}

impl std::fmt::Debug for BrowserToolRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserToolRuntime")
            .field("session_id", &self.session_id)
            .field("runtime_id", &self.runtime_id)
            .finish_non_exhaustive()
    }
}

impl BrowserTools {
    pub(crate) fn attach(&self, address: String, broker: &Arc<BrowserBroker>) {
        let mut state = self.state.lock();
        state.address = Some(address);
        state.broker = Arc::downgrade(broker);
    }

    /// Issued only when a private OpenCode process starts, never from a
    /// plugin-supplied value. The main daemon token is not involved.
    pub fn issue_server(self: &Arc<Self>) -> anyhow::Result<Arc<BrowserToolServer>> {
        let mut state = self.state.lock();
        let address = state
            .address
            .clone()
            .context("browser tools are not attached to a daemon")?;
        if state.servers.len() >= MAX_TOOL_SERVERS {
            bail!("too many private browser tool servers");
        }
        let id = Uuid::new_v4();
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        state.servers.insert(
            id,
            Server {
                activated: false,
                token: token.clone(),
                sessions: HashMap::new(),
            },
        );
        Ok(Arc::new(BrowserToolServer {
            registry: self.clone(),
            id,
            address,
            token,
        }))
    }

    pub(crate) fn connect(&self, token: &str) -> anyhow::Result<u64> {
        let mut state = self.state.lock();
        let server_id = state
            .servers
            .iter()
            .find_map(|(id, server)| {
                bool::from(server.token.as_bytes().ct_eq(token.as_bytes())).then_some(*id)
            })
            .context("browser tool authentication failed")?;
        let id = NEXT_TOOL_CONNECTION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| anyhow::anyhow!("browser tool connection ids exhausted"))?;
        let broker = state
            .broker
            .upgrade()
            .context("browser daemon is unavailable")?;
        // Registry -> broker is the only lock order. Revocation uses the same
        // order, so it cannot race registration into an orphan live caller.
        broker.register_connection(id);
        state
            .servers
            .get_mut(&server_id)
            .expect("matched live server")
            .activated = true;
        state.connections.insert(
            id,
            Connection {
                server_id,
                binding_id: None,
            },
        );
        Ok(id)
    }

    pub(crate) fn disconnect(&self, connection: u64) {
        let mut state = self.state.lock();
        state.connections.remove(&connection);
        if let Some(broker) = state.broker.upgrade() {
            broker.remove_connection(connection);
        }
    }

    pub(crate) fn is_connected(&self, connection: u64) -> bool {
        self.state.lock().connections.contains_key(&connection)
    }

    fn resolve(
        &self,
        connection: u64,
        native_session: &str,
    ) -> anyhow::Result<(Arc<BrowserBroker>, Mapping)> {
        if native_session.is_empty() || native_session.len() > MAX_SESSION_ID_BYTES {
            bail!("invalid OpenCode session identity");
        }
        let mut state = self.state.lock();
        let mapping = state
            .connections
            .get(&connection)
            .and_then(|entry| state.servers.get(&entry.server_id))
            .and_then(|server| server.sessions.get(native_session))
            .copied()
            .context("this OpenCode session has no Fintwind browser binding")?;
        let entry = state
            .connections
            .get_mut(&connection)
            .context("browser tool connection ended")?;
        if entry.binding_id.is_some_and(|id| id != mapping.binding_id) {
            bail!("browser tool connection cannot change session binding");
        }
        entry.binding_id = Some(mapping.binding_id);
        Ok((
            state
                .broker
                .upgrade()
                .context("browser daemon is unavailable")?,
            mapping,
        ))
    }

    pub(crate) fn list(
        &self,
        connection: u64,
        native_session: &str,
    ) -> anyhow::Result<Vec<BrowserToolPage>> {
        let (broker, mapping) = self.resolve(connection, native_session)?;
        Ok(broker
            .list_for_caller(mapping.session_id, mapping.runtime_id, connection)?
            .into_iter()
            .map(|page| BrowserToolPage {
                page_id: page.scope.page_id,
                grant_id: page.scope.grant_id,
                url: page.url,
                title: page.title,
            })
            .collect())
    }

    pub(crate) fn invoke(
        &self,
        connection: u64,
        native_session: &str,
        request_id: Uuid,
        page_id: Uuid,
        grant_id: Uuid,
        action: BrowserAction,
    ) -> BrowserResult {
        let (broker, mapping) = match self.resolve(connection, native_session) {
            Ok(bound) => bound,
            Err(error) => return BrowserResult::error(error.to_string()),
        };
        broker.invoke(
            BrowserScope {
                session_id: mapping.session_id,
                runtime_id: mapping.runtime_id,
                page_id,
                grant_id,
            },
            action,
            connection,
            request_id,
        )
    }

    /// Open a new tab for the mapped session runtime. The caller names no
    /// page: the mapping resolved from the connection's own binding decides
    /// the session and runtime, and the broker routes to that runtime's
    /// registered launcher or refuses. The URL is validated by the broker,
    /// which owns the navigation rules.
    pub(crate) fn open(
        &self,
        connection: u64,
        native_session: &str,
        request_id: Uuid,
        url: &str,
    ) -> BrowserResult {
        let (broker, mapping) = match self.resolve(connection, native_session) {
            Ok(bound) => bound,
            Err(error) => return BrowserResult::error(error.to_string()),
        };
        broker.open(
            mapping.session_id,
            mapping.runtime_id,
            url,
            connection,
            request_id,
        )
    }

    pub(crate) fn cancel(&self, connection: u64, request_id: Uuid) -> anyhow::Result<()> {
        let state = self.state.lock();
        if !state.connections.contains_key(&connection) {
            bail!("browser tool connection has ended");
        }
        state
            .broker
            .upgrade()
            .context("browser daemon is unavailable")?
            .cancel(request_id, connection)
    }
}

impl BrowserToolServer {
    pub(crate) fn is_activated(&self) -> bool {
        self.registry
            .state
            .lock()
            .servers
            .get(&self.id)
            .is_some_and(|server| server.activated)
    }
    pub(crate) fn belongs_to(&self, registry: &Arc<BrowserTools>) -> bool {
        Arc::ptr_eq(&self.registry, registry)
    }
    pub fn address(&self) -> &str {
        &self.address
    }
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Called by the private process owner even if runtime binding guards
    /// still hold references to this capability after the process exits.
    pub fn revoke(&self) {
        let mut state = self.registry.state.lock();
        state.servers.remove(&self.id);
        let connections: Vec<_> = state
            .connections
            .iter()
            .filter_map(|(connection, entry)| (entry.server_id == self.id).then_some(*connection))
            .collect();
        for connection in connections {
            state.connections.remove(&connection);
            if let Some(broker) = state.broker.upgrade() {
                broker.remove_connection(connection);
            }
        }
    }

    pub fn bind(
        self: &Arc<Self>,
        native_session: String,
        runtime: &BrowserToolRuntime,
    ) -> anyhow::Result<BrowserToolBinding> {
        if !Arc::ptr_eq(&self.registry, &runtime.registry)
            || native_session.is_empty()
            || native_session.len() > MAX_SESSION_ID_BYTES
            || runtime.session_id.is_nil()
            || runtime.runtime_id.is_nil()
        {
            bail!("invalid browser runtime binding");
        }
        let mapping = Mapping {
            session_id: runtime.session_id,
            runtime_id: runtime.runtime_id,
            binding_id: Uuid::new_v4(),
        };
        let mut state = self.registry.state.lock();
        let server = state
            .servers
            .get_mut(&self.id)
            .context("private browser tool server ended")?;
        if server.sessions.len() >= MAX_TOOL_SESSIONS
            && !server.sessions.contains_key(&native_session)
        {
            bail!("too many browser session bindings");
        }
        if let Some(previous) = server.sessions.get(&native_session)
            && previous.session_id != runtime.session_id
        {
            bail!("OpenCode session already belongs to another Fintwind session");
        }
        let previous = server.sessions.insert(native_session.clone(), mapping);
        if let Some(previous) = previous {
            retire_binding_connections(&mut state, previous.binding_id);
        }
        Ok(BrowserToolBinding {
            server: self.clone(),
            native_session_id: native_session,
            mapping,
        })
    }
}

impl Drop for BrowserToolServer {
    fn drop(&mut self) {
        self.revoke();
    }
}

impl Drop for BrowserToolBinding {
    fn drop(&mut self) {
        let mut state = self.server.registry.state.lock();
        if let Some(server) = state.servers.get_mut(&self.server.id)
            && server.sessions.get(&self.native_session_id) == Some(&self.mapping)
        {
            server.sessions.remove(&self.native_session_id);
            retire_binding_connections(&mut state, self.mapping.binding_id);
        }
    }
}

fn retire_binding_connections(state: &mut State, binding_id: Uuid) {
    let connections: Vec<_> = state
        .connections
        .iter()
        .filter_map(|(id, entry)| (entry.binding_id == Some(binding_id)).then_some(*id))
        .collect();
    for connection in connections {
        state.connections.remove(&connection);
        if let Some(broker) = state.broker.upgrade() {
            broker.remove_connection(connection);
        }
    }
}

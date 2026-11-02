//! Browser-only capabilities for Fintwind-owned OpenCode processes.
//!
//! Failures to guard against: cwd/session confusion, a model selecting another
//! runtime, stale process credentials, child-session inheritance, late work
//! after disconnect, and credentials escaping through Debug or persisted state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use anyhow::{Context as _, bail};
use crossbeam_channel::Sender;
use parking_lot::Mutex;
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::browser_broker::BrowserBroker;
use fintwind_protocol::browser::{BrowserAction, BrowserResult, BrowserScope};
use fintwind_protocol::browser_tools::{BrowserToolPage, BrowserToolReply};

const MAX_TOOL_SERVERS: usize = 32;
// Desktop subscribers count up from zero. Plugin callers use the upper half,
// so closing one transport can never retire another transport's capabilities.
static NEXT_TOOL_CONNECTION: AtomicU64 = AtomicU64::new(1 << 63);

#[derive(Default)]
struct State {
    address: Option<String>,
    broker: Weak<BrowserBroker>,
    servers: HashMap<Uuid, Server>,
    connections: HashMap<u64, Connection>,
    outgoing: HashMap<u64, Sender<BrowserToolReply>>,
}

struct Connection {
    server_id: Uuid,
}

struct Server {
    activated: bool,
    token: String,
}

/// Held while a driver owns a native OpenCode session. The lease is only
/// a lifetime guard: it carries no session binding.
pub struct BrowserToolBinding {
    _server: Arc<BrowserToolServer>,
}

/// One registry per daemon. It is never written to a database or shared with
/// the GUI; only the backend and its owned OpenCode processes issue mappings.
#[derive(Default)]
pub struct BrowserTools {
    /// Off until the settings say otherwise: browser tools cost model
    /// context, so enabling them is an explicit act.
    enabled: AtomicBool,
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

    /// Apply the browser-tools setting and push the new state to every
    /// connected plugin. A dead sender is dropped lazily here; a connection
    /// between its hello and its registration is covered by the initial
    /// state [`Self::register_outgoing`] sends.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        let mut state = self.state.lock();
        state.outgoing.retain(|_, sender| {
            sender
                .send(BrowserToolReply::ToolsState { enabled })
                .is_ok()
        });
    }

    /// Hand the transport's reply channel for one connection to the registry
    /// so setting changes can be pushed to the plugin. The current state is
    /// stated on the same lock that serializes [`Self::set_enabled`] pushes,
    /// so the plugin can never observe the two reordered.
    pub(crate) fn register_outgoing(&self, connection: u64, sender: Sender<BrowserToolReply>) {
        let enabled = self.enabled.load(Ordering::Acquire);
        let mut state = self.state.lock();
        if sender
            .send(BrowserToolReply::ToolsState { enabled })
            .is_ok()
        {
            state.outgoing.insert(connection, sender);
        }
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
        state.connections.insert(id, Connection { server_id });
        Ok(id)
    }

    pub(crate) fn disconnect(&self, connection: u64) {
        let mut state = self.state.lock();
        state.connections.remove(&connection);
        state.outgoing.remove(&connection);
        if let Some(broker) = state.broker.upgrade() {
            broker.remove_connection(connection);
        }
    }

    pub(crate) fn is_connected(&self, connection: u64) -> bool {
        self.state.lock().connections.contains_key(&connection)
    }

    fn resolve(&self, connection: u64) -> anyhow::Result<Arc<BrowserBroker>> {
        // No session binding: any tool connection may address any live
        // page by page id.
        let state = self.state.lock();
        if !state.connections.contains_key(&connection) {
            bail!("browser tool connection has ended");
        }
        state
            .broker
            .upgrade()
            .context("browser daemon is unavailable")
    }

    /// Every capability except a cancel is refused while the setting is off:
    /// the plugin already keeps the tools out of model context, and this is
    /// the enforcement point that makes a stale plugin state harmless.
    fn ensure_enabled(&self) -> anyhow::Result<()> {
        if !self.enabled.load(Ordering::Acquire) {
            bail!("browser tools are disabled in Fintwind's settings");
        }
        Ok(())
    }

    pub(crate) fn list(&self, connection: u64) -> anyhow::Result<Vec<BrowserToolPage>> {
        self.ensure_enabled()?;
        let broker = self.resolve(connection)?;
        Ok(broker
            .list_for_caller(connection)?
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
        request_id: Uuid,
        page_id: Uuid,
        grant_id: Uuid,
        action: BrowserAction,
    ) -> BrowserResult {
        if let Err(error) = self.ensure_enabled() {
            return BrowserResult::error(error.to_string());
        }
        let broker = match self.resolve(connection) {
            Ok(bound) => bound,
            Err(error) => return BrowserResult::error(error.to_string()),
        };
        broker.invoke(
            BrowserScope {
                session_id: Uuid::nil(),
                runtime_id: Uuid::nil(),
                page_id,
                grant_id,
            },
            action,
            connection,
            request_id,
        )
    }

    /// Open a new tab through the global launcher. There is no session
    /// binding: any tool connection may open.
    pub(crate) fn open(&self, connection: u64, request_id: Uuid, url: &str) -> BrowserResult {
        if let Err(error) = self.ensure_enabled() {
            return BrowserResult::error(error.to_string());
        }
        let broker = match self.resolve(connection) {
            Ok(bound) => bound,
            Err(error) => return BrowserResult::error(error.to_string()),
        };
        broker.open(url, connection, request_id)
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
        _native_session: String,
        runtime: &BrowserToolRuntime,
    ) -> anyhow::Result<BrowserToolBinding> {
        if !Arc::ptr_eq(&self.registry, &runtime.registry) {
            bail!("invalid browser runtime binding");
        }
        // No session binding: the lease is only a lifetime guard for the
        // private server. Any tool connection may address any live page.
        Ok(BrowserToolBinding {
            _server: self.clone(),
        })
    }
}

impl Drop for BrowserToolServer {
    fn drop(&mut self) {
        self.revoke();
    }
}

impl Drop for BrowserToolBinding {
    fn drop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    // The push contract the plugin depends on: a connection hears the state
    // it registered under, then every later change, in order.
    #[test]
    fn tools_state_reaches_registered_connections() {
        let tools = BrowserTools::default();
        let (sender, receiver) = unbounded();
        tools.register_outgoing(1, sender);
        assert!(matches!(
            receiver.recv(),
            Ok(BrowserToolReply::ToolsState { enabled: false })
        ));
        tools.set_enabled(true);
        assert!(matches!(
            receiver.recv(),
            Ok(BrowserToolReply::ToolsState { enabled: true })
        ));
        tools.set_enabled(false);
        assert!(matches!(
            receiver.recv(),
            Ok(BrowserToolReply::ToolsState { enabled: false })
        ));
    }
}

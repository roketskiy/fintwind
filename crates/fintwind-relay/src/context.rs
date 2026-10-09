//! Shared per-process state for the relay's HTTP and WS halves.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::config::RelayConfig;
use crate::metrics::Metrics;
use crate::ratelimit::RateLimiter;
use crate::registry::Registry;
use crate::store::Store;

pub struct RelayContext {
    pub config: RelayConfig,
    pub store: Store,
    pub registry: Registry,
    /// Registration / challenge attempt budget, process-wide (M0 rate
    /// limit table). There are no accounts to key it by.
    pub auth_limiter: RateLimiter,
    /// Aggregate-only counters for operations and privacy review. No
    /// per-subject dimension exists, so nothing here identifies anyone.
    pub metrics: Metrics,
    /// One-time challenges for registration and WS authentication.
    pub challenges: crate::rest::ChallengeBook,
    /// Browser origins allowed to call the REST endpoints and open the WS.
    /// Native clients send no Origin header and bypass this list — an
    /// Origin header is a browser promise, never an authentication.
    pub allowed_origins: Vec<String>,
    /// Connections admitted but not yet authenticated (the head was read,
    /// the WS upgrade or REST handler is running). Counted at admission
    /// and released the moment the registry takes the connection over,
    /// so this cap governs only the unauthenticated pile-up an attacker
    /// can create cheaply — never live sessions.
    pub pending_connections: std::sync::atomic::AtomicUsize,
    pub shutdown: Arc<AtomicBool>,
}

impl RelayContext {
    pub fn new(
        config: RelayConfig,
        store: Store,
        allowed_origins: Vec<String>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        let queue_ttl = config.queue_ttl;
        Self {
            auth_limiter: RateLimiter::new(
                crate::config::AUTH_ATTEMPTS_PER_HOUR * 12,
                std::time::Duration::from_secs(3600),
            ),
            metrics: Metrics::default(),
            challenges: crate::rest::ChallengeBook::new(),
            config,
            store,
            registry: Registry::with_queue_ttl(queue_ttl),
            allowed_origins,
            pending_connections: std::sync::atomic::AtomicUsize::new(0),
            shutdown,
        }
    }

    /// Admission slot check for a new connection. Registered (already
    /// authenticated) connections are separate: the cap that matters here
    /// is the unauthenticated pile-up an attacker can create cheaply.
    pub fn has_admission_slot(&self) -> bool {
        use std::sync::atomic::Ordering;
        let pending = self.pending_connections.load(Ordering::Acquire);
        pending < crate::config::MAX_UNAUTHENTICATED_CONNECTIONS
    }

    /// Browser-origin check: a present Origin must be whitelisted. Absent
    /// origins pass (native clients); the CSRF surface this defends is the
    /// browser one, which is exactly where Origin exists.
    pub fn origin_allowed(&self, origin: Option<&str>) -> bool {
        match origin {
            None => true,
            Some(origin) => self.allowed_origins.iter().any(|allowed| allowed == origin),
        }
    }
}

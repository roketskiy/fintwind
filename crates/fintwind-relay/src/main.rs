//! Fintwind remote-control relay.
//!
//! The relay is a deliberately narrow public service: it authenticates
//! hosts and devices by challenge-signature, routes opaque ciphertext
//! between them, coordinates pairing, and enforces resource limits. It
//! never sees business payloads — everything rides inside `Frame::Sealed`
//! end-to-end encryption — and it never persists one (queues are
//! in-memory and bounded; the SQLite store holds public keys, pairing
//! links, and nonce hashes only). There are no accounts and no passwords
//! anywhere (decision revision, M0 freeze document).
//!
//! Deployment shape: this process binds loopback only; Caddy terminates
//! public TLS for `relay.fintwind.xyz` and proxies to it.

mod config;
mod context;
mod http;
mod ratelimit;
mod registry;
mod rest;
mod store;
mod ws;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use config::RelayConfig;

fn main() -> anyhow::Result<()> {
    let config = RelayConfig::parse(std::env::args().skip(1))?;
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let store = store::Store::open(&config.database)?;
    log::info!(target: "audit", "fintwind-relay starting; public url {}", config.public_url);
    let allowed_origins = std::env::var("FINTWIND_RELAY_ORIGINS")
        .unwrap_or_else(|_| "https://remote.fintwind.xyz,https://fintwind.xyz".to_owned())
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(str::to_owned)
        .collect();
    let shutdown = Arc::new(AtomicBool::new(false));
    let context = Arc::new(context::RelayContext::new(
        config,
        store,
        allowed_origins,
        Arc::clone(&shutdown),
    ));
    let listener = std::net::TcpListener::bind(&context.config.bind)?;
    // Lifecycle note: no in-process signal handler. systemd owns the
    // process; a SIGTERM kill is safe because the store runs in WAL mode
    // and queues are in-memory by design.
    // Background sweeper: nonce expiry and rate-limit windows need a slow
    // clock, not per-request work.
    {
        let sweeper_context = Arc::clone(&context);
        std::thread::Builder::new()
            .name("fintwind-relay-sweep".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
                if sweeper_context.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if let Err(error) = sweeper_context.store.sweep() {
                    log::warn!(target: "audit", "sweep failed: {error}");
                }
                sweeper_context.auth_limiter.sweep();
            })?;
    }
    http::serve(listener, context)
}

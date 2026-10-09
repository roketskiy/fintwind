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
mod metrics;
mod ratelimit;
mod registry;
mod rest;
mod store;
mod ws;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use config::RelayConfig;

fn main() -> anyhow::Result<()> {
    let config = RelayConfig::parse(std::env::args().skip(1))?;
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let store = store::Store::open(&config.database)?;
    // Output hygiene: the only stdout line this process ever writes is
    // the structured ready signal below; every log goes to stderr, and
    // the deployment URL stays out of the process output entirely.
    log::info!(target: "audit", "fintwind-relay starting");
    // Allowed browser origins, exact strings only — never a wildcard.
    // These are the *page* origins of the PWA, which is a different host
    // from this relay's own API endpoint; the default mirrors the
    // deployment unit so an unconfigured start is not silently open.
    // Native clients send no Origin and never consult this list.
    let allowed_origins = std::env::var("FINTWIND_RELAY_ORIGINS")
        .unwrap_or_else(|_| {
            "https://remote.fintwind.xyz,https://fintwind-remote.pages.dev".to_owned()
        })
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
    // Background sweeper: nonce expiry, rate-limit windows, and expired
    // offline queues need a slow clock, not per-request work. The queue
    // sweep is what makes TTL reclaim a property of time: without it an
    // abandoned queue stays resident until the process ends.
    {
        let sweeper_context = Arc::clone(&context);
        std::thread::Builder::new()
            .name("fintwind-relay-sweep".into())
            .spawn(move || {
                // A 5 s tick keeps every reclaim prompt without per-request
                // work: each pass is one shutdown read plus two elapsed
                // comparisons, and the real sweeps stay on their slower
                // clocks.
                let mut last_queue_sweep = std::time::Instant::now();
                let mut last_store_sweep = std::time::Instant::now();
                loop {
                    std::thread::sleep(crate::config::SWEEP_TICK);
                    if sweeper_context.shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    if last_store_sweep.elapsed() >= crate::config::STORE_SWEEP_INTERVAL {
                        last_store_sweep = std::time::Instant::now();
                        if let Err(error) = sweeper_context.store.sweep() {
                            log::warn!(target: "audit", "sweep failed: {error}");
                        }
                        sweeper_context.auth_limiter.sweep();
                    }
                    // Expired offline frames leave on their own clock, so a
                    // device that never comes back does not keep its
                    // ciphertext resident for the life of the process.
                    if last_queue_sweep.elapsed() >= sweeper_context.config.queue_sweep_interval {
                        last_queue_sweep = std::time::Instant::now();
                        let dropped = sweeper_context.registry.sweep_queues();
                        if dropped > 0 {
                            log::info!(target: "audit", "expired offline frames dropped; frames={dropped}");
                        }
                    }
                }
            })?;
    }
    // The structured ready signal, the one and only stdout line: the
    // bound address comes from the listener itself (kernel-assigned
    // under `--bind 127.0.0.1:0`), never from the configured port or a
    // log string. A failed write or flush is a failed start — the
    // process refuses to serve something its launcher cannot see.
    let bound = listener.local_addr()?;
    {
        use std::io::Write;
        let ready = format!(
            "{{\"type\":\"relayReady\",\"boundAddress\":\"{bound}\",\"pid\":{}}}\n",
            std::process::id()
        );
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(ready.as_bytes())?;
        stdout.flush()?;
    }
    http::serve(listener, context)
}

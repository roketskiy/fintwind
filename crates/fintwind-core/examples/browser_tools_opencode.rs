//! Phase-three E2E daemon: the real Fintwind daemon (persistence, settings,
//! the OpenCode driver, the browser broker and the restricted browser-tool
//! registry) on a loopback port, wired for a behavioral run of the OpenCode
//! browser tools.
//!
//! Unlike the phase-two `browser_bridge_e2e`, this host runs the production
//! `fintwind_core::daemon::FintwindBackend` so a `Command::Start` genuinely
//! launches `opencode serve` through the browser-instrumenting pool, the
//! plugin really loads, and the real tool-execution context issues
//! `list`/`invoke`/`cancel` over the private `/v1/browser-tools` socket. The
//! only things faked are outside Fintwind's control and live in the runner's
//! fixtures: the *model provider* (a loopback OpenAI-compatible stub that emits
//! deterministic tool calls) and, when the run opts into it, the *GUI* (a
//! protocol owner that publishes pages and answers actions instead of a native
//! WebView2 host).
//!
//! # Failure modes this host is built to surface
//!
//! - A plugin not loading while ordinary chat starts in degraded mode: only
//!   a real tool round-trip proves activation; a healthy Start is insufficient.
//! - Cross-session scope confusion, unknown sessions, cancellation races, and
//!   stale process credentials — all the real `browser_tools` paths, exercised
//!   through the real driver and plugin.
//!
//! # Isolation
//!
//! State, settings and the OpenCode plugin directory all live under
//! `FINTWIND_BROWSER_TOOLS_E2E_STATE`. The OpenCode child inherits the runner's
//! isolated `OPENCODE_CONFIG_DIR`/`XDG_*` and the injected
//! `OPENCODE_CONFIG_CONTENT`, so no user configuration or database is read or
//! written. Everything binds `127.0.0.1`.
//!
//! # Contract with the runner
//!
//! Exactly one JSON line on stdout when the listener is bound:
//!
//! ```json
//! {"kind":"browser-tools-opencode","address":"127.0.0.1:PORT","token":"...","protocolVersion":N}
//! ```
//!
//! Everything else goes to stderr. The daemon speaks the ordinary desktop
//! protocol on `/v1`; the plugin reaches it on `/v1/browser-tools`.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use fintwind_core::daemon::FintwindBackend;
use fintwind_core::persistence::StateStore;
use fintwind_core::{Backend, DaemonSettingsStore, ServerOptions, serve};
use fintwind_protocol::PROTOCOL_VERSION;
use uuid::Uuid;

fn backend_for(state_dir: &PathBuf) -> anyhow::Result<Arc<dyn Backend>> {
    let settings = DaemonSettingsStore::open(state_dir.join("settings.json"))
        .context("could not open the isolated settings store")?;
    let task_store = StateStore::daemon(state_dir.join("app.db"));
    let backend =
        FintwindBackend::new(settings, task_store).context("could not build the E2E backend")?;
    Ok(Arc::new(backend))
}

fn main() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").context("could not bind the e2e listener")?;
    let address = listener
        .local_addr()
        .context("could not read the e2e listener address")?;
    let token = Uuid::new_v4().simple().to_string();

    // The runner owns the state directory (under its per-run output) so the
    // daemon writes only there. Fall back to a fresh temp dir when run by hand.
    let state_dir = match std::env::var_os("FINTWIND_BROWSER_TOOLS_E2E_STATE") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => std::env::temp_dir().join(format!("fintwind-browser-tools-e2e-{}", Uuid::new_v4())),
    };
    std::fs::create_dir_all(&state_dir).context("could not create the E2E state directory")?;
    eprintln!(
        "[browser-tools-opencode] state directory {}",
        state_dir.display()
    );

    let backend = backend_for(&state_dir)?;
    let shutdown = Arc::new(AtomicBool::new(false));

    // A runner that exits without a graceful shutdown leaves this process
    // behind; the runner kills the exact pid it spawned as a fallback, and
    // this idle deadline keeps a forgotten run from lingering forever.
    let reaper = shutdown.clone();
    std::thread::Builder::new()
        .name("browser-tools-opencode-reaper".into())
        .spawn(move || {
            let deadline = Duration::from_secs(900);
            let step = Duration::from_millis(250);
            let mut waited = Duration::ZERO;
            while waited < deadline {
                if reaper.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(step);
                waited += step;
            }
            eprintln!("[browser-tools-opencode] idle deadline reached; exiting");
            std::process::exit(0);
        })
        .context("could not start the e2e reaper thread")?;

    // The one ready line. stdout carries only this; the token is parsed in
    // memory by the runner and never persisted into an artifact.
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "kind": "browser-tools-opencode",
            "address": address.to_string(),
            "token": token,
            "protocolVersion": PROTOCOL_VERSION,
        }))
        .context("could not serialize the ready line")?
    );
    use std::io::Write as _;
    std::io::stdout().flush().ok();

    // `allow_shutdown` lets the runner stop this process through the ordinary
    // protocol instead of only by killing it.
    serve(
        listener,
        token,
        backend,
        shutdown,
        ServerOptions {
            allow_shutdown: true,
        },
    )
}

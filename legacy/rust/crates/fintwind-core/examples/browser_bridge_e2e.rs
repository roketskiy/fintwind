//! Phase-two E2E daemon: the real Fintwind server, bound to a loopback port,
//! driving the real browser broker with a minimal in-memory backend.
//!
//! This is not a mock of the bridge. It runs
//! [`fintwind_core::server::serve`] exactly like `fintwind-daemon` does, so
//! the handshake, the per-session runtime mailboxes, the response cache, the
//! event hub and — the point of the exercise — the real
//! [`fintwind_core::browser_broker::BrowserBroker`] are the production code
//! paths. The only thing faked is the *provider runtime*: `Command::Start`
//! registers a runtime and returns a payload without launching OpenCode,
//! reading the app database, or touching the network.
//!
//! # Failure modes this host is built to surface
//!
//! - **Unshared invoke**: a `BrowserInvoke` for a page nobody published must
//!   be refused with an error, so a stray client cannot make a page act.
//! - **Scope confusion**: the outer request's session/runtime must match the
//!   scope's, and the broker refuses nil ids, foreign sessions and superseded
//!   grants — a request addressed to one session can never be served by
//!   another session's page.
//! - **Cross-connection spoofing**: only the connection that published a page
//!   may answer or cancel its requests.
//! - **Replay**: browser traffic is live-only. A reconnecting client replays
//!   nothing and keeps no lease; a replaced runtime drops the old grants.
//! - **Half-open shutdown**: `Shutdown` ends the process; the runner also
//!   kills the exact pid it spawned.
//!
//! # Contract with the runner
//!
//! Exactly one line on stdout when the listener is bound:
//!
//! ```json
//! {"kind":"browser-bridge-e2e","address":"127.0.0.1:PORT","token":"…","protocolVersion":N}
//! ```
//!
//! Response payloads are the production shapes:
//!
//! - `Command::Start` -> `ResponsePayload::Started` (runtime registered).
//! - `Command::BrowserList` -> `ResponsePayload::Json` holding a JSON array of
//!   `BrowserShare`.
//! - `Command::BrowserInvoke` -> `ResponsePayload::Json` holding a serialized
//!   `BrowserResult` (`{"kind":"ok","value":…}` / `{"kind":"error","message":…}`).
//!
//! Everything else on stderr.

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, bail};
// The crate root re-exports the real server API (`pub use server::{...}`), so
// this example drives the production `serve` and `Backend` even though the
// module itself stays private to the crate.
use fintwind_core::{Backend, EventSink, Request, ResponsePayload, ServerOptions, serve};
use fintwind_protocol::{Command, PROTOCOL_VERSION};
use uuid::Uuid;

/// The one backend the E2E needs. Browser commands never reach it — the
/// server answers those through the broker — so it only has to activate a
/// runtime without starting a provider.
struct E2EBackend;

impl Backend for E2EBackend {
    fn handle(&self, request: Request, _events: EventSink) -> anyhow::Result<ResponsePayload> {
        match &request.command {
            // The runtime is registered by the server before this call; this
            // payload only has to be `Ok` for the registration to survive.
            Command::Start { .. } => Ok(ResponsePayload::Started {
                supports_steer: false,
            }),
            other => bail!("the e2e backend does not implement {other:?}"),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").context("could not bind the e2e listener")?;
    let address = listener
        .local_addr()
        .context("could not read the e2e listener address")?;
    let token = Uuid::new_v4().simple().to_string();
    let backend: Arc<dyn Backend> = Arc::new(E2EBackend);
    let shutdown = Arc::new(AtomicBool::new(false));

    // A runner that exits without a graceful shutdown leaves this process
    // behind; the runner kills the exact pid it spawned as a fallback, and
    // this idle deadline keeps a forgotten run from lingering forever.
    let reaper = shutdown.clone();
    std::thread::Builder::new()
        .name("browser-bridge-e2e-reaper".into())
        .spawn(move || {
            let deadline = Duration::from_secs(600);
            let step = Duration::from_millis(250);
            let mut waited = Duration::ZERO;
            while waited < deadline {
                if reaper.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(step);
                waited += step;
            }
            eprintln!("[browser-bridge-e2e] idle deadline reached; exiting");
            std::process::exit(0);
        })
        .context("could not start the e2e reaper thread")?;

    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "kind": "browser-bridge-e2e",
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

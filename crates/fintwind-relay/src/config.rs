//! Relay configuration and resource limits.
//!
//! Every number here is an M0 resource-table initial value
//! ([remote-control-m0-freeze.md]). The relay refuses load beyond these
//! limits instead of degrading silently — hitting a limit is a visible
//! rejection, never truncation. Values are frozen for external trial only
//! after isolated-load verification; changing one is a documented change.
//!
//! [remote-control-m0-freeze.md]: ../../docs/remote-control-m0-freeze.md

use std::path::PathBuf;
use std::time::Duration;

/// Default public URL handed to pairing QRs and host enroll. Override with
/// `--public-url` for isolated test deployments.
pub const DEFAULT_PUBLIC_URL: &str = "https://relay.fintwind.xyz";

/// Wire frame bound, ciphertext included. Mirrors
/// `fintwind_remote_protocol::MAX_FRAME_BYTES`; enforced before parsing.
pub const MAX_FRAME_BYTES: usize = fintwind_remote_protocol::MAX_FRAME_BYTES;

/// Per-device undelivered ciphertext queue: message count cap. A full
/// queue rejects the frame with `QueueFull`; it never truncates silently.
pub const QUEUE_MAX_MESSAGES: usize = 4096;

/// Per-device undelivered ciphertext queue: byte cap.
pub const QUEUE_MAX_BYTES: usize = 32 * 1024 * 1024;

/// How long an undelivered sealed frame stays queued before the relay
/// drops it and tells the sender the route is gone. Deliberately equal to
/// the host-side pending-input TTL so both ends age out together.
pub const QUEUE_TTL: Duration = Duration::from_secs(10 * 60);

/// Unauthenticated connections: how long they may run the auth flow.
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(10);

/// Unauthenticated connections accepted at once, process-wide.
pub const MAX_UNAUTHENTICATED_CONNECTIONS: usize = 64;

/// Authenticated connections accepted at once, process-wide. Phase one is
/// a handful of hosts and devices; the cap exists so exhaustion is a clean
/// rejection, not a memory cliff.
pub const MAX_AUTHENTICATED_CONNECTIONS: usize = 64;

/// Frames per connection per window before `RateLimited`. A burst allowance
/// keeps a transcript replay and a live stream flowing; sustained excess is
/// refused, not buffered.
pub const FRAME_RATE_LIMIT: u32 = 200;
pub const FRAME_RATE_WINDOW: Duration = Duration::from_secs(1);

/// Login / enroll / device-registration attempts per account per hour.
pub const AUTH_ATTEMPTS_PER_HOUR: u32 = 5;

/// Authenticated connections with no frame at all for this long are closed
/// (heartbeat candidates: ping 10s / pong timeout 60s live at the endpoint
/// layer; the relay only enforces total silence).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Pairing nonce lifetime at the relay. The host consumes atomically too;
/// this is the additional defense in depth.
pub const PAIRING_NONCE_TTL: Duration = Duration::from_secs(5 * 60);

/// Command-line configuration for one relay process.
#[derive(Clone, Debug)]
pub struct RelayConfig {
    /// WS + REST bind address. The deployment keeps this on loopback;
    /// Caddy terminates public TLS and proxies to it.
    pub bind: String,
    /// SQLite identity store path.
    pub database: PathBuf,
    /// Public URL embedded in pairing QRs.
    pub public_url: String,
}

impl RelayConfig {
    /// Parse command-line arguments. Kept hand-rolled to match the
    /// `fintwind-daemon` argument style; the surface is four flags.
    pub fn parse(arguments: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut bind: Option<String> = None;
        let mut database: Option<PathBuf> = None;
        let mut public_url: Option<String> = None;
        let mut iter = arguments.into_iter();
        while let Some(argument) = iter.next() {
            let mut value = |name: &str| {
                iter.next()
                    .ok_or_else(|| anyhow::anyhow!("{name} requires a value"))
            };
            match argument.as_str() {
                "--bind" => bind = Some(value("--bind")?),
                "--db" => database = Some(PathBuf::from(value("--db")?)),
                "--public-url" => public_url = Some(value("--public-url")?),
                other => anyhow::bail!("unknown argument {other:?}"),
            }
        }
        Ok(Self {
            bind: bind.unwrap_or_else(|| "127.0.0.1:8090".to_owned()),
            database: database.unwrap_or_else(|| PathBuf::from("fintwind-relay.sqlite3")),
            public_url: public_url.unwrap_or_else(|| DEFAULT_PUBLIC_URL.to_owned()),
        })
    }
}

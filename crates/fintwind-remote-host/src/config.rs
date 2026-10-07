//! Host daemon configuration and timing constants.
//!
//! Timing values come from the M0 freeze (remote-control-m0-freeze.md § 2)
//! and the ZCode-derived transport numbers in the implementation plan:
//! reconnect backoff is a literal array, ack timeout and buffer grace are
//! the measured PersistentProtocol values, heartbeat ping/pong are the
//! architecture document's candidates.
//!
//! There are no credentials in this configuration (decision revision,
//! M0 freeze document): identity lives in key files under the data
//! directory, registration is automatic, and nothing is ever typed in.

use std::path::PathBuf;
use std::time::Duration;

/// Per-connection reconnect delays in seconds; past the last entry the
/// link is treated as down until the next scheduled attempt cycle resets.
pub const RECONNECT_DELAYS_SECONDS: &[u64] = &[1, 2, 4, 8, 15, 30, 60];

/// A frame sent to the device is dead if no end-to-end ack arrives in
/// this window: the connection is torn down and rebuilt (ZCode
/// PersistentProtocol `ACK_TIMEOUT`).
pub const ACK_TIMEOUT: Duration = Duration::from_secs(20);

/// If the oldest unacked frame has waited longer than this, the session
/// is abandoned outright instead of limping (ZCode
/// `replayBufferGraceMs`). Events recover through cursor resume, not
/// through cross-key-period replay.
pub const REPLAY_BUFFER_GRACE: Duration = Duration::from_secs(45);

/// Host-to-relay keepalive cadence and the silence budget before the
/// link is declared dead (architecture document candidate values).
pub const RELAY_PING_INTERVAL: Duration = Duration::from_secs(10);
pub const RELAY_PONG_TIMEOUT: Duration = Duration::from_secs(60);

/// The device-side approval / user-input offer lifetime (M0 table).
pub const APPROVAL_TTL: Duration = Duration::from_secs(5 * 60);

/// Host daemon configuration, parsed from flags. The daemon bearer token
/// comes from the environment so it never sits in a process listing.
#[derive(Clone, Debug)]
pub struct HostConfig {
    /// Relay origin for REST (`https://relay.fintwind.xyz`); the WSS url
    /// derives from it.
    pub relay_origin: String,
    /// Loopback address of the local fintwind-daemon.
    pub daemon_address: String,
    /// Daemon bearer token (read from this environment variable).
    pub daemon_token_env: String,
    /// State directory: identity keys, host id, dedup store.
    pub data_dir: PathBuf,
    /// The paired-device grant file. No grant file means no device is
    /// trusted yet: the host connects and waits, which is the correct
    /// pre-pairing posture.
    pub paired_device_file: PathBuf,
}

impl HostConfig {
    pub fn parse(arguments: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut relay_origin = None;
        let mut daemon_address = None;
        let mut daemon_token_env = None;
        let mut data_dir = None;
        let mut paired_device_file = None;
        let mut iter = arguments.into_iter();
        while let Some(argument) = iter.next() {
            let mut value = |name: &str| {
                iter.next()
                    .ok_or_else(|| anyhow::anyhow!("{name} requires a value"))
            };
            match argument.as_str() {
                "--relay-origin" => relay_origin = Some(value("--relay-origin")?),
                "--daemon-address" => daemon_address = Some(value("--daemon-address")?),
                "--daemon-token-env" => daemon_token_env = Some(value("--daemon-token-env")?),
                "--data-dir" => data_dir = Some(PathBuf::from(value("--data-dir")?)),
                "--paired-device" => paired_device_file = Some(PathBuf::from(value("--paired-device")?)),
                other => anyhow::bail!("unknown argument {other:?}"),
            }
        }
        let data_dir = data_dir.unwrap_or_else(|| PathBuf::from("."));
        Ok(Self {
            relay_origin: relay_origin
                .unwrap_or_else(|| "https://relay.fintwind.xyz".to_owned()),
            daemon_address: daemon_address.unwrap_or_else(|| "127.0.0.1:0".to_owned()),
            daemon_token_env: daemon_token_env
                .ok_or_else(|| anyhow::anyhow!("--daemon-token-env is required"))?
                .to_owned(),
            paired_device_file: paired_device_file
                .unwrap_or_else(|| data_dir.join("paired-device.json")),
            data_dir,
        })
    }

    /// The WSS endpoint derived from the REST origin.
    pub fn relay_ws_url(&self) -> String {
        let trimmed = self.relay_origin.trim_end_matches('/');
        let ws = trimmed
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        format!("{ws}/ws")
    }
}

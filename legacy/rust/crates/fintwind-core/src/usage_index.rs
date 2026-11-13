//! Persistent usage facts, source-scoped snapshots and single-flight refresh.
//! No provider or price-network request is made on the snapshot read path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::driver::native::{self, SessionSplit};
use fintwind_protocol::provider_session::UsageStats;

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Provider boundary used by the incremental scanner and black-box harness.
pub trait UsageSource: Send + Sync {
    fn request(&self, path: &str) -> anyhow::Result<Value>;
}

impl UsageSource for crate::opencode_pool::PooledServer {
    fn request(&self, path: &str) -> anyhow::Result<Value> {
        self.request_with_timeout("GET", path, None, Duration::from_secs(15))
    }
}

struct Slot {
    snapshot: UsageStats,
    checked: Option<Instant>,
    running: bool,
}

/// One instance belongs to a daemon and uses that daemon's isolated data path.
pub struct UsageIndex {
    path: PathBuf,
    slots: Mutex<HashMap<String, Arc<Mutex<Slot>>>>,
}

impl UsageIndex {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// The factory is invoked only by the owned worker, never for a cache read.
    pub fn snapshot(
        &self,
        source: String,
        force_refresh: bool,
        factory: impl FnOnce() -> anyhow::Result<Arc<dyn UsageSource>> + Send + 'static,
    ) -> anyhow::Result<UsageStats> {
        let slot = {
            let mut slots = self.slots.lock();
            if let Some(slot) = slots.get(&source) {
                slot.clone()
            } else {
                let cache = UsageCache::open(&self.path, &source)?;
                let mut snapshot = cache.snapshot()?.unwrap_or_default();
                snapshot.refreshing = false;
                let slot = Arc::new(Mutex::new(Slot {
                    snapshot,
                    checked: None,
                    running: false,
                }));
                slots.insert(source.clone(), slot.clone());
                slot
            }
        };
        let mut state = slot.lock();
        if !state.running
            && (force_refresh
                || state
                    .checked
                    .is_none_or(|at| at.elapsed() >= REFRESH_INTERVAL))
        {
            state.running = true;
            state.snapshot.refreshing = true;
            state.snapshot.refresh_error = None;
            let path = self.path.clone();
            let worker_slot = slot.clone();
            let spawned = std::thread::Builder::new()
                .name("usage-index".into())
                .spawn(move || {
                    let started = Instant::now();
                    let result = (|| {
                        let cache = UsageCache::open(&path, &source)?;
                        let provider = factory()?;
                        let mut stats = native::fetch_usage_stats(
                            &|path| provider.request(path),
                            &cache,
                            &|snapshot| {
                                worker_slot.lock().snapshot = snapshot;
                            },
                        )?;
                        stats.refreshing = false;
                        cache.save_snapshot(&stats)?;
                        anyhow::Ok(stats)
                    })();
                    let mut state = worker_slot.lock();
                    match result {
                        Ok(stats) => state.snapshot = stats,
                        Err(error) => state.snapshot.refresh_error = Some(error.to_string()),
                    }
                    state.snapshot.refreshing = false;
                    state.running = false;
                    state.checked = Some(Instant::now());
                    eprintln!(
                        "usage index refresh: {} ms, {} sessions",
                        started.elapsed().as_millis(),
                        state.snapshot.sessions_scanned
                    );
                });
            if let Err(error) = spawned {
                state.running = false;
                state.snapshot.refreshing = false;
                state.snapshot.refresh_error = Some(error.to_string());
            }
        }
        Ok(state.snapshot.clone())
    }
}

/// Binary identity plus inherited provider storage environment; never project
/// directory or ephemeral server port. Data directories isolate release/debug.
pub(crate) fn source_key(binary: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(
        binary
            .canonicalize()
            .unwrap_or_else(|_| binary.to_owned())
            .to_string_lossy()
            .as_bytes(),
    );
    for name in [
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        digest.update([0]);
        digest.update(name.as_bytes());
        digest.update(
            std::env::var_os(name)
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        );
    }
    format!("{:x}", digest.finalize())
}

pub(crate) struct UsageCache {
    connection: Mutex<Connection>,
    source: String,
}

impl UsageCache {
    fn open(path: &Path, source: &str) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path).context("open usage index")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS usage_splits_v1 (
               source TEXT NOT NULL, session TEXT NOT NULL, fingerprint TEXT NOT NULL,
               checked INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(source,session));
             CREATE TABLE IF NOT EXISTS usage_snapshots_v1 (
               source TEXT PRIMARY KEY, payload TEXT NOT NULL);",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
            source: source.into(),
        })
    }

    fn snapshot(&self) -> anyhow::Result<Option<UsageStats>> {
        let payload: Option<String> = self
            .connection
            .lock()
            .query_row(
                "SELECT payload FROM usage_snapshots_v1 WHERE source=?",
                [&self.source],
                |row| row.get(0),
            )
            .optional()?;
        payload
            .map(|payload| serde_json::from_str(&payload).context("decode usage snapshot"))
            .transpose()
    }

    pub(crate) fn save_snapshot(&self, stats: &UsageStats) -> anyhow::Result<()> {
        let payload = serde_json::to_string(stats)?;
        self.connection.lock().execute(
            "INSERT OR REPLACE INTO usage_snapshots_v1 VALUES (?,?)",
            params![self.source, payload],
        )?;
        Ok(())
    }

    pub(crate) fn split(
        &self,
        id: &str,
        fingerprint: &str,
        recent: bool,
    ) -> anyhow::Result<Option<SessionSplit>> {
        let row: Option<(String, u64)> = self.connection.lock().query_row(
            "SELECT payload,checked FROM usage_splits_v1 WHERE source=? AND session=? AND fingerprint=?",
            params![self.source, id, fingerprint], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        match row {
            // Recently active sessions are periodically reconciled even if the
            // provider amended a message without changing the session summary.
            Some((payload, checked))
                if !recent || crate::model::unix_time().saturating_sub(checked) < 300 =>
            {
                Ok(Some(serde_json::from_str(&payload)?))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn previous_split(&self, id: &str) -> anyhow::Result<Option<SessionSplit>> {
        let payload: Option<String> = self
            .connection
            .lock()
            .query_row(
                "SELECT payload FROM usage_splits_v1 WHERE source=? AND session=?",
                params![self.source, id],
                |row| row.get(0),
            )
            .optional()?;
        payload
            .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
            .transpose()
    }

    pub(crate) fn save_split(
        &self,
        id: &str,
        fingerprint: &str,
        split: &SessionSplit,
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_string(split)?;
        self.connection.lock().execute(
            "INSERT OR REPLACE INTO usage_splits_v1 VALUES (?,?,?,?,?)",
            params![
                self.source,
                id,
                fingerprint,
                crate::model::unix_time(),
                payload
            ],
        )?;
        Ok(())
    }

    pub(crate) fn retain(&self, seen: &std::collections::HashSet<String>) -> anyhow::Result<()> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let ids = {
            let mut statement =
                transaction.prepare("SELECT session FROM usage_splits_v1 WHERE source=?")?;
            statement
                .query_map([&self.source], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for id in ids.into_iter().filter(|id| !seen.contains(id)) {
            transaction.execute(
                "DELETE FROM usage_splits_v1 WHERE source=? AND session=?",
                params![self.source, id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

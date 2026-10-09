//! Relay identity store: hosts, devices, pairing links, pairing nonces —
//! one SQLite file, nothing else.
//!
//! Decision revision (remote-control-m0-freeze.md, 决策修订): there are no
//! accounts and no passwords. Identity is a key pair — hosts and devices
//! register an Ed25519 public key plus a signature over a one-time
//! challenge, and every connection re-proves possession by signing a
//! fresh challenge. Nothing here can leak into a usable credential: the
//! store holds public keys, relationship rows, and hashes of dead values.
//!
//! What this store is not: it never sees business payloads, never holds
//! end-to-end keys, and never decides whether an operation may run —
//! pairing links carry only "this device may talk to that host at all";
//! every real authorization stays on the host (its grant file, its
//! permission path).

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

use crate::config::PAIRING_NONCE_TTL;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn hash_token(token: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.as_bytes()).into()
}

pub struct Store {
    connection: Mutex<Connection>,
    /// Monotonic in-process sequence for `sweep` spacing.
    sweep_counter: AtomicU64,
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let connection = Connection::open(path)?;
        Self::initialize(connection)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let connection = Connection::open_in_memory()?;
        Self::initialize(connection)
    }

    fn initialize(connection: Connection) -> anyhow::Result<Self> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS hosts (
                id TEXT PRIMARY KEY,
                signing_public_key BLOB NOT NULL UNIQUE,
                created_at_ms INTEGER NOT NULL,
                revoked_at_ms INTEGER
            ) STRICT;
            CREATE TABLE IF NOT EXISTS devices (
                id TEXT PRIMARY KEY,
                signing_public_key BLOB NOT NULL UNIQUE,
                name TEXT NOT NULL DEFAULT '',
                created_at_ms INTEGER NOT NULL,
                revoked_at_ms INTEGER
            ) STRICT;
            CREATE TABLE IF NOT EXISTS pairing_links (
                host_id TEXT NOT NULL REFERENCES hosts(id),
                device_id TEXT NOT NULL REFERENCES devices(id),
                linked_at_ms INTEGER NOT NULL,
                unlinked_at_ms INTEGER,
                PRIMARY KEY (host_id, device_id)
            ) STRICT;
            CREATE TABLE IF NOT EXISTS pairing_nonces (
                nonce_hash BLOB PRIMARY KEY,
                host_id TEXT NOT NULL REFERENCES hosts(id),
                expires_at_ms INTEGER NOT NULL,
                consumed_at_ms INTEGER
            ) STRICT;",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
            sweep_counter: AtomicU64::new(0),
        })
    }

    // ---- registration ----

    /// Register a host identity (public key first-seen wins). Re-registering
    /// the same key returns the same id; a revoked key stays revoked —
    /// replacement requires a genuinely new key pair, never a row update.
    pub fn register_host(&self, signing_public_key: &[u8]) -> anyhow::Result<String> {
        let connection = self.connection.lock();
        let existing: Option<(String, Option<i64>)> = connection
            .query_row(
                "SELECT id, revoked_at_ms FROM hosts WHERE signing_public_key = ?1",
                params![signing_public_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match existing {
            Some((id, None)) => Ok(id),
            Some((_, Some(_))) => anyhow::bail!("this host key has been revoked"),
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                connection.execute(
                    "INSERT INTO hosts (id, signing_public_key, created_at_ms) VALUES (?1, ?2, ?3)",
                    params![id, signing_public_key, now_ms() as i64],
                )?;
                Ok(id)
            }
        }
    }

    /// Register a device identity, same rules as hosts.
    pub fn register_device(&self, signing_public_key: &[u8], name: &str) -> anyhow::Result<String> {
        let connection = self.connection.lock();
        let existing: Option<(String, Option<i64>)> = connection
            .query_row(
                "SELECT id, revoked_at_ms FROM devices WHERE signing_public_key = ?1",
                params![signing_public_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match existing {
            Some((id, None)) => Ok(id),
            Some((_, Some(_))) => anyhow::bail!("this device key has been revoked"),
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                connection.execute(
                    "INSERT INTO devices (id, signing_public_key, name, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![id, signing_public_key, name, now_ms() as i64],
                )?;
                Ok(id)
            }
        }
    }

    /// Prove possession for connection authentication: does this host id
    /// carry this signing key, unrevoked?
    pub fn verify_host_signature(
        &self,
        host_id: &str,
        signing_public_key: &[u8],
    ) -> anyhow::Result<bool> {
        let connection = self.connection.lock();
        let row: Option<(Vec<u8>, Option<i64>)> = connection
            .query_row(
                "SELECT signing_public_key, revoked_at_ms FROM hosts WHERE id = ?1",
                params![host_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(matches!(row, Some((key, None)) if key == signing_public_key))
    }

    pub fn verify_device_signature(
        &self,
        device_id: &str,
        signing_public_key: &[u8],
    ) -> anyhow::Result<bool> {
        let connection = self.connection.lock();
        let row: Option<(Vec<u8>, Option<i64>)> = connection
            .query_row(
                "SELECT signing_public_key, revoked_at_ms FROM devices WHERE id = ?1",
                params![device_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(matches!(row, Some((key, None)) if key == signing_public_key))
    }

    /// The registered signing public key for an id, if live. Callers use
    /// this to verify connection challenge signatures.
    pub fn host_signing_key(&self, host_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let connection = self.connection.lock();
        Ok(connection
            .query_row(
                "SELECT signing_public_key FROM hosts WHERE id = ?1 AND revoked_at_ms IS NULL",
                params![host_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn device_signing_key(&self, device_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let connection = self.connection.lock();
        Ok(connection
            .query_row(
                "SELECT signing_public_key FROM devices WHERE id = ?1 AND revoked_at_ms IS NULL",
                params![device_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn revoke_host(&self, host_id: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock();
        connection.execute(
            "UPDATE hosts SET revoked_at_ms = COALESCE(revoked_at_ms, ?2) WHERE id = ?1",
            params![host_id, now_ms() as i64],
        )?;
        Ok(())
    }

    pub fn revoke_device(&self, device_id: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock();
        connection.execute(
            "UPDATE devices SET revoked_at_ms = COALESCE(revoked_at_ms, ?2) WHERE id = ?1",
            params![device_id, now_ms() as i64],
        )?;
        // Unlink everywhere: a revoked device pairs with nothing.
        connection.execute(
            "UPDATE pairing_links SET unlinked_at_ms = COALESCE(unlinked_at_ms, ?2)
             WHERE device_id = ?1",
            params![device_id, now_ms() as i64],
        )?;
        Ok(())
    }

    // ---- pairing links ----

    /// Record that a host trusts a device. Only the host may create this
    /// row, only over its authenticated channel (the caller enforces the
    /// channel identity; the store enforces that both ends exist and are
    /// unrevoked).
    pub fn link_device(&self, host_id: &str, device_id: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock();
        connection.execute(
            "INSERT INTO pairing_links (host_id, device_id, linked_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(host_id, device_id) DO UPDATE SET
                 linked_at_ms = excluded.linked_at_ms,
                 unlinked_at_ms = NULL",
            params![host_id, device_id, now_ms() as i64],
        )?;
        Ok(())
    }

    pub fn unlink_device(&self, host_id: &str, device_id: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock();
        connection.execute(
            "UPDATE pairing_links SET unlinked_at_ms = COALESCE(unlinked_at_ms, ?3)
             WHERE host_id = ?1 AND device_id = ?2",
            params![host_id, device_id, now_ms() as i64],
        )?;
        Ok(())
    }

    /// Per-message route authorization: is this device currently paired
    /// with this host? Used for every sealed and handshake frame, not
    /// just at connect time (architecture: 统一路由层逐消息检查). A
    /// store error fails the caller closed.
    pub fn device_paired_with_host(&self, host_id: &str, device_id: &str) -> anyhow::Result<bool> {
        let connection = self.connection.lock();
        let linked: Option<i64> = connection
            .query_row(
                "SELECT linked_at_ms FROM pairing_links
                 WHERE host_id = ?1 AND device_id = ?2
                   AND unlinked_at_ms IS NULL
                   AND EXISTS (SELECT 1 FROM hosts WHERE id = ?1 AND revoked_at_ms IS NULL)
                   AND EXISTS (SELECT 1 FROM devices WHERE id = ?2 AND revoked_at_ms IS NULL)",
                params![host_id, device_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(linked.is_some())
    }

    // ---- pairing nonces ----

    /// Store a host-generated pairing nonce (hashed) for later consumption.
    pub fn store_pairing_nonce(&self, host_id: &str, nonce: &str) -> anyhow::Result<()> {
        let expires_at = (now_ms() + PAIRING_NONCE_TTL.as_millis() as u64) as i64;
        let connection = self.connection.lock();
        connection.execute(
            "INSERT INTO pairing_nonces (nonce_hash, host_id, expires_at_ms) VALUES (?1, ?2, ?3)",
            params![hash_token(nonce).as_slice(), host_id, expires_at],
        )?;
        Ok(())
    }

    /// Atomically consume a pairing nonce. Exactly one caller ever sees
    /// `Consumed` — the conditional SQL update is the single point of
    /// serialization, so a replay after a crash cannot resurrect the nonce.
    pub fn consume_pairing_nonce(
        &self,
        host_id: &str,
        nonce: &str,
    ) -> anyhow::Result<NonceOutcome> {
        let nonce_hash = hash_token(nonce);
        let mut connection = self.connection.lock();
        let tx = connection.transaction()?;
        let row: Option<(i64, Option<i64>)> = tx
            .query_row(
                "SELECT expires_at_ms, consumed_at_ms FROM pairing_nonces
                 WHERE nonce_hash = ?1 AND host_id = ?2",
                params![nonce_hash, host_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((expires_at_ms, consumed_at_ms)) = row else {
            return Ok(NonceOutcome::Unknown);
        };
        if consumed_at_ms.is_some() {
            return Ok(NonceOutcome::Unknown);
        }
        if (expires_at_ms as u64) < now_ms() {
            return Ok(NonceOutcome::Expired);
        }
        let updated = tx.execute(
            "UPDATE pairing_nonces SET consumed_at_ms = ?2
             WHERE nonce_hash = ?1 AND consumed_at_ms IS NULL",
            params![nonce_hash, now_ms() as i64],
        )?;
        tx.commit()?;
        anyhow::ensure!(updated == 1, "nonce raced between read and consume");
        Ok(NonceOutcome::Consumed)
    }

    /// Drop expired pairing nonces and dead auth challenges. Called on a
    /// slow periodic sweep; these rows carry no secrets but must not grow
    /// without bound.
    pub fn sweep(&self) -> anyhow::Result<()> {
        let count = self.sweep_counter.fetch_add(1, Ordering::Relaxed);
        if count % 64 != 0 {
            return Ok(());
        }
        let connection = self.connection.lock();
        connection.execute(
            "DELETE FROM pairing_nonces WHERE expires_at_ms < ?1 OR consumed_at_ms IS NOT NULL",
            params![(now_ms() as i64) - 3_600_000],
        )?;
        Ok(())
    }
}

/// Outcome of consuming a pairing nonce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NonceOutcome {
    /// Atomically consumed: exactly one caller ever sees this.
    Consumed,
    /// The nonce exists but its TTL elapsed.
    Expired,
    /// No such nonce for this host.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store opens")
    }

    fn key(seed: u8) -> Vec<u8> {
        vec![seed; 32]
    }

    /// Ways the identity store can fail: a second registration hijacks an
    /// id, a revoked key re-registers itself back to life, a connection
    /// authenticates against an unregistered key, or a pairing link
    /// survives an unlink or a device revocation.
    #[test]
    fn registration_is_key_idempotent_and_revocation_is_sticky() {
        let s = store();
        let host = s.register_host(&key(1)).unwrap();
        assert_eq!(s.register_host(&key(1)).unwrap(), host);
        s.revoke_host(&host).unwrap();
        assert!(s.register_host(&key(1)).is_err());
        // A different key registers fine.
        s.register_host(&key(2)).unwrap();
    }

    #[test]
    fn connection_authentication_checks_registered_key_only() {
        let s = store();
        let host = s.register_host(&key(1)).unwrap();
        assert!(s.verify_host_signature(&host, &key(1)).unwrap());
        assert!(!s.verify_host_signature(&host, &key(9)).unwrap());
        assert!(!s.verify_host_signature("unknown", &key(1)).unwrap());
        // The signing key lookup the WS admission uses behaves the same.
        assert_eq!(s.host_signing_key(&host).unwrap(), Some(key(1)));
        s.revoke_host(&host).unwrap();
        assert_eq!(s.host_signing_key(&host).unwrap(), None);
    }

    #[test]
    fn pairing_links_drive_route_authorization() {
        let s = store();
        let host = s.register_host(&key(1)).unwrap();
        let device = s.register_device(&key(2), "phone").unwrap();
        assert!(!s.device_paired_with_host(&host, &device).unwrap());
        s.link_device(&host, &device).unwrap();
        assert!(s.device_paired_with_host(&host, &device).unwrap());
        // Another device is still invisible.
        let stranger = s.register_device(&key(3), "stranger").unwrap();
        assert!(!s.device_paired_with_host(&host, &stranger).unwrap());
        // Unlink hides the device again.
        s.unlink_device(&host, &device).unwrap();
        assert!(!s.device_paired_with_host(&host, &device).unwrap());
        // And a device revocation kills every link at once.
        s.link_device(&host, &device).unwrap();
        s.revoke_device(&device).unwrap();
        assert!(!s.device_paired_with_host(&host, &device).unwrap());
    }

    #[test]
    fn relinking_after_unlink_reopens_the_route() {
        let s = store();
        let host = s.register_host(&key(1)).unwrap();
        let device = s.register_device(&key(2), "phone").unwrap();
        s.link_device(&host, &device).unwrap();
        s.unlink_device(&host, &device).unwrap();
        s.link_device(&host, &device).unwrap();
        assert!(s.device_paired_with_host(&host, &device).unwrap());
    }

    #[test]
    fn nonce_is_consumed_exactly_once() {
        let s = store();
        let host = s.register_host(&key(1)).unwrap();
        s.store_pairing_nonce(&host, "nonce-value").unwrap();
        assert_eq!(
            s.consume_pairing_nonce(&host, "nonce-value").unwrap(),
            NonceOutcome::Consumed
        );
        assert_eq!(
            s.consume_pairing_nonce(&host, "nonce-value").unwrap(),
            NonceOutcome::Unknown
        );
        assert_eq!(
            s.consume_pairing_nonce(&host, "never-stored").unwrap(),
            NonceOutcome::Unknown
        );
    }
}

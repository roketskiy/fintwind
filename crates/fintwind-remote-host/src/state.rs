//! Shared state for the host daemon: identity, the local daemon client,
//! the dedup store, the approval registry, and the completed-work queue
//! that hands worker results back to the relay session loop.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use fintwind_client::DaemonClient;
use fintwind_remote_crypto::IdentityKey;
use fintwind_remote_protocol::{
    MessageBody, OperationRequest, PermissionDecision, SealedMessage, UserInputDecision,
};

use crate::adapter::RuntimeCatalog;
use crate::approvals::ApprovalRegistry;
use crate::config::HostConfig;
use crate::dedup::OperationStore;
use crate::grants::GrantFile;
use crate::identity::HostIdentity;

/// Work the relay session hands to the single worker thread. Daemon RPCs
/// can take seconds; the session's read loop must never wait on them.
pub enum SessionCommand {
    Operation(OperationRequest),
    PermissionDecision(PermissionDecision),
    UserInputDecision(UserInputDecision),
    Snapshot,
}

pub struct HostState {
    config: HostConfig,
    identity: HostIdentity,
    grants: GrantFile,
    approvals: ApprovalRegistry,
    dedup: OperationStore,
    catalog: Mutex<RuntimeCatalog>,
    daemon: Mutex<Option<Arc<DaemonClient>>>,
    completed: Mutex<VecDeque<SealedMessage>>,
    shutdown: AtomicBool,
}

impl HostState {
    pub fn new(
        config: HostConfig,
        identity: HostIdentity,
        grants: GrantFile,
        dedup: OperationStore,
    ) -> Self {
        Self {
            config,
            identity,
            grants,
            approvals: ApprovalRegistry::new(),
            dedup,
            catalog: Mutex::new(RuntimeCatalog::default()),
            daemon: Mutex::new(None),
            completed: Mutex::new(VecDeque::new()),
            shutdown: AtomicBool::new(false),
        }
    }

    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    pub fn host_id(&self) -> &str {
        &self.identity.host_id
    }

    pub fn identity(&self) -> &IdentityKey {
        &self.identity.noise
    }

    pub fn signing_key(&self) -> &fintwind_remote_crypto::SigningKey {
        &self.identity.signing
    }

    /// The paired device's Noise static public key, for the KK responder.
    /// No grant, no handshake — a device the operator never confirmed is
    /// invisible to this host even if the relay routes its frames here.
    pub fn grant_public_key(&self, device_id: &str) -> anyhow::Result<Vec<u8>> {
        self.grants.device_public_key_bytes(device_id)
    }

    /// The sole active grant's device id (phase one is single-device).
    pub fn grants_for_device(&self) -> Option<String> {
        self.grants
            .grants
            .iter()
            .find(|grant| grant.revoked_at_ms.is_none())
            .map(|grant| grant.device_id.clone())
    }

    pub fn grant_version(&self, device_id: &str) -> u64 {
        self.grants
            .active_grant(device_id)
            .map(|grant| grant.grant_version)
            .unwrap_or(0)
    }

    pub fn approvals(&self) -> &ApprovalRegistry {
        &self.approvals
    }

    pub fn dedup(&self) -> &OperationStore {
        &self.dedup
    }

    pub fn catalog(&self) -> &Mutex<RuntimeCatalog> {
        &self.catalog
    }

    /// The live daemon client, if the local connection is up. Absent means
    /// "host degraded": the relay session stays connected and answers
    /// honestly that the workspace is unreachable.
    pub fn daemon(&self) -> Option<Arc<DaemonClient>> {
        self.daemon.lock().clone()
    }

    pub fn set_daemon(&self, client: Option<Arc<DaemonClient>>) {
        *self.daemon.lock() = client;
    }

    /// Worker results waiting to be sealed and sent to the device.
    pub fn push_completed(&self, message: SealedMessage) {
        self.completed.lock().push_back(message);
    }

    pub fn take_completed(&self) -> Option<SealedMessage> {
        self.completed.lock().pop_front()
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    pub fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// Build the envelope for one host-to-device message.
    pub fn seal_envelope(&self, body: MessageBody, device_id: &str) -> SealedMessage {
        SealedMessage {
            protocol_version: fintwind_remote_protocol::REMOTE_PROTOCOL_VERSION,
            host_id: self.identity.host_id.parse().unwrap_or_default(),
            device_id: device_id.parse().unwrap_or_default(),
            sent_at_ms: now_ms(),
            body,
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Where identity and dedup files live.
pub fn identity_paths(data_dir: &PathBuf) -> (PathBuf, PathBuf, PathBuf) {
    (
        data_dir.join("host-signing-key.bin"),
        data_dir.join("host-noise-key.bin"),
        data_dir.join("host-id.txt"),
    )
}

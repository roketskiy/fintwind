//! Local device-grant record: the paired-device file this host trusts.
//!
//! The pairing flow (M3) creates and revokes these rows. Each row carries
//! the device's routing id, the name the operator saw on the confirmation
//! dialog, its Noise static public key, the grant version the pairing
//! confirmed, and the revocation state. The confirmation dialog shows the
//! fingerprint derived from the stored public key, so what the operator
//! approved is exactly what every later handshake pins.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Scope text shown verbatim on the pairing confirmation dialog (architecture
/// document, 配对授权范围, and E39: the grant must not be dressed up as
/// read-only): the device may view and manage every session and history of
/// this host — including sessions created later — run the agent and answer
/// its approvals, and read files, diffs and task output in already-registered
/// projects. The agent itself acts under this computer's own permissions.
/// Keeping it in the grant file binds what was confirmed to what is enforced.
///
/// This constant is the canonical Chinese wording, shared by the CLI. The
/// desktop dialog shows the same scope through the localized
/// `settings.devices_grant_scope` keys, which must be kept in sync with it.
pub const GRANT_SCOPE_TEXT: &str = "允许查看和管理此电脑的全部现有及后续会话与历史，运行 agent 并回复审批，读取已有项目的文件、差异和输出。agent 可按本机权限修改文件或执行命令。";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceGrant {
    pub device_id: String,
    /// What the host operator saw on the confirmation dialog. Self-reported
    /// by the device, so it is a label for humans, never an authorization
    /// input; the fingerprint is what identifies the device.
    #[serde(default)]
    pub device_name: Option<String>,
    /// Noise static public key (base64 standard). KK handshake fails the
    /// connection unless the peer presents exactly this key.
    pub device_public_key: String,
    pub grant_version: u64,
    pub granted_at_ms: u64,
    #[serde(default)]
    pub revoked_at_ms: Option<u64>,
}

impl DeviceGrant {
    /// Hex of the first 16 bytes of SHA-256(public key) — the same digest
    /// the pairing dialog and the check-code flow show, so the list, the
    /// confirmation, and the grant all name one identity.
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.device_public_key)
    }

    /// What the device list shows: the confirmed name when there is one,
    /// a short fingerprint otherwise. A UUID is never the human label.
    pub fn display_label(&self) -> String {
        match self.device_name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => self.fingerprint(),
        }
    }
}

/// The fingerprint of a base64-encoded Noise static public key. An
/// unreadable key hashes to a stable placeholder rather than panicking:
/// a broken grant row must still render, and the handshake refuses it.
pub fn fingerprint_of(public_key_b64: &str) -> String {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(public_key_b64)
        .unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    digest[..16].iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantFile {
    pub grants: Vec<DeviceGrant>,
}

impl GrantFile {
    pub fn load(path: &Path) -> anyhow::Result<Option<GrantFile>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(serde_json::from_str(&text)?))
    }

    /// The live grant for a device id, if one exists and is not revoked.
    pub fn active_grant(&self, device_id: &str) -> Option<&DeviceGrant> {
        self.grants.iter().find(|grant| {
            grant.device_id == device_id && grant.revoked_at_ms.is_none()
        })
    }

    /// Decode the grant's stored public key into raw bytes for the Noise
    /// responder. A grant with an unreadable key is a provisioning error
    /// and fails closed.
    pub fn device_public_key_bytes(&self, device_id: &str) -> anyhow::Result<Vec<u8>> {
        use base64::Engine as _;
        let grant = self
            .active_grant(device_id)
            .ok_or_else(|| anyhow::anyhow!("no active grant for device {device_id}"))?;
        base64::engine::general_purpose::STANDARD
            .decode(&grant.device_public_key)
            .map_err(|error| anyhow::anyhow!("grant public key unreadable: {error}"))
    }

    /// Insert or refresh one device's grant, preserving every other row.
    /// The pairing flow calls this after the on-host confirmation; the
    /// grant version moves forward so a stale confirmation cannot
    /// resurrect an older binding.
    pub fn upsert(&mut self, grant: DeviceGrant) {
        match self
            .grants
            .iter_mut()
            .find(|existing| existing.device_id == grant.device_id)
        {
            Some(existing) => *existing = grant,
            None => self.grants.push(grant),
        }
    }

    /// Mark a device's grant revoked. The row stays: the device list shows
    /// the revocation instead of pretending the device never existed.
    pub fn revoke(&mut self, device_id: &str, at_ms: u64) -> bool {
        let mut revoked = false;
        for grant in &mut self.grants {
            if grant.device_id == device_id && grant.revoked_at_ms.is_none() {
                grant.revoked_at_ms = Some(at_ms);
                revoked = true;
            }
        }
        revoked
    }
}

/// Stable digest over an approval's displayed content. The device sees
/// the same fields; a decision is only consumed when this digest still
/// matches what the driver currently asks for.
pub fn action_digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    digest[..16].iter().map(|byte| format!("{byte:02x}")).collect()
}

//! Local device-grant record: the paired-device file this host trusts.
//!
//! Phase one is single-device (M0 scope), so the file holds exactly one
//! grant. The pairing flow (M3) will create and revoke these files; for
//! now they are provisioned by hand, and every field the M3 flow needs is
//! already here: the device's routing id, its Noise static public key,
//! the grant version the pairing confirmed, and the scope text the host
//! operator accepted on the confirmation dialog.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Scope text shown verbatim on the pairing confirmation dialog (architecture
/// document, 配对授权范围): the device may manage every session and history of
/// this host and read files, diffs and task output in already-registered
/// projects. Keeping it in the grant file binds what was confirmed to what
/// is enforced.
pub const GRANT_SCOPE_TEXT: &str = "该设备可读取和管理这台 host 的全部会话及其历史，包括之后创建的会话，并查看本机已有项目中的文件、diff 与任务输出。";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceGrant {
    pub device_id: String,
    /// Noise static public key (base64 standard). KK handshake fails the
    /// connection unless the peer presents exactly this key.
    pub device_public_key: String,
    pub grant_version: u64,
    pub granted_at_ms: u64,
    #[serde(default)]
    pub revoked_at_ms: Option<u64>,
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

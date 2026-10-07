//! Approval binding and atomic consumption (architecture document: 审批绑定
//! 与审批竞争有唯一终态).
//!
//! Every driver permission / user-input request that goes out to a device
//! is wrapped in a remote approval record: a fresh id, the underlying
//! driver request id, the session runtime it belongs to, a digest over
//! the displayed content, the grant version, and a TTL. A decision is
//! consumed exactly once — the first valid decision wins, and every late,
//! duplicate, expired, or digest-mismatched decision gets an explicit
//! failure instead of a second execution.
//!
//! Consumption happens before the driver sees anything: a consumed offer
//! can never reach `Respond` twice.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use uuid::Uuid;

use fintwind_remote_protocol::PermissionOffer;

use crate::config::APPROVAL_TTL;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsumeError {
    /// No such offer, or already consumed by an earlier decision.
    UnknownOrConsumed,
    /// The TTL elapsed; the host will not act on it.
    Expired,
    /// The driver's live request no longer matches what was displayed.
    DigestMismatch,
}

struct ApprovalRecord {
    offer: PermissionOffer,
    consumed: bool,
    /// The option id of the winning decision, for the audit trail.
    decision: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub struct ApprovalRegistry {
    approvals: Mutex<HashMap<String, ApprovalRecord>>,
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        Self {
            approvals: Mutex::new(HashMap::new()),
        }
    }

    /// Wrap a live driver request into a remote offer. `session_id` and
    /// `runtime_id` come from the event's own envelope, so the offer can
    /// never be re-targeted by the device.
    pub fn create_offer(
        &self,
        session_id: Uuid,
        runtime_id: Uuid,
        request_ref: String,
        title: String,
        detail: String,
        options: Vec<(String, String)>,
        action_digest: String,
        grant_version: u64,
    ) -> PermissionOffer {
        let offer = PermissionOffer {
            approval_id: Uuid::new_v4(),
            session_id,
            runtime_id,
            request_ref,
            title,
            detail,
            options: options
                .into_iter()
                .map(|(id, label)| fintwind_remote_protocol::PermissionOption { id, label })
                .collect(),
            expires_at_ms: now_ms() + APPROVAL_TTL.as_millis() as u64,
            action_digest,
            grant_version,
        };
        self.approvals.lock().insert(
            offer.approval_id.to_string(),
            ApprovalRecord {
                offer: offer.clone(),
                consumed: false,
                decision: None,
            },
        );
        offer
    }

    /// Atomically consume a decision. Exactly one call ever gets `Ok` —
    /// the mutex plus the consumed flag make late duplicates explicit
    /// failures, and every refusal names its reason.
    pub fn consume(
        &self,
        approval_id: &str,
        option_id: &str,
        live_digest: Option<&str>,
    ) -> Result<(PermissionOffer, String), ConsumeError> {
        let mut approvals = self.approvals.lock();
        let record = approvals
            .get_mut(approval_id)
            .ok_or(ConsumeError::UnknownOrConsumed)?;
        if record.consumed {
            return Err(ConsumeError::UnknownOrConsumed);
        }
        if record.offer.expires_at_ms < now_ms() {
            record.consumed = true;
            return Err(ConsumeError::Expired);
        }
        if let Some(live) = live_digest {
            if live != record.offer.action_digest {
                record.consumed = true;
                return Err(ConsumeError::DigestMismatch);
            }
        }
        record.consumed = true;
        record.decision = Some(option_id.to_owned());
        Ok((record.offer.clone(), option_id.to_owned()))
    }

    /// Whether an offer is still open (live pending view for snapshots).
    pub fn pending_offers(&self, session_id: Uuid) -> Vec<PermissionOffer> {
        let now = now_ms();
        self.approvals
            .lock()
            .values()
            .filter(|record| {
                !record.consumed
                    && record.offer.expires_at_ms > now
                    && record.offer.session_id == session_id
            })
            .map(|record| record.offer.clone())
            .collect()
    }

    /// Drop consumed and expired records so the table stays bounded.
    pub fn sweep(&self) {
        let cutoff = now_ms() - 24 * 60 * 60 * 1000;
        self.approvals
            .lock()
            .retain(|_, record| record.offer.expires_at_ms > cutoff);
    }
}

/// Convert a TTL in seconds for logging without importing time plumbing
/// everywhere.
pub fn ttl_seconds() -> u64 {
    APPROVAL_TTL.as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with_offer() -> (ApprovalRegistry, String, String) {
        let registry = ApprovalRegistry::new();
        let offer = registry.create_offer(
            Uuid::nil(),
            Uuid::nil(),
            "per_1".to_owned(),
            "run bash".to_owned(),
            "detail".to_owned(),
            vec![("once".into(), "Allow once".into())],
            "digest-1".to_owned(),
            1,
        );
        (
            registry,
            offer.approval_id.to_string(),
            offer.request_ref,
        )
    }

    /// Ways this registry can fail: a duplicate decision executes twice,
    /// an expired offer is granted posthumously, the digest drifts
    /// without notice, or an unknown id guesses its way through.
    #[test]
    fn duplicate_decision_is_an_explicit_failure() {
        let (registry, approval_id, _) = registry_with_offer();
        assert!(registry.consume(&approval_id, "once", None).is_ok());
        assert!(matches!(
            registry.consume(&approval_id, "once", None),
            Err(ConsumeError::UnknownOrConsumed)
        ));
    }

    #[test]
    fn unknown_id_fails_closed() {
        let (registry, _, _) = registry_with_offer();
        assert!(matches!(
            registry.consume("nope", "once", None),
            Err(ConsumeError::UnknownOrConsumed)
        ));
    }

    #[test]
    fn digest_mismatch_consumes_and_refuses() {
        let (registry, approval_id, _) = registry_with_offer();
        assert!(matches!(
            registry.consume(&approval_id, "once", Some("different")),
            Err(ConsumeError::DigestMismatch)
        ));
        // The refusal also ends the offer: no second chance with the
        // original digest after a mismatch was observed.
        assert!(matches!(
            registry.consume(&approval_id, "once", Some("digest-1")),
            Err(ConsumeError::UnknownOrConsumed)
        ));
    }

    #[test]
    fn pending_view_only_shows_open_offers() {
        let (registry, approval_id, _) = registry_with_offer();
        assert_eq!(registry.pending_offers(Uuid::nil()).len(), 1);
        let _ = registry.consume(&approval_id, "once", None);
        assert!(registry.pending_offers(Uuid::nil()).is_empty());
    }
}

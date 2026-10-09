//! Approval binding and atomic consumption (architecture document: 审批绑定
//! 与审批竞争有唯一终态).
//!
//! Every driver permission / user-input request that goes out to a device
//! is wrapped in a remote offer: a fresh id, the underlying driver request
//! id, the session runtime it belongs to, a digest over the displayed
//! content, the grant version it was issued under, the device it was
//! issued to, and a TTL. A decision is consumed exactly once — the first
//! valid decision wins, and every late, duplicate, expired, foreign, or
//! voided decision gets an explicit failure instead of a second execution.
//! Consumption happens before the driver hears anything: a consumed offer
//! can never reach `Respond` twice.
//!
//! ## What "valid" means without an authoritative pending-request query
//!
//! The base exposes no query for "which requests are still pending" (M0
//! freeze, 缺口 1), so this registry is the host's authority for the
//! *remote* view and must never claim a liveness it cannot prove:
//!
//! - The digest binds the exact displayed action. A re-announced request
//!   whose content changed voids the old offer (`invalidate_offer`) and
//!   mints a new one; an identical re-announcement is ignored.
//! - Turn-lifecycle signals (`turnStarted`, `turnFinished`, `steerAccepted`,
//!   `processExited`, `nativeSessionMoved`) end the exchange that produced
//!   a request, so they void that session's offers.
//! - A runtime-instance change (a new event epoch on the same session, or a
//!   daemon reconnect) voids everything: the old instance's requests are
//!   gone and none may be answered on their behalf.
//! - A grant change voids that device's offers; the device must re-pair.
//!
//! The local GUI remains able to answer whatever is genuinely pending; the
//! remote side simply never acts on a request it can no longer confirm.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use uuid::Uuid;

use fintwind_remote_protocol::{
    MessageBody, PermissionOffer, UserInputAnswer, UserInputOffer, UserInputQuestion,
};

use crate::config::APPROVAL_TTL;

/// How one offer met its end. Only `Decided` ever reaches the driver.
#[derive(Clone, Debug, Eq, PartialEq)]
enum RecordOutcome {
    Open,
    /// A device's winning decision (the option id, or "answers").
    Decided(String),
    /// Voided before any decision could count, with the reason the device
    /// is told.
    Invalidated(String),
}

/// One offer as the host holds it: the wire payload plus the device it was
/// issued to and how it ended.
#[derive(Clone, Debug)]
struct ApprovalRecord {
    device_id: Uuid,
    offer: RemoteOffer,
    outcome: RecordOutcome,
}

/// The two offer payloads, kept distinct so a permission decision can
/// never be replayed as a question answer or the other way round.
#[derive(Clone, Debug)]
enum OfferPayload {
    Permission(PermissionOffer),
    UserInput(UserInputOffer),
}

/// One offer as the host holds it: the wire payload, the device it was
/// issued to, and the digest over the displayed action. The device binding
/// and the digest live here rather than in the wire structs — the payload
/// shape is what the device renders, while "who may answer this" and "what
/// exactly was shown" are host-side authorization data.
#[derive(Clone, Debug)]
pub struct RemoteOffer {
    pub device_id: Uuid,
    action_digest: String,
    payload: OfferPayload,
}

impl RemoteOffer {
    fn new(device_id: Uuid, action_digest: String, payload: OfferPayload) -> Self {
        Self {
            device_id,
            action_digest,
            payload,
        }
    }

    pub fn approval_id(&self) -> Uuid {
        match &self.payload {
            OfferPayload::Permission(offer) => offer.approval_id,
            OfferPayload::UserInput(offer) => offer.approval_id,
        }
    }

    pub fn session_id(&self) -> Uuid {
        match &self.payload {
            OfferPayload::Permission(offer) => offer.session_id,
            OfferPayload::UserInput(offer) => offer.session_id,
        }
    }

    pub fn runtime_id(&self) -> Uuid {
        match &self.payload {
            OfferPayload::Permission(offer) => offer.runtime_id,
            OfferPayload::UserInput(offer) => offer.runtime_id,
        }
    }

    pub fn request_ref(&self) -> &str {
        match &self.payload {
            OfferPayload::Permission(offer) => &offer.request_ref,
            OfferPayload::UserInput(offer) => &offer.request_ref,
        }
    }

    pub fn expires_at_ms(&self) -> u64 {
        match &self.payload {
            OfferPayload::Permission(offer) => offer.expires_at_ms,
            OfferPayload::UserInput(offer) => offer.expires_at_ms,
        }
    }

    pub fn grant_version(&self) -> u64 {
        match &self.payload {
            OfferPayload::Permission(offer) => offer.grant_version,
            OfferPayload::UserInput(offer) => offer.grant_version,
        }
    }

    pub fn action_digest(&self) -> &str {
        &self.action_digest
    }

    /// The sealed body to send to the device.
    pub fn body(&self) -> MessageBody {
        match &self.payload {
            OfferPayload::Permission(offer) => MessageBody::PermissionOffer(offer.clone()),
            OfferPayload::UserInput(offer) => MessageBody::UserInputOffer(offer.clone()),
        }
    }
}

/// One question as the remote offer carries it. Option ids are synthesized
/// by the host (the base's own option shape has labels only), so the device
/// answers with ids the host can map back exactly.
#[derive(Clone, Debug)]
pub struct OfferQuestion {
    pub id: String,
    pub question: String,
    pub multi_select: bool,
    /// `(id, label)` pairs; ids are `opt-<index>` within this question.
    pub options: Vec<(String, String)>,
}

/// Everything a decision is checked against, captured at decision time.
pub struct DecisionContext {
    /// The authenticated device that sent the decision.
    pub device_id: Uuid,
    /// The device's grant version *now*; an offer issued under a different
    /// version is void.
    pub grant_version: u64,
    /// The session's current runtime id. `None` means the host cannot
    /// confirm the runtime, which fails closed.
    pub runtime_id: Option<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConsumeError {
    /// No such offer, or a decision already won.
    UnknownOrConsumed,
    /// The TTL elapsed; the host will not act on it.
    Expired,
    /// The live digest no longer matches what was displayed.
    DigestMismatch,
    /// The decision came from a device the offer was not issued to.
    DeviceMismatch,
    /// The device's grant version moved (re-pairing or revocation).
    GrantMismatch,
    /// The session's runtime instance changed under the offer.
    RuntimeMismatch,
    /// The offer was voided: answered locally, cancelled, or its exchange
    /// ended. Carries the reason the device is told.
    Invalidated(String),
    /// The answer set does not match the questions that were asked.
    InvalidAnswer(String),
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub struct ApprovalRegistry {
    approvals: Mutex<HashMap<String, ApprovalRecord>>,
    /// (session, driver request ref) -> the approval id that won it. The
    /// claim is what makes "one provider request, one authoritative remote
    /// decision" true across devices: two devices may each hold an offer
    /// for the same request, and exactly one decision may ever reach the
    /// driver.
    claims: Mutex<HashMap<(Uuid, String), String>>,
}

impl Default for ApprovalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        Self {
            approvals: Mutex::new(HashMap::new()),
            claims: Mutex::new(HashMap::new()),
        }
    }

    /// Wrap a live driver permission request into a remote offer. The
    /// caller has already removed every "always"-style option: a remote
    /// decision never creates a standing rule, it answers one request.
    pub fn create_permission_offer(
        &self,
        device_id: Uuid,
        session_id: Uuid,
        runtime_id: Uuid,
        request_ref: String,
        title: String,
        detail: String,
        options: Vec<(String, String)>,
        action_digest: String,
        grant_version: u64,
    ) -> RemoteOffer {
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
            action_digest: action_digest.clone(),
            grant_version,
        };
        let offer = RemoteOffer::new(device_id, action_digest, OfferPayload::Permission(offer));
        self.insert(offer)
    }

    /// Wrap a live driver question set into a remote offer. `allow_custom`
    /// is always true: the local flow answers with a free-text alternative
    /// for every question, so the remote one may too.
    pub fn create_user_input_offer(
        &self,
        device_id: Uuid,
        session_id: Uuid,
        runtime_id: Uuid,
        request_ref: String,
        questions: Vec<OfferQuestion>,
        action_digest: String,
        grant_version: u64,
    ) -> RemoteOffer {
        let offer = UserInputOffer {
            approval_id: Uuid::new_v4(),
            session_id,
            runtime_id,
            request_ref,
            questions: questions
                .into_iter()
                .map(|question| UserInputQuestion {
                    id: question.id,
                    question: question.question,
                    multi_select: question.multi_select,
                    allow_custom: true,
                    options: question
                        .options
                        .into_iter()
                        .map(|(id, label)| fintwind_remote_protocol::PermissionOption { id, label })
                        .collect(),
                })
                .collect(),
            expires_at_ms: now_ms() + APPROVAL_TTL.as_millis() as u64,
            grant_version,
        };
        let offer = RemoteOffer::new(device_id, action_digest, OfferPayload::UserInput(offer));
        self.insert(offer)
    }

    fn insert(&self, offer: RemoteOffer) -> RemoteOffer {
        self.approvals.lock().insert(
            offer.approval_id().to_string(),
            ApprovalRecord {
                device_id: offer.device_id,
                offer: offer.clone(),
                outcome: RecordOutcome::Open,
            },
        );
        offer
    }

    /// The offer as it stands, for context capture before consumption.
    /// Returned even for a finished record so the caller can compute the
    /// current runtime; `consume` makes the final call.
    pub fn offer_snapshot(&self, approval_id: &str) -> Option<RemoteOffer> {
        self.approvals
            .lock()
            .get(approval_id)
            .map(|record| record.offer.clone())
    }

    /// The still-open offer for one driver request *and one device*. The
    /// event pump asks per device, because each device holding the session
    /// open gets its own offer for the same underlying request.
    pub fn open_offer_for_request(
        &self,
        session_id: Uuid,
        request_ref: &str,
        device_id: Uuid,
    ) -> Option<RemoteOffer> {        self.approvals
            .lock()
            .values()
            .find(|record| {
                record.outcome == RecordOutcome::Open
                    && record.offer.session_id() == session_id
                    && record.offer.request_ref() == request_ref
                    && record.device_id == device_id
            })
            .map(|record| record.offer.clone())
    }

    /// Void every offer for one driver request, whatever device holds it:
    /// the request changed, so no previously displayed action may be
    /// answered. The request's claim is released with them.
    pub fn invalidate_request(&self, session_id: Uuid, request_ref: &str, reason: &str) {
        let mut approvals = self.approvals.lock();
        for record in approvals.values_mut() {
            if record.outcome == RecordOutcome::Open
                && record.offer.session_id() == session_id
                && record.offer.request_ref() == request_ref
            {
                record.outcome = RecordOutcome::Invalidated(reason.to_owned());
            }
        }
        drop(approvals);
        self.claims
            .lock()
            .remove(&(session_id, request_ref.to_owned()));
    }

    /// Every still-open offer issued to one device — what a reconnecting
    /// device is sent when it asks for a snapshot.
    pub fn pending_for_device(&self, device_id: Uuid) -> Vec<RemoteOffer> {
        let now = now_ms();
        self.approvals
            .lock()
            .values()
            .filter(|record| {
                record.outcome == RecordOutcome::Open
                    && record.device_id == device_id
                    && record.offer.expires_at_ms() > now
            })
            .map(|record| record.offer.clone())
            .collect()
    }

    /// Atomically consume a permission decision. Exactly one call ever gets
    /// `Ok`; every refusal names its reason and, where the offer is void,
    /// ends it so no later decision can count.
    pub fn consume(
        &self,
        approval_id: &str,
        option_id: &str,
        live_digest: Option<&str>,
        context: &DecisionContext,
    ) -> Result<(PermissionOffer, String), ConsumeError> {
        let mut approvals = self.approvals.lock();
        let record = approvals
            .get_mut(approval_id)
            .ok_or(ConsumeError::UnknownOrConsumed)?;
        // A foreign device may not even probe another device's offer: the
        // refusal happens before the record can be touched.
        if record.device_id != context.device_id {
            return Err(ConsumeError::DeviceMismatch);
        }
        Self::guard(record, live_digest, context)?;
        let OfferPayload::Permission(offer) = record.offer.payload.clone() else {
            return Err(ConsumeError::UnknownOrConsumed);
        };
        // The request-level claim: exactly one decision for this driver
        // request may ever reach the provider, whichever device sends it.
        if !Self::claim_request(&self.claims, record, &approval_id) {
            return Err(ConsumeError::Invalidated(
                "another device already answered this request".to_owned(),
            ));
        }
        record.outcome = RecordOutcome::Decided(option_id.to_owned());
        Ok((offer, option_id.to_owned()))
    }

    /// Atomically consume a question-answer set, validating it against the
    /// questions that were actually asked and mapping it to the base's
    /// label-plus-text answer shape.
    pub fn consume_user_input(
        &self,
        approval_id: &str,
        answers: &[UserInputAnswer],
        context: &DecisionContext,
    ) -> Result<Vec<fintwind_protocol::model::UserInputAnswer>, ConsumeError> {
        let mut approvals = self.approvals.lock();
        let record = approvals
            .get_mut(approval_id)
            .ok_or(ConsumeError::UnknownOrConsumed)?;
        if record.device_id != context.device_id {
            return Err(ConsumeError::DeviceMismatch);
        }
        Self::guard(record, None, context)?;
        let OfferPayload::UserInput(offer) = record.offer.payload.clone() else {
            return Err(ConsumeError::UnknownOrConsumed);
        };
        let mapped = map_answers(&offer, answers)?;
        // The request-level claim: exactly one decision for this driver
        // request may ever reach the provider, whichever device sends it.
        if !Self::claim_request(&self.claims, record, &approval_id) {
            return Err(ConsumeError::Invalidated(
                "another device already answered this request".to_owned(),
            ));
        }
        record.outcome = RecordOutcome::Decided("answers".to_owned());
        Ok(mapped)
    }

    /// Atomically claim the driver request behind one offer. Returns false
    /// when another device's decision already won it, in which case the
    /// caller's offer is voided — a second execution never happens.
    fn claim_request(
        claims: &Mutex<HashMap<(Uuid, String), String>>,
        record: &mut ApprovalRecord,
        approval_id: &str,
    ) -> bool {
        let key = (record.offer.session_id(), record.offer.request_ref().to_owned());
        let mut claims = claims.lock();
        match claims.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                if entry.get() == approval_id {
                    true
                } else {
                    record.outcome = RecordOutcome::Invalidated(
                        "another device already answered this request".to_owned(),
                    );
                    false
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(approval_id.to_owned());
                true
            }
        }
    }

    /// The shared liveness gate. Runs under the registry lock, so an
    /// invalidation that lands mid-decision is observed here rather than
    /// racing the driver call.
    fn guard(
        record: &mut ApprovalRecord,
        live_digest: Option<&str>,
        context: &DecisionContext,
    ) -> Result<(), ConsumeError> {
        match &record.outcome {
            RecordOutcome::Open => {}
            RecordOutcome::Decided(_) => return Err(ConsumeError::UnknownOrConsumed),
            RecordOutcome::Invalidated(reason) => {
                return Err(ConsumeError::Invalidated(reason.clone()))
            }
        }
        if record.offer.expires_at_ms() < now_ms() {
            record.outcome = RecordOutcome::Invalidated("the approval expired".to_owned());
            return Err(ConsumeError::Expired);
        }
        if let Some(live) = live_digest {
            if live != record.offer.action_digest() {
                record.outcome =
                    RecordOutcome::Invalidated("the action changed after the offer".to_owned());
                return Err(ConsumeError::DigestMismatch);
            }
        }
        if record.offer.grant_version() != context.grant_version {
            record.outcome =
                RecordOutcome::Invalidated("the device grant changed".to_owned());
            return Err(ConsumeError::GrantMismatch);
        }
        // An unconfirmable runtime fails closed: the offer belonged to an
        // instance this host can no longer vouch for.
        if context.runtime_id != Some(record.offer.runtime_id()) {
            record.outcome =
                RecordOutcome::Invalidated("the session runtime instance changed".to_owned());
            return Err(ConsumeError::RuntimeMismatch);
        }
        Ok(())
    }

    /// Void one offer: the request it displayed is no longer the request
    /// the driver would act on.
    pub fn invalidate_offer(&self, approval_id: &str, reason: &str) {
        if let Some(record) = self.approvals.lock().get_mut(approval_id) {
            if record.outcome == RecordOutcome::Open {
                record.outcome = RecordOutcome::Invalidated(reason.to_owned());
            }
        }
    }

    /// Void every offer for one session: the device closed the view, the
    /// turn ended, the process exited, or the native session moved. The
    /// session's request claims go with them.
    pub fn invalidate_session(&self, session_id: Uuid, reason: &str) {
        let mut approvals = self.approvals.lock();
        for record in approvals.values_mut() {
            if record.outcome == RecordOutcome::Open && record.offer.session_id() == session_id {
                record.outcome = RecordOutcome::Invalidated(reason.to_owned());
            }
        }
        drop(approvals);
        self.claims
            .lock()
            .retain(|(session, _), _| *session != session_id);
    }

    /// Void every offer issued to one device (grant revoked or re-paired).
    pub fn invalidate_device(&self, device_id: Uuid, reason: &str) {
        let mut approvals = self.approvals.lock();
        for record in approvals.values_mut() {
            if record.outcome == RecordOutcome::Open && record.device_id == device_id {
                record.outcome = RecordOutcome::Invalidated(reason.to_owned());
            }
        }
    }

    /// Void everything. Used when the runtime instance itself changed (a
    /// daemon reconnect): no request from the previous instance may be
    /// answered, because none can be confirmed to still exist.
    pub fn invalidate_all(&self, reason: &str) {
        let mut approvals = self.approvals.lock();
        for record in approvals.values_mut() {
            if record.outcome == RecordOutcome::Open {
                record.outcome = RecordOutcome::Invalidated(reason.to_owned());
            }
        }
        drop(approvals);
        self.claims.lock().clear();
    }

    /// Drop finished records so the table stays bounded.
    pub fn sweep(&self) {
        let cutoff = now_ms() - 24 * 60 * 60 * 1000;
        self.approvals
            .lock()
            .retain(|_, record| record.offer.expires_at_ms() > cutoff);
    }
}

/// Map a device's answers onto the base's answer shape, refusing anything
/// that does not match the questions that were asked.
fn map_answers(
    offer: &UserInputOffer,
    answers: &[UserInputAnswer],
) -> Result<Vec<fintwind_protocol::model::UserInputAnswer>, ConsumeError> {
    let invalid = |reason: &str| ConsumeError::InvalidAnswer(reason.to_owned());
    let mut mapped: Vec<fintwind_protocol::model::UserInputAnswer> = Vec::new();
    for answer in answers {
        let question = offer
            .questions
            .iter()
            .find(|question| question.id == answer.question_id)
            .ok_or_else(|| invalid("the answer names a question that was not asked"))?;
        if mapped
            .iter()
            .any(|existing| existing.question_id == answer.question_id)
        {
            return Err(invalid("the same question was answered twice"));
        }
        let labels: Vec<&str> = question
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect();
        let mut values: Vec<String> = Vec::new();
        for option_id in &answer.selected_option_ids {
            let index = option_id
                .strip_prefix("opt-")
                .and_then(|index| index.parse::<usize>().ok())
                .ok_or_else(|| invalid("the answer names an option that was not offered"))?;
            let label = labels
                .get(index)
                .ok_or_else(|| invalid("the answer names an option that was not offered"))?;
            if !values.iter().any(|value| value == label) {
                values.push((*label).to_owned());
            }
        }
        if !question.multi_select && values.len() > 1 {
            return Err(invalid("the question allows only one answer"));
        }
        if let Some(custom) = &answer.custom_text {
            let custom = custom.trim();
            if !custom.is_empty() {
                if !question.allow_custom {
                    return Err(invalid("the question does not allow a custom answer"));
                }
                values.push(custom.to_owned());
            }
        }
        if values.is_empty() {
            return Err(invalid("the question was left unanswered"));
        }
        mapped.push(fintwind_protocol::model::UserInputAnswer {
            question_id: answer.question_id.clone(),
            answers: values,
        });
    }
    if mapped.len() != offer.questions.len() {
        return Err(invalid("every question must be answered"));
    }
    Ok(mapped)
}

/// Convert a TTL in seconds for logging without importing time plumbing
/// everywhere.
pub fn ttl_seconds() -> u64 {
    APPROVAL_TTL.as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fintwind_remote_protocol::{PermissionDecision, UserInputDecision};

    fn context(device_id: Uuid) -> DecisionContext {
        DecisionContext {
            device_id,
            grant_version: 1,
            runtime_id: Some(Uuid::nil()),
        }
    }

    fn registry_with_offer() -> (ApprovalRegistry, String, String) {
        let registry = ApprovalRegistry::new();
        let offer = registry.create_permission_offer(
            Uuid::nil(),
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
            offer.approval_id().to_string(),
            offer.request_ref().to_owned(),
        )
    }

    fn question_serial() -> u32 {
        QUESTION_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    static QUESTION_SERIAL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

    fn question_offer(registry: &ApprovalRegistry) -> RemoteOffer {
        registry.create_user_input_offer(
            Uuid::nil(),
            Uuid::nil(),
            Uuid::nil(),
            format!("q_{}", question_serial()),
            vec![OfferQuestion {
                id: "which".to_owned(),
                question: "which one?".to_owned(),
                multi_select: false,
                options: vec![
                    ("opt-0".to_owned(), "Alpha".to_owned()),
                    ("opt-1".to_owned(), "Beta".to_owned()),
                ],
            }],
            "digest-q".to_owned(),
            1,
        )
    }

    /// Ways this registry can fail: a duplicate decision executes twice, an
    /// expired offer is granted posthumously, the digest drifts without
    /// notice, an unknown id guesses its way through, another device
    /// answers an offer it was never shown, a changed grant keeps an old
    /// offer alive, or an answer names options that were never offered.
    #[test]
    fn duplicate_decision_is_an_explicit_failure() {
        let (registry, approval_id, _) = registry_with_offer();
        let context = context(Uuid::nil());
        assert!(registry.consume(&approval_id, "once", None, &context).is_ok());
        assert!(matches!(
            registry.consume(&approval_id, "once", None, &context),
            Err(ConsumeError::UnknownOrConsumed)
        ));
    }

    #[test]
    fn unknown_id_fails_closed() {
        let (registry, _, _) = registry_with_offer();
        assert!(matches!(
            registry.consume("nope", "once", None, &context(Uuid::nil())),
            Err(ConsumeError::UnknownOrConsumed)
        ));
    }

    #[test]
    fn digest_mismatch_consumes_and_refuses() {
        let (registry, approval_id, _) = registry_with_offer();
        assert!(matches!(
            registry.consume(&approval_id, "once", Some("different"), &context(Uuid::nil())),
            Err(ConsumeError::DigestMismatch)
        ));
        // The refusal also ends the offer: no second chance with the
        // original digest after a mismatch was observed.
        assert!(matches!(
            registry.consume(&approval_id, "once", Some("digest-1"), &context(Uuid::nil())),
            Err(ConsumeError::Invalidated(_))
        ));
    }

    #[test]
    fn foreign_device_cannot_answer() {
        let (registry, approval_id, _) = registry_with_offer();
        assert!(matches!(
            registry.consume(&approval_id, "once", None, &context(Uuid::max())),
            Err(ConsumeError::DeviceMismatch)
        ));
        // The refusal must not have consumed the real device's offer.
        assert!(registry
            .consume(&approval_id, "once", None, &context(Uuid::nil()))
            .is_ok());
    }

    #[test]
    fn only_one_device_may_answer_a_request() {
        // Two devices hold offers for the same driver request; exactly one
        // decision may reach the provider.
        let registry = ApprovalRegistry::new();
        let first = registry.create_permission_offer(
            Uuid::nil(),
            Uuid::nil(),
            Uuid::nil(),
            "per_shared".to_owned(),
            "run bash".to_owned(),
            "detail".to_owned(),
            vec![("once".into(), "Allow once".into())],
            "digest-shared".to_owned(),
            1,
        );
        let second = registry.create_permission_offer(
            Uuid::max(),
            Uuid::nil(),
            Uuid::nil(),
            "per_shared".to_owned(),
            "run bash".to_owned(),
            "detail".to_owned(),
            vec![("once".into(), "Allow once".into())],
            "digest-shared".to_owned(),
            1,
        );
        assert!(registry
            .consume(
                &first.approval_id().to_string(),
                "once",
                None,
                &context(Uuid::nil())
            )
            .is_ok());
        // The second device's decision is refused explicitly, and its offer
        // is voided — no second execution of the same request.
        assert!(matches!(
            registry.consume(
                &second.approval_id().to_string(),
                "once",
                None,
                &context(Uuid::max())
            ),
            Err(ConsumeError::Invalidated(reason)) if reason.contains("another device")
        ));
    }

    #[test]
    fn changed_grant_voids_the_offer() {
        let (registry, approval_id, _) = registry_with_offer();
        let stale = DecisionContext {
            device_id: Uuid::nil(),
            grant_version: 1,
            runtime_id: Some(Uuid::nil()),
        };
        assert!(registry.consume(&approval_id, "once", None, &stale).is_ok());
        let (registry, approval_id, _) = registry_with_offer();
        let moved = DecisionContext {
            device_id: Uuid::nil(),
            grant_version: 2,
            runtime_id: Some(Uuid::nil()),
        };
        assert!(matches!(
            registry.consume(&approval_id, "once", None, &moved),
            Err(ConsumeError::GrantMismatch)
        ));
    }

    #[test]
    fn unconfirmable_runtime_fails_closed() {
        let (registry, approval_id, _) = registry_with_offer();
        let blind = DecisionContext {
            device_id: Uuid::nil(),
            grant_version: 1,
            runtime_id: None,
        };
        assert!(matches!(
            registry.consume(&approval_id, "once", None, &blind),
            Err(ConsumeError::RuntimeMismatch)
        ));
    }

    #[test]
    fn invalidation_ends_the_offer_with_a_reason() {
        let (registry, approval_id, _) = registry_with_offer();
        registry.invalidate_offer(&approval_id, "answered on this computer");
        assert!(matches!(
            registry.consume(&approval_id, "once", None, &context(Uuid::nil())),
            Err(ConsumeError::Invalidated(reason)) if reason == "answered on this computer"
        ));
    }

    #[test]
    fn answers_must_match_the_questions_asked() {
        let registry = ApprovalRegistry::new();
        let offer = question_offer(&registry);
        let approval_id = offer.approval_id().to_string();
        let context = context(Uuid::nil());

        // A custom answer alone is a complete answer.
        let custom = vec![UserInputAnswer {
            question_id: "which".to_owned(),
            selected_option_ids: Vec::new(),
            custom_text: Some("Gamma".to_owned()),
        }];
        assert!(registry
            .consume_user_input(&approval_id, &custom, &context)
            .is_ok());

        // A re-announced question starts a fresh offer.
        let offer = question_offer(&registry);
        let approval_id = offer.approval_id().to_string();
        let unknown_option = vec![UserInputAnswer {
            question_id: "which".to_owned(),
            selected_option_ids: vec!["opt-9".to_owned()],
            custom_text: None,
        }];
        assert!(matches!(
            registry.consume_user_input(&approval_id, &unknown_option, &context),
            Err(ConsumeError::InvalidAnswer(_))
        ));

        let offer = question_offer(&registry);
        let approval_id = offer.approval_id().to_string();
        let unknown_question = vec![UserInputAnswer {
            question_id: "nonexistent".to_owned(),
            selected_option_ids: vec!["opt-0".to_owned()],
            custom_text: None,
        }];
        assert!(matches!(
            registry.consume_user_input(&approval_id, &unknown_question, &context),
            Err(ConsumeError::InvalidAnswer(_))
        ));

        // A valid selection maps to the option's label, not its id.
        let offer = question_offer(&registry);
        let approval_id = offer.approval_id().to_string();
        let selected = vec![UserInputAnswer {
            question_id: "which".to_owned(),
            selected_option_ids: vec!["opt-1".to_owned()],
            custom_text: None,
        }];
        let mapped = registry
            .consume_user_input(&approval_id, &selected, &context)
            .expect("a valid selection is consumed");
        assert_eq!(mapped[0].answers, vec!["Beta".to_owned()]);
    }

    #[test]
    fn pending_view_only_shows_open_offers_for_that_device() {
        let (registry, approval_id, _) = registry_with_offer();
        assert_eq!(registry.pending_for_device(Uuid::nil()).len(), 1);
        assert!(registry.pending_for_device(Uuid::max()).is_empty());
        let _ = registry.consume(&approval_id, "once", None, &context(Uuid::nil()));
        assert!(registry.pending_for_device(Uuid::nil()).is_empty());
    }

    #[test]
    fn invalidation_scopes_to_session_device_or_everything() {
        let registry = ApprovalRegistry::new();
        let a = registry.create_permission_offer(
            Uuid::nil(),
            Uuid::nil(),
            Uuid::nil(),
            "per_a".to_owned(),
            "a".to_owned(),
            "a".to_owned(),
            vec![("once".into(), "once".into())],
            "digest-a".to_owned(),
            1,
        );
        let b = registry.create_permission_offer(
            Uuid::nil(),
            Uuid::max(),
            Uuid::max(),
            "per_b".to_owned(),
            "b".to_owned(),
            "b".to_owned(),
            vec![("once".into(), "once".into())],
            "digest-b".to_owned(),
            1,
        );
        registry.invalidate_session(Uuid::nil(), "turn ended");
        assert!(registry.offer_snapshot(&a.approval_id().to_string()).is_some());
        assert!(matches!(
            registry.consume(&a.approval_id().to_string(), "once", None, &context(Uuid::nil())),
            Err(ConsumeError::Invalidated(_))
        ));
        let other_runtime = DecisionContext {
            device_id: Uuid::nil(),
            grant_version: 1,
            runtime_id: Some(Uuid::max()),
        };
        assert!(registry
            .consume(&b.approval_id().to_string(), "once", None, &other_runtime)
            .is_ok());
    }

    #[test]
    fn decision_bodies_round_trip_through_the_wire_shape() {
        let registry = ApprovalRegistry::new();
        let offer = registry.create_permission_offer(
            Uuid::nil(),
            Uuid::nil(),
            Uuid::nil(),
            "per_1".to_owned(),
            "run bash".to_owned(),
            "detail".to_owned(),
            vec![("once".into(), "Allow once".into())],
            "digest-1".to_owned(),
            1,
        );
        let body = offer.body();
        let text = serde_json::to_string(&body).expect("offer body serializes");
        assert!(text.contains("permissionOffer"));
        let parsed: MessageBody = serde_json::from_str(&text).expect("offer body parses");
        assert!(matches!(parsed, MessageBody::PermissionOffer(_)));
        // A decision is refused for an offer that was never created.
        let stray = PermissionDecision {
            approval_id: Uuid::new_v4(),
            option_id: "once".to_owned(),
            decided_at_ms: 0,
        };
        assert!(matches!(
            registry.consume(&stray.approval_id.to_string(), &stray.option_id, None, &context(Uuid::nil())),
            Err(ConsumeError::UnknownOrConsumed)
        ));
        let stray_answers = UserInputDecision {
            approval_id: Uuid::new_v4(),
            answers: Vec::new(),
        };
        assert!(matches!(
            registry.consume_user_input(&stray_answers.approval_id.to_string(), &[], &context(Uuid::nil())),
            Err(ConsumeError::UnknownOrConsumed)
        ));
    }
}

//! The host-managed per-session prompt queue.
//!
//! An ordinary send while the session is busy is *accepted and queued*,
//! never silently turned into a steer and never dropped. Only an explicit
//! `CancelTurn` interrupts a running turn; a queued prompt runs when the
//! session goes idle, in order.
//!
//! The queue is bounded in three ways, because an unbounded one is a
//! memory defect the device can trigger by typing: at most this many
//! prompts per session, at most this many prompt bytes per session, and
//! every item carries the operation's own validity window. A full queue
//! refuses visibly; an expired item is reported expired and never runs.
//!
//! What the queue is *not*: a claim that content executed. `queued` means
//! the host accepted the input durably and will run it; `dispatching`
//! means it is being handed to the driver; only a separate `executed`
//! outcome says the turn ran. Those states are never conflated, and a
//! cancelled, expired, revoked or deleted target can never be completed
//! later.

use std::collections::{HashMap, VecDeque};

use fintwind_remote_protocol::{QueueItemEntry, RejectCode};
use uuid::Uuid;

/// Prompts one session may hold.
pub const MAX_QUEUE_ITEMS: usize = 16;

/// Prompt text one session may hold queued.
pub const MAX_QUEUE_PROMPT_BYTES: usize = 64 * 1024;

/// Prompts the whole host may hold queued, across every session.
///
/// Per-session caps alone are not a bound: with enough sessions, each
/// individually under its own cap, the host could still hold an unbounded
/// total. This is the second, global ceiling.
pub const MAX_TOTAL_QUEUE_ITEMS: usize = 256;

/// How many settled items one session keeps for its own bookkeeping.
///
/// A terminal item's prompt text is dropped immediately — the outcome was
/// already reported, and keeping the body would let a long-lived session
/// grow without bound while its visible queue stayed small.
const RETAINED_TERMINAL_ITEMS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueItemState {
    /// Accepted, waiting for the session to go idle.
    Queued,
    /// Handed to the driver; the outcome follows as its own result.
    Dispatching,
    /// The driver accepted it.
    Dispatched,
    /// Removed by the device before dispatch.
    Cancelled,
    /// Its validity window elapsed; it will never run.
    Expired,
}

impl QueueItemState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatching => "dispatching",
            Self::Dispatched => "dispatched",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
}

#[derive(Clone, Debug)]
pub struct QueuedPrompt {
    pub operation_id: Uuid,
    /// The device that sent it; results and queue views are addressed to
    /// it, and its revocation drops the item.
    pub device_id: Uuid,
    /// The grant version the daemon recorded this prompt under. A device's
    /// live version moves when it is re-paired; this one does not, so the
    /// host's own cleanup can always quote what the row itself carries.
    pub grant_version: u64,
    pub session_id: Uuid,
    pub prompt: String,
    /// The frozen human summary confirmed when the operation was sent.
    pub summary: String,
    pub enqueued_at_ms: u64,
    pub expires_at_ms: u64,
    pub state: QueueItemState,
    /// The device's cancel operation awaiting an outcome for this prompt, if
    /// any. Set when the daemon could not honour the cancel yet — the row was
    /// mid-dispatch, so the outcome was still being decided. It lives here
    /// until the intent is honoured or the prompt is shown to have run, so a
    /// cancel is never silently dropped and the device is never left to
    /// retry.
    pub cancel_requested: Option<Uuid>,
}

impl QueuedPrompt {
    pub fn entry(&self, position: usize) -> QueueItemEntry {
        QueueItemEntry {
            operation_id: self.operation_id,
            position,
            state: self.state.label().to_owned(),
            summary: self.summary.clone(),
            enqueued_at_ms: self.enqueued_at_ms,
            expires_at_ms: self.expires_at_ms,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            QueueItemState::Dispatched | QueueItemState::Cancelled | QueueItemState::Expired
        )
    }
}

/// Every session's queue.
#[derive(Default)]
pub struct PromptQueues {
    sessions: HashMap<Uuid, VecDeque<QueuedPrompt>>,
}

/// Why one enqueue was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueRefusal {
    /// The session's queue is at its item or byte cap, or the host-wide
    /// ceiling is reached.
    Full,
}

impl PromptQueues {
    /// Reconcile one authoritative daemon row without adding another copy.
    /// A local projection may have settled during the lost-link window; the
    /// durable queued row is still pending. Preserve an accepted local cancel.
    pub fn restore(&mut self, mut item: QueuedPrompt) -> Result<usize, EnqueueRefusal> {
        if let Some(queue) = self.sessions.get_mut(&item.session_id)
            && let Some(position) = queue.iter().position(|known| known.operation_id == item.operation_id)
        {
            item.cancel_requested = item.cancel_requested.or(queue[position].cancel_requested);
            queue[position] = item;
            return Ok(position);
        }
        self.enqueue(item)
    }

    /// Queue one prompt, returning its zero-based position.
    pub fn enqueue(&mut self, item: QueuedPrompt) -> Result<usize, EnqueueRefusal> {
        if self.total_pending() >= MAX_TOTAL_QUEUE_ITEMS {
            return Err(EnqueueRefusal::Full);
        }
        let queue = self.sessions.entry(item.session_id).or_default();
        let pending: usize = queue.iter().filter(|item| !item.is_terminal()).count();
        if pending >= MAX_QUEUE_ITEMS {
            return Err(EnqueueRefusal::Full);
        }
        let pending_bytes: usize = queue
            .iter()
            .filter(|item| !item.is_terminal())
            .map(|item| item.prompt.len())
            .sum();
        if pending_bytes + item.prompt.len() > MAX_QUEUE_PROMPT_BYTES {
            return Err(EnqueueRefusal::Full);
        }
        queue.push_back(item);
        Ok(pending)
    }

    /// Prompts still waiting across every session, for the global bound.
    pub fn total_pending(&self) -> usize {
        self.sessions
            .values()
            .map(|queue| queue.iter().filter(|item| !item.is_terminal()).count())
            .sum()
    }

    /// Drop a settled item's prompt text and trim the retained tail.
    ///
    /// A terminal item's outcome was already reported to its device, so its
    /// body has no remaining use: keeping it would let a session that sends
    /// all day grow without bound while its visible queue stays small.
    fn settle(&mut self, session_id: Uuid, operation_id: Uuid, state: QueueItemState) {
        let Some(queue) = self.sessions.get_mut(&session_id) else {
            return;
        };
        for item in queue.iter_mut() {
            if item.operation_id == operation_id {
                item.state = state;
                // The text goes as soon as the item is settled, before any
                // trimming, so it is never retained even briefly.
                item.prompt = String::new();
                item.summary = String::new();
            }
        }
        while queue
            .iter()
            .filter(|item| item.is_terminal())
            .count()
            > RETAINED_TERMINAL_ITEMS
        {
            let Some(position) = queue.iter().position(|item| item.is_terminal()) else {
                break;
            };
            queue.remove(position);
        }
        if queue.is_empty() {
            self.sessions.remove(&session_id);
        }
    }

    /// Cancel one queued prompt. Only the device that sent it may cancel
    /// it, and only while it has not been handed to the driver.
    pub fn cancel(&mut self, session_id: Uuid, operation_id: Uuid, device_id: Uuid) -> bool {
        let Some(queue) = self.sessions.get_mut(&session_id) else {
            return false;
        };
        let Some(item) = queue
            .iter_mut()
            .find(|item| item.operation_id == operation_id && item.device_id == device_id)
        else {
            return false;
        };
        if item.state != QueueItemState::Queued {
            return false;
        }
        self.settle(session_id, operation_id, QueueItemState::Cancelled);
        true
    }

    /// Record that the device wants this prompt cancelled, whatever state the
    /// item is in, naming the cancel operation that asked.
    ///
    /// Returns whether the item exists and belongs to that device — the
    /// caller still has to ask the daemon, which owns the authoritative
    /// answer. Recording the intent *before* asking is what makes it survive
    /// a `dispatching` refusal: the daemon declines only while the outcome is
    /// genuinely being decided, and this host honours the intent the moment
    /// that decision lands.
    pub fn request_cancel(
        &mut self,
        session_id: Uuid,
        operation_id: Uuid,
        device_id: Uuid,
        cancel_operation_id: Uuid,
    ) -> bool {
        let Some(queue) = self.sessions.get_mut(&session_id) else {
            return false;
        };
        let Some(item) = queue
            .iter_mut()
            .find(|item| item.operation_id == operation_id && item.device_id == device_id)
        else {
            return false;
        };
        if item.is_terminal() {
            return false;
        }
        item.cancel_requested = Some(cancel_operation_id);
        true
    }

    /// Take the cancel awaiting an outcome for this item, if any, clearing it.
    pub fn take_cancel_request(&mut self, session_id: Uuid, operation_id: Uuid) -> Option<Uuid> {
        let item = self
            .sessions
            .get_mut(&session_id)?
            .iter_mut()
            .find(|item| item.operation_id == operation_id)?;
        item.cancel_requested.take()
    }

    /// The cancel operation awaiting an outcome for this item, without
    /// clearing it.
    ///
    /// Reading without clearing is deliberate: the intent is only dropped
    /// once the daemon has answered authoritatively, so an unanswered query
    /// leaves the prompt exactly as it was — still cancelled, still not
    /// dispatched.
    pub fn pending_cancel_operation(
        &self,
        session_id: Uuid,
        operation_id: Uuid,
    ) -> Option<Uuid> {
        self.sessions
            .get(&session_id)?
            .iter()
            .find(|item| item.operation_id == operation_id)?
            .cancel_requested
    }

    /// Whether a cancel is waiting to be honoured for this item.
    pub fn cancel_requested(&self, session_id: Uuid, operation_id: Uuid) -> bool {
        self.sessions
            .get(&session_id)
            .and_then(|queue| queue.iter().find(|item| item.operation_id == operation_id))
            .is_some_and(|item| item.cancel_requested.is_some())
    }

    /// The grant version one queued item was admitted under, wherever it
    /// sits in any session's queue.
    /// The device one queued item belongs to, wherever it sits.
    pub fn device_for(&self, operation_id: Uuid) -> Option<Uuid> {
        self.sessions
            .values()
            .flatten()
            .find(|item| item.operation_id == operation_id)
            .map(|item| item.device_id)
    }

    pub fn recorded_grant_version(&self, operation_id: Uuid) -> Option<u64> {
        self.sessions
            .values()
            .flatten()
            .find(|item| item.operation_id == operation_id)
            .map(|item| item.grant_version)
    }

    pub fn mark_dispatching(&mut self, session_id: Uuid, operation_id: Uuid) {
        if let Some(item) = self.find_mut(session_id, operation_id) {
            item.state = QueueItemState::Dispatching;
        }
    }

    /// Return one item to waiting after the daemon declined to dispatch it.
    ///
    /// The daemon answers `queued` when the session is busy or another
    /// caller holds the claim: the prompt is still on the books and a later
    /// tick takes it. Leaving it `dispatching` would hide it from the next
    /// tick and from the queue the device sees.
    pub fn mark_queued(&mut self, session_id: Uuid, operation_id: Uuid) {
        if let Some(item) = self.find_mut(session_id, operation_id)
            && item.state == QueueItemState::Dispatching
        {
            item.state = QueueItemState::Queued;
        }
    }

    pub fn mark_dispatched(&mut self, session_id: Uuid, operation_id: Uuid) {
        self.settle(session_id, operation_id, QueueItemState::Dispatched);
    }

    fn find_mut(&mut self, session_id: Uuid, operation_id: Uuid) -> Option<&mut QueuedPrompt> {
        self.sessions
            .get_mut(&session_id)?
            .iter_mut()
            .find(|item| item.operation_id == operation_id)
    }

    /// Expire every item whose validity window elapsed, returning them so
    /// the caller can report each one as expired.
    pub fn expire_due(&mut self, now_ms: u64) -> Vec<QueuedPrompt> {
        let mut expired = Vec::new();
        let mut touched: Vec<Uuid> = Vec::new();
        for (session_id, queue) in self.sessions.iter_mut() {
            let mut expired_here = false;
            for item in queue.iter_mut() {
                if item.state == QueueItemState::Queued && item.expires_at_ms <= now_ms {
                    item.state = QueueItemState::Expired;
                    // The text goes as soon as the item settles: the outcome
                    // is about to be reported, and the body has no use after.
                    item.prompt = String::new();
                    item.summary = String::new();
                    expired.push(item.clone());
                    expired_here = true;
                }
            }
            if expired_here {
                touched.push(*session_id);
            }
        }
        for session_id in touched {
            self.trim_terminal(session_id);
        }
        expired
    }

    /// The next prompt that may run: the oldest still-queued item.
    pub fn next_dispatchable(&self, session_id: Uuid) -> Option<QueuedPrompt> {
        self.sessions
            .get(&session_id)?
            .iter()
            .find(|item| item.state == QueueItemState::Queued)
            .cloned()
    }

    /// Drop every item belonging to one device (grant revoked, device
    /// gone). Returned so the caller can report each as refused.
    pub fn drop_device(&mut self, device_id: Uuid) -> Vec<QueuedPrompt> {
        let mut dropped = Vec::new();
        let mut touched: Vec<Uuid> = Vec::new();
        for (session_id, queue) in self.sessions.iter_mut() {
            let mut dropped_here = false;
            for item in queue.iter_mut() {
                if item.device_id == device_id && !item.is_terminal() {
                    item.state = QueueItemState::Cancelled;
                    item.prompt = String::new();
                    item.summary = String::new();
                    dropped.push(item.clone());
                    dropped_here = true;
                }
            }
            if dropped_here {
                touched.push(*session_id);
            }
        }
        for session_id in touched {
            self.trim_terminal(session_id);
        }
        dropped
    }

    /// Drop every item for one session (it was deleted). Returned so the
    /// caller can report each as refused — a deleted session's queued
    /// prompts never run anywhere.
    pub fn drop_session(&mut self, session_id: Uuid) -> Vec<QueuedPrompt> {
        let Some(queue) = self.sessions.remove(&session_id) else {
            return Vec::new();
        };
        queue
            .into_iter()
            .filter(|item| !item.is_terminal())
            .map(|mut item| {
                item.state = QueueItemState::Cancelled;
                item
            })
            .collect()
    }

    /// Trim one session's settled tail and forget an empty queue.
    fn trim_terminal(&mut self, session_id: Uuid) {
        let Some(queue) = self.sessions.get_mut(&session_id) else {
            return;
        };
        while queue
            .iter()
            .filter(|item| item.is_terminal())
            .count()
            > RETAINED_TERMINAL_ITEMS
        {
            let Some(position) = queue.iter().position(|item| item.is_terminal()) else {
                break;
            };
            queue.remove(position);
        }
        if queue.is_empty() {
            self.sessions.remove(&session_id);
        }
    }

    /// The visible queue for one session, terminal items dropped.
    pub fn snapshot(&self, session_id: Uuid) -> Vec<QueueItemEntry> {
        let Some(queue) = self.sessions.get(&session_id) else {
            return Vec::new();
        };
        queue
            .iter()
            .filter(|item| !item.is_terminal())
            .enumerate()
            .map(|(position, item)| item.entry(position))
            .collect()
    }

    /// Sessions with something waiting, for the dispatch tick.
    pub fn pending_sessions(&self) -> Vec<Uuid> {
        self.sessions
            .iter()
            .filter(|(_, queue)| {
                queue.iter().any(|item| item.state == QueueItemState::Queued)
            })
            .map(|(session_id, _)| *session_id)
            .collect()
    }
}

/// Map an enqueue refusal onto the wire refusal the device is told.
pub fn refusal_code(refusal: &EnqueueRefusal) -> RejectCode {
    match refusal {
        EnqueueRefusal::Full => RejectCode::RateLimited,
    }
}

/// The message a full queue carries.
pub fn refusal_message(refusal: &EnqueueRefusal) -> &'static str {
    match refusal {
        EnqueueRefusal::Full => "This session's queue is full; wait for a prompt to finish.",
    }
}

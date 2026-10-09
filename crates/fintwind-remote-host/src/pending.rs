//! The bounded host-originated outbound queue.
//!
//! Everything the host pushes toward the device — driver events, transcript
//! snapshots, session lists, approval offers, operation results — waits
//! here until the relay session loop can seal and send it. The queue is
//! bounded in three ways, because an unbounded one is a memory defect the
//! device can trigger by simply not reading:
//!
//! - **Count and bytes** (aligned with the relay's per-device queue caps).
//! - **TTL** (aligned with the device-side pending-input TTL): a message
//!   older than the window is worthless, the device resynchronizes from a
//!   snapshot instead.
//! - **Priority under pressure.** Events are re-derivable — the next
//!   session-list push or turn reconciliation replaces them — so they are
//!   evicted first, oldest first. Results are never evicted to make room
//!   for an event. A result that still cannot be kept is dropped only
//!   because it is *also* persisted in the dedup store, from which the
//!   device recovers it through `QueryOperationResult`; that path is the
//!   guarantee this module relies on, and it is logged when used.
//!
//! Backpressure is therefore explicit: `push` reports what happened to the
//! message, and the session loop refuses to send (and tears the link down)
//! before the wire when the sent-unacked buffer is full — never after.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fintwind_remote_protocol::SealedMessage;

/// What a queued message is worth when the queue is under pressure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingKind {
    /// Event-stream traffic: driver events, transcripts, session lists,
    /// approval offers. Re-derivable from the host's authoritative state.
    Event,
    /// An operation result. Persisted in the dedup store, so a lost copy is
    /// recoverable through `QueryOperationResult` — but delivery is still
    /// preferred, because the device should not have to ask.
    Result,
}

/// One queued host-originated message.
#[derive(Clone, Debug)]
pub struct PendingMessage {
    pub message: SealedMessage,
    pub kind: PendingKind,
    /// Serialized wire size, measured once at push time so queue
    /// accounting never re-serializes a large transcript.
    pub bytes: usize,
    pub enqueued_at_ms: u64,
}

/// What one push did with the message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingVerdict {
    /// The message is queued as-is.
    Queued,
    /// The message is queued; the queue made room by evicting older events.
    QueuedAfterEviction,
    /// The message was dropped. For a result this means the dedup record is
    /// the only copy left and the device must query for it.
    Dropped,
}

/// Message-count cap. Mirrors the relay's per-device queue depth.
pub const MAX_PENDING_MESSAGES: usize = 4096;

/// Byte cap. Mirrors the relay's per-device queue bytes.
pub const MAX_PENDING_BYTES: usize = 32 * 1024 * 1024;

/// How long a queued message stays worth sending. Equal to the device-side
/// pending-input TTL so both ends age out together.
pub const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct Lane {
    messages: VecDeque<PendingMessage>,
    bytes: usize,
}

impl Lane {
    fn push_back(&mut self, message: PendingMessage) {
        self.bytes += message.bytes;
        self.messages.push_back(message);
    }

    fn push_front(&mut self, message: PendingMessage) {
        self.bytes += message.bytes;
        self.messages.push_front(message);
    }

    fn pop_front(&mut self) -> Option<PendingMessage> {
        let message = self.messages.pop_front()?;
        self.bytes = self.bytes.saturating_sub(message.bytes);
        Some(message)
    }

    fn sweep_expired(&mut self, ttl: Duration) {
        let cutoff = now_ms().saturating_sub(ttl.as_millis() as u64);
        while self
            .messages
            .front()
            .is_some_and(|message| message.enqueued_at_ms < cutoff)
        {
            self.pop_front();
        }
    }
}

/// The bounded queue. Two lanes so a flood of events can never delay a
/// result, and results can never be evicted by an event.
#[derive(Default)]
pub struct PendingOutbound {
    events: Lane,
    results: Lane,
}

impl PendingOutbound {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.events.messages.len() + self.results.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes(&self) -> usize {
        self.events.bytes + self.results.bytes
    }

    /// Queue one host-originated message, reporting what became of it.
    pub fn push(&mut self, message: SealedMessage, kind: PendingKind) -> PendingVerdict {
        self.events.sweep_expired(PENDING_TTL);
        self.results.sweep_expired(PENDING_TTL);
        let bytes = serialized_len(&message);
        let queued = PendingMessage {
            message,
            kind,
            bytes,
            enqueued_at_ms: now_ms(),
        };
        let mut evicted = false;
        // Events never displace results; a result may evict the oldest
        // events. If only results remain and they do not fit, the queue is
        // full of results — which means the device is not reading at all —
        // and the oldest result is dropped (the dedup record keeps it).
        while self.over_capacity(bytes) {
            let dropped = match kind {
                PendingKind::Event => self.events.pop_front(),
                PendingKind::Result => self.events.pop_front().or_else(|| self.results.pop_front()),
            };
            match dropped {
                Some(_) => evicted = true,
                None => {
                    log::warn!(
                        target: "audit",
                        "outbound queue full; dropping {} message",
                        match kind {
                            PendingKind::Event => "event",
                            PendingKind::Result => "result (recoverable via QueryOperationResult)",
                        }
                    );
                    return PendingVerdict::Dropped;
                }
            }
        }
        match kind {
            PendingKind::Event => self.events.push_back(queued),
            PendingKind::Result => self.results.push_back(queued),
        }
        if evicted {
            PendingVerdict::QueuedAfterEviction
        } else {
            PendingVerdict::Queued
        }
    }

    fn over_capacity(&self, incoming: usize) -> bool {
        self.len() + 1 > MAX_PENDING_MESSAGES || self.bytes() + incoming > MAX_PENDING_BYTES
    }

    /// Take up to `max` messages, results first so a device that reconnects
    /// mid-turn still learns the outcome before the next delta. Expired
    /// messages are dropped here rather than sent.
    pub fn take_batch(&mut self, max: usize) -> Vec<PendingMessage> {
        let mut batch = Vec::new();
        while batch.len() < max {
            self.results.sweep_expired(PENDING_TTL);
            let next = self
                .results
                .pop_front()
                .or_else(|| self.events.pop_front());
            match next {
                Some(message) => batch.push(message),
                None => break,
            }
        }
        batch
    }

    /// Put unsent messages back at the front, preserving order. Used when a
    /// dispatch fails part-way through a batch: the message that failed and
    /// everything after it return to the head of their lanes. Capacity is
    /// not re-checked — these messages were just taken out, and the next
    /// push evicts if the queue genuinely overflowed meanwhile.
    pub fn requeue_front(&mut self, messages: Vec<PendingMessage>) {
        for message in messages.into_iter().rev() {
            match message.kind {
                PendingKind::Event => self.events.push_front(message),
                PendingKind::Result => self.results.push_front(message),
            }
        }
    }

    /// Put messages back at the tail, preserving order. Used for messages
    /// whose device has no live channel: they wait for that device without
    /// blocking another device's traffic.
    pub fn requeue_back(&mut self, messages: Vec<PendingMessage>) {
        for message in messages {
            match message.kind {
                PendingKind::Event => self.events.push_back(message),
                PendingKind::Result => self.results.push_back(message),
            }
        }
    }
}

/// The serialized wire size of one sealed message. Measured, not
/// estimated: the byte cap is a memory bound, and a transcript snapshot is
/// exactly the message that must not be undercounted.
fn serialized_len(message: &SealedMessage) -> usize {
    serde_json::to_vec(message).map(|bytes| bytes.len()).unwrap_or(0)
}

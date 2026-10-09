//! Minimal aggregate counters for operations and privacy review.
//!
//! Deliberately aggregate-only. There is no per-subject, per-connection,
//! or per-payload dimension anywhere in this module, so nothing here can
//! grow into a surveillance surface: an operator can see that refusals
//! are happening and which kind, never who or what. Payloads never enter
//! a log line either — the counters are the whole story, and the only
//! place they surface is the public `/api/health` answer as totals.

use std::sync::atomic::{AtomicU64, Ordering};

use fintwind_remote_protocol::RejectCode;

/// Rejection codes as a fixed index set, in wire (camelCase) order.
pub const REJECT_CODE_NAMES: [&str; 6] = [
    "authFailed",
    "versionMismatch",
    "unknownRoute",
    "rateLimited",
    "queueFull",
    "malformed",
];

fn code_index(code: RejectCode) -> usize {
    match code {
        RejectCode::AuthFailed => 0,
        RejectCode::VersionMismatch => 1,
        RejectCode::UnknownRoute => 2,
        RejectCode::RateLimited => 3,
        RejectCode::QueueFull => 4,
        RejectCode::Malformed => 5,
    }
}

/// Process-wide counters. Atomics because the connection threads update
/// them without coordination.
#[derive(Default)]
pub struct Metrics {
    auth_accepted: AtomicU64,
    auth_rejected: AtomicU64,
    frames_routed: AtomicU64,
    timeouts: AtomicU64,
    unauthenticated_refused: AtomicU64,
    rejections: [AtomicU64; REJECT_CODE_NAMES.len()],
}

/// A consistent-enough read of every counter. Not a transaction: each
/// field is one atomic load, and a health answer does not need more.
#[derive(Clone, Copy, Debug, Default)]
pub struct MetricsSnapshot {
    pub auth_accepted: u64,
    pub auth_rejected: u64,
    pub frames_routed: u64,
    pub timeouts: u64,
    pub unauthenticated_refused: u64,
    pub rejections: [u64; REJECT_CODE_NAMES.len()],
}

impl Metrics {
    pub fn auth_accepted(&self) {
        self.auth_accepted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn auth_rejected(&self) {
        self.auth_rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn frame_routed(&self) {
        self.frames_routed.fetch_add(1, Ordering::Relaxed);
    }

    /// Any deadline that ended a connection or an admission window:
    /// authentication window, idle session, stuck write.
    pub fn timeout(&self) {
        self.timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn unauthenticated_refused(&self) {
        self.unauthenticated_refused.fetch_add(1, Ordering::Relaxed);
    }

    /// One relay-visible refusal, counted by code.
    pub fn rejection(&self, code: RejectCode) {
        self.rejections[code_index(code)].fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let mut rejections = [0u64; REJECT_CODE_NAMES.len()];
        for (slot, counter) in rejections.iter_mut().zip(self.rejections.iter()) {
            *slot = counter.load(Ordering::Relaxed);
        }
        MetricsSnapshot {
            auth_accepted: self.auth_accepted.load(Ordering::Relaxed),
            auth_rejected: self.auth_rejected.load(Ordering::Relaxed),
            frames_routed: self.frames_routed.load(Ordering::Relaxed),
            timeouts: self.timeouts.load(Ordering::Relaxed),
            unauthenticated_refused: self.unauthenticated_refused.load(Ordering::Relaxed),
            rejections,
        }
    }
}

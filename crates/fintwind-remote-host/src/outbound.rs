//! The bounded outbound buffer: frames sent to the device but not yet
//! acknowledged end-to-end.
//!
//! Layered semantics (architecture document, 分层 ack): a relay ack only
//! proves relay receipt and releases nothing here; only the device's
//! `Acknowledged { upto_seq }` does. A connection tear-down also empties
//! the buffer, because every reconnect runs a fresh Noise key period and
//! old ciphertext can never be replayed — event recovery across
//! reconnects goes through cursor resume from the daemon's journal, not
//! through this buffer. What the buffer buys is the ack-timeout tripwire
//! (a dead link is detected in seconds, not TCP minutes) and a hard
//! memory bound on in-flight traffic.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use fintwind_remote_protocol::Frame;

use crate::config::{ACK_TIMEOUT, REPLAY_BUFFER_GRACE};

/// Message and byte caps (M0 table), shared with the relay's queue caps.
pub const BUFFER_MAX_MESSAGES: usize = 4096;
pub const BUFFER_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug)]
pub struct OutboundFrame {
    pub seq: u64,
    pub frame: Frame,
    pub bytes: usize,
    pub sent_at: Instant,
}

/// Why an outbound session stopped being viable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferVerdict {
    /// The oldest unacked frame outlived `ACK_TIMEOUT` with no progress:
    /// the connection is presumed dead.
    AckTimeout,
    /// The oldest unacked frame outlived the grace window: abandon the
    /// session so the device resynchronizes from a snapshot.
    Abandon,
}

#[derive(Default)]
pub struct BoundedOutboundBuffer {
    queue: VecDeque<OutboundFrame>,
    bytes: usize,
    next_seq: u64,
}

impl BoundedOutboundBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve the next delivery sequence. Assigned at enqueue time and
    /// never reused, so device-side ordering and dedup stay stable even
    /// across reconnects.
    pub fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// Record a frame as sent. Refuses (with the frame handed back) when
    /// the buffer is full — backpressure is visible, never silent loss.
    pub fn mark_sent(&mut self, seq: u64, frame: Frame, bytes: usize) -> Result<(), (u64, Frame, usize)> {
        if self.queue.len() + 1 > BUFFER_MAX_MESSAGES || self.bytes + bytes > BUFFER_MAX_BYTES {
            return Err((seq, frame, bytes));
        }
        self.bytes += bytes;
        self.queue.push_back(OutboundFrame {
            seq,
            frame,
            bytes,
            sent_at: Instant::now(),
        });
        Ok(())
    }

    /// Release everything acknowledged by the device.
    pub fn acknowledge(&mut self, upto_seq: u64) {
        while let Some(oldest) = self.queue.front() {
            if oldest.seq > upto_seq {
                break;
            }
            if let Some(released) = self.queue.pop_front() {
                self.bytes = self.bytes.saturating_sub(released.bytes);
            }
        }
    }

    /// The timeout verdict for the oldest unacked frame, if any.
    pub fn deadline_check(&self) -> Option<BufferVerdict> {
        let oldest = self.queue.front()?;
        let waited = oldest.sent_at.elapsed();
        if waited > REPLAY_BUFFER_GRACE {
            Some(BufferVerdict::Abandon)
        } else if waited > ACK_TIMEOUT {
            Some(BufferVerdict::AckTimeout)
        } else {
            None
        }
    }

    /// Drop everything (connection rebuilt / session abandoned). Sequence
    /// numbers keep climbing; only ciphertext dies here.
    pub fn clear(&mut self) {
        self.queue.clear();
        self.bytes = 0;
    }

    pub fn pending_messages(&self) -> usize {
        self.queue.len()
    }

    /// Whether any frame has been waiting past `duration`, used by the
    /// session loop to decide when to run the deadline check.
    pub fn oldest_waited(&self) -> Option<Duration> {
        self.queue.front().map(|frame| frame.sent_at.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fintwind_remote_protocol::ControlFrame;

    fn frame(seq: u64) -> (u64, Frame, usize) {
        (
            seq,
            Frame::Control {
                frame: ControlFrame::Ping { sent_at_ms: seq },
            },
            64,
        )
    }

    /// Ways this buffer can fail: an unbounded flood of in-flight frames,
    /// a lost ack holding memory forever, an ack that releases frames it
    /// should not, or a dead link never noticed.
    #[test]
    fn buffer_refuses_when_full_and_ack_releases() {
        let mut buffer = BoundedOutboundBuffer::new();
        let mut overflow = None;
        for seq in 0..=(BUFFER_MAX_MESSAGES as u64) {
            let (s, frame, bytes) = frame(seq);
            if let Err(pending) = buffer.mark_sent(s, frame, bytes) {
                overflow = Some(pending.0);
                break;
            }
        }
        assert_eq!(
            overflow,
            Some(BUFFER_MAX_MESSAGES as u64),
            "buffer must refuse, not truncate"
        );
        assert_eq!(buffer.pending_messages(), BUFFER_MAX_MESSAGES);
        // Ack through seq 2048 releases 2049 frames; the rest stay.
        buffer.acknowledge(2048);
        assert_eq!(buffer.pending_messages(), BUFFER_MAX_MESSAGES - 2049);
        // Acknowledging backwards is a no-op, not an error.
        buffer.acknowledge(0);
        assert_eq!(buffer.pending_messages(), BUFFER_MAX_MESSAGES - 2049);
    }

    #[test]
    fn sequences_are_monotonic_across_clears() {
        let mut buffer = BoundedOutboundBuffer::new();
        assert_eq!(buffer.take_seq(), 0);
        assert_eq!(buffer.take_seq(), 1);
        buffer.clear();
        assert_eq!(buffer.take_seq(), 2, "clear never rewinds delivery order");
    }
}

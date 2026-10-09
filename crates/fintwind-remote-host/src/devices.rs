//! Per-device channel state: one Noise session, one delivery sequence
//! space, one unacked buffer, one fragment table — each bound to the
//! device that authenticated for it.
//!
//! The host holds one relay connection but serves every paired device
//! over it, and nothing about one device's channel may leak into
//! another's: a device that disconnects, corrupts a frame, or loses its
//! grant loses *its own* channel and nothing else. That isolation is the
//! whole point of this module.

use std::collections::HashMap;

use uuid::Uuid;

use fintwind_remote_crypto::SecureChannel;

use crate::outbound::{BoundedOutboundBuffer, BufferVerdict};

/// Inbound reassembly bounds, per device. A device that fragments a
/// message larger than one Noise frame still gets a bounded buffer: at
/// most this many concurrent groups, this many fragments each, this many
/// bytes in total, and this long to finish one group. A refused fragment
/// loses that logical message — the device notices the missing answer and
/// recovers — and nothing is silently reassembled wrong.
const MAX_FRAGMENT_GROUPS: usize = 32;
const MAX_FRAGMENTS_PER_MESSAGE: u32 = 256;
const MAX_FRAGMENT_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const FRAGMENT_GROUP_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Inbound reassembly of one device's fragmented messages. Keyed by the
/// logical message's seq, so a fresh group always starts clean — and a new
/// key period starts with an empty table, so fragments from a previous
/// channel can never be concatenated with the current one's.
#[derive(Default)]
pub struct FragmentGroups {
    groups: HashMap<u64, FragmentGroup>,
}

struct FragmentGroup {
    of: u32,
    parts: Vec<Option<Vec<u8>>>,
    arrived: usize,
    bytes: usize,
    started_at: std::time::Instant,
}

impl FragmentGroups {
    fn total_bytes(&self) -> usize {
        self.groups.values().map(|group| group.bytes).sum()
    }

    fn sweep_expired(&mut self) {
        let now = std::time::Instant::now();
        let expired: Vec<u64> = self
            .groups
            .iter()
            .filter(|(_, group)| now.duration_since(group.started_at) > FRAGMENT_GROUP_TTL)
            .map(|(seq, _)| *seq)
            .collect();
        for seq in expired {
            self.groups.remove(&seq);
        }
    }

    /// Buffer one fragment. `transport_seq` is the *logical* message's
    /// seq; the fragment's own frame seq was already verified by the
    /// caller against the frame it rode in.
    pub fn insert(&mut self, transport_seq: u64, index: u32, of: u32, data: Vec<u8>) -> bool {
        self.sweep_expired();
        if of == 0 || of > MAX_FRAGMENTS_PER_MESSAGE || index >= of {
            return false;
        }
        if !self.groups.contains_key(&transport_seq) && self.groups.len() >= MAX_FRAGMENT_GROUPS {
            return false;
        }
        if self.total_bytes() + data.len() > MAX_FRAGMENT_TOTAL_BYTES {
            return false;
        }
        let group = self.groups.entry(transport_seq).or_insert_with(|| FragmentGroup {
            of,
            parts: vec![None; of as usize],
            arrived: 0,
            bytes: 0,
            started_at: std::time::Instant::now(),
        });
        if group.of != of {
            return false;
        }
        let Some(slot) = group.parts.get_mut(index as usize) else {
            return false;
        };
        if slot.is_none() {
            group.arrived += 1;
            group.bytes += data.len();
        }
        *slot = Some(data);
        true
    }

    pub fn is_complete(&self, transport_seq: u64) -> bool {
        self.groups
            .get(&transport_seq)
            .is_some_and(|group| group.arrived == group.parts.len())
    }

    pub fn take(&mut self, transport_seq: u64) -> Option<Vec<u8>> {
        let group = self.groups.remove(&transport_seq)?;
        let mut plaintext = Vec::with_capacity(group.bytes);
        for part in group.parts.into_iter().flatten() {
            plaintext.extend_from_slice(&part);
        }
        Some(plaintext)
    }
}

/// One device's live channel over the shared relay connection.
pub struct DeviceChannel {
    pub device_id: Uuid,
    pub channel: SecureChannel,
    /// Sent-unacked frames for *this* device; only this device's
    /// end-to-end `Acknowledged` releases them.
    pub buffer: BoundedOutboundBuffer,
    pub fragments: FragmentGroups,
    /// The grant version the handshake ran under. A grant change voids the
    /// channel (and its offers) rather than letting a stale authorization
    /// keep running.
    pub grant_version: u64,
}

/// Every live device channel, keyed by device id.
#[derive(Default)]
pub struct DeviceChannels {
    channels: HashMap<Uuid, DeviceChannel>,
}

impl DeviceChannels {
    /// Install a device's fresh channel, replacing any previous one. A
    /// re-handshake means the device started a new key period, so the old
    /// channel's ciphertext is meaningless — it is dropped, not merged.
    /// Returns the replaced channel's device id when there was one.
    pub fn insert(&mut self, channel: DeviceChannel) -> Option<Uuid> {
        self.channels.insert(channel.device_id, channel).map(|old| old.device_id)
    }

    pub fn remove(&mut self, device_id: Uuid) -> bool {
        self.channels.remove(&device_id).is_some()
    }

    pub fn get(&self, device_id: Uuid) -> Option<&DeviceChannel> {
        self.channels.get(&device_id)
    }

    pub fn get_mut(&mut self, device_id: Uuid) -> Option<&mut DeviceChannel> {
        self.channels.get_mut(&device_id)
    }

    pub fn ids(&self) -> Vec<Uuid> {
        self.channels.keys().copied().collect()
    }

    pub fn contains(&self, device_id: Uuid) -> bool {
        self.channels.contains_key(&device_id)
    }

    /// Unacknowledged ciphertext across every device channel at once. The
    /// per-device caps do not bound this total, so the session loop checks
    /// it separately before putting anything else on the wire.
    pub fn total_unacked_bytes(&self) -> usize {
        self.channels
            .values()
            .map(|device| device.buffer.unacked_bytes())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// The first timeout verdict per device. Isolated on purpose: one
    /// device's dead link never tears down another's live channel.
    pub fn deadline_verdicts(&self) -> Vec<(Uuid, BufferVerdict)> {
        self.channels
            .values()
            .filter_map(|device| {
                device
                    .buffer
                    .deadline_check()
                    .map(|verdict| (device.device_id, verdict))
            })
            .collect()
    }
}

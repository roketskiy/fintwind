//! In-memory connection registry: who is online, addressed by what, and
//! which undelivered ciphertext frames wait for them.
//!
//! This is the relay's entire business state. It lives and dies with the
//! process — a relay restart means endpoints reconnect and re-resolve
//! state from their peer (architecture: business state and operation
//! records belong to the host, never here). One live connection per
//! entity: a reconnecting host or device replaces its stale connection,
//! and the replaced one is closed.

use std::collections::{HashMap, VecDeque};

use crossbeam_channel::Sender;
use parking_lot::Mutex;

use fintwind_remote_protocol::Frame;

use crate::config::{QUEUE_MAX_BYTES, QUEUE_MAX_MESSAGES, QUEUE_TTL};

/// The authenticated identity of one live connection. Roles are fixed at
/// authentication and never switch (architecture rule 4).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionRole {
    Host { host_id: String },
    Device { device_id: String },
}

impl ConnectionRole {
    pub fn subject_id(&self) -> &str {
        match self {
            Self::Host { host_id } => host_id,
            Self::Device { device_id } => device_id,
        }
    }
}

/// One live connection's outbound half. The WS write thread drains this
/// channel; the registry only ever clones the sender.
pub struct ConnectionHandle {
    pub id: u64,
    pub role: ConnectionRole,
    pub outbound: Sender<Frame>,
}

/// An undelivered host-to-device sealed frame, kept opaque on purpose.
#[derive(Clone, Debug)]
pub struct QueuedFrame {
    /// The sender's delivery sequence, preserved for order and acks.
    pub seq: u64,
    pub host_id: String,
    pub frame: Frame,
    pub enqueued_at_ms: u64,
}

#[derive(Default)]
struct DeviceQueue {
    frames: VecDeque<QueuedFrame>,
    bytes: usize,
}

#[derive(Default)]
struct RegistryInner {
    next_id: u64,
    connections: HashMap<u64, ConnectionHandle>,
    /// host_id -> connection id (the host's one live connection).
    hosts: HashMap<String, u64>,
    /// device_id -> connection id (the device's one live connection).
    devices: HashMap<String, u64>,
    /// device_id -> ciphertext waiting for that device to be online.
    queues: HashMap<String, DeviceQueue>,
    /// host_id -> device_id of the pairing attempt in flight. A host
    /// pairs one device at a time; the mapping is what routes the host's
    /// pairing reply back before the host knows the device's routing id.
    pairings: HashMap<String, String>,
}

/// Result of routing one frame toward its subject.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteOutcome {
    /// Handed to the subject's live connection.
    Delivered,
    /// The device is offline; the frame joined its bounded queue.
    Queued,
    /// No such connected subject. For device targets this follows a queue
    /// enqueue too — the sender is told the frame is queued only after it
    /// actually fits.
    NoRoute,
    /// The device's queue is full or the frame expired its window: the
    /// frame is refused, never truncated.
    QueueFull,
}

pub struct Registry {
    inner: Mutex<RegistryInner>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            inner: Mutex::new(RegistryInner {
                next_id: 0,
                connections: HashMap::new(),
                hosts: HashMap::new(),
                devices: HashMap::new(),
                queues: HashMap::new(),
                pairings: HashMap::new(),
            }),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

impl Registry {
    /// Register a connection. The returning id also names the connection
    /// this registration displaced, if any: same-entity reconnects close
    /// the stale connection instead of multiplying them.
    pub fn register(
        &self,
        role: ConnectionRole,
        outbound: Sender<Frame>,
    ) -> (u64, Option<ConnectionHandle>) {
        let mut inner = self.inner.lock();
        let id = inner.next_id;
        inner.next_id += 1;
        let subject = role.subject_id().to_owned();
        let displaced = match &role {
            ConnectionRole::Host { host_id } => inner
                .hosts
                .insert(host_id.clone(), id)
                .and_then(|old| inner.connections.remove(&old)),
            ConnectionRole::Device { device_id } => {
                inner
                    .devices
                    .insert(device_id.clone(), id)
                    .and_then(|old| inner.connections.remove(&old))
            }
        };
        inner.connections.insert(
            id,
            ConnectionHandle {
                id,
                role,
                outbound,
            },
        );
        log::info!(target: "audit", "connection registered; subject={subject} id={id}");
        (id, displaced)
    }

    /// Remove a connection if it is still the registered one (a displaced
    /// connection's late exit must not unregister its replacement).
    pub fn unregister(&self, id: u64) {
        let mut inner = self.inner.lock();
        if let Some(handle) = inner.connections.remove(&id) {
            match &handle.role {
                ConnectionRole::Host { host_id } => {
                    if inner.hosts.get(host_id) == Some(&id) {
                        inner.hosts.remove(host_id);
                    }
                }
                ConnectionRole::Device { device_id } => {
                    if inner.devices.get(device_id) == Some(&id) {
                        inner.devices.remove(device_id);
                        // Keep queued frames: the device may reconnect
                        // within the queue TTL and still receive them.
                    }
                }
            }
            log::info!(target: "audit", "connection closed; subject={} id={id}", handle.role.subject_id());
            // A pairing endpoint vanishing ends the pairing session; the
            // device restarts it with the same or a fresh invite.
            match &handle.role {
                ConnectionRole::Host { host_id } => {
                    inner.pairings.remove(host_id);
                }
                ConnectionRole::Device { device_id } => {
                    inner
                        .pairings
                        .retain(|_, peer| peer != device_id);
                }
            }
        }
    }

    /// Record that `device_id` opened a pairing handshake toward `host_id`.
    /// The host's pairing reply routes back through this mapping — before
    /// the handshake completes, the host cannot know the device's id.
    pub fn note_pairing(&self, host_id: &str, device_id: &str) {
        self.inner
            .lock()
            .pairings
            .insert(host_id.to_owned(), device_id.to_owned());
    }

    /// The device currently pairing with this host, if any.
    pub fn pairing_peer(&self, host_id: &str) -> Option<String> {
        self.inner.lock().pairings.get(host_id).cloned()
    }

    /// Explicitly end the pairing session (handshake finished either way).
    pub fn clear_pairing(&self, host_id: &str) {
        self.inner.lock().pairings.remove(host_id);
    }

    /// The sender channel of a live connection, if any.
    pub fn sender(&self, role: &ConnectionRole) -> Option<Sender<Frame>> {
        let inner = self.inner.lock();
        let id = match role {
            ConnectionRole::Host { host_id } => inner.hosts.get(host_id),
            ConnectionRole::Device { device_id } => inner.devices.get(device_id),
        }?;
        Some(inner.connections.get(id)?.outbound.clone())
    }

    /// Route one frame to a device: deliver live, or queue while offline.
    /// `frame_bytes` sizes the queue against `QUEUE_MAX_BYTES` before the
    /// frame is accepted anywhere.
    pub fn route_to_device(
        &self,
        device_id: &str,
        host_id: &str,
        seq: u64,
        frame: Frame,
        frame_bytes: usize,
    ) -> RouteOutcome {
        let mut inner = self.inner.lock();
        if let Some(id) = inner.devices.get(device_id) {
            if let Some(handle) = inner.connections.get(id) {
                return if handle.outbound.send(frame).is_ok() {
                    RouteOutcome::Delivered
                } else {
                    RouteOutcome::NoRoute
                };
            }
        }
        // Offline: bounded queue. Expired frames leave first (they are
        // already unreachable for the device's recovery window); after
        // that a full queue is a refusal, never a silent eviction — the
        // sender must know its frame will not be delivered.
        {
            let queue = inner.queues.entry(device_id.to_owned()).or_default();
            let now = now_ms();
            while queue
                .frames
                .front()
                .is_some_and(|oldest| now - oldest.enqueued_at_ms > QUEUE_TTL.as_millis() as u64)
            {
                if let Some(oldest) = queue.frames.pop_front() {
                    queue.bytes = queue.bytes.saturating_sub(oldest.frame_bytes());
                }
            }
            if frame_bytes > QUEUE_MAX_BYTES
                || queue.bytes + frame_bytes > QUEUE_MAX_BYTES
                || queue.frames.len() + 1 > QUEUE_MAX_MESSAGES
            {
                return RouteOutcome::QueueFull;
            }
            queue.bytes += frame_bytes;
            queue.frames.push_back(QueuedFrame {
                seq,
                host_id: host_id.to_owned(),
                frame,
                enqueued_at_ms: now,
            });
        }
        RouteOutcome::Queued
    }

    /// Take everything queued for a device, in order. Called right after
    /// the device authenticates, before any live traffic flows.
    pub fn drain_queue(&self, device_id: &str) -> Vec<QueuedFrame> {        let mut inner = self.inner.lock();
        inner
            .queues
            .remove(device_id)
            .map(|queue| queue.frames.into_iter().collect())
            .unwrap_or_default()
    }

    /// Route one frame to a host. Hosts are never queued: a device's
    /// commands carry their own TTL and the host replays authoritative
    /// state on reconnect, so buffering device-to-host frames here would
    /// fake durability the host does not know about.
    pub fn route_to_host(&self, host_id: &str, frame: Frame) -> RouteOutcome {
        let inner = self.inner.lock();
        match inner.hosts.get(host_id) {
            Some(id) => match inner.connections.get(id) {
                Some(handle) => {
                    if handle.outbound.send(frame).is_ok() {
                        RouteOutcome::Delivered
                    } else {
                        RouteOutcome::NoRoute
                    }
                }
                None => RouteOutcome::NoRoute,
            },
            None => RouteOutcome::NoRoute,
        }
    }

    /// Close a subject's live connection (credential revoked mid-flight).
    /// Returns whether one was closed.
    pub fn disconnect(&self, role: &ConnectionRole) -> bool {
        let subject = role.subject_id().to_owned();
        let mut inner = self.inner.lock();
        let id = match role {
            ConnectionRole::Host { host_id } => inner.hosts.remove(host_id),
            ConnectionRole::Device { device_id } => inner.devices.remove(device_id),
        };
        let Some(id) = id else {
            return false;
        };
        if let Some(handle) = inner.connections.remove(&id) {
            let _ = handle.outbound.send(Frame::Control {
                frame: fintwind_remote_protocol::ControlFrame::Rejected {
                    code: fintwind_remote_protocol::RejectCode::AuthFailed,
                    message: "credential revoked".to_owned(),
                },
            });
            log::info!(target: "audit", "connection force-closed; subject={subject} id={id}");
            true
        } else {
            false
        }
    }
}

/// Extra trait-local helper: queue accounting needs the encoded size of a
/// queued frame, stored beside it.
impl QueuedFrame {
    fn frame_bytes(&self) -> usize {
        // Approximation is fine: JSON encoding cost is close to linear in
        // the ciphertext, and the cap is an order-of-magnitude guard.
        self.frame.encoded_len_hint()
    }
}

trait EncodedLenHint {
    fn encoded_len_hint(&self) -> usize;
}

impl EncodedLenHint for Frame {
    fn encoded_len_hint(&self) -> usize {
        match self {
            Frame::Sealed { ciphertext, .. } => ciphertext.0.len() + 128,
            Frame::Handshake { payload, .. } => payload.0.len() + 128,
            Frame::LinkDevice { device_public_key, .. } => device_public_key.0.len() + 192,
            Frame::UnlinkDevice { .. } => 192,
            Frame::Control { .. } | Frame::Auth { .. } => 128,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use fintwind_remote_protocol::{Base64Bytes, ControlFrame};
    use uuid::Uuid;

    fn sealed(seq: u64, payload_len: usize) -> (Frame, usize) {
        let frame = Frame::Sealed {
            seq,
            host_id: Uuid::new_v4(),
            device_id: Some(Uuid::new_v4()),
            ciphertext: Base64Bytes(vec![0u8; payload_len]),
        };
        (frame, payload_len + 128)
    }

    fn host_role(host_id: &str) -> ConnectionRole {
        ConnectionRole::Host {
            host_id: host_id.to_owned(),
        }
    }

    fn device_role(device_id: &str) -> ConnectionRole {
        ConnectionRole::Device {
            device_id: device_id.to_owned(),
        }
    }

    /// Ways this registry can fail, driven below: an offline device loses
    /// frames silently, a full queue truncates instead of refusing, a
    /// stale connection unregisters its replacement, or a displaced
    /// connection keeps receiving traffic.
    #[test]
    fn offline_device_queues_then_drains_in_order() {
        let registry = Registry::default();
        let (frame_a, _) = sealed(1, 16);
        let (frame_b, _) = sealed(2, 16);
        assert_eq!(
            registry.route_to_device("d", "h", 1, frame_a, 144),
            RouteOutcome::Queued
        );
        assert_eq!(
            registry.route_to_device("d", "h", 2, frame_b, 144),
            RouteOutcome::Queued
        );

        let (tx, rx) = unbounded();
        let (_, displaced) = registry.register(device_role("d"), tx);
        assert!(displaced.is_none());
        let drained = registry.drain_queue("d");
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].seq, 1, "queue preserves delivery order");
        assert_eq!(drained[1].seq, 2);
        assert!(rx.try_recv().is_err(), "queued frames are not double-delivered");
    }

    #[test]
    fn queue_full_is_a_refusal_not_truncation() {
        let registry = Registry::default();
        // Fill the queue to its byte cap with one huge frame: the next
        // frame cannot fit and must be refused outright, never silently
        // dropping the queued one.
        let big = QUEUE_MAX_BYTES - 128;
        let (frame, bytes) = sealed(1, big);
        assert_eq!(
            registry.route_to_device("d", "h", 1, frame, bytes),
            RouteOutcome::Queued
        );
        let (frame, bytes) = sealed(2, big);
        assert_eq!(
            registry.route_to_device("d", "h", 2, frame, bytes),
            RouteOutcome::QueueFull
        );
        // The queue still holds exactly the first frame.
        registry.register(device_role("d"), unbounded().0);
        assert_eq!(registry.drain_queue("d").len(), 1);
    }

    #[test]
    fn same_entity_reconnect_displaces_the_old_connection() {
        let registry = Registry::default();
        let (tx_old, rx_old) = unbounded();
        let (old_id, _) = registry.register(host_role("h"), tx_old);

        let (tx_new, rx_new) = unbounded();
        let (new_id, displaced) = registry.register(host_role("h"), tx_new);
        assert_eq!(displaced.map(|handle| handle.id), Some(old_id));

        // The stale write half still drains (the channel is closed from
        // the registry side only when the handle is dropped by its owning
        // loop), but routing reaches only the new connection.
        let (frame, _) = sealed(1, 8);
        assert_eq!(
            registry.route_to_host("h", frame),
            RouteOutcome::Delivered
        );
        assert!(rx_new.try_recv().is_ok());
        assert!(rx_old.try_recv().is_err());

        // The displaced loop's unregister must not remove the replacement.
        registry.unregister(old_id);
        assert!(registry.sender(&host_role("h")).is_some());
        registry.unregister(new_id);
        assert!(registry.sender(&host_role("h")).is_none());
    }

    #[test]
    fn device_targets_never_queue_to_a_host() {
        let registry = Registry::default();
        let (frame, _) = sealed(1, 8);
        assert_eq!(
            registry.route_to_host("missing", frame),
            RouteOutcome::NoRoute
        );
    }

    #[test]
    fn disconnect_notifies_and_unregisters() {
        let registry = Registry::default();
        let (tx, rx) = unbounded();
        registry.register(device_role("d"), tx);
        assert!(registry.disconnect(&device_role("d")));
        // Rejection reached the socket loop so it can close itself.
        assert!(matches!(
            rx.try_recv().unwrap(),
            Frame::Control {
                frame: ControlFrame::Rejected { .. }
            }
        ));
        assert!(!registry.disconnect(&device_role("d")));
    }
}

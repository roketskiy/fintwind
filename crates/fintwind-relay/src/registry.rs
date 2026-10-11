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
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

use crossbeam_channel::Sender;
use parking_lot::Mutex;

use fintwind_remote_protocol::Frame;

use crate::config::{
    MAX_AUTHENTICATED_CONNECTIONS, OUTBOUND_MAX_BYTES, OUTBOUND_MAX_MESSAGES,
    OUTBOUND_TOTAL_MAX_BYTES, QUEUE_MAX_BYTES, QUEUE_MAX_MESSAGES, QUEUE_TOTAL_MAX_BYTES,
    QUEUE_TOTAL_MAX_DEVICES, QUEUE_TOTAL_MAX_MESSAGES, QUEUE_TTL,
};

/// What one pairing handshake step did to the host's pairing slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingNote {
    /// This device opened the pairing.
    Started,
    /// The same device sent another step: idempotent, the pairing
    /// continues.
    Retried,
    /// A *different* device is already pairing with this host. One
    /// invite routes to one device; the second attempt is refused
    /// instead of silently displacing the first.
    Conflict,
}

/// Admission refused: the process-wide authenticated-connection cap is
/// reached. Same-entity reconnects do not count against it (they
/// displace), so this only ever refuses a genuinely new subject.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistryFull;

/// The authenticated identity of one live connection. Roles are fixed at
/// authentication and never switch (architecture rule 4). A host occupies
/// one of two separate slots: its long-lived business session, or — while
/// a pairing is in flight — one short-lived pairing channel. The slots
/// displace independently, so pairing and business traffic can never
/// replace each other.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionRole {
    Host { host_id: String },
    HostPairing { host_id: String },
    Device { device_id: String },
}

impl ConnectionRole {
    pub fn subject_id(&self) -> &str {
        match self {
            Self::Host { host_id } | Self::HostPairing { host_id } => host_id,
            Self::Device { device_id } => device_id,
        }
    }

    /// The host id when this connection belongs to a host, in either slot.
    pub fn host_id(&self) -> Option<&str> {
        match self {
            Self::Host { host_id } | Self::HostPairing { host_id } => Some(host_id),
            Self::Device { .. } => None,
        }
    }

    /// Whether this connection may declare links (both host slots may:
    /// the link declaration rides the pairing channel it completes).
    pub fn is_host(&self) -> bool {
        matches!(self, Self::Host { .. } | Self::HostPairing { .. })
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

/// One live connection's outbound buffer accounting. The channel itself
/// is unbounded (frames are never dropped), but the accounting is what
/// keeps a slow reader from growing this process: once the buffer is at
/// its cap, routing refuses with `QueueFull` instead of queueing more.
#[derive(Default)]
struct OutboundBuffer {
    bytes: usize,
    messages: usize,
}

#[derive(Default)]
struct RegistryInner {
    next_id: u64,
    connections: HashMap<u64, ConnectionHandle>,
    /// Interrupt a displaced or revoked peer immediately, including a
    /// blocked write. It must not retain a hidden thread and buffer.
    sockets: HashMap<u64, TcpStream>,
    /// host_id -> connection id (the host's one live business session).
    hosts: HashMap<String, u64>,
    /// host_id -> connection id (the host's one live pairing channel).
    host_pairings: HashMap<String, u64>,
    /// device_id -> connection id (the device's one live connection).
    devices: HashMap<String, u64>,
    /// device_id -> ciphertext waiting for that device to be online.
    queues: HashMap<String, DeviceQueue>,
    /// Sum over `queues`: bytes and messages held offline. The aggregate
    /// is what the process-wide caps bind, so a hundred offline devices
    /// cannot hold gigabytes with nobody left to reclaim them.
    queued_total_bytes: usize,
    queued_total_messages: usize,
    /// host_id -> device_id of the pairing attempt in flight. A host
    /// pairs one device at a time; the mapping is what routes the host's
    /// pairing reply back before the host knows the device's routing id.
    pairings: HashMap<String, String>,
    /// connection id -> outbound buffer accounting.
    outbound: HashMap<u64, OutboundBuffer>,
    /// Sum over `outbound`: bytes buffered live for peers that have not
    /// read yet. Per-connection caps multiply by the connection cap, so
    /// this total is what keeps the sum inside the memory budget.
    outbound_total_bytes: usize,
    /// Undeliverable-after window for offline ciphertext, set at
    /// construction. The deployment default is `QUEUE_TTL`; an override
    /// exists so an isolated run can prove expiry by waiting, not by
    /// reading code.
    queue_ttl: Duration,
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
        Self::with_queue_ttl(QUEUE_TTL)
    }
}

impl Registry {
    pub fn with_queue_ttl(queue_ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(RegistryInner {
                next_id: 0,
                connections: HashMap::new(),
                sockets: HashMap::new(),
                hosts: HashMap::new(),
                host_pairings: HashMap::new(),
                devices: HashMap::new(),
                queues: HashMap::new(),
                queued_total_bytes: 0,
                queued_total_messages: 0,
                pairings: HashMap::new(),
                outbound: HashMap::new(),
                outbound_total_bytes: 0,
                queue_ttl,
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
    ///
    /// Admission is atomic and global: the cap counts every authenticated
    /// connection — both host slots and devices — and a same-entity
    /// reconnect is decided on its *projected* count, so displacing never
    /// trips the cap. Nothing is mutated before the cap says yes.
    pub fn register(
        &self,
        role: ConnectionRole,
        outbound: Sender<Frame>,
    ) -> Result<(u64, Option<ConnectionHandle>), RegistryFull> {
        let mut inner = self.inner.lock();
        let stale = match &role {
            ConnectionRole::Host { host_id } => inner.hosts.get(host_id).copied(),
            ConnectionRole::HostPairing { host_id } => inner.host_pairings.get(host_id).copied(),
            ConnectionRole::Device { device_id } => inner.devices.get(device_id).copied(),
        };
        let projected = inner
            .connections
            .len()
            .saturating_sub(usize::from(stale.is_some()));
        if projected >= MAX_AUTHENTICATED_CONNECTIONS {
            return Err(RegistryFull);
        }
        let id = inner.next_id;
        inner.next_id += 1;
        let subject = role.subject_id().to_owned();
        let displaced = match &role {
            ConnectionRole::Host { host_id } => inner
                .hosts
                .insert(host_id.clone(), id)
                .and_then(|old| inner.connections.remove(&old)),
            // The pairing slot displaces only itself: a fresh pairing
            // channel never touches the host's business session.
            ConnectionRole::HostPairing { host_id } => {
                let displaced = inner
                    .host_pairings
                    .insert(host_id.clone(), id)
                    .and_then(|old| inner.connections.remove(&old));
                if displaced.is_some() {
                    // A fresh pairing channel is a fresh pairing attempt.
                    // A mapping left behind by the displaced channel would
                    // route the next handshake to a device that is no
                    // longer there, so it must not survive the replacement.
                    inner.pairings.remove(host_id);
                }
                displaced
            }
            ConnectionRole::Device { device_id } => inner
                .devices
                .insert(device_id.clone(), id)
                .and_then(|old| inner.connections.remove(&old)),
        };
        if let Some(old) = displaced.as_ref() {
            if let Some(socket) = inner.sockets.remove(&old.id) {
                let _ = socket.shutdown(Shutdown::Both);
            }
            if let Some(buffer) = inner.outbound.remove(&old.id) {
                inner.outbound_total_bytes =
                    inner.outbound_total_bytes.saturating_sub(buffer.bytes);
            }
        }
        inner
            .connections
            .insert(id, ConnectionHandle { id, role, outbound });
        log::info!(target: "audit", "connection registered; subject={subject} id={id}");
        Ok((id, displaced))
    }

    /// Remove a connection if it is still the registered one (a displaced
    /// connection's late exit must not unregister its replacement).
    pub fn unregister(&self, id: u64) {
        let mut inner = self.inner.lock();
        inner.sockets.remove(&id);
        if let Some(buffer) = inner.outbound.remove(&id) {
            inner.outbound_total_bytes = inner.outbound_total_bytes.saturating_sub(buffer.bytes);
        }
        if let Some(handle) = inner.connections.remove(&id) {
            match &handle.role {
                ConnectionRole::Host { host_id } => {
                    if inner.hosts.get(host_id) == Some(&id) {
                        inner.hosts.remove(host_id);
                    }
                }
                ConnectionRole::HostPairing { host_id } => {
                    if inner.host_pairings.get(host_id) == Some(&id) {
                        inner.host_pairings.remove(host_id);
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
            // device restarts it with the same or a fresh invite. The
            // host's business session exiting does not: with the pairing
            // channel in its own slot, the two lifecycles are independent.
            match &handle.role {
                ConnectionRole::HostPairing { host_id } => {
                    inner.pairings.remove(host_id);
                }
                ConnectionRole::Device { device_id } => {
                    inner.pairings.retain(|_, peer| peer != device_id);
                }
                ConnectionRole::Host { .. } => {}
            }
        }
    }

    /// Record that `device_id` opened a pairing handshake toward `host_id`.
    /// The host's pairing reply routes back through this mapping — before
    /// the handshake completes, the host cannot know the device's id.
    /// One invite routes to exactly one device: a second, different
    /// device is refused rather than silently replacing the first.
    pub fn note_pairing(&self, host_id: &str, device_id: &str) -> PairingNote {
        let mut inner = self.inner.lock();
        match inner.pairings.get(host_id) {
            Some(current) if current == device_id => PairingNote::Retried,
            Some(_) => PairingNote::Conflict,
            None => {
                inner
                    .pairings
                    .insert(host_id.to_owned(), device_id.to_owned());
                PairingNote::Started
            }
        }
    }

    /// The device currently pairing with this host, if any.
    pub fn pairing_peer(&self, host_id: &str) -> Option<String> {
        self.inner.lock().pairings.get(host_id).cloned()
    }

    /// Explicitly end the pairing session (handshake finished either way).
    pub fn clear_pairing(&self, host_id: &str) {
        self.inner.lock().pairings.remove(host_id);
    }

    /// Total live authenticated connections, across both host slots and
    /// devices. An aggregate gauge for the health answer; it names no
    /// subject.
    pub fn connection_count(&self) -> usize {
        self.inner.lock().connections.len()
    }

    /// The sender channel of a live connection, if any.
    pub fn sender(&self, role: &ConnectionRole) -> Option<Sender<Frame>> {
        let inner = self.inner.lock();
        let id = match role {
            ConnectionRole::Host { host_id } => inner.hosts.get(host_id),
            ConnectionRole::HostPairing { host_id } => inner.host_pairings.get(host_id),
            ConnectionRole::Device { device_id } => inner.devices.get(device_id),
        }?;
        Some(inner.connections.get(id)?.outbound.clone())
    }

    pub fn bind_socket(&self, id: u64, socket: TcpStream) {
        let mut inner = self.inner.lock();
        if inner.connections.contains_key(&id) {
            inner.sockets.insert(id, socket);
        } else {
            // Replaced before registration finished attaching its handle.
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    /// Route one frame to a device: deliver live, or queue while offline.
    /// The frame's own encoded size sizes both the live buffer and the
    /// offline queue, so neither can be talked past by a caller.
    pub fn route_to_device(
        &self,
        device_id: &str,
        host_id: &str,
        seq: u64,
        frame: Frame,
    ) -> RouteOutcome {
        let frame_bytes = encoded_len_hint(&frame);
        let mut inner = self.inner.lock();
        if let Some(id) = inner.devices.get(device_id).copied() {
            return deliver(&mut inner, id, frame, frame_bytes);
        }
        // Offline: bounded queue. Expired frames leave first (they are
        // already unreachable for the device's recovery window); after
        // that a full queue is a refusal, never a silent eviction — the
        // sender must know its frame will not be delivered. The
        // process-wide caps are checked in the same critical section, so
        // a refusal leaves every counter exactly as it was.
        let now = now_ms();
        // Totals are staged beside the lock and written back once: the
        // queue itself is borrowed from the same map.
        let ttl_ms = inner.queue_ttl.as_millis() as u64;
        let mut total_bytes = inner.queued_total_bytes;
        let mut total_messages = inner.queued_total_messages;
        let known_device = inner.queues.contains_key(device_id);
        let queue_count = inner.queues.len();
        let fits = {
            let queue = inner.queues.entry(device_id.to_owned()).or_default();
            drop_expired(queue, &mut total_bytes, &mut total_messages, now, ttl_ms);
            let fits_device = frame_bytes <= QUEUE_MAX_BYTES
                && queue.bytes + frame_bytes <= QUEUE_MAX_BYTES
                && queue.frames.len() + 1 <= QUEUE_MAX_MESSAGES;
            // A new device queue may not start once the device budget is
            // spent; an existing one may still grow within the aggregate.
            let fits_process = (known_device || queue_count < QUEUE_TOTAL_MAX_DEVICES)
                && total_bytes + frame_bytes <= QUEUE_TOTAL_MAX_BYTES
                && total_messages + 1 <= QUEUE_TOTAL_MAX_MESSAGES;
            if !fits_device || !fits_process {
                // Nothing was added; an empty placeholder must not linger
                // as a phantom queue that spends the device-count budget.
                if queue.frames.is_empty() {
                    inner.queues.remove(device_id);
                }
                false
            } else {
                queue.bytes += frame_bytes;
                queue.frames.push_back(QueuedFrame {
                    seq,
                    host_id: host_id.to_owned(),
                    frame,
                    enqueued_at_ms: now,
                });
                total_bytes += frame_bytes;
                total_messages += 1;
                true
            }
        };
        inner.queued_total_bytes = total_bytes;
        inner.queued_total_messages = total_messages;
        if fits {
            RouteOutcome::Queued
        } else {
            RouteOutcome::QueueFull
        }
    }

    /// Take everything queued for a device, in order. Called right after
    /// the device authenticates, before any live traffic flows. The
    /// aggregate accounting is released here: those bytes leave the
    /// process's offline budget the moment the device owns them again.
    pub fn drain_queue(&self, device_id: &str) -> Vec<QueuedFrame> {
        let mut inner = self.inner.lock();
        let ttl_ms = inner.queue_ttl.as_millis() as u64;
        match inner.queues.remove(device_id) {
            Some(mut queue) => {
                // Expiry is rechecked on reconnect even between sweeper ticks.
                let original_bytes = queue.bytes;
                let original_messages = queue.frames.len();
                let mut removed_bytes = 0;
                let mut removed_messages = 0;
                drop_expired(
                    &mut queue,
                    &mut removed_bytes,
                    &mut removed_messages,
                    now_ms(),
                    ttl_ms,
                );
                inner.queued_total_bytes = inner.queued_total_bytes.saturating_sub(original_bytes);
                inner.queued_total_messages = inner
                    .queued_total_messages
                    .saturating_sub(original_messages);
                queue.frames.into_iter().collect()
            }
            None => Vec::new(),
        }
    }

    /// Drop everything queued for a device, on purpose. Called when the
    /// pairing is undone or a credential is revoked: the device is no
    /// longer a delivery target, so its ciphertext has nowhere to go and
    /// must not sit in the process until restart.
    pub fn drop_queue(&self, device_id: &str) -> bool {
        let mut inner = self.inner.lock();
        match inner.queues.remove(device_id) {
            Some(queue) => {
                inner.queued_total_bytes = inner.queued_total_bytes.saturating_sub(queue.bytes);
                inner.queued_total_messages = inner
                    .queued_total_messages
                    .saturating_sub(queue.frames.len());
                log::info!(target: "audit", "device queue dropped; device={device_id} frames={}", queue.frames.len());
                true
            }
            None => false,
        }
    }

    /// A device can be paired to multiple hosts. Revoke only this link's
    /// queued ciphertext; the other host's accepted frames stay deliverable.
    pub fn drop_link_queue(&self, device_id: &str, host_id: &str) {
        let mut inner = self.inner.lock();
        let Some(mut queue) = inner.queues.remove(device_id) else {
            return;
        };
        let before_bytes = queue.bytes;
        let before_messages = queue.frames.len();
        queue.frames.retain(|frame| frame.host_id != host_id);
        queue.bytes = queue.frames.iter().map(QueuedFrame::frame_bytes).sum();
        inner.queued_total_bytes = inner
            .queued_total_bytes
            .saturating_sub(before_bytes - queue.bytes);
        inner.queued_total_messages = inner
            .queued_total_messages
            .saturating_sub(before_messages - queue.frames.len());
        if !queue.frames.is_empty() {
            inner.queues.insert(device_id.to_owned(), queue);
        }
    }

    /// Periodic reclaim of undeliverable ciphertext. TTL reclaim used to
    /// run only when the same device sent another frame or reconnected,
    /// so an abandoned queue stayed resident for the life of the process;
    /// the sweeper makes expiry a property of time, not of traffic.
    pub fn sweep_queues(&self) -> usize {
        let mut inner = self.inner.lock();
        let now = now_ms();
        let ttl_ms = inner.queue_ttl.as_millis() as u64;
        let mut total_bytes = inner.queued_total_bytes;
        let mut total_messages = inner.queued_total_messages;
        let mut dropped_frames = 0usize;
        let mut empty: Vec<String> = Vec::new();
        for (device_id, queue) in inner.queues.iter_mut() {
            let before = queue.frames.len();
            drop_expired(queue, &mut total_bytes, &mut total_messages, now, ttl_ms);
            dropped_frames += before - queue.frames.len();
            if queue.frames.is_empty() {
                empty.push(device_id.clone());
            }
        }
        for device_id in empty {
            inner.queues.remove(&device_id);
        }
        inner.queued_total_bytes = total_bytes;
        inner.queued_total_messages = total_messages;
        dropped_frames
    }

    /// Offline depth as (bytes, messages) plus the number of devices
    /// holding a queue. Aggregate gauges for the health answer.
    pub fn queue_totals(&self) -> (usize, usize, usize) {
        let inner = self.inner.lock();
        (
            inner.queued_total_bytes,
            inner.queued_total_messages,
            inner.queues.len(),
        )
    }

    pub fn outbound_total_bytes(&self) -> usize {
        self.inner.lock().outbound_total_bytes
    }

    /// Route one frame to a host. Hosts are never queued: a device's
    /// commands carry their own TTL and the host replays authoritative
    /// state on reconnect, so buffering device-to-host frames here would
    /// fake durability the host does not know about.
    pub fn route_to_host(&self, host_id: &str, frame: Frame) -> RouteOutcome {
        let frame_bytes = encoded_len_hint(&frame);
        let mut inner = self.inner.lock();
        let Some(id) = inner.hosts.get(host_id).copied() else {
            return RouteOutcome::NoRoute;
        };
        deliver(&mut inner, id, frame, frame_bytes)
    }

    /// Route one frame to a host's pairing channel — the slot a pairing
    /// handshake and its reply ride. Separate from `route_to_host` so a
    /// pairing can proceed (or fail) without ever touching the business
    /// session.
    pub fn route_to_host_pairing(&self, host_id: &str, frame: Frame) -> RouteOutcome {
        let frame_bytes = encoded_len_hint(&frame);
        let mut inner = self.inner.lock();
        let Some(id) = inner.host_pairings.get(host_id).copied() else {
            return RouteOutcome::NoRoute;
        };
        deliver(&mut inner, id, frame, frame_bytes)
    }

    /// Close a subject's live connection (credential revoked mid-flight).
    /// Returns whether one was closed.
    pub fn disconnect(&self, role: &ConnectionRole) -> bool {
        let subject = role.subject_id().to_owned();
        let mut inner = self.inner.lock();
        let id = match role {
            ConnectionRole::Host { host_id } => inner.hosts.remove(host_id),
            ConnectionRole::HostPairing { host_id } => inner.host_pairings.remove(host_id),
            ConnectionRole::Device { device_id } => inner.devices.remove(device_id),
        };
        let Some(id) = id else {
            return false;
        };
        if let Some(handle) = inner.connections.remove(&id) {
            if let Some(socket) = inner.sockets.remove(&id) {
                let _ = socket.shutdown(Shutdown::Both);
            }
            if let Some(buffer) = inner.outbound.remove(&id) {
                inner.outbound_total_bytes =
                    inner.outbound_total_bytes.saturating_sub(buffer.bytes);
            }
            let _ = handle.outbound.send(Frame::Control {
                frame: fintwind_remote_protocol::ControlFrame::Rejected {
                    code: fintwind_remote_protocol::RejectCode::AuthFailed,
                    message: "credential revoked".to_owned(),
                    // No frame is being refused here — the whole
                    // connection is — so there is nothing to reference.
                    frame: None,
                },
            });
            log::info!(target: "audit", "connection force-closed; subject={subject} id={id}");
            true
        } else {
            false
        }
    }

    /// Release one delivered frame's outbound accounting once the
    /// session loop has actually written it to the socket. Without this
    /// the buffer would only ever fill, and a momentarily slow peer
    /// would stay refused long after it caught up.
    pub fn release_outbound(&self, id: u64, bytes: usize) {
        let mut inner = self.inner.lock();
        if let Some(buffer) = inner.outbound.get_mut(&id) {
            buffer.bytes = buffer.bytes.saturating_sub(bytes);
            buffer.messages = buffer.messages.saturating_sub(1);
            inner.outbound_total_bytes = inner.outbound_total_bytes.saturating_sub(bytes);
        }
    }
}

/// Deliver into one live connection's bounded outbound buffer. A full
/// buffer is a refusal, never silent growth: the sender's own bounded
/// outbound buffer retransmits unacked frames, which is the honest
/// signal for a peer that stopped reading.
fn deliver(inner: &mut RegistryInner, id: u64, frame: Frame, frame_bytes: usize) -> RouteOutcome {
    let Some(sender) = inner
        .connections
        .get(&id)
        .map(|handle| handle.outbound.clone())
    else {
        return RouteOutcome::NoRoute;
    };
    let buffer = inner.outbound.entry(id).or_default();
    // Both bounds in one check, and nothing changes unless the frame fits
    // both: the per-connection cap stops one peer from spending the whole
    // budget, the process-wide cap keeps the sum inside the memory budget.
    if buffer.messages >= OUTBOUND_MAX_MESSAGES
        || buffer.bytes + frame_bytes > OUTBOUND_MAX_BYTES
        || inner.outbound_total_bytes + frame_bytes > OUTBOUND_TOTAL_MAX_BYTES
    {
        return RouteOutcome::QueueFull;
    }
    if sender.send(frame).is_err() {
        return RouteOutcome::NoRoute;
    }
    buffer.messages += 1;
    buffer.bytes += frame_bytes;
    inner.outbound_total_bytes += frame_bytes;
    RouteOutcome::Delivered
}

/// Drop frames whose recovery window has passed, releasing both the
/// queue's own accounting and the process-wide totals.
fn drop_expired(
    queue: &mut DeviceQueue,
    total_bytes: &mut usize,
    total_messages: &mut usize,
    now: u64,
    ttl_ms: u64,
) {
    while queue
        .frames
        .front()
        .is_some_and(|oldest| now.saturating_sub(oldest.enqueued_at_ms) > ttl_ms)
    {
        if let Some(oldest) = queue.frames.pop_front() {
            queue.bytes = queue.bytes.saturating_sub(oldest.frame_bytes());
            *total_bytes = total_bytes.saturating_sub(oldest.frame_bytes());
            *total_messages = total_messages.saturating_sub(1);
        }
    }
}

/// Approximate encoded size of one wire frame. JSON encoding cost is
/// close to linear in the payload and the caps are order-of-magnitude
/// guards; counting the base64 expansion keeps the accounting honest
/// about what actually crosses the socket.
pub(crate) fn encoded_len_hint(frame: &Frame) -> usize {
    match frame {
        Frame::Sealed { ciphertext, .. } => ciphertext.0.len() * 4 / 3 + 160,
        Frame::Handshake { payload, .. } => payload.0.len() * 4 / 3 + 160,
        Frame::LinkDevice {
            device_public_key, ..
        } => device_public_key.0.len() * 4 / 3 + 224,
        Frame::UnlinkDevice { .. } => 224,
        Frame::Control { .. } | Frame::Auth { .. } => 160,
    }
}

/// Queue accounting needs the encoded size of a queued frame, stored
/// beside it.
impl QueuedFrame {
    fn frame_bytes(&self) -> usize {
        encoded_len_hint(&self.frame)
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
            registry.route_to_device("d", "h", 1, frame_a),
            RouteOutcome::Queued
        );
        assert_eq!(
            registry.route_to_device("d", "h", 2, frame_b),
            RouteOutcome::Queued
        );

        let (tx, rx) = unbounded();
        let (_, displaced) = registry.register(device_role("d"), tx).expect("admitted");
        assert!(displaced.is_none());
        let drained = registry.drain_queue("d");
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].seq, 1, "queue preserves delivery order");
        assert_eq!(drained[1].seq, 2);
        assert!(
            rx.try_recv().is_err(),
            "queued frames are not double-delivered"
        );
    }

    #[test]
    fn queue_full_is_a_refusal_not_truncation() {
        let registry = Registry::default();
        // Fill the queue to its byte cap with one huge frame: the next
        // frame cannot fit and must be refused outright, never silently
        // dropping the queued one.
        let big = QUEUE_MAX_BYTES - 160;
        let (frame, _) = sealed(1, big);
        assert_eq!(
            registry.route_to_device("d", "h", 1, frame),
            RouteOutcome::Queued
        );
        let (frame, _) = sealed(2, big);
        assert_eq!(
            registry.route_to_device("d", "h", 2, frame),
            RouteOutcome::QueueFull
        );
        // The queue still holds exactly the first frame.
        registry
            .register(device_role("d"), unbounded().0)
            .expect("admitted");
        assert_eq!(registry.drain_queue("d").len(), 1);
    }

    #[test]
    fn same_entity_reconnect_displaces_the_old_connection() {
        let registry = Registry::default();
        let (tx_old, rx_old) = unbounded();
        let (old_id, _) = registry.register(host_role("h"), tx_old).expect("admitted");

        let (tx_new, rx_new) = unbounded();
        let (new_id, displaced) = registry.register(host_role("h"), tx_new).expect("admitted");
        assert_eq!(displaced.map(|handle| handle.id), Some(old_id));

        // The stale write half still drains (the channel is closed from
        // the registry side only when the handle is dropped by its owning
        // loop), but routing reaches only the new connection.
        let (frame, _) = sealed(1, 8);
        assert_eq!(registry.route_to_host("h", frame), RouteOutcome::Delivered);
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
        registry.register(device_role("d"), tx).expect("admitted");
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

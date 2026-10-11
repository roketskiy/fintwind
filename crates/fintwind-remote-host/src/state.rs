//! Shared state for the host daemon: identity, the local daemon client,
//! the dedup store, the approval registry, the bounded outbound queue, the
//! per-device channels' bookkeeping, and the session views that decide who
//! receives what.
//!
//! Three addressing facts live here and everything else follows from them:
//!
//! - **A session view belongs to the device that opened it.** Several
//!   devices may hold the same session open at once; each has its own
//!   runtime resolution, its own event delivery, and its own offers.
//! - **Every device channel is independent.** Online state, delivery
//!   sequences and unacked buffers live per device (see `devices`), so one
//!   device's disconnect, bad frame or revocation never touches another's.
//! - **Host-wide pushes go to every online device.** The session list and
//!   workspace notices are addressed to all of them, not to "the" device.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use uuid::Uuid;

use fintwind_client::DaemonClient;
use fintwind_protocol::SequencedEvent;
use fintwind_remote_crypto::IdentityKey;
use fintwind_remote_protocol::{
    AgentPresetEntry, MessageBody, ModelOptionEntry, OperationRequest, PermissionDecision,
    ResumeCursor, SealedMessage, UserInputDecision,
};

use crate::adapter::RuntimeCatalog;
use crate::approvals::ApprovalRegistry;
use crate::config::HostConfig;
use crate::dedup::OperationStore;
use crate::grants::GrantFile;
use crate::identity::HostIdentity;
use crate::pending::{PendingKind, PendingMessage, PendingOutbound};
use crate::queue::PromptQueues;

/// How many sequenced events one session retains for cursor resume. Past
/// this window a resume is answered with a resync, never with a gap.
pub const EVENT_WINDOW: usize = 4096;

/// How long a probed options catalog stays trusted before the host asks
/// the provider again.
pub const CATALOG_TTL: Duration = Duration::from_secs(5 * 60);

/// Work the relay session hands to the single worker thread. Daemon RPCs
/// can take seconds; the session's read loop must never wait on them. The
/// device id rides along because the connection that delivered a message
/// is the only proof of who sent it — re-reading "the current device"
/// later would let a reconnect re-attribute another device's operation.
pub enum SessionCommand {
    Operation {
        device_id: Uuid,
        operation: OperationRequest,
    },
    PermissionDecision {
        device_id: Uuid,
        decision: PermissionDecision,
    },
    UserInputDecision {
        device_id: Uuid,
        decision: UserInputDecision,
    },
    Snapshot {
        device_id: Uuid,
    },
    QueryResult {
        device_id: Uuid,
        operation_id: Uuid,
    },
    /// Device resumes event streams from cursors it holds.
    Resume {
        device_id: Uuid,
        cursors: Vec<ResumeCursor>,
    },
}

/// One session view a device holds open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionView {
    pub runtime_id: Uuid,
    pub device_id: Uuid,
}

/// The device whose handshake completed on the live relay connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveDevice {
    pub device_id: Uuid,
    pub grant_version: u64,
}

/// One session's bounded window of sequenced events, for cursor resume.
#[derive(Default)]
pub struct EventWindow {
    epoch: Option<Uuid>,
    events: VecDeque<SequencedEvent>,
}

/// Why a resume cursor could or could not be honored.
pub enum ResumeOutcome {
    /// Events after the cursor, in sequence order.
    Replay(Vec<SequencedEvent>),
    /// The cursor cannot be honored: a different runtime instance, or a
    /// sequence that already fell out of the window.
    Stale(String),
}

impl EventWindow {
    fn record(&mut self, event: SequencedEvent) {
        self.epoch = Some(event.epoch);
        self.events.push_back(event);
        while self.events.len() > EVENT_WINDOW {
            self.events.pop_front();
        }
    }

    /// Events after `cursor`, or the reason the cursor is stale. A gap is
    /// never bridged by guessing: if the oldest retained event is newer
    /// than the cursor's successor, the caller is told to resync.
    fn resume(&self, cursor: &ResumeCursor) -> ResumeOutcome {
        let Some(epoch) = self.epoch else {
            return ResumeOutcome::Stale("this session has no retained events yet".to_owned());
        };
        if epoch != cursor.epoch {
            return ResumeOutcome::Stale(
                "the session's runtime instance changed; a fresh snapshot is required".to_owned(),
            );
        }
        let Some(oldest) = self.events.front() else {
            return ResumeOutcome::Stale("this session has no retained events yet".to_owned());
        };
        if oldest.sequence > cursor.sequence.saturating_add(1) {
            return ResumeOutcome::Stale(
                "the requested position is outside the retained window".to_owned(),
            );
        }
        let replay: Vec<SequencedEvent> = self
            .events
            .iter()
            .filter(|event| event.sequence > cursor.sequence)
            .cloned()
            .collect();
        ResumeOutcome::Replay(replay)
    }
}

/// The relay link's last known state, mirrored to `host-status.json` for
/// the desktop's supervisor. Written by a background thread, never by a
/// frame path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkStatus {
    pub relay_connected: bool,
    pub updated_at_ms: u64,
}

/// A cancellation this host answered `Accepted` whose ending is still
/// undelivered.
///
/// The device was told its cancellation was recorded; that promise does not
/// die with a queue item, a host restart, or the prompt's row. Each entry
/// stays here until the daemon's durable record supplies a deterministic
/// ending — reached by query, never by re-sending the cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct OrphanedCancel {
    pub prompt_operation_id: Uuid,
    pub cancel_operation_id: Uuid,
    /// The device that raised the cancellation, when the record names one.
    /// Entries recovered from the daemon's rows carry it; entries recovered
    /// from the host's own dedup store carry it too, so the restarted host
    /// can deliver the ending without the device replaying anything.
    pub device_id: Option<Uuid>,
}

pub struct HostState {
    config: HostConfig,
    identity: HostIdentity,
    approvals: ApprovalRegistry,
    dedup: OperationStore,
    catalog: Mutex<RuntimeCatalog>,
    daemon: Mutex<Option<Arc<DaemonClient>>>,
    /// (session, device) -> runtime. Each entry owns one daemon
    /// subscription and one event pump per (session, runtime).
    views: Mutex<HashMap<(Uuid, Uuid), SessionView>>,
    /// Devices with a live channel on the relay connection.
    online: Mutex<HashMap<Uuid, ActiveDevice>>,
    /// Session busy state, learned from the driver's own turn events.
    busy: Mutex<HashMap<Uuid, bool>>,
    /// Per-session bounded event windows for cursor resume.
    windows: Mutex<HashMap<Uuid, EventWindow>>,
    /// Per-session ordering locks: a resume replay must complete before
    /// the live pump forwards the next event for that session.
    send_locks: Mutex<HashMap<Uuid, Arc<Mutex<()>>>>,
    /// Per-session prompt queues.
    queues: Mutex<PromptQueues>,
    /// Per-project probed options catalog, with the time it was fetched.
    options: Mutex<HashMap<Uuid, (Instant, Vec<ModelOptionEntry>, Vec<AgentPresetEntry>)>>,
    /// Host-originated traffic waiting for a channel (or for the device to
    /// come back). Bounded; see `pending`.
    pending: Mutex<PendingOutbound>,
    /// The last session list pushed per device, so an unchanged workspace
    /// does not re-send hundreds of rows on every task-state tick.
    last_session_list: Mutex<HashMap<Uuid, String>>,
    /// Devices whose frames a relay refused. Their ciphertext was dropped,
    /// not replayed, so they are asked to resynchronize from a snapshot and
    /// a cursor instead of bridging a gap they cannot see.
    desync: Mutex<HashSet<Uuid>>,
    /// Cancellations answered `Accepted` whose prompt is no longer in the
    /// dispatch queue — it already ran, or its row is going away. Kept apart
    /// from the queue because there is nothing left to dispatch, only an
    /// ending to fetch and deliver.
    orphaned_cancels: Mutex<HashSet<OrphanedCancel>>,
    /// The relay link state mirrored to disk for the desktop supervisor.
    link_status: Mutex<LinkStatus>,
    shutdown: AtomicBool,
}

impl HostState {
    pub fn new(
        config: HostConfig,
        identity: HostIdentity,
        _grants_loaded: usize,
        dedup: OperationStore,
    ) -> Self {
        Self {
            config,
            identity,
            approvals: ApprovalRegistry::new(),
            dedup,
            catalog: Mutex::new(RuntimeCatalog::default()),
            daemon: Mutex::new(None),
            views: Mutex::new(HashMap::new()),
            online: Mutex::new(HashMap::new()),
            busy: Mutex::new(HashMap::new()),
            windows: Mutex::new(HashMap::new()),
            send_locks: Mutex::new(HashMap::new()),
            queues: Mutex::new(PromptQueues::default()),
            options: Mutex::new(HashMap::new()),
            pending: Mutex::new(PendingOutbound::new()),
            last_session_list: Mutex::new(HashMap::new()),
            desync: Mutex::new(HashSet::new()),
            orphaned_cancels: Mutex::new(HashSet::new()),
            link_status: Mutex::new(LinkStatus {
                relay_connected: false,
                updated_at_ms: now_ms(),
            }),
            shutdown: AtomicBool::new(false),
        }
    }

    /// The grant file as of *now*. Grants are read fresh on every use —
    /// pairing grants, and revocations especially, must take effect while
    /// the daemon runs, not after its next restart. The reads sit on the
    /// once-per-session handshake path and on decision handling, never on
    /// a per-frame path.
    fn load_grants(&self) -> anyhow::Result<Option<GrantFile>> {
        GrantFile::load(&self.config.paired_device_file)
    }

    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    pub fn host_id(&self) -> &str {
        &self.identity.host_id
    }

    pub fn identity(&self) -> &IdentityKey {
        &self.identity.noise
    }

    pub fn signing_key(&self) -> &fintwind_remote_crypto::SigningKey {
        &self.identity.signing
    }

    /// The paired device's Noise static public key, for the KK responder.
    /// No grant, no handshake — a device the operator never confirmed is
    /// invisible to this host even if the relay routes its frames here.
    pub fn grant_public_key(&self, device_id: Uuid) -> anyhow::Result<Vec<u8>> {
        // A missing grant file is the pre-pairing state, not a panic: the
        // daemon starts before any device is trusted, and a handshake that
        // arrives then must be declined, never crash the process.
        let Some(file) = self.load_grants()? else {
            anyhow::bail!("no grant file yet");
        };
        file.device_public_key_bytes(&device_id.to_string())
    }

    /// The sole active grant's device id, for the operator tooling that
    /// predates multi-device addressing (`--revoke-device`, provisioning).
    /// The serving paths never use it: they carry the device id from the
    /// authenticated connection instead.
    pub fn grants_for_device(&self) -> Option<String> {
        self.load_grants()
            .ok()
            .flatten()?
            .grants
            .iter()
            .find(|grant| grant.revoked_at_ms.is_none())
            .map(|grant| grant.device_id.clone())
    }

    pub fn grant_version(&self, device_id: Uuid) -> u64 {
        self.load_grants()
            .ok()
            .flatten()
            .and_then(|file| {
                file.active_grant(&device_id.to_string())
                    .map(|grant| grant.grant_version)
            })
            .unwrap_or(0)
    }

    /// Cancel one prompt on the daemon, naming the device whose prompt it is.
    ///
    /// The daemon releases a prompt only to the device that asked for it:
    /// the provenance is this host's own identity plus the grant version it
    /// holds for that device, so a revoked or re-paired device cannot drive
    /// a prompt it no longer owns. Every path that gives up on a queued
    /// prompt — a refusal, an expiry, a revocation, a failed dispatch —
    /// comes through here, so the daemon's books and this host's never
    /// disagree about what is still waiting.
    ///
    /// Two callers, two sources, one rule. A **device-initiated** cancel
    /// quotes the grant version the device holds *now*: after a re-pairing
    /// that no longer matches the row, and the daemon refuses — correctly,
    /// because the prompt was admitted under a grant the device no longer
    /// holds. A **host cleanup** — an expiry, a revocation, a session going
    /// away — quotes the version the row itself carries, so releasing a row
    /// this computer durably holds is never blocked by the device's newer
    /// grant. The daemon's rule is the same for both: an exact match on
    /// device, host and version. What differs is who is asking and which
    /// version that asker truthfully holds.
    pub fn cancel_remote_prompt(
        &self,
        device_id: Uuid,
        operation_id: Uuid,
        cancel_operation_id: Option<Uuid>,
    ) {
        let Some(daemon) = self.daemon() else {
            return;
        };
        // The device's own request: its live grant version.
        let Ok(provenance) = crate::adapter::device_provenance(self, device_id) else {
            return;
        };
        let _ = daemon.request(
            Uuid::nil(),
            Uuid::nil(),
            fintwind_client::Command::CancelRemotePrompt {
                provenance,
                operation_id,
                // The device's own request: its cancel operation, so the
                // daemon can bind the promise to it and answer it later if
                // the prompt turns out to be mid-dispatch.
                cancel_operation_id,
            },
        );
    }

    /// Cancel one prompt on the daemon, quoting the grant version **the row
    /// itself carries**.
    ///
    /// This is the host's own cleanup path — an expiry, a revocation, a
    /// session going away. The daemon requires an exact version match, so
    /// quoting the device's *current* grant would strand every row of a
    /// revoked or re-paired device on this computer's books: the newer
    /// grant could no longer release the older rows. The row's own version
    /// is the truthful one for a cleanup — it is what the daemon durably
    /// recorded when it admitted the prompt — so releasing it needs no
    /// device request and never pretends to be one.
    ///
    /// This is the host acting on its own durable record, which is what
    /// makes it different from a device request: a device has no way to name
    /// a version the row does not carry, and this path only ever releases
    /// rows, never dispatches them.
    pub fn cancel_remote_prompt_as_recorded(&self, device_id: Uuid, operation_id: Uuid) {
        let Some(daemon) = self.daemon() else {
            return;
        };
        let Ok(host_id) = self.host_id().parse::<Uuid>() else {
            return;
        };
        let Some(grant_version) = self.recorded_grant_version(operation_id) else {
            return;
        };
        let provenance = fintwind_protocol::submission::RemoteProvenance {
            host_id,
            device_id,
            grant_version,
        };
        let _ = daemon.request(
            Uuid::nil(),
            Uuid::nil(),
            fintwind_client::Command::CancelRemotePrompt {
                provenance,
                operation_id,
                // The host is releasing this row on its own initiative: no
                // device operation awaits an outcome, so nothing is bound to
                // it and nothing will be answered later.
                cancel_operation_id: None,
            },
        );
    }

    /// Ask the daemon what became of one prompt's cancel.
    ///
    /// This is how the host resolves a cancel it already answered `Accepted`
    /// for, after a crash or an unanswered request, **without** re-sending
    /// the cancellation: re-sending would repeat a side effect whose outcome
    /// is already unknown, which this system never does. The daemon reports
    /// the cancel it durably recorded.
    ///
    /// Naming the cancel operation asks about *that* cancellation: one
    /// prompt can carry more than one over its life, and a query without the
    /// id can only report whichever ended last. The resolution paths always
    /// name theirs, so an old cancellation is never answered with another's
    /// fate.
    pub fn query_remote_cancel(
        &self,
        device_id: Uuid,
        operation_id: Uuid,
        cancel_operation_id: Option<Uuid>,
    ) -> Option<(Option<Uuid>, String)> {
        let daemon = self.daemon()?;
        let Ok(provenance) = crate::adapter::device_provenance(self, device_id) else {
            return None;
        };
        match daemon.request(
            Uuid::nil(),
            Uuid::nil(),
            fintwind_client::Command::QueryRemoteCancel {
                provenance,
                operation_id,
                cancel_operation_id,
            },
        ) {
            Ok(fintwind_client::ResponsePayload::RemoteCancelState {
                cancel_operation_id,
                cancel_state,
                ..
            }) => Some((cancel_operation_id, cancel_state)),
            _ => None,
        }
    }

    /// The device one queued prompt belongs to, if this host still holds it.
    pub fn queue_device_for(&self, operation_id: Uuid) -> Option<Uuid> {
        self.queues().lock().device_for(operation_id)
    }

    /// The grant version this host durably recorded for one prompt, from its
    /// own queue record. `None` when the host holds no such item.
    fn recorded_grant_version(&self, operation_id: Uuid) -> Option<u64> {
        self.queues()
            .lock()
            .recorded_grant_version(operation_id)
    }

    pub fn approvals(&self) -> &ApprovalRegistry {
        &self.approvals
    }

    pub fn dedup(&self) -> &OperationStore {
        &self.dedup
    }

    pub fn catalog(&self) -> &Mutex<RuntimeCatalog> {
        &self.catalog
    }

    /// The live daemon client, if the local connection is up. Absent means
    /// "host degraded": the relay session stays connected and answers
    /// honestly that the workspace is unreachable.
    pub fn daemon(&self) -> Option<Arc<DaemonClient>> {
        self.daemon.lock().clone()
    }

    pub fn set_daemon(&self, client: Option<Arc<DaemonClient>>) {
        *self.daemon.lock() = client;
    }

    // ---- devices ----

    pub fn note_device_online(&self, device_id: Uuid, grant_version: u64) {
        self.online
            .lock()
            .insert(device_id, ActiveDevice { device_id, grant_version });
    }

    pub fn note_device_offline(&self, device_id: Uuid) {
        self.online.lock().remove(&device_id);
    }

    /// Every device with a live channel. Host-wide pushes address all of
    /// them; per-session traffic still goes only to that session's owners.
    pub fn online_devices(&self) -> Vec<ActiveDevice> {
        self.online.lock().values().cloned().collect()
    }

    pub fn is_device_online(&self, device_id: Uuid) -> bool {
        self.online.lock().contains_key(&device_id)
    }

    // ---- session views ----

    /// Mark a session view as opened by `device_id`. Returns `true` when
    /// this is a fresh view (the caller then spawns the event pump);
    /// re-attaching an already-streaming view is a no-op, so a device
    /// retry can never duplicate the stream.
    pub fn attach_view(&self, session_id: Uuid, device_id: Uuid, runtime_id: Uuid) -> bool {
        self.views
            .lock()
            .insert((session_id, device_id), SessionView { runtime_id, device_id })
            .is_none()
    }

    /// The device closed one session view. Returns the view so the caller
    /// can unsubscribe when the last owner leaves.
    pub fn detach_view(&self, session_id: Uuid, device_id: Uuid) -> Option<SessionView> {
        self.views.lock().remove(&(session_id, device_id))
    }

    /// Every open view, for re-subscription after the local daemon link is
    /// re-established.
    pub fn views(&self) -> Vec<(Uuid, SessionView)> {
        self.views
            .lock()
            .iter()
            .map(|((session_id, _), view)| (*session_id, *view))
            .collect()
    }

    /// Every device holding a session open.
    pub fn session_owners(&self, session_id: Uuid) -> Vec<Uuid> {
        self.views
            .lock()
            .iter()
            .filter(|((session, _), _)| *session == session_id)
            .map(|((_, device), _)| *device)
            .collect()
    }

    /// The runtime a device resolved for a session, if it holds the view.
    pub fn session_runtime_for(&self, session_id: Uuid, device_id: Uuid) -> Option<Uuid> {
        self.views
            .lock()
            .get(&(session_id, device_id))
            .map(|view| view.runtime_id)
    }

    /// A daemon restart invalidates every runtime id and every open offer:
    /// the requests they belonged to died with the old instance, and none
    /// can be confirmed to still exist. Returns the views that were open
    /// so the caller can re-open them against the fresh instance.
    pub fn invalidate_runtime_state(&self, reason: &str) -> Vec<(Uuid, SessionView)> {
        let views: Vec<(Uuid, SessionView)> = self
            .views
            .lock()
            .drain()
            .map(|((session_id, _), view)| (session_id, view))
            .collect();
        self.catalog.lock().clear();
        self.approvals.invalidate_all(reason);
        self.windows.lock().clear();
        views
    }

    // ---- busy state ----

    pub fn set_session_busy(&self, session_id: Uuid, busy: bool) {
        self.busy.lock().insert(session_id, busy);
    }

    pub fn is_session_busy(&self, session_id: Uuid) -> bool {
        self.busy.lock().get(&session_id).copied().unwrap_or(false)
    }

    /// Whether this computer's own record says the session is archived.
    /// Read from the trusted task state, never from the device.
    pub fn is_session_archived(&self, session_id: Uuid) -> bool {
        let Some(daemon) = self.daemon() else {
            return false;
        };
        match daemon.request(
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
            fintwind_client::Command::LoadTaskState,
        ) {
            Ok(fintwind_client::ResponsePayload::TaskState { sessions, .. }) => sessions
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| session.archived),
            _ => false,
        }
    }

    // ---- event window and resume ----

    pub fn record_event(&self, session_id: Uuid, event: &SequencedEvent) {
        self.windows
            .lock()
            .entry(session_id)
            .or_default()
            .record(event.clone());
    }

    pub fn resume_cursor(&self, cursor: &ResumeCursor) -> ResumeOutcome {
        match self.windows.lock().get(&cursor.session_id) {
            Some(window) => window.resume(cursor),
            None => ResumeOutcome::Stale("this session has no retained events yet".to_owned()),
        }
    }

    /// Serialize a session's resume replay against its live forwarding, so
    /// a device never sees a replayed event after a newer live one.
    pub fn session_lock(&self, session_id: Uuid) -> Arc<Mutex<()>> {
        let mut locks = self.send_locks.lock();
        Arc::clone(locks.entry(session_id).or_insert_with(|| Arc::new(Mutex::new(()))))
    }

    // ---- prompt queues ----

    pub fn queues(&self) -> &Mutex<PromptQueues> {
        &self.queues
    }

    // ---- options catalog ----

    pub fn cached_options(
        &self,
        project_id: Uuid,
    ) -> Option<(Vec<ModelOptionEntry>, Vec<AgentPresetEntry>)> {
        let mut options = self.options.lock();
        let (fetched_at, models, presets) = options.get(&project_id)?;
        if fetched_at.elapsed() > CATALOG_TTL {
            options.remove(&project_id);
            return None;
        }
        Some((models.clone(), presets.clone()))
    }

    pub fn cache_options(
        &self,
        project_id: Uuid,
        models: Vec<ModelOptionEntry>,
        presets: Vec<AgentPresetEntry>,
    ) {
        self.options
            .lock()
            .insert(project_id, (Instant::now(), models, presets));
    }

    // ---- outbound traffic ----

    /// Queue one host-originated message for its device. Returns whether
    /// the message was kept; a dropped event is re-derivable and a dropped
    /// result is recoverable through `QueryOperationResult`.
    pub fn push_outbound(&self, message: SealedMessage, kind: PendingKind) -> bool {
        match self.pending.lock().push(message, kind) {
            crate::pending::PendingVerdict::Dropped => false,
            _ => true,
        }
    }

    /// Seal and queue one body for one device. Returns false when the body
    /// could not be sealed (a corrupt stored identity) or was dropped.
    pub fn send_to_device(&self, body: MessageBody, device_id: Uuid, kind: PendingKind) -> bool {
        let Some(message) = self.seal_envelope(body, device_id) else {
            return false;
        };
        self.push_outbound(message, kind)
    }

    /// Worker results waiting to be sealed and sent to the device. Results
    /// are also persisted in the dedup store, so the queue is a delivery
    /// optimization, never the only copy.
    pub fn push_completed(&self, message: SealedMessage) {
        self.push_outbound(message, PendingKind::Result);
    }

    /// Take up to `max` queued messages, results first. The relay session
    /// loop is the only consumer.
    pub fn take_pending(&self, max: usize) -> Vec<PendingMessage> {
        self.pending.lock().take_batch(max)
    }

    /// Put unsent messages back at the front after a failed dispatch.
    pub fn requeue_pending_front(&self, messages: Vec<PendingMessage>) {
        self.pending.lock().requeue_front(messages);
    }

    /// Put messages whose device has no live channel back at the tail, so
    /// an offline device's backlog never blocks another device's traffic.
    pub fn requeue_pending_back(&self, messages: Vec<PendingMessage>) {
        self.pending.lock().requeue_back(messages);
    }

    /// The last session-list payload pushed to one device, for change
    /// detection.
    pub fn last_session_list(&self, device_id: Uuid) -> Option<String> {
        self.last_session_list.lock().get(&device_id).cloned()
    }

    pub fn set_last_session_list(&self, device_id: Uuid, payload: Option<String>) {
        match payload {
            Some(payload) => {
                self.last_session_list.lock().insert(device_id, payload);
            }
            None => {
                self.last_session_list.lock().remove(&device_id);
            }
        }
    }

    // ---- relay-refused frames ----

    /// The queue as this computer durably holds it: one record per prompt
    /// still on the books, so a host that restarts rebuilds its view instead
    /// of trusting its own memory. Bound by the global ceiling before
    /// anything is re-admitted.
    pub fn recover_queues(&self, daemon: &DaemonClient) {
        // The list is the host's own read of what this computer durably
        // holds: no device is asking, so the provenance names the host and
        // no device, and every prompt it holds comes back for routing to
        // the device that sent it.
        let Ok(host_id) = self.host_id().parse::<Uuid>() else {
            return;
        };
        let provenance = fintwind_protocol::submission::RemoteProvenance {
            host_id,
            device_id: Uuid::nil(),
            grant_version: 0,
        };
        let Ok(fintwind_client::ResponsePayload::RemotePrompts { prompts }) =
            daemon.request(uuid::Uuid::nil(), uuid::Uuid::nil(), fintwind_client::Command::ListRemotePrompts { provenance })
        else {
            return;
        };
        let total = prompts.len();
        // Two passes over the same list, deliberately.
        //
        // The first restores what may still be dispatched. The second is for
        // cancellations that are waiting on a prompt which is **not** in that
        // set — a prompt that already reached the driver, or one whose row is
        // about to go away. Their cancel operations were answered `Accepted`
        // and must not be left without an ending just because the prompt is
        // no longer dispatchable, so they are recorded for resolution
        // regardless of the prompt's own state.
        let mut orphaned_cancels: Vec<OrphanedCancel> = Vec::new();
        for prompt in &prompts {
            if let Some(cancel_operation_id) = prompt.cancel_operation_id {
                orphaned_cancels.push(OrphanedCancel {
                    prompt_operation_id: prompt.operation_id,
                    cancel_operation_id,
                    device_id: Some(prompt.device_id),
                });
            }
        }
        for prompt in prompts {
            if !matches!(prompt.state.as_str(), "queued" | "dispatching") {
                // Already settled: nothing to restore, and re-admitting it
                // would risk running a prompt that already ran.
                continue;
            }
            let item = crate::queue::QueuedPrompt {
                operation_id: prompt.operation_id,
                device_id: prompt.device_id,
                session_id: prompt.session_id,
                prompt: String::new(),
                summary: prompt.summary.clone(),
                enqueued_at_ms: prompt.enqueued_at_ms,
                expires_at_ms: prompt.expires_at_ms,
                // The daemon's own record of the grant this prompt was
                // admitted under, restored with it: the cleanup paths quote
                // it, so a re-paired device's newer grant cannot strand it.
                grant_version: prompt.grant_version,
                state: crate::queue::QueueItemState::Queued,
                // The daemon's own durable cancel binding, restored with the
                // prompt: a cancellation promised before a crash is still
                // promised, and the prompt must not be dispatched while it
                // waits.
                cancel_requested: prompt.cancel_operation_id,
            };
            if self.queues().lock().total_pending() >= crate::queue::MAX_TOTAL_QUEUE_ITEMS {
                log::warn!(
                    target: "audit",
                    "queue recovery hit the global ceiling; {} prompt(s) left with the daemon",
                    total
                );
                return;
            }
            let position = self.queues().lock().restore(item);
            log::info!(
                target: "audit",
                "queue recovered; session={} operation={} submission={} position={:?}",
                prompt.session_id,
                prompt.operation_id,
                prompt.submission_id,
                position
            );
        }
        // Cancellations whose prompt is no longer dispatchable are recorded
        // for resolution: they were answered `Accepted`, and their ending
        // still has to reach the device that asked.
        self.record_orphaned_cancels(orphaned_cancels);
    }

    /// Remember cancellations whose prompt is not coming back into the queue.
    ///
    /// Kept apart from the queue because there is nothing to dispatch: the
    /// prompt already ran, or its row is going away. What remains is to ask
    /// the daemon what it recorded and answer the device.
    pub fn record_orphaned_cancels(&self, cancels: Vec<OrphanedCancel>) {
        self.orphaned_cancels.lock().extend(cancels);
    }

    /// The undelivered cancellations, without taking them.
    ///
    /// Removing an entry is only correct once a deterministic ending has
    /// been delivered; the resolution path peeks here and removes exactly
    /// the entries it finished, so a failed or refused query leaves the
    /// entry in place for the next tick instead of losing it.
    pub fn orphaned_cancels(&self) -> Vec<OrphanedCancel> {
        self.orphaned_cancels.lock().iter().copied().collect()
    }

    /// Remove exactly the entries whose ending was delivered.
    pub fn remove_orphaned_cancels(&self, delivered: &[OrphanedCancel]) {
        let mut guard = self.orphaned_cancels.lock();
        for cancel in delivered {
            guard.remove(cancel);
        }
    }


    /// Mark one device as needing a resynchronize: a relay refused a frame
    /// addressed to it, so that frame's ciphertext was dropped rather than
    /// replayed. The device is told to rebuild from a snapshot and a cursor.
    pub fn note_device_desync(&self, device_id: Uuid) {
        self.desync.lock().insert(device_id);
    }

    /// Take the devices waiting for a resynchronize, clearing the set. The
    /// session loop is the only consumer.
    pub fn take_desync_devices(&self) -> Vec<Uuid> {
        self.desync.lock().drain().collect()
    }

    // ---- link status ----

    pub fn link_status(&self) -> LinkStatus {
        *self.link_status.lock()
    }

    /// Record the relay link state. The disk mirror is written by the
    /// status thread, never from here.
    pub fn set_link_status(&self, relay_connected: bool) {
        let mut status = self.link_status.lock();
        if status.relay_connected != relay_connected {
            *status = LinkStatus {
                relay_connected,
                updated_at_ms: now_ms(),
            };
        }
    }

    pub fn touch_link_status(&self) {
        self.link_status.lock().updated_at_ms = now_ms();
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    pub fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// Build the envelope for one host-to-device message. A stored identity
    /// that is not a uuid is corruption, not something to route around:
    /// refuse to seal rather than address the default uuid.
    ///
    /// `transport_seq` is stamped `0` here on purpose: the per-connection
    /// delivery sequence is only known when the relay session loop puts the
    /// message on the wire, and that loop overwrites this field with the
    /// frame's real seq immediately before encrypting. A business-channel
    /// message never leaves with `0`; if one ever did, the peer's
    /// inner/outer seq check would refuse it, which is the correct
    /// fail-closed outcome. (The pairing channel is the one place `0` is
    /// the agreed value, and that path does not run through here.)
    pub fn seal_envelope(&self, body: MessageBody, device_id: Uuid) -> Option<SealedMessage> {
        let host_id = match self.identity.host_id.parse::<Uuid>() {
            Ok(host_id) => host_id,
            Err(error) => {
                log::warn!(
                    target: "audit",
                    "stored host id {host_id:?} is not a uuid ({error}); refusing to seal",
                    host_id = self.identity.host_id
                );
                return None;
            }
        };
        Some(SealedMessage {
            protocol_version: fintwind_remote_protocol::REMOTE_PROTOCOL_VERSION,
            // Stamped by the session loop with the frame's delivery seq.
            transport_seq: 0,
            host_id,
            device_id,
            sent_at_ms: now_ms(),
            body,
        })
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Where identity and dedup files live.
pub fn identity_paths(data_dir: &PathBuf) -> (PathBuf, PathBuf, PathBuf) {
    (
        data_dir.join("host-signing-key.bin"),
        data_dir.join("host-noise-key.bin"),
        data_dir.join("host-id.txt"),
    )
}

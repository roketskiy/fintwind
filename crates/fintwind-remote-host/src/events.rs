//! The host→device event stream: one pump thread per (session, runtime)
//! view, plus the transcript snapshot fetch, the session-list snapshot,
//! the offer factory that turns driver permission / question requests into
//! device-answerable offers, and the cursor-resume replay.
//!
//! The daemon's own `SequencedEvent`s are forwarded verbatim inside
//! `DriverEvent` frames — no re-encoding — minus the kinds the device has
//! no use for, and with `turnTranscriptReconciled` traded for a fresh
//! `Transcript` snapshot so the device recalibrates on the authoritative
//! window instead of replaying a projection on its own.
//!
//! Permission and question requests do **not** ride that raw channel: they
//! become `PermissionOffer` / `UserInputOffer` messages, one per device
//! holding the session open, each bound to one runtime instance, one grant
//! version and one TTL, and all of them claiming the same underlying
//! driver request so exactly one decision can ever reach the provider.

use std::sync::Arc;
use std::time::Duration;

use fintwind_client::{Command, DaemonClient, ResponsePayload};
use fintwind_protocol::provider_session::NativeTranscript;
use fintwind_protocol::{
    model::{ActivityItem, Message as NativeMessage, MessageRole},
    SequencedEvent,
};
use fintwind_remote_protocol::{
    MessageBody, ProjectSummary, ResumeCursor, SessionSummary, TranscriptActivity,
    TranscriptMessage, TranscriptRole, TranscriptTurn,
};
use serde_json::Value;
use uuid::Uuid;

use crate::approvals::OfferQuestion;
use crate::grants::action_digest;
use crate::pending::PendingKind;
use crate::state::{HostState, ResumeOutcome};

/// Kinds the device's session view consumes. Everything else — catalog
/// churn, client-only cursors, large reconciled windows — stays local.
/// `permission` and `userInputRequested` are absent on purpose: they
/// travel as bound offers instead (see the module docs).
const FORWARDED_KINDS: &[&str] = &[
    "textStarted",
    "textDelta",
    "textEnded",
    "reasoningStarted",
    "reasoningDelta",
    "reasoningEnded",
    "activity",
    "turnStarted",
    "turnFinished",
    "submissionUpdated",
    "usageUpdated",
    "providerBusy",
    "error",
];

/// Kinds that end the exchange a pending request belonged to. When one of
/// these arrives, every open offer for that session is void: the request
/// it displayed is no longer the request the driver would act on. The
/// local GUI may still answer whatever is genuinely pending; the remote
/// side simply never acts on a request it can no longer confirm.
const INVALIDATING_KINDS: &[&str] = &[
    "turnStarted",
    "turnFinished",
    "steerAccepted",
    "processExited",
    "nativeSessionMoved",
];

/// The driver's "always" option. It updates a session-local approval rule,
/// which is exactly what a remote decision must never do: it answers one
/// request or nothing.
const ALWAYS_OPTION_ID: &str = "always";

/// What the device is told when the local workspace cannot answer.
pub const WORKSPACE_UNAVAILABLE: &str = "The desktop workspace isn't running.";

/// Spawn the pump for one (session, runtime) view. The thread ends when the
/// subscription closes (daemon link lost or unsubscribed) or the host
/// shuts down; the daemon-link thread re-attaches views after reconnect.
pub fn spawn_session_pump(state: Arc<HostState>, session_id: Uuid, runtime_id: Uuid) {
    std::thread::Builder::new()
        .name("fintwind-remote-events".into())
        .spawn(move || run_session_pump(state, session_id, runtime_id))
        .ok();
}

fn run_session_pump(state: Arc<HostState>, session_id: Uuid, runtime_id: Uuid) {
    let Some(daemon) = state.daemon() else {
        return;
    };
    let events = daemon.subscribe(session_id, runtime_id);
    // The epoch of the runtime instance this subscription was opened
    // against. A different epoch on a later event means the instance was
    // replaced, and every offer minted under the old one is void.
    let mut known_epoch: Option<Uuid> = None;
    loop {
        if state.shutdown_requested() {
            return;
        }
        // The receiver closes when the daemon client is dropped (link
        // lost) or the session is unsubscribed (device closed the view).
        match events.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => {
                if let Some(previous) = known_epoch {
                    if previous != event.epoch {
                        state.approvals().invalidate_session(
                            session_id,
                            "the session's runtime instance changed",
                        );
                    }
                }
                known_epoch = Some(event.epoch);
                handle_driver_event(&state, &daemon, &event);
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn handle_driver_event(state: &Arc<HostState>, daemon: &Arc<DaemonClient>, event: &SequencedEvent) {
    // Every event joins the session's bounded window first: a device that
    // reconnects with a cursor replays from here, and a gap is answered
    // with a resync rather than bridged by guessing.
    state.record_event(event.session_id, event);

    let kind = event.event.kind.as_str();
    if kind == "turnTranscriptReconciled" {
        // The settled window is authoritative; send a fresh snapshot
        // rather than making the device reconcile windows itself.
        fetch_and_send_transcript(state, daemon, event.session_id);
        return;
    }
    // Busy state is the queue's dispatch signal, learned from the driver's
    // own turn lifecycle rather than polled.
    match kind {
        "turnStarted" => state.set_session_busy(event.session_id, true),
        "turnFinished" | "processExited" => state.set_session_busy(event.session_id, false),
        _ => {}
    }
    match kind {
        "permission" => offer_permission(state, event),
        "userInputRequested" => offer_user_input(state, event),
        _ => {
            if INVALIDATING_KINDS.contains(&kind) {
                state
                    .approvals()
                    .invalidate_session(event.session_id, "the turn that raised the request ended");
            }
            if FORWARDED_KINDS.contains(&kind) {
                forward_event(state, event);
            }
        }
    }
}

/// Forward one event to every device holding the session open. The
/// per-session ordering lock keeps a resume replay from interleaving with
/// live traffic.
fn forward_event(state: &Arc<HostState>, event: &SequencedEvent) {
    let owners = state.session_owners(event.session_id);
    if owners.is_empty() {
        return;
    }
    let lock = state.session_lock(event.session_id);
    let _guard = lock.lock();
    for device_id in owners {
        state.send_to_device(
            MessageBody::DriverEvent {
                event: event.clone().into(),
            },
            device_id,
            PendingKind::Event,
        );
    }
}

/// Turn one driver permission request into device-bound offers — one per
/// device holding the session open.
///
/// The displayed action is what the digest binds, so a re-announcement
/// with different content voids the old offers (for every device) instead
/// of silently keeping a decision basis that no longer exists.
fn offer_permission(state: &Arc<HostState>, event: &SequencedEvent) {
    let payload = &event.event.payload;
    let Some(request_ref) = payload.get("requestId").and_then(Value::as_str) else {
        return;
    };
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let detail = payload
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let options: Vec<(String, String)> = payload
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let id = option.get("id").and_then(Value::as_str)?.to_owned();
                    let label = option.get("label").and_then(Value::as_str)?.to_owned();
                    Some((id, label))
                })
                .filter(|(id, _)| id != ALWAYS_OPTION_ID)
                .collect()
        })
        .unwrap_or_default();
    if options.is_empty() {
        // Nothing a device may answer (only an "always"-style rule was
        // offered, or the driver named no options): the local GUI is the
        // only place this request can be answered.
        log::warn!(
            target: "audit",
            "permission request {request_ref} has no remotely answerable option; leaving it local"
        );
        return;
    }
    let option_ids: Vec<&str> = options.iter().map(|(id, _)| id.as_str()).collect();
    let digest = action_digest(&[&title, &detail, &option_ids.join(",")]);

    let owners = state.session_owners(event.session_id);
    if owners.is_empty() {
        return;
    }
    // A changed action voids every device's offer for this request before
    // any new one is minted; an identical re-announcement is a duplicate.
    let mut changed = false;
    for device_id in &owners {
        if let Some(open) =
            state
                .approvals()
                .open_offer_for_request(event.session_id, request_ref, *device_id)
        {
            if open.action_digest() != digest {
                changed = true;
            }
        }
    }
    if changed {
        state.approvals().invalidate_request(
            event.session_id,
            request_ref,
            "the request changed after the offer was issued",
        );
    } else if owners.iter().any(|device_id| {
        state
            .approvals()
            .open_offer_for_request(event.session_id, request_ref, *device_id)
            .is_some()
    }) {
        // The same request announced twice (a replayed journal event); the
        // standing offers already cover it.
        return;
    }

    for device_id in owners {
        let grant_version = state.grant_version(device_id);
        if grant_version == 0 {
            // No live grant for the device that opened this view: it must
            // re-pair before it may answer anything.
            continue;
        }
        let offer = state.approvals().create_permission_offer(
            device_id,
            event.session_id,
            event.runtime_id,
            request_ref.to_owned(),
            title.clone(),
            detail.clone(),
            options.clone(),
            digest.clone(),
            grant_version,
        );
        log::info!(
            target: "audit",
            "permission offer issued; session={} request={} approval={} device={device_id}",
            event.session_id,
            request_ref,
            offer.approval_id()
        );
        state.send_to_device(offer.body(), device_id, PendingKind::Event);
    }
}

/// Turn one driver question set into device-bound offers. Option ids are
/// synthesized by position because the base's own option shape carries
/// labels only; answers are mapped back through the same positions.
fn offer_user_input(state: &Arc<HostState>, event: &SequencedEvent) {
    let payload = &event.event.payload;
    let Some(request_ref) = payload.get("requestId").and_then(Value::as_str) else {
        return;
    };
    let Some(raw_questions) = payload.get("questions") else {
        return;
    };
    let Ok(questions) = serde_json::from_value::<Vec<fintwind_protocol::model::UserInputQuestion>>(
        raw_questions.clone(),
    ) else {
        log::warn!(
            target: "audit",
            "question request {request_ref} could not be decoded; leaving it local"
        );
        return;
    };
    let mut digest_parts: Vec<String> = vec![request_ref.to_owned()];
    let mapped: Vec<OfferQuestion> = questions
        .iter()
        .map(|question| {
            let text = if question.header.trim().is_empty() {
                question.question.clone()
            } else {
                format!("{}\n{}", question.header, question.question)
            };
            digest_parts.push(question.id.clone());
            digest_parts.push(text.clone());
            digest_parts.push(
                question
                    .options
                    .iter()
                    .map(|option| option.label.clone())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            OfferQuestion {
                id: question.id.clone(),
                question: text,
                multi_select: question.multi_select,
                options: question
                    .options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| (format!("opt-{index}"), option.label.clone()))
                    .collect(),
            }
        })
        .collect();
    if mapped.is_empty() {
        return;
    }
    let digest = action_digest(
        &digest_parts
            .iter()
            .map(|part| part.as_str())
            .collect::<Vec<_>>(),
    );

    let owners = state.session_owners(event.session_id);
    if owners.is_empty() {
        return;
    }
    let mut changed = false;
    for device_id in &owners {
        if let Some(open) =
            state
                .approvals()
                .open_offer_for_request(event.session_id, request_ref, *device_id)
        {
            if open.action_digest() != digest {
                changed = true;
            }
        }
    }
    if changed {
        state.approvals().invalidate_request(
            event.session_id,
            request_ref,
            "the questions changed after the offer was issued",
        );
    } else if owners.iter().any(|device_id| {
        state
            .approvals()
            .open_offer_for_request(event.session_id, request_ref, *device_id)
            .is_some()
    }) {
        return;
    }

    for device_id in owners {
        let grant_version = state.grant_version(device_id);
        if grant_version == 0 {
            continue;
        }
        let offer = state.approvals().create_user_input_offer(
            device_id,
            event.session_id,
            event.runtime_id,
            request_ref.to_owned(),
            mapped.clone(),
            digest.clone(),
            grant_version,
        );
        log::info!(
            target: "audit",
            "question offer issued; session={} request={} approval={} device={device_id}",
            event.session_id,
            request_ref,
            offer.approval_id()
        );
        state.send_to_device(offer.body(), device_id, PendingKind::Event);
    }
}

/// Restart with every fresh daemon link: the task-state version changes
/// whenever any session changes, and each change re-pushes the session
/// list snapshot to every online device. The receiver closes when the link
/// drops; the daemon-link thread spawns a new pump on reconnect.
pub fn spawn_task_state_pump(state: Arc<HostState>, daemon: Arc<DaemonClient>) {
    std::thread::Builder::new()
        .name("fintwind-remote-taskstate".into())
        .spawn(move || {
            let versions = daemon.subscribe_task_state();
            loop {
                if state.shutdown_requested() {
                    return;
                }
                match versions.recv_timeout(Duration::from_secs(1)) {
                    Ok(_version) => {
                        for device in state.online_devices() {
                            push_session_list(&state, device.device_id, false);
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .ok();
}

/// Push the authoritative session list to one device (shared by the
/// device's snapshot request and the task-state pump). An unchanged list
/// is not re-sent unless the caller forces it — a snapshot request forces,
/// a task-state tick does not.
pub fn push_session_list(state: &Arc<HostState>, device_id: Uuid, force: bool) {
    let Some(daemon) = state.daemon() else {
        // Degraded workspace: say so instead of leaving the device waiting
        // on a list that will never arrive.
        state.send_to_device(
            MessageBody::WorkspaceUnavailable {
                reason: WORKSPACE_UNAVAILABLE.to_owned(),
            },
            device_id,
            PendingKind::Event,
        );
        return;
    };
    let Ok(ResponsePayload::TaskState { projects, sessions, .. }) =
        daemon.request(Uuid::nil(), Uuid::nil(), Command::LoadTaskState)
    else {
        state.send_to_device(
            MessageBody::WorkspaceUnavailable {
                reason: WORKSPACE_UNAVAILABLE.to_owned(),
            },
            device_id,
            PendingKind::Event,
        );
        return;
    };
    let summaries: Vec<SessionSummary> = sessions
        .iter()
        .map(|session| SessionSummary {
            session_id: session.id,
            project_id: Some(session.project_id),
            // The project's own name, so a device never renders a UUID.
            project_name: projects
                .iter()
                .find(|project| project.id == session.project_id)
                .map(|project| project.name.clone()),
            title: session.title.clone(),
            status: serde_json::to_value(&session.status)
                .ok()
                .and_then(|value| value.as_str().map(|text| text.to_owned()))
                .unwrap_or_default(),
            // AgentSession timestamps are seconds; the wire field is ms.
            updated_at_ms: session.updated_at.saturating_mul(1000),
            // Tidy-up state comes from the session's own record, which the
            // daemon owns: only its archive and restore commands change it.
            archived: session.archived,
        })
        .collect();
    let payload = serde_json::to_string(&summaries).unwrap_or_default();
    if !force && state.last_session_list(device_id).as_deref() == Some(payload.as_str()) {
        return;
    }
    state.set_last_session_list(device_id, Some(payload));
    state.send_to_device(
        MessageBody::SessionList {
            revision: now_ms(),
            sessions: summaries,
        },
        device_id,
        PendingKind::Event,
    );
    // The project directory rides along: names only, never paths, so a
    // device can name a target for a new task or a workspace read.
    let directory: Vec<ProjectSummary> = projects
        .iter()
        .map(|project| ProjectSummary {
            project_id: project.id,
            name: project.name.clone(),
        })
        .collect();
    state.send_to_device(
        MessageBody::ProjectList {
            revision: now_ms(),
            projects: directory,
        },
        device_id,
        PendingKind::Event,
    );
}

/// Send every still-valid offer bound to one device. A reconnecting device
/// calls this through its snapshot request, so a turn that paused while
/// the phone was away is answerable the moment it returns — the offers
/// were never tied to the connection that carried them.
pub fn push_pending_offers(state: &Arc<HostState>, device_id: Uuid) {
    for offer in state.approvals().pending_for_device(device_id) {
        state.send_to_device(offer.body(), offer.device_id, PendingKind::Event);
    }
}

/// Answer one device's resume request. A cursor the host can honor is
/// answered with the events after it; anything else — a different runtime
/// instance, or a position outside the retained window — is answered with
/// an explicit resync, never with a bridged gap.
pub fn resume_streams(state: &Arc<HostState>, device_id: Uuid, cursors: &[ResumeCursor]) {
    for cursor in cursors {
        // A device may only resume a session it still holds open.
        if state.session_runtime_for(cursor.session_id, device_id).is_none() {
            state.send_to_device(
                MessageBody::Resync {
                    session_id: Some(cursor.session_id),
                    reason: "this session view is no longer open".to_owned(),
                },
                device_id,
                PendingKind::Event,
            );
            continue;
        }
        let outcome = state.resume_cursor(cursor);
        match outcome {
            ResumeOutcome::Replay(events) => {
                let lock = state.session_lock(cursor.session_id);
                let _guard = lock.lock();
                for event in events {
                    state.send_to_device(
                        MessageBody::DriverEvent {
                            event: event.into(),
                        },
                        device_id,
                        PendingKind::Event,
                    );
                }
            }
            ResumeOutcome::Stale(reason) => {
                state.send_to_device(
                    MessageBody::Resync {
                        session_id: Some(cursor.session_id),
                        reason,
                    },
                    device_id,
                    PendingKind::Event,
                );
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Fetch one session's native transcript through the daemon and push it to
/// every device holding the session open. The workspace coordinates come
/// from the session's own record; a missing coordinate or a failed fetch
/// is logged and dropped — the next reconciled turn retries naturally.
pub fn fetch_and_send_transcript(state: &Arc<HostState>, daemon: &Arc<DaemonClient>, session_id: Uuid) {
    let owners = state.session_owners(session_id);
    if owners.is_empty() {
        return;
    }
    let Some((binary, directory, native_session_id)) =
        crate::adapter::session_coordinates(daemon, session_id)
    else {
        return;
    };
    let transcript = match daemon.request(
        Uuid::nil(),
        Uuid::nil(),
        Command::FetchNativeTranscript {
            binary,
            directory,
            session_id: native_session_id,
        },
    ) {
        Ok(ResponsePayload::NativeTranscript { transcript }) => transcript,
        Ok(_) => return,
        Err(error) => {
            log::warn!(target: "audit", "transcript fetch failed: {error}");
            return;
        }
    };
    // Whether a turn may be branched or rewound to is this computer's own
    // rule, computed from its own session status — a device never guesses
    // eligibility, and the host never offers an action that would be
    // refused here.
    let actionable = session_actionable(daemon, session_id);
    let messages = map_transcript(&transcript, actionable);
    for device_id in owners {
        state.send_to_device(
            MessageBody::Transcript {
                session_id,
                messages: messages.clone(),
            },
            device_id,
            PendingKind::Event,
        );
    }
}

/// Whether this computer would currently accept a fork or a rewind on this
/// session. Read once per snapshot: the desktop refuses both while a turn is
/// running, so a busy session offers neither.
fn session_actionable(daemon: &Arc<DaemonClient>, session_id: Uuid) -> bool {
    match daemon.request(Uuid::nil(), Uuid::nil(), Command::LoadTaskState) {
        Ok(ResponsePayload::TaskState { sessions, .. }) => sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| {
                matches!(
                    session.status,
                    fintwind_protocol::model::SessionStatus::Idle
                        | fintwind_protocol::model::SessionStatus::Failed
                )
            }),
        _ => false,
    }
}

/// Flatten the desktop's transcript model (messages, trailing activity
/// blocks, turns) into the device's linear message list. Blocks attach after
/// `after_message` messages, so each merge point is exact. A block-only row
/// carries `role: system` with empty text: the device renders its trailing
/// activities, and no message is invented.
///
/// Turns are carried, not inferred: every message that belongs to a turn is
/// bound to that turn's own id and count, and to whether this computer would
/// branch or rewind to it. A device therefore never counts messages or roles
/// to work out which turn it is looking at.
fn map_transcript(transcript: &NativeTranscript, actionable: bool) -> Vec<TranscriptMessage> {
    use std::collections::BTreeMap;

    let mut activities_by_position: BTreeMap<usize, Vec<TranscriptActivity>> = BTreeMap::new();
    for block in &transcript.blocks {
        activities_by_position
            .entry(block.after_message)
            .or_default()
            .extend(block.activities.iter().map(map_activity));
    }

    let mut messages = Vec::with_capacity(transcript.messages.len());
    for (index, message) in transcript.messages.iter().enumerate() {
        if let Some(trailing) = activities_by_position.remove(&index) {
            messages.push(TranscriptMessage {
                role: TranscriptRole::System,
                text: String::new(),
                activities: trailing,
                created_at_ms: 0,
                turn: None,
            });
        }
        messages.push(map_message(message, transcript, actionable));
    }
    if let Some(trailing) = activities_by_position.remove(&transcript.messages.len()) {
        messages.push(TranscriptMessage {
            role: TranscriptRole::System,
            text: String::new(),
            activities: trailing,
            created_at_ms: 0,
            turn: None,
        });
    }
    messages
}

fn map_message(
    message: &NativeMessage,
    transcript: &NativeTranscript,
    actionable: bool,
) -> TranscriptMessage {
    // The turn this message belongs to, resolved from the desktop's own
    // association. A message with no turn keeps `None` rather than a guessed
    // position.
    let turn = message.turn_id.and_then(|turn_id| {
        transcript
            .turns
            .iter()
            .find(|turn| turn.id == turn_id)
            .map(|turn| TranscriptTurn {
                turn_id: turn.id,
                turn_count: turn.turn_count,
                // A response may be branched only from a turn that actually
                // reached the provider; a rewind targets the user message
                // that starts a turn.
                can_fork: actionable && turn.provider_turn_started,
                can_rewind: actionable
                    && message.role == MessageRole::User
                    && transcript.messages.iter().any(|candidate| {
                        candidate.turn_id == Some(turn.id) && candidate.role == MessageRole::User
                    }),
            })
    });
    TranscriptMessage {
        role: match message.role {
            MessageRole::User => TranscriptRole::User,
            MessageRole::Assistant => TranscriptRole::Assistant,
            MessageRole::System => TranscriptRole::System,
            MessageRole::Compaction => TranscriptRole::Compaction,
        },
        text: message.content.clone(),
        activities: Vec::new(),
        created_at_ms: message.created_at.saturating_mul(1000),
        turn,
    }
}

fn map_activity(item: &ActivityItem) -> TranscriptActivity {
    TranscriptActivity {
        kind: serde_json::to_value(&item.kind)
            .ok()
            .and_then(|value| value.as_str().map(|text| text.to_owned()))
            .unwrap_or_default(),
        title: item.title.clone(),
        detail: item.detail.clone(),
        failed: item.failed,
        complete: item.complete,
    }
}

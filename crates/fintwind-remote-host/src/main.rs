//! Fintwind remote-control host daemon.
//!
//! One process, four concerns: hold the local identity (signing + Noise
//! keys, auto-registered with the relay on first start — nothing to type
//! anywhere), keep one authenticated loopback connection to the local
//! `fintwind-daemon`, run the outbound relay session that moves frames
//! between every paired device and the workspace, and own the prompt
//! queues that keep an ordinary send from ever becoming a silent steer.
//!
//! Operations are deduplicated durably before any side effect; approvals
//! are consumed atomically — with one authoritative decision per driver
//! request, whichever device sends it — before the driver hears anything;
//! and the device that sent a message is the one its answer is addressed
//! to.
//!
//! First start sequence: generate keys → fetch challenge → register →
//! store the assigned host id. If no paired-device grant file exists the
//! host still connects and waits — the correct pre-pairing posture is
//! invisibility, not interactivity.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use crossbeam_channel::unbounded;
use fintwind_client::DaemonClient;
use fintwind_protocol::Command;
use fintwind_remote_host::{
    adapter, approvals,
    config::HostConfig,
    dedup, events, grants,
    grants::GrantFile,
    identity, pair,
    pending::PendingKind,
    queue, relay_session,
    state::{HostState, OrphanedCancel, SessionCommand},
};
use fintwind_remote_protocol::{
    MessageBody, OperationKind, OperationOutcome, OperationRequest, OperationState,
    PermissionDecision, QueueItemEntry, RejectCode, UserInputDecision,
};
use serde_json::Value;
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    // The relay link is wss in production. The dependency tree carries
    // rustls without a process-level crypto provider, and rustls refuses
    // to guess; install ring once before the first handshake. Setting it
    // twice is harmless, so the tool subcommands take this path too.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut arguments = std::env::args().skip(1);
    let mut provision: Option<String> = None;
    let mut print_identity = false;
    let mut pair = false;
    let mut revoke_device: Option<String> = None;
    let mut parent_pid: Option<u32> = None;
    let mut config_arguments = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            // Operator tooling: write the paired-device grant (phase one is
            // single-device; the file is replaced wholesale) or print this
            // host's identity for the pairing QR. Neither starts the daemon
            // and neither needs the serve-only flags.
            "--provision-device" => {
                provision = Some(arguments.next().ok_or_else(|| {
                    anyhow::anyhow!("--provision-device requires \"deviceId,noisePublicKeyB64\"")
                })?)
            }
            "--print-identity" => print_identity = true,
            "--pair" => pair = true,
            "--parent-pid" => {
                parent_pid = Some(
                    arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--parent-pid requires a process id"))?
                        .parse()
                        .context("--parent-pid is not a valid process id")?,
                )
            }
            "--revoke-device" => {
                revoke_device = Some(
                    arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--revoke-device requires a device id"))?,
                )
            }
            other => config_arguments.push(other.to_owned()),
        }
    }

    // Tooling mode: only the data-dir (and grant path) matter here.
    if print_identity || provision.is_some() {
        let data_dir = take_flag_value(&mut config_arguments, "--data-dir")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        if let Some(provision) = provision {
            let paired_device_file = take_flag_value(&mut config_arguments, "--paired-device")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("paired-device.json"));
            let config = HostConfig {
                relay_origin: String::new(),
                daemon_address: String::new(),
                daemon_token_env: String::new(),
                data_dir,
                paired_device_file,
            };
            provision_device(&config, &provision)?;
            return Ok(());
        }
        let config = HostConfig {
            relay_origin: String::new(),
            daemon_address: String::new(),
            daemon_token_env: String::new(),
            data_dir,
            paired_device_file: PathBuf::from("paired-device.json"),
        };
        let identity = identity::load_or_create(&config.data_dir)?;
        use base64::Engine as _;
        println!(
            "{}",
            serde_json::json!({
                "hostId": identity.host_id,
                "noisePublicKey": base64::engine::general_purpose::STANDARD.encode(&identity.noise.public),
                "signingPublicKey": base64::engine::general_purpose::STANDARD.encode(&identity.signing.verifying),
            })
        );
        return Ok(());
    }

    // Pairing and revocation need the relay origin but not the daemon
    // link: they are short interactive sessions, not the serving loop.
    if pair || revoke_device.is_some() {
        let data_dir = take_flag_value(&mut config_arguments, "--data-dir")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let relay_origin = take_flag_value(&mut config_arguments, "--relay-origin")
            .unwrap_or_else(|| "https://relay.fintwind.xyz".to_owned());
        let paired_device_file = take_flag_value(&mut config_arguments, "--paired-device")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("paired-device.json"));
        let config = HostConfig {
            relay_origin,
            daemon_address: String::new(),
            daemon_token_env: String::new(),
            data_dir,
            paired_device_file,
        };
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
        if pair {
            return pair::run_pair_session(&config);
        }
        let device_id = revoke_device.expect("checked above");
        return pair::run_revoke_device(&config, &device_id);
    }

    let config = HostConfig::parse(config_arguments)?;
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Watchdog: when the desktop app goes away, so does the relay link's
    // owner. Same contract as the daemon's --parent-pid.
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(parent_pid) = parent_pid {
        let monitor_shutdown = Arc::clone(&shutdown);
        std::thread::Builder::new()
            .name("fintwind-remote-host-parent".into())
            .spawn(move || {
                while !monitor_shutdown.load(std::sync::atomic::Ordering::Acquire) {
                    if !process_is_alive(parent_pid) {
                        monitor_shutdown.store(true, std::sync::atomic::Ordering::Release);
                        log::info!(target: "audit", "parent process gone; shutting down");
                        std::process::exit(0);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            })?;
    }

    run_daemon(config)
}

/// Unix: signal 0 probes existence without side effects; EPERM means the
/// process exists but belongs to someone else.
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Windows reuses process ids, so the handle is opened for the narrowest
/// right that answers the question and closed immediately. A pid that no
/// longer exists fails to open; one that has exited but is still held open by
/// another handle reports an exit code instead of `STILL_ACTIVE`.
#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut exit_code = 0_u32;
        let read = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        // A failed read leaves the parent's state unknown; outliving the app
        // is the safer error than shutting a live daemon down.
        read == 0 || exit_code == STILL_ACTIVE as u32
    }
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

fn take_flag_value(arguments: &mut Vec<String>, name: &str) -> Option<String> {
    let position = arguments.iter().position(|argument| argument == name)?;
    arguments.get(position + 1).cloned()
}

/// Replace the grant file with exactly one active device grant. Revoking
/// in phase one is `--revoke-device`'s job later; wholesale replacement
/// keeps the single-device story honest — no partial trust states.
fn provision_device(config: &HostConfig, spec: &str) -> anyhow::Result<()> {
    let mut parts = spec.split(',');
    let device_id = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing device id"))?
        .trim()
        .to_owned();
    let device_public_key = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing device noise public key"))?
        .trim()
        .to_owned();
    if parts.next().is_some() {
        anyhow::bail!("unexpected extra fields");
    }
    uuid::Uuid::parse_str(&device_id)
        .map_err(|error| anyhow::anyhow!("device id is not a uuid: {error}"))?;
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(&device_public_key)
        .map_err(|error| anyhow::anyhow!("device public key is not base64: {error}"))?;
    anyhow::ensure!(raw.len() == 32, "device public key must be 32 bytes");
    let grant = grants::DeviceGrant {
        device_id: device_id.clone(),
        // The operator tooling has no device-supplied name; the grant is
        // keyed by the public key, and the name is only a human label the
        // pairing flow fills in.
        device_name: None,
        device_public_key,
        grant_version: 1,
        granted_at_ms: now_ms(),
        revoked_at_ms: None,
    };
    let file = grants::GrantFile {
        grants: vec![grant],
    };
    std::fs::write(
        &config.paired_device_file,
        serde_json::to_string_pretty(&file)?,
    )?;
    log::info!(target: "audit", "device provisioned; device={device_id}");
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Map the daemon's prompt row state to the operation result for the original
/// prompt. A prompt that was cancelled, expired, or never sent is not an
/// executed prompt; only `dispatched` (and its in-flight equivalent) proves
/// that it reached the driver.
fn prompt_result_for_cancel_state(state: &str) -> OperationState {
    match state {
        "dispatched" | "dispatching" => OperationState::Executed,
        "expired" => OperationState::Expired,
        "cancelled" | "notSent" => OperationState::Rejected {
            code: RejectCode::Malformed,
        },
        _ => OperationState::Unknown,
    }
}

/// Map the daemon's prompt row state to the cancel operation's result.
fn cancel_result_for_prompt_state(state: &str) -> (OperationState, bool) {
    match state {
        "cancelled" | "expired" | "notSent" => (OperationState::Executed, true),
        "dispatched" | "dispatching" => (
            OperationState::Rejected {
                code: RejectCode::Malformed,
            },
            false,
        ),
        _ => (OperationState::Unknown, false),
    }
}

fn run_daemon(config: HostConfig) -> anyhow::Result<()> {
    // Identity: load or create keys, register on first start.
    let mut identity = identity::load_or_create(&config.data_dir)?;
    if identity.host_id.is_empty() {
        let host_id = identity::register_with_relay(&config.relay_origin, &identity.signing)?;
        identity::store_host_id(&config.data_dir, &host_id)?;
        identity.host_id = host_id;
        log::info!(target: "audit", "host registered; host={}", identity.host_id);
    } else {
        log::info!(target: "audit", "host identity loaded; host={}", identity.host_id);
    }

    // Grants: which devices this operator has confirmed. Absent file =
    // nothing trusted yet; the daemon still runs and waits. The file is
    // re-read on every handshake and every decision, so this is only the
    // startup tally.
    let grants =
        GrantFile::load(&config.paired_device_file)?.unwrap_or(GrantFile { grants: Vec::new() });
    log::info!(target: "audit", "grants loaded; count={}", grants.grants.len());

    let dedup = dedup::OperationStore::open(&config.data_dir.join("operations.sqlite3"))?;
    let (work_tx, work_rx) = unbounded::<SessionCommand>();

    let state = Arc::new(HostState::new(
        config.clone(),
        identity,
        grants.grants.len(),
        dedup,
    ));

    // Local daemon link. A degraded workspace is reported honestly to the
    // device, never guessed around: when the link drops, every runtime id
    // and every open offer dies with it (they belonged to that instance
    // and none can be confirmed to survive), the device is told the
    // workspace is gone, and the views it left open are re-opened against
    // the fresh instance once the link returns.
    {
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("fintwind-daemon-link".into())
            .spawn(move || {
                // Views to re-open after a reconnect: (session, device).
                let mut reattach: Vec<(Uuid, Uuid)> = Vec::new();
                loop {
                    if state.shutdown_requested() {
                        return;
                    }
                    let mut needs_connect = state.daemon().is_none();
                    if !needs_connect {
                        let client = state.daemon().expect("checked above");
                        if client.is_disconnected() {
                            drop(client);
                            state.set_daemon(None);
                            reattach = state
                                .invalidate_runtime_state("the local workspace link was lost")
                                .into_iter()
                                .map(|(session_id, view)| (session_id, view.device_id))
                                .collect();
                            for device in state.online_devices() {
                                state.send_to_device(
                                    MessageBody::WorkspaceUnavailable {
                                        reason: events::WORKSPACE_UNAVAILABLE.to_owned(),
                                    },
                                    device.device_id,
                                    PendingKind::Event,
                                );
                            }
                            log::warn!(target: "audit", "daemon link lost; runtime state invalidated");
                            needs_connect = true;
                        }
                    }
                    if needs_connect {
                        let token = match std::env::var(&state.config().daemon_token_env) {
                            Ok(token) => token,
                            Err(_) => {
                                log::warn!(target: "audit", "daemon token env unset; waiting");
                                std::thread::sleep(Duration::from_secs(5));
                                continue;
                            }
                        };
                        match DaemonClient::connect(&state.config().daemon_address, token) {
                            Ok(client) => {
                                let client = Arc::new(client);
                                log::info!(target: "audit", "daemon connected");
                                // Rebuild the queue view from the daemon's
                                // durable rows: a prompt admitted before the
                                // host went away is still queued there.
                                state.recover_queues(&client);
                                state.set_daemon(Some(Arc::clone(&client)));
                                // Re-arm the cancellations this host accepted
                                // but never finished delivering, straight from
                                // its own durable records. A restarted host
                                // must not wait for the device to replay its
                                // cancellation just to learn what happened:
                                // the acceptance was this host's promise, and
                                // the promise is recovered here by query, not
                                // by re-sending anything.
                                match state.dedup().pending_cancel_targets() {
                                    Ok(pending) => {
                                        state.record_orphaned_cancels(
                                            pending
                                                .into_iter()
                                                .map(|record| OrphanedCancel {
                                                    prompt_operation_id: record.prompt_operation_id,
                                                    cancel_operation_id: record.cancel_operation_id,
                                                    device_id: Some(record.device_id),
                                                    grant_version: record.grant_version,
                                                })
                                                .collect(),
                                        );
                                    }
                                    Err(error) => {
                                        log::warn!(
                                            target: "audit",
                                            "could not recover accepted cancellations: {error}"
                                        );
                                    }
                                }
                                events::spawn_task_state_pump(Arc::clone(&state), Arc::clone(&client));
                                // Re-open the views the devices left open.
                                // The old runtime ids died with the previous
                                // instance, so each session resolves a fresh
                                // one instead of resubscribing to a ghost.
                                for (session_id, device_id) in std::mem::take(&mut reattach) {
                                    let resolved = {
                                        let mut catalog = state.catalog().lock();
                                        adapter::resolve_runtime(&client, &mut catalog, session_id)
                                    };
                                    let Ok(runtime_id) = resolved else {
                                        log::warn!(target: "audit", "session {session_id} no longer attachable after reconnect");
                                        continue;
                                    };
                                    if state.attach_view(session_id, device_id, runtime_id) {
                                        events::spawn_session_pump(
                                            Arc::clone(&state),
                                            session_id,
                                            runtime_id,
                                        );
                                        events::fetch_and_send_transcript(&state, &client, session_id);
                                        push_queue_view(&state, session_id, device_id);
                                    }
                                }
                                for device in state.online_devices() {
                                    events::push_session_list(&state, device.device_id, true);
                                }
                            }
                            Err(error) => {
                                log::warn!(target: "audit", "daemon connect failed: {error}");
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_secs(3));
                }
            })?;
    }

    // The link-status mirror for the desktop's supervisor: written by this
    // background thread, atomically, on change and at least every 20s.
    {
        let state = Arc::clone(&state);
        let path = config.data_dir.join("host-status.json");
        std::thread::Builder::new()
            .name("fintwind-remote-host-status".into())
            .spawn(move || {
                let mut last_write: Option<(bool, std::time::Instant)> = None;
                loop {
                    if state.shutdown_requested() {
                        return;
                    }
                    let status = state.link_status();
                    let changed =
                        last_write.is_none_or(|(connected, _)| connected != status.relay_connected);
                    let stale =
                        last_write.is_none_or(|(_, at)| at.elapsed() >= Duration::from_secs(20));
                    if changed || stale {
                        if write_link_status(&path, status.relay_connected) {
                            last_write = Some((status.relay_connected, std::time::Instant::now()));
                        }
                    }
                    std::thread::sleep(Duration::from_secs(5));
                }
            })?;
    }

    // The single worker: serial operation execution keeps ordering, and
    // daemon RPC latency never stalls the relay session loop. It also owns
    // the queue tick and the approval sweep.
    {
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("fintwind-remote-worker".into())
            .spawn(move || run_worker(state, work_rx))?;
    }

    let links = Arc::new(relay_session::SessionLinks {
        state: Arc::clone(&state),
        work_tx,
    });
    relay_session::run_forever(links);
    Ok(())
}

/// Write the link status atomically: a temp file beside the target, then a
/// rename. A supervisor that reads mid-write sees either the old file or
/// the new one, never a truncated one.
fn write_link_status(path: &std::path::Path, relay_connected: bool) -> bool {
    let payload = serde_json::json!({
        "relayConnected": relay_connected,
        "updatedAtMs": now_ms(),
    });
    let text = match serde_json::to_string_pretty(&payload) {
        Ok(text) => text,
        Err(error) => {
            log::warn!(target: "audit", "link status encode failed: {error}");
            return false;
        }
    };
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    if std::fs::write(&temporary, text).is_err() {
        return false;
    }
    std::fs::rename(&temporary, path).is_ok()
}

fn run_worker(state: Arc<HostState>, work_rx: crossbeam_channel::Receiver<SessionCommand>) {
    loop {
        if state.shutdown_requested() {
            return;
        }
        match work_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(SessionCommand::Operation {
                device_id,
                operation,
            }) => run_operation(&state, device_id, operation),
            Ok(SessionCommand::PermissionDecision {
                device_id,
                decision,
            }) => run_permission_decision(&state, device_id, decision),
            Ok(SessionCommand::UserInputDecision {
                device_id,
                decision,
            }) => run_user_input_decision(&state, device_id, decision),
            Ok(SessionCommand::Snapshot { device_id }) => run_snapshot(&state, device_id),
            Ok(SessionCommand::QueryResult {
                device_id,
                operation_id,
            }) => run_query_result(&state, device_id, operation_id),
            Ok(SessionCommand::Resume { device_id, cursors }) => {
                events::resume_streams(&state, device_id, &cursors)
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                state.approvals().sweep();
                dispatch_ready_queues(&state);
                expire_due_queue_items(&state);
                resolve_orphaned_cancels(&state);
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The runtime id an attach result reports, for the event pump subscription.
fn session_runtime_id(detail: &serde_json::Value) -> Option<Uuid> {
    detail
        .get("runtimeId")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| Uuid::parse_str(text).ok())
}

/// One operation's prompt content hash, or `None` when it carries no
/// prompt.
///
/// A prompt-carrying operation is bound to its text by hash, so a retry
/// that reuses the operation id with a different prompt is refused instead
/// of silently adopting the first one's body. Only the hash is stored: the
/// prompt itself is never persisted by this store, never logged, and never
/// compared against anything but a retry of the same id.
fn prompt_content_hash(operation: &OperationRequest) -> Option<String> {
    let prompt = operation
        .payload
        .get("prompt")
        .and_then(serde_json::Value::as_str)?
        .trim();
    if prompt.is_empty() {
        return None;
    }
    Some(dedup::prompt_hash(prompt))
}

fn run_operation(state: &Arc<HostState>, device_id: Uuid, operation: OperationRequest) {
    if matches!(
        operation.kind,
        OperationKind::ReadFile
            | OperationKind::ReadDirectory
            | OperationKind::ReadDiff
            | OperationKind::ReadTaskOutput
    ) {
        run_read(state, device_id, operation);
        return;
    }
    if let OperationKind::LoadOptionsCatalog = operation.kind {
        run_options_catalog(state, device_id, operation);
        return;
    }

    let summary = operation.summary.clone();
    let operation_id = operation.operation_id.to_string();
    let kind = format!("{:?}", operation.kind);
    let expires_at_ms = operation.expires_at_ms;

    // The admission gate: replayed ids answer from the record, summary-
    // changing retries are refused, expired requests never run.
    let check = match state.dedup().check(
        &operation_id,
        &kind,
        &summary,
        &device_id.to_string(),
        expires_at_ms,
        prompt_content_hash(&operation).as_deref(),
    ) {
        Ok(check) => check,
        Err(error) => {
            log::warn!(target: "audit", "dedup store error: {error}");
            complete(state, device_id, &operation, OperationState::Unknown, None);
            return;
        }
    };
    if check.replay {
        // A cancellation still recorded as `accepted` has an outcome this
        // host promised to deliver, so a replay of it is answered from the
        // daemon's durable record rather than from the host's memory of
        // having taken the request. That is what keeps `Accepted` from
        // quietly becoming a guarantee: if the daemon holds the binding, its
        // state is the truth; if it holds nothing, the answer is unknown,
        // not a cancellation.
        if let OperationKind::CancelQueuedPrompt = operation.kind
            && matches!(check.state, OperationState::Accepted)
        {
            let stored_target = state.dedup().cancel_target(&operation_id);
            let Some(stored_target) = stored_target else {
                // The host may have crashed between accepting C and freezing
                // its target. There is no durable identity to replay safely,
                // so fail closed instead of trusting a new payload target.
                let finished = state
                    .dedup()
                    .record_finished_if_accepted(
                        &operation_id,
                        &OperationState::Unknown,
                        Some("the cancellation target was not durably recorded"),
                    )
                    .unwrap_or(false);
                if finished {
                    complete(
                        state,
                        device_id,
                        &operation,
                        OperationState::Unknown,
                        Some(serde_json::json!({
                            "message": "the cancellation target was not durably recorded",
                        })),
                    );
                }
                return;
            };
            let Some(target) = operation
                .payload
                .get("operationId")
                .and_then(serde_json::Value::as_str)
                .and_then(|text| Uuid::parse_str(text).ok())
            else {
                let finished = state
                    .dedup()
                    .record_finished_if_accepted(
                        &operation_id,
                        &OperationState::Unknown,
                        Some("the cancellation target could not be verified"),
                    )
                    .unwrap_or(false);
                if finished {
                    complete(
                        state,
                        device_id,
                        &operation,
                        OperationState::Unknown,
                        Some(serde_json::json!({
                            "message": "the cancellation target could not be verified",
                        })),
                    );
                }
                return;
            };
            if stored_target != target.to_string() {
                // The operation id is bound to the first prompt target. Do
                // not mutate that durable record or query a different prompt
                // on behalf of a conflicting replay.
                complete(
                    state,
                    device_id,
                    &operation,
                    OperationState::Rejected {
                        code: RejectCode::Malformed,
                    },
                    Some(serde_json::json!({
                        "message": "this cancellation operation is bound to another prompt",
                    })),
                );
                return;
            }
            resolve_replayed_cancel(state, device_id, &operation, target);
            return;
        }
        complete(state, device_id, &operation, check.state, None);
        return;
    }
    if let OperationState::Expired = check.state {
        complete(state, device_id, &operation, OperationState::Expired, None);
        return;
    }

    // The host, not the device, bounds how long an operation may stay
    // retryable. An over-long window is refused *and recorded*, so waiting
    // cannot turn a refused request into an accepted one later.
    if expires_at_ms > now_ms() + dedup::MAX_OPERATION_TTL_MS {
        refuse_operation(
            state,
            device_id,
            &operation,
            RejectCode::Malformed,
            "operation validity exceeds this host's 10 minute limit",
        );
        return;
    }

    // Every ordinary send uses durable admission. Idle and not-yet-started
    // sessions dispatch through the same queue pump as busy sessions; they
    // must not require an existing runtime or bypass operation provenance.
    if let OperationKind::SendPrompt = operation.kind {
        enqueue_prompt(state, device_id, &operation);
        return;
    }

    // Cancellation now passes through the same replay gate as every other
    // operation. A replay of an accepted cancellation must query its durable
    // ending, never attempt to record or execute the side effect again.
    if let OperationKind::CancelQueuedPrompt = operation.kind {
        run_cancel_queued(state, device_id, operation);
        return;
    }

    // Accept durably **before** any side effect.
    if let Err(error) = state.dedup().record_accepted(
        &operation_id,
        &kind,
        &summary,
        &device_id.to_string(),
        operation.session_id.map(|id| id.to_string()).as_deref(),
        expires_at_ms,
        prompt_content_hash(&operation).as_deref(),
    ) {
        log::warn!(target: "audit", "could not persist acceptance: {error}");
        complete(
            state,
            device_id,
            &operation,
            OperationState::Unknown,
            Some(serde_json::json!("acceptance not persisted")),
        );
        return;
    }

    // Detaching never touches the workspace, so it needs no daemon and no
    // admission beyond the dedup record above.
    if let OperationKind::DetachSession = operation.kind {
        if let Some(session_id) = operation.session_id {
            if let Some(view) = state.detach_view(session_id, device_id) {
                // The view is gone, so nothing it displayed may be acted
                // on any more.
                state
                    .approvals()
                    .invalidate_session(session_id, "the device closed the session view");
                if state.session_owners(session_id).is_empty() {
                    if let Some(daemon) = state.daemon().filter(|_| !view.runtime_id.is_nil()) {
                        daemon.unsubscribe(session_id, view.runtime_id);
                    }
                }
            }
        }
        state
            .dedup()
            .record_finished(&operation_id, &OperationState::Executed, None)
            .ok();
        complete(state, device_id, &operation, OperationState::Executed, None);
        return;
    }

    // A deletion invalidates everything queued or offered for the target
    // sessions before anything runs: a deleted session's prompts never
    // execute anywhere.
    if let OperationKind::DeleteSessions = operation.kind {
        let ids: Vec<Uuid> = operation
            .payload
            .get("sessionIds")
            .and_then(serde_json::Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str())
                    .filter_map(|id| Uuid::parse_str(id).ok())
                    .collect()
            })
            .unwrap_or_default();
        for session_id in ids {
            for dropped in state.queues().lock().drop_session(session_id) {
                report_dropped_queue_item(state, &dropped, "the session was deleted");
            }
            state
                .approvals()
                .invalidate_session(session_id, "the session was deleted");
            for owner in state.session_owners(session_id) {
                state.detach_view(session_id, owner);
            }
        }
    }

    let Some(daemon) = state.daemon() else {
        // Acceptance is already durably recorded above; this path only
        // freezes the outcome. Re-recording acceptance here would collide
        // with the existing row and leave the operation stuck in
        // `accepted`, which a replay would then answer as a live
        // acceptance instead of the recorded refusal.
        state
            .dedup()
            .record_finished(
                &operation_id,
                &OperationState::Rejected {
                    code: RejectCode::UnknownRoute,
                },
                Some("workspace unreachable"),
            )
            .ok();
        complete_with_detail(
            state,
            device_id,
            &operation,
            OperationState::Rejected {
                code: RejectCode::UnknownRoute,
            },
            Some(serde_json::json!({ "message": "workspace unreachable" })),
        );
        return;
    };

    let outcome = {
        // One catalog for the whole host: the worker and the session loop
        // must never resolve a session to two different runtime ids.
        let mut catalog = state.catalog().lock();
        adapter::execute(state, &daemon, &mut catalog, device_id, &operation)
    };
    match outcome {
        Ok(detail) => {
            state
                .dedup()
                .record_finished(&operation_id, &OperationState::Executed, None)
                .ok();
            // An attach opens a session view on this device: start
            // streaming that session's events and deliver the transcript
            // snapshot the view renders. A fresh attach starts the pump; a
            // retry by the same device re-sends the transcript.
            if let (OperationKind::AttachSession, Some(session_id)) =
                (&operation.kind, operation.session_id)
            {
                let runtime_id = session_runtime_id(&detail)
                    .or_else(|| state.catalog().lock().runtime_of(session_id));
                if let Some(runtime_id) = runtime_id {
                    let fresh = state.attach_view(session_id, device_id, runtime_id);
                    if fresh {
                        events::spawn_session_pump(Arc::clone(state), session_id, runtime_id);
                    }
                    if fresh || state.session_owners(session_id).contains(&device_id) {
                        events::fetch_and_send_transcript(state, &daemon, session_id);
                    }
                } else {
                    // A persisted task can have an open view before its
                    // first runtime exists. Keep ownership for queue admission
                    // without inventing a runtime or starting a provider.
                    state.attach_view(session_id, device_id, Uuid::nil());
                    events::fetch_and_send_transcript(state, &daemon, session_id);
                }
                push_queue_view(state, session_id, device_id);
                events::push_pending_offers(state, device_id);
            }
            // A batch delete carries its per-item results in `detail`; the
            // overall `Executed` below is only reached when every item
            // succeeded, because a partial batch arrives as a rejection
            // whose detail keeps the per-item truth.
            complete_with_detail(
                state,
                device_id,
                &operation,
                OperationState::Executed,
                Some(detail),
            );
            if matches!(
                operation.kind,
                OperationKind::CreateSession
                    | OperationKind::RenameSession
                    | OperationKind::ArchiveSession
                    | OperationKind::RestoreArchivedSession
                    | OperationKind::DeleteSessions
                    | OperationKind::ForkFromResponse
                    | OperationKind::RewindToMessage
            ) {
                for device in state.online_devices() {
                    events::push_session_list(state, device.device_id, true);
                }
            }
        }
        Err((code, message)) => {
            // A batch's partial failure arrives as a rejection whose detail
            // carries the per-item results; keep that detail intact.
            let detail = serde_json::from_str::<serde_json::Value>(&message)
                .unwrap_or_else(|_| serde_json::json!({ "message": message }));
            let detail = if detail.get("results").is_some() {
                detail
            } else {
                serde_json::json!({ "message": message })
            };
            state
                .dedup()
                .record_finished(
                    &operation_id,
                    &OperationState::Rejected { code },
                    Some(&message),
                )
                .ok();
            complete_with_detail(
                state,
                device_id,
                &operation,
                OperationState::Rejected { code },
                Some(detail),
            );
        }
    }
}

/// Queue one ordinary send against a busy session. The daemon admits it
/// durably — reserving its turn and committing the prompt, the operation id
/// and the submission id in one transaction — so `queued` always means there
/// is a receipt on this computer, not just an entry in host memory.
fn enqueue_prompt(state: &Arc<HostState>, device_id: Uuid, operation: &OperationRequest) {
    let Some(session_id) = operation.session_id else {
        refuse_operation(
            state,
            device_id,
            operation,
            RejectCode::Malformed,
            "session id required",
        );
        return;
    };
    let prompt = operation
        .payload
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(str::to_owned);
    let Some(prompt) = prompt else {
        refuse_operation(
            state,
            device_id,
            operation,
            RejectCode::Malformed,
            "a non-empty prompt is required",
        );
        return;
    };
    if prompt.len() > adapter::MAX_PROMPT_BYTES {
        refuse_operation(
            state,
            device_id,
            operation,
            RejectCode::Malformed,
            "prompt is too long",
        );
        return;
    }
    let Some(daemon) = state.daemon() else {
        refuse_operation(
            state,
            device_id,
            operation,
            RejectCode::UnknownRoute,
            "workspace unreachable",
        );
        return;
    };
    // The global bound is checked before the daemon is asked: an over-long
    // backlog is refused here rather than written and then dropped.
    let queued_total = state.queues().lock().total_pending();
    if queued_total >= queue::MAX_TOTAL_QUEUE_ITEMS {
        refuse_operation(
            state,
            device_id,
            operation,
            queue::refusal_code(&queue::EnqueueRefusal::Full),
            queue::refusal_message(&queue::EnqueueRefusal::Full),
        );
        return;
    }
    let provenance = match adapter::device_provenance(state, device_id) {
        Ok(provenance) => provenance,
        Err((code, message)) => {
            refuse_operation(state, device_id, operation, code, &message);
            return;
        }
    };
    let admitted = daemon.request(
        Uuid::nil(),
        Uuid::nil(),
        fintwind_client::Command::AdmitRemotePrompt {
            provenance,
            session_id,
            operation_id: operation.operation_id,
            prompt: prompt.clone(),
            expires_at_ms: operation.expires_at_ms,
            summary: operation.summary.clone(),
        },
    );
    let submission_id = match admitted {
        Ok(fintwind_client::ResponsePayload::RemotePromptAdmitted { submission_id, .. }) => {
            submission_id
        }
        Ok(fintwind_client::ResponsePayload::RemotePromptState {
            submission_id,
            state: label,
            ..
        }) => {
            // The daemon already holds this operation: this host lost its
            // record, so it adopts what the daemon still holds rather than
            // admitting a second prompt. A row that already settled answers
            // with its recorded state — nothing runs twice, and a prompt
            // whose outcome is unknown is never re-sent.
            match label.as_str() {
                "queued" | "dispatching" => submission_id,
                "dispatched" => {
                    complete(
                        state,
                        device_id,
                        operation,
                        OperationState::Executed,
                        Some(serde_json::json!({ "dispatched": true })),
                    );
                    return;
                }
                _ => {
                    complete(
                        state,
                        device_id,
                        operation,
                        OperationState::Expired,
                        Some(serde_json::json!({ "queueState": label })),
                    );
                    return;
                }
            }
        }
        Ok(_) => {
            refuse_operation(
                state,
                device_id,
                operation,
                RejectCode::Malformed,
                "this host did not confirm the prompt was queued",
            );
            return;
        }
        Err(error) => {
            refuse_operation(
                state,
                device_id,
                operation,
                RejectCode::Malformed,
                &error.to_string(),
            );
            return;
        }
    };
    let item = queue::QueuedPrompt {
        operation_id: operation.operation_id,
        device_id,
        session_id,
        prompt,
        summary: operation.summary.clone(),
        enqueued_at_ms: now_ms(),
        expires_at_ms: operation.expires_at_ms,
        // The version the daemon recorded the row under, kept with the item
        // so this host's own cleanup can release it after the device's grant
        // has moved on.
        grant_version: provenance.grant_version,
        state: queue::QueueItemState::Queued,
        cancel_requested: None,
    };
    let position = match state.queues().lock().enqueue(item) {
        Ok(position) => position,
        Err(refusal) => {
            // The daemon already holds it durably; release it rather than
            // leave a prompt admitted that this host will never dispatch.
            state.cancel_remote_prompt(device_id, operation.operation_id, None);
            refuse_operation(
                state,
                device_id,
                operation,
                queue::refusal_code(&refusal),
                queue::refusal_message(&refusal),
            );
            return;
        }
    };
    // Record the binding before the item can ever run, so a query answers
    // `accepted` and a replay is answered from the record.
    let operation_id = operation.operation_id.to_string();
    let kind = format!("{:?}", operation.kind);
    if state
        .dedup()
        .record_accepted(
            &operation_id,
            &kind,
            &operation.summary,
            &device_id.to_string(),
            Some(&session_id.to_string()),
            operation.expires_at_ms,
            prompt_content_hash(&operation).as_deref(),
        )
        .is_err()
    {
        state.cancel_remote_prompt(device_id, operation.operation_id, None);
        state
            .queues()
            .lock()
            .cancel(session_id, operation.operation_id, device_id);
        complete(
            state,
            device_id,
            operation,
            OperationState::Unknown,
            Some(serde_json::json!("acceptance not persisted")),
        );
        return;
    }
    log::info!(
        target: "audit",
        "prompt queued durably; session={session_id} device={device_id} \
         operation={operation_id} submission={submission_id} position={position}"
    );
    complete_with_detail(
        state,
        device_id,
        operation,
        OperationState::Accepted,
        Some(serde_json::json!({
            "queued": true,
            "position": position,
            "submissionId": submission_id,
        })),
    );
    push_queue_view(state, session_id, device_id);
}

/// Cancel one queued prompt that has not been dispatched yet. The daemon
/// owns the durable row, so the cancellation reaches it: a prompt that was
/// admitted durably is removed durably, and a crash cannot resurrect it.
///
/// The intent is recorded before the daemon is asked. When the daemon
/// answers `dispatching` — the outcome is being decided right now — the
/// cancel is *kept* and this operation is answered `Accepted`: the host
/// honours it the moment the dispatch resolves, and pushes the final answer
/// itself. Answering `Rejected` here, as this path once did, dropped the
/// device's intent on the floor while the prompt could still run.
fn run_cancel_queued(state: &Arc<HostState>, device_id: Uuid, operation: OperationRequest) {
    let Some(session_id) = operation.session_id else {
        refuse_operation(
            state,
            device_id,
            &operation,
            RejectCode::Malformed,
            "session id required",
        );
        return;
    };
    let target = operation
        .payload
        .get("operationId")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| Uuid::parse_str(text).ok());
    let Some(target) = target else {
        refuse_operation(
            state,
            device_id,
            &operation,
            RejectCode::Malformed,
            "operationId field required",
        );
        return;
    };
    let Some(daemon) = state.daemon() else {
        refuse_operation(
            state,
            device_id,
            &operation,
            RejectCode::UnknownRoute,
            "workspace unreachable",
        );
        return;
    };
    let Ok(provenance) = adapter::device_provenance(state, device_id) else {
        refuse_operation(
            state,
            device_id,
            &operation,
            RejectCode::Malformed,
            "this host has no usable identity",
        );
        return;
    };
    // The cancel operation is recorded **before** the daemon is asked, so a
    // retry of it is answered from the record and its terminal state is
    // deduplicated like any other operation's.
    let cancel_operation_id = operation.operation_id.to_string();
    let kind = format!("{:?}", operation.kind);
    if let Err(error) = state.dedup().record_accepted(
        &cancel_operation_id,
        &kind,
        &operation.summary,
        &device_id.to_string(),
        operation.session_id.map(|id| id.to_string()).as_deref(),
        operation.expires_at_ms,
        // A cancellation carries no prompt body, so there is no content to
        // bind beyond the target it names — which the daemon checks itself.
        None,
    ) {
        log::warn!(target: "audit", "could not persist cancellation: {error}");
        complete(state, device_id, &operation, OperationState::Unknown, None);
        return;
    }
    // Bind the accepted cancellation to the prompt it names. This is what
    // lets a restarted host find the still-accepted cancel by itself and
    // resolve it by query — the device never has to replay its cancellation
    // just to learn what happened to it. A record that failed to bind
    // cannot promise an ending after a restart, so the request is answered
    // unknown instead of accepted-in-spirit.
    match state.dedup().set_cancel_target(
        &cancel_operation_id,
        &target.to_string(),
        provenance.grant_version,
    ) {
        Ok(true) => {}
        Ok(false) => {
            log::warn!(target: "audit", "could not bind cancellation target: record disappeared");
            let finished = state
                .dedup()
                .record_finished_if_accepted(
                    &cancel_operation_id,
                    &OperationState::Unknown,
                    Some("the cancellation target was not durably recorded"),
                )
                .unwrap_or(false);
            if finished {
                complete(state, device_id, &operation, OperationState::Unknown, None);
            }
            return;
        }
        Err(error) => {
            log::warn!(target: "audit", "could not bind cancellation target: {error}");
            let finished = state
                .dedup()
                .record_finished_if_accepted(
                    &cancel_operation_id,
                    &OperationState::Unknown,
                    Some("the cancellation target was not durably recorded"),
                )
                .unwrap_or(false);
            if finished {
                complete(state, device_id, &operation, OperationState::Unknown, None);
            }
            return;
        }
    }
    // The intent is recorded first, so a `dispatching` refusal cannot lose
    // it. A prompt this host does not hold for this device is refused
    // outright: there is nothing here to cancel.
    let known =
        state
            .queues()
            .lock()
            .request_cancel(session_id, target, device_id, operation.operation_id);
    if !known {
        let finished = state
            .dedup()
            .record_finished_if_accepted(
                &cancel_operation_id,
                &OperationState::Rejected {
                    code: RejectCode::Malformed,
                },
                Some("that prompt is no longer waiting in the queue"),
            )
            .unwrap_or(false);
        if finished {
            complete_with_detail(
                state,
                device_id,
                &operation,
                OperationState::Rejected {
                    code: RejectCode::Malformed,
                },
                Some(serde_json::json!({
                    "message": "that prompt is no longer waiting in the queue",
                })),
            );
        }
        return;
    }
    let settled = daemon.request(
        Uuid::nil(),
        Uuid::nil(),
        fintwind_client::Command::CancelRemotePrompt {
            provenance,
            operation_id: target,
            cancel_operation_id: Some(operation.operation_id),
        },
    );
    let label = match settled {
        Ok(fintwind_client::ResponsePayload::RemotePromptState { state: label, .. }) => label,
        // The daemon did not answer authoritatively: the prompt's fate is
        // unknown, so it is never re-sent, and the recorded intent stays
        // until the row settles one way or the other.
        _ => {
            complete(
                state,
                device_id,
                &operation,
                OperationState::Accepted,
                Some(serde_json::json!({
                    "cancelled": false,
                    "operationId": target,
                    "message": "the cancellation is recorded; retrying the workspace query",
                })),
            );
            return;
        }
    };
    match label.as_str() {
        "cancelled" | "expired" | "notSent" => {
            state.queues().lock().cancel(session_id, target, device_id);
            let prompt_result = prompt_result_for_cancel_state(&label);
            let prompt_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &target.to_string(),
                    &prompt_result,
                    Some("the prompt was not sent because it was cancelled"),
                )
                .unwrap_or(false);
            let cancel_finished = state
                .dedup()
                .record_finished_if_accepted(&cancel_operation_id, &OperationState::Executed, None)
                .unwrap_or(false);
            // The cancel operation itself is finished too, so a retry of it
            // is answered from this record rather than run again.
            if prompt_finished {
                complete_by_id(
                    state,
                    device_id,
                    &target,
                    prompt_result,
                    Some(serde_json::json!({
                        "cancelled": true,
                        "operationId": target,
                        "queueState": label,
                    })),
                );
            }
            push_queue_view(state, session_id, device_id);
            if cancel_finished {
                complete_with_detail(
                    state,
                    device_id,
                    &operation,
                    OperationState::Executed,
                    Some(serde_json::json!({
                        "cancelled": true,
                        "operationId": target,
                        "queueState": label,
                    })),
                );
            }
        }
        "dispatching" => {
            // The outcome is being decided right now. The intent stays on the
            // item; `dispatch_ready_queues` honours it the moment the
            // dispatch answers, and pushes this operation's final result
            // then. The device is told the cancel is recorded, not refused,
            // and never has to retry to find out.
            complete(
                state,
                device_id,
                &operation,
                OperationState::Accepted,
                Some(serde_json::json!({
                    "cancelled": false,
                    "operationId": target,
                    "message": "the cancellation is recorded; its outcome follows",
                })),
            );
        }
        "dispatched" => {
            // The prompt reached the driver. The cancel came too late and
            // must not be reported as a successful cancellation.
            state
                .queues()
                .lock()
                .take_cancel_request(session_id, target);
            state.queues().lock().mark_dispatched(session_id, target);
            let prompt_finished = state
                .dedup()
                .record_finished_if_accepted(&target.to_string(), &OperationState::Executed, None)
                .unwrap_or(false);
            let cancel_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &cancel_operation_id,
                    &OperationState::Rejected {
                        code: RejectCode::Malformed,
                    },
                    Some("the prompt had already reached the driver"),
                )
                .unwrap_or(false);
            push_queue_view(state, session_id, device_id);
            if prompt_finished {
                complete_by_id(
                    state,
                    device_id,
                    &target,
                    OperationState::Executed,
                    Some(serde_json::json!({ "dispatched": true })),
                );
            }
            if cancel_finished {
                complete_with_detail(
                    state,
                    device_id,
                    &operation,
                    OperationState::Rejected {
                        code: RejectCode::Malformed,
                    },
                    Some(serde_json::json!({
                        "cancelled": false,
                        "operationId": target,
                        "message": "the prompt had already reached the driver",
                    })),
                );
            }
            state.forget_orphaned_cancel(target, operation.operation_id);
        }
        _ => {
            // The daemon could not prove what happened. Keep the original
            // prompt non-dispatchable and preserve the honest unknown result;
            // an unrecognised label must never become a cancellation success.
            state.cancel_remote_prompt_as_recorded(device_id, target);
            state
                .queues()
                .lock()
                .take_cancel_request(session_id, target);
            state.queues().lock().settle_unknown(session_id, target);
            let prompt_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &target.to_string(),
                    &OperationState::Unknown,
                    Some("the prompt outcome could not be confirmed"),
                )
                .unwrap_or(false);
            let cancel_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &cancel_operation_id,
                    &OperationState::Unknown,
                    Some("the cancellation outcome could not be confirmed"),
                )
                .unwrap_or(false);
            push_queue_view(state, session_id, device_id);
            if prompt_finished {
                complete_by_id(
                    state,
                    device_id,
                    &target,
                    OperationState::Unknown,
                    Some(serde_json::json!({
                        "cancelled": false,
                        "operationId": target,
                        "message": "the prompt outcome could not be confirmed",
                    })),
                );
            }
            if cancel_finished {
                complete_with_detail(
                    state,
                    device_id,
                    &operation,
                    OperationState::Unknown,
                    Some(serde_json::json!({
                        "cancelled": false,
                        "operationId": target,
                        "message": "the cancellation outcome could not be confirmed",
                    })),
                );
            }
            state.forget_orphaned_cancel(target, operation.operation_id);
        }
    }
}

/// Honour one recorded cancel whose dispatch has since resolved without the
/// prompt running.
///
/// The daemon is asked once more, now that the row is back to `queued`; a
/// prompt that already reached the driver is never cancelled and never
/// reported as unsent — it ran, and the device is told so.
/// Resolve one cancel whose dispatch has since resolved, by asking the
/// daemon what it durably recorded.
///
/// The local intent is read **without** being cleared first: clearing it
/// before the daemon answers is how a cancel used to vanish — an
/// unanswered request left the prompt looking uncancelled and the next tick
/// dispatched it. Here the daemon is asked, and only an authoritative
/// answer moves the local state. Nothing is ever re-sent: a cancel whose
/// outcome is already unknown is queried, never repeated.
fn honour_pending_cancel(state: &Arc<HostState>, item: &queue::QueuedPrompt) {
    let cancel_operation_id = match state
        .queues()
        .lock()
        .pending_cancel_operation(item.session_id, item.operation_id)
    {
        Some(operation_id) => operation_id,
        None => return,
    };
    let Some((recorded, cancel_state)) = state.query_remote_cancel_at_grant(
        item.device_id,
        item.grant_version,
        item.operation_id,
        Some(cancel_operation_id),
    ) else {
        // The daemon did not answer. The intent stays exactly where it is,
        // so a later tick asks again — and the prompt is not dispatched
        // while a cancel is waiting on it.
        return;
    };
    // The daemon's own record is authoritative, including which cancel
    // operation it bound. A different one means this host's view is stale.
    if recorded != Some(cancel_operation_id) {
        return;
    }
    match cancel_state.as_str() {
        // The daemon still owns the prompt and the cancellation is still
        // pending. Keep both records untouched and query again next tick.
        "pending" => return,
        "cancelled" => {
            let _ = state
                .queues()
                .lock()
                .take_cancel_request(item.session_id, item.operation_id);
            state
                .queues()
                .lock()
                .cancel(item.session_id, item.operation_id, item.device_id);
            let prompt_result = prompt_result_for_cancel_state(cancel_state.as_str());
            let prompt_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &item.operation_id.to_string(),
                    &prompt_result,
                    Some("the prompt was not sent because it was cancelled"),
                )
                .unwrap_or(false);
            let cancel_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &cancel_operation_id.to_string(),
                    &OperationState::Executed,
                    None,
                )
                .unwrap_or(false);
            push_queue_view(state, item.session_id, item.device_id);
            if prompt_finished {
                complete_by_id(
                    state,
                    item.device_id,
                    &item.operation_id,
                    prompt_result,
                    Some(serde_json::json!({ "cancelled": true })),
                );
            }
            if cancel_finished {
                complete_by_id(
                    state,
                    item.device_id,
                    &cancel_operation_id,
                    OperationState::Executed,
                    Some(serde_json::json!({
                        "cancelled": true,
                        "operationId": item.operation_id,
                    })),
                );
            }
            state.forget_orphaned_cancel(item.operation_id, cancel_operation_id);
        }
        // The prompt reached the driver. It ran; saying otherwise would be a
        // lie, and cancelling it now is impossible.
        "dispatched" | "dispatching" => {
            let _ = state
                .queues()
                .lock()
                .take_cancel_request(item.session_id, item.operation_id);
            state
                .queues()
                .lock()
                .mark_dispatched(item.session_id, item.operation_id);
            // The normal dispatch path may already have recorded and pushed
            // the prompt's `Executed` result. CAS the record so this cancel
            // path sends that prompt result only when it is the first owner.
            let prompt_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &item.operation_id.to_string(),
                    &OperationState::Executed,
                    None,
                )
                .unwrap_or(false);
            let cancel_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &cancel_operation_id.to_string(),
                    &OperationState::Rejected {
                        code: RejectCode::Malformed,
                    },
                    Some("the prompt had already reached the driver"),
                )
                .unwrap_or(false);
            if prompt_finished {
                complete_by_id(
                    state,
                    item.device_id,
                    &item.operation_id,
                    OperationState::Executed,
                    Some(serde_json::json!({ "dispatched": true })),
                );
            }
            if cancel_finished {
                complete_by_id(
                    state,
                    item.device_id,
                    &cancel_operation_id,
                    OperationState::Rejected {
                        code: RejectCode::Malformed,
                    },
                    Some(serde_json::json!({
                        "cancelled": false,
                        "operationId": item.operation_id,
                        "message": "the prompt had already reached the driver",
                    })),
                );
            }
            state.forget_orphaned_cancel(item.operation_id, cancel_operation_id);
        }
        // The row is gone, or the daemon holds no recorded ending for it. A
        // missing row proves nothing — rows are deleted when a session goes
        // away and when a queue is swept — so absence is **not** read as a
        // cancellation. `expired` and `notSent` do mean the prompt did not
        // run, so the cancellation is settled as holding; `unknown` and a
        // bare absence are settled as unknown, never as a success.
        "expired" | "notSent" => {
            let _ = state
                .queues()
                .lock()
                .take_cancel_request(item.session_id, item.operation_id);
            state
                .queues()
                .lock()
                .cancel(item.session_id, item.operation_id, item.device_id);
            let prompt_result = prompt_result_for_cancel_state(cancel_state.as_str());
            let prompt_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &item.operation_id.to_string(),
                    &prompt_result,
                    Some("the prompt did not reach the driver"),
                )
                .unwrap_or(false);
            let cancel_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &cancel_operation_id.to_string(),
                    &OperationState::Executed,
                    None,
                )
                .unwrap_or(false);
            push_queue_view(state, item.session_id, item.device_id);
            if prompt_finished {
                complete_by_id(
                    state,
                    item.device_id,
                    &item.operation_id,
                    prompt_result,
                    Some(serde_json::json!({
                        "cancelled": true,
                        "operationId": item.operation_id,
                        "queueState": cancel_state,
                    })),
                );
            }
            if cancel_finished {
                complete_by_id(
                    state,
                    item.device_id,
                    &cancel_operation_id,
                    OperationState::Executed,
                    Some(serde_json::json!({
                        "cancelled": true,
                        "operationId": item.operation_id,
                    })),
                );
            }
            state.forget_orphaned_cancel(item.operation_id, cancel_operation_id);
        }
        // `unknown`, a bare absence, or a state this host does not
        // interpret. The honest ending is unknown: the evidence ran out and
        // is never upgraded to a success by inference. The intent is dropped
        // so the prompt is not held forever by a question the daemon cannot
        // answer, and the device is told the truth.
        _ => {
            let _ = state
                .queues()
                .lock()
                .take_cancel_request(item.session_id, item.operation_id);
            // If the daemon has no terminal record, also release any live
            // prompt row under its recorded grant. The local unknown tombstone
            // prevents this process from dispatching it; this cleanup keeps a
            // later restart from resurrecting the daemon row.
            state.cancel_remote_prompt_as_recorded(item.device_id, item.operation_id);
            state
                .queues()
                .lock()
                .settle_unknown(item.session_id, item.operation_id);
            let prompt_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &item.operation_id.to_string(),
                    &OperationState::Unknown,
                    Some("the prompt outcome could not be confirmed"),
                )
                .unwrap_or(false);
            let cancel_finished = state
                .dedup()
                .record_finished_if_accepted(
                    &cancel_operation_id.to_string(),
                    &OperationState::Unknown,
                    Some("the workspace could not confirm the cancellation"),
                )
                .unwrap_or(false);
            if prompt_finished {
                complete_by_id(
                    state,
                    item.device_id,
                    &item.operation_id,
                    OperationState::Unknown,
                    Some(serde_json::json!({
                        "cancelled": false,
                        "message": "the prompt outcome could not be confirmed",
                    })),
                );
            }
            if cancel_finished {
                complete_by_id(
                    state,
                    item.device_id,
                    &cancel_operation_id,
                    OperationState::Unknown,
                    Some(serde_json::json!({
                        "cancelled": false,
                        "operationId": item.operation_id,
                        "message": "the workspace could not confirm the cancellation",
                    })),
                );
            }
            state.forget_orphaned_cancel(item.operation_id, cancel_operation_id);
            push_queue_view(state, item.session_id, item.device_id);
        }
    }
}

/// Answer one device's options-catalog request from the host's own probe.
/// The operation settles as executed; the catalog itself travels as its
/// own typed message, so a large model list never rides inside a result
/// detail (and is never persisted).
fn run_options_catalog(state: &Arc<HostState>, device_id: Uuid, operation: OperationRequest) {
    let Some(session_id) = operation.session_id else {
        refuse_operation(
            state,
            device_id,
            &operation,
            RejectCode::Malformed,
            "session id required",
        );
        return;
    };
    let Some(daemon) = state.daemon() else {
        refuse_operation(
            state,
            device_id,
            &operation,
            RejectCode::UnknownRoute,
            "workspace unreachable",
        );
        return;
    };
    let outcome = {
        let mut catalog = state.catalog().lock();
        adapter::execute(state, &daemon, &mut catalog, device_id, &operation)
    };
    match outcome {
        Ok(detail) => {
            let models: Vec<fintwind_remote_protocol::ModelOptionEntry> = detail
                .get("models")
                .and_then(serde_json::Value::as_array)
                .map(|models| {
                    models
                        .iter()
                        .map(|model| fintwind_remote_protocol::ModelOptionEntry {
                            id: model
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            name: model
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            is_default: model
                                .get("is_default")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            reasoning_efforts: string_list(model, "reasoning_efforts"),
                            service_tiers: string_list(model, "service_tiers"),
                            context_windows: string_list(model, "context_windows"),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let agent_presets: Vec<fintwind_remote_protocol::AgentPresetEntry> = detail
                .get("agentPresets")
                .and_then(serde_json::Value::as_array)
                .map(|presets| {
                    presets
                        .iter()
                        .map(|preset| fintwind_remote_protocol::AgentPresetEntry {
                            id: preset
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            name: preset
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            is_default: preset
                                .get("is_default")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default();
            state.send_to_device(
                MessageBody::OptionsCatalog {
                    session_id,
                    models,
                    agent_presets,
                },
                device_id,
                PendingKind::Event,
            );
            complete(state, device_id, &operation, OperationState::Executed, None);
        }
        Err((code, message)) => refuse_operation(state, device_id, &operation, code, &message),
    }
}

/// One option-id list off a probed model entry.
fn string_list(entry: &serde_json::Value, key: &str) -> Vec<String> {
    entry
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Execute one read-only operation. The content travels in the result the
/// device sees; **nothing** is persisted in the dedup record or logged, so
/// a replay answers the recorded state and a query never returns a body.
fn run_read(state: &Arc<HostState>, device_id: Uuid, operation: OperationRequest) {
    let Some(daemon) = state.daemon() else {
        refuse_operation(
            state,
            device_id,
            &operation,
            RejectCode::UnknownRoute,
            "workspace unreachable",
        );
        return;
    };
    let outcome = {
        let mut catalog = state.catalog().lock();
        adapter::execute(state, &daemon, &mut catalog, device_id, &operation)
    };
    match outcome {
        Ok(detail) => {
            // Recorded as executed with **no** detail: the read is
            // repeatable, and its content is not durable state.
            let operation_id = operation.operation_id.to_string();
            let kind = format!("{:?}", operation.kind);
            let _ = state.dedup().record_accepted(
                &operation_id,
                &kind,
                &operation.summary,
                &device_id.to_string(),
                operation.session_id.map(|id| id.to_string()).as_deref(),
                operation.expires_at_ms,
                prompt_content_hash(&operation).as_deref(),
            );
            state
                .dedup()
                .record_finished(&operation_id, &OperationState::Executed, None)
                .ok();
            complete_with_detail(
                state,
                device_id,
                &operation,
                OperationState::Executed,
                Some(detail),
            );
        }
        Err((code, message)) => {
            refuse_operation(state, device_id, &operation, code, &message);
        }
    }
}

/// Dispatch every queued prompt whose session has gone idle.
///
/// The daemon owns the preparation and the dispatch: it hydrates the session
/// from its own task state, prepares the workspace and its checkpoint, starts
/// the driver if the session has none, and only then claims the submission.
/// `executed` here means the prompt reached the driver — a turn running is
/// not a finished task, and the two are never conflated.
fn dispatch_ready_queues(state: &Arc<HostState>) {
    let Some(daemon) = state.daemon() else {
        return;
    };
    let sessions: Vec<Uuid> = state.queues().lock().pending_sessions();
    for session_id in sessions {
        // One session's head may be an expired prompt with a runnable one
        // behind it: the expired head is settled — cancelled on the daemon
        // too, so nothing durable keeps a prompt that will never run — and
        // the next item is taken. Skipping it silently would hold the
        // queue behind a prompt nobody waits for any more.
        loop {
            if state.is_session_busy(session_id) {
                break;
            }
            let Some(item) = state.queues().lock().next_dispatchable(session_id) else {
                break;
            };
            if item.expires_at_ms <= now_ms() {
                state.cancel_remote_prompt_as_recorded(item.device_id, item.operation_id);
                settle_released_cancel(state, &item);
                state
                    .queues()
                    .lock()
                    .cancel(session_id, item.operation_id, item.device_id);
                state
                    .dedup()
                    .record_finished(
                        &item.operation_id.to_string(),
                        &OperationState::Expired,
                        None,
                    )
                    .ok();
                complete_by_id(
                    state,
                    item.device_id,
                    &item.operation_id,
                    OperationState::Expired,
                    None,
                );
                push_queue_view(state, session_id, item.device_id);
                continue;
            }
            // A disconnected view is not deletion. Restored durable prompts
            // wait for an owner to re-attach instead of being cancelled during
            // the host's startup gap.
            if state.session_owners(session_id).is_empty() {
                break;
            }
            if state.is_session_archived(session_id) {
                let dropped_items = state.queues().lock().drop_session(session_id);
                for dropped in dropped_items {
                    state.cancel_remote_prompt_as_recorded(dropped.device_id, dropped.operation_id);
                    settle_released_cancel(state, &dropped);
                    report_dropped_queue_item(
                        state,
                        &dropped,
                        "the session was removed or archived",
                    );
                }
                break;
            }
            // A cancel awaiting this prompt is honoured before anything is
            // dispatched: the device asked for it not to run, so the next
            // thing that happens to it is its release, not a new attempt.
            if state
                .queues()
                .lock()
                .cancel_requested(session_id, item.operation_id)
            {
                honour_pending_cancel(state, &item);
                continue;
            }
            let Ok(provenance) = adapter::device_provenance(state, item.device_id) else {
                settle_undispatched(state, &item, "this host has no usable identity");
                break;
            };
            state
                .queues()
                .lock()
                .mark_dispatching(session_id, item.operation_id);
            push_queue_view(state, session_id, item.device_id);
            let outcome = daemon.request(
                Uuid::nil(),
                Uuid::nil(),
                fintwind_client::Command::DispatchRemotePrompt {
                    provenance,
                    operation_id: item.operation_id,
                },
            );
            match outcome {
                Ok(fintwind_client::ResponsePayload::RemotePromptState {
                    state: label, ..
                }) if label == "dispatched" => {
                    // Dispatch may create the first runtime. Upgrade every
                    // already-open pre-runtime view before delivering events.
                    if let Ok(fintwind_client::ResponsePayload::SessionRuntime {
                        runtime_id: Some(runtime_id),
                        ..
                    }) = daemon.request(
                        session_id,
                        Uuid::nil(),
                        fintwind_client::Command::AttachSession,
                    ) {
                        let mut changed = false;
                        for owner in state.session_owners(session_id) {
                            if state.session_runtime_for(session_id, owner) != Some(runtime_id) {
                                state.attach_view(session_id, owner, runtime_id);
                                changed = true;
                            }
                        }
                        if changed {
                            events::spawn_session_pump(Arc::clone(state), session_id, runtime_id);
                        }
                    }
                    state
                        .queues()
                        .lock()
                        .mark_dispatched(session_id, item.operation_id);
                    let prompt_finished = state
                        .dedup()
                        .record_finished_if_accepted(
                            &item.operation_id.to_string(),
                            &OperationState::Executed,
                            None,
                        )
                        .unwrap_or(false);
                    if prompt_finished {
                        complete_by_id(
                            state,
                            item.device_id,
                            &item.operation_id,
                            OperationState::Executed,
                            Some(serde_json::json!({ "dispatched": true })),
                        );
                    }
                    // A cancel that arrived while this dispatch was in
                    // flight came too late: the prompt ran. Say so, rather
                    // than letting the device believe its cancel held.
                    if state
                        .queues()
                        .lock()
                        .cancel_requested(session_id, item.operation_id)
                    {
                        honour_pending_cancel(state, &item);
                    }
                    push_queue_view(state, session_id, item.device_id);
                }
                Ok(fintwind_client::ResponsePayload::RemotePromptState {
                    state: label, ..
                }) if label == "queued" || label == "dispatching" => {
                    // The daemon still holds it: the session is busy, or
                    // another caller claimed the dispatch first. The prompt
                    // stays queued and a later tick takes it — an ordinary
                    // prompt waits its turn and is never reported settled
                    // while it has not run.
                    state
                        .queues()
                        .lock()
                        .mark_queued(session_id, item.operation_id);
                    // A cancel waiting on this prompt is honoured now: the
                    // row is back to `queued`, so the daemon can release it
                    // and the device gets its final answer this tick.
                    if state
                        .queues()
                        .lock()
                        .cancel_requested(session_id, item.operation_id)
                    {
                        honour_pending_cancel(state, &item);
                    }
                    push_queue_view(state, session_id, item.device_id);
                }
                Ok(fintwind_client::ResponsePayload::RemotePromptState {
                    state: label, ..
                }) if label == "unknown" => {
                    // The daemon could not prove the dispatch outcome. A
                    // local dispatch retry is forbidden; if a cancel was
                    // waiting, keep it in the orphan set for query-only
                    // recovery instead of dropping the device's intent.
                    if state
                        .queues()
                        .lock()
                        .cancel_requested(session_id, item.operation_id)
                    {
                        honour_pending_cancel(state, &item);
                        if state
                            .queues()
                            .lock()
                            .pending_cancel_operation(session_id, item.operation_id)
                            .is_some()
                        {
                            if let Some(cancel_operation_id) = state
                                .queues()
                                .lock()
                                .take_cancel_request(session_id, item.operation_id)
                            {
                                state.record_orphaned_cancels(vec![OrphanedCancel {
                                    prompt_operation_id: item.operation_id,
                                    cancel_operation_id,
                                    device_id: Some(item.device_id),
                                    grant_version: Some(item.grant_version),
                                }]);
                            }
                        }
                    }
                    let still_live = state
                        .queues()
                        .lock()
                        .state_for(item.operation_id)
                        .is_some_and(|queue_state| !queue_state.is_terminal());
                    if still_live {
                        state.cancel_remote_prompt_as_recorded(item.device_id, item.operation_id);
                        state
                            .queues()
                            .lock()
                            .settle_unknown(session_id, item.operation_id);
                        let prompt_finished = state
                            .dedup()
                            .record_finished_if_accepted(
                                &item.operation_id.to_string(),
                                &OperationState::Unknown,
                                Some("the dispatch outcome could not be confirmed"),
                            )
                            .unwrap_or(false);
                        if prompt_finished {
                            complete_by_id(
                                state,
                                item.device_id,
                                &item.operation_id,
                                OperationState::Unknown,
                                Some(serde_json::json!({
                                    "message": "the dispatch outcome could not be confirmed",
                                })),
                            );
                        }
                    }
                    push_queue_view(state, session_id, item.device_id);
                }
                Ok(fintwind_client::ResponsePayload::RemotePromptState {
                    state: label, ..
                }) => {
                    // The daemon settled it without dispatching (expired,
                    // cancelled, never sent). Nothing ran, so nothing is
                    // reported as executed.
                    let prompt_result = prompt_result_for_cancel_state(&label);
                    state
                        .queues()
                        .lock()
                        .cancel(session_id, item.operation_id, item.device_id);
                    let prompt_finished = state
                        .dedup()
                        .record_finished_if_accepted(
                            &item.operation_id.to_string(),
                            &prompt_result,
                            None,
                        )
                        .unwrap_or(false);
                    if prompt_finished {
                        complete_by_id(
                            state,
                            item.device_id,
                            &item.operation_id,
                            prompt_result,
                            Some(serde_json::json!({ "queueState": label })),
                        );
                    }
                    push_queue_view(state, session_id, item.device_id);
                }
                Ok(_) => {
                    settle_undispatched(state, &item, "this host did not confirm the dispatch");
                }
                Err(error) => {
                    settle_undispatched(state, &item, &error.to_string());
                }
            }
            break;
        }
    }
}

/// Answer a cancel that was waiting on a prompt this host has just released.
///
/// The host knows the outcome because it caused it: the prompt will not run,
/// which is exactly what the cancel asked for. No daemon query is needed —
/// and nothing is re-sent — because the release is this host's own act.
fn settle_released_cancel(state: &Arc<HostState>, item: &queue::QueuedPrompt) {
    let Some(cancel_operation_id) = state
        .queues()
        .lock()
        .take_cancel_request(item.session_id, item.operation_id)
    else {
        return;
    };
    let cancel_finished = state
        .dedup()
        .record_finished_if_accepted(
            &cancel_operation_id.to_string(),
            &OperationState::Executed,
            None,
        )
        .unwrap_or(false);
    if cancel_finished {
        complete_by_id(
            state,
            item.device_id,
            &cancel_operation_id,
            OperationState::Executed,
            Some(serde_json::json!({
                "cancelled": true,
                "operationId": item.operation_id,
            })),
        );
    }
    state.forget_orphaned_cancel(item.operation_id, cancel_operation_id);
}

/// Answer a replayed cancellation from the daemon's durable record.
///
/// A replay of a cancel the host already took is a question, not a request:
/// asking the daemon to cancel again would repeat a side effect whose
/// outcome may already be decided, which this system never does. The daemon
/// is therefore only *asked*, and its recorded ending is reported. When it
/// holds nothing, the honest answer is unknown — never a cancellation
/// inferred from a missing row.
fn resolve_replayed_cancel(
    state: &Arc<HostState>,
    device_id: Uuid,
    operation: &OperationRequest,
    target: Uuid,
) {
    let Some(grant_version) = state
        .dedup()
        .cancel_grant_version(&operation.operation_id.to_string())
    else {
        let finished = state
            .dedup()
            .record_finished_if_accepted(
                &operation.operation_id.to_string(),
                &OperationState::Unknown,
                Some("the cancellation grant was not durably recorded"),
            )
            .unwrap_or(false);
        if finished {
            complete(
                state,
                device_id,
                operation,
                OperationState::Unknown,
                Some(serde_json::json!({
                    "cancelled": false,
                    "operationId": target,
                    "message": "the cancellation grant was not durably recorded",
                })),
            );
        }
        return;
    };
    let query = state.query_remote_cancel_at_grant(
        device_id,
        grant_version,
        target,
        Some(operation.operation_id),
    );
    let Some((recorded, cancel_state)) = query else {
        complete(
            state,
            device_id,
            operation,
            OperationState::Accepted,
            Some(serde_json::json!({
                "cancelled": false,
                "operationId": target,
                "message": "the cancellation is recorded; retrying the workspace query",
            })),
        );
        return;
    };
    if recorded != Some(operation.operation_id) {
        complete(
            state,
            device_id,
            operation,
            OperationState::Accepted,
            Some(serde_json::json!({
                "cancelled": false,
                "operationId": target,
                "message": "the workspace did not confirm this cancellation identity; retrying",
            })),
        );
        return;
    }
    if cancel_state == "pending" {
        complete(
            state,
            device_id,
            operation,
            OperationState::Accepted,
            Some(serde_json::json!({
                "cancelled": false,
                "operationId": target,
                "message": "the cancellation is still pending",
            })),
        );
        return;
    }
    let (result, cancelled) = cancel_result_for_prompt_state(&cancel_state);
    let prompt_result = prompt_result_for_cancel_state(&cancel_state);
    if let Some(item) = state.queues().lock().item_for(target) {
        state
            .queues()
            .lock()
            .take_cancel_request(item.session_id, target);
        match cancel_state.as_str() {
            "cancelled" | "expired" | "notSent" => {
                state
                    .queues()
                    .lock()
                    .cancel(item.session_id, target, item.device_id);
            }
            "dispatched" | "dispatching" => {
                state
                    .queues()
                    .lock()
                    .mark_dispatched(item.session_id, target);
            }
            _ => {
                state.cancel_remote_prompt_as_recorded(item.device_id, target);
                state
                    .queues()
                    .lock()
                    .settle_unknown(item.session_id, target);
            }
        }
        push_queue_view(state, item.session_id, item.device_id);
    }
    let prompt_finished = state
        .dedup()
        .record_finished_if_accepted(
            &target.to_string(),
            &prompt_result,
            Some("the prompt outcome was resolved from the daemon record"),
        )
        .unwrap_or(false);
    if prompt_finished {
        complete_by_id(
            state,
            device_id,
            &target,
            prompt_result,
            Some(serde_json::json!({
                "cancelled": cancelled,
                "operationId": target,
                "cancelState": cancel_state,
            })),
        );
    }
    let cancel_finished = state
        .dedup()
        .record_finished_if_accepted(&operation.operation_id.to_string(), &result, None)
        .unwrap_or(false);
    if cancel_finished {
        complete_with_detail(
            state,
            device_id,
            operation,
            result,
            Some(serde_json::json!({
                "cancelled": cancelled,
                "operationId": target,
                "cancelState": cancel_state,
            })),
        );
        state.forget_orphaned_cancel(target, operation.operation_id);
    } else if state
        .dedup()
        .lookup(&operation.operation_id.to_string())
        .is_some_and(|record| !matches!(record.state, OperationState::Accepted))
    {
        // A concurrent recovery path already settled this C. Do not leave a
        // stale orphan that could emit the same terminal answer later.
        state.forget_orphaned_cancel(target, operation.operation_id);
    }
}

/// Deliver the ending of every cancellation whose prompt is no longer in the
/// dispatch queue.
///
/// These were answered `Accepted` and then their prompt either reached the
/// driver or its row went away, so the queue path can no longer settle them.
/// Each is resolved by asking the daemon what it recorded — never by
/// re-sending the cancellation — and the device gets its final answer.
///
/// The set is **peeked, not taken**. An entry leaves only when a
/// deterministic ending has been delivered: a query that fails, is refused,
/// or comes back with a binding that is not this entry's leaves the entry
/// exactly where it is, and the next tick asks again. Draining first is how
/// an ending used to be lost — a single failed query silently retired a
/// cancellation the device was still waiting on.
fn resolve_orphaned_cancels(state: &Arc<HostState>) {
    let pending = state.orphaned_cancels();
    let mut delivered: Vec<OrphanedCancel> = Vec::new();
    for entry in pending {
        let device_id = match entry
            .device_id
            .or_else(|| state.queue_device_for(entry.prompt_operation_id))
        {
            Some(device_id) => device_id,
            // Nobody to deliver to yet. The entry stays; the device may
            // reconnect, and the ending keeps until it can be handed over.
            None => continue,
        };
        // Only a query, and only about *this* cancellation: a prompt can
        // carry more than one over its life, and the unnamed query answers
        // with whichever ended last.
        let (recorded, cancel_state) = match entry.grant_version {
            Some(grant_version) => {
                let Some(query) = state.query_remote_cancel_at_grant(
                    device_id,
                    grant_version,
                    entry.prompt_operation_id,
                    Some(entry.cancel_operation_id),
                ) else {
                    // The daemon did not answer. The entry stays; asking
                    // again is the whole recovery, and it never re-sends
                    // anything.
                    continue;
                };
                query
            }
            None => (Some(entry.cancel_operation_id), "unknown".to_owned()),
        };
        if recorded != Some(entry.cancel_operation_id) {
            // The daemon is not holding this cancellation under this id.
            // That is not an ending, so nothing is answered from it and the
            // entry stays for a later tick.
            continue;
        }
        if cancel_state == "pending" {
            // Still waiting on a live row. The next tick asks again.
            continue;
        }
        let (result, cancelled) = cancel_result_for_prompt_state(&cancel_state);
        if matches!(&result, OperationState::Unknown)
            && let Some(item) = state.queues().lock().item_for(entry.prompt_operation_id)
        {
            // This is the crash window where the host accepted C but the
            // daemon never recorded C. The local prompt must become a
            // durable non-dispatchable tombstone as well; otherwise the
            // dispatch tick could run it before the next recovery query.
            state.cancel_remote_prompt_as_recorded(item.device_id, item.operation_id);
            state
                .queues()
                .lock()
                .take_cancel_request(item.session_id, item.operation_id);
            state
                .queues()
                .lock()
                .settle_unknown(item.session_id, item.operation_id);
            push_queue_view(state, item.session_id, item.device_id);
        }
        // A host crash may have left the original prompt accepted locally
        // while only the cancellation's durable ending survived. Reconcile
        // the prompt operation too, but do not emit a duplicate if it was
        // already settled before the crash.
        let prompt_state = prompt_result_for_cancel_state(&cancel_state);
        let prompt_finished = state
            .dedup()
            .record_finished_if_accepted(
                &entry.prompt_operation_id.to_string(),
                &prompt_state,
                None,
            )
            .unwrap_or(false);
        if prompt_finished {
            complete_by_id(
                state,
                device_id,
                &entry.prompt_operation_id,
                prompt_state,
                Some(serde_json::json!({
                    "cancelled": cancelled,
                    "operationId": entry.prompt_operation_id,
                    "cancelState": cancel_state,
                })),
            );
        }
        let cancel_finished = state
            .dedup()
            .record_finished_if_accepted(&entry.cancel_operation_id.to_string(), &result, None)
            .unwrap_or(false);
        if cancel_finished {
            complete_by_id(
                state,
                device_id,
                &entry.cancel_operation_id,
                result,
                Some(serde_json::json!({
                    "cancelled": cancelled,
                    "operationId": entry.prompt_operation_id,
                    "cancelState": cancel_state,
                })),
            );
            delivered.push(entry);
        } else if state
            .dedup()
            .lookup(&entry.cancel_operation_id.to_string())
            .is_some_and(|record| !matches!(record.state, OperationState::Accepted))
        {
            // Another path already settled and delivered this cancellation.
            delivered.push(entry);
        }
    }
    // Only now are finished entries removed; everything else survives.
    state.remove_orphaned_cancels(&delivered);
}

/// A dispatch response that did not prove a terminal state is unknown, never
/// retried, and kept as a local tombstone. A waiting cancel is moved to the
/// query-only orphan set instead of being guessed as successful.
fn settle_undispatched(state: &Arc<HostState>, item: &queue::QueuedPrompt, reason: &str) {
    state.cancel_remote_prompt_as_recorded(item.device_id, item.operation_id);
    if let Some(cancel_operation_id) = state
        .queues()
        .lock()
        .take_cancel_request(item.session_id, item.operation_id)
    {
        state.record_orphaned_cancels(vec![OrphanedCancel {
            prompt_operation_id: item.operation_id,
            cancel_operation_id,
            device_id: Some(item.device_id),
            grant_version: Some(item.grant_version),
        }]);
    }
    state
        .queues()
        .lock()
        .settle_unknown(item.session_id, item.operation_id);
    state
        .dedup()
        .record_finished(
            &item.operation_id.to_string(),
            &OperationState::Unknown,
            Some(reason),
        )
        .ok();
    complete_by_id(
        state,
        item.device_id,
        &item.operation_id,
        OperationState::Unknown,
        Some(serde_json::json!({ "message": reason })),
    );
    push_queue_view(state, item.session_id, item.device_id);
}

/// Report every queued prompt whose validity window elapsed. An expired
/// prompt never runs — including on the daemon, whose durable copy is
/// released here rather than left waiting for a dispatch that will not
/// come.
fn expire_due_queue_items(state: &Arc<HostState>) {
    let expired = state.queues().lock().expire_due(now_ms());
    for item in expired {
        state.cancel_remote_prompt_as_recorded(item.device_id, item.operation_id);
        settle_released_cancel(state, &item);
        state
            .dedup()
            .record_finished(
                &item.operation_id.to_string(),
                &OperationState::Expired,
                None,
            )
            .ok();
        complete_by_id(
            state,
            item.device_id,
            &item.operation_id,
            OperationState::Expired,
            None,
        );
        push_queue_view(state, item.session_id, item.device_id);
    }
}

/// Push one session's visible queue to one device.
fn push_queue_view(state: &Arc<HostState>, session_id: Uuid, device_id: Uuid) {
    let items: Vec<QueueItemEntry> = state.queues().lock().snapshot(session_id);
    state.send_to_device(
        MessageBody::SessionQueue { session_id, items },
        device_id,
        PendingKind::Event,
    );
}

/// Tell one device its queued prompt was dropped (revoked, or its target
/// disappeared). The item never ran and never will.
fn report_dropped_queue_item(state: &Arc<HostState>, item: &queue::QueuedPrompt, reason: &str) {
    state.send_to_device(
        MessageBody::OperationResult(OperationOutcome {
            operation_id: item.operation_id,
            state: OperationState::Rejected {
                code: RejectCode::AuthFailed,
            },
            detail: Some(serde_json::json!({ "message": reason }).to_string()),
        }),
        item.device_id,
        PendingKind::Result,
    );
}

fn run_permission_decision(state: &Arc<HostState>, device_id: Uuid, decision: PermissionDecision) {
    let approval_id = decision.approval_id.to_string();
    let Some(offer) = state.approvals().offer_snapshot(&approval_id) else {
        complete_by_id(
            state,
            device_id,
            &decision.approval_id,
            OperationState::Unknown,
            None,
        );
        return;
    };
    let context = decision_context(state, device_id, offer.session_id());
    match state
        .approvals()
        .consume(&approval_id, &decision.option_id, None, &context)
    {
        Ok((offer, option_id)) => {
            let outcome = state
                .daemon()
                .ok_or_else(|| anyhow::anyhow!("workspace unreachable"))
                .and_then(|daemon| {
                    daemon.request(
                        offer.session_id,
                        offer.runtime_id,
                        Command::Respond {
                            request_id: offer.request_ref.clone(),
                            option_id: option_id.clone(),
                        },
                    )
                });
            let (op_state, detail) = match outcome {
                Ok(_) => (OperationState::Executed, None),
                Err(error) => (
                    OperationState::Rejected {
                        code: RejectCode::Malformed,
                    },
                    Some(serde_json::json!({ "message": error.to_string() })),
                ),
            };
            complete_by_id(state, device_id, &decision.approval_id, op_state, detail);
        }
        Err(error) => {
            let (op_state, detail) = decision_failure(error);
            complete_by_id(state, device_id, &decision.approval_id, op_state, detail);
        }
    }
}

fn run_user_input_decision(state: &Arc<HostState>, device_id: Uuid, decision: UserInputDecision) {
    let approval_id = decision.approval_id.to_string();
    let Some(offer) = state.approvals().offer_snapshot(&approval_id) else {
        complete_by_id(
            state,
            device_id,
            &decision.approval_id,
            OperationState::Unknown,
            None,
        );
        return;
    };
    let context = decision_context(state, device_id, offer.session_id());
    match state
        .approvals()
        .consume_user_input(&approval_id, &decision.answers, &context)
    {
        Ok(answers) => {
            let outcome = state
                .daemon()
                .ok_or_else(|| anyhow::anyhow!("workspace unreachable"))
                .and_then(|daemon| {
                    daemon.request(
                        offer.session_id(),
                        offer.runtime_id(),
                        Command::RespondUserInput {
                            request_id: offer.request_ref().to_owned(),
                            answers,
                        },
                    )
                });
            let (op_state, detail) = match outcome {
                Ok(_) => (OperationState::Executed, None),
                Err(error) => (
                    OperationState::Rejected {
                        code: RejectCode::Malformed,
                    },
                    Some(serde_json::json!({ "message": error.to_string() })),
                ),
            };
            complete_by_id(state, device_id, &decision.approval_id, op_state, detail);
        }
        Err(error) => {
            let (op_state, detail) = decision_failure(error);
            complete_by_id(state, device_id, &decision.approval_id, op_state, detail);
        }
    }
}

/// Everything a decision is checked against, read at decision time.
fn decision_context(
    state: &HostState,
    device_id: Uuid,
    session_id: Uuid,
) -> approvals::DecisionContext {
    approvals::DecisionContext {
        device_id,
        grant_version: state.grant_version(device_id),
        runtime_id: state.session_runtime_for(session_id, device_id),
    }
}

/// Map a refused decision onto the outcome the device is told.
fn decision_failure(error: approvals::ConsumeError) -> (OperationState, Option<serde_json::Value>) {
    match error {
        approvals::ConsumeError::UnknownOrConsumed => (OperationState::Unknown, None),
        approvals::ConsumeError::Expired => (OperationState::Expired, None),
        approvals::ConsumeError::DigestMismatch => (
            OperationState::Rejected {
                code: RejectCode::Malformed,
            },
            Some(
                serde_json::json!({ "message": "the action changed after this offer was issued" }),
            ),
        ),
        approvals::ConsumeError::DeviceMismatch => (
            OperationState::Rejected {
                code: RejectCode::AuthFailed,
            },
            Some(serde_json::json!({ "message": "this offer belongs to another device" })),
        ),
        approvals::ConsumeError::GrantMismatch => (
            OperationState::Rejected {
                code: RejectCode::AuthFailed,
            },
            Some(serde_json::json!({ "message": "this device's grant changed; pair again" })),
        ),
        approvals::ConsumeError::RuntimeMismatch => (
            OperationState::Rejected {
                code: RejectCode::UnknownRoute,
            },
            Some(serde_json::json!({ "message": "the session's runtime instance changed" })),
        ),
        approvals::ConsumeError::Invalidated(reason) => (
            OperationState::Rejected {
                code: RejectCode::Malformed,
            },
            Some(serde_json::json!({ "message": reason })),
        ),
        approvals::ConsumeError::InvalidAnswer(reason) => (
            OperationState::Rejected {
                code: RejectCode::Malformed,
            },
            Some(serde_json::json!({ "message": reason })),
        ),
    }
}

/// A snapshot answers with the authoritative session list plus every offer
/// that is still valid for this device, so a reconnecting device sees both
/// the workspace and the turn that is paused waiting for it.
fn run_snapshot(state: &Arc<HostState>, device_id: Uuid) {
    events::push_session_list(state, device_id, true);
    events::push_pending_offers(state, device_id);
    // The device's open views are re-advertised too, so a device that lost
    // its local projection knows which sessions it still holds.
    for session_id in state
        .views()
        .into_iter()
        .filter(|(_, view)| view.device_id == device_id)
        .map(|(session_id, _)| session_id)
        .collect::<Vec<_>>()
    {
        push_queue_view(state, session_id, device_id);
    }
}

/// Answer a result query from the durable record. Nothing is executed: the
/// recorded answer *is* the answer, including `Accepted` for an operation
/// whose execution never finished, which tells the device to verify rather
/// than retry. An id this host never accepted for this device answers
/// `Unknown` — the record's existence is not another device's business. A
/// read's content is never in the record, so a query never returns a body.
fn run_query_result(state: &Arc<HostState>, device_id: Uuid, operation_id: Uuid) {
    let outcome = match state.dedup().lookup(&operation_id.to_string()) {
        Some(record) if record.device_id == device_id.to_string() => OperationOutcome {
            operation_id,
            state: record.state,
            detail: record.detail,
        },
        _ => OperationOutcome {
            operation_id,
            state: OperationState::Unknown,
            detail: None,
        },
    };
    state.send_to_device(
        MessageBody::OperationResult(outcome),
        device_id,
        PendingKind::Result,
    );
}

/// Refuse an operation and record the refusal, so a retry of the same id
/// is answered from the record instead of re-evaluated against a clock
/// that has moved on.
fn refuse_operation(
    state: &Arc<HostState>,
    device_id: Uuid,
    operation: &OperationRequest,
    code: RejectCode,
    message: &str,
) {
    let operation_id = operation.operation_id.to_string();
    let kind = format!("{:?}", operation.kind);
    let persisted = state.dedup().record_accepted(
        &operation_id,
        &kind,
        &operation.summary,
        &device_id.to_string(),
        operation.session_id.map(|id| id.to_string()).as_deref(),
        operation.expires_at_ms,
        prompt_content_hash(&operation).as_deref(),
    );
    if persisted.is_ok() {
        state
            .dedup()
            .record_finished(
                &operation_id,
                &OperationState::Rejected { code },
                Some(message),
            )
            .ok();
    } else {
        log::warn!(target: "audit", "could not persist the refusal for {operation_id}");
    }
    complete_with_detail(
        state,
        device_id,
        operation,
        OperationState::Rejected { code },
        Some(serde_json::json!({ "message": message })),
    );
}

fn complete(
    state: &Arc<HostState>,
    device_id: Uuid,
    operation: &OperationRequest,
    result: OperationState,
    detail: Option<serde_json::Value>,
) {
    complete_by_id(state, device_id, &operation.operation_id, result, detail);
}

fn complete_by_id(
    state: &Arc<HostState>,
    device_id: Uuid,
    operation_id: &Uuid,
    result: OperationState,
    detail: Option<serde_json::Value>,
) {
    let Some(message) = state.seal_envelope(
        MessageBody::OperationResult(OperationOutcome {
            operation_id: *operation_id,
            state: result,
            detail: detail.map(|value| value.to_string()),
        }),
        device_id,
    ) else {
        return;
    };
    state.push_completed(message);
}

fn complete_with_detail(
    state: &Arc<HostState>,
    device_id: Uuid,
    operation: &OperationRequest,
    result: OperationState,
    detail: Option<serde_json::Value>,
) {
    complete(state, device_id, operation, result, detail);
}

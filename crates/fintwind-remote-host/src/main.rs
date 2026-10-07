//! Fintwind remote-control host daemon.
//!
//! One process, three concerns: hold the local identity (signing + Noise
//! keys, auto-registered with the relay on first start — nothing to type
//! anywhere), keep one authenticated loopback connection to the local
//! `fintwind-daemon`, and run the outbound relay session that moves
//! frames between the paired device and the workspace. Operations are
//! deduplicated durably before any side effect; approvals are consumed
//! atomically before the driver hears anything.
//!
//! First start sequence: generate keys → fetch challenge → register →
//! store the assigned host id. If no paired-device grant file exists the
//! host still connects and waits — the correct pre-pairing posture is
//! invisibility, not interactivity.

mod adapter;
mod approvals;
mod config;
mod dedup;
mod grants;
mod identity;
mod outbound;
mod pair;
mod relay_session;
mod state;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::unbounded;
use uuid::Uuid;

use config::HostConfig;
use fintwind_client::DaemonClient;
use fintwind_protocol::{Command, ResponsePayload};
use fintwind_remote_protocol::{
    MessageBody, OperationOutcome, OperationRequest, OperationState, PermissionDecision,
    RejectCode, SealedMessage, UserInputDecision,
};

use grants::GrantFile;
use state::{HostState, SessionCommand};

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
    let mut config_arguments = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            // Operator tooling: write the paired-device grant (phase one is
            // single-device; the file is replaced wholesale) or print this
            // host's identity for the pairing QR. Neither starts the daemon
            // and neither needs the serve-only flags.
            "--provision-device" => {
                provision = Some(
                    arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--provision-device requires \"deviceId,noisePublicKeyB64\""))?,
                )
            }
            "--print-identity" => print_identity = true,
            "--pair" => pair = true,
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
    run_daemon(config)
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
    // nothing trusted yet; the daemon still runs and waits.
    let grants = GrantFile::load(&config.paired_device_file)?.unwrap_or(GrantFile {
        grants: Vec::new(),
    });
    log::info!(target: "audit", "grants loaded; count={}", grants.grants.len());

    let dedup = dedup::OperationStore::open(&config.data_dir.join("operations.sqlite3"))?;
    let (work_tx, work_rx) = unbounded::<SessionCommand>();
    let (pending_tx, pending_rx) = unbounded::<SealedMessage>();
    drop(pending_tx); // No event pump yet; the channel exists for it (M3).

    let state = Arc::new(state::HostState::new(
        config.clone(),
        identity,
        grants,
        dedup,
    ));

    // Local daemon reconnect loop: a degraded workspace is reported
    // honestly to the device, never guessed around.
    {
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("fintwind-daemon-link".into())
            .spawn(move || loop {
                if state.shutdown_requested() {
                    return;
                }
                if state.daemon().is_none() {
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
                            log::info!(target: "audit", "daemon connected");
                            state.set_daemon(Some(Arc::new(client)));
                        }
                        Err(error) => {
                            log::warn!(target: "audit", "daemon connect failed: {error}");
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(3));
            })?;
    }

    // The single worker: serial operation execution keeps ordering, and
    // daemon RPC latency never stalls the relay session loop.
    {
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("fintwind-remote-worker".into())
            .spawn(move || run_worker(state, work_rx))?;
    }

    let links = Arc::new(relay_session::SessionLinks {
        state: Arc::clone(&state),
        pending_outbound: pending_rx,
        work_tx,
    });
    relay_session::run_forever(links);
    Ok(())
}

fn run_worker(state: Arc<HostState>, work_rx: crossbeam_channel::Receiver<SessionCommand>) {
    let mut catalog = adapter::RuntimeCatalog::default();
    loop {
        if state.shutdown_requested() {
            return;
        }
        match work_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(SessionCommand::Operation(operation)) => {
                run_operation(&state, &mut catalog, operation);
            }
            Ok(SessionCommand::PermissionDecision(decision)) => {
                run_permission_decision(&state, decision);
            }
            Ok(SessionCommand::UserInputDecision(decision)) => {
                run_user_input_decision(&state, decision);
            }
            Ok(SessionCommand::Snapshot) => run_snapshot(&state),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                state.approvals().sweep();
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn device_id_of(state: &HostState) -> String {
    // Phase one is single-device: the sole active grant is the
    // correspondent. Multi-device addressing lands with pairing (M3).
    state
        .grants_for_device()
        .unwrap_or_else(|| "unknown".to_owned())
}

fn run_operation(state: &Arc<HostState>, catalog: &mut adapter::RuntimeCatalog, operation: OperationRequest) {
    let device_id = device_id_of(&state);
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
        &device_id,
        expires_at_ms,
    ) {
        Ok(check) => check,
        Err(error) => {
            log::warn!(target: "audit", "dedup store error: {error}");
            complete(state, &device_id, &operation, OperationState::Unknown, None);
            return;
        }
    };
    if check.replay {
        complete(state, &device_id, &operation, check.state, None);
        return;
    }
    if let OperationState::Expired = check.state {
        complete(state, &device_id, &operation, OperationState::Expired, None);
        return;
    }

    // Accept durably **before** any side effect.
    if let Err(error) = state.dedup().record_accepted(
        &operation_id,
        &kind,
        &summary,
        &device_id,
        operation.session_id.map(|id| id.to_string()).as_deref(),
        expires_at_ms,
    ) {
        log::warn!(target: "audit", "could not persist acceptance: {error}");
        complete(
            state,
            &device_id,
            &operation,
            OperationState::Unknown,
            Some(serde_json::json!("acceptance not persisted")),
        );
        return;
    }

    let Some(daemon) = state.daemon() else {
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
        complete(
            state,
            &device_id,
            &operation,
            OperationState::Rejected {
                code: RejectCode::UnknownRoute,
            },
            Some(serde_json::json!("workspace unreachable")),
        );
        return;
    };

    let outcome = adapter::execute(&daemon, catalog, &operation);
    match outcome {
        Ok(detail) => {
            state
                .dedup()
                .record_finished(&operation_id, &OperationState::Executed, None)
                .ok();
            complete_with_detail(state, &device_id, &operation, OperationState::Executed, Some(detail));
        }
        Err((code, message)) => {
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
                &device_id,
                &operation,
                OperationState::Rejected { code },
                Some(serde_json::json!({ "message": message })),
            );
        }
    }
}

fn run_permission_decision(state: &Arc<HostState>, decision: PermissionDecision) {
    let device_id = device_id_of(&state);
    // The live digest check lands with the driver-event pipeline (M3);
    // consumption itself is already atomic.
    match state
        .approvals()
        .consume(&decision.approval_id.to_string(), &decision.option_id, None)
    {
        Ok((offer, option_id)) => {
            let outcome = state.daemon().ok_or_else(|| anyhow::anyhow!("workspace unreachable"))
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
            complete_by_id(state, &device_id, &decision.approval_id, op_state, detail);
        }
        Err(error) => {
            let op_state = match error {
                approvals::ConsumeError::Expired => OperationState::Expired,
                approvals::ConsumeError::UnknownOrConsumed => OperationState::Unknown,
                approvals::ConsumeError::DigestMismatch => OperationState::Rejected {
                    code: RejectCode::Malformed,
                },
            };
            complete_by_id(state, &device_id, &decision.approval_id, op_state, None);
        }
    }
}

fn run_user_input_decision(state: &Arc<HostState>, decision: UserInputDecision) {
    let device_id = device_id_of(&state);
    match state
        .approvals()
        .consume(&decision.approval_id.to_string(), "answers", None)
    {
        Ok((offer, _)) => {
            // Remote answers carry selected ids plus optional custom text;
            // the local shape is a flat string list per question.
            let mut answers_by_question: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            for answer in &decision.answers {
                let entry = answers_by_question
                    .entry(answer.question_id.clone())
                    .or_default();
                entry.extend(answer.selected_option_ids.iter().cloned());
                if let Some(custom) = &answer.custom_text {
                    if !custom.is_empty() {
                        entry.push(custom.clone());
                    }
                }
            }
            let answers: Vec<fintwind_protocol::model::UserInputAnswer> = answers_by_question
                .into_iter()
                .map(|(question_id, answers)| fintwind_protocol::model::UserInputAnswer {
                    question_id,
                    answers,
                })
                .collect();
            let outcome = state.daemon().ok_or_else(|| anyhow::anyhow!("workspace unreachable"))
                .and_then(|daemon| {
                    daemon.request(
                        offer.session_id,
                        offer.runtime_id,
                        Command::RespondUserInput {
                            request_id: offer.request_ref.clone(),
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
            complete_by_id(state, &device_id, &decision.approval_id, op_state, detail);
        }
        Err(error) => {
            let op_state = match error {
                approvals::ConsumeError::Expired => OperationState::Expired,
                _ => OperationState::Unknown,
            };
            complete_by_id(state, &device_id, &decision.approval_id, op_state, None);
        }
    }
}

fn run_snapshot(state: &Arc<HostState>) {
    let device_id = device_id_of(&state);
    let Some(daemon) = state.daemon() else {
        return;
    };
    let Ok(ResponsePayload::TaskState { projects, sessions, .. }) =
        daemon.request(Uuid::nil(), Uuid::nil(), Command::LoadTaskState)
    else {
        return;
    };
    let summaries: Vec<fintwind_remote_protocol::SessionSummary> = sessions
        .iter()
        .map(|session| fintwind_remote_protocol::SessionSummary {
            session_id: session.id,
            project_id: Some(session.project_id),
            title: session.title.clone(),
            status: serde_json::to_value(&session.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            updated_at_ms: session.updated_at,
            // Archival state lives outside AgentSession; it surfaces in
            // the snapshot when the control-plane milestone wires it in.
            archived: false,
        })
        .collect();
    let message = state.seal_envelope(
        MessageBody::SessionList {
            revision: now_ms(),
            sessions: summaries,
        },
        &device_id,
    );
    state.push_completed(message);
    let _ = projects;
}

fn complete(
    state: &Arc<HostState>,
    device_id: &str,
    operation: &OperationRequest,
    result: OperationState,
    detail: Option<serde_json::Value>,
) {
    complete_by_id(state, device_id, &operation.operation_id, result, detail);
}

fn complete_by_id(
    state: &Arc<HostState>,
    device_id: &str,
    operation_id: &Uuid,
    result: OperationState,
    detail: Option<serde_json::Value>,
) {
    let message = state.seal_envelope(
        MessageBody::OperationResult(OperationOutcome {
            operation_id: *operation_id,
            state: result,
            detail: detail.map(|value| value.to_string()),
        }),
        device_id,
    );
    state.push_completed(message);
}

fn complete_with_detail(
    state: &Arc<HostState>,
    device_id: &str,
    operation: &OperationRequest,
    result: OperationState,
    detail: Option<serde_json::Value>,
) {
    complete(state, device_id, operation, result, detail);
}

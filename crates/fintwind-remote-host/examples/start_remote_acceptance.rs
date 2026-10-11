//! Drive the frozen remote-control daemon contract end to end against one
//! real daemon and the synthetic provider.
//!
//! Every step speaks the interface frozen in
//! `docs/remote-control-backend-interface.md`: `ProbeProvider` through the
//! daemon's own trusted path (no binary override), `CreateRemoteTask`,
//! `AdmitRemotePrompt`, `DispatchRemotePrompt`, `CancelRemotePrompt`,
//! `ListRemotePrompts` and `ArchiveRemoteSession`. The interactive wire —
//! a permission ask answered through `Respond`, a question set answered
//! through `RespondUserInput` (single-select, multi-select, custom free
//! text) and the turn's settlement — is the real driver path, observed on
//! the daemon's event stream.
//!
//! Guardrails:
//!
//! * every command carries an explicit timeout, and the artifact is
//!   rewritten after every phase — a hung or killed run still names the
//!   exact command it was waiting on;
//! * `ResponsePayload::Error` is never a success: each command must decode
//!   into its own authoritative variant;
//! * queue states are compared against `RemotePromptState`, never against
//!   "some reply arrived";
//! * a contract step that fails softly (dedup, second-prompt cancel) is
//!   recorded and the run continues, so later phases still produce their
//!   own evidence; the core chain (probe, create, admit, dispatch, the
//!   interactive wire) fails hard.
//!
//! Usage:
//!   start_remote_acceptance --daemon-address 127.0.0.1:PORT \
//!     --daemon-token TOKEN --workspace <synthetic dir> \
//!     --project-id <seeded uuid> --out <artifact.json>

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context as _};
use uuid::Uuid;

use fintwind_client::{Command, DaemonClient, ResponsePayload};
use fintwind_protocol::submission::{RemotePromptState, RemoteProvenance};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

fn take_flag(arguments: &mut Vec<String>, name: &str) -> Option<String> {
    let position = arguments.iter().position(|argument| argument == name)?;
    let value = arguments.get(position + 1).cloned();
    arguments.remove(position);
    if let Some(value) = value.as_ref() {
        if let Some(index) = arguments.iter().position(|argument| argument == value) {
            arguments.remove(index);
        }
    }
    value
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn main() -> anyhow::Result<()> {
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    let queue_phase = take_flag(&mut arguments, "--queue-phase");
    let queue_fixture = take_flag(&mut arguments, "--queue-fixture").map(PathBuf::from);
    let daemon_address = take_flag(&mut arguments, "--daemon-address")
        .ok_or_else(|| anyhow::anyhow!("--daemon-address is required"))?;
    let daemon_token = take_flag(&mut arguments, "--daemon-token")
        .ok_or_else(|| anyhow::anyhow!("--daemon-token is required"))?;
    let workspace = take_flag(&mut arguments, "--workspace")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("--workspace is required"))?;
    let project_id: Uuid = take_flag(&mut arguments, "--project-id")
        .ok_or_else(|| anyhow::anyhow!("--project-id is required"))?
        .parse()
        .context("--project-id is not a uuid")?;
    let expect_provider_dir = take_flag(&mut arguments, "--expect-provider-dir").map(PathBuf::from);
    let prompt = take_flag(&mut arguments, "--prompt")
        .unwrap_or_else(|| "synthetic acceptance prompt".to_owned());
    let out = take_flag(&mut arguments, "--out")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("--out is required"))?;
    if let Some(phase) = queue_phase {
        if !workspace.is_absolute() || !workspace.join(".fintwind-e2e-fixture").is_file() {
            bail!("queue recovery requires a marked isolated workspace");
        }
        let fixture = queue_fixture.ok_or_else(|| anyhow::anyhow!("--queue-fixture is required"))?;
        let client = connect_with_retry(&daemon_address, &daemon_token, 30)?;
        return queue_recovery_phase(&client, project_id, &phase, &fixture, &out);
    }

    // Direct-daemon acceptance: no host relay is involved, so the
    // provenance is a synthetic stand-in and is labeled as such. A real
    // host fills these from the authenticated device's grant.
    let provenance = RemoteProvenance {
        host_id: Uuid::new_v4(),
        device_id: Uuid::new_v4(),
        grant_version: 1,
    };

    let mut artifact = serde_json::json!({
        "phases": [],
        "probeOk": false,
        "probeInstalled": false,
        "probePathIsFixture": false,
        "remoteTaskCreated": false,
        "admitQueued": false,
        "admitIdempotent": null,
        "secondPromptCancelled": null,
        "listMatched": false,
        "operationIdsUnique": false,
        "dispatched": false,
        "dispatchState": null,
        "permissionReplied": false,
        "questionReplied": false,
        "questionAnswerKinds": [],
        "turnSettled": false,
        "archiveAcked": false,
        "eventKinds": {},
        "provenanceSynthetic": true,
        "knownGaps": [],
        "passed": false,
    });
    let persist = |artifact: &serde_json::Value| {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&out, serde_json::to_string_pretty(artifact).unwrap_or_default()).ok();
    };
    let mut record_phase = |artifact: &mut serde_json::Value, phase: &str| {
        artifact["phases"]
            .as_array_mut()
            .expect("phases array")
            .push(serde_json::json!(phase));
        persist(artifact);
    };

    let client = connect_with_retry(&daemon_address, &daemon_token, 30)?;
    record_phase(&mut artifact, "daemon-connected");

    let note_gap = |artifact: &mut serde_json::Value, gap: &str| {
        artifact["knownGaps"]
            .as_array_mut()
            .expect("gaps array")
            .push(serde_json::json!(gap));
        persist(artifact);
    };

    // ---- the daemon's trusted probe path ---------------------------------
    let probe = client.request_with_timeout(
        Uuid::nil(),
        Uuid::nil(),
        Command::ProbeProvider {
            binary_override: None,
            directory: Some(workspace.clone()),
            discover_models: false,
            probe_version: true,
        },
        COMMAND_TIMEOUT,
    );
    match probe {
        Ok(ResponsePayload::ProviderProbe { probe, .. }) => {
            artifact["probeInstalled"] = serde_json::json!(probe.installed);
            // The resolved path is the adoption evidence: inside the
            // harness's isolated provider directory means the daemon's
            // trusted resolution picked the fixture, never a real install.
            match (&probe.path, &expect_provider_dir) {
                (Some(path), Some(dir)) => {
                    artifact["probePathIsFixture"] = serde_json::json!(path.starts_with(dir));
                }
                _ => artifact["probePathIsFixture"] = serde_json::json!(false),
            }
            if !probe.installed {
                bail!("the daemon's trusted provider probe found no provider binary");
            }
        }
        Ok(other) => bail!("ProbeProvider answered with an unexpected payload: {other:?}"),
        Err(error) => bail!("ProbeProvider failed: {error:#}"),
    }
    artifact["probeOk"] = serde_json::json!(true);
    record_phase(&mut artifact, "provider-probed");

    // ---- one remote task, the daemon's own defaults ----------------------
    let session_id = match client.request_with_timeout(
        Uuid::nil(),
        Uuid::nil(),
        Command::CreateRemoteTask {
            provenance: provenance.clone(),
            project_id,
        },
        COMMAND_TIMEOUT,
    ) {
        Ok(ResponsePayload::RemoteTaskCreated { session_id }) => session_id,
        Ok(other) => bail!("CreateRemoteTask answered with an unexpected payload: {other:?}"),
        Err(error) => bail!("CreateRemoteTask failed: {error:#}"),
    };
    artifact["remoteTaskCreated"] = serde_json::json!(true);
    record_phase(&mut artifact, "remote-task-created");
    // This isolated interactive scenario must ask, rather than exercising
    // the computer's default auto-approval policy and counting it as a reply.
    match client.request_with_timeout(session_id, Uuid::nil(), Command::SetRemoteSessionOptions {
        provenance: provenance.clone(),
        session_id,
        agent_preset: None,
        options: fintwind_protocol::WireSessionOptions {
            mode: "ask".to_owned(), interaction_mode: "build".to_owned(),
            model: None, reasoning_effort: None, service_tier: None, context_window: None,
        },
    }, COMMAND_TIMEOUT)? {
        ResponsePayload::OptionsApplied { .. } => record_phase(&mut artifact, "approval-policy-set"),
        _ => bail!("the isolated approval policy was not recorded"),
    }

    // ---- admit: one real prompt, one retry, one to cancel ----------------
    let operation_id = Uuid::new_v4();
    let second_operation_id = Uuid::new_v4();
    let expires_at_ms = now_ms() + 10 * 60_000;
    let mut admit = |artifact: &mut serde_json::Value, operation: Uuid| -> anyhow::Result<Uuid> {
        match client.request_with_timeout(
            session_id,
            Uuid::nil(),
            Command::AdmitRemotePrompt {
                provenance: provenance.clone(),
                session_id,
                operation_id: operation,
                prompt: prompt.clone(),
                expires_at_ms,
                summary: "synthetic acceptance turn".to_owned(),
            },
            COMMAND_TIMEOUT,
        ) {
            Ok(ResponsePayload::RemotePromptAdmitted {
                operation_id: echoed,
                submission_id,
                state,
            }) | Ok(ResponsePayload::RemotePromptState {
                operation_id: echoed,
                submission_id,
                state,
            }) => {
                if echoed != operation {
                    bail!("AdmitRemotePrompt echoed a different operation id");
                }
                if state != RemotePromptState::Queued.label() {
                    bail!("the admitted prompt is {state:?}, not queued");
                }
                Ok(submission_id)
            }
            Ok(other) => bail!("AdmitRemotePrompt answered with an unexpected payload: {other:?}"),
            Err(error) => bail!("AdmitRemotePrompt failed: {error:#}"),
        }
    };
    let submission_id = admit(&mut artifact, operation_id)?;
    artifact["admitQueued"] = serde_json::json!(true);
    record_phase(&mut artifact, "admit-queued");

    // The same operation id must land on the same durable submission, never
    // on a second one. A daemon that does not dedup yet is recorded as a
    // gap; the rest of the chain still produces its own evidence.
    match admit(&mut artifact, operation_id) {
        Ok(again) => {
            artifact["admitIdempotent"] = serde_json::json!(again == submission_id);
        }
        Err(_) => {
            artifact["admitIdempotent"] = serde_json::json!(false);
        }
    }
    if artifact["admitIdempotent"].as_bool() != Some(true) {
        note_gap(&mut artifact, "a retried admit did not dedup onto the same submission");
    }
    record_phase(&mut artifact, "admit-dedup-checked");

    let second_admitted = admit(&mut artifact, second_operation_id).is_ok();
    let cancel_id = Uuid::new_v4();
    if second_admitted {
        match client.request_with_timeout(
            session_id,
            Uuid::nil(),
            Command::CancelRemotePrompt {
                provenance: provenance.clone(),
                operation_id: second_operation_id,
                cancel_operation_id: Some(cancel_id),
            },
            COMMAND_TIMEOUT,
        ) {
            Ok(ResponsePayload::RemotePromptState { state, .. }) => {
                artifact["secondPromptCancelled"] = serde_json::json!(
                    state == RemotePromptState::Cancelled.label()
                );
            }
            Ok(other) => {
                bail!("CancelRemotePrompt answered with an unexpected payload: {other:?}")
            }
            Err(error) => bail!("CancelRemotePrompt failed: {error:#}"),
        }
    } else {
        artifact["secondPromptCancelled"] = serde_json::json!(false);
        note_gap(&mut artifact, "the second admit was rejected; cancel was not exercised");
    }
    record_phase(&mut artifact, "second-prompt-cancelled");
    // Exact cancellation identity, tombstone lookup and cross-device
    // isolation are checked through public IPC, not internal store helpers.
    let query_cancel = |who: RemoteProvenance, target: Uuid, cancel: Uuid| {
        client.request_with_timeout(session_id, Uuid::nil(), Command::QueryRemoteCancel {
            provenance: who, operation_id: target, cancel_operation_id: Some(cancel),
        }, COMMAND_TIMEOUT)
    };
    match query_cancel(provenance, second_operation_id, cancel_id)? {
        ResponsePayload::RemoteCancelState { cancel_operation_id: Some(id), cancel_state, host_id: Some(host), device_id: Some(device), grant_version: Some(1), .. }
            if id == cancel_id && cancel_state == "cancelled" && host == provenance.host_id && device == provenance.device_id => {}
        _ => bail!("exact cancellation did not return its own durable ending"),
    }
    match query_cancel(provenance, second_operation_id, Uuid::new_v4())? {
        ResponsePayload::RemoteCancelState { cancel_state, .. } if cancel_state == "unknown" => {}
        _ => bail!("an unknown cancellation borrowed another cancellation's result"),
    }
    match query_cancel(provenance, operation_id, cancel_id)? {
        ResponsePayload::RemoteCancelState { cancel_state, .. } if cancel_state == "unknown" => {}
        _ => bail!("a cancellation ending leaked to a different target prompt"),
    }
    let stranger = RemoteProvenance { device_id: Uuid::new_v4(), ..provenance };
    if matches!(query_cancel(stranger, second_operation_id, cancel_id), Ok(ResponsePayload::RemoteCancelState { cancel_state, .. }) if cancel_state == "cancelled") {
        bail!("another device could read a cancellation's success");
    }
    artifact["exactCancelIdentityVerified"] = serde_json::json!(true);
    record_phase(&mut artifact, "exact-cancel-identity-verified");

    // ---- the queue view ---------------------------------------------------
    match client.request_with_timeout(
        session_id,
        Uuid::nil(),
        Command::ListRemotePrompts {
            provenance: provenance.clone(),
        },
        COMMAND_TIMEOUT,
    ) {
        Ok(ResponsePayload::RemotePrompts { prompts }) => {
            let mut operations: Vec<Uuid> = prompts.iter().map(|row| row.operation_id).collect();
            operations.sort();
            operations.dedup();
            artifact["operationIdsUnique"] =
                serde_json::json!(operations.len() == prompts.len());
            artifact["listMatched"] = serde_json::json!(prompts
                .iter()
                .any(|row| row.operation_id == operation_id));
        }
        Ok(other) => bail!("ListRemotePrompts answered with an unexpected payload: {other:?}"),
        Err(error) => bail!("ListRemotePrompts failed: {error:#}"),
    }
    record_phase(&mut artifact, "list-verified");

    // ---- dispatch, then the real interactive wire ------------------------
    match client.request_with_timeout(
        session_id,
        Uuid::nil(),
        Command::DispatchRemotePrompt {
            provenance: provenance.clone(),
            operation_id,
        },
        COMMAND_TIMEOUT,
    ) {
        Ok(ResponsePayload::RemotePromptState { state, .. }) => {
            artifact["dispatched"] = serde_json::json!(matches!(
                RemotePromptState::parse(&state),
                Some(RemotePromptState::Dispatching | RemotePromptState::Dispatched)
            ));
            artifact["dispatchState"] = serde_json::json!(state);
            persist(&artifact);
        }
        Ok(_) => {
            artifact["failedCommand"] = serde_json::json!("dispatch");
            artifact["failureClass"] = serde_json::json!("unexpected-response");
            persist(&artifact);
            bail!("DispatchRemotePrompt answered with an unexpected payload");
        }
        Err(error) => {
            let diagnostic = format!("{error:#}");
            let class = ["timeout", "timed out", "busy", "locked", "no such", "NOT NULL", "FOREIGN KEY", "UNIQUE", "another device", "missing", "unavailable", "refused", "disconnected", "cancel", "syntax"]
                .into_iter().find(|label| diagnostic.to_lowercase().contains(&label.to_lowercase())).unwrap_or("unclassified");
            artifact["failedCommand"] = serde_json::json!("dispatch");
            artifact["failureClass"] = serde_json::json!(class);
            artifact["failureStage"] = serde_json::json!(["remote-dispatch-claim", "remote-dispatch-target", "remote-dispatch-materialize", "remote-dispatch-commit", "remote-dispatch-cancel-settlement"]
                .into_iter().find(|label| diagnostic.contains(label)).unwrap_or("before-claim-or-unclassified"));
            persist(&artifact);
            bail!("DispatchRemotePrompt failed ({class})");
        }
    }
    if artifact["dispatched"].as_bool() != Some(true) {
        bail!("the dispatched prompt reported neither dispatching nor dispatched");
    }
    record_phase(&mut artifact, "dispatched");

    // The runtime is the daemon's own; attach to learn its id, then watch.
    let runtime_id = match client.request_with_timeout(
        session_id,
        Uuid::nil(),
        Command::AttachSession,
        COMMAND_TIMEOUT,
    ) {
        Ok(ResponsePayload::SessionRuntime { runtime_id, .. }) => {
            runtime_id.ok_or_else(|| anyhow::anyhow!("the dispatched session has no runtime"))?
        }
        Ok(other) => bail!("AttachSession answered with an unexpected payload: {other:?}"),
        Err(error) => bail!("AttachSession failed: {error:#}"),
    };
    let events = client.subscribe(session_id, runtime_id);
    record_phase(&mut artifact, "attached");

    let deadline = std::time::Instant::now() + Duration::from_secs(
        std::env::var("FINTWIND_ACCEPTANCE_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(60),
    );
    while std::time::Instant::now() < deadline {
        let remaining = deadline - std::time::Instant::now();
        let event = match events.recv_timeout(remaining.min(Duration::from_secs(5))) {
            Ok(event) => event,
            Err(_) => {
                if interactive_done(&artifact) && artifact["turnSettled"].as_bool() == Some(true) {
                    break;
                }
                continue;
            }
        };
        count_kind(&mut artifact, &event);
        persist(&artifact);
        match event.event.kind.as_str() {
            "permission" => {
                if artifact["permissionReplied"].as_bool() == Some(true) {
                    continue;
                }
                let request_id = text_field(&event.event.payload, "requestId");
                let option_id = event
                    .event
                    .payload
                    .get("options")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|options| options.first())
                    .and_then(|option| option.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("once")
                    .to_owned();
                if let Err(error) = client.request_with_timeout(
                    session_id,
                    runtime_id,
                    Command::Respond {
                        request_id,
                        option_id,
                    },
                    COMMAND_TIMEOUT,
                ) {
                    bail!("Respond failed: {error:#}");
                }
                artifact["permissionReplied"] = serde_json::json!(true);
                record_phase(&mut artifact, "permission-answered");
            }
            "userInputRequested" => {
                if artifact["questionReplied"].as_bool() == Some(true) {
                    continue;
                }
                let request_id = text_field(&event.event.payload, "requestId");
                let questions = event
                    .event
                    .payload
                    .get("questions")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut answers = Vec::new();
                let mut kinds = Vec::new();
                for question in &questions {
                    let question_id = question
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let first_label = question
                        .get("options")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|options| options.first())
                        .and_then(|option| option.get("label"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let multi = question
                        .get("multiSelect")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    if multi {
                        kinds.push("multi+custom");
                        answers.push(fintwind_protocol::model::UserInputAnswer {
                            question_id,
                            answers: vec![first_label, "synthetic custom answer".to_owned()],
                        });
                    } else {
                        kinds.push("single");
                        answers.push(fintwind_protocol::model::UserInputAnswer {
                            question_id,
                            answers: vec![first_label],
                        });
                    }
                }
                if let Err(error) = client.request_with_timeout(
                    session_id,
                    runtime_id,
                    Command::RespondUserInput {
                        request_id,
                        answers,
                    },
                    COMMAND_TIMEOUT,
                ) {
                    bail!("RespondUserInput failed: {error:#}");
                }
                artifact["questionAnswerKinds"] = serde_json::json!(kinds);
                artifact["questionReplied"] = serde_json::json!(true);
                record_phase(&mut artifact, "question-answered");
            }
            "turnFinished" => {
                artifact["turnSettled"] = serde_json::json!(
                    event
                        .event
                        .payload
                        .get("success")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                );
                record_phase(&mut artifact, "turn-settled");
                break;
            }
            _ => {}
        }
        if interactive_done(&artifact) && artifact["turnSettled"].as_bool() == Some(true) {
            break;
        }
    }
    persist(&mut artifact);

    if artifact["permissionReplied"].as_bool() != Some(true)
        || artifact["questionReplied"].as_bool() != Some(true)
    {
        bail!("the interactive wire did not complete");
    }
    if artifact["turnSettled"].as_bool() != Some(true) {
        bail!("the real turn completion boundary was not observed");
    }
    let kinds = artifact["questionAnswerKinds"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let kind_text: Vec<&str> = kinds.iter().filter_map(serde_json::Value::as_str).collect();
    if !kind_text.contains(&"single")
        || !kind_text.iter().any(|kind| kind.contains("multi"))
        || !kind_text.iter().any(|kind| kind.contains("custom"))
    {
        bail!("not every question answer shape was exercised");
    }

    // ---- tidy: archive the task out of the active list -------------------
    match client.request_with_timeout(
        session_id,
        Uuid::nil(),
        Command::ArchiveRemoteSession {
            provenance: provenance.clone(),
            session_id,
        },
        COMMAND_TIMEOUT,
    ) {
        Ok(ResponsePayload::Ack) => artifact["archiveAcked"] = serde_json::json!(true),
        Ok(other) => bail!("ArchiveRemoteSession answered with an unexpected payload: {other:?}"),
        Err(error) => bail!("ArchiveRemoteSession failed: {error:#}"),
    }
    record_phase(&mut artifact, "archived");

    artifact["passed"] = serde_json::json!(artifact["archiveAcked"].as_bool() == Some(true));
    persist(&mut artifact);
    if artifact["turnSettled"].as_bool() == Some(true) {
        println!("[OK] the remote chain ran the interactive wire and the turn settled.");
    } else {
        println!(
            "[OK] the remote chain ran the interactive wire; turn settlement is still not reported (see turnSettled)."
        );
    }
    println!("[ARTIFACT] {}", out.display());
    Ok(())
}

/// Failure matrix for the restart black box (written before implementation):
/// a queued item disappears, changes identity or auto-dispatches on startup;
/// a completed cancel loses its exact ending; another cancel/target/device/
/// grant borrows it; a query mutates the queue; a cancelled item resurrects.
/// Three fresh daemon processes use the same isolated DB. No provider starts.
fn queue_recovery_phase(client: &DaemonClient, project: Uuid, phase: &str, fixture: &std::path::Path, out: &std::path::Path) -> anyhow::Result<()> {
    use serde::{Deserialize, Serialize};
    #[derive(Serialize, Deserialize)]
    struct Fixture {
        synthetic: bool, provenance: RemoteProvenance, session: Uuid,
        queued: Uuid, queued_submission: Uuid, cancelled: Uuid,
        cancel: Uuid, recovery_cancel: Uuid,
    }
    let call = |command| client.request_with_timeout(Uuid::nil(), Uuid::nil(), command, COMMAND_TIMEOUT);
    let data = if phase == "prepare" {
        if fixture.exists() { bail!("recovery fixture must not already exist"); }
        let provenance = RemoteProvenance { host_id: Uuid::new_v4(), device_id: Uuid::new_v4(), grant_version: 1 };
        let ResponsePayload::RemoteTaskCreated { session_id } = call(Command::CreateRemoteTask { provenance, project_id: project })? else { bail!("task creation failed"); };
        let queued = Uuid::new_v4();
        let cancelled = Uuid::new_v4();
        let admit = |operation_id| call(Command::AdmitRemotePrompt {
            provenance, session_id, operation_id, prompt: "isolated restart prompt".to_owned(),
            expires_at_ms: now_ms() + 600_000, summary: "restart fixture".to_owned(),
        });
        let ResponsePayload::RemotePromptAdmitted { submission_id, state, .. } = admit(queued)? else { bail!("queue admission failed"); };
        if state != "queued" { bail!("new item was not queued"); }
        if !matches!(admit(cancelled)?, ResponsePayload::RemotePromptAdmitted { .. }) { bail!("cancel target admission failed"); }
        let cancel = Uuid::new_v4();
        if !matches!(call(Command::CancelRemotePrompt { provenance, operation_id: cancelled, cancel_operation_id: Some(cancel) })?, ResponsePayload::RemotePromptState { state, .. } if state == "cancelled") { bail!("initial cancellation failed"); }
        let data = Fixture { synthetic: true, provenance, session: session_id, queued, queued_submission: submission_id, cancelled, cancel, recovery_cancel: Uuid::new_v4() };
        std::fs::write(fixture, serde_json::to_vec_pretty(&data)?)?;
        data
    } else if matches!(phase, "recover" | "verify") {
        serde_json::from_slice::<Fixture>(&std::fs::read(fixture)?)?
    } else { bail!("unknown queue recovery phase"); };
    if !data.synthetic { bail!("not a synthetic recovery fixture"); }
    let list = || -> anyhow::Result<Vec<fintwind_protocol::submission::RemotePromptRecord>> {
        let ResponsePayload::RemotePrompts { prompts } = call(Command::ListRemotePrompts { provenance: data.provenance })? else { bail!("list response mismatch"); };
        Ok(prompts)
    };
    let before = list()?;
    if before.iter().any(|row| row.operation_id == data.cancelled) { bail!("cancelled prompt resurrected"); }
    let expected = phase != "verify";
    let queued = before.iter().find(|row| row.operation_id == data.queued);
    if expected {
        let row = queued.ok_or_else(|| anyhow::anyhow!("queued prompt lost across restart"))?;
        if row.submission_id != data.queued_submission || row.session_id != data.session || row.state != "queued" { bail!("queued identity or state changed across restart"); }
    } else if queued.is_some() { bail!("recovered cancellation resurrected"); }
    let query = |provenance, operation_id, cancel_operation_id| call(Command::QueryRemoteCancel { provenance, operation_id, cancel_operation_id: Some(cancel_operation_id) });
    let exact = |target, cancel| -> anyhow::Result<()> {
        match query(data.provenance, target, cancel)? {
            ResponsePayload::RemoteCancelState { cancel_operation_id: Some(id), cancel_state, host_id: Some(host), device_id: Some(device), grant_version: Some(version), .. }
                if id == cancel && cancel_state == "cancelled" && host == data.provenance.host_id && device == data.provenance.device_id && version == 1 => Ok(()),
            _ => bail!("exact cancel ending lost across restart"),
        }
    };
    exact(data.cancelled, data.cancel)?;
    for (who, target, cancel) in [
        (data.provenance, data.cancelled, Uuid::new_v4()),
        (data.provenance, Uuid::new_v4(), data.cancel),
        (RemoteProvenance { device_id: Uuid::new_v4(), ..data.provenance }, data.cancelled, data.cancel),
        (RemoteProvenance { grant_version: 2, ..data.provenance }, data.cancelled, data.cancel),
    ] {
        if !matches!(query(who, target, cancel)?, ResponsePayload::RemoteCancelState { cancel_state, .. } if cancel_state == "unknown") { bail!("cancel result crossed its identity boundary"); }
    }
    if serde_json::to_vec(&before)? != serde_json::to_vec(&list()?)? { bail!("cancel query mutated the queue"); }
    if phase == "recover" {
        if !matches!(call(Command::CancelRemotePrompt { provenance: data.provenance, operation_id: data.queued, cancel_operation_id: Some(data.recovery_cancel) })?, ResponsePayload::RemotePromptState { state, .. } if state == "cancelled") { bail!("recovered queue item could not be cancelled"); }
        exact(data.queued, data.recovery_cancel)?;
    } else if phase == "verify" {
        exact(data.queued, data.recovery_cancel)?;
    }
    std::fs::write(out, serde_json::to_vec_pretty(&serde_json::json!({
        "synthetic": true, "phase": phase, "passed": true,
        "exactCancelIdentityVerified": true, "queryReadOnly": true,
        "queuedAfterRestart": phase == "recover", "cancelledAfterRestart": phase == "verify",
        "providerStarted": false, "fullPlanVerified": false,
    }))?)?;
    Ok(())
}

fn interactive_done(artifact: &serde_json::Value) -> bool {
    artifact["permissionReplied"].as_bool() == Some(true)
        && artifact["questionReplied"].as_bool() == Some(true)
}

fn text_field(payload: &serde_json::Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn count_kind(artifact: &mut serde_json::Value, event: &fintwind_protocol::SequencedEvent) {
    let kinds = artifact["eventKinds"].as_object_mut().expect("kinds map");
    let entry = kinds
        .entry(event.event.kind.clone())
        .or_insert(serde_json::json!(0));
    let count = entry.as_u64().unwrap_or(0);
    *entry = serde_json::json!(count + 1);
}

/// Connect to the daemon, retrying inside the deadline: the harness starts
/// the daemon and this example in parallel.
fn connect_with_retry(address: &str, token: &str, seconds: u64) -> anyhow::Result<DaemonClient> {
    let deadline = std::time::Instant::now() + Duration::from_secs(seconds);
    let mut last_error = None;
    while std::time::Instant::now() < deadline {
        match DaemonClient::connect(address, token.to_owned()) {
            Ok(client) => return Ok(client),
            Err(error) => {
                last_error = Some(error);
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    bail!(
        "could not connect to the daemon at {address}: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "timed out".to_owned())
    )
}

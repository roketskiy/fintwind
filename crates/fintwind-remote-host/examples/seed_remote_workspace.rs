//! Seed a real, explicitly isolated daemon with a synthetic project and task.
//! No provider is started and no model/daemon credential is printed. Failure
//! modes: accidental user directory, missing isolation marker, or mismatched
//! task-state reply. All fail closed before the seed request.
//! Ask-policy configuration refuses an unmarked root, a missing/started
//! task, a foreign project, or a missing write confirmation. It is a local
//! acceptance-data operation, never a phone or production approval bypass.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use fintwind_client::DaemonClient;
use fintwind_protocol::model::{AgentSession, Project};
use fintwind_protocol::{Command, ResponsePayload};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    let inspect = arguments.first().is_some_and(|value| value == "--inspect");
    if inspect { arguments.remove(0); }
    let normal_cancel = arguments.first().is_some_and(|value| value == "--verify-phone-cancel-normal");
    let verify_cancel = if arguments.first().is_some_and(|value| value == "--verify-phone-cancel" || value == "--verify-phone-cancel-normal") {
        arguments.remove(0);
        if arguments.is_empty() { bail!("--verify-phone-cancel requires the isolated host directory"); }
        Some(PathBuf::from(arguments.remove(0)))
    } else { None };
    let ask_session = if arguments.first().is_some_and(|value| value == "--ask-session") {
        arguments.remove(0);
        if arguments.is_empty() { bail!("--ask-session requires a UUID"); }
        Some(Uuid::parse_str(&arguments.remove(0))?)
    } else { None };
    if arguments.len() != 3 {
        bail!("usage: seed_remote_workspace [--inspect] DAEMON_ADDRESS ISOLATED_WORKSPACE OUTPUT_JSON");
    }
    let root = PathBuf::from(&arguments[1]);
    let output = PathBuf::from(&arguments[2]);
    if !root.is_absolute() || !output.is_absolute()
        || !root.join(".fintwind-e2e-fixture").is_file()
    {
        bail!("a marked, absolute isolated workspace is required");
    }
    let token = std::env::var("FINTWIND_E2E_DAEMON_TOKEN")
        .context("isolated daemon credential is missing")?;
    let daemon = DaemonClient::connect(&arguments[0], token)?;
    let before = daemon.request(Uuid::nil(), Uuid::nil(), Command::LoadTaskState)?;
    let ResponsePayload::TaskState { projects, mut sessions, .. } = before else {
        bail!("daemon did not provide task state");
    };
    if let Some(host_root) = verify_cancel {
        use fintwind_protocol::submission::RemoteProvenance;
        if !host_root.is_absolute()
            || std::fs::read(host_root.join(".fintwind-owned-crash-fixture"))? != b"synthetic\n" {
            bail!("cancel verification requires a marked isolated host");
        }
        let canonical = root.canonicalize()?;
        if projects.iter().any(|project| project.path.canonicalize().ok().as_ref() != Some(&canonical)) {
            bail!("cancel verification includes a foreign workspace");
        }
        let host_id = Uuid::parse_str(std::fs::read_to_string(host_root.join("host-id.txt"))?.trim())?;
        let grants: serde_json::Value = serde_json::from_slice(&std::fs::read(host_root.join("paired-device.json"))?)?;
        let connection = rusqlite::Connection::open_with_flags(host_root.join("operations.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        let mut statement = connection.prepare("SELECT operation_id, cancel_target, device_id FROM operations WHERE cancel_target IS NOT NULL")?;
        let records = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        if records.is_empty() || records.len() > 16 { bail!("unexpected phone cancellation count"); }
        let system = RemoteProvenance { host_id, device_id: Uuid::nil(), grant_version: 0 };
        let list = || daemon.request(Uuid::nil(), Uuid::nil(), Command::ListRemotePrompts { provenance: system });
        let before = serde_json::to_vec(&list()?)?;
        let mut checks = 0;
        for (cancel, target, device) in records {
            let cancel = Uuid::parse_str(&cancel)?;
            let target = Uuid::parse_str(&target)?;
            let device_id = Uuid::parse_str(&device)?;
            let version = grants["grants"].as_array().context("missing grant directory")?.iter()
                .find(|grant| grant["deviceId"].as_str() == Some(device.as_str()))
                .and_then(|grant| grant["grantVersion"].as_u64()).context("missing exact grant version")?;
            let who = RemoteProvenance { host_id, device_id, grant_version: version };
            let query = |provenance, operation_id, cancel_operation_id| daemon.request(Uuid::nil(), Uuid::nil(),
                Command::QueryRemoteCancel { provenance, operation_id, cancel_operation_id: Some(cancel_operation_id) });
            for _ in 0..2 {
                if !matches!(query(who, target, cancel)?, ResponsePayload::RemoteCancelState {
                    cancel_operation_id: Some(id), cancel_state, host_id: Some(host), device_id: Some(device), grant_version: Some(grant), ..
                } if id == cancel && cancel_state == "cancelled" && host == host_id && device == device_id && grant == version) {
                    bail!("phone cancellation lost its exact durable binding");
                }
                checks += 1;
            }
            if !normal_cancel {
                for (provenance, target, cancel) in [
                    (who, target, Uuid::new_v4()), (who, Uuid::new_v4(), cancel),
                    (RemoteProvenance { host_id: Uuid::new_v4(), ..who }, target, cancel),
                    (RemoteProvenance { device_id: Uuid::new_v4(), ..who }, target, cancel),
                    (RemoteProvenance { grant_version: version + 1, ..who }, target, cancel),
                ] {
                    if !matches!(query(provenance, target, cancel)?, ResponsePayload::RemoteCancelState { cancel_state, .. } if cancel_state == "unknown") {
                        bail!("phone cancellation query crossed identity boundary");
                    }
                    checks += 1;
                }
            }
        }
        if before != serde_json::to_vec(&list()?)? { bail!("cancel query mutated durable queue"); }
        std::fs::write(output, serde_json::to_vec(&serde_json::json!({
            "synthetic": true, "passed": true, "exactCancelChecks": checks, "queryReadOnly": true,
            "normalOnly": normal_cancel, "identityBoundaryNegativeVerified": !normal_cancel
        }))?)?;
        return Ok(());
    }
    if let Some(session_id) = ask_session {
        let canonical = root.canonicalize()?;
        if projects.iter().any(|project| project.path.canonicalize().ok().as_ref() != Some(&canonical)) {
            bail!("task state includes a foreign workspace");
        }
        let session = sessions.iter_mut().find(|session| session.id == session_id)
            .context("isolated task missing")?;
        if session.native_session_id.is_some() || !projects.iter().any(|project| project.id == session.project_id) {
            bail!("ask policy requires an unstarted isolated task");
        }
        session.runtime_mode = fintwind_protocol::model::RuntimeMode::Ask;
        let live_session_ids = sessions.iter().map(|session| session.id).collect();
        let reply = daemon.request(Uuid::nil(), Uuid::nil(), Command::SaveTaskState { projects, sessions, live_session_ids })?;
        if !matches!(reply, ResponsePayload::TaskStateSaved { .. } | ResponsePayload::Ack) {
            bail!("local ask policy write was not acknowledged");
        }
        std::fs::write(output, b"{\"synthetic\":true,\"localAskPolicy\":true}")?;
        return Ok(());
    }
    if inspect {
        let root = root.canonicalize()?;
        if projects.iter().any(|project| project.path.canonicalize().ok().as_ref() != Some(&root))
            || sessions.iter().any(|session| !projects.iter().any(|project| project.id == session.project_id))
        {
            bail!("task state contains data outside this acceptance workspace");
        }
        for session in &mut sessions {
            let ResponsePayload::Session { session: Some(hydrated) } = daemon.request(
                session.id, Uuid::nil(), Command::HydrateSession { session_id: session.id },
            )? else { bail!("synthetic task detail is unavailable"); };
            if hydrated.id != session.id || hydrated.project_id != session.project_id {
                bail!("synthetic task detail binding differs");
            }
            *session = hydrated;
        }
        std::fs::write(output, serde_json::to_vec_pretty(&serde_json::json!({
            "synthetic": true,
            "projects": projects.iter().map(|project| project.id).collect::<Vec<_>>(),
            "sessions": sessions.iter().map(|session| serde_json::json!({
                "sessionId": session.id, "projectId": session.project_id, "title": session.display_title(),
                "nativeSessionId": session.native_session_id, "turnCount": session.turns.len(),
                "model": session.model, "agentPreset": session.agent_preset,
                "reasoningEffort": session.reasoning_effort, "runtimeMode": session.runtime_mode,
                "interactionMode": session.interaction_mode
            })).collect::<Vec<_>>()
        }))?)?;
        println!("[OK] Isolated synthetic task state inspected.");
        return Ok(());
    }
    if !projects.is_empty() || !sessions.is_empty() {
        bail!("the daemon must have an empty isolated task database");
    }
    let mut project = Project::from_path(root);
    project.name = "Acceptance workspace".to_owned();
    let mut session = AgentSession::new(project.id);
    session.title = "Remote acceptance fixture".to_owned();
    let session_id = session.id;
    let project_id = project.id;
    let reply = daemon.request(
        Uuid::nil(), Uuid::nil(),
        Command::SaveTaskState { projects: vec![project], live_session_ids: vec![session.id], sessions: vec![session] },
    )?;
    if matches!(reply, ResponsePayload::TaskStateSaved { .. }) || matches!(reply, ResponsePayload::Ack) {
        let ResponsePayload::TaskState { projects, sessions, .. } = daemon.request(
            Uuid::nil(), Uuid::nil(), Command::LoadTaskState,
        )? else { bail!("daemon did not provide the seeded task state"); };
        if projects.len() != 1 || sessions.len() != 1
            || projects[0].id != project_id || sessions[0].id != session_id
        {
            bail!("daemon did not persist exactly the requested synthetic task");
        }
        std::fs::write(output, serde_json::to_vec_pretty(&serde_json::json!({
            "sessionId": session_id,
            "projectId": project_id,
            "synthetic": true
        }))?)?;
        println!("[OK] Synthetic workspace seeded into the isolated daemon.");
        Ok(())
    } else {
        bail!("daemon did not confirm synthetic task persistence");
    }
}

//! Seed a real, explicitly isolated daemon with a synthetic project and task.
//! No provider is started and no model/daemon credential is printed. Failure
//! modes: accidental user directory, missing isolation marker, or mismatched
//! task-state reply. All fail closed before the seed request.

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
    let ResponsePayload::TaskState { projects, sessions, .. } = before else {
        bail!("daemon did not provide task state");
    };
    if inspect {
        let root = root.canonicalize()?;
        if projects.iter().any(|project| project.path.canonicalize().ok().as_ref() != Some(&root))
            || sessions.iter().any(|session| !projects.iter().any(|project| project.id == session.project_id))
        {
            bail!("task state contains data outside this acceptance workspace");
        }
        std::fs::write(output, serde_json::to_vec_pretty(&serde_json::json!({
            "synthetic": true,
            "projects": projects.iter().map(|project| project.id).collect::<Vec<_>>(),
            "sessions": sessions.iter().map(|session| serde_json::json!({
                "sessionId": session.id, "projectId": session.project_id, "title": session.title
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

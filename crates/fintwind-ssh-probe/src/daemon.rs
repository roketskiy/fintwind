use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use fintwind_client::DaemonClient;
use fintwind_protocol::{
    Command, ResponsePayload, WireDriverStartOptions, WorkspaceOperation, WorkspaceResult,
    model::{AgentSession, InteractionMode, Project, RuntimeMode, SessionWorkspace},
    submission::SubmissionRecord,
};
use tokio::{task::spawn_blocking, time::timeout};
use uuid::Uuid;

use crate::{Check, DEADLINE, Session, SshTarget, connect};

#[derive(Clone)]
pub(super) struct Target {
    pub port: u16,
    pub cwd: PathBuf,
    pub agent: Option<Agent>,
}

#[derive(Clone)]
pub(super) struct Agent {
    pub binary: PathBuf,
    pub model: String,
    pub expected: String,
}

const RPC: Duration = Duration::from_secs(20);
const STREAM: Duration = Duration::from_secs(45);

pub(super) async fn run(
    session: Session,
    ssh: &SshTarget,
    target: &Target,
    checks: &mut Vec<Check>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let result = bridge(session, ssh.clone(), target, tx).await;
    checks.extend(rx.try_iter());
    if let Err(error) = result {
        checks.push(Check {
            name: "daemon_e2e",
            passed: false,
            detail: format!("{error:#}"),
        });
        eprintln!("FAIL daemon_e2e: {error:#}");
    }
}

async fn bridge(
    session: Session,
    ssh: SshTarget,
    target: &Target,
    report: std::sync::mpsc::Sender<Check>,
) -> Result<()> {
    let token_file = std::env::var("FINTWIND_PROBE_REMOTE_TOKEN_FILE")
        .context("missing remote token-file path")?;
    timeout(DEADLINE + 5 * RPC + STREAM, async {
        let session = std::sync::Arc::new(session);
        if std::env::var_os("FINTWIND_PROBE_PROJECT_CREATION").is_some() {
            verify_project_creation(&session, &target.cwd, &report).await?;
            return Ok(());
        }
        let ssh_for_peer = ssh.clone();
        let connection = fintwind_client::ssh::Connection::open_with_reconnect(
            session.clone(),
            target.port,
            &token_file,
            Some(move || {
                let ssh = ssh.clone();
                async move { connect(&ssh).await }
            }),
        )
        .await?;
        if let Ok(port) = std::env::var("FINTWIND_PROBE_PROVIDER_PEER_PORT") {
            let peer = fintwind_client::ssh::Connection::open(
                connect(&ssh_for_peer).await?, port.parse()?,
                &std::env::var("FINTWIND_PROBE_PROVIDER_PEER_TOKEN_FILE")?,
            ).await?;
            let binary = target.agent.as_ref().context("missing fixture OpenCode")?.binary.clone();
            let selected = connection.supervisor();
            let other = peer.supervisor();
            spawn_blocking(move || verify_provider_isolation(selected, other, binary, &report)).await??;
            return Ok(());
        }
        if let Ok(root) = std::env::var("FINTWIND_PROBE_SKILLS_LOCAL") {
            let client = connection.client();
            let before = client.clone();
            let local_path = spawn_blocking(move || verify_skills(client, &root)).await??;
            let recovered = interrupt_and_reconnect(&session, &connection).await?;
            spawn_blocking(move || {
                use fintwind_client::ssh::{change_skill, load_skills};
                ensure!(change_skill(Some(&before), vec![local_path.clone()], Some(false)).is_err_and(|e| e.to_string() == "Skills daemon is offline"), "offline mutation not blocked");
                ensure!(load_skills(&recovered)?.skills.iter().all(|s| s.name != "ssh-routing-fixture"), "removed skill still present after reconnect");
                ensure!(std::fs::read_to_string(local_path.join("SKILL.md"))?.contains("Windows fixture"), "Windows skill changed");
                let detail = "remote Skill scanned, disabled, enabled and removed; disconnected mutation refused; Windows copy unchanged";
                println!("PASS remote_skill_isolation: {detail}");
                let _ = report.send(Check { name: "remote_skill_isolation", passed: true, detail: detail.into() });
                Ok::<_, anyhow::Error>(())
            }).await??;
            return Ok(());
        }
        if let Ok(address) = std::env::var("FINTWIND_PROBE_LOCAL_SETTINGS_ADDRESS") {
            let target = target.clone();
            let original = connection.client();
            let remote = original.clone();
            let local = spawn_blocking(move || verify_settings_write(remote, &target, &address)).await??;
            let recovered = interrupt_and_reconnect(&session, &connection).await?;
            spawn_blocking(move || {
                use fintwind_client::ssh::{GeneralSetting, save_general_setting};
                let change = save_general_setting(Some(original), GeneralSetting::Workspace, "/never-written");
                ensure!(change.is_err_and(|error| error.to_string() == "SSH daemon is offline"), "disconnected client unexpectedly accepted a settings write");
                ensure!(read_settings(&recovered)?.general_workspace_path().is_some_and(|path| path.ends_with("settings-workspace")), "offline write changed remote settings");
                ensure!(read_settings(&local.0)? == local.1, "Windows daemon settings changed");
                let detail = "remote settings saved and reread; unverified OpenCode features and disconnected writes refused; Windows daemon settings unchanged";
                println!("PASS remote_general_settings: {detail}");
                let _ = report.send(Check { name: "remote_general_settings", passed: true, detail: detail.into() });
                Ok::<_, anyhow::Error>(())
            }).await??;
            return Ok(());
        }
        let target = target.clone();
        let observer = report.clone();
        let original = connection.client();
        let runtime = tokio::runtime::Handle::current();
        let (connection, interrupted_during_turn) = spawn_blocking(move || {
            let interrupt = || runtime.block_on(interrupt_and_reconnect(&session, &connection));
            let interrupted_during_turn = target.agent.is_some();
            exercise(original, &target, &report, &interrupt)?;
            if !interrupted_during_turn {
                interrupt()?;
            }
            Ok::<_, anyhow::Error>((connection, interrupted_during_turn))
        }).await??;
        let recovered = connection.client();
        let settings = spawn_blocking(move || {
            recovered.request(Uuid::nil(), Uuid::nil(), Command::GetSettings)
        }).await??;
        ensure!(matches!(settings, ResponsePayload::Settings { .. }), "wrong response after SSH reconnection");
        let detail = if interrupted_during_turn {
            "SSH interrupted after the first text delta; original runtime finished after reconnect without resubmission"
        } else {
            "new SSH transport restored the original daemon supervisor; GetSettings succeeded"
        };
        println!("PASS ssh_daemon_reconnect: {detail}");
        let _ = observer.send(Check { name: "ssh_daemon_reconnect", passed: true, detail: detail.into() });
        Ok(())
    }).await.context("daemon SSH probe timed out")?
}

fn verify_skills(remote: DaemonClient, root: &str) -> Result<PathBuf> {
    use fintwind_client::ssh::{change_skill, load_skills};
    let local = DaemonClient::connect(
        &std::env::var("FINTWIND_PROBE_LOCAL_SETTINGS_ADDRESS")?,
        std::env::var("FINTWIND_PROBE_LOCAL_SETTINGS_TOKEN")?,
    )?;
    let ResponsePayload::SkillsCatalog { catalog } = local.request(
        Uuid::nil(),
        Uuid::nil(),
        Command::LoadSkills {
            projects: vec![("Windows fixture".into(), PathBuf::from(root))],
        },
    )?
    else {
        bail!("invalid local catalog");
    };
    let local_path = catalog
        .skills
        .iter()
        .find(|s| {
            s.project.as_deref() == Some("Windows fixture")
                && s.name == "ssh-routing-fixture"
                && s.enabled
        })
        .context("missing local skill")?
        .primary()
        .dir
        .clone();
    let get = || {
        Ok::<_, anyhow::Error>(
            load_skills(&remote)?
                .skills
                .into_iter()
                .find(|s| s.name == "ssh-routing-fixture"),
        )
    };
    let entry = get()?.context("missing remote skill")?;
    ensure!(
        entry.enabled && entry.body.contains("Remote fixture"),
        "wrong remote Skill"
    );
    let dirs = vec![entry.primary().dir.clone()];
    change_skill(Some(&remote), dirs.clone(), Some(false))?;
    ensure!(
        get()?.is_some_and(|s| !s.enabled),
        "disable did not persist"
    );
    change_skill(Some(&remote), dirs.clone(), Some(true))?;
    ensure!(get()?.is_some_and(|s| s.enabled), "enable did not persist");
    change_skill(Some(&remote), dirs, None)?;
    ensure!(get()?.is_none(), "remove did not persist");
    Ok(local_path)
}

fn verify_provider_isolation(selected: fintwind_client::DaemonSupervisor, peer: fintwind_client::DaemonSupervisor, binary: PathBuf, report: &std::sync::mpsc::Sender<Check>) -> Result<()> {
    use fintwind_client::{persistence::StateStore, ssh::{GeneralSetting, provider_integrations, save_general_setting}};
    for daemon in [&selected, &peer] {
        save_general_setting(Some(daemon.client()), GeneralSetting::OpenCodePath, binary.to_str().context("invalid fixture binary path")?)?;
    }
    const PROVIDER: &str = "openai";
    let ready = |daemon: &fintwind_client::DaemonSupervisor, host: &str| {
        let start = Instant::now();
        loop {
            let (workspace, rows) = provider_integrations(daemon.clone())?;
            if rows.iter().any(|row| row.id == PROVIDER) {
                return Ok::<_, anyhow::Error>((workspace, rows));
            }
            ensure!(start.elapsed() < Duration::from_secs(12), "{host} has no {PROVIDER} integration after startup; available IDs: {:?}", rows.iter().take(12).map(|row| row.id.as_str()).collect::<Vec<_>>());
            std::thread::sleep(Duration::from_millis(250));
        }
    };
    let ((binary, directory), before) = ready(&selected, "selected daemon")?;
    let (_, other_before) = ready(&peer, "peer daemon")?;
    let auth_state = |rows: &[fintwind_protocol::provider_session::IntegrationSummary], host: &str| {
        rows.iter().find(|row| row.id == PROVIDER).map(|row| (row.supports_key, row.connected, row.key_accounts.len()))
            .with_context(|| format!("{host} has no {PROVIDER} integration"))
    };
    let selected_before = auth_state(&before, "selected daemon")?;
    let peer_before = auth_state(&other_before, "peer daemon")?;
    ensure!(selected_before == (true, false, 0) && peer_before == (true, false, 0), "expected key support and no authorization at baseline: selected={selected_before:?}, peer={peer_before:?}");
    StateStore::remote(selected.clone()).authorize_provider(binary, directory, PROVIDER.into(), "ssh-probe-fixture-key".into())?;
    let selected_after = auth_state(&provider_integrations(selected)?.1, "selected daemon")?;
    let peer_after = auth_state(&provider_integrations(peer)?.1, "peer daemon")?;
    ensure!(selected_after == (true, true, 1), "selected daemon did not report one key account: {selected_after:?}");
    ensure!(peer_after == peer_before, "peer daemon authorization changed: {peer_before:?} -> {peer_after:?}");
    let detail = "OpenAI fixture: selected daemon disconnected (0 keys) -> connected (1 key); independent daemon stayed disconnected (0 keys)";
    println!("PASS remote_provider_isolation: {detail}");
    let _ = report.send(Check { name: "remote_provider_isolation", passed: true, detail: detail.into() });
    Ok(())
}

fn read_settings(client: &DaemonClient) -> Result<fintwind_protocol::settings::DaemonSettings> {
    match client.request_with_timeout(Uuid::nil(), Uuid::nil(), Command::GetSettings, RPC)? {
        ResponsePayload::Settings { settings } => Ok(settings),
        _ => bail!("unexpected settings response"),
    }
}

fn verify_settings_write(
    remote: DaemonClient,
    target: &Target,
    local_address: &str,
) -> Result<(DaemonClient, fintwind_protocol::settings::DaemonSettings)> {
    use fintwind_client::ssh::{GeneralSetting, save_general_setting};
    let local = DaemonClient::connect(
        local_address,
        std::env::var("FINTWIND_PROBE_LOCAL_SETTINGS_TOKEN")?,
    )?;
    let local_before = read_settings(&local)?;
    let binary = target
        .agent
        .as_ref()
        .context("missing OpenCode fixture")?
        .binary
        .to_str()
        .context("invalid binary path")?;
    let workspace = target.cwd.join("settings-workspace");
    let workspace = workspace.to_str().context("invalid workspace path")?;
    let first = save_general_setting(Some(remote.clone()), GeneralSetting::OpenCodePath, binary)?;
    ensure!(
        first.opencode_binary_path() == Some(binary),
        "OpenCode path did not persist"
    );
    save_general_setting(Some(remote.clone()), GeneralSetting::Workspace, workspace)?;
    for (field, value) in [
        (GeneralSetting::NetworkProxy, "http://127.0.0.1:9"),
        (GeneralSetting::BrowserTools, "true"),
        (GeneralSetting::ScheduledTools, "true"),
        (GeneralSetting::Watchdog, "false"),
        (GeneralSetting::AutoCompact, "false"),
        (GeneralSetting::CompactThreshold, "78%"),
        (GeneralSetting::BackgroundCompact, "false"),
    ] {
        save_general_setting(Some(remote.clone()), field, value)?;
    }
    let second = read_settings(&remote)?;
    ensure!(
        second.opencode_binary_path() == Some(binary)
            && second.general_workspace_path() == Some(workspace),
        "remote settings were not preserved"
    );
    ensure!(second.network_proxy_url() == Some("http://127.0.0.1:9")
        && second.browser_tools_enabled()
        && second.scheduled_tools_enabled()
        && !second.watchdog_enabled()
        && !second.auto_compact_enabled()
        && !second.background_compact_enabled()
        && second.auto_compact_threshold_percent() == 78,
        "remote general settings did not reread");
    ensure!(save_general_setting(Some(remote.clone()), GeneralSetting::CompactThreshold, "100%").is_err(),
        "invalid threshold was accepted");
    ensure!(read_settings(&remote)? == second, "invalid threshold modified remote settings");
    let cleared = save_general_setting(Some(remote.clone()), GeneralSetting::NetworkProxy, "")?;
    ensure!(cleared.network_proxy_url().is_none(), "remote proxy could not be cleared");
    let unavailable = save_general_setting(Some(remote.clone()), GeneralSetting::OpenCodePath, "/bin/sh")?;
    for field in [
        GeneralSetting::BrowserTools,
        GeneralSetting::ScheduledTools,
        GeneralSetting::AutoCompact,
        GeneralSetting::CompactThreshold,
        GeneralSetting::BackgroundCompact,
    ] {
        ensure!(!field.supported_by(&unavailable), "unverified OpenCode advertised {field:?}");
    }
    ensure!(save_general_setting(Some(remote.clone()), GeneralSetting::ScheduledTools, "false")
        .is_err_and(|error| error.to_string() == "SSH daemon does not support this setting"),
        "unsupported setting update was accepted");
    let restored = save_general_setting(Some(remote), GeneralSetting::OpenCodePath, binary)?;
    ensure!(GeneralSetting::ScheduledTools.supported_by(&restored), "OpenCode capability was not restored");
    Ok((local, local_before))
}

async fn verify_project_creation(
    session: &Session,
    parent: &std::path::Path,
    report: &std::sync::mpsc::Sender<Check>,
) -> Result<()> {
    use fintwind_client::ssh::{GitCloneStage, ProjectCreation, create_project};
    let parent = parent.to_str().context("invalid fixture path")?;
    let repository_url = "https://github.com/BurntSushi/ripgrep.git";
    let repo = Some(repository_url);
    let mut received_objects = None;
    for (name, source) in [("empty-fixture", None), ("clone-fixture", repo)] {
        let created = create_project(session, parent, name, source, |progress| {
            if progress.stage == GitCloneStage::Receiving {
                received_objects = Some(progress.percent);
            }
        }).await?;
        ensure!(
            matches!(created, ProjectCreation::Created(ref path) if path == &format!("{parent}/{name}")),
            "creation failed: {created:?}"
        );
        let collision = create_project(session, parent, name, repo, |_| {}).await?;
        ensure!(
            matches!(collision, ProjectCreation::Exists),
            "clone did not refuse existing directory: {collision:?}"
        );
        let collision = create_project(session, parent, name, None, |_| {}).await?;
        ensure!(
            matches!(collision, ProjectCreation::Exists),
            "mkdir did not refuse existing directory: {collision:?}"
        );
    }
    ensure!(received_objects == Some(100), "Git checkout did not report receiving all objects: {received_objects:?}");
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    let mut channel = session.channel_open_session().await?;
    channel.exec(true, format!("cd {} && test -z \"$(ls -A empty-fixture)\" && test \"$(git -C clone-fixture remote get-url origin)\" = {} && git -C clone-fixture diff --exit-code && git -C clone-fixture rev-parse --verify HEAD >/dev/null", quote(parent), quote(repository_url))).await?;
    let mut status = None;
    while let Some(message) = channel.wait().await {
        if let russh::ChannelMsg::ExitStatus { exit_status } = message {
            status = Some(exit_status);
        }
    }
    ensure!(
        status == Some(0),
        "remote folder or Git checkout was not preserved"
    );
    let detail = "remote mkdir and GitHub checkout succeeded; received Git's 100% object transfer update; both operations refused existing directories";
    println!("PASS remote_project_creation: {detail}");
    let _ = report.send(Check {
        name: "remote_project_creation",
        passed: true,
        detail: detail.into(),
    });
    Ok(())
}

async fn interrupt_and_reconnect(
    session: &Session,
    connection: &fintwind_client::ssh::Connection,
) -> Result<DaemonClient> {
    let original = connection.client();
    session
        .disconnect(
            russh::Disconnect::ByApplication,
            "interrupt owned SSH fixture",
            "en",
        )
        .await?;
    timeout(Duration::from_secs(20), async {
        while !original.is_disconnected() || !connection.is_ready() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("SSH transport did not recover")?;
    Ok(connection.client())
}

fn exercise(
    client: DaemonClient,
    target: &Target,
    report: &std::sync::mpsc::Sender<Check>,
    interrupt: &impl Fn() -> Result<DaemonClient>,
) -> Result<()> {
    let pass = |name, detail: String| {
        println!("PASS {name}: {detail}");
        let _ = report.send(Check {
            name,
            passed: true,
            detail,
        });
    };
    pass(
        "daemon_handshake",
        "real DaemonClient authenticated through direct-tcpip".into(),
    );
    let cwd = &target.cwd;
    let nil = Uuid::nil();
    let branch = client
        .request_with_timeout(
            nil,
            nil,
            Command::Workspace {
                operation: WorkspaceOperation::CurrentBranch { cwd: cwd.clone() },
            },
            RPC,
        )
        .context("read remote workspace branch")?;
    ensure!(
        matches!(branch, ResponsePayload::Workspace { result: WorkspaceResult::CurrentBranch { branch: Some(ref name) } } if name == "ssh-probe"),
        "unexpected remote branch: {branch:?}"
    );
    pass(
        "remote_git_workspace",
        format!("remote Git reported ssh-probe for {}", cwd.display()),
    );

    if let Some(agent) = &target.agent {
        let saved = client
            .request_with_timeout(
                nil,
                nil,
                Command::StoreCustomProviderKey {
                    binary: agent.binary.clone(),
                    directory: cwd.clone(),
                    provider_id: agent
                        .model
                        .split_once('/')
                        .context("expected provider/model")?
                        .0
                        .into(),
                    key: "ssh-probe-fixture-key".into(),
                },
                RPC,
            )
            .context("save fixture key in remote OpenCode")?;
        ensure!(
            matches!(saved, ResponsePayload::Ack),
            "fixture key was not acknowledged"
        );
        let (client, detail) = stream_agent(client, cwd.clone(), agent, interrupt)?;
        pass(
            "remote_provider_key",
            "fixture key saved over SSH and accepted by the model endpoint; no inline key or credential environment variable".into(),
        );
        pass("agent_text_delta", detail);
        let status = client.request_with_timeout(
            nil,
            nil,
            Command::ListMcpServerStatuses {
                binary: agent.binary.clone(),
                directory: cwd.clone(),
            },
            RPC,
        )?;
        let ResponsePayload::McpServerStatuses { statuses } = status else {
            bail!("unexpected MCP status response");
        };
        ensure!(
            statuses.iter().any(|row| row.name == "ssh-fixture"
                && matches!(
                    row.status,
                    fintwind_protocol::provider_session::McpConnectionState::Connected
                )),
            "remote MCP fixture did not connect: {statuses:?}"
        );
        pass(
            "remote_mcp_header",
            "remote OpenCode initialized MCP using the remote process credential".into(),
        );
    }
    Ok(())
}

fn stream_agent(
    mut client: DaemonClient,
    cwd: PathBuf,
    agent: &Agent,
    interrupt: &impl Fn() -> Result<DaemonClient>,
) -> Result<(DaemonClient, String)> {
    ensure!(
        !agent.expected.is_empty(),
        "expected Agent text must not be empty"
    );
    let project = Project {
        id: Uuid::new_v4(),
        name: "SSH E2E".into(),
        path: cwd.clone(),
        created_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    };
    let mut session = AgentSession::new(project.id);
    session.model = Some(agent.model.clone());
    session.runtime_mode = RuntimeMode::Ask;
    session.interaction_mode = InteractionMode::Build;
    let turn = Uuid::new_v4();
    let runtime = Uuid::new_v4();
    let prompt = "Reply with a short greeting in plain text. Do not use tools.".to_owned();
    session.begin_turn_with_id(turn, prompt.clone());
    let record = SubmissionRecord::new(session.id, turn, prompt, Vec::new());
    session
        .turns
        .last_mut()
        .context("missing new turn")?
        .submission = Some(record.receipt.clone());
    let id = session.id;
    let submission_id = record.receipt.id;
    let mut events = client.subscribe(id, runtime);
    let accepted = client
        .request_with_timeout(
            id,
            Uuid::nil(),
            Command::AcceptSubmission {
                project,
                session: Box::new(session),
                submission: Box::new(record),
            },
            RPC,
        )
        .context("accept test submission")?;
    ensure!(
        matches!(accepted, ResponsePayload::SubmissionSaved { .. }),
        "unexpected acceptance: {accepted:?}"
    );
    let started = client
        .request_with_timeout(
            id,
            runtime,
            Command::Start {
                options: WireDriverStartOptions {
                    binary: agent.binary.clone(),
                    cwd,
                    mode: "ask".into(),
                    interaction_mode: "build".into(),
                    model: Some(agent.model.clone()),
                    reasoning_effort: None,
                    service_tier: None,
                    context_window: None,
                    agent_preset: None,
                    provider_cursor: None,
                    mermaid_prompt_version: None,
                },
            },
            RPC,
        )
        .context("start remote OpenCode")?;
    ensure!(
        matches!(started, ResponsePayload::Started { .. }),
        "unexpected start: {started:?}"
    );
    let dispatched = client
        .request_with_timeout(
            id,
            runtime,
            Command::DispatchSubmission {
                submission_id,
                workspace: SessionWorkspace::Local,
            },
            RPC,
        )
        .context("dispatch test submission")?;
    ensure!(
        matches!(dispatched, ResponsePayload::SubmissionSaved { .. }),
        "unexpected dispatch: {dispatched:?}"
    );
    let until = Instant::now() + STREAM;
    let mut text = String::new();
    let mut sequences = Vec::new();
    while let Ok(event) = events.recv_deadline(until) {
        match event.event.kind.as_str() {
            "textDelta" => {
                let payload = &event.event.payload;
                if let Some(delta) = payload.as_str().or_else(|| payload["delta"].as_str())
                    && !delta.is_empty()
                {
                    text.push_str(delta);
                    ensure!(
                        sequences.last().is_none_or(|last| *last < event.sequence),
                        "duplicate or out-of-order delta after reconnect"
                    );
                    sequences.push(event.sequence);
                    if sequences.len() == 1 {
                        client = interrupt()?;
                        let attached = client
                            .request_with_timeout(id, Uuid::nil(), Command::AttachSession, RPC)
                            .context("reattach to the remote runtime after disconnect")?;
                        ensure!(
                            matches!(attached, ResponsePayload::SessionRuntime { runtime_id: Some(active), .. } if active == runtime),
                            "original runtime did not survive disconnect: {attached:?}"
                        );
                        events = client.subscribe(id, runtime);
                    }
                }
            }
            "turnFinished" => {
                ensure!(
                    event.event.payload["success"].as_bool() == Some(true),
                    "Agent turn did not succeed: {}",
                    event.event.payload
                );
                ensure!(
                    sequences.len() >= 2 && text.contains(&agent.expected),
                    "Agent turn finished without expected streamed text; observed sequences {sequences:?}"
                );
                return Ok((
                    client,
                    format!(
                        "reattached to the original runtime and received expected text across delta sequences {sequences:?} before successful turn completion"
                    ),
                ));
            }
            "error" => bail!("runtime error: {}", event.event.payload),
            _ => {}
        }
    }
    bail!(
        "did not receive expected Agent text in multiple deltas; observed sequences {sequences:?}"
    )
}

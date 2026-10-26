use super::*;
use serde_json::{Value, json};

pub(super) fn verify(
    client: &DaemonClient,
    second: &DaemonClient,
    project: &Project,
    arguments: &Arguments,
    prefix: &str,
    checks: &mut Checks,
) -> anyhow::Result<()> {
    desktop_setting_round_trip(client, arguments, checks)?;
    let title = format!("{prefix}-mcp-current");
    let input = json!({
        "title": title, "prompt": format!("{prefix}-mcp-dispatched"), "enabled": true,
        "target": "currentSession", "schedule": {"kind": "every", "intervalSeconds": 3600},
    });
    let mut off = input.clone();
    off["title"] = json!(format!("{prefix}-mcp-default-off"));
    let session = turn(client, project, arguments, off)?;
    if !list(client)?.is_empty() {
        bail!("scheduled MCP created a task while disabled by default");
    }
    require_catalog(arguments, &format!("{prefix}-mcp-default-off"), false)?;
    checks.ok("scheduled MCP is absent from the model catalog and refuses task creation while off");
    set_enabled(client, true)?;
    let first_changes = client.subscribe_scheduled_tasks();
    let second_changes = second.subscribe_scheduled_tasks();
    repeat(client, project, session.id, input.clone())?;
    let task = list(client)?
        .into_iter()
        .find(|task| task.title == title)
        .context("the real OpenCode MCP call created no task")?;
    if task.target
        != (TaskTarget::ExistingSession {
            session_id: session.id,
        })
        || task.schedule
            != (Recurrence::Every {
                interval_secs: 3600,
            })
        || task.next_due_at.is_none()
    {
        bail!("the MCP-created task lost its session, schedule or next run: {task:?}");
    }
    first_changes.recv_timeout(Duration::from_secs(5))?;
    second_changes.recv_timeout(Duration::from_secs(5))?;
    checks.ok("real OpenCode client-hosted MCP creates a task for its own session and notifies both clients");
    checks.fact("mcp_current_task", task.id.0.to_string());

    let model_result = result_for(arguments, &title)?;
    if !model_result.contains(&task.id.0.to_string()) {
        bail!("the provider did not receive the created task ID: {model_result}");
    }
    checks.ok("the created task ID and next run reach the model through MCP");

    run_now(client, task.id)?;
    let settled = poll_task(client, task.id, RUN_TIMEOUT, |task| {
        task.pending.is_none() && task.run_count == 1
    })?;
    let last = settled
        .last
        .as_ref()
        .context("MCP task has no run record")?;
    let (ran_session, _) = require_accepted(&last.outcome, "the MCP-created task")?;
    if ran_session != session.id {
        bail!("the MCP task ran in another session");
    }
    checks.ok("an agent-created task dispatches through the existing scheduler into its originating session");

    let mut fresh = input.clone();
    fresh["title"] = json!(format!("{prefix}-mcp-new"));
    fresh["target"] = json!("newSession");
    fresh["enabled"] = json!(false);
    fresh["schedule"] =
        json!({"kind": "atTime", "time": "09:15", "weekdays": ["monday", "friday"]});
    turn(client, project, arguments, fresh)?;
    let new_task = list(client)?
        .into_iter()
        .find(|task| task.title == format!("{prefix}-mcp-new"))
        .context("the MCP new-session task was not created")?;
    let TaskTarget::NewSession {
        project_id,
        launch,
        workspace,
    } = &new_task.target
    else {
        bail!("the MCP task ignored the explicit new-session target");
    };
    if *project_id != project.id
        || launch.model != FIXTURE_MODEL
        || launch.runtime_mode != RuntimeMode::FullAccess
        || *workspace != SessionWorkspace::Local
        || new_task.next_due_at.is_some()
        || new_task.enabled
        || new_task.schedule
            != (Recurrence::AtTime {
                at: ClockTime::new(9, 15).map_err(anyhow::Error::msg)?,
                weekdays: Weekdays::new((1 << 1) | (1 << 5)).map_err(anyhow::Error::msg)?,
            })
    {
        bail!("the MCP new-session task lost confirmed settings: {new_task:?}");
    }
    checks.ok("explicit new-session tasks copy the workspace and launch settings and preserve weekday and paused choices");

    let mut spoof = input.clone();
    spoof["title"] = json!(format!("{prefix}-mcp-spoof"));
    let foreign_session = Uuid::new_v4();
    spoof["sessionId"] = json!(foreign_session);
    let originating = turn(client, project, arguments, spoof)?;
    let spoofed = list(client)?
        .into_iter()
        .find(|task| task.title == format!("{prefix}-mcp-spoof"))
        .context("the normalized Code Mode call created no task")?;
    if spoofed.target
        != (TaskTarget::ExistingSession {
            session_id: originating.id,
        })
    {
        bail!("a model-supplied session ID changed the originating target: {spoofed:?}");
    }
    checks.ok("two sessions share the MCP host and a model-supplied foreign session ID cannot change the target");

    let baseline = list(client)?.len();
    for (label, patch) in [
        ("missing target", json!({"target": null})),
        ("missing schedule", json!({"schedule": null})),
        ("missing active choice", json!({"enabled": null})),
        (
            "invalid interval",
            json!({"schedule": {"kind": "every", "intervalSeconds": 1}}),
        ),
        (
            "invalid clock",
            json!({"schedule": {"kind": "atTime", "time": "25:00", "weekdays": ["monday"]}}),
        ),
        (
            "empty weekdays",
            json!({"schedule": {"kind": "atTime", "time": "09:00", "weekdays": []}}),
        ),
        ("arbitrary target", json!({"target": Uuid::new_v4()})),
    ] {
        let mut invalid = input.clone();
        let invalid_title = format!("{prefix}-mcp-invalid-{label}");
        invalid["title"] = json!(invalid_title);
        let object = invalid
            .as_object_mut()
            .context("fixture input is not an object")?;
        for (key, value) in patch
            .as_object()
            .context("fixture patch is not an object")?
        {
            if value.is_null() {
                object.remove(key);
            } else {
                object.insert(key.clone(), value.clone());
            }
        }
        turn(client, project, arguments, invalid)?;
        if list(client)?.len() != baseline {
            bail!("{label} created a task");
        }
        let result = result_for(arguments, &invalid_title)?;
        if result.is_empty() {
            bail!("{label} returned no error to the provider");
        }
        checks.ok(format!("MCP refuses {label} without creating a task"));
    }

    for task in [settled, new_task, spoofed] {
        expect_ack(
            client,
            Command::DeleteScheduledTask {
                id: task.id,
                expected_revision: task.revision,
            },
        )?;
    }
    let baseline = list(client)?.len();
    set_enabled(client, false)?;
    let disabled_title = format!("{prefix}-mcp-disabled-live");
    let mut disabled = input.clone();
    disabled["title"] = json!(disabled_title);
    repeat(client, project, session.id, disabled.clone())?;
    if list(client)?.len() != baseline {
        bail!("disabled MCP created a task");
    }
    let settled_title = format!("{prefix}-mcp-disabled-settled");
    disabled["title"] = json!(settled_title);
    repeat(client, project, session.id, disabled)?;
    require_catalog(arguments, &settled_title, false)?;
    checks.ok("turning off scheduled MCP removes it from the existing session without changing scheduled tasks");

    set_enabled(client, true)?;
    set_enabled(client, false)?;
    set_enabled(client, true)?;
    let reenabled_title = format!("{prefix}-mcp-reenabled");
    let mut reenabled = input;
    reenabled["title"] = json!(reenabled_title);
    repeat(client, project, session.id, reenabled)?;
    let reenabled = list(client)?
        .into_iter()
        .find(|task| task.title == reenabled_title)
        .context("rapid off/on did not restore the existing session's tool")?;
    expect_ack(
        client,
        Command::DeleteScheduledTask {
            id: reenabled.id,
            expected_revision: reenabled.revision,
        },
    )?;
    checks.ok("rapid off/on automatically restores experimental MCP in the same live session");
    Ok(())
}

fn set_enabled(client: &DaemonClient, enabled: bool) -> anyhow::Result<()> {
    let ResponsePayload::Settings { mut settings } =
        client.request(Uuid::nil(), Uuid::nil(), Command::GetSettings)?
    else {
        bail!("GetSettings did not return settings");
    };
    settings.extra.insert(
        fintwind_protocol::settings::SCHEDULED_TOOLS_ENABLED_KEY.to_owned(),
        json!(enabled),
    );
    expect_ack(client, Command::UpdateSettings { settings })
}

fn desktop_setting_round_trip(
    client: &DaemonClient,
    arguments: &Arguments,
    checks: &mut Checks,
) -> anyhow::Result<()> {
    use fintwind_client::{DaemonSupervisor, persistence::StateStore};
    let supervisor = DaemonSupervisor::connect(
        &std::env::var(DAEMON_ADDRESS_ENV)?,
        std::env::var(DAEMON_TOKEN_ENV)?,
    )?;
    let directory = arguments.data_root.join("desktop-settings");
    let store = StateStore::remote_in(supervisor, &directory);
    let mut state = store.load_or_fresh(arguments.data_root.join("workspace"));
    if state.scheduled_tools_enabled {
        bail!("new desktop settings unexpectedly enable scheduled MCP");
    }
    for enabled in [true, false] {
        state.scheduled_tools_enabled = enabled;
        store.flush(&mut state)?;
        if store.load()?.scheduled_tools_enabled != enabled {
            bail!("desktop settings did not persist the MCP switch");
        }
        let ResponsePayload::Settings { settings } =
            client.request(Uuid::nil(), Uuid::nil(), Command::GetSettings)?
        else {
            bail!("GetSettings did not return settings");
        };
        if settings.scheduled_tools_enabled() != enabled {
            bail!("desktop switch did not reach the daemon");
        }
    }
    checks.ok(
        "the real desktop store persists the general-setting switch and applies it to the daemon",
    );
    Ok(())
}

fn repeat(
    client: &DaemonClient,
    project: &Project,
    session_id: Uuid,
    input: Value,
) -> anyhow::Result<()> {
    let mut session = hydrate(client, session_id)?.context("MCP source session disappeared")?;
    let ResponsePayload::SessionRuntime {
        runtime_id: Some(runtime_id),
        ..
    } = client.request(session_id, Uuid::nil(), Command::AttachSession)?
    else {
        bail!("the existing session lost its runtime");
    };
    let prompt = format!("FINTWIND_MCP_CREATE:{}", serde_json::to_string(&input)?);
    let turn_id = Uuid::new_v4();
    session.begin_turn_with_id(turn_id, prompt.clone());
    let record = SubmissionRecord::new(session_id, turn_id, prompt, Vec::new());
    session
        .turns
        .last_mut()
        .context("new turn is missing")?
        .submission = Some(record.receipt.clone());
    accept_session(client, project, &session, &record)?;
    dispatch_submission(client, session_id, runtime_id, turn_id)?;
    wait_session(client, session_id, RUN_TIMEOUT, |session| {
        session
            .turns
            .iter()
            .any(|turn| turn.id == turn_id && turn.status == TurnStatus::Completed)
    })?;
    Ok(())
}

fn require_catalog(arguments: &Arguments, title: &str, visible: bool) -> anyhow::Result<()> {
    let first = std::fs::read_to_string(&arguments.fixture_log)?
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|entry| {
            entry["kind"] == "mcp"
                && entry["prompt"]
                    .as_str()
                    .is_some_and(|prompt| prompt.contains(title))
        })
        .context("the provider did not receive the scheduled MCP turn")?;
    if first["catalogVisible"] != visible {
        bail!("scheduled MCP catalog visibility did not match the setting: {first}");
    }
    Ok(())
}

fn turn(
    client: &DaemonClient,
    project: &Project,
    arguments: &Arguments,
    input: Value,
) -> anyhow::Result<AgentSession> {
    let prompt = format!("FINTWIND_MCP_CREATE:{}", serde_json::to_string(&input)?);
    let (mut session, record) = begin_session(project.id, &prompt, FIXTURE_MODEL);
    session.runtime_mode = RuntimeMode::FullAccess;
    accept_session(client, project, &session, &record)?;
    let runtime_id = Uuid::new_v4();
    let options = WireDriverStartOptions {
        binary: arguments.opencode_bin.clone(),
        cwd: arguments.data_root.join("workspace"),
        mode: "fullAccess".into(),
        interaction_mode: "build".into(),
        model: Some(FIXTURE_MODEL.into()),
        reasoning_effort: None,
        service_tier: None,
        context_window: None,
        agent_preset: None,
        provider_cursor: None,
        mermaid_prompt_version: None,
    };
    match client.request(session.id, runtime_id, Command::Start { options })? {
        ResponsePayload::Started { .. } => {}
        other => bail!("MCP runtime did not start: {other:?}"),
    }
    dispatch_submission(client, session.id, runtime_id, record.receipt.id)?;
    wait_session(client, session.id, RUN_TIMEOUT, |session| {
        session
            .turns
            .iter()
            .any(|turn| turn.id == record.receipt.id && turn.status == TurnStatus::Completed)
    })?;
    Ok(session)
}

fn result_for(arguments: &Arguments, title: &str) -> anyhow::Result<String> {
    std::fs::read_to_string(&arguments.fixture_log)?
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|entry| {
            entry["kind"] == "mcp"
                && entry["catalogVisible"] == true
                && entry["prompt"]
                    .as_str()
                    .is_some_and(|prompt| prompt.contains(title))
                && entry["results"]
                    .as_array()
                    .is_some_and(|results| !results.is_empty())
        })
        .map(|entry| entry["results"].to_string())
        .context("no MCP tool result reached the real provider request")
}

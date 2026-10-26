//! Black-box check of scheduled-task dispatch against a real daemon.
//!
//! Drives the public client and protocol only: create, edit, pause, resume and
//! delete with compare-and-swap from two connections, validation refusals, a
//! manual run, automatic interval dispatch, a busy existing session, change
//! notifications, and one graceful restart on the same data directory with its
//! own daemon. Nothing here reads the core crate or the database file.
//!
//! The initial daemon is supplied through `FINTWIND_DAEMON_ADDRESS` and
//! `FINTWIND_DAEMON_TOKEN`; this run stops it with a graceful shutdown, then
//! starts its own daemon from `--daemon-bin` on the same `--data-root` and
//! reaps only that child. Every wait is bounded.
//!
//! Provider requests go to the loopback fixture named by `--fixture-url`; its
//! JSONL log at `--fixture-log` is the evidence that a prompt reached a model.
//! The real OpenCode binary at `--opencode-bin` serves the manual busy session;
//! scheduled runs resolve the same binary from PATH.
//!
//! Usage:
//!   scheduled_tasks_e2e --data-root DIR --proof FILE --daemon-bin EXE
//!     --opencode-bin EXE --fixture-url URL --fixture-log FILE

use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use fintwind_client::DaemonClient;
use fintwind_protocol::model::{
    AgentSession, InteractionMode, Project, RuntimeMode, SessionWorkspace, TurnStatus,
};
use fintwind_protocol::scheduled_task::{
    ClockTime, DispatchOutcome, MAX_INTERVAL_SECS, MIN_INTERVAL_SECS, Recurrence, ScheduledTask,
    ScheduledTaskDraft, ScheduledTaskEdit, ScheduledTaskId, SessionLaunch, TaskTarget, Weekdays,
};
use fintwind_protocol::submission::{SubmissionRecord, SubmissionState};
use fintwind_protocol::{
    Command, DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, DaemonReady, ResponsePayload,
    WireDriverStartOptions,
};
use uuid::Uuid;

#[path = "scheduled_tasks_e2e/mcp.rs"]
mod mcp;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const RUN_TIMEOUT: Duration = Duration::from_secs(90);
const TICK_WINDOW: Duration = Duration::from_secs(7);
const AUTO_TIMEOUT: Duration = Duration::from_secs(85);

fn main() {
    let arguments = match Arguments::parse(std::env::args().skip(1)) {
        Ok(arguments) => arguments,
        Err(error) => {
            eprintln!("scheduled_tasks_e2e: {error:#}");
            std::process::exit(2);
        }
    };
    let mut checks = Checks::default();
    match run(&arguments, &mut checks) {
        Ok(()) => {
            write_proof(&arguments.proof, true, &checks, None);
            println!(
                "scheduled_tasks_e2e: passed ({} checks, proof {})",
                checks.passed.len(),
                arguments.proof.display()
            );
        }
        Err(error) => {
            let message = format!("{error:#}");
            write_proof(&arguments.proof, false, &checks, Some(&message));
            eprintln!("scheduled_tasks_e2e: failed: {message}");
            std::process::exit(1);
        }
    }
}

struct Arguments {
    data_root: PathBuf,
    proof: PathBuf,
    daemon_bin: PathBuf,
    opencode_bin: PathBuf,
    fixture_url: String,
    fixture_log: PathBuf,
    mcp_only: bool,
}

impl Arguments {
    fn parse(arguments: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut data_root = None;
        let mut proof = None;
        let mut daemon_bin = None;
        let mut opencode_bin = None;
        let mut fixture_url = None;
        let mut fixture_log = None;
        let mut mcp_only = false;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--mcp-only" => mcp_only = true,
                "--data-root" => data_root = Some(PathBuf::from(value(&mut arguments, &argument)?)),
                "--proof" => proof = Some(PathBuf::from(value(&mut arguments, &argument)?)),
                "--daemon-bin" => {
                    daemon_bin = Some(PathBuf::from(value(&mut arguments, &argument)?))
                }
                "--opencode-bin" => {
                    opencode_bin = Some(PathBuf::from(value(&mut arguments, &argument)?))
                }
                "--fixture-url" => fixture_url = Some(value(&mut arguments, &argument)?),
                "--fixture-log" => {
                    fixture_log = Some(PathBuf::from(value(&mut arguments, &argument)?))
                }
                unknown => bail!("unknown argument {unknown:?}"),
            }
        }
        let data_root = data_root.context("--data-root is required")?;
        if !data_root.is_absolute() {
            bail!("--data-root must be an absolute directory");
        }
        let daemon_bin = daemon_bin.context("--daemon-bin is required")?;
        if !daemon_bin.is_file() {
            bail!("--daemon-bin is not a file: {}", daemon_bin.display());
        }
        let opencode_bin = opencode_bin.context("--opencode-bin is required")?;
        if !opencode_bin.is_file() {
            bail!("--opencode-bin is not a file: {}", opencode_bin.display());
        }
        let fixture_url = fixture_url.context("--fixture-url is required")?;
        if !fixture_url.starts_with("http://127.0.0.1:") {
            bail!("--fixture-url must name a loopback fixture");
        }
        let fixture_log = fixture_log.context("--fixture-log is required")?;
        Ok(Self {
            data_root,
            proof: proof.context("--proof is required")?,
            daemon_bin,
            opencode_bin,
            fixture_url,
            fixture_log,
            mcp_only,
        })
    }
}

fn value(arguments: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    arguments
        .next()
        .with_context(|| format!("{flag} requires a value"))
}

#[derive(Default)]
struct Checks {
    passed: Vec<String>,
    facts: serde_json::Map<String, serde_json::Value>,
}

impl Checks {
    fn ok(&mut self, name: impl Into<String>) {
        let name = name.into();
        println!("PASS {name}");
        self.passed.push(name);
    }

    fn fact(&mut self, key: &str, value: impl Into<serde_json::Value>) {
        self.facts.insert(key.to_owned(), value.into());
    }
}

fn run(arguments: &Arguments, checks: &mut Checks) -> anyhow::Result<()> {
    protocol_checks(checks)?;

    let address = std::env::var(DAEMON_ADDRESS_ENV)
        .context("FINTWIND_DAEMON_ADDRESS is not set; this check uses the supplied daemon")?;
    let token = std::env::var(DAEMON_TOKEN_ENV).context("FINTWIND_DAEMON_TOKEN is not set")?;
    let fixture = Fixture::new(&arguments.fixture_url, &arguments.fixture_log);

    let client = connect(&address, &token)?;
    let second = connect(&address, &token)?;
    checks.ok("two connections completed the daemon handshake");

    let first_changes = client.subscribe_scheduled_tasks();
    let second_changes = second.subscribe_scheduled_tasks();
    let task_changes = client.subscribe_task_state();
    let initial = list(&client)?;
    checks.ok(format!(
        "ListScheduledTasks answered with {} task(s)",
        initial.len()
    ));

    let run_id = Uuid::new_v4().simple().to_string();
    let prefix = format!("scheduled-e2e-{}", &run_id[..8]);
    let project = setup_project(&client, arguments)?;
    let project_id = project.id;
    checks.ok("isolated project registered through SaveTaskState");

    mcp::verify(&client, &second, &project, arguments, &prefix, checks)?;
    if arguments.mcp_only {
        second.disconnect();
        client.shutdown();
        if !wait_listener_closed(&address, SHUTDOWN_TIMEOUT) {
            bail!("the isolated MCP daemon did not shut down");
        }
        checks.ok("the isolated MCP daemon shut down through its own lifecycle");
        return Ok(());
    }

    let baseline = count_prefixed(&list(&client)?, &prefix);
    validation_refusals(&client, &second, project_id, &prefix)?;
    let after_refusals = count_prefixed(&list(&client)?, &prefix);
    if after_refusals != baseline {
        bail!(
            "a refused create left {} task(s) behind",
            after_refusals - baseline
        );
    }
    checks.ok("no refused create left a task behind");

    let interval = 3600;
    let created = save(
        &client,
        ScheduledTaskEdit::Create {
            draft: base_draft(project_id, format!("{prefix}-interval"), interval),
        },
    )?;
    if created.revision != 0 {
        bail!("a new task started at revision {}", created.revision);
    }
    if !created.enabled {
        bail!("a task created enabled came back disabled");
    }
    let Some(due) = created.next_due_at else {
        bail!("an enabled interval task has no next occurrence");
    };
    if due != created.created_at + u64::from(interval) {
        bail!(
            "next occurrence {due} is not created_at {} plus {interval}",
            created.created_at
        );
    }
    if created.pending.is_some() || created.last.is_some() || created.run_count != 0 {
        bail!("a new task carried run state");
    }
    if created.updated_at != created.created_at {
        bail!("a new task has different created and updated times");
    }
    match &created.target {
        TaskTarget::NewSession { launch, .. }
            if launch.runtime_mode == RuntimeMode::Ask && launch.model == FIXTURE_MODEL => {}
        other => bail!("the created target did not preserve the frozen launch: {other:?}"),
    }
    checks.ok("create returns revision 0 and created_at + interval");

    let edited = save(
        &client,
        ScheduledTaskEdit::Update {
            id: created.id,
            expected_revision: 0,
            draft: edited_draft(&created, |draft| {
                draft.title = format!("{prefix}-interval-renamed");
            }),
        },
    )?;
    if edited.revision != 1 {
        bail!("a successful edit did not move the revision to 1");
    }
    if edited.next_due_at != created.next_due_at {
        bail!("a title edit moved the next occurrence");
    }
    checks.ok("title edit preserves the next occurrence");

    let stale = edited_draft(&edited, |draft| {
        draft.title = format!("{prefix}-stale");
        draft.prompt = "a stale writer must not win".into();
    });
    expect_error(
        &second,
        Command::SaveScheduledTask {
            edit: ScheduledTaskEdit::Update {
                id: edited.id,
                expected_revision: 0,
                draft: stale,
            },
        },
        "changed by another client",
        "a stale revision update",
    )?;
    let current = require(&list(&second)?, edited.id)?;
    if current.revision != 1 || current.title != edited.title {
        bail!("the newer edit was lost after a refused stale write");
    }
    checks.ok("a stale revision write from the second client is refused");

    let paused = save(
        &client,
        ScheduledTaskEdit::Update {
            id: edited.id,
            expected_revision: 1,
            draft: edited_draft(&edited, |draft| draft.enabled = false),
        },
    )?;
    if paused.enabled || paused.next_due_at.is_some() || paused.pending.is_some() {
        bail!("pause left a due time or a pending run");
    }
    if paused.revision != 2 {
        bail!("pause did not move the revision to 2");
    }
    checks.ok("pause clears the due time");

    let resumed = save(
        &client,
        ScheduledTaskEdit::Update {
            id: paused.id,
            expected_revision: 2,
            draft: edited_draft(&paused, |draft| draft.enabled = true),
        },
    )?;
    match resumed.next_due_at {
        Some(due) if due == resumed.updated_at + u64::from(interval) => {}
        other => bail!("resume did not schedule updated_at + {interval}: {other:?}"),
    }
    checks.ok("resume schedules a future occurrence");

    let doomed = save(
        &client,
        ScheduledTaskEdit::Create {
            draft: base_draft(project_id, format!("{prefix}-delete"), 900),
        },
    )?;
    let renamed = save(
        &client,
        ScheduledTaskEdit::Update {
            id: doomed.id,
            expected_revision: 0,
            draft: edited_draft(&doomed, |draft| {
                draft.title = format!("{prefix}-delete-renamed");
            }),
        },
    )?;
    expect_error(
        &second,
        Command::DeleteScheduledTask {
            id: doomed.id,
            expected_revision: 0,
        },
        "changed by another client",
        "a stale revision delete",
    )?;
    if require(&list(&second)?, doomed.id)?.revision != 1 {
        bail!("a refused stale delete changed the task");
    }
    expect_ack(
        &second,
        Command::DeleteScheduledTask {
            id: doomed.id,
            expected_revision: 1,
        },
    )?;
    if list(&second)?.iter().any(|task| task.id == doomed.id) {
        bail!("the deleted task is still listed");
    }
    expect_error(
        &client,
        Command::SaveScheduledTask {
            edit: ScheduledTaskEdit::Update {
                id: doomed.id,
                expected_revision: 1,
                draft: edited_draft(&renamed, |draft| {
                    draft.title = format!("{prefix}-revived");
                }),
            },
        },
        "no longer exists",
        "an edit of a deleted task",
    )?;
    if list(&client)?.iter().any(|task| task.id == doomed.id) {
        bail!("an edit revived a deleted task");
    }
    checks.ok("stale delete refused and an edit cannot revive the deleted task");

    let unknown = ScheduledTaskId(Uuid::new_v4());
    expect_error(
        &second,
        Command::DeleteScheduledTask {
            id: unknown,
            expected_revision: 0,
        },
        "no longer exists",
        "a delete of an unknown task",
    )?;
    expect_error(
        &client,
        Command::RunScheduledTaskNow { id: unknown },
        "does not exist",
        "a run now of an unknown task",
    )?;
    checks.ok("unknown task ids are refused");

    let mut rejected = base_draft(project_id, format!("{prefix}-no-request-id"), 600);
    rejected.prompt = "a mutation without a request id must not apply".into();
    client.notify(
        Uuid::nil(),
        Uuid::nil(),
        Command::SaveScheduledTask {
            edit: ScheduledTaskEdit::Create {
                draft: rejected.clone(),
            },
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if list(&client)?
            .iter()
            .any(|task| task.title == rejected.title)
        {
            bail!("a scheduled-task mutation with no request id was applied");
        }
        thread::sleep(Duration::from_millis(100));
    }
    checks.ok("a mutation without a request id is not applied within a bounded window");

    let auto_marker = format!("{prefix}-auto-prompt");
    let paused_marker = format!("{prefix}-paused-prompt");
    let deleted_marker = format!("{prefix}-deleted-prompt");
    let auto = save(
        &client,
        ScheduledTaskEdit::Create {
            draft: draft_with_prompt(project_id, format!("{prefix}-auto"), 60, &auto_marker),
        },
    )?;
    let paused_due = save(
        &client,
        ScheduledTaskEdit::Create {
            draft: draft_with_prompt(project_id, format!("{prefix}-paused"), 60, &paused_marker),
        },
    )?;
    let stopped_for_due = save(
        &client,
        ScheduledTaskEdit::Update {
            id: paused_due.id,
            expected_revision: 0,
            draft: edited_draft(&paused_due, |draft| draft.enabled = false),
        },
    )?;
    if stopped_for_due.next_due_at.is_some() || stopped_for_due.pending.is_some() {
        bail!("pausing before the due time kept schedule state");
    }
    let deleted_due = save(
        &client,
        ScheduledTaskEdit::Create {
            draft: draft_with_prompt(project_id, format!("{prefix}-deleted"), 60, &deleted_marker),
        },
    )?;
    expect_ack(
        &client,
        Command::DeleteScheduledTask {
            id: deleted_due.id,
            expected_revision: 0,
        },
    )?;
    if list(&client)?.iter().any(|task| task.id == deleted_due.id) {
        bail!("a deleted 60 s task is still listed");
    }
    checks.ok("a paused and a deleted 60 s task are cleared before their due time");

    let manual_marker = format!("{prefix}-manual-prompt");
    let due_before_prompt_edit = resumed.next_due_at;
    let resumed = save(
        &client,
        ScheduledTaskEdit::Update {
            id: resumed.id,
            expected_revision: resumed.revision,
            draft: edited_draft(&resumed, |draft| draft.prompt = manual_marker.clone()),
        },
    )?;
    if resumed.next_due_at != due_before_prompt_edit {
        bail!("a prompt edit moved the next occurrence");
    }
    checks.ok("prompt edit preserves the next occurrence");

    while task_changes.try_recv().is_ok() {}
    let before_run = require(&list(&client)?, resumed.id)?;
    let due_before = before_run.next_due_at;
    let revision_before = before_run.revision;
    let started = now();
    let queued = run_now(&client, resumed.id)?;
    if queued.revision != revision_before {
        bail!("run now bumped the edit revision");
    }
    if queued.pending.is_none() {
        bail!("run now returned no pending occurrence");
    }
    let observed = require(&list(&client)?, resumed.id)?;
    let fact_race = match &observed.pending {
        Some(pending) if !pending.preparing => "queued",
        Some(_) => "preparing",
        None => "settled",
    };
    checks.fact("run_now_phase", fact_race);
    checks.ok(format!(
        "run now was observed in the {fact_race} phase and holds one occurrence"
    ));
    let settled = poll_task(&client, resumed.id, RUN_TIMEOUT, |task| {
        task.pending.is_none() && task.run_count >= 1
    })?;
    if settled.run_count != 1 {
        bail!("one manual run counted {} attempts", settled.run_count);
    }
    if settled.next_due_at != due_before {
        bail!("run now moved the scheduled due time");
    }
    if settled.revision != revision_before {
        bail!("settling a run bumped the edit revision");
    }
    let last = settled
        .last
        .as_ref()
        .context("a settled run has no record")?;
    if last.at < started {
        bail!("the dispatch record predates the run request");
    }
    let (session_a, submission_a) = require_accepted(&last.outcome, "the first manual run")?;
    checks.fact("manual_outcome", outcome_name(&last.outcome));
    checks.fact("manual_session_a", session_a.to_string());
    fixture.wait_main(&manual_marker, Duration::from_secs(30))?;
    if fixture.main_count(&manual_marker) != 1 {
        bail!(
            "the first manual occurrence produced {} provider requests",
            fixture.main_count(&manual_marker)
        );
    }
    task_changes
        .recv_timeout(Duration::from_secs(15))
        .context("no task-state notification followed the scheduled dispatch")?;
    if !load_sessions(&client)?.iter().any(|s| s.id == session_a) {
        bail!("the dispatched session is absent from LoadTaskState");
    }
    checks.ok(format!(
        "run now settles once as {}, reaches the fixture once, and the new session is visible without a save",
        outcome_name(&last.outcome)
    ));

    let queued_second = run_now(&client, resumed.id)?;
    if queued_second.pending.is_none() {
        bail!("a second run now returned no pending occurrence");
    }
    let settled_second = poll_task(&client, resumed.id, RUN_TIMEOUT, |task| {
        task.pending.is_none() && task.run_count >= 2
    })?;
    let second_last = settled_second
        .last
        .as_ref()
        .context("the second run has no record")?;
    let (session_b, submission_b) =
        require_accepted(&second_last.outcome, "the second manual run")?;
    checks.fact("manual_outcome_2", outcome_name(&second_last.outcome));
    if session_a == session_b {
        bail!("two new-session runs reused session {session_a}");
    }
    if submission_a == submission_b {
        bail!("two runs reused submission {submission_a}");
    }
    fixture.wait_main(&manual_marker, Duration::from_secs(30))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while fixture.main_count(&manual_marker) < 2 {
        if Instant::now() >= deadline {
            bail!("the second manual occurrence never reached the fixture");
        }
        thread::sleep(Duration::from_millis(100));
    }
    if fixture.main_count(&manual_marker) != 2 {
        bail!(
            "two occurrences produced {} provider requests",
            fixture.main_count(&manual_marker)
        );
    }
    let sessions = load_sessions(&client)?;
    if !sessions.iter().any(|s| s.id == session_b) {
        bail!("the second dispatched session is absent from LoadTaskState");
    }
    checks.fact("manual_session_b", session_b.to_string());
    checks.ok("two new-session occurrences created distinct sessions and prompts");

    let cancelling = save(
        &client,
        ScheduledTaskEdit::Create {
            draft: base_draft(project_id, format!("{prefix}-pause"), 7200),
        },
    )?;
    run_now(&client, cancelling.id)?;
    let stopped = save(
        &client,
        ScheduledTaskEdit::Update {
            id: cancelling.id,
            expected_revision: 0,
            draft: edited_draft(&cancelling, |draft| draft.enabled = false),
        },
    )?;
    let cleared = poll_task(&client, stopped.id, RUN_TIMEOUT, |task| {
        task.pending.is_none()
    })?;
    if cleared.enabled || cleared.next_due_at.is_some() {
        bail!("a paused task kept a schedule");
    }
    checks.ok("pause clears a queued or admitted manual run");

    let busy_marker = format!("{prefix}-busy-hold");
    let existing_marker = format!("{prefix}-existing-prompt");
    fixture.hold(&[busy_marker.as_str()])?;
    let busy_prompt = format!("Hold this turn open. {busy_marker}");
    let (busy_session, busy_record) = begin_session(project_id, &busy_prompt, FIXTURE_MODEL);
    let busy_runtime = Uuid::new_v4();
    accept_session(&client, &project, &busy_session, &busy_record)?;
    start_runtime(
        &client,
        busy_session.id,
        busy_runtime,
        &arguments.opencode_bin,
        arguments.data_root.join("workspace"),
    )?;
    dispatch_submission(
        &client,
        busy_session.id,
        busy_runtime,
        busy_record.receipt.id,
    )?;
    fixture.wait_main(&busy_marker, Duration::from_secs(30))?;
    wait_session(
        &client,
        busy_session.id,
        Duration::from_secs(20),
        |session| has_running_turn(session, busy_record.receipt.id),
    )?;
    checks.ok("a real OpenCode turn is held open on the busy session");

    let existing_task = save(
        &client,
        ScheduledTaskEdit::Create {
            draft: draft_for_target(
                format!("{prefix}-existing"),
                3600,
                &existing_marker,
                TaskTarget::ExistingSession {
                    session_id: busy_session.id,
                },
            ),
        },
    )?;
    run_now(&client, existing_task.id)?;
    let queued_existing = require(&list(&client)?, existing_task.id)?;
    let Some(pending) = queued_existing.pending.as_ref() else {
        bail!("the busy target's run-now occurrence is not pending");
    };
    if pending.preparing {
        bail!("a busy target's occurrence entered preparation");
    }
    expect_error(
        &client,
        Command::RunScheduledTaskNow {
            id: existing_task.id,
        },
        "pending",
        "a second run now while one is queued for a busy target",
    )?;
    let window_end = Instant::now() + TICK_WINDOW;
    while Instant::now() < window_end {
        let task = require(&list(&client)?, existing_task.id)?;
        match task.pending.as_ref() {
            Some(pending) if !pending.preparing => {}
            Some(_) => bail!("a busy target's occurrence entered preparation"),
            None => bail!("a busy target's queued occurrence disappeared"),
        }
        if fixture.main_count(&existing_marker) != 0 {
            bail!("a scheduled prompt reached the fixture while the target was busy");
        }
        let session = hydrate(&client, busy_session.id)?.context("the busy session disappeared")?;
        if !has_running_turn(&session, busy_record.receipt.id) {
            bail!("the busy target stopped being busy before the fixture was released");
        }
        thread::sleep(Duration::from_millis(250));
    }
    checks.ok(
        "a due existing-session run stays queued across a scheduler tick while the target is busy",
    );

    fixture.release()?;
    let existing_settled = poll_task(&client, existing_task.id, RUN_TIMEOUT, |task| {
        task.pending.is_none() && task.run_count >= 1
    })?;
    let existing_last = existing_settled
        .last
        .as_ref()
        .context("the existing-session run has no record")?;
    if existing_settled.run_count != 1 {
        bail!(
            "the existing-session occurrence counted {} runs",
            existing_settled.run_count
        );
    }
    let (existing_session, existing_submission) =
        require_accepted(&existing_last.outcome, "the existing-session run")?;
    checks.fact("existing_outcome", outcome_name(&existing_last.outcome));
    if existing_session != busy_session.id {
        bail!("the scheduled run used session {existing_session}, not the busy target");
    }
    fixture.wait_main(&existing_marker, Duration::from_secs(30))?;
    if fixture.main_count(&existing_marker) != 1 {
        bail!(
            "the existing-session occurrence produced {} provider requests",
            fixture.main_count(&existing_marker)
        );
    }
    if fixture.main_count(&busy_marker) != 1 {
        bail!(
            "the busy turn produced {} provider requests",
            fixture.main_count(&busy_marker)
        );
    }
    let hydrated = hydrate(&client, busy_session.id)?
        .context("the busy session disappeared after its scheduled run")?;
    let last_turn = hydrated
        .turns
        .last()
        .context("the existing session has no turns")?;
    if last_turn.id != existing_submission {
        bail!(
            "the scheduled submission {} is not the session's last turn {}",
            existing_submission,
            last_turn.id
        );
    }
    if !hydrated
        .messages
        .iter()
        .rev()
        .find(|message| message.role == fintwind_protocol::model::MessageRole::User)
        .is_some_and(|message| message.content.contains(&existing_marker))
    {
        bail!("the existing session's last user message is not the scheduled prompt");
    }
    let busy_turn = hydrated
        .turns
        .iter()
        .find(|turn| turn.id == busy_record.receipt.id)
        .context("the busy turn is missing from the existing session")?;
    if busy_turn.status != TurnStatus::Completed {
        bail!(
            "the busy turn ended as {:?} before the scheduled run",
            busy_turn.status
        );
    }
    let busy_completed = busy_turn
        .completed_at
        .context("the busy turn has no completion time")?;
    if last_turn.started_at < busy_completed {
        bail!("the scheduled turn started before the busy turn completed");
    }
    let entries = fixture.entries();
    let busy_request = entries
        .iter()
        .find(|entry| {
            entry.kind == "request" && entry.tools > 0 && entry.prompt.contains(&busy_marker)
        })
        .context("the busy request is missing from the fixture log")?;
    let busy_reply = entries
        .iter()
        .find(|entry| {
            entry.kind == "response"
                && entry.tools > 0
                && entry.held
                && entry.prompt.contains(&busy_marker)
        })
        .context("the busy reply is missing from the fixture log")?;
    let scheduled_request = entries
        .iter()
        .find(|entry| {
            entry.kind == "request" && entry.tools > 0 && entry.prompt.contains(&existing_marker)
        })
        .context("the scheduled request is missing from the fixture log")?;
    if !(busy_request.seq < busy_reply.seq && busy_reply.seq < scheduled_request.seq) {
        bail!(
            "the scheduled prompt did not follow the busy reply ({} {} {})",
            busy_request.seq,
            busy_reply.seq,
            scheduled_request.seq
        );
    }
    checks.fact("busy_prompt_seq", busy_request.seq);
    checks.fact("busy_reply_seq", busy_reply.seq);
    checks.fact("scheduled_after_busy_seq", scheduled_request.seq);
    checks.ok("the busy turn completed before the queued scheduled prompt was dispatched");

    let auto_settled = poll_task(&client, auto.id, AUTO_TIMEOUT, |task| {
        task.pending.is_none() && task.run_count >= 1
    })?;
    let auto_due = auto
        .next_due_at
        .context("the 60 s task had no due time at creation")?;
    let auto_last = auto_settled
        .last
        .as_ref()
        .context("the automatic occurrence has no record")?;
    if auto_last.due_at != auto_due {
        bail!(
            "the automatic record names due {} instead of {auto_due}",
            auto_last.due_at
        );
    }
    if auto_last.at < auto_due {
        bail!(
            "the automatic run at {} predates its due time {auto_due}",
            auto_last.at
        );
    }
    if auto_settled.run_count != 1 {
        bail!(
            "one automatic occurrence counted {} runs",
            auto_settled.run_count
        );
    }
    match auto_settled.next_due_at {
        Some(next) if next == auto_last.at + 60 => {}
        other => bail!(
            "the 60 s task scheduled {other:?} after settling at {}",
            auto_last.at
        ),
    }
    let (auto_session, _) = require_accepted(&auto_last.outcome, "the automatic run")?;
    checks.fact("auto_outcome", outcome_name(&auto_last.outcome));
    fixture.wait_main(&auto_marker, Duration::from_secs(30))?;
    if fixture.main_count(&auto_marker) != 1 {
        bail!(
            "the automatic occurrence produced {} provider requests",
            fixture.main_count(&auto_marker)
        );
    }
    let paused_now = require(&list(&client)?, stopped_for_due.id)?;
    if paused_now.run_count != 0 || paused_now.last.is_some() || paused_now.next_due_at.is_some() {
        bail!("a paused 60 s task ran or kept a schedule");
    }
    if list(&client)?.iter().any(|task| task.id == deleted_due.id) {
        bail!("a deleted 60 s task reappeared");
    }
    if fixture.main_count(&paused_marker) != 0 || fixture.main_count(&deleted_marker) != 0 {
        bail!("a paused or deleted 60 s task reached the fixture");
    }
    let stopped_auto = save(
        &client,
        ScheduledTaskEdit::Update {
            id: auto.id,
            expected_revision: auto_settled.revision,
            draft: edited_draft(&auto_settled, |draft| draft.enabled = false),
        },
    )?;
    if stopped_auto.pending.is_some() || stopped_auto.next_due_at.is_some() {
        bail!("pausing the automatic task left pending or due state");
    }
    checks.fact("auto_due", auto_due);
    checks.fact("auto_settled_at", auto_last.at);
    checks.fact("auto_session", auto_session.to_string());
    checks.ok("a real 60 s interval dispatched once at its due time and the paused and deleted tasks stayed silent");

    let mut manual_before = require(&list(&client)?, resumed.id)?;
    let manual_prompt_count = fixture.main_count(&manual_marker);
    while first_changes.try_recv().is_ok() {}
    while second_changes.try_recv().is_ok() {}
    manual_before = save(
        &client,
        ScheduledTaskEdit::Update {
            id: manual_before.id,
            expected_revision: manual_before.revision,
            draft: edited_draft(&manual_before, |draft| {
                draft.title = format!("{prefix}-notified");
            }),
        },
    )?;
    first_changes
        .recv_timeout(Duration::from_secs(5))
        .context("the first client heard no change for the notification edit")?;
    second_changes
        .recv_timeout(Duration::from_secs(5))
        .context("the second client heard no change for the notification edit")?;
    checks.ok("both clients receive ScheduledTasksChanged");

    second.disconnect();
    client.shutdown();
    if !wait_listener_closed(&address, SHUTDOWN_TIMEOUT) {
        bail!("the supplied daemon kept listening after a graceful shutdown");
    }
    checks.ok("the supplied daemon stopped through its own shutdown");

    let (mut daemon, restarted, restarted_address) =
        start_owned_daemon(arguments, &token, Duration::from_secs(60))?;
    checks.ok(format!("own daemon ready at {restarted_address}"));
    let manual_after = require(&list(&restarted)?, resumed.id)?;
    if manual_after.run_count != manual_before.run_count
        || manual_after.revision != manual_before.revision
        || manual_after.next_due_at != manual_before.next_due_at
        || !same_dispatch(manual_after.last.as_ref(), manual_before.last.as_ref())
    {
        bail!("a settled manual task changed across the restart");
    }
    if fixture.main_count(&manual_marker) != manual_prompt_count {
        bail!("the restart resent a settled manual occurrence");
    }
    let probe_marker = format!("{prefix}-restart-probe");
    let probe = save(
        &restarted,
        ScheduledTaskEdit::Create {
            draft: draft_with_prompt(
                project_id,
                format!("{prefix}-restart-probe"),
                3600,
                &probe_marker,
            ),
        },
    )?;
    run_now(&restarted, probe.id)?;
    let probe_settled = poll_task(&restarted, probe.id, RUN_TIMEOUT, |task| {
        task.pending.is_none() && task.run_count >= 1
    })?;
    if probe_settled.run_count != 1 {
        bail!("the restart probe counted {} runs", probe_settled.run_count);
    }
    require_dispatched(
        &probe_settled
            .last
            .as_ref()
            .context("the restart probe has no record")?
            .outcome,
    )?;
    fixture.wait_main(&probe_marker, Duration::from_secs(30))?;
    if fixture.main_count(&manual_marker) != manual_prompt_count {
        bail!("a later tick resent a settled manual occurrence");
    }
    let manual_outcome = manual_after
        .last
        .as_ref()
        .context("the settled manual task lost its record")?;
    require_dispatched(&manual_outcome.outcome)?;
    checks.fact(
        "restart_manual_outcome",
        outcome_name(&manual_outcome.outcome),
    );
    checks.ok(
        "a graceful restart keeps settled records and resends no Accepted or Unconfirmed input",
    );

    restarted.shutdown();
    drop(restarted);
    // The supplied daemon proved the same lifecycle needs its full
    // SHUTDOWN_TIMEOUT before the listener closes; the own daemon gets the same
    // window for the same reason. Acceptance is observed separately so a lost
    // shutdown request cannot be reported as a slow teardown.
    if !wait_listener_closed(&restarted_address, SHUTDOWN_TIMEOUT) {
        bail!(
            "the own daemon at {restarted_address} kept accepting connections for {}s after its shutdown (pid {})",
            SHUTDOWN_TIMEOUT.as_secs(),
            daemon.pid()
        );
    }
    let reaping = Instant::now();
    match daemon.wait_for_exit(Duration::from_secs(15)) {
        Ok(true) => {}
        Ok(false) => bail!(
            "the own daemon stopped accepting connections but its process did not exit within {}s (pid {}, waited {:?} after the listener closed)",
            15,
            daemon.pid(),
            reaping.elapsed()
        ),
        Err(error) => {
            bail!("the own daemon's exit could not be observed after its listener closed: {error}")
        }
    }
    checks.ok("own daemon reaped after its shutdown");

    Ok(())
}

fn protocol_checks(checks: &mut Checks) -> anyhow::Result<()> {
    if (MIN_INTERVAL_SECS, MAX_INTERVAL_SECS) != (60, 31_536_000) {
        bail!("the frozen interval bounds changed");
    }
    let minute = Recurrence::Every { interval_secs: 60 };
    let hour = Recurrence::Every {
        interval_secs: 3600,
    };
    let longest = Recurrence::Every {
        interval_secs: 31_536_000,
    };
    if minute.next_due(1_000_000).ok() != Some(1_000_060) {
        bail!("one minute after 1_000_000 is not 1_000_060");
    }
    if hour.next_due(1_700_000_000).ok() != Some(1_700_003_600) {
        bail!("one hour after 1_700_000_000 is not 1_700_003_600");
    }
    if longest.next_due(0).ok() != Some(31_536_000) {
        bail!("the maximum interval does not add 365 days");
    }
    let short = Recurrence::Every { interval_secs: 59 };
    let zero = Recurrence::Every { interval_secs: 0 };
    let over = Recurrence::Every {
        interval_secs: 31_536_001,
    };
    if minute.validate().is_err()
        || short.validate().is_ok()
        || zero.validate().is_ok()
        || over.validate().is_ok()
    {
        bail!("an interval was accepted or rejected at the wrong bound");
    }
    if ClockTime::new(23, 59).map(ClockTime::minutes).ok() != Some(1439)
        || ClockTime::new(0, 0).map(ClockTime::hour).ok() != Some(0)
        || ClockTime::new(24, 0).is_ok()
        || ClockTime::new(0, 60).is_ok()
        || Weekdays::new(127).map(Weekdays::bits).ok() != Some(127)
        || Weekdays::new(1).map(Weekdays::bits).ok() != Some(1)
        || Weekdays::new(0).is_ok()
        || Weekdays::new(128).is_ok()
    {
        bail!("clock time or weekday bounds are wrong");
    }

    let wire = serde_json::to_string(&minute)?;
    if wire != r#"{"kind":"every","intervalSecs":60}"# {
        bail!("the interval wire shape is {wire}");
    }
    if serde_json::from_str::<Recurrence>(&wire)? != minute {
        bail!("the interval wire round-trip changed the value");
    }
    let at = ClockTime::new(1, 30).map_err(anyhow::Error::msg)?;
    let weekdays = Weekdays::new(5).map_err(anyhow::Error::msg)?;
    let at_time = Recurrence::AtTime { at, weekdays };
    let wire = serde_json::to_string(&at_time)?;
    if wire != r#"{"kind":"atTime","at":90,"weekdays":5}"# {
        bail!("the fixed-time wire shape is {wire}");
    }
    if serde_json::from_str::<Recurrence>(&wire)? != at_time {
        bail!("the fixed-time wire round-trip changed the value");
    }
    for rejected in [
        r#"{"kind":"every","intervalSecs":59}"#,
        r#"{"kind":"every","intervalSecs":0}"#,
        r#"{"kind":"atTime","at":1440,"weekdays":1}"#,
        r#"{"kind":"atTime","at":60,"weekdays":0}"#,
        r#"{"kind":"atTime","at":60,"weekdays":128}"#,
    ] {
        if serde_json::from_str::<Recurrence>(rejected).is_ok() {
            bail!("the wire accepted {rejected}");
        }
    }

    let anchor = 1_800_000_000;
    let daily_at = ClockTime::new(3, 30).map_err(anyhow::Error::msg)?;
    let daily = Recurrence::AtTime {
        at: daily_at,
        weekdays: Weekdays::every_day(),
    };
    let first = daily.next_due(anchor).map_err(anyhow::Error::msg)?;
    if first <= anchor || first % 60 != 0 || first - anchor > 15 * 86_400 {
        bail!("a daily fixed time landed {first} from {anchor}");
    }
    let next = daily.next_due(first).map_err(anyhow::Error::msg)?;
    if !(82_800..=90_000).contains(&next.saturating_sub(first)) {
        bail!("a daily fixed time repeated after {} s", next - first);
    }
    if daily.next_due(first - 1).map_err(anyhow::Error::msg)? != first {
        bail!("a daily fixed time is not stable just before its occurrence");
    }
    let weekly_at = ClockTime::new(9, 15).map_err(anyhow::Error::msg)?;
    let weekly_days = Weekdays::new(1).map_err(anyhow::Error::msg)?;
    let weekly = Recurrence::AtTime {
        at: weekly_at,
        weekdays: weekly_days,
    };
    let first = weekly.next_due(anchor).map_err(anyhow::Error::msg)?;
    let next = weekly.next_due(first).map_err(anyhow::Error::msg)?;
    if first <= anchor || first % 60 != 0 || !(6 * 86_400..=8 * 86_400).contains(&(next - first)) {
        bail!("a Sunday-only fixed time repeated after {} s", next - first);
    }
    if weekly.next_due(first - 1).map_err(anyhow::Error::msg)? != first {
        bail!("a weekly fixed time is not stable just before its occurrence");
    }

    checks.ok("recurrence arithmetic, bounds and wire shapes hold");
    Ok(())
}

const FIXTURE_MODEL: &str = "fixture/fixture-model";

fn base_draft(project_id: Uuid, title: String, interval_secs: u32) -> ScheduledTaskDraft {
    draft_with_prompt(
        project_id,
        title,
        interval_secs,
        "summarize the repository state",
    )
}

fn draft_with_prompt(
    project_id: Uuid,
    title: String,
    interval_secs: u32,
    prompt: &str,
) -> ScheduledTaskDraft {
    draft_for_target(
        title,
        interval_secs,
        prompt,
        TaskTarget::NewSession {
            project_id,
            launch: launch(),
            workspace: SessionWorkspace::Local,
        },
    )
}

fn draft_for_target(
    title: String,
    interval_secs: u32,
    prompt: &str,
    target: TaskTarget,
) -> ScheduledTaskDraft {
    ScheduledTaskDraft {
        title,
        prompt: prompt.to_owned(),
        enabled: true,
        schedule: Recurrence::Every { interval_secs },
        target,
    }
}

fn launch() -> SessionLaunch {
    SessionLaunch {
        // The isolated OpenCode config names this provider and model; a fixture
        // name here keeps any accidental real-provider use impossible.
        model: FIXTURE_MODEL.into(),
        runtime_mode: RuntimeMode::Ask,
        interaction_mode: InteractionMode::Build,
        reasoning_effort: None,
        service_tier: None,
        context_window: None,
        agent_preset: None,
    }
}

fn edited_draft(
    task: &ScheduledTask,
    change: impl FnOnce(&mut ScheduledTaskDraft),
) -> ScheduledTaskDraft {
    let mut draft = ScheduledTaskDraft {
        title: task.title.clone(),
        prompt: task.prompt.clone(),
        enabled: task.enabled,
        schedule: task.schedule,
        target: task.target.clone(),
    };
    change(&mut draft);
    draft
}

fn validation_refusals(
    client: &DaemonClient,
    second: &DaemonClient,
    project_id: Uuid,
    prefix: &str,
) -> anyhow::Result<()> {
    let mut cases: Vec<(&str, ScheduledTaskDraft, bool)> = Vec::new();

    let mut draft = base_draft(project_id, format!("{prefix}-reject-title"), 600);
    draft.title = "   ".into();
    cases.push(("a blank title", draft, false));

    let mut draft = base_draft(project_id, format!("{prefix}-reject-prompt"), 600);
    draft.prompt = String::new();
    cases.push(("a blank prompt", draft, false));

    cases.push((
        "a 59 s interval",
        base_draft(project_id, format!("{prefix}-reject-short"), 59),
        false,
    ));
    cases.push((
        "a zero interval",
        base_draft(project_id, format!("{prefix}-reject-zero"), 0),
        false,
    ));
    cases.push((
        "a 365 d + 1 s interval",
        base_draft(project_id, format!("{prefix}-reject-long"), 31_536_001),
        false,
    ));

    let mut draft = base_draft(project_id, format!("{prefix}-reject-worktree"), 600);
    draft.target = TaskTarget::NewSession {
        project_id,
        launch: launch(),
        workspace: SessionWorkspace::NewWorktree { base_branch: None },
    };
    cases.push(("a new-worktree target", draft, false));

    let mut draft = base_draft(project_id, format!("{prefix}-reject-project"), 600);
    draft.target = TaskTarget::NewSession {
        project_id: Uuid::new_v4(),
        launch: launch(),
        workspace: SessionWorkspace::Local,
    };
    cases.push(("an unknown project", draft, false));

    let mut draft = base_draft(project_id, format!("{prefix}-reject-session"), 600);
    draft.target = TaskTarget::ExistingSession {
        session_id: Uuid::new_v4(),
    };
    cases.push(("an unknown session", draft, true));

    for (name, draft, on_second) in cases {
        let target = if on_second { second } else { client };
        expect_error(
            target,
            Command::SaveScheduledTask {
                edit: ScheduledTaskEdit::Create { draft },
            },
            "",
            name,
        )?;
    }
    Ok(())
}

fn setup_project(client: &DaemonClient, arguments: &Arguments) -> anyhow::Result<Project> {
    let workspace = arguments.data_root.join("workspace");
    std::fs::create_dir_all(&workspace).context("could not create the task workspace")?;
    let project = Project {
        id: Uuid::new_v4(),
        name: "scheduled e2e".into(),
        path: workspace,
        created_at: now(),
    };
    client
        .request(
            Uuid::nil(),
            Uuid::nil(),
            Command::SaveTaskState {
                projects: vec![project.clone()],
                live_session_ids: Vec::new(),
                sessions: Vec::new(),
            },
        )
        .context("the daemon refused the isolated project")?;
    Ok(project)
}

fn list(client: &DaemonClient) -> anyhow::Result<Vec<ScheduledTask>> {
    match client.request(Uuid::nil(), Uuid::nil(), Command::ListScheduledTasks)? {
        ResponsePayload::ScheduledTasks { tasks } => Ok(tasks),
        _ => bail!("ListScheduledTasks answered with the wrong payload"),
    }
}

fn save(client: &DaemonClient, edit: ScheduledTaskEdit) -> anyhow::Result<ScheduledTask> {
    match client.request(
        Uuid::nil(),
        Uuid::nil(),
        Command::SaveScheduledTask { edit },
    )? {
        ResponsePayload::ScheduledTaskSaved { task } => Ok(task),
        _ => bail!("SaveScheduledTask answered with the wrong payload"),
    }
}

fn run_now(client: &DaemonClient, id: ScheduledTaskId) -> anyhow::Result<ScheduledTask> {
    match client.request(
        Uuid::nil(),
        Uuid::nil(),
        Command::RunScheduledTaskNow { id },
    )? {
        ResponsePayload::ScheduledTaskSaved { task } => Ok(task),
        _ => bail!("RunScheduledTaskNow answered with the wrong payload"),
    }
}

fn expect_ack(client: &DaemonClient, command: Command) -> anyhow::Result<()> {
    match client.request(Uuid::nil(), Uuid::nil(), command)? {
        ResponsePayload::Ack => Ok(()),
        _ => bail!("the daemon answered a delete without an acknowledgement"),
    }
}

fn expect_error(
    client: &DaemonClient,
    command: Command,
    needle: &str,
    what: &str,
) -> anyhow::Result<()> {
    match client.request(Uuid::nil(), Uuid::nil(), command) {
        Ok(_) => bail!("{what} was accepted"),
        Err(error) => {
            let text = format!("{error:#}");
            if !needle.is_empty() && !text.contains(needle) {
                bail!("{what} failed with an unexpected message: {text}");
            }
            println!("PASS {what}: refused with {text}");
            Ok(())
        }
    }
}

fn require(tasks: &[ScheduledTask], id: ScheduledTaskId) -> anyhow::Result<ScheduledTask> {
    tasks
        .iter()
        .find(|task| task.id == id)
        .cloned()
        .context("the task is missing from the list")
}

fn count_prefixed(tasks: &[ScheduledTask], prefix: &str) -> usize {
    tasks
        .iter()
        .filter(|task| task.title.starts_with(prefix))
        .count()
}

fn poll_task(
    client: &DaemonClient,
    id: ScheduledTaskId,
    timeout: Duration,
    mut done: impl FnMut(&ScheduledTask) -> bool,
) -> anyhow::Result<ScheduledTask> {
    let deadline = Instant::now() + timeout;
    loop {
        let tasks = list(client)?;
        if let Some(task) = tasks.iter().find(|task| task.id == id)
            && done(task)
        {
            return Ok(task.clone());
        }
        if Instant::now() >= deadline {
            bail!("the task did not reach the expected state within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

#[derive(Clone)]
struct FixtureEntry {
    seq: u64,
    kind: String,
    prompt: String,
    tools: u64,
    held: bool,
}

/// The loopback provider's JSONL log and its hold/release control endpoint.
struct Fixture {
    origin: String,
    log: PathBuf,
}

impl Fixture {
    fn new(origin: &str, log: &Path) -> Self {
        Self {
            origin: origin.trim_end_matches('/').to_owned(),
            log: log.to_path_buf(),
        }
    }

    fn entries(&self) -> Vec<FixtureEntry> {
        let Ok(text) = std::fs::read_to_string(&self.log) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|value| {
                Some(FixtureEntry {
                    seq: value.get("seq")?.as_u64()?,
                    kind: value.get("kind")?.as_str()?.to_owned(),
                    prompt: value.get("prompt")?.as_str()?.to_owned(),
                    tools: value.get("tools")?.as_u64()?,
                    held: value
                        .get("held")
                        .and_then(|held| held.as_bool())
                        .unwrap_or(false),
                })
            })
            .collect()
    }

    fn main_count(&self, marker: &str) -> usize {
        self.entries()
            .iter()
            .filter(|entry| {
                entry.kind == "request" && entry.tools > 0 && entry.prompt.contains(marker)
            })
            .count()
    }

    fn wait_main(&self, marker: &str, timeout: Duration) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.main_count(marker) > 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("no main provider request carried the marker {marker:?} within {timeout:?}");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn hold(&self, markers: &[&str]) -> anyhow::Result<()> {
        self.post("/__fixture/hold", serde_json::json!({ "markers": markers }))
    }

    fn release(&self) -> anyhow::Result<()> {
        self.post("/__fixture/release", serde_json::json!({}))
    }

    fn post(&self, path: &str, body: serde_json::Value) -> anyhow::Result<()> {
        let rest = self
            .origin
            .strip_prefix("http://")
            .context("the fixture URL is not plain HTTP")?;
        let address: SocketAddr = rest
            .parse()
            .with_context(|| format!("the fixture URL is not loopback host:port: {rest}"))?;
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
            .context("could not reach the fixture control endpoint")?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let payload = serde_json::to_vec(&body)?;
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: {rest}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        )?;
        stream.write_all(&payload)?;
        stream.flush()?;
        let mut response = String::new();
        stream
            .take(64 * 1024)
            .read_to_string(&mut response)
            .context("the fixture control endpoint did not answer")?;
        if !(response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200")) {
            bail!("the fixture refused {path}: {response}");
        }
        Ok(())
    }
}

fn begin_session(project_id: Uuid, prompt: &str, model: &str) -> (AgentSession, SubmissionRecord) {
    let mut session = AgentSession::new(project_id);
    session.model = Some(model.to_owned());
    session.runtime_mode = RuntimeMode::Ask;
    session.interaction_mode = InteractionMode::Build;
    let turn_id = Uuid::new_v4();
    session.begin_turn_with_id(turn_id, prompt.to_owned());
    let record = SubmissionRecord::new(session.id, turn_id, prompt.to_owned(), Vec::new());
    if let Some(turn) = session.turns.last_mut() {
        turn.submission = Some(record.receipt.clone());
    }
    (session, record)
}

fn accept_session(
    client: &DaemonClient,
    project: &Project,
    session: &AgentSession,
    record: &SubmissionRecord,
) -> anyhow::Result<()> {
    match client.request(
        session.id,
        Uuid::nil(),
        Command::AcceptSubmission {
            project: project.clone(),
            session: Box::new(session.clone()),
            submission: Box::new(record.clone()),
        },
    )? {
        ResponsePayload::SubmissionSaved { receipt }
            if receipt.state == SubmissionState::Preparing =>
        {
            Ok(())
        }
        other => bail!("the daemon did not admit the manual session: {other:?}"),
    }
}

fn start_runtime(
    client: &DaemonClient,
    session_id: Uuid,
    runtime_id: Uuid,
    binary: &Path,
    cwd: PathBuf,
) -> anyhow::Result<()> {
    let options = WireDriverStartOptions {
        binary: binary.to_path_buf(),
        cwd,
        mode: "ask".into(),
        interaction_mode: "build".into(),
        model: Some(FIXTURE_MODEL.into()),
        reasoning_effort: None,
        service_tier: None,
        context_window: None,
        agent_preset: None,
        provider_cursor: None,
        mermaid_prompt_version: None,
    };
    match client.request(session_id, runtime_id, Command::Start { options })? {
        ResponsePayload::Started { .. } => Ok(()),
        other => bail!("the daemon refused the manual runtime start: {other:?}"),
    }
}

fn dispatch_submission(
    client: &DaemonClient,
    session_id: Uuid,
    runtime_id: Uuid,
    submission_id: Uuid,
) -> anyhow::Result<()> {
    match client.request(
        session_id,
        runtime_id,
        Command::DispatchSubmission {
            submission_id,
            workspace: SessionWorkspace::Local,
        },
    )? {
        ResponsePayload::SubmissionSaved { .. } => Ok(()),
        other => bail!("the daemon refused the manual dispatch: {other:?}"),
    }
}

fn load_sessions(client: &DaemonClient) -> anyhow::Result<Vec<AgentSession>> {
    match client.request(Uuid::nil(), Uuid::nil(), Command::LoadTaskState)? {
        ResponsePayload::TaskState { sessions, .. } => Ok(sessions),
        other => bail!("LoadTaskState answered with {other:?}"),
    }
}

fn hydrate(client: &DaemonClient, session_id: Uuid) -> anyhow::Result<Option<AgentSession>> {
    match client.request(
        Uuid::nil(),
        Uuid::nil(),
        Command::HydrateSession { session_id },
    )? {
        ResponsePayload::Session { session } => Ok(session),
        other => bail!("HydrateSession answered with {other:?}"),
    }
}

fn has_running_turn(session: &AgentSession, turn_id: Uuid) -> bool {
    session
        .turns
        .iter()
        .any(|turn| turn.id == turn_id && turn.status == TurnStatus::Running)
}

fn wait_session(
    client: &DaemonClient,
    session_id: Uuid,
    timeout: Duration,
    done: impl Fn(&AgentSession) -> bool,
) -> anyhow::Result<AgentSession> {
    let deadline = Instant::now() + timeout;
    loop {
        let session = hydrate(client, session_id)?.context("the session disappeared")?;
        if done(&session) {
            return Ok(session);
        }
        if Instant::now() >= deadline {
            bail!("session {session_id} did not reach the expected state within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(150));
    }
}

fn require_dispatched(outcome: &DispatchOutcome) -> anyhow::Result<(Uuid, Uuid)> {
    match outcome {
        DispatchOutcome::Accepted {
            session_id,
            submission_id,
        }
        | DispatchOutcome::Unconfirmed {
            session_id,
            submission_id,
        } => Ok((*session_id, *submission_id)),
        DispatchOutcome::NotSent { reason, .. } => bail!("the dispatch was not sent: {reason}"),
        DispatchOutcome::Rejected { reason } => bail!("the dispatch was rejected: {reason}"),
        DispatchOutcome::Missed => bail!("the dispatch was recorded as missed"),
    }
}

/// A dispatch that the daemon confirmed into a working fixture session must
/// settle as `Accepted`. `Unconfirmed` here means a confirmed dispatch was
/// recorded as if its outcome were unknown.
fn require_accepted(outcome: &DispatchOutcome, label: &str) -> anyhow::Result<(Uuid, Uuid)> {
    match outcome {
        DispatchOutcome::Accepted {
            session_id,
            submission_id,
        } => Ok((*session_id, *submission_id)),
        DispatchOutcome::Unconfirmed { .. } => {
            bail!("{label} was confirmed by the daemon but recorded as unconfirmed")
        }
        DispatchOutcome::NotSent { reason, .. } => bail!("{label} was not sent: {reason}"),
        DispatchOutcome::Rejected { reason } => bail!("{label} was rejected: {reason}"),
        DispatchOutcome::Missed => bail!("{label} was recorded as missed"),
    }
}

fn same_dispatch(
    left: Option<&fintwind_protocol::scheduled_task::DispatchRecord>,
    right: Option<&fintwind_protocol::scheduled_task::DispatchRecord>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.due_at == right.due_at
                && left.at == right.at
                && serde_json::to_value(&left.outcome).ok()
                    == serde_json::to_value(&right.outcome).ok()
        }
        _ => false,
    }
}

fn start_owned_daemon(
    arguments: &Arguments,
    token: &str,
    timeout: Duration,
) -> anyhow::Result<(OwnedDaemon, DaemonClient, String)> {
    let deadline = Instant::now() + timeout;
    loop {
        let failure = match spawn_daemon(arguments, token) {
            Ok((daemon, address)) => match connect(&address, token) {
                Ok(client) => match list(&client) {
                    Ok(_) => return Ok((daemon, client, address)),
                    Err(error) => {
                        client.disconnect();
                        format!("{error:#}")
                    }
                },
                Err(error) => format!("{error:#}"),
            },
            Err(error) => format!("{error:#}"),
        };
        if Instant::now() >= deadline {
            bail!("the replacement daemon did not serve within {timeout:?}: {failure}");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn outcome_name(outcome: &DispatchOutcome) -> &'static str {
    match outcome {
        DispatchOutcome::Accepted { .. } => "accepted",
        DispatchOutcome::Unconfirmed { .. } => "unconfirmed",
        DispatchOutcome::NotSent { .. } => "not sent",
        DispatchOutcome::Rejected { .. } => "rejected",
        DispatchOutcome::Missed => "missed",
    }
}

fn connect(address: &str, token: &str) -> anyhow::Result<DaemonClient> {
    let (sender, receiver) = mpsc::channel();
    let address = address.to_owned();
    let token = token.to_owned();
    thread::spawn(move || {
        let _ = sender.send(DaemonClient::connect(&address, token));
    });
    receiver
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .map_err(|_| anyhow::anyhow!("the handshake did not finish within {HANDSHAKE_TIMEOUT:?}"))?
        .context("the daemon handshake failed")
}

fn wait_listener_closed(address: &str, timeout: Duration) -> bool {
    let Ok(target) = address.parse::<SocketAddr>() else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&target, Duration::from_millis(300)).is_err() {
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }
    false
}

struct OwnedDaemon {
    child: Child,
}

impl OwnedDaemon {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> std::io::Result<bool> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(false)
    }
}

impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_daemon(arguments: &Arguments, token: &str) -> anyhow::Result<(OwnedDaemon, String)> {
    let mut child = StdCommand::new(&arguments.daemon_bin)
        .args(["--bind", "127.0.0.1:0", "--parent-pid"])
        .arg(std::process::id().to_string())
        .arg("--data-dir")
        .arg(&arguments.data_root)
        .env(DAEMON_TOKEN_ENV, token)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("could not start the daemon from --daemon-bin")?;
    let stdout = child.stdout.take().context("the daemon stdout was piped")?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let line = std::io::BufReader::new(stdout)
            .lines()
            .next()
            .and_then(|line| line.ok());
        let _ = sender.send(line);
    });
    let mut daemon = OwnedDaemon { child };
    match receiver.recv_timeout(READY_TIMEOUT) {
        Ok(Some(line)) => {
            let ready: DaemonReady = serde_json::from_str(&line)
                .with_context(|| format!("the readiness line is not a DaemonReady: {line}"))?;
            if ready.pid != daemon.pid() {
                bail!(
                    "the readiness line names pid {} but the child is {}",
                    ready.pid,
                    daemon.pid()
                );
            }
            Ok((daemon, ready.address))
        }
        Ok(None) => {
            let _ = daemon.wait_for_exit(Duration::from_secs(5));
            bail!("the daemon closed stdout before announcing readiness")
        }
        Err(_) => {
            let _ = daemon.wait_for_exit(Duration::from_secs(5));
            bail!("no readiness line within {READY_TIMEOUT:?}")
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn write_proof(path: &Path, passed: bool, checks: &Checks, error: Option<&str>) {
    let proof = serde_json::json!({
        "passed": passed,
        "count": checks.passed.len(),
        "checks": &checks.passed,
        "facts": &checks.facts,
        "error": error,
    });
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(path, serde_json::to_vec_pretty(&proof).unwrap_or_default()));
    if let Err(write_error) = written {
        eprintln!(
            "scheduled_tasks_e2e: could not write {}: {write_error}",
            path.display()
        );
    }
}

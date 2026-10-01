//! Behavioural end-to-end fault injection for OpenCode private-serve recovery.
//!
//! These tests drive the *real* chain — a `fintwind-core` daemon `Backend`
//! (`FintwindBackend`) behind the real `serve()` WebSocket endpoint, a real
//! `fintwind-client::DaemonClient`, the real OpenCode driver, and the real
//! model discovery — against a local fake `opencode serve` implemented by
//! `scripts/fixtures/opencode-recovery/fake_opencode.py`. Nothing here reaches
//! the network, starts the real public OpenCode service, or sends a model
//! request.
//!
//! Each case is a behavioural acceptance from `docs/opencode-recovery.md`, not
//! a source-string check. The driver's private serve, its per-request Basic
//! auth, and the driver/`Backend`/`StateStore` interfaces are unchanged; the
//! driver reconciliation is under construction, so several cases are expected
//! to fail until it lands (fail-first). Artifacts (scenario pass/fail, request
//! counts, event timelines) are written to `temp/recovery-e2e/<uuid>/`.
//!
//! A deliberately long-delay reconciliation case is `#[ignore]`d and run
//! separately: `cargo test -p fintwind-core --test opencode_recovery -- --ignored`.
//!
//! Windows-only: the fake provider binary is a `.cmd` shim spawned the same
//! way Fintwind runs an npm-installed `opencode` (through `cmd.exe`), matching
//! the product's Windows target.

#![cfg(windows)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use serde_json::{Value, json};
use uuid::Uuid;

use fintwind_client::DaemonClient;
use fintwind_core::composer_complete::{SlashCommand, expanded_submission};
use fintwind_core::daemon::FintwindBackend;
use fintwind_core::model::{
    ActivityItem, ActivityKind, AgentSession, AgentTurn, Message, MessageRole, TurnStatus,
};
use fintwind_core::persistence::StateStore;
use fintwind_core::settings::DaemonSettingsStore;
use fintwind_core::{
    Command, PromptFile, ResponsePayload, SequencedEvent, ServerOptions, WireDriverStartOptions,
    serve,
};
use fintwind_protocol::composer::CommandScope;
use fintwind_protocol::model::{MessageAttachment, TranscriptBlock};
use fintwind_protocol::provider_session::NativeTranscript;

const DAEMON_TOKEN: &str = "opencode-recovery-e2e-token";

#[test]
#[ignore = "owns a private daemon/pool; run separately from the recovery matrix"]
fn opencode_session_moves() {
    let run = Run::new();
    let source = run.workspace("move-source");
    let destination = run.root.join("move-worktree");
    let plain = run.workspace("move-plain");
    // Temp artifacts live inside the checkout: without its own repository,
    // this directory would inherit the harness's enclosing Git worktree.
    assert!(
        std::process::Command::new("git")
            .args(["init", "-b", "plain"])
            .current_dir(&plain)
            .output()
            .unwrap()
            .status
            .success()
    );
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=E2E",
            "-c",
            "user.email=e2e@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
        vec![
            "worktree",
            "add",
            "-b",
            "work/move",
            destination.to_str().unwrap(),
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&source)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let row = |id: &str| {
        json!({
            "id": id, "title": id,
            "time": {"created": 1700000000000u64, "updated": 1700000000000u64},
            "location": {"directory": source},
        })
    };
    let fixture = build_fixture(
        &run,
        "session-moves",
        &json!({
            "session_id": "ses_move_root",
            "session_rows": [row("ses_move_root"), row("ses_move_other")],
            "turns": [{"active": true, "sse": [
                {"move": {"directory": destination}},
                {"move": {"sessionID": "ses_move_other", "directory": plain}},
                {"sleep_ms": 100, "move": {"directory": plain}},
                {"sleep_ms": 100, "move": {"directory": destination}},
            ]}],
        }),
    );
    let harness = start_harness(&run.root);
    let list = || match harness
        .client
        .request(
            Uuid::nil(),
            Uuid::nil(),
            Command::ListProviderSessions {
                binary: fixture.bin.clone(),
                directory: source.clone(),
                tracked_session_ids: vec![
                    "ses_move_root".into(),
                    "ses_move_other".into(),
                    "ses_deleted".into(),
                ],
            },
        )
        .unwrap()
    {
        ResponsePayload::ProviderSessions { sessions } => sessions,
        _ => panic!("expected provider sessions"),
    };
    let before = list();
    assert_eq!(before.len(), 2);
    assert!(before.iter().all(
        |summary| summary.workspace == Some(fintwind_protocol::model::SessionWorkspace::Local)
    ));
    let (session, runtime) = (Uuid::new_v4(), Uuid::new_v4());
    harness
        .client
        .request(
            session,
            runtime,
            Command::Start {
                options: start_options(&fixture.bin, &source),
            },
        )
        .unwrap();
    let events = harness.client.subscribe(session, runtime);
    harness
        .client
        .request(
            session,
            runtime,
            Command::Prompt {
                prompt: "move".into(),
                files: vec![],
            },
        )
        .unwrap();
    let mut collected = Collected::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while collected
        .kinds
        .iter()
        .filter(|(kind, _)| kind == "nativeSessionMoved")
        .count()
        < 4
        && Instant::now() < deadline
    {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(100)) {
            collected.absorb(&event);
        }
    }
    let after = list();
    harness
        .client
        .request(session, runtime, Command::RefreshBackgroundWork)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let background_directory_updated = loop {
        if read_jsonl(&fixture.server_log).iter().any(|entry| {
            entry["event"] == "session_list"
                && entry["directory"].as_str() == destination.to_str()
                && entry["header_directory"].as_str() == destination.to_str()
        }) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let artifact = run.root.join("result-session-moves.json");
    fs::write(
        &artifact,
        serde_json::to_vec_pretty(
            &json!({"before": before, "after": after, "timeline": collected.timeline, "backgroundDirectoryUpdated": background_directory_updated}),
        )
        .unwrap(),
    )
    .unwrap();
    eprintln!("session move artifact: {}", artifact.display());
    assert!(
        background_directory_updated,
        "driver location-scoped requests must follow the move"
    );
    let moved_ids: Vec<_> = collected
        .timeline
        .iter()
        .filter(|event| event["kind"] == "nativeSessionMoved")
        .map(|event| {
            event["payload"]["nativeSessionId"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        moved_ids,
        [
            "ses_move_root",
            "ses_move_other",
            "ses_move_root",
            "ses_move_root"
        ]
    );
    assert_eq!(
        after.len(),
        2,
        "moved sessions survive; deleted sessions stay absent"
    );
    let root = after
        .iter()
        .find(|summary| summary.session_id == "ses_move_root")
        .unwrap();
    assert_eq!(root.directory.as_ref(), Some(&destination));
    assert_eq!(
        root.workspace,
        Some(fintwind_protocol::model::SessionWorkspace::Worktree {
            path: destination,
            branch: "work/move".into()
        })
    );
    let other = after
        .iter()
        .find(|summary| summary.session_id == "ses_move_other")
        .unwrap();
    assert_eq!(other.directory.as_ref(), Some(&plain));
    assert_eq!(
        other.workspace,
        Some(fintwind_protocol::model::SessionWorkspace::Local)
    );
}

/// Missing V2 status fields must not revive history; an active child can carry
/// an old outcome, failed/malformed active reads must not settle it, and
/// unknown execution state must never invent a start or a stop control.
#[test]
#[ignore = "owns a private daemon/pool; run separately from the recovery matrix"]
fn opencode_subagent_restore_status() {
    let run = Run::new();
    let workspace = run.workspace("subagent-restore");
    let created = 1_700_000_000_000u64;
    let row = |id: &str, parent: Option<&str>, outcome: Option<&str>| {
        let mut row = json!({
            "id": id, "title": id,
            "location": {"directory": workspace},
            "time": {"created": created, "updated": created + 2000},
        });
        if let Some(parent) = parent {
            row["parentID"] = json!(parent);
        }
        if let Some(outcome) = outcome {
            row["outcome"] = json!(outcome);
            row["time"]["idle"] = json!(created + 2000);
        }
        row
    };
    let expected = [
        ("ses_done", "completed", false),
        ("ses_failed", "failed", false),
        ("ses_stopped", "stopped", false),
        ("ses_running", "running", true),
        ("ses_unknown", "lost", false),
        ("ses_legacy", "completed", false),
    ];
    let mut legacy = row("ses_legacy", Some("ses_parent"), None);
    legacy["status"] = json!({"type": "idle"});
    let messages: serde_json::Map<String, Value> = expected
        .iter()
        .map(|(id, ..)| {
            (
                (*id).to_owned(),
                json!(settled_history(
                    "msg_user",
                    "msg_answer",
                    created,
                    id,
                    "succeeded"
                )),
            )
        })
        .collect();
    let fixture = build_fixture(
        &run,
        "subagent-restore",
        &json!({
            "models": {workspace.to_string_lossy(): [{"id": "m", "providerID": "p", "enabled": true}]},
            "session_rows": [
                row("ses_parent", None, Some("succeeded")),
                row("ses_done", Some("ses_parent"), Some("succeeded")),
                row("ses_failed", Some("ses_parent"), Some("failed")),
                row("ses_stopped", Some("ses_parent"), Some("interrupted")),
                row("ses_running", Some("ses_parent"), Some("succeeded")),
                row("ses_unknown", Some("ses_parent"), None),
                legacy,
                row("ses_unrelated", Some("ses_other"), Some("succeeded")),
            ],
            "session_messages": messages,
            "active_responses": [
                {"body": {"data": {"ses_running": {"type": "running"}}}},
                {"http_status": 503, "body": {"error": "unavailable"}},
                {"body": {"data": []}},
                {"body": {"data": {"ses_running": {"type": "running"}}}},
            ],
            "turns": [{"active": true, "created_ms": created + 3000, "sse": [
                {"event": {"type": "session.execution.started", "data": {"sessionID": "ses_done"}}},
                {"event": {"type": "session.tool.input.started", "data": {"sessionID": "ses_parent", "id": "call_resume", "name": "subagent"}}},
                {"event": {"type": "session.tool.called", "data": {"sessionID": "ses_parent", "id": "call_resume", "input": {"sessionID": "ses_done", "prompt": "continue the child"}}}},
                {"event": {"type": "session.execution.started", "data": {"sessionID": "ses_done"}}},
                {"event": {"type": "session.text.delta", "data": {"sessionID": "ses_done", "delta": "new child output"}}},
            ]}],
        }),
    );
    let harness = start_harness(&run.root);
    let (session, runtime) = (Uuid::new_v4(), Uuid::new_v4());
    let mut options = start_options(&fixture.bin, &workspace);
    options.provider_cursor = Some(json!({"provider": "openCode", "sessionId": "ses_parent"}));
    harness
        .client
        .request(session, runtime, Command::Start { options })
        .unwrap();
    let events = harness.client.subscribe(session, runtime);
    let mut report = Fact::default();
    report.server_log = Some(fixture.server_log.clone());
    for round in 0..4 {
        harness
            .client
            .request(session, runtime, Command::RefreshBackgroundWork)
            .unwrap();
        let mut items = None;
        let mut snapshots = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(event) = events.recv_timeout(Duration::from_millis(50)) {
                if event.event.kind == "backgroundWork" {
                    let payload = &event.event.payload;
                    if payload["type"] == "reconcileLive" {
                        items = payload["items"].as_array().cloned();
                    }
                    if payload["type"] == "transcript" && payload.get("snapshot").is_some() {
                        snapshots += 1;
                    }
                }
                report.collected.absorb(&event);
            }
            // Successful rounds complete on their roster (plus first hydration).
            // Failed rounds close an observation window after the read is logged.
            let read_done = read_jsonl(&fixture.server_log)
                .iter()
                .any(|entry| entry["event"] == "active" && entry["response_index"] == round);
            if (round == 0 && items.is_some() && snapshots == expected.len())
                || (round == 3 && items.is_some())
                || ((round == 1 || round == 2)
                    && read_done
                    && deadline.saturating_duration_since(Instant::now()) < Duration::from_secs(4))
            {
                break;
            }
        }
        if round == 1 || round == 2 {
            report.expect(
                items.is_none(),
                format!("round {round}: a failed active read must preserve existing state"),
            );
            continue;
        }
        let items = items.unwrap_or_default();
        report.expect(
            items.len() == expected.len(),
            format!("round {round}: only this parent's children belong in the roster"),
        );
        for (id, status, can_stop) in expected {
            let item = items
                .iter()
                .find(|item| item.pointer("/key/providerId") == Some(&json!(id)));
            report.expect(
                item.is_some_and(|item| item["status"] == status && item["canStop"] == can_stop),
                format!("round {round}: {id} must be {status} with canStop={can_stop}"),
            );
            report.expect(
                item.is_some_and(|item| item["startedAtMs"] == created),
                format!("round {round}: {id} must retain its native creation time"),
            );
        }
        if round == 0 {
            report.expect(
                snapshots == expected.len(),
                "historical transcripts must remain readable",
            );
        }
    }
    let requests = read_jsonl(&fixture.server_log);
    report.expect(
        !requests.iter().any(|entry| {
            entry["event"] == "post"
                && entry["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with("/prompt") || path.ends_with("/interrupt"))
        }),
        "restoring a session must neither execute nor interrupt a child",
    );
    // A reused native child emits no session.created. Its bound execution
    // must use a live Upsert, which can reopen the desktop registry's settled
    // item; another ReconcileLive is deliberately not enough to do that.
    harness
        .client
        .request(
            session,
            runtime,
            Command::Prompt {
                prompt: "continue parent".into(),
                files: vec![],
            },
        )
        .unwrap();
    let mut resumed = false;
    let mut prompt_seen = false;
    let mut output_seen = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !output_seen {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(50)) {
            if event.event.kind == "backgroundWork" {
                let payload = &event.event.payload;
                if payload["type"] == "upsert"
                    && payload.pointer("/key/providerId") == Some(&json!("ses_done"))
                {
                    report.expect(
                        payload["status"] == "running" && payload["canStop"] == true,
                        "the resumed child must become live and stoppable",
                    );
                    report.expect(
                        payload["originActivityIds"] == json!(["call_resume"]),
                        "only the bound resume may reopen a historical child",
                    );
                    report.expect(
                        payload["title"] == "",
                        "a resume without metadata must retain the restored child title",
                    );
                    resumed = true;
                }
                if let Some(started) = payload.get("started") {
                    prompt_seen = started["prompt"] == "continue the child";
                }
                if let Some(delta) = payload.get("textDelta") {
                    output_seen = delta["delta"] == "new child output";
                }
            }
            report.collected.absorb(&event);
        }
    }
    report.expect(
        resumed && prompt_seen && output_seen,
        "a completed historical child must support a genuine resume without a new session.created",
    );
    let artifact = run.root.join("result-subagent-restore.json");
    fs::write(
        &artifact,
        serde_json::to_vec_pretty(&finish_artifact(&[("subagent-restore", &report)], vec![]))
            .unwrap(),
    )
    .unwrap();
    eprintln!("subagent restore artifact: {}", artifact.display());
    assert!(report.failures.is_empty(), "{:?}", report.failures);
}

// -- run layout -----------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("fintwind-core is two levels below the repo root")
        .to_path_buf()
}

fn fake_opencode_py() -> PathBuf {
    repo_root().join("scripts/fixtures/opencode-recovery/fake_opencode.py")
}

fn python_exe() -> PathBuf {
    for name in ["python", "python3", "py"] {
        if let Some(found) = fintwind_core::command_env::find_executable(name) {
            return found;
        }
    }
    panic!("opencode-recovery fake needs a python interpreter on PATH");
}

struct Run {
    root: PathBuf,
    uuid: String,
}

impl Run {
    fn new() -> Self {
        let uuid = Uuid::new_v4().simple().to_string();
        let root = repo_root().join("temp").join("recovery-e2e").join(&uuid);
        fs::create_dir_all(&root).expect("the recovery-e2e artifact directory should be created");
        // The task forbids removing user temp paths; every run gets its own
        // unique uuid directory, so nothing pre-existing is ever touched.
        Self { root, uuid }
    }

    fn workspace(&self, scenario: &str) -> PathBuf {
        let path = self.root.join("workspaces").join(scenario);
        fs::create_dir_all(&path).expect("workspace should be created");
        path
    }
}

// -- fixture: per-scenario behavior.json + a .cmd shim the pool can run ----

struct Fixture {
    bin: PathBuf,
    server_log: PathBuf,
    spawn_log: PathBuf,
}

fn build_fixture(run: &Run, scenario: &str, behavior: &Value) -> Fixture {
    let dir = run.root.join("scenarios").join(scenario);
    fs::create_dir_all(&dir).expect("scenario dir should be created");
    let behavior_path = dir.join("behavior.json");
    fs::write(&behavior_path, serde_json::to_vec_pretty(behavior).unwrap())
        .expect("behavior.json should be written");
    let server_log = dir.join("server-log.jsonl");
    let spawn_log = dir.join("spawn-log.jsonl");
    let bin = dir.join("opencode.cmd");
    // The shim carries the fake's configuration through the environment and
    // leaves `%*` (the driver's own arguments, e.g. `serve --port …`) untouched,
    // so the recorded argv is exactly what the driver supplied. Quoted paths
    // and CRLF endings keep cmd.exe happy regardless of the checkout location.
    let shim = format!(
        "@echo off\r\nset \"FINTWIND_FAKE_BEHAVIOR={}\"\r\nset \"FINTWIND_FAKE_LOG={}\"\r\nset \"FINTWIND_FAKE_SPAWN_LOG={}\"\r\n\"{}\" \"{}\" %*\r\n",
        behavior_path.display(),
        server_log.display(),
        spawn_log.display(),
        python_exe().display(),
        fake_opencode_py().display(),
    );
    fs::write(&bin, shim).expect("the opencode.cmd shim should be written");
    Fixture {
        bin,
        server_log,
        spawn_log,
    }
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

// -- daemon + serve + client harness --------------------------------------

struct Harness {
    client: DaemonClient,
    shutdown: Arc<AtomicBool>,
    server: Option<JoinHandle<()>>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Ends the serve loop, whose exit runs `backend.shutdown()` →
        // `opencode_pool::shutdown_all()`, killing every fake process started
        // across the scenarios before the test process can orphan them.
        self.shutdown.store(true, Ordering::Release);
        self.client.shutdown();
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn start_harness(root: &Path) -> Harness {
    let backend = FintwindBackend::new(
        DaemonSettingsStore::open(root.join("settings.json")).expect("settings store should open"),
        StateStore::daemon(root.join("app.db")),
    )
    .expect("the FintwindBackend should construct against the test database");
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback test port should bind");
    let address = listener.local_addr().unwrap().to_string();
    let shutdown = Arc::new(AtomicBool::new(false));
    let server_shutdown = shutdown.clone();
    let server = std::thread::spawn(move || {
        serve(
            listener,
            DAEMON_TOKEN.to_owned(),
            Arc::new(backend),
            server_shutdown,
            ServerOptions {
                allow_shutdown: true,
                ..ServerOptions::default()
            },
        )
        .expect("the test daemon should serve cleanly");
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = loop {
        match DaemonClient::connect(&address, DAEMON_TOKEN.to_owned()) {
            Ok(client) => break client,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "could not connect to the test daemon: {error}"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };
    Harness {
        client,
        shutdown,
        server: Some(server),
    }
}

fn start_options(binary: &Path, cwd: &Path) -> WireDriverStartOptions {
    WireDriverStartOptions {
        binary: binary.to_path_buf(),
        cwd: cwd.to_path_buf(),
        mode: "fullAccess".to_owned(),
        interaction_mode: "build".to_owned(),
        model: None,
        reasoning_effort: None,
        service_tier: None,
        context_window: None,
        agent_preset: None,
        provider_cursor: None,
    }
}

// -- event observation ----------------------------------------------------

#[derive(Default)]
struct Collected {
    kinds: Vec<(String, u64)>,
    delta_text: String,
    ended_text: Option<String>,
    finish_indices: Vec<usize>,
    snapshot_indices: Vec<usize>,
    snapshot_texts: Vec<String>,
    snapshot_continuations: Vec<bool>,
    snapshot_parse_errors: Vec<String>,
    finishes: Vec<(bool, Option<String>)>,
    starts: usize,
    exited: bool,
    timeline: Vec<Value>,
}

impl Collected {
    fn absorb(&mut self, event: &SequencedEvent) {
        let kind = event.event.kind.clone();
        match kind.as_str() {
            "turnStarted" => self.starts += 1,
            "textDelta" => {
                if let Some(delta) = event.event.payload.get("delta").and_then(Value::as_str) {
                    self.delta_text.push_str(delta);
                }
            }
            "textEnded" => {
                self.ended_text = event
                    .event
                    .payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "turnTranscriptReconciled" => {
                self.snapshot_indices.push(self.kinds.len());
                let continuation = event
                    .event
                    .payload
                    .get("continuation")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // The payload is `{ transcript, continuation }`, not a bare
                // transcript; `transcript` carries the native-order messages.
                match event
                    .event
                    .payload
                    .get("transcript")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<NativeTranscript>(value).ok())
                {
                    Some(transcript) => {
                        self.snapshot_continuations.push(continuation);
                        self.snapshot_texts.push(assistant_text(&transcript));
                    }
                    None => {
                        self.snapshot_parse_errors
                            .push(event.event.payload.to_string());
                        self.kinds.push((kind.clone(), event.sequence));
                        self.timeline.push(json!({
                            "seq": event.sequence,
                            "kind": "turnTranscriptReconciled_parse_error",
                            "payload": event.event.payload,
                        }));
                        return;
                    }
                }
            }
            "turnFinished" => {
                self.finish_indices.push(self.kinds.len());
                let success = event
                    .event
                    .payload
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let summary = event
                    .event
                    .payload
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.finishes.push((success, summary));
            }
            "processExited" => self.exited = true,
            _ => {}
        }
        self.kinds.push((kind.clone(), event.sequence));
        // Timeline entries carry only synthetic deltas/finishes; no credential
        // ever crosses the wire, so no redaction is required.
        self.timeline.push(json!({
            "seq": event.sequence,
            "kind": event.event.kind,
            "payload": event.event.payload.clone(),
        }));
    }

    /// The authoritative final text: a reconciled native snapshot wins over a
    /// streamed `textEnded`, which wins over the accumulated deltas.
    fn authoritative_text(&self) -> String {
        if let Some(text) = self.snapshot_texts.last() {
            return text.clone();
        }
        self.ended_text
            .clone()
            .unwrap_or_else(|| self.delta_text.clone())
    }

    /// A reconciled snapshot must be published strictly before the turn settles.
    fn snapshot_before_finish(&self) -> bool {
        !self.finish_indices.is_empty()
            && self.snapshot_parse_errors.is_empty()
            && self.snapshot_indices.len() == self.finish_indices.len()
            && self
                .snapshot_indices
                .iter()
                .zip(&self.finish_indices)
                .enumerate()
                .all(|(index, (snapshot, finish))| {
                    snapshot < finish && (index == 0 || *snapshot > self.finish_indices[index - 1])
                })
    }

    fn saw(&self, kind: &str) -> bool {
        self.kinds.iter().any(|(name, _)| name == kind)
    }
}

/// Assistant text of a reconciled transcript: every assistant message's
/// content, concatenated in native order (multiple text messages joined).
fn assistant_text(transcript: &NativeTranscript) -> String {
    transcript
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Observe the subscription until a `turnFinished` arrives (then `grace` more,
/// to catch a duplicate settle or a spurious process exit) or `finish_timeout`
/// elapses without one — the negative cases rely on that window closing with no
/// finish at all.
fn observe(
    rx: &Receiver<SequencedEvent>,
    finish_timeout: Duration,
    grace: Duration,
    collected: &mut Collected,
) {
    observe_finishes(rx, finish_timeout, grace, collected, 1);
}

fn observe_finishes(
    rx: &Receiver<SequencedEvent>,
    finish_timeout: Duration,
    grace: Duration,
    collected: &mut Collected,
    expected_finishes: usize,
) {
    let started = Instant::now();
    let mut finished_at: Option<Instant> = None;
    loop {
        let now = Instant::now();
        if let Some(marked) = finished_at {
            if now.duration_since(marked) >= grace {
                break;
            }
        } else if now.duration_since(started) >= finish_timeout {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(event) => {
                if event.event.kind == "turnFinished"
                    && collected.finishes.len() + 1 >= expected_finishes
                {
                    finished_at = Some(Instant::now());
                }
                collected.absorb(&event);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

// -- scenario reporting ---------------------------------------------------

#[derive(Default)]
struct Fact {
    failures: Vec<String>,
    collected: Collected,
    facts: serde_json::Map<String, Value>,
    server_log: Option<PathBuf>,
}

impl Fact {
    fn expect(&mut self, condition: bool, message: impl Into<String>) {
        if !condition {
            self.failures.push(message.into());
        }
    }
}

/// Counts the fake provider's logged request/event categories for one scenario,
/// so the artifact shows how often the daemon actually queried the private
/// serve (model discovery, reconciliation session/active reads, SSE gens…).
fn request_summary(server_log: &Path) -> Value {
    let mut summary = serde_json::Map::new();
    for entry in read_jsonl(server_log) {
        let Some(kind) = entry.get("event").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(
            kind,
            "prompt"
                | "model"
                | "session_get"
                | "active"
                | "sse_open"
                | "sse_reconnect"
                | "auth_fail"
                | "terminal_sent"
        ) {
            continue;
        }
        *summary.entry(kind.to_owned()).or_insert(Value::from(0u64)) =
            Value::from(summary.get(kind).and_then(Value::as_u64).unwrap_or(0) + 1);
    }
    Value::Object(summary)
}

fn check(condition: bool, message: impl Into<String>) -> Vec<String> {
    if condition {
        Vec::new()
    } else {
        vec![message.into()]
    }
}

fn finish_artifact(reports: &[(&str, &Fact)], global_failures: Vec<String>) -> Value {
    let passed =
        global_failures.is_empty() && reports.iter().all(|(_, report)| report.failures.is_empty());
    json!({
        "result": if passed { "pass" } else { "fail" },
        "scenarios": reports.iter().map(|(name, report)| json!({
            "scenario": name,
            "passed": report.failures.is_empty(),
            "failures": report.failures,
            "events": {
                "turn_started": report.collected.starts,
                "turn_finished": report.collected.finishes.iter().map(|(ok, summary)| json!({
                    "success": ok, "summary": summary })).collect::<Vec<_>>(),
                "process_exited": report.collected.exited,
                "kinds": report.collected.kinds.iter().map(|(kind, _)| kind.clone()).collect::<Vec<_>>(),
                "streamed_text": report.collected.delta_text,
                "authoritative_text": report.collected.authoritative_text(),
                "snapshot_seen": !report.collected.snapshot_texts.is_empty(),
                "snapshot_before_finish": report.collected.snapshot_before_finish(),
                "snapshot_continuation": report.collected.snapshot_continuations.last().copied(),
                "snapshot_parse_errors": report.collected.snapshot_parse_errors,
                "timeline": report.collected.timeline,
            },
            "requests": report.server_log.as_deref().map(request_summary),
            "facts": Value::Object(report.facts.clone()),
        })).collect::<Vec<_>>(),
        "global_failures": global_failures,
    })
}

// -- behavior.json builders ----------------------------------------------

fn user_msg(id: &str, created: u64, text: &str) -> Value {
    json!({"id": id, "type": "user", "time": {"created": created}, "text": text,
           "content": [{"type": "text", "text": text}]})
}

fn assistant_msg(id: &str, created: u64, text: &str) -> Value {
    json!({"id": id, "type": "assistant", "time": {"created": created},
           "content": [{"type": "text", "text": text}]})
}

fn idle_msg(id: &str, created: u64, outcome: &str) -> Value {
    json!({"id": id, "type": "idle", "outcome": outcome, "time": {"created": created}})
}

// Durable history, newest first, for a settled turn.
fn settled_history(
    user_id: &str,
    asst_id: &str,
    created: u64,
    text: &str,
    outcome: &str,
) -> Vec<Value> {
    vec![
        idle_msg(&format!("{user_id}_idle"), created + 2000, outcome),
        assistant_msg(asst_id, created + 1000, text),
        user_msg(user_id, created, "prompt"),
    ]
}

// -- scenarios ------------------------------------------------------------

/// Scenario 1 (recovery doc #1, #2): model discovery and a driver share one
/// private `serve` per binary, workspace variants stay isolated, and the fake
/// is only ever started as `serve` — never the `api`/`models` CLI paths that
/// would join or start the real public service.
fn scenario_discovery(harness: &Harness, run: &Run) -> Fact {
    let mut report = Fact::default();
    let ws_a = run.workspace("discovery_a");
    let ws_b = run.workspace("discovery_b");
    let ws_a_str = ws_a.to_string_lossy().to_string();
    let ws_b_str = ws_b.to_string_lossy().to_string();

    let behavior = json!({
        "scenario": "discovery",
        "models": {
            ws_a_str.clone(): [{
                "id": "m", "providerID": "prov", "name": "M", "enabled": true,
                "variants": [{"id": "low"}, {"id": "high"}],
            }],
            ws_b_str.clone(): [{
                "id": "n", "providerID": "prov2", "name": "N", "enabled": true,
                "variants": [{"id": "alpha"}, {"id": "beta"}],
            }],
        },
        "turns": [],
    });
    let fixture = build_fixture(run, "discovery", &behavior);
    let bin_string = fixture.bin.to_string_lossy().to_string();

    let mut catalog_ids_a = Vec::new();
    let mut variants_a = 0u64;
    let mut catalog_ids_b = Vec::new();
    let mut variants_b = 0u64;
    for (directory, ids_out, variants_out) in [
        (ws_a.clone(), &mut catalog_ids_a, &mut variants_a),
        (ws_b.clone(), &mut catalog_ids_b, &mut variants_b),
    ] {
        if let Ok(ResponsePayload::ProviderProbe { probe, .. }) = harness.client.request(
            Uuid::nil(),
            Uuid::nil(),
            Command::ProbeProvider {
                binary_override: Some(bin_string.clone()),
                directory: Some(directory.clone()),
                discover_models: true,
                probe_version: false,
            },
        ) {
            *variants_out = probe
                .models
                .first()
                .map(|m| m.reasoning_efforts.len() as u64)
                .unwrap_or(0);
            if let Some(model) = probe.models.first() {
                *ids_out = model
                    .reasoning_efforts
                    .iter()
                    .map(|v| v.id.clone())
                    .collect();
            }
        } else {
            report.failures.push(format!(
                "model discovery failed for {}",
                directory.display()
            ));
        }
    }

    report.expect(
        catalog_ids_a == vec!["low".to_owned(), "high".to_owned()],
        format!("workspace A variants must be preserved in order, got {catalog_ids_a:?}"),
    );
    report.expect(
        catalog_ids_b == vec!["alpha".to_owned(), "beta".to_owned()],
        format!("workspace B variants must be preserved in order, got {catalog_ids_b:?}"),
    );
    report.expect(
        catalog_ids_a != catalog_ids_b,
        "workspace isolation lost: both workspaces returned the same variants",
    );
    report.expect(
        variants_a == 2 && variants_b == 2,
        "each workspace must carry two variants",
    );

    // The driver session must reuse the one private serve discovery started,
    // sharing the same process (pool keyed by binary), not a second one.
    let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
    match harness.client.request(
        session_id,
        runtime_id,
        Command::Start {
            options: start_options(&fixture.bin, &ws_a),
        },
    ) {
        Ok(ResponsePayload::Started { supports_steer }) => {
            report.expect(supports_steer, "the OpenCode driver supports steering");
        }
        other => report.failures.push(format!(
            "the driver should start against the shared private serve: {other:?}"
        )),
    }

    let spawns = read_jsonl(&fixture.spawn_log);
    report.expect(
        spawns.len() == 1,
        format!(
            "discovery and the session must share one private serve, saw {} spawns",
            spawns.len()
        ),
    );
    let only_serve = spawns.iter().all(|entry| {
        entry
            .get("argv")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            == Some("serve")
    });
    report.expect(only_serve, "the fake must only ever be started as `serve`");

    let model_requests = read_jsonl(&fixture.server_log);
    let model_dirs: Vec<String> = model_requests
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("model"))
        .filter_map(|e| {
            e.get("directory")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    report.expect(
        model_dirs.iter().any(|d| d == &ws_a_str) && model_dirs.iter().any(|d| d == &ws_b_str),
        format!("discovery must query both isolated workspaces, saw {model_dirs:?}"),
    );
    report.facts.insert("spawns".into(), json!(spawns.len()));
    report
        .facts
        .insert("workspace_a_variants".into(), json!(catalog_ids_a));
    report
        .facts
        .insert("workspace_b_variants".into(), json!(catalog_ids_b));
    report
        .facts
        .insert("model_directories".into(), json!(model_dirs));
    report
}

/// Scenario 2 (recovery doc #3): a text part split into many tiny SSE deltas,
/// delivered over a chunked connection whose bytes are fragmented mid-UTF-8,
/// mid-CRLF and mid-body across pauses far beyond the hub's read poll, must
/// still assemble to the exact stored text — proven by the accumulated deltas,
/// not only by a durable snapshot that could mask lost deltas.
fn scenario_fragment(harness: &Harness, run: &Run) -> Fact {
    let mut report = Fact::default();
    let ws = run.workspace("fragment");
    let text = "Hello 世界";
    let behavior = json!({
        "scenario": "fragment",
        "sse_chunked": true,
        "turns": [{
            "user_msg_id": "msg_user_1", "created_ms": 1_700_000_000_000u64,
            "assistant_msg_id": "msg_asst_1",
            // Keep the execution live while delivering all TCP fragments;
            // an early durable recovery must not mask a broken stream decoder.
            "active_until_terminal": true,
            "sse": [
                {"step_started": true}, {"text_started": true},
                // Each delta frame is chunk-encoded then split at a multibyte
                // UTF-8 boundary (世/界), at CR|LF, and mid-body, with a >poll
                // pause at every split.
                {"delta": "Hel", "fragment": true, "pause_ms": 320},
                {"delta": "lo ", "fragment": true, "pause_ms": 320},
                {"delta": "世", "fragment": true, "pause_ms": 320},
                {"delta": "界", "fragment": true, "pause_ms": 320},
                {"text_ended": text}, {"terminal": "succeeded"},
            ],
            "durable_messages": settled_history("msg_user_1", "msg_asst_1", 1_700_000_000_000, text, "succeeded"),
            "session_status": "idle", "session_outcome": "succeeded",
            "session_time_idle": 1_700_000_002_000u64, "active": false,
        }],
    });
    let fixture = build_fixture(run, "fragment", &behavior);
    report.server_log = Some(fixture.server_log.clone());

    let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
    if let Err(error) = harness.client.request(
        session_id,
        runtime_id,
        Command::Start {
            options: start_options(&fixture.bin, &ws),
        },
    ) {
        report.failures.push(format!("start failed: {error}"));
        return report;
    }
    let events = harness.client.subscribe(session_id, runtime_id);
    let _ = harness.client.request(
        session_id,
        runtime_id,
        Command::Prompt {
            prompt: "fragment".into(),
            files: Vec::new(),
        },
    );

    observe(
        &events,
        Duration::from_secs(12),
        Duration::from_millis(2000),
        &mut report.collected,
    );

    // The streamed deltas themselves must reassemble to the stored text: this
    // is what catches a half-line or multibyte split lost across the read poll.
    report.expect(
        report.collected.delta_text == text,
        format!("fragmented+chunked deltas (incl. multibyte + >poll pauses) must assemble to {text:?}, got streamed {:?}", report.collected.delta_text),
    );
    report.expect(
        report.collected.authoritative_text() == text,
        format!(
            "authoritative text must be {text:?}, got {:?}",
            report.collected.authoritative_text()
        ),
    );
    report.expect(
        report.collected.snapshot_parse_errors.is_empty(),
        format!(
            "reconciled snapshot payload must parse: {:?}",
            report.collected.snapshot_parse_errors
        ),
    );
    report.expect(
        report.collected.finishes.len() == 1,
        "exactly one turnFinished",
    );
    report.expect(
        report.collected.finishes.first().is_some_and(|(ok, _)| *ok),
        "the fragmented turn finishes successfully",
    );
    report.expect(
        !report.collected.exited,
        "a live process must not report processExited",
    );
    // A normal, unbroken finish now also publishes a native snapshot before it.
    if report.collected.saw("turnTranscriptReconciled") {
        report.expect(
            report.collected.snapshot_before_finish(),
            "a reconciled snapshot must be published before turnFinished",
        );
        report.expect(
            report.collected.snapshot_texts.last().map(String::as_str) == Some(text),
            "the snapshot's assistant text must match the streamed text",
        );
    }

    report
        .facts
        .insert("streamed_text".into(), json!(report.collected.delta_text));
    report.facts.insert(
        "authoritative".into(),
        json!(report.collected.authoritative_text()),
    );
    report.facts.insert(
        "snapshot_seen".into(),
        json!(report.collected.saw("turnTranscriptReconciled")),
    );
    report
}

/// Scenario 3 (recovery doc #5, #9, #10): the SSE stream breaks mid-turn while
/// the process stays alive, and on reconnect the fake answers the turn terminal
/// *immediately* without re-sending the lost tail. The driver must reconnect,
/// publish the durable native snapshot (healing the partial text) before
/// settling, and finish exactly once — never reporting the transport fault as
/// a process exit, never duplicating the already-streamed text prefix.
fn scenario_disconnect(harness: &Harness, run: &Run) -> Fact {
    let mut report = Fact::default();
    let ws = run.workspace("disconnect");
    let text = "Hello world";
    let behavior = json!({
        "scenario": "disconnect",
        "turns": [{
            "user_msg_id": "msg_user_1", "created_ms": 1_700_000_300_000u64,
            "assistant_msg_id": "msg_asst_1",
            // Partial text, then the socket is torn down mid-stream.
            "sse": [
                {"step_started": true}, {"text_started": true},
                {"sleep_ms": 40}, {"delta": "Hello "},
                {"sleep_ms": 40}, {"close": true},
            ],
            // On reconnect, answer the terminal at once and do NOT replay the
            // missing tail: the turn must be healed from durable history and
            // settled once, with no duplicate of the streamed "Hello " prefix.
            "reconnect_sse": [ {"terminal": "succeeded"} ],
            "durable_messages": settled_history("msg_user_1", "msg_asst_1", 1_700_000_300_000, text, "succeeded"),
            "session_status": "idle", "session_outcome": "succeeded",
            "session_time_idle": 1_700_000_302_000u64, "active": false,
        }],
    });
    let fixture = build_fixture(run, "disconnect", &behavior);
    report.server_log = Some(fixture.server_log.clone());

    let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
    if let Err(error) = harness.client.request(
        session_id,
        runtime_id,
        Command::Start {
            options: start_options(&fixture.bin, &ws),
        },
    ) {
        report.failures.push(format!("start failed: {error}"));
        return report;
    }
    let events = harness.client.subscribe(session_id, runtime_id);
    let _ = harness.client.request(
        session_id,
        runtime_id,
        Command::Prompt {
            prompt: "hello".into(),
            files: Vec::new(),
        },
    );

    // Reconnect backoff plus durable reconciliation needs a generous window.
    observe(
        &events,
        Duration::from_secs(20),
        Duration::from_millis(2000),
        &mut report.collected,
    );

    report.expect(
        !report.collected.exited,
        "an SSE drop on a live process must not be misreported as processExited",
    );
    report.expect(
        report.collected.finishes.len() == 1,
        format!(
            "reconnect + durable history must settle the turn exactly once; finishes={:?}",
            report.collected.finishes
        ),
    );
    report.expect(
        report.collected.finishes.first().is_some_and(|(ok, _)| *ok),
        "the reconnected turn settles successfully",
    );
    report.expect(
        report.collected.saw("turnTranscriptReconciled"),
        "the reconnected turn must publish a durable native snapshot",
    );
    report.expect(
        report.collected.snapshot_before_finish(),
        "the durable snapshot must be published before turnFinished",
    );
    report.expect(
        report.collected.authoritative_text() == text,
        format!(
            "the durable snapshot must complete the interrupted text to {text:?}, got {:?}",
            report.collected.authoritative_text()
        ),
    );
    report.expect(
        text.starts_with(&report.collected.delta_text),
        format!("streamed deltas must stay a clean prefix of the final text (no repeat duplication), got {:?}", report.collected.delta_text),
    );

    let log = read_jsonl(&fixture.server_log);
    let reconnects = log
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("sse_reconnect"))
        .count();
    report
        .facts
        .insert("sse_reconnects".into(), json!(reconnects));
    report
        .facts
        .insert("streamed_prefix".into(), json!(report.collected.delta_text));
    report.facts.insert(
        "snapshot_text".into(),
        json!(report.collected.authoritative_text()),
    );
    report
}

/// Scenario 5 (recovery doc #7, #11): a turn whose newest assistant message is
/// only a completed model step, with a long tool call running and the session
/// busy, must never settle. `assistant step ended != turn finished`.
fn scenario_busy(harness: &Harness, run: &Run) -> Fact {
    let mut report = Fact::default();
    let ws = run.workspace("busy");
    let behavior = json!({
        "scenario": "busy",
        "turns": [{
            "user_msg_id": "msg_user_1", "created_ms": 1_700_000_500_000u64,
            "assistant_msg_id": "msg_asst_1",
            "sse": [
                {"step_started": true}, {"text_started": true},
                {"delta": "let me check"}, {"step_ended": true},
                {"tool_input_started": "bash"},
                // No terminal; the connection stays alive with heartbeats while
                // the long tool runs.
            ],
            "durable_messages": [
                assistant_msg("msg_asst_1", 1_700_000_500_500, "let me check"),
                user_msg("msg_user_1", 1_700_000_500_000, "prompt"),
            ],
            // No busy/running status to trip the early guard — the ONLY thing
            // that must veto the reconcile is the active roster naming this
            // session, which is the real v2 `GET /api/session/active` contract.
            "session_status": "idle", "session_outcome": "succeeded",
            "session_time_idle": 1_700_000_502_000u64, "active": true,
        }],
    });
    let fixture = build_fixture(run, "busy", &behavior);
    report.server_log = Some(fixture.server_log.clone());

    let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
    if let Err(error) = harness.client.request(
        session_id,
        runtime_id,
        Command::Start {
            options: start_options(&fixture.bin, &ws),
        },
    ) {
        report.failures.push(format!("start failed: {error}"));
        return report;
    }
    let events = harness.client.subscribe(session_id, runtime_id);
    let _ = harness.client.request(
        session_id,
        runtime_id,
        Command::Prompt {
            prompt: "run".into(),
            files: Vec::new(),
        },
    );

    observe(
        &events,
        Duration::from_secs(4),
        Duration::from_millis(500),
        &mut report.collected,
    );

    report.expect(
        report.collected.finishes.is_empty(),
        format!(
            "a step ended + running tool + busy session must not settle the turn; finishes={:?}",
            report.collected.finishes
        ),
    );
    report.expect(
        !report.collected.exited,
        "a busy turn keeps the process alive",
    );
    report.expect(
        report.collected.saw("richActivity"),
        "the running tool should surface as live activity (a step ended is not a finished turn)",
    );
    report
}

/// Scenario 6 (recovery doc #8): after a completed turn, a new prompt that
/// produces no reply must not settle — a previous turn's completion is not the
/// current turn's.
fn scenario_prev_turn(harness: &Harness, run: &Run) -> Fact {
    let mut report = Fact::default();
    let ws = run.workspace("prev_turn");
    let base = 1_700_000_000_000u64;
    let behavior = json!({
        "scenario": "prev_turn",
        "turns": [
            {
                "user_msg_id": "msg_user_1", "created_ms": base,
                "assistant_msg_id": "msg_asst_1",
                "sse": [
                    {"step_started": true}, {"text_started": true},
                    {"delta": "first answer"}, {"text_ended": "first answer"},
                    {"terminal": "succeeded"},
                ],
                "durable_messages": settled_history("msg_user_1", "msg_asst_1", base, "first answer", "succeeded"),
                "session_status": "idle", "session_outcome": "succeeded",
                "session_time_idle": base + 2000, "active": false,
            },
            {
                "user_msg_id": "msg_user_2", "created_ms": base + 100_000,
                "assistant_msg_id": "msg_asst_2",
                // A buffered first-turn part arrives only after the second
                // input. Its native envelope time still belongs to turn one.
                "sse": [{"delta": "late old answer", "assistant_msg_id": "msg_asst_1", "created": base + 1000}],
                // History carries the *previous* turn's idle marker, which is
                // older than this turn's input and must not settle it.
                "durable_messages": [
                    user_msg("msg_user_2", base + 100_000, "second prompt"),
                    idle_msg("msg_user_1_idle", base + 2000, "succeeded"),
                    assistant_msg("msg_asst_1", base + 1000, "first answer"),
                    user_msg("msg_user_1", base, "first prompt"),
                ],
                "session_status": "idle", "session_outcome": "succeeded",
                "session_time_idle": base + 2000, "active": false,
            },
        ],
    });
    let fixture = build_fixture(run, "prev_turn", &behavior);

    let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
    if let Err(error) = harness.client.request(
        session_id,
        runtime_id,
        Command::Start {
            options: start_options(&fixture.bin, &ws),
        },
    ) {
        report.failures.push(format!("start failed: {error}"));
        return report;
    }
    let events = harness.client.subscribe(session_id, runtime_id);
    let _ = harness.client.request(
        session_id,
        runtime_id,
        Command::Prompt {
            prompt: "first".into(),
            files: Vec::new(),
        },
    );
    observe(
        &events,
        Duration::from_secs(8),
        Duration::from_millis(800),
        &mut report.collected,
    );
    report.expect(
        report.collected.finishes.len() == 1,
        "the first completed turn settles once",
    );

    // A second prompt that gets no reply must not settle the new turn.
    let _ = harness.client.request(
        session_id,
        runtime_id,
        Command::Prompt {
            prompt: "second".into(),
            files: Vec::new(),
        },
    );
    let before = report.collected.finishes.len();
    observe(
        &events,
        Duration::from_secs(4),
        Duration::from_millis(500),
        &mut report.collected,
    );

    report.expect(
        report.collected.finishes.len() == before,
        format!("a quiet second turn must not settle from the previous turn's completion; finishes={:?}", report.collected.finishes),
    );
    report.expect(
        report.collected.starts >= 2,
        "the second prompt must reopen a turn (turnStarted)",
    );
    report.expect(
        report.collected.delta_text == "first answer",
        "a buffered old assistant delta must not leak into the next input's live output",
    );
    report
}

// -- default (fast) matrix ------------------------------------------------

/// R1: a steer is acknowledged, but the old execution terminal arrives *after*
/// the steer and the latest steer input is delivered late. The old terminal must
/// not settle (0 TurnFinished, 0 snapshot) until the latest input is durable and
/// a fresh terminal arrives; only then does the turn settle once, with the
/// latest content preserved. Every POST happens exactly once.
fn scenario_steer_old_terminal(harness: &Harness, run: &Run) -> Fact {
    let mut report = Fact::default();
    let ws = run.workspace("steer_old_terminal");
    let base = 1_700_000_500_000u64;
    let behavior = json!({
        "scenario": "steer_old_terminal",
        "steer_mode": true,
        "turns": [{
            "user_msg_id": "msg_user_1", "created_ms": base,
            "assistant_msg_id": "msg_asst_1",
            "sse": [
                {"step_started": true}, {"text_started": true},
                {"delta": "first answer"},
                // Block until the steer POST lands, then emit the *stale* (pre
                // -steer) execution terminal, which must settle nothing.
                {"await_steer": true},
                {"terminal": "succeeded"},
                // A window for the test to observe the stale reconcile finding
                // no deliverable latest input, before the input actually lands.
                {"hold_ms": 4000},
                {"deliver_latest": true},
                {"terminal": "succeeded"},
            ],
            // The previous execution really is durably idle. Its completion
            // must not settle a newer input admitted but still in the inbox.
            "durable_messages": [
                idle_msg("msg_idle_old", base + 2000, "succeeded"),
                assistant_msg("msg_asst_1", base + 1000, "first answer"),
                user_msg("msg_user_1", base, "first prompt"),
            ],
            "session_status": "idle",
            "session_outcome": "succeeded",
            "session_time_idle": base + 2000,
            "active": false,
            "active_until_steer": true,
            // The steer input lands here (later), completing + ending the turn.
            "steer_created_ms": [base + 4000],
            "durable_messages_after": [
                idle_msg("idle_2", base + 6000, "succeeded"),
                assistant_msg("msg_asst_steer", base + 5000, "steer answer"),
                user_msg("msg_user_2", base + 4000, "second steer"),
                assistant_msg("msg_asst_1", base + 1000, "first answer"),
                user_msg("msg_user_1", base, "first prompt"),
            ],
            "session_after": {
                "session_status": "idle",
                "session_outcome": "succeeded",
                "session_time_idle": base + 6000,
            },
        }],
    });
    let fixture = build_fixture(run, "steer_old_terminal", &behavior);
    report.server_log = Some(fixture.server_log.clone());

    let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
    if let Err(error) = harness.client.request(
        session_id,
        runtime_id,
        Command::Start {
            options: start_options(&fixture.bin, &ws),
        },
    ) {
        report.failures.push(format!("start failed: {error}"));
        return report;
    }
    let events = harness.client.subscribe(session_id, runtime_id);
    let _ = harness.client.request(
        session_id,
        runtime_id,
        Command::Prompt {
            prompt: "prompt one".into(),
            files: Vec::new(),
        },
    );
    // Let the first turn begin streaming before steering into it.
    std::thread::sleep(Duration::from_millis(500));
    let _ = harness.client.request(
        session_id,
        runtime_id,
        Command::Steer {
            prompt: "steer two".into(),
            files: Vec::new(),
        },
    );

    // Phase 1: the stale terminal has fired; nothing may settle yet.
    let finishes_before = report.collected.finishes.len();
    let snapshots_before = report.collected.snapshot_texts.len();
    observe(
        &events,
        Duration::from_millis(3200),
        Duration::from_millis(200),
        &mut report.collected,
    );
    report.expect(
        report.collected.finishes.len() == finishes_before && finishes_before == 0,
        format!(
            "an execution terminal arriving after the steer ack must settle nothing yet; finishes={:?}",
            report.collected.finishes
        ),
    );
    report.expect(
        report.collected.snapshot_texts.len() == snapshots_before,
        "no snapshot may be published before the latest input is durable",
    );

    let log = read_jsonl(&fixture.server_log);
    let probed = log
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("session_get"))
        .count();
    report.expect(
        probed >= 1,
        "the stale terminal must still drive a reconciliation probe of the durable session",
    );

    // Phase 2: the latest input lands and a fresh terminal arrives.
    observe(
        &events,
        Duration::from_secs(14),
        Duration::from_millis(1500),
        &mut report.collected,
    );
    report.expect(
        report.collected.finishes.len() == 1,
        format!(
            "once the latest input is durable the turn settles exactly once; finishes={:?}",
            report.collected.finishes
        ),
    );
    report.expect(
        report.collected.finishes.first().is_some_and(|(ok, _)| *ok),
        "the reconciled steer turn settles successfully",
    );
    report.expect(
        report.collected.snapshot_before_finish(),
        "the reconciled snapshot is published before the finish",
    );
    report.expect(
        report
            .collected
            .authoritative_text()
            .contains("steer answer"),
        format!(
            "the latest steer content must be preserved, got {:?}",
            report.collected.authoritative_text()
        ),
    );
    report.expect(
        !report.collected.exited,
        "steering must not be a process exit",
    );

    // Re-read the log after phase 2: both the stale and the current terminal
    // have now been sent, so count against the final state.
    let log = read_jsonl(&fixture.server_log);
    let posts = log
        .iter()
        .filter(|e| {
            matches!(
                e.get("event").and_then(Value::as_str),
                Some("prompt") | Some("steer")
            )
        })
        .count();
    let steers = log
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("steer"))
        .count();
    let terminals = log
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("terminal_sent"))
        .count();
    report.expect(
        steers == 1,
        format!("the steer must be submitted exactly once (no resend), saw {steers}"),
    );
    report.expect(
        posts == 2 && terminals == 2,
        format!("expected prompt+steer and the stale+current terminals (2/2), saw posts={posts} terminals={terminals}"),
    );

    report.facts.insert("steers".into(), json!(steers));
    report
        .facts
        .insert("terminals_sent".into(), json!(terminals));
    report.facts.insert("probes".into(), json!(probed));
    report.facts.insert(
        "final_text".into(),
        json!(report.collected.authoritative_text()),
    );
    report
}

/// R2: after a normally-completed turn, a *server-initiated* continuation runs
/// (no prompt). The fake emits `session.execution.started` with a top-level
/// native sortable event id and `created`, persists the continuation's native
/// assistant content plus the previous turn's idle (id below the start event),
/// then drops the SSE and reconnects with a terminal. The driver must publish a
/// `continuation` snapshot in native order before settling, so the assistant
/// produced after the start is not missed by local receive time.
/// Run separately: shutting down a daemon permanently closes its process pool.
#[test]
#[ignore = "runs in a separate process via scripts/opencode-recovery-e2e.ps1 -Long"]
fn opencode_recovery_server_initiated_continuation() {
    let run = Run::new();
    let harness = start_harness(&run.root);
    let report = scenario_server_initiated_continuation(&harness, &run);
    for failure in &report.failures {
        eprintln!("[opencode-recovery] R2 FAIL {failure}");
    }
    let mut artifact =
        finish_artifact(&[("R2_server_initiated_continuation", &report)], Vec::new());
    artifact["uuid"] = Value::String(run.uuid.clone());
    let artifact_path = run.root.join("result-r2.json");
    let _ = fs::write(
        &artifact_path,
        serde_json::to_vec_pretty(&artifact).unwrap(),
    );
    eprintln!(
        "[opencode-recovery] R2 artifact: {}",
        artifact_path.display()
    );
    assert!(
        report.failures.is_empty(),
        "R2 server-initiated continuation: {:?} (artifact: {})",
        report.failures,
        artifact_path.display()
    );
}

fn scenario_server_initiated_continuation(harness: &Harness, run: &Run) -> Fact {
    let mut report = Fact::default();
    let ws = run.workspace("server_continuation");
    let base = fintwind_core::model::unix_time_millis() - 30_000;
    // Native 12-hex millisecond/seq prefix + 14 chars. The previous turn's idle
    // id sorts strictly before this execution's id (evt_ -> msg_).
    let sortable_id = |prefix: &str, timestamp: u64| {
        format!(
            "{prefix}_{:012x}00000000000001",
            (timestamp * 0x1000 + 1) & 0xffff_ffff_ffff
        )
    };
    let prev_idle_id = sortable_id("msg", base + 2000);
    let start_event_id = sortable_id("evt", base + 3000);
    let assistant_id = sortable_id("msg", base + 4000);
    let idle_id = sortable_id("msg", base + 5000);
    let behavior = json!({
        "scenario": "server_continuation",
        "turns": [
            {
                "user_msg_id": "msg_user_1", "created_ms": base,
                "assistant_msg_id": "msg_asst_1",
                "sse": [
                    {"step_started": true}, {"text_started": true},
                    {"delta": "first answer"}, {"text_ended": "first answer"},
                    {"terminal": "succeeded"},
                    // Let this turn settle (reconcile) before the server
                    // continues on its own, so the continuation does not preempt it.
                    {"hold_ms": 2500},
                ],
                "durable_messages": settled_history("msg_user_1", "msg_asst_1", base, "first answer", "succeeded"),
                "session_status": "idle", "session_outcome": "succeeded",
                "session_time_idle": base + 2000, "active": false,
            },
            {
                // No prompt: the provider starts this run on its own.
                "server_initiated": true,
                "user_msg_id": "msg_user_1",
                "created_ms": base,
                "assistant_msg_id": assistant_id,
                // execution_started carries the native envelope (top-level id +
                // created); only a PARTIAL content streams live, then the
                // terminal drives the durable continuation reconcile.
                "sse": [
                    {"execution_started": {"id": start_event_id, "created": base + 3000}},
                    {"step_started": true},
                    {"text_started": true},
                    {"delta": "continued"},
                    {"delta": " ans"},
                    {"close": true},
                ],
                "reconnect_sse": [{"terminal": "succeeded"}],
                // Newest-first: this turn's idle and
                // native assistant (the FULL text, not the partial stream), then
                // the previous turn's idle (id < start), which bounds the window.
                "durable_messages": [
                    idle_msg(&idle_id, base + 5000, "succeeded"),
                    assistant_msg(&assistant_id, base + 4000, "continued answer"),
                    idle_msg(&prev_idle_id, base + 2000, "succeeded"),
                    assistant_msg("msg_asst_1", base + 1000, "first answer"),
                    user_msg("msg_user_1", base, "first prompt"),
                ],
                "session_status": "idle",
                "session_outcome": "succeeded",
                "session_time_idle": base + 5000,
                "active": false,
            },
        ],
    });
    let fixture = build_fixture(run, "server_continuation", &behavior);
    report.server_log = Some(fixture.server_log.clone());

    let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
    if let Err(error) = harness.client.request(
        session_id,
        runtime_id,
        Command::Start {
            options: start_options(&fixture.bin, &ws),
        },
    ) {
        report.failures.push(format!("start failed: {error}"));
        return report;
    }
    let events = harness.client.subscribe(session_id, runtime_id);
    let _ = harness.client.request(
        session_id,
        runtime_id,
        Command::Prompt {
            prompt: "first prompt".into(),
            files: Vec::new(),
        },
    );

    // Both turns share the subscription: the first prompt settles quickly, then
    // the server continues. The prompt turn turns quickly, so the window is
    // generous.
    observe_finishes(
        &events,
        Duration::from_secs(25),
        Duration::from_millis(1800),
        &mut report.collected,
        2,
    );

    report.expect(
        report.collected.finishes.len() == 2,
        format!(
            "the prompt turn completes and the server continuation settles (2 finishes); got={:?}",
            report.collected.finishes
        ),
    );
    report.expect(
        report.collected.snapshot_continuations.last().copied() == Some(true),
        format!(
            "the final settle must be a continuation snapshot; continuations={:?}",
            report.collected.snapshot_continuations
        ),
    );
    report.expect(
        report.collected.snapshot_before_finish(),
        "the continuation snapshot is published before the finish",
    );
    report.expect(
        report.collected.authoritative_text() == "continued answer",
        format!(
            "the continuation's native assistant content must be the authoritative text, got {:?}",
            report.collected.authoritative_text()
        ),
    );
    report.expect(
        !report.collected.exited,
        "a continuation is not a process exit",
    );
    let snapshots = report
        .collected
        .timeline
        .iter()
        .filter_map(|event| {
            event
                .pointer("/payload/transcript")
                .cloned()
                .and_then(|value| serde_json::from_value::<NativeTranscript>(value).ok())
        })
        .collect::<Vec<_>>();
    if let [first, continuation] = snapshots.as_slice() {
        let mut session = AgentSession::new(Uuid::new_v4());
        session.begin_turn("first prompt");
        report.expect(
            session.reconcile_active_transcript(first.clone()),
            "the actual first native window applies",
        );
        session.turns.last_mut().unwrap().status = TurnStatus::Completed;
        let (messages, blocks) = (session.messages.len(), session.transcript_blocks.len());
        session.resume_provider_turn();
        session.push_message(MessageRole::Assistant, "continued ans");
        report.expect(
            session.reconcile_active_transcript_from(continuation.clone(), messages, blocks),
            "the actual continuation window replaces its suffix",
        );
        let replies = session
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .map(|message| message.content.clone())
            .collect::<Vec<_>>();
        report.expect(replies == ["first answer", "continued answer"],
            format!("the final projection preserves the settled reply and heals the continuation: {replies:?}"));
        report
            .facts
            .insert("projected_replies".into(), json!(replies));
    } else {
        report.failures.push(
            "expected two actual native windows to verify the continuation projection".into(),
        );
    }

    let started = read_jsonl(&fixture.server_log)
        .iter()
        .any(|e| e.get("event").and_then(Value::as_str) == Some("execution_started"));
    report.expect(
        started,
        "the fake emitted a native execution.started envelope",
    );

    report.facts.insert("continuation".into(), json!(true));
    report.facts.insert(
        "continuation_snapshot_text".into(),
        json!(report.collected.authoritative_text()),
    );
    report
}

/// P1 scenario: an *attachment-only* prompt — empty text with non-image
/// PDF/txt/dir files. The persisted native user is `{text:'', files:[...]}`;
/// recovery must preserve that confirmed input so the turn reconciles and
/// settles exactly once, with no placeholder prompt hiding a dropped user.
fn scenario_attachment_only_prompt(harness: &Harness, run: &Run) -> Fact {
    let mut report = Fact::default();
    let ws = run.workspace("attachment_only");
    let base = 1_700_000_900_000u64;
    let pdf = ws.join("report.pdf");
    let txt = ws.join("notes.txt");
    let dir = ws.join("assets");
    fs::write(&pdf, b"%PDF-1.4 fake attachment").ok();
    fs::write(&txt, b"attachment notes").ok();
    fs::create_dir_all(&dir).ok();
    let behavior = json!({
        "scenario": "attachment_only",
        "turns": [{
            "user_msg_id": "msg_user_1", "created_ms": base,
            "assistant_msg_id": "msg_asst_1",
            "sse": [
                {"step_started": true}, {"text_started": true},
                {"delta": "here is the answer"}, {"text_ended": "here is the answer"},
                {"terminal": "succeeded"},
            ],
            "durable_messages": settled_history("msg_user_1", "msg_asst_1", base, "here is the answer", "succeeded"),
            "session_status": "idle", "session_outcome": "succeeded",
            "session_time_idle": base + 2000, "active": false,
        }],
    });
    let fixture = build_fixture(run, "attachment_only", &behavior);
    report.server_log = Some(fixture.server_log.clone());

    let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
    if let Err(error) = harness.client.request(
        session_id,
        runtime_id,
        Command::Start {
            options: start_options(&fixture.bin, &ws),
        },
    ) {
        report.failures.push(format!("start failed: {error}"));
        return report;
    }
    let events = harness.client.subscribe(session_id, runtime_id);
    // Empty text, non-empty non-image attachments.
    let _ = harness.client.request(
        session_id,
        runtime_id,
        Command::Prompt {
            prompt: String::new(),
            files: vec![
                PromptFile {
                    path: pdf,
                    name: "report.pdf".into(),
                },
                PromptFile {
                    path: txt,
                    name: "notes.txt".into(),
                },
                PromptFile {
                    path: dir,
                    name: "assets".into(),
                },
            ],
        },
    );
    observe(
        &events,
        Duration::from_secs(10),
        Duration::from_millis(1500),
        &mut report.collected,
    );

    report.expect(
        report.collected.finishes.len() == 1,
        format!(
            "an attachment-only prompt must still settle exactly once; finishes={:?}",
            report.collected.finishes
        ),
    );
    report.expect(
        report.collected.finishes.first().is_some_and(|(ok, _)| *ok),
        "the attachment-only turn settles successfully",
    );
    report.expect(
        report.collected.snapshot_before_finish(),
        "the durable snapshot is published before the finish",
    );
    report.expect(
        !report.collected.exited,
        "an attachment-only prompt is not a process exit",
    );
    report.expect(
        report.collected.authoritative_text() == "here is the answer",
        format!(
            "the assistant answer is preserved, got {:?}",
            report.collected.authoritative_text()
        ),
    );
    let native_user = report.collected.timeline.iter().find_map(|event| {
        event
            .pointer("/payload/transcript/messages")
            .and_then(Value::as_array)
            .and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            })
    });
    report.expect(
        native_user
            .and_then(|user| user.get("content"))
            .and_then(Value::as_str)
            == Some(""),
        "the actual reconciled user must have empty text, never the fixture's default prompt",
    );
    let actual = report.collected.timeline.iter().find_map(|event| {
        event
            .pointer("/payload/transcript")
            .cloned()
            .and_then(|value| serde_json::from_value::<NativeTranscript>(value).ok())
    });
    if let Some(actual) = actual {
        let local_files = [
            ("report.pdf", false),
            ("notes.txt", false),
            ("assets", true),
        ]
        .into_iter()
        .map(|(name, is_dir)| MessageAttachment {
            path: ws.join(name),
            mention: format!("@{name}"),
            name: name.into(),
            is_dir,
            is_image: false,
            blob_reference: Some(format!("local-{name}")),
        })
        .collect::<Vec<_>>();
        let mut session = AgentSession::new(Uuid::new_v4());
        session.begin_turn_with_presentation("", None, local_files.clone());
        report.expect(
            session.reconcile_active_transcript(actual),
            "the actual empty native input can replace the local projection",
        );
        report.expect(
            session
                .messages
                .first()
                .is_some_and(|user| user.content.is_empty() && user.attachments == local_files),
            "the actual native snapshot preserves all three local attachments with empty text",
        );
    } else {
        report
            .failures
            .push("no actual native snapshot for attachment projection".into());
    }
    report
}

/// Run separately because a daemon shutdown permanently closes its process pool.
#[test]
#[ignore = "runs in a separate process via scripts/opencode-recovery-e2e.ps1 -Long"]
fn opencode_recovery_attachment_only_prompt() {
    let run = Run::new();
    let harness = start_harness(&run.root);
    let report = scenario_attachment_only_prompt(&harness, &run);
    for failure in &report.failures {
        eprintln!("[opencode-recovery] P1 FAIL {failure}");
    }
    let mut artifact = finish_artifact(&[("P1_attachment_only_prompt", &report)], Vec::new());
    artifact["uuid"] = Value::String(run.uuid.clone());
    let artifact_path = run.root.join("result-p1.json");
    let _ = fs::write(
        &artifact_path,
        serde_json::to_vec_pretty(&artifact).unwrap(),
    );
    eprintln!(
        "[opencode-recovery] P1 artifact: {}",
        artifact_path.display()
    );
    assert!(
        report.failures.is_empty(),
        "P1 attachment-only prompt recovery: {:?} (artifact: {})",
        report.failures,
        artifact_path.display()
    );
}

/// A typed template command `/name args` is sent to the
/// transport as its *expanded* text (via the real `expanded_submission` seam),
/// while the transcript keeps the typed command. The production helper must
/// match the initial user to the native expanded row through the active turn's
/// `provider_prompt`, replace the projection, and copy the original `content`
/// back. The real daemon snapshot is applied after the same submit seam binds
/// the actual transport prompt, including a round-trip of persisted turn JSON.
#[test]
#[ignore = "runs in a separate process via scripts/opencode-recovery-e2e.ps1 -Long"]
fn opencode_recovery_template_expansion_preserves_typed() {
    let commands = vec![SlashCommand {
        name: "deploy".into(),
        description: "Deploy".into(),
        scope: CommandScope::Project,
        argument_hint: None,
        template: Some("Run the release for $ARGUMENTS now".into()),
    }];
    let typed = "/deploy staging";
    let expanded =
        expanded_submission(typed, &commands).expect("a known template command must expand");
    assert_ne!(
        expanded, typed,
        "the transport prompt differs from the typed command"
    );

    // Local transcript: the typed command is what the user entered.
    let mut session = AgentSession::new(Uuid::new_v4());
    let attachment = attachment("template");
    let active = session.begin_turn_with_presentation(typed, None, vec![attachment.clone()]);

    session.bind_active_prompt_transport(&expanded);
    let turn_json = serde_json::to_value(&session.turns.last().unwrap()).unwrap();
    *session.turns.last_mut().unwrap() = serde_json::from_value::<AgentTurn>(turn_json).unwrap();
    let run = Run::new();
    let harness = start_harness(&run.root);
    let workspace = run.workspace("template");
    let base = 1_700_001_000_000u64;
    let behavior = json!({ "turns": [{
        "user_msg_id": "msg_user_template", "created_ms": base, "assistant_msg_id": "msg_asst_template",
        "sse": [{"text_started": true}, {"delta": "deployed"}, {"close": true}],
        "reconnect_sse": [{"terminal": "succeeded"}],
        "durable_messages": settled_history("msg_user_template", "msg_asst_template", base, "deployed to staging", "succeeded"),
        "session_status": "idle", "session_outcome": "succeeded", "session_time_idle": base + 2000, "active": false
    }] });
    let fixture = build_fixture(&run, "template", &behavior);
    let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
    harness
        .client
        .request(
            session_id,
            runtime_id,
            Command::Start {
                options: start_options(&fixture.bin, &workspace),
            },
        )
        .unwrap();
    let events = harness.client.subscribe(session_id, runtime_id);
    harness
        .client
        .request(
            session_id,
            runtime_id,
            Command::Prompt {
                prompt: expanded.clone(),
                files: Vec::new(),
            },
        )
        .unwrap();
    let mut collected = Collected::default();
    observe(
        &events,
        Duration::from_secs(12),
        Duration::from_millis(1000),
        &mut collected,
    );
    assert!(
        collected.snapshot_before_finish(),
        "template native window must precede completion"
    );
    assert_eq!(collected.finishes.len(), 1);
    let snapshot = collected
        .timeline
        .iter()
        .find_map(|event| {
            event
                .pointer("/payload/transcript")
                .cloned()
                .and_then(|value| serde_json::from_value::<NativeTranscript>(value).ok())
        })
        .expect("the real daemon supplies the expanded input window");
    let applied = session.reconcile_active_transcript(snapshot);
    // Record the failure as a concrete artifact before asserting, so the gap is
    // inspectable even while it is fail-first.
    let user_content_after = session
        .messages
        .iter()
        .find(|message| message.role == MessageRole::User && message.turn_id == Some(active))
        .map(|message| message.content.clone())
        .unwrap_or_default();
    let artifact = json!({
        "scenario": "P2_template_expansion_preserves_typed",
        "passed": applied && user_content_after == typed,
        "typed_command": typed,
        "expanded_transport_prompt": expanded,
        "native_snapshot_user": expanded,
        "helper_applied": applied,
        "user_content_after": user_content_after,
        "snapshot_before_finish": collected.snapshot_before_finish(),
        "event_timeline": collected.timeline,
        "expected": "helper matches via turn.provider_prompt; original typed command preserved",
    });
    let artifact_path = run.root.join("result-p2.json");
    let _ = fs::write(
        &artifact_path,
        serde_json::to_vec_pretty(&artifact).unwrap(),
    );
    eprintln!(
        "[opencode-recovery] P2 artifact: {}",
        artifact_path.display()
    );
    assert!(
        applied,
        "the helper must match the expanded transport prompt to the typed command via turn.provider_prompt (artifact: {})",
        artifact_path.display(),
    );

    let user = session
        .messages
        .iter()
        .find(|message| message.role == MessageRole::User && message.turn_id == Some(active))
        .expect("the active turn keeps its user message");
    assert_eq!(
        user.content, typed,
        "the original typed command must be preserved, not replaced by the expansion",
    );
    assert_eq!(
        user.attachments,
        vec![attachment],
        "template recovery keeps local attachment handles"
    );
    assert!(
        session
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant
                && message.content == "deployed to staging"),
        "the expanded snapshot's answer is projected",
    );
}

#[test]
fn opencode_recovery_fast_faults() {
    let run = Run::new();
    let harness = start_harness(&run.root);
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let discovery = scenario_discovery(&harness, &run);
    let fragment = scenario_fragment(&harness, &run);
    let disconnect = scenario_disconnect(&harness, &run);
    let busy = scenario_busy(&harness, &run);
    let prev_turn = scenario_prev_turn(&harness, &run);
    let steer = scenario_steer_old_terminal(&harness, &run);

    // Cross-scenario contract checks, read from every fake's logs.
    let mut global_failures: Vec<String> = Vec::new();
    let scenarios_dir = run.root.join("scenarios");
    let mut all_argv: Vec<Vec<Value>> = Vec::new();
    let mut auth_failures = 0u64;
    if let Ok(entries) = fs::read_dir(&scenarios_dir) {
        for entry in entries.flatten() {
            let dir = entry.path();
            for spawn in read_jsonl(&dir.join("spawn-log.jsonl")) {
                if let Some(argv) = spawn.get("argv").and_then(Value::as_array) {
                    all_argv.push(argv.clone());
                }
            }
            auth_failures += read_jsonl(&dir.join("server-log.jsonl"))
                .iter()
                .filter(|e| e.get("event").and_then(Value::as_str) == Some("auth_fail"))
                .count() as u64;
        }
    }
    global_failures.extend(check(
        !all_argv.is_empty(),
        "no fake process was ever recorded",
    ));
    for argv in &all_argv {
        global_failures.extend(check(
            argv.iter()
                .all(|arg| arg.as_str() != Some("api") && arg.as_str() != Some("models")),
            format!("the fake was started with a public-service CLI subcommand: {argv:?}"),
        ));
    }
    global_failures.extend(check(
        all_argv
            .iter()
            .all(|argv| argv.first().and_then(Value::as_str) == Some("serve")),
        "every fake must be started as `serve`",
    ));
    global_failures.extend(check(
        auth_failures == 0,
        format!(
            "the chain must authenticate every private request; saw {auth_failures} auth failures"
        ),
    ));

    let reports: Vec<(&str, &Fact)> = vec![
        ("1_discovery_shared_serve_and_variants", &discovery),
        ("2_fragmented_and_paused_sse", &fragment),
        ("3_sse_disconnect_reconnect_reconcile", &disconnect),
        ("5_busy_step_plus_tool_no_settle", &busy),
        ("6_previous_turn_does_not_settle_new", &prev_turn),
        ("R1_steer_ack_then_old_terminal", &steer),
    ];
    let mut artifact = finish_artifact(&reports, global_failures);
    artifact["uuid"] = Value::String(run.uuid.clone());
    artifact["started_unix"] = Value::from(started);
    artifact["fake_binary"] = Value::String(fake_opencode_py().to_string_lossy().into_owned());
    let artifact_path = run.root.join("result.json");
    let _ = fs::write(
        &artifact_path,
        serde_json::to_vec_pretty(&artifact).unwrap(),
    );

    for (name, report) in &reports {
        eprintln!(
            "[opencode-recovery] {name}: {} {}",
            if report.failures.is_empty() {
                "PASS"
            } else {
                "FAIL"
            },
            report.failures.join("; ")
        );
    }
    eprintln!("[opencode-recovery] artifact: {}", artifact_path.display());

    for (name, report) in &reports {
        assert!(
            report.failures.is_empty(),
            "scenario {name} failed: {:?} (artifact: {})",
            report.failures,
            artifact_path.display(),
        );
    }
    assert!(
        artifact.get("result").and_then(Value::as_str) == Some("pass"),
        "recovery E2E global contract failed (artifact: {})",
        artifact_path.display(),
    );
}

// -- ignored long-delay reconciliation ------------------------------------

/// Scenario 4 (recovery doc #6) plus the reconciliation vetoes (#7, #8) that
/// only fire after the ~15s quiet interval. Run separately:
/// `cargo test -p fintwind-core --test opencode_recovery -- --ignored --nocapture`.
///
/// One daemon serves every sub-case: dropping the first daemon would run
/// `opencode_pool::shutdown_all()` and close the process-global pool, so the
/// cases share one harness (and one final teardown) and only differ by their
/// private fake binary.
#[test]
#[ignore = "waits out the ~15s idle reconciliation interval; run separately"]
fn opencode_recovery_idle_reconciliation() {
    let run = Run::new();
    let harness = start_harness(&run.root);
    let mut reports: Vec<(&str, Fact)> = Vec::new();

    // 4: no terminal event at all; the turn must converge from durable data.
    {
        let ws = run.workspace("no_terminal");
        let text = "Hello world";
        let behavior = json!({
            "scenario": "no_terminal",
            "turns": [{
                "user_msg_id": "msg_user_1", "created_ms": 1_700_001_000_000u64,
                "assistant_msg_id": "msg_asst_1",
                "sse": [
                    {"step_started": true}, {"text_started": true},
                    {"delta": "Hello"}, {"delta": " world"},
                    // Deliberately no terminal here; the fake then goes quiet.
                ],
                "durable_messages": settled_history("msg_user_1", "msg_asst_1", 1_700_001_000_000, text, "succeeded"),
                "session_status": "idle", "session_outcome": "succeeded",
                "session_time_idle": 1_700_001_002_000u64, "active": false,
            }],
        });
        let fixture = build_fixture(&run, "no_terminal", &behavior);
        let mut report = Fact::default();
        let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Start {
                    options: start_options(&fixture.bin, &ws),
                },
            )
            .unwrap();
        let events = harness.client.subscribe(session_id, runtime_id);
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Prompt {
                    prompt: "hi".into(),
                    files: Vec::new(),
                },
            )
            .unwrap();

        observe(
            &events,
            Duration::from_secs(35),
            Duration::from_millis(1500),
            &mut report.collected,
        );
        report.expect(
            report.collected.finishes.len() == 1,
            format!(
                "idle reconciliation must settle a turn with no terminal event; finishes={:?}",
                report.collected.finishes
            ),
        );
        report.expect(
            report.collected.finishes.first().is_some_and(|(ok, _)| *ok),
            "the reconciled turn reports success",
        );
        report.expect(
            report.collected.saw("turnTranscriptReconciled"),
            "reconciliation must publish a native snapshot before settling",
        );
        report.expect(
            report.collected.snapshot_before_finish(),
            "the native snapshot must be published before turnFinished",
        );
        report.expect(
            report.collected.authoritative_text() == text,
            format!(
                "reconciliation completes the durable text to {text:?}, got {:?}",
                report.collected.authoritative_text()
            ),
        );
        report.expect(
            !report.collected.exited,
            "reconciliation must not treat a live process as exited",
        );
        report.server_log = Some(fixture.server_log.clone());
        reports.push(("4_no_terminal_idle_reconcile", report));
    }

    // R3: `/api/event` refuses every subscription (HTTP 500), so no SSE
    // generation ever establishes (gen0). Prompt + history still work. The turn
    // must STILL converge through reconciliation within ~15s, purely from HTTP,
    // without ever seeing an SSE terminal or faulting a process exit.
    {
        let ws = run.workspace("no_terminal_sse500");
        let text = "Hello world";
        let behavior = json!({
            "scenario": "no_terminal_sse500",
            "sse_status": 500,
            "turns": [{
                "user_msg_id": "msg_user_1", "created_ms": 1_700_001_500_000u64,
                "assistant_msg_id": "msg_asst_1",
                "sse": [
                    {"step_started": true}, {"text_started": true},
                    {"delta": "Hello"}, {"delta": " world"},
                ],
                "durable_messages": settled_history("msg_user_1", "msg_asst_1", 1_700_001_500_000, text, "succeeded"),
                "session_status": "idle", "session_outcome": "succeeded",
                "session_time_idle": 1_700_001_502_000u64, "active": false,
            }],
        });
        let fixture = build_fixture(&run, "no_terminal_sse500", &behavior);
        let mut report = Fact::default();
        let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Start {
                    options: start_options(&fixture.bin, &ws),
                },
            )
            .unwrap();
        let events = harness.client.subscribe(session_id, runtime_id);
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Prompt {
                    prompt: "hi".into(),
                    files: Vec::new(),
                },
            )
            .unwrap();

        observe(
            &events,
            Duration::from_secs(30),
            Duration::from_millis(1500),
            &mut report.collected,
        );
        report.server_log = Some(fixture.server_log.clone());
        let log = read_jsonl(&fixture.server_log);
        let sse_refused = log
            .iter()
            .any(|e| e.get("event").and_then(Value::as_str) == Some("sse_refused"));
        let terminal_sent = log
            .iter()
            .filter(|e| e.get("event").and_then(Value::as_str) == Some("terminal_sent"))
            .count();
        report.expect(
            sse_refused,
            "the fake must have refused every /api/event with 500",
        );
        report.expect(
            terminal_sent == 0,
            "no SSE terminal may be sent (gen0 never had a stream)",
        );
        report.expect(
            report.collected.finishes.len() == 1,
            format!(
                "a gen0 turn must still settle once via HTTP reconciliation; finishes={:?}",
                report.collected.finishes
            ),
        );
        report.expect(
            report.collected.finishes.first().is_some_and(|(ok, _)| *ok),
            "the gen0-reconciled turn reports success",
        );
        report.expect(
            report.collected.snapshot_before_finish(),
            "a durable snapshot must still be published before the finish",
        );
        report.expect(
            report.collected.authoritative_text() == text,
            format!(
                "gen0 reconciliation completes the durable text to {text:?}, got {:?}",
                report.collected.authoritative_text()
            ),
        );
        report.expect(
            !report.collected.exited,
            "a refused SSE stream is a connection fault, not a private-process exit",
        );
        reports.push(("R3_gen0_sse500_idle_reconcile", report));
    }

    // 7: busy session — the ~15s reconciliation must be vetoed by the active
    // roster, never settling a running long tool.
    {
        let ws = run.workspace("busy_idle");
        let behavior = json!({
            "scenario": "busy_idle",
            "turns": [{
                "user_msg_id": "msg_user_1", "created_ms": 1_700_002_000_000u64,
                "assistant_msg_id": "msg_asst_1",
                "sse": [{"step_started": true}, {"text_started": true}, {"delta": "working"},
                        {"step_ended": true}, {"tool_input_started": "bash"}],
                "durable_messages": [
                    assistant_msg("msg_asst_1", 1_700_002_000_500, "working"),
                    user_msg("msg_user_1", 1_700_002_000_000, "prompt")],
                // Genuine active-veto: no running status to short-circuit the
                // guard, so ONLY `GET /api/session/active` naming this session
                // can keep the ~15s reconciliation from settling a live tool.
                "session_status": "idle", "session_outcome": "succeeded",
                "session_time_idle": 1_700_002_002_000u64, "active": true,
            }],
        });
        let fixture = build_fixture(&run, "busy_idle", &behavior);
        let mut report = Fact::default();
        let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Start {
                    options: start_options(&fixture.bin, &ws),
                },
            )
            .unwrap();
        let events = harness.client.subscribe(session_id, runtime_id);
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Prompt {
                    prompt: "run".into(),
                    files: Vec::new(),
                },
            )
            .unwrap();
        observe(
            &events,
            Duration::from_secs(22),
            Duration::from_millis(500),
            &mut report.collected,
        );
        report.server_log = Some(fixture.server_log.clone());
        let log = read_jsonl(&fixture.server_log);
        let probed = log
            .iter()
            .any(|e| e.get("event").and_then(Value::as_str) == Some("session_get"));
        let active_probed = log
            .iter()
            .any(|e| e.get("event").and_then(Value::as_str) == Some("active"));
        report.expect(
            probed,
            "reconciliation should have queried the session after ~15s",
        );
        report.expect(
            active_probed,
            "reconciliation should consult /api/session/active",
        );
        report.expect(
            report.collected.finishes.is_empty(),
            format!(
                "an active long tool must never be settled by reconciliation; finishes={:?}",
                report.collected.finishes
            ),
        );
        reports.push(("7_busy_active_veto", report));
    }

    // 8: previous turn's completion must not settle a quiet new turn even after
    // the reconciliation interval elapses.
    {
        let ws = run.workspace("prev_idle");
        let base = 1_700_003_000_000u64;
        let behavior = json!({
            "scenario": "prev_idle",
            "turns": [
                {"user_msg_id": "msg_user_1", "created_ms": base, "assistant_msg_id": "msg_asst_1",
                 "sse": [{"step_started": true}, {"text_started": true}, {"delta": "answer"},
                         {"text_ended": "answer"}, {"terminal": "succeeded"}],
                 "durable_messages": settled_history("msg_user_1", "msg_asst_1", base, "answer", "succeeded"),
                 "session_status": "idle", "session_outcome": "succeeded",
                 "session_time_idle": base + 2000, "active": false},
                {"user_msg_id": "msg_user_2", "created_ms": base + 100_000, "assistant_msg_id": "msg_asst_2",
                 "sse": [],
                 "durable_messages": [
                    user_msg("msg_user_2", base + 100_000, "second"),
                    idle_msg("msg_user_1_idle", base + 2000, "succeeded"),
                    assistant_msg("msg_asst_1", base + 1000, "answer"),
                    user_msg("msg_user_1", base, "first")],
                 "session_status": "idle", "session_outcome": "succeeded",
                 "session_time_idle": base + 2000, "active": false},
            ],
        });
        let fixture = build_fixture(&run, "prev_idle", &behavior);
        let mut report = Fact::default();
        let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Start {
                    options: start_options(&fixture.bin, &ws),
                },
            )
            .unwrap();
        let events = harness.client.subscribe(session_id, runtime_id);
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Prompt {
                    prompt: "first".into(),
                    files: Vec::new(),
                },
            )
            .unwrap();
        observe(
            &events,
            Duration::from_secs(8),
            Duration::from_millis(800),
            &mut report.collected,
        );
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Prompt {
                    prompt: "second".into(),
                    files: Vec::new(),
                },
            )
            .unwrap();
        let before = report.collected.finishes.len();
        observe(
            &events,
            Duration::from_secs(22),
            Duration::from_millis(500),
            &mut report.collected,
        );
        report.expect(
            report.collected.finishes.len() == before,
            format!(
                "a previous turn's idle marker must not settle a quiet new turn; finishes={:?}",
                report.collected.finishes
            ),
        );
        reports.push(("8_previous_turn_idle_veto", report));
    }

    // 9/11: the prompt is stored durably but the acknowledgement never returns
    // (the connection is dropped). An unknown transport fault must NOT resubmit
    // or fail the turn; the durable history later heals it into exactly one
    // settled turn.
    {
        let ws = run.workspace("unknown_ack");
        let text = "Hello world";
        // Rebase native timestamps to the fixture's actual POST receipt time,
        // as a real local OpenCode server does. No acknowledgement is available.
        let created = 1_700_000_000_000u64;
        let behavior = json!({
            "scenario": "unknown_ack",
            "turns": [{
                "user_msg_id": "msg_user_1", "created_ms": created,
                "assistant_msg_id": "msg_asst_1",
                "drop_ack": true,
                "rebase_clock": true,
                // Quiet after the (dropped) prompt; no SSE terminal at all.
                "sse": [],
                "durable_messages": settled_history("msg_user_1", "msg_asst_1", created, text, "succeeded"),
                "session_status": "idle", "session_outcome": "succeeded",
                "session_time_idle": created + 2000, "active": false,
            }],
        });
        let fixture = build_fixture(&run, "unknown_ack", &behavior);
        let mut report = Fact::default();
        let (session_id, runtime_id) = (Uuid::new_v4(), Uuid::new_v4());
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Start {
                    options: start_options(&fixture.bin, &ws),
                },
            )
            .unwrap();
        let events = harness.client.subscribe(session_id, runtime_id);
        harness
            .client
            .request(
                session_id,
                runtime_id,
                Command::Prompt {
                    prompt: "hi".into(),
                    files: Vec::new(),
                },
            )
            .unwrap();
        observe(
            &events,
            Duration::from_secs(35),
            Duration::from_millis(1500),
            &mut report.collected,
        );

        report.server_log = Some(fixture.server_log.clone());
        let log = read_jsonl(&fixture.server_log);
        let stored_prompts = log
            .iter()
            .filter(|e| e.get("event").and_then(Value::as_str) == Some("prompt"))
            .count();
        report.expect(stored_prompts == 1,
            format!("an unknown ack fault must not resubmit the prompt; the fake saw {stored_prompts} storage writes"));
        report.expect(
            report.collected.finishes.len() == 1,
            format!(
                "a dropped ack must still resolve to exactly one settled turn; finishes={:?}",
                report.collected.finishes
            ),
        );
        report.expect(
            report.collected.finishes.first().is_some_and(|(ok, _)| *ok),
            "the once-unacknowledged turn settles successfully from durable history",
        );
        report.expect(
            report.collected.snapshot_before_finish(),
            "the durable snapshot must be published before the settled finish",
        );
        report.expect(
            report.collected.authoritative_text() == text,
            format!(
                "durable history supplies the full text {text:?}, got {:?}",
                report.collected.authoritative_text()
            ),
        );
        report.expect(
            !report.collected.exited,
            "a dropped ack is not a process exit",
        );
        reports.push(("9_11_unknown_ack_no_resend_recovers", report));
    }

    let report_refs: Vec<(&str, &Fact)> =
        reports.iter().map(|(name, fact)| (*name, fact)).collect();
    let mut artifact = finish_artifact(&report_refs, Vec::new());
    artifact["uuid"] = Value::String(run.uuid.clone());
    artifact["fake_binary"] = Value::String(fake_opencode_py().to_string_lossy().into_owned());
    let artifact_path = run.root.join("result-long.json");
    let _ = fs::write(
        &artifact_path,
        serde_json::to_vec_pretty(&artifact).unwrap(),
    );
    eprintln!(
        "[opencode-recovery] long artifact: {}",
        artifact_path.display()
    );

    for (name, report) in &report_refs {
        assert!(
            report.failures.is_empty(),
            "idle reconciliation case {name} failed: {:?} (artifact: {})",
            report.failures,
            artifact_path.display(),
        );
    }
}

// -- atomic transcript-projection contract --------------------------------

/// Behaviour of the production projection helper `AgentSession::reconcile_
/// active_transcript` (and its continuation `_from` variant) that the
/// `turnTranscriptReconciled` wire event feeds. Built from real sessions and
/// real native transcripts, not source strings.
#[test]
fn opencode_active_turn_reconciliation_projection() {
    let project = Uuid::new_v4();
    let mut session = AgentSession::new(project);

    // A settled historical turn must survive everything below.
    let prior = session.begin_turn("prior prompt");
    session.push_message(MessageRole::Assistant, "prior answer");
    for turn in session.turns.iter_mut() {
        if turn.id == prior {
            turn.status = TurnStatus::Completed;
        }
    }

    // The active turn already shows the user (with client-owned presentation +
    // attachment) and a *late* part B the live stream rendered out of order.
    let active = session.begin_turn_with_presentation(
        "look at notes",
        Some("look at notes (see @notes.txt)".to_owned()),
        vec![MessageAttachment {
            path: PathBuf::from("C:/recovery/notes.txt"),
            mention: "@notes.txt".to_owned(),
            name: "notes.txt".to_owned(),
            is_dir: false,
            is_image: false,
            blob_reference: Some("blob-1".into()),
        }],
    );
    session.push_message(MessageRole::Assistant, "B");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: session.messages.len(),
        turn_id: Some(active),
        activities: vec![ActivityItem::new(
            Some("live_stray".into()),
            ActivityKind::Tool,
            "stray",
            None,
            false,
        )],
    });
    let user_presentation = session
        .messages
        .iter()
        .find(|message| message.role == MessageRole::User && message.turn_id == Some(active))
        .cloned()
        .expect("the active turn has its user message");

    // Native snapshot: the correct order is User, A, [tool, reasoning], B.
    let snapshot = NativeTranscript {
        messages: vec![
            Message::new(MessageRole::User, "look at notes"),
            Message::new(MessageRole::Assistant, "A"),
            Message::new(MessageRole::Assistant, "B"),
        ],
        blocks: vec![TranscriptBlock {
            after_message: 2, // after User + A, before B
            turn_id: None,
            activities: vec![
                ActivityItem::new(
                    Some("call_1".into()),
                    ActivityKind::Command,
                    "bash",
                    None,
                    true,
                ),
                ActivityItem::new(
                    Some("think_1".into()),
                    ActivityKind::Reasoning,
                    "thinking",
                    None,
                    true,
                ),
            ],
        }],
        turns: Vec::new(),
    };

    let applied = session.reconcile_active_transcript(snapshot);
    assert!(
        applied,
        "a complete native snapshot must apply to the active turn"
    );

    let rendered: Vec<(MessageRole, &str)> = session
        .messages
        .iter()
        .filter(|message| message.turn_id == Some(active))
        .map(|message| (message.role, message.content.as_str()))
        .collect();
    assert_eq!(
        rendered,
        vec![
            (MessageRole::User, "look at notes"),
            (MessageRole::Assistant, "A"),
            (MessageRole::Assistant, "B"),
        ],
        "the active turn must render the native order User/A/B (B is not lost or first)",
    );

    // The client-owned user presentation is preserved, not overwritten by the
    // native (blank) user row.
    let rebuilt_user = session
        .messages
        .iter()
        .find(|message| message.role == MessageRole::User && message.turn_id == Some(active))
        .unwrap();
    assert_eq!(
        rebuilt_user.id, user_presentation.id,
        "the local user id is retained"
    );
    assert_eq!(
        rebuilt_user.display_content,
        user_presentation.display_content
    );
    assert_eq!(rebuilt_user.attachments, user_presentation.attachments);

    // The tool/reasoning activity block sits between A and B, in native order.
    let block = session
        .transcript_blocks
        .iter()
        .find(|block| block.turn_id == Some(active))
        .expect("the reconciled tool/reasoning activity block");
    assert_eq!(
        block.activities.iter().map(|a| a.kind).collect::<Vec<_>>(),
        vec![ActivityKind::Command, ActivityKind::Reasoning],
        "the block must carry tool then reasoning in native order",
    );

    // No duplicate of the late part B, and the history turn survives untouched.
    let b_count = session
        .messages
        .iter()
        .filter(|m| m.turn_id == Some(active) && m.content == "B")
        .count();
    assert_eq!(b_count, 1, "the already-live B must not duplicate");
    assert!(
        session.messages.iter().any(|m| m.content == "prior answer"),
        "the settled history turn must be preserved",
    );

    // Re-applying the same snapshot must be idempotent — no appended duplicates.
    let repeat = NativeTranscript {
        messages: vec![
            Message::new(MessageRole::User, "look at notes"),
            Message::new(MessageRole::Assistant, "A"),
            Message::new(MessageRole::Assistant, "B"),
        ],
        blocks: vec![TranscriptBlock {
            after_message: 2,
            turn_id: None,
            activities: vec![ActivityItem::new(
                Some("call_1".into()),
                ActivityKind::Command,
                "bash",
                None,
                true,
            )],
        }],
        turns: Vec::new(),
    };
    let messages_before = session.messages.len();
    let blocks_before = session.transcript_blocks.len();
    assert!(session.reconcile_active_transcript(repeat));
    assert_eq!(
        session.messages.len(),
        messages_before,
        "a repeat snapshot must not append"
    );
    assert_eq!(session.transcript_blocks.len(), blocks_before);

    // An incomplete window (a native snapshot missing the local user) must be
    // refused without discarding the local input.
    let incomplete = NativeTranscript {
        messages: vec![Message::new(MessageRole::Assistant, "A")],
        blocks: Vec::new(),
        turns: Vec::new(),
    };
    let messages_before = session.messages.len();
    let blocks_before = session.transcript_blocks.len();
    assert!(
        !session.reconcile_active_transcript(incomplete),
        "an incomplete native window must never discard the local user input",
    );
    assert_eq!(session.messages.len(), messages_before);
    assert_eq!(session.transcript_blocks.len(), blocks_before);

    // Continuation path: a provider-initiated continuance replaces only the
    // messages after the recorded prefix, preserving that settled prefix.
    let mut session = AgentSession::new(Uuid::new_v4());
    let turn = session.begin_turn("go");
    session.push_message(MessageRole::Assistant, "settled prefix");
    let message_start = session.messages.len();
    let block_start = session.transcript_blocks.len();
    session.push_message(MessageRole::Assistant, "live tail");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: message_start,
        turn_id: Some(turn),
        activities: vec![ActivityItem::new(
            None,
            ActivityKind::Tool,
            "stale",
            None,
            false,
        )],
    });
    let continuation = NativeTranscript {
        messages: vec![Message::new(MessageRole::Assistant, "continued answer")],
        blocks: vec![TranscriptBlock {
            after_message: 0,
            turn_id: None,
            activities: vec![ActivityItem::new(
                None,
                ActivityKind::Tool,
                "fresh",
                None,
                true,
            )],
        }],
        turns: Vec::new(),
    };
    assert!(session.reconcile_active_transcript_from(continuation, message_start, block_start));
    let tail: Vec<&str> = session.messages[message_start..]
        .iter()
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(
        tail,
        vec!["continued answer"],
        "only the suffix is replaced"
    );
    assert!(
        session.messages[..message_start]
            .iter()
            .any(|m| m.content == "settled prefix"),
        "the settled prefix before message_start is preserved",
    );

    // Reversed arrival: a lost-ack early prompt whose durable row only lands
    // after a later acked steer, so the native snapshot reports the users in a
    // different order than the client holds. Presentation is matched by
    // content, so each attachment stays with its own prompt — not swapped by
    // position — while the transcript still renders the native order.
    let mut session = AgentSession::new(Uuid::new_v4());
    let active = session.begin_turn_with_presentation(
        "first prompt",
        Some("first prompt".to_owned()),
        vec![attachment("A")],
    );
    session.push_user_message_with_presentation(
        "second steer",
        Some("second steer".to_owned()),
        vec![attachment("B")],
    );
    let reversed = NativeTranscript {
        messages: vec![
            Message::new(MessageRole::User, "second steer"),
            Message::new(MessageRole::User, "first prompt"),
            Message::new(MessageRole::Assistant, "answer"),
        ],
        blocks: Vec::new(),
        turns: Vec::new(),
    };
    assert!(
        session.reconcile_active_transcript(reversed),
        "every native user matched a local user by content, so the window is complete",
    );
    let blob_of = |content: &str| -> Option<String> {
        session
            .messages
            .iter()
            .find(|m| {
                m.role == MessageRole::User && m.turn_id == Some(active) && m.content == content
            })
            .and_then(|m| m.attachments.first())
            .and_then(|a| a.blob_reference.clone())
    };
    assert_eq!(
        blob_of("first prompt").as_deref(),
        Some("A"),
        "attachment A must stay with its own prompt despite reversed native order",
    );
    assert_eq!(
        blob_of("second steer").as_deref(),
        Some("B"),
        "attachment B must stay with its own prompt despite reversed native order",
    );
    let rendered_users: Vec<&str> = session
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::User && m.turn_id == Some(active))
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(
        rendered_users,
        vec!["second steer", "first prompt"],
        "the transcript renders native user order while attachments follow content",
    );

    // A native snapshot that omits a local user must be refused: matching by
    // content cannot drop a local input, so the helper returns false and leaves
    // the local transcript untouched.
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn_with_presentation("kept prompt", None, Vec::new());
    let foreign = NativeTranscript {
        messages: vec![
            Message::new(MessageRole::User, "a different prompt"),
            Message::new(MessageRole::Assistant, "answer"),
        ],
        blocks: Vec::new(),
        turns: Vec::new(),
    };
    let messages_before = session.messages.len();
    let blocks_before = session.transcript_blocks.len();
    assert!(
        !session.reconcile_active_transcript(foreign),
        "a local user with no matching native row must be refused, never dropped",
    );
    assert_eq!(session.messages.len(), messages_before);
    assert_eq!(session.transcript_blocks.len(), blocks_before);
    assert!(
        session.messages.iter().any(|m| m.content == "kept prompt"),
        "the unmatched local user survives a refused reconciliation",
    );
}

/// A small attachment with a blob reference used to trace which prompt it stays
/// attached to after a content-matched reconciliation.
fn attachment(reference: &str) -> MessageAttachment {
    let name = format!("{reference}.txt");
    MessageAttachment {
        path: PathBuf::from(format!("C:/recovery/{name}")),
        mention: format!("@{name}"),
        name,
        is_dir: false,
        is_image: false,
        blob_reference: Some(reference.to_owned()),
    }
}

/// P1 helper guard: an attachment-only (empty-text) user with PDF/txt/dir files
/// must still be matched and its attachments preserved by the production helper,
/// with no placeholder text substituted for the empty prompt. This is the
/// client-side half of the P1 gap; it is the projection the recovery translate
/// fix must feed.
#[test]
fn opencode_attachment_only_user_projection_is_preserved() {
    let pdf = attachment("doc.pdf");
    let txt = attachment("notes.txt");
    let directory = MessageAttachment {
        path: PathBuf::from("C:/recovery/assets"),
        mention: "@assets".into(),
        name: "assets".into(),
        is_dir: true,
        is_image: false,
        blob_reference: Some("assets".into()),
    };
    let mut session = AgentSession::new(Uuid::new_v4());
    let active = session.begin_turn_with_presentation(
        "",
        None,
        vec![pdf.clone(), txt.clone(), directory.clone()],
    );

    // A native snapshot for that empty-text input: it must reconcile, keep the
    // empty text, and carry the local attachments through.
    let snapshot = NativeTranscript {
        messages: vec![
            Message::new(MessageRole::User, ""),
            Message::new(MessageRole::Assistant, "here is the answer"),
        ],
        blocks: Vec::new(),
        turns: Vec::new(),
    };
    assert!(
        session.reconcile_active_transcript(snapshot),
        "an empty-text attachment-only user must still be reconciled",
    );
    let user = session
        .messages
        .iter()
        .find(|message| message.role == MessageRole::User && message.turn_id == Some(active))
        .expect("the active turn keeps its user message");
    assert_eq!(
        user.content, "",
        "the empty prompt stays empty - no fake placeholder text"
    );
    assert_eq!(
        user.attachments,
        vec![pdf, txt, directory],
        "the PDF/txt/dir attachments must be preserved, not dropped or swapped",
    );
}

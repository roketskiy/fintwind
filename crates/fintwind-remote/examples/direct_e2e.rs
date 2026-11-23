//! Desktop-side fixture for the direct-connect end-to-end run. It starts the
//! real `RemoteControl` against the configured signaling endpoint under an
//! isolated profile, answers business commands with deterministic data, and
//! exchanges progress with the test runner through files in an owned output
//! directory. The phone side is driven separately by the runner.

use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use fintwind_remote::{
    DesktopRequest, RelayConfig, RemoteCommand, RemoteControl, RemoteEvent, RemoteStatus,
};
use serde_json::{Value, json};
use uuid::Uuid;

const WORKSPACE_ID: Uuid = Uuid::from_u128(0x00000000_0000_4000_8000_00000000e2e1);
const FIRST_SESSION_ID: Uuid = Uuid::from_u128(0x00000000_0000_4000_8000_00000000e2e2);
const DEFAULT_RELAY: &str = "https://fintwind-mobile.roketskiy.top";

struct Options {
    profile_dir: PathBuf,
    relay_url: String,
    pairing_file: PathBuf,
    out_dir: PathBuf,
    timeout: Duration,
    signal_connect_addr: Option<String>,
    stun_port: Option<u16>,
}

fn parse_options() -> Result<Options, String> {
    let mut profile_dir = None;
    let mut relay_url = DEFAULT_RELAY.to_owned();
    let mut pairing_file = None;
    let mut out_dir = None;
    let mut timeout = Duration::from_secs(180);
    let mut signal_connect_addr = None;
    let mut stun_port = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("missing value for {flag}"));
        match flag.as_str() {
            "--profile-dir" => profile_dir = Some(PathBuf::from(value()?)),
            "--relay-url" => relay_url = value()?,
            "--pairing-file" => pairing_file = Some(PathBuf::from(value()?)),
            "--out-dir" => out_dir = Some(PathBuf::from(value()?)),
            "--timeout" => {
                let seconds: u64 = value()?
                    .parse()
                    .map_err(|_| "--timeout expects whole seconds".to_owned())?;
                timeout = Duration::from_secs(seconds.max(1));
            }
            "--signal-connect-addr" => signal_connect_addr = Some(value()?),
            "--stun-port" => {
                stun_port = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--stun-port expects a port number".to_owned())?,
                );
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let profile_dir = profile_dir.ok_or("--profile-dir is required")?;
    let pairing_file = pairing_file.ok_or("--pairing-file is required")?;
    let out_dir = out_dir.ok_or("--out-dir is required")?;
    if !profile_dir.is_absolute() || !pairing_file.is_absolute() || !out_dir.is_absolute() {
        return Err("all paths must be absolute".into());
    }
    Ok(Options {
        profile_dir,
        relay_url,
        pairing_file,
        out_dir,
        timeout,
        signal_connect_addr,
        stun_port,
    })
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn append_jsonl(path: &Path, value: Value) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{value}")
}

struct Message {
    id: String,
    role: &'static str,
    text: String,
}

struct FixtureSession {
    title: String,
    status: &'static str,
    updated_at: u64,
    messages: Vec<Message>,
    watch: Option<smol::channel::Sender<Result<Value, String>>>,
    pending_watch: Option<Value>,
}

impl FixtureSession {
    fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            status: "idle",
            updated_at: now_millis() / 1000,
            messages: Vec::new(),
            watch: None,
            pending_watch: None,
        }
    }

    fn detail(&self, id: Uuid) -> Value {
        json!({
            "id": id,
            "projectId": WORKSPACE_ID,
            "title": self.title,
            "status": self.status,
            "messages": self.messages.iter().map(|message| json!({
                "id": message.id,
                "role": message.role,
                "text": message.text,
            })).collect::<Vec<_>>(),
            "features": ["sessionDeltas", "steering"],
            "model": "fixture",
            "modelName": "Fixture",
            "interactionMode": "build",
            "reasoningEffort": "",
            "truncated": false,
            "permission": null,
            "questions": null,
            "desktopRequired": false,
            "canSend": true,
            "canSteer": true,
        })
    }

    fn push_watch(&mut self, id: Uuid) {
        self.pending_watch = Some(self.detail(id));
    }

    fn flush_watch(&mut self, id: Uuid, out_dir: &Path) {
        let Some(watch) = &self.watch else {
            self.pending_watch = None;
            return;
        };
        let Some(detail) = &self.pending_watch else {
            return;
        };
        match watch.try_send(Ok(detail.clone())) {
            Ok(()) => {
                let last_assistant = detail["messages"].as_array().and_then(|messages| {
                    messages
                        .iter()
                        .rev()
                        .find(|message| message["role"] == "assistant")
                        .and_then(|message| message["text"].as_str())
                });
                let _ = append_jsonl(
                    &out_dir.join("events.jsonl"),
                    json!({"ts": now_millis(), "type": "watchPush", "sessionId": id, "status": detail["status"], "lastAssistantText": last_assistant}),
                );
                self.pending_watch = None;
            }
            Err(smol::channel::TrySendError::Full(_)) => {}
            Err(smol::channel::TrySendError::Closed(_)) => {
                self.watch = None;
                self.pending_watch = None;
            }
        }
    }
}

struct Upload {
    name: String,
    bytes: Vec<u8>,
}

struct ScheduledReply {
    due: Instant,
    session_id: Uuid,
    text: String,
    prefix: &'static str,
}

struct Fixture {
    sessions: Vec<(Uuid, FixtureSession)>,
    uploads: HashMap<Uuid, Upload>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            sessions: vec![(FIRST_SESSION_ID, FixtureSession::new("E2E 会话"))],
            uploads: HashMap::new(),
        }
    }

    fn session(&mut self, id: Uuid) -> Option<&mut FixtureSession> {
        self.sessions
            .iter_mut()
            .find(|(session_id, _)| *session_id == id)
            .map(|(_, session)| session)
    }

    fn begin_prompt(&mut self, id: Uuid, text: &str) {
        let Some(session) = self.session(id) else {
            return;
        };
        session.updated_at = now_millis() / 1000;
        session.messages.push(Message {
            id: Uuid::new_v4().to_string(),
            role: "user",
            text: text.to_owned(),
        });
        session.status = "working";
        session.push_watch(id);
    }

    fn complete_prompt(&mut self, id: Uuid, text: &str, prefix: &str) {
        let Some(session) = self.session(id) else {
            return;
        };
        session.updated_at = now_millis() / 1000;
        session.messages.push(Message {
            id: Uuid::new_v4().to_string(),
            role: "assistant",
            text: format!("{prefix}{text}"),
        });
        session.status = "idle";
        session.push_watch(id);
    }
}

fn request_log(command: &RemoteCommand) -> (&'static str, Value) {
    match command {
        RemoteCommand::ListWorkspaces => ("listWorkspaces", json!({})),
        RemoteCommand::GetTaskActivity => ("getTaskActivity", json!({})),
        RemoteCommand::ListSessions { project_id } => {
            ("listSessions", json!({"projectId": project_id}))
        }
        RemoteCommand::GetSession { session_id } => ("getSession", json!({"sessionId": session_id})),
        RemoteCommand::WatchSession {
            session_id,
            watch_id,
            enabled,
        } => (
            "watchSession",
            json!({"sessionId": session_id, "watchId": watch_id, "enabled": enabled}),
        ),
        RemoteCommand::GetComposer { session_id } => {
            ("getComposer", json!({"sessionId": session_id}))
        }
        RemoteCommand::SearchFiles { session_id, query } => {
            ("searchFiles", json!({"sessionId": session_id, "query": query}))
        }
        RemoteCommand::ConfigureSession { session_id, .. } => {
            ("configureSession", json!({"sessionId": session_id}))
        }
        RemoteCommand::CreateSession { project_id } => {
            ("createSession", json!({"projectId": project_id}))
        }
        RemoteCommand::SendPrompt {
            session_id,
            text,
            attachment_ids,
        } => (
            "sendPrompt",
            json!({"sessionId": session_id, "text": text, "attachmentIds": attachment_ids}),
        ),
        RemoteCommand::SteerPrompt {
            session_id,
            text,
            attachment_ids,
        } => (
            "steerPrompt",
            json!({"sessionId": session_id, "text": text, "attachmentIds": attachment_ids}),
        ),
        RemoteCommand::UploadChunk {
            session_id,
            upload_id,
            name,
            offset,
            data_base64,
            complete,
        } => (
            "uploadChunk",
            json!({
                "sessionId": session_id, "uploadId": upload_id, "name": name,
                "offset": offset, "dataLength": data_base64.len(), "complete": complete,
            }),
        ),
        RemoteCommand::Stop { session_id } => ("stop", json!({"sessionId": session_id})),
        RemoteCommand::RespondPermission { session_id, .. } => {
            ("respondPermission", json!({"sessionId": session_id}))
        }
        RemoteCommand::AnswerQuestions { session_id, .. } => {
            ("answerQuestions", json!({"sessionId": session_id}))
        }
    }
}

enum Reply {
    Immediate(Result<Value, String>),
    Deferred,
}

fn handle_request(
    fixture: &mut Fixture,
    schedule: &mut Vec<ScheduledReply>,
    out_dir: &Path,
    request: DesktopRequest,
) -> Reply {
    let (kind, fields) = request_log(&request.command);
    let _ = append_jsonl(
        &out_dir.join("requests.jsonl"),
        json!({"ts": now_millis(), "id": request.id, "type": kind, "fields": fields}),
    );
    let missing_session = || Err("Unknown session".to_owned());
    match &request.command {
        RemoteCommand::ListWorkspaces => Reply::Immediate(Ok(json!({
            "workspaces": [{"id": WORKSPACE_ID, "name": "E2E 工作区"}],
            "selectedProjectId": WORKSPACE_ID,
        }))),
        RemoteCommand::GetTaskActivity => Reply::Immediate(Ok(json!({"tasks": []}))),
        RemoteCommand::ListSessions { project_id } => {
            if *project_id != WORKSPACE_ID {
                return Reply::Immediate(Err("Unknown project".into()));
            }
            Reply::Immediate(Ok(json!({
                "sessions": fixture.sessions.iter().map(|(id, session)| json!({
                    "id": id,
                    "projectId": WORKSPACE_ID,
                    "title": session.title,
                    "status": session.status,
                    "updatedAt": session.updated_at,
                })).collect::<Vec<_>>(),
                "cutoff": 0,
            })))
        }
        RemoteCommand::GetSession { session_id } => match fixture.session(*session_id) {
            Some(session) => Reply::Immediate(Ok(session.detail(*session_id))),
            None => Reply::Immediate(missing_session()),
        },
        RemoteCommand::GetComposer { session_id } => {
            if fixture.session(*session_id).is_none() {
                return Reply::Immediate(missing_session());
            }
            Reply::Immediate(Ok(json!({"canConfigure": false, "models": [], "commands": []})))
        }
        RemoteCommand::CreateSession { project_id } => {
            if *project_id != WORKSPACE_ID {
                return Reply::Immediate(Err("Unknown project".into()));
            }
            let id = Uuid::new_v4();
            fixture.sessions.push((id, FixtureSession::new("E2E 新会话")));
            Reply::Immediate(Ok(json!({"sessionId": id})))
        }
        RemoteCommand::SendPrompt {
            session_id, text, ..
        } => {
            if fixture.session(*session_id).is_none() {
                return Reply::Immediate(missing_session());
            }
            fixture.begin_prompt(*session_id, text);
            // The stop and steer scenarios keep their turn open until the
            // matching command arrives.
            if !text.starts_with("e2e-stop-") && !text.starts_with("e2e-steer-") {
                schedule.push(ScheduledReply {
                    due: Instant::now() + Duration::from_millis(300),
                    session_id: *session_id,
                    text: text.clone(),
                    prefix: "fixture-reply:",
                });
            }
            Reply::Immediate(Ok(json!({"accepted": true})))
        }
        RemoteCommand::Stop { session_id } => {
            if fixture.session(*session_id).is_none() {
                return Reply::Immediate(missing_session());
            }
            let stopped: Vec<ScheduledReply> = schedule
                .extract_if(.., |item| item.session_id == *session_id)
                .collect();
            for item in stopped {
                fixture.complete_prompt(item.session_id, &item.text, item.prefix);
            }
            let session = fixture.session(*session_id).expect("session exists");
            session.status = "idle";
            session.updated_at = now_millis() / 1000;
            session.push_watch(*session_id);
            Reply::Immediate(Ok(json!({"accepted": true})))
        }
        RemoteCommand::UploadChunk {
            upload_id,
            name,
            offset,
            data_base64,
            complete,
            ..
        } => {
            let bytes = match STANDARD.decode(data_base64) {
                Ok(bytes) => bytes,
                Err(_) => return Reply::Immediate(Err("Invalid upload data".into())),
            };
            let upload = fixture.uploads.entry(*upload_id).or_insert_with(|| Upload {
                name: name.clone(),
                bytes: Vec::new(),
            });
            if *offset != upload.bytes.len() {
                return Reply::Immediate(Err("Unexpected upload offset".into()));
            }
            upload.bytes.extend_from_slice(&bytes);
            let next_offset = upload.bytes.len();
            if *complete {
                let upload = fixture.uploads.remove(upload_id).expect("upload exists");
                let digest = ring::digest::digest(&ring::digest::SHA256, &upload.bytes);
                let sha256 = digest
                    .as_ref()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                let _ = append_jsonl(
                    &out_dir.join("uploads.jsonl"),
                    json!({
                        "ts": now_millis(), "uploadId": upload_id, "name": upload.name,
                        "bytes": upload.bytes.len(), "sha256": sha256,
                    }),
                );
            }
            Reply::Immediate(Ok(json!({"accepted": true, "offset": next_offset})))
        }
        RemoteCommand::SteerPrompt {
            session_id, text, ..
        } => {
            if fixture.session(*session_id).is_none() {
                return Reply::Immediate(missing_session());
            }
            fixture.begin_prompt(*session_id, text);
            schedule.push(ScheduledReply {
                due: Instant::now() + Duration::from_millis(300),
                session_id: *session_id,
                text: text.clone(),
                prefix: "fixture-steer:",
            });
            Reply::Deferred
        }
        _ => Reply::Immediate(Err("not supported in fixture".into())),
    }
}

fn report_result(passed: bool, connected: bool, reason: &str) -> i32 {
    println!(
        "RESULT={{\"passed\":{passed},\"connected\":{connected},\"reason\":\"{reason}\"}}"
    );
    if passed { 0 } else { 1 }
}

fn main() {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let run_result = run(&options);
    let code = match run_result {
        Ok((passed, connected, reason)) => report_result(passed, connected, &reason),
        Err(error) => {
            eprintln!("fixture failed: {error:#}");
            report_result(false, false, "fixture error")
        }
    };
    std::process::exit(code);
}

fn run(options: &Options) -> anyhow::Result<(bool, bool, String)> {
    std::fs::create_dir_all(options.profile_dir.join(".fintwind"))?;
    std::fs::write(
        options
            .profile_dir
            .join(".fintwind")
            .join("remote-control.json"),
        serde_json::to_vec(&json!({"relay_url": options.relay_url}))?,
    )?;
    // The fixture owns this process; redirecting the profile keeps the real
    // user configuration untouched while exercising the real config loader.
    unsafe {
        std::env::set_var("USERPROFILE", &options.profile_dir);
        std::env::set_var("HOME", &options.profile_dir);
    }
    let config = RelayConfig::load_default()?;
    let (control, events) =
        RemoteControl::start_with_options(config, options.signal_connect_addr.clone(), options.stun_port)?;
    std::fs::write(&options.pairing_file, format!("{}\n", control.pairing_url()))?;
    println!("PAIRING_FILE={}", options.pairing_file.display());

    let (reply_tx, reply_rx) =
        mpsc::channel::<(smol::channel::Sender<Result<Value, String>>, Result<Value, String>)>();
    std::thread::spawn(move || {
        while let Ok((sender, answer)) = reply_rx.recv() {
            let _ = sender.send_blocking(answer);
        }
    });

    let mut fixture = Fixture::new();
    let mut deferred: Vec<smol::channel::Sender<Result<Value, String>>> = Vec::new();
    let mut schedule: Vec<ScheduledReply> = Vec::new();
    let mut ever_connected = false;
    let mut failed_reason: Option<String> = None;
    let finish_path = options.out_dir.join("finish");
    let started = Instant::now();
    let outcome = loop {
        if finish_path.exists() {
            break "finish";
        }
        if started.elapsed() >= options.timeout {
            break "timeout";
        }
        let session_ids: Vec<Uuid> = fixture.sessions.iter().map(|(id, _)| *id).collect();
        for id in session_ids {
            if let Some(session) = fixture.session(id) {
                session.flush_watch(id, &options.out_dir);
            }
        }
        for item in schedule.extract_if(.., |item| item.due <= Instant::now()) {
            fixture.complete_prompt(item.session_id, &item.text, item.prefix);
        }
        let mut pending = std::mem::take(&mut deferred);
        for respond in pending.drain(..) {
            match respond.try_send(Ok(json!({"accepted": true}))) {
                Ok(()) => {}
                Err(smol::channel::TrySendError::Full(_)) => deferred.push(respond),
                Err(smol::channel::TrySendError::Closed(_)) => {}
            }
        }
        match events.try_recv() {
            Ok(RemoteEvent::Status(status)) => {
                let (label, error) = match &status {
                    RemoteStatus::Connecting => ("connecting", None),
                    RemoteStatus::Waiting => ("waiting", None),
                    RemoteStatus::Connected => ("connected", None),
                    RemoteStatus::Failed(reason) => ("failed", Some(reason.clone())),
                };
                let _ = append_jsonl(
                    &options.out_dir.join("status.jsonl"),
                    json!({"ts": now_millis(), "status": label, "error": error}),
                );
                match status {
                    RemoteStatus::Connected => ever_connected = true,
                    RemoteStatus::Failed(reason) => {
                        failed_reason = Some(reason);
                        break "failed";
                    }
                    _ => {}
                }
            }
            Ok(RemoteEvent::WatchSession {
                session_id,
                snapshots,
            }) => {
                let _ = append_jsonl(
                    &options.out_dir.join("events.jsonl"),
                    json!({"ts": now_millis(), "type": "watchSession", "sessionId": session_id}),
                );
                if let Some(session) = fixture.session(session_id) {
                    session.watch = Some(snapshots);
                    session.push_watch(session_id);
                }
            }
            Ok(RemoteEvent::Request(request)) => {
                let respond = request.respond.clone();
                match handle_request(&mut fixture, &mut schedule, &options.out_dir, request) {
                    Reply::Immediate(answer) => {
                        let _ = reply_tx.send((respond, answer));
                    }
                    Reply::Deferred => {
                        deferred.push(respond);
                    }
                }
            }
            Err(smol::channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(smol::channel::TryRecvError::Closed) => break "events closed",
        }
    };
    control.stop();
    let reason = failed_reason.unwrap_or_else(|| outcome.to_owned());
    let passed = match outcome {
        "finish" => true,
        "timeout" => ever_connected,
        _ => false,
    };
    Ok((passed, ever_connected, reason))
}

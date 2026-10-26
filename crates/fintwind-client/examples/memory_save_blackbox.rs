//! Exercises the real desktop store and WebSocket client against a controlled
//! slow/failing peer. No provider, child process, user directory or GUI is used.
//! See docs/memory-save-failure-matrix.md for the failure cases.

use anyhow::{Context, ensure};
use fintwind_client::{
    DaemonSupervisor,
    persistence::{PersistedState, StateStore},
};
use fintwind_protocol::{
    ClientMessage, Command, DaemonSettings, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION,
    ResponseOutcome, ResponsePayload, RpcError, ServerMessage,
    model::{
        AgentSession, BackgroundWorkItem, BackgroundWorkKind, BackgroundWorkSnapshot,
        BackgroundWorkStatus, BackgroundWorkTranscript, Project,
    },
};
use serde_json::json;
use std::{
    collections::HashMap,
    fs,
    net::TcpListener,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, protocol::WebSocketConfig};
use uuid::Uuid;

#[derive(Default)]
struct Observed {
    checkpoints: usize,
    sessions: HashMap<Uuid, (String, usize, usize)>,
    skeletons: Vec<Uuid>,
    largest_request: usize,
}

fn wait_completion(
    store: &StateStore,
    state: &mut PersistedState,
) -> anyhow::Result<std::io::Result<()>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(result) = store.finish_save(state) {
            return Ok(result);
        }
        ensure!(
            Instant::now() < deadline,
            "save completion deadline exceeded"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("provide a new isolated artifact directory")?,
    );
    ensure!(
        root.is_dir(),
        "the harness must create the isolated directory"
    );
    ensure!(
        !root.join("result.json").exists(),
        "refusing to overwrite an earlier run"
    );
    let data = root.join("desktop");
    fs::create_dir(&data)?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?.to_string();
    let token = Uuid::new_v4().to_string();
    let peer_token = token.clone();
    let project = Project::from_path(root.clone());
    let peer_project = project.clone();
    let observed = Arc::new(Mutex::new(Observed::default()));
    let peer_observed = observed.clone();
    let (started_tx, started) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let peer = thread::spawn(move || -> anyhow::Result<()> {
        let (stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(20)))?;
        stream.set_write_timeout(Some(Duration::from_secs(20)))?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_WIRE_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_WIRE_MESSAGE_BYTES));
        let mut socket = tungstenite::accept_with_config(stream, Some(config))?;
        let hello = socket.read()?;
        let ClientMessage::Hello {
            token,
            protocol_version,
            ..
        } = serde_json::from_slice(&hello.into_data())?
        else {
            anyhow::bail!("missing hello");
        };
        ensure!(
            token == peer_token && protocol_version == PROTOCOL_VERSION,
            "invalid isolated handshake"
        );
        socket.send(Message::Text(
            serde_json::to_string(&ServerMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "blackbox".into(),
            })?
            .into(),
        ))?;
        loop {
            let message = match socket.read() {
                Ok(Message::Text(text)) => text,
                Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => break,
                Ok(_) => continue,
                Err(error) => return Err(error.into()),
            };
            let request_bytes = message.len();
            let ClientMessage::Request(request) = serde_json::from_str(&message)? else {
                continue;
            };
            let outcome = match request.command {
                Command::GetSettings => ResponseOutcome::Ok {
                    payload: ResponsePayload::Settings {
                        settings: DaemonSettings::default(),
                    },
                },
                Command::UpdateSettings { .. } => ResponseOutcome::Ok {
                    payload: ResponsePayload::Ack,
                },
                Command::LoadTaskState => ResponseOutcome::Ok {
                    payload: ResponsePayload::TaskState {
                        projects: vec![peer_project.clone()],
                        sessions: Vec::new(),
                        default_cwd: peer_project.path.clone(),
                        projectless_root: None,
                        projectless_legacy_root: None,
                    },
                },
                Command::SaveTaskStateCheckpoint {
                    sessions,
                    skeleton_session_ids,
                    ..
                } => {
                    let count = {
                        let mut seen = peer_observed.lock().unwrap();
                        seen.checkpoints += 1;
                        seen.largest_request = seen.largest_request.max(request_bytes);
                        seen.checkpoints
                    };
                    if count == 1 || count == 3 {
                        started_tx.send(count)?;
                        release_rx.recv_timeout(Duration::from_secs(15))?;
                    }
                    if count == 1 {
                        ResponseOutcome::Error {
                            error: RpcError {
                                message: "controlled commit failure".into(),
                            },
                        }
                    } else {
                        let mut seen = peer_observed.lock().unwrap();
                        seen.skeletons = skeleton_session_ids;
                        for session in &sessions {
                            seen.sessions.insert(
                                session.id,
                                (
                                    session.title.clone(),
                                    session
                                        .messages
                                        .last()
                                        .map_or(0, |message| message.content.len()),
                                    session.background_work.len(),
                                ),
                            );
                        }
                        ResponseOutcome::Ok {
                            payload: ResponsePayload::TaskStateSaved {
                                sessions: sessions
                                    .iter()
                                    .map(AgentSession::list_projection)
                                    .collect(),
                            },
                        }
                    }
                }
                command => anyhow::bail!("unexpected command: {command:?}"),
            };
            socket.send(Message::Text(
                serde_json::to_string(&ServerMessage::Response {
                    request_id: request.request_id,
                    outcome,
                })?
                .into(),
            ))?;
        }
        Ok(())
    });
    let daemon = DaemonSupervisor::connect(&address, token)?;
    let store = StateStore::remote_in(daemon.clone(), &data);
    let (wake, wakes) = smol::channel::bounded(1);
    store.set_save_wake(wake);
    let mut state = store.load_or_fresh(root.clone());
    state.push_session(AgentSession::new(project.id));
    let id = state.sessions[0].id;
    let session = state.session_mut(id).unwrap();
    session.begin_turn("large history");
    session.messages.last_mut().unwrap().content = "x".repeat(2 * 1024 * 1024);
    for index in 0..24 {
        let mut item = BackgroundWorkItem::new(
            BackgroundWorkKind::Subagent,
            format!("child-{index}"),
            "child",
            BackgroundWorkStatus::Completed,
        );
        item.output = Some("y".repeat(512 * 1024));
        session
            .background_work
            .push(Arc::new(BackgroundWorkSnapshot {
                item,
                transcript: BackgroundWorkTranscript::default(),
            }));
    }
    ensure!(store.save(&mut state)?, "first save was not admitted");
    ensure!(
        started.recv_timeout(Duration::from_secs(15))? == 1,
        "slow peer did not receive the first save"
    );
    let mut max_busy_attempt = Duration::ZERO;
    for index in 0..2000 {
        state.session_mut(id).unwrap().title = format!("latest-{index}");
        let now = Instant::now();
        ensure!(
            !store.save(&mut state)?,
            "a busy save admitted another snapshot"
        );
        max_busy_attempt = max_busy_attempt.max(now.elapsed());
    }
    ensure!(
        observed.lock().unwrap().checkpoints == 1,
        "save backlog reached the peer"
    );
    let mut second = AgentSession::new(project.id);
    let second_id = second.id;
    second.begin_turn("new session while saving");
    state.push_session(second);
    release.send(())?;
    ensure!(
        wait_completion(&store, &mut state)?.is_err(),
        "controlled failure was hidden"
    );
    ensure!(wakes.try_recv().is_ok(), "idle event pump was not woken");
    ensure!(store.save(&mut state)?, "retry was not admitted");
    wait_completion(&store, &mut state)??;
    {
        let seen = observed.lock().unwrap();
        ensure!(
            seen.sessions[&id] == ("latest-1999".into(), 2 * 1024 * 1024, 24),
            "failed dirty history or latest edits were lost"
        );
        ensure!(
            seen.sessions.contains_key(&second_id),
            "newer dirty ids were lost"
        );
    }
    let settings_time = fs::metadata(data.join("app.json"))?.modified()?;
    let navigation_time = fs::metadata(data.join("state.json"))?.modified()?;
    state.session_mut(id).unwrap().title = "before-quit".into();
    ensure!(store.save(&mut state)?, "pre-quit save was not admitted");
    ensure!(
        started.recv_timeout(Duration::from_secs(15))? == 3,
        "quit barrier setup failed"
    );
    state.session_mut(id).unwrap().title = "final-quit".into();
    let flush_store = store.clone();
    let flush = thread::spawn(move || flush_store.flush(&mut state));
    thread::sleep(Duration::from_millis(100));
    ensure!(
        !flush.is_finished(),
        "quit did not wait for the pending commit"
    );
    release.send(())?;
    flush
        .join()
        .map_err(|_| anyhow::anyhow!("flush thread panicked"))??;
    ensure!(
        observed.lock().unwrap().sessions[&id].0 == "final-quit",
        "quit lost the final edits"
    );
    ensure!(
        fs::metadata(data.join("app.json"))?.modified()? == settings_time,
        "unchanged settings were rewritten"
    );
    ensure!(
        fs::metadata(data.join("state.json"))?.modified()? == navigation_time,
        "unchanged navigation was rewritten"
    );
    let seen = observed.lock().unwrap();
    fs::write(
        root.join("result.json"),
        serde_json::to_vec_pretty(&json!({
            "passed": true, "busyAttempts": 2000, "checkpointRequests": seen.checkpoints,
            "largestRequestBytes": seen.largest_request, "maxBusyAttemptMicros": max_busy_attempt.as_micros(),
            "latestEditsPreserved": true, "failedDirtyIdsRetried": true, "completionWokeIdlePump": true,
            "quitWaitedForCommit": true, "unchangedDesktopFilesSkipped": true,
            "providerProcessesStarted": 0,
        }))?,
    )?;
    drop(seen);
    daemon.client().disconnect();
    daemon.shutdown();
    peer.join()
        .map_err(|_| anyhow::anyhow!("peer thread panicked"))??;
    println!("[OK] {}", root.join("result.json").display());
    Ok(())
}

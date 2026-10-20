use super::*;
use fintwind_client::driver::{DriverControl, DriverEventSender};
use gpui::TestAppContext;
use std::sync::Mutex;

#[derive(Default)]
struct RecordingProvider(Mutex<Vec<String>>);

impl DriverControl for RecordingProvider {
    fn prompt(&self, _: String, _: Vec<fintwind_protocol::PromptFile>) {}
    fn cancel(&self) {
        self.0.lock().unwrap().push("cancel".into());
    }
    fn respond(&self, _: String, _: String) {}
    fn compact(&self) {
        self.0.lock().unwrap().push("foreground".into());
    }
    fn compact_child(&self, id: String) {
        self.0.lock().unwrap().push(id);
    }
}

struct HeadlessApp(Entity<Fintwind>);

impl Render for HeadlessApp {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn usage(tokens: u64) -> DriverEvent {
    DriverEvent::UsageUpdated {
        context_tokens: Some(tokens),
        context_window: Some(1_000_000),
        context_window_resolved: true,
        session_total: None,
        cache_read: None,
        prompt_tokens: None,
        latest: None,
    }
}

fn child(id: &str, tokens: u64, status: BackgroundWorkStatus) -> DriverEvent {
    let mut item = BackgroundWorkItem::new(BackgroundWorkKind::Subagent, id, id, status);
    item.context_tokens = Some(tokens);
    item.origin_activity_ids.push(id.into());
    DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item))
}

fn deliver(
    app: &mut Fintwind,
    sender: &DriverEventSender,
    event: DriverEvent,
    cx: &mut Context<Fintwind>,
) {
    sender.send(event).unwrap();
    app.drain_driver_events(cx);
}

#[gpui::test]
#[ignore = "run with scripts/auto-compaction-e2e.py for isolated data and process ownership"]
fn automatic_compaction_event_pump_e2e(cx: &mut TestAppContext) {
    cx.background_executor.allow_parking();
    let proof_path = std::path::PathBuf::from(std::env::var("FINTWIND_E2E_PROOF").unwrap());
    let workspace = proof_path.parent().unwrap().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::env::set_current_dir(&workspace).unwrap();
    let daemon = fintwind_client::DaemonSupervisor::connect(
        &std::env::var("FINTWIND_DAEMON_ADDRESS").expect("run scripts/auto-compaction-e2e.py"),
        std::env::var("FINTWIND_DAEMON_TOKEN").unwrap(),
    )
    .unwrap();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::input::init(cx);
        crate::ui::menu::init(cx);
        crate::theme::init(cx);
    });
    let desktop = proof_path.parent().unwrap().join("desktop");
    std::fs::create_dir_all(&desktop).unwrap();
    let store = StateStore::remote_in(daemon.clone(), &desktop);
    let (view, cx) = cx.add_window_view(|window, cx| {
        HeadlessApp(Fintwind::new_with_store(window, cx, daemon, store))
    });
    let app = cx.read_entity(&view, |view, _| view.0.clone());
    let provider = Arc::new(RecordingProvider::default());
    cx.update(|_, cx| app.update(cx, |app, cx| {
        let mut session = AgentSession::new(Uuid::new_v4());
        session.detail_loaded = true;
        let id = session.id;
        app.state.sessions.push(session);
        app.state.selected_session = None;
        app.state.auto_compact_enabled = true;
        app.state.auto_compact_threshold_tokens = 400_000;
        let (sender, events) = fintwind_client::driver::event_channel(app.event_wake_tx.clone());
        app.runtimes.insert(id, SessionRuntime {
            driver: DriverHandle::from_control(provider.clone()),
            options_generation: 0,
            events,
            pending_events: VecDeque::new(),
            pending_steers: VecDeque::new(),
            stream_phase: None,
            stream_remeasure_pending: false,
            open_reasoning: HashMap::new(),
            settled_reasoning: HashSet::new(),
            open_text: HashMap::new(),
            settled_text: HashSet::new(),
            continuation_projection_start: None,
            transcript_reconciliation_failed: false,
            provider_phase: None,
            pending_permission: None,
            pending_user_input: None,
            last_driver_error: None,
            last_active_at: Instant::now(),
            turn_started_at: None,
            first_token_recorded: false,
            last_background_refresh_at: Instant::now(),
        });
        deliver(app, &sender, usage(399_999), cx);
        assert_eq!(*provider.0.lock().unwrap(), Vec::<String>::new());
        deliver(app, &sender, usage(505_700), cx);
        assert_eq!(*provider.0.lock().unwrap(), ["foreground"], "505.7K occupancy must compact at 400K");
        deliver(app, &sender, usage(510_000), cx);
        assert_eq!(*provider.0.lock().unwrap(), ["foreground"]);
        deliver(app, &sender, usage(399_999), cx);
        deliver(app, &sender, usage(400_000), cx);
        assert_eq!(*provider.0.lock().unwrap(), ["foreground", "foreground"]);
        app.state.auto_compact_enabled = false;
        deliver(app, &sender, usage(505_700), cx);
        assert_eq!(provider.0.lock().unwrap().len(), 2);
        app.state.auto_compact_enabled = true;
        app.state.session_mut(id).unwrap().compaction = Some(CompactionState {
            status: CompactionStatus::Running, reason: None, model: None, error: None, summary: None,
        });
        deliver(app, &sender, usage(505_700), cx);
        assert_eq!(provider.0.lock().unwrap().len(), 2);
        app.state.session_mut(id).unwrap().compaction = None;
        deliver(app, &sender, usage(505_700), cx);
        assert_eq!(*provider.0.lock().unwrap(), ["foreground", "foreground", "foreground"]);
        deliver(app, &sender, child("child-a", 505_700, BackgroundWorkStatus::Running), cx);
        deliver(app, &sender, child("child-a", 510_000, BackgroundWorkStatus::Running), cx);
        assert_eq!(*provider.0.lock().unwrap(), ["foreground", "foreground", "foreground", "child-a"]);
        deliver(app, &sender, child("child-a", 399_999, BackgroundWorkStatus::Running), cx);
        deliver(app, &sender, child("child-a", 400_000, BackgroundWorkStatus::Running), cx);
        deliver(app, &sender, child("finished-child", 505_700, BackgroundWorkStatus::Completed), cx);
        assert_eq!(*provider.0.lock().unwrap(), ["foreground", "foreground", "foreground", "child-a", "child-a"]);
        deliver(app, &sender, DriverEvent::BackgroundWork(BackgroundWorkEvent::Transcript(
            BackgroundWorkTranscriptEvent::Finished {
                key: BackgroundWorkKey::new(BackgroundWorkKind::Subagent, "child-a"),
                success: true,
            },
        )), cx);
        deliver(app, &sender, child("child-a", 505_700, BackgroundWorkStatus::Running), cx);
        assert_eq!(*provider.0.lock().unwrap(), ["foreground", "foreground", "foreground", "child-a", "child-a", "child-a"]);
        assert_eq!(app.background_work_for_activity(id, "child-a").unwrap().status, BackgroundWorkStatus::Running);
        app.state.selected_session = Some(id);
        deliver(app, &sender, usage(399_999), cx);
        sender.send(usage(505_700)).unwrap();
        sender.send(child("cancelled-child", 505_700, BackgroundWorkStatus::Running)).unwrap();
        app.cancel_turn(cx);
        assert_eq!(*provider.0.lock().unwrap(), ["foreground", "foreground", "foreground", "child-a", "child-a", "child-a", "cancel"], "stopping must drain buffered events without starting compaction");
        deliver(app, &sender, usage(505_700), cx);
        sender.send(usage(399_999)).unwrap();
        sender.send(child("child-a", 399_999, BackgroundWorkStatus::Running)).unwrap();
        app.cancel_turn(cx);
        assert_eq!(*provider.0.lock().unwrap(), ["foreground", "foreground", "foreground", "child-a", "child-a", "child-a", "cancel", "foreground", "cancel"]);
        deliver(app, &sender, usage(400_000), cx);
        deliver(app, &sender, child("child-a", 400_000, BackgroundWorkStatus::Running), cx);
        assert_eq!(*provider.0.lock().unwrap(), ["foreground", "foreground", "foreground", "child-a", "child-a", "child-a", "cancel", "foreground", "cancel", "foreground", "child-a"], "low usage drained during stop must rearm foreground and child compaction");
        let proof = serde_json::json!({
            "passed": true,
            "surface": "Fintwind event channel -> actual driver event pump -> provider control",
            "commands": provider.0.lock().unwrap().clone(),
            "contextTokens": app.state.session_mut(id).unwrap().context_usage.as_ref().unwrap().tokens,
            "thresholdTokens": 400_000,
        });
        std::fs::write(proof_path, serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
    }));
}

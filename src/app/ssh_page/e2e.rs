use super::*;
use gpui::TestAppContext;

struct Desktop(Entity<Fintwind>, bool);

impl Render for Desktop {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (dialog, toast, progress) = self.0.update(cx, |app, cx| {
            let progress = app.toast.as_ref().is_some_and(|toast| matches!(toast.tone, ToastTone::Progress(_)));
            (app.render_add_project_dialog(cx), app.render_active_toast(cx), progress)
        });
        self.1 |= progress;
        div().size_full().children(dialog).children(toast)
    }
}

#[derive(Deserialize)]
struct Fixture {
    profile: Profile,
    workspace: String,
    binary: PathBuf,
    state_dir: PathBuf,
    proof: PathBuf,
    live_model: Option<String>,
}

const LIVE_PROMPT: &str = "Work only inside the current test project. Use the file write tool to create ssh-e2e.txt containing FINTWIND-SSH-FILE, then use the file read tool to verify it. Use the subagent tool to launch a general subagent and wait for its result. Ask the subagent to read ssh-e2e.txt and report FINTWIND-SSH-CHILD with the file marker. After its tool result arrives, give a short final answer including FINTWIND-SSH-DONE.";

#[gpui::test]
#[ignore = "requires the isolated SSH daemon fixture"]
fn desktop_ssh_project_streams(cx: &mut TestAppContext) {
    cx.background_executor.allow_parking();
    let fixture: Fixture =
        serde_json::from_str(&std::env::var("FINTWIND_SSH_DESKTOP_FIXTURE").unwrap()).unwrap();
    let bootstrap = prepare_project_launch(
        fixture.profile.clone(),
        fixture.workspace.clone(),
        "SSH fixture".into(),
        fixture.state_dir.join("picker"),
    )
    .unwrap();
    let client = bootstrap.connection.client();
    let cwd = bootstrap.directory.clone();
    let request = |command| client.request(Uuid::nil(), Uuid::nil(), command).unwrap();
    let read_settings = || match request(fintwind_client::Command::GetSettings) {
        fintwind_client::ResponsePayload::Settings { settings } => settings,
        _ => panic!("expected daemon settings"),
    };
    let original_settings = read_settings();
    if fixture.live_model.is_none() {
        assert!(matches!(
            request(fintwind_client::Command::StoreCustomProviderKey {
                binary: fixture.binary.clone(),
                directory: cwd.clone(),
                provider_id: "fixture".into(),
                key: "ssh-probe-fixture-key".into(),
            }),
            fintwind_client::ResponsePayload::Ack
        ));
    }
    let fintwind_client::ResponsePayload::ProviderProbe { probe, .. } =
        request(fintwind_client::Command::ProbeProvider {
            binary_override: Some(fixture.binary.to_string_lossy().into_owned()),
            directory: Some(cwd.clone()),
            discover_models: false,
            probe_version: false,
        })
    else {
        panic!("expected provider probe");
    };
    cx.set_assets(ely_gpui_component::Assets::before(crate::assets::Assets));
    cx.update(|cx| {
        crate::assets::register_fonts(cx).expect("register bundled fonts");
        gpui_kit::init(cx);
        ely_gpui_component::init(cx).expect("initialize Ely");
        crate::ely::init(cx);
        crate::input::init(cx);
        crate::ui::menu::init(cx);
        crate::theme::init(cx);
    });
    let (launch_tx, launch_rx) = std::sync::mpsc::channel();
    let mut profile = fixture.profile.clone();
    profile.fingerprint = None;
    let profile_id = profile.id;
    let picker_daemon = bootstrap.connection.supervisor();
    let picker_store = StateStore::ssh_workspace(picker_daemon.clone(), &bootstrap.state_dir);
    let (picker, picker_cx) = cx.add_window_view(|window, cx| {
        let desktop = Fintwind::new_with_store(window, cx, picker_daemon, picker_store);
        desktop.update(cx, |app, cx| {
            app.ssh.loaded = true;
            app.ssh.profiles.push(profile);
            app.ssh.test_launch = Some(launch_tx);
            app.open_add_project_dialog(window, cx);
        });
        Desktop(desktop, false)
    });
    picker_cx.run_until_parked();
    let picker_view = picker;
    let picker = picker_cx.read_entity(&picker_view, |view, _| view.0.clone());
    let mut location_origin = None;
    for mode in ["segment existing", "segment clone", "segment empty"] {
        let method = picker_cx.debug_bounds(mode).expect("Ely mode selector");
        picker_cx.simulate_click(method.center(), gpui::Modifiers::none());
        picker_cx.run_until_parked();
        let location = picker_cx
            .debug_bounds("add-project-location")
            .expect("location selector");
        if let Some(origin) = location_origin {
            assert_eq!(location.origin, origin, "location selector moved in {mode}");
        } else {
            location_origin = Some(location.origin);
        }
        picker_cx.simulate_click(location.center(), gpui::Modifiers::none());
        picker_cx.run_until_parked();
        let server = picker_cx
            .debug_bounds("menu-entry SSH fixture")
            .expect("saved SSH host");
        picker_cx.simulate_click(server.center(), gpui::Modifiers::none());
        picker_cx.run_until_parked();
        assert!(
            picker_cx.debug_bounds("menu-entry SSH fixture").is_none(),
            "location menu stayed open after selecting the host"
        );
    }
    let (input, folder) = picker_cx.read_entity(&picker, |app, _| {
        super::super::add_project_dialog::ssh_dialog_inputs(app)
    });
    picker_cx.run_until_parked();
    picker_cx.update(|window, cx| {
        let focus = picker.update(cx, |app, cx| {
            app.transcript_control_focus("add-project-confirm", cx)
        });
        window.focus(&focus, cx);
    });
    picker_cx.simulate_keystrokes("enter");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        picker_cx.run_until_parked();
        let (ready, error) = picker_cx.read_entity(&picker, |app, _| {
            (
                app.ssh_client_for(profile_id).is_some(),
                app.project_host_connection(Some(profile_id)).2,
            )
        });
        assert!(error.is_none(), "project picker connection failed: {error:?}");
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "saved host did not connect from project picker"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let pinned = read_profiles()
        .unwrap()
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .unwrap();
    assert_eq!(pinned.fingerprint, fixture.profile.fingerprint);
    let clone = picker_cx.debug_bounds("segment clone").unwrap();
    picker_cx.simulate_click(clone.center(), gpui::Modifiers::none());
    picker_cx.run_until_parked();
    let repository = picker_cx.read_entity(&picker, |app, _| {
        super::super::add_project_dialog::project_repository_input(app)
    });
    picker_cx.update(|window, cx| {
        repository.update(cx, |input, cx| {
            input.set_content("https://github.com/octocat/Hello-World", cx)
        });
        input.update(cx, |input, cx| input.set_content(fixture.workspace.clone(), cx));
        folder.update(cx, |input, cx| input.set_content(".git", cx));
        window.focus(&folder.read(cx).focus(), cx);
    });
    picker_cx.run_until_parked();
    let submit = picker_cx.debug_bounds("add-project-submit").unwrap();
    picker_cx.simulate_click(submit.center(), gpui::Modifiers::none());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        picker_cx.run_until_parked();
        let collision_toast = picker_cx.read_entity(&picker, |app, _| {
            app.toast.as_ref().is_some_and(|toast| {
                toast.tone == ToastTone::Alert && toast.message.contains(&tr!("project.target_exists"))
            })
        });
        if collision_toast {
            break;
        }
        assert!(Instant::now() < deadline, "clone collision did not become an error toast");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(picker_cx.read_entity(&picker_view, |view, _| view.1), "clone progress toast never rendered");
    let dismiss = picker_cx.debug_bounds("dismiss-notification-toast").unwrap();
    picker_cx.simulate_click(dismiss.center(), gpui::Modifiers::none());
    picker_cx.run_until_parked();
    assert!(picker_cx.read_entity(&picker, |app, _| app.toast.is_none() && app.add_project_dialog.is_some()));
    let empty = picker_cx.debug_bounds("segment empty").unwrap();
    picker_cx.simulate_click(empty.center(), gpui::Modifiers::none());
    picker_cx.run_until_parked();
    picker_cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.set_content(fixture.workspace.clone(), cx)
        });
        folder.update(cx, |input, cx| input.set_content("new-remote-project", cx));
        window.focus(&folder.read(cx).focus(), cx);
    });
    picker_cx.run_until_parked();
    picker_cx.simulate_keystrokes("enter");
    let deadline = Instant::now() + Duration::from_secs(15);
    let (chosen_id, chosen_path, chosen_name) = loop {
        picker_cx.run_until_parked();
        if let Ok(launch) = launch_rx.try_recv() {
            break launch;
        }
        assert!(
            Instant::now() < deadline,
            "dialog did not request an SSH project window"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(chosen_id, profile_id);
    assert_eq!(chosen_path, cwd.join("new-remote-project"));
    let workspace = chosen_path.to_str().unwrap().to_owned();
    let launch = prepare_project_launch(
        fixture.profile,
        workspace.clone(),
        chosen_name,
        fixture.state_dir,
    )
    .unwrap();
    let daemon = launch.connection.supervisor();
    let store = StateStore::ssh_workspace(daemon.clone(), &launch.state_dir);
    let (view, cx) = cx
        .add_window_view(|window, cx| Desktop(Fintwind::new_with_store(window, cx, daemon, store), false));
    let desktop = cx.read_entity(&view, |view, _| view.0.clone());
    let live = fixture.live_model.is_some();
    let id = cx.update(|_, cx| {
        desktop.update(cx, |app, cx| {
            app.open_ssh_project(launch, cx);
            assert_eq!(app.selected_project().unwrap().path, chosen_path);
            let id = app.state.selected_session.unwrap();
            let session = app.state.session_mut(id).unwrap();
            session.model = Some(fixture.live_model.clone().unwrap_or_else(|| "fixture/fixture-model".into()));
            if live {
                session.runtime_mode = fintwind_protocol::model::RuntimeMode::FullAccess;
            }
            session.workspace = SessionWorkspace::Local;
            app.probes = vec![probe];
            assert_eq!(
                app.native_binary_path().as_deref(),
                Some(fixture.binary.as_path())
            );
            app.submit_submission_for_session(
                id,
                ComposerSubmission::plain(
                    if live { LIVE_PROMPT } else { "Reply with a short greeting in plain text. Do not use tools." }.into(),
                ),
                cx,
            );
            id
        })
    });
    let until = Instant::now() + Duration::from_secs(if live { 210 } else { 35 });
    let mut observed_live = false;
    loop {
        cx.run_until_parked();
        let (streaming, complete, text, live_proof) = cx.update(|_, cx| {
            desktop.update(cx, |app, cx| {
                app.drain_event_pump(cx);
                let session = app.state.session_mut(id).unwrap();
                let streaming = session
                    .messages
                    .iter()
                    .any(|m| m.streaming && (live || m.content.contains("Hello from the")) && !m.content.is_empty());
                let complete = session.active_turn_id().is_none();
                let text = session
                    .messages
                    .iter()
                    .map(|m| m.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let live_proof = live.then(|| live_session_proof(session));
                (streaming, complete, text, live_proof)
            })
        });
        observed_live |= streaming;
        if let Some(mut proof) = live_proof {
            if (complete && proof["childCompleted"] == true) || Instant::now() >= until {
                let file_written = matches!(client.request(Uuid::nil(), Uuid::nil(), fintwind_client::Command::Workspace {
                    operation: fintwind_client::WorkspaceOperation::ReadTextFile {
                        root: chosen_path.clone(),
                        relative_path: "ssh-e2e.txt".into(),
                    },
                }), Ok(fintwind_client::ResponsePayload::Workspace {
                    result: fintwind_client::WorkspaceResult::TextFile { content }
                }) if content.trim() == "FINTWIND-SSH-FILE");
                proof["workspace"] = serde_json::json!(workspace);
                proof["observedLive"] = serde_json::json!(observed_live);
                proof["remoteFileVerified"] = serde_json::json!(file_written);
                proof["passed"] = serde_json::json!(["completed", "reasoning", "assistantText", "fileRead", "fileWrite", "childCompleted", "observedLive", "remoteFileVerified"]
                    .into_iter().all(|field| proof[field].as_bool() == Some(true)));
                std::fs::write(&fixture.proof, serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
                assert_eq!(proof["passed"], true, "real provider run did not meet all checks: {proof}; transcript: {text}");
                break;
            }
        } else if complete {
            assert!(
                observed_live,
                "no live text observed before settlement: {text}"
            );
            assert!(
                text.contains("Hello from the SSH stream fixture."),
                "{text}"
            );
            assert_eq!(
                read_settings(),
                original_settings,
                "opening a project must not update daemon settings"
            );
            std::fs::write(&fixture.proof, serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true, "workspace": workspace, "session": id,
                "selectedHost": chosen_id, "observedLive": observed_live, "settledText": "Hello from the SSH stream fixture.",
                "cloneProgressToast": true, "cloneErrorToastDismissibleOverDialog": true
            })).unwrap()).unwrap();
            break;
        }
        assert!(Instant::now() < until, "unfinished SSH task: {text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn live_session_proof(session: &fintwind_protocol::model::AgentSession) -> serde_json::Value {
    use fintwind_protocol::model::{ActivityKind, BackgroundWorkKind, BackgroundWorkStatus, MessageRole, TurnStatus};
    let activities = session.transcript_blocks.iter().flat_map(|block| &block.activities);
    let children = session.background_work.iter().filter(|work| work.item.key.kind == BackgroundWorkKind::Subagent);
    serde_json::json!({
        "session": session.id,
        "model": session.model,
        "completed": session.turns.last().is_some_and(|turn| turn.status == TurnStatus::Completed),
        "reasoning": activities.clone().any(|row| row.reasoning.as_ref().is_some_and(|part| !part.content.trim().is_empty())),
        "fileRead": activities.clone().any(|row| row.kind == ActivityKind::FileRead && row.complete && !row.failed),
        "fileWrite": activities.clone().any(|row| row.kind == ActivityKind::FileChange && row.complete && !row.failed),
        "activities": activities.map(|row| serde_json::json!({"kind": row.kind, "title": row.title, "complete": row.complete, "failed": row.failed})).collect::<Vec<_>>(),
        "children": children.clone().map(|work| serde_json::json!({"title": work.item.title, "status": work.item.status, "model": work.item.model})).collect::<Vec<_>>(),
        "childCompleted": children.clone().any(|work| work.item.status == BackgroundWorkStatus::Completed && work.transcript.messages.iter().any(|message| matches!(message.role, MessageRole::Assistant) && message.content.contains("FINTWIND-SSH-CHILD"))),
        "assistantText": session.messages.iter().any(|message| matches!(message.role, MessageRole::Assistant) && !message.content.trim().is_empty()),
        "finalMarker": session.messages.iter().any(|message| matches!(message.role, MessageRole::Assistant) && message.content.contains("FINTWIND-SSH-DONE")),
    })
}

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
    peer_profile: Option<Profile>,
    peer_workspace: Option<String>,
}

const LIVE_PROMPT: &str = "Work only inside the current test project. Use the file write tool to create ssh-e2e.txt containing FINTWIND-SSH-FILE, then use the file read tool to verify it. Use the subagent tool to launch a general subagent and wait for its result. Ask the subagent to read ssh-e2e.txt and report FINTWIND-SSH-CHILD with the file marker. After its tool result arrives, give a short final answer including FINTWIND-SSH-DONE.";

#[gpui::test]
#[ignore = "requires the isolated SSH daemon fixture"]
fn desktop_ssh_project_streams(test_app: &mut TestAppContext) {
    let cx = &mut *test_app;
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
    let cwd = bootstrap.project.path.clone();
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
    if std::env::var_os("FINTWIND_SSH_RESTART_PHASE").is_some() {
        let mut proof: serde_json::Value = serde_json::from_slice(&std::fs::read(&fixture.proof).unwrap()).unwrap();
        let expected: Uuid = serde_json::from_value(proof["session"].clone()).unwrap();
        let daemon = bootstrap.connection.supervisor();
        let store = StateStore::ssh_workspace(daemon.clone(), &bootstrap.state_dir);
        let (view, desktop_cx) = cx.add_window_view(|window, cx| {
            let view = Fintwind::new_with_store(window, cx, daemon, store);
            view.update(cx, |app, cx| {
                app.load_ssh_profiles_with_navigation(true, cx);
            });
            Desktop(view, false)
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            desktop_cx.run_until_parked();
            desktop_cx.update(|window, cx| {
                view.read(cx).0.clone().update(cx, |app, cx| app.restore_ssh_navigation(window, cx));
            });
            let restored = desktop_cx.read_entity(&view, |view, cx| {
                view.0.read(cx).active_ssh_workspace().is_some_and(|workspace| {
                    let app = workspace.read(cx);
                    app.state.selected_session == Some(expected)
                        && app.selected_session().is_some_and(|session| {
                            session.detail_loaded
                                && session.turns.last().is_some_and(|turn| turn.status == TurnStatus::Completed)
                                && session.messages.iter().filter(|message| {
                                    message.role == fintwind_protocol::model::MessageRole::User
                                }).count() == 1
                        })
                })
            });
            if restored { break; }
            assert!(Instant::now() < deadline, "new desktop process did not restore the selected SSH session");
            std::thread::sleep(Duration::from_millis(25));
        }
        proof["restoredInNewProcess"] = serde_json::json!(true);
        proof["restoredCompletedTurnWithoutDuplicateMessage"] = serde_json::json!(true);
        std::fs::write(&fixture.proof, serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
        return;
    }
    let mut profile = fixture.profile.clone();
    profile.fingerprint = None;
    let profile_id = profile.id;
    let local_workspace = std::env::var_os("FINTWIND_E2E_LOCAL_WORKSPACE").map(PathBuf::from);
    let picker_daemon = if local_workspace.is_some() {
        fintwind_client::DaemonSupervisor::connect(
            &std::env::var("FINTWIND_PROBE_LOCAL_SETTINGS_ADDRESS").unwrap(),
            std::env::var("FINTWIND_PROBE_LOCAL_SETTINGS_TOKEN").unwrap(),
        ).unwrap()
    } else {
        bootstrap.connection.supervisor()
    };
    let local_probe = local_workspace.as_ref().map(|path| {
        let response = picker_daemon.client().request(Uuid::nil(), Uuid::nil(),
            fintwind_client::Command::ProbeProvider {
                binary_override: Some(std::env::var("FINTWIND_E2E_LOCAL_OPENCODE").unwrap()),
                directory: Some(path.clone()),
                discover_models: false,
                probe_version: false,
            }).unwrap();
        let fintwind_client::ResponsePayload::ProviderProbe { probe, .. } = response else {
            panic!("expected Windows OpenCode probe");
        };
        probe
    });
    let picker_store = StateStore::ssh_workspace(picker_daemon.clone(), &bootstrap.state_dir);
    let (picker, picker_cx) = cx.add_window_view(|window, cx| {
        let desktop = Fintwind::new_with_store(window, cx, picker_daemon, picker_store);
        desktop.update(cx, |app, cx| {
            app.ssh.loaded = true;
            app.ssh.profiles.push(profile);
            app.ssh.test_location_picker = true;
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
    picker_cx.update(|_, cx| picker.update(cx, |app, _| {
        app.ssh.profiles.iter_mut().find(|profile| profile.id == profile_id).unwrap().user =
            format!("fintwind-e2e-{}", &Uuid::new_v4().simple().to_string()[..12]);
    }));
    let connect = picker_cx.debug_bounds("add-project-submit").unwrap();
    picker_cx.simulate_click(connect.center(), gpui::Modifiers::none());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        picker_cx.run_until_parked();
        if picker_cx.read_entity(&picker, |app, _| {
            app.toast.as_ref().is_some_and(|toast| {
                toast.tone == ToastTone::Alert && toast.message.contains(&tr!("ssh.auth_failed"))
            })
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "SSH authentication failure did not become an error toast");
        std::thread::sleep(Duration::from_millis(20));
    }
    let dismiss = picker_cx.debug_bounds("dismiss-notification-toast").unwrap();
    picker_cx.simulate_click(dismiss.center(), gpui::Modifiers::none());
    picker_cx.update(|_, cx| picker.update(cx, |app, _| {
        assert!(app.add_project_dialog.is_some());
        assert!(app.ssh_client_for(profile_id).is_none());
        app.ssh.profiles.iter_mut().find(|profile| profile.id == profile_id).unwrap().user =
            fixture.profile.user.clone();
    }));
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
    picker_cx.update(|_, cx| {
        assert_eq!(repository.read(cx).content(), "https://github.com/octocat/Hello-World");
        assert_eq!(input.read(cx).content(), fixture.workspace);
        assert_eq!(folder.read(cx).content(), ".git");
    });
    if std::env::var_os("FINTWIND_SSH_E2E_REJECTED_CLONE").is_some() {
        let inaccessible = format!("https://github.com/octocat/fintwind-e2e-{}", Uuid::new_v4().simple());
        picker_cx.update(|_, cx| {
            repository.update(cx, |input, cx| input.set_content(inaccessible.clone(), cx));
            folder.update(cx, |input, cx| input.set_content("new-remote-project", cx));
        });
        picker_cx.simulate_click(submit.center(), gpui::Modifiers::none());
        let deadline = Instant::now() + Duration::from_secs(135);
        loop {
            picker_cx.run_until_parked();
            if picker_cx.read_entity(&picker, |app, _| app.toast.as_ref().is_some_and(|toast| {
                toast.tone == ToastTone::Alert && toast.message.contains(&tr!("project.creation_failed"))
            })) {
                break;
            }
            assert!(Instant::now() < deadline, "inaccessible repository did not become an error toast");
            std::thread::sleep(Duration::from_millis(20));
        }
        picker_cx.update(|_, cx| {
            assert_eq!(repository.read(cx).content(), inaccessible);
            assert_eq!(input.read(cx).content(), fixture.workspace);
            assert_eq!(folder.read(cx).content(), "new-remote-project");
        });
    }
    let clone_disconnect = std::env::var_os("FINTWIND_SSH_E2E_CLONE_DISCONNECT").is_some();
    if clone_disconnect {
        eprintln!("SSH E2E: starting clone to interrupt after progress");
        picker_cx.update(|_, cx| {
            repository.update(cx, |input, cx| input.set_content("https://github.com/git/git", cx));
            folder.update(cx, |input, cx| input.set_content("interrupted-clone", cx));
        });
        picker_cx.simulate_click(submit.center(), gpui::Modifiers::none());
        let deadline = Instant::now() + Duration::from_secs(135);
        let executor = picker_cx.update(|_, cx| cx.background_executor().clone());
        loop {
            for _ in 0..32 {
                if !executor.scheduler_executor().scheduler().as_test().unwrap().tick() {
                    break;
                }
            }
            if picker_cx.read_entity(&picker, |app, _| app.toast.as_ref().is_some_and(|toast| {
                toast.tone == ToastTone::Alert && toast.message.contains(&tr!("project.creation_unknown"))
            })) {
                break;
            }
            assert!(Instant::now() < deadline, "interrupted clone did not report an unconfirmed outcome");
            std::thread::sleep(Duration::from_millis(20));
        }
        picker_cx.update(|_, cx| {
            assert_eq!(repository.read(cx).content(), "https://github.com/git/git");
            assert_eq!(input.read(cx).content(), fixture.workspace);
            assert_eq!(folder.read(cx).content(), "interrupted-clone");
        });
        eprintln!("SSH E2E: interruption confirmed and clone inputs retained");
        let dismiss = picker_cx.debug_bounds("dismiss-notification-toast").unwrap();
        picker_cx.simulate_click(dismiss.center(), gpui::Modifiers::none());
    }
    let registration_retry = std::env::var_os("FINTWIND_SSH_E2E_REGISTRATION_RETRY").is_some();
    if registration_retry {
        picker_cx.update(|_, cx| {
            repository.update(cx, |input, cx| input.set_content("https://github.com/octocat/Hello-World", cx));
            folder.update(cx, |input, cx| input.set_content("new-remote-project", cx));
        });
        picker_cx.simulate_click(submit.center(), gpui::Modifiers::none());
        let deadline = Instant::now() + Duration::from_secs(135);
        loop {
            picker_cx.run_until_parked();
            if picker_cx.read_entity(&picker, |app, _| app.toast.as_ref().is_some_and(|toast| {
                toast.tone == ToastTone::Alert && toast.message.contains(&tr!("ssh.offline"))
            })) {
                break;
            }
            assert!(Instant::now() < deadline, "successful clone did not report the injected registration failure");
            std::thread::sleep(Duration::from_millis(20));
        }
        let dismiss = picker_cx.debug_bounds("dismiss-notification-toast").unwrap();
        picker_cx.simulate_click(dismiss.center(), gpui::Modifiers::none());
        picker_cx.run_until_parked();
    }
    if !clone_disconnect {
        let mode = picker_cx.debug_bounds(if registration_retry { "segment clone" } else { "segment empty" }).unwrap();
        picker_cx.simulate_click(mode.center(), gpui::Modifiers::none());
        picker_cx.run_until_parked();
    }
    picker_cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.set_content(fixture.workspace.clone(), cx)
        });
        if clone_disconnect {
            repository.update(cx, |input, cx| {
                input.set_content("https://github.com/octocat/Hello-World", cx);
            });
        } else {
            folder.update(cx, |input, cx| input.set_content("new-remote-project", cx));
        }
        window.focus(&folder.read(cx).focus(), cx);
    });
    if !clone_disconnect {
        picker_cx.run_until_parked();
    }
    picker_cx.simulate_keystrokes("enter");
    if clone_disconnect {
        eprintln!("SSH E2E: submitted clone retry at the same destination");
    }
    let deadline = Instant::now() + Duration::from_secs(if clone_disconnect { 135 } else { 15 });
    let executor = picker_cx.update(|_, cx| cx.background_executor().clone());
    let desktop = loop {
        for _ in 0..32 {
            if !executor.scheduler_executor().scheduler().as_test().unwrap().tick() {
                break;
            }
        }
        if let Some(workspace) = picker_cx.read_entity(&picker, |app, _| app.active_ssh_workspace()) {
            break workspace;
        }
        assert!(
            Instant::now() < deadline,
            "dialog did not open the SSH project in the current window"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let chosen_id = picker_cx.read_entity(&picker, |app, _| app.active_ssh_host().unwrap());
    assert_eq!(chosen_id, profile_id);
    let project = picker_cx.read_entity(&desktop, |app, _| app.selected_project().unwrap().clone());
    let chosen_path = project.path.clone();
    assert_eq!(chosen_path, cwd.join(if clone_disconnect { "interrupted-clone" } else { "new-remote-project" }));
    let workspace = chosen_path.to_str().unwrap().to_owned();
    let launch = prepare_project_launch(
        fixture.profile.clone(),
        workspace.clone(),
        project.name.clone(),
        fixture.state_dir.clone(),
    )
    .unwrap();
    let cx = picker_cx;
    let peer_verified = if let (Some(profile), Some(path)) = (&fixture.peer_profile, &fixture.peer_workspace) {
        let peer = prepare_project_launch(profile.clone(), path.clone(), project.name.clone(), fixture.state_dir.join("peer")).unwrap();
        let loaded = load_ssh_workspace(profile.id, &peer.connection.supervisor()).unwrap();
        cx.update(|window, cx| {
            picker.update(cx, |app, cx| {
                app.ssh.profiles.push(profile.clone());
                app.ssh.connections.insert(profile.id, Arc::new(peer.connection));
                app.ssh.loaded_workspaces.insert(profile.id, loaded);
                assert!(app.open_registered_ssh_project(profile.id, peer.project, window, cx));
                let active = app.active_ssh_workspace().unwrap();
                assert_ne!(active, desktop);
                assert_eq!(active.read(cx).selected_project().unwrap().path, PathBuf::from(path));
                assert!(app.open_registered_ssh_project(profile_id, project.clone(), window, cx));
                assert_eq!(app.active_ssh_workspace(), Some(desktop.clone()));
            });
        });
        true
    } else {
        false
    };
    let client_updates = cx.read_entity(&desktop, |app, _| app.daemon.subscribe_clients());
    client_updates.try_recv().expect("initial SSH daemon client");
    cx.update(|_, cx| {
        picker.update(cx, |app, _| {
            Arc::get_mut(app.ssh.connections.get_mut(&profile_id).unwrap()).unwrap().replace_tunnel(launch.connection);
        });
    });
    let reconnect_deadline = Instant::now() + Duration::from_secs(15);
    while client_updates.try_recv().is_err() {
        assert!(Instant::now() < reconnect_deadline, "embedded workspace did not reconnect through the replacement tunnel");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
    let live = fixture.live_model.is_some();
    cx.update(|_, cx| {
        desktop.update(cx, |app, cx| app.open_settings_page(SettingsPage::McpServers, cx));
        picker.update(cx, |app, cx| {
            assert_eq!(app.mode, WorkspaceMode::Settings);
            assert_eq!(app.settings_ssh_host(), Some(profile_id));
            app.mode = WorkspaceMode::Sessions;
            cx.notify();
        });
    });
    let id = cx.update(|_, cx| {
        desktop.update(cx, |app, cx| {
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
    let observed_from = Instant::now();
    let until = observed_from + Duration::from_secs(if live { 210 } else { 35 });
    let mut settled_at = None;
    let mut child_result_at = None;
    let mut observed_live = false;
    let quit_during_turn = std::env::var_os("FINTWIND_SSH_QUIT_DURING_TURN").is_some();
    let mut switched_during_stream = false;
    let mut reconnected_during_stream = false;
    let local_session_id = cx.update(|_, cx| {
        picker.update(cx, |app, cx| {
            let project_id = if let Some(path) = &local_workspace {
                let id = Uuid::new_v4();
                app.state.projects.push(Project { id, name: "Windows fixture".into(), path: path.clone(), created_at: 0 });
                id
            } else {
                project.id
            };
            let mut session = AgentSession::new(project_id);
            let session_id = session.id;
            session.model = Some("fixture/fixture-model".into());
            app.state.sessions.push(session);
            if let Some(probe) = local_probe {
                app.probes = vec![probe];
                app.submit_submission_for_session(session_id, ComposerSubmission::plain(
                    "Reply with a short greeting in plain text. Do not use tools.".into()), cx);
            }
            session_id
        })
    });
    let mut concurrent_turns = false;
    let mut upgraded_client = None;
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
        let local_finished = cx.update(|_, cx| picker.update(cx, |app, cx| {
            app.drain_event_pump(cx);
            let session = app.state.session_mut(local_session_id).unwrap();
            concurrent_turns |= streaming && !complete && session.active_turn_id().is_some();
            session.active_turn_id().is_none() && session.messages.iter()
                .any(|message| message.content.contains("Hello from the SSH stream fixture."))
        }));
        if streaming && !complete && quit_during_turn {
            save_ssh_navigation(Some(profile_id)).unwrap();
            let before = request(fintwind_client::Command::HydrateSession { session_id: id });
            assert!(matches!(before,
                fintwind_client::ResponsePayload::Session { session: Some(session) }
                    if session.active_turn_id().is_some()
            ), "remote turn must be active when the desktop quits");
            let busy = require_idle_daemon(&client).expect_err("active remote turn must block daemon updates");
            assert_eq!(busy.to_string(), tr!("ssh.update_busy"));
            test_app.quit();
            let deadline = Instant::now() + Duration::from_secs(90);
            loop {
                let response = client.request_with_timeout(
                    Uuid::nil(),
                    Uuid::nil(),
                    fintwind_client::Command::HydrateSession { session_id: id },
                    Duration::from_secs(5),
                ).unwrap();
                if let fintwind_client::ResponsePayload::Session { session: Some(session) } = response
                    && session.turns.last().is_some_and(|turn| turn.status == TurnStatus::Completed)
                {
                    require_idle_daemon(&client).expect("settled remote turn must permit daemon updates");
                    let proof = serde_json::json!({
                        "passed": true,
                        "session": id,
                        "quitDuringStreaming": true,
                        "remoteTurnActiveBeforeQuit": true,
                        "remoteTurnCompletedAfterQuit": true,
                        "updateBlockedWhileBusy": true,
                        "updateAllowedWhenIdle": true,
                    });
                    std::fs::write(&fixture.proof, serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
                    return;
                }
                assert!(Instant::now() < deadline, "remote turn did not complete after desktop quit");
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        if streaming && !complete && !switched_during_stream {
            cx.update(|window, cx| {
                desktop.update(cx, |app, cx| {
                    app.route_sidebar_host_action(window, cx, move |app, window, cx| {
                        app.switch_sidebar_host(None, window, cx);
                        app.select_session(local_session_id, cx);
                    });
                });
                if live {
                    desktop.read(cx).daemon.client().disconnect();
                }
            });
            cx.run_until_parked();
            cx.read_entity(&picker, |app, _| {
                assert!(app.active_ssh_workspace().is_none());
                assert_eq!(app.state.selected_session, Some(local_session_id));
            });
            switched_during_stream = true;
        }
        if live && switched_during_stream && client_updates.try_recv().is_ok() {
            reconnected_during_stream = true;
        }
        if let Some(mut proof) = live_proof {
            if complete && proof["completed"] == true {
                settled_at.get_or_insert_with(|| observed_from.elapsed().as_millis());
            }
            if proof["childCompleted"] == true {
                child_result_at.get_or_insert_with(|| observed_from.elapsed().as_millis());
            }
            let transcript_ready = complete
                && proof["completed"] == true
                && proof["childCompleted"] == true
                && proof["assistantText"] == true;
            if (transcript_ready && (local_workspace.is_none() || local_finished)) || Instant::now() >= until {
                if local_workspace.is_some() {
                    assert!(concurrent_turns && local_finished, "Windows and SSH turns must overlap and both complete");
                    proof["windowsAndRemoteTurnsOverlapped"] = serde_json::json!(true);
                }
                eprintln!("SSH E2E: checking remote file and session catalog");
                proof["desktopTurnSettledMs"] = serde_json::json!(settled_at);
                proof["desktopChildResultMs"] = serde_json::json!(child_result_at);
                proof["desktopTurnAndChildObservedTogetherMs"] = serde_json::json!(observed_from.elapsed().as_millis());
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
                assert!(switched_during_stream, "remote stream did not survive switching views");
                proof["switchedDuringStream"] = serde_json::json!(switched_during_stream);
                proof["openedLocalSidebarSession"] = serde_json::json!(true);
                proof["switchedBetweenRemoteDaemons"] = serde_json::json!(peer_verified);
                let catalog_deadline = Instant::now() + Duration::from_secs(12);
                let executor = cx.update(|_, cx| cx.background_executor().clone());
                loop {
                    executor.advance_clock(Duration::from_millis(100));
                    cx.run_until_parked();
                    if cx.read_entity(&picker, |app, _| {
                        app.ssh_project_catalog(profile_id)
                            .is_some_and(|catalog| catalog.sessions.iter().any(|session| session.id == id))
                    }) {
                        break;
                    }
                    assert!(Instant::now() < catalog_deadline, "remote session missing from host catalog");
                    std::thread::sleep(Duration::from_millis(20));
                }
                cx.update(|window, cx| {
                    picker.update(cx, |app, cx| {
                        app.switch_sidebar_host(Some(profile_id), window, cx);
                        assert!(app.active_ssh_workspace().is_some_and(|active| active == desktop));
                    });
                    assert_eq!(desktop.read(cx).state.selected_session, Some(id));
                });
                proof["reopenedFromHostCatalog"] = serde_json::json!(true);
                proof["desktopSessionReopenedMs"] = serde_json::json!(observed_from.elapsed().as_millis());
                proof["replacementTunnelUsed"] = serde_json::json!(true);
                proof["reconnectedDuringTask"] = serde_json::json!(reconnected_during_stream);
                assert!(reconnected_during_stream, "daemon client did not reconnect during the remote task");
                let user_messages = cx.read_entity(&desktop, |app, _| {
                    app.state.sessions.iter().find(|session| session.id == id).unwrap()
                        .messages.iter().filter(|message| {
                            message.role == fintwind_protocol::model::MessageRole::User
                        }).count()
                });
                assert_eq!(user_messages, 1, "reconnecting duplicated the submitted message");
                proof["userMessageCount"] = serde_json::json!(user_messages);
                proof["mcpSettingsTargetedActiveHost"] = serde_json::json!(true);
                eprintln!("SSH E2E: saving workspace navigation");
                let (store, mut persisted) = cx.read_entity(&desktop, |app, _| {
                    (app.store.clone(), app.state.clone())
                });
                store.flush(&mut persisted).expect("save remote workspace navigation");
                proof["desktopNavigationFlushedMs"] = serde_json::json!(observed_from.elapsed().as_millis());
                save_ssh_navigation(Some(profile_id)).expect("save selected host");
                let daemon = cx.read_entity(&desktop, |app, _| app.daemon.clone());
                let loaded = load_ssh_workspace(profile_id, &daemon).expect("load saved workspace");
                proof["desktopWorkspaceLoadedMs"] = serde_json::json!(observed_from.elapsed().as_millis());
                eprintln!("SSH E2E: dispatching workspace restore");
                cx.update(|window, cx| {
                    picker.update(cx, |app, cx| {
                        app.ssh.workspaces.remove(&profile_id);
                        app.ssh.loaded_workspaces.insert(profile_id, loaded);
                        app.ssh.active_workspace = None;
                        app.ssh.restoring_workspace = read_ssh_navigation();
                        app.restore_ssh_navigation(window, cx);
                    });
                });
                proof["desktopRestoreDispatchMs"] = serde_json::json!(observed_from.elapsed().as_millis());
                cx.update(|_, cx| {
                    let restored = picker.read(cx).active_ssh_workspace().expect("restored SSH workspace");
                    assert_ne!(restored, desktop);
                    assert_eq!(restored.read(cx).state.selected_session, Some(id));
                });
                proof["restoredSavedSessionInNewWorkspace"] = serde_json::json!(true);
                proof["desktopWorkspaceRestoredMs"] = serde_json::json!(observed_from.elapsed().as_millis());
                if std::env::var_os("FINTWIND_SSH_E2E_DEPLOY_ROOT").is_some() {
                    let runtime = tunnel::RUNTIME.as_ref().unwrap();
                    let managed_port = || runtime.block_on(async {
                        let (ssh, _) = authenticate(&fixture.profile).await.unwrap();
                        match deploy::detect(&ssh).await.unwrap() {
                            deploy::Detection::Running { port, .. } => port,
                            _ => panic!("expected a running isolated managed daemon"),
                        }
                    });
                    let previous_port = managed_port();
                    assert_eq!(Some(previous_port), fixture.profile.daemon_port);
                    let restored = cx.read_entity(&picker, |app, _| app.active_ssh_workspace().unwrap());
                    cx.update(|_, cx| picker.update(cx, |app, cx| {
                        let mut profile = app.ssh.profiles.iter().find(|profile| profile.id == profile_id).unwrap().clone();
                        profile.daemon_port = None;
                        app.ssh_connect(profile, SshConnectionAction::Deploy, cx);
                    }));
                    let deadline = Instant::now() + Duration::from_secs(90);
                    loop {
                        cx.run_until_parked();
                        let done = cx.read_entity(&picker, |app, _| !app.ssh.busy);
                        if done {
                            break;
                        }
                        assert!(Instant::now() < deadline, "desktop deployment did not finish");
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    cx.read_entity(&picker, |app, _| {
                        assert!(app.ssh.connection_errors.get(&profile_id).is_none(), "deployment connection error: {:?}", app.ssh.connection_errors.get(&profile_id));
                        assert!(app.ssh.backend_errors.get(&profile_id).is_none(), "deployment backend error: {:?}", app.ssh.backend_errors.get(&profile_id));
                        assert_eq!(app.active_ssh_workspace(), Some(restored.clone()));
                    });
                    assert_ne!(managed_port(), previous_port, "managed daemon did not restart on a new port");
                    let daemon = cx.read_entity(&restored, |app, _| app.daemon.clone());
                    let connections = daemon.subscribe_clients();
                    let deadline = Instant::now() + Duration::from_secs(15);
                    loop {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        let candidate = connections.recv_timeout(remaining).expect("desktop reconnected after deployment");
                        if !candidate.is_disconnected() {
                            break;
                        }
                    }
                    let session = fintwind_client::persistence::hydrate_session(&daemon, id)
                        .expect("read upgraded daemon session").expect("session survived upgrade");
                    assert_eq!(live_session_proof(&session)["completed"], true);
                    upgraded_client = Some(daemon.client());
                    proof["desktopDeploymentPreservedSession"] = serde_json::json!(true);
                }
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
                "cloneProgressToast": true, "cloneErrorToastDismissibleOverDialog": true,
                "sshAuthenticationErrorToastAndRetry": true,
                "reusedCloneAfterRegistrationFailure": registration_retry,
                "interruptedCloneReportedUnconfirmed": clone_disconnect,
                "interruptedCloneRetriedAtSamePath": clone_disconnect,
                "openedLocalSidebarSession": switched_during_stream,
                "switchedBetweenRemoteDaemons": peer_verified
            })).unwrap()).unwrap();
            break;
        }
        assert!(Instant::now() < until, "unfinished SSH task: {text}");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!quit_during_turn, "no active streaming turn was observed before desktop quit");
    let quit_started = Instant::now();
    test_app.quit();
    assert!(matches!(
        upgraded_client.as_ref().unwrap_or(&client).request(
            Uuid::nil(), Uuid::nil(), fintwind_client::Command::LoadTaskState,
        ),
        Ok(fintwind_client::ResponsePayload::TaskState { .. })
    ));
    let mut proof: serde_json::Value = serde_json::from_slice(&std::fs::read(&fixture.proof).unwrap()).unwrap();
    proof["daemonReachableAfterDesktopQuit"] = serde_json::json!(true);
    proof["desktopQuitAndDaemonProbeMs"] = serde_json::json!(quit_started.elapsed().as_millis());
    std::fs::write(&fixture.proof, serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
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

use super::*;
use gpui::TestAppContext;

struct Desktop(Entity<Fintwind>);

impl Render for Desktop {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(
            self.0
                .update(cx, |app, cx| app.render_add_project_dialog(cx)),
        )
    }
}

#[derive(Deserialize)]
struct Fixture {
    profile: Profile,
    workspace: String,
    binary: PathBuf,
    state_dir: PathBuf,
    proof: PathBuf,
}

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
    assert!(matches!(
        request(fintwind_client::Command::StoreCustomProviderKey {
            binary: fixture.binary.clone(),
            directory: cwd.clone(),
            provider_id: "fixture".into(),
            key: "ssh-probe-fixture-key".into(),
        }),
        fintwind_client::ResponsePayload::Ack
    ));
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
    let profile = fixture.profile.clone();
    let profile_id = profile.id;
    let picker_daemon = bootstrap.connection.supervisor();
    let picker_store = StateStore::ssh_workspace(picker_daemon.clone(), &bootstrap.state_dir);
    let (picker, picker_cx) = cx.add_window_view(|window, cx| {
        let desktop = Fintwind::new_with_store(window, cx, picker_daemon, picker_store);
        desktop.update(cx, |app, cx| {
            app.ssh.loaded = true;
            app.ssh.profiles.push(profile);
            app.ssh.ready.insert(profile_id);
            app.ssh.connections.insert(profile_id, bootstrap.connection);
            app.ssh.test_launch = Some(launch_tx);
            app.open_add_project_dialog(window, cx);
        });
        Desktop(desktop)
    });
    picker_cx.run_until_parked();
    let picker = picker_cx.read_entity(&picker, |view, _| view.0.clone());
    let handle = picker_cx
        .update(|_, cx| picker.update(cx, |app, cx| app.menu_handle("add-project-location", cx)));
    picker_cx.update(|window, cx| window.focus(handle.trigger_focus_handle(), cx));
    picker_cx.simulate_keystrokes("enter");
    assert!(handle.is_open(), "project location menu did not open");
    picker_cx.run_until_parked();
    picker_cx.update(|window, cx| window.focus(handle.focus_handle(), cx));
    picker_cx.simulate_keystrokes("down down enter");
    assert!(!handle.is_open(), "host selection did not close the menu");
    let input = picker_cx.read_entity(&picker, |app, _| {
        super::super::add_project_dialog::ssh_dialog_path(app)
    });
    picker_cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.set_content(fixture.workspace.clone(), cx)
        });
        window.focus(&input.read(cx).focus(), cx);
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
    assert_eq!(chosen_path, cwd);
    let launch = prepare_project_launch(
        fixture.profile,
        chosen_path.to_str().unwrap().to_owned(),
        chosen_name,
        fixture.state_dir,
    )
    .unwrap();
    let daemon = launch.connection.supervisor();
    let store = StateStore::ssh_workspace(daemon.clone(), &launch.state_dir);
    let (view, cx) = cx
        .add_window_view(|window, cx| Desktop(Fintwind::new_with_store(window, cx, daemon, store)));
    let desktop = cx.read_entity(&view, |view, _| view.0.clone());
    let id = cx.update(|_, cx| {
        desktop.update(cx, |app, cx| {
            app.open_ssh_project(launch, cx);
            assert_eq!(app.selected_project().unwrap().path, cwd);
            let id = app.state.selected_session.unwrap();
            let session = app.state.session_mut(id).unwrap();
            session.model = Some("fixture/fixture-model".into());
            session.workspace = SessionWorkspace::Local;
            app.probes = vec![probe];
            app.submit_submission_for_session(
                id,
                ComposerSubmission::plain(
                    "Reply with a short greeting in plain text. Do not use tools.".into(),
                ),
                cx,
            );
            id
        })
    });
    let until = Instant::now() + Duration::from_secs(35);
    let mut observed_live = false;
    loop {
        cx.run_until_parked();
        let (live, complete, text) = cx.update(|_, cx| {
            desktop.update(cx, |app, cx| {
                app.drain_driver_events(cx);
                let session = app.state.session_mut(id).unwrap();
                let live = session
                    .messages
                    .iter()
                    .any(|m| m.streaming && m.content.contains("Hello from the"));
                let complete = session.active_turn_id().is_none();
                let text = session
                    .messages
                    .iter()
                    .map(|m| m.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                (live, complete, text)
            })
        });
        observed_live |= live;
        if complete {
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
                "passed": true, "workspace": fixture.workspace, "session": id,
                "selectedHost": chosen_id, "observedLive": observed_live, "settledText": "Hello from the SSH stream fixture."
            })).unwrap()).unwrap();
            break;
        }
        assert!(Instant::now() < until, "unfinished SSH task: {text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

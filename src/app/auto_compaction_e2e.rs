use super::*;
use gpui::TestAppContext;

struct HeadlessApp(Entity<Fintwind>);

impl Render for HeadlessApp {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[gpui::test]
#[ignore = "run with scripts/auto-compaction-e2e.py for isolated data and process ownership"]
fn compaction_settings_e2e(cx: &mut TestAppContext) {
    cx.background_executor.allow_parking();
    let directory = std::path::PathBuf::from(std::env::var_os("FINTWIND_E2E_DIRECTORY").unwrap());
    let daemon = fintwind_client::DaemonSupervisor::connect(
        &std::env::var("FINTWIND_DAEMON_ADDRESS").unwrap(),
        std::env::var("FINTWIND_DAEMON_TOKEN").unwrap(),
    )
    .unwrap();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::input::init(cx);
        crate::ui::menu::init(cx);
        crate::theme::init(cx);
    });
    let desktop = directory.join("desktop");
    std::fs::create_dir_all(&desktop).unwrap();
    let store = StateStore::remote_in(daemon.clone(), &desktop);
    let (view, cx) = cx.add_window_view(|window, cx| {
        HeadlessApp(Fintwind::new_with_store(window, cx, daemon, store))
    });
    let app = cx.read_entity(&view, |view, _| view.0.clone());
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            assert!(app.state.background_compact_enabled);
            assert_eq!(app.state.auto_compact_threshold_percent, 85);
            for invalid in ["0%", "100%", "500K"] {
                app.auto_compact_threshold_input.update(cx, |input, cx| {
                    input.set_content(invalid.to_owned(), cx);
                });
                app.confirm_auto_compact_threshold(cx);
                assert_eq!(app.auto_compact_threshold_input.read(cx).content(), "85%");
            }
            app.auto_compact_threshold_input.update(cx, |input, cx| {
                input.set_content("72%".to_owned(), cx);
            });
            app.confirm_auto_compact_threshold(cx);
            app.toggle_background_compact(cx);
            app.set_auto_compact_enabled(false, cx);
            app.store.flush(&mut app.state).unwrap();
        });
    });
    let config = directory.join("daemon/fintwind-opencode.json");
    let expected = serde_json::json!({"auto": false, "background": false, "buffer": "28%"});
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        if value["compaction"] == expected {
            break;
        }
        assert!(Instant::now() < deadline, "daemon did not apply settings: {value}");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

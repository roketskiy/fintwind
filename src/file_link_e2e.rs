//! Native hit-testing through Markdown, its link callback and the shared menu.
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use crate::md::render::{Ctx, MarkdownView, Metrics, Palette, TranscriptSelection, markdown};
use crate::ui::menu::{ContextMenuHandle, MenuItem, context_menu};
use gpui::{
    Context, Modifiers, MouseButton, MouseDownEvent, Render, TestAppContext, Window, div, point,
    prelude::*, px,
};

struct Harness {
    markdown: MarkdownView,
    menu: ContextMenuHandle,
    opened: Rc<RefCell<Vec<String>>>,
    external: Rc<RefCell<Vec<String>>>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&crate::theme::Theme::light());
        let menu = self.menu.clone();
        let opened = self.opened.clone();
        let external = self.external.clone();
        let handler = Rc::new(
            move |target: &str, window: &mut Window, cx: &mut gpui::App| {
                if let Some(path) =
                    crate::file_target::resolve(target, Some(Path::new("C:/workspace")))
                {
                    let opened = opened.clone();
                    menu.set_context_items(vec![MenuItem::new(
                        "Open with default app",
                        move |_, _| {
                            opened
                                .borrow_mut()
                                .push(path.to_string_lossy().into_owned());
                        },
                    )]);
                    menu.open_at_pointer(window, cx);
                } else {
                    external.borrow_mut().push(target.to_owned());
                }
            },
        );
        let ctx = Ctx::new(
            "file-link-e2e",
            &palette,
            Metrics::body(),
            TranscriptSelection::default(),
        )
        .with_link_handler(handler);
        div()
            .w(px(500.0))
            .h(px(300.0))
            .relative()
            .children(markdown(&self.markdown, &ctx))
            .child(context_menu(
                div().absolute(),
                "file-menu-e2e",
                &self.menu,
                |_| Vec::new(),
            ))
    }
}

#[gpui::test]
fn inline_file_menu_e2e(cx: &mut TestAppContext) {
    // Failures: no inline hit target; eager launch before confirmation;
    // lost relative base or URL decoding; web links intercepted; code treated
    // as a path; dismiss still launches; keyboard cannot confirm the menu.
    cx.update(crate::ui::menu::init);
    let scenarios = [
        (
            "relative-code",
            "`src/lib.rs`",
            Some("C:/workspace/src/lib.rs"),
            None,
        ),
        (
            "absolute-code",
            "`C:\\work\\report.pdf`",
            Some("C:/work/report.pdf"),
            None,
        ),
        (
            "file-url",
            "[report](file:///C:/work/report%20final.pdf)",
            Some("C:/work/report final.pdf"),
            None,
        ),
        (
            "relative-link-location",
            "[source](src/lib.rs:12:3)",
            Some("C:/workspace/src/lib.rs"),
            None,
        ),
        (
            "web-link",
            "[web](https://example.com)",
            None,
            Some("https://example.com"),
        ),
        ("ordinary-code", "`some_identifier`", None, None),
        (
            "math-paragraph",
            "`src/lib.rs` and $x^2$",
            Some("C:/workspace/src/lib.rs"),
            None,
        ),
    ];
    let mut results = Vec::new();
    for (name, source, expected, web) in scenarios {
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut markdown = MarkdownView::new();
            markdown.set_text(source, false);
            Harness {
                markdown,
                menu: ContextMenuHandle::new(cx),
                opened: Rc::default(),
                external: Rc::default(),
            }
        });
        cx.run_until_parked();
        let target = point(px(15.0), px(10.0));
        cx.simulate_mouse_move(target, None, Modifiers::none());
        cx.simulate_event(MouseDownEvent {
            position: target,
            button: MouseButton::Left,
            click_count: 1,
            ..Default::default()
        });
        cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
        for _ in 0..3 {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
        let before = cx.read_entity(&view, |view, _| {
            (view.menu.is_open(), view.opened.borrow().len())
        });
        let mut dismissed = false;
        if expected.is_some() && before.0 {
            // TestAppContext has no platform display-link ticks. Put focus on
            // the rendered card after its deferred element joins the tree.
            let menu = cx.read_entity(&view, |view, _| view.menu.clone());
            cx.update(|window, cx| window.focus(menu.focus_handle(), cx));
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
            dismissed = cx.read_entity(&view, |view, _| {
                !view.menu.is_open() && view.opened.borrow().is_empty()
            });
            cx.simulate_event(MouseDownEvent {
                position: target,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            });
            cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
            for _ in 0..3 {
                cx.update(|window, _| window.refresh());
                cx.run_until_parked();
            }
            cx.update(|window, cx| window.focus(menu.focus_handle(), cx));
            cx.simulate_keystrokes("down enter");
            cx.run_until_parked();
        }
        let (opened, external) = cx.read_entity(&view, |view, _| {
            (
                view.opened
                    .borrow()
                    .iter()
                    .map(|path| path.replace('\\', "/"))
                    .collect::<Vec<_>>(),
                view.external.borrow().clone(),
            )
        });
        let expected_paths = expected.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let expected_web = web.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let passed = before == (expected.is_some(), 0)
            && (expected.is_none() || dismissed)
            && opened == expected_paths
            && external == expected_web;
        results.push(serde_json::json!({"scenario": name, "passed": passed,
            "menuBeforeConfirmation": before.0, "openedBeforeConfirmation": before.1,
            "dismissedWithoutOpening": dismissed, "opened": opened, "external": external}));
    }
    let output = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/file-link-e2e")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(
        output.join("proof.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
    eprintln!("file link E2E proof: {}", output.display());
    assert!(
        results.iter().all(|result| result["passed"] == true),
        "{results:#?}"
    );
}

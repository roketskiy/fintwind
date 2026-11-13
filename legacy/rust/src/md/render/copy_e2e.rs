//! Headless component E2E: real Markdown hit testing, focus, input actions and
//! clipboard dispatch. No daemon, child processes or user data are used.

use super::*;
use crate::input::ComposerInput;
use gpui::{Context, Entity, FocusHandle, Modifiers, Render, TestAppContext};

struct Harness {
    root_focus: FocusHandle,
    transcript_focus: FocusHandle,
    input: Entity<ComposerInput>,
    markdown: MarkdownView,
    selection: TranscriptSelection,
    focus_selection: bool,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&Theme::light());
        let ctx = Ctx::new(
            "copy-e2e",
            &palette,
            Metrics::body(),
            self.selection.clone(),
        );
        let selection = self.selection.clone();
        let focus = self.focus_selection.then(|| self.transcript_focus.clone());
        let copy = self.selection.clone();
        div().track_focus(&self.root_focus).child(
            div()
                .key_context("Fintwind")
                .on_action(move |_: &crate::CopySelection, _, cx| {
                    if let Some(text) = copy.selection.borrow().selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    } else {
                        cx.propagate();
                    }
                })
                .w(px(400.0))
                .flex()
                .flex_col()
                .child(
                    div()
                        .when(self.focus_selection, |surface| {
                            surface.track_focus(&self.transcript_focus)
                        })
                        .child(frame_reset(self.selection.clone()))
                        .children(markdown(&self.markdown, &ctx))
                        .child(
                            canvas(
                                |_, _, _| (),
                                move |_, _, window, _| match focus.as_ref() {
                                    Some(focus) => install_selection_input_with_focus(
                                        window, &selection, focus,
                                    ),
                                    None => install_selection_input(window, &selection),
                                },
                            )
                            .absolute()
                            .size(px(0.0)),
                        ),
                )
                .child(self.input.clone()),
        )
    }
}

#[gpui::test]
fn transcript_selection_keyboard_copy_e2e(cx: &mut TestAppContext) {
    // Failure modes: focus stays outside the app key context; an old input
    // selection wins over freshly selected prose; copying prose breaks later
    // input copying; focus changes accidentally clear the Markdown selection.
    cx.update(|cx| {
        crate::input::init(cx);
        cx.bind_keys([gpui::KeyBinding::new(
            "secondary-c",
            crate::CopySelection,
            Some("Fintwind"),
        )]);
    });
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut markdown = MarkdownView::new();
        markdown.set_text("Transcript selected text", false);
        Harness {
            root_focus: cx.focus_handle(),
            transcript_focus: cx.focus_handle(),
            input: cx.new(|cx| ComposerInput::new(window, cx)),
            markdown,
            selection: TranscriptSelection::default(),
            focus_selection: false,
        }
    });
    cx.run_until_parked();
    let (root, transcript, input, selection) = cx.read_entity(&view, |view, _| {
        (
            view.root_focus.clone(),
            view.transcript_focus.clone(),
            view.input.clone(),
            view.selection.clone(),
        )
    });
    // Reproduce the old surface: text is visibly selected, but the shortcut
    // never reaches the app because focus remains on its outer ancestor.
    cx.update(|window, cx| {
        window.focus(&root, cx);
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
    });
    let position = selection.registry.borrow().entries()[0]
        .geometry
        .bounds()
        .origin
        + point(px(4.0), px(8.0));
    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Left,
        click_count: 2,
        ..Default::default()
    });
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());
    assert_eq!(
        selection.selection.borrow().selected_text().as_deref(),
        Some("Transcript")
    );
    cx.simulate_keystrokes("ctrl-c");
    let baseline = cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap());
    assert_eq!(
        baseline, "unchanged",
        "the original surface must reproduce the failure"
    );
    view.update(cx, |view, cx| {
        view.focus_selection = true;
        cx.notify();
    });
    cx.run_until_parked();
    let mut proof = Vec::new();
    for source in ["outer-root", "selected-input", "empty-input"] {
        cx.update(|window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
            if source == "outer-root" {
                window.focus(&root, cx);
            } else {
                input.update(cx, |input, cx| {
                    input.set_content(
                        if source == "selected-input" {
                            "draft"
                        } else {
                            ""
                        },
                        cx,
                    );
                });
                window.focus(&input.read(cx).focus(), cx);
            }
        });
        if source == "selected-input" {
            cx.simulate_keystrokes("ctrl-a");
        }
        cx.run_until_parked();
        let position = selection.registry.borrow().entries()[0]
            .geometry
            .bounds()
            .origin
            + point(px(4.0), px(8.0));
        if source == "selected-input" {
            let (start, end) = {
                let registry = selection.registry.borrow();
                let TextGeometry::Text { layout, .. } = &registry.entries()[0].geometry else {
                    panic!("plain prose must use a text layout");
                };
                (
                    layout.position_for_index(0).unwrap() + point(px(0.5), px(8.0)),
                    layout.position_for_index("Transcript".len()).unwrap()
                        + point(px(0.0), px(8.0)),
                )
            };
            cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
            cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
            cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        } else {
            cx.simulate_event(MouseDownEvent {
                position,
                button: MouseButton::Left,
                click_count: 2,
                ..Default::default()
            });
            cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());
        }
        cx.run_until_parked();
        assert_eq!(
            selection.selection.borrow().selected_text().as_deref(),
            Some("Transcript")
        );
        cx.simulate_keystrokes("ctrl-c");
        let copied = cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap());
        assert_eq!(copied, "Transcript", "copy after selecting from {source}");
        cx.update(|window, _| assert!(transcript.is_focused(window)));
        proof.push(serde_json::json!({ "previousFocus": source, "copied": copied }));
    }
    cx.update(|window, cx| {
        input.update(cx, |input, cx| input.set_content("draft", cx));
        window.focus(&input.read(cx).focus(), cx);
    });
    cx.simulate_keystrokes("ctrl-a ctrl-c");
    let copied = cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap());
    assert_eq!(copied, "draft");
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/copy-e2e")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(
        output.join("proof.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "passed": true, "originalSurfaceClipboard": baseline,
            "transcript": proof, "inputCopy": copied,
        }))
        .unwrap(),
    )
    .unwrap();
    eprintln!("copy E2E proof: {}", output.display());
}

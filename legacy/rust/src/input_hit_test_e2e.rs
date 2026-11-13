//! Native GPUI hit-test E2E for the composer and transcript selection listener.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    Context, Entity, FocusHandle, Modifiers, MouseButton, MouseDownEvent, Render, TestAppContext,
    Window, canvas, div, point, prelude::*, px,
};

use crate::input::ComposerInput;
use crate::md::render::{
    Ctx, MarkdownView, Metrics, Palette, TranscriptSelection, frame_reset,
    install_selection_input_with_focus, markdown,
};
use crate::theme::Theme;

struct Harness {
    root_focus: FocusHandle,
    transcript_focus: FocusHandle,
    input: Entity<ComposerInput>,
    markdown: MarkdownView,
    selection: TranscriptSelection,
    input_bounds: Rc<Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    underlay_clicks: Rc<Cell<usize>>,
    popup_clicks: Rc<Cell<usize>>,
    dropped_paths: Rc<RefCell<Vec<PathBuf>>>,
    popup: bool,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&Theme::light());
        let ctx = Ctx::new(
            "input-hit-test-e2e",
            &palette,
            Metrics::body(),
            self.selection.clone(),
        );
        let selection = self.selection.clone();
        let transcript_focus = self.transcript_focus.clone();
        let input_bounds = self.input_bounds.clone();
        let underlay_clicks = self.underlay_clicks.clone();
        let popup_clicks = self.popup_clicks.clone();
        let dropped_paths = self.dropped_paths.clone();
        let input = self.input.clone();

        let popup = self.popup.then(|| {
            div()
                .occlude()
                .absolute()
                .left(px(0.0))
                .top(px(44.0))
                .w(px(400.0))
                .h(px(48.0))
                .on_mouse_down(MouseButton::Left, move |_, _, _| {
                    popup_clicks.set(popup_clicks.get() + 1);
                })
                .child("Popup")
        });

        let mut root = div()
            .track_focus(&self.root_focus)
            .on_drop(move |paths: &gpui::ExternalPaths, _, _| {
                dropped_paths.borrow_mut().extend(paths.0.iter().cloned());
            })
            .w(px(400.0))
            .flex()
            .flex_col()
            .child(
                div()
                    .track_focus(&self.transcript_focus)
                    .h(px(44.0))
                    .w(px(400.0))
                    .overflow_hidden()
                    .child(frame_reset(self.selection.clone()))
                    .children(markdown(&self.markdown, &ctx))
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |_, _, window, _| {
                                install_selection_input_with_focus(
                                    window,
                                    &selection,
                                    &transcript_focus,
                                )
                            },
                        )
                        .absolute()
                        .size(px(0.0)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(44.0))
                    .w(px(400.0))
                    .h(px(48.0))
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        underlay_clicks.set(underlay_clicks.get() + 1);
                    })
                    .child("Underlay control"),
            )
            .child(
                div()
                    .id("composer-hit-test-target")
                    .relative()
                    .w(px(400.0))
                    .h(px(48.0))
                    .child(input)
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, _, _| {
                                input_bounds.set(Some(bounds));
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(400.0))
                        .h(px(48.0)),
                    ),
            );
        if let Some(popup) = popup {
            root = root.child(popup);
        }
        root
    }
}

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    initial_text: &'static str,
    search: bool,
    initially_focused: bool,
    multiline: bool,
    popup: bool,
    context_cut: bool,
    file_drop: bool,
}

#[gpui::test]
fn composer_click_is_not_stolen_by_transcript_selection_e2e(cx: &mut TestAppContext) {
    let failures = [
        "a clipped Markdown paragraph geometrically extends into the composer and steals its first click",
        "the global selection listener selects Markdown beneath a composer click",
        "the first click focuses the composer but the following character is lost",
        "the failure only occurs with existing text, multiline input, or search mode",
        "an occluding popup receives a click but the transcript listener also takes focus",
        "the input target is not interactive across its padding area",
        "the input blocks ancestor file drops or remains transparent after a drop",
    ];
    eprintln!("input hit-test failure modes: {failures:?}");

    cx.update(|cx| {
        crate::input::init(cx);
        crate::ui::menu::init(cx);
    });
    let scenarios = [
        Scenario {
            name: "empty-unfocused-multiline",
            initial_text: "",
            search: false,
            initially_focused: false,
            multiline: true,
            popup: false,
            context_cut: false,
            file_drop: false,
        },
        Scenario {
            name: "nonempty-focused-multiline",
            initial_text: "draft text\nsecond line",
            search: false,
            initially_focused: true,
            multiline: true,
            popup: false,
            context_cut: false,
            file_drop: false,
        },
        Scenario {
            name: "empty-unfocused-search",
            initial_text: "",
            search: true,
            initially_focused: false,
            multiline: false,
            popup: false,
            context_cut: false,
            file_drop: false,
        },
        Scenario {
            name: "popup-occludes-composer",
            initial_text: "",
            search: false,
            initially_focused: false,
            multiline: false,
            popup: true,
            context_cut: false,
            file_drop: false,
        },
        Scenario {
            name: "input-context-menu-cut",
            initial_text: "draft",
            search: false,
            initially_focused: true,
            multiline: false,
            popup: false,
            context_cut: true,
            file_drop: false,
        },
        Scenario {
            name: "ancestor-file-drop",
            initial_text: "",
            search: false,
            initially_focused: false,
            multiline: false,
            popup: false,
            context_cut: false,
            file_drop: true,
        },
    ];

    let mut results = Vec::new();
    for scenario in scenarios {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut markdown = MarkdownView::new();
            let prose = if scenario.multiline {
                "clipped markdown words continue below the visible transcript frame and overlap the composer input region. ".repeat(8)
            } else {
                "markdown beneath the composer continues below the visible frame and overlaps the input area. ".repeat(8)
            };
            markdown.set_text(&prose, false);
            let input = cx.new(|cx| {
                let input = ComposerInput::new(window, cx);
                if scenario.search {
                    input.search_field()
                } else {
                    input
                }
            });
            input.update(cx, |input, cx| {
                input.set_content(scenario.initial_text, cx);
            });
            Harness {
                root_focus: cx.focus_handle(),
                transcript_focus: cx.focus_handle(),
                input,
                markdown,
                selection: TranscriptSelection::default(),
                input_bounds: Rc::new(Cell::new(None)),
                underlay_clicks: Rc::new(Cell::new(0)),
                popup_clicks: Rc::new(Cell::new(0)),
                dropped_paths: Rc::default(),
                popup: scenario.popup,
            }
        });
        cx.run_until_parked();

        let (
            input,
            input_bounds,
            selection,
            underlay_clicks,
            popup_clicks,
            transcript_focus,
            root_focus,
        ) = cx.read_entity(&view, |view, _| {
            (
                view.input.clone(),
                view.input_bounds.clone(),
                view.selection.clone(),
                view.underlay_clicks.clone(),
                view.popup_clicks.clone(),
                view.transcript_focus.clone(),
                view.root_focus.clone(),
            )
        });
        if scenario.initially_focused {
            cx.update(|window, cx| window.focus(&input.read(cx).focus(), cx));
        } else {
            cx.update(|window, cx| window.focus(&root_focus, cx));
        }
        let bounds = input_bounds
            .get()
            .expect("the composer hit target must have laid out");
        let target = bounds.origin + point(px(12.0), px(12.0));
        if scenario.file_drop {
            cx.simulate_event(gpui::FileDropEvent::Entered {
                position: target,
                paths: gpui::ExternalPaths(
                    [PathBuf::from("input-hit-test-attachment.txt")]
                        .into_iter()
                        .collect(),
                ),
            });
            cx.simulate_event(gpui::FileDropEvent::Pending { position: target });
            cx.simulate_event(gpui::FileDropEvent::Submit { position: target });
            cx.simulate_event(gpui::FileDropEvent::Ended);
        }
        cx.simulate_mouse_move(target, None, Modifiers::none());
        if scenario.context_cut {
            cx.simulate_keystrokes("ctrl-a");
        }
        let button = if scenario.context_cut {
            MouseButton::Right
        } else {
            MouseButton::Left
        };
        cx.simulate_event(MouseDownEvent {
            position: target,
            button,
            click_count: 1,
            ..Default::default()
        });
        cx.simulate_mouse_up(target, button, Modifiers::none());
        cx.run_until_parked();

        if scenario.context_cut {
            cx.simulate_keystrokes("down enter");
        } else if !scenario.popup {
            cx.simulate_keystrokes("ctrl-a x");
        }
        cx.run_until_parked();

        let (content, input_focused) = cx.update(|window, cx| {
            let input = input.read(cx);
            (input.content().to_owned(), input.focus().is_focused(window))
        });
        let result = serde_json::json!({
            "scenario": scenario.name,
            "clickPosition": { "x": f32::from(target.x), "y": f32::from(target.y) },
            "expectedContent": if scenario.popup || scenario.context_cut { "" } else { "x" },
            "content": content,
            "inputFocused": input_focused,
            "transcriptFocused": cx.update(|window, _| transcript_focus.is_focused(window)),
            "selectedMarkdown": selection.selection.borrow().selected_text(),
            "underlayClicks": underlay_clicks.get(),
            "popupClicks": popup_clicks.get(),
            "droppedPaths": cx.read_entity(&view, |view, _| view.dropped_paths.borrow().clone()),
            "expectedDroppedPaths": if scenario.file_drop {
                vec!["input-hit-test-attachment.txt"]
            } else {
                Vec::<&str>::new()
            },
        });
        results.push(result);
    }

    let failures = results
        .iter()
        .filter(|result| {
            if result["droppedPaths"] != result["expectedDroppedPaths"] {
                return true;
            }
            if result["scenario"] == "popup-occludes-composer" {
                result["popupClicks"] != 1
                    || result["transcriptFocused"] == true
                    || result["inputFocused"] == true
                    || result["content"] != ""
                    || result["underlayClicks"] != 0
            } else {
                result["content"] != result["expectedContent"]
                    || result["inputFocused"] != true
                    || result["selectedMarkdown"] != serde_json::Value::Null
                    || result["underlayClicks"] != 0
            }
        })
        .count();
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/input-hit-test-e2e")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&output).expect("create E2E proof directory");
    std::fs::write(
        output.join("proof.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "passed": failures == 0,
            "failureModes": [
                "clipped markdown overlaps the composer hit area",
                "global transcript selection steals focus or selects underlying prose",
                "first click does not permit the next character to enter the input",
                "empty, nonempty, multiline, and search inputs behave differently",
                "an occluding popup loses its click or focus",
            ],
            "scenarios": results,
        }))
        .expect("serialize E2E proof"),
    )
    .expect("write E2E proof");
    eprintln!("input hit-test E2E proof: {}", output.display());
    assert_eq!(
        failures, 0,
        "composer click behavior differs from the user's target"
    );
}

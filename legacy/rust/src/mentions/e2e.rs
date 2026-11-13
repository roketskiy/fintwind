//! Headless component E2E: real input actions, submission and selectable
//! Markdown rendering. No daemon, child processes or user data are used.

use super::*;
use crate::input::{ComposerEvent, ComposerInput};
use crate::md::render::{self, Ctx, MarkdownView, Metrics, Palette, TranscriptSelection};
use crate::theme::Theme;
use gpui::{Context, Entity, Render, Subscription, TestAppContext, div, prelude::*};
use serde_json::json;

struct Harness {
    input: Entity<ComposerInput>,
    catalog: Rc<Catalog>,
    sent: Option<String>,
    markdown: MarkdownView,
    selection: TranscriptSelection,
    _subscription: Subscription,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&Theme::light());
        let ctx = Ctx::new(
            "sent-reference-e2e",
            &palette,
            Metrics::user_message(),
            self.selection.clone(),
        )
        .with_references(self.catalog.clone());
        div()
            .w(px(180.0))
            .flex()
            .flex_col()
            .child(self.input.clone())
            .child(render::frame_reset(self.selection.clone()))
            .children(render::markdown(&self.markdown, &ctx))
    }
}

#[gpui::test]
fn reference_draft_send_edit_copy_e2e(cx: &mut TestAppContext) {
    // Failure modes: offsets shift after Unicode or a slash icon; decorations
    // alter copied/submitted text; narrow wrapping breaks input navigation;
    // changing the catalog leaves stale reference styling; directories get an
    // icon; Markdown code literals are incorrectly treated as references.
    cx.update(crate::input::init);
    let commands = Rc::new(vec![crate::composer_complete::SlashCommand {
        name: "poteto-mode".into(),
        description: "Brief replies".into(),
        scope: crate::composer_complete::CommandScope::Skill,
        argument_hint: None,
        template: None,
    }]);
    let files = Rc::new(vec![
        crate::composer_complete::FileEntry {
            path: "AGENTS.md".into(),
            is_dir: false,
        },
        crate::composer_complete::FileEntry {
            path: "src/".into(),
            is_dir: true,
        },
        crate::composer_complete::FileEntry {
            path: "src/main.rs".into(),
            is_dir: false,
        },
    ]);
    let catalog = Rc::new(Catalog::new(commands, files, |path| {
        if path.ends_with(".rs") {
            "icons/file-types/rust.svg"
        } else {
            "icons/file-types/markdown.svg"
        }
    }));
    let initial = "/poteto-mode \u{8bf7}\u{1f642}@AGENTS.md @src/ @src/main.rs";
    let (harness, cx) = cx.add_window_view(move |window, cx| {
        let input = cx.new(|cx| {
            let mut input = ComposerInput::new(window, cx);
            input.set_reference_catalog(catalog.clone(), cx);
            input.set_content(initial, cx);
            input
        });
        let _subscription = cx.subscribe(&input, |this: &mut Harness, _, event, cx| {
            if let ComposerEvent::Submit(text) = event {
                this.sent = Some(text.clone());
                this.markdown.set_text(text, false);
                cx.notify();
            }
        });
        Harness {
            input,
            catalog,
            sent: None,
            markdown: MarkdownView::new(),
            selection: TranscriptSelection::default(),
            _subscription,
        }
    });
    let input = cx.read_entity(&harness, |harness, _| harness.input.clone());
    cx.update(|window, cx| window.focus(&input.read(cx).focus(), cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-a ctrl-c");
    let copied = cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap());
    assert_eq!(copied, initial);
    cx.simulate_keystrokes("left right backspace ctrl-z");
    cx.read_entity(&input, |input, _| assert_eq!(input.content(), initial));
    cx.simulate_keystrokes("ctrl-a right left right enter");
    cx.run_until_parked();
    let (sent, registered) = cx.read_entity(&harness, |harness, _| {
        (
            harness.sent.clone().unwrap(),
            harness
                .selection
                .registry
                .borrow()
                .entries()
                .iter()
                .map(|entry| entry.text.to_string())
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(sent, initial);
    assert_eq!(registered.join(""), initial);
    cx.update(|_, cx| {
        input.update(cx, |input, cx| {
            input.set_content("`@poteto-mode` and @src/main.rs", cx);
            input.set_reference_catalog(Rc::new(Catalog::default()), cx);
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-a ctrl-c");
    let after_reload = cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap());
    assert_eq!(after_reload, "`@poteto-mode` and @src/main.rs");
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/mention-e2e")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(
        output.join("proof.json"),
        serde_json::to_vec_pretty(&json!({
            "passed": true, "surface": "headless GPUI input -> submission -> selectable Markdown",
            "copiedDraft": copied, "submitted": sent, "selectableMessage": registered,
            "afterCatalogChange": after_reload,
        }))
        .unwrap(),
    )
    .unwrap();
    eprintln!("mention E2E proof: {}", output.display());
}

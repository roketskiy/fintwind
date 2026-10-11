//! The rail's help entry and the cards it opens.
//!
//! The entry is a rail-shaped trigger for a [`dropdown_menu`], not a mode: the
//! rail's modes occupy the whole workspace, while these entries open surfaces
//! on top of whatever mode is showing. The first card is the changelog, which
//! renders the CHANGELOG.md embedded from the repository root — the same file
//! the Release workflow publishes from, so the card can never drift from the
//! published release notes.

use crate::md::virtualized::ReasoningView;
use crate::ui::ActivationExt;
use gpui::{KeyBinding, actions};

use super::*;

actions!(fintwind_help, [DismissChangelog]);

/// Key context the open changelog card declares, so `escape` closes the card
/// instead of also reaching the surfaces beneath it.
const CARD_CONTEXT: &str = "ChangelogCard";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", DismissChangelog, Some(CARD_CONTEXT)),
        KeyBinding::new("secondary-c", CopySelection, Some(CARD_CONTEXT)),
    ]);
}

/// CHANGELOG.md at the repository root, embedded at build time.
const CHANGELOG_SOURCE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/CHANGELOG.md"));

/// The embedded changelog reduced to its released sections. The file's header
/// prose and the `unreleased` scratch section never ship to users, and the
/// bracketed version headings render as plain headings. `None` means the file
/// carries no released section at all.
fn released_changelog_markdown() -> Option<SharedString> {
    let mut out = String::new();
    let mut in_release = false;
    for line in CHANGELOG_SOURCE.lines() {
        if let Some(rest) = line.strip_prefix("## [") {
            let version = rest.split_once(']').map(|(version, _)| version);
            in_release = version.is_some_and(|version| version != "unreleased");
            if let Some(version) = version.filter(|_| in_release) {
                out.push_str("## ");
                out.push_str(version);
                out.push('\n');
            }
            continue;
        }
        if in_release {
            // Tag link references beneath the sections only serve the Release
            // workflow's extraction; the card renders no link column for them.
            if line.starts_with("[") && line.contains("]:") {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    (!out.trim().is_empty()).then(|| out.into())
}

/// Window-modal changelog card. Its markdown is parsed once when the card
/// opens; frames only read the resulting view.
pub(super) struct ChangelogCard {
    focus: FocusHandle,
    close_focus: FocusHandle,
    notes: Option<Entity<ReasoningView>>,
    selection: TranscriptSelection,
}

impl Fintwind {
    pub(super) fn open_changelog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.changelog.is_some() {
            return;
        }
        let selection = TranscriptSelection::default();
        let notes = released_changelog_markdown().map(|markdown| {
            cx.new(|cx| {
                let mut view = ReasoningView::new(
                    "changelog-notes".into(),
                    selection.clone(),
                    Rc::new(|url, _, cx| cx.open_url(url)),
                    Some(px(0.0)),
                    cx,
                )
                .with_height(gpui::relative(1.0));
                view.set_source(&markdown, (1, 1), false, cx);
                view
            })
        });
        let focus = cx.focus_handle();
        self.changelog = Some(ChangelogCard {
            focus: focus.clone(),
            close_focus: cx.focus_handle(),
            notes,
            selection,
        });
        // Like Fintwind's other deferred surfaces, the modal joins the dispatch
        // tree only after it has drawn. Focus it two frames later so `escape`
        // reaches the card instead of the surface beneath it.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| window.focus(&focus, cx));
        });
        cx.notify();
    }

    pub(super) fn close_changelog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.changelog.take().is_none() {
            return;
        }
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(super) fn render_changelog_card(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let card = self.changelog.as_ref()?;
        let theme = Theme::current(cx);
        let width = (window.viewport_size().width - px(48.0)).min(px(560.0));
        let height = (window.viewport_size().height - px(96.0)).min(px(640.0));
        let selection = card.selection.clone();
        let selection_input = canvas(
            |_, _, _| (),
            move |_, _, window, _| md::render::install_selection_input(window, &selection),
        )
        .absolute()
        .size(px(0.0));

        let close = icon_button("dismiss-changelog", "icons/x.svg", theme)
            .track_focus(&card.close_focus)
            .tab_index(0)
            .size(px(30.0))
            .flex_none()
            .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
            .tooltip(Tooltip::text(tr!("help.dismiss")))
            .on_activation(cx, Self::close_changelog);

        let card = div()
            .id("changelog-card")
            .key_context(CARD_CONTEXT)
            .track_focus(&card.focus)
            .tab_group()
            .tab_stop(false)
            .w(width)
            .h(height)
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .rounded(px(18.0))
            .bg(theme.composer)
            .shadow_xl()
            .font_family(crate::theme::ui_font_family())
            .text_size(ui_px(12.0))
            .text_color(theme.text)
            .on_action(cx.listener(|this, _: &DismissChangelog, window, cx| {
                this.close_changelog(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| {
                if let Some(text) = this
                    .changelog
                    .as_ref()
                    .and_then(|card| card.selection.selection.borrow().selected_text())
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(md::render::frame_reset(card.selection.clone()))
            .child(
                div()
                    .flex_none()
                    .pt(px(16.0))
                    .px(px(20.0))
                    .pb(px(12.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .items_start()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_px(15.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tr!("help.changelog")),
                            )
                            .child(
                                div()
                                    .mt(px(3.0))
                                    .text_size(ui_px(12.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!(
                                        "update.current_version",
                                        version = crate::update::APP_VERSION
                                    )),
                            ),
                    )
                    .child(close),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .children(card.notes.clone())
                    .when(card.notes.is_none(), |element| {
                        element.child(
                            div()
                                .p(px(20.0))
                                .text_color(theme.text_secondary)
                                .child(tr!("help.changelog_empty")),
                        )
                    })
                    .child(selection_input),
            );

        let scrim = if theme.is_dark {
            gpui::hsla(0.0, 0.0, 0.0, 0.34)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        };
        let layer = div()
            .id("changelog-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(scrim)
            .p(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|fintwind, _, window, cx| fintwind.close_changelog(window, cx)),
            )
            .child(card);
        Some(gpui::deferred(layer).with_priority(4).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_sections_drop_prose_unreleased_and_link_refs() {
        let markdown = released_changelog_markdown().expect("changelog has released sections");
        let heading = |version: &str| format!("## {version}\n");

        // Released sections render with bracket-free headings, in file order.
        assert!(markdown.contains(&heading("0.2.6")));
        assert!(markdown.starts_with(&heading("0.2.7")));

        // The header prose and the unreleased scratch section never ship.
        assert!(!markdown.contains("source of truth"));
        assert!(!markdown.contains("unreleased"));

        // The first released section's bullets survive.
        assert!(markdown.contains("用量页「模型用量」改为矩形树图"));
    }
}

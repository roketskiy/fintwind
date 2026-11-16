//! The rail's help entry and the cards it opens.
//!
//! The entry is a rail-shaped trigger for a [`dropdown_menu`], not a mode: the
//! rail's modes occupy the whole workspace, while these entries open surfaces
//! on top of whatever mode is showing. The first card is the changelog, which
//! renders the CHANGELOG.md embedded from the repository root — the same file
//! the Release workflow publishes from, so the card can never drift from the
//! published release notes.

use crate::ui::ActivationExt;
use ely_gpui_component::{
    data_display::{TimelineItem, Tone},
    theme::{ActiveTheme as _, ControlSize},
};
use gpui::{KeyBinding, ListAlignment, ListState, actions, list};

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
    notes_focus: FocusHandle,
    releases: Rc<Vec<ChangelogRelease>>,
    list: ListState,
    scrollbar: Rc<ScrollbarState>,
    selection: TranscriptSelection,
}

struct ChangelogRelease {
    title: MarkdownView,
    body: MarkdownView,
}

fn changelog_releases(markdown: &str) -> Vec<ChangelogRelease> {
    markdown
        .split("\n## ")
        .map(|section| section.strip_prefix("## ").unwrap_or(section))
        .map(|section| {
            let (version, body) = section.split_once('\n').unwrap_or((section, ""));
            let mut title = MarkdownView::new();
            title.set_text(&format!("## {version}"), false);
            let mut notes = MarkdownView::new();
            notes.set_text(body, false);
            ChangelogRelease { title, body: notes }
        })
        .collect()
}

impl Fintwind {
    /// Open the "Performance Overhead" tool window, or raise the one that is
    /// already open. The window owns its own sampling loop, so the app only
    /// keeps the handle for re-activation.
    pub(super) fn open_perf_monitor(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.perf_window.as_ref() {
            let raised = handle
                .update(cx, |_, window, cx| {
                    window.activate_window();
                    cx.activate(true);
                })
                .is_ok();
            if raised {
                return;
            }
        }
        match crate::perf_monitor::open_window(cx) {
            Ok(handle) => self.perf_window = Some(handle),
            Err(error) => eprintln!("[perf-monitor] {error:#}"),
        }
    }

    pub(super) fn open_changelog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.changelog.is_some() {
            return;
        }
        let selection = TranscriptSelection::default();
        let releases = Rc::new(
            released_changelog_markdown()
                .map(|markdown| changelog_releases(&markdown))
                .unwrap_or_default(),
        );
        let list = ListState::new(releases.len(), ListAlignment::Top, px(160.0));
        let view = cx.entity().downgrade();
        list.set_scroll_handler(move |_, _, cx| {
            let _ = view.update(cx, |_, cx| cx.notify());
        });
        let focus = cx.focus_handle();
        self.changelog = Some(ChangelogCard {
            focus: focus.clone(),
            close_focus: cx.focus_handle(),
            notes_focus: cx.focus_handle(),
            releases,
            list,
            scrollbar: ScrollbarState::new(),
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
        let releases = card.releases.clone();
        let selection = card.selection.clone();
        let palette = MarkdownPalette::from_theme(&theme);
        let slot = cx.theme().control_height(ControlSize::Sm);
        let timeline = list(card.list.clone(), move |index, _, _| {
            let release = &releases[index];
            let title_ctx = MarkdownCtx::new(
                format!("changelog-{index}-title"),
                &palette,
                MarkdownMetrics::release_notes(),
                selection.clone(),
            )
            .with_streaming_animation(false);
            let body_ctx = MarkdownCtx::new(
                format!("changelog-{index}-body"),
                &palette,
                MarkdownMetrics::release_notes(),
                selection.clone(),
            )
            .with_streaming_animation(false);
            let title = div().children(md::render::markdown(&release.title, &title_ctx));
            let body = div().children(md::render::markdown(&release.body, &body_ctx));
            let item = TimelineItem::new(title)
                .tone(if index == 0 {
                    Tone::Accent
                } else {
                    Tone::Neutral
                })
                .child(body);
            div()
                .relative()
                .w_full()
                .when(index + 1 < releases.len(), |row| row.pb(px(24.0)))
                .child(
                    div()
                        .absolute()
                        .left(slot / 2.0)
                        .top_0()
                        .bottom_0()
                        .when(index == 0, |rail| rail.top(slot / 2.0))
                        .when(index + 1 == releases.len(), |rail| {
                            rail.bottom_auto().h(slot / 2.0)
                        })
                        .border_l_1()
                        .border_color(theme.border),
                )
                .child(item)
                .into_any_element()
        });

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
                    .pt(px(20.0))
                    .px(px(24.0))
                    .pb(px(16.0))
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
                                    .text_size(ui_px(16.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tr!("help.changelog")),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(ui_px(11.0))
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
                    .relative()
                    .overflow_hidden()
                    .when(!card.releases.is_empty(), |element| {
                        element
                            .track_focus(&card.notes_focus)
                            .tab_index(0)
                            .focus_visible(|style| style.border_1().border_color(theme.accent))
                            .child(
                                div()
                                    .size_full()
                                    .px(px(24.0))
                                    .child(timeline.size_full().py(px(20.0))),
                            )
                            .child(scrollbar::vertical(&card.list, &card.scrollbar))
                    })
                    .when(card.releases.is_empty(), |element| {
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
        assert!(markdown.starts_with(&heading(crate::update::APP_VERSION)));

        // The header prose and the unreleased scratch section never ship.
        assert!(!markdown.contains("source of truth"));
        assert!(!markdown.contains("unreleased"));

        // The first released section's bullets survive.
        assert!(markdown.contains("用量页「模型用量」改为矩形树图"));
    }
}

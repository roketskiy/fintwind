//! Non-modal startup update notice. Release fetching and Markdown preparation
//! run off-thread; the floating card only reads the resulting in-memory state.

use gpui::{KeyBinding, actions};

use crate::md::virtualized::ReasoningView;
use crate::ui::ActivationExt;
use crate::update::ReleaseInfo;

use super::*;

actions!(fintwind_update_card, [DismissUpdateCard]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", DismissUpdateCard, Some("UpdateCard")),
        KeyBinding::new("secondary-c", CopySelection, Some("UpdateCard")),
    ]);
}

pub(super) struct UpdateCard {
    pub(super) release: ReleaseInfo,
    visible: bool,
    notes: Option<Entity<ReasoningView>>,
    selection: TranscriptSelection,
    focus: FocusHandle,
    close_focus: FocusHandle,
    download_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
}

impl UpdateCard {
    pub(super) fn new(release: ReleaseInfo, cx: &mut Context<Fintwind>) -> Self {
        let selection = TranscriptSelection::default();
        let notes = (!release.notes.trim().is_empty()).then(|| {
            cx.new(|cx| {
                let mut view = ReasoningView::new(
                    "update-release-notes".into(),
                    selection.clone(),
                    Rc::new(|url, _, cx| cx.open_url(url)),
                    Some(px(0.0)),
                    cx,
                )
                .with_height(gpui::relative(1.0));
                view.set_source(&release.notes, (1, 1), false, cx);
                view
            })
        });
        Self {
            release,
            visible: true,
            notes,
            selection,
            focus: cx.focus_handle(),
            close_focus: cx.focus_handle(),
            download_focus: cx.focus_handle(),
            previous_focus: None,
        }
    }

    /// The release-notes text the user currently has selected, for the
    /// global copy action.
    pub(super) fn selected_text(&self) -> Option<String> {
        self.selection.selection.borrow().selected_text()
    }
}

impl Fintwind {
    pub(super) fn update_card_visible(&self) -> bool {
        self.latest_available
            .as_ref()
            .is_some_and(|card| card.visible)
    }

    /// The card floats above the transcript, sidebar, and settings surfaces
    /// with a selection registry of its own. Window-level selection listeners
    /// cannot see GPUI occlusion, so while the card is open every covered
    /// surface skips installing its listeners (checked at each install site)
    /// and any selection it still shows is dropped here — otherwise one drag
    /// would select the card's release notes and the underlying text at once,
    /// and the stale background highlight would outlive the card.
    pub(super) fn drop_covered_selections(&mut self) {
        for selection in [
            &self.transcript_selection,
            &self.toast_selection,
            &self.skills_selection,
            &self.right_panel_diff_selection,
        ] {
            selection.selection.borrow_mut().clear();
            selection.registry.borrow_mut().clear();
        }
        for registry in self.background_work.values_mut() {
            registry.clear_selection();
        }
    }

    pub(super) fn open_update_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.latest_available.is_none() {
            return;
        }
        self.drop_covered_selections();
        let Some(card) = self.latest_available.as_mut() else {
            return;
        };
        card.visible = true;
        if !card.focus.contains_focused(window, cx) {
            card.previous_focus = window.focused(cx);
        }
        let focus = card.close_focus.clone();
        let weak = cx.entity().downgrade();
        // Only an explicit open moves focus. A startup notification must not
        // interrupt typing while the asynchronous version check completes.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if this
                        .latest_available
                        .as_ref()
                        .is_some_and(|card| card.visible)
                    {
                        window.focus(&focus, cx);
                    }
                });
            });
        });
        cx.notify();
    }

    fn dismiss_update_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = self.latest_available.as_mut() else {
            return;
        };
        card.visible = false;
        card.selection.selection.borrow_mut().clear();
        card.selection.registry.borrow_mut().clear();
        let restore_focus = card.focus.contains_focused(window, cx);
        let previous_focus = card.previous_focus.take();
        if restore_focus {
            let focus = previous_focus.unwrap_or_else(|| {
                if self.settings_page.is_some() {
                    self.settings_focus.clone()
                } else {
                    self.composer_focus(cx)
                }
            });
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    pub(super) fn render_update_card(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let card = self.latest_available.as_ref().filter(|card| card.visible)?;
        let theme = Theme::current(cx);
        let width = (window.viewport_size().width - px(24.0)).min(px(400.0));
        let height = (window.viewport_size().height - px(112.0)).min(px(440.0));
        let selection = card.selection.clone();
        let selection_input = canvas(
            |_, _, _| (),
            move |_, _, window, _| md::render::install_selection_input(window, &selection),
        )
        .absolute()
        .size(px(0.0));

        let close = icon_button("dismiss-update-card", "icons/x.svg", theme)
            .track_focus(&card.close_focus)
            .tab_index(0)
            .size(px(30.0))
            .flex_none()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .tooltip(Tooltip::text(tr!("update.dismiss")))
            .on_activation(cx, Self::dismiss_update_card);

        let download = div()
            .id("download-update")
            .track_focus(&card.download_focus)
            .tab_index(0)
            .w_full()
            .min_w_0()
            .p(px(10.0))
            .rounded(px(7.0))
            .bg(theme.overlay)
            .cursor_pointer()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay_strong))
            .tooltip(Tooltip::text(card.release.url.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .text_color(theme.accent)
                    .font_weight(FontWeight::MEDIUM)
                    .child(icon("icons/download.svg", 14.0, theme.accent))
                    .child(tr!("update.download")),
            )
            .child(
                div()
                    .mt(px(4.0))
                    .text_size(ui_px(10.5))
                    .text_color(theme.text_secondary)
                    .truncate()
                    .child(card.release.url.clone()),
            )
            .on_activation(cx, |this, _, cx| {
                if let Some(card) = &this.latest_available {
                    cx.open_url(&card.release.url);
                }
            });

        Some(
            div()
                .id("update-card")
                .key_context("UpdateCard")
                .track_focus(&card.focus)
                .tab_group()
                .tab_stop(false)
                .absolute()
                .left(px(12.0))
                .bottom(px(48.0))
                .w(width)
                .h(height)
                .flex()
                .flex_col()
                .overflow_hidden()
                .occlude()
                .rounded(px(12.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.raised)
                .shadow_lg()
                .font_family(crate::theme::ui_font_family())
                .text_size(ui_px(12.0))
                .text_color(theme.text)
                .on_action(cx.listener(|this, _: &DismissUpdateCard, window, cx| {
                    this.dismiss_update_card(window, cx);
                    cx.stop_propagation();
                }))
                .on_action(cx.listener(|this, _: &CopySelection, _, cx| {
                    if let Some(text) = this
                        .latest_available
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
                        .p(px(14.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(icon("icons/download.svg", 16.0, theme.accent))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(ui_px(14.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(tr!("update.title")),
                                )
                                .child(close),
                        )
                        .child(
                            div()
                                .mt(px(6.0))
                                .text_color(theme.text_secondary)
                                .child(tr!(
                                    "update.current_version",
                                    version = crate::update::APP_VERSION
                                )),
                        )
                        .child(
                            div().mt(px(3.0)).font_weight(FontWeight::MEDIUM).child(tr!(
                                "update.latest_version",
                                version = card.release.version
                            )),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .px(px(14.0))
                        .pt(px(10.0))
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr!("update.release_notes")),
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
                                    .p(px(14.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("update.no_release_notes")),
                            )
                        }),
                )
                .child(div().flex_none().p(px(10.0)).child(download))
                .child(selection_input)
                .into_any_element(),
        )
    }
}

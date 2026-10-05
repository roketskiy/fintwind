//! Non-modal startup update notice. Release fetching and Markdown preparation
//! run off-thread; the floating card only reads the resulting in-memory state.
//!
//! The card also owns the in-app update: it asks `crate::update` to download the
//! installer for this architecture and verify it, and only then hands it to a
//! silent install. No step of that runs on the thread that draws the card.

use gpui::{KeyBinding, actions};

use crate::md::virtualized::ReasoningView;
use crate::ui::ActivationExt;
use crate::update::{ReleaseAsset, ReleaseInfo};

use super::*;

actions!(fintwind_update_card, [DismissUpdateCard]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", DismissUpdateCard, Some("UpdateCard")),
        KeyBinding::new("secondary-c", CopySelection, Some("UpdateCard")),
    ]);
}

/// What the one-click update is doing. A card the user walks away from is
/// abandoned rather than paused, which is why the phase drops back to idle on
/// dismiss and the in-flight transfer is stranded by its generation instead.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum UpdatePhase {
    /// Waiting for the user to ask for it.
    Idle,
    /// Measuring which source will relay the installer fastest.
    Selecting,
    /// The installer is being downloaded and checked against the release.
    Downloading,
    /// The installer is running; this process is on its way out.
    Installing,
}

pub(super) struct UpdateCard {
    pub(super) release: ReleaseInfo,
    /// Whether this copy was installed rather than unpacked from the portable
    /// archive. Only an installed copy can be updated in place, so a portable
    /// one is offered the release page instead of a button that would quietly
    /// install a second copy beside it.
    installed: bool,
    visible: bool,
    phase: UpdatePhase,
    /// Bumped whenever the card is dismissed, which strands a transfer still in
    /// flight: it may finish downloading, but it may not install, because the
    /// card that asked for it is gone.
    generation: u64,
    /// The generation of the download in flight, if any. Held apart from
    /// `generation` so a stranded transfer cannot be mistaken for the current
    /// one.
    running: Option<u64>,
    notes: Option<Entity<ReasoningView>>,
    selection: TranscriptSelection,
    focus: FocusHandle,
    close_focus: FocusHandle,
    download_focus: FocusHandle,
    update_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
}

impl UpdateCard {
    pub(super) fn new(release: ReleaseInfo, installed: bool, cx: &mut Context<Fintwind>) -> Self {
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
            installed,
            visible: true,
            phase: UpdatePhase::Idle,
            generation: 0,
            running: None,
            notes,
            selection,
            focus: cx.focus_handle(),
            close_focus: cx.focus_handle(),
            download_focus: cx.focus_handle(),
            update_focus: cx.focus_handle(),
            previous_focus: None,
        }
    }

    /// The release-notes text the user currently has selected, for the
    /// global copy action.
    pub(super) fn selected_text(&self) -> Option<String> {
        self.selection.selection.borrow().selected_text()
    }

    /// Whether a transfer the user has not walked away from owns the controls.
    fn busy(&self) -> bool {
        !matches!(self.phase, UpdatePhase::Idle)
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
        for state in self.btw_states.values() {
            state.clear_selection();
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
        // An update the user walks away from is abandoned, not paused: the
        // controls come back, and a download that is still running is stranded
        // by the generation it no longer matches.
        card.phase = UpdatePhase::Idle;
        card.generation = card.generation.wrapping_add(1);
        card.selection.selection.borrow_mut().clear();
        card.selection.registry.borrow_mut().clear();
        let restore_focus = card.focus.contains_focused(window, cx);
        let previous_focus = card.previous_focus.take();
        if restore_focus {
            let focus = previous_focus.unwrap_or_else(|| {
                if self.mode == WorkspaceMode::Settings {
                    self.settings_focus.clone()
                } else {
                    self.composer_focus(cx)
                }
            });
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Downloads the release installer and hands it to a silent install.
    ///
    /// Everything that touches the network or the disk runs on the background
    /// executor, and the result is only ever read as a phase the next frame can
    /// draw. The one thing that happens on this thread is starting the
    /// installer, which is a fork-exec and not a wait.
    pub(super) fn start_update(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = self.latest_available.as_mut() else {
            return;
        };
        // The button is unmounted or inert while anything is in flight, so this
        // is belt and braces: a stranded transfer is still a transfer, and two
        // of them would write the same file.
        if card.running.is_some() || card.busy() {
            return;
        }
        let Some(asset) = card.release.installer().cloned() else {
            return;
        };
        card.phase = UpdatePhase::Selecting;
        card.generation = card.generation.wrapping_add(1);
        let generation = card.generation;
        card.running = Some(generation);
        let version = card.release.version.clone();
        let destination = crate::update::installer_download_path(&version);
        let installer = destination.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            // Two background turns, so the card can say which step it is on:
            // the choice of source first, then the transfer it made. Splitting
            // them is also what keeps the sample a source is measured with from
            // being mistaken for the installer.
            let chosen = cx
                .background_executor()
                .spawn({
                    let asset = asset.clone();
                    let destination = destination.clone();
                    async move { crate::update::pick_source(&asset, &destination) }
                })
                .await;
            let Ok(still_current) = this.update(cx, |this, cx| {
                let Some(card) = this.latest_available.as_mut() else {
                    return false;
                };
                if card.generation != generation || card.running != Some(generation) {
                    return false;
                }
                card.phase = UpdatePhase::Downloading;
                cx.notify();
                true
            }) else {
                return;
            };
            if !still_current {
                return;
            }
            let downloaded = cx
                .background_executor()
                .spawn(
                    async move { crate::update::download_installer(&asset, &chosen, &destination) },
                )
                .await;
            let Ok(installing) = this.update(cx, |this, cx| {
                let Some(card) = this.latest_available.as_mut() else {
                    return false;
                };
                card.running = None;
                if card.generation != generation {
                    // Dismissed while this downloaded. The file is on disk and
                    // verified, but nothing asked for it anymore.
                    return false;
                }
                match downloaded {
                    Ok(()) => {
                        card.phase = UpdatePhase::Installing;
                        cx.notify();
                        // Setup cannot replace this executable while it runs, so
                        // the app has to be out of the way: `on_app_quit` has
                        // already flushed state and stopped the daemon by the
                        // time the process is gone. The installer's `[Run]`
                        // entry launches the newly installed executable.
                        if let Err(error) = crate::update::launch_installer(&installer) {
                            card.phase = UpdatePhase::Idle;
                            this.show_toast_with_tone(
                                tr!("update.install_failed", reason = format!("{error:#}")),
                                ToastTone::Alert,
                            );
                            cx.notify();
                            return false;
                        }
                        true
                    }
                    Err(error) => {
                        card.phase = UpdatePhase::Idle;
                        this.show_toast_with_tone(
                            tr!("update.download_failed", reason = format!("{error:#}")),
                            ToastTone::Alert,
                        );
                        cx.notify();
                        false
                    }
                }
            }) else {
                return;
            };
            if installing {
                cx.update(|cx| cx.quit());
            }
        })
        .detach();
    }

    /// The one-click update control, or `None` when this copy of fintwind
    /// cannot be updated in place. It is the same element throughout an update,
    /// so focus and its focus ring survive: an in-flight update takes away the
    /// pointer and the accent fill, never the control.
    fn render_update_action(
        &self,
        card: &UpdateCard,
        asset: &ReleaseAsset,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = card.busy();
        let (label, glyph) = match card.phase {
            UpdatePhase::Idle => (
                tr!("update.install"),
                icon("icons/download.svg", 14.0, theme.on_accent).into_any_element(),
            ),
            UpdatePhase::Selecting => (
                tr!("update.selecting_source"),
                motion::spin_slow(icon("icons/loader-circle.svg", 14.0, theme.text_secondary)),
            ),
            UpdatePhase::Downloading => (
                tr!("update.downloading", size = asset.size_label()),
                motion::spin_slow(icon("icons/loader-circle.svg", 14.0, theme.text_secondary)),
            ),
            UpdatePhase::Installing => (
                tr!("update.installing"),
                motion::spin_slow(icon("icons/loader-circle.svg", 14.0, theme.text_secondary)),
            ),
        };
        div()
            .id("install-update")
            .track_focus(&card.update_focus)
            .tab_index(0)
            .w_full()
            .min_w_0()
            .h(px(34.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(7.0))
            .rounded(px(7.0))
            .text_size(ui_px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
            // Attached in both states: an in-flight update must swallow the key
            // rather than let Enter and Space fall through to whatever owns the
            // window, and `start_update` ignores them anyway.
            .on_activation(cx, Self::start_update)
            .when(busy, |element| {
                element
                    .bg(theme.overlay)
                    .text_color(theme.text_secondary)
                    .cursor_default()
            })
            .when(!busy, |element| {
                element
                    .bg(theme.accent_fill)
                    .text_color(theme.on_accent)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent_text))
                    .tooltip(Tooltip::text(tr!("update.install_tooltip")))
            })
            .child(glyph)
            .child(label)
            .into_any_element()
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
            .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
            .tooltip(Tooltip::text(tr!("update.dismiss")))
            .on_activation(cx, Self::dismiss_update_card);

        // The release page stays reachable: it is the only way forward for a
        // portable copy, and a fallback when the download itself fails.
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
            .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
            .hover(|style| style.bg(theme.overlay_strong))
            .tooltip(Tooltip::text(card.release.url.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .text_color(theme.link)
                    .font_weight(FontWeight::MEDIUM)
                    .child(icon("icons/external-link.svg", 14.0, theme.link))
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

        let installer = card
            .installed
            .then(|| card.release.installer())
            .flatten()
            .cloned();
        let actions = div()
            .flex_none()
            .p(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children(
                installer
                    .as_ref()
                    .map(|asset| self.render_update_action(card, asset, theme, cx)),
            )
            .child(download);

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
                .child(actions)
                .child(selection_input)
                .into_any_element(),
        )
    }
}

//! The usage meter under the composer: a circular context-window gauge that
//! opens a panel with the session's context occupancy and its cumulative
//! token throughput and cache hit rate. Context numbers stream in from the
//! OpenCode transport. Frames read only snapshots stored on the entity.

use crate::theme::ui_px;

use gpui::{PathBuilder, WeakEntity, relative};

use super::*;
use crate::usage::{cache_hit_percent, format_percent, format_tokens};

const USAGE_METER_MENU_ID: &str = "usage-meter";

impl Fintwind {
    /// Whether the footer shows the gauge. Always true with a session
    /// selected — an empty ring is the honest "nothing measured yet" state,
    /// and hiding it would make the control feel intermittent.
    pub(super) fn usage_meter_available(&self) -> bool {
        self.selected_session().is_some()
    }

    /// Primary modifier + U: toggle the usage panel as if its footer trigger were clicked.
    pub(super) fn toggle_usage_panel_action(
        &mut self,
        _: &ToggleUsagePanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.is_some() || !self.usage_meter_available() {
            return;
        }
        let menus = self.menus.borrow();
        let Some(handle) = menus.get(USAGE_METER_MENU_ID).cloned() else {
            return;
        };
        // A keyboard toggle produces no mouse-down for another open menu's
        // dismiss-on-down-out to see, so close the rest here.
        let other_open: Vec<_> = menus
            .iter()
            .filter(|(id, other)| id.as_ref() != USAGE_METER_MENU_ID && other.is_open())
            .map(|(_, other)| other.clone())
            .collect();
        drop(menus);
        window.defer(cx, move |window, cx| {
            for menu in other_open {
                menu.close(window, cx);
            }
            crate::ui::menu::toggle_popover(&handle, MenuAlign::AboveRight, window, cx);
        });
    }

    /// The footer's circular context gauge plus its anchored panel. `None`
    /// while there is nothing to show for the selected session's provider.
    pub(super) fn render_usage_meter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.usage_meter_available() {
            return None;
        }
        let session = self.selected_session()?;
        let context = session.context_usage;
        let compaction = session.compaction.clone();
        let session_id = session.id;
        let theme = Theme::current(cx);

        let weak = cx.entity().downgrade();
        let panel_weak = cx.entity().downgrade();
        let handle = self.menu_handle_with(USAGE_METER_MENU_ID, cx, move |open, window, cx| {
            if open {
                let mut card_focus = None;
                let _ = weak.update(cx, |this, cx| {
                    card_focus = this
                        .menus
                        .borrow()
                        .get(USAGE_METER_MENU_ID)
                        .map(|handle| handle.focus_handle().clone());
                    cx.notify();
                });
                // The card is deferred, so its focus handle joins the
                // dispatch tree only after the deferred draw — the same
                // two-frame wait the menus use. Focused, the card's menu
                // context is what lets `escape` dismiss it.
                if let Some(focus) = card_focus {
                    window.on_next_frame(move |window, _| {
                        window.on_next_frame(move |window, cx| window.focus(&focus, cx));
                    });
                }
            } else {
                let mut composer_focus = None;
                let _ = weak.update(cx, |this, cx| {
                    composer_focus = Some(this.composer.read(cx).focus());
                    cx.notify();
                });
                if let Some(focus) = composer_focus {
                    window.focus(&focus, cx);
                }
            }
        });

        let percent = context.and_then(context_percent);
        let fill = match percent {
            Some(percent) if percent >= 95.0 => theme.danger,
            Some(percent) if percent >= 80.0 => theme.warning,
            _ => theme.gauge,
        };
        let tooltip = match percent {
            Some(percent) => SharedString::from(tr!(
                "usage.context_used",
                percent = format!("{percent:.1}"),
                shortcut = "Ctrl+U"
            )),
            None => SharedString::from(tr!("usage.shortcut", shortcut = "Ctrl+U")),
        };

        let trigger = div()
            .id("usage-meter")
            .h(px(24.0))
            .px(px(6.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .flex_none()
            .cursor_default()
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .when(handle.is_open(), |element| element.bg(theme.overlay_strong))
            .tooltip(Tooltip::text(tooltip))
            .child(context_gauge(percent, theme.border_strong, fill));

        Some(popover(
            trigger,
            &handle,
            MenuAlign::AboveRight,
            move |handle, _, cx| {
                usage_panel(
                    handle,
                    context,
                    compaction.clone(),
                    session_id,
                    panel_weak.clone(),
                    cx,
                )
            },
        ))
    }
}

fn context_percent(usage: ContextUsage) -> Option<f64> {
    usage
        .window
        .filter(|window| *window > 0)
        .map(|window| usage.tokens as f64 * 100.0 / window as f64)
}

/// The trigger glyph: a ring whose arc fills clockwise from 12 o'clock as the
/// context window does, over a faint full ring. An unknown fraction draws the
/// track alone. This is Zed's `CircularProgress` drawing sized for the footer
/// — `PathBuilder::stroke` arcs, which lyon tessellates correctly where a
/// hand-built annulus fill does not survive GPUI's fill rule.
fn context_gauge(percent: Option<f64>, track: Hsla, fill: Hsla) -> impl IntoElement {
    const SIZE: f32 = 13.0;
    const STROKE: f32 = 2.5;
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = px((SIZE - STROKE) / 2.0);

            // A full circle is two 180° arcs; lyon rejects a single
            // zero-length one.
            let full_circle = |builder: &mut PathBuilder| {
                builder.move_to(point(center.x + radius, center.y));
                builder.arc_to(
                    point(radius, radius),
                    px(0.0),
                    false,
                    true,
                    point(center.x - radius, center.y),
                );
                builder.arc_to(
                    point(radius, radius),
                    px(0.0),
                    false,
                    true,
                    point(center.x + radius, center.y),
                );
                builder.close();
            };

            let mut track_builder = PathBuilder::stroke(px(STROKE));
            full_circle(&mut track_builder);
            if let Ok(path) = track_builder.build() {
                window.paint_path(path, track);
            }

            let Some(percent) = percent else {
                return;
            };
            // Keep a visible sliver for a nearly-empty context.
            let fraction = ((percent / 100.0) as f32).clamp(0.0, 1.0).max(0.05);
            let mut arc_builder = PathBuilder::stroke(px(STROKE));
            if fraction >= 0.999 {
                full_circle(&mut arc_builder);
            } else {
                let start = -std::f32::consts::FRAC_PI_2;
                let angle = start + fraction * std::f32::consts::TAU;
                arc_builder.move_to(point(center.x, center.y - radius));
                arc_builder.arc_to(
                    point(radius, radius),
                    px(0.0),
                    fraction > 0.5,
                    true,
                    point(
                        center.x + radius * angle.cos(),
                        center.y + radius * angle.sin(),
                    ),
                );
            }
            if let Ok(path) = arc_builder.build() {
                window.paint_path(path, fill);
            }
        },
    )
    .w(px(SIZE))
    .h(px(SIZE))
    .flex_none()
}

fn usage_panel(
    handle: &ContextMenuHandle,
    context: Option<ContextUsage>,
    compaction: Option<CompactionState>,
    session_id: Uuid,
    weak: WeakEntity<Fintwind>,
    cx: &App,
) -> AnyElement {
    let theme = Theme::current(cx);
    let mut panel = div()
        // Focused on open so the surrounding menu context sees `escape`.
        .track_focus(handle.focus_handle())
        .w(px(320.0))
        .p(px(14.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.raised)
        .shadow_lg()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .text_size(ui_px(12.0));

    // The context row always renders; a session with nothing measured yet
    // reads "0" over an empty track, exactly like the CLI's own panel. The
    // totals row beneath it only renders once the provider has reported
    // something — an unknown number stays absent rather than reading zero.
    let usage = context.unwrap_or_default();
    let percent = context_percent(usage);
    let value = match (usage.window, percent) {
        (Some(window), Some(percent)) => format!(
            "{} / {} ({})",
            format_tokens(usage.tokens),
            format_tokens(window),
            format_percent(percent)
        ),
        // The transport reports occupancy but not the window size.
        _ => format_tokens(usage.tokens),
    };
    let mut context_section = div()
        .flex()
        .flex_col()
        .gap(px(7.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_color(theme.text)
                        .child(tr!("usage.context_window")),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(ui_px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(SharedString::from(value)),
                ),
        )
        .child(meter_bar(&theme, percent.unwrap_or(0.0)));
    if let Some(totals) = usage_totals_row(&theme, usage) {
        context_section = context_section.child(totals);
    }
    panel = panel.child(context_section);
    // Only a live or failed attempt renders; a completed compaction shows
    // through the context numbers above, which the provider re-reports
    // smaller on its next call.
    if let Some(state) = compaction.as_ref().filter(|state| {
        matches!(
            state.status,
            CompactionStatus::Running | CompactionStatus::Failed
        )
    }) {
        panel = panel
            .child(div().h(px(1.0)).flex_none().bg(theme.border))
            .child(compaction_row(&theme, state, session_id, weak));
    }

    panel.into_any_element()
}

/// The context section's second row: the session's cumulative token
/// throughput on the left, the latest call's cache hit rate on the right.
/// A metric the provider hasn't reported yet renders as a dash so the row
/// holds still while numbers stream in; a row with neither metric reported
/// is omitted entirely.
fn usage_totals_row(theme: &Theme, usage: ContextUsage) -> Option<Div> {
    let total = usage.total_tokens.map(format_tokens);
    let hit = cache_hit_percent(
        usage.cache_read.unwrap_or(0),
        usage.prompt_tokens.unwrap_or(0),
    )
    .map(format_percent);
    if total.is_none() && hit.is_none() {
        return None;
    }
    let cell = move |label: String, value: Option<String>, theme: &Theme| {
        div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .child(
                div()
                    .text_size(ui_px(11.0))
                    .text_color(theme.text_tertiary)
                    .child(label),
            )
            .child(
                div()
                    .text_size(ui_px(11.0))
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(value.unwrap_or_else(|| "—".into()))),
            )
    };
    Some(
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(cell(tr!("usage.total_tokens"), total, theme))
            .child(div().flex_1())
            .child(cell(tr!("usage.cache_hit_rate"), hit, theme)),
    )
}

/// The compaction action row: a pulsing progress label while the provider
/// summarizes, the failure notice with a retry button while the last attempt
/// failed. The retry re-asks the provider, which coalesces or runs it as
/// usual; `escape` and the row labels stay readable in both themes.
fn compaction_row(
    theme: &Theme,
    state: &CompactionState,
    session_id: Uuid,
    weak: WeakEntity<Fintwind>,
) -> Div {
    // The pulse closure outlives the borrow, so the theme rides along owned.
    let theme = *theme;
    match state.status {
        CompactionStatus::Running => {
            let label = tr!("usage.compacting").to_owned();
            div()
                .flex()
                .items_center()
                .min_h(px(22.0))
                .gap(px(8.0))
                .child(
                    motion::pulse(Duration::from_millis(1400), move |phase| {
                        div()
                            .text_size(ui_px(11.0))
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(label.clone()))
                            .opacity(pulsating_between(0.5, 1.0)(phase))
                            .into_any_element()
                    })
                    .every(2)
                    .into_any_element(),
                )
        }
        CompactionStatus::Failed => {
            let detail = state.error.as_deref().unwrap_or_default();
            div()
                .flex()
                .items_center()
                .min_h(px(22.0))
                .gap(px(8.0))
                .child(
                    div()
                        .id("usage-compaction-error")
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(ui_px(11.0))
                        .text_color(theme.warning)
                        .when(!detail.is_empty(), |element| {
                            element.tooltip(Tooltip::text(detail.to_owned()))
                        })
                        .child(tr!("usage.compaction_failed")),
                )
                .child(
                    div()
                        .id("usage-compaction-retry")
                        .flex_none()
                        .h(px(22.0))
                        .px(px(8.0))
                        .rounded(px(5.0))
                        .flex()
                        .items_center()
                        .text_size(ui_px(11.0))
                        .text_color(theme.text)
                        .cursor_default()
                        .hover(|element| element.bg(theme.overlay))
                        .active(|element| element.bg(theme.overlay_strong))
                        .on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.request_context_compaction(session_id, cx)
                            });
                        })
                        .child(tr!("usage.compaction_retry")),
                )
        }
        CompactionStatus::Completed | CompactionStatus::Cancelled => div(),
    }
}

/// A meter bar: full-width track, fill proportional to `percent`. A nonzero
/// value keeps a visible sliver even under one percent.
fn meter_bar(theme: &Theme, percent: f64) -> Div {
    let fraction = (percent / 100.0).clamp(0.0, 1.0) as f32;
    let fraction = if fraction > 0.0 {
        fraction.max(0.015)
    } else {
        0.0
    };
    let fill = if percent >= 95.0 {
        theme.danger
    } else if percent >= 80.0 {
        theme.warning
    } else {
        theme.gauge
    };
    div()
        .h(px(3.0))
        .w_full()
        .flex_none()
        .rounded_full()
        .bg(theme.overlay_strong)
        .child(div().h_full().w(relative(fraction)).rounded_full().bg(fill))
}

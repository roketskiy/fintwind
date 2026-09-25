//! Session context inspector and the compact footer gauge that opens it.
use crate::theme::ui_px;

use gpui::{PathBuilder, relative};

use super::*;
use crate::usage::{cache_hit_percent, format_percent, format_tokens};

impl Fintwind {
    pub(super) fn usage_meter_available(&self) -> bool {
        self.selected_session().is_some()
    }

    pub(super) fn toggle_usage_panel_action(
        &mut self,
        _: &ToggleUsagePanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.is_some() || !self.usage_meter_available() {
            return;
        }
        self.toggle_context_panel(window, cx);
    }

    fn toggle_context_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.right_panel_visible
            && self.active_right_panel_surface() == Some(&RightPanelSurface::Context)
        {
            self.close_context_panel(cx);
            window.focus(&self.composer.read(cx).focus(), cx);
        } else {
            self.open_right_panel_surface(RightPanelSurface::Context, cx);
            window.focus(&self.context_panel_focus, cx);
        }
    }

    /// The context window the user recorded for this `provider/id` on the
    /// Providers page. That value is what they expect the meter to use; the
    /// live catalog can also contain another provider's copy of the same id
    /// with a different limit.
    pub(super) fn configured_context_window(&self, model: Option<&str>) -> Option<u64> {
        let (provider_id, model_id) = model?.split_once('/')?;
        self.providers_store
            .iter()
            .find(|provider| {
                provider.slug.eq_ignore_ascii_case(provider_id)
                    || provider.id.eq_ignore_ascii_case(provider_id)
            })?
            .models
            .iter()
            .find(|entry| entry.id.eq_ignore_ascii_case(model_id))?
            .context_window
            .filter(|window| *window > 0)
    }

    /// Session usage with the recorded context window applied, so the gauge
    /// and the context page agree with the Providers page even when the
    /// driver cached another provider's limit.
    fn effective_context_usage(&self, session: &AgentSession) -> Option<ContextUsage> {
        let configured = self.configured_context_window(session.model.as_deref());
        let mut usage = session.context_usage;
        if let Some(window) = configured {
            usage.get_or_insert(ContextUsage::default()).window = Some(window);
        }
        usage
    }

    pub(super) fn render_usage_meter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.selected_session()?;
        let usage = self.effective_context_usage(session);
        let theme = Theme::current(cx);
        let percent = usage.and_then(context_percent);
        let fill = match percent {
            Some(percent) if percent >= 95.0 => theme.danger,
            Some(percent) if percent >= 80.0 => theme.warning,
            _ => theme.gauge,
        };
        let tooltip = match percent {
            Some(percent) => tr!(
                "usage.context_used",
                percent = format!("{percent:.1}"),
                shortcut = "Ctrl+U"
            ),
            None => tr!("usage.shortcut", shortcut = "Ctrl+U"),
        };
        let open = self.right_panel_visible
            && self.active_right_panel_surface() == Some(&RightPanelSurface::Context);

        Some(
            div()
                .id("usage-meter")
                .track_focus(&self.context_meter_focus)
                .tab_index(0)
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .h(px(24.0))
                .px(px(6.0))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .flex_none()
                .cursor_default()
                .hover(|element| element.bg(theme.overlay))
                .active(|element| element.bg(theme.overlay_strong))
                .when(open, |element| element.bg(theme.overlay_strong))
                .tooltip(Tooltip::text(tooltip))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_context_panel(window, cx)))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.toggle_context_panel(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(context_gauge(percent, theme.border_strong, fill))
                .into_any_element(),
        )
    }

    pub(super) fn render_context_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(session) = self.selected_session() else {
            return div().into_any_element();
        };
        let session_id = session.id;
        let usage = self.effective_context_usage(session);
        let compaction = session.compaction.clone();
        let model_name = session.model.clone();
        let selected_window = session.context_window.clone();
        let model = self.model_metadata_for_session(session).cloned();
        let can_compact = self.runtimes.contains_key(&session_id);
        let percent = usage.and_then(context_percent);
        let status = match percent {
            Some(value) if value >= 95.0 => Some(tr!("usage.full")),
            Some(value) if value >= 80.0 => Some(tr!("usage.near_full")),
            Some(_) => None,
            None => Some(tr!("usage.unknown")),
        };
        let status_color = match percent {
            Some(value) if value >= 95.0 => theme.danger,
            Some(value) if value >= 80.0 => theme.warning,
            _ => theme.text_secondary,
        };
        let (headline, capacity, hint) = match usage {
            Some(usage)
                if (usage.measured || usage.tokens > 0)
                    && usage.window.is_some_and(|window| window > 0) =>
            {
                let window = usage.window.unwrap_or(0);
                (
                    format_percent(percent.unwrap_or(0.0)),
                    format!(
                        "{} / {}",
                        format_tokens(usage.tokens),
                        format_tokens(window)
                    ),
                    tr!(
                        "usage.remaining",
                        count = format_tokens(window.saturating_sub(usage.tokens))
                    ),
                )
            }
            Some(usage) if usage.measured || usage.tokens > 0 => (
                format_tokens(usage.tokens),
                tr!("usage.window_unknown"),
                tr!("usage.window_unknown_detail"),
            ),
            _ => (
                "—".to_owned(),
                tr!("usage.not_measured"),
                tr!("usage.not_measured_detail"),
            ),
        };
        let notice = match percent {
            Some(value) if value >= 95.0 => tr!("usage.full_detail"),
            Some(value) if value >= 80.0 => tr!("usage.near_full_detail"),
            _ => hint,
        };
        let running = compaction
            .as_ref()
            .is_some_and(|state| state.status == CompactionStatus::Running);
        let failed = compaction
            .as_ref()
            .is_some_and(|state| state.status == CompactionStatus::Failed);
        let action_label = if running {
            tr!("usage.compacting")
        } else if failed {
            tr!("usage.compaction_retry")
        } else {
            tr!("usage.compact_action")
        };
        let action_enabled = can_compact && !running;
        let compact_focus = self.context_compact_focus.clone();

        let mut body = div()
            .id("context-panel-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .text_size(ui_px(12.0))
            .text_color(theme.text);

        let hero = div()
            .flex()
            .flex_col()
            .gap(px(9.0))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(ui_px(28.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(headline),
                    )
                    .when_some(status, |element, status| {
                        element.child(div().text_color(status_color).child(status))
                    }),
            )
            .child(div().text_color(theme.text_secondary).child(capacity))
            .when_some(percent, |element, percent| {
                element.child(meter_bar(&theme, percent))
            })
            .child(div().text_color(theme.text_secondary).child(notice))
            .when(failed, |element| {
                element.child(
                    div().text_color(theme.warning).child(
                        compaction
                            .as_ref()
                            .and_then(|state| state.error.clone())
                            .unwrap_or_else(|| tr!("usage.compaction_failed")),
                    ),
                )
            })
            .child(
                div().flex().justify_end().child(
                    div()
                        .id("context-compact-action")
                        .track_focus(&compact_focus)
                        .tab_index(0)
                        .min_h(px(28.0))
                        .px(px(10.0))
                        .rounded(px(6.0))
                        .flex()
                        .items_center()
                        .cursor_default()
                        .bg(theme.overlay)
                        .border_1()
                        .border_color(
                            if action_enabled && percent.is_some_and(|value| value >= 80.0) {
                                theme.accent
                            } else {
                                theme.border
                            },
                        )
                        .text_color(if action_enabled {
                            theme.text
                        } else {
                            theme.text_tertiary
                        })
                        .focus_visible(|style| style.border_1().border_color(theme.accent))
                        .when(action_enabled, |element| {
                            element
                                .hover(|style| style.bg(theme.overlay_strong))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.request_context_compaction(session_id, cx)
                                }))
                                .on_key_down(cx.listener(
                                    move |this, event: &KeyDownEvent, _, cx| {
                                        if matches!(event.keystroke.key.as_str(), "enter" | "space")
                                        {
                                            this.request_context_compaction(session_id, cx);
                                            cx.stop_propagation();
                                        }
                                    },
                                ))
                        })
                        .child(action_label),
                ),
            )
            .when(!can_compact && !running, |element| {
                element.child(
                    div()
                        .text_color(theme.text_tertiary)
                        .child(tr!("usage.compact_unavailable")),
                )
            });
        body = body.child(hero);

        if let Some(latest) = usage.and_then(|usage| usage.latest) {
            // Providers may report `tokens.total` independently of the split.
            // Percentages in this section describe only the reported split.
            let total = latest
                .input
                .saturating_add(latest.cache_read)
                .saturating_add(latest.cache_write)
                .saturating_add(latest.output)
                .saturating_add(latest.reasoning)
                .max(1);
            let rows = [
                (tr!("usage.cache_read"), latest.cache_read),
                (tr!("usage.cache_write"), latest.cache_write),
                (tr!("usage.input"), latest.input),
                (tr!("usage.output"), latest.output),
                (tr!("usage.reasoning"), latest.reasoning),
            ];
            let segments = rows
                .iter()
                .enumerate()
                .filter(|(_, (_, count))| *count > 0)
                .map(|(index, (_, count))| {
                    (index, (*count as f64 / total as f64).clamp(0.0, 1.0) as f32)
                });
            let colors = [
                theme.accent,
                theme.gauge,
                theme.text_secondary,
                theme.border_strong,
                theme.text_tertiary,
            ];
            let mut bar = div()
                .h(px(6.0))
                .w_full()
                .rounded_full()
                .overflow_hidden()
                .flex()
                .bg(theme.overlay);
            for (index, fraction) in segments {
                bar = bar.child(div().h_full().w(relative(fraction)).bg(colors[index]));
            }
            let mut section = inspector_section(&theme, tr!("usage.latest_call")).child(bar);
            for (index, (label, count)) in rows.into_iter().enumerate() {
                if count > 0 {
                    section = section.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .size(px(6.0))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(colors[index]),
                            )
                            .child(div().text_color(theme.text_secondary).child(label))
                            .child(div().flex_1())
                            .child(div().text_color(theme.text).child(format!(
                                "{}   {}",
                                format_tokens(count),
                                format_percent(count as f64 * 100.0 / total as f64)
                            ))),
                    );
                }
            }
            body = body.child(section);
        }

        if let Some(usage) =
            usage.filter(|usage| usage.total_tokens.is_some() || usage.prompt_tokens.is_some())
        {
            let mut section = inspector_section(&theme, tr!("usage.session_total"));
            if let Some(total) = usage.total_tokens {
                section = section.child(inspector_row(
                    &theme,
                    tr!("usage.total_tokens"),
                    format_tokens(total),
                ));
            }
            if let Some(rate) = cache_hit_percent(
                usage.cache_read.unwrap_or(0),
                usage.prompt_tokens.unwrap_or(0),
            ) {
                section = section.child(inspector_row(
                    &theme,
                    tr!("usage.cache_hit_rate"),
                    format_percent(rate),
                ));
            }
            body = body.child(section);
        }

        let mut model_section = inspector_section(&theme, tr!("usage.window_section"));
        if let Some(name) = model_name {
            model_section = model_section.child(inspector_row(&theme, tr!("usage.model"), name));
        }
        if let Some(model) = model {
            let selected = selected_window
                .as_deref()
                .filter(|selected| {
                    model
                        .context_windows
                        .iter()
                        .any(|option| option.id == *selected)
                })
                .or(model.default_context_window.as_deref())
                .or_else(|| {
                    model
                        .context_windows
                        .first()
                        .map(|option| option.id.as_str())
                });
            if model.context_windows.len() > 1 {
                let options = model.context_windows.clone();
                let selected_id = selected.map(str::to_owned);
                let selected_label = model
                    .context_windows
                    .iter()
                    .find(|option| Some(option.id.as_str()) == selected)
                    .map(|option| option.label.clone())
                    .unwrap_or_default();
                let weak = cx.entity().downgrade();
                let handle = self.menu_handle("context-window-options", cx);
                model_section = model_section.child(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .text_color(theme.text_secondary)
                                .child(tr!("usage.window_size")),
                        )
                        .child(div().flex_1())
                        .child(dropdown_menu(
                            MenuChip::new("context-window-options")
                                .label(selected_label)
                                .selected(handle.is_open()),
                            "context-window-options-menu",
                            &handle,
                            MenuAlign::BelowRight,
                            move |_| {
                                options
                                    .clone()
                                    .into_iter()
                                    .map(|option| {
                                        let weak = weak.clone();
                                        let selected =
                                            selected_id.as_deref() == Some(option.id.as_str());
                                        MenuItem::new(option.label, move |_, cx| {
                                            let _ = weak.update(cx, |this, cx| {
                                                this.set_context_window(option.id.clone(), cx)
                                            });
                                        })
                                        .selected(selected)
                                    })
                                    .collect()
                            },
                        )),
                );
            } else if let Some(selected) = selected {
                let label = model
                    .context_windows
                    .first()
                    .map(|option| option.label.clone())
                    .unwrap_or_else(|| selected.to_owned());
                model_section =
                    model_section.child(inspector_row(&theme, tr!("usage.window_size"), label));
            } else if let Some(window) = usage.and_then(|usage| usage.window) {
                model_section = model_section.child(inspector_row(
                    &theme,
                    tr!("usage.window_size"),
                    format_tokens(window),
                ));
            }
        } else if let Some(window) = usage.and_then(|usage| usage.window) {
            model_section = model_section.child(inspector_row(
                &theme,
                tr!("usage.window_size"),
                format_tokens(window),
            ));
        }
        body = body.child(model_section);

        if let Some(state) = compaction.filter(|state| state.status == CompactionStatus::Completed)
        {
            let reason = match state.reason.as_deref() {
                Some("auto") => tr!("usage.automatic"),
                Some("manual") => tr!("usage.manual"),
                _ => tr!("usage.completed"),
            };
            let mut section = inspector_section(&theme, tr!("usage.compaction_section"))
                .child(inspector_row(&theme, tr!("usage.last_compaction"), reason));
            if let Some(message_id) = self.context_summary_ids.get(&session_id).copied().flatten() {
                let summary_focus = self.context_summary_focus.clone();
                section = section.child(
                    div()
                        .id("context-view-summary")
                        .track_focus(&summary_focus)
                        .tab_index(0)
                        .min_h(px(26.0))
                        .cursor_default()
                        .text_color(theme.accent)
                        .focus_visible(|style| style.border_1().border_color(theme.accent))
                        .child(tr!("usage.view_summary"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reveal_compaction_summary(message_id, cx)
                        }))
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                this.reveal_compaction_summary(message_id, cx);
                                cx.stop_propagation();
                            }
                        })),
                );
            }
            body = body.child(section);
        }

        div()
            .track_focus(&self.context_panel_focus)
            .tab_index(0)
            .tab_group()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_context_panel(cx);
                    window.focus(&this.composer.read(cx).focus(), cx);
                    cx.stop_propagation();
                }
            }))
            .child(body)
            .into_any_element()
    }

    fn reveal_compaction_summary(&mut self, message_id: Uuid, cx: &mut Context<Self>) {
        // A click is a one-shot user action. Finding its row must never run in render.
        let index = self.selected_session().and_then(|session| {
            session
                .messages
                .iter()
                .position(|message| message.id == message_id)
        });
        if let Some(index) = index {
            if let Some(row) = self
                .transcript_row_kinds
                .borrow()
                .iter()
                .position(|kind| *kind == TranscriptRowKind::Message(index))
            {
                self.active_transcript_rows().scroll_to(ListOffset {
                    item_ix: row,
                    offset_in_item: Pixels::ZERO,
                });
                self.transcript_anchor_following.set(false);
                self.transcript_is_scrolled.set(true);
                cx.notify();
            }
        }
    }

    pub(super) fn refresh_context_summary_id(&mut self, session_id: Uuid) {
        let id = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                let summary = session.compaction.as_ref()?.summary.as_deref()?.trim();
                session
                    .messages
                    .iter()
                    .rev()
                    .find(|message| {
                        message.role == MessageRole::Compaction && message.content.trim() == summary
                    })
                    .map(|message| message.id)
            });
        self.context_summary_ids.insert(session_id, id);
    }
}

fn inspector_section(theme: &Theme, title: String) -> Div {
    div()
        .pt(px(16.0))
        .border_t_1()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .gap(px(9.0))
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
}

fn inspector_row(theme: &Theme, label: String, value: String) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(div().text_color(theme.text_secondary).child(label))
        .child(div().flex_1())
        .child(div().text_color(theme.text).child(value))
}

fn context_percent(usage: ContextUsage) -> Option<f64> {
    if !usage.measured && usage.tokens == 0 {
        return None;
    }
    usage
        .window
        .filter(|window| *window > 0)
        .map(|window| usage.tokens as f64 * 100.0 / window as f64)
}

/// The footer ring paints only the track until the provider reports a window.
fn context_gauge(percent: Option<f64>, track: Hsla, fill: Hsla) -> impl IntoElement {
    const SIZE: f32 = 13.0;
    const STROKE: f32 = 2.5;
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = px((SIZE - STROKE) / 2.0);
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

fn meter_bar(theme: &Theme, percent: f64) -> Div {
    let fraction = (percent / 100.0).clamp(0.0, 1.0) as f32;
    let fill = if percent >= 95.0 {
        theme.danger
    } else if percent >= 80.0 {
        theme.warning
    } else {
        theme.gauge
    };
    div()
        .relative()
        .h(px(21.0))
        .w_full()
        .child(
            div()
                .h(px(5.0))
                .w_full()
                .rounded_full()
                .bg(theme.overlay_strong)
                .child(div().h_full().w(relative(fraction)).rounded_full().bg(fill)),
        )
        .child(
            div()
                .absolute()
                .left(relative(0.8))
                .top(px(6.0))
                .text_size(ui_px(9.0))
                .text_color(theme.text_tertiary)
                .child("80"),
        )
        .child(
            div()
                .absolute()
                .left(relative(0.95))
                .top(px(6.0))
                .text_size(ui_px(9.0))
                .text_color(theme.text_tertiary)
                .child("95"),
        )
}

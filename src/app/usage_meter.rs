//! Session context inspector and the compact footer gauge that opens it.
use crate::theme::ui_px;
use crate::ui::ActivationExt;

use gpui::PathBuilder;

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
        if matches!(self.mode, WorkspaceMode::Settings | WorkspaceMode::Usage)
            || !self.usage_meter_available()
        {
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
        let mut usage = session.context_usage.clone();
        if let Some(window) = configured {
            usage.get_or_insert(ContextUsage::default()).window = Some(window);
        }
        usage
    }

    pub(super) fn render_usage_meter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.selected_session()?;
        let usage = self.effective_context_usage(session);
        let theme = Theme::current(cx);
        let percent = usage.as_ref().and_then(context_percent);
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
        let native_session_id = session
            .provider_cursor
            .as_ref()
            .map(ProviderResumeCursor::native_id)
            .or(session.native_session_id.as_deref())
            .filter(|id| !id.is_empty())
            .unwrap_or("—")
            .to_owned();
        let usage = self.effective_context_usage(session);
        let compaction = session.compaction.clone();
        let model_name = session.model.clone();
        let selected_window = session.context_window.clone();
        let model = self.model_metadata_for_session(session).cloned();
        let can_compact = self.runtimes.contains_key(&session_id);
        let percent = usage.as_ref().and_then(context_percent);
        let status = match percent {
            Some(value) if value >= 95.0 => Some(tr!("usage.full")),
            Some(value) if value >= 80.0 => Some(tr!("usage.near_full")),
            Some(_) => Some(tr!("usage.healthy")),
            None => Some(tr!("usage.unknown")),
        };
        let status_color = match percent {
            Some(value) if value >= 95.0 => theme.danger_text,
            Some(value) if value >= 80.0 => theme.warning_text,
            _ => theme.text_secondary,
        };
        let breakdown = ContextBreakdown::new(usage.as_ref());
        let headline = breakdown
            .occupancy_known
            .then(|| format_tokens(breakdown.used))
            .unwrap_or_else(|| "—".to_owned());
        let capacity = breakdown
            .window
            .map(|window| format!("/ {}", format_tokens(window)))
            .unwrap_or_else(|| tr!("usage.window_unknown"));
        let hint = match (percent, breakdown.window) {
            (Some(value), Some(_)) if value >= 95.0 => tr!("usage.full_detail"),
            (Some(value), Some(_)) if value >= 80.0 => tr!("usage.near_full_detail"),
            (_, Some(window)) if breakdown.occupancy_known => tr!(
                "usage.remaining",
                count = format_tokens(window.saturating_sub(breakdown.used))
            ),
            (_, Some(_)) => tr!("usage.unknown_context"),
            (_, None) if breakdown.occupancy_known => tr!("usage.window_unknown_detail"),
            _ => tr!("usage.not_measured_detail"),
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
            .p(px(12.0))
            .rounded(px(10.0))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(tr!("usage.used_context")),
            )
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
                    .child(div().text_color(theme.text_secondary).child(capacity))
                    .when_some(status, |element, status| {
                        element.child(div().text_color(status_color).child(status))
                    }),
            )
            .when_some(percent, |element, percent| {
                element.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(format_percent(percent)),
                        )
                        .child(div().text_color(theme.text_secondary).child(hint.clone())),
                )
            })
            .when(percent.is_none(), |element| {
                element.child(div().text_color(theme.text_secondary).child(hint.clone()))
            })
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(context_stat(
                        &theme,
                        tr!("usage.total_tokens"),
                        usage
                            .as_ref()
                            .and_then(|usage| usage.total_tokens)
                            .map(format_tokens)
                            .unwrap_or_else(|| "—".to_owned()),
                    ))
                    .child(context_stat(
                        &theme,
                        tr!("usage.cache_hit_rate"),
                        usage
                            .as_ref()
                            .and_then(|usage| {
                                cache_hit_percent(
                                    usage.cache_read.unwrap_or(0),
                                    usage.prompt_tokens.unwrap_or(0),
                                )
                            })
                            .map(format_percent)
                            .unwrap_or_else(|| "—".to_owned()),
                    )),
            )
            .child(context_waffle(&breakdown, &theme))
            .child(context_legend(&breakdown, &theme))
            .when(failed, |element| {
                element.child(
                    div().text_color(theme.warning_text).child(
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
                        .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
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
            } else if let Some(window) = usage.as_ref().and_then(|usage| usage.window) {
                model_section = model_section.child(inspector_row(
                    &theme,
                    tr!("usage.window_size"),
                    format_tokens(window),
                ));
            }
        } else if let Some(window) = usage.as_ref().and_then(|usage| usage.window) {
            model_section = model_section.child(inspector_row(
                &theme,
                tr!("usage.window_size"),
                format_tokens(window),
            ));
        }
        body = body.child(model_section);
        body = body.child(
            inspector_section(&theme, tr!("usage.opencode_session_id")).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        div()
                            .min_w_0()
                            .font_family(crate::theme::code_font_family())
                            .child(native_session_id.clone()),
                    )
                    .when(native_session_id != "—", |row| {
                        let focus = self.transcript_control_focus("context-copy-session-id", cx);
                        row.child(
                            crate::ui::icon_button(
                                "context-copy-session-id",
                                "icons/copy.svg",
                                theme,
                            )
                            .flex_none()
                            .track_focus(&focus)
                            .tab_index(0)
                            .focus_visible(|style| style.border_1().border_color(theme.accent))
                            .tooltip(Tooltip::text(tr!("common.copy")))
                            .on_activation(cx, move |this, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    native_session_id.clone(),
                                ));
                                this.show_success_toast(tr!("common.copied"));
                                cx.notify();
                            }),
                        )
                    }),
            ),
        );

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
                        .text_color(theme.link)
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

fn context_percent(usage: &ContextUsage) -> Option<f64> {
    if !usage.measured && usage.tokens == 0 {
        return None;
    }
    usage
        .window
        .filter(|window| *window > 0)
        .map(|window| usage.tokens as f64 * 100.0 / window as f64)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ContextPartKind {
    Input,
    CacheRead,
    CacheWrite,
    Output,
    Reasoning,
    Unclassified,
    Used,
    Remaining,
    Unknown,
}

struct ContextPart {
    kind: ContextPartKind,
    tokens: Option<u64>,
}

struct ContextBreakdown {
    entries: Vec<ContextPart>,
    used: u64,
    window: Option<u64>,
    denominator: u64,
    occupancy_known: bool,
}

impl ContextPartKind {
    fn label(self) -> String {
        match self {
            Self::Input => tr!("usage.input"),
            Self::CacheRead => tr!("usage.cache_read"),
            Self::CacheWrite => tr!("usage.cache_write"),
            Self::Output => tr!("usage.output"),
            Self::Reasoning => tr!("usage.reasoning"),
            Self::Unclassified => tr!("usage.unclassified_context"),
            Self::Used => tr!("usage.used_context"),
            Self::Remaining => tr!("usage.available_context"),
            Self::Unknown => tr!("usage.unknown_context"),
        }
    }

    fn color(self, theme: &Theme) -> Hsla {
        match self {
            Self::Input => theme.accent,
            Self::CacheRead => theme.success,
            Self::CacheWrite => theme.gauge,
            Self::Output => theme.link,
            Self::Reasoning => theme.code_keyword,
            Self::Unclassified => theme.text_tertiary,
            Self::Used => theme.accent,
            Self::Remaining | Self::Unknown => theme.overlay_strong,
        }
    }
}

impl ContextBreakdown {
    fn new(usage: Option<&ContextUsage>) -> Self {
        let window = usage.and_then(|usage| usage.window.filter(|window| *window > 0));
        let occupancy_known = usage.is_some_and(|usage| usage.measured || usage.tokens > 0);
        let used = usage.map(|usage| usage.tokens).unwrap_or(0);
        let denominator = window.unwrap_or(used).max(used);
        let mut entries = Vec::new();
        if !occupancy_known {
            entries.push(ContextPart {
                kind: ContextPartKind::Unknown,
                tokens: None,
            });
        } else if let Some(latest) = usage.and_then(|usage| usage.latest) {
            let categories = [
                (ContextPartKind::Input, latest.input),
                (ContextPartKind::CacheRead, latest.cache_read),
                (ContextPartKind::CacheWrite, latest.cache_write),
                (ContextPartKind::Output, latest.output),
                (ContextPartKind::Reasoning, latest.reasoning),
            ];
            let total: u128 = categories.iter().map(|(_, count)| u128::from(*count)).sum();
            if total <= u128::from(used) {
                for (kind, tokens) in categories {
                    if tokens > 0 {
                        entries.push(ContextPart {
                            kind,
                            tokens: Some(tokens),
                        });
                    }
                }
                if total < u128::from(used) {
                    entries.push(ContextPart {
                        kind: ContextPartKind::Unclassified,
                        tokens: Some(used - total as u64),
                    });
                }
            } else {
                // A provider total can disagree with its split. Keep measured occupancy intact.
                entries.push(ContextPart {
                    kind: ContextPartKind::Used,
                    tokens: Some(used),
                });
            }
        } else {
            entries.push(ContextPart {
                kind: ContextPartKind::Used,
                tokens: Some(used),
            });
        }
        if occupancy_known && denominator > used {
            entries.push(ContextPart {
                kind: ContextPartKind::Remaining,
                tokens: Some(denominator - used),
            });
        }
        if entries.is_empty() {
            entries.push(ContextPart {
                kind: ContextPartKind::Used,
                tokens: Some(0),
            });
        }
        Self {
            entries,
            used,
            window,
            denominator,
            occupancy_known,
        }
    }
}

fn context_stat(theme: &Theme, label: String, value: String) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .p(px(8.0))
        .rounded(px(7.0))
        .bg(theme.overlay)
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(div().text_color(theme.text_secondary).child(label))
        .child(div().font_weight(FontWeight::MEDIUM).child(value))
}

fn context_legend(breakdown: &ContextBreakdown, theme: &Theme) -> Div {
    let mut legend = div().flex().flex_col().gap(px(5.0));
    for entry in &breakdown.entries {
        let count = entry
            .tokens
            .map(format_tokens)
            .unwrap_or_else(|| "—".to_owned());
        let percent = entry
            .tokens
            .map(|tokens| context_part_percent(tokens, breakdown.denominator))
            .unwrap_or_else(|| "—".to_owned());
        legend = legend.child(
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(
                    div()
                        .size(px(8.0))
                        .flex_none()
                        .rounded(px(2.0))
                        .bg(entry.kind.color(theme)),
                )
                .child(
                    div()
                        .flex_1()
                        .text_color(theme.text_secondary)
                        .child(entry.kind.label()),
                )
                .child(div().text_color(theme.text).child(count))
                .child(
                    div()
                        .w(px(55.0))
                        .flex_none()
                        .text_color(theme.text_secondary)
                        .child(percent),
                ),
        );
    }
    legend
}

fn context_part_percent(tokens: u64, denominator: u64) -> String {
    if denominator == 0 || tokens == 0 {
        return format_percent(0.0);
    }
    let percent = tokens as f64 * 100.0 / denominator as f64;
    if percent < 0.001 {
        tr!("usage.below_min_percent")
    } else if percent < 0.1 {
        format!("{percent:.3}%")
    } else {
        format_percent(percent)
    }
}

fn context_waffle(breakdown: &ContextBreakdown, theme: &Theme) -> impl IntoElement {
    const MIN_COLUMNS: usize = 10;
    const MAX_COLUMNS: usize = 60;
    const ROWS: usize = 8;
    const GAP: f32 = 3.0;
    const HEIGHT: f32 = 150.0;
    let entries = breakdown
        .entries
        .iter()
        .map(|entry| (entry.kind, entry.tokens))
        .collect::<Vec<_>>();
    let denominator = breakdown.denominator;
    let palette = entries
        .iter()
        .map(|(kind, _)| kind.color(theme))
        .collect::<Vec<_>>();
    let kinds = entries.iter().map(|(kind, _)| *kind).collect::<Vec<_>>();
    let cell_base = theme.overlay_strong;
    let unknown_alt = theme.text_tertiary;

    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let gap = px(GAP);
            let columns = ((bounds.size.width / px(17.0)) as usize).clamp(MIN_COLUMNS, MAX_COLUMNS);
            let cell_width = (bounds.size.width - gap * (columns - 1) as f32) / columns as f32;
            let cell_height = (px(HEIGHT) - gap * (ROWS - 1) as f32) / ROWS as f32;
            let cell = cell_width.min(cell_height).max(px(4.0));
            let cell_count = columns * ROWS;
            let grid_width = cell * columns as f32 + gap * (columns - 1) as f32;
            let grid_height = cell * ROWS as f32 + gap * (ROWS - 1) as f32;
            let left = bounds.origin.x + (bounds.size.width - grid_width) / 2.0;
            let top = bounds.origin.y + (bounds.size.height - grid_height) / 2.0;
            let mut ranges = Vec::with_capacity(entries.len());
            let mut cumulative = 0u64;
            for (_, tokens) in &entries {
                let end = cumulative.saturating_add(tokens.unwrap_or(denominator));
                ranges.push((cumulative, end));
                cumulative = end;
            }

            for cell_index in 0..cell_count {
                let x = left + (cell_index % columns) as f32 * (cell + gap);
                let y = top + (cell_index / columns) as f32 * (cell + gap);
                window.paint_quad(
                    gpui::fill(
                        Bounds::new(
                            point(x, y),
                            gpui::Size {
                                width: cell,
                                height: cell,
                            },
                        ),
                        cell_base,
                    )
                    .corner_radii(px(3.0)),
                );
                if denominator == 0 {
                    if kinds[0] == ContextPartKind::Unknown
                        && (cell_index + cell_index / columns) % 2 == 0
                    {
                        window.paint_quad(
                            gpui::fill(
                                Bounds::new(
                                    point(x, y),
                                    gpui::Size {
                                        width: cell,
                                        height: cell,
                                    },
                                ),
                                unknown_alt,
                            )
                            .corner_radii(px(3.0)),
                        );
                    }
                    continue;
                }

                let denominator_wide = u128::from(denominator);
                let cell_start = cell_index as u128 * denominator_wide;
                let cell_end = (cell_index + 1) as u128 * denominator_wide;
                for (segment, (range_start, range_end)) in ranges.iter().enumerate() {
                    let start = u128::from(*range_start) * cell_count as u128;
                    let end = u128::from(*range_end) * cell_count as u128;
                    let overlap_start = start.max(cell_start);
                    let overlap_end = end.min(cell_end);
                    if overlap_end <= overlap_start {
                        continue;
                    }

                    let offset = (overlap_start - cell_start) as f64 / denominator as f64
                        * cell.as_f32() as f64;
                    let actual_width = (overlap_end - overlap_start) as f64 / denominator as f64
                        * cell.as_f32() as f64;
                    let offset = offset as f32;
                    let width = actual_width as f32;
                    let width = width.min(cell.as_f32() - offset);
                    if width <= 0.0 {
                        continue;
                    }
                    let color = if kinds[segment] == ContextPartKind::Unknown
                        && (cell_index + cell_index / columns) % 2 == 0
                    {
                        unknown_alt
                    } else {
                        palette[segment]
                    };
                    window.paint_quad(
                        gpui::fill(
                            Bounds::new(
                                point(x + px(offset), y),
                                gpui::Size {
                                    width: px(width),
                                    height: cell,
                                },
                            ),
                            color,
                        )
                        .corner_radii(px(2.0)),
                    );
                }
            }
        },
    )
    .w_full()
    .h(px(HEIGHT))
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

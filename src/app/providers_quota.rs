//! Account allowance in the provider inspector. Rendering reads a stored snapshot only.

use fintwind_protocol::provider_quota::{
    ProviderQuota, QuotaStatus, QuotaWindow, QuotaWindowKind, supports_provider_quota,
};

use super::*;

#[derive(Default)]
pub(super) struct QuotaViewState {
    generation: u64,
    provider: Option<String>,
    loading: bool,
    reading: Option<ProviderQuota>,
}

impl Fintwind {
    pub(super) fn invalidate_provider_quota(&mut self) {
        self.provider_quota = QuotaViewState {
            generation: self.provider_quota.generation.wrapping_add(1),
            ..Default::default()
        };
    }

    pub(super) fn load_selected_provider_quota(&mut self, force: bool, cx: &mut Context<Self>) {
        self.invalidate_provider_quota();
        let Some(provider) = self
            .selected_builtin_catalog_id()
            .filter(|id| supports_provider_quota(id))
            .map(str::to_owned)
        else {
            return;
        };
        if !self.provider_is_authorized(&provider) || self.provider_needs_auth(&provider) {
            return;
        }
        let Some(workspace) = self.provider_workspace() else {
            return;
        };
        let generation = self.provider_quota.generation;
        self.provider_quota.provider = Some(provider.clone());
        self.provider_quota.loading = true;
        let daemon = self.daemon.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let expected_workspace = workspace.clone();
            let requested = provider.clone();
            let reading = cx
                .background_executor()
                .spawn(async move {
                    let (binary, directory) = workspace;
                    StateStore::remote(daemon)
                        .fetch_provider_quota(binary, directory, requested, force)
                        .unwrap_or_else(|_| ProviderQuota::empty(QuotaStatus::Unavailable))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.provider_quota.generation != generation {
                    return;
                }
                if this.provider_workspace().as_ref() != Some(&expected_workspace) {
                    this.load_selected_provider_quota(false, cx);
                    return;
                }
                this.provider_quota.loading = false;
                this.provider_quota.reading = Some(reading);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_provider_quota(
        &self,
        provider: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !supports_provider_quota(provider)
            || !self.provider_is_authorized(provider)
            || self.provider_needs_auth(provider)
        {
            return None;
        }
        let state = &self.provider_quota;
        if state.provider.as_deref() != Some(provider) {
            return None;
        }
        let reading = state.reading.as_ref();
        if reading.is_some_and(|q| {
            matches!(
                q.status,
                QuotaStatus::Unsupported | QuotaStatus::NoCredential
            )
        }) {
            return None;
        }
        let loading = state.loading;
        // Reuse the inspector's native command control. Ely's reviewed button
        // uses a pointing cursor; keep Fintwind's Windows cursor/key contract.
        let refresh = super::providers_page::small_action_button(
            "refresh-provider-quota",
            "icons/refresh-cw.svg",
            if loading {
                tr!("providers.quota_loading")
            } else {
                tr!("providers.quota_refresh")
            },
            theme.text_secondary,
            theme,
        )
        .tab_index(0)
        .focus_visible(|style| style.border_color(theme.accent_text))
        .when(loading, |button| button.opacity(0.5))
        .on_click(cx.listener(|this, _, _, cx| {
            if !this.provider_quota.loading {
                this.load_selected_provider_quota(true, cx);
            }
        }))
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
            if !this.provider_quota.loading
                && !event.keystroke.modifiers.modified()
                && matches!(event.keystroke.key.as_str(), "enter" | "space")
            {
                this.load_selected_provider_quota(true, cx);
                cx.stop_propagation();
            }
        }));
        let mut section = div().mt_4().flex().flex_col().gap_3().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_size(ui_px(11.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_secondary)
                        .child(tr!("providers.quota_title")),
                )
                .child(refresh),
        );
        if let Some(q) = reading {
            if let Some(plan) = &q.plan {
                section = section.child(
                    div()
                        .text_size(ui_px(11.0))
                        .text_color(theme.text_secondary)
                        .child(plan.clone()),
                );
            }
            for balance in &q.balances {
                let label = if balance.unit == "credits" {
                    tr!("providers.quota_credits")
                } else {
                    tr!("providers.quota_balance")
                };
                let value = if balance.unlimited {
                    tr!("providers.quota_unlimited")
                } else if balance.unit.is_empty() || balance.unit == "credits" {
                    balance.remaining.clone()
                } else {
                    format!("{} {}", balance.remaining, balance.unit)
                };
                section = section.child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .text_size(ui_px(11.0))
                                .text_color(theme.text_secondary)
                                .child(label),
                        )
                        .child(
                            div()
                                .text_size(ui_px(14.0))
                                .text_color(theme.text)
                                .child(value),
                        ),
                );
            }
            for window in &q.windows {
                section = section.child(quota_window(window, theme));
            }
            let status = match q.status {
                QuotaStatus::NeedsAuth => Some(tr!("providers.quota_needs_auth")),
                QuotaStatus::AccessDenied => Some(tr!("providers.quota_access_denied")),
                QuotaStatus::RateLimited => Some(tr!("providers.quota_rate_limited")),
                QuotaStatus::Unavailable => Some(tr!("providers.quota_unavailable")),
                QuotaStatus::NoData => Some(tr!("providers.quota_no_data")),
                _ => None,
            };
            if let Some(status) = status {
                section = section.child(
                    div()
                        .text_size(ui_px(10.5))
                        .text_color(theme.warning_text)
                        .child(status),
                );
            }
            if let Some(at) = q.measured_at.and_then(|at| DateTime::from_timestamp(at, 0)) {
                let time = at.with_timezone(&Local).format("%m-%d %H:%M").to_string();
                let measured = if q.status == QuotaStatus::Ready {
                    tr!("providers.quota_measured", time = time)
                } else {
                    tr!("providers.quota_cached_measured", time = time)
                };
                section = section.child(
                    div()
                        .text_size(ui_px(10.0))
                        .text_color(theme.text_tertiary)
                        .child(measured),
                );
            }
        }
        Some(section.into_any_element())
    }
}

fn quota_window(window: &QuotaWindow, theme: &Theme) -> AnyElement {
    let label = match window.kind {
        QuotaWindowKind::Rolling => match window.span_seconds {
            Some(s) if s > 0 && s % 86400 == 0 => tr!("providers.quota_days", count = s / 86400),
            Some(s) if s > 0 && s % 3600 == 0 => tr!("providers.quota_hours", count = s / 3600),
            Some(s) if s > 0 => tr!("providers.quota_minutes", count = s.div_ceil(60)),
            _ => tr!("providers.quota_window"),
        },
        QuotaWindowKind::Weekly => tr!("providers.quota_weekly"),
        QuotaWindowKind::Monthly => tr!("providers.quota_monthly"),
        QuotaWindowKind::ToolCalls => tr!("providers.quota_tool_calls"),
        QuotaWindowKind::Chat => tr!("providers.quota_chat"),
        QuotaWindowKind::Completions => tr!("providers.quota_completions"),
        QuotaWindowKind::Premium => tr!("providers.quota_premium"),
        QuotaWindowKind::Model => window.model.clone().unwrap_or_default(),
    };
    let label = if window.kind != QuotaWindowKind::Model {
        window
            .model
            .as_ref()
            .map(|model| format!("{model} · {label}"))
            .unwrap_or(label)
    } else {
        label
    };
    let value = if window.unlimited {
        tr!("providers.quota_unlimited")
    } else if let Some(used) = window.used_percent {
        tr!(
            "providers.quota_remaining",
            percent = format!("{:.1}", (100.0 - used).clamp(0.0, 100.0))
        )
    } else {
        "—".into()
    };
    let mut row = div().flex().flex_col().gap_1().child(
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui_px(11.0))
                    .text_color(theme.text_secondary)
                    .child(label),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(ui_px(11.0))
                    .text_color(theme.text)
                    .child(value),
            ),
    );
    if let Some(used) = window.used_percent {
        // Ely's thin-track geometry without glide/sweep: allowance is not task
        // progress, and static data must not lease an animation clock.
        row = row.child(
            div()
                .h_1()
                .w_full()
                .rounded_full()
                .bg(theme.overlay_strong)
                .child(
                    div()
                        .h_full()
                        .rounded_full()
                        .w(gpui::relative(
                            ((100.0 - used).clamp(0.0, 100.0) / 100.0) as f32,
                        ))
                        .bg(if used >= 90.0 {
                            theme.warning_text
                        } else {
                            theme.text_secondary
                        }),
                ),
        );
    }
    if let Some(reset) = window.resets_at {
        if let Some(at) = DateTime::from_timestamp(reset, 0) {
            row = row.child(
                div()
                    .text_size(ui_px(10.0))
                    .text_color(theme.text_tertiary)
                    .child(tr!(
                        "providers.quota_resets",
                        time = at.with_timezone(&Local).format("%m-%d %H:%M").to_string()
                    )),
            );
        }
    }
    row.into_any_element()
}

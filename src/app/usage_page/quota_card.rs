//! Subscription balance & quota KPI card on the usage dashboard.
//! Matches the two-layer KPI card shell while rendering rich balance and quota
//! readings across connected providers, with click-to-inspect multi-window
//! breakdown, pagination beyond three providers, and inline refresh.

use ely_gpui_component::{
    motion::Spinner,
    primitives::{Icon, IconName},
    theme::IconSize,
    typography::tabular,
};
use fintwind_protocol::provider_quota::{
    ProviderQuota, QuotaBalance, QuotaStatus, QuotaWindow, QuotaWindowKind,
};

use super::*;
use crate::ui::ActivationExt;
use crate::ui::loading::{LoadingDelay, LoadingPhase};
use crate::ui::{model_mark_element, provider_color, provider_mark};

const PAGE_SIZE: usize = 3;

impl Fintwind {
    pub(super) fn render_usage_subscription_card(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let cards = self.subscription_cards();
        let (quota_phase, wake) = if self.subscriptions_connection_error() {
            *self.usage_quota_loading.borrow_mut() = None;
            (LoadingPhase::Done, None)
        } else {
            LoadingDelay::observe(
                &mut self.usage_quota_loading.borrow_mut(),
                (),
                cards.is_empty()
                    && !self.subscriptions_connections_loaded()
                    && self.subscriptions_loading(),
            )
        };
        if let Some(wake) = wake {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wake.delay()).await;
                wake.fired();
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }

        let focused = if cards.len() > 1 {
            self.usage_quota_focus
                .as_deref()
                .and_then(|target| cards.iter().find(|(id, _, _)| id == target))
        } else {
            None
        };

        let total_pages = cards.len().div_ceil(PAGE_SIZE).max(1);
        let page = self.usage_quota_page.min(total_pages - 1);
        let any_loading = self.subscriptions_loading();

        let header_left = if let Some((provider, _, _)) = focused {
            let name = self.subscription_display_name(provider);
            div()
                .id("usage-quota-back")
                .role(gpui::Role::Button)
                .tab_index(0)
                .flex_1()
                .min_w_0()
                .rounded(px(5.0))
                .px_1()
                .py(px(1.0))
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .focus_visible(|s| s.border_1().border_color(theme.accent_focus))
                .hover(|s| s.bg(theme.overlay))
                .tooltip(Tooltip::text(tr!("usage_page.quota_back")))
                .child(
                    Icon::new(IconName::ChevronLeft)
                        .size(IconSize::Xs)
                        .color(theme.text_secondary),
                )
                .child(model_mark_element(
                    provider_mark(provider, theme.is_dark),
                    12.0,
                    provider_color(theme, provider),
                ))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(ui_px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_secondary)
                        .child(name),
                )
                .on_activation(cx, |this, _, cx| {
                    this.usage_quota_focus = None;
                    cx.notify();
                })
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_1p5()
                .child(
                    Icon::new(IconName::Wallet)
                        .size(IconSize::Sm)
                        .color(theme.text_tertiary),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(ui_px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_secondary)
                        .child(tr!("usage_page.kpi_balance")),
                )
                .into_any_element()
        };

        let mut header_right = div().flex_none().flex().items_center().gap_1();
        if focused.is_none() && total_pages > 1 {
            header_right = header_right
                .child(
                    div()
                        .id("usage-quota-prev-page")
                        .role(gpui::Role::Button)
                        .tab_index(0)
                        .size(px(18.0))
                        .rounded(px(4.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .focus_visible(|s| s.border_1().border_color(theme.accent_focus))
                        .hover(|s| s.bg(theme.overlay))
                        .child(
                            Icon::new(IconName::ChevronLeft)
                                .size(IconSize::Xs)
                                .color(theme.text_secondary),
                        )
                        .on_activation(cx, move |this, _, cx| {
                            this.usage_quota_page = if page == 0 {
                                total_pages - 1
                            } else {
                                page - 1
                            };
                            cx.notify();
                        }),
                )
                .child(
                    tabular(div())
                        .text_size(ui_px(10.5))
                        .text_color(theme.text_tertiary)
                        .child(format!("{}/{}", page + 1, total_pages)),
                )
                .child(
                    div()
                        .id("usage-quota-next-page")
                        .role(gpui::Role::Button)
                        .tab_index(0)
                        .size(px(18.0))
                        .rounded(px(4.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .focus_visible(|s| s.border_1().border_color(theme.accent_focus))
                        .hover(|s| s.bg(theme.overlay))
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .size(IconSize::Xs)
                                .color(theme.text_secondary),
                        )
                        .on_activation(cx, move |this, _, cx| {
                            this.usage_quota_page = (page + 1) % total_pages;
                            cx.notify();
                        }),
                );
        }

        let focus_provider_for_refresh = focused
            .map(|(p, _, _)| p.clone())
            .or_else(|| (cards.len() == 1).then(|| cards[0].0.clone()));
        let refresh_busy = focused
            .map(|(_, loading, _)| *loading)
            .unwrap_or(any_loading);

        if !cards.is_empty() {
            let refresh_graphic = if refresh_busy {
                Spinner::new("usage-quota-refresh-spin")
                    .size(IconSize::Xs)
                    .into_any_element()
            } else {
                Icon::new(IconName::RotateCw)
                    .size(IconSize::Xs)
                    .color(theme.text_tertiary)
                    .into_any_element()
            };
            header_right = header_right.child(
                div()
                    .id("usage-quota-card-refresh")
                    .role(gpui::Role::Button)
                    .aria_label(tr!("usage_page.refresh"))
                    .tab_index(0)
                    .size(px(20.0))
                    .rounded(px(5.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .focus_visible(|s| s.border_1().border_color(theme.accent_focus))
                    .hover(|s| s.bg(theme.overlay))
                    .tooltip(Tooltip::text(tr!("usage_page.refresh")))
                    .child(refresh_graphic)
                    .on_activation(cx, move |this, _, cx| {
                        if let Some(ref target) = focus_provider_for_refresh {
                            this.refresh_subscription(target, cx);
                        } else if !this.subscriptions_loading() {
                            this.load_subscriptions(true, cx);
                        }
                    }),
            );
        }

        let mut inner = div()
            .flex_1()
            .min_w_0()
            .rounded(px(9.0))
            .border_1()
            .border_color(theme.card_border())
            .bg(theme.composer)
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .justify_center();

        if self.subscriptions_connection_error() {
            inner = inner.gap_2().child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(icon("icons/alert.svg", 13.0, theme.danger_text))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui_px(12.0))
                                    .text_color(theme.danger_text)
                                    .child(tr!("usage_page.connections_failed")),
                            ),
                    )
                    .child(
                        div()
                            .id("usage-quota-retry")
                            .role(gpui::Role::Button)
                            .tab_index(0)
                            .px_2()
                            .py(px(3.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(theme.danger.opacity(0.45))
                            .bg(theme.danger_soft)
                            .text_size(ui_px(11.5))
                            .text_color(theme.danger_text)
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.danger.opacity(0.18)))
                            .child(tr!("common.retry"))
                            .on_activation(cx, |this, _, cx| this.load_subscriptions(true, cx)),
                    ),
            );
        } else if quota_phase != LoadingPhase::Done {
            inner = inner.gap_1p5().when(quota_phase == LoadingPhase::Visible, |box_div| {
                box_div.children((0..3).map(|ix| {
                    div()
                        .id(SharedString::from(format!("usage-quota-skel-{ix}")))
                        .h(px(20.0))
                        .w_full()
                        .rounded(px(5.0))
                        .bg(theme.overlay_strong)
                }))
            });
        } else if cards.is_empty() {
            inner = inner
                .justify_between()
                .gap_2()
                .child(
                    tabular(div())
                        .text_size(ui_px(22.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text_tertiary)
                        .child("—"),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(ui_px(12.0))
                        .text_color(theme.text_tertiary)
                        .child(tr!("usage_page.subscriptions_empty")),
                );
        } else if let Some((provider, loading, reading)) = focused.or_else(|| {
            (cards.len() == 1).then(|| &cards[0])
        }) {
            let show_header = focused.is_none();
            inner = inner.child(self.render_single_quota_detail(
                provider,
                *loading,
                reading.as_ref(),
                show_header,
                theme,
                cx,
            ));
        } else {
            let start = page * PAGE_SIZE;
            let visible = cards.iter().skip(start).take(PAGE_SIZE);
            inner = inner.justify_center().child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(visible.map(|(provider, loading, reading)| {
                        self.render_quota_row(provider, *loading, reading.as_ref(), theme, cx)
                    })),
            );
        }

        div()
            .min_w_0()
            .rounded(px(13.0))
            .border_1()
            .border_color(theme.card_border())
            .bg(theme.inset)
            .shadow_xs()
            .p(px(4.0))
            .flex()
            .flex_col()
            .child(
                div()
                    .px_2()
                    .py_1p5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(header_left)
                    .child(header_right),
            )
            .child(inner)
    }

    fn render_quota_row(
        &self,
        provider: &str,
        loading: bool,
        reading: Option<&ProviderQuota>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let display_name = self.subscription_display_name(provider);
        let provider_key = provider.to_owned();
        let has_data = reading
            .is_some_and(|quota| !quota.windows.is_empty() || !quota.balances.is_empty());

        let right_widget = if loading && !has_data {
            Spinner::new(SharedString::from(format!("usage-quota-spin-{provider}")))
                .size(IconSize::Xs)
                .into_any_element()
        } else if let Some(quota) = reading {
            if let Some(balance) = quota.balances.first() {
                let text = format_balance(balance);
                let color = if balance.unlimited {
                    theme.success_text
                } else {
                    theme.text
                };
                tabular(div())
                    .text_size(ui_px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(text)
                    .into_any_element()
            } else if let Some(window) = primary_window(&quota.windows) {
                if window.unlimited {
                    div()
                        .text_size(ui_px(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.success_text)
                        .child(tr!("providers.quota_unlimited"))
                        .into_any_element()
                } else if let Some(used) = window.used_percent.filter(|v| v.is_finite()) {
                    let remaining = (100.0 - used).clamp(0.0, 100.0);
                    let (text_color, bar_fill) = quota_tones(used, theme);
                    let extra_count = quota.windows.len().saturating_sub(1);
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(ui_px(10.5))
                                .text_color(theme.text_tertiary)
                                .child(window_label(window)),
                        )
                        .child(
                            div()
                                .w(px(34.0))
                                .h(px(4.5))
                                .rounded_full()
                                .overflow_hidden()
                                .bg(theme.border)
                                .child(
                                    div()
                                        .h_full()
                                        .w(gpui::relative((remaining / 100.0) as f32))
                                        .rounded_full()
                                        .bg(bar_fill),
                                ),
                        )
                        .child(
                            tabular(div())
                                .text_size(ui_px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(text_color)
                                .child(format!("{remaining:.0}%")),
                        )
                        .when(extra_count > 0, |row| {
                            row.child(
                                tabular(div())
                                    .text_size(ui_px(10.0))
                                    .text_color(theme.text_tertiary)
                                    .child(format!("+{extra_count}")),
                            )
                        })
                        .into_any_element()
                } else {
                    div()
                        .text_size(ui_px(11.5))
                        .text_color(theme.text_tertiary)
                        .child("—")
                        .into_any_element()
                }
            } else {
                let tone = status_tone(&quota.status, theme);
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(px(5.0)).rounded_full().bg(tone))
                    .child(
                        div()
                            .text_size(ui_px(11.0))
                            .text_color(tone)
                            .child(compact_status(&quota.status)),
                    )
                    .into_any_element()
            }
        } else {
            div()
                .text_size(ui_px(11.5))
                .text_color(theme.text_tertiary)
                .child("—")
                .into_any_element()
        };

        div()
            .id(SharedString::from(format!("usage-quota-row-{provider}")))
            .role(gpui::Role::Button)
            .aria_label(display_name.clone())
            .tab_index(0)
            .h(px(26.0))
            .px_1p5()
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .cursor_pointer()
            .focus_visible(|s| s.border_1().border_color(theme.accent_focus))
            .hover(|s| s.bg(theme.overlay))
            .tooltip(Tooltip::text(card_tooltip(&display_name, reading)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(model_mark_element(
                        provider_mark(provider, theme.is_dark),
                        13.0,
                        provider_color(theme, provider),
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(display_name),
                    ),
            )
            .child(div().flex_none().child(right_widget))
            .on_activation(cx, move |this, _, cx| {
                if has_data {
                    this.usage_quota_focus = Some(provider_key.clone());
                    cx.notify();
                } else if !loading {
                    this.refresh_subscription(&provider_key, cx);
                }
            })
    }

    fn render_single_quota_detail(
        &self,
        provider: &str,
        loading: bool,
        reading: Option<&ProviderQuota>,
        show_header: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let display_name = self.subscription_display_name(provider);
        let provider_key = provider.to_owned();
        let has_data = reading
            .is_some_and(|quota| !quota.windows.is_empty() || !quota.balances.is_empty());

        let mut panel = div()
            .id(SharedString::from(format!("usage-quota-detail-{provider}")))
            .flex()
            .flex_col()
            .justify_between()
            .gap_1p5()
            .tooltip(Tooltip::text(card_tooltip(&display_name, reading)));

        if show_header {
            let plan = reading
                .and_then(|q| q.plan.as_deref())
                .filter(|p| !p.is_empty())
                .map(ToOwned::to_owned);
            let account = reading
                .and_then(|q| q.account.as_deref())
                .filter(|a| !a.is_empty())
                .map(ToOwned::to_owned);
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(model_mark_element(
                                provider_mark(provider, theme.is_dark),
                                13.0,
                                provider_color(theme, provider),
                            ))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(display_name),
                            )
                            .children(plan.map(|plan| {
                                div()
                                    .flex_none()
                                    .px_1p5()
                                    .py(px(1.0))
                                    .rounded(px(4.0))
                                    .bg(theme.overlay)
                                    .text_size(ui_px(10.0))
                                    .text_color(theme.text_secondary)
                                    .child(plan)
                            })),
                    )
                    .children(account.map(|account| {
                        div()
                            .flex_none()
                            .max_w(px(100.0))
                            .truncate()
                            .text_size(ui_px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(account)
                    })),
            );
        }

        if loading && !has_data {
            panel = panel.child(
                div().py_2().flex().items_center().gap_2().child(
                    Spinner::new(SharedString::from(format!("usage-quota-detail-spin-{provider}")))
                        .size(IconSize::Sm),
                ),
            );
            return panel;
        }

        let Some(quota) = reading else {
            return panel.child(
                div()
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_tertiary)
                    .child("—"),
            );
        };

        for balance in &quota.balances {
            let label = if balance.unit == "credits" {
                tr!("providers.quota_credits")
            } else {
                tr!("providers.quota_balance")
            };
            let large = quota.windows.is_empty() && quota.balances.len() == 1;
            panel = panel.child(
                div()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .gap_2()
                    .child(
                        tabular(div())
                            .text_size(ui_px(if large { 22.0 } else { 15.0 }))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if balance.unlimited {
                                theme.success_text
                            } else {
                                theme.text
                            })
                            .child(format_balance(balance)),
                    )
                    .child(
                        div()
                            .text_size(ui_px(11.5))
                            .text_color(theme.text_tertiary)
                            .child(label),
                    ),
            );
        }

        let max_windows = if quota.balances.is_empty() { 2 } else { 1 };
        for window in quota.windows.iter().take(max_windows) {
            let title = window_label(window);
            let reset = format_reset(window.resets_at);
            if window.unlimited {
                panel = panel.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_size(ui_px(11.5))
                                .text_color(theme.text_secondary)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(ui_px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.success_text)
                                .child(tr!("providers.quota_unlimited")),
                        ),
                );
            } else if let Some(used) = window.used_percent.filter(|v| v.is_finite()) {
                let remaining = (100.0 - used).clamp(0.0, 100.0);
                let (text_color, bar_fill) = quota_tones(used, theme);
                panel = panel.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .child(
                                            div()
                                                .text_size(ui_px(11.5))
                                                .text_color(theme.text_secondary)
                                                .child(title),
                                        )
                                        .children(reset.map(|r| {
                                            tabular(div())
                                                .truncate()
                                                .text_size(ui_px(10.5))
                                                .text_color(theme.text_tertiary)
                                                .child(r)
                                        })),
                                )
                                .child(
                                    tabular(div())
                                        .flex_none()
                                        .text_size(ui_px(12.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(text_color)
                                        .child(format!("{remaining:.0}%")),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .h(px(5.0))
                                .rounded_full()
                                .overflow_hidden()
                                .bg(theme.border)
                                .child(
                                    div()
                                        .h_full()
                                        .w(gpui::relative((remaining / 100.0) as f32))
                                        .rounded_full()
                                        .bg(bar_fill),
                                ),
                        ),
                );
            }
        }

        if quota.balances.is_empty() && quota.windows.is_empty() {
            let tone = status_tone(&quota.status, theme);
            panel = panel.child(
                div()
                    .id("usage-quota-detail-retry")
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .cursor_pointer()
                    .child(div().size(px(6.0)).rounded_full().bg(tone))
                    .child(
                        div()
                            .text_size(ui_px(12.0))
                            .text_color(tone)
                            .child(compact_status(&quota.status)),
                    )
                    .on_activation(cx, move |this, _, cx| {
                        this.refresh_subscription(&provider_key, cx);
                    }),
            );
        }

        panel
    }
}

fn quota_tones(used: f64, theme: &Theme) -> (Hsla, Hsla) {
    if used >= 95.0 {
        (theme.danger_text, theme.danger)
    } else if used >= 80.0 {
        (theme.warning_text, theme.warning)
    } else {
        (theme.text, theme.accent)
    }
}

fn status_tone(status: &QuotaStatus, theme: &Theme) -> Hsla {
    match status {
        QuotaStatus::NeedsAuth | QuotaStatus::RateLimited | QuotaStatus::AccessDenied => {
            theme.warning_text
        }
        QuotaStatus::Unavailable => theme.danger_text,
        _ => theme.text_tertiary,
    }
}

fn primary_window(windows: &[QuotaWindow]) -> Option<&QuotaWindow> {
    windows
        .iter()
        .filter(|window| window.kind != QuotaWindowKind::ExtraUsage)
        .max_by(|a, b| {
            a.used_percent
                .unwrap_or(-1.0)
                .total_cmp(&b.used_percent.unwrap_or(-1.0))
        })
        .or_else(|| windows.first())
}

fn format_balance(balance: &QuotaBalance) -> String {
    if balance.unlimited {
        return tr!("providers.quota_unlimited");
    }
    let unit = balance.unit.trim();
    if unit.eq_ignore_ascii_case("USD") || unit == "$" {
        format!("${}", balance.remaining)
    } else if unit.eq_ignore_ascii_case("CNY") || unit.eq_ignore_ascii_case("RMB") || unit == "¥" {
        format!("¥{}", balance.remaining)
    } else if unit.is_empty() || unit == "credits" {
        balance.remaining.clone()
    } else {
        format!("{} {unit}", balance.remaining)
    }
}

fn format_reset(resets_at: Option<i64>) -> Option<String> {
    let reset = resets_at?;
    let at = DateTime::from_timestamp(reset, 0)?;
    let local = at.with_timezone(&Local);
    let formatted = if local.year() == Local::now().year() {
        local.format("%m-%d %H:%M").to_string()
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    };
    Some(tr!("providers.quota_resets", time = formatted))
}

fn compact_status(status: &QuotaStatus) -> String {
    tr!(match status {
        QuotaStatus::Ready => "providers.quota_no_data",
        QuotaStatus::Unsupported => "providers.quota_unsupported",
        QuotaStatus::NoCredential => "providers.quota_no_credential",
        QuotaStatus::NeedsAuth => "providers.quota_needs_auth",
        QuotaStatus::AccessDenied => "providers.quota_access_denied",
        QuotaStatus::RateLimited => "providers.quota_rate_limited",
        QuotaStatus::Unavailable => "providers.quota_unavailable",
        QuotaStatus::NoData => "providers.quota_no_data",
    })
}

fn card_tooltip(name: &str, reading: Option<&ProviderQuota>) -> String {
    let mut lines = Vec::new();
    let mut header = name.to_owned();
    if let Some(plan) = reading
        .and_then(|q| q.plan.as_deref())
        .filter(|p| !p.is_empty())
    {
        header.push_str(&format!(" · {plan}"));
    }
    lines.push(header);
    if let Some(account) = reading
        .and_then(|q| q.account.as_deref())
        .filter(|a| !a.is_empty())
    {
        lines.push(account.to_owned());
    }
    if let Some(quota) = reading {
        for balance in &quota.balances {
            let label = if balance.unit == "credits" {
                tr!("providers.quota_credits")
            } else {
                tr!("providers.quota_balance")
            };
            lines.push(format!("{label}: {}", format_balance(balance)));
        }
        for window in &quota.windows {
            let value = if window.kind == QuotaWindowKind::ExtraUsage {
                tr!("providers.quota_in_use")
            } else if window.unlimited {
                tr!("providers.quota_unlimited")
            } else if let Some(used) = window.used_percent {
                tr!(
                    "providers.quota_remaining",
                    percent = format!("{:.1}", (100.0 - used).clamp(0.0, 100.0))
                )
            } else {
                "—".into()
            };
            let title = window_label(window);
            let mut line = format!("{title}: {value}");
            if let Some(reset) = format_reset(window.resets_at) {
                line.push_str(&format!(" ({reset})"));
            }
            lines.push(line);
        }
        if quota.status != QuotaStatus::Ready {
            lines.push(compact_status(&quota.status));
        }
    }
    lines.join("\n")
}

fn window_label(window: &QuotaWindow) -> String {
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
        QuotaWindowKind::ExtraUsage => tr!("providers.quota_extra_usage"),
    };
    if label.is_empty() {
        tr!("providers.quota_window")
    } else {
        label
    }
}

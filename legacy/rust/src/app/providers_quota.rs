//! Subscription and balance board on the usage page. Rendering reads stored
//! snapshots only; every query runs on the background executor with its own
//! generation guard, so a slow vendor never blocks the others.

use std::collections::BTreeMap;

use chrono::Datelike as _;
use fintwind_protocol::provider_quota::{
    ProviderQuota, QuotaBalance, QuotaStatus, QuotaWindow, QuotaWindowKind, is_local_quota_source,
    supports_provider_quota,
};

use super::*;
use ely_gpui_component::{
    buttons::IconButton,
    data_display::Badge,
    feedback::InlineMessage,
    primitives::{IconName, Severity},
    theme::ControlSize,
    typography::{Caption, Label, Title, tabular},
};

/// Board display order: local agents first, then coding plans, then
/// pay-as-you-go balances.
const SUBSCRIPTION_ORDER: &[&str] = &[
    "anthropic",
    "openai",
    "github-copilot",
    "google",
    "opencode",
    "zhipuai-coding-plan",
    "zai-coding-plan",
    "kimi-code-plan-cn",
    "kimi-code-plan-global",
    "minimax-coding-plan",
    "minimax-cn-coding-plan",
    "commandcode",
    "ollama-cloud",
    "zhipuai",
    "zai",
    "minimax",
    "minimax-cn",
    "openrouter",
    "siliconflow",
    "siliconflow-cn",
    "moonshotai",
    "moonshotai-cn",
    "deepseek",
    "stepfun",
    "stepfun-ai",
    "stepfun-ai-step-plan",
    "stepfun-step-plan",
];

#[derive(Default)]
pub(super) struct SubscriptionEntry {
    generation: u64,
    loading: bool,
    reading: Option<ProviderQuota>,
}

#[derive(Default)]
pub(super) struct SubscriptionBoard {
    generation: u64,
    refresh: Option<bool>,
    connection_error: bool,
    workspace: Option<(std::path::PathBuf, std::path::PathBuf)>,
    entries: BTreeMap<String, SubscriptionEntry>,
}

impl Fintwind {
    /// Drop in-flight subscription reads; the next load repopulates.
    pub(super) fn invalidate_subscriptions(&mut self) {
        self.provider_quota.generation = self.provider_quota.generation.wrapping_add(1);
        self.provider_quota.entries.clear();
        self.provider_quota.workspace = None;
    }

    fn sync_subscription_workspace(&mut self) {
        let workspace = self.provider_workspace();
        if self.provider_quota.workspace != workspace {
            self.invalidate_subscriptions();
            self.provider_quota.workspace = workspace;
        }
    }

    /// OpenCode's connected integrations own visibility, including for local
    /// Claude/Codex readers. Expired connections remain visible as such.
    fn subscription_visible(&self, provider: &str) -> bool {
        supports_provider_quota(provider) && self.provider_is_authorized(provider)
    }

    /// Resolve connections before reading quotas. A request made while that
    /// fetch is in flight joins it without losing a manual refresh.
    pub(super) fn load_subscriptions(&mut self, force: bool, cx: &mut Context<Self>) {
        self.sync_subscription_workspace();
        self.provider_quota.connection_error = false;
        self.provider_quota.refresh = Some(force || self.provider_quota.refresh.unwrap_or(false));
        self.ensure_builtin_providers(true, cx);
    }

    /// Only a successful connection snapshot can repopulate the board.
    pub(super) fn subscriptions_connections_updated(&mut self, cx: &mut Context<Self>) {
        self.provider_quota.connection_error = false;
        let force = self.provider_quota.refresh.take().unwrap_or(false);
        self.sync_subscription_workspace();
        self.provider_quota.generation = self.provider_quota.generation.wrapping_add(1);
        let visible: Vec<_> = SUBSCRIPTION_ORDER
            .iter()
            .copied()
            .filter(|id| self.subscription_visible(id))
            .collect();
        self.provider_quota
            .entries
            .retain(|id, _| visible.contains(&id.as_str()));
        for entry in self.provider_quota.entries.values_mut() {
            entry.loading = false;
        }
        if self.mode != WorkspaceMode::Usage
            || self.usage_tab != usage_page::UsageTab::Subscriptions
        {
            return;
        }
        for provider in visible {
            self.load_one_subscription(provider, force, cx);
        }
        cx.notify();
    }

    pub(super) fn subscriptions_connections_failed(&mut self) {
        self.provider_quota.refresh = None;
        self.provider_quota.connection_error = true;
    }

    pub(super) fn subscriptions_loading(&self) -> bool {
        self.provider_quota.refresh.is_some()
            || self
                .provider_quota
                .entries
                .values()
                .any(|entry| entry.loading)
    }

    pub(super) fn subscriptions_connection_error(&self) -> bool {
        self.provider_quota.connection_error
    }

    pub(super) fn load_one_subscription(
        &mut self,
        provider: &str,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        self.sync_subscription_workspace();
        if !self.subscription_visible(provider) {
            self.provider_quota.entries.remove(provider);
            return;
        }
        let needs_auth = self.provider_needs_auth(provider);
        let provider = provider.to_owned();
        let board_generation = self.provider_quota.generation;
        let entry = self
            .provider_quota
            .entries
            .entry(provider.clone())
            .or_default();
        entry.generation = entry.generation.wrapping_add(1);
        if needs_auth {
            entry.loading = false;
            entry.reading = Some(ProviderQuota::empty(QuotaStatus::NeedsAuth));
            cx.notify();
            return;
        }
        let entry_generation = entry.generation;
        entry.loading = true;
        // Local agents read the desktop machine's own sign-in in this
        // process, never through the daemon: a remote daemon must not stand
        // in for the desktop. Server providers resolve the workspace's
        // OpenCode server through the daemon.
        let workspace = self.provider_workspace();
        let requested_workspace = workspace.clone();
        let daemon = self.daemon.clone();
        let local_cache = self.local_quota_cache.clone();
        cx.notify();
        let provider_for_fetch = provider.clone();
        cx.spawn(async move |this, cx| {
            let reading = cx
                .background_executor()
                .spawn(async move {
                    if is_local_quota_source(&provider_for_fetch) {
                        local_cache.fetch(&provider_for_fetch, force)
                    } else {
                        match workspace {
                            Some((binary, directory)) => StateStore::remote(daemon)
                                .fetch_provider_quota(binary, directory, provider_for_fetch, force)
                                .unwrap_or_else(|_| ProviderQuota::empty(QuotaStatus::Unavailable)),
                            None => ProviderQuota::empty(QuotaStatus::Unavailable),
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.provider_quota.generation != board_generation {
                    return;
                }
                if this.provider_workspace() != requested_workspace {
                    this.invalidate_subscriptions();
                    if this.mode == WorkspaceMode::Usage
                        && this.usage_tab == usage_page::UsageTab::Subscriptions
                    {
                        this.load_subscriptions(false, cx);
                    }
                    cx.notify();
                    return;
                }
                if !this.subscription_visible(&provider) {
                    this.provider_quota.entries.remove(&provider);
                    cx.notify();
                    return;
                }
                let Some(entry) = this.provider_quota.entries.get_mut(&provider) else {
                    return;
                };
                if entry.generation != entry_generation {
                    return;
                }
                entry.loading = false;
                // A connected service must not silently disappear just
                // because its quota credentials or endpoint are unavailable.
                let mut reading = reading;
                if matches!(
                    reading.status,
                    QuotaStatus::Unavailable | QuotaStatus::RateLimited
                ) && reading.windows.is_empty()
                    && reading.balances.is_empty()
                    && let Some(mut cached) = entry.reading.take()
                    && (!cached.windows.is_empty() || !cached.balances.is_empty())
                {
                    cached.status = reading.status;
                    cached.retry_at = reading.retry_at;
                    reading = cached;
                }
                entry.reading = Some(reading);
                cx.notify();
            });
        })
        .detach();
    }

    /// Connected services in stable display order, with their stored snapshots.
    pub(super) fn subscription_cards(&self) -> Vec<(String, bool, Option<ProviderQuota>)> {
        SUBSCRIPTION_ORDER
            .iter()
            .filter(|id| self.subscription_visible(id))
            .filter_map(|id| {
                self.provider_quota
                    .entries
                    .get(*id)
                    .map(|entry| ((*id).to_owned(), entry.loading, entry.reading.clone()))
            })
            .collect()
    }

    pub(super) fn subscription_display_name(&self, provider: &str) -> String {
        if let Some(service) = self
            .provider_quota
            .entries
            .get(provider)
            .and_then(|entry| entry.reading.as_ref())
            .and_then(|reading| reading.service.clone())
            .filter(|service| !service.is_empty())
        {
            return service;
        }
        self.builtin_provider(provider)
            .and_then(|record| {
                if record.name.is_empty() {
                    None
                } else {
                    Some(record.name.clone())
                }
            })
            .unwrap_or_else(|| provider.to_owned())
    }

    /// One subscription card: service, account, plan (only when the vendor
    /// told one), windows with resets, balances, and an explicit state line
    /// whenever the reading is not fresh. Numbers are tabular; status never
    /// rides on color alone.
    pub(super) fn render_subscription_card(
        &self,
        provider: &str,
        loading: bool,
        reading: Option<&ProviderQuota>,
        slots: (usize, usize),
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = self.subscription_display_name(provider);
        let refresh = self.subscription_refresh_button(provider, loading, cx);
        let (balance_only, _) = subscription_layout(provider, reading);
        let has_data = reading.is_some_and(|q| !q.windows.is_empty() || !q.balances.is_empty());
        let account = reading
            .and_then(|q| q.account.as_ref())
            .filter(|s| !s.is_empty());
        let plan = reading
            .and_then(|q| q.plan.as_ref())
            .filter(|s| !s.is_empty());
        let identity = div()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w_0()
                    .child(crate::ui::model_mark_element(
                        crate::ui::provider_mark(provider, theme.is_dark),
                        22.0,
                        theme.text,
                    ))
                    .child(Title::new(title).flex_1().min_w_0().truncate())
                    .child(refresh),
            )
            .when(!balance_only || account.is_some(), |identity| {
                identity.child(
                    div()
                        .h_5()
                        .children(account.map(|account| Caption::new(account.clone()).truncate())),
                )
            })
            .when(!balance_only || plan.is_some(), |identity| {
                identity.child(
                    div()
                        .h_6()
                        .flex()
                        .items_start()
                        .children(plan.map(|plan| Badge::new(plan.clone()))),
                )
            });
        // Only window-based peers reserve matching meter slots. Balance-only
        // peers size to their real data, without empty identity or meter lanes.
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .when(balance_only, |body| body.min_h(gpui::rems(4.5)))
            .when(!balance_only, |body| {
                body.flex_1()
                    .min_h(gpui::rems(slots.0 as f32 * 4.5 + slots.1 as f32 * 2.0))
            });
        let mut status = div().flex_1().min_w_0();
        if loading {
            status = status.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(crate::ui::motion::spin_slow(crate::ui::icon(
                        "icons/loader-circle.svg",
                        12.0,
                        theme.text_tertiary,
                    )))
                    .child(Caption::new(tr!("providers.quota_loading"))),
            );
        } else if let Some(state) = subscription_state_line(
            reading.map(|q| &q.status).unwrap_or(&QuotaStatus::NoData),
            theme,
        ) {
            status = status.child(state);
        }
        let mut footer = div()
            .min_h(gpui::rems(1.75))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .child(status);
        if loading && !has_data {
            body = body.child(subscription_loading_body(balance_only, slots, *theme));
        }
        if let Some(q) = reading {
            for balance in &q.balances {
                body = body.child(quota_balance_row(balance, balance_only));
            }
            for window in &q.windows {
                body = body.child(quota_window(window, theme));
            }
            if let Some(at) = q.measured_at.and_then(|at| DateTime::from_timestamp(at, 0)) {
                let time = quota_time(at.with_timezone(&Local));
                let measured = if q.status != QuotaStatus::Ready {
                    tr!("providers.quota_cached_measured", time = time)
                } else {
                    tr!("providers.quota_measured", time = time)
                };
                footer = footer.child(tabular(Caption::new(measured)).flex_none().text_right());
            }
        }
        super::usage_page::usage_card(theme)
            .id(SharedString::from(format!("subscription-card-{provider}")))
            .h_full()
            .p_5()
            .gap_4()
            .child(identity)
            .child(body)
            .child(footer)
            .into_any_element()
    }

    fn subscription_refresh_button(
        &self,
        provider: &str,
        loading: bool,
        cx: &mut Context<Self>,
    ) -> IconButton {
        let provider_key = provider.to_owned();
        IconButton::new(
            SharedString::from(format!("refresh-subscription-{provider}")),
            IconName::RefreshCw,
        )
        .size(ControlSize::Sm)
        .disabled(loading)
        .tooltip(format!(
            "{} · {}",
            self.subscription_display_name(provider),
            tr!("usage_page.refresh")
        ))
        .on_click(cx.listener(move |this, _, _, cx| {
            if !this
                .provider_quota
                .entries
                .get(&provider_key)
                .is_some_and(|entry| entry.loading)
            {
                this.load_one_subscription(&provider_key, true, cx);
            }
        }))
    }
}

/// Placeholder geometry only. Connection eligibility still comes exclusively
/// from OpenCode, and real readings always determine the displayed values.
pub(super) fn subscription_layout(
    provider: &str,
    reading: Option<&ProviderQuota>,
) -> (bool, usize) {
    let balance_only =
        if let Some(q) = reading.filter(|q| !q.windows.is_empty() || !q.balances.is_empty()) {
            q.windows.is_empty()
        } else {
            matches!(
                provider,
                "openrouter"
                    | "siliconflow"
                    | "siliconflow-cn"
                    | "moonshotai"
                    | "moonshotai-cn"
                    | "zhipuai"
                    | "zai"
                    | "deepseek"
                    | "stepfun"
                    | "stepfun-ai"
                    | "stepfun-ai-step-plan"
                    | "stepfun-step-plan"
            )
        };
    let windows = if balance_only {
        0
    } else {
        let baseline = if matches!(provider, "opencode" | "opencode-go" | "github-copilot") {
            3
        } else {
            2
        };
        reading.map_or(baseline, |q| q.windows.len().max(baseline))
    };
    (balance_only, windows)
}

fn subscription_loading_body(
    balance_only: bool,
    slots: (usize, usize),
    theme: Theme,
) -> AnyElement {
    // Ely Skeleton repeats at display rate. Keep these lightweight placeholders
    // on the product clock instead; opacity never changes their layout.
    crate::ui::motion::pulse(Duration::from_millis(1600), move |phase| {
        let block = || div().rounded(px(3.0)).bg(theme.overlay_strong);
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .opacity(0.78 + 0.18 * (std::f32::consts::TAU * phase).cos());
        if balance_only {
            body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(block().w(gpui::rems(5.0)).h_4())
                    .child(block().w(gpui::rems(9.0)).h(gpui::rems(2.5))),
            )
            .into_any_element()
        } else {
            body.children((0..slots.0).map(|_| {
                div()
                    .min_h(gpui::rems(3.75))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(block().w(gpui::rems(4.0)).h_4())
                            .child(block().w(gpui::rems(5.0)).h_4()),
                    )
                    .child(block().w_full().h(px(4.0)))
                    .child(block().w(gpui::rems(7.0)).h_3())
            }))
            .into_any_element()
        }
    })
    .every(3)
    .into_any_element()
}

pub(super) fn subscription_loading_card(balance_only: bool, theme: &Theme) -> AnyElement {
    let block = || div().rounded(px(3.0)).bg(theme.overlay_strong);
    let identity = div()
        .flex_none()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .h_7()
                .flex()
                .items_center()
                .gap_3()
                .child(block().size(px(22.0)))
                .child(block().w(gpui::rems(10.0)).h_5()),
        )
        .when(!balance_only, |identity| {
            identity.child(div().h_5()).child(div().h_6())
        });
    super::usage_page::usage_card(theme)
        .h_full()
        .p_5()
        .gap_4()
        .child(identity)
        .child(
            div()
                .flex_1()
                .min_h(gpui::rems(if balance_only { 4.5 } else { 13.5 }))
                .child(subscription_loading_body(balance_only, (3, 0), *theme)),
        )
        .child(
            div()
                .h_7()
                .flex()
                .justify_end()
                .items_center()
                .child(block().w(gpui::rems(8.0)).h_3()),
        )
        .into_any_element()
}

/// A state line with an icon and words: status is never color-only.
fn subscription_status_text(status: &QuotaStatus) -> String {
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

fn subscription_state_line(status: &QuotaStatus, _theme: &Theme) -> Option<AnyElement> {
    if *status == QuotaStatus::Ready {
        return None;
    }
    Some(InlineMessage::new(Severity::Warning, subscription_status_text(status)).into_any_element())
}

fn quota_balance_row(balance: &QuotaBalance, prominent: bool) -> AnyElement {
    let label = if balance.unit == "credits" {
        tr!("providers.quota_credits")
    } else {
        tr!("providers.quota_balance")
    };
    let value = if balance.unlimited {
        tr!("providers.quota_unlimited")
    } else if balance.unit.is_empty() || balance.unit == "credits" || prominent {
        balance.remaining.clone()
    } else {
        format!("{} {}", balance.remaining, balance.unit)
    };
    div()
        .flex()
        .items_baseline()
        .justify_between()
        .gap_3()
        .when(prominent, |row| row.flex_col().items_start().gap_2())
        .child(Caption::new(label))
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .child(tabular(Label::new(value)).when(prominent, |value| {
                    value.text_3xl().font_weight(FontWeight::SEMIBOLD)
                }))
                .when(
                    prominent
                        && !balance.unlimited
                        && !balance.unit.is_empty()
                        && balance.unit != "credits",
                    |row| row.child(Caption::new(balance.unit.clone())),
                ),
        )
        .into_any_element()
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
        QuotaWindowKind::ExtraUsage => tr!("providers.quota_extra_usage"),
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
    let mut row = div()
        .min_h(gpui::rems(3.75))
        .flex_none()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap_3()
                .child(Caption::new(label).flex_1().min_w_0())
                .child(tabular(Label::new(value)).flex_none().when(
                    window.used_percent.is_some_and(|used| used >= 90.0) && !window.unlimited,
                    |label| label.text_color(theme.warning_text),
                )),
        );
    if let Some(used) = window.used_percent
        && used.is_finite()
        && !window.unlimited
        && window.kind != QuotaWindowKind::ExtraUsage
    {
        // Ely's fixed ProgressBar paints with foreground ink and has no
        // color override. This read-only meter uses the product accent
        // without changing the theme of the surrounding content.
        let remaining = ((100.0 - used).clamp(0.0, 100.0) / 100.0) as f32;
        row = row.child(
            div()
                .w_full()
                .h(px(4.0))
                .rounded(px(2.0))
                .overflow_hidden()
                .bg(theme.accent.opacity(0.15))
                .child(div().h_full().w(gpui::relative(remaining)).bg(theme.accent)),
        );
    }
    if let Some(reset) = window.resets_at
        && let Some(at) = DateTime::from_timestamp(reset, 0)
    {
        row = row.child(Caption::new(tr!(
            "providers.quota_resets",
            time = quota_time(at.with_timezone(&Local))
        )));
    }
    row.into_any_element()
}

/// Quota timestamps stay short inside their year and unambiguous outside
/// it, so a January reset stamped last December never reads as upcoming.
fn quota_time(at: chrono::DateTime<Local>) -> String {
    if at.year() == Local::now().year() {
        at.format("%m-%d %H:%M").to_string()
    } else {
        at.format("%Y-%m-%d %H:%M").to_string()
    }
}

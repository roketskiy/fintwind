//! The Usage page: one daemon-side scan over the workspace's OpenCode
//! sessions feeds token-usage statistics — KPI cards, a GitHub-style
//! activity heatmap, daily/hourly timelines, and per-model, per-provider, and
//! per-project rankings. Frames read only the aggregated views stored on the
//! entity; the raw scan is touched only when it lands or the range changes.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Datelike as _, Duration as ChronoDuration, Local, NaiveDate, Timelike as _};

#[cfg(test)]
use chrono::TimeZone as _;

use crate::usage::{cache_hit_percent, format_tokens};
use crate::ui::loading::{LoadingDelay, LoadingPhase};
use crate::ui::ActivationExt;
use fintwind_client::models_dev::{self, ModelsDevTable};

use super::*;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, SegmentedControl},
    feedback::{Alert, EmptyState},
    primitives::{Icon, IconName, Severity},
    theme::{ControlSize, IconSize},
    typography::tabular,
};
use fintwind_client::provider_session::{UsageDayShare, UsageEntry, UsageStats};
mod channels_card;
mod quota_card;
mod trend_chart;

#[cfg(test)]
use fintwind_client::provider_session::UsageModelLane;

/// Heatmap horizon, in whole weeks ending today.
pub(super) const HEATMAP_WEEKS: i64 = 26;
/// Weekly retention-style matrix horizon, in whole weeks ending this week.
pub(super) const MATRIX_WEEKS: i64 = 6;
/// Timeline horizon for the unbounded range.
const BAR_HORIZON_DAYS: i64 = 30;
const DAY_WINDOW: i64 = 30;
/// Chart scrubbing restarts the tooltip timer on every bar, so the framework
/// default of 500ms feels stuck. Zero still waits one timer tick.
pub(super) const CHART_TOOLTIP_DELAY: Duration = Duration::from_millis(0);
/// A stored scan older than this is refreshed silently on page open; within
/// it, reopening the page costs no server traversal.
const STALE_AFTER: Duration = Duration::from_secs(60);

/// What the KPI cards, timelines, and model ranking aggregate over. The
/// heatmap keeps its own fixed week window, like GitHub's, so a short range
/// does not gut it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum UsageRange {
    All,
    Days30,
    #[default]
    Days7,
    Day(u8),
}

/// Which metric the main trend chart plots and which KPI card highlights.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum UsageChartMetric {
    #[default]
    Total,
    Generated,
    CacheHit,
}

impl UsageChartMetric {
    pub(super) fn palette_index(self) -> usize {
        match self {
            Self::Total => 0,
            Self::Generated => 1,
            Self::CacheHit => 2,
        }
    }

    pub(super) fn title(self) -> String {
        match self {
            Self::Total => tr!("usage_page.kpi_total_tokens"),
            Self::Generated => tr!("usage_page.kpi_generated_tokens"),
            Self::CacheHit => tr!("usage_page.kpi_cache_hit"),
        }
    }

    pub(super) fn daily_value(self, day: &UsageDay) -> f64 {
        match self {
            Self::Total => day.total as f64,
            Self::Generated => day.output as f64,
            Self::CacheHit => cache_hit_percent(
                day.cache_read,
                day.input
                    .saturating_add(day.cache_read)
                    .saturating_add(day.cache_write),
            )
            .unwrap_or(0.0),
        }
    }

    pub(super) fn hourly_value(self, hour: &UsageHour) -> f64 {
        match self {
            Self::Total => hour
                .input
                .saturating_add(hour.output)
                .saturating_add(hour.cache_read)
                .saturating_add(hour.cache_write) as f64,
            Self::Generated => hour.output as f64,
            Self::CacheHit => cache_hit_percent(
                hour.cache_read,
                hour.input
                    .saturating_add(hour.cache_read)
                    .saturating_add(hour.cache_write),
            )
            .unwrap_or(0.0),
        }
    }

    pub(super) fn format_total(self, totals: &UsageTotals) -> String {
        match self {
            Self::Total => format_tokens(totals.total()),
            Self::Generated => format_tokens(totals.generated()),
            Self::CacheHit => cache_hit_percent(
                totals.cache_read,
                totals
                    .input
                    .saturating_add(totals.cache_read)
                    .saturating_add(totals.cache_write),
            )
            .map(|v| format!("{v:.1}%"))
            .unwrap_or_else(|| "—".into()),
        }
    }

    pub(super) fn format_axis(self, val: f64) -> String {
        match self {
            Self::Total | Self::Generated => format_tokens(val.round().max(0.0) as u64),
            Self::CacheHit => format!("{val:.0}%"),
        }
    }

    pub(super) fn format_point(self, val: f64) -> String {
        match self {
            Self::Total | Self::Generated => format_tokens(val.round().max(0.0) as u64),
            Self::CacheHit => format!("{val:.1}%"),
        }
    }
}

/// Which breakdown the bottom-left channel bars card shows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum UsageChannelDimension {
    #[default]
    Models,
    Providers,
    Projects,
    Composition,
}

impl UsageChannelDimension {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Models => "models",
            Self::Providers => "providers",
            Self::Projects => "projects",
            Self::Composition => "composition",
        }
    }

    pub(super) fn from_key(key: &str) -> Self {
        match key {
            "providers" => Self::Providers,
            "projects" => Self::Projects,
            "composition" => Self::Composition,
            _ => Self::Models,
        }
    }
}

/// Which view the bottom-right activity card shows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum UsageActivityView {
    #[default]
    WeeklyMatrix,
    CalendarHeatmap,
}

impl UsageActivityView {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::WeeklyMatrix => "matrix",
            Self::CalendarHeatmap => "heatmap",
        }
    }

    pub(super) fn from_key(key: &str) -> Self {
        match key {
            "heatmap" => Self::CalendarHeatmap,
            _ => Self::WeeklyMatrix,
        }
    }
}

/// Selection is scoped to the currently aggregated snapshot. It is cleared
/// whenever a replacement snapshot or time range lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UsageFlowSelection {
    Day(usize),
    Hour(usize),
}

impl UsageRange {
    pub(super) const ALL: [Self; 4] = [Self::Day(0), Self::Days7, Self::Days30, Self::All];

    fn label(self) -> String {
        tr!(match self {
            Self::All => "usage_page.range_all",
            Self::Days30 => "usage_page.range_30d",
            Self::Days7 => "usage_page.range_7d",
            Self::Day(_) => "usage_page.range_day",
        })
    }

    /// The segment value the segmented control holds; every day offset is the
    /// same segment.
    fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Days30 => "30d",
            Self::Days7 => "7d",
            Self::Day(_) => "day",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "30d" => Self::Days30,
            "7d" => Self::Days7,
            "day" => Self::Day(0),
            _ => Self::All,
        }
    }

    fn days(self) -> Option<i64> {
        match self {
            Self::All => None,
            Self::Days30 => Some(30),
            Self::Days7 => Some(7),
            Self::Day(_) => Some(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct UsageTotals {
    pub sessions: u64,
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub active_days: u32,
    /// Sum of the server's cost estimates; sessions without one add nothing.
    pub cost: f64,
    /// How many sessions reported a cost at all, for the KPI subtitle.
    pub costed_sessions: u32,
    /// Sub-agent sessions folded into `sessions`, and the tokens they
    /// contributed. A sub-agent is its own session whose parent's aggregate
    /// excludes it, so without this the totals would drop real spend.
    pub subagent_sessions: u64,
    pub subagent_tokens: u64,
}

impl UsageTotals {
    fn add(&mut self, entry: &UsageEntry) {
        self.sessions += 1;
        self.input = self.input.saturating_add(entry.input_tokens);
        self.output = self.output.saturating_add(entry.output_tokens);
        self.reasoning = self.reasoning.saturating_add(entry.reasoning_tokens);
        self.cache_read = self.cache_read.saturating_add(entry.cache_read_tokens);
        self.cache_write = self.cache_write.saturating_add(entry.cache_write_tokens);
        if let Some(cost) = entry.cost {
            self.cost += cost;
            self.costed_sessions += 1;
        }
        self.subagent_sessions += u64::from(entry.subagent_sessions);
        self.subagent_tokens = self.subagent_tokens.saturating_add(entry.subagent_tokens);
    }

    fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.reasoning)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// Tokens the provider generated, reasoning included — the volume the
    /// model actually produced rather than the traffic cache hits inflate.
    fn generated(&self) -> u64 {
        self.output.saturating_add(self.reasoning)
    }

    fn direct(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.reasoning)
    }
}

#[derive(Clone, Debug)]
pub(super) struct UsageDay {
    pub date: NaiveDate,
    /// All lanes, for the tooltip's honest total.
    pub total: u64,
    /// Input + output + reasoning, for heatmap intensity.
    pub direct: u64,
    pub sessions: u32,
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub cost: f64,
}

#[derive(Clone, Debug, Default)]
pub(super) struct UsageHour {
    pub hour: u32,
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub cost: f64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct UsageHeatCell {
    pub date: NaiveDate,
    #[cfg(test)]
    pub total: u64,
    pub direct: u64,
    #[cfg(test)]
    pub sessions: u32,
    /// 0 (quiet) through 4 (heaviest), keyed to the window's busiest day.
    pub level: u8,
}

#[derive(Clone, Debug)]
pub(super) struct UsageModelRow {
    /// `<providerID>/<modelID>` as the server recorded it.
    pub model: Option<String>,
    pub sessions: u32,
    pub total: u64,
    pub cost: f64,
    pub last_used: u64,
}

/// One project directory's share of the store, for the per-project ranking.
#[derive(Clone, Debug)]
pub(super) struct UsageProjectRow {
    pub directory: String,
    pub sessions: u32,
    pub total: u64,
    pub cost: f64,
}

/// One provider's share of the store, for the provider ranking. Derived from
/// the model strings the scan already carries — `providerID/modelID` — so it
/// costs no extra request and no extra protocol field. Sessions are deliberately
/// absent: a session that switched providers belongs to both, and a session
/// count that does not add up reads as a bug rather than as attribution.
#[derive(Clone, Debug)]
pub(super) struct UsageProviderRow {
    pub provider: String,
    pub models: u32,
    pub total: u64,
    pub cost: f64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct UsageWeekMatrixCell {
    pub date: NaiveDate,
    pub direct: u64,
    pub total: u64,
    pub sessions: u32,
}

#[derive(Clone, Debug)]
pub(super) struct UsageWeekMatrixRow {
    pub label: String,
    pub direct_total: u64,
    pub cells: [Option<UsageWeekMatrixCell>; 7],
}

#[derive(Clone, Debug, Default)]
pub(super) struct UsageViews {
    pub totals: UsageTotals,
    pub prev_totals: Option<UsageTotals>,
    /// Contiguous days ending today (or the selected day), oldest first.
    pub daily: Vec<UsageDay>,
    pub prev_daily: Vec<UsageDay>,
    pub hourly: Vec<UsageHour>,
    pub prev_hourly: Vec<UsageHour>,
    pub hourly_estimated: bool,
    /// Week columns of seven cells, Monday first; `None` past today.
    pub weeks: Vec<Vec<Option<UsageHeatCell>>>,
    pub heatmap_active_days: u32,
    pub weekly_matrix: Vec<UsageWeekMatrixRow>,
    pub matrix_active_days: u32,
    pub models: Vec<UsageModelRow>,
    pub model_count: usize,
    pub providers: Vec<UsageProviderRow>,
    pub provider_count: usize,
    pub projects: Vec<UsageProjectRow>,
    pub project_count: usize,
}

impl Fintwind {
    /// Price-network latency must never gate token snapshots. Stale prices
    /// remain usable while this independent task refreshes the catalog.
    fn refresh_usage_pricing(&mut self, cx: &mut Context<Self>) {
        let retry_after = if self
            .usage_price_table
            .as_ref()
            .is_some_and(|table| table.age() < PRICE_TTL)
        {
            PRICE_TTL
        } else {
            Duration::from_secs(60)
        };
        if self.usage_price_pending
            || self
                .usage_price_checked_at
                .is_some_and(|at| at.elapsed() < retry_after)
        {
            return;
        }
        self.usage_price_pending = true;
        let cached = self.usage_price_table.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let local = pricing_table(cached);
                    if local.as_ref().is_some_and(|table| table.age() < PRICE_TTL) {
                        return local;
                    }
                    // Prices only live in legacy caches; the bundled
                    // snapshot omits them, so no download is worth making.
                    local
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.usage_price_pending = false;
                // Failed downloads retry later rather than on every poll.
                this.usage_price_checked_at = Some(Instant::now());
                if let Some(table) = result {
                    this.usage_price_table = Some(table);
                    this.usage_stats_loaded_at = None;
                    if this.mode == WorkspaceMode::Usage {
                        this.ensure_usage_stats(false, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Load the usage scan if none is stored or the stored one went stale.
    /// Snapshots cover the whole source rather than the selected project.
    /// Provider reconciliation belongs to the daemon; this path only reads
    /// its snapshot and prices it off the UI thread.
    ///
    /// A scan that is already in flight is never started twice, and a stale
    /// scan is never discarded while it is being replaced: the page keeps
    /// drawing the previous numbers, so a refresh reads as the page updating
    /// rather than emptying.
    pub(super) fn ensure_usage_stats(&mut self, force: bool, cx: &mut Context<Self>) {
        self.refresh_usage_pricing(cx);
        // The directory only anchors which resident server the daemon asks;
        // the scan itself is global.
        let Some(binary) = self.native_binary_path() else {
            return;
        };
        if self.usage_stats_source.as_ref() != Some(&binary) {
            self.usage_stats_source = Some(binary.clone());
            self.usage_stats = None;
            self.usage_stats_complete = false;
            self.usage_stats_loaded_at = None;
            self.usage_stats_pending = false;
            self.usage_stats_generation += 1;
            self.rebuild_usage_views(cx);
        }
        if self.usage_stats_pending {
            return;
        }
        let Some(project_id) = self.state.selected_project else {
            return;
        };
        let Some(directory) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
        else {
            return;
        };
        if !force
            && !self
                .usage_stats
                .as_ref()
                .is_some_and(|stats| stats.refreshing)
            && self
                .usage_stats_loaded_at
                .is_some_and(|loaded_at| loaded_at.elapsed() < STALE_AFTER)
        {
            return;
        }
        self.usage_stats_pending = true;
        self.usage_stats_error = None;
        self.usage_stats_generation += 1;
        let generation = self.usage_stats_generation;
        let daemon = self.daemon.clone();
        let detailed_day = None;
        let cached_table = self.usage_price_table.clone();
        cx.spawn(async move |this, cx| {
            let (scanned, table) =
                {
                    let executor = cx.background_executor().clone();
                    let scan =
                        executor.spawn(async move {
                            fintwind_client::persistence::StateStore::remote(daemon)
                                .fetch_usage_stats(binary, directory, detailed_day, force)
                        });
                    // Local prices only. Network refresh is independent and
                    // cannot delay token availability.
                    let pricing = executor.spawn(async move { pricing_table(cached_table) });
                    (scan.await, pricing.await)
                };
            let used_table = table.clone();
            let priced = cx
                .background_executor()
                .spawn(async move {
                    let mut priced = scanned;
                    if let Ok(stats) = &mut priced {
                        price_usage_stats(stats, used_table.as_deref());
                    }
                    priced
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A newer scan superseded this one.
                if this.usage_stats_generation != generation {
                    return;
                }
                this.usage_stats_pending = false;
                match priced {
                    Ok(stats) => {
                        // The table and the stats land together, so the
                        // cost KPI's claim of "priced" is always true of
                        // the numbers above it.
                        let newer_prices = this.usage_price_table.as_ref().is_some_and(|current| {
                            table
                                .as_ref()
                                .is_none_or(|used| current.fetched_at > used.fetched_at)
                        });
                        if !newer_prices {
                            this.usage_price_table = table;
                        }
                        this.usage_stats_error = stats.refresh_error.clone();
                        this.usage_stats_loaded_at = Some(Instant::now());
                        if !stats.refreshing && stats.refresh_error.is_none() {
                            this.usage_stats_complete = true;
                        }
                        this.usage_stats = Some(Arc::new(stats));
                        this.rebuild_usage_views(cx);
                        if newer_prices {
                            this.usage_stats_loaded_at = None;
                            this.ensure_usage_stats(false, cx);
                        }
                    }
                    Err(error) => {
                        // Keep the old snapshot and back off before another
                        // transport request rather than retrying every poll.
                        this.usage_stats_error = Some(error.to_string());
                        this.usage_stats_loaded_at = Some(Instant::now());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Keep the page current while it is open. The loop ends on its own when
    /// the page is left or the entity goes away, so nothing has to cancel it.
    ///
    /// A loop already running is never joined by a second one: leaving and
    /// reopening the page quickly would otherwise stack several, each waking
    /// the daemon on its own schedule.
    pub(super) fn start_usage_auto_refresh(&mut self, cx: &mut Context<Self>) {
        if self.usage_refresh_running {
            return;
        }
        self.usage_refresh_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let still_open = this
                    .update(cx, |this, cx| {
                        if this.mode != WorkspaceMode::Usage {
                            // Release the guard from inside: a later visit must
                            // be able to start a fresh loop.
                            this.usage_refresh_running = false;
                            return false;
                        }
                        this.ensure_usage_stats(false, cx);
                        true
                    })
                    .unwrap_or(false);
                if !still_open {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn set_usage_range(&mut self, range: UsageRange, cx: &mut Context<Self>) {
        if self.usage_range == range {
            return;
        }
        self.usage_range = range;
        self.usage_flow_selection = None;
        self.rebuild_usage_views(cx);
    }

    fn rebuild_usage_views(&mut self, cx: &mut Context<Self>) {
        self.usage_views_generation += 1;
        let generation = self.usage_views_generation;
        let stats = self.usage_stats.clone();
        let range = self.usage_range;
        cx.spawn(async move |this, cx| {
            let views = cx
                .background_executor()
                .spawn(async move { build_views(stats.as_deref(), range) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.usage_views_generation == generation {
                    this.usage_views = views;
                    this.usage_flow_selection = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn render_usage_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let first_load = self.usage_stats.as_ref().is_none_or(|stats| {
            !self.usage_stats_complete && stats.entries.is_empty() && stats.refreshing
        });
        let source = self.native_binary_path().zip(self.state.selected_project);
        let no_workspace = source.is_none();
        let loading_phase = if let Some(source) = source.filter(|_| self.usage_stats_error.is_none())
        {
            let (phase, wake) = LoadingDelay::observe(
                &mut self.usage_loading,
                source,
                first_load
                    && (self.usage_stats_pending
                        || self.usage_stats.as_ref().is_some_and(|stats| stats.refreshing)),
            );
            if let Some(wake) = wake {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wake.delay()).await;
                    wake.fired();
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }
            phase
        } else {
            self.usage_loading = None;
            LoadingPhase::Done
        };

        let summary = (loading_phase == LoadingPhase::Done
            && self
                .usage_stats
                .as_ref()
                .is_some_and(|stats| !stats.entries.is_empty())
            && self.usage_views.totals.sessions > 0)
            .then(|| {
                let totals = &self.usage_views.totals;
                let mut text = tr!("usage_page.model_sessions", count = totals.sessions);
                if totals.active_days > 0 {
                    text.push_str(&format!(
                        " · {}",
                        tr!("usage_page.active_days_value", count = totals.active_days)
                    ));
                }
                if totals.cost >= 0.005 {
                    text.push_str(&format!(" · {}", format_cost(totals.cost)));
                }
                tabular(div())
                    .truncate()
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_secondary)
                    .child(text)
            });
        let filter = self.render_usage_history_filter(window, &theme, cx);
        let top_bar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_3()
            .child(div().flex_1().min_w_0().children(summary))
            .child(div().flex_none().child(filter));

        let mut column = div().flex().flex_col().gap_4().min_w_0().w_full().child(top_bar);

        if let Some(error) = &self.usage_stats_error {
            column = column.child(
                usage_notice(
                    &theme,
                    "icons/alert.svg",
                    theme.warning,
                    tr!("usage_page.load_failed", error = error.clone()),
                )
                .action(usage_retry_button(&theme, cx)),
            );
        } else if loading_phase != LoadingPhase::Done {
            let placeholder_opacity = if loading_phase == LoadingPhase::Visible {
                1.0
            } else {
                0.0
            };
            let cols = kpi_columns(self.usage_content_width, window);
            let bottom_cols = two_card_columns(self.usage_content_width);
            column = column
                .child(
                    div()
                        .w_full()
                        .grid()
                        .grid_cols(cols)
                        .gap_3()
                        .opacity(placeholder_opacity)
                        .children((0..4).map(|_| {
                            div()
                                .h(px(138.0))
                                .rounded(px(13.0))
                                .border_1()
                                .border_color(theme.card_border())
                                .bg(theme.inset)
                                .p_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .px_2p5()
                                        .py_1p5()
                                        .child(div().w(px(76.0)).h(px(12.0)).rounded(px(4.0)).bg(theme.overlay_strong)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .rounded(px(9.0))
                                        .bg(theme.composer)
                                        .p_3()
                                        .flex()
                                        .flex_col()
                                        .justify_between()
                                        .child(div().w(px(96.0)).h(px(24.0)).rounded(px(5.0)).bg(theme.overlay_strong))
                                        .child(div().w(px(132.0)).h(px(12.0)).rounded(px(4.0)).bg(theme.overlay_strong)),
                                )
                        })),
                )
                .child(
                    usage_card(&theme)
                        .h(px(340.0))
                        .opacity(placeholder_opacity),
                )
                .child(
                    div()
                        .w_full()
                        .grid()
                        .grid_cols(bottom_cols)
                        .gap_4()
                        .opacity(placeholder_opacity)
                        .child(usage_card(&theme).h(px(300.0)))
                        .child(usage_card(&theme).h(px(300.0))),
                );
        }

        if no_workspace {
            column = column.child(usage_empty_card(
                &theme,
                tr!("usage_page.no_workspace_title"),
                tr!("usage_page.no_workspace_body"),
            ));
        } else if loading_phase == LoadingPhase::Done
            && let Some(stats) = self.usage_stats.as_ref().filter(|stats| {
                !stats.entries.is_empty() || !stats.refreshing || self.usage_stats_complete
            })
        {
            if stats.entries.is_empty() && (!stats.refreshing || self.usage_stats_complete) {
                column = column.child(usage_empty_card(
                    &theme,
                    tr!("usage_page.empty_title"),
                    tr!("usage_page.empty_body"),
                ));
            } else {
                if stats.truncated {
                    column = column.child(usage_notice(
                        &theme,
                        "icons/info.svg",
                        theme.text_tertiary,
                        tr!(
                            "usage_page.truncated_notice",
                            sessions = stats.sessions_scanned
                        ),
                    ));
                }
                let views = &self.usage_views;
                let kpis = self.render_usage_kpis(views, window, &theme, cx);
                let trend = self.render_usage_trend_chart(views, window, &theme, cx);
                let channels = self.render_usage_channels(views, &theme, cx);
                let activity = self.render_usage_activity_card(views, &theme, cx);
                let bottom_cols = two_card_columns(self.usage_content_width);
                column = column
                    .child(kpis)
                    .child(trend)
                    .child(
                        div()
                            .w_full()
                            .grid()
                            .grid_cols(bottom_cols)
                            .items_stretch()
                            .gap_4()
                            .child(div().min_w_0().child(channels))
                            .child(div().min_w_0().child(activity)),
                    );
            }
        }

        column.into_any_element()
    }

    pub(super) fn render_usage_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let busy = self.usage_stats_pending || self.subscriptions_loading();
        let focus = self.transcript_control_focus("usage-refresh", cx);
        let refresh_icon = motion::action_icon(
            &self.usage_refresh_spin,
            busy,
            false,
            icon("icons/rotate-cw.svg", 14.0, theme.text_secondary),
            cx,
        );
        let refresh = div()
            .id("usage-refresh")
            .role(gpui::Role::Button)
            .aria_label(tr!("usage_page.refresh"))
            .track_focus(&focus)
            .tab_index(0)
            .size(px(26.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
            .hover(|style| style.bg(theme.overlay))
            .tooltip(Tooltip::text(tr!("usage_page.refresh")))
            .child(refresh_icon)
            .on_activation(cx, |this, _, cx| {
                if !this.usage_stats_pending && !this.subscriptions_loading() {
                    *this.usage_refresh_spin.get_mut() = Some(motion::ActionSpin::start());
                    this.ensure_usage_stats(true, cx);
                    this.load_subscriptions(true, cx);
                }
            });
        div()
            .flex()
            .items_center()
            .h_8()
            .gap_3()
            .child(
                div()
                    .text_size(ui_px(16.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(tr!("usage_page.statistics_title")),
            )
            .child(div().flex_1().min_w_0())
            .child(refresh)
    }

    /// Statistics-only controls; no control here changes a quota snapshot.
    fn render_usage_history_filter(
        &self,
        window: &Window,
        _theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut selector =
            SegmentedControl::new("usage-range", self.usage_range.key()).size(ControlSize::Sm);
        for range in UsageRange::ALL {
            selector = selector.segment(range.key(), range.label(), None);
        }
        let selector = selector.on_change(cx.listener(|this, value: &SharedString, _, cx| {
            this.set_usage_range(UsageRange::from_key(value), cx);
        }));
        let day_navigation = if let UsageRange::Day(offset) = self.usage_range {
            let today = Local::now().date_naive();
            let date = today - ChronoDuration::days(i64::from(offset));
            let earliest = today - ChronoDuration::days(DAY_WINDOW - 1);
            let weak = cx.entity().downgrade();
            Some(
                ely_gpui_component::forms::DatePicker::new(
                    "usage-day-picker",
                    date.to_string().parse().ok(),
                )
                .range(earliest.to_string().parse().ok(), today.to_string().parse().ok())
                .on_change(move |date, _, cx| {
                    let selected = date.strftime("%Y-%m-%d").to_string();
                    if let Ok(selected) = NaiveDate::parse_from_str(&selected, "%Y-%m-%d") {
                        let offset = (Local::now().date_naive() - selected).num_days();
                        let _ = weak.update(cx, |this, cx| {
                            this.set_usage_range(
                                UsageRange::Day(offset.clamp(0, DAY_WINDOW - 1) as u8),
                                cx,
                            );
                        });
                    }
                }),
            )
        } else {
            None
        };
        let stacked = self.usage_content_width > px(0.0)
            && self.usage_content_width < gpui::rems(36.0).to_pixels(window.rem_size());
        div()
            .flex()
            .items_center()
            .gap_2()
            .when(stacked, |row| row.flex_wrap())
            .children(day_navigation)
            .child(div().flex_none().child(selector))
    }

    fn render_usage_kpis(
        &self,
        views: &UsageViews,
        window: &Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let totals = &views.totals;
        let prev = views.prev_totals.as_ref();
        let series = dashboard_series_colors(theme);

        let hit = cache_hit_percent(
            totals.cache_read,
            totals
                .input
                .saturating_add(totals.cache_read)
                .saturating_add(totals.cache_write),
        );
        let prev_hit = prev.and_then(|p| {
            cache_hit_percent(
                p.cache_read,
                p.input
                    .saturating_add(p.cache_read)
                    .saturating_add(p.cache_write),
            )
        });

        let total_tokens = totals.total();
        let direct_ratio = if total_tokens > 0 {
            (totals.direct() as f32 / total_tokens as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let gen_basis = totals.input.saturating_add(totals.generated());
        let gen_ratio = if gen_basis > 0 {
            (totals.generated() as f32 / gen_basis as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let hit_ratio = (hit.unwrap_or(0.0) / 100.0).clamp(0.0, 1.0) as f32;

        let cards = [
            KpiCardSpec {
                id: "total",
                metric: UsageChartMetric::Total,
                icon: IconName::Activity,
                label: tr!("usage_page.kpi_total_tokens"),
                header_meta: None,
                value: format_tokens(total_tokens),
                tick_ratio: direct_ratio,
                color: series[0],
                delta: relative_delta(
                    total_tokens as f64,
                    prev.map(|p| p.total() as f64),
                ),
                fallback_caption: format!(
                    "{} {} · {} {}",
                    tr!("usage_page.tokens_input"),
                    format_tokens(totals.input),
                    tr!("usage_page.tokens_cache"),
                    format_tokens(totals.cache_read.saturating_add(totals.cache_write))
                ),
            },
            KpiCardSpec {
                id: "generated",
                metric: UsageChartMetric::Generated,
                icon: IconName::Sparkles,
                label: tr!("usage_page.kpi_generated_tokens"),
                header_meta: None,
                value: format_tokens(totals.generated()),
                tick_ratio: gen_ratio,
                color: series[1],
                delta: relative_delta(
                    totals.generated() as f64,
                    prev.map(|p| p.generated() as f64),
                ),
                fallback_caption: if matches!(self.usage_range, UsageRange::All) {
                    format!(
                        "{} {} · {} {}",
                        tr!("usage_page.tokens_output"),
                        format_tokens(totals.output),
                        tr!("usage_page.flow_reasoning"),
                        format_tokens(totals.reasoning)
                    )
                } else {
                    format!(
                        "{} {}",
                        tr!("usage_page.tokens_input"),
                        format_tokens(totals.input)
                    )
                },
            },
            KpiCardSpec {
                id: "cache-hit",
                metric: UsageChartMetric::CacheHit,
                icon: IconName::Zap,
                label: tr!("usage_page.kpi_cache_hit"),
                header_meta: None,
                value: hit
                    .map(|v| format!("{v:.1}%"))
                    .unwrap_or_else(|| "—".into()),
                tick_ratio: hit_ratio,
                color: series[2],
                delta: point_delta(hit, prev_hit),
                fallback_caption: tr!(
                    "usage_page.cache_read_tokens",
                    tokens = format_tokens(totals.cache_read)
                ),
            },
        ];

        let cols = kpi_columns(self.usage_content_width, window);
        let subscription_card = self.render_usage_subscription_card(theme, cx);
        div()
            .w_full()
            .grid()
            .grid_cols(cols)
            .items_stretch()
            .gap_3()
            .children(cards.into_iter().map(|card| {
                let selected = self.usage_chart_metric == card.metric;
                render_kpi_card(card, selected, theme, cx)
            }))
            .child(subscription_card)
    }
}

struct KpiCardSpec {
    id: &'static str,
    metric: UsageChartMetric,
    icon: IconName,
    label: String,
    header_meta: Option<String>,
    value: String,
    tick_ratio: f32,
    color: Hsla,
    delta: Option<f64>,
    fallback_caption: String,
}

fn kpi_columns(content_width: Pixels, window: &Window) -> u16 {
    let w = if content_width > px(0.0) {
        f32::from(content_width)
    } else {
        f32::from(window.viewport_size().width) - 96.0
    };
    if w >= 860.0 {
        4
    } else if w >= 480.0 {
        2
    } else {
        1
    }
}

fn two_card_columns(content_width: Pixels) -> u16 {
    if content_width > px(0.0) && content_width < px(820.0) {
        1
    } else {
        2
    }
}

fn relative_delta(current: f64, previous: Option<f64>) -> Option<f64> {
    let prev = previous?;
    if prev <= 0.0 || !prev.is_finite() || !current.is_finite() {
        return None;
    }
    Some((current - prev) / prev * 100.0)
}

fn point_delta(current: Option<f64>, previous: Option<f64>) -> Option<f64> {
    let (cur, prev) = (current?, previous?);
    if !cur.is_finite() || !prev.is_finite() {
        return None;
    }
    Some(cur - prev)
}

fn render_kpi_card(
    spec: KpiCardSpec,
    selected: bool,
    theme: &Theme,
    cx: &mut Context<Fintwind>,
) -> Stateful<Div> {
    const TICK_COUNT: usize = 32;
    let filled = ((spec.tick_ratio.clamp(0.0, 1.0) * TICK_COUNT as f32).round() as usize)
        .min(TICK_COUNT);
    let unfilled_color = theme.border_strong.opacity(if theme.is_dark { 0.6 } else { 0.8 });

    let ticks = div()
        .w_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(3.0))
        .children((0..TICK_COUNT).map(|ix| {
            div()
                .w(px(2.5))
                .h(px(7.0))
                .rounded_full()
                .flex_none()
                .bg(if ix < filled {
                    spec.color
                } else {
                    unfilled_color
                })
        }));

    let footer = if let Some(delta) = spec.delta {
        let (trend_icon, trend_color) = if delta > 0.05 {
            (IconName::TrendingUp, theme.success_text)
        } else if delta < -0.05 {
            (IconName::TrendingDown, theme.danger_text)
        } else {
            (IconName::Minus, theme.text_tertiary)
        };
        let signed = if delta >= 0.0 {
            format!("+{delta:.1}%")
        } else {
            format!("{delta:.1}%")
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .min_w_0()
            .child(Icon::new(trend_icon).size(IconSize::Xs).color(trend_color))
            .child(
                tabular(div())
                    .flex_none()
                    .text_size(ui_px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(trend_color)
                    .child(signed),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_secondary)
                    .child(tr!("usage_page.vs_prior_period")),
            )
    } else {
        div()
            .min_w_0()
            .truncate()
            .text_size(ui_px(12.0))
            .text_color(theme.text_tertiary)
            .child(spec.fallback_caption)
    };

    let metric = spec.metric;
    let outer_bg = if selected {
        theme.overlay
    } else {
        theme.inset
    };
    let outer_border = if selected {
        theme.border_strong
    } else {
        theme.card_border()
    };

    div()
        .id(SharedString::from(format!("usage-kpi-{}", spec.id)))
        .role(gpui::Role::Button)
        .tab_index(0)
        .min_w_0()
        .rounded(px(13.0))
        .border_1()
        .border_color(outer_border)
        .bg(outer_bg)
        .shadow_xs()
        .p(px(4.0))
        .flex()
        .flex_col()
        .cursor_pointer()
        .focus_visible(|style| style.border_color(theme.accent_focus))
        .hover(|style| style.bg(theme.overlay))
        .on_activation(cx, move |this, _, cx| {
            if this.usage_chart_metric != metric {
                this.usage_chart_metric = metric;
                cx.notify();
            }
        })
        .child(
            div()
                .px_2()
                .py_1p5()
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
                        .child(
                            Icon::new(spec.icon)
                                .size(IconSize::Sm)
                                .color(theme.text_tertiary),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(ui_px(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text_secondary)
                                .child(spec.label),
                        ),
                )
                .children(spec.header_meta.map(|meta| {
                    tabular(div())
                        .flex_none()
                        .text_size(ui_px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(meta)
                })),
        )
        .child(
            div()
                .flex_1()
                .rounded(px(9.0))
                .border_1()
                .border_color(theme.card_border())
                .bg(theme.composer)
                .px_3()
                .py_2p5()
                .flex()
                .flex_col()
                .justify_between()
                .gap_3()
                .child(
                    tabular(div())
                        .truncate()
                        .text_size(ui_px(22.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(spec.value),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(ticks)
                        .child(footer),
                ),
        )
}

/// Primary dashboard palette inspired by the analytics reference, adapted
/// cleanly for both light and dark themes.
pub(super) fn dashboard_series_colors(theme: &Theme) -> [Hsla; 5] {
    let colors = if theme.is_dark {
        [0xff6e4a, 0x60a5fa, 0x34d399, 0xfacc15, 0xf472b6]
    } else {
        [0xfd4b23, 0x2563eb, 0x18b170, 0xe8b41c, 0xec4899]
    };
    colors.map(|color| crate::theme::mute_saturation(rgb(color).into()))
}

/// The hue series every chart on this page draws from. The treemap borrows it
/// so the page keeps one palette instead of two.
pub(super) fn plot_colors(theme: &Theme) -> [Hsla; 5] {
    let colors = if theme.is_dark {
        [0x60a5fa, 0x4ade80, 0xfb923c, 0xc084fc, 0xfb7185]
    } else {
        [0x1d4ed8, 0x15803d, 0xc2410c, 0x7e22ce, 0xbe123c]
    };
    colors.map(|color| crate::theme::mute_saturation(rgb(color).into()))
}

/// Product-owned surface treatment around Ely content. Match the composer
/// and inspector cards, rather than Ely's unrelated default flat surface.
pub(super) fn usage_card(theme: &Theme) -> Div {
    crate::ui::card(theme)
}

fn usage_notice(_theme: &Theme, icon_path: &'static str, _tint: Hsla, message: String) -> Alert {
    Alert::new(
        SharedString::from(format!("usage-notice-{icon_path}")),
        if icon_path == "icons/alert.svg" {
            Severity::Warning
        } else {
            Severity::Info
        },
        message,
    )
}

fn usage_retry_button(_theme: &Theme, cx: &mut Context<Fintwind>) -> Button {
    Button::new("usage-stats-retry", tr!("usage_page.retry"))
        .size(ControlSize::Sm)
        .variant(ButtonVariant::Ghost)
        .on_click(cx.listener(|this, _, _, cx| {
            this.ensure_usage_stats(true, cx);
        }))
}

fn usage_empty_card(_theme: &Theme, title: String, body: String) -> EmptyState {
    EmptyState::new("usage-history-empty", IconName::ChartColumn, title).body(body)
}

/// One day's aggregate bucket, zeroed.
fn empty_day(date: NaiveDate) -> UsageDay {
    UsageDay {
        date,
        total: 0,
        direct: 0,
        sessions: 0,
        input: 0,
        output: 0,
        cache_write: 0,
        cache_read: 0,
        cost: 0.0,
    }
}

/// The local calendar day a unix second falls on. Shared with the daemon's
/// scan so both sides agree on where a usage bucket sits, DST included.
fn local_date(timestamp: u64) -> Option<NaiveDate> {
    fintwind_protocol::model::local_date(timestamp)
}

/// `HH:MM` in the local zone, for the toolbar's freshness reading.
/// Session message-day shares, or one last-touched share when unsplit.
/// Older scans lacking sub-agent day shares receive only their unallocated
/// remainder here; current scans place child usage on the child's date.
fn entry_day_lanes(entry: &UsageEntry) -> Vec<UsageDayShare> {
    match &entry.days {
        Some(shares) => {
            let mut lanes = shares.clone();
            let remainder = entry
                .total_tokens()
                .saturating_sub(shares.iter().map(|s| s.total).sum());
            if remainder > 0 {
                lanes.push(UsageDayShare {
                    timestamp: entry.timestamp,
                    subagent: entry.subagent_tokens > 0,
                    direct: entry
                        .input_tokens
                        .saturating_add(entry.output_tokens)
                        .saturating_add(entry.reasoning_tokens)
                        .saturating_sub(shares.iter().map(|s| s.direct).sum()),
                    total: remainder,
                    input: entry
                        .input_tokens
                        .saturating_sub(shares.iter().map(|s| s.input).sum()),
                    output: entry
                        .output_tokens
                        .saturating_add(entry.reasoning_tokens)
                        .saturating_sub(shares.iter().map(|s| s.output).sum()),
                    cache_read: entry
                        .cache_read_tokens
                        .saturating_sub(shares.iter().map(|s| s.cache_read).sum()),
                    cache_write: entry
                        .cache_write_tokens
                        .saturating_sub(shares.iter().map(|s| s.cache_write).sum()),
                    cost: (entry.cost.unwrap_or_default()
                        - shares.iter().map(|s| s.cost).sum::<f64>())
                    .max(0.0),
                });
            } else {
                // Some providers report more cost at session level than the
                // sum of their messages. Keep the difference on last activity.
                let gap = (entry.cost.unwrap_or_default()
                    - shares.iter().map(|s| s.cost).sum::<f64>())
                .max(0.0);
                if gap > 0.0 {
                    let entry_date = local_date(entry.timestamp);
                    if let Some(last) = lanes
                        .iter_mut()
                        .filter(|lane| local_date(lane.timestamp) == entry_date)
                        .max_by_key(|lane| lane.timestamp)
                    {
                        last.cost += gap;
                    } else {
                        lanes.push(UsageDayShare {
                            timestamp: entry.timestamp,
                            cost: gap,
                            ..Default::default()
                        });
                    }
                }
            }
            lanes
        }
        None => {
            let direct = entry
                .input_tokens
                .saturating_add(entry.output_tokens)
                .saturating_add(entry.reasoning_tokens);
            vec![UsageDayShare {
                timestamp: entry.timestamp,
                subagent: entry.subagent_tokens > 0,
                direct,
                total: entry.total_tokens(),
                input: entry.input_tokens,
                output: entry.output_tokens.saturating_add(entry.reasoning_tokens),
                cache_read: entry.cache_read_tokens,
                cache_write: entry.cache_write_tokens,
                cost: entry.cost.unwrap_or_default(),
            }]
        }
    }
}

/// Select only the spend on a local calendar day. Session-level totals cannot
/// answer this for a conversation crossing midnight; its message buckets can.
fn entry_on_day(entry: &UsageEntry, date: NaiveDate) -> Option<UsageEntry> {
    entry_in_dates(entry, |candidate| candidate == date)
}

fn entry_in_dates(entry: &UsageEntry, includes: impl Fn(NaiveDate) -> bool) -> Option<UsageEntry> {
    let accept = |timestamp| local_date(timestamp).is_some_and(&includes);
    let lanes: Vec<_> = entry_day_lanes(entry)
        .into_iter()
        .filter(|lane| accept(lane.timestamp))
        .collect();
    let last = lanes.iter().max_by_key(|lane| lane.timestamp)?;
    let child_days: Vec<_> = lanes.iter().filter(|lane| lane.subagent).collect();
    let subagent_models = entry
        .subagent_models
        .iter()
        .filter_map(|child| {
            let days: Vec<_> = child
                .days
                .iter()
                .filter(|day| accept(day.timestamp))
                .copied()
                .collect();
            let day_tokens: u64 = days.iter().map(|day| day.total).sum();
            (day_tokens > 0).then(|| {
                let mut selected = child.clone();
                selected.input_tokens = days.iter().map(|day| day.input).sum();
                selected.output_tokens = days.iter().map(|day| day.output).sum();
                selected.reasoning_tokens = 0;
                selected.cache_read_tokens = days.iter().map(|day| day.cache_read).sum();
                selected.cache_write_tokens = days.iter().map(|day| day.cache_write).sum();
                selected.cost = selected.cost.map(|_| days.iter().map(|day| day.cost).sum());
                selected
                    .requests
                    .retain(|request| accept(request.timestamp));
                selected.days = days;
                selected
            })
        })
        .collect();
    let model_lanes = entry
        .model_lanes
        .iter()
        .filter_map(|lane| {
            let days: Vec<_> = lane
                .days
                .iter()
                .filter(|day| accept(day.timestamp))
                .copied()
                .collect();
            let total = days.iter().map(|day| day.total).sum::<u64>();
            if total == 0 {
                return None;
            }
            let mut selected = lane.clone();
            selected.total = total;
            selected.cost = days.iter().map(|day| day.cost).sum();
            selected.tokens = [
                days.iter().map(|day| day.input).sum(),
                days.iter().map(|day| day.output).sum(),
                0,
                days.iter().map(|day| day.cache_read).sum(),
                days.iter().map(|day| day.cache_write).sum(),
            ];
            selected.days = days;
            selected.hours.clear();
            selected
                .requests
                .retain(|request| accept(request.timestamp));
            Some(selected)
        })
        .collect();
    Some(UsageEntry {
        timestamp: last.timestamp,
        model: entry.model.clone(),
        directory: entry.directory.clone(),
        cost: entry.cost.map(|_| lanes.iter().map(|lane| lane.cost).sum()),
        input_tokens: lanes.iter().map(|lane| lane.input).sum(),
        output_tokens: lanes.iter().map(|lane| lane.output).sum(),
        reasoning_tokens: 0, // included in the output series
        cache_read_tokens: lanes.iter().map(|lane| lane.cache_read).sum(),
        cache_write_tokens: lanes.iter().map(|lane| lane.cache_write).sum(),
        subagent_sessions: if entry.days.is_some() {
            if entry.subagent_models.is_empty() {
                child_days.len() as u32
            } else {
                entry
                    .subagent_models
                    .iter()
                    .filter(|child| child.days.iter().any(|day| accept(day.timestamp)))
                    .map(|child| child.session_id.as_str())
                    .collect::<std::collections::HashSet<_>>()
                    .len() as u32
            }
        } else {
            entry.subagent_sessions
        },
        subagent_tokens: if entry.days.is_some() {
            child_days.iter().map(|lane| lane.total).sum()
        } else {
            entry.subagent_tokens
        },
        subagent_direct: 0,
        days: None,
        hours: Vec::new(),
        model_lanes,
        subagent_models,
    })
}

/// How one session's usage distributes over models.
///
/// A session the daemon split by message spent under each model it used, and
/// the session-level `model` names only the one it ended on — so the lanes win
/// whenever they exist. Every share carries a `primary` flag and exactly one
/// does per session, so the ranking's session counts still add up to the KPI
/// even when a session used three models.
struct ModelShare {
    /// `<providerID>/<modelID>`, or `None` for a session the server never
    /// named a model for — it folds into the ranking's unknown row.
    model: Option<String>,
    total: u64,
    cost: f64,
    /// Whether this share represents the session as a whole.
    primary: bool,
}

/// A session's per-model shares, largest first so the primary flag lands on
/// the model the session spent most under.
///
/// Two adjustments keep the ranking consistent with the totals above it:
///
/// - Folded sub-agent spend uses each child's model; the parent's message
///   lanes cover only its own work. Older scans lacking child model data fall
///   back to the session-level model.
/// - A provider that reported no per-message cost would otherwise show the
///   model as free while the KPI above it charges for it; the session's own
///   estimate then lands on its biggest share instead.
fn entry_model_shares(entry: &UsageEntry) -> Vec<ModelShare> {
    let child_total: u64 = entry
        .subagent_models
        .iter()
        .map(|child| child.total_tokens())
        .sum();
    let parent_total = entry.total_tokens().saturating_sub(child_total);
    let child_cost: f64 = entry
        .subagent_models
        .iter()
        .map(|child| child.cost.unwrap_or_default())
        .sum();
    let parent_cost = (entry.cost.unwrap_or_default() - child_cost).max(0.0);
    let mut shares: Vec<(Option<String>, u64, f64)> = if entry.model_lanes.is_empty() {
        vec![(entry.model.clone(), parent_total, parent_cost)]
    } else {
        let mut lanes: Vec<(Option<String>, u64, f64)> = entry
            .model_lanes
            .iter()
            .map(|lane| (Some(lane.model.clone()), lane.total, lane.cost))
            .collect();
        if entry.subagent_models.is_empty() && entry.subagent_tokens > 0 {
            lanes.push((entry.model.clone(), entry.subagent_tokens, 0.0));
        }
        let missing_tokens = parent_total.saturating_sub(lanes.iter().map(|lane| lane.1).sum());
        let mut missing_index = None;
        if missing_tokens > 0 {
            missing_index = Some(lanes.len());
            lanes.push((entry.model.clone(), missing_tokens, 0.0));
        }
        if let Some(cost) = entry.cost {
            let remainder =
                (cost - child_cost - lanes.iter().map(|lane| lane.2).sum::<f64>()).max(0.0);
            if remainder > 0.0 {
                // A folded sub-agent has no model-level cost lane; book its
                // remaining charge to the parent's named model when possible.
                let recipient = missing_index
                    .or_else(|| {
                        entry.model.as_deref().and_then(|model| {
                            lanes
                                .iter()
                                .position(|lane| lane.0.as_deref() == Some(model))
                        })
                    })
                    .unwrap_or(0);
                if let Some(lane) = lanes.get_mut(recipient) {
                    lane.2 += remainder;
                }
            }
        }
        lanes
    };

    // A child's work must not make the parent's session count migrate to the
    // child's model when its token volume is larger.
    let primary_model = shares
        .iter()
        .max_by_key(|share| share.1)
        .map(|share| share.0.clone());
    shares.extend(entry.subagent_models.iter().map(|child| {
        (
            child.model.clone(),
            child.total_tokens(),
            child.cost.unwrap_or_default(),
        )
    }));

    shares.sort_by(|a, b| b.1.cmp(&a.1));
    let mut counted_primary = false;
    shares
        .into_iter()
        .map(|(model, total, cost)| {
            let primary = !counted_primary && primary_model.as_ref() == Some(&model);
            counted_primary |= primary;
            ModelShare {
                model,
                total,
                cost,
                primary,
            }
        })
        .collect()
}

/// How long a disk-cached catalog answers before one refresh is worth a
/// download. Prices move slowly, and the page rescans itself every half
/// minute — a per-scan download would be abuse; a day-old official price
/// is still far closer to the truth than the server's own numbers.
const PRICE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Local prices, including stale ones. Never downloads; token snapshots must
/// not wait for the independent catalog refresh. Background executor only.
fn pricing_table(cached: Option<Arc<ModelsDevTable>>) -> Option<Arc<ModelsDevTable>> {
    if let Some(table) = cached.filter(|table| table.age() < models_dev::TABLE_REUSE_WINDOW) {
        return Some(table);
    }
    models_dev::cached_catalog()
        .filter(|table| table.has_any_cost())
        .map(Arc::new)
}

/// Rewrite every cost the scan carries into official-API estimates.
///
/// The server's own cost numbers carry whatever semantics the provider
/// chose — subscription relays answer with numbers far under list price,
/// some providers answer nothing — so the page prices each session's tokens
/// from the models.dev catalog instead. A session whose model the catalog
/// does not price — or every session, when no catalog could be had — loses
/// its cost entirely: it reads as unpriced through the `None` rather than
/// as free, and no server number survives anywhere on the page.
///
/// Indexed requests are priced individually (including context tiers), then
/// bucketed by model/day/hour. Older snapshots without request facts retain
/// proportional fallback pricing. Runs on the background executor.
pub(super) fn price_usage_stats(stats: &mut UsageStats, table: Option<&ModelsDevTable>) {
    let price = |model: Option<&str>,
                 model_provider: Option<&str>,
                 input,
                 output,
                 reasoning,
                 cache_read,
                 cache_write| {
        model
            .and_then(model_id_part)
            .and_then(|id| table.and_then(|table| table.resolve_model_cost_for(id, model_provider)))
            // The tier follows the request's context — input plus cache
            // tokens, the same basis OpenCode's own cost pass uses — so a
            // long-context session prices at the catalog's long-context
            // rates instead of the base ones.
            .map(|rate| rate.for_context(input + cache_read + cache_write))
            .map(|rate| {
                rate.charge(input, rate.input)
                    + rate.charge(output + reasoning, rate.output)
                    + rate.charge(cache_read, rate.cache_read)
                    + rate.charge(cache_write, rate.cache_write)
            })
    };
    let price_requests = |model: Option<&str>,
                          requests: &[fintwind_client::provider_session::UsageRequest],
                          tokens: [u64; 5]| {
        if requests.is_empty() {
            return price(
                model,
                model.and_then(model_provider_part),
                tokens[0],
                tokens[1],
                tokens[2],
                tokens[3],
                tokens[4],
            );
        }
        requests.iter().try_fold(0.0, |total, request| {
            let tokens = request.tokens;
            Some(
                total
                    + price(
                        model,
                        model.and_then(model_provider_part),
                        tokens[0],
                        tokens[1],
                        tokens[2],
                        tokens[3],
                        tokens[4],
                    )?,
            )
        })
    };
    for entry in &mut stats.entries {
        let mut parent_tokens = [
            entry.input_tokens,
            entry.output_tokens,
            entry.reasoning_tokens,
            entry.cache_read_tokens,
            entry.cache_write_tokens,
        ];
        for child in &mut entry.subagent_models {
            let child_tokens = [
                child.input_tokens,
                child.output_tokens,
                child.reasoning_tokens,
                child.cache_read_tokens,
                child.cache_write_tokens,
            ];
            for (parent, child) in parent_tokens.iter_mut().zip(child_tokens) {
                *parent = parent.saturating_sub(child);
            }
            child.cost = if child.total_tokens() == 0 {
                Some(0.0)
            } else {
                price_requests(child.model.as_deref(), &child.requests, child_tokens)
            };
            reprice_request_buckets(
                &child.requests,
                &mut child.days,
                &mut child.hours,
                |tokens| {
                    price(
                        child.model.as_deref(),
                        child.model.as_deref().and_then(model_provider_part),
                        tokens[0],
                        tokens[1],
                        tokens[2],
                        tokens[3],
                        tokens[4],
                    )
                },
            );
        }
        let mut assigned = [0u64; 5];
        let mut lane_cost = Some(0.0);
        for lane in &mut entry.model_lanes {
            for (sum, tokens) in assigned.iter_mut().zip(lane.tokens) {
                *sum = sum.saturating_add(tokens);
            }
            let tokens = lane.tokens;
            lane.cost = if tokens.iter().all(|count| *count == 0) {
                0.0
            } else {
                let cost = price_requests(Some(&lane.model), &lane.requests, tokens);
                lane_cost = lane_cost.and_then(|subtotal| Some(subtotal + cost?));
                cost.unwrap_or_default()
            };
            reprice_request_buckets(&lane.requests, &mut lane.days, &mut lane.hours, |tokens| {
                price(
                    Some(&lane.model),
                    model_provider_part(&lane.model),
                    tokens[0],
                    tokens[1],
                    tokens[2],
                    tokens[3],
                    tokens[4],
                )
            });
        }
        let missing = std::array::from_fn::<_, 5, _>(|index| {
            parent_tokens[index].saturating_sub(assigned[index])
        });
        let remainder_cost = if missing.iter().all(|tokens| *tokens == 0) {
            Some(0.0)
        } else {
            price(
                entry.model.as_deref(),
                entry.model.as_deref().and_then(model_provider_part),
                missing[0],
                missing[1],
                missing[2],
                missing[3],
                missing[4],
            )
        };
        let parent_cost = lane_cost.and_then(|subtotal| Some(subtotal + remainder_cost?));
        let cost = parent_cost.and_then(|parent_cost| {
            entry
                .subagent_models
                .iter()
                .try_fold(parent_cost, |total, child| Some(total + child.cost?))
        });
        let Some(cost) = cost else {
            entry.cost = None;
            for child in &mut entry.subagent_models {
                child.cost = None;
            }
            if let Some(days) = &mut entry.days {
                for share in days {
                    share.cost = 0.0;
                }
            }
            for hour in &mut entry.hours {
                hour.cost = 0.0;
            }
            for lane in &mut entry.model_lanes {
                lane.cost = 0.0;
            }
            continue;
        };
        let own_total: u64 = parent_tokens.iter().sum();
        let own_cost = parent_cost.unwrap_or_default();
        let own_share = |tokens: u64| {
            if own_total == 0 {
                0.0
            } else {
                own_cost * tokens as f64 / own_total as f64
            }
        };
        entry.cost = Some(cost);
        let exact_parent = missing.iter().all(|tokens| *tokens == 0)
            && entry
                .model_lanes
                .iter()
                .all(|lane| !lane.requests.is_empty());
        let mut parent_days = HashMap::<NaiveDate, f64>::new();
        let mut parent_hours = HashMap::<u64, f64>::new();
        if exact_parent {
            for lane in &entry.model_lanes {
                for day in &lane.days {
                    if let Some(date) = local_date(day.timestamp) {
                        *parent_days.entry(date).or_default() += day.cost;
                    }
                }
                for hour in &lane.hours {
                    *parent_hours.entry(hour.timestamp / 3600).or_default() += hour.cost;
                }
            }
        }
        if let Some(days) = &mut entry.days {
            let mut child_costs: HashMap<NaiveDate, f64> = HashMap::new();
            for child in &entry.subagent_models {
                let total = child.total_tokens();
                for day in &child.days {
                    if let Some(date) = local_date(day.timestamp) {
                        *child_costs.entry(date).or_default() += if !child.requests.is_empty() {
                            day.cost
                        } else if total == 0 {
                            0.0
                        } else {
                            child.cost.unwrap_or_default() * day.total as f64 / total as f64
                        };
                    }
                }
            }
            let mut child_day_totals: HashMap<NaiveDate, u64> = HashMap::new();
            for day in days.iter().filter(|day| day.subagent) {
                if let Some(date) = local_date(day.timestamp) {
                    *child_day_totals.entry(date).or_default() += day.total;
                }
            }
            for day in days {
                day.cost = if day.subagent {
                    local_date(day.timestamp).map_or(0.0, |date| {
                        let total = child_day_totals.get(&date).copied().unwrap_or_default();
                        if total == 0 {
                            0.0
                        } else {
                            child_costs.get(&date).copied().unwrap_or_default() * day.total as f64
                                / total as f64
                        }
                    })
                } else {
                    if exact_parent {
                        local_date(day.timestamp)
                            .and_then(|date| parent_days.get(&date).copied())
                            .unwrap_or_default()
                    } else {
                        own_share(day.total)
                    }
                };
            }
        }
        let child_hours: usize = entry
            .subagent_models
            .iter()
            .map(|child| child.hours.len())
            .sum();
        let own_hours = entry.hours.len().saturating_sub(child_hours);
        for hour in &mut entry.hours[..own_hours] {
            hour.cost = if exact_parent {
                parent_hours
                    .get(&(hour.timestamp / 3600))
                    .copied()
                    .unwrap_or_default()
            } else {
                own_share(hour.input + hour.output + hour.cache_read + hour.cache_write)
            };
        }
        let mut remaining_hours = &mut entry.hours[own_hours..];
        for child in &entry.subagent_models {
            let count = child.hours.len().min(remaining_hours.len());
            let (child_hours, rest) = remaining_hours.split_at_mut(count);
            remaining_hours = rest;
            let child_total = child.total_tokens();
            for (hour, source) in child_hours.iter_mut().zip(&child.hours) {
                let tokens = hour.input + hour.output + hour.cache_read + hour.cache_write;
                hour.cost = if !child.requests.is_empty() {
                    source.cost
                } else if child_total == 0 {
                    0.0
                } else {
                    child.cost.unwrap_or_default() * tokens as f64 / child_total as f64
                };
            }
        }
        for lane in &mut entry.model_lanes {
            if lane.tokens.iter().all(|tokens| *tokens == 0) {
                lane.cost = own_share(lane.total);
            }
        }
    }
}

fn reprice_request_buckets(
    requests: &[fintwind_client::provider_session::UsageRequest],
    days: &mut [UsageDayShare],
    hours: &mut [fintwind_client::provider_session::UsageHourShare],
    price: impl Fn([u64; 5]) -> Option<f64>,
) {
    if requests.is_empty() {
        return;
    }
    let mut daily = HashMap::<NaiveDate, f64>::new();
    let mut hourly = HashMap::<u64, f64>::new();
    for request in requests {
        let cost = price(request.tokens).unwrap_or_default();
        if let Some(date) = local_date(request.timestamp) {
            *daily.entry(date).or_default() += cost;
        }
        *hourly.entry(request.timestamp / 3600).or_default() += cost;
    }
    for day in days {
        day.cost = local_date(day.timestamp)
            .and_then(|date| daily.get(&date).copied())
            .unwrap_or_default();
    }
    for hour in hours {
        hour.cost = hourly
            .get(&(hour.timestamp / 3600))
            .copied()
            .unwrap_or_default();
    }
}

/// The model part of the `<providerID>/<modelID>` string the server
/// records. A string without a slash is all model; a trailing slash names
/// nothing.
fn model_id_part(model: &str) -> Option<&str> {
    let id = match model.split_once('/') {
        Some((_provider, id)) => id,
        None => model,
    };
    (!id.is_empty()).then_some(id)
}

/// The provider part of the `<providerID>/<modelID>` string the server
/// records — the catalog provider whose copy of the model should answer
/// for it, so a vendor's own entry outranks the relays' re-listings.
fn model_provider_part(model: &str) -> Option<&str> {
    let (provider, _) = model.split_once('/')?;
    (!provider.is_empty()).then_some(provider)
}

/// Aggregate a snapshot for `range` on the background executor, never per frame.
pub(super) fn build_views(stats: Option<&UsageStats>, range: UsageRange) -> UsageViews {
    let Some(stats) = stats else {
        return UsageViews::default();
    };
    let today = Local::now().date_naive();
    let selected_day = match range {
        UsageRange::Day(offset) => Some(today - ChronoDuration::days(i64::from(offset))),
        _ => None,
    };
    let bar_horizon = range
        .days()
        .unwrap_or(BAR_HORIZON_DAYS)
        .min(BAR_HORIZON_DAYS);
    let bar_start = selected_day.unwrap_or(today - ChronoDuration::days(bar_horizon - 1));

    let mut totals = UsageTotals::default();
    let mut prev_totals = (!matches!(range, UsageRange::All)).then(UsageTotals::default);
    // Day buckets are built from every entry: the heatmap shows its fixed
    // 26-week window however narrow the selected range is. The range only
    // gates what the totals, daily bars, and rankings count.
    let mut days: HashMap<NaiveDate, UsageDay> = HashMap::new();
    let mut hourly = (0..24)
        .map(|hour| UsageHour {
            hour,
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let mut prev_hourly = selected_day.map(|_| {
        (0..24)
            .map(|hour| UsageHour {
                hour,
                ..Default::default()
            })
            .collect::<Vec<_>>()
    });
    let mut hourly_estimated = false;
    // Model rows key on the raw `provider/id` string, except a session the
    // server recorded no model for — it folds into the unknown-model row so
    // the ranking's session counts add up to the KPI.
    let mut models: HashMap<Option<String>, (u32, u64, f64, u64)> = HashMap::new();
    // Provider rows derive from the model rows above, once those are complete:
    // a provider is then counted once per model rather than once per session,
    // and the provider count survives the model ranking being truncated.
    let mut providers: HashMap<String, (u32, u64, f64)> = HashMap::new();
    // Projects key on a normalized directory (separators + case), so a
    // server that records `E:\work\x` and `e:/work/x` as the same project
    // ranks as one; the first-seen spelling is what gets displayed.
    let mut projects: HashMap<String, (String, u32, u64, f64)> = HashMap::new();
    let mut range_days: std::collections::HashSet<NaiveDate> = std::collections::HashSet::new();
    let mut prev_range_days: std::collections::HashSet<NaiveDate> =
        std::collections::HashSet::new();

    for entry in &stats.entries {
        let mut session_days = std::collections::HashSet::new();
        let selected = if let Some(date) = selected_day {
            entry_on_day(entry, date)
        } else if let Some(days) = range.days() {
            let start = today - ChronoDuration::days(days - 1);
            entry_in_dates(entry, |date| date >= start)
        } else {
            None
        };
        if let Some(prev_totals) = &mut prev_totals {
            let prev_entry = if let Some(date) = selected_day {
                entry_on_day(entry, date - ChronoDuration::days(1))
            } else if let Some(days) = range.days() {
                let start = today - ChronoDuration::days(days - 1);
                let prev_start = start - ChronoDuration::days(days);
                entry_in_dates(entry, |date| date >= prev_start && date < start)
            } else {
                None
            };
            if let Some(prev_in_range) = prev_entry {
                prev_totals.add(&prev_in_range);
                if let Some(date) = local_date(prev_in_range.timestamp) {
                    prev_range_days.insert(date);
                }
            }
        }
        let in_range = if matches!(range, UsageRange::All) {
            Some(entry)
        } else {
            selected.as_ref()
        };
        if let Some(in_range) = in_range {
            totals.add(in_range);
            if let Some(date) = local_date(in_range.timestamp) {
                range_days.insert(date);
            }
            // A refined session spent under more than one model, and the
            // session-level `model` names only the last. Attribute its usage
            // per message when the walk produced the lanes; otherwise the
            // session-level attribution is all there is.
            for share in entry_model_shares(in_range) {
                let row = models
                    .entry(share.model)
                    .or_insert((0, 0, 0.0, in_range.timestamp));
                // Only the primary share counts a session: a session that used
                // three models is still one session, and the ranking's session
                // counts are read against the KPI's.
                if share.primary {
                    row.0 += 1;
                }
                row.1 = row.1.saturating_add(share.total);
                row.2 += share.cost;
                row.3 = row.3.max(in_range.timestamp);
            }
            if let Some(directory) = &entry.directory {
                let key = directory.replace('\\', "/").to_lowercase();
                let row = projects
                    .entry(key)
                    .or_insert_with(|| (directory.clone(), 0, 0, 0.0));
                row.1 += 1;
                row.2 = row.2.saturating_add(in_range.total_tokens());
                row.3 += in_range.cost.unwrap_or_default();
            }
        }

        // The timeline is built from every entry regardless of range, so the
        // heatmap keeps its fixed window however narrow the selection is. A
        // refined session contributes its per-message days instead of dumping
        // everything on the day it was last touched.
        for lane in entry_day_lanes(entry) {
            let Some(date) = local_date(lane.timestamp) else {
                continue;
            };
            if selected_day.map_or_else(
                || range.days().is_none_or(|days| date >= today - ChronoDuration::days(days - 1)),
                |selected| date == selected,
            ) {
                range_days.insert(date);
            } else if let Some(selected) = selected_day {
                if date == selected - ChronoDuration::days(1) {
                    prev_range_days.insert(date);
                }
            } else if let Some(days) = range.days() {
                let start = today - ChronoDuration::days(days - 1);
                let prev_start = start - ChronoDuration::days(days);
                if date >= prev_start && date < start {
                    prev_range_days.insert(date);
                }
            }
            let day = days.entry(date).or_insert_with(|| empty_day(date));
            day.total = day.total.saturating_add(lane.total);
            day.direct = day.direct.saturating_add(lane.direct);
            day.input = day.input.saturating_add(lane.input);
            day.output = day.output.saturating_add(lane.output);
            day.cache_read = day.cache_read.saturating_add(lane.cache_read);
            day.cache_write = day.cache_write.saturating_add(lane.cache_write);
            day.cost += lane.cost;
            if session_days.insert(date) {
                day.sessions += 1;
            }
        }
        if let Some(selected) = selected_day {
            hourly_estimated |= accumulate_day_hours(entry, selected, &mut hourly);
            if let Some(prev_hourly) = &mut prev_hourly {
                accumulate_day_hours(entry, selected - ChronoDuration::days(1), prev_hourly);
            }
        }
    }
    totals.active_days = range_days.len() as u32;
    if let Some(prev_totals) = &mut prev_totals {
        prev_totals.active_days = prev_range_days.len() as u32;
    }

    let mut daily = Vec::with_capacity(bar_horizon as usize);
    for offset in 0..bar_horizon {
        let date = bar_start + ChronoDuration::days(offset);
        // The map stays intact: the heatmap below reads the same days.
        let day = days.get(&date).cloned().unwrap_or_else(|| empty_day(date));
        daily.push(day);
    }
    let prev_daily = if selected_day.is_none() && !matches!(range, UsageRange::All) {
        let prev_start = bar_start - ChronoDuration::days(bar_horizon);
        (0..bar_horizon)
            .map(|offset| {
                let date = prev_start + ChronoDuration::days(offset);
                days.get(&date).cloned().unwrap_or_else(|| empty_day(date))
            })
            .collect()
    } else {
        Vec::new()
    };

    // The heatmap window ends on this week's Sunday, so the final column
    // always contains today and every column starts on a Monday. Days past
    // today (the rest of this week) render as empty placeholders.
    let today_weekday = today.weekday().num_days_from_monday() as i64;
    let grid_start = today + ChronoDuration::days(6 - today_weekday)
        - ChronoDuration::days(HEATMAP_WEEKS * 7 - 1);
    let grid_len = HEATMAP_WEEKS * 7;
    let mut cells: Vec<Option<UsageHeatCell>> = Vec::with_capacity(grid_len as usize);
    let mut max_direct = 0u64;
    for offset in 0..grid_len {
        let date = grid_start + ChronoDuration::days(offset);
        // Quiet past days stay dated cells. `None` is only the future, so a
        // month boundary is the calendar 1st even when that day had no session.
        // Dropping quiet days folded empty weeks into the previous month.
        let cell = if date > today {
            None
        } else {
            let day = days.get(&date);
            Some(UsageHeatCell {
                date,
                #[cfg(test)]
                total: day.map(|day| day.total).unwrap_or(0),
                direct: day.map(|day| day.direct).unwrap_or(0),
                #[cfg(test)]
                sessions: day.map(|day| day.sessions).unwrap_or(0),
                level: 0,
            })
        };
        if let Some(cell) = &cell {
            max_direct = max_direct.max(cell.direct);
        }
        cells.push(cell);
    }
    let heatmap_active_days = cells
        .iter()
        .flatten()
        .filter(|cell| cell.direct > 0)
        .count() as u32;
    // Four equal shares of the window's busiest day. Scaling by 4 (not 3)
    // is what lets that day reach level 4, the legend's top swatch; a factor
    // of 3 tops out at level 3 and leaves the brightest color unused.
    for cell in cells.iter_mut().flatten() {
        cell.level = heat_level(cell.direct, max_direct);
    }
    let weeks = cells
        .chunks(7)
        .map(<[Option<UsageHeatCell>]>::to_vec)
        .collect();

    let matrix_start = today + ChronoDuration::days(6 - today_weekday)
        - ChronoDuration::days(MATRIX_WEEKS * 7 - 1);
    let mut weekly_matrix = Vec::with_capacity(MATRIX_WEEKS as usize);
    let mut matrix_active_days = 0u32;
    for week_ix in 0..MATRIX_WEEKS {
        let week_monday = matrix_start + ChronoDuration::days(week_ix * 7);
        let mut row_cells: [Option<UsageWeekMatrixCell>; 7] = [None; 7];
        let mut direct_total = 0u64;
        for (day_ix, slot) in row_cells.iter_mut().enumerate() {
            let date = week_monday + ChronoDuration::days(day_ix as i64);
            if date <= today {
                let day = days.get(&date);
                let direct = day.map_or(0, |d| d.direct);
                let total = day.map_or(0, |d| d.total);
                let sessions = day.map_or(0, |d| d.sessions);
                if direct > 0 {
                    matrix_active_days += 1;
                }
                direct_total = direct_total.saturating_add(direct);
                *slot = Some(UsageWeekMatrixCell {
                    date,
                    direct,
                    total,
                    sessions,
                });
            }
        }
        let label = if week_ix + 1 == MATRIX_WEEKS {
            tr!("usage_page.this_week")
        } else {
            format_day_label(week_monday)
        };
        weekly_matrix.push(UsageWeekMatrixRow {
            label,
            direct_total,
            cells: row_cells,
        });
    }

    // Providers roll up the model rows, before the ranking is truncated: the
    // provider's model count and total must stay right even when only the top
    // N models are shown. Rolling up here rather than while walking sessions
    // also counts a provider once per model, not once per session — a store
    // can spend heavily on a single model, and the session count would say
    // nothing about it.
    for (model, (_, total, cost, _)) in &models {
        let Some((provider, _)) = model.as_deref().and_then(|model| model.split_once('/')) else {
            continue;
        };
        let entry = providers.entry(provider.to_owned()).or_insert((0, 0, 0.0));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(*total);
        entry.2 += cost;
    }

    let mut models: Vec<UsageModelRow> = models
        .into_iter()
        .map(
            |(model, (sessions, total, cost, last_used))| UsageModelRow {
                model,
                sessions,
                total,
                cost,
                last_used,
            },
        )
        .collect();
    models.sort_by(|a, b| b.total.cmp(&a.total).then(b.last_used.cmp(&a.last_used)));
    let model_count = models.len();

    let mut provider_rows: Vec<UsageProviderRow> = providers
        .into_iter()
        .map(|(provider, (models, total, cost))| UsageProviderRow {
            provider,
            models,
            total,
            cost,
        })
        .collect();
    provider_rows.sort_by_key(|b| std::cmp::Reverse(b.total));
    let provider_count = provider_rows.len();

    let mut projects: Vec<UsageProjectRow> = projects
        .into_iter()
        .map(|(_, (directory, sessions, total, cost))| UsageProjectRow {
            directory,
            sessions,
            total,
            cost,
        })
        .collect();
    projects.sort_by(|a, b| b.total.cmp(&a.total).then(b.directory.cmp(&a.directory)));
    let project_count = projects.len();

    UsageViews {
        totals,
        prev_totals,
        daily,
        prev_daily,
        hourly,
        prev_hourly: prev_hourly.unwrap_or_default(),
        hourly_estimated,
        weeks,
        heatmap_active_days,
        weekly_matrix,
        matrix_active_days,
        models,
        model_count,
        providers: provider_rows,
        provider_count,
        projects,
        project_count,
    }
}

fn accumulate_day_hours(
    entry: &UsageEntry,
    target_day: NaiveDate,
    hourly: &mut [UsageHour],
) -> bool {
    let mut estimated = false;
    if entry.hours.is_empty() {
        if entry.total_tokens() > 0 && local_date(entry.timestamp) == Some(target_day) {
            estimated = true;
            let hour = local_hour(entry.timestamp) as usize;
            let bucket = &mut hourly[hour];
            bucket.input = bucket.input.saturating_add(entry.input_tokens);
            bucket.output = bucket
                .output
                .saturating_add(entry.output_tokens.saturating_add(entry.reasoning_tokens));
            bucket.cache_read = bucket.cache_read.saturating_add(entry.cache_read_tokens);
            bucket.cache_write = bucket
                .cache_write
                .saturating_add(entry.cache_write_tokens);
            bucket.cost += entry.cost.unwrap_or_default();
        }
    } else {
        for share in &entry.hours {
            if local_date(share.timestamp) != Some(target_day) {
                continue;
            }
            estimated |= share.estimated;
            let bucket = &mut hourly[local_hour(share.timestamp) as usize];
            bucket.input = bucket.input.saturating_add(share.input);
            bucket.output = bucket.output.saturating_add(share.output);
            bucket.cache_read = bucket.cache_read.saturating_add(share.cache_read);
            bucket.cache_write = bucket.cache_write.saturating_add(share.cache_write);
            bucket.cost += share.cost;
        }
        // A session summary can contain tokens absent from its
        // message walk (including folded sub-agents). Attribute the
        // unlocated remainder to last activity.
        if local_date(entry.timestamp) == Some(target_day) {
            let missing_input = entry
                .input_tokens
                .saturating_sub(entry.hours.iter().map(|h| h.input).sum());
            let missing_output = entry
                .output_tokens
                .saturating_add(entry.reasoning_tokens)
                .saturating_sub(entry.hours.iter().map(|h| h.output).sum());
            let missing_read = entry
                .cache_read_tokens
                .saturating_sub(entry.hours.iter().map(|h| h.cache_read).sum());
            let missing_write = entry
                .cache_write_tokens
                .saturating_sub(entry.hours.iter().map(|h| h.cache_write).sum());
            let missing_cost = (entry.cost.unwrap_or_default()
                - entry.hours.iter().map(|h| h.cost).sum::<f64>())
            .max(0.0);
            // Ignore floating-point dust from the pricing pass.
            estimated |= missing_input > 0
                || missing_output > 0
                || missing_read > 0
                || missing_write > 0
                || missing_cost > 0.005;
            let bucket = &mut hourly[local_hour(entry.timestamp) as usize];
            bucket.input = bucket.input.saturating_add(missing_input);
            bucket.output = bucket.output.saturating_add(missing_output);
            bucket.cache_read = bucket.cache_read.saturating_add(missing_read);
            bucket.cache_write = bucket.cache_write.saturating_add(missing_write);
            bucket.cost += missing_cost;
        }
    }
    estimated
}

fn local_hour(timestamp: u64) -> u32 {
    chrono::DateTime::from_timestamp(timestamp as i64, 0)
        .map(|instant| instant.with_timezone(&Local).hour())
        .unwrap_or(0)
}

/// 0 when the day is quiet, otherwise 1–4 by equal shares of `max_direct`.
fn heat_level(direct: u64, max_direct: u64) -> u8 {
    if direct == 0 || max_direct == 0 {
        0
    } else {
        ((direct as f64 / max_direct as f64) * 4.0)
            .ceil()
            .clamp(1.0, 4.0) as u8
    }
}

#[cfg(test)]
struct HeatMonthBand {
    key: (i32, u32),
    start: usize,
    end: usize,
}

/// Group week columns into calendar months. A week belongs to the month of
/// its 1st when it contains one (so September opens on the column that
/// actually enters September, even if that Monday is still August);
/// otherwise it belongs to its Monday. Weeks are never split.
#[cfg(test)]
fn heatmap_month_bands(weeks: &[Vec<Option<UsageHeatCell>>]) -> Vec<HeatMonthBand> {
    let mut bands: Vec<HeatMonthBand> = Vec::new();
    for (index, week) in weeks.iter().enumerate() {
        let Some(key) = week_month(week) else {
            if let Some(band) = bands.last_mut() {
                band.end = index + 1;
            }
            continue;
        };
        let same_month = bands.last().is_some_and(|band| band.key == key);
        if same_month {
            if let Some(band) = bands.last_mut() {
                band.end = index + 1;
            }
        } else {
            bands.push(HeatMonthBand {
                key,
                start: index,
                end: index + 1,
            });
        }
    }
    bands
}

#[cfg(test)]
fn week_month(week: &[Option<UsageHeatCell>]) -> Option<(i32, u32)> {
    let mut monday = None;
    for cell in week.iter().flatten() {
        if cell.date.day() == 1 {
            return Some((cell.date.year(), cell.date.month()));
        }
        if monday.is_none() {
            monday = Some((cell.date.year(), cell.date.month()));
        }
    }
    monday
}

fn format_day_label(date: NaiveDate) -> String {
    if crate::i18n::uses_east_asian_date_format() {
        format!("{}月{}日", date.month(), date.day())
    } else {
        format!("{}/{}", date.month(), date.day())
    }
}

/// The server's cost estimates are US dollars; two decimals read naturally
/// from cents up to hundreds of dollars, which is where stores max out.
pub(super) fn format_cost(cost: f64) -> String {
    format!("${cost:.2}")
}

/// The ranking rows show the directory's own name; the full path is the
/// tooltip.
fn project_display_name(directory: &str) -> String {
    directory
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(directory)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(timestamp: u64, model: &str, output: u64) -> UsageEntry {
        UsageEntry {
            timestamp,
            model: Some(model.to_owned()),
            directory: None,
            cost: None,
            input_tokens: 0,
            output_tokens: output,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            subagent_sessions: 0,
            subagent_tokens: 0,
            subagent_direct: 0,
            days: None,
            hours: Vec::new(),
            model_lanes: Vec::new(),
            subagent_models: Vec::new(),
        }
    }

    /// The pricing pass replaces the server's cost with the catalog's
    /// official-API estimate and re-derives every split from it, so each
    /// view conserves the session's one total; a model the catalog does not
    /// price reads as unpriced, not free.
    #[test]
    fn pricing_prices_from_the_catalog_and_conserves_the_splits() {
        use fintwind_client::models_dev::parse_catalog;
        let table = parse_catalog(
            r#"{"prov": {"id": "prov", "models": {
                "m1": {"id": "m1", "cost": {"input": 3.0, "output": 15.0}},
                "bare": {"id": "bare", "cost": {"input": 1.0, "output": 1.0}}
            }}}"#,
        )
        .unwrap();

        let mut priced = entry(1_800_000, "prov/m1", 100_000);
        priced.input_tokens = 1_000_000;
        priced.reasoning_tokens = 900_000;
        priced.cost = Some(999.0); // the server's number must not survive
        priced.days = Some(vec![
            // 1M of the session's 2M tokens on each of two days.
            fintwind_client::provider_session::UsageDayShare {
                timestamp: 1_700_000,
                subagent: false,
                direct: 500_000,
                total: 1_000_000,
                input: 500_000,
                output: 500_000,
                cache_read: 0,
                cache_write: 0,
                cost: 123.0,
            },
            fintwind_client::provider_session::UsageDayShare {
                timestamp: 1_800_000,
                subagent: false,
                direct: 500_000,
                total: 1_000_000,
                input: 500_000,
                output: 500_000,
                cache_read: 0,
                cache_write: 0,
                cost: 456.0,
            },
        ]);
        priced.hours = vec![fintwind_client::provider_session::UsageHourShare {
            timestamp: 1_800_000,
            estimated: false,
            input: 200_000,
            output: 300_000,
            cache_read: 0,
            cache_write: 0,
            cost: 789.0,
        }];
        priced.model_lanes = vec![fintwind_client::provider_session::UsageModelLane {
            model: "prov/m1".into(),
            total: 500_000,
            cost: 321.0,
            tokens: [0; 5],
            days: Vec::new(),
            hours: Vec::new(),
            requests: Vec::new(),
        }];

        let mut unpriced = entry(1_800_000, "prov/other", 10);
        unpriced.cost = Some(5.0);
        unpriced.days = Some(vec![fintwind_client::provider_session::UsageDayShare {
            timestamp: 1_800_000,
            subagent: false,
            direct: 10,
            total: 10,
            input: 10,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cost: 5.0,
        }]);

        let mut stats = UsageStats {
            entries: vec![priced, unpriced],
            truncated: false,
            sessions_scanned: 2,
            ..Default::default()
        };
        price_usage_stats(&mut stats, Some(&table));

        // Input at $3/M plus 1M generated at $15/M.
        let priced = &stats.entries[0];
        assert_eq!(priced.cost, Some(18.0));
        // Each day carried half the session's tokens, so each carries half
        // the session's price; the old server numbers are gone.
        let days = priced.days.as_deref().unwrap();
        assert_eq!(days.len(), 2);
        for day in days {
            assert!((day.cost - 9.0).abs() < 1e-9, "day cost {}", day.cost);
        }
        // The hour's 500k of 2M tokens prices at a quarter of the session.
        assert!((priced.hours[0].cost - 4.5).abs() < 1e-9);
        // The model lane's 500k likewise.
        assert!((priced.model_lanes[0].cost - 4.5).abs() < 1e-9);

        // A model the catalog does not price loses its cost entirely.
        let unpriced = &stats.entries[1];
        assert_eq!(unpriced.cost, None);
        assert_eq!(unpriced.days.as_deref().unwrap()[0].cost, 0.0);
    }

    /// Without a catalog nothing is priced at all: every cost the scan
    /// carried reads as absent, so no server-side number can leak through
    /// any view dressed as an official-API estimate.
    #[test]
    fn pricing_without_a_catalog_prices_nothing() {
        let mut stats = UsageStats {
            entries: vec![entry(1_800_000, "prov/m1", 10)],
            truncated: false,
            sessions_scanned: 1,
            ..Default::default()
        };
        stats.entries[0].cost = Some(2.5);
        price_usage_stats(&mut stats, None);
        assert_eq!(stats.entries[0].cost, None);
    }

    #[test]
    fn model_id_part_splits_the_provider_prefix() {
        assert_eq!(model_id_part("anthropic/claude-4-5"), Some("claude-4-5"));
        assert_eq!(model_id_part("bare"), Some("bare"));
        assert_eq!(model_id_part("prov/"), None);
        assert_eq!(model_id_part(""), None);
    }

    /// The timestamp cutoff decides what a range keeps, and the daily window
    /// is always contiguous days ending today.
    #[test]
    fn views_follow_the_selected_range() {
        let now = unix_time();
        let mut old = entry(now - 90 * 86_400, "p/m2", 30);
        old.directory = Some("E:\\work\\old".into());
        old.cost = Some(1.5);
        let mut mid = entry(now - 40 * 86_400, "p/m2", 20);
        mid.directory = Some("E:\\work\\mid".into());
        let stats = UsageStats {
            entries: vec![entry(now - 86_400, "p/m1", 10), mid, old],
            truncated: false,
            sessions_scanned: 3,
            ..Default::default()
        };

        let all = build_views(Some(&stats), UsageRange::All);
        assert_eq!(all.totals.sessions, 3);
        assert_eq!(all.totals.output, 60);
        assert_eq!(all.totals.active_days, 3);
        assert_eq!(all.totals.costed_sessions, 1);
        assert_eq!(all.totals.cost, 1.5);
        assert_eq!(all.daily.len(), BAR_HORIZON_DAYS as usize);
        assert_eq!(all.weeks.len(), HEATMAP_WEEKS as usize);
        assert_eq!(all.models.len(), 2);
        assert_eq!(all.models[0].model.as_deref(), Some("p/m2"));
        assert_eq!(all.models[0].total, 50);
        assert_eq!(all.models[0].cost, 1.5);
        // Every session carries a directory here, so two projects rank —
        // largest token share first, which puts the old 30-token session's
        // project on top.
        assert_eq!(all.project_count, 2);
        assert_eq!(all.projects[0].directory, "E:\\work\\old");
        assert_eq!(all.projects[0].total, 30);
        assert_eq!(all.projects[0].cost, 1.5);
        assert_eq!(all.projects[0].sessions, 1);

        let week = build_views(Some(&stats), UsageRange::Days7);
        assert_eq!(week.totals.sessions, 1);
        assert_eq!(week.models.len(), 1);
        assert_eq!(week.daily.len(), 7);
        // The heatmap ignores the range: all three entries land inside its
        // 26-week window under either selection.
        assert_eq!(all.heatmap_active_days, week.heatmap_active_days);
        assert_eq!(all.heatmap_active_days, 3);
    }

    /// The heatmap grid ends today and every column begins on a Monday, so
    /// the drawn rows line up with the weekday gutter labels.
    #[test]
    fn heatmap_grid_is_monday_anchored_and_ends_today() {
        let stats = UsageStats {
            entries: vec![entry(unix_time(), "p/m", 5)],
            truncated: false,
            sessions_scanned: 1,
            ..Default::default()
        };
        let views = build_views(Some(&stats), UsageRange::All);
        let last = views.weeks.last().unwrap();
        let today = Local::now().date_naive();
        let today_index = today.weekday().num_days_from_monday() as usize;
        assert!(last[today_index].is_some());
        // Every cell after today in the final column is past the present.
        assert!(last[today_index + 1..].iter().all(|cell| cell.is_none()));
        for week in &views.weeks {
            assert_eq!(week.len(), 7);
            // Each drawn cell sits in the row its weekday names.
            for (row_index, cell) in week.iter().enumerate() {
                if let Some(cell) = cell {
                    assert_eq!(
                        cell.date.weekday().num_days_from_monday() as usize,
                        row_index
                    );
                }
            }
        }
    }

    /// A week that contains the 1st opens that month, even when its Monday is
    /// still the previous month. Later weeks of the new month stay in the band,
    /// and January does not collapse into December.
    #[test]
    fn heatmap_months_split_on_the_first() {
        let first = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let opening = first - ChronoDuration::days(first.weekday().num_days_from_monday() as i64);
        let weeks = vec![
            filled_week(opening - ChronoDuration::days(7)),
            filled_week(opening),
            filled_week(opening + ChronoDuration::days(7)),
        ];
        let bands = heatmap_month_bands(&weeks);
        assert_eq!(bands.len(), 2);
        assert_eq!(bands[0].end, 1);
        assert_eq!(bands[1].key, (2026, 9));
        assert_eq!((bands[1].start, bands[1].end), (1, 3));

        let new_year = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();
        let opening =
            new_year - ChronoDuration::days(new_year.weekday().num_days_from_monday() as i64);
        let weeks = vec![
            filled_week(opening - ChronoDuration::days(7)),
            filled_week(opening),
        ];
        let bands = heatmap_month_bands(&weeks);
        assert_eq!(bands.len(), 2);
        assert_eq!(bands[0].key.1, 12);
        assert_eq!(bands[1].key, (2027, 1));
    }

    /// A quiet 1st still opens its month. Activity earlier that week must not
    /// keep the column in the previous month, and a fully quiet window still
    /// draws every week under some month.
    #[test]
    fn quiet_first_of_month_still_opens_the_band() {
        let first = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let monday = first - ChronoDuration::days(first.weekday().num_days_from_monday() as i64);
        let mut week = filled_week(monday);
        for cell in week.iter_mut().flatten() {
            if cell.date.day() == 1 {
                cell.direct = 0;
                cell.total = 0;
            } else if cell.date < first {
                cell.direct = 50;
                cell.total = 50;
                cell.sessions = 1;
            }
        }
        assert_eq!(week_month(&week), Some((2026, 9)));

        let views = build_views(
            Some(&UsageStats {
                entries: Vec::new(),
                truncated: false,
                sessions_scanned: 0,
                ..Default::default()
            }),
            UsageRange::All,
        );
        assert_eq!(views.weeks.len(), HEATMAP_WEEKS as usize);
        let today = Local::now().date_naive();
        let future = 6 - today.weekday().num_days_from_monday() as i64;
        let dated = views.weeks.iter().flatten().flatten().count() as i64;
        assert_eq!(dated, HEATMAP_WEEKS * 7 - future);
        let month_first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();
        assert!(
            views
                .weeks
                .iter()
                .flatten()
                .flatten()
                .any(|cell| cell.date == month_first && cell.direct == 0)
        );
        let bands = heatmap_month_bands(&views.weeks);
        let covered: usize = bands.iter().map(|band| band.end - band.start).sum();
        assert_eq!(covered, views.weeks.len());
        assert!(
            bands
                .iter()
                .any(|band| band.key == (today.year(), today.month()))
        );
    }

    /// The busiest direct day reaches the top heatmap swatch even when a
    /// quieter day has more cached tokens.
    #[test]
    fn heatmap_intensity_excludes_cache() {
        let now = Local::now();
        let today = now.date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let mut cache_heavy = entry(now.timestamp() as u64, "p/m", 1_000);
        cache_heavy.cache_read_tokens = 50_000_000;
        let direct_heavy = entry(
            (now - ChronoDuration::days(1)).timestamp() as u64,
            "p/m",
            5_000_000,
        );
        let stats = UsageStats {
            entries: vec![cache_heavy, direct_heavy],
            truncated: false,
            sessions_scanned: 2,
            ..Default::default()
        };
        let views = build_views(Some(&stats), UsageRange::All);
        let cache_day = views.daily.iter().find(|day| day.date == today).unwrap();
        let direct_day = views
            .daily
            .iter()
            .find(|day| day.date == yesterday)
            .unwrap();
        assert!(cache_day.total > direct_day.total);
        assert!(cache_day.direct < direct_day.direct);

        let level = |date: NaiveDate| {
            views
                .weeks
                .iter()
                .flatten()
                .flatten()
                .find(|cell| cell.date == date)
                .unwrap()
                .level
        };
        assert_eq!(level(yesterday), 4);
        assert!(level(today) < level(yesterday));
    }

    /// A session that delegates to sub-agents and ran across days must still
    /// put every token it holds on the timeline. Its split covers only its own
    /// messages, so the folded amount needs its own lane — otherwise the
    /// daily bars and the heatmap would sum to less than the KPI printed
    /// beside them, on exactly the sessions that delegate the most.
    ///
    /// The folded lane rides `subagent_direct` for its non-cache share: the
    /// chart is titled "excludes cache", and pushing the fold's whole total
    /// through it would silently re-label cache as direct.
    #[test]
    fn a_split_session_carries_its_folded_subagent_spend_on_the_timeline() {
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let noon = |date: NaiveDate| {
            Local
                .from_local_datetime(&date.and_hms_opt(12, 0, 0).unwrap())
                .single()
                .unwrap()
                .timestamp() as u64
        };
        let mut split = entry(noon(today), "p/m", 30);
        split.days = Some(vec![
            UsageDayShare {
                timestamp: noon(yesterday),
                direct: 30,
                total: 40,
                ..Default::default()
            },
            UsageDayShare {
                timestamp: noon(today),
                direct: 40,
                total: 60,
                ..Default::default()
            },
        ]);
        // Folded in from a sub-agent: 50 tokens, 20 of them non-cache. The
        // fold has already merged those into the session's own lanes, which
        // is what the KPI reads.
        split.subagent_sessions = 1;
        split.subagent_tokens = 50;
        split.subagent_direct = 20;
        split.input_tokens = 60;
        split.cache_read_tokens = 60;

        let stats = UsageStats {
            entries: vec![split],
            truncated: false,
            sessions_scanned: 1,
            ..Default::default()
        };
        let views = build_views(Some(&stats), UsageRange::All);
        let day_of = |date: NaiveDate| {
            views
                .daily
                .iter()
                .find(|day| day.date == date)
                .unwrap()
                .clone()
        };

        // Yesterday keeps only its own share; the fold rides the parent's last
        // touch, which is today.
        assert_eq!(day_of(yesterday).direct, 30);
        assert_eq!(day_of(yesterday).total, 40);
        let today_day = day_of(today);
        assert_eq!(today_day.direct, 40 + 20);
        assert_eq!(today_day.total, 60 + 50);
        // The KPI reads the entry's lanes, fold included — which is exactly
        // what makes the assertion below meaningful.
        assert_eq!(views.totals.generated(), 30);
        assert_eq!(views.totals.input, 60);
        assert_eq!(views.totals.cache_read, 60);
        assert_eq!(views.totals.sessions, 1);
        // Nothing lands outside the days the session is known to have used,
        // and the timeline now sums to exactly the KPI's total — the
        // disagreement the split-without-the-fold produced.
        let drawn: u64 = views
            .daily
            .iter()
            .filter(|day| day.total > 0)
            .map(|day| day.total)
            .sum();
        assert_eq!(drawn, views.totals.total());
        assert_eq!(drawn, 150);
    }

    /// A split session spreads its usage across the days it ran, so one that
    /// ran from yesterday into today is not drawn as a single spike today.
    /// Without the split every token lands on the day the session was last
    /// touched, which is the distortion the split exists to remove.
    #[test]
    fn a_split_session_spreads_its_usage_across_the_days_it_ran() {
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        // Noon, so "yesterday" cannot roll back two days near midnight.
        let noon = |date: NaiveDate| {
            Local
                .from_local_datetime(&date.and_hms_opt(12, 0, 0).unwrap())
                .single()
                .unwrap()
                .timestamp() as u64
        };
        let mut split = entry(noon(today), "p/m", 30);
        split.days = Some(vec![
            UsageDayShare {
                timestamp: noon(yesterday),
                direct: 30,
                total: 40,
                ..Default::default()
            },
            UsageDayShare {
                timestamp: noon(today),
                direct: 70,
                total: 90,
                ..Default::default()
            },
        ]);
        let unsplit = entry(noon(today), "p/m", 100);

        let stats = UsageStats {
            entries: vec![split, unsplit],
            truncated: false,
            sessions_scanned: 2,
            ..Default::default()
        };
        let views = build_views(Some(&stats), UsageRange::All);

        let day_of = |date: NaiveDate| {
            views
                .daily
                .iter()
                .find(|day| day.date == date)
                .unwrap()
                .clone()
        };
        // 30 + 100 from the unsplit session, whose every token still rides its
        // own single-day bucket.
        assert_eq!(day_of(yesterday).direct, 30);
        // 70 split + 100 unsplit.
        assert_eq!(day_of(today).direct, 170);
        // The session-level totals are untouched by the split: both lanes add
        // up to the same whole.
        assert_eq!(views.totals.output, 130);
        assert_eq!(views.totals.generated(), 130);
    }

    /// A session that used three models is one session, and a session that
    /// delegated to a sub-agent spent all of that too. Both properties are
    /// what keeps the model ranking readable against the KPI above it; losing
    /// either makes the ranking's counts silently disagree with the headline.
    #[test]
    fn model_shares_preserve_the_session_count_and_the_folded_spend() {
        let mut switched = entry(unix_time(), "p/second", 10);
        switched.model_lanes = vec![
            UsageModelLane {
                model: "p/first".to_owned(),
                total: 100,
                cost: 0.0,
                tokens: [0; 5],
                days: Vec::new(),
                hours: Vec::new(),
                requests: Vec::new(),
            },
            UsageModelLane {
                model: "p/second".to_owned(),
                total: 40,
                cost: 0.0,
                tokens: [0; 5],
                days: Vec::new(),
                hours: Vec::new(),
                requests: Vec::new(),
            },
            UsageModelLane {
                model: "p/third".to_owned(),
                total: 20,
                cost: 0.0,
                tokens: [0; 5],
                days: Vec::new(),
                hours: Vec::new(),
                requests: Vec::new(),
            },
        ];
        // The session spent 160 of its own plus 50 folded in from a sub-agent,
        // and the slot-level model names the one it ended on.
        switched.input_tokens = 160;
        switched.subagent_tokens = 50;
        switched.subagent_direct = 50;
        let plain = entry(unix_time(), "p/second", 30);

        let stats = UsageStats {
            entries: vec![switched, plain],
            truncated: false,
            sessions_scanned: 2,
            ..Default::default()
        };
        let views = build_views(Some(&stats), UsageRange::All);

        let row = |model: &str| {
            views
                .models
                .iter()
                .find(|row| row.model.as_deref() == Some(model))
                .unwrap()
                .clone()
        };
        assert_eq!(row("p/first").total, 100);
        assert_eq!(row("p/third").total, 20);
        // The fold has no lane of its own and rides the session-level model,
        // joining the `plain` session's 30 there too. That makes it the largest
        // row despite `p/first` being the biggest single lane.
        assert_eq!(row("p/second").total, 40 + 50 + 30);
        assert_eq!(views.models[0].model.as_deref(), Some("p/second"));
        // Three models used across two sessions, but only two sessions were
        // run: the ranking's session counts still add up to the KPI's.
        let counted: u32 = views.models.iter().map(|row| row.sessions).sum();
        assert_eq!(u64::from(counted), views.totals.sessions);
        assert_eq!(counted, 2);
    }

    fn filled_week(monday: NaiveDate) -> Vec<Option<UsageHeatCell>> {
        (0..7)
            .map(|offset| {
                Some(UsageHeatCell {
                    date: monday + ChronoDuration::days(offset),
                    total: 0,
                    direct: 0,
                    sessions: 0,
                    level: 0,
                })
            })
            .collect()
    }
}

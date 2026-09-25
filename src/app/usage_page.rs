//! The Usage page: one daemon-side scan over the workspace's OpenCode
//! sessions feeds token-usage statistics — KPI cards, a GitHub-style
//! activity heatmap, daily/hourly timelines, and per-model, per-provider, and
//! per-project rankings. Frames read only the aggregated views stored on the
//! entity; the raw scan is touched only when it lands or the range changes.

use std::collections::HashMap;

use chrono::{Datelike as _, Duration as ChronoDuration, Local, NaiveDate, Timelike as _};

#[cfg(test)]
use chrono::TimeZone as _;

use crate::theme::ui_px;
use crate::usage::{cache_hit_percent, format_percent, format_tokens};

use super::*;
use crate::ui::ActivationExt;
use fintwind_client::provider_session::{UsageDayShare, UsageEntry, UsageStats};
use gpui::PathBuilder;
use gpui::relative;

#[cfg(test)]
use fintwind_client::provider_session::UsageModelLane;

/// Heatmap horizon, in whole weeks ending today.
pub(super) const HEATMAP_WEEKS: i64 = 26;
/// Timeline horizon for the unbounded range.
const BAR_HORIZON_DAYS: i64 = 30;
const DAILY_CHART_PX: f32 = 96.0;
const DAY_WINDOW: i64 = 30;
const HEAT_CELL_PX: f32 = 12.0;
const HEAT_GAP_PX: f32 = 3.0;
const HEAT_LABEL_PX: f32 = 14.0;
const HEAT_LABEL_GAP_PX: f32 = 4.0;
/// Chart scrubbing restarts the tooltip timer on every bar, so the framework
/// default of 500ms feels stuck. Zero still waits one timer tick.
const CHART_TOOLTIP_DELAY: Duration = Duration::from_millis(0);
/// A stored scan older than this is refreshed silently on page open; within
/// it, reopening the page costs no server traversal.
const STALE_AFTER: Duration = Duration::from_secs(60);
/// While the page is open, how often to look for newer sessions. The scan
/// itself is daemon-side and cached, so a quiet interval is cheap; a session
/// that is actively streaming lands within half a minute of its next hop.
const AUTO_REFRESH: Duration = Duration::from_secs(30);
/// Model rows drawn before the rest collapse into the header's model count.
const MAX_MODEL_ROWS: usize = 8;

/// What the KPI cards, timelines, and model ranking aggregate over. The
/// heatmap keeps its own fixed week window, like GitHub's, so a short range
/// does not gut it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum UsageRange {
    #[default]
    All,
    Days30,
    Days7,
    Day(u8),
}

impl UsageRange {
    pub(super) const ALL: [Self; 4] = [Self::All, Self::Days30, Self::Days7, Self::Day(0)];

    fn label(self) -> String {
        tr!(match self {
            Self::All => "usage_page.range_all",
            Self::Days30 => "usage_page.range_30d",
            Self::Days7 => "usage_page.range_7d",
            Self::Day(_) => "usage_page.range_day",
        })
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
    pub total: u64,
    pub direct: u64,
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

#[derive(Clone, Debug, Default)]
pub(super) struct UsageViews {
    pub totals: UsageTotals,
    /// Contiguous days ending today (or the selected day), oldest first.
    pub daily: Vec<UsageDay>,
    pub hourly: Vec<UsageHour>,
    pub hourly_estimated: bool,
    /// Week columns of seven cells, Monday first; `None` past today.
    pub weeks: Vec<Vec<Option<UsageHeatCell>>>,
    pub heatmap_active_days: u32,
    pub models: Vec<UsageModelRow>,
    pub model_count: usize,
    pub providers: Vec<UsageProviderRow>,
    pub provider_count: usize,
    pub projects: Vec<UsageProjectRow>,
    pub project_count: usize,
}

impl Fintwind {
    /// Load the usage scan if none is stored or the stored one went stale.
    /// The scan covers the whole OpenCode store, so no per-project state
    /// matters here — only freshness does. One blocking traversal runs on
    /// the background executor; a refresh button call bypasses the
    /// staleness window.
    ///
    /// A scan that is already in flight is never started twice, and a stale
    /// scan is never discarded while it is being replaced: the page keeps
    /// drawing the previous numbers, so a refresh reads as the page updating
    /// rather than emptying.
    pub(super) fn ensure_usage_stats(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.usage_stats_pending {
            return;
        }
        // The directory only anchors which resident server the daemon asks;
        // the scan itself is global.
        let Some(binary) = self.native_binary_path() else {
            return;
        };
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
        let detailed_date = match self.usage_range {
            UsageRange::Day(offset) => {
                Some(Local::now().date_naive() - ChronoDuration::days(i64::from(offset)))
            }
            _ => None,
        };
        if !force
            && self.usage_stats.is_some()
            && detailed_date.map_or(true, |date| self.usage_detail_day == Some(date))
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
        let detailed_day = detailed_date.map(|date| i64::from(date.num_days_from_ce()));
        cx.spawn(async move |this, cx| {
            let scanned = cx
                .background_executor()
                .spawn(async move {
                    fintwind_client::persistence::StateStore::remote(daemon).fetch_usage_stats(
                        binary,
                        directory,
                        detailed_day,
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A newer scan superseded this one.
                if this.usage_stats_generation != generation {
                    return;
                }
                this.usage_stats_pending = false;
                match scanned {
                    Ok(stats) => {
                        this.usage_stats_error = None;
                        this.usage_stats_loaded_at = Some(Instant::now());
                        this.usage_detail_day = detailed_date;
                        this.usage_loaded_label = Some(clock_label());
                        this.usage_stats = Some(Rc::new(stats));
                        this.rebuild_usage_views(cx);
                        if matches!(this.usage_range, UsageRange::Day(_)) {
                            this.ensure_usage_stats(false, cx);
                        }
                    }
                    Err(error) => {
                        // `loaded_at` stays put, so reopening the page
                        // retries instead of waiting out the staleness
                        // window on a failed scan. A cached scan is kept on
                        // screen: an error is reported, not displayed by
                        // blanking numbers the user was still reading.
                        this.usage_stats_error = Some(error.to_string());
                        if let UsageRange::Day(offset) = this.usage_range {
                            let wanted =
                                Local::now().date_naive() - ChronoDuration::days(i64::from(offset));
                            if Some(wanted) != detailed_date {
                                this.ensure_usage_stats(false, cx);
                            }
                        }
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
                cx.background_executor().timer(AUTO_REFRESH).await;
                let still_open = this
                    .update(cx, |this, cx| {
                        if this.settings_page != Some(SettingsPage::Usage) {
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

    /// The toolbar's freshness reading: the wall-clock time the stored scan
    /// landed, or nothing before the first one.
    pub(super) fn usage_freshness_label(&self) -> Option<SharedString> {
        self.usage_loaded_label
            .as_deref()
            .map(|label| SharedString::from(tr!("usage_page.updated_at", time = label)))
    }

    pub(super) fn set_usage_range(&mut self, range: UsageRange, cx: &mut Context<Self>) {
        if self.usage_range == range {
            return;
        }
        self.usage_range = range;
        self.rebuild_usage_views(cx);
        if matches!(range, UsageRange::Day(_)) {
            self.ensure_usage_stats(false, cx);
        }
    }

    fn shift_usage_day(&mut self, delta: i64, cx: &mut Context<Self>) {
        if let UsageRange::Day(offset) = self.usage_range {
            let next = (i64::from(offset) + delta).clamp(0, DAY_WINDOW - 1) as u8;
            self.set_usage_range(UsageRange::Day(next), cx);
        }
    }

    fn rebuild_usage_views(&mut self, cx: &mut Context<Self>) {
        self.usage_views = build_views(self.usage_stats.as_deref(), self.usage_range);
        cx.notify();
    }

    pub(super) fn render_usage_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let first_load = self.usage_stats.is_none();
        let no_workspace =
            self.native_binary_path().is_none() || self.state.selected_project.is_none();

        let mut column = div().mt(px(15.0)).flex().flex_col().gap(px(12.0));

        column = column.child(self.render_usage_toolbar(&theme, cx));

        if let Some(error) = &self.usage_stats_error {
            column = column.child(
                usage_notice(
                    &theme,
                    "icons/alert.svg",
                    theme.warning,
                    tr!("usage_page.load_failed", error = error.clone()),
                )
                .child(usage_retry_button(&theme, cx)),
            );
        } else if self.usage_stats_pending && first_load {
            let label = tr!("usage_page.loading").to_owned();
            column = column.child(
                motion::pulse(Duration::from_millis(1400), move |phase| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(ui_px(12.0))
                        .text_color(theme.text_tertiary)
                        .child(icon("icons/loader-circle.svg", 13.0, theme.text_tertiary))
                        .child(SharedString::from(label.clone()))
                        .opacity(pulsating_between(0.5, 1.0)(phase))
                        .into_any_element()
                })
                .every(2)
                .into_any_element(),
            );
        }

        if no_workspace {
            column = column.child(usage_empty_card(
                &theme,
                tr!("usage_page.no_workspace_title"),
                tr!("usage_page.no_workspace_body"),
            ));
        } else if let Some(stats) = &self.usage_stats {
            if stats.entries.is_empty() {
                column = column.child(usage_empty_card(
                    &theme,
                    tr!("usage_page.empty_title"),
                    tr!("usage_page.empty_body"),
                ));
            } else {
                // A scan that ran out of pages covers only the newest
                // sessions. The numbers below are still correct for what they
                // cover — say what that is rather than let a quiet-looking
                // total speak for a store it never saw.
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
                let needs_hourly = match self.usage_range {
                    UsageRange::Day(offset) => {
                        self.usage_detail_day
                            != Some(
                                Local::now().date_naive() - ChronoDuration::days(i64::from(offset)),
                            )
                    }
                    _ => false,
                };
                column = column.child(self.render_usage_kpis(views, &theme));
                if !matches!(self.usage_range, UsageRange::Day(_)) {
                    column = column.child(self.render_usage_heatmap(views, &theme));
                }
                column = column
                    .child(if needs_hourly {
                        usage_notice(
                            &theme,
                            "icons/loader-circle.svg",
                            theme.text_secondary,
                            tr!("usage_page.hourly_loading"),
                        )
                        .into_any_element()
                    } else {
                        self.render_usage_daily(views, &theme).into_any_element()
                    })
                    .child(self.render_usage_models(views, &theme))
                    .child(self.render_usage_providers(views, &theme))
                    .child(self.render_usage_projects(views, &theme));
            }
        }

        column.into_any_element()
    }

    fn render_usage_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut range_buttons = Vec::new();
        for (index, range) in UsageRange::ALL.into_iter().enumerate() {
            let selected = match range {
                UsageRange::Day(_) => matches!(self.usage_range, UsageRange::Day(_)),
                _ => self.usage_range == range,
            };
            let mut button = div()
                .id(SharedString::from(format!("usage-range-{index}")))
                .tab_index(0)
                .h(px(24.0))
                .px(px(10.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .cursor_default()
                .text_size(ui_px(11.5))
                .border_1()
                .border_color(if selected {
                    theme.border_strong
                } else {
                    gpui::transparent_black()
                })
                .text_color(if selected {
                    theme.text
                } else {
                    theme.text_secondary
                })
                .hover(|element| element.bg(theme.overlay))
                .active(|element| element.bg(theme.overlay_strong))
                .focus_visible(|style| style.border_color(theme.accent))
                .when(selected, |element| element.bg(theme.raised))
                .child(range.label());
            button = button.on_activation(cx, move |this, _, cx| {
                this.set_usage_range(range, cx);
            });
            range_buttons.push(button);
        }
        let selector = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.0))
            .p(px(2.0))
            .rounded(px(8.0))
            .bg(theme.inset)
            .border_1()
            .border_color(theme.border)
            .children(range_buttons);

        let refresh = icon_button("usage-refresh", "icons/rotate-cw.svg", *theme)
            .tab_index(0)
            .when(self.usage_stats_pending, |element| element.opacity(0.5))
            .tooltip(Tooltip::text(tr!("usage_page.refresh")))
            .on_activation(cx, |this, _, cx| {
                this.ensure_usage_stats(true, cx);
            });

        // The freshness reading doubles as the "a scan ran" signal: nothing
        // shows before the first one lands.
        let freshness = self.usage_freshness_label().map(|label| {
            div()
                .flex_none()
                .text_size(ui_px(10.5))
                .text_color(theme.text_tertiary)
                .child(label)
        });

        let day_navigation = if let UsageRange::Day(offset) = self.usage_range {
            let date = Local::now().date_naive() - ChronoDuration::days(i64::from(offset));
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(usage_day_button(
                        "usage-day-previous",
                        "icons/chevron-left.svg",
                        offset < (DAY_WINDOW - 1) as u8,
                        1,
                        theme,
                        cx,
                    ))
                    .child(
                        div()
                            .min_w(px(88.0))
                            .text_center()
                            .text_size(ui_px(11.0))
                            .text_color(theme.text_secondary)
                            .child(date.format("%Y-%m-%d").to_string()),
                    )
                    .child(usage_day_button(
                        "usage-day-next",
                        "icons/chevron-right.svg",
                        offset > 0,
                        -1,
                        theme,
                        cx,
                    )),
            )
        } else {
            None
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .child(div().flex_1().min_w_0())
            .children(freshness)
            .children(day_navigation)
            .child(selector)
            .child(refresh)
    }

    fn render_usage_kpis(&self, views: &UsageViews, theme: &Theme) -> Div {
        let totals = &views.totals;
        let hit = cache_hit_percent(
            totals.cache_read,
            totals
                .input
                .saturating_add(totals.cache_read)
                .saturating_add(totals.cache_write),
        );
        let breakdown = format!(
            "{} {} · {} {} · {} {} · {} {}",
            tr!("usage_page.tokens_input"),
            format_tokens(totals.input),
            tr!("usage_page.tokens_output"),
            format_tokens(totals.output),
            tr!("usage_page.tokens_cache"),
            format_tokens(totals.cache_read.saturating_add(totals.cache_write)),
            tr!("usage_page.cache_hit_short"),
            hit.map(format_percent).unwrap_or_else(|| "—".into()),
        );
        div()
            .flex()
            .items_stretch()
            .gap(px(12.0))
            .child(usage_kpi_card(
                theme,
                tr!("usage_page.kpi_total_tokens"),
                format_tokens(totals.total()),
                breakdown,
            ))
            .child(usage_kpi_card(
                theme,
                tr!("usage_page.kpi_generated_tokens"),
                format_tokens(totals.generated()),
                format!(
                    "{} {}",
                    tr!("usage_page.incl_reasoning"),
                    format_tokens(totals.reasoning)
                ),
            ))
            .child(usage_kpi_card(
                theme,
                tr!("usage_page.kpi_sessions"),
                totals.sessions.to_string(),
                usage_sessions_subtitle(totals),
            ))
            .child(usage_kpi_card(
                theme,
                tr!("usage_page.kpi_cost"),
                format_cost(totals.cost),
                tr!("usage_page.cost_sessions", count = totals.costed_sessions),
            ))
    }

    fn render_usage_heatmap(&self, views: &UsageViews, theme: &Theme) -> Div {
        let cell_size = px(HEAT_CELL_PX);
        let gap = px(HEAT_GAP_PX);
        let label_h = px(HEAT_LABEL_PX);
        let label_gap = px(HEAT_LABEL_GAP_PX);

        // Weekday gutter on the Monday/Wednesday/Friday rows. The top spacer
        // matches each month group's label plus the gap under it, so the
        // rows stay locked to the cells.
        let weekday_row = |label: String| {
            div()
                .h(cell_size)
                .flex_none()
                .flex()
                .items_center()
                .text_size(ui_px(9.0))
                .text_color(theme.text_tertiary)
                .child(label)
        };
        let gutter = div()
            .w(px(18.0))
            .mr(px(6.0))
            .flex_none()
            .flex()
            .flex_col()
            .child(div().h(px(HEAT_LABEL_PX + HEAT_LABEL_GAP_PX)).flex_none())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(gap)
                    .child(weekday_row(tr!("usage_page.weekday_mon")))
                    .child(div().h(cell_size).flex_none())
                    .child(weekday_row(tr!("usage_page.weekday_wed")))
                    .child(div().h(cell_size).flex_none())
                    .child(weekday_row(tr!("usage_page.weekday_fri")))
                    .child(div().h(cell_size).flex_none())
                    .child(div().h(cell_size).flex_none()),
            );

        // One band per calendar month. Weeks stay intact (a column is always
        // Mon–Sun); the band opens on the week that contains the 1st, and a
        // hairline sits in the gap so months don't run together. Labels are
        // not squeezed into a single cell — that wrapped "9月" onto two lines.
        let bands = heatmap_month_bands(&views.weeks);
        let grid = div()
            .flex()
            .items_end()
            .children(bands.iter().enumerate().map(|(band_index, band)| {
                let weeks = div().flex().gap(gap).children(
                    views.weeks[band.start..band.end]
                        .iter()
                        .enumerate()
                        .map(|(offset, week)| {
                            let column_index = band.start + offset;
                            div().flex_none().flex().flex_col().gap(gap).children(
                                week.iter().enumerate().map(|(row_index, cell)| {
                                    let id = SharedString::from(format!(
                                        "usage-heat-{column_index}-{row_index}"
                                    ));
                                    match cell {
                                        Some(cell) => div()
                                            .id(id)
                                            .size(cell_size)
                                            .rounded(px(3.0))
                                            .flex_none()
                                            .bg(heat_level_color(theme, cell.level))
                                            .border_1()
                                            .border_color(gpui::transparent_black())
                                            .hover(|element| {
                                                element.border_color(theme.text_tertiary)
                                            })
                                            .when(
                                                cell.direct > 0 || cell.sessions > 0,
                                                |cell_el| {
                                                    cell_el
                                                        .tooltip(Tooltip::text(usage_day_tooltip(
                                                            cell.date,
                                                            cell.direct,
                                                            cell.total,
                                                            cell.sessions,
                                                        )))
                                                        .tooltip_show_delay(CHART_TOOLTIP_DELAY)
                                                },
                                            ),
                                        None => div()
                                            .id(id)
                                            .size(cell_size)
                                            .rounded(px(3.0))
                                            .flex_none()
                                            .bg(theme.inset),
                                    }
                                }),
                            )
                        }),
                );
                div()
                    .flex()
                    .flex_none()
                    .items_end()
                    .when(band_index > 0, |row| {
                        row.child(
                            div()
                                .w(px(1.0))
                                .h(px(heat_grid_px()))
                                .mx(px(6.0))
                                .flex_none()
                                .bg(theme.text_tertiary),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .min_w(ui_px(36.0))
                            .flex()
                            .flex_col()
                            .gap(label_gap)
                            .child(
                                div()
                                    .h(label_h)
                                    .flex()
                                    .items_end()
                                    .whitespace_nowrap()
                                    .text_size(ui_px(10.0))
                                    .text_color(theme.text_secondary)
                                    .child(band.label()),
                            )
                            .child(weeks),
                    )
            }));

        let legend_cell = |level: u8| {
            div()
                .size(px(10.0))
                .rounded(px(2.5))
                .flex_none()
                .bg(heat_level_color(theme, level))
        };

        div()
            .w_full()
            .px(px(16.0))
            .py(px(14.0))
            .rounded(px(13.0))
            .bg(theme.raised)
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(ui_px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(tr!("usage_page.heatmap_title")),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        div()
                            .text_size(ui_px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(tr!(
                                "usage_page.heatmap_caption",
                                weeks = HEATMAP_WEEKS,
                                active = views.heatmap_active_days
                            )),
                    ),
            )
            .child(div().flex().items_start().child(gutter).child(grid))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_size(ui_px(9.5))
                            .text_color(theme.text_tertiary)
                            .child(tr!("usage_page.legend_less")),
                    )
                    .child(legend_cell(0))
                    .child(legend_cell(1))
                    .child(legend_cell(2))
                    .child(legend_cell(3))
                    .child(legend_cell(4))
                    .child(
                        div()
                            .text_size(ui_px(9.5))
                            .text_color(theme.text_tertiary)
                            .child(tr!("usage_page.legend_more")),
                    ),
            )
    }

    fn render_usage_daily(&self, views: &UsageViews, theme: &Theme) -> Div {
        let single_day = matches!(self.usage_range, UsageRange::Day(_));
        let points: Vec<UsagePlotPoint> = if single_day {
            views
                .hourly
                .iter()
                .map(|hour| UsagePlotPoint {
                    label: format!("{:02}:00", hour.hour),
                    input: hour.input,
                    output: hour.output,
                    cache_write: hour.cache_write,
                    cache_read: hour.cache_read,
                    cost: hour.cost,
                })
                .collect()
        } else {
            views
                .daily
                .iter()
                .map(|day| UsagePlotPoint {
                    label: format_day_label(day.date),
                    input: day.input,
                    output: day.output,
                    cache_write: day.cache_write,
                    cache_read: day.cache_read,
                    cost: day.cost,
                })
                .collect()
        };
        let horizon = points.len() as i64;
        let chart = usage_line_chart(&points, theme);
        let label_step = if single_day {
            4
        } else if horizon > 14 {
            7
        } else {
            2
        };
        let axis = div()
            .flex()
            .mt(px(6.0))
            .children(points.iter().enumerate().map(|(index, point)| {
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .text_size(ui_px(9.0))
                    .text_color(theme.text_tertiary)
                    .child(if index % label_step == 0 {
                        point.label.clone()
                    } else {
                        String::new()
                    })
            }));

        div()
            .w_full()
            .px(px(16.0))
            .py(px(14.0))
            .rounded(px(13.0))
            .bg(theme.raised)
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(ui_px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(if single_day {
                                tr!("usage_page.hourly_title")
                            } else {
                                tr!("usage_page.daily_title")
                            }),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        div()
                            .text_size(ui_px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(if single_day {
                                tr!("usage_page.hourly_caption")
                            } else {
                                tr!("usage_page.daily_caption", days = horizon)
                            }),
                    ),
            )
            .child(chart)
            .child(axis)
            .child(usage_chart_legend(theme))
            .when(single_day && views.hourly_estimated, |card| {
                card.child(
                    div()
                        .text_size(ui_px(10.5))
                        .text_color(theme.text_tertiary)
                        .child(tr!("usage_page.hourly_estimated")),
                )
            })
    }

    fn render_usage_models(&self, views: &UsageViews, theme: &Theme) -> Div {
        let rows: Vec<UsageRankRow> = views
            .models
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let (display, full, icon_path) = match &row.model {
                    Some(model) => match model.split_once('/') {
                        Some((provider, id)) => {
                            (id.to_owned(), model.clone(), model_icon(id, id, provider))
                        }
                        None => (
                            model.clone(),
                            model.clone(),
                            model_icon(model, model, "opencode"),
                        ),
                    },
                    None => (
                        tr!("usage_page.unknown_model").to_owned(),
                        String::new(),
                        "icons/bot.svg",
                    ),
                };
                UsageRankRow {
                    id: format!("usage-model-{index}"),
                    icon_path,
                    // Hover shows the whole `provider/model`: two providers can
                    // serve the same model name, and the bare id cannot tell
                    // them apart.
                    tooltip: if full.is_empty() {
                        display.clone()
                    } else {
                        full
                    },
                    display,
                    count_label: tr!("usage_page.model_sessions", count = row.sessions),
                    total: row.total,
                    cost: row.cost,
                }
            })
            .collect();
        usage_ranking_card(
            tr!("usage_page.models_title"),
            tr!("usage_page.models_caption", count = views.model_count),
            rows,
            theme,
        )
    }

    /// The per-provider ranking: where the store's usage actually went.
    /// Derived from the model strings the scan already carries, so it costs
    /// no extra request and no extra protocol field.
    fn render_usage_providers(&self, views: &UsageViews, theme: &Theme) -> Div {
        let rows: Vec<UsageRankRow> = views
            .providers
            .iter()
            .enumerate()
            .map(|(index, row)| UsageRankRow {
                id: format!("usage-provider-{index}"),
                icon_path: "icons/globe.svg",
                tooltip: row.provider.clone(),
                display: row.provider.clone(),
                count_label: tr!("usage_page.provider_models", count = row.models),
                total: row.total,
                cost: row.cost,
            })
            .collect();
        usage_ranking_card(
            tr!("usage_page.providers_title"),
            tr!("usage_page.providers_caption", count = views.provider_count),
            rows,
            theme,
        )
    }

    /// The per-project ranking: which working directories the store's
    /// sessions actually ran in, largest share first.
    fn render_usage_projects(&self, views: &UsageViews, theme: &Theme) -> Div {
        let rows: Vec<UsageRankRow> = views
            .projects
            .iter()
            .enumerate()
            .map(|(index, row)| UsageRankRow {
                id: format!("usage-project-{index}"),
                icon_path: "icons/folder.svg",
                tooltip: row.directory.clone(),
                display: project_display_name(&row.directory),
                count_label: tr!("usage_page.model_sessions", count = row.sessions),
                total: row.total,
                cost: row.cost,
            })
            .collect();
        usage_ranking_card(
            tr!("usage_page.projects_title"),
            tr!("usage_page.projects_caption", count = views.project_count),
            rows,
            theme,
        )
    }
}

fn usage_day_button(
    id: &'static str,
    icon_path: &'static str,
    enabled: bool,
    delta: i64,
    theme: &Theme,
    cx: &mut Context<Fintwind>,
) -> Stateful<Div> {
    div()
        .id(id)
        .tab_index(0)
        .tab_stop(enabled)
        .size(px(24.0))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(|style| style.border_color(theme.accent))
        .when(enabled, |button| {
            button.hover(|style| style.bg(theme.overlay))
        })
        .when(!enabled, |button| button.opacity(0.35))
        .child(icon(icon_path, 12.0, theme.text_secondary))
        .tooltip(Tooltip::text(tr!(match delta {
            1 => "usage_page.previous_day",
            _ => "usage_page.next_day",
        })))
        .on_activation(cx, move |this, _, cx| {
            if enabled {
                this.shift_usage_day(delta, cx);
            }
        })
}

#[derive(Clone)]
struct UsagePlotPoint {
    label: String,
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
    cost: f64,
}

fn plot_colors(theme: &Theme) -> [Hsla; 5] {
    let colors = if theme.is_dark {
        [0x60a5fa, 0x4ade80, 0xfb923c, 0xc084fc, 0xfb7185]
    } else {
        [0x1d4ed8, 0x15803d, 0xc2410c, 0x7e22ce, 0xbe123c]
    };
    colors.map(|color| rgb(color).into())
}

fn plot_values(point: &UsagePlotPoint) -> [f64; 5] {
    [
        point.input as f64,
        point.output as f64,
        point.cache_write as f64,
        point.cache_read as f64,
        point.cost,
    ]
}

fn usage_line_chart(points: &[UsagePlotPoint], theme: &Theme) -> Div {
    let colors = plot_colors(theme);
    let values: Vec<_> = points.iter().map(plot_values).collect();
    let token_max = values
        .iter()
        .flat_map(|v| v[..4].iter())
        .copied()
        .fold(1.0_f64, f64::max);
    let cost_max = values.iter().map(|v| v[4]).fold(0.0_f64, f64::max);
    let count = points.len();
    let plot = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if count == 0 {
                return;
            }
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            // A subtle area under cache hits mirrors the reference chart
            // without hiding the other four independent traces.
            let mut area = PathBuilder::fill();
            area.move_to(point(
                bounds.origin.x + px(width / (2.0 * count as f32)),
                bounds.origin.y + px(height),
            ));
            for (index, datum) in values.iter().enumerate() {
                let x = (index as f32 + 0.5) * width / count as f32;
                let y = height - (datum[3] / token_max) as f32 * (height - 5.0) - 2.0;
                area.line_to(point(bounds.origin.x + px(x), bounds.origin.y + px(y)));
            }
            area.line_to(point(
                bounds.origin.x + px(width - width / (2.0 * count as f32)),
                bounds.origin.y + px(height),
            ));
            area.close();
            if let Ok(area) = area.build() {
                window.paint_path(area, colors[3].opacity(0.10));
            }
            for series in 0..5 {
                let max = if series == 4 { cost_max } else { token_max };
                let mut path = PathBuilder::stroke(px(2.0));
                for (index, datum) in values.iter().enumerate() {
                    let x = (index as f32 + 0.5) * width / count as f32;
                    let y = height - (datum[series] / max.max(1e-12)) as f32 * (height - 5.0) - 2.0;
                    let position = point(bounds.origin.x + px(x), bounds.origin.y + px(y));
                    if index == 0 {
                        path.move_to(position);
                    } else {
                        path.line_to(position);
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, colors[series]);
                }
            }
        },
    );
    div()
        .h(px(DAILY_CHART_PX + 44.0))
        .relative()
        .child(plot.absolute().inset_0())
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .children(points.iter().enumerate().map(|(index, datum)| {
                    let id = SharedString::from(format!("usage-plot-{index}"));
                    let datum = datum.clone();
                    let values = plot_values(&datum);
                    div()
                        .id(id.clone())
                        .group(id.clone())
                        .tab_index(0)
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .border_1()
                        .border_color(gpui::transparent_black())
                        .focus_visible(|style| style.border_color(theme.accent))
                        .child(
                            div()
                                .absolute()
                                .left(gpui::relative(0.5))
                                .top_0()
                                .h_full()
                                .w(px(1.0))
                                .bg(gpui::transparent_black())
                                .group_hover(id.clone(), |line| line.bg(theme.border_strong)),
                        )
                        .children((0..5).map(|series| {
                            let max = if series == 4 { cost_max } else { token_max };
                            let bottom = (values[series] / max.max(1e-12)) as f32
                                * (DAILY_CHART_PX + 39.0)
                                - 1.5;
                            div()
                                .absolute()
                                .left(gpui::relative(0.5))
                                .ml(px(-3.5))
                                .bottom(px(bottom))
                                .size(px(7.0))
                                .rounded_full()
                                .border_1()
                                .border_color(theme.raised)
                                .bg(colors[series])
                                .opacity(0.0)
                                .group_hover(id.clone(), |dot| dot.opacity(1.0))
                        }))
                        .tooltip(move |_, cx| {
                            cx.new(|_| UsagePlotTooltip {
                                point: datum.clone(),
                            })
                            .into()
                        })
                        .tooltip_show_delay(CHART_TOOLTIP_DELAY)
                })),
        )
}

struct UsagePlotTooltip {
    point: UsagePlotPoint,
}

impl Render for UsagePlotTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let values = plot_values(&self.point);
        let labels = [
            tr!("usage_page.plot_input"),
            tr!("usage_page.plot_output"),
            tr!("usage_page.plot_cache_write"),
            tr!("usage_page.plot_cache_read"),
            tr!("usage_page.plot_cost"),
        ];
        let colors = plot_colors(&theme);
        div().pt(px(4.0)).child(
            div()
                .px(px(13.0))
                .py(px(10.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.raised)
                .shadow_md()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .mb(px(3.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(self.point.label.clone()),
                )
                .children((0..5).map(|index| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .text_size(ui_px(11.0))
                        .text_color(colors[index])
                        .child(div().size(px(7.0)).rounded_full().bg(colors[index]))
                        .child(format!(
                            "{}: {}",
                            labels[index],
                            if index == 4 {
                                format!("${:.6}", values[index])
                            } else {
                                format_grouped_tokens(values[index] as u64)
                            }
                        ))
                })),
        )
    }
}

fn format_grouped_tokens(tokens: u64) -> String {
    tokens
        .to_string()
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|group| std::str::from_utf8(group).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(",")
}

fn usage_chart_legend(theme: &Theme) -> Div {
    let labels = [
        tr!("usage_page.plot_input"),
        tr!("usage_page.plot_output"),
        tr!("usage_page.plot_cache_write"),
        tr!("usage_page.plot_cache_read"),
        tr!("usage_page.plot_cost"),
    ];
    let colors = plot_colors(theme);
    div()
        .flex()
        .flex_wrap()
        .gap(px(12.0))
        .children((0..5).map(|index| {
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .text_size(ui_px(10.0))
                .text_color(theme.text_secondary)
                .child(div().size(px(7.0)).rounded_full().bg(colors[index]))
                .child(labels[index].clone())
        }))
}

/// One row of a usage ranking card. The model, provider, and project rankings
/// differ only in what fills these fields, so they share one renderer; the
/// alternative was three copies of the same bar, tooltip, and focus handling.
struct UsageRankRow {
    id: String,
    icon_path: &'static str,
    /// The label, truncated to fit its column.
    display: String,
    /// Hover text, with the cost appended when there is one.
    tooltip: String,
    /// The trailing count, already localized — "3 个会话" or "2 个模型".
    count_label: String,
    total: u64,
    cost: f64,
}

/// A ranking card: a titled list of rows, each a label, a share bar scaled to
/// the busiest row, and a token total. Rows are keyboard-reachable in the
/// order they are drawn.
fn usage_ranking_card(
    title: String,
    caption: String,
    rows: Vec<UsageRankRow>,
    theme: &Theme,
) -> Div {
    let max_total = rows.first().map(|row| row.total).unwrap_or_default();
    let mut card = div()
        .w_full()
        .px(px(16.0))
        .py(px(14.0))
        .rounded(px(13.0))
        .bg(theme.raised)
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(ui_px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(title),
                )
                .child(div().flex_1().min_w_0())
                .child(
                    div()
                        .text_size(ui_px(10.5))
                        .text_color(theme.text_tertiary)
                        .child(caption),
                ),
        );

    for (index, row) in rows.iter().enumerate() {
        let share = if max_total > 0 {
            (row.total as f32 / max_total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mut tooltip = row.tooltip.clone();
        if row.cost > 0.0 {
            tooltip = format!("{} · {}", tooltip, format_cost(row.cost));
        }
        let last = index + 1 >= rows.len();
        card = card.child(
            div()
                .id(SharedString::from(row.id.clone()))
                .tab_index(0)
                .focus_visible(|style| style.border_color(theme.accent))
                .flex()
                .items_center()
                .gap(px(10.0))
                .py(px(7.0))
                .border_b_1()
                .when(last, |row| row.border_color(gpui::transparent_black()))
                .when(!last, |row| row.border_color(theme.border))
                .child(icon(row.icon_path, 14.0, theme.text_tertiary))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .min_w(px(0.0))
                                        .truncate()
                                        .text_size(ui_px(12.0))
                                        .text_color(theme.text)
                                        .child(SharedString::from(row.display.clone())),
                                )
                                .child(div().flex_1().min_w_0())
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(ui_px(10.0))
                                        .text_color(theme.text_tertiary)
                                        .child(SharedString::from(row.count_label.clone())),
                                ),
                        )
                        .child(
                            div()
                                .h(px(4.0))
                                .w_full()
                                .rounded_full()
                                .bg(theme.overlay_strong)
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(share))
                                        .rounded_full()
                                        .bg(theme.gauge),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(64.0))
                        .flex()
                        .flex_col()
                        .items_end()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_size(ui_px(11.5))
                                .text_color(theme.text_secondary)
                                .child(SharedString::from(format_tokens(row.total))),
                        )
                        .when(row.cost > 0.0, |cost| {
                            cost.child(
                                div()
                                    .text_size(ui_px(9.5))
                                    .text_color(theme.text_tertiary)
                                    .child(SharedString::from(format_cost(row.cost))),
                            )
                        }),
                )
                .tooltip(Tooltip::text(SharedString::from(tooltip))),
        );
    }

    card
}

/// The session KPI's subtitle: active days, plus the sub-agent sessions whose
/// usage the totals absorbed. The second half is only appended when there are
/// any, so an ordinary store keeps the short reading.
fn usage_sessions_subtitle(totals: &UsageTotals) -> String {
    let base = tr!("usage_page.active_days_value", count = totals.active_days);
    if totals.subagent_sessions == 0 {
        return base;
    }
    format!(
        "{} · {}",
        base,
        tr!(
            "usage_page.subagent_sessions_value",
            count = totals.subagent_sessions,
            tokens = format_tokens(totals.subagent_tokens)
        )
    )
}

fn usage_kpi_card(
    theme: &Theme,
    label: impl IntoElement,
    value: impl IntoElement,
    sub: impl IntoElement,
) -> Div {
    div()
        .flex_1()
        .min_w(px(0.0))
        .px(px(14.0))
        .py(px(12.0))
        .rounded(px(13.0))
        .bg(theme.raised)
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .text_size(ui_px(11.0))
                .text_color(theme.text_tertiary)
                .child(label),
        )
        .child(
            div()
                .text_size(ui_px(19.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .truncate()
                .child(value),
        )
        .child(
            div()
                .text_size(ui_px(10.0))
                .text_color(theme.text_tertiary)
                .truncate()
                .child(sub),
        )
}

fn usage_notice(
    theme: &Theme,
    icon_path: &'static str,
    tint: Hsla,
    message: impl IntoElement,
) -> Div {
    div()
        .w_full()
        .px(px(14.0))
        .py(px(11.0))
        .rounded(px(10.0))
        .bg(theme.raised)
        .border_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .gap(px(9.0))
        .child(icon(icon_path, 13.0, tint))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_size(ui_px(11.5))
                .line_height(ui_px(16.0))
                .text_color(theme.text_secondary)
                .child(message),
        )
}

fn usage_retry_button(theme: &Theme, cx: &mut Context<Fintwind>) -> Stateful<Div> {
    div()
        .id("usage-stats-retry")
        .tab_index(0)
        .h(px(26.0))
        .px(px(10.0))
        .rounded(px(7.0))
        .border_1()
        .border_color(theme.border_strong)
        .flex()
        .items_center()
        .cursor_default()
        .text_size(ui_px(11.0))
        .text_color(theme.text_secondary)
        .focus_visible(|style| style.border_color(theme.accent))
        .hover(|element| element.bg(theme.overlay))
        .active(|element| element.bg(theme.overlay_strong))
        .child(tr!("usage_page.retry"))
        .on_activation(cx, |this, _, cx| {
            this.ensure_usage_stats(true, cx);
        })
}

fn usage_empty_card(theme: &Theme, title: String, body: String) -> Div {
    div()
        .w_full()
        .px(px(20.0))
        .py(px(36.0))
        .rounded(px(13.0))
        .bg(theme.raised)
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.0))
        .child(icon("icons/chart-column.svg", 22.0, theme.text_ghost))
        .child(
            div()
                .mt(px(6.0))
                .text_size(ui_px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(title),
        )
        .child(
            div()
                .text_size(ui_px(11.5))
                .text_color(theme.text_tertiary)
                .child(body),
        )
}

fn heat_level_color(theme: &Theme, level: u8) -> Hsla {
    match level {
        0 => theme.inset,
        1 => theme.accent.opacity(0.25),
        2 => theme.accent.opacity(0.45),
        3 => theme.accent.opacity(0.7),
        _ => theme.accent,
    }
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
fn clock_label() -> String {
    Local::now().format("%H:%M").to_string()
}

/// Session message-day shares, or one last-touched share when unsplit.
/// Older scans lacking sub-agent day shares receive only their unallocated
/// remainder here; current scans place child usage on the child's date.
fn entry_day_lanes(entry: &UsageEntry) -> Vec<UsageDayShare> {
    let lanes = match &entry.days {
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
    };
    lanes
}

/// Select only the spend on a local calendar day. Session-level totals cannot
/// answer this for a conversation crossing midnight; its message buckets can.
fn entry_on_day(entry: &UsageEntry, date: NaiveDate) -> Option<UsageEntry> {
    let lanes: Vec<_> = entry_day_lanes(entry)
        .into_iter()
        .filter(|lane| local_date(lane.timestamp) == Some(date))
        .collect();
    let last = lanes.iter().max_by_key(|lane| lane.timestamp)?;
    let child_days: Vec<_> = lanes.iter().filter(|lane| lane.subagent).collect();
    // Model shares span the whole session; scaling them to this day's spend
    // would invent per-model timestamps the scan does not have. Construct the
    // day's entry without cloning message/hour vectors on the UI thread.
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
            child_days.len() as u32
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
        model_lanes: Vec::new(),
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
/// - Sub-agent spend rides the session-level model. The lanes cover only the
///   parent's own messages, so dropping the folded amount would make every
///   model row short by exactly the sessions that delegate the most.
/// - A provider that reported no per-message cost would otherwise show the
///   model as free while the KPI above it charges for it; the session's own
///   estimate then lands on its biggest share instead.
fn entry_model_shares(entry: &UsageEntry) -> Vec<ModelShare> {
    let mut shares: Vec<(Option<String>, u64, f64)> = if entry.model_lanes.is_empty() {
        vec![(
            entry.model.clone(),
            entry.total_tokens(),
            entry.cost.unwrap_or_default(),
        )]
    } else {
        let mut lanes: Vec<(Option<String>, u64, f64)> = entry
            .model_lanes
            .iter()
            .map(|lane| (Some(lane.model.clone()), lane.total, lane.cost))
            .collect();
        if entry.subagent_tokens > 0 {
            lanes.push((entry.model.clone(), entry.subagent_tokens, 0.0));
        }
        if let Some(cost) = entry.cost {
            let remainder = (cost - lanes.iter().map(|lane| lane.2).sum::<f64>()).max(0.0);
            if remainder > 0.0 {
                // A folded sub-agent has no model-level cost lane; book its
                // remaining charge to the parent's named model when possible.
                let recipient = entry
                    .model
                    .as_deref()
                    .and_then(|model| {
                        lanes
                            .iter()
                            .position(|lane| lane.0.as_deref() == Some(model))
                    })
                    .unwrap_or(0);
                if let Some(lane) = lanes.get_mut(recipient) {
                    lane.2 += remainder;
                }
            }
        }
        lanes
    };

    shares.sort_by(|a, b| b.1.cmp(&a.1));
    shares
        .into_iter()
        .enumerate()
        .map(|(index, (model, total, cost))| ModelShare {
            model,
            total,
            cost,
            primary: index == 0,
        })
        .collect()
}

/// Aggregate the scan into the page's view models for `range`. Pure and
/// allocation-light: it runs on the UI thread when a scan lands or the range
/// changes, never per frame.
pub(super) fn build_views(stats: Option<&UsageStats>, range: UsageRange) -> UsageViews {
    let Some(stats) = stats else {
        return UsageViews::default();
    };
    let today = Local::now().date_naive();
    let now = unix_time();
    let selected_day = match range {
        UsageRange::Day(offset) => Some(today - ChronoDuration::days(i64::from(offset))),
        _ => None,
    };
    let bar_horizon = range
        .days()
        .unwrap_or(BAR_HORIZON_DAYS)
        .min(BAR_HORIZON_DAYS);
    let bar_start = selected_day.unwrap_or(today - ChronoDuration::days(bar_horizon - 1));
    let range_cutoff = range
        .days()
        .map(|days| now.saturating_sub(days as u64 * 86_400))
        .unwrap_or(0);

    let mut totals = UsageTotals::default();
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

    for entry in &stats.entries {
        // Totals, rankings, and the active-day count stay gated by the whole
        // session's last touch, not by where its messages landed: a session
        // that began before the window and continued into it spent that usage
        // in the window, and splitting it would need per-message aggregation
        // the scan deliberately does not do for totals.
        let day_entry = selected_day.and_then(|date| entry_on_day(entry, date));
        let in_range = match selected_day {
            Some(_) => day_entry.as_ref(),
            None if entry.timestamp >= range_cutoff => Some(entry),
            None => None,
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
            if selected_day.map_or(lane.timestamp >= range_cutoff, |selected| date == selected) {
                range_days.insert(date);
            }
            let day = days.entry(date).or_insert_with(|| empty_day(date));
            day.total = day.total.saturating_add(lane.total);
            day.direct = day.direct.saturating_add(lane.direct);
            day.input = day.input.saturating_add(lane.input);
            day.output = day.output.saturating_add(lane.output);
            day.cache_read = day.cache_read.saturating_add(lane.cache_read);
            day.cache_write = day.cache_write.saturating_add(lane.cache_write);
            day.cost += lane.cost;
            day.sessions += 1;
        }
        if let Some(selected) = selected_day {
            if entry.hours.is_empty() {
                if entry.total_tokens() > 0 && local_date(entry.timestamp) == Some(selected) {
                    hourly_estimated = true;
                    let hour = local_hour(entry.timestamp) as usize;
                    let bucket = &mut hourly[hour];
                    bucket.input = bucket.input.saturating_add(entry.input_tokens);
                    bucket.output = bucket
                        .output
                        .saturating_add(entry.output_tokens.saturating_add(entry.reasoning_tokens));
                    bucket.cache_read = bucket.cache_read.saturating_add(entry.cache_read_tokens);
                    bucket.cache_write =
                        bucket.cache_write.saturating_add(entry.cache_write_tokens);
                    bucket.cost += entry.cost.unwrap_or_default();
                }
            } else {
                for share in &entry.hours {
                    if local_date(share.timestamp) != Some(selected) {
                        continue;
                    }
                    hourly_estimated |= share.estimated;
                    let bucket = &mut hourly[local_hour(share.timestamp) as usize];
                    bucket.input = bucket.input.saturating_add(share.input);
                    bucket.output = bucket.output.saturating_add(share.output);
                    bucket.cache_read = bucket.cache_read.saturating_add(share.cache_read);
                    bucket.cache_write = bucket.cache_write.saturating_add(share.cache_write);
                    bucket.cost += share.cost;
                }
                // A session summary can contain tokens absent from its
                // message walk (including folded sub-agents). Attribute the
                // unlocated remainder to last activity and label the estimate.
                if local_date(entry.timestamp) == Some(selected) {
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
                    hourly_estimated |= missing_input > 0
                        || missing_output > 0
                        || missing_read > 0
                        || missing_write > 0
                        || missing_cost > 0.0;
                    let bucket = &mut hourly[local_hour(entry.timestamp) as usize];
                    bucket.input = bucket.input.saturating_add(missing_input);
                    bucket.output = bucket.output.saturating_add(missing_output);
                    bucket.cache_read = bucket.cache_read.saturating_add(missing_read);
                    bucket.cache_write = bucket.cache_write.saturating_add(missing_write);
                    bucket.cost += missing_cost;
                }
            }
        }
    }
    totals.active_days = range_days.len() as u32;

    let mut daily = Vec::with_capacity(bar_horizon as usize);
    for offset in 0..bar_horizon {
        let date = bar_start + ChronoDuration::days(offset);
        // The map stays intact: the heatmap below reads the same days.
        let day = days.get(&date).cloned().unwrap_or_else(|| empty_day(date));
        daily.push(day);
    }

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
                total: day.map(|day| day.total).unwrap_or(0),
                direct: day.map(|day| day.direct).unwrap_or(0),
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
    models.truncate(MAX_MODEL_ROWS);

    let mut provider_rows: Vec<UsageProviderRow> = providers
        .into_iter()
        .map(|(provider, (models, total, cost))| UsageProviderRow {
            provider,
            models,
            total,
            cost,
        })
        .collect();
    provider_rows.sort_by(|a, b| b.total.cmp(&a.total));
    let provider_count = provider_rows.len();
    provider_rows.truncate(MAX_MODEL_ROWS);

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
    projects.truncate(MAX_MODEL_ROWS);

    UsageViews {
        totals,
        daily,
        hourly,
        hourly_estimated,
        weeks,
        heatmap_active_days,
        models,
        model_count,
        providers: provider_rows,
        provider_count,
        projects,
        project_count,
    }
}

fn local_hour(timestamp: u64) -> u32 {
    chrono::DateTime::from_timestamp(timestamp as i64, 0)
        .map(|instant| instant.with_timezone(&Local).hour())
        .unwrap_or(0)
}

fn heat_grid_px() -> f32 {
    HEAT_CELL_PX * 7.0 + HEAT_GAP_PX * 6.0
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

/// Tooltip copy for a heatmap cell and a daily bar. `tokens` is the non-cache
/// amount the mark encodes; `cache` is the rest, so a short bar with a huge
/// raw total is explained instead of looking inverted.
fn usage_day_tooltip(date: NaiveDate, direct: u64, total: u64, sessions: u32) -> SharedString {
    SharedString::from(tr!(
        "usage_page.day_tooltip",
        date = format_day_label(date),
        tokens = format_tokens(direct),
        cache = format_tokens(total.saturating_sub(direct)),
        sessions = sessions,
    ))
}

struct HeatMonthBand {
    key: (i32, u32),
    start: usize,
    end: usize,
}

impl HeatMonthBand {
    fn label(&self) -> String {
        NaiveDate::from_ymd_opt(self.key.0, self.key.1, 1)
            .map(month_label)
            .unwrap_or_default()
    }
}

/// Group week columns into calendar months. A week belongs to the month of
/// its 1st when it contains one (so September opens on the column that
/// actually enters September, even if that Monday is still August);
/// otherwise it belongs to its Monday. Weeks are never split.
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
fn format_cost(cost: f64) -> String {
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

fn month_label(date: NaiveDate) -> String {
    if crate::i18n::uses_east_asian_date_format() {
        format!("{}月", date.month())
    } else {
        date.format("%b").to_string()
    }
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
        }
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
            },
            UsageModelLane {
                model: "p/second".to_owned(),
                total: 40,
                cost: 0.0,
            },
            UsageModelLane {
                model: "p/third".to_owned(),
                total: 20,
                cost: 0.0,
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

//! The Usage page: one daemon-side scan over the workspace's OpenCode
//! sessions feeds token-usage statistics — KPI cards, a GitHub-style
//! activity heatmap, daily bars, and a per-model ranking. Frames read only
//! the aggregated views stored on the entity; the raw scan is touched only
//! when it lands or the range changes.

use std::collections::HashMap;

use chrono::{Datelike as _, Duration as ChronoDuration, Local, NaiveDate, TimeZone as _};

use crate::theme::ui_px;
use crate::usage::{cache_hit_percent, format_percent, format_tokens};

use super::*;
use crate::ui::ActivationExt;
use fintwind_client::provider_session::{UsageEntry, UsageStats};
use gpui::relative;

/// Heatmap horizon, in whole weeks ending today.
pub(super) const HEATMAP_WEEKS: i64 = 26;
/// Daily-bar horizon for the unbounded range.
const BAR_HORIZON_DAYS: i64 = 30;
/// Plot height of the daily chart. Bar pixels are this times the day's share
/// of the tallest day, so height tracks non-cache usage linearly. A percentage
/// height inside the flex column is not used: it resolves against whatever
/// the parent flex pass decides, which is not the plot.
const DAILY_CHART_PX: f32 = 96.0;
const HEAT_CELL_PX: f32 = 12.0;
const HEAT_GAP_PX: f32 = 3.0;
const HEAT_LABEL_PX: f32 = 14.0;
const HEAT_LABEL_GAP_PX: f32 = 4.0;
/// Chart scrubbing restarts the tooltip timer on every bar, so the framework
/// default of 500ms feels stuck. Zero still waits one timer tick.
const CHART_TOOLTIP_DELAY: Duration = Duration::from_millis(0);
/// A stored scan older than this is refreshed silently on page open; within
/// it, reopening the page costs no server traversal.
const STALE_AFTER: Duration = Duration::from_secs(300);
/// Model rows drawn before the rest collapse into the header's model count.
const MAX_MODEL_ROWS: usize = 8;

/// What the KPI cards, daily bars, and model ranking aggregate over. The
/// heatmap keeps its own fixed week window, like GitHub's, so a short range
/// does not gut it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum UsageRange {
    #[default]
    All,
    Days30,
    Days7,
}

impl UsageRange {
    pub(super) const ALL: [Self; 3] = [Self::All, Self::Days30, Self::Days7];

    fn label(self) -> String {
        tr!(match self {
            Self::All => "usage_page.range_all",
            Self::Days30 => "usage_page.range_30d",
            Self::Days7 => "usage_page.range_7d",
        })
    }

    fn days(self) -> Option<i64> {
        match self {
            Self::All => None,
            Self::Days30 => Some(30),
            Self::Days7 => Some(7),
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
    /// Input + output + reasoning, the height the bar draws.
    pub direct: u64,
    pub sessions: u32,
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

#[derive(Clone, Debug, Default)]
pub(super) struct UsageViews {
    pub totals: UsageTotals,
    /// Contiguous days ending today, oldest first.
    pub daily: Vec<UsageDay>,
    pub daily_direct_max: u64,
    /// Week columns of seven cells, Monday first; `None` past today.
    pub weeks: Vec<Vec<Option<UsageHeatCell>>>,
    pub heatmap_active_days: u32,
    pub models: Vec<UsageModelRow>,
    pub model_count: usize,
    pub projects: Vec<UsageProjectRow>,
    pub project_count: usize,
}

impl Fintwind {
    /// Load the usage scan if none is stored or the stored one went stale.
    /// The scan covers the whole OpenCode store, so no per-project state
    /// matters here — only freshness does. One blocking traversal runs on
    /// the background executor; a refresh button call bypasses the
    /// staleness window.
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
        if !force
            && self.usage_stats.is_some()
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
        cx.spawn(async move |this, cx| {
            let scanned = cx
                .background_executor()
                .spawn(async move {
                    fintwind_client::persistence::StateStore::remote(daemon)
                        .fetch_usage_stats(binary, directory)
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
                        this.usage_stats = Some(Rc::new(stats));
                        this.rebuild_usage_views(cx);
                    }
                    Err(error) => {
                        // `loaded_at` stays put, so reopening the page
                        // retries instead of waiting out the staleness
                        // window on a failed scan.
                        this.usage_stats_error = Some(error.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn set_usage_range(&mut self, range: UsageRange, cx: &mut Context<Self>) {
        if self.usage_range == range {
            return;
        }
        self.usage_range = range;
        self.rebuild_usage_views(cx);
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
                let views = &self.usage_views;
                column = column
                    .child(self.render_usage_kpis(views, &theme))
                    .child(self.render_usage_heatmap(views, &theme))
                    .child(self.render_usage_daily(views, &theme))
                    .child(self.render_usage_models(views, &theme))
                    .child(self.render_usage_projects(views, &theme));
            }
        }

        column.into_any_element()
    }

    fn render_usage_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut range_buttons = Vec::new();
        for (index, range) in UsageRange::ALL.into_iter().enumerate() {
            let selected = self.usage_range == range;
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

        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(div().flex_1().min_w_0())
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
                tr!("usage_page.active_days_value", count = totals.active_days),
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
        let horizon = views.daily.len() as i64;
        let label_step = if horizon > 14 {
            7
        } else if horizon > 7 {
            5
        } else {
            3
        };
        // Padding instead of a flex gap, so the pointer never leaves a bar
        // while scrubbing — a gap restarted the tooltip between columns.
        let chart = div().h(px(DAILY_CHART_PX)).flex().items_end().children(
            views.daily.iter().enumerate().map(|(index, day)| {
                let id = SharedString::from(format!("usage-bar-{index}"));
                let height = daily_bar_height(day.direct, views.daily_direct_max);
                div()
                    .id(id.clone())
                    // The whole column is the hit target. Hovering only the
                    // bar left the space above a short bar dead, and each bar
                    // restarted the 500ms tooltip delay.
                    .group(id.clone())
                    .tab_index(0)
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .focus_visible(|style| style.border_color(theme.accent))
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .px(px(2.0))
                    .flex()
                    .flex_col()
                    .justify_end()
                    .rounded(px(3.0))
                    .hover(|column| column.bg(theme.overlay))
                    .tooltip(Tooltip::text(usage_day_tooltip(
                        day.date,
                        day.direct,
                        day.total,
                        day.sessions,
                    )))
                    .tooltip_show_delay(CHART_TOOLTIP_DELAY)
                    .child(
                        div()
                            .w_full()
                            .h(px(height))
                            .rounded(px(3.0))
                            .flex_none()
                            .bg(if day.direct > 0 {
                                theme.accent
                            } else {
                                theme.overlay_strong
                            })
                            .group_hover(id, |bar| bar.bg(theme.text_tertiary)),
                    )
            }),
        );
        let today = views.daily.last().map(|day| day.date);
        let axis = div()
            .flex()
            .mt(px(6.0))
            .children(views.daily.iter().enumerate().map(|(index, day)| {
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .px(px(2.0))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .whitespace_nowrap()
                    .line_height(ui_px(12.0))
                    .text_size(ui_px(9.0))
                    .text_color(if Some(day.date) == today {
                        theme.text_secondary
                    } else {
                        theme.text_ghost
                    })
                    .child(SharedString::from(
                        (index % label_step == 0)
                            .then(|| format_day_label(day.date))
                            .unwrap_or_default(),
                    ))
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
                            .child(tr!("usage_page.daily_title")),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        div()
                            .text_size(ui_px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(tr!("usage_page.daily_caption", days = horizon)),
                    ),
            )
            .child(chart)
            .child(axis)
    }

    fn render_usage_models(&self, views: &UsageViews, theme: &Theme) -> Div {
        let max_total = views
            .models
            .first()
            .map(|row| row.total)
            .unwrap_or_default();
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
                            .child(tr!("usage_page.models_title")),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        div()
                            .text_size(ui_px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(tr!("usage_page.models_caption", count = views.model_count)),
                    ),
            );

        for (index, row) in views.models.iter().enumerate() {
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
            let share = if max_total > 0 {
                (row.total as f32 / max_total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut tooltip = if full.is_empty() {
                display.clone()
            } else {
                full.clone()
            };
            if row.cost > 0.0 {
                tooltip = format!("{} · {}", tooltip, format_cost(row.cost));
            }
            card = card.child(
                div()
                    .id(SharedString::from(format!("usage-model-{index}")))
                    .tab_index(0)
                    .focus_visible(|style| style.border_color(theme.accent))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .py(px(7.0))
                    .border_b_1()
                    .when(index + 1 >= views.models.len(), |row| {
                        row.border_color(gpui::transparent_black())
                    })
                    .when(index + 1 < views.models.len(), |row| {
                        row.border_color(theme.border)
                    })
                    .child(icon(icon_path, 14.0, theme.text_tertiary))
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
                                            .child(SharedString::from(display)),
                                    )
                                    .child(div().flex_1().min_w_0())
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(ui_px(10.0))
                                            .text_color(theme.text_tertiary)
                                            .child(tr!(
                                                "usage_page.model_sessions",
                                                count = row.sessions
                                            )),
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

    /// The per-project ranking: which working directories the store's
    /// sessions actually ran in, largest share first.
    fn render_usage_projects(&self, views: &UsageViews, theme: &Theme) -> Div {
        let max_total = views
            .projects
            .first()
            .map(|row| row.total)
            .unwrap_or_default();
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
                            .child(tr!("usage_page.projects_title")),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        div()
                            .text_size(ui_px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(tr!(
                                "usage_page.projects_caption",
                                count = views.project_count
                            )),
                    ),
            );

        for (index, row) in views.projects.iter().enumerate() {
            let share = if max_total > 0 {
                (row.total as f32 / max_total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut tooltip = row.directory.clone();
            if row.cost > 0.0 {
                tooltip = format!("{} · {}", tooltip, format_cost(row.cost));
            }
            card = card.child(
                div()
                    .id(SharedString::from(format!("usage-project-{index}")))
                    .tab_index(0)
                    .focus_visible(|style| style.border_color(theme.accent))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .py(px(7.0))
                    .border_b_1()
                    .when(index + 1 >= views.projects.len(), |row| {
                        row.border_color(gpui::transparent_black())
                    })
                    .when(index + 1 < views.projects.len(), |row| {
                        row.border_color(theme.border)
                    })
                    .child(icon("icons/folder.svg", 14.0, theme.text_tertiary))
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
                                            .child(SharedString::from(project_display_name(
                                                &row.directory,
                                            ))),
                                    )
                                    .child(div().flex_1().min_w_0())
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(ui_px(10.0))
                                            .text_color(theme.text_tertiary)
                                            .child(tr!(
                                                "usage_page.model_sessions",
                                                count = row.sessions
                                            )),
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
    }
}

fn local_date(timestamp: u64) -> Option<NaiveDate> {
    Local
        .timestamp_opt(timestamp as i64, 0)
        .single()
        .map(|time| time.date_naive())
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
    let bar_horizon = range
        .days()
        .unwrap_or(BAR_HORIZON_DAYS)
        .min(BAR_HORIZON_DAYS);
    let bar_start = today - ChronoDuration::days(bar_horizon - 1);
    let range_cutoff = range
        .days()
        .map(|days| now.saturating_sub(days as u64 * 86_400))
        .unwrap_or(0);

    let mut totals = UsageTotals::default();
    // Day buckets are built from every entry: the heatmap shows its fixed
    // 26-week window however narrow the selected range is. The range only
    // gates what the totals, daily bars, and rankings count.
    let mut days: HashMap<NaiveDate, UsageDay> = HashMap::new();
    // Model rows key on the raw `provider/id` string, except a session the
    // server recorded no model for — it folds into the unknown-model row so
    // the ranking's session counts add up to the KPI.
    let mut models: HashMap<Option<String>, (u32, u64, f64, u64)> = HashMap::new();
    // Projects key on a normalized directory (separators + case), so a
    // server that records `E:\work\x` and `e:/work/x` as the same project
    // ranks as one; the first-seen spelling is what gets displayed.
    let mut projects: HashMap<String, (String, u32, u64, f64)> = HashMap::new();
    let mut range_days: std::collections::HashSet<NaiveDate> = std::collections::HashSet::new();

    for entry in &stats.entries {
        let date = local_date(entry.timestamp);
        if entry.timestamp >= range_cutoff {
            totals.add(entry);
            if let Some(date) = date {
                range_days.insert(date);
            }
            let row = models
                .entry(entry.model.clone())
                .or_insert((0, 0, 0.0, entry.timestamp));
            row.0 += 1;
            row.1 = row.1.saturating_add(entry.total_tokens());
            row.2 += entry.cost.unwrap_or_default();
            row.3 = row.3.max(entry.timestamp);
            if let Some(directory) = &entry.directory {
                let key = directory.replace('\\', "/").to_lowercase();
                let row = projects
                    .entry(key)
                    .or_insert_with(|| (directory.clone(), 0, 0, 0.0));
                row.1 += 1;
                row.2 = row.2.saturating_add(entry.total_tokens());
                row.3 += entry.cost.unwrap_or_default();
            }
        }
        let Some(date) = date else {
            continue;
        };
        let day = days.entry(date).or_insert_with(|| empty_day(date));
        day.total = day.total.saturating_add(entry.total_tokens());
        day.direct = day.direct.saturating_add(
            entry
                .input_tokens
                .saturating_add(entry.output_tokens)
                .saturating_add(entry.reasoning_tokens),
        );
        day.sessions += 1;
    }
    totals.active_days = range_days.len() as u32;

    let mut daily = Vec::with_capacity(bar_horizon as usize);
    let mut daily_direct_max = 0u64;
    for offset in 0..bar_horizon {
        let date = bar_start + ChronoDuration::days(offset);
        // The map stays intact: the heatmap below reads the same days.
        let day = days.get(&date).cloned().unwrap_or_else(|| empty_day(date));
        daily_direct_max = daily_direct_max.max(day.direct);
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
        daily_direct_max,
        weeks,
        heatmap_active_days,
        models,
        model_count,
        projects,
        project_count,
    }
}

/// Pixel height of one daily bar. Zero days stay a 2px baseline; any real
/// day is at least that tall, then linear in `direct` up to the plot height.
/// Cache is not part of `direct` — the chart title excludes it, and folding
/// it in made a cache-heavy day look shorter than a busier one.
fn daily_bar_height(direct: u64, max_direct: u64) -> f32 {
    if direct == 0 || max_direct == 0 {
        return 2.0;
    }
    let fraction = ((direct as f64 / max_direct as f64) as f32).clamp(0.0, 1.0);
    (fraction * DAILY_CHART_PX).max(2.0)
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

    /// Bar pixels follow non-cache tokens. A day whose raw total is larger
    /// only because of cache must stay shorter than a day with more direct
    /// usage, and the busiest direct day reaches the top heatmap swatch.
    #[test]
    fn bar_height_follows_non_cache_usage_not_cache_inflated_total() {
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
        assert_eq!(views.daily_direct_max, direct_day.direct);
        assert!(
            daily_bar_height(direct_day.direct, views.daily_direct_max)
                > daily_bar_height(cache_day.direct, views.daily_direct_max)
        );
        assert_eq!(
            daily_bar_height(direct_day.direct, views.daily_direct_max),
            DAILY_CHART_PX
        );

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

    #[test]
    fn daily_bar_height_grows_with_usage_and_floors_quiet_days() {
        assert_eq!(daily_bar_height(0, 100), 2.0);
        assert_eq!(daily_bar_height(100, 0), 2.0);
        let half = daily_bar_height(50, 100);
        let full = daily_bar_height(100, 100);
        assert!((half - DAILY_CHART_PX / 2.0).abs() < 0.01);
        assert_eq!(full, DAILY_CHART_PX);
        assert!(full > half);
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

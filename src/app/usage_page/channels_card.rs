//! Bottom row of the usage dashboard:
//! - Left: Channel distribution bars with striped tracks, saturated gradient
//!   bars, and vertical pill end-caps (Models / Providers / Projects / Token composition).
//! - Right: Cohort-retention-style 6-week Mon..Sun activity matrix, switchable
//!   to Ely's 26-week CalendarHeatmap.

use ely_gpui_component::{
    charts::CalendarHeatmap,
    forms::{Choice, Select},
    theme::ControlSize,
    typography::tabular,
};
use gpui::{Bounds, PathBuilder, canvas, fill, hsla, point, size};

use super::*;

#[derive(Clone)]
struct ChannelBarRow {
    id: String,
    label: String,
    detail: String,
    value_text: String,
    tooltip: String,
    share: f64,
    color: Hsla,
}

impl Fintwind {
    pub(super) fn render_usage_channels(
        &self,
        views: &UsageViews,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let dimension = self.usage_channel_dimension;
        let colors = dashboard_series_colors(theme);
        let totals = &views.totals;
        let total_tokens = totals.total();

        let rows: Vec<ChannelBarRow> = match dimension {
            UsageChannelDimension::Models => {
                let total_basis = total_tokens as f64;
                views
                    .models
                    .iter()
                    .take(5)
                    .enumerate()
                    .map(|(ix, row)| {
                        let unknown = tr!("usage_page.unknown_model");
                        let (short, qualified) = match &row.model {
                            Some(m) => match m.split_once('/') {
                                Some((_, id)) => (id.to_owned(), m.clone()),
                                None => (m.clone(), m.clone()),
                            },
                            None => (unknown.clone(), unknown),
                        };
                        let val = row.total as f64;
                        let share = if total_basis > 0.0 {
                            (val / total_basis * 100.0).clamp(0.0, 100.0)
                        } else {
                            0.0
                        };
                        let value_text = format_tokens(row.total);
                        let sessions_text = tr!("usage_page.model_sessions", count = row.sessions);
                        let tooltip = if row.cost > 0.0 {
                            format!(
                                "{qualified} · {} ({share:.1}%) · {sessions_text} · {}",
                                format_tokens(row.total),
                                format_cost(row.cost)
                            )
                        } else {
                            format!(
                                "{qualified} · {} ({share:.1}%) · {sessions_text}",
                                format_tokens(row.total)
                            )
                        };
                        ChannelBarRow {
                            id: format!("model-{ix}"),
                            label: short,
                            detail: format!("{share:.0}% · {sessions_text}"),
                            value_text,
                            tooltip,
                            share,
                            color: colors[ix % colors.len()],
                        }
                    })
                    .collect()
            }
            UsageChannelDimension::Providers => {
                let total_basis = total_tokens as f64;
                views
                    .providers
                    .iter()
                    .take(5)
                    .enumerate()
                    .map(|(ix, row)| {
                        let val = row.total as f64;
                        let share = if total_basis > 0.0 {
                            (val / total_basis * 100.0).clamp(0.0, 100.0)
                        } else {
                            0.0
                        };
                        let value_text = format_tokens(row.total);
                        let models_text = tr!("usage_page.provider_models", count = row.models);
                        let tooltip = if row.cost > 0.0 {
                            format!(
                                "{} · {} ({share:.1}%) · {models_text} · {}",
                                row.provider,
                                format_tokens(row.total),
                                format_cost(row.cost)
                            )
                        } else {
                            format!(
                                "{} · {} ({share:.1}%) · {models_text}",
                                row.provider,
                                format_tokens(row.total)
                            )
                        };
                        ChannelBarRow {
                            id: format!("provider-{ix}"),
                            label: row.provider.clone(),
                            detail: format!("{share:.0}% · {models_text}"),
                            value_text,
                            tooltip,
                            share,
                            color: colors[ix % colors.len()],
                        }
                    })
                    .collect()
            }
            UsageChannelDimension::Projects => {
                let total_basis = total_tokens as f64;
                views
                    .projects
                    .iter()
                    .take(5)
                    .enumerate()
                    .map(|(ix, row)| {
                        let val = row.total as f64;
                        let share = if total_basis > 0.0 {
                            (val / total_basis * 100.0).clamp(0.0, 100.0)
                        } else {
                            0.0
                        };
                        let value_text = format_tokens(row.total);
                        let sessions_text = tr!("usage_page.model_sessions", count = row.sessions);
                        let tooltip = if row.cost > 0.0 {
                            format!(
                                "{} · {} ({share:.1}%) · {sessions_text} · {}",
                                row.directory,
                                format_tokens(row.total),
                                format_cost(row.cost)
                            )
                        } else {
                            format!(
                                "{} · {} ({share:.1}%) · {sessions_text}",
                                row.directory,
                                format_tokens(row.total)
                            )
                        };
                        ChannelBarRow {
                            id: format!("project-{ix}"),
                            label: project_display_name(&row.directory),
                            detail: format!("{share:.0}% · {sessions_text}"),
                            value_text,
                            tooltip,
                            share,
                            color: colors[ix % colors.len()],
                        }
                    })
                    .collect()
            }
            UsageChannelDimension::Composition => {
                let reasoning_known = matches!(self.usage_range, UsageRange::All);
                let items = [
                    (
                        tr!("usage_page.plot_cache_read"),
                        totals.cache_read,
                        colors[2],
                    ),
                    (tr!("usage_page.tokens_input"), totals.input, colors[1]),
                    (
                        if reasoning_known {
                            tr!("usage_page.tokens_output")
                        } else {
                            tr!("usage_page.kpi_generated_tokens")
                        },
                        if reasoning_known {
                            totals.output
                        } else {
                            totals.generated()
                        },
                        colors[0],
                    ),
                    (
                        tr!("usage_page.plot_cache_write"),
                        totals.cache_write,
                        colors[3],
                    ),
                    (
                        tr!("usage_page.flow_reasoning"),
                        if reasoning_known { totals.reasoning } else { 0 },
                        colors[4],
                    ),
                ];
                items
                    .into_iter()
                    .enumerate()
                    .filter(|(ix, (_, value, _))| *ix != 4 || (reasoning_known && *value > 0))
                    .map(|(ix, (label, value, color))| {
                        let share = if total_tokens > 0 {
                            (value as f64 / total_tokens as f64 * 100.0).clamp(0.0, 100.0)
                        } else {
                            0.0
                        };
                        ChannelBarRow {
                            id: format!("lane-{ix}"),
                            label: label.clone(),
                            detail: format!("{share:.1}%"),
                            value_text: format_tokens(value),
                            tooltip: format!("{label} · {} ({share:.1}%)", format_tokens(value)),
                            share,
                            color,
                        }
                    })
                    .collect()
            }
        };

        let (title, subtitle) = match dimension {
            UsageChannelDimension::Models => (
                tr!("usage_page.models_title"),
                format!(
                    "{} · {}",
                    tr!("usage_page.models_caption", count = views.model_count),
                    format_tokens(total_tokens)
                ),
            ),
            UsageChannelDimension::Providers => (
                tr!("usage_page.providers_title"),
                format!(
                    "{} · {}",
                    tr!("usage_page.providers_caption", count = views.provider_count),
                    format_tokens(total_tokens)
                ),
            ),
            UsageChannelDimension::Projects => (
                tr!("usage_page.projects_title"),
                format!(
                    "{} · {}",
                    tr!("usage_page.projects_caption", count = views.project_count),
                    format_tokens(total_tokens)
                ),
            ),
            UsageChannelDimension::Composition => (
                tr!("usage_page.flow_composition"),
                format!("{} tokens", format_tokens(total_tokens)),
            ),
        };

        let weak = cx.entity().downgrade();
        let selector = Select::new(
            "usage-channel-dimension",
            [
                Choice::new("models", tr!("usage_page.models_title")),
                Choice::new("providers", tr!("usage_page.providers_title")),
                Choice::new("projects", tr!("usage_page.projects_title")),
                Choice::new("composition", tr!("usage_page.flow_composition")),
            ],
        )
        .size(ControlSize::Sm)
        .selected(dimension.key())
        .on_change(move |value, _, cx| {
            let next = UsageChannelDimension::from_key(value);
            let _ = weak.update(cx, |this, cx| {
                if this.usage_channel_dimension != next {
                    this.usage_channel_dimension = next;
                    cx.notify();
                }
            });
        });

        let header = div()
            .flex()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(ui_px(15.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(ui_px(12.0))
                            .text_color(theme.text_tertiary)
                            .truncate()
                            .child(subtitle),
                    ),
            )
            .child(div().flex_none().w(px(128.0)).child(selector));

        let mut list = div().flex().flex_col().gap_3();
        for row in rows {
            list = list.child(render_striped_channel_row(&row, theme));
        }

        usage_card(theme)
            .h_full()
            .p_5()
            .gap_4()
            .child(header)
            .child(list)
    }

    pub(super) fn render_usage_activity_card(
        &self,
        views: &UsageViews,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let view_mode = self.usage_activity_view;
        let weak = cx.entity().downgrade();
        let selector = Select::new(
            "usage-activity-view",
            [
                Choice::new("matrix", tr!("usage_page.matrix_weekly")),
                Choice::new("heatmap", tr!("usage_page.matrix_heatmap")),
            ],
        )
        .size(ControlSize::Sm)
        .selected(view_mode.key())
        .on_change(move |value, _, cx| {
            let next = UsageActivityView::from_key(value);
            let _ = weak.update(cx, |this, cx| {
                if this.usage_activity_view != next {
                    this.usage_activity_view = next;
                    cx.notify();
                }
            });
        });

        let (title, subtitle) = match view_mode {
            UsageActivityView::WeeklyMatrix => (
                tr!("usage_page.matrix_title"),
                tr!(
                    "usage_page.matrix_caption",
                    weeks = MATRIX_WEEKS,
                    active = views.matrix_active_days
                ),
            ),
            UsageActivityView::CalendarHeatmap => (
                tr!("usage_page.heatmap_title"),
                tr!(
                    "usage_page.heatmap_caption",
                    weeks = HEATMAP_WEEKS,
                    active = views.heatmap_active_days
                ),
            ),
        };

        let header = div()
            .flex()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(ui_px(15.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(ui_px(12.0))
                            .text_color(theme.text_tertiary)
                            .truncate()
                            .child(subtitle),
                    ),
            )
            .child(div().flex_none().w(px(116.0)).child(selector));

        let body = match view_mode {
            UsageActivityView::WeeklyMatrix => render_weekly_matrix(views, theme),
            UsageActivityView::CalendarHeatmap => render_calendar_heatmap(views),
        };

        usage_card(theme)
            .h_full()
            .p_5()
            .gap_4()
            .child(header)
            .child(body)
    }
}

fn render_striped_channel_row(row: &ChannelBarRow, theme: &Theme) -> Stateful<Div> {
    let share_frac = (row.share / 100.0).clamp(0.0, 1.0) as f32;
    let color = row.color;
    let track_bg = theme.inset;
    let stripe_color = theme.border.opacity(if theme.is_dark { 0.45 } else { 0.65 });
    let cap_border = theme.composer;
    let has_value = share_frac > 0.0;

    let bar_canvas = canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            let w = f32::from(bounds.size.width);
            let h = f32::from(bounds.size.height);
            if w <= 2.0 || h <= 2.0 {
                return;
            }
            let track_top = 2.0;
            let track_h = (h - 4.0).max(8.0);
            let track_bounds = Bounds::new(
                bounds.origin + point(px(0.0), px(track_top)),
                size(px(w), px(track_h)),
            );
            // Track background.
            window.paint_quad(fill(track_bounds, track_bg).corner_radii(px(7.0)));

            // Diagonal hatched stripes across the empty track.
            let stripe_step = 11.0f32;
            let stripe_w = 4.0f32;
            let mut sx = -track_h;
            while sx < w + track_h {
                let x0 = sx.max(0.0);
                let x1 = (sx + track_h).min(w);
                if x1 > x0 {
                    let mut stripe = PathBuilder::fill();
                    let y_top = f32::from(bounds.top()) + track_top;
                    let y_bot = y_top + track_h;
                    let left_px = f32::from(bounds.left());
                    let top_left_x = (sx + track_h).clamp(0.0, w);
                    let top_right_x = (sx + track_h + stripe_w).clamp(0.0, w);
                    let bot_right_x = (sx + stripe_w).clamp(0.0, w);
                    let bot_left_x = sx.clamp(0.0, w);
                    stripe.move_to(point(px(left_px + top_left_x), px(y_top)));
                    stripe.line_to(point(px(left_px + top_right_x), px(y_top)));
                    stripe.line_to(point(px(left_px + bot_right_x), px(y_bot)));
                    stripe.line_to(point(px(left_px + bot_left_x), px(y_bot)));
                    stripe.close();
                    if let Ok(path) = stripe.build() {
                        window.paint_path(path, stripe_color);
                    }
                }
                sx += stripe_step;
            }

            if has_value {
                let fill_w = (w * share_frac).max(8.0).min(w);
                let fill_bounds = Bounds::new(
                    bounds.origin + point(px(0.0), px(track_top)),
                    size(px(fill_w), px(track_h)),
                );
                // Two-layer subtle gradient feel: deep base + lighter tail overlay.
                window.paint_quad(fill(fill_bounds, color).corner_radii(px(7.0)));
                let tail_w = (fill_w * 0.35).min(fill_w);
                if tail_w > 2.0 {
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                bounds.origin + point(px(fill_w - tail_w), px(track_top)),
                                size(px(tail_w), px(track_h)),
                            ),
                            hsla(0.0, 0.0, 1.0, 0.14),
                        )
                        .corner_radii(px(6.0)),
                    );
                }
                // Crisp vertical end-cap pill extending slightly past top/bottom of track.
                let cap_w = 9.0f32;
                let cap_x = (fill_w - cap_w * 0.6).clamp(0.0, (w - cap_w).max(0.0));
                let cap_bounds = Bounds::new(
                    bounds.origin + point(px(cap_x), px(0.0)),
                    size(px(cap_w), px(h)),
                );
                window.paint_quad(
                    fill(cap_bounds, color)
                        .corner_radii(px(4.0))
                        .border_widths(px(1.2))
                        .border_color(cap_border),
                );
            }
        },
    )
    .size_full();

    div()
        .id(SharedString::from(format!("usage-channel-{}", row.id)))
        .tab_index(0)
        .rounded(px(8.0))
        .px_1p5()
        .py_1()
        .flex()
        .flex_col()
        .gap_1p5()
        .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
        .hover(|style| style.bg(theme.overlay))
        .tooltip(Tooltip::text(row.tooltip.clone()))
        .child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .child(
                            div()
                                .truncate()
                                .text_size(ui_px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(row.label.clone()),
                        )
                        .child(
                            tabular(div())
                                .flex_none()
                                .text_size(ui_px(11.5))
                                .text_color(theme.text_tertiary)
                                .child(row.detail.clone()),
                        ),
                )
                .child(
                    tabular(div())
                        .flex_none()
                        .text_size(ui_px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(row.value_text.clone()),
                ),
        )
        .child(div().w_full().h(px(26.0)).child(bar_canvas))
}

fn render_weekly_matrix(views: &UsageViews, theme: &Theme) -> Div {
    let weekdays = [
        tr!("usage_page.weekday_mon"),
        tr!("usage_page.weekday_tue"),
        tr!("usage_page.weekday_wed"),
        tr!("usage_page.weekday_thu"),
        tr!("usage_page.weekday_fri"),
        tr!("usage_page.weekday_sat"),
        tr!("usage_page.weekday_sun"),
    ];
    let accent = dashboard_series_colors(theme)[0];
    let peak = views
        .weekly_matrix
        .iter()
        .flat_map(|row| row.cells.iter().flatten())
        .map(|cell| cell.direct)
        .max()
        .unwrap_or(0);

    let mut table = div().flex().flex_col().gap(px(5.0));

    // Weekday column headers.
    let mut head = div().flex().items_center().gap(px(5.0));
    head = head.child(div().w(px(92.0)).flex_none());
    for day in weekdays {
        head = head.child(
            div()
                .flex_1()
                .min_w_0()
                .text_center()
                .text_size(ui_px(11.0))
                .text_color(theme.text_tertiary)
                .child(day),
        );
    }
    table = table.child(head);

    for (row_ix, week) in views.weekly_matrix.iter().enumerate() {
        let mut row = div().flex().items_center().gap(px(5.0));
        row = row.child(
            div()
                .w(px(92.0))
                .flex_none()
                .flex()
                .items_baseline()
                .justify_between()
                .gap_1()
                .pr_1()
                .child(
                    div()
                        .truncate()
                        .text_size(ui_px(11.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_secondary)
                        .child(week.label.clone()),
                )
                .child(
                    tabular(div())
                        .flex_none()
                        .text_size(ui_px(10.5))
                        .text_color(theme.text_tertiary)
                        .child(format_tokens(week.direct_total)),
                ),
        );

        for (col_ix, cell) in week.cells.iter().enumerate() {
            let cell_el = match cell {
                None => div()
                    .flex_1()
                    .min_w_0()
                    .h(px(28.0))
                    .rounded(px(5.0))
                    .bg(theme.inset.opacity(0.45))
                    .into_any_element(),
                Some(cell) if cell.direct == 0 => div()
                    .id(SharedString::from(format!(
                        "usage-matrix-cell-{row_ix}-{col_ix}"
                    )))
                    .flex_1()
                    .min_w_0()
                    .h(px(28.0))
                    .rounded(px(5.0))
                    .bg(theme.inset)
                    .flex()
                    .items_center()
                    .justify_center()
                    .tooltip(Tooltip::text(format!(
                        "{} · 0",
                        format_day_label(cell.date)
                    )))
                    .child(
                        div()
                            .text_size(ui_px(10.5))
                            .text_color(theme.text_ghost)
                            .child("—"),
                    )
                    .into_any_element(),
                Some(cell) => {
                    let ratio = if peak > 0 {
                        (cell.direct as f32 / peak as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let alpha = 0.16 + 0.78 * ratio.powf(0.75);
                    let high_contrast = alpha >= 0.56;
                    let text_color = if high_contrast {
                        rgb(0xffffff).into()
                    } else {
                        theme.text
                    };
                    let tip = tr!(
                        "usage_page.day_tooltip",
                        date = cell.date.to_string(),
                        tokens = format_tokens(cell.direct),
                        cache = format_tokens(cell.total.saturating_sub(cell.direct)),
                        sessions = cell.sessions
                    );
                    div()
                        .id(SharedString::from(format!(
                            "usage-matrix-cell-{row_ix}-{col_ix}"
                        )))
                        .flex_1()
                        .min_w_0()
                        .h(px(28.0))
                        .rounded(px(5.0))
                        .bg(accent.opacity(alpha))
                        .flex()
                        .items_center()
                        .justify_center()
                        .px_1()
                        .tooltip(Tooltip::text(tip))
                        .child(
                            tabular(div())
                                .truncate()
                                .text_size(ui_px(10.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(text_color)
                                .child(format_tokens(cell.direct)),
                        )
                        .into_any_element()
                }
            };
            row = row.child(cell_el);
        }
        table = table.child(row);
    }

    table
}

fn render_calendar_heatmap(views: &UsageViews) -> Div {
    let mut cells = views.weeks.iter().flatten().flatten();
    let Some(first) = cells.next() else {
        return div();
    };
    let Ok(start) = first.date.to_string().parse() else {
        return div();
    };
    let counts = std::iter::once(first)
        .chain(cells)
        .map(|cell| cell.direct as f64);
    div().w_full().child(
        CalendarHeatmap::new("usage-activity", start, counts)
            .h_48()
            .format(|value| format_tokens(value as u64)),
    )
}

//! Interactive dual-series trend area chart matching the analytics reference:
//! smooth monotone spline, translucent area fill, previous-period comparison
//! curve, dashed horizontal & vertical gridlines, and hover highlight column
//! with floating tooltip.

use ely_gpui_component::{
    primitives::{Icon, IconName},
    theme::IconSize,
    typography::tabular,
};
use gpui::{
    Bounds, HoverListenerMode, PathBuilder, Point, canvas, fill, point, size,
};

use crate::*;
use fintwind_ui::ui::ActivationExt;

const PLOT_HEIGHT: f32 = 264.0;
const GUTTER_LEFT: f32 = 52.0;
const GUTTER_RIGHT: f32 = 14.0;
const GUTTER_TOP: f32 = 16.0;
const GUTTER_BOTTOM: f32 = 26.0;

#[derive(Default)]
struct TrendPlotState {
    bounds: Bounds<Pixels>,
    hover: Option<usize>,
}

#[derive(Clone)]
struct TrendSeriesPoint {
    label: String,
    value: f64,
    prev_value: Option<f64>,
    sessions: u32,
}

impl UsagePage {
    pub(crate) fn render_usage_trend_chart(
        &self,
        views: &UsageViews,
        window: &mut Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let single_day = matches!(self.range, UsageRange::Day(_));
        let metric = self.chart_metric;
        let palette = dashboard_series_colors(theme);
        let series_color = palette[metric.palette_index()];
        let prev_color = series_color.opacity(0.36);

        let current_hour_cap = if matches!(self.range, UsageRange::Day(0)) {
            Some(Local::now().hour() as usize)
        } else {
            None
        };

        let points: Vec<TrendSeriesPoint> = if single_day {
            views
                .hourly
                .iter()
                .enumerate()
                .filter(|(ix, _)| current_hour_cap.is_none_or(|cap| *ix <= cap))
                .map(|(ix, hour)| {
                    let prev = views.prev_hourly.get(ix);
                    TrendSeriesPoint {
                        label: format!("{:02}:00", hour.hour),
                        value: metric.hourly_value(hour),
                        prev_value: prev.map(|p| metric.hourly_value(p)),
                        sessions: 0,
                    }
                })
                .collect()
        } else {
            views
                .daily
                .iter()
                .enumerate()
                .map(|(ix, day)| {
                    let prev = views.prev_daily.get(ix);
                    TrendSeriesPoint {
                        label: format_day_label(day.date),
                        value: metric.daily_value(day),
                        prev_value: prev.map(|p| metric.daily_value(p)),
                        sessions: day.sessions,
                    }
                })
                .collect()
        };

        let has_prev = !matches!(self.range, UsageRange::All)
            && points.iter().any(|p| p.prev_value.is_some());
        let comparing = self.compare_previous && has_prev;

        let current_summary = metric.format_total(&views.totals);
        let prev_summary = views
            .prev_totals
            .as_ref()
            .map(|prev| metric.format_total(prev))
            .unwrap_or_default();

        let span_label = if single_day {
            let offset = match self.range {
                UsageRange::Day(d) => i64::from(d),
                _ => 0,
            };
            let day = Local::now().date_naive() - ChronoDuration::days(offset);
            format!(
                "{} · {}",
                tr!("usage_page.hourly_granularity"),
                format_day_label(day)
            )
        } else if let (Some(first), Some(last)) = (views.daily.first(), views.daily.last()) {
            format!(
                "{} · {} – {}",
                tr!("usage_page.daily_granularity"),
                format_day_label(first.date),
                format_day_label(last.date)
            )
        } else {
            tr!("usage_page.daily_granularity")
        };

        let legend_swatch = |bg: Hsla, border: Hsla| {
            div()
                .size(px(10.0))
                .rounded(px(3.0))
                .bg(bg)
                .border_1()
                .border_color(border)
        };

        let mut subtitle_row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .text_size(ui_px(12.0))
            .text_color(theme.text_secondary)
            .child(div().text_color(theme.text_tertiary).child(span_label))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(legend_swatch(series_color, series_color.opacity(0.6)))
                    .child(format!("{} · {current_summary}", tr!("usage_page.this_period"))),
            );

        if comparing {
            subtitle_row = subtitle_row.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(legend_swatch(prev_color, series_color.opacity(0.25)))
                    .child(format!(
                        "{} · {prev_summary}",
                        tr!("usage_page.previous_period")
                    )),
            );
        }

        let compare_control = has_prev.then(|| {
            let label = if comparing {
                tr!("usage_page.compare_previous")
            } else {
                tr!("usage_page.compare_off")
            };
            div()
                .id("usage-trend-compare")
                .role(gpui::Role::Button)
                .tab_index(0)
                .h(px(28.0))
                .px_2p5()
                .rounded(px(8.0))
                .border_1()
                .border_color(if comparing {
                    theme.border_strong
                } else {
                    theme.border
                })
                .bg(if comparing {
                    theme.surface
                } else {
                    theme.composer
                })
                .flex()
                .items_center()
                .gap_1p5()
                .cursor_pointer()
                .focus_visible(|style| style.border_color(theme.accent_focus))
                .hover(|style| style.bg(theme.overlay))
                .child(
                    Icon::new(IconName::ArrowDownUp)
                        .size(IconSize::Xs)
                        .color(theme.text_secondary),
                )
                .child(
                    div()
                        .text_size(ui_px(12.0))
                        .text_color(theme.text_secondary)
                        .child(label),
                )
                .on_activation(cx, |this, _, cx| {
                    this.compare_previous = !this.compare_previous;
                    cx.notify();
                })
        });

        let header = div()
            .flex()
            .flex_wrap()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui_px(15.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(metric.title()),
                    )
                    .child(subtitle_row),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(compare_control),
            );

        let plot = render_trend_plot(
            &points,
            metric,
            comparing,
            series_color,
            prev_color,
            single_day,
            &self.flow_selection,
            window,
            theme,
            cx,
        );

        usage_card(theme)
            .p_5()
            .gap_4()
            .child(header)
            .child(plot)
            .when(single_day && views.hourly_estimated, |card| {
                card.child(
                    div()
                        .text_size(ui_px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(tr!("usage_page.hourly_estimated")),
                )
            })
    }
}

#[allow(clippy::too_many_arguments)]
fn render_trend_plot(
    points: &[TrendSeriesPoint],
    metric: UsageChartMetric,
    comparing: bool,
    series_color: Hsla,
    prev_color: Hsla,
    single_day: bool,
    selection: &Option<UsageFlowSelection>,
    window: &mut Window,
    theme: &Theme,
    cx: &mut Context<UsagePage>,
) -> Stateful<Div> {
    let state = window.use_keyed_state("usage-trend-plot", cx, |_, _| TrendPlotState::default());
    let (bounds, hover) = {
        let s = state.read(cx);
        (s.bounds, s.hover.filter(|ix| *ix < points.len()))
    };

    let selected_ix = selection.as_ref().and_then(|sel| match (sel, single_day) {
        (UsageFlowSelection::Hour(ix), true) if *ix < points.len() => Some(*ix),
        (UsageFlowSelection::Day(ix), false) if *ix < points.len() => Some(*ix),
        _ => None,
    });
    let active_ix = hover.or(selected_ix);

    let max_raw = points
        .iter()
        .map(|p| {
            let prev = if comparing {
                p.prev_value.unwrap_or(0.0)
            } else {
                0.0
            };
            p.value.max(prev)
        })
        .fold(0.0f64, f64::max);

    let (y_max, y_ticks) = if matches!(metric, UsageChartMetric::CacheHit) {
        (100.0, vec![0.0, 25.0, 50.0, 75.0, 100.0])
    } else {
        nice_y_ticks(max_raw)
    };

    let plot_w = (f32::from(bounds.size.width) - GUTTER_LEFT - GUTTER_RIGHT).max(0.0);
    let plot_h = (PLOT_HEIGHT - GUTTER_TOP - GUTTER_BOTTOM).max(0.0);

    let x_for = |ix: usize| -> f32 {
        if points.len() <= 1 {
            GUTTER_LEFT + plot_w * 0.5
        } else {
            GUTTER_LEFT + plot_w * (ix as f32 / (points.len() - 1) as f32)
        }
    };
    let y_for = |val: f64| -> f32 {
        let ratio = if y_max > 0.0 {
            (val / y_max).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        GUTTER_TOP + plot_h * (1.0 - ratio)
    };

    let current_pts: Vec<(f32, f32)> = points
        .iter()
        .enumerate()
        .map(|(ix, p)| (x_for(ix), y_for(p.value)))
        .collect();
    let prev_pts: Vec<(f32, f32)> = if comparing {
        points
            .iter()
            .enumerate()
            .filter_map(|(ix, p)| p.prev_value.map(|v| (x_for(ix), y_for(v))))
            .collect()
    } else {
        Vec::new()
    };

    let label_every = if points.len() <= 8 {
        1
    } else if single_day {
        4
    } else {
        (points.len() / 6).max(1)
    };
    let x_tick_indices: Vec<usize> = (0..points.len())
        .filter(|ix| *ix % label_every == 0 || *ix + 1 == points.len())
        .collect();

    let grid_color = theme.border.opacity(0.65);
    let baseline_color = theme.border_strong;
    let surface_fill = theme.composer;
    let base_y = GUTTER_TOP + plot_h;
    let x_positions_for_grid: Vec<f32> = x_tick_indices.iter().map(|ix| x_for(*ix)).collect();
    let y_positions_for_grid: Vec<f32> = y_ticks.iter().map(|v| y_for(*v)).collect();
    let active_pt = active_ix.and_then(|ix| current_pts.get(ix).copied());
    let last_pt = current_pts.last().copied();
    let paint_current_pts = current_pts.clone();

    let paint_layer = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let origin = bounds.origin;
            if plot_w <= 1.0 || plot_h <= 1.0 {
                return;
            }
            // Horizontal gridlines (bottom is solid baseline, upper are dashed).
            for (ix, &gy) in y_positions_for_grid.iter().enumerate() {
                if ix == 0 {
                    window.paint_quad(fill(
                        Bounds::new(
                            origin + point(px(GUTTER_LEFT), px(gy)),
                            size(px(plot_w), px(1.0)),
                        ),
                        baseline_color,
                    ));
                } else {
                    paint_dashed_hline(
                        window,
                        origin.x + px(GUTTER_LEFT),
                        origin.x + px(GUTTER_LEFT + plot_w),
                        origin.y + px(gy),
                        grid_color,
                    );
                }
            }
            // Vertical dashed gridlines at x ticks.
            for &gx in &x_positions_for_grid {
                paint_dashed_vline(
                    window,
                    origin.x + px(gx),
                    origin.y + px(GUTTER_TOP),
                    origin.y + px(base_y),
                    grid_color,
                );
            }
            // Active highlight band & baseline bar.
            if let Some((ax, ay)) = active_pt {
                let band_w = 24.0f32.min(plot_w * 0.12).max(12.0);
                let band_left = (ax - band_w * 0.5).max(GUTTER_LEFT);
                let band_right = (ax + band_w * 0.5).min(GUTTER_LEFT + plot_w);
                let bw = (band_right - band_left).max(0.0);
                if bw > 0.0 && base_y > ay {
                    window.paint_quad(fill(
                        Bounds::new(
                            origin + point(px(band_left), px(ay)),
                            size(px(bw), px(base_y - ay)),
                        ),
                        series_color.opacity(0.09),
                    ));
                    window.paint_quad(fill(
                        Bounds::new(
                            origin + point(px(band_left), px(base_y - 1.5)),
                            size(px(bw), px(2.5)),
                        ),
                        series_color,
                    ));
                }
            }
            // Previous period comparison line.
            if prev_pts.len() > 1 {
                let mut path = PathBuilder::stroke(px(1.5));
                trace_smooth(&mut path, &prev_pts, origin);
                if let Ok(built) = path.build() {
                    window.paint_path(built, prev_color);
                }
            }
            // Current period area fill + line.
            if paint_current_pts.len() > 1 {
                let mut area = PathBuilder::fill();
                trace_smooth(&mut area, &paint_current_pts, origin);
                if let (Some(last), Some(first)) =
                    (paint_current_pts.last(), paint_current_pts.first())
                {
                    area.line_to(origin + point(px(last.0), px(base_y)));
                    area.line_to(origin + point(px(first.0), px(base_y)));
                    area.close();
                    if let Ok(built) = area.build() {
                        window.paint_path(built, series_color.opacity(0.14));
                    }
                }
                let mut stroke = PathBuilder::stroke(px(2.0));
                trace_smooth(&mut stroke, &paint_current_pts, origin);
                if let Ok(built) = stroke.build() {
                    window.paint_path(built, series_color);
                }
            }
            // End-of-line ring marker.
            if let Some((lx, ly)) = last_pt {
                paint_ring(
                    origin + point(px(lx), px(ly)),
                    px(4.0),
                    surface_fill,
                    series_color,
                    px(2.0),
                    window,
                );
            }
            // Active hovered/selected ring marker.
            if let Some((ax, ay)) = active_pt {
                paint_ring(
                    origin + point(px(ax), px(ay)),
                    px(5.0),
                    surface_fill,
                    series_color,
                    px(2.5),
                    window,
                );
            }
        },
    )
    .absolute()
    .inset_0();

    // Y-axis tick labels.
    let mut labels_layer = div().absolute().inset_0();
    for &val in &y_ticks {
        let gy = y_for(val);
        labels_layer = labels_layer.child(
            div()
                .absolute()
                .left_0()
                .top(px(gy - 8.0))
                .w(px(GUTTER_LEFT - 8.0))
                .text_right()
                .child(
                    tabular(div())
                        .text_size(ui_px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(metric.format_axis(val)),
                ),
        );
    }

    // X-axis tick labels (avoid overlapping final label with preceding one).
    let mut last_x_drawn: Option<f32> = None;
    for &ix in &x_tick_indices {
        let gx = x_for(ix);
        if let Some(prev_x) = last_x_drawn
            && (gx - prev_x) < 42.0
        {
            continue;
        }
        last_x_drawn = Some(gx);
        let align_right = ix + 1 == points.len() && points.len() > 1;
        let align_left = ix == 0 && points.len() > 1;
        let label = points[ix].label.clone();
        labels_layer = labels_layer.child(
            div()
                .absolute()
                .left(px(if align_right {
                    (gx - 56.0).max(GUTTER_LEFT)
                } else if align_left {
                    gx
                } else {
                    gx - 28.0
                }))
                .top(px(base_y + 7.0))
                .w(px(56.0))
                .when(align_right, |el| el.text_right())
                .when(!align_right && !align_left, |el| el.text_center())
                .truncate()
                .text_size(ui_px(11.0))
                .text_color(theme.text_tertiary)
                .child(label),
        );
    }

    // Floating dark pill tooltip above the hovered/selected point.
    let tooltip_el = active_ix.and_then(|ix| {
        let point_data = points.get(ix)?;
        let (ax, ay) = current_pts.get(ix).copied()?;
        let mut text = format!(
            "{} · {}",
            point_data.label,
            metric.format_point(point_data.value)
        );
        if comparing && let Some(prev) = point_data.prev_value {
            text.push_str(&format!(" ({} {})", tr!("usage_page.previous_period"), metric.format_point(prev)));
        }
        if point_data.sessions > 0 {
            text.push_str(&format!(
                " · {}",
                tr!("usage_page.model_sessions", count = point_data.sessions)
            ));
        }
        let pill_bg = if theme.is_dark {
            theme.raised
        } else {
            rgb(0x1f1f22).into()
        };
        let pill_fg: Hsla = if theme.is_dark {
            theme.text
        } else {
            rgb(0xffffff).into()
        };
        let tip_y = (ay - 34.0).max(2.0);
        let align_right = ax > GUTTER_LEFT + plot_w * 0.65;
        let align_left = ax < GUTTER_LEFT + plot_w * 0.25;
        let left_px = if align_right {
            (ax - 180.0).max(GUTTER_LEFT)
        } else if align_left {
            ax.max(GUTTER_LEFT)
        } else {
            (ax - 90.0).max(GUTTER_LEFT)
        };
        Some(
            div()
                .absolute()
                .left(px(left_px))
                .top(px(tip_y))
                .px_2p5()
                .py_1()
                .rounded(px(7.0))
                .bg(pill_bg)
                .border_1()
                .border_color(if theme.is_dark {
                    theme.border_strong
                } else {
                    gpui::transparent_black()
                })
                .shadow_md()
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().size(px(6.0)).rounded_full().bg(series_color))
                .child(
                    tabular(div())
                        .whitespace_nowrap()
                        .text_size(ui_px(11.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(pill_fg)
                        .child(text),
                ),
        )
    });

    let count = points.len();
    let move_state = state.clone();
    let leave_state = state.clone();
    let click_state = state.clone();
    let measure_state = state;

    div()
        .id("usage-trend-plot")
        .tab_index(0)
        .relative()
        .w_full()
        .h(px(PLOT_HEIGHT))
        .rounded(px(8.0))
        .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
        .on_mouse_move(move |event: &MouseMoveEvent, _, cx| {
            let bounds = move_state.read(cx).bounds;
            let width = (f32::from(bounds.size.width) - GUTTER_LEFT - GUTTER_RIGHT).max(1.0);
            let rel_x = f32::from(event.position.x - bounds.left()) - GUTTER_LEFT;
            let next = if count == 0 || rel_x < -8.0 || rel_x > width + 8.0 {
                None
            } else if count == 1 {
                Some(0)
            } else {
                let ratio = (rel_x / width).clamp(0.0, 1.0);
                Some(((ratio * (count - 1) as f32).round() as usize).min(count - 1))
            };
            move_state.update(cx, |s, cx| {
                if s.hover != next {
                    s.hover = next;
                    cx.notify();
                }
            });
        })
        .hover_listener_mode(HoverListenerMode::InputModalityIndependent)
        .on_hover(move |inside, _, cx| {
            if !*inside {
                leave_state.update(cx, |s, cx| {
                    if s.hover.is_some() {
                        s.hover = None;
                        cx.notify();
                    }
                });
            }
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(ix) = click_state.read(cx).hover {
                let next = if single_day {
                    UsageFlowSelection::Hour(ix)
                } else {
                    UsageFlowSelection::Day(ix)
                };
                if this.flow_selection.as_ref() == Some(&next) {
                    this.flow_selection = None;
                } else {
                    this.flow_selection = Some(next);
                }
                cx.notify();
            }
        }))
        .child(paint_layer)
        .child(labels_layer)
        .children(tooltip_el)
        .child(
            canvas(
                move |bounds, _, cx| {
                    measure_state.update(cx, |s, cx| {
                        if s.bounds != bounds {
                            s.bounds = bounds;
                            cx.notify();
                        }
                    });
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
}

fn nice_y_ticks(max_val: f64) -> (f64, Vec<f64>) {
    if max_val <= 0.0 {
        return (4.0, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
    }
    let raw_step = max_val / 4.0;
    let mag = 10.0f64.powf(raw_step.log10().floor());
    let norm = raw_step / mag;
    let nice_norm = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 2.5 {
        2.5
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    };
    let step = nice_norm * mag;
    let top = step * 4.0;
    let ticks = (0..=4).map(|i| step * i as f64).collect();
    (top, ticks)
}

fn paint_dashed_hline(
    window: &mut Window,
    x0: Pixels,
    x1: Pixels,
    y: Pixels,
    color: Hsla,
) {
    let mut x = f32::from(x0);
    let end = f32::from(x1);
    while x < end {
        let seg = (end - x).min(3.5);
        window.paint_quad(fill(
            Bounds::new(point(px(x), y), size(px(seg), px(1.0))),
            color,
        ));
        x += 7.0;
    }
}

fn paint_dashed_vline(
    window: &mut Window,
    x: Pixels,
    y0: Pixels,
    y1: Pixels,
    color: Hsla,
) {
    let mut y = f32::from(y0);
    let end = f32::from(y1);
    while y < end {
        let seg = (end - y).min(3.0);
        window.paint_quad(fill(
            Bounds::new(point(x, px(y)), size(px(1.0), px(seg))),
            color,
        ));
        y += 6.5;
    }
}

fn paint_ring(
    center: Point<Pixels>,
    radius: Pixels,
    inside: Hsla,
    edge: Hsla,
    width: Pixels,
    window: &mut Window,
) {
    let bounds = Bounds::new(
        center - point(radius, radius),
        size(radius * 2.0, radius * 2.0),
    );
    window.paint_quad(
        fill(bounds, inside)
            .corner_radii(radius)
            .border_widths(width)
            .border_color(edge),
    );
}

/// Fritsch-Carlson monotone cubic spline so the curve stays smooth and never
/// dips below the baseline.
fn trace_smooth(path: &mut PathBuilder, points: &[(f32, f32)], origin: Point<Pixels>) {
    let Some(first) = points.first() else {
        return;
    };
    path.move_to(origin + point(px(first.0), px(first.1)));
    if points.len() < 3 {
        for next in &points[1..] {
            path.line_to(origin + point(px(next.0), px(next.1)));
        }
        return;
    }
    let slopes = monotone_tangents(points);
    for ix in 0..points.len() - 1 {
        let ((x0, y0), (x1, y1)) = (points[ix], points[ix + 1]);
        let third = (x1 - x0) / 3.0;
        path.cubic_bezier_to(
            origin + point(px(x1), px(y1)),
            origin + point(px(x0 + third), px(y0 + slopes[ix] * third)),
            origin + point(px(x1 - third), px(y1 - slopes[ix + 1] * third)),
        );
    }
}

fn monotone_tangents(points: &[(f32, f32)]) -> Vec<f32> {
    let count = points.len();
    if count < 2 {
        return vec![0.0; count];
    }
    let slopes: Vec<f32> = points
        .windows(2)
        .map(|pair| {
            let dx = pair[1].0 - pair[0].0;
            if dx.abs() < 1e-4 {
                0.0
            } else {
                let s = (pair[1].1 - pair[0].1) / dx;
                if s.is_finite() { s } else { 0.0 }
            }
        })
        .collect();
    let mut tangents: Vec<f32> = (0..count)
        .map(|ix| match ix {
            0 => slopes[0],
            last if last == count - 1 => slopes[count - 2],
            ix if slopes[ix - 1] * slopes[ix] <= 0.0 => 0.0,
            ix => (slopes[ix - 1] + slopes[ix]) * 0.5,
        })
        .collect();
    for (ix, slope) in slopes.iter().enumerate() {
        if *slope == 0.0 {
            tangents[ix] = 0.0;
            tangents[ix + 1] = 0.0;
            continue;
        }
        let (a, b) = (tangents[ix] / slope, tangents[ix + 1] / slope);
        let length = a.hypot(b);
        if length > 3.0 {
            tangents[ix] = 3.0 * a / length * slope;
            tangents[ix + 1] = 3.0 * b / length * slope;
        }
    }
    tangents
}

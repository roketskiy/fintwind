//! Aggregate model shares and aggregate token composition meet at a common
//! total. The two sides are independent: the picture never claims a
//! per-model token-category breakdown that the scan did not provide.

use super::*;
use gpui::{canvas, point, size, PathBuilder, Point};

const FLOW_MIN_HEIGHT: f32 = 282.0;
const FLOW_TOP: f32 = 44.0;
const FLOW_BOTTOM: f32 = 20.0;
const FLOW_GAP: f32 = 8.0;
const FLOW_LABEL_GAP: f32 = 4.0;
const FLOW_LABEL_GUTTER: f32 = 20.0;
const FLOW_LEFT_NODE: f32 = 0.22;
const FLOW_RIGHT_NODE: f32 = 0.78;

#[derive(Default)]
struct FlowWidth {
    width: f32,
    bounds: Option<Bounds<Pixels>>,
}

#[derive(Clone)]
struct FlowLane {
    title: String,
    detail: String,
    value: u64,
    color: Hsla,
    selection: Option<UsageFlowSelection>,
    known: bool,
}

#[derive(Clone, Copy)]
struct Band {
    start: f32,
    height: f32,
    center: f32,
}

fn bands(lanes: &[FlowLane], total: u64, slots: usize, height: f32) -> (Vec<Band>, f32) {
    let gaps = slots.saturating_sub(1) as f32 * FLOW_GAP;
    let available = (height - FLOW_TOP - FLOW_BOTTOM - gaps).max(0.0);
    let mut y = FLOW_TOP;
    let bands = lanes.iter().map(|lane| {
        let height = if total == 0 { 0.0 } else { available * (lane.value as f64 / total as f64) as f32 };
        let band = Band { start: y, height, center: y + height / 2.0 };
        y += height + FLOW_GAP;
        band
    }).collect();
    (bands, available)
}

fn label_positions(bands: &[Band], height: f32, label_height: f32) -> Vec<f32> {
    let first = FLOW_TOP + label_height / 2.0;
    let last = height - FLOW_BOTTOM - label_height / 2.0;
    let mut ys: Vec<_> = bands.iter().map(|band| band.center).collect();
    if let Some(top) = ys.first_mut() {
        *top = top.max(first);
    }
    let spacing = label_height + FLOW_LABEL_GAP;
    for i in 1..ys.len() {
        ys[i] = ys[i].max(ys[i - 1] + spacing);
    }
    if let Some(bottom) = ys.last_mut() {
        *bottom = bottom.min(last);
    }
    for i in (0..ys.len().saturating_sub(1)).rev() {
        ys[i] = ys[i].min(ys[i + 1] - spacing);
    }
    ys
}

fn flow_band(
    window: &mut Window,
    x0: Pixels,
    y0: Pixels,
    x1: Pixels,
    y1: Pixels,
    h: Pixels,
    color: Hsla,
) {
    if h <= px(0.0) { return; }
    let mid = (x0 + x1) / 2.0;
    let mut path = PathBuilder::fill();
    path.move_to(point(x0, y0));
    path.cubic_bezier_to(point(x1, y1), point(mid, y0), point(mid, y1));
    path.line_to(point(x1, y1 + h));
    path.cubic_bezier_to(point(x0, y0 + h), point(mid, y1 + h), point(mid, y0 + h));
    path.close();
    if let Ok(path) = path.build() { window.paint_path(path, color); }
}

fn label_leader(
    window: &mut Window,
    node: Point<Pixels>,
    label: Point<Pixels>,
    color: Hsla,
) {
    let mut path = PathBuilder::stroke(px(1.0));
    let midpoint = (node.x + label.x) / 2.0;
    path.move_to(node);
    path.cubic_bezier_to(label, point(midpoint, node.y), point(midpoint, label.y));
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn flow_canvas(left: Vec<FlowLane>, right: Vec<FlowLane>, total: u64, label_height: f32, selected: Option<UsageFlowSelection>, trunk_color: Hsla) -> impl IntoElement {
    canvas(|_, _, _| (), move |bounds, _, window, _| {
        let height = f32::from(bounds.size.height);
        let slots = left.len().max(right.len());
        let (left_bands, left_height) = bands(&left, total, slots, height);
        let (right_bands, right_height) = bands(&right, total, slots, height);
        let left_labels = label_positions(&left_bands, height, label_height);
        let right_labels = label_positions(&right_bands, height, label_height);
        let center_x = bounds.center().x;
        let left_x = bounds.left() + bounds.size.width * FLOW_LEFT_NODE;
        let right_x = bounds.left() + bounds.size.width * FLOW_RIGHT_NODE;
        let left_trunk = FLOW_TOP + (height - FLOW_TOP - FLOW_BOTTOM - left_height) / 2.0;
        let right_trunk = FLOW_TOP + (height - FLOW_TOP - FLOW_BOTTOM - right_height) / 2.0;
        let mut offset = 0.0;
        for ((lane, band), label_y) in left.iter().zip(left_bands).zip(left_labels) {
            let opacity = match (&selected, &lane.selection) {
                (Some(UsageFlowSelection::Model(i)), Some(UsageFlowSelection::Model(j))) if i == j => 0.50,
                (Some(UsageFlowSelection::Model(_)), _) => 0.06,
                _ => 0.20,
            };
            flow_band(window, left_x, bounds.top() + px(band.start), center_x - px(4.0), bounds.top() + px(left_trunk + offset), px(band.height), lane.color.opacity(opacity));
            if band.height > 0.0 { window.paint_quad(fill(Bounds::new(point(left_x - px(6.0), bounds.top() + px(band.start)), size(px(6.0), px(band.height))), lane.color)); }
            if band.height > 0.0 {
                label_leader(window, point(left_x - px(6.0), bounds.top() + px(band.center)), point(left_x - px(FLOW_LABEL_GUTTER), bounds.top() + px(label_y)), lane.color.opacity(0.55));
            }
            offset += band.height;
        }
        offset = 0.0;
        for ((lane, band), label_y) in right.iter().zip(right_bands).zip(right_labels) {
            let opacity = match (&selected, &lane.selection) {
                (Some(UsageFlowSelection::Lane(i)), Some(UsageFlowSelection::Lane(j))) if i == j => 0.50,
                (Some(UsageFlowSelection::Lane(_)), _) => 0.06,
                _ => 0.20,
            };
            flow_band(window, center_x + px(4.0), bounds.top() + px(right_trunk + offset), right_x, bounds.top() + px(band.start), px(band.height), lane.color.opacity(opacity));
            if band.height > 0.0 { window.paint_quad(fill(Bounds::new(point(right_x, bounds.top() + px(band.start)), size(px(6.0), px(band.height))), lane.color)); }
            if band.height > 0.0 {
                label_leader(window, point(right_x + px(6.0), bounds.top() + px(band.center)), point(right_x + px(FLOW_LABEL_GUTTER), bounds.top() + px(label_y)), lane.color.opacity(0.55));
            }
            offset += band.height;
        }
        if total > 0 {
            window.paint_quad(fill(Bounds::new(point(center_x - px(4.0), bounds.top() + px(left_trunk)), size(px(8.0), px(left_height))), trunk_color));
        }
    }).absolute().inset_0()
}

fn lane_button(id: String, lane: &FlowLane, bounds: Bounds<Pixels>, right: bool, selected: bool, theme: &Theme, cx: &mut Context<Fintwind>) -> Stateful<Div> {
    let selection = lane.selection.clone();
    let key_selection = selection.clone();
    let title = lane.title.clone();
    let detail = lane.detail.clone();
    let mut text = div().flex().flex_col().min_w_0().text_size(ui_px(11.0));
    if right { text = text.items_start(); } else { text = text.items_end().text_right(); }
    text = text.child(div().w_full().truncate().text_color(theme.text).child(SharedString::from(title)))
        .child(div().w_full().truncate().text_color(theme.text_tertiary).child(SharedString::from(detail)));
    div().id(SharedString::from(id)).absolute().left(bounds.left()).top(bounds.top())
        .w(bounds.size.width).h(bounds.size.height).rounded(px(5.0)).px_2()
        .flex().items_center()
        .border_1().border_color(if selected { theme.border_strong } else { gpui::transparent_black() })
        .when(selected, |element| element.bg(theme.overlay))
        .hover(|style| style.bg(theme.overlay))
        .when(selection.is_some(), |element| {
            element.tab_index(0).focus_visible(|style| style.border_color(theme.accent_focus))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.usage_flow_selection = selection.clone(); cx.notify();
                }))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.usage_flow_selection = key_selection.clone(); cx.notify(); cx.stop_propagation();
                    }
                }))
        }).child(text.flex_1())
}

impl Fintwind {
    pub(super) fn render_usage_flow(&self, views: &UsageViews, window: &mut Window, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let totals = views.totals;
        let total = totals.total();
        let colors = plot_colors(theme);
        let mut left: Vec<FlowLane> = views.models.iter().take(MAX_MODEL_ROWS).enumerate().map(|(i, row)| {
            let unknown = tr!("usage_page.unknown_model");
            let title = row.model.as_deref().and_then(|s| s.rsplit('/').next()).unwrap_or(&unknown);
            FlowLane { title: title.to_owned(), detail: flow_detail(row.total, total), value: row.total, color: colors[i % colors.len()], selection: Some(UsageFlowSelection::Model(i)), known: true }
        }).collect();
        let represented = left.iter().fold(0u64, |sum, row| sum.saturating_add(row.value));
        if represented < total {
            left.push(FlowLane { title: tr!("usage_page.flow_other"), detail: flow_detail(total - represented, total), value: total - represented, color: theme.text_ghost, selection: None, known: true });
        }
        let values = [totals.cache_read, totals.input, totals.output, totals.cache_write, totals.reasoning];
        let reasoning_known = matches!(self.usage_range, UsageRange::All);
        let labels = [tr!("usage_page.plot_cache_read"), tr!("usage_page.tokens_input"), if reasoning_known { tr!("usage_page.tokens_output") } else { tr!("usage_page.kpi_generated_tokens") }, tr!("usage_page.plot_cache_write"), tr!("usage_page.flow_reasoning")];
        let palette = [theme.text_tertiary, colors[0], colors[1], colors[4], colors[2]];
        let right: Vec<FlowLane> = values.into_iter().enumerate().map(|(i, value)| {
            let known = i != 4 || reasoning_known;
            FlowLane { title: labels[i].clone(), detail: if known { flow_detail(value, total) } else { "—".into() }, value, color: palette[i], selection: known.then_some(UsageFlowSelection::Lane(i)), known }
        }).collect();
        let width = window.use_keyed_state("usage-flow-width", cx, |_, _| FlowWidth::default());
        let w = width.read(cx).width;
        let lx = w * FLOW_LEFT_NODE;
        let rx = w * FLOW_RIGHT_NODE;
        let slots = left.len().max(right.len());
        let label_height = f32::from(ui_px(32.0)).max(32.0);
        let height = FLOW_MIN_HEIGHT.max(
            FLOW_TOP + FLOW_BOTTOM + slots as f32 * label_height
                + slots.saturating_sub(1) as f32 * FLOW_LABEL_GAP,
        );
        let (lb, _) = bands(&left, total, slots, height);
        let (rb, _) = bands(&right, total, slots, height);
        let left_positions = label_positions(&lb, height, label_height);
        let right_positions = label_positions(&rb, height, label_height);
        let selected = self.usage_flow_selection.clone();
        let hit_width = width.clone();
        let hit_left = left.clone();
        let hit_right = right.clone();
        let mut field = div().id("usage-flow-field").tab_group().relative().w_full().h(px(height))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                let Some(bounds) = hit_width.read(cx).bounds else { return; };
                let width = f32::from(bounds.size.width);
                let x = f32::from(event.position.x - bounds.left());
                let y = f32::from(event.position.y - bounds.top());
                let center = width * 0.5;
                let (lanes, is_left, start, end) = if x >= width * FLOW_LEFT_NODE && x < center - 4.0 {
                    (&hit_left, true, width * FLOW_LEFT_NODE, center - 4.0)
                } else if x > center + 4.0 && x <= width * FLOW_RIGHT_NODE {
                    (&hit_right, false, center + 4.0, width * FLOW_RIGHT_NODE)
                } else { return; };
                let (pieces, band_height) = bands(lanes, total, hit_left.len().max(hit_right.len()), height);
                let trunk = FLOW_TOP + (height - FLOW_TOP - FLOW_BOTTOM - band_height) / 2.0;
                let t = ((x - start) / (end - start)).clamp(0.0, 1.0);
                let blend = t * t * (3.0 - 2.0 * t);
                let mut offset = 0.0;
                for (lane, band) in lanes.iter().zip(pieces) {
                    let from = if is_left { band.start } else { trunk + offset };
                    let to = if is_left { trunk + offset } else { band.start };
                    let top = from + (to - from) * blend;
                    if y >= top && y <= top + band.height {
                        if let Some(selection) = &lane.selection { this.usage_flow_selection = Some(selection.clone()); cx.notify(); }
                        return;
                    }
                    offset += band.height;
                }
            }));
        field = field.child(flow_canvas(left.clone(), right.clone(), total, label_height, selected.clone(), theme.border_strong));
        field = field.child(div().absolute().left(px(8.0)).top(px(12.0)).text_size(ui_px(11.0)).text_color(theme.text_tertiary).child(tr!("usage_page.models_title")))
            .child(div().absolute().left(gpui::relative(0.4)).w(gpui::relative(0.2)).top(px(8.0)).text_center().text_size(ui_px(11.0)).text_color(theme.text_ghost).child(tr!("usage_page.flow_total")))
            .child(div().absolute().left(gpui::relative(FLOW_RIGHT_NODE)).right(px(8.0)).top(px(12.0)).truncate().text_size(ui_px(11.0)).text_color(theme.text_tertiary).child(tr!("usage_page.flow_composition")));
        if w > 0.0 {
            field = field.child(div().id("usage-flow-total").absolute().left(px(w * 0.5 - 12.0))
                .top(px(FLOW_TOP)).w(px(24.0)).h(px(height - FLOW_TOP - FLOW_BOTTOM))
                .tab_index(0).rounded(px(5.0))
                .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
                .on_click(cx.listener(|this, _, _, cx| { this.usage_flow_selection = None; cx.notify(); }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.usage_flow_selection = None; cx.notify(); cx.stop_propagation();
                    }
                })));
            for (i, lane) in left.iter().enumerate() {
                let bounds = Bounds::new(point(px(0.0), px(left_positions[i] - label_height / 2.0)), size(px((lx - FLOW_LABEL_GUTTER).max(1.0)), px(label_height)));
                field = field.child(lane_button(format!("usage-flow-model-{i}"), lane, bounds, false, lane.selection == selected && selected.is_some(), theme, cx));
            }
            for (i, lane) in right.iter().enumerate() {
                let bounds = Bounds::new(point(px(rx + FLOW_LABEL_GUTTER), px(right_positions[i] - label_height / 2.0)), size(px((w - rx - FLOW_LABEL_GUTTER).max(1.0)), px(label_height)));
                field = field.child(lane_button(format!("usage-flow-lane-{i}"), lane, bounds, true, lane.selection == selected && selected.is_some(), theme, cx));
            }
        }
        field = field.child(canvas(move |bounds, _, cx| {
            let measured = f32::from(bounds.size.width);
            width.update(cx, |width, cx| {
                let changed = (width.width - measured).abs() > 0.5;
                width.width = measured;
                width.bounds = Some(bounds);
                if changed { cx.notify(); }
            });
        }, |_, _, _, _| {}).absolute().inset_0());
        let facts_for_total = || right.iter().map(|lane| (
            lane.title.clone(),
            if lane.known { format_tokens(lane.value) } else { "—".into() },
        )).collect::<Vec<_>>();
        let details = match &selected {
            Some(UsageFlowSelection::Model(i)) => views.models.get(*i).map(|row| (
                row.model.as_deref().unwrap_or("—").to_owned(), tr!("usage_page.models_title"),
                vec![(tr!("usage_page.kpi_total_tokens"), format_tokens(row.total)), (tr!("usage_page.flow_share"), flow_percent(row.total, total)), (tr!("usage_page.kpi_sessions"), row.sessions.to_string())],
            )),
            Some(UsageFlowSelection::Lane(i)) => right.get(*i).map(|lane| (
                lane.title.clone(), lane.detail.clone(),
                vec![(tr!("usage_page.kpi_total_tokens"), format_tokens(lane.value)), (tr!("usage_page.flow_share"), flow_percent(lane.value, total)), (tr!("usage_page.flow_scope"), self.usage_range.label())],
            )),
            Some(UsageFlowSelection::Day(i)) => views.daily.get(*i).map(|day| (
                day.date.to_string(), tr!("usage_page.model_sessions", count = day.sessions),
                vec![(tr!("usage_page.tokens_input"), format_tokens(day.input)), (tr!("usage_page.kpi_generated_tokens"), format_tokens(day.output)), (tr!("usage_page.plot_cache_read"), format_tokens(day.cache_read)), (tr!("usage_page.plot_cache_write"), format_tokens(day.cache_write))],
            )),
            Some(UsageFlowSelection::Hour(i)) => views.hourly.get(*i).map(|hour| (
                format!("{:02}:00", hour.hour), self.usage_range.label(),
                vec![(tr!("usage_page.tokens_input"), format_tokens(hour.input)), (tr!("usage_page.kpi_generated_tokens"), format_tokens(hour.output))],
            )),
            Some(UsageFlowSelection::Provider(i)) => views.providers.get(*i).map(|row| (
                row.provider.clone(), tr!("usage_page.providers_title"),
                vec![(tr!("usage_page.kpi_total_tokens"), format_tokens(row.total)), (tr!("usage_page.flow_share"), flow_percent(row.total, total)), (tr!("usage_page.models_title"), row.models.to_string())],
            )),
            Some(UsageFlowSelection::Project(i)) => views.projects.get(*i).map(|row| (
                project_display_name(&row.directory), row.directory.clone(),
                vec![(tr!("usage_page.kpi_total_tokens"), format_tokens(row.total)), (tr!("usage_page.flow_share"), flow_percent(row.total, total)), (tr!("usage_page.kpi_sessions"), row.sessions.to_string())],
            )),
            None => None,
        };
        let (title, subtitle, facts) = details.unwrap_or_else(|| (
            tr!("usage_page.flow_composition"), format!("{} tokens", format_tokens(total)), facts_for_total(),
        ));
        let footer = div().flex().items_center().gap_4().px_3().py_2().min_h(px(64.0))
            .bg(theme.surface).border_t_1().border_color(theme.border)
            .child(div().flex_1().min_w_0().flex().flex_col().gap_1()
                .child(div().w_full().truncate().text_size(ui_px(12.0)).text_color(theme.text).child(title))
                .child(div().w_full().truncate().text_size(ui_px(11.0)).text_color(theme.text_ghost).child(subtitle)))
            .child(div().flex().flex_wrap().items_center().gap_4().children(facts.into_iter().map(|(label, value)| {
                div().flex().flex_col().gap_1()
                    .child(div().text_size(ui_px(10.0)).text_color(theme.text_tertiary).child(label))
                    .child(div().text_size(ui_px(12.0)).text_color(theme.text).child(value))
            })));
        let header = div().flex().items_center().justify_between()
            .child(div().text_size(ui_px(13.0)).font_weight(FontWeight::MEDIUM).child(tr!("usage_page.flow_title")))
            .when(selected.is_some(), |header| header.child(
                Button::new("usage-flow-clear", tr!("usage_page.flow_clear"))
                    .size(ControlSize::Sm).variant(ButtonVariant::Ghost)
                    .on_click(cx.listener(|this, _, _, cx| {this.usage_flow_selection = None;cx.notify();}))
            ));
        usage_card(theme).flex_1().min_w_0().flex().flex_col().gap_2().p_3()
            .child(header)
            .child(div().bg(theme.composer).border_1().border_color(theme.border).rounded(px(6.0)).overflow_hidden().child(field).child(footer))
    }
}

fn flow_detail(value: u64, total: u64) -> String {
    if total == 0 { return format_tokens(value); }
    format!("{} · {:.1}%", format_tokens(value), value as f64 / total as f64 * 100.0)
}

fn flow_percent(value: u64, total: u64) -> String {
    if total == 0 { return "—".into(); }
    format!("{:.1}%", value as f64 / total as f64 * 100.0)
}

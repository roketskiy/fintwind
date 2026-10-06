//! The usage page's model treemap: one tile per model, its area its share of
//! the tokens.
//!
//! Ported from Ely's `Treemap` (Ely-GPUI-Components `src/charts/tiles.rs`,
//! MIT — see `docs/licenses/ely-gpui-components-MIT.txt`): the same squarify
//! layout, which lays the values out in rows that keep every tile close to
//! square, largest first. Ely paints the tiles into one canvas and finds the
//! tile under the pointer by hit-testing that canvas. Here each tile is an
//! element instead, so it is a tab stop with a focus ring, and it carries the
//! numbers the row list did without needing a hover to read them.

use gpui::{
    AnyElement, App, Div, Entity, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Window, canvas, div, px,
};

use super::usage_page::{CHART_TOOLTIP_DELAY, format_cost, plot_colors};
use crate::theme::{Theme, ui_px};
use crate::ui::tooltip::Tooltip;
use crate::usage::format_tokens;

/// The field's height. Fixed, like the daily chart's plot, so the card does
/// not change size as the window does. The width is measured instead: it is
/// the page column's, and a tile's area depends on both sides.
pub(super) const TREEMAP_PX: f32 = 200.0;
/// Neighbouring tiles are inset by half of this, so a pair reads as two boxes.
const TILE_GAP: f32 = 2.0;
/// A tile drops the lines it has no room for rather than clipping them into
/// unreadable stubs. Its area still says how large the model was, and the
/// tooltip still names it.
const NAME_MIN_W: f32 = 44.0;
const NAME_MIN_H: f32 = 17.0;
const VALUE_MIN_W: f32 = 30.0;
const VALUE_MIN_H: f32 = 12.0;
/// The detail line is a phrase, not a number, so it needs room in both
/// directions before it is worth drawing.
const DETAIL_MIN_W: f32 = 104.0;
const DETAIL_MIN_H: f32 = 54.0;

/// The field's measured width, read back on the frame after the measure canvas
/// saw it. Nothing else is stored: the tiles are laid out again from the
/// models every frame, which is eight rectangles of arithmetic.
#[derive(Default)]
struct TreemapWidth {
    width: f32,
}

/// One model's tile.
pub(super) struct TreemapTile {
    /// The model id as drawn in the tile, truncated to whatever the tile's
    /// width allows.
    pub name: String,
    /// `provider/model`: what the tile has no room for, so it is the tooltip.
    pub qualified: String,
    /// The trailing count, already localized — "3 个会话".
    pub count_label: String,
    pub total: u64,
    pub cost: f64,
}

/// The tile field. Empty on the first frame after the page opens, because a
/// tile's area needs the field's width and only layout knows that; the measure
/// canvas below reports it and the page repaints.
pub(super) fn usage_treemap(
    window: &mut Window,
    cx: &mut App,
    tiles: &[TreemapTile],
    theme: &Theme,
) -> AnyElement {
    let values: Vec<f64> = tiles.iter().map(|tile| tile.total as f64).collect();
    // Checked before the field exists rather than after it is laid out: a
    // field with nothing to divide must not be measured, or an unmeasured
    // field would look like that same nothing.
    if values.iter().all(|value| *value <= 0.0) {
        return div().into_any_element();
    }

    let width = window.use_keyed_state("usage-model-treemap-width", cx, |_, _| {
        TreemapWidth::default()
    });
    let rects = squarify(
        &values,
        TileRect {
            x: 0.0,
            y: 0.0,
            w: width.read(cx).width,
            h: TREEMAP_PX,
        },
    );
    let colors = plot_colors(theme);

    let mut field = div()
        .id("usage-model-treemap")
        // The tiles are the tab stops; the field itself is not one.
        .tab_group()
        .relative()
        .w_full()
        .h(px(TREEMAP_PX));

    for (index, tile) in tiles.iter().enumerate() {
        let placed = inset(rects[index], TILE_GAP);
        // A model that spent no tokens has no area, so it has no tile. The
        // caption still counts it.
        if placed.w <= 0.0 || placed.h <= 0.0 {
            continue;
        }
        field = field.child(usage_treemap_tile(
            index,
            tile,
            placed,
            colors[index % colors.len()],
            theme,
        ));
    }

    field.child(measure_width(width)).into_any_element()
}

fn usage_treemap_tile(
    index: usize,
    tile: &TreemapTile,
    rect: TileRect,
    ink: Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    let tokens = format_tokens(tile.total);
    let detail = if tile.cost > 0.0 {
        format!("{} · {}", tile.count_label, format_cost(tile.cost))
    } else {
        tile.count_label.clone()
    };
    let roomy = rect.w >= DETAIL_MIN_W && rect.h >= DETAIL_MIN_H;

    let mut readout = div().flex_1().min_w_0().flex().flex_col().gap(px(1.0));
    if rect.w >= NAME_MIN_W && rect.h >= NAME_MIN_H {
        readout = readout.child(
            div()
                .min_w_0()
                .truncate()
                .text_size(ui_px(10.5))
                .text_color(theme.text)
                .child(SharedString::from(tile.name.clone())),
        );
    }
    if rect.w >= VALUE_MIN_W && rect.h >= VALUE_MIN_H {
        readout = readout.child(
            div()
                .min_w_0()
                .truncate()
                .text_size(if roomy { ui_px(11.5) } else { ui_px(9.5) })
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text_secondary)
                .child(SharedString::from(tokens.clone())),
        );
    }
    if roomy {
        readout = readout.child(
            div()
                .min_w_0()
                .truncate()
                .text_size(ui_px(9.5))
                .text_color(theme.text_tertiary)
                .child(SharedString::from(detail.clone())),
        );
    }

    // The one reading a tile cannot hold: which provider served a model id
    // that another provider may serve too.
    let heading = if tile.qualified.is_empty() {
        tile.name.clone()
    } else {
        tile.qualified.clone()
    };
    let mut tooltip = format!("{heading} · {} · {tokens}", tile.count_label);
    if tile.cost > 0.0 {
        tooltip = format!("{tooltip} · {}", format_cost(tile.cost));
    }

    div()
        .id(SharedString::from(format!("usage-model-tile-{index}")))
        .tab_index(0)
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.w))
        .h(px(rect.h))
        .rounded(px(5.0))
        .overflow_hidden()
        .border_1()
        .border_color(ink.opacity(0.55))
        .bg(ink.opacity(0.20))
        .hover(|style| style.bg(ink.opacity(0.32)).border_color(ink.opacity(0.9)))
        .focus_visible(|style| style.border_color(theme.accent_focus))
        // A roomy tile can afford its own margin; a narrow one spends its
        // width on the number instead.
        .p(px(if roomy { 5.0 } else { 2.0 }))
        .child(readout)
        .tooltip(Tooltip::text(SharedString::from(tooltip)))
        .tooltip_show_delay(CHART_TOOLTIP_DELAY)
}

/// The canvas that records the field's width for the frame after this one.
/// Absolute, so it covers the field without taking part in its flow, and empty,
/// so it paints nothing.
fn measure_width(width: Entity<TreemapWidth>) -> impl IntoElement {
    canvas(
        move |bounds, _, cx| {
            let next = f32::from(bounds.size.width);
            width.update(cx, |width, cx| {
                // Sub-pixel jitter must not buy a repaint every time the page
                // column is measured.
                if (width.width - next).abs() > 0.5 {
                    width.width = next;
                    cx.notify();
                }
            });
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// A box in the field's own pixels.
#[derive(Clone, Copy, Debug, Default)]
struct TileRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// The box `gap` inside `rect` on every side, so neighbouring tiles keep a seam
/// of the card between them. A rect thinner than the seam collapses to nothing
/// rather than to a negative box.
fn inset(rect: TileRect, gap: f32) -> TileRect {
    TileRect {
        x: rect.x + gap / 2.0,
        y: rect.y + gap / 2.0,
        w: (rect.w - gap).max(0.0),
        h: (rect.h - gap).max(0.0),
    }
}

/// Where each value's tile sits, in the order the values were given.
///
/// Ported from Ely's `charts::layout::squarify` (Ely-GPUI-Components
/// `src/charts/layout.rs`, MIT — see
/// `docs/licenses/ely-gpui-components-MIT.txt`): values are laid out in rows
/// along the shorter side, and a row grows only while the worst tile in it
/// stays closer to square than the row without it, so the tiles fill the frame
/// without slivers. Largest first, because the greedy row walk only reads well
/// in that order. A value of zero is left empty: there is no area to give it.
fn squarify(values: &[f64], frame: TileRect) -> Vec<TileRect> {
    let total: f64 = values.iter().filter(|value| **value > 0.0).sum();
    let mut out = vec![TileRect::default(); values.len()];
    if total <= 0.0 || frame.w <= 0.0 || frame.h <= 0.0 {
        return out;
    }
    let mut areas: Vec<(usize, f64)> = values
        .iter()
        .enumerate()
        .filter(|(_, value)| **value > 0.0)
        .map(|(ix, value)| (ix, value * f64::from(frame.w * frame.h) / total))
        .collect();
    areas.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut rest = frame;
    let mut start = 0;
    while start < areas.len() {
        let side = f64::from(rest.w.min(rest.h));
        // How far the row's worst tile is from square. Lower is better, and
        // the row stops growing once adding one would make it worse.
        let worst = |row: &[(usize, f64)]| {
            let sum: f64 = row.iter().map(|(_, area)| area).sum();
            let (least, most) = row
                .iter()
                .fold((f64::MAX, 0.0f64), |(least, most), (_, area)| {
                    (least.min(*area), most.max(*area))
                });
            (side * side * most / (sum * sum)).max(sum * sum / (side * side * least))
        };
        let mut end = start + 1;
        while end < areas.len() && worst(&areas[start..=end]) <= worst(&areas[start..end]) {
            end += 1;
        }
        let row = &areas[start..end];
        let sum: f64 = row.iter().map(|(_, area)| area).sum();
        let thick = (sum / side) as f32;
        let mut along = 0.0f32;
        for (ix, area) in row {
            let length = (*area / f64::from(thick)) as f32;
            out[*ix] = if rest.w >= rest.h {
                TileRect {
                    x: rest.x,
                    y: rest.y + along,
                    w: thick,
                    h: length,
                }
            } else {
                TileRect {
                    x: rest.x + along,
                    y: rest.y,
                    w: length,
                    h: thick,
                }
            };
            along += length;
        }
        rest = if rest.w >= rest.h {
            TileRect {
                x: rest.x + thick,
                w: rest.w - thick,
                ..rest
            }
        } else {
            TileRect {
                y: rest.y + thick,
                h: rest.h - thick,
                ..rest
            }
        };
        start = end;
    }
    out
}

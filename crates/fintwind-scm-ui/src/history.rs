use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use fintwind_git::git_history::{CommitEntry, CommitGraph, GraphHalf, GraphRow};
use fintwind_protocol::tr;
use fintwind_ui::theme::{Theme, code_font_family, code_px, mute_saturation, ui_px};
use fintwind_ui::ui::tooltip::Tooltip;
use fintwind_ui::ui::{format_time_ago, icon};
use gpui::{
    AnyElement, Bounds, Div, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    PathBuilder, Pixels, Point, SharedString, Stateful, Styled, Window, canvas, div, fill, point,
    prelude::*, px, relative, rgb, size,
};

pub const HISTORY_ROW_HEIGHT: f32 = 60.0;
/// How many commits one history read asks the daemon for.
pub const HISTORY_COMMIT_LIMIT: usize = 500;

pub type HistoryKey = (PathBuf, Option<String>);
pub type HistoryResult = Result<Option<Arc<HistoryRows>>, String>;

/// Prepared off-thread once per fetch. Frames clone an Arc, never traverse the
/// complete history or recompute its topology.
#[derive(Default)]
pub struct HistoryRows {
    pub commits: Vec<CommitEntry>,
    pub graph: CommitGraph,
    pub positions: HashMap<String, usize>,
}

impl HistoryRows {
    /// The loaded entry for one hash, if the current history still shows it.
    pub fn commit(&self, hash: &str) -> Option<&CommitEntry> {
        self.commits.iter().find(|commit| commit.hash == hash)
    }

    pub fn new(commits: Vec<CommitEntry>) -> Self {
        let graph = CommitGraph::new(&commits);
        let positions = commits
            .iter()
            .enumerate()
            .map(|(index, commit)| (commit.hash.clone(), index))
            .collect();
        Self {
            commits,
            graph,
            positions,
        }
    }
}

/// What the history panel has to draw for the selected workspace: either the
/// synced row cache (the commit list) or one of the empty states that must
/// stay distinguishable (not a repository / daemon error / fetch in flight).
/// Keyboard and click handlers read the same prepared snapshot as the list.
pub enum HistoryFetch {
    Ready,
    Refreshing,
    NotRepository,
    Error(String),
    Loading,
    Waiting,
    Placeholder,
}

/// The shared span preserves connections even when neighboring rows are
/// unmounted. Lane geometry follows VSCode's `renderSCMHistoryItemGraph`
/// (`swimlanes`, rounded `curve` bends); branch colors cycle its five
/// `scmGraph.foreground` registrations so a lane keeps its color.
pub fn graph_cell(
    row: GraphRow,
    span: usize,
    height: Pixels,
    is_head: bool,
    merge: bool,
    selected: bool,
    theme: Theme,
) -> AnyElement {
    // VSCode scmGraph.foreground1-5: #FFB000 #DC267F #994F00 #40B0A6 #B66DFF.
    let palette: [Hsla; 5] = [
        rgb(0xFFB000).into(),
        rgb(0xDC267F).into(),
        rgb(0x994F00).into(),
        rgb(0x40B0A6).into(),
        rgb(0xB66DFF).into(),
    ]
    .map(mute_saturation);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let step = bounds.size.width / span.max(1) as f32;
            let x = |lane: usize| bounds.origin.x + step * (lane as f32 + 0.5);
            let top = bounds.origin.y;
            let middle = top + bounds.size.height / 2.0;
            let bottom = top + bounds.size.height;
            // One S-curve with vertical tangents at both ends, like VSCode's
            // paired arc bends: straight verticals stay straight, lane hops
            // bend smoothly instead of cutting diagonally across lanes.
            let mut curve = |from: Point<Pixels>, to: Point<Pixels>, color: Hsla| {
                let mut path = PathBuilder::stroke(px(2.0));
                path.move_to(from);
                if (f32::from(from.x) - f32::from(to.x)).abs() < 0.5 {
                    path.line_to(to);
                } else {
                    let mid = point((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
                    path.curve_to(mid, point(from.x, mid.y));
                    path.curve_to(to, point(to.x, mid.y));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            for stroke in &row.strokes {
                let color = palette[stroke.color % palette.len()];
                match stroke.half {
                    GraphHalf::Top => {
                        curve(point(x(stroke.from), top), point(x(stroke.to), middle), color)
                    }
                    GraphHalf::Bottom => {
                        curve(point(x(stroke.from), middle), point(x(stroke.to), bottom), color)
                    }
                    GraphHalf::Through => {
                        curve(point(x(stroke.from), top), point(x(stroke.to), bottom), color)
                    }
                }
            }
            let center = point(x(row.lane), middle);
            let dot = step.min(px(16.0)) * 0.45;
            let color = palette[row.color % palette.len()];
            let paint_dot = |window: &mut Window, diameter: Pixels, color: Hsla| {
                window.paint_quad(
                    fill(
                        Bounds::new(
                            center - point(diameter / 2.0, diameter / 2.0),
                            size(diameter, diameter),
                        ),
                        color,
                    )
                    .corner_radii(diameter / 2.0),
                );
            };
            if is_head {
                // VSCode HEAD node: outer ring plus inner dot.
                paint_dot(window, dot + px(5.0), color);
                paint_dot(window, dot * 0.5, theme.surface);
            } else {
                if selected {
                    paint_dot(window, dot + px(5.0), color.opacity(0.22));
                }
                paint_dot(window, dot, color);
                if merge {
                    // VSCode multi-parent node: outer ring with a smaller core.
                    paint_dot(window, dot * 0.45, theme.surface);
                }
            }
        },
    )
    .flex_none()
    .w(px(16.0 * span.max(1) as f32))
    // Reserve room for the subject in a narrow panel; compress lanes together
    // instead of clipping edges or silently discarding branches.
    .max_w(relative(0.35))
    .h(height)
    .into_any_element()
}

pub fn commit_details(commit: &CommitEntry, index: usize, now: u64, theme: Theme) -> Div {
    let mut chips = div()
        .flex_none()
        .max_w(relative(0.5))
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(4.0));
    let shown = commit.refs.len().min(2);
    for (ref_index, commit_ref) in commit.refs.iter().take(shown).enumerate() {
        chips = chips.child(ref_chip(
            format!("history-ref-{index}-{ref_index}"),
            commit_ref.label.clone(),
            commit_ref.remote,
            commit_ref.head,
            &theme,
            None,
        ));
    }
    if commit.refs.len() > shown {
        chips = chips.child(ref_chip(
            format!("history-ref-{index}-overflow"),
            format!("+{}", commit.refs.len() - shown),
            false,
            false,
            &theme,
            Some(
                commit
                    .refs
                    .iter()
                    .skip(shown)
                    .map(|r| r.label.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        ));
    }
    let title = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .min_w_0()
        .line_height(ui_px(20.0))
        .child(
            div()
                .id(SharedString::from(format!("history-subject-{index}")))
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(ui_px(12.5))
                .font_weight(if commit.is_head {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(theme.text)
                .tooltip(Tooltip::text(commit.subject.clone()))
                .child(SharedString::from(commit.subject.clone())),
        )
        .when(!commit.refs.is_empty(), |title| title.child(chips))
        .when(!commit.pushed, |title| {
            title.child(
                div()
                    .id(SharedString::from(format!("history-unpushed-{index}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .tooltip(Tooltip::text(tr!("git_history.unpushed")))
                    .child(icon("icons/cloud-upload.svg", 12.0, theme.warning)),
            )
        });
    let metadata = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .min_w_0()
        .line_height(ui_px(18.0))
        .text_size(ui_px(10.5))
        .text_color(theme.text_tertiary)
        .child(
            div()
                .flex_none()
                .text_size(code_px(10.5))
                .font_family(code_font_family())
                .text_color(if commit.is_head {
                    theme.accent_text
                } else {
                    theme.text_secondary
                })
                .child(SharedString::from(commit.short_hash.clone())),
        )
        .child(
            div()
                .id(SharedString::from(format!("history-author-{index}")))
                .flex_1()
                .min_w_0()
                .truncate()
                .tooltip(Tooltip::text(commit.author.clone()))
                .child(SharedString::from(commit.author.clone())),
        )
        .child(
            div()
                .flex_none()
                .child(format_time_ago(now.saturating_sub(commit.timestamp))),
        );
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(title)
        .child(metadata)
}

/// Ref decorations stay bounded so long branch names do not consume a row.
/// Colors follow VSCode's history-item refs: local branches blue, remote
/// branches purple, tags amber, HEAD solid. Icons match each kind.
pub fn ref_chip(
    id: String,
    label: String,
    remote: bool,
    is_head: bool,
    theme: &Theme,
    tooltip: Option<String>,
) -> Stateful<Div> {
    let is_tag = label.starts_with("tag: ");
    let (color, icon_path) = if is_head {
        (theme.on_accent, None)
    } else if is_tag {
        (theme.warning_text, Some("icons/tag.svg"))
    } else if remote {
        (theme.code_keyword, Some("icons/globe.svg"))
    } else {
        (theme.link, Some("icons/git-branch.svg"))
    };
    div()
        .id(SharedString::from(id))
        .flex_none()
        .max_w(ui_px(104.0))
        .overflow_hidden()
        .px(px(5.0))
        .h(ui_px(16.0))
        .rounded(px(4.0))
        .flex()
        .items_center()
        .gap(px(3.0))
        .when_some(icon_path, |chip, path| {
            chip.child(icon(path, 9.0, color))
        })
        .tooltip(Tooltip::text(tooltip.unwrap_or_else(|| {
            if remote {
                format!("{label}\n{}", tr!("git_history.remote_ref"))
            } else {
                label.clone()
            }
        })))
        .text_size(ui_px(10.0))
        .text_color(color)
        .when(is_head, |chip| chip.bg(theme.accent))
        .when(!is_head, |chip| chip.bg(theme.overlay))
        .child(div().min_w_0().truncate().child(SharedString::from(label)))
}

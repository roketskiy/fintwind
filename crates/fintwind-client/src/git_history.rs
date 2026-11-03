//! In-memory commit graph layout. Call once when a history fetch completes,
//! not from a row builder or a frame's render path.
// Lane allocation adapted from Ely GPUI Components, src/git/graph.rs,
// commit e17e31a6890c09ebcfa8b61133d7bc7c625edf69, under the MIT license.
// See docs/licenses/ely-gpui-components-MIT.txt for copyright and permission.
// Swimlane continuation and per-branch color rotation follow VSCode's
// `toISCMHistoryItemViewModelArray` in
// `src/vs/workbench/contrib/scm/browser/scmHistory.ts` (MIT).

pub use fintwind_protocol::git::{CommitEntry, CommitRef};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphHalf {
    Top,
    Bottom,
    Through,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphStroke {
    pub from: usize,
    pub to: usize,
    pub half: GraphHalf,
    /// Index into the branch palette. Stable per branch: the first parent
    /// inherits its commit's color, each extra parent rotates to the next.
    pub color: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphRow {
    pub lane: usize,
    /// Stable branch color of the commit node, index into the branch palette.
    pub color: usize,
    pub strokes: Vec<GraphStroke>,
}

#[derive(Clone, Debug, Default)]
pub struct CommitGraph {
    pub rows: Vec<GraphRow>,
    /// Shared span keeps the same lane at the same x coordinate in every row.
    pub width: usize,
}

/// Number of distinct branch colors cycled through, matching VSCode's
/// five `scmGraph.foreground` registrations.
pub const GRAPH_COLOR_COUNT: usize = 5;

impl CommitGraph {
    /// Input must be topologically ordered (children before all their parents).
    /// Parents outside a truncated window keep outgoing lines at its bottom.
    pub fn new(commits: &[CommitEntry]) -> Self {
        let mut graph = Self::default();
        // Lanes waiting for their commit, in lane order: (expected hash, color).
        let mut lanes: Vec<(String, usize)> = Vec::new();
        let mut next_color: usize = 0;

        for commit in commits {
            let before = lanes.clone();
            let arrival = before.iter().position(|(hash, _)| hash == &commit.hash);
            let lane = arrival.unwrap_or(before.len());
            let row_color = match arrival {
                Some(index) => before[index].1,
                None => {
                    let color = next_color % GRAPH_COLOR_COUNT;
                    next_color += 1;
                    color
                }
            };

            // Output lanes: carry-overs before the arrival slot, then the
            // commit's parents inserted where the commit was, then the
            // remaining carry-overs. A parent already waited on by another
            // lane joins that lane instead of opening a duplicate.
            let mut output: Vec<(String, usize)> = before.iter().take(lane).cloned().collect();
            for (parent_index, parent) in commit.parents.iter().enumerate() {
                if output.iter().any(|(wait, _)| wait == parent) {
                    continue;
                }
                if let Some(waiter) = before
                    .iter()
                    .skip(lane)
                    .find(|(wait, _)| wait == parent)
                    .cloned()
                {
                    output.push(waiter);
                    continue;
                }
                let color = if parent_index == 0 {
                    row_color
                } else {
                    let color = next_color % GRAPH_COLOR_COUNT;
                    next_color += 1;
                    color
                };
                output.push((parent.clone(), color));
            }
            for (index, waiter) in before.iter().enumerate().skip(lane) {
                if Some(index) == arrival {
                    continue;
                }
                if output.iter().any(|(wait, _)| wait == &waiter.0) {
                    continue;
                }
                output.push(waiter.clone());
            }

            let mut strokes = Vec::new();
            for (index, waiter) in before.iter().enumerate() {
                if waiter.0 == commit.hash {
                    strokes.push(GraphStroke {
                        from: index,
                        to: lane,
                        half: GraphHalf::Top,
                        color: row_color,
                    });
                    continue;
                }
                if let Some(target) = output.iter().position(|(wait, _)| wait == &waiter.0) {
                    strokes.push(GraphStroke {
                        from: index,
                        to: target,
                        half: GraphHalf::Through,
                        color: waiter.1,
                    });
                }
            }
            for parent in &commit.parents {
                if let Some(target) = output.iter().position(|(wait, _)| wait == parent) {
                    strokes.push(GraphStroke {
                        from: lane,
                        to: target,
                        half: GraphHalf::Bottom,
                        color: output[target].1,
                    });
                }
            }
            graph.width = graph
                .width
                .max(before.len())
                .max(output.len())
                .max(lane + 1);
            lanes = output;
            graph.rows.push(GraphRow {
                lane,
                color: row_color,
                strokes,
            });
        }
        graph
    }
}

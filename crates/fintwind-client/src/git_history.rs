//! In-memory commit graph layout. Call once when a history fetch completes,
//! not from a row builder or a frame's render path.
// Fixed columns, like `git log --graph`: a branch keeps its lane's x
// forever, so every branch draws as one straight vertical line and bends
// appear only at real joins. Swimlane continuation and per-branch color
// rotation follow VSCode's `toISCMHistoryItemViewModelArray` in
// `src/vs/workbench/contrib/scm/browser/scmHistory.ts` (MIT).

pub use fintwind_protocol::git::{CommitEntry, CommitRef};
use std::collections::{HashMap, HashSet};

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
    /// Index into the branch palette. An edge keeps its color until it
    /// reaches its parent node, including when it joins another branch.
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

/// The spine's color: the HEAD-first-parent chain (usually `main`) keeps one
/// calm color no matter how many side branches merge in, like VSCode's blue
/// main line. Side branches rotate through the remaining four.
pub const MAIN_COLOR: usize = 3;

fn fresh_color(next: &mut usize) -> usize {
    loop {
        let color = *next % GRAPH_COLOR_COUNT;
        *next += 1;
        if color != MAIN_COLOR {
            return color;
        }
    }
}

/// Leftmost empty slot at or after 1; slot 0 belongs to the mainline.
/// Pushes new slots when every existing one is busy.
fn free_side_slot<'a>(lanes: &mut Vec<Option<(&'a str, usize)>>) -> usize {
    if let Some(index) = lanes
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, pending)| pending.is_none())
        .map(|(index, _)| index)
    {
        return index;
    }
    if lanes.is_empty() {
        lanes.push(None);
    }
    lanes.push(None);
    lanes.len() - 1
}

fn find_slot(lanes: &[Option<(&str, usize)>], hash: &str) -> Option<usize> {
    lanes
        .iter()
        .position(|pending| pending.is_some_and(|(wait, _)| wait == hash))
}

impl CommitGraph {
    /// Input must be topologically ordered (children before all their parents).
    /// Parents outside a truncated window keep outgoing lines at its bottom.
    pub fn new(commits: &[CommitEntry]) -> Self {
        let mut graph = Self::default();
        // The spine whose color never changes: HEAD's first-parent chain,
        // falling back to the list tip when HEAD is out of view (detached or
        // filtered). Without this, whichever side branch reaches a shared
        // ancestor first would repaint the whole main line below the merge.
        let by_hash: HashMap<&str, &CommitEntry> = commits
            .iter()
            .map(|commit| (commit.hash.as_str(), commit))
            .collect();
        let mut mainline = HashSet::new();
        let mut next = commits
            .iter()
            .find(|commit| commit.is_head)
            .or(commits.first())
            .map(|commit| commit.hash.as_str());
        while let Some(hash) = next {
            if !mainline.insert(hash) {
                break;
            }
            next = by_hash
                .get(hash)
                .and_then(|commit| commit.parents.first())
                .map(String::as_str);
        }
        // A slot represents one visible edge waiting for one parent, not a
        // queue of ancestors sharing the same line. Multiple slots may wait
        // for the same ancestor: keep them separate until that node's row.
        // Slot zero is reserved for the mainline, even above HEAD when other
        // refs have newer tips. Never compact live slots after a join.
        let mut lanes: Vec<Option<(&str, usize)>> = vec![None];
        let mut next_color: usize = 0;

        for commit in commits {
            let arrival = find_slot(&lanes, &commit.hash);
            let is_mainline = mainline.contains(commit.hash.as_str());
            let lane = if is_mainline {
                0
            } else {
                arrival.unwrap_or_else(|| free_side_slot(&mut lanes))
            };
            let row_color = if is_mainline {
                MAIN_COLOR
            } else {
                arrival
                    .and_then(|index| lanes[index].map(|(_, color)| color))
                    .unwrap_or_else(|| fresh_color(&mut next_color))
            };

            // Only edges that entered from the previous row can have Top
            // or Through strokes. New parent edges start at this node's
            // midpoint; drawing them Through would invent an edge above it.
            let mut strokes = Vec::new();
            for (index, pending) in lanes.iter_mut().enumerate() {
                if let Some((hash, color)) = *pending {
                    let arrives = hash == commit.hash;
                    strokes.push(GraphStroke {
                        from: index,
                        to: if arrives { lane } else { index },
                        half: if arrives {
                            GraphHalf::Top
                        } else {
                            GraphHalf::Through
                        },
                        color,
                    });
                    if arrives {
                        *pending = None;
                    }
                }
            }

            // The first parent continues straight in this branch's own slot,
            // even if another slot already waits for that ancestor. Joining
            // now would bend the branch early and overlay distinct edges.
            // Extra parents fork at this node into an existing or free slot.
            for (parent_index, parent) in commit.parents.iter().enumerate() {
                let target = if parent_index == 0 {
                    lane
                } else {
                    find_slot(&lanes, parent).unwrap_or_else(|| free_side_slot(&mut lanes))
                };
                let color = if parent_index == 0 {
                    row_color
                } else {
                    lanes[target]
                        .map(|(_, color)| color)
                        .unwrap_or_else(|| fresh_color(&mut next_color))
                };
                lanes[target] = Some((parent.as_str(), color));
                strokes.push(GraphStroke {
                    from: lane,
                    to: target,
                    half: GraphHalf::Bottom,
                    color,
                });
            }
            let occupied = lanes
                .iter()
                .rposition(Option::is_some)
                .map_or(0, |index| index + 1);
            graph.width = graph.width.max(occupied).max(lane + 1);
            graph.rows.push(GraphRow {
                lane,
                color: row_color,
                strokes,
            });
        }
        graph
    }
}

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
fn free_side_slot(lanes: &mut Vec<Vec<(String, usize)>>) -> usize {
    if let Some(index) = lanes
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, queue)| queue.is_empty())
        .map(|(index, _)| index)
    {
        return index;
    }
    if lanes.is_empty() {
        lanes.push(Vec::new());
    }
    lanes.push(Vec::new());
    lanes.len() - 1
}

fn find_slot(lanes: &[Vec<(String, usize)>], hash: &str) -> Option<usize> {
    lanes.iter().position(|queue| {
        queue.iter().any(|(wait, _)| wait == hash)
    })
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
        // Fixed columns, like `git log --graph`: once a branch opens a lane
        // it keeps that x forever, so every branch draws as one straight
        // vertical line and bends happen only at real joins. Each slot holds
        // a queue of the commits it still waits for: slot 0 belongs to the
        // mainline, so a side branch that reaches a shared ancestor first
        // queues behind the mainline waiter instead of stealing the ancestor
        // into its own column — the main line can never be bent by
        // processing order.
        let mut lanes: Vec<Vec<(String, usize)>> = Vec::new();
        let mut next_color: usize = 0;

        for commit in commits {
            let arrival = find_slot(&lanes, &commit.hash);
            let lane = match arrival {
                Some(index) => index,
                None => {
                    if mainline.contains(commit.hash.as_str()) {
                        if lanes.is_empty() {
                            lanes.push(Vec::new());
                        }
                        if lanes[0].is_empty() {
                            0
                        } else {
                            // A mainline tip while slot 0 is busy (a second
                            // disconnected history): fall back to a side slot
                            // rather than evicting the pending waiter.
                            free_side_slot(&mut lanes)
                        }
                    } else {
                        free_side_slot(&mut lanes)
                    }
                }
            };
            let row_color = if mainline.contains(commit.hash.as_str()) {
                MAIN_COLOR
            } else {
                match arrival {
                    Some(index) => lanes[index]
                        .iter()
                        .find(|(wait, _)| wait == &commit.hash)
                        .map(|(_, color)| *color)
                        .unwrap_or_else(|| fresh_color(&mut next_color)),
                    None => fresh_color(&mut next_color),
                }
            };

            // Consume the arrival; any queue behind it stays and keeps the
            // column alive. The snapshot below decides Through strokes: a
            // column whose waiter survives this row still draws straight
            // down, even through the arrival's own row.
            let before = lanes.clone();
            if arrival.is_some() {
                let queue = &mut lanes[lane];
                if let Some(position) =
                    queue.iter().position(|(wait, _)| wait == &commit.hash)
                {
                    queue.remove(position);
                }
            }
            // Parents: a mainline parent always queues in slot 0 (appending
            // behind any pending waiter); otherwise the first parent reuses
            // this row's slot so the branch continues straight, and extra
            // parents open side slots. A parent already waited on by another
            // slot joins that slot instead of opening a duplicate.
            let mut bottoms: Vec<(usize, usize)> = Vec::new();
            for (parent_index, parent) in commit.parents.iter().enumerate() {
                if let Some(target) = find_slot(&lanes, parent) {
                    let color = lanes[target]
                        .iter()
                        .find(|(wait, _)| wait == parent)
                        .map(|(_, color)| *color)
                        .unwrap_or(row_color);
                    bottoms.push((target, color));
                    continue;
                }
                let target = if mainline.contains(parent.as_str()) {
                    if lanes.is_empty() {
                        lanes.push(Vec::new());
                    }
                    0
                } else if parent_index == 0 {
                    lane
                } else {
                    free_side_slot(&mut lanes)
                };
                let color = if mainline.contains(parent.as_str()) {
                    MAIN_COLOR
                } else if parent_index == 0 {
                    row_color
                } else {
                    fresh_color(&mut next_color)
                };
                lanes[target].push((parent.clone(), color));
                bottoms.push((target, color));
            }

            let bottom_to_lane = bottoms.iter().any(|(target, _)| *target == lane);
            let mut strokes = Vec::new();
            if arrival.is_some() {
                strokes.push(GraphStroke {
                    from: lane,
                    to: lane,
                    half: GraphHalf::Top,
                    color: row_color,
                });
            }
            // Every column still waiting for something draws straight down.
            // The arrival column is already covered by Top, plus Bottom when
            // a parent continues in it — except a second edge can share that
            // column (a side branch already queued behind the main waiter),
            // which still needs its own Through segment to stay continuous.
            for (index, queue) in lanes.iter().enumerate() {
                let Some((hash, color)) = queue.first() else {
                    continue;
                };
                if Some(index) == arrival && bottom_to_lane {
                    let shared = before
                        .get(index)
                        .is_some_and(|pending| pending.iter().any(|(wait, _)| wait == hash));
                    if !shared {
                        continue;
                    }
                }
                strokes.push(GraphStroke {
                    from: index,
                    to: index,
                    half: GraphHalf::Through,
                    color: *color,
                });
            }
            for (target, color) in bottoms {
                strokes.push(GraphStroke {
                    from: lane,
                    to: target,
                    half: GraphHalf::Bottom,
                    color,
                });
            }
            let occupied = lanes
                .iter()
                .rposition(|queue| !queue.is_empty())
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

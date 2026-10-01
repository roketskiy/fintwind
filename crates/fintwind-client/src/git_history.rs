//! In-memory commit graph layout. Call once when a history fetch completes,
//! not from a row builder or a frame's render path.
// Lane allocation adapted from Ely GPUI Components, src/git/graph.rs,
// commit e17e31a6890c09ebcfa8b61133d7bc7c625edf69, under the MIT license.
// See docs/licenses/ely-gpui-components-MIT.txt for copyright and permission.

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
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphRow {
    pub lane: usize,
    pub strokes: Vec<GraphStroke>,
}

#[derive(Clone, Debug, Default)]
pub struct CommitGraph {
    pub rows: Vec<GraphRow>,
    /// Shared span keeps the same lane at the same x coordinate in every row.
    pub width: usize,
}

impl CommitGraph {
    /// Input must be topologically ordered (children before all their parents).
    /// Parents outside a truncated window keep outgoing lines at its bottom.
    pub fn new(commits: &[CommitEntry]) -> Self {
        // Reserve lane zero for the displayed tip's first-parent ancestry.
        // A side branch may reach a common ancestor earlier in topo order;
        // keep its incoming lane until that ancestor instead of moving the
        // main line into the side branch's lane.
        let by_hash: HashMap<_, _> = commits
            .iter()
            .map(|commit| (commit.hash.as_str(), commit))
            .collect();
        let mut mainline = HashSet::new();
        let mut next = commits.first().map(|commit| commit.hash.as_str());
        while let Some(hash) = next {
            if !mainline.insert(hash) {
                break;
            }
            next = by_hash
                .get(hash)
                .and_then(|commit| commit.parents.first())
                .map(String::as_str);
        }
        let mut waiting: Vec<Option<&str>> = Vec::new();
        let mut graph = Self::default();
        for commit in commits {
            let before = waiting.clone();
            let on_mainline = mainline.contains(commit.hash.as_str());
            let lane = if on_mainline {
                if waiting.is_empty() {
                    waiting.push(None);
                }
                0
            } else {
                before
                    .iter()
                    .position(|wait| *wait == Some(commit.hash.as_str()))
                    .or_else(|| waiting.iter().position(Option::is_none))
                    .unwrap_or_else(|| {
                        waiting.push(None);
                        waiting.len() - 1
                    })
            };
            for wait in waiting
                .iter_mut()
                .filter(|wait| **wait == Some(commit.hash.as_str()))
            {
                *wait = None;
            }
            let mut strokes: Vec<_> = before
                .iter()
                .enumerate()
                .filter_map(|(at, wait)| {
                    let wait = (*wait)?;
                    Some(GraphStroke {
                        from: at,
                        to: if wait == commit.hash { lane } else { at },
                        half: if wait == commit.hash {
                            GraphHalf::Top
                        } else {
                            GraphHalf::Through
                        },
                    })
                })
                .collect();
            for (index, parent) in commit.parents.iter().enumerate() {
                let target = if on_mainline && index == 0 {
                    // Do not consume another lane waiting for this ancestor:
                    // both lines terminate at its node through Top strokes.
                    waiting[lane] = Some(parent);
                    lane
                } else {
                    match waiting
                        .iter()
                        .position(|wait| *wait == Some(parent.as_str()))
                    {
                        Some(joined) => joined,
                        None if index == 0 => {
                            waiting[lane] = Some(parent);
                            lane
                        }
                        None => {
                            let free =
                                waiting.iter().position(Option::is_none).unwrap_or_else(|| {
                                    waiting.push(None);
                                    waiting.len() - 1
                                });
                            waiting[free] = Some(parent);
                            free
                        }
                    }
                };
                strokes.push(GraphStroke {
                    from: lane,
                    to: target,
                    half: GraphHalf::Bottom,
                });
            }
            graph.width = graph
                .width
                .max(before.len())
                .max(waiting.len())
                .max(lane + 1);
            while waiting.last() == Some(&None) {
                waiting.pop();
            }
            graph.rows.push(GraphRow { lane, strokes });
        }
        graph
    }
}

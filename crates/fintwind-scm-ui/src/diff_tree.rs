use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use fintwind_diff::{
    self as review_diff, Source as ReviewDiffSource,
};
use fintwind_markdown::md;
use fintwind_markdown::md::render::Palette as MarkdownPalette;
use fintwind_markdown::md::render::TranscriptSelection;
use fintwind_protocol::tr;
use fintwind_ui::theme::{Theme, code_px, ui_px};
use gpui::{AnyElement, Div, FontWeight, Hsla, ParentElement, SharedString, Styled, TextRun, div, font, prelude::*, px};
use uuid::Uuid;

pub struct PendingGitQuery {
    pub id: Uuid,
    pub client: fintwind_client::WorkspaceClient,
}

/// One rendered row of the source-control page's single-file diff reader.
/// Rows are flattened out of the snapshot ahead of time so the virtualized
/// list never re-derives pairing per frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceControlDiffRow {
    /// A full-width row mapping to one snapshot line: a hunk header, a folded
    /// gap, a meta note, or a code line in the unified layout.
    Line(usize),
    /// A side-by-side row: the left (old) and right (new) snapshot line
    /// indexes. `None` leaves that side blank.
    Pair(Option<usize>, Option<usize>),
}

/// The snapshot line range of one file's diff: from after its header row to
/// before the next file's header, or the end of the snapshot.
pub fn source_control_file_line_range(
    snapshot: &review_diff::Snapshot,
    file_index: usize,
) -> std::ops::Range<usize> {
    let start = snapshot
        .lines
        .iter()
        .position(|line| line.file_index == file_index)
        .map(|start| {
            // The file's header row moves to the fixed file header above the
            // list; the rows start with the first hunk.
            start + 1
        })
        .unwrap_or(snapshot.lines.len());
    let end = snapshot
        .lines
        .iter()
        .skip(start)
        .position(|line| line.file_index != file_index)
        .map(|offset| start + offset)
        .unwrap_or(snapshot.lines.len());
    start..end
}

/// Side-by-side rows for one file's line range: runs of deletions pair with
/// the additions that follow them, context carries both sides, and hunk
/// headers, gaps, and meta notes stay full width.
pub fn source_control_split_rows(
    snapshot: &review_diff::Snapshot,
    range: std::ops::Range<usize>,
) -> Vec<SourceControlDiffRow> {
    use review_diff::LineKind;

    let lines = &snapshot.lines[range.clone()];
    let mut rows = Vec::new();
    let mut at = range.start;
    while at < range.end {
        let index = at - range.start;
        match lines[index].kind {
            LineKind::Context => {
                rows.push(SourceControlDiffRow::Pair(Some(at), Some(at)));
                at += 1;
            }
            LineKind::Deletion | LineKind::Addition => {
                let deletions = lines[index..]
                    .iter()
                    .take_while(|line| line.kind == LineKind::Deletion)
                    .count();
                let additions = lines[index + deletions..]
                    .iter()
                    .take_while(|line| line.kind == LineKind::Addition)
                    .count();
                let pairs = deletions.max(additions);
                for offset in 0..pairs {
                    rows.push(SourceControlDiffRow::Pair(
                        (offset < deletions).then_some(at + offset),
                        (offset < additions).then_some(at + deletions + offset),
                    ));
                }
                at += deletions + additions;
            }
            LineKind::FileHeader | LineKind::HunkHeader | LineKind::Gap(_) | LineKind::Meta => {
                rows.push(SourceControlDiffRow::Line(at));
                at += 1;
            }
        }
    }
    rows
}

/// One flattened row of the source-control second column: a group header, a
/// directory of the change tree, or a file from one of its groups. `title`
/// carries the rename form `origin → path` when Git detected a rename;
/// `untracked` marks an entry Git has no record of, whose discard deletes
/// the file.
#[derive(Default)]
pub struct SourceControlReviewFiles {
    pub uncommitted: SourceControlFileList,
    pub unstaged: SourceControlFileList,
    pub staged: SourceControlFileList,
    pub status_indices: std::collections::HashMap<String, usize>,
}

pub struct NestedRepositoryState {
    pub repository: fintwind_client::git::NestedRepository,
    pub open: bool,
    pub loading: bool,
    pub stale: bool,
    pub request: Option<PendingGitQuery>,
    pub status: Option<fintwind_client::git::WorktreeStatus>,
    /// Whether the nested repository's untracked listing reached its end.
    pub untracked_complete: bool,
    pub error: Option<String>,
    pub children: Vec<NestedRepositoryState>,
}

pub fn take_nested_requests(nodes: &mut [NestedRepositoryState], requests: &mut Vec<PendingGitQuery>) {
    for node in nodes {
        if let Some(request) = node.request.take() {
            requests.push(request);
        }
        node.loading = false;
        node.open = false;
        node.stale = false;
        node.status = None;
        node.untracked_complete = false;
        node.error = None;
        take_nested_requests(&mut node.children, requests);
    }
}

pub fn invalidate_nested_requests(
    nodes: &mut [NestedRepositoryState],
    requests: &mut Vec<PendingGitQuery>,
) {
    for node in nodes {
        if let Some(request) = node.request.take() {
            requests.push(request);
        }
        node.loading = false;
        node.stale = true;
        invalidate_nested_requests(&mut node.children, requests);
    }
}

pub fn collect_stale_nested_paths(nodes: &[NestedRepositoryState], paths: &mut Vec<PathBuf>) {
    for node in nodes {
        if !node.open {
            continue;
        }
        if node.stale && !node.loading && node.repository.initialized {
            paths.push(node.repository.path.clone());
        }
        collect_stale_nested_paths(&node.children, paths);
    }
}

pub fn merge_nested_repositories(
    previous: Vec<NestedRepositoryState>,
    repositories: Vec<fintwind_client::git::NestedRepository>,
    removed_requests: &mut Vec<PendingGitQuery>,
) -> Vec<NestedRepositoryState> {
    let mut previous: std::collections::HashMap<_, _> = previous
        .into_iter()
        .map(|node| (node.repository.path.clone(), node))
        .collect();
    let merged = repositories
        .into_iter()
        .map(|repository| {
            if let Some(mut node) = previous.remove(&repository.path) {
                node.repository = repository;
                node
            } else {
                NestedRepositoryState {
                    repository,
                    open: false,
                    loading: false,
                    stale: false,
                    request: None,
                    status: None,
                    untracked_complete: false,
                    error: None,
                    children: Vec::new(),
                }
            }
        })
        .collect();
    for (_, mut removed) in previous {
        take_nested_requests(std::slice::from_mut(&mut removed), removed_requests);
    }
    merged
}

pub fn find_nested_repository_mut<'a>(
    nodes: &'a mut [NestedRepositoryState],
    path: &Path,
) -> Option<&'a mut NestedRepositoryState> {
    for node in nodes {
        if node.repository.path == path {
            return Some(node);
        }
        if let Some(found) = find_nested_repository_mut(&mut node.children, path) {
            return Some(found);
        }
    }
    None
}

pub fn nested_repository_candidates(status: &fintwind_client::git::WorktreeStatus) -> Vec<String> {
    status
        .entries
        .iter()
        .filter(|entry| entry.worktree_status == '?' && entry.path.ends_with('/'))
        .map(|entry| entry.path.clone())
        .collect()
}

#[derive(Default)]
pub struct SourceControlFileList {
    pub paths: Vec<String>,
    pub indices: std::collections::HashMap<String, usize>,
}

impl SourceControlFileList {
    fn push(&mut self, path: &str) {
        self.indices.insert(path.to_owned(), self.paths.len());
        self.paths.push(path.to_owned());
    }
}

impl SourceControlReviewFiles {
    pub fn from_status(status: Option<&fintwind_client::git::WorktreeStatus>) -> Self {
        let mut files = Self::default();
        if let Some(status) = status {
            for (index, entry) in status.entries.iter().enumerate() {
                files.status_indices.insert(entry.path.clone(), index);
                let staged = source_control_entry_is_staged(entry);
                let unstaged = source_control_entry_is_unstaged(entry);
                if staged || unstaged {
                    files.uncommitted.push(&entry.path);
                }
                if staged {
                    files.staged.push(&entry.path);
                }
                if unstaged {
                    files.unstaged.push(&entry.path);
                }
            }
        }
        files
    }

    pub fn for_source(&self, source: &ReviewDiffSource) -> Option<&SourceControlFileList> {
        match source {
            ReviewDiffSource::Uncommitted => Some(&self.uncommitted),
            ReviewDiffSource::Unstaged => Some(&self.unstaged),
            ReviewDiffSource::Staged => Some(&self.staged),
            _ => None,
        }
    }

    pub fn entry<'a>(
        &self,
        status: Option<&'a fintwind_client::git::WorktreeStatus>,
        path: &str,
    ) -> Option<&'a fintwind_client::git::WorktreeStatusEntry> {
        status?.entries.get(*self.status_indices.get(path)?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceControlGroup {
    Changes,
    Staged,
    Unstaged,
    Untracked,
    Ignored,
}

impl SourceControlGroup {
    pub const ALL: [Self; 3] = [Self::Staged, Self::Unstaged, Self::Untracked];
    pub const REVEALABLE: [Self; 4] = [Self::Changes, Self::Staged, Self::Unstaged, Self::Untracked];

    pub fn key(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Untracked => "untracked",
            Self::Ignored => "ignored",
        }
    }

    pub fn matches(self, entry: &fintwind_client::git::WorktreeStatusEntry) -> bool {
        match self {
            Self::Changes => false,
            Self::Staged => source_control_entry_is_staged(entry),
            Self::Unstaged => {
                source_control_entry_is_unstaged(entry) && entry.worktree_status != '?'
            }
            Self::Untracked => entry.worktree_status == '?',
            Self::Ignored => entry.worktree_status == '!',
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewTreeRetry {
    Refresh,
    Ignored,
    Nested(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceControlRow {
    Header {
        group: SourceControlGroup,
        count: usize,
        incomplete: bool,
    },
    IgnoredHeader {
        open: bool,
        loading: bool,
    },
    IgnoredError {
        message: String,
        retry: Option<ReviewTreeRetry>,
    },
    IgnoredEmpty,
    /// The next page of a directory listing. `directory` is `None` for the
    /// worktree root.
    LoadMore {
        group: SourceControlGroup,
        directory: Option<String>,
        depth: usize,
    },
    /// The listing hit its budget; only a narrower directory can show more.
    ListingIncomplete {
        group: SourceControlGroup,
        directory: Option<String>,
        depth: usize,
    },
    Repository {
        path: PathBuf,
        name: String,
        depth: usize,
        open: bool,
        loading: bool,
        initialized: bool,
    },
    NestedHeader {
        group: SourceControlGroup,
        count: usize,
        depth: usize,
        incomplete: bool,
    },
    NestedDirectory {
        repository: PathBuf,
        path: String,
        depth: usize,
    },
    NestedFile {
        repository: PathBuf,
        group: SourceControlGroup,
        path: String,
        depth: usize,
        letter: char,
        origin: Option<String>,
    },
    Directory {
        group: SourceControlGroup,
        path: String,
        name: String,
        depth: usize,
        open: bool,
    },
    File {
        group: SourceControlGroup,
        path: String,
        title: String,
        name: String,
        depth: usize,
        letter: char,
        untracked: bool,
    },
}

pub fn source_control_nested_rows(
    rows: &mut Vec<SourceControlRow>,
    nodes: &[NestedRepositoryState],
    depth: usize,
    filter: &str,
) {
    for node in nodes {
        rows.push(SourceControlRow::Repository {
            path: node.repository.path.clone(),
            name: node.repository.relative_path.clone(),
            depth,
            open: node.open,
            loading: node.loading,
            initialized: node.repository.initialized,
        });
        if !node.open {
            continue;
        }
        if let Some(error) = node.error.as_ref() {
            rows.push(SourceControlRow::IgnoredError {
                message: error.clone(),
                retry: Some(ReviewTreeRetry::Nested(node.repository.path.clone())),
            });
        }
        if let Some(status) = node.status.as_ref() {
            let nested_paths: std::collections::HashSet<&str> = node
                .children
                .iter()
                .map(|child| child.repository.relative_path.as_str())
                .collect();
            for group in SourceControlGroup::ALL {
                let entries: Vec<_> = status
                    .entries
                    .iter()
                    .filter(|entry| {
                        group.matches(entry)
                            && !(group == SourceControlGroup::Untracked
                                && entry.path.ends_with('/')
                                && nested_paths.contains(entry.path.trim_end_matches('/')))
                            && (filter.is_empty()
                                || entry.path.to_ascii_lowercase().contains(filter))
                    })
                    .collect();
                if entries.is_empty() {
                    continue;
                }
                rows.push(SourceControlRow::NestedHeader {
                    group,
                    count: entries
                        .iter()
                        .filter(|entry| !entry.path.ends_with('/'))
                        .count(),
                    depth: depth + 1,
                    incomplete: group == SourceControlGroup::Untracked
                        && (node.error.is_some() || !node.untracked_complete),
                });
                for entry in entries {
                    if entry.path.ends_with('/') {
                        rows.push(SourceControlRow::NestedDirectory {
                            repository: node.repository.path.clone(),
                            path: entry.path.clone(),
                            depth: depth + 1,
                        });
                        continue;
                    }
                    rows.push(SourceControlRow::NestedFile {
                        repository: node.repository.path.clone(),
                        group,
                        path: entry.path.clone(),
                        depth: depth + 1,
                        origin: entry.origin_path.clone().filter(|_| {
                            group == SourceControlGroup::Staged
                                || matches!(entry.worktree_status, 'R' | 'C')
                        }),
                        letter: if group == SourceControlGroup::Staged {
                            entry.index_status
                        } else {
                            entry.worktree_status
                        },
                    });
                }
            }
            if !node.untracked_complete {
                rows.push(SourceControlRow::ListingIncomplete {
                    group: SourceControlGroup::Untracked,
                    directory: None,
                    depth: depth + 1,
                });
            }
        }
        source_control_nested_rows(rows, &node.children, depth + 1, filter);
    }
}

pub fn review_file_status_letter(status: review_diff::FileStatus) -> char {
    match status {
        review_diff::FileStatus::Added => 'A',
        review_diff::FileStatus::Deleted => 'D',
        review_diff::FileStatus::Binary => 'B',
        review_diff::FileStatus::Modified => 'M',
    }
}

#[derive(Clone, Copy)]
pub struct SourceControlTreeItem<'a> {
    pub path: &'a str,
    pub origin_path: Option<&'a str>,
    pub letter: char,
    pub untracked: bool,
}

impl<'a> SourceControlTreeItem<'a> {
    pub fn from_status(
        group: SourceControlGroup,
        entry: &'a fintwind_client::git::WorktreeStatusEntry,
    ) -> Self {
        Self {
            path: &entry.path,
            origin_path: entry.origin_path.as_deref(),
            letter: if group == SourceControlGroup::Staged {
                entry.index_status
            } else {
                entry.worktree_status
            },
            untracked: entry.worktree_status == '?',
        }
    }
}

impl SourceControlRow {
    fn file(group: SourceControlGroup, depth: usize, item: SourceControlTreeItem<'_>) -> Self {
        let title = match item.origin_path {
            Some(origin) => format!("{origin} → {}", item.path),
            None => item.path.to_owned(),
        };
        let name = item
            .path
            .rsplit_once('/')
            .map_or(item.path, |(_, name)| name);
        Self::File {
            group,
            path: item.path.to_owned(),
            title,
            name: name.to_owned(),
            depth,
            letter: item.letter,
            untracked: item.untracked,
        }
    }
}

/// The source-control change tree's row height. Shared by the virtualized
/// list and every row so a uniform height stays exact.
pub const SOURCE_CONTROL_ROW_HEIGHT: f32 = 28.0;

/// One directory level's indent, matching Ely's tree list.
pub const SOURCE_CONTROL_TREE_INDENT: f32 = 16.0;

/// One directory while a group's change tree is built: subdirectories and
/// the files directly inside it, both keyed by name so the walk is stable.
#[derive(Default)]
struct SourceControlTreeNode<'a> {
    directories: std::collections::BTreeMap<String, SourceControlTreeNode<'a>>,
    files: std::collections::BTreeMap<&'a str, SourceControlTreeItem<'a>>,
}

/// The fold set's key for one directory on one side of the tree.
pub fn source_control_directory_key(group: SourceControlGroup, path: &str) -> String {
    format!("{}:{path}", group.key())
}

/// Flattens one group's changed files into directory rows and file rows,
/// depth first. Directories keep their children behind a fold; files keep a
/// stable name order inside each directory.
pub fn source_control_tree_rows<'a>(
    rows: &mut Vec<SourceControlRow>,
    group: SourceControlGroup,
    items: &[SourceControlTreeItem<'a>],
    folded: &std::collections::HashSet<String>,
    force_open: bool,
    next_pages: &HashMap<String, String>,
    incomplete: &HashSet<String>,
) {
    let mut root = SourceControlTreeNode::<'a>::default();
    for &item in items {
        let mut node = &mut root;
        let directory_entry = item.path.ends_with('/');
        let mut segments = item.path.trim_end_matches('/').split('/').peekable();
        while let Some(segment) = segments.next() {
            if segments.peek().is_none() {
                if directory_entry {
                    node.directories.entry(segment.to_owned()).or_default();
                } else {
                    node.files.insert(segment, item);
                }
                break;
            }
            node = node.directories.entry(segment.to_owned()).or_default();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk<'a>(
        node: &SourceControlTreeNode<'a>,
        group: SourceControlGroup,
        prefix: &str,
        depth: usize,
        folded: &std::collections::HashSet<String>,
        force_open: bool,
        next_pages: &HashMap<String, String>,
        incomplete: &HashSet<String>,
        rows: &mut Vec<SourceControlRow>,
    ) {
        for (name, child) in &node.directories {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let key = source_control_directory_key(group, &path);
            let open = force_open
                || if matches!(
                    group,
                    SourceControlGroup::Ignored | SourceControlGroup::Untracked
                ) {
                    folded.contains(&key)
                } else {
                    !folded.contains(&key)
                };
            rows.push(SourceControlRow::Directory {
                group,
                path: path.clone(),
                name: name.clone(),
                depth,
                open,
            });
            if open {
                walk(
                    child,
                    group,
                    &path,
                    depth + 1,
                    folded,
                    force_open,
                    next_pages,
                    incomplete,
                    rows,
                );
                if next_pages.contains_key(&path) {
                    rows.push(SourceControlRow::LoadMore {
                        group,
                        directory: Some(path.clone()),
                        depth: depth + 1,
                    });
                }
                if incomplete.contains(&path) {
                    rows.push(SourceControlRow::ListingIncomplete {
                        group,
                        directory: Some(path.clone()),
                        depth: depth + 1,
                    });
                }
            }
        }
        for &item in node.files.values() {
            rows.push(SourceControlRow::file(group, depth, item));
        }
    }

    walk(
        &root, group, "", 0, folded, force_open, next_pages, incomplete, rows,
    );
    if next_pages.contains_key("") {
        rows.push(SourceControlRow::LoadMore {
            group,
            directory: None,
            depth: 0,
        });
    }
    if incomplete.contains("") {
        rows.push(SourceControlRow::ListingIncomplete {
            group,
            directory: None,
            depth: 0,
        });
    }
}

/// The staged side of a porcelain entry: a real index change, not untracked.
pub fn source_control_entry_is_staged(entry: &fintwind_client::git::WorktreeStatusEntry) -> bool {
    !matches!(entry.index_status, ' ' | '?' | '!')
}

/// The unstaged side: a worktree change or an untracked file. Untracked
/// files belong here so they can be seen (and later staged) like anywhere
/// else in a Git UI.
pub fn source_control_entry_is_unstaged(entry: &fintwind_client::git::WorktreeStatusEntry) -> bool {
    !matches!(entry.worktree_status, ' ' | '!')
        && !(entry.worktree_status == '?' && entry.path.ends_with('/'))
}

/// Faint vertical guides down each level a tree row sits under, after Ely's
/// tree list. A hairline only: the tree's shape should read without more ink.
pub fn source_control_guides(depth: usize, theme: &Theme) -> Div {
    div()
        .absolute()
        .inset_0()
        .pl(px(12.0))
        .flex()
        .children((0..depth).map(|_| {
            div()
                .w(px(SOURCE_CONTROL_TREE_INDENT))
                .h_full()
                .pl(px(5.0))
                .child(div().h_full().border_l_1().border_color(theme.border))
        }))
}

/// Status-letter color following the Git convention Fintwind uses elsewhere:
/// green for additions, red for deletions, amber for everything modified,
/// moved, or conflicting, muted for untracked.
pub fn status_letter_color(letter: char, theme: &Theme) -> gpui::Hsla {
    match letter {
        'A' | 'C' => theme.success_text,
        'D' => theme.danger,
        'M' | 'R' | 'T' | 'U' => theme.warning_text,
        '?' => theme.text_tertiary,
        _ => theme.text_secondary,
    }
}

/// How many of the stat squares read as added; the rest read as removed.
fn diff_stat_split(added: usize, removed: usize) -> Option<usize> {
    let total = added + removed;
    (total > 0).then(|| (added * 5 + total / 2) / total)
}

/// A count grouped for reading, so a five-figure total stays legible in a
/// narrow capsule.
pub fn grouped_count(value: u64) -> String {
    let text = value.to_string();
    let mut grouped = String::with_capacity(text.len() + text.len() / 3);
    for (index, digit) in text.chars().enumerate() {
        if index > 0 && (text.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// A file's line-change stat: the two counts plus five squares shared
/// between them, after Ely's `DiffStat` badge.
pub fn render_diff_stat(added: u64, removed: u64, theme: &Theme) -> Div {
    let added = added as usize;
    let removed = removed as usize;
    let green = diff_stat_split(added, removed);
    let square = |index: usize| {
        let color = match green {
            Some(green) if index < green => theme.success,
            Some(_) => theme.danger,
            None => theme.border,
        };
        div().size(px(5.0)).rounded(px(1.5)).bg(color)
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.0))
        .child(
            div()
                .text_size(ui_px(11.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.success_text)
                .child(format!("+{added}")),
        )
        .child(
            div()
                .text_size(ui_px(11.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.danger_text)
                .child(format!("\u{2212}{removed}")),
        )
        .child(div().flex().gap(px(2.0)).children((0..5).map(square)))
}

pub fn review_diff_gap_icon_path(direction: review_diff::ExpansionDirection) -> &'static str {
    match direction {
        // Pierre's direction attributes and rendered chevrons are inverted by
        // CSS. Fintwind names the data operation directly, so encode the resulting
        // visual here: reveal-from-start points down; reveal-from-end points up.
        review_diff::ExpansionDirection::Start => "icons/chevron-down.svg",
        review_diff::ExpansionDirection::End => "icons/chevron-up.svg",
        review_diff::ExpansionDirection::Both
        | review_diff::ExpansionDirection::All => "icons/chevrons-up-down.svg",
    }
}

pub fn review_diff_gap_tooltip(direction: review_diff::ExpansionDirection) -> String {
    match direction {
        review_diff::ExpansionDirection::Start => tr!("diff.expand_context_below"),
        review_diff::ExpansionDirection::End => tr!("diff.expand_context_above"),
        review_diff::ExpansionDirection::Both => tr!("diff.expand_context"),
        review_diff::ExpansionDirection::All => tr!("diff.expand_all_context"),
    }
}

pub fn review_diff_gap_directions(
    position: review_diff::GapPosition,
    chunked: bool,
) -> &'static [review_diff::ExpansionDirection] {
    use review_diff::{ExpansionDirection, GapPosition};

    match (position, chunked) {
        (GapPosition::Leading, _) => &[ExpansionDirection::End],
        (GapPosition::Trailing, _) => &[ExpansionDirection::Start],
        (GapPosition::Between, false) => &[ExpansionDirection::Both],
        (GapPosition::Between, true) => &[ExpansionDirection::Start, ExpansionDirection::End],
    }
}

#[derive(Clone, Copy)]
pub struct DiffRowStyle {
    /// One line-number column's width, sized to the widest number the diff
    /// shows so the code column never shifts between rows.
    pub number_width: f32,
    pub row_height: f32,
    /// Unified rows carry the old and the new number side by side.
    pub two_numbers: bool,
}

impl DiffRowStyle {
    /// The review page's rows: one file's diff, both sides numbered.
    pub fn review(number_width: f32) -> Self {
        Self {
            number_width,
            row_height: 20.0,
            two_numbers: true,
        }
    }

    /// A change card's rows: the same code, one number column, because a
    /// provider's synthesised patch often has no positions at all.
    pub fn activity(number_width: f32) -> Self {
        Self {
            number_width,
            row_height: 20.0,
            two_numbers: false,
        }
    }
}

/// Selection identity for one diff code row. Selection resolves a drag by
/// looking rows up by key, so every row must have its own.
///
/// Rows with line numbers key on them: they survive the review page's gap
/// expansion, where a revealed gap shifts every later row's index. Rows
/// without them 鈥?a diff synthesized from a provider's before/after text 鈥?/// key on the row index instead, which is stable there because an activity
/// diff is only ever rebuilt whole. Keying those on their (absent) numbers
/// gave every added row the same key, and a drag resolved against whichever
/// duplicate registered first: selections jumped rows, skipped wrapped lines,
/// and collapsed when the head crossed into context.
pub fn diff_row_selection_key(
    key_prefix: &str,
    line: &review_diff::Line,
    index: usize,
) -> String {
    let kind = match &line.kind {
        review_diff::LineKind::Context => "context",
        review_diff::LineKind::Addition => "addition",
        review_diff::LineKind::Deletion => "deletion",
        _ => "other",
    };
    match (line.old_line, line.new_line) {
        (None, None) => format!("{key_prefix}-line-{}-{kind}-i{index}", line.file_index),
        (old, new) => format!(
            "{key_prefix}-line-{}-{kind}-{}-{}",
            line.file_index,
            old.unwrap_or(0),
            new.unwrap_or(0),
        ),
    }
}

/// The widest line number in `range`, at least two digits.
pub fn review_diff_digits(
    snapshot: &review_diff::Snapshot,
    range: Range<usize>,
) -> usize {
    snapshot
        .lines
        .get(range)
        .unwrap_or_default()
        .iter()
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .max()
        .unwrap_or(1)
        .to_string()
        .len()
        .max(2)
}

/// One number column's width for a given digit count, so the code column
/// keeps the same place on every row.
pub fn review_diff_number_width(digits: usize) -> f32 {
    let size: f32 = code_px(10.5).into();
    (size * (digits as f32 * 0.62 + 0.8)).max(size * 1.6)
}

/// One context, addition, or deletion row, shared by the review page and the
/// diff inside an expanded file-change activity so the two never drift.
pub fn render_diff_code_row(
    line: &review_diff::Line,
    index: usize,
    key_prefix: &str,
    selection: &TranscriptSelection,
    style: DiffRowStyle,
    theme: &Theme,
) -> AnyElement {
    let (marker, body_background, sign_color) = match &line.kind {
        review_diff::LineKind::Addition => (
            "+",
            Some(theme.success.opacity(DIFF_ROW_WASH)),
            theme.success_text,
        ),
        review_diff::LineKind::Deletion => (
            "\u{2212}",
            Some(theme.danger.opacity(DIFF_ROW_WASH)),
            theme.danger_text,
        ),
        _ => (" ", None, theme.text_ghost),
    };
    let flat = review_diff_flat_text(line, theme);
    let selectable = md::render::selectable_flat_text(
        &flat,
        md::selection::TextKey::new(diff_row_selection_key(key_prefix, line, index), 0),
        selection.clone(),
        theme.code_wash,
        theme.selection,
        false,
    );
    let number = |value: Option<u32>| {
        div()
            .w(px(style.number_width))
            .flex_none()
            .text_right()
            .children(value.map(|value| value.to_string()))
    };
    let mut gutter = div()
        .min_h(px(style.row_height))
        .self_stretch()
        .flex_none()
        .pl(px(8.0))
        .pr(px(8.0))
        .flex()
        .gap(px(6.0))
        .items_start()
        .text_color(theme.text_tertiary);
    if style.two_numbers {
        gutter = gutter
            .child(number(line.old_line))
            .child(number(line.new_line));
    } else {
        gutter = gutter.child(number(line.new_line.or(line.old_line)));
    }
    let body = div()
        .min_h(px(style.row_height))
        .self_stretch()
        .min_w_0()
        .flex_1()
        .flex()
        .items_start()
        .child(
            div()
                .id(SharedString::from(format!("{key_prefix}-sign-{index}")))
                .w(px(10.0))
                .min_h(px(style.row_height))
                .flex_none()
                .text_color(sign_color)
                .child(marker),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "{key_prefix}-line-content-{index}"
                )))
                .min_h(px(style.row_height))
                .min_w_0()
                .flex_1()
                .pl(px(2.0))
                .pr(px(10.0))
                .flex()
                .items_start()
                .overflow_hidden()
                .whitespace_normal()
                .child(selectable),
        );
    div()
        .id(SharedString::from(format!("{key_prefix}-row-{index}")))
        .w_full()
        .min_w_0()
        .min_h(px(style.row_height))
        // A wrapped line makes the row taller than one line. Stacked in a
        // scrolling column, a shrinkable row would be squeezed back to one
        // and paint its overflow over the row beneath it.
        .flex_none()
        .flex()
        .items_stretch()
        .when_some(body_background, |row, background| row.bg(background))
        .font_family(md::render::mono_family())
        .text_size(code_px(10.5))
        .line_height(code_px(style.row_height))
        .child(gutter)
        .child(body)
        .into_any_element()
}

/// The faint row wash the review reader uses. The changed words inside the
/// row are washed harder, so an edit reads without either half shouting.
pub const DIFF_ROW_WASH: f32 = 0.08;

pub fn review_diff_flat_text(line: &review_diff::Line, theme: &Theme) -> md::render::FlatText {
    let text = line.content.clone();
    let palette = MarkdownPalette::from_theme(theme);
    let code_font = font(md::render::mono_family());
    let mut runs = Vec::with_capacity(line.tokens.len() * 2 + 1);
    let mut offset = 0;
    let mut push = |len: usize, color: Hsla| {
        if len > 0 {
            runs.push(TextRun {
                len,
                font: code_font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    };
    for token in &line.tokens {
        if token.range.start > offset {
            push(token.range.start - offset, theme.text_secondary);
        }
        push(token.range.len(), palette.token(token.class));
        offset = token.range.end;
    }
    if offset < text.len() {
        push(text.len() - offset, theme.text_secondary);
    }
    if !line.words.is_empty() {
        let wash = match line.kind {
            review_diff::LineKind::Addition => Some(theme.success.opacity(WORD_WASH)),
            review_diff::LineKind::Deletion => Some(theme.danger.opacity(WORD_WASH)),
            _ => None,
        };
        if let Some(wash) = wash {
            runs = wash_word_runs(runs, &line.words, wash);
        }
    }
    md::render::FlatText {
        text: text.into(),
        runs,
        links: Vec::new(),
        code_ranges: Vec::new(),
        active_link: Rc::default(),
        math: None,
        references: Vec::new(),
        allow_references: false,
    }
}

/// The wash behind the words that changed, matching the diff viewer this
/// surface follows: the row stays faint, the edit itself reads at full
/// strength.
const WORD_WASH: f32 = 0.25;

/// Splits syntax runs so the changed words carry a background. Every input
/// run becomes one or more runs with the same total length, so the text still
/// lays out exactly as it did. Ranges are sorted and non-overlapping by
/// construction; clamping to the cursor keeps that a hard guarantee rather
/// than an assumption.
fn wash_word_runs(runs: Vec<TextRun>, words: &[Range<usize>], wash: Hsla) -> Vec<TextRun> {
    let mut output = Vec::with_capacity(runs.len() + words.len() * 2);
    let mut offset = 0;
    for run in runs {
        let start = offset;
        let end = offset + run.len;
        offset = end;
        let mut cursor = start;
        for word in words {
            let word_start = word.start.max(start).max(cursor);
            let word_end = word.end.min(end);
            if word_start >= word_end {
                continue;
            }
            if cursor < word_start {
                output.push(run_segment(&run, cursor - start, word_start - start, None));
            }
            output.push(run_segment(
                &run,
                word_start - start,
                word_end - start,
                Some(wash),
            ));
            cursor = word_end;
        }
        if cursor < end {
            output.push(run_segment(&run, cursor - start, end - start, None));
        }
    }
    output
}

fn run_segment(run: &TextRun, start: usize, end: usize, background: Option<Hsla>) -> TextRun {
    TextRun {
        len: end - start,
        font: run.font.clone(),
        color: run.color,
        background_color: background.or(run.background_color),
        underline: run.underline,
        strikethrough: run.strikethrough,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review_file(path: &str) -> review_diff::File {
        review_diff::File {
            path: path.into(),
            additions: 1,
            deletions: 0,
            status: review_diff::FileStatus::Modified,
            diff_line: None,
        }
    }

    fn split_test_line(
        file_index: usize,
        kind: review_diff::LineKind,
    ) -> review_diff::Line {
        review_diff::Line {
            file_index,
            old_line: None,
            new_line: None,
            kind,
            content: String::new(),
            tokens: Vec::new(),
            words: Vec::new(),
        }
    }

    fn split_test_snapshot(lines: Vec<review_diff::Line>) -> review_diff::Snapshot {
        review_diff::Snapshot {
            source: review_diff::Source::default(),
            files: vec![review_file("a.rs")],
            lines,
            additions: 0,
            deletions: 0,
            truncated: false,
        }
    }

    #[test]
    fn split_rows_pair_deletions_with_the_additions_that_follow() {
        use review_diff::LineKind::{Addition, Context, Deletion, FileHeader, HunkHeader};

        let snapshot = split_test_snapshot(vec![
            split_test_line(0, FileHeader),
            split_test_line(0, HunkHeader),
            split_test_line(0, Context),
            split_test_line(0, Deletion),
            split_test_line(0, Deletion),
            split_test_line(0, Addition),
            split_test_line(0, Context),
            split_test_line(0, Addition),
        ]);
        let rows =
            source_control_split_rows(&snapshot, source_control_file_line_range(&snapshot, 0));
        assert_eq!(
            rows,
            vec![
                // The hunk header stays full width; the file header is not
                // part of the range at all.
                SourceControlDiffRow::Line(1),
                SourceControlDiffRow::Pair(Some(2), Some(2)),
                SourceControlDiffRow::Pair(Some(3), Some(5)),
                SourceControlDiffRow::Pair(Some(4), None),
                SourceControlDiffRow::Pair(Some(6), Some(6)),
                SourceControlDiffRow::Pair(None, Some(7)),
            ]
        );
    }

    #[test]
    fn the_file_range_skips_its_header_and_stops_at_the_next_file() {
        use review_diff::LineKind::{Context, FileHeader};

        let mut snapshot = split_test_snapshot(vec![
            split_test_line(0, FileHeader),
            split_test_line(0, Context),
            split_test_line(1, FileHeader),
            split_test_line(1, Context),
        ]);
        snapshot.files.push(review_file("b.rs"));
        assert_eq!(source_control_file_line_range(&snapshot, 0), 1..2);
        assert_eq!(source_control_file_line_range(&snapshot, 1), 3..4);
        assert_eq!(source_control_file_line_range(&snapshot, 9), 4..4);
    }

    #[test]
    fn review_gap_expansion_icons_match_pierre_visual_directions() {
        use review_diff::{ExpansionDirection, GapPosition};

        assert_eq!(
            review_diff_gap_directions(GapPosition::Leading, true),
            &[ExpansionDirection::End]
        );
        assert_eq!(
            review_diff_gap_directions(GapPosition::Trailing, true),
            &[ExpansionDirection::Start]
        );
        assert_eq!(
            review_diff_gap_directions(GapPosition::Between, false),
            &[ExpansionDirection::Both]
        );
        assert_eq!(
            review_diff_gap_directions(GapPosition::Between, true),
            &[ExpansionDirection::Start, ExpansionDirection::End]
        );

        assert_eq!(
            review_diff_gap_icon_path(ExpansionDirection::Start),
            "icons/chevron-down.svg"
        );
        assert_eq!(
            review_diff_gap_icon_path(ExpansionDirection::End),
            "icons/chevron-up.svg"
        );
        assert_eq!(
            review_diff_gap_icon_path(ExpansionDirection::Both),
            "icons/chevrons-up-down.svg"
        );
    }
}

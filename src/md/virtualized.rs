//! Complete reasoning history, prepared off-thread and rendered in bounded rows.

use std::{collections::HashMap, rc::Rc, sync::Arc};

use gpui::{
    Context, FocusHandle, FollowMode, KeyDownEvent, ListAlignment, ListOffset, ListState, Render,
    Window, div, list, prelude::*, px,
};

use super::{
    parser::{Block, IncrementalParser, InlineRun},
    render::{self, LinkHandler, MarkdownView, Metrics, Palette, TranscriptSelection},
};
use crate::{
    theme::Theme,
    ui::{
        scrollbar::{self, Scrollable, ScrollbarState},
        tooltip::Tooltip,
    },
};

const ROW_BYTES: usize = 2048;
pub const THRESHOLD: usize = 6 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Decoration {
    Quote,
    List {
        marker: Option<String>,
        task: Option<bool>,
        ordered: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub block: Block,
    pub decorations: Vec<Decoration>,
    pub gap: bool,
    pub code: Option<(usize, bool, bool)>,
    pub table: Option<TablePart>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TablePart {
    pub widths: Arc<[f32]>,
    pub header: bool,
    pub first: bool,
    pub last: bool,
}

/// Split parsed text, not Markdown source: fences, links and emphasis survive.
fn text_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut remaining = text;
    while remaining.len() > ROW_BYTES {
        let mut end = ROW_BYTES;
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        if let Some((offset, ch)) = remaining[..end]
            .char_indices()
            .rev()
            .find(|(offset, ch)| *offset >= ROW_BYTES / 2 && ch.is_whitespace())
        {
            end = offset + ch.len_utf8();
        }
        parts.push(&remaining[..end]);
        remaining = &remaining[end..];
    }
    if !remaining.is_empty() || parts.is_empty() {
        parts.push(remaining);
    }
    parts
}

fn split_runs(runs: &[InlineRun]) -> Vec<Vec<InlineRun>> {
    let mut chunks = vec![Vec::new()];
    let mut bytes = 0;
    for run in runs {
        if run.style.math {
            if bytes > 0 && bytes + run.text.len() > ROW_BYTES {
                chunks.push(Vec::new());
                bytes = 0;
            }
            chunks.last_mut().unwrap().push(run.clone());
            bytes += run.text.len();
            continue;
        }
        for part in text_parts(&run.text) {
            if bytes > 0 && bytes + part.len() > ROW_BYTES {
                chunks.push(Vec::new());
                bytes = 0;
            }
            chunks.last_mut().unwrap().push(InlineRun {
                text: part.to_owned(),
                style: run.style.clone(),
            });
            bytes += part.len();
        }
    }
    chunks
}

fn push_block(block: &Block, rows: &mut Vec<Arc<Row>>, codes: &mut HashMap<usize, Arc<str>>) {
    let first = rows.len();
    let mut push = |block| {
        rows.push(Arc::new(Row {
            block,
            decorations: Vec::new(),
            gap: true,
            code: None,
            table: None,
        }))
    };
    match block {
        Block::Paragraph { runs } | Block::Heading { runs, .. } => {
            for runs in split_runs(runs) {
                push(match block {
                    Block::Heading { level, .. } => Block::Heading {
                        level: *level,
                        runs,
                    },
                    _ => Block::Paragraph { runs },
                });
            }
        }
        Block::CodeBlock { language, code } => {
            codes.insert(first, Arc::from(code.as_str()));
            let parts = text_parts(code);
            let count = parts.len();
            for (index, code) in parts.into_iter().enumerate() {
                rows.push(Arc::new(Row {
                    block: Block::CodeBlock {
                        language: language.clone(),
                        code: code.to_owned(),
                    },
                    decorations: Vec::new(),
                    gap: index == 0,
                    code: Some((first, index == 0, index + 1 == count)),
                    table: None,
                }));
            }
        }
        Block::BlockQuote { children } => {
            for child in children {
                push_block(child, rows, codes);
            }
            for row in &mut rows[first..] {
                Arc::make_mut(row).decorations.insert(0, Decoration::Quote);
            }
            return;
        }
        Block::List {
            ordered_start,
            items,
        } => {
            for (index, item) in items.iter().enumerate() {
                let item_start = rows.len();
                for child in &item.blocks {
                    push_block(child, rows, codes);
                }
                for (part, row) in rows[item_start..].iter_mut().enumerate() {
                    let row = Arc::make_mut(row);
                    row.decorations.insert(
                        0,
                        Decoration::List {
                            marker: (part == 0 && item.task.is_none()).then(|| {
                                ordered_start.map_or_else(
                                    || "\u{2022}".into(),
                                    |start| format!("{}.", start + index as u64),
                                )
                            }),
                            task: (part == 0).then_some(item.task).flatten(),
                            ordered: ordered_start.is_some(),
                        },
                    );
                }
            }
            return;
        }
        Block::Table {
            header,
            rows: body,
            align,
        } => {
            let columns = header.len().max(1);
            let widths: Arc<[f32]> = render::column_widths(header, body, columns).into();
            for (index, cells) in std::iter::once(header).chain(body).enumerate() {
                let cells = cells
                    .iter()
                    .map(|cell| split_runs(cell))
                    .collect::<Vec<_>>();
                let count = cells.iter().map(Vec::len).max().unwrap_or(1);
                for part in 0..count {
                    rows.push(Arc::new(Row {
                        block: Block::Table {
                            header: cells
                                .iter()
                                .map(|cell| cell.get(part).cloned().unwrap_or_default())
                                .collect(),
                            rows: Vec::new(),
                            align: align.clone(),
                        },
                        decorations: Vec::new(),
                        gap: index == 0 && part == 0,
                        code: None,
                        table: Some(TablePart {
                            widths: widths.clone(),
                            header: index == 0,
                            first: index == 0 && part == 0,
                            last: index == body.len() && part + 1 == count,
                        }),
                    }));
                }
            }
            return;
        }
        _ => push(block.clone()),
    }
    for row in rows.iter_mut().skip(first + 1) {
        Arc::make_mut(row).gap = false;
    }
}

#[derive(Default)]
struct Document {
    parser: IncrementalParser,
    block_rows: Vec<usize>,
    rows: Vec<Arc<Row>>,
    codes: HashMap<usize, Arc<str>>,
}

struct Patch {
    from: usize,
    rows: Vec<Arc<Row>>,
    codes: HashMap<usize, Arc<str>>,
}

impl Document {
    fn update(&mut self, text: &str, reset: bool, live: bool) -> Patch {
        let mut block_start = if reset {
            0
        } else {
            self.parser.stable_prefix()
        };
        if reset {
            self.parser.reset(text);
        } else {
            self.parser.append(text);
        }
        if self.parser.stable_prefix() == 0 {
            block_start = 0;
        }
        let row_start = self
            .block_rows
            .get(block_start)
            .copied()
            .unwrap_or(self.rows.len());
        self.block_rows.truncate(block_start);
        self.codes.retain(|index, _| *index < row_start);
        let old = self.rows.split_off(row_start);
        let tail = live.then(|| self.parser.display_tail()).flatten();
        let blocks = &self.parser.tree().blocks;
        let tail_start = if tail.is_some() {
            self.parser.display_tail_start()
        } else {
            usize::MAX
        };
        for (index, block) in blocks.iter().enumerate().skip(block_start) {
            self.block_rows.push(self.rows.len());
            if index == tail_start {
                for block in tail.as_ref().unwrap() {
                    push_block(&block.block, &mut self.rows, &mut self.codes);
                }
            } else if index > tail_start {
                // Already replaced as part of the mended source group.
            } else {
                push_block(&block.block, &mut self.rows, &mut self.codes);
            }
        }
        let same = if reset {
            0
        } else {
            old.iter()
                .zip(&self.rows[row_start..])
                .take_while(|(a, b)| a == b)
                .count()
        };
        let from = row_start + same;
        Patch {
            from,
            rows: self.rows[from..].to_vec(),
            codes: self.codes.clone(),
        }
    }
}

pub struct ReasoningView {
    document: Option<Document>,
    rows: Vec<Arc<Row>>,
    codes: HashMap<usize, Arc<str>>,
    pending: String,
    reset: bool,
    dirty: bool,
    generation: u64,
    source_len: usize,
    source_stamp: (u64, u64),
    live: bool,
    list: ListState,
    scrollbar: Rc<ScrollbarState>,
    focus: FocusHandle,
    cache: MarkdownView,
    selection: TranscriptSelection,
    link_handler: LinkHandler,
    row_id: String,
    initial_scroll: Option<gpui::Pixels>,
}

impl ReasoningView {
    pub fn new(
        row_id: String,
        selection: TranscriptSelection,
        link_handler: LinkHandler,
        initial_scroll: Option<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(160.0));
        list.set_follow_mode(FollowMode::Tail);
        let entity = cx.entity().downgrade();
        list.set_scroll_handler(move |_, _, cx| {
            let _ = entity.update(cx, |_, cx| cx.notify());
        });
        Self {
            document: Some(Document::default()),
            rows: Vec::new(),
            codes: HashMap::new(),
            pending: String::new(),
            reset: false,
            dirty: false,
            generation: 0,
            source_len: 0,
            source_stamp: (0, 0),
            live: true,
            list,
            scrollbar: ScrollbarState::new(),
            focus: cx.focus_handle(),
            cache: MarkdownView::new(),
            selection,
            link_handler,
            row_id,
            initial_scroll,
        }
    }

    pub fn set_source(
        &mut self,
        source: &str,
        stamp: (u64, u64),
        live: bool,
        cx: &mut Context<Self>,
    ) {
        if self.source_len == source.len() && self.source_stamp == stamp && self.live == live {
            return;
        }
        if self.source_len == 0 && !live {
            self.list.set_follow_mode(FollowMode::Normal);
            self.initial_scroll.get_or_insert(px(0.0));
        }
        let reset = stamp.0 != self.source_stamp.0
            || source.len() < self.source_len
            || !source.is_char_boundary(self.source_len)
            || (source.len() == self.source_len && stamp != self.source_stamp);
        if reset {
            self.generation += 1;
            self.pending.clear();
            self.reset = true;
        }
        self.pending.push_str(if reset {
            source
        } else {
            &source[self.source_len..]
        });
        self.source_len = source.len();
        self.source_stamp = stamp;
        self.live = live;
        self.dirty = true;
        self.prepare(cx);
    }

    fn prepare(&mut self, cx: &mut Context<Self>) {
        if !self.dirty {
            return;
        }
        let Some(mut document) = self.document.take() else {
            return;
        };
        let pending = std::mem::take(&mut self.pending);
        let reset = std::mem::take(&mut self.reset);
        self.dirty = false;
        let generation = self.generation;
        let live = self.live;
        let work = cx.background_executor().spawn(async move {
            let patch = document.update(&pending, reset, live);
            (document, patch)
        });
        cx.spawn(async move |this, cx| {
            let (document, patch) = work.await;
            let _ = this.update(cx, |this, cx| {
                this.document = Some(document);
                if generation == this.generation {
                    let old_len = this.rows.len();
                    let new_len = patch.from + patch.rows.len();
                    this.rows.truncate(patch.from);
                    this.rows.extend(patch.rows);
                    this.codes = patch.codes;
                    // Remeasure existing rows without replacing their scroll anchor.
                    if new_len != old_len {
                        this.list.splice(
                            old_len.min(new_len)..old_len,
                            new_len.saturating_sub(old_len),
                        );
                    }
                    this.list.remeasure_items(patch.from..new_len);
                    this.cache.invalidate_rows_from(patch.from);
                    if new_len > 0
                        && let Some(offset) = this.initial_scroll.take()
                    {
                        this.list.scroll_to(ListOffset {
                            item_ix: 0,
                            offset_in_item: offset,
                        });
                    }
                    cx.notify();
                }
                this.prepare(cx);
            });
        })
        .detach();
    }
}

impl Render for ReasoningView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let mut palette = Palette::from_theme(&theme);
        palette.text = theme.text_secondary;
        palette.secondary = theme.text_tertiary;
        let view = cx.entity().downgrade();
        let list_state = self.list.clone();
        let wheel_list = self.list.clone();
        div()
            .id("reasoning-history")
            .track_focus(&self.focus)
            .tab_index(0)
            .w_full()
            .h(px(400.0))
            .min_w_0()
            .relative()
            .overflow_hidden()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .child(
                // `List` ignores horizontal padding: it positions items at the
                // element's left edge and measures them against the full border
                // box, so `.px()` on the list itself has no effect. The page
                // margin has to come from a wrapping element instead.
                div().w_full().h_full().min_w_0().px(px(12.0)).child(
                    list(self.list.clone(), move |index, _, cx| {
                        view.update(cx, |this, _| {
                            let ctx = render::Ctx::new(
                                this.row_id.clone(),
                                &palette,
                                Metrics::compact(),
                                this.selection.clone(),
                            )
                            .with_link_handler(this.link_handler.clone())
                            .with_streaming_animation(false);
                            render::virtual_row(
                                &this.rows[index],
                                index,
                                &this.codes,
                                &this.cache,
                                &ctx,
                            )
                        })
                        .unwrap_or_else(|_| div().into_any_element())
                    })
                    .size_full()
                    .py(px(8.0)),
                ),
            )
            .child(scrollbar::vertical(&self.list, &self.scrollbar))
            .on_scroll_wheel(move |_, _, cx| {
                if wheel_list.max_offset() > px(0.5) {
                    cx.stop_propagation();
                }
            })
            .when(
                !self.list.is_following_tail() && self.list.is_scrolled_to_end() != Some(true),
                |el| {
                    el.child(
                        div()
                            .id("reasoning-latest")
                            .tab_index(0)
                            .absolute()
                            .bottom(px(8.0))
                            .right(px(14.0))
                            .size(px(28.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .bg(theme.surface)
                            .border_1()
                            .border_color(theme.border_strong)
                            .focus_visible(|style| style.border_color(theme.accent))
                            .tooltip(Tooltip::text(tr!("transcript.scroll_to_bottom")))
                            .child(crate::ui::icon(
                                "icons/arrow-down.svg",
                                14.0,
                                theme.text_secondary,
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.list.set_follow_mode(FollowMode::Normal);
                                this.list.set_follow_mode(FollowMode::Tail);
                                cx.notify();
                            }))
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    this.list.set_follow_mode(FollowMode::Normal);
                                    this.list.set_follow_mode(FollowMode::Tail);
                                    cx.stop_propagation();
                                    cx.notify();
                                }
                            })),
                    )
                },
            )
            .on_key_down(cx.listener(move |_, event: &KeyDownEvent, _, cx| {
                let delta = match event.keystroke.key.as_str() {
                    "up" => -24.0,
                    "down" => 24.0,
                    "pageup" => -360.0,
                    "pagedown" => 360.0,
                    "home" => {
                        list_state.pause_following_tail();
                        list_state.scroll_to(ListOffset::default());
                        0.0
                    }
                    "end" => {
                        list_state.set_follow_mode(FollowMode::Normal);
                        list_state.set_follow_mode(FollowMode::Tail);
                        0.0
                    }
                    _ => return,
                };
                if delta != 0.0 {
                    list_state.pause_following_tail();
                    Scrollable::scroll_to(
                        &list_state,
                        (list_state.scrolled() + px(delta)).max(px(0.0)),
                    );
                }
                cx.stop_propagation();
                cx.notify();
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(row: &Row) -> String {
        match &row.block {
            Block::Paragraph { runs } | Block::Heading { runs, .. } => {
                runs.iter().map(|run| run.text.as_str()).collect()
            }
            Block::CodeBlock { code, .. } => code.clone(),
            _ => String::new(),
        }
    }

    #[test]
    fn long_reasoning_keeps_every_character_and_bounds_each_row() {
        let prose = "\u{754c}\u{601d} **bold** ".repeat(10_000);
        let mut document = Document::default();
        document.update(&prose, true, true);
        assert!(document.rows.len() > 48);
        let actual = document
            .rows
            .iter()
            .map(|row| text(row))
            .collect::<String>();
        let expected = "\u{754c}\u{601d} bold ".repeat(10_000);
        assert_eq!(actual.len(), expected.trim_end().len());
        assert!(actual == expected.trim_end());
        assert!(document.rows.iter().all(|row| text(row).len() <= ROW_BYTES));
        assert!(document.rows.iter().any(|row| matches!(&row.block, Block::Paragraph { runs } if runs.iter().any(|run| run.style.bold))));
        let first = document.rows[0].clone();
        let patch = document.update(" more", false, true);
        assert!(patch.from > 0);
        assert_eq!(&*document.rows[0], &*first);
    }

    #[test]
    fn code_and_nested_lists_keep_content_and_markers() {
        let code = "const s = '\u{754c}';\n".repeat(4_000);
        let source = format!("> 7. {}\n\n```js\n{code}```", "item ".repeat(4_000));
        let mut document = Document::default();
        document.update(&source, true, false);
        let code_rows = document
            .rows
            .iter()
            .filter(|row| row.code.is_some())
            .collect::<Vec<_>>();
        assert_eq!(
            code_rows.iter().map(|row| text(row)).collect::<String>(),
            code.trim_end_matches('\n')
        );
        assert!(code_rows.iter().all(|row| text(row).len() <= ROW_BYTES));
        assert_eq!(
            code_rows.iter().filter(|row| row.code.unwrap().1).count(),
            1
        );
        assert_eq!(
            code_rows.iter().filter(|row| row.code.unwrap().2).count(),
            1
        );
        let code_id = code_rows[0].code.unwrap().0;
        assert_eq!(
            document.codes[&code_id].as_ref(),
            code.trim_end_matches('\n')
        );
        assert_eq!(document.rows.iter().flat_map(|row| &row.decorations)
            .filter(|decoration| matches!(decoration, Decoration::List { marker: Some(marker), .. } if marker == "7.")).count(), 1);
    }

    #[test]
    fn patches_replay_appends_finalization_references_and_reset() {
        let mut document = Document::default();
        let mut displayed = Vec::new();
        let mut source = String::new();
        for (delta, reset, live) in [
            ("First.\n\n[reference][link]\n\n```js\n", true, true),
            ("let a = 1;\n", false, true),
            ("```\n\n[link]: https://example.com\n\n**last", false, true),
            ("", false, false),
            ("replacement", true, false),
            ("", true, false),
        ] {
            if reset {
                source.clear();
            }
            source.push_str(delta);
            let patch = document.update(delta, reset, live);
            displayed.truncate(patch.from);
            displayed.extend(patch.rows);
            let mut fresh = Document::default();
            fresh.update(&source, true, live);
            assert_eq!(displayed, fresh.rows);
            assert_eq!(document.codes, fresh.codes);
        }
    }

    #[test]
    fn tail_updates_preserve_a_readers_logical_anchor() {
        let state = ListState::new(100, ListAlignment::Top, px(160.0));
        state.scroll_to(ListOffset {
            item_ix: 5,
            offset_in_item: px(12.0),
        });
        state.splice(100..100, 20);
        state.remeasure_items(99..120);
        assert_eq!(state.logical_scroll_top().item_ix, 5);
        assert_eq!(state.logical_scroll_top().offset_in_item, px(12.0));
    }

    #[gpui::test]
    fn queued_appends_and_replaced_sources_do_not_publish_stale_rows(
        cx: &mut gpui::TestAppContext,
    ) {
        let view = cx.new(|cx| {
            ReasoningView::new(
                "test".into(),
                TranscriptSelection::default(),
                Rc::new(|_, _, _| {}),
                None,
                cx,
            )
        });
        view.update(cx, |view, cx| {
            view.set_source("old thought", (1, 1), true, cx);
            view.set_source("replacement", (2, 2), true, cx);
            view.set_source("replacement and more", (2, 3), true, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.rows.iter().map(|row| text(row)).collect::<String>(),
                "replacement and more"
            );
            assert_eq!(view.list.item_count(), view.rows.len());
            assert!(view.document.is_some());
        });
    }
}

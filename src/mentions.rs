//! Paint-only inline skill/file references shared by drafts and user messages.
//! The catalog wraps prefetched indexes. Matching never touches disk or IPC,
//! and decoration keeps every UTF-8 byte offset intact for native text editing.

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, FontWeight, Hsla, IntoElement, SharedString, Styled,
    TextLayout, TextRun, Window, canvas, point, px, size,
};

use crate::composer_complete::{CommandScope, FileEntry, SlashCommand};

pub(crate) const SKILL_ICON: &str = "icons/package.svg";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Skill,
    File(&'static str),
    Directory,
}

#[derive(Clone, Debug)]
pub(crate) struct Span {
    pub range: Range<usize>,
    pub kind: Kind,
}

impl Span {
    fn has_icon(&self) -> bool {
        !matches!(self.kind, Kind::Directory)
    }
}

pub(crate) struct Catalog {
    commands: Rc<Vec<SlashCommand>>,
    files: Rc<Vec<FileEntry>>,
    skills: HashSet<String>,
    file_icon: fn(&str) -> &'static str,
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new(
            Rc::new(Vec::new()),
            Rc::new(Vec::new()),
            |_| "icons/file-types/file.svg",
        )
    }
}

impl Catalog {
    pub fn new(
        commands: Rc<Vec<SlashCommand>>,
        files: Rc<Vec<FileEntry>>,
        file_icon: fn(&str) -> &'static str,
    ) -> Self {
        let skills = commands
            .iter()
            .filter(|command| command.scope == CommandScope::Skill)
            .map(|command| command.name.clone())
            .collect();
        Self {
            commands,
            files,
            skills,
            file_icon,
        }
    }

    pub fn has_sources(
        &self,
        commands: &Rc<Vec<SlashCommand>>,
        files: &Rc<Vec<FileEntry>>,
    ) -> bool {
        Rc::ptr_eq(&self.commands, commands) && Rc::ptr_eq(&self.files, files)
    }

    fn file(&self, path: &str) -> Option<Kind> {
        // ListProjectFiles orders entries by depth then path. Reuse that index
        // directly instead of hashing/cloning 50,000 paths on the UI thread.
        let depth = path
            .matches('/')
            .count()
            .saturating_sub(usize::from(path.ends_with('/')));
        let index = self
            .files
            .binary_search_by(|entry| {
                let entry_depth = entry
                    .path
                    .matches('/')
                    .count()
                    .saturating_sub(usize::from(entry.is_dir));
                (entry_depth, entry.path.as_str()).cmp(&(depth, path))
            })
            .ok()?;
        Some(if self.files[index].is_dir {
            Kind::Directory
        } else {
            Kind::File((self.file_icon)(path))
        })
    }

    pub fn find(&self, text: &str) -> Vec<Span> {
        if self.skills.is_empty() && self.files.is_empty() {
            return Vec::new();
        }
        let mut spans = Vec::new();
        let mut covered = 0;
        let mut code_ticks = 0;
        let mut ticks_end = 0;
        for (start, sigil) in text.char_indices() {
            if start < covered || start < ticks_end {
                continue;
            }
            if sigil == '`' {
                let count = text[start..]
                    .bytes()
                    .take_while(|byte| *byte == b'`')
                    .count();
                ticks_end = start + count;
                if code_ticks == 0 {
                    code_ticks = count;
                } else if code_ticks == count {
                    code_ticks = 0;
                }
                continue;
            }
            if code_ticks != 0
                || (sigil != '@'
                    && (sigil != '/' || (start != 0 && !text[..start].ends_with('\n'))))
            {
                continue;
            }
            let rest = &text[start + 1..];
            let limit = rest.find(['\n', '\r', '@']).unwrap_or(rest.len()).min(4096);
            let mut matched = None;
            for end in rest
                .char_indices()
                .map(|(index, _)| index)
                .chain([rest.len()])
                .take_while(|end| *end <= limit)
            {
                if end == 0 {
                    continue;
                }
                let tail = &rest[end..];
                if !tail.is_empty()
                    && !tail.starts_with(|ch: char| {
                        ch.is_whitespace()
                            || ",;!?)]}\u{ff0c}\u{3002}\u{ff01}\u{ff1f}\u{ff1b}\u{ff1a}"
                                .contains(ch)
                    })
                {
                    continue;
                }
                let name = &rest[..end];
                let kind = if self.skills.contains(name) {
                    Some(Kind::Skill)
                } else if sigil == '@' {
                    self.file(name).or_else(|| self.file(&format!("{name}/")))
                } else {
                    None
                };
                if let Some(kind) = kind {
                    matched = Some(Span {
                        range: start..start + 1 + end,
                        kind,
                    });
                }
            }
            if let Some(span) = matched {
                covered = span.range.end;
                spans.push(span);
            }
        }
        spans
    }
}

/// Reserve an @-sized icon slot for slash skills, without changing byte lengths
/// or the source used by copy, undo, IME, history and transport.
pub(crate) fn display_text(source: &SharedString, spans: &[Span]) -> SharedString {
    if !spans
        .iter()
        .any(|span| span.has_icon() && source.as_bytes()[span.range.start] == b'/')
    {
        return source.clone();
    }
    let mut display = source.to_string();
    for span in spans.iter().filter(|span| span.has_icon()) {
        display.replace_range(span.range.start..span.range.start + 1, "@");
    }
    display.into()
}

pub(crate) fn style_runs(runs: Vec<TextRun>, spans: &[Span], color: Hsla) -> Vec<TextRun> {
    if spans.is_empty() {
        return runs;
    }
    let mut out = Vec::new();
    let mut offset = 0;
    for run in runs {
        let end = offset + run.len;
        while offset < end {
            let index = spans.partition_point(|span| span.range.end <= offset);
            let span = spans.get(index);
            let mut next = end;
            let mut styled = run.clone();
            if let Some(span) = span {
                if span.range.start <= offset {
                    next = next.min(span.range.end);
                    if styled.font.weight < FontWeight::MEDIUM {
                        styled.font.weight = FontWeight::MEDIUM;
                    }
                    styled.color = color;
                    if span.has_icon() && offset == span.range.start {
                        next = next.min(offset + 1);
                        styled.color = gpui::transparent_black();
                    }
                } else {
                    next = next.min(span.range.start);
                }
            }
            styled.len = next - offset;
            out.push(styled);
            offset = next;
        }
    }
    out
}

pub(crate) fn prepaint_icons(
    layout: &TextLayout,
    spans: &[Span],
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let mut icons = Vec::new();
    for span in spans.iter().filter(|span| span.has_icon()) {
        // Resolve the glyph itself, not the caret at a soft-wrap boundary
        // (which has preceding-line affinity in GPUI).
        let Some(after) = layout.position_for_index(span.range.start + 1) else {
            continue;
        };
        let Some(before) = layout.position_for_index(span.range.start) else {
            continue;
        };
        let origin = if before.y == after.y {
            before
        } else {
            point(layout.bounds().left(), after.y)
        };
        let clip = window.content_mask().bounds;
        if origin.y + layout.line_height() < clip.top() || origin.y > clip.bottom() {
            continue;
        }
        let slot = after.x - origin.x;
        let side = (slot - px(1.0))
            .min(layout.line_height() * 0.66)
            .max(px(1.0));
        let mut element = match span.kind {
            Kind::Skill => crate::ui::icon(SKILL_ICON, f32::from(side), color).into_any_element(),
            Kind::File(path) => crate::ui::file_icon(path, f32::from(side)).into_any_element(),
            Kind::Directory => continue,
        };
        element.prepaint_as_root(
            point(origin.x, origin.y + (layout.line_height() - side) / 2.0),
            size(
                AvailableSpace::Definite(side),
                AvailableSpace::Definite(side),
            ),
            window,
            cx,
        );
        icons.push(element);
    }
    icons
}

pub(crate) fn overlay(layout: TextLayout, spans: Vec<Span>, color: Hsla) -> impl IntoElement {
    canvas(
        move |_, window, cx| prepaint_icons(&layout, &spans, color, window, cx),
        |_, mut icons, window, cx| {
            for icon in &mut icons {
                icon.paint(window, cx);
            }
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod e2e;

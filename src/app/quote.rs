//! Composer references: quotes of selected code, transcript messages and files
//! staged as chips above the input, folded into the next submission's prompt.
//!
//! A reference is source-attributed text, not a file upload: nothing here
//! travels on the prompt `files` channel. The chip carries what a reader
//! needs to recognize the source (`label`), while `block` is the attributed
//! text the submission sends. Splitting the two keeps the chip row narrow
//! while the provider still receives the full quote with its attribution,
//! which is what makes a reference usable without re-opening the source.

use std::ops::Range;

use gpui::SharedString;

/// What a staged reference points at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReferenceKind {
    /// A selection from the code editor.
    Selection,
    /// A transcript message, quoted whole or by its selected text.
    Message,
    /// A file named for the agent to open itself.
    File,
}

impl ReferenceKind {
    /// The chip's leading icon. Chosen for the source, not the label, so a
    /// truncated chip still reads as code, a message or a file.
    pub(super) fn icon(self) -> &'static str {
        match self {
            Self::Selection => "icons/file-bottom-left-arrow.svg",
            Self::Message => "icons/message-square-text.svg",
            Self::File => "icons/file.svg",
        }
    }

    /// The persistence tag. Drafts outlive the process, so the tag is part of
    /// the stored format; an unknown tag degrades to a file reference.
    fn tag(self) -> &'static str {
        match self {
            Self::Selection => "selection",
            Self::Message => "message",
            Self::File => "file",
        }
    }

    fn from_tag(tag: &str) -> Self {
        match tag {
            "selection" => Self::Selection,
            "message" => Self::Message,
            _ => Self::File,
        }
    }
}

/// One staged reference. `label` is what the chip shows; `block` is the
/// attributed text the submission sends ahead of the typed prompt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ComposerReference {
    pub(super) kind: ReferenceKind,
    pub(super) label: SharedString,
    pub(super) block: String,
}

/// The 1-based line span a byte range covers, clamped to the text so a stale
/// range from an edited file cannot panic or report a line past the end.
///
/// A range ending exactly at the text's end reports the last line, not the
/// blank line its trailing newline would open: a selection covering all of
/// "a\nb\n" is lines 1-2, the same lines the gutter numbers.
pub(super) fn line_span(content: &str, range: &Range<usize>) -> (usize, usize) {
    let end = range.end.min(content.len());
    let start = range.start.min(end);
    let end = if end == content.len() {
        content.trim_end_matches('\n').len().max(start)
    } else {
        end
    };
    (
        content[..start].matches('\n').count() + 1,
        content[..end].matches('\n').count() + 1,
    )
}

/// A quote of editor-selected code, attributed with its file and line span.
pub(super) fn selection_reference(
    path: &str,
    language: &str,
    lines: (usize, usize),
    text: &str,
) -> ComposerReference {
    let (start, end) = lines;
    let span = if start == end {
        format!("第 {start} 行")
    } else {
        format!("第 {start}–{end} 行")
    };
    ComposerReference {
        kind: ReferenceKind::Selection,
        label: SharedString::from(format!("引用代码 · {path} {span}")),
        block: format!("引用代码 {path}（{span}）：\n```{language}\n{text}\n```\n"),
    }
}

/// A quote of a transcript message — its whole visible text or the part the
/// reader selected — attributed with the speaker's role.
pub(super) fn message_reference(role: &str, text: &str) -> ComposerReference {
    let preview = first_line(text);
    ComposerReference {
        kind: ReferenceKind::Message,
        label: SharedString::from(format!("引用{role}消息 · {preview}")),
        block: message_block(role, text),
    }
}

/// A named file reference. The block states the path only; the agent opens
/// the file itself, which keeps a large file's bytes out of the prompt.
pub(super) fn file_reference(path: &str) -> ComposerReference {
    ComposerReference {
        kind: ReferenceKind::File,
        label: SharedString::from(format!("引用文件 · {path}")),
        block: format!("引用文件 {path}\n"),
    }
}

/// The reference text a submission sends ahead of the typed prompt. Empty
/// when nothing is staged, so an ordinary prompt is unchanged.
pub(super) fn references_block(references: &[ComposerReference]) -> String {
    references
        .iter()
        .map(|reference| reference.block.trim_end())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The block for a quoted message: the role attribution, then the text as a
/// block quote so a multi-line message keeps one spoken turn.
fn message_block(role: &str, text: &str) -> String {
    let mut block = format!("引用{role}消息：\n");
    for line in text.lines() {
        block.push_str("> ");
        block.push_str(line);
        block.push('\n');
    }
    block
}

/// The first line of a quote, for a chip label that previews the source
/// without spending the row's width on its whole text.
fn first_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
}

impl From<&ComposerReference> for crate::persistence::ComposerDraftReference {
    fn from(reference: &ComposerReference) -> Self {
        Self {
            label: reference.label.to_string(),
            block: reference.block.clone(),
            kind: reference.kind.tag().to_owned(),
        }
    }
}

impl From<&crate::persistence::ComposerDraftReference> for ComposerReference {
    fn from(reference: &crate::persistence::ComposerDraftReference) -> Self {
        Self {
            kind: ReferenceKind::from_tag(&reference.kind),
            label: SharedString::from(reference.label.clone()),
            block: reference.block.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_span_counts_lines_one_based_and_clamps() {
        let content = "one\ntwo\nthree\n";
        assert_eq!(line_span(content, &(0..3)), (1, 1));
        assert_eq!(line_span(content, &(4..7)), (2, 2));
        assert_eq!(line_span(content, &(0..14)), (1, 3));
        // A range past the end of an edited file reports the last line rather
        // than panicking or counting a line that no longer exists.
        assert_eq!(line_span(content, &(10..999)), (3, 3));
    }

    #[test]
    fn selection_reference_attributes_file_language_and_lines() {
        let single = selection_reference("src/build.rs", "rust", (8, 8), "fn main() {}");
        assert_eq!(single.label.as_ref(), "引用代码 · src/build.rs 第 8 行");
        assert_eq!(
            single.block,
            "引用代码 src/build.rs（第 8 行）：\n```rust\nfn main() {}\n```\n"
        );

        let span = selection_reference("app.py", "python", (10, 13), "a = 1\nb = 2");
        assert_eq!(span.label.as_ref(), "引用代码 · app.py 第 10–13 行");
        assert_eq!(
            span.block,
            "引用代码 app.py（第 10–13 行）：\n```python\na = 1\nb = 2\n```\n"
        );
    }

    #[test]
    fn message_reference_quotes_every_line_with_role_attribution() {
        let reference = message_reference("用户", "你好\n请引用这段");
        assert_eq!(reference.label.as_ref(), "引用用户消息 · 你好");
        assert_eq!(reference.block, "引用用户消息：\n> 你好\n> 请引用这段\n");
    }

    #[test]
    fn message_reference_label_skips_blank_leading_lines() {
        let reference = message_reference("助手", "\n\n  正文");
        assert_eq!(reference.label.as_ref(), "引用助手消息 ·   正文");
    }

    #[test]
    fn file_reference_names_the_path_only() {
        let reference = file_reference("src/main.rs");
        assert_eq!(reference.label.as_ref(), "引用文件 · src/main.rs");
        assert_eq!(reference.block, "引用文件 src/main.rs\n");
    }

    #[test]
    fn references_block_joins_with_blank_lines_and_empty_stays_empty() {
        assert_eq!(references_block(&[]), "");
        let references = vec![file_reference("a.rs"), message_reference("用户", "text")];
        assert_eq!(
            references_block(&references),
            "引用文件 a.rs\n\n引用用户消息：\n> text"
        );
    }

    #[test]
    fn draft_round_trip_preserves_kind_label_and_block() {
        let reference = selection_reference("src/build.rs", "rust", (2, 4), "let x = 1;");
        let draft = crate::persistence::ComposerDraftReference::from(&reference);
        assert_eq!(draft.kind, "selection");
        let restored = ComposerReference::from(&draft);
        assert_eq!(restored, reference);

        let unknown = crate::persistence::ComposerDraftReference {
            label: "旧引用".to_owned(),
            block: "引用文件 old.rs\n".to_owned(),
            kind: String::new(),
        };
        let restored = ComposerReference::from(&unknown);
        assert_eq!(restored.kind, ReferenceKind::File);
        assert_eq!(restored.label.as_ref(), "旧引用");
    }
}

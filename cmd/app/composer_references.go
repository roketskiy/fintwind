package main

import (
	"strconv"
	"strings"

	"github.com/egoist/mygo/ui"
)

// composer_references.go stages text quotes above the composer, mirroring
// src/app/quote.rs: a code selection, a transcript message — source-attributed
// text folded into the next submission's prompt. Files keep traveling the
// attachment path, so only the text kinds stage here.

// composerReferenceKind names what a staged reference points at.
type composerReferenceKind int

const (
	referenceSelection composerReferenceKind = iota
	referenceMessage
)

// composerReference is one staged quote: label for the chip, block for the
// prompt. Like the source's ComposerReference it never uploads bytes: the
// block goes ahead of the typed prompt, nothing travels on the prompt
// `files` channel.
type composerReference struct {
	kind  composerReferenceKind
	label string
	block string
}

// stageComposerReference stages one quote above the composer, deduped by
// block like the source's stage_composer_reference, and returns focus to
// the composer for the next keystroke.
func (a *app) stageComposerReference(ref composerReference) {
	for _, existing := range a.pendingReferences {
		if existing.block == ref.block {
			return
		}
	}
	a.pendingReferences = append(a.pendingReferences, ref)
	if a.detail != nil {
		a.stateForSession(a.detail).references = a.pendingReferences
	}
	a.focusComposerOnce = true
}

// removePendingReference drops one staged quote by render-time index.
func (a *app) removePendingReference(index int) {
	if index < 0 || index >= len(a.pendingReferences) {
		return
	}
	a.pendingReferences = append(a.pendingReferences[:index], a.pendingReferences[index+1:]...)
	if a.detail != nil {
		a.stateForSession(a.detail).references = a.pendingReferences
	}
}

// referencesBlock joins staged quotes the way the source's references_block
// does: trimmed blocks separated by blank lines, empty when none staged, so
// an ordinary prompt is unchanged.
func referencesBlock(references []composerReference) string {
	blocks := make([]string, 0, len(references))
	for _, ref := range references {
		if block := strings.TrimRight(ref.block, "\n"); block != "" {
			blocks = append(blocks, block)
		}
	}
	return strings.Join(blocks, "\n\n")
}

// promptWithReferences folds staged quotes ahead of the typed prompt.
func (a *app) promptWithReferences(text string) string {
	if block := referencesBlock(a.pendingReferences); block != "" {
		if strings.TrimSpace(text) == "" {
			return block
		}
		return block + "\n\n" + text
	}
	return text
}

// referenceLineSpan is the 1-based line span a byte range covers, mirroring
// quote::line_span: clamped to the text, a range ending at the end reports
// the last line rather than the blank line its trailing newline opens.
func referenceLineSpan(content string, start, end int) (int, int) {
	if end > len(content) {
		end = len(content)
	}
	if start > end {
		start = end
	}
	if start < 0 {
		start = 0
	}
	if end == len(content) {
		end = max(len(strings.TrimRight(content, "\n")), start)
	}
	return strings.Count(content[:start], "\n") + 1, strings.Count(content[:end], "\n") + 1
}

// selectionReference quotes editor-selected code with its file and line
// span, mirroring quote::selection_reference.
func selectionReference(path, language string, start, end int, text string) composerReference {
	span := "第 " + strconv.Itoa(start) + " 行"
	if start != end {
		span = "第 " + strconv.Itoa(start) + "–" + strconv.Itoa(end) + " 行"
	}
	return composerReference{
		kind:  referenceSelection,
		label: path + " " + span,
		block: "引用代码 " + path + "（" + span + "）：\n```" + language + "\n" + text + "\n```\n",
	}
}

// messageReference quotes a transcript message — its whole visible text or
// the part the reader selected — attributed with the speaker's role,
// mirroring quote::message_reference.
func messageReference(role, text string) composerReference {
	var block strings.Builder
	block.WriteString("引用" + role + "消息：\n")
	for _, line := range strings.Split(text, "\n") {
		block.WriteString("> " + line + "\n")
	}
	return composerReference{kind: referenceMessage, label: referencePreview(text), block: block.String()}
}

// referencePreview labels a message quote with its opening words, mirroring
// the source's message_preview: 24 chars plus an ellipsis, blank leading
// lines skipped.
func referencePreview(text string) string {
	for _, line := range strings.Split(text, "\n") {
		if trimmed := strings.TrimSpace(line); trimmed != "" {
			if runes := []rune(trimmed); len(runes) > 24 {
				return string(runes[:24]) + "…"
			} else {
				return trimmed
			}
		}
	}
	return ""
}

func (a *app) pendingReferenceChips(c *ui.Context) {
	removeIndex := -1
	for index := range a.pendingReferences {
		ref := a.pendingReferences[index]
		index := index
		chip := ui.Row(c).Height(28).Padding(0, 4, 0, 8).Radius(7).Gap(6).
			AlignItems(ui.Center).Border(1, palette.border).Tooltip(ref.label)
		if chip.Hovered() {
			chip.Background(palette.overlay)
		}
		chip.Children(func() {
			quoteIcon := "file-bottom-left-arrow"
			if ref.kind == referenceMessage {
				quoteIcon = "message-square-text"
			}
			icon(c, quoteIcon, 12, palette.textSecondary)
			ui.Row(c).MinWidth(0).MaxWidth(180).Children(func() {
				ui.Text(c, ref.label).FontSize(uiPx(12)).FixedLineHeight(uiPx(16)).
					TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
			})
			remove := ui.ButtonBase(c).Label(tr("Remove reference")).
				Size(20, 20).Radius(5).Center()
			if remove.Hovered() {
				remove.Background(palette.overlayStrong)
			}
			remove.Children(func() {
				icon(c, "x", 11, palette.textTertiary)
			})
			if remove.Clicked() {
				removeIndex = index
			}
		})
	}
	if removeIndex >= 0 {
		a.removePendingReference(removeIndex)
	}
}

package main

import (
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/editor"
)

// The diff reader turns daemon-captured Git patches into render-ready
// viewers, the client side of Rust's review_diff.rs. Whole-source snapshots
// arrive with unlimited context, so the reader rebuilds two whole texts and
// the viewer collapses context into separators that expand in memory; live
// sources arrive clipped to the requested context and growing it re-requests.

type reviewDiffData struct {
	Source          json.RawMessage `json:"source"`
	Numstat         string          `json:"numstat"`
	Patch           string          `json:"patch"`
	CompleteContext bool            `json:"completeContext"`
	Truncated       bool            `json:"truncated"`
	File            *struct {
		Path      string `json:"path"`
		Additions uint64 `json:"additions"`
		Deletions uint64 `json:"deletions"`
		Binary    bool   `json:"binary"`
	} `json:"file"`
}

type reviewDiffResponse struct {
	Type string         `json:"type"`
	Data reviewDiffData `json:"data"`
}

// reviewReader is one file's viewer state, kept across frames.
type reviewReader struct {
	Generation  uint64
	Loading     bool
	Error, Note string
	Viewer      *editor.DiffState
	// Full marks the whole-text mode: the viewer compares rebuilt files and
	// its separators expand collapsed context in memory, so "more context"
	// has nothing to request.
	Full                 bool
	Context              int
	Split, CanExpand     bool
	Additions, Deletions uint64
	Hunks, Hunk          int
	Key                  string
	Hash                 [32]byte
}

func reviewPatchPath(p editor.FilePatch) string {
	if p.NewName != "" {
		return p.NewName
	}
	return p.OldName
}

func reviewPatchStats(p editor.FilePatch) (add, del uint64) {
	for _, h := range p.Hunks {
		for _, line := range h.Lines {
			if len(line) > 0 {
				switch line[0] {
				case '+':
					add++
				case '-':
					del++
				}
			}
		}
	}
	return
}

// reviewBuildReader builds the reader of one file diff the daemon clipped
// itself: live sources always come back with completeContext false, and the
// requested context already applied.
func reviewBuildReader(data reviewDiffData, path string, contextLines int, split bool) (reviewReader, error) {
	patches, err := editor.ParsePatch(data.Patch)
	if err != nil {
		return reviewReader{}, fmt.Errorf("%s: %w", tr("Could not parse diff"), err)
	}
	for _, patch := range patches {
		if reviewPatchPath(patch) == path {
			reader := reviewReaderForPatch(patch, data.CompleteContext, data.Truncated, contextLines, split)
			reader.Hash = sha256.Sum256([]byte(data.Patch))
			if data.File != nil {
				reader.Additions, reader.Deletions = data.File.Additions, data.File.Deletions
			}
			return reader, nil
		}
	}
	if data.File != nil && data.File.Path == path {
		note := tr("No text changes (empty file or metadata-only change)")
		if data.File.Binary {
			note = tr("Binary file changed. Text diff unavailable.")
		}
		return reviewReader{Context: contextLines, Split: split, Note: note, Additions: data.File.Additions, Deletions: data.File.Deletions}, nil
	}
	return reviewReader{}, errors.New(tr("The file is not present in this snapshot"))
}

// reviewReaderForPatch builds the viewer of one patch. A complete patch
// rebuilds two whole texts so context collapses into expandable separators;
// anything else shows the patch itself, as the daemon clipped it.
func reviewReaderForPatch(patch editor.FilePatch, complete, truncated bool, contextLines int, split bool) reviewReader {
	reader := reviewReader{Context: contextLines, Split: split, Hunk: -1}
	reader.Additions, reader.Deletions = reviewPatchStats(patch)
	if patch.Binary {
		reader.Note = tr("Binary file changed. Text diff unavailable.")
		return reader
	}
	if len(patch.Hunks) == 0 {
		reader.Note = tr("Metadata-only change")
		if patch.OldName != patch.NewName {
			reader.Note = fmt.Sprintf("%s → %s", patch.OldName, patch.NewName)
		}
		if patch.OldMode != patch.NewMode {
			reader.Note += fmt.Sprintf(" · mode %s → %s", patch.OldMode, patch.NewMode)
		}
		return reader
	}
	// DiffSplit is the editor's zero layout, so unified must be set
	// explicitly or every reader starts side by side.
	viewer := &editor.DiffState{Layout: editor.DiffUnified, Indicators: editor.DiffClassic, Editable: false}
	if split {
		viewer.Layout = editor.DiffSplit
	}
	if complete {
		if oldText, newText, ok := patchFullTexts(patch); ok {
			viewer.Old = editor.NewBuffer(oldText)
			viewer.New = editor.NewBuffer(newText)
			viewer.ContextLines = 3
			viewer.ExpansionLines = 100
			reader.Full = true
		}
	}
	if !reader.Full {
		viewer.SetPatch(patch)
		if truncated {
			reader.Note = tr("Snapshot context is limited by the daemon. Missing lines cannot be expanded.")
		}
	}
	reader.Viewer = viewer
	reader.Hunks = len(viewer.Hunks())
	return reader
}

// patchFullTexts rebuilds the two whole texts a patch was made from, so the
// viewer can collapse context and expand it in memory. Whole-source
// snapshots come back with unlimited context, so their hunks tile the file;
// hunks that do not line up mean partial context, and ok reports that.
func patchFullTexts(p editor.FilePatch) (oldText, newText string, ok bool) {
	if p.Binary || len(p.Hunks) == 0 {
		return "", "", false
	}
	var oldLines, newLines []string
	oldNoNewline, newNoNewline := false, false
	oldPos, newPos := 1, 1
	// Where the previous patch line landed, so a "\ No newline" marker
	// follows the side or sides it describes.
	lastOld, lastNew := -1, -1
	appendLine := func(line string) {
		marker, content := line[0], line[1:]
		switch marker {
		case ' ':
			oldLines = append(oldLines, content)
			newLines = append(newLines, content)
			lastOld, lastNew = len(oldLines)-1, len(newLines)-1
			oldPos++
			newPos++
		case '-':
			oldLines = append(oldLines, content)
			lastOld, lastNew = len(oldLines)-1, -1
			oldPos++
		case '+':
			newLines = append(newLines, content)
			lastOld, lastNew = -1, len(newLines)-1
			newPos++
		case '\\':
			if lastOld >= 0 {
				oldNoNewline = true
			}
			if lastNew >= 0 {
				newNoNewline = true
			}
		default:
			lastOld, lastNew = -1, -1
		}
	}
	for _, h := range p.Hunks {
		// A zero-count range reports the line before the hunk, so a pure
		// insertion at the start of a file declares 0.
		if h.OldLines > 0 && h.OldStart != oldPos {
			return "", "", false
		}
		if h.OldLines == 0 && h.OldStart != oldPos-1 {
			return "", "", false
		}
		if h.NewLines > 0 && h.NewStart != newPos {
			return "", "", false
		}
		if h.NewLines == 0 && h.NewStart != newPos-1 {
			return "", "", false
		}
		oldCount, newCount := 0, 0
		for _, line := range h.Lines {
			if len(line) == 0 {
				continue
			}
			if line[0] == '\\' {
				appendLine(line)
				continue
			}
			appendLine(line)
			switch line[0] {
			case ' ':
				oldCount++
				newCount++
			case '-':
				oldCount++
			case '+':
				newCount++
			}
		}
		if oldCount != h.OldLines || newCount != h.NewLines {
			return "", "", false
		}
	}
	join := func(lines []string, noNewline bool) string {
		text := ""
		for i, line := range lines {
			if i > 0 {
				text += "\n"
			}
			text += line
		}
		if len(lines) > 0 && !noNewline {
			text += "\n"
		}
		return text
	}
	return join(oldLines, oldNoNewline), join(newLines, newNoNewline), true
}

// reviewSeparator styles the diff viewer's separators: collapsed context
// expands toward the nearest change, Shift expands everything at once, and
// a patch hunk header renders as its own row. onHeaderClick runs when a
// hunk header is clicked, which live sources use to request more context.
func reviewSeparator(onHeaderClick func()) func(*ui.Context, ui.Element, *editor.DiffSeparator) {
	return func(c *ui.Context, sep ui.Element, s *editor.DiffSeparator) {
		t := c.Theme()
		ct := s.Theme()
		sep.Children(func() {
			bar := ui.Row(c).Grow(1).AlignSelf(ui.Stretch).AlignItems(ui.Center).Gap(4).Padding(0, 10).Radius(6).
				Background(ct.Background.Mix(ct.Text, 0.06))
			bar.Children(func() {
				if s.Lines == 0 {
					if onHeaderClick != nil {
						header := ui.ButtonBase(c).Label(tr("Show more context")).Tooltip(tr("Expand unmodified lines · Shift-click for all")).
							FillWidth().AlignItems(ui.Center).Radius(6)
						if header.Hovered() {
							header.Background(ct.Text.Alpha(0.06))
						}
						header.Children(func() {
							ui.Text(c, s.Header).Font(s.Font().Family).FontSize(s.Font().Size - 1).
								TextColor(t.TextMuted).SingleLine().Ellipsis("…")
						})
						header.OnClick(onHeaderClick)
						return
					}
					ui.Text(c, s.Header).Font(s.Font().Family).FontSize(s.Font().Size - 1).
						TextColor(t.TextMuted).SingleLine().Ellipsis("…")
					return
				}
				expand := func(fn func()) {
					if c.Modifiers()&ui.Shift != 0 {
						fn = s.ExpandAll
					}
					fn()
				}
				chevron := func(down bool, label string, fn func()) {
					b := ui.ButtonBase(c).Key(label).Size(20, 20).Radius(4).Label(label).Tooltip(label)
					if b.Hovered() {
						b.Background(ct.Text.Alpha(0.08))
					}
					b.Children(func() {
						name := "chevron-up"
						if down {
							name = "chevron-down"
						}
						icon(c, name, 11, t.TextMuted)
					})
					if b.Clicked() {
						expand(fn)
					}
				}
				below := tr("Show 100 lines below · Shift-click for all")
				above := tr("Show 100 lines above · Shift-click for all")
				all := tr("Expand all unmodified lines")
				switch {
				case s.First:
					chevron(true, below, s.ExpandUp)
				case s.Last:
					chevron(false, above, s.ExpandDown)
				case s.Lines <= s.Step:
					chevron(false, all, s.ExpandAll)
				default:
					chevron(true, below, s.ExpandDown)
					chevron(false, above, s.ExpandUp)
				}
				label := trf("%d unmodified lines", s.Lines)
				b := ui.ButtonBase(c).Key("all").Padding(2, 6).Radius(4).Label(label).Tooltip(all)
				if b.Hovered() {
					b.Background(ct.Text.Alpha(0.06))
				}
				b.Children(func() {
					ui.Text(c, label).FontSize(11).TextColor(t.TextMuted)
				})
				if b.Clicked() {
					expand(s.ExpandAll)
				}
			})
		})
	}
}

// editorThemes keeps one editor theme per appearance scheme and mode, the
// same cache discipline as the lobe tokens: applyAppearance rebuilds the
// palette deterministically per pair, so a cached theme stays valid, and a
// new pair makes a new entry. A stable pointer keeps the editor from
// restyling its text every frame.
var editorThemes = map[lobeConfigKey]*editor.Theme{}

// reviewEditorTheme colors the diff viewer from the palette, the way the
// Rust reader colors tokens and change washes. Fields left zero resolve to
// the editor's defaults for the window's appearance.
func reviewEditorTheme() *editor.Theme {
	key := lobeConfigKey{activeAppearance.scheme, activeAppearance.dark}
	if t, ok := editorThemes[key]; ok {
		return t
	}
	t := &editor.Theme{
		Background: palette.surface,
		Text:       palette.text,
		LineNumber: palette.textGhost,
		Selection:  palette.selection,
		Added:      palette.success,
		Deleted:    palette.danger,
		Modified:   palette.accent,
	}
	tok := func(kind editor.TokenKind, color ui.Color) { t.Tokens[kind].Color = color }
	tok(editor.TokenKeyword, palette.codeKeyword)
	tok(editor.TokenOperator, palette.codeKeyword)
	tok(editor.TokenString, palette.codeString)
	tok(editor.TokenEscape, palette.codeString)
	tok(editor.TokenNumber, palette.codeNumber)
	tok(editor.TokenConstant, palette.codeLiteral)
	tok(editor.TokenProperty, palette.codeLiteral)
	tok(editor.TokenType, palette.codeType)
	tok(editor.TokenTag, palette.codeType)
	tok(editor.TokenFunction, palette.codeFunction)
	tok(editor.TokenMeta, palette.codeFunction)
	tok(editor.TokenHeading, palette.codeFunction)
	tok(editor.TokenComment, palette.textMuted)
	editorThemes[key] = t
	return t
}

// reviewApplyMetrics gives the viewer the app's code font and theme. It runs
// in the render path, where the palette is safe to read.
func reviewApplyMetrics(v *editor.DiffState) {
	v.Font = ui.Font{Family: monoFont(), Size: codeMetric(12)}
	v.LineHeight = 20.0 / 12.0
	v.Theme = reviewEditorTheme()
}

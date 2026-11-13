package main

import (
	"fmt"
	"strings"

	"fintwind-go/assets"
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/editor"
	"github.com/lobehub/lobe-mygo/pkg/action"
	"github.com/lobehub/lobe-mygo/pkg/display"
	"github.com/lobehub/lobe-mygo/pkg/overlay"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

// The review surface shows a whole-source snapshot: one collectReviewDiff
// per load, files listed in the secondary column, the patch read in the
// main area. The turn checkpoint adds the history banner; the committed and
// branch ranges read the same surface without it.

func (a *app) reviewView(c *ui.Context) {
	ui.Row(c).FillWidth().Grow(1).MinWidth(0).FillHeight().Children(func() {
		if a.sidebarOpen {
			secondary := ui.Column(c).Width(a.scSecWidth).FillHeight().Shrink(0).
				Background(palette.sidebar).Radius(workspaceRadius).Clip()
			secondary = sidebarShadow(secondary)
			secondary.Children(func() {
				a.reviewSecondary(c)
				a.secondaryResizeHandle(c, &a.scSecWidth)
			})
		}
		a.reviewMain(c)
	})
}

func (a *app) reviewMain(c *ui.Context) {
	st := &a.review
	main := ui.Column(c).Grow(1).MinWidth(0).FillHeight().Background(palette.surface).
		BorderWidth(0, 0, 0, 1).BorderColor(palette.sidebarBorder).Radius(workspaceRadius, 0, 0, workspaceRadius).Clip()
	main = workspaceShadow(main)
	main.Children(func() {
		if st.Kind == reviewKindTurn {
			a.reviewBanner(c)
		}
		a.reviewToolbar(c)
		if !a.scCurrent(st.Scope) {
			reviewEmpty(c, tr("Review source changed"), false)
			return
		}
		if st.Loading {
			reviewEmpty(c, tr("Loading checkpoint…"), false)
			return
		}
		if st.Error != "" {
			reviewEmpty(c, st.Error, true)
			return
		}
		if a.sourceControl.Discard != nil {
			a.scDiscardDialog(c)
		}
		a.reviewReaderView(c, st.Selected, &st.Reader, true)
	})
}

// reviewSecondary lists the snapshot's files. The whole patch is already in
// memory, so picking a file never asks Git.
func (a *app) reviewSecondary(c *ui.Context) {
	st := &a.review
	title := st.Source.label()
	if st.Kind == reviewKindTurn {
		title = trf("Turn %d", st.TurnCount)
	}
	ui.Column(c).FillWidth().Padding(12, 12, 10, 12).Gap(8).Children(func() {
		ui.Text(c, title).FontSize(uiPx(12.5)).TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
		sidebarFilterSearch(c, "review-files", tr("Filter files"), &st.Filter)
		if st.Filter != st.AppliedFilter {
			st.rebuildRows()
		}
	})
	if !a.scCurrent(st.Scope) {
		reviewNote(c, tr("The selected session or connection changed. Reopen the turn to review it."), true)
		return
	}
	if st.Loading {
		reviewNote(c, tr("Loading checkpoint…"), false)
		return
	}
	if st.Error != "" {
		reviewNote(c, st.Error, true)
		return
	}
	if len(st.Rows) == 0 {
		reviewNote(c, tr("No changes"), false)
		return
	}
	// One group header for the whole file list, like the source control
	// group rows: count plus a collapse chevron for the entire group.
	c.Key("review-files-group")
	group := ui.ButtonBase(c).Label(trf("%d files", len(st.Rows))).FillWidth().MinHeight(28).
		Padding(5, 12, 5, 8).Gap(6).AlignItems(ui.Center)
	if group.Hovered() {
		group.Background(palette.overlay)
	}
	if group.Pressed() {
		group.Background(palette.overlayStrong)
	}
	group.Children(func() {
		if st.FilesCollapsed {
			icon(c, "chevron-right", 10, palette.textGhost)
		} else {
			icon(c, "chevron-down", 10, palette.textGhost)
		}
		ui.Text(c, trf("%d files", len(st.Rows))).FontSize(uiPx(11)).TextColor(palette.textTertiary)
	})
	// OnClick, not Clicked: like the source control group fold, hiding rows
	// must wait until the frame ends or the list panics past the end.
	group.OnClick(func() {
		st.FilesCollapsed = !st.FilesCollapsed
	})
	if st.FilesCollapsed {
		return
	}
	writable := a.reviewFilesWritable()
	st.FileList.Selected = &st.FileIndex
	c.Key("review-checkpoint-files")
	ui.List(c, &st.FileList, len(st.Rows)).FillWidth().Grow(1).MinHeight(0).ItemKey(func(i int) any { return st.Rows[i].Path }).Rows(func(row ui.ListRow) {
		file := st.Rows[row.Index]
		c.Key(file.Path)
		entry := ui.ButtonBase(c).Label(scFileLabel(file)).FillWidth().MinHeight(34).Padding(5, 12).Gap(5).AlignItems(ui.Center)
		if st.Selected == file.Path {
			entry.Background(palette.overlayStrong)
		} else if entry.Hovered() {
			entry.Background(palette.overlay)
		}
		if entry.Pressed() {
			entry.Background(palette.overlayStrong)
		}
		entry.Children(func() {
			ui.Row(c).Width(10).Shrink(0).Children(func() {})
			brandIcon(c, fileTypeIcon(file.Path), 13)
			ui.Text(c, file.Path).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).Grow(1).MinWidth(0).SingleLine().Ellipsis("...").Tooltip(file.Path)
			ui.Text(c, file.Letter).FontSize(uiPx(10)).TextColor(scLetterColor(file.Letter))
			ui.Text(c, trf("+%d −%d", file.Additions, file.Deletions)).FontSize(uiPx(10)).TextColor(palette.textTertiary)
			if writable {
				a.reviewRowActions(c, file)
			}
		})
		entry.OnClick(func() { st.FileIndex = row.Index; a.reviewSelect(file.Path, 3) })
	}).OnChange(func() {
		if st.FileIndex >= 0 && st.FileIndex < len(st.Rows) {
			path := st.Rows[st.FileIndex].Path
			if st.Selected != path {
				a.reviewSelect(path, 3)
			}
		}
	})
}

// reviewFilesWritable reports whether the listed files are live and can be
// staged or discarded: snapshots of turns and ranges are read-only.
func (a *app) reviewFilesWritable() bool {
	st := &a.review
	if st.Kind != reviewKindSource {
		return false
	}
	switch st.Source {
	case reviewSourceUncommitted, reviewSourceUnstaged, reviewSourceStaged:
		return a.sourceControl.writable()
	}
	return false
}

// reviewRowActions stages or discards one listed file, like the header's
// actions do for the shown file.
func (a *app) reviewRowActions(c *ui.Context, file scFile) {
	f := file
	stageLabel := tr("Stage")
	if f.Group == "staged" {
		stageLabel = tr("Unstage")
	}
	stage := reviewMutedButton(c, stageLabel)
	stage.OnClick(func() { a.scStage(f, f.Group == "staged") })
	if f.Group != "staged" {
		discard := reviewMutedButton(c, tr("Discard…"))
		discard.OnClick(func() { a.sourceControl.Discard = &f })
	}
}

// reviewBanner marks a turn snapshot: where it came from, that it is
// read-only, and the ways out.
func (a *app) reviewBanner(c *ui.Context) {
	st := &a.review
	title := ""
	if session := a.byID[st.Scope.SessionID]; session != nil && session.Title != "" {
		title = session.Title
	}
	turnLabel := trf("Turn %d", st.TurnCount)
	snapshot := turnLabel
	if title != "" {
		snapshot = fmt.Sprintf("%s · %s", title, turnLabel)
	}
	ui.Row(c).FillWidth().Padding(8, 12).Gap(8).AlignItems(ui.Center).
		Background(palette.overlay).BorderWidth(0, 0, 1, 0).BorderColor(palette.border).Children(func() {
		icon(c, "history", 13, palette.textTertiary)
		ui.Row(c).Grow(1).MinWidth(0).Children(func() {
			ui.Text(c, snapshot).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
		})
		display.Tag(c, tr("Read-only snapshot"), display.TagProps{Variant: display.TagOutlined, Size: theme.Small})
		live := action.Button(c, tr("View workspace"), action.ButtonProps{Variant: action.Text, Size: theme.Small})
		live.OnClick(func() { a.reviewBackToLive() })
		back := action.Button(c, tr("Back to session"), action.ButtonProps{Variant: action.Text, Size: theme.Small})
		back.OnClick(func() { a.reviewBackToSession() })
	})
}

func (a *app) reviewToolbar(c *ui.Context) {
	st := &a.review
	total := len(st.Rows)
	current := st.FileIndex + 1
	if total == 0 {
		current = 0
	}
	ui.Row(c).FillWidth().Height(44).Padding(0, 12).Gap(8).AlignItems(ui.Center).
		BorderWidth(0, 0, 1, 0).BorderColor(palette.border).Children(func() {
		a.reviewSourcePill(c, true)
		if st.Truncated || st.Stats.Truncated {
			ui.Text(c, tr("Large diff · context incomplete")).FontSize(uiPx(10.5)).TextColor(palette.warningText)
		}
		ui.Row(c).Grow(1).Children(func() {})
		a.reviewFileNav(c, total, current, a.reviewStep)
		a.reviewRefreshButton(c, true)
	})
}

// reviewFileNav is the previous/next file pair and its position, shared by
// both toolbars.
func (a *app) reviewFileNav(c *ui.Context, total, current int, step func(int)) {
	if total <= 0 {
		return
	}
	prev := action.ActionIcon(c, assets.SVG("chevron-up"), action.ActionIconProps{Size: theme.Small, Title: tr("Previous file")})
	if current <= 1 {
		prev.Disabled(true).Opacity(0.4)
	} else {
		prev.OnClick(func() { step(-1) })
	}
	ui.Row(c).MinWidth(52).Center().Children(func() {
		ui.Text(c, trf("%d / %d", current, total)).FontSize(uiPx(11)).TextColor(palette.textTertiary)
	})
	next := action.ActionIcon(c, assets.SVG("chevron-down"), action.ActionIconProps{Size: theme.Small, Title: tr("Next file")})
	if current >= total {
		next.Disabled(true).Opacity(0.4)
	} else {
		next.OnClick(func() { step(1) })
	}
}

func (a *app) reviewRefreshButton(c *ui.Context, history bool) {
	// No loading look: the 5s background refresh would blink the icon, and
	// the data updating is feedback enough.
	c.Key("review-diff-refresh")
	btn := action.ActionIcon(c, assets.SVG("rotate-cw"), action.ActionIconProps{Size: theme.Small, Title: tr("Refresh")})
	btn.OnClick(func() {
		if history {
			a.reviewLoad()
		} else {
			a.scRefreshSurface()
		}
	})
}

// reviewSourcePill shows the active source with its area totals and opens
// the source menu, for either surface: the snapshot reads reviewState, the
// live area reads sourceControlState.
func (a *app) reviewSourcePill(c *ui.Context, history bool) {
	st := &a.review
	ls := &a.sourceControl
	open := &ls.SourceMenuOpen
	label, current := ls.Source.label(), string(ls.Source)
	stats, valid := ls.Stats, ls.Stats.Valid
	if history {
		open = &st.SourceMenuOpen
		stats, valid = st.Stats, st.Stats.Valid
		if st.Kind == reviewKindTurn {
			label, current = trf("Turn %d", st.TurnCount), "turn"
		} else {
			label, current = st.Source.label(), string(st.Source)
		}
	}
	items := a.reviewSourceItems(current)
	chosen := overlay.DropdownMenu(c, open, overlay.DropdownMenuProps{Items: items}, func(t ui.Element) {
		action.StyleButton(c, t, "", action.ButtonProps{Size: theme.Small})
		if valid {
			t.Tooltip(trf("%d files", stats.Files))
		}
		t.Children(func() {
			ui.Text(c, label).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).SingleLine().MaxWidth(160)
			icon(c, "chevron-down", 10, palette.textGhost)
			if valid {
				ui.Row(c).Width(1).Height(10).Background(palette.border).Children(func() {})
				ui.Text(c, trf("+%d", stats.Additions)).FontSize(uiPx(11.5)).TextColor(palette.successText)
				ui.Text(c, trf("−%d", stats.Deletions)).FontSize(uiPx(11.5)).TextColor(palette.dangerText)
				if stats.Truncated {
					ui.Text(c, "…").FontSize(uiPx(11.5)).TextColor(palette.textTertiary)
				}
			}
		})
	})
	if chosen != "" {
		*open = false
		a.pickReviewSource(chosen, history)
	}
}

// reviewSourceItems lists every review source: the latest turn, the live
// areas, and the whole-source ranges, with the active one checked.
func (a *app) reviewSourceItems(current string) []overlay.MenuItem {
	turnLabel := tr("Last turn")
	hasTurn := false
	if a.detail != nil && len(a.detail.Turns) > 0 {
		last := a.detail.Turns[len(a.detail.Turns)-1]
		turnLabel = trf("Turn %d", last.TurnCount)
		hasTurn = true
	}
	if a.review.Active && a.review.Kind == reviewKindTurn {
		hasTurn = true
	}
	check := func(key string) string {
		if key == current {
			return "✓"
		}
		return ""
	}
	items := []overlay.MenuItem{{Key: "turn", Label: turnLabel, Disabled: !hasTurn, Extra: check("turn")}, {Key: "sep", Divider: true}}
	for _, source := range []reviewSource{reviewSourceUncommitted, reviewSourceUnstaged, reviewSourceStaged, reviewSourceCommitted, reviewSourceBranch} {
		items = append(items, overlay.MenuItem{Key: string(source), Label: source.label(), Extra: check(string(source))})
	}
	return items
}

// pickReviewSource retargets the review surface from the menu. Leaving a
// snapshot for a live source exits it first.
func (a *app) pickReviewSource(key string, history bool) {
	switch key {
	case "turn":
		if a.detail != nil && len(a.detail.Turns) > 0 {
			last := a.detail.Turns[len(a.detail.Turns)-1]
			a.openTurnReview(last.ID)
		}
	case string(reviewSourceCommitted), string(reviewSourceBranch):
		a.openSourceReview(reviewSource(key))
	default:
		if history {
			a.reviewBackToLive()
		}
		a.scSetSource(reviewSource(key))
	}
}

// reviewFileHeader names the shown file with its change totals and, for the
// live area, its write actions.
func (a *app) reviewFileHeader(c *ui.Context, path string, reader *reviewReader, history bool) {
	folder, name := path, path
	if index := strings.LastIndex(path, "/"); index >= 0 {
		folder, name = path[:index]+"/", path[index+1:]
	} else {
		folder = ""
	}
	letter := ""
	add, del := reader.Additions, reader.Deletions
	if history {
		for _, file := range a.review.Files {
			if file.Path == path {
				letter = file.Letter
				break
			}
		}
	} else if file := a.sourceControl.selectedFile(); file != nil {
		letter = file.Letter
	}
	ui.Column(c).FillWidth().BorderWidth(0, 0, 1, 0).BorderColor(palette.border).Children(func() {
		ui.Row(c).FillWidth().Height(36).Padding(0, 12).Gap(8).AlignItems(ui.Center).Children(func() {
			brandIcon(c, fileTypeIcon(path), 14)
			if folder != "" {
				ui.Row(c).Shrink(0).Children(func() {
					ui.Text(c, folder).FontSize(uiPx(11)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
				})
			}
			ui.Row(c).MinWidth(0).Shrink(1).Children(func() {
				ui.Text(c, name).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).SingleLine().Ellipsis("...").Tooltip(path)
			})
			if letter != "" && letter != " " {
				ui.Text(c, letter).FontSize(uiPx(10.5)).TextColor(scLetterColor(letter))
			}
			reviewDiffStat(c, add, del)
			ui.Row(c).Grow(1).Children(func() {})
			if !history {
				a.reviewFileActions(c)
			}
			reviewLayoutToggle(c, &a.sourceControl.LayoutSel, reader)
		})
	})
}

// reviewMutedButton is a bordered Text button at the muted contrast the row
// actions have always had: tertiary text, a step brighter under the pointer.
func reviewMutedButton(c *ui.Context, label string) ui.Element {
	b := action.Button(c, label, action.ButtonProps{Variant: action.Text, Size: theme.Small})
	color := palette.textTertiary
	if b.Hovered() {
		color = palette.textSecondary
	}
	b.TextColor(color).Border(1, palette.border).Radius(6)
	return b
}

// reviewFileActions are the selected live file's write actions.
func (a *app) reviewFileActions(c *ui.Context) {
	st := &a.sourceControl
	file := st.selectedFile()
	if file == nil {
		return
	}
	f := *file
	stageLabel := tr("Stage")
	if f.Group == "staged" {
		stageLabel = tr("Unstage")
	}
	stage := reviewMutedButton(c, stageLabel)
	if !st.writable() {
		stage.Disabled(true).Opacity(0.4)
	} else {
		stage.OnClick(func() { a.scStage(f, f.Group == "staged") })
	}
	if f.Group != "staged" {
		discard := reviewMutedButton(c, tr("Discard…"))
		if !st.writable() || f.Origin != "" {
			discard.Disabled(true).Opacity(0.4)
		} else {
			discard.OnClick(func() { st.Discard = &f })
		}
	}
}

// reviewDiffStat draws "+N −N" beside five blocks scaled to the balance of
// the change.
func reviewDiffStat(c *ui.Context, add, del uint64) {
	ui.Text(c, trf("+%d", add)).FontSize(uiPx(11)).TextColor(palette.successText)
	ui.Text(c, trf("−%d", del)).FontSize(uiPx(11)).TextColor(palette.dangerText)
	const blocks = 5
	green := 0
	if total := add + del; total > 0 {
		green = int(add * blocks / total)
	}
	for i := 0; i < blocks; i++ {
		color := palette.dangerSoft
		if add+del > 0 {
			if i < green {
				color = palette.success
			} else {
				color = palette.danger
			}
		}
		ui.Row(c).Width(8).Height(4).Radius(1).Background(color).Children(func() {})
	}
}

// reviewLayoutToggle is the unified/side-by-side switch. layoutSel is the
// surface's bound model; the reader follows it the frame after a pick.
func reviewLayoutToggle(c *ui.Context, layoutSel *int, reader *reviewReader) {
	options := []string{tr("Unified"), tr("Split")}
	themeSegmented(c, layoutSel, options)
	if (*layoutSel == 1) != reader.Split {
		reader.Split = *layoutSel == 1
		if reader.Viewer != nil {
			if reader.Split {
				reader.Viewer.Layout = editor.DiffSplit
			} else {
				reader.Viewer.Layout = editor.DiffUnified
			}
		}
	}
}

// reviewReaderView renders one file's diff, shared by the snapshot surface
// and the live area: file header, reader actions, then the viewer.
func (a *app) reviewReaderView(c *ui.Context, path string, reader *reviewReader, history bool) {
	a.reviewFileHeader(c, path, reader, history)
	if reader.Loading && reader.Viewer == nil {
		reviewEmpty(c, tr("Loading diff…"), false)
		return
	}
	if reader.Error != "" {
		reviewEmpty(c, reader.Error, true)
		return
	}
	if reader.Note != "" {
		reviewNote(c, reader.Note, false)
	}
	if reader.Viewer == nil {
		if reader.Note == "" {
			reviewEmpty(c, tr("Select a file to review"), false)
		}
		return
	}
	var onHeaderClick func()
	if !history {
		onHeaderClick = a.scGrowContext
	}
	reader.Viewer.RenderSeparator = reviewSeparator(onHeaderClick)
	reviewApplyMetrics(reader.Viewer)
	key := "review-native-diff-" + a.sourceControl.Root + "-" + a.sourceControl.Selected
	if history {
		key = "review-native-checkpoint-" + a.review.Scope.SessionID + "-" + a.review.TurnID + "-" + path
	}
	c.Key(key)
	editor.DiffView(c, reader.Viewer).FillWidth().Grow(1).MinHeight(0)
}

func reviewNote(c *ui.Context, text string, failure bool) {
	color := palette.textTertiary
	if failure {
		color = palette.dangerText
	}
	ui.Text(c, text).FontSize(uiPx(11)).TextColor(color).Wrap()
}

// reviewEmpty fills the reader with why there is nothing to show.
func reviewEmpty(c *ui.Context, text string, failure bool) {
	iconName := "changes"
	if failure {
		iconName = "alert"
	}
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Padding(24).AlignItems(ui.Center).Children(func() {
		display.Empty(c, display.EmptyProps{Icon: assets.SVG(iconName), Title: text})
	})
}

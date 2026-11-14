package main

import (
	"context"
	"fmt"
	"time"

	"fintwind-go/internal/graph"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/transfer"
	"github.com/egoist/mygo/ui"
)

// The right panel's History surface: the branch graph the capsule's changes
// tab opens, mirroring render_right_panel_history. Lane geometry follows
// VSCode's renderSCMHistoryItemGraph (the MyGit logview paints the same
// swimlanes), with one 60px row per commit and a selection detail below.

const (
	historyRowHeight    = 60.0
	historyDetailsH     = 140.0
	historyLaneWidth    = 14
	historyMaxLanes     = 24 // wider graphs are cut, so the subjects keep their room
	historyGraphStroke  = 1.6
	historyStatsTimeout = 15 * time.Second
	// historyLoadBuffer is how close to the loaded tail the virtualized
	// builder must reach before the next page is requested.
	historyLoadBuffer = 40
)

// capsuleCommitStats is one settled commit's numstat summary.
type capsuleCommitStats struct {
	Files     uint64 `json:"files"`
	Additions uint64 `json:"additions"`
	Deletions uint64 `json:"deletions"`
}

// openHistorySurface opens the History surface in the right panel, the way
// the capsule's History action row and the surfaces menu both do. The list
// starts with nothing chosen, like the desktop's.
func (a *app) openHistorySurface() {
	a.rightOpen = true
	a.capsule.HistorySel = -1
	a.openSurface(rightPanelHistory)
	a.historyEnsure(true)
}

func (a *app) historySurface(c *ui.Context) {
	a.historyEnsure(false)
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		a.historyToolbar(c)
		switch {
		case a.capsule.HistoryErr != "":
			a.historyEmpty(c, tr("Could not read history"), a.capsule.HistoryErr)
		case a.capsule.HistoryLoading && len(a.capsule.HistoryRows) == 0:
			a.historyEmpty(c, tr("Loading history"), tr("Reading the repository's commits"))
		case len(a.capsule.HistoryRows) == 0:
			a.historyEmpty(c, tr("No commits"), tr("This repository has no commits yet"))
		default:
			a.historyList(c)
		}
	})
}

func (a *app) historyEmpty(c *ui.Context, title, description string) {
	ui.Column(c).FillWidth().Grow(1).Padding(24, 12).Gap(4).
		AlignItems(ui.Center).Children(func() {
		ui.Text(c, title).FontSize(uiPx(12.5)).FontWeight(500).TextColor(palette.text)
		ui.Text(c, description).FontSize(uiPx(12)).TextColor(palette.textTertiary)
	})
}

// historyToolbar is the branch filter chip over the refresh button.
func (a *app) historyToolbar(c *ui.Context) {
	ui.Row(c).FillWidth().Shrink(0).Height(44).Padding(0, 10).Gap(8).AlignItems(ui.Center).Children(func() {
		a.historyBranchFilter(c)
		ui.Row(c).Grow(1)
		refresh := ui.ButtonBase(c).Label(tr("Refresh")).Tooltip(tr("Refresh")).
			Size(28, 28).Radius(7).Center()
		if refresh.Hovered() {
			refresh.Background(palette.overlay)
		}
		refresh.Children(func() {
			icon(c, "rotate-cw", 14, palette.textSecondary)
		})
		if refresh.Clicked() {
			a.historyRefresh()
		}
	})
}

// historyBranchFilter is the branch dropdown: every branch reads its own
// history without a checkout, like the desktop's filter.
func (a *app) historyBranchFilter(c *ui.Context) {
	label := a.capsule.HistoryBranch
	if label == "" {
		label = tr("All refs")
	}
	chip := ui.ButtonBase(c).Label(label).Tooltip(label).Height(28).Padding(0, 10).
		Radius(7).Gap(6).Justify(ui.Start).AlignItems(ui.Center).MaxWidth(160)
	chip.Children(func() {
		icon(c, "git-branch", 13, palette.textTertiary)
		ui.Text(c, label).FontSize(uiPx(12)).TextColor(palette.textSecondary).
			SingleLine().Ellipsis("…")
		icon(c, "chevron-down", 11, palette.textTertiary)
	})
	ui.PopoverBase(c, chip, &a.capsule.HistoryMenuOpen, func(panel ui.Element) {
		panel.Background(palette.raised).Border(1, palette.borderStrong).Radius(10).
			Width(220).MaxHeight(300).Padding(4).Clip().Children(func() {
			a.historyBranchItem(c, tr("All refs"), "", a.capsule.HistoryBranch == "")
			for _, branch := range a.capsule.HistoryBranches {
				branch := branch
				a.historyBranchItem(c, branch.Name, branch.Name, a.capsule.HistoryBranch == branch.Name)
			}
		})
	})
}

func (a *app) historyBranchItem(c *ui.Context, label, branch string, selected bool) {
	item := ui.ButtonBase(c).Label(label).FillWidth().Height(28).Padding(0, 8).Radius(6).
		Gap(8).Justify(ui.Start).AlignItems(ui.Center)
	if selected {
		item.Background(palette.overlayStrong)
	} else if item.Hovered() {
		item.Background(palette.overlay)
	}
	item.Children(func() {
		ui.Text(c, label).FontSize(uiPx(12)).TextColor(palette.text).
			Grow(1).MinWidth(0).SingleLine().Ellipsis("…")
		if selected {
			icon(c, "check", 12, palette.accentText)
		}
	})
	if item.Clicked() {
		a.capsule.HistoryMenuOpen = false
		if a.capsule.HistoryBranch != branch {
			a.capsule.HistoryBranch = branch
			a.capsule.HistorySel = -1
			a.capsule.HistoryRows = nil
			a.capsule.HistoryExhausted = false
			a.capsule.HistoryLoadingMore = false
			a.historyRefresh()
		}
	}
}

// historyList is the virtualized 60px rows with the selection detail below.
// The row builder only materializes visible rows, so reaching the tail is
// the lazy load trigger: one extra page stays buffered past the viewport.
func (a *app) historyList(c *ui.Context) {
	a.capsule.HistoryList.Selected = &a.capsule.HistorySel
	rows := a.capsule.HistoryRows
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		lanes := a.historyVisibleLanes(rows)
		list := ui.List(c, &a.capsule.HistoryList, len(rows), func(index int) {
			if index >= len(rows)-historyLoadBuffer && index >= 0 {
				a.historyRequestMore()
			}
			a.historyRowView(c, &rows[index], lanes, index)
		}).FillWidth().Grow(1)
		if list.Changed() && a.capsule.HistorySel >= 0 && a.capsule.HistorySel < len(rows) {
			a.capsule.HistoryStats = capsuleCommitStats{}
			a.capsule.HistoryStatsErr = ""
			a.historyLoadStats(rows[a.capsule.HistorySel].Commit.Hash)
		}
		a.historySelection(c)
	})
}

func (a *app) historyRowView(c *ui.Context, row *historyRow, lanes int, index int) {
	selected := a.capsule.HistorySel == index
	ui.Row(c).FillWidth().Height(historyRowHeight).Padding(0, 12, 0, 8).Gap(8).
		AlignItems(ui.Center).Children(func() {
		if selected {
			ui.Row(c).Absolute().Left(0).Top(0).Bottom(0).Width(2).Background(palette.accent)
		}
		a.historyGraphCell(c, row, lanes)
		ui.Column(c).Grow(1).MinWidth(0).Gap(2).Children(func() {
			a.historyRowTitle(c, row, index)
			a.historyRowMeta(c, row)
		})
	})
}

// historyRowTitle is the subject over the ref chips and the unpushed mark.
func (a *app) historyRowTitle(c *ui.Context, row *historyRow, index int) {
	ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
		weight := 500
		if row.Commit.Head {
			weight = 600
		}
		ui.Text(c, row.Commit.Subject).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(20)).
			FontWeight(weight).TextColor(palette.text).Grow(1).MinWidth(0).
			SingleLine().Ellipsis("…").Tooltip(row.Commit.Subject)
		shown := len(row.Commit.Refs)
		if shown > 2 {
			shown = 2
		}
		for refIndex := 0; refIndex < shown; refIndex++ {
			a.historyRefChip(c, row.Commit.Refs[refIndex])
		}
		if len(row.Commit.Refs) > shown {
			overflow := ""
			for _, ref := range row.Commit.Refs[shown:] {
				if overflow != "" {
					overflow += "\n"
				}
				overflow += ref.Label
			}
			a.historyRefChip(c, capsuleRef{Label: fmt.Sprintf("+%d", len(row.Commit.Refs)-shown), Tip: overflow})
		}
		if !row.Commit.Pushed {
			icon(c, "cloud-upload", 12, palette.warning).Tooltip(tr("Unpushed"))
		}
	})
}

func (a *app) historyRowMeta(c *ui.Context, row *historyRow) {
	hashColor := palette.textTertiary
	if row.Commit.Head {
		hashColor = palette.accentText
	}
	ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
		ui.Text(c, row.Commit.ShortHash).Font(monoFont()).FontSize(uiPx(10.5)).
			TextColor(hashColor).Shrink(0)
		ui.Text(c, row.Commit.Author).FontSize(uiPx(10.5)).TextColor(palette.textTertiary).
			Grow(1).MinWidth(0).SingleLine().Ellipsis("…")
		ui.Text(c, historyTimeAgo(row.Commit.Timestamp)).FontSize(uiPx(10.5)).
			TextColor(palette.textTertiary).Shrink(0)
	})
}

func (a *app) historyRefChip(c *ui.Context, ref capsuleRef) {
	color := palette.codeType
	if ref.Head {
		color = palette.accentText
	} else if ref.Remote {
		color = palette.codeKeyword
	}
	chip := ui.Row(c).Shrink(0).MaxWidth(120).Height(18).Padding(0, 6).Radius(4).
		Gap(3).AlignItems(ui.Center).Background(palette.overlay).
		Children(func() {
			ui.Text(c, ref.Label).FontSize(uiPx(10)).TextColor(color).SingleLine().Ellipsis("…")
		})
	if ref.Tip != "" {
		chip.Tooltip(ref.Tip)
	}
}

// historyVisibleLanes is the widest graph of the rows in view, which the
// graph column takes, so the subjects line up without the room a branch
// far away would take. Ported from MyGit's logview.
func (a *app) historyVisibleLanes(rows []historyRow) int {
	first, last := a.capsule.HistoryList.Visible()
	if last < first {
		first, last = 0, 50 // nothing shown yet
	}
	lanes := 1
	for i := max(first, 0); i <= min(last, len(rows)-1); i++ {
		lanes = max(lanes, rows[i].Row.Width)
	}
	return min(lanes, historyMaxLanes)
}

// historyGraphCell is one row's graph column at the shared lane width.
func (a *app) historyGraphCell(c *ui.Context, row *historyRow, lanes int) {
	width := float32(lanes*historyLaneWidth + 4)
	cell := ui.Box(c).Size(width, historyRowHeight).Draw(func(p *ui.Painter, r ui.Rect) {
		drawHistoryRow(p, r, row, row.Commit.Pushed)
	})
	if row.Commit.Pushed {
		return
	}
	cell.Tooltip(tr("Unpushed"))
}

// historyLaneColors color the graph's lanes in turn.
var historyLaneColors = []ui.Color{
	ui.RGB(66, 133, 244), ui.RGB(52, 168, 83), ui.RGB(234, 67, 53), ui.RGB(251, 188, 4),
	ui.RGB(171, 71, 188), ui.RGB(0, 172, 193), ui.RGB(255, 112, 67), ui.RGB(124, 179, 66),
}

func historyLaneColor(i int) ui.Color { return historyLaneColors[i%len(historyLaneColors)] }

// historyUnpushedColor draws the commits the upstream hasn't yet.
var historyUnpushedColor = ui.RGB(245, 130, 32)

// drawHistoryRow paints a row of the graph: its lines, then the commit's
// dot. Ported from MyGit's logview drawRow.
func drawHistoryRow(p *ui.Painter, r ui.Rect, row *historyRow, unpushed bool) {
	x := func(lane int) float32 { return r.X + 2 + float32(lane)*historyLaneWidth + historyLaneWidth/2 }
	y := func(at graph.Y) float32 { return r.Y + float32(at)*r.H/2 }
	limit := float32(historyMaxLanes) * historyLaneWidth
	for _, l := range row.Row.Lines {
		if l.From.Lane >= historyMaxLanes && l.To.Lane >= historyMaxLanes {
			continue
		}
		x0, y0, x1, y1 := x(l.From.Lane), y(l.From.Y), x(l.To.Lane), y(l.To.Y)
		x0, x1 = min(x0, r.X+limit), min(x1, r.X+limit)
		color := historyLaneColor(l.Color)
		if unpushed && l.From.Lane == row.Row.Lane && l.To.Lane == row.Row.Lane {
			color = historyUnpushedColor
		}
		if x0 == x1 {
			p.Line(x0, y0, x1, y1, historyGraphStroke, color)
			continue
		}
		var path ui.Path
		mid := (y0 + y1) / 2
		path.MoveTo(x0, y0).CubeTo(x0, mid, x1, mid, x1, y1)
		p.StrokePath(&path, historyGraphStroke, color)
	}
	if row.Row.Lane < historyMaxLanes {
		cx, cy := x(row.Row.Lane), y(graph.Mid)
		color := historyLaneColor(row.Row.Color)
		if unpushed {
			color = historyUnpushedColor
		}
		p.Fill(ui.Rect{X: cx - 4, Y: cy - 4, W: 8, H: 8}, color, 4)
	}
}

func historyTimeAgo(unix uint64) string {
	if unix == 0 {
		return ""
	}
	seconds := uint64(time.Now().Unix()) - unix
	if seconds < 60 {
		return tr("Just now")
	}
	return formatTurnStatsDuration(seconds * 1000)
}

// historySelection is the 140px detail area under the list: subject, hash
// with copy, numstat totals, author, refs and the unpushed mark.
func (a *app) historySelection(c *ui.Context) {
	ui.Column(c).FillWidth().Shrink(0).Height(historyDetailsH).
		BorderWidth(1, 0, 0, 0).BorderColor(palette.border).Padding(8, 10).Gap(2).Clip().Children(func() {
		if a.capsule.HistorySel < 0 || a.capsule.HistorySel >= len(a.capsule.HistoryRows) {
			ui.Text(c, tr("Select a commit")).FontSize(uiPx(11)).FixedLineHeight(uiPx(18)).
				TextColor(palette.textSecondary)
			return
		}
		row := &a.capsule.HistoryRows[a.capsule.HistorySel]
		commit := &row.Commit
		ui.Text(c, commit.Subject).FontSize(uiPx(11)).FixedLineHeight(uiPx(18)).
			TextColor(palette.text)
		ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
			ui.Text(c, commit.Hash).Font(monoFont()).FontSize(uiPx(11)).
				TextColor(palette.textSecondary).Shrink(0)
			copy := ui.ButtonBase(c).Label(tr("Copy hash")).Tooltip(tr("Copy hash")).
				Size(22, 22).Radius(5).Center().Shrink(0)
			if copy.Hovered() {
				copy.Background(palette.overlayStrong)
			}
			copy.Children(func() {
				copied := a.copiedMessages[commit.Hash] != 0
				name := "copy"
				if copied {
					name = "check"
				}
				icon(c, name, 12, palette.textMuted)
			})
			if copy.Clicked() {
				a.copyHistoryHash(commit.Hash, commit.ShortHash)
			}
		})
		stats := ""
		if a.capsule.HistoryStatsErr != "" {
			stats = tr("Stats unavailable")
		} else if a.capsule.HistoryStats.Files > 0 || a.capsule.HistoryStats.Additions > 0 || a.capsule.HistoryStats.Deletions > 0 {
			stats = fmt.Sprintf(tr("%d files"), a.capsule.HistoryStats.Files) + " " +
				fmt.Sprintf("+%d", a.capsule.HistoryStats.Additions) + " " +
				fmt.Sprintf("-%d", a.capsule.HistoryStats.Deletions)
		}
		if stats != "" {
			ui.Text(c, stats).FontSize(uiPx(11)).FixedLineHeight(uiPx(18)).
				TextColor(palette.textSecondary)
		}
		ui.Text(c, commit.Author).FontSize(uiPx(11)).FixedLineHeight(uiPx(18)).
			TextColor(palette.textSecondary)
		for _, ref := range commit.Refs {
			ui.Text(c, ref.Label).FontSize(uiPx(11)).FixedLineHeight(uiPx(18)).
				TextColor(palette.textSecondary)
		}
		if !commit.Pushed {
			ui.Text(c, tr("Unpushed")).FontSize(uiPx(11)).FixedLineHeight(uiPx(18)).
				TextColor(palette.warningText)
		}
	})
}

func (a *app) copyHistoryHash(hash, shortHash string) {
	_ = mygo.Clipboard.Write(transfer.TextData(hash))
	a.copiedMessages[hash] = uint64(time.Now().Unix())
	a.showCopied(hash)
}

// historyEnsure refreshes the history read and its branch list on the same
// throttle the rest of the capsule uses; force bypasses the throttle.
func (a *app) historyEnsure(force bool) {
	if a.capsule.Scope == "" {
		return
	}
	if force || time.Since(a.capsule.HistoryAt) >= capsuleRefreshInterval {
		a.capsule.HistoryAt = time.Now()
		a.historyRefresh()
		a.historyLoadBranches()
	}
}

// historyRefresh re-reads the newest page and merges it over the pages
// already loaded, so the lazy list keeps its depth and its place. A branch
// change or a fresh open resets the accumulation first.
func (a *app) historyRefresh() {
	scope := a.scScope()
	if scope.Client == nil {
		return
	}
	a.capsule.HistoryGen++
	gen := a.capsule.HistoryGen
	branch := a.capsule.HistoryBranch
	origin := a.capsule.Scope
	older := a.capsule.HistoryRows
	if len(older) == 0 {
		a.capsule.HistoryLoading = true
	}
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		_, root, err := scHydrate(ctx, scope)
		var response struct {
			Type    string          `json:"type"`
			Commits []capsuleCommit `json:"commits"`
		}
		if err == nil {
			operation := map[string]any{"type": "listCommits", "cwd": root, "limit": historyPageSize}
			if branch != "" {
				operation["branch"] = branch
			}
			err = scRequest(ctx, scope.Client, operation, &response)
		}
		var rows []historyRow
		if err == nil && response.Type == "commits" {
			rows = historyMergePages(response.Commits, older)
		}
		a.post(func() {
			if gen != a.capsule.HistoryGen || a.capsule.Scope != origin {
				return
			}
			a.capsule.HistoryLoading = false
			if err != nil {
				a.capsule.HistoryErr = err.Error()
				return
			}
			a.capsule.HistoryErr = ""
			a.capsule.HistoryRows = rows
			a.capsule.HistoryExhausted = len(response.Commits) < historyPageSize && len(rows) <= historyPageSize
			if a.capsule.HistorySel >= len(rows) {
				a.capsule.HistorySel = -1
			}
			a.capsule.HistoryStats = capsuleCommitStats{}
			a.capsule.HistoryStatsErr = ""
		})
	}()
}

// historyMergePages folds the fresh newest page over the older pages by
// hash, then relays the whole merged order through one graph layout.
func historyMergePages(fresh []capsuleCommit, older []historyRow) []historyRow {
	seen := make(map[string]bool, len(fresh))
	merged := make([]capsuleCommit, 0, len(fresh)+len(older))
	for _, commit := range fresh {
		seen[commit.Hash] = true
		merged = append(merged, commit)
	}
	for _, row := range older {
		if !seen[row.Commit.Hash] {
			merged = append(merged, row.Commit)
		}
	}
	return layoutHistory(merged)
}

// historyRequestMore appends the next page once the virtualized row builder
// reaches the tail of what is loaded; one page in flight at a time.
func (a *app) historyRequestMore() {
	if a.capsule.HistoryExhausted || a.capsule.HistoryLoadingMore || a.capsule.Scope == "" {
		return
	}
	loaded := len(a.capsule.HistoryRows)
	if loaded == 0 || loaded+historyPageSize > maxHistoryCommits {
		a.capsule.HistoryExhausted = true
		return
	}
	scope := a.scScope()
	if scope.Client == nil {
		return
	}
	a.capsule.HistoryLoadingMore = true
	gen := a.capsule.HistoryGen
	branch := a.capsule.HistoryBranch
	origin := a.capsule.Scope
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		_, root, err := scHydrate(ctx, scope)
		var response struct {
			Type    string          `json:"type"`
			Commits []capsuleCommit `json:"commits"`
		}
		if err == nil {
			operation := map[string]any{"type": "listCommits", "cwd": root, "limit": historyPageSize, "skip": loaded}
			if branch != "" {
				operation["branch"] = branch
			}
			err = scRequest(ctx, scope.Client, operation, &response)
		}
		a.post(func() {
			a.capsule.HistoryLoadingMore = false
			if gen != a.capsule.HistoryGen || a.capsule.Scope != origin || err != nil {
				return
			}
			fresh := response.Commits
			if len(fresh) < historyPageSize {
				a.capsule.HistoryExhausted = true
			}
			if len(fresh) == 0 {
				return
			}
			older := a.capsule.HistoryRows
			merged := make([]capsuleCommit, 0, len(older)+len(fresh))
			for _, row := range older {
				merged = append(merged, row.Commit)
			}
			seen := make(map[string]bool, len(older))
			for _, row := range older {
				seen[row.Commit.Hash] = true
			}
			for _, commit := range fresh {
				if !seen[commit.Hash] {
					merged = append(merged, commit)
				}
			}
			a.capsule.HistoryRows = layoutHistory(merged)
		})
	}()
}

// historyLoadBranches reads the branch list the filter menu offers.
func (a *app) historyLoadBranches() {
	scope := a.scScope()
	if scope.Client == nil {
		return
	}
	client := scope.Client
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		_, root, err := scHydrate(ctx, scope)
		var response struct {
			Type     string `json:"type"`
			Snapshot *struct {
				Branches []scBranch `json:"branches"`
			} `json:"snapshot"`
		}
		if err == nil {
			err = scRequest(ctx, client, map[string]any{"type": "inspectBranchRefs", "cwd": root}, &response)
		}
		a.post(func() {
			if err != nil || a.capsule.Scope == "" {
				return
			}
			if response.Snapshot != nil {
				a.capsule.HistoryBranches = response.Snapshot.Branches
			}
		})
	}()
}

// historyLoadStats reads the chosen commit's numstat totals; a changed
// selection supersedes the answer.
func (a *app) historyLoadStats(hash string) {
	scope := a.scScope()
	if scope.Client == nil || len(hash) != 40 {
		return
	}
	a.capsule.HistoryStatsGen++
	gen := a.capsule.HistoryStatsGen
	client := scope.Client
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), historyStatsTimeout)
		defer cancel()
		_, root, err := scHydrate(ctx, scope)
		var response struct {
			Type  string             `json:"type"`
			Stats capsuleCommitStats `json:"stats"`
		}
		if err == nil {
			err = scRequest(ctx, client, map[string]any{"type": "readCommitStats", "cwd": root, "hash": hash}, &response)
		}
		a.post(func() {
			if gen != a.capsule.HistoryStatsGen || a.capsule.Scope == "" {
				return
			}
			if err != nil {
				a.capsule.HistoryStatsErr = err.Error()
				return
			}
			a.capsule.HistoryStatsErr = ""
			a.capsule.HistoryStats = response.Stats
		})
	}()
}

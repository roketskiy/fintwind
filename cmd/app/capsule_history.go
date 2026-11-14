package main

import (
	"context"
	"fmt"
	"time"

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
	historyGraphLane    = 16.0
	historyGraphStroke  = 2.0
	historyStatsTimeout = 15 * time.Second
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
			a.historyRefresh()
		}
	}
}

// historyList is the virtualized 60px rows with the selection detail below.
func (a *app) historyList(c *ui.Context) {
	a.capsule.HistoryList.Selected = &a.capsule.HistorySel
	rows := a.capsule.HistoryRows
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		list := ui.List(c, &a.capsule.HistoryList, len(rows), func(index int) {
			a.historyRowView(c, &rows[index], index)
		}).FillWidth().Grow(1)
		if list.Changed() && a.capsule.HistorySel >= 0 && a.capsule.HistorySel < len(rows) {
			a.capsule.HistoryStats = capsuleCommitStats{}
			a.capsule.HistoryStatsErr = ""
			a.historyLoadStats(rows[a.capsule.HistorySel].Commit.Hash)
		}
		a.historySelection(c)
	})
}

func (a *app) historyRowView(c *ui.Context, row *capsuleGraphRow, index int) {
	selected := a.capsule.HistorySel == index
	ui.Row(c).FillWidth().Height(historyRowHeight).Padding(0, 12, 0, 8).Gap(8).
		AlignItems(ui.Center).Children(func() {
		if selected {
			ui.Row(c).Absolute().Left(0).Top(0).Bottom(0).Width(2).Background(palette.accent)
		}
		a.historyGraphCell(c, row)
		ui.Column(c).Grow(1).MinWidth(0).Gap(2).Children(func() {
			a.historyRowTitle(c, row, index)
			a.historyRowMeta(c, row)
		})
	})
}

// historyRowTitle is the subject over the ref chips and the unpushed mark.
func (a *app) historyRowTitle(c *ui.Context, row *capsuleGraphRow, index int) {
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

func (a *app) historyRowMeta(c *ui.Context, row *capsuleGraphRow) {
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

// historyGraphCell paints one row's lane geometry, VSCode scmGraph style:
// two-DIP strokes with S-curve bends, five cycling colors, a ringed HEAD
// dot, a cored merge dot and a soft halo on the chosen row.
func (a *app) historyGraphCell(c *ui.Context, row *capsuleGraphRow) {
	width := float32(row.LaneSpan) * historyGraphLane
	ui.Box(c).Width(width).MaxWidthPercent(0.35).Height(historyRowHeight).Shrink(0).
		Draw(func(p *ui.Painter, r ui.Rect) {
			step := r.W / float32(max(row.LaneSpan, 1))
			x := func(lane int) float32 {
				return r.X + step*(float32(lane)+0.5)
			}
			mid := r.Y + r.H/2
			bottom := r.Y + r.H
			curve := func(x0, y0, x1, y1 float32, color ui.Color) {
				var path ui.Path
				path.MoveTo(x0, y0)
				if (x0-x1) < 0.5 && (x1-x0) < 0.5 {
					path.LineTo(x1, y1)
				} else {
					midX := (x0 + x1) / 2
					midY := (y0 + y1) / 2
					path.CubeTo(x0, midY, midX, midY, midX, midY)
					path.CubeTo(midX, midY, x1, midY, x1, y1)
				}
				p.StrokePath(&path, historyGraphStroke, color)
			}
			in := x(row.Lane)
			color := historyLaneColor(row.Color)
			curve(in, r.Y, in, mid, color)
			if row.FlowDown {
				curve(in, mid, in, bottom, color)
			}
			for _, line := range row.Lines {
				lineColor := historyLaneColor(line.Color)
				if line.Dot {
					out := x(line.To)
					curve(in, mid, out, bottom, lineColor)
					continue
				}
				from, to := x(line.From), x(line.To)
				curve(from, r.Y, to, bottom, lineColor)
			}
			dot := min(step, float32(16)) * 0.45
			center := ui.Point{X: in, Y: mid}
			paintDot := func(diameter float32, dotColor ui.Color) {
				p.Fill(ui.Rect{X: center.X - diameter/2, Y: center.Y - diameter/2, W: diameter, H: diameter}, dotColor, diameter/2)
			}
			if row.Commit.Head {
				paintDot(dot+5, color)
				paintDot(dot*0.5, palette.composer)
			} else {
				if sel := a.capsule.HistorySel; sel >= 0 && sel < len(a.capsule.HistoryRows) && a.capsule.HistoryRows[sel].Commit.Hash == row.Commit.Hash {
					paintDot(dot+5, color.Alpha(0.22))
				}
				paintDot(dot, color)
				if len(row.Commit.Parents) > 1 {
					paintDot(dot*0.45, palette.composer)
				}
			}
		})
}

// historyLaneColor cycles VSCode's scmGraph.foreground registrations so a
// lane keeps its color across rows.
func historyLaneColor(index int) ui.Color {
	colors := []ui.Color{
		ui.RGB(0xFF, 0xB0, 0x00),
		ui.RGB(0xDC, 0x26, 0x7F),
		ui.RGB(0x99, 0x4F, 0x00),
		ui.RGB(0x40, 0xB0, 0xA6),
		ui.RGB(0xB6, 0x6D, 0xFF),
	}
	return colors[index%len(colors)]
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

// historyRefresh loads the branch-filtered history window and lays out its
// graph once, off-thread.
func (a *app) historyRefresh() {
	scope := a.scScope()
	if scope.Client == nil {
		return
	}
	a.capsule.HistoryGen++
	gen := a.capsule.HistoryGen
	branch := a.capsule.HistoryBranch
	origin := a.capsule.Scope
	a.capsule.HistoryLoading = true
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		_, root, err := scHydrate(ctx, scope)
		var response struct {
			Type    string          `json:"type"`
			Commits []capsuleCommit `json:"commits"`
		}
		if err == nil {
			operation := map[string]any{"type": "listCommits", "cwd": root, "limit": historyCommitLimit}
			if branch != "" {
				operation["branch"] = branch
			}
			err = scRequest(ctx, scope.Client, operation, &response)
		}
		var rows []capsuleGraphRow
		if err == nil && response.Type == "commits" {
			rows = layoutCapsuleGraph(response.Commits)
		}
		a.post(func() {
			if gen != a.capsule.HistoryGen || a.capsule.Scope != origin {
				return
			}
			a.capsule.HistoryLoading = false
			if err != nil {
				if a.capsule.HistoryErr == "" {
					a.capsule.HistoryErr = err.Error()
				}
				return
			}
			a.capsule.HistoryErr = ""
			a.capsule.HistoryRows = rows
			if a.capsule.HistorySel >= len(rows) {
				a.capsule.HistorySel = -1
			}
			a.capsule.HistoryStats = capsuleCommitStats{}
			a.capsule.HistoryStatsErr = ""
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

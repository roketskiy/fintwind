package main

import (
	"fmt"
	"time"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
	lobechoice "github.com/lobehub/lobe-mygo/pkg/choice"
	"github.com/lobehub/lobe-mygo/pkg/display"
	"github.com/lobehub/lobe-mygo/pkg/feedback"
)

// The capsule's view: an icon bar above the composer whose tabs open a
// card, mirroring render_task_capsule in background_work/capsule.rs. The
// collapsed bar carries the change totals and the live counts; the card
// holds the tab bodies.

const (
	capsuleBarHeight   = 30.0
	capsuleChipSize    = 26.0
	capsulePanelWidth  = 420.0
	capsulePanelMaxH   = 440.0
	capsuleGraphLane   = 14.0
	capsuleGraphRowH   = 26.0
	capsuleGraphDot    = 8.0
	capsuleGraphStroke = 1.6
)

// capsuleTabMeta describes one tab: identity, icon and its collapsed count.
type capsuleTabMeta struct {
	Tab   capsuleTab
	Icon  string
	Label string
}

func capsuleTabs() []capsuleTabMeta {
	return []capsuleTabMeta{
		{capsuleChanges, "file-diff", tr("Changes")},
		{capsuleTodo, "list", tr("Todo")},
		{capsuleTasks, "bot", tr("Tasks")},
	}
}

// capsuleAnchor draws the bar row above the composer card and hosts its
// popover. It reads as nothing without a workspace.
func (a *app) capsuleAnchor(c *ui.Context) {
	a.capsuleEnsure()
	if a.capsule.Scope == "" {
		return
	}
	ui.Row(c).FillWidth().MaxWidth(transcriptWidth).Margin(0, ui.Auto, 6, ui.Auto).
		Justify(ui.End).Children(func() {
		var trigger ui.Element
		ui.Row(c).FillWidth().Justify(ui.End).Children(func() {
			bar := ui.Row(c).Height(capsuleBarHeight).Padding(0, 5).Gap(2).AlignItems(ui.Center).
				Background(palette.composer).Border(1, palette.cardBorder).Radius(10)
			bar = cardShadow(bar)
			bar.Children(func() {
				a.capsuleChips(c, false)
			})
			trigger = bar
		})
		ui.PopoverBase(c, trigger, &a.capsule.Open, func(panel ui.Element) {
			panel.AttachTo(trigger, ui.AnchorTopRight, ui.AnchorBottomRight).Margin(0, 0, 6, 0)
			panelWidth := float32(capsulePanelWidth)
			if winW, _ := c.Size(); float32(winW)-40 < panelWidth {
				panelWidth = max(float32(winW)-40, 200)
			}
			panel.Width(panelWidth).
				Background(palette.raised).Border(1, palette.borderStrong).Radius(12)
			panel = cardShadow(panel)
			panel.Children(func() {
				ui.Scroll(c).FillWidth().MaxHeight(capsulePanelMaxH - capsuleBarHeight - 8).Children(func() {
					ui.Column(c).FillWidth().Padding(12).Gap(10).Children(func() {
						switch a.capsule.Tab {
						case capsuleChanges:
							a.capsuleChangesBody(c)
						case capsuleTodo:
							a.capsuleTodoBody(c)
						case capsuleTasks:
							a.capsuleTasksBody(c)
						}
					})
				})
				ui.Row(c).FillWidth().Height(1).Shrink(0).Background(palette.border)
				ui.Row(c).FillWidth().Shrink(0).Height(capsuleBarHeight).Padding(0, 5).
					Gap(2).AlignItems(ui.Center).Children(func() {
					a.capsuleChips(c, true)
				})
			})
		})
	})
}

// capsuleChips builds the tab bar: icon chips with a selected wash, the
// collapsed changes chip widened by its diff stat, and count badges.
func (a *app) capsuleChips(c *ui.Context, expanded bool) {
	todo := a.capsuleTodosFor()
	openTodos := 0
	if todo != nil {
		openTodos = todo.Open
	}
	live := 0
	for _, item := range a.capsuleItemsFor() {
		if item.Live() {
			live++
		}
	}
	for _, meta := range capsuleTabs() {
		meta := meta
		selected := expanded && a.capsule.Tab == meta.Tab
		chip := ui.ButtonBase(c).Label(meta.Label).Tooltip(meta.Label).
			Height(capsuleChipSize).MinWidth(capsuleChipSize).Padding(0, 6).Radius(7).
			Center().Shrink(0)
		if selected {
			chip.Background(palette.overlayStrong)
		} else if chip.Hovered() {
			chip.Background(palette.overlay)
		}
		chip.Children(func() {
			color := palette.textSecondary
			if selected {
				color = palette.text
			}
			additions, deletions := a.capsule.Additions, a.capsule.Deletions
			if !expanded && meta.Tab == capsuleChanges && (additions > 0 || deletions > 0) {
				ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
					icon(c, meta.Icon, 15, color)
					ui.Row(c).Gap(5).AlignItems(ui.Center).Children(func() {
						if additions > 0 {
							ui.Text(c, fmt.Sprintf("+%d", additions)).FontSize(uiPx(11.5)).
								FontWeight(500).TextColor(palette.successText)
						}
						if deletions > 0 {
							ui.Text(c, fmt.Sprintf("-%d", deletions)).FontSize(uiPx(11.5)).
								FontWeight(500).TextColor(palette.dangerText)
						}
					})
				})
				return
			}
			icon(c, meta.Icon, 15, color)
			count := 0
			switch meta.Tab {
			case capsuleTodo:
				count = openTodos
			case capsuleTasks:
				count = live
			}
			if count > 0 {
				ui.Row(c).Absolute().Top(-2).Right(-2).MinWidth(14).Height(14).Padding(0, 3).
					Radius(7).Center().Background(palette.accentFill).Children(func() {
					ui.Text(c, fmt.Sprintf("%d", count)).FontSize(uiPx(9)).
						TextColor(palette.onAccent)
				})
			}
		})
		if chip.Clicked() {
			if a.capsule.Open && a.capsule.Tab == meta.Tab {
				a.capsule.Open = false
			} else {
				a.capsule.Tab = meta.Tab
				a.capsule.Open = true
				if meta.Tab == capsuleChanges && time.Since(a.capsule.CommitsAt) >= capsuleRefreshInterval {
					a.capsule.CommitsAt = time.Now()
					a.capsuleRefreshCommits()
				}
			}
		}
	}
}

// capsuleHeading is one section title inside the card.
func capsuleHeading(c *ui.Context, text string) {
	ui.Text(c, text).FontSize(uiPx(13)).FontWeight(600).TextColor(palette.text)
}

// capsuleChangesBody is the changes tab: the branch and working-tree totals
// over the local history graph. It shows no code content, only stats.
func (a *app) capsuleChangesBody(c *ui.Context) {
	branch := a.capsule.Branch
	if branch == "" {
		branch = tr("No branch")
	}
	ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
		icon(c, "git-branch", 14, palette.textSecondary)
		ui.Text(c, branch).FontSize(uiPx(12.5)).TextColor(palette.text).SingleLine()
		ui.Row(c).Grow(1)
		if a.capsule.Additions > 0 || a.capsule.Deletions > 0 {
			ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
				ui.Text(c, fmt.Sprintf("+%d", a.capsule.Additions)).FontSize(uiPx(12)).
					FontWeight(500).TextColor(palette.successText)
				ui.Text(c, fmt.Sprintf("-%d", a.capsule.Deletions)).FontSize(uiPx(12)).
					FontWeight(500).TextColor(palette.dangerText)
			})
		}
	})
	if len(a.capsule.Commits) == 0 {
		capsuleGraphEmpty(c)
		return
	}
	ui.Column(c).FillWidth().Gap(2).Children(func() {
		for index := range a.capsule.Commits {
			a.capsuleGraphRowView(c, &a.capsule.Commits[index])
		}
	})
}

func capsuleGraphEmpty(c *ui.Context) {
	ui.Row(c).FillWidth().Padding(8, 0, 4, 0).Children(func() {
		ui.Text(c, tr("No commits yet")).FontSize(uiPx(12)).TextColor(palette.textTertiary)
	})
}

// capsuleGraphRowView is one history row: the drawn lane geometry beside
// the subject, author and time.
func (a *app) capsuleGraphRowView(c *ui.Context, row *capsuleGraphRow) {
	width := float32(row.LaneSpan)*capsuleGraphLane + 4
	ui.Row(c).FillWidth().MinWidth(0).Height(capsuleGraphRowH).Gap(8).AlignItems(ui.Center).Children(func() {
		ui.Box(c).Size(width, capsuleGraphRowH).Draw(func(p *ui.Painter, r ui.Rect) {
			a.drawCapsuleGraphRow(p, r, row)
		})
		ui.Column(c).Grow(1).MinWidth(0).Gap(1).Children(func() {
			ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
				ui.Text(c, row.Commit.Subject).FontSize(uiPx(12)).FixedLineHeight(uiPx(15)).
					TextColor(palette.text).Grow(1).MinWidth(0).SingleLine().Ellipsis("…")
				if row.Commit.Head {
					ui.Text(c, tr("HEAD")).FontSize(uiPx(10)).FontWeight(600).
						TextColor(palette.accentText)
				} else if !row.Commit.Pushed {
					ui.Text(c, tr("Unpushed")).FontSize(uiPx(10)).TextColor(palette.warningText)
				}
			})
			ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
				ui.Text(c, row.Commit.Author).FontSize(uiPx(10.5)).TextColor(palette.textTertiary).
					Grow(1).MinWidth(0).SingleLine().Ellipsis("…")
				ui.Text(c, row.Commit.ShortHash).FontSize(uiPx(10.5)).TextColor(palette.textGhost)
				ui.Text(c, capsuleCommitTime(row.Commit.Timestamp)).FontSize(uiPx(10.5)).
					TextColor(palette.textTertiary)
			})
		})
	})
}

// drawCapsuleGraphRow paints one row the way MyGit's logview does: a dot at
// the commit's lane center, straight lane lines through the row, and cubic
// curves leaving the dot for lanes that open or merge.
func (a *app) drawCapsuleGraphRow(p *ui.Painter, r ui.Rect, row *capsuleGraphRow) {
	x := func(lane int) float32 {
		return r.X + 2 + float32(lane)*capsuleGraphLane + capsuleGraphLane/2
	}
	mid := r.Y + r.H/2
	in := x(row.Lane)
	color := capsuleLaneColor(row.Color)
	p.Line(in, r.Y, in, mid, capsuleGraphStroke, color)
	if row.FlowDown {
		p.Line(in, mid, in, r.Y+r.H, capsuleGraphStroke, color)
	}
	p.Fill(ui.Rect{X: in - capsuleGraphDot/2, Y: mid - capsuleGraphDot/2, W: capsuleGraphDot, H: capsuleGraphDot}, color, capsuleGraphDot/2)
	for _, line := range row.Lines {
		lineColor := capsuleLaneColor(line.Color)
		if line.Dot {
			out := x(line.To)
			control := (mid + r.Y + r.H) / 2
			var path ui.Path
			path.MoveTo(in, mid).CubeTo(in, control, out, control, out, r.Y+r.H)
			p.StrokePath(&path, capsuleGraphStroke, lineColor)
			continue
		}
		from, to := x(line.From), x(line.To)
		if from == to {
			p.Line(from, r.Y, to, r.Y+r.H, capsuleGraphStroke, lineColor)
		}
	}
}

// capsuleLaneColor cycles the graph's lane palette, kept inside the app's
// own hues.
func capsuleLaneColor(index int) ui.Color {
	colors := []ui.Color{
		palette.link,
		palette.success,
		palette.danger,
		palette.warning,
		palette.codeKeyword,
		palette.codeType,
		palette.favorite,
	}
	return colors[index%len(colors)]
}

func capsuleCommitTime(unix uint64) string {
	if unix == 0 {
		return ""
	}
	at := time.Unix(int64(unix), 0)
	if now := time.Now(); at.Year() == now.Year() && at.YearDay() == now.YearDay() {
		return at.Format("3:04 PM")
	}
	return at.Format("Jan 2")
}

// capsuleTodoBody is the todo tab: the agent's plan, one marked row per item.
func (a *app) capsuleTodoBody(c *ui.Context) {
	todos := a.capsuleTodosFor()
	if todos == nil || len(todos.Items) == 0 {
		ui.Column(c).FillWidth().Gap(8).Children(func() {
			capsuleHeading(c, tr("Todo"))
			display.Empty(c, display.EmptyProps{Description: tr("No todo list yet")})
		})
		return
	}
	ui.Column(c).FillWidth().Gap(8).Children(func() {
		ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
			ui.Row(c).Grow(1).Children(func() {
				capsuleHeading(c, tr("Todo"))
			})
			ui.Text(c, fmt.Sprintf("%d/%d", todos.Completed, len(todos.Items))).
				FontSize(uiPx(11)).TextColor(palette.textTertiary)
		})
		for _, item := range todos.Items {
			a.capsuleTodoRow(c, item)
		}
	})
}

func (a *app) capsuleTodoRow(c *ui.Context, item capsuleTodoItem) {
	ui.Row(c).FillWidth().MinWidth(0).Gap(8).AlignItems(ui.Start).Children(func() {
		ui.Row(c).Width(16).Height(17).Shrink(0).Center().Children(func() {
			switch item.Status {
			case todoCompleted:
				checked := true
				lobechoice.Checkbox(c, &checked, "", lobechoice.CheckboxProps{Size: 14})
			case todoInProgress:
				feedback.Spin(c, feedback.SpinProps{CustomSize: 14}, nil)
			case todoCancelled:
				icon(c, "x", 13, palette.textTertiary)
			default:
				checked := false
				lobechoice.Checkbox(c, &checked, "", lobechoice.CheckboxProps{Size: 14})
			}
		})
		color := palette.textSecondary
		switch item.Status {
		case todoInProgress:
			color = palette.text
		case todoCompleted, todoCancelled:
			color = palette.textTertiary
		}
		text := ui.Text(c, item.Content).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(17)).
			TextColor(color).Grow(1).MinWidth(0)
		if item.Status == todoCompleted {
			text.Strikethrough()
		}
	})
}

// capsuleTasksBody is the tasks tab: the session's live subagents over its
// background processes. A row opens its surface in the right panel.
func (a *app) capsuleTasksBody(c *ui.Context) {
	items := a.capsuleItemsFor()
	agents := make([]model.BackgroundWorkItem, 0, len(items))
	processes := make([]model.BackgroundWorkItem, 0, len(items))
	for _, item := range items {
		if item.Key.Kind == "subagent" {
			agents = append(agents, item)
		} else {
			processes = append(processes, item)
		}
	}
	ui.Column(c).FillWidth().Gap(12).Children(func() {
		a.capsuleTaskSection(c, tr("Subagents"), agents, tr("No agents"))
		a.capsuleTaskSection(c, tr("Processes"), processes, tr("No background processes"))
	})
}

func (a *app) capsuleTaskSection(c *ui.Context, label string, items []model.BackgroundWorkItem, empty string) {
	ui.Column(c).FillWidth().Gap(4).Children(func() {
		ui.Text(c, label).FontSize(uiPx(12)).TextColor(palette.textTertiary)
		if len(items) == 0 {
			ui.Row(c).FillWidth().Padding(6, 8).Children(func() {
				ui.Text(c, empty).FontSize(uiPx(12)).TextColor(palette.textTertiary)
			})
			return
		}
		for _, item := range items {
			a.capsuleTaskRow(c, item)
		}
	})
}

func (a *app) capsuleTaskRow(c *ui.Context, item model.BackgroundWorkItem) {
	row := ui.ButtonBase(c).Label(item.Title).FillWidth().Height(30).Padding(0, 8).Radius(7).
		Gap(8).Justify(ui.Start).AlignItems(ui.Center)
	if row.Hovered() {
		row.Background(palette.overlay)
	}
	row.Children(func() {
		a.capsuleStatusIcon(c, item)
		ui.Text(c, item.Title).FontSize(uiPx(12.5)).TextColor(palette.text).
			Grow(1).MinWidth(0).SingleLine().Ellipsis("…")
		ui.Text(c, capsuleElapsed(item)).FontSize(uiPx(11)).TextColor(palette.textTertiary)
	})
	if row.Clicked() {
		a.openBackgroundSurface(backgroundSurfaceKey(item.Key))
	}
}

// capsuleStatusIcon mirrors work_status_icon: a slow loader while live,
// then the settled mark.
func (a *app) capsuleStatusIcon(c *ui.Context, item model.BackgroundWorkItem) {
	color := palette.textTertiary
	name := "square"
	switch {
	case item.Live():
		color = palette.accent
		name = "loader-circle"
	case item.Status == "completed":
		color = palette.successText
		name = "check"
	case item.Status == "failed":
		color = palette.dangerText
		name = "x"
	case item.Status == "lost":
		color = palette.dangerText
		name = "alert"
	case item.Status == "stopping" || item.Status == "stopped":
		name = "stop"
	}
	if item.Live() {
		c.AnimationFrame()
		icon(c, name, 13, color).Rotate(float32(c.Now().UnixMilli()%1000) * 0.36)
		return
	}
	icon(c, name, 13, color)
}

func capsuleElapsed(item model.BackgroundWorkItem) string {
	ms := item.UpdatedAtMs - item.StartedAtMs
	if item.Live() {
		ms = uint64(time.Now().UnixMilli()) - item.StartedAtMs
	} else if item.DurationMs != nil {
		ms = *item.DurationMs
	}
	if ms < 0 {
		ms = 0
	}
	return formatTurnStatsDuration(ms)
}

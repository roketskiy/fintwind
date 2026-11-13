package main

import (
	"fmt"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/feedback"
	"github.com/lobehub/lobe-mygo/pkg/layout"
)

type scheduledLane int

const (
	scheduledPaused scheduledLane = iota
	scheduledEnabled
	scheduledRunning
)

type scheduledBoardState struct {
	lists          [3]ui.ListState
	open           bool
	discard        bool
	original       string
	ready          bool
	query, filter  string
	lanes          [3][]scheduledTask
	visibleCount   int
	refreshPending bool
	editingTask    *scheduledTask
	saveStarted    time.Time
}

// Carry identity, not a task snapshot: a drop must use the latest revision.
type scheduledDrag struct{ ID string }

func (a *app) scheduledTaskRunning(task scheduledTask) bool {
	if task.Pending != nil {
		return true
	}
	if task.Last == nil || task.Last.Outcome.Outcome != "accepted" || task.Last.Outcome.SubmissionID == nil {
		return false
	}
	session := a.eventSession(schedLastSession(task))
	if session == nil {
		session = a.summarySession(schedLastSession(task))
	}
	return session != nil && session.Busy() && session.ActiveTurnID() == *task.Last.Outcome.SubmissionID
}

func (a *app) scheduledTaskLane(task scheduledTask) scheduledLane {
	if a.scheduledTaskRunning(task) {
		return scheduledRunning
	}
	if task.Enabled {
		return scheduledEnabled
	}
	return scheduledPaused
}

func (a *app) scheduledCanDrop(value scheduledDrag, lane scheduledLane) bool {
	st := &a.settings.backend
	if !st.sched.loaded || st.sched.writing || st.sched.uncertain {
		return false
	}
	for _, task := range st.schedTasks {
		if task.ID != value.ID {
			continue
		}
		switch lane {
		case scheduledPaused:
			return task.Enabled
		case scheduledEnabled:
			return !task.Enabled
		case scheduledRunning:
			return !a.scheduledTaskRunning(task)
		}
	}
	return false
}

func (a *app) scheduledDrop(value scheduledDrag, lane scheduledLane) {
	if !a.scheduledCanDrop(value, lane) {
		return
	}
	for _, task := range a.settings.backend.schedTasks {
		if task.ID != value.ID {
			continue
		}
		if lane == scheduledRunning {
			a.runScheduledTaskNow(task)
		} else {
			a.toggleScheduledTask(task, lane == scheduledEnabled)
		}
		return
	}
}

func (a *app) scheduledBoard(c *ui.Context) {
	st, form := &a.settings.backend, &a.settings.catalog
	board := &st.schedBoard
	if !board.ready || board.query != form.schedQuery || board.filter != form.schedFilter {
		visible, _ := a.schedVisible(st.schedTasks, form.schedFilter, form.schedQuery)
		board.lanes = [3][]scheduledTask{}
		for _, task := range visible {
			lane := a.scheduledTaskLane(task)
			board.lanes[lane] = append(board.lanes[lane], task)
		}
		board.visibleCount, board.query, board.filter, board.ready = len(visible), form.schedQuery, form.schedFilter, true
	}
	ui.Column(c.Key("scheduled-workspace")).Fill().MinWidth(0).Background(palette.canvas).
		Radius(workspaceRadius, 0, 0, workspaceRadius).Clip().Padding(18, 24).Gap(18).Children(func() {
		ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
			ui.Text(c, tr("Scheduled tasks")).FontSize(uiPx(20)).FontWeight(500).TextColor(palette.text).Grow(1)
			ui.Row(c).Width(220).Children(func() { settingsSearchInput(c, "scheduled-search", tr("Search tasks"), &form.schedQuery) })
			componentTextButton(c, "scheduled-reload", tr("Reload"), false).Disabled(st.sched.loading || st.sched.writing).OnClick(a.loadSettingsScheduled)
			componentTextButton(c, "scheduled-new", tr("New task"), true).Disabled(st.sched.writing).OnClick(func() { a.beginScheduledTask(nil) })
		})
		if st.sched.writing && !board.saveStarted.IsZero() {
			if time.Since(board.saveStarted) < time.Second {
				c.After(time.Second)
			}
			label := tr("Saving...")
			if time.Since(board.saveStarted) >= time.Second {
				label = tr("Save not confirmed")
			}
			ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
				ui.Text(c, label).FontSize(uiPx(11)).TextColor(palette.textSecondary)
				componentTextButton(c, "scheduled-return-to-draft", tr("Edit task"), false).OnClick(func() { board.open = true })
			})
		} else {
			settingsOperationStatus(c, st.sched, "")
		}
		if !st.sched.writing && form.schedAdding && !board.open {
			componentTextButton(c, "scheduled-resume-draft", tr("Keep editing"), false).OnClick(func() { board.open = true })
		}
		if st.sched.loaded && board.visibleCount == 0 && len(st.schedTasks) > 0 {
			ui.Text(c, tr("No matching tasks")).TextColor(palette.textTertiary)
		}
		ui.Row(c).FillWidth().Grow(1).MinHeight(0).Gap(16).AlignItems(ui.Start).Children(func() {
			for lane := scheduledPaused; lane <= scheduledRunning; lane++ {
				a.scheduledBoardLane(c, lane, board.lanes[lane])
			}
		})
	})
	a.scheduledTaskModal(c)
}

func (a *app) scheduledBoardLane(c *ui.Context, lane scheduledLane, tasks []scheduledTask) {
	st := &a.settings.backend
	titles := [3]string{tr("Paused"), tr("Enabled"), tr("Running")}
	glyphs := [3]string{"pause", "clock", "play"}
	title := titles[lane]
	_, windowHeight := c.Size()
	laneHeight := min(max(float32(180), windowHeight-160), float32(64+len(tasks)*150))
	laneHeight = max(float32(150), laneHeight)
	column := layout.Block(c.Key(fmt.Sprintf("scheduled-lane-%d", lane)), layout.BlockProps{Variant: layout.BlockFilled}).
		Grow(1).Basis(0).MinWidth(0).Height(laneHeight).Radius(10).Padding(8).Gap(4).Background(palette.overlay).Label(title)
	value, over := ui.DragOver[scheduledDrag](column)
	active := over && a.scheduledCanDrop(value, lane)
	if active {
		column.Height(min(max(float32(180), windowHeight-160), laneHeight+40))
		column.Border(1, palette.accent)
	} else {
		column.Border(1, ui.Transparent)
	}
	if dropped, ok := ui.Drop[scheduledDrag](column); ok {
		a.scheduledDrop(dropped, lane)
	}
	column.Children(func() {
		ui.Row(c).FillWidth().Height(34).Padding(0, 6).Gap(8).AlignItems(ui.Center).Children(func() {
			icon(c, glyphs[lane], 14, palette.textSecondary)
			ui.Text(c, title).FontSize(uiPx(12.5)).FontWeight(500).TextColor(palette.text)
			ui.Text(c, fmt.Sprint(len(tasks))).FontSize(uiPx(11)).TextColor(palette.textTertiary).Grow(1)
		})
		// One stable target owns the entire lane. A row target that grows its
		// own hit box oscillates between the lane and row as siblings move.
		landing := ui.Column(c.Key("landing")).FillWidth().Clip()
		height := float32(0)
		if active {
			height = 40
		}
		landing.Height(height).Transition(ui.ElementTransition{Height: true, Duration: 140 * time.Millisecond}).Children(func() {
			if active {
				scheduledLanding(c, lane, title)
			}
		})
		ui.List(c.Key("cards"), &st.schedBoard.lists[lane], len(tasks)).FillWidth().Grow(1).MinHeight(0).
			ItemKey(func(i int) any { return tasks[i].ID }).Rows(func(row ui.ListRow) {
			task := tasks[row.Index]
			rc := row.Context
			wrap := ui.Column(rc.Key("card-slot")).FillWidth().Padding(0, 0, 10, 0)
			wrap.Children(func() {
				a.scheduledBoardCard(rc, task)
			})
		}).Children(func() {
			if len(tasks) == 0 && !active {
				ui.Column(c).FillWidth().Height(88).Center().Children(func() {
					ui.Text(c, "—").FontSize(uiPx(15)).TextColor(palette.textTertiary)
				})
			}
		})
	})
}

func scheduledLanding(c *ui.Context, lane scheduledLane, title string) {
	label := title
	if lane == scheduledRunning {
		label = tr("Run now")
	}
	layout.Block(c, layout.BlockProps{Variant: layout.BlockOutlined}).FillWidth().Height(36).Radius(6).
		Border(1, palette.accent).Background(palette.raised).Center().Children(func() {
		ui.Text(c, label).FontSize(uiPx(12)).TextColor(palette.textSecondary)
	})
}

func (a *app) scheduledBoardCard(c *ui.Context, task scheduledTask) {
	st := &a.settings.backend
	busy := st.sched.writing || st.sched.uncertain || !st.sched.loaded
	card := layout.Block(c.Key("task-"+task.ID), layout.BlockProps{Variant: layout.BlockOutlined, Clickable: true}).
		FillWidth().MinWidth(0).Height(140).Padding(12).Gap(6).Radius(7).Background(palette.raised.Mix(hex(0xFFFFFF), choice(activeAppearance.dark, float32(0.025), float32(0.75)))).Border(1, palette.cardBorder).
		Label(task.Title)
	if !busy {
		card.Drag(scheduledDrag{task.ID})
	}
	if card.Hovered() || card.Focused() {
		card.Border(1, palette.accent)
	}
	if card.Dragging() {
		// MyGo clears root opacity on the pointer preview, but not Pose fade.
		// Dim the source without making the preview equally translucent.
		card.Opacity(0.3)
	}
	card.Children(func() {
		ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
			icon(c, "clock", 12, palette.textTertiary)
			ui.Text(c, schedScheduleSummary(task.Schedule)).FontSize(uiPx(10.5)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...").Grow(1).MinWidth(0)
		})
		ui.Text(c, task.Title).FontSize(uiPx(13.5)).FontWeight(600).TextColor(palette.text).SingleLine().Ellipsis("...")
		excerpt := task.Prompt
		if len(excerpt) > 384 {
			excerpt = strings.ToValidUTF8(excerpt[:384], "") + "…"
		}
		ui.Text(c, strings.Join(strings.Fields(excerpt), " ")).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
		ui.Row(c).FillWidth().Gap(5).AlignItems(ui.Center).Children(func() {
			model := task.Target.Launch.Model
			if task.Target.Kind == "existingSession" {
				if session := a.summarySession(task.Target.SessionID); session != nil && session.Model != nil {
					model = *session.Model
				}
			}
			mark := companyIconFor(model)
			if !activeAppearance.dark {
				mark = strings.TrimSuffix(mark, "-dark")
			}
			if mark == "" {
				mark = "sparkles"
			}
			markIcon(c, mark, 16, palette.textSecondary)
			if model == "" {
				model = tr("Existing session")
			}
			name := model
			if _, tail, ok := strings.Cut(model, "/"); ok {
				name = tail
			}
			ui.Text(c, name).FontSize(uiPx(10.5)).TextColor(palette.textSecondary).SingleLine().Ellipsis("...").Grow(1).MinWidth(0).Tooltip(model)
		})
		ui.Row(c).FillWidth().Gap(6).Children(func() {
			ui.Text(c, a.schedNextLabel(task)).FontSize(uiPx(10)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...").Grow(1).MinWidth(0)
			status := schedStateLabel(schedTaskState(task))
			if a.scheduledTaskRunning(task) && task.Pending == nil {
				status = tr("Running")
			}
			ui.Text(c, status).FontSize(uiPx(10)).TextColor(palette.textSecondary)
		})
	})
	card.OnClick(func() {
		a.settings.catalog.schedAdding = false
		st.schedSelected, st.schedEditing, st.schedDeleteConfirm = task.ID, "", ""
		st.schedBoard.open, st.schedBoard.discard = true, false
		a.closeSettingsMenus()
	})
	card.ContextMenu(func(m *ui.Menu) {
		if m.Item(tr("Edit task")).Disabled(busy).Chosen() {
			a.beginScheduledTask(&task)
		}
		if m.Item(tr("Run now")).Disabled(busy || a.scheduledTaskRunning(task)).Chosen() {
			a.runScheduledTaskNow(task)
		}
		if m.Item(choice(task.Enabled, tr("Pause"), tr("Enable"))).Disabled(busy).Chosen() {
			a.toggleScheduledTask(task, !task.Enabled)
		}
		m.Separator()
		if m.Item(tr("Delete")).Disabled(busy).Chosen() {
			st.schedSelected, st.schedDeleteConfirm, st.schedBoard.open = task.ID, "scheduled:"+task.ID, true
		}
	})
}

func (a *app) scheduledFormSnapshot() string {
	f := &a.settings.catalog
	return fmt.Sprintf("%#v", []any{f.schedTitle, f.schedPrompt, f.schedSchedule, f.schedTime, f.schedWeekdays, f.schedDate, f.schedInterval, f.schedTarget, f.schedProject, f.schedSession, f.schedModel, f.schedAccess, f.schedEnabled})
}

func (a *app) scheduledTaskModal(c *ui.Context) {
	st := &a.settings.backend
	board := &st.schedBoard
	if !a.settings.catalog.schedAdding && a.selectedScheduledTask() == nil {
		board.open = false
	}
	wasOpen := board.open
	_, h := c.Size()
	height := min(float32(560), max(float32(200), h-180))
	title, width := tr("Scheduled tasks"), float32(640)
	if a.settings.catalog.schedAdding {
		title = choice(st.schedEditing != "", tr("Edit task"), tr("New task"))
	}
	if board.discard {
		title, width = tr("Discard changes?"), 380
	}
	feedback.Modal(c.Key("scheduled-task-modal"), &board.open, feedback.ModalProps{Title: title, Width: width, NoFooter: true}, func() {
		if board.discard {
			ui.Row(c).FillWidth().Padding(16, 0, 0, 0).Gap(8).Justify(ui.End).Children(func() {
				componentTextButton(c, "scheduled-keep-editing", tr("Keep editing"), false).OnClick(func() { board.discard = false })
				componentTextButton(c, "scheduled-discard", tr("Discard"), true).OnClick(a.finishScheduledEditor)
			})
		} else {
			ui.Column(c).FillWidth().Height(height).Children(func() { a.scheduledDetail(c) })
		}
	})
	if wasOpen && !board.open {
		if board.discard {
			board.open, board.discard = true, false
			return
		}
		a.cancelScheduledTask()
	}
}

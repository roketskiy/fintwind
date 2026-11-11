package main

import (
	"fmt"
	"strings"

	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/theme"
	lobechoice "github.com/lobehub/lobe-mygo/pkg/choice"
)

// scheduledSettings is the standalone scheduled-tasks workspace: a
// mail-style roster left and detail right, entered from the mode rail or
// the command palette like GPUI's WorkspaceMode::ScheduledTasks.
func (a *app) scheduledSettings(c *ui.Context) {
	a.mailRoot(c, func() {
		a.scheduledListPane(c)
	}, func() {
		a.scheduledDetail(c)
	})
}

func schedFilterOptions() []string {
	return []string{tr("All"), tr("Active"), tr("Paused")}
}

func schedFilterIndex(filter string) int {
	switch filter {
	case "active":
		return 1
	case "paused":
		return 2
	default:
		return 0
	}
}

func (a *app) scheduledListPane(c *ui.Context) {
	st, backend := &a.settings.catalog, &a.settings.backend
	mailPaneHeader(c, tr("Scheduled tasks"))
	ui.Column(c).FillWidth().Padding(10, 12, 0, 12).Gap(8).Children(func() {
		settingsSearchInput(c, "scheduled-search", tr("Search tasks"), &st.schedQuery)
		c.Key("scheduled-filter")
		index := schedFilterIndex(st.schedFilter)
		themeSegmented(c, &index, schedFilterOptions())
		selected := "all"
		if index == 1 {
			selected = "active"
		} else if index == 2 {
			selected = "paused"
		}
		if selected != st.schedFilter && !(st.schedFilter == "" && selected == "all") {
			if selected == "all" {
				st.schedFilter = ""
			} else {
				st.schedFilter = selected
			}
		}
	})
	settingsOperationStatus(c, backend.sched, "")
	visible, _ := a.schedVisible(backend.schedTasks, st.schedFilter, st.schedQuery)
	if backend.sched.loaded && len(visible) == 0 {
		if len(backend.schedTasks) == 0 {
			mailStatusRow(c, tr("No scheduled tasks"))
		} else {
			mailStatusRow(c, tr("No matching tasks"))
		}
	}
	c.Key("settings-scheduled-list")
	ui.List(c, &backend.schedList, len(visible)).FillWidth().Grow(1).MinHeight(0).ItemKey(func(i int) any { return visible[i].ID }).Rows(func(row ui.ListRow) {
		task := visible[row.Index]
		rowCtx := row.Context
		ui.Column(rowCtx).FillWidth().Padding(2, 8, 2, 8).Children(func() {
			a.mailRow(rowCtx, task.Title, backend.schedSelected == task.ID && !st.schedAdding, func() {
				st.schedAdding = false
				backend.schedEditing, backend.schedDeleteConfirm = "", ""
				backend.schedSelected = task.ID
				a.closeSettingsMenus()
			}, func() { mailTile(rowCtx, "clock", backend.schedSelected == task.ID) }, func() {
				mailEnabledBadge(rowCtx, task.Enabled, choice(task.Enabled, tr("Enabled"), tr("Paused")))
			})
			meta := schedScheduleSummary(task.Schedule) + tr(" · ") + a.schedNextLabel(task)
			ui.Text(rowCtx, meta).FontSize(uiPx(10.5)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...").Margin(0, 9, 0, 9)
		})
	})
	ui.Column(c).FillWidth().Padding(0, 8, 8, 8).Gap(4).Children(func() {
		if mailAddRow(c, tr("New task"), st.schedAdding) {
			a.beginScheduledTask(nil)
		}
		componentTextButton(c, "settings-scheduled-reload", tr("Reload"), false).Disabled(backend.sched.loading || backend.sched.writing).OnClick(a.loadSettingsScheduled)
	})
	caption := tr("Not loaded")
	if backend.sched.loaded {
		caption = fmt.Sprintf("%d tasks", len(backend.schedTasks))
	}
	mailFooter(c, caption, false)
}

func (a *app) schedNextLabel(task scheduledTask) string {
	if !task.Enabled {
		return tr("Not scheduled")
	}
	if task.NextDueAt == nil {
		return tr("Not scheduled")
	}
	return strings.ReplaceAll(tr("Next %{time}"), "%{time}", schedFormatTimestamp(*task.NextDueAt))
}

// scheduledDetail reads the add/edit form, the selected task, or the
// placeholder, like the MCP detail does.
func (a *app) scheduledDetail(c *ui.Context) {
	st := &a.settings.catalog
	if st.schedAdding {
		a.scheduledForm(c)
		return
	}
	task := a.selectedScheduledTask()
	if task == nil {
		mailDetailPlaceholder(c, "clock", tr("Select a task to inspect it"))
		return
	}
	a.scheduledTaskDetail(c, *task)
}

func (a *app) scheduledTaskDetail(c *ui.Context, task scheduledTask) {
	st := &a.settings.backend
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24).Gap(10).Children(func() {
			ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
				mailDetailTile(c, "clock", 17)
				ui.Column(c).Grow(1).MinWidth(0).Children(func() {
					ui.Text(c, task.Title).FontSize(uiPx(15)).FontWeight(500).TextColor(palette.text).Wrap()
					ui.Text(c, schedStateLabel(schedTaskState(task))).FontSize(uiPx(10.5)).TextColor(palette.textTertiary)
				})
				mailEnabledBadge(c, task.Enabled, choice(task.Enabled, tr("Enabled"), tr("Paused")))
			})
			ui.Text(c, task.Prompt).FontSize(uiPx(12.5)).TextColor(palette.textSecondary).Wrap()
			meta := a.schedTargetLabel(task) + tr(" · ") + schedScheduleSummary(task.Schedule) + tr(" · ") + a.schedNextLabel(task)
			ui.Text(c, meta).FontSize(uiPx(11)).TextColor(palette.textTertiary).Wrap()
			if task.Last != nil {
				ui.Text(c, strings.ReplaceAll(tr("Last %{time}"), "%{time}", schedFormatTimestamp(task.Last.At))).FontSize(uiPx(11)).TextColor(palette.textTertiary)
			}
			if reason := schedOutcomeReason(task); reason != "" {
				ui.Text(c, reason).FontSize(uiPx(11)).TextColor(palette.danger).Wrap()
			}
			ui.Row(c).Gap(8).Children(func() {
				componentTextButton(c, "scheduled-edit", tr("Edit task"), false).Disabled(st.sched.writing).OnClick(func() { a.beginScheduledTask(&task) })
				runDisabled := st.sched.writing || st.sched.uncertain || task.Pending != nil
				componentTextButton(c, "scheduled-run", tr("Run now"), false).Disabled(runDisabled).OnClick(func() { a.runScheduledTaskNow(task) })
				componentTextButton(c, "scheduled-toggle", choice(task.Enabled, tr("Pause"), tr("Enable")), false).Disabled(st.sched.writing || st.sched.uncertain).OnClick(func() { a.toggleScheduledTask(task, !task.Enabled) })
				componentTextButton(c, "scheduled-remove", tr("Delete"), false).Disabled(st.sched.writing || st.sched.uncertain).OnClick(func() { st.schedDeleteConfirm = "scheduled:" + task.ID })
			})
			if st.schedDeleteConfirm == "scheduled:"+task.ID {
				ui.Text(c, strings.ReplaceAll(tr("Delete “%{title}”?"), "%{title}", task.Title)).FontSize(uiPx(12)).TextColor(palette.text).Wrap()
				ui.Text(c, tr("Sessions it already started are kept.")).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
				ui.Row(c).Gap(8).Children(func() {
					componentTextButton(c, "scheduled-remove-cancel", tr("Cancel"), false).OnClick(func() { st.schedDeleteConfirm = "" })
					componentTextButton(c, "scheduled-remove-confirm", tr("Delete"), true).Disabled(st.sched.writing).OnClick(func() { a.deleteScheduledTask(task) })
				})
			}
			if sessionID := schedLastSession(task); sessionID != "" {
				if _, ok := a.byID[sessionID]; ok {
					sessionID := sessionID
					componentTextButton(c, "scheduled-open-session", tr("Open session"), false).OnClick(func() { a.openSession(sessionID) })
				}
			}
			settingsOperationStatus(c, st.sched, "")
			ui.Text(c, tr("Scheduled runs stop when Fintwind exits, and missed runs are not replayed.")).FontSize(uiPx(11)).TextColor(palette.textTertiary).Wrap()
		})
	})
}

func (a *app) scheduledForm(c *ui.Context) {
	st, backend := &a.settings.catalog, &a.settings.backend
	editing := backend.schedEditing != ""
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			formTitle(c, choice(editing, tr("Edit task"), tr("New task")))
			settingsFormField(c, "scheduled-form-title", tr("Name"), &st.schedTitle, tr("e.g. Weekly dependency update"))
			a.scheduledPromptField(c)
			ui.Column(c).FillWidth().Gap(6).Children(func() {
				ui.Text(c, tr("Schedule")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
				a.settingsChoice(c, "scheduled-form-kind", tr("Schedule"), st.schedSchedule, formPickerWidth, []settingsOption{
					{label: "At time", value: "atTime"},
					{label: "Interval", value: "every"},
					{label: "Once", value: "once"},
				}, func(option settingsOption) { st.schedSchedule = option.value })
			})
			a.scheduledTimingFields(c)
			ui.Column(c).FillWidth().Gap(6).Children(func() {
				ui.Text(c, tr("Target")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
				a.settingsChoice(c, "scheduled-form-target", tr("Target"), st.schedTarget, formPickerWidth, []settingsOption{
					{label: "New session", value: "new"},
					{label: "Existing session", value: "existing"},
				}, func(option settingsOption) { st.schedTarget = option.value })
			})
			if st.schedTarget == "existing" {
				a.scheduledSessionField(c)
			} else {
				a.scheduledProjectField(c)
				a.scheduledModelField(c)
				ui.Column(c).FillWidth().Gap(6).Children(func() {
					ui.Text(c, tr("Permissions")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
					a.settingsChoice(c, "scheduled-form-access", tr("Permissions"), st.schedAccess, formPickerWidth, schedAccessOptions(), func(option settingsOption) { st.schedAccess = option.value })
				})
			}
			ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
				c.Key("settings-toggle-scheduled-enabled")
				lobechoice.Switch(c, &st.schedEnabled, lobechoice.SwitchProps{Size: theme.Small}).Label(tr("Enabled")).OnChange(func() {})
			})
			settingsOperationStatus(c, backend.sched, "")
			formActions(c, "scheduled-form", tr("Save"), !backend.sched.loaded || backend.sched.writing || backend.sched.uncertain, a.cancelScheduledTask, a.saveScheduledTask)
			ui.Text(c, tr("New sessions run in the project directory.")).FontSize(uiPx(11)).TextColor(palette.textTertiary).Wrap()
		})
	})
}

func (a *app) scheduledPromptField(c *ui.Context) {
	st := &a.settings.catalog
	c.Key("scheduled-form-prompt-field")
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, tr("Prompt")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
		c.Key("scheduled-form-prompt-shell")
		field := ui.Row(c).FillWidth().MinHeight(96).Padding(8, 9).Radius(7).
			Border(1, palette.borderStrong).Background(palette.composer)
		field.Children(func() {
			c.Key("scheduled-form-prompt")
			input := ui.TextAreaBase(c, &st.schedPrompt).Label(tr("Prompt")).
				Placeholder(tr("What the session should do")).
				FillWidth().MinWidth(0).FontSize(uiPx(12.5)).TextColor(palette.text).Lines(4, 10)
			if input.Focused() {
				field.Border(1, palette.accent)
			}
		})
	})
}

func (a *app) scheduledTimingFields(c *ui.Context) {
	st := &a.settings.catalog
	switch st.schedSchedule {
	case "every":
		ui.Column(c).FillWidth().Gap(6).Children(func() {
			settingsTextInput(c, "scheduled-form-interval", tr("Interval"), &st.schedInterval, 0, "60")
			ui.Text(c, tr("minutes")).FontSize(uiPx(11)).TextColor(palette.textSecondary)
		})
	case "once":
		settingsFormField(c, "scheduled-form-date", tr("Date"), &st.schedDate, tr("YYYY-MM-DD"))
		settingsFormField(c, "scheduled-form-time", tr("Time"), &st.schedTime, "09:00")
	default:
		settingsFormField(c, "scheduled-form-time", tr("Time"), &st.schedTime, "09:00")
		c.Key("scheduled-form-weekdays")
		ui.Column(c).FillWidth().Gap(6).Children(func() {
			ui.Text(c, tr("Days")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
			ui.Row(c).FillWidth().Gap(6).Children(func() {
				for _, day := range schedWeekdayOrder {
					day := day
					selected := st.schedWeekdays&(1<<day.bit) != 0
					c.Key("scheduled-weekday-" + day.label)
					chip := ui.ButtonBase(c).Label(tr(day.label)).Height(26).Padding(0, 10).
						Radius(7).AlignItems(ui.Center)
					if selected {
						chip.Background(ui.RGBA(palette.accent.R, palette.accent.G, palette.accent.B, 0.16))
					} else {
						chip.Border(1, palette.border)
					}
					chip.Children(func() {
						color := palette.textSecondary
						if selected {
							color = palette.text
						}
						ui.Text(c, tr(day.label)).FontSize(uiPx(11.5)).TextColor(color)
					})
					if chip.Clicked() {
						if selected {
							st.schedWeekdays &^= 1 << day.bit
						} else {
							st.schedWeekdays |= 1 << day.bit
						}
					}
				}
			})
		})
	}
}

// scheduledProjectField is the New-session project picker over every known
// project, reusing the settingsChoice dropdown.
func (a *app) scheduledProjectField(c *ui.Context) {
	st := &a.settings.catalog
	options := make([]settingsOption, 0, len(a.projects))
	for _, project := range a.projects {
		name := project.Name
		if name == "" {
			name = project.Path
		}
		options = append(options, settingsOption{label: name, value: project.ID, raw: project.ID})
	}
	current := st.schedProject
	if current == "" && len(options) > 0 {
		current = options[0].value
		st.schedProject = current
	}
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, tr("Project")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
		a.settingsChoice(c, "scheduled-form-project", tr("Select a project"), current, formPickerWidth, options, func(option settingsOption) {
			st.schedProject = option.value
		})
	})
}

func (a *app) scheduledModelField(c *ui.Context) {
	st := &a.settings.catalog
	settingsFormField(c, "scheduled-form-model", tr("Model"), &st.schedModel, tr("provider/model"))
	if a.modelProbe == nil || len(a.modelProbe.Models) == 0 {
		ui.Text(c, tr("No available models yet.")).FontSize(uiPx(11)).TextColor(palette.textTertiary).Wrap()
		return
	}
	options := make([]settingsOption, 0, len(a.modelProbe.Models))
	for _, model := range a.modelProbe.Models {
		label := model.Name
		if label == "" {
			label = model.ID
		}
		options = append(options, settingsOption{label: label + " — " + model.ID, value: model.ID, raw: model.ID})
	}
	current := st.schedModel
	found := false
	for _, option := range options {
		if option.value == current {
			found = true
			break
		}
	}
	if !found && current != "" {
		options = append([]settingsOption{{label: current, value: current, raw: current}}, options...)
	}
	a.settingsSearchChoice(c, "scheduled-form-model-pick", tr("Model"), current, formPickerWidth, options, func(option settingsOption) {
		st.schedModel = option.value
	})
}

func (a *app) scheduledSessionField(c *ui.Context) {
	st := &a.settings.catalog
	options := make([]settingsOption, 0, len(a.sessions))
	for _, session := range a.sessions {
		title := session.DisplayTitle()
		if title == "" {
			title = session.ID
		}
		project := ""
		for _, p := range a.projects {
			if p.ID == session.ProjectID && p.Name != "" {
				project = p.Name
				break
			}
		}
		label := title
		if project != "" {
			label = title + " · " + project
		}
		options = append(options, settingsOption{label: label, value: session.ID, raw: session.ID})
	}
	current := st.schedSession
	found := current == ""
	for _, option := range options {
		if option.value == current {
			found = true
			break
		}
	}
	if !found {
		current = ""
		st.schedSession = ""
	}
	if len(options) == 0 {
		mailStatusRow(c, tr("No sessions yet"))
		return
	}
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, tr("Session")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
		a.settingsSearchChoice(c, "scheduled-form-session", tr("Select a session"), current, formPickerWidth, options, func(option settingsOption) {
			st.schedSession = option.value
		})
	})
}

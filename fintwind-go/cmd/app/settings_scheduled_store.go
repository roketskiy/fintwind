package main

import (
	"context"
	"encoding/json"
	"errors"
	"strconv"
	"time"
)

// openScheduledTasks enters the standalone scheduled-tasks workspace, the
// MyGo counterpart of GPUI's WorkspaceMode::ScheduledTasks rail entry.
func (a *app) openScheduledTasks() {
	a.mode = modeScheduled
	a.loadSettingsScheduled()
}

func (a *app) loadSettingsScheduled() {
	a.syncSettingsSource()
	if a.client == nil {
		return
	}
	st := &a.settings.backend
	if st.sched.loading || st.sched.writing {
		return
	}
	st.sched.loading, st.sched.err = true, ""
	source, generation := st.source, st.generation
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 45*time.Second)
		defer cancel()
		var body struct {
			Tasks []scheduledTask `json:"tasks"`
		}
		err := settingsRequest(ctx, source, settingsCommand("listScheduledTasks", source), "scheduledTasks", &body)
		if err == nil && body.Tasks == nil {
			err = errors.New("Task list is missing from the response")
		}
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.sched.loading = false
			if err != nil {
				st.sched.err = tr("Could not load scheduled tasks. ") + err.Error()
				return
			}
			schedSortTasks(body.Tasks)
			st.schedTasks = body.Tasks
			st.sched.loaded, st.sched.uncertain = true, false
			a.keepScheduledSelection()
		})
	}()
}

// keepScheduledSelection drops a selection that no longer exists, the way
// the mail pages fall back to their placeholder.
func (a *app) keepScheduledSelection() {
	st := &a.settings.backend
	if st.schedSelected == "" {
		return
	}
	for i := range st.schedTasks {
		if st.schedTasks[i].ID == st.schedSelected {
			return
		}
	}
	st.schedSelected = ""
}

func (a *app) selectedScheduledTask() *scheduledTask {
	st := &a.settings.backend
	for i := range st.schedTasks {
		if st.schedTasks[i].ID == st.schedSelected {
			return &st.schedTasks[i]
		}
	}
	return nil
}

func (a *app) beginScheduledTask(task *scheduledTask) {
	st, form := &a.settings.backend, &a.settings.catalog
	st.schedDraftGeneration++
	st.schedEditing, st.schedDeleteConfirm, st.sched.err = "", "", ""
	if task == nil {
		form.schedAdding = true
		form.schedTitle, form.schedPrompt = "", ""
		form.schedSchedule = "atTime"
		form.schedTime = "09:00"
		form.schedWeekdays = 0
		form.schedDate = time.Now().In(time.Local).Format("2006-01-02")
		form.schedInterval = "60"
		form.schedTarget = "new"
		form.schedSession, form.schedModel, form.schedAccess = "", "", "ask"
		form.schedEnabled = true
		if len(a.projects) > 0 {
			form.schedProject = a.projects[0].ID
		} else {
			form.schedProject = ""
		}
		if a.detail != nil && a.detail.Model != nil {
			form.schedModel = *a.detail.Model
		}
	} else {
		form.schedAdding = true
		st.schedEditing = task.ID
		st.schedSelected = task.ID
		form.schedTitle, form.schedPrompt, form.schedEnabled = task.Title, task.Prompt, task.Enabled
		switch task.Schedule.Kind {
		case "every":
			form.schedSchedule = "every"
			minutes := task.Schedule.IntervalSecs / 60
			if minutes < 1 {
				minutes = 1
			}
			form.schedInterval = strconv.FormatUint(uint64(minutes), 10)
		case "once":
			form.schedSchedule = "once"
			moment := time.Unix(int64(task.Schedule.OnceAt), 0).In(time.Local)
			form.schedDate = moment.Format("2006-01-02")
			form.schedTime = moment.Format("15:04")
			form.schedInterval = "60"
		default:
			form.schedSchedule = "atTime"
			form.schedTime = schedFormatClock(task.Schedule.AtMinutes)
			form.schedWeekdays = task.Schedule.Weekdays
			form.schedDate = time.Now().In(time.Local).Format("2006-01-02")
			form.schedInterval = "60"
		}
		switch task.Target.Kind {
		case "existingSession":
			form.schedTarget = "existing"
			form.schedSession = task.Target.SessionID
			form.schedProject = ""
			form.schedModel, form.schedAccess = "", "ask"
		default:
			form.schedTarget = "new"
			form.schedProject = task.Target.ProjectID
			form.schedModel = task.Target.Launch.Model
			form.schedAccess = schedAccessMode(task.Target.Launch.RuntimeMode)
			form.schedSession = ""
		}
	}
	a.closeSettingsMenus()
}

func (a *app) cancelScheduledTask() {
	a.settings.backend.schedDraftGeneration++
	st := &a.settings.catalog
	st.schedAdding = false
	a.settings.backend.schedEditing, a.settings.backend.schedDeleteConfirm = "", ""
	a.closeSettingsMenus()
}

func (a *app) saveScheduledTask() {
	form, st := &a.settings.catalog, &a.settings.backend
	if st.sched.writing || st.sched.uncertain || !st.sched.loaded {
		return
	}
	var editing *scheduledTask
	for i := range st.schedTasks {
		if st.schedTasks[i].ID == st.schedEditing {
			editing = &st.schedTasks[i]
			break
		}
	}
	if st.schedEditing != "" && editing == nil {
		st.sched.err = tr("The task changed elsewhere. Reload before saving.")
		st.sched.uncertain = true
		return
	}
	var timezone *string
	var workspace json.RawMessage
	if editing != nil {
		timezone = editing.Schedule.Timezone
		workspace = editing.Target.Workspace
	}
	draft, message := schedDraft(form.schedTitle, form.schedPrompt, form.schedSchedule, form.schedTime, form.schedWeekdays, form.schedDate, form.schedInterval, form.schedTarget, form.schedProject, form.schedSession, form.schedModel, form.schedAccess, form.schedEnabled, timezone, workspace)
	if message != "" {
		st.sched.err = message
		return
	}
	source, generation := st.source, st.generation
	draftGeneration, wasAdding := st.schedDraftGeneration, form.schedAdding
	command := settingsCommand("saveScheduledTask", source)
	if editing == nil {
		command["edit"] = map[string]any{"kind": "create", "draft": draft}
	} else {
		command["edit"] = map[string]any{"kind": "update", "id": editing.ID, "expectedRevision": editing.Revision, "draft": draft}
	}
	st.sched.writing, st.sched.err = true, ""
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 45*time.Second)
		defer cancel()
		var body struct {
			Task scheduledTask `json:"task"`
		}
		err := settingsRequest(ctx, source, command, "scheduledTaskSaved", &body)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.sched.writing = false
			if err != nil {
				st.sched.err = tr("Save failed. Reload before retrying. ") + err.Error()
				st.sched.uncertain = true
				return
			}
			found := false
			for i := range st.schedTasks {
				if st.schedTasks[i].ID == body.Task.ID {
					st.schedTasks[i], found = body.Task, true
					break
				}
			}
			if !found {
				st.schedTasks = append(st.schedTasks, body.Task)
			}
			schedSortTasks(st.schedTasks)
			st.schedSelected = body.Task.ID
			if wasAdding && st.schedDraftGeneration == draftGeneration {
				a.settings.catalog.schedAdding = false
				st.schedEditing = ""
			}
		})
	}()
}

func (a *app) toggleScheduledTask(task scheduledTask, enabled bool) {
	st := &a.settings.backend
	if st.sched.writing || st.sched.uncertain || !st.sched.loaded {
		return
	}
	draft := map[string]any{
		"title": task.Title, "prompt": task.Prompt, "enabled": enabled,
		"schedule": task.Schedule, "target": task.Target,
	}
	source, generation := st.source, st.generation
	st.sched.writing, st.sched.err = true, ""
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 45*time.Second)
		defer cancel()
		command := settingsCommand("saveScheduledTask", source)
		command["edit"] = map[string]any{"kind": "update", "id": task.ID, "expectedRevision": task.Revision, "draft": draft}
		var body struct {
			Task scheduledTask `json:"task"`
		}
		err := settingsRequest(ctx, source, command, "scheduledTaskSaved", &body)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.sched.writing = false
			if err != nil {
				st.sched.err = tr("Save failed. Reload before retrying. ") + err.Error()
				st.sched.uncertain = true
				return
			}
			for i := range st.schedTasks {
				if st.schedTasks[i].ID == body.Task.ID {
					st.schedTasks[i] = body.Task
					break
				}
			}
			schedSortTasks(st.schedTasks)
		})
	}()
}

func (a *app) runScheduledTaskNow(task scheduledTask) {
	st := &a.settings.backend
	if st.sched.writing || st.sched.uncertain || !st.sched.loaded || task.Pending != nil {
		return
	}
	source, generation := st.source, st.generation
	st.sched.writing, st.sched.err = true, ""
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 45*time.Second)
		defer cancel()
		command := settingsCommand("runScheduledTaskNow", source)
		command["id"] = task.ID
		var body struct {
			Task scheduledTask `json:"task"`
		}
		err := settingsRequest(ctx, source, command, "scheduledTaskSaved", &body)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.sched.writing = false
			if err != nil {
				st.sched.err = tr("Could not start the run. ") + err.Error()
				st.sched.uncertain = true
				return
			}
			for i := range st.schedTasks {
				if st.schedTasks[i].ID == body.Task.ID {
					st.schedTasks[i] = body.Task
					break
				}
			}
			schedSortTasks(st.schedTasks)
		})
	}()
}

func (a *app) deleteScheduledTask(task scheduledTask) {
	st := &a.settings.backend
	if st.sched.writing || st.sched.uncertain || !st.sched.loaded {
		return
	}
	source, generation := st.source, st.generation
	st.sched.writing, st.sched.err = true, ""
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 45*time.Second)
		defer cancel()
		command := settingsCommand("deleteScheduledTask", source)
		command["id"], command["expectedRevision"] = task.ID, task.Revision
		err := settingsRequest(ctx, source, command, "ack", nil)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.sched.writing = false
			if err != nil {
				st.sched.err = tr("Delete failed. Reload before retrying. ") + err.Error()
				st.sched.uncertain = true
				return
			}
			kept := st.schedTasks[:0]
			for _, item := range st.schedTasks {
				if item.ID != task.ID {
					kept = append(kept, item)
				}
			}
			st.schedTasks = kept
			if st.schedSelected == task.ID {
				st.schedSelected = ""
			}
			st.schedDeleteConfirm = ""
			a.settings.catalog.schedAdding = false
			st.schedEditing = ""
		})
	}()
}

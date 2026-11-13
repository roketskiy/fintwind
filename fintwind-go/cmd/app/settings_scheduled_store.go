package main

import (
	"context"
	"encoding/json"
	"errors"
	"strconv"
	"strings"
	"time"
)

// openScheduledTasks enters the standalone scheduled-tasks workspace, the
// MyGo counterpart of GPUI's WorkspaceMode::ScheduledTasks rail entry.
func (a *app) openScheduledTasks() {
	a.mode = modeScheduled
	a.settings.backend.schedBoard.ready = false
	a.loadSettingsScheduled()
}

func (a *app) loadSettingsScheduled() {
	a.syncSettingsSource()
	if a.client == nil {
		return
	}
	st := &a.settings.backend
	if st.sched.loading || st.sched.writing {
		st.schedBoard.refreshPending = true
		return
	}
	st.schedBoard.refreshPending = false
	st.sched.loading, st.sched.err = true, ""
	source, generation := st.source, st.generation
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 15*time.Second)
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
			defer a.scheduledRefreshPending()
			if err != nil {
				st.sched.err = tr("Could not load scheduled tasks. ") + err.Error()
				return
			}
			schedSortTasks(body.Tasks)
			st.schedTasks = body.Tasks
			st.schedBoard.ready = false
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
	if st.sched.writing || st.sched.uncertain {
		return
	}
	st.schedBoard.editingTask = nil
	if task != nil {
		original := *task
		st.schedBoard.editingTask = &original
	}
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
	st.schedBoard.open, st.schedBoard.discard = true, false
	st.schedBoard.original = a.scheduledFormSnapshot()
}

func (a *app) cancelScheduledTask() {
	st := &a.settings.backend
	if st.sched.writing {
		// Closing the modal does not cancel the daemon request. Its result is
		// still applied when it arrives, but the request must not trap the user
		// behind a modal for the whole timeout window.
		st.schedBoard.open = false
		return
	}
	if a.settings.catalog.schedAdding && a.scheduledFormSnapshot() != st.schedBoard.original {
		st.schedBoard.open, st.schedBoard.discard = true, true
		return
	}
	a.finishScheduledEditor()
}

func (a *app) finishScheduledEditor() {
	a.settings.backend.schedDraftGeneration++
	st := &a.settings.catalog
	st.schedAdding = false
	a.settings.backend.schedEditing, a.settings.backend.schedDeleteConfirm = "", ""
	a.settings.backend.schedBoard.open, a.settings.backend.schedBoard.discard = false, false
	a.settings.backend.schedBoard.editingTask = nil
	a.settings.backend.sched.err = ""
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
	if original := st.schedBoard.editingTask; editing != nil && original != nil && original.ID == editing.ID {
		editing = original
	}
	var timezone *string
	var workspace json.RawMessage
	if editing != nil {
		timezone = editing.Schedule.Timezone
		if editing.Target.ProjectID == form.schedProject {
			workspace = editing.Target.Workspace
		}
	}
	draft, message := schedDraft(form.schedTitle, form.schedPrompt, form.schedSchedule, form.schedTime, form.schedWeekdays, form.schedDate, form.schedInterval, form.schedTarget, form.schedProject, form.schedSession, form.schedModel, form.schedAccess, form.schedEnabled, timezone, workspace)
	if message != "" {
		st.sched.err = message
		return
	}
	// Preserve the launch options that the GPUI editor retains when changing
	// only the model or access mode on an existing new-session task.
	if editing != nil && editing.Target.Kind == "newSession" && form.schedTarget != "existing" {
		launch := editing.Target.Launch
		launch.Model, launch.RuntimeMode = strings.TrimSpace(form.schedModel), schedAccessMode(form.schedAccess)
		if launch.Model != editing.Target.Launch.Model {
			launch.ReasoningEffort, launch.ServiceTier, launch.ContextWindow = nil, nil, nil
		}
		draft["target"].(map[string]any)["launch"] = launch
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
	st.schedBoard.saveStarted = time.Now()
	started := st.schedBoard.saveStarted
	// A slow acknowledgement must not keep the rest of the app behind a modal.
	// This does not cancel or retry the write: its outcome is still unknown.
	releaseModal := time.AfterFunc(time.Second, func() {
		a.post(func() {
			if a.settingsSourceMatches(source, generation) && a.settings.backend.sched.writing && a.settings.backend.schedBoard.saveStarted == started {
				a.settings.backend.schedBoard.open = false
			}
		})
	})
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 45*time.Second)
		defer cancel()
		defer releaseModal.Stop()
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
			defer a.scheduledRefreshPending()
			if err != nil {
				st.sched.err = scheduledSaveError(err)
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
			st.schedBoard.ready = false
			if wasAdding && st.schedDraftGeneration == draftGeneration {
				a.settings.catalog.schedAdding = false
				st.schedEditing = ""
			}
		})
	}()
}

func scheduledSaveError(err error) string {
	if errors.Is(err, context.DeadlineExceeded) {
		return tr("保存超时。结果可能仍在 daemon 中处理，请先重新加载，不要重复保存。")
	}
	return tr("保存失败。请重新加载后再试。")
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
			defer a.scheduledRefreshPending()
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
			st.schedBoard.ready = false
		})
	}()
}

func (a *app) runScheduledTaskNow(task scheduledTask) {
	st := &a.settings.backend
	if st.sched.writing || st.sched.uncertain || !st.sched.loaded || a.scheduledTaskRunning(task) {
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
			defer a.scheduledRefreshPending()
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
			st.schedBoard.ready = false
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
			defer a.scheduledRefreshPending()
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
			st.schedBoard.ready = false
			if st.schedSelected == task.ID {
				st.schedSelected = ""
			}
			st.schedDeleteConfirm = ""
			a.settings.catalog.schedAdding = false
			st.schedEditing = ""
			st.schedBoard.open = false
		})
	}()
}

// A change notice can arrive while the previous read or mutation is in flight.
// Coalesce it into one read after completion, rather than losing the notice.
func (a *app) scheduledRefreshPending() {
	if a.settings.backend.schedBoard.refreshPending && a.mode == modeScheduled {
		a.loadSettingsScheduled()
	}
}

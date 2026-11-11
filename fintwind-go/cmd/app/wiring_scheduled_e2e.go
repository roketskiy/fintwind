//go:build wiringe2e

package main

import (
	"context"
	"encoding/json"
	"errors"

	"fintwind-go/internal/daemon"

	"github.com/egoist/mygo/ui"
)

var errScheduledMissing = errors.New("scheduled task is missing from the fixture")

func wiringScheduledFixtureTask() map[string]any {
	return map[string]any{
		"id": "11111111-1111-4111-8111-111111111111", "revision": uint64(3),
		"title": "Fixture nightly", "prompt": "Summarize overnight activity", "enabled": true,
		"schedule": map[string]any{"kind": "atTime", "at": uint64(540), "weekdays": uint64(127)},
		"target": map[string]any{"kind": "newSession", "projectId": "fixture-project",
			"launch":    map[string]any{"model": "fixture/model", "runtimeMode": "ask"},
			"workspace": map[string]any{"kind": "local"}},
		"nextDueAt": uint64(4102444800), "createdAt": uint64(1700000000),
		"updatedAt": uint64(1700000000), "runCount": uint64(0),
	}
}

func wiringScheduledTasks(ctx context.Context, r *wiringReport, client *daemon.Client, root string, f *wiringFixture) {
	tasks := []map[string]any{wiringScheduledFixtureTask()}
	f.extended = func(envelope wiringEnvelope) (any, error, bool, bool) {
		switch envelope.Command.Type {
		case "listScheduledTasks":
			return map[string]any{"type": "scheduledTasks", "tasks": tasks}, nil, false, true
		case "saveScheduledTask":
			var command struct {
				Edit struct {
					Kind             string         `json:"kind"`
					ID               string         `json:"id"`
					ExpectedRevision uint64         `json:"expectedRevision"`
					Draft            map[string]any `json:"draft"`
				} `json:"edit"`
			}
			if err := json.Unmarshal(envelope.Command.Raw, &command); err != nil {
				return nil, err, false, true
			}
			id, revision := "22222222-2222-4222-8222-222222222222", uint64(1)
			if command.Edit.Kind == "update" {
				id, revision = command.Edit.ID, command.Edit.ExpectedRevision+1
			}
			task := map[string]any{
				"id": id, "revision": revision,
				"title": command.Edit.Draft["title"], "prompt": command.Edit.Draft["prompt"],
				"enabled": command.Edit.Draft["enabled"], "schedule": command.Edit.Draft["schedule"],
				"target": command.Edit.Draft["target"], "nextDueAt": uint64(4102444800),
				"createdAt": uint64(1700000000), "updatedAt": uint64(1700003600), "runCount": uint64(0),
			}
			replaced := false
			for i := range tasks {
				if tasks[i]["id"] == id {
					tasks[i], replaced = task, true
				}
			}
			if !replaced {
				tasks = append(tasks, task)
			}
			return map[string]any{"type": "scheduledTaskSaved", "task": task}, nil, false, true
		case "runScheduledTaskNow":
			var command struct {
				ID string `json:"id"`
			}
			if err := json.Unmarshal(envelope.Command.Raw, &command); err != nil {
				return nil, err, false, true
			}
			for i := range tasks {
				if tasks[i]["id"] == command.ID {
					ran := map[string]any{}
					for key, value := range tasks[i] {
						ran[key] = value
					}
					ran["pending"] = map[string]any{"id": "p1", "dueAt": uint64(4102444800), "preparing": true}
					tasks[i] = ran
					return map[string]any{"type": "scheduledTaskSaved", "task": ran}, nil, false, true
				}
			}
			return nil, errScheduledMissing, false, true
		case "deleteScheduledTask":
			var command struct {
				ID string `json:"id"`
			}
			if err := json.Unmarshal(envelope.Command.Raw, &command); err != nil {
				return nil, err, false, true
			}
			kept := tasks[:0]
			for _, task := range tasks {
				if task["id"] != command.ID {
					kept = append(kept, task)
				}
			}
			tasks = kept
			return map[string]any{"type": "ack"}, nil, false, true
		}
		return nil, nil, false, false
	}
	a := wiringApp(ctx, client, root)
	a.settingsLoaded = true
	a.modelProbe = &providerProbe{Installed: true, Models: []providerModel{{ID: "fixture/model", Name: "Fixture model"}}}
	queue, release := installHeadlessDispatcher(a)
	defer release()

	a.openScheduledTasks()
	if !wiringApplyUntil(ctx, queue, func() bool { return a.settings.backend.sched.loaded }) {
		r.check("scheduled task list loads", true, false)
		return
	}
	r.check("scheduled task list requests catalog once", 1, f.count("listScheduledTasks"))
	r.check("scheduled task list holds fixture task", 1, len(a.settings.backend.schedTasks))

	tt := ui.NewTester(a.scheduledSettings, 1100, 1000)
	r.check("scheduled roster shows task title", true, tt.HasText("Fixture nightly"))
	r.check("scheduled roster summarizes wall-clock repeat", true, tt.HasText("Every day at 09:00"))
	r.check("scheduled roster shows next run", true, tt.HasText("Next "))

	a.settings.catalog.schedQuery = "zzz-no-match"
	tt.Frame()
	r.check("scheduled search with no hit explains itself", true, tt.HasText("No matching tasks"))
	a.settings.catalog.schedQuery = ""
	tt.Frame()

	wiringClick(r, tt, "New task")
	tt.Frame()
	r.check("scheduled form opens with target section", true, tt.HasText("Target") && tt.HasText("Permissions"))
	wiringClick(r, tt, "Save")
	tt.Frame()
	r.check("scheduled form requires a name first", true, tt.HasText("Enter a name"))
	r.check("scheduled validation emits no request", 0, f.count("saveScheduledTask"))

	wiringType(r, tt, "Name", "E2E task")
	wiringType(r, tt, "Prompt", "E2E prompt")
	wiringType(r, tt, "Model", "fixture/model")
	tt.Frame()
	wiringClick(r, tt, "Save")
	tt.Frame()
	r.check("scheduled form requires at least one weekday", true, tt.HasText("Select at least one day"))
	wiringClick(r, tt, "Mon")
	wiringClick(r, tt, "Save")
	if !wiringApplyUntil(ctx, queue, func() bool {
		return !a.settings.catalog.schedAdding && len(a.settings.backend.schedTasks) == 2
	}) {
		r.check("scheduled create applies and closes the form", true, false)
		return
	}
	tt.Frame()
	r.check("scheduled create selects the new task", "E2E task", a.selectedScheduledTask().Title)
	r.check("scheduled create emits one save", 1, f.count("saveScheduledTask"))
	requests, _ := f.snapshot()
	for _, request := range requests {
		if request.Command.Type != "saveScheduledTask" {
			continue
		}
		var command struct {
			Edit struct {
				Kind  string         `json:"kind"`
				Draft map[string]any `json:"draft"`
			} `json:"edit"`
		}
		if err := json.Unmarshal(request.Command.Raw, &command); err != nil {
			r.check("scheduled create wire decodes", "success", err.Error())
			continue
		}
		schedule, _ := command.Edit.Draft["schedule"].(map[string]any)
		target, _ := command.Edit.Draft["target"].(map[string]any)
		launch, _ := target["launch"].(map[string]any)
		r.check("scheduled create kind", "create", command.Edit.Kind)
		r.check("scheduled create wall-clock schedule", []any{"atTime", float64(540), float64(2)}, []any{schedule["kind"], schedule["at"], schedule["weekdays"]})
		r.check("scheduled create new-session target", []any{"newSession", "fixture-project", "fixture/model", "ask"}, []any{target["kind"], target["projectId"], launch["model"], launch["runtimeMode"]})
	}

	wiringClick(r, tt, "Run now")
	if !wiringApplyUntil(ctx, queue, func() bool {
		task := a.selectedScheduledTask()
		return task != nil && task.Pending != nil
	}) {
		r.check("scheduled run marks the task preparing", true, false)
		return
	}
	tt.Frame()
	r.check("scheduled run shows preparing state", true, tt.HasText("Preparing"))
	r.check("scheduled run emits one request", 1, f.count("runScheduledTaskNow"))

	wiringClick(r, tt, "Pause")
	if !wiringApplyUntil(ctx, queue, func() bool {
		task := a.selectedScheduledTask()
		return task != nil && !task.Enabled
	}) {
		r.check("scheduled pause disables the task", true, false)
		return
	}
	r.check("scheduled pause emits an update", 2, f.count("saveScheduledTask"))

	a.settings.catalog.schedFilter = "active"
	a.settings.backend.schedSelected = ""
	tt.Frame()
	r.check("scheduled active filter hides the paused task", false, tt.HasText("E2E task"))
	r.check("scheduled active filter keeps enabled tasks", true, tt.HasText("Fixture nightly"))
	a.settings.catalog.schedFilter = ""
	tt.Frame()

	wiringClick(r, tt, "Fixture nightly")
	tt.Frame()
	wiringClick(r, tt, "Delete")
	tt.Frame()
	r.check("scheduled delete asks for confirmation", true, tt.HasText("Sessions it already started are kept."))
	fixture := scheduledTask{}
	for _, task := range a.settings.backend.schedTasks {
		if task.Title == "Fixture nightly" {
			fixture = task
		}
	}
	a.deleteScheduledTask(fixture)
	if !wiringApplyUntil(ctx, queue, func() bool { return len(a.settings.backend.schedTasks) == 1 }) {
		r.check("scheduled delete removes the task", true, false)
		return
	}
	tt.Frame()
	r.check("scheduled delete emits one request", 1, f.count("deleteScheduledTask"))
	r.check("scheduled delete clears the selection", true, tt.HasText("Select a task to inspect it"))
	r.check("scheduled delete keeps the other task", true, tt.HasText("E2E task"))
}

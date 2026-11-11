//go:build wiringe2e

package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/ELY-Open-Source/CrychicUI/sound"
	"github.com/egoist/mygo/ui"
)

func wiringRun(ctx context.Context, r *wiringReport, round string) {
	r.check("component sounds muted", false, sound.Enabled())
	for _, key := range []string{"HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"} {
		r.check("isolated "+key, filepath.Join(round, "home"), os.Getenv(key))
	}
	root := filepath.Join(round, "repository")
	if err := wiringInitGit(ctx, root, round); err != nil {
		r.check("create isolated Git repository", "success", err.Error())
		return
	}
	f, err := wiringNewFixture(ctx, root)
	if err != nil {
		r.check("loopback fixture", "success", err.Error())
		return
	}
	defer f.close()
	defer f.stop()
	dialCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	client, err := daemon.Dial(dialCtx, f.server.Listener.Addr().String(), "")
	cancel()
	if err != nil {
		r.check("protocol handshake", "success", err.Error())
		return
	}
	defer client.Close()
	r.check("real daemon.Client protocol identity", wiringEnvironment, client.Identity().EnvironmentID)
	a := wiringApp(ctx, client, root)
	wiringReplies(ctx, r, a, f)
	wiringSessionDeleteCancel(r, a, f)
	wiringSourceControl(ctx, r, a, f, round)
	wiringRuntimePreferences(ctx, r, client, root, f)
	_, fixtureErrors := f.snapshot()
	r.check("typed fixture command allowlist", []string{}, fixtureErrors)
	wiringPreferences(ctx, r, round)
	wiringAppliedCallbacks(ctx, r, round)
	wiringHistoryEditing(ctx, r, round)
	wiringAuthCompletion(ctx, r, round)
	wiringProviders(ctx, r, round)
	wiringRuntimeConfig(ctx, r, round)
	wiringLanguage(ctx, r, round)
	wiringSidebarHistory(ctx, r, root)
	wiringSkills(ctx, r, a, f, root)
}

func wiringApp(ctx context.Context, client *daemon.Client, root string) *app {
	SetLanguage("english")
	return &app{client: client, lifetime: ctx, done: make(chan struct{}), lastProject: "fixture-project", selectedID: "fixture-session",
		projects: []model.Project{{ID: "fixture-project", Name: "Fixture project", Path: root, EnvironmentID: wiringEnvironment}},
		sessions: []model.Session{{ID: "fixture-session", Title: "Fixture session", ProjectID: "fixture-project", Provider: "opencode"}},
		runtimes: map[string]string{"fixture-session": "fixture-runtime"},
	}
}

func wiringClick(r *wiringReport, tt *ui.Tester, label string) bool {
	if err := tt.Click(label); err != nil {
		r.check("click "+label, "success", err.Error())
		return false
	}
	return true
}

func wiringType(r *wiringReport, tt *ui.Tester, label, value string) {
	if !wiringRoleClick(r, tt, ui.RoleTextField, label) {
		return
	}
	tt.Key(ui.Ctrl, ui.KeyA)
	tt.Type(value)
	entry, found := tt.AccessByRole(ui.RoleTextField, label)
	r.check("typed text field "+label, value, entry.Value)
	if !found {
		r.check("text field remains available "+label, true, false)
	}
}

func wiringChoice(r *wiringReport, tt *ui.Tester, label string, index int) {
	if !wiringRoleClick(r, tt, ui.RolePopUpButton, label) {
		return
	}
	tt.Key(0, ui.KeyHome)
	for i := 0; i < index; i++ {
		tt.Key(0, ui.KeyDown)
	}
	tt.Key(0, ui.KeyEnter)
}

func wiringRoleClick(r *wiringReport, tt *ui.Tester, role ui.Role, label string) bool {
	entry, found := tt.AccessByRole(role, label)
	if !found || entry.Bounds.W <= 0 || entry.Bounds.H <= 0 {
		r.check("role-located control "+label, true, false)
		return false
	}
	tt.ClickAt(entry.Bounds.X+entry.Bounds.W/2, entry.Bounds.Y+entry.Bounds.H/2)
	return true
}

func wiringWait(ctx context.Context, r *wiringReport, f *wiringFixture, kind string) bool {
	if err := f.await(ctx, kind); err != nil {
		r.check("wire completes "+kind, "success", err.Error())
		return false
	}
	return true
}

func wiringReplies(ctx context.Context, r *wiringReport, a *app, f *wiringFixture) {
	a.detail = &model.Session{ID: "fixture-session", ProjectID: "fixture-project"}
	a.pendingPermission = &pendingPermission{RequestID: "permission-1", Title: "Fixture permission", Detail: "No command executes", Options: []permissionOption{{ID: "allow-once", Label: "Allow once", Allow: true}}}
	tt := ui.NewTester(a.permissionCard, 1000, 600)
	tt.Key(0, ui.KeyTab)
	tt.Key(0, ui.KeyEnter)
	wiringWait(ctx, r, f, "respond")
	r.check("permission retained pending acknowledgment apply", true, a.pendingPermission != nil && a.pendingPermission.Response.Phase == "sending")
	entry, ok := tt.Access("Allow once")
	r.check("permission inflight button disabled", true, ok && entry.Disabled)
	a.respondPermission("allow-once")
	r.check("permission repeat blocked", 1, f.count("respond"))
	a.pendingPermission = nil
	a.pendingUserInput = &userInputState{RequestID: "question-1", Questions: []userInputQuestion{{ID: "question-a", Header: "Fixture", Question: "Select fixture answer", Options: []userInputOption{{Label: "Choice A"}}}}, Selections: map[int][]string{}, Custom: []string{""}}
	tt = ui.NewTester(a.userInputCard, 1000, 700)
	wiringClick(r, tt, "Choice A")
	wiringType(r, tt, "Another answer", "Custom answer")
	wiringClick(r, tt, "Submit")
	wiringWait(ctx, r, f, "respondUserInput")
	a.submitUserInput()
	r.check("question repeat blocked", 1, f.count("respondUserInput"))
	r.check("question remains pending before callback", true, a.pendingUserInput != nil && a.pendingUserInput.Response.Phase == "sending")
	requests, _ := f.snapshot()
	for _, request := range requests {
		switch request.Command.Type {
		case "respond":
			r.check("permission wire identity and choice", []string{"fixture-session", "fixture-runtime", "permission-1", "allow-once"}, []string{request.SessionID, request.RuntimeID, request.Command.RequestID, request.Command.OptionID})
		case "respondUserInput":
			r.check("question wire literal answers", `[{"answers":["Choice A","Custom answer"],"questionId":"question-a"}]`, wiringJSON(request.Command.Answers))
		}
	}
	a.detail = nil
	a.pendingUserInput = nil
}

func wiringJSON(value any) string { data, _ := json.Marshal(value); return string(data) }

func wiringSessionDeleteCancel(r *wiringReport, a *app, f *wiringFixture) {
	before, _ := f.snapshot()
	a.removeSession("fixture-session")
	tt := ui.NewTester(func(c *ui.Context) {
		ui.Column(c).Fill().Children(func() { a.sessionDeleteConfirmLayer(c) })
	}, 1000, 700)
	r.check("session delete opens named confirmation", true, tt.HasText("Delete task?") && tt.HasText("Fixture session"))
	r.check("session delete does not request before confirmation", 0, f.count("deleteSession"))
	wiringClick(r, tt, "Cancel")
	r.check("cancel closes session deletion confirmation", true, a.sessionActions.confirmation == nil)
	r.check("cancel retains session", "Fixture session", a.summarySession("fixture-session").DisplayTitle())
	a.removeSession("fixture-session")
	tt.Frame()
	original := a.projects[0].Path
	a.projects[0].Path = original + "-changed"
	wiringClick(r, tt, "Delete task")
	r.check("changed workspace refuses prior deletion confirmation", "The workspace changed. Select Delete again.", a.status)
	a.projects[0].Path = original
	after, _ := f.snapshot()
	r.check("cancel and stale confirmation emit no requests", 0, len(after)-len(before))
}

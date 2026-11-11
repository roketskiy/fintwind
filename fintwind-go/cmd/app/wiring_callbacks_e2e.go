//go:build wiringe2e

package main

import (
	"context"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

func wiringApplyUntil(ctx context.Context, queue chan func(), ready func() bool) bool {
	limit, cancel := context.WithTimeout(ctx, 4*time.Second)
	defer cancel()
	for !ready() {
		select {
		case update := <-queue:
			update()
		case <-limit.Done():
			return false
		}
	}
	return true
}

func wiringAppliedCallbacks(ctx context.Context, r *wiringReport, round string) {
	f, err := wiringNewFixture(ctx, round)
	if err != nil {
		r.check("callback fixture startup", "success", err.Error())
		return
	}
	defer f.close()
	defer f.stop()
	client, err := daemon.Dial(ctx, f.server.Listener.Addr().String(), "")
	if err != nil {
		r.check("callback fixture handshake", "success", err.Error())
		return
	}
	defer client.Close()
	a := wiringApp(ctx, client, round)
	a.detail = &model.Session{ID: "fixture-session", ProjectID: "fixture-project"}
	a.live = map[string]*sessionState{}
	a.sessionAttention = map[string]string{"fixture-session": "permission"}
	queue, release := installHeadlessDispatcher(a)
	defer release()
	pending := &pendingPermission{RequestID: "callback-permission", Title: "Fixture permission", Options: []permissionOption{{ID: "allow-once", Label: "Allow once", Allow: true}}}
	a.pendingPermission = pending
	a.stateForSession(a.detail).permission = pending
	tt := ui.NewTester(a.permissionCard, 1000, 600)
	wiringClick(r, tt, "Allow once")
	r.check("permission kept before actual acknowledgement callback", true, a.pendingPermission == pending)
	r.check("permission acknowledgement callback is applied", true, wiringApplyUntil(ctx, queue, func() bool { return pending.Response.Phase != "sending" }))
	tt.Frame()
	r.check("ack clears exact pending card and live state", true, a.pendingPermission == nil && a.live["fixture-session"].permission == nil)
	r.check("ack clears session attention", 0, len(a.sessionAttention))
	old := &pendingPermission{RequestID: "older-permission", Options: pending.Options}
	a.pendingPermission = old
	a.respondPermission("allow-once")
	newer := &pendingPermission{RequestID: "newer-permission", Options: pending.Options}
	a.pendingPermission = newer
	a.live["fixture-session"].permission = newer
	wiringApplyUntil(ctx, queue, func() bool { return old.Response.Phase != "sending" })
	r.check("late acknowledgement cannot clear a new request", true, a.pendingPermission == newer && a.live["fixture-session"].permission == newer)
	a.pendingPermission = nil
	question := &userInputState{RequestID: "callback-question", Questions: []userInputQuestion{{ID: "question-a", Header: "Fixture", Question: "Fixture answer", Options: []userInputOption{{Label: "Choice A"}}}}, Selections: map[int][]string{0: {"Choice A"}}, Custom: []string{""}}
	a.pendingUserInput = question
	a.live["fixture-session"].question = question
	a.submitUserInput()
	r.check("question acknowledged before clear", true, wiringApplyUntil(ctx, queue, func() bool { return question.Response.Phase != "sending" }))
	r.check("question ack removes exact pending question", true, a.pendingUserInput == nil && a.live["fixture-session"].question == nil)
	a.pendingUserInput = nil
	a.saveDaemonSetting("auto_compact_enabled", false)
	r.check("atomic settings reply applies to UI", true, wiringApplyUntil(ctx, queue, func() bool { return !a.settings.backend.settingsWrite }))
	r.check("confirmed atomic value installed", false, settingsBool(a.settingsRaw, "auto_compact_enabled", true))
}

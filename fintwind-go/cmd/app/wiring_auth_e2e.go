//go:build wiringe2e

package main

import (
	"context"
	"encoding/json"
	"sync"

	"fintwind-go/internal/daemon"

	"github.com/egoist/mygo/ui"
)

func wiringAuthCompletion(ctx context.Context, r *wiringReport, round string) {
	f, err := wiringNewFixture(ctx, round)
	if err != nil {
		r.check("auth fixture", "success", err.Error())
		return
	}
	defer f.close()
	defer f.stop()
	var mu sync.Mutex
	active := false
	var activation, oauth map[string]any
	const roster = `{"type":"integrations","integrations":[{"id":"provider-exact","name":"Fixture provider","connected":true,"keyAccounts":[{"id":"credential/server/42","label":"team-account","active":false}],"oauthMethods":[{"id":"browser","label":"Browser login","form":[{"key":"tenant","title":"Tenant","type":"string","required":true,"when":[{"key":"region","op":"eq","value":"eu"}]},{"key":"region","title":"Region","type":"string","required":true,"options":[{"value":"eu","label":"Europe"},{"value":"us","label":"United States"}]},{"key":"accessToken","title":"Access token","type":"string","required":true},{"key":"serverDefault","hidden":true,"default":"server-only"}]}]}]}`
	f.extended = func(envelope wiringEnvelope) (any, error, bool, bool) {
		var body map[string]any
		_ = json.Unmarshal(envelope.Command.Raw, &body)
		switch envelope.Command.Type {
		case "fetchIntegrations":
			var result map[string]any
			_ = json.Unmarshal([]byte(roster), &result)
			mu.Lock()
			isActive := active
			mu.Unlock()
			integration := result["integrations"].([]any)[0].(map[string]any)
			integration["keyAccounts"].([]any)[0].(map[string]any)["active"] = isActive
			return result, nil, false, true
		case "activateProviderKeyAccount":
			mu.Lock()
			activation, active = body, true
			mu.Unlock()
			return map[string]any{"type": "ack"}, nil, false, true
		case "startProviderOAuth":
			mu.Lock()
			oauth = body
			mu.Unlock()
			return map[string]any{"type": "providerOAuthStarted", "attempt": map[string]any{"attemptId": "fixture-attempt", "url": "https://auth.example.test/confirm", "mode": "auto", "instructions": "Fixture sign-in"}}, nil, false, true
		case "cancelProviderOAuth":
			return map[string]any{"type": "ack"}, nil, false, true
		}
		return nil, nil, false, false
	}
	client, err := daemon.Dial(ctx, f.server.Listener.Addr().String(), "")
	if err != nil {
		r.check("auth fixture handshake", "success", err.Error())
		return
	}
	defer client.Close()
	a := wiringApp(ctx, client, round)
	a.syncSettingsSource()
	queue, release := installHeadlessDispatcher(a)
	defer release()
	tt := ui.NewTester(func(c *ui.Context) { ui.Column(c).Width(650).Children(func() { a.settingsBuiltinIntegrations(c) }) }, 900, 1000)
	wiringClick(r, tt, "Load accounts")
	r.check("auth roster applies from daemon", true, wiringApplyUntil(ctx, queue, func() bool { return a.settings.backend.integration.loaded }))
	tt.Frame()
	wiringClick(r, tt, "Select")
	wiringClick(r, tt, "Use selected account")
	r.check("auth activation waits for acknowledgement", true, a.settings.backend.auth.busy)
	r.check("auth activation refresh applies", true, wiringApplyUntil(ctx, queue, func() bool {
		return len(a.settings.backend.integrations) > 0 && a.settings.backend.integrations[0].KeyAccounts[0].Active
	}))
	mu.Lock()
	command := activation
	mu.Unlock()
	r.check("auth opaque credential ID reaches daemon", "credential/server/42", command["credentialId"])
	r.check("auth exact provider reaches daemon", "provider-exact", command["providerId"])
	tt.Frame()
	wiringClick(r, tt, "Browser login")
	r.check("auth form does not start authorization", 0, f.count("startProviderOAuth"))
	wiringClick(r, tt, "Europe")
	wiringType(r, tt, "Tenant", "tenant-1")
	wiringAuthSecret(r, tt, a)
	form := a.settings.backend.oauthForm
	secret := form.inputs["accessToken"]
	wiringClick(r, tt, "United States")
	_, tenantVisible := tt.AccessByRole(ui.RoleTextField, "Tenant")
	r.check("auth conditional field hides when inactive", false, tenantVisible)
	wiringClick(r, tt, "Cancel")
	r.check("auth cancel clears sensitive form", true, a.settings.backend.oauthForm == nil && len(form.answers) == 0 && *secret == "")
	r.check("auth cancel does not start authorization", 0, f.count("startProviderOAuth"))
	wiringClick(r, tt, "Browser login")
	wiringClick(r, tt, "Europe")
	wiringType(r, tt, "Tenant", "tenant-1")
	wiringAuthSecret(r, tt, a)
	wiringClick(r, tt, "Continue")
	r.check("auth attempt installs after daemon confirmation", true, wiringApplyUntil(ctx, queue, func() bool { return a.settings.backend.auth.attemptID == "fixture-attempt" }))
	mu.Lock()
	command = oauth
	mu.Unlock()
	answers, _ := command["answers"].(map[string]any)
	r.check("auth complete conditional and secret answers reach daemon", true, answers["region"] == "eu" && answers["tenant"] == "tenant-1" && answers["accessToken"] == "fixture-secret")
	_, hiddenSent := answers["serverDefault"]
	r.check("auth hidden server defaults not echoed", false, hiddenSent)
	r.check("auth start uses selected directory", round, command["directory"])
	r.check("auth URL awaits explicit external-open action", "https://auth.example.test/confirm", a.settings.backend.auth.url)
	r.check("auth submitted form secrets cleared", true, a.settings.backend.oauthForm == nil)
	a.startSettingsOAuthWithAnswers("provider", "provider-exact", "browser", nil)
	r.check("auth duplicate attempt blocked", 1, f.count("startProviderOAuth"))
	a.cancelSettingsOAuth()
	r.check("auth cancellation releases local attempt", "", a.settings.backend.auth.attemptID)
}

func wiringAuthSecret(r *wiringReport, tt *ui.Tester, a *app) {
	if !wiringRoleClick(r, tt, ui.RoleTextField, "Access token") {
		return
	}
	tt.Key(ui.Ctrl, ui.KeyA)
	tt.Type("fixture-secret")
	tt.Frame()
	form := a.settings.backend.oauthForm
	r.check("auth password input updates protected draft", true, form != nil && form.inputs["accessToken"] != nil && *form.inputs["accessToken"] == "fixture-secret")
	entry, _ := tt.AccessByRole(ui.RoleTextField, "Access token")
	r.check("auth password not exposed through accessibility", "", entry.Value)
}

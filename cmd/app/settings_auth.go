package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net/url"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
)

type settingsAuthState struct {
	kind, providerID, name, attemptID, url, mode, instructions, status, code string
	busy                                                                     bool
}

type settingsIntegration struct {
	ID           string                `json:"id"`
	Name         string                `json:"name"`
	SupportsKey  bool                  `json:"supportsKey"`
	Connected    bool                  `json:"connected"`
	NeedsAuth    bool                  `json:"needsAuth"`
	EnvNames     []string              `json:"envNames"`
	KeyAccounts  []settingsKeyAccount  `json:"keyAccounts"`
	OAuthMethods []settingsOAuthMethod `json:"oauthMethods"`
}

type settingsKeyAccount struct {
	ID     string `json:"id"`
	Label  string `json:"label"`
	Active bool   `json:"active"`
}

type settingsOAuthMethod struct {
	ID    string               `json:"id"`
	Label string               `json:"label"`
	Form  []settingsOAuthField `json:"form"`
}

type settingsOAuthField struct {
	Key         string                   `json:"key"`
	Title       string                   `json:"title"`
	Description string                   `json:"description"`
	Required    bool                     `json:"required"`
	Hidden      bool                     `json:"hidden"`
	Options     []settingsOAuthOption    `json:"options"`
	When        []settingsOAuthCondition `json:"when"`
	Placeholder string                   `json:"placeholder"`
	Default     any                      `json:"default"`
	Type        string                   `json:"type"`
}

type settingsOAuthOption struct {
	Value string `json:"value"`
	Label string `json:"label"`
}
type settingsOAuthCondition struct {
	Key   string          `json:"key"`
	Op    string          `json:"op"`
	Value json.RawMessage `json:"value"`
}

func settingsAuthURL(raw string) bool {
	parsed, err := url.Parse(raw)
	return err == nil && parsed.Host != "" && parsed.User == nil && (parsed.Scheme == "https" || parsed.Scheme == "http")
}

func (a *app) loadSettingsIntegrations() {
	st := &a.settings.backend
	if st.integration.loading || a.client == nil {
		return
	}
	st.integration.loading, st.integration.err = true, ""
	source, generation := st.source, st.generation
	binary := a.settingsStr("opencode_binary_path", "opencode")
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
		defer cancel()
		var body struct {
			Integrations []settingsIntegration `json:"integrations"`
		}
		err := settingsRequest(ctx, source, map[string]any{"type": "fetchIntegrations", "binary": binary, "directory": source.directory}, "integrations", &body)
		if err == nil && body.Integrations == nil {
			err = fmt.Errorf("integration list is missing from the response")
		}
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.integration.loading = false
			refreshAgain := st.integrationRefreshPending
			st.integrationRefreshPending = false
			if err != nil {
				if st.integration.message != "" {
					st.integration.err = st.integration.message + tr(" Authorization status could not refresh. ") + err.Error()
				} else {
					st.integration.err = tr("Could not load authorization status. ") + err.Error()
				}
				if refreshAgain {
					a.loadSettingsIntegrations()
				}
				return
			}
			st.integrations, st.integration.loaded, st.integration.uncertain = body.Integrations, true, false
			st.integration.message = ""
			if refreshAgain {
				a.loadSettingsIntegrations()
			}
		})
	}()
}

func (a *app) refreshSettingsIntegrations() {
	if a.settings.backend.integration.loading {
		a.settings.backend.integrationRefreshPending = true
		return
	}
	a.loadSettingsIntegrations()
}

func (a *app) settingsBuiltinIntegrations(c *ui.Context) {
	st := &a.settings.backend
	ui.Row(c).FillWidth().Gap(8).Margin(16, 0, 0, 0).Children(func() {
		ui.Text(c, tr("Provider accounts")).Grow(1).FontSize(uiPx(12)).TextColor(palette.text)
		componentTextButton(c, "integrations-load", tr("Load accounts"), false).Disabled(st.integration.loading).OnClick(a.loadSettingsIntegrations)
	})
	for _, integration := range st.integrations {
		integration := integration
		c.Key("integration-" + integration.ID)
		ui.Column(c).FillWidth().Padding(10, 0).Gap(7).BorderWidth(0, 0, 1, 0).BorderColor(palette.border).Children(func() {
			ui.Row(c).FillWidth().Gap(8).Children(func() {
				ui.Text(c, integration.Name).Grow(1).FontSize(uiPx(12.5)).TextColor(palette.text)
				status := tr("Disconnected")
				if integration.Connected {
					status = tr("Connected")
				}
				if integration.NeedsAuth {
					status = tr("Needs authentication")
				}
				mailEnabledBadge(c, integration.Connected && !integration.NeedsAuth, status)
			})
			for _, account := range integration.KeyAccounts {
				account := account
				label := account.Label
				if account.Active {
					label += tr(" · Active")
				}
				ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
					ui.Text(c, label).Grow(1).FontSize(uiPx(11)).TextColor(palette.textSecondary)
					if !account.Active {
						componentTextButton(c, "integration-account-select-"+integration.ID+"-"+account.ID, tr("Select"), false).Disabled(st.auth.busy || st.integration.uncertain).OnClick(func() {
							a.cancelSettingsOAuthForm()
							st.selectedCredentialID, st.integrationSelected = account.ID, integration.ID
						})
					}
				})
			}
			if st.integrationSelected == integration.ID && st.selectedCredentialID != "" {
				componentTextButton(c, "integration-account-activate-"+integration.ID, tr("Use selected account"), true).Disabled(st.auth.busy || st.integration.uncertain).OnClick(func() { a.activateSettingsKeyAccount(integration.ID, st.selectedCredentialID) })
			}
			if st.integrationSelected == integration.ID && integration.SupportsKey {
				formSecretField(c, "integration-key-"+integration.ID, tr("API key"), &st.integrationKey, &st.integrationShowKey)
				componentTextButton(c, "integration-authorize-"+integration.ID, tr("Connect"), true).Disabled(st.auth.busy || st.integration.uncertain || st.integrationKey == "").OnClick(func() { a.authorizeSettingsIntegration(integration.ID) })
				componentTextButton(c, "integration-key-cancel-"+integration.ID, tr("Cancel"), false).OnClick(func() {
					st.integrationKey, st.integrationShowKey, st.integrationSelected = "", false, ""
					a.closeSettingsMenus()
				})
			}
			ui.Row(c).Gap(8).Wrap().Children(func() {
				if integration.SupportsKey {
					componentTextButton(c, "integration-enter-key-"+integration.ID, tr("API key"), false).Disabled(st.auth.busy).OnClick(func() {
						a.cancelSettingsOAuthForm()
						st.integrationKey = ""
						st.integrationShowKey = false
						st.integrationSelected = integration.ID
						st.selectedCredentialID = ""
					})
				}
				for _, method := range integration.OAuthMethods {
					method := method
					componentTextButton(c, "integration-oauth-"+integration.ID+"-"+method.ID, method.Label, false).Disabled(st.auth.busy || st.integration.uncertain || st.auth.attemptID != "").OnClick(func() {
						if len(method.Form) > 0 {
							a.beginSettingsOAuthForm(integration.ID, method)
							return
						}
						a.cancelSettingsOAuthForm()
						a.startSettingsOAuth("provider", integration.ID, method.ID)
					})
				}
				if integration.Connected {
					componentTextButton(c, "integration-logout-"+integration.ID, tr("Disconnect"), false).Disabled(st.auth.busy || st.integration.uncertain || len(integration.EnvNames) > 0).Tooltip(choice(len(integration.EnvNames) > 0, tr("Environment credentials cannot be removed here"), "")).OnClick(func() { a.disconnectSettingsIntegration(integration.ID) })
				}
			})
		})
	}
	if st.auth.kind == "provider" {
		a.settingsOAuthControls(c)
	}
	if form := st.oauthForm; form != nil {
		a.settingsOAuthForm(c, form)
	}
}

func (a *app) authorizeSettingsIntegration(id string) {
	key := a.settings.backend.integrationKey
	a.runSettingsIntegrationCommand(map[string]any{"type": "authorizeProvider", "providerId": id, "key": key})
}

func (a *app) beginSettingsOAuthForm(providerID string, method settingsOAuthMethod) {
	a.cancelSettingsOAuthForm()
	answers := make(map[string]string)
	inputs := make(map[string]*string)
	showSecret := make(map[string]*bool)
	for _, field := range method.Form {
		value, _ := field.Default.(string)
		answers[field.Key] = value
		inputs[field.Key] = &value
		show := false
		showSecret[field.Key] = &show
	}
	a.settings.backend.oauthForm = &settingsOAuthFormState{providerID: providerID, methodID: method.ID, answers: answers, inputs: inputs, showSecret: showSecret}
}

func (a *app) settingsOAuthForm(c *ui.Context, form *settingsOAuthFormState) {
	for key, input := range form.inputs {
		form.answers[key] = *input
	}
	var method *settingsOAuthMethod
	for i := range a.settings.backend.integrations {
		integration := &a.settings.backend.integrations[i]
		if integration.ID == form.providerID {
			for j := range integration.OAuthMethods {
				if integration.OAuthMethods[j].ID == form.methodID {
					method = &integration.OAuthMethods[j]
				}
			}
		}
	}
	if method == nil {
		a.cancelSettingsOAuthForm()
		return
	}
	activeFields := settingsOAuthActiveFields(method.Form, form.answers)
	ui.Column(c).FillWidth().Padding(10, 0).Gap(8).Children(func() {
		ui.Text(c, method.Label).FontSize(uiPx(12.5)).FontWeight(500).TextColor(palette.text)
		for _, field := range method.Form {
			if field.Hidden || !activeFields[field.Key] {
				continue
			}
			field := field
			label := field.Title
			if label == "" {
				label = field.Key
			}
			if len(field.Options) > 0 {
				ui.Text(c, label).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
				for _, option := range field.Options {
					option := option
					componentTextButton(c, "oauth-choice-"+field.Key+"-"+option.Value, option.Label, form.answers[field.Key] == option.Value).OnClick(func() { form.answers[field.Key] = option.Value; *form.inputs[field.Key] = option.Value })
				}
			} else if settingsOAuthFieldIsSecret(field) {
				formSecretField(c, "oauth-field-"+field.Key, label, form.inputs[field.Key], form.showSecret[field.Key])
			} else {
				settingsFormField(c, "oauth-field-"+field.Key, label, form.inputs[field.Key], field.Placeholder)
			}
			if field.Description != "" {
				ui.Text(c, field.Description).FontSize(uiPx(10.5)).TextColor(palette.textSecondary).Wrap()
			}
		}
		ui.Row(c).Gap(8).Children(func() {
			for key, input := range form.inputs {
				form.answers[key] = *input
			}
			componentTextButton(c, "oauth-form-cancel", tr("Cancel"), false).OnClick(a.cancelSettingsOAuthForm)
			componentTextButton(c, "oauth-form-submit", tr("Continue"), true).Disabled(!settingsOAuthFormReady(method.Form, form.answers) || a.settings.backend.auth.busy).OnClick(func() {
				for key, input := range form.inputs {
					form.answers[key] = *input
				}
				answers := settingsOAuthAnswers(method.Form, form.answers)
				for key := range form.answers {
					form.answers[key] = ""
					delete(form.answers, key)
				}
				for key, input := range form.inputs {
					*input = ""
					delete(form.inputs, key)
				}
				clear(form.showSecret)
				a.settings.backend.oauthForm = nil
				a.startSettingsOAuthWithAnswers("provider", form.providerID, form.methodID, answers)
			})
		})
	})
}

func settingsOAuthFieldIsSecret(field settingsOAuthField) bool {
	key := strings.ToLower(field.Key + " " + field.Title + " " + field.Type)
	for _, marker := range []string{"secret", "password", "token", "api_key", "apikey", "credential"} {
		if strings.Contains(key, marker) {
			return true
		}
	}
	return false
}

func settingsOAuthFieldActive(field settingsOAuthField, answers map[string]string) bool {
	for _, condition := range field.When {
		actual, exists := answers[condition.Key]
		var expected string
		if !exists || json.Unmarshal(condition.Value, &expected) != nil {
			return false
		}
		switch condition.Op {
		case "eq":
			if actual != expected {
				return false
			}
		case "neq":
			if actual == expected {
				return false
			}
		default:
			return false
		}
	}
	return true
}

func settingsOAuthActiveFields(fields []settingsOAuthField, answers map[string]string) map[string]bool {
	active := make(map[string]bool, len(fields))
	effective := make(map[string]string, len(answers))
	for pass := 0; pass < len(fields); pass++ {
		changed := false
		for _, field := range fields {
			if active[field.Key] || field.Hidden || !settingsOAuthFieldActive(field, effective) {
				continue
			}
			active[field.Key], changed = true, true
			if value := strings.TrimSpace(answers[field.Key]); value != "" {
				effective[field.Key] = value
			}
		}
		if !changed {
			break
		}
	}
	return active
}

func settingsOAuthFormReady(fields []settingsOAuthField, answers map[string]string) bool {
	activeFields := settingsOAuthActiveFields(fields, answers)
	for _, field := range fields {
		if field.Hidden || !activeFields[field.Key] {
			continue
		}
		value := strings.TrimSpace(answers[field.Key])
		if field.Required && value == "" {
			return false
		}
		if value != "" && len(field.Options) > 0 {
			valid := false
			for _, option := range field.Options {
				if option.Value == value {
					valid = true
					break
				}
			}
			if !valid {
				return false
			}
		}
	}
	return true
}

func settingsOAuthAnswers(fields []settingsOAuthField, answers map[string]string) map[string]any {
	result := make(map[string]any)
	activeFields := settingsOAuthActiveFields(fields, answers)
	for _, field := range fields {
		if !field.Hidden && activeFields[field.Key] {
			if value := strings.TrimSpace(answers[field.Key]); value != "" {
				result[field.Key] = value
			}
		}
	}
	return result
}

func (a *app) cancelSettingsOAuthForm() {
	if form := a.settings.backend.oauthForm; form != nil {
		for key := range form.answers {
			form.answers[key] = ""
			delete(form.answers, key)
		}
		for key, input := range form.inputs {
			*input = ""
			delete(form.inputs, key)
		}
		clear(form.showSecret)
	}
	a.settings.backend.oauthForm = nil
}

func (a *app) activateSettingsKeyAccount(providerID, credentialID string) {
	st := &a.settings.backend
	if st.auth.busy || st.integration.uncertain || credentialID == "" {
		return
	}
	foundProvider, foundAccount := false, false
	for _, integration := range st.integrations {
		if integration.ID != providerID {
			continue
		}
		foundProvider = true
		for _, account := range integration.KeyAccounts {
			if account.ID == credentialID {
				foundAccount = true
				break
			}
		}
		break
	}
	if !foundProvider || !foundAccount {
		return
	}
	a.runSettingsIntegrationCommand(settingsActivateProviderKeyAccountCommand(providerID, credentialID))
}

func settingsActivateProviderKeyAccountCommand(providerID, credentialID string) map[string]any {
	return map[string]any{"type": "activateProviderKeyAccount", "providerId": providerID, "credentialId": credentialID}
}

func (a *app) disconnectSettingsIntegration(id string) {
	a.runSettingsIntegrationCommand(map[string]any{"type": "logoutProvider", "providerId": id})
}

func (a *app) runSettingsIntegrationCommand(command map[string]any) {
	st := &a.settings.backend
	if st.auth.busy || st.integration.uncertain || st.auth.attemptID != "" {
		return
	}
	a.cancelSettingsOAuthForm()
	source, generation := st.source, st.generation
	st.authGeneration++
	authGeneration := st.authGeneration
	commandType, _ := command["type"].(string)
	command["binary"], command["directory"] = a.settingsStr("opencode_binary_path", "opencode"), source.directory
	st.integration.err, st.integration.message = "", ""
	st.auth.busy = true
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
		defer cancel()
		err := settingsRequest(ctx, source, command, "ack", nil)
		delete(command, "key")
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) || a.settings.backend.authGeneration != authGeneration {
				return
			}
			st := &a.settings.backend
			st.auth.busy = false
			if err != nil {
				st.integration.uncertain = true
				st.integration.err = tr("Authentication failed. Reload accounts before retrying. ") + err.Error()
				return
			}
			st.integrationKey, st.integrationShowKey = "", false
			if commandType == "authorizeProvider" || commandType == "activateProviderKeyAccount" {
				st.integration.message = tr("Credential saved. Account status is refreshing.")
			} else {
				st.integration.message = tr("Saved")
			}
			if commandType == "activateProviderKeyAccount" {
				st.selectedCredentialID = ""
			}
			a.refreshSettingsIntegrations()
		})
	}()
}

func (a *app) settingsMcpAuth(c *ui.Context, server settingsMcpServer) {
	st := &a.settings.backend
	if server.OAuth.Mode != "disabled" {
		componentTextButton(c, "mcp-sign-in", tr("Sign in with OAuth"), false).Disabled(st.auth.busy || st.auth.attemptID != "" || st.mcp.writing).OnClick(func() { a.startSettingsOAuth("mcp", server.Name, "") })
	} else {
		ui.Text(c, tr("Headers-only authentication")).FontSize(uiPx(11)).TextColor(palette.textSecondary)
	}
	if st.auth.kind == "mcp" && st.auth.name == server.Name {
		a.settingsOAuthControls(c)
	}
}

func (a *app) startSettingsOAuth(kind, id, method string) {
	a.startSettingsOAuthWithAnswers(kind, id, method, map[string]any{})
}

func (a *app) startSettingsOAuthWithAnswers(kind, id, method string, answers map[string]any) {
	st := &a.settings.backend
	if st.auth.busy || st.auth.attemptID != "" {
		return
	}
	if kind == "provider" && st.integration.uncertain || kind == "mcp" && st.mcp.uncertain {
		return
	}
	source, generation := st.source, st.generation
	st.authGeneration++
	authGeneration := st.authGeneration
	st.integration.message = ""
	binary := a.settingsStr("opencode_binary_path", "opencode")
	command := settingsCommand("startMcpOAuth", source)
	expected := "mcpOAuthStarted"
	if kind == "provider" {
		command["type"], command["binary"], command["providerId"], command["methodId"], command["answers"] = "startProviderOAuth", a.settingsStr("opencode_binary_path", "opencode"), id, method, answers
		expected = "providerOAuthStarted"
	} else {
		command["name"] = id
	}
	st.auth = settingsAuthState{kind: kind, providerID: id, name: id, busy: true, status: "Starting sign-in..."}
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
		defer cancel()
		var body struct {
			Attempt struct {
				AttemptID    string `json:"attemptId"`
				URL          string `json:"url"`
				Mode         string `json:"mode"`
				Instructions string `json:"instructions"`
			} `json:"attempt"`
		}
		err := settingsRequest(ctx, source, command, expected, &body)
		if answerMap, ok := command["answers"].(map[string]any); ok {
			for key := range answerMap {
				delete(answerMap, key)
			}
		}
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) || a.settings.backend.authGeneration != authGeneration {
				if err == nil && body.Attempt.AttemptID != "" {
					a.cancelSettingsOAuthAttempt(source, settingsAuthState{kind: kind, providerID: id, attemptID: body.Attempt.AttemptID}, binary)
				}
				return
			}
			st := &a.settings.backend
			st.auth.busy = false
			if err != nil {
				st.auth = settingsAuthState{kind: kind, providerID: id, name: id}
				if kind == "provider" {
					st.integration.uncertain = true
					st.integration.err = tr("Sign-in result is unknown. Reload accounts before starting another attempt.")
				} else {
					st.mcp.uncertain = true
					st.mcp.err = tr("Sign-in result is unknown. Reload accounts before starting another attempt.")
				}
				return
			}
			if body.Attempt.AttemptID == "" || !settingsAuthURL(body.Attempt.URL) {
				if body.Attempt.AttemptID != "" {
					attempt := settingsAuthState{kind: kind, providerID: id, attemptID: body.Attempt.AttemptID}
					a.cancelSettingsOAuthAttempt(source, attempt, binary)
				}
				st.auth = settingsAuthState{kind: kind, providerID: id, name: id}
				if kind == "provider" {
					st.integration.uncertain = true
					st.integration.err = tr("Invalid authorization response. Reload accounts before retrying.")
				} else {
					st.mcp.uncertain = true
					st.mcp.err = tr("Invalid authorization response. Reload accounts before retrying.")
				}
				return
			}
			st.auth.attemptID, st.auth.url, st.auth.mode, st.auth.instructions, st.auth.status = body.Attempt.AttemptID, body.Attempt.URL, body.Attempt.Mode, body.Attempt.Instructions, tr("Awaiting sign-in")
		})
	}()
}

func (a *app) settingsOAuthControls(c *ui.Context) {
	st := &a.settings.backend
	if st.auth.status != "" {
		ui.Text(c, st.auth.status).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
	}
	if st.auth.instructions != "" {
		ui.Text(c, st.auth.instructions).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
	}
	if st.auth.attemptID == "" {
		return
	}
	if st.auth.mode == "code" {
		settingsFormField(c, "settings-oauth-code", tr("Authorization code"), &st.auth.code, "")
	}
	services := c.Services()
	ui.Row(c).Gap(8).Children(func() {
		componentTextButton(c, "settings-oauth-open", tr("Open sign-in page"), false).Disabled(st.auth.busy).OnClick(func() {
			if settingsAuthURL(st.auth.url) {
				services.OpenURL(st.auth.url)
			}
		})
		componentTextButton(c, "settings-oauth-check", choice(st.auth.mode == "code", tr("Submit code"), tr("Check status")), false).Disabled(st.auth.busy || (st.auth.mode == "code" && st.auth.code == "")).OnClick(func() { a.finishSettingsOAuth(false) })
		componentTextButton(c, "settings-oauth-cancel", tr("Cancel sign-in"), false).Disabled(st.auth.busy).OnClick(func() { a.finishSettingsOAuth(true) })
	})
}

func (a *app) finishSettingsOAuth(cancelAttempt bool) {
	st := &a.settings.backend
	auth := st.auth
	if auth.attemptID == "" || auth.busy {
		return
	}
	source, generation := st.source, st.generation
	authGeneration := st.authGeneration
	command := settingsCommand("pollMcpOAuth", source)
	command["attemptId"] = auth.attemptID
	expected := "mcpOAuthPolled"
	if auth.kind == "provider" {
		command["type"], command["providerId"], command["binary"] = "pollProviderOAuth", auth.providerID, a.settingsStr("opencode_binary_path", "opencode")
		expected = "providerOAuthPolled"
		if auth.mode == "code" {
			command["type"], command["code"] = "completeProviderOAuth", auth.code
			expected = "ack"
		}
	}
	if cancelAttempt {
		command["type"] = choice(auth.kind == "provider", "cancelProviderOAuth", "cancelMcpOAuth")
		expected = "ack"
	}
	st.auth.busy = true
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 45*time.Second)
		defer cancel()
		var body struct {
			Status struct {
				Status string `json:"status"`
			} `json:"status"`
			Poll struct {
				Status  string `json:"status"`
				Message string `json:"message"`
			} `json:"poll"`
		}
		err := settingsRequest(ctx, source, command, expected, &body)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) || a.settings.backend.authGeneration != authGeneration || a.settings.backend.auth.attemptID != auth.attemptID {
				return
			}
			st := &a.settings.backend
			st.auth.busy = false
			if err != nil {
				if auth.kind == "provider" {
					st.integration.err = tr("Sign-in status unavailable. ") + err.Error()
				} else {
					st.mcp.err = tr("Sign-in status unavailable. ") + err.Error()
				}
				return
			}
			if cancelAttempt {
				st.auth = settingsAuthState{}
				return
			}
			status := body.Status.Status
			if auth.kind == "provider" {
				status = body.Poll.Status
			}
			if expected == "ack" {
				status = "complete"
			}
			st.auth.status = fmt.Sprintf(tr("Sign-in %s"), status)
			if status == "complete" || status == "failed" || status == "expired" {
				if status == "complete" {
					st.integration.message = tr("Saved")
				} else if auth.kind == "provider" {
					st.integration.err = st.auth.status
				} else {
					st.mcp.err = st.auth.status
				}
				st.auth.status = ""
				st.auth.attemptID, st.auth.url, st.auth.code = "", "", ""
				a.collectSettingsFeedback()
				if auth.kind == "provider" {
					a.refreshSettingsIntegrations()
				} else {
					a.loadSettingsMcp()
				}
			}
		})
	}()
}

func (a *app) cancelSettingsOAuth() {
	st := &a.settings.backend
	a.cancelSettingsOAuthForm()
	auth, source := st.auth, st.source
	st.authGeneration++
	st.auth = settingsAuthState{}
	a.cancelSettingsOAuthAttempt(source, auth, a.settingsStr("opencode_binary_path", "opencode"))
}

func (a *app) cancelSettingsOAuthAttempt(source settingsSource, auth settingsAuthState, binary string) {
	if auth.attemptID == "" || source.client == nil {
		return
	}
	command := settingsCommand("cancelMcpOAuth", source)
	command["attemptId"] = auth.attemptID
	if auth.kind == "provider" {
		command["type"], command["providerId"], command["binary"] = "cancelProviderOAuth", auth.providerID, binary
	}
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 5*time.Second)
		defer cancel()
		_ = settingsRequest(ctx, source, command, "ack", nil)
	}()
}

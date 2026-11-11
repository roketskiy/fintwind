package main

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
)

func (a *app) beginSettingsMcp(server *settingsMcpServer) {
	st, backend := &a.settings.catalog, &a.settings.backend
	backend.mcpDraftGeneration++
	backend.mcpEditing, backend.removeConfirm = "", ""
	st.mfName, st.mfKind, st.mfCommand, st.mfURL = "", "local", `["npx", "-y", "my-server"]`, ""
	st.mfEnvironment, st.mfHeaders, st.mfOAuth, st.mfClientID, st.mfClientSecret, st.mfScope = "{}", "{}", "automatic", "", "", ""
	st.mfShowSecret = false
	if server != nil {
		backend.mcpEditing, backend.mcpSelected = server.Name, server.Name
		st.mfName, st.mfKind, st.mfURL = server.Name, server.Kind, server.URL
		data, _ := json.Marshal(server.Command)
		st.mfCommand = string(data)
		st.mfEnvironment, st.mfHeaders = settingsPairsJSON(server.Environment), settingsPairsJSON(server.Headers)
		st.mfOAuth, st.mfClientID, st.mfClientSecret, st.mfScope = server.OAuth.Mode, server.OAuth.ClientID, server.OAuth.ClientSecret, server.OAuth.Scope
	}
	st.mcpAdding = true
	a.closeSettingsMenus()
}

func (a *app) settingsMcpDraft() (settingsMcpServer, error) {
	form, st := &a.settings.catalog, &a.settings.backend
	server := settingsMcpServer{Enabled: true, Environment: [][2]string{}, Headers: [][2]string{}, Command: []string{}, Raw: map[string]any{}, OAuth: settingsMcpOAuth{Mode: "automatic"}}
	for _, item := range st.mcpServers {
		if item.Name == st.mcpEditing {
			server = item
			break
		}
	}
	server.Name, server.Kind = strings.TrimSpace(form.mfName), form.mfKind
	if server.Name == "" {
		return server, errors.New("Enter a server name")
	}
	for _, item := range st.mcpServers {
		if item.Name == server.Name && item.Name != st.mcpEditing {
			return server, errors.New("A server with this name already exists")
		}
	}
	if server.Kind == "remote" {
		parsed, err := url.Parse(strings.TrimSpace(form.mfURL))
		if err != nil || parsed.Host == "" || (parsed.Scheme != "http" && parsed.Scheme != "https") || parsed.User != nil {
			return server, errors.New("Enter an HTTP or HTTPS URL without credentials")
		}
		server.URL = strings.TrimSpace(form.mfURL)
	} else if server.Kind == "local" {
		var command []string
		if json.Unmarshal([]byte(form.mfCommand), &command) != nil || len(command) == 0 || strings.TrimSpace(command[0]) == "" {
			return server, errors.New("Command must be a JSON array of arguments")
		}
		server.Command = command
	} else {
		return server, errors.New("Choose a server type")
	}
	var err error
	server.Environment, err = settingsParsePairs(form.mfEnvironment)
	if err != nil {
		return server, errors.New("Environment must be a JSON object of string values")
	}
	server.Headers, err = settingsParsePairs(form.mfHeaders)
	if err != nil {
		return server, errors.New("Headers must be a JSON object of string values")
	}
	server.OAuth = settingsMcpOAuth{Mode: form.mfOAuth, ClientID: form.mfClientID, ClientSecret: form.mfClientSecret, Scope: form.mfScope}
	return server, nil
}

func settingsPairsJSON(pairs [][2]string) string {
	values := map[string]string{}
	for _, pair := range pairs {
		values[pair[0]] = pair[1]
	}
	data, _ := json.Marshal(values)
	return string(data)
}

func settingsParsePairs(text string) ([][2]string, error) {
	values := map[string]string{}
	if strings.TrimSpace(text) != "" {
		if err := json.Unmarshal([]byte(text), &values); err != nil {
			return nil, err
		}
	}
	pairs := make([][2]string, 0, len(values))
	for key, value := range values {
		pairs = append(pairs, [2]string{key, value})
	}
	return pairs, nil
}

func (a *app) saveSettingsMcpForm() {
	server, err := a.settingsMcpDraft()
	st := &a.settings.backend
	if err != nil {
		st.mcp.err = err.Error()
		return
	}
	roster := append([]settingsMcpServer{}, st.mcpServers...)
	found := false
	for i := range roster {
		if roster[i].Name == st.mcpEditing {
			roster[i], found = server, true
			break
		}
	}
	if !found {
		roster = append(roster, server)
	}
	a.commitSettingsMcp(roster, server.Name)
}

func (a *app) commitSettingsMcp(roster []settingsMcpServer, selected string) {
	st := &a.settings.backend
	if !st.mcp.loaded || st.mcp.writing || st.mcp.uncertain {
		return
	}
	source, generation, revision := st.source, st.generation, st.mcpRevision
	draftGeneration, wasAdding, previousSelected := st.mcpDraftGeneration, a.settings.catalog.mcpAdding, st.mcpSelected
	binary := a.settingsStr("opencode_binary_path", "opencode")
	st.mcp.writing, st.mcp.err, st.mcp.message = true, "", ""
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
		defer cancel()
		command := settingsCommand("saveMcpServers", source)
		command["expectedRevision"], command["servers"] = revision, roster
		var body struct {
			Servers  []settingsMcpServer `json:"servers"`
			Revision uint64              `json:"revision"`
		}
		err := settingsRequest(ctx, source, command, "mcpServers", &body)
		if err == nil && body.Servers == nil {
			err = errors.New("Saved MCP server list is missing from the response")
		}
		var reloadErr error
		var statuses struct {
			Statuses []settingsMcpStatus `json:"statuses"`
		}
		if err == nil {
			reloadErr = settingsRequest(ctx, source, map[string]any{"type": "reloadOpenCodeServer", "binary": binary, "directory": source.directory}, "ack", nil)
			if reloadErr == nil {
				reloadErr = settingsRequest(ctx, source, map[string]any{"type": "listMcpServerStatuses", "binary": binary, "directory": source.directory}, "mcpServerStatuses", &statuses)
			}
		}
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.mcp.writing = false
			if err != nil {
				st.mcp.err = tr("Save failed. Reload before retrying. ") + err.Error()
				st.mcp.uncertain = true
				return
			}
			st.mcpServers, st.mcpRevision, st.mcpStatuses = body.Servers, body.Revision, statuses.Statuses
			if st.mcpSelected == previousSelected {
				st.mcpSelected = selected
			}
			st.removeConfirm = ""
			if wasAdding && st.mcpDraftGeneration == draftGeneration {
				a.settings.catalog.mcpAdding = false
				st.mcpEditing = ""
				a.clearSettingsMcpSecrets()
			}
			st.mcp.message, st.mcpStatusError = tr("Saved"), ""
			if reloadErr != nil {
				st.mcpStatusError = "Configuration saved; connection status unavailable. " + reloadErr.Error()
			}
		})
	}()
}

func (a *app) clearSettingsMcpSecrets() {
	a.settings.backend.mcpDraftGeneration++
	form := &a.settings.catalog
	form.mfClientSecret, form.mfHeaders, form.mfEnvironment, form.mfShowSecret = "", "", "", false
}

func (a *app) installSettingsMarket(entry settingsMarketEntry) {
	st := &a.settings.backend
	for _, server := range st.mcpServers {
		if server.Name == entry.id {
			st.mcpSelected = server.Name
			a.openSettingsPage("mcpServers")
			return
		}
	}
	server := settingsMcpServer{Name: entry.id, Kind: "remote", URL: entry.url, Enabled: true, Command: []string{}, Environment: [][2]string{}, Headers: [][2]string{}, OAuth: settingsMcpOAuth{Mode: "automatic"}, Raw: map[string]any{}}
	roster := append(append([]settingsMcpServer{}, st.mcpServers...), server)
	a.commitSettingsMcp(roster, server.Name)
}

func (a *app) settingsMcpDetail(c *ui.Context) {
	st := &a.settings.backend
	var selected *settingsMcpServer
	for i := range st.mcpServers {
		if st.mcpServers[i].Name == st.mcpSelected {
			selected = &st.mcpServers[i]
			break
		}
	}
	if selected == nil {
		mailDetailPlaceholder(c, "wrench", "Select a server to inspect it")
		return
	}
	server := *selected
	statusText := "Not checked"
	for _, status := range st.mcpStatuses {
		if status.Name == server.Name {
			statusText = status.Status
			if status.Error != "" {
				statusText += " · " + status.Error
			}
			break
		}
	}
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24).Gap(14).Children(func() {
			formTitle(c, server.Name)
			mailEnabledBadge(c, server.Enabled, choice(server.Enabled, "Enabled", "Disabled"))
			ui.Text(c, statusText).FontSize(12).TextColor(palette.textSecondary).Wrap()
			endpoint := server.URL
			if server.Kind == "local" {
				data, _ := json.Marshal(server.Command)
				endpoint = string(data)
			}
			ui.Text(c, endpoint).Font(monoFont()).FontSize(11.5).TextColor(palette.textSecondary).Wrap()
			ui.Row(c).Gap(8).Children(func() {
				componentTextButton(c, "mcp-edit", tr("Edit"), false).Disabled(st.mcp.writing).OnClick(func() { a.beginSettingsMcp(&server) })
				componentTextButton(c, "mcp-toggle", choice(server.Enabled, tr("Disable"), tr("Enable")), false).Disabled(st.mcp.writing || st.mcp.uncertain).OnClick(func() {
					roster := append([]settingsMcpServer{}, st.mcpServers...)
					for i := range roster {
						if roster[i].Name == server.Name {
							roster[i].Enabled = !server.Enabled
						}
					}
					a.commitSettingsMcp(roster, server.Name)
				})
				componentTextButton(c, "mcp-remove", tr("Remove"), false).Disabled(st.mcp.writing || st.mcp.uncertain).OnClick(func() { st.removeConfirm = "mcp:" + server.Name })
			})
			if st.removeConfirm == "mcp:"+server.Name {
				ui.Text(c, tr("Remove this MCP server configuration?")).FontSize(12).TextColor(palette.text)
				ui.Row(c).Gap(8).Children(func() {
					componentTextButton(c, "mcp-remove-cancel", tr("Cancel"), false).OnClick(func() { st.removeConfirm = "" })
					componentTextButton(c, "mcp-remove-confirm", tr("Remove server"), true).Disabled(st.mcp.writing).OnClick(func() {
						roster := make([]settingsMcpServer, 0, len(st.mcpServers))
						for _, item := range st.mcpServers {
							if item.Name != server.Name {
								roster = append(roster, item)
							}
						}
						a.commitSettingsMcp(roster, "")
					})
				})
			}
			if server.Kind == "remote" {
				a.settingsMcpAuth(c, server)
			}
			settingsOperationStatus(c, st.mcp, "")
			if st.mcpStatusError != "" {
				ui.Text(c, st.mcpStatusError).FontSize(11).TextColor(palette.textSecondary).Wrap()
			}
		})
	})
}

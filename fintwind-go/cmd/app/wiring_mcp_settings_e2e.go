//go:build wiringe2e

package main

import (
	"context"
	"encoding/json"
	"errors"
	"sync"

	"fintwind-go/internal/daemon"

	"github.com/egoist/mygo/ui"
)

// wiringMcpSettings exercises the MCP settings page headless: roster load,
// search predicate, selection drafts, enable toggle, rename, key-value
// variables, kind switch, add form with slug minting, and delete. Daemon
// traffic goes through the shared loopback fixture via the extended hook so
// the allowlist snapshot taken earlier in wiringRun stays clean.
func wiringMcpSettings(ctx context.Context, r *wiringReport, client *daemon.Client, f *wiringFixture, root string) {
	servers := []any{
		map[string]any{
			"name": "chart", "kind": "local", "command": []any{"npx", "-y", "chart"}, "url": "",
			"environment": []any{[]any{"API_KEY", "x"}}, "headers": []any{},
			"oauth": map[string]any{"mode": "automatic", "clientId": "", "clientSecret": "", "scope": ""},
			"enabled": true, "raw": map[string]any{},
		},
		map[string]any{
			"name": "docs", "kind": "remote", "command": []any{}, "url": "https://docs.example.test/mcp",
			"environment": []any{}, "headers": []any{[]any{"Authorization", "Bearer fixture"}},
			"oauth": map[string]any{"mode": "automatic", "clientId": "", "clientSecret": "", "scope": ""},
			"enabled": false, "raw": map[string]any{},
		},
	}
	revision := uint64(7)
	var fixtureMu sync.Mutex
	f.extended = func(envelope wiringEnvelope) (any, error, bool, bool) {
		switch envelope.Command.Type {
		case "loadMcpServers":
			fixtureMu.Lock()
			defer fixtureMu.Unlock()
			return map[string]any{"type": "mcpServers", "servers": servers, "revision": revision}, nil, false, true
		case "saveMcpServers":
			var command struct {
				ExpectedRevision uint64 `json:"expectedRevision"`
				Servers          []any  `json:"servers"`
			}
			if err := json.Unmarshal(envelope.Command.Raw, &command); err != nil {
				return nil, err, false, true
			}
			fixtureMu.Lock()
			defer fixtureMu.Unlock()
			if command.ExpectedRevision != revision {
				return nil, errors.New("revision conflict"), false, true
			}
			servers = command.Servers
			revision++
			return map[string]any{"type": "mcpServers", "servers": servers, "revision": revision}, nil, false, true
		case "reloadOpenCodeServer":
			return map[string]any{"type": "ack"}, nil, false, true
		case "listMcpServerStatuses":
			fixtureMu.Lock()
			defer fixtureMu.Unlock()
			statuses := []any{}
			for _, item := range servers {
				entry, _ := item.(map[string]any)
				status := "disabled"
				if enabled, _ := entry["enabled"].(bool); enabled {
					status = "connected"
				}
				statuses = append(statuses, map[string]any{"name": entry["name"], "status": status})
			}
			return map[string]any{"type": "mcpServerStatuses", "statuses": statuses}, nil, false, true
		}
		return nil, nil, false, false
	}

	a := wiringApp(ctx, client, root)
	a.syncSettingsSource()
	queue, release := installHeadlessDispatcher(a)
	defer release()
	idle := func() bool {
		return wiringApplyUntil(ctx, queue, func() bool {
			return !a.settings.backend.mcp.loading && !a.settings.backend.mcp.writing
		})
	}

	a.loadSettingsMcp()
	r.check("mcp roster loads from daemon", true, wiringApplyUntil(ctx, queue, func() bool { return a.settings.backend.mcp.loaded }))
	r.check("mcp roster names", []string{"chart", "docs"}, mcpServerNames(a.settings.backend.mcpServers))
	r.check("mcp revision tracks daemon", uint64(7), a.settings.backend.mcpRevision)
	r.check("mcp statuses refresh on load", 2, len(a.settings.backend.mcpStatuses))
	r.check("mcp selection falls back to first server", "chart", a.effectiveMcpServerName())
	r.check("mcp command draft syncs on load", "npx -y chart", a.settings.catalog.mdCommand)

	probe := settingsMcpServer{Name: "chart", Kind: "local", Command: []string{"npx", "-y", "chart"}}
	r.check("mcp search matches name case-insensitively", true, mcpServerMatches("CHART", probe))
	r.check("mcp search matches command text", true, mcpServerMatches("npx", probe))
	remote := settingsMcpServer{Name: "docs", Kind: "remote", URL: "https://docs.example.test/mcp"}
	r.check("mcp search matches url", true, mcpServerMatches("docs.example", remote))
	r.check("mcp search rejects unrelated query", false, mcpServerMatches("zzz", remote))
	r.check("mcp search empty query keeps every row", true, mcpServerMatches("  ", remote))

	tt := ui.NewTester(func(c *ui.Context) { a.mcpSettings(c) }, 900, 1000)
	tt.Frame()

	a.selectMcpServer("docs")
	r.check("mcp select pins drafts to url", "https://docs.example.test/mcp", a.settings.catalog.mdURL)

	saveCount := func() int { return f.count("saveMcpServers") }
	before := saveCount()
	a.toggleSettingsMcp("docs")
	r.check("mcp toggle commits and settles", true, wiringApplyUntil(ctx, queue, func() bool {
		return !a.settings.backend.mcp.writing && a.settings.backend.mcp.loaded && len(a.settings.backend.mcpServers) == 2
	}) && idle())
	r.check("mcp toggle enables server", true, a.findMcpServer("docs").Enabled)
	r.check("mcp toggle sends one save", before+1, saveCount())
	r.check("mcp status follows enabled flip", "connected", mcpStatusOf(a, "docs"))
	tt.Frame()

	a.selectMcpServer("chart")
	a.settings.catalog.mdCommand = "npx -y chart --verbose"
	a.updateMcpCommandFromDraft()
	r.check("mcp command edit debounces into commit", true, wiringApplyUntil(ctx, queue, func() bool {
		srv := a.findMcpServer("chart")
		return srv != nil && len(srv.Command) == 4 && a.settings.backend.mcpRevision == 9 && !a.settings.backend.mcp.writing
	}) && idle())
	r.check("mcp command draft keeps edited text", "npx -y chart --verbose", a.settings.catalog.mdCommand)

	a.selectMcpServer("docs")
	beforeRename := saveCount()
	a.beginMcpRename()
	a.settings.catalog.mdRename = "chart"
	a.confirmMcpRename()
	r.check("mcp rename duplicate reports error", "A server with this name already exists", a.settings.backend.mcp.err)
	r.check("mcp rename duplicate sends no save", beforeRename, saveCount())
	a.settings.backend.mcp.err = ""
	a.beginMcpRename()
	a.settings.catalog.mdRename = "docs2"
	a.confirmMcpRename()
	r.check("mcp rename commits and reselects", true, idle() && a.settings.backend.mcpSelected == "docs2")
	r.check("mcp rename keeps origin revision chain", uint64(10), a.settings.backend.mcpRevision)
	tt.Frame()

	a.beginMcpVariableEditor("headers", 0, true)
	a.settings.catalog.mcpVarKey = "Authorization"
	a.settings.catalog.mcpVarValue = "Bearer other"
	dupBefore := saveCount()
	a.confirmMcpVariableEditor()
	r.check("mcp variable duplicate key reports error", "That key already exists", a.settings.backend.mcp.err)
	r.check("mcp variable duplicate sends no save", dupBefore, saveCount())
	a.settings.backend.mcp.err = ""
	a.beginMcpVariableEditor("headers", 0, true)
	a.settings.catalog.mcpVarKey = "X-Trace"
	a.settings.catalog.mcpVarValue = "1"
	a.confirmMcpVariableEditor()
	r.check("mcp variable add commits", true, idle() && len(a.findMcpServer("docs2").Headers) == 2)
	a.deleteMcpVariable("headers", 1)
	r.check("mcp variable delete commits", true, idle() && len(a.findMcpServer("docs2").Headers) == 1)
	tt.Frame()

	a.setMcpServerKind("docs2", "local")
	r.check("mcp kind switch commits", true, idle() && a.findMcpServer("docs2").Kind == "local")
	tt.Frame()

	a.beginSettingsMcp()
	a.settings.catalog.mfName = "New Helper"
	a.settings.catalog.mfKind = "local"
	a.settings.catalog.mfCommand = "npx -y helper"
	a.submitMcpAddForm()
	r.check("mcp add mints slug and selects", true, idle() && a.findMcpServer("new-helper") != nil && a.settings.backend.mcpSelected == "new-helper")
	a.beginSettingsMcp()
	a.settings.catalog.mfName = "New Helper"
	a.settings.catalog.mfKind = "local"
	a.settings.catalog.mfCommand = "npx -y helper"
	a.submitMcpAddForm()
	r.check("mcp add duplicate mints unique slug", true, idle() && a.findMcpServer("new-helper-2") != nil)
	a.beginSettingsMcp()
	a.settings.catalog.mfName = "bad remote"
	a.settings.catalog.mfKind = "remote"
	a.settings.catalog.mfURL = "ftp://example.test/mcp"
	savesBeforeBad := saveCount()
	a.submitMcpAddForm()
	r.check("mcp add rejects non-http url", true, a.settings.backend.mcp.err != "" && saveCount() == savesBeforeBad)
	r.check("mcp slug lowercases and dashes", "new-helper", mcpSlug("New Helper"))
	r.check("mcp slug dedupes with suffix", "new-helper-3", uniqueMcpSlug("New Helper", []string{"new-helper", "new-helper-2"}))
	r.check("mcp url validation accepts https", true, mcpURLValid("https://x.test/mcp"))
	r.check("mcp url validation rejects ftp", false, mcpURLValid("ftp://x.test/mcp"))
	r.check("mcp command splits whitespace", []string{"npx", "-y", "helper"}, mcpParseCommand("  npx  -y helper "))
	tt.Frame()

	a.deleteMcpServer("new-helper-2")
	r.check("mcp delete commits and drops server", true, idle() && a.findMcpServer("new-helper-2") == nil)
	r.check("mcp list keeps remaining servers", []string{"chart", "docs2", "new-helper"}, mcpServerNames(a.settings.backend.mcpServers))
	tt.Frame()
}

func mcpServerNames(servers []settingsMcpServer) []string {
	names := make([]string, 0, len(servers))
	for _, server := range servers {
		names = append(names, server.Name)
	}
	return names
}

func mcpStatusOf(a *app, name string) string {
	for _, status := range a.settings.backend.mcpStatuses {
		if status.Name == name {
			return status.Status
		}
	}
	return ""
}

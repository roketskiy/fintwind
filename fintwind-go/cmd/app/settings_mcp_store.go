package main

import (
	"context"
	"errors"
	"strconv"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
)

// beginSettingsMcp opens the add-server form with the Rust defaults: a local
// server, empty name, and no connection yet.
func (a *app) beginSettingsMcp() {
	st, backend := &a.settings.catalog, &a.settings.backend
	backend.mcpDraftGeneration++
	backend.mcpEditing, backend.removeConfirm = "", ""
	backend.mcpSelected = ""
	st.mcpRenaming, st.mcpVarActive = false, false
	st.mfName, st.mfKind, st.mfCommand, st.mfURL = "", "local", "", ""
	st.mcpAdding = true
	a.closeSettingsMenus()
}

// mcpParseCommand splits the detail/add command field the way the Rust page
// does: whitespace-separated argv, runs of whitespace folded.
func mcpParseCommand(text string) []string {
	return strings.Fields(text)
}

// mcpURLValid mirrors the Rust add-form check: the endpoint must start with
// an http(s) scheme. Full URL syntax is left to the server.
func mcpURLValid(url string) bool {
	return strings.HasPrefix(url, "http://") || strings.HasPrefix(url, "https://")
}

// mcpSlug derives a stable configuration key from a display name the way
// unique_provider_slug does: lowercased ASCII alphanumerics joined by dashes.
func mcpSlug(name string) string {
	var slug strings.Builder
	for _, char := range name {
		switch {
		case char >= 'a' && char <= 'z' || char >= '0' && char <= '9':
			slug.WriteRune(char)
		case char >= 'A' && char <= 'Z':
			slug.WriteRune(char - 'A' + 'a')
		case char == ' ' || char == '-' || char == '_' || char == '.':
			slug.WriteRune('-')
		}
	}
	slugText := strings.Trim(slug.String(), "-")
	if len(slugText) > 40 {
		slugText = slugText[:40]
	}
	slugText = strings.Trim(slugText, "-")
	if slugText == "" {
		return "p"
	}
	return slugText
}

// uniqueMcpSlug appends -2, -3, … until the slug is free on the roster.
func uniqueMcpSlug(name string, taken []string) string {
	base := mcpSlug(name)
	for _, slug := range taken {
		if slug == base {
			goto taken
		}
	}
	return base
taken:
	for index := 2; ; index++ {
		candidate := base + "-" + strconv.Itoa(index)
		free := true
		for _, slug := range taken {
			if slug == candidate {
				free = false
				break
			}
		}
		if free {
			return candidate
		}
	}
}

func equalStringSlices(left, right []string) bool {
	if len(left) != len(right) {
		return false
	}
	for i := range left {
		if left[i] != right[i] {
			return false
		}
	}
	return true
}

// effectiveMcpServerName is the detail target: the stored selection while it
// still resolves, else the first server on the roster.
func (a *app) effectiveMcpServerName() string {
	backend := &a.settings.backend
	for i := range backend.mcpServers {
		if backend.mcpServers[i].Name == backend.mcpSelected {
			return backend.mcpSelected
		}
	}
	if len(backend.mcpServers) != 0 {
		return backend.mcpServers[0].Name
	}
	return ""
}

func (a *app) findMcpServer(name string) *settingsMcpServer {
	backend := &a.settings.backend
	for i := range backend.mcpServers {
		if backend.mcpServers[i].Name == name {
			return &backend.mcpServers[i]
		}
	}
	return nil
}

// selectMcpServer mirrors the Rust select: clear the transient editor state,
// pin the selection, and load the detail fields onto it.
func (a *app) selectMcpServer(name string) {
	backend := &a.settings.backend
	if backend.mcpSelected != name {
		a.cancelSettingsOAuth()
	}
	backend.mcpSelected = name
	backend.mcpEditing, backend.removeConfirm = "", ""
	a.settings.catalog.mcpAdding = false
	a.settings.catalog.mcpRenaming = false
	a.settings.catalog.mcpVarActive = false
	a.syncMcpDetailDrafts(name)
}

// syncMcpDetailDrafts loads the detail's connection fields from the server
// now under selection. The text inputs bind these drafts; the server copy
// only changes through the field handlers below.
func (a *app) syncMcpDetailDrafts(name string) {
	st := &a.settings.catalog
	server := a.findMcpServer(name)
	if server == nil {
		return
	}
	st.mdCommand = strings.Join(server.Command, " ")
	st.mdURL = server.URL
	st.mdClientID, st.mdClientSecret, st.mdScope = server.OAuth.ClientID, server.OAuth.ClientSecret, server.OAuth.Scope
	st.mdRename = server.Name
}

// syncMcpSelectionLocked keeps a valid selection after loads and commits,
// then reloads the detail drafts onto it. Callers hold no lock; the app is
// single-threaded between posts.
func (a *app) syncMcpSelectionLocked() {
	backend := &a.settings.backend
	name := a.effectiveMcpServerName()
	if backend.mcpSelected != name {
		backend.mcpSelected = name
		a.settings.catalog.mcpRenaming = false
		a.settings.catalog.mcpVarActive = false
		backend.removeConfirm = ""
	}
	if name != "" {
		a.syncMcpDetailDrafts(name)
	}
}

// scheduleMcpCommit debounces keystroke-level detail edits so a burst of
// typing costs one save instead of one per key, like the Rust 700ms timer.
// A save already in flight reschedules instead of dropping the edit.
func (a *app) scheduleMcpCommit() {
	st := &a.settings.catalog
	backend := &a.settings.backend
	st.mdCommitGeneration++
	generation := st.mdCommitGeneration
	source, gen := backend.source, backend.generation
	go func() {
		time.Sleep(700 * time.Millisecond)
		a.post(func() {
			if !a.settingsSourceMatches(source, gen) {
				return
			}
			if a.settings.catalog.mdCommitGeneration != generation {
				return
			}
			a.settings.catalog.mdCommitGeneration = 0
			committing := &a.settings.backend
			if !committing.mcp.loaded || committing.mcp.uncertain {
				return
			}
			if committing.mcp.writing {
				a.scheduleMcpCommit()
				return
			}
			roster := append([]settingsMcpServer{}, committing.mcpServers...)
			a.commitSettingsMcp(roster, committing.mcpSelected)
		})
	}()
}

// updateMcpCommandFromDraft applies the detail command field to the selected
// server and debounces the save. Echoes from draft syncs are filtered.
func (a *app) updateMcpCommandFromDraft() {
	name := a.effectiveMcpServerName()
	if name == "" {
		return
	}
	server := a.findMcpServer(name)
	if server == nil {
		return
	}
	command := mcpParseCommand(a.settings.catalog.mdCommand)
	if equalStringSlices(server.Command, command) {
		return
	}
	server.Command = command
	a.scheduleMcpCommit()
}

// updateMcpURLFromDraft applies the detail URL field and debounces the save.
func (a *app) updateMcpURLFromDraft() {
	name := a.effectiveMcpServerName()
	if name == "" {
		return
	}
	server := a.findMcpServer(name)
	if server == nil {
		return
	}
	url := strings.TrimSpace(a.settings.catalog.mdURL)
	if server.URL == url {
		return
	}
	server.URL = url
	a.scheduleMcpCommit()
}

// updateMcpOAuthFieldFromDraft applies one OAuth custom-client field. The
// working copy always updates; only Custom mode schedules the save, so
// edits made under another mode land when Custom is picked.
func (a *app) updateMcpOAuthFieldFromDraft(field string) {
	name := a.effectiveMcpServerName()
	if name == "" {
		return
	}
	server := a.findMcpServer(name)
	if server == nil {
		return
	}
	st := &a.settings.catalog
	switch field {
	case "clientID":
		if server.OAuth.ClientID == st.mdClientID {
			return
		}
		server.OAuth.ClientID = st.mdClientID
	case "clientSecret":
		if server.OAuth.ClientSecret == st.mdClientSecret {
			return
		}
		server.OAuth.ClientSecret = st.mdClientSecret
	case "scope":
		if server.OAuth.Scope == st.mdScope {
			return
		}
		server.OAuth.Scope = st.mdScope
	default:
		return
	}
	if server.OAuth.Mode == "custom" {
		a.scheduleMcpCommit()
	}
}

// setMcpServerKind switches a server between local and remote immediately,
// like the Rust detail dropdown. The stale status no longer describes the
// server, so it is dropped before the refresh.
func (a *app) setMcpServerKind(name, kind string) {
	if kind != "local" && kind != "remote" {
		return
	}
	server := a.findMcpServer(name)
	if server == nil || server.Kind == kind {
		return
	}
	server.Kind = kind
	a.settings.catalog.mcpVarActive = false
	kept := make([]settingsMcpStatus, 0, len(a.settings.backend.mcpStatuses))
	for _, status := range a.settings.backend.mcpStatuses {
		if status.Name != name {
			kept = append(kept, status)
		}
	}
	a.settings.backend.mcpStatuses = kept
	roster := append([]settingsMcpServer{}, a.settings.backend.mcpServers...)
	a.commitSettingsMcp(roster, name)
}

// setMcpOAuthMode switches the remote OAuth mode immediately.
func (a *app) setMcpOAuthMode(name, mode string) {
	if mode != "automatic" && mode != "disabled" && mode != "custom" {
		return
	}
	server := a.findMcpServer(name)
	if server == nil || server.OAuth.Mode == mode {
		return
	}
	server.OAuth.Mode = mode
	roster := append([]settingsMcpServer{}, a.settings.backend.mcpServers...)
	a.commitSettingsMcp(roster, name)
}

// toggleSettingsMcp flips one server's enabled flag optimistically, then
// commits and refreshes its connection state.
func (a *app) toggleSettingsMcp(name string) {
	backend := &a.settings.backend
	if backend.mcp.writing || backend.mcp.uncertain || !backend.mcp.loaded {
		return
	}
	for i := range backend.mcpServers {
		if backend.mcpServers[i].Name == name {
			backend.mcpServers[i].Enabled = !backend.mcpServers[i].Enabled
			break
		}
	}
	roster := append([]settingsMcpServer{}, backend.mcpServers...)
	a.commitSettingsMcp(roster, name)
}

// deleteMcpServer removes a server, drops its status snapshot, and lands
// the detail on the fallback the roster picks next.
func (a *app) deleteMcpServer(name string) {
	backend := &a.settings.backend
	roster := make([]settingsMcpServer, 0, len(backend.mcpServers))
	for _, item := range backend.mcpServers {
		if item.Name != name {
			roster = append(roster, item)
		}
	}
	kept := make([]settingsMcpStatus, 0, len(backend.mcpStatuses))
	for _, status := range backend.mcpStatuses {
		if status.Name != name {
			kept = append(kept, status)
		}
	}
	backend.mcpStatuses = kept
	next := ""
	if len(roster) != 0 {
		next = roster[0].Name
	}
	backend.removeConfirm = ""
	a.settings.catalog.mcpRenaming = false
	a.settings.catalog.mcpVarActive = false
	a.commitSettingsMcp(roster, next)
}

// beginMcpRename opens the inline rename editor on the selected server.
func (a *app) beginMcpRename() {
	name := a.effectiveMcpServerName()
	if name == "" {
		return
	}
	a.settings.catalog.mcpRenaming = true
	a.settings.catalog.mdRename = name
}

func (a *app) cancelMcpRename() {
	a.settings.catalog.mcpRenaming = false
}

// confirmMcpRename rewrites the server's key in mcp.servers. An empty or
// unchanged name cancels; a duplicate name reports the duplicate error.
func (a *app) confirmMcpRename() {
	st := &a.settings.catalog
	backend := &a.settings.backend
	if !st.mcpRenaming {
		return
	}
	name := strings.TrimSpace(st.mdRename)
	st.mcpRenaming = false
	selected := a.effectiveMcpServerName()
	if name == "" || name == selected {
		return
	}
	for _, item := range backend.mcpServers {
		if item.Name == name {
			backend.mcp.err = tr("A server with this name already exists")
			return
		}
	}
	roster := append([]settingsMcpServer{}, backend.mcpServers...)
	for i := range roster {
		if roster[i].Name == selected {
			roster[i].Name = name
		}
	}
	for i := range backend.mcpStatuses {
		if backend.mcpStatuses[i].Name == selected {
			backend.mcpStatuses[i].Name = name
		}
	}
	a.commitSettingsMcp(roster, name)
}

// mcpVarTableFor returns the key-value table of the selected server: the
// environment for local servers, headers for remote ones.
func (a *app) mcpVarTableFor(server settingsMcpServer) (string, [][2]string) {
	if server.Kind == "remote" {
		return "headers", server.Headers
	}
	return "environment", server.Environment
}

func (a *app) beginMcpVariableEditor(table string, index int, isAdd bool) {
	st := &a.settings.catalog
	name := a.effectiveMcpServerName()
	server := a.findMcpServer(name)
	if server == nil {
		return
	}
	st.mcpVarActive, st.mcpVarTable, st.mcpVarIndex, st.mcpVarIsAdd = true, table, index, isAdd
	st.mcpVarKey, st.mcpVarValue = "", ""
	if !isAdd {
		_, pairs := a.mcpVarTableFor(*server)
		if index >= 0 && index < len(pairs) {
			st.mcpVarKey, st.mcpVarValue = pairs[index][0], pairs[index][1]
		}
	}
}

func (a *app) cancelMcpVariableEditor() {
	a.settings.catalog.mcpVarActive = false
}

// confirmMcpVariableEditor saves the inline key-value row. An empty key
// cancels; a duplicate key in the same table reports the duplicate error.
func (a *app) confirmMcpVariableEditor() {
	st := &a.settings.catalog
	if !st.mcpVarActive {
		return
	}
	key := strings.TrimSpace(st.mcpVarKey)
	if key == "" {
		st.mcpVarActive = false
		return
	}
	value := st.mcpVarValue
	name := a.effectiveMcpServerName()
	server := a.findMcpServer(name)
	if server == nil {
		st.mcpVarActive = false
		return
	}
	var pairs *[][2]string
	if st.mcpVarTable == "headers" {
		pairs = &server.Headers
	} else {
		pairs = &server.Environment
	}
	for i, pair := range *pairs {
		if pair[0] == key && (st.mcpVarIsAdd || i != st.mcpVarIndex) {
			a.settings.backend.mcp.err = tr("That key already exists")
			return
		}
	}
	if st.mcpVarIsAdd {
		*pairs = append(*pairs, [2]string{key, value})
	} else if st.mcpVarIndex >= 0 && st.mcpVarIndex < len(*pairs) {
		(*pairs)[st.mcpVarIndex] = [2]string{key, value}
	}
	st.mcpVarActive = false
	roster := append([]settingsMcpServer{}, a.settings.backend.mcpServers...)
	a.commitSettingsMcp(roster, name)
}

// deleteMcpVariable removes one key-value row immediately.
func (a *app) deleteMcpVariable(table string, index int) {
	st := &a.settings.catalog
	name := a.effectiveMcpServerName()
	server := a.findMcpServer(name)
	if server == nil {
		return
	}
	if st.mcpVarActive && !st.mcpVarIsAdd && st.mcpVarTable == table && st.mcpVarIndex == index {
		st.mcpVarActive = false
	}
	if table == "headers" {
		if index >= 0 && index < len(server.Headers) {
			server.Headers = append(server.Headers[:index], server.Headers[index+1:]...)
		}
	} else if index >= 0 && index < len(server.Environment) {
		server.Environment = append(server.Environment[:index], server.Environment[index+1:]...)
	}
	roster := append([]settingsMcpServer{}, a.settings.backend.mcpServers...)
	a.commitSettingsMcp(roster, name)
}

// submitMcpAddForm validates the add form and commits the new server. A name
// clash mints a unique slug the way the Rust form does; an empty name or
// connection never submits.
func (a *app) submitMcpAddForm() {
	st := &a.settings.catalog
	backend := &a.settings.backend
	if !st.mcpAdding {
		return
	}
	name := strings.TrimSpace(st.mfName)
	if name == "" {
		return
	}
	kind := st.mfKind
	if kind != "local" && kind != "remote" {
		backend.mcp.err = tr("Choose a server type")
		return
	}
	var command []string
	url := strings.TrimSpace(st.mfURL)
	switch kind {
	case "local":
		command = mcpParseCommand(st.mfCommand)
		if len(command) == 0 {
			backend.mcp.err = tr("Enter the command that launches the server.")
			return
		}
	case "remote":
		if !mcpURLValid(url) {
			backend.mcp.err = tr("Enter an HTTP or HTTPS URL")
			return
		}
	}
	taken := make([]string, 0, len(backend.mcpServers))
	for _, item := range backend.mcpServers {
		taken = append(taken, item.Name)
	}
	key := uniqueMcpSlug(name, taken)
	server := settingsMcpServer{
		Name: key, Kind: kind, Command: command, URL: url,
		Environment: [][2]string{}, Headers: [][2]string{},
		OAuth:   settingsMcpOAuth{Mode: "automatic"},
		Enabled: true, Raw: map[string]any{},
	}
	roster := append(append([]settingsMcpServer{}, backend.mcpServers...), server)
	a.commitSettingsMcp(roster, key)
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
			a.settings.catalog.mcpRenaming = false
			a.settings.catalog.mcpVarActive = false
			// Keep the selection valid, but leave the detail drafts alone
			// while keystroke edits are still in flight so the sync never
			// clobbers newer typing.
			if st.mcpSelected == previousSelected {
				st.mcpSelected = selected
			}
			if a.findMcpServer(st.mcpSelected) == nil {
				st.mcpSelected = a.effectiveMcpServerName()
				a.settings.catalog.mcpRenaming = false
				a.settings.catalog.mcpVarActive = false
				st.removeConfirm = ""
			}
			if a.settings.catalog.mdCommitGeneration == 0 && st.mcpSelected != "" {
				a.syncMcpDetailDrafts(st.mcpSelected)
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
	form.mcpVarKey, form.mcpVarValue, form.mcpVarActive = "", "", false
	form.mcpRenaming = false
}

func (a *app) installSettingsMarket(entry settingsMarketEntry) {
	st := &a.settings.backend
	for _, server := range st.mcpServers {
		if server.Name == entry.id {
			st.mcpSelected = server.Name
			a.openSettingsPage("mcpServers")
			a.syncMcpDetailDrafts(server.Name)
			return
		}
	}
	server := settingsMcpServer{Name: entry.id, Kind: "remote", URL: entry.url, Enabled: true, Command: []string{}, Environment: [][2]string{}, Headers: [][2]string{}, OAuth: settingsMcpOAuth{Mode: "automatic"}, Raw: map[string]any{}}
	roster := append(append([]settingsMcpServer{}, st.mcpServers...), server)
	a.commitSettingsMcp(roster, server.Name)
}

// mcpStatusLabel maps the daemon's snake_case connection state to the same
// user-facing copy the Rust page shows.
func mcpStatusLabel(status string) string {
	switch status {
	case "connected":
		return tr("Connected")
	case "pending":
		return tr("Pending")
	case "disabled":
		return tr("Disabled")
	case "failed":
		return tr("Failed")
	case "needs_auth":
		return tr("Needs sign-in")
	default:
		return status
	}
}

func mcpOAuthModeOptions() []settingsOption {
	return []settingsOption{
		{label: "Automatic OAuth", value: "automatic"},
		{label: "Headers only", value: "disabled"},
		{label: "Custom OAuth client", value: "custom"},
	}
}

// settingsMcpDetail is the selected server's connection, OAuth, and
// key-value tables, all edited in place like the Rust detail pane.
func (a *app) settingsMcpDetail(c *ui.Context) {
	backend := &a.settings.backend
	catalog := &a.settings.catalog
	name := a.effectiveMcpServerName()
	server := a.findMcpServer(name)
	if server == nil {
		if backend.mcp.loaded && len(backend.mcpServers) == 0 {
			ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
				ui.Column(c).FillWidth().Grow(1).MinHeight(0).Justify(ui.Center).
					AlignItems(ui.Center).Gap(10).Padding(18, 24).Children(func() {
					mailDetailTile(c, "wrench", 20)
					ui.Text(c, tr("No servers")).FontSize(uiPx(13.5)).FontWeight(500).TextColor(palette.text)
					ui.Text(c, tr("Add a server so every new task can use it.")).FontSize(uiPx(11.5)).FixedLineHeight(uiPx(17)).TextColor(palette.textSecondary).Wrap()
					if componentTextButton(c, "mcp-empty-add", tr("Add server"), true).Clicked() {
						a.beginSettingsMcp()
					}
				})
			})
			return
		}
		mailDetailPlaceholder(c, "wrench", tr("Select a server to inspect it"))
		return
	}
	current := *server
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			// Header: tile, name (or rename editor), badge, toggle, delete.
			ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
				mailDetailTile(c, choice(current.Kind == "remote", "globe", "terminal"), 17)
				ui.Column(c).Grow(1).MinWidth(0).Children(func() {
					if catalog.mcpRenaming {
						settingsTextInput(c, "mcp-rename", tr("Name"), &catalog.mdRename, 0, "my-server").
							OnSubmit(a.confirmMcpRename)
						ui.Row(c).Gap(8).Margin(6, 0, 0, 0).Children(func() {
							componentTextButton(c, "mcp-rename-save", tr("Save"), false).
								Disabled(strings.TrimSpace(catalog.mdRename) == "").OnClick(a.confirmMcpRename)
							if componentTextButton(c, "mcp-rename-cancel", tr("Cancel"), false).Clicked() {
								a.cancelMcpRename()
							}
						})
					} else {
						ui.Row(c).Gap(7).AlignItems(ui.Center).Children(func() {
							ui.Text(c, current.Name).FontSize(uiPx(15)).FontWeight(500).
								TextColor(palette.text).SingleLine().Ellipsis("...")
							if componentIconButton(c, "mcp-rename-open", "pencil", true).Size(24, 24).Label(tr("Rename")).Clicked() {
								a.beginMcpRename()
							}
						})
					}
					ui.Row(c).Margin(4, 0, 0, 0).Children(func() {
						mailEnabledBadge(c, current.Enabled, choice(current.Enabled, tr("Enabled"), tr("Disabled")))
					})
				})
				componentTextButton(c, "mcp-toggle", choice(current.Enabled, tr("Disable"), tr("Enable")), false).
					Disabled(backend.mcp.writing || backend.mcp.uncertain).OnClick(func() { a.toggleSettingsMcp(current.Name) })
				deleteArmed := backend.removeConfirm == "mcp:"+current.Name
				componentTextButton(c, "mcp-remove", choice(deleteArmed, tr("Confirm delete"), tr("Remove")), deleteArmed).
					Disabled(backend.mcp.writing).OnClick(func() {
					if deleteArmed {
						a.deleteMcpServer(current.Name)
					} else {
						backend.removeConfirm = "mcp:" + current.Name
					}
				})
			})
			if backend.removeConfirm == "mcp:"+current.Name {
				ui.Text(c, tr("Remove this MCP server configuration?")).FontSize(uiPx(12)).TextColor(palette.text).Wrap()
				ui.Row(c).Gap(8).Children(func() {
					if componentTextButton(c, "mcp-remove-cancel", tr("Cancel"), false).Clicked() {
						backend.removeConfirm = ""
					}
				})
			}
			// Connection: the kind dropdown, then the one field the kind speaks.
			ui.Column(c).FillWidth().Gap(6).Children(func() {
				ui.Text(c, tr("Type")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
				a.settingsChoice(c, "mcp-kind", tr("Server type"), current.Kind, formPickerWidth, mcpKindOptions(), func(option settingsOption) {
					a.setMcpServerKind(current.Name, option.value)
				}).Disabled(backend.mcp.writing || backend.mcp.uncertain)
			})
			if current.Kind == "remote" {
				settingsTextInput(c, "mcp-url", tr("URL"), &catalog.mdURL, 0, "https://example.com/mcp").
					Disabled(backend.mcp.writing).OnChange(a.updateMcpURLFromDraft)
			} else {
				settingsTextInput(c, "mcp-command", tr("Command"), &catalog.mdCommand, 0, "npx -y my-server").
					Disabled(backend.mcp.writing).OnChange(a.updateMcpCommandFromDraft)
			}
			// Status snapshot with the refresh hint.
			known := ""
			var knownErr string
			for _, status := range backend.mcpStatuses {
				if status.Name == current.Name {
					known, knownErr = status.Status, status.Error
					break
				}
			}
			if known != "" {
				ui.Column(c).FillWidth().Gap(4).Margin(2, 0, 0, 0).Children(func() {
					ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
						if glyph, color, ok := mcpStatusGlyph(backend, current.Name); ok {
							icon(c, glyph, 13, color)
						}
						ui.Text(c, mcpStatusLabel(known)).FontSize(uiPx(12)).FontWeight(500).TextColor(palette.text)
					})
					if knownErr != "" {
						ui.Text(c, knownErr).FontSize(uiPx(10.5)).FixedLineHeight(uiPx(15)).TextColor(palette.textTertiary).Wrap()
					}
					ui.Text(c, tr("Connection status refreshes when this page opens or the server changes.")).FontSize(uiPx(10.5)).FixedLineHeight(uiPx(15)).TextColor(palette.textTertiary).Wrap()
				})
			}
			// OAuth (remote only): tri-state mode, custom fields, sign-in.
			if current.Kind == "remote" {
				ui.Column(c).FillWidth().Gap(6).Margin(2, 0, 0, 0).Children(func() {
					ui.Text(c, tr("OAuth")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
					a.settingsChoice(c, "mcp-oauth", tr("OAuth"), current.OAuth.Mode, formPickerWidth, mcpOAuthModeOptions(), func(option settingsOption) {
						a.setMcpOAuthMode(current.Name, option.value)
					}).Disabled(backend.mcp.writing || backend.mcp.uncertain)
					ui.Text(c, tr("Automatic signs in with the browser when needed.")).FontSize(uiPx(10.5)).FixedLineHeight(uiPx(15)).TextColor(palette.textTertiary).Wrap()
				})
				if current.OAuth.Mode == "custom" {
					settingsTextInput(c, "mcp-oauth-client", tr("OAuth client ID"), &catalog.mdClientID, 0, "").
						Disabled(backend.mcp.writing).OnChange(func() { a.updateMcpOAuthFieldFromDraft("clientID") })
					formSecretField(c, "mcp-oauth-secret", tr("OAuth client secret"), &catalog.mdClientSecret, &catalog.mfShowSecret)
					// formSecretField binds the draft directly without an
					// OnChange hook; pick up its edit here, guarded so the
					// sync only fires on an actual change.
					if catalog.mdClientSecret != current.OAuth.ClientSecret {
						a.updateMcpOAuthFieldFromDraft("clientSecret")
					}
					settingsTextInput(c, "mcp-oauth-scope", tr("OAuth scope"), &catalog.mdScope, 0, "").
						Disabled(backend.mcp.writing).OnChange(func() { a.updateMcpOAuthFieldFromDraft("scope") })
				}
				a.settingsMcpAuth(c, current)
			}
			// Key-value table: environment for local, headers for remote.
			a.mcpVariablesCard(c, current)
			ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
				icon(c, "info", 12, palette.textTertiary)
				ui.Text(c, tr("This roster is mcp.servers in OpenCode's own configuration file (opencode.json). The CLI and TUI see the same servers, and OpenCode hot-reloads the file. Extra fields like timeout, cwd, or oauth are preserved.")).FontSize(uiPx(10.5)).FixedLineHeight(uiPx(15)).TextColor(palette.textTertiary).Wrap()
			})
			settingsOperationStatus(c, backend.mcp, "")
			if backend.mcpStatusError != "" {
				ui.Text(c, backend.mcpStatusError).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
			}
		})
	})
}

// mcpVariablesCard renders the selected server's key-value table with the
// inline add/edit row, mirroring the Rust bordered card.
func (a *app) mcpVariablesCard(c *ui.Context, server settingsMcpServer) {
	catalog := &a.settings.catalog
	table, pairs := a.mcpVarTableFor(server)
	title := tr("Environment variables")
	addLabel := tr("Add variable")
	if table == "headers" {
		title, addLabel = tr("Headers"), tr("Add header")
	}
	mailSectionLabel(c, title, true)
	ui.Column(c).FillWidth().Margin(8, 0, 0, 0).Radius(9).Border(1, palette.border).Clip().Children(func() {
		if len(pairs) == 0 && !(catalog.mcpVarActive && catalog.mcpVarTable == table && catalog.mcpVarIsAdd) {
			ui.Row(c).FillWidth().Padding(12, 10).Gap(8).AlignItems(ui.Center).Children(func() {
				icon(c, "info", 12, palette.textTertiary)
				ui.Text(c, tr("No entries yet")).FontSize(uiPx(10.5)).FixedLineHeight(uiPx(15)).TextColor(palette.textTertiary)
			})
		}
		for i, pair := range pairs {
			i, pair := i, pair
			if catalog.mcpVarActive && catalog.mcpVarTable == table && !catalog.mcpVarIsAdd && catalog.mcpVarIndex == i {
				a.mcpVariableEditorRow(c, table, true)
				continue
			}
			row := ui.Row(c).FillWidth().MinHeight(38).Padding(0, 10).Gap(8).AlignItems(ui.Center)
			if i > 0 {
				row.BorderWidth(1, 0, 0, 0).BorderColor(palette.border)
			}
			row.Children(func() {
				ui.Text(c, pair[0]).Font(monoFont()).FontSize(uiPx(11)).TextColor(palette.text).
					SingleLine().Ellipsis("...").MaxWidth(180)
				ui.Text(c, pair[1]).Font(monoFont()).FontSize(uiPx(9.5)).TextColor(palette.textTertiary).
					Grow(1).MinWidth(0).SingleLine().Ellipsis("...")
				if componentIconButton(c, "mcp-var-edit", "pencil", true).Size(24, 24).Label(tr("Edit variable")).Clicked() {
					a.beginMcpVariableEditor(table, i, false)
				}
				if componentIconButton(c, "mcp-var-del", "trash", true).Size(24, 24).Label(tr("Delete variable")).Clicked() {
					a.deleteMcpVariable(table, i)
				}
			})
		}
		if catalog.mcpVarActive && catalog.mcpVarTable == table && catalog.mcpVarIsAdd {
			a.mcpVariableEditorRow(c, table, len(pairs) != 0)
		} else {
			add := ui.ButtonBase(c).Label(addLabel).FillWidth().MinHeight(36).Padding(0, 10).
				Gap(7).Justify(ui.Start).AlignItems(ui.Center)
			if len(pairs) != 0 {
				add.BorderWidth(1, 0, 0, 0).BorderColor(palette.border)
			}
			add.Children(func() {
				icon(c, "plus", 12, palette.textTertiary)
				ui.Text(c, addLabel).FontSize(uiPx(12)).TextColor(palette.textSecondary)
			})
			if add.Clicked() {
				a.beginMcpVariableEditor(table, 0, true)
			}
		}
	})
}

// mcpVariableEditorRow is the inline key/value editor with Save/Cancel,
// reusing the shared form actions.
func (a *app) mcpVariableEditorRow(c *ui.Context, table string, separated bool) {
	catalog := &a.settings.catalog
	row := ui.Column(c).FillWidth().Padding(8, 10).Gap(8).Background(palette.composer)
	if separated {
		row.BorderWidth(1, 0, 0, 0).BorderColor(palette.border)
	}
	row.Children(func() {
		settingsTextInput(c, "mcp-var-key", tr("Key"), &catalog.mcpVarKey, 0, "NAME")
		settingsTextInput(c, "mcp-var-value", tr("Value"), &catalog.mcpVarValue, 0, "value")
		formActions(c, "mcp-var", tr("Save"), strings.TrimSpace(catalog.mcpVarKey) == "", a.cancelMcpVariableEditor, a.confirmMcpVariableEditor)
		_ = table
	})
}

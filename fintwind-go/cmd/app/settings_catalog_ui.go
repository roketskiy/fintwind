package main

import (
	"fmt"
	"strings"

	"github.com/egoist/mygo/ui"
	lobechoice "github.com/lobehub/lobe-mygo/pkg/choice"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

type settingsCatalogUIState struct {
	providerAdding    bool
	providerModelID   string
	providerModelList ui.ListState
	pfName            string
	pfBaseURL         string
	pfAPIKey          string
	pfFormat          string
	pfShowKey         bool
	pfModels          []*settingsProviderModelDraft
	pfModelSerial     uint64
	pfModelsReady     bool
	pfModelList       ui.ListState
	skillQuery        string
	skillSource       string
	mcpQuery          string
	mcpAdding         bool
	mfName            string
	mfKind            string
	mfCommand         string
	mfURL             string
	mfEnvironment     string
	mfHeaders         string
	mfOAuth           string
	mfClientID        string
	mfClientSecret    string
	mfScope           string
	mfShowSecret      bool
	// MCP detail drafts mirror the selected server's editable fields. They
	// are synced on selection and after loads/commits; keystroke edits
	// update the working roster immediately and debounce the save.
	mdCommand      string
	mdURL          string
	mdClientID     string
	mdClientSecret string
	mdScope        string
	mdRename       string
	mcpRenaming    bool
	// Inline variable editor: active table ("environment"/"headers"), row
	// index for edits, and key/value drafts. mcpVarIsAdd selects the add
	// row; otherwise mcpVarIndex edits that table row.
	mcpVarActive bool
	mcpVarTable  string
	mcpVarIndex  int
	mcpVarIsAdd  bool
	mcpVarKey    string
	mcpVarValue  string
	mdCommitGeneration uint64
	marketQuery  string
	marketCat    string
	marketSel    string
	marketList   ui.ListState
}

// settingsFilterWidth is the full inner width of each roster pane: the
// 264 list minus its horizontal padding (20 skills, 24 MCP/market).
const (
	skillsFilterWidth = 244.0
	mcpFilterWidth    = 240.0
	formPickerWidth   = 280.0
)

// skillSourceOptions mirrors the Rust source filter: every source first,
// then the two install roots. Counts need the scan and stay out.
func skillSourceOptions() []settingsOption {
	return []settingsOption{
		{label: "All sources", value: "", raw: "package"},
		{label: "Shared", value: "shared", raw: "package"},
		{label: "OpenCode", value: "opencode", raw: "package"},
	}
}

// marketCategoryOptions mirrors McpMarketCategory::FILTERS in display order.
func marketCategoryOptions() []settingsOption {
	return []settingsOption{
		{label: "All categories", value: ""},
		{label: "Web search", value: "web"},
		{label: "Academic", value: "academic"},
		{label: "Code", value: "code"},
		{label: "Office", value: "office"},
		{label: "Design", value: "design"},
		{label: "Data", value: "data"},
		{label: "Automation", value: "automation"},
		{label: "DevOps", value: "devops"},
		{label: "Business", value: "business"},
	}
}

func (a *app) providersListPane(c *ui.Context) {
	st := &a.settings.catalog
	backend := &a.settings.backend
	mailPaneHeader(c, "Providers")
	ui.Column(c).FillWidth().Padding(6, 8, 0, 8).Gap(1).Children(func() {
		mailSectionLabel(c, "Built-in", true)
		a.mailRow(c, "OpenCode", !st.providerAdding && backend.providerSelected == "", func() { a.cancelProviderAdd(); backend.providerSelected = "" }, func() {
			mailTile(c, "bot", !st.providerAdding && backend.providerSelected == "")
		}, func() {
			mailStatusDot(c, a.modelProbe != nil && a.modelProbe.Installed)
		})
		mailSectionLabel(c, "Custom", false)
		settingsOperationStatus(c, backend.provider, "")
		if backend.provider.loaded && len(backend.providers) == 0 {
			mailStatusRow(c, "No custom providers")
		}
	})
	c.Key("settings-custom-provider-list")
	ui.List(c, &backend.providerList, len(backend.providers)).FillWidth().Grow(1).MinHeight(0).ItemKey(func(i int) any { return backend.providers[i].ID }).Rows(func(row ui.ListRow) {
		provider, c := backend.providers[row.Index], row.Context
		ui.Column(c).FillWidth().Padding(0, 8).Children(func() {
			a.mailRow(c, settingsProviderName(provider), backend.providerSelected == provider.ID, func() { a.cancelProviderAdd(); backend.providerSelected = provider.ID }, func() { mailTile(c, "bot", backend.providerSelected == provider.ID) }, func() { mailEnabledBadge(c, provider.Enabled, choice(provider.Enabled, "Enabled", "Disabled")) })
		})
	})
	ui.Column(c).FillWidth().Padding(0, 8, 8, 8).Gap(4).Children(func() {
		if mailAddRow(c, "Add provider", st.providerAdding) {
			a.beginSettingsProvider(nil)
		}
		componentTextButton(c, "settings-providers-reload", tr("Reload"), false).Disabled(backend.provider.loading || backend.provider.writing).OnClick(a.loadSettingsProviders)
	})
	caption := "Not loaded"
	if backend.provider.loaded {
		caption = fmt.Sprintf("%d custom providers", len(backend.providers))
	}
	mailFooter(c, caption, false)
}

func (a *app) skillsListPane(c *ui.Context) {
	st := &a.settings.catalog
	ui.Column(c).FillWidth().Padding(22, 10, 8, 10).Gap(7).Children(func() {
		settingsSearchInput(c, "skills-search", tr("Search skills"), &st.skillQuery)
		a.settingsChoice(c, "skills-source-filter", tr("Skill source"), st.skillSource, skillsFilterWidth, skillSourceOptions(), func(option settingsOption) {
			st.skillSource = option.value
		})
	})
	backend := &a.settings.backend
	settingsOperationStatus(c, backend.skill, "")
	visible := make([]settingsSkill, 0, len(backend.skills))
	for _, skill := range backend.skills {
		if !settingsMatches(st.skillQuery, skill.Name+" "+skill.Description+" "+skill.Project) {
			continue
		}
		match := st.skillSource == ""
		for _, install := range skill.Installs {
			if strings.EqualFold(install.Source, st.skillSource) {
				match = true
			}
		}
		if match {
			visible = append(visible, skill)
		}
	}
	c.Key("settings-skill-list")
	ui.List(c, &backend.skillList, len(visible)).FillWidth().Grow(1).MinHeight(0).ItemKey(func(i int) any { return visible[i].key() }).Rows(func(row ui.ListRow) {
		skill := visible[row.Index]
		c := row.Context
		a.mailRow(c, skill.Name, backend.skillSelected == skill.key(), func() { backend.skillSelected = skill.key(); backend.skillTrashConfirm = false }, func() { mailTile(c, "package", backend.skillSelected == skill.key()) }, func() { mailEnabledBadge(c, skill.Enabled, choice(skill.Enabled, "Enabled", "Disabled")) })
	})
	componentTextButton(c, "settings-skills-reload", tr("Reload"), false).Disabled(backend.skill.loading || backend.skill.writing).OnClick(a.loadSettingsSkills)
	caption := "Not loaded"
	if backend.skill.loaded {
		caption = fmt.Sprintf("%d skills", len(backend.skills))
	}
	mailFooter(c, caption, true)
}

// mcpListPane is the MCP roster: live search, Local/Remote sections hidden
// while empty as in Rust, then the add row.
func (a *app) mcpListPane(c *ui.Context) {
	st := &a.settings.catalog
	backend := &a.settings.backend
	mailPaneHeader(c, tr("MCP Servers"))
	ui.Column(c).FillWidth().Padding(10, 12, 0, 12).Children(func() {
		settingsSearchInput(c, "mcp-search", tr("Search servers"), &st.mcpQuery)
	})
	settingsOperationStatus(c, backend.mcp, "")
	matches := mcpServerMatchesFunc(st.mcpQuery)
	// Sections render as plain columns (not a virtual list) so Local/Remote
	// headers can hide while empty like the Rust page. The roster stays
	// small; virtualization would complicate section headers.
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(0, 8, 8, 8).Gap(1).Children(func() {
			selected := backend.mcpSelected
			if st.mcpAdding {
				selected = ""
			}
			for _, section := range []struct {
				key  string
				kind string
			}{
				{tr("Local"), "local"},
				{tr("Remote"), "remote"},
			} {
				visible := make([]settingsMcpServer, 0)
				for _, server := range backend.mcpServers {
					if server.Kind == section.kind && matches(server) {
						visible = append(visible, server)
					}
				}
				if len(visible) == 0 {
					continue
				}
				mailSectionLabel(c, section.key, false)
				for _, server := range visible {
					server := server
					a.mcpRow(c, server, selected == server.Name)
				}
			}
			if mailAddRow(c, tr("Add server"), st.mcpAdding) {
				a.beginSettingsMcp()
			}
		})
	})
	componentTextButton(c, "settings-mcp-reload", tr("Reload"), false).Disabled(backend.mcp.loading || backend.mcp.writing).OnClick(a.loadSettingsMcp)
	caption := tr("Not loaded")
	if backend.mcp.loaded {
		count := len(backend.mcpServers)
		caption = fmt.Sprintf("%d servers", count)
		if count == 0 {
			caption = tr("No servers")
		}
	}
	mailFooter(c, caption, false)
}

// mcpServerMatches mirrors the Rust roster filter: a server stays visible
// when the trimmed, case-insensitive query hits its name, command, or URL.
func mcpServerMatchesFunc(query string) func(settingsMcpServer) bool {
	return func(server settingsMcpServer) bool {
		return mcpServerMatches(query, server)
	}
}

// mcpServerMatches is the testable predicate behind mcpServerMatchesFunc.
func mcpServerMatches(query string, server settingsMcpServer) bool {
	return settingsMatches(query, server.Name+" "+strings.Join(server.Command, " ")+" "+server.URL)
}

// mcpRow is one roster entry: tile, name over a mono connection caption,
// status glyph when known, and an enable switch. Geometry reuses the mail
// row (px9 py7, r8, gap9, selected on sidebarItem).
func (a *app) mcpRow(c *ui.Context, server settingsMcpServer, selected bool) {
	backend := &a.settings.backend
	caption := mcpRowCaption(server)
	tileIcon := choice(server.Kind == "remote", "globe", "terminal")
	row := ui.ButtonBase(c).Label(server.Name).FillWidth().Padding(7, 9).
		Radius(8).Gap(9).Justify(ui.Start).AlignItems(ui.Center)
	if selected {
		row.Background(palette.sidebarItem)
	} else {
		if row.Hovered() {
			row.Background(palette.overlay)
		}
		if row.Pressed() {
			row.Background(palette.overlayStrong)
		}
	}
	row.Children(func() {
		mailTile(c, tileIcon, selected)
		ui.Column(c).Grow(1).MinWidth(0).Children(func() {
			color := palette.textSecondary
			if selected || server.Enabled {
				color = palette.text
			}
			ui.Text(c, server.Name).FontSize(uiPx(12.5)).FontWeight(500).
				TextColor(color).SingleLine().Ellipsis("...")
			ui.Text(c, caption).Font(monoFont()).FontSize(uiPx(10.5)).
				TextColor(palette.textTertiary).SingleLine().Ellipsis("...").Margin(1, 0, 0, 0)
		})
		if glyph, color, ok := mcpStatusGlyph(backend, server.Name); ok {
			icon(c, glyph, 12, color)
		}
		c.Key("mcp-toggle-" + server.Name)
		enabled := server.Enabled
		lobechoice.Switch(c, &enabled, lobechoice.SwitchProps{Size: theme.Small}).Label(server.Name).Disabled(backend.mcp.writing || backend.mcp.uncertain).OnChange(func() {
			a.toggleSettingsMcp(server.Name)
		})
	})
	if row.Clicked() {
		a.selectMcpServer(server.Name)
	}
}

// mcpRowCaption mirrors the Rust row caption: the command it launches or
// the endpoint it serves, else the no-connection copy.
func mcpRowCaption(server settingsMcpServer) string {
	if server.Kind == "local" && len(server.Command) != 0 {
		return strings.Join(server.Command, " ")
	}
	if server.Kind == "remote" && strings.TrimSpace(server.URL) != "" {
		return strings.TrimSpace(server.URL)
	}
	return tr("No command or URL yet")
}

// mcpStatusGlyph maps the daemon's snake_case connection state to the same
// glyph/color pairs the Rust page uses. Unknown/absent states hide the dot.
func mcpStatusGlyph(backend *settingsBackendState, name string) (string, ui.Color, bool) {
	for _, status := range backend.mcpStatuses {
		if status.Name != name {
			continue
		}
		switch status.Status {
		case "connected":
			return "check", palette.success, true
		case "pending":
			return "loader-circle", palette.accent, true
		case "disabled":
			return "block", palette.textTertiary, true
		case "failed":
			return "alert", palette.danger, true
		case "needs_auth":
			return "lock", palette.danger, true
		}
		return "", ui.Color{}, false
	}
	return "", ui.Color{}, false
}

func (a *app) marketListPane(c *ui.Context) {
	a.settingsMarketList(c)
}

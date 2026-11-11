package main

import (
	"fmt"
	"github.com/egoist/mygo/ui"
	"strings"
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
	marketQuery       string
	marketCat         string
	marketSel         string
	marketList        ui.ListState
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
	a.providersListPaneFull(c)
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
	mailPaneHeader(c, "MCP Servers")
	ui.Column(c).FillWidth().Padding(10, 12, 0, 12).Children(func() {
		settingsSearchInput(c, "mcp-search", tr("Search servers"), &st.mcpQuery)
	})
	settingsOperationStatus(c, backend.mcp, "")
	visible := make([]settingsMcpServer, 0, len(backend.mcpServers))
	for _, server := range backend.mcpServers {
		if settingsMatches(st.mcpQuery, server.Name+" "+server.Kind+" "+server.URL+" "+strings.Join(server.Command, " ")) {
			visible = append(visible, server)
		}
	}
	c.Key("settings-mcp-list")
	ui.List(c, &backend.mcpList, len(visible)).FillWidth().Grow(1).MinHeight(0).ItemKey(func(i int) any { return visible[i].Name }).Rows(func(row ui.ListRow) {
		server := visible[row.Index]
		c := row.Context
		a.mailRow(c, server.Name, backend.mcpSelected == server.Name && !st.mcpAdding, func() {
			if backend.mcpSelected != server.Name {
				a.cancelSettingsOAuth()
			}
			a.clearSettingsMcpSecrets()
			st.mcpAdding = false
			backend.mcpSelected = server.Name
			backend.removeConfirm = ""
		}, func() { mailTile(c, "wrench", backend.mcpSelected == server.Name) }, func() { mailEnabledBadge(c, server.Enabled, choice(server.Enabled, "Enabled", "Disabled")) })
	})
	if mailAddRow(c, "Add server", st.mcpAdding) {
		a.beginSettingsMcp(nil)
	}
	componentTextButton(c, "settings-mcp-reload", tr("Reload"), false).Disabled(backend.mcp.loading || backend.mcp.writing).OnClick(a.loadSettingsMcp)
	caption := "Not loaded"
	if backend.mcp.loaded {
		caption = fmt.Sprintf("%d servers", len(backend.mcpServers))
	}
	mailFooter(c, caption, false)
}

func (a *app) marketListPane(c *ui.Context) {
	a.settingsMarketList(c)
}

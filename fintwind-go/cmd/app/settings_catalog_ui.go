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

// skillSourceOptions mirrors the GPUI source filter: every source first,
// then the two install roots. Counts come from the loaded catalog so an
// empty pick is never a surprise.
func skillSourceOptions(counts map[string]int) []settingsOption {
	option := func(value, label string) settingsOption {
		text := label
		if counts != nil {
			if n := counts[value]; n > 0 {
				text = label + " · " + fmt.Sprint(n)
			}
		}
		return settingsOption{label: text, value: value, raw: value}
	}
	return []settingsOption{
		{label: tr("All skills"), value: "", raw: ""},
		option("shared", tr("Shared")),
		option("opencode", "OpenCode"),
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
	backend := &a.settings.backend
	ui.Column(c).FillWidth().Padding(22, 10, 8, 10).Gap(7).Children(func() {
		settingsSearchInput(c, "skills-search", tr("Search skills…"), &st.skillQuery)
		a.settingsChoice(c, "skills-source-filter", tr("Skill source"), st.skillSource, skillsFilterWidth, skillSourceOptions(skillsSourceCounts(backend.skills)), func(option settingsOption) {
			st.skillSource = option.value
		})
	})
	settingsOperationStatus(c, backend.skill, "")
	// Mirror render_skills_settings: an unloaded catalog is a scanning
	// state, an empty catalog is an empty state, otherwise the grouped
	// roster. Query and source filter compose over catalog order.
	if !backend.skill.loaded {
		mailStatusRow(c, tr("Scanning skill folders…"))
	}
	visible := skillsVisible(backend.skills, st.skillQuery, st.skillSource)
	effective := skillsEffectiveSelection(visible, backend.skillSelected)
	effectiveKey := ""
	if effective != nil {
		effectiveKey = effective.key()
	}
	type skillRow struct {
		key, section string
		first        bool
		skill        settingsSkill
	}
	rows := make([]skillRow, 0, len(visible))
	var current string
	var sectionStart = -1
	for _, skill := range visible {
		label := skillProjectLabel(skill)
		if sectionStart == -1 || label != current {
			current = label
			sectionStart = len(rows)
			rows = append(rows, skillRow{key: "section-" + label, section: label, first: len(rows) == 0})
		}
		rows = append(rows, skillRow{key: skill.key(), skill: skill})
	}
	if len(rows) != 0 {
		// Backfill each section header with its skill count, the way
		// backfill_section_count does after its span is known.
		for i := 0; i < len(rows); i++ {
			if rows[i].section == "" {
				continue
			}
			count := 0
			for j := i + 1; j < len(rows) && rows[j].section == ""; j++ {
				count++
			}
			rows[i].section = rows[i].section + " " + fmt.Sprint(count)
		}
	}
	switch {
	case !backend.skill.loaded && len(visible) == 0:
		// Scanning status above stands in for the roster.
	case len(backend.skills) == 0 && backend.skill.loaded:
		mailStatusRow(c, tr("No skills yet"))
	case len(rows) == 0:
		mailStatusRow(c, tr("No skills match your search"))
	default:
		c.Key("settings-skill-list")
		ui.List(c, &backend.skillList, len(rows)).FillWidth().Grow(1).MinHeight(0).ItemKey(func(i int) any { return rows[i].key }).Rows(func(row ui.ListRow) {
			item := rows[row.Index]
			c := row.Context
			ui.Column(c).FillWidth().Padding(0, 8).Children(func() {
				if item.section != "" {
					mailSectionLabel(c, item.section, item.first)
					return
				}
				a.skillRosterRow(c, item.skill, effectiveKey == item.skill.key())
			})
		})
	}
	componentTextButton(c, "settings-skills-reload", tr("Reload"), false).Disabled(backend.skill.loading || backend.skill.writing).OnClick(a.loadSettingsSkills)
	filtered := strings.TrimSpace(st.skillQuery) != "" || st.skillSource != ""
	mailFooter(c, skillsFooterCaption(visible, backend, filtered), true)
}

// skillRosterRow is one roster entry: the 28 tile, the 12.5 medium name,
// and the 11.0 tertiary description-or-sources line. A disabled skill dims
// its name and carries a Disabled badge, so the state never reads as
// color-only.
func (a *app) skillRosterRow(c *ui.Context, skill settingsSkill, selected bool) {
	backend := &a.settings.backend
	row := ui.ButtonBase(c).Label(skill.Name).FillWidth().Padding(7, 9).
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
		mailTile(c, skillIconName(skill), selected)
		ui.Column(c).Grow(1).MinWidth(0).Children(func() {
			ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
				name := ui.Text(c, skill.Name).FontSize(uiPx(12.5)).FontWeight(500).
					SingleLine().Ellipsis("...")
				if skill.Enabled {
					name.TextColor(palette.text)
				} else {
					name.TextColor(palette.textSecondary)
				}
				if !skill.Enabled {
					mailEnabledBadge(c, false, tr("Disabled"))
				}
			})
			sub := skill.Description
			if strings.TrimSpace(sub) == "" {
				sub = skillSourcesLabel(skill)
			}
			ui.Text(c, sub).FontSize(uiPx(11)).TextColor(palette.textTertiary).
				SingleLine().Ellipsis("...").Margin(1, 0, 0, 0)
		})
	})
	if row.Clicked() {
		backend.skillSelected = skill.key()
		backend.skillTrashConfirm = false
	}
}

// skillsFooterCaption mirrors the GPUI roster caption: the shown/total
// filter caption while a query or source filter is active, else the skill
// count plus the disabled count.
func skillsFooterCaption(visible []settingsSkill, backend *settingsBackendState, filtered bool) string {
	if !backend.skill.loaded {
		return tr("Scanning skill folders…")
	}
	total := len(backend.skills)
	if filtered {
		shown := fmt.Sprint(len(visible))
		caption := strings.ReplaceAll(tr("%{shown} of %{total} shown"), "%{shown}", shown)
		return strings.ReplaceAll(caption, "%{total}", fmt.Sprint(total))
	}
	if total == 1 {
		return tr("1 skill")
	}
	caption := strings.ReplaceAll(tr("%{count} skills"), "%{count}", fmt.Sprint(total))
	disabled := 0
	for _, skill := range backend.skills {
		if !skill.Enabled {
			disabled++
		}
	}
	if disabled > 0 {
		caption += " · " + strings.ReplaceAll(tr("%{count} disabled"), "%{count}", fmt.Sprint(disabled))
	}
	return caption
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

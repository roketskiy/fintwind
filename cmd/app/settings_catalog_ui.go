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
	mcpVarActive       bool
	mcpVarTable        string
	mcpVarIndex        int
	mcpVarIsAdd        bool
	mcpVarKey          string
	mcpVarValue        string
	mdCommitGeneration uint64
	marketQuery        string
	marketCat          string
	marketSel          string
	marketList         ui.ListState
	schedQuery         string
	schedFilter        string
	schedAdding        bool
	schedTitle         string
	schedPrompt        string
	schedSchedule      string
	schedTime          string
	schedWeekdays      uint8
	schedDate          string
	schedInterval      string
	schedTarget        string
	schedProject       string
	schedSession       string
	schedModel         string
	schedAccess        string
	schedEnabled       bool
}

// settingsFilterWidth is the full inner width of each roster pane: the
// 264 list minus its horizontal padding (20 skills, 24 MCP/market).
const (
	skillsFilterWidth = 244.0
	mcpFilterWidth    = 240.0
	formPickerWidth   = 280.0
)

func (a *app) skillScopeOptions() []settingsOption {
	options := []settingsOption{{label: tr("Global"), value: "", raw: ""}}
	for _, project := range a.projects {
		if (project.EnvironmentID == "" || project.EnvironmentID == settingsLocalEnvironment) && project.Path != "" {
			options = append(options, settingsOption{label: project.Name, value: project.Path, raw: project.Path})
		}
	}
	return options
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
	options := a.skillScopeOptions()
	selectedScope := false
	for _, option := range options {
		if option.value == st.skillSource {
			selectedScope = true
			break
		}
	}
	if !selectedScope {
		st.skillSource = ""
		backend.skillSelected = ""
	}
	ui.Column(c).FillWidth().Padding(22, 10, 8, 10).Gap(7).Children(func() {
		settingsSearchInput(c, "skills-search", tr("Search skills…"), &st.skillQuery)
		a.settingsChoice(c, "skills-source-filter", tr("Skills"), st.skillSource, skillsFilterWidth, options, func(option settingsOption) {
			st.skillSource = option.value
			backend.skillSelected = ""
			backend.skillTrashConfirm = false
		})
	})
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
	case len(skillsVisible(backend.skills, "", st.skillSource)) == 0 && backend.skill.loaded:
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
	filtered := strings.TrimSpace(st.skillQuery) != ""
	mailFooter(c, skillsFooterCaption(visible, skillsVisible(backend.skills, "", st.skillSource), backend.skill.loaded, filtered), true)
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
		if row.Hovered() || row.Focused() {
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
	if row.Clicked() || (row.Focused() && (c.Shortcut(0, ui.KeyEnter) || c.Shortcut(0, ui.KeySpace))) {
		backend.skillSelected = skill.key()
		backend.skillTrashConfirm = false
	}
}

// skillsFooterCaption mirrors the GPUI roster caption: the shown/total
// filter caption while a query or source filter is active, else the skill
// count plus the disabled count.
func skillsFooterCaption(visible, skills []settingsSkill, loaded, filtered bool) string {
	if !loaded {
		return tr("Scanning skill folders…")
	}
	total := len(skills)
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
	for _, skill := range skills {
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
	mailPaneHeader(c, tr("MCP Servers"))
	ui.Column(c).FillWidth().Padding(10, 12, 0, 12).Children(func() {
		settingsSearchInput(c, "mcp-search", tr("Search servers"), &st.mcpQuery)
	})
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
		if row.Hovered() || row.Focused() {
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
	if row.Clicked() || (row.Focused() && (c.Shortcut(0, ui.KeyEnter) || c.Shortcut(0, ui.KeySpace))) {
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

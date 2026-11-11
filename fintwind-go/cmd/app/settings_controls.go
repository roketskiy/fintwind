package main

import (
	"strings"

	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

type settingsChoiceState struct {
	value, source string
	open          bool
	// searchable menus (font pickers) filter their rows by this text.
	searchable bool
	search     string
	wasOpen    bool
}

func (a *app) openSettingsPage(page string) {
	if a.settings.page != page {
		a.cancelSettingsOAuth()
		a.clearSettingsMcpSecrets()
		a.settings.catalog.mcpAdding = false
		a.settings.catalog.mcpRenaming = false
		a.settings.catalog.mcpVarActive = false
	}
	a.cancelProviderAdd()
	a.mode = modeSettings
	a.settings.page = page
	a.settings.scroll = ui.ScrollState{}
	a.ensureSettingsLoaded()
	a.loadSettingsCatalog(page)
	if page == "general" {
		a.loadRuntimeConfig()
		a.detectSettingsShells()
		runtime := &a.settings.runtime
		if len(runtime.models) == 0 && !runtime.modelsLoading {
			a.loadRuntimeImageModels()
		}
		// Like Rust's open_settings_page: check for an OpenCode update once.
		general := &a.settings.backend.general
		if !general.operation.loaded && !general.operation.writing {
			a.manageSettingsOpenCode("check")
		}
	}
	if page == "appearance" {
		a.detectSettingsShells()
	}
	if page == "providers" && a.modelProbe == nil && !a.modelProbeLoad {
		a.requestModelProbe()
	}
}

func (a *app) cancelProviderAdd() {
	a.settings.backend.providerDraftGeneration++
	st := &a.settings.catalog
	st.providerAdding, st.pfShowKey = false, false
	st.pfAPIKey = ""
	a.settings.backend.integrationKey, a.settings.backend.integrationShowKey = "", false
	a.settings.backend.providerEditing = ""
	a.settings.backend.removeConfirm = ""
	a.closeSettingsMenus()
}

func (a *app) closeSettingsMenus() {
	for _, menu := range a.settings.menus {
		menu.open = false
	}
}

func settingsTextInput(c *ui.Context, key, label string, value *string, width float32, placeholder string) ui.Element {
	c.Key(key + "-shell")
	var input ui.Element
	field := ui.Row(c).Height(32).Padding(0, 9).Radius(7).
		Border(1, palette.borderStrong).Background(palette.composer).AlignItems(ui.Center)
	if width > 0 {
		field.Width(width).Shrink(0)
	} else {
		field.FillWidth()
	}
	field.Children(func() {
		c.Key(key)
		input = ui.TextInputBase(c, value).Label(label).Placeholder(placeholder).
			FillWidth().MinWidth(0).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(17)).TextColor(palette.text)
		if input.Focused() {
			field.Border(1, palette.accent)
		}
	})
	return input
}

func settingsSearchInput(c *ui.Context, key, placeholder string, value *string) {
	c.Key(key + "-shell")
	ui.Row(c).FillWidth().Height(32).Padding(0, 9).Radius(7).
		Border(1, palette.borderStrong).Background(palette.composer).
		Gap(7).AlignItems(ui.Center).Children(func() {
		icon(c, "search", 13, palette.textTertiary)
		c.Key(key)
		ui.TextInputBase(c, value).Label(placeholder).Placeholder(placeholder).
			Grow(1).MinWidth(0).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(17)).TextColor(palette.text).
			OnTextKey(func(k ui.TextKey) bool {
				if k.Key != ui.KeyEscape || k.Text() == "" {
					return false
				}
				k.SetText("")
				return true
			})
		if *value != "" {
			if componentIconButton(c, key+"-clear", "x", true).Size(20, 24).Label(tr("Clear search")).Clicked() {
				*value = ""
			}
		}
	})
}

// settingsSearchDropdown mirrors settingsDropdown for long font menus: the
// popup carries a filter field over the rows.
func (a *app) settingsSearchDropdown(c *ui.Context, id, label, key, current string, width float32, options []settingsOption) {
	a.settingsSearchChoice(c, id, tr(label), current, width, options, func(option settingsOption) {
		if option.value == current {
			return
		}
		a.savePreference(key, option.raw)
	})
}

// settingsSearchChoice mirrors settingsChoice with a row filter. Only the
// font pickers use it; every other menu keeps the plain popup.
func (a *app) settingsSearchChoice(c *ui.Context, id, label, current string, width float32, options []settingsOption, onSelect func(settingsOption)) ui.Element {
	if a.settings.menus == nil {
		a.settings.menus = map[string]*settingsChoiceState{}
	}
	menu := a.settings.menus[id]
	if menu == nil {
		menu = &settingsChoiceState{value: current, source: current}
		a.settings.menus[id] = menu
	}
	menu.searchable = true
	return a.settingsChoice(c, id, label, current, width, options, onSelect)
}

// filterSettingsOptions keeps rows whose label or value contains the query,
// case-insensitive. An empty query keeps every row.
func filterSettingsOptions(options []settingsOption, query string) []settingsOption {
	q := strings.ToLower(strings.TrimSpace(query))
	if q == "" {
		return options
	}
	var out []settingsOption
	for _, option := range options {
		if strings.Contains(strings.ToLower(option.label), q) ||
			strings.Contains(strings.ToLower(option.value), q) {
			out = append(out, option)
		}
	}
	return out
}

// settingsChoice is one settings dropdown, assembled on ui.SelectBaseOpen so
// the trigger keeps its popup-button role and the rows keep keyboard
// navigation. Styling follows the lobe tokens the surrounding page reads.
func (a *app) settingsChoice(c *ui.Context, id, label, current string, width float32, options []settingsOption, onSelect func(settingsOption)) ui.Element {
	return a.settingsChoiceSwatched(c, id, label, current, width, options, nil, onSelect)
}

// settingsChoiceSwatched extends settingsChoice with an optional swatch
// callback: three colors drawn as dots before a row's label (the scheme
// menu's palette preview).
func (a *app) settingsChoiceSwatched(c *ui.Context, id, label, current string, width float32, options []settingsOption, swatch func(value string) []ui.Color, onSelect func(settingsOption)) ui.Element {
	tk := theme.TokensOf(c)
	if a.settings.menus == nil {
		a.settings.menus = make(map[string]*settingsChoiceState)
	}
	menu := a.settings.menus[id]
	if menu == nil {
		menu = &settingsChoiceState{value: current, source: current}
		a.settings.menus[id] = menu
	} else if menu.source != current {
		menu.value, menu.source = current, current
	}
	if menu.open != menu.wasOpen {
		menu.wasOpen = menu.open
		if !menu.open {
			menu.search = ""
		}
	}
	c.Key("settings-choice-" + id)
	selectParts := ui.SelectBaseOpen(c, &menu.value, &menu.open)
	trigger := selectParts.Trigger.Label(label).Height(32).Padding(0, 10).Radius(tk.BorderRadiusSM).
		Gap(6).AlignItems(ui.Center).Border(1, tk.FillBorder())
	if width > 0 {
		trigger.Width(width).Justify(ui.SpaceBetween)
	}
	if menu.open || trigger.Hovered() {
		trigger.Background(tk.FillTertiary)
	}
	trigger.Children(func() {
		ui.Text(c, settingsOptionLabel(menu.value, options)).FontSize(uiPx(12.5)).TextColor(tk.TextSecondary).
			Grow(1).MinWidth(0).SingleLine().Ellipsis("...")
		icon(c, "chevrons-up-down", 12, tk.TextTertiary)
	})
	selectParts.Popup(func(panel ui.Element) {
		panelWidth := max(float32(200), width)
		panel.Background(tk.BgElevated).Radius(tk.BorderRadius).
			Padding(4).Width(panelWidth)
		tk.Elevate(panel, theme.BoxShadowSecondary)
		rows := options
		if menu.searchable {
			c.Key("settings-choice-search-" + id)
			ui.Row(c).FillWidth().Height(32).Padding(0, 9).Gap(7).AlignItems(ui.Center).Children(func() {
				icon(c, "search", 13, tk.TextTertiary)
				c.Key("settings-choice-search-field-" + id)
				ui.TextInputBase(c, &menu.search).Label(tr("Search fonts…")).Placeholder(tr("Search fonts…")).
					Grow(1).MinWidth(0).FontSize(uiPx(12.5)).TextColor(tk.TextSecondary).
					OnTextKey(func(k ui.TextKey) bool {
						if k.Key != ui.KeyEscape || k.Text() == "" {
							return false
						}
						k.SetText("")
						return true
					})
			})
			rows = filterSettingsOptions(options, menu.search)
		}
		if len(rows) == 0 {
			ui.Row(c).FillWidth().Padding(8).Children(func() {
				ui.Text(c, tr("No matching items")).FontSize(uiPx(12)).TextColor(tk.TextTertiary)
			})
			return
		}
		ui.Scroll(c).FillWidth().MaxHeight(300).Children(func() {
			ui.Column(c).FillWidth().Children(func() {
				for _, option := range rows {
					c.Key(option.value)
					item := selectParts.Item(option.value).Label(tr(option.label)).FillWidth().Height(28).
						Padding(0, 8).Radius(tk.BorderRadiusSM).Gap(10).AlignItems(ui.Center)
					if item.Highlighted() || item.Hovered() {
						item.Background(tk.FillTertiary)
					}
					selected := option.value == menu.value
					dots := []ui.Color(nil)
					if swatch != nil {
						dots = swatch(option.value)
					}
					item.Children(func() {
						if len(dots) == 3 {
							ui.Row(c).Gap(3).AlignItems(ui.Center).Children(func() {
								for _, color := range dots {
									ui.Box(c).Size(10, 10).Radius(5).Border(1, tk.TextSecondary).
										Background(color).Children(func() {})
								}
							})
						}
						text := tk.TextSecondary
						if selected {
							text = tk.Text
						}
						weight := 400
						if selected {
							weight = 500
						}
						ui.Text(c, tr(option.label)).FontSize(uiPx(12.5)).FontWeight(weight).TextColor(text).
							Grow(1).MinWidth(0).SingleLine().Ellipsis("...")
						if selected {
							icon(c, "check", 12, tk.TextTertiary)
						}
					})
				}
			})
		})
	})
	trigger.OnChange(func() {
		for _, option := range options {
			if option.value == menu.value {
				onSelect(option)
				return
			}
		}
	})
	return trigger
}

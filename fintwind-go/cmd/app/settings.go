package main

import (
	"strings"

	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/action"
	lobechoice "github.com/lobehub/lobe-mygo/pkg/choice"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

// Values from settings.rs.
const (
	settingsWidth      = 252.0
	settingsMaxWidth   = 760.0
	settingsNavHeight  = 38.0
	settingsRowHeight  = 60.0
	settingsGroupTitle = 12.0
)

type settingsPage struct {
	key   string
	label string
	icon  string
}

var settingsPages = []settingsPage{
	{"general", "General", "settings"},
	{"appearance", "Appearance", "appearance"},
	{"providers", "Providers", "bot"},
	{"skills", "Skills", "package"},
	{"mcpServers", "MCP Servers", "wrench"},
	{"mcpMarket", "MCP Marketplace", "sparkle"},
}

// settingsState holds the General page's editable values.
type settingsState struct {
	page              string
	query             string
	networkProxy      string
	autoCompactThresh string
	// loading guards the one daemon getSettings fetch; settingsLoaded (on
	// app) marks its success. Both live outside settingsRaw so a failed
	// load simply keeps the default chip labels.
	loading           bool
	browserTools      bool
	scheduledTools    bool
	mermaidVisual     bool
	keepAwake         bool
	termCopySelect    bool
	termEnv           string
	autoCompact       bool
	busySendSteer     bool
	busySendSel       int // 0 queue, 1 steer; the bound Segmented model
	shells            []settingsShell
	shellsLoading     bool
	shellsGeneration  uint64
	catalog           settingsCatalogUIState
	menus             map[string]*settingsChoiceState
	scroll            ui.ScrollState
	backend           settingsBackendState
	runtime           runtimeConfigState
	preferences       appPreferences
	message           string
	settingsWriteGate chan struct{}
}

// settingsSecondary is the settings column: search, then the page list.
func (a *app) settingsSecondary(c *ui.Context) {
	secondary := ui.Column(c).Width(a.settingsSecWidth).FillHeight().Shrink(0).
		Background(palette.sidebar).Radius(workspaceRadius).Clip()
	secondary = sidebarShadow(secondary)
	secondary.Children(func() {
		ui.Row(c).Padding(12, 12, 0, 12).Children(func() {
			settingsSearchInput(c, "settings-search", tr("Search Settings"), &a.settings.query)
		})
		ui.Row(c).Height(18)
		ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
			ui.Column(c).Padding(0, 12).Gap(3).FocusGroup(ui.Vertical).Children(func() {
				query := strings.ToLower(strings.TrimSpace(a.settings.query))
				matches := 0
				for _, page := range settingsPages {
					if query != "" && !strings.Contains(strings.ToLower(page.label), query) && !strings.Contains(strings.ToLower(tr(page.label)), query) {
						continue
					}
					page := page
					matches++
					selected := a.settings.page == page.key
					c.Key("settings-page-" + page.key)
					row := ui.ButtonBase(c).Label(tr(page.label)).FillWidth().Height(settingsNavHeight).
						Padding(0, 11).Radius(8).Gap(10).Justify(ui.Start).AlignItems(ui.Center)
					if row.Hovered() || row.Pressed() || selected {
						row.Background(palette.sidebarItem)
					}
					row.Children(func() {
						color := palette.textTertiary
						text := palette.textSecondary
						if selected {
							text = palette.text
							color = palette.textSecondary
						}
						icon(c, page.icon, 16, color)
						ui.Row(c).Grow(1).MinWidth(0).Children(func() {
							ui.Text(c, tr(page.label)).FontSize(uiPx(13.5)).TextColor(text)
						})
					})
					if row.Clicked() {
						a.openSettingsPage(page.key)
					}
				}
				if matches == 0 {
					ui.Text(c, tr("No settings found")).FontSize(uiPx(12)).TextColor(palette.textMuted).Padding(10, 11)
				}
			})
		})
		a.secondaryResizeHandle(c, &a.settingsSecWidth)
	})
}

// settingsContent is the page column. General/Appearance render the 18px
// title over the 760 wide document column; Providers/Skills/MCP render the
// mail-style split full-bleed — no title, no width cap, no card — matching
// render_settings_content in settings.rs.
func (a *app) settingsContent(c *ui.Context) {
	content := ui.Column(c).Grow(1).MinWidth(0).FillHeight().Background(palette.surface).
		BorderWidth(0, 0, 0, 1).BorderColor(palette.sidebarBorder).
		Radius(workspaceRadius, 0, 0, workspaceRadius).Clip()
	content = workspaceShadow(content)
	content.Children(func() {
		switch a.settings.page {
		case "providers":
			a.providersSettings(c)
			return
		case "skills":
			a.skillsSettings(c)
			return
		case "mcpServers":
			a.mcpSettings(c)
			return
		case "mcpMarket":
			a.mcpMarketSettings(c)
			return
		}
		c.Key("settings-document-" + a.settings.page)
		ui.Scroll(c).TrackScroll(&a.settings.scroll).FillWidth().Grow(1).MinHeight(0).Children(func() {
			ui.Column(c).FillWidth().Padding(0, 32, 48, 32).Children(func() {
				ui.Column(c).FillWidth().MaxWidth(settingsMaxWidth).Margin(0, ui.Auto).Children(func() {
					ui.Text(c, settingsPageTitle(a.settings.page)).FontSize(uiPx(18)).
						FontWeight(500).TextColor(palette.text).Padding(2, 0, 0, 0)
					switch a.settings.page {
					case "general":
						a.generalSettings(c)
					case "appearance":
						a.appearanceSettings(c)
					}
				})
			})
		})
	})
}

func settingsPageTitle(key string) string {
	for _, page := range settingsPages {
		if page.key == key {
			return tr(page.label)
		}
	}
	return "Settings"
}

func (a *app) appearanceSettings(c *ui.Context) {
	lobeThemed(c, func() {
		a.settingsGroupCard(c, "", func() {
			settingsRow(c, tr("Theme"), tr("Automatic uses light from 07:00 to 19:00 local time"), func() {
				a.settingsDropdown(c, "theme", "Theme", settingsKeyTheme, a.themeValue(), 116, themeOptions())
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Color scheme"), tr("Choose a palette for both light and dark appearance"), func() {
				a.settingsChoiceSwatched(c, "scheme", tr("Color scheme"), a.schemeValue(), 200, schemeOptions(), a.schemeSwatch, func(option settingsOption) {
					if option.value != a.schemeValue() {
						a.savePreference(settingsKeyThemeScheme, option.raw)
					}
				})
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Language"), tr("Choose the language used throughout fintwind"), func() {
				a.settingsDropdown(c, "language", "Language", settingsKeyLanguage, a.languageValue(), 160, languageOptions())
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Interface scale"), tr("Scale the app interface"), func() {
				a.settingsDropdown(c, "ui-scale", "Interface scale", settingsKeyUITextScale, a.uiScaleValue(), 116, textScaleOptions())
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Interface font"), tr("Choose the font used throughout the app interface"), func() {
				current := a.uiFontValue()
				a.settingsSearchDropdown(c, "ui-font", "Interface font", settingsKeyUIFontFamily, current, 200, fontOptionsWithCurrent(current, uiFontOptions()))
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Code text size"), tr("Scale code in transcripts, tool output, diffs, and the file pane."), func() {
				a.settingsDropdown(c, "code-scale", "Code text size", settingsKeyCodeTextScale, a.codeScaleValue(), 116, textScaleOptions())
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Code font"), tr("Choose the font used for code, diffs, the file pane, and the terminal."), func() {
				current := a.codeFontValue()
				a.settingsSearchDropdown(c, "code-font", "Code font", settingsKeyCodeFontFamily, current, 200, fontOptionsWithCurrent(current, codeFontOptions()))
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Transcript width"), tr("Set how wide messages and the composer can grow on large screens."), func() {
				a.settingsDropdown(c, "transcript-width", "Transcript width", settingsKeyTranscriptWidth, a.transcriptWidthValue(), 116, transcriptWidthOptions())
			})
		})
		a.settingsGroupCard(c, tr("Terminal"), func() {
			settingsRow(c, tr("Shell"), tr("New terminals start here; open a new tab to apply"), func() {
				current := a.terminalShellValue()
				a.settingsDropdown(c, "terminal-shell", "Terminal shell", settingsKeyTerminalShell, current, 220, a.terminalShellOptions(current))
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Environment"), tr("Extra KEY=value entries for new terminals, separated by semicolons"), func() {
				settingsTextInput(c, "terminal-env", tr("Environment"), &a.settings.termEnv, 220, "KEY=value").
					OnSubmit(a.saveTerminalEnv)
				settingsButton(c, "settings-terminal-env-apply", tr("Apply")).OnClick(a.saveTerminalEnv)
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Right-click"), tr("New terminals start here"), func() {
				a.settingsDropdown(c, "terminal-right-click", "Right-click", settingsKeyTerminalRightClick, a.terminalRightClickValue(), 160, terminalRightClickOptions())
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Copy on select"), tr("Copy selected text immediately"), func() {
				c.Key("settings-toggle-terminal-copy-select")
				lobechoice.Switch(c, &a.settings.termCopySelect, lobechoice.SwitchProps{Size: theme.Small}).Label(tr("Copy on select")).OnChange(func() {
					a.savePreference(settingsKeyTerminalCopyOnSelect, a.settings.termCopySelect)
				})
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Scrollback"), tr("New terminals keep this much output"), func() {
				a.settingsDropdown(c, "terminal-scrollback", "Scrollback", settingsKeyTerminalScrollback, a.terminalScrollbackValue(), 116, terminalScrollbackOptions())
			})
		})
	})
}

// saveTerminalEnv writes the environment field to local preferences.
func (a *app) saveTerminalEnv() {
	a.savePreference(settingsKeyTerminalEnv, a.settings.termEnv)
}

// schemeSwatch resolves one scheme's palette preview for the appearance
// currently in force, the way Rust's scheme_choice does.
func (a *app) schemeSwatch(value string) []ui.Color {
	colors := schemeSwatchColors(value, activeAppearance.dark)
	return colors[:]
}

// settingsGroupCard is settingsGroup with rows appended by the caller.
func (a *app) settingsGroupCard(c *ui.Context, title string, rows func()) {
	margin := float32(20)
	if title == "" {
		margin = 15
	}
	ui.Column(c).FillWidth().Margin(margin, 0, 0, 0).Radius(13).Clip().
		Background(palette.raised).Border(1, palette.cardBorder).Shadow(0, 1, 2, 0, ui.RGBA(0, 0, 0, 0.05)).Children(func() {
		if title != "" {
			ui.Text(c, title).FontSize(uiPx(settingsGroupTitle)).FontWeight(500).
				TextColor(palette.textSecondary).Padding(14, 20, 4, 20)
		}
		rows()
	})
}

// settingsSeparator is the hairline between two rows of a group.
func (a *app) settingsSeparator(c *ui.Context) {
	ui.Row(c).Margin(0, 20).Height(1).Background(palette.border)
}

// settingsRow lays out one group row: title, optional description, control.
func settingsRow(c *ui.Context, title, description string, control func()) {
	row := ui.Row(c).FillWidth().MinHeight(settingsRowHeight).Padding(12, 20).
		Gap(24).AlignItems(ui.Center)
	row.Children(func() {
		ui.Column(c).Grow(1).MinWidth(0).Children(func() {
			ui.Text(c, title).FontSize(uiPx(13.5)).FontWeight(500).TextColor(palette.text)
			if description != "" {
				ui.Text(c, description).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(18)).
					TextColor(palette.textSecondary).Wrap().Margin(5, 0, 0, 0)
			}
		})
		if control != nil {
			ui.Row(c).Shrink(0).Gap(8).AlignItems(ui.Center).Children(control)
		}
	})
}

// settingsSegmented is the Queue/Steer control, a lobe Segmented bound to
// the selection index; the preference follows the pick the frame it changes.
func (a *app) settingsSegmented(c *ui.Context) {
	options := []string{tr("Queue"), tr("Steer")}
	themeSegmented(c, &a.settings.busySendSel, options)
	steer := a.settings.busySendSel == 1
	if steer != a.settings.busySendSteer && !a.settings.backend.settingsWrite && !a.settings.backend.settingsUncertain {
		a.settings.busySendSteer = steer
		a.savePreference("busy_send_mode", choice(steer, "steer", "queue"))
	}
}

func (a *app) toggle(c *ui.Context, label string, value *bool) {
	c.Key("settings-toggle-" + label)
	disabled := a.settings.backend.settingsWrite || a.settings.backend.settingsUncertain || !a.settingsLoaded
	if label == "Mermaid visualization prompt" || label == "Keep screen awake" {
		disabled = false
	}
	lobechoice.Switch(c, value, lobechoice.SwitchProps{Size: theme.Small}).Label(tr(label)).Disabled(disabled).OnChange(func() {
		switch label {
		case "Scheduled task MCP":
			a.saveDaemonSetting("scheduled_tools_enabled", *value)
		case "Auto compact":
			a.saveDaemonSetting("auto_compact_enabled", *value)
		case "Mermaid visualization prompt":
			a.savePreference("mermaid_visualization_enabled", *value)
		case "Keep screen awake":
			a.savePreference("keep_screen_awake", *value)
		}
	})
}

// settingsButton is a row's secondary action, the lobe Default button at
// h32 to match the settings inputs.
func settingsButton(c *ui.Context, key, label string) ui.Element {
	c.Key(key)
	return action.Button(c, label, action.ButtonProps{Variant: action.Default, Size: theme.Small}).Height(32)
}

// generalSettings is render_general_settings: four groups of rows and the
// version caption. The OpenCode shell, todo list, image generation, and
// default model rows read the daemon's runtime configuration and save it
// immediately on every edit, like Rust's rows write straight through.
func (a *app) generalSettings(c *ui.Context) {
	lobeThemed(c, func() {
		a.settingsGroupCard(c, tr("Runtime environment"), func() {
			version := a.settings.backend.general.update.CurrentVersion
			if version == "" {
				version = "Version not checked"
			}
			settingsRow(c, tr("OpenCode"), version, func() {
				a.settingsOpenCodeControl(c)
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Network proxy"), tr("Applied to update checks, the model catalog, and new sessions"), func() {
				settingsTextInput(c, "network-proxy", tr("Network proxy"), &a.settings.networkProxy, 220, "http://127.0.0.1:7897").
					Disabled(a.settings.backend.settingsWrite || a.settings.backend.settingsUncertain).OnSubmit(a.saveSettingsProxy)
				settingsButton(c, "settings-proxy-apply", tr("Apply")).Disabled(a.settings.backend.settingsWrite || a.settings.backend.settingsUncertain).OnClick(a.saveSettingsProxy)
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("OpenCode shell"), tr("Choose the shell OpenCode uses to run commands"), func() {
				a.runtimeShellDropdown(c)
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("OpenCode executable"), a.settingsStr("opencode_binary_path", "Automatic detection"), func() {
				settingsButton(c, "settings-choose-executable", tr("Choose executable")).Disabled(a.settings.backend.general.choosing || a.settings.backend.settingsWrite).OnClick(func() { a.chooseSettingsPath("opencode_binary_path", false) })
				if a.settingsStr("opencode_binary_path", "") != "" {
					settingsButton(c, "settings-reset-executable", tr("Use automatic detection")).Disabled(a.settings.backend.settingsWrite || a.settings.backend.settingsUncertain).OnClick(func() {
						a.resetSettingsDaemonKey("opencode_binary_path")
					})
				}
			})
			a.settingsPathConfirmation(c, "opencode_binary_path")
		})
		a.settingsGroupCard(c, tr("Features"), func() {
			s := &a.settings.runtime
			runtimeDisabled := s.writeBusy || !s.loaded
			settingsRow(c, tr("While running"), "", func() {
				a.settingsSegmented(c)
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Browser tools"), tr("Not available in this build"), func() {
				c.Key("settings-toggle-browser-tools")
				ui.Switch(c, &a.settings.browserTools).Label(tr("Browser tools")).Disabled(true)
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Todo list"), tr("Off by default. When on, agents can keep a task list with todowrite and show it above the composer"), func() {
				c.Key("settings-toggle-todo-list")
				lobechoice.Switch(c, &s.todo, lobechoice.SwitchProps{Size: theme.Small}).Label(tr("Todo list")).Disabled(runtimeDisabled).OnChange(func() {
					a.saveRuntimeTodo(s.todo)
				})
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Scheduled task MCP"), tr("Let agents create scheduled tasks. Requires OpenCode 2.1.4+; enables the experimental MCP connection automatically"), func() {
				a.toggle(c, "Scheduled task MCP", &a.settings.scheduledTools)
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Image generation"), tr("Off by default. When on, the model can create and edit images with the image_generate tool"), func() {
				c.Key("settings-toggle-image-generation")
				lobechoice.Switch(c, &s.image, lobechoice.SwitchProps{Size: theme.Small}).Label(tr("Image generation")).Disabled(runtimeDisabled).OnChange(func() {
					a.saveRuntimeImage(s.image, s.model)
				})
			})
			if s.image {
				a.settingsSeparator(c)
				settingsRow(c, tr("Default model"), tr("Used when a request does not name a model"), func() {
					a.settingsChoice(c, "runtime-model-choice", tr("Default model"), s.model, 220, a.runtimeImageModelOptions(), func(option settingsOption) {
						if option.value != s.model {
							a.saveRuntimeImage(true, option.value)
						}
					}).Disabled(runtimeDisabled)
				})
			}
			a.settingsSeparator(c)
			settingsRow(c, tr("Mermaid visualization prompt"), tr("New sessions only"), func() {
				a.toggle(c, "Mermaid visualization prompt", &a.settings.mermaidVisual)
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Keep screen awake"), tr("While on, the display and system stay awake for as long as the app runs"), func() {
				a.toggle(c, "Keep screen awake", &a.settings.keepAwake)
			})
		})
		a.settingsGroupCard(c, tr("Context management"), func() {
			settingsRow(c, tr("Auto compact"), tr("Compact once when live context reaches the threshold below, for the session and its subagents"), func() {
				a.toggle(c, "Auto compact", &a.settings.autoCompact)
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Compaction threshold"), tr("Windows below this usage never reach it and keep the provider default"), func() {
				settingsTextInput(c, "compaction-threshold", tr("Compaction threshold"), &a.settings.autoCompactThresh, 120, "500000").
					Disabled(!a.settings.autoCompact || a.settings.backend.settingsWrite || a.settings.backend.settingsUncertain).OnSubmit(a.saveSettingsThreshold)
				settingsButton(c, "settings-threshold-apply", tr("Apply")).Disabled(!a.settings.autoCompact || a.settings.backend.settingsWrite || a.settings.backend.settingsUncertain).OnClick(a.saveSettingsThreshold)
			})
		})
		a.settingsGroupCard(c, tr("Data management"), func() {
			path := a.projectlessRoot
			if path == "" {
				path = "Default workspace"
			}
			settingsRow(c, tr("General workspace"), path, func() {
				settingsButton(c, "settings-choose-workspace", tr("Choose folder")).Disabled(a.settings.backend.general.choosing || a.settings.backend.settingsWrite).OnClick(func() { a.chooseSettingsPath("general_workspace_path", true) })
				if a.projectlessRoot != "" {
					settingsButton(c, "settings-reset-workspace", tr("Restore default")).Disabled(a.settings.backend.settingsWrite || a.settings.backend.settingsUncertain).OnClick(func() {
						a.resetSettingsDaemonKey("general_workspace_path")
					})
				}
			})
			a.settingsSeparator(c)
			settingsRow(c, tr("Local by default"), tr("Projects, conversations, and settings are stored on this computer"), nil)
			a.settingsSeparator(c)
			settingsRow(c, tr("Session cleanup"), tr("Preview eligible inactive conversations before deletion"), func() {
				period := a.settings.backend.general.cleanupPeriod
				if period == "" {
					period = "Days90"
				}
				a.settingsChoice(c, "cleanup-period", tr("Retention"), period, 116, []settingsOption{{label: "30 days", value: "Days30"}, {label: "90 days", value: "Days90"}, {label: "180 days", value: "Days180"}}, func(option settingsOption) {
					a.settings.backend.general.cleanupPeriod = option.value
					a.settings.backend.general.cleanupPreview = nil
				}).Disabled(a.settings.backend.general.operation.writing)
				settingsButton(c, "settings-cleanup-preview", tr("Preview")).Disabled(a.settings.backend.general.operation.writing).OnClick(a.previewSettingsCleanup)
			})
			a.settingsPathConfirmation(c, "general_workspace_path")
			a.settingsCleanupConfirmation(c)
		})
		settingsOperationStatus(c, a.settings.backend.general.operation, "")
		a.settingsFooter(c)
	})
}

// settingsFooter is the version caption under the General page, like Rust's
// settings.app_version. It reads the daemon's handshake version and hides
// itself when the daemon did not send one.
func (a *app) settingsFooter(c *ui.Context) {
	version := ""
	if a.client != nil {
		version = a.client.Version()
	}
	if version == "" {
		return
	}
	ui.Text(c, trf("fintwind %s", version)).FontSize(uiPx(12)).TextColor(palette.textTertiary).Padding(16, 20, 0, 20)
}

package main

import (
	"github.com/egoist/mygo/ui"
)

// providerFormatOptions mirrors the API format picker on the Rust add form.
func providerFormatOptions() []settingsOption {
	return []settingsOption{
		{label: "OpenAI Chat Completions", value: "openai"},
		{label: "OpenAI Responses", value: "openai-responses"},
		{label: "Anthropic", value: "anthropic"},
		{label: "Google", value: "google"},
	}
}

// mcpKindOptions mirrors the Local/Remote type picker on the Rust add form.
func mcpKindOptions() []settingsOption {
	return []settingsOption{
		{label: "Local", value: "local"},
		{label: "Remote", value: "remote"},
	}
}

// formSecretField is a labeled h32 password input with a Show/Hide toggle.
// The value lives in *value only; nothing is logged.
func formSecretField(c *ui.Context, key, label string, value *string, show *bool) {
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, label).FontSize(11).FontWeight(500).TextColor(palette.text)
		ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
			ui.Row(c).Grow(1).MinWidth(0).Height(32).Padding(0, 9).Radius(7).
				Border(1, palette.borderStrong).Background(palette.composer).
				AlignItems(ui.Center).Children(func() {
				c.Key(key)
				input := ui.TextInputBase(c, value).FillWidth().FontSize(12.5).
					FixedLineHeight(17).TextColor(palette.text).Label(label)
				if !*show {
					input.Password()
				}
			})
			toggle := tr("Show")
			if *show {
				toggle = tr("Hide")
			}
			if componentTextButton(c, key+"-reveal", toggle, false).Clicked() {
				*show = !*show
			}
		})
	})
}

func formActions(c *ui.Context, key, saveLabel string, disabled bool, onCancel, onSave func()) {
	ui.Row(c).FillWidth().Gap(8).Justify(ui.End).AlignItems(ui.Center).Children(func() {
		if componentTextButton(c, key+"-cancel", tr("Cancel"), false).Clicked() && onCancel != nil {
			onCancel()
		}
		componentTextButton(c, key+"-save", saveLabel, true).Disabled(disabled).OnClick(onSave)
	})
}

// deviceChip is one h26 device option: the active device sits on a 16%
// accent wash. Only this computer is available.
func deviceChip(c *ui.Context, label string, selected bool) {
	chip := ui.ButtonBase(c).Label(label).Height(26).Padding(0, 10).
		Radius(7).AlignItems(ui.Center)
	if selected {
		chip.Background(ui.RGBA(palette.accent.R, palette.accent.G, palette.accent.B, 0.16))
	} else {
		chip.Border(1, palette.border)
	}
	chip.Children(func() {
		color := palette.textSecondary
		if selected {
			color = palette.text
		}
		ui.Text(c, label).FontSize(11.5).TextColor(color)
	})
}

// formTitle is the 15px medium detail title used by both add forms.
func formTitle(c *ui.Context, title string) {
	ui.Text(c, title).FontSize(15).FontWeight(500).TextColor(palette.text)
}

// providersDetail reads the Built-in detail or the custom add form.
func (a *app) providersDetail(c *ui.Context) {
	st := &a.settings.catalog
	if st.providerAdding {
		a.providerAddForm(c)
		return
	}
	if a.settings.backend.providerSelected != "" {
		a.settingsCustomProviderDetail(c)
		return
	}
	ui.Row(c).FillWidth().Padding(14, 24, 0, 24).Gap(6).
		AlignItems(ui.Center).Children(func() {
		ui.Text(c, tr("Device")).FontSize(11).FontWeight(500).
			TextColor(palette.textSecondary)
		deviceChip(c, "This computer", true)
	})
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Children(func() {
			ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
				mailDetailTile(c, "bot", 17)
				ui.Column(c).Grow(1).MinWidth(0).Children(func() {
					ui.Text(c, tr("OpenCode")).FontSize(15).FontWeight(500).
						TextColor(palette.text)
					ui.Text(c, tr("Built-in · managed by the OpenCode CLI")).
						FontSize(10.5).TextColor(palette.textTertiary).
						SingleLine().Ellipsis("...")
				})
				status := tr("Not checked")
				installed := a.modelProbe != nil && a.modelProbe.Installed
				if a.modelProbeLoad {
					status = tr("Checking")
				} else if a.modelProbe != nil {
					if installed {
						status = tr("Installed")
					} else {
						status = tr("Not installed")
					}
				}
				mailEnabledBadge(c, installed, status)
				a.providerRefreshButton(c)
			})
			a.providerModelSection(c)
			a.settingsBuiltinIntegrations(c)
		})
	})
}

func (a *app) providerAddForm(c *ui.Context) {
	st := &a.settings.catalog
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			formTitle(c, choice(a.settings.backend.providerEditing != "", tr("Edit provider"), tr("Add provider")))
			settingsFormField(c, "provider-form-name", tr("Name"), &st.pfName, "My provider")
			settingsFormField(c, "provider-form-url", tr("Base URL"), &st.pfBaseURL, "https://api.example.com/v1")
			formSecretField(c, "provider-form-key", tr("API key"), &st.pfAPIKey, &st.pfShowKey)
			ui.Column(c).FillWidth().Gap(6).Children(func() {
				ui.Text(c, tr("API format")).FontSize(11).FontWeight(500).TextColor(palette.text)
				a.settingsChoice(c, "provider-form-format", tr("API format"), st.pfFormat, formPickerWidth, providerFormatOptions(), func(option settingsOption) {
					st.pfFormat = option.value
					a.settings.backend.providerFormatTouched = true
				})
			})
			a.settingsProviderModels(c)
			settingsOperationStatus(c, a.settings.backend.provider, "")
			formActions(c, "provider-form", tr("Save"), !a.settings.backend.provider.loaded || a.settings.backend.provider.writing || a.settings.backend.provider.uncertain, a.cancelProviderAdd, a.saveSettingsProvider)
		})
	})
}

// skillsDetail has no selection without a catalog: the placeholder stands.
func (a *app) skillsDetail(c *ui.Context) {
	a.settingsSkillDetail(c)
}

// mcpDetail reads the placeholder or the add form.
func (a *app) mcpDetail(c *ui.Context) {
	st := &a.settings.catalog
	if st.mcpAdding {
		a.mcpAddForm(c)
		return
	}
	a.settingsMcpDetail(c)
}

func (a *app) mcpAddForm(c *ui.Context) {
	st := &a.settings.catalog
	if st.mfKind == "" {
		st.mfKind = "local"
	}
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			formTitle(c, choice(a.settings.backend.mcpEditing != "", tr("Edit server"), tr("Add server")))
			settingsFormField(c, "mcp-form-name", tr("Name"), &st.mfName, "my-server")
			ui.Column(c).FillWidth().Gap(6).Children(func() {
				ui.Text(c, tr("Type")).FontSize(11).FontWeight(500).TextColor(palette.text)
				a.settingsChoice(c, "mcp-form-kind", tr("Server type"), st.mfKind, formPickerWidth, mcpKindOptions(), func(option settingsOption) {
					st.mfKind = option.value
				})
			})
			if st.mfKind == "remote" {
				settingsFormField(c, "mcp-form-url", tr("URL"), &st.mfURL, "https://example.com/mcp")
			} else {
				settingsFormField(c, "mcp-form-command", tr("Command arguments (JSON)"), &st.mfCommand, `["npx", "-y", "my-server"]`)
			}
			settingsFormField(c, "mcp-form-environment", tr("Environment (JSON)"), &st.mfEnvironment, `{"NAME":"value"}`)
			if st.mfKind == "remote" {
				settingsFormField(c, "mcp-form-headers", tr("Headers (JSON)"), &st.mfHeaders, `{"Authorization":"Bearer ..."}`)
				a.settingsChoice(c, "mcp-form-oauth", tr("OAuth"), st.mfOAuth, formPickerWidth, []settingsOption{{label: "Automatic OAuth", value: "automatic"}, {label: "Headers only", value: "disabled"}, {label: "Custom OAuth client", value: "custom"}}, func(option settingsOption) { st.mfOAuth = option.value })
				if st.mfOAuth == "custom" {
					settingsFormField(c, "mcp-form-client", tr("OAuth client ID"), &st.mfClientID, "")
					formSecretField(c, "mcp-form-client-secret", tr("OAuth client secret"), &st.mfClientSecret, &st.mfShowSecret)
					settingsFormField(c, "mcp-form-scope", tr("OAuth scope"), &st.mfScope, "")
				}
			}
			settingsOperationStatus(c, a.settings.backend.mcp, "")
			formActions(c, "mcp-form", tr("Save"), !a.settings.backend.mcp.loaded || a.settings.backend.mcp.writing || a.settings.backend.mcp.uncertain, func() {
				st.mcpAdding = false
				a.clearSettingsMcpSecrets()
				a.closeSettingsMenus()
			}, a.saveSettingsMcpForm)
		})
	})
}

func (a *app) marketDetail(c *ui.Context) {
	a.settingsMarketDetail(c)
}

func settingsFormField(c *ui.Context, key, label string, value *string, placeholder string) {
	c.Key(key + "-field")
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, label).FontSize(11).FontWeight(500).TextColor(palette.text)
		settingsTextInput(c, key, label, value, 0, placeholder)
	})
}

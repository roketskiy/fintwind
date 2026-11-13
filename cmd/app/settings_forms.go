package main

import (
	"strings"

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
		ui.Text(c, label).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
		ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
			ui.Row(c).Grow(1).MinWidth(0).Height(32).Padding(0, 9).Radius(7).
				Border(1, palette.borderStrong).Background(palette.composer).
				AlignItems(ui.Center).Children(func() {
				c.Key(key)
				input := ui.TextInputBase(c, value).FillWidth().FontSize(uiPx(12.5)).
					FixedLineHeight(uiPx(17)).TextColor(palette.text).Label(label)
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
		ui.Text(c, label).FontSize(uiPx(11.5)).TextColor(color)
	})
}

// formTitle is the 15px medium detail title used by both add forms.
func formTitle(c *ui.Context, title string) {
	ui.Text(c, title).FontSize(uiPx(15)).FontWeight(500).TextColor(palette.text)
}

// providersDetail reads the Built-in detail or the custom add form.
func (a *app) providersDetail(c *ui.Context) {
	a.providersDetailFull(c)
}

func (a *app) providerAddForm(c *ui.Context) {
	a.providerAddFormFull(c)
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
	backend := &a.settings.backend
	if st.mfKind == "" {
		st.mfKind = "local"
	}
	name := strings.TrimSpace(st.mfName)
	command := mcpParseCommand(st.mfCommand)
	url := strings.TrimSpace(st.mfURL)
	valid := name != ""
	hint := ""
	switch st.mfKind {
	case "local":
		valid = valid && len(command) != 0
	case "remote":
		if !mcpURLValid(url) {
			valid = false
			if url != "" {
				hint = tr("Enter an HTTP or HTTPS URL")
			}
		}
	default:
		valid = false
	}
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			formTitle(c, tr("Add an MCP server"))
			settingsFormField(c, "mcp-form-name", tr("Name"), &st.mfName, "my-server")
			ui.Column(c).FillWidth().Gap(6).Children(func() {
				ui.Text(c, tr("Type")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
				a.settingsChoice(c, "mcp-form-kind", tr("Server type"), st.mfKind, formPickerWidth, mcpKindOptions(), func(option settingsOption) {
					st.mfKind = option.value
				})
			})
			if st.mfKind == "remote" {
				settingsFormField(c, "mcp-form-url", tr("URL"), &st.mfURL, "https://example.com/mcp")
			} else {
				settingsFormField(c, "mcp-form-command", tr("Command"), &st.mfCommand, "npx -y my-server")
			}
			if hint != "" {
				ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
					icon(c, "info", 12, palette.textTertiary)
					ui.Text(c, hint).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
				})
			}
			settingsOperationStatus(c, backend.mcp, "")
			formActions(c, "mcp-form", tr("Add server"), !valid || !backend.mcp.loaded || backend.mcp.writing || backend.mcp.uncertain, func() {
				st.mcpAdding = false
				a.clearSettingsMcpSecrets()
				a.closeSettingsMenus()
			}, a.submitMcpAddForm)
		})
	})
}

func (a *app) marketDetail(c *ui.Context) {
	a.settingsMarketDetail(c)
}

func settingsFormField(c *ui.Context, key, label string, value *string, placeholder string) {
	c.Key(key + "-field")
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, label).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
		settingsTextInput(c, key, label, value, 0, placeholder)
	})
}

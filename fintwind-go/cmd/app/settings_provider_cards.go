package main

import (
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
	lobechoice "github.com/lobehub/lobe-mygo/pkg/choice"
	"github.com/lobehub/lobe-mygo/pkg/field"
	"github.com/lobehub/lobe-mygo/pkg/layout"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

// Match the two-level usage cards: a quiet title shelf around an inset surface.
func providerInfoCard(c *ui.Context, key, glyph, title string, actions, body func()) {
	layout.Block(c.Key(key), layout.BlockProps{Variant: layout.BlockFilled}).
		FillWidth().MinWidth(0).Radius(12).Background(palette.overlay).Padding(4).Children(func() {
		ui.Row(c).FillWidth().MinHeight(34).Padding(2, 9).Gap(8).AlignItems(ui.Center).Children(func() {
			icon(c, glyph, 13, palette.textTertiary)
			ui.Text(c, tr(title)).Grow(1).MinWidth(0).FontSize(uiPx(11.5)).FontWeight(500).TextColor(palette.textSecondary)
			if actions != nil {
				actions()
			}
		})
		layout.Block(c, layout.BlockProps{Variant: layout.BlockOutlined}).
			FillWidth().MinWidth(0).Radius(8).Background(palette.raised).
			Border(1, palette.cardBorder).Padding(16).Gap(14).Children(body)
	})
}

func (a *app) providerEnabledSwitch(c *ui.Context, provider settingsProvider) {
	enabled := provider.Enabled
	lobechoice.Switch(c.Key("provider-enabled-"+provider.ID), &enabled, lobechoice.SwitchProps{Size: theme.Small}).
		Label(tr("Enabled")).Disabled(a.settings.backend.provider.writing || a.settings.backend.provider.uncertain).
		OnChange(func() { a.toggleCustomProvider(provider.ID) })
}

func providerModelMark(c *ui.Context, id, name, provider string) {
	mark := companyIconFor(id + " " + name)
	if mark == "" {
		mark = providerMark(provider)
	}
	if !activeAppearance.dark {
		mark = strings.TrimSuffix(mark, "-dark")
	}
	ui.Row(c).Size(28, 28).Shrink(0).Center().Children(func() {
		markIcon(c, mark, 18, palette.textSecondary)
	})
}

func (a *app) providerLatencyControls(c *ui.Context, provider settingsProvider) {
	st := &a.settings.backend
	models := providerModelsList(provider)
	if len(models) == 0 {
		return
	}
	selected := st.providerLatencyModel
	found := false
	options := make([]settingsOption, 0, len(models))
	for _, model := range models {
		name := model.id
		if model.name != nil && strings.TrimSpace(*model.name) != "" {
			name = *model.name
		}
		options = append(options, settingsOption{label: name, value: model.id})
		found = found || model.id == selected
	}
	if !found {
		selected = models[0].id
	}
	latency := st.providerLatency[provider.ID+"\x00"+selected]
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, tr("Measure first-token latency")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
		ui.Row(c).FillWidth().Wrap().Gap(8).AlignItems(ui.Center).Children(func() {
			a.settingsChoice(c, "provider-latency-model", tr("Model"), selected, formPickerWidth, options, func(option settingsOption) {
				st.providerLatencyModel = option.value
			})
			componentTextButton(c, "provider-latency-test", tr("Test"), false).
				Disabled(st.provider.writing || latency.testing).OnClick(func() { a.probeModelLatency(provider.ID, selected) })
		})
		if latency.testing {
			ui.Text(c, tr("Testing…")).FontSize(uiPx(11)).TextColor(palette.textSecondary)
		} else if latency.err != "" {
			ui.Text(c, latency.err).FontSize(uiPx(11)).TextColor(palette.dangerText).Wrap()
		} else if latency.latencyMs > 0 {
			ui.Text(c, trf("first token in %s", formatProbeLatency(time.Duration(latency.latencyMs)*time.Millisecond))).FontSize(uiPx(11)).TextColor(palette.textSecondary)
		}
	})
}

func (a *app) providerPasswordInput(c *ui.Context, provider settingsProvider, authorized bool) {
	placeholder := tr("Enter API key")
	if authorized {
		// The CLI store does not expose the saved key. Never put a mask into the draft.
		placeholder = "************"
	}
	field.InputPassword(c.Key("provider-password-"+provider.ID), &a.settings.backend.providerKeyDraft,
		field.InputProps{Placeholder: placeholder}).Label(tr("API Key")).FontSize(uiPx(12.5)).TextColor(palette.text).
		Disabled(a.settings.backend.provider.writing)
}

func (a *app) providerModelModalities(id, provider string, input, output []string) ([]string, []string) {
	if len(input) == 0 || len(output) == 0 {
		basics := a.settings.backend.modelsTable.resolveBasics(id, &provider)
		if len(input) == 0 {
			input = basics.inputModalities
		}
		if len(output) == 0 {
			output = basics.outputModalities
		}
	}
	return input, output
}

func providerModalityIcons(c *ui.Context, direction string, modes []string) {
	ui.Row(c).Gap(5).AlignItems(ui.Center).Children(func() {
		ui.Text(c, tr(direction)).FontSize(uiPx(10.5)).TextColor(palette.textTertiary)
		if len(modes) == 0 {
			ui.Text(c, "—").FontSize(uiPx(10.5)).TextColor(palette.textTertiary).Tooltip(tr("Unknown"))
			return
		}
		seen := map[string]bool{}
		for _, mode := range modes {
			if seen[mode] {
				continue
			}
			seen[mode] = true
			glyph := ""
			switch mode {
			case "text", "image", "audio", "video", "pdf":
				glyph = "modality-" + mode
			}
			tip := tr(direction) + " · " + tr(mode)
			ui.Row(c.Key(direction + "-" + mode)).MinWidth(20).Height(22).Radius(4).Center().
				Focusable().Label(tip).Tooltip(tip).Children(func() {
				if glyph != "" {
					icon(c, glyph, 13, palette.textSecondary)
				} else {
					ui.Text(c, mode).FontSize(uiPx(10.5)).TextColor(palette.textSecondary)
				}
			})
		}
	})
}

// Only the whole row owns the hover surface. The name remains clickable without
// adding an inner surface; trailing actions are separate, never nested buttons.
func providerModelListRow(c *ui.Context, key, id, name, provider string, window *uint64, input, output []string, selected bool, activate, actions func()) {
	if strings.TrimSpace(name) == "" {
		name = id
	}
	description := id
	if name == id {
		description = ""
	}
	row := layout.Block(c.Key(key), layout.BlockProps{Variant: layout.BlockBorderless}).FillWidth().MinWidth(0).Padding(4).Radius(8)
	if selected || row.Hovered() || row.FocusWithin() {
		row.Background(palette.overlay)
	}
	row.Children(func() {
		ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
			ui.Column(c).Grow(1).Basis(0).MinWidth(0).Clip().Children(func() {
				nameControl := layout.Block(c.Key("model-name"), layout.BlockProps{Variant: layout.BlockBorderless, Clickable: activate != nil}).
					FillWidth().Background(ui.Transparent).Label(name)
				if activate != nil {
					nameControl.OnClick(activate)
				}
				nameControl.Children(func() {
					ui.Row(c).FillWidth().Gap(12).Padding(6, 8).AlignItems(ui.Center).Children(func() {
						providerModelMark(c, id, name, provider)
						ui.Column(c).Grow(1).MinWidth(0).Gap(3).Children(func() {
							ui.Text(c, name).FontSize(uiPx(13)).FontWeight(500).TextColor(palette.text).SingleLine().Ellipsis("...")
							if description != "" {
								ui.Text(c, description).FontSize(uiPx(11)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
							}
						})
					})
				})
				ui.Row(c).FillWidth().Padding(0, 8, 2, 48).Gap(14).Wrap().AlignItems(ui.Center).Children(func() {
					providerModalityIcons(c, "Input", input)
					providerModalityIcons(c, "Output", output)
				})
			})
			ui.Row(c).Gap(10).Shrink(0).AlignItems(ui.Center).Children(func() {
				if window != nil && *window > 0 {
					ui.Row(c).Gap(5).Padding(4, 6).Radius(5).Background(palette.overlay).
						Label(tr("Context window")).Tooltip(tr("Context window") + " · " + formatContextWindow(*window)).Children(func() {
						icon(c, "gauge", 12, palette.textTertiary)
						ui.Text(c, formatContextWindow(*window)).FontSize(uiPx(11)).TextColor(palette.textSecondary)
					})
				}
				if actions != nil {
					ui.Row(c).Gap(2).AlignItems(ui.Center).Children(actions)
				}
			})
		})
	})
}

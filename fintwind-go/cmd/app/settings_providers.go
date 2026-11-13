package main

import (
	"github.com/egoist/mygo/ui"
)

func (a *app) currentProviderModelID() string {
	if id := a.settings.catalog.providerModelID; id != "" {
		return id
	}
	if a.modelProbe != nil {
		for i := range a.modelProbe.Models {
			if a.modelProbe.Models[i].IsDefault {
				return a.modelProbe.Models[i].ID
			}
		}
		if len(a.modelProbe.Models) > 0 {
			return a.modelProbe.Models[0].ID
		}
	}
	return ""
}

// providerRefreshButton mirrors the settingsAction geometry (h32 px10 r6
// strong border) but is clickable: it re-runs the shared model probe and
// reads the a.modelProbeLoad loading state. requestModelProbe is a
// first-load guard (it returns while a probe exists), so Refresh clears the
// cached catalog first and then triggers the same shared fetch.
func (a *app) providerRefreshButton(c *ui.Context) {
	label := tr("Refresh")
	if a.modelProbeLoad {
		label = tr("Checking...")
	}
	button := componentTextButton(c, "settings-provider-refresh", label, false).
		Height(32).Padding(0, 10).Radius(6).Disabled(a.modelProbeLoad)
	if button.Clicked() && !a.modelProbeLoad {
		a.modelProbe = nil
		resetModelProbeRetry()
		a.requestModelProbe()
	}
}

// Keep the CLI catalog virtualized and share provider model row presentation.
func (a *app) providerModelSection(c *ui.Context) {
	var models []providerModel
	if a.modelProbe != nil {
		models = a.modelProbe.Models
	}
	if len(models) == 0 {
		label := tr("No available models yet.")
		if a.modelProbeLoad {
			label = tr("Loading...")
		}
		ui.Row(c).FillWidth().Margin(14, 0, 0, 0).Gap(8).
			AlignItems(ui.Center).Children(func() {
			icon(c, "info", 12, palette.textTertiary)
			ui.Text(c, label).FontSize(uiPx(10.5)).FixedLineHeight(uiPx(15)).
				TextColor(palette.textTertiary).Wrap()
		})
		return
	}
	current := a.currentProviderModelID()
	c.Key("settings-provider-model-list")
	ui.List(c, &a.settings.catalog.providerModelList, len(models)).FillWidth().Height(float32(min(len(models)*88, 440))).
		Radius(6).
		ItemKey(func(index int) any { return models[index].ID }).Rows(func(listRow ui.ListRow) {
		c := listRow.Context
		m := models[listRow.Index]
		selected := m.ID == current
		provider := "opencode"
		if m.SubProvider != nil {
			provider = *m.SubProvider
		}
		input, output := a.providerModelModalities(m.ID, provider, nil, nil)
		providerModelListRow(c, m.ID, m.ID, m.Name, provider, nil, input, output, selected,
			func() { a.settings.catalog.providerModelID = m.ID }, func() {
				if m.IsDefault {
					ui.Text(c, tr("Default")).FontSize(uiPx(10.5)).TextColor(palette.textTertiary)
				}
				if selected {
					icon(c, "check", 12, palette.textTertiary)
				}
			})
	})
}

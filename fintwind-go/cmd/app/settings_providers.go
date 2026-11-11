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

// providerModelSection is the detail catalog card: a bordered r9 list under
// an 8px top margin (render_builtin_provider_detail's models card). Rows are
// two-line picker rows (13 semibold name over the 11.5 tertiary id);
// empty/loading states reuse the Rust copy.
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
	ui.List(c, &a.settings.catalog.providerModelList, len(models)).FillWidth().Height(float32(min(len(models)*54, 400))).
		Margin(8, 0, 0, 0).Radius(9).Border(1, palette.border).
		ItemKey(func(index int) any { return models[index].ID }).Rows(func(listRow ui.ListRow) {
		c := listRow.Context
		i := listRow.Index
		m := models[i]
		selected := m.ID == current
		row := ui.ButtonBase(c).Label(m.ID).FillWidth().
			Padding(8, 10).Gap(8).Justify(ui.Start).AlignItems(ui.Center)
		if i > 0 {
			row.BorderWidth(1, 0, 0, 0).BorderColor(palette.border)
		}
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
			ui.Column(c).Grow(1).MinWidth(0).Children(func() {
				ui.Text(c, m.Name).FontSize(uiPx(13)).FontWeight(600).
					TextColor(palette.text).SingleLine().Ellipsis("...")
				ui.Text(c, m.ID).FontSize(uiPx(11.5)).FixedLineHeight(uiPx(16)).
					TextColor(palette.textTertiary).SingleLine().
					Ellipsis("...").Margin(2, 0, 0, 0)
			})
			if m.IsDefault {
				mailEnabledBadge(c, true, "Default")
			}
			if selected {
				icon(c, "check", 12, palette.textTertiary)
			}
		})
		if row.Clicked() {
			a.settings.catalog.providerModelID = m.ID
		}
	})
}

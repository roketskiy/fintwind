package main

import (
	"github.com/ELY-Open-Source/CrychicUI/button"
	"github.com/egoist/mygo/ui"
)

func componentTextButton(c *ui.Context, key, label string, primary bool) ui.Element {
	c.Key(key)
	control := button.Ghost(c, label).
		Scale(1, 1).Height(30).Padding(0, 11).Radius(7).
		AlignItems(ui.Center).Justify(ui.Center).
		FontSize(12.5).TextColor(palette.textSecondary).
		Border(1, palette.border).Background(palette.overlay)
	if control.Hovered() || control.Pressed() {
		control.Background(palette.overlayStrong)
	}
	if primary {
		control.Padding(0, 12).Border(0, ui.Transparent).
			Background(palette.inverse).TextColor(palette.onInverse).FontWeight(500)
		if control.Hovered() || control.Pressed() {
			control.Opacity(0.9)
		}
	}
	return control
}

func componentIconButton(c *ui.Context, key, iconName string, enabled bool) ui.Element {
	c.Key(key)
	control := ui.ButtonBase(c).Size(28, 28).Radius(7).Center().
		Background(ui.Transparent)
	if !enabled {
		control.Disabled(true)
		control.Children(func() {
			icon(c, iconName, 15, palette.textGhost)
		})
		return control
	}
	if control.Hovered() || control.Pressed() {
		control.Background(palette.overlay)
	}
	if control.Pressed() {
		control.Background(palette.overlayStrong)
	}
	control.Children(func() {
		icon(c, iconName, 15, palette.textSecondary)
	})
	return control
}

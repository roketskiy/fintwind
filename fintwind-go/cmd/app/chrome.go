package main

import (
	"fintwind-go/assets"

	"github.com/egoist/mygo/ui"
)

func iconButton(c *ui.Context, name string, box, glyph float32, color ui.Color, tip string) bool {
	el := ui.ButtonBase(c).Label(tip).Tooltip(tip).Size(box, box).Center().Radius(7)
	if el.Hovered() {
		el.Background(palette.overlay)
	}
	if el.Pressed() {
		el.Background(palette.overlayStrong)
	}
	el.Children(func() {
		icon(c, name, glyph, color)
	})
	return el.Clicked()
}

// historyButton is the top bar's back/forward control: 28 square, dimmed to
// 35% when there is nowhere to go.
func historyButton(c *ui.Context, name, tip string, enabled bool) bool {
	el := ui.ButtonBase(c).Label(tip).Tooltip(tip).Size(28, 28).Center().Radius(7)
	if enabled {
		if el.Hovered() {
			el.Background(palette.overlay)
		}
		if el.Pressed() {
			el.Background(palette.overlayStrong)
		}
	} else {
		el.Opacity(0.35)
	}
	el.Children(func() {
		icon(c, name, 15, palette.textTertiary)
	})
	return enabled && el.Clicked()
}

// revealButton is the top bar's "open folder" control: dimmed and inert when
// the current mode has no workspace to reveal.
func revealButton(c *ui.Context, enabled bool, tip string) bool {
	el := ui.ButtonBase(c).Label(tip).Tooltip(tip).Size(28, 28).Center().Radius(7)
	if !enabled {
		el.Opacity(0.35)
	} else {
		if el.Hovered() {
			el.Background(palette.overlay)
		}
		if el.Pressed() {
			el.Background(palette.overlayStrong)
		}
	}
	el.Children(func() {
		icon(c, "folder", 15, palette.textTertiary)
	})
	return enabled && el.Clicked()
}

func (a *app) topBar(c *ui.Context) {
	bar := c.TitleBar()
	ui.Row(c).FillWidth().Height(topbarHeight).Background(palette.rail).AlignItems(ui.Center).Children(func() {
		ui.Row(c).Width(modeRailWidth).Height(topbarHeight).Justify(ui.Center).AlignItems(ui.Center).
			DragWindow().Children(func() {
			ui.Row(c).Size(22, 22).Radius(6).Clip().Children(func() {
				ui.Image(c, assets.SVG("logo-dark")).Size(22, 22)
			})
		})
		ui.Row(c).Height(topbarHeight).Gap(6).AlignItems(ui.Center).Children(func() {
			if iconButton(c, "panel-left", 28, 15, palette.textTertiary, "Toggle sidebar") {
				a.sidebarOpen = !a.sidebarOpen
			}
			if historyButton(c, "arrow-left", "Go back", a.navIndex > 0) {
				a.navigateHistory(-1)
			}
			if historyButton(c, "arrow-right", "Go forward", a.navIndex >= 0 && a.navIndex+1 < len(a.navHistory)) {
				a.navigateHistory(1)
			}
			revealEnabled := a.mode == modeSessions || a.mode == modeSourceControl
			revealTip := "Reveal project folder"
			if !revealEnabled {
				revealTip = "No folder to reveal here"
			}
			if revealButton(c, revealEnabled && a.selectedProject() != nil, revealTip) {
				a.revealProjectFolder()
			}
		})
		ui.Row(c).Grow(1).FillHeight().DragWindow()
		ui.Row(c).Height(topbarHeight).Gap(0).Padding(0, bar.Right, 0, 0).
			AlignItems(ui.Center).Children(func() {
			rightEnabled := a.mode == modeSessions
			rightTip := "Toggle right panel"
			if !rightEnabled {
				rightTip = "Right panel is unavailable here"
			}
			toggle := ui.ButtonBase(c).Label(rightTip).Tooltip(rightTip).Size(28, 28).
				Center().Radius(7)
			if !rightEnabled {
				toggle.Opacity(0.35)
			} else {
				if toggle.Hovered() {
					toggle.Background(palette.overlay)
				}
				if toggle.Pressed() {
					toggle.Background(palette.overlayStrong)
				}
			}
			toggle.Children(func() {
				icon(c, "panel-right", 15, palette.textTertiary)
			})
			if rightEnabled && toggle.Clicked() {
				a.rightOpen = !a.rightOpen
			}
		})
	})
}

func (a *app) modeRail(c *ui.Context) {
	entries := []struct {
		m    mode
		icon string
		tip  string
	}{
		{modeSessions, "message-square-text", "Sessions"},
		{modeSourceControl, "file-diff", "Source control"},
		{modeUsage, "chart-column", "Usage"},
	}
	ui.Column(c).Width(modeRailWidth).FillHeight().Background(palette.rail).
		AlignItems(ui.Center).Children(func() {
		ui.Row(c).FillWidth().Height(topbarHeight).DragWindow()
		for _, entry := range entries {
			entry := entry
			a.railEntry(c, entry.icon, entry.tip, a.mode == entry.m, func() {
				if entry.m == modeSourceControl {
					a.openSourceControl()
				} else {
					a.mode = entry.m
				}
				a.cancelProviderAdd()
			})
		}
		ui.Row(c).Grow(1)
		services := c.Services()
		a.railEntry(c, "info", "Help", false, func() {
			services.OpenURL("https://github.com/roketskiy/fintwind#readme")
		})
		a.railEntry(c, "settings", "Settings", a.mode == modeSettings, func() {
			a.openSettingsPage(a.settings.page)
		})
		ui.Row(c).Height(4)
	})
}

// railEntry is one mode-rail square: 48 high, 20 icon, 2x20 active bar.
func (a *app) railEntry(c *ui.Context, iconName, tip string, active bool, onClick func()) {
	c.Key("mode-rail-" + iconName)
	entry := ui.ButtonBase(c).Size(modeRailWidth, modeRailWidth).Justify(ui.Center).
		AlignItems(ui.Center).Cursor(ui.CursorPointer).Tooltip(tip).Label(tip)
	entry.Children(func() {
		color := palette.textTertiary
		if active || entry.Hovered() {
			color = palette.text
		}
		icon(c, iconName, 20, color)
		if active {
			ui.Row(c).Absolute().Left(0).Top(14).Size(2, 20).Background(palette.text)
		}
	})
	if entry.Clicked() {
		onClick()
	}
}

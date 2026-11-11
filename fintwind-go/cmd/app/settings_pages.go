package main

import (
	"strings"

	"github.com/egoist/mygo/ui"
)

// Shared geometry from the Rust settings list panes.
const mailListWidth = 264.0

// mailRoot is the full-bleed row the mail pages render instead of the
// titled 760px document column.
func (a *app) mailRoot(c *ui.Context, left, right func()) {
	ui.Row(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).Width(mailListWidth).FillHeight().Shrink(0).
			BorderWidth(0, 1, 0, 0).BorderColor(palette.border).Children(left)
		ui.Column(c).Grow(1).MinWidth(0).FillHeight().Children(right)
	})
}

// mailPaneHeader is the 13px medium title at pt14 px16.
func mailPaneHeader(c *ui.Context, title string) {
	ui.Text(c, title).FontSize(13).FontWeight(500).TextColor(palette.text).
		Margin(14, 16, 0, 16)
}

// mailSectionLabel mirrors section_label: 9.5 semibold tertiary, uppercase,
// pt 2 on the first section else 16, pb 4, px 9.
func mailSectionLabel(c *ui.Context, label string, first bool) {
	top := float32(16)
	if first {
		top = 2
	}
	ui.Text(c, strings.ToUpper(label)).FontSize(9.5).FontWeight(600).
		TextColor(palette.textTertiary).Margin(top, 9, 4, 9)
}

// mailTile is the 28x28 overlay icon box inside roster rows.
func mailTile(c *ui.Context, iconName string, active bool) {
	color := palette.textSecondary
	if active {
		color = palette.text
	}
	ui.Row(c).Size(28, 28).Shrink(0).Radius(2).Background(palette.overlay).
		Justify(ui.Center).AlignItems(ui.Center).Children(func() {
		icon(c, iconName, 14, color)
	})
}

// mailRow is one roster entry: px9 py7, radius 8, gap 9. Selected rows sit on
// sidebarItem (accent 0.13 in Rust); the rest hover/press on the overlays.
func (a *app) mailRow(c *ui.Context, label string, selected bool, onClick func(), tile func(), tail func()) {
	row := ui.ButtonBase(c).Label(label).FillWidth().Padding(7, 9).
		Radius(8).Gap(9).Justify(ui.Start).AlignItems(ui.Center)
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
		if tile != nil {
			tile()
		}
		ui.Text(c, label).FontSize(12.5).FontWeight(500).
			TextColor(palette.text).SingleLine().Ellipsis("...")
		if tail != nil {
			tail()
		}
	})
	if row.Clicked() && onClick != nil {
		onClick()
	}
}

// mailStatusDot is the 8px presence dot at the end of provider rows.
func mailStatusDot(c *ui.Context, active bool) {
	color := palette.textGhost
	if active {
		color = palette.text
	}
	ui.Row(c).Size(8, 8).Shrink(0).Radius(4).Background(color)
}

// mailAddRow is the dashed-style add entry: bordered, plus icon, hover lifts
// to overlay. `adding` marks the open-form state with the accent border.
func mailAddRow(c *ui.Context, label string, adding bool) bool {
	row := ui.ButtonBase(c).Label(label).FillWidth().Padding(7, 9).
		Radius(8).Gap(9).Justify(ui.Start).AlignItems(ui.Center).
		Border(1, palette.borderStrong).Background(palette.surface)
	if adding {
		row.Border(1, palette.accent)
	}
	row.Children(func() {
		iconColor := palette.textTertiary
		textColor := palette.textSecondary
		if adding {
			iconColor = palette.accent
			textColor = palette.text
		}
		icon(c, "plus", 13, iconColor)
		ui.Text(c, label).FontSize(12.5).TextColor(textColor)
	})
	if !adding {
		if row.Hovered() {
			row.Background(palette.overlay)
		}
		if row.Pressed() {
			row.Background(palette.overlayStrong)
		}
	}
	return row.Clicked()
}

// mailFooter is the h26 caption strip under the roster, over a top hairline.
// A dash reads as unknown, never as a zero count.
func mailFooter(c *ui.Context, caption string, centered bool) {
	footer := ui.Row(c).FillWidth().Height(26).Padding(0, 12).
		BorderWidth(1, 0, 0, 0).BorderColor(palette.border).AlignItems(ui.Center)
	if centered {
		footer.Justify(ui.Center)
	}
	footer.Children(func() {
		ui.Text(c, caption).FontSize(9.5).TextColor(palette.textMuted)
	})
}

// mailStatusRow is the fixed px18 py16 11px tertiary line used for
// page-level states (offline, scanning, empty roster).
func mailStatusRow(c *ui.Context, message string) {
	ui.Text(c, message).FontSize(11).TextColor(palette.textTertiary).
		Margin(16, 18, 0, 18)
}

// mailDetailPlaceholder centers a ghost icon over a muted caption, the way
// the Skills/MCP panes read before anything is selected.
func mailDetailPlaceholder(c *ui.Context, iconName, caption string) {
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Justify(ui.Center).
		AlignItems(ui.Center).Gap(8).Children(func() {
		icon(c, iconName, 22, palette.textGhost)
		ui.Text(c, caption).FontSize(11).TextColor(palette.textMuted)
	})
}

// mailEnabledBadge mirrors enabled_badge: a 9.5 pill, tinted on accent while
// enabled, quiet on overlay while off.
func mailEnabledBadge(c *ui.Context, enabled bool, label string) {
	badge := ui.Row(c).Padding(2, 7).Radius(10).AlignItems(ui.Center)
	if enabled {
		badge.Background(ui.RGBA(palette.accent.R, palette.accent.G, palette.accent.B, 0.14))
	} else {
		badge.Background(palette.overlay)
	}
	badge.Children(func() {
		color := palette.textTertiary
		if enabled {
			color = palette.accent
		}
		ui.Text(c, label).FontSize(9.5).TextColor(color)
	})
}

// mailDetailTile is the 38x38 overlay tile that opens every detail header.
func mailDetailTile(c *ui.Context, iconName string, size float32) {
	ui.Row(c).Size(38, 38).Shrink(0).Radius(9).Background(palette.overlay).
		Justify(ui.Center).AlignItems(ui.Center).Children(func() {
		icon(c, iconName, size, palette.textSecondary)
	})
}

// ---------------------------------------------------------------- providers

func (a *app) providersSettings(c *ui.Context) {
	a.mailRoot(c, func() {
		a.providersListPane(c)
	}, func() {
		a.providersDetail(c)
	})
}

// -------------------------------------------------------------------- skills

// skillsSettings is render_skills_settings: search plus source filter over
// the roster, honest unloaded states, placeholder detail. No scan runs here.
func (a *app) skillsSettings(c *ui.Context) {
	a.mailRoot(c, func() {
		a.skillsListPane(c)
	}, func() {
		a.skillsDetail(c)
	})
}

// ------------------------------------------------------------------ mcp servers

// mcpSettings is render_mcp_page: title plus search, Local/Remote sections
// hidden while empty, the add row, and the detail placeholder or add form.
func (a *app) mcpSettings(c *ui.Context) {
	a.mailRoot(c, func() {
		a.mcpListPane(c)
	}, func() {
		a.mcpDetail(c)
	})
}

// ------------------------------------------------------------ mcp marketplace

// mcpMarketSettings is render_mcp_market_page: title, search plus category
// filter, the honest unloaded roster, and the entry placeholder.
func (a *app) mcpMarketSettings(c *ui.Context) {
	a.mailRoot(c, func() {
		a.marketListPane(c)
	}, func() {
		a.marketDetail(c)
	})
}

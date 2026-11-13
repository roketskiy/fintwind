package main

import (
	"time"

	"github.com/ELY-Open-Source/CrychicUI/button"
	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/motion"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

// themeLayer washes a fill token over a base, like the lobe button hover
// states do: the button base colors stay on theme tokens instead of the
// hardcoded palette overlays.
func themeLayer(base, top ui.Color) ui.Color {
	return base.Mix(ui.RGB(top.R, top.G, top.B), float32(top.A)/255)
}

// sidebarFilterSearch is the file/branch filter field for the review and
// source control sidebars: lobe SearchBar Small geometry (h24) with a thin
// 1px focus border and no outer glow. lobe paints Shadow spread 2 on focus,
// which reads too heavy in these narrow columns.
func sidebarFilterSearch(c *ui.Context, key, placeholder string, value *string) {
	tk := theme.TokensOf(c)
	c.Key(key + "-shell")
	row := ui.Row(c).FillWidth().MinHeight(24).Padding(0, 8).Radius(tk.BorderRadiusSM).
		Gap(6).AlignItems(ui.Center).Background(tk.BgContainer).
		Border(1, tk.BorderSecondary)
	row.Children(func() {
		icon(c, "search", 12, tk.TextPlaceholder)
		c.Key(key)
		input := ui.TextInputBase(c, value).Label(placeholder).Placeholder(placeholder).
			PlaceholderColor(tk.TextPlaceholder).
			Grow(1).MinWidth(0).FontSize(uiPx(12)).TextColor(tk.Text).
			FocusRing(false).
			OnTextKey(func(k ui.TextKey) bool {
				if k.Key != ui.KeyEscape || k.Text() == "" {
					return false
				}
				k.SetText("")
				return true
			})
		if input.Focused() {
			row.Border(1, tk.Primary)
		}
		if *value != "" {
			clearColor := tk.TextTertiary
			c.Key(key + "-clear")
			clear := ui.ButtonBase(c).Label(tr("Clear")).Size(16, 16).Radius(8).
				Center().Shrink(0).Cursor(ui.CursorPointer)
			if clear.Hovered() {
				clearColor = tk.TextSecondary
			}
			clear.TextColor(clearColor)
			clear.Children(func() {
				icon(c, "x", 12, clearColor)
			})
			if clear.Clicked() {
				*value = ""
			}
		}
	})
}

func componentTextButton(c *ui.Context, key, label string, primary bool) ui.Element {
	c.Key(key)
	tk := theme.TokensOf(c)
	control := button.Ghost(c, label).
		Scale(1, 1).Height(32).Padding(0, 11).Radius(7).
		AlignItems(ui.Center).Justify(ui.Center).
		FontSize(uiPx(12.5)).TextColor(tk.TextSecondary).
		Border(1, tk.Border).Background(tk.BgContainer)
	if control.Hovered() {
		control.Background(themeLayer(tk.BgContainer, tk.FillTertiary)).TextColor(tk.Text)
	}
	if control.Pressed() {
		control.Background(themeLayer(tk.BgContainer, tk.FillSecondary)).TextColor(tk.Text)
	}
	if primary {
		bg := tk.Primary
		if control.Hovered() {
			bg = tk.PrimaryHover
		}
		if control.Pressed() {
			bg = tk.PrimaryActive
		}
		control.Padding(0, 12).Border(0, ui.Transparent).
			Background(bg).TextColor(tk.PrimaryForeground).FontWeight(500)
	}
	return control
}

func componentIconButton(c *ui.Context, key, iconName string, enabled bool) ui.Element {
	c.Key(key)
	tk := theme.TokensOf(c)
	control := ui.ButtonBase(c).Size(28, 28).Radius(7).Center().
		Background(ui.Transparent)
	if !enabled {
		control.Disabled(true)
		control.Children(func() {
			icon(c, iconName, 15, tk.TextTertiary)
		})
		return control
	}
	if control.Hovered() {
		control.Background(tk.FillSecondary)
	}
	if control.Pressed() {
		control.Background(tk.Fill)
	}
	control.Children(func() {
		icon(c, iconName, 15, tk.TextSecondary)
	})
	return control
}

type themeSegmentMatch struct{ selected *int }

var themeSegmentPress = ui.Animation{Duration: 120 * time.Millisecond, Ease: motion.EaseOut}

// themeSegmented is a small two-option switch with the accent pill: the
// selected option sits on accentFill in onAccent, the other in secondary
// text. Same keyboard behavior as the lobe switch (SegmentedBase), but the
// selected color follows the app theme instead of lobe tokens.
func themeSegmented(c *ui.Context, selected *int, options []string) ui.Element {
	parts := ui.SegmentedBase(c, selected, len(options))
	track := parts.Track.Padding(3).Gap(4).Radius(10).Border(1, palette.border)
	slide := ui.ElementTransition{Duration: 240 * time.Millisecond, Ease: motion.EaseOut, Match: themeSegmentMatch{selected}}
	track.Children(func() {
		for i, option := range options {
			chosen := i == *selected
			seg := parts.Segment(i).Height(26).Padding(0, 10).Radius(8).
				FontSize(uiPx(12)).FontWeight(500).Cursor(ui.CursorPointer)
			text := palette.textSecondary
			if chosen {
				text = palette.onAccent
			} else if seg.Hovered() {
				text = palette.text
			}
			pose := ui.Pose{}
			if seg.Pressed() {
				pose.Scale = 0.98
			}
			if !chosen && seg.Hovered() {
				seg.Background(palette.overlay)
			}
			seg.TextColor(text).AnimatePose(pose, themeSegmentPress)
			seg.Children(func() {
				if chosen {
					ui.Box(c).Absolute().Top(0).Left(0).Right(0).Bottom(0).
						Radius(8).Background(palette.accentFill).ZIndex(-1).Transition(slide)
				}
				ui.Text(c, option).SingleLine()
			})
		}
	})
	return track
}

package main

import (
	"time"

	"github.com/egoist/mygo/ui"
)

const (
	usageHeatCell = 22
	usageHeatGap  = 4
)

func (a *app) usageHeatmap(c *ui.Context) {
	c.Key("usage-activity")
	usagePanel(c, "history", tr("Activity"), tr("26 weeks · Direct tokens"), func() {
		heat := a.usage.History.Heat
		if len(heat) == 0 {
			label := tr("No activity available")
			if a.usage.History.Status == "error" {
				label = tr("Activity unavailable")
			}
			ui.Text(c, label).FontSize(uiPx(12)).TextColor(palette.textTertiary)
			return
		}
		if len(heat) > 182 {
			heat = heat[len(heat)-182:]
		}
		var maximum uint64
		for _, day := range heat {
			maximum = max(maximum, day.Direct)
		}
		first := heat[0].Date
		start := time.Date(first.Year(), first.Month(), first.Day(), 0, 0, 0, 0, first.Location())
		start = start.AddDate(0, 0, -(int(start.Weekday())+6)%7)
		last := heat[len(heat)-1].Date.Format("2006-01-02")
		at := 0
		ui.Row(c).FillWidth().Gap(10).Children(func() {
			ui.Column(c).Width(29).Gap(usageHeatGap).Children(func() {
				ui.Box(c).Height(16)
				for weekday := 0; weekday < 7; weekday++ {
					label := ""
					switch weekday {
					case 0:
						label = tr("Mon")
					case 2:
						label = tr("Wed")
					case 4:
						label = tr("Fri")
					}
					ui.Row(c).Height(usageHeatCell).AlignItems(ui.Center).Children(func() {
						ui.Text(c, label).FontSize(uiPx(10)).TextColor(palette.textTertiary).SingleLine()
					})
				}
			})
			ui.Row(c).Gap(usageHeatGap).FocusGroup(ui.Horizontal | ui.Vertical).Label(tr("Daily activity calendar")).Children(func() {
				for week := 0; week < 26; week++ {
					monday := start.AddDate(0, 0, week*7)
					c.Key("usage-week-" + monday.Format("2006-01-02"))
					ui.Column(c).Width(usageHeatCell).Gap(usageHeatGap).Children(func() {
						month := ""
						if week == 0 || monday.Month() != monday.AddDate(0, 0, -7).Month() {
							month = monday.Format("Jan")
						}
						ui.Text(c, month).Height(16).FontSize(uiPx(9.5)).TextColor(palette.textTertiary).SingleLine().Shrink(0)
						for weekday := 0; weekday < 7; weekday++ {
							date := monday.AddDate(0, 0, weekday)
							key := date.Format("2006-01-02")
							if key > last || at >= len(heat) || heat[at].Date.Format("2006-01-02") != key {
								ui.Box(c).Size(usageHeatCell, usageHeatCell)
								continue
							}
							day := heat[at]
							at++
							a.usageHeatCell(c, day, maximum)
						}
					})
				}
			})
		})
		ui.Row(c).FillWidth().Wrap().Gap(12).AlignItems(ui.Center).Children(func() {
			if selected := a.usage.SelectedDay; selected != "" {
				ui.Text(c, selected+tr(" selected")).Grow(1).MinWidth(0).FontSize(uiPx(11)).TextColor(palette.textSecondary)
			} else {
				ui.Text(c, tr("Input + generated, excluding cache")).Grow(1).MinWidth(0).FontSize(uiPx(10.5)).TextColor(palette.textTertiary)
			}
			ui.Row(c).Gap(4).AlignItems(ui.Center).Children(func() {
				ui.Text(c, tr("Less")).FontSize(uiPx(10)).TextColor(palette.textTertiary).Margin(0, 4, 0, 0)
				for level := 0; level < 5; level++ {
					ui.Box(c).Size(10, 10).Radius(2).Background(usageHeatColor(level)).Border(1, palette.border)
				}
				ui.Text(c, tr("More")).FontSize(uiPx(10)).TextColor(palette.textTertiary).Margin(0, 0, 0, 4)
			})
		})
	})
}

func (a *app) usageHeatCell(c *ui.Context, day usageHeatDay, maximum uint64) {
	key := day.Date.Format("2006-01-02")
	level := 0
	if day.Direct > 0 && maximum > 0 {
		level = 1 + min(3, int(float64(day.Direct)/float64(maximum)*4))
	}
	tooltip := trf("%s · %d direct tokens · %d cached tokens · %d sessions", key, day.Direct, day.Total-min(day.Total, day.Direct), day.Sessions)
	c.Key("usage-day-" + key)
	cell := ui.ButtonBase(c).Label(key).Description(tooltip).Tooltip(tooltip).
		Size(usageHeatCell, usageHeatCell).Radius(4).Background(usageHeatColor(level)).Border(1, palette.border)
	if a.usage.SelectedDay == key {
		cell.Border(2, palette.text)
	} else if cell.Hovered() || cell.Pressed() {
		cell.Border(1, palette.textSecondary)
	}
	cell.OnClick(func() {
		a.usage.UI.chartIndex = -1
		if a.usage.SelectedDay == key {
			if a.usage.Range == "day" {
				a.setUsageRange("30d")
			} else {
				a.selectUsageDay("")
			}
		} else {
			a.selectUsageDay(key)
		}
	})
}

func usageHeatColor(level int) ui.Color {
	switch level {
	case 1:
		return palette.raised.Mix(palette.accent, 0.28)
	case 2:
		return palette.raised.Mix(palette.accent, 0.48)
	case 3:
		return palette.raised.Mix(palette.accent, 0.72)
	case 4:
		return palette.accent
	default:
		return palette.overlayStrong
	}
}

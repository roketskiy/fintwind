package main

import (
	"fmt"
	"time"

	"github.com/egoist/mygo/ui"
)

type usageUIState struct {
	scroll                  ui.ScrollState
	chartIndex              int
	chartHover              int
	chartSource             string
	rangeValue, rangeSource string
	rangeMenu               bool
}

var usageRangeChoices = []struct{ value, label string }{
	{"7d", "Last 7 days"},
	{"30d", "Last 30 days"},
	{"90d", "Last 90 days"},
	{"all", "All time"},
}

func (a *app) usageView(c *ui.Context) {
	a.ensureUsageFresh()
	ui.Column(c).Grow(1).MinWidth(0).FillHeight().Background(palette.surface).
		Radius(workspaceRadius, 0, 0, workspaceRadius).Clip().Children(func() {
		c.Key("usage-scroll")
		ui.Scroll(c).TrackScroll(&a.usage.UI.scroll).FillWidth().Grow(1).MinHeight(0).Children(func() {
			ui.Column(c).FillWidth().Padding(24, 24, 40, 24).Children(func() {
				ui.Column(c).FillWidth().MaxWidth(1280).MinWidth(0).Margin(0, ui.Auto).Gap(18).Children(func() {
					a.usageHeader(c)
					a.usageHistoryBody(c)
					a.usageSubscriptionsSection(c)
					a.usageHeatmap(c)
					if view := a.currentUsageView(); view != nil {
						a.usageRankings(c, view)
					}
				})
			})
		})
	})
}

func (a *app) usageHeader(c *ui.Context) {
	ui.Row(c).FillWidth().Wrap().Gap(12).AlignItems(ui.Center).Children(func() {
		ui.Column(c).Grow(1).Basis(280).MinWidth(0).Gap(5).Children(func() {
			ui.Text(c, tr("Usage")).FontSize(22).FontWeight(600).TextColor(palette.text)
			ui.Text(c, tr("OpenCode session store")).FontSize(11.5).TextColor(palette.textTertiary)
		})
		ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
			a.usageRangeSelect(c)
			usageRefreshControl(c, "usage-history-refresh", "Refresh history", a.usage.History.Status == "loading", func() {
				a.refreshUsageHistory(true)
			})
		})
	})
}

func (a *app) usageRangeSelect(c *ui.Context) {
	s := &a.usage.UI
	if s.rangeSource != a.usage.Range || s.rangeValue == "" {
		s.rangeValue, s.rangeSource = a.usage.Range, a.usage.Range
	}
	label := tr("Last 30 days")
	for _, option := range usageRangeChoices {
		if option.value == s.rangeValue {
			label = option.label
		}
	}
	c.Key("usage-range")
	parts := ui.SelectBaseOpen(c, &s.rangeValue, &s.rangeMenu)
	trigger := parts.Trigger.Label(tr("Usage date range")).Width(146).Height(30).Padding(0, 10).
		Radius(7).Gap(8).AlignItems(ui.Center).Border(1, palette.borderStrong).Background(palette.overlay)
	trigger.Children(func() {
		icon(c, "history", 12, palette.textTertiary)
		ui.Text(c, label).Grow(1).MinWidth(0).FontSize(12).TextColor(palette.textSecondary).SingleLine()
		icon(c, "chevrons-up-down", 11, palette.textTertiary)
	})
	parts.Popup(func(panel ui.Element) {
		panel.Width(170).Padding(4).Radius(9).Background(palette.raised).Border(1, palette.borderStrong)
		ui.Column(c).FillWidth().Children(func() {
			for _, option := range usageRangeChoices {
				c.Key(option.value)
				item := parts.Item(option.value).Label(option.label).FillWidth().Height(30).Padding(0, 8).
					Radius(5).Gap(8).AlignItems(ui.Center)
				if item.Highlighted() || item.Hovered() {
					item.Background(palette.overlayStrong)
				}
				item.Children(func() {
					ui.Text(c, option.label).Grow(1).MinWidth(0).FontSize(12).TextColor(palette.textSecondary)
					if option.value == s.rangeValue {
						icon(c, "check", 12, palette.textSecondary)
					}
				})
			}
		})
	})
	trigger.OnChange(func() {
		a.usage.UI.chartIndex = -1
		a.setUsageRange(s.rangeValue)
	})
}

func (a *app) usageHistoryBody(c *ui.Context) {
	history := &a.usage.History
	view := a.currentUsageView()
	if history.Status == "error" {
		usageNotice(c, "alert", "History unavailable", history.Error, palette.warningText)
		if view != nil && !history.UpdatedAt.IsZero() {
			ui.Text(c, usageUpdatedLabel(history.UpdatedAt)).FontSize(10.5).TextColor(palette.textTertiary)
		}
	} else if history.Status == "loading" {
		usageNotice(c, "rotate-cw", "Updating history", usageUpdatedLabel(history.UpdatedAt), palette.textTertiary)
	} else if !history.UpdatedAt.IsZero() {
		ui.Text(c, usageUpdatedLabel(history.UpdatedAt)).FontSize(10.5).TextColor(palette.textTertiary)
	}
	if view == nil {
		if history.Status != "error" && history.Status != "loading" {
			usageNotice(c, "chart-column", "No usage available", "Select a workspace to load its session store.", palette.textSecondary)
		}
		return
	}
	if a.usage.SelectedDay != "" {
		ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
			componentTextButton(c, "usage-back-range", tr("Back to range"), false).OnClick(func() {
				a.usage.UI.chartIndex = -1
				a.selectUsageDay("")
			})
			ui.Text(c, a.usage.SelectedDay).FontSize(12).TextColor(palette.textSecondary)
		})
	}
	if history.Stats != nil && history.Stats.Truncated {
		usageNotice(c, "alert", "Partial history", fmt.Sprintf("Scan limited to %d sessions.", history.Stats.SessionsScanned), palette.warningText)
	}
	a.usageSummary(c, view.Totals)
	a.usageTrend(c, view)
}

func usageRefreshControl(c *ui.Context, key, label string, loading bool, refresh func()) {
	control := componentTextButton(c, key, choice(loading, tr("Refreshing..."), label), false).
		Label(label).Disabled(loading)
	control.OnClick(refresh)
}

func usageNotice(c *ui.Context, iconName, title, detail string, color ui.Color) {
	ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
		icon(c, iconName, 13, color)
		ui.Text(c, title).FontSize(12).TextColor(color).Shrink(0)
		if detail != "" {
			ui.Text(c, detail).Grow(1).MinWidth(0).FontSize(11.5).TextColor(palette.textTertiary).Wrap()
		}
	})
}

func usageUpdatedLabel(at time.Time) string {
	if at.IsZero() {
		return ""
	}
	return "Last updated " + at.Local().Format("Jan 2, 15:04")
}

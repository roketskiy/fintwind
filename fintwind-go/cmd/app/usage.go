package main

import (
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
	// dayDate is the date picker's model; dayDefault selects today once
	// the aggregates land after opening the usage mode.
	dayDate    time.Time
	dayDefault bool
}

var usageRangeChoices = []struct{ value, label string }{
	{"day", "Single day"},
	{"7d", "Last 7 days"},
	{"30d", "Last 30 days"},
	{"90d", "Last 90 days"},
	{"all", "All time"},
}

func (a *app) usageView(c *ui.Context) {
	a.ensureUsageFresh()
	view := ui.Column(c).Grow(1).MinWidth(0).FillHeight().Background(palette.surface).
		Radius(workspaceRadius, 0, 0, workspaceRadius).Clip()
	view = workspaceShadow(view)
	view.Children(func() {
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
			ui.Text(c, tr("Usage")).FontSize(uiPx(22)).FontWeight(600).TextColor(palette.text)
			ui.Text(c, tr("OpenCode session store")).FontSize(uiPx(11.5)).TextColor(palette.textTertiary)
		})
		ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
			a.usageDayPicker(c)
			a.usageRangeSelect(c)
			usageRefreshControl(c, "usage-history-refresh", tr("Refresh history"), a.usage.History.Status == "loading", func() {
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
			label = tr(option.label)
		}
	}
	c.Key("usage-range")
	parts := ui.SelectBaseOpen(c, &s.rangeValue, &s.rangeMenu)
	trigger := parts.Trigger.Label(tr("Usage date range")).Width(146).Height(30).Padding(0, 10).
		Radius(7).Gap(8).AlignItems(ui.Center).Border(1, palette.borderStrong).Background(palette.overlay)
	trigger.Children(func() {
		icon(c, "history", 12, palette.textTertiary)
		ui.Text(c, label).Grow(1).MinWidth(0).FontSize(uiPx(12)).TextColor(palette.textSecondary).SingleLine()
		icon(c, "chevrons-up-down", 11, palette.textTertiary)
	})
	parts.Popup(func(panel ui.Element) {
		panel.Width(170).Padding(4).Radius(9).Background(palette.raised).Border(1, palette.borderStrong)
		ui.Column(c).FillWidth().Children(func() {
			for _, option := range usageRangeChoices {
				c.Key(option.value)
				item := parts.Item(option.value).Label(tr(option.label)).FillWidth().Height(30).Padding(0, 8).
					Radius(5).Gap(8).AlignItems(ui.Center)
				if item.Highlighted() || item.Hovered() {
					item.Background(palette.overlayStrong)
				}
				item.Children(func() {
					ui.Text(c, tr(option.label)).Grow(1).MinWidth(0).FontSize(uiPx(12)).TextColor(palette.textSecondary)
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

// usageDayPicker switches the view to one day: the calendar picks the
// date, the view follows when that day has data.
func (a *app) usageDayPicker(c *ui.Context) {
	s := &a.usage.UI
	if s.dayDate.IsZero() {
		s.dayDate = time.Now()
	}
	c.Key("usage-day")
	pick := ui.DateInput(c, &s.dayDate).Height(30).Padding(0, 10).Radius(7).
		Border(1, palette.borderStrong).Background(palette.overlay)
	if pick.Changed() {
		s.dayDefault = false
		key := s.dayDate.Format("2006-01-02")
		if _, ok := a.usage.History.Days[key]; ok {
			a.usage.UI.chartIndex = -1
			a.selectUsageDay(key)
		} else if a.usage.SelectedDay != "" {
			if current, err := time.Parse("2006-01-02", a.usage.SelectedDay); err == nil {
				s.dayDate = current
			}
		} else if latest := a.latestUsageDay(); latest != "" {
			if day, err := time.Parse("2006-01-02", latest); err == nil {
				s.dayDate = day
			}
		} else {
			s.dayDate = time.Now()
		}
	}
}

// latestUsageDay is the newest day with data, for defaulting the single
// day when today has none yet.
func (a *app) latestUsageDay() string {
	best := ""
	for key := range a.usage.History.Days {
		if key > best {
			best = key
		}
	}
	return best
}

// applyDefaultUsageDay selects the single day to show: today when it has
// data, else the newest day with data. The range follows to "day" so the
// dropdown always names the visible view.
func (a *app) applyDefaultUsageDay() {
	if latest := a.latestUsageDay(); latest != "" {
		if day, err := time.Parse("2006-01-02", latest); err == nil {
			a.usage.UI.dayDate = day
		}
		a.selectUsageDay(latest)
		a.usage.Range = "day"
		return
	}
	today := time.Now()
	a.usage.UI.dayDate = today
	a.selectUsageDay(today.Format("2006-01-02"))
	if a.usage.SelectedDay != "" {
		a.usage.Range = "day"
	}
}

// defaultUsageDay shows a single day when the usage mode opens; the
// selection lands once the aggregates arrive when they have not yet.
func (a *app) defaultUsageDay() {
	a.usage.UI.dayDate = time.Now()
	a.usage.UI.dayDefault = true
	a.usage.UI.chartIndex = -1
	a.applyDefaultUsageDay()
}

func (a *app) usageHistoryBody(c *ui.Context) {
	history := &a.usage.History
	view := a.currentUsageView()
	if history.Status == "error" {
		usageNotice(c, "alert", tr("History unavailable"), history.Error, palette.warningText)
		if view != nil && !history.UpdatedAt.IsZero() {
			ui.Text(c, usageUpdatedLabel(history.UpdatedAt)).FontSize(uiPx(10.5)).TextColor(palette.textTertiary)
		}
	}
	// No loading row: it flashes in and out on every refresh and shoves
	// the content below. Stale numbers stay put until new ones land.
	if view == nil {
		if history.Status != "error" && history.Status != "loading" {
			usageNotice(c, "chart-column", tr("No usage available"), tr("Select a workspace to load its session store."), palette.textSecondary)
		}
		return
	}
	if history.Stats != nil && history.Stats.Truncated {
		usageNotice(c, "alert", tr("Partial history"), trf("Scan limited to %d sessions.", history.Stats.SessionsScanned), palette.warningText)
	}
	a.usageSummary(c, view.Totals)
	a.usageTrend(c, view)
}

func usageRefreshControl(c *ui.Context, key, label string, loading bool, refresh func()) {
	// Pinned label: swapping in a longer "Refreshing..." text changes the
	// button width and jumps the header on every refresh.
	control := componentTextButton(c, key, label, false).
		Label(label).Disabled(loading)
	control.OnClick(refresh)
}

func usageNotice(c *ui.Context, iconName, title, detail string, color ui.Color) {
	ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
		icon(c, iconName, 13, color)
		ui.Text(c, title).FontSize(uiPx(12)).TextColor(color).Shrink(0)
		if detail != "" {
			ui.Text(c, detail).Grow(1).MinWidth(0).FontSize(uiPx(11.5)).TextColor(palette.textTertiary).Wrap()
		}
	})
}

func usageUpdatedLabel(at time.Time) string {
	if at.IsZero() {
		return ""
	}
	return trf("Last updated %s", at.Local().Format("Jan 2, 15:04"))
}

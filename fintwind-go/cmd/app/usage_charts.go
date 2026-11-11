package main

import (
	"fmt"

	"github.com/ELY-Open-Source/CrychicUI/chart"
	"github.com/ELY-Open-Source/CrychicUI/kit"
	"github.com/egoist/mygo/ui"
	mc "github.com/egoist/mygo/ui/chart"
)

func (a *app) usageTrend(c *ui.Context, view *usageViewData) {
	meta := view.Start.Format("Jan 2, 2006") + " – " + view.End.Format("Jan 2, 2006")
	if a.usage.SelectedDay != "" {
		meta = a.usage.SelectedDay
	}
	source := a.usage.Range + "/" + a.usage.SelectedDay + "/" + fmt.Sprint(len(view.Daily))
	if a.usage.UI.chartSource != source || a.usage.UI.chartIndex >= len(view.Daily) {
		a.usage.UI.chartIndex, a.usage.UI.chartSource = -1, source
		a.usage.UI.chartHover = -1
	}
	c.Key("usage-trend-" + source)
	usagePanel(c, "chart-column", "Daily usage", meta, func() {
		if len(view.Daily) == 0 {
			usageChartEmpty(c)
			return
		}
		var maximum uint64
		labels := make([]string, len(view.Daily))
		input, output := make([]float64, len(labels)), make([]float64, len(labels))
		for i, day := range view.Daily {
			labels[i] = day.date.Format("Jan 2")
			input[i], output[i] = float64(day.input), float64(day.output)
			maximum = max(maximum, day.input, day.output)
		}
		if maximum == 0 {
			usageChartEmpty(c)
			return
		}
		if len(labels) == 1 {
			day := view.Daily[0]
			maximum = max(maximum, day.cache)
			ui.Column(c).FillWidth().Gap(14).Padding(12, 0).Children(func() {
				usageSingleDayLane(c, "Input", day.input, maximum, palette.accent)
				usageSingleDayLane(c, "Generated", day.output, maximum, palette.success)
				usageSingleDayLane(c, "Cached", day.cache, maximum, palette.textTertiary)
			})
		} else if len(labels) <= 7 {
			chart.Area(c, "Daily token usage", labels, []chart.Series{
				{Name: "Input", Values: input}, {Name: "Generated", Values: output},
			}, &a.usage.UI.chartIndex, chart.AreaOptions{Height: 190, Format: "%.0f"})
		} else {
			a.usageSparseTrend(c, view.Daily, maximum)
			ui.Row(c).Gap(18).Children(func() {
				usageLegend(c, "Input", palette.accent)
				usageLegend(c, "Generated", palette.success)
			})
		}
		if len(view.Daily) > 1 {
			ui.Row(c).FillWidth().Wrap().Gap(12).AlignItems(ui.Center).Children(func() {
				ui.Row(c).Gap(2).FocusGroup(ui.Horizontal).Children(func() {
					componentIconButton(c, "usage-chart-previous", "arrow-left", a.usage.UI.chartIndex != 0).
						Label(tr("Previous chart day")).Tooltip(tr("Previous day")).OnClick(func() {
						if a.usage.UI.chartIndex < 0 {
							a.usage.UI.chartIndex = len(view.Daily) - 1
						} else {
							a.usage.UI.chartIndex--
						}
					})
					componentIconButton(c, "usage-chart-next", "arrow-right", a.usage.UI.chartIndex != len(view.Daily)-1).
						Label(tr("Next chart day")).Tooltip(tr("Next day")).OnClick(func() {
						a.usage.UI.chartIndex = min(a.usage.UI.chartIndex+1, len(view.Daily)-1)
					})
				})
				at := a.usage.UI.chartIndex
				if at < 0 || at >= len(view.Daily) {
					return
				}
				day := view.Daily[at]
				ui.Text(c, day.date.Format("Jan 2, 2006")).FontSize(11.5).TextColor(palette.textSecondary)
				kit.Numeric(c, formatTokens(usageAdd(day.input, day.output))+" direct").FontFeatures("tnum").FontSize(11).TextColor(palette.text)
				ui.Text(c, fmt.Sprintf(tr("%d sessions"), day.sessions)).FontSize(11).TextColor(palette.textTertiary)
				componentTextButton(c, "usage-trend-select-day", tr("View day"), false).OnClick(func() {
					a.usage.UI.chartIndex = -1
					a.selectUsageDay(day.date.Format("2006-01-02"))
				})
			})
		}
	})
}

func usageSingleDayLane(c *ui.Context, label string, value, maximum uint64, color ui.Color) {
	ui.Column(c).FillWidth().Gap(8).Children(func() {
		ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
			usageLegend(c, label, color)
			ui.Box(c).Grow(1)
			kit.Numeric(c, formatTokens(value)).FontFeatures("tnum").FontSize(15).TextColor(palette.text)
		})
		fraction := float32(0)
		if maximum > 0 {
			fraction = float32(float64(value) / float64(maximum))
		}
		usageProgress(c, fraction, color)
	})
}

func (a *app) usageSparseTrend(c *ui.Context, days []usageDayBucket, maximum uint64) {
	xs, ins, outs := make([]float32, len(days)), make([]float32, len(days)), make([]float32, len(days))
	y := mc.NewLinear(0, float64(maximum)).Nice(4)
	y.Format = func(v float64) string { return formatTokens(uint64(max(0, v))) }
	var bottom []mc.Tick
	stride := max(1, (len(days)-1+4)/5)
	for i, day := range days {
		xs[i] = (float32(i) + 0.5) / float32(len(days))
		ins[i], outs[i] = y.At(float64(day.input)), y.At(float64(day.output))
		if i%stride == 0 || i == len(days)-1 && len(days)-1-(len(days)-1)/stride*stride >= stride/2 {
			bottom = append(bottom, mc.Tick{At: xs[i], Label: day.date.Format("Jan 2")})
		}
	}
	ticks := y.Ticks(4)
	mc.Plot(c, mc.Axes{Left: ticks, Bottom: bottom, Label: func(_ mc.Side, tick mc.Tick) {
		ui.Text(c, tick.Label).FontSize(10).TextColor(palette.textTertiary).NoWrap()
	}}, func(area ui.Element) {
		area.Label(tr("Daily token usage")).Draw(kit.Inside(0, func(p *ui.Painter, r ui.Rect) {
			for _, tick := range ticks {
				at := mc.Y(r, tick.At)
				p.Line(r.X, at, r.X+r.W, at, 1, palette.border)
			}
			p.FillPath(mc.Area(r, xs, nil, ins, mc.CurveMonotone), palette.accent.Alpha(0.12))
			p.FillPath(mc.Area(r, xs, nil, outs, mc.CurveMonotone), palette.success.Alpha(0.1))
			p.StrokePath(mc.Line(r, xs, ins, mc.CurveMonotone), 2, palette.accent)
			p.StrokePath(mc.Line(r, xs, outs, mc.CurveMonotone), 2, palette.success)
		}))
		describe := func(i int) string {
			day := days[i]
			return fmt.Sprintf("%s · Input %s · Generated %s · Cached %s · %d sessions",
				day.date.Format("2006-01-02"), formatTokens(day.input), formatTokens(day.output), formatTokens(day.cache), day.sessions)
		}
		mc.Select(c, area, &a.usage.UI.chartHover, xs, describe)
		if hover := a.usage.UI.chartHover; hover >= 0 && hover < len(xs) {
			area.Tooltip(describe(hover))
			if area.Clicked() {
				a.usage.UI.chartIndex = hover
			}
		}
		at := a.usage.UI.chartHover
		if at < 0 {
			at = a.usage.UI.chartIndex
		}
		if at >= 0 && at < len(xs) {
			mc.RuleX(c, xs[at], 1).Background(palette.textTertiary)
			mc.Point(c, xs[at], ins[at], 6, 6).Radius(3).Background(palette.accent)
			mc.Point(c, xs[at], outs[at], 6, 6).Radius(3).Background(palette.success)
		}
	}).FillWidth().Height(190).Gap(9)
}

func usageChartEmpty(c *ui.Context) {
	ui.Column(c).FillWidth().Height(160).Center().Gap(10).Children(func() {
		icon(c, "chart-column", 22, palette.textGhost)
		ui.Text(c, tr("No direct token usage in this period")).FontSize(12).TextColor(palette.textTertiary)
	})
}

func usageLegend(c *ui.Context, label string, color ui.Color) {
	ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
		ui.Box(c).Size(7, 7).Radius(2).Background(color)
		ui.Text(c, label).FontSize(11).TextColor(palette.textSecondary)
	})
}

package main

import (
	"math"
	"sort"

	"fintwind-go/internal/model"
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/chart"
)

type contextComponent struct {
	name   string
	tokens uint64
	opaque bool
	color  ui.Color
}

func contextComposition(c *ui.Context, usage *model.ContextUsage) {
	ui.Text(c, tr("Composition (estimated)")).FontSize(11.5).TextColor(palette.textSecondary)
	if usage == nil || usage.Composition == nil || usage.Composition.Version != 2 {
		ui.Text(c, tr("Composition not reported yet")).FontSize(11).TextColor(palette.textTertiary).Wrap()
		return
	}
	composition := usage.Composition
	entries := []contextComponent{
		{"System", composition.System, false, palette.textSecondary},
		{"Tools", composition.Tools, false, usageGaugeFill},
		{"Dialogue", composition.Messages, false, palette.accent},
		{"Reasoning", composition.Reasoning, composition.OpaqueReasoning, palette.warning},
		{"Tool calls", composition.ToolCalls, false, palette.warningText},
		{"Files", composition.Files, false, palette.textTertiary},
		{"Tool results", composition.ToolResults, false, palette.successText},
		{"Summary", composition.Memory, composition.OpaqueMemory, usageGaugeFill},
	}
	total := uint64(0)
	for _, entry := range entries {
		if math.MaxUint64-total < entry.tokens {
			total = math.MaxUint64
		} else {
			total += entry.tokens
		}
	}
	contextInspector(c, "Estimated tokens", "≈ "+formatTokens(total))
	positive := make([]contextComponent, 0, len(entries))
	for _, entry := range entries {
		if entry.tokens > 0 {
			positive = append(positive, entry)
		}
	}
	sort.SliceStable(positive, func(i, j int) bool { return positive[i].tokens > positive[j].tokens })
	if len(positive) > 0 {
		chart.Plot(c.Key("context-composition-treemap"), chart.Axes{}, func(area ui.Element) {
			area.Clip().Label(tr("Composition (estimated)"))
			contextTreemap(c, positive, 0, 0, 1, 1)
		}).FillWidth().Height(180)
	}
	for _, entry := range entries {
		value := "≈ " + formatTokens(entry.tokens)
		if entry.opaque {
			if entry.tokens == 0 {
				value = tr("Unmeasured")
			} else {
				value += " +"
			}
		}
		percent := float64(0)
		if total > 0 {
			percent = float64(entry.tokens) * 100 / float64(total)
		}
		detail := formatPercent(percent)
		if entry.opaque {
			detail = tr("Partial estimate")
		}
		ui.Row(c.Key("context-composition-" + entry.name)).FillWidth().Gap(6).AlignItems(ui.Center).Focusable().
			Label(tr(entry.name)).Description(detail + " · " + value).Tooltip(detail + " · " + value).Children(func() {
			ui.Box(c).Size(7, 7).Shrink(0).Radius(2).Background(entry.color)
			ui.Column(c).Grow(1).MinWidth(0).Children(func() {
				contextInspector(c, entry.name, value+" · "+detail)
			})
		})
	}
	if composition.Incomplete {
		ui.Text(c, tr("Partial estimate")).FontSize(11).TextColor(palette.textTertiary)
	}
}

// Split near half the weight along the longer side. Work stays bounded by
// the eight composition categories; zero values remain in the legend only.
func contextTreemap(c *ui.Context, entries []contextComponent, x, y, w, h float32) {
	if len(entries) == 1 {
		entry := entries[0]
		value := "≈ " + formatTokens(entry.tokens)
		if entry.opaque {
			value += " + · " + tr("Partial estimate")
		}
		chart.Rect(c.Key("context-tile-"+entry.name), chart.Span{Lo: x, Hi: x + w}, chart.Span{Lo: 1 - y - h, Hi: 1 - y}).
			Background(entry.color.Alpha(0.18)).Border(1, palette.raised).Clip().
			Focusable().Label(tr(entry.name)).Description(value).Tooltip(tr(entry.name) + " · " + value).
			Children(func() {
				// Tiny cells keep their exact area, with details in the legend.
				if w >= 0.22 && h >= 0.16 {
					ui.Column(c).Fill().Padding(5).Gap(2).Children(func() {
						ui.Text(c, tr(entry.name)).FontSize(11).FontWeight(600).TextColor(palette.text).SingleLine().Ellipsis("…")
						if h >= 0.28 {
							ui.Text(c, value).FontSize(10).TextColor(palette.textSecondary).SingleLine().Ellipsis("…")
						}
					})
				}
			})
		return
	}
	var total float64
	for _, entry := range entries {
		total += float64(entry.tokens)
	}
	weight := float64(entries[0].tokens)
	split := 1
	for split < len(entries)-1 && math.Abs(weight+float64(entries[split].tokens)-total/2) < math.Abs(weight-total/2) {
		weight += float64(entries[split].tokens)
		split++
	}
	fraction := float32(weight / total)
	// The panel is wider than the fixed-height plot.
	if w*1.5 >= h {
		cut := w * fraction
		contextTreemap(c, entries[:split], x, y, cut, h)
		contextTreemap(c, entries[split:], x+cut, y, w-cut, h)
	} else {
		cut := h * fraction
		contextTreemap(c, entries[:split], x, y, w, cut)
		contextTreemap(c, entries[split:], x, y+cut, w, h-cut)
	}
}

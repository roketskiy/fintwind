package main

import (
	"math"

	"fintwind-go/internal/model"
	"github.com/egoist/mygo/ui"
)

type contextComponent struct {
	name   string
	tokens uint64
	opaque bool
	color  ui.Color
}

func contextComposition(c *ui.Context, usage *model.ContextUsage) {
	ui.Text(c, tr("Composition (estimated)")).FontSize(uiPx(11.5)).TextColor(palette.textSecondary)
	if usage == nil || usage.Composition == nil || usage.Composition.Version != 2 {
		ui.Text(c, tr("Composition not reported yet")).FontSize(uiPx(11)).TextColor(palette.textTertiary).Wrap()
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
	// Static proportion strip in fixed category order. The library treemap
	// animates entrance, selection and every value change with no toggle,
	// which reads as constant sluggish motion here; plain boxes carry the
	// same shares with zero animation calls.
	positive := make([]contextComponent, 0, len(entries))
	for _, entry := range entries {
		if entry.tokens > 0 {
			positive = append(positive, entry)
		}
	}
	if len(positive) > 0 && total > 0 {
		c.Key("context-composition-strip")
		ui.Row(c).FillWidth().Height(12).Radius(6).Clip().
			Label(tr("Composition (estimated)")).Children(func() {
			for _, entry := range positive {
				share := float32(entry.tokens) / float32(total) * 100
				tip := tr(entry.name) + " · ≈ " + formatTokens(entry.tokens) +
					" · " + formatPercent(float64(entry.tokens)*100/float64(total))
				ui.Box(c).Height(12).WidthPercent(share).Background(entry.color).Tooltip(tip)
			}
		})
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
		ui.Text(c, tr("Partial estimate")).FontSize(uiPx(11)).TextColor(palette.textTertiary)
	}
}

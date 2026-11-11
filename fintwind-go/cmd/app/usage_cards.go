package main

import (
	"fmt"
	"math"

	"github.com/ELY-Open-Source/CrychicUI/kit"
	"github.com/egoist/mygo/ui"
)

func usagePanel(c *ui.Context, iconName, title, meta string, content func()) ui.Element {
	return ui.Column(c).FillWidth().MinWidth(0).Radius(12).Background(palette.overlay).Padding(4).Children(func() {
		ui.Row(c).FillWidth().Height(30).Padding(0, 9).Gap(7).AlignItems(ui.Center).Children(func() {
			icon(c, iconName, 12, palette.textTertiary)
			ui.Text(c, title).Grow(1).MinWidth(0).FontSize(11.5).FontWeight(500).TextColor(palette.textSecondary).
				SingleLine().Ellipsis("...")
			if meta != "" {
				ui.Text(c, meta).Grow(1).MinWidth(0).FontSize(10.5).TextColor(palette.textTertiary).
					TextAlign(ui.End).SingleLine().Ellipsis("...")
			}
		})
		ui.Column(c).FillWidth().MinWidth(0).Radius(9).Background(palette.raised).
			Border(1, palette.cardBorder).Padding(16).Gap(14).Children(content)
	})
}

func (a *app) usageSummary(c *ui.Context, totals usageTotals) {
	hitValue := "—"
	if hit, ok := usageCacheHit(totals); ok {
		hitValue = formatPercent(hit)
	}
	ui.Column(c).FillWidth().Gap(10).Children(func() {
		ui.Row(c).FillWidth().Wrap().Gap(12).Children(func() {
			usageMetric(c, "total", "chart-column", "Total tokens", formatTokens(totals.total()), "Input "+formatTokens(totals.input))
			usageMetric(c, "generated", "sparkle", "Generated", formatTokens(totals.generated()), "Output + reasoning")
			usageMetric(c, "sessions", "message-square-text", "Sessions", fmt.Sprint(totals.sessions), fmt.Sprintf("%d active days", totals.activeDays))
			usageMetric(c, "cache", "server", "Cache hit", hitValue, "Read "+formatTokens(totals.cacheRead))
		})
		if totals.costedSessions > 0 && !math.IsNaN(totals.cost) && !math.IsInf(totals.cost, 0) {
			usageNotice(c, "tag", "API estimate "+formatUsageCost(totals.cost),
				fmt.Sprintf("%d/%d sessions priced · Not a subscription charge", totals.costedSessions, totals.sessions), palette.textSecondary)
		}
	})
}

func usageMetric(c *ui.Context, key, iconName, title, value, detail string) {
	c.Key("usage-metric-" + key)
	usagePanel(c, iconName, title, "", func() {
		kit.Numeric(c, value).FontFeatures("tnum").FontSize(24).FontWeight(500).TextColor(palette.text).SingleLine()
		ui.Text(c, detail).FontSize(11).TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
	}).Grow(1).Basis(185).MinWidth(0)
}

func (a *app) usageRankings(c *ui.Context, view *usageViewData) {
	ui.Row(c).FillWidth().Wrap().Gap(14).AlignItems(ui.Start).Children(func() {
		usageRanking(c, "models", "package", "Models", view.Models)
		usageRanking(c, "providers", "globe", "Providers", view.Providers)
		usageRanking(c, "projects", "folder", "Projects", view.Projects)
	})
}

func usageRanking(c *ui.Context, key, iconName, title string, rows []usageRank) {
	c.Key("usage-rank-" + key)
	usagePanel(c, iconName, title, fmt.Sprintf("%d", len(rows)), func() {
		if len(rows) == 0 {
			ui.Text(c, tr("No recorded usage")).FontSize(12).TextColor(palette.textTertiary)
			return
		}
		maximum := rows[0].Tokens
		for _, row := range rows[:min(8, len(rows))] {
			maximum = max(maximum, row.Tokens)
		}
		for _, row := range rows[:min(8, len(rows))] {
			c.Key("usage-rank-row-" + row.ID)
			countLabel := choice(row.Sessions == 1, "session", "sessions")
			if key != "projects" {
				countLabel = "primary " + countLabel
			}
			detail := fmt.Sprintf("%s · %d tokens · %d %s", row.Label, row.Tokens, row.Sessions, countLabel)
			item := ui.Column(c).FillWidth().Gap(6).Focusable().Label(row.Label).Description(detail).Tooltip(detail)
			item.Children(func() {
				ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
					ui.Text(c, row.Label).Grow(1).MinWidth(0).FontSize(11.5).TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
					kit.Numeric(c, formatTokens(row.Tokens)).FontFeatures("tnum").FontSize(11).TextColor(palette.text).Shrink(0)
				})
				fraction := float32(0)
				if maximum > 0 {
					fraction = float32(float64(row.Tokens) / float64(maximum))
				}
				usageProgress(c, fraction, palette.accent.Alpha(0.65))
				if item.Focused() {
					ui.Text(c, detail).FontSize(11).TextColor(palette.textSecondary).Wrap()
				}
			})
		}
		if len(rows) > 8 {
			ui.Text(c, fmt.Sprintf(tr("%d more"), len(rows)-8)).FontSize(11).TextColor(palette.textTertiary)
		}
	}).Grow(1).Basis(260).MinWidth(0).FocusGroup(ui.Vertical)
}

func usageProgress(c *ui.Context, fraction float32, color ui.Color) {
	ui.Row(c).FillWidth().Height(4).Radius(2).Background(palette.overlayStrong).Clip().Children(func() {
		if fraction > 0 {
			ui.Box(c).Height(4).WidthPercent(min(1, max(0, fraction)) * 100).Radius(2).Background(color)
		}
	})
}

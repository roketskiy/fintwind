package main

import (
	"fmt"
	"math"
	"strings"
	"time"

	"github.com/ELY-Open-Source/CrychicUI/kit"
	"github.com/egoist/mygo/ui"
)

func (a *app) usageSubscriptionsSection(c *ui.Context) {
	s := &a.usage.Subscriptions
	ui.Column(c).FillWidth().MinWidth(0).Gap(12).Children(func() {
		ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
			ui.Text(c, tr("Subscriptions & balances")).Grow(1).MinWidth(0).FontSize(uiPx(13)).FontWeight(500).TextColor(palette.text)
			usageRefreshControl(c, "usage-subscriptions-refresh", tr("Refresh quotas"), s.Status == "loading", func() {
				a.refreshUsageSubscriptions(true)
			})
		})
		if s.Status == "error" {
			title := tr("Connections unavailable")
			if len(s.Cards) > 0 {
				title = tr("Quota refresh incomplete")
			}
			usageNotice(c, "alert", title, s.Error, palette.warningText)
		}
		// No updating row: same flash-and-shove as the history one.
		if len(s.Cards) == 0 {
			usagePanel(c, "gauge", tr("Account quotas"), "", func() {
				switch s.Status {
				case "loading":
					ui.Text(c, tr("Loading connected accounts...")).FontSize(uiPx(12)).TextColor(palette.textTertiary)
				case "error":
					ui.Text(c, tr("Quota data unavailable")).FontSize(uiPx(12)).TextColor(palette.textTertiary)
				case "idle":
					ui.Text(c, tr("No workspace connected")).FontSize(uiPx(12)).TextColor(palette.textTertiary)
				default:
					ui.Text(c, tr("Not connected")).FontSize(uiPx(12)).TextColor(palette.textTertiary)
				}
			})
			return
		}
		var subscriptions, balances []usageSubscription
		for _, card := range s.Cards {
			if card.Quota != nil && len(card.Quota.Windows) == 0 && len(card.Quota.Balances) > 0 {
				balances = append(balances, card)
			} else {
				subscriptions = append(subscriptions, card)
			}
		}
		ui.Column(c).FillWidth().Gap(14).Children(func() {
			usageSubscriptionGrid(c, subscriptions)
			usageSubscriptionGrid(c, balances)
		})
	})
}

func usageSubscriptionGrid(c *ui.Context, cards []usageSubscription) {
	if len(cards) == 0 {
		return
	}
	width, _ := c.Size()
	available := min(float32(1280), width-modeRailWidth-48)
	columns := min(len(cards), max(1, int((available+14)/(370+14))))
	rows := (len(cards) + columns - 1) / columns
	tracks := columns
	if columns == 3 {
		tracks = 6
	}
	rowTracks := make([]ui.Track, rows)
	for index := range rowTracks {
		rowTracks[index] = ui.Fr(1)
	}
	ui.Grid(c).FillWidth().Columns(tracks).RowTracks(rowTracks...).Gap(14).Children(func() {
		for index, card := range cards {
			c.Key("usage-subscription-" + card.ID)
			span := tracks / columns
			if remaining := len(cards) % columns; remaining > 0 && index >= len(cards)-remaining {
				span = tracks / remaining
			}
			usageSubscriptionCard(c, card).ColumnSpan(span)
		}
	})
}

func usageSubscriptionCard(c *ui.Context, subscription usageSubscription) ui.Element {
	quota := subscription.Quota
	meta := ""
	if quota != nil {
		meta = usageOptionalText(quota.Plan)
	}
	return usagePanelBody(c, "gauge", subscription.Name, meta, true, func() {
		if quota != nil {
			identity := usageOptionalText(quota.Service)
			if account := usageOptionalText(quota.Account); account != "" {
				if identity != "" {
					identity += " · "
				}
				identity += account
			}
			if identity != "" {
				ui.Text(c, identity).FillWidth().MinWidth(0).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).Wrap()
			}
		}
		// Stale quotas stay put through a refresh; no flashing row.
		if subscription.Status == "error" && (quota == nil || quota.Status == "ready") {
			usageNotice(c, "alert", tr("Quota unavailable"), subscription.Error, palette.warningText)
		}
		if quota == nil {
			if subscription.Status != "loading" && subscription.Status != "error" {
				ui.Text(c, tr("No quota reported")).FontSize(uiPx(12)).TextColor(palette.textTertiary)
			}
			return
		}
		status := usageQuotaStatus(quota.Status)
		if status != "ready" {
			usageNotice(c, "alert", usageQuotaStatusLabel(status), "", palette.warningText)
		}
		showReading := status == "ready" || status == "unavailable" || status == "rate_limited"
		if showReading {
			for i, window := range quota.Windows {
				c.Key(fmt.Sprintf("quota-window-%d-%s", i, window.ID))
				usageQuotaWindowCard(c, window)
			}
			for i, balance := range quota.Balances {
				c.Key(fmt.Sprintf("quota-balance-%d", i))
				usageQuotaBalanceRow(c, balance)
			}
			if status == "ready" && len(quota.Windows) == 0 && len(quota.Balances) == 0 {
				ui.Text(c, tr("No quota reported")).FontSize(uiPx(12)).TextColor(palette.textTertiary)
			}
		}
		if quota.MeasuredAt != nil {
			measured := time.Unix(*quota.MeasuredAt, 0).Local().Format("Jan 2, 15:04")
			prefix := trf("Last measured %s", measured)
			if status != "ready" || subscription.Status == "error" || subscription.Expired {
				prefix = trf("Previous reading %s", measured)
			}
			ui.Text(c, prefix).
				FontSize(uiPx(10.5)).TextColor(palette.textTertiary)
		}
		if quota.RetryAt != nil && status != "ready" {
			ui.Text(c, trf("Retry after %s", time.Unix(*quota.RetryAt, 0).Local().Format("Jan 2, 15:04"))).
				FontSize(uiPx(10.5)).TextColor(palette.textTertiary)
		}
	})
}

func usageQuotaWindowCard(c *ui.Context, window usageQuotaWindow) {
	ui.Column(c).FillWidth().Gap(8).Children(func() {
		value := "—"
		percentKnown := window.UsedPercent != nil && !math.IsNaN(*window.UsedPercent) && !math.IsInf(*window.UsedPercent, 0)
		if window.Unlimited {
			value = tr("Unlimited")
		} else if percentKnown {
			value = trf("%s used", usageQuotaPercent(*window.UsedPercent))
		}
		ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
			ui.Text(c, usageQuotaWindowLabel(window)).Grow(1).MinWidth(0).FontSize(uiPx(12)).TextColor(palette.textSecondary).Wrap()
			kit.Numeric(c, value).FontFeatures("tnum").FontSize(uiPx(12)).TextColor(palette.text).Shrink(0)
		})
		if percentKnown && !window.Unlimited {
			percent := *window.UsedPercent
			color := palette.accent
			if percent >= 90 {
				color = palette.warning
			}
			usageProgress(c, float32(max(0, min(100, percent))/100), color)
		}
		remaining, limit := usageOptionalText(window.Remaining), usageOptionalText(window.Limit)
		if remaining != "" || limit != "" {
			text := ""
			if remaining != "" {
				text = remaining + tr(" remaining")
			}
			if limit != "" {
				if text != "" {
					text += tr(" · ")
				}
				text += tr("Limit ") + limit
			}
			ui.Text(c, text).FontSize(uiPx(11)).TextColor(palette.textTertiary).Wrap()
		}
		if window.ResetsAt != nil {
			ui.Text(c, tr("Resets ")+time.Unix(*window.ResetsAt, 0).Local().Format("Jan 2, 15:04")).
				FontSize(uiPx(10.5)).TextColor(palette.textTertiary)
		}
	})
}

func usageQuotaBalanceRow(c *ui.Context, balance usageQuotaBalance) {
	label := tr("Balance remaining")
	if unit := strings.TrimSpace(balance.Unit); unit != "" {
		label += tr(" · ") + unit
	}
	value := strings.TrimSpace(balance.Remaining)
	if balance.Unlimited {
		value = tr("Unlimited")
	} else if value == "" {
		value = "—"
	}
	ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
		ui.Text(c, label).Grow(1).MinWidth(0).FontSize(uiPx(12)).TextColor(palette.textSecondary).Wrap()
		kit.Numeric(c, value).FontFeatures("tnum").FontSize(uiPx(14)).TextColor(palette.text).Shrink(0)
	})
}

func usageOptionalText(value *string) string {
	if value == nil {
		return ""
	}
	return strings.TrimSpace(*value)
}

func usageQuotaPercent(value float64) string {
	if value > 9999 {
		return ">9999%"
	}
	if value < -9999 {
		return "<-9999%"
	}
	return fmt.Sprintf("%.1f%%", value)
}

func usageQuotaStatus(status string) string {
	switch status {
	case "noCredential":
		return "no_credential"
	case "needsAuth":
		return "needs_auth"
	case "accessDenied":
		return "access_denied"
	case "rateLimited":
		return "rate_limited"
	case "noData":
		return "no_data"
	default:
		return status
	}
}

func usageQuotaStatusLabel(status string) string {
	switch status {
	case "no_credential":
		return tr("Not signed in")
	case "needs_auth":
		return tr("Sign in again")
	case "access_denied":
		return tr("Access denied")
	case "rate_limited":
		return tr("Rate limited")
	case "unavailable":
		return tr("Service unavailable")
	case "no_data":
		return tr("No quota reported")
	case "unsupported":
		return tr("Quota not supported")
	default:
		return tr("Quota unavailable")
	}
}

func usageQuotaWindowLabel(window usageQuotaWindow) string {
	labels := map[string]string{
		"rolling": tr("Rolling"), "weekly": tr("Weekly"), "monthly": tr("Monthly"), "tool_calls": tr("Tool calls"),
		"chat": tr("Chat"), "completions": tr("Completions"), "premium": tr("Premium"), "model": tr("Model"), "extra_usage": tr("Extra usage"),
	}
	label := labels[window.Kind]
	if label == "" {
		label = tr("Quota")
	}
	if window.Kind == "rolling" && window.SpanSeconds != nil {
		seconds := *window.SpanSeconds
		if seconds > 0 && seconds%3600 == 0 {
			label += trf(" · %dh", seconds/3600)
		} else if seconds > 0 && seconds%60 == 0 {
			label += trf(" · %dm", seconds/60)
		} else if seconds > 0 {
			label += trf(" · %ds", seconds)
		}
	}
	if model := usageOptionalText(window.Model); model != "" {
		label += " · " + model
	}
	return label
}

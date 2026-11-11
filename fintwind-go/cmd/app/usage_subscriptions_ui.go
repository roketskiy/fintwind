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
			ui.Text(c, tr("Subscriptions & balances")).Grow(1).MinWidth(0).FontSize(13).FontWeight(500).TextColor(palette.text)
			usageRefreshControl(c, "usage-subscriptions-refresh", "Refresh quotas", s.Status == "loading", func() {
				a.refreshUsageSubscriptions(true)
			})
		})
		if s.Status == "error" {
			title := tr("Connections unavailable")
			if len(s.Cards) > 0 {
				title = tr("Quota refresh incomplete")
			}
			usageNotice(c, "alert", title, s.Error, palette.warningText)
		} else if s.Status == "loading" {
			usageNotice(c, "rotate-cw", "Updating quotas", usageUpdatedLabel(s.UpdatedAt), palette.textTertiary)
		}
		if len(s.Cards) == 0 {
			usagePanel(c, "gauge", "Account quotas", "", func() {
				switch s.Status {
				case "loading":
					ui.Text(c, tr("Loading connected accounts...")).FontSize(12).TextColor(palette.textTertiary)
				case "error":
					ui.Text(c, tr("Quota data unavailable")).FontSize(12).TextColor(palette.textTertiary)
				case "idle":
					ui.Text(c, tr("No workspace connected")).FontSize(12).TextColor(palette.textTertiary)
				default:
					ui.Text(c, tr("Not connected")).FontSize(12).TextColor(palette.textTertiary)
				}
			})
			return
		}
		ui.Row(c).FillWidth().Wrap().Gap(14).AlignItems(ui.Start).Children(func() {
			for _, subscription := range s.Cards {
				c.Key("usage-subscription-" + subscription.ID)
				usageSubscriptionCard(c, subscription)
			}
		})
	})
}

func usageSubscriptionCard(c *ui.Context, subscription usageSubscription) {
	quota := subscription.Quota
	meta := ""
	if quota != nil {
		meta = usageOptionalText(quota.Plan)
	}
	usagePanel(c, "gauge", subscription.Name, meta, func() {
		if quota != nil {
			identity := usageOptionalText(quota.Service)
			if account := usageOptionalText(quota.Account); account != "" {
				if identity != "" {
					identity += " · "
				}
				identity += account
			}
			if identity != "" {
				ui.Text(c, identity).FillWidth().MinWidth(0).FontSize(11.5).TextColor(palette.textSecondary).Wrap()
			}
		}
		if subscription.Status == "loading" {
			usageNotice(c, "rotate-cw", "Refreshing...", "", palette.textTertiary)
		} else if subscription.Status == "error" && (quota == nil || quota.Status == "ready") {
			usageNotice(c, "alert", "Quota unavailable", subscription.Error, palette.warningText)
		}
		if quota == nil {
			if subscription.Status != "loading" && subscription.Status != "error" {
				ui.Text(c, tr("No quota reported")).FontSize(12).TextColor(palette.textTertiary)
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
				ui.Text(c, tr("No quota reported")).FontSize(12).TextColor(palette.textTertiary)
			}
		}
		if quota.MeasuredAt != nil {
			prefix := "Last measured "
			if status != "ready" || subscription.Status == "error" || subscription.Expired {
				prefix = "Previous reading "
			}
			ui.Text(c, prefix+time.Unix(*quota.MeasuredAt, 0).Local().Format("Jan 2, 15:04")).
				FontSize(10.5).TextColor(palette.textTertiary)
		}
		if quota.RetryAt != nil && status != "ready" {
			ui.Text(c, tr("Retry after ")+time.Unix(*quota.RetryAt, 0).Local().Format("Jan 2, 15:04")).
				FontSize(10.5).TextColor(palette.textTertiary)
		}
	}).Grow(1).Basis(370).MinWidth(0)
}

func usageQuotaWindowCard(c *ui.Context, window usageQuotaWindow) {
	ui.Column(c).FillWidth().Gap(8).Children(func() {
		value := "—"
		percentKnown := window.UsedPercent != nil && !math.IsNaN(*window.UsedPercent) && !math.IsInf(*window.UsedPercent, 0)
		if window.Unlimited {
			value = "Unlimited"
		} else if percentKnown {
			value = usageQuotaPercent(*window.UsedPercent) + " used"
		}
		ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
			ui.Text(c, usageQuotaWindowLabel(window)).Grow(1).MinWidth(0).FontSize(12).TextColor(palette.textSecondary).Wrap()
			kit.Numeric(c, value).FontFeatures("tnum").FontSize(12).TextColor(palette.text).Shrink(0)
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
			ui.Text(c, text).FontSize(11).TextColor(palette.textTertiary).Wrap()
		}
		if window.ResetsAt != nil {
			ui.Text(c, tr("Resets ")+time.Unix(*window.ResetsAt, 0).Local().Format("Jan 2, 15:04")).
				FontSize(10.5).TextColor(palette.textTertiary)
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
		value = "Unlimited"
	} else if value == "" {
		value = "—"
	}
	ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
		ui.Text(c, label).Grow(1).MinWidth(0).FontSize(12).TextColor(palette.textSecondary).Wrap()
		kit.Numeric(c, value).FontFeatures("tnum").FontSize(14).TextColor(palette.text).Shrink(0)
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
		return "Not signed in"
	case "needs_auth":
		return "Sign in again"
	case "access_denied":
		return "Access denied"
	case "rate_limited":
		return "Rate limited"
	case "unavailable":
		return "Service unavailable"
	case "no_data":
		return "No quota reported"
	case "unsupported":
		return "Quota not supported"
	default:
		return "Quota unavailable"
	}
}

func usageQuotaWindowLabel(window usageQuotaWindow) string {
	labels := map[string]string{
		"rolling": "Rolling", "weekly": "Weekly", "monthly": "Monthly", "tool_calls": "Tool calls",
		"chat": "Chat", "completions": "Completions", "premium": "Premium", "model": "Model", "extra_usage": "Extra usage",
	}
	label := labels[window.Kind]
	if label == "" {
		label = "Quota"
	}
	if window.Kind == "rolling" && window.SpanSeconds != nil {
		seconds := *window.SpanSeconds
		if seconds > 0 && seconds%3600 == 0 {
			label += fmt.Sprintf(" · %dh", seconds/3600)
		} else if seconds > 0 && seconds%60 == 0 {
			label += fmt.Sprintf(" · %dm", seconds/60)
		} else if seconds > 0 {
			label += fmt.Sprintf(" · %ds", seconds)
		}
	}
	if model := usageOptionalText(window.Model); model != "" {
		label += " · " + model
	}
	return label
}

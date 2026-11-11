package main

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"fintwind-go/internal/daemon"
)

func usageRoot(a *app) string {
	if project := a.selectedProject(); project != nil {
		return project.Path
	}
	return ""
}

func usageClientOnline(client *daemon.Client) bool {
	if client == nil {
		return false
	}
	select {
	case <-client.Done():
		return false
	default:
		return true
	}
}

// Route changes invalidate both streams, including the transition to no route.
func (a *app) syncUsageRoute() bool {
	root, client := usageRoot(a), a.client
	var identity daemon.Identity
	if client != nil {
		identity = client.Identity()
	}
	online := usageClientOnline(client)
	if !online {
		client = nil
	}
	u := &a.usage
	if u.Range == "" {
		u.Range = "30d"
	}
	if u.root != root || u.client != client || u.identity != identity {
		if u.History.cancel != nil {
			u.History.cancel()
		}
		if u.Subscriptions.cancel != nil {
			u.Subscriptions.cancel()
		}
		historyGen, subscriptionsGen := u.History.generation+1, u.Subscriptions.generation+1
		u.History = usageHistory{Status: usageIdle, generation: historyGen}
		u.Subscriptions = usageSubscriptions{Status: usageIdle, generation: subscriptionsGen}
		u.SelectedDay = ""
		u.root, u.client, u.identity = root, client, identity
	}
	if root == "" {
		u.History.Status, u.History.Error = usageIdle, "No workspace selected"
		u.Subscriptions.Status, u.Subscriptions.Error = usageIdle, "No workspace selected"
		return false
	}
	if !online {
		u.History.Status, u.History.Error = usageError, "Daemon is disconnected"
		u.Subscriptions.Status, u.Subscriptions.Error = usageError, "Daemon is disconnected"
		return false
	}
	return a.lifetime != nil && a.lifetime.Err() == nil
}

func (a *app) usageRouteMatches(root string, client *daemon.Client, identity daemon.Identity) bool {
	return a.lifetime != nil && a.lifetime.Err() == nil && a.client == client &&
		usageClientOnline(client) && client.Identity() == identity && usageRoot(a) == root &&
		a.usage.root == root && a.usage.client == client && a.usage.identity == identity
}

func (a *app) ensureUsageFresh() {
	if !a.syncUsageRoute() {
		return
	}
	now := time.Now()
	h := &a.usage.History
	if h.Status != usageLoading && !h.building && (h.attemptedAt.IsZero() || now.Sub(h.attemptedAt) >= usageStaleAfter) {
		a.refreshUsageHistory(false)
	} else if h.Stats != nil && h.Status != usageLoading && !h.building && h.aggregateDay != usageDate(now).Format("2006-01-02") {
		a.rebuildUsageCalendar(now)
	}
	s := &a.usage.Subscriptions
	if s.Status != usageLoading && (s.attemptedAt.IsZero() || now.Sub(s.attemptedAt) >= usageStaleAfter) {
		a.refreshUsageSubscriptions(false)
	}
}

func (a *app) refreshUsageHistory(force bool) {
	if !a.syncUsageRoute() {
		return
	}
	h := &a.usage.History
	if h.Status == usageLoading {
		return
	}
	if !force && !h.attemptedAt.IsZero() && time.Since(h.attemptedAt) < usageStaleAfter {
		return
	}
	if h.cancel != nil {
		h.cancel()
	}
	ctx, cancel := context.WithTimeout(a.lifetime, usageFetchTimeout)
	h.cancel = cancel
	h.generation++
	gen := h.generation
	h.Status, h.Error, h.Refreshing, h.building = usageLoading, "", true, false
	h.Expired = h.Stats != nil
	h.attemptedAt = time.Now()
	root, client, identity := a.usage.root, a.usage.client, a.usage.identity
	go func() {
		defer cancel()
		binary := configuredOpencodeBinary(ctx, client)
		var pollDeadline time.Time
		for {
			requestCtx := ctx
			requestCancel := func() {}
			if !pollDeadline.IsZero() {
				requestCtx, requestCancel = context.WithDeadline(ctx, pollDeadline)
			}
			payload, err := client.Request(requestCtx, map[string]any{
				"type": "fetchUsageStats", "binary": binary, "directory": root, "forceRefresh": force,
			})
			requestCancel()
			force = false
			if err != nil {
				a.finishUsageHistoryError(root, client, identity, gen, "Could not load usage")
				return
			}
			stats := usageParseStats(payload)
			if stats == nil {
				a.finishUsageHistoryError(root, client, identity, gen, "Usage data is unavailable")
				return
			}
			if stats.Refreshing && pollDeadline.IsZero() {
				pollDeadline = time.Now().Add(usagePollTimeout)
			}
			if stats.Refreshing && len(stats.Entries) == 0 && stats.RefreshError != nil {
				a.post(func() {
					if a.usageRouteMatches(root, client, identity) && a.usage.History.generation == gen {
						a.usage.History.Error = "Could not update usage"
					}
				})
			}
			if !stats.Refreshing || len(stats.Entries) > 0 {
				now := time.Now()
				aggregates := usageBuildAggregates(stats, now)
				a.post(func() {
					if !a.usageRouteMatches(root, client, identity) || a.usage.History.generation != gen {
						return
					}
					h := &a.usage.History
					// A failed empty scan must not replace a previously successful snapshot.
					if stats.RefreshError == nil || len(stats.Entries) > 0 {
						h.Stats, h.Views, h.Days, h.Heat = stats, aggregates.Views, aggregates.Days, aggregates.Heat
						h.aggregateDay = usageDate(now).Format("2006-01-02")
					}
					h.Refreshing, h.Expired = stats.Refreshing, stats.Refreshing || stats.RefreshError != nil
					if stats.RefreshError != nil {
						h.Error = "Could not update usage"
					} else {
						h.Error = ""
						h.UpdatedAt = now
					}
					if !stats.Refreshing {
						h.Status = usageReady
						if stats.RefreshError != nil {
							h.Status = usageError
						}
						h.attemptedAt = time.Now()
						h.cancel = nil
					}
				})
			}
			if !stats.Refreshing {
				return
			}
			if !time.Now().Add(time.Second).Before(pollDeadline) {
				a.finishUsageHistoryError(root, client, identity, gen, "Usage is still updating. Try again")
				return
			}
			timer := time.NewTimer(time.Second)
			select {
			case <-ctx.Done():
				timer.Stop()
				a.finishUsageHistoryError(root, client, identity, gen, "Could not load usage")
				return
			case <-timer.C:
			}
		}
	}()
}

func (a *app) finishUsageHistoryError(root string, client *daemon.Client, identity daemon.Identity, gen uint64, message string) {
	a.post(func() {
		if !a.usageRouteMatches(root, client, identity) || a.usage.History.generation != gen {
			return
		}
		h := &a.usage.History
		h.Status, h.Error, h.Refreshing = usageError, message, false
		h.Expired = h.Stats != nil
		h.attemptedAt, h.cancel = time.Now(), nil
	})
}

func (a *app) rebuildUsageCalendar(now time.Time) {
	h := &a.usage.History
	h.building = true
	h.generation++
	gen, stats := h.generation, h.Stats
	root, client, identity := a.usage.root, a.usage.client, a.usage.identity
	go func() {
		aggregates := usageBuildAggregates(stats, now)
		a.post(func() {
			if !a.usageRouteMatches(root, client, identity) || a.usage.History.generation != gen {
				return
			}
			h := &a.usage.History
			h.Views, h.Days, h.Heat = aggregates.Views, aggregates.Days, aggregates.Heat
			h.aggregateDay, h.building = usageDate(now).Format("2006-01-02"), false
		})
	}()
}

func usageParseStats(payload json.RawMessage) *usageStats {
	var body struct {
		Type  string      `json:"type"`
		Stats *usageStats `json:"stats"`
	}
	if json.Unmarshal(payload, &body) != nil || body.Type != "usageStats" || body.Stats == nil || body.Stats.Entries == nil {
		return nil
	}
	if body.Stats.RefreshError != nil {
		message := "Could not update usage"
		body.Stats.RefreshError = &message
	}
	return body.Stats
}

func (a *app) setUsageRange(value string) {
	switch value {
	case "7d", "30d", "90d", "all":
		a.usage.Range, a.usage.SelectedDay = value, ""
	}
}

func (a *app) selectUsageDay(value string) {
	if value == "" {
		a.usage.SelectedDay = ""
		return
	}
	if _, ok := a.usage.History.Days[value]; ok {
		a.usage.SelectedDay = value
	}
}

func (a *app) currentUsageView() *usageViewData {
	if a.usage.SelectedDay != "" {
		return a.usage.History.Days[a.usage.SelectedDay]
	}
	key := a.usage.Range
	if key == "" {
		key = "30d"
	}
	return a.usage.History.Views[key]
}

func usageCacheHit(t usageTotals) (float64, bool) {
	denominator := float64(t.input) + float64(t.cacheRead) + float64(t.cacheWrite)
	if denominator == 0 {
		return 0, false
	}
	return float64(t.cacheRead) * 100 / denominator, true
}

func formatUsageCost(cost float64) string {
	return fmt.Sprintf("$%.2f", usageSafeCost(cost))
}

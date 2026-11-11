//go:build usagee2e

package main

import (
	"context"
	"encoding/json"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

func usageE2ECheckEdgeCases(ctx context.Context, report *usageE2EReport, client *daemon.Client, now time.Time) {
	zero := 0.0
	for _, sample := range []struct {
		name string
		days []usageDayShare
		cost *float64
	}{
		{"unindexed zero session", nil, nil},
		{"empty split zero session", []usageDayShare{}, &zero},
		{"explicit zero day", []usageDayShare{{Timestamp: uint64(now.Unix())}}, nil},
	} {
		stats := usageE2EDecodeEntries(report, sample.name, []usageEntry{{Timestamp: uint64(now.Unix()), Days: sample.days, Cost: sample.cost}})
		if stats == nil {
			continue
		}
		a := usageE2EInstallSnapshot(ctx, client, stats, now)
		counts := make(map[string]uint64)
		for _, key := range []string{"7d", "30d", "90d", "all"} {
			counts[key] = a.usage.History.Views[key].Totals.sessions
		}
		counts["day"] = a.usage.History.Days[now.Format("2006-01-02")].Totals.sessions
		report.check(sample.name+" keeps sessions in every date scope", map[string]uint64{"7d": 1, "30d": 1, "90d": 1, "all": 1, "day": 1}, counts)
		last := a.usage.History.Heat[len(a.usage.History.Heat)-1]
		report.check(sample.name+" keeps zero heat intensity with a session", map[string]any{"tokens": 0, "direct": 0, "sessions": 1}, map[string]any{"tokens": last.Total, "direct": last.Direct, "sessions": last.Sessions})
		tt := ui.NewTesterWith(a.usageView, 932, 2000, ui.TesterOptions{Preferences: ui.Preferences{ReduceMotion: true}, Dark: true})
		report.check(sample.name+" hides price estimate notice", sample.cost != nil, !tt.HasText("API estimate"))
		date := now.Format("2006-01-02")
		if err := tt.Click(date); err != nil {
			report.check(sample.name+" selects zero-usage day", "no error", err.Error())
		}
		report.check(sample.name+" day UI preserves its session without inventing fees", true, a.usage.SelectedDay == date && a.currentUsageView().Totals.sessions == 1 && !tt.HasText("API estimate"))
	}
	entries := make([]usageEntry, 1000)
	for i := range entries {
		entries[i] = usageEntry{Timestamp: uint64(now.Unix()), Cost: &zero}
	}
	fee := 2.5
	entries[0].Cost, entries[0].InputTokens = &fee, 1
	entries[999].Cost = nil
	stats := usageE2EDecodeEntries(report, "partial price coverage", entries)
	if stats == nil {
		return
	}
	a := usageE2EInstallSnapshot(ctx, client, stats, now)
	tt := ui.NewTesterWith(a.usageView, 932, 2000, ui.TesterOptions{Preferences: ui.Preferences{ReduceMotion: true}, Dark: true})
	report.check("partial price coverage hides fee notice", true, a.currentUsageView().Totals.sessions == 1000 && a.currentUsageView().Totals.costedSessions == 999 && !tt.HasText("sessions priced") && !tt.HasText("API estimate") && !tt.HasText("100% session coverage"))
}

func usageE2EDecodeEntries(report *usageE2EReport, name string, entries []usageEntry) *usageStats {
	payload, err := json.Marshal(map[string]any{"type": "usageStats", "stats": &usageStats{Entries: entries}})
	if err != nil {
		report.check(name+" response serialization", "no error", err.Error())
		return nil
	}
	stats := usageParseStats(payload)
	if stats == nil {
		report.check(name+" production response decode", true, false)
	}
	return stats
}

func usageE2EInstallSnapshot(ctx context.Context, client *daemon.Client, stats *usageStats, now time.Time) *app {
	a := &app{client: client, lifetime: ctx, done: make(chan struct{}), lastProject: "fixture-project", projects: []model.Project{{ID: "fixture-project", Path: "fixture/workspace"}}}
	a.syncUsageRoute()
	aggregates := usageBuildAggregates(stats, now)
	a.usage.History = usageHistory{Status: usageReady, UpdatedAt: now, Stats: stats, Views: aggregates.Views, Days: aggregates.Days, Heat: aggregates.Heat, attemptedAt: now, aggregateDay: now.Format("2006-01-02")}
	a.usage.Subscriptions = usageSubscriptions{Status: usageReady, attemptedAt: now}
	return a
}

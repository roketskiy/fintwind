//go:build usagee2e

package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"time"
	_ "time/tzdata"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"github.com/ELY-Open-Source/CrychicUI/sound"
	"github.com/egoist/mygo/ui"
)

func usageE2ERun(ctx context.Context, report *usageE2EReport) {
	report.check("headless component sounds are muted", false, sound.Enabled())
	round := os.Getenv("FINTWIND_USAGE_E2E_DIR")
	report.check("owned supervisor isolates profile directories", map[string]string{"HOME": filepath.Join(round, "home"), "USERPROFILE": filepath.Join(round, "home"), "APPDATA": filepath.Join(round, "home"), "LOCALAPPDATA": filepath.Join(round, "home")}, map[string]string{"HOME": os.Getenv("HOME"), "USERPROFILE": os.Getenv("USERPROFILE"), "APPDATA": os.Getenv("APPDATA"), "LOCALAPPDATA": os.Getenv("LOCALAPPDATA")})
	now := time.Now().Local()
	fixture, err := usageE2ENewFixture(ctx, now)
	if err != nil {
		report.check("start isolated loopback fixture", "no error", err.Error())
		return
	}
	defer fixture.close()
	defer fixture.stop()
	dialCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	client, err := daemon.Dial(dialCtx, fixture.server.Listener.Addr().String(), "")
	cancel()
	if err != nil {
		report.check("protocol 42 handshake", "no error", err.Error())
		return
	}
	defer client.Close()
	report.check("protocol 42 handshake identity", "10000000-0000-4000-8000-000000000001", client.Identity().EnvironmentID)
	requestCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	payload, err := client.Request(requestCtx, map[string]any{"type": "fetchUsageStats", "binary": "fixture-opencode", "directory": "fixture/workspace", "forceRefresh": false})
	cancel()
	if err != nil {
		report.check("fetchUsageStats through real daemon.Client", "no error", err.Error())
		return
	}
	stats := usageParseStats(payload)
	if stats == nil {
		report.check("production usage decode", "four sessions", "decode rejected payload")
		return
	}
	report.check("production usage decode", 4, len(stats.Entries))
	aggregates := usageBuildAggregates(stats, now)
	usageE2ECheckAggregates(report, aggregates, now)
	cards := usageE2EFetchCards(ctx, client, report, fixture, now)
	for _, width := range []int{932, 980, 1440} {
		if ctx.Err() != nil {
			report.check("UI deadline", "active", ctx.Err().Error())
			return
		}
		a := &app{client: client, lifetime: ctx, done: make(chan struct{}), lastProject: "fixture-project", projects: []model.Project{{ID: "fixture-project", Path: "fixture/workspace"}}}
		a.syncUsageRoute()
		a.usage.Range = "30d"
		a.usage.History = usageHistory{Status: usageReady, UpdatedAt: now, Stats: stats, Views: aggregates.Views, Days: aggregates.Days, Heat: aggregates.Heat, attemptedAt: now, aggregateDay: now.Format("2006-01-02")}
		a.usage.Subscriptions = usageSubscriptions{Status: usageReady, UpdatedAt: now, Cards: cards, attemptedAt: now}
		usageE2ECheckUI(report, a, width, now, fixture)
	}
	usageE2ECheckCalendarPipeline(ctx, report, client)
	usageE2ECheckEdgeCases(ctx, report, client, now)
	requests, fixtureErrors := fixture.snapshot()
	report.check("fixture command allowlist", []string{}, fixtureErrors)
	names := usageE2ERequestNames(requests)
	sort.Strings(names)
	report.check("literal IPC requests including production subscription eligibility", []string{"fetchIntegrations", "fetchProviderQuota:openai", "fetchProviderQuota:openai", "fetchProviderQuota:opencode-go", "fetchProviderQuota:opencode-go", "fetchUsageStats", "getSettings"}, names)
}

func usageE2EFetchCards(ctx context.Context, client *daemon.Client, report *usageE2EReport, fixture *usageE2EFixture, now time.Time) []usageSubscription {
	requestCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	a := &app{client: client, lifetime: requestCtx, done: make(chan struct{}), lastProject: "fixture-project", projects: []model.Project{{ID: "fixture-project", Path: "fixture/workspace"}}}
	a.refreshUsageSubscriptions(false)
	completed := make([]string, 0, 4)
	quotaResponses := 0
	for quotaResponses < 2 {
		select {
		case name := <-fixture.completed:
			if name == "fetchUsageStats" {
				continue
			}
			completed = append(completed, name)
			if name == "fetchProviderQuota:opencode-go" || name == "fetchProviderQuota:openai" {
				quotaResponses++
			}
		case <-requestCtx.Done():
			report.check("production subscription request deadline", "two supported authorized quotas", requestCtx.Err().Error())
			return nil
		}
	}
	sort.Strings(completed)
	report.check("production subscription excludes needsAuth and unsupported IDs", []string{"fetchIntegrations", "fetchProviderQuota:openai", "fetchProviderQuota:opencode-go", "getSettings"}, completed)
	var cards []usageSubscription
	for _, item := range []struct{ id, name string }{{"opencode-go", "OpenCode Go"}, {"openai", "OpenAI"}} {
		payload, err := client.Request(requestCtx, map[string]any{"type": "fetchProviderQuota", "binary": "fixture-opencode", "directory": "fixture/workspace", "providerId": item.id, "force": false})
		if err != nil {
			report.check("quota IPC "+item.id, "no error", err.Error())
			continue
		}
		quota := usageParseQuota(payload)
		if quota == nil {
			report.check("production quota decode "+item.id, "available", "rejected")
			continue
		}
		status := usageReady
		message := usageQuotaError(quota.Status)
		if message != "" {
			status = usageError
		}
		cards = append(cards, usageSubscription{ID: item.id, Name: item.name, Status: status, Quota: quota, Error: message, Expired: quota.Status != "ready", UpdatedAt: now})
		if item.id == "opencode-go" {
			report.check("quota exact percent and unlimited wire model", map[string]any{"usedPercent": 37.5, "unlimited": true, "windowCount": 2}, map[string]any{"usedPercent": quota.Windows[0].UsedPercent, "unlimited": quota.Windows[1].Unlimited, "windowCount": len(quota.Windows)})
		} else {
			report.check("unavailable quota retains previous measured windows", map[string]any{"status": "unavailable", "windowCount": 1, "measuredAt": now.Add(-48 * time.Hour).Unix()}, map[string]any{"status": quota.Status, "windowCount": len(quota.Windows), "measuredAt": quota.MeasuredAt})
		}
	}
	cards = append(cards, usageSubscription{ID: "deepseek", Name: "DeepSeek", Status: usageError, Quota: &usageQuota{Status: "needs_auth"}, Error: "Authorization required"})
	return cards
}

func usageE2ERequestNames(requests []map[string]any) []string {
	names := make([]string, 0, len(requests))
	for _, command := range requests {
		name, _ := command["type"].(string)
		if id, ok := command["providerId"].(string); ok {
			name += ":" + id
		}
		names = append(names, name)
	}
	return names
}

func usageE2ETotals(view *usageViewData) map[string]any {
	if view == nil {
		return nil
	}
	t := view.Totals
	return map[string]any{"total": t.total(), "direct": t.input + t.generated(), "input": t.input, "generated": t.generated(), "cache": t.cacheRead + t.cacheWrite, "sessions": t.sessions, "cost": t.cost, "knownCostSessions": t.costedSessions}
}

func usageE2ERankTokens(rows []usageRank) map[string]uint64 {
	values := make(map[string]uint64)
	for _, row := range rows {
		values[row.ID] = row.Tokens
	}
	return values
}

func usageE2ECheckAggregates(report *usageE2EReport, aggregates usageAggregates, now time.Time) {
	for _, entry := range []struct {
		key              string
		tokens, sessions uint64
		known            int
	}{{"7d", 460, 3, 1}, {"30d", 480, 3, 1}, {"90d", 496, 4, 2}, {"all", 496, 4, 2}} {
		view := aggregates.Views[entry.key]
		if view == nil {
			report.check("range "+entry.key, "available", "missing")
			continue
		}
		report.check("range "+entry.key+" literal totals and cost coverage", map[string]any{"tokens": entry.tokens, "sessions": entry.sessions, "knownCostSessions": entry.known, "cost": 2.5}, map[string]any{"tokens": view.Totals.total(), "sessions": view.Totals.sessions, "knownCostSessions": view.Totals.costedSessions, "cost": view.Totals.cost})
	}
	day := now.Format("2006-01-02")
	report.check("today direct and cached tokens, no invented day costs", map[string]any{"total": 460, "direct": 210, "input": 135, "generated": 75, "cache": 250, "sessions": 3, "cost": 0, "knownCostSessions": 0}, usageE2ETotals(aggregates.Days[day]))
	report.check("all models use actual lanes and do not double count child", map[string]uint64{"openai/model-a": 400, "anthropic/model-b": 30, "openai/old": 16, "openai/parent": 30, "openai/child": 20}, usageE2ERankTokens(aggregates.Views["all"].Models))
	report.check("seven day model attribution respects cross-day shares", map[string]uint64{"openai/model-a": 400, "anthropic/model-b": 10, "openai/parent": 30, "openai/child": 20}, usageE2ERankTokens(aggregates.Views["7d"].Models))
	report.check("seven day provider attribution", map[string]uint64{"openai": 450, "anthropic": 10}, usageE2ERankTokens(aggregates.Views["7d"].Providers))
	report.check("all project attribution", map[string]uint64{"fixture/project-a": 466, "fixture/project-b": 30}, usageE2ERankTokens(aggregates.Views["all"].Projects))
	if len(aggregates.Heat) == 0 {
		report.check("heat calendar", "nonempty", "empty")
		return
	}
	last := aggregates.Heat[len(aggregates.Heat)-1]
	report.check("heat calendar stops today and separates cached traffic", map[string]any{"date": day, "direct": 210, "total": 460, "sessions": 3}, map[string]any{"date": last.Date.Format("2006-01-02"), "direct": last.Direct, "total": last.Total, "sessions": last.Sessions})
	future := 0
	for _, cell := range aggregates.Heat {
		if cell.Date.After(usageDate(now)) {
			future++
		}
	}
	report.check("future dates are not filled as zero usage", 0, future)
}

func usageE2ECheckUI(report *usageE2EReport, a *app, width int, now time.Time, fixture *usageE2EFixture) {
	prefix := fmt.Sprintf("%d DIP ", width)
	before, _ := fixture.snapshot()
	tt := ui.NewTesterWith(a.usageView, width, 2000, ui.TesterOptions{Preferences: ui.Preferences{ReduceMotion: true}, Dark: true})
	report.check(prefix+"initial production dashboard metrics", true, tt.HasText("480") && tt.HasText("Total tokens") && tt.HasText("Cache hit"))
	report.check(prefix+"quota readings and stale status", true, tt.HasText("37.5% used") && tt.HasText("Unlimited") && tt.HasText("Service unavailable") && tt.HasText("Previous reading "))
	report.check(prefix+"reset timestamp", true, tt.HasText("Resets "+now.Add(time.Hour).Format("Jan 2, 15:04")))
	trigger, found := tt.Access("Usage date range")
	report.check(prefix+"filter accessibility", true, found && trigger.Focusable && !trigger.Disabled)
	tt.Key(0, ui.KeyTab)
	trigger, found = tt.Access("Usage date range")
	report.check(prefix+"filter receives Tab focus", true, found && trigger.Focused)
	tt.Key(0, ui.KeyEnter)
	tt.Key(0, ui.KeyHome)
	tt.Key(0, ui.KeyEnter)
	report.check(prefix+"keyboard filter chooses seven days", "7d", a.usage.Range)
	report.check(prefix+"filter updates metrics", true, tt.HasText("460"))
	if err := tt.Click("Next chart day"); err != nil {
		report.check(prefix+"select chart day with pointer", "no error", err.Error())
	}
	report.check(prefix+"chart navigation selects first day", 0, a.usage.UI.chartIndex)
	tt.Key(0, ui.KeyEnter)
	report.check(prefix+"chart navigation accepts keyboard activation", 1, a.usage.UI.chartIndex)
	report.check(prefix+"cache denominator includes input and cache writes", true, tt.HasText("54.5%") && tt.HasText("Read 210"))
	report.check(prefix+"reauthorization visible and unsupported service absent", true, tt.HasText("Sign in again") && tt.HasText("DeepSeek") && !tt.HasText("Custom unknown"))
	if err := tt.Click("Usage date range"); err != nil {
		report.check(prefix+"open filter with pointer", "no error", err.Error())
	} else if err = tt.Click("All time"); err != nil {
		report.check(prefix+"choose all time", "no error", err.Error())
	}
	report.check(prefix+"pointer filter chooses all time", "all", a.usage.Range)
	report.check(prefix+"all-time hides fee estimate", true, tt.HasText("496") && !tt.HasText("API estimate") && !tt.HasText("sessions priced"))
	day := now.Format("2006-01-02")
	cell, found := tt.Access(day)
	report.check(prefix+"heat day accessible direct/cached semantics", true, found && cell.Focusable && cell.Description == fmt.Sprintf("%s · 210 direct tokens · 250 cached tokens · 3 sessions", day))
	if err := tt.Click(day); err != nil {
		report.check(prefix+"click today's calendar cell", "no error", err.Error())
	}
	report.check(prefix+"date input selects production day view", day, a.usage.SelectedDay)
	report.check(prefix+"daily metrics exclude invented fee", true, tt.HasText("460") && !tt.HasText("API estimate"))
	tt.Key(0, ui.KeyEnter)
	report.check(prefix+"calendar keyboard activation toggles day", "", a.usage.SelectedDay)
	a.usage.UI.chartIndex = 10000
	a.setUsageRange("7d")
	tt.Frame()
	report.check(prefix+"short range clears stale chart index", -1, a.usage.UI.chartIndex)
	for i := 0; i < 32; i++ {
		row, found := tt.Access("openai/model-a")
		if found && row.Focused {
			break
		}
		tt.Key(0, ui.KeyTab)
	}
	ranking, found := tt.Access("openai/model-a")
	report.check(prefix+"ranking exact details available on keyboard focus", true, found && ranking.Focusable && ranking.Focused && ranking.Description == "openai/model-a · 400 tokens · 1 primary session" && tt.HasText("openai/model-a · 400 tokens · 1 primary session"))
	for i := 0; i < 5; i++ {
		tt.Frame()
	}
	after, _ := fixture.snapshot()
	report.check(prefix+"date/filter/repeated frames do not refetch IPC", len(before), len(after))
	report.check(prefix+"quota unchanged by date range", true, tt.HasText("37.5% used") && tt.HasText("Unlimited"))
	report.Steps = append(report.Steps, usageE2EStep{Name: prefix + "text-only semantic artifact", Passed: true, Detail: tt.Texts()})
	usageE2ECheckStates(report, a, tt, prefix, now)
}

func usageE2ECheckStates(report *usageE2EReport, a *app, tt *ui.Tester, prefix string, now time.Time) {
	for _, state := range []struct {
		status usageLoadStatus
		label  string
	}{{usageLoading, ""}, {usageError, "History unavailable"}, {usageIdle, "No usage available"}} {
		a.usage.History = usageHistory{Status: state.status, Error: "Fixture history unavailable", attemptedAt: now}
		tt.Frame()
		visible := state.label == "" || tt.HasText(state.label)
		report.check(prefix+string(state.status)+" is visible and not a zero metric", true, visible && !tt.HasText("Total tokens") && !tt.HasText("Updating history"))
	}
	a.usage.Subscriptions = usageSubscriptions{Status: usageError, Error: "Fixture quota unavailable", attemptedAt: now}
	tt.Frame()
	report.check(prefix+"quota errors are visible not zero", true, tt.HasText("Connections unavailable") && tt.HasText("Quota data unavailable") && !tt.HasText("0% used"))
	a.lastProject = ""
	tt.Frame()
	report.check(prefix+"lost workspace clears old history and subscription cards", true, a.usage.History.Stats == nil && len(a.usage.Subscriptions.Cards) == 0 && !tt.HasText("37.5% used"))
}

func usageE2ECheckCalendarPipeline(ctx context.Context, report *usageE2EReport, client *daemon.Client) {
	loc, err := time.LoadLocation("America/New_York")
	if err != nil {
		report.check("DST timezone available", "no error", err.Error())
		return
	}
	for _, test := range []struct {
		name  string
		now   time.Time
		today string
	}{
		{"spring DST", time.Date(2026, 3, 8, 18, 0, 0, 0, loc), "2026-03-08"},
		{"fall DST", time.Date(2026, 11, 1, 18, 0, 0, 0, loc), "2026-11-01"},
		{"year rollover", time.Date(2027, 1, 1, 18, 0, 0, 0, loc), "2027-01-01"},
	} {
		if ctx.Err() != nil {
			return
		}
		stats := usageParseStats(usageE2EHistoryJSON(test.now))
		aggregates := usageBuildAggregates(stats, test.now)
		view := aggregates.Views["7d"]
		labels := make([]string, 0, len(view.Daily))
		for _, day := range view.Daily {
			labels = append(labels, day.date.Format("2006-01-02"))
		}
		consecutive := len(labels) == 7
		for i := 1; i < len(view.Daily); i++ {
			consecutive = consecutive && view.Daily[i-1].date.AddDate(0, 0, 1).Equal(view.Daily[i].date)
		}
		report.check(test.name+" pipeline uses seven local calendar dates", map[string]any{"last": test.today, "consecutive": true, "total": 460}, map[string]any{"last": labels[len(labels)-1], "consecutive": consecutive, "total": view.Totals.total()})
	}
}

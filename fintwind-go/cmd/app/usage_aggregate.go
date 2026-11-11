package main

import (
	"fmt"
	"math"
	"sort"
	"strings"
	"time"
)

type usageAggregates struct {
	Views map[string]*usageViewData
	Days  map[string]*usageViewData
	Heat  []usageHeatDay
}

type usageViewBuilder struct {
	view      usageViewData
	models    map[string]*usageRank
	providers map[string]*usageRank
	projects  map[string]*usageRank
	active    map[string]bool
}

func usageNewBuilder(start, end time.Time) *usageViewBuilder {
	return &usageViewBuilder{
		view:   usageViewData{Start: start, End: end},
		models: make(map[string]*usageRank), providers: make(map[string]*usageRank),
		projects: make(map[string]*usageRank), active: make(map[string]bool),
	}
}

func usageDate(date time.Time) time.Time {
	return time.Date(date.Year(), date.Month(), date.Day(), 0, 0, 0, 0, date.Location())
}

func usageTimestampDay(timestamp uint64, location *time.Location) (time.Time, bool) {
	if timestamp > math.MaxInt64 {
		return time.Time{}, false
	}
	date := time.Unix(int64(timestamp), 0).In(location)
	if date.Year() < 1 || date.Year() > 9999 {
		return time.Time{}, false
	}
	return usageDate(date), true
}

func usageBuildAggregates(stats *usageStats, now time.Time) usageAggregates {
	today := usageDate(now)
	mondayOffset := (int(today.Weekday()) + 6) % 7
	heatStart := today.AddDate(0, 0, -mondayOffset-25*7)
	builders := map[string]*usageViewBuilder{
		"7d":  usageNewBuilder(today.AddDate(0, 0, -6), today),
		"30d": usageNewBuilder(today.AddDate(0, 0, -29), today),
		"90d": usageNewBuilder(today.AddDate(0, 0, -89), today),
		"all": usageNewBuilder(today.AddDate(0, 0, -89), today),
	}
	dayBuilders := make(map[string]*usageViewBuilder)
	daily := make(map[string]usageDayBucket)
	heat := make(map[string]usageHeatDay)
	for date := heatStart; !date.After(today); date = date.AddDate(0, 0, 1) {
		key := date.Format("2006-01-02")
		dayBuilders[key] = usageNewBuilder(date, date)
		heat[key] = usageHeatDay{Date: date}
	}
	if stats != nil {
		for _, original := range stats.Entries {
			lanes := usageEntryDayLanes(original)
			entryDays, allDaysValid := usageSplitEntryDays(original, lanes, now.Location())
			allActive := make(map[string]bool)
			for key, entry := range entryDays {
				allActive[key] = true
				date, _ := time.ParseInLocation("2006-01-02", key, now.Location())
				if date.After(today) {
					continue
				}
				bucket := daily[key]
				bucket.date, bucket.label = date, fmt.Sprintf("%d/%d", date.Month(), date.Day())
				bucket.input = usageAdd(bucket.input, entry.InputTokens)
				bucket.output = usageAdd(bucket.output, entry.OutputTokens, entry.ReasoningTokens)
				bucket.cache = usageAdd(bucket.cache, entry.CacheReadTokens, entry.CacheWriteTokens)
				bucket.sessions++
				daily[key] = bucket
				if builder := dayBuilders[key]; builder != nil {
					builder.addEntry(*entry, map[string]bool{key: true})
				}
			}
			seenDays := make(map[string]bool)
			for _, lane := range lanes {
				date, ok := usageTimestampDay(lane.Timestamp, now.Location())
				if !ok || date.Before(heatStart) || date.After(today) {
					continue
				}
				key := date.Format("2006-01-02")
				cell := heat[key]
				cell.Direct, cell.Total = usageAdd(cell.Direct, lane.Direct), usageAdd(cell.Total, lane.Total)
				if !seenDays[key] {
					cell.Sessions++
					seenDays[key] = true
				}
				heat[key] = cell
			}
			builders["all"].addEntry(original, allActive)
			for _, key := range []string{"7d", "30d", "90d"} {
				builder := builders[key]
				selected := usageEntry{Model: original.Model, Directory: original.Directory}
				if len(original.ModelLanes) > 0 {
					selected.Model = nil
				}
				active := make(map[string]bool)
				for dateKey, entry := range entryDays {
					date, _ := time.ParseInLocation("2006-01-02", dateKey, now.Location())
					if date.Before(builder.view.Start) || date.After(today) {
						continue
					}
					usageMergeEntry(&selected, *entry)
					active[dateKey] = true
				}
				if len(active) > 0 {
					if allDaysValid && len(active) == len(entryDays) {
						selected = original
					}
					builder.addEntry(selected, active)
				}
			}
		}
	}
	result := usageAggregates{Views: make(map[string]*usageViewData), Days: make(map[string]*usageViewData)}
	for key, builder := range builders {
		builder.fillDaily(daily)
		result.Views[key] = builder.finish()
	}
	for date := heatStart; !date.After(today); date = date.AddDate(0, 0, 1) {
		key := date.Format("2006-01-02")
		builder := dayBuilders[key]
		builder.fillDaily(daily)
		result.Days[key] = builder.finish()
		result.Heat = append(result.Heat, heat[key])
	}
	return result
}

func usageEntryDayLanes(entry usageEntry) []usageDayShare {
	total := usageEntryTotal(entry)
	direct := usageAdd(entry.InputTokens, entry.OutputTokens, entry.ReasoningTokens)
	var cost float64
	if entry.Cost != nil {
		cost = usageSafeCost(*entry.Cost)
	}
	if entry.Days == nil {
		return []usageDayShare{{
			Timestamp: entry.Timestamp, Subagent: entry.SubagentTokens > 0, Total: total, Direct: direct,
			Input: entry.InputTokens, Output: usageAdd(entry.OutputTokens, entry.ReasoningTokens),
			CacheRead: entry.CacheReadTokens, CacheWrite: entry.CacheWriteTokens,
			Cost: cost,
		}}
	}
	lanes := append([]usageDayShare(nil), entry.Days...)
	var sum usageDayShare
	for _, lane := range lanes {
		sum.Total = usageAdd(sum.Total, lane.Total)
		sum.Direct = usageAdd(sum.Direct, lane.Direct)
		sum.Input = usageAdd(sum.Input, lane.Input)
		sum.Output = usageAdd(sum.Output, lane.Output)
		sum.CacheRead = usageAdd(sum.CacheRead, lane.CacheRead)
		sum.CacheWrite = usageAdd(sum.CacheWrite, lane.CacheWrite)
		sum.Cost = usageAddCost(sum.Cost, lane.Cost)
	}
	if remainder := saturatingSub(total, sum.Total); remainder > 0 {
		lanes = append(lanes, usageDayShare{
			Timestamp: entry.Timestamp, Subagent: entry.SubagentTokens > 0, Total: remainder,
			Direct: saturatingSub(direct, sum.Direct), Input: saturatingSub(entry.InputTokens, sum.Input),
			Output:     saturatingSub(usageAdd(entry.OutputTokens, entry.ReasoningTokens), sum.Output),
			CacheRead:  saturatingSub(entry.CacheReadTokens, sum.CacheRead),
			CacheWrite: saturatingSub(entry.CacheWriteTokens, sum.CacheWrite),
			Cost:       max(0, cost-sum.Cost),
		})
	} else if gap := max(0, cost-sum.Cost); gap > 0 {
		lanes = append(lanes, usageDayShare{Timestamp: entry.Timestamp, Cost: gap})
	}
	if len(lanes) == 0 {
		lanes = append(lanes, usageDayShare{Timestamp: entry.Timestamp})
	}
	return lanes
}

// Day costs are deliberately absent. The protocol's default zero cost is not proof of coverage.
func usageSplitEntryDays(original usageEntry, lanes []usageDayShare, location *time.Location) (map[string]*usageEntry, bool) {
	days := make(map[string]*usageEntry)
	valid := true
	for _, lane := range lanes {
		date, ok := usageTimestampDay(lane.Timestamp, location)
		if !ok {
			valid = false
			continue
		}
		key := date.Format("2006-01-02")
		entry := days[key]
		if entry == nil {
			entry = &usageEntry{Directory: original.Directory, Model: original.Model}
			if len(original.ModelLanes) > 0 {
				entry.Model = nil
			}
			days[key] = entry
		}
		entry.Timestamp = max(entry.Timestamp, lane.Timestamp)
		entry.InputTokens = usageAdd(entry.InputTokens, lane.Input)
		entry.OutputTokens = usageAdd(entry.OutputTokens, lane.Output)
		entry.CacheReadTokens = usageAdd(entry.CacheReadTokens, lane.CacheRead)
		entry.CacheWriteTokens = usageAdd(entry.CacheWriteTokens, lane.CacheWrite)
		if original.Days == nil {
			entry.SubagentTokens, entry.SubagentDirect = original.SubagentTokens, original.SubagentDirect
		} else if lane.Subagent {
			entry.SubagentTokens = usageAdd(entry.SubagentTokens, lane.Total)
			entry.SubagentDirect = usageAdd(entry.SubagentDirect, lane.Direct)
		}
	}
	for _, lane := range original.ModelLanes {
		if len(lane.Days) == 0 && original.Days == nil {
			date, ok := usageTimestampDay(original.Timestamp, location)
			if ok {
				if entry := days[date.Format("2006-01-02")]; entry != nil {
					entry.ModelLanes = append(entry.ModelLanes, lane)
				}
			}
			continue
		}
		for _, day := range lane.Days {
			date, ok := usageTimestampDay(day.Timestamp, location)
			if !ok {
				continue
			}
			if entry := days[date.Format("2006-01-02")]; entry != nil && day.Total > 0 {
				entry.ModelLanes = append(entry.ModelLanes, usageModelLane{Model: lane.Model, Total: day.Total})
			}
		}
	}
	for _, child := range original.SubagentModels {
		for _, day := range child.Days {
			date, ok := usageTimestampDay(day.Timestamp, location)
			if !ok {
				continue
			}
			if entry := days[date.Format("2006-01-02")]; entry != nil && day.Total > 0 {
				entry.SubagentModels = append(entry.SubagentModels, usageSubagentModel{
					SessionID: child.SessionID, Model: child.Model,
					InputTokens: day.Input, OutputTokens: day.Output,
					CacheReadTokens: day.CacheRead, CacheWriteTokens: day.CacheWrite,
				})
			}
		}
	}
	return days, valid
}

func usageMergeEntry(dst *usageEntry, src usageEntry) {
	dst.Timestamp = max(dst.Timestamp, src.Timestamp)
	dst.InputTokens = usageAdd(dst.InputTokens, src.InputTokens)
	dst.OutputTokens = usageAdd(dst.OutputTokens, src.OutputTokens)
	dst.ReasoningTokens = usageAdd(dst.ReasoningTokens, src.ReasoningTokens)
	dst.CacheReadTokens = usageAdd(dst.CacheReadTokens, src.CacheReadTokens)
	dst.CacheWriteTokens = usageAdd(dst.CacheWriteTokens, src.CacheWriteTokens)
	dst.SubagentTokens = usageAdd(dst.SubagentTokens, src.SubagentTokens)
	dst.SubagentDirect = usageAdd(dst.SubagentDirect, src.SubagentDirect)
	dst.ModelLanes = append(dst.ModelLanes, src.ModelLanes...)
	dst.SubagentModels = append(dst.SubagentModels, src.SubagentModels...)
}

func usageEntryTotal(entry usageEntry) uint64 {
	return usageAdd(entry.InputTokens, entry.OutputTokens, entry.ReasoningTokens, entry.CacheReadTokens, entry.CacheWriteTokens)
}

type usageModelShare struct {
	ID      string
	Tokens  uint64
	Primary bool
}

func usageModelID(model *string) string {
	if model == nil {
		return ""
	}
	return strings.TrimSpace(*model)
}

// Child totals already belong to the parent entry. Only the attribution is split here.
func usageEntryModelShares(entry usageEntry) []usageModelShare {
	var childTotal uint64
	for _, child := range entry.SubagentModels {
		childTotal = usageAdd(childTotal, usageAdd(child.InputTokens, child.OutputTokens, child.ReasoningTokens, child.CacheReadTokens, child.CacheWriteTokens))
	}
	total := usageEntryTotal(entry)
	childTotal = min(childTotal, total)
	unknownChild := min(saturatingSub(entry.SubagentTokens, childTotal), saturatingSub(total, childTotal))
	parentTotal := saturatingSub(total, usageAdd(childTotal, unknownChild))
	parent := make(map[string]uint64)
	remaining := parentTotal
	if len(entry.ModelLanes) == 0 {
		parent[usageModelID(entry.Model)] = remaining
		remaining = 0
	} else {
		for _, lane := range entry.ModelLanes {
			tokens := min(lane.Total, remaining)
			id := strings.TrimSpace(lane.Model)
			parent[id] = usageAdd(parent[id], tokens)
			remaining -= tokens
		}
	}
	parent[""] = usageAdd(parent[""], remaining)
	primary := ""
	var largest uint64
	for id, tokens := range parent {
		if tokens > largest || (tokens == largest && id < primary) {
			primary, largest = id, tokens
		}
	}
	shares := parent
	shares[""] = usageAdd(shares[""], unknownChild)
	remaining = childTotal
	for _, child := range entry.SubagentModels {
		tokens := min(usageAdd(child.InputTokens, child.OutputTokens, child.ReasoningTokens, child.CacheReadTokens, child.CacheWriteTokens), remaining)
		id := usageModelID(child.Model)
		shares[id] = usageAdd(shares[id], tokens)
		remaining -= tokens
	}
	if largest == 0 {
		for id, tokens := range shares {
			if tokens > largest || (tokens == largest && id < primary) {
				primary, largest = id, tokens
			}
		}
	}
	result := make([]usageModelShare, 0, len(shares))
	for id, tokens := range shares {
		if tokens > 0 || id == primary {
			result = append(result, usageModelShare{ID: id, Tokens: tokens, Primary: id == primary})
		}
	}
	return result
}

func usageAddRank(rows map[string]*usageRank, id, label string, tokens uint64, session bool) {
	row := rows[id]
	if row == nil {
		row = &usageRank{ID: id, Label: label}
		rows[id] = row
	}
	row.Tokens = usageAdd(row.Tokens, tokens)
	if session {
		row.Sessions++
	}
}

func (b *usageViewBuilder) addEntry(entry usageEntry, active map[string]bool) {
	t := &b.view.Totals
	t.sessions = usageAdd(t.sessions, 1)
	t.input = usageAdd(t.input, entry.InputTokens)
	t.output = usageAdd(t.output, entry.OutputTokens)
	t.reasoning = usageAdd(t.reasoning, entry.ReasoningTokens)
	t.cacheRead = usageAdd(t.cacheRead, entry.CacheReadTokens)
	t.cacheWrite = usageAdd(t.cacheWrite, entry.CacheWriteTokens)
	if entry.Cost != nil && usageValidCost(*entry.Cost) {
		t.cost = usageAddCost(t.cost, *entry.Cost)
		t.costedSessions++
	}
	for day := range active {
		b.active[day] = true
	}
	for _, share := range usageEntryModelShares(entry) {
		label := share.ID
		if label == "" {
			label = "Unknown model"
		}
		usageAddRank(b.models, share.ID, usagePlainLabel(label), share.Tokens, share.Primary)
		provider, _, ok := strings.Cut(share.ID, "/")
		if !ok || provider == "" {
			provider = ""
		}
		providerLabel := provider
		if providerLabel == "" {
			providerLabel = "Unknown provider"
		}
		usageAddRank(b.providers, provider, usagePlainLabel(providerLabel), share.Tokens, share.Primary)
	}
	directory := ""
	if entry.Directory != nil {
		directory = strings.TrimSpace(*entry.Directory)
	}
	normalized := strings.ReplaceAll(directory, "\\", "/")
	key := strings.ToLower(strings.TrimRight(normalized, "/"))
	if key == "" && normalized != "" {
		key = "/"
	}
	label := usagePlainLabel(directory)
	if label == "" {
		label = "Unknown project"
	}
	usageAddRank(b.projects, key, label, usageEntryTotal(entry), true)
}

func (b *usageViewBuilder) fillDaily(daily map[string]usageDayBucket) {
	for date := b.view.Start; !date.After(b.view.End); date = date.AddDate(0, 0, 1) {
		bucket := daily[date.Format("2006-01-02")]
		bucket.date, bucket.label = date, fmt.Sprintf("%d/%d", date.Month(), date.Day())
		b.view.Daily = append(b.view.Daily, bucket)
	}
}

func (b *usageViewBuilder) finish() *usageViewData {
	b.view.Totals.activeDays = len(b.active)
	b.view.Models = usageSortedRanks(b.models)
	b.view.Providers = usageSortedRanks(b.providers)
	b.view.Projects = usageSortedRanks(b.projects)
	return &b.view
}

func usageSortedRanks(rows map[string]*usageRank) []usageRank {
	result := make([]usageRank, 0, len(rows))
	for _, row := range rows {
		result = append(result, *row)
	}
	sort.Slice(result, func(i, j int) bool {
		if result[i].Tokens != result[j].Tokens {
			return result[i].Tokens > result[j].Tokens
		}
		return result[i].ID < result[j].ID
	})
	return result
}

func usageAdd(values ...uint64) uint64 {
	var total uint64
	for _, value := range values {
		if math.MaxUint64-total < value {
			return math.MaxUint64
		}
		total += value
	}
	return total
}

func saturatingSub(a, b uint64) uint64 {
	if a < b {
		return 0
	}
	return a - b
}

func usageValidCost(value float64) bool {
	return value >= 0 && !math.IsNaN(value) && !math.IsInf(value, 0)
}

func usageSafeCost(value float64) float64 {
	if !usageValidCost(value) {
		return 0
	}
	return value
}

func usageAddCost(a, b float64) float64 {
	a, b = usageSafeCost(a), usageSafeCost(b)
	if math.MaxFloat64-a < b {
		return math.MaxFloat64
	}
	return a + b
}

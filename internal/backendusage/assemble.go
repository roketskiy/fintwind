package backendusage

import "sort"

func subtract(a, b uint64) uint64 {
	if a < b {
		return 0
	}
	return a - b
}
func lanes(e Entry) [5]uint64 {
	return [5]uint64{e.Input, e.Output, e.Reasoning, e.CacheRead, e.CacheWrite}
}
func total(tokens [5]uint64) uint64 {
	var sum uint64
	for _, n := range tokens {
		sum = add(sum, n)
	}
	return sum
}
func costValue(cost *float64) float64 {
	if cost == nil {
		return 0
	}
	return *cost
}
func wholeDay(e Entry) DayShare {
	return DayShare{Timestamp: e.Timestamp, Direct: add(add(e.Input, e.Output), e.Reasoning), Total: total(lanes(e)), Input: e.Input, Output: add(e.Output, e.Reasoning), CacheRead: e.CacheRead, CacheWrite: e.CacheWrite, Cost: costValue(e.Cost)}
}
func wholeHour(e Entry) HourShare {
	return HourShare{Timestamp: e.Timestamp, Estimated: true, Input: e.Input, Output: add(e.Output, e.Reasoning), CacheRead: e.CacheRead, CacheWrite: e.CacheWrite, Cost: costValue(e.Cost)}
}

func applySplit(entry *Entry, split *sessionSplit) {
	if split == nil {
		return
	}
	var measured [5]uint64
	for _, m := range split.Messages {
		tokens := [5]uint64{m.Input, subtract(m.Output, m.Reasoning), m.Reasoning, m.CacheRead, m.CacheWrite}
		for i, n := range tokens {
			measured[i] = add(measured[i], n)
		}
	}
	entry.Input = max(entry.Input, measured[0])
	entry.Output = max(entry.Output, measured[1])
	entry.Reasoning = max(entry.Reasoning, measured[2])
	entry.CacheRead = max(entry.CacheRead, measured[3])
	entry.CacheWrite = max(entry.CacheWrite, measured[4])
	entry.Days = append([]DayShare{}, split.Days...)
	entry.Hours = append([]HourShare{}, split.Hours...)
	entry.ModelLanes = append([]ModelLane{}, split.Models...)
}

func foldChild(parent *Entry, child Entry, id string) {
	days := append([]DayShare(nil), child.Days...)
	if child.Days == nil {
		days = []DayShare{wholeDay(child)}
	}
	var recorded [4]uint64
	var dayCost float64
	for _, d := range days {
		parts := [4]uint64{d.Input, d.Output, d.CacheRead, d.CacheWrite}
		for i, n := range parts {
			recorded[i] = add(recorded[i], n)
		}
		dayCost += d.Cost
	}
	want := [4]uint64{child.Input, add(child.Output, child.Reasoning), child.CacheRead, child.CacheWrite}
	var missing [4]uint64
	var missingTotal uint64
	for i, n := range want {
		missing[i] = subtract(n, recorded[i])
		missingTotal = add(missingTotal, missing[i])
	}
	if missingTotal > 0 {
		days = append(days, DayShare{Timestamp: child.Timestamp, Direct: add(missing[0], missing[1]), Total: missingTotal, Input: missing[0], Output: missing[1], CacheRead: missing[2], CacheWrite: missing[3], Cost: max(0, costValue(child.Cost)-dayCost)})
	}
	hours := append([]HourShare(nil), child.Hours...)
	if len(hours) == 0 {
		hours = []HourShare{wholeHour(child)}
	} else {
		recorded = [4]uint64{}
		for _, h := range hours {
			parts := [4]uint64{h.Input, h.Output, h.CacheRead, h.CacheWrite}
			for i, n := range parts {
				recorded[i] = add(recorded[i], n)
			}
		}
		missingTotal = 0
		for i, n := range want {
			missing[i] = subtract(n, recorded[i])
			missingTotal = add(missingTotal, missing[i])
		}
		if missingTotal > 0 {
			hours = append(hours, HourShare{Timestamp: child.Timestamp, Estimated: true, Input: missing[0], Output: missing[1], CacheRead: missing[2], CacheWrite: missing[3]})
		}
	}
	parts := []SubagentModel{}
	represented := [5]uint64{}
	var representedCost float64
	for _, lane := range child.ModelLanes {
		if lane.Total == 0 {
			continue
		}
		model, cost := lane.Model, lane.Cost
		part := SubagentModel{SessionID: id, Model: &model, Input: lane.Tokens[0], Output: lane.Tokens[1], Reasoning: lane.Tokens[2], CacheRead: lane.Tokens[3], CacheWrite: lane.Tokens[4], Cost: &cost, Days: lane.Days, Hours: lane.Hours, Requests: lane.Requests}
		parts = append(parts, part)
		for i, n := range lane.Tokens {
			represented[i] = add(represented[i], n)
		}
		representedCost += cost
	}
	var remaining [5]uint64
	var unassigned uint64
	for i, n := range lanes(child) {
		remaining[i] = subtract(n, represented[i])
		unassigned = add(unassigned, remaining[i])
	}
	if len(parts) == 0 || unassigned > 0 {
		dayRemainders := []DayShare{}
		seenDays := map[string]bool{}
		for _, day := range days {
			key := localDay(day.Timestamp)
			sums := [4]uint64{}
			var sumCost float64
			if !seenDays[key] {
				for _, part := range parts {
					for _, d := range part.Days {
						if localDay(d.Timestamp) == key {
							values := [4]uint64{d.Input, d.Output, d.CacheRead, d.CacheWrite}
							for i, n := range values {
								sums[i] = add(sums[i], n)
							}
							sumCost += d.Cost
						}
					}
				}
			}
			seenDays[key] = true
			day.Input = subtract(day.Input, sums[0])
			day.Output = subtract(day.Output, sums[1])
			day.CacheRead = subtract(day.CacheRead, sums[2])
			day.CacheWrite = subtract(day.CacheWrite, sums[3])
			day.Direct = add(day.Input, day.Output)
			day.Total = add(add(day.Direct, day.CacheRead), day.CacheWrite)
			day.Cost = max(0, day.Cost-sumCost)
			if day.Total > 0 {
				dayRemainders = append(dayRemainders, day)
			}
		}
		hourRemainders := []HourShare{}
		seenHours := map[uint64]bool{}
		for _, hour := range hours {
			key := hour.Timestamp / 3600
			sums := [4]uint64{}
			if !seenHours[key] {
				for _, part := range parts {
					for _, h := range part.Hours {
						if h.Timestamp/3600 == key {
							values := [4]uint64{h.Input, h.Output, h.CacheRead, h.CacheWrite}
							for i, n := range values {
								sums[i] = add(sums[i], n)
							}
						}
					}
				}
			}
			seenHours[key] = true
			hour.Input = subtract(hour.Input, sums[0])
			hour.Output = subtract(hour.Output, sums[1])
			hour.CacheRead = subtract(hour.CacheRead, sums[2])
			hour.CacheWrite = subtract(hour.CacheWrite, sums[3])
			hour.Estimated = true
			if add(add(hour.Input, hour.Output), add(hour.CacheRead, hour.CacheWrite)) > 0 {
				hourRemainders = append(hourRemainders, hour)
			}
		}
		var cost *float64
		if child.Cost != nil {
			value := max(0, *child.Cost-representedCost)
			cost = &value
		}
		parts = append(parts, SubagentModel{SessionID: id, Model: child.Model, Input: remaining[0], Output: remaining[1], Reasoning: remaining[2], CacheRead: remaining[3], CacheWrite: remaining[4], Cost: cost, Days: dayRemainders, Hours: hourRemainders})
	}
	if parent.Days == nil {
		parent.Days = []DayShare{wholeDay(*parent)}
	}
	if len(parent.Hours) == 0 {
		parent.Hours = []HourShare{wholeHour(*parent)}
	}
	for _, d := range days {
		d.Subagent = true
		parent.Days = append(parent.Days, d)
	}
	for _, part := range parts {
		parent.Hours = append(parent.Hours, part.Hours...)
	}
	parent.SubagentModels = append(parent.SubagentModels, parts...)
	parent.Input = add(parent.Input, child.Input)
	parent.Output = add(parent.Output, child.Output)
	parent.Reasoning = add(parent.Reasoning, child.Reasoning)
	parent.CacheRead = add(parent.CacheRead, child.CacheRead)
	parent.CacheWrite = add(parent.CacheWrite, child.CacheWrite)
	if parent.Cost != nil || child.Cost != nil {
		value := costValue(parent.Cost) + costValue(child.Cost)
		parent.Cost = &value
	}
	if parent.SubagentSessions < ^uint32(0) {
		parent.SubagentSessions++
	}
	parent.SubagentTokens = add(parent.SubagentTokens, total(lanes(child)))
	parent.SubagentDirect = add(parent.SubagentDirect, add(add(child.Input, child.Output), child.Reasoning))
}

func assemble(rows []sessionFact) []Entry {
	rows = append([]sessionFact(nil), rows...)
	entries := []Entry{}
	entryIndex := map[string]int{}
	for _, row := range orderedFacts(rows) {
		f := row.Facts
		entry := Entry{Timestamp: max(f.Updated, f.Created), Directory: f.Directory, Cost: f.Cost, Input: f.Tokens[0], Output: f.Tokens[1], Reasoning: f.Tokens[2], CacheRead: f.Tokens[3], CacheWrite: f.Tokens[4]}
		applySplit(&entry, row.Split)
		if f.Kind == "SubAgent" && f.Parent != nil {
			if i, ok := entryIndex[*f.Parent]; ok {
				foldChild(&entries[i], entry, row.ID)
				entryIndex[row.ID] = i
				continue
			}
		}
		entryIndex[row.ID] = len(entries)
		entries = append(entries, entry)
	}
	sort.SliceStable(entries, func(i, j int) bool { return entries[i].Timestamp < entries[j].Timestamp })
	return entries
}

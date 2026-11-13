package main

// Daemon wire suites. All timestamps derive from time.Local on the test
// side; the daemon inherits the same system zone (BuildEnv drops TZ), so day
// and hour buckets are deterministic without pinning any zone.

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"sort"
	"time"
)

var (
	universeTS struct {
		t10, t12, tNext uint64
		t0ms            uint64
		oldMs           uint64
	}
	universeBaseline []map[string]any
)

func initTimes() {
	base := time.Now().Add(-50 * time.Hour)
	y, m, d := base.Date()
	t10 := time.Date(y, m, d, 10, 0, 0, 0, time.Local).Unix()
	t12 := t10 + 2*3600
	universeTS.t10 = uint64(t10)
	universeTS.t12 = uint64(t12)
	universeTS.tNext = uint64(t10 + 30*3600)
	universeTS.t0ms = uint64(t12)*1000 - 3600000 + 123
	universeTS.oldMs = uint64(time.Now().UnixMilli()) - 10*86400*1000
}

func msgRow(id string, tsMs uint64, in, out, reason, cr, cw uint64, model map[string]any, cost any) map[string]any {
	row := map[string]any{
		"id": id, "type": "assistant",
		"time":   map[string]any{"created": tsMs},
		"tokens": map[string]any{"input": in, "output": out, "reasoning": reason, "cache": map[string]any{"read": cr, "write": cw}},
	}
	if model != nil {
		row["model"] = model
	}
	if cost != nil {
		row["cost"] = cost
	}
	return row
}

func modelA() map[string]any { return map[string]any{"providerID": "p", "modelID": "a", "id": "zzz"} }

func listRow(id string, createdMs, updatedMs uint64, extra map[string]any) map[string]any {
	row := map[string]any{"id": id, "time": map[string]any{"created": createdMs, "updated": updatedMs}}
	for key, value := range extra {
		row[key] = value
	}
	return row
}

func tokens(in, out, reason, cr, cw uint64) map[string]any {
	return map[string]any{"input": in, "output": out, "reasoning": reason, "cache": map[string]any{"read": cr, "write": cw}}
}

func universeSeed() map[string]any {
	t10ms, t12ms := universeTS.t10*1000, universeTS.t12*1000
	tNextMs := universeTS.tNext * 1000
	createdMs := t10ms - 86400*1000
	rows := []any{
		listRow("ses_r", createdMs, t12ms, map[string]any{
			"model": map[string]any{"providerID": "p", "modelID": "m", "id": "i"}, "location": map[string]any{"directory": "/proj"},
			"tokens": tokens(1000, 400, 100, 50, 60), "cost": 1.5}),
		listRow("ses_c", createdMs, t12ms, map[string]any{
			"parentID": "ses_r", "tokens": tokens(100, 30, 10, 5, 6), "cost": 0.0}),
		listRow("ses_g", createdMs, tNextMs, map[string]any{
			"parentID": "ses_c", "tokens": tokens(10, 3, 1, 0, 0)}),
		listRow("ses_o", createdMs, t12ms, map[string]any{
			"parentID": "ses_missing", "tokens": tokens(17, 3, 0, 0, 0)}),
		listRow("ses_f", createdMs, t12ms, map[string]any{
			"fork_session_id": "ses_r", "parentID": "ses_r", "tokens": tokens(50, 20, 0, 0, 0)}),
		listRow("ses_s", createdMs, t12ms, map[string]any{
			"tokens": tokens(30, 10, 0, 0, 0)}),
		listRow("ses_old", universeTS.oldMs-86400*1000, universeTS.oldMs, map[string]any{
			"tokens": tokens(5, 2, 0, 0, 0)}),
		map[string]any{"id": "ses_t", "time": map[string]any{"created": universeTS.t0ms},
			"tokens": tokens(11, 1, 0, 0, 0)},
		listRow("ses_m", 0, t12ms, map[string]any{"tokens": tokens(13, 2, 0, 0, 0)}),
		listRow("ses_bad", 0, 0, map[string]any{"tokens": tokens(3, 1, 0, 0, 0)}),
		listRow("ses_nc1", createdMs, t12ms, map[string]any{"tokens": tokens(6, 2, 0, 0, 0), "cost": "free"}),
		listRow("ses_nc2", createdMs, t12ms, map[string]any{"tokens": tokens(6, 2, 0, 0, 0), "cost": map[string]any{}}),
		listRow("ses_nc3", createdMs, t12ms, map[string]any{"tokens": tokens(6, 2, 0, 0, 0), "cost": true}),
		listRow("ses_nc4", createdMs, t12ms, map[string]any{"tokens": tokens(6, 2, 0, 0, 0), "cost": json.RawMessage("1e400")}),
	}
	pages := []any{map[string]any{"cursor": "", "rows": rows, "next": ""}}
	messages := map[string]any{
		"ses_r": []any{map[string]any{"cursor": "", "rows": []any{
			msgRow("m1", t10ms, 600, 200, 100, 50, 60, modelA(), 1.0),
			msgRow("m2", t12ms, 400, 200, 0, 0, 0, modelA(), 0.5),
		}, "next": ""}},
		"ses_c": []any{map[string]any{"cursor": "", "rows": []any{
			msgRow("m3", t10ms, 100, 30, 10, 5, 6, nil, nil),
		}, "next": ""}},
		"ses_g": []any{map[string]any{"cursor": "", "rows": []any{
			msgRow("m4", tNextMs, 10, 3, 1, 0, 0, map[string]any{"providerID": "p", "modelID": "b"}, nil),
		}, "next": ""}},
		"ses_o": []any{map[string]any{"cursor": "", "rows": []any{
			msgRow("m5", t10ms, 7, 3, 0, 0, 0, modelA(), 0.2),
			msgRow("m0", 0, 10, 0, 0, 0, 0, modelA(), nil),
		}, "next": ""}},
		"ses_f": []any{map[string]any{"cursor": "", "rows": []any{
			msgRow("m6", t10ms, 50, 20, 0, 0, 0, modelA(), nil),
		}, "next": ""}},
		"ses_old": []any{map[string]any{"cursor": "", "rows": []any{
			msgRow("m7", t10ms, 5, 2, 0, 0, 0, modelA(), nil),
		}, "next": ""}},
		"ses_t": []any{map[string]any{"cursor": "", "rows": []any{
			msgRow("m8", t10ms, 11, 1, 0, 0, 0, modelA(), nil),
		}, "next": ""}},
		"ses_m": []any{map[string]any{"cursor": "", "rows": []any{
			msgRow("m9", t10ms, 13, 2, 0, 0, 0, modelA(), nil),
		}, "next": ""}},
		"ses_bad": []any{map[string]any{"cursor": "", "rows": []any{
			msgRow("m10", t10ms, 3, 1, 0, 0, 0, modelA(), nil),
		}, "next": ""}},
		"ses_nc1": []any{map[string]any{"cursor": "", "rows": []any{msgRow("n1", t10ms, 6, 2, 0, 0, 0, modelA(), nil)}, "next": ""}},
		"ses_nc2": []any{map[string]any{"cursor": "", "rows": []any{msgRow("n2", t10ms, 6, 2, 0, 0, 0, modelA(), nil)}, "next": ""}},
		"ses_nc3": []any{map[string]any{"cursor": "", "rows": []any{msgRow("n3", t10ms, 6, 2, 0, 0, 0, modelA(), nil)}, "next": ""}},
		"ses_nc4": []any{map[string]any{"cursor": "", "rows": []any{msgRow("n4", t10ms, 6, 2, 0, 0, 0, modelA(), nil)}, "next": ""}},
	}
	return map[string]any{"pages": pages, "messages": messages}
}

func (h *harness) universeSuite() error {
	initTimes()
	if err := h.writeSeed(universeSeed()); err != nil {
		return err
	}
	h.resetCalls()
	first, err := h.snapshot(false, "")
	if err != nil {
		return fmt.Errorf("first snapshot: %w", err)
	}
	if refreshing, _ := first["refreshing"].(bool); !refreshing {
		return errors.New("first snapshot did not start a refresh")
	}
	if entries := entriesOf(first); len(entries) != 0 {
		return fmt.Errorf("first snapshot should be empty, got %d entries", len(entries))
	}
	if scanned, _ := first["sessionsScanned"].(float64); scanned != 0 {
		return fmt.Errorf("first snapshot scanned %v sessions", scanned)
	}
	stats, err := h.pollDone(false, "", 90*time.Second)
	if err != nil {
		return err
	}
	if refreshing, _ := stats["refreshing"].(bool); refreshing {
		return errors.New("refresh never finished")
	}
	if errValue, _ := stats["refreshError"].(string); stats["refreshError"] != nil && errValue != "" {
		return fmt.Errorf("unexpected refresh error: %v", stats["refreshError"])
	}
	entries := entriesOf(stats)
	if len(entries) != 12 {
		raw, _ := json.Marshal(stats)
		return fmt.Errorf("want 12 entries, got %d: %s", len(entries), raw)
	}
	for _, entry := range entries {
		if _, ok := entry["model"]; ok {
			return fmt.Errorf("entry unexpectedly attributed a selected model: %v", entry)
		}
	}
	if err := h.assertUniverse(entries); err != nil {
		return err
	}
	rawStats, _ := json.Marshal(stats)
	var wire struct {
		Entries []map[string]any `json:"entries"`
	}
	if err := json.Unmarshal(rawStats, &wire); err != nil {
		return err
	}
	byTokens := func(input float64) map[string]any {
		for _, entry := range wire.Entries {
			if u64(entry["inputTokens"]) == uint64(input) && u64(entry["timestamp"]) != 0 {
				return entry
			}
		}
		return nil
	}
	_ = byTokens
	sEntry := findEntry(entries, func(entry map[string]any) bool { return u64(entry["inputTokens"]) == 30 })
	if sEntry == nil {
		return errors.New("summary-only entry missing")
	}
	for _, key := range []string{"days", "hours", "modelLanes"} {
		if _, ok := sEntry[key]; ok {
			return fmt.Errorf("summary-only entry exposes %s", key)
		}
	}
	rEntry := findEntry(entries, func(entry map[string]any) bool { return u64(entry["inputTokens"]) == 1110 })
	if rEntry == nil {
		return errors.New("root entry missing")
	}
	if _, ok := rEntry["days"]; !ok {
		return errors.New("split entry lost days")
	}
	tables, err := h.tableNames()
	if err != nil {
		return err
	}
	if !equalStrings(tables, []string{"usage_snapshots_v1", "usage_splits_v1"}) {
		return fmt.Errorf("usage index tables = %v", tables)
	}
	dirs, err := h.engineDirs()
	if err != nil {
		return err
	}
	if len(dirs) != 1 {
		return fmt.Errorf("want exactly one usage engine, got %v", dirs)
	}
	if payload, ok, err := h.splitPayload(h.anySource(), "ses_s"); err != nil || !ok || payload != "" {
		return fmt.Errorf("summary-only session cached a split: %q %v", payload, err)
	}
	universeBaseline = entries
	seedRaw, _ := json.Marshal(universeSeed())
	seedSum := sha256.Sum256(seedRaw)
	_ = os.WriteFile(h.callPath("universe-seed.json"), seedRaw, 0o600)
	h.notes["universe-seed-sha256"] = hex.EncodeToString(seedSum[:])
	h.notes["universe-formula"] = "root input=1000 own+100 ses_c+10 ses_g=1110; output=400+30+3=433 reasoning=100+10+1=111; subagentTokens=(100+30+10+5+6)+(10+3+1)=151+14=165 sessions=2 direct=(100+40)+(10+4)=154; days=1 own+2 subagent=3 (t10/t12 share one local date); cost keys only [1110], nc1-4 nil, ses_c fallback explicit 0"
	h.checks["universe-snapshot-shape"] = true
	return nil
}

func (h *harness) anySource() string {
	sources, err := h.splitSources()
	if err != nil || len(sources) == 0 {
		return ""
	}
	return sources[0]
}

func (h *harness) assertUniverse(entries []map[string]any) error {
	byInput := func(input uint64) map[string]any {
		return findEntry(entries, func(entry map[string]any) bool { return u64(entry["inputTokens"]) == input })
	}
	root := byInput(1110)
	if root == nil {
		return errors.New("root entry (input 1110) missing")
	}
	expect := map[string]uint64{"outputTokens": 433, "reasoningTokens": 111, "cacheReadTokens": 55, "cacheWriteTokens": 66, "subagentSessions": 2, "subagentTokens": 165, "subagentDirect": 154}
	for key, want := range expect {
		if got := u64(root[key]); got != want {
			return fmt.Errorf("root %s = %d, want %d", key, got, want)
		}
	}
	if cost, ok := f64(root["cost"]); !ok || cost != 1.5 {
		return fmt.Errorf("root cost = %v", root["cost"])
	}
	lanes, _ := root["modelLanes"].([]any)
	if len(lanes) != 1 {
		return fmt.Errorf("root lanes = %d, want 1", len(lanes))
	}
	lane, _ := lanes[0].(map[string]any)
	if lane["model"] != "p/a" || u64(lane["total"]) != 1610 {
		return fmt.Errorf("root lane = %v", lane)
	}
	tokens, _ := lane["tokens"].([]any)
	want := []uint64{1000, 400, 100, 50, 60}
	for i, number := range tokens {
		if u64(number) != want[i] {
			return fmt.Errorf("root lane tokens = %v", tokens)
		}
	}
	requests, _ := lane["requests"].([]any)
	if len(requests) != 2 {
		return fmt.Errorf("root lane requests = %d", len(requests))
	}
	first, _ := requests[0].(map[string]any)
	if u64(first["timestamp"]) != universeTS.t10 {
		return fmt.Errorf("root lane first request ts = %v", first["timestamp"])
	}
	firstTokens, _ := first["tokens"].([]any)
	for i, number := range firstTokens {
		if u64(number) != ([]uint64{600, 200, 100, 50, 60})[i] {
			return fmt.Errorf("root lane first request tokens = %v", firstTokens)
		}
	}
	days, _ := root["days"].([]any)
	if len(days) != 3 {
		return fmt.Errorf("root days = %d, want 3", len(days))
	}
	own := 0
	for _, day := range days {
		bucket, _ := day.(map[string]any)
		subagent, _ := bucket["subagent"].(bool)
		if !subagent {
			own++
			if u64(bucket["output"]) != 500 || u64(bucket["total"]) != 1610 || u64(bucket["input"]) != 1000 {
				return fmt.Errorf("root own day bucket = %v", bucket)
			}
		}
	}
	if own != 1 {
		return errors.New("root own day bucket not unique")
	}
	hours, _ := root["hours"].([]any)
	haveHours := map[uint64]bool{}
	for _, hour := range hours {
		bucket, _ := hour.(map[string]any)
		haveHours[u64(bucket["timestamp"])] = true
	}
	if !haveHours[universeTS.t10] || !haveHours[universeTS.t12] {
		return fmt.Errorf("root own hours missing t10/t12: %v", hours)
	}
	models, _ := root["subagentModels"].([]any)
	bySession := map[string]map[string]any{}
	for _, part := range models {
		item, _ := part.(map[string]any)
		bySession[item["sessionId"].(string)] = item
	}
	fallback, ok := bySession["ses_c"]
	if !ok {
		return errors.New("subagent fallback for ses_c missing")
	}
	if fallback["model"] != nil {
		return fmt.Errorf("fallback model should be null: %v", fallback["model"])
	}
	if cost, ok := f64(fallback["cost"]); !ok || cost != 0 {
		return fmt.Errorf("fallback cost = %v (want explicit 0)", fallback["cost"])
	}
	if u64(fallback["inputTokens"]) != 100 || u64(fallback["reasoningTokens"]) != 10 {
		return fmt.Errorf("fallback tokens wrong: %v", fallback)
	}
	lanePart, ok := bySession["ses_g"]
	if !ok {
		return errors.New("subagent lane for ses_g missing")
	}
	if lanePart["model"] != "p/b" || u64(lanePart["inputTokens"]) != 10 {
		return fmt.Errorf("ses_g part = %v", lanePart)
	}
	orphan := byInput(17)
	if orphan == nil {
		return errors.New("orphan entry missing")
	}
	orphanLanes, _ := orphan["modelLanes"].([]any)
	if len(orphanLanes) != 1 {
		return errors.New("orphan lost its lane")
	}
	orphanDays, _ := orphan["days"].([]any)
	zeroDay := false
	for _, day := range orphanDays {
		if u64(day.(map[string]any)["timestamp"]) == 0 {
			zeroDay = true
		}
	}
	if !zeroDay || len(orphanDays) != 2 {
		return fmt.Errorf("orphan zero-timestamp day missing: %v", orphanDays)
	}
	orphanRequests := []any{}
	for _, laneItem := range orphanLanes {
		requests, _ := laneItem.(map[string]any)["requests"].([]any)
		orphanRequests = append(orphanRequests, requests...)
	}
	zeroRequest := false
	for _, request := range orphanRequests {
		if u64(request.(map[string]any)["timestamp"]) == 0 {
			zeroRequest = true
		}
	}
	if !zeroRequest {
		return errors.New("orphan zero-timestamp request missing")
	}
	fork := findEntry(entries, func(entry map[string]any) bool { return u64(entry["inputTokens"]) == 50 })
	if fork == nil {
		return errors.New("fork entry missing")
	}
	if _, ok := fork["cost"]; ok {
		return errors.New("fork entry should not carry a cost")
	}
	forkLanes, _ := fork["modelLanes"].([]any)
	if len(forkLanes) != 1 {
		return errors.New("fork lost its lane")
	}
	old := findEntry(entries, func(entry map[string]any) bool { return u64(entry["inputTokens"]) == 5 })
	if old == nil {
		return errors.New("old session entry missing")
	}
	createdOnly := findEntry(entries, func(entry map[string]any) bool { return u64(entry["inputTokens"]) == 11 })
	if createdOnly == nil {
		return errors.New("created-only session entry missing")
	}
	wantTs := universeTS.t0ms / 1000 * 1000 / 1000
	if u64(createdOnly["timestamp"]) != wantTs {
		return fmt.Errorf("created-only timestamp = %v, want %v", createdOnly["timestamp"], wantTs)
	}
	missingCreated := findEntry(entries, func(entry map[string]any) bool { return u64(entry["inputTokens"]) == 13 })
	if missingCreated == nil || u64(missingCreated["timestamp"]) != universeTS.t12 {
		return errors.New("explicit-zero-created entry wrong")
	}
	withCost := []uint64{}
	for _, entry := range entries {
		if _, ok := entry["cost"]; ok {
			withCost = append(withCost, u64(entry["inputTokens"]))
		}
	}
	sort.Slice(withCost, func(i, j int) bool { return withCost[i] < withCost[j] })
	if !equalUints(withCost, []uint64{1110}) {
		return fmt.Errorf("top-level cost keys = %v, want only root", withCost)
	}
	return nil
}

func equalUints(left, right []uint64) bool {
	if len(left) != len(right) {
		return false
	}
	for i := range left {
		if left[i] != right[i] {
			return false
		}
	}
	return true
}

func (h *harness) rawEntries(stats map[string]any) []map[string]any {
	raw, _ := json.Marshal(stats)
	var wire struct {
		Entries []map[string]any `json:"entries"`
	}
	_ = json.Unmarshal(raw, &wire)
	return wire.Entries
}

func (h *harness) costKeyPresent(stats map[string]any, input uint64) (bool, error) {
	for _, entry := range h.rawEntries(stats) {
		if u64(entry["inputTokens"]) == input {
			_, ok := entry["cost"]
			return ok, nil
		}
	}
	return false, fmt.Errorf("entry with input %d missing", input)
}

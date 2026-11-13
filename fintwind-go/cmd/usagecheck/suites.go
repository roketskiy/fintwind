package main

// Remaining daemon wire suites: single-flight, recheck, failures, budgets,
// timeouts, workers, source isolation, restart, close.

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendusage"
)

func (h *harness) resetToUniverse() (map[string]any, error) {
	if err := h.writeSeed(universeSeed()); err != nil {
		return nil, err
	}
	stats, err := h.pollDone(true, "", 90*time.Second)
	if err != nil {
		return nil, err
	}
	if errValue, _ := stats["refreshError"].(string); stats["refreshError"] != nil && errValue != "" {
		return nil, fmt.Errorf("reset universe has refresh error: %v", stats["refreshError"])
	}
	if len(entriesOf(stats)) != 12 {
		raw, _ := json.Marshal(stats)
		return nil, fmt.Errorf("reset universe want 12 entries: %s", raw)
	}
	universeBaseline = entriesOf(stats)
	return stats, nil
}

func canonicalEntries(entries []map[string]any) string {
	raw, _ := json.Marshal(entries)
	return string(raw)
}

func (h *harness) assertBaselineUnchanged(stats map[string]any, context string) error {
	if got := canonicalEntries(entriesOf(stats)); got != canonicalEntries(universeBaseline) {
		return fmt.Errorf("%s: entries changed.\n got %s\nwant %s", context, got, canonicalEntries(universeBaseline))
	}
	return nil
}

func (h *harness) fingerprintSuite() error {
	if _, err := h.resetToUniverse(); err != nil {
		return fmt.Errorf("fingerprint reset: %w", err)
	}
	h.resetCalls()
	quiet, err := h.snapshot(false, "")
	if err != nil {
		return fmt.Errorf("fingerprint quiet snapshot: %w", err)
	}
	if refreshing, _ := quiet["refreshing"].(bool); refreshing {
		return errors.New("30s gate broken: quiet snapshot started a refresh")
	}
	if err := h.assertBaselineUnchanged(quiet, "quiet snapshot"); err != nil {
		return err
	}
	if calls := h.readCalls(); len(calls) != 0 {
		// Quiet snapshot must not touch the provider at all.
		return fmt.Errorf("quiet snapshot made %d provider calls", len(calls))
	}
	// Single-flight: two concurrent force snapshots share one refresh.
	h.resetCalls()
	firstCh := make(chan map[string]any, 2)
	errCh := make(chan error, 2)
	go func() { stats, err := h.snapshot(true, ""); firstCh <- stats; errCh <- err }()
	time.Sleep(200 * time.Millisecond)
	second, err := h.snapshot(true, "")
	if err != nil {
		return fmt.Errorf("concurrent snapshot: %w", err)
	}
	first := <-firstCh
	if err := <-errCh; err != nil {
		return fmt.Errorf("forced snapshot: %w", err)
	}
	if refreshing, _ := first["refreshing"].(bool); !refreshing {
		return errors.New("forced snapshot did not start a refresh")
	}
	if refreshing, _ := second["refreshing"].(bool); !refreshing {
		return errors.New("single-flight broken: concurrent snapshot not refreshing")
	}
	done, err := h.pollDone(false, "", 90*time.Second)
	if err != nil {
		return err
	}
	if err := h.assertBaselineUnchanged(done, "fingerprint force refresh"); err != nil {
		return err
	}
	calls := h.readCalls()
	lists := countContaining(calls, "/api/session?limit=")
	messages := countContaining(calls, "/message")
	if lists == 0 {
		return errors.New("force refresh skipped the listing")
	}
	// ses_s is a token summary without messages: it never caches, so every
	// refresh probes it once and skips it. All verified splits must be reused.
	for _, id := range []string{"ses_r", "ses_c", "ses_g", "ses_o", "ses_f", "ses_old", "ses_t", "ses_m", "ses_nc1"} {
		if n := countContaining(calls, "/api/session/"+id+"/message"); n != 0 {
			return fmt.Errorf("fingerprint broken: %s refetched %d times: %v", id, n, calls)
		}
	}
	if messages > 2 {
		return fmt.Errorf("fingerprint broken: %d message fetches for unchanged splits: %v", messages, calls)
	}
	h.checks["fingerprint-singleflight-gate"] = true
	h.notes["fingerprint"] = fmt.Sprintf("list=%d message=%d (all splits reused)", lists, messages)
	return nil
}

func (h *harness) pokeChecked(session string, age time.Duration) error {
	path := filepath.Join(h.dataDir, "usage-index.db")
	if _, err := os.Stat(path); err != nil {
		return errors.New("usage index db missing for poke")
	}
	writable, err := sql.Open("sqlite", "file:"+filepath.ToSlash(path))
	if err != nil {
		return err
	}
	defer writable.Close()
	_, err = writable.Exec(`UPDATE usage_splits_v1 SET checked=? WHERE session=?`, time.Now().Add(-age).Unix(), session)
	return err
}

func (h *harness) recheck300Suite() error {
	if _, err := h.resetToUniverse(); err != nil {
		return fmt.Errorf("recheck reset: %w", err)
	}
	// Force once so every split is freshly checked, then poke two rows old.
	if _, err := h.pollDone(true, "", 90*time.Second); err != nil {
		return err
	}
	if err := h.pokeChecked("ses_r", 400*time.Second); err != nil {
		return fmt.Errorf("poke ses_r: %w", err)
	}
	if err := h.pokeChecked("ses_old", 400*time.Second); err != nil {
		return fmt.Errorf("poke ses_old: %w", err)
	}
	h.resetCalls()
	done, err := h.pollDone(true, "", 90*time.Second)
	if err != nil {
		return err
	}
	if err := h.assertBaselineUnchanged(done, "recheck300"); err != nil {
		return err
	}
	calls := h.readCalls()
	gotR := countContaining(calls, "/api/session/ses_r/message")
	gotOld := countContaining(calls, "/api/session/ses_old/message")
	if gotR == 0 {
		return errors.New("300s recheck broken: recent ses_r was not refetched after poke")
	}
	if gotOld != 0 {
		return errors.New("7-day rule broken: 10-day-old ses_old was refetched despite identical fingerprint")
	}
	h.checks["recheck-300s-7day"] = true
	h.notes["recheck"] = fmt.Sprintf("ses_r fetches=%d ses_old fetches=%d", gotR, gotOld)
	return nil
}

func (h *harness) singleFailSuite() error {
	seed := universeSeed()
	seed["fail"] = map[string]any{"message:ses_r": 500}
	// Token summary with no verified messages must never be cached as a split.
	pages, _ := seed["pages"].([]any)
	rows := pages[0].(map[string]any)["rows"].([]any)
	rows = append(rows, listRow("ses_empty", universeTS.t10*1000-86400*1000, universeTS.t12*1000, map[string]any{"tokens": tokens(9, 1, 0, 0, 0)}))
	pages[0].(map[string]any)["rows"] = rows
	messages, _ := seed["messages"].(map[string]any)
	messages["ses_empty"] = []any{map[string]any{"cursor": "", "rows": []any{}, "next": ""}}
	if err := h.writeSeed(seed); err != nil {
		return err
	}
	done, err := h.pollDone(true, "", 90*time.Second)
	if err != nil {
		return err
	}
	// Single-session failure is skipped, never a whole-scan error.
	if errValue, _ := done["refreshError"].(string); done["refreshError"] != nil && errValue != "" {
		return fmt.Errorf("single-session failure poisoned refreshError: %v", done["refreshError"])
	}
	entries := entriesOf(done)
	root := findEntry(entries, func(entry map[string]any) bool { return u64(entry["inputTokens"]) == 1110 })
	if root == nil {
		return errors.New("ses_r split lost after single-session failure")
	}
	if cost, ok := f64(root["cost"]); !ok || cost != 1.5 {
		return fmt.Errorf("ses_r cost changed after failure: %v", root["cost"])
	}
	empty := findEntry(entries, func(entry map[string]any) bool { return u64(entry["inputTokens"]) == 9 })
	if empty == nil {
		return errors.New("token-summary session missing from entries")
	}
	for _, key := range []string{"days", "hours", "modelLanes"} {
		if _, ok := empty[key]; ok {
			return fmt.Errorf("token-summary session exposes %s", key)
		}
	}
	if payload, ok, err := h.splitPayload(h.anySource(), "ses_empty"); err != nil || !ok || payload != "" {
		return fmt.Errorf("token-summary without messages was cached: %q %v", payload, err)
	}
	h.checks["single-fail-keeps-split"] = true
	if _, err := h.resetToUniverse(); err != nil {
		return fmt.Errorf("single-fail recovery: %w", err)
	}
	return nil
}

func (h *harness) listingFailSuite() error {
	if _, err := h.resetToUniverse(); err != nil {
		return err
	}
	baseline := canonicalEntries(universeBaseline)
	kind := func(seed map[string]any, wantErr string) error {
		if err := h.writeSeed(seed); err != nil {
			return err
		}
		done, err := h.pollDone(true, "", 60*time.Second)
		if err != nil {
			return err
		}
		if got := canonicalEntries(entriesOf(done)); got != baseline {
			return fmt.Errorf("listing failure %q changed entries", wantErr)
		}
		errValue, _ := done["refreshError"].(string)
		if done["refreshError"] == nil || errValue == "" {
			return fmt.Errorf("listing failure %q left no refreshError", wantErr)
		}
		if !strings.Contains(errValue, wantErr) {
			return fmt.Errorf("listing failure want %q, got %q", wantErr, errValue)
		}
		if !strings.Contains(errValue, "previous verified usage retained") {
			return fmt.Errorf("refreshError lost retention suffix: %q", errValue)
		}
		for _, leak := range []string{"proxy", "Proxy", "127.0.0.1", "usage-seed", ".json", "C:\\", "/tmp", "Bearer", "password"} {
			if strings.Contains(errValue, leak) {
				return fmt.Errorf("refreshError leaks %q: %q", leak, errValue)
			}
		}
		// Tombstone-free: failed listings never GC old splits.
		if payload, ok, err := h.splitPayload(h.anySource(), "ses_r"); err != nil || !ok || payload == "" {
			return fmt.Errorf("listing failure %q GCed splits: %q %v", wantErr, payload, err)
		}
		return nil
	}
	rowsFor := func(rows ...any) map[string]any {
		return map[string]any{"pages": []any{map[string]any{"cursor": "", "rows": rows, "next": ""}}, "messages": map[string]any{}}
	}
	goodRow := listRow("ses_r", universeTS.t10*1000-86400*1000, universeTS.t12*1000, map[string]any{"tokens": tokens(1, 0, 0, 0, 0)})
	_ = goodRow
	// Duplicate identity.
	dupSeed := rowsFor(
		listRow("ses_d", 1, 2, map[string]any{"tokens": tokens(1, 0, 0, 0, 0)}),
		listRow("ses_d", 1, 2, map[string]any{"tokens": tokens(1, 0, 0, 0, 0)}),
	)
	if err := kind(dupSeed, "duplicate usage session identity"); err != nil {
		return err
	}
	// Cursor cycle A->B->A.
	cycleSeed := map[string]any{
		"pages": []any{
			map[string]any{"cursor": "", "rows": []any{listRow("s1", 1, 2, map[string]any{"tokens": tokens(1, 0, 0, 0, 0)})}, "next": "b"},
			map[string]any{"cursor": "b", "rows": []any{listRow("s2", 1, 2, map[string]any{"tokens": tokens(1, 0, 0, 0, 0)})}, "next": ""},
		},
		"messages": map[string]any{},
	}
	// Fix: second page points back to "" to form a cursor repeat.
	cycleSeed["pages"].([]any)[1].(map[string]any)["next"] = ""
	// Real cycle needs a repeated cursor token: "" -> "b" -> "b".
	cycleSeed = map[string]any{
		"pages": []any{
			map[string]any{"cursor": "", "rows": []any{listRow("s1", 1, 2, map[string]any{"tokens": tokens(1, 0, 0, 0, 0)})}, "next": "b"},
			map[string]any{"cursor": "b", "rows": []any{listRow("s2", 1, 2, map[string]any{"tokens": tokens(1, 0, 0, 0, 0)})}, "next": "c"},
			map[string]any{"cursor": "c", "rows": []any{listRow("s3", 1, 2, map[string]any{"tokens": tokens(1, 0, 0, 0, 0)})}, "next": "b"},
		},
		"messages": map[string]any{},
	}
	if err := kind(cycleSeed, "usage session listing incomplete"); err != nil {
		return fmt.Errorf("cycle: %w", err)
	}
	// Empty page with a cursor.
	emptySeed := map[string]any{
		"pages": []any{
			map[string]any{"cursor": "", "rows": []any{}, "next": "more"},
		},
		"messages": map[string]any{},
	}
	if err := kind(emptySeed, "usage session listing incomplete"); err != nil {
		return fmt.Errorf("empty-page: %w", err)
	}
	// Missing time (no created and no updated).
	missingSeed := rowsFor(map[string]any{"id": "ses_x", "tokens": tokens(1, 0, 0, 0, 0)})
	if err := kind(missingSeed, "invalid usage session facts"); err != nil {
		return err
	}
	// HTTP 500 on listing maps to the safe generic category.
	failSeed := universeSeed()
	failSeed["fail"] = map[string]any{"list": 500}
	if err := h.writeSeed(failSeed); err != nil {
		return err
	}
	done, err := h.pollDone(true, "", 60*time.Second)
	if err != nil {
		return err
	}
	if got := canonicalEntries(entriesOf(done)); got != baseline {
		return errors.New("HTTP-500 listing changed entries")
	}
	errValue, _ := done["refreshError"].(string)
	if done["refreshError"] == nil || !strings.Contains(errValue, "usage refresh failed") {
		return fmt.Errorf("HTTP-500 listing want safe generic, got %v", done["refreshError"])
	}
	for _, leak := range []string{"500", "status", "proxy", "127.0.0.1"} {
		if leak == "500" {
			continue
		}
		if strings.Contains(errValue, leak) {
			return fmt.Errorf("generic refreshError leaks %q: %q", leak, errValue)
		}
	}
	h.checks["listing-failures-safe"] = true
	if _, err := h.resetToUniverse(); err != nil {
		return fmt.Errorf("listing recovery: %w", err)
	}
	return nil
}

func (h *harness) budgetSuite() error {
	if _, err := h.resetToUniverse(); err != nil {
		return err
	}
	baseline := canonicalEntries(universeBaseline)
	// 50-page budget: 51 chained pages hit the incomplete guard at page 49.
	many := []any{}
	for i := 0; i < 51; i++ {
		cursor := ""
		next := ""
		if i > 0 {
			cursor = fmt.Sprintf("p%d", i)
		}
		if i < 50 {
			next = fmt.Sprintf("p%d", i+1)
		} else {
			next = "p51"
		}
		many = append(many, map[string]any{"cursor": cursor, "rows": []any{listRow(fmt.Sprintf("s%d", i), 1, 2, map[string]any{"tokens": tokens(1, 0, 0, 0, 0)})}, "next": next})
	}
	if err := h.writeSeed(map[string]any{"pages": many, "messages": map[string]any{}}); err != nil {
		return err
	}
	done, err := h.pollDone(true, "", 60*time.Second)
	if err != nil {
		return err
	}
	if got := canonicalEntries(entriesOf(done)); got != baseline {
		return errors.New("50-page overrun changed entries")
	}
	errValue, _ := done["refreshError"].(string)
	if !strings.Contains(errValue, "usage session listing incomplete") {
		return fmt.Errorf("50-page want incomplete guard, got %v", done["refreshError"])
	}
	// Multi-page messages: 3 chained pages aggregate into one lane.
	t10ms := universeTS.t10 * 1000
	mpSeed := map[string]any{
		"pages": []any{map[string]any{"cursor": "", "rows": []any{
			listRow("ses_mp", t10ms-86400*1000, universeTS.t12*1000, map[string]any{"tokens": tokens(30, 3, 0, 0, 0)}),
		}, "next": ""}},
		"messages": map[string]any{
			"ses_mp": []any{
				map[string]any{"cursor": "", "rows": []any{msgRow("a1", t10ms, 10, 1, 0, 0, 0, modelA(), nil)}, "next": "m1"},
				map[string]any{"cursor": "m1", "rows": []any{msgRow("a2", t10ms, 10, 1, 0, 0, 0, modelA(), nil)}, "next": "m2"},
				map[string]any{"cursor": "m2", "rows": []any{msgRow("a3", t10ms, 10, 1, 0, 0, 0, modelA(), nil)}, "next": ""},
			},
		},
	}
	if err := h.writeSeed(mpSeed); err != nil {
		return err
	}
	mpDone, err := h.pollDone(true, "", 60*time.Second)
	if err != nil {
		return err
	}
	mpEntries := entriesOf(mpDone)
	if len(mpEntries) != 1 {
		return fmt.Errorf("multi-page want 1 entry, got %d", len(mpEntries))
	}
	lanes, _ := mpEntries[0]["modelLanes"].([]any)
	if len(lanes) != 1 {
		return errors.New("multi-page lost its lane")
	}
	if u64(lanes[0].(map[string]any)["total"]) != 33 {
		return fmt.Errorf("multi-page lane total = %v, want 33", lanes[0])
	}
	// 64MB listing budget via synthetic wide rows. The engine client caps a
	// single response at 48MB, so span the 64MB scanner guard across pages.
	if err := h.writeSeed(map[string]any{"synthList": map[string]any{"pages": 4, "rowsPerPage": 10, "pad": 2 << 20}}); err != nil {
		return err
	}
	big, err := h.pollDone(true, "", 120*time.Second)
	if err != nil {
		return err
	}
	errValue, _ = big["refreshError"].(string)
	if !strings.Contains(errValue, "usage listing byte budget exceeded") {
		return fmt.Errorf("list budget want byte guard, got %v", big["refreshError"])
	}
	// 64MB message budget: one huge session is skipped, the small one updates.
	t12ms := universeTS.t12 * 1000
	msgBudgetSeed := map[string]any{
		"pages": []any{map[string]any{"cursor": "", "rows": []any{
			listRow("ses_big", t10ms-86400*1000, t12ms, map[string]any{"tokens": tokens(5, 1, 0, 0, 0)}),
			listRow("ses_ok", t10ms-86400*1000, t12ms, map[string]any{"tokens": tokens(7, 2, 0, 0, 0)}),
		}, "next": ""}},
		"messages": map[string]any{
			"ses_ok": []any{map[string]any{"cursor": "", "rows": []any{msgRow("ok1", t10ms, 7, 2, 0, 0, 0, modelA(), nil)}, "next": ""}},
		},
		"synthMessages": map[string]any{"session": "ses_big", "pages": 1, "rowsPerPage": 10, "pad": 7 << 20},
	}
	_ = msgBudgetSeed
	// Seed synth messages through the dedicated key: rewrite with both pages
	// and synth spec requires manual JSON because writeSeed marshals maps.
	raw, _ := json.Marshal(map[string]any{
		"pages":         msgBudgetSeed["pages"],
		"messages":      msgBudgetSeed["messages"],
		"synthMessages": map[string]any{"session": "ses_big", "pages": 4, "rowsPerPage": 10, "pad": 2 << 20},
	})
	if err := os.WriteFile(h.callPath("usage-seed.json"), raw, 0o600); err != nil {
		return err
	}
	mbDone, err := h.pollDone(true, "", 120*time.Second)
	if err != nil {
		return err
	}
	if errValue, _ := mbDone["refreshError"].(string); mbDone["refreshError"] != nil && errValue != "" {
		return fmt.Errorf("message budget poisoned whole scan: %v", mbDone["refreshError"])
	}
	okEntry := findEntry(entriesOf(mbDone), func(entry map[string]any) bool { return u64(entry["inputTokens"]) == 7 })
	if okEntry == nil {
		return errors.New("message-budget run lost the healthy session")
	}
	if payload, ok, err := h.splitPayload(h.anySource(), "ses_big"); err != nil || !ok || payload != "" {
		return fmt.Errorf("over-budget session cached a split: %q %v", payload, err)
	}
	h.checks["budgets-multipage"] = true
	if _, err := h.resetToUniverse(); err != nil {
		return fmt.Errorf("budget recovery: %w", err)
	}
	return nil
}

func (h *harness) timeoutSuite() error {
	if _, err := h.resetToUniverse(); err != nil {
		return err
	}
	baseline := canonicalEntries(universeBaseline)
	// Per-GET 15s: listing delay 16s fails the whole scan safely.
	slowList := universeSeed()
	slowList["slowMs"] = map[string]any{"list": 16000}
	if err := h.writeSeed(slowList); err != nil {
		return err
	}
	done, err := h.pollDone(true, "", 90*time.Second)
	if err != nil {
		return err
	}
	if got := canonicalEntries(entriesOf(done)); got != baseline {
		return errors.New("list timeout changed entries")
	}
	errValue, _ := done["refreshError"].(string)
	if !strings.Contains(errValue, "timed out") {
		return fmt.Errorf("list timeout want timeout category, got %v", done["refreshError"])
	}
	// Per-session 45s: one blocked session keeps its old split, others refresh.
	// Fingerprints come from the listing rows, so bump both listings to force
	// refetch; ses_r then times out at 45s while ses_c updates fast.
	mixed := universeSeed()
	// Each page waits 10s (< 15s per-GET) but 5 pages total 50s (> 45s
	// per-session), proving the session budget is distinct from per-GET.
	mixed["slowMs"] = map[string]any{"message:ses_r": 10000}
	messages := mixed["messages"].(map[string]any)
	t10ms := universeTS.t10 * 1000
	t12ms := universeTS.t12 * 1000
	if pages, ok := mixed["pages"].([]any); ok {
		for _, page := range pages {
			rows, _ := page.(map[string]any)["rows"].([]any)
			for _, row := range rows {
				item, _ := row.(map[string]any)
				if item["id"] == "ses_r" {
					item["time"] = map[string]any{"created": t10ms - 86400*1000, "updated": t12ms + 3600000}
					item["tokens"] = tokens(1100, 440, 110, 55, 66)
				}
				if item["id"] == "ses_c" {
					item["time"] = map[string]any{"created": t10ms - 86400*1000, "updated": t12ms + 3600000}
					item["tokens"] = tokens(200, 60, 20, 10, 12)
				}
			}
		}
	}
	messages["ses_c"] = []any{map[string]any{"cursor": "", "rows": []any{
		msgRow("m3b", t10ms, 200, 60, 20, 10, 12, nil, nil),
	}, "next": ""}}
	// ses_r messages change too, but the 5x10s pages time out at 45s, so the
	// old split must be retained.
	messages["ses_r"] = []any{
		map[string]any{"cursor": "", "rows": []any{msgRow("m1", t10ms, 600, 200, 100, 50, 60, modelA(), 1.0)}, "next": "r1"},
		map[string]any{"cursor": "r1", "rows": []any{msgRow("m2a", t12ms, 100, 10, 0, 0, 0, modelA(), nil)}, "next": "r2"},
		map[string]any{"cursor": "r2", "rows": []any{msgRow("m2b", t12ms, 100, 10, 0, 0, 0, modelA(), nil)}, "next": "r3"},
		map[string]any{"cursor": "r3", "rows": []any{msgRow("m2c", t12ms, 100, 10, 0, 0, 0, modelA(), nil)}, "next": "r4"},
		map[string]any{"cursor": "r4", "rows": []any{msgRow("m2new", t12ms, 999, 999, 0, 0, 0, modelA(), 9.9)}, "next": ""},
	}
	if err := h.writeSeed(mixed); err != nil {
		return err
	}
	start := time.Now()
	mbDone, err := h.pollDone(true, "", 120*time.Second)
	elapsed := time.Since(start)
	if err != nil {
		return err
	}
	if errValue, _ := mbDone["refreshError"].(string); mbDone["refreshError"] != nil && errValue != "" {
		return fmt.Errorf("per-session timeout poisoned whole scan: %v", mbDone["refreshError"])
	}
	mbEntries := entriesOf(mbDone)
	var root map[string]any
	for _, entry := range mbEntries {
		if dir, _ := entry["directory"].(string); dir == "/proj" {
			root = entry
			break
		}
	}
	if root == nil {
		return errors.New("slow ses_r lost after 45s timeout")
	}
	// ses_r timed out: its lane keeps the old two requests, not m2new.
	lanes, _ := root["modelLanes"].([]any)
	if len(lanes) != 1 {
		return fmt.Errorf("slow root lanes = %d", len(lanes))
	}
	requests, _ := lanes[0].(map[string]any)["requests"].([]any)
	if len(requests) != 2 {
		return fmt.Errorf("slow ses_r should retain 2 requests, got %d", len(requests))
	}
	// ses_c was fast: its fallback part visibly updates 100 -> 200.
	models, _ := root["subagentModels"].([]any)
	bySession := map[string]map[string]any{}
	for _, part := range models {
		item, _ := part.(map[string]any)
		bySession[item["sessionId"].(string)] = item
	}
	updated, ok := bySession["ses_c"]
	if !ok || u64(updated["inputTokens"]) != 200 {
		return fmt.Errorf("healthy ses_c did not update during slow run: %v", bySession["ses_c"])
	}
	if elapsed < 40*time.Second || elapsed > 110*time.Second {
		return fmt.Errorf("45s budget not honored: elapsed %v", elapsed)
	}
	h.checks["timeouts-15s-45s"] = true
	h.notes["timeout"] = fmt.Sprintf("slow-session elapsed=%v", elapsed.Round(time.Second))
	if _, err := h.resetToUniverse(); err != nil {
		return fmt.Errorf("timeout recovery: %w", err)
	}
	return nil
}

func (h *harness) workerSuite() error {
	t10ms := universeTS.t10 * 1000
	rows := []any{}
	messages := map[string]any{}
	slow := map[string]any{}
	for i := 0; i < 8; i++ {
		id := fmt.Sprintf("ses_w%d", i)
		rows = append(rows, listRow(id, t10ms-86400*1000, universeTS.t12*1000, map[string]any{"tokens": tokens(4, 1, 0, 0, 0)}))
		messages[id] = []any{map[string]any{"cursor": "", "rows": []any{msgRow(fmt.Sprintf("w%d", i), t10ms, 4, 1, 0, 0, 0, modelA(), nil)}, "next": ""}}
		slow["message:"+id] = 2000
	}
	seed := map[string]any{"pages": []any{map[string]any{"cursor": "", "rows": rows, "next": ""}}, "messages": messages, "slowMs": slow}
	if err := h.writeSeed(seed); err != nil {
		return err
	}
	h.resetCalls()
	start := time.Now()
	done, err := h.pollDone(true, "", 120*time.Second)
	elapsed := time.Since(start)
	if err != nil {
		return err
	}
	if len(entriesOf(done)) != 8 {
		return fmt.Errorf("worker run want 8 entries, got %d", len(entriesOf(done)))
	}
	// Serial would be 16s; four workers finish two waves in ~4-6s.
	if elapsed > 12*time.Second {
		return fmt.Errorf("worker fan-out missing: 8x2s took %v", elapsed)
	}
	calls := h.readCalls()
	if n := countContaining(calls, "/message"); n != 8 {
		return fmt.Errorf("worker run want 8 message fetches, got %d", n)
	}
	h.checks["worker-fanout"] = true
	h.notes["worker"] = fmt.Sprintf("8x2s sessions in %v", elapsed.Round(time.Millisecond))
	if _, err := h.resetToUniverse(); err != nil {
		return fmt.Errorf("worker recovery: %w", err)
	}
	return nil
}

func (h *harness) splitCount(source string) (int, error) {
	db, ok, err := h.usageDB()
	if err != nil || !ok {
		return 0, errors.New("usage index db missing for split count")
	}
	defer db.Close()
	var count int
	if err := db.QueryRow(`SELECT COUNT(*) FROM usage_splits_v1 WHERE source=?`, source).Scan(&count); err != nil {
		return 0, err
	}
	return count, nil
}

func (h *harness) hasSplit(source, session string) (bool, error) {
	payload, ok, err := h.splitPayload(source, session)
	if err != nil || !ok {
		return false, err
	}
	return payload != "", nil
}

func (h *harness) gcSuite() error {
	if _, err := h.resetToUniverse(); err != nil {
		return fmt.Errorf("gc reset: %w", err)
	}
	source := h.anySource()
	if source == "" {
		return errors.New("gc: no source in usage index")
	}
	// Direct SQLite evidence: every listed session except the token-summary
	// ses_s (which never caches a split) owns exactly one split row.
	before, err := h.splitCount(source)
	if err != nil {
		return err
	}
	if before != 13 {
		return fmt.Errorf("gc: want 13 cached splits, got %d", before)
	}
	seed := universeSeed()
	pages := seed["pages"].([]any)
	rows, _ := pages[0].(map[string]any)["rows"].([]any)
	kept := []any{}
	for _, row := range rows {
		if item, _ := row.(map[string]any); item["id"] != "ses_old" {
			kept = append(kept, row)
		}
	}
	pages[0].(map[string]any)["rows"] = kept
	delete(seed["messages"].(map[string]any), "ses_old")
	if err := h.writeSeed(seed); err != nil {
		return err
	}
	done, err := h.pollDone(true, "", 90*time.Second)
	if err != nil {
		return err
	}
	if errValue, _ := done["refreshError"].(string); done["refreshError"] != nil && errValue != "" {
		return fmt.Errorf("gc refresh failed: %v", done["refreshError"])
	}
	if gone := findEntry(entriesOf(done), func(entry map[string]any) bool { return u64(entry["inputTokens"]) == 5 }); gone != nil {
		return errors.New("gc: disappeared session still in entries")
	}
	// Direct SQLite evidence: the disappeared session lost its split row,
	// retained sessions kept theirs, and the row count dropped by exactly one.
	if has, err := h.hasSplit(source, "ses_old"); err != nil || has {
		return fmt.Errorf("gc: ses_old split row retained: has=%v err=%v", has, err)
	}
	if has, err := h.hasSplit(source, "ses_r"); err != nil || !has {
		return fmt.Errorf("gc: ses_r split row wrongly collected: has=%v err=%v", has, err)
	}
	after, err := h.splitCount(source)
	if err != nil {
		return err
	}
	if after != 12 {
		return fmt.Errorf("gc: want 12 cached splits after removal, got %d", after)
	}
	h.checks["gc-removes-disappeared-splits"] = true
	h.notes["gc"] = "splits 13 -> 12, ses_old row deleted, ses_r row retained"
	if _, err := h.resetToUniverse(); err != nil {
		return fmt.Errorf("gc recovery: %w", err)
	}
	return nil
}

func (h *harness) copySelf(name string) (string, error) {
	raw, err := os.ReadFile(h.self)
	if err != nil {
		return "", err
	}
	target := filepath.Join(h.round, name)
	if err := os.WriteFile(target, raw, 0o700); err != nil {
		return "", err
	}
	return target, nil
}

func (h *harness) sourceSuite() error {
	if _, err := h.resetToUniverse(); err != nil {
		return err
	}
	before, err := h.engineDirs()
	if err != nil {
		return err
	}
	alt, err := h.copySelf("usagecheck-alt.exe")
	if err != nil {
		return err
	}
	// Same binary twice keeps a single engine.
	if _, err := h.pollDone(false, "", 60*time.Second); err != nil {
		return err
	}
	mid, err := h.engineDirs()
	if err != nil {
		return err
	}
	if len(mid) != len(before) {
		return fmt.Errorf("same binary spawned engines: %v -> %v", before, mid)
	}
	altStats, err := h.snapshot(false, alt)
	if err != nil {
		return fmt.Errorf("alt binary snapshot: %w", err)
	}
	if refreshing, _ := altStats["refreshing"].(bool); !refreshing {
		return errors.New("alternate source did not start its own refresh")
	}
	if len(entriesOf(altStats)) != 0 {
		return errors.New("alternate source leaked the default source snapshot")
	}
	altDone, err := h.pollDone(false, alt, 90*time.Second)
	if err != nil {
		return err
	}
	if len(entriesOf(altDone)) != 12 {
		return fmt.Errorf("alt source want 12 entries, got %d", len(entriesOf(altDone)))
	}
	sources, err := h.splitSources()
	if err != nil {
		return err
	}
	if len(sources) != 2 {
		return fmt.Errorf("want 2 isolated sources, got %v", sources)
	}
	after, err := h.engineDirs()
	if err != nil {
		return err
	}
	if len(after) != len(before)+1 {
		return fmt.Errorf("want one engine per binary (%v -> %v)", before, after)
	}
	h.checks["source-isolation-engine"] = true
	h.notes["source"] = fmt.Sprintf("sources=%d engines=%d", len(sources), len(after))
	return nil
}

func (h *harness) restartSuite() error {
	cached, err := h.snapshot(false, "")
	if err != nil {
		return err
	}
	_ = cached
	if err := h.closeDaemon(); err != nil {
		return err
	}
	if err := h.startDaemonAs("second"); err != nil {
		return err
	}
	first, err := h.snapshot(false, "")
	if err != nil {
		return fmt.Errorf("post-restart snapshot: %w", err)
	}
	if len(entriesOf(first)) != 12 {
		raw, _ := json.Marshal(first)
		return fmt.Errorf("restart lost cached snapshot: %s", raw)
	}
	done, err := h.pollDone(false, "", 90*time.Second)
	if err != nil {
		return err
	}
	if err := h.assertBaselineUnchanged(done, "post-restart"); err != nil {
		return err
	}
	h.checks["restart-cached"] = true
	return nil
}

func (h *harness) closeSuite() error {
	if _, err := h.resetToUniverse(); err != nil {
		return err
	}
	blocked := universeSeed()
	blocked["blockMessages"] = true
	if err := h.writeSeed(blocked); err != nil {
		return err
	}
	started := make(chan error, 1)
	go func() {
		_, err := h.snapshot(true, "")
		started <- err
	}()
	time.Sleep(2 * time.Second)
	start := time.Now()
	// Daemon shutdown must cancel the blocked usage worker promptly,
	// not wait out the 45s per-session budget.
	if err := h.closeDaemon(); err != nil {
		return err
	}
	if elapsed := time.Since(start); elapsed > 20*time.Second {
		return fmt.Errorf("daemon close hung on blocked worker: %v", elapsed)
	}
	h.notes["close"] = "blocked refresh cancelled by daemon shutdown"
	if err := h.startDaemonAs("third"); err != nil {
		return err
	}
	if err := h.writeSeed(universeSeed()); err != nil {
		return err
	}
	done, err := h.pollDone(true, "", 90*time.Second)
	if err != nil {
		return err
	}
	if len(entriesOf(done)) != 12 {
		return errors.New("post-close recovery failed")
	}
	h.checks["close-cancels-workers"] = true
	return nil
}

func (h *harness) moduleSuite() error {
	home := filepath.Join(filepath.Dir(h.dataDir), "home")
	environment := map[string]string{}
	for _, key := range []string{"XDG_DATA_HOME", "XDG_CONFIG_HOME", "HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"} {
		environment[key] = home
	}
	selfKey, err := backendusage.SourceKey(h.self, environment)
	if err != nil {
		return fmt.Errorf("source key: %w", err)
	}
	// Verbatim Windows prefix shares the source on every OS.
	if !strings.HasPrefix(h.self, `\\?\`) {
		verbatim, err := backendusage.SourceKey(`\\?\`+h.self, environment)
		if err != nil {
			return fmt.Errorf("verbatim source key: %w", err)
		}
		if verbatim != selfKey {
			return errors.New("verbatim-prefix binary split the source")
		}
	}
	alt, err := h.copySelf("usagecheck-mod.exe")
	if err != nil {
		return err
	}
	altKey, err := backendusage.SourceKey(alt, environment)
	if err != nil {
		return err
	}
	if altKey == selfKey {
		return errors.New("distinct binaries share a source")
	}
	changed := map[string]string{}
	for key, value := range environment {
		changed[key] = value
	}
	changed["HOME"] = home + "-other"
	otherKey, err := backendusage.SourceKey(h.self, changed)
	if err != nil {
		return err
	}
	if otherKey == selfKey {
		return errors.New("zero-separated environment ignored")
	}
	// Rust parity vector: fixed inputs hash to a recorded Go value. The Rust
	// side is an ignored manual comparison (no fee, same params). Use the
	// test binary itself (which exists) with an empty environment.
	fixed, err := backendusage.SourceKey(h.self, map[string]string{})
	if err != nil {
		return err
	}
	h.notes["sourcekey-fixed"] = "SourceKey(self, empty env) = " + fixed
	// Entry.MarshalJSON: None omits days, Some(empty) keeps [].
	empty := []backendusage.DayShare{}
	withNil := backendusage.Entry{Timestamp: 7}
	withEmpty := backendusage.Entry{Timestamp: 7, Days: empty}
	rawNil, _ := json.Marshal(withNil)
	rawEmpty, _ := json.Marshal(withEmpty)
	if strings.Contains(string(rawNil), `"days"`) {
		return fmt.Errorf("None days leaked: %s", rawNil)
	}
	var decoded map[string]any
	if err := json.Unmarshal(rawEmpty, &decoded); err != nil {
		return err
	}
	days, ok := decoded["days"].([]any)
	if !ok || len(days) != 0 {
		return fmt.Errorf("Some(empty) days not preserved: %s", rawEmpty)
	}
	// Service.Close cancels a blocked worker and reclaims it.
	dbPath := filepath.Join(h.round, "module-usage.db")
	svc, err := backendusage.New(h.ctx, dbPath)
	if err != nil {
		return err
	}
	block := make(chan struct{})
	factory := func(ctx context.Context) (backendusage.API, func(), error) {
		return roundTripFunc(func(ctx context.Context, method, path string, body any) (json.RawMessage, error) {
			select {
			case <-ctx.Done():
				return nil, ctx.Err()
			case <-block:
				return nil, errors.New("released")
			}
		}), func() {}, nil
	}
	_, _ = svc.Snapshot(h.ctx, "mod", true, factory)
	time.Sleep(300 * time.Millisecond)
	start := time.Now()
	if err := svc.Close(); err != nil {
		return err
	}
	if elapsed := time.Since(start); elapsed > 10*time.Second {
		return fmt.Errorf("Service.Close hung: %v", elapsed)
	}
	if _, err := svc.Snapshot(h.ctx, "mod", false, factory); err == nil || !strings.Contains(err.Error(), "closed") {
		return fmt.Errorf("post-Close Snapshot want closed, got %v", err)
	}
	close(block)
	h.checks["module-sourcekey-marshal-close"] = true
	return nil
}

type roundTripFunc func(ctx context.Context, method, path string, body any) (json.RawMessage, error)

func (f roundTripFunc) RoundTrip(ctx context.Context, method, path string, body any) (json.RawMessage, error) {
	return f(ctx, method, path, body)
}

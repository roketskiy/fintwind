package main

// Harness: real Go daemon over WS, fixture engine binary via daemon.json,
// real SQLite usage-index.db. No models, no credentials, no pricing network.

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
	_ "modernc.org/sqlite"
)

type report struct {
	DaemonSHA256 string            `json:"daemonSha256"`
	Checks       map[string]bool   `json:"checks"`
	Notes        map[string]string `json:"notes"`
	Covered      []string          `json:"covered"`
	Uncovered    []string          `json:"uncovered"`
	Passed       bool              `json:"passed"`
	Error        string            `json:"error,omitempty"`
	Command      string            `json:"command"`
}

type harness struct {
	ctx       context.Context
	round     string
	dataDir   string
	token     string
	daemonBin string
	self      string
	child     *ownedprocess.Owned
	client    *daemon.Client
	checks    map[string]bool
	notes     map[string]string
}

func run(ctx context.Context, daemonBin string) (err error) {
	if !filepath.IsAbs(daemonBin) {
		return errors.New("absolute daemon path required")
	}
	raw, err := os.ReadFile(daemonBin)
	if err != nil {
		return errors.New("daemon binary is not readable")
	}
	sum := sha256.Sum256(raw)
	out := report{DaemonSHA256: hex.EncodeToString(sum[:]), Checks: map[string]bool{}, Notes: map[string]string{}, Command: "usagecheck --daemon " + daemonBin}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	defer func() {
		out.Passed = err == nil
		if err != nil {
			out.Error = err.Error()
		}
		out.Covered = []string{
			"RPC returns stale snapshot with refreshing=true; background refresh bounded (500x50 list, 200x50 messages, 64MB budgets, 15s per-GET, 45s per-session)",
			"single-flight refresh with concurrent snapshots and force; 30s slot gate; 300s recent-fingerprint recheck via DB-checked poke",
			"listing failures (cycle/duplicate/empty-page/over-budget/50-page/missing-time) keep old snapshot, set safe refreshError, never GC",
			"single-session failure keeps old split; token-summary without messages never cached; refreshError categories never leak proxy/paths",
			"successful refresh GCs disappeared sessions by direct split-row evidence (13 -> 12, deleted row gone, retained rows kept)",
			"non-numeric cost entries succeed with nil cost; explicit 0 keeps cost 0 on the wire",
			"tree folding (fork independent, deep subagent fold, orphan retained); message lanes with tokens[5]/requests/days/hours; output-with-reasoning vs lane-stripped output; all entry models nil",
			"day/hour buckets under local timezone; multi-page/multi-session refresh; worker fan-out timing",
			"source isolation across binaries; verbatim-prefix binary shares source; single engine per binary",
			"restart reads cached snapshot immediately; Close cancels blocked workers and reclaims them",
			"NY DST clock-switch local-day buckets via temp time.Local (tzdata), real loopback HTTP + real SQLite: spring 01:59EST/03:01EDT, fall dual 01:30EDT+01:30EST, evenings prove UTC-date differs from NY-date",
			"SourceKey canonical paths share identity; distinct binary paths and all six provider storage locations separate identity; missing binary is rejected",
			"usage-index.db table shapes; tombstone-free GC only after successful refresh",
		}
		out.Uncovered = []string{
			"daemon system-zone DST: buckets follow process time.Local; daemon runs in system zone, module NY DST proven separately",
			"context.Canceled refreshError string: cancellation proven by prompt Close return, value itself code-mapped",
			"row selected-model id-preference: facts.Model is write-only in the implementation, not wire-observable",
			"engine pool exhaustion: usage holds one engine per binary by design; forcing 4 distinct binaries is out of scope",
		}
		data, marshalErr := json.MarshalIndent(out, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(round, "usage-result.json"), data, 0o600)
		}
		if marshalErr != nil {
			err = errors.Join(err, marshalErr)
		}
		fmt.Println("[REPORT]", filepath.Join(round, "usage-result.json"))
	}()
	self, err := os.Executable()
	if err != nil {
		return err
	}
	h := &harness{ctx: ctx, round: round, dataDir: filepath.Join(round, "data"), token: uuid.NewString(), daemonBin: daemonBin, self: self, checks: out.Checks, notes: out.Notes}
	if err := os.MkdirAll(h.dataDir, 0o700); err != nil {
		return err
	}
	settings, _ := json.Marshal(map[string]string{"opencode_binary_path": self})
	if err := os.WriteFile(filepath.Join(h.dataDir, "daemon.json"), settings, 0o600); err != nil {
		return err
	}
	if err := h.startDaemon(); err != nil {
		return err
	}
	defer func() { err = errors.Join(err, h.closeDaemon()) }()
	for _, suite := range []func() error{
		h.universeSuite, h.fingerprintSuite, h.recheck300Suite, h.singleFailSuite,
		h.listingFailSuite, h.gcSuite, h.budgetSuite, h.timeoutSuite, h.workerSuite, h.sourceSuite,
		h.restartSuite, h.closeSuite, h.moduleSuite, h.dstNYSuite, h.sourceIdentitySuite,
	} {
		if err := suite(); err != nil {
			return err
		}
	}
	return nil
}

func (h *harness) startDaemon() error {
	return h.startDaemonAs("first")
}

func (h *harness) startDaemonAs(name string) error {
	logs := filepath.Join(h.round, name)
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(h.daemonBin, []string{"--data-dir", h.dataDir, "--bind", "127.0.0.1:0"}, logs, h.round, ownedprocess.BuildEnv(h.round, map[string]string{"FINTWIND_DAEMON_TOKEN": h.token}))
	if err != nil {
		return err
	}
	address, err := waitDaemonReady(h.ctx, child)
	if err != nil {
		_ = child.Close()
		return err
	}
	client, err := daemon.Dial(h.ctx, address, h.token)
	if err != nil {
		_ = child.Close()
		return err
	}
	h.child, h.client = child, client
	return nil
}

func (h *harness) closeDaemon() error {
	if h.client != nil {
		h.client.Close()
		h.client = nil
	}
	if h.child != nil {
		err := h.child.Close()
		h.child = nil
		return err
	}
	return nil
}

func waitDaemonReady(ctx context.Context, child *ownedprocess.Owned) (string, error) {
	timer := time.NewTimer(15 * time.Second)
	defer timer.Stop()
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		data, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
		}
		if json.Unmarshal(data, &ready) == nil && ready.Address != "" {
			return ready.Address, nil
		}
		if done, err := child.Exited(0); err != nil || done {
			return "", errors.New("daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-timer.C:
			return "", errors.New("ready deadline exceeded")
		case <-tick.C:
		}
	}
}

func (h *harness) fetchStats(command map[string]any) (map[string]any, error) {
	raw, err := h.client.Request(h.ctx, command)
	if err != nil {
		return nil, err
	}
	response, err := backendstore.Decode(raw)
	if err != nil {
		return nil, err
	}
	if response.String("type") != "usageStats" {
		return nil, fmt.Errorf("wrong reply type: %s", response.String("type"))
	}
	var stats map[string]any
	if err := json.Unmarshal(response["stats"], &stats); err != nil {
		return nil, err
	}
	return stats, nil
}

func (h *harness) snapshot(force bool, binary string) (map[string]any, error) {
	command := map[string]any{"type": "fetchUsageStats", "forceRefresh": force}
	if binary != "" {
		command["binary"] = binary
	}
	return h.fetchStats(command)
}

func (h *harness) pollDone(force bool, binary string, timeout time.Duration) (map[string]any, error) {
	deadline := time.Now().Add(timeout)
	for {
		stats, err := h.snapshot(force, binary)
		if err != nil {
			return nil, err
		}
		if refreshing, _ := stats["refreshing"].(bool); !refreshing {
			return stats, nil
		}
		force = false
		if time.Now().After(deadline) {
			raw, _ := json.Marshal(stats)
			return nil, fmt.Errorf("refresh did not finish: %s", raw)
		}
		time.Sleep(300 * time.Millisecond)
	}
}

func (h *harness) callPath(name string) string { return filepath.Join(h.round, name) }

func (h *harness) resetCalls() { _ = os.Remove(h.callPath("usage-calls.jsonl")) }

func (h *harness) readCalls() []string {
	raw, err := os.ReadFile(h.callPath("usage-calls.jsonl"))
	if err != nil {
		return nil
	}
	lines := []string{}
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		if line != "" {
			lines = append(lines, line)
		}
	}
	return lines
}

func countContaining(calls []string, substrs ...string) int {
	total := 0
outer:
	for _, call := range calls {
		for _, sub := range substrs {
			if !strings.Contains(call, sub) {
				continue outer
			}
		}
		total++
	}
	return total
}

func (h *harness) writeSeed(seed any) error {
	raw, err := json.Marshal(seed)
	if err != nil {
		return err
	}
	return os.WriteFile(h.callPath("usage-seed.json"), raw, 0o600)
}

func failIf(err error, contains string) error {
	if err == nil {
		return errors.New("expected failure containing " + contains + " but the call succeeded")
	}
	if !strings.Contains(err.Error(), contains) {
		return fmt.Errorf("expected error containing %q, got %q", contains, err.Error())
	}
	return nil
}

func equalStrings(left, right []string) bool {
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

func (h *harness) usageDB() (*sql.DB, bool, error) {
	path := filepath.Join(h.dataDir, "usage-index.db")
	if _, err := os.Stat(path); err != nil {
		return nil, false, nil
	}
	db, err := sql.Open("sqlite", "file:"+filepath.ToSlash(path)+"?mode=ro")
	if err != nil {
		return nil, true, err
	}
	return db, true, nil
}

func (h *harness) splitPayload(source, session string) (string, bool, error) {
	db, ok, err := h.usageDB()
	if err != nil || !ok {
		return "", ok, err
	}
	defer db.Close()
	var payload string
	err = db.QueryRow(`SELECT payload FROM usage_splits_v1 WHERE source=? AND session=?`, source, session).Scan(&payload)
	if errors.Is(err, sql.ErrNoRows) {
		return "", true, nil
	}
	return payload, true, err
}

func (h *harness) splitSources() ([]string, error) {
	db, ok, err := h.usageDB()
	if err != nil {
		return nil, err
	}
	if !ok {
		return nil, nil
	}
	defer db.Close()
	rows, err := db.Query(`SELECT DISTINCT source FROM usage_splits_v1`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []string
	for rows.Next() {
		var source string
		if err := rows.Scan(&source); err != nil {
			return nil, err
		}
		out = append(out, source)
	}
	return out, rows.Err()
}

func (h *harness) tableNames() ([]string, error) {
	db, ok, err := h.usageDB()
	if err != nil {
		return nil, err
	}
	if !ok {
		return nil, errors.New("usage index db missing")
	}
	defer db.Close()
	rows, err := db.Query(`SELECT name FROM sqlite_master WHERE type='table' ORDER BY name`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []string
	for rows.Next() {
		var name string
		if err := rows.Scan(&name); err != nil {
			return nil, err
		}
		out = append(out, name)
	}
	return out, rows.Err()
}

func (h *harness) engineDirs() ([]string, error) {
	entries, err := os.ReadDir(h.round)
	if err != nil {
		return nil, err
	}
	var out []string
	for _, entry := range entries {
		if entry.IsDir() && strings.HasPrefix(entry.Name(), "engine-") {
			out = append(out, entry.Name())
		}
	}
	return out, nil
}

func entriesOf(stats map[string]any) []map[string]any {
	raw, _ := json.Marshal(stats["entries"])
	var entries []map[string]any
	_ = json.Unmarshal(raw, &entries)
	if entries == nil {
		return []map[string]any{}
	}
	return entries
}

func findEntry(entries []map[string]any, match func(map[string]any) bool) map[string]any {
	for _, entry := range entries {
		if match(entry) {
			return entry
		}
	}
	return nil
}

func f64(value any) (float64, bool) {
	number, ok := value.(float64)
	return number, ok
}

func u64(value any) uint64 {
	number, _ := value.(float64)
	if number < 0 {
		return 0
	}
	return uint64(number)
}

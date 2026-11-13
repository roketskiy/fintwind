package main

// Cross-implementation parities. Both run after the daemon wire suites and
// use no production data, no real models, no real networks beyond loopback:
//
//   - dstNYSuite proves local-day bucketing across the US DST clock switches
//     with a temporary time.Local (America/New_York via embedded tzdata), a
//     real loopback HTTP provider, and a real isolated SQLite index. Spring
//     messages straddle the missing hour (01:59 EST -> 03:01 EDT) and fall
//     messages cover both 01:30 occurrences (EDT then EST). The daemon process
//     keeps its own zone; this is module integration, not a daemon setting.
//   - sourceRustSuite proves backendusage.SourceKey is byte-identical to the
//     Rust usage_index.rs algorithm by compiling the tracked fixture
//     (fixture/sourcekey, std::fs::canonicalize + sha2 with NUL/name/value
//     framing) into the round's own target dir and executing it on the same
//     binary with fixed non-secret env values. A missing toolchain fails the
//     check honestly; Go is never compared against itself as cross-Rust.

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	_ "time/tzdata"

	"fintwind-go/internal/backendusage"
)

var parityEnvNames = []string{"XDG_DATA_HOME", "XDG_CONFIG_HOME", "HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"}

func parityEnvValues() []string {
	return []string{"parity-xdg-data", "parity-xdg-config", "parity-home", "parity-userprofile", "parity-appdata", "parity-localappdata"}
}

func (h *harness) repoRoot() string {
	dir := h.self
	for range 3 {
		dir = filepath.Dir(dir)
	}
	return dir
}

func copyFile(src, dst string) error {
	raw, err := os.ReadFile(src)
	if err != nil {
		return err
	}
	return os.WriteFile(dst, raw, 0o600)
}

func (h *harness) dstNYSuite() error {
	ny, err := time.LoadLocation("America/New_York")
	if err != nil {
		return fmt.Errorf("tzdata LoadLocation: %w", err)
	}
	// Evening pair: same NY date, different UTC dates (local-vs-UTC guard).
	e1 := time.Date(2026, 3, 8, 20, 0, 0, 0, ny).Unix()
	e2 := time.Date(2026, 3, 8, 22, 0, 0, 0, ny).Unix()
	e3 := time.Date(2026, 11, 1, 20, 0, 0, 0, ny).Unix()
	e4 := time.Date(2026, 11, 1, 22, 0, 0, 0, ny).Unix()
	// Spring transition pair: 01:59 EST exists, 02:xx does not, 03:01 EDT.
	t1 := time.Date(2026, 3, 8, 1, 59, 0, 0, ny).Unix()
	t2 := time.Date(2026, 3, 8, 3, 1, 0, 0, ny).Unix()
	// Fall transition pair: both wall-clock 01:30, first EDT then EST.
	f1 := time.Date(2026, 11, 1, 5, 30, 0, 0, time.UTC).In(ny).Unix()
	f2 := time.Date(2026, 11, 1, 6, 30, 0, 0, time.UTC).In(ny).Unix()
	// Fixture sanity: the pairs really straddle the switches.
	if got := time.Unix(t1, 0).In(ny).Format("15:04 MST"); got != "01:59 EST" {
		return fmt.Errorf("spring pre-switch fixture = %s, want 01:59 EST", got)
	}
	if got := time.Unix(t2, 0).In(ny).Format("15:04 MST"); got != "03:01 EDT" {
		return fmt.Errorf("spring post-switch fixture = %s, want 03:01 EDT", got)
	}
	if got := time.Unix(f1, 0).In(ny).Format("15:04 MST"); got != "01:30 EDT" {
		return fmt.Errorf("fall first fixture = %s, want 01:30 EDT", got)
	}
	if got := time.Unix(f2, 0).In(ny).Format("15:04 MST"); got != "01:30 EST" {
		return fmt.Errorf("fall second fixture = %s, want 01:30 EST", got)
	}
	if f2-f1 != 3600 {
		return fmt.Errorf("fall pair not one absolute hour apart: %d", f2-f1)
	}
	// Evening messages discriminate local-day from UTC-day bucketing.
	if time.Unix(e1, 0).UTC().Format("2006-01-02") == time.Unix(e1, 0).In(ny).Format("2006-01-02") {
		return errors.New("dst fixture not discriminating: e1 shares UTC and NY dates")
	}
	ordered := []int64{t1, t2, e1, e2, f1, f2, e3, e4}
	listingRow, _ := json.Marshal(map[string]any{
		"id": "ses_dst",
		"time": map[string]any{
			"created": uint64(t1) * 1000,
			"updated": uint64(e4) * 1000,
		},
		"tokens": map[string]any{
			"input": uint64(80), "output": uint64(8), "reasoning": uint64(0),
			"cache": map[string]any{"read": uint64(0), "write": uint64(0)},
		},
	})
	rows := []any{}
	for i, ts := range ordered {
		rows = append(rows, map[string]any{
			"id": fmt.Sprintf("d%d", i+1), "type": "assistant",
			"time": map[string]any{"created": uint64(ts) * 1000},
			"tokens": map[string]any{
				"input": uint64(10), "output": uint64(1), "reasoning": uint64(0),
				"cache": map[string]any{"read": uint64(0), "write": uint64(0)},
			},
			"model": map[string]any{"providerID": "p", "modelID": "a"},
		})
	}
	messageRows, _ := json.Marshal(rows)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	server := &http.Server{
		ReadHeaderTimeout: 5 * time.Second,
		Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			switch r.URL.Path {
			case "/api/session":
				_, _ = w.Write([]byte(`{"data":[` + string(listingRow) + `],"cursor":{}}`))
			case "/api/session/ses_dst/message":
				_, _ = w.Write([]byte(`{"data":` + string(messageRows) + `,"cursor":{}}`))
			default:
				http.Error(w, "unexpected operation", 400)
			}
		}),
	}
	go func() { _ = server.Serve(listener) }()
	defer func() { _ = server.Close() }()
	address := listener.Addr().String()
	plain := &http.Client{Transport: &http.Transport{Proxy: nil}}
	factory := func(context.Context) (backendusage.API, func(), error) {
		return roundTripFunc(func(ctx context.Context, method, path string, _ any) (json.RawMessage, error) {
			request, err := http.NewRequestWithContext(ctx, "GET", "http://"+address+path, nil)
			if err != nil {
				return nil, err
			}
			response, err := plain.Do(request)
			if err != nil {
				return nil, err
			}
			defer response.Body.Close()
			if response.StatusCode < 200 || response.StatusCode >= 300 {
				return nil, fmt.Errorf("dst provider HTTP %d", response.StatusCode)
			}
			var raw json.RawMessage
			if err := json.NewDecoder(response.Body).Decode(&raw); err != nil {
				return nil, err
			}
			_ = method
			return raw, nil
		}), func() {}, nil
	}
	previous := time.Local
	time.Local = ny
	defer func() { time.Local = previous }()
	dbPath := filepath.Join(h.round, "dst-ny-usage.db")
	_ = os.Remove(dbPath)
	service, err := backendusage.New(h.ctx, dbPath)
	if err != nil {
		return err
	}
	defer func() { _ = service.Close() }()
	if _, err := service.Snapshot(h.ctx, "dst-ny", true, factory); err != nil {
		return fmt.Errorf("dst snapshot start: %w", err)
	}
	deadline := time.Now().Add(60 * time.Second)
	var stats backendusage.Stats
	for {
		snapshot, err := service.Snapshot(h.ctx, "dst-ny", false, factory)
		if err != nil {
			return fmt.Errorf("dst snapshot poll: %w", err)
		}
		if !snapshot.Refreshing {
			stats = snapshot
			break
		}
		if time.Now().After(deadline) {
			return errors.New("dst refresh did not finish")
		}
		time.Sleep(200 * time.Millisecond)
	}
	if len(stats.Entries) != 1 {
		raw, _ := json.Marshal(stats.Entries)
		return fmt.Errorf("dst want 1 entry, got %d: %s", len(stats.Entries), raw)
	}
	entry := stats.Entries[0]
	if entry.Input != 80 || entry.Output != 8 {
		return fmt.Errorf("dst totals = input %d output %d, want 80/8 (nothing doubled or dropped)", entry.Input, entry.Output)
	}
	if entry.Timestamp != uint64(e4) {
		return fmt.Errorf("dst timestamp = %d, want %d", entry.Timestamp, e4)
	}
	if len(entry.Days) != 2 {
		raw, _ := json.Marshal(entry.Days)
		return fmt.Errorf("dst want 2 local-day buckets, got %d: %s", len(entry.Days), raw)
	}
	dayInputs := map[string]uint64{}
	for _, day := range entry.Days {
		date := time.Unix(int64(day.Timestamp), 0).In(ny).Format("2006-01-02")
		dayInputs[date] = day.Input
		if day.Subagent {
			return fmt.Errorf("dst day bucket wrongly marked subagent: %+v", day)
		}
	}
	if dayInputs["2026-03-08"] != 40 || dayInputs["2026-11-01"] != 40 {
		return fmt.Errorf("dst day inputs = %v, want map[2026-03-08:40 2026-11-01:40]", dayInputs)
	}
	if len(entry.Hours) != 8 {
		return fmt.Errorf("dst want 8 hour buckets, got %d", len(entry.Hours))
	}
	wantHours := map[uint64]bool{}
	for _, ts := range ordered {
		wantHours[uint64(ts)] = true
	}
	for _, hour := range entry.Hours {
		delete(wantHours, hour.Timestamp)
	}
	if len(wantHours) != 0 {
		return fmt.Errorf("dst hour timestamps mismatch, missing %v", wantHours)
	}
	if len(entry.ModelLanes) != 1 || entry.ModelLanes[0].Model != "p/a" {
		return fmt.Errorf("dst lanes = %+v", entry.ModelLanes)
	}
	lane := entry.ModelLanes[0]
	if lane.Total != 88 || lane.Tokens != [5]uint64{80, 8, 0, 0, 0} {
		return fmt.Errorf("dst lane total/tokens = %d %v, want 88 [80 8 0 0 0]", lane.Total, lane.Tokens)
	}
	if len(lane.Requests) != 8 {
		return fmt.Errorf("dst lane requests = %d, want 8", len(lane.Requests))
	}
	h.checks["dst-ny-buckets"] = true
	h.notes["dst-ny"] = "spring 01:59EST+03:01EDT, fall 01:30EDT+01:30EST, evenings e1-e4 -> 2 NY days x40, 8 UTC hours, total 80/8"
	return nil
}

func (h *harness) sourceRustSuite() error {
	values := parityEnvValues()
	environment := map[string]string{}
	for i, name := range parityEnvNames {
		environment[name] = values[i]
	}
	goKey, err := backendusage.SourceKey(h.self, environment)
	if err != nil {
		return fmt.Errorf("go source key: %w", err)
	}
	// The fixture is tracked source; compile it into the round's own target
	// dir so no Cargo.lock or target/ output ever pollutes the source tree
	// and no global cargo profile or env is changed.
	fixture := filepath.Join(h.repoRoot(), "fintwind-go", "cmd", "usagecheck", "fixture", "sourcekey")
	buildDir := filepath.Join(h.round, "sourcekey-build")
	if err := os.MkdirAll(filepath.Join(buildDir, "src"), 0o700); err != nil {
		return err
	}
	if err := copyFile(filepath.Join(fixture, "Cargo.toml"), filepath.Join(buildDir, "Cargo.toml")); err != nil {
		return fmt.Errorf("stage fixture manifest: %w", err)
	}
	if err := copyFile(filepath.Join(fixture, "src", "main.rs"), filepath.Join(buildDir, "src", "main.rs")); err != nil {
		return fmt.Errorf("stage fixture source: %w", err)
	}
	harness := filepath.Join(buildDir, "target", "debug", "sourcekey.exe")
	buildCtx, stop := context.WithTimeout(h.ctx, 300*time.Second)
	defer stop()
	build := exec.CommandContext(buildCtx, "cargo", "build", "--offline",
		"--manifest-path", filepath.Join(buildDir, "Cargo.toml"),
		"--target-dir", filepath.Join(buildDir, "target"))
	build.Dir = buildDir
	if output, buildErr := build.CombinedOutput(); buildErr != nil {
		if errors.Is(buildErr, exec.ErrNotFound) {
			return fmt.Errorf("rust toolchain missing, parity not proven (no Go fallback used): %w", buildErr)
		}
		return fmt.Errorf("rust harness build: %v: %s", buildErr, output)
	}
	run := func(binary string, vals []string) (string, error) {
		args := append([]string{binary}, vals...)
		command := exec.CommandContext(h.ctx, harness, args...)
		output, err := command.Output()
		if err != nil {
			return "", err
		}
		return strings.TrimSpace(string(output)), nil
	}
	rustKey, err := run(h.self, values)
	if err != nil {
		return fmt.Errorf("rust harness run: %w", err)
	}
	if rustKey != goKey {
		return fmt.Errorf("sourcekey mismatch:\n rust %s\n go   %s", rustKey, goKey)
	}
	verbatimRust, err := run(`\\?\`+h.self, values)
	if err != nil {
		return fmt.Errorf("rust verbatim run: %w", err)
	}
	if verbatimRust != goKey {
		return fmt.Errorf("verbatim source split:\n plain %s\n go    %s", verbatimRust, goKey)
	}
	changed := append([]string{}, values...)
	changed[2] += "-other"
	otherRust, err := run(h.self, changed)
	if err != nil {
		return fmt.Errorf("rust changed-env run: %w", err)
	}
	if otherRust == goKey {
		return errors.New("rust harness ignored the environment")
	}
	otherEnv := map[string]string{}
	for i, name := range parityEnvNames {
		otherEnv[name] = changed[i]
	}
	otherGo, err := backendusage.SourceKey(h.self, otherEnv)
	if err != nil {
		return err
	}
	if otherGo != otherRust {
		return fmt.Errorf("changed-env mismatch:\n rust %s\n go   %s", otherRust, otherGo)
	}
	h.checks["sourcekey-rust-parity"] = true
	h.notes["sourcekey-rust"] = "rust " + rustKey + " == go " + goKey + " (tracked fixture, round target dir, verbatim shared, env-sensitive both sides)"
	return nil
}

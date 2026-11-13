package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/backendquota"
	"fintwind-go/internal/ownedprocess"
)

type moduleFixture struct {
	home, config string
	local        *backendquota.Local
}

func (h *harness) newModuleFixture(name string, value seed) (*moduleFixture, error) {
	home := filepath.Join(h.round, "module", name, "home")
	config := filepath.Join(home, ".claude")
	for _, dir := range []string{config, filepath.Join(home, "AppData", "Roaming"), filepath.Join(home, "AppData", "Local"), filepath.Join(h.round, "module", name, "tmp")} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return nil, err
		}
	}
	f := &moduleFixture{home: home, config: config, local: backendquota.NewLocal(home, nil)}
	if err := f.writeSeed(value); err != nil {
		return nil, err
	}
	if err := os.WriteFile(filepath.Join(config, ".credentials.json"), []byte(`{"round":"fake-credential-A"}`), 0o600); err != nil {
		return nil, err
	}
	return f, nil
}

func (f *moduleFixture) writeSeed(value seed) error {
	raw, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(f.config, "e2e-seed.json"), raw, 0o600)
}
func (f *moduleFixture) fetch(ctx context.Context, force bool) backendquota.Quota {
	return f.local.Fetch(ctx, "anthropic", force)
}

func (h *harness) moduleSuite() error {
	oldPath, hadPath := os.LookupEnv("PATH")
	if err := os.Setenv("PATH", h.bin); err != nil {
		return err
	}
	defer func() {
		if hadPath {
			_ = os.Setenv("PATH", oldPath)
		} else {
			_ = os.Unsetenv("PATH")
		}
	}()
	oldSecret, hadSecret := os.LookupEnv("ANTHROPIC_API_KEY")
	_ = os.Setenv("ANTHROPIC_API_KEY", "ROUND-ONLY-FAKE-SENTINEL")
	defer func() {
		if hadSecret {
			_ = os.Setenv("ANTHROPIC_API_KEY", oldSecret)
		} else {
			_ = os.Unsetenv("ANTHROPIC_API_KEY")
		}
	}()
	for _, tc := range []struct {
		name    string
		fixture seed
		want    string
	}{
		{"no-login", seed{Status: "false", Panel: "quota"}, "no_credential"},
		{"malformed-status", seed{Status: "malformed"}, "unavailable"},
		{"failed-status", seed{Status: "fail"}, "unavailable"},
		{"status-budget", seed{Status: "flood"}, "unavailable"},
	} {
		f, err := h.newModuleFixture(tc.name, tc.fixture)
		if err != nil {
			return err
		}
		q := f.fetch(h.ctx, false)
		if q.Status != tc.want {
			return fmt.Errorf("module %s status=%q want %q", tc.name, q.Status, tc.want)
		}
		if tc.name == "failed-status" && strings.Contains(q.Status, "private-fake") {
			return errors.New("status output leaked fake private text")
		}
	}
	h.checks["module-auth-status-failures"] = true
	var suiteErr error
	if err := h.moduleCancellationSuite(); err != nil {
		h.checks["module-cancel-job-descendant"] = false
		suiteErr = err
	}
	if err := h.shimSafetySuite(); err != nil {
		h.checks["module-cmd-shim-argument-safety"] = false
		suiteErr = err
	}

	good, err := h.newModuleFixture("cache", seed{Status: "valid", Panel: "quota"})
	if err != nil {
		return err
	}
	first := good.fetch(h.ctx, false)
	if err := assertReadyQuota(first); err != nil {
		calls, _ := readCalls(good.config)
		h.checks["module-real-pty-panel-quota"] = false
		return errors.Join(suiteErr, fmt.Errorf("module ready quota: %w; %s", err, consoleEvidence(calls)))
	}
	calls, err := readCalls(good.config)
	if err != nil {
		return err
	}
	if len(calls) != 3 {
		return fmt.Errorf("initial module fetch spawned %d commands, want initial status+panel+identity verification", len(calls))
	}
	var wg sync.WaitGroup
	results := make(chan backendquota.Quota, 8)
	for range 8 {
		wg.Add(1)
		go func() { defer wg.Done(); results <- good.fetch(h.ctx, false) }()
	}
	wg.Wait()
	close(results)
	for q := range results {
		if err := assertReadyQuota(q); err != nil {
			return fmt.Errorf("cached concurrent reading: %w", err)
		}
	}
	if q := good.fetch(h.ctx, true); q.Status != "ready" {
		return fmt.Errorf("force call inside cooldown returned %q", q.Status)
	}
	calls, err = readCalls(good.config)
	if err != nil {
		return err
	}
	if len(calls) != 3 {
		return fmt.Errorf("cache/concurrency/force made %d child calls, want 3", len(calls))
	}
	h.checks["module-cache-singleflight-force-cooldown"] = true

	for _, tc := range []struct{ name, panel, want string }{{"denied", "denied", "needs_auth"}, {"cost-only", "cost-only", "unavailable"}, {"unknown", "unknown", "unavailable"}} {
		f, e := h.newModuleFixture(tc.name, seed{Status: "valid", Panel: tc.panel})
		if e != nil {
			return e
		}
		q := f.fetch(h.ctx, false)
		if q.Status != tc.want || len(q.Windows) != 0 {
			return fmt.Errorf("module %s produced status=%s windows=%d", tc.name, q.Status, len(q.Windows))
		}
	}
	h.checks["module-denied-cost-unknown"] = true

	rot, err := h.newModuleFixture("rotate-mid-query", seed{Status: "valid", Panel: "quota", Rotate: true})
	if err != nil {
		return err
	}
	q := rot.fetch(h.ctx, false)
	if q.Status != "unavailable" || len(q.Windows) != 0 {
		return fmt.Errorf("mid-query credential rotation returned status=%s windows=%d", q.Status, len(q.Windows))
	}
	identity, err := h.newModuleFixture("rotate-between-fetches", seed{Status: "valid", Panel: "quota"})
	if err != nil {
		return err
	}
	if q := identity.fetch(h.ctx, false); q.Status != "ready" {
		return fmt.Errorf("initial identity status=%s", q.Status)
	}
	if err := os.WriteFile(filepath.Join(identity.config, ".credentials.json"), []byte(`{"round":"fake-credential-B"}`), 0o600); err != nil {
		return err
	}
	if err := identity.writeSeed(seed{Status: "valid", Panel: "unknown"}); err != nil {
		return err
	}
	q = identity.fetch(h.ctx, true)
	if q.Status != "unavailable" || len(q.Windows) != 0 {
		return fmt.Errorf("new fingerprint reused old data: status=%s windows=%d", q.Status, len(q.Windows))
	}
	h.checks["module-fingerprint-rotation"] = true

	return suiteErr
}

func (h *harness) moduleCancellationSuite() error {
	cancelled, err := h.newModuleFixture("cancel-status", seed{Status: "hang"})
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(h.ctx, 250*time.Millisecond)
	started := time.Now()
	q := cancelled.fetch(ctx, false)
	elapsed := time.Since(started)
	cancel()
	if q.Status != "unavailable" || elapsed > 8*time.Second {
		return fmt.Errorf("module status cancellation status=%s elapsed=%v", q.Status, elapsed)
	}
	heartbeat := filepath.Join(cancelled.config, "child-heartbeat.txt")
	if err := waitFile(heartbeat, 5*time.Second); err != nil {
		return fmt.Errorf("owned grandchild not observed: %w", err)
	}
	before, _ := os.ReadFile(heartbeat)
	time.Sleep(350 * time.Millisecond)
	after, _ := os.ReadFile(heartbeat)
	if string(before) != string(after) {
		return errors.New("owned Job descendant continued after cancellation")
	}
	h.checks["module-cancel-job-descendant"] = true
	h.notes["module-cancel"] = "Direct Local.Fetch: 250ms context cancellation; owned fake descendant heartbeat stopped"
	return nil
}

func consoleEvidence(calls []invocation) string {
	var out []string
	for _, call := range calls {
		if len(call.Args) > 0 && call.Args[0] == "--setting-sources" {
			out = append(out, fmt.Sprintf("panel GetConsoleMode(stdin=%v,type=%d; stdout=%v,type=%d; %s)", call.ConsoleIn, call.StdinType, call.ConsoleOut, call.StdoutType, call.ConsoleError))
		}
	}
	return strings.Join(out, "; ")
}

func (h *harness) shimSafetySuite() error {
	args := []string{"", "plain", "two words", "A&B", "100%literal", `quote"value`, "safe & echo INJECTED > \"" + filepath.Join(h.round, "shim", "injected-command.marker") + "\" & rem"}
	fixtures := []struct {
		name            string
		script          string
		exactArgs       bool
		batchMarker     string
		wantBatchMarker bool
	}{
		{"percent-1-through-7", fmt.Sprintf("@echo off\r\nsetlocal DisableDelayedExpansion\r\n\"%%~dp0claude.exe\" shim-record %%1 %%2 %%3 %%4 %%5 %%6 %%7\r\n"), true, "", false},
		{"percent-star", "@echo off\r\nsetlocal DisableDelayedExpansion\r\n\"%~dp0claude.exe\" shim-record %*\r\n", true, "", false},
		{"numeric-extra-statement", "", false, filepath.Join(h.round, "shim", "numeric-extra-statement", "batch-branch.marker"), true},
	}
	fixtures[2].script = "@echo off\r\nsetlocal DisableDelayedExpansion\r\n>\"" + fixtures[2].batchMarker + "\" echo batch-branch\r\n\"%~dp0claude.exe\" shim-record %1 %2 %3 %4 %5 %6 %7\r\n"
	var failures []error
	for _, fixture := range fixtures {
		if err := h.runShimFixture(fixture.name, fixture.script, args, fixture.exactArgs, fixture.batchMarker, fixture.wantBatchMarker); err != nil {
			h.checks["module-cmd-shim-"+fixture.name] = false
			failures = append(failures, fmt.Errorf("%s: %w", fixture.name, err))
		} else {
			h.checks["module-cmd-shim-"+fixture.name] = true
		}
	}
	return errors.Join(failures...)
}

func (h *harness) runShimFixture(name, script string, args []string, exactArgs bool, batchMarker string, wantBatchMarker bool) error {
	root := filepath.Join(h.round, "shim", name)
	config := filepath.Join(root, ".claude")
	if err := os.MkdirAll(config, 0o700); err != nil {
		return err
	}
	scriptPath := filepath.Join(h.bin, "claude-shim.cmd")
	if err := os.WriteFile(scriptPath, []byte(script), 0o600); err != nil {
		return err
	}
	env := ownedprocess.BuildEnv(root, map[string]string{"PATH": h.bin, "CLAUDE_CONFIG_DIR": config, "HOME": root, "USERPROFILE": root, "TEMP": filepath.Join(root, "tmp"), "TMP": filepath.Join(root, "tmp")})
	if err := os.MkdirAll(filepath.Join(root, "tmp"), 0o700); err != nil {
		return err
	}
	stdout, stdoutWriter, err := os.Pipe()
	if err != nil {
		return err
	}
	stderr, stderrWriter, err := os.Pipe()
	if err != nil {
		stdout.Close()
		stdoutWriter.Close()
		return err
	}
	owned, err := ownedprocess.StartWithOutput(scriptPath, args, root, root, env, stdoutWriter, stderrWriter)
	if err != nil {
		stdout.Close()
		stdoutWriter.Close()
		stderr.Close()
		stderrWriter.Close()
		return fmt.Errorf("start owned cmd shim: %w", err)
	}
	_ = stdoutWriter.Close()
	_ = stderrWriter.Close()
	outputDone := make(chan struct{}, 2)
	go func() { _, _ = io.Copy(io.Discard, stdout); _ = stdout.Close(); outputDone <- struct{}{} }()
	go func() { _, _ = io.Copy(io.Discard, stderr); _ = stderr.Close(); outputDone <- struct{}{} }()
	deadline := time.Now().Add(10 * time.Second)
	for {
		exited, e := owned.Exited(50)
		if e != nil {
			_ = owned.Close()
			return e
		}
		if exited {
			break
		}
		if time.Now().After(deadline) {
			_ = owned.Close()
			return errors.New("cmd shim did not exit within 10s")
		}
		time.Sleep(25 * time.Millisecond)
	}
	code, err := owned.ExitCode()
	if err != nil {
		return err
	}
	if code != 0 {
		return fmt.Errorf("cmd shim exit code %d", code)
	}
	if err := owned.Close(); err != nil {
		return fmt.Errorf("close cmd shim PTY: %w", err)
	}
	<-outputDone
	<-outputDone
	if _, err := os.Stat(filepath.Join(h.round, "shim", "injected-command.marker")); err == nil {
		return errors.New("cmd shim argument injected a shell command")
	}
	if batchMarker != "" {
		_, err := os.Stat(batchMarker)
		if wantBatchMarker && err != nil {
			return fmt.Errorf("nontransparent batch branch marker was not created: %w", err)
		}
		if !wantBatchMarker && err == nil {
			return errors.New("unexpected nontransparent batch branch marker")
		}
		if err != nil && !os.IsNotExist(err) {
			return fmt.Errorf("stat nontransparent batch marker: %w", err)
		}
	}
	calls, err := readCalls(config)
	if err != nil {
		return fmt.Errorf("cmd shim did not forward to round fake: %w", err)
	}
	if len(calls) != 1 {
		return fmt.Errorf("cmd shim invocation count %d, want 1", len(calls))
	}
	if exactArgs && !reflect.DeepEqual(calls[0].Args, args) {
		if len(calls) == 1 {
			return fmt.Errorf("cmd shim argv mismatch: got %#v want %#v", calls[0].Args, args)
		}
	}
	return nil
}

func readCalls(config string) ([]invocation, error) {
	raw, err := os.ReadFile(filepath.Join(config, "e2e-calls.jsonl"))
	if err != nil {
		return nil, err
	}
	var calls []invocation
	for _, line := range strings.Split(string(raw), "\n") {
		if strings.TrimSpace(line) == "" {
			continue
		}
		var v invocation
		if err := json.Unmarshal([]byte(line), &v); err != nil {
			return nil, err
		}
		calls = append(calls, v)
	}
	return calls, nil
}

func waitFile(path string, limit time.Duration) error {
	deadline := time.Now().Add(limit)
	for {
		if _, err := os.Stat(path); err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			return errors.New("deadline exceeded")
		}
		time.Sleep(30 * time.Millisecond)
	}
}

func assertReadyQuota(q backendquota.Quota) error {
	if q.Status != "ready" {
		return fmt.Errorf("status %q", q.Status)
	}
	if q.Account == nil || *q.Account != "fixture@example.invalid" || q.Plan == nil || *q.Plan != "max" {
		return errors.New("account/plan metadata mismatch")
	}
	if len(q.Windows) != 4 {
		return fmt.Errorf("want session, weekly, model-weekly, overages; got %d windows", len(q.Windows))
	}
	values := map[string]float64{}
	for _, w := range q.Windows {
		if w.UsedPercent != nil {
			values[w.ID] = *w.UsedPercent
		}
	}
	if values["session"] != 42 || values["weekly"] != 62 || values["week:sonnet"] != 25 || values["extra-usage"] != 100 {
		return fmt.Errorf("unexpected window values: %#v", values)
	}
	for _, w := range q.Windows {
		if w.ID != "extra-usage" && w.ResetsAt == nil {
			return fmt.Errorf("known reset did not parse for %s", w.ID)
		}
	}
	return nil
}

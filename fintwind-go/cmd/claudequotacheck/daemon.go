package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"time"

	"fintwind-go/internal/backendquota"
	"fintwind-go/internal/daemon"
	"github.com/google/uuid"
)

func (h *harness) daemonSuite() error {
	if err := h.writeCredential(`{"fixture":"daemon-identity-1"}`); err != nil {
		return err
	}
	if err := h.writeSeed(seed{Status: "valid", Panel: "quota"}); err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(h.ctx, 90*time.Second)
	defer cancel()
	q, err := h.quota(ctx, false)
	if err != nil {
		return fmt.Errorf("daemon fetchProviderQuota: %w", err)
	}
	if err := h.checkCallsAndEnvironment(); err != nil {
		h.checks["daemon-real-rpc-conpty-quota"] = false
		return fmt.Errorf("daemon fetchProviderQuota returned %q; %w", q.Status, err)
	}
	if err := assertReadyQuota(q); err != nil {
		h.checks["daemon-real-rpc-conpty-quota"] = false
		return fmt.Errorf("daemon quota payload: %w", err)
	}
	h.checks["daemon-real-rpc-conpty-quota"] = true

	callsBefore, err := h.calls()
	if err != nil {
		return err
	}
	if q, err = h.quota(ctx, false); err != nil || q.Status != "ready" {
		return fmt.Errorf("daemon cached quota: status=%s err=%v", q.Status, err)
	}
	if q, err = h.quota(ctx, true); err != nil || q.Status != "ready" {
		return fmt.Errorf("daemon forced quota in cooldown: status=%s err=%v", q.Status, err)
	}
	callsAfter, err := h.calls()
	if err != nil {
		return err
	}
	if len(callsAfter) != len(callsBefore) {
		return fmt.Errorf("daemon cache/force spawned extra CLI: before=%d after=%d", len(callsBefore), len(callsAfter))
	}
	h.checks["daemon-cache-force-cooldown"] = true

	for index, tc := range []struct {
		name    string
		fixture seed
		want    string
	}{
		{"no-login", seed{Status: "false", Panel: "quota"}, "no_credential"},
		{"malformed", seed{Status: "malformed", Panel: "quota"}, "unavailable"},
		{"denied", seed{Status: "valid", Panel: "denied"}, "needs_auth"},
		{"cost-only", seed{Status: "valid", Panel: "cost-only"}, "unavailable"},
		{"unknown", seed{Status: "valid", Panel: "unknown"}, "unavailable"},
		{"unknown-reset", seed{Status: "valid", Panel: "unknown-reset"}, "ready"},
		{"startup-fail", seed{Status: "valid", Panel: "startup-fail"}, "unavailable"},
		{"overflow", seed{Status: "valid", Panel: "quota", OverBytes: 2 << 20}, "unavailable"},
		{"rotate-mid-query", seed{Status: "valid", Panel: "quota", Rotate: true}, "unavailable"},
	} {
		if err := h.writeCredential(fmt.Sprintf(`{"fixture":"daemon-case-%d"}`, index+2)); err != nil {
			return err
		}
		if err := h.writeSeed(tc.fixture); err != nil {
			return err
		}
		q, err := h.quota(ctx, true)
		if err != nil {
			return fmt.Errorf("daemon %s RPC: %w", tc.name, err)
		}
		if q.Status != tc.want {
			return fmt.Errorf("daemon %s status=%q want %q", tc.name, q.Status, tc.want)
		}
		if tc.name == "unknown-reset" {
			if err := assertUnknownResetQuota(q); err != nil {
				return fmt.Errorf("daemon unknown-reset RPC: %w", err)
			}
			h.checks["daemon-valid-quota-unknown-reset-unset"] = true
		}
		if tc.want != "ready" && len(q.Windows) != 0 {
			return fmt.Errorf("daemon %s returned stale quota windows", tc.name)
		}
	}
	h.checks["daemon-auth-denial-unknown-overflow-rotation"] = true

	if err := h.shutdownCancellationSuite(); err != nil {
		return err
	}
	return nil
}

func assertUnknownResetQuota(q backendquota.Quota) error {
	if q.Status != "ready" || q.Account == nil || *q.Account != "fixture@example.invalid" || q.Plan == nil || *q.Plan != "max" {
		return fmt.Errorf("ready metadata mismatch: status=%q account=%v plan=%v", q.Status, q.Account, q.Plan)
	}
	if len(q.Windows) != 4 {
		return fmt.Errorf("want four valid quota windows, got %d", len(q.Windows))
	}
	type expectedWindow struct {
		kind  string
		used  float64
		model string
		reset string
	}
	want := map[string]expectedWindow{
		"session":     {kind: "rolling", used: 42, reset: "unset"},
		"weekly":      {kind: "weekly", used: 62, reset: "set"},
		"week:sonnet": {kind: "weekly", used: 25, model: "Sonnet", reset: "set"},
		"extra-usage": {kind: "extraUsage", used: 100, reset: "unset"},
	}
	for _, window := range q.Windows {
		expect, ok := want[window.ID]
		if !ok {
			return fmt.Errorf("unexpected quota window %q", window.ID)
		}
		delete(want, window.ID)
		if window.Kind != expect.kind || window.UsedPercent == nil || *window.UsedPercent != expect.used {
			return fmt.Errorf("window %s kind/used mismatch: kind=%q used=%v", window.ID, window.Kind, window.UsedPercent)
		}
		if expect.model == "" {
			if window.Model != nil {
				return fmt.Errorf("window %s unexpectedly has model %q", window.ID, *window.Model)
			}
		} else if window.Model == nil || *window.Model != expect.model {
			return fmt.Errorf("window %s model=%v want %q", window.ID, window.Model, expect.model)
		}
		switch expect.reset {
		case "unset":
			if window.ResetsAt != nil {
				return fmt.Errorf("window %s unknown reset unexpectedly parsed as %d", window.ID, *window.ResetsAt)
			}
		case "set":
			if window.ResetsAt == nil {
				return fmt.Errorf("window %s known reset was not parsed", window.ID)
			}
		}
	}
	if len(want) != 0 {
		return fmt.Errorf("missing quota windows: %v", want)
	}
	return nil
}

func (h *harness) checkCallsAndEnvironment() error {
	calls, err := h.calls()
	if err != nil {
		return err
	}
	var statusCall, panelCall *invocation
	for i := range calls {
		c := &calls[i]
		if reflect.DeepEqual(c.Args, []string{"auth", "status", "--json"}) {
			statusCall = c
		}
		if len(c.Args) > 0 && c.Args[0] == "--setting-sources" {
			panelCall = c
		}
	}
	if statusCall == nil || panelCall == nil {
		return errors.New("fake CLI did not record both auth status and ConPTY panel commands")
	}
	if statusCall.ConsoleOut {
		return errors.New("auth status stdout unexpectedly attached to a console")
	}
	wantArgs := []string{"--setting-sources", "", "--settings", `{"disableDeepLinkRegistration":"disable"}`, "--strict-mcp-config", "--tools", "", "--permission-mode", "default", "--allowedTools", "", "--no-chrome", "--session-id"}
	if len(panelCall.Args) != len(wantArgs)+1 || !reflect.DeepEqual(panelCall.Args[:len(wantArgs)], wantArgs) {
		return fmt.Errorf("panel fixed argv mismatch: %q", panelCall.Args)
	}
	if _, err := uuid.Parse(panelCall.Args[len(wantArgs)]); err != nil {
		return fmt.Errorf("panel session id is not a UUID: %q", panelCall.Args[len(wantArgs)])
	}
	if panelCall.Cwd != h.workspace {
		return fmt.Errorf("panel cwd %q differs from owned workspace %q", panelCall.Cwd, h.workspace)
	}
	wantEnv := map[string]string{
		"HOME": h.home, "USERPROFILE": h.home, "CLAUDE_CONFIG_DIR": h.configDir(),
		"APPDATA": filepath.Join(h.home, "AppData", "Roaming"), "LOCALAPPDATA": filepath.Join(h.home, "AppData", "Local"),
		"TEMP": filepath.Join(h.round, "tmp"), "TMP": filepath.Join(h.round, "tmp"), "PATH": h.bin,
	}
	for key, want := range wantEnv {
		if panelCall.Env[key] != want {
			return fmt.Errorf("isolated child env %s=%q want %q", key, panelCall.Env[key], want)
		}
	}
	for key := range panelCall.Env {
		if strings.HasPrefix(key, "ANTHROPIC_PRESENT:") {
			return fmt.Errorf("Anthropic secret env key crossed boundary: %s", key)
		}
	}
	for _, key := range []string{"SystemRoot", "WINDIR", "COMSPEC"} {
		if panelCall.Env[key] == "" {
			return fmt.Errorf("required Windows base environment %s absent", key)
		}
	}
	h.checks["daemon-isolated-env"] = true
	if !panelCall.ConsoleIn || !panelCall.ConsoleOut {
		return fmt.Errorf("panel GetConsoleMode failed (in=%v type=%d; out=%v type=%d; %s)", panelCall.ConsoleIn, panelCall.StdinType, panelCall.ConsoleOut, panelCall.StdoutType, panelCall.ConsoleError)
	}
	if !strings.Contains(panelCall.Input, "/usage\r") {
		return fmt.Errorf("fake did not receive /usage command: %q", panelCall.Input)
	}
	if !strings.Contains(panelCall.Input, "\x1b") || strings.Count(panelCall.Input, "\r") < 4 {
		return errors.New("expected onboarding key inputs were not sent")
	}
	if !strings.Contains(panelCall.Input, "\x1b[B\r") {
		return errors.New("trust prompt did not move default No to the owned-workspace Yes choice")
	}
	h.checks["daemon-exact-argv-env-console-input"] = true
	return nil
}

func (h *harness) shutdownCancellationSuite() error {
	if err := h.writeCredential(`{"fixture":"daemon-hanging-status"}`); err != nil {
		return err
	}
	if err := h.writeSeed(seed{Status: "hang", Panel: "quota"}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 300*time.Second)
	defer stop()
	done := make(chan error, 1)
	go func() { _, err := h.quota(ctx, false); done <- err }()
	if err := waitFile(filepath.Join(h.configDir(), "holder-started.marker"), 15*time.Second); err != nil {
		return fmt.Errorf("daemon status child did not start owned descendant: %w", err)
	}
	if err := waitFile(filepath.Join(h.configDir(), "child-heartbeat.txt"), 5*time.Second); err != nil {
		return err
	}
	before, _ := os.ReadFile(filepath.Join(h.configDir(), "child-heartbeat.txt"))
	shutdown, err := daemon.Dial(h.ctx, h.address, h.token)
	if err != nil {
		return err
	}
	sendCtx, sendCancel := context.WithTimeout(h.ctx, 5*time.Second)
	err = shutdown.Send(sendCtx, map[string]any{"type": "shutdown"})
	sendCancel()
	if err != nil {
		shutdown.Close()
		return fmt.Errorf("daemon shutdown frame: %w", err)
	}
	owned := h.daemon
	deadline := time.Now().Add(20 * time.Second)
	for {
		exited, e := owned.Exited(100)
		if e != nil {
			shutdown.Close()
			return e
		}
		if exited {
			break
		}
		if time.Now().After(deadline) {
			shutdown.Close()
			return errors.New("daemon did not exit after shutdown cancellation within 20s")
		}
		time.Sleep(40 * time.Millisecond)
	}
	code, err := owned.ExitCode()
	if err != nil {
		shutdown.Close()
		return err
	}
	if code != 0 {
		shutdown.Close()
		return fmt.Errorf("daemon shutdown exit code=%d want 0", code)
	}
	flowErr, err := waitFlow(done, 8*time.Second)
	if err != nil {
		shutdown.Close()
		return err
	}
	if flowErr == nil {
		shutdown.Close()
		return errors.New("hanging auth-status RPC unexpectedly succeeded after shutdown")
	}
	// Observe the heartbeat stop while only Exited/ExitCode have been queried.
	// Do not call Owned.Close until after this assertion: it is cleanup only.
	time.Sleep(350 * time.Millisecond)
	after, _ := os.ReadFile(filepath.Join(h.configDir(), "child-heartbeat.txt"))
	if string(before) == "" || string(before) != string(after) {
		shutdown.Close()
		return fmt.Errorf("owned descendant was not stopped by graceful daemon shutdown (before=%q after=%q)", before, after)
	}
	h.checks["daemon-shutdown-cancels-owned-tree"] = true
	shutdown.Close()
	h.client.Close()
	h.client = nil
	if err := owned.Close(); err != nil {
		return err
	}
	h.daemon = nil
	if err := h.startDaemon(); err != nil {
		return fmt.Errorf("daemon restart after cancellation: %w", err)
	}
	if err := h.writeCredential(`{"fixture":"daemon-after-restart"}`); err != nil {
		return err
	}
	if err := h.writeSeed(seed{Status: "false", Panel: "quota"}); err != nil {
		return err
	}
	ctx2, cancel2 := context.WithTimeout(h.ctx, 90*time.Second)
	defer cancel2()
	q, err := h.quota(ctx2, false)
	if err != nil {
		return err
	}
	if q.Status != "no_credential" {
		return fmt.Errorf("post-cancel restarted daemon status=%q want no_credential", q.Status)
	}
	h.checks["daemon-restart-after-shutdown"] = true
	h.checks["daemon-shutdown-cancels-owned-tree-restarts"] = true
	h.notes["shutdown-cancel"] = "separate WS shutdown frame; auth RPC socket kept open; daemon handle exited code 0 before Owned.Close; fake descendant heartbeat stopped; restarted same data dir"
	return nil
}

func waitFlow(done <-chan error, limit time.Duration) (error, error) {
	select {
	case err := <-done:
		return err, nil
	case <-time.After(limit):
		return nil, errors.New("quota RPC did not resolve after daemon shutdown")
	}
}

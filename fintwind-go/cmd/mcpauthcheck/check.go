package main

// Harness: real Go daemon over WS, this same executable as the fake OpenCode
// CLI via daemon.json. No real OAuth, no browser, no credentials, no models.

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
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
	workspace string
	token     string
	daemonBin string
	self      string
	child     *ownedprocess.Owned
	client    *daemon.Client
	addr      string
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
	out := report{DaemonSHA256: hex.EncodeToString(sum[:]), Checks: map[string]bool{}, Notes: map[string]string{}, Command: "mcpauthcheck --daemon " + daemonBin}
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
			"daemon WS: success/empty-output ack; argv mcp auth name recorded; fixture never prints https",
			"daemon WS: name validation (empty/over-256/option-prefix/control/outside-dir/missing-binary) rejects before spawn",
			"daemon WS: nonzero exit maps to fixed safe error with no CLI text leak",
			"daemon WS: same-name concurrency single flow; unknown cancel ack idempotent; cancel-then-reauth generation safe",
			"daemon WS: short-lived silent CLI succeeds; output-budget failure is fixed and safe",
			"daemon WS: grandchild pipe holder reclaimed by Owned Job (no PID scan); concurrent flows do not inherit each other's write ends",
			"daemon WS: 16 auth-worker capacity; ordinary workers unblocked (probe ok mid-storm); second connection works; management busy while auth holds read gate",
			"daemon WS: restart clears pending; graceful shutdown frame cancels/waits an in-flight auth + holder, daemon exits code 0 before handle cleanup; forced Job-close case retained; task settings unchanged",
			"daemon WS: child env isolation (OPENCODE_DB/CONFIG_DIR/TEST_HOME pinned under round, no secrets/proxy leak)",
			"module: first https opens once; ANSI CSI/OSC8 sanitized; userinfo/non-https skipped; trailing line without newline read; >64K line dropped but later URL opens; 1MB stream budget kills job and next flow works; opener failure fixed error without URL; cross-stream once; cancel/100ms-ctx/Close reclaim promptly; old generation never drops new flow",
			"module: product has no fake/test backdoor (no marker reads; OPENCODE_TEST_HOME is set-only plumbing)",
			"module: Owned StartWithOutput precise handle list verified by cross-cancel EOF behavior; reclamation by handle close only",
		}
		out.Uncovered = []string{
			"auth rejected while management write lock held: only the reverse (busy on update) is staged; rejection is structural (dispatch TryRLock)",
			"360s CLI budget and 400s WS worker budget: structural constants; cancellation proven via 100ms caller ctx, never waited out",
			"2MB total stream budget: unreachable without tripping the 1MB per-stream guard first; per-stream budget proven, total is structural",
			"ordinary terminal semantics and ptys: out of scope for this migration",
			"real browser launch and real OAuth: forbidden in E2E by design",
		}
		data, marshalErr := json.MarshalIndent(out, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(round, "mcp-auth-result.json"), data, 0o600)
		}
		if marshalErr != nil {
			err = errors.Join(err, marshalErr)
		}
		fmt.Println("[REPORT]", filepath.Join(round, "mcp-auth-result.json"))
	}()
	self, err := os.Executable()
	if err != nil {
		return err
	}
	h := &harness{ctx: ctx, round: round, dataDir: filepath.Join(round, "data"), token: uuid.NewString(), daemonBin: daemonBin, self: self, checks: out.Checks, notes: out.Notes}
	h.workspace = filepath.Join(filepath.Dir(h.dataDir), "workspace")
	if err := os.MkdirAll(h.workspace, 0o700); err != nil {
		return err
	}
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
		h.okSuite, h.validationSuite, h.failSuite, h.concurrentSuite,
		h.budgetDaemonSuite, h.holderSuite, h.crossHandleSuite, h.capacitySuite,
		h.envSuite, h.shortSuite, h.restartSuite, h.closeSuite, h.gracefulCloseSuite, h.configSuite,
		h.moduleFirstURLSuite, h.moduleOnceSuite, h.moduleSanitizeSuite, h.moduleSkipSuite,
		h.moduleTrailingSuite, h.moduleGiantSuite, h.moduleFloodSuite, h.moduleOpenerFailSuite,
		h.moduleCrossStreamSuite, h.moduleCancelSuite, h.moduleGenerationSuite, h.moduleCtxTimeoutSuite,
		h.moduleCloseSuite, h.moduleCrossHandleSuite, h.moduleBackdoorSuite, h.noURLAuditSuite,
	} {
		if err := suite(); err != nil {
			return err
		}
	}
	return nil
}

func (h *harness) startDaemon() error { return h.startDaemonAs("first") }

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
	h.child, h.client, h.addr = child, client, address
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

func (h *harness) request(ctx context.Context, command map[string]any) (json.RawMessage, error) {
	return h.client.Request(ctx, command)
}

func daemonErrContains(err error, substr string) error {
	if err == nil {
		return errors.New("expected failure containing " + substr + " but the call succeeded")
	}
	if !strings.Contains(err.Error(), substr) {
		return fmt.Errorf("expected error containing %q, got %q", substr, err.Error())
	}
	return nil
}

func (h *harness) workdir(name string) (string, error) {
	dir := filepath.Join(h.workspace, name)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", err
	}
	return dir, nil
}

func (h *harness) authenticate(ctx context.Context, name, directory string) error {
	command := map[string]any{"type": "authenticateMcpServer", "name": name, "directory": directory}
	_, err := h.request(ctx, command)
	return err
}

func (h *harness) cancel(name string) error {
	_, err := h.request(h.ctx, map[string]any{"type": "cancelAuthenticateMcpServer", "name": name})
	return err
}

func (h *harness) callsFor(name string) []fakeCall {
	var out []fakeCall
	for _, call := range h.readCalls() {
		if call.Name == name {
			out = append(out, call)
		}
	}
	return out
}

func (h *harness) repoRoot() string {
	dir := h.self
	for range 3 {
		dir = filepath.Dir(dir)
	}
	return dir
}

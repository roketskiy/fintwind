package main

// Module layer: direct backendmcpauth.Service with a recording opener and the
// real Owned fake processes. No browser is ever opened; URLs only reach the
// recorder. The internal 360s CLI budget is structural: cancellation is
// proven with a 100ms caller context and never waited out.

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/backendmcpauth"
)

type recording struct {
	mu   sync.Mutex
	urls []string
	fail error
}

func (r *recording) open(url string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.urls = append(r.urls, url)
	return r.fail
}

func (r *recording) count() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return len(r.urls)
}

func (r *recording) first() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(r.urls) == 0 {
		return ""
	}
	return r.urls[0]
}

type moduleCase struct {
	root string
	wd   string
	svc  *backendmcpauth.Service
	rec  *recording
}

func (h *harness) newModuleCase(name, mode string) (*moduleCase, error) {
	root := filepath.Join(h.round, "mod-"+name)
	wd := filepath.Join(root, "workspace", "wd")
	if err := os.MkdirAll(wd, 0o700); err != nil {
		return nil, err
	}
	seed, _ := json.Marshal(map[string]any{"behaviors": map[string]string{name: mode}})
	if err := os.WriteFile(filepath.Join(root, "mcp-auth-seed.json"), seed, 0o600); err != nil {
		return nil, err
	}
	rec := &recording{}
	svc := backendmcpauth.New(h.ctx, root, false, rec.open)
	return &moduleCase{root: root, wd: wd, svc: svc, rec: rec}, nil
}

func (h *harness) waitModSpawned(c *moduleCase, name string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	path := filepath.Join(c.root, "mcp-auth-calls.jsonl")
	for {
		if raw, err := os.ReadFile(path); err == nil && len(raw) > 0 {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("module fake %s never spawned", name)
		}
		time.Sleep(100 * time.Millisecond)
	}
}

func (m *moduleCase) authenticate(ctx context.Context, binary, name string) error {
	return m.svc.Authenticate(ctx, binary, m.wd, name, nil)
}

func (h *harness) moduleFirstURLSuite() error {
	c, err := h.newModuleCase("mod-url", "url")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := c.authenticate(ctx, h.self, "mod-url"); err != nil {
		return fmt.Errorf("first URL flow: %w", err)
	}
	if c.rec.count() != 1 || c.rec.first() != "https://auth.example.test/start?x=1" {
		return fmt.Errorf("opener saw %v", c.rec.urls)
	}
	h.checks["module-first-url-once"] = true
	return nil
}

func (h *harness) moduleOnceSuite() error {
	c, err := h.newModuleCase("mod-multi", "multi")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := c.authenticate(ctx, h.self, "mod-multi"); err != nil {
		return fmt.Errorf("multi URL flow: %w", err)
	}
	if c.rec.count() != 1 || c.rec.first() != "https://auth.example.test/first" {
		return fmt.Errorf("only the first URL may open, got %v", c.rec.urls)
	}
	h.checks["module-only-first-opens"] = true
	return nil
}

func (h *harness) moduleSanitizeSuite() error {
	ansi, err := h.newModuleCase("mod-ansi", "ansi")
	if err != nil {
		return err
	}
	defer ansi.svc.Close()
	osc, err := h.newModuleCase("mod-osc8", "osc8")
	if err != nil {
		return err
	}
	defer osc.svc.Close()
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := ansi.authenticate(ctx, h.self, "mod-ansi"); err != nil {
		return fmt.Errorf("CSI flow: %w", err)
	}
	if ansi.rec.count() != 1 || ansi.rec.first() != "https://auth.example.test/ansi" {
		return fmt.Errorf("CSI sanitize failed: %v", ansi.rec.urls)
	}
	if err := osc.authenticate(ctx, h.self, "mod-osc8"); err != nil {
		return fmt.Errorf("OSC8 flow: %w", err)
	}
	if osc.rec.count() != 1 || osc.rec.first() != "https://auth.example.test/real" {
		return fmt.Errorf("OSC8 params not preferred: %v", osc.rec.urls)
	}
	h.checks["module-ansi-osc8"] = true
	return nil
}

func (h *harness) moduleSkipSuite() error {
	c, err := h.newModuleCase("mod-skip", "userinfo")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := c.authenticate(ctx, h.self, "mod-skip"); err != nil {
		return fmt.Errorf("userinfo flow: %w", err)
	}
	c2, err := h.newModuleCase("mod-skip2", "nonhttps")
	if err != nil {
		return err
	}
	defer c2.svc.Close()
	if err := c2.authenticate(ctx, h.self, "mod-skip2"); err != nil {
		return fmt.Errorf("non-https flow: %w", err)
	}
	if c.rec.count() != 0 || c2.rec.count() != 0 {
		return fmt.Errorf("userinfo/non-https must never open: %v %v", c.rec.urls, c2.rec.urls)
	}
	h.checks["module-skip-userinfo-nonhttps"] = true
	return nil
}

func (h *harness) moduleTrailingSuite() error {
	c, err := h.newModuleCase("mod-trail", "trailing")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := c.authenticate(ctx, h.self, "mod-trail"); err != nil {
		return fmt.Errorf("trailing flow: %w", err)
	}
	if c.rec.count() != 1 || c.rec.first() != "https://auth.example.test/trailing-now" {
		return fmt.Errorf("unterminated tail line lost: %v", c.rec.urls)
	}
	h.checks["module-trailing-no-newline"] = true
	return nil
}

func (h *harness) moduleGiantSuite() error {
	c, err := h.newModuleCase("mod-giant", "giant")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := c.authenticate(ctx, h.self, "mod-giant"); err != nil {
		return fmt.Errorf("giant-line flow: %w", err)
	}
	if c.rec.count() != 1 || c.rec.first() != "https://auth.example.test/after-giant" {
		return fmt.Errorf("stream did not recover after >64K line: %v", c.rec.urls)
	}
	h.checks["module-giant-line-recovery"] = true
	return nil
}

func (h *harness) moduleFloodSuite() error {
	c, err := h.newModuleCase("mod-flood", "flood")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	ctx, stop := context.WithTimeout(h.ctx, 90*time.Second)
	defer stop()
	err = c.authenticate(ctx, h.self, "mod-flood")
	if err == nil || !strings.Contains(err.Error(), "exceeded its budget") {
		return fmt.Errorf("budget flow want budget error, got %v", err)
	}
	if c.rec.count() != 0 {
		return fmt.Errorf("no URL may open on budget failure: %v", c.rec.urls)
	}
	// The failed job is reaped: the name is immediately reusable.
	if cancelled, cancelErr := c.svc.Cancel("mod-flood"); cancelErr != nil || cancelled {
		return fmt.Errorf("failed flow left a registration: %v %v", cancelled, cancelErr)
	}
	c2, err := h.newModuleCase("mod-flood2", "silent-ok")
	if err != nil {
		return err
	}
	defer c2.svc.Close()
	if err := c2.authenticate(ctx, h.self, "mod-flood2"); err != nil {
		return fmt.Errorf("post-budget flow: %w", err)
	}
	h.checks["module-budget-kills-job"] = true
	h.notes["flood"] = "1.5MB single stream -> budget error, name reusable"
	return nil
}

func (h *harness) moduleOpenerFailSuite() error {
	c, err := h.newModuleCase("mod-openfail", "url")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	c.rec.fail = errors.New("nope")
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	err = c.authenticate(ctx, h.self, "mod-openfail")
	if err == nil || err.Error() != "could not open MCP authorization page in the system browser" {
		return fmt.Errorf("opener failure want fixed error, got %v", err)
	}
	if strings.Contains(err.Error(), "https://") {
		return fmt.Errorf("URL leaked into opener error: %q", err.Error())
	}
	h.checks["module-opener-failure-fixed"] = true
	return nil
}

func (h *harness) moduleCrossStreamSuite() error {
	c, err := h.newModuleCase("mod-cross", "cross")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := c.authenticate(ctx, h.self, "mod-cross"); err != nil {
		return fmt.Errorf("cross-stream flow: %w", err)
	}
	if c.rec.count() != 1 {
		return fmt.Errorf("cross-stream must open exactly once, got %v", c.rec.urls)
	}
	h.checks["module-cross-stream-once"] = true
	return nil
}

func (h *harness) moduleCancelSuite() error {
	c, err := h.newModuleCase("mod-cancel", "hang")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	longCtx, longStop := context.WithTimeout(h.ctx, 120*time.Second)
	defer longStop()
	type outcome struct{ err error }
	done := make(chan outcome, 1)
	go func() { done <- outcome{c.authenticate(longCtx, h.self, "mod-cancel")} }()
	if err := h.waitModSpawned(c, "mod-cancel", 20*time.Second); err != nil {
		return err
	}
	cancelled, err := c.svc.Cancel("mod-cancel")
	if err != nil || !cancelled {
		return fmt.Errorf("cancel want (true, nil), got (%v, %v)", cancelled, err)
	}
	if err := (<-done).err; err == nil || !strings.Contains(err.Error(), "cancell") {
		return fmt.Errorf("cancelled flow want cancel error, got %v", err)
	}
	if cancelled, err := c.svc.Cancel("mod-cancel"); err != nil || cancelled {
		return fmt.Errorf("second cancel want (false, nil), got (%v, %v)", cancelled, err)
	}
	if _, err := c.svc.Cancel(""); err == nil || !strings.Contains(err.Error(), "empty or invalid") {
		return fmt.Errorf("invalid cancel want validation, got %v", err)
	}
	h.checks["module-cancel-idempotent"] = true
	return nil
}

func (h *harness) moduleGenerationSuite() error {
	c, err := h.newModuleCase("mod-gen", "hang")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	longCtx, longStop := context.WithTimeout(h.ctx, 120*time.Second)
	defer longStop()
	type outcome struct{ err error }
	done := make(chan outcome, 1)
	go func() { done <- outcome{c.authenticate(longCtx, h.self, "mod-gen")} }()
	if err := h.waitModSpawned(c, "mod-gen", 20*time.Second); err != nil {
		return err
	}
	// While the old flow is still registered, a new flow must be refused
	// rather than stealing or dropping the registration.
	quickCtx, quickStop := context.WithTimeout(h.ctx, 30*time.Second)
	defer quickStop()
	if err := c.authenticate(quickCtx, h.self, "mod-gen"); err == nil || !strings.Contains(err.Error(), "already in progress") {
		return fmt.Errorf("overlapping flow want rejection, got %v", err)
	}
	if cancelled, err := c.svc.Cancel("mod-gen"); err != nil || !cancelled {
		return fmt.Errorf("cancel want (true, nil), got (%v, %v)", cancelled, err)
	}
	old := (<-done).err
	if old == nil || !strings.Contains(old.Error(), "cancell") {
		return fmt.Errorf("old flow want cancel error, got %v", old)
	}
	// The old generation is fully gone (defer ran before return): the same
	// name authenticates fresh and its opener fires.
	seed, _ := json.Marshal(map[string]any{"behaviors": map[string]string{"mod-gen": "url"}})
	if err := os.WriteFile(filepath.Join(c.root, "mcp-auth-seed.json"), seed, 0o600); err != nil {
		return err
	}
	if err := c.authenticate(quickCtx, h.self, "mod-gen"); err != nil {
		return fmt.Errorf("re-auth after cancel: %w", err)
	}
	if c.rec.count() != 1 {
		return fmt.Errorf("new generation never opened: %v", c.rec.urls)
	}
	if cancelled, err := c.svc.Cancel("mod-gen"); err != nil || cancelled {
		return fmt.Errorf("post-flow cancel want (false, nil), got (%v, %v)", cancelled, err)
	}
	h.checks["module-generation-guard"] = true
	return nil
}

func (h *harness) moduleCtxTimeoutSuite() error {
	c, err := h.newModuleCase("mod-ctx", "hang")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	ctx, stop := context.WithTimeout(h.ctx, 100*time.Millisecond)
	defer stop()
	start := time.Now()
	err = c.authenticate(ctx, h.self, "mod-ctx")
	elapsed := time.Since(start)
	if err == nil || !strings.Contains(err.Error(), "cancell") {
		return fmt.Errorf("100ms ctx want cancel error, got %v", err)
	}
	if elapsed > 15*time.Second {
		return fmt.Errorf("100ms cancel took %v (360s budget must be structural)", elapsed)
	}
	h.checks["module-caller-cancel-fast"] = true
	h.notes["ctxtimeout"] = "100ms caller ctx cancelled a hang without waiting out the 360s budget"
	return nil
}

func (h *harness) moduleCloseSuite() error {
	c, err := h.newModuleCase("mod-close", "hang")
	if err != nil {
		return err
	}
	longCtx, longStop := context.WithTimeout(h.ctx, 120*time.Second)
	defer longStop()
	type outcome struct{ err error }
	done := make(chan outcome, 1)
	go func() { done <- outcome{c.authenticate(longCtx, h.self, "mod-close")} }()
	if err := h.waitModSpawned(c, "mod-close", 20*time.Second); err != nil {
		return err
	}
	start := time.Now()
	c.svc.Close()
	if err := (<-done).err; err == nil || !strings.Contains(err.Error(), "cancell") {
		return fmt.Errorf("close want cancel error, got %v", err)
	}
	if elapsed := time.Since(start); elapsed > 10*time.Second {
		return fmt.Errorf("Close did not reclaim promptly: %v", elapsed)
	}
	if err := c.svc.Authenticate(h.ctx, h.self, c.wd, "mod-close", nil); err == nil || !strings.Contains(err.Error(), "closed") {
		return fmt.Errorf("post-Close want closed, got %v", err)
	}
	h.checks["module-close-reclaims"] = true
	return nil
}

func (h *harness) moduleCrossHandleSuite() error {
	newHang := func(name string) (*moduleCase, context.CancelFunc, chan error, error) {
		c, err := h.newModuleCase("mod-x-"+name, "hang")
		if err != nil {
			return nil, nil, nil, err
		}
		ctx, stop := context.WithTimeout(h.ctx, 120*time.Second)
		done := make(chan error, 1)
		go func() { done <- c.authenticate(ctx, h.self, "mod-x-"+name) }()
		if err := h.waitModSpawned(c, "mod-x-"+name, 20*time.Second); err != nil {
			stop()
			c.svc.Close()
			return nil, nil, nil, err
		}
		return c, stop, done, nil
	}
	// Second-started flow cancelled while the first is alive: if the second
	// had inherited the first's write ends, the first's cancel would stall
	// in the 5s output drain instead of reporting cancellation.
	a, stopA, doneA, err := newHang("a")
	if err != nil {
		return err
	}
	defer a.svc.Close()
	defer stopA()
	b, stopB, doneB, err := newHang("b")
	if err != nil {
		return err
	}
	defer b.svc.Close()
	defer stopB()
	if _, err := b.svc.Cancel("mod-x-b"); err != nil {
		return err
	}
	start := time.Now()
	if err := <-doneB; err == nil || !strings.Contains(err.Error(), "cancell") {
		return fmt.Errorf("B while A alive want cancel, got %v", err)
	}
	if elapsed := time.Since(start); elapsed > 8*time.Second {
		return fmt.Errorf("B cancel stalled (cross-inherited EOF?): %v", elapsed)
	}
	if _, err := a.svc.Cancel("mod-x-a"); err != nil {
		return err
	}
	if err := <-doneA; err == nil || !strings.Contains(err.Error(), "cancell") {
		return fmt.Errorf("A after B want cancel, got %v", err)
	}
	// First-started flow cancelled while the second is alive: the mirror
	// direction of the same handle-list property.
	c, stopC, doneC, err := newHang("c")
	if err != nil {
		return err
	}
	defer c.svc.Close()
	defer stopC()
	d, stopD, doneD, err := newHang("d")
	if err != nil {
		return err
	}
	defer d.svc.Close()
	defer stopD()
	if _, err := c.svc.Cancel("mod-x-c"); err != nil {
		return err
	}
	start = time.Now()
	if err := <-doneC; err == nil || !strings.Contains(err.Error(), "cancell") {
		return fmt.Errorf("C while D alive want cancel, got %v", err)
	}
	if elapsed := time.Since(start); elapsed > 8*time.Second {
		return fmt.Errorf("C cancel stalled (cross-inherited EOF?): %v", elapsed)
	}
	if _, err := d.svc.Cancel("mod-x-d"); err != nil {
		return err
	}
	if err := <-doneD; err == nil || !strings.Contains(err.Error(), "cancell") {
		return fmt.Errorf("D after C want cancel, got %v", err)
	}
	h.checks["module-no-cross-inherit"] = true
	h.notes["handles"] = "cross-cancel both directions fast with cancel (never drain-timeout); reclamation by Owned handle close only, no PID scan"
	return nil
}

func (h *harness) moduleBackdoorSuite() error {
	files := []string{
		"internal/backendmcpauth/service.go",
		"internal/backendmcpauth/run.go",
		"internal/backendmcpauth/output.go",
		"internal/backendmcpauth/open_windows.go",
		"internal/backendmcpauth/open_other.go",
		"internal/backend/mcp_auth.go",
		"internal/ownedprocess/ownedprocess.go",
		"internal/ownedprocess/ownedprocess_windows.go",
		"internal/ownedprocess/ownedprocess_other.go",
	}
	markers := []string{"mcp-auth-seed", "MCP_AUTH_FAKE", "OPENCODE_TEST_FAKE", "mcpauthcheck", "mcp-fake-hold"}
	for _, file := range files {
		raw, err := os.ReadFile(filepath.Join(h.repoRoot(), file))
		if err != nil {
			return fmt.Errorf("backdoor read %s: %w", file, err)
		}
		for _, marker := range markers {
			if strings.Contains(string(raw), marker) {
				return fmt.Errorf("test marker %q found in product file %s", marker, file)
			}
		}
		if strings.Contains(string(raw), `os.Getenv("OPENCODE_TEST_HOME")`) {
			return fmt.Errorf("product file %s reads the test marker env", file)
		}
	}
	h.checks["module-no-test-backdoor"] = true
	h.notes["backdoor"] = "product never branches on the fixture marker; OPENCODE_TEST_HOME is set-only plumbing"
	return nil
}

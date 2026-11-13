package main

// Daemon WS layer: real daemon, this executable as the fake `mcp auth` CLI.
// The fixture never prints an https URL at this layer.

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
)

func (h *harness) seedBehaviors(behaviors map[string]string) error {
	return h.writeSeed(map[string]any{"behaviors": behaviors})
}

func (h *harness) waitSpawned(name string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for {
		if len(h.callsFor(name)) > 0 {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("fake CLI %s never spawned", name)
		}
		time.Sleep(100 * time.Millisecond)
	}
}

func (h *harness) okSuite() error {
	dir, err := h.workdir("ok-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-ok": "text-ok"}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := h.authenticate(ctx, "mcp-ok", dir); err != nil {
		return fmt.Errorf("ok flow: %w", err)
	}
	calls := h.callsFor("mcp-ok")
	if len(calls) != 1 {
		return fmt.Errorf("want 1 CLI spawn, got %d", len(calls))
	}
	if len(calls[0].Argv) != 3 || calls[0].Argv[0] != "mcp" || calls[0].Argv[1] != "auth" || calls[0].Argv[2] != "mcp-ok" {
		return fmt.Errorf("unexpected argv: %v", calls[0].Argv)
	}
	if calls[0].Cwd != dir {
		return fmt.Errorf("unexpected cwd: %q want %q", calls[0].Cwd, dir)
	}
	h.checks["daemon-ok-argv"] = true
	return nil
}

func (h *harness) validationSuite() error {
	dir, err := h.workdir("valid-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	badNames := map[string]string{
		"":                       "empty or invalid",
		strings.Repeat("n", 257): "empty or invalid",
		"-x":                     "empty or invalid",
		"a\x01b":                 "control characters",
	}
	for name, want := range badNames {
		if err := daemonErrContains(h.authenticate(ctx, name, dir), want); err != nil {
			return fmt.Errorf("name %q: %w", name, err)
		}
		if len(h.callsFor(name)) != 0 {
			return fmt.Errorf("invalid name %q spawned a CLI", name)
		}
	}
	if err := daemonErrContains(h.authenticate(ctx, "mcp-out", `C:\Windows`), "outside isolated workspace"); err != nil {
		return fmt.Errorf("outside dir: %w", err)
	}
	if len(h.callsFor("mcp-out")) != 0 {
		return errors.New("outside dir spawned a CLI")
	}
	missing := dir + `\no-such-dir`
	if err := daemonErrContains(h.authenticate(ctx, "mcp-missing", missing), "unavailable"); err != nil {
		return fmt.Errorf("missing dir: %w", err)
	}
	if _, err := h.request(ctx, map[string]any{"type": "authenticateMcpServer", "name": "mcp-nobin", "directory": dir, "binary": `C:\nonexistent\cli.exe`}); err == nil {
		return errors.New("missing binary was accepted")
	} else if len(h.callsFor("mcp-nobin")) != 0 {
		return errors.New("missing binary spawned a CLI")
	}
	h.checks["daemon-validation"] = true
	return nil
}

func (h *harness) failSuite() error {
	dir, err := h.workdir("fail-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-fail": "fail"}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	err = h.authenticate(ctx, "mcp-fail", dir)
	if err := daemonErrContains(err, "did not finish successfully"); err != nil {
		return fmt.Errorf("nonzero exit: %w", err)
	}
	if strings.Contains(err.Error(), "boom") {
		return fmt.Errorf("CLI text leaked into error: %q", err.Error())
	}
	h.checks["daemon-nonzero-exit"] = true
	return nil
}

func (h *harness) concurrentSuite() error {
	dir, err := h.workdir("long-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-long": "hang"}); err != nil {
		return err
	}
	longCtx, longStop := context.WithTimeout(h.ctx, 300*time.Second)
	defer longStop()
	type outcome struct{ err error }
	first := make(chan outcome, 1)
	go func() { first <- outcome{h.authenticate(longCtx, "mcp-long", dir)} }()
	if err := h.waitSpawned("mcp-long", 20*time.Second); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := daemonErrContains(h.authenticate(ctx, "mcp-long", dir), "already in progress"); err != nil {
		return fmt.Errorf("same-name overlap: %w", err)
	}
	if err := h.cancel("mcp-long"); err != nil {
		return fmt.Errorf("cancel: %w", err)
	}
	if err := daemonErrContains((<-first).err, "cancell"); err != nil {
		return fmt.Errorf("cancelled flow: %w", err)
	}
	if err := h.cancel("mcp-never"); err != nil {
		return fmt.Errorf("unknown cancel should ack: %w", err)
	}
	if err := h.seedBehaviors(map[string]string{"mcp-long": "text-ok"}); err != nil {
		return err
	}
	if err := h.authenticate(ctx, "mcp-long", dir); err != nil {
		return fmt.Errorf("re-auth after cancel: %w", err)
	}
	if len(h.callsFor("mcp-long")) != 2 {
		return fmt.Errorf("want 2 spawns (hang + re-auth), got %d", len(h.callsFor("mcp-long")))
	}
	h.checks["daemon-concurrent-cancel-generation"] = true
	h.notes["concurrent"] = "overlap rejected, cancel acked, re-auth ok"
	return nil
}

func (h *harness) budgetDaemonSuite() error {
	dir, err := h.workdir("flood-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-flood": "flood"}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 90*time.Second)
	defer stop()
	if err := daemonErrContains(h.authenticate(ctx, "mcp-flood", dir), "exceeded its budget"); err != nil {
		return fmt.Errorf("stream budget: %w", err)
	}
	h.checks["daemon-stream-budget"] = true
	return nil
}

func (h *harness) holderSuite() error {
	dir, err := h.workdir("holder-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-holder": "holder"}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	start := time.Now()
	if err := h.authenticate(ctx, "mcp-holder", dir); err != nil {
		return fmt.Errorf("holder flow: %w", err)
	}
	if elapsed := time.Since(start); elapsed > 15*time.Second {
		return fmt.Errorf("grandchild holder not reclaimed by Job: %v", elapsed)
	}
	h.checks["daemon-job-reclaims-grandchild"] = true
	h.notes["holder"] = "holder parent exited, grandchild killed by Job close"
	return nil
}

func (h *harness) crossHandleSuite() error {
	dirA, err := h.workdir("xa-wd")
	if err != nil {
		return err
	}
	dirB, err := h.workdir("xb-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-xa": "hang", "mcp-xb": "hang"}); err != nil {
		return err
	}
	ctxA, stopA := context.WithTimeout(h.ctx, 300*time.Second)
	defer stopA()
	ctxB, stopB := context.WithTimeout(h.ctx, 300*time.Second)
	defer stopB()
	type outcome struct{ err error }
	aCh := make(chan outcome, 1)
	bCh := make(chan outcome, 1)
	go func() { aCh <- outcome{h.authenticate(ctxA, "mcp-xa", dirA)} }()
	if err := h.waitSpawned("mcp-xa", 20*time.Second); err != nil {
		return err
	}
	go func() { bCh <- outcome{h.authenticate(ctxB, "mcp-xb", dirB)} }()
	if err := h.waitSpawned("mcp-xb", 20*time.Second); err != nil {
		return err
	}
	cancelled := time.Now()
	if err := h.cancel("mcp-xa"); err != nil {
		return fmt.Errorf("cancel A: %w", err)
	}
	aErr := (<-aCh).err
	if err := daemonErrContains(aErr, "cancell"); err != nil {
		return fmt.Errorf("A while B alive: %w", err)
	}
	if strings.Contains(aErr.Error(), "did not close") {
		return fmt.Errorf("A drain blocked on B's pipes: %q", aErr.Error())
	}
	if elapsed := time.Since(cancelled); elapsed > 8*time.Second {
		return fmt.Errorf("A cancel slow while B alive: %v", elapsed)
	}
	if err := h.cancel("mcp-xb"); err != nil {
		return fmt.Errorf("cancel B: %w", err)
	}
	if err := daemonErrContains((<-bCh).err, "cancell"); err != nil {
		return fmt.Errorf("B after A: %w", err)
	}
	if err := h.seedBehaviors(map[string]string{"mcp-xc": "text-ok"}); err != nil {
		return err
	}
	dirC, err := h.workdir("xc-wd")
	if err != nil {
		return err
	}
	quickCtx, quickStop := context.WithTimeout(h.ctx, 60*time.Second)
	defer quickStop()
	if err := h.authenticate(quickCtx, "mcp-xc", dirC); err != nil {
		return fmt.Errorf("quick flow after hangs: %w", err)
	}
	h.checks["daemon-no-cross-inherit"] = true
	return nil
}

func (h *harness) capacitySuite() error {
	dir, err := h.workdir("cap-wd")
	if err != nil {
		return err
	}
	behaviors := map[string]string{}
	names := []string{}
	for i := range 16 {
		name := fmt.Sprintf("cap-%02d", i)
		names = append(names, name)
		behaviors[name] = "hang"
	}
	if err := h.seedBehaviors(behaviors); err != nil {
		return err
	}
	type outcome struct{ err error }
	done := make([]chan outcome, 16)
	for i, name := range names {
		done[i] = make(chan outcome, 1)
		ctx, stop := context.WithTimeout(h.ctx, 300*time.Second)
		defer stop()
		go func(ch chan outcome, n string, c context.Context) {
			ch <- outcome{h.authenticate(c, n, dir)}
		}(done[i], name, ctx)
	}
	deadline := time.Now().Add(30 * time.Second)
	for {
		spawned := 0
		for _, call := range h.readCalls() {
			if strings.HasPrefix(call.Name, "cap-") {
				spawned++
			}
		}
		if spawned >= 16 || time.Now().After(deadline) {
			if spawned < 16 {
				return fmt.Errorf("only %d of 16 flows spawned", spawned)
			}
			break
		}
		time.Sleep(200 * time.Millisecond)
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := daemonErrContains(h.authenticate(ctx, "cap-extra", dir), "capacity reached"); err != nil {
		return fmt.Errorf("17th flow: %w", err)
	}
	probeStart := time.Now()
	if _, err := h.request(ctx, map[string]any{"type": "probeProvider"}); err != nil {
		return fmt.Errorf("ordinary worker blocked by auth storm: %w", err)
	}
	if elapsed := time.Since(probeStart); elapsed > 25*time.Second {
		return fmt.Errorf("probe starved by auth workers: %v", elapsed)
	}
	second, err := daemon.Dial(h.ctx, h.addr, h.token)
	if err != nil {
		return fmt.Errorf("second connection: %w", err)
	}
	if _, err := second.Request(ctx, map[string]any{"type": "probeProvider"}); err != nil {
		second.Close()
		return fmt.Errorf("probe on second connection: %w", err)
	}
	second.Close()
	if _, err := h.request(ctx, map[string]any{"type": "updateOpenCode"}); err == nil {
		return errors.New("update during auth storm was accepted")
	} else if !strings.Contains(err.Error(), "busy") {
		return fmt.Errorf("update during storm want busy, got %q", err.Error())
	}
	for _, name := range names {
		if err := h.cancel(name); err != nil {
			return fmt.Errorf("cancel %s: %w", name, err)
		}
	}
	for i, name := range names {
		if err := daemonErrContains((<-done[i]).err, "cancell"); err != nil {
			return fmt.Errorf("storm flow %s: %w", name, err)
		}
	}
	if err := h.seedBehaviors(map[string]string{"cap-00": "text-ok"}); err != nil {
		return err
	}
	if err := h.authenticate(ctx, "cap-00", dir); err != nil {
		return fmt.Errorf("post-storm re-auth: %w", err)
	}
	h.checks["daemon-capacity-workers-management"] = true
	h.notes["capacity"] = "16 hangs, 17th rejected, probe ok on both conns, update busy, all cancelled"
	return nil
}

func (h *harness) envSuite() error {
	dir, err := h.workdir("env-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-env": "text-ok"}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := h.authenticate(ctx, "mcp-env", dir); err != nil {
		return fmt.Errorf("env flow: %w", err)
	}
	calls := h.callsFor("mcp-env")
	if len(calls) != 1 {
		return fmt.Errorf("want 1 spawn, got %d", len(calls))
	}
	env := calls[0].Env
	want := map[string]string{
		"OPENCODE_DB":                     filepath.Join(h.round, "data", "opencode.db"),
		"OPENCODE_CONFIG_DIR":             filepath.Join(h.round, "home", "opencode"),
		"OPENCODE_TEST_HOME":              filepath.Join(h.round, "home"),
		"OPENCODE_DISABLE_PROJECT_CONFIG": "true",
		"HOME":                            filepath.Join(h.round, "home"),
		"USERPROFILE":                     filepath.Join(h.round, "home"),
		"TEMP":                            filepath.Join(h.round, "tmp"),
	}
	for key, value := range want {
		if env[key] != value {
			return fmt.Errorf("env %s = %q, want %q", key, env[key], value)
		}
	}
	if real, _ := os.UserHomeDir(); env["HOME"] == real || env["USERPROFILE"] == real {
		return fmt.Errorf("child inherited the real user home: %q", real)
	}
	for _, key := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "OPENCODE_API_KEY", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY"} {
		if _, ok := env[key]; ok {
			return fmt.Errorf("secret/proxy key %s crossed into the child", key)
		}
	}
	if _, ok := env["PATH"]; !ok {
		return errors.New("allowlisted PATH missing from child env")
	}
	h.checks["daemon-env-isolation"] = true
	return nil
}

func (h *harness) shortSuite() error {
	dir, err := h.workdir("short-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-short": "silent-ok"}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := h.authenticate(ctx, "mcp-short", dir); err != nil {
		return fmt.Errorf("silent short-lived flow: %w", err)
	}
	h.checks["daemon-short-silent"] = true
	return nil
}

func (h *harness) restartSuite() error {
	dir, err := h.workdir("re-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-re": "text-ok", "mcp-re2": "text-ok"}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := h.authenticate(ctx, "mcp-re", dir); err != nil {
		return fmt.Errorf("pre-restart auth: %w", err)
	}
	if err := h.closeDaemon(); err != nil {
		return err
	}
	if err := h.startDaemonAs("second"); err != nil {
		return err
	}
	if err := h.authenticate(ctx, "mcp-re2", dir); err != nil {
		return fmt.Errorf("post-restart auth: %w", err)
	}
	h.checks["daemon-restart"] = true
	return nil
}

// closeSuite is the retained forced-shutdown control: closing Owned directly
// exercises the outer Job kill path, not backend.Server.Close.
func (h *harness) closeSuite() error {
	dir, err := h.workdir("close-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-close": "hang"}); err != nil {
		return err
	}
	longCtx, longStop := context.WithTimeout(h.ctx, 300*time.Second)
	defer longStop()
	type outcome struct{ err error }
	done := make(chan outcome, 1)
	go func() { done <- outcome{h.authenticate(longCtx, "mcp-close", dir)} }()
	if err := h.waitSpawned("mcp-close", 20*time.Second); err != nil {
		return err
	}
	start := time.Now()
	if err := h.closeDaemon(); err != nil {
		return err
	}
	if elapsed := time.Since(start); elapsed > 20*time.Second {
		return fmt.Errorf("daemon close hung on in-flight auth: %v", elapsed)
	}
	<-done
	if err := h.startDaemonAs("third"); err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-close2": "text-ok"}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := h.authenticate(ctx, "mcp-close2", dir); err != nil {
		return fmt.Errorf("post-close auth: %w", err)
	}
	h.checks["daemon-close-reclaims"] = true
	h.notes["close"] = "kill with in-flight hang returned promptly; daemon restarted clean"
	return nil
}

// gracefulCloseSuite sends the daemon's supported shutdown frame on a second
// WebSocket, while the auth RPC's original WebSocket stays live. This keeps
// the auth request context from being cancelled by a client disconnect. It
// does not close Owned to establish
// success: success requires the held daemon process handle to signal and
// report exit code 0 first. The fixture grandchild writes a ready marker and
// holds inherited stdout/stderr, so Server.Close must cancel the auth service,
// kill its owned Job, release both pipe readers, and wait before main exits.
func (h *harness) gracefulCloseSuite() error {
	name := "mcp-graceful-close"
	dir, err := h.workdir("graceful-close-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{name: "holder-hang"}); err != nil {
		return err
	}
	before, err := h.request(h.ctx, map[string]any{"type": "getSettings"})
	if err != nil {
		return fmt.Errorf("pre-close settings: %w", err)
	}
	flowCtx, flowStop := context.WithTimeout(h.ctx, 300*time.Second)
	defer flowStop()
	type outcome struct{ err error }
	flowDone := make(chan outcome, 1)
	go func() { flowDone <- outcome{h.authenticate(flowCtx, name, dir)} }()
	if err := h.waitSpawned(name, 20*time.Second); err != nil {
		return err
	}
	marker := filepath.Join(h.round, "holder-ready-"+name+".json")
	deadline := time.Now().Add(15 * time.Second)
	for {
		if _, err := os.Stat(marker); err == nil {
			break
		}
		if time.Now().After(deadline) {
			return errors.New("holder grandchild did not publish its ready marker")
		}
		time.Sleep(50 * time.Millisecond)
	}
	owned := h.child
	authClient := h.client
	if owned == nil || authClient == nil {
		return errors.New("daemon handles missing before graceful shutdown")
	}
	shutdownClient, err := daemon.Dial(h.ctx, h.addr, h.token)
	if err != nil {
		return fmt.Errorf("open shutdown connection: %w", err)
	}
	started := time.Now()
	sendCtx, sendStop := context.WithTimeout(h.ctx, 5*time.Second)
	if err := shutdownClient.Send(sendCtx, map[string]any{"type": "shutdown"}); err != nil {
		sendStop()
		shutdownClient.Close()
		return fmt.Errorf("send shutdown frame: %w", err)
	}
	sendStop()
	// Poll only the Owned process handle. Do not call Owned.Close unless this
	// bounded observation fails; the deferred harness cleanup remains fallback.
	deadline = time.Now().Add(20 * time.Second)
	for {
		exited, err := owned.Exited(100)
		if err != nil {
			return fmt.Errorf("wait graceful daemon exit: %w", err)
		}
		if exited {
			break
		}
		if time.Now().After(deadline) {
			return errors.New("daemon did not exit through graceful shutdown within 20s")
		}
		time.Sleep(50 * time.Millisecond)
	}
	code, err := owned.ExitCode()
	if err != nil {
		return fmt.Errorf("read graceful daemon exit code: %w", err)
	}
	if code != 0 {
		return fmt.Errorf("graceful daemon exit code = %d, want 0", code)
	}
	if elapsed := time.Since(started); elapsed > 20*time.Second {
		return fmt.Errorf("graceful shutdown exceeded 20s: %v", elapsed)
	}
	// The daemon's Server.Close waits for every auth worker; only after its
	// normal exit may the harness close the now-signaled process handle.
	authClient.Close()
	shutdownClient.Close()
	h.client = nil
	if err := owned.Close(); err != nil {
		return fmt.Errorf("release exited daemon handle: %w", err)
	}
	h.child = nil
	select {
	case <-flowDone:
	case <-time.After(5 * time.Second):
		return errors.New("auth RPC did not resolve after graceful daemon exit")
	}
	// The holder's own readiness marker proves it was alive inside the daemon
	// Job before shutdown. A clean daemon exit implies Server.Close cancelled
	// that Job and drained both auth pipes. Then restart against the same data
	// directory and prove the canceled name no longer remains registered.
	if err := h.startDaemonAs("graceful-restart"); err != nil {
		return fmt.Errorf("restart after graceful shutdown: %w", err)
	}
	if err := h.seedBehaviors(map[string]string{name: "text-ok"}); err != nil {
		return err
	}
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	if err := h.authenticate(ctx, name, dir); err != nil {
		return fmt.Errorf("same-name auth after graceful shutdown: %w", err)
	}
	after, err := h.request(ctx, map[string]any{"type": "getSettings"})
	if err != nil {
		return fmt.Errorf("post-close settings: %w", err)
	}
	if string(before) != string(after) {
		return fmt.Errorf("graceful shutdown changed settings:\nbefore %s\nafter  %s", before, after)
	}
	h.checks["daemon-graceful-close-waits"] = true
	h.notes["graceful-close"] = fmt.Sprintf("shutdown frame; holder ready; daemon process handle exited code 0 in %v before Owned.Close; same-name reauth and settings stable", time.Since(started).Round(time.Millisecond))
	return nil
}

func (h *harness) configSuite() error {
	ctx, stop := context.WithTimeout(h.ctx, 60*time.Second)
	defer stop()
	before, err := h.request(ctx, map[string]any{"type": "getSettings"})
	if err != nil {
		return fmt.Errorf("baseline settings: %w", err)
	}
	dir, err := h.workdir("cfg-wd")
	if err != nil {
		return err
	}
	if err := h.seedBehaviors(map[string]string{"mcp-cfg": "text-ok"}); err != nil {
		return err
	}
	if err := h.authenticate(ctx, "mcp-cfg", dir); err != nil {
		return err
	}
	after, err := h.request(ctx, map[string]any{"type": "getSettings"})
	if err != nil {
		return fmt.Errorf("post-auth settings: %w", err)
	}
	if string(before) != string(after) {
		return fmt.Errorf("task settings changed by auth flow:\nbefore %s\nafter  %s", before, after)
	}
	h.checks["daemon-settings-intact"] = true
	return nil
}

func (h *harness) noURLAuditSuite() error {
	raw, err := os.ReadFile(h.seedPath())
	if err != nil {
		return err
	}
	if strings.Contains(string(raw), "https://") {
		return errors.New("daemon-layer seed contains an https URL")
	}
	for _, call := range h.readCalls() {
		for _, part := range call.Argv {
			if strings.Contains(part, "https://") {
				return errors.New("daemon-layer argv contains an https URL")
			}
		}
	}
	h.checks["daemon-never-prints-url"] = true
	h.notes["nourl"] = "round seed and daemon-layer argv free of https"
	return nil
}

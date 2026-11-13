package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"

	"github.com/google/uuid"
)

func pathInside(parent, target string) bool {
	rel, err := filepath.Rel(parent, target)
	return err == nil && filepath.IsLocal(rel)
}

func assertManagedLayout(round isoRound, binary, version string, counts map[string]any) error {
	if !pathInside(round.dataDir, binary) {
		return fmt.Errorf("managed binary escapes profile: %s", binary)
	}
	info, err := os.Stat(binary)
	if err != nil || !info.Mode().IsRegular() {
		return errors.New("managed binary missing or irregular")
	}
	packageJSON := filepath.Join(round.dataDir, "opencode-pm", "bun", "install", "global", "node_modules", "@opencode", "cli", "package.json")
	raw, err := os.ReadFile(packageJSON)
	if err != nil {
		return errors.New("managed package manifest missing")
	}
	var manifest struct {
		Version string `json:"version"`
	}
	if json.Unmarshal(raw, &manifest) != nil || manifest.Version != version {
		return fmt.Errorf("managed package version %q", manifest.Version)
	}
	calls := bunInstallCalls(round.workspace)
	if len(calls) != 1 {
		return fmt.Errorf("bun install ran %d times", len(calls))
	}
	env := calls[0].Env
	home := filepath.Join(round.round, "home")
	for key, want := range map[string]string{
		"BUN_INSTALL":           filepath.Join(round.dataDir, "opencode-pm", "bun"),
		"BUN_INSTALL_CACHE_DIR": filepath.Join(round.dataDir, "opencode-pm", "bun-cache"),
		"HOME":                  home,
	} {
		got := filepath.Clean(env[key])
		if !strings.EqualFold(got, filepath.Clean(want)) {
			return fmt.Errorf("%s=%q outside profile", key, env[key])
		}
	}
	return nil
}

func installOnce(ctx context.Context, client *daemon.Client) (json.RawMessage, error) {
	return client.Request(ctx, map[string]any{"type": "installOpenCode"})
}

func parseInstalled(raw json.RawMessage) (string, string, error) {
	var body struct {
		Type     string `json:"type"`
		Settings struct {
			Binary string `json:"opencode_binary_path"`
		} `json:"settings"`
	}
	if err := json.Unmarshal(raw, &body); err != nil {
		return "", "", err
	}
	if body.Type != "openCodeInstalled" || body.Settings.Binary == "" {
		return "", "", fmt.Errorf("unexpected install reply %s", strings.TrimSpace(string(raw)))
	}
	return body.Type, body.Settings.Binary, nil
}

func roundInstallCore(ctx context.Context, daemonBinary string, iso isoRound, checks map[string]bool, note func(string, error), counts map[string]any, versions map[string]any) error {
	token := uuid.NewString()
	child, address, err := launchDaemon(ctx, daemonBinary, iso, token)
	if err != nil {
		return err
	}
	defer child.Close()
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()
	// A second connection: the server dispatches one request at a time per
	// connection, so true management concurrency needs two peers.
	client2, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client2.Close()

	type outcome struct {
		raw json.RawMessage
		err error
	}
	first := make(chan outcome, 1)
	go func() { raw, err := installOnce(ctx, client); first <- outcome{raw, err} }()
	time.Sleep(300 * time.Millisecond)
	t0 := time.Now()
	second, secondErr := installOnce(ctx, client2)
	secondElapsed := time.Since(t0)
	note("concurrent-second-install-busy", secondErr)
	if secondErr == nil || !strings.Contains(secondErr.Error(), "management is busy") {
		return fmt.Errorf("concurrent install was not refused fast: %v", secondErr)
	}
	if secondElapsed > 10*time.Second {
		return fmt.Errorf("concurrent install refusal took %v", secondElapsed)
	}
	_ = second
	t1 := time.Now()
	catalog, catalogErr := client2.Request(ctx, map[string]any{"type": "loadTaskState"})
	catalogElapsed := time.Since(t1)
	note("ordinary-rpc-during-management", catalogErr)
	if catalogErr == nil || !strings.Contains(catalogErr.Error(), "in progress") {
		return fmt.Errorf("ordinary RPC during management was not refused: %v", catalogErr)
	}
	if catalogElapsed > 10*time.Second {
		return fmt.Errorf("ordinary RPC refusal took %v", catalogElapsed)
	}
	_ = catalog
	checks["management-busy-fast"] = true

	firstOut := <-first
	note("install-race-winner", firstOut.err)
	if firstOut.err != nil {
		return fmt.Errorf("raced install failed: %w", firstOut.err)
	}
	_, binary, err := parseInstalled(firstOut.raw)
	if err != nil {
		return err
	}
	version, err := requestedVersion(iso.workspace)
	if err != nil {
		return err
	}
	versions["officialLatest"] = version
	if err := assertManagedLayout(iso, binary, version, counts); err != nil {
		return fmt.Errorf("install layout: %w", err)
	}
	if selectedBinary(iso.dataDir) != binary {
		return errors.New("daemon.json was not set to the managed binary")
	}
	checks["install-success-profile-local"] = true
	counts["installCallsR1"] = len(bunInstallCalls(iso.workspace))

	_, err = installOnce(ctx, client)
	note("install-again-refused", err)
	if err == nil || !strings.Contains(err.Error(), "already installed") {
		return fmt.Errorf("second install was not refused: %v", err)
	}
	if len(bunInstallCalls(iso.workspace)) != 1 {
		return errors.New("refused reinstall invoked bun")
	}
	checks["install-again-refused"] = true

	enginesBefore := engineDirCount(iso.round)
	probeRaw, err := client.Request(ctx, map[string]any{"type": "probeProvider", "binaryOverride": binary, "probeVersion": true, "discoverModels": true})
	note("pool-reacquire-after-reset", err)
	if err != nil {
		return fmt.Errorf("pool probe after reset failed: %w", err)
	}
	var probe struct {
		Probe struct {
			Installed bool   `json:"installed"`
			Path      string `json:"path"`
		} `json:"probe"`
		Version *string `json:"version"`
	}
	if json.Unmarshal(probeRaw, &probe) != nil || !probe.Probe.Installed || probe.Probe.Path != binary {
		return fmt.Errorf("pool probe %+v", probe)
	}
	if probe.Version == nil || *probe.Version != version {
		return fmt.Errorf("pool probe version %+v", probe.Version)
	}
	if engineDirCount(iso.round) != enginesBefore+1 {
		return errors.New("pool reset did not start a fresh engine on re-acquire")
	}
	checks["pool-reset-reacquire"] = true

	sid, rid := uuid.NewString(), uuid.NewString()
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": map[string]any{"binary": binary, "cwd": filepath.Join(iso.workspace, "main"), "mode": "ask", "interactionMode": "code", "model": "fixture/pool"}}); err != nil {
		return fmt.Errorf("runtime start on managed binary failed: %w", err)
	}
	_, err = installOnce(ctx, client)
	note("active-runtime-blocks-management", err)
	if err == nil || !strings.Contains(err.Error(), "close task runtimes") {
		_ = sid
		return fmt.Errorf("active runtime did not block management: %v", err)
	}
	checks["active-runtime-blocks-management"] = true
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "closeSession"}); err != nil {
		return fmt.Errorf("close session: %w", err)
	}
	return nil
}

func roundSentinelRefusal(ctx context.Context, daemonBinary string, iso isoRound, name, sentinel string, checks map[string]bool, note func(string, error), counts map[string]any) error {
	if err := os.WriteFile(filepath.Join(iso.workspace, sentinel), []byte("1"), 0o600); err != nil {
		return err
	}
	token := uuid.NewString()
	child, address, err := launchDaemon(ctx, daemonBinary, iso, token)
	if err != nil {
		return err
	}
	defer child.Close()
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()

	_, err = installOnce(ctx, client)
	note(name, err)
	if err == nil || !strings.Contains(err.Error(), "installation failed") {
		return fmt.Errorf("%s was not refused: %v", name, err)
	}
	if selectedBinary(iso.dataDir) != "" {
		return fmt.Errorf("%s changed configuration", name)
	}
	if len(bunInstallCalls(iso.workspace)) != 1 {
		return fmt.Errorf("%s ran bun %d times", name, len(bunInstallCalls(iso.workspace)))
	}
	homeBin := filepath.Join(iso.dataDir, "opencode-pm", "bun", "bin", "opencode.exe")
	if _, statErr := os.Stat(homeBin); statErr == nil && name == "installer-failure" {
		return errors.New("failed install left a managed binary")
	}
	checks[name+"-config-unchanged"] = true
	counts[name+"Calls"] = len(bunInstallCalls(iso.workspace))
	return nil
}

func roundCASRace(ctx context.Context, daemonBinary string, iso isoRound, checks map[string]bool, note func(string, error), counts map[string]any) error {
	token := uuid.NewString()
	child, address, err := launchDaemon(ctx, daemonBinary, iso, token)
	if err != nil {
		return err
	}
	defer child.Close()
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()

	done := make(chan error, 1)
	go func() { _, err := installOnce(ctx, client); done <- err }()
	time.Sleep(800 * time.Millisecond)
	external, _ := json.Marshal(map[string]string{"network_proxy_url": "http://127.0.0.1:7897", "opencode_binary_path": `C:\external\opencode.exe`})
	if err := os.WriteFile(filepath.Join(iso.dataDir, "daemon.json"), external, 0o600); err != nil {
		return err
	}
	err = <-done
	note("cas-race-install", err)
	if err == nil || !strings.Contains(err.Error(), "changed during installation") {
		return fmt.Errorf("external CAS change was not detected: %v", err)
	}
	if selectedBinary(iso.dataDir) != `C:\external\opencode.exe` {
		return fmt.Errorf("CAS race overwrote external selection: %q", selectedBinary(iso.dataDir))
	}
	checks["cas-external-change-preserved"] = true
	return nil
}

func roundSlowBudget(ctx context.Context, daemonBinary string, iso isoRound, checks map[string]bool, note func(string, error), counts map[string]any, versions map[string]any) error {
	if err := os.WriteFile(filepath.Join(iso.workspace, "fixture-slow-install"), []byte("1"), 0o600); err != nil {
		return err
	}
	token := uuid.NewString()
	child, address, err := launchDaemon(ctx, daemonBinary, iso, token)
	if err != nil {
		return err
	}
	defer child.Close()
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()

	start := time.Now()
	raw, err := installOnce(ctx, client)
	elapsed := time.Since(start)
	note("slow-install-budget", err)
	if err != nil {
		return fmt.Errorf("slow install failed: %w", err)
	}
	if elapsed < 30*time.Second {
		return fmt.Errorf("slow install finished in %v, budget not proven", elapsed)
	}
	_, binary, err := parseInstalled(raw)
	if err != nil {
		return err
	}
	version, err := requestedVersion(iso.workspace)
	if err != nil {
		return err
	}
	versions["slowInstallVersion"] = version
	if err := assertManagedLayout(iso, binary, version, counts); err != nil {
		return fmt.Errorf("slow install layout: %w", err)
	}
	counts["slowInstallSeconds"] = int(elapsed.Seconds())
	checks["budget-300s-tolerates-slow-install"] = true
	return nil
}

func updateOnce(ctx context.Context, client *daemon.Client, path, version string) (json.RawMessage, error) {
	return client.Request(ctx, map[string]any{"type": "updateOpenCode", "path": path, "version": version})
}

func roundUpdateRefusals(ctx context.Context, daemonBinary string, iso isoRound, checks map[string]bool, note func(string, error), counts map[string]any, versions map[string]any) error {
	token := uuid.NewString()
	child, address, err := launchDaemon(ctx, daemonBinary, iso, token)
	if err != nil {
		return err
	}
	defer child.Close()
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()

	raw, err := installOnce(ctx, client)
	note("update-round-install", err)
	if err != nil {
		return fmt.Errorf("update round install failed: %w", err)
	}
	_, selected, err := parseInstalled(raw)
	if err != nil {
		return err
	}
	latest, err := requestedVersion(iso.workspace)
	if err != nil {
		return err
	}
	versions["updateLatest"] = latest
	configIs := func() string { return selectedBinary(iso.dataDir) }

	_, err = updateOnce(ctx, client, filepath.Join(iso.fixtureDir, "bun.exe"), latest)
	note("update-external-path", err)
	if err == nil || !strings.Contains(err.Error(), "only this isolated profile") {
		return fmt.Errorf("external update path was not refused: %v", err)
	}
	checks["update-external-path-refused"] = true

	_, err = updateOnce(ctx, client, filepath.Join(iso.round, "does-not-exist.exe"), latest)
	note("update-missing-path", err)
	if err == nil || !strings.Contains(err.Error(), "update target unavailable") {
		return fmt.Errorf("missing update path was not refused: %v", err)
	}
	checks["update-missing-path-refused"] = true

	_, err = updateOnce(ctx, client, selected, "0.0.0")
	note("update-stale-version", err)
	if err == nil || !strings.Contains(err.Error(), "no longer matches") {
		return fmt.Errorf("stale update version was not refused: %v", err)
	}
	checks["update-stale-version-refused"] = true

	_, err = updateOnce(ctx, client, selected, latest)
	note("update-not-newer", err)
	if err == nil || !strings.Contains(err.Error(), "not newer") {
		return fmt.Errorf("current update version was not refused: %v", err)
	}
	checks["update-not-newer-refused"] = true

	unownedDir := filepath.Join(iso.round, "unowned")
	if err := os.MkdirAll(unownedDir, 0o700); err != nil {
		return err
	}
	installedRaw, err := os.ReadFile(selected)
	if err != nil {
		return err
	}
	unowned := filepath.Join(unownedDir, "opencode.exe")
	if err := os.WriteFile(unowned, installedRaw, 0o700); err != nil {
		return err
	}
	if _, err := client.Request(ctx, map[string]any{"type": "updateSetting", "key": "opencode_binary_path", "value": unowned}); err != nil {
		return fmt.Errorf("point selection at unowned copy: %w", err)
	}
	_, err = updateOnce(ctx, client, unowned, latest)
	note("update-unowned-binary", err)
	if err == nil || !strings.Contains(err.Error(), "only this isolated profile") {
		return fmt.Errorf("unowned update target was not refused: %v", err)
	}
	checks["update-unowned-refused"] = true

	missing := filepath.Join(iso.round, "gone", "opencode.exe")
	if _, err := client.Request(ctx, map[string]any{"type": "updateSetting", "key": "opencode_binary_path", "value": missing}); err != nil {
		return fmt.Errorf("point selection at missing binary: %w", err)
	}
	_, err = updateOnce(ctx, client, missing, latest)
	note("update-missing-selected", err)
	if err == nil || !strings.Contains(err.Error(), "selected OpenCode executable unavailable") {
		return fmt.Errorf("missing selected binary was not refused: %v", err)
	}
	checks["update-missing-selected-refused"] = true

	if _, err := client.Request(ctx, map[string]any{"type": "updateSetting", "key": "opencode_binary_path", "value": selected}); err != nil {
		return fmt.Errorf("restore selection: %w", err)
	}
	if configIs() != selected {
		return fmt.Errorf("refusals changed configuration: %q", configIs())
	}
	checks["update-refusals-keep-config"] = true
	return nil
}

func olderThan(latest string) (string, error) {
	major := 0
	parts := strings.SplitN(latest, ".", 2)
	if len(parts) == 0 {
		return "", errors.New("empty latest version")
	}
	for _, digit := range parts[0] {
		if digit < '0' || digit > '9' {
			return "", errors.New("non-numeric latest major")
		}
		major = major*10 + int(digit-'0')
	}
	if major > 2 {
		return "2.0.0", nil
	}
	if major == 2 && latest != "2.0.0" {
		return "2.0.0", nil
	}
	if major == 1 {
		return "0.9.0", nil
	}
	return "", errors.New("no safe older version below latest")
}

func roundUpdateSuccess(ctx context.Context, daemonBinary string, iso isoRound, checks map[string]bool, note func(string, error), counts map[string]any, versions map[string]any) error {
	token := uuid.NewString()
	child, address, err := launchDaemon(ctx, daemonBinary, iso, token)
	if err != nil {
		return err
	}
	defer child.Close()
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()

	raw, err := installOnce(ctx, client)
	note("update-success-setup-install", err)
	if err != nil {
		return fmt.Errorf("setup install failed: %w", err)
	}
	_, selected, err := parseInstalled(raw)
	if err != nil {
		return err
	}
	latest, err := requestedVersion(iso.workspace)
	if err != nil {
		return err
	}
	old, err := olderThan(latest)
	if err != nil {
		return err
	}
	versions["updateOld"] = old
	versions["updateLatest"] = latest
	managedDir := filepath.Join(iso.dataDir, "opencode-pm", "bun")
	if err := os.WriteFile(filepath.Join(managedDir, "bin", "opencode.version"), []byte(old+"\n"), 0o600); err != nil {
		return err
	}
	packageJSON, _ := json.Marshal(map[string]any{"name": "@opencode/cli", "version": old})
	if err := os.WriteFile(filepath.Join(managedDir, "install", "global", "node_modules", "@opencode", "cli", "package.json"), packageJSON, 0o600); err != nil {
		return err
	}
	before := selectedBinary(iso.dataDir)
	if before != selected {
		return errors.New("setup install selection mismatch")
	}

	installsBefore := len(bunInstallCalls(iso.workspace))
	ackRaw, err := updateOnce(ctx, client, selected, latest)
	note("update-success-call", err)
	if err != nil {
		return fmt.Errorf("update to latest failed: %w", err)
	}
	var ackBody struct {
		Type string `json:"type"`
	}
	if json.Unmarshal(ackRaw, &ackBody) != nil || ackBody.Type != "ack" {
		return fmt.Errorf("update reply was not ack: %s", strings.TrimSpace(string(ackRaw)))
	}
	checks["update-success-ack"] = true
	if delta := len(bunInstallCalls(iso.workspace)) - installsBefore; delta != 1 {
		return fmt.Errorf("update ran %d installs", delta)
	}
	checks["update-success-single-install"] = true
	if selectedBinary(iso.dataDir) != selected {
		return fmt.Errorf("update changed selection: %q", selectedBinary(iso.dataDir))
	}
	checks["update-success-config-stable"] = true

	enginesBefore := engineDirCount(iso.round)
	probeRaw, err := client.Request(ctx, map[string]any{"type": "probeProvider", "binaryOverride": selected, "probeVersion": true, "discoverModels": true})
	note("update-success-probe", err)
	if err != nil {
		return fmt.Errorf("post-update probe failed: %w", err)
	}
	var probe struct {
		Probe struct {
			Installed bool   `json:"installed"`
			Path      string `json:"path"`
		} `json:"probe"`
		Version *string `json:"version"`
	}
	if json.Unmarshal(probeRaw, &probe) != nil || !probe.Probe.Installed || probe.Probe.Path != selected {
		return fmt.Errorf("post-update probe %+v", probe)
	}
	if probe.Version == nil || *probe.Version != latest {
		return fmt.Errorf("post-update version %+v, want %s", probe.Version, latest)
	}
	if engineDirCount(iso.round) != enginesBefore+1 {
		return errors.New("post-update pool did not start a fresh engine")
	}
	checks["update-success-version-latest"] = true
	checks["update-success-pool-reacquire"] = true
	return nil
}

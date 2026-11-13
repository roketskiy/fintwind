package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

type statusDaemon struct {
	root      string
	workspace string
	logs      []string
	address   string
	token     string
	child     *ownedprocess.Owned
}

type statusReport struct {
	Checks    map[string]bool `json:"checks"`
	Passed    bool            `json:"passed"`
	Command   string          `json:"command"`
	Daemon    string          `json:"daemonBinary"`
	DaemonSHA string          `json:"daemonSha256"`
	Runner    string          `json:"runnerBinary"`
	RunnerSHA string          `json:"runnerSha256"`
	Round     string          `json:"roundDir"`
	Logs      []string        `json:"childLogDirectories"`
	Wire      map[string]any  `json:"wire"`
	Uncovered []string        `json:"uncovered"`
	Error     string          `json:"error,omitempty"`
}

type gitStatusWire struct {
	Type   string `json:"type"`
	Status *struct {
		Branch   *string `json:"branch"`
		Upstream *string `json:"upstream"`
		Ahead    uint64  `json:"ahead"`
		Behind   uint64  `json:"behind"`
		Entries  []struct {
			Path           string  `json:"path"`
			IndexStatus    string  `json:"index_status"`
			WorktreeStatus string  `json:"worktree_status"`
			OriginPath     *string `json:"origin_path"`
		} `json:"entries"`
	} `json:"status"`
}

type statusProbeResult struct {
	raw []byte
	err error
}

type heldGitQuery struct {
	client    *daemon.Client
	cwd       string
	queryID   string
	started   string
	heartbeat string
}

func runStatusDaemon(daemonBinary string) (err error) {
	if daemonBinary != "" && !filepath.IsAbs(daemonBinary) {
		daemonBinary, err = filepath.Abs(daemonBinary)
		if err != nil {
			return fmt.Errorf("resolve daemon binary path: %w", err)
		}
	}
	if !filepath.IsAbs(daemonBinary) {
		return errors.New("absolute daemon binary required for status wire E2E")
	}
	for _, key := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "FINTWIND_AGENT_REAL_PROVIDER"} {
		if strings.TrimSpace(os.Getenv(key)) != "" {
			return fmt.Errorf("credential variable %s must be unset", key)
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 180*time.Second)
	defer cancel()
	root, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	report := statusReport{
		Checks: map[string]bool{}, Wire: map[string]any{}, Round: root, Daemon: daemonBinary,
		Uncovered: []string{"Read status during a running workspace mutation is not exercised without a deterministic safe fixture", "Copy status records (C) are not guaranteed by local Git status configuration; rename (R) is covered"},
	}
	report.Runner, err = os.Executable()
	if err != nil {
		return err
	}
	report.RunnerSHA, err = fileSHA(report.Runner)
	if err != nil {
		return err
	}
	report.DaemonSHA, err = fileSHA(daemonBinary)
	if err != nil {
		return err
	}
	report.Command = report.Runner + " --daemon " + daemonBinary
	defer func() {
		report.Passed = err == nil
		if err != nil {
			report.Error = err.Error()
		}
		raw, _ := json.MarshalIndent(report, "", "  ")
		_ = os.WriteFile(filepath.Join(root, "git-status-result.json"), raw, 0o600)
		fmt.Println("[REPORT]", filepath.Join(root, "git-status-result.json"))
	}()

	workspace := filepath.Join(root, "workspace", "git-status")
	if err := os.MkdirAll(workspace, 0o700); err != nil {
		return err
	}
	git, err := exec.LookPath("git")
	if err != nil {
		return err
	}
	bare := filepath.Join(workspace, "origin.git")
	repo := filepath.Join(workspace, "repo")
	peer := filepath.Join(workspace, "peer")
	unborn := filepath.Join(workspace, "unborn")
	nonrepo := filepath.Join(workspace, "not-a-repository")
	fakeGitDir := filepath.Join(root, "fake-git")
	if err := os.MkdirAll(fakeGitDir, 0o700); err != nil {
		return err
	}
	if err := copyFile(report.Runner, filepath.Join(fakeGitDir, "git.exe")); err != nil {
		return err
	}
	fakeConfigPath := filepath.Join(fakeGitDir, "git-wrapper.json")
	fakeConfig := fakeGitConfig{RealGit: git, RoundRoot: root, Mode: "pass", TargetCwd: repo}
	if err := writeFakeGitConfig(fakeConfigPath, fakeConfig); err != nil {
		return err
	}
	daemonPath := fakeGitDir + string(os.PathListSeparator) + os.Getenv("PATH")
	inst, err := startStatusDaemon(ctx, daemonBinary, root, workspace, daemonPath)
	if err != nil {
		return err
	}
	defer func() {
		report.Logs = append(report.Logs, inst.logs...)
		if inst.child != nil {
			_ = inst.child.Close()
		}
	}()
	client, err := daemon.Dial(ctx, inst.address, inst.token)
	if err != nil {
		return err
	}
	defer client.Close()

	for _, path := range []string{repo, unborn, nonrepo} {
		if err := os.MkdirAll(path, 0o700); err != nil {
			return err
		}
	}
	if err := runOwnedGit(ctx, git, root, workspace, "init", "--bare", "--initial-branch=main", bare); err != nil {
		return err
	}
	if err := runOwnedGit(ctx, git, root, repo, "init", "--initial-branch=main"); err != nil {
		return err
	}
	for _, args := range [][]string{{"config", "user.name", "Git Status E2E"}, {"config", "user.email", "git-status-e2e@invalid"}} {
		if err := runOwnedGit(ctx, git, root, repo, args...); err != nil {
			return err
		}
	}
	if err := writeFixtureFile(repo, "base.txt", "base\n"); err != nil {
		return err
	}
	for _, args := range [][]string{{"add", "base.txt"}, {"commit", "-m", "base"}, {"remote", "add", "origin", bare}, {"push", "-u", "origin", "main"}} {
		if err := runOwnedGit(ctx, git, root, repo, args...); err != nil {
			return err
		}
	}
	if err := writeFixtureFile(repo, "local.txt", "local ahead\n"); err != nil {
		return err
	}
	for _, args := range [][]string{{"add", "local.txt"}, {"commit", "-m", "local ahead"}} {
		if err := runOwnedGit(ctx, git, root, repo, args...); err != nil {
			return err
		}
	}
	ahead, rawAhead, err := requestGitStatus(ctx, client, repo, "inspectStatus", nil)
	if err != nil {
		return err
	}
	report.Wire["ahead"] = json.RawMessage(rawAhead)
	if err := requireBranchCounts(ahead, "main", "origin/main", 1, 0); err != nil {
		return fmt.Errorf("ahead status: %w", err)
	}
	report.Checks["real-daemon-wire-ahead"] = true

	if err := runOwnedGit(ctx, git, root, workspace, "clone", "--branch", "main", bare, peer); err != nil {
		return err
	}
	for _, args := range [][]string{{"config", "user.name", "Git Status E2E Peer"}, {"config", "user.email", "git-status-peer@invalid"}} {
		if err := runOwnedGit(ctx, git, root, peer, args...); err != nil {
			return err
		}
	}
	if err := writeFixtureFile(peer, "peer.txt", "peer update\n"); err != nil {
		return err
	}
	for _, args := range [][]string{{"add", "peer.txt"}, {"commit", "-m", "peer update"}, {"push", "origin", "main"}} {
		if err := runOwnedGit(ctx, git, root, peer, args...); err != nil {
			return err
		}
	}
	if err := runOwnedGit(ctx, git, root, repo, "fetch", "origin"); err != nil {
		return err
	}
	diverged, rawDiverged, err := requestGitStatus(ctx, client, repo, "inspectStatus", nil)
	if err != nil {
		return err
	}
	report.Wire["diverged"] = json.RawMessage(rawDiverged)
	if err := requireBranchCounts(diverged, "main", "origin/main", 1, 1); err != nil {
		return fmt.Errorf("diverged status: %w", err)
	}
	report.Checks["real-daemon-wire-diverged"] = true
	if err := runOwnedGit(ctx, git, root, repo, "reset", "--hard", "origin/main"); err != nil {
		return err
	}
	if err := writeFixtureFile(peer, "peer-two.txt", "second peer update\n"); err != nil {
		return err
	}
	for _, args := range [][]string{{"add", "peer-two.txt"}, {"commit", "-m", "second peer update"}, {"push", "origin", "main"}} {
		if err := runOwnedGit(ctx, git, root, peer, args...); err != nil {
			return err
		}
	}
	if err := runOwnedGit(ctx, git, root, repo, "fetch", "origin"); err != nil {
		return err
	}
	behind, rawBehind, err := requestGitStatus(ctx, client, repo, "inspectStatus", nil)
	if err != nil {
		return err
	}
	report.Wire["behind"] = json.RawMessage(rawBehind)
	if err := requireBranchCounts(behind, "main", "origin/main", 0, 1); err != nil {
		return fmt.Errorf("behind status: %w", err)
	}
	report.Checks["real-daemon-wire-behind"] = true

	untrackedPath := filepath.Join(repo, "nested", "deeper", "new.txt")
	if err := os.MkdirAll(filepath.Dir(untrackedPath), 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(untrackedPath, []byte("nested untracked\n"), 0o600); err != nil {
		return err
	}
	if err := writeFixtureFile(repo, "base.txt", "tracked change\n"); err != nil {
		return err
	}
	all, rawAll, err := requestGitStatus(ctx, client, repo, "inspectStatus", nil)
	if err != nil {
		return err
	}
	report.Wire["allFiles"] = json.RawMessage(rawAll)
	if !hasEntry(all, "nested/deeper/new.txt", "?", "?") || !hasEntry(all, "base.txt", " ", "M") {
		return errors.New("inspectStatus did not return nested untracked and tracked modification entries")
	}
	tracked, rawTracked, err := requestGitStatus(ctx, client, repo, "inspectTrackedStatus", nil)
	if err != nil {
		return err
	}
	report.Wire["trackedOnly"] = json.RawMessage(rawTracked)
	if hasPath(tracked, "nested/deeper/new.txt") || !hasEntry(tracked, "base.txt", " ", "M") {
		return errors.New("inspectTrackedStatus included untracked paths or omitted tracked change")
	}
	report.Checks["untracked-nested-and-tracked-filter"] = true

	if err := runOwnedGit(ctx, git, root, repo, "add", "-A"); err != nil {
		return err
	}
	if err := runOwnedGit(ctx, git, root, repo, "commit", "-m", "tracked files for rename"); err != nil {
		return err
	}
	if err := runOwnedGit(ctx, git, root, repo, "mv", "base.txt", "renamed.txt"); err != nil {
		return err
	}
	rename, rawRename, err := requestGitStatus(ctx, client, repo, "inspectStatus", nil)
	if err != nil {
		return err
	}
	report.Wire["rename"] = json.RawMessage(rawRename)
	if !hasRename(rename, "renamed.txt", "base.txt") {
		return errors.New("rename entry lost destination or origin_path")
	}
	report.Checks["rename-two-field-porcelain-record"] = true

	if err := runOwnedGit(ctx, git, root, unborn, "init", "--initial-branch=unborn"); err != nil {
		return err
	}
	unbornStatus, rawUnborn, err := requestGitStatus(ctx, client, unborn, "inspectStatus", nil)
	if err != nil {
		return err
	}
	report.Wire["unborn"] = json.RawMessage(rawUnborn)
	if unbornStatus.Status == nil || unbornStatus.Status.Branch == nil || *unbornStatus.Status.Branch != "unborn" || unbornStatus.Status.Upstream != nil || unbornStatus.Status.Ahead != 0 || unbornStatus.Status.Behind != 0 {
		return errors.New("unborn branch header was not parsed as branch with null upstream and zero counts")
	}
	report.Checks["unborn-branch-header"] = true

	if err := runOwnedGit(ctx, git, root, repo, "checkout", "--detach", "HEAD"); err != nil {
		return err
	}
	detached, rawDetached, err := requestGitStatus(ctx, client, repo, "inspectStatus", nil)
	if err != nil {
		return err
	}
	report.Wire["detached"] = json.RawMessage(rawDetached)
	if detached.Status == nil || detached.Status.Branch != nil || detached.Status.Upstream != nil || detached.Status.Ahead != 0 || detached.Status.Behind != 0 {
		return errors.New("detached HEAD must report null branch/upstream and zero counts")
	}
	report.Checks["detached-head-null-branch"] = true

	status, rawNonRepo, err := requestGitStatus(ctx, client, nonrepo, "inspectStatus", nil)
	if err != nil {
		return fmt.Errorf("non-repository status: %w", err)
	}
	report.Wire["nonRepository"] = json.RawMessage(rawNonRepo)
	if status.Status != nil {
		return errors.New("non-repository status must be null")
	}
	report.Checks["non-repository-null-status"] = true

	queryID := uuid.NewString()
	if _, _, err := requestWorkspace(ctx, client, map[string]any{"type": "cancelGitQuery", "queryId": queryID}); err != nil {
		return fmt.Errorf("pre-cancel query: %w", err)
	}
	_, rawCancelled, err := requestGitStatus(ctx, client, repo, "inspectStatus", map[string]any{"queryId": queryID})
	if err == nil || !strings.Contains(err.Error(), "Git query cancelled") {
		return fmt.Errorf("pre-cancelled query was not rejected before status work: err=%v response=%s", err, rawCancelled)
	}
	report.Wire["preCancelledQuery"] = map[string]any{"queryId": queryID, "error": err.Error()}
	report.Checks["pre-cancelled-query-rejected"] = true

	fakeConfig.Mode = "exit"
	fakeConfig.ExitCode = 128
	fakeConfig.Diagnostic = "fatal: unsafe fake Git repository configuration"
	if err := writeFakeGitConfig(fakeConfigPath, fakeConfig); err != nil {
		return err
	}
	_, _, fake128Err := requestGitStatus(ctx, client, repo, "inspectStatus", nil)
	if fake128Err == nil || !strings.Contains(fake128Err.Error(), "git exited with code 128") || !strings.Contains(fake128Err.Error(), fakeConfig.Diagnostic) {
		return fmt.Errorf("non-repository classifier hid or changed unrelated code-128 failure: %v", fake128Err)
	}
	report.Wire["fakeCode128UnsafeDiagnostic"] = map[string]any{"exitCode": fakeConfig.ExitCode, "diagnostic": fakeConfig.Diagnostic, "error": fake128Err.Error()}
	report.Checks["code128-non-repository-diagnostic-is-specific"] = true

	fakeConfig.ExitCode = 126
	fakeConfig.Diagnostic = "fatal: fake Git permission denied"
	if err := writeFakeGitConfig(fakeConfigPath, fakeConfig); err != nil {
		return err
	}
	_, _, fake126Err := requestGitStatus(ctx, client, repo, "inspectStatus", nil)
	if fake126Err == nil || !strings.Contains(fake126Err.Error(), "git exited with code 126") || !strings.Contains(fake126Err.Error(), fakeConfig.Diagnostic) {
		return fmt.Errorf("Git execution failure was hidden as a successful status: %v", fake126Err)
	}
	report.Wire["fakeCode126ExecutionFailure"] = map[string]any{"exitCode": fakeConfig.ExitCode, "diagnostic": fakeConfig.Diagnostic, "error": fake126Err.Error()}
	report.Checks["git-execution-failure-propagates"] = true

	fakeConfig.Mode = "hold"
	fakeConfig.ExitCode = 0
	fakeConfig.Diagnostic = ""
	fakeConfig.StartedPath = filepath.Join(fakeGitDir, "probe-started")
	fakeConfig.ReleasePath = filepath.Join(fakeGitDir, "probe-release")
	fakeConfig.Heartbeat = filepath.Join(fakeGitDir, "probe-heartbeat")
	_ = os.Remove(fakeConfig.StartedPath)
	_ = os.Remove(fakeConfig.ReleasePath)
	_ = os.Remove(fakeConfig.Heartbeat)
	if err := writeFakeGitConfig(fakeConfigPath, fakeConfig); err != nil {
		return err
	}
	probeQueryID := uuid.NewString()
	probeCtx, cancelProbe := context.WithTimeout(ctx, 60*time.Second)
	defer cancelProbe()
	probeDone := make(chan statusProbeResult, 1)
	probeStartedAt := time.Now()
	go func() {
		_, raw, requestErr := requestGitStatus(probeCtx, client, repo, "inspectStatus", map[string]any{"queryId": probeQueryID})
		probeDone <- statusProbeResult{raw: raw, err: requestErr}
	}()
	if err := waitForFile(ctx, fakeConfig.StartedPath, 5*time.Second); err != nil {
		_ = os.WriteFile(fakeConfig.ReleasePath, []byte("release\n"), 0o600)
		return fmt.Errorf("fake Git rev-parse probe did not start: %w", err)
	}
	heartbeatAtStart, err := waitHeartbeat(ctx, fakeConfig.Heartbeat, 2*time.Second)
	if err != nil {
		_ = os.WriteFile(fakeConfig.ReleasePath, []byte("release\n"), 0o600)
		return fmt.Errorf("fake Git heartbeat did not start: %w", err)
	}
	cancelClient, err := daemon.Dial(ctx, inst.address, inst.token)
	if err != nil {
		_ = os.WriteFile(fakeConfig.ReleasePath, []byte("release\n"), 0o600)
		return fmt.Errorf("open independent cancellation wire: %w", err)
	}
	defer cancelClient.Close()
	cancelStartedAt := time.Now()
	cancelRPCctx, cancelRPC := context.WithTimeout(ctx, 3*time.Second)
	cancelPayload, _, err := requestWorkspace(cancelRPCctx, cancelClient, map[string]any{"type": "cancelGitQuery", "queryId": probeQueryID})
	cancelRPC()
	if err != nil {
		_ = os.WriteFile(fakeConfig.ReleasePath, []byte("release\n"), 0o600)
		return fmt.Errorf("cancel active status query: %w", err)
	}
	if !strings.Contains(string(cancelPayload), "ack") {
		_ = os.WriteFile(fakeConfig.ReleasePath, []byte("release\n"), 0o600)
		return fmt.Errorf("cancel active status query did not acknowledge: %s", cancelPayload)
	}
	cancelAckMs := time.Since(cancelStartedAt).Milliseconds()
	heartbeatAtCancel, err := readHeartbeat(fakeConfig.Heartbeat)
	if err != nil {
		_ = os.WriteFile(fakeConfig.ReleasePath, []byte("release\n"), 0o600)
		return err
	}
	var canceledProbe statusProbeResult
	select {
	case canceledProbe = <-probeDone:
	case <-time.After(5 * time.Second):
		_ = os.WriteFile(fakeConfig.ReleasePath, []byte("release\n"), 0o600)
		select {
		case canceledProbe = <-probeDone:
		case <-time.After(3 * time.Second):
			return errors.New("cancelled Git status did not return after releasing the owned fake probe")
		}
		return errors.New("active query cancellation did not promptly stop the fake Git probe")
	}
	if canceledProbe.err == nil || !(strings.Contains(strings.ToLower(canceledProbe.err.Error()), "canceled") || strings.Contains(strings.ToLower(canceledProbe.err.Error()), "cancelled")) {
		return fmt.Errorf("active status query did not report cancellation: err=%v response=%s", canceledProbe.err, canceledProbe.raw)
	}
	heartbeatAfterReturn, err := readHeartbeat(fakeConfig.Heartbeat)
	if err != nil {
		return err
	}
	time.Sleep(300 * time.Millisecond)
	heartbeatAfterCancel, err := readHeartbeat(fakeConfig.Heartbeat)
	if err != nil {
		return err
	}
	if heartbeatAfterReturn != heartbeatAfterCancel {
		return fmt.Errorf("fake Git descendant heartbeat continued after cancelled status returned: after-return=%d later=%d", heartbeatAfterReturn, heartbeatAfterCancel)
	}
	if cancelAckMs > 2500 {
		return fmt.Errorf("cancelGitQuery acknowledgement took too long: %dms", cancelAckMs)
	}
	report.Wire["activeCancelledQuery"] = map[string]any{"queryId": probeQueryID, "started": true, "heartbeatBeforeCancel": heartbeatAtStart, "heartbeatAtCancelAck": heartbeatAtCancel, "heartbeatAfterRequestReturn": heartbeatAfterReturn, "heartbeatAfterStabilityWindow": heartbeatAfterCancel, "cancelAckMs": cancelAckMs, "responseError": canceledProbe.err.Error(), "elapsedMs": time.Since(probeStartedAt).Milliseconds(), "killByPid": false}
	report.Checks["active-query-cancellation-stops-owned-fake-git-job"] = true

	for _, queryKind := range []string{"inspectStatus", "inspectTrackedStatus"} {
		wire, checks, err := runSameWebSocketCancellation(ctx, client, repo, fakeConfigPath, fakeConfig, fakeGitDir, queryKind)
		if err != nil {
			return err
		}
		report.Wire["sameWebSocket_"+queryKind] = wire
		for name, passed := range checks {
			report.Checks[name] = passed
		}
	}
	if err := runQueryLifecycleChecks(ctx, inst, fakeConfigPath, fakeConfig, fakeGitDir, workspace, root, &report); err != nil {
		return err
	}
	return nil
}

func runSameWebSocketCancellation(parent context.Context, client *daemon.Client, repo, configPath string, config fakeGitConfig, fakeGitDir, queryKind string) (map[string]any, map[string]bool, error) {
	label := strings.ToLower(queryKind)
	config.StartedPath = filepath.Join(fakeGitDir, label+"-same-ws-started")
	config.ReleasePath = filepath.Join(fakeGitDir, label+"-same-ws-release")
	config.Heartbeat = filepath.Join(fakeGitDir, label+"-same-ws-heartbeat")
	_ = os.Remove(config.StartedPath)
	_ = os.Remove(config.ReleasePath)
	_ = os.Remove(config.Heartbeat)
	if err := writeFakeGitConfig(configPath, config); err != nil {
		return nil, nil, err
	}
	ctx, cancel := context.WithTimeout(parent, 30*time.Second)
	defer cancel()
	queryID := uuid.NewString()
	done := make(chan statusProbeResult, 1)
	startedAt := time.Now()
	go func() {
		_, raw, err := requestGitStatus(ctx, client, repo, queryKind, map[string]any{"queryId": queryID})
		done <- statusProbeResult{raw: raw, err: err}
	}()
	if err := waitForFile(ctx, config.StartedPath, 5*time.Second); err != nil {
		_ = os.WriteFile(config.ReleasePath, []byte("release\n"), 0o600)
		return nil, nil, fmt.Errorf("%s same-WebSocket fake Git probe did not start: %w", queryKind, err)
	}
	heartbeatStart, err := waitHeartbeat(ctx, config.Heartbeat, 2*time.Second)
	if err != nil {
		_ = os.WriteFile(config.ReleasePath, []byte("release\n"), 0o600)
		return nil, nil, fmt.Errorf("%s same-WebSocket heartbeat did not start: %w", queryKind, err)
	}
	settingsStartedAt := time.Now()
	settingsCtx, cancelSettings := context.WithTimeout(ctx, 3*time.Second)
	settingsRaw, err := client.Request(settingsCtx, map[string]any{"type": "getSettings"})
	cancelSettings()
	settingsMs := time.Since(settingsStartedAt).Milliseconds()
	if err != nil {
		_ = os.WriteFile(config.ReleasePath, []byte("release\n"), 0o600)
		return nil, nil, fmt.Errorf("getSettings blocked by held same-WebSocket %s query: %w", queryKind, err)
	}
	if len(settingsRaw) == 0 || settingsMs > 2500 {
		_ = os.WriteFile(config.ReleasePath, []byte("release\n"), 0o600)
		return nil, nil, fmt.Errorf("getSettings response empty or slow during held %s query: %dms %s", queryKind, settingsMs, settingsRaw)
	}
	cancelStartedAt := time.Now()
	cancelCtx, cancelCancel := context.WithTimeout(ctx, 3*time.Second)
	cancelRaw, err := client.Request(cancelCtx, map[string]any{"type": "workspace", "operation": map[string]any{"type": "cancelGitQuery", "queryId": queryID}})
	cancelCancel()
	cancelMs := time.Since(cancelStartedAt).Milliseconds()
	if err != nil {
		_ = os.WriteFile(config.ReleasePath, []byte("release\n"), 0o600)
		return nil, nil, fmt.Errorf("same-WebSocket cancelGitQuery for %s did not acknowledge: %w", queryKind, err)
	}
	if !strings.Contains(string(cancelRaw), "ack") || cancelMs > 2500 {
		_ = os.WriteFile(config.ReleasePath, []byte("release\n"), 0o600)
		return nil, nil, fmt.Errorf("same-WebSocket cancelGitQuery for %s ack invalid or slow: %dms %s", queryKind, cancelMs, cancelRaw)
	}
	var result statusProbeResult
	select {
	case result = <-done:
	case <-ctx.Done():
		_ = os.WriteFile(config.ReleasePath, []byte("release\n"), 0o600)
		return nil, nil, fmt.Errorf("same-WebSocket %s status did not return after cancellation: %w", queryKind, ctx.Err())
	}
	if result.err == nil || !strings.Contains(strings.ToLower(result.err.Error()), "context canceled") {
		return nil, nil, fmt.Errorf("same-WebSocket %s status must report context canceled, got err=%v response=%s", queryKind, result.err, result.raw)
	}
	heartbeatReturn, err := readHeartbeat(config.Heartbeat)
	if err != nil {
		return nil, nil, err
	}
	time.Sleep(300 * time.Millisecond)
	heartbeatStable, err := readHeartbeat(config.Heartbeat)
	if err != nil {
		return nil, nil, err
	}
	if heartbeatReturn != heartbeatStable {
		return nil, nil, fmt.Errorf("%s fake-Git heartbeat continued after same-WebSocket cancellation: %d -> %d", queryKind, heartbeatReturn, heartbeatStable)
	}
	return map[string]any{"queryId": queryID, "started": true, "heartbeatAtStart": heartbeatStart, "heartbeatAfterRequestReturn": heartbeatReturn, "heartbeatAfterStabilityWindow": heartbeatStable, "settingsResponseMs": settingsMs, "settingsResponse": json.RawMessage(settingsRaw), "cancelAckMs": cancelMs, "cancelResponse": json.RawMessage(cancelRaw), "statusError": result.err.Error(), "elapsedMs": time.Since(startedAt).Milliseconds()}, map[string]bool{"same-websocket-" + label + "-get-settings-responsive": true, "same-websocket-" + label + "-cancel-ack-heartbeat-stop": true}, nil
}

func runQueryLifecycleChecks(parent context.Context, inst *statusDaemon, configPath string, config fakeGitConfig, fakeGitDir, workspace, root string, report *statusReport) error {
	ctx, cancel := context.WithTimeout(parent, 70*time.Second)
	defer cancel()
	const workerSlots = 8
	queries := make([]heldGitQuery, workerSlots)
	config.Mode = "hold"
	config.TargetCwd = ""
	config.StartedPath, config.ReleasePath, config.Heartbeat = "", "", ""
	config.Targets = make([]fakeGitTarget, workerSlots)
	clients := make([]*daemon.Client, workerSlots)
	for i := range queries {
		cwd := filepath.Join(workspace, fmt.Sprintf("saturated-query-%02d", i))
		if err := os.MkdirAll(cwd, 0o700); err != nil {
			return err
		}
		started := filepath.Join(fakeGitDir, fmt.Sprintf("saturation-%02d-started", i))
		release := filepath.Join(fakeGitDir, fmt.Sprintf("saturation-%02d-release", i))
		heartbeat := filepath.Join(fakeGitDir, fmt.Sprintf("saturation-%02d-heartbeat", i))
		for _, path := range []string{started, release, heartbeat} {
			_ = os.Remove(path)
		}
		config.Targets[i] = fakeGitTarget{Cwd: cwd, StartedPath: started, ReleasePath: release, Heartbeat: heartbeat}
		client, err := daemon.Dial(ctx, inst.address, inst.token)
		if err != nil {
			for _, client := range clients {
				if client != nil {
					client.Close()
				}
			}
			return fmt.Errorf("dial saturation client %d: %w", i, err)
		}
		clients[i] = client
		queries[i] = heldGitQuery{client: client, cwd: cwd, queryID: uuid.NewString(), started: started, heartbeat: heartbeat}
	}
	defer func() {
		for _, client := range clients {
			if client != nil {
				client.Close()
			}
		}
	}()
	if err := writeFakeGitConfig(configPath, config); err != nil {
		return err
	}
	if err := sendHeldStatus(ctx, queries[0]); err != nil {
		return err
	}
	if err := waitForFile(ctx, queries[0].started, 5*time.Second); err != nil {
		return fmt.Errorf("first saturation fake Git did not start: %w", err)
	}
	firstHeartbeat, err := waitHeartbeat(ctx, queries[0].heartbeat, 2*time.Second)
	if err != nil {
		return err
	}
	settingsBeforeFull := 0
	var fullSettingsErr string
	for i := 0; i < workerSlots; i++ {
		if i > 0 {
			if err := sendHeldStatus(ctx, queries[i]); err != nil {
				return err
			}
		}
		settingsCtx, stop := context.WithTimeout(ctx, 2*time.Second)
		_, settingsErr := queries[i].client.Request(settingsCtx, map[string]any{"type": "getSettings"})
		stop()
		if i < workerSlots-1 {
			if settingsErr != nil {
				return fmt.Errorf("getSettings failed before all %d Git query slots were filled (query %d): %w", workerSlots, i, settingsErr)
			}
			settingsBeforeFull++
		} else {
			if settingsErr == nil || !strings.Contains(strings.ToLower(settingsErr.Error()), "capacity reached") {
				return fmt.Errorf("getSettings on the eighth same-WebSocket query must fail fast at full capacity, got %v", settingsErr)
			}
			fullSettingsErr = settingsErr.Error()
		}
	}
	if err := waitForStartedCount(ctx, queries, 4, 5*time.Second); err != nil {
		return fmt.Errorf("four fake Git probes did not become active under the local Git gate: %w", err)
	}
	ninthClient, err := daemon.Dial(ctx, inst.address, inst.token)
	if err != nil {
		return err
	}
	defer ninthClient.Close()
	ninthID := uuid.NewString()
	ninthStarted := time.Now()
	ninthCtx, stopNinth := context.WithTimeout(ctx, 2500*time.Millisecond)
	_, ninthErr := ninthClient.Request(ninthCtx, map[string]any{"type": "workspace", "operation": map[string]any{"type": "inspectStatus", "cwd": filepath.Join(workspace, "saturated-ninth"), "queryId": ninthID}})
	stopNinth()
	ninthMs := time.Since(ninthStarted).Milliseconds()
	if ninthErr == nil || !strings.Contains(strings.ToLower(ninthErr.Error()), "capacity reached") || ninthMs > 2500 {
		return fmt.Errorf("ninth Git query must fail fast with capacity reached, elapsed=%dms err=%v", ninthMs, ninthErr)
	}
	ninthError := ninthErr.Error()
	fullSettingsAtStart := time.Now()
	fullSettingsCtx, stopFullSettings := context.WithTimeout(ctx, 2*time.Second)
	_, fullSettingsErr2 := queries[0].client.Request(fullSettingsCtx, map[string]any{"type": "getSettings"})
	stopFullSettings()
	fullSettingsAtMs := time.Since(fullSettingsAtStart).Milliseconds()
	if fullSettingsErr2 == nil || !strings.Contains(strings.ToLower(fullSettingsErr2.Error()), "capacity reached") || fullSettingsAtMs > 1500 {
		return fmt.Errorf("same-WebSocket getSettings at full capacity must fail fast before cancel, elapsed=%dms err=%v", fullSettingsAtMs, fullSettingsErr2)
	}
	cancelAck, cancelMs, err := cancelHeldGitQuery(ctx, queries[0].client, queries[0].queryID)
	if err != nil {
		return fmt.Errorf("same-WebSocket cancel at full capacity: %w", err)
	}
	firstHeartbeatAfterCancel, err := waitHeartbeatStable(ctx, queries[0].heartbeat, 4*time.Second, 250*time.Millisecond)
	if err != nil {
		return fmt.Errorf("first saturated query heartbeat did not stop: %w", err)
	}
	settingsAfterCancel, settingsAfterCancelMs, err := retrySettingsUntilAvailable(ctx, queries[0].client, 4*time.Second)
	if err != nil {
		return fmt.Errorf("getSettings did not recover after cancel freed a query slot: %w", err)
	}
	startedIndexes := make([]int, 0, 4)
	for i, query := range queries {
		if _, statErr := os.Stat(query.started); statErr == nil {
			startedIndexes = append(startedIndexes, i)
		}
	}
	if len(startedIndexes) < 4 {
		return fmt.Errorf("only %d fake-Git probes started while all eight query slots were occupied", len(startedIndexes))
	}
	disconnectedIndex, survivorIndex := startedIndexes[1], startedIndexes[2]
	survivorBefore, err := readHeartbeat(queries[survivorIndex].heartbeat)
	if err != nil {
		return err
	}
	queries[disconnectedIndex].client.Close()
	if _, err := waitHeartbeatStable(ctx, queries[disconnectedIndex].heartbeat, 4*time.Second, 250*time.Millisecond); err != nil {
		return fmt.Errorf("disconnect did not stop its owned query heartbeat: %w", err)
	}
	survivorAfter, err := waitHeartbeatAdvanced(ctx, queries[survivorIndex].heartbeat, survivorBefore, 3*time.Second)
	if err != nil {
		return fmt.Errorf("disconnect cancelled an unrelated held query: %w", err)
	}
	for i := range queries {
		if i == 0 || i == disconnectedIndex {
			continue
		}
		if _, _, err := cancelHeldGitQuery(ctx, queries[i].client, queries[i].queryID); err != nil {
			return fmt.Errorf("cancel remaining saturated query %d: %w", i, err)
		}
		if _, statErr := os.Stat(queries[i].started); statErr == nil {
			if _, err := waitHeartbeatStable(ctx, queries[i].heartbeat, 4*time.Second, 200*time.Millisecond); err != nil {
				return fmt.Errorf("query %d heartbeat remained after cleanup cancel: %w", i, err)
			}
		}
	}

	shutdownCwd := filepath.Join(workspace, "shutdown-held-query")
	if err := os.MkdirAll(shutdownCwd, 0o700); err != nil {
		return err
	}
	shutdownTarget := fakeGitTarget{Cwd: shutdownCwd, StartedPath: filepath.Join(fakeGitDir, "shutdown-started"), ReleasePath: filepath.Join(fakeGitDir, "shutdown-release"), Heartbeat: filepath.Join(fakeGitDir, "shutdown-heartbeat")}
	for _, path := range []string{shutdownTarget.StartedPath, shutdownTarget.ReleasePath, shutdownTarget.Heartbeat} {
		_ = os.Remove(path)
	}
	config.Targets = []fakeGitTarget{shutdownTarget}
	if err := writeFakeGitConfig(configPath, config); err != nil {
		return err
	}
	shutdownQueryClient, err := daemon.Dial(ctx, inst.address, inst.token)
	if err != nil {
		return err
	}
	defer shutdownQueryClient.Close()
	shutdownQuery := heldGitQuery{client: shutdownQueryClient, cwd: shutdownCwd, queryID: uuid.NewString(), started: shutdownTarget.StartedPath, heartbeat: shutdownTarget.Heartbeat}
	if err := sendHeldStatus(ctx, shutdownQuery); err != nil {
		return err
	}
	if err := waitForFile(ctx, shutdownTarget.StartedPath, 5*time.Second); err != nil {
		return fmt.Errorf("shutdown probe fake Git did not start: %w", err)
	}
	shutdownHeartbeatStart, err := waitHeartbeat(ctx, shutdownTarget.Heartbeat, 2*time.Second)
	if err != nil {
		return err
	}
	shutdownClient, err := daemon.Dial(ctx, inst.address, inst.token)
	if err != nil {
		return err
	}
	defer shutdownClient.Close()
	if err := shutdownClient.Send(ctx, map[string]any{"type": "shutdown"}); err != nil {
		return fmt.Errorf("send normal daemon shutdown: %w", err)
	}
	shutdownStartedAt := time.Now()
	for {
		exited, waitErr := inst.child.Exited(25)
		if waitErr != nil {
			return waitErr
		}
		if exited {
			break
		}
		if time.Since(shutdownStartedAt) > 15*time.Second {
			return errors.New("daemon did not exit naturally after shutdown message")
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(25 * time.Millisecond):
		}
	}
	shutdownExitCode, err := inst.child.ExitCode()
	if err != nil {
		return err
	}
	if shutdownExitCode != 0 {
		return fmt.Errorf("daemon shutdown exit code %d, want 0", shutdownExitCode)
	}
	shutdownHeartbeatAfter, err := waitHeartbeatStable(ctx, shutdownTarget.Heartbeat, 4*time.Second, 250*time.Millisecond)
	if err != nil {
		return fmt.Errorf("daemon shutdown left fake-Git heartbeat running: %w", err)
	}
	report.Wire["globalQueryLifecycle"] = map[string]any{
		"admittedHeldQueries": workerSlots, "activeFakeGitProbesAtSaturation": len(startedIndexes), "successfulSettingsBeforeFull": settingsBeforeFull,
		"ninthQueryError": ninthError, "ninthQueryMs": ninthMs,
		"sameSocketEighthBarrierError": fullSettingsErr, "fullCapacityGetSettingsError": fullSettingsErr2.Error(), "fullCapacityGetSettingsMs": fullSettingsAtMs,
		"sameWebSocketCancelAck": json.RawMessage(cancelAck), "cancelAckMs": cancelMs,
		"firstHeartbeatAtStart": firstHeartbeat, "firstHeartbeatAfterCancel": firstHeartbeatAfterCancel,
		"settingsAfterCancelMs": settingsAfterCancelMs, "settingsAfterCancel": json.RawMessage(settingsAfterCancel),
		"disconnectQueryIndex": disconnectedIndex, "disconnectHeartbeatStopped": true,
		"unrelatedQueryIndex": survivorIndex, "unrelatedHeartbeatBeforeDisconnect": survivorBefore, "unrelatedHeartbeatAfterDisconnect": survivorAfter,
		"shutdownHeartbeatAtStart": shutdownHeartbeatStart, "shutdownHeartbeatAfterExit": shutdownHeartbeatAfter,
		"shutdownExitCode": shutdownExitCode, "ownedCloseCalledAfterNaturalExit": true,
	}
	report.Checks["global-eight-query-slots-ninth-failfast"] = true
	report.Checks["full-capacity-same-websocket-cancel-and-slot-recovery"] = true
	report.Checks["connection-disconnect-cancels-only-its-query"] = true
	report.Checks["normal-shutdown-drains-query-and-exits-zero-before-owned-close"] = true
	return nil
}

func sendHeldStatus(ctx context.Context, query heldGitQuery) error {
	return query.client.Send(ctx, map[string]any{"type": "request", "requestId": uuid.NewString(), "sessionId": "00000000-0000-0000-0000-000000000000", "runtimeId": "00000000-0000-0000-0000-000000000000", "command": map[string]any{"type": "workspace", "operation": map[string]any{"type": "inspectStatus", "cwd": query.cwd, "queryId": query.queryID}}})
}

func waitForStartedCount(ctx context.Context, queries []heldGitQuery, minimum int, limit time.Duration) error {
	waitCtx, cancel := context.WithTimeout(ctx, limit)
	defer cancel()
	tick := time.NewTicker(20 * time.Millisecond)
	defer tick.Stop()
	for {
		count := 0
		for _, query := range queries {
			if _, err := os.Stat(query.started); err == nil {
				count++
			}
		}
		if count >= minimum {
			return nil
		}
		select {
		case <-waitCtx.Done():
			return waitCtx.Err()
		case <-tick.C:
		}
	}
}

func cancelHeldGitQuery(ctx context.Context, client *daemon.Client, queryID string) (json.RawMessage, int64, error) {
	started := time.Now()
	requestCtx, cancel := context.WithTimeout(ctx, 2500*time.Millisecond)
	raw, err := client.Request(requestCtx, map[string]any{"type": "workspace", "operation": map[string]any{"type": "cancelGitQuery", "queryId": queryID}})
	cancel()
	elapsed := time.Since(started).Milliseconds()
	if err != nil {
		return nil, elapsed, err
	}
	if !strings.Contains(string(raw), "ack") || elapsed > 2000 {
		return raw, elapsed, fmt.Errorf("cancel ack invalid or late (%dms): %s", elapsed, raw)
	}
	return raw, elapsed, nil
}

func waitHeartbeatStable(parent context.Context, path string, limit, stableWindow time.Duration) (uint64, error) {
	ctx, cancel := context.WithTimeout(parent, limit)
	defer cancel()
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	var current uint64
	var stableSince time.Time
	for {
		value, err := readHeartbeat(path)
		if err != nil {
			return 0, err
		}
		if current == 0 || value != current {
			current = value
			stableSince = time.Now()
		} else if time.Since(stableSince) >= stableWindow {
			return current, nil
		}
		select {
		case <-ctx.Done():
			return current, ctx.Err()
		case <-tick.C:
		}
	}
}

func waitHeartbeatAdvanced(parent context.Context, path string, previous uint64, limit time.Duration) (uint64, error) {
	ctx, cancel := context.WithTimeout(parent, limit)
	defer cancel()
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		value, err := readHeartbeat(path)
		if err != nil {
			return 0, err
		}
		if value > previous {
			return value, nil
		}
		select {
		case <-ctx.Done():
			return value, ctx.Err()
		case <-tick.C:
		}
	}
}

func retrySettingsUntilAvailable(parent context.Context, client *daemon.Client, limit time.Duration) (json.RawMessage, int64, error) {
	ctx, cancel := context.WithTimeout(parent, limit)
	defer cancel()
	started := time.Now()
	for {
		requestCtx, stop := context.WithTimeout(ctx, 500*time.Millisecond)
		raw, err := client.Request(requestCtx, map[string]any{"type": "getSettings"})
		stop()
		if err == nil {
			return raw, time.Since(started).Milliseconds(), nil
		}
		if !strings.Contains(strings.ToLower(err.Error()), "capacity reached") {
			return nil, time.Since(started).Milliseconds(), err
		}
		select {
		case <-ctx.Done():
			return nil, time.Since(started).Milliseconds(), ctx.Err()
		case <-time.After(20 * time.Millisecond):
		}
	}
}

func startStatusDaemon(ctx context.Context, binary, root, workspace, path string) (*statusDaemon, error) {
	data := filepath.Join(root, "data")
	logs := filepath.Join(root, "daemon-logs-status-"+uuid.NewString())
	if err := os.MkdirAll(data, 0o700); err != nil {
		return nil, err
	}
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return nil, err
	}
	token := uuid.NewString()
	child, err := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", data, "--bind", "127.0.0.1:0"}, logs, root, ownedprocess.BuildEnv(root, map[string]string{"FINTWIND_DAEMON_TOKEN": token, "PATH": path}))
	if err != nil {
		return nil, err
	}
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		raw, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
			PID     uint32 `json:"pid"`
		}
		if json.Unmarshal(raw, &ready) == nil && ready.PID == child.PID && ready.Address != "" {
			if err := checkLoopback(ready.Address); err != nil {
				_ = child.Close()
				return nil, err
			}
			return &statusDaemon{root: root, workspace: workspace, logs: []string{logs}, address: ready.Address, token: token, child: child}, nil
		}
		if done, _ := child.Exited(0); done {
			_ = child.Close()
			return nil, errors.New("Owned daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			_ = child.Close()
			return nil, ctx.Err()
		case <-tick.C:
		}
	}
}

func checkLoopback(address string) error {
	host, _, err := net.SplitHostPort(address)
	if err != nil {
		return err
	}
	ip := net.ParseIP(host)
	if ip == nil || !ip.IsLoopback() {
		return fmt.Errorf("daemon bound outside loopback: %s", address)
	}
	return nil
}

func runOwnedGit(ctx context.Context, git, round, cwd string, args ...string) error {
	logDir := filepath.Join(round, "git-logs-"+uuid.NewString())
	if err := os.MkdirAll(logDir, 0o700); err != nil {
		return err
	}
	owned, err := ownedprocess.StartWithEnvironment(git, args, logDir, cwd, ownedprocess.BuildEnv(round, map[string]string{"GIT_TERMINAL_PROMPT": "0", "GIT_CONFIG_NOSYSTEM": "1"}))
	if err != nil {
		return err
	}
	defer owned.Close()
	for {
		done, waitErr := owned.Exited(25)
		if waitErr != nil {
			return waitErr
		}
		if done {
			code, err := owned.ExitCode()
			if err != nil {
				return err
			}
			if code != 0 {
				stderr, _ := os.ReadFile(owned.StderrPath)
				stdout, _ := os.ReadFile(owned.StdoutPath)
				return fmt.Errorf("owned git %v exited %d: %s %s", args, code, strings.TrimSpace(string(stdout)), strings.TrimSpace(string(stderr)))
			}
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("owned git %v: %w", args, ctx.Err())
		case <-time.After(25 * time.Millisecond):
		}
	}
}

func writeFixtureFile(root, rel, value string) error {
	path := filepath.Join(root, filepath.FromSlash(rel))
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	return os.WriteFile(path, []byte(value), 0o600)
}

func requestWorkspace(ctx context.Context, client *daemon.Client, operation map[string]any) (json.RawMessage, map[string]json.RawMessage, error) {
	raw, err := client.Request(ctx, map[string]any{"type": "workspace", "operation": operation})
	if err != nil {
		return nil, nil, err
	}
	var payload map[string]json.RawMessage
	if err := json.Unmarshal(raw, &payload); err != nil {
		return raw, nil, err
	}
	return raw, payload, nil
}

func requestGitStatus(ctx context.Context, client *daemon.Client, cwd, kind string, extra map[string]any) (gitStatusWire, []byte, error) {
	operation := map[string]any{"type": kind, "cwd": cwd}
	for key, value := range extra {
		operation[key] = value
	}
	raw, payload, err := requestWorkspace(ctx, client, operation)
	if err != nil {
		return gitStatusWire{}, raw, err
	}
	if string(payload["type"]) != `"workspace"` {
		return gitStatusWire{}, raw, fmt.Errorf("unexpected daemon workspace response: %s", raw)
	}
	var envelope struct {
		Result json.RawMessage `json:"result"`
	}
	if err := json.Unmarshal(raw, &envelope); err != nil {
		return gitStatusWire{}, raw, err
	}
	var result gitStatusWire
	if err := json.Unmarshal(envelope.Result, &result); err != nil {
		return result, raw, err
	}
	if result.Type != "worktreeStatus" {
		return result, raw, fmt.Errorf("unexpected status result type: %s", envelope.Result)
	}
	return result, envelope.Result, nil
}

func requireBranchCounts(status gitStatusWire, branch, upstream string, ahead, behind uint64) error {
	if status.Status == nil {
		return errors.New("status unexpectedly null")
	}
	if status.Status.Branch == nil || *status.Status.Branch != branch || status.Status.Upstream == nil || *status.Status.Upstream != upstream || status.Status.Ahead != ahead || status.Status.Behind != behind {
		return fmt.Errorf("got branch=%v upstream=%v ahead=%d behind=%d; want %s %s %d %d", status.Status.Branch, status.Status.Upstream, status.Status.Ahead, status.Status.Behind, branch, upstream, ahead, behind)
	}
	return nil
}

func hasEntry(status gitStatusWire, path, index, worktree string) bool {
	if status.Status == nil {
		return false
	}
	for _, entry := range status.Status.Entries {
		if entry.Path == path && entry.IndexStatus == index && entry.WorktreeStatus == worktree {
			return true
		}
	}
	return false
}

func hasPath(status gitStatusWire, path string) bool {
	if status.Status == nil {
		return false
	}
	for _, entry := range status.Status.Entries {
		if entry.Path == path {
			return true
		}
	}
	return false
}

func hasRename(status gitStatusWire, path, origin string) bool {
	if status.Status == nil {
		return false
	}
	for _, entry := range status.Status.Entries {
		if entry.Path == path && strings.ContainsAny(entry.IndexStatus+entry.WorktreeStatus, "RC") && entry.OriginPath != nil && *entry.OriginPath == origin {
			return true
		}
	}
	return false
}

func fileSHA(path string) (string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:]), nil
}

func copyFile(source, destination string) error {
	raw, err := os.ReadFile(source)
	if err != nil {
		return err
	}
	return os.WriteFile(destination, raw, 0o700)
}

func writeFakeGitConfig(path string, config fakeGitConfig) error {
	raw, err := json.Marshal(config)
	if err != nil {
		return err
	}
	return os.WriteFile(path, raw, 0o600)
}

func waitForFile(parent context.Context, path string, limit time.Duration) error {
	ctx, cancel := context.WithTimeout(parent, limit)
	defer cancel()
	tick := time.NewTicker(20 * time.Millisecond)
	defer tick.Stop()
	for {
		if _, err := os.Stat(path); err == nil {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-tick.C:
		}
	}
}

func waitHeartbeat(parent context.Context, path string, limit time.Duration) (uint64, error) {
	ctx, cancel := context.WithTimeout(parent, limit)
	defer cancel()
	tick := time.NewTicker(20 * time.Millisecond)
	defer tick.Stop()
	for {
		value, err := readHeartbeat(path)
		if err == nil && value >= 3 {
			return value, nil
		}
		select {
		case <-ctx.Done():
			return 0, ctx.Err()
		case <-tick.C:
		}
	}
}

func readHeartbeat(path string) (uint64, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return 0, err
	}
	return strconv.ParseUint(strings.TrimSpace(string(raw)), 10, 64)
}

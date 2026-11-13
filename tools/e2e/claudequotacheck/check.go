package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"fintwind-go/internal/backendquota"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
)

type resultReport struct {
	Passed       bool              `json:"passed"`
	DaemonSHA256 string            `json:"daemonSha256"`
	Command      string            `json:"command"`
	Checks       map[string]bool   `json:"checks"`
	Notes        map[string]string `json:"notes"`
	Covered      []string          `json:"covered"`
	Uncovered    []string          `json:"uncovered"`
	Error        string            `json:"error,omitempty"`
	Round        string            `json:"round"`
}

type harness struct {
	ctx                                                   context.Context
	round, dataDir, home, workspace, bin, self, daemonBin string
	token, address                                        string
	daemon                                                *ownedprocess.Owned
	client                                                *daemon.Client
	daemonStarts                                          int
	checks                                                map[string]bool
	notes                                                 map[string]string
}

func run(ctx context.Context, daemonBin, outputDir string) (runErr error) {
	if !filepath.IsAbs(daemonBin) {
		return errors.New("absolute daemon executable path required")
	}
	if _, err := os.Stat(daemonBin); err != nil {
		return errors.New("isolated daemon executable is unavailable")
	}
	if outputDir == "" {
		outputDir = filepath.Join("..", "target", "go-backend-claudequota")
	}
	outputDir, err := filepath.Abs(outputDir)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(outputDir, 0o700); err != nil {
		return err
	}
	raw, err := os.ReadFile(daemonBin)
	if err != nil {
		return err
	}
	digest := sha256.Sum256(raw)
	self, err := os.Executable()
	if err != nil {
		return err
	}
	round, err := os.MkdirTemp(outputDir, "round-claude-quota-*")
	if err != nil {
		return err
	}
	h := &harness{ctx: ctx, round: round, dataDir: filepath.Join(round, "data"), home: filepath.Join(round, "home"), workspace: filepath.Join(round, "workspace", "fintwind-claude-quota"), bin: filepath.Join(round, "bin"), self: self, daemonBin: daemonBin, token: uuid.NewString(), checks: map[string]bool{}, notes: map[string]string{}}
	report := resultReport{DaemonSHA256: hex.EncodeToString(digest[:]), Command: "claudequotacheck.exe --daemon " + daemonBin + " --out " + outputDir, Checks: h.checks, Notes: h.notes, Round: round,
		Covered:   []string{},
		Uncovered: []string{"No real Claude binary, credentials, network, browser, OAuth, model call, daily app, or visual test is used", "CreatePseudoConsole/Job allocation and ResumeThread failure injection are structural only", "Production 5s/65s limits are not waited out; bounded cancellation/hang cases are exercised", "Remaining cases after a blocking failure are not claimed as tested"}}
	reportPath := filepath.Join(outputDir, "claude-quota-result-"+filepath.Base(round)+".json")
	defer func() {
		report.Passed = runErr == nil
		report.Covered = report.Covered[:0]
		for name, passed := range report.Checks {
			if passed {
				report.Covered = append(report.Covered, name)
			} else {
				report.Uncovered = append(report.Uncovered, "not passed: "+name)
			}
		}
		sort.Strings(report.Covered)
		sort.Strings(report.Uncovered)
		if runErr != nil {
			report.Error = runErr.Error()
		}
		body, marshalErr := json.MarshalIndent(report, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(reportPath, body, 0o600)
		}
		if marshalErr != nil {
			runErr = errors.Join(runErr, marshalErr)
		}
		fmt.Println("[REPORT]", reportPath)
	}()
	if err := h.prepare(); err != nil {
		return err
	}
	directErr := h.directPTYProbe()
	if directErr != nil {
		h.notes["direct-pty-failure"] = directErr.Error()
	}
	moduleErr := h.moduleSuite()
	if moduleErr != nil {
		h.notes["module-failure"] = moduleErr.Error()
	}
	if err := h.startDaemon(); err != nil {
		return errors.Join(directErr, moduleErr, err)
	}
	defer func() { runErr = errors.Join(runErr, h.closeDaemon()) }()
	daemonErr := h.daemonSuite()
	if daemonErr != nil {
		h.notes["daemon-failure"] = daemonErr.Error()
		if !h.checks["daemon-shutdown-cancels-owned-tree-restarts"] && h.daemon != nil && h.client != nil {
			cancelErr := h.shutdownCancellationSuite()
			if cancelErr != nil {
				h.notes["shutdown-cancel-failure"] = cancelErr.Error()
			}
			daemonErr = errors.Join(daemonErr, cancelErr)
		}
	}
	return errors.Join(directErr, moduleErr, daemonErr)
}

func (h *harness) prepare() error {
	for _, dir := range []string{h.dataDir, h.home, h.workspace, h.bin, filepath.Join(h.home, ".claude"), filepath.Join(h.round, "tmp"), filepath.Join(h.home, "AppData", "Roaming"), filepath.Join(h.home, "AppData", "Local")} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
	}
	if err := copyFile(h.self, filepath.Join(h.bin, "claude.exe")); err != nil {
		return err
	}
	settings, _ := json.Marshal(map[string]string{"opencode_binary_path": filepath.Join(h.bin, "claude.exe")})
	if err := os.WriteFile(filepath.Join(h.dataDir, "daemon.json"), settings, 0o600); err != nil {
		return err
	}
	return h.writeSeed(seed{Status: "valid", Panel: "quota"})
}

func copyFile(source, dest string) error {
	in, err := os.Open(source)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(dest, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o700)
	if err != nil {
		return err
	}
	defer out.Close()
	_, err = io.Copy(out, in)
	return err
}

func (h *harness) configDir() string { return filepath.Join(h.home, ".claude") }
func (h *harness) seedPath() string  { return filepath.Join(h.configDir(), "e2e-seed.json") }
func (h *harness) callsPath() string { return filepath.Join(h.configDir(), "e2e-calls.jsonl") }

func (h *harness) writeSeed(value seed) error {
	body, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return os.WriteFile(h.seedPath(), body, 0o600)
}

func (h *harness) writeCredential(value string) error {
	return os.WriteFile(filepath.Join(h.configDir(), ".credentials.json"), []byte(value), 0o600)
}

func (h *harness) calls() ([]invocation, error) {
	raw, err := os.ReadFile(h.callsPath())
	if err != nil {
		return nil, err
	}
	var calls []invocation
	for _, line := range strings.Split(string(raw), "\n") {
		if strings.TrimSpace(line) == "" {
			continue
		}
		var c invocation
		if err := json.Unmarshal([]byte(line), &c); err != nil {
			return nil, err
		}
		calls = append(calls, c)
	}
	return calls, nil
}

func (h *harness) startDaemon() error {
	if h.daemon != nil {
		return errors.New("daemon already started")
	}
	h.daemonStarts++
	logDir := filepath.Join(h.round, fmt.Sprintf("daemon-logs-%02d", h.daemonStarts))
	if err := os.MkdirAll(logDir, 0o700); err != nil {
		return err
	}
	// The child PATH contains only this round's fake CLI. It must not fall
	// through to an installed Claude on the host.
	pathValue := h.bin
	env := ownedprocess.BuildEnv(h.round, map[string]string{"FINTWIND_DAEMON_TOKEN": h.token, "PATH": pathValue})
	child, err := ownedprocess.StartWithEnvironment(h.daemonBin, []string{"--data-dir", h.dataDir, "--bind", "127.0.0.1:0"}, logDir, h.round, env)
	if err != nil {
		return err
	}
	address, err := waitReady(h.ctx, child)
	if err != nil {
		_ = child.Close()
		return err
	}
	client, err := daemon.Dial(h.ctx, address, h.token)
	if err != nil {
		_ = child.Close()
		return err
	}
	h.daemon, h.client, h.address = child, client, address
	return nil
}

func waitReady(ctx context.Context, child *ownedprocess.Owned) (string, error) {
	deadline := time.NewTimer(15 * time.Second)
	defer deadline.Stop()
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		raw, _ := os.ReadFile(child.StdoutPath)
		var v struct {
			Address string `json:"address"`
		}
		if json.Unmarshal(raw, &v) == nil && v.Address != "" {
			return v.Address, nil
		}
		if done, e := child.Exited(0); e != nil || done {
			return "", errors.New("daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-deadline.C:
			return "", errors.New("daemon ready timeout")
		case <-tick.C:
		}
	}
}

func (h *harness) closeDaemon() error {
	if h.client != nil {
		h.client.Close()
		h.client = nil
	}
	if h.daemon != nil {
		err := h.daemon.Close()
		h.daemon = nil
		return err
	}
	return nil
}

func (h *harness) quota(ctx context.Context, force bool) (backendquota.Quota, error) {
	raw, err := h.client.Request(ctx, map[string]any{"type": "fetchProviderQuota", "providerId": "anthropic", "forceRefresh": force})
	if err != nil {
		return backendquota.Quota{}, err
	}
	var envelope struct {
		Quota backendquota.Quota `json:"quota"`
	}
	if err := json.Unmarshal(raw, &envelope); err != nil {
		return backendquota.Quota{}, fmt.Errorf("decode provider quota: %w", err)
	}
	return envelope.Quota, nil
}

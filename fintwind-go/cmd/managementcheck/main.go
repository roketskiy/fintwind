package main

// Isolated OpenCode CLI management E2E (installOpenCode/updateOpenCode).
// The daemon runs real management against a fixture bun/manager (this same
// binary copied per round as bun.exe); package state never leaves the round
// and no real download, install, or upgrade ever happens. Module honesty:
// this is fixture-manager E2E over the real daemon WS plus one read-only
// official metadata GET per operation, never a real package install.

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/ownedprocess"
)

type opRecord struct {
	Name   string `json:"name"`
	OK     bool   `json:"ok"`
	Detail string `json:"detail"`
}

type isoRound struct {
	round      string
	dataDir    string
	workspace  string
	fixtureDir string
}

func main() {
	name := filepath.Base(os.Args[0])
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		if err := runServeFixture(os.Args[1:]); err != nil {
			fmt.Fprintln(os.Stderr, "[X]", err)
			os.Exit(2)
		}
		return
	}
	if strings.HasPrefix(strings.ToLower(strings.TrimSuffix(name, ".exe")), "bun") {
		if err := runBunInstaller(os.Args[1:]); err != nil {
			fmt.Fprintln(os.Stderr, "[X]", err)
			os.Exit(3)
		}
		return
	}
	if strings.HasPrefix(strings.ToLower(strings.TrimSuffix(name, ".exe")), "opencode") {
		if err := runOpencode(os.Args[1:]); err != nil {
			fmt.Fprintln(os.Stderr, "[X]", err)
			os.Exit(3)
		}
		return
	}
	daemonBinary := flag.String("daemon", "", "absolute daemon binary")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Second)
	defer cancel()
	if err := run(ctx, *daemonBinary); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, daemonBinary string) (err error) {
	if !filepath.IsAbs(daemonBinary) {
		return errors.New("absolute daemon binary required")
	}
	for _, key := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "FINTWIND_AGENT_REAL_PROVIDER"} {
		if strings.TrimSpace(os.Getenv(key)) != "" {
			return fmt.Errorf("credential/marker %s must stay unset for this fixture run", key)
		}
	}
	self, err := os.Executable()
	if err != nil {
		return err
	}
	daemonHash, err := fileSHA256(daemonBinary)
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	ops := []opRecord{}
	note := func(name string, opErr error) {
		rec := opRecord{Name: name, OK: opErr == nil}
		if opErr != nil {
			rec.Detail = opErr.Error()
		}
		ops = append(ops, rec)
	}
	counts := map[string]any{}
	versions := map[string]any{}
	report := map[string]any{
		"command":      "managementcheck --daemon " + daemonBinary,
		"module":       "real daemon WS + fixture bun/manager; real metadata GET only, no package install",
		"provider":     "none",
		"realRequests": 0,
		"daemonSha256": daemonHash,
		"daemonBinary": daemonBinary,
		"checks":       checks,
		"versions":     versions,
		"notes": map[string]string{
			"scope": "fixture-manager E2E over the real daemon wire; not a real OpenCode install",
		},
		"uncovered": []string{
			"scheduled vs management lock ordering: structural, no deterministic wire interleave",
			"pool reset error-join path and metadata byte-budget defenses",
			"checkOpenCodeUpdate: covered by existing updatecheck, not invoked here",
		},
	}
	firstRound := ""
	defer func() {
		report["ops"] = ops
		report["checks"] = checks
		report["counts"] = counts
		report["passed"] = err == nil
		if firstRound == "" {
			if round, rerr := ownedprocess.NewRoundDir(); rerr == nil {
				firstRound = round
			} else {
				fmt.Fprintln(os.Stderr, "[X] report dir unavailable")
				return
			}
		}
		raw, _ := json.MarshalIndent(report, "", "  ")
		_ = os.WriteFile(filepath.Join(firstRound, "management-result.json"), raw, 0o600)
		fmt.Println("[REPORT]", filepath.Join(firstRound, "management-result.json"))
	}()

	r1, r1err := newIsoRound(self)
	if r1err != nil {
		return r1err
	}
	firstRound = r1.round
	if err := roundInstallCore(ctx, daemonBinary, r1, checks, note, counts, versions); err != nil {
		return err
	}
	for i, setup := range []struct {
		name    string
		sential string
	}{
		{"installer-failure", "fixture-fail-install"},
		{"wrong-version", "fixture-wrong-version"},
	} {
		_ = i
		round, rerr := newIsoRound(self)
		if rerr != nil {
			return rerr
		}
		if err := roundSentinelRefusal(ctx, daemonBinary, round, setup.name, setup.sential, checks, note, counts); err != nil {
			return err
		}
	}
	roundCAS, rerr := newIsoRound(self)
	if rerr != nil {
		return rerr
	}
	if err := roundCASRace(ctx, daemonBinary, roundCAS, checks, note, counts); err != nil {
		return err
	}
	roundSlow, rerr := newIsoRound(self)
	if rerr != nil {
		return rerr
	}
	if err := roundSlowBudget(ctx, daemonBinary, roundSlow, checks, note, counts, versions); err != nil {
		return err
	}
	roundUpdate, rerr := newIsoRound(self)
	if rerr != nil {
		return rerr
	}
	if err := roundUpdateRefusals(ctx, daemonBinary, roundUpdate, checks, note, counts, versions); err != nil {
		return err
	}
	roundUpdateOK, rerr := newIsoRound(self)
	if rerr != nil {
		return rerr
	}
	if err := roundUpdateSuccess(ctx, daemonBinary, roundUpdateOK, checks, note, counts, versions); err != nil {
		return err
	}
	return nil
}

func newIsoRound(self string) (isoRound, error) {
	var out isoRound
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return out, err
	}
	out.round = round
	out.dataDir = filepath.Join(round, "data")
	out.workspace = filepath.Join(round, "workspace")
	out.fixtureDir = filepath.Join(round, "fixture-bin")
	for _, dir := range []string{out.dataDir, filepath.Join(out.workspace, "main"), out.fixtureDir} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return out, err
		}
	}
	raw, err := os.ReadFile(self)
	if err != nil {
		return out, err
	}
	if err := os.WriteFile(filepath.Join(out.fixtureDir, "bun.exe"), raw, 0o700); err != nil {
		return out, err
	}
	settings, _ := json.Marshal(map[string]string{"network_proxy_url": "http://127.0.0.1:7897"})
	if err := os.WriteFile(filepath.Join(out.dataDir, "daemon.json"), settings, 0o600); err != nil {
		return out, err
	}
	return out, nil
}

func daemonEnv(round, fixtureDir, token string) []string {
	return ownedprocess.BuildEnv(round, map[string]string{
		"FINTWIND_DAEMON_TOKEN": token,
		"PATH":                  fixtureDir,
	})
}

func launchDaemon(ctx context.Context, daemonBinary string, iso isoRound, token string) (*ownedprocess.Owned, string, error) {
	logs := filepath.Join(iso.round, "daemon-logs")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return nil, "", err
	}
	child, err := ownedprocess.StartWithEnvironment(daemonBinary, []string{"--data-dir", iso.dataDir, "--bind", "127.0.0.1:0"}, logs, iso.round, daemonEnv(iso.round, iso.fixtureDir, token))
	if err != nil {
		return nil, "", err
	}
	address, err := waitReady(ctx, child)
	if err != nil {
		_ = child.Close()
		return nil, "", err
	}
	return child, address, nil
}

func waitReady(ctx context.Context, child *ownedprocess.Owned) (string, error) {
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		raw, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
			PID     uint32 `json:"pid"`
		}
		if json.Unmarshal(raw, &ready) == nil && ready.PID == child.PID && ready.Address != "" {
			return ready.Address, nil
		}
		if done, _ := child.Exited(0); done {
			return "", errors.New("daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-tick.C:
		}
	}
}

func readDaemonJSON(dataDir string) map[string]any {
	raw, _ := os.ReadFile(filepath.Join(dataDir, "daemon.json"))
	out := map[string]any{}
	_ = json.Unmarshal(raw, &out)
	return out
}

func selectedBinary(dataDir string) string {
	value, _ := readDaemonJSON(dataDir)["opencode_binary_path"].(string)
	return value
}

type fixtureCall struct {
	Method string            `json:"method"`
	Path   string            `json:"path"`
	Body   json.RawMessage   `json:"body"`
	Env    map[string]string `json:"env"`
}

func readCalls(workspace string) []fixtureCall {
	raw, _ := os.ReadFile(filepath.Join(workspace, "fixture-calls.jsonl"))
	calls := []fixtureCall{}
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		var call fixtureCall
		if json.Unmarshal([]byte(line), &call) == nil {
			calls = append(calls, call)
		}
	}
	return calls
}

func bunInstallCalls(workspace string) []fixtureCall {
	matched := []fixtureCall{}
	for _, call := range readCalls(workspace) {
		if call.Method == "bun" {
			matched = append(matched, call)
		}
	}
	return matched
}

func requestedVersion(workspace string) (string, error) {
	for _, call := range bunInstallCalls(workspace) {
		var args []string
		if json.Unmarshal(call.Body, &args) != nil {
			continue
		}
		for _, arg := range args {
			if index := strings.LastIndex(arg, "@opencode/cli@"); index >= 0 {
				return arg[index+len("@opencode/cli@"):], nil
			}
		}
	}
	return "", errors.New("no recorded bun install version")
}

func engineDirCount(round string) int {
	entries, _ := os.ReadDir(round)
	count := 0
	for _, entry := range entries {
		if entry.IsDir() && strings.HasPrefix(entry.Name(), "engine-") {
			count++
		}
	}
	return count
}

func fileSHA256(path string) (string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:]), nil
}

package main

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

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

type record struct {
	Name   string `json:"name"`
	OK     bool   `json:"ok"`
	Detail string `json:"detail,omitempty"`
}

type round struct {
	root      string
	data      string
	workspace string
	logs      string
	token     string
	child     *ownedprocess.Owned
	address   string
}

type reportState struct {
	Checks     map[string]bool
	Records    []record
	Counts     map[string]any
	Round      string
	Daemon     string
	Self       string
	RunnerHash string
	Workspace  string
	LogDirs    []string
}

func main() {
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		if err := serveFakeCLI(); err != nil {
			fmt.Fprintln(os.Stderr, "[X] fixture:", err)
			os.Exit(2)
		}
		return
	}
	path := flag.String("daemon", "", "absolute path to isolated daemon binary")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 570*time.Second)
	defer cancel()
	if err := run(ctx, *path); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, daemonBinary string) (err error) {
	if !filepath.IsAbs(daemonBinary) {
		return errors.New("absolute daemon path required")
	}
	for _, key := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "FINTWIND_AGENT_REAL_PROVIDER"} {
		if strings.TrimSpace(os.Getenv(key)) != "" {
			return fmt.Errorf("credential variable %s must be unset", key)
		}
	}
	self, err := os.Executable()
	if err != nil {
		return err
	}
	runnerHash, err := shaFile(self)
	if err != nil {
		return err
	}
	daemonHash, err := shaFile(daemonBinary)
	if err != nil {
		return err
	}
	root, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	state := &reportState{Checks: map[string]bool{}, Counts: map[string]any{}, Round: root, Daemon: daemonHash, Self: self, RunnerHash: runnerHash,
		Workspace: filepath.Join(root, "workspace", "activity")}
	if err := os.MkdirAll(state.Workspace, 0o700); err != nil {
		return err
	}
	defer func() {
		out := map[string]any{
			"passed": err == nil, "command": self + " --daemon " + daemonBinary,
			"roundDir": root, "daemonBinary": daemonBinary, "daemonSha256": daemonHash,
			"runnerBinary": self, "runnerSha256": runnerHash, "childLogDirectories": state.LogDirs,
			"provider": "none", "realRequests": 0,
			"checks": state.Checks, "ops": state.Records, "counts": state.Counts,
			"uncovered": []string{"Nested CodeMode activities are intentionally not synthesized as independent activities", "real DeepSeek low file-read check was previously completed once and is not repeated", "real OpenCode provider runtime is not used; live events come from an Owned loopback fake CLI", "visual rendering and remote image loading are not covered"},
		}
		if err != nil {
			out["error"] = err.Error()
		}
		raw, _ := json.MarshalIndent(out, "", "  ")
		_ = os.WriteFile(filepath.Join(root, "metadata-result.json"), raw, 0o600)
		fmt.Println("[REPORT]", filepath.Join(root, "metadata-result.json"))
	}()

	inst, err := startDaemon(ctx, daemonBinary, root, state.Workspace)
	if err != nil {
		return err
	}
	state.LogDirs = append(state.LogDirs, inst.logs)
	defer closeDaemon(inst)
	client, err := daemon.Dial(ctx, inst.address, inst.token)
	if err != nil {
		return err
	}
	defer client.Close()

	if err = runTerminalWire(ctx, client, state); err != nil {
		return err
	}
	if err = runActivityFlow(ctx, daemonBinary, inst, client, state); err != nil {
		return err
	}
	return nil
}

func shaFile(path string) (string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:]), nil
}

func startDaemon(ctx context.Context, binary, root, workspace string) (*round, error) {
	data := filepath.Join(root, "data")
	logs := filepath.Join(root, "daemon-logs-"+uuid.NewString())
	if err := os.MkdirAll(data, 0o700); err != nil {
		return nil, err
	}
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return nil, err
	}
	token := uuid.NewString()
	env := ownedprocess.BuildEnv(root, map[string]string{"FINTWIND_DAEMON_TOKEN": token})
	child, err := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", data, "--bind", "127.0.0.1:0"}, logs, root, env)
	if err != nil {
		return nil, err
	}
	address, err := waitDaemon(ctx, child)
	if err != nil {
		_ = child.Close()
		return nil, err
	}
	rel, err := filepath.Rel(root, workspace)
	if err != nil || !filepath.IsLocal(rel) {
		_ = child.Close()
		return nil, fmt.Errorf("workspace escapes isolated profile: %s", workspace)
	}
	return &round{root: root, data: data, workspace: workspace, logs: logs, token: token, child: child, address: address}, nil
}

func waitDaemon(ctx context.Context, child *ownedprocess.Owned) (string, error) {
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
			return "", errors.New("owned daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-tick.C:
		}
	}
}

func closeDaemon(inst *round) {
	if inst != nil && inst.child != nil {
		_ = inst.child.Close()
	}
}

func note(state *reportState, name string, err error) {
	r := record{Name: name, OK: err == nil}
	if err != nil {
		r.Detail = err.Error()
	}
	state.Records = append(state.Records, r)
	if err == nil {
		state.Checks[name] = true
	}
}

func runTerminalWire(ctx context.Context, client *daemon.Client, state *reportState) error {
	sid := uuid.NewString()
	_, err := client.RequestFor(ctx, sid, "", map[string]any{"type": "openTerminal", "cwd": state.Workspace})
	expectErr := fmt.Sprintf("daemon terminals are not supported on this platform")
	if err == nil || !strings.Contains(err.Error(), expectErr) {
		got := fmt.Errorf("openTerminal: expected %q, got %v", expectErr, err)
		note(state, "terminal-open-unsupported", got)
		return got
	}
	note(state, "terminal-open-unsupported", nil)

	for _, kind := range []string{"writeTerminal", "resizeTerminal"} {
		_, err = client.RequestFor(ctx, sid, "", map[string]any{"type": kind, "terminalId": uuid.NewString(), "data": "unused", "cols": 80, "rows": 24})
		expect := "daemon terminal " + sid + " is not running"
		if err == nil || !strings.Contains(err.Error(), expect) {
			got := fmt.Errorf("%s: expected %q, got %v", kind, expect, err)
			note(state, "terminal-"+strings.TrimSuffix(kind, "Terminal")+"-not-running", got)
			return got
		}
		note(state, "terminal-"+strings.TrimSuffix(kind, "Terminal")+"-not-running", nil)
	}
	raw, err := client.RequestFor(ctx, sid, "", map[string]any{"type": "closeTerminal", "terminalId": uuid.NewString()})
	if err != nil {
		note(state, "terminal-close-idempotent", err)
		return err
	}
	var ack struct {
		Type string `json:"type"`
	}
	if err := json.Unmarshal(raw, &ack); err != nil || ack.Type != "ack" {
		err = fmt.Errorf("closeTerminal: expected ack, got %s", raw)
		note(state, "terminal-close-idempotent", err)
		return err
	}
	note(state, "terminal-close-idempotent", nil)
	return nil
}

func admitSession(ctx context.Context, client *daemon.Client, sid, pid, workspace, turnID, prompt string, turnCount int, previous map[string]any, now uint64) error {
	record := model.NewSubmissionRecord(sid, turnID, prompt, nil)
	turn := map[string]any{"id": turnID, "status": "running", "turn_count": turnCount, "started_at": now, "submission": record.Receipt}
	messages := []any{}
	turns := []any{turn}
	blocks := []any{}
	if previous != nil {
		messages, _ = previous["messages"].([]any)
		turns, _ = previous["turns"].([]any)
		blocks, _ = previous["transcript_blocks"].([]any)
		messages = append(append([]any{}, messages...), map[string]any{"id": uuid.NewString(), "turn_id": turnID, "role": "user", "content": prompt, "created_at": now, "attachments": []any{}, "streaming": false})
		turns = append(append([]any{}, turns...), turn)
	} else {
		messages = append(messages, map[string]any{"id": uuid.NewString(), "turn_id": turnID, "role": "user", "content": prompt, "created_at": now, "attachments": []any{}, "streaming": false})
	}
	session := map[string]any{
		"id": sid, "project_id": pid, "title": "activity fixture", "provider": "opencode", "status": "working",
		"created_at": now, "updated_at": now, "detail_loaded": true,
		"messages": messages, "turns": turns, "transcript_blocks": blocks,
	}
	project := map[string]any{"id": pid, "name": "activity-fixture", "path": workspace, "created_at": now}
	_, err := client.RequestFor(ctx, sid, "", map[string]any{"type": "acceptSubmission", "project": project, "session": session, "submission": record})
	return err
}

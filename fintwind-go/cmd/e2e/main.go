// Command e2e exercises the daemon protocol the Go client depends on: it
// starts an isolated daemon, saves a project and its draft task, starts the
// runtime, sends one prompt and waits for the turn to settle. It prints one
// PASS/FAIL line per checked behavior and exits non-zero on failure.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

func main() {
	daemonPath := flag.String("daemon", defaultDaemonPath(), "path to fintwind-daemon.exe")
	dataDir := flag.String("data-dir", filepath.Join(os.TempDir(), "fintwind-go-e2e"), "isolated daemon data directory")
	workspace := flag.String("workspace", "", "task workspace directory (defaults to a temp folder)")
	sendPrompt := flag.Bool("prompt", true, "send one real prompt and wait for turnFinished")
	flag.Parse()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()

	if err := os.RemoveAll(*dataDir); err != nil {
		fail("prepare data directory: %v", err)
	}
	taskDir := *workspace
	if taskDir == "" {
		taskDir = filepath.Join(os.TempDir(), "fintwind-go-e2e-workspace")
	}
	if err := os.MkdirAll(taskDir, 0o755); err != nil {
		fail("prepare workspace: %v", err)
	}

	proc, err := daemon.Start(ctx, *daemonPath, *dataDir)
	if err != nil {
		fail("start daemon: %v", err)
	}
	defer proc.Stop()
	pass("daemon ready at %s (protocol 42, environment %q)", proc.Address, proc.EnvironmentID)

	client, err := daemon.Dial(ctx, proc.Address, proc.Token)
	if err != nil {
		fail("dial: %v", err)
	}
	defer client.Close()
	pass("hello handshake")

	var state model.TaskState
	request(ctx, client, map[string]any{"type": "loadTaskState"}, &state, "loadTaskState")
	pass("loadTaskState returned %d projects and %d sessions", len(state.Projects), len(state.Sessions))

	now := uint64(time.Now().Unix())
	project := model.Project{
		ID:        uuid.NewString(),
		Name:      filepath.Base(taskDir),
		Path:      taskDir,
		CreatedAt: now,
	}
	sessionID := uuid.NewString()
	save := map[string]any{
		"type": "saveTaskState",
		"projects": []any{map[string]any{
			"id":             project.ID,
			"name":           project.Name,
			"path":           project.Path,
			"environment_id": "",
			"created_at":     project.CreatedAt,
		}},
		"liveSessionIds": []string{sessionID},
		"sessions": []any{map[string]any{
			"id":                     sessionID,
			"title":                  "New task",
			"project_id":             project.ID,
			"provider":               "opencode",
			"runtime_mode":           "fullAccess",
			"interaction_mode":       "build",
			"status":                 "idle",
			"created_at":             now,
			"updated_at":             now,
			"mermaid_prompt_version": "v4",
		}},
	}
	request(ctx, client, save, nil, "saveTaskState")

	state = model.TaskState{}
	request(ctx, client, map[string]any{"type": "loadTaskState"}, &state, "loadTaskState after save")
	if len(state.Projects) != 1 || len(state.Sessions) != 1 {
		fail("saved draft did not round-trip: %d projects, %d sessions", len(state.Projects), len(state.Sessions))
	}
	pass("saved draft round-trips")

	var detail model.SessionDetail
	request(ctx, client, map[string]any{
		"type":          "hydrateSession",
		"sessionId":     sessionID,
		"environmentId": "00000000-0000-0000-0000-000000000000",
	}, &detail, "hydrateSession")
	if detail.Session == nil {
		fail("hydrateSession returned no session")
	}
	pass("hydrated draft has %d messages and %d turns", len(detail.Session.Messages), len(detail.Session.Turns))

	binary := ""
	if path, err := exec.LookPath("opencode"); err == nil {
		binary = path
	}
	if binary == "" {
		fail("opencode binary not found on PATH")
	}
	version := providerVersion(binary)
	if version == "" {
		fail("could not read the opencode version")
	}
	pass("opencode version %s", version)
	runtimeID := uuid.NewString()
	start := map[string]any{
		"type": "start",
		"options": map[string]any{
			"binary":               binary,
			"cwd":                  taskDir,
			"mode":                 "fullAccess",
			"interactionMode":      "build",
			"model":                nil,
			"reasoningEffort":      nil,
			"serviceTier":          nil,
			"contextWindow":        nil,
			"agentPreset":          nil,
			"providerCursor":       nil,
			"mermaidPromptVersion": "v4",
		},
		"environmentId":           "00000000-0000-0000-0000-000000000000",
		"expectedOpencodeVersion": version,
	}
	requestFor(ctx, client, sessionID, runtimeID, start, nil, "start")
	pass("runtime started (%s)", runtimeID)

	if !*sendPrompt {
		pass("prompt skipped by flag")
		return
	}

	requestFor(ctx, client, sessionID, runtimeID, map[string]any{
		"type":   "prompt",
		"prompt": "Reply with exactly: pong",
		"files":  []any{},
	}, nil, "prompt")

	deadline := time.NewTimer(3 * time.Minute)
	defer deadline.Stop()
	sawText := false
	var connected json.RawMessage
	for {
		select {
		case <-deadline.C:
			fail("no turnFinished within 3 minutes (text=%v)", sawText)
		case event, ok := <-client.Events():
			if !ok {
				fail("event stream closed")
			}
			if event.SessionID != sessionID {
				continue
			}
			switch event.Kind {
			case "connected":
				connected = append(json.RawMessage(nil), event.Payload...)
			case "textDelta", "textEnded":
				sawText = true
			case "turnFinished":
				var payload struct {
					Success bool `json:"success"`
				}
				_ = json.Unmarshal(event.Payload, &payload)
				if !payload.Success {
					fail("turn finished unsuccessfully")
				}
				if !sawText {
					fail("turn finished without streamed text")
				}
				pass("prompt streamed text and finished successfully")
				// Persist the settled transcript the way the app does.
				hydrated, err := client.Request(ctx, map[string]any{
					"type":          "hydrateSession",
					"sessionId":     sessionID,
					"environmentId": "00000000-0000-0000-0000-000000000000",
				})
				if err != nil {
					fail("hydrate settled session: %v", err)
				}
				var holder map[string]json.RawMessage
				if json.Unmarshal(hydrated, &holder) != nil {
					fail("decode settled session")
				}
				var settled map[string]any
				if json.Unmarshal(holder["session"], &settled) != nil {
					fail("decode settled session body")
				}
				if len(connected) > 0 && string(connected) != "null" {
					var cursor any
					if json.Unmarshal(connected, &cursor) == nil {
						settled["provider_cursor"] = cursor
						if asMap, ok := cursor.(map[string]any); ok {
							if native, ok := asMap["sessionId"].(string); ok && native != "" {
								settled["native_session_id"] = native
							}
						}
					}
				}
				request(ctx, client, map[string]any{
					"type": "saveTaskState",
					"projects": []any{map[string]any{
						"id":             project.ID,
						"name":           project.Name,
						"path":           project.Path,
						"environment_id": "",
						"created_at":     project.CreatedAt,
					}},
					"liveSessionIds": []string{sessionID},
					"sessions":       []any{settled},
				}, nil, "save settled session")
				state = model.TaskState{}
				request(ctx, client, map[string]any{"type": "loadTaskState"}, &state, "final loadTaskState")
				pass("final task state holds %d sessions with %d messages",
					len(state.Sessions), len(state.Sessions[0].Messages))
				if len(state.Sessions) == 1 {
					cursorID := ""
					if state.Sessions[0].NativeSessionID != nil {
						cursorID = *state.Sessions[0].NativeSessionID
					}
					if cursorID == "" {
						var cursor struct {
							SessionID string `json:"sessionId"`
						}
						_ = json.Unmarshal(state.Sessions[0].ProviderCursor, &cursor)
						cursorID = cursor.SessionID
					}
					if cursorID == "" {
						fail("settled turn has no native session id")
					}
					request(ctx, client, map[string]any{
						"type": "forkProviderSession",
						"request": map[string]any{
							"provider":   "openCodeRevert",
							"binary":     binary,
							"cwd":        taskDir,
							"session_id": cursorID,
							"turn_count": 0,
						},
						"expectedOpencodeVersion": version,
					}, nil, "rewind")
					pass("provider rewind accepted")
				}
				return
			}
		}
	}
}

func request(ctx context.Context, client *daemon.Client, command map[string]any, out any, label string) {
	payload, err := client.Request(ctx, command)
	if err != nil {
		fail("%s: %v", label, err)
	}
	decode(payload, out, label)
}

func requestFor(ctx context.Context, client *daemon.Client, sessionID, runtimeID string, command map[string]any, out any, label string) {
	payload, err := client.RequestFor(ctx, sessionID, runtimeID, command)
	if err != nil {
		fail("%s: %v", label, err)
	}
	decode(payload, out, label)
}

func decode(payload json.RawMessage, out any, label string) {
	if out == nil || len(payload) == 0 {
		return
	}
	if err := json.Unmarshal(payload, out); err != nil {
		fail("%s decode: %v", label, err)
	}
}

func pass(format string, args ...any) {
	fmt.Printf("PASS %s\n", fmt.Sprintf(format, args...))
}

func fail(format string, args ...any) {
	fmt.Printf("FAIL %s\n", fmt.Sprintf(format, args...))
	os.Exit(1)
}

// providerVersion reads "<binary> --version" and returns its first dotted
// version token, the exact attestation a daemon with an environment identity
// requires.
func providerVersion(binary string) string {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	output, err := exec.CommandContext(ctx, binary, "--version").CombinedOutput()
	if err != nil {
		return ""
	}
	line := ""
	for _, candidate := range strings.Split(string(output), "\n") {
		if strings.TrimSpace(candidate) != "" {
			line = candidate
			break
		}
	}
	for _, token := range strings.Fields(line) {
		token = strings.TrimPrefix(token, "v")
		token = strings.TrimFunc(token, func(r rune) bool {
			return !(r >= '0' && r <= '9' || r >= 'a' && r <= 'z' ||
				r >= 'A' && r <= 'Z' || r == '.' || r == '-')
		})
		parts := strings.Split(token, ".")
		if len(parts) < 2 || parts[0] == "" {
			continue
		}
		leading := true
		for _, r := range parts[0] {
			if r < '0' || r > '9' {
				leading = false
				break
			}
		}
		if leading && parts[1] != "" && parts[1][0] >= '0' && parts[1][0] <= '9' {
			return token
		}
	}
	return ""
}

func defaultDaemonPath() string {
	candidates := []string{
		filepath.Join("..", "target", "release", "fintwind-daemon.exe"),
		filepath.Join("..", "..", "target", "release", "fintwind-daemon.exe"),
	}
	for _, candidate := range candidates {
		if _, err := os.Stat(candidate); err == nil {
			absolute, _ := filepath.Abs(candidate)
			return absolute
		}
	}
	return candidates[0]
}

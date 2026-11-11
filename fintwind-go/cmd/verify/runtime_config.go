package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"
)

type runtimeConfigScope struct {
	Scope     string `json:"scope"`
	ProjectID string `json:"projectId,omitempty"`
}

type runtimeConfigSnapshot struct {
	Scope    runtimeConfigScope `json:"scope"`
	Revision string             `json:"revision"`
	Shell    *string            `json:"shell"`
	Todo     bool               `json:"todoEnabled"`
	Image    bool               `json:"imageGenerateEnabled"`
	Model    *string            `json:"imageModel"`
}

func runRuntimeConfig(ctx context.Context, executable, round string) []caseResult {
	const scopeName = "realDaemonRuntimeConfig"
	var cases []caseResult
	check := func(name string, ok bool, detail string) bool {
		status := "pass"
		if !ok {
			status = "fail"
		}
		cases = append(cases, caseResult{Name: name, Scope: scopeName, Status: status, Detail: detail})
		return ok
	}
	configDir := filepath.Join(round, "runtime-config-home", "config")
	dataDir := filepath.Join(round, "runtime-config-data")
	procDir := filepath.Join(round, "runtime-config-process")
	if err := os.MkdirAll(configDir, 0o700); err != nil {
		check("create isolated OpenCode config", false, "could not create isolated config directory")
		return cases
	}
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		check("create isolated daemon data", false, "could not create isolated daemon data directory")
		return cases
	}
	primary := filepath.Join(configDir, "opencode.jsonc")
	lower := filepath.Join(configDir, "opencode.json")
	imageDraft := filepath.Join(configDir, ".fintwind-image-generate.jsonc")
	seed := []byte("{\n  // preserve root comment\n  \"experimental\": {\n    // preserve unknown integer exactly\n    \"futureInteger\": 9007199254740993,\n    \"unrelatedFlag\": true,\n    \"image_generate\": {\n      // retain image details\n      \"model\": \"fixture/old\",\n      \"custom\": {\"value\": 9007199254740993}\n    }\n  }\n}\n")
	if err := os.WriteFile(primary, seed, 0o600); err != nil {
		check("seed JSONC config", false, "could not seed isolated JSONC config")
		return cases
	}
	if err := os.WriteFile(lower, []byte("{\"shell\":\"fallback-shell\",\"other\":true}\n"), 0o600); err != nil {
		check("seed fallback JSON config", false, "could not seed isolated fallback config")
		return cases
	}
	token, err := randomToken()
	if err != nil {
		check("generate daemon auth", false, "could not generate daemon auth")
		return cases
	}
	if err := os.MkdirAll(procDir, 0o700); err != nil {
		check("create daemon process directory", false, "could not create owned process directory")
		return cases
	}
	owned, err := ownedprocess.Start(executable, []string{"--bind", "127.0.0.1:0", "--data-dir", dataDir, "--parent-pid", strconv.Itoa(os.Getpid())}, procDir, map[string]string{"FINTWIND_DAEMON_TOKEN": token, "OPENCODE_CONFIG_DIR": configDir})
	if err != nil {
		check("start owned runtime config daemon", false, "could not start owned daemon")
		return cases
	}
	defer owned.Close()
	ready, err := waitReadyLine(ctx, owned.StdoutPath, 15*time.Second)
	if err != nil || ready.PID != int(owned.PID) || ready.ProtocolVersion != daemon.ProtocolVersion {
		check("owned daemon readiness", false, "owned daemon did not report its expected identity")
		return cases
	}
	client, err := daemon.Dial(ctx, ready.Address, token)
	if err != nil {
		check("connect runtime config client", false, "could not connect to owned daemon")
		return cases
	}
	defer client.Close()
	second, err := daemon.Dial(ctx, ready.Address, token)
	if err != nil {
		check("connect second runtime config client", false, "could not connect independent client")
		return cases
	}
	defer second.Close()
	request := func(connection *daemon.Client, command any) (json.RawMessage, error) {
		call, cancel := context.WithTimeout(ctx, 8*time.Second)
		defer cancel()
		return connection.Request(call, command)
	}
	load := func(connection *daemon.Client) (runtimeConfigSnapshot, error) {
		payload, err := request(connection, map[string]any{"type": "loadRuntimeConfig", "scope": runtimeConfigScope{Scope: "global"}})
		if err != nil {
			return runtimeConfigSnapshot{}, err
		}
		var response struct {
			Type     string                `json:"type"`
			Snapshot runtimeConfigSnapshot `json:"snapshot"`
		}
		if err := json.Unmarshal(payload, &response); err != nil || response.Type != "runtimeConfig" || response.Snapshot.Revision == "" {
			return runtimeConfigSnapshot{}, fmt.Errorf("invalid runtime config response")
		}
		return response.Snapshot, nil
	}
	save := func(connection *daemon.Client, snapshot runtimeConfigSnapshot, shell *string, todo, image bool, model *string) (runtimeConfigSnapshot, bool, error) {
		draft := map[string]any{"scope": snapshot.Scope, "expectedRevision": snapshot.Revision, "shell": shell, "todoEnabled": todo, "imageGenerateEnabled": image, "imageModel": model}
		payload, err := request(connection, map[string]any{"type": "saveRuntimeConfig", "draft": draft})
		if err != nil {
			return runtimeConfigSnapshot{}, false, err
		}
		var response struct {
			Type     string                `json:"type"`
			Snapshot runtimeConfigSnapshot `json:"snapshot"`
			Conflict bool                  `json:"conflict"`
		}
		if err := json.Unmarshal(payload, &response); err != nil || response.Type != "runtimeConfig" || response.Snapshot.Revision == "" {
			return runtimeConfigSnapshot{}, false, fmt.Errorf("invalid runtime config response")
		}
		return response.Snapshot, response.Conflict, nil
	}
	initial, err := load(client)
	if err != nil {
		check("load seeded runtime config", false, "daemon rejected seeded JSONC config")
		return cases
	}
	check("image object is initially enabled", initial.Image, "image_generate object must be treated as enabled")
	check("initial image model", initial.Model != nil && *initial.Model == "fixture/old", "initial image model is loaded")
	check("JSONC shell uses lower JSON fallback", initial.Shell != nil && *initial.Shell == "fallback-shell", "selected JSONC without shell retains the lower JSON setting")
	secondStale, err := load(second)
	if err != nil {
		check("second client loads stale revision", false, "independent client could not load runtime config")
		return cases
	}
	updated, conflict, err := save(client, initial, stringPtr("shell-fixture"), true, true, stringPtr("fixture/new"))
	if err != nil || conflict {
		check("save runtime config", false, "valid runtime config draft was not saved")
		return cases
	}
	check("save returned persisted draft", updated.Shell != nil && *updated.Shell == "shell-fixture" && updated.Todo && updated.Image && updated.Model != nil && *updated.Model == "fixture/new", "daemon confirmed shell, todo, image and model values")
	_, conflict, err = save(second, secondStale, nil, false, false, nil)
	check("stale revision conflicts", err == nil && conflict, "stale client receives conflict without applying its draft")
	raw, readErr := os.ReadFile(primary)
	check("conflict preserves disk content", readErr == nil && strings.Contains(string(raw), `"shell": "shell-fixture"`) && strings.Contains(string(raw), "9007199254740993"), "saved values and unknown large integer remain on disk")
	loaded, err := load(client)
	if err != nil {
		check("reload saved config", false, "saved config could not be reloaded")
		return cases
	}
	check("saved values survive reload", loaded.Shell != nil && *loaded.Shell == "shell-fixture" && loaded.Todo && loaded.Image && loaded.Model != nil && *loaded.Model == "fixture/new", "reload returns saved values")
	disabled, conflict, err := save(client, loaded, loaded.Shell, loaded.Todo, false, loaded.Model)
	if err != nil || conflict {
		check("disable image generation", false, "image generation disable was not saved")
		return cases
	}
	sidecar, sidecarErr := os.ReadFile(imageDraft)
	raw, readErr = os.ReadFile(primary)
	check("disable retains image object in sidecar", sidecarErr == nil && strings.Contains(string(sidecar), "fixture/new") && strings.Contains(string(sidecar), "9007199254740993") && readErr == nil && strings.Contains(string(raw), `"image_generate": false`), "disabled image object is retained in sidecar")
	restored, conflict, err := save(client, disabled, disabled.Shell, disabled.Todo, true, disabled.Model)
	if err != nil || conflict {
		check("re-enable image generation", false, "image generation re-enable was not saved")
		return cases
	}
	raw, readErr = os.ReadFile(primary)
	check("re-enable preserves image fields and comments", readErr == nil && strings.Contains(string(raw), "fixture/new") && strings.Contains(string(raw), "custom") && strings.Contains(string(raw), "retain image details") && strings.Contains(string(raw), "9007199254740993"), "enabled image object keeps custom syntax")
	_, conflict, err = save(client, restored, nil, restored.Todo, restored.Image, restored.Model)
	if err != nil || conflict {
		check("clear shell", false, "shell reset was not saved")
		return cases
	}
	raw, readErr = os.ReadFile(primary)
	lowerRaw, lowerErr := os.ReadFile(lower)
	check("shell reset removes fallback value", readErr == nil && !strings.Contains(string(raw), `"shell":`) && lowerErr == nil && !strings.Contains(string(lowerRaw), `"shell"`), "shell is absent from both JSONC and fallback JSON")
	check("fallback unrelated field remains", lowerErr == nil && strings.Contains(string(lowerRaw), `"other":true`), "fallback JSON keeps unrelated data")
	stableBytes, _ := os.ReadFile(primary)
	stableSnapshot, stableErr := load(client)
	if stableErr == nil {
		_, _, invalidErr := save(client, stableSnapshot, nil, stableSnapshot.Todo, stableSnapshot.Image, stringPtr("invalid-model"))
		afterInvalid, _ := os.ReadFile(primary)
		check("reject invalid image model", invalidErr != nil && string(afterInvalid) == string(stableBytes), "invalid model is rejected without changing config")
	} else {
		check("reject invalid image model", false, "could not load config before invalid model check")
	}
	unknownScope := runtimeConfigScope{Scope: "project", ProjectID: "00000000-0000-4000-8000-000000000001"}
	unknownPayload, unknownErr := request(client, map[string]any{"type": "loadRuntimeConfig", "scope": unknownScope})
	_ = unknownPayload
	check("reject unknown project scope", unknownErr != nil, "project absent from the isolated daemon catalog is rejected")
	projectRoot := filepath.Join(round, "runtime-project")
	nestedRoot := filepath.Join(projectRoot, ".opencode")
	if err := os.MkdirAll(nestedRoot, 0o700); err != nil {
		check("create isolated project", false, err.Error())
		return cases
	}
	_ = os.WriteFile(filepath.Join(projectRoot, "opencode.jsonc"), []byte("{\"shell\":\"lower-project-shell\"}"), 0o600)
	nestedFile := filepath.Join(nestedRoot, "opencode.jsonc")
	_ = os.WriteFile(nestedFile, []byte("{\n // project owned comment\n \"experimental\": {\"todo\":false}, \"projectOnly\":true\n}"), 0o600)
	const projectID = "70000000-0000-4000-8000-000000000007"
	_, projectErr := request(client, map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{map[string]any{"id": projectID, "name": "Runtime fixture", "path": projectRoot, "environment_id": "", "created_at": uint64(1700000000)}}, "sessions": []any{}, "skeletonSessionIds": []string{}})
	check("register local project through daemon", projectErr == nil, "project configuration scope is resolved from a registered project")
	projectScope := runtimeConfigScope{Scope: "project", ProjectID: projectID}
	projectPayload, projectErr := request(client, map[string]any{"type": "loadRuntimeConfig", "scope": projectScope})
	var projectBody struct {
		Snapshot runtimeConfigSnapshot `json:"snapshot"`
	}
	_ = json.Unmarshal(projectPayload, &projectBody)
	check("project scope reads nested config", projectErr == nil && projectBody.Snapshot.Revision != "" && projectBody.Snapshot.Shell == nil, "nested .opencode file has precedence over the root project file")
	globalBefore, _ := os.ReadFile(primary)
	_, projectConflict, projectErr := save(client, projectBody.Snapshot, stringPtr("project-shell"), true, false, nil)
	nestedAfter, _ := os.ReadFile(nestedFile)
	globalAfter, _ := os.ReadFile(primary)
	check("project save stays inside selected scope", projectErr == nil && !projectConflict && strings.Contains(string(nestedAfter), "project-shell") && strings.Contains(string(nestedAfter), "project owned comment") && string(globalBefore) == string(globalAfter), "project edit keeps comments and does not change global settings")

	for _, malformed := range []struct{ name, content string }{{"malformed", "{ invalid"}, {"non-object", "[]"}} {
		before, _ := os.ReadFile(primary)
		if writeErr := os.WriteFile(primary, []byte(malformed.content), 0o600); writeErr != nil {
			check("seed "+malformed.name+" config", false, "could not seed isolated invalid config")
			continue
		}
		_, loadErr := load(client)
		after, _ := os.ReadFile(primary)
		check("reject "+malformed.name+" config without overwrite", loadErr != nil && string(after) == malformed.content && string(before) != string(after), "invalid config is rejected and left unchanged")
		_ = os.WriteFile(primary, before, 0o600)
	}
	current, readErr := os.ReadFile(primary)
	if readErr == nil {
		recovered := append([]byte("// recovered transaction\n"), current...)
		journal, _ := json.Marshal([]map[string]any{{"name": "opencode.jsonc", "before": string(current), "after": string(recovered)}})
		journalPath := filepath.Join(configDir, ".fintwind-runtime-transaction.json")
		if writeErr := os.WriteFile(journalPath, journal, 0o600); writeErr != nil {
			check("seed recoverable config transaction", false, "could not write isolated recovery journal")
		} else {
			_, recoveryErr := load(client)
			afterRecovery, _ := os.ReadFile(primary)
			_, journalErr := os.Stat(journalPath)
			check("recover pending config transaction", recoveryErr == nil && string(afterRecovery) == string(recovered) && os.IsNotExist(journalErr), "load completes known before/after transaction")
		}
		beforeDrift := string(recovered)
		drift := []byte("{\"external\":true}\n")
		if err := os.WriteFile(primary, drift, 0o600); err == nil {
			driftJournal, _ := json.Marshal([]map[string]any{{"name": "opencode.jsonc", "before": beforeDrift, "after": string(recovered)}})
			_ = os.WriteFile(filepath.Join(configDir, ".fintwind-runtime-transaction.json"), driftJournal, 0o600)
			_, recoveryErr := load(client)
			afterDrift, _ := os.ReadFile(primary)
			check("external edit blocks recovery without data loss", recoveryErr != nil && string(afterDrift) == string(drift), "external bytes remain untouched while recovery is blocked")
			_ = os.Remove(filepath.Join(configDir, ".fintwind-runtime-transaction.json"))
			_ = os.WriteFile(primary, current, 0o600)
		}
	} else {
		check("recover pending config transaction", false, "could not read isolated config before recovery check")
		check("external edit blocks recovery without data loss", false, "could not read isolated config before recovery check")
	}
	return cases
}

func stringPtr(value string) *string { return &value }

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

type check struct {
	Name   string `json:"name"`
	OK     bool   `json:"ok"`
	Detail string `json:"detail,omitempty"`
}
type results struct{ checks []check }

// diagDir is the owned round root; diagnostics are evidence-only and never
// change assertions, fixture data, or synchronization.
var diagDir string

func diagEvent(ev daemon.Event) {
	if diagDir == "" {
		return
	}
	raw, _ := json.Marshal(map[string]any{
		"t": time.Now().Format(time.RFC3339Nano), "kind": ev.Kind,
		"sessionId": ev.SessionID, "runtimeId": ev.RuntimeID, "payload": json.RawMessage(ev.Payload),
	})
	file, err := os.OpenFile(filepath.Join(diagDir, "background-events.jsonl"), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		return
	}
	_, _ = file.Write(append(raw, '\n'))
	_ = file.Close()
}

func diagHydrate(label string, state map[string]json.RawMessage) {
	if diagDir == "" {
		return
	}
	raw, _ := json.MarshalIndent(state, "", "  ")
	_ = os.WriteFile(filepath.Join(diagDir, "hydrate-"+label+".json"), raw, 0600)
}

func (r *results) add(name string, err error) {
	c := check{Name: name, OK: err == nil}
	if err != nil {
		c.Detail = err.Error()
	}
	r.checks = append(r.checks, c)
}

func main() {
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		if err := serveFixture(); err != nil {
			fmt.Fprintln(os.Stderr, "fixture:", err)
			os.Exit(2)
		}
		return
	}
	binary := flag.String("daemon", "", "absolute path to the isolated daemon binary")
	metadataBudgetOnly := flag.Bool("metadata-budget-only", false, "run the isolated metadata budget E2E after core session admission")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 540*time.Second)
	defer cancel()
	if err := run(ctx, *binary, *metadataBudgetOnly); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, binary string, metadataBudgetOnly bool) (err error) {
	if !filepath.IsAbs(binary) {
		return errors.New("absolute daemon path is required")
	}
	if daemonProtocol() != 25 {
		return fmt.Errorf("harness protocol is %d, want 25", daemonProtocol())
	}
	for _, key := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "FINTWIND_AGENT_REAL_PROVIDER"} {
		if strings.TrimSpace(os.Getenv(key)) != "" {
			return fmt.Errorf("credential variable %s must be unset", key)
		}
	}
	self, e := os.Executable()
	if e != nil {
		return e
	}
	hash, e := fileHash(binary)
	if e != nil {
		return e
	}
	root, e := ownedprocess.NewRoundDir()
	if e != nil {
		return e
	}
	diagDir = root
	workspace := filepath.Join(root, "workspace", "project")
	if e = os.MkdirAll(workspace, 0700); e != nil {
		return e
	}
	res := &results{checks: []check{}}
	defer func() {
		ops := res.checks
		passed := err == nil
		repoRoot := filepath.Dir(filepath.Dir(filepath.Dir(binary)))
		script := filepath.Join(repoRoot, "tools", "e2e", "subagentcheck", "run.ps1")
		command := "pwsh -NoProfile -File " + script + " -Harness " + self + " -Daemon " + binary
		payload := map[string]any{"passed": passed, "command": command, "buildCommands": []string{"pwsh -NoProfile -File tools/e2e/subagentcheck/build-budget.ps1 -OutputDir target/go-backend-subagent-budget"}, "daemonBinary": binary, "daemonSha256": hash, "protocol": 25, "sessionProvider": "opencode", "cursorProvider": "openCode", "fixture": "source-constructed OpenCode-compatible fake HTTP/SSE; not real provider capture", "realModels": false, "realRequests": 0, "checks": ops, "coverage": coverageOf(ops), "uncovered": []string{"successful stale-generation result discarded (not safely constructible without a product test hook)", "successful stale-revision result discarded by actor (server response completion does not prove actor consumption)", "real provider capture and paid/provider requests", "visual UI, MCP authorization flow (separate mcpauthcheck is not run)"}}
		if metadataBudgetOnly {
			payload["skipped"] = []string{"remaining lifecycle, roster race, controls, history edit, and legacy-save checks (dedicated metadata-budget E2E mode)"}
		}
		if err != nil {
			payload["error"] = err.Error()
		}
		raw, _ := json.MarshalIndent(payload, "", "  ")
		_ = os.WriteFile(filepath.Join(root, "subagent-result.json"), raw, 0600)
		fmt.Println("[REPORT]", filepath.Join(root, "subagent-result.json"))
	}()
	logs := filepath.Join(root, "daemon-logs")
	if e = os.MkdirAll(logs, 0700); e != nil {
		return e
	}
	token := uuid.NewString()
	daemonDir := filepath.Join(root, "data")
	if e = os.MkdirAll(daemonDir, 0700); e != nil {
		return e
	}
	if e = os.MkdirAll(filepath.Join(root, "home"), 0700); e != nil {
		return e
	}
	settingsRaw, _ := json.Marshal(map[string]string{"opencode_binary_path": self})
	if e = os.WriteFile(filepath.Join(daemonDir, "daemon.json"), settingsRaw, 0600); e != nil {
		return e
	}
	if e = setFixtureMode(root, "default"); e != nil {
		return e
	}
	child, e := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", daemonDir, "--bind", "127.0.0.1:0"}, logs, root, ownedprocess.BuildEnv(root, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if e != nil {
		return e
	}
	defer child.Close()
	address, e := waitDaemon(ctx, child)
	if e != nil {
		return e
	}
	client, e := daemon.Dial(ctx, address, token)
	if e != nil {
		return e
	}
	defer client.Close()
	sid, rid, pid := uuid.NewString(), uuid.NewString(), uuid.NewString()
	options := map[string]any{"binary": self, "cwd": workspace, "mode": "ask", "interactionMode": "code", "model": "fixture/no-provider"}
	if _, e = client.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": options}); e != nil {
		return fmt.Errorf("start daemon runtime: %w", e)
	}
	if e = awaitNotice(ctx, client, "connected", sid); e != nil {
		return fmt.Errorf("wait connected: %w", e)
	}
	now := uint64(time.Now().Unix())
	project := map[string]any{"id": pid, "name": "subagent fixture", "path": workspace, "created_at": now}
	turnID := uuid.NewString()
	receipt := model.NewSubmissionRecord(sid, turnID, "subagent lifecycle", nil)
	turn := map[string]any{"id": turnID, "status": "running", "turn_count": 1, "started_at": now, "submission": receipt.Receipt}
	user := map[string]any{"id": uuid.NewString(), "turn_id": turnID, "role": "user", "content": "subagent lifecycle", "created_at": now, "attachments": []any{}, "streaming": false}
	session := map[string]any{"id": sid, "project_id": pid, "title": "subagent fixture", "provider": "opencode", "status": "working", "created_at": now, "updated_at": now, "detail_loaded": true, "provider_cursor": map[string]string{"provider": "openCode", "sessionId": rootNative}, "messages": []any{user}, "turns": []any{turn}, "transcript_blocks": []any{}}
	if _, e = client.RequestFor(ctx, sid, "", map[string]any{"type": "acceptSubmission", "project": project, "session": session, "submission": receipt}); e != nil {
		return fmt.Errorf("admit root session: %w", e)
	}
	if _, e = client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]string{"kind": "local"}}); e != nil {
		return fmt.Errorf("dispatch root: %w", e)
	}
	events, e := collectUntil(ctx, client, "turnFinished", sid, rid)
	if e != nil {
		return e
	}
	if e = verifyLifecycle(events, res); e != nil {
		return e
	}
	state, e := hydrate(ctx, client, sid)
	if e != nil {
		return e
	}
	items, e := backgroundItems(state)
	if e != nil {
		return e
	}
	if len(items) != 1 || items[0].Key.ProviderID != childOne || items[0].Status != "completed" {
		return fmt.Errorf("root SQLite snapshot child roster unexpected: %+v", items)
	}
	if len(items[0].OriginActivityIDs) != 1 || items[0].OriginActivityIDs[0] != "call_task_exact" || strings.Contains(string(state["background_work"]), "call_task_one") {
		return fmt.Errorf("exact target did not win over task_id alias or pending call survived root turn: %+v", items[0].OriginActivityIDs)
	}
	res.add("root-background-snapshot-hydrated", nil)
	if metadataBudgetOnly {
		return runMetadataBudgetRestart(ctx, binary, client, child, sid, rid, self, workspace, daemonDir, token, root, res, true)
	}
	if provider := stringField(state, "provider"); provider != "opencode" {
		return fmt.Errorf("session provider %q, want opencode", provider)
	}
	if cursor := rawObject(state["provider_cursor"]); cursor["provider"] != "openCode" {
		return fmt.Errorf("native cursor provider %v, want openCode", cursor["provider"])
	}
	res.add("provider-and-cursor-casing", nil)
	// A later root task may revive only the explicitly targeted known child.
	turn2 := uuid.NewString()
	if e = admitTurn(ctx, client, sid, pid, workspace, state, turn2, "continue child", 2, project); e != nil {
		return e
	}
	if _, e = client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turn2, "workspace": map[string]string{"kind": "local"}}); e != nil {
		return e
	}
	continued, e := collectUntil(ctx, client, "turnFinished", sid, rid)
	if e != nil {
		return e
	}
	state, e = hydrate(ctx, client, sid)
	if e != nil {
		return e
	}
	items, e = backgroundItems(state)
	if e != nil {
		return e
	}
	if len(items) != 1 || len(items[0].OriginActivityIDs) != 2 || items[0].OriginActivityIDs[0] != "call_task_exact" || items[0].OriginActivityIDs[1] != "call_task_two" {
		return fmt.Errorf("continuation origin IDs not uniquely appended: %+v", items)
	}
	if strings.Count(string(state["background_work"]), "call_task_two") != 1 {
		return errors.New("duplicate task replay duplicated continuation origin")
	}
	res.add("exact-child-target-and-origin-dedup", nil)
	if e = verifyContinuation(continued); e != nil {
		return e
	}
	res.add("continuation-revive-and-root-end-cleanup", nil)
	// Start/refresh checks operate on the live actor and on the same isolated fake.
	if _, e = client.RequestFor(ctx, sid, rid, map[string]any{"type": "refreshBackgroundWork"}); e != nil {
		return fmt.Errorf("refresh roster: %w", e)
	}
	refreshEvents, e := collectBackground(ctx, client, sid, rid, "reconcileLive")
	if e != nil {
		return e
	}
	if !hasTaggedReconcile(refreshEvents) {
		return errors.New("refresh did not emit tagged reconcileLive")
	}
	if diagState, diagErr := hydrate(ctx, client, sid); diagErr == nil {
		diagHydrate("refresh1-default", diagState)
	}
	res.add("async-refresh-tagged-reconcile", nil)
	if _, e = client.RequestFor(ctx, sid, rid, map[string]any{"type": "refreshBackgroundWork"}); e != nil {
		return e
	}
	if e = waitNotice(ctx, client, "error", sid, rid); e != nil {
		return fmt.Errorf("malformed active refresh did not fail safely: %w", e)
	}
	state, e = hydrate(ctx, client, sid)
	if e != nil {
		return e
	}
	items, e = backgroundItems(state)
	if e != nil {
		return e
	}
	if len(items) != 1 || items[0].Key.ProviderID != childOne {
		return errors.New("failed active refresh replaced the prior child registry")
	}
	res.add("malformed-active-refresh-preserves-old-state", nil)
	if e = runSlowRosterAndPartialResults(ctx, client, sid, rid, pid, workspace, state, project, root, res); e != nil {
		return e
	}
	state, e = hydrate(ctx, client, sid)
	if e != nil {
		return e
	}
	if e = runCapacityBoundary(ctx, client, sid, rid, pid, workspace, state, project, res); e != nil {
		return e
	}
	state, e = hydrate(ctx, client, sid)
	if e != nil {
		return e
	}
	// Stop and compact controls must reject unknown/root IDs without issuing native requests.
	if e = checkControlRejections(ctx, client, sid, rid, res); e != nil {
		return e
	}
	if _, e = client.RequestFor(ctx, sid, rid, map[string]any{"type": "compactChildSession", "controlId": childOne}); e != nil {
		return fmt.Errorf("valid child compaction: %w", e)
	}
	res.add("compact-targets-child-native-session", nil)
	// A second discovered child stays live after root completion, accepts its
	// own scoped permission/form responses, and can be interrupted by registry key.
	turn3 := uuid.NewString()
	if e = admitTurn(ctx, client, sid, pid, workspace, state, turn3, "active child control", 3, project); e != nil {
		return e
	}
	if _, e = client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turn3, "workspace": map[string]string{"kind": "local"}}); e != nil {
		return e
	}
	activeEvents, e := collectUntil(ctx, client, "turnFinished", sid, rid)
	if e != nil {
		return e
	}
	if _, e = client.RequestFor(ctx, sid, "", map[string]any{"type": "rewindSessionToMessage", "turnCount": 1}); e == nil || !strings.Contains(e.Error(), "stop background work") {
		return fmt.Errorf("rewind during live child should be explicitly refused, got %v", e)
	}
	res.add("history-edit-refused-while-child-live", nil)
	if e = checkScopedReplies(ctx, client, sid, rid, activeEvents, res); e != nil {
		return e
	}
	key := map[string]any{"kind": "subagent", "providerId": childTwo}
	if _, e = client.RequestFor(ctx, sid, rid, map[string]any{"type": "stopBackgroundWork", "key": key, "controlId": childTwo}); e != nil {
		return fmt.Errorf("valid child stop: %w", e)
	}
	if _, e = client.RequestFor(ctx, sid, rid, map[string]any{"type": "respond", "requestId": "perm_stop_pending", "optionId": "once"}); e == nil || !strings.Contains(strings.ToLower(e.Error()), "expired") {
		return fmt.Errorf("stopped child request did not expire: %v", e)
	}
	res.add("stop-expires-child-request-scope", nil)
	res.add("valid-child-stop-interrupt-control", nil)
	state, e = hydrate(ctx, client, sid)
	if e != nil {
		return e
	}
	items, e = backgroundItems(state)
	if e != nil {
		return e
	}
	if len(items) != 4 {
		return fmt.Errorf("expected four root children after scoped control and roster races, got %+v", items)
	}
	if e = runUnknownTarget(ctx, client, sid, rid, pid, workspace, state, project, root, 4, childUnknownEmpty, "unknown empty child", "unknown-empty", res); e != nil {
		return e
	}
	state, e = hydrate(ctx, client, sid)
	if e != nil {
		return e
	}
	if e = runUnknownTarget(ctx, client, sid, rid, pid, workspace, state, project, root, 5, childUnknownOld, "unknown old child", "unknown-old", res); e != nil {
		return e
	}
	state, e = hydrate(ctx, client, sid)
	if e != nil {
		return e
	}
	if e = checkFixtureAudit(filepath.Join(root, "home", "subagent-audit.json"), res); e != nil {
		return e
	}
	// Legacy save omitting background_work is exercised through the public save RPC.
	if e = checkLegacySave(ctx, client, sid, state, project, res); e != nil {
		return e
	}
	// Stop the runtime and process only via owned handles. Reopen with SQLite to
	// assert durable child state and raw transcript blocks.
	if _, e = client.RequestFor(ctx, sid, rid, map[string]any{"type": "closeSession"}); e != nil {
		return e
	}
	client.Close()
	_ = child.Close()
	restartLogs := filepath.Join(root, "daemon-logs-restart")
	if e = os.MkdirAll(restartLogs, 0700); e != nil {
		return e
	}
	child2, e := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", daemonDir, "--bind", "127.0.0.1:0"}, restartLogs, root, ownedprocess.BuildEnv(root, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if e != nil {
		return e
	}
	defer child2.Close()
	address2, e := waitDaemon(ctx, child2)
	if e != nil {
		return e
	}
	client2, e := daemon.Dial(ctx, address2, token)
	if e != nil {
		return e
	}
	defer client2.Close()
	state2, e := hydrate(ctx, client2, sid)
	if e != nil {
		return e
	}
	items2, e := backgroundItems(state2)
	if e != nil {
		return e
	}
	if len(items2) != 6 || !hasBackgroundID(items2, childOne) || !hasBackgroundID(items2, childUnknownEmpty) || !hasBackgroundID(items2, childUnknownOld) || !hasBackgroundID(items2, childRace) || !hasBackgroundID(items2, childRevision) {
		return fmt.Errorf("background roster missing after daemon restart: %+v", items2)
	}
	res.add("sqlite-background-restart", nil)
	if e = verifyRawTranscript(findBackground(items2, childOne), state2); e != nil {
		return e
	}
	res.add("raw-background-transcript-blocks", nil)
	if e = verifyUnknownSnapshots(state2, res); e != nil {
		return e
	}
	if e = runHistoryEdits(ctx, client2, sid, state2, project, root, res); e != nil {
		return e
	}
	if e = runMetadataBudgetRestart(ctx, binary, client2, child2, sid, rid, self, workspace, daemonDir, token, root, res, false); e != nil {
		return e
	}
	return nil
}

func daemonProtocol() int { return daemon.ProtocolVersion }
func fileHash(path string) (string, error) {
	b, e := os.ReadFile(path)
	if e != nil {
		return "", e
	}
	s := sha256.Sum256(b)
	return hex.EncodeToString(s[:]), nil
}
func coverageOf(checks []check) []string {
	out := []string{}
	for _, c := range checks {
		if c.OK {
			out = append(out, c.Name)
		}
	}
	return out
}
func waitDaemon(ctx context.Context, p *ownedprocess.Owned) (string, error) {
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	for {
		b, _ := os.ReadFile(p.StdoutPath)
		var v struct {
			Address string `json:"address"`
			PID     uint32 `json:"pid"`
		}
		if json.Unmarshal(b, &v) == nil && v.PID == p.PID && v.Address != "" {
			return v.Address, nil
		}
		if done, _ := p.Exited(0); done {
			return "", errors.New("owned daemon exited before readiness")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-ticker.C:
		}
	}
}
func awaitNotice(ctx context.Context, c *daemon.Client, kind, sid string) error {
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case ev, ok := <-c.Events():
			if !ok {
				return errors.New("daemon event stream closed")
			}
			diagEvent(ev)
			if ev.Type == "event" {
				c.Ack(ev)
			}
			if ev.Kind == kind && (ev.SessionID == sid || ev.SessionID == "") {
				return nil
			}
		}
	}
}
func collectUntil(ctx context.Context, c *daemon.Client, kind, sid, rid string) ([]daemon.Event, error) {
	var out []daemon.Event
	for {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case ev, ok := <-c.Events():
			if !ok {
				return nil, errors.New("daemon event stream closed before " + kind)
			}
			diagEvent(ev)
			if ev.Type == "event" {
				c.Ack(ev)
			}
			if ev.SessionID == sid && (ev.RuntimeID == rid || ev.RuntimeID == "") {
				out = append(out, ev)
			}
			if ev.Kind == kind && ev.SessionID == sid {
				return out, nil
			}
		}
	}
}
func collectBackground(ctx context.Context, c *daemon.Client, sid, rid, tag string) ([]daemon.Event, error) {
	return collectBackgroundWithin(ctx, c, sid, rid, tag, 8*time.Second)
}
func collectBackgroundWithin(ctx context.Context, c *daemon.Client, sid, rid, tag string, timeout time.Duration) ([]daemon.Event, error) {
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	var out []daemon.Event
	for {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-timer.C:
			return nil, fmt.Errorf("timed out waiting for backgroundWork %s", tag)
		case ev, ok := <-c.Events():
			if !ok {
				return nil, errors.New("daemon stream closed")
			}
			diagEvent(ev)
			if ev.Type == "event" {
				c.Ack(ev)
			}
			if ev.SessionID != sid {
				continue
			}
			out = append(out, ev)
			if ev.Kind == "backgroundWork" && strings.Contains(string(ev.Payload), `"type":"`+tag+`"`) {
				return out, nil
			}
		}
	}
}
func waitNotice(ctx context.Context, c *daemon.Client, kind, sid, rid string) error {
	timer := time.NewTimer(8 * time.Second)
	defer timer.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-timer.C:
			return fmt.Errorf("timed out waiting for %s", kind)
		case ev, ok := <-c.Events():
			if !ok {
				return errors.New("daemon stream closed")
			}
			diagEvent(ev)
			if ev.Type == "event" {
				c.Ack(ev)
			}
			if ev.SessionID == sid && (ev.RuntimeID == rid || ev.RuntimeID == "") && ev.Kind == kind {
				return nil
			}
		}
	}
}
func hydrate(ctx context.Context, c *daemon.Client, sid string) (map[string]json.RawMessage, error) {
	raw, e := c.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sid})
	if e != nil {
		return nil, e
	}
	var result struct {
		Session map[string]json.RawMessage `json:"session"`
	}
	if e = json.Unmarshal(raw, &result); e != nil {
		return nil, e
	}
	if result.Session == nil {
		return nil, fmt.Errorf("hydrate omitted session: %s", raw)
	}
	return result.Session, nil
}
func rawObject(raw json.RawMessage) map[string]any {
	m := map[string]any{}
	_ = json.Unmarshal(raw, &m)
	return m
}
func stringField(s map[string]json.RawMessage, key string) string {
	var out string
	_ = json.Unmarshal(s[key], &out)
	return out
}

type backgroundItem struct {
	Key struct {
		Kind       string `json:"kind"`
		ProviderID string `json:"providerId"`
	} `json:"key"`
	Title             string   `json:"title"`
	Detail            *string  `json:"detail"`
	Command           *string  `json:"command"`
	Role              *string  `json:"role"`
	Model             *string  `json:"model"`
	ParentID          *string  `json:"parentId"`
	Status            string   `json:"status"`
	OriginActivityIDs []string `json:"originActivityIds"`
	ControlID         *string  `json:"controlId"`
	OutputTruncated   bool     `json:"outputTruncated"`
}

// resumeStartOptions builds start options for resuming an existing local task.
// The product requires an explicit providerCursor to load the saved snapshot
// (manager.start); a cursor-less start creates a new native with a nil snapshot
// and never persists refreshes. The cursor is read from the hydrated task state
// and validated against the fixture native identity, never fabricated.
func resumeStartOptions(ctx context.Context, c *daemon.Client, sid, self, workspace string) (map[string]any, error) {
	state, err := hydrate(ctx, c, sid)
	if err != nil {
		return nil, err
	}
	cursor := rawObject(state["provider_cursor"])
	provider, _ := cursor["provider"].(string)
	native, _ := cursor["sessionId"].(string)
	if provider != "openCode" || native != rootNative {
		return nil, fmt.Errorf("resume cursor invalid: provider=%q sessionId=%q, want openCode/%s", provider, native, rootNative)
	}
	return map[string]any{
		"binary": self, "cwd": workspace, "mode": "ask", "interactionMode": "code", "model": "fixture/no-provider",
		"providerCursor": map[string]string{"provider": provider, "sessionId": native},
	}, nil
}

func runMetadataBudgetRestart(ctx context.Context, binary string, client *daemon.Client, owned *ownedprocess.Owned, sid, rid, self, workspace, daemonDir, token, root string, res *results, runtimeAlreadyStarted bool) error {
	if err := setFixtureMode(root, "budget-roster"); err != nil {
		return err
	}
	if !runtimeAlreadyStarted {
		resumeOptions, err := resumeStartOptions(ctx, client, sid, self, workspace)
		if err != nil {
			return fmt.Errorf("resume isolated runtime for metadata budget: %w", err)
		}
		if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": resumeOptions}); err != nil {
			return fmt.Errorf("resume isolated runtime for metadata budget: %w", err)
		}
		if err := awaitNotice(ctx, client, "connected", sid); err != nil {
			return fmt.Errorf("wait resumed runtime: %w", err)
		}
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "refreshBackgroundWork"}); err != nil {
		return fmt.Errorf("refresh metadata-heavy roster: %w", err)
	}
	if _, err := collectBackgroundWithin(ctx, client, sid, rid, "reconcileLive", 90*time.Second); err != nil {
		return err
	}
	state, err := waitForBackgroundSnapshotCount(ctx, client, sid, 76)
	if err != nil {
		return err
	}
	raw := state["background_work"]
	if len(raw) == 0 || len(raw) > 8<<20 {
		return fmt.Errorf("complete background_work JSON size=%d, want 1..%d", len(raw), 8<<20)
	}
	snaps, err := snapshotsFromState(state)
	if err != nil {
		return err
	}
	byID := make(map[string]backgroundSnapshot, len(snaps))
	truncated := false
	for _, snap := range snaps {
		byID[snap.Item.Key.ProviderID] = snap
		truncated = truncated || snap.Item.OutputTruncated
	}
	if len(snaps) != 76 {
		return fmt.Errorf("metadata roster snapshots=%d, want 76", len(snaps))
	}
	short, ok := byID["ses_budget_short"]
	if !ok || short.Item.Title != "短 metadata" || short.Item.Role == nil || *short.Item.Role != "reviewer" || short.Item.Model == nil || *short.Item.Model != "fixture/model-v1" || short.Item.Command == nil || *short.Item.Command != "保留这个 prompt" || short.Item.Status != "failed" {
		return fmt.Errorf("short metadata/status was not preserved exactly: %+v", short.Item)
	}
	large, ok := byID["ses_budget_000"]
	if !ok || !large.Item.OutputTruncated || large.Item.Detail == nil || *large.Item.Detail == "" || large.Item.Status != "completed" || len(large.Item.Title) > 2048 || large.Item.Role == nil || len(*large.Item.Role) > 256 || large.Item.Model == nil || len(*large.Item.Model) > 1024 || large.Item.Command == nil || len(*large.Item.Command) > 16000 {
		return fmt.Errorf("large metadata did not carry explicit truncation/detail: %+v", large.Item)
	}
	if !truncated {
		return errors.New("metadata budget overflow did not mark any snapshot OutputTruncated")
	}
	if child, ok := byID[childOne]; !ok || child.Item.Key.ProviderID != childOne || child.Item.ControlID == nil || *child.Item.ControlID != childOne || child.Item.ParentID == nil || *child.Item.ParentID != rootNative || child.Item.Status != "stopped" {
		return fmt.Errorf("identity/control/parent/outcome mapping changed: %+v", child.Item)
	}
	if !strings.Contains(string(state["messages"]), "capacity boundary") && !strings.Contains(string(state["messages"]), "subagent lifecycle") {
		return errors.New("ordinary root task text was lost while bounding child metadata")
	}
	res.add("complete-metadata-json-budget-and-truncation-marker", nil)
	initialSnapshotHash := sha256.Sum256(raw)
	res.checks = append(res.checks, check{Name: "metadata-snapshot-json-metrics", OK: true, Detail: fmt.Sprintf("bytes=%d/8388608 sha256=%s", len(raw), hex.EncodeToString(initialSnapshotHash[:]))})
	res.add("short-metadata-and-outcome-status-preserved", nil)
	res.add("metadata-budget-root-text-and-child-identity-preserved", nil)
	if _, err = client.RequestFor(ctx, sid, rid, map[string]any{"type": "closeSession"}); err != nil {
		return fmt.Errorf("close budget runtime: %w", err)
	}
	client.Close()
	if err = owned.Close(); err != nil {
		return fmt.Errorf("close owned pre-budget daemon: %w", err)
	}
	logs := filepath.Join(root, "daemon-logs-budget-restart")
	if err = os.MkdirAll(logs, 0700); err != nil {
		return err
	}
	owned2, err := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", daemonDir, "--bind", "127.0.0.1:0"}, logs, root, ownedprocess.BuildEnv(root, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if err != nil {
		return err
	}
	defer owned2.Close()
	address, err := waitDaemon(ctx, owned2)
	if err != nil {
		return err
	}
	client2, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client2.Close()
	state, err = hydrate(ctx, client2, sid)
	if err != nil {
		return fmt.Errorf("hydrate after metadata-budget restart: %w", err)
	}
	if len(state["background_work"]) == 0 || len(state["background_work"]) > 8<<20 {
		return fmt.Errorf("restarted background_work JSON size=%d exceeds loader budget", len(state["background_work"]))
	}
	restartedSnapshotHash := sha256.Sum256(state["background_work"])
	snaps, err = snapshotsFromState(state)
	if err != nil {
		return fmt.Errorf("decode roster after metadata-budget restart: %w", err)
	}
	byID = make(map[string]backgroundSnapshot, len(snaps))
	for _, snap := range snaps {
		byID[snap.Item.Key.ProviderID] = snap
	}
	if len(snaps) != 76 || byID["ses_budget_short"].Item.Model == nil || *byID["ses_budget_short"].Item.Model != "fixture/model-v1" || !byID["ses_budget_000"].Item.OutputTruncated || !strings.Contains(string(state["messages"]), "capacity boundary") && !strings.Contains(string(state["messages"]), "subagent lifecycle") {
		return errors.New("metadata roster, truncation marker, or root text did not survive daemon restart")
	}
	resumeOptions, err := resumeStartOptions(ctx, client2, sid, self, workspace)
	if err != nil {
		return fmt.Errorf("resume session after metadata-budget restart: %w", err)
	}
	if _, err = client2.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": resumeOptions}); err != nil {
		return fmt.Errorf("resume session after metadata-budget restart: %w", err)
	}
	if err = awaitNotice(ctx, client2, "connected", sid); err != nil {
		return fmt.Errorf("wait session resume after metadata-budget restart: %w", err)
	}
	if _, err = client2.RequestFor(ctx, sid, rid, map[string]any{"type": "closeSession"}); err != nil {
		return fmt.Errorf("close resumed metadata-budget session: %w", err)
	}
	res.add("metadata-budget-close-restart-hydrate-resume", nil)
	res.checks = append(res.checks, check{Name: "restarted-metadata-snapshot-json-metrics", OK: true, Detail: fmt.Sprintf("bytes=%d/8388608 sha256=%s", len(state["background_work"]), hex.EncodeToString(restartedSnapshotHash[:]))})
	return nil
}

func waitForBackgroundSnapshotCount(ctx context.Context, client *daemon.Client, sid string, want int) (map[string]json.RawMessage, error) {
	timer := time.NewTimer(30 * time.Second)
	defer timer.Stop()
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()
	lastCount := 0
	for {
		state, err := hydrate(ctx, client, sid)
		if err != nil {
			return nil, err
		}
		items, err := backgroundItems(state)
		if err == nil {
			lastCount = len(items)
			if lastCount == want {
				return state, nil
			}
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-timer.C:
			// Evidence-only: record whatever error/notice/stream-end events are
			// already buffered so a timeout is not mistaken for "no error". This
			// runs solely on the failure path and never alters a passing sequence.
			drainBufferedEvents(client)
			return nil, fmt.Errorf("timed out waiting for %d background snapshots (last count %d)", want, lastCount)
		case <-ticker.C:
		}
	}
}

// drainBufferedEvents records already-buffered daemon events without blocking.
// It is diagnostics-only, invoked exclusively on a failure path, so it cannot
// change assertions, fixture data, or synchronization on any passing run.
func drainBufferedEvents(c *daemon.Client) {
	for {
		select {
		case ev, ok := <-c.Events():
			if !ok {
				return
			}
			diagEvent(ev)
			if ev.Type == "event" {
				c.Ack(ev)
			}
		default:
			return
		}
	}
}

type backgroundSnapshot struct {
	Item       backgroundItem `json:"item"`
	Transcript struct {
		Messages         []json.RawMessage `json:"messages"`
		TranscriptBlocks []json.RawMessage `json:"transcriptBlocks"`
		Turns            []json.RawMessage `json:"turns"`
	} `json:"transcript"`
}

func backgroundItems(s map[string]json.RawMessage) ([]backgroundItem, error) {
	var snaps []backgroundSnapshot
	if len(s["background_work"]) == 0 {
		return nil, errors.New("hydrate omitted background_work")
	}
	if e := json.Unmarshal(s["background_work"], &snaps); e != nil {
		return nil, e
	}
	items := make([]backgroundItem, 0, len(snaps))
	for _, snap := range snaps {
		items = append(items, snap.Item)
	}
	return items, nil
}
func verifyRawTranscript(item backgroundItem, state map[string]json.RawMessage) error {
	var snaps []backgroundSnapshot
	if e := json.Unmarshal(state["background_work"], &snaps); e != nil {
		return e
	}
	for _, snap := range snaps {
		if snap.Item.Key.ProviderID == item.Key.ProviderID {
			if len(snap.Transcript.Messages) == 0 || len(snap.Transcript.TranscriptBlocks) == 0 {
				return errors.New("child message/raw transcriptBlocks snapshot is empty")
			}
			if !strings.Contains(string(state["background_work"]), `"transcriptBlocks"`) {
				return errors.New("camelCase transcriptBlocks absent")
			}
			return nil
		}
	}
	return errors.New("background snapshot not found")
}

func verifyLifecycle(events []daemon.Event, res *results) error {
	var taskIDs []string
	var childOrder []string
	var trace []string
	var childStatuses []string
	childStarted, childFinished := 0, 0
	for _, ev := range events {
		trace = append(trace, ev.Kind)
		if ev.Kind == "backgroundWork" {
			trace[len(trace)-1] += "=" + string(ev.Payload)
		}
		if ev.Kind == "richActivity" {
			var v map[string]json.RawMessage
			_ = json.Unmarshal(ev.Payload, &v)
			var source, title, kind string
			_ = json.Unmarshal(v["source_id"], &source)
			_ = json.Unmarshal(v["title"], &title)
			_ = json.Unmarshal(v["kind"], &kind)
			if source == "call_task_one" {
				var id string
				_ = json.Unmarshal(v["id"], &id)
				taskIDs = append(taskIDs, id)
				if title != "task" {
					return fmt.Errorf("task called event lost original name: %q", title)
				}
			}
		}
		if ev.Kind == "backgroundWork" {
			var v map[string]json.RawMessage
			if json.Unmarshal(ev.Payload, &v) != nil {
				continue
			}
			var typ string
			_ = json.Unmarshal(v["type"], &typ)
			var key map[string]any
			_ = json.Unmarshal(v["key"], &key)
			if key == nil {
				for _, part := range v {
					var nested map[string]json.RawMessage
					if json.Unmarshal(part, &nested) == nil && len(nested["key"]) > 0 {
						_ = json.Unmarshal(nested["key"], &key)
						break
					}
				}
			}
			if key["providerId"] != childOne {
				continue
			}
			if typ == "upsert" {
				var status string
				_ = json.Unmarshal(v["status"], &status)
				childStatuses = append(childStatuses, status)
			}
			for _, k := range []string{"started", "textDelta", "reasoningDelta", "activity", "finished"} {
				if len(v[k]) > 0 {
					childOrder = append(childOrder, k)
					if k == "started" {
						childStarted++
					}
					if k == "finished" {
						childFinished++
					}
				}
			}
			_ = typ
		}
	}
	if len(taskIDs) < 3 {
		return errors.New("root task started/called richActivity frames missing")
	}
	if len(childOrder) < 5 || childOrder[0] != "started" || childOrder[1] != "textDelta" || childOrder[2] != "reasoningDelta" || childOrder[3] != "activity" || childFinished != 1 || childStarted != 1 {
		return fmt.Errorf("child transcript ordering/terminal mismatch: child=%v eventKinds=%v", childOrder, trace)
	}
	if len(childStatuses) < 3 || childStatuses[0] != "starting" || childStatuses[1] != "running" || childStatuses[len(childStatuses)-1] != "completed" {
		return fmt.Errorf("child was not starting until first execution.started: statuses=%v", childStatuses)
	}
	if strings.Contains(strings.Join(childOrder, ","), "late") {
		return errors.New("late child delta reopened completed transcript")
	}
	if taskIDs[0] == "" || taskIDs[0] != taskIDs[1] || taskIDs[1] != taskIDs[2] {
		return fmt.Errorf("root task started/called/terminal activity identity differs: %v", taskIDs)
	}
	res.add("root-task-identity-and-name", nil)
	res.add("parent-child-isolation-and-unrelated-session", nil)
	res.add("child-interleaved-transcript-and-terminal", nil)
	res.add("late-delta-and-duplicate-start-ignored", nil)
	res.add("child-terminal-does-not-finish-root", nil)
	return nil
}

func hasTaggedReconcile(events []daemon.Event) bool {
	for _, ev := range events {
		if ev.Kind == "backgroundWork" {
			var v map[string]any
			if json.Unmarshal(ev.Payload, &v) == nil && v["type"] == "reconcileLive" {
				return true
			}
		}
	}
	return false
}

func checkControlRejections(ctx context.Context, c *daemon.Client, sid, rid string, res *results) error {
	for _, id := range []string{rootNative, "ses_unknown_child"} {
		key := map[string]any{"kind": "subagent", "providerId": id}
		_, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "stopBackgroundWork", "key": key, "controlId": id})
		if err == nil {
			return fmt.Errorf("stop accepted invalid child control %s", id)
		}
	}
	_, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "compactChildSession", "controlId": rootNative})
	if err == nil {
		return errors.New("compact accepted root native id")
	}
	for _, command := range []map[string]any{{"type": "respond", "requestId": "unknown_permission", "optionId": "once"}, {"type": "respondUserInput", "requestId": "unknown_form", "answers": []any{}}, {"type": "cancelUserInput", "requestId": "unknown_cancel"}} {
		_, scopeErr := c.RequestFor(ctx, sid, rid, command)
		if scopeErr == nil || !strings.Contains(strings.ToLower(scopeErr.Error()), "expired") {
			return fmt.Errorf("unknown request did not fail as expired scope: %s -> %v", command["type"], scopeErr)
		}
	}
	res.add("stop-and-compact-identity-rejection", nil)
	res.add("unknown-request-scope-rejected-not-root-fallback", nil)
	return nil
}

func checkLegacySave(ctx context.Context, c *daemon.Client, sid string, state map[string]json.RawMessage, project map[string]any, res *results) error {
	legacy := map[string]any{"id": sid, "project_id": stringField(state, "project_id"), "title": "legacy checkpoint", "provider": "opencode", "status": stringField(state, "status"), "created_at": 1, "updated_at": uint64(time.Now().Unix()), "detail_loaded": true}
	for _, field := range []string{"messages", "turns", "transcript_blocks", "provider_cursor", "runtime_event_cursor"} {
		if len(state[field]) > 0 {
			var v any
			if json.Unmarshal(state[field], &v) == nil {
				legacy[field] = v
			}
		}
	}
	if _, err := c.Request(ctx, map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{project}, "sessions": []any{legacy}, "skeletonSessionIds": []any{}}); err != nil {
		return fmt.Errorf("legacy checkpoint save: %w", err)
	}
	after, err := hydrate(ctx, c, sid)
	if err != nil {
		return err
	}
	items, err := backgroundItems(after)
	if err != nil {
		return err
	}
	if len(items) == 0 || items[0].Key.ProviderID != childOne {
		return errors.New("ordinary legacy save erased daemon background_work")
	}
	res.add("legacy-save-preserves-background-snapshot", nil)
	return nil
}

func admitTurn(ctx context.Context, c *daemon.Client, sid, pid, workspace string, state map[string]json.RawMessage, turnID, prompt string, count int, project map[string]any) error {
	var messages, turns, blocks []any
	_ = json.Unmarshal(state["messages"], &messages)
	_ = json.Unmarshal(state["turns"], &turns)
	_ = json.Unmarshal(state["transcript_blocks"], &blocks)
	now := uint64(time.Now().Unix())
	record := model.NewSubmissionRecord(sid, turnID, prompt, nil)
	turn := map[string]any{"id": turnID, "status": "running", "turn_count": count, "started_at": now, "submission": record.Receipt}
	user := map[string]any{"id": uuid.NewString(), "turn_id": turnID, "role": "user", "content": prompt, "created_at": now, "attachments": []any{}, "streaming": false}
	messages = append(messages, user)
	turns = append(turns, turn)
	session := map[string]any{"id": sid, "project_id": pid, "title": "subagent fixture", "provider": "opencode", "status": "working", "created_at": now, "updated_at": now, "detail_loaded": true, "provider_cursor": map[string]string{"provider": "openCode", "sessionId": rootNative}, "messages": messages, "turns": turns, "transcript_blocks": blocks}
	_, err := c.RequestFor(ctx, sid, "", map[string]any{"type": "acceptSubmission", "project": project, "session": session, "submission": record})
	if err != nil {
		return fmt.Errorf("admit %s: %w", prompt, err)
	}
	_ = workspace
	return nil
}

func verifyContinuation(events []daemon.Event) error {
	started, finished := 0, 0
	for _, ev := range events {
		if ev.Kind != "backgroundWork" {
			continue
		}
		var v map[string]json.RawMessage
		_ = json.Unmarshal(ev.Payload, &v)
		var key map[string]any
		_ = json.Unmarshal(v["key"], &key)
		if key == nil {
			for _, part := range v {
				var nested map[string]json.RawMessage
				if json.Unmarshal(part, &nested) == nil && len(nested["key"]) > 0 {
					_ = json.Unmarshal(nested["key"], &key)
					break
				}
			}
		}
		if key["providerId"] != childOne {
			continue
		}
		if len(v["started"]) > 0 {
			started++
		}
		if len(v["finished"]) > 0 {
			finished++
		}
	}
	if started != 1 || finished != 1 {
		var trace []string
		for _, ev := range events {
			if ev.Kind == "backgroundWork" {
				trace = append(trace, string(ev.Payload))
			}
		}
		return fmt.Errorf("continuation should emit one child turn start and finish, got %d/%d; events=%v", started, finished, trace)
	}
	return nil
}

func checkScopedReplies(ctx context.Context, c *daemon.Client, sid, rid string, events []daemon.Event, res *results) error {
	seen := map[string]bool{}
	for _, ev := range events {
		if ev.Kind == "permission" || ev.Kind == "userInputRequested" {
			seen[ev.Kind] = true
			if strings.Contains(string(ev.Payload), "providerSecret") {
				return fmt.Errorf("private provider fields leaked in %s payload", ev.Kind)
			}
			if ev.Kind == "userInputRequested" {
				var v map[string]json.RawMessage
				if json.Unmarshal(ev.Payload, &v) != nil {
					return errors.New("invalid child form wire payload")
				}
				var questions []map[string]json.RawMessage
				if json.Unmarshal(v["questions"], &questions) != nil || len(questions) != 1 {
					return errors.New("child form public question list invalid")
				}
				for _, q := range questions {
					for key := range q {
						if key != "id" && key != "header" && key != "question" && key != "options" && key != "multi_select" {
							return fmt.Errorf("non-public form field %q leaked", key)
						}
					}
				}
			}
		}
	}
	if !seen["permission"] || !seen["userInputRequested"] {
		return fmt.Errorf("child permission/form events missing: %+v", seen)
	}
	if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "respond", "requestId": "perm_child_two", "optionId": "once"}); err != nil {
		return fmt.Errorf("respond child permission: %w", err)
	}
	if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "respondUserInput", "requestId": "frm_child_two", "answers": []any{map[string]any{"question_id": "choice", "answers": []string{"yes"}}}}); err != nil {
		return fmt.Errorf("respond child form: %w", err)
	}
	if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "cancelUserInput", "requestId": "frm_cancel_child_two"}); err != nil {
		return fmt.Errorf("cancel child form: %w", err)
	}
	res.add("child-form-permission-events-forwarded", nil)
	res.add("child-form-and-permission-scoped-replies", nil)
	return nil
}

func checkFixtureAudit(path string, res *results) error {
	raw, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("read owned fake CLI audit: %w", err)
	}
	var calls []map[string]string
	if err = json.Unmarshal(raw, &calls); err != nil {
		return err
	}
	wanted := []string{"/api/session/" + childOne + "/compact", "/api/session/" + childTwo + "/permission/perm_child_two/reply", "/api/session/" + childTwo + "/form/frm_child_two/reply", "/api/session/" + childTwo + "/form/frm_cancel_child_two/cancel", "/api/session/" + childTwo + "/interrupt"}
	for _, path := range wanted {
		found := false
		for _, call := range calls {
			if strings.Contains(call["path"], path) {
				found = true
				break
			}
		}
		if !found {
			return fmt.Errorf("fake CLI audit lacks scoped endpoint %s", path)
		}
	}
	for _, call := range calls {
		if strings.Contains(call["path"], "/api/shell/") {
			return errors.New("subagent stop used shell endpoint")
		}
	}
	pageSeen := false
	fallbackSeen := false
	for _, call := range calls {
		p := call["path"]
		if strings.HasPrefix(p, "/api/session?") && strings.Contains(p, "cursor=page-2") {
			pageSeen = true
		}
		if strings.HasPrefix(p, "/api/session?") && !strings.Contains(p, "parentID=") {
			fallbackSeen = true
		}
	}
	if !pageSeen || !fallbackSeen {
		return fmt.Errorf("roster paging/400 fallback not observed (page=%v fallback=%v)", pageSeen, fallbackSeen)
	}
	for _, target := range []struct{ child, suffix string }{
		{child: childUnknownEmpty, suffix: "empty"},
		{child: childUnknownOld, suffix: "old"},
	} {
		permissionID := "perm_early_" + target.suffix
		formID := "frm_early_" + target.suffix
		cancelID := "frm_early_cancel_" + target.suffix
		scoped := map[string]string{
			"permission reply": "/api/session/" + target.child + "/permission/" + permissionID + "/reply",
			"form reply":       "/api/session/" + target.child + "/form/" + formID + "/reply",
			"form cancel":      "/api/session/" + target.child + "/form/" + cancelID + "/cancel",
		}
		for label, expectedPath := range scoped {
			if countAuditPath(calls, "POST", expectedPath) != 1 {
				return fmt.Errorf("early %s for %s did not issue exactly one child-scoped POST %s", label, target.child, expectedPath)
			}
		}
		rootPaths := []string{
			"/api/session/" + rootNative + "/permission/" + permissionID + "/reply",
			"/api/session/" + rootNative + "/form/" + formID + "/reply",
			"/api/session/" + rootNative + "/form/" + cancelID + "/cancel",
		}
		for _, rootPath := range rootPaths {
			if countAuditPath(calls, "POST", rootPath) != 0 {
				return fmt.Errorf("early child request incorrectly fell back to root POST %s", rootPath)
			}
		}
		res.add("early-request-http-scope-"+target.suffix, nil)
	}
	res.add("native-child-control-and-request-scope-audit", nil)
	return nil
}

func countAuditPath(calls []map[string]string, method, path string) int {
	count := 0
	for _, call := range calls {
		if call["method"] == method && call["path"] == path {
			count++
		}
	}
	return count
}

func runSlowRosterAndPartialResults(ctx context.Context, c *daemon.Client, sid, rid, pid, workspace string, state map[string]json.RawMessage, project map[string]any, root string, res *results) error {
	if err := setFixtureMode(root, "roster-slow-one"); err != nil {
		return err
	}
	if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "refreshBackgroundWork"}); err != nil {
		return fmt.Errorf("start held roster generation 1: %w", err)
	}
	if err := waitFixtureFile(ctx, root, "roster-one-held"); err != nil {
		return err
	}
	turnID := uuid.NewString()
	if err := admitTurn(ctx, c, sid, pid, workspace, state, turnID, "slow roster race", 6, project); err != nil {
		return err
	}
	if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return err
	}
	streamEvents, err := collectUntil(ctx, c, "turnFinished", sid, rid)
	if err != nil {
		return err
	}
	if !eventPayloadContains(streamEvents, "ROOT_SSE_UNBLOCKED") || !eventPayloadContains(streamEvents, "RACE_CHILD_SURVIVES") {
		return errors.New("root SSE or child-created/delta was blocked by held roster HTTP")
	}
	res.add("slow-roster-does-not-block-root-sse-or-new-child", nil)

	// Start generation 2 while generation 1 remains held. Generation 2 is a
	// complete roster containing the new child; only then release generation 1.
	if err := setFixtureMode(root, "roster-second"); err != nil {
		return err
	}
	if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "refreshBackgroundWork"}); err != nil {
		return fmt.Errorf("start roster generation 2: %w", err)
	}
	second, err := collectBackground(ctx, c, sid, rid, "reconcileLive")
	if err != nil {
		return err
	}
	if !hasTaggedReconcile(second) || !eventPayloadContains(second, childRace) {
		return errors.New("generation 2 did not reconcile the newly created child")
	}
	payloadItems, err := reconcilePayloadItems(second)
	if err != nil {
		return err
	}
	if err := assertItemsPresentAndNotLost(payloadItems, childOne, childRace); err != nil {
		return fmt.Errorf("generation 2 reconcileLive payload itself marked children lost: %w", err)
	}
	for _, item := range payloadItems {
		if item.Key.ProviderID == childOne && item.Status != "completed" {
			return fmt.Errorf("generation 2 reconcileLive payload mapped child one to %q, want completed", item.Status)
		}
	}
	if diagState, diagErr := hydrate(ctx, c, sid); diagErr == nil {
		diagHydrate("gen2-reconcile", diagState)
	}
	// Prove the generation 2 roster is persisted before releasing the held
	// generation 1; the reconcileLive event alone precedes the flush commit.
	// The gen2-specific title marker rules out an older SSE snapshot satisfying
	// the barrier: SSE created childRace as "Race child", only the gen2 roster
	// renames it to "Race child gen2".
	gen2Persisted := func(items []backgroundItem) error {
		if err := assertItemsPresentAndNotLost(items, childOne, childRace); err != nil {
			return err
		}
		for _, item := range items {
			switch item.Key.ProviderID {
			case childRace:
				if item.Title != "Race child gen2" {
					return fmt.Errorf("generation 2 marker absent: child race title %q", item.Title)
				}
				if item.Status != "completed" {
					return fmt.Errorf("generation 2 child race status %q, want completed", item.Status)
				}
			case childOne:
				if item.Status != "completed" {
					return fmt.Errorf("generation 2 child one status %q, want completed", item.Status)
				}
			}
		}
		return nil
	}
	if _, err := waitPersistedBackground(ctx, c, sid, gen2Persisted); err != nil {
		return fmt.Errorf("generation 2 roster was not persisted before releasing generation 1: %w", err)
	}
	if err := releaseFixture(root, "release-roster-one"); err != nil {
		return err
	}
	if err := waitFixtureFile(ctx, root, "roster-one-finished"); err != nil {
		return err
	}
	// The finished signal precedes the HTTP write; wait for the response
	// completion barrier so the old handler is provably done.
	if err := waitFixtureFile(ctx, root, "roster-one-response-written"); err != nil {
		return err
	}
	// The response-written barrier marks server completion only; it does not
	// claim the actor processed the stale result (that discard stays uncovered).
	// Generation 2 is already proven persisted, so no fixed sleep is needed.
	state, err = hydrate(ctx, c, sid)
	if err != nil {
		return err
	}
	diagHydrate("gen1-released", state)
	if err := assertBackgroundPresentAndNotLost(state, childOne, childRace); err != nil {
		return fmt.Errorf("stale generation overwrote current roster: %w", err)
	}
	res.add("cancelled-older-refresh-cannot-overwrite-newer-roster", nil)

	if err := runRevisionRosterRace(ctx, c, sid, rid, pid, workspace, state, project, root, res); err != nil {
		return err
	}
	state, err = hydrate(ctx, c, sid)
	if err != nil {
		return err
	}

	for _, mode := range []string{"cursor-cycle", "fullpage-no-cursor"} {
		if err := setFixtureMode(root, mode); err != nil {
			return err
		}
		if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "refreshBackgroundWork"}); err != nil {
			return fmt.Errorf("start %s validation: %w", mode, err)
		}
		if err := waitNotice(ctx, c, "error", sid, rid); err != nil {
			return fmt.Errorf("%s roster was not rejected as incomplete: %w", mode, err)
		}
		state, err = hydrate(ctx, c, sid)
		if err != nil {
			return err
		}
		if err := assertBackgroundPresentAndNotLost(state, childOne, childRace); err != nil {
			return fmt.Errorf("%s roster replaced authoritative state: %w", mode, err)
		}
		res.add("incomplete-roster-preserves-prior-state-"+mode, nil)
	}
	return setFixtureMode(root, "default")
}

func waitFixtureFile(ctx context.Context, root, name string) error {
	path := filepath.Join(root, "home", name)
	timer := time.NewTimer(20 * time.Second)
	defer timer.Stop()
	ticker := time.NewTicker(10 * time.Millisecond)
	defer ticker.Stop()
	for {
		if _, err := os.Stat(path); err == nil {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-timer.C:
			return fmt.Errorf("fixture signal %s timed out", name)
		case <-ticker.C:
		}
	}
}

func runRevisionRosterRace(ctx context.Context, c *daemon.Client, sid, rid, pid, workspace string, state map[string]json.RawMessage, project map[string]any, root string, res *results) error {
	if err := setFixtureMode(root, "roster-revision-held"); err != nil {
		return err
	}
	if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "refreshBackgroundWork"}); err != nil {
		return fmt.Errorf("start revision-guard roster: %w", err)
	}
	if err := waitFixtureFile(ctx, root, "revision-roster-held"); err != nil {
		return err
	}
	turnID := uuid.NewString()
	if err := admitTurn(ctx, c, sid, pid, workspace, state, turnID, "revision roster race", 7, project); err != nil {
		return err
	}
	if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return err
	}
	events, err := collectUntil(ctx, c, "turnFinished", sid, rid)
	if err != nil {
		return err
	}
	if !eventPayloadContains(events, "ROOT_SSE_DURING_REVISION_ROSTER") || !eventPayloadContains(events, "REVISION_CHILD_SURVIVES") {
		return errors.New("revision-race fixture did not process root SSE and child creation while roster was held")
	}
	if err := releaseFixture(root, "release-revision-roster"); err != nil {
		return err
	}
	if err := waitFixtureFile(ctx, root, "revision-roster-finished"); err != nil {
		return err
	}
	if err := waitFixtureFile(ctx, root, "revision-roster-response-written"); err != nil {
		return err
	}
	// This request is not superseded: it completes successfully with a roster
	// captured before the child-creation events advanced backgroundRevision.
	// The response-written barrier marks server completion only; the actor's
	// discard of the stale-revision result stays uncovered. No fixed sleep.
	state, err = hydrate(ctx, c, sid)
	if err != nil {
		return err
	}
	if err := assertBackgroundPresentAndNotLost(state, childRevision); err != nil {
		return fmt.Errorf("successful stale-revision roster overwrote SSE child: %w", err)
	}
	revisionItems, err := backgroundItems(state)
	if err != nil {
		return err
	}
	for _, item := range revisionItems {
		if item.Key.ProviderID == childRevision && item.Status != "completed" {
			return fmt.Errorf("this-round revision SSE child status %q, want completed", item.Status)
		}
	}
	res.add("revision-race-final-hydrated-child-remains-completed", nil)
	return setFixtureMode(root, "default")
}

func eventPayloadContains(events []daemon.Event, needle string) bool {
	for _, event := range events {
		if strings.Contains(string(event.Payload), needle) {
			return true
		}
	}
	return false
}

func assertBackgroundPresentAndNotLost(state map[string]json.RawMessage, ids ...string) error {
	items, err := backgroundItems(state)
	if err != nil {
		return err
	}
	return assertItemsPresentAndNotLost(items, ids...)
}

func assertItemsPresentAndNotLost(items []backgroundItem, ids ...string) error {
	for _, id := range ids {
		found := false
		for _, item := range items {
			if item.Key.ProviderID == id {
				found = true
				if item.Status == "lost" {
					return fmt.Errorf("%s marked lost", id)
				}
			}
		}
		if !found {
			return fmt.Errorf("%s missing from authoritative snapshot", id)
		}
	}
	return nil
}

// reconcilePayloadItems extracts the items of the first reconcileLive payload
// in the collected events, so the emitted roster can be checked independently
// of any later hydrate read window.
func reconcilePayloadItems(events []daemon.Event) ([]backgroundItem, error) {
	for _, event := range events {
		if event.Kind != "backgroundWork" {
			continue
		}
		var payload struct {
			Type  string           `json:"type"`
			Items []backgroundItem `json:"items"`
		}
		if err := json.Unmarshal(event.Payload, &payload); err != nil {
			return nil, err
		}
		if payload.Type == "reconcileLive" {
			return payload.Items, nil
		}
	}
	return nil, errors.New("no reconcileLive payload in collected events")
}

// waitPersistedBackground polls hydrate until the authoritative store satisfies
// ready. reconcileLive is emitted before the flush commits, so an immediate
// hydrate can still read the previous state; this deadline wait is the
// observable persisted barrier, not a larger sleep. ready must demand a
// generation-specific marker so an older SSE snapshot cannot satisfy it.
func waitPersistedBackground(ctx context.Context, c *daemon.Client, sid string, ready func([]backgroundItem) error) (map[string]json.RawMessage, error) {
	timer := time.NewTimer(20 * time.Second)
	defer timer.Stop()
	ticker := time.NewTicker(10 * time.Millisecond)
	defer ticker.Stop()
	var lastErr error
	for {
		state, err := hydrate(ctx, c, sid)
		if err != nil {
			lastErr = err
		} else {
			items, itemsErr := backgroundItems(state)
			if itemsErr != nil {
				lastErr = itemsErr
			} else if lastErr = ready(items); lastErr == nil {
				return state, nil
			}
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-timer.C:
			return nil, fmt.Errorf("roster not persisted within deadline: %w", lastErr)
		case <-ticker.C:
		}
	}
}

func runCapacityBoundary(ctx context.Context, c *daemon.Client, sid, rid, pid, workspace string, state map[string]json.RawMessage, project map[string]any, res *results) error {
	turnID := uuid.NewString()
	if err := admitTurn(ctx, c, sid, pid, workspace, state, turnID, "capacity boundary", 7, project); err != nil {
		return err
	}
	if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return err
	}
	events, err := collectUntil(ctx, c, "turnFinished", sid, rid)
	if err != nil {
		return err
	}
	warned, rootContinued := false, eventPayloadContains(events, "ROOT_CONTINUES_AFTER_TRUNCATION")
	for _, event := range events {
		if event.Kind == "error" && strings.Contains(string(event.Payload), "Subagent prompt was truncated") {
			warned = true
		}
	}
	if !warned {
		return errors.New("over-64-KiB task prompt was not truncated with a warning")
	}
	if !rootContinued {
		return errors.New("root turn did not continue after child output truncation")
	}
	state, err = hydrate(ctx, c, sid)
	if err != nil {
		return err
	}
	items, err := backgroundItems(state)
	if err != nil {
		return err
	}
	for _, item := range items {
		if item.Key.ProviderID == childOne && item.OutputTruncated {
			res.add("over-64k-prompt-warning-and-child-output-truncation", nil)
			res.add("root-continues-after-child-truncation", nil)
			return nil
		}
	}
	return errors.New("over-64-KiB raw child event did not mark outputTruncated")
}

func setFixtureMode(root, mode string) error {
	home := filepath.Join(root, "home")
	if err := os.MkdirAll(home, 0700); err != nil {
		return err
	}
	tmp := filepath.Join(home, "fixture-mode.tmp")
	if err := os.WriteFile(tmp, []byte(mode), 0600); err != nil {
		return err
	}
	return os.Rename(tmp, filepath.Join(home, "fixture-mode"))
}
func releaseFixture(root, name string) error {
	return os.WriteFile(filepath.Join(root, "home", name), []byte("release"), 0600)
}

func runUnknownTarget(ctx context.Context, c *daemon.Client, sid, rid, pid, workspace string, state map[string]json.RawMessage, project map[string]any, root string, turnCount int, childID, prompt, mode string, res *results) error {
	if err := setFixtureMode(root, mode); err != nil {
		return err
	}
	turnID := uuid.NewString()
	if err := admitTurn(ctx, c, sid, pid, workspace, state, turnID, prompt, turnCount, project); err != nil {
		return err
	}
	if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return err
	}
	early, err := waitUnknownReady(ctx, c, sid, rid, childID)
	if err != nil {
		return err
	}
	permissionID, formID := "perm_early_empty", "frm_early_empty"
	if childID == childUnknownOld {
		permissionID, formID = "perm_early_old", "frm_early_old"
	}
	if _, err = c.RequestFor(ctx, sid, rid, map[string]any{"type": "respond", "requestId": permissionID, "optionId": "once"}); err != nil {
		return fmt.Errorf("early child permission scope for %s: %w", childID, err)
	}
	if _, err = c.RequestFor(ctx, sid, rid, map[string]any{"type": "respondUserInput", "requestId": formID, "answers": []any{map[string]any{"question_id": "choice", "answers": []string{"yes"}}}}); err != nil {
		return fmt.Errorf("early child form scope for %s: %w", childID, err)
	}
	cancelFormID := strings.Replace(formID, "frm_early_", "frm_early_cancel_", 1)
	if _, err = c.RequestFor(ctx, sid, rid, map[string]any{"type": "cancelUserInput", "requestId": cancelFormID}); err != nil {
		return fmt.Errorf("early child form cancellation scope for %s: %w", childID, err)
	}
	releaseName := "release-unknown-empty"
	deleted := childID == childUnknownEmpty
	if !deleted {
		releaseName = "release-unknown-old"
	}
	if err := releaseFixture(root, releaseName); err != nil {
		return err
	}
	settled, err := collectUntil(ctx, c, "turnFinished", sid, rid)
	if err != nil {
		return err
	}
	if deleted {
		found := false
		for _, ev := range settled {
			if ev.Kind == "permission" && strings.Contains(string(ev.Payload), "perm_delete_pending") {
				found = true
			}
		}
		if !found {
			return errors.New("session.deleted fixture omitted outstanding child permission")
		}
		if _, err := c.RequestFor(ctx, sid, rid, map[string]any{"type": "respond", "requestId": "perm_delete_pending", "optionId": "once"}); err == nil || !strings.Contains(strings.ToLower(err.Error()), "expired") {
			return fmt.Errorf("deleted child request did not expire: %v", err)
		}
		res.add("session-deleted-stops-finishes-and-expires-request", nil)
	} else {
		res.add("unknown-history-target-old-transcript-new-turn", nil)
	}
	if !sawUnknownDelta(early, childID) {
		return fmt.Errorf("queued child delta not replayed for %s", childID)
	}
	res.add("unknown-history-target-roster-activation-and-queued-delta", nil)
	return nil
}

func waitUnknownReady(ctx context.Context, c *daemon.Client, sid, rid, childID string) ([]daemon.Event, error) {
	timer := time.NewTimer(25 * time.Second)
	defer timer.Stop()
	var seen []daemon.Event
	running, delta, permission, form, cancelForm := false, false, false, false, false
	for !(running && delta && permission && form && cancelForm) {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-timer.C:
			return nil, fmt.Errorf("unknown target %s did not activate: running/delta/permission/form=%v/%v/%v/%v", childID, running, delta, permission, form)
		case ev, ok := <-c.Events():
			if !ok {
				return nil, errors.New("daemon event stream closed waiting for roster activation")
			}
			diagEvent(ev)
			if ev.Type == "event" {
				c.Ack(ev)
			}
			if ev.SessionID == sid && (ev.RuntimeID == rid || ev.RuntimeID == "") {
				seen = append(seen, ev)
			}
			if ev.Kind == "backgroundWork" {
				var p map[string]json.RawMessage
				_ = json.Unmarshal(ev.Payload, &p)
				key := nestedBackgroundKey(p)
				if key == childID {
					var typ, status string
					_ = json.Unmarshal(p["type"], &typ)
					_ = json.Unmarshal(p["status"], &status)
					if typ == "upsert" && status == "running" {
						running = true
					}
					if raw := p["textDelta"]; len(raw) > 0 && strings.Contains(string(raw), childID) {
						delta = true
					}
				}
			}
			if ev.Kind == "permission" && (strings.Contains(string(ev.Payload), "perm_early_empty") || strings.Contains(string(ev.Payload), "perm_early_old")) {
				if strings.Contains(string(ev.Payload), "private") {
					return nil, errors.New("early child permission leaked provider field")
				}
				permission = true
			}
			if ev.Kind == "userInputRequested" && (strings.Contains(string(ev.Payload), "frm_early_empty") || strings.Contains(string(ev.Payload), "frm_early_old")) {
				if strings.Contains(string(ev.Payload), "private") {
					return nil, errors.New("early child form leaked provider field")
				}
				form = true
			}
			if ev.Kind == "userInputRequested" && strings.Contains(string(ev.Payload), "frm_early_cancel_") {
				cancelForm = true
			}
		}
	}
	return seen, nil
}

func nestedBackgroundKey(p map[string]json.RawMessage) string {
	var key map[string]json.RawMessage
	_ = json.Unmarshal(p["key"], &key)
	if key == nil {
		for _, raw := range p {
			var n map[string]json.RawMessage
			if json.Unmarshal(raw, &n) == nil && len(n["key"]) > 0 {
				_ = json.Unmarshal(n["key"], &key)
				break
			}
		}
	}
	var id string
	if key != nil {
		_ = json.Unmarshal(key["providerId"], &id)
	}
	return id
}
func sawUnknownDelta(events []daemon.Event, id string) bool {
	for _, ev := range events {
		if ev.Kind != "backgroundWork" {
			continue
		}
		var p map[string]json.RawMessage
		_ = json.Unmarshal(ev.Payload, &p)
		if nestedBackgroundKey(p) == id && len(p["textDelta"]) > 0 {
			return true
		}
	}
	return false
}

func hasBackgroundID(items []backgroundItem, id string) bool {
	for _, item := range items {
		if item.Key.ProviderID == id {
			return true
		}
	}
	return false
}
func findBackground(items []backgroundItem, id string) backgroundItem {
	for _, item := range items {
		if item.Key.ProviderID == id {
			return item
		}
	}
	return backgroundItem{}
}
func snapshotsFromState(state map[string]json.RawMessage) ([]backgroundSnapshot, error) {
	var snaps []backgroundSnapshot
	if err := json.Unmarshal(state["background_work"], &snaps); err != nil {
		return nil, err
	}
	return snaps, nil
}
func verifyUnknownSnapshots(state map[string]json.RawMessage, res *results) error {
	snaps, err := snapshotsFromState(state)
	if err != nil {
		return err
	}
	byID := map[string]backgroundSnapshot{}
	for _, snap := range snaps {
		byID[snap.Item.Key.ProviderID] = snap
	}
	empty, ok := byID[childUnknownEmpty]
	if !ok || empty.Item.Status != "stopped" || len(empty.Transcript.Turns) != 1 {
		return fmt.Errorf("empty historical child did not start one stopped turn: %+v", empty)
	}
	var emptyMessages []struct {
		Role    string  `json:"role"`
		Content string  `json:"content"`
		TurnID  *string `json:"turn_id"`
	}
	for _, raw := range empty.Transcript.Messages {
		var m struct {
			Role    string  `json:"role"`
			Content string  `json:"content"`
			TurnID  *string `json:"turn_id"`
		}
		_ = json.Unmarshal(raw, &m)
		emptyMessages = append(emptyMessages, m)
	}
	if !containsMessage(emptyMessages, "EMPTY_CHILD_DELTA") || containsMessage(emptyMessages, "OLD_TURN_SENTINEL") {
		return errors.New("empty historical child delta was lost or contaminated")
	}
	old, ok := byID[childUnknownOld]
	if !ok || old.Item.Status != "completed" || len(old.Transcript.Turns) != 2 {
		return fmt.Errorf("old historical child did not start a fresh second turn: %+v", old)
	}
	var oldMessages []struct {
		Role    string  `json:"role"`
		Content string  `json:"content"`
		TurnID  *string `json:"turn_id"`
	}
	for _, raw := range old.Transcript.Messages {
		var m struct {
			Role    string  `json:"role"`
			Content string  `json:"content"`
			TurnID  *string `json:"turn_id"`
		}
		_ = json.Unmarshal(raw, &m)
		oldMessages = append(oldMessages, m)
	}
	oldTurn, newTurn := "", ""
	for _, m := range oldMessages {
		if strings.Contains(m.Content, "OLD_TURN_SENTINEL") && m.TurnID != nil {
			oldTurn = *m.TurnID
		}
		if strings.Contains(m.Content, "NEW_CHILD_DELTA") && m.TurnID != nil {
			newTurn = *m.TurnID
		}
	}
	if oldTurn == "" || newTurn == "" || oldTurn == newTurn || containsMessage(oldMessages, "OLD_TURN_SENTINELNEW_CHILD_DELTA") {
		return fmt.Errorf("old and new child turn text was spliced: old=%q new=%q messages=%+v", oldTurn, newTurn, oldMessages)
	}
	res.add("empty-transcript-replay-starts-fresh-turn", nil)
	res.add("old-transcript-replay-does-not-splice-turns", nil)
	return nil
}
func containsMessage(messages []struct {
	Role    string  `json:"role"`
	Content string  `json:"content"`
	TurnID  *string `json:"turn_id"`
}, needle string) bool {
	for _, m := range messages {
		if strings.Contains(m.Content, needle) {
			return true
		}
	}
	return false
}

func runHistoryEdits(ctx context.Context, c *daemon.Client, sid string, state map[string]json.RawMessage, project map[string]any, root string, res *results) error {
	if err := setFixtureMode(root, "complete-empty"); err != nil {
		return err
	}
	raw, err := c.RequestFor(ctx, sid, "", map[string]any{"type": "forkSessionFromResponse", "turnCount": 1})
	if err != nil {
		return fmt.Errorf("safe provider fork with completed empty roster: %w", err)
	}
	var forkReply struct {
		Session map[string]json.RawMessage `json:"session"`
	}
	if err = json.Unmarshal(raw, &forkReply); err != nil {
		return err
	}
	var forkID string
	_ = json.Unmarshal(forkReply.Session["id"], &forkID)
	var forkWork []json.RawMessage
	_ = json.Unmarshal(forkReply.Session["background_work"], &forkWork)
	if forkID == "" || len(forkWork) != 0 {
		return fmt.Errorf("fork copied background controls: id=%q work=%s", forkID, forkReply.Session["background_work"])
	}
	res.add("fork-clears-background-controls-with-complete-roster", nil)
	if _, err = c.RequestFor(ctx, sid, "", map[string]any{"type": "rewindSessionToMessage", "turnCount": 1}); err != nil {
		return fmt.Errorf("safe provider rewind with complete empty roster: %w", err)
	}
	after, err := hydrate(ctx, c, sid)
	if err != nil {
		return err
	}
	var remaining []backgroundSnapshot
	if err = json.Unmarshal(after["background_work"], &remaining); err != nil {
		return err
	}
	var turns []json.RawMessage
	_ = json.Unmarshal(after["turns"], &turns)
	if len(turns) != 0 || len(remaining) != 0 {
		return fmt.Errorf("rewind retained removed-turn origins/background: turns=%d background=%+v", len(turns), remaining)
	}
	res.add("rewind-trims-removed-origin-activities", nil)
	if err := verifyTaskEditAudit(filepath.Join(root, "home", "subagent-audit.json")); err != nil {
		return err
	}
	_ = state
	_ = project
	res.add("task-edit-uses-complete-provider-roster-before-mutation", nil)
	return nil
}

func verifyTaskEditAudit(path string) error {
	raw, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var calls []map[string]string
	if err = json.Unmarshal(raw, &calls); err != nil {
		return err
	}
	fork, roster, active, revert := false, false, false, false
	for _, call := range calls {
		p := call["path"]
		if strings.HasSuffix(p, "/fork") {
			fork = true
		}
		if strings.HasPrefix(p, "/api/session?") {
			roster = true
		}
		if p == "/api/session/active" {
			active = true
		}
		if strings.Contains(p, "/revert") {
			revert = true
		}
	}
	if !fork || !roster || !active || !revert {
		return fmt.Errorf("task-edit fake audit missed complete roster/fork/revert: fork=%v roster=%v active=%v revert=%v", fork, roster, active, revert)
	}
	return nil
}

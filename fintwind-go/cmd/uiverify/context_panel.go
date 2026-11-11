package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"
)

const contextModelID = "fixture/context-model"

func checkContextPanel(ctx context.Context, client *autoClient, control fixtureControl, windowID int, sessionA, sessionB, dataDir string, cases *[]caseResult, roundDir string) {
	const scope = "actualUI+fixtureDaemon"
	push := func(name, status, detail string) {
		*cases = append(*cases, caseResult{Name: name, Scope: scope, Status: status, Detail: detail})
	}
	fail := func(name string, err error, nodes []autoNode, stage string) {
		push(name, "fail", err.Error()+"; labels: "+visibleLabels(nodes))
		lastTreeJSON = desensitizedTree(windowID, nodes)
		saveStageTree(roundDir, stage)
	}

	if err := selectSessionWait(ctx, client, windowID, sessionA, knownTitleA, seededBodyA); err != nil {
		fail("context panel unknown usage and model window fallback", err, nil, "context-a-select")
		return
	}
	nodes, err := ensureContextPanel(ctx, client, windowID)
	if err != nil {
		fail("context panel unknown usage and model window fallback", err, nodes, "context-a-open")
		return
	}
	if !anyLabel(nodes, "Unknown") || !anyLabel(nodes, "100K tokens") {
		fail("context panel unknown usage and model window fallback",
			fmt.Errorf("unknown usage and the model's 100K default were not both visible"), nodes, "context-a-fallback")
		return
	}
	if err := waitFixtureContextWindow(ctx, control, sessionA, "100k"); err != nil {
		fail("context panel unknown usage and model window fallback", err, nodes, "context-a-fallback-persisted")
		return
	}
	push("context panel unknown usage and model window fallback", "pass", "unmeasured usage is Unknown; the catalog default supplies 100K")

	selectNode := findContextWindowSelect(nodes, sessionA)
	if selectNode == nil {
		fail("context window selection persists for the model", fmt.Errorf("context window select not in automation tree"), nodes, "context-window-select")
		return
	}
	if err := client.act(ctx, windowID, selectNode.ID, "", "press", ""); err != nil {
		fail("context window selection persists for the model", fmt.Errorf("open window select: %w", err), nodes, "context-window-select")
		return
	}
	if err := client.inputKey(ctx, windowID, "ArrowUp"); err != nil {
		fail("context window selection persists for the model", fmt.Errorf("move to 80K: %w", err), nodes, "context-window-select")
		return
	}
	if err := client.inputKey(ctx, windowID, "Enter"); err != nil {
		fail("context window selection persists for the model", fmt.Errorf("confirm 80K: %w", err), nodes, "context-window-select")
		return
	}
	nodes, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if !anyLabel(nodes, "80K tokens") {
			return fmt.Errorf("80K selection did not settle")
		}
		return nil
	})
	if err != nil {
		fail("context window selection persists for the model", err, nodes, "context-window-selected")
		return
	}
	if err := waitContextPreference(ctx, filepath.Join(dataDir, "app-settings.json"), "80k"); err != nil {
		fail("context window selection persists for the model", err, nodes, "context-window-persist")
		return
	}
	if err := waitFixtureContextWindow(ctx, control, sessionA, "80k"); err != nil {
		fail("context window selection persists for the model", err, nodes, "context-window-session-persist")
		return
	}
	push("context window selection persists for the model", "pass", "keyboard selection is saved under the model id in app-settings.json")

	if err := selectSessionWait(ctx, client, windowID, sessionB, knownTitleB, seededBodyB); err != nil {
		fail("context window preference follows session", err, nodes, "context-b-select")
		return
	}
	nodes, err = ensureContextPanel(ctx, client, windowID)
	if err != nil {
		fail("context window preference follows session", err, nodes, "context-b-open")
		return
	}
	if !anyLabel(nodes, "100K tokens") {
		fail("new draft restores context window preference", fmt.Errorf("existing session did not retain its own 100K window"), nodes, "context-b-window")
		return
	}
	if !anyLabel(nodes, "Compaction unavailable") {
		fail("compaction unavailable without a runtime", fmt.Errorf("no-runtime session did not report compaction unavailable"), nodes, "context-b-unavailable")
		return
	}
	push("compaction unavailable without a runtime", "pass", "session B has no runtime and the UI exposes the unavailable state")
	newTask := findSmallButton(nodes, "New task")
	if newTask == nil {
		fail("new draft restores context window preference", fmt.Errorf("New task action not available"), nodes, "context-new-draft")
		return
	}
	if err := client.act(ctx, windowID, newTask.ID, "", "press", ""); err != nil {
		fail("new draft restores context window preference", fmt.Errorf("press New task: %w", err), nodes, "context-new-draft")
		return
	}
	nodes, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findTextField(nodes, "Message") == nil || findNodeKey(nodes, "context-window-") == nil || !anyLabel(nodes, "80K tokens") {
			return fmt.Errorf("new draft has not restored the model's 80K preference")
		}
		return nil
	})
	if err != nil {
		fail("new draft restores context window preference", err, nodes, "context-new-draft")
		return
	}
	push("new draft restores context window preference", "pass", "new task restores lastModel and the remembered 80K window")

	if err := selectSessionWait(ctx, client, windowID, sessionA, knownTitleA, seededBodyA); err != nil {
		fail("context usage marks 80 percent near full", err, nodes, "context-a-reselect")
		return
	}
	nodes, err = ensureContextPanel(ctx, client, windowID)
	if err != nil {
		fail("context usage marks 80 percent near full", err, nodes, "context-a-reopen")
		return
	}
	events, err := queryJournal(ctx, control, sessionA)
	if err != nil {
		fail("context usage marks 80 percent near full", fmt.Errorf("journal runtime: %w", err), nodes, "context-runtime")
		return
	}
	runtimeID := ""
	for _, event := range events {
		if event.RuntimeID != "" {
			runtimeID = event.RuntimeID
		}
	}
	if runtimeID == "" {
		fail("context usage marks 80 percent near full", fmt.Errorf("session A has no fixture runtime"), nodes, "context-runtime")
		return
	}
	if err := postFixtureEvent(ctx, control, sessionA, runtimeID, "usageUpdated", map[string]any{
		"contextTokens": 64000, "contextWindow": 80000, "contextWindowResolved": true,
	}); err != nil {
		fail("context usage marks 80 percent near full", err, nodes, "context-usage-80")
		return
	}
	nodes, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if !anyLabel(nodes, "Near full") || !anyValue(nodes, "80.0%") {
			return fmt.Errorf("80 percent usage status not visible")
		}
		return nil
	})
	if err != nil {
		fail("context usage marks 80 percent near full", err, nodes, "context-usage-80")
		return
	}
	push("context usage marks 80 percent near full", "pass", "usageUpdated event produces the near-full threshold at 80%")

	if err := postFixtureEvent(ctx, control, sessionA, runtimeID, "usageUpdated", map[string]any{
		"contextTokens": 76000, "contextWindow": 80000, "contextWindowResolved": true,
		"sessionTotal": 123456, "promptTokens": 100000, "cacheRead": 50000,
	}); err != nil {
		fail("context usage marks 95 percent full", err, nodes, "context-usage-95")
		return
	}
	nodes, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if !anyLabel(nodes, "Full") || !anyValue(nodes, "95.0%") {
			return fmt.Errorf("95 percent usage status not visible")
		}
		return nil
	})
	if err != nil {
		fail("context usage marks 95 percent full", err, nodes, "context-usage-95")
		return
	}
	push("context usage marks 95 percent full", "pass", "usageUpdated event produces the full threshold at 95%")

	if err := postFixtureEvent(ctx, control, sessionA, runtimeID, "contextCompositionUpdated", map[string]any{
		"version": 2, "model": contextModelID, "system": 100, "tools": 200, "memory": 300,
		"messages": 400, "reasoning": 500, "toolCalls": 600, "files": 700, "toolResults": 800,
		"opaqueReasoning": true, "opaqueMemory": true, "incomplete": true,
	}); err != nil {
		fail("context composition and usage KPIs render from events", err, nodes, "context-composition")
		return
	}
	nodes, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if !anyLabel(nodes, "Estimated tokens") || !anyLabel(nodes, "Partial estimate") ||
			!anyLabel(nodes, "Total tokens") || !anyLabel(nodes, "Cache hit rate") || !anyLabel(nodes, "First token latency") {
			return fmt.Errorf("composition status or usage KPI labels not visible")
		}
		if !anyValue(nodes, "123K") || !anyValue(nodes, "50.0%") {
			return fmt.Errorf("total token or cache hit KPI values are missing")
		}
		if findNodeKey(nodes, "context-composition-Reasoning") == nil || findNodeKey(nodes, "context-composition-Summary") == nil {
			return fmt.Errorf("opaque reasoning or summary component missing")
		}
		return nil
	})
	if err != nil {
		fail("context composition and usage KPIs render from events", err, nodes, "context-composition")
		return
	}
	push("context composition and usage KPIs render from events", "pass", "version 2 partial/opaque composition and token, cache, latency KPIs render")

	push("context copy exposes the native session id", "blocked", "BLOCKED: validating clipboard content would replace user clipboard formats")

	before, err := fixtureCompactCount(ctx, control)
	if err != nil {
		fail("compaction acknowledgement stays pending", err, nodes, "context-compact-before")
		return
	}
	compact := findNodeKey(nodes, "context-compact")
	if compact == nil {
		fail("compaction acknowledgement stays pending", fmt.Errorf("compact control not visible"), nodes, "context-compact-control")
		return
	}
	if err := client.act(ctx, windowID, compact.ID, "", "press", ""); err != nil {
		fail("compaction acknowledgement stays pending", fmt.Errorf("press compact: %w", err), nodes, "context-compact-control")
		return
	}
	if err := waitCompactCount(ctx, control, before+1); err != nil {
		fail("compaction acknowledgement stays pending", err, nodes, "context-compact-ack")
		return
	}
	nodes, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		button := findNodeKey(nodes, "context-compact")
		if button == nil || button.Label != "Compacting context…" {
			return fmt.Errorf("acknowledged request stopped showing pending state")
		}
		return nil
	})
	if err != nil {
		fail("compaction acknowledgement stays pending", err, nodes, "context-compact-ack")
		return
	}
	push("compaction acknowledgement stays pending", "pass", "fixture ack arrived, but the action stays pending until an authoritative event")

	if err := postFixtureEvent(ctx, control, sessionA, runtimeID, "compactionUpdated", map[string]any{"status": "running", "reason": "manual"}); err != nil {
		fail("compaction running and failure allow retry", err, nodes, "context-compaction-running")
		return
	}
	nodes, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		button := findNodeKey(nodes, "context-compact")
		if button == nil || button.Label != "Compacting context…" {
			return fmt.Errorf("running event did not keep the control disabled")
		}
		return nil
	})
	if err != nil {
		fail("compaction running and failure allow retry", err, nodes, "context-compaction-running")
		return
	}
	if err := postFixtureEvent(ctx, control, sessionA, runtimeID, "compactionUpdated", map[string]any{
		"status": "failed", "reason": "manual", "error": "fixture compaction failed",
	}); err != nil {
		fail("compaction running and failure allow retry", err, nodes, "context-compaction-failed")
		return
	}
	nodes, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		button := findNodeKey(nodes, "context-compact")
		if button == nil || button.Label != "Retry compaction" || !anyLabel(nodes, "fixture compaction failed") {
			return fmt.Errorf("failed event did not enable retry or expose its error")
		}
		return nil
	})
	if err != nil {
		fail("compaction running and failure allow retry", err, nodes, "context-compaction-failed")
		return
	}
	before, err = fixtureCompactCount(ctx, control)
	if err != nil {
		fail("compaction running and failure allow retry", err, nodes, "context-compaction-retry")
		return
	}
	compact = findNodeKey(nodes, "context-compact")
	if err := client.act(ctx, windowID, compact.ID, "", "press", ""); err != nil {
		fail("compaction running and failure allow retry", fmt.Errorf("press retry: %w", err), nodes, "context-compaction-retry")
		return
	}
	if err := waitCompactCount(ctx, control, before+1); err != nil {
		fail("compaction running and failure allow retry", err, nodes, "context-compaction-retry")
		return
	}
	if err := postFixtureEvent(ctx, control, sessionA, runtimeID, "compactionUpdated", map[string]any{
		"status": "completed", "reason": "manual", "summary": "uiverify compaction summary anchor 4391",
	}); err != nil {
		fail("compaction summary action reveals its transcript row", err, nodes, "context-summary")
		return
	}
	nodes, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findNodeKey(nodes, "context-summary") == nil {
			return fmt.Errorf("completed compaction did not expose View summary")
		}
		return nil
	})
	if err != nil {
		fail("compaction summary action reveals its transcript row", err, nodes, "context-summary")
		return
	}
	if err := client.act(ctx, windowID, findNodeKey(nodes, "context-summary").ID, "", "press", ""); err != nil {
		fail("compaction summary action reveals its transcript row", fmt.Errorf("press summary: %w", err), nodes, "context-summary")
		return
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		summary := findTextContaining(nodes, "uiverify compaction summary anchor 4391")
		if summary == nil || summary.Bounds == nil || summary.Bounds.Height <= 0 ||
			containsState(summary.States, "offscreen") || summary.Bounds.Y < 0 || summary.Bounds.Y > 900 {
			return fmt.Errorf("summary transcript row not brought into view")
		}
		return nil
	}); err != nil {
		fail("compaction summary action reveals its transcript row", err, nodes, "context-summary")
		return
	}
	push("compaction running and failure allow retry", "pass", "running disables; failed exposes a retry; retry is sent and acknowledged by fixture")
	push("compaction summary action reveals its transcript row", "pass", "completed event creates the summary row and View summary scrolls to it")
	saveStageTree(roundDir, "fixture-context-panel")
}

func ensureContextPanel(ctx context.Context, client *autoClient, windowID int) ([]autoNode, error) {
	nodes, err := client.tree(ctx, windowID)
	if err != nil {
		return nil, err
	}
	if findNodeKey(nodes, "context-panel") != nil {
		return nodes, nil
	}
	button := findNodeKey(nodes, "composer-usage")
	if button == nil {
		return nodes, fmt.Errorf("composer Context control is not in the automation tree")
	}
	if err := client.act(ctx, windowID, button.ID, "", "press", ""); err != nil {
		return nodes, fmt.Errorf("open Context: %w", err)
	}
	return waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findNodeKey(nodes, "context-panel") == nil {
			return fmt.Errorf("Context surface has not opened")
		}
		return nil
	})
}

func findContextWindowSelect(nodes []autoNode, sessionID string) *autoNode {
	return findNodeKey(nodes, "context-window-"+sessionID+"-")
}

func containsState(states []string, want string) bool {
	for _, state := range states {
		if state == want {
			return true
		}
	}
	return false
}

func findNodeKey(nodes []autoNode, prefix string) *autoNode {
	for i := range nodes {
		if strings.HasPrefix(keyString(nodes[i].Key), prefix) {
			return &nodes[i]
		}
	}
	return nil
}

func waitContextPreference(ctx context.Context, path, value string) error {
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		data, err := os.ReadFile(path)
		if err == nil {
			var prefs map[string]json.RawMessage
			var contextPrefs struct {
				LastModel string            `json:"lastModel"`
				Windows   map[string]string `json:"windows"`
			}
			if json.Unmarshal(data, &prefs) == nil && json.Unmarshal(prefs["context_model_preferences"], &contextPrefs) == nil &&
				contextPrefs.LastModel == contextModelID && contextPrefs.Windows[contextModelID] == value {
				return nil
			}
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
	return fmt.Errorf("app-settings.json never saved lastModel=%q and window=%q", contextModelID, value)
}

func fixtureCompactCount(ctx context.Context, control fixtureControl) (uint64, error) {
	data, err := control.call(ctx, http.MethodGet, "/compactions")
	if err != nil {
		return 0, err
	}
	var result struct {
		Count uint64 `json:"count"`
	}
	if err := json.Unmarshal(data, &result); err != nil {
		return 0, err
	}
	return result.Count, nil
}

func waitFixtureContextWindow(ctx context.Context, control fixtureControl, sessionID, want string) error {
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		raw, err := control.call(ctx, http.MethodGet, "/session?sessionId="+sessionID)
		if err == nil {
			var session struct {
				ContextWindow string `json:"context_window"`
			}
			if json.Unmarshal(raw, &session) == nil && session.ContextWindow == want {
				return nil
			}
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
	return fmt.Errorf("fixture persisted session %s with contextWindow=%q", shortSession(sessionID), want)
}

func waitCompactCount(ctx context.Context, control fixtureControl, want uint64) error {
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		count, err := fixtureCompactCount(ctx, control)
		if err == nil && count >= want {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
	count, err := fixtureCompactCount(ctx, control)
	if err != nil {
		return err
	}
	return fmt.Errorf("fixture received %d compactSession requests, want at least %d", count, want)
}

func postFixtureEvent(ctx context.Context, control fixtureControl, sessionID, runtimeID, kind string, payload any) error {
	data, err := json.Marshal(map[string]any{"sessionId": sessionID, "runtimeId": runtimeID, "kind": kind, "payload": payload})
	if err != nil {
		return err
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, "http://"+control.address+"/event", bytes.NewReader(data))
	if err != nil {
		return err
	}
	request.Header.Set("Authorization", "Bearer "+control.token)
	request.Header.Set("Content-Type", "application/json")
	response, err := (&http.Client{Timeout: perRequestTimeout}).Do(request)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return fmt.Errorf("fixture event %s: HTTP %s", kind, response.Status)
	}
	return nil
}

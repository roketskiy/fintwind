package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"fintwind-go/internal/daemon"

	"github.com/google/uuid"
)

type liveEvent struct {
	Kind string
	Raw  json.RawMessage
}

type activityView struct {
	ID        string  `json:"id"`
	SourceID  *string `json:"source_id"`
	Kind      string  `json:"kind"`
	Title     string  `json:"title"`
	Arguments *string `json:"arguments"`
	Output    *string `json:"output"`
	Failed    bool    `json:"failed"`
	Complete  bool    `json:"complete"`
}

func runActivityFlow(ctx context.Context, daemonBinary string, inst *round, client *daemon.Client, state *reportState) error {
	sid, rid, pid := uuid.NewString(), uuid.NewString(), uuid.NewString()
	options := map[string]any{
		"binary": state.Self, "cwd": state.Workspace, "mode": "ask", "interactionMode": "code", "model": "fixture/activity",
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": options}); err != nil {
		return fmt.Errorf("start isolated runtime: %w", err)
	}
	if err := waitKind(ctx, client, "connected"); err != nil {
		return fmt.Errorf("wait runtime connected: %w", err)
	}
	now := uint64(time.Now().Unix())
	turn1 := uuid.NewString()
	if err := admitSession(ctx, client, sid, pid, state.Workspace, turn1, "activity-suite", 1, nil, now); err != nil {
		return fmt.Errorf("admit first turn: %w", err)
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turn1, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return fmt.Errorf("dispatch first turn: %w", err)
	}
	firstEvents, err := waitTurnEvents(ctx, client)
	if err != nil {
		return err
	}
	if err := verifyLiveEvents(firstEvents, state); err != nil {
		return err
	}
	firstSnapshot, err := hydrate(ctx, client, sid)
	if err != nil {
		return err
	}
	firstActivities, err := activities(firstSnapshot)
	if err != nil {
		return err
	}
	if err := verifyActivities(firstActivities, "persisted live transcript"); err != nil {
		return err
	}
	note(state, "sqlite-live-activity-hydrate", nil)

	// A second accepted submission contains only invalid/late tool frames.
	// It must not start the provider turn or alter the previous terminal item.
	turn2 := uuid.NewString()
	if err := admitSession(ctx, client, sid, pid, state.Workspace, turn2, "invalid-only", 2, firstSnapshot, uint64(time.Now().Unix())); err != nil {
		return fmt.Errorf("admit invalid-only turn: %w", err)
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turn2, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return fmt.Errorf("dispatch invalid-only turn: %w", err)
	}
	secondEvents, err := waitTurnEvents(ctx, client)
	if err != nil {
		return err
	}
	for _, ev := range secondEvents {
		if ev.Kind == "turnStarted" || ev.Kind == "richActivity" {
			err := fmt.Errorf("invalid-only turn emitted %s", ev.Kind)
			note(state, "invalid-events-no-activity-or-turn", err)
			return err
		}
	}
	secondSnapshot, err := hydrate(ctx, client, sid)
	if err != nil {
		return err
	}
	secondActivities, err := activities(secondSnapshot)
	if err != nil {
		return err
	}
	if err := verifyActivities(secondActivities, "after invalid-only turn"); err != nil {
		note(state, "invalid-events-no-activity-or-turn", err)
		return err
	}
	turns, _ := secondSnapshot["turns"].([]any)
	if len(turns) != 2 {
		err := fmt.Errorf("expected the two admitted turns only, got %d", len(turns))
		note(state, "invalid-events-no-activity-or-turn", err)
		return err
	}
	turn2Obj, _ := turns[1].(map[string]any)
	if turn2Obj["provider_turn_started"] == true {
		err := errors.New("invalid tool events marked provider turn as started")
		note(state, "invalid-events-no-activity-or-turn", err)
		return err
	}
	note(state, "invalid-events-no-activity-or-turn", nil)

	// Stop the runtime and daemon, then restart on the same isolated SQLite
	// profile. A fresh actor resumes the native session and reconciles transcript.
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "closeSession"}); err != nil {
		return fmt.Errorf("close runtime before restart: %w", err)
	}
	client.Close()
	closeDaemon(inst)

	restarted, err := startDaemon(ctx, daemonBinary, inst.root, state.Workspace)
	if err != nil {
		return fmt.Errorf("restart isolated daemon: %w", err)
	}
	defer closeDaemon(restarted)
	restartedClient, err := daemon.Dial(ctx, restarted.address, restarted.token)
	if err != nil {
		return fmt.Errorf("dial restarted daemon: %w", err)
	}
	defer restartedClient.Close()
	resumeOptions := map[string]any{
		"binary": state.Self, "cwd": state.Workspace, "mode": "ask", "interactionMode": "code", "model": "fixture/activity",
		"providerCursor": map[string]string{"provider": "openCode", "sessionId": nativeFixtureSession},
	}
	if _, err := restartedClient.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": resumeOptions}); err != nil {
		return fmt.Errorf("resume native fixture runtime: %w", err)
	}
	if err := waitKind(ctx, restartedClient, "connected"); err != nil {
		return err
	}
	recovered, err := waitHydratedActivities(ctx, restartedClient, sid)
	if err != nil {
		return err
	}
	if err := verifyActivities(recovered, "native transcript after restart"); err != nil {
		return err
	}
	if err := compareActivityViews(firstActivities, recovered); err != nil {
		return err
	}
	note(state, "native-restart-reconciliation", nil)
	state.Counts["activities"] = len(recovered)
	state.Counts["liveRichActivityFrames"] = countKind(firstEvents, "richActivity")
	state.Counts["invalidOnlyEvents"] = len(secondEvents)
	state.Counts["sessionProvider"] = "opencode"
	state.Counts["nativeSession"] = nativeFixtureSession
	return nil
}

func waitKind(ctx context.Context, client *daemon.Client, kind string) error {
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case ev, ok := <-client.Events():
			if !ok {
				return errors.New("daemon event stream closed")
			}
			if ev.Kind == kind {
				if ev.Type == "event" {
					client.Ack(ev)
				}
				return nil
			}
			if ev.Type == "event" {
				client.Ack(ev)
			}
		}
	}
}

func waitTurnEvents(ctx context.Context, client *daemon.Client) ([]liveEvent, error) {
	var events []liveEvent
	for {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case ev, ok := <-client.Events():
			if !ok {
				return nil, errors.New("daemon event stream closed before turnFinished")
			}
			if ev.Type == "event" {
				client.Ack(ev)
			}
			events = append(events, liveEvent{Kind: ev.Kind, Raw: append(json.RawMessage(nil), ev.Payload...)})
			if ev.Kind == "turnFinished" {
				return events, nil
			}
		}
	}
}

func hydrate(ctx context.Context, client *daemon.Client, sid string) (map[string]any, error) {
	raw, err := client.RequestFor(ctx, sid, "", map[string]any{"type": "hydrateSession", "sessionId": sid})
	if err != nil {
		return nil, err
	}
	var result map[string]any
	if err := json.Unmarshal(raw, &result); err != nil {
		return nil, err
	}
	session, ok := result["session"].(map[string]any)
	if !ok {
		return nil, fmt.Errorf("hydrateSession omitted session: %s", raw)
	}
	return session, nil
}

func activities(session map[string]any) ([]activityView, error) {
	rawBlocks, _ := session["transcript_blocks"].([]any)
	var out []activityView
	for _, rawBlock := range rawBlocks {
		block, _ := rawBlock.(map[string]any)
		content, _ := block["content"].(map[string]any)
		if content["kind"] != "activities" {
			continue
		}
		items, _ := content["data"].([]any)
		for _, raw := range items {
			encoded, _ := json.Marshal(raw)
			var item activityView
			if err := json.Unmarshal(encoded, &item); err != nil {
				return nil, err
			}
			out = append(out, item)
		}
	}
	return out, nil
}

func expectedKinds() []string {
	return []string{"fileRead", "command", "fileChange", "fileSearch", "plan", "fileRead", "tool", "tool"}
}

func verifyActivities(items []activityView, label string) error {
	tools := fixtureTools()
	if len(items) != len(tools) {
		return fmt.Errorf("%s: expected %d activities, got %d", label, len(tools), len(items))
	}
	kinds := expectedKinds()
	for i, item := range items {
		tool := tools[i]
		if item.SourceID == nil || *item.SourceID != tool.id {
			return fmt.Errorf("%s activity %d source_id=%v, want %s", label, i, item.SourceID, tool.id)
		}
		if item.Kind != kinds[i] || item.Title != tool.name {
			return fmt.Errorf("%s activity %s kind/title=%s/%q, want %s/%q", label, tool.id, item.Kind, item.Title, kinds[i], tool.name)
		}
		if !item.Complete || item.Failed != (tool.error != "") {
			return fmt.Errorf("%s activity %s complete/failed=%v/%v", label, tool.id, item.Complete, item.Failed)
		}
		if item.Arguments == nil {
			return fmt.Errorf("%s activity %s lost arguments", label, tool.id)
		}
		var gotInput map[string]any
		if err := json.Unmarshal([]byte(*item.Arguments), &gotInput); err != nil {
			return fmt.Errorf("%s activity %s arguments invalid: %w", label, tool.id, err)
		}
		wantRaw, _ := json.Marshal(tool.input)
		gotRaw, _ := json.Marshal(gotInput)
		if string(gotRaw) != string(wantRaw) {
			return fmt.Errorf("%s activity %s arguments=%s want=%s", label, tool.id, gotRaw, wantRaw)
		}
		wantOutput := tool.output
		if tool.error != "" {
			wantOutput = tool.error
		}
		if item.Output == nil || *item.Output != wantOutput {
			return fmt.Errorf("%s activity %s output=%v want=%q", label, tool.id, item.Output, wantOutput)
		}
		if tool.error != "" && (strings.HasPrefix(*item.Output, "{") || strings.Contains(*item.Output, `"message"`)) {
			return fmt.Errorf("%s activity %s exposed JSON error envelope: %q", label, tool.id, *item.Output)
		}
		if strings.Contains(item.Title, "<|") || (item.Output != nil && strings.Contains(*item.Output, "<|")) {
			return fmt.Errorf("%s activity %s leaked provider control marker", label, tool.id)
		}
	}
	return nil
}

func verifyLiveEvents(events []liveEvent, state *reportState) error {
	tools := fixtureTools()
	type observation struct {
		id, source, title, kind string
		args                    *string
		output                  *string
		complete, failed        bool
	}
	var rich []observation
	var deltas []string
	for _, ev := range events {
		if ev.Kind == "textDelta" {
			var payload struct {
				Delta string `json:"delta"`
			}
			if err := json.Unmarshal(ev.Raw, &payload); err == nil {
				deltas = append(deltas, payload.Delta)
			}
		}
		if ev.Kind != "richActivity" {
			continue
		}
		var item activityView
		if err := json.Unmarshal(ev.Raw, &item); err != nil {
			return fmt.Errorf("decode richActivity: %w", err)
		}
		source := ""
		if item.SourceID != nil {
			source = *item.SourceID
		}
		rich = append(rich, observation{id: item.ID, source: source, title: item.Title, kind: item.Kind, args: item.Arguments, output: item.Output, complete: item.Complete, failed: item.Failed})
	}
	if len(rich) != len(tools)*4 {
		return fmt.Errorf("richActivity frames=%d, want started/called/progress/terminal x %d", len(rich), len(tools))
	}
	for index, tool := range tools {
		base := index * 4
		first := rich[base]
		if first.source != tool.id || first.title != tool.name || first.kind != expectedKinds()[index] || first.id == "" {
			return fmt.Errorf("richActivity %s started identity/category mismatch: %+v", tool.id, first)
		}
		for j := 0; j < 4; j++ {
			current := rich[base+j]
			if current.id != first.id || current.source != first.source || current.title != first.title || current.kind != first.kind {
				return fmt.Errorf("richActivity %s changed identity/name/category at step %d: %+v", tool.id, j, current)
			}
		}
		called := rich[base+1]
		if called.args == nil {
			return fmt.Errorf("richActivity %s called lost input", tool.id)
		}
		var got, want map[string]any
		if err := json.Unmarshal([]byte(*called.args), &got); err != nil {
			return err
		}
		wantRaw, _ := json.Marshal(tool.input)
		gotRaw, _ := json.Marshal(got)
		if json.Unmarshal(wantRaw, &want) != nil || string(gotRaw) != string(wantRaw) {
			return fmt.Errorf("richActivity %s input mismatch: %s", tool.id, gotRaw)
		}
		terminal := rich[base+3]
		wantOutput := tool.output
		if tool.error != "" {
			wantOutput = tool.error
		}
		if !terminal.complete || terminal.failed != (tool.error != "") || terminal.output == nil || *terminal.output != wantOutput {
			return fmt.Errorf("richActivity %s terminal mismatch: %+v", tool.id, terminal)
		}
	}
	if len(deltas) != 3 || deltas[0] != "before " || deltas[1] != "middle " || deltas[2] != "after" {
		return fmt.Errorf("text delta order/content mismatch: %q", deltas)
	}
	kinds := make([]string, 0, len(events))
	for _, event := range events {
		if event.Kind == "textDelta" || event.Kind == "richActivity" {
			kinds = append(kinds, event.Kind)
		}
	}
	firstRich, midText, lastRich := -1, -1, -1
	for i, kind := range kinds {
		if kind == "richActivity" && firstRich < 0 {
			firstRich = i
		}
		if kind == "textDelta" && midText < 0 && i > 0 {
			midText = i
		}
		if kind == "richActivity" {
			lastRich = i
		}
	}
	if firstRich < 0 || midText <= firstRich || lastRich <= midText {
		return fmt.Errorf("text/tool interleave order invalid: %v", kinds)
	}
	for _, event := range events {
		if strings.Contains(string(event.Raw), "<|") {
			return fmt.Errorf("provider private marker leaked in %s", event.Kind)
		}
	}
	note(state, "live-rich-activity-order-and-input", nil)
	note(state, "live-text-tool-interleaving", nil)
	note(state, "live-activity-classification", nil)
	note(state, "live-terminal-error-message", nil)
	note(state, "unknown-progress-late-progress-ignored", nil)
	return nil
}

func compareActivityViews(live, recovered []activityView) error {
	if len(live) != len(recovered) {
		return fmt.Errorf("restart activity count %d differs from live %d", len(recovered), len(live))
	}
	for i := range live {
		a, b := live[i], recovered[i]
		if a.SourceID == nil || b.SourceID == nil || *a.SourceID != *b.SourceID || a.Kind != b.Kind || a.Title != b.Title || a.Output == nil || b.Output == nil || *a.Output != *b.Output || a.Complete != b.Complete || a.Failed != b.Failed {
			return fmt.Errorf("restart activity %d differs: live=%+v recovered=%+v", i, a, b)
		}
		if a.Arguments == nil || b.Arguments == nil {
			return fmt.Errorf("restart activity %d lost arguments", i)
		}
		var av, bv any
		if json.Unmarshal([]byte(*a.Arguments), &av) != nil || json.Unmarshal([]byte(*b.Arguments), &bv) != nil {
			return fmt.Errorf("restart activity %d arguments are invalid", i)
		}
		ar, _ := json.Marshal(av)
		br, _ := json.Marshal(bv)
		if string(ar) != string(br) {
			return fmt.Errorf("restart activity %d arguments differ: %s != %s", i, ar, br)
		}
	}
	return nil
}

func waitHydratedActivities(ctx context.Context, client *daemon.Client, sid string) ([]activityView, error) {
	ticker := time.NewTicker(50 * time.Millisecond)
	defer ticker.Stop()
	for {
		session, err := hydrate(ctx, client, sid)
		if err == nil {
			items, parseErr := activities(session)
			if parseErr == nil && len(items) == len(fixtureTools()) {
				return items, nil
			}
		}
		select {
		case <-ctx.Done():
			return nil, fmt.Errorf("native activities not hydrated: %w", ctx.Err())
		case <-ticker.C:
		}
	}
}

func countKind(events []liveEvent, kind string) int {
	count := 0
	for _, event := range events {
		if event.Kind == kind {
			count++
		}
	}
	return count
}

func dispatch(ctx context.Context, client *daemon.Client, sid, rid, submission string) error {
	_, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": submission, "workspace": map[string]string{"kind": "local"}})
	return err
}

func _compileGuard() {
	_ = dispatch
}

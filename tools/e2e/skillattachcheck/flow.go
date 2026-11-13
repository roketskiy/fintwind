package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

type observedEvent struct {
	Kind    string          `json:"kind"`
	Payload json.RawMessage `json:"payload,omitempty"`
}
type harness struct {
	ctx                                               context.Context
	round, home, dataDir, workspace, daemonPath, self string
	result                                            map[string]any
	checks                                            map[string]bool
	owned                                             *ownedprocess.Owned
	client                                            *daemon.Client
	address, fixtureURL                               string
	sessionID, runtimeID, projectID                   string
	events                                            []observedEvent
}

func newHarness(ctx context.Context, round, self, daemonPath string, result map[string]any, checks map[string]bool) (*harness, error) {
	h := &harness{ctx: ctx, round: round, home: filepath.Join(round, "home"), dataDir: filepath.Join(round, "data"), daemonPath: daemonPath, self: self, result: result, checks: checks, sessionID: uuid.NewString(), runtimeID: uuid.NewString(), projectID: uuid.NewString()}
	h.workspace = filepath.Join(h.round, "workspace", "main", "skillattachcheck")
	for _, dir := range []string{h.home, h.dataDir, h.workspace, filepath.Join(round, "daemon-logs")} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return nil, errors.New("cannot create isolated E2E directories")
		}
	}
	attachPath := filepath.Join(h.workspace, "skill-attachment.txt")
	if err := os.WriteFile(attachPath, []byte("synthetic attachment for skillattachcheck\n"), 0o600); err != nil {
		return nil, errors.New("cannot write workspace attachment fixture")
	}
	outsidePath := filepath.Join(h.home, "outside-workspace.txt")
	if err := os.WriteFile(outsidePath, []byte("outside workspace fixture\n"), 0o600); err != nil {
		return nil, errors.New("cannot write outside-workspace fixture")
	}
	if err := h.launchDaemon(); err != nil {
		return nil, err
	}
	if err := h.openRuntime(); err != nil {
		h.closeOwned()
		return nil, err
	}
	if err := h.bootstrap(); err != nil {
		h.closeOwned()
		return nil, err
	}
	addr, err := waitFixtureAddress(ctx, filepath.Join(h.home, "goalcheck-address.txt"))
	if err != nil {
		h.closeOwned()
		return nil, errors.New("fake OpenCode server address was not announced")
	}
	h.fixtureURL = addr
	result["syntheticProviderFixture"] = map[string]any{"engine": "owned skillattachcheck loopback HTTP/SSE", "nativeSession": fakeNativeID, "realRequests": 0, "networkScope": "127.0.0.1 only"}
	result["attachmentFixturePath"] = attachPath
	return h, nil
}

func (h *harness) launchDaemon() error {
	token := uuid.NewString()
	logs := filepath.Join(h.round, "daemon-logs-"+uuid.NewString())
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return errors.New("cannot create daemon-specific logs directory")
	}
	child, err := ownedprocess.StartWithEnvironment(h.daemonPath, []string{"--data-dir", h.dataDir, "--bind", "127.0.0.1:0"}, logs, h.round, ownedprocess.BuildEnv(h.round, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if err != nil {
		return errors.New("cannot launch owned daemon")
	}
	h.owned = child
	address, err := waitReady(h.ctx, child)
	if err != nil {
		_ = child.Close()
		h.owned = nil
		return errors.New("owned daemon did not become ready")
	}
	client, err := daemon.Dial(h.ctx, address, token)
	if err != nil {
		_ = child.Close()
		h.owned = nil
		return errors.New("cannot connect to owned daemon websocket")
	}
	h.address = address
	h.client = client
	return nil
}

func (h *harness) openRuntime() error {
	options := map[string]any{"binary": h.self, "cwd": h.workspace, "mode": "ask", "interactionMode": "code", "model": "fixture/goalcheck"}
	if _, err := h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "start", "options": options}); err != nil {
		return fmt.Errorf("start runtime: %w", err)
	}
	if _, err := h.waitKind("connected"); err != nil {
		return fmt.Errorf("wait runtime connected: %w", err)
	}
	return nil
}

func (h *harness) bootstrap() error {
	now := uint64(time.Now().Unix())
	turnID := uuid.NewString()
	record := model.NewSubmissionRecord(h.sessionID, turnID, "bootstrap", nil)
	turn := map[string]any{"id": turnID, "status": "running", "turn_count": 1, "started_at": now, "submission": record.Receipt}
	project := map[string]any{"id": h.projectID, "name": "goalcheck", "path": h.workspace, "created_at": now}
	user := map[string]any{"id": uuid.NewString(), "turn_id": turnID, "role": "user", "content": "bootstrap", "created_at": now, "attachments": []any{}, "streaming": false}
	session := map[string]any{"id": h.sessionID, "project_id": h.projectID, "title": "goalcheck synthetic fixture", "provider": "opencode", "status": "working", "created_at": now, "updated_at": now, "detail_loaded": true, "messages": []any{user}, "turns": []any{turn}, "transcript_blocks": []any{}}
	if _, err := h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "acceptSubmission", "project": project, "session": session, "submission": record}); err != nil {
		return fmt.Errorf("accept bootstrap: %w", err)
	}
	if _, err := h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return fmt.Errorf("dispatch synthetic bootstrap: %w", err)
	}
	done, err := h.waitTurnFinished()
	if err != nil {
		return err
	}
	if !eventBool(done.Payload, "success") {
		return errors.New("synthetic bootstrap did not complete successfully")
	}
	sessionData, err := h.hydrate()
	if err != nil {
		return err
	}
	if string(sessionData["status"]) != `"idle"` {
		return errors.New("bootstrap did not settle the local runtime")
	}
	h.checks["daemon-websocket-owned-fake-bootstrap"] = true
	return nil
}

func (h *harness) closeOwned() error {
	if h.client != nil {
		h.client.Close()
		h.client = nil
	}
	if h.owned != nil {
		err := h.owned.Close()
		h.owned = nil
		h.result["ownedDaemonHandleClosed"] = err == nil
		if err != nil {
			return err
		}
	}
	return nil
}

func (h *harness) nextEvent(ctx context.Context) (daemon.Event, error) {
	select {
	case <-ctx.Done():
		return daemon.Event{}, ctx.Err()
	case ev, ok := <-h.client.Events():
		if !ok {
			return daemon.Event{}, errors.New("daemon websocket event stream closed")
		}
		if ev.Type == "event" {
			h.client.Ack(ev)
		}
		h.events = append(h.events, observedEvent{Kind: ev.Kind, Payload: append(json.RawMessage(nil), ev.Payload...)})
		if bytes.Contains(ev.Payload, []byte("synthetic-private-payload")) {
			return ev, fmt.Errorf("private fixture payload leaked in %s", ev.Kind)
		}
		return ev, nil
	}
}

func (h *harness) waitKind(kind string) (daemon.Event, error) {
	ctx, cancel := context.WithTimeout(h.ctx, 15*time.Second)
	defer cancel()
	for {
		ev, err := h.nextEvent(ctx)
		if err != nil {
			if errors.Is(err, context.DeadlineExceeded) {
				return ev, fmt.Errorf("timed out after 15 seconds waiting for daemon event %s", kind)
			}
			return ev, err
		}
		if ev.Kind == kind {
			return ev, nil
		}
	}
}

func (h *harness) waitTurnFinished() (daemon.Event, error) { return h.waitKind("turnFinished") }

func (h *harness) waitGoalStatus(status string) (daemon.Event, error) {
	for {
		ev, err := h.waitKind("goalUpdated")
		if err != nil {
			return ev, err
		}
		got, ok := eventString(ev.Payload, "status")
		if status == "null" {
			if string(ev.Payload) == "null" {
				return ev, nil
			}
		} else if ok && got == status {
			return ev, nil
		}
	}
}

func (h *harness) waitGoalStatusReported(ok bool) (daemon.Event, error) {
	ev, err := h.waitKind("goalStatusReported")
	if err != nil {
		return ev, err
	}
	var payload map[string]json.RawMessage
	if json.Unmarshal(ev.Payload, &payload) != nil {
		return ev, errors.New("goalStatusReported payload invalid")
	}
	key := "Err"
	if ok {
		key = "Ok"
	}
	if _, found := payload[key]; !found {
		return ev, fmt.Errorf("goalStatusReported omitted %s", key)
	}
	if ok {
		if _, found := payload["Err"]; found {
			return ev, errors.New("successful Goal Get included Err")
		}
	} else if eventStringRaw(payload["Err"]) != "Could not read Goal status" {
		return ev, errors.New("Goal Get error was not sanitized")
	}
	return ev, nil
}

func (h *harness) goal(action map[string]any) error {
	_, err := h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "goal", "action": action})
	return err
}

func (h *harness) hydrate() (map[string]json.RawMessage, error) {
	raw, err := h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "hydrateSession", "sessionId": h.sessionID})
	if err != nil {
		return nil, err
	}
	var envelope struct {
		Session map[string]json.RawMessage `json:"session"`
	}
	if json.Unmarshal(raw, &envelope) != nil || envelope.Session == nil {
		return nil, errors.New("hydrateSession omitted session")
	}
	return envelope.Session, nil
}

func (h *harness) fixtureConfig(values map[string]any) error {
	_, status, err := fixtureRequest(h.ctx, h.fixtureURL, "/__fixture/config", values)
	if err != nil || status != 200 {
		return errors.New("configure local synthetic fixture failed")
	}
	return nil
}

func (h *harness) fixtureEmit(kind string, data map[string]any) error {
	_, status, err := fixtureRequest(h.ctx, h.fixtureURL, "/__fixture/emit", map[string]any{"type": kind, "data": data})
	if err != nil || status != 200 {
		return errors.New("emit local synthetic event failed")
	}
	return nil
}

func (h *harness) fixtureStats() (map[string]json.RawMessage, error) {
	raw, status, err := fixtureRequest(h.ctx, h.fixtureURL, "/__fixture/stats", nil)
	if err != nil || status != 200 {
		return nil, errors.New("read local fixture counters failed")
	}
	var stats map[string]json.RawMessage
	if json.Unmarshal(raw, &stats) != nil {
		return nil, errors.New("local fixture counters invalid")
	}
	return stats, nil
}

func (h *harness) expectNoKind(kind string, duration time.Duration) error {
	ctx, cancel := context.WithTimeout(h.ctx, duration)
	defer cancel()
	for {
		ev, err := h.nextEvent(ctx)
		if errors.Is(err, context.DeadlineExceeded) {
			return nil
		}
		if err != nil {
			return err
		}
		if ev.Kind == kind {
			return fmt.Errorf("unexpected %s event", kind)
		}
	}
}

func (h *harness) drainQuiet(duration time.Duration) error {
	ctx, cancel := context.WithTimeout(h.ctx, duration)
	defer cancel()
	for {
		_, err := h.nextEvent(ctx)
		if errors.Is(err, context.DeadlineExceeded) {
			return nil
		}
		if err != nil {
			return err
		}
	}
}

func (h *harness) runScenario() error {
	if stats, err := h.fixtureStats(); err != nil {
		return err
	} else {
		var calls []fakeCall
		_ = json.Unmarshal(stats["calls"], &calls)
		for _, call := range calls {
			if strings.Contains(call.Path, "/api/config") || strings.Contains(call.Path, "/api/skill") {
				return errors.New("plain bootstrap prompt unexpectedly queried registry")
			}
		}
	}
	registry := []map[string]any{{"id": "short", "name": "Short name"}, {"id": "short name", "name": "Long display"}, {"id": "short-name", "name": "Display is not an ID"}, {"id": "CaseID", "name": "caseid"}, {"id": "éclair", "name": "unicode"}, {"id": "spaceID", "name": "unicode whitespace"}, {"id": "slash-cmd", "name": "Slash"}}
	if err := h.fixtureConfig(map[string]any{"registry": registry}); err != nil {
		return err
	}
	if err := h.submitRoot(uuid.NewString(), "use @short name! @CaseID, @short-name; @éclair。 @spaceID\u2003\n/CaseID\n/short-name", true); err != nil {
		return err
	}
	if err := h.assertLastRequest("/prompt", []string{"short name", "CaseID", "short-name", "éclair", "spaceID"}, true); err != nil {
		return err
	}
	h.checks["root-prompt-longest-case-order-unicode-files"] = true
	h.checks["duplicate-sigil-mixed-at-slash-dedup-first-occurrence-order"] = true
	if err := h.assertNoFabricatedSkillExecution(); err != nil {
		return err
	}
	h.checks["sqlite-history-keeps-text-without-fabricated-skill-execution"] = true
	if err := h.submitRoot(uuid.NewString(), "@caseid @short-nameX @Short name", false); err != nil {
		return err
	}
	if err := h.assertLastRequest("/prompt", nil, false); err != nil {
		return err
	}
	h.checks["case-sensitive-boundary-no-match-omits-skills"] = true
	before := h.registryCallCount()
	if err := h.submitRoot(uuid.NewString(), "C:/workspace/slash-cmd", false); err != nil {
		return err
	}
	if h.registryCallCount() != before {
		return errors.New("path slash unexpectedly triggered skill registry")
	}
	if err := h.submitRoot(uuid.NewString(), "/slash-cmd", false); err != nil {
		return err
	}
	if err := h.assertLastRequest("/prompt", []string{"slash-cmd"}, false); err != nil {
		return err
	}
	h.checks["slash-only-at-command-line-start"] = true
	if err := h.fixtureConfig(map[string]any{"registry": []map[string]any{{"id": "fresh"}}}); err != nil {
		return err
	}
	if err := h.submitRoot(uuid.NewString(), "@fresh", false); err != nil {
		return err
	}
	if err := h.assertLastRequest("/prompt", []string{"fresh"}, false); err != nil {
		return err
	}
	h.checks["registry-is-fetched-dynamically"] = true
	if err := h.steer("steer @fresh", true); err != nil {
		return err
	}
	if err := h.assertLastRequest("/prompt", []string{"fresh"}, true); err != nil {
		return err
	}
	h.checks["steer-native-skills-and-files"] = true
	goalFile := filepath.Join(h.workspace, "goal.txt")
	if err := os.WriteFile(goalFile, []byte("synthetic Goal attachment"), 0600); err != nil {
		return err
	}
	if _, err := h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "goal", "action": map[string]any{"type": "start", "objective": "/fresh", "attachments": []map[string]any{{"path": goalFile, "name": "goal.txt"}}}}); err != nil {
		return err
	}
	if _, err := h.waitTurnFinished(); err != nil {
		return err
	}
	if err := h.assertLastRequest("/command", []string{"fresh"}, true); err != nil {
		return err
	}
	h.checks["goal-start-native-skills"] = true
	if err := h.testPrePostFailures(); err != nil {
		return err
	}
	return nil
}

func (h *harness) submitRoot(turnID, text string, withFile bool) error {
	return h.submitRootExpect(turnID, text, withFile, true, "")
}

func (h *harness) submitRootExpect(turnID, text string, withFile, wantSuccess bool, receiptState string) error {
	sessionRaw, err := h.hydrate()
	if err != nil {
		return err
	}
	encoded, _ := json.Marshal(sessionRaw)
	var session map[string]any
	if json.Unmarshal(encoded, &session) != nil {
		return errors.New("cannot decode local session")
	}
	projectID, _ := session["project_id"].(string)
	files := []model.SubmissionFile{}
	fileInput := []map[string]any{}
	if withFile {
		path := filepath.Join(h.workspace, "attached.txt")
		if err := os.WriteFile(path, []byte("synthetic file"), 0600); err != nil {
			return err
		}
		files = append(files, model.SubmissionFile{Path: path, Name: "attached.txt"})
		fileInput = append(fileInput, map[string]any{"path": path, "name": "attached.txt"})
	}
	record := model.NewSubmissionRecord(h.sessionID, turnID, text, files)
	now := uint64(time.Now().Unix())
	turns, _ := session["turns"].([]any)
	turns = append(turns, map[string]any{"id": turnID, "status": "running", "turn_count": len(turns) + 1, "started_at": now, "submission": record.Receipt})
	session["turns"] = turns
	messages, _ := session["messages"].([]any)
	messages = append(messages, map[string]any{"id": uuid.NewString(), "turn_id": turnID, "role": "user", "content": text, "created_at": now, "attachments": fileInput, "streaming": false})
	session["messages"] = messages
	session["status"] = "working"
	session["updated_at"] = now
	session["last_reply_at"] = now
	project := map[string]any{"id": projectID, "name": "skillattachcheck", "path": h.workspace, "created_at": now}
	if _, err = h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "acceptSubmission", "project": project, "session": session, "submission": record}); err != nil {
		return err
	}
	if _, err = h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return err
	}
	if wantSuccess {
		ev, waitErr := h.waitTurnFinished()
		if waitErr != nil {
			return waitErr
		}
		if !eventBool(ev.Payload, "success") {
			return fmt.Errorf("root turn %s failed", turnID)
		}
	} else if receiptState != "" {
		if err := h.waitSubmissionState(turnID, receiptState); err != nil {
			return err
		}
	}
	if !wantSuccess {
		snap, e := h.hydrate()
		if e != nil {
			return e
		}
		encoded, _ := json.Marshal(snap)
		if !bytes.Contains(encoded, []byte(text)) {
			return errors.New("failed skill resolution did not preserve local prompt text")
		}
		if receiptState != "" {
			var turns []map[string]json.RawMessage
			if json.Unmarshal(snap["turns"], &turns) != nil || len(turns) == 0 {
				return errors.New("failed submission omitted local turn")
			}
			var receipt map[string]json.RawMessage
			_ = json.Unmarshal(turns[len(turns)-1]["submission"], &receipt)
			var state string
			_ = json.Unmarshal(receipt["state"], &state)
			if state != receiptState {
				return fmt.Errorf("failed submission receipt state=%q, expected %s", state, receiptState)
			}
		}
	}
	return nil
}

func (h *harness) waitSubmissionState(id, state string) error {
	ctx, cancel := context.WithTimeout(h.ctx, 15*time.Second)
	defer cancel()
	for {
		ev, err := h.nextEvent(ctx)
		if err != nil {
			return err
		}
		if ev.Kind != "submissionUpdated" {
			continue
		}
		got, _ := eventString(ev.Payload, "id")
		actual, _ := eventString(ev.Payload, "state")
		if got == id && actual == state {
			return nil
		}
	}
}

func (h *harness) steer(text string, withFile bool) error {
	files := []map[string]any{}
	if withFile {
		path := filepath.Join(h.workspace, "steer.txt")
		if err := os.WriteFile(path, []byte("synthetic steer file"), 0600); err != nil {
			return err
		}
		files = append(files, map[string]any{"path": path, "name": "steer.txt"})
	}
	_, err := h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "steer", "prompt": text, "files": files})
	return err
}

func (h *harness) assertLastRequest(suffix string, skills []string, files bool) error {
	stats, err := h.fixtureStats()
	if err != nil {
		return err
	}
	var calls []fakeCall
	if json.Unmarshal(stats["calls"], &calls) != nil {
		return errors.New("invalid fixture trace")
	}
	var found *fakeCall
	foundIndex := -1
	for i := range calls {
		if strings.HasSuffix(calls[i].Path, suffix) {
			found = &calls[i]
			foundIndex = i
		}
	}
	if found == nil {
		return fmt.Errorf("fixture has no %s request", suffix)
	}
	if len(skills) > 0 {
		if foundIndex < 2 || !strings.Contains(calls[foundIndex-2].Path, "/api/config?") || !strings.Contains(calls[foundIndex-1].Path, "/api/skill?") {
			return errors.New("registry requests were not config-then-skill immediately before provider POST")
		}
		for _, call := range calls[foundIndex-2 : foundIndex] {
			parsed, e := url.Parse(call.Path)
			if e != nil || parsed.Query().Get("location[directory]") != h.workspace || call.Directory != url.PathEscape(h.workspace) {
				return errors.New("registry directory query/header mismatch")
			}
		}
	}
	var body map[string]json.RawMessage
	if json.Unmarshal(found.Body, &body) != nil {
		return errors.New("request body invalid")
	}
	if len(skills) == 0 {
		if _, ok := body["skills"]; ok {
			return errors.New("unexpected skills field")
		}
	} else {
		var got []struct {
			ID string `json:"id"`
		}
		if json.Unmarshal(body["skills"], &got) != nil || len(got) != len(skills) {
			return fmt.Errorf("skills mismatch: %s", body["skills"])
		}
		for i, id := range skills {
			if got[i].ID != id {
				return fmt.Errorf("skill order mismatch: %s", body["skills"])
			}
		}
	}
	if files {
		var got []map[string]any
		if json.Unmarshal(body["files"], &got) != nil || len(got) != 1 {
			return errors.New("request did not preserve one attachment")
		}
	} else if _, ok := body["files"]; ok {
		return errors.New("unexpected file attachment")
	}
	return nil
}

func (h *harness) registryCallCount() int {
	stats, err := h.fixtureStats()
	if err != nil {
		return -1
	}
	var calls []fakeCall
	_ = json.Unmarshal(stats["calls"], &calls)
	n := 0
	for _, call := range calls {
		if strings.Contains(call.Path, "/api/config?") || strings.Contains(call.Path, "/api/skill?") {
			n++
		}
	}
	return n
}

func (h *harness) testPrePostFailures() error {
	registry := []map[string]any{{"id": "fresh"}}
	cases := []struct {
		name   string
		config map[string]any
	}{{"config-http500", map[string]any{"registry": registry, "registryStatus": 0, "configStatus": 500, "registryPayload": nil}}, {"skill-http500", map[string]any{"registry": registry, "registryStatus": 500, "configStatus": 0, "registryPayload": nil}}, {"null", map[string]any{"registry": registry, "registryStatus": 0, "registryPayload": json.RawMessage(`{"data":null}`)}}, {"nonarray", map[string]any{"registry": registry, "registryStatus": 0, "registryPayload": json.RawMessage(`{"data":{}}`)}}, {"invalid-id", map[string]any{"registry": []map[string]any{{"id": ""}}, "registryStatus": 0, "registryPayload": nil}}, {"oversized-id", map[string]any{"registry": []map[string]any{{"id": strings.Repeat("x", 1025)}}, "registryStatus": 0, "registryPayload": nil}}}
	for _, tc := range cases {
		if err := h.fixtureConfig(tc.config); err != nil {
			return err
		}
		beforePrompt, beforeCommand := h.promptCount(), h.commandCount()
		if err := h.submitRootExpect(uuid.NewString(), "@fresh", false, false, "notSent"); err != nil {
			return fmt.Errorf("%s root: %w", tc.name, err)
		}
		if h.promptCount() != beforePrompt {
			return fmt.Errorf("%s posted root prompt after registry failure", tc.name)
		}
		if err := h.steer("@fresh", false); err == nil {
			return fmt.Errorf("%s steer unexpectedly succeeded", tc.name)
		}
		if err := h.goal(map[string]any{"type": "start", "objective": "@fresh"}); err == nil {
			return fmt.Errorf("%s Goal start unexpectedly succeeded", tc.name)
		}
		if h.promptCount() != beforePrompt || h.commandCount() != beforeCommand {
			return fmt.Errorf("%s sent provider request after registry failure", tc.name)
		}
	}
	h.checks["registry-http-and-invalid-payload-fail-before-post"] = true
	validPayload := json.RawMessage(`{"data":[{"id":"fresh"}]}`)
	if err := h.fixtureConfig(map[string]any{"registry": registry, "registryPayload": validPayload, "registryStatus": 0, "configStatus": 0, "promptStatus": 422}); err != nil {
		return err
	}
	before := h.promptCount()
	if err := h.submitRootExpect(uuid.NewString(), "@fresh", false, false, "rejected"); err != nil {
		return err
	}
	if h.promptCount() != before+1 {
		return errors.New("legacy skills rejection retried or omitted prompt POST")
	}
	if err := h.assertLastRequest("/prompt", []string{"fresh"}, false); err != nil {
		return err
	}
	h.checks["legacy-422-single-post-no-skills-fallback"] = true
	if err := h.fixtureConfig(map[string]any{"promptStatus": 0, "registryPayload": validPayload, "commandMode": "reject400"}); err != nil {
		return err
	}
	beforeCommands := h.commandCount()
	err := h.goal(map[string]any{"type": "start", "objective": "/fresh"})
	if err == nil {
		return errors.New("legacy Goal command rejection was accepted")
	}
	if h.commandCount() != beforeCommands+1 {
		return errors.New("legacy Goal command rejection retried")
	}
	h.checks["legacy-goal-400-single-post"] = true
	return nil
}

func (h *harness) promptCount() int  { return h.routeCount("/prompt") }
func (h *harness) commandCount() int { return h.routeCount("/command") }
func (h *harness) routeCount(suffix string) int {
	stats, err := h.fixtureStats()
	if err != nil {
		return -1
	}
	var calls []fakeCall
	_ = json.Unmarshal(stats["calls"], &calls)
	n := 0
	for _, call := range calls {
		if strings.HasSuffix(call.Path, suffix) {
			n++
		}
	}
	return n
}

func (h *harness) assertNoFabricatedSkillExecution() error {
	snap, err := h.hydrate()
	if err != nil {
		return err
	}
	raw, _ := json.Marshal(snap)
	if strings.Contains(strings.ToLower(string(raw)), "skill executed") {
		return errors.New("SQLite snapshot fabricated skill execution")
	}
	if !bytes.Contains(raw, []byte("use @short name! @CaseID, @short-name; @éclair。")) {
		return errors.New("SQLite snapshot omitted ordinary root prompt text")
	}
	return nil
}

func eventString(raw json.RawMessage, key string) (string, bool) {
	var value map[string]json.RawMessage
	if json.Unmarshal(raw, &value) != nil {
		return "", false
	}
	var out string
	if json.Unmarshal(value[key], &out) != nil {
		return "", false
	}
	return out, true
}
func eventStringRaw(raw json.RawMessage) string {
	var out string
	_ = json.Unmarshal(raw, &out)
	return out
}
func eventBool(raw json.RawMessage, key string) bool {
	var value map[string]json.RawMessage
	if json.Unmarshal(raw, &value) != nil {
		return false
	}
	var out bool
	_ = json.Unmarshal(value[key], &out)
	return out
}
func eventInt(raw json.RawMessage, key string) int64 {
	var value map[string]json.RawMessage
	if json.Unmarshal(raw, &value) != nil {
		return -1
	}
	var out int64
	_ = json.Unmarshal(value[key], &out)
	return out
}

func (h *harness) safeGoalShape(raw json.RawMessage, status string, turns, maxTurns int64, revision int64, completed *int64) error {
	var goal map[string]json.RawMessage
	if json.Unmarshal(raw, &goal) != nil {
		return errors.New("goalUpdated did not contain a Goal object")
	}
	for _, forbidden := range []string{"privateFixtureField", "metadata", "tools", "providerPayload"} {
		if _, ok := goal[forbidden]; ok {
			return fmt.Errorf("Goal wire leaked native field %s", forbidden)
		}
	}
	if value, _ := eventString(raw, "status"); value != status {
		return fmt.Errorf("Goal status=%s want=%s", value, status)
	}
	if got, ok := eventString(raw, "sessionId"); !ok || got != fakeNativeID {
		return errors.New("Goal native session ID was not mapped")
	}
	if value := eventInt(raw, "turns"); value != turns {
		return fmt.Errorf("Goal turns=%d want=%d", value, turns)
	}
	if value := eventInt(raw, "maxTurns"); value != maxTurns {
		return fmt.Errorf("Goal maxTurns=%d want=%d", value, maxTurns)
	}
	if value := eventInt(raw, "revision"); value != revision {
		return fmt.Errorf("Goal revision=%d want=%d", value, revision)
	}
	if completed != nil && eventInt(raw, "completedAt") != *completed {
		return errors.New("Goal completedAt milliseconds-to-seconds conversion mismatch")
	}
	return nil
}

func (h *harness) testGoalReadAndNormalization() error {
	const updated int64 = 1700000000123
	goalRaw, _ := json.Marshal(map[string]any{"objective": "initial goal", "status": "active", "reason": "synthetic initial", "rounds": 3, "maxRounds": 8, "updatedAt": updated, "privateFixtureField": "synthetic-private-payload"})
	if err := h.fixtureConfig(map[string]any{"goal": json.RawMessage(goalRaw)}); err != nil {
		return err
	}
	statsBefore, err := h.fixtureStats()
	if err != nil {
		return err
	}
	beforeCommands := rawInt(statsBefore["commandCount"])
	if err := h.goal(map[string]any{"type": "get"}); err != nil {
		return fmt.Errorf("Goal get command failed: %w", err)
	}
	updatedEvent, err := h.waitGoalStatus("active")
	if err != nil {
		return err
	}
	if err := h.safeGoalShape(updatedEvent.Payload, "active", 3, 8, updated, nil); err != nil {
		return err
	}
	if _, err := h.waitGoalStatusReported(true); err != nil {
		return err
	}
	statsAfter, err := h.fixtureStats()
	if err != nil {
		return err
	}
	if rawInt(statsAfter["commandCount"]) != beforeCommands {
		return errors.New("Goal get unexpectedly posted /command")
	}
	var calls []fakeCall
	if json.Unmarshal(statsAfter["calls"], &calls) != nil {
		return errors.New("fixture trace invalid")
	}
	getBodyOK := false
	for _, call := range calls {
		if call.Path != "/api/rpc/opencode.goal/get" {
			continue
		}
		var body map[string]map[string]string
		if json.Unmarshal(call.Body, &body) == nil && len(body) == 1 && len(body["input"]) == 1 && body["input"]["sessionID"] == fakeNativeID {
			getBodyOK = true
		}
	}
	if !getBodyOK {
		return errors.New("Goal get POST body did not exactly contain input.sessionID")
	}
	h.checks["goal-get-rpc-exact-body-no-command"] = true
	h.checks["native-goal-active-rounds-revision-whitelist"] = true

	completeRaw, _ := json.Marshal(map[string]any{"objective": "completed goal", "status": "complete", "reason": "fixture complete", "rounds": 7, "maxRounds": 9, "updatedAt": updated, "privateFixtureField": "synthetic-private-payload"})
	if err := h.fixtureConfig(map[string]any{"goal": json.RawMessage(completeRaw)}); err != nil {
		return err
	}
	if err := h.goal(map[string]any{"type": "get"}); err != nil {
		return err
	}
	completeEvent, err := h.waitGoalStatus("done")
	if err != nil {
		return err
	}
	completedAt := updated / 1000
	if err := h.safeGoalShape(completeEvent.Payload, "done", 7, 9, updated, &completedAt); err != nil {
		return err
	}
	if _, err := h.waitGoalStatusReported(true); err != nil {
		return err
	}
	if _, err := h.waitHydratedGoalStatus("done", 2*time.Second); err != nil {
		return fmt.Errorf("successful native Goal read was not durably hydrated: %w", err)
	}
	h.checks["native-goal-complete-done-completedAt-seconds"] = true
	return nil
}

func rawInt(raw json.RawMessage) int { var value int; _ = json.Unmarshal(raw, &value); return value }

func (h *harness) testGoalActionsAndBoundaries() error {
	// A failed get must not replace the last persisted completed Goal.
	if err := h.fixtureConfig(map[string]any{"getStatus": 503}); err != nil {
		return err
	}
	if err := h.goal(map[string]any{"type": "get"}); err == nil {
		return errors.New("failed Goal get unexpectedly succeeded")
	}
	if _, err := h.waitGoalStatusReported(false); err != nil {
		return err
	}
	session, err := h.hydrate()
	if err != nil {
		return err
	}
	if !bytes.Contains(session["goal"], []byte(`"status":"done"`)) {
		h.result["lastHydratedGoalOnReadFailure"] = string(session["goal"])
		return fmt.Errorf("failed Goal get did not preserve the saved done Goal snapshot: %s", session["goal"])
	}
	if err := h.fixtureConfig(map[string]any{"getStatus": 0}); err != nil {
		return err
	}
	h.checks["goal-get-failure-safe-error-preserves-old-snapshot"] = true

	attachment := filepath.Join(h.workspace, "goal-attachment.txt")
	stats, err := h.fixtureStats()
	if err != nil {
		return err
	}
	turnCount := hydrateTurnCount(session)
	if err := h.goal(map[string]any{"type": "start", "objective": "Complete the synthetic fixture goal", "attachments": []map[string]string{{"path": attachment, "name": "goal-attachment.txt"}}}); err != nil {
		return fmt.Errorf("Goal start with attachment failed: %w", err)
	}
	startedGoal, err := h.waitGoalStatus("active")
	if err != nil {
		return err
	}
	finished, err := h.waitTurnFinished()
	if err != nil {
		return err
	}
	if !eventBool(finished.Payload, "success") {
		return errors.New("successful Goal start turn was marked failed")
	}
	if err := h.drainQuiet(80 * time.Millisecond); err != nil {
		return err
	}
	after, err := h.hydrate()
	if err != nil {
		return err
	}
	if hydrateTurnCount(after) != turnCount+1 {
		return errors.New("Goal start did not persist exactly one local turn")
	}
	if err := assertGoalAttachment(after, attachment); err != nil {
		return err
	}
	stats, err = h.fixtureStats()
	if err != nil {
		return err
	}
	commandSummary, err := commandSummaryFor(stats, "Complete the synthetic fixture goal")
	if err != nil {
		return err
	}
	if commandSummary.FileName != "goal-attachment.txt" || !strings.HasPrefix(commandSummary.URI, "file:///") || commandSummary.Metadata {
		return errors.New("Goal start did not send only normalized files URI/name")
	}
	h.checks["goal-start-attachment-files-uri-name-and-hydrate"] = true
	_ = startedGoal

	priorTurns := hydrateTurnCount(after)
	if err := h.goal(map[string]any{"type": "pause"}); err != nil {
		return err
	}
	paused, err := h.waitGoalStatus("paused")
	if err != nil {
		return err
	}
	_ = paused
	if err := h.drainQuiet(100 * time.Millisecond); err != nil {
		return err
	}
	afterPause, err := h.hydrate()
	if err != nil {
		return err
	}
	if hydrateTurnCount(afterPause) != priorTurns {
		return errors.New("Goal pause opened a local turn")
	}
	if err := h.assertCommandText("pause", "pause"); err != nil {
		return err
	}
	h.checks["goal-pause-text-no-turn"] = true

	if err := h.goal(map[string]any{"type": "resume"}); err != nil {
		return err
	}
	if _, err := h.waitGoalStatus("active"); err != nil {
		return err
	}
	resumeFinished, err := h.waitTurnFinished()
	if err != nil {
		return err
	}
	if !eventBool(resumeFinished.Payload, "success") {
		return errors.New("Goal resume turn did not finish successfully")
	}
	afterResume, err := h.hydrate()
	if err != nil {
		return err
	}
	if hydrateTurnCount(afterResume) != priorTurns+1 {
		return errors.New("Goal resume did not open exactly one local turn")
	}
	if err := h.assertCommandText("resume", "resume"); err != nil {
		return err
	}
	h.checks["goal-resume-text-opens-turn"] = true

	priorTurns = hydrateTurnCount(afterResume)
	if err := h.goal(map[string]any{"type": "clear"}); err != nil {
		return err
	}
	if _, err := h.waitGoalStatus("null"); err != nil {
		return err
	}
	if err := h.drainQuiet(100 * time.Millisecond); err != nil {
		return err
	}
	afterClear, err := h.hydrate()
	if err != nil {
		return err
	}
	if string(afterClear["goal"]) != "null" || hydrateTurnCount(afterClear) != priorTurns {
		return errors.New("Goal clear did not persist null without opening a turn")
	}
	if err := h.assertCommandText("clear", "clear"); err != nil {
		return err
	}
	h.checks["goal-clear-text-null-no-turn"] = true
	if err := h.goal(map[string]any{"type": "get"}); err != nil {
		return err
	}
	if _, err := h.waitGoalStatusReported(true); err != nil {
		return err
	}
	if err := h.expectNoKind("goalUpdated", 80*time.Millisecond); err != nil {
		return err
	}
	h.checks["native-null-goal-get-is-ok-null"] = true

	statsBefore, err := h.fixtureStats()
	if err != nil {
		return err
	}
	commandsBefore := rawInt(statsBefore["commandCount"])
	for _, action := range []map[string]any{{"type": "telemetry"}, {"type": "start", "objective": "  \t"}, {"type": "start", "objective": "outside file", "attachments": []map[string]string{{"path": filepath.Join(h.home, "outside-workspace.txt"), "name": "outside.txt"}}}} {
		if err := h.goal(action); err == nil {
			return fmt.Errorf("invalid Goal action %q unexpectedly accepted", action["type"])
		}
	}
	statsAfter, err := h.fixtureStats()
	if err != nil {
		return err
	}
	if rawInt(statsAfter["commandCount"]) != commandsBefore {
		return errors.New("invalid Goal action posted provider /command")
	}
	h.checks["unknown-empty-and-outside-file-rejected-before-post"] = true

	for _, status := range []int{400, 422} {
		mode := fmt.Sprintf("reject%d", status)
		if err := h.fixtureConfig(map[string]any{"commandMode": mode}); err != nil {
			return err
		}
		statsBefore, err := h.fixtureStats()
		if err != nil {
			return err
		}
		commandsBefore = rawInt(statsBefore["commandCount"])
		if err := h.goal(map[string]any{"type": "start", "objective": fmt.Sprintf("fixture deterministic rejection %d", status)}); err == nil {
			return fmt.Errorf("HTTP %d Goal command unexpectedly succeeded", status)
		}
		failedTurn, err := h.waitTurnFinished()
		if err != nil {
			return err
		}
		if eventBool(failedTurn.Payload, "success") {
			return fmt.Errorf("deterministic HTTP %d did not fail the local turn", status)
		}
		if _, err := h.waitKind("goalCommandFailed"); err != nil {
			return err
		}
		statsAfter, err = h.fixtureStats()
		if err != nil {
			return err
		}
		if rawInt(statsAfter["commandCount"]) != commandsBefore+1 {
			return fmt.Errorf("deterministic HTTP %d rejection was retried", status)
		}
		failedState, err := h.hydrate()
		if err != nil {
			return err
		}
		if string(failedState["status"]) != `"idle"` {
			return fmt.Errorf("deterministic HTTP %d rejection left local turn active", status)
		}
	}
	h.checks["goal-command-400-422-single-post-fail-turn"] = true
	return nil
}

type commandSummary struct {
	Text, URI, FileName string
	Metadata            bool
}

func commandSummaryFor(stats map[string]json.RawMessage, text string) (commandSummary, error) {
	var calls []fakeCall
	if json.Unmarshal(stats["calls"], &calls) != nil {
		return commandSummary{}, errors.New("cannot decode fixture call trace")
	}
	for _, call := range calls {
		if !strings.HasSuffix(call.Path, "/command") {
			continue
		}
		var body struct {
			Name  string `json:"name"`
			Text  string `json:"text"`
			Files []struct {
				URI  string `json:"uri"`
				Name string `json:"name"`
			} `json:"files"`
		}
		if json.Unmarshal(call.Body, &body) != nil || body.Text != text {
			continue
		}
		var keys map[string]json.RawMessage
		_ = json.Unmarshal(call.Body, &keys)
		out := commandSummary{Text: body.Text}
		for key := range keys {
			if key != "name" && key != "text" && key != "delivery" && key != "files" {
				out.Metadata = true
			}
		}
		if len(body.Files) == 1 {
			out.URI, out.FileName = body.Files[0].URI, body.Files[0].Name
		}
		if body.Name != "goal" {
			out.Metadata = true
		}
		return out, nil
	}
	return commandSummary{}, errors.New("matching Goal command not found in local fixture trace")
}

func (h *harness) assertCommandText(want, action string) error {
	stats, err := h.fixtureStats()
	if err != nil {
		return err
	}
	summary, err := commandSummaryFor(stats, want)
	if err != nil {
		return err
	}
	if summary.Text != action || summary.Metadata {
		return fmt.Errorf("Goal %s command text/body mismatch", action)
	}
	return nil
}

func assertGoalAttachment(session map[string]json.RawMessage, path string) error {
	var messages []map[string]json.RawMessage
	if json.Unmarshal(session["messages"], &messages) != nil {
		return errors.New("cannot decode hydrated messages")
	}
	for _, message := range messages {
		var role, content string
		_ = json.Unmarshal(message["role"], &role)
		_ = json.Unmarshal(message["content"], &content)
		if role != "user" || content != "Complete the synthetic fixture goal" {
			continue
		}
		var attachments []struct {
			Path string `json:"path"`
			Name string `json:"name"`
		}
		if json.Unmarshal(message["attachments"], &attachments) != nil || len(attachments) != 1 || attachments[0].Path != path || attachments[0].Name != "goal-attachment.txt" {
			return errors.New("Goal start user message lost attachment")
		}
		return nil
	}
	return errors.New("Goal start objective was not persisted as a user message")
}

func hydrateTurnCount(session map[string]json.RawMessage) int {
	var turns []json.RawMessage
	_ = json.Unmarshal(session["turns"], &turns)
	return len(turns)
}

func (h *harness) waitHydratedGoalStatus(status string, timeout time.Duration) (map[string]json.RawMessage, error) {
	ctx, cancel := context.WithTimeout(h.ctx, timeout)
	defer cancel()
	ticker := time.NewTicker(50 * time.Millisecond)
	defer ticker.Stop()
	for {
		session, err := h.hydrate()
		if err == nil {
			if got, ok := eventString(session["goal"], "status"); ok && got == status {
				return session, nil
			}
		}
		select {
		case <-ctx.Done():
			return nil, fmt.Errorf("timed out waiting for durable status %s", status)
		case <-ticker.C:
		}
	}
}

func (h *harness) waitHydratedGoalObjective(objective string, timeout time.Duration) (map[string]json.RawMessage, error) {
	ctx, cancel := context.WithTimeout(h.ctx, timeout)
	defer cancel()
	ticker := time.NewTicker(50 * time.Millisecond)
	defer ticker.Stop()
	for {
		session, err := h.hydrate()
		if err == nil {
			if got, ok := eventString(session["goal"], "objective"); ok && got == objective {
				return session, nil
			}
		}
		select {
		case <-ctx.Done():
			return nil, fmt.Errorf("timed out waiting for durable objective %q", objective)
		case <-ticker.C:
		}
	}
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
		if exited, _ := child.Exited(0); exited {
			return "", errors.New("owned daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-tick.C:
		}
	}
}

func (h *harness) testRetryIdleAndChildScope() error {
	if err := h.fixtureConfig(map[string]any{"holdNextCommand": true}); err != nil {
		return err
	}
	if err := h.goal(map[string]any{"type": "start", "objective": "hold synthetic retry turn"}); err != nil {
		return err
	}
	if _, err := h.waitGoalStatus("active"); err != nil {
		return err
	}
	if err := h.expectProviderRetry("session.retry.scheduled", map[string]any{
		"sessionID": fakeNativeID, "attempt": 3, "at": int64(1700000000456),
		"error": map[string]any{"type": "synthetic", "message": "synthetic modern retry"},
	}, 3, "synthetic modern retry", "", int64(1700000000456)); err != nil {
		return err
	}
	if err := h.expectProviderRetry("session.status", map[string]any{
		"sessionID": fakeNativeID, "status": map[string]any{"type": "retry", "attempt": 4, "message": "synthetic legacy retry", "action": map[string]any{"reason": "rate_limit", "provider": "fixture", "title": "Synthetic backoff"}, "next": int64(1700000000789)},
	}, 4, "synthetic legacy retry", "fixture", int64(1700000000789)); err != nil {
		return err
	}
	if err := h.fixtureEmit("session.status", map[string]any{"sessionID": fakeNativeID, "status": map[string]any{"type": "busy"}}); err != nil {
		return err
	}
	if _, err := h.waitKind("providerBusy"); err != nil {
		return err
	}
	if err := h.expectNoKind("turnFinished", 100*time.Millisecond); err != nil {
		return err
	}
	h.checks["modern-legacy-retry-busy-normalized"] = true

	if err := h.fixtureConfig(map[string]any{"messageStatus": 503}); err != nil {
		return err
	}
	if err := h.fixtureEmit("session.idle", map[string]any{"sessionID": fakeNativeID}); err != nil {
		return err
	}
	if err := h.expectNoKind("turnFinished", 180*time.Millisecond); err != nil {
		return err
	}
	stillWorking, err := h.hydrate()
	if err != nil {
		return err
	}
	if string(stillWorking["status"]) != `"working"` {
		return errors.New("idle message GET failure settled the active Goal turn")
	}

	if err := h.fixtureConfig(map[string]any{
		"messageStatus": 0,
		"messages": []map[string]any{
			{"id": "newest-assistant", "type": "assistant", "error": map[string]any{"name": "SyntheticError", "data": map[string]any{"message": "synthetic assistant error"}}},
			{"id": "older-assistant", "type": "assistant", "error": nil},
			{"id": "newest-user", "type": "user"},
		},
	}); err != nil {
		return err
	}
	if err := h.fixtureEmit("session.idle", map[string]any{"sessionID": fakeNativeID}); err != nil {
		return err
	}
	failed, err := h.waitTurnFinished()
	if err != nil {
		return err
	}
	if eventBool(failed.Payload, "success") {
		return errors.New("idle settlement ignored newest assistant error")
	}
	if !strings.Contains(string(failed.Payload), "synthetic assistant error") {
		return errors.New("idle failure summary did not use newest assistant error")
	}
	if err := h.fixtureEmit("session.idle", map[string]any{"sessionID": fakeNativeID}); err != nil {
		return err
	}
	if err := h.fixtureEmit("session.execution.failed", map[string]any{"sessionID": fakeNativeID}); err != nil {
		return err
	}
	if err := h.expectNoKind("turnFinished", 180*time.Millisecond); err != nil {
		return err
	}
	h.checks["idle-get-failure-keeps-working-newest-assistant-error-and-dedup"] = true

	if err := h.fixtureConfig(map[string]any{"holdNextCommand": true}); err != nil {
		return err
	}
	if err := h.goal(map[string]any{"type": "resume"}); err != nil {
		return err
	}
	if _, err := h.waitGoalStatus("active"); err != nil {
		return err
	}
	if err := h.fixtureEmit("session.idle", map[string]any{"sessionID": "ses_synthetic_child_scope"}); err != nil {
		return err
	}
	if err := h.expectNoKind("turnFinished", 120*time.Millisecond); err != nil {
		return err
	}
	stillWorking, err = h.hydrate()
	if err != nil {
		return err
	}
	if string(stillWorking["status"]) != `"working"` {
		return errors.New("child-scoped idle settled the root turn")
	}
	// Let the held root turn settle before the ambiguous command case.
	if err := h.fixtureConfig(map[string]any{"messageStatus": 0, "messages": []map[string]any{{"id": "assistant-ok", "type": "assistant"}}}); err != nil {
		return err
	}
	if err := h.fixtureEmit("session.idle", map[string]any{"sessionID": fakeNativeID}); err != nil {
		return err
	}
	if _, err := h.waitTurnFinished(); err != nil {
		return err
	}
	h.checks["child-scoped-idle-does-not-settle-root"] = true
	return nil
}

func (h *harness) expectProviderRetry(kind string, data map[string]any, attempt int64, message string, actionProvider string, nextAt int64) error {
	if err := h.fixtureEmit(kind, data); err != nil {
		return err
	}
	ev, err := h.waitKind("providerRetry")
	if err != nil {
		return err
	}
	if eventInt(ev.Payload, "attempt") != attempt {
		return fmt.Errorf("%s retry attempt mismatch", kind)
	}
	if got, ok := eventString(ev.Payload, "message"); !ok || got != message {
		return fmt.Errorf("%s retry message mismatch", kind)
	}
	if eventInt(ev.Payload, "nextAtMs") != nextAt {
		return fmt.Errorf("%s retry nextAtMs mismatch", kind)
	}
	if bytes.Contains(ev.Payload, []byte("synthetic-private-payload")) {
		return errors.New("retry event leaked private provider payload")
	}
	if actionProvider == "" {
		if raw, ok := eventRawField(ev.Payload, "action"); ok && string(raw) != "null" {
			return errors.New("modern retry unexpectedly exposed a legacy action")
		}
	} else if got, ok := eventStringFor(ev.Payload, "action", "provider"); !ok || got != actionProvider {
		return errors.New("legacy retry action mapping mismatch")
	}
	return nil
}

func eventRawField(raw json.RawMessage, key string) (json.RawMessage, bool) {
	var value map[string]json.RawMessage
	if json.Unmarshal(raw, &value) != nil {
		return nil, false
	}
	field, ok := value[key]
	return field, ok
}

func eventStringFor(raw json.RawMessage, parent, key string) (string, bool) {
	field, ok := eventRawField(raw, parent)
	if !ok {
		return "", false
	}
	return eventString(field, key)
}

func (h *harness) testAmbiguousAndRestartHydrate() error {
	if err := h.fixtureConfig(map[string]any{"commandMode": "drop"}); err != nil {
		return err
	}
	statsBefore, err := h.fixtureStats()
	if err != nil {
		return err
	}
	commandsBefore := rawInt(statsBefore["commandCount"])
	err = h.goal(map[string]any{"type": "start", "objective": "ambiguous synthetic command"})
	if err == nil {
		return errors.New("dropped Goal response unexpectedly confirmed")
	}
	failed, err := h.waitKind("goalCommandFailed")
	if err != nil {
		return err
	}
	if strings.Contains(string(failed.Payload), "synthetic-private-payload") {
		return errors.New("ambiguous failure leaked provider details")
	}
	if err := h.expectNoKind("turnFinished", 160*time.Millisecond); err != nil {
		return err
	}
	state, err := h.hydrate()
	if err != nil {
		return err
	}
	if string(state["status"]) != `"working"` {
		return errors.New("ambiguous Goal command fabricated a terminal local turn")
	}
	statsAfter, err := h.fixtureStats()
	if err != nil {
		return err
	}
	if rawInt(statsAfter["commandCount"]) != commandsBefore+1 {
		return errors.New("ambiguous Goal command was automatically retried")
	}
	h.checks["ambiguous-goal-single-post-no-fabricated-terminal"] = true

	// The interrupted turn and Goal snapshot must survive daemon process restart.
	before := cloneRawMap(state)
	if err := h.restartDaemon(); err != nil {
		return err
	}
	after, err := h.hydrate()
	if err != nil {
		return err
	}
	for _, key := range []string{"goal", "messages", "turns", "transcript_blocks", "status", "native_session_id", "provider_cursor"} {
		if !bytes.Equal(bytes.TrimSpace(before[key]), bytes.TrimSpace(after[key])) {
			return fmt.Errorf("SQLite hydrate changed %s after daemon restart", key)
		}
	}
	if string(after["status"]) != `"working"` {
		return errors.New("ambiguous turn did not remain working after restart hydrate")
	}
	if err := h.closeRuntime(); err != nil {
		return err
	}
	h.checks["goal-messages-transcript-and-turns-hydrate-after-restart"] = true
	return nil
}

func (h *harness) testNativePollAndCancel() error {
	// The actor should observe a native Goal change on its asynchronous poll,
	// without a user-issued get or another /command POST.
	statsBefore, err := h.fixtureStats()
	if err != nil {
		return err
	}
	commandsBefore := rawInt(statsBefore["commandCount"])
	const nextRevision int64 = 1800000000123
	pollGoal, _ := json.Marshal(map[string]any{"objective": "synthetic polled objective", "status": "active", "reason": "fixture poll", "rounds": 11, "maxRounds": 13, "updatedAt": nextRevision, "privateFixtureField": "synthetic-private-payload"})
	if err := h.fixtureConfig(map[string]any{"goal": json.RawMessage(pollGoal)}); err != nil {
		return err
	}
	objectiveCtx, objectiveCancel := context.WithTimeout(h.ctx, 8*time.Second)
	defer objectiveCancel()
	for {
		ev, nextErr := h.nextEvent(objectiveCtx)
		if errors.Is(nextErr, context.DeadlineExceeded) {
			return errors.New("asynchronous Goal poll did not publish changed native snapshot")
		}
		if nextErr != nil {
			return nextErr
		}
		if ev.Kind != "goalUpdated" {
			continue
		}
		objective, ok := eventString(ev.Payload, "objective")
		if !ok || objective != "synthetic polled objective" {
			continue
		}
		if eventInt(ev.Payload, "revision") != nextRevision || eventInt(ev.Payload, "turns") != 11 || eventInt(ev.Payload, "maxTurns") != 13 {
			return errors.New("asynchronous Goal poll mapped native fields incorrectly")
		}
		break
	}
	statsAfter, err := h.fixtureStats()
	if err != nil {
		return err
	}
	if rawInt(statsAfter["commandCount"]) != commandsBefore {
		return errors.New("native Goal poll unexpectedly posted /command")
	}
	h.checks["async-native-goal-poll-publishes-change-no-command"] = true

	// Invalid required native fields are rejected. They cannot replace the
	// previously saved Goal or become a zero-valued snapshot.
	badSnapshots := []map[string]any{
		{"objective": "bad rounds", "status": "active", "maxRounds": 3, "updatedAt": 1800000000222},
		{"objective": "null rounds", "status": "active", "rounds": nil, "maxRounds": 3, "updatedAt": 1800000000222},
		{"objective": "bad max rounds", "status": "active", "rounds": 1, "updatedAt": 1800000000222},
		{"objective": "null max rounds", "status": "active", "rounds": 1, "maxRounds": nil, "updatedAt": 1800000000222},
		{"objective": "rounds overflow", "status": "active", "rounds": int64(4294967296), "maxRounds": 3, "updatedAt": 1800000000222},
		{"objective": "max rounds overflow", "status": "active", "rounds": 1, "maxRounds": int64(4294967296), "updatedAt": 1800000000222},
		{"objective": 7, "status": "active", "rounds": 1, "maxRounds": 3, "updatedAt": 1800000000222},
		{"status": "active", "rounds": 1, "maxRounds": 3, "updatedAt": 1800000000222},
		{"objective": nil, "status": "active", "rounds": 1, "maxRounds": 3, "updatedAt": 1800000000222},
		{"objective": "negative revision", "status": "active", "rounds": 1, "maxRounds": 3, "updatedAt": -1},
		{"objective": "missing revision", "status": "active", "rounds": 1, "maxRounds": 3},
		{"objective": "null revision", "status": "active", "rounds": 1, "maxRounds": 3, "updatedAt": nil},
	}
	var malformedFailures []error
	for index, bad := range badSnapshots {
		baseline, _ := json.Marshal(map[string]any{"objective": "baseline for malformed snapshot", "status": "active", "rounds": 2, "maxRounds": 5, "updatedAt": int64(1800000000200 + index)})
		if err := h.fixtureConfig(map[string]any{"goal": json.RawMessage(baseline)}); err != nil {
			malformedFailures = append(malformedFailures, err)
			continue
		}
		if err := h.goal(map[string]any{"type": "get"}); err != nil {
			malformedFailures = append(malformedFailures, fmt.Errorf("baseline Goal read: %w", err))
			continue
		}
		if _, err := h.waitGoalStatusReported(true); err != nil {
			malformedFailures = append(malformedFailures, fmt.Errorf("baseline Goal result: %w", err))
			continue
		}
		if _, err := h.waitHydratedGoalObjective("baseline for malformed snapshot", 2*time.Second); err != nil {
			malformedFailures = append(malformedFailures, fmt.Errorf("baseline Goal persistence: %w", err))
			continue
		}
		badRaw, _ := json.Marshal(bad)
		if err := h.fixtureConfig(map[string]any{"goal": json.RawMessage(badRaw)}); err != nil {
			malformedFailures = append(malformedFailures, err)
			continue
		}
		getErr := h.goal(map[string]any{"type": "get"})
		if getErr == nil {
			if _, reportErr := h.waitGoalStatusReported(false); reportErr != nil {
				malformedFailures = append(malformedFailures, fmt.Errorf("invalid native Goal accepted (%v): %w", bad, reportErr))
			} else {
				malformedFailures = append(malformedFailures, fmt.Errorf("invalid native Goal snapshot accepted: %v", bad))
			}
		} else if _, reportErr := h.waitGoalStatusReported(false); reportErr != nil {
			malformedFailures = append(malformedFailures, fmt.Errorf("invalid native Goal result (%v): %w", bad, reportErr))
		}
		persisted, err := h.hydrate()
		if err != nil {
			malformedFailures = append(malformedFailures, err)
			continue
		}
		if !bytes.Contains(persisted["goal"], []byte("baseline for malformed snapshot")) || bytes.Contains(persisted["goal"], []byte("bad rounds")) || bytes.Contains(persisted["goal"], []byte("negative revision")) {
			malformedFailures = append(malformedFailures, fmt.Errorf("invalid native Goal snapshot replaced saved Goal: %s", persisted["goal"]))
		}
	}
	if len(malformedFailures) == 0 {
		h.checks["malformed-required-native-goal-fields-rejected"] = true
	}

	// Return a valid active Goal, start one held local Goal turn, then cancel.
	validRaw, _ := json.Marshal(map[string]any{"objective": "synthetic cancel objective", "status": "active", "rounds": 1, "maxRounds": 6, "updatedAt": 1800000000333})
	if err := h.fixtureConfig(map[string]any{"goal": json.RawMessage(validRaw), "holdNextCommand": true}); err != nil {
		return err
	}
	if err := h.goal(map[string]any{"type": "resume"}); err != nil {
		return err
	}
	if _, err := h.waitGoalStatus("active"); err != nil {
		return err
	}
	workingBeforeCancel, err := h.hydrate()
	if err != nil {
		return err
	}
	if string(workingBeforeCancel["status"]) != `"working"` {
		return errors.New("resume did not leave an active turn before cancel")
	}
	statsBeforeCancel, err := h.fixtureStats()
	if err != nil {
		return err
	}
	cancelCommandCount := rawInt(statsBeforeCancel["commandCount"])
	interruptBefore := countFixturePath(statsBeforeCancel["calls"], "/interrupt")
	if _, err := h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "cancel"}); err != nil {
		return fmt.Errorf("cancel active Goal: %w", err)
	}
	if _, err := h.waitGoalStatus("paused"); err != nil {
		return err
	}
	finished, err := h.waitTurnFinished()
	if err != nil {
		return err
	}
	if eventBool(finished.Payload, "success") {
		return errors.New("cancel of active Goal did not interrupt the local turn")
	}
	if err := h.assertCommandText("pause", "pause"); err != nil {
		return fmt.Errorf("cancel did not pause native Goal before interrupt: %w", err)
	}
	statsAfterCancel, err := h.fixtureStats()
	if err != nil {
		return err
	}
	if rawInt(statsAfterCancel["commandCount"]) != cancelCommandCount+1 || countFixturePath(statsAfterCancel["calls"], "/interrupt") != interruptBefore+1 {
		return errors.New("cancel did not send exactly one pause command followed by one interrupt")
	}
	if err := h.drainQuiet(150 * time.Millisecond); err != nil {
		return err
	}
	state, err := h.hydrate()
	if err != nil {
		return err
	}
	if string(state["status"]) != `"idle"` || !bytes.Contains(state["goal"], []byte(`"status":"paused"`)) {
		return errors.New("cancel failed to persist paused Goal and idle turn")
	}
	if hydrateArrayLen(state["messages"]) == 0 || hydrateArrayLen(state["turns"]) == 0 || hydrateArrayLen(state["transcript_blocks"]) == 0 {
		return errors.New("fixture conversation messages, turns, or transcript blocks were not persisted")
	}
	h.checks["cancel-pauses-active-goal-before-interrupt"] = true
	if len(malformedFailures) > 0 {
		return errors.Join(malformedFailures...)
	}
	return nil
}

func countFixturePath(raw json.RawMessage, suffix string) int {
	var calls []fakeCall
	_ = json.Unmarshal(raw, &calls)
	count := 0
	for _, call := range calls {
		if strings.HasSuffix(call.Path, suffix) {
			count++
		}
	}
	return count
}

func hydrateArrayLen(raw json.RawMessage) int {
	var values []json.RawMessage
	_ = json.Unmarshal(raw, &values)
	return len(values)
}

func cloneRawMap(in map[string]json.RawMessage) map[string]json.RawMessage {
	out := make(map[string]json.RawMessage, len(in))
	for key, value := range in {
		out[key] = append(json.RawMessage(nil), value...)
	}
	return out
}

func (h *harness) restartDaemon() error {
	if h.client != nil {
		h.client.Close()
		h.client = nil
	}
	if h.owned != nil {
		if err := h.owned.Close(); err != nil {
			return fmt.Errorf("close owned daemon job: %w", err)
		}
		h.owned = nil
	}
	newRuntime := uuid.NewString()
	h.runtimeID = newRuntime
	if err := h.launchDaemon(); err != nil {
		return err
	}
	options := map[string]any{"binary": h.self, "cwd": h.workspace, "mode": "ask", "interactionMode": "code", "model": "fixture/goalcheck", "providerCursor": map[string]string{"provider": "openCode", "sessionId": fakeNativeID}}
	if _, err := h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "start", "options": options}); err != nil {
		return fmt.Errorf("restart runtime from saved cursor: %w", err)
	}
	if _, err := h.waitKind("connected"); err != nil {
		return fmt.Errorf("wait restarted runtime: %w", err)
	}
	return nil
}

func (h *harness) closeRuntime() error {
	if h.client != nil {
		_, err := h.client.RequestFor(h.ctx, h.sessionID, h.runtimeID, map[string]any{"type": "closeSession"})
		if err != nil {
			return fmt.Errorf("close isolated runtime: %w", err)
		}
	}
	return nil
}

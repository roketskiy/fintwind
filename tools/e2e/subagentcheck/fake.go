package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

const rootNative = "ses_root_fixture"
const childOne = "ses_child_one"
const childTwo = "ses_child_two"
const childUnknownEmpty = "ses_child_unknown_empty"
const childUnknownOld = "ses_child_unknown_old"
const childRace = "ses_child_race"
const childRevision = "ses_child_revision"

type fixture struct {
	mu             sync.Mutex
	home, password string
	streams        map[chan []byte]bool
	calls          []map[string]string
	childMessages  map[string][]map[string]any
	rootMessages   []map[string]any
	activeCalls    int
	diagMu         sync.Mutex
}

// diag appends a diagnostic timeline entry. Diagnostics only: it never changes
// responses, holds, or releases.
func (f *fixture) diag(event string, fields map[string]any) {
	f.diagMu.Lock()
	defer f.diagMu.Unlock()
	entry := map[string]any{"t": time.Now().Format(time.RFC3339Nano), "event": event}
	for k, v := range fields {
		entry[k] = v
	}
	raw, _ := json.Marshal(entry)
	path := filepath.Join(f.home, "roster-diag.jsonl")
	file, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		return
	}
	_, _ = file.Write(append(raw, '\n'))
	_ = file.Close()
}

func rosterRowSummary(rows []any) []map[string]any {
	out := make([]map[string]any, 0, len(rows))
	for _, row := range rows {
		if m, ok := row.(map[string]any); ok {
			out = append(out, map[string]any{"id": m["id"], "parentID": m["parentID"], "outcome": m["outcome"], "status": m["status"]})
		}
	}
	return out
}

func serveFixture() error {
	home, password := os.Getenv("OPENCODE_TEST_HOME"), os.Getenv("OPENCODE_SERVER_PASSWORD")
	if !filepath.IsAbs(home) || password == "" {
		return errors.New("isolated engine profile missing")
	}
	for _, key := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY"} {
		if os.Getenv(key) != "" {
			return errors.New("provider credential leaked into fake CLI environment")
		}
	}
	if err := os.MkdirAll(home, 0700); err != nil {
		return err
	}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	f := &fixture{home: home, password: password, streams: map[chan []byte]bool{}, childMessages: map[string][]map[string]any{}}
	if raw, readErr := os.ReadFile(filepath.Join(home, "root-transcript.json")); readErr == nil {
		_ = json.Unmarshal(raw, &f.rootMessages)
	}
	s := &http.Server{ReadHeaderTimeout: 5 * time.Second, Handler: f.handler()}
	defer s.Close()
	fmt.Printf("OpenCode server listening on http://%s\n", ln.Addr())
	return s.Serve(ln)
}
func (f *fixture) handler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		user, pw, ok := r.BasicAuth()
		if !ok || user != "opencode" || pw != f.password || r.Header.Get("x-opencode-directory") == "" {
			http.Error(w, "unauthorized", 401)
			return
		}
		f.record(r)
		switch {
		case r.Method == "GET" && r.URL.Path == "/api/info":
			writeJSON(w, map[string]any{"data": map[string]any{}})
		case r.Method == "POST" && r.URL.Path == "/api/session":
			writeJSON(w, map[string]any{"data": map[string]string{"id": rootNative}})
		case r.Method == "GET" && r.URL.Path == "/api/model":
			writeJSON(w, map[string]any{"data": []any{}})
		case r.Method == "GET" && r.URL.Path == "/api/event":
			f.serveSSE(w, r)
		case r.Method == "GET" && strings.HasPrefix(r.URL.Path, "/api/session/") && strings.HasSuffix(r.URL.Path, "/message"):
			f.transcript(w, r)
		case r.Method == "GET" && r.URL.Path == "/api/session":
			mode := f.mode()
			f.diag("roster-request", map[string]any{"mode": mode, "cursor": r.URL.Query().Get("cursor"), "parentID": r.URL.Query().Get("parentID")})
			if mode == "budget-roster" {
				rows := make([]any, 0, 76)
				long := strings.Repeat("\x01", 20000)
				for i := 0; i < 74; i++ {
					rows = append(rows, map[string]any{
						"id": fmt.Sprintf("ses_budget_%03d", i), "parentID": rootNative,
						"title": long, "agent": long, "model": long, "prompt": long,
						"outcome": "succeeded",
					})
				}
				rows = append(rows,
					map[string]any{"id": "ses_budget_short", "parentID": rootNative, "title": "短 metadata", "agent": "reviewer", "model": "fixture/model-v1", "prompt": "保留这个 prompt", "outcome": "failed"},
					map[string]any{"id": childOne, "parentID": rootNative, "title": long, "agent": long, "model": long, "prompt": long, "outcome": "interrupted"},
				)
				writeJSON(w, map[string]any{"data": rows, "cursor": map[string]any{}})
				return
			}
			if r.URL.Query().Get("parentID") != "" && r.URL.Query().Get("cursor") == "" {
				f.diag("roster-response", map[string]any{"branch": "parentID-400"})
				http.Error(w, "parentID filter unsupported", 400)
				return
			}
			if mode := f.mode(); mode == "complete-empty" {
				f.diag("roster-response", map[string]any{"branch": "complete-empty"})
				writeJSON(w, map[string]any{"data": []any{}, "cursor": map[string]any{}})
				return
			}
			if mode == "unknown-empty" || mode == "unknown-old" {
				id := childUnknownEmpty
				if mode == "unknown-old" {
					id = childUnknownOld
				}
				rows := []any{
					map[string]any{"id": childOne, "parentID": rootNative, "title": "Child one"},
					map[string]any{"id": childTwo, "parentID": rootNative, "title": "Active child", "outcome": "interrupted"},
					map[string]any{"id": id, "parentID": rootNative, "title": "Historical child"},
				}
				writeJSON(w, map[string]any{"data": rows, "cursor": map[string]any{}})
				return
			}
			heldOne, heldRevision := false, false
			if mode == "roster-slow-one" {
				if r.URL.Query().Get("cursor") == "" {
					heldOne = true
					_ = os.WriteFile(filepath.Join(f.home, "roster-one-held"), []byte("held"), 0600)
					f.diag("roster-one-held", map[string]any{"mode": mode})
					if err := f.waitRelease("release-roster-one"); err != nil {
						http.Error(w, "fixture hold timed out", 500)
						return
					}
					_ = os.WriteFile(filepath.Join(f.home, "roster-one-finished"), []byte("done"), 0600)
					f.diag("roster-one-released", map[string]any{"modeAtRelease": f.mode()})
				}
			}
			if mode == "roster-revision-held" && r.URL.Query().Get("cursor") == "" {
				heldRevision = true
				_ = os.WriteFile(filepath.Join(f.home, "revision-roster-held"), []byte("held"), 0600)
				f.diag("revision-roster-held", map[string]any{"mode": mode})
				if err := f.waitRelease("release-revision-roster"); err != nil {
					http.Error(w, "fixture revision hold timed out", 500)
					return
				}
				_ = os.WriteFile(filepath.Join(f.home, "revision-roster-finished"), []byte("done"), 0600)
				f.diag("revision-roster-released", map[string]any{"modeAtRelease": f.mode()})
			}
			if mode == "cursor-cycle" {
				rows := []any{map[string]any{"id": childOne, "parentID": rootNative, "title": "Child one"}}
				f.diag("roster-response", map[string]any{"branch": "cursor-cycle"})
				writeJSON(w, map[string]any{"data": rows, "cursor": map[string]any{"next": "cycle"}})
				return
			}
			if mode == "fullpage-no-cursor" {
				rows := make([]any, 200)
				for i := range rows {
					rows[i] = map[string]any{"id": fmt.Sprintf("ses_fullpage_%03d", i), "parentID": "ses_other_root", "title": "unrelated"}
				}
				f.diag("roster-response", map[string]any{"branch": "fullpage-no-cursor"})
				writeJSON(w, map[string]any{"data": rows})
				return
			}
			if mode == "roster-second" {
				rows := []any{
					map[string]any{"id": childOne, "parentID": rootNative, "title": "Child one", "outcome": "succeeded"},
					map[string]any{"id": childTwo, "parentID": rootNative, "title": "Active child", "outcome": "interrupted"},
					map[string]any{"id": childRace, "parentID": rootNative, "title": "Race child gen2", "outcome": "succeeded"},
				}
				f.diag("roster-response", map[string]any{"branch": "roster-second", "rows": rosterRowSummary(rows)})
				writeJSON(w, map[string]any{"data": rows, "cursor": map[string]any{}})
				return
			}
			if r.URL.Query().Get("cursor") == "page-2" {
				rows := []any{map[string]any{"id": childTwo, "parentID": "ses_other_root", "title": "unrelated"}}
				f.diag("roster-response", map[string]any{"branch": "page-2", "mode": f.mode(), "rows": rosterRowSummary(rows)})
				writeJSON(w, map[string]any{"data": rows, "cursor": map[string]any{}})
				return
			}
			rows := make([]any, 0, 200)
			for i := 0; i < 199; i++ {
				rows = append(rows, map[string]any{"id": fmt.Sprintf("ses_unrelated_%03d", i), "parentID": "ses_other_root", "title": "unrelated"})
			}
			rows = append(rows, map[string]any{"id": childOne, "parentID": rootNative, "title": "Child one", "outcome": "succeeded"})
			payload := map[string]any{"data": rows, "cursor": map[string]any{"next": "page-2"}}
			f.diag("roster-response", map[string]any{"branch": "default-page-1", "capturedMode": mode, "rowCount": len(rows), "childOne": rosterRowSummary(rows[len(rows)-1:])})
			writeJSON(w, payload)
			if heldOne {
				_ = os.WriteFile(filepath.Join(f.home, "roster-one-response-written"), []byte("done"), 0600)
				f.diag("roster-one-response-written", map[string]any{"branch": "default-page-1"})
			}
			if heldRevision {
				_ = os.WriteFile(filepath.Join(f.home, "revision-roster-response-written"), []byte("done"), 0600)
				f.diag("revision-roster-response-written", map[string]any{"branch": "default-page-1"})
			}
		case r.Method == "GET" && r.URL.Path == "/api/session/active":
			f.mu.Lock()
			f.activeCalls++
			activeCall := f.activeCalls
			f.mu.Unlock()
			if f.mode() == "malformed-active" || (f.mode() == "default" && activeCall > 1) {
				f.diag("active-response", map[string]any{"mode": f.mode(), "activeCall": activeCall, "shape": "array-malformed"})
				writeJSON(w, map[string]any{"data": []any{}})
			} else if f.mode() == "unknown-empty" {
				f.diag("active-response", map[string]any{"mode": f.mode(), "activeCall": activeCall, "busy": childUnknownEmpty})
				writeJSON(w, map[string]any{"data": map[string]any{childUnknownEmpty: map[string]any{"status": "busy"}}})
			} else if f.mode() == "unknown-old" {
				f.diag("active-response", map[string]any{"mode": f.mode(), "activeCall": activeCall, "busy": childUnknownOld})
				writeJSON(w, map[string]any{"data": map[string]any{childUnknownOld: map[string]any{"status": "busy"}}})
			} else if f.mode() == "roster-second" {
				f.diag("active-response", map[string]any{"mode": f.mode(), "activeCall": activeCall, "busy": childTwo})
				writeJSON(w, map[string]any{"data": map[string]any{childTwo: map[string]any{"status": "busy"}}})
			} else {
				f.diag("active-response", map[string]any{"mode": f.mode(), "activeCall": activeCall, "busy": nil})
				writeJSON(w, map[string]any{"data": map[string]any{}})
			}
		case r.Method == "POST" && strings.HasSuffix(r.URL.Path, "/prompt"):
			f.prompt(w, r)
		case r.Method == "POST" && strings.HasSuffix(r.URL.Path, "/interrupt"):
			writeJSON(w, map[string]any{"data": map[string]any{}})
		case r.Method == "POST" && strings.HasSuffix(r.URL.Path, "/fork"):
			writeJSON(w, map[string]any{"data": map[string]string{"id": "ses_fork_fixture"}})
		case r.Method == "GET" && strings.HasPrefix(r.URL.Path, "/api/session/"):
			writeJSON(w, map[string]any{"data": map[string]string{"id": strings.TrimPrefix(r.URL.Path, "/api/session/")}})
		default:
			writeJSON(w, map[string]any{"data": map[string]any{}})
		}
	})
}
func (f *fixture) mode() string {
	raw, err := os.ReadFile(filepath.Join(f.home, "fixture-mode"))
	if err != nil {
		return "default"
	}
	return strings.TrimSpace(string(raw))
}
func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}
func (f *fixture) record(r *http.Request) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, map[string]string{"method": r.Method, "path": r.URL.RequestURI()})
	raw, _ := json.Marshal(f.calls)
	tmp := filepath.Join(f.home, "subagent-audit.tmp")
	_ = os.WriteFile(tmp, raw, 0600)
	_ = os.Rename(tmp, filepath.Join(f.home, "subagent-audit.json"))
}
func (f *fixture) serveSSE(w http.ResponseWriter, r *http.Request) {
	ch := make(chan []byte, 512)
	f.mu.Lock()
	f.streams[ch] = true
	f.mu.Unlock()
	defer func() { f.mu.Lock(); delete(f.streams, ch); f.mu.Unlock() }()
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.WriteHeader(200)
	w.(http.Flusher).Flush()
	for {
		select {
		case <-r.Context().Done():
			return
		case b := <-ch:
			_, _ = fmt.Fprintf(w, "data: %s\n\n", b)
			w.(http.Flusher).Flush()
		}
	}
}
func (f *fixture) emit(kind string, data map[string]any) {
	raw, _ := json.Marshal(map[string]any{"type": kind, "data": data})
	f.mu.Lock()
	defer f.mu.Unlock()
	for ch := range f.streams {
		select {
		case ch <- raw:
		default:
		}
	}
}
func (f *fixture) prompt(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Text string `json:"text"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	f.appendRootTranscript(body.Text)
	switch body.Text {
	case "subagent lifecycle":
		go f.firstTurn()
	case "continue child":
		go f.continueTurn()
	case "active child control":
		go f.activeTurn()
	case "unknown empty child":
		f.mu.Lock()
		f.childMessages[childUnknownEmpty] = []map[string]any{}
		f.mu.Unlock()
		go f.unknownTurn(childUnknownEmpty, "perm_early_empty", "frm_early_empty", "release-unknown-empty", true)
	case "unknown old child":
		f.mu.Lock()
		f.childMessages[childUnknownOld] = oldTranscript(childUnknownOld)
		f.mu.Unlock()
		go f.unknownTurn(childUnknownOld, "perm_early_old", "frm_early_old", "release-unknown-old", false)
	case "slow roster race":
		go f.slowRosterTurn()
	case "revision roster race":
		go f.revisionRosterTurn()
	case "capacity boundary":
		go f.capacityTurn()
	default:
		go f.emit("session.execution.succeeded", map[string]any{"sessionID": rootNative})
	}
	writeJSON(w, map[string]any{"data": map[string]any{}})
}
func (f *fixture) firstTurn() {
	f.emit("session.execution.started", map[string]any{"sessionID": rootNative})
	f.emit("session.tool.input.started", map[string]any{"sessionID": rootNative, "id": "call_task_one", "name": "task"})
	f.emit("session.tool.called", map[string]any{"sessionID": rootNative, "id": "call_task_one", "input": map[string]any{"prompt": "inspect child", "task_id": childOne}})
	f.emit("session.tool.input.started", map[string]any{"sessionID": rootNative, "id": "call_task_exact", "name": "task"})
	f.emit("session.tool.called", map[string]any{"sessionID": rootNative, "id": "call_task_exact", "input": map[string]any{"message": "exact target child", "sessionID": childOne}})
	f.emit("session.created", map[string]any{"id": childTwo, "parentID": "ses_unrelated", "title": "unrelated child"})
	f.createChild(childOne, "first child task")
	f.emit("session.tool.success", map[string]any{"sessionID": rootNative, "id": "call_task_one", "content": "child created"})
	f.emit("session.tool.success", map[string]any{"sessionID": rootNative, "id": "call_task_exact", "content": "child created"})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": rootNative})
}
func (f *fixture) createChild(id, prompt string) {
	f.emit("session.created", map[string]any{"id": id, "parentID": rootNative, "title": "Subagent alpha"})
	f.emit("session.execution.started", map[string]any{"sessionID": id})
	f.emit("session.text.started", map[string]any{"sessionID": id, "assistantMessageID": "a1", "ordinal": 0})
	f.emit("session.text.delta", map[string]any{"sessionID": id, "assistantMessageID": "a1", "ordinal": 0, "delta": "child "})
	f.emit("session.reasoning.started", map[string]any{"sessionID": id, "assistantMessageID": "a1", "ordinal": 1})
	f.emit("session.reasoning.delta", map[string]any{"sessionID": id, "assistantMessageID": "a1", "ordinal": 1, "delta": "thinking"})
	f.emit("session.tool.input.started", map[string]any{"sessionID": id, "id": "child_read", "name": "read"})
	f.emit("session.tool.called", map[string]any{"sessionID": id, "id": "child_read", "input": map[string]any{"filePath": "/tmp/fixture.txt"}})
	f.emit("session.tool.success", map[string]any{"sessionID": id, "id": "child_read", "content": "fixture data"})
	f.emit("session.text.delta", map[string]any{"sessionID": id, "assistantMessageID": "a1", "ordinal": 0, "delta": "done"})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": id})
	f.mu.Lock()
	f.childMessages[id] = []map[string]any{{"id": "m_user_" + id, "type": "user", "time": map[string]any{"created": time.Now().UnixMilli()}, "content": []any{map[string]any{"type": "text", "text": prompt}}}, {"id": "m_assistant_" + id, "type": "assistant", "time": map[string]any{"created": time.Now().UnixMilli(), "completed": time.Now().UnixMilli() + 1}, "content": []any{map[string]any{"type": "text", "text": "child done"}, map[string]any{"type": "reasoning", "text": "thinking"}, map[string]any{"type": "tool", "id": "child_read", "callID": "call_child_read", "tool": "read", "state": map[string]any{"status": "completed", "title": "read", "input": map[string]any{"filePath": "/tmp/fixture.txt"}, "output": "fixture data"}}}}}
	f.mu.Unlock()
	// Provider stragglers are deliberately sent after the terminal frame.
	f.emit("session.text.delta", map[string]any{"sessionID": id, "assistantMessageID": "a1", "ordinal": 0, "delta": "late"})
	f.emit("session.execution.started", map[string]any{"sessionID": id})
}
func (f *fixture) continueTurn() {
	f.emit("session.execution.started", map[string]any{"sessionID": rootNative})
	f.emit("session.tool.input.started", map[string]any{"sessionID": rootNative, "id": "call_task_two", "name": "task"})
	f.emit("session.tool.called", map[string]any{"sessionID": rootNative, "id": "call_task_two", "input": map[string]any{"prompt": "continue child", "sessionID": childOne}})
	// Duplicate the same durable task call; origin IDs must remain unique.
	f.emit("session.tool.called", map[string]any{"sessionID": rootNative, "id": "call_task_two", "input": map[string]any{"prompt": "continue child", "sessionID": childOne}})
	f.emit("session.execution.started", map[string]any{"sessionID": childOne})
	// A repeated start while the revived child is live must not consume another
	// revival or allocate a second transcript turn.
	f.emit("session.execution.started", map[string]any{"sessionID": childOne})
	f.emit("session.text.started", map[string]any{"sessionID": childOne, "assistantMessageID": "a2", "ordinal": 0})
	f.emit("session.text.delta", map[string]any{"sessionID": childOne, "assistantMessageID": "a2", "ordinal": 0, "delta": "continued"})
	f.emit("session.tool.success", map[string]any{"sessionID": rootNative, "id": "call_task_two", "content": "continued"})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": childOne})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": rootNative})
}
func (f *fixture) activeTurn() {
	f.emit("session.execution.started", map[string]any{"sessionID": rootNative})
	f.emit("session.created", map[string]any{"id": childTwo, "parentID": rootNative, "title": "Active child"})
	f.emit("session.execution.started", map[string]any{"sessionID": childTwo})
	f.emit("session.text.started", map[string]any{"sessionID": childTwo, "assistantMessageID": "active", "ordinal": 0})
	f.emit("session.text.delta", map[string]any{"sessionID": childTwo, "assistantMessageID": "active", "ordinal": 0, "delta": "waiting"})
	f.emit("permission.asked", map[string]any{"sessionID": childTwo, "id": "perm_child_two", "action": "edit", "providerSecret": "must-not-leak"})
	f.emit("form.created", map[string]any{"form": map[string]any{"id": "frm_child_two", "sessionID": childTwo, "fields": []any{map[string]any{"id": "choice", "label": "Choose", "type": "select", "providerSecret": "must-not-leak"}}}})
	f.emit("form.created", map[string]any{"form": map[string]any{"id": "frm_cancel_child_two", "sessionID": childTwo, "fields": []any{map[string]any{"id": "confirm", "label": "Confirm", "type": "text"}}}})
	f.emit("permission.asked", map[string]any{"sessionID": childTwo, "id": "perm_stop_pending", "action": "edit"})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": rootNative})
}
func (f *fixture) transcript(w http.ResponseWriter, r *http.Request) {
	parts := strings.Split(strings.Trim(r.URL.Path, "/"), "/")
	if len(parts) < 4 {
		writeJSON(w, map[string]any{"data": []any{}})
		return
	}
	id := parts[2]
	f.mu.Lock()
	rows := append([]map[string]any(nil), f.childMessages[id]...)
	if id == rootNative {
		rows = append([]map[string]any(nil), f.rootMessages...)
	}
	f.mu.Unlock()
	for i, j := 0, len(rows)-1; i < j; i, j = i+1, j-1 {
		rows[i], rows[j] = rows[j], rows[i]
	}
	writeJSON(w, map[string]any{"data": rows, "cursor": map[string]any{}})
}

func (f *fixture) appendRootTranscript(text string) {
	now := time.Now().UnixMilli()
	userID := fmt.Sprintf("root-user-%d", now)
	assistantID := fmt.Sprintf("root-assistant-%d", now)
	f.mu.Lock()
	f.rootMessages = append(f.rootMessages, map[string]any{"id": userID, "type": "user", "time": map[string]any{"created": now}, "content": []any{map[string]any{"type": "text", "text": text}}}, map[string]any{"id": assistantID, "type": "assistant", "time": map[string]any{"created": now, "completed": now + 1}, "content": []any{map[string]any{"type": "text", "text": "fixture answer"}}})
	raw, _ := json.Marshal(f.rootMessages)
	f.mu.Unlock()
	tmp := filepath.Join(f.home, "root-transcript.tmp")
	_ = os.WriteFile(tmp, raw, 0600)
	_ = os.Rename(tmp, filepath.Join(f.home, "root-transcript.json"))
}

func oldTranscript(id string) []map[string]any {
	now := time.Now().UnixMilli()
	return []map[string]any{
		{"id": "old-user-" + id, "type": "user", "time": map[string]any{"created": now - 1000}, "content": []any{map[string]any{"type": "text", "text": "old prompt"}}},
		{"id": "old-assistant-" + id, "type": "assistant", "time": map[string]any{"created": now - 900, "completed": now - 800}, "content": []any{map[string]any{"type": "text", "text": "OLD_TURN_SENTINEL"}}},
	}
}

func (f *fixture) unknownTurn(id, permissionID, formID, release string, deleted bool) {
	f.emit("session.execution.started", map[string]any{"sessionID": rootNative})
	callID := "call_" + id
	f.emit("session.tool.input.started", map[string]any{"sessionID": rootNative, "id": callID, "name": "task"})
	f.emit("session.tool.called", map[string]any{"sessionID": rootNative, "id": callID, "input": map[string]any{"prompt": "historical child task", "sessionID": id}})
	// No session.created is sent. All child events below arrive before the
	// complete roster can establish parentage.
	f.emit("session.execution.started", map[string]any{"sessionID": id})
	f.emit("session.text.started", map[string]any{"sessionID": id, "assistantMessageID": "historical-answer", "ordinal": 0})
	delta := "EMPTY_CHILD_DELTA"
	if id == childUnknownOld {
		delta = "NEW_CHILD_DELTA"
	}
	f.emit("session.text.delta", map[string]any{"sessionID": id, "assistantMessageID": "historical-answer", "ordinal": 0, "delta": delta})
	f.emit("permission.asked", map[string]any{"sessionID": id, "id": permissionID, "action": "edit", "private": "drop"})
	f.emit("form.created", map[string]any{"form": map[string]any{"id": formID, "sessionID": id, "fields": []any{map[string]any{"id": "choice", "question": "Choose", "options": []any{map[string]any{"label": "yes"}}, "private": "drop"}}}})
	f.emit("form.created", map[string]any{"form": map[string]any{"id": strings.Replace(formID, "frm_early_", "frm_early_cancel_", 1), "sessionID": id, "fields": []any{map[string]any{"id": "cancel-choice", "question": "Cancel scope", "options": []any{map[string]any{"label": "yes"}}}}}})
	if err := f.waitRelease(release); err != nil {
		return
	}
	if deleted {
		f.emit("permission.asked", map[string]any{"sessionID": id, "id": "perm_delete_pending", "action": "edit"})
		f.emit("session.deleted", map[string]any{"id": id})
	} else {
		f.emit("session.execution.succeeded", map[string]any{"sessionID": id})
	}
	f.emit("session.execution.succeeded", map[string]any{"sessionID": rootNative})
}

func (f *fixture) waitRelease(name string) error {
	path := filepath.Join(f.home, name)
	deadline := time.Now().Add(180 * time.Second)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(path); err == nil {
			return nil
		}
		time.Sleep(15 * time.Millisecond)
	}
	return errors.New("fixture release timed out")
}

func (f *fixture) slowRosterTurn() {
	f.emit("session.execution.started", map[string]any{"sessionID": rootNative})
	f.emit("session.text.started", map[string]any{"sessionID": rootNative, "assistantMessageID": "race-root", "ordinal": 0})
	f.emit("session.text.delta", map[string]any{"sessionID": rootNative, "assistantMessageID": "race-root", "ordinal": 0, "delta": "ROOT_SSE_UNBLOCKED"})
	f.emit("session.created", map[string]any{"id": childRace, "parentID": rootNative, "title": "Race child"})
	f.emit("session.execution.started", map[string]any{"sessionID": childRace})
	f.emit("session.text.started", map[string]any{"sessionID": childRace, "assistantMessageID": "race-child", "ordinal": 0})
	f.emit("session.text.delta", map[string]any{"sessionID": childRace, "assistantMessageID": "race-child", "ordinal": 0, "delta": "RACE_CHILD_SURVIVES"})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": childRace})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": rootNative})
	f.mu.Lock()
	f.childMessages[childRace] = []map[string]any{{"id": "race-user", "type": "user", "time": map[string]any{"created": time.Now().UnixMilli()}, "content": []any{map[string]any{"type": "text", "text": "race child"}}}}
	f.mu.Unlock()
}

func (f *fixture) revisionRosterTurn() {
	f.emit("session.execution.started", map[string]any{"sessionID": rootNative})
	f.emit("session.text.started", map[string]any{"sessionID": rootNative, "assistantMessageID": "revision-root", "ordinal": 0})
	f.emit("session.text.delta", map[string]any{"sessionID": rootNative, "assistantMessageID": "revision-root", "ordinal": 0, "delta": "ROOT_SSE_DURING_REVISION_ROSTER"})
	f.emit("session.created", map[string]any{"id": childRevision, "parentID": rootNative, "title": "Revision child"})
	f.emit("session.execution.started", map[string]any{"sessionID": childRevision})
	f.emit("session.text.started", map[string]any{"sessionID": childRevision, "assistantMessageID": "revision-child", "ordinal": 0})
	f.emit("session.text.delta", map[string]any{"sessionID": childRevision, "assistantMessageID": "revision-child", "ordinal": 0, "delta": "REVISION_CHILD_SURVIVES"})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": childRevision})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": rootNative})
}

func (f *fixture) capacityTurn() {
	f.emit("session.execution.started", map[string]any{"sessionID": rootNative})
	prompt := strings.Repeat("p", 70<<10)
	f.emit("session.tool.input.started", map[string]any{"sessionID": rootNative, "id": "call_capacity", "name": "task"})
	f.emit("session.tool.called", map[string]any{"sessionID": rootNative, "id": "call_capacity", "input": map[string]any{"prompt": prompt, "sessionID": childOne}})
	f.emit("session.execution.started", map[string]any{"sessionID": childOne})
	f.emit("session.text.started", map[string]any{"sessionID": childOne, "assistantMessageID": "capacity-child", "ordinal": 0})
	f.emit("session.text.delta", map[string]any{"sessionID": childOne, "assistantMessageID": "capacity-child", "ordinal": 0, "delta": strings.Repeat("x", 70<<10)})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": childOne})
	f.emit("session.text.started", map[string]any{"sessionID": rootNative, "assistantMessageID": "capacity-root", "ordinal": 0})
	f.emit("session.text.delta", map[string]any{"sessionID": rootNative, "assistantMessageID": "capacity-root", "ordinal": 0, "delta": "ROOT_CONTINUES_AFTER_TRUNCATION"})
	f.emit("session.execution.succeeded", map[string]any{"sessionID": rootNative})
}

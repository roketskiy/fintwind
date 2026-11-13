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

const nativeFixtureSession = "ses_activity_fixture"

type fixtureStore struct {
	Messages []map[string]any `json:"messages"`
}

type fakeEngine struct {
	mu          sync.Mutex
	statePath   string
	state       fixtureStore
	subscribers map[chan []byte]struct{}
}

func serveFakeCLI() error {
	home := os.Getenv("OPENCODE_TEST_HOME")
	password := os.Getenv("OPENCODE_SERVER_PASSWORD")
	if !filepath.IsAbs(home) || password == "" {
		return errors.New("isolated engine environment is required")
	}
	for _, key := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY"} {
		if os.Getenv(key) != "" {
			return fmt.Errorf("fixture unexpectedly received %s", key)
		}
	}
	if err := os.MkdirAll(home, 0o700); err != nil {
		return err
	}
	e := &fakeEngine{statePath: filepath.Join(home, "activity-transcript.json"), subscribers: map[chan []byte]struct{}{}}
	if raw, err := os.ReadFile(e.statePath); err == nil {
		if err := json.Unmarshal(raw, &e.state); err != nil {
			return fmt.Errorf("read isolated transcript fixture: %w", err)
		}
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	server := &http.Server{ReadHeaderTimeout: 5 * time.Second, Handler: e.handler(password)}
	defer server.Close()
	fmt.Printf("OpenCode server listening on http://%s\n", listener.Addr())
	return server.Serve(listener)
}

func (e *fakeEngine) handler(password string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		user, gotPassword, ok := r.BasicAuth()
		if !ok || user != "opencode" || gotPassword != password || r.Header.Get("x-opencode-directory") == "" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/api/info":
			writeJSON(w, map[string]any{"data": map[string]any{}})
		case r.Method == http.MethodPost && r.URL.Path == "/api/session":
			writeJSON(w, map[string]any{"data": map[string]string{"id": nativeFixtureSession}})
		case r.Method == http.MethodGet && r.URL.Path == "/api/model":
			writeJSON(w, map[string]any{"data": []any{}})
		case r.Method == http.MethodGet && r.URL.Path == "/api/event":
			e.stream(w, r)
		case r.Method == http.MethodGet && r.URL.Path == "/api/session/"+nativeFixtureSession:
			writeJSON(w, map[string]any{"data": map[string]string{"id": nativeFixtureSession}})
		case r.Method == http.MethodGet && r.URL.Path == "/api/session/"+nativeFixtureSession+"/message":
			e.messages(w)
		case r.Method == http.MethodPost && strings.HasPrefix(r.URL.Path, "/api/session/") && strings.HasSuffix(r.URL.Path, "/prompt"):
			e.prompt(w, r)
		case r.Method == http.MethodPost && strings.HasPrefix(r.URL.Path, "/api/session/"):
			writeJSON(w, map[string]any{"data": map[string]any{}})
		default:
			writeJSON(w, map[string]any{"data": map[string]any{}})
		}
	})
}

func writeJSON(w http.ResponseWriter, value any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(value)
}

func (e *fakeEngine) stream(w http.ResponseWriter, r *http.Request) {
	ch := make(chan []byte, 128)
	e.mu.Lock()
	e.subscribers[ch] = struct{}{}
	e.mu.Unlock()
	defer func() {
		e.mu.Lock()
		delete(e.subscribers, ch)
		e.mu.Unlock()
	}()
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.WriteHeader(http.StatusOK)
	w.(http.Flusher).Flush()
	for {
		select {
		case <-r.Context().Done():
			return
		case raw := <-ch:
			_, _ = fmt.Fprintf(w, "data: %s\n\n", raw)
			w.(http.Flusher).Flush()
		}
	}
}

func (e *fakeEngine) emit(kind string, data map[string]any) {
	raw, _ := json.Marshal(map[string]any{"type": kind, "data": data})
	e.mu.Lock()
	defer e.mu.Unlock()
	for ch := range e.subscribers {
		select {
		case ch <- raw:
		default:
		}
	}
}

func (e *fakeEngine) messages(w http.ResponseWriter) {
	e.mu.Lock()
	messages := append([]map[string]any(nil), e.state.Messages...)
	e.mu.Unlock()
	for i, j := 0, len(messages)-1; i < j; i, j = i+1, j-1 {
		messages[i], messages[j] = messages[j], messages[i]
	}
	writeJSON(w, map[string]any{"data": messages, "cursor": map[string]any{}})
}

func (e *fakeEngine) appendTranscript(rows ...map[string]any) error {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.state.Messages = append(e.state.Messages, rows...)
	raw, err := json.Marshal(e.state)
	if err != nil {
		return err
	}
	tmp := e.statePath + ".tmp"
	if err := os.WriteFile(tmp, raw, 0o600); err != nil {
		return err
	}
	return os.Rename(tmp, e.statePath)
}

type toolFixture struct {
	id     string
	name   string
	input  map[string]any
	output string
	error  string
}

func fixtureTools() []toolFixture {
	names := []string{"read", "bash", "edit", "grep", "todo", "mcp__external__read", "create_thread", "read_mcp_resource"}
	out := make([]toolFixture, 0, len(names))
	for i, name := range names {
		id := fmt.Sprintf("tool-%02d", i+1)
		input := map[string]any{"path": fmt.Sprintf("file-%02d.txt", i+1), "ordinal": i + 1}
		item := toolFixture{id: id, name: name, input: input, output: "output:" + name}
		if name == "read_mcp_resource" {
			item.error = "fixture resource read failed"
		}
		out = append(out, item)
	}
	return out
}

func toolData(id string, fields map[string]any) map[string]any {
	fields["sessionID"] = nativeFixtureSession
	fields["id"] = id
	return fields
}

func (e *fakeEngine) prompt(w http.ResponseWriter, r *http.Request) {
	var body struct {
		ID   string `json:"id"`
		Text string `json:"text"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil || body.ID == "" {
		http.Error(w, "invalid prompt", http.StatusBadRequest)
		return
	}
	now := time.Now().UnixMilli()
	userID := "user-" + body.ID
	assistantID := "assistant-" + body.ID
	user := map[string]any{"id": userID, "type": "user", "time": map[string]any{"created": now}, "content": []any{map[string]any{"type": "text", "text": body.Text}}}
	parts := []any{}
	if body.Text == "invalid-only" {
		// Prior ID deliberately arrives after actor.tools is reset for this turn.
		e.emit("session.tool.progress", toolData("tool-01", map[string]any{"status": "running", "output": "late progress must be ignored"}))
		e.emit("session.tool.input.started", toolData("", map[string]any{"name": "read"}))
		e.emit("session.tool.input.started", toolData("missing-name", map[string]any{"input": map[string]any{"x": 1}}))
	} else {
		// Invalid starts/progress precede all valid events and must be inert.
		e.emit("session.tool.progress", toolData("unknown-tool", map[string]any{"status": "running"}))
		e.emit("session.tool.input.started", toolData("", map[string]any{"name": "read"}))
		e.emit("session.tool.input.started", toolData("missing-name", map[string]any{"input": map[string]any{"x": 1}}))
		e.emit("session.text.started", map[string]any{"sessionID": nativeFixtureSession, "assistantMessageID": assistantID, "ordinal": 0})
		e.emit("session.text.delta", map[string]any{"sessionID": nativeFixtureSession, "assistantMessageID": assistantID, "ordinal": 0, "delta": "before "})
		parts = append(parts, map[string]any{"type": "text", "text": "before "})
		for index, tool := range fixtureTools() {
			e.emit("session.tool.input.started", toolData(tool.id, map[string]any{"name": tool.name}))
			e.emit("session.tool.called", toolData(tool.id, map[string]any{"input": tool.input}))
			e.emit("session.tool.progress", toolData(tool.id, map[string]any{"status": "running", "output": "in progress"}))
			state := map[string]any{"status": "completed", "title": tool.name, "input": tool.input, "output": tool.output}
			terminal := "session.tool.success"
			terminalFields := map[string]any{"content": tool.output}
			if tool.error != "" {
				state = map[string]any{"status": "error", "title": tool.name, "input": tool.input, "error": map[string]any{"message": tool.error}}
				terminal = "session.tool.error"
				terminalFields = map[string]any{"error": map[string]any{"message": tool.error}}
			}
			e.emit(terminal, toolData(tool.id, terminalFields))
			part := map[string]any{"type": "tool", "id": tool.id, "callID": "call-" + tool.id, "tool": tool.name, "state": state}
			parts = append(parts, part)
			if index == 0 {
				e.emit("session.text.delta", map[string]any{"sessionID": nativeFixtureSession, "assistantMessageID": assistantID, "ordinal": 0, "delta": "middle "})
				parts = append(parts, map[string]any{"type": "text", "text": " middle "})
			}
		}
		e.emit("session.text.delta", map[string]any{"sessionID": nativeFixtureSession, "assistantMessageID": assistantID, "ordinal": 0, "delta": "after"})
		e.emit("session.text.ended", map[string]any{"sessionID": nativeFixtureSession, "assistantMessageID": assistantID, "ordinal": 0, "text": "before middle after"})
		parts = append(parts, map[string]any{"type": "text", "text": " after"})
		// Late progress after terminal must not reopen the last tool.
		e.emit("session.tool.progress", toolData("tool-08", map[string]any{"status": "running", "output": "late terminal progress"}))
	}
	e.emit("session.execution.succeeded", map[string]any{"sessionID": nativeFixtureSession})
	assistant := map[string]any{"id": assistantID, "type": "assistant", "time": map[string]any{"created": now, "completed": now + 2}, "content": parts}
	if err := e.appendTranscript(user, assistant); err != nil {
		http.Error(w, "fixture persistence failed", http.StatusInternalServerError)
		return
	}
	writeJSON(w, map[string]any{"data": map[string]any{}})
}

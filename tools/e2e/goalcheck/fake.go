package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

const fakeNativeID = "ses_goalcheck_native"

type fakeCall struct {
	Method string          `json:"method"`
	Path   string          `json:"path"`
	Body   json.RawMessage `json:"body,omitempty"`
	Status int             `json:"status"`
}

type fakeState struct {
	Goal            json.RawMessage  `json:"goal"`
	Messages        []map[string]any `json:"messages"`
	GetStatus       int              `json:"getStatus"`
	MessageStatus   int              `json:"messageStatus"`
	CommandMode     string           `json:"commandMode"`
	HoldNextCommand bool             `json:"holdNextCommand"`
}

type fakeEngine struct {
	mu          sync.Mutex
	home        string
	password    string
	state       fakeState
	subscribers map[chan []byte]struct{}
	trace       *os.File
}

func runFakeServer() error {
	home := os.Getenv("OPENCODE_TEST_HOME")
	password := os.Getenv("OPENCODE_SERVER_PASSWORD")
	if !filepath.IsAbs(home) || password == "" {
		return errors.New("isolated fixture environment required")
	}
	for _, name := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "OPENROUTER_API_KEY", "GOOGLE_API_KEY", "AZURE_OPENAI_API_KEY", "FINTWIND_AGENT_REAL_PROVIDER"} {
		if strings.TrimSpace(os.Getenv(name)) != "" {
			return fmt.Errorf("fixture received forbidden credential or real-provider opt-in: %s", name)
		}
	}
	if err := os.MkdirAll(home, 0o700); err != nil {
		return err
	}
	e := &fakeEngine{home: home, password: password, subscribers: map[chan []byte]struct{}{}}
	statePath := filepath.Join(home, "goalcheck-state.json")
	if raw, err := os.ReadFile(statePath); err == nil {
		if err := json.Unmarshal(raw, &e.state); err != nil {
			return errors.New("isolated fixture state is invalid")
		}
	}
	if e.state.Messages == nil {
		e.state.Messages = []map[string]any{}
	}
	trace, err := os.OpenFile(filepath.Join(home, "goalcheck-http-trace.jsonl"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	e.trace = trace
	defer trace.Close()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	address := "http://" + listener.Addr().String()
	if err := os.WriteFile(filepath.Join(home, "goalcheck-address.txt"), []byte(address), 0o600); err != nil {
		listener.Close()
		return err
	}
	server := &http.Server{ReadHeaderTimeout: 5 * time.Second, Handler: e.handler()}
	defer server.Close()
	fmt.Println(address)
	err = server.Serve(listener)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

func (e *fakeEngine) saveLocked() error {
	raw, err := json.Marshal(e.state)
	if err != nil {
		return err
	}
	tmp := filepath.Join(e.home, "goalcheck-state.tmp")
	if err := os.WriteFile(tmp, raw, 0o600); err != nil {
		return err
	}
	return os.Rename(tmp, filepath.Join(e.home, "goalcheck-state.json"))
}

func (e *fakeEngine) recordLocked(method, path string, body []byte, status int) {
	if e.trace == nil {
		return
	}
	raw, _ := json.Marshal(fakeCall{Method: method, Path: path, Body: append(json.RawMessage(nil), body...), Status: status})
	_, _ = e.trace.Write(append(raw, '\n'))
	_ = e.trace.Sync()
}

func (e *fakeEngine) handler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		host, _, _ := net.SplitHostPort(r.RemoteAddr)
		ip := net.ParseIP(host)
		if ip == nil || !ip.IsLoopback() {
			http.Error(w, "loopback only", http.StatusForbidden)
			return
		}
		if strings.HasPrefix(r.URL.Path, "/__fixture/") {
			e.handleFixture(w, r)
			return
		}
		user, password, ok := r.BasicAuth()
		if !ok || user != "opencode" || password != e.password || r.Header.Get("x-opencode-directory") == "" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		body, readErr := io.ReadAll(io.LimitReader(r.Body, 2<<20))
		if readErr != nil {
			http.Error(w, "bad body", http.StatusBadRequest)
			return
		}
		if r.URL.Path == "/api/event" && r.Method == http.MethodGet {
			e.stream(w, r)
			return
		}
		status := http.StatusOK
		e.mu.Lock()
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/api/info":
			e.recordLocked(r.Method, r.URL.Path, body, status)
			e.mu.Unlock()
			writeJSON(w, map[string]any{"data": map[string]any{}})
			return
		case r.Method == http.MethodGet && r.URL.Path == "/api/model":
			e.recordLocked(r.Method, r.URL.Path, body, status)
			e.mu.Unlock()
			writeJSON(w, map[string]any{"data": []any{}})
			return
		case r.Method == http.MethodPost && r.URL.Path == "/api/session":
			e.recordLocked(r.Method, r.URL.Path, body, status)
			e.mu.Unlock()
			writeJSON(w, map[string]any{"data": map[string]string{"id": fakeNativeID}})
			return
		case r.Method == http.MethodGet && r.URL.Path == "/api/session/"+fakeNativeID:
			e.recordLocked(r.Method, r.URL.Path, body, status)
			e.mu.Unlock()
			writeJSON(w, map[string]any{"data": map[string]string{"id": fakeNativeID}})
			return
		case r.Method == http.MethodPost && (strings.HasSuffix(r.URL.Path, "/agent") || strings.HasSuffix(r.URL.Path, "/model")):
			e.recordLocked(r.Method, r.URL.Path, body, status)
			e.mu.Unlock()
			writeJSON(w, map[string]any{"data": map[string]any{}})
			return
		case r.Method == http.MethodPost && r.URL.Path == "/api/rpc/opencode.goal/get":
			if e.state.GetStatus != 0 {
				status = e.state.GetStatus
			}
			e.recordLocked(r.Method, r.URL.Path, body, status)
			goal := append(json.RawMessage(nil), e.state.Goal...)
			e.mu.Unlock()
			if status != http.StatusOK {
				http.Error(w, "fixture goal read failed", status)
				return
			}
			if len(goal) == 0 {
				goal = json.RawMessage("null")
			}
			writeJSON(w, map[string]any{"output": goal})
			return
		case r.Method == http.MethodPost && r.URL.Path == "/api/session/"+fakeNativeID+"/command":
			e.handleGoalCommandLocked(w, r, body)
			return
		case r.Method == http.MethodPost && r.URL.Path == "/api/session/"+fakeNativeID+"/interrupt":
			e.recordLocked(r.Method, r.URL.Path, body, status)
			e.mu.Unlock()
			e.emit("session.execution.interrupted", map[string]any{"sessionID": fakeNativeID})
			writeJSON(w, map[string]any{"data": map[string]any{}})
			return
		case r.Method == http.MethodPost && r.URL.Path == "/api/session/"+fakeNativeID+"/prompt":
			e.recordLocked(r.Method, r.URL.Path, body, status)
			e.mu.Unlock()
			e.handlePrompt(w, body)
			return
		case r.Method == http.MethodGet && strings.HasPrefix(r.URL.Path, "/api/session/") && strings.HasSuffix(r.URL.Path, "/message"):
			if e.state.MessageStatus != 0 {
				status = e.state.MessageStatus
			}
			e.recordLocked(r.Method, r.URL.RequestURI(), body, status)
			messages := append([]map[string]any(nil), e.state.Messages...)
			e.mu.Unlock()
			if status != http.StatusOK {
				http.Error(w, "fixture message read failed", status)
				return
			}
			writeJSON(w, map[string]any{"data": messages, "cursor": map[string]any{}})
			return
		default:
			e.recordLocked(r.Method, r.URL.RequestURI(), body, http.StatusNotFound)
			e.mu.Unlock()
			http.Error(w, "unexpected fixture route", http.StatusNotFound)
			return
		}
	})
}

func (e *fakeEngine) handleGoalCommandLocked(w http.ResponseWriter, r *http.Request, body []byte) {
	var command map[string]any
	if json.Unmarshal(body, &command) != nil {
		e.recordLocked(r.Method, r.URL.Path, body, 400)
		e.mu.Unlock()
		http.Error(w, "bad command", 400)
		return
	}
	mode := e.state.CommandMode
	hold := e.state.HoldNextCommand
	e.state.CommandMode = ""
	e.state.HoldNextCommand = false
	if mode == "reject400" || mode == "reject422" {
		status := 400
		if mode == "reject422" {
			status = 422
		}
		e.recordLocked(r.Method, r.URL.Path, body, status)
		_ = e.saveLocked()
		e.mu.Unlock()
		http.Error(w, "fixture rejected", status)
		return
	}
	text, _ := command["text"].(string)
	if mode == "drop" {
		e.commitGoalLocked(text)
		e.recordLocked(r.Method, r.URL.Path, body, 200)
		_ = e.saveLocked()
		e.mu.Unlock()
		if hijacker, ok := w.(http.Hijacker); ok {
			connection, _, err := hijacker.Hijack()
			if err == nil {
				_ = connection.Close()
			}
		}
		return
	}
	e.commitGoalLocked(text)
	e.recordLocked(r.Method, r.URL.Path, body, 200)
	_ = e.saveLocked()
	goal := append(json.RawMessage(nil), e.state.Goal...)
	e.mu.Unlock()
	if text != "pause" && text != "clear" {
		e.emit("session.execution.started", map[string]any{"sessionID": fakeNativeID})
	}
	e.emit("session.goal.updated", map[string]any{"sessionID": fakeNativeID, "goal": rawOrNull(goal), "privateFixtureField": "synthetic-private-payload"})
	writeJSON(w, map[string]any{"data": map[string]any{}})
	if !hold && text != "pause" && text != "clear" {
		e.emit("session.execution.succeeded", map[string]any{"sessionID": fakeNativeID})
	}
}

func (e *fakeEngine) commitGoalLocked(text string) {
	now := time.Now().UnixMilli()
	switch text {
	case "clear":
		e.state.Goal = json.RawMessage("null")
	case "pause":
		e.state.Goal = json.RawMessage(fmt.Sprintf(`{"objective":"goalcheck objective","status":"paused","reason":"fixture pause","rounds":5,"maxRounds":9,"updatedAt":%d,"privateFixtureField":"synthetic-private-payload"}`, now))
	case "resume":
		e.state.Goal = json.RawMessage(fmt.Sprintf(`{"objective":"goalcheck objective","status":"active","reason":"fixture resume","rounds":6,"maxRounds":9,"updatedAt":%d,"privateFixtureField":"synthetic-private-payload"}`, now))
	default:
		e.state.Goal = json.RawMessage(fmt.Sprintf(`{"objective":%q,"status":"active","reason":"fixture start","rounds":1,"maxRounds":9,"updatedAt":%d,"privateFixtureField":"synthetic-private-payload"}`, text, now))
	}
}

func rawOrNull(raw []byte) json.RawMessage {
	if len(raw) == 0 {
		return json.RawMessage("null")
	}
	return raw
}

func (e *fakeEngine) handlePrompt(w http.ResponseWriter, body []byte) {
	var prompt struct {
		ID    string           `json:"id"`
		Text  string           `json:"text"`
		Files []map[string]any `json:"files"`
	}
	if json.Unmarshal(body, &prompt) != nil || prompt.ID == "" {
		http.Error(w, "invalid prompt", 400)
		return
	}
	user := map[string]any{"id": "user-" + prompt.ID, "type": "user", "time": map[string]any{"created": time.Now().UnixMilli()}, "content": []any{map[string]any{"type": "text", "text": prompt.Text}}}
	e.mu.Lock()
	e.state.Messages = append([]map[string]any{user}, e.state.Messages...)
	_ = e.saveLocked()
	e.mu.Unlock()
	e.emit("session.execution.started", map[string]any{"sessionID": fakeNativeID})
	if prompt.Text == "bootstrap" {
		assistantID := "assistant-" + prompt.ID
		e.emit("session.text.started", map[string]any{"sessionID": fakeNativeID, "assistantMessageID": assistantID, "ordinal": 0})
		e.emit("session.text.delta", map[string]any{"sessionID": fakeNativeID, "assistantMessageID": assistantID, "ordinal": 0, "delta": "fixture ready"})
		e.emit("session.text.ended", map[string]any{"sessionID": fakeNativeID, "assistantMessageID": assistantID, "ordinal": 0, "text": "fixture ready"})
		e.emit("session.tool.input.started", map[string]any{"sessionID": fakeNativeID, "id": "tool-goalcheck-bootstrap", "name": "read", "input": map[string]any{"path": "synthetic fixture"}})
		e.emit("session.tool.success", map[string]any{"sessionID": fakeNativeID, "id": "tool-goalcheck-bootstrap", "content": "synthetic tool fixture complete"})
		assistant := map[string]any{"id": assistantID, "type": "assistant", "time": map[string]any{"created": time.Now().UnixMilli()}, "content": []any{map[string]any{"type": "text", "text": "fixture ready"}}}
		e.mu.Lock()
		e.state.Messages = append([]map[string]any{assistant}, e.state.Messages...)
		_ = e.saveLocked()
		e.mu.Unlock()
		e.emit("session.execution.succeeded", map[string]any{"sessionID": fakeNativeID})
	}
	writeJSON(w, map[string]any{"data": map[string]any{}})
}

func (e *fakeEngine) stream(w http.ResponseWriter, r *http.Request) {
	ch := make(chan []byte, 128)
	e.mu.Lock()
	e.subscribers[ch] = struct{}{}
	e.mu.Unlock()
	defer func() { e.mu.Lock(); delete(e.subscribers, ch); e.mu.Unlock() }()
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

func (e *fakeEngine) handleFixture(w http.ResponseWriter, r *http.Request) {
	switch r.URL.Path {
	case "/__fixture/config":
		var input map[string]json.RawMessage
		if json.NewDecoder(io.LimitReader(r.Body, 1<<20)).Decode(&input) != nil {
			http.Error(w, "bad fixture config", 400)
			return
		}
		e.mu.Lock()
		if raw, ok := input["goal"]; ok {
			e.state.Goal = append(json.RawMessage(nil), raw...)
		}
		if raw, ok := input["getStatus"]; ok {
			_ = json.Unmarshal(raw, &e.state.GetStatus)
		}
		if raw, ok := input["messageStatus"]; ok {
			_ = json.Unmarshal(raw, &e.state.MessageStatus)
		}
		if raw, ok := input["commandMode"]; ok {
			_ = json.Unmarshal(raw, &e.state.CommandMode)
		}
		if raw, ok := input["holdNextCommand"]; ok {
			_ = json.Unmarshal(raw, &e.state.HoldNextCommand)
		}
		if raw, ok := input["messages"]; ok {
			_ = json.Unmarshal(raw, &e.state.Messages)
		}
		err := e.saveLocked()
		e.mu.Unlock()
		if err != nil {
			http.Error(w, "fixture state write failed", 500)
			return
		}
		writeJSON(w, map[string]any{"ok": true})
	case "/__fixture/emit":
		var input struct {
			Type string         `json:"type"`
			Data map[string]any `json:"data"`
		}
		if json.NewDecoder(io.LimitReader(r.Body, 1<<20)).Decode(&input) != nil || input.Type == "" {
			http.Error(w, "bad event fixture", 400)
			return
		}
		e.emit(input.Type, input.Data)
		writeJSON(w, map[string]any{"ok": true})
	case "/__fixture/stats":
		e.mu.Lock()
		defer e.mu.Unlock()
		e.trace.Sync()
		raw, _ := os.ReadFile(filepath.Join(e.home, "goalcheck-http-trace.jsonl"))
		var calls []fakeCall
		for _, line := range strings.Split(string(raw), "\n") {
			var call fakeCall
			if json.Unmarshal([]byte(line), &call) == nil {
				calls = append(calls, call)
			}
		}
		var gets, commands, prompts, messageReads int
		for _, call := range calls {
			if call.Path == "/api/rpc/opencode.goal/get" {
				gets++
			}
			if strings.HasSuffix(call.Path, "/command") {
				commands++
			}
			if strings.HasSuffix(call.Path, "/prompt") {
				prompts++
			}
			if strings.Contains(call.Path, "/message?") {
				messageReads++
			}
		}
		writeJSON(w, map[string]any{"calls": calls, "goalGetCount": gets, "commandCount": commands, "promptCount": prompts, "messageReadCount": messageReads})
	default:
		http.NotFound(w, r)
	}
}

func writeJSON(w http.ResponseWriter, value any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(value)
}

func waitFixtureAddress(ctx context.Context, path string) (string, error) {
	ticker := time.NewTicker(20 * time.Millisecond)
	defer ticker.Stop()
	for {
		raw, err := os.ReadFile(path)
		if err == nil && strings.HasPrefix(string(raw), "http://127.0.0.1:") {
			return strings.TrimSpace(string(raw)), nil
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-ticker.C:
		}
	}
}

func fixtureRequest(ctx context.Context, base, path string, body any) (json.RawMessage, int, error) {
	var reader io.Reader
	if body != nil {
		raw, err := json.Marshal(body)
		if err != nil {
			return nil, 0, err
		}
		reader = strings.NewReader(string(raw))
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, base+path, reader)
	if err != nil {
		return nil, 0, err
	}
	req.Header.Set("Content-Type", "application/json")
	client := &http.Client{Transport: &http.Transport{Proxy: nil}, Timeout: 5 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return nil, 0, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 2<<20))
	return raw, resp.StatusCode, err
}

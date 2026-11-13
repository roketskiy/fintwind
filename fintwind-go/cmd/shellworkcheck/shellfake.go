package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
)

const (
	nativeRootSession  = "ses_shellwork_root"
	nativeChildSession = "ses_shellwork_child"
)

type fixturePage struct {
	Output string `json:"output"`
	Cursor uint64 `json:"cursor"`
	Size   uint64 `json:"size"`
}

type shellFixture struct {
	Owner                 string                 `json:"owner"`
	Status                string                 `json:"status"`
	Exit                  int                    `json:"exit"`
	Missing               bool                   `json:"missing"`
	RowStatusCode         int                    `json:"rowStatusCode"`
	RowFailureRemaining   int                    `json:"rowFailureRemaining"`
	MalformedRowRemaining int                    `json:"malformedRowRemaining"`
	WrongOwnerRows        int                    `json:"wrongOwnerRows"`
	DeleteStatusCode      int                    `json:"deleteStatusCode"`
	FailOutputCursor      string                 `json:"failOutputCursor"`
	FailOutputRemaining   int                    `json:"failOutputRemaining"`
	HoldOutputCursor      string                 `json:"holdOutputCursor"`
	HoldOutput            bool                   `json:"holdOutput"`
	SlowCursor            string                 `json:"slowCursor"`
	SlowDelayMs           int                    `json:"slowDelayMs"`
	SlowUsed              bool                   `json:"slowUsed"`
	Pages                 map[string]fixturePage `json:"pages"`
}

type shellRequestRecord struct {
	ID     string `json:"id"`
	Cursor uint64 `json:"cursor,omitempty"`
	Limit  uint64 `json:"limit,omitempty"`
	Status int    `json:"status"`
}

type shellFixtureStore struct {
	Messages       map[string][]map[string]any `json:"messages"`
	Shells         map[string]shellFixture     `json:"shells"`
	RowCalls       map[string]int              `json:"rowCalls"`
	RowRequests    []shellRequestRecord        `json:"rowRequests"`
	OutputRequests []shellRequestRecord        `json:"outputRequests"`
	DeleteRequests []string                    `json:"deleteRequests"`
	SlowStarted    map[string]bool             `json:"slowStarted"`
}

type shellEngine struct {
	mu          sync.Mutex
	statePath   string
	state       shellFixtureStore
	subscribers map[chan []byte]struct{}
	password    string
	adminToken  string
}

func serveShellFakeCLI() error {
	home := os.Getenv("OPENCODE_TEST_HOME")
	password := os.Getenv("OPENCODE_SERVER_PASSWORD")
	if !filepath.IsAbs(home) || password == "" {
		return errors.New("isolated shell fixture environment is required")
	}
	for _, key := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "FINTWIND_AGENT_REAL_PROVIDER"} {
		if os.Getenv(key) != "" {
			return fmt.Errorf("fixture unexpectedly received %s", key)
		}
	}
	if err := os.MkdirAll(home, 0o700); err != nil {
		return err
	}
	e := &shellEngine{statePath: filepath.Join(home, "shellwork-state.json"), subscribers: map[chan []byte]struct{}{}, password: password, adminToken: uuid.NewString()}
	e.state = shellFixtureStore{Messages: map[string][]map[string]any{}, Shells: map[string]shellFixture{}, RowCalls: map[string]int{}, SlowStarted: map[string]bool{}}
	if raw, err := os.ReadFile(e.statePath); err == nil {
		if err := json.Unmarshal(raw, &e.state); err != nil {
			return fmt.Errorf("read shell fixture state: %w", err)
		}
		e.ensureStateMaps()
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	ready := struct {
		Address string `json:"address"`
		Token   string `json:"token"`
	}{Address: listener.Addr().String(), Token: e.adminToken}
	rawReady, _ := json.Marshal(ready)
	if err := os.WriteFile(filepath.Join(home, "shellwork-ready.json"), rawReady, 0o600); err != nil {
		_ = listener.Close()
		return err
	}
	server := &http.Server{ReadHeaderTimeout: 5 * time.Second, Handler: e.handler()}
	defer server.Close()
	fmt.Printf("OpenCode server listening on http://%s\n", listener.Addr())
	return server.Serve(listener)
}

func (e *shellEngine) ensureStateMaps() {
	if e.state.Messages == nil {
		e.state.Messages = map[string][]map[string]any{}
	}
	if e.state.Shells == nil {
		e.state.Shells = map[string]shellFixture{}
	}
	if e.state.RowCalls == nil {
		e.state.RowCalls = map[string]int{}
	}
	if e.state.SlowStarted == nil {
		e.state.SlowStarted = map[string]bool{}
	}
}

func (e *shellEngine) saveLocked() error {
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

func (e *shellEngine) handler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasPrefix(r.URL.Path, "/_fixture/") {
			e.admin(w, r)
			return
		}
		user, password, ok := r.BasicAuth()
		if !ok || user != "opencode" || password != e.password || r.Header.Get("x-opencode-directory") == "" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/api/info":
			writeShellJSON(w, map[string]any{"data": map[string]any{}})
		case r.Method == http.MethodPost && r.URL.Path == "/api/session":
			writeShellJSON(w, map[string]any{"data": map[string]string{"id": nativeRootSession}})
		case r.Method == http.MethodGet && r.URL.Path == "/api/model":
			writeShellJSON(w, map[string]any{"data": []any{}})
		case r.Method == http.MethodGet && r.URL.Path == "/api/event":
			e.stream(w, r)
		case r.Method == http.MethodGet && r.URL.Path == "/api/session/active":
			writeShellJSON(w, map[string]any{"data": map[string]any{}})
		case r.Method == http.MethodGet && r.URL.Path == "/api/session":
			writeShellJSON(w, map[string]any{"data": []any{map[string]any{"id": nativeChildSession, "parentID": nativeRootSession, "title": "fixture child", "status": "idle"}}, "cursor": nil})
		case r.Method == http.MethodGet && strings.HasPrefix(r.URL.Path, "/api/shell/"):
			e.shell(w, r)
		case r.Method == http.MethodDelete && strings.HasPrefix(r.URL.Path, "/api/shell/"):
			e.deleteShell(w, r)
		case r.Method == http.MethodGet && strings.HasPrefix(r.URL.Path, "/api/session/") && strings.HasSuffix(r.URL.Path, "/message"):
			e.messages(w, strings.TrimSuffix(strings.TrimPrefix(r.URL.Path, "/api/session/"), "/message"))
		case r.Method == http.MethodGet && strings.HasPrefix(r.URL.Path, "/api/session/"):
			id := strings.TrimPrefix(r.URL.Path, "/api/session/")
			writeShellJSON(w, map[string]any{"data": map[string]string{"id": id}})
		case r.Method == http.MethodPost && strings.HasPrefix(r.URL.Path, "/api/session/") && strings.HasSuffix(r.URL.Path, "/prompt"):
			e.prompt(w, r)
		case r.Method == http.MethodPost && strings.HasPrefix(r.URL.Path, "/api/session/"):
			writeShellJSON(w, map[string]any{"data": map[string]any{}})
		default:
			writeShellJSON(w, map[string]any{"data": map[string]any{}})
		}
	})
}

func (e *shellEngine) admin(w http.ResponseWriter, r *http.Request) {
	if r.Header.Get("x-fixture-token") != e.adminToken || !strings.HasPrefix(r.RemoteAddr, "127.0.0.1:") {
		http.Error(w, "unauthorized fixture control", http.StatusUnauthorized)
		return
	}
	if r.Method == http.MethodGet && r.URL.Path == "/_fixture/state" {
		e.mu.Lock()
		state := e.state
		raw, err := json.Marshal(state)
		e.mu.Unlock()
		if err != nil {
			http.Error(w, "state encoding failed", 500)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write(raw)
		return
	}
	if r.Method != http.MethodPost || r.URL.Path != "/_fixture/control" {
		http.NotFound(w, r)
		return
	}
	var command struct {
		Type  string         `json:"type"`
		ID    string         `json:"id"`
		Shell shellFixture   `json:"shell"`
		Kind  string         `json:"kind"`
		Data  map[string]any `json:"data"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&command); err != nil {
		http.Error(w, "invalid fixture control", http.StatusBadRequest)
		return
	}
	switch command.Type {
	case "configure":
		e.mu.Lock()
		e.ensureStateMaps()
		e.state.Shells[command.ID] = command.Shell
		err := e.saveLocked()
		e.mu.Unlock()
		if err != nil {
			http.Error(w, "fixture persistence failed", 500)
			return
		}
	case "emit":
		e.emit(command.Kind, command.Data)
	default:
		http.Error(w, "unknown fixture control", http.StatusBadRequest)
		return
	}
	writeShellJSON(w, map[string]any{"ok": true})
}

func (e *shellEngine) shell(w http.ResponseWriter, r *http.Request) {
	path := strings.TrimPrefix(r.URL.Path, "/api/shell/")
	if strings.HasSuffix(path, "/output") {
		e.output(w, r, strings.TrimSuffix(path, "/output"))
		return
	}
	id, _ := url.PathUnescape(path)
	e.mu.Lock()
	fixture, ok := e.state.Shells[id]
	if !ok || fixture.Missing {
		e.state.RowRequests = append(e.state.RowRequests, shellRequestRecord{ID: id, Status: http.StatusNotFound})
		_ = e.saveLocked()
		e.mu.Unlock()
		http.NotFound(w, r)
		return
	}
	e.state.RowCalls[id]++
	call := e.state.RowCalls[id]
	if fixture.RowFailureRemaining > 0 {
		fixture.RowFailureRemaining--
		e.state.Shells[id] = fixture
		e.state.RowRequests = append(e.state.RowRequests, shellRequestRecord{ID: id, Status: fixture.RowStatusCode})
		_ = e.saveLocked()
		e.mu.Unlock()
		status := fixture.RowStatusCode
		if status < 400 {
			status = http.StatusInternalServerError
		}
		http.Error(w, "fixture row failure", status)
		return
	}
	effectiveOwner := fixture.Owner
	if fixture.WrongOwnerRows > 0 && call <= fixture.WrongOwnerRows {
		effectiveOwner = "ses_foreign_owner"
	}
	if fixture.MalformedRowRemaining > 0 {
		fixture.MalformedRowRemaining--
		e.state.Shells[id] = fixture
		e.state.RowRequests = append(e.state.RowRequests, shellRequestRecord{ID: id, Status: http.StatusOK})
		_ = e.saveLocked()
		e.mu.Unlock()
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("{broken"))
		return
	}
	e.state.RowRequests = append(e.state.RowRequests, shellRequestRecord{ID: id, Status: http.StatusOK})
	_ = e.saveLocked()
	e.mu.Unlock()
	writeShellJSON(w, map[string]any{"data": map[string]any{"id": id, "status": fixture.Status, "exit": fixture.Exit, "metadata": map[string]any{"sessionID": effectiveOwner}}})
}

func (e *shellEngine) output(w http.ResponseWriter, r *http.Request, id string) {
	id, _ = url.PathUnescape(id)
	cursor, _ := strconvParseUint(r.URL.Query().Get("cursor"))
	limit, _ := strconvParseUint(r.URL.Query().Get("limit"))
	e.mu.Lock()
	fixture, ok := e.state.Shells[id]
	status := http.StatusOK
	page, exists := fixture.Pages[fmt.Sprint(cursor)]
	if !exists {
		page = fixturePage{Cursor: cursor, Size: cursor}
	}
	if ok && fixture.FailOutputRemaining > 0 && fixture.FailOutputCursor == fmt.Sprint(cursor) {
		fixture.FailOutputRemaining--
		e.state.Shells[id] = fixture
		status = http.StatusInternalServerError
	}
	if ok && fixture.HoldOutput && fixture.HoldOutputCursor == fmt.Sprint(cursor) && status == http.StatusOK {
		status = http.StatusServiceUnavailable
	}
	delay := time.Duration(0)
	if ok && fixture.SlowCursor == fmt.Sprint(cursor) && !fixture.SlowUsed {
		fixture.SlowUsed = true
		delay = time.Duration(fixture.SlowDelayMs) * time.Millisecond
		e.state.Shells[id] = fixture
		e.state.SlowStarted[id] = true
	}
	e.state.OutputRequests = append(e.state.OutputRequests, shellRequestRecord{ID: id, Cursor: cursor, Limit: limit, Status: status})
	_ = e.saveLocked()
	e.mu.Unlock()
	if delay > 0 {
		e.emit("session.retry.scheduled", map[string]any{"sessionID": nativeRootSession, "attempt": 1, "message": "fixture while shell output is pending", "at": time.Now().UnixMilli() + delay.Milliseconds()})
		select {
		case <-r.Context().Done():
			return
		case <-time.After(delay):
		}
	}
	if status != http.StatusOK {
		http.Error(w, "fixture output failure", status)
		return
	}
	if !ok {
		http.NotFound(w, r)
		return
	}
	if fixture.WrongOwnerRows > 0 && e.ownerWasForeign(id) {
		page.Output = "FOREIGN-OUTPUT-MUST-NOT-EMIT"
	}
	writeShellJSON(w, map[string]any{"data": page})
}

func (e *shellEngine) ownerWasForeign(id string) bool {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.state.RowCalls[id] <= e.state.Shells[id].WrongOwnerRows
}

func (e *shellEngine) deleteShell(w http.ResponseWriter, r *http.Request) {
	id, _ := url.PathUnescape(strings.TrimPrefix(r.URL.Path, "/api/shell/"))
	e.mu.Lock()
	fixture, ok := e.state.Shells[id]
	if ok {
		e.state.DeleteRequests = append(e.state.DeleteRequests, id)
		if fixture.DeleteStatusCode == 0 {
			fixture.Status = "killed"
			fixture.Exit = 137
			e.state.Shells[id] = fixture
		}
		_ = e.saveLocked()
	}
	status := fixture.DeleteStatusCode
	e.mu.Unlock()
	if !ok {
		http.NotFound(w, r)
		return
	}
	if status != 0 {
		http.Error(w, "fixture stop failure", status)
		return
	}
	writeShellJSON(w, map[string]any{"data": map[string]any{}})
}

func (e *shellEngine) stream(w http.ResponseWriter, r *http.Request) {
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

func (e *shellEngine) emit(kind string, data map[string]any) {
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

func (e *shellEngine) appendMessages(session string, rows ...map[string]any) error {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.ensureStateMaps()
	e.state.Messages[session] = append(e.state.Messages[session], rows...)
	return e.saveLocked()
}

func (e *shellEngine) messages(w http.ResponseWriter, session string) {
	e.mu.Lock()
	rows := append([]map[string]any(nil), e.state.Messages[session]...)
	e.mu.Unlock()
	for i, j := 0, len(rows)-1; i < j; i, j = i+1, j-1 {
		rows[i], rows[j] = rows[j], rows[i]
	}
	writeShellJSON(w, map[string]any{"data": rows, "cursor": nil})
}

func (e *shellEngine) prompt(w http.ResponseWriter, r *http.Request) {
	var body struct {
		ID   string `json:"id"`
		Text string `json:"text"`
	}
	if json.NewDecoder(r.Body).Decode(&body) != nil || body.ID == "" {
		http.Error(w, "invalid prompt", 400)
		return
	}
	now := time.Now().UnixMilli()
	user := map[string]any{"id": "user-" + body.ID, "type": "user", "time": map[string]any{"created": now}, "content": []any{map[string]any{"type": "text", "text": body.Text}}}
	parts := []any{}
	if body.Text == "shellwork-suite" {
		e.emit("shell.created", map[string]any{"id": "unknown-shell", "sessionID": "ses_foreign_owner"})
		e.emit("shell.exited", map[string]any{"id": "unknown-shell", "sessionID": "ses_foreign_owner", "exit": 9})
		e.emit("shell.deleted", map[string]any{"id": "unknown-shell", "sessionID": "ses_foreign_owner"})
		parts = append(parts, e.emitTool(nativeRootSession, "activity-foreground", "bash", map[string]any{"command": "foreground fixture only", "cwd": "fixture"}, "", "ordinary tool result"))
		for _, row := range []struct{ id, activity, command, shell string }{
			{"activity-poll", "activity-poll", "fixture poll job", "poll-shell"},
			{"activity-foreign", "activity-foreign", "fixture foreign owner", "foreign-shell"},
			{"activity-delete", "activity-delete", "fixture delete job", "delete-shell"},
			{"activity-missing", "activity-missing", "fixture missing job", "missing-shell"},
			{"activity-malformed", "activity-malformed", "fixture malformed row", "malformed-shell"},
			{"activity-row500", "activity-row500", "fixture row error", "row500-shell"},
			{"activity-stop-root", "activity-stop-root", "fixture root stop", "root-stop-shell"},
			{"activity-stop-fail", "activity-stop-fail", "fixture failed stop", "failed-stop-shell"},
			{"activity-complete", "activity-complete", "fixture complete", "complete-shell"},
			{"activity-tail", "activity-tail", "fixture tail bound", "tail-shell"},
		} {
			parts = append(parts, e.emitTool(nativeRootSession, row.id, "bash", map[string]any{"command": row.command, "cwd": "fixture-root-cwd"}, row.shell, "provider tool returned"))
		}
		childInput := map[string]any{"sessionID": nativeChildSession, "prompt": "fixture child; no command execution"}
		parts = append(parts, e.emitTool(nativeRootSession, "activity-task", "task", childInput, "", "child fixture started"))
		e.emit("session.created", map[string]any{"id": nativeChildSession, "parentID": nativeRootSession, "title": "fixture child"})
		e.emit("session.execution.started", map[string]any{"sessionID": nativeChildSession})
		childPart := e.emitTool(nativeChildSession, "activity-child-stop", "bash", map[string]any{"command": "fixture child stop", "cwd": "fixture-child-cwd"}, "child-stop-shell", "child tool returned")
		childAssistant := map[string]any{"id": "assistant-child-shellwork", "type": "assistant", "time": map[string]any{"created": now, "completed": now + 1}, "content": []any{childPart}}
		if err := e.appendMessages(nativeChildSession, childAssistant); err != nil {
			http.Error(w, "child transcript write failed", 500)
			return
		}
		e.emit("session.execution.succeeded", map[string]any{"sessionID": nativeChildSession})
	}
	e.emit("session.execution.succeeded", map[string]any{"sessionID": nativeRootSession})
	assistant := map[string]any{"id": "assistant-shellwork-" + body.ID, "type": "assistant", "time": map[string]any{"created": now, "completed": now + 1}, "content": parts}
	if err := e.appendMessages(nativeRootSession, user, assistant); err != nil {
		http.Error(w, "transcript write failed", 500)
		return
	}
	writeShellJSON(w, map[string]any{"data": map[string]any{}})
}

func (e *shellEngine) emitTool(session, id, name string, input map[string]any, shellID, output string) map[string]any {
	e.emit("session.tool.input.started", map[string]any{"sessionID": session, "id": id, "name": name})
	e.emit("session.tool.called", map[string]any{"sessionID": session, "id": id, "input": input})
	fields := map[string]any{"sessionID": session, "id": id, "content": output}
	metadata := map[string]any{}
	if shellID != "" {
		metadata["shellID"] = shellID
		fields["metadata"] = metadata
	}
	e.emit("session.tool.success", fields)
	state := map[string]any{"status": "completed", "title": name, "input": input, "output": output}
	part := map[string]any{"type": "tool", "id": id, "callID": "call-" + id, "tool": name, "state": state}
	if shellID != "" {
		part["metadata"] = metadata
	}
	return part
}

func writeShellJSON(w http.ResponseWriter, value any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(value)
}

func strconvParseUint(value string) (uint64, error) {
	var out uint64
	_, err := fmt.Sscan(value, &out)
	return out, err
}

// Command fixturedaemon is a test-only stand-in for fintwind-daemon.
//
// It speaks the real wire (hello with protocol 42 plus nonzero environment
// and instance UUIDs, sequenced driver events, replay cursors) against an
// isolated data directory, so the real e2e-built app window can drive a
// deterministic stream: connected, turnStarted, textStarted, textDelta,
// reasoningStarted, reasoningDelta, reasoningEnded, activity, permission,
// userInputRequested, textEnded, turnFinished. Scope is actualUI plus
// fixtureDaemon, never a real provider.
package main

import (
	"crypto/rand"
	"crypto/subtle"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/daemon"

	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

const protocolVersion = daemon.ProtocolVersion

const (
	streamFullText = "fixture-stream-正文-7391-complete"
	streamReason   = "fixture-reason-part-2"
)

var streamChunks = []string{"fixture-stream-", "正文-", "7391-", "complete"}

var streamOrder = []string{
	"connected", "turnStarted", "textStarted", "textDelta",
	"reasoningStarted", "reasoningDelta", "reasoningEnded",
	"activity", "permission", "userInputRequested",
	"textEnded", "turnFinished",
}

type replayCursor struct {
	SessionID string `json:"sessionId"`
	RuntimeID string `json:"runtimeId"`
	Epoch     string `json:"epoch"`
	Sequence  uint64 `json:"sequence"`
}

type sequencedEvent struct {
	SessionID string          `json:"sessionId"`
	RuntimeID string          `json:"runtimeId"`
	Epoch     string          `json:"epoch"`
	Sequence  uint64          `json:"sequence"`
	Kind      string          `json:"kind"`
	Payload   json.RawMessage `json:"payload"`
}

type fixture struct {
	dataDir string
	token   string

	environmentID string
	instanceID    string

	mu       sync.Mutex
	projects []json.RawMessage
	sessions map[string]json.RawMessage
	runtimes map[string]string
	journal  map[string][]sequencedEvent
	seqs     map[string]uint64
	pending  map[string]chan string
	// Admission registry: frozen submission identities keyed by turn ID.
	submissions         map[string]*submissionEntry
	acceptHoldUntilMs   int64
	dispatchHoldUntilMs int64

	connsMu sync.Mutex
	conns   map[*daemonConn]struct{}

	streamMu        sync.Mutex
	compactRequests uint64
}

type daemonConn struct {
	ws      *websocket.Conn
	writeMu sync.Mutex
}

func main() {
	bind := flag.String("bind", "127.0.0.1:0", "loopback bind address")
	dataDir := flag.String("data-dir", "", "isolated fixture data directory")
	parentPID := flag.Int("parent-pid", 0, "parent process id to watch")
	flag.Parse()

	token := os.Getenv("FINTWIND_DAEMON_TOKEN")
	if token == "" {
		fmt.Fprintln(os.Stderr, "fixturedaemon: FINTWIND_DAEMON_TOKEN is missing")
		os.Exit(2)
	}
	if *dataDir == "" {
		fmt.Fprintln(os.Stderr, "fixturedaemon: --data-dir is required")
		os.Exit(2)
	}
	if err := checkLoopback(*bind); err != nil {
		fmt.Fprintln(os.Stderr, "fixturedaemon: "+err.Error())
		os.Exit(2)
	}
	if err := os.MkdirAll(*dataDir, 0o755); err != nil {
		fmt.Fprintln(os.Stderr, "fixturedaemon: create data dir: "+err.Error())
		os.Exit(2)
	}

	fx := &fixture{
		dataDir:     *dataDir,
		token:       token,
		sessions:    map[string]json.RawMessage{},
		runtimes:    map[string]string{},
		journal:     map[string][]sequencedEvent{},
		seqs:        map[string]uint64{},
		pending:     map[string]chan string{},
		conns:       map[*daemonConn]struct{}{},
		submissions: map[string]*submissionEntry{},
	}
	fx.environmentID = fx.loadOrCreateEnvironmentID()
	fx.instanceID = uuid.NewString()
	fx.loadTaskState()
	fx.loadSubmissions()

	listener, err := net.Listen("tcp", *bind)
	if err != nil {
		fmt.Fprintln(os.Stderr, "fixturedaemon: bind: "+err.Error())
		os.Exit(2)
	}
	address := listener.Addr().String()

	controlToken := randomHex(16)
	controlListener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		fmt.Fprintln(os.Stderr, "fixturedaemon: control bind: "+err.Error())
		os.Exit(2)
	}
	controlAddress := controlListener.Addr().String()
	controlInfo, _ := json.Marshal(map[string]string{"address": controlAddress, "token": controlToken})
	_ = os.WriteFile(filepath.Join(*dataDir, "fixture-control.json"), append(controlInfo, '\n'), 0o600)

	if *parentPID > 0 {
		watchParent(*parentPID)
	}

	go fx.serveControl(controlListener, controlToken)
	go func() {
		_ = http.Serve(listener, fx.wsHandler())
	}()

	ready, _ := json.Marshal(map[string]any{
		"type":            "ready",
		"address":         address,
		"protocolVersion": protocolVersion,
		"pid":             os.Getpid(),
		"environmentId":   fx.environmentID,
		"instanceId":      fx.instanceID,
	})
	fmt.Println(string(ready))
	fmt.Fprintf(os.Stderr, "fixturedaemon: ws=%s control=%s env=%s\n", address, controlAddress, shortID(fx.environmentID))

	select {}
}

func checkLoopback(bind string) error {
	host := bind
	if parsed, _, err := net.SplitHostPort(bind); err == nil {
		host = parsed
	}
	host = strings.Trim(host, "[]")
	if strings.EqualFold(host, "localhost") {
		return nil
	}
	if ip := net.ParseIP(host); ip != nil && ip.IsLoopback() {
		return nil
	}
	return fmt.Errorf("refusing non-loopback bind %q: loopback only", bind)
}

func shortID(id string) string {
	if len(id) > 8 {
		return id[:8]
	}
	return id
}

func randomHex(n int) string {
	buffer := make([]byte, n)
	if _, err := rand.Read(buffer); err != nil {
		panic(err)
	}
	return hex.EncodeToString(buffer)
}

func (fx *fixture) loadOrCreateEnvironmentID() string {
	path := filepath.Join(fx.dataDir, "environment_id")
	if raw, err := os.ReadFile(path); err == nil {
		if id, err := uuid.Parse(strings.TrimSpace(string(raw))); err == nil && id != uuid.Nil {
			return id.String()
		}
	}
	id := uuid.NewString()
	_ = os.WriteFile(path, []byte(id+"\n"), 0o644)
	return id
}

func (fx *fixture) statePath() string {
	return filepath.Join(fx.dataDir, "fixture-task-state.json")
}

func (fx *fixture) loadTaskState() {
	raw, err := os.ReadFile(fx.statePath())
	if err != nil {
		return
	}
	var stored struct {
		Projects []json.RawMessage `json:"projects"`
		Sessions []json.RawMessage `json:"sessions"`
	}
	if err := json.Unmarshal(raw, &stored); err != nil {
		return
	}
	fx.projects = stored.Projects
	for _, session := range stored.Sessions {
		if id := sessionIDOf(session); id != "" {
			fx.sessions[id] = session
		}
	}
}

func (fx *fixture) persistTaskState() {
	stored := struct {
		Projects []json.RawMessage `json:"projects"`
		Sessions []json.RawMessage `json:"sessions"`
	}{Projects: fx.projects}
	for _, session := range fx.sessions {
		stored.Sessions = append(stored.Sessions, session)
	}
	raw, err := json.MarshalIndent(stored, "", "  ")
	if err != nil {
		return
	}
	_ = os.WriteFile(fx.statePath(), append(raw, '\n'), 0o644)
}

func sessionIDOf(raw json.RawMessage) string {
	var holder struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(raw, &holder); err != nil {
		return ""
	}
	return holder.ID
}

func (fx *fixture) wsHandler() http.Handler {
	upgrader := websocket.Upgrader{CheckOrigin: func(*http.Request) bool { return true }}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1" {
			http.NotFound(w, r)
			return
		}
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		fx.serveConn(conn)
	})
}

func (fx *fixture) serveConn(ws *websocket.Conn) {
	_ = ws.SetReadDeadline(time.Now().Add(30 * time.Second))
	var hello struct {
		Type            string         `json:"type"`
		ProtocolVersion int            `json:"protocolVersion"`
		Token           string         `json:"token"`
		ResumeFrom      []replayCursor `json:"resumeFrom"`
	}
	if err := ws.ReadJSON(&hello); err != nil {
		_ = ws.Close()
		return
	}
	if hello.Type != "hello" || subtle.ConstantTimeCompare([]byte(hello.Token), []byte(fx.token)) != 1 {
		_ = ws.WriteJSON(map[string]any{"type": "rejected", "message": "bad token"})
		_ = ws.Close()
		return
	}
	if hello.ProtocolVersion != protocolVersion {
		_ = ws.WriteJSON(map[string]any{"type": "rejected", "message": "protocol mismatch"})
		_ = ws.Close()
		return
	}
	dc := &daemonConn{ws: ws}
	fx.connsMu.Lock()
	fx.conns[dc] = struct{}{}
	fx.connsMu.Unlock()
	defer func() {
		fx.connsMu.Lock()
		delete(fx.conns, dc)
		fx.connsMu.Unlock()
		_ = ws.Close()
	}()

	_ = dc.writeJSON(map[string]any{
		"type":            "hello",
		"protocolVersion": protocolVersion,
		"daemonVersion":   "fixture-1",
		"environmentId":   fx.environmentID,
		"instanceId":      fx.instanceID,
	})
	_ = ws.SetReadDeadline(time.Time{})
	for _, event := range fx.replayFor(hello.ResumeFrom) {
		if err := dc.writeJSON(eventMessage(event)); err != nil {
			return
		}
	}
	fmt.Fprintf(os.Stderr, "fixturedaemon: hello accepted, replayed\n")

	for {
		var frame struct {
			Type      string          `json:"type"`
			RequestID string          `json:"requestId"`
			SessionID string          `json:"sessionId"`
			RuntimeID string          `json:"runtimeId"`
			Command   json.RawMessage `json:"command"`
		}
		if err := ws.ReadJSON(&frame); err != nil {
			return
		}
		switch frame.Type {
		case "request":
			fx.dispatch(dc, frame.RequestID, frame.SessionID, frame.RuntimeID, frame.Command)
		case "browserPublish", "browserHost", "browserResult", "browserCancel", "shutdown":
			continue
		default:
			fx.respondError(dc, frame.RequestID, "fixture: unknown frame type")
		}
	}
}

func (dc *daemonConn) writeJSON(value any) error {
	dc.writeMu.Lock()
	defer dc.writeMu.Unlock()
	_ = dc.ws.SetWriteDeadline(time.Now().Add(15 * time.Second))
	return dc.ws.WriteJSON(value)
}

func eventMessage(event sequencedEvent) map[string]any {
	return map[string]any{
		"type":      "event",
		"sessionId": event.SessionID,
		"runtimeId": event.RuntimeID,
		"epoch":     event.Epoch,
		"sequence":  event.Sequence,
		"event":     map[string]any{"kind": event.Kind, "payload": event.Payload},
	}
}

func (fx *fixture) replayFor(cursors []replayCursor) []sequencedEvent {
	fx.mu.Lock()
	defer fx.mu.Unlock()
	applied := map[string]uint64{}
	for _, cursor := range cursors {
		if cursor.Epoch != fx.instanceID {
			continue
		}
		key := cursor.SessionID + "\x00" + cursor.RuntimeID
		if cursor.Sequence > applied[key] {
			applied[key] = cursor.Sequence
		}
	}
	var out []sequencedEvent
	for key, events := range fx.journal {
		mark := applied[key]
		for _, event := range events {
			if event.Sequence > mark {
				out = append(out, event)
			}
		}
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].SessionID != out[j].SessionID {
			return out[i].SessionID < out[j].SessionID
		}
		if out[i].RuntimeID != out[j].RuntimeID {
			return out[i].RuntimeID < out[j].RuntimeID
		}
		return out[i].Sequence < out[j].Sequence
	})
	return out
}

func (fx *fixture) emit(sessionID, runtimeID, kind string, payload any) sequencedEvent {
	raw, _ := json.Marshal(payload)
	fx.mu.Lock()
	key := sessionID + "\x00" + runtimeID
	fx.seqs[key]++
	event := sequencedEvent{
		SessionID: sessionID,
		RuntimeID: runtimeID,
		Epoch:     fx.instanceID,
		Sequence:  fx.seqs[key],
		Kind:      kind,
		Payload:   json.RawMessage(raw),
	}
	fx.journal[key] = append(fx.journal[key], event)
	fx.mu.Unlock()

	fx.connsMu.Lock()
	conns := make([]*daemonConn, 0, len(fx.conns))
	for dc := range fx.conns {
		conns = append(conns, dc)
	}
	fx.connsMu.Unlock()
	message := eventMessage(event)
	for _, dc := range conns {
		if err := dc.writeJSON(message); err != nil {
			fx.connsMu.Lock()
			delete(fx.conns, dc)
			fx.connsMu.Unlock()
			_ = dc.ws.Close()
		}
	}
	return event
}

func (fx *fixture) respondOK(dc *daemonConn, requestID string, payload any) {
	_ = dc.writeJSON(map[string]any{
		"type":      "response",
		"requestId": requestID,
		"outcome":   map[string]any{"status": "ok", "payload": payload},
	})
}

func (fx *fixture) respondError(dc *daemonConn, requestID, message string) {
	_ = dc.writeJSON(map[string]any{
		"type":      "response",
		"requestId": requestID,
		"outcome":   map[string]any{"status": "error", "error": map[string]any{"message": message}},
	})
}

func (fx *fixture) dispatch(dc *daemonConn, requestID, sessionID, runtimeID string, rawCommand json.RawMessage) {
	var command struct {
		Type string `json:"type"`
	}
	if err := json.Unmarshal(rawCommand, &command); err != nil {
		fx.respondError(dc, requestID, "fixture: bad command")
		return
	}
	fmt.Fprintf(os.Stderr, "fixturedaemon: command %s session=%s\n", command.Type, shortID(sessionID))
	switch command.Type {
	case "loadTaskState":
		fx.mu.Lock()
		projects := append([]json.RawMessage{}, fx.projects...)
		sessions := make([]json.RawMessage, 0, len(fx.sessions))
		for _, session := range fx.sessions {
			sessions = append(sessions, session)
		}
		fx.mu.Unlock()
		if projects == nil {
			projects = []json.RawMessage{}
		}
		if sessions == nil {
			sessions = []json.RawMessage{}
		}
		fx.respondOK(dc, requestID, map[string]any{
			"type": "taskState", "projects": projects, "sessions": sessions,
			"defaultCwd": fx.dataDir, "projectlessRoot": fx.dataDir,
		})
	case "saveTaskState", "saveTaskStateCheckpoint":
		checkpoint := command.Type == "saveTaskStateCheckpoint"
		var save struct {
			Projects []json.RawMessage `json:"projects"`
			Sessions []json.RawMessage `json:"sessions"`
			// Autosave checkpoints name list-only sessions here; a list-only
			// edit must not erase the stored transcript.
			Skeletons []string `json:"skeletonSessionIds"`
		}
		if err := json.Unmarshal(rawCommand, &save); err != nil {
			fx.respondError(dc, requestID, "fixture: bad save payload")
			return
		}
		skeleton := map[string]bool{}
		if checkpoint {
			for _, id := range save.Skeletons {
				skeleton[id] = true
			}
		}
		fx.mu.Lock()
		fx.projects = mergeByID(fx.projects, save.Projects)
		for _, session := range save.Sessions {
			id := sessionIDOf(session)
			if id == "" {
				continue
			}
			if stored, ok := fx.sessions[id]; ok && skeleton[id] {
				fx.sessions[id] = fx.reconcileReceiptsOnSaveLocked(mergeListFields(stored, session))
				continue
			}
			fx.sessions[id] = fx.reconcileReceiptsOnSaveLocked(session)
		}
		fx.persistTaskState()
		fx.mu.Unlock()
		// The autosave reply carries no transcript: the client already owns
		// what it sent, and skeletons never carried detail.
		fx.respondOK(dc, requestID, map[string]any{"type": "taskStateSaved", "sessions": []any{}})
	case "hydrateSession":
		var hydrate struct {
			SessionID string `json:"sessionId"`
		}
		_ = json.Unmarshal(rawCommand, &hydrate)
		target := sessionID
		if hydrate.SessionID != "" && hydrate.SessionID != "00000000-0000-0000-0000-000000000000" {
			target = hydrate.SessionID
		}
		fx.mu.Lock()
		session, ok := fx.sessions[target]
		if ok {
			session = fx.reconcileReceiptsOnSaveLocked(session)
			fx.sessions[target] = session
		}
		fx.mu.Unlock()
		var value any
		if ok {
			value = session
		}
		fx.respondOK(dc, requestID, map[string]any{"type": "session", "session": value})
	case "acceptSubmission":
		go fx.handleAccept(dc, requestID, sessionID, rawCommand)
	case "dispatchSubmission":
		go fx.handleDispatch(dc, requestID, sessionID, runtimeID, rawCommand)
	case "abandonSubmission":
		fx.handleAbandon(dc, requestID, sessionID, rawCommand)
	case "attachSession":
		fx.mu.Lock()
		runtime, ok := fx.runtimes[sessionID]
		fx.mu.Unlock()
		var value any
		if ok {
			value = runtime
		}
		fx.respondOK(dc, requestID, map[string]any{"type": "sessionRuntime", "runtimeId": value, "supportsSteer": true})
	case "start":
		runtime := runtimeID
		if runtime == "" || runtime == "00000000-0000-0000-0000-000000000000" {
			runtime = uuid.NewString()
		}
		fx.mu.Lock()
		fx.runtimes[sessionID] = runtime
		fx.mu.Unlock()
		fx.emit(sessionID, runtime, "connected", map[string]any{})
		fx.respondOK(dc, requestID, map[string]any{"type": "started", "supportsSteer": true})
	case "prompt":
		var prompt struct {
			Prompt string `json:"prompt"`
		}
		_ = json.Unmarshal(rawCommand, &prompt)
		fx.respondOK(dc, requestID, map[string]any{"type": "ack"})
		go fx.streamTurn(sessionID, runtimeID, prompt.Prompt)
	case "steer":
		fx.respondOK(dc, requestID, map[string]any{"type": "ack"})
	case "respond":
		var answer struct {
			RequestID string `json:"requestId"`
			OptionID  string `json:"optionId"`
		}
		_ = json.Unmarshal(rawCommand, &answer)
		fx.resolvePending(answer.RequestID, answer.OptionID)
		fx.respondOK(dc, requestID, map[string]any{"type": "ack"})
	case "respondUserInput", "cancelUserInput":
		var answer struct {
			RequestID string `json:"requestId"`
		}
		_ = json.Unmarshal(rawCommand, &answer)
		fx.resolvePending(answer.RequestID, "answered")
		fx.respondOK(dc, requestID, map[string]any{"type": "ack"})
	case "cancel":
		fx.cancelAdmission(sessionID, runtimeID)
		fx.respondOK(dc, requestID, map[string]any{"type": "ack"})
	case "probeProvider":
		fx.respondOK(dc, requestID, map[string]any{
			"type": "providerProbe",
			"probe": map[string]any{
				"installed": true,
				"path":      nil,
				"models": []any{map[string]any{
					"id": "fixture/context-model", "name": "Fixture Context Model", "sub_provider": "Fixture",
					"context_windows": []any{
						map[string]any{"id": "80k", "label": "80K tokens"},
						map[string]any{"id": "100k", "label": "100K tokens"},
					},
					"default_context_window": "100k",
				}},
				"agentPresets": []any{},
			},
		})
	case "applyOptions":
		var options struct {
			Options map[string]any `json:"options"`
		}
		if err := json.Unmarshal(rawCommand, &options); err != nil {
			fx.respondError(dc, requestID, "fixture: bad applyOptions payload")
			return
		}
		if window, present := options.Options["contextWindow"]; present {
			fx.mu.Lock()
			if raw, ok := fx.sessions[sessionID]; ok {
				var session map[string]any
				if json.Unmarshal(raw, &session) == nil {
					session["context_window"] = window
					if saved, err := json.Marshal(session); err == nil {
						fx.sessions[sessionID] = saved
						fx.persistTaskState()
					}
				}
			}
			fx.mu.Unlock()
		}
		fx.respondOK(dc, requestID, map[string]any{"type": "ack"})
	case "compactSession":
		fx.mu.Lock()
		fx.compactRequests++
		fx.mu.Unlock()
		fx.respondOK(dc, requestID, map[string]any{"type": "ack"})
	case "refreshBackgroundWork",
		"closeSession", "fork", "removeSession", "removeProject",
		"stopBackgroundWork", "compactChildSession":
		fx.respondOK(dc, requestID, map[string]any{"type": "ack"})
	case "getSettings":
		fx.respondOK(dc, requestID, map[string]any{"type": "settings", "settings": map[string]any{}})
	default:
		fx.respondError(dc, requestID, "fixture: unsupported command "+command.Type)
	}
}

// mergeListFields folds a list-only checkpoint stub into the stored session:
// list-facing fields move, the stored transcript (messages, turns, blocks,
// cursors such as runtime_event_cursor) stays.
func mergeListFields(stored, incoming json.RawMessage) json.RawMessage {
	var base map[string]any
	if err := json.Unmarshal(stored, &base); err != nil {
		return incoming
	}
	var stub map[string]any
	if err := json.Unmarshal(incoming, &stub); err != nil {
		return stored
	}
	for _, key := range []string{"title", "auto_title", "model", "status",
		"updated_at", "last_reply_at", "archived", "runtime_mode",
		"interaction_mode", "project_id", "provider"} {
		if value, ok := stub[key]; ok {
			base[key] = value
		}
	}
	raw, err := json.Marshal(base)
	if err != nil {
		return stored
	}
	return json.RawMessage(raw)
}

func mergeByID(current, incoming []json.RawMessage) []json.RawMessage {
	index := map[string]int{}
	for i, raw := range current {
		if id := sessionIDOf(raw); id != "" {
			index[id] = i
		}
	}
	for _, raw := range incoming {
		if id := sessionIDOf(raw); id != "" {
			if at, ok := index[id]; ok {
				current[at] = raw
				continue
			}
			index[id] = len(current)
		}
		current = append(current, raw)
	}
	if current == nil {
		current = []json.RawMessage{}
	}
	return current
}

func (fx *fixture) resolvePending(requestID, value string) {
	fx.mu.Lock()
	waiter, ok := fx.pending[requestID]
	if ok {
		delete(fx.pending, requestID)
	}
	fx.mu.Unlock()
	if ok {
		select {
		case waiter <- value:
		default:
		}
	}
}

func (fx *fixture) waitPending(requestID string, timeout time.Duration) {
	waiter := make(chan string, 1)
	fx.mu.Lock()
	fx.pending[requestID] = waiter
	fx.mu.Unlock()
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case <-waiter:
	case <-timer.C:
		fx.mu.Lock()
		delete(fx.pending, requestID)
		fx.mu.Unlock()
	}
}

func pause() {
	time.Sleep(40 * time.Millisecond)
}

func (fx *fixture) streamTurn(sessionID, runtimeID, prompt string) {
	fx.streamMu.Lock()
	defer fx.streamMu.Unlock()

	turnID := uuid.NewString()
	permissionID := uuid.NewString()
	questionID := uuid.NewString()

	fx.emit(sessionID, runtimeID, "turnStarted", map[string]any{})
	pause()
	fx.emit(sessionID, runtimeID, "textStarted", map[string]any{})
	for _, chunk := range streamChunks {
		pause()
		fx.emit(sessionID, runtimeID, "textDelta", chunk)
	}
	pause()
	fx.emit(sessionID, runtimeID, "reasoningStarted", map[string]any{})
	pause()
	fx.emit(sessionID, runtimeID, "reasoningDelta", streamReason)
	pause()
	fx.emit(sessionID, runtimeID, "reasoningEnded", streamReason)
	pause()
	fx.emit(sessionID, runtimeID, "activity", map[string]any{
		"id": "fixture-activity-1", "kind": "tool",
		"title": "fixture tool call", "detail": "echo ok", "complete": true,
	})
	pause()
	fx.emit(sessionID, runtimeID, "permission", map[string]any{
		"requestId": permissionID, "title": "Fixture permission",
		"detail": "Allow the fixture step to continue?",
		"options": []any{
			map[string]any{"id": "allow", "label": "Allow", "allow": true},
			map[string]any{"id": "deny", "label": "Deny", "allow": false},
		},
	})
	fx.waitPending(permissionID, 20*time.Second)
	pause()
	fx.emit(sessionID, runtimeID, "userInputRequested", map[string]any{
		"requestId": questionID,
		"questions": []any{map[string]any{
			"id": "q1", "header": "Fixture", "question": "Fixture asks: pick one?",
			"options": []any{
				map[string]any{"label": "Option A"},
				map[string]any{"label": "Option B"},
			},
			"multiSelect": false,
		}},
	})
	fx.waitPending(questionID, 20*time.Second)
	pause()
	fx.emit(sessionID, runtimeID, "textEnded", streamFullText)
	pause()
	fx.emit(sessionID, runtimeID, "turnFinished", map[string]any{"success": true})

	fx.appendTranscript(sessionID, turnID, prompt)
}

func (fx *fixture) appendTranscript(sessionID, turnID, prompt string) {
	fx.mu.Lock()
	defer fx.mu.Unlock()
	raw, ok := fx.sessions[sessionID]
	if !ok {
		return
	}
	var session map[string]any
	if err := json.Unmarshal(raw, &session); err != nil {
		return
	}
	now := uint64(time.Now().Unix())
	messages, _ := session["messages"].([]any)
	// The app checkpoints the full snapshot carrying the user message
	// before attach/start/prompt, so the user text is usually already
	// stored: appending it again would duplicate the turn's input. A
	// headless prompt with no prior checkpoint still needs it.
	alreadyStored := false
	for _, item := range messages {
		message, _ := item.(map[string]any)
		if message == nil || message["role"] != "user" {
			continue
		}
		if text, _ := message["content"].(string); text == prompt {
			alreadyStored = true
			break
		}
	}
	if !alreadyStored {
		messages = append(messages, map[string]any{
			"id": "fixture-user-" + turnID, "turn_id": turnID,
			"role": "user", "content": prompt, "created_at": now,
		})
	}
	assistantMessage := map[string]any{
		"id": "fixture-assistant-" + turnID, "turn_id": turnID,
		"role": "assistant", "content": streamFullText, "created_at": now,
	}
	session["messages"] = append(messages, assistantMessage)
	turns, _ := session["turns"].([]any)
	session["turns"] = append(turns, map[string]any{
		"id": turnID, "status": "completed",
		"started_at": now, "completed_at": now,
	})
	session["status"] = "idle"
	session["updated_at"] = now
	updated, err := json.Marshal(session)
	if err != nil {
		return
	}
	fx.sessions[sessionID] = json.RawMessage(updated)
	fx.persistTaskState()
}

func (fx *fixture) serveControl(listener net.Listener, token string) {
	mux := http.NewServeMux()
	guard := func(next func(http.ResponseWriter, *http.Request)) func(http.ResponseWriter, *http.Request) {
		return func(w http.ResponseWriter, r *http.Request) {
			if subtle.ConstantTimeCompare([]byte(bearer(r)), []byte(token)) != 1 {
				http.Error(w, "forbidden", http.StatusForbidden)
				return
			}
			next(w, r)
		}
	}
	mux.HandleFunc("/health", guard(func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, map[string]any{"ok": true, "epoch": fx.instanceID})
	}))
	mux.HandleFunc("/disconnect", guard(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		fx.connsMu.Lock()
		conns := make([]*daemonConn, 0, len(fx.conns))
		for dc := range fx.conns {
			conns = append(conns, dc)
		}
		fx.connsMu.Unlock()
		for _, dc := range conns {
			_ = dc.ws.Close()
		}
		fmt.Fprintf(os.Stderr, "fixturedaemon: control disconnect, closed=%d journal kept\n", len(conns))
		writeJSON(w, map[string]any{"closed": len(conns)})
	}))
	mux.HandleFunc("/journal", guard(func(w http.ResponseWriter, r *http.Request) {
		session := r.URL.Query().Get("sessionId")
		fx.mu.Lock()
		var events []map[string]any
		for key, stored := range fx.journal {
			for _, event := range stored {
				if session != "" && event.SessionID != session {
					continue
				}
				_ = key
				events = append(events, map[string]any{
					"sessionId": event.SessionID, "runtimeId": event.RuntimeID,
					"sequence": event.Sequence, "kind": event.Kind,
				})
			}
		}
		fx.mu.Unlock()
		sort.Slice(events, func(i, j int) bool {
			return events[i]["sequence"].(uint64) < events[j]["sequence"].(uint64)
		})
		writeJSON(w, map[string]any{"events": events})
	}))
	mux.HandleFunc("/compactions", guard(func(w http.ResponseWriter, r *http.Request) {
		fx.mu.Lock()
		count := fx.compactRequests
		fx.mu.Unlock()
		writeJSON(w, map[string]any{"count": count})
	}))
	mux.HandleFunc("/event", guard(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		var event struct {
			SessionID string          `json:"sessionId"`
			RuntimeID string          `json:"runtimeId"`
			Kind      string          `json:"kind"`
			Payload   json.RawMessage `json:"payload"`
		}
		if err := json.NewDecoder(r.Body).Decode(&event); err != nil || event.SessionID == "" || event.RuntimeID == "" || event.Kind == "" || !json.Valid(event.Payload) {
			http.Error(w, "invalid event payload", http.StatusBadRequest)
			return
		}
		fx.emit(event.SessionID, event.RuntimeID, event.Kind, json.RawMessage(event.Payload))
		writeJSON(w, map[string]any{"accepted": true})
	}))
	mux.HandleFunc("/submissions", guard(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet {
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		writeJSON(w, map[string]any{"submissions": fx.submissionStats(r.URL.Query().Get("sessionId"))})
	}))
	mux.HandleFunc("/holds", guard(func(w http.ResponseWriter, r *http.Request) {
		switch r.Method {
		case http.MethodGet:
			fx.mu.Lock()
			accept, dispatch := fx.acceptHoldUntilMs, fx.dispatchHoldUntilMs
			fx.mu.Unlock()
			writeJSON(w, map[string]any{
				"acceptHoldUntilMs": accept, "dispatchHoldUntilMs": dispatch,
				"nowMs": time.Now().UnixMilli(),
			})
		case http.MethodPost:
			var holds struct {
				AcceptHoldUntilMs   *int64 `json:"acceptHoldUntilMs"`
				DispatchHoldUntilMs *int64 `json:"dispatchHoldUntilMs"`
			}
			if err := json.NewDecoder(r.Body).Decode(&holds); err != nil {
				http.Error(w, "bad holds payload", http.StatusBadRequest)
				return
			}
			fx.mu.Lock()
			if holds.AcceptHoldUntilMs != nil {
				fx.acceptHoldUntilMs = *holds.AcceptHoldUntilMs
			}
			if holds.DispatchHoldUntilMs != nil {
				fx.dispatchHoldUntilMs = *holds.DispatchHoldUntilMs
			}
			accept, dispatch := fx.acceptHoldUntilMs, fx.dispatchHoldUntilMs
			fx.mu.Unlock()
			writeJSON(w, map[string]any{
				"acceptHoldUntilMs": accept, "dispatchHoldUntilMs": dispatch,
				"nowMs": time.Now().UnixMilli(),
			})
		default:
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		}
	}))
	mux.HandleFunc("/session", guard(func(w http.ResponseWriter, r *http.Request) {
		session := r.URL.Query().Get("sessionId")
		fx.mu.Lock()
		raw, ok := fx.sessions[session]
		fx.mu.Unlock()
		if !ok {
			http.Error(w, "no session", http.StatusNotFound)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write(raw)
	}))
	_ = http.Serve(listener, mux)
}

func bearer(r *http.Request) string {
	header := r.Header.Get("Authorization")
	if strings.HasPrefix(header, "Bearer ") {
		return strings.TrimPrefix(header, "Bearer ")
	}
	return r.URL.Query().Get("token")
}

func writeJSON(w http.ResponseWriter, value any) {
	w.Header().Set("Content-Type", "application/json")
	raw, _ := json.Marshal(value)
	_, _ = w.Write(append(raw, '\n'))
}

func watchParent(pid int) {
	go func() {
		for {
			if !parentAlive(pid) {
				fmt.Fprintf(os.Stderr, "fixturedaemon: parent %d gone, exiting\n", pid)
				os.Exit(0)
			}
			time.Sleep(500 * time.Millisecond)
		}
	}()
}

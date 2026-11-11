//go:build usagee2e

package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"sync"
	"time"

	"fintwind-go/internal/daemon"

	"github.com/gorilla/websocket"
)

type usageE2EFixture struct {
	server      *httptest.Server
	mu          sync.Mutex
	connections map[*websocket.Conn]struct{}
	requests    []map[string]any
	errors      []string
	history     json.RawMessage
	quotas      map[string]json.RawMessage
	completed   chan string
	stop        func() bool
}

func usageE2ENewFixture(ctx context.Context, now time.Time) (*usageE2EFixture, error) {
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return nil, err
	}
	f := &usageE2EFixture{connections: make(map[*websocket.Conn]struct{}), history: usageE2EHistoryJSON(now), quotas: usageE2EQuotaJSON(now), completed: make(chan string, 32)}
	f.server = &httptest.Server{Listener: listener, Config: &http.Server{Handler: http.HandlerFunc(f.serve), ReadHeaderTimeout: 3 * time.Second}}
	f.server.Start()
	f.stop = context.AfterFunc(ctx, f.close)
	return f, nil
}

func (f *usageE2EFixture) close() {
	f.mu.Lock()
	for conn := range f.connections {
		_ = conn.Close()
	}
	f.mu.Unlock()
	f.server.Close()
}

func (f *usageE2EFixture) snapshot() ([]map[string]any, []string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]map[string]any{}, f.requests...), append([]string{}, f.errors...)
}

func (f *usageE2EFixture) serve(w http.ResponseWriter, request *http.Request) {
	conn, err := (&websocket.Upgrader{HandshakeTimeout: 3 * time.Second}).Upgrade(w, request, nil)
	if err != nil {
		return
	}
	f.mu.Lock()
	f.connections[conn] = struct{}{}
	f.mu.Unlock()
	defer func() { _ = conn.Close(); f.mu.Lock(); delete(f.connections, conn); f.mu.Unlock() }()
	conn.SetReadLimit(1 << 20)
	_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
	_ = conn.SetWriteDeadline(time.Now().Add(5 * time.Second))
	var hello struct {
		Type            string `json:"type"`
		ProtocolVersion int    `json:"protocolVersion"`
	}
	if err = conn.ReadJSON(&hello); err != nil {
		return
	}
	if hello.Type != "hello" || hello.ProtocolVersion != daemon.ProtocolVersion {
		f.mu.Lock()
		f.errors = append(f.errors, "invalid protocol hello")
		f.mu.Unlock()
		return
	}
	if err = conn.WriteJSON(map[string]any{"type": "hello", "protocolVersion": daemon.ProtocolVersion, "environmentId": "10000000-0000-4000-8000-000000000001", "instanceId": "20000000-0000-4000-8000-000000000002"}); err != nil {
		return
	}
	for {
		_ = conn.SetReadDeadline(time.Now().Add(10 * time.Second))
		var envelope struct {
			Type      string         `json:"type"`
			RequestID string         `json:"requestId"`
			Command   map[string]any `json:"command"`
		}
		if err = conn.ReadJSON(&envelope); err != nil {
			return
		}
		if envelope.Type != "request" || envelope.RequestID == "" {
			f.mu.Lock()
			f.errors = append(f.errors, "invalid request envelope")
			f.mu.Unlock()
			return
		}
		f.mu.Lock()
		f.requests = append(f.requests, envelope.Command)
		f.mu.Unlock()
		kind, _ := envelope.Command["type"].(string)
		var payload any
		switch kind {
		case "getSettings":
			payload = json.RawMessage(`{"type":"settings","settings":{"opencode_binary_path":"fixture-opencode"}}`)
		case "fetchUsageStats":
			payload = f.history
		case "fetchIntegrations":
			payload = json.RawMessage(`{"type":"integrations","integrations":[{"id":"opencode-go","name":"OpenCode Go","supportsKey":true,"connected":true,"needsAuth":false},{"id":"openai","name":"OpenAI","supportsKey":false,"connected":true,"needsAuth":false},{"id":"deepseek","name":"DeepSeek","supportsKey":true,"connected":true,"needsAuth":true},{"id":"custom-unknown","name":"Custom unknown","supportsKey":true,"connected":true,"needsAuth":false}]}`)
		case "fetchProviderQuota":
			id, _ := envelope.Command["providerId"].(string)
			payload = f.quotas[id]
			if payload == nil {
				f.mu.Lock()
				f.errors = append(f.errors, "quota requested for unsupported or unauthorized service "+id)
				f.mu.Unlock()
			}
		default:
			f.mu.Lock()
			f.errors = append(f.errors, "unexpected command "+kind)
			f.mu.Unlock()
		}
		outcome := map[string]any{"status": "ok", "payload": payload}
		if payload == nil {
			outcome = map[string]any{"status": "error", "error": map[string]string{"message": "fixture command not allowed"}}
		}
		_ = conn.SetWriteDeadline(time.Now().Add(3 * time.Second))
		if err = conn.WriteJSON(map[string]any{"type": "response", "requestId": envelope.RequestID, "outcome": outcome}); err != nil {
			return
		}
		if id, ok := envelope.Command["providerId"].(string); ok {
			kind += ":" + id
		}
		f.completed <- kind
	}
}

func usageE2EHistoryJSON(now time.Time) json.RawMessage {
	today := time.Date(now.Year(), now.Month(), now.Day(), 12, 0, 0, 0, now.Location())
	t0, t8, t40 := today.Unix(), today.AddDate(0, 0, -8).Unix(), today.AddDate(0, 0, -40).Unix()
	return json.RawMessage(fmt.Sprintf(`{"type":"usageStats","stats":{"sessionsScanned":4,"truncated":false,"refreshing":false,"refreshError":null,"entries":[
	{"timestamp":%[1]d,"model":"wrong/fallback","directory":"fixture/project-a","cost":2.5,"inputTokens":100,"outputTokens":50,"reasoningTokens":10,"cacheReadTokens":200,"cacheWriteTokens":40,"days":[{"timestamp":%[1]d,"direct":160,"total":400,"input":100,"output":60,"cacheRead":200,"cacheWrite":40}],"modelLanes":[{"model":"openai/model-a","total":400,"cost":2.5,"tokens":[100,50,10,200,40],"days":[{"timestamp":%[1]d,"direct":160,"total":400,"input":100,"output":60,"cacheRead":200,"cacheWrite":40}]}]},
	{"timestamp":%[1]d,"model":"wrong/fallback","directory":"fixture/project-b","cost":null,"inputTokens":20,"outputTokens":10,"days":[{"timestamp":%[1]d,"direct":10,"total":10,"input":5,"output":5},{"timestamp":%[2]d,"direct":20,"total":20,"input":15,"output":5}],"modelLanes":[{"model":"anthropic/model-b","total":30,"tokens":[20,10,0,0,0],"days":[{"timestamp":%[1]d,"direct":10,"total":10,"input":5,"output":5},{"timestamp":%[2]d,"direct":20,"total":20,"input":15,"output":5}]}]},
	{"timestamp":%[3]d,"model":"openai/old","directory":"fixture/project-a","cost":0,"inputTokens":4,"outputTokens":2,"cacheReadTokens":10,"days":[{"timestamp":%[3]d,"direct":6,"total":16,"input":4,"output":2,"cacheRead":10}],"modelLanes":[{"model":"openai/old","total":16,"tokens":[4,2,0,10,0],"days":[{"timestamp":%[3]d,"direct":6,"total":16,"input":4,"output":2,"cacheRead":10}]}]},
	{"timestamp":%[1]d,"model":"wrong/fallback","directory":"fixture/project-a","cost":null,"inputTokens":30,"outputTokens":10,"cacheReadTokens":10,"subagentSessions":1,"subagentTokens":20,"subagentDirect":15,"days":[{"timestamp":%[1]d,"direct":25,"total":30,"input":20,"output":5,"cacheRead":5},{"timestamp":%[1]d,"subagent":true,"direct":15,"total":20,"input":10,"output":5,"cacheRead":5}],"modelLanes":[{"model":"openai/parent","total":30,"tokens":[20,5,0,5,0],"days":[{"timestamp":%[1]d,"direct":25,"total":30,"input":20,"output":5,"cacheRead":5}]}],"subagentModels":[{"sessionId":"fixture-child","model":"openai/child","inputTokens":10,"outputTokens":5,"cacheReadTokens":5,"cost":null,"days":[{"timestamp":%[1]d,"subagent":true,"direct":15,"total":20,"input":10,"output":5,"cacheRead":5}]}]}
	]}}`, t0, t8, t40))
}

func usageE2EQuotaJSON(now time.Time) map[string]json.RawMessage {
	return map[string]json.RawMessage{
		"opencode-go": json.RawMessage(fmt.Sprintf(`{"type":"providerQuota","providerId":"opencode-go","quota":{"status":"ready","plan":"Go","service":"OpenCode Go","account":null,"windows":[{"id":"rolling","kind":"rolling","model":null,"spanSeconds":18000,"usedPercent":37.5,"remaining":null,"limit":null,"resetsAt":%d,"unlimited":false},{"id":"weekly","kind":"weekly","spanSeconds":604800,"unlimited":true}],"balances":[],"measuredAt":%d,"retryAt":null}}`, now.Add(time.Hour).Unix(), now.Unix())),
		"openai":      json.RawMessage(fmt.Sprintf(`{"type":"providerQuota","providerId":"openai","quota":{"status":"unavailable","plan":null,"service":"OpenAI","account":null,"windows":[{"id":"rolling","kind":"rolling","spanSeconds":18000,"usedPercent":70,"unlimited":false}],"balances":[],"measuredAt":%d,"retryAt":null}}`, now.Add(-48*time.Hour).Unix())),
	}
}

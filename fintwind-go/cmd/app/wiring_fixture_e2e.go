//go:build wiringe2e

package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"sync"
	"time"

	"fintwind-go/internal/daemon"

	"github.com/gorilla/websocket"
)

const wiringEnvironment = "10000000-0000-4000-8000-000000000001"

type wiringCommand struct {
	Raw       json.RawMessage    `json:"-"`
	Type      string             `json:"type"`
	ID        string             `json:"id"`
	RequestID string             `json:"requestId"`
	OptionID  string             `json:"optionId"`
	Prompt    string             `json:"prompt"`
	Key       string             `json:"key"`
	Value     json.RawMessage    `json:"value"`
	Answers   json.RawMessage    `json:"answers"`
	Operation wiringGitOperation `json:"operation"`
}

func (c *wiringCommand) UnmarshalJSON(data []byte) error {
	type command wiringCommand
	if err := json.Unmarshal(data, (*command)(c)); err != nil {
		return err
	}
	c.Raw = append(json.RawMessage(nil), data...)
	return nil
}

type wiringEnvelope struct {
	Type      string        `json:"type"`
	RequestID string        `json:"requestId"`
	SessionID string        `json:"sessionId"`
	RuntimeID string        `json:"runtimeId"`
	Command   wiringCommand `json:"command"`
}

type wiringFixture struct {
	server      *httptest.Server
	ctx         context.Context
	mu          sync.Mutex
	connections map[*websocket.Conn]struct{}
	requests    []wiringEnvelope
	errors      []string
	completed   chan string
	stop        func() bool
	root        string
	extended    func(wiringEnvelope) (any, error, bool, bool)
}

func wiringNewFixture(ctx context.Context, root string) (*wiringFixture, error) {
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return nil, err
	}
	f := &wiringFixture{ctx: ctx, connections: map[*websocket.Conn]struct{}{}, completed: make(chan string, 128), root: root}
	f.server = &httptest.Server{Listener: listener, Config: &http.Server{Handler: http.HandlerFunc(f.serve), ReadHeaderTimeout: 3 * time.Second}}
	f.server.Start()
	f.stop = context.AfterFunc(ctx, f.close)
	return f, nil
}

func (f *wiringFixture) close() {
	f.mu.Lock()
	for conn := range f.connections {
		_ = conn.Close()
	}
	f.mu.Unlock()
	f.server.Close()
}

func (f *wiringFixture) snapshot() ([]wiringEnvelope, []string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]wiringEnvelope{}, f.requests...), append([]string{}, f.errors...)
}

func (f *wiringFixture) count(kind string) int {
	requests, _ := f.snapshot()
	n := 0
	for _, request := range requests {
		if request.Command.Type == kind || request.Command.Type+":"+request.Command.Operation.Type == kind {
			n++
		}
	}
	return n
}

func (f *wiringFixture) await(ctx context.Context, kind string) error {
	wait, cancel := context.WithTimeout(ctx, 4*time.Second)
	defer cancel()
	for {
		select {
		case completed := <-f.completed:
			if completed == kind {
				return nil
			}
		case <-wait.Done():
			return fmt.Errorf("waiting for %s: %w", kind, wait.Err())
		}
	}
}

func (f *wiringFixture) serve(w http.ResponseWriter, request *http.Request) {
	conn, err := (&websocket.Upgrader{HandshakeTimeout: 3 * time.Second}).Upgrade(w, request, nil)
	if err != nil {
		return
	}
	f.mu.Lock()
	f.connections[conn] = struct{}{}
	f.mu.Unlock()
	defer func() { _ = conn.Close(); f.mu.Lock(); delete(f.connections, conn); f.mu.Unlock() }()
	conn.SetReadLimit(1 << 20)
	_ = conn.SetReadDeadline(time.Now().Add(4 * time.Second))
	_ = conn.SetWriteDeadline(time.Now().Add(4 * time.Second))
	var hello struct {
		Type            string `json:"type"`
		ProtocolVersion int    `json:"protocolVersion"`
	}
	if conn.ReadJSON(&hello) != nil {
		return
	}
	if hello.Type != "hello" || hello.ProtocolVersion != daemon.ProtocolVersion {
		f.mu.Lock()
		f.errors = append(f.errors, "invalid protocol hello")
		f.mu.Unlock()
		return
	}
	if conn.WriteJSON(map[string]any{"type": "hello", "protocolVersion": daemon.ProtocolVersion, "environmentId": wiringEnvironment, "instanceId": "20000000-0000-4000-8000-000000000002"}) != nil {
		return
	}
	for {
		_ = conn.SetReadDeadline(time.Now().Add(15 * time.Second))
		var envelope wiringEnvelope
		if conn.ReadJSON(&envelope) != nil {
			return
		}
		if envelope.Type != "request" || envelope.RequestID == "" {
			return
		}
		f.mu.Lock()
		f.requests = append(f.requests, envelope)
		f.mu.Unlock()
		payload, rpcErr, drop := f.dispatch(envelope)
		kind := envelope.Command.Type
		if kind == "workspace" {
			kind += ":" + envelope.Command.Operation.Type
		}
		if drop {
			f.completed <- kind
			return
		}
		outcome := map[string]any{"status": "ok", "payload": payload}
		if rpcErr != nil {
			outcome = map[string]any{"status": "error", "error": map[string]string{"message": rpcErr.Error()}}
		}
		_ = conn.SetWriteDeadline(time.Now().Add(4 * time.Second))
		if conn.WriteJSON(map[string]any{"type": "response", "requestId": envelope.RequestID, "outcome": outcome}) != nil {
			return
		}
		f.completed <- kind
	}
}

func (f *wiringFixture) dispatch(envelope wiringEnvelope) (any, error, bool) {
	if f.extended != nil {
		if payload, err, drop, handled := f.extended(envelope); handled {
			return payload, err, drop
		}
	}
	c := envelope.Command
	switch c.Type {
	case "updateSetting":
		if c.Key != "auto_compact_enabled" || string(c.Value) != "false" {
			return nil, errors.New("unexpected atomic setting"), false
		}
		return map[string]any{"type": "settings", "settings": map[string]any{"auto_compact_enabled": false, "future_fixture": "preserved"}}, nil, false
	case "respond", "respondUserInput", "cancelUserInput", "compactSession", "steer":
		if envelope.SessionID != "fixture-session" || envelope.RuntimeID != "fixture-runtime" {
			return nil, errors.New("fixture session identity rejected"), false
		}
		return map[string]string{"type": "ack"}, nil, false
	case "workspace":
		payload, err := f.gitOperation(c.Operation)
		return map[string]any{"type": "workspace", "result": payload}, err, false
	default:
		f.mu.Lock()
		f.errors = append(f.errors, "unexpected command "+c.Type)
		f.mu.Unlock()
		return nil, errors.New("fixture command not allowed"), false
	}
}

package main

// Isolated native session API fixture. The daemon engine starts this
// executable as its OpenCode binary; this server emulates only the endpoints
// the history-edit path uses. State survives daemon restarts via a JSON file
// in the profile root (the fixture working directory).

import (
	"context"
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

type fixtureMessage struct {
	ID   string `json:"id"`
	Role string `json:"role"`
}

type fixtureSession struct {
	Messages []fixtureMessage `json:"messages"`
	Staged   string           `json:"staged"`
}

type fixtureState struct {
	Sessions map[string]*fixtureSession `json:"sessions"`
	Forks    int                        `json:"forks"`
}

type fixtureCall struct {
	Method string          `json:"method"`
	Path   string          `json:"path"`
	Body   json.RawMessage `json:"body"`
}

type fixture struct {
	mu    sync.Mutex
	root  string
	state fixtureState
}

func fixturePath(name string) string {
	cwd, _ := os.Getwd()
	return filepath.Join(cwd, name)
}

func loadFixture(root string) (*fixture, error) {
	f := &fixture{root: root, state: fixtureState{Sessions: map[string]*fixtureSession{}}}
	raw, err := os.ReadFile(fixturePath("taskedit-native.json"))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return f, nil
		}
		return nil, err
	}
	if err := json.Unmarshal(raw, &f.state); err != nil {
		return nil, err
	}
	if f.state.Sessions == nil {
		f.state.Sessions = map[string]*fixtureSession{}
	}
	return f, nil
}

func (f *fixture) persist() {
	raw, err := json.Marshal(f.state)
	if err != nil {
		return
	}
	_ = os.WriteFile(fixturePath("taskedit-native.json"), raw, 0o600)
}

func (f *fixture) ensure(id string) *fixtureSession {
	session := f.state.Sessions[id]
	if session == nil {
		session = &fixtureSession{Messages: []fixtureMessage{
			{ID: "u1", Role: "user"}, {ID: "a1", Role: "assistant"},
			{ID: "u2", Role: "user"}, {ID: "a2", Role: "assistant"},
			{ID: "u3", Role: "user"}, {ID: "a3", Role: "assistant"},
		}}
		f.state.Sessions[id] = session
		f.persist()
	}
	return session
}

func (f *fixture) logCall(method, path string, body json.RawMessage) {
	f.mu.Lock()
	defer f.mu.Unlock()
	file, err := os.OpenFile(fixturePath("taskedit-calls.jsonl"), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return
	}
	defer file.Close()
	raw, _ := json.Marshal(fixtureCall{Method: method, Path: path, Body: body})
	raw = append(raw, '\n')
	_, _ = file.Write(raw)
}

func serveFixture(args []string) error {
	password := os.Getenv("OPENCODE_SERVER_PASSWORD")
	if password == "" || os.Getenv("OPENCODE_TEST_HOME") == "" {
		return errors.New("isolated fixture environment required")
	}
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}
	f, err := loadFixture(cwd)
	if err != nil {
		return err
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	defer listener.Close()
	server := &http.Server{ReadHeaderTimeout: 5 * time.Second, Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		user, key, ok := r.BasicAuth()
		if !ok || user != "opencode" || key != password {
			http.Error(w, "unauthorized", 401)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		if r.Method == "GET" && r.URL.Path == "/api/info" {
			_, _ = w.Write([]byte(`{"data":{}}`))
			return
		}
		if r.Method == "GET" && r.URL.Path == "/api/session" {
			// This fixture has no detached child sessions. History editing now
			// requires a complete roster, not a missing-endpoint fallback.
			_, _ = w.Write([]byte(`{"data":[],"cursor":{"next":null}}`))
			return
		}
		if r.Method == "GET" && r.URL.Path == "/api/session/active" {
			_, _ = w.Write([]byte(`{"data":{}}`))
			return
		}
		if !strings.HasPrefix(r.URL.Path, "/api/session/") {
			http.Error(w, "unexpected operation", 400)
			return
		}
		rest := strings.TrimPrefix(r.URL.Path, "/api/session/")
		var id, action string
		if index := strings.Index(rest, "/"); index >= 0 {
			id, action = rest[:index], rest[index+1:]
		} else {
			id, action = rest, ""
		}
		if id == "" {
			http.Error(w, "native session ID required", 400)
			return
		}
		var body json.RawMessage
		if r.Body != nil && r.ContentLength != 0 {
			var value any
			if err := json.NewDecoder(r.Body).Decode(&value); err != nil {
				http.Error(w, "invalid body", 400)
				return
			}
			body, _ = json.Marshal(value)
		}
		f.mu.Lock()
		session := f.ensure(id)
		switch {
		case r.Method == "GET" && action == "message":
			messages := make([]map[string]any, 0, len(session.Messages))
			for i := len(session.Messages) - 1; i >= 0; i-- {
				message := session.Messages[i]
				messages = append(messages, map[string]any{"id": message.ID, "type": message.Role, "content": []any{map[string]string{"type": "text", "text": "isolated " + message.ID}}})
			}
			f.mu.Unlock()
			raw, _ := json.Marshal(map[string]any{"data": messages, "cursor": map[string]any{}})
			_, _ = w.Write(raw)
			return
		case r.Method == "GET" && action == "":
			revert := map[string]any{}
			if session.Staged != "" {
				revert["messageID"] = session.Staged
			}
			f.mu.Unlock()
			raw, _ := json.Marshal(map[string]any{"data": map[string]any{"revert": revert}})
			_, _ = w.Write(raw)
			return
		case r.Method == "POST" && action == "fork":
			var request struct {
				Before string `json:"before"`
			}
			_ = json.Unmarshal(body, &request)
			users := []string{}
			for _, message := range session.Messages {
				if message.Role == "user" {
					users = append(users, message.ID)
				}
			}
			keep := append([]fixtureMessage(nil), session.Messages...)
			if request.Before != "" {
				index := -1
				for i, message := range session.Messages {
					if message.Role == "user" && message.ID == request.Before {
						index = i
						break
					}
				}
				if index < 0 || len(users) == 0 {
					f.mu.Unlock()
					http.Error(w, "unknown fork boundary", 400)
					return
				}
				keep = append([]fixtureMessage(nil), session.Messages[:index]...)
			}
			if _, fail := os.Stat(fixturePath("taskedit-fail-fork")); fail == nil {
				f.mu.Unlock()
				f.logCall(r.Method, r.URL.Path, body)
				http.Error(w, "injected fork failure", 500)
				return
			}
			f.state.Forks++
			forkID := fmt.Sprintf("ses_fork_%d", f.state.Forks)
			f.state.Sessions[forkID] = &fixtureSession{Messages: keep}
			f.persist()
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, body)
			raw, _ := json.Marshal(map[string]any{"data": map[string]any{"id": forkID}})
			_, _ = w.Write(raw)
			return
		case r.Method == "DELETE" && action == "revert":
			session.Staged = ""
			f.persist()
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, body)
			_, _ = w.Write([]byte(`{}`))
			return
		case r.Method == "POST" && action == "revert/stage":
			var request struct {
				MessageID string `json:"messageID"`
			}
			_ = json.Unmarshal(body, &request)
			found := false
			for _, message := range session.Messages {
				if message.Role == "user" && message.ID == request.MessageID {
					found = true
				}
			}
			if !found {
				f.mu.Unlock()
				http.Error(w, "unknown stage boundary", 400)
				return
			}
			session.Staged = request.MessageID
			f.persist()
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, body)
			_, _ = w.Write([]byte(`{}`))
			return
		case r.Method == "POST" && action == "revert/commit":
			if _, fail := os.Stat(fixturePath("taskedit-fail-commit")); fail == nil {
				f.mu.Unlock()
				f.logCall(r.Method, r.URL.Path, body)
				http.Error(w, "injected revert failure", 500)
				return
			}
			if session.Staged != "" {
				kept := session.Messages[:0]
				for _, message := range session.Messages {
					if message.Role == "user" && message.ID == session.Staged {
						break
					}
					kept = append(kept, message)
				}
				session.Messages = append([]fixtureMessage(nil), kept...)
				session.Staged = ""
				f.persist()
			}
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, body)
			_, _ = w.Write([]byte(`{}`))
			return
		}
		f.mu.Unlock()
		http.Error(w, "unexpected operation", 400)
	})}
	deadline, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()
	go func() { <-deadline.Done(); _ = server.Close() }()
	fmt.Println("http://" + listener.Addr().String())
	err = server.Serve(listener)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

package main

// Isolated OpenCode-subset fake for the runtime fork command. The daemon
// starts this executable as its engine binary; it emulates only the
// endpoints the runtime fork path uses. State lives in the engine working
// directory (the round root) so the harness can assert on it.

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

type fakeMessage map[string]any

type fakeSession struct {
	Messages []fakeMessage `json:"messages"`
	Busy     bool          `json:"busy"`
	NextU    int           `json:"nextU"`
	NextA    int           `json:"nextA"`
	NextD    int           `json:"nextD"`
}

type fakeState struct {
	Sessions map[string]*fakeSession `json:"sessions"`
	Forks    int                     `json:"forks"`
	Creates  int                     `json:"creates"`
	Seq      int                     `json:"seq"`
}

type fake struct {
	mu    sync.Mutex
	root  string
	state fakeState
	subs  map[chan string]bool
}

func fakePath(name string) string {
	cwd, _ := os.Getwd()
	return filepath.Join(cwd, name)
}

func loadFake(root string) (*fake, error) {
	f := &fake{root: root, state: fakeState{Sessions: map[string]*fakeSession{}}, subs: map[chan string]bool{}}
	raw, err := os.ReadFile(fakePath("fixture-native.json"))
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
		f.state.Sessions = map[string]*fakeSession{}
	}
	return f, nil
}

func (f *fake) persist() {
	raw, err := json.Marshal(f.state)
	if err != nil {
		return
	}
	_ = os.WriteFile(fakePath("fixture-native.json"), raw, 0o600)
}

func (f *fake) logCall(method, path string, body json.RawMessage) {
	f.mu.Lock()
	defer f.mu.Unlock()
	file, err := os.OpenFile(fakePath("fixture-calls.jsonl"), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return
	}
	defer file.Close()
	raw, _ := json.Marshal(fakeCall{Method: method, Path: path, Body: body})
	raw = append(raw, '\n')
	_, _ = file.Write(raw)
}

func (f *fake) emit(event map[string]any) {
	raw, _ := json.Marshal(event)
	text := string(raw)
	f.mu.Lock()
	defer f.mu.Unlock()
	for stream := range f.subs {
		select {
		case stream <- text:
		default:
		}
	}
}

func textPart(text string) map[string]any {
	return map[string]any{"type": "text", "text": text}
}

func (f *fake) appendUser(session *fakeSession, text string) (string, fakeMessage) {
	var id string
	var message fakeMessage
	switch {
	case strings.HasPrefix(text, "third-flat"):
		session.NextU++
		id = fmt.Sprintf("u%d", session.NextU)
		message = fakeMessage{"id": id, "type": "user", "text": text}
	case strings.HasPrefix(text, "note-empty"):
		session.NextD++
		id = fmt.Sprintf("d%d", session.NextD)
		message = fakeMessage{"id": id, "type": "user", "text": "", "content": []any{textPart("")}}
	case strings.HasPrefix(text, "note-attach"):
		session.NextD++
		id = fmt.Sprintf("d%d", session.NextD)
		message = fakeMessage{"id": id, "type": "user", "content": []any{map[string]any{"type": "file", "name": "a.bin"}}}
	case strings.HasPrefix(text, "note-role"):
		session.NextD++
		id = fmt.Sprintf("d%d", session.NextD)
		message = fakeMessage{"id": id, "role": "user", "text": "decoy-role"}
	case strings.HasPrefix(text, "note-synth"):
		session.NextD++
		id = fmt.Sprintf("d%d", session.NextD)
		message = fakeMessage{"id": id, "type": "synthetic", "text": "decoy-synth"}
	default:
		session.NextU++
		id = fmt.Sprintf("u%d", session.NextU)
		message = fakeMessage{"id": id, "type": "user", "content": []any{textPart(text)}}
	}
	session.Messages = append(session.Messages, message)
	return id, message
}

func (f *fake) appendAssistant(session *fakeSession) string {
	session.NextA++
	id := fmt.Sprintf("a%d", session.NextA)
	session.Messages = append(session.Messages, fakeMessage{"id": id, "type": "assistant", "content": []any{textPart("ack")}})
	return id
}

func (f *fake) succeed(sessionID string) {
	base := map[string]any{"sessionID": sessionID, "assistantMessageID": "a", "ordinal": 0}
	go func() {
		time.Sleep(50 * time.Millisecond)
		f.emit(map[string]any{"type": "session.text.started", "data": base})
		f.emit(map[string]any{"type": "session.text.delta", "data": withDelta(base, "rk")})
		f.emit(map[string]any{"type": "session.text.ended", "data": withText(base, "rk")})
		f.emit(map[string]any{"type": "session.execution.succeeded", "data": map[string]any{"sessionID": sessionID}})
	}()
}

func withDelta(base map[string]any, delta string) map[string]any {
	out := map[string]any{}
	for k, v := range base {
		out[k] = v
	}
	out["delta"] = delta
	return out
}

func withText(base map[string]any, text string) map[string]any {
	out := map[string]any{}
	for k, v := range base {
		out[k] = v
	}
	out["text"] = text
	return out
}

func serveFake(args []string) error {
	password := os.Getenv("OPENCODE_SERVER_PASSWORD")
	if password == "" || os.Getenv("OPENCODE_TEST_HOME") == "" {
		return errors.New("isolated fixture environment required")
	}
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}
	f, err := loadFake(cwd)
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
		if r.URL.Path == "/api/event" {
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(200)
			w.(http.Flusher).Flush()
			stream := make(chan string, 64)
			f.mu.Lock()
			f.subs[stream] = true
			f.mu.Unlock()
			defer func() { f.mu.Lock(); delete(f.subs, stream); f.mu.Unlock() }()
			for {
				select {
				case <-r.Context().Done():
					return
				case data := <-stream:
					fmt.Fprintf(w, "data: %s\n\n", data)
					w.(http.Flusher).Flush()
				}
			}
		}
		if r.URL.Path == "/api/info" {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"data":{}}`))
			return
		}
		if r.Method == "POST" && r.URL.Path == "/api/session" {
			f.mu.Lock()
			f.state.Seq++
			f.state.Creates++
			id := fmt.Sprintf("ses_%d", f.state.Seq)
			f.state.Sessions[id] = &fakeSession{}
			f.persist()
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, nil)
			w.Header().Set("Content-Type", "application/json")
			raw, _ := json.Marshal(map[string]any{"data": map[string]any{"id": id}})
			_, _ = w.Write(raw)
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
		session := f.state.Sessions[id]
		if session == nil {
			session = &fakeSession{}
			f.state.Sessions[id] = session
		}
		switch {
		case r.Method == "POST" && action == "agent":
			f.persist()
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, body)
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{}`))
			return
		case r.Method == "POST" && action == "model":
			f.persist()
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, body)
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{}`))
			return
		case r.Method == "POST" && action == "prompt":
			var request struct {
				Text string `json:"text"`
			}
			_ = json.Unmarshal(body, &request)
			f.appendUser(session, request.Text)
			hold := strings.HasPrefix(request.Text, "hold")
			session.Busy = hold
			f.persist()
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, body)
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"data":{}}`))
			if !hold {
				f.appendAssistantSync(id)
				f.succeed(id)
			}
			return
		case r.Method == "POST" && action == "interrupt":
			session.Busy = false
			f.persist()
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, body)
			f.emit(map[string]any{"type": "session.execution.interrupted", "data": map[string]any{"sessionID": id}})
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{}`))
			return
		case r.Method == "GET" && action == "message":
			const pageSize = 2
			offset := 0
			if cursor := r.URL.Query().Get("cursor"); cursor != "" {
				offset, _ = strconv.Atoi(strings.TrimPrefix(cursor, "c"))
			}
			reversed := make([]fakeMessage, 0, len(session.Messages))
			for i := len(session.Messages) - 1; i >= 0; i-- {
				reversed = append(reversed, session.Messages[i])
			}
			end := offset + pageSize
			if end > len(reversed) {
				end = len(reversed)
			}
			page := reversed[offset:end]
			if offset > len(reversed) {
				page = []fakeMessage{}
			}
			items := make([]any, 0, len(page))
			for _, message := range page {
				items = append(items, message)
			}
			next := ""
			if end < len(reversed) {
				next = "c" + strconv.Itoa(end)
			}
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, body)
			w.Header().Set("Content-Type", "application/json")
			raw, _ := json.Marshal(map[string]any{"data": items, "cursor": map[string]any{"next": next}})
			_, _ = w.Write(raw)
			return
		case r.Method == "POST" && action == "fork":
			var request struct {
				Before string `json:"before"`
			}
			_ = json.Unmarshal(body, &request)
			keep := append([]fakeMessage(nil), session.Messages...)
			if request.Before != "" {
				index := -1
				for i, message := range session.Messages {
					if message["type"] == "user" && message["id"] == request.Before {
						index = i
						break
					}
				}
				if index < 0 {
					f.mu.Unlock()
					f.logCall(r.Method, r.URL.Path, body)
					http.Error(w, "unknown fork boundary", 400)
					return
				}
				keep = append([]fakeMessage(nil), session.Messages[:index]...)
			}
			if _, fail := os.Stat(fakePath("fixture-fail-fork")); fail == nil {
				f.mu.Unlock()
				f.logCall(r.Method, r.URL.Path, body)
				http.Error(w, "injected fork failure", 500)
				return
			}
			f.state.Forks++
			forkID := fmt.Sprintf("ses_fork_%d", f.state.Forks)
			f.state.Sessions[forkID] = &fakeSession{Messages: keep}
			f.persist()
			f.mu.Unlock()
			f.logCall(r.Method, r.URL.Path, body)
			w.Header().Set("Content-Type", "application/json")
			raw, _ := json.Marshal(map[string]any{"data": map[string]any{"id": forkID}})
			_, _ = w.Write(raw)
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

func (f *fake) appendAssistantSync(id string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	session := f.state.Sessions[id]
	if session == nil {
		return
	}
	f.appendAssistant(session)
	f.persist()
}

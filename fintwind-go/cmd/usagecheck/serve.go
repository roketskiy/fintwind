package main

// Isolated OpenCode session API fixture for the usage index. The daemon
// engine starts this executable as its OpenCode binary. The parent test
// drives list/message inventories through usage-seed.json in the profile
// root; the fixture reloads it whenever its content changes, so every phase
// gets a deterministic inventory without restarting the daemon. Mutations
// (deletions) do not apply here; phases rewrite the seed instead.

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
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

type seedPage struct {
	Cursor string            `json:"cursor"`
	Rows   []json.RawMessage `json:"rows"`
	Next   string            `json:"next"`
}

type usageSeed struct {
	Pages          []seedPage            `json:"pages"`
	Messages       map[string][]seedPage `json:"messages"`
	Fail           map[string]int        `json:"fail"`
	SlowMs         map[string]int        `json:"slowMs"`
	BlockMessages  bool                  `json:"blockMessages"`
	MessageDelayMs int                   `json:"messageDelayMs"`
	SynthList      *synthSpec            `json:"synthList"`
	SynthMessages  *synthSpec            `json:"synthMessages"`
}

type synthSpec struct {
	Session     string `json:"session"`
	Pages       int    `json:"pages"`
	RowsPerPage int    `json:"rowsPerPage"`
	Pad         int    `json:"pad"`
}

type usageCall struct {
	Method string          `json:"method"`
	Path   string          `json:"path"`
	Body   json.RawMessage `json:"body"`
}

type usageFixture struct {
	mu       sync.Mutex
	root     string
	seedHash string
	seed     usageSeed
}

func seedPath(root string) string       { return filepath.Join(root, "usage-seed.json") }
func usageCallsPath(root string) string { return filepath.Join(root, "usage-calls.jsonl") }

func (f *usageFixture) sync() {
	raw, err := os.ReadFile(seedPath(f.root))
	if err != nil {
		return
	}
	sum := sha256.Sum256(raw)
	hash := hex.EncodeToString(sum[:])
	if hash == f.seedHash {
		return
	}
	var seed usageSeed
	if err := json.Unmarshal(raw, &seed); err != nil {
		return
	}
	if seed.Messages == nil {
		seed.Messages = map[string][]seedPage{}
	}
	if seed.Fail == nil {
		seed.Fail = map[string]int{}
	}
	if seed.SlowMs == nil {
		seed.SlowMs = map[string]int{}
	}
	f.seed = seed
	f.seedHash = hash
}

func (f *usageFixture) logCall(method, path string, body json.RawMessage) {
	file, err := os.OpenFile(usageCallsPath(f.root), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return
	}
	defer file.Close()
	raw, _ := json.Marshal(usageCall{Method: method, Path: path, Body: body})
	raw = append(raw, '\n')
	_, _ = file.Write(raw)
}

func synthRow(prefix string, pad int, ts uint64) map[string]any {
	return map[string]any{
		"id":   prefix,
		"type": "assistant",
		"time": map[string]any{"created": ts},
		"tokens": map[string]any{
			"input": uint64(1), "output": uint64(0), "reasoning": uint64(0),
			"cache": map[string]any{"read": uint64(0), "write": uint64(0)},
		},
		"model":   map[string]any{"providerID": "p", "modelID": "a"},
		"content": strings.Repeat("x", pad),
	}
}

func synthSession(id string, pad int, ts uint64) map[string]any {
	return map[string]any{
		"id":       id,
		"time":     map[string]any{"created": ts, "updated": ts},
		"model":    map[string]any{"providerID": "p", "modelID": "m", "id": "i"},
		"location": map[string]any{"directory": "/proj"},
		"tokens": map[string]any{
			"input": uint64(1), "output": uint64(0), "reasoning": uint64(0),
			"cache": map[string]any{"read": uint64(0), "write": uint64(0)},
		},
		"cost":      0.0,
		"directory": strings.Repeat("y", pad),
	}
}

type statusWriter struct {
	http.ResponseWriter
	status int
}

func (w *statusWriter) WriteHeader(status int) {
	w.status = status
	w.ResponseWriter.WriteHeader(status)
}

func serveFixture(args []string) error {
	password := os.Getenv("OPENCODE_SERVER_PASSWORD")
	if password == "" || os.Getenv("OPENCODE_TEST_HOME") == "" {
		return errors.New("isolated fixture environment required")
	}
	root, err := os.Getwd()
	if err != nil {
		return err
	}
	f := &usageFixture{root: root}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	defer listener.Close()
	server := &http.Server{ReadHeaderTimeout: 5 * time.Second, Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sw := &statusWriter{ResponseWriter: w, status: 200}
		w = sw
		user, key, ok := r.BasicAuth()
		if !ok || user != "opencode" || key != password {
			http.Error(w, "unauthorized", 401)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		var body json.RawMessage
		if r.Body != nil && r.ContentLength != 0 {
			var value any
			if err := json.NewDecoder(r.Body).Decode(&value); err != nil {
				http.Error(w, "invalid body", 400)
				return
			}
			body, _ = json.Marshal(value)
		}
		defer func() {
			f.logCall(r.Method, fmt.Sprintf("%s -> %d", r.URL.RequestURI(), sw.status), body)
		}()
		f.mu.Lock()
		defer f.mu.Unlock()
		f.sync()
		path := r.URL.Path
		write := func(value any) {
			raw, _ := json.Marshal(value)
			_, _ = w.Write(raw)
		}
		if r.Method == "GET" && path == "/api/info" {
			write(map[string]any{"data": map[string]any{}})
			return
		}
		if r.Method == "POST" && path == "/api/session" {
			write(map[string]any{"data": map[string]any{"id": "ses_unused"}})
			return
		}
		if r.Method == "GET" && path == "/api/session" {
			if status, ok := f.seed.Fail["list"]; ok && status != 0 {
				http.Error(w, "injected list failure", status)
				return
			}
			if delay := f.seed.SlowMs["list"]; delay > 0 {
				f.mu.Unlock()
				select {
				case <-r.Context().Done():
					f.mu.Lock()
					return
				case <-time.After(time.Duration(delay) * time.Millisecond):
				}
				f.mu.Lock()
				f.sync()
			}
			nowMs := uint64(time.Now().UnixMilli())
			if spec := f.seed.SynthList; spec != nil {
				cursor := r.URL.Query().Get("cursor")
				index := 0
				if cursor != "" {
					_, _ = fmt.Sscanf(cursor, "syn%d", &index)
				}
				if index >= spec.Pages {
					write(map[string]any{"data": []any{}, "cursor": map[string]any{}})
					return
				}
				rows := make([]any, 0, spec.RowsPerPage)
				for i := 0; i < spec.RowsPerPage; i++ {
					rows = append(rows, synthSession(fmt.Sprintf("ses_syn_%d_%d", index, i), spec.Pad, nowMs))
				}
				next := map[string]any{}
				if index+1 < spec.Pages {
					next = map[string]any{"next": fmt.Sprintf("syn%d", index+1)}
				}
				write(map[string]any{"data": rows, "cursor": next})
				return
			}
			cursor := r.URL.Query().Get("cursor")
			for _, page := range f.seed.Pages {
				if page.Cursor == cursor {
					rows := page.Rows
					if rows == nil {
						rows = []json.RawMessage{}
					}
					cursorOut := map[string]any{}
					if page.Next != "" {
						cursorOut = map[string]any{"next": page.Next}
					}
					write(map[string]any{"data": rows, "cursor": cursorOut})
					return
				}
			}
			write(map[string]any{"data": []json.RawMessage{}, "cursor": map[string]any{}})
			return
		}
		if strings.HasPrefix(path, "/api/session/") && path != "/api/session/active" {
			rest := strings.TrimPrefix(path, "/api/session/")
			if index := strings.Index(rest, "/"); index >= 0 {
				id, action := rest[:index], rest[index+1:]
				if action == "message" && r.Method == "GET" {
					key := "message:" + id
					if status, ok := f.seed.Fail[key]; ok && status != 0 {
						http.Error(w, "injected message failure", status)
						return
					}
					for f.seed.BlockMessages {
						f.mu.Unlock()
						select {
						case <-r.Context().Done():
							f.mu.Lock()
							return
						case <-time.After(200 * time.Millisecond):
						}
						f.mu.Lock()
						f.sync()
					}
					if delay := f.seed.SlowMs[key]; delay > 0 {
						f.mu.Unlock()
						select {
						case <-r.Context().Done():
							f.mu.Lock()
							return
						case <-time.After(time.Duration(delay) * time.Millisecond):
						}
						f.mu.Lock()
						f.sync()
					}
					if delay := f.seed.MessageDelayMs; delay > 0 {
						f.mu.Unlock()
						select {
						case <-r.Context().Done():
							f.mu.Lock()
							return
						case <-time.After(time.Duration(delay) * time.Millisecond):
						}
						f.mu.Lock()
						f.sync()
					}
					if spec := f.seed.SynthMessages; spec != nil && spec.Session == id {
						nowMs := uint64(time.Now().UnixMilli())
						cursor := r.URL.Query().Get("cursor")
						index := 0
						if cursor != "" {
							_, _ = fmt.Sscanf(cursor, "msg%d", &index)
						}
						if index >= spec.Pages {
							write(map[string]any{"data": []any{}, "cursor": map[string]any{}})
							return
						}
						rows := make([]any, 0, spec.RowsPerPage)
						for i := 0; i < spec.RowsPerPage; i++ {
							rows = append(rows, synthRow(fmt.Sprintf("m_%d_%d", index, i), spec.Pad, nowMs))
						}
						next := map[string]any{}
						if index+1 < spec.Pages {
							next = map[string]any{"next": fmt.Sprintf("msg%d", index+1)}
						}
						write(map[string]any{"data": rows, "cursor": next})
						return
					}
					pages := f.seed.Messages[id]
					cursor := r.URL.Query().Get("cursor")
					matched := false
					for _, page := range pages {
						if page.Cursor == cursor {
							matched = true
							rows := page.Rows
							if rows == nil {
								rows = []json.RawMessage{}
							}
							cursorOut := map[string]any{}
							if page.Next != "" {
								cursorOut = map[string]any{"next": page.Next}
							}
							write(map[string]any{"data": rows, "cursor": cursorOut})
							return
						}
					}
					if !matched {
						write(map[string]any{"data": []json.RawMessage{}, "cursor": map[string]any{}})
					}
					return
				}
				http.Error(w, "unexpected operation", 400)
				return
			}
			http.Error(w, "unexpected operation", 400)
			return
		}
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

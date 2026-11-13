package main

// Isolated native session API fixture for retention cleanup. The daemon
// engine starts this executable as its OpenCode binary. The parent test
// drives inventory shapes through cleanup-seed.json in the profile root
// (the fixture working directory); the fixture reloads it whenever its
// content changes, so every phase gets a deterministic inventory without
// restarting the daemon. Local deletions apply to an in-memory overlay.

import (
	"bufio"
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

type seedRow struct {
	ID     string `json:"id"`
	Parent string `json:"parentID"`
	Time   struct {
		Created uint64 `json:"created"`
		Updated uint64 `json:"updated"`
	} `json:"time"`
}

type seedPage struct {
	Cursor string    `json:"cursor"`
	Rows   []seedRow `json:"rows"`
	Next   string    `json:"next"`
}

type seedGetOverride struct {
	Status   int             `json:"status"`
	Row      *seedRow        `json:"row"`
	Mismatch string          `json:"mismatch"`
	Fresh    bool            `json:"fresh"`
	Body     json.RawMessage `json:"body"`
}

type seedDeleteOverride struct {
	Status int `json:"status"`
}

type nativeSeed struct {
	Pages      []seedPage                    `json:"pages"`
	Active     map[string]json.RawMessage    `json:"active"`
	ActiveNull bool                          `json:"activeNull"`
	ActiveFlip []string                      `json:"activeFlip"`
	Get        map[string]seedGetOverride    `json:"get"`
	Delete     map[string]seedDeleteOverride `json:"delete"`
}

type fixtureCall struct {
	Method string          `json:"method"`
	Path   string          `json:"path"`
	Body   json.RawMessage `json:"body"`
}

type cleanupFixture struct {
	mu         sync.Mutex
	root       string
	seedHash   string
	seed       nativeSeed
	deleted    map[string]bool
	live       int
	activeHits int
}

func seedPath(root string) string  { return filepath.Join(root, "cleanup-seed.json") }
func callsPath(root string) string { return filepath.Join(root, "cleanup-calls.jsonl") }

func (f *cleanupFixture) sync() {
	raw, err := os.ReadFile(seedPath(f.root))
	if err != nil {
		return
	}
	sum := sha256.Sum256(raw)
	hash := hex.EncodeToString(sum[:])
	if hash == f.seedHash {
		return
	}
	var seed nativeSeed
	if err := json.Unmarshal(raw, &seed); err != nil {
		return
	}
	f.seed = seed
	f.seedHash = hash
	f.deleted = map[string]bool{}
	f.activeHits = 0
}

func (f *cleanupFixture) logCall(method, path string, body json.RawMessage) {
	file, err := os.OpenFile(callsPath(f.root), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return
	}
	defer file.Close()
	raw, _ := json.Marshal(fixtureCall{Method: method, Path: path, Body: body})
	raw = append(raw, '\n')
	_, _ = file.Write(raw)
}

func (f *cleanupFixture) findRow(id string) *seedRow {
	for _, page := range f.seed.Pages {
		for i := range page.Rows {
			if page.Rows[i].ID == id && !f.deleted[id] {
				row := page.Rows[i]
				return &row
			}
		}
	}
	return nil
}

type statusWriter struct {
	http.ResponseWriter
	status int
}

func (w *statusWriter) WriteHeader(status int) {
	w.status = status
	w.ResponseWriter.WriteHeader(status)
}

func (w *statusWriter) Hijack() (net.Conn, *bufio.ReadWriter, error) {
	hijacker, ok := w.ResponseWriter.(http.Hijacker)
	if !ok {
		return nil, nil, errors.New("streaming unavailable")
	}
	return hijacker.Hijack()
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
	f := &cleanupFixture{root: root, deleted: map[string]bool{}}
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
			f.live++
			write(map[string]any{"data": map[string]any{"id": fmt.Sprintf("ses_live_%d", f.live)}})
			return
		}
		if r.Method == "GET" && path == "/api/event" {
			hijacker, ok := w.(http.Hijacker)
			if !ok {
				http.Error(w, "streaming unavailable", 500)
				return
			}
			conn, buf, err := hijacker.Hijack()
			if err != nil {
				return
			}
			_, _ = buf.WriteString("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n")
			_ = buf.Flush()
			_ = conn.SetDeadline(time.Now().Add(9 * time.Minute))
			one := make([]byte, 1)
			_, _ = conn.Read(one)
			_ = conn.Close()
			return
		}
		if strings.HasPrefix(path, "/api/session/") && path != "/api/session/active" {
			rest := strings.TrimPrefix(path, "/api/session/")
			if index := strings.Index(rest, "/"); index >= 0 {
				action := rest[index+1:]
				if action == "model" || action == "agent" {
					write(map[string]any{})
					return
				}
				http.Error(w, "unexpected operation", 400)
				return
			}
			id := rest
			if id == "" {
				http.Error(w, "native session ID required", 400)
				return
			}
			switch r.Method {
			case "GET":
				if override, ok := f.seed.Get[id]; ok {
					switch {
					case override.Status != 0:
						http.Error(w, "injected fetch failure", override.Status)
						return
					case override.Mismatch != "":
						write(map[string]any{"data": map[string]any{"id": override.Mismatch, "time": map[string]any{"created": 1, "updated": 1}}})
						return
					case override.Fresh:
						now := uint64(time.Now().UnixMilli())
						write(map[string]any{"data": map[string]any{"id": id, "time": map[string]any{"created": now, "updated": now}}})
						return
					case override.Row != nil:
						write(map[string]any{"data": override.Row})
						return
					case len(override.Body) > 0:
						_, _ = w.Write(override.Body)
						return
					}
				}
				if row := f.findRow(id); row != nil {
					write(map[string]any{"data": row})
					return
				}
				http.Error(w, "no such session", 404)
				return
			case "DELETE":
				if override, ok := f.seed.Delete[id]; ok && override.Status != 0 {
					http.Error(w, "injected delete failure", override.Status)
					return
				}
				if f.findRow(id) == nil {
					http.Error(w, "no such session", 404)
					return
				}
				f.deleted[id] = true
				write(map[string]any{})
				return
			}
			http.Error(w, "unexpected operation", 400)
			return
		}
		if r.Method == "GET" && (path == "/api/session" || path == "/api/session/active") {
			if path == "/api/session/active" {
				if f.seed.ActiveNull {
					write(map[string]any{"data": nil})
					return
				}
				active := map[string]json.RawMessage{}
				for id, value := range f.seed.Active {
					active[id] = value
				}
				if f.activeHits > 0 {
					for _, id := range f.seed.ActiveFlip {
						active[id] = json.RawMessage(`{}`)
					}
				}
				f.activeHits++
				write(map[string]any{"data": active})
				return
			}
			cursor := r.URL.Query().Get("cursor")
			for _, page := range f.seed.Pages {
				if page.Cursor == cursor {
					rows := []seedRow{}
					for _, row := range page.Rows {
						if !f.deleted[row.ID] {
							rows = append(rows, row)
						}
					}
					if rows == nil {
						rows = []seedRow{}
					}
					cursorOut := map[string]any{}
					if page.Next != "" {
						cursorOut = map[string]any{"next": page.Next}
					}
					write(map[string]any{"data": rows, "cursor": cursorOut})
					return
				}
			}
			write(map[string]any{"data": []seedRow{}, "cursor": map[string]any{}})
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

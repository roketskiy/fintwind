package main

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
	"time"

	"fintwind-go/internal/daemon"
	"github.com/google/uuid"
)

// This CLI fixture runs only when the daemon launches this check executable
// in its owned engine Job with a private home and generated HTTP password.
func serveGenerationFixture() error {
	password := os.Getenv("OPENCODE_SERVER_PASSWORD")
	if password == "" || os.Getenv("OPENCODE_TEST_HOME") == "" {
		return errors.New("isolated fixture environment required")
	}
	root, err := os.Getwd()
	if err != nil {
		return err
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	defer listener.Close()
	server := &http.Server{ReadHeaderTimeout: time.Second, Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		user, key, ok := r.BasicAuth()
		if !ok || user != "opencode" || key != password {
			http.Error(w, "unauthorized", 401)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		if r.Method == "GET" && r.URL.Path == "/api/info" {
			_, _ = w.Write([]byte(`{}`))
			return
		}
		if r.Method != "POST" || r.URL.Path != "/api/session/side/generate" {
			http.Error(w, "unexpected operation", 400)
			return
		}
		var body struct {
			Prompt string `json:"prompt"`
		}
		if json.NewDecoder(r.Body).Decode(&body) != nil {
			http.Error(w, "invalid body", 400)
			return
		}
		if strings.HasSuffix(body.Prompt, "\n\nblock") {
			if err := os.WriteFile(filepath.Join(root, "generation-entered"), []byte("isolated"), 0o600); err != nil {
				http.Error(w, "fixture marker failed", 500)
				return
			}
			<-r.Context().Done()
			return
		}
		if strings.HasSuffix(body.Prompt, "\n\nlong") {
			timer := time.NewTimer(31 * time.Second)
			defer timer.Stop()
			select {
			case <-r.Context().Done():
				return
			case <-timer.C:
			}
		}
		_, _ = w.Write([]byte(`{"data":{"text":"isolated daemon answer"}}`))
	})}
	deadline, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	go func() { <-deadline.Done(); _ = server.Close() }()
	fmt.Println("http://" + listener.Addr().String())
	err = server.Serve(listener)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

func checkGenerationDaemon(ctx context.Context, c *daemon.Client, round string) error {
	self, err := os.Executable()
	if err != nil {
		return err
	}
	request := func(id, question string) (json.RawMessage, error) {
		return c.Request(ctx, map[string]any{"type": "generateSessionText", "generationId": id, "binary": self, "directory": filepath.Join(round, "workspace"), "sessionId": "side", "question": question})
	}
	id := uuid.NewString()
	finished := make(chan error, 1)
	go func() { _, err := request(id, "block"); finished <- err }()
	deadline, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	tick := time.NewTicker(20 * time.Millisecond)
	defer tick.Stop()
	waiting := true
	for waiting {
		select {
		case <-deadline.Done():
			return deadline.Err()
		case err := <-finished:
			return fmt.Errorf("side generation ended before fixture entry: %v", err)
		case <-tick.C:
			if _, err := os.Stat(filepath.Join(round, "generation-entered")); err == nil {
				waiting = false
			}
		}
	}
	cancelCtx, stop := context.WithTimeout(ctx, 3*time.Second)
	defer stop()
	if _, err := c.Request(cancelCtx, map[string]any{"type": "cancelSessionGeneration", "generationId": id}); err != nil {
		return fmt.Errorf("same-connection cancellation blocked: %w", err)
	}
	select {
	case <-cancelCtx.Done():
		return cancelCtx.Err()
	case err := <-finished:
		if err == nil || !strings.Contains(err.Error(), "canceled") {
			return errors.New("daemon generation cancellation did not reach HTTP exchange")
		}
	}
	start := time.Now()
	raw, err := request(uuid.NewString(), "long")
	if err != nil {
		return err
	}
	var response struct{ Type, Text string }
	if json.Unmarshal(raw, &response) != nil || response.Type != "sessionTextGenerated" || response.Text != "isolated daemon answer" {
		return errors.New("daemon generation answer mismatch")
	}
	artifact, _ := json.Marshal(map[string]any{"sameConnectionCancel": true, "longResponseMs": time.Since(start).Milliseconds(), "realModelTested": false})
	return os.WriteFile(filepath.Join(round, "generation-daemon.json"), artifact, 0o600)
}

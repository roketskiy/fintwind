package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"sync"
	"time"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/ownedprocess"
)

func main() {
	binary := flag.String("binary", "", "optional absolute OpenCode binary for isolated process verification")
	flag.Parse()
	if err := run(*binary); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(binary string) (err error) {
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	modelAvailability := "not checked"
	defer func() {
		raw, _ := json.MarshalIndent(map[string]any{"checks": checks, "passed": err == nil, "realModelTested": false, "isolatedDeepseekAvailability": modelAvailability}, "", "  ")
		_ = os.WriteFile(filepath.Join(round, "engine-result.json"), raw, 0o600)
		fmt.Println("[REPORT]", filepath.Join(round, "engine-result.json"))
	}()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	defer listener.Close()
	var mu sync.Mutex
	var prompts int
	mux := http.NewServeMux()
	mux.HandleFunc("/api/session", func(w http.ResponseWriter, r *http.Request) {
		user, password, ok := r.BasicAuth()
		if !ok || user != "opencode" || password != "fixture-password" {
			http.Error(w, "unauthorized", 401)
			return
		}
		if r.Header.Get("x-opencode-directory") == "" {
			http.Error(w, "missing directory", 400)
			return
		}
		var body struct {
			Location struct {
				Directory string `json:"directory"`
			} `json:"location"`
		}
		if r.Method != "POST" || json.NewDecoder(r.Body).Decode(&body) != nil || body.Location.Directory != round {
			http.Error(w, "wrong create payload", 400)
			return
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{"id": "ses_fixture"}})
	})
	mux.HandleFunc("/api/session/ses_fixture/prompt", func(w http.ResponseWriter, r *http.Request) {
		var body struct {
			ID       string `json:"id"`
			Text     string `json:"text"`
			Delivery string `json:"delivery"`
		}
		if r.Method != "POST" || json.NewDecoder(r.Body).Decode(&body) != nil || body.ID != "msg_fixture" || body.Delivery != "steer" {
			http.Error(w, "bad prompt", 400)
			return
		}
		mu.Lock()
		prompts++
		mu.Unlock()
		http.Error(w, "ambiguous provider conflict", 409)
	})
	mux.HandleFunc("/api/event", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, "data: {\"type\":\"session.text.delta\",\"data\":{\"sessionID\":\"ses_fixture\",\"assistantMessageID\":\"msg_assistant\",\"ordinal\":0,\"delta\":\"literal\"}}\n\n")
		w.(http.Flusher).Flush()
		<-r.Context().Done()
	})
	server := &http.Server{Handler: mux, ReadHeaderTimeout: time.Second}
	go server.Serve(listener)
	defer server.Close()
	client, err := backendengine.Connect(listener.Addr().String(), "fixture-password", round)
	if err != nil {
		return err
	}
	defer client.Close()
	if _, err := backendengine.Connect("192.0.2.1:1", "password", round); err == nil {
		return errors.New("non-loopback accepted")
	}
	checks["non-loopback-refused"] = true
	id, err := client.CreateSession(ctx)
	if err != nil || id != "ses_fixture" {
		return errors.New("create session contract mismatch")
	}
	checks["create-session-contract"] = true
	stream, err := client.Events(ctx)
	if err != nil {
		return err
	}
	select {
	case event := <-stream:
		if event.Kind != "session.text.delta" || event.SessionID != "ses_fixture" {
			return errors.New("native stream routing mismatch")
		}
	case <-ctx.Done():
		return ctx.Err()
	}
	checks["native-sse"] = true
	state, err := client.Prompt(ctx, id, "msg_fixture", "hello", nil)
	if err == nil || state != "unknown" {
		return errors.New("409 must be unknown, not safe-to-retry")
	}
	mu.Lock()
	count := prompts
	mu.Unlock()
	if count != 1 {
		return errors.New("prompt was retried")
	}
	checks["unknown-post-never-retried"] = true
	if err := extendedContracts(ctx, round); err != nil {
		return err
	}
	checks["resume-paginated-transcript-forms-permissions"] = true
	if binary != "" {
		process, err := backendengine.Start(ctx, binary, round, round, "", false)
		if err != nil {
			return err
		}
		if catalog, err := process.Client.Catalog(ctx); err == nil {
			if bytes.Contains(catalog, []byte("deepseek-v4.1-flash")) {
				modelAvailability = "listed in isolated model catalog; inference not attempted"
			} else {
				modelAvailability = "not listed in isolated model catalog"
			}
		} else {
			modelAvailability = "catalog query failed; inference not attempted"
		}
		if err := process.Close(); err != nil {
			return err
		}
		checks["real-owned-engine-start-stop"] = true
	}
	return nil
}

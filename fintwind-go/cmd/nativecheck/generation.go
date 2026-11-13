package main

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendservices"
	"fintwind-go/internal/backendstore"
	"github.com/google/uuid"
)

func checkGeneration(ctx context.Context, round string, checks map[string]bool) error {
	var mu sync.Mutex
	calls := []exchange{}
	entered := make(chan struct{}, 64)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body json.RawMessage
		if r.Method == "POST" {
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				http.Error(w, "bad body", 400)
				return
			}
		}
		mu.Lock()
		calls = append(calls, exchange{r.Method, r.URL.Path, body})
		mu.Unlock()
		w.Header().Set("Content-Type", "application/json")
		if r.Method == "GET" {
			_, _ = w.Write([]byte(`{"data":{"model":{"providerID":"fixture","id":"model","variant":"fast"}}}`))
			return
		}
		if r.URL.Path != "/api/session/side/generate" {
			http.Error(w, "unexpected mutation", 400)
			return
		}
		var request struct {
			Prompt string `json:"prompt"`
		}
		_ = json.Unmarshal(body, &request)
		switch {
		case strings.HasSuffix(request.Prompt, "\n\nblock"):
			select {
			case entered <- struct{}{}:
			default:
			}
			<-r.Context().Done()
			return
		case strings.HasSuffix(request.Prompt, "\n\nlong"):
			timer := time.NewTimer(31 * time.Second)
			defer timer.Stop()
			select {
			case <-r.Context().Done():
				return
			case <-timer.C:
			}
		case strings.HasSuffix(request.Prompt, "\n\nambiguous"):
			http.Error(w, "unknown result", 504)
			return
		case strings.HasSuffix(request.Prompt, "\n\nempty"):
			_, _ = w.Write([]byte(`{"data":{"text":"  "}}`))
			return
		case strings.HasSuffix(request.Prompt, "\n\ninvalid"):
			_, _ = w.Write([]byte(`{"text":"wrong envelope"}`))
			return
		}
		_, _ = w.Write([]byte(`{"data":{"text":"  isolated answer  "}}`))
	}))
	defer server.Close()
	engine, err := backendengine.Connect(strings.TrimPrefix(server.URL, "http://"), "isolated-fixture", round)
	if err != nil {
		return err
	}
	defer engine.Close()
	service := backendservices.New(round, func(context.Context, backendstore.Object) (backendservices.API, error) { return engine, nil })
	call := func(kind, id, question string, model any) (backendstore.Object, error) {
		raw, _ := json.Marshal(map[string]any{"type": kind, "generationId": id, "sessionId": "side", "question": question, "model": model})
		response, handled, err := service.Handle(ctx, raw)
		if err != nil {
			return nil, err
		}
		if !handled {
			return nil, errors.New("generation command unhandled")
		}
		return backendstore.Decode(response)
	}
	count := func() int { mu.Lock(); defer mu.Unlock(); return len(calls) }
	for _, scenario := range []struct{ id, question string }{{uuid.NewString(), " "}, {"invalid", "question"}} {
		if _, err := call("generateSessionText", scenario.id, scenario.question, nil); err == nil {
			return errors.New("invalid side generation accepted")
		}
	}
	id := uuid.NewString()
	if _, err := call("cancelSessionGeneration", id, "", nil); err != nil {
		return err
	}
	if _, err := call("generateSessionText", id, "question", nil); err == nil {
		return errors.New("pre-cancelled generation executed")
	}
	if count() != 0 {
		return errors.New("invalid generation reached HTTP")
	}
	checks["generation-invalid-and-pre-cancel-no-http"] = true
	before := count()
	result, err := call("generateSessionText", uuid.NewString(), "question", map[string]any{"model": "fixture/other", "variant": "fast"})
	if err != nil {
		return err
	}
	if result.String("type") != "sessionGenerationModelNotSynced" || count() != before+1 {
		return errors.New("model mismatch did not stop generation")
	}
	result, err = call("generateSessionText", uuid.NewString(), "question", map[string]any{"model": "fixture/model"})
	if err != nil {
		return err
	}
	if result.String("type") != "sessionGenerationModelNotSynced" {
		return errors.New("variant mismatch accepted")
	}
	checks["generation-model-and-variant-readonly-guard"] = true
	for _, question := range []string{"empty", "invalid", "ambiguous"} {
		before := count()
		if _, err := call("generateSessionText", uuid.NewString(), question, nil); err == nil {
			return errors.New("invalid generation response accepted")
		}
		if count() != before+1 {
			return errors.New("generation POST retried")
		}
	}
	checks["generation-invalid-answer-and-no-post-retry"] = true
	id = uuid.NewString()
	finished := make(chan error, 1)
	go func() { _, err := call("generateSessionText", id, "block", nil); finished <- err }()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-entered:
	}
	if _, err := call("generateSessionText", id, "question", nil); err == nil {
		return errors.New("duplicate active generation accepted")
	}
	if _, err := call("cancelSessionGeneration", id, "", nil); err != nil {
		return err
	}
	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-finished:
		if !errors.Is(err, context.Canceled) {
			return errors.New("generation cancellation not propagated")
		}
	}
	checks["generation-active-cancel-without-main-interrupt"] = true
	// Hold every permitted exchange at the real HTTP boundary, then verify
	// the next request fails before sending a POST. Release only our requests.
	ids := make([]string, 64)
	capacityResults := make(chan error, 64)
	for i := range ids {
		ids[i] = uuid.NewString()
		id := ids[i]
		go func() { _, err := call("generateSessionText", id, "block", nil); capacityResults <- err }()
	}
	for range ids {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-entered:
		}
	}
	before = count()
	if _, err := call("generateSessionText", uuid.NewString(), "question", nil); err == nil || !strings.Contains(err.Error(), "capacity") {
		return errors.New("generation capacity was not enforced")
	}
	if count() != before {
		return errors.New("over-capacity generation reached HTTP")
	}
	for _, id := range ids {
		if _, err := call("cancelSessionGeneration", id, "", nil); err != nil {
			return err
		}
	}
	for range ids {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case err := <-capacityResults:
			if !errors.Is(err, context.Canceled) {
				return errors.New("capacity cleanup did not cancel owned exchange")
			}
		}
	}
	checks["generation-64-active-capacity-and-owned-cancel"] = true
	before = count()
	result, err = call("generateSessionText", uuid.NewString(), "  question  ", map[string]any{"model": "fixture/model", "variant": "fast"})
	if err != nil {
		return err
	}
	if result.String("type") != "sessionTextGenerated" || result.String("text") != "isolated answer" || count() != before+2 {
		return errors.New("generation success payload mismatch")
	}
	checks["generation-trimmed-answer-http"] = true
	start := time.Now()
	result, err = call("generateSessionText", uuid.NewString(), "long", nil)
	if err != nil {
		return err
	}
	if result.String("text") != "isolated answer" {
		return errors.New("long generation response mismatch")
	}
	checks["generation-header-and-request-exceed-30s"] = true
	mu.Lock()
	snapshot := append([]exchange(nil), calls...)
	mu.Unlock()
	for _, entry := range snapshot {
		if entry.Method == "GET" && entry.Path == "/api/session/side" {
			continue
		}
		if entry.Method == "POST" && entry.Path == "/api/session/side/generate" {
			continue
		}
		return errors.New("side generation touched main session mutation")
	}
	raw, err := json.MarshalIndent(map[string]any{"exchanges": snapshot, "longResponseMs": time.Since(start).Milliseconds(), "paidRequests": false}, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(round, "generation-http.json"), raw, 0o600)
}

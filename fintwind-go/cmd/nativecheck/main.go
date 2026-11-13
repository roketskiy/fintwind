package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendservices"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/ownedprocess"
)

type exchange struct {
	Method, Path string
	Body         json.RawMessage
}
type fixture struct {
	calls           []exchange
	boundary        string
	failFork, cycle bool
}

func (f *fixture) RoundTrip(ctx context.Context, method, path string, body any) (json.RawMessage, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	raw, _ := json.Marshal(body)
	f.calls = append(f.calls, exchange{method, path, raw})
	if strings.Contains(path, "/message?") {
		if f.cycle {
			return json.RawMessage(`{"data":[{"id":"u2","type":"user"}],"cursor":{"next":"loop"}}`), nil
		}
		if strings.Contains(path, "cursor=older") {
			return json.RawMessage(`{"data":[{"id":"u1","type":"user","content":[{"type":"text","text":"literal first"}]}],"cursor":{}}`), nil
		}
		return json.RawMessage(`{"data":[{"id":"a2","type":"assistant","time":{"created":2000,"streamed":2500},"tokens":{"output":10,"reasoning":5},"model":{"providerID":"fixture","id":"second"},"agent":"build"},{"id":"u2","type":"user","content":[{"type":"text","text":"literal second"}]},{"id":"a1","type":"assistant","time":{"created":1000,"streamed":1200},"tokens":{"output":3,"reasoning":2},"model":{"providerID":"fixture","id":"first"}}],"cursor":{"next":"older"}}`), nil
	}
	if strings.HasSuffix(path, "/fork") {
		if f.failFork {
			return nil, errors.New("ambiguous transport failure")
		}
		return json.RawMessage(`{"data":{"id":"ses_fork"}}`), nil
	}
	if method == "GET" {
		raw, _ := json.Marshal(map[string]any{"data": map[string]any{"revert": map[string]string{"messageID": f.boundary}}})
		return raw, nil
	}
	return json.RawMessage(`{}`), nil
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}
func run() (err error) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	api := &fixture{}
	defer func() {
		report := map[string]any{"passed": err == nil, "checks": checks, "lastExchanges": api.calls}
		if err != nil {
			report["error"] = err.Error()
		}
		raw, e := json.MarshalIndent(report, "", "  ")
		if e == nil {
			e = os.WriteFile(filepath.Join(round, "native-result.json"), raw, 0o600)
		}
		err = errors.Join(err, e)
		fmt.Println("[REPORT]", filepath.Join(round, "native-result.json"))
	}()
	httpServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		user, password, ok := r.BasicAuth()
		if !ok || user != "opencode" || password != "isolated-fixture" || r.Header.Get("x-opencode-directory") == "" {
			http.Error(w, "unauthorized", 401)
			return
		}
		var body any
		if r.Body != nil && r.ContentLength != 0 {
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				http.Error(w, "invalid body", 400)
				return
			}
		}
		raw, err := api.RoundTrip(r.Context(), r.Method, r.URL.RequestURI(), body)
		if err != nil {
			http.Error(w, "injected failure", 504)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write(raw)
	}))
	defer httpServer.Close()
	engine, err := backendengine.Connect(strings.TrimPrefix(httpServer.URL, "http://"), "isolated-fixture", round)
	if err != nil {
		return err
	}
	defer engine.Close()
	service := backendservices.New(round, func(context.Context, backendstore.Object) (backendservices.API, error) { return engine, nil })
	call := func(kind string, count int) (backendstore.Object, error) {
		raw, _ := json.Marshal(map[string]any{"type": "forkProviderSession", "request": map[string]any{"provider": kind, "binary": "fixture", "cwd": round, "session_id": "ses_source", "turn_count": count}})
		response, handled, err := service.Handle(ctx, raw)
		if !handled && err == nil {
			return nil, errors.New("native command unhandled")
		}
		if err != nil {
			return nil, err
		}
		return backendstore.Decode(response)
	}
	if _, err := call("openCode", 3); err == nil {
		return errors.New("retained native overflow accepted")
	}
	checks["retain-overflow-refused"] = true
	api.calls = nil
	api.cycle = true
	if _, err := call("openCode", 1); err == nil {
		return errors.New("cycling history accepted")
	}
	api.cycle = false
	checks["history-cycle-refused"] = true
	api.calls = nil
	api.failFork = true
	if _, err := call("openCode", 1); err == nil {
		return errors.New("fork error hidden")
	}
	posts := 0
	for _, e := range api.calls {
		if e.Method == "POST" {
			posts++
		}
	}
	if posts != 1 {
		return errors.New("ambiguous fork repeated")
	}
	api.failFork = false
	checks["ambiguous-fork-not-retried"] = true
	api.calls = nil
	response, err := call("openCode", 1)
	if err != nil {
		return err
	}
	var result struct {
		Cursor struct {
			Session string `json:"sessionId"`
		} `json:"cursor"`
	}
	if err := json.Unmarshal(response["result"], &result); err != nil {
		return err
	}
	if response.String("type") != "providerSessionForked" || result.Cursor.Session != "ses_fork" {
		return errors.New("native fork wire mismatch")
	}
	last := api.calls[len(api.calls)-1]
	if string(last.Body) != `{"before":"u2"}` {
		return errors.New("fork did not use first discarded user")
	}
	checks["paginated-fork-before-boundary"] = true
	transcriptRequest, _ := json.Marshal(map[string]any{"type": "fetchNativeTranscript", "sessionId": "ses_source"})
	transcriptResponse, handled, err := service.Handle(ctx, transcriptRequest)
	if err != nil {
		return err
	}
	if !handled {
		return errors.New("native transcript command not handled")
	}
	var transcript struct {
		Transcript struct {
			Messages []struct {
				Content string `json:"content"`
			} `json:"messages"`
		} `json:"transcript"`
	}
	if err := json.Unmarshal(transcriptResponse, &transcript); err != nil {
		return err
	}
	if len(transcript.Transcript.Messages) != 2 || transcript.Transcript.Messages[0].Content != "literal first" || transcript.Transcript.Messages[1].Content != "literal second" {
		return errors.New("native transcript page order mismatch")
	}
	checks["native-transcript-http-page-order"] = true
	statsRequest, _ := json.Marshal(map[string]any{"type": "fetchNativeTurnStats", "sessionId": "ses_source", "stepIds": []string{"a1", "a2"}})
	statsRaw, handled, err := service.Handle(ctx, statsRequest)
	if err != nil {
		return err
	}
	if !handled {
		return errors.New("turn stats command not handled")
	}
	var stats struct {
		Stats struct {
			Output   uint64 `json:"output_tokens"`
			Stream   uint64 `json:"stream_ms"`
			Verified bool   `json:"stream_verified"`
			Model    string `json:"model"`
		} `json:"stats"`
	}
	if err := json.Unmarshal(statsRaw, &stats); err != nil {
		return err
	}
	if stats.Stats.Output != 20 || stats.Stats.Stream != 700 || !stats.Stats.Verified || stats.Stats.Model != "fixture/second" {
		return errors.New("turn stats did not use provider stream boundaries")
	}
	checks["native-turn-stats-verified-stream-boundaries"] = true
	statsRequest, _ = json.Marshal(map[string]any{"type": "fetchNativeTurnStats", "sessionId": "ses_source", "stepIds": []string{"a1", "a1"}})
	statsRaw, _, err = service.Handle(ctx, statsRequest)
	if err != nil {
		return err
	}
	var nullable struct {
		Stats json.RawMessage `json:"stats"`
	}
	if err := json.Unmarshal(statsRaw, &nullable); err != nil {
		return err
	}
	if string(nullable.Stats) != "null" {
		return errors.New("duplicate turn steps double counted")
	}
	checks["duplicate-native-step-ids-not-counted"] = true
	api.calls = nil
	if _, err := call("openCode", 2); err != nil {
		return err
	}
	last = api.calls[len(api.calls)-1]
	if string(last.Body) != "{}" {
		return errors.New("full fork body not empty object")
	}
	checks["retain-all-empty-object"] = true
	api.calls = nil
	if _, err := call("openCodeRevert", 1); err != nil {
		return err
	}
	writes := []exchange{}
	for _, e := range api.calls {
		if e.Method != "GET" {
			writes = append(writes, e)
		}
	}
	if len(writes) != 3 || writes[0].Method != "DELETE" || !strings.HasSuffix(writes[1].Path, "/revert/stage") || string(writes[1].Body) != `{"messageID":"u2"}` || !strings.HasSuffix(writes[2].Path, "/revert/commit") {
		return errors.New("revert sequence mismatch")
	}
	checks["revert-clear-stage-commit"] = true
	api.calls = nil
	api.boundary = "u2"
	if _, err := call("openCodeUndoTurn", 0); err != nil {
		return err
	}
	last = api.calls[len(api.calls)-1]
	if string(last.Body) != `{"messageID":"u1"}` || !strings.HasSuffix(last.Path, "/stage") {
		return errors.New("undo did not move to previous native boundary")
	}
	checks["undo-stages-previous-boundary"] = true
	api.boundary = "u1"
	if _, err := call("openCodeUndoTurn", 0); err == nil {
		return errors.New("undo past first message accepted")
	}
	checks["earliest-undo-refused"] = true
	api.calls = nil
	if _, err := call("openCodeRedoTurn", 0); err != nil {
		return err
	}
	if len(api.calls) != 1 || api.calls[0].Method != "DELETE" {
		return errors.New("redo must only clear staged boundary")
	}
	checks["redo-only-clears-boundary"] = true
	if err := checkGeneration(ctx, round, checks); err != nil {
		return err
	}
	return nil
}

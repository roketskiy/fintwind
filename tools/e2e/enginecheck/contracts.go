package main

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"path/filepath"
	"sync"

	"fintwind-go/internal/backendengine"
)

func extendedContracts(ctx context.Context, root string) error {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	defer listener.Close()
	var mu sync.Mutex
	seen := map[string]bool{}
	server := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body map[string]json.RawMessage
		if r.Body != nil {
			_ = json.NewDecoder(r.Body).Decode(&body)
		}
		key := r.Method + " " + r.URL.Path
		if key == "GET /api/session/ses_match" {
			_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{"location": map[string]string{"directory": root}}})
			return
		}
		if key == "GET /api/session/ses_other" {
			_ = json.NewEncoder(w).Encode(map[string]any{"location": map[string]string{"directory": filepath.Join(root, "other")}})
			return
		}
		if key == "GET /api/session/ses_restore/message" {
			if r.URL.Query().Get("cursor") == "next" {
				_ = json.NewEncoder(w).Encode(map[string]any{"data": []any{map[string]any{"id": "msg_input", "type": "user", "text": "literal question", "time": map[string]any{"created": 1000}}}})
			} else {
				_ = json.NewEncoder(w).Encode(map[string]any{"data": []any{map[string]any{"id": "msg_answer", "type": "assistant", "time": map[string]any{"created": 2000, "completed": 3000}, "content": []any{map[string]any{"type": "text", "text": "literal restored"}}}}, "cursor": map[string]string{"next": "next"}})
			}
			return
		}
		if key == "POST /api/session/ses_restore/form/frm_literal/reply" {
			var answer map[string]any
			if json.Unmarshal(body["answer"], &answer) != nil || answer["single"] != "yes" {
				http.Error(w, "single invalid", 422)
				return
			}
			if values, ok := answer["multi"].([]any); !ok || len(values) != 1 || values[0] != "x" {
				http.Error(w, "multi invalid", 422)
				return
			}
		}
		if key == "POST /api/session/ses_restore/permission/per_fixture/reply" {
			if string(body["decision"]) != `"once"` {
				http.Error(w, "persistent approval leak", 400)
				return
			}
		}
		mu.Lock()
		seen[key] = true
		mu.Unlock()
		_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{"id": "ses_restore"}})
	})}
	go server.Serve(listener)
	defer server.Close()
	client, err := backendengine.Connect(listener.Addr().String(), "password", root)
	if err != nil {
		return err
	}
	defer client.Close()
	if err := client.ResumeSession(ctx, "ses_restore"); err != nil {
		return err
	}
	if err := client.ResumeSession(ctx, "ses_match"); err != nil {
		return errors.New("resume rejected the same workspace")
	}
	if err := client.ResumeSession(ctx, "ses_other"); err == nil {
		return errors.New("resume accepted a different workspace")
	}
	transcript, err := client.Transcript(ctx, "ses_restore")
	if err != nil {
		return err
	}
	if len(transcript.Messages) != 2 || transcript.Messages[0].Content != "literal question" || transcript.Messages[1].Content != "literal restored" || len(transcript.Turns) != 1 {
		return errors.New("paginated native transcript did not preserve conversation")
	}
	answers := []backendengine.Answer{{QuestionID: "single", Answers: []string{"yes"}}, {QuestionID: "multi", Answers: []string{"x"}}}
	if err := client.Answer(ctx, "ses_restore", "frm_literal", answers, map[string]bool{"multi": true}); err != nil {
		return err
	}
	if err := client.CancelQuestion(ctx, "ses_restore", "frm_literal"); err != nil {
		return err
	}
	if err := client.Permission(ctx, "ses_restore", "per_fixture", "always"); err != nil {
		return err
	}
	mu.Lock()
	defer mu.Unlock()
	for _, key := range []string{"GET /api/session/ses_restore", "POST /api/session/ses_restore/form/frm_literal/reply", "POST /api/session/ses_restore/form/frm_literal/cancel", "POST /api/session/ses_restore/permission/per_fixture/reply"} {
		if !seen[key] {
			return errors.New("extended engine operation was not sent")
		}
	}
	return nil
}

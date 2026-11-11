//go:build wiringe2e

package main

import (
	"context"
	"encoding/json"
	"fmt"
	"sync"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

func wiringHistoryEditing(ctx context.Context, r *wiringReport, round string) {
	for _, scenario := range []string{"confirmed", "lost-reply", "wrong-session"} {
		wiringHistoryScenario(ctx, r, round, scenario)
	}
}

func wiringHistoryScenario(ctx context.Context, r *wiringReport, round, scenario string) {
	f, err := wiringNewFixture(ctx, round)
	if err != nil {
		r.check("history fixture "+scenario, "success", err.Error())
		return
	}
	defer f.close()
	defer f.stop()
	turnID, native, reference := "history-turn", "history-native", "opaque-history-reference"
	session := &model.Session{ID: "fixture-session", ProjectID: "fixture-project", Provider: "opencode", Status: "idle", RuntimeMode: "ask", InteractionMode: "build", NativeSessionID: &native, ProviderCursor: json.RawMessage(`{"provider":"openCode","sessionId":"history-native"}`),
		Turns:    []model.AgentTurn{{ID: turnID, TurnCount: 1, Status: "completed"}},
		Messages: []model.Message{{ID: "history-message", TurnID: &turnID, Role: "user", Content: "Original history", Attachments: []model.Attachment{{Path: "fixture/image.png", Name: "image.png", IsImage: true, BlobReference: &reference}}}},
	}
	var mu sync.Mutex
	var receipt map[string]any
	var accepted map[string]any
	var order []string
	saveEntered, releaseSave := make(chan struct{}), make(chan struct{})
	var enterOnce sync.Once
	defer func() {
		select {
		case <-releaseSave:
		default:
			close(releaseSave)
		}
	}()
	f.extended = func(envelope wiringEnvelope) (any, error, bool, bool) {
		kind := envelope.Command.Type
		mu.Lock()
		order = append(order, kind)
		mu.Unlock()
		switch kind {
		case "saveTaskStateCheckpoint":
			enterOnce.Do(func() {
				close(saveEntered)
				select {
				case <-releaseSave:
				case <-ctx.Done():
				}
			})
			return map[string]any{"type": "taskStateSaved", "sessions": []any{}}, nil, false, true
		case "rewindSessionToMessage":
			var body struct {
				TurnCount int `json:"turnCount"`
			}
			_ = json.Unmarshal(envelope.Command.Raw, &body)
			if body.TurnCount != 1 || envelope.SessionID != session.ID {
				return nil, fmt.Errorf("wrong rewind target"), false, true
			}
			if scenario == "lost-reply" {
				return nil, nil, true, true
			}
			id := session.ID
			if scenario == "wrong-session" {
				id = "another-session"
			}
			return map[string]any{"type": "sessionRewound", "session": map[string]any{"id": id, "project_id": "fixture-project", "provider": "opencode", "status": "idle", "runtime_mode": "ask", "interaction_mode": "build", "native_session_id": native, "provider_cursor": map[string]string{"provider": "openCode", "sessionId": native}, "messages": []any{}, "turns": []any{}, "transcript_blocks": []any{}, "future_history": json.Number("9007199254740993")}}, nil, false, true
		case "acceptSubmission":
			var body map[string]any
			_ = json.Unmarshal(envelope.Command.Raw, &body)
			entry, _ := body["submission"].(map[string]any)
			mu.Lock()
			accepted = body
			receipt, _ = entry["receipt"].(map[string]any)
			copyReceipt := settingsCloneObject(receipt)
			mu.Unlock()
			return map[string]any{"type": "submissionSaved", "receipt": copyReceipt}, nil, false, true
		case "attachSession":
			return map[string]any{"type": "sessionRuntime", "runtimeId": "fresh-history-runtime", "supportsSteer": true}, nil, false, true
		case "dispatchSubmission":
			mu.Lock()
			copyReceipt := settingsCloneObject(receipt)
			copyReceipt["state"] = "accepted"
			mu.Unlock()
			return map[string]any{"type": "submissionSaved", "receipt": copyReceipt}, nil, false, true
		}
		return nil, nil, false, false
	}
	client, err := daemon.Dial(ctx, f.server.Listener.Addr().String(), "")
	if err != nil {
		r.check("history handshake "+scenario, "success", err.Error())
		return
	}
	defer client.Close()
	a := wiringApp(ctx, client, round)
	a.projects[0].EnvironmentID = ""
	a.connection.phase = "online"
	a.detail = session
	a.detailRaw = map[string]any{"id": session.ID}
	a.stateForSession(session).raw = a.detailRaw
	queue, release := installHeadlessDispatcher(a)
	defer release()
	a.beginMessageEdit(0)
	a.editDraft = "Edited history"
	tt := ui.NewTester(func(c *ui.Context) { a.messageEditCard(c, &session.Messages[0]) }, 1000, 600)
	wiringClick(r, tt, "Send")
	r.check("history confirmation sends nothing "+scenario, 0, f.count("rewindSessionToMessage"))
	wiringClick(r, tt, "Keep editing")
	r.check("history cancel preserves original "+scenario, "Original history", session.Messages[0].Content)
	a.saveSessionSnapshot(session)
	select {
	case <-saveEntered:
	case <-time.After(4 * time.Second):
		r.check("history pending snapshot entered "+scenario, true, false)
		return
	}
	a.submitMessageEdit()
	a.confirmMessageEdit()
	a.confirmMessageEdit()
	r.check("history waits for pending save "+scenario, 0, f.count("rewindSessionToMessage"))
	r.check("history blocks new submissions "+scenario, false, a.submitSessionSubmission(sessionSubmission{sessionID: session.ID, text: "Must not send"}))
	a.steerQueuedMessage(session.ID, "missing")
	a.saveSessionSnapshot(session)
	r.check("history no early truncate "+scenario, 1, len(a.detail.Messages))
	close(releaseSave)
	if scenario == "confirmed" {
		r.check("history callback completes replacement", true, wiringApplyUntil(ctx, queue, func() bool {
			current := a.eventSession(session.ID)
			return f.count("dispatchSubmission") == 1 && !a.stateForSession(current).admissionBusy
		}))
		current := a.eventSession(session.ID)
		r.check("history exact replacement text", "Edited history", current.Messages[0].Content)
		r.check("history original attachment reference", reference, *current.Messages[0].Attachments[0].BlobReference)
		r.check("history raw unknown integer preserved", json.Number("9007199254740993"), a.rawSession(current)["future_history"])
		mu.Lock()
		saved := accepted
		sequence := append([]string(nil), order...)
		mu.Unlock()
		entry, _ := saved["submission"].(map[string]any)
		r.check("history production admission prompt", "Edited history", entry["prompt"])
		r.check("history order snapshot rewind admission", []string{"saveTaskStateCheckpoint", "rewindSessionToMessage", "acceptSubmission"}, sequence[:3])
		_ = a.applyEvent(daemon.Event{SessionID: session.ID, RuntimeID: "fixture-runtime", Kind: "autoTitleUpdated", Payload: json.RawMessage(`"Old runtime title"`)})
		r.check("history rejects retired runtime", true, current.AutoTitle == nil)
		r.check("history no bare prompt", 0, f.count("prompt"))
		r.check("history rewind exactly once", 1, f.count("rewindSessionToMessage"))
	} else {
		r.check("history unknown result protected "+scenario, true, wiringApplyUntil(ctx, queue, func() bool { op := a.sessionActions.operations[session.ID]; return op != nil && op.phase == "unknown" }))
		a.submitMessageEdit()
		a.confirmMessageEdit()
		r.check("history unknown cannot repeat "+scenario, 1, f.count("rewindSessionToMessage"))
		r.check("history unknown no replacement "+scenario, 0, f.count("acceptSubmission"))
		r.check("history unknown keeps draft "+scenario, "Edited history", a.editDraft)
		r.check("history unknown keeps attachments "+scenario, reference, a.editMessage.Attachments[0].Reference)
	}
}

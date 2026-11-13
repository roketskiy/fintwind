package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

func checkRestartRecovery(ctx context.Context, client *daemon.Client, child *ownedprocess.Owned, binary, fixture, round, workspace, token string, project map[string]any, sid, pid, rid string) error {
	before, err := hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	now := uint64(time.Now().Unix())
	turnID := uuid.NewString()
	record := model.NewSubmissionRecord(sid, turnID, "hold-during-text restart", nil)
	turn := map[string]any{"id": turnID, "status": "running", "turn_count": len(before.Turns) + 1, "started_at": now, "submission": record.Receipt}
	message := map[string]any{"id": uuid.NewString(), "turn_id": turnID, "role": "user", "content": record.Prompt, "created_at": now, "attachments": []any{}, "streaming": false}
	session := map[string]any{"id": sid, "project_id": pid, "title": "fixture", "provider": "openCode", "status": "working", "created_at": now, "updated_at": now, "detail_loaded": true,
		"messages": append(append([]any{}, before.RawMessages...), message), "turns": append(append([]any{}, before.RawTurns...), turn), "transcript_blocks": before.RawBlocks}
	if _, err := client.RequestFor(ctx, sid, "", map[string]any{"type": "acceptSubmission", "project": project, "session": session, "submission": record}); err != nil {
		return fmt.Errorf("admit restart turn: %w", err)
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return fmt.Errorf("dispatch restart turn: %w", err)
	}
	if err := waitPartial(ctx, client, sid, len(before.Messages)+2, "literal partial"); err != nil {
		return err
	}
	active, err := hydrateState(ctx, client, sid)
	if err != nil || active.Status != "working" || len(active.Messages) == 0 || !active.Messages[len(active.Messages)-1].Streaming {
		return errors.New("restart fixture was not streaming")
	}
	seed := func(status string) (string, model.SubmissionRecord, error) {
		id := uuid.NewString()
		pending := model.NewSubmissionRecord(id, uuid.NewString(), "pending restart", nil)
		pendingTurn := map[string]any{"id": pending.Receipt.ID, "status": "running", "turn_count": 1, "started_at": now, "submission": pending.Receipt}
		pendingMessage := map[string]any{"id": uuid.NewString(), "turn_id": pending.Receipt.ID, "role": "user", "content": pending.Prompt, "created_at": now, "attachments": []any{}, "streaming": false}
		pendingSession := map[string]any{"id": id, "project_id": pid, "title": "pending fixture", "provider": "openCode", "status": status, "created_at": now, "updated_at": now, "detail_loaded": true, "messages": []any{pendingMessage}, "turns": []any{pendingTurn}, "transcript_blocks": []any{}}
		_, err := client.RequestFor(ctx, id, "", map[string]any{"type": "acceptSubmission", "project": project, "session": pendingSession, "submission": pending})
		return id, pending, err
	}
	preparingID, preparing, err := seed("connecting")
	if err != nil {
		return err
	}
	unknownID, unknown, err := seed("working")
	if err != nil {
		return err
	}
	client.Close()
	if err := child.Close(); err != nil {
		return err
	}
	store, err := backendstore.Open(ctx, filepath.Join(round, "data"))
	if err != nil {
		return err
	}
	_, changed, transitionErr := store.Transition(ctx, unknownID, unknown.Receipt.ID, model.SubmissionPreparing, model.SubmissionDispatching, nil)
	closeErr := store.Close()
	if transitionErr != nil || closeErr != nil || !changed {
		return errors.New("could not seed an interrupted dispatch")
	}
	logs := filepath.Join(round, "restart-logs")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return err
	}
	restarted, err := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", filepath.Join(round, "data"), "--bind", "127.0.0.1:0"}, logs, round, ownedprocess.BuildEnv(round, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if err != nil {
		return err
	}
	defer restarted.Close()
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	address := ""
	for address == "" {
		raw, _ := os.ReadFile(restarted.StdoutPath)
		var ready struct {
			Address string `json:"address"`
			PID     uint32 `json:"pid"`
		}
		if json.Unmarshal(raw, &ready) == nil && ready.PID == restarted.PID {
			address = ready.Address
			break
		}
		if exited, _ := restarted.Exited(0); exited {
			return errors.New("restarted daemon exited")
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
		}
	}
	resumed, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer resumed.Close()
	for _, expected := range []struct {
		id, turn, state string
	}{{preparingID, preparing.Receipt.ID, model.SubmissionNotSent}, {unknownID, unknown.Receipt.ID, model.SubmissionUnknown}} {
		state, err := hydrateState(ctx, resumed, expected.id)
		if err != nil || state.Status != "idle" || state.TurnStatus(expected.turn) != "interrupted" || len(state.RawTurns) != 1 {
			return fmt.Errorf("pending restart state: %v", err)
		}
		encoded, _ := json.Marshal(state.RawTurns[0])
		var turn struct {
			Submission model.SubmissionReceipt `json:"submission"`
		}
		if err := json.Unmarshal(encoded, &turn); err != nil || turn.Submission.State != expected.state {
			return fmt.Errorf("pending receipt after restart: got %s, want %s", turn.Submission.State, expected.state)
		}
	}
	recovered, err := hydrateState(ctx, resumed, sid)
	if err != nil {
		return err
	}
	if recovered.Status != "idle" || recovered.TurnStatus(turnID) != "interrupted" || len(recovered.Messages) != len(before.Messages)+2 || recovered.Messages[len(recovered.Messages)-1].Content != "literal partial" || recovered.Messages[len(recovered.Messages)-1].Streaming {
		return errors.New("restart did not preserve and interrupt the running turn")
	}
	raw, err := resumed.Request(ctx, map[string]any{"type": "loadTaskState"})
	if err != nil {
		return err
	}
	var list struct {
		Sessions []struct{ ID, Status string } `json:"sessions"`
	}
	if err := json.Unmarshal(raw, &list); err != nil {
		return err
	}
	status := ""
	for _, item := range list.Sessions {
		if item.ID == sid {
			status = item.Status
		}
	}
	if status != "idle" {
		return errors.New("restarted session list still reports running")
	}
	var native []map[string]any
	for index := len(recovered.Messages) - 1; index >= 0; index-- {
		message := recovered.Messages[index]
		if message.Role != "user" && message.Role != "assistant" {
			continue
		}
		text := message.Content
		if index == len(recovered.Messages)-1 {
			text = "older provider snapshot"
		}
		native = append(native, map[string]any{"id": uuid.NewString(), "role": message.Role, "text": text})
	}
	nativeRaw, err := json.Marshal(map[string]any{"data": native})
	if err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(round, "home", "native-message-fixture.json"), nativeRaw, 0o600); err != nil {
		return err
	}
	nextID := uuid.NewString()
	next := model.NewSubmissionRecord(sid, nextID, "after restart", nil)
	nextTurn := map[string]any{"id": nextID, "status": "running", "turn_count": len(recovered.Turns) + 1, "started_at": now, "submission": next.Receipt}
	nextMessage := map[string]any{"id": uuid.NewString(), "turn_id": nextID, "role": "user", "content": next.Prompt, "created_at": now, "attachments": []any{}, "streaming": false}
	session["messages"] = append(append([]any{}, recovered.RawMessages...), nextMessage)
	session["turns"] = append(append([]any{}, recovered.RawTurns...), nextTurn)
	session["transcript_blocks"] = recovered.RawBlocks
	session["provider_cursor"] = map[string]any{"provider": "openCode", "sessionId": "ses_fixture"}
	if _, err := resumed.RequestFor(ctx, sid, "", map[string]any{"type": "acceptSubmission", "project": project, "session": session, "submission": next}); err != nil {
		return fmt.Errorf("admit after restart: %w", err)
	}
	newRuntime := uuid.NewString()
	options := map[string]any{"binary": fixture, "cwd": workspace, "mode": "ask", "interactionMode": "code", "model": "fixture/literal", "providerCursor": session["provider_cursor"]}
	if _, err := resumed.RequestFor(ctx, sid, newRuntime, map[string]any{"type": "start", "options": options}); err != nil {
		return fmt.Errorf("resume runtime: %w", err)
	}
	if _, err := resumed.RequestFor(ctx, sid, newRuntime, map[string]any{"type": "dispatchSubmission", "submissionId": nextID, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return fmt.Errorf("dispatch after restart: %w", err)
	}
	text, success, err := waitTurn(ctx, resumed)
	if err != nil || !success || text != "literal answer" {
		return fmt.Errorf("after restart response: %q, success=%t, err=%v", text, success, err)
	}
	final, err := hydrateState(ctx, resumed, sid)
	if err != nil {
		return err
	}
	if final.TurnStatus(turnID) != "interrupted" || final.TurnStatus(nextID) != "completed" || len(final.Messages) != len(recovered.Messages)+2 || final.Messages[len(recovered.Messages)-1].Content != "literal partial" || final.Messages[len(recovered.Messages)].Content != "after restart" {
		return errors.New("resuming lost the interrupted or newly submitted transcript")
	}
	return nil
}

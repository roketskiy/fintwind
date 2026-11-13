package backendruntime

import (
	"context"
	"errors"
	"strings"
	"time"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

func (a *actor) run() {
	defer close(a.done)
	defer a.process.Close()
	defer a.cancel()
	defer a.flush()
	_ = a.emit("connected", map[string]string{"provider": "openCode", "sessionId": a.nativeID})
	if a.snapshot != nil {
		if err := a.flush(); err != nil {
			_ = a.emit("error", "restored snapshot could not be saved")
			return
		}
		if err := a.publishBackground(); err != nil {
			return
		}
		_ = a.refreshChildren(a.ctx)
	}
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-a.ctx.Done():
			return
		case command := <-a.commands:
			value, err := a.handle(command.ctx, command.command)
			command.reply <- outcome{value: value, err: err}
		case result := <-a.backgroundResults:
			if result.generation == a.backgroundGeneration && result.revision == a.backgroundRevision {
				if result.err != nil {
					_ = a.emit("error", "Could not refresh background sessions")
				} else if err := a.applyChildren(result); err != nil {
					_ = a.emit("error", "Could not restore background sessions")
				}
			} else if result.generation == a.backgroundGeneration && len(a.unresolvedChildEvents) > 0 {
				_ = a.refreshChildren(a.ctx)
			}
		case event, ok := <-a.events:
			if !ok {
				_ = a.flush()
				_ = a.emit("error", "OpenCode stream ended; session recovery required")
				return
			}
			if strings.HasPrefix(event.Kind, "shell.") {
				if err := a.shellEvent(event); err != nil {
					_ = a.emit("error", "Could not update background process")
				}
				continue
			}
			if event.SessionID != a.nativeID {
				if err := a.childEvent(event); err != nil {
					_ = a.flush()
					_ = a.emit("error", "background session recovery required")
					return
				}
				continue
			}
			if err := a.event(event); err != nil {
				_ = a.emit("error", "runtime persistence or event error; recovery required")
				return
			}
		case <-ticker.C:
			a.pollShells()
			a.pollGoal()
			if err := a.flush(); err != nil {
				_ = a.emit("error", "runtime save failed; recovery required")
				return
			}
		case results := <-a.shellResults:
			a.shellReading = false
			if err := a.applyShells(results); err != nil {
				_ = a.emit("error", "Could not refresh background processes")
			}
		case result := <-a.goalResults:
			a.goalReading = false
			if result.revision == a.goalRevision && result.err == nil {
				_ = a.saveGoal(result.goal)
			}
		}
	}
}

func (a *actor) emit(kind string, payload any) error {
	if a.parent != nil {
		return a.parent.childEmission(a.child, kind, payload)
	}
	return a.manager.emit(a.sessionID, a.runtimeID, kind, payload)
}

func (a *actor) flush() error {
	if !a.dirty || a.snapshot == nil {
		return nil
	}
	if err := a.syncSnapshot(); err != nil {
		return err
	}
	if a.parent != nil {
		a.parent.dirty = true
		a.dirty = false
		return nil
	}
	if a.manager.EventCursor != nil {
		cursor, ok := a.manager.EventCursor(a.sessionID, a.runtimeID)
		if !ok {
			return errors.New("runtime event cursor unavailable")
		}
		a.snapshot.Set("runtime_event_cursor", cursor)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := a.manager.store.SaveRuntime(ctx, a.snapshot); err != nil {
		return err
	}
	a.dirty = false
	return nil
}

func (a *actor) handle(ctx context.Context, command backendstore.Object) (backendstore.Object, error) {
	if value, handled, err := a.controls(ctx, command); handled {
		return value, err
	}
	switch command.String("type") {
	case "admitScheduled":
		if a.snapshot != nil && a.snapshot.String("status") != "idle" && a.snapshot.String("status") != "failed" {
			return nil, errors.New("session is busy")
		}
		receipt, err := a.manager.store.Admit(ctx, a.sessionID, command)
		if err != nil {
			return nil, err
		}
		return backendstore.Reply("submissionSaved", backendstore.Fields(map[string]any{"receipt": receipt})), nil
	case "dispatchSubmission":
		id, err := command.ID("submissionId")
		if err != nil {
			return nil, err
		}
		record, claimed, err := a.manager.store.Transition(ctx, a.sessionID, id, model.SubmissionPreparing, model.SubmissionDispatching, &a.nativeID)
		if err != nil {
			return nil, err
		}
		if !claimed {
			return backendstore.Reply("submissionSaved", backendstore.Fields(map[string]any{"receipt": record.Receipt})), nil
		}
		files, fileErr := a.promptFiles(record.Files)
		if fileErr != nil {
			record, _, err = a.manager.store.Transition(ctx, a.sessionID, id, model.SubmissionDispatching, model.SubmissionNotSent, nil)
			if err != nil {
				return nil, err
			}
			return backendstore.Reply("submissionSaved", backendstore.Fields(map[string]any{"receipt": record.Receipt})), nil
		}
		payload, err := a.manager.store.Hydrate(ctx, a.sessionID)
		if err != nil {
			return nil, err
		}
		a.snapshot, err = backendstore.Decode(payload["session"])
		if err != nil {
			return nil, err
		}
		if err := a.loadSnapshot(); err != nil {
			return nil, err
		}
		a.snapshot.Set("native_session_id", a.nativeID)
		a.snapshot.Set("provider_cursor", map[string]string{"provider": "openCode", "sessionId": a.nativeID})
		a.turnID = id
		a.parts = map[string]int{}
		a.reasoning = map[string]int{}
		a.started = false
		a.tools = map[string]string{}
		if admission := command["scheduledAdmission"]; len(admission) > 0 {
			if err := a.emit("scheduledTurnAdmitted", admission); err != nil {
				return nil, err
			}
		}
		state, promptErr := a.process.Client.Prompt(ctx, a.nativeID, record.Receipt.InputID, record.Prompt, files)
		record, _, err = a.manager.store.Transition(ctx, a.sessionID, id, model.SubmissionDispatching, state, &a.nativeID)
		if err != nil {
			return nil, err
		}
		a.receipt(record.Receipt)
		if promptErr != nil && (state == model.SubmissionNotSent || state == model.SubmissionRejected) {
			if err := a.recordProviderError(promptErr.Error()); err != nil {
				return nil, err
			}
		}
		if state == model.SubmissionNotSent || state == model.SubmissionRejected {
			a.finish(false, "failed")
		}
		if err := a.flush(); err != nil {
			return nil, err
		}
		_ = a.emit("submissionUpdated", record.Receipt)
		return backendstore.Reply("submissionSaved", backendstore.Fields(map[string]any{"receipt": record.Receipt})), nil
	case "cancel":
		if a.snapshot != nil {
			if goal, err := backendstore.Decode(a.snapshot["goal"]); err == nil && goal.String("status") == "active" {
				_ = a.goalCommand(ctx, backendstore.Fields(map[string]any{"type": "pause"}))
			}
		}
		if err := a.process.Client.Interrupt(ctx, a.nativeID); err != nil {
			return nil, err
		}
		return backendstore.Reply("ack", nil), nil
	case "respond":
		request := command.String("requestId")
		scope := a.requestScopes[request]
		if scope == "" {
			return nil, errors.New("provider request expired or unknown")
		}
		if err := a.process.Client.Permission(ctx, scope, request, command.String("optionId")); err != nil {
			return nil, err
		}
		delete(a.requestScopes, request)
		return backendstore.Reply("ack", nil), nil
	case "compactSession":
		if err := a.process.Client.Compact(ctx, a.nativeID); err != nil {
			return nil, err
		}
		return backendstore.Reply("ack", nil), nil
	case "closeSession":
		a.cancel()
		return backendstore.Reply("ack", nil), nil
	case "abandonSubmission":
		id, err := command.ID("submissionId")
		if err != nil {
			return nil, err
		}
		record, _, err := a.manager.store.Transition(ctx, a.sessionID, id, model.SubmissionPreparing, model.SubmissionNotSent, nil)
		if err != nil {
			return nil, err
		}
		return backendstore.Reply("submissionSaved", backendstore.Fields(map[string]any{"receipt": record.Receipt})), nil
	default:
		return nil, errors.New("runtime command not yet supported")
	}
}

func (a *actor) receipt(receipt model.SubmissionReceipt) {
	for _, turn := range a.turns {
		if turn.String("id") == receipt.ID {
			turn.Set("submission", receipt)
		}
	}
	a.dirty = true
}

func (a *actor) begin() error {
	if a.snapshot == nil {
		return errors.New("unadmitted provider execution")
	}
	if a.started {
		return nil
	}
	a.started = true
	a.snapshot.Set("status", "working")
	for _, turn := range a.turns {
		if turn.String("id") == a.turnID {
			turn.Set("provider_turn_started", true)
		}
	}
	a.dirty = true
	return a.emit("turnStarted", nil)
}

func (a *actor) finish(success bool, status string) {
	if a.snapshot == nil {
		return
	}
	for _, turn := range a.turns {
		if turn.String("id") == a.turnID {
			turn.Set("status", status)
			turn.Set("completed_at", time.Now().Unix())
		}
	}
	for _, message := range a.messages {
		message.Set("streaming", false)
	}
	a.snapshot.Set("status", "idle")
	a.snapshot.Set("updated_at", time.Now().Unix())
	if success {
		a.snapshot.Set("last_reply_at", time.Now().Unix())
	}
	a.dirty = true
	if a.parent == nil {
		a.pendingChildren = nil
		for _, child := range a.children {
			child.revives = nil
		}
	}
}

func (a *actor) event(event backendengine.Event) error {
	payload, err := backendstore.Decode(event.Payload)
	if err != nil {
		return err
	}
	if handled, err := a.extendedEvent(event.Kind, payload); handled {
		return err
	}
	switch event.Kind {
	case "form.created", "question.asked":
		return a.formEvent(payload)
	case "permission.asked", "permission.requested", "permission.created":
		id := payload.String("id")
		if id == "" {
			return errors.New("permission request identity missing")
		}
		if err := a.rememberRequest(id); err != nil {
			return err
		}
		return a.emit("permission", map[string]any{"requestId": id, "title": payload.String("action"), "detail": "", "options": []any{map[string]any{"id": "once", "label": "Allow once", "allow": true}, map[string]any{"id": "reject", "label": "Deny", "allow": false}}})
	case "session.execution.started":
		return a.begin()
	case "session.text.started", "session.text.delta", "session.text.ended":
		if err := a.begin(); err != nil {
			return err
		}
		part := "text:" + payload.String("assistantMessageID") + ":" + string(payload["ordinal"])
		index, ok := a.parts[part]
		if !ok {
			index = len(a.messages)
			a.parts[part] = index
			a.messages = append(a.messages, backendstore.Fields(map[string]any{"id": uuid.NewSHA1(uuid.MustParse(a.sessionID), []byte(a.turnID+part)).String(), "turn_id": a.turnID, "role": "assistant", "content": "", "created_at": time.Now().Unix(), "streaming": true, "attachments": []any{}}))
		}
		message := a.messages[index]
		kind := "textStarted"
		wire := map[string]any{"part": part}
		if event.Kind == "session.text.delta" {
			kind = "textDelta"
			delta := payload.String("delta")
			message.Set("content", message.String("content")+delta)
			wire["delta"] = delta
		}
		if event.Kind == "session.text.ended" {
			kind = "textEnded"
			if payload["text"] != nil {
				message.Set("content", payload.String("text"))
			}
			message.Set("streaming", false)
			wire["text"] = message.String("content")
		}
		a.dirty = true
		return a.emit(kind, wire)
	case "session.execution.succeeded", "session.execution.failed", "session.execution.interrupted":
		if a.snapshot == nil || a.snapshot.String("status") != "working" {
			return nil
		}
		success := event.Kind == "session.execution.succeeded"
		status := "failed"
		if success {
			status = "completed"
		}
		if event.Kind == "session.execution.interrupted" {
			status = "interrupted"
		}
		if event.Kind == "session.execution.failed" {
			if err := a.recordProviderError(providerErrorText(payload)); err != nil {
				return err
			}
		}
		a.finish(success, status)
		if err := a.flush(); err != nil {
			return err
		}
		return a.emit("turnFinished", map[string]any{"success": success, "summary": nil})
	case "session.reasoning.started", "session.reasoning.delta", "session.reasoning.ended":
		if err := a.begin(); err != nil {
			return err
		}
		part := "reasoning:" + payload.String("assistantMessageID") + ":" + string(payload["ordinal"])
		index, ok := a.reasoning[part]
		if !ok {
			index = len(a.blocks)
			a.reasoning[part] = index
			a.blocks = append(a.blocks, backendstore.Fields(map[string]any{"turn_id": a.turnID, "after_message": len(a.messages), "content": map[string]any{"kind": "reasoning", "data": map[string]any{"content": "", "started_at_ms": time.Now().UnixMilli(), "finished_at_ms": 0}}}))
		}
		content, _ := backendstore.Decode(a.blocks[index]["content"])
		data, _ := backendstore.Decode(content["data"])
		kind := "reasoningStarted"
		wire := map[string]any{"part": part}
		if event.Kind == "session.reasoning.delta" {
			kind = "reasoningDelta"
			delta := payload.String("delta")
			data.Set("content", data.String("content")+delta)
			wire["delta"] = delta
		}
		if event.Kind == "session.reasoning.ended" {
			kind = "reasoningEnded"
			if payload["text"] != nil {
				data.Set("content", payload.String("text"))
			}
			data.Set("finished_at_ms", time.Now().UnixMilli())
			wire["text"] = data.String("content")
		}
		content.Set("data", data)
		a.blocks[index].Set("content", content)
		a.dirty = true
		return a.emit(kind, wire)
	default:
		if event.Kind == "session.error" {
			return a.recordProviderError(providerErrorText(payload))
		}
		return nil
	}
}

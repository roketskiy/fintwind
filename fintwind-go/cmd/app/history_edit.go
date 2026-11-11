package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
)

func messageEditSessionID(edit *messageEditState) string {
	if edit == nil {
		return ""
	}
	return edit.SessionID
}

func (a *app) confirmMessageEdit() {
	edit := a.editMessage
	if edit == nil || !edit.Confirm || a.detail == nil || a.detail.ID != edit.SessionID {
		return
	}
	if strings.TrimSpace(a.editDraft) == "" && len(edit.Attachments) == 0 {
		a.status = tr("The edited message is empty")
		return
	}
	if sync := a.optionsSync[edit.SessionID]; sync != nil && sync.sending {
		a.status = tr("Wait for session options to finish saving")
		return
	}
	operation := a.beginSessionOperation(edit.SessionID, "history-edit")
	if operation == nil {
		return
	}
	edit.Draft, edit.Confirm = a.editDraft, false
	operation.history, operation.phase, operation.message = edit, "draining", tr("Waiting for pending saves")
	a.startNextSnapshot()
}

func (a *app) startHistoryRewinds() {
	for id, operation := range a.sessionActions.operations {
		if operation.kind != "history-edit" || operation.phase != "draining" {
			continue
		}
		if _, queued := a.snapshots.pending[id]; queued {
			continue
		}
		waiting := false
		for _, admission := range a.snapshots.admissions {
			if admission.sessionID == id {
				waiting = true
			}
		}
		if waiting {
			continue
		}
		if !a.sessionOperationCurrent(id, operation) {
			operation.phase, operation.message = "unknown", tr("The task or connection changed. Reload history before continuing.")
			continue
		}
		operation.phase, operation.message = "rewinding", tr("Reverting history and file changes")
		go a.runMessageRewind(id, operation)
	}
}

func (a *app) runMessageRewind(id string, operation *sessionOperation) {
	ctx, cancel := context.WithTimeout(a.lifetime, 120*time.Second)
	defer cancel()
	payload, err := operation.client.RequestFor(ctx, id, "", map[string]any{"type": "rewindSessionToMessage", "turnCount": operation.history.TurnCount})
	var session *model.Session
	var raw map[string]any
	if err == nil {
		session, raw, err = decodeMessageRewind(payload, id, operation.history.TurnCount)
	}
	if err != nil {
		a.finishSessionOperationError(id, operation, "History rewind", err, true)
		return
	}
	a.post(func() {
		if !a.sessionOperationCurrent(id, operation) {
			operation.phase, operation.message = "unknown", tr("Rewind completed on the previous task or connection. Reload history before continuing.")
			return
		}
		a.installMessageRewind(operation, session, raw, true)
	})
}

func decodeMessageRewind(payload json.RawMessage, id string, turnCount int) (*model.Session, map[string]any, error) {
	var body struct {
		Type    string          `json:"type"`
		Session json.RawMessage `json:"session"`
	}
	if json.Unmarshal(payload, &body) != nil || body.Type != "sessionRewound" {
		return nil, nil, errors.New("Invalid rewind confirmation")
	}
	var session model.Session
	var raw map[string]any
	decoder := json.NewDecoder(bytes.NewReader(body.Session))
	decoder.UseNumber()
	if json.Unmarshal(body.Session, &session) != nil || decoder.Decode(&raw) != nil || raw == nil || session.ID != id || session.Busy() {
		return nil, nil, errors.New("Invalid rewound task")
	}
	if _, ok := raw["messages"]; !ok {
		return nil, nil, errors.New("Rewound task has no message data")
	}
	if _, ok := raw["turns"]; !ok {
		return nil, nil, errors.New("Rewound task has no turn data")
	}
	for _, turn := range session.Turns {
		if turn.TurnCount >= turnCount {
			return nil, nil, errors.New("Rewind did not remove the selected turn")
		}
	}
	return &session, raw, nil
}

func (a *app) historyRejectsEvent(event daemon.Event) bool {
	if operation := a.sessionActions.operations[event.SessionID]; operation != nil && operation.kind == "history-edit" && operation.phase != "draining" {
		return true
	}
	state := a.live[event.SessionID]
	return state != nil && state.retiredRuntimes[event.RuntimeID]
}

func (a *app) installMessageRewind(operation *sessionOperation, session *model.Session, raw map[string]any, send bool) {
	id, edit := session.ID, operation.history
	if session.ProjectID != operation.project.ID || sessionOperationNative(session) != sessionOperationNative(operation.session) {
		operation.phase, operation.message = "unknown", tr("The rewound task identity differs. Reload history before continuing.")
		return
	}
	retained := 0
	for _, turn := range operation.session.Turns {
		if turn.TurnCount < edit.TurnCount {
			if retained >= len(session.Turns) || session.Turns[retained].ID != turn.ID {
				operation.phase, operation.message = "unknown", tr("The retained history differs. Reload history before continuing.")
				return
			}
			retained++
		}
	}
	if retained != len(session.Turns) {
		operation.phase, operation.message = "unknown", tr("The retained history differs. Reload history before continuing.")
		return
	}
	state := a.stateForSession(operation.session)
	if state.retiredRuntimes == nil {
		state.retiredRuntimes = map[string]bool{}
	}
	for runtime := range state.cursors {
		state.retiredRuntimes[runtime] = true
	}
	if runtime := a.runtimes[id]; runtime != "" {
		state.retiredRuntimes[runtime] = true
	}
	delete(a.runtimes, id)
	delete(a.optionsSync, id)
	state.session, state.raw, state.edit = session, raw, nil
	state.permission, state.question, state.extension = nil, nil, nil
	state.textParts, state.reasoningParts = nil, nil
	state.preparing, state.admissionBusy, state.reconciliationFailed = false, false, false
	state.admission = nil
	delete(a.sessionActions.operations, id)
	if a.detail != nil && a.detail.ID == id {
		a.detail, a.detailRaw = session, raw
		a.editMessage, a.editDraft = nil, ""
		a.pendingPermission, a.pendingUserInput = nil, nil
		a.rebuildTranscript()
	}
	a.syncSummary(session)
	a.rebuildRows()
	if send && a.submitSessionSubmission(sessionSubmission{sessionID: id, text: edit.Draft, attachments: edit.Attachments}) {
		return
	}
	// Recovery never repeats the rewind or sends a possibly unconfirmed input.
	a.enqueueSessionSubmission(session, edit.Draft, edit.Attachments)
	if len(session.QueuedMessages) > 0 {
		delete(a.automaticQueueEntries(session), session.QueuedMessages[len(session.QueuedMessages)-1].ID)
	}
	a.status = tr("History restored. The edited message is in the queue for explicit sending.")
}

func (a *app) reconcileMessageEdit(operation *sessionOperation) {
	if operation == nil || operation.phase != "unknown" || a.client != operation.client || a.client.Identity() != operation.identity {
		return
	}
	operation.phase, operation.message = "reconciling", tr("Checking saved history")
	id := operation.history.SessionID
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 30*time.Second)
		defer cancel()
		session, raw, err := hydrateSessionOperation(ctx, operation.client, id)
		a.post(func() {
			if !a.sessionOperationCurrent(id, operation) {
				operation.phase = "unknown"
				return
			}
			if err != nil {
				operation.phase, operation.message = "unknown", tr("Could not confirm saved history. ")+err.Error()
				return
			}
			for _, turn := range session.Turns {
				if turn.TurnCount >= operation.history.TurnCount {
					operation.phase, operation.message = "unknown", tr("Rewind is still unconfirmed. No message was resent.")
					return
				}
			}
			if session.Busy() {
				operation.phase, operation.message = "unknown", tr("The task is active. No message was resent.")
				return
			}
			a.installMessageRewind(operation, session, raw, false)
		})
	}()
}

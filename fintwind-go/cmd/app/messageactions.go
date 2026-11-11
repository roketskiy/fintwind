package main

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/transfer"
	"github.com/egoist/mygo/ui"
)

// forkAction is one assistant footer's fork affordance, mirroring the
// source's AssistantMessageAction: which turn would fork, and whether the
// button is live or waiting for a confirmed result.
type forkAction struct {
	sessionID string
	messageID string
	turnCount int
	enabled   bool
	preparing bool
}

// forkActionForMessage resolves the fork affordance for one assistant
// message, or nil when no fork button should render. It mirrors
// assistant_message_action_for_message: only the message that owns its
// turn's settled footer offers a fork, only while the session is idle or
// failed, and only with a provider cursor to fork from.
func (a *app) forkActionForMessage(messageIndex int) *forkAction {
	session := a.detail
	if session == nil || messageIndex < 0 || messageIndex >= len(session.Messages) {
		return nil
	}
	message := &session.Messages[messageIndex]
	if message.Role != "assistant" || message.TurnID == nil {
		return nil
	}
	if _, _, ok := a.assistantFooter(messageIndex); !ok {
		return nil
	}
	turn := a.turnByID(*message.TurnID)
	if turn == nil {
		return nil
	}
	if session.Status != "idle" && session.Status != "failed" {
		return nil
	}
	if len(session.ProviderCursor) == 0 || string(session.ProviderCursor) == "null" {
		return nil
	}
	// forkingResponses is keyed by message id. Any pending fork in this
	// session parks every fork button; the owning message spins instead.
	pending := false
	for index := range session.Messages {
		if a.forkingResponses[session.Messages[index].ID] {
			pending = true
			break
		}
	}
	return &forkAction{
		sessionID: session.ID,
		messageID: message.ID,
		turnCount: turn.TurnCount,
		enabled:   !pending && a.sessionActions.operations[session.ID] == nil,
		preparing: a.forkingResponses[message.ID],
	}
}

// The confirmed response identifies the fork. Do not infer its id from an
// unrelated new row in a refreshed catalog or replace unsaved local drafts.
func (a *app) forkSessionFromResponse(sessionID, messageID string, turnCount int) {
	if a.client == nil || a.detail == nil || sessionID != a.detail.ID {
		return
	}
	valid := false
	for index := range a.detail.Messages {
		if a.detail.Messages[index].ID == messageID {
			action := a.forkActionForMessage(index)
			valid = action != nil && action.enabled && action.turnCount == turnCount
			break
		}
	}
	if !valid || a.sessionActions.operations[sessionID] != nil {
		return
	}
	for index := range a.detail.Messages {
		if a.forkingResponses[a.detail.Messages[index].ID] {
			a.status = tr("A fork is already running")
			return
		}
	}
	if a.forkingResponses == nil {
		a.forkingResponses = map[string]bool{}
	}
	operation := a.beginSessionOperation(sessionID, "fork")
	if operation == nil {
		return
	}
	a.forkingResponses[messageID] = true
	client := a.client
	identity := client.Identity()
	generation := a.generation
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 120*time.Second)
		defer cancel()
		payload, err := client.RequestFor(ctx, sessionID, "", map[string]any{
			"type":      "forkSessionFromResponse",
			"turnCount": turnCount,
		})
		var result struct {
			Type              string         `json:"type"`
			Session           *model.Session `json:"session"`
			CheckpointWarning *string        `json:"checkpointWarning"`
		}
		if err == nil {
			err = json.Unmarshal(payload, &result)
		}
		if err == nil && (result.Type != "sessionForked" || result.Session == nil || result.Session.ID == "" || result.Session.ID == sessionID) {
			err = fmt.Errorf("the daemon did not confirm the forked task")
		}
		a.post(func() {
			if !a.sessionOperationCurrent(sessionID, operation) || a.client != client || client.Identity() != identity {
				operation.phase, operation.message = "unknown", tr("Fork result belongs to a previous task or connection. Check tasks before forking again.")
				delete(a.forkingResponses, messageID)
				return
			}
			delete(a.forkingResponses, messageID)
			if err != nil {
				operation.phase = "unknown"
				operation.message = tr("Fork not confirmed. Check tasks before forking again: ") + err.Error()
				a.status = operation.message
				return
			}
			delete(a.sessionActions.operations, sessionID)
			forked := result.Session
			if a.summarySession(forked.ID) == nil {
				a.sessions = append(a.sessions, *forked)
				a.byID = nil
			}
			a.rebuildRows()
			a.refreshCatalog()
			a.status = tr("Task forked")
			if result.CheckpointWarning != nil && *result.CheckpointWarning != "" {
				a.status = tr("Task forked: ") + *result.CheckpointWarning
			}
			if a.selectedID == sessionID && a.generation == generation {
				a.openSession(forked.ID)
			}
		})
	}()
}

// messageCopyMenu attaches one message row's context menu: Copy message.
// Message text keeps its own selection menu (the innermost element with a
// menu gets the click), which already covers copying a selection. Quote
// staging is unavailable in this client — there are no composer reference
// chips to stage into — so the menu stays at the single always-available
// item and never shows an empty or disabled entry.
func (a *app) messageCopyMenu(target ui.Element, messageID, copyContent string) ui.Element {
	return target.ContextMenu(func(m *ui.Menu) {
		if m.Item("Copy message").Chosen() {
			if err := mygo.Clipboard.Write(transfer.TextData(copyContent)); err != nil {
				a.status = tr("Cannot copy message: ") + err.Error()
				return
			}
			a.showCopied(messageID)
		}
	})
}

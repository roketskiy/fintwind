package main

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
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
func (a *app) forkActionForMessage(session *model.Session, messageIndex int) *forkAction {
	if session == nil || messageIndex < 0 || messageIndex >= len(session.Messages) {
		return nil
	}
	message := &session.Messages[messageIndex]
	if message.Role != "assistant" || message.TurnID == nil {
		return nil
	}
	if _, _, ok := a.assistantFooter(session, messageIndex); !ok {
		return nil
	}
	turn := a.turnByID(session, *message.TurnID)
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
			action := a.forkActionForMessage(a.detail, index)
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

// messageCopyMenu attaches one message row's unified context menu, mirroring
// the source's message_menu_items order: Copy selection first while the row's
// shared selection covers text, then Copy message, then Quote. The markdown
// views leave their selection to the Selectable row (ShareSelection), so
// right-clicking text or row padding opens this same menu. Quote stages a
// text reference chip above the composer; only the entries that live
// elsewhere (rewind/fork) stay out.
func (a *app) messageCopyMenu(target ui.Element, messageID, role, copyContent string) ui.Element {
	return target.ContextMenu(func(m *ui.Menu) {
		selected := messageSelectedText(target)
		if selected != "" {
			if m.Item(tr("Copy Selection")).Chosen() {
				if err := mygo.Clipboard.Write(transfer.TextData(selected)); err != nil {
					a.status = tr("Cannot copy selection: ") + err.Error()
					return
				}
				a.showCopied(messageID)
			}
		}
		if m.Item(tr("Copy message")).Chosen() {
			if err := mygo.Clipboard.Write(transfer.TextData(copyContent)); err != nil {
				a.status = tr("Cannot copy message: ") + err.Error()
				return
			}
			a.showCopied(messageID)
		}
		// A quote keeps what was read — the selection when one covers part
		// of the message, the whole message otherwise — like the source's
		// quote_message item.
		quoteText := selected
		if quoteText == "" {
			quoteText = copyContent
		}
		if strings.TrimSpace(quoteText) != "" {
			if m.Item(tr("Quote to composer")).Chosen() {
				a.stageComposerReference(messageReference(quoteRoleLabel(role), quoteText))
			}
		}
	})
}

// quoteRoleLabel attributes a quoted message with its speaker, mirroring
// the source's message_role_label. The label travels into the prompt block,
// so it stays fixed like the source's.
func quoteRoleLabel(role string) string {
	if role == "assistant" {
		return "助手"
	}
	return "用户"
}

// messageSelectedText joins the row's shared selection in build order, the
// way the framework's own Copy does, or reports nothing when the row holds
// no selection.
func messageSelectedText(target ui.Element) string {
	parts := target.SelectedTexts()
	if len(parts) == 0 {
		return ""
	}
	selected := make([]string, 0, len(parts))
	for _, part := range parts {
		runes := []rune(part.Text)
		start, end := max(0, part.Start), min(len(runes), part.End)
		if start < end {
			selected = append(selected, string(runes[start:end]))
		}
	}
	return strings.Join(selected, "\n")
}

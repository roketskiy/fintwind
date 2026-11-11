package main

import (
	"context"
	"encoding/json"
	"time"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

type sessionReplyState struct {
	Phase   string
	Message string
}

type sessionOptionsSync struct {
	generation uint64
	sending    bool
	error      string
}

func (a *app) sendSessionReply(sessionID, runtimeID string, state *sessionReplyState, command map[string]any, accepted func()) {
	if a.client == nil || state.Phase != "" || a.lifetime == nil {
		return
	}
	if runtimeID == "" {
		state.Message = "The live session is unavailable. Reconnect before responding."
		return
	}
	client := a.client
	identity := client.Identity()
	state.Phase, state.Message = "sending", "Sending response..."
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 15*time.Second)
		defer cancel()
		payload, err := client.RequestFor(ctx, sessionID, runtimeID, command)
		var result struct {
			Type string `json:"type"`
		}
		confirmed := err == nil && json.Unmarshal(payload, &result) == nil && result.Type == "ack"
		a.post(func() {
			if a.client != client || client.Identity() != identity {
				state.Phase, state.Message = "unknown", "Response not confirmed. Check the session before responding again."
				return
			}
			if !confirmed {
				state.Phase, state.Message = "unknown", "Response not confirmed. Check the session before responding again."
				return
			}
			state.Phase, state.Message = "accepted", ""
			accepted()
		})
	}()
}

func (a *app) clearSessionPermission(sessionID string, pending *pendingPermission) {
	if live := a.live[sessionID]; live != nil && live.permission == pending {
		live.permission = nil
	}
	if a.detail != nil && a.detail.ID == sessionID && a.pendingPermission == pending {
		a.pendingPermission = nil
	}
	a.updateSessionAttention(sessionID)
}

func (a *app) clearSessionQuestion(sessionID string, pending *userInputState) {
	if live := a.live[sessionID]; live != nil && live.question == pending {
		live.question = nil
	}
	if a.detail != nil && a.detail.ID == sessionID && a.pendingUserInput == pending {
		a.pendingUserInput = nil
	}
	a.updateSessionAttention(sessionID)
}

func (a *app) updateSessionAttention(sessionID string) {
	live := a.live[sessionID]
	if live != nil && (live.permission != nil || live.question != nil) {
		return
	}
	if a.detail != nil && a.detail.ID == sessionID && (a.pendingPermission != nil || a.pendingUserInput != nil) {
		return
	}
	delete(a.sessionAttention, sessionID)
}

func sessionResponseNotice(c *ui.Context, response sessionReplyState) {
	if response.Message == "" {
		return
	}
	color := palette.textTertiary
	if response.Phase == "unknown" {
		color = palette.warningText
	}
	ui.Text(c, response.Message).FontSize(11.5).TextColor(color).Wrap().Margin(8, 0, 0, 0)
}

func (a *app) sendSessionOptions(session *model.Session, state *sessionOptionsSync) {
	if a.sessionActionBlocksInput(session.ID) {
		return
	}
	client, runtimeID, generation := a.client, a.runtimes[session.ID], state.generation
	if client == nil || runtimeID == "" {
		return
	}
	identity, sessionID := client.Identity(), session.ID
	state.sending, state.error = true, ""
	optional := func(value *string) any {
		if value == nil || *value == "" {
			return nil
		}
		return *value
	}
	options := map[string]any{
		"mode": session.RuntimeMode, "interactionMode": session.InteractionMode,
		"model": optional(session.Model), "reasoningEffort": optional(session.ReasoningEffort),
		"serviceTier": optional(session.ServiceTier), "contextWindow": optional(session.ContextWindow),
	}
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 15*time.Second)
		defer cancel()
		payload, err := client.RequestFor(ctx, sessionID, runtimeID, map[string]any{"type": "applyOptions", "options": options})
		var response struct {
			Type string `json:"type"`
		}
		confirmed := err == nil && json.Unmarshal(payload, &response) == nil && response.Type == "ack"
		a.post(func() {
			if a.optionsSync[sessionID] != state {
				return
			}
			state.sending = false
			if a.client != client || client.Identity() != identity || a.runtimes[sessionID] != runtimeID {
				state.error = "Session options were not confirmed on the current connection."
			} else if !confirmed {
				state.error = "Live session options were not confirmed. The selection is saved for the next session start."
			}
			if state.error != "" && a.selectedID == sessionID {
				a.status = state.error
			}
			if confirmed && state.generation != generation && a.client == client && a.runtimes[sessionID] == runtimeID {
				if current := a.eventSession(sessionID); current != nil {
					a.sendSessionOptions(current, state)
				}
			}
		})
	}()
}

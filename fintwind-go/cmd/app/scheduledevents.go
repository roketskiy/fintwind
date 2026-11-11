package main

import (
	"encoding/json"
	"time"

	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

func (a *app) applyScheduledTurnAdmission(session *model.Session, payload json.RawMessage) bool {
	var admission struct {
		Turn    model.AgentTurn `json:"turn"`
		Message model.Message   `json:"message"`
	}
	if json.Unmarshal(payload, &admission) != nil || admission.Turn.ID == "" ||
		admission.Message.TurnID == nil || *admission.Message.TurnID != admission.Turn.ID ||
		admission.Message.Role != "user" || admission.Turn.Status != "running" {
		return false
	}
	var receipt submissionReceipt
	if json.Unmarshal(admission.Turn.Submission, &receipt) != nil || receipt.ID != admission.Turn.ID || receipt.InputID == "" {
		return false
	}
	if session.TurnByID(admission.Turn.ID) != nil {
		return true
	}
	state := a.stateForSession(session)
	if state.preparing && len(session.Turns) > 0 {
		turn := session.Turns[len(session.Turns)-1]
		if turn.Status == "running" && !turn.ProviderTurnStarted {
			removed := make([]int, len(session.Messages)+1)
			messages := make([]model.Message, 0, len(session.Messages))
			for index, message := range session.Messages {
				removed[index+1] = removed[index]
				if message.TurnID != nil && *message.TurnID == turn.ID {
					removed[index+1]++
					if message.Role == "user" {
						session.QueuedMessages = append(session.QueuedMessages, model.QueuedMessage{
							ID: uuid.NewString(), CreatedAt: message.CreatedAt, Content: message.Content,
							DisplayContent: message.DisplayContent, Attachments: message.Attachments,
						})
					}
					continue
				}
				messages = append(messages, message)
			}
			blocks := make([]model.TranscriptBlock, 0, len(session.TranscriptBlocks))
			for _, block := range session.TranscriptBlocks {
				if block.TurnID != nil && *block.TurnID == turn.ID {
					continue
				}
				if block.AfterMessage >= 0 && block.AfterMessage < len(removed) {
					block.AfterMessage -= removed[block.AfterMessage]
				}
				blocks = append(blocks, block)
			}
			session.Messages, session.TranscriptBlocks = messages, blocks
			session.Turns = session.Turns[:len(session.Turns)-1]
		}
	}
	session.Messages = append(session.Messages, admission.Message)
	session.Turns = append(session.Turns, admission.Turn)
	session.Status, session.UpdatedAt = "connecting", uint64(time.Now().Unix())
	state.preparing, state.stopped = false, false
	state.textParts, state.reasoningParts = nil, nil
	state.continuationStart = nil
	return true
}

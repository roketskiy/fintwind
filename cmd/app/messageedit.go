package main

import (
	"encoding/json"
	"strings"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

// messageEditState is the in-transcript editor for one past user message. It
// replaces the bubble until the user sends or cancels, like the source's
// MessageEdit.
type messageEditState struct {
	SessionID   string
	MessageID   string
	TurnID      string
	TurnCount   int
	Draft       string
	Attachments []draftAttachment
	Confirm     bool
}

// editRewindState reports whether one message can open its turn's editor and
// why not when it cannot.
func (a *app) editRewindState(session *model.Session, messageIndex int) (int, bool, string) {
	if session == nil || messageIndex < 0 || messageIndex >= len(session.Messages) {
		return 0, false, "That message is no longer available"
	}
	message := &session.Messages[messageIndex]
	if message.Role != "user" || message.TurnID == nil {
		return 0, false, ""
	}
	turn := a.turnByID(session, *message.TurnID)
	if turn == nil {
		return 0, false, "That message is not editable right now"
	}
	opening := -1
	for index := range session.Messages {
		candidate := &session.Messages[index]
		if candidate.Role == "user" && candidate.TurnID != nil && *candidate.TurnID == *message.TurnID {
			opening = index
			break
		}
	}
	if opening != messageIndex {
		return turn.TurnCount, false, "Only the message that opened this turn can revert it"
	}
	if session.Status == "connecting" || session.Status == "working" || session.Status == "waiting" {
		return turn.TurnCount, false, "Stop the task before reverting"
	}
	if nativeSessionID(session.ProviderCursor) == "" {
		return turn.TurnCount, false, "This session lost its provider link, so the rewind cannot be driven"
	}
	return turn.TurnCount, true, "Revert to here"
}

// beginMessageEdit swaps the bubble for the editor with the original text.
func (a *app) beginMessageEdit(messageIndex int) {
	if a.detail != nil && a.sessionActionBlocksInput(a.detail.ID) {
		return
	}
	turnCount, ok, reason := a.editRewindState(a.detail, messageIndex)
	if !ok {
		a.status = reason
		return
	}
	message := &a.detail.Messages[messageIndex]
	a.editMessage = &messageEditState{
		SessionID:   a.detail.ID,
		MessageID:   message.ID,
		TurnID:      *message.TurnID,
		TurnCount:   turnCount,
		Draft:       message.Text(),
		Attachments: draftAttachmentsFromModel(message.Attachments),
	}
	a.stateForSession(a.detail).edit = a.editMessage
	a.editDraft = message.Text()
	a.rebuildTranscript()
}

func (a *app) cancelMessageEdit() {
	if a.editMessage == nil {
		return
	}
	if a.sessionActionBlocksInput(a.editMessage.SessionID) {
		return
	}
	if state := a.live[a.editMessage.SessionID]; state != nil {
		state.edit = nil
	}
	a.editMessage = nil
	a.editDraft = ""
	a.rebuildTranscript()
}

func (a *app) submitMessageEdit() {
	edit := a.editMessage
	if edit == nil || a.detail == nil || a.client == nil || edit.SessionID != a.detail.ID {
		return
	}
	text := strings.TrimSpace(a.editDraft)
	session := a.detail
	index := -1
	for i := range session.Messages {
		if session.Messages[i].ID == edit.MessageID {
			index = i
			break
		}
	}
	if index < 0 {
		a.editMessage = nil
		a.editDraft = ""
		a.rebuildTranscript()
		return
	}
	if text == "" && len(session.Messages[index].Attachments) == 0 {
		a.status = tr("The edited message is empty")
		return
	}
	if a.sessionActionBlocksInput(edit.SessionID) {
		return
	}
	edit.Draft, edit.Confirm = a.editDraft, true
}

// nativeSessionID extracts the provider's session id from a resume cursor.
func nativeSessionID(cursor json.RawMessage) string {
	if len(cursor) == 0 {
		return ""
	}
	var value struct {
		Provider  string `json:"provider"`
		SessionID string `json:"sessionId"`
	}
	if json.Unmarshal(cursor, &value) != nil {
		return ""
	}
	return value.SessionID
}

// messageEditCard is the editor that replaces a user bubble while its message
// is being rewritten: 540 wide, raised, 12 radius, Cancel and Send below.
func (a *app) messageEditCard(c *ui.Context, message *model.Message) {
	card := ui.Column(c).FillWidth().MaxWidth(540).MinWidth(0).Shrink(0).
		Radius(12).Background(palette.raised).Padding(9, 0, 8, 0)
	card.Children(func() {
		operation := a.sessionActions.operations[messageEditSessionID(a.editMessage)]
		blocked := operation != nil && operation.kind == "history-edit"
		ui.Row(c).FillWidth().Padding(0, 12).Children(func() {
			ui.TextAreaBase(c, &a.editDraft).FillWidth().Lines(1, 10).
				MinHeight(46).FixedLineHeight(uiPx(22)).AutoFocus().
				Label(tr("Edit message")).FontSize(uiPx(14)).TextColor(palette.text).Disabled(blocked).
				OnTextKey(func(k ui.TextKey) bool {
					if k.Key == ui.KeyEscape {
						a.cancelMessageEdit()
						return true
					}
					if k.Key != ui.KeyEnter || k.Mods != 0 {
						return false
					}
					a.submitMessageEdit()
					return true
				})
		})
		if blocked {
			ui.Text(c, operation.message).FontSize(uiPx(12)).TextColor(palette.textSecondary).Wrap().Padding(8, 12)
			if operation.phase == "unknown" {
				componentTextButton(c, "history-reconcile", tr("Reload history"), false).OnClick(func() { a.reconcileMessageEdit(operation) })
			} else if operation.phase == "draining" {
				componentTextButton(c, "history-save-retry", tr("Retry pending saves"), false).OnClick(a.startNextSnapshot)
			}
			return
		}
		if a.editMessage != nil && a.editMessage.Confirm {
			ui.Text(c, tr("Revert this turn and all later turns, including their file changes, then send the edited message?")).FontSize(uiPx(12)).TextColor(palette.warningText).Wrap().Padding(8, 12)
			ui.Row(c).Gap(6).Padding(0, 12).Children(func() {
				componentTextButton(c, "history-confirm-cancel", tr("Keep editing"), false).OnClick(func() { a.editMessage.Confirm = false })
				componentTextButton(c, "history-confirm-send", tr("Revert and send"), true).OnClick(a.confirmMessageEdit)
			})
			return
		}
		canSubmit := strings.TrimSpace(a.editDraft) != "" || len(message.Attachments) > 0
		ui.Row(c).FillWidth().Margin(7, 0, 0, 0).Padding(0, 12).Justify(ui.End).
			Gap(6).Children(func() {
			cancel := componentTextButton(c, "edit-cancel", tr("Cancel"), false)
			if cancel.Clicked() {
				a.cancelMessageEdit()
			}
			send := componentTextButton(c, "edit-send", tr("Send"), true).Disabled(!canSubmit)
			if send.Clicked() && canSubmit {
				a.submitMessageEdit()
			}
		})
	})
}

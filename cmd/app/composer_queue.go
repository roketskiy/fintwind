package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

// composer_queue.go owns the queued follow-up rows: queue, remove, edit and
// steer operate on the session named by id, never on the selected one. Steer
// send outcomes are tracked in a pending-steer registry under the session
// extension store, consumed by applySteerAccepted/Rejected.

// pendingSteersKey is the extension-store slot holding []pendingSteer.
// Runtime-only, like the desktop's pending_steers: never persisted.
const pendingSteersKey = "pendingSteers"

func (a *app) automaticQueueEntries(session *model.Session) map[string]bool {
	extension := a.extension(session)
	entries, _ := extension["automaticQueueEntries"].(map[string]bool)
	if entries == nil {
		entries = make(map[string]bool)
		extension["automaticQueueEntries"] = entries
	}
	return entries
}

// pendingSteer is one steer awaiting provider acknowledgement, with full
// presentation so accept and reject paths keep attachments and display text.
type pendingSteer struct {
	messageID   string
	createdAt   uint64
	prompt      string
	display     *string
	attachments []draftAttachment
}

// queuedSteerable gates the Steer control: it only shows while the session
// is busy and a runtime exists to receive the steer. The Rust source also
// requires driver.supports_steer, which this client cannot query, so runtime
// presence is the gate here.
func (a *app) queuedSteerable(session *model.Session) bool {
	if session == nil || a.client == nil {
		return false
	}
	if !session.Busy() {
		return false
	}
	return a.runtimes[session.ID] != ""
}

// queuedPrompt is the text a queued message carries back into the turn.
// Older rows appended @ paths to the prompt and kept the typed text in the
// display content; with attachments present the display text is the prompt
// (mirrors ComposerSubmission::provider_prompt).
func queuedPrompt(message model.QueuedMessage) string {
	if len(message.Attachments) > 0 {
		if message.DisplayContent != nil && strings.TrimSpace(*message.DisplayContent) != "" {
			return *message.DisplayContent
		}
	}
	if strings.TrimSpace(message.Content) != "" {
		return message.Content
	}
	if message.DisplayContent != nil {
		return *message.DisplayContent
	}
	return ""
}

// findQueuedMessage locates one queue entry by id, or -1. Callers resolve by
// id (not render-time index) so a turn that consumed a row between render
// and click cannot misplace another one.
func findQueuedMessage(session *model.Session, messageID string) int {
	if session == nil {
		return -1
	}
	for index := range session.QueuedMessages {
		if session.QueuedMessages[index].ID == messageID {
			return index
		}
	}
	return -1
}

type queuedMessageDrag struct {
	sessionID string
	messageID string
}

func (a *app) reorderQueuedMessage(sessionID, messageID, targetID string) {
	if a.sessionActionBlocksInput(sessionID) {
		return
	}
	session := a.eventSession(sessionID)
	from, to := findQueuedMessage(session, messageID), findQueuedMessage(session, targetID)
	if from < 0 || to < 0 || from == to {
		return
	}
	message := session.QueuedMessages[from]
	if from < to {
		copy(session.QueuedMessages[from:to], session.QueuedMessages[from+1:to+1])
	} else {
		copy(session.QueuedMessages[to+1:from+1], session.QueuedMessages[to:from])
	}
	session.QueuedMessages[to] = message
	session.UpdatedAt = uint64(time.Now().Unix())
	a.saveSessionSnapshot(session)
}

func (a *app) moveQueuedMessage(sessionID, messageID string, direction int) {
	session := a.eventSession(sessionID)
	index := findQueuedMessage(session, messageID)
	if index < 0 || index+direction < 0 || index+direction >= len(session.QueuedMessages) {
		return
	}
	a.reorderQueuedMessage(sessionID, messageID, session.QueuedMessages[index+direction].ID)
}

// removeQueuedMessage drops one follow-up and persists the queue, mirroring
// runtime.rs remove_queued_message (which saves).
func (a *app) removeQueuedMessage(sessionID, messageID string) {
	session := a.eventSession(sessionID)
	if session == nil {
		return
	}
	index := findQueuedMessage(session, messageID)
	if index < 0 {
		return
	}
	session.QueuedMessages = append(session.QueuedMessages[:index], session.QueuedMessages[index+1:]...)
	delete(a.automaticQueueEntries(session), messageID)
	session.UpdatedAt = uint64(time.Now().Unix())
	a.saveSessionSnapshot(session)
	a.syncSummary(session)
	a.rebuildRows()
}

// editQueuedMessage pops a queued follow-up back into its own session's
// composer so the user can edit and resubmit it, mirroring runtime.rs
// edit_queued_message. Attachments travel back with their blob references;
// background sessions keep their own draft, the current input is untouched
// unless the queue belongs to it.
func (a *app) editQueuedMessage(sessionID, messageID string) {
	if a.sessionActionBlocksInput(sessionID) {
		return
	}
	session := a.eventSession(sessionID)
	if session == nil {
		return
	}
	index := findQueuedMessage(session, messageID)
	if index < 0 {
		return
	}
	message := session.QueuedMessages[index]
	state := a.stateForSession(session)
	if a.detail == session {
		if strings.TrimSpace(a.draft) != "" || len(a.pendingAttachments) > 0 || len(a.pendingReferences) > 0 {
			a.status = tr("Clear the composer before editing a queued message")
			return
		}
	} else if strings.TrimSpace(state.draft) != "" || len(state.attachments) > 0 || len(state.references) > 0 {
		return
	}
	session.QueuedMessages = append(session.QueuedMessages[:index], session.QueuedMessages[index+1:]...)
	delete(a.automaticQueueEntries(session), messageID)
	session.UpdatedAt = uint64(time.Now().Unix())
	state.draft = queuedPrompt(message)
	state.attachments = draftAttachmentsFromModel(message.Attachments)
	if a.detail == session {
		a.draft = state.draft
		a.pendingAttachments = state.attachments
	}
	a.saveSessionSnapshot(session)
	a.syncSummary(session)
	a.rebuildRows()
}

// The durable row remains until an exact provider echo confirms delivery.
func (a *app) steerQueuedMessage(sessionID, messageID string) {
	if a.sessionActionBlocksInput(sessionID) {
		return
	}
	session := a.eventSession(sessionID)
	if session == nil || a.client == nil {
		return
	}
	index := findQueuedMessage(session, messageID)
	if index < 0 {
		return
	}
	runtimeID := a.runtimes[sessionID]
	if runtimeID == "" {
		if a.detail == session {
			a.status = tr("Cannot steer while the agent runtime is unavailable")
		}
		return
	}
	message := session.QueuedMessages[index]
	for _, pending := range a.pendingSteerList(session) {
		if pending.messageID == messageID {
			return
		}
	}
	entry := pendingSteer{
		messageID:   message.ID,
		createdAt:   message.CreatedAt,
		prompt:      queuedPrompt(message),
		display:     message.DisplayContent,
		attachments: draftAttachmentsFromModel(message.Attachments),
	}
	delete(a.automaticQueueEntries(session), message.ID)
	a.pushPendingSteer(session, entry)
	prompt := entry.prompt
	files := promptFilesForAttachments(entry.attachments)
	client := a.client
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 30*time.Second)
		defer cancel()
		_, err := client.RequestFor(ctx, sessionID, runtimeID, map[string]any{
			"type":   "steer",
			"prompt": prompt,
			"files":  files,
		})
		if err == nil {
			return
		}
		text := err.Error()
		a.post(func() {
			if state := a.live[sessionID]; state == nil || state.session != session {
				return
			}
			pending, exists := a.peekPendingSteer(session, prompt)
			if !exists || pending.messageID != messageID {
				return
			}
			session.Messages = append(session.Messages, model.Message{
				ID: uuid.NewString(), Role: "system", Content: "Steer delivery is unconfirmed. Check the session before resending.", CreatedAt: uint64(time.Now().Unix()),
			})
			a.tabUnread[sessionID] = a.detail != session
			a.saveSessionSnapshot(session)
			if a.detail == session {
				a.rebuildTranscript()
			}
			if a.detail == session {
				a.status = tr("Steer delivery is unconfirmed. Check the session before resending: ") + text
			}
		})
	}()
}

// pendingSteerList reads the session's unacknowledged steers in send order.
func (a *app) pendingSteerList(session *model.Session) []pendingSteer {
	if session == nil {
		return nil
	}
	entries, _ := a.extension(session)[pendingSteersKey].([]pendingSteer)
	return entries
}

// pushPendingSteer records a steer send awaiting acknowledgement.
func (a *app) pushPendingSteer(session *model.Session, entry pendingSteer) {
	entries := append(append([]pendingSteer(nil), a.pendingSteerList(session)...), entry)
	a.extension(session)[pendingSteersKey] = entries
}

// Unmatched echoes cannot claim another input's attachment references.
func (a *app) peekPendingSteer(session *model.Session, providerMessage string) (pendingSteer, bool) {
	entries := a.pendingSteerList(session)
	for _, entry := range entries {
		if entry.prompt == providerMessage {
			return entry, true
		}
	}
	return pendingSteer{}, false
}

// takePendingSteer removes and returns the acknowledged entry. Duplicate
// prompts resolve FIFO by send order.
func (a *app) takePendingSteer(session *model.Session, providerMessage string) (pendingSteer, bool) {
	entries := a.pendingSteerList(session)
	index := -1
	for i := range entries {
		if entries[i].prompt == providerMessage {
			index = i
			break
		}
	}
	if index < 0 {
		return pendingSteer{}, false
	}
	taken := entries[index]
	rest := append([]pendingSteer(nil), entries[:index]...)
	rest = append(rest, entries[index+1:]...)
	a.extension(session)[pendingSteersKey] = rest
	return taken, true
}

// requeuePendingSteer puts an unacknowledged steer back into its session's
// queue with full fields, at the head for a settled-cleanly turn or at the
// tail otherwise. Exactly one requeue per taken entry; the caller decides.
func (a *app) requeuePendingSteer(sessionID string, entry pendingSteer, atHead bool) {
	session := a.eventSession(sessionID)
	if session == nil {
		return
	}
	message := pendingSteerToQueuedMessage(entry)
	a.automaticQueueEntries(session)[message.ID] = true
	if index := findQueuedMessage(session, entry.messageID); index >= 0 {
		session.QueuedMessages = append(session.QueuedMessages[:index], session.QueuedMessages[index+1:]...)
	}
	if atHead {
		session.QueuedMessages = append([]model.QueuedMessage{message}, session.QueuedMessages...)
	} else {
		session.QueuedMessages = append(session.QueuedMessages, message)
	}
	session.UpdatedAt = uint64(time.Now().Unix())
	a.saveSessionSnapshot(session)
	a.syncSummary(session)
	a.rebuildRows()
}

// pendingSteerToQueuedMessage rebuilds a queue row from a pending steer,
// keeping display content and every attachment field.
func pendingSteerToQueuedMessage(entry pendingSteer) model.QueuedMessage {
	return model.QueuedMessage{
		ID:             entry.messageID,
		Content:        entry.prompt,
		DisplayContent: entry.display,
		Attachments:    messageAttachmentsFromDraft(entry.attachments),
		CreatedAt:      entry.createdAt,
	}
}

// draftAttachmentsFromModel restores staged files from a queued row without
// dropping blob references, so an edited follow-up resends identically.
func draftAttachmentsFromModel(attachments []model.Attachment) []draftAttachment {
	out := make([]draftAttachment, 0, len(attachments))
	for _, attachment := range attachments {
		entry := draftAttachment{
			Path:    attachment.Path,
			Mention: attachment.Mention,
			Name:    attachment.Name,
			IsDir:   attachment.IsDir,
			IsImage: attachment.IsImage,
		}
		if attachment.BlobReference != nil {
			entry.Reference = *attachment.BlobReference
		}
		out = append(out, entry)
	}
	return out
}

// queuedCollapsed reports whether one session's queue list is folded. State
// is keyed by session id so it survives re-renders, queue updates and
// session switches, and is also kept on disk so it survives restarts. The
// in-memory map stays the source of truth; every read or write tolerates a
// missing or corrupt file.
func (a *app) queuedCollapsed(sessionID string) bool {
	return a.collapsedQueued[sessionID]
}

// setQueuedCollapsed folds or unfolds one session's queue list and persists
// it. Only folded sessions are stored; unfolding deletes the entry.
func (a *app) setQueuedCollapsed(sessionID string, collapsed bool) {
	if a.collapsedQueued == nil {
		a.collapsedQueued = a.loadQueuedCollapsed()
	}
	if collapsed {
		a.collapsedQueued[sessionID] = true
	} else {
		delete(a.collapsedQueued, sessionID)
	}
	a.saveQueuedCollapsed()
}

// queuedCollapsedPath is the on-disk home of the folded-session map. An
// empty path disables file persistence silently.
func queuedCollapsedPath() string {
	if root := os.Getenv("LOCALAPPDATA"); root != "" {
		return filepath.Join(root, "Fintwind Go", "ui-state", "queued-collapsed.json")
	}
	if dir, err := os.UserCacheDir(); err == nil && dir != "" {
		return filepath.Join(dir, "Fintwind Go", "ui-state", "queued-collapsed.json")
	}
	return ""
}

func (a *app) loadQueuedCollapsed() map[string]bool {
	collapsed := map[string]bool{}
	path := queuedCollapsedPath()
	if path == "" {
		return collapsed
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return collapsed
	}
	var stored map[string]bool
	if json.Unmarshal(data, &stored) != nil {
		return collapsed
	}
	for sessionID, folded := range stored {
		if folded && sessionID != "" {
			collapsed[sessionID] = true
		}
	}
	return collapsed
}

func (a *app) saveQueuedCollapsed() {
	path := queuedCollapsedPath()
	if path == "" {
		return
	}
	data, err := json.Marshal(a.collapsedQueued)
	if err != nil {
		return
	}
	_ = os.MkdirAll(filepath.Dir(path), 0o755)
	_ = os.WriteFile(path, data, 0o600)
}

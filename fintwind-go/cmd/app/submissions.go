package main

import (
	"encoding/json"
	"strings"
	"time"

	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

// submissions.go is the only entry point for session submission: one call
// decides queue versus new turn for an explicit session, one drain promotes
// the queue head after a clean turn. Neither reads selectedID nor
// selectedProject; ownership always comes from the passed session id.

// sessionSubmission is plain app data: which session, what text, which
// already staged files. Attachments are copied on entry, so clearing the
// caller's composer cannot affect the stored message.
type sessionSubmission struct {
	sessionID   string
	text        string
	attachments []draftAttachment
}

// submitSessionSubmission queues on a busy session and starts a running
// turn on an idle one. It never touches a.draft or a.pendingAttachments;
// the caller clears those only when accepted targets the current session.
// True means accepted: the text is queued or a turn worker owns it.
func (a *app) submitSessionSubmission(submission sessionSubmission) bool {
	text := strings.TrimSpace(submission.text)
	attachments := append([]draftAttachment(nil), submission.attachments...)
	if text == "" && len(attachments) == 0 {
		return false
	}
	if a.client == nil {
		return false
	}
	if text == "/compact" && len(attachments) == 0 && a.detail != nil && a.detail.ID == submission.sessionID {
		if !a.canCompactContext() {
			a.status = tr("Context compaction is unavailable")
			return false
		}
		a.requestContextCompaction()
		return true
	}
	session := a.resolveSubmissionSession(submission.sessionID)
	if session == nil {
		return false
	}
	if a.sessionActionBlocksInput(session.ID) {
		a.status = tr("Resolve the pending task operation before sending")
		return false
	}
	if len(a.stateForSession(session).uploads) > 0 {
		if a.detail == session {
			a.status = tr("Wait for the attachments to finish importing")
		}
		return false
	}
	if session.Busy() {
		if !a.enqueueSessionSubmission(session, text, attachments) {
			return false
		}
		if a.runtimePreferences().BusySendSteer && session.Status == "working" && a.runtimes[session.ID] != "" {
			message := session.QueuedMessages[len(session.QueuedMessages)-1]
			a.steerQueuedMessage(session.ID, message.ID)
		}
		return true
	}
	if a.stateForSession(session).admissionBusy {
		return false
	}
	if hasUnsettledSubmissionReceipts(session) {
		a.status = tr("A previous submission is unconfirmed. Check the session before sending again.")
		return false
	}
	// An unacknowledged steer owns the running turn's input. A fresh prompt
	// must not overtake it, and the pending row must never silently become
	// a new prompt: only the steer echo may consume it.
	if len(a.pendingSteerList(session)) > 0 {
		if a.detail == session {
			a.status = tr("A steer is unconfirmed. Check the session before sending again.")
		}
		return false
	}
	return a.startIdleTurn(session, text, attachments)
}

// drainQueuedSubmission promotes one queue head into a fresh turn. The
// caller runs it after turnFinished success and Ack, off the event path.
// Stopped, preparing, busy, empty or receipt-unsettled sessions refuse.
func (a *app) drainQueuedSubmission(session *model.Session) bool {
	if session == nil || a.client == nil {
		return false
	}
	if a.sessionActionBlocksInput(session.ID) {
		return false
	}
	live := a.eventSession(session.ID)
	if live == nil {
		return false
	}
	state := a.stateForSession(live)
	if state.stopped || state.preparing || state.admissionBusy || live.Busy() || len(live.QueuedMessages) == 0 {
		return false
	}
	if !lastTurnSettledCleanly(live) {
		return false
	}
	if hasUnsettledSubmissionReceipts(live) {
		return false
	}
	// An unacknowledged steer blocks the drain: the pending row must never
	// become a prompt behind the user's back.
	if len(a.pendingSteerList(live)) > 0 {
		return false
	}
	head := live.QueuedMessages[0]
	// Only rows this process queued, or rows restored after a real
	// rejection, are eligible for automatic send. History restored after a
	// restart needs an explicit user Edit or Steer and never drains here.
	if !a.automaticQueueEntries(live)[head.ID] {
		return false
	}
	live.QueuedMessages = append([]model.QueuedMessage(nil), live.QueuedMessages[1:]...)
	delete(a.automaticQueueEntries(live), head.ID)
	if !a.startIdleTurn(live, queuedPrompt(head), draftAttachmentsFromModel(head.Attachments)) {
		live.QueuedMessages = append([]model.QueuedMessage{head}, live.QueuedMessages...)
		a.automaticQueueEntries(live)[head.ID] = true
		a.saveSessionSnapshot(live)
		return false
	}
	return true
}

// composerTargetSessionID names the session the composer card shows. Empty
// means no session yet; the submit path creates the draft then.
func (a *app) composerTargetSessionID() string {
	if a.detail != nil {
		return a.detail.ID
	}
	return ""
}

// resolveSubmissionSession maps an explicit id to its session, creating the
// draft for a first message. It never falls back to the selected session.
func (a *app) resolveSubmissionSession(sessionID string) *model.Session {
	if sessionID != "" {
		return a.eventSession(sessionID)
	}
	if a.detail == nil && !a.createDraftSession() {
		return nil
	}
	return a.detail
}

// enqueueSessionSubmission appends one follow-up with creation time and full
// attachments. It never starts a provider turn. The row is marked eligible
// for automatic drain: only same-process queues and real-rejection restores
// may send without an explicit user action.
func (a *app) enqueueSessionSubmission(session *model.Session, text string, attachments []draftAttachment) bool {
	now := uint64(time.Now().Unix())
	row := model.QueuedMessage{
		ID:          uuid.NewString(),
		Content:     text,
		Attachments: messageAttachmentsFromDraft(attachments),
		CreatedAt:   now,
	}
	session.QueuedMessages = append(session.QueuedMessages, row)
	a.automaticQueueEntries(session)[row.ID] = true
	session.UpdatedAt = now
	a.saveSessionSnapshot(session)
	a.syncSummary(session)
	a.rebuildRows()
	return true
}

// startIdleTurn builds the running turn with its user message and frozen
// admission record, then hands the send to runSubmissionWorker. A fresh
// accept clears stopped. The accept RPC carries the full session snapshot
// ahead of autosaves, so no Prompt is ever sent on a stale snapshot: the
// daemon admits by turn id and input id, and executes at most once per
// DispatchSubmission. The worker rechecks stopped and the active turn on
// the UI thread before dispatching.
func (a *app) startIdleTurn(session *model.Session, text string, attachments []draftAttachment) bool {
	if session == nil || a.client == nil {
		return false
	}
	if a.sessionActionBlocksInput(session.ID) {
		return false
	}
	state := a.stateForSession(session)
	now := uint64(time.Now().Unix())
	state.stopped = false
	state.preparing = true
	prompt := text
	if project := a.projectForID(session.ProjectID); project != nil && project.Path == a.autocompleteDir {
		prompt = expandSlashSubmission(text, a.slashIndex)
	}
	record := model.NewSubmissionRecord(session.ID, uuid.NewString(), prompt, submissionFilesForAttachments(attachments))
	turnID := record.Receipt.ID
	receiptRaw, err := json.Marshal(record.Receipt)
	if err != nil {
		state.preparing = false
		if a.detail == session {
			a.status = tr("Could not admit the send: ") + err.Error()
		}
		return false
	}
	turn := model.AgentTurn{
		ID:         turnID,
		TurnCount:  len(session.Turns) + 1,
		Status:     "running",
		StartedAt:  now,
		Submission: receiptRaw,
	}
	session.Turns = append(session.Turns, turn)
	session.Status = "working"
	files := messageAttachmentsFromDraft(attachments)
	session.Messages = append(session.Messages, model.Message{
		ID:          uuid.NewString(),
		TurnID:      &turnID,
		Role:        "user",
		Content:     text,
		Attachments: files,
		CreatedAt:   now,
	})
	lastReply := turn.StartedAt
	session.LastReplyAt = &lastReply
	session.UpdatedAt = now
	if a.detail == session {
		a.expandedTurns = map[string]bool{}
		a.rebuildTranscript()
		a.messageList.FollowEnd = true
	}
	a.syncSummary(session)
	a.rebuildRows()
	var cwd string
	if project := a.projectForID(session.ProjectID); project != nil {
		cwd = project.Path
	}
	launch := newRuntimeLaunch(session, cwd)
	launch.mermaidVersion = a.sessionMermaidVersion(session)
	state.admissionBusy = true
	admission := a.acceptSessionSubmission(session, record)
	state.admission = admission
	go a.runSubmissionWorker(a.client, session.ID, turnID, record, launch, admission)
	return true
}

// hasUnsettledSubmissionReceipts ports the blocking gate of
// has_unsettled_submissions: a running turn or a Preparing/unconfirmed
// receipt blocks any further POST for the session.
func hasUnsettledSubmissionReceipts(session *model.Session) bool {
	for index := range session.Turns {
		turn := &session.Turns[index]
		if turn.Status == "running" {
			return true
		}
		if len(turn.Submission) == 0 {
			continue
		}
		var receipt struct {
			State string `json:"state"`
		}
		if json.Unmarshal(turn.Submission, &receipt) != nil {
			continue
		}
		switch receipt.State {
		case "preparing", "dispatching", "unknown":
			return true
		}
	}
	return false
}

// lastTurnSettledCleanly reports whether the newest turn completed, the
// only case where a rejected steer may head the queue for a drain.
func lastTurnSettledCleanly(session *model.Session) bool {
	if session == nil || len(session.Turns) == 0 {
		return false
	}
	return session.Turns[len(session.Turns)-1].Status == "completed"
}

// messageAttachmentsFromDraft keeps every daemon-side field, so queued and
// sent rows render the same tiles the transcript expects.
func messageAttachmentsFromDraft(attachments []draftAttachment) []model.Attachment {
	out := make([]model.Attachment, 0, len(attachments))
	for _, attachment := range attachments {
		entry := model.Attachment{
			Path:    attachment.Path,
			IsDir:   attachment.IsDir,
			Name:    attachment.Name,
			Mention: attachment.Mention,
			IsImage: attachment.IsImage,
		}
		if attachment.Reference != "" {
			reference := attachment.Reference
			entry.BlobReference = &reference
		}
		out = append(out, entry)
	}
	return out
}

// submissionFilesForAttachments maps staged files to SubmissionRecord files:
// {path, name, blob_reference} with the daemon-side path in every slot.
func submissionFilesForAttachments(attachments []draftAttachment) []model.SubmissionFile {
	files := make([]model.SubmissionFile, 0, len(attachments))
	for _, attachment := range attachments {
		entry := model.SubmissionFile{
			Path: attachment.Path,
			Name: attachment.Name,
		}
		if attachment.Reference != "" {
			reference := attachment.Reference
			entry.BlobReference = &reference
		}
		files = append(files, entry)
	}
	return files
}

// promptFilesForAttachments maps staged files to prompt `files` entries:
// {path, name, blob_reference} with the daemon-side path in every slot.
func promptFilesForAttachments(attachments []draftAttachment) []any {
	files := make([]any, 0, len(attachments))
	for _, attachment := range attachments {
		files = append(files, map[string]any{
			"path":           attachment.Path,
			"name":           attachment.Name,
			"blob_reference": attachment.Reference,
		})
	}
	return files
}

// Admission-submission support for the fixture daemon.
//
// Real daemon behavior (crates/fintwind-core/src/daemon.rs,
// crates/fintwind-protocol/src/submission.rs) reduced to fixture scope:
//   - AcceptSubmission stores the frozen prompt identity once.
//   - DispatchSubmission claims Preparing -> Dispatching exactly once and
//     streams a single canonical turn bound to the accepted turn ID.
//   - AbandonSubmission moves only Preparing -> NotSent; any other state
//     returns the stored receipt unchanged, never a forged NotSent.
//
// Wire keys follow the Rust structs exactly: SubmissionReceipt and
// SubmissionRecord use snake_case fields, states are camelCase words
// (preparing, dispatching, accepted, unknown, notSent, rejected).
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"time"

	"github.com/google/uuid"
)

type fixtureReceipt struct {
	ID              string  `json:"id"`
	InputID         string  `json:"input_id"`
	State           string  `json:"state"`
	CreatedAt       uint64  `json:"created_at"`
	UpdatedAt       uint64  `json:"updated_at"`
	NativeSessionID *string `json:"native_session_id,omitempty"`
}

type submissionEntry struct {
	SessionID     string          `json:"session_id"`
	RuntimeID     string          `json:"runtime_id,omitempty"`
	Receipt       fixtureReceipt  `json:"receipt"`
	Prompt        string          `json:"prompt"`
	Files         json.RawMessage `json:"files"`
	DispatchCount int             `json:"dispatch_count"`
	Cancelled     bool            `json:"cancelled,omitempty"`
	cancel        chan struct{}
	streamActive  bool
}

func nowMillis() uint64 {
	return uint64(time.Now().UnixMilli())
}

func canonicalJSON(raw json.RawMessage) string {
	if len(raw) == 0 {
		return "[]"
	}
	var value any
	if err := json.Unmarshal(raw, &value); err != nil {
		return string(raw)
	}
	if value == nil {
		return "[]"
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return string(raw)
	}
	return string(encoded)
}

func filesEqual(a, b json.RawMessage) bool {
	return canonicalJSON(normalizeFiles(a)) == canonicalJSON(normalizeFiles(b))
}

func normalizeFiles(raw json.RawMessage) json.RawMessage {
	if len(raw) == 0 {
		return json.RawMessage("[]")
	}
	var value any
	if err := json.Unmarshal(raw, &value); err != nil {
		return raw
	}
	if value == nil {
		return json.RawMessage("[]")
	}
	return raw
}

func newUUID() string {
	return uuid.NewString()
}

func (fx *fixture) submissionsPath() string {
	return filepath.Join(fx.dataDir, "fixture-submissions.json")
}

func (fx *fixture) loadSubmissions() {
	raw, err := os.ReadFile(fx.submissionsPath())
	if err != nil {
		return
	}
	var stored struct {
		Submissions []submissionEntry `json:"submissions"`
	}
	if err := json.Unmarshal(raw, &stored); err != nil {
		return
	}
	for _, entry := range stored.Submissions {
		if entry.Receipt.ID == "" {
			continue
		}
		kept := entry
		fx.submissions[entry.Receipt.ID] = &kept
	}
}

func (fx *fixture) persistSubmissionsLocked() {
	entries := make([]submissionEntry, 0, len(fx.submissions))
	for _, entry := range fx.submissions {
		entries = append(entries, *entry)
	}
	sort.Slice(entries, func(i, j int) bool {
		return entries[i].Receipt.ID < entries[j].Receipt.ID
	})
	stored := struct {
		Submissions []submissionEntry `json:"submissions"`
	}{Submissions: entries}
	raw, err := json.MarshalIndent(stored, "", "  ")
	if err != nil {
		return
	}
	_ = os.WriteFile(fx.submissionsPath(), append(raw, '\n'), 0o644)
}

type wireTurn struct {
	ID                  string          `json:"id"`
	Status              string          `json:"status"`
	Submission          *fixtureReceipt `json:"submission"`
	ProviderTurnStarted bool            `json:"provider_turn_started"`
}

type wireSession struct {
	ID        string         `json:"id"`
	ProjectID string         `json:"project_id"`
	Status    string         `json:"status"`
	Turns     []wireTurn     `json:"turns"`
	Raw       map[string]any `json:"-"`
}

func parseWireSession(raw json.RawMessage) (wireSession, error) {
	var session wireSession
	if err := json.Unmarshal(raw, &session); err != nil {
		return session, err
	}
	var generic map[string]any
	if err := json.Unmarshal(raw, &generic); err == nil {
		session.Raw = generic
	}
	return session, nil
}

func sessionIsBusy(status string) bool {
	return status == "connecting" || status == "working" || status == "waiting"
}

// setTurnReceipt rewrites the submission receipt of one turn inside a stored
// session blob without touching any other field.
func setTurnReceipt(sessionRaw json.RawMessage, turnID string, receipt fixtureReceipt) json.RawMessage {
	var session map[string]any
	if err := json.Unmarshal(sessionRaw, &session); err != nil {
		return sessionRaw
	}
	turns, _ := session["turns"].([]any)
	for _, item := range turns {
		turn, _ := item.(map[string]any)
		if turn == nil {
			continue
		}
		if id, _ := turn["id"].(string); id != turnID {
			continue
		}
		raw, _ := json.Marshal(receipt)
		var receiptValue any
		_ = json.Unmarshal(raw, &receiptValue)
		turn["submission"] = receiptValue
	}
	updated, err := json.Marshal(session)
	if err != nil {
		return sessionRaw
	}
	return json.RawMessage(updated)
}

func failTurnIfPreProvider(sessionRaw json.RawMessage, turnID string) json.RawMessage {
	var session map[string]any
	if err := json.Unmarshal(sessionRaw, &session); err != nil {
		return sessionRaw
	}
	turns, _ := session["turns"].([]any)
	changed := false
	for _, item := range turns {
		turn, _ := item.(map[string]any)
		if turn == nil {
			continue
		}
		if id, _ := turn["id"].(string); id != turnID {
			continue
		}
		started, _ := turn["provider_turn_started"].(bool)
		status, _ := turn["status"].(string)
		if !started && status == "running" {
			turn["status"] = "failed"
			turn["completed_at"] = uint64(time.Now().Unix())
			changed = true
		}
	}
	if changed {
		session["status"] = "idle"
		session["updated_at"] = uint64(time.Now().Unix())
	}
	updated, err := json.Marshal(session)
	if err != nil {
		return sessionRaw
	}
	return json.RawMessage(updated)
}

// reconcileReceiptsOnSave keeps a stale autosave from downgrading a receipt
// that already reached a higher rank. Called with fx.mu held.
func (fx *fixture) reconcileReceiptsOnSaveLocked(sessionRaw json.RawMessage) json.RawMessage {
	var session map[string]any
	if err := json.Unmarshal(sessionRaw, &session); err != nil {
		return sessionRaw
	}
	turns, _ := session["turns"].([]any)
	dirty := false
	for _, item := range turns {
		turn, _ := item.(map[string]any)
		if turn == nil {
			continue
		}
		submission, _ := turn["submission"].(map[string]any)
		if submission == nil {
			continue
		}
		raw, _ := json.Marshal(submission)
		var incoming fixtureReceipt
		if err := json.Unmarshal(raw, &incoming); err != nil || incoming.ID == "" {
			continue
		}
		entry, ok := fx.submissions[incoming.ID]
		if !ok {
			continue
		}
		if entry.Receipt.InputID != incoming.InputID {
			continue
		}
		kept, _ := json.Marshal(entry.Receipt)
		if canonicalJSON(raw) != canonicalJSON(kept) {
			var keptValue any
			_ = json.Unmarshal(kept, &keptValue)
			turn["submission"] = keptValue
			dirty = true
		}
	}
	if !dirty {
		return sessionRaw
	}
	updated, err := json.Marshal(session)
	if err != nil {
		return sessionRaw
	}
	return json.RawMessage(updated)
}

type submissionStat struct {
	SessionID     string `json:"sessionId"`
	SubmissionID  string `json:"submissionId"`
	State         string `json:"state"`
	DispatchCount int    `json:"dispatchCount"`
	InputID       string `json:"inputId"`
	UpdatedAt     uint64 `json:"updatedAt"`
	StreamActive  bool   `json:"streamActive"`
	Cancelled     bool   `json:"cancelled"`
}

func (fx *fixture) submissionStats(sessionFilter string) []submissionStat {
	fx.mu.Lock()
	defer fx.mu.Unlock()
	stats := make([]submissionStat, 0, len(fx.submissions))
	for _, entry := range fx.submissions {
		if sessionFilter != "" && entry.SessionID != sessionFilter {
			continue
		}
		stats = append(stats, submissionStat{
			SessionID:     entry.SessionID,
			SubmissionID:  entry.Receipt.ID,
			State:         entry.Receipt.State,
			DispatchCount: entry.DispatchCount,
			InputID:       entry.Receipt.InputID,
			UpdatedAt:     entry.Receipt.UpdatedAt,
			StreamActive:  entry.streamActive,
			Cancelled:     entry.Cancelled,
		})
	}
	sort.Slice(stats, func(i, j int) bool {
		return stats[i].SubmissionID < stats[j].SubmissionID
	})
	return stats
}

func (fx *fixture) waitForGate(kind string) {
	for {
		fx.mu.Lock()
		var until int64
		if kind == "accept" {
			until = fx.acceptHoldUntilMs
		} else {
			until = fx.dispatchHoldUntilMs
		}
		fx.mu.Unlock()
		now := time.Now().UnixMilli()
		if until <= 0 || now >= until {
			return
		}
		remaining := until - now
		if remaining > 50 {
			remaining = 50
		}
		time.Sleep(time.Duration(remaining) * time.Millisecond)
	}
}

func (fx *fixture) handleAccept(dc *daemonConn, requestID, envelopeSession string, rawCommand json.RawMessage) {
	var command struct {
		Project struct {
			ID string `json:"id"`
		} `json:"project"`
		Session    json.RawMessage `json:"session"`
		Submission struct {
			SessionID string          `json:"session_id"`
			Receipt   fixtureReceipt  `json:"receipt"`
			Prompt    string          `json:"prompt"`
			Files     json.RawMessage `json:"files"`
		} `json:"submission"`
	}
	if err := json.Unmarshal(rawCommand, &command); err != nil {
		fx.respondError(dc, requestID, "fixture: bad accept payload")
		return
	}
	fx.waitForGate("accept")
	session, err := parseWireSession(command.Session)
	if err != nil {
		fx.respondError(dc, requestID, "invalid submission identity")
		return
	}
	receipt := command.Submission.Receipt
	if envelopeSession == "" || session.ID == "" || session.ID != envelopeSession ||
		command.Submission.SessionID != envelopeSession ||
		session.ProjectID == "" || session.ProjectID != command.Project.ID ||
		receipt.ID == "" || receipt.InputID == "" ||
		receipt.State != "preparing" {
		fx.respondError(dc, requestID, "invalid submission identity")
		return
	}
	if loaded, _ := session.Raw["detail_loaded"].(bool); !loaded {
		fx.respondError(dc, requestID, "invalid submission identity")
		return
	}
	if len(session.Turns) == 0 {
		fx.respondError(dc, requestID, "invalid submission identity")
		return
	}
	last := session.Turns[len(session.Turns)-1]
	if last.ID != receipt.ID || last.Status != "running" {
		fx.respondError(dc, requestID, "invalid submission identity")
		return
	}

	fx.mu.Lock()
	defer fx.mu.Unlock()
	if existing, ok := fx.submissions[receipt.ID]; ok {
		if existing.SessionID != envelopeSession ||
			existing.Receipt.InputID != receipt.InputID ||
			existing.Prompt != command.Submission.Prompt ||
			!filesEqual(existing.Files, command.Submission.Files) {
			fx.respondError(dc, requestID, "submission ID conflicts with a saved input")
			return
		}
		fx.respondOK(dc, requestID, map[string]any{
			"type": "submissionSaved", "receipt": existing.Receipt,
		})
		return
	}
	if stored, ok := fx.sessions[envelopeSession]; ok {
		if kept, err := parseWireSession(stored); err == nil && len(kept.Turns) > 0 {
			storedLast := kept.Turns[len(kept.Turns)-1]
			if storedLast.ID != receipt.ID &&
				(sessionIsBusy(kept.Status) || storedLast.Status == "running") {
				fx.respondError(dc, requestID, "session already has an active submission")
				return
			}
		}
	}
	for _, entry := range fx.submissions {
		if entry.SessionID != envelopeSession || entry.Receipt.ID == receipt.ID {
			continue
		}
		switch entry.Receipt.State {
		case "preparing", "dispatching", "unknown":
			fx.respondError(dc, requestID, "session already has an active submission")
			return
		}
	}
	files := command.Submission.Files
	if len(files) == 0 {
		files = json.RawMessage("[]")
	}
	fx.submissions[receipt.ID] = &submissionEntry{
		SessionID: envelopeSession,
		Receipt:   receipt,
		Prompt:    command.Submission.Prompt,
		Files:     files,
	}
	fx.sessions[envelopeSession] = fx.reconcileReceiptsOnSaveLocked(command.Session)
	if projectID := command.Project.ID; projectID != "" {
		found := false
		for _, raw := range fx.projects {
			if sessionIDOf(raw) == projectID {
				found = true
				break
			}
		}
		if !found {
			fx.projects = append(fx.projects, extractProjectRaw(rawCommand))
		}
	}
	fx.persistTaskState()
	fx.persistSubmissionsLocked()
	fmt.Fprintf(os.Stderr, "fixturedaemon: acceptSubmission saved id=%s\n", shortID(receipt.ID))
	fx.respondOK(dc, requestID, map[string]any{
		"type": "submissionSaved", "receipt": receipt,
	})
}

func extractProjectRaw(rawCommand json.RawMessage) json.RawMessage {
	var command struct {
		Project json.RawMessage `json:"project"`
	}
	if err := json.Unmarshal(rawCommand, &command); err != nil {
		return nil
	}
	return command.Project
}

func (fx *fixture) handleDispatch(dc *daemonConn, requestID, envelopeSession, envelopeRuntime string, rawCommand json.RawMessage) {
	var command struct {
		SubmissionID string         `json:"submissionId"`
		Workspace    map[string]any `json:"workspace"`
	}
	if err := json.Unmarshal(rawCommand, &command); err != nil || command.SubmissionID == "" {
		fx.respondError(dc, requestID, "fixture: bad dispatch payload")
		return
	}
	fx.waitForGate("dispatch")
	fx.mu.Lock()
	entry, ok := fx.submissions[command.SubmissionID]
	if !ok {
		fx.mu.Unlock()
		fx.respondError(dc, requestID, "submission is missing")
		return
	}
	if entry.SessionID != envelopeSession {
		fx.mu.Unlock()
		fx.respondError(dc, requestID, "submission belongs to another session")
		return
	}
	if entry.Receipt.State != "preparing" {
		receipt := entry.Receipt
		fx.mu.Unlock()
		fx.respondOK(dc, requestID, map[string]any{
			"type": "submissionSaved", "receipt": receipt,
		})
		return
	}
	runtime, bound := fx.runtimes[envelopeSession]
	if !bound || runtime != envelopeRuntime {
		fx.mu.Unlock()
		if !bound {
			fx.respondError(dc, requestID, "submission runtime is unavailable")
		} else {
			fx.respondError(dc, requestID, "submission runtime was replaced")
		}
		return
	}
	stored, ok := fx.sessions[envelopeSession]
	if !ok {
		fx.mu.Unlock()
		fx.respondError(dc, requestID, "submission session is missing")
		return
	}
	kept, err := parseWireSession(stored)
	if err != nil || len(kept.Turns) == 0 || kept.Turns[len(kept.Turns)-1].ID != command.SubmissionID {
		fx.mu.Unlock()
		fx.respondError(dc, requestID, "submission turn was replaced")
		return
	}
	entry.Receipt.State = "dispatching"
	entry.Receipt.UpdatedAt = nowMillis()
	entry.DispatchCount++
	entry.RuntimeID = envelopeRuntime
	entry.cancel = make(chan struct{})
	entry.streamActive = true
	dispatched := entry.Receipt
	fx.sessions[envelopeSession] = setTurnReceipt(stored, command.SubmissionID, dispatched)
	fx.persistTaskState()
	fx.persistSubmissionsLocked()
	fx.mu.Unlock()

	fmt.Fprintf(os.Stderr, "fixturedaemon: dispatchSubmission id=%s count=1\n", shortID(command.SubmissionID))
	fx.respondOK(dc, requestID, map[string]any{
		"type": "submissionSaved", "receipt": dispatched,
	})
	go fx.streamAdmission(envelopeSession, envelopeRuntime, command.SubmissionID)
}

func (fx *fixture) handleAbandon(dc *daemonConn, requestID, envelopeSession string, rawCommand json.RawMessage) {
	var command struct {
		SubmissionID string `json:"submissionId"`
	}
	if err := json.Unmarshal(rawCommand, &command); err != nil || command.SubmissionID == "" {
		fx.respondError(dc, requestID, "fixture: bad abandon payload")
		return
	}
	fx.mu.Lock()
	defer fx.mu.Unlock()
	entry, ok := fx.submissions[command.SubmissionID]
	if !ok {
		fx.respondError(dc, requestID, "submission is missing")
		return
	}
	if entry.SessionID != envelopeSession {
		fx.respondError(dc, requestID, "submission belongs to another session")
		return
	}
	if entry.Receipt.State == "preparing" {
		entry.Receipt.State = "notSent"
		entry.Receipt.UpdatedAt = nowMillis()
		if stored, ok := fx.sessions[envelopeSession]; ok {
			updated := setTurnReceipt(stored, command.SubmissionID, entry.Receipt)
			updated = failTurnIfPreProvider(updated, command.SubmissionID)
			fx.sessions[envelopeSession] = fx.reconcileReceiptsOnSaveLocked(updated)
		}
		fx.persistTaskState()
		fx.persistSubmissionsLocked()
		fmt.Fprintf(os.Stderr, "fixturedaemon: abandonSubmission id=%s notSent\n", shortID(command.SubmissionID))
	}
	fx.respondOK(dc, requestID, map[string]any{
		"type": "submissionSaved", "receipt": entry.Receipt,
	})
}

func (fx *fixture) streamAdmission(sessionID, runtimeID, submissionID string) {
	defer func() {
		fx.mu.Lock()
		if entry := fx.submissions[submissionID]; entry != nil {
			entry.streamActive = false
		}
		fx.mu.Unlock()
	}()
	fx.mu.Lock()
	entry, ok := fx.submissions[submissionID]
	if !ok || entry.Cancelled || entry.Receipt.State != "dispatching" {
		fx.mu.Unlock()
		return
	}
	entry.Receipt.State = "accepted"
	entry.Receipt.UpdatedAt = nowMillis()
	accepted := entry.Receipt
	cancelled := entry.cancel
	if stored, ok := fx.sessions[sessionID]; ok {
		updated := setTurnReceipt(stored, submissionID, accepted)
		var session map[string]any
		if err := json.Unmarshal(updated, &session); err == nil {
			turns, _ := session["turns"].([]any)
			for _, item := range turns {
				turn, _ := item.(map[string]any)
				if turn == nil {
					continue
				}
				if id, _ := turn["id"].(string); id == submissionID {
					turn["provider_turn_started"] = true
				}
			}
			if raw, err := json.Marshal(session); err == nil {
				updated = json.RawMessage(raw)
			}
		}
		fx.sessions[sessionID] = fx.reconcileReceiptsOnSaveLocked(updated)
	}
	fx.persistTaskState()
	fx.persistSubmissionsLocked()
	fx.mu.Unlock()

	permissionID := newUUID()
	questionID := newUUID()
	type streamStep struct {
		kind    string
		payload any
		replyID string
	}
	steps := []streamStep{
		{kind: "submissionUpdated", payload: accepted},
		{kind: "turnStarted", payload: map[string]any{}},
		{kind: "textStarted", payload: map[string]any{}},
	}
	for _, chunk := range streamChunks {
		steps = append(steps, streamStep{kind: "textDelta", payload: chunk})
	}
	steps = append(steps,
		streamStep{kind: "reasoningStarted", payload: map[string]any{}},
		streamStep{kind: "reasoningDelta", payload: streamReason},
		streamStep{kind: "reasoningEnded", payload: streamReason},
		streamStep{kind: "activity", payload: map[string]any{
			"id": "fixture-activity-1", "kind": "tool",
			"title": "fixture tool call", "detail": "echo ok", "complete": true,
		}},
		streamStep{kind: "permission", replyID: permissionID, payload: map[string]any{
			"requestId": permissionID, "title": "Fixture permission",
			"detail": "Allow the fixture step to continue?",
			"options": []any{
				map[string]any{"id": "allow", "label": "Allow", "allow": true},
				map[string]any{"id": "deny", "label": "Deny", "allow": false},
			},
		}},
		streamStep{kind: "userInputRequested", replyID: questionID, payload: map[string]any{
			"requestId": questionID,
			"questions": []any{map[string]any{
				"id": "q1", "header": "Fixture", "question": "Fixture asks: pick one?",
				"options": []any{
					map[string]any{"label": "Option A"},
					map[string]any{"label": "Option B"},
				},
				"multiSelect": false,
			}},
		}},
		streamStep{kind: "textEnded", payload: streamFullText})
	for _, step := range steps {
		timer := time.NewTimer(40 * time.Millisecond)
		select {
		case <-cancelled:
			timer.Stop()
			return
		case <-timer.C:
		}
		var reply chan string
		if step.replyID != "" {
			reply = make(chan string, 1)
			fx.mu.Lock()
			fx.pending[step.replyID] = reply
			fx.mu.Unlock()
		}
		fx.emit(sessionID, runtimeID, step.kind, step.payload)
		if reply != nil {
			timer = time.NewTimer(20 * time.Second)
			select {
			case <-reply:
			case <-cancelled:
			case <-timer.C:
			}
			timer.Stop()
			fx.mu.Lock()
			delete(fx.pending, step.replyID)
			fx.mu.Unlock()
		}
	}
	select {
	case <-cancelled:
		return
	default:
	}
	fx.finishAdmissionTranscript(sessionID, submissionID)
	select {
	case <-cancelled:
		return
	default:
	}
	fx.emit(sessionID, runtimeID, "turnFinished", map[string]any{"success": true})
}

func (fx *fixture) cancelAdmission(sessionID, runtimeID string) {
	fx.mu.Lock()
	defer fx.mu.Unlock()
	raw, exists := fx.sessions[sessionID]
	if !exists {
		return
	}
	var session map[string]any
	if json.Unmarshal(raw, &session) != nil {
		return
	}
	turns, _ := session["turns"].([]any)
	if len(turns) == 0 {
		return
	}
	turn, _ := turns[len(turns)-1].(map[string]any)
	if turn == nil || turn["status"] == "completed" {
		return
	}
	turnID, _ := turn["id"].(string)
	entry := fx.submissions[turnID]
	if entry == nil || entry.RuntimeID != runtimeID || entry.Cancelled {
		return
	}
	entry.Cancelled = true
	if entry.cancel != nil {
		close(entry.cancel)
	}
	turn["status"] = "interrupted"
	turn["completed_at"] = uint64(time.Now().Unix())
	session["status"] = "idle"
	if updated, err := json.Marshal(session); err == nil {
		fx.sessions[sessionID] = updated
	}
	fx.persistTaskState()
	fx.persistSubmissionsLocked()
}

func (fx *fixture) finishAdmissionTranscript(sessionID, turnID string) {
	fx.mu.Lock()
	defer fx.mu.Unlock()
	entry, ok := fx.submissions[turnID]
	if !ok || entry.Cancelled {
		return
	}
	raw, ok := fx.sessions[sessionID]
	if !ok {
		return
	}
	var session map[string]any
	if err := json.Unmarshal(raw, &session); err != nil {
		return
	}
	now := uint64(time.Now().Unix())
	turns, _ := session["turns"].([]any)
	for _, item := range turns {
		turn, _ := item.(map[string]any)
		if turn == nil {
			continue
		}
		if id, _ := turn["id"].(string); id != turnID {
			continue
		}
		turn["status"] = "completed"
		turn["completed_at"] = now
		turn["provider_turn_started"] = true
		receiptRaw, _ := json.Marshal(entry.Receipt)
		var receiptValue any
		_ = json.Unmarshal(receiptRaw, &receiptValue)
		turn["submission"] = receiptValue
	}
	messages, _ := session["messages"].([]any)
	alreadyStored := false
	for _, item := range messages {
		message, _ := item.(map[string]any)
		if message == nil || message["role"] != "user" {
			continue
		}
		if text, _ := message["content"].(string); text == entry.Prompt {
			alreadyStored = true
			break
		}
	}
	if !alreadyStored {
		messages = append(messages, map[string]any{
			"id": "fixture-user-" + turnID, "turn_id": turnID,
			"role": "user", "content": entry.Prompt, "created_at": now,
		})
	}
	messages = append(messages, map[string]any{
		"id": "fixture-assistant-" + turnID, "turn_id": turnID,
		"role": "assistant", "content": streamFullText, "created_at": now,
	})
	session["messages"] = messages
	session["status"] = "idle"
	session["updated_at"] = now
	updated, err := json.Marshal(session)
	if err != nil {
		return
	}
	fx.sessions[sessionID] = fx.reconcileReceiptsOnSaveLocked(json.RawMessage(updated))
	fx.persistTaskState()
	fx.persistSubmissionsLocked()
}

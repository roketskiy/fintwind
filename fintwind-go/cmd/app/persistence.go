package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"strconv"
	"time"

	"fintwind-go/internal/model"
)

// persistence sends the client-owned task state back to the daemon. The
// hydrated session JSON is the base so fields this client never reads
// (workspace, goal, usage, runtime cursor) survive the round trip; only the
// keys the client owns are patched.

// captureRawSession keeps the session object exactly as the daemon sent it.
func (a *app) captureRawSession(payload json.RawMessage) {
	var holder map[string]json.RawMessage
	if json.Unmarshal(payload, &holder) != nil {
		a.detailRaw = nil
		return
	}
	raw, ok := holder["session"]
	if !ok {
		a.detailRaw = nil
		return
	}
	var session map[string]any
	if json.Unmarshal(raw, &session) != nil {
		a.detailRaw = nil
		return
	}
	a.detailRaw = session
}

// preserveLocalSessionFields keeps facts this client learned live (provider
// cursor, auto title) when the daemon's stored projection has not caught up.
func (a *app) preserveLocalSessionFields(incoming *model.Session) {
	if incoming == nil {
		return
	}
	if len(incoming.ProviderCursor) == 0 && incoming.NativeSessionID != nil {
		incoming.ProviderCursor = json.RawMessage(
			`{"provider":"openCode","sessionId":` + strconv.Quote(*incoming.NativeSessionID) + `}`)
	}
	if a.detail != nil && a.detail.ID == incoming.ID {
		if len(incoming.ProviderCursor) == 0 && len(a.detail.ProviderCursor) > 0 {
			incoming.ProviderCursor = a.detail.ProviderCursor
		}
		if incoming.AutoTitle == nil && a.detail.AutoTitle != nil {
			incoming.AutoTitle = a.detail.AutoTitle
		}
	}
	if incoming.NativeSessionID == nil {
		if native := nativeSessionID(incoming.ProviderCursor); native != "" {
			incoming.NativeSessionID = &native
		}
	}
}

// saveSessionSnapshot sends the current session to the daemon. The call runs
// on a worker goroutine; the snapshot is built first so the payload cannot
// race later edits.
func (a *app) saveSessionSnapshot(session *model.Session) {
	if session == nil || a.client == nil {
		return
	}
	if a.sessionActionBlocksSnapshot(session.ID) {
		return
	}
	project := a.projectForID(session.ProjectID)
	if project == nil {
		return
	}
	payload := a.buildSessionSnapshot(session, project)
	if payload == nil {
		return
	}
	encoded, err := json.Marshal(payload)
	if err != nil {
		a.status = tr("Could not save the session: ") + err.Error()
		return
	}
	if a.snapshots.pending == nil {
		a.snapshots.pending = make(map[string]json.RawMessage)
	}
	a.snapshots.pending[session.ID] = encoded
	a.startNextSnapshot()
}

type snapshotWriter struct {
	inFlight   bool
	pending    map[string]json.RawMessage
	finished   chan struct{}
	admissions []*submissionAdmission
}

type submissionAdmission struct {
	done      chan struct{}
	payload   json.RawMessage
	err       error
	sessionID string
	request   json.RawMessage
}

func (a *app) acceptSessionSubmission(session *model.Session, record any) *submissionAdmission {
	admission := &submissionAdmission{done: make(chan struct{}), sessionID: session.ID}
	project := a.projectForID(session.ProjectID)
	if a.client == nil || a.connection.phase != "online" || project == nil || a.sessionActionBlocksInput(session.ID) {
		admission.err = fmt.Errorf("cannot admit a session without its connected project")
		close(admission.done)
		return admission
	}
	snapshot := a.buildSessionSnapshot(session, project)
	raw := snapshot["sessions"].([]any)[0].(map[string]any)
	raw["detail_loaded"] = true
	encoded, err := json.Marshal(map[string]any{
		"type": "acceptSubmission", "project": snapshot["projects"].([]any)[0],
		"session": raw, "submission": record,
	})
	if err != nil {
		admission.err = err
		close(admission.done)
		return admission
	}
	admission.request = encoded
	delete(a.snapshots.pending, session.ID)
	a.snapshots.admissions = append(a.snapshots.admissions, admission)
	a.startNextSnapshot()
	return admission
}

func (a *app) finishSubmissionAdmission(session *model.Session) {
	state := a.stateForSession(session)
	state.admissionBusy = false
	a.saveSessionSnapshot(session)
	if !state.stopped && lastTurnSettledCleanly(session) {
		a.post(func() { a.drainQueuedSubmission(session) })
	}
}

func (a *app) startNextAdmission(admission *submissionAdmission) {
	a.snapshots.inFlight = true
	finished := make(chan struct{})
	a.snapshots.finished = finished
	client := a.client
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 30*time.Second)
		defer cancel()
		payload, err := client.RequestFor(ctx, admission.sessionID, "", admission.request)
		admission.payload, admission.err = payload, err
		close(admission.done)
		close(finished)
		a.post(func() {
			a.snapshots.inFlight = false
			a.startNextSnapshot()
		})
	}()
}

func (a *app) startNextSnapshot() {
	if a.snapshots.inFlight || a.client == nil || a.connection.phase != "online" {
		return
	}
	a.startHistoryRewinds()
	if len(a.snapshots.admissions) > 0 {
		admission := a.snapshots.admissions[0]
		a.snapshots.admissions = a.snapshots.admissions[1:]
		a.startNextAdmission(admission)
		return
	}
	var id string
	var payload json.RawMessage
	for candidate, snapshot := range a.snapshots.pending {
		id, payload = candidate, snapshot
		break
	}
	if id == "" {
		return
	}
	delete(a.snapshots.pending, id)
	a.snapshots.inFlight = true
	finished := make(chan struct{})
	a.snapshots.finished = finished
	client := a.client
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		_, err := client.Request(ctx, payload)
		close(finished)
		a.post(func() {
			a.snapshots.inFlight = false
			if err != nil {
				superseded := false
				for _, admission := range a.snapshots.admissions {
					if admission.sessionID == id {
						superseded = true
						break
					}
				}
				if _, newer := a.snapshots.pending[id]; !newer && !superseded {
					a.snapshots.pending[id] = payload
				}
				a.status = fmt.Sprintf(tr("Could not save the session: %v"), err)
				if len(a.snapshots.admissions) > 0 {
					a.startNextSnapshot()
				}
				return
			}
			a.startNextSnapshot()
		})
	}()
}

func (a *app) flushSnapshotsOnClose() {
	if a.client == nil || a.connection.phase != "online" {
		return
	}
	a.rememberSession()
	for _, admission := range a.snapshots.admissions {
		admission.err = fmt.Errorf("window closed before submission admission")
		var command struct {
			Submission struct {
				Receipt submissionReceipt `json:"receipt"`
			} `json:"submission"`
		}
		if json.Unmarshal(admission.request, &command) == nil {
			if state := a.live[admission.sessionID]; state != nil && state.session != nil {
				if state.admission == admission {
					state.admissionBusy = false
				}
				turnID := command.Submission.Receipt.ID
				if turn := state.session.TurnByID(turnID); turn != nil {
					turn.Submission = json.RawMessage("null")
					turn.Status = "failed"
					completed := uint64(time.Now().Unix())
					turn.CompletedAt = &completed
					if last := len(state.session.Turns) - 1; last >= 0 && state.session.Turns[last].ID == turnID {
						state.session.Status = "idle"
					}
				}
			}
		}
		close(admission.done)
	}
	a.snapshots.admissions = nil
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if a.snapshots.inFlight {
		select {
		case <-a.snapshots.finished:
		case <-ctx.Done():
			return
		}
	}
	for _, state := range a.live {
		if !state.admissionBusy || state.session == nil || state.admission == nil {
			continue
		}
		var command struct {
			Submission struct {
				Receipt submissionReceipt `json:"receipt"`
			} `json:"submission"`
		}
		if json.Unmarshal(state.admission.request, &command) != nil {
			continue
		}
		receiptID := command.Submission.Receipt.ID
		if receiptID == "" {
			continue
		}
		payload, err := a.client.RequestFor(ctx, state.session.ID, "", map[string]any{"type": "abandonSubmission", "submissionId": receiptID})
		if err != nil {
			log.Printf("resolve submission while closing: %v", err)
			continue
		}
		var saved struct {
			Receipt json.RawMessage `json:"receipt"`
		}
		if json.Unmarshal(payload, &saved) == nil && len(saved.Receipt) > 0 {
			a.applySubmissionUpdated(state.session, saved.Receipt)
		}
	}
	projects := make(map[string]any)
	sessions := make(map[string]any)
	collect := func(payload map[string]any) {
		for _, field := range []struct {
			name    string
			entries map[string]any
		}{{"projects", projects}, {"sessions", sessions}} {
			if values, ok := payload[field.name].([]any); ok {
				for _, value := range values {
					if entry, ok := value.(map[string]any); ok {
						if id, ok := entry["id"].(string); ok {
							field.entries[id] = entry
						}
					}
				}
			}
		}
	}
	for _, raw := range a.snapshots.pending {
		var payload map[string]any
		if json.Unmarshal(raw, &payload) == nil {
			collect(payload)
		}
	}
	for _, state := range a.live {
		session := state.session
		if session == nil || (session.Draft && len(session.Messages) == 0) {
			continue
		}
		project := a.projectForID(session.ProjectID)
		if project == nil {
			continue
		}
		collect(a.buildSessionSnapshot(session, project))
	}
	if len(sessions) == 0 {
		return
	}
	projectValues := make([]any, 0, len(projects))
	for _, project := range projects {
		projectValues = append(projectValues, project)
	}
	sessionValues := make([]any, 0, len(sessions))
	for _, session := range sessions {
		sessionValues = append(sessionValues, session)
	}
	if _, err := a.client.Request(ctx, map[string]any{
		"type": "saveTaskStateCheckpoint", "projects": projectValues,
		"sessions": sessionValues, "skeletonSessionIds": []string{},
	}); err != nil {
		log.Printf("save sessions while closing: %v", err)
	}
}

// buildSessionSnapshot patches the hydrated session object with the fields
// this client mutated and wraps it in a merge-only save.
func (a *app) buildSessionSnapshot(session *model.Session, project *model.Project) map[string]any {
	if session == nil || a.sessionActionBlocksSnapshot(session.ID) {
		return nil
	}
	raw := map[string]any{}
	base := a.stateForSession(session).raw
	if a.detail != nil && a.detail.ID == session.ID {
		base = a.detailRaw
	}
	if base != nil {
		if id, _ := base["id"].(string); id == session.ID {
			for key, value := range base {
				raw[key] = value
			}
		}
	}
	raw["id"] = session.ID
	raw["title"] = session.Title
	raw["project_id"] = session.ProjectID
	raw["provider"] = session.Provider
	raw["runtime_mode"] = session.RuntimeMode
	raw["interaction_mode"] = session.InteractionMode
	raw["status"] = session.Status
	if _, exists := raw["mermaid_prompt_version"]; !exists {
		raw["mermaid_prompt_version"] = nullableString(a.sessionMermaidVersion(session))
	}
	raw["created_at"] = session.CreatedAt
	raw["updated_at"] = session.UpdatedAt
	raw["archived"] = session.Archived
	setOptional(raw, "auto_title", session.AutoTitle)
	setOptional(raw, "model", session.Model)
	setOptional(raw, "reasoning_effort", session.ReasoningEffort)
	setOptional(raw, "service_tier", session.ServiceTier)
	setOptional(raw, "context_window", session.ContextWindow)
	setOptional(raw, "agent_preset", session.AgentPreset)
	setOptional(raw, "native_session_id", session.NativeSessionID)
	if session.LastReplyAt != nil {
		raw["last_reply_at"] = *session.LastReplyAt
	} else {
		delete(raw, "last_reply_at")
	}
	if len(session.ProviderCursor) > 0 {
		var cursor any
		if json.Unmarshal(session.ProviderCursor, &cursor) == nil {
			raw["provider_cursor"] = cursor
		}
	}
	raw["messages"] = mergeEntries(raw["messages"], messagesToMaps(session.Messages), "id")
	raw["turns"] = mergeEntries(raw["turns"], turnsToMaps(session.Turns), "id")
	raw["transcript_blocks"] = transcriptBlocksToMaps(session.TranscriptBlocks)
	if cursor := session.RuntimeEventCursor; cursor != nil {
		raw["runtime_event_cursor"] = map[string]any{"runtime_id": cursor.RuntimeID, "epoch": cursor.Epoch, "sequence": cursor.Sequence}
	}
	if session.ContextUsage != nil {
		if encoded, err := marshalJSON(session.ContextUsage); err == nil {
			merged := map[string]any{}
			if previous, ok := raw["context_usage"].(map[string]any); ok {
				for key, value := range previous {
					merged[key] = value
				}
			}
			if incoming, ok := encoded.(map[string]any); ok {
				for key, value := range incoming {
					merged[key] = value
				}
			}
			raw["context_usage"] = merged
		}
	}
	if session.Compaction != nil {
		if value, err := marshalJSON(session.Compaction); err == nil {
			raw["compaction"] = value
		}
	}
	queued := make([]any, 0, len(session.QueuedMessages))
	for _, message := range session.QueuedMessages {
		encoded, err := marshalJSON(message)
		if err == nil {
			queued = append(queued, encoded)
		}
	}
	raw["queued_messages"] = queued
	return map[string]any{
		"type": "saveTaskStateCheckpoint",
		"projects": []any{map[string]any{
			"id":             project.ID,
			"name":           project.Name,
			"path":           project.Path,
			"environment_id": project.EnvironmentID,
			"created_at":     project.CreatedAt,
		}},
		"skeletonSessionIds": []string{},
		"sessions":           []any{raw},
	}
}

// messagesToMaps encodes the messages this client owns.
func messagesToMaps(messages []model.Message) []any {
	out := make([]any, 0, len(messages))
	for index := range messages {
		if encoded, err := marshalJSON(messages[index]); err == nil {
			out = append(out, encoded)
		}
	}
	return out
}

// turnsToMaps encodes the turn fields this client owns, leaving the rest to
// the merge below.
func turnsToMaps(turns []model.AgentTurn) []any {
	out := make([]any, 0, len(turns))
	for index := range turns {
		encoded, err := marshalJSON(turns[index])
		if err != nil {
			continue
		}
		if asMap, ok := encoded.(map[string]any); ok {
			out = append(out, asMap)
		}
	}
	return out
}

// mergeEntries keeps every existing entry's unknown keys and replaces the
// values of the keys the client owns, matched by id.
func mergeEntries(existing any, incoming []any, key string) []any {
	old := map[string]map[string]any{}
	if list, ok := existing.([]any); ok {
		for _, entry := range list {
			if asMap, ok := entry.(map[string]any); ok {
				if id, ok := asMap[key].(string); ok {
					old[id] = asMap
				}
			}
		}
	}
	merged := make([]any, 0, len(incoming))
	for _, entry := range incoming {
		asMap, ok := entry.(map[string]any)
		if !ok {
			continue
		}
		if id, ok := asMap[key].(string); ok {
			if previous, found := old[id]; found {
				combined := make(map[string]any, len(previous)+len(asMap))
				for field, value := range previous {
					combined[field] = value
				}
				for field, value := range asMap {
					combined[field] = value
				}
				merged = append(merged, combined)
				continue
			}
		}
		merged = append(merged, asMap)
	}
	return merged
}

func transcriptBlocksToMaps(blocks []model.TranscriptBlock) []any {
	result := make([]any, 0, len(blocks))
	for _, block := range blocks {
		entry := map[string]any{}
		if len(block.Raw) > 0 {
			if json.Unmarshal(block.Raw, &entry) != nil {
				continue
			}
		}
		entry["after_message"] = block.AfterMessage
		if block.TurnID == nil {
			delete(entry, "turn_id")
		} else {
			entry["turn_id"] = *block.TurnID
		}
		if block.ContentKind != "" && block.ContentKind != "activities" && block.ContentKind != "reasoning" {
			result = append(result, entry)
			continue
		}
		var oldActivities any
		if content, ok := entry["content"].(map[string]any); ok && content["kind"] == "activities" {
			oldActivities = content["data"]
		}
		activities := make([]any, 0, len(block.Activities))
		for _, activity := range block.Activities {
			encoded, err := marshalJSON(activity)
			if err == nil {
				activities = append(activities, encoded)
			}
		}
		entry["content"] = map[string]any{"kind": "activities", "data": mergeEntries(oldActivities, activities, "id")}
		result = append(result, entry)
	}
	return result
}

func setOptional(raw map[string]any, key string, value *string) {
	if value == nil {
		delete(raw, key)
		return
	}
	raw[key] = *value
}

func marshalJSON(value any) (any, error) {
	encoded, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}
	var out any
	if err := json.Unmarshal(encoded, &out); err != nil {
		return nil, err
	}
	return out, nil
}

func (a *app) projectForID(id string) *model.Project {
	for index := range a.projects {
		if a.projects[index].ID == id {
			return &a.projects[index]
		}
	}
	return nil
}

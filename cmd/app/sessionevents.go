package main

import (
	"encoding/json"
	"strings"
	"time"
	"unicode/utf8"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

const (
	backgroundEntryLimit = 200
	backgroundRunLimit   = 32 * 1024
	backgroundTailLimit  = 16 * 1024
)

func (a *app) extension(session *model.Session) map[string]any {
	state := a.stateForSession(session)
	if state.extension == nil {
		state.extension = make(map[string]any)
	}
	return state.extension
}

func (a *app) rawSession(session *model.Session) map[string]any {
	state := a.stateForSession(session)
	if state.raw == nil {
		if a.detail == session && a.detailRaw != nil {
			state.raw = a.detailRaw
		} else {
			state.raw = map[string]any{"id": session.ID}
		}
	}
	if a.detail == session {
		a.detailRaw = state.raw
	}
	return state.raw
}

func (a *app) applyExtendedSessionEvent(session *model.Session, event daemon.Event) bool {
	switch event.Kind {
	case "scheduledTurnAdmitted":
		return a.applyScheduledTurnAdmission(session, event.Payload)
	case "turnTranscriptReconciled":
		return a.applyTurnTranscriptReconciled(session, event.Payload)
	case "backgroundWork":
		return a.applyBackgroundWorkEvent(session, event.Payload)
	case "contextCompositionUpdated":
		return a.applyContextComposition(session, event.Payload)
	case "planUsageUpdated":
		// The desktop retired the plan meters and ignores the payload
		// (src/app/streaming.rs:1180); the event stays acknowledged for
		// protocol stability.
		return true
	case "goalUpdated":
		return a.applyGoalUpdated(session, event.Payload)
	case "goalCompleted":
		var message model.Message
		if json.Unmarshal(event.Payload, &message) != nil || message.ID == "" || message.Role != "goal_complete" {
			return false
		}
		for _, existing := range session.Messages {
			if existing.ID == message.ID {
				return true
			}
		}
		session.Messages = append(session.Messages, message)
		return true
	case "goalStatusReported":
		var report struct {
			OK  json.RawMessage `json:"Ok"`
			Err string          `json:"Err"`
		}
		if json.Unmarshal(event.Payload, &report) != nil {
			return false
		}
		if report.Err != "" && a.detail == session {
			a.status = tr("Goal command failed: ") + report.Err
		} else if len(report.OK) > 0 {
			if !a.applyGoalUpdated(session, report.OK) {
				return false
			}
			if string(report.OK) == "null" && a.detail == session {
				a.status = tr("No goal")
			}
		}
		return true
	case "goalTurnStarted":
		var started struct {
			Turn    model.AgentTurn `json:"turn"`
			Message *model.Message  `json:"message"`
		}
		if json.Unmarshal(event.Payload, &started) != nil || started.Turn.ID == "" {
			return false
		}
		if session.TurnByID(started.Turn.ID) == nil {
			session.Turns = append(session.Turns, started.Turn)
			if started.Message != nil {
				session.Messages = append(session.Messages, *started.Message)
			}
		}
		return true
	case "goalCommandFailed":
		var message string
		if json.Unmarshal(event.Payload, &message) != nil {
			return false
		}
		if a.detail == session {
			a.status = tr("Goal command failed: ") + message
		}
		return true
	case "submissionUpdated":
		return a.applySubmissionUpdated(session, event.Payload)
	case "availableCommands":
		return a.applyAvailableCommands(session, event.Payload)
	case "firstTokenMeasured":
		return a.applyFirstTokenMeasured(session, event.Payload)
	case "providerRetry":
		return a.applyProviderRetry(session, event.Payload)
	case "steerAccepted":
		return a.applySteerAccepted(session, event.Payload)
	case "steerRejected":
		return a.applySteerRejected(session, event.Payload)
	}
	return false
}

// applyTurnTranscriptReconciled replaces the active turn's projection with
// the provider's validated window (model.rs reconcile_active_transcript, via
// src/app/streaming.rs:819). A refusal is reported as a status failure, the
// way the desktop records a session error, and never silently acknowledged.
func (a *app) applyTurnTranscriptReconciled(session *model.Session, payload json.RawMessage) bool {
	var wire struct {
		Transcript   model.NativeTranscript `json:"transcript"`
		Continuation bool                   `json:"continuation"`
	}
	if json.Unmarshal(payload, &wire) != nil {
		a.status = tr("Cannot decode the reconciled provider transcript")
		return false
	}
	if !a.reconcileActiveTranscript(session, wire.Transcript, wire.Continuation) {
		a.status = tr("The provider transcript could not replace this turn safely")
		a.stateForSession(session).reconciliationFailed = true
		return true
	}
	a.stateForSession(session).reconciliationFailed = false
	return true
}

func (a *app) reconcileActiveTranscript(session *model.Session, transcript model.NativeTranscript, continuation bool) bool {
	turnID := session.ActiveTurnID()
	if turnID == "" {
		return false
	}
	messageStart := len(session.Messages)
	for index := range session.Messages {
		if session.Messages[index].TurnID != nil && *session.Messages[index].TurnID == turnID {
			messageStart = index
			break
		}
	}
	blockStart := len(session.TranscriptBlocks)
	for index := range session.TranscriptBlocks {
		if session.TranscriptBlocks[index].TurnID != nil && *session.TranscriptBlocks[index].TurnID == turnID {
			blockStart = index
			break
		}
	}
	if continuation {
		start := a.stateForSession(session).continuationStart
		if start == nil || start.messages > len(session.Messages) || start.blocks > len(session.TranscriptBlocks) {
			return false
		}
		messageStart, blockStart = start.messages, start.blocks
	}
	for _, message := range session.Messages[messageStart:] {
		if message.TurnID != nil && *message.TurnID != turnID {
			return false
		}
	}
	for _, block := range transcript.Blocks {
		if block.AfterMessage < 0 || block.AfterMessage > len(transcript.Messages) {
			return false
		}
	}
	// Presentation metadata is client-owned. Native user rows decide the
	// positions; a match restores the local stable id, typed text, display
	// content and attachments over the native row.
	users := make([]model.Message, 0, 2)
	var goalMessages []model.Message
	firstUserID := ""
	for _, message := range session.Messages {
		if message.Role == "user" && message.TurnID != nil && *message.TurnID == turnID {
			firstUserID = message.ID
			break
		}
	}
	for _, message := range session.Messages[messageStart:] {
		if message.Role == "user" {
			users = append(users, message)
		} else if message.Role == "goal_complete" {
			goalMessages = append(goalMessages, message)
		}
	}
	for index := range transcript.Messages {
		message := &transcript.Messages[index]
		bound := turnID
		message.TurnID = &bound
		message.Streaming = false
		if message.Role != "user" {
			continue
		}
		for local := range users {
			expected := users[local].Content
			if turn := session.TurnByID(turnID); users[local].ID == firstUserID && turn != nil && turn.ProviderPrompt != nil {
				expected = *turn.ProviderPrompt
			}
			if strings.TrimSpace(expected) == strings.TrimSpace(message.Content) {
				message.ID = users[local].ID
				message.Content = users[local].Content
				message.DisplayContent = users[local].DisplayContent
				message.Attachments = users[local].Attachments
				users = append(users[:local], users[local+1:]...)
				break
			}
		}
	}
	// An incomplete native window must never discard local inputs.
	if len(users) > 0 {
		return false
	}
	for index := range transcript.Blocks {
		bound := turnID
		transcript.Blocks[index].TurnID = &bound
		transcript.Blocks[index].AfterMessage += messageStart
	}
	session.Messages = append(session.Messages[:messageStart], transcript.Messages...)
	session.Messages = append(session.Messages, goalMessages...)
	session.TranscriptBlocks = append(session.TranscriptBlocks[:blockStart], transcript.Blocks...)
	session.UpdatedAt = uint64(time.Now().Unix())
	// The old part indexes point into the replaced suffix.
	state := a.stateForSession(session)
	state.textParts = nil
	state.reasoningParts = nil
	return true
}

// applyBackgroundWorkEvent folds background work into a bounded, ordered
// projection. The desktop keeps a full registry (src/app/background_work.rs)
// this client does not render yet; the fold preserves arrival order at run
// granularity — consecutive deltas of one stream accumulate into a run and
// activities and snapshots land in order — so subagent tools and their body
// text stay attributed in provider order.
func (a *app) applyBackgroundWorkEvent(session *model.Session, payload json.RawMessage) bool {
	var decoded map[string]any
	if json.Unmarshal(payload, &decoded) != nil {
		return false
	}
	// The capsule reads its items and the open surface's transcript from
	// this fold; mark both stale instead of re-decoding per frame.
	a.capsule.ItemsValid = false
	if session == a.detail && a.capsule.AgentKey != "" {
		a.capsule.AgentDirty = true
	}
	ext := a.extension(session)
	switch kind, _ := decoded["type"].(string); kind {
	case "upsert":
		items := backgroundItems(ext)
		if key := backgroundKey(decoded["key"]); key != "" {
			items[key] = decoded
		}
	case "reconcileProcesses", "reconcileLive":
		items := map[string]any{}
		if list, ok := decoded["items"].([]any); ok {
			for _, entry := range list {
				item, _ := entry.(map[string]any)
				if key := backgroundKey(item["key"]); key != "" {
					items[key] = item
				}
			}
		}
		ext["backgroundItems"] = items
	case "outputDelta":
		// Deltas for a key the provider never upserted are dropped, the way
		// the desktop's append_output does.
		key := backgroundKey(decoded["key"])
		item, _ := backgroundItems(ext)[key].(map[string]any)
		if item == nil {
			return true
		}
		delta, _ := decoded["delta"].(string)
		if delta == "" {
			return true
		}
		output, _ := item["output"].(string)
		if tail := backgroundTail(output+delta, backgroundTailLimit); len(tail) != len(output)+len(delta) {
			item["output"] = tail
			item["outputTruncated"] = true
		} else {
			item["output"] = tail
		}
	case "transcript":
		a.applyBackgroundWorkTranscript(ext, decoded)
	case "stopRequested":
		ext["backgroundStopRequested"] = decoded
	case "stopFailed":
		ext["backgroundStopFailed"] = decoded
	default:
		return false
	}
	return true
}

// applyBackgroundWorkTranscript folds one nested transcript event (externally
// tagged inside the internally tagged backgroundWork payload, per the wire
// round-trip test in driver_wire.rs).
func (a *app) applyBackgroundWorkTranscript(ext map[string]any, decoded map[string]any) {
	var name string
	var body map[string]any
	for _, candidate := range []string{"started", "textDelta", "reasoningDelta", "activity", "snapshot", "finished"} {
		if value, ok := decoded[candidate].(map[string]any); ok {
			name, body = candidate, value
			break
		}
	}
	if name == "" {
		return
	}
	key := backgroundKey(body["key"])
	if key == "" {
		return
	}
	transcripts := backgroundTranscripts(ext)
	fold, _ := transcripts[key].(map[string]any)
	if fold == nil {
		fold = map[string]any{}
		transcripts[key] = fold
	}
	switch name {
	case "started":
		fold["entries"] = []any{}
		delete(fold, "prompt")
		if prompt, ok := body["prompt"].(string); ok && prompt != "" {
			fold["prompt"] = prompt
		}
	case "textDelta", "reasoningDelta":
		if delta, ok := body["delta"].(string); ok && delta != "" {
			fold["entries"] = appendBackgroundRun(fold["entries"], name, delta)
		}
	case "activity":
		if activity, ok := body["activity"].(map[string]any); ok {
			fold["entries"] = appendBackgroundEntry(fold["entries"], map[string]any{"activity": activity})
		}
	case "snapshot":
		// An authoritative snapshot replaces the accumulated runs; its own
		// message order is preserved as delivered.
		if transcript, ok := body["transcript"].(map[string]any); ok {
			fold["entries"] = []any{map[string]any{"snapshot": transcript}}
		}
	case "finished":
		if success, ok := body["success"].(bool); ok {
			fold["success"] = success
		}
	}
	fold["entries"] = capBackgroundEntries(fold["entries"])
}

// applySubmissionUpdated stores the receipt under the extension store and
// ports the failure effects: a NotSent or Rejected receipt for the last turn
// settles that turn as failed and the session idle (src/app/streaming.rs:698).
func (a *app) applySubmissionUpdated(session *model.Session, payload json.RawMessage) bool {
	var receipt submissionReceipt
	if json.Unmarshal(payload, &receipt) != nil {
		return false
	}
	ext := a.extension(session)
	submissions, _ := ext["submissions"].(map[string]any)
	if submissions == nil {
		submissions = map[string]any{}
		ext["submissions"] = submissions
	}
	// The desktop stores a receipt only when its turn exists; orphan
	// receipts are dropped, and the failure branch below checks the last
	// turn independently.
	if session.TurnByID(receipt.ID) != nil {
		if previous, ok := submissions[receipt.ID].(map[string]any); ok {
			var prior submissionReceipt
			if encoded, err := json.Marshal(previous); err == nil && json.Unmarshal(encoded, &prior) == nil {
				if !receipt.supersedes(&prior) {
					return true
				}
			}
		}
		var decoded any
		if json.Unmarshal(payload, &decoded) != nil {
			return false
		}
		submissions[receipt.ID] = decoded
		session.TurnByID(receipt.ID).Submission = append(json.RawMessage(nil), payload...)
	}
	if receipt.State == "notSent" || receipt.State == "rejected" {
		if turns := session.Turns; len(turns) > 0 {
			turn := &turns[len(turns)-1]
			if turn.ID == receipt.ID && !(receipt.State == "notSent" && turn.Status == "interrupted") {
				turn.Status = "failed"
				completed := uint64(time.Now().Unix())
				turn.CompletedAt = &completed
				session.Status = "idle"
			}
		}
	}
	return true
}

// submissionReceipt mirrors submission.rs SubmissionReceipt: no rename
// attribute, so the wire keys are snake_case while the state values are
// camelCase.
type submissionReceipt struct {
	ID              string  `json:"id"`
	InputID         string  `json:"input_id"`
	State           string  `json:"state"`
	CreatedAt       uint64  `json:"created_at"`
	UpdatedAt       uint64  `json:"updated_at"`
	NativeSessionID *string `json:"native_session_id"`
}

// supersedes ports submission.rs supersedes: same turn and input, then a
// higher state rank, or the same rank with a newer updated_at that does not
// lose a known native session id.
func (r *submissionReceipt) supersedes(previous *submissionReceipt) bool {
	if r.ID != previous.ID || r.InputID != previous.InputID {
		return false
	}
	rank := func(state string) int {
		switch state {
		case "preparing":
			return 0
		case "dispatching":
			return 1
		case "unknown":
			return 2
		}
		return 3
	}
	next, prior := rank(r.State), rank(previous.State)
	if next != prior {
		return next > prior
	}
	if r.UpdatedAt < previous.UpdatedAt {
		return false
	}
	return r.NativeSessionID != nil || previous.NativeSessionID == nil
}

// applyAvailableCommands stores the authoritative slash-command list
// (model.rs ReportedCommand accepts a plain name or a name/description pair).
func (a *app) applyAvailableCommands(session *model.Session, payload json.RawMessage) bool {
	var list []json.RawMessage
	if json.Unmarshal(payload, &list) != nil {
		return false
	}
	commands := make([]any, 0, len(list))
	for _, raw := range list {
		var name string
		if json.Unmarshal(raw, &name) == nil {
			commands = append(commands, map[string]any{"name": name, "description": ""})
			continue
		}
		var entry map[string]any
		if json.Unmarshal(raw, &entry) == nil && entry != nil {
			commands = append(commands, entry)
		}
	}
	a.extension(session)["availableCommands"] = commands
	a.rawSession(session)["available_commands"] = commands
	return true
}

// applyFirstTokenMeasured adds the plugin's server-side latency sample to the
// session usage, once per turn (src/app/streaming.rs add_first_token_sample).
func (a *app) applyFirstTokenMeasured(session *model.Session, payload json.RawMessage) bool {
	var wire struct {
		LatencyMs uint64 `json:"latencyMs"`
	}
	if json.Unmarshal(payload, &wire) != nil {
		return false
	}
	turnID := session.ActiveTurnID()
	if turnID == "" {
		return true
	}
	ext := a.extension(session)
	if sameTurn, _ := ext["firstTokenTurn"].(string); sameTurn == turnID {
		return true
	}
	ext["firstTokenTurn"] = turnID
	if session.ContextUsage == nil {
		session.ContextUsage = &model.ContextUsage{}
	}
	latency := wire.LatencyMs
	if session.ContextUsage.FirstTokenMs != nil {
		previous := *session.ContextUsage.FirstTokenMs
		if latency > ^uint64(0)-previous {
			latency = ^uint64(0)
		} else {
			latency += previous
		}
	}
	session.ContextUsage.FirstTokenMs = &latency
	turns := uint64(1)
	if session.ContextUsage.FirstTokenTurns != nil {
		previous := *session.ContextUsage.FirstTokenTurns
		if previous == ^uint64(0) {
			turns = previous
		} else {
			turns += previous
		}
	}
	session.ContextUsage.FirstTokenTurns = &turns
	return true
}

func (a *app) applyContextComposition(session *model.Session, payload json.RawMessage) bool {
	var composition model.ContextComposition
	if json.Unmarshal(payload, &composition) != nil {
		return false
	}
	if session.Model != nil && *session.Model != composition.Model {
		return true
	}
	var decoded any
	if json.Unmarshal(payload, &decoded) != nil {
		return false
	}
	a.extension(session)["contextComposition"] = decoded
	if session.ContextUsage == nil {
		session.ContextUsage = &model.ContextUsage{}
	}
	session.ContextUsage.Composition = &composition
	return true
}

// applyGoalUpdated stores the goal snapshot under revision gating
// (src/app/streaming.rs:1248): a newer revision or a different objective
// session wins.
func (a *app) applyGoalUpdated(session *model.Session, payload json.RawMessage) bool {
	var decoded map[string]any
	if json.Unmarshal(payload, &decoded) != nil {
		return false
	}
	if decoded == nil {
		a.extension(session)["goal"] = nil
		a.rawSession(session)["goal"] = nil
		return true
	}
	revision := goalRevision(decoded["revision"])
	sessionID, _ := decoded["sessionId"].(string)
	if previous, ok := a.rawSession(session)["goal"].(map[string]any); ok {
		oldRevision := goalRevision(previous["revision"])
		oldSession, _ := previous["sessionId"].(string)
		if oldSession == sessionID && oldRevision > revision {
			return true
		}
	}
	a.extension(session)["goal"] = decoded
	a.rawSession(session)["goal"] = decoded
	return true
}

// applyProviderRetry keeps the latest retry report with its receive time; the
// backoff card is not rendered yet. The status flip mirrors the main switch.
func (a *app) applyProviderRetry(session *model.Session, payload json.RawMessage) bool {
	var decoded any
	if json.Unmarshal(payload, &decoded) != nil {
		return false
	}
	entry := map[string]any{"receivedAtMs": time.Now().UnixMilli()}
	if report, ok := decoded.(map[string]any); ok {
		for key, value := range report {
			entry[key] = value
		}
	}
	a.extension(session)["providerRetry"] = entry
	session.Status = "waiting"
	return true
}

func (a *app) applySteerAccepted(session *model.Session, payload json.RawMessage) bool {
	var decoded map[string]any
	if json.Unmarshal(payload, &decoded) != nil {
		return false
	}
	message, _ := decoded["message"].(string)
	if message == "" {
		return false
	}
	entry := model.Message{
		ID:        uuid.NewString(),
		Role:      "user",
		Content:   message,
		CreatedAt: uint64(time.Now().Unix()),
	}
	if pending, ok := a.takePendingSteer(session, message); ok {
		if index := findQueuedMessage(session, pending.messageID); index >= 0 {
			session.QueuedMessages = append(session.QueuedMessages[:index], session.QueuedMessages[index+1:]...)
		}
		entry.ID = pending.messageID
		entry.Content = pending.prompt
		entry.DisplayContent = pending.display
		entry.Attachments = messageAttachmentsFromDraft(pending.attachments)
	}
	if turnID := session.ActiveTurnID(); turnID != "" {
		bound := turnID
		entry.TurnID = &bound
	}
	session.Messages = append(session.Messages, entry)
	session.UpdatedAt = uint64(time.Now().Unix())
	return true
}

// Only a confirmed rejection makes the durable input eligible for a new send.
func (a *app) applySteerRejected(session *model.Session, payload json.RawMessage) bool {
	var decoded map[string]any
	if json.Unmarshal(payload, &decoded) != nil {
		return false
	}
	message, _ := decoded["message"].(string)
	if message == "" {
		return false
	}
	pending, ok := a.takePendingSteer(session, message)
	if !ok {
		return true
	}
	a.extension(session)["steerRejected"] = decoded
	state := a.stateForSession(session)
	settled := !state.stopped && !session.Busy() && lastTurnSettledCleanly(session)
	a.requeuePendingSteer(session.ID, pending, settled)
	if a.detail == session {
		a.status = tr("The steer was not delivered")
		if reason, ok := decoded["reason"].(string); ok && reason != "" {
			a.status += tr(": ") + reason
		}
	}
	if settled {
		a.post(func() { a.drainQueuedSubmission(session) })
	}
	return true
}

func backgroundItems(ext map[string]any) map[string]any {
	items, _ := ext["backgroundItems"].(map[string]any)
	if items == nil {
		items = map[string]any{}
		ext["backgroundItems"] = items
	}
	return items
}

func backgroundTranscripts(ext map[string]any) map[string]any {
	transcripts, _ := ext["backgroundTranscripts"].(map[string]any)
	if transcripts == nil {
		transcripts = map[string]any{}
		ext["backgroundTranscripts"] = transcripts
	}
	return transcripts
}

// backgroundKey renders a BackgroundWorkKey (kind + providerId) as a stable
// map key.
func backgroundKey(value any) string {
	key, _ := value.(map[string]any)
	if key == nil {
		return ""
	}
	kind, _ := key["kind"].(string)
	provider, _ := key["providerId"].(string)
	if kind == "" || provider == "" {
		return ""
	}
	return kind + "/" + provider
}

// appendBackgroundRun merges a delta into the trailing run of the same
// stream, so interleaved text, reasoning and activities keep their order.
func appendBackgroundRun(entriesAny any, name, delta string) []any {
	entries, _ := entriesAny.([]any)
	kind := "text"
	if name == "reasoningDelta" {
		kind = "reasoning"
	}
	if len(entries) > 0 {
		if last, ok := entries[len(entries)-1].(map[string]any); ok {
			if run, ok := last[kind].(string); ok && len(run)+len(delta) <= backgroundRunLimit {
				last[kind] = run + delta
				return entries
			}
		}
	}
	return appendBackgroundEntry(entries, map[string]any{kind: delta})
}

func appendBackgroundEntry(entriesAny any, entry map[string]any) []any {
	entries, _ := entriesAny.([]any)
	return append(entries, entry)
}

// capBackgroundEntries bounds one transcript's history by dropping its oldest
// runs; the store is a projection, not the durable registry.
func capBackgroundEntries(entriesAny any) []any {
	entries, _ := entriesAny.([]any)
	if len(entries) <= backgroundEntryLimit {
		return entries
	}
	return append([]any(nil), entries[len(entries)-backgroundEntryLimit:]...)
}

// backgroundTail keeps the newest bytes of an output string without splitting
// a UTF-8 sequence.
func backgroundTail(text string, limit int) string {
	if len(text) <= limit {
		return text
	}
	cut := len(text) - limit
	for cut < len(text) && !utf8.RuneStart(text[cut]) {
		cut++
	}
	return text[cut:]
}

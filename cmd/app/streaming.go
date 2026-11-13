package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

func (a *app) applyEvent(event daemon.Event) error {
	if a.historyRejectsEvent(event) {
		return nil
	}
	if !json.Valid(event.Payload) {
		return fmt.Errorf("invalid payload for %s", event.Kind)
	}
	switch event.Kind {
	case "textDelta", "reasoningDelta":
		if _, ok := eventText(event.Payload, "delta"); !ok {
			return fmt.Errorf("missing text delta for %s", event.Kind)
		}
	case "textEnded", "reasoningEnded":
		if _, ok := completeText(event.Payload); !ok {
			return fmt.Errorf("missing final text for %s", event.Kind)
		}
	}
	session := a.eventSession(event.SessionID)
	if session == nil {
		return fmt.Errorf("cannot apply an event for an unloaded session")
	}
	state := a.stateForSession(session)
	if state.preparing {
		switch event.Kind {
		case "turnStarted", "turnFinished", "textStarted", "textDelta", "textEnded", "reasoningStarted", "reasoningDelta", "reasoningEnded", "activity", "richActivity", "permission", "userInputRequested", "turnTranscriptReconciled", "steerAccepted", "steerRejected":
			return nil
		}
	}
	switch event.Kind {
	case "textStarted", "textDelta", "textEnded", "reasoningStarted", "reasoningDelta", "reasoningEnded", "activity", "richActivity", "permission", "userInputRequested", "turnTranscriptReconciled", "turnFinished", "firstTokenMeasured":
		if session.ActiveTurnID() == "" || (session.Status != "working" && session.Status != "waiting" && session.Status != "connecting") {
			return nil
		}
	}
	switch event.Kind {
	case "textDelta", "reasoningDelta", "activity", "richActivity":
		if state.firstContentAt.IsZero() {
			state.firstContentAt = time.Now()
		}
	}
	if event.RuntimeID != "" {
		a.runtimes[event.SessionID] = event.RuntimeID
	}
	a.noteSessionEvent(event)
	if a.detail == session {
		state.permission = a.pendingPermission
		state.question = a.pendingUserInput
	}
	switch event.Kind {
	case "connected":
		if len(event.Payload) > 0 && string(event.Payload) != "null" {
			session.ProviderCursor = append(json.RawMessage(nil), event.Payload...)
			if id := nativeSessionID(session.ProviderCursor); id != "" {
				session.NativeSessionID = &id
			}
		}
	case "autoTitleUpdated":
		var title *string
		if err := json.Unmarshal(event.Payload, &title); err != nil {
			return err
		}
		session.AutoTitle = title
	case "turnStarted":
		delete(state.extension, "firstTokenTurn")
		state.turnStartedAt, state.firstContentAt = time.Now(), time.Time{}
		state.reconciliationFailed = false
		state.continuationStart = &transcriptPosition{messages: len(session.Messages), blocks: len(session.TranscriptBlocks)}
		state.textParts, state.reasoningParts = nil, nil
		if len(session.Turns) > 0 {
			turn := &session.Turns[len(session.Turns)-1]
			if turn.Status == "interrupted" {
				return nil
			}
			turn.Status, turn.CompletedAt, turn.ProviderTurnStarted = "running", nil, true
		}
		session.Status = "working"
		session.UpdatedAt = uint64(time.Now().Unix())
		a.ensureRunningTurn(session)
	case "textStarted":
		a.messageForPart(session, payloadString(event.Payload, "part"))
	case "textDelta":
		message := a.messageForPart(session, payloadString(event.Payload, "part"))
		message.Content += textDelta(event.Payload)
		message.Streaming = true
	case "textEnded":
		message := a.messageForPart(session, payloadString(event.Payload, "part"))
		if text, present := completeText(event.Payload); present {
			message.Content = text
		}
		message.Streaming = false
	case "reasoningStarted":
		activity := a.reasoningForPart(session, payloadString(event.Payload, "part"))
		activity.Complete = false
		activity.Reasoning = &model.ReasoningBlock{StartedAtMs: uint64(time.Now().UnixMilli())}
	case "reasoningDelta":
		activity := a.reasoningForPart(session, payloadString(event.Payload, "part"))
		if activity.Reasoning == nil {
			activity.Reasoning = &model.ReasoningBlock{}
		}
		activity.Reasoning.Content += textDelta(event.Payload)
		activity.Complete = false
	case "reasoningEnded":
		activity := a.reasoningForPart(session, payloadString(event.Payload, "part"))
		if activity.Reasoning == nil {
			activity.Reasoning = &model.ReasoningBlock{}
		}
		if text, present := completeText(event.Payload); present {
			activity.Reasoning.Content = text
		}
		if activity.Reasoning.FinishedAtMs == 0 {
			activity.Reasoning.FinishedAtMs = uint64(time.Now().UnixMilli())
		}
		activity.Complete = true
	case "activity":
		var payload streamActivityPayload
		if json.Unmarshal(event.Payload, &payload) != nil {
			return fmt.Errorf("cannot decode tool activity")
		}
		block := a.ensureActivityBlock(session)
		var item *model.ActivityItem
		for index := range block.Activities {
			if payload.ID != "" && block.Activities[index].ID == payload.ID {
				item = &block.Activities[index]
				break
			}
		}
		if item == nil {
			id := payload.ID
			if id == "" {
				id = uuid.NewString()
			}
			block.Activities = append(block.Activities, model.ActivityItem{ID: id, Kind: payload.Kind})
			item = &block.Activities[len(block.Activities)-1]
		}
		item.Title, item.Detail, item.Complete = payload.Title, payload.Detail, payload.Complete
	case "richActivity":
		var activity model.ActivityItem
		if err := json.Unmarshal(event.Payload, &activity); err != nil {
			return fmt.Errorf("cannot decode a tool activity: %w", err)
		}
		if activity.ID == "" {
			activity.ID = uuid.NewString()
		}
		updated := false
		for blockIndex := range session.TranscriptBlocks {
			block := &session.TranscriptBlocks[blockIndex]
			for index := range block.Activities {
				if block.Activities[index].ID == activity.ID {
					block.Activities[index] = activity
					updated = true
					break
				}
			}
			if updated {
				break
			}
		}
		if !updated {
			block := a.ensureActivityBlock(session)
			block.Activities = append(block.Activities, activity)
		}
	case "compactionUpdated":
		var compaction model.CompactionState
		if err := json.Unmarshal(event.Payload, &compaction); err != nil {
			return err
		}
		session.Compaction = &compaction
		state.contextCompact = nil
		if compaction.Status == "completed" && compaction.Summary != nil {
			summary := strings.TrimSpace(*compaction.Summary)
			present := summary == ""
			for _, message := range session.Messages {
				if message.Role == "compaction" && strings.TrimSpace(message.Content) == summary {
					present = true
					break
				}
			}
			if !present {
				session.Messages = append(session.Messages, model.Message{ID: uuid.NewString(), Role: "compaction", Content: summary, CreatedAt: uint64(time.Now().Unix())})
			}
		}
		if compaction.Status == "failed" && compaction.Error != nil && a.detail == session {
			a.status = *compaction.Error
		}
	case "turnStatsUpdated":
		if turn := a.currentTurn(session); turn != nil {
			var stats model.TurnStats
			if err := json.Unmarshal(event.Payload, &stats); err != nil {
				return err
			}
			turn.Stats = &stats
		}
	case "usageUpdated":
		if err := a.applyUsageEvent(session, event.Payload); err != nil {
			return err
		}
		a.maybeAutoCompact(session)
	case "agentPresetSelected":
		var preset *string
		if err := json.Unmarshal(event.Payload, &preset); err != nil {
			return err
		}
		session.AgentPreset = preset
	case "providerBusy":
		session.Status = "waiting"
	case "turnFinished":
		var finish struct {
			Success *bool `json:"success"`
		}
		if json.Unmarshal(event.Payload, &finish) != nil || finish.Success == nil {
			return fmt.Errorf("invalid turn completion")
		}
		success := payloadBool(event.Payload, "success")
		if state.reconciliationFailed {
			success = false
		}
		if !state.turnStartedAt.IsZero() && !state.firstContentAt.IsZero() {
			latency := max(state.firstContentAt.Sub(state.turnStartedAt).Milliseconds(), 0)
			payload, _ := json.Marshal(map[string]int64{"latencyMs": latency})
			a.applyFirstTokenMeasured(session, payload)
		}
		session.UpdatedAt = uint64(time.Now().Unix())
		state.permission = nil
		state.question = nil
		if success {
			session.Status = "idle"
		} else {
			session.Status = "failed"
		}
		if turn := a.currentTurn(session); turn != nil {
			turn.Status = "completed"
			if !success {
				turn.Status = "failed"
			}
			completed := uint64(time.Now().Unix())
			turn.CompletedAt = &completed
		}
	case "error":
		text := payloadString(event.Payload, "message")
		if text == "" {
			text = string(event.Payload)
		}
		session.Messages = append(session.Messages, model.Message{
			ID:        uuid.NewString(),
			Role:      "system",
			Content:   text,
			CreatedAt: uint64(time.Now().Unix()),
		})
		a.rebuildTranscript()
	case "processExited":
		session.Status = "idle"
		state.permission = nil
		state.question = nil
	case "permission":
		var payload struct {
			RequestID string             `json:"requestId"`
			Title     string             `json:"title"`
			Detail    string             `json:"detail"`
			Options   []permissionOption `json:"options"`
		}
		if json.Unmarshal(event.Payload, &payload) != nil {
			return fmt.Errorf("cannot decode a permission request")
		}
		if payload.RequestID == "" || len(payload.Options) == 0 {
			return fmt.Errorf("incomplete permission request")
		}
		state.permission = &pendingPermission{
			RequestID: payload.RequestID,
			Title:     payload.Title,
			Detail:    payload.Detail,
			Options:   payload.Options,
		}
		session.Status = "waiting"
	case "userInputRequested":
		var payload struct {
			RequestID string              `json:"requestId"`
			Questions []userInputQuestion `json:"questions"`
		}
		if json.Unmarshal(event.Payload, &payload) != nil {
			return fmt.Errorf("cannot decode a question request")
		}
		if payload.RequestID == "" || len(payload.Questions) == 0 {
			return fmt.Errorf("incomplete question request")
		}
		state.question = &userInputState{
			RequestID:  payload.RequestID,
			Questions:  payload.Questions,
			Selections: map[int][]string{},
			Custom:     make([]string, len(payload.Questions)),
		}
		session.Status = "waiting"
	case "nativeSessionsChanged", "nativeSessionMoved":
		a.refreshCatalog()
	default:
		if !a.applyExtendedSessionEvent(session, event) {
			return fmt.Errorf("unsupported or invalid driver event: %s", event.Kind)
		}
	}
	if a.detail == session {
		a.pendingPermission = state.permission
		a.pendingUserInput = state.question
		a.rebuildTranscript()
	}
	a.syncSummary(session)
	a.rebuildRows()
	return nil
}

func (a *app) messageForPart(session *model.Session, part string) *model.Message {
	if part == "" {
		return a.ensureAssistantMessage(session)
	}
	state := a.stateForSession(session)
	if state.textParts == nil {
		state.textParts = make(map[string]int)
	}
	if index, ok := state.textParts[part]; ok && index >= 0 && index < len(session.Messages) {
		return &session.Messages[index]
	}
	message := model.Message{ID: uuid.NewString(), Role: "assistant", CreatedAt: uint64(time.Now().Unix()), Streaming: true}
	if turn := a.currentTurn(session); turn != nil {
		id := turn.ID
		message.TurnID = &id
	}
	session.Messages = append(session.Messages, message)
	state.textParts[part] = len(session.Messages) - 1
	a.rebuildTranscript()
	return &session.Messages[len(session.Messages)-1]
}

func (a *app) reasoningForPart(session *model.Session, part string) *model.ActivityItem {
	if part == "" {
		return a.ensureReasoningActivity(session)
	}
	state := a.stateForSession(session)
	if state.reasoningParts == nil {
		state.reasoningParts = make(map[string]activityPosition)
	}
	if position, ok := state.reasoningParts[part]; ok && position.block < len(session.TranscriptBlocks) && position.item < len(session.TranscriptBlocks[position.block].Activities) {
		return &session.TranscriptBlocks[position.block].Activities[position.item]
	}
	for blockIndex := range session.TranscriptBlocks {
		activities := session.TranscriptBlocks[blockIndex].Activities
		for itemIndex := range activities {
			activity := &session.TranscriptBlocks[blockIndex].Activities[itemIndex]
			if activity.Kind == model.KindReasoning && activity.SourceID != nil && *activity.SourceID == part {
				state.reasoningParts[part] = activityPosition{block: blockIndex, item: itemIndex}
				return activity
			}
		}
	}
	block := a.ensureActivityBlock(session)
	activity := model.ActivityItem{ID: uuid.NewString(), SourceID: &part, Kind: model.KindReasoning, Title: "Reasoning",
		Reasoning: &model.ReasoningBlock{StartedAtMs: uint64(time.Now().UnixMilli())}}
	block.Activities = append(block.Activities, activity)
	for index := range session.TranscriptBlocks {
		if &session.TranscriptBlocks[index] == block {
			state.reasoningParts[part] = activityPosition{block: index, item: len(block.Activities) - 1}
			break
		}
	}
	return &block.Activities[len(block.Activities)-1]
}

func (a *app) applyUsageEvent(session *model.Session, payload json.RawMessage) error {
	var usage struct {
		Tokens       *uint64 `json:"contextTokens"`
		Window       *uint64 `json:"contextWindow"`
		Resolved     bool    `json:"contextWindowResolved"`
		Total        *uint64 `json:"sessionTotal"`
		CacheRead    *uint64 `json:"cacheRead"`
		PromptTokens *uint64 `json:"promptTokens"`
	}
	if err := json.Unmarshal(payload, &usage); err != nil {
		return err
	}
	if session.ContextUsage == nil {
		session.ContextUsage = &model.ContextUsage{}
	}
	current := session.ContextUsage
	if usage.Tokens != nil {
		current.Tokens, current.Measured = *usage.Tokens, true
	}
	if usage.Resolved {
		current.Window = usage.Window
	}
	if usage.Total != nil {
		current.TotalTokens = usage.Total
	}
	if usage.CacheRead != nil {
		current.CacheRead = usage.CacheRead
	}
	if usage.PromptTokens != nil {
		current.PromptTokens = usage.PromptTokens
	}
	return nil
}

type streamActivityPayload struct {
	ID       string  `json:"id"`
	Kind     string  `json:"kind"`
	Title    string  `json:"title"`
	Detail   *string `json:"detail"`
	Complete bool    `json:"complete"`
}

func completeText(payload json.RawMessage) (string, bool) {
	return eventText(payload, "text")
}

func eventText(payload json.RawMessage, field string) (string, bool) {
	var text *string
	if json.Unmarshal(payload, &text) == nil && text != nil {
		return *text, true
	}
	var holder map[string]*json.RawMessage
	if json.Unmarshal(payload, &holder) != nil || holder[field] == nil {
		return "", false
	}
	text = nil
	if json.Unmarshal(*holder[field], &text) != nil || text == nil {
		return "", false
	}
	return *text, true
}

func (a *app) maybeSaveStreaming(session *model.Session) {
	if session == nil || a.client == nil {
		return
	}
	state := a.stateForSession(session)
	if time.Since(state.lastSnapshot) < time.Second {
		return
	}
	state.lastSnapshot = time.Now()
	a.saveSessionSnapshot(session)
}

func (a *app) ensureRunningTurn(session *model.Session) *model.AgentTurn {
	for index := len(session.Turns) - 1; index >= 0; index-- {
		if session.Turns[index].Status == "running" {
			return &session.Turns[index]
		}
	}
	turn := model.AgentTurn{
		ID:        uuid.NewString(),
		TurnCount: len(session.Turns) + 1,
		Status:    "running",
		StartedAt: uint64(time.Now().Unix()),
	}
	session.Turns = append(session.Turns, turn)
	return &session.Turns[len(session.Turns)-1]
}

func (a *app) currentTurn(session *model.Session) *model.AgentTurn {
	for index := len(session.Turns) - 1; index >= 0; index-- {
		if session.Turns[index].Status == "running" {
			return &session.Turns[index]
		}
	}
	return nil
}

func (a *app) ensureAssistantMessage(session *model.Session) *model.Message {
	for index := len(session.Messages) - 1; index >= 0; index-- {
		message := &session.Messages[index]
		if message.Role == "user" {
			break
		}
		if message.Role == "assistant" && message.Streaming {
			return message
		}
	}
	turn := a.currentTurn(session)
	message := model.Message{
		ID:        uuid.NewString(),
		Role:      "assistant",
		CreatedAt: uint64(time.Now().Unix()),
		Streaming: true,
	}
	if turn != nil {
		message.TurnID = &turn.ID
	}
	session.Messages = append(session.Messages, message)
	a.rebuildTranscript()
	return &session.Messages[len(session.Messages)-1]
}

func (a *app) ensureActivityBlock(session *model.Session) *model.TranscriptBlock {
	turn := a.currentTurn(session)
	turnID := ""
	if turn != nil {
		turnID = turn.ID
	}
	for index := len(session.TranscriptBlocks) - 1; index >= 0; index-- {
		block := &session.TranscriptBlocks[index]
		if (block.ContentKind == "" || block.ContentKind == "activities") && block.TurnID != nil && *block.TurnID == turnID && block.AfterMessage == len(session.Messages) {
			return block
		}
	}
	block := model.TranscriptBlock{AfterMessage: len(session.Messages)}
	if turnID != "" {
		block.TurnID = &turnID
	}
	session.TranscriptBlocks = append(session.TranscriptBlocks, block)
	a.rebuildTranscript()
	return &session.TranscriptBlocks[len(session.TranscriptBlocks)-1]
}

func (a *app) ensureReasoningActivity(session *model.Session) *model.ActivityItem {
	block := a.ensureActivityBlock(session)
	for index := len(block.Activities) - 1; index >= 0; index-- {
		activity := &block.Activities[index]
		if activity.Kind == model.KindReasoning && !activity.Complete {
			return activity
		}
	}
	block.Activities = append(block.Activities, model.ActivityItem{
		ID:       uuid.NewString(),
		Kind:     model.KindReasoning,
		Title:    "Reasoning",
		Complete: false,
		Reasoning: &model.ReasoningBlock{
			StartedAtMs: uint64(time.Now().UnixMilli()),
		},
	})
	return &block.Activities[len(block.Activities)-1]
}

func textDelta(payload json.RawMessage) string {
	var direct string
	if err := json.Unmarshal(payload, &direct); err == nil {
		return direct
	}
	return payloadString(payload, "delta")
}

func payloadString(payload json.RawMessage, key string) string {
	var value struct {
		Text  string `json:"text"`
		Delta string `json:"delta"`
		Msg   string `json:"message"`
		Part  string `json:"part"`
	}
	_ = json.Unmarshal(payload, &value)
	switch key {
	case "text":
		return value.Text
	case "delta":
		return value.Delta
	case "part":
		return value.Part
	case "message":
		if value.Msg != "" {
			return value.Msg
		}
		var direct string
		if err := json.Unmarshal(payload, &direct); err == nil {
			return direct
		}
	}
	return ""
}

func payloadBool(payload json.RawMessage, key string) bool {
	var value map[string]json.RawMessage
	if json.Unmarshal(payload, &value) != nil {
		return false
	}
	raw, ok := value[key]
	if !ok {
		return false
	}
	var flag bool
	if json.Unmarshal(raw, &flag) != nil {
		return false
	}
	return flag
}

// summarySession finds the task-list entry the sidebar and tabs read.
func (a *app) summarySession(id string) *model.Session {
	if id == "" {
		return nil
	}
	for index := range a.sessions {
		if a.sessions[index].ID == id {
			return &a.sessions[index]
		}
	}
	return nil
}

// sessionStatus is the status every row shows: the summary entry already
// carries live updates from noteSessionEvent, so this only guards nil.
func (a *app) sessionStatus(id string) string {
	if session := a.summarySession(id); session != nil {
		return session.Status
	}
	return "idle"
}

// noteSessionEvent records the coarse live state of one session, including
// background tabs whose transcript is not loaded.
func (a *app) noteSessionEvent(event daemon.Event) {
	switch event.Kind {
	case "turnStarted", "turnFinished", "processExited", "providerBusy":
		a.settings.backend.schedBoard.ready = false
	}
	session := a.summarySession(event.SessionID)
	if session == nil {
		return
	}
	switch event.Kind {
	case "turnStarted":
		session.Status = "working"
		session.UpdatedAt = uint64(time.Now().Unix())
		session.Draft = false
		delete(a.sessionAttention, event.SessionID)
	case "turnFinished":
		if payloadBool(event.Payload, "success") {
			session.Status = "idle"
		} else {
			session.Status = "failed"
		}
		session.UpdatedAt = uint64(time.Now().Unix())
		delete(a.sessionAttention, event.SessionID)
	case "processExited":
		session.Status = "idle"
		delete(a.sessionAttention, event.SessionID)
	case "permission":
		a.sessionAttention[event.SessionID] = "permission"
	case "userInputRequested":
		a.sessionAttention[event.SessionID] = "question"
	}
	switch event.Kind {
	case "textStarted", "textDelta", "textEnded",
		"reasoningStarted", "reasoningDelta", "reasoningEnded",
		"activity", "turnFinished", "error",
		"permission", "userInputRequested":
		if event.SessionID != a.selectedID && a.hasOpenTab(event.SessionID) {
			a.tabUnread[event.SessionID] = true
		}
	}
}

func (a *app) hasOpenTab(id string) bool {
	for _, open := range a.openTabs {
		if open == id {
			return true
		}
	}
	return false
}

func (a *app) submitDraft(text string) {
	a.submitSessionSubmission(sessionSubmission{sessionID: a.composerTargetSessionID(), text: text})
}

func (a *app) appendSystemError(sessionID, text string) {
	session := a.eventSession(sessionID)
	if session == nil {
		return
	}
	session.Status = "failed"
	a.stateForSession(session).preparing = false
	session.Messages = append(session.Messages, model.Message{
		ID:        uuid.NewString(),
		Role:      "system",
		Content:   text,
		CreatedAt: uint64(time.Now().Unix()),
	})
	a.syncSummary(session)
	a.rebuildRows()
	if a.detail == session {
		a.rebuildTranscript()
	}
}

type runtimeLaunch struct {
	mermaidVersion  string
	sessionID       string
	cwd             string
	model           string
	mode            string
	interaction     string
	reasoningEffort *string
	serviceTier     *string
	contextWindow   *string
	agentPreset     *string
	cursor          json.RawMessage
}

func newRuntimeLaunch(session *model.Session, cwd string) runtimeLaunch {
	modelID := ""
	if session.Model != nil {
		modelID = *session.Model
	}
	return runtimeLaunch{sessionID: session.ID, cwd: cwd, model: modelID,
		mode: session.RuntimeMode, interaction: session.InteractionMode,
		reasoningEffort: session.ReasoningEffort, serviceTier: session.ServiceTier,
		contextWindow: session.ContextWindow, agentPreset: session.AgentPreset,
		cursor: append(json.RawMessage(nil), session.ProviderCursor...)}
}

func ensureRuntime(ctx context.Context, client *daemon.Client, launch runtimeLaunch) (string, error) {
	payload, err := client.RequestFor(ctx, launch.sessionID, "", map[string]any{"type": "attachSession"})
	if err != nil {
		return "", err
	}
	var attached struct {
		RuntimeID *string `json:"runtimeId"`
	}
	if err := json.Unmarshal(payload, &attached); err != nil {
		return "", fmt.Errorf("decode attached runtime: %w", err)
	}
	if attached.RuntimeID != nil && *attached.RuntimeID != "" {
		return *attached.RuntimeID, nil
	}
	runtimeID := uuid.NewString()
	binary := configuredOpencodeBinary(ctx, client)
	var cursorValue any
	if len(launch.cursor) > 0 {
		var value any
		if json.Unmarshal(launch.cursor, &value) == nil {
			cursorValue = value
		}
	}
	command := map[string]any{
		"type": "start",
		"options": map[string]any{
			"binary":               binary,
			"cwd":                  launch.cwd,
			"mode":                 launch.mode,
			"interactionMode":      launch.interaction,
			"model":                nullableString(launch.model),
			"reasoningEffort":      launch.reasoningEffort,
			"serviceTier":          launch.serviceTier,
			"contextWindow":        launch.contextWindow,
			"agentPreset":          launch.agentPreset,
			"providerCursor":       cursorValue,
			"mermaidPromptVersion": nullableString(launch.mermaidVersion),
		},
		"environmentId": "00000000-0000-0000-0000-000000000000",
	}
	if version := providerVersion(binary); version != "" {
		command["expectedOpencodeVersion"] = version
	}
	if _, err := client.RequestFor(ctx, launch.sessionID, runtimeID, command); err != nil {
		return "", err
	}
	return runtimeID, nil
}

func nullableString(value string) any {
	if value == "" {
		return nil
	}
	return value
}

func configuredOpencodeBinary(parent context.Context, client *daemon.Client) string {
	if client != nil {
		ctx, cancel := context.WithTimeout(parent, 10*time.Second)
		defer cancel()
		if payload, err := client.Request(ctx, map[string]any{"type": "getSettings"}); err == nil {
			var settings struct {
				Settings map[string]json.RawMessage `json:"settings"`
			}
			if json.Unmarshal(payload, &settings) == nil {
				if raw, ok := settings.Settings["opencode_binary_path"]; ok {
					var path string
					if json.Unmarshal(raw, &path) == nil && path != "" {
						return path
					}
				}
			}
		}
	}
	if path, err := exec.LookPath("opencode"); err == nil {
		return path
	}
	return "opencode"
}

func providerVersion(binary string) string {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	output, err := exec.CommandContext(ctx, binary, "--version").CombinedOutput()
	if err != nil {
		return ""
	}
	return parseProviderVersion(string(output))
}

// parseProviderVersion mirrors parse_cli_version: the first non-empty line's
// first token that looks like a dotted version, without a leading "v".
func parseProviderVersion(output string) string {
	line := ""
	for _, candidate := range strings.Split(output, "\n") {
		if strings.TrimSpace(candidate) != "" {
			line = candidate
			break
		}
	}
	for _, token := range strings.Fields(line) {
		token = strings.TrimPrefix(token, "v")
		token = strings.TrimFunc(token, func(r rune) bool {
			return !(r >= '0' && r <= '9' || r >= 'a' && r <= 'z' ||
				r >= 'A' && r <= 'Z' || r == '.' || r == '-')
		})
		parts := strings.Split(token, ".")
		if len(parts) < 2 || parts[0] == "" {
			continue
		}
		leading := true
		for _, r := range parts[0] {
			if r < '0' || r > '9' {
				leading = false
				break
			}
		}
		if !leading || parts[1] == "" {
			continue
		}
		if parts[1][0] >= '0' && parts[1][0] <= '9' {
			return token
		}
	}
	return ""
}

// cancelTurn stops the running turn.
func (a *app) cancelTurn() {
	if a.detail == nil || a.client == nil {
		return
	}
	a.cancelTurnForSession(a.detail)
}

func (a *app) cancelTurnForSession(session *model.Session) {
	if session == nil || a.client == nil {
		return
	}
	sessionID := session.ID
	runtimeID := a.runtimes[sessionID]
	turnID := session.ActiveTurnID()
	state := a.stateForSession(session)
	state.stopped, state.preparing = true, false
	if turn := a.currentTurn(session); turn != nil {
		turn.Status = "interrupted"
		completed := uint64(time.Now().Unix())
		turn.CompletedAt = &completed
	}
	session.Status = "idle"
	for index := range session.Messages {
		session.Messages[index].Streaming = false
	}
	if a.detail != nil && a.detail.ID == sessionID {
		a.pendingPermission, a.pendingUserInput = nil, nil
	}
	state.permission, state.question = nil, nil
	a.syncSummary(session)
	a.rebuildRows()
	if a.detail != nil && a.detail.ID == sessionID {
		a.rebuildTranscript()
	}
	a.saveSessionSnapshot(session)
	a.stopSessionSubmission(sessionID, turnID, runtimeID)
}

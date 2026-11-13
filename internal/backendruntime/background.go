package backendruntime

import (
	"encoding/json"
	"errors"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"
	"github.com/google/uuid"
)

const maxChildren = 256
const maxBackgroundSnapshotBytes = 8 << 20

func boundedBackgroundText(text string, limit int) (string, bool) {
	if len(text) <= limit {
		return text, false
	}
	end := limit
	for end > 0 && !utf8.RuneStart(text[end]) {
		end--
	}
	return text[:end], true
}

type pendingChild struct{ id, prompt, target string }
type childState struct {
	item      model.BackgroundWorkItem
	actor     *actor
	revives   []pendingChild
	seen      map[string]bool
	bytes     int
	truncated bool
	announced bool
}

func (a *actor) backgroundEvent(kind string, value any) error {
	data := backendstore.Fields(map[string]any{"type": kind})
	if kind == "upsert" {
		raw, _ := json.Marshal(value)
		item, err := backendstore.Decode(raw)
		if err != nil {
			return err
		}
		for k, v := range item {
			data[k] = v
		}
	} else if kind == "transcript" {
		raw, _ := json.Marshal(value)
		event, err := backendstore.Decode(raw)
		if err != nil {
			return err
		}
		for k, v := range event {
			data[k] = v
		}
	} else {
		data.Set("items", value)
	}
	return a.emit("backgroundWork", data)
}
func (a *actor) childTranscript(c *childState, kind string, fields map[string]any) error {
	fields["key"] = c.item.Key
	return a.backgroundEvent("transcript", map[string]any{kind: fields})
}
func (a *actor) childUpsert(c *childState) error {
	c.item.UpdatedAtMs = uint64Now()
	c.item.CanStop = c.item.Live() && c.item.Status != "stopping"
	a.dirty = true
	return a.backgroundEvent("upsert", c.item)
}

func childMetadata(payload backendstore.Object) backendstore.Object {
	for _, key := range []string{"session", "info"} {
		if nested, err := backendstore.Decode(payload[key]); err == nil && nested != nil {
			return nested
		}
	}
	return payload
}
func childParent(meta backendstore.Object) string {
	if value := meta.String("parentID"); value != "" {
		return value
	}
	return meta.String("parentId")
}
func childTarget(input backendstore.Object) string {
	for _, key := range []string{"sessionID", "sessionId", "session_id"} {
		if value := input.String(key); value != "" {
			return value
		}
	}
	return ""
}

func updateChildMetadata(item *model.BackgroundWorkItem, meta backendstore.Object) {
	info, _ := backendstore.Decode(meta["info"])
	text := func(keys ...string) string {
		for _, key := range keys {
			if value := strings.TrimSpace(meta.String(key)); value != "" {
				return value
			}
			if value := strings.TrimSpace(info.String(key)); value != "" {
				return value
			}
		}
		return ""
	}
	if title := text("title", "name"); title != "" {
		value, truncated := boundedBackgroundText(title, 2048)
		item.Title = value
		item.OutputTruncated = item.OutputTruncated || truncated
	}
	if role := text("agent", "role"); role != "" {
		value, truncated := boundedBackgroundText(role, 256)
		item.Role = &value
		item.OutputTruncated = item.OutputTruncated || truncated
	}
	modelName := text("model")
	if modelName == "" {
		value, e := backendstore.Decode(meta["model"])
		if e != nil || value == nil {
			value, _ = backendstore.Decode(info["model"])
		}
		if provider, id := value.String("providerID"), value.String("id"); provider != "" && id != "" {
			modelName = provider + "/" + id
		}
	}
	if modelName != "" {
		value, truncated := boundedBackgroundText(modelName, 1024)
		item.Model = &value
		item.OutputTruncated = item.OutputTruncated || truncated
	}
	if item.Command == nil {
		prompt := text("prompt", "text")
		if prompt == "" {
			for _, key := range []string{"prompt", "message"} {
				nested, _ := backendstore.Decode(meta[key])
				if value := nested.String("text"); value != "" {
					prompt = value
					break
				}
			}
		}
		if prompt != "" {
			value, truncated := boundedBackgroundText(prompt, 16000)
			prompt = value
			item.OutputTruncated = item.OutputTruncated || truncated
			item.Command = &prompt
		}
	}
}

func restoredChildStatus(row backendstore.Object, active bool) string {
	if active {
		return "running"
	}
	switch row.String("outcome") {
	case "succeeded":
		return "completed"
	case "failed":
		return "failed"
	case "interrupted":
		return "stopped"
	}
	status := row.String("status")
	if nested, err := backendstore.Decode(row["status"]); err == nil {
		status = nested.String("type")
	}
	switch status {
	case "busy", "running", "starting":
		return "running"
	case "error", "failed":
		return "failed"
	case "idle", "completed", "success":
		return "completed"
	}
	return "lost"
}

func (a *actor) newChild(item model.BackgroundWorkItem) *childState {
	c := &childState{item: item, seen: map[string]bool{}}
	c.actor = &actor{manager: a.manager, parent: a, child: c, sessionID: a.sessionID, runtimeID: a.runtimeID, nativeID: item.Key.ProviderID, process: a.process, ctx: a.ctx, snapshot: backendstore.Fields(map[string]any{"status": "idle", "messages": []any{}, "transcript_blocks": []any{}, "turns": []any{}}), parts: map[string]int{}, reasoning: map[string]int{}, tools: map[string]string{}, forms: map[string]map[string]bool{}}
	return c
}
func (a *actor) loadBackground() error {
	a.children = map[string]*childState{}
	var snapshots []model.BackgroundWorkSnapshot
	if raw := a.snapshot["background_work"]; len(raw) > 0 && string(raw) != "null" {
		if len(raw) > maxBackgroundSnapshotBytes {
			return errors.New("background snapshot exceeds budget")
		}
		if err := json.Unmarshal(raw, &snapshots); err != nil {
			return errors.New("invalid background snapshot")
		}
	}
	if len(snapshots) > maxChildren {
		return errors.New("background registry exceeds budget")
	}
	for _, saved := range snapshots {
		item := saved.Item
		if item.Key.Kind != "subagent" {
			a.otherBackground = append(a.otherBackground, saved)
			continue
		}
		if item.Key.Kind != "subagent" || item.Key.ProviderID == "" || item.Key.ProviderID == a.nativeID || item.ParentID == nil || *item.ParentID != a.nativeID {
			continue
		}
		id := item.Key.ProviderID
		item.ControlID = &id
		c := a.newChild(item)
		raw, _ := json.Marshal(saved.Transcript)
		c.bytes = len(raw)
		a.backgroundBytes += c.bytes
		c.actor.snapshot.Set("messages", saved.Transcript.Messages)
		c.actor.snapshot.Set("transcript_blocks", saved.Transcript.TranscriptBlocks)
		c.actor.snapshot.Set("turns", saved.Transcript.Turns)
		if err := c.actor.loadSnapshot(); err != nil {
			return err
		}
		if len(c.actor.turns) > 0 {
			c.actor.turnID = c.actor.turns[len(c.actor.turns)-1].String("id")
			c.actor.started = item.Live()
		}
		for _, origin := range item.OriginActivityIDs {
			c.seen[origin] = true
		}
		// An execution-start after restart is not by itself permission to reopen
		// a settled child. Revive bindings intentionally are not persisted.
		a.children[id] = c
	}
	a.restoreShells()
	return nil
}
func (a *actor) backgroundSnapshots() []model.BackgroundWorkSnapshot {
	result := append([]model.BackgroundWorkSnapshot{}, a.otherBackground...)
	for _, shell := range a.shells {
		if shell.history {
			continue
		}
		result = append(result, model.BackgroundWorkSnapshot{Item: shell.item, Transcript: model.BackgroundWorkTranscript{Messages: []model.Message{}, TranscriptBlocks: []json.RawMessage{}, Turns: []model.AgentTurn{}}})
	}
	ids := make([]string, 0, len(a.children))
	for id := range a.children {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	for _, id := range ids {
		c := a.children[id]
		_ = c.actor.syncSnapshot() // Child snapshots do not contain background work.
		var transcript model.BackgroundWorkTranscript
		_ = json.Unmarshal(c.actor.snapshot["messages"], &transcript.Messages)
		_ = json.Unmarshal(c.actor.snapshot["transcript_blocks"], &transcript.TranscriptBlocks)
		_ = json.Unmarshal(c.actor.snapshot["turns"], &transcript.Turns)
		if transcript.Messages == nil {
			transcript.Messages = []model.Message{}
		}
		if transcript.TranscriptBlocks == nil {
			transcript.TranscriptBlocks = []json.RawMessage{}
		}
		if transcript.Turns == nil {
			transcript.Turns = []model.AgentTurn{}
		}
		result = append(result, model.BackgroundWorkSnapshot{Item: c.item, Transcript: transcript})
	}
	sort.SliceStable(result, func(i, j int) bool {
		left, right := result[i].Item.Key, result[j].Item.Key
		if left.Kind != right.Kind {
			return left.Kind < right.Kind
		}
		return left.ProviderID < right.ProviderID
	})
	return result
}
func (a *actor) publishBackground() error {
	for _, saved := range a.backgroundSnapshots() {
		if err := a.backgroundEvent("upsert", saved.Item); err != nil {
			return err
		}
		if err := a.backgroundEvent("transcript", map[string]any{"snapshot": map[string]any{"key": saved.Item.Key, "transcript": saved.Transcript}}); err != nil {
			return err
		}
	}
	return nil
}

func (a *actor) bindChildCall(id, name string, raw json.RawMessage) error {
	leaf := strings.ToLower(strings.TrimSpace(name))
	if leaf != "task" && leaf != "subagent" {
		return nil
	}
	a.backgroundRevision++
	input, err := backendstore.Decode(raw)
	if err != nil || input == nil {
		return nil
	}
	prompt := ""
	for _, key := range []string{"prompt", "message", "task", "description"} {
		if prompt == "" {
			prompt = input.String(key)
		}
	}
	if len(prompt) > 64<<10 {
		prompt = string([]rune(prompt)[:min(len([]rune(prompt)), 16000)]) + "\n[truncated]"
		_ = a.emit("error", "Subagent prompt was truncated")
	}
	call := pendingChild{id: id, prompt: prompt, target: childTarget(input)}
	queuedBytes := len(prompt)
	for _, pending := range a.pendingChildren {
		queuedBytes += len(pending.prompt)
	}
	if queuedBytes > 1<<20 {
		return a.emit("error", "Pending subagent prompts reached their size limit")
	}
	if c := a.children[call.target]; c != nil {
		return a.armChild(c, call)
	}
	for _, pending := range a.pendingChildren {
		if pending.id == id {
			return nil
		}
	}
	if len(a.pendingChildren) >= maxChildren {
		return a.emit("error", "Pending subagent capacity reached")
	}
	a.pendingChildren = append(a.pendingChildren, call)
	return nil
}
func (a *actor) armChild(c *childState, call pendingChild) error {
	if c.seen[call.id] {
		return nil
	}
	if len(c.seen) >= 1024 || len(c.revives) >= maxChildren {
		return a.emit("error", "Subagent continuation capacity reached")
	}
	c.seen[call.id] = true
	c.item.OriginActivityIDs = append(c.item.OriginActivityIDs, call.id)
	c.revives = append(c.revives, call)
	return a.childUpsert(c)
}
func (a *actor) takeChildCall(id string) (pendingChild, bool) {
	index := -1
	for i, call := range a.pendingChildren {
		if call.target == id {
			index = i
			break
		}
	}
	if index < 0 {
		for i, call := range a.pendingChildren {
			if call.target == "" {
				index = i
				break
			}
		}
	}
	if index < 0 {
		return pendingChild{}, false
	}
	call := a.pendingChildren[index]
	a.pendingChildren = append(a.pendingChildren[:index], a.pendingChildren[index+1:]...)
	return call, true
}
func (a *actor) startChild(c *childState, prompt string) error {
	child := c.actor
	if len(child.turns) >= 256 || len(child.messages) >= 4096 || len(child.blocks) >= 4096 {
		c.truncated = true
		c.item.OutputTruncated = true
		detail := "Subagent transcript reached its size limit"
		c.item.Detail = &detail
		return a.childUpsert(c)
	}
	if len(prompt)+c.bytes > 1<<20 || a.backgroundBytes+len(prompt) > 8<<20 {
		c.truncated = true
		c.item.OutputTruncated = true
		detail := "Subagent transcript reached its size limit"
		c.item.Detail = &detail
		prompt = ""
	}
	c.bytes += len(prompt)
	a.backgroundBytes += len(prompt)
	if c.item.Status != "starting" {
		c.announced = false
	}
	child.turnID = uuid.NewString()
	child.started = false
	child.parts = map[string]int{}
	child.reasoning = map[string]int{}
	child.tools = map[string]string{}
	child.turns = append(child.turns, backendstore.Fields(map[string]any{"id": child.turnID, "status": "running", "turn_count": len(child.turns) + 1, "started_at": time.Now().Unix()}))
	if prompt != "" {
		child.messages = append(child.messages, backendstore.Fields(map[string]any{"id": uuid.NewString(), "turn_id": child.turnID, "role": "user", "content": prompt, "created_at": time.Now().Unix(), "streaming": false, "attachments": []any{}}))
		c.item.Command = &prompt
	}
	c.item.Status = "running"
	c.item.DurationMs = nil
	c.item.StartedAtMs = uint64Now()
	if err := a.childUpsert(c); err != nil {
		return err
	}
	return child.begin()
}

func (a *actor) childEvent(event backendengine.Event) error {
	payload, err := backendstore.Decode(event.Payload)
	if err != nil {
		return err
	}
	if payload == nil {
		return nil
	}
	id := event.SessionID
	if event.Kind == "session.created" {
		meta := childMetadata(payload)
		if childParent(meta) != a.nativeID || id == "" || id == a.nativeID {
			return nil
		}
		a.backgroundRevision++
		if a.children == nil {
			a.children = map[string]*childState{}
		}
		c := a.children[id]
		if c != nil {
			return nil
		}
		if len(a.children)+len(a.shells)+len(a.otherBackground) >= maxChildren {
			return a.emit("error", "Background session capacity reached")
		}
		parent := a.nativeID
		title := meta.String("title")
		if title == "" {
			title = "Subagent"
		}
		c = a.newChild(model.BackgroundWorkItem{Key: model.BackgroundWorkKey{Kind: "subagent", ProviderID: id}, Title: title, ParentID: &parent, ControlID: &id, Status: "starting", Background: true, OriginActivityIDs: []string{}, StartedAtMs: uint64Now(), UpdatedAtMs: uint64Now()})
		updateChildMetadata(&c.item, meta)
		a.children[id] = c
		prompt := ""
		if call, ok := a.takeChildCall(id); ok {
			prompt = call.prompt
			c.seen[call.id] = true
			c.item.OriginActivityIDs = append(c.item.OriginActivityIDs, call.id)
		}
		if prompt != "" {
			c.item.Command = &prompt
		}
		c.announced = true
		if err := a.childUpsert(c); err != nil {
			return err
		}
		return a.childTranscript(c, "started", map[string]any{"prompt": c.item.Command})
	}
	c := a.children[id]
	if c == nil {
		allowed := false
		for _, pending := range a.pendingChildren {
			if pending.target == id {
				allowed = true
				break
			}
		}
		if strings.HasPrefix(event.Kind, "permission.") || event.Kind == "form.created" || event.Kind == "question.asked" {
			allowed = true
		}
		if allowed && id != "" {
			if a.unresolvedChildEvents == nil {
				a.unresolvedChildEvents = map[string][]backendengine.Event{}
			}
			if len(a.unresolvedChildEvents) >= 256 || len(a.unresolvedChildEvents[id]) >= 64 || a.unresolvedChildBytes+len(event.Payload) > 1<<20 {
				return a.emit("error", "Background session event limit reached")
			}
			first := len(a.unresolvedChildEvents[id]) == 0
			a.unresolvedChildEvents[id] = append(a.unresolvedChildEvents[id], event)
			a.unresolvedChildBytes += len(event.Payload)
			if first {
				return a.refreshChildren(a.ctx)
			}
		}
		return nil
	}
	a.backgroundRevision++
	if event.Kind == "session.deleted" {
		c.item.Status = "stopped"
		c.revives = nil
		a.clearChildRequests(id)
		c.actor.finish(false, "interrupted")
		if err := a.childUpsert(c); err != nil {
			return err
		}
		return a.childTranscript(c, "finished", map[string]any{"success": false})
	}
	if event.Kind == "session.execution.started" {
		if c.item.Status == "starting" {
			prompt := ""
			if c.item.Command != nil {
				prompt = *c.item.Command
			}
			return a.startChild(c, prompt)
		}
		if !c.item.Live() && len(c.revives) > 0 {
			call := c.revives[0]
			c.revives = c.revives[1:]
			return a.startChild(c, call.prompt)
		}
		// New child creation already opens its initial transcript. Duplicated
		// starts and settled-child stragglers must not allocate another turn.
		return nil
	}
	if event.Kind == "session.updated" {
		meta := childMetadata(payload)
		updateChildMetadata(&c.item, meta)
		return a.childUpsert(c)
	}
	if !c.item.Live() {
		return nil
	}
	if event.Kind == "session.execution.succeeded" || event.Kind == "session.execution.failed" || event.Kind == "session.execution.interrupted" {
		status := "failed"
		success := event.Kind == "session.execution.succeeded"
		if success {
			status = "completed"
		}
		if event.Kind == "session.execution.interrupted" {
			status = "stopped"
		}
		c.actor.finish(success, status)
		c.item.Status = status
		duration := uint64Now() - c.item.StartedAtMs
		c.item.DurationMs = &duration
		for request, scope := range a.requestScopes {
			if scope == id {
				delete(a.requestScopes, request)
				delete(a.forms, request)
			}
		}
		if err := a.childUpsert(c); err != nil {
			return err
		}
		return a.childTranscript(c, "finished", map[string]any{"success": success})
	}
	if strings.HasPrefix(event.Kind, "permission.") || event.Kind == "question.asked" || event.Kind == "form.created" {
		return c.actor.event(event)
	}
	if c.actor.turnID == "" {
		return nil
	}
	if c.truncated {
		return nil
	}
	size := len(event.Payload)
	if size > 64<<10 || c.bytes+size*8+1024 > 1<<20 || a.backgroundBytes+size*8+1024 > 8<<20 || len(c.actor.messages) >= 4096 || len(c.actor.blocks) >= 4096 {
		c.truncated = true
		c.item.OutputTruncated = true
		text := "Subagent transcript reached its size limit"
		c.item.Detail = &text
		return a.childUpsert(c)
	}
	c.bytes += size*8 + 1024
	a.backgroundBytes += size*8 + 1024
	if err := c.actor.event(event); err != nil {
		return err
	}
	a.dirty = true
	return nil
}

func (a *actor) clearChildRequests(id string) {
	for request, scope := range a.requestScopes {
		if scope == id {
			delete(a.requestScopes, request)
			delete(a.forms, request)
		}
	}
}

func (a *actor) childEmission(c *childState, kind string, payload any) error {
	switch kind {
	case "turnStarted":
		if c.announced {
			return nil
		}
		c.announced = true
		return a.childTranscript(c, "started", map[string]any{"prompt": c.item.Command})
	case "textDelta", "reasoningDelta":
		raw, _ := json.Marshal(payload)
		value, _ := backendstore.Decode(raw)
		return a.childTranscript(c, kind, map[string]any{"delta": value.String("delta")})
	case "richActivity":
		return a.childTranscript(c, "activity", map[string]any{"activity": payload})
	case "textEnded", "reasoningEnded":
		c.actor.syncSnapshot()
		var transcript model.BackgroundWorkTranscript
		_ = json.Unmarshal(c.actor.snapshot["messages"], &transcript.Messages)
		_ = json.Unmarshal(c.actor.snapshot["transcript_blocks"], &transcript.TranscriptBlocks)
		_ = json.Unmarshal(c.actor.snapshot["turns"], &transcript.Turns)
		return a.childTranscript(c, "snapshot", map[string]any{"transcript": transcript})
	case "permission", "userInputRequested":
		raw, _ := json.Marshal(payload)
		value, _ := backendstore.Decode(raw)
		request := value.String("requestId")
		if request == "" {
			return errors.New("child request identity missing")
		}
		if a.requestScopes == nil {
			a.requestScopes = map[string]string{}
		}
		if previous := a.requestScopes[request]; previous != "" && previous != c.item.Key.ProviderID {
			return errors.New("ambiguous child request identity")
		}
		if len(a.requestScopes) >= 256 {
			return errors.New("child request capacity reached")
		}
		a.requestScopes[request] = c.item.Key.ProviderID
		if kind == "userInputRequested" {
			a.forms[request] = c.actor.forms[request]
		}
		return a.emit(kind, payload)
	case "usageUpdated":
		raw, _ := json.Marshal(payload)
		value, _ := backendstore.Decode(raw)
		if n, err := value.Int("contextTokens"); err == nil && n >= 0 {
			tokens := uint64(n)
			c.item.ContextTokens = &tokens
		}
		return a.childUpsert(c)
	}
	return nil
}

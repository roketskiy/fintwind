package backendruntime

import (
	"context"
	"encoding/json"
	"net/url"
	"strings"
	"time"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

func (a *actor) extendedEvent(kind string, payload backendstore.Object) (bool, error) {
	switch kind {
	case "session.tool.input.started", "session.tool.called", "session.tool.progress", "session.tool.success", "session.tool.error", "session.tool.failed":
		id := payload.String("id")
		if id == "" {
			return true, nil
		}
		if a.tools == nil {
			a.tools = map[string]string{}
		}
		name, known := a.tools[id]
		if kind == "session.tool.progress" && !known {
			return true, nil
		}
		if kind == "session.tool.input.started" {
			name = payload.String("name")
			if strings.TrimSpace(name) == "" {
				return true, nil
			}
			a.tools[id] = name
		}
		if err := a.begin(); err != nil {
			return true, err
		}
		item := model.ActivityItem{ID: uuid.NewSHA1(uuid.MustParse(a.sessionID), []byte(a.turnID+id)).String(), SourceID: &id, Kind: "tool", Title: payload.String("name")}
		blockIndex, activityIndex := -1, -1
		for bi, block := range a.blocks {
			content, _ := backendstore.Decode(block["content"])
			if content.String("kind") != "activities" {
				continue
			}
			var activities []model.ActivityItem
			if json.Unmarshal(content["data"], &activities) != nil {
				continue
			}
			for ai, activity := range activities {
				if activity.ID == item.ID {
					item = activity
					blockIndex = bi
					activityIndex = ai
					break
				}
			}
			if blockIndex >= 0 {
				break
			}
		}
		if item.Title == "" {
			item.Title = "Tool"
		}
		if name != "" {
			item.Title = name
			item.Kind = model.ToolKind(name)
		}
		if raw := payload["input"]; len(raw) > 0 {
			value := string(raw)
			item.Arguments = &value
			if kind == "session.tool.called" && a.parent == nil {
				if err := a.bindChildCall(id, name, raw); err != nil {
					return true, err
				}
			}
		}
		if raw := payload["content"]; len(raw) > 0 {
			var text string
			if json.Unmarshal(raw, &text) != nil {
				text = string(raw)
			}
			item.Output = &text
		}
		if strings.HasSuffix(kind, ".success") || strings.HasSuffix(kind, ".error") || strings.HasSuffix(kind, ".failed") {
			delete(a.tools, id)
			item.Complete = true
			item.Failed = !strings.HasSuffix(kind, ".success")
		}
		if raw := payload["error"]; item.Failed && len(raw) > 0 {
			text := activityText(raw)
			item.Output = &text
		}
		output := payload["content"]
		if item.Failed && len(payload["error"]) > 0 {
			output = payload["error"]
		}
		source, _ := json.Marshal(payload)
		model.ApplyToolMetadata(&item, payload["input"], output, source)
		if item.Complete {
			if err := a.observeShell(item, payload); err != nil {
				return true, err
			}
		}
		if blockIndex < 0 {
			a.blocks = append(a.blocks, backendstore.Fields(map[string]any{"after_message": len(a.messages), "turn_id": a.turnID, "content": map[string]any{"kind": "activities", "data": []model.ActivityItem{item}}}))
		} else {
			content, _ := backendstore.Decode(a.blocks[blockIndex]["content"])
			var activities []model.ActivityItem
			_ = json.Unmarshal(content["data"], &activities)
			activities[activityIndex] = item
			content.Set("data", activities)
			a.blocks[blockIndex].Set("content", content)
		}
		a.dirty = true
		return true, a.emit("richActivity", item)
	case "session.compaction.started", "session.compaction.delta", "session.compaction.ended", "session.compaction.failed":
		status := "running"
		if strings.HasSuffix(kind, ".ended") {
			status = "completed"
		}
		if strings.HasSuffix(kind, ".failed") {
			status = "failed"
		}
		value := backendstore.Fields(map[string]any{"status": status, "reason": payload.Nullable("reason"), "model": payload.Nullable("model"), "error": payload.Nullable("error"), "summary": payload.Nullable("summary")})
		if a.snapshot != nil {
			a.snapshot.Set("compaction", value)
			a.dirty = true
		}
		return true, a.emit("compactionUpdated", value)
	case "session.usage.updated", "session.step.ended":
		var data struct {
			Tokens struct {
				Input, Output, Reasoning, Total uint64
				Cache                           struct{ Read, Write uint64 }
			} `json:"tokens"`
		}
		raw, _ := json.Marshal(payload)
		if json.Unmarshal(raw, &data) != nil {
			return true, nil
		}
		prompt := data.Tokens.Input + data.Tokens.Cache.Read + data.Tokens.Cache.Write
		contextTokens := data.Tokens.Total
		if contextTokens == 0 {
			contextTokens = prompt + data.Tokens.Output + data.Tokens.Reasoning
		}
		if contextTokens == 0 {
			return true, nil
		}
		wire := map[string]any{"contextWindowResolved": false}
		if kind == "session.usage.updated" {
			a.usageTotal = prompt + data.Tokens.Output + data.Tokens.Reasoning
			a.usageCache, a.usagePrompt, a.usageAuthoritative = data.Tokens.Cache.Read, prompt, true
		} else {
			wire["contextTokens"] = contextTokens
			if !a.usageAuthoritative {
				a.usageTotal += contextTokens
				a.usageCache += data.Tokens.Cache.Read
				a.usagePrompt += prompt
			}
		}
		wire["sessionTotal"], wire["cacheRead"], wire["promptTokens"] = a.usageTotal, a.usageCache, a.usagePrompt
		if a.snapshot != nil {
			usage, _ := backendstore.Decode(a.snapshot["context_usage"])
			if usage == nil {
				usage = backendstore.Object{}
			}
			if kind == "session.step.ended" {
				usage.Set("tokens", contextTokens)
				usage.Set("measured", true)
			}
			usage.Set("total_tokens", a.usageTotal)
			usage.Set("cache_read", a.usageCache)
			usage.Set("prompt_tokens", a.usagePrompt)
			a.snapshot.Set("context_usage", usage)
			a.dirty = true
		}
		return true, a.emit("usageUpdated", wire)
	case "rpc.opencode.goal.changed", "session.goal.updated", "goal.updated":
		raw, _ := json.Marshal(payload)
		if nested := payload["goal"]; len(nested) > 0 {
			raw = nested
		}
		goal, err := normalizeGoal(raw, a.nativeID)
		if err != nil {
			return true, err
		}
		return true, a.saveGoal(goal)
	case "session.retry.scheduled":
		return true, a.retryEvent(payload, "at")
	case "session.status":
		if a.snapshot != nil {
			a.snapshot.Set("updated_at", time.Now().Unix())
			a.dirty = true
		}
		status, _ := backendstore.Decode(payload["status"])
		switch status.String("type") {
		case "busy":
			return true, a.emit("providerBusy", nil)
		case "retry":
			return true, a.retryEvent(status, "next")
		case "idle":
			return true, a.idleReport()
		}
		return true, nil
	case "session.idle":
		return true, a.idleReport()
	}
	return false, nil
}

func (a *actor) retryEvent(payload backendstore.Object, nextKey string) error {
	attempt, _ := payload.Int("attempt")
	if attempt < 0 {
		attempt = 0
	}
	if attempt > 4294967295 {
		attempt = 4294967295
	}
	message := payload.String("message")
	if detail, err := backendstore.Decode(payload["error"]); err == nil && detail.String("message") != "" {
		message = detail.String("message")
	}
	var action any
	if detail, err := backendstore.Decode(payload["action"]); err == nil && detail != nil {
		action = backendstore.Fields(map[string]any{"reason": detail.String("reason"), "provider": detail.String("provider"), "title": detail.String("title"), "message": detail.String("message"), "label": detail.String("label"), "link": detail.String("link")})
	}
	var nextAt *int64
	if value, err := payload.Int(nextKey); err == nil && value >= 0 {
		nextAt = &value
	}
	return a.emit("providerRetry", map[string]any{"attempt": attempt, "message": message, "action": action, "nextAtMs": nextAt})
}
func (a *actor) idleReport() error {
	if a.snapshot == nil || a.snapshot.String("status") != "working" {
		return nil
	}
	ctx, cancel := context.WithTimeout(a.ctx, 3*time.Second)
	defer cancel()
	raw, err := a.process.Client.RoundTrip(ctx, "GET", "/api/session/"+url.PathEscape(a.nativeID)+"/message?limit=5", nil)
	if err != nil {
		return nil
	}
	var messages struct {
		Data []backendstore.Object `json:"data"`
	}
	if json.Unmarshal(raw, &messages) != nil {
		return nil
	}
	var summary any
	for _, message := range messages.Data {
		if message.String("type") == "assistant" || message.String("role") == "assistant" {
			if raw := message["error"]; len(raw) > 0 && string(raw) != "null" {
				text := activityText(raw)
				summary = text
			}
			break
		}
	}
	success := summary == nil
	status := "completed"
	if !success {
		status = "failed"
		if err := a.recordProviderError(summary.(string)); err != nil {
			return err
		}
	}
	a.finish(success, status)
	a.tools = map[string]string{}
	if err := a.flush(); err != nil {
		return err
	}
	return a.emit("turnFinished", map[string]any{"success": success, "summary": summary})
}

func activityText(raw json.RawMessage) string {
	var text string
	if json.Unmarshal(raw, &text) == nil {
		return text
	}
	var detail struct {
		Message string `json:"message"`
		Data    struct {
			Message string `json:"message"`
		} `json:"data"`
	}
	if json.Unmarshal(raw, &detail) == nil && detail.Message != "" {
		return detail.Message
	}
	if detail.Data.Message != "" {
		return detail.Data.Message
	}
	return string(raw)
}

package backendremote

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"unicode/utf8"

	"fintwind-go/internal/model"
)

type projectedEntry struct {
	anchor int
	kind   int
	block  int
	item   int
	value  map[string]any
}

func Transcript(session *model.Session) (messages []map[string]any, entries []map[string]any, truncated bool) {
	messages = make([]map[string]any, 0)
	entries = make([]map[string]any, 0)
	if session == nil {
		return
	}
	start := max(0, len(session.Messages)-100)
	truncated = start > 0
	turns := make(map[string]model.AgentTurn, len(session.Turns))
	for _, turn := range session.Turns {
		turns[turn.ID] = turn
	}
	terminal := make(map[string]string)
	for index := len(session.Messages) - 1; index >= start; index-- {
		message := &session.Messages[index]
		if message.Role == "assistant" && message.TurnID != nil && terminal[*message.TurnID] == "" {
			terminal[*message.TurnID] = message.ID
		}
	}
	ordered := make([]projectedEntry, 0, 200)
	for index := start; index < len(session.Messages); index++ {
		message := &session.Messages[index]
		if message.Role != "user" && message.Role != "assistant" {
			continue
		}
		text := message.Content
		if message.DisplayContent != nil {
			text = *message.DisplayContent
		}
		visible := limitText(text, 16384, message.Role == "assistant")
		truncated = truncated || len(text) != len(visible)
		attachments := make([]any, 0, min(8, len(message.Attachments)))
		for _, attachment := range message.Attachments[:min(8, len(message.Attachments))] {
			name := limitText(attachment.Name, 128, false)
			if name == "" {
				name = "attachment"
			}
			attachments = append(attachments, map[string]any{"name": name, "isImage": attachment.IsImage})
		}
		entry := map[string]any{"type": "message", "id": message.ID, "role": message.Role, "text": visible, "streaming": message.Streaming, "createdAt": message.CreatedAt, "attachments": attachments}
		if message.TurnID != nil {
			entry["turnId"] = *message.TurnID
			if turn, ok := turns[*message.TurnID]; ok {
				entry["turnRunning"] = turn.Running()
				entry["responseComplete"] = message.Role == "assistant" && !message.Streaming && !turn.Running() && terminal[*message.TurnID] == message.ID
			}
		} else {
			entry["responseComplete"] = message.Role == "assistant" && !message.Streaming
		}
		ordered = append(ordered, projectedEntry{anchor: index, kind: 1, value: entry})
	}
	firstBlock := max(0, len(session.TranscriptBlocks)-200)
	truncated = truncated || firstBlock > 0
	for blockIndex := firstBlock; blockIndex < len(session.TranscriptBlocks); blockIndex++ {
		block := &session.TranscriptBlocks[blockIndex]
		if block.AfterMessage < start || block.AfterMessage > len(session.Messages) {
			continue
		}
		first := max(0, len(block.Activities)-64)
		truncated = truncated || first > 0
		for index := first; index < len(block.Activities); index++ {
			activity := &block.Activities[index]
			text := ""
			if activity.Kind == model.KindReasoning && activity.Reasoning != nil {
				text = activity.Reasoning.Content
			}
			visible := limitText(text, 12288, true)
			id := activity.ID
			if id == "" {
				id = fmt.Sprintf("legacy-%d-%d", blockIndex, index)
			}
			entry := map[string]any{"type": "activity", "id": id, "kind": activity.Kind, "title": limitText(activity.Title, 256, false), "text": visible, "complete": activity.Complete, "failed": activity.Failed, "truncated": len(text) != len(visible)}
			if block.TurnID != nil {
				entry["turnId"] = *block.TurnID
				if turn, ok := turns[*block.TurnID]; ok {
					entry["turnRunning"] = turn.Running()
				}
			}
			ordered = append(ordered, projectedEntry{anchor: block.AfterMessage, block: blockIndex, item: index, value: entry})
		}
	}
	sort.SliceStable(ordered, func(left, right int) bool {
		first, second := ordered[left], ordered[right]
		if first.anchor != second.anchor {
			return first.anchor < second.anchor
		}
		if first.kind != second.kind {
			return first.kind < second.kind
		}
		if first.block != second.block {
			return first.block < second.block
		}
		return first.item < second.item
	})
	remaining := 128 * 1024
	for index := len(ordered) - 1; index >= 0; index-- {
		if len(entries) >= 200 {
			truncated = true
			break
		}
		raw, err := json.Marshal(ordered[index].value)
		if err != nil || len(raw) > remaining {
			truncated = true
			break
		}
		remaining -= len(raw)
		entries = append(entries, ordered[index].value)
	}
	for left, right := 0, len(entries)-1; left < right; left, right = left+1, right-1 {
		entries[left], entries[right] = entries[right], entries[left]
	}
	for _, entry := range entries {
		if entry["type"] == "message" {
			messages = append(messages, entry)
		}
	}
	return
}

func limitText(text string, limit int, tail bool) string {
	text = strings.ToValidUTF8(text, "�")
	if len(text) <= limit {
		return text
	}
	if tail {
		text = text[len(text)-limit:]
		for !utf8.ValidString(text) {
			text = text[1:]
		}
		return text
	}
	text = text[:limit]
	for !utf8.ValidString(text) {
		text = text[:len(text)-1]
	}
	return text
}

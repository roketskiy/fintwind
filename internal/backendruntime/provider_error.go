package backendruntime

import (
	"time"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendstore"

	"github.com/google/uuid"
)

func providerErrorText(payload backendstore.Object) string {
	for _, key := range []string{"error", "message", "detail"} {
		if text := backendengine.ErrorText(payload[key]); text != "" {
			return text
		}
	}
	return "OpenCode session failed"
}

func (a *actor) recordProviderError(text string) error {
	if a.snapshot == nil {
		return a.emit("error", text)
	}
	if len(a.messages) > 0 {
		last := a.messages[len(a.messages)-1]
		if last.String("role") == "system" && last.String("turn_id") == a.turnID && (last.String("content") == text || text == "OpenCode session failed") {
			return nil
		}
	}
	id := uuid.NewString()
	now := time.Now().Unix()
	message := backendstore.Fields(map[string]any{
		"id": id, "turn_id": a.turnID, "role": "system", "content": text,
		"created_at": now, "streaming": false, "attachments": []any{},
	})
	a.messages = append(a.messages, message)
	a.dirty = true
	if err := a.flush(); err != nil {
		return err
	}
	return a.emit("error", map[string]any{"id": id, "message": text, "turnId": a.turnID, "createdAt": now})
}

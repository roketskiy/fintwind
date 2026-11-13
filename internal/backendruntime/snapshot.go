package backendruntime

import (
	"context"
	"encoding/json"
	"errors"

	"fintwind-go/internal/backendstore"
)

func (a *actor) loadSnapshot() error {
	if a.snapshot == nil {
		return errors.New("session snapshot required")
	}
	if !a.usageInitialized {
		var usage struct {
			Total  uint64 `json:"total_tokens"`
			Cache  uint64 `json:"cache_read"`
			Prompt uint64 `json:"prompt_tokens"`
		}
		_ = json.Unmarshal(a.snapshot["context_usage"], &usage)
		a.usageTotal, a.usageCache, a.usagePrompt = usage.Total, usage.Cache, usage.Prompt
		a.usageInitialized = true
	}
	for _, field := range []struct {
		key    string
		target *[]backendstore.Object
	}{{"messages", &a.messages}, {"transcript_blocks", &a.blocks}, {"turns", &a.turns}} {
		*field.target = nil
		if raw := a.snapshot[field.key]; len(raw) > 0 {
			if err := json.Unmarshal(raw, field.target); err != nil {
				return err
			}
		}
		if *field.target == nil {
			*field.target = []backendstore.Object{}
		}
	}
	if a.parent == nil && a.children == nil {
		return a.loadBackground()
	}
	return nil
}

func (a *actor) syncSnapshot() error {
	a.snapshot.Set("messages", a.messages)
	a.snapshot.Set("transcript_blocks", a.blocks)
	a.snapshot.Set("turns", a.turns)
	if a.parent == nil {
		raw, err := boundedBackgroundSnapshot(a.backgroundSnapshots())
		if err != nil {
			return err
		}
		a.snapshot["background_work"] = raw
		cursors := map[string]uint64{}
		for id, shell := range a.shells {
			cursors[id] = shell.cursor
		}
		a.snapshot.Set("_fintwind_shell_cursors", cursors)
	}
	return nil
}

func (a *actor) reconcile(ctx context.Context) error {
	transcript, err := a.process.Client.Transcript(ctx, a.nativeID)
	if err != nil {
		return err
	}
	a.restoreNativeShells(transcript.DetachedShells)
	preserveAt := len(transcript.Turns)
	for index, turn := range a.turns {
		if turn.String("status") == "interrupted" && index < preserveAt {
			preserveAt = index
			break
		}
	}
	if preserveAt < len(transcript.Turns) {
		preserved := make(map[string]bool, len(transcript.Turns)-preserveAt)
		for _, turn := range transcript.Turns[preserveAt:] {
			preserved[turn.ID] = true
		}
		messages := transcript.Messages[:0]
		for _, message := range transcript.Messages {
			if message.TurnID == nil || !preserved[*message.TurnID] {
				messages = append(messages, message)
			}
		}
		transcript.Messages = messages
		blocks := transcript.Blocks[:0]
		for _, block := range transcript.Blocks {
			if block.TurnID == nil || !preserved[*block.TurnID] {
				blocks = append(blocks, block)
			}
		}
		transcript.Blocks = blocks
		transcript.Turns = transcript.Turns[:preserveAt]
	}
	identities := map[string]string{}
	for index := range transcript.Turns {
		if index < len(a.turns) {
			old := transcript.Turns[index].ID
			id := a.turns[index].String("id")
			identities[old] = id
			transcript.Turns[index].ID = id
			transcript.Turns[index].Submission = a.turns[index]["submission"]
		}
	}
	for index := range transcript.Messages {
		if transcript.Messages[index].TurnID != nil {
			if id, ok := identities[*transcript.Messages[index].TurnID]; ok {
				transcript.Messages[index].TurnID = &id
			}
		}
	}
	for index := range transcript.Blocks {
		block := &transcript.Blocks[index]
		if block.TurnID != nil {
			if id, ok := identities[*block.TurnID]; ok {
				block.TurnID = &id
				raw, _ := json.Marshal(map[string]any{"after_message": block.AfterMessage, "turn_id": id, "content": map[string]any{"kind": "activities", "data": block.Activities}})
				block.Raw = raw
			}
		}
	}
	messages := make([]json.RawMessage, 0, len(transcript.Messages)+len(a.messages))
	for _, message := range transcript.Messages {
		raw, err := json.Marshal(message)
		if err != nil {
			return err
		}
		messages = append(messages, raw)
	}
	turns := make([]json.RawMessage, 0, len(transcript.Turns)+len(a.turns))
	for _, turn := range transcript.Turns {
		raw, err := json.Marshal(turn)
		if err != nil {
			return err
		}
		turns = append(turns, raw)
	}
	blocks := make([]json.RawMessage, len(transcript.Blocks))
	for index, block := range transcript.Blocks {
		blocks[index] = block.Raw
	}
	if len(transcript.Turns) < len(a.turns) {
		missing := make(map[string]bool, len(a.turns)-len(transcript.Turns))
		for _, turn := range a.turns[len(transcript.Turns):] {
			missing[turn.String("id")] = true
			raw, err := json.Marshal(turn)
			if err != nil {
				return err
			}
			turns = append(turns, raw)
		}
		start := len(a.messages)
		for index, message := range a.messages {
			if missing[message.String("turn_id")] {
				start = index
				break
			}
		}
		for _, message := range a.messages[start:] {
			raw, err := json.Marshal(message)
			if err != nil {
				return err
			}
			messages = append(messages, raw)
		}
		for _, block := range a.blocks {
			if !missing[block.String("turn_id")] {
				continue
			}
			after, err := block.Int("after_message")
			if err != nil {
				return err
			}
			block.Set("after_message", max(0, len(transcript.Messages)+int(after)-start))
			raw, err := json.Marshal(block)
			if err != nil {
				return err
			}
			blocks = append(blocks, raw)
		}
	}
	a.snapshot.Set("messages", messages)
	a.snapshot.Set("turns", turns)
	a.snapshot.Set("transcript_blocks", blocks)
	if err := a.loadSnapshot(); err != nil {
		return err
	}
	a.snapshot.Set("provider_cursor", map[string]string{"provider": "openCode", "sessionId": a.nativeID})
	a.snapshot.Set("native_session_id", a.nativeID)
	a.dirty = true
	return nil
}

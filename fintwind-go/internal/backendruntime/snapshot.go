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
	a.snapshot.Set("messages", transcript.Messages)
	a.snapshot.Set("turns", transcript.Turns)
	blocks := make([]json.RawMessage, len(transcript.Blocks))
	for index, block := range transcript.Blocks {
		blocks[index] = block.Raw
	}
	a.snapshot.Set("transcript_blocks", blocks)
	if err := a.loadSnapshot(); err != nil {
		return err
	}
	a.snapshot.Set("provider_cursor", map[string]string{"provider": "openCode", "sessionId": a.nativeID})
	a.snapshot.Set("native_session_id", a.nativeID)
	a.dirty = true
	return nil
}

package main

import (
	"context"
	"encoding/json"
	"errors"

	"fintwind-go/internal/daemon"

	"github.com/google/uuid"
)

func checkSettings(ctx context.Context, client *daemon.Client) error {
	request := func(value map[string]any) (json.RawMessage, error) { return client.Request(ctx, value) }
	if _, err := request(map[string]any{"type": "updateSettings", "settings": map[string]any{"preserve_future": "literal", "auto_compact_enabled": true}}); err != nil {
		return err
	}
	if _, err := request(map[string]any{"type": "updateSetting", "key": "auto_compact_threshold_tokens", "value": 500000}); err != nil {
		return err
	}
	if _, err := request(map[string]any{"type": "updateSetting", "key": "auto_compact_enabled", "value": nil}); err == nil {
		return errors.New("null boolean setting accepted")
	}
	if _, err := request(map[string]any{"type": "updateSetting", "key": "auto_compact_threshold_tokens", "value": 9999}); err == nil {
		return errors.New("out of range threshold accepted")
	}
	raw, err := request(map[string]any{"type": "getSettings"})
	if err != nil {
		return err
	}
	var settings struct {
		Settings map[string]json.RawMessage `json:"settings"`
	}
	if err := json.Unmarshal(raw, &settings); err != nil {
		return err
	}
	if string(settings.Settings["preserve_future"]) != `"literal"` || string(settings.Settings["auto_compact_threshold_tokens"]) != "500000" {
		return errors.New("settings update lost unrelated fields")
	}
	id := uuid.NewString()
	if _, err := request(map[string]any{"type": "saveComposerDrafts", "generation": 10, "drafts": map[string]any{"sessions": map[string]any{id: map[string]any{"text": "newer"}}}}); err != nil {
		return err
	}
	if _, err := request(map[string]any{"type": "saveComposerDrafts", "generation": 9, "drafts": map[string]any{}}); err != nil {
		return err
	}
	raw, err = request(map[string]any{"type": "loadComposerDrafts"})
	if err != nil {
		return err
	}
	var drafts struct {
		Drafts struct {
			Sessions map[string]struct {
				Text string `json:"text"`
			} `json:"sessions"`
		} `json:"drafts"`
	}
	if err := json.Unmarshal(raw, &drafts); err != nil {
		return err
	}
	if drafts.Drafts.Sessions[id].Text != "newer" {
		return errors.New("older generation overwrote draft")
	}
	if _, err := request(map[string]any{"type": "applyComposerDraftChanges", "changes": []any{map[string]any{"target": map[string]string{"type": "session", "sessionId": id}, "draft": map[string]string{"text": ""}}}}); err != nil {
		return err
	}
	raw, err = request(map[string]any{"type": "loadComposerDrafts"})
	if err != nil {
		return err
	}
	drafts.Drafts.Sessions = nil
	if err := json.Unmarshal(raw, &drafts); err != nil {
		return err
	}
	if _, exists := drafts.Drafts.Sessions[id]; exists {
		return errors.New("empty draft not removed")
	}
	return nil
}

package main

import (
	"context"
	"encoding/json"
	"errors"
	"time"

	"fintwind-go/internal/daemon"
	"github.com/google/uuid"
)

func checkScheduled(ctx context.Context, c *daemon.Client, sessionID string) (string, error) {
	draft := map[string]any{"title": "isolated schedule", "prompt": "literal prompt", "enabled": false, "schedule": map[string]any{"kind": "every", "intervalSecs": 1}, "target": map[string]any{"kind": "existingSession", "sessionId": sessionID}}
	if _, err := c.Request(ctx, map[string]any{"type": "saveScheduledTask", "edit": map[string]any{"kind": "create", "draft": draft}}); err == nil {
		return "", errors.New("invalid scheduled interval accepted")
	}
	draft["schedule"] = map[string]any{"kind": "once", "at": time.Now().Unix() - 60}
	draft["enabled"] = true
	if _, err := c.Request(ctx, map[string]any{"type": "saveScheduledTask", "edit": map[string]any{"kind": "create", "draft": draft}}); err == nil {
		return "", errors.New("past scheduled task accepted")
	}
	draft["schedule"] = map[string]any{"kind": "every", "intervalSecs": 60}
	draft["enabled"] = false
	raw, err := c.Request(ctx, map[string]any{"type": "saveScheduledTask", "edit": map[string]any{"kind": "create", "draft": draft}})
	if err != nil {
		return "", err
	}
	var saved struct {
		Type string `json:"type"`
		Task struct {
			ID       string `json:"id"`
			Revision int    `json:"revision"`
		} `json:"task"`
	}
	if err := json.Unmarshal(raw, &saved); err != nil {
		return "", err
	}
	if saved.Type != "scheduledTaskSaved" || uuid.Validate(saved.Task.ID) != nil || saved.Task.Revision != 0 {
		return "", errors.New("invalid scheduled task response")
	}
	id := saved.Task.ID
	if _, err := c.Request(ctx, map[string]any{"type": "saveScheduledTask", "edit": map[string]any{"kind": "update", "id": id, "expectedRevision": 1, "draft": draft}}); err == nil {
		return "", errors.New("stale scheduled update accepted")
	}
	if _, err := c.Request(ctx, map[string]any{"type": "deleteScheduledTask", "id": id, "expectedRevision": 1}); err == nil {
		return "", errors.New("stale scheduled delete accepted")
	}
	return id, nil
}

func checkScheduledRestart(ctx context.Context, c *daemon.Client, id string) error {
	raw, err := c.Request(ctx, map[string]any{"type": "listScheduledTasks"})
	if err != nil {
		return err
	}
	var list struct {
		Tasks []struct {
			ID      string `json:"id"`
			Title   string `json:"title"`
			Enabled bool   `json:"enabled"`
		} `json:"tasks"`
	}
	if err := json.Unmarshal(raw, &list); err != nil {
		return err
	}
	if len(list.Tasks) != 1 || list.Tasks[0].ID != id || list.Tasks[0].Title != "isolated schedule" || list.Tasks[0].Enabled {
		return errors.New("scheduled task restart lost data")
	}
	_, err = c.Request(ctx, map[string]any{"type": "deleteScheduledTask", "id": id, "expectedRevision": 0})
	return err
}

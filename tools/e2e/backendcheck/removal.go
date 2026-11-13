package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"

	"fintwind-go/internal/daemon"

	"github.com/google/uuid"
)

func checkRemoveProject(ctx context.Context, client *daemon.Client) error {
	projectID, sessionID, messageID := uuid.NewString(), uuid.NewString(), uuid.NewString()
	project := map[string]any{"id": projectID, "name": "doomed project", "path": "isolated", "created_at": 7}
	session := map[string]any{
		"id": sessionID, "project_id": projectID, "title": "doomed", "provider": "openCode", "status": "idle", "created_at": 7, "updated_at": 8,
		"messages": []any{map[string]any{"id": messageID, "role": "user", "content": "doomed content", "attachments": []any{}, "created_at": 7, "streaming": false}},
		"turns":    []any{}, "transcript_blocks": []any{},
	}
	if _, err := client.Request(ctx, map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{project}, "sessions": []any{session}, "skeletonSessionIds": []any{}}); err != nil {
		return fmt.Errorf("save doomed snapshot: %w", err)
	}
	if _, err := client.Request(ctx, map[string]any{"type": "removeProject", "projectId": projectID}); err != nil {
		return fmt.Errorf("remove project: %w", err)
	}
	var catalog struct {
		Projects []map[string]any `json:"projects"`
		Sessions []map[string]any `json:"sessions"`
	}
	raw, err := client.Request(ctx, map[string]any{"type": "loadTaskState"})
	if err != nil {
		return err
	}
	if err := json.Unmarshal(raw, &catalog); err != nil {
		return err
	}
	for _, entry := range catalog.Projects {
		if entry["id"] == projectID {
			return errors.New("removed project still listed")
		}
	}
	for _, entry := range catalog.Sessions {
		if entry["id"] == sessionID {
			return errors.New("removed project session still listed")
		}
	}
	if err := expectNullSession(ctx, client, sessionID); err != nil {
		return err
	}
	if _, err := client.Request(ctx, map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{project}, "sessions": []any{session}, "skeletonSessionIds": []any{}}); err != nil {
		return fmt.Errorf("re-save after project removal: %w", err)
	}
	if err := expectNullSession(ctx, client, sessionID); err != nil {
		return errors.New("stale snapshot resurrected a removed project session")
	}
	reuseProject := map[string]any{"id": uuid.NewString(), "name": "reuse project", "path": "isolated", "created_at": 9}
	reuseSession := map[string]any{
		"id": uuid.NewString(), "project_id": reuseProject["id"], "title": "reuse", "provider": "openCode", "status": "idle", "created_at": 9, "updated_at": 10,
		"messages": []any{map[string]any{"id": messageID, "role": "user", "content": "reused identity", "attachments": []any{}, "created_at": 9, "streaming": false}},
		"turns":    []any{}, "transcript_blocks": []any{},
	}
	if _, err := client.Request(ctx, map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{reuseProject}, "sessions": []any{reuseSession}, "skeletonSessionIds": []any{}}); err != nil {
		return fmt.Errorf("message identity was not released with its project: %w", err)
	}
	raw, err = client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": reuseSession["id"]})
	if err != nil {
		return err
	}
	var hydrated struct {
		Session struct {
			Messages []struct {
				Content string `json:"content"`
			} `json:"messages"`
		} `json:"session"`
	}
	if err := json.Unmarshal(raw, &hydrated); err != nil {
		return err
	}
	if len(hydrated.Session.Messages) != 1 || hydrated.Session.Messages[0].Content != "reused identity" {
		return errors.New("reused message identity did not hydrate")
	}
	if _, err := client.Request(ctx, map[string]any{"type": "removeProject", "projectId": reuseProject["id"]}); err != nil {
		return fmt.Errorf("remove reuse project: %w", err)
	}
	if err := expectNullSession(ctx, client, reuseSession["id"].(string)); err != nil {
		return err
	}
	raw, err = client.RequestFor(ctx, sessionID, "", map[string]any{"type": "attachSession"})
	if err != nil {
		return err
	}
	var attached struct {
		RuntimeID *string `json:"runtimeId"`
	}
	if err := json.Unmarshal(raw, &attached); err != nil {
		return err
	}
	if attached.RuntimeID != nil {
		return errors.New("removed session still attaches a live runtime")
	}
	return nil
}

func checkSkeletonStale(ctx context.Context, client *daemon.Client) error {
	projectID, sessionID, messageID := uuid.NewString(), uuid.NewString(), uuid.NewString()
	project := map[string]any{"id": projectID, "name": "skeleton project", "path": "isolated", "created_at": 11}
	full := map[string]any{
		"id": sessionID, "project_id": projectID, "title": "New Title", "provider": "openCode", "status": "idle", "created_at": 11, "updated_at": 20,
		"messages": []any{map[string]any{"id": messageID, "role": "user", "content": "fresh transcript", "attachments": []any{}, "created_at": 11, "streaming": false}},
		"turns":    []any{}, "transcript_blocks": []any{}, "future_extension": map[string]any{"mustSurvive": "fresh"},
	}
	if _, err := client.Request(ctx, map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{project}, "sessions": []any{full}, "skeletonSessionIds": []any{}}); err != nil {
		return fmt.Errorf("save full session: %w", err)
	}
	stale := map[string]any{
		"id": sessionID, "project_id": projectID, "title": "Stale Title", "provider": "openCode", "status": "idle", "created_at": 11, "updated_at": 12,
		"messages": []any{}, "turns": []any{}, "transcript_blocks": []any{},
	}
	saved, err := client.Request(ctx, map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{}, "sessions": []any{stale}, "skeletonSessionIds": []any{sessionID}})
	if err != nil {
		return fmt.Errorf("save stale skeleton: %w", err)
	}
	var projection struct {
		Sessions []struct {
			Title     string `json:"title"`
			UpdatedAt int64  `json:"updated_at"`
		} `json:"sessions"`
	}
	if err := json.Unmarshal(saved, &projection); err != nil {
		return err
	}
	if len(projection.Sessions) != 1 || projection.Sessions[0].Title != "New Title" || projection.Sessions[0].UpdatedAt != 20 {
		return errors.New("stale skeleton regressed the session projection")
	}
	raw, err := client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sessionID})
	if err != nil {
		return err
	}
	var hydrated struct {
		Session struct {
			Title    string `json:"title"`
			Messages []struct {
				Content string `json:"content"`
			} `json:"messages"`
			Future struct {
				Value string `json:"mustSurvive"`
			} `json:"future_extension"`
		} `json:"session"`
	}
	if err := json.Unmarshal(raw, &hydrated); err != nil {
		return err
	}
	if hydrated.Session.Title != "New Title" || len(hydrated.Session.Messages) != 1 || hydrated.Session.Messages[0].Content != "fresh transcript" || hydrated.Session.Future.Value != "fresh" {
		return errors.New("stale skeleton damaged the stored transcript")
	}
	if _, err := client.Request(ctx, map[string]any{"type": "removeProject", "projectId": projectID}); err != nil {
		return fmt.Errorf("remove skeleton project: %w", err)
	}
	return expectNullSession(ctx, client, sessionID)
}

func expectNullSession(ctx context.Context, client *daemon.Client, sessionID string) error {
	raw, err := client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sessionID})
	if err != nil {
		return err
	}
	var value struct {
		Session json.RawMessage `json:"session"`
	}
	if err := json.Unmarshal(raw, &value); err != nil {
		return err
	}
	if string(value.Session) != "null" {
		return errors.New("expected a null session")
	}
	return nil
}

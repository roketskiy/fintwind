package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"

	"fintwind-go/internal/daemon"

	"github.com/google/uuid"
)

func pressure(ctx context.Context, address, token, root string) error {
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()
	projectID, sessionID := uuid.NewString(), uuid.NewString()
	messages := make([]any, 2000)
	for i := range messages {
		messages[i] = map[string]any{"id": uuid.NewString(), "role": "user", "content": fmt.Sprintf("literal-pressure-%04d", i), "created_at": i + 1, "streaming": false, "attachments": []any{}}
	}
	session := map[string]any{"id": sessionID, "project_id": projectID, "title": "pressure fixture", "provider": "openCode", "status": "idle", "created_at": 1, "updated_at": 2, "messages": messages, "turns": []any{}, "transcript_blocks": []any{}, "test_secret": "SENTINEL_ONLY_NOT_REAL_SECRET"}
	command := map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{map[string]any{"id": projectID, "name": "pressure", "path": root, "created_at": 1}}, "sessions": []any{session}, "skeletonSessionIds": []any{}}
	for i := 0; i < 3; i++ {
		if _, err := client.Request(ctx, command); err != nil {
			return err
		}
	}
	var wg sync.WaitGroup
	errorsOut := make(chan error, 8)
	for range 8 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			reader, err := daemon.Dial(ctx, address, token)
			if err != nil {
				errorsOut <- err
				return
			}
			defer reader.Close()
			for range 8 {
				raw, err := reader.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sessionID})
				if err != nil {
					errorsOut <- err
					return
				}
				var got struct {
					Session struct {
						Messages []struct {
							Content string `json:"content"`
						} `json:"messages"`
					} `json:"session"`
				}
				if err := json.Unmarshal(raw, &got); err != nil {
					errorsOut <- err
					return
				}
				if len(got.Session.Messages) != 2000 || got.Session.Messages[1999].Content != "literal-pressure-1999" {
					errorsOut <- errors.New("pressure read lost ordered messages")
					return
				}
			}
		}()
	}
	wg.Wait()
	close(errorsOut)
	for err := range errorsOut {
		if err != nil {
			return err
		}
	}
	return nil
}

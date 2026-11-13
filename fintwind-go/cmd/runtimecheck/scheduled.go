package main

import (
	"context"
	"encoding/json"
	"errors"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
)

func checkScheduledDelivery(ctx context.Context, client *daemon.Client, sessionID string) error {
	before, err := hydrateState(ctx, client, sessionID)
	if err != nil {
		return err
	}
	raw, err := client.Request(ctx, map[string]any{"type": "saveScheduledTask", "edit": map[string]any{"kind": "create", "draft": map[string]any{"title": "Literal scheduled delivery", "prompt": "scheduled literal request", "enabled": true, "schedule": map[string]any{"kind": "every", "intervalSecs": 86400}, "target": map[string]any{"kind": "existingSession", "sessionId": sessionID}}}})
	if err != nil {
		return err
	}
	var saved struct {
		Task struct {
			ID string `json:"id"`
		} `json:"task"`
	}
	if err := json.Unmarshal(raw, &saved); err != nil {
		return err
	}
	if _, err := client.Request(ctx, map[string]any{"type": "runScheduledTaskNow", "id": saved.Task.ID}); err != nil {
		return err
	}
	admitted := ""
	text := ""
	for {
		select {
		case <-ctx.Done():
			return errors.New("scheduled delivery did not finish")
		case event, ok := <-client.Events():
			if !ok {
				return errors.New("scheduled event stream closed")
			}
			if event.SessionID != sessionID {
				continue
			}
			switch event.Kind {
			case "scheduledTurnAdmitted":
				var payload struct {
					Turn struct {
						ID      string                  `json:"id"`
						Receipt model.SubmissionReceipt `json:"submission"`
					} `json:"turn"`
					Message struct {
						TurnID  string `json:"turn_id"`
						Content string `json:"content"`
					} `json:"message"`
				}
				if err := json.Unmarshal(event.Payload, &payload); err != nil {
					return err
				}
				admitted = payload.Turn.ID
				if admitted == "" || payload.Message.TurnID != admitted || payload.Turn.Receipt.ID != admitted || payload.Message.Content != "scheduled literal request" {
					return errors.New("scheduled admission identity mismatch")
				}
			case "turnStarted", "textDelta":
				if admitted == "" {
					return errors.New("scheduled provider event preceded durable admission")
				}
				if event.Kind == "textDelta" {
					var delta struct {
						Delta string `json:"delta"`
					}
					if err := json.Unmarshal(event.Payload, &delta); err != nil {
						return err
					}
					text += delta.Delta
				}
			case "turnFinished":
				if admitted == "" || text != "literal answer" {
					return errors.New("scheduled output mismatch")
				}
				state, err := hydrateState(ctx, client, sessionID)
				if err != nil {
					return err
				}
				if len(state.Messages) != len(before.Messages)+2 || state.TurnStatus(admitted) != "completed" {
					return errors.New("scheduled transcript not durable")
				}
				return waitScheduledSettlement(ctx, client, saved.Task.ID, admitted)
			}
			if event.Type == "event" {
				client.Ack(event)
			}
		}
	}
}

func waitScheduledSettlement(ctx context.Context, client *daemon.Client, id, submission string) error {
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	for {
		raw, err := client.Request(ctx, map[string]any{"type": "listScheduledTasks"})
		if err != nil {
			return err
		}
		var list struct {
			Tasks []struct {
				ID       string          `json:"id"`
				RunCount int             `json:"runCount"`
				Pending  json.RawMessage `json:"pending"`
				Last     *struct {
					Outcome struct {
						Kind         string `json:"outcome"`
						SubmissionID string `json:"submissionId"`
					} `json:"outcome"`
				} `json:"last"`
			} `json:"tasks"`
		}
		if err := json.Unmarshal(raw, &list); err != nil {
			return err
		}
		for _, task := range list.Tasks {
			if task.ID == id && task.Last != nil {
				if task.RunCount != 1 || string(task.Pending) != "null" || task.Last.Outcome.Kind != "accepted" || task.Last.Outcome.SubmissionID != submission {
					return errors.New("scheduled receipt settlement mismatch")
				}
				return nil
			}
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
		}
	}
}

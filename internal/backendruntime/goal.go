package backendruntime

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"strings"
	"time"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"
	"github.com/google/uuid"
)

func normalizeGoal(raw json.RawMessage, session string) (backendstore.Object, error) {
	return normalizeGoalDepth(raw, session, 0)
}
func normalizeGoalDepth(raw json.RawMessage, session string, depth int) (backendstore.Object, error) {
	if depth > 4 {
		return nil, errors.New("invalid native goal envelope")
	}
	if string(raw) == "null" {
		return nil, nil
	}
	value, err := backendstore.Decode(raw)
	if err != nil {
		return nil, err
	}
	if output, ok := value["output"]; ok {
		return normalizeGoalDepth(output, session, depth+1)
	}
	status := value.String("status")
	switch status {
	case "active", "paused":
	case "complete":
		status = "done"
	case "done", "blocked":
	default:
		return nil, errors.New("invalid native goal state")
	}
	rounds, err := value.Int("rounds")
	if len(value["rounds"]) == 0 || string(value["rounds"]) == "null" || err != nil || rounds < 0 || rounds > 4294967295 {
		return nil, errors.New("invalid native goal rounds")
	}
	max, err := value.Int("maxRounds")
	if len(value["maxRounds"]) == 0 || string(value["maxRounds"]) == "null" || err != nil || max < 0 || max > 4294967295 {
		return nil, errors.New("invalid native goal limit")
	}
	updated, err := value.Int("updatedAt")
	if len(value["updatedAt"]) == 0 || string(value["updatedAt"]) == "null" || err != nil || updated < 0 {
		return nil, errors.New("invalid native goal revision")
	}
	var objective string
	if len(value["objective"]) == 0 || string(value["objective"]) == "null" || json.Unmarshal(value["objective"], &objective) != nil {
		return nil, errors.New("invalid native goal objective")
	}
	goal := backendstore.Fields(map[string]any{"sessionId": session, "id": value.String("id"), "objective": value.String("objective"), "status": status, "turns": rounds, "maxTurns": max, "reason": value.String("reason"), "revision": updated})
	if status == "done" {
		goal.Set("completedAt", updated/1000)
	}
	return goal, nil
}
func (a *actor) readGoal(ctx context.Context) (backendstore.Object, error) {
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	raw, err := a.process.Client.RoundTrip(ctx, "POST", "/api/rpc/opencode.goal/get", map[string]any{"input": map[string]string{"sessionID": a.nativeID}})
	if err != nil {
		return nil, err
	}
	return normalizeGoal(raw, a.nativeID)
}
func (a *actor) saveGoal(goal backendstore.Object) error {
	unchanged := false
	if a.snapshot != nil {
		var currentValue any
		_ = json.Unmarshal(a.snapshot["goal"], &currentValue)
		current, _ := json.Marshal(currentValue)
		next, _ := json.Marshal(goal)
		unchanged = string(current) == string(next)
	}
	var completed backendstore.Object
	if a.snapshot != nil && goal.String("status") == "done" {
		identity := goal.String("id")
		if identity == "" {
			identity = goal.String("objective") + ":" + string(goal["revision"])
		}
		id := uuid.NewSHA1(uuid.NameSpaceURL, []byte("fintwind:goal:"+a.sessionID+":"+identity)).String()
		found := false
		for _, message := range a.messages {
			if message.String("id") == id {
				found = true
				break
			}
		}
		if !found {
			content := strings.TrimSpace(goal.String("objective"))
			if reason := strings.TrimSpace(goal.String("reason")); reason != "" {
				content += "\n\n" + reason
			}
			var turnID any
			if a.turnID != "" {
				turnID = a.turnID
			}
			completed = backendstore.Fields(map[string]any{"id": id, "turn_id": turnID, "role": "goal_complete", "content": content, "created_at": time.Now().Unix(), "streaming": false, "attachments": []any{}})
			a.messages = append(a.messages, completed)
			a.dirty = true
		}
	}
	if unchanged && completed == nil {
		return nil
	}
	if a.snapshot != nil {
		a.snapshot.Set("goal", goal)
		a.dirty = true
	}
	if completed != nil {
		if err := a.flush(); err != nil {
			return err
		}
		if err := a.emit("goalCompleted", completed); err != nil {
			return err
		}
	}
	a.goalRevision++
	return a.emit("goalUpdated", goal)
}
func (a *actor) goalCommand(ctx context.Context, action backendstore.Object) error {
	a.goalRevision++
	if a.snapshot == nil {
		saved, err := a.manager.store.Hydrate(ctx, a.sessionID)
		if err != nil {
			return err
		}
		a.snapshot, err = backendstore.Decode(saved["session"])
		if err != nil || a.snapshot == nil {
			return errors.New("saved task required for Goal")
		}
		if err := a.loadSnapshot(); err != nil {
			return err
		}
		a.snapshot.Set("native_session_id", a.nativeID)
		a.snapshot.Set("provider_cursor", map[string]string{"provider": "openCode", "sessionId": a.nativeID})
	}
	kind := action.String("type")
	if kind == "get" {
		goal, err := a.readGoal(ctx)
		if err != nil {
			_ = a.emit("goalStatusReported", map[string]any{"Err": "Could not read Goal status"})
			return err
		}
		if err := a.saveGoal(goal); err != nil {
			return err
		}
		return a.emit("goalStatusReported", map[string]any{"Ok": goal})
	}
	if kind != "start" && kind != "pause" && kind != "resume" && kind != "clear" {
		return errors.New("unknown Goal action")
	}
	text := kind
	files := []backendengine.PromptFile{}
	if kind == "start" {
		text = action.String("objective")
		if strings.TrimSpace(text) == "" {
			return errors.New("Goal objective required")
		}
		var attachments []model.SubmissionFile
		if raw := action["attachments"]; len(raw) > 0 && string(raw) != "null" {
			if err := json.Unmarshal(raw, &attachments); err != nil {
				return err
			}
		}
		var err error
		files, err = a.promptFiles(attachments)
		if err != nil {
			return err
		}
	}
	skills, err := a.process.Client.PromptSkills(ctx, text)
	if err != nil {
		return err
	}
	starts := kind == "start" || kind == "resume"
	opened := false
	if starts && a.snapshot != nil && a.snapshot.String("status") != "working" {
		a.turnID = uuid.NewString()
		a.started = false
		a.parts = map[string]int{}
		a.reasoning = map[string]int{}
		a.tools = map[string]string{}
		turn := backendstore.Fields(map[string]any{"id": a.turnID, "status": "running", "turn_count": len(a.turns) + 1, "started_at": time.Now().Unix()})
		a.turns = append(a.turns, turn)
		var message backendstore.Object
		if kind == "start" {
			attachments := action["attachments"]
			if len(attachments) == 0 || string(attachments) == "null" {
				attachments = json.RawMessage(`[]`)
			}
			message = backendstore.Fields(map[string]any{"id": uuid.NewString(), "turn_id": a.turnID, "role": "user", "content": text, "created_at": time.Now().Unix(), "streaming": false, "attachments": attachments})
			a.messages = append(a.messages, message)
		}
		if err := a.emit("goalTurnStarted", map[string]any{"turn": turn, "message": message}); err != nil {
			return err
		}
		if err := a.begin(); err != nil {
			return err
		}
		opened = true
	}
	body := map[string]any{"name": "goal", "text": text, "delivery": "steer"}
	if len(skills) > 0 {
		body["skills"] = skills
	}
	if len(files) > 0 {
		body["files"] = files
	}
	_, err = a.process.Client.RoundTrip(ctx, "POST", "/api/session/"+url.PathEscape(a.nativeID)+"/command", body)
	if err != nil {
		var status *backendengine.HTTPError
		rejected := errors.As(err, &status) && (status.Status == 400 || status.Status == 422)
		if opened && rejected {
			a.finish(false, "failed")
			_ = a.emit("turnFinished", map[string]any{"success": false, "summary": nil})
		}
		_ = a.emit("goalCommandFailed", "Goal command was not confirmed; do not retry automatically")
		_ = a.flush()
		return err
	}
	goal, err := a.readGoal(ctx)
	if err == nil {
		if err := a.saveGoal(goal); err != nil {
			return err
		}
	}
	return a.flush()
}

type goalReadResult struct {
	revision uint64
	goal     backendstore.Object
	err      error
}

func (a *actor) pollGoal() {
	if a.snapshot == nil || a.goalReading || time.Since(a.goalChecked) < 5*time.Second {
		return
	}
	a.goalChecked = time.Now()
	a.goalReading = true
	if a.goalResults == nil {
		a.goalResults = make(chan goalReadResult, 1)
	}
	client, session, results, revision, ctx := a.process.Client, a.nativeID, a.goalResults, a.goalRevision, a.ctx
	go func() {
		work, cancel := context.WithTimeout(ctx, 10*time.Second)
		defer cancel()
		raw, err := client.RoundTrip(work, "POST", "/api/rpc/opencode.goal/get", map[string]any{"input": map[string]string{"sessionID": session}})
		var goal backendstore.Object
		if err == nil {
			goal, err = normalizeGoal(raw, session)
		}
		select {
		case results <- goalReadResult{revision: revision, goal: goal, err: err}:
		case <-ctx.Done():
		}
	}()
}

package backend

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"os/exec"
	"path/filepath"
	"time"

	sched "fintwind-go/internal/backendscheduled"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"
	"github.com/google/uuid"
)

func (s *Server) scheduledCommand(ctx context.Context, c backendstore.Object) (backendstore.Object, error) {
	switch c.String("type") {
	case "listScheduledTasks":
		tasks, err := s.Store.ListScheduled(ctx)
		if err != nil {
			return nil, err
		}
		return backendstore.Reply("scheduledTasks", backendstore.Fields(map[string]any{"tasks": tasks})), nil
	case "saveScheduledTask":
		var edit sched.Edit
		if err := json.Unmarshal(c["edit"], &edit); err != nil {
			return nil, err
		}
		if err := edit.Draft.Validate(); err != nil {
			return nil, err
		}
		if _, _, err := s.scheduledTarget(ctx, edit.Draft); err != nil {
			return nil, err
		}
		task, err := s.Store.SaveScheduled(ctx, edit, time.Now().Unix())
		if err != nil {
			return nil, err
		}
		s.notify("scheduledTasksChanged")
		s.wakeScheduled()
		return backendstore.Reply("scheduledTaskSaved", backendstore.Fields(map[string]any{"task": task})), nil
	case "deleteScheduledTask":
		id, err := c.ID("id")
		if err != nil {
			return nil, err
		}
		revision, err := c.Int("expectedRevision")
		if err != nil {
			return nil, err
		}
		if err := s.Store.DeleteScheduled(ctx, id, revision); err != nil {
			return nil, err
		}
		s.notify("scheduledTasksChanged")
		return backendstore.Reply("ack", nil), nil
	case "runScheduledTaskNow":
		id, err := c.ID("id")
		if err != nil {
			return nil, err
		}
		if _, err := s.Store.ClaimScheduled(ctx, id, s.Identity.InstanceID, time.Now().Unix()); err != nil {
			return nil, err
		}
		s.notify("scheduledTasksChanged")
		s.wakeScheduled()
		return backendstore.Reply("ack", nil), nil
	}
	return nil, errors.New("invalid scheduled command")
}

func (s *Server) wakeScheduled() {
	select {
	case s.scheduledWake <- struct{}{}:
	default:
	}
}

func (s *Server) scheduledTarget(ctx context.Context, draft sched.Draft) (backendstore.Object, backendstore.Object, error) {
	target, err := backendstore.Decode(draft.Target)
	if err != nil {
		return nil, nil, err
	}
	var session backendstore.Object
	projectID := target.String("projectId")
	if target.String("kind") == "existingSession" {
		payload, err := s.Store.Hydrate(ctx, target.String("sessionId"))
		if err != nil {
			return nil, nil, err
		}
		session, err = backendstore.Decode(payload["session"])
		if err != nil || session == nil {
			return nil, nil, errors.New("scheduled target no longer exists")
		}
		projectID = session.String("project_id")
	}
	project, err := s.Store.Project(ctx, projectID)
	if err != nil {
		return nil, nil, err
	}
	cwd := project.String("path")
	workspace := target["workspace"]
	if session != nil {
		workspace = session["workspace"]
	}
	if w, err := backendstore.Decode(workspace); err == nil && w.String("path") != "" {
		cwd = w.String("path")
	}
	// Reuse the filesystem boundary used by all ordinary workspace commands.
	canonical, err := filepath.EvalSymlinks(cwd)
	if err != nil {
		return nil, nil, err
	}
	if !s.daily {
		rel, err := filepath.Rel(s.WorkspaceRoot, canonical)
		if err != nil || !filepath.IsLocal(rel) {
			return nil, nil, errors.New("scheduled workspace outside isolated workspace")
		}
	}
	if session == nil {
		now := time.Now().Unix()
		launch, err := backendstore.Decode(target["launch"])
		if err != nil {
			return nil, nil, err
		}
		session = backendstore.Fields(map[string]any{"id": uuid.NewString(), "project_id": projectID, "title": draft.Title, "provider": "opencode", "model": launch.String("model"), "status": "idle", "created_at": now, "updated_at": now, "last_reply_at": 0, "detail_loaded": true, "messages": []any{}, "turns": []any{}, "transcript_blocks": []any{}, "queued_messages": []any{}, "workspace": json.RawMessage(workspace)})
		for _, field := range []struct{ wire, stored string }{{"runtimeMode", "runtime_mode"}, {"interactionMode", "interaction_mode"}, {"reasoningEffort", "reasoning_effort"}, {"serviceTier", "service_tier"}, {"contextWindow", "context_window"}, {"agentPreset", "agent_preset"}} {
			if raw := launch[field.wire]; len(raw) > 0 {
				session[field.stored] = raw
			}
		}
	}
	return project, session, nil
}

func (s *Server) runScheduled(ctx context.Context) {
	defer close(s.scheduledDone)
	ticker := time.NewTicker(5 * time.Second)
	defer ticker.Stop()
	// No active preparation belongs to this fresh runner. Resolve interrupted
	// runs from durable submission facts, never replay their network request.
	if open, err := s.Store.OpenScheduled(ctx); err == nil {
		for _, o := range open {
			if o.State == "preparing" {
				s.recoverScheduled(ctx, o)
			}
		}
	}
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		case <-s.scheduledWake:
		}
		if ctx.Err() != nil {
			return
		}
		iteration, cancel := context.WithTimeout(ctx, 45*time.Second)
		s.scheduledTick(iteration)
		cancel()
	}
}

func (s *Server) scheduledTick(ctx context.Context) {
	now := time.Now().Unix()
	if _, err := s.Store.ClaimDueScheduled(ctx, s.Identity.InstanceID, now, 4); err != nil {
		return
	}
	open, err := s.Store.OpenScheduled(ctx)
	if err != nil {
		return
	}
	// Like the Rust system channel, execution waits for an attached client.
	connected := false
	s.connections.Range(func(_, _ any) bool { connected = true; return false })
	if !connected {
		return
	}
	attempts := 0
	for _, o := range open {
		if ctx.Err() != nil || attempts >= 4 {
			return
		}
		if o.State == "preparing" {
			s.recoverScheduled(ctx, o)
			continue
		}
		if !o.Manual && o.Draft.Schedule.Kind != "every" && now-o.DueAt > 600 {
			_ = s.Store.SettleScheduled(ctx, o.ID, sched.Outcome{Kind: "missed"}, now)
			s.notify("scheduledTasksChanged")
			continue
		}
		if s.deliverScheduled(ctx, o) {
			attempts++
		}
	}
}

func (s *Server) recoverScheduled(ctx context.Context, o sched.Occurrence) {
	var saved model.SubmissionRecord
	if json.Unmarshal(o.Submission, &saved) != nil || saved.Receipt.ID == "" {
		_ = s.Store.SettleScheduled(ctx, o.ID, sched.Outcome{Kind: "notSent", Reason: "preparation interrupted before admission"}, time.Now().Unix())
		return
	}
	record, err := s.Store.Submission(ctx, saved.Receipt.ID)
	if errors.Is(err, sql.ErrNoRows) {
		_ = s.Store.SettleScheduled(ctx, o.ID, sched.Outcome{Kind: "notSent", SessionID: &saved.SessionID, SubmissionID: &saved.Receipt.ID, Reason: "preparation interrupted before admission"}, time.Now().Unix())
		return
	}
	if err != nil {
		return
	}
	if record.Receipt.State == model.SubmissionPreparing {
		record, _, err = s.Store.Transition(ctx, record.SessionID, record.Receipt.ID, model.SubmissionPreparing, model.SubmissionNotSent, nil)
		if err != nil {
			return
		}
	}
	_ = s.Store.SettleScheduled(ctx, o.ID, scheduledOutcome(record, false), time.Now().Unix())
}

func scheduledOutcome(record model.SubmissionRecord, confirmed bool) sched.Outcome {
	outcome := sched.Outcome{SessionID: &record.SessionID, SubmissionID: &record.Receipt.ID}
	switch record.Receipt.State {
	case model.SubmissionAccepted:
		outcome.Kind = "accepted"
	case model.SubmissionDispatching:
		outcome.Kind = "unconfirmed"
		if confirmed {
			outcome.Kind = "accepted"
		}
	case model.SubmissionUnknown:
		outcome.Kind = "unconfirmed"
	case model.SubmissionRejected:
		outcome.Kind = "rejected"
		outcome.Reason = "submission rejected"
	default:
		outcome.Kind = "notSent"
		outcome.Reason = "submission was not sent"
	}
	return outcome
}

func (s *Server) deliverScheduled(ctx context.Context, o sched.Occurrence) bool {
	if !s.managementMu.TryRLock() {
		return false
	}
	defer s.managementMu.RUnlock()
	s.taskEditMu.RLock()
	defer s.taskEditMu.RUnlock()
	settle := func(outcome sched.Outcome) {
		_ = s.Store.SettleScheduled(ctx, o.ID, outcome, time.Now().Unix())
		s.notify("scheduledTasksChanged")
	}
	project, session, err := s.scheduledTarget(ctx, o.Draft)
	if err != nil {
		settle(sched.Outcome{Kind: "rejected", Reason: err.Error()})
		return true
	}
	status := session.String("status")
	if status != "idle" && status != "failed" {
		return false
	}
	id := session.String("id")
	attached, err := s.runtime.Handle(ctx, id, "", backendstore.Fields(map[string]any{"type": "attachSession"}))
	if err != nil {
		return false
	}
	runtime := attached.String("runtimeId")
	live := runtime != ""
	if !live {
		runtime = uuid.NewString()
	}
	turnID := uuid.NewString()
	record := model.NewSubmissionRecord(id, turnID, o.Draft.Prompt, nil)
	var turns, messages []backendstore.Object
	if err := json.Unmarshal(session["turns"], &turns); err != nil {
		settle(sched.Outcome{Kind: "rejected", Reason: "invalid saved turns"})
		return true
	}
	if err := json.Unmarshal(session["messages"], &messages); err != nil {
		settle(sched.Outcome{Kind: "rejected", Reason: "invalid saved messages"})
		return true
	}
	now := time.Now().Unix()
	turn := backendstore.Fields(map[string]any{"id": turnID, "status": "running", "turn_count": len(turns) + 1, "started_at": now, "submission": record.Receipt})
	message := backendstore.Fields(map[string]any{"id": uuid.NewString(), "turn_id": turnID, "role": "user", "content": o.Draft.Prompt, "created_at": now, "streaming": false, "attachments": []any{}})
	turns = append(turns, turn)
	messages = append(messages, message)
	session.Set("turns", turns)
	session.Set("messages", messages)
	session.Set("detail_loaded", true)
	session.Set("status", "connecting")
	session.Set("updated_at", now)
	if session["last_reply_at"] == nil || string(session["last_reply_at"]) == "null" {
		session.Set("last_reply_at", 0)
	}
	raw, err := json.Marshal(record)
	if err != nil {
		return false
	}
	if err := s.Store.PrepareScheduled(ctx, o.ID, id, runtime, raw); err != nil {
		return false
	}
	admit := backendstore.Fields(map[string]any{"type": "acceptSubmission", "project": project, "session": session, "submission": record})
	if live {
		admit.Set("type", "admitScheduled")
		_, err = s.runtime.Handle(ctx, id, runtime, admit)
	} else {
		_, err = s.Store.Admit(ctx, id, admit)
	}
	if err != nil {
		settle(sched.Outcome{Kind: "notSent", SessionID: &id, SubmissionID: &turnID, Reason: "target did not accept scheduled input"})
		return true
	}
	// From this point recovery is based on the persisted receipt, including
	// failures in launch or dispatch. A Preparing receipt can be abandoned;
	// Dispatching/Unknown is never automatically retried.
	confirmed := false
	defer func() {
		cleanup, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_, _, _ = s.Store.Transition(cleanup, id, turnID, model.SubmissionPreparing, model.SubmissionNotSent, nil)
		saved, e := s.Store.Submission(cleanup, turnID)
		if e == nil {
			_ = s.Store.SettleScheduled(cleanup, o.ID, scheduledOutcome(saved, confirmed), time.Now().Unix())
			s.notify("scheduledTasksChanged")
		}
	}()
	if !live {
		binary, err := exec.LookPath("opencode")
		if err != nil {
			return true
		}
		cwd := project.String("path")
		if w, e := backendstore.Decode(session["workspace"]); e == nil && w.String("path") != "" {
			cwd = w.String("path")
		}
		options := backendstore.Fields(map[string]any{"binary": binary, "cwd": cwd, "model": session.String("model"), "interactionMode": session.String("interaction_mode"), "reasoningEffort": session.Nullable("reasoning_effort"), "providerCursor": session.Nullable("provider_cursor")})
		if _, err := s.runtime.Handle(ctx, id, runtime, backendstore.Fields(map[string]any{"type": "start", "options": options})); err != nil {
			return true
		}
	}
	s.notify("taskStateChanged")
	response, dispatchErr := s.runtime.Handle(ctx, id, runtime, backendstore.Fields(map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "scheduledAdmission": map[string]any{"turn": turn, "message": message}}))
	confirmed = dispatchErr == nil && response.String("type") == "submissionSaved"
	return true
}

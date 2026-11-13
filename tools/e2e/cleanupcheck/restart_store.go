package main

// Restart tombstone fence and module-level store CAS.

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

func (h *harness) restartSuite() error {
	projectX := h.project(uuid.NewString(), "ordinary", h.projDir)
	x1 := oldTask(h, uuid.NewString(), projectX.String("id"), "ordinary-one", "ses_x1")
	projectGone := h.project(uuid.NewString(), "gone-project", h.projDir)
	x2 := oldTask(h, uuid.NewString(), projectGone.String("id"), "gone-two", "ses_x2")
	if err := h.saveState([]any{projectX, projectGone}, x1, x2); err != nil {
		return err
	}
	x1Raw, _ := json.Marshal(x1)
	x2Raw, _ := json.Marshal(x2)
	if _, err := h.request(x1.String("id"), map[string]any{"type": "removeSession", "sessionId": x1.String("id")}); err != nil {
		return fmt.Errorf("removeSession: %w", err)
	}
	if _, err := h.request(x2.String("id"), map[string]any{"type": "removeProject", "projectId": projectGone.String("id")}); err != nil {
		return fmt.Errorf("removeProject: %w", err)
	}
	if err := h.closeDaemon(); err != nil {
		return err
	}
	if err := h.startDaemon("restart"); err != nil {
		return err
	}
	if session, err := h.hydrate(successT1ID); err != nil || session != nil {
		return fmt.Errorf("cleaned session resurrected after restart: %v", err)
	}
	var stale backendstore.Object
	if err := json.Unmarshal(successT1Old, &stale); err != nil {
		return err
	}
	normal := h.project(stale.String("project_id"), "normal", h.projDir)
	if err := h.saveState([]any{normal}, stale); err != nil {
		return fmt.Errorf("stale save after restart: %w", err)
	}
	if session, err := h.hydrate(successT1ID); err != nil || session != nil {
		return fmt.Errorf("old Save resurrected cleaned session: %v", err)
	}
	running := stale
	turnID := uuid.NewString()
	running.Set("turns", []any{map[string]any{"id": turnID, "turn_count": 1, "status": "running", "provider_turn_started": false, "started_at": now()}})
	running.Set("detail_loaded", true)
	record := model.NewSubmissionRecord(successT1ID, turnID, "isolated prompt", nil)
	submission, _ := json.Marshal(record)
	if _, err := h.request(successT1ID, map[string]any{"type": "acceptSubmission", "project": normal, "session": running, "submission": json.RawMessage(submission)}); err == nil {
		return errors.New("old Admit resurrected cleaned session")
	} else if err := failIf(err, "submission target removed"); err != nil {
		return err
	}
	gone, err := h.cleanup(successT1ID, midSec())
	if err != nil {
		return fmt.Errorf("cleanup of deleted id: %w", err)
	}
	if !isSkipped(gone) {
		return errors.New("cleanup of deleted id was not skipped")
	}
	h.checks["restart-tombstone-fence"] = true

	var oldX1 backendstore.Object
	if err := json.Unmarshal(x1Raw, &oldX1); err != nil {
		return err
	}
	if err := h.saveState([]any{projectX}, oldX1); err != nil {
		return fmt.Errorf("stale ordinary save: %w", err)
	}
	if session, err := h.hydrate(x1.String("id")); err != nil || session != nil {
		return fmt.Errorf("removed session resurrected: %v", err)
	}
	var oldX2 backendstore.Object
	if err := json.Unmarshal(x2Raw, &oldX2); err != nil {
		return err
	}
	if err := h.saveState([]any{projectGone}, oldX2); err != nil {
		return fmt.Errorf("stale project save: %w", err)
	}
	projects, err := h.projectIDs()
	if err != nil {
		return err
	}
	for _, id := range projects {
		if id == projectGone.String("id") {
			return errors.New("removed project resurrected")
		}
	}
	if session, err := h.hydrate(x2.String("id")); err != nil || session != nil {
		return fmt.Errorf("removed project session resurrected: %v", err)
	}
	h.checks["ordinary-remove-restart-fence"] = true
	return nil
}

func (h *harness) storeSuite() error {
	dir := h.round + "-cleanstore"
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	store, err := backendstore.Open(h.ctx, dir)
	if err != nil {
		return fmt.Errorf("open clean store: %w", err)
	}
	project := backendstore.Fields(map[string]any{"id": uuid.NewString(), "name": "cas", "path": dir, "created_at": 1})
	mkSession := func(updated int64) backendstore.Object {
		return backendstore.Fields(map[string]any{
			"id": uuid.NewString(), "project_id": project.String("id"), "title": "CAS", "provider": "opencode", "status": "idle",
			"created_at": oldSec(), "updated_at": updated, "last_reply_at": oldSec(), "detail_loaded": true,
			"messages": []any{map[string]any{"id": uuid.NewString(), "turn_id": uuid.NewString(), "role": "user", "content": "cas", "attachments": []any{}, "created_at": oldSec(), "streaming": false}},
			"turns":    []any{}, "transcript_blocks": []any{}, "queued_messages": []any{},
			"provider_cursor": map[string]any{"provider": "openCode", "sessionId": "ses_cas"}, "native_session_id": "ses_cas",
		})
	}
	saveCmd := func(session backendstore.Object) backendstore.Object {
		return backendstore.Fields(map[string]any{"projects": []any{project}, "sessions": []any{session}})
	}
	first := mkSession(oldSec())
	firstID := first.String("id")
	if _, err := store.Save(h.ctx, saveCmd(first)); err != nil {
		_ = store.Close()
		return fmt.Errorf("cas seed: %w", err)
	}
	_, fingerprint, err := store.TaskEditSnapshot(h.ctx, firstID)
	if err != nil {
		_ = store.Close()
		return fmt.Errorf("cas snapshot: %w", err)
	}
	if err := store.RemoveCleaned(h.ctx, map[string]string{firstID: fingerprint}, nil); err != nil {
		_ = store.Close()
		return fmt.Errorf("cas remove: %w", err)
	}
	payload, err := store.Hydrate(h.ctx, firstID)
	if err != nil {
		_ = store.Close()
		return err
	}
	if len(payload["session"]) == 0 || string(payload["session"]) != "null" {
		_ = store.Close()
		return errors.New("RemoveCleaned left a hydratable session")
	}
	if err := store.Close(); err != nil {
		return err
	}
	reopened, err := backendstore.Open(h.ctx, dir)
	if err != nil {
		return fmt.Errorf("reopen clean store: %w", err)
	}
	defer reopened.Close()
	var stale backendstore.Object
	raw, _ := json.Marshal(first)
	if err := json.Unmarshal(raw, &stale); err != nil {
		return err
	}
	stale.Set("updated_at", now())
	if _, err := reopened.Save(h.ctx, saveCmd(stale)); err != nil {
		return fmt.Errorf("save after tombstone reload: %w", err)
	}
	again, err := reopened.Hydrate(h.ctx, firstID)
	if err != nil {
		return err
	}
	if len(again["session"]) == 0 || string(again["session"]) != "null" {
		return errors.New("tombstone reload did not fence old Save")
	}
	h.checks["store-clean-tombstone-reload"] = true

	second := mkSession(oldSec())
	secondID := second.String("id")
	if _, err := reopened.Save(h.ctx, saveCmd(second)); err != nil {
		return fmt.Errorf("cas second seed: %w", err)
	}
	_, staleFP, err := reopened.TaskEditSnapshot(h.ctx, secondID)
	if err != nil {
		return err
	}
	second.Set("updated_at", now())
	if _, err := reopened.Save(h.ctx, saveCmd(second)); err != nil {
		return err
	}
	if err := reopened.RemoveCleaned(h.ctx, map[string]string{secondID: staleFP}, nil); err == nil {
		return errors.New("stale CAS remove was accepted")
	} else if !strings.Contains(err.Error(), "task changed during retention cleanup") {
		return fmt.Errorf("CAS conflict wrong error: %v", err)
	}
	h.checks["store-clean-cas-conflict"] = true
	return nil
}

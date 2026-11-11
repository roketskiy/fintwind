package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
)

type sessionActionState struct {
	generation   uint64
	operations   map[string]*sessionOperation
	confirmation *sessionDeleteConfirmation
}

type sessionOperation struct {
	kind       string
	phase      string
	generation uint64
	client     *daemon.Client
	identity   daemon.Identity
	project    model.Project
	session    *model.Session
	message    string
	history    *messageEditState
}

// Deleting the provider while another input is being admitted can destroy the
// conversation that input targets. Submission and snapshot entry points share
// this barrier; neither may clear an unconfirmed operation.
func (a *app) sessionActionBlocksInput(id string) bool {
	operation := a.sessionActions.operations[id]
	return operation != nil && (operation.kind == "delete" || operation.kind == "history-edit")
}

func (a *app) sessionActionBlocksSnapshot(id string) bool {
	operation := a.sessionActions.operations[id]
	return operation != nil && (operation.kind == "delete" || operation.kind == "history-edit")
}

func (a *app) beginSessionOperation(id, kind string) *sessionOperation {
	if existing := a.sessionActions.operations[id]; existing != nil {
		a.status = existing.message
		return nil
	}
	session := a.summarySession(id)
	if session == nil || a.client == nil || a.connection.phase != "online" {
		a.status = tr("Connect to the daemon before changing this task")
		return nil
	}
	project := a.projectForID(session.ProjectID)
	if project == nil || project.Path == "" || !sessionProjectIsLocal(project) {
		a.status = tr("This task's workspace is unavailable on this device")
		return nil
	}
	if live := a.eventSession(id); live != nil {
		session = live
		state := a.stateForSession(live)
		if state.preparing || state.admissionBusy || hasUnsettledSubmissionReceipts(live) || len(a.pendingSteerList(live)) > 0 {
			a.status = tr("Resolve the pending submission before changing this task")
			return nil
		}
	}
	if session.Busy() {
		a.status = tr("Stop the task before changing it")
		return nil
	}
	if kind == "delete" && (a.snapshots.inFlight || len(a.snapshots.pending) > 0 || len(a.snapshots.admissions) > 0) {
		a.status = tr("Wait for pending saves before deleting this task")
		return nil
	}
	a.sessionActions.generation++
	operation := &sessionOperation{
		kind: kind, phase: "loading", generation: a.sessionActions.generation,
		client: a.client, identity: a.client.Identity(), project: *project,
		session: a.eventSession(id),
		message: "Task operation in progress",
	}
	if a.sessionActions.operations == nil {
		a.sessionActions.operations = make(map[string]*sessionOperation)
	}
	a.sessionActions.operations[id] = operation
	return operation
}

func sessionProjectIsLocal(project *model.Project) bool {
	return project.EnvironmentID == "" || project.EnvironmentID == "00000000-0000-0000-0000-000000000000"
}

func (a *app) sessionOperationCurrent(id string, operation *sessionOperation) bool {
	current := a.sessionActions.operations[id]
	if current != operation || current.generation != operation.generation || a.client != operation.client || a.client.Identity() != operation.identity {
		return false
	}
	session := a.summarySession(id)
	if session == nil || session.ProjectID != operation.project.ID {
		return false
	}
	if operation.session != nil && a.eventSession(id) != operation.session {
		return false
	}
	project := a.projectForID(session.ProjectID)
	return project != nil && *project == operation.project
}

func hydrateSessionOperation(ctx context.Context, client *daemon.Client, id string) (*model.Session, map[string]any, error) {
	payload, err := client.Request(ctx, map[string]any{
		"type": "hydrateSession", "sessionId": id,
		"environmentId": "00000000-0000-0000-0000-000000000000",
	})
	if err != nil {
		return nil, nil, err
	}
	var response struct {
		Type    string          `json:"type"`
		Session json.RawMessage `json:"session"`
	}
	if err := json.Unmarshal(payload, &response); err != nil {
		return nil, nil, err
	}
	var session model.Session
	var raw map[string]any
	if err := json.Unmarshal(response.Session, &session); err != nil {
		return nil, nil, err
	}
	decoder := json.NewDecoder(bytes.NewReader(response.Session))
	decoder.UseNumber()
	if err := decoder.Decode(&raw); err != nil {
		return nil, nil, err
	}
	if raw == nil || session.ID != id {
		return nil, nil, fmt.Errorf("the daemon did not return the requested task")
	}
	return &session, raw, nil
}

func sessionOperationNative(session *model.Session) string {
	if session.Provider != "opencode" {
		return ""
	}
	if session.NativeSessionID != nil && *session.NativeSessionID != "" {
		return *session.NativeSessionID
	}
	return nativeSessionID(session.ProviderCursor)
}

func sessionOperationDirectory(project model.Project, raw map[string]any) (string, error) {
	workspace, _ := raw["workspace"].(map[string]any)
	if workspace == nil {
		return project.Path, nil
	}
	kind, _ := workspace["kind"].(string)
	switch kind {
	case "local", "newWorktree":
		return project.Path, nil
	case "worktree":
		if path, _ := workspace["path"].(string); strings.TrimSpace(path) != "" {
			return path, nil
		}
	}
	return "", fmt.Errorf("the task workspace is invalid")
}

func sessionOperationAck(payload json.RawMessage) bool {
	var result struct {
		Type string `json:"type"`
	}
	return json.Unmarshal(payload, &result) == nil && result.Type == "ack"
}

func (a *app) finishSessionOperationError(id string, operation *sessionOperation, stage string, err error, wrote bool) {
	a.post(func() {
		if a.sessionActions.operations[id] != operation {
			return
		}
		message := stage + ": " + err.Error()
		if wrote {
			operation.phase = "unknown"
			message = stage + " not confirmed. Check the task before trying again: " + err.Error()
			operation.message = message
		} else {
			delete(a.sessionActions.operations, id)
		}
		if a.client == operation.client && operation.client.Identity() == operation.identity {
			a.status = message
		}
	})
}

func (a *app) runSessionOperation(id, title string, updatedAt uint64, operation *sessionOperation) {
	ctx, cancel := context.WithTimeout(a.lifetime, 120*time.Second)
	defer cancel()
	session, raw, err := hydrateSessionOperation(ctx, operation.client, id)
	if err != nil {
		a.finishSessionOperationError(id, operation, "Cannot load task", err, false)
		return
	}
	directory, err := sessionOperationDirectory(operation.project, raw)
	if err != nil {
		a.finishSessionOperationError(id, operation, "Cannot change task", err, false)
		return
	}
	if err := a.onUI(func() error {
		if !a.sessionOperationCurrent(id, operation) || session.ProjectID != operation.project.ID || session.Busy() {
			return fmt.Errorf("the task or connection changed")
		}
		if current := a.eventSession(id); current != nil {
			state := a.stateForSession(current)
			if current.Busy() || state.preparing || state.admissionBusy || hasUnsettledSubmissionReceipts(current) || len(a.pendingSteerList(current)) > 0 {
				return fmt.Errorf("the task has a pending submission")
			}
			if sessionOperationNative(current) != sessionOperationNative(session) {
				return fmt.Errorf("the provider session changed")
			}
		}
		operation.phase = "writing"
		return nil
	}); err != nil {
		a.finishSessionOperationError(id, operation, "Cannot change task", err, false)
		return
	}
	native := sessionOperationNative(session)
	if native != "" {
		binary := configuredOpencodeBinary(ctx, operation.client)
		if err := a.onUI(func() error {
			if !a.sessionOperationCurrent(id, operation) {
				return fmt.Errorf("the task or connection changed before the provider operation")
			}
			return nil
		}); err != nil {
			a.finishSessionOperationError(id, operation, "Cannot change task", err, false)
			return
		}
		command := map[string]any{
			"type": "renameProviderSession", "binary": binary,
			"directory": directory, "sessionId": native, "title": title,
		}
		if operation.kind == "delete" {
			command["type"] = "deleteProviderSession"
			delete(command, "title")
		}
		payload, err := operation.client.Request(ctx, command)
		if err == nil && !sessionOperationAck(payload) {
			err = fmt.Errorf("the daemon returned an invalid acknowledgement")
		}
		if err != nil {
			a.finishSessionOperationError(id, operation, "Provider "+operation.kind, err, true)
			return
		}
	}
	if operation.kind == "delete" {
		stage := "App record removal"
		if native != "" {
			stage = "Provider deleted; app record removal"
		}
		if err := a.onUI(func() error {
			operation.phase = "providerDeleted"
			operation.message = stage + tr(" is pending.")
			if !a.sessionOperationCurrent(id, operation) {
				return fmt.Errorf("the connection or task changed after provider deletion")
			}
			return nil
		}); err != nil {
			a.finishSessionOperationError(id, operation, stage, err, true)
			return
		}
		payload, err := operation.client.RequestFor(ctx, id, "", map[string]any{"type": "removeSession"})
		if err == nil && !sessionOperationAck(payload) {
			err = fmt.Errorf("the daemon returned an invalid acknowledgement")
		}
		if err != nil {
			a.finishSessionOperationError(id, operation, stage, err, true)
			return
		}
		a.post(func() {
			if !a.sessionOperationCurrent(id, operation) {
				operation.phase, operation.message = "unknown", tr("Deletion completed on the previous connection. Reload tasks to check.")
				return
			}
			delete(a.sessionActions.operations, id)
			a.removeSessionLocal(id)
			a.status = tr("Task deleted")
		})
		return
	}
	a.post(func() {
		if !a.sessionOperationCurrent(id, operation) {
			operation.phase, operation.message = "unknown", tr("Desktop title save was not applied because the task or connection changed. Check the provider title.")
			return
		}
		// A newer live transcript wins over the hydration used to resolve the
		// native target. Only a fully hydrated record enters the snapshot queue.
		current := a.eventSession(id)
		if current == nil {
			current = session
		}
		state := a.stateForSession(current)
		if state.raw == nil {
			state.raw = raw
		}
		if a.detail == current && a.detailRaw == nil {
			a.detailRaw = state.raw
		}
		current.Title, current.UpdatedAt = title, updatedAt
		a.syncSummary(current)
		a.rebuildRows()
		delete(a.sessionActions.operations, id)
		a.status = tr("Desktop title save queued")
		if native != "" {
			a.status = tr("Provider title updated; desktop save queued")
		}
		a.saveSessionSnapshot(current)
	})
}

package main

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

// runRealDaemon starts the true daemon inside an ownedprocess Job and drives
// handshake, loadTaskState, saveTaskState and hydrate over the real wire.
// No provider call, no prompt, no paid traffic. Scope stays "realDaemon".
func runRealDaemon(ctx context.Context, executable, roundDir string) []caseResult {
	const scope = "realDaemon"
	handshake := "real daemon handshake and ready line"
	if _, err := os.Stat(executable); err != nil {
		return []caseResult{{
			Name: handshake, Scope: scope, Status: "fail",
			Detail: fmt.Sprintf("daemon binary missing: %s", executable),
		}}
	}

	dataDir := filepath.Join(roundDir, "daemon-data")
	if err := os.MkdirAll(dataDir, 0o755); err != nil {
		return []caseResult{{Name: handshake, Scope: scope, Status: "fail", Detail: fmt.Sprintf("data dir: %v", err)}}
	}
	token, err := randomToken()
	if err != nil {
		return []caseResult{{Name: handshake, Scope: scope, Status: "fail", Detail: fmt.Sprintf("token: %v", err)}}
	}
	owned, err := ownedprocess.Start(executable,
		[]string{"--bind", "127.0.0.1:0", "--data-dir", dataDir, "--parent-pid", strconv.Itoa(os.Getpid())},
		roundDir,
		map[string]string{"FINTWIND_DAEMON_TOKEN": token})
	if err != nil {
		return []caseResult{{Name: handshake, Scope: scope, Status: "fail", Detail: fmt.Sprintf("owned start: %v", err)}}
	}
	defer owned.Close()

	ready, err := waitReadyLine(ctx, owned.StdoutPath, 30*time.Second)
	if err != nil {
		return []caseResult{{Name: handshake, Scope: scope, Status: "fail", Detail: fmt.Sprintf("ready line: %v", err)}}
	}
	if ready.ProtocolVersion != daemon.ProtocolVersion {
		return []caseResult{{Name: handshake, Scope: scope, Status: "fail", Detail: fmt.Sprintf("daemon protocol %d, client %d", ready.ProtocolVersion, daemon.ProtocolVersion)}}
	}
	if ready.PID != int(owned.PID) || ready.EnvironmentID == "" {
		return []caseResult{{Name: handshake, Scope: scope, Status: "fail", Detail: "ready identity does not match the owned daemon"}}
	}

	dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	client, err := daemon.Dial(dialCtx, ready.Address, token)
	cancel()
	if err != nil {
		return []caseResult{{Name: handshake, Scope: scope, Status: "fail", Detail: fmt.Sprintf("dial: %v", err)}}
	}
	defer client.Close()

	cases := []caseResult{{
		Name: handshake, Scope: scope, Status: "pass",
		Detail: fmt.Sprintf("protocol %d, environment set, pid owned by job", daemon.ProtocolVersion),
	}}

	request := func(label string, command map[string]any, out any) (caseResult, json.RawMessage, bool) {
		callCtx, cancel := context.WithTimeout(ctx, 20*time.Second)
		defer cancel()
		payload, err := client.Request(callCtx, command)
		if err != nil {
			return caseResult{Name: label, Scope: scope, Status: "fail", Detail: fmt.Sprintf("%v", err)}, nil, false
		}
		if out != nil {
			if err := json.Unmarshal(payload, out); err != nil {
				return caseResult{Name: label, Scope: scope, Status: "fail", Detail: fmt.Sprintf("decode: %v", err)}, nil, false
			}
		}
		return caseResult{}, payload, true
	}

	var state model.TaskState
	if c, _, ok := request("real loadTaskState", map[string]any{"type": "loadTaskState"}, &state); !ok {
		cases = append(cases, c)
		return cases
	}
	cases = append(cases, caseResult{Name: "real loadTaskState", Scope: scope, Status: "pass",
		Detail: fmt.Sprintf("%d projects, %d sessions", len(state.Projects), len(state.Sessions))})

	now := uint64(time.Now().Unix())
	projectID := uuid.NewString()
	sessionID := uuid.NewString()
	save := map[string]any{
		"type": "saveTaskState",
		"projects": []any{map[string]any{
			"id": projectID, "name": "verify", "path": dataDir,
			"environment_id": "", "created_at": now,
		}},
		"liveSessionIds": []string{sessionID},
		"sessions": []any{map[string]any{
			"id": sessionID, "title": "verify draft", "project_id": projectID,
			"provider": "opencode", "runtime_mode": "fullAccess", "interaction_mode": "build",
			"status": "idle", "created_at": now, "updated_at": now, "mermaid_prompt_version": "v4",
		}},
	}
	if c, _, ok := request("real saveTaskState", save, nil); !ok {
		cases = append(cases, c)
		return cases
	}
	cases = append(cases, caseResult{Name: "real saveTaskState", Scope: scope, Status: "pass", Detail: "draft project and session accepted"})

	state = model.TaskState{}
	if c, _, ok := request("real loadTaskState round-trip", map[string]any{"type": "loadTaskState"}, &state); !ok {
		cases = append(cases, c)
		return cases
	}
	if len(state.Projects) != 1 || len(state.Sessions) != 1 {
		cases = append(cases, caseResult{Name: "real loadTaskState round-trip", Scope: scope, Status: "fail",
			Detail: fmt.Sprintf("want 1 project and 1 session, got %d and %d", len(state.Projects), len(state.Sessions))})
		return cases
	}
	cases = append(cases, caseResult{Name: "real loadTaskState round-trip", Scope: scope, Status: "pass", Detail: "saved draft round-trips"})

	var detail model.SessionDetail
	hydrateCmd := map[string]any{
		"type": "hydrateSession", "sessionId": sessionID,
		"environmentId": "00000000-0000-0000-0000-000000000000",
	}
	if c, _, ok := request("real hydrateSession", hydrateCmd, &detail); !ok {
		cases = append(cases, c)
		return cases
	}
	if detail.Session == nil {
		cases = append(cases, caseResult{Name: "real hydrateSession", Scope: scope, Status: "fail", Detail: "no session in hydrate response"})
		return cases
	}
	cases = append(cases, caseResult{Name: "real hydrateSession", Scope: scope, Status: "pass",
		Detail: fmt.Sprintf("%d messages, %d turns", len(detail.Session.Messages), len(detail.Session.Turns))})
	checkpoint := save["sessions"].([]any)[0].(map[string]any)
	messageID, runtimeID, epoch := uuid.NewString(), uuid.NewString(), uuid.NewString()
	checkpoint["messages"] = []any{map[string]any{
		"id": messageID, "role": "assistant", "content": "checkpoint body 7391", "created_at": now, "streaming": false,
	}}
	checkpoint["runtime_event_cursor"] = map[string]any{"runtime_id": runtimeID, "epoch": epoch, "sequence": 42}
	checkpointCommand := map[string]any{
		"type": "saveTaskStateCheckpoint", "projects": save["projects"],
		"sessions": []any{checkpoint}, "skeletonSessionIds": []string{},
	}
	if c, _, ok := request("real checkpoint save", checkpointCommand, nil); !ok {
		return append(cases, c)
	}
	detail = model.SessionDetail{}
	if c, _, ok := request("real checkpoint hydrate", hydrateCmd, &detail); !ok {
		return append(cases, c)
	}
	if detail.Session == nil || len(detail.Session.Messages) != 1 || detail.Session.Messages[0].Content != "checkpoint body 7391" ||
		detail.Session.RuntimeEventCursor == nil || detail.Session.RuntimeEventCursor.RuntimeID != runtimeID ||
		detail.Session.RuntimeEventCursor.Epoch != epoch || detail.Session.RuntimeEventCursor.Sequence != 42 {
		return append(cases, caseResult{Name: "real checkpoint hydrate", Scope: scope, Status: "fail", Detail: "checkpoint body or applied cursor did not round-trip"})
	}
	cases = append(cases, caseResult{Name: "real checkpoint hydrate", Scope: scope, Status: "pass", Detail: "body and runtime/epoch/sequence round-trip exactly"})
	if c, _, ok := request("real skeleton checkpoint", map[string]any{
		"type": "saveTaskStateCheckpoint", "projects": []any{}, "sessions": []any{}, "skeletonSessionIds": []string{sessionID},
	}, nil); !ok {
		return append(cases, c)
	}
	detail = model.SessionDetail{}
	if c, _, ok := request("real skeleton preserves body", hydrateCmd, &detail); !ok {
		return append(cases, c)
	}
	if detail.Session == nil || len(detail.Session.Messages) != 1 || detail.Session.Messages[0].ID != messageID || detail.Session.Messages[0].Content != "checkpoint body 7391" {
		return append(cases, caseResult{Name: "real skeleton preserves body", Scope: scope, Status: "fail", Detail: "skeleton checkpoint lost the saved body"})
	}
	cases = append(cases, caseResult{Name: "real skeleton preserves body", Scope: scope, Status: "pass", Detail: "skeleton checkpoint preserves full transcript"})
	attachmentPath := filepath.Join(dataDir, "verify-attachment.txt")
	if err := os.WriteFile(attachmentPath, []byte("attachment payload 4821"), 0o600); err != nil {
		return append(cases, caseResult{Name: "real attachment checkpoint", Scope: scope, Status: "fail", Detail: err.Error()})
	}
	var imported struct {
		Attachment struct {
			Reference string `json:"reference"`
			Path      string `json:"path"`
			Name      string `json:"name"`
			IsDir     bool   `json:"is_dir"`
		} `json:"attachment"`
	}
	if c, _, ok := request("real attachment import", map[string]any{"type": "importPathAttachment", "path": attachmentPath}, &imported); !ok {
		return append(cases, c)
	}
	if imported.Attachment.Reference == "" || imported.Attachment.Path == "" {
		return append(cases, caseResult{Name: "real attachment import", Scope: scope, Status: "fail", Detail: "missing staged attachment reference or path"})
	}
	reference := imported.Attachment.Reference
	attachments := []model.Attachment{{Path: imported.Attachment.Path, Mention: attachmentPath, Name: imported.Attachment.Name, IsDir: imported.Attachment.IsDir, BlobReference: &reference}}
	checkpoint["messages"].([]any)[0].(map[string]any)["attachments"] = attachments
	checkpoint["queued_messages"] = []model.QueuedMessage{{ID: uuid.NewString(), Content: "queued body 4821", CreatedAt: now, Attachments: attachments}}
	if c, _, ok := request("real attachment checkpoint save", checkpointCommand, nil); !ok {
		return append(cases, c)
	}
	detail = model.SessionDetail{}
	if c, _, ok := request("real attachment checkpoint hydrate", hydrateCmd, &detail); !ok {
		return append(cases, c)
	}
	if detail.Session == nil || len(detail.Session.Messages) != 1 || len(detail.Session.Messages[0].Attachments) != 1 || len(detail.Session.QueuedMessages) != 1 || len(detail.Session.QueuedMessages[0].Attachments) != 1 {
		return append(cases, caseResult{Name: "real attachment checkpoint hydrate", Scope: scope, Status: "fail", Detail: "message or queued attachment missing"})
	}
	messageAttachment := detail.Session.Messages[0].Attachments[0]
	queued := detail.Session.QueuedMessages[0]
	queuedAttachment := queued.Attachments[0]
	if messageAttachment.Path != imported.Attachment.Path || messageAttachment.BlobReference == nil || *messageAttachment.BlobReference != reference ||
		messageAttachment.Name != imported.Attachment.Name || queued.Content != "queued body 4821" || queued.CreatedAt != now ||
		queuedAttachment.Path != messageAttachment.Path || queuedAttachment.BlobReference == nil || *queuedAttachment.BlobReference != reference {
		return append(cases, caseResult{Name: "real attachment checkpoint hydrate", Scope: scope, Status: "fail", Detail: "attachment metadata or queued creation time changed"})
	}
	cases = append(cases, caseResult{Name: "real attachment checkpoint hydrate", Scope: scope, Status: "pass", Detail: "staged blob references and queued creation time round-trip exactly"})
	turnID := uuid.NewString()
	inputID := fmt.Sprintf("msg_%012x%s", (uint64(time.Now().UnixMilli())*0x1000)&0xffffffffffff, strings.ReplaceAll(uuid.NewString(), "-", "")[:14])
	receipt := map[string]any{"id": turnID, "input_id": inputID, "state": "preparing", "created_at": now * 1000, "updated_at": now * 1000, "native_session_id": nil}
	checkpoint["status"], checkpoint["detail_loaded"] = "working", true
	checkpoint["turns"] = []any{map[string]any{"id": turnID, "turn_count": 1, "status": "running", "started_at": now, "completed_at": nil, "submission": receipt}}
	checkpoint["messages"] = append(checkpoint["messages"].([]any), map[string]any{"id": uuid.NewString(), "role": "user", "content": "admission body 6842", "turn_id": turnID, "streaming": false, "created_at": now})
	record := map[string]any{"session_id": sessionID, "receipt": receipt, "prompt": "admission body 6842", "files": []any{}}
	accept := map[string]any{"type": "acceptSubmission", "project": save["projects"].([]any)[0], "session": checkpoint, "submission": record}
	var admitted struct {
		Receipt struct {
			ID      string `json:"id"`
			InputID string `json:"input_id"`
			State   string `json:"state"`
		} `json:"receipt"`
	}
	callCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	accepted, err := client.RequestFor(callCtx, sessionID, "", accept)
	cancel()
	if err != nil || json.Unmarshal(accepted, &admitted) != nil || admitted.Receipt.ID != turnID || admitted.Receipt.InputID != inputID || admitted.Receipt.State != "preparing" {
		return append(cases, caseResult{Name: "real atomic admission", Scope: scope, Status: "fail", Detail: fmt.Sprintf("admission identity failed: %v", err)})
	}
	cases = append(cases, caseResult{Name: "real atomic admission", Scope: scope, Status: "pass", Detail: "frozen turn and input identity admitted without provider execution"})
	record["prompt"] = "conflicting body 9999"
	callCtx, cancel = context.WithTimeout(ctx, 15*time.Second)
	_, err = client.RequestFor(callCtx, sessionID, "", accept)
	cancel()
	if err == nil {
		return append(cases, caseResult{Name: "real admission rejects identity conflict", Scope: scope, Status: "fail", Detail: "changed prompt accepted under the same submission identity"})
	}
	cases = append(cases, caseResult{Name: "real admission rejects identity conflict", Scope: scope, Status: "pass", Detail: "same identity with changed prompt refused"})
	callCtx, cancel = context.WithTimeout(ctx, 15*time.Second)
	abandoned, err := client.RequestFor(callCtx, sessionID, "", map[string]any{"type": "abandonSubmission", "submissionId": turnID})
	cancel()
	if err != nil || json.Unmarshal(abandoned, &admitted) != nil || admitted.Receipt.State != "notSent" {
		return append(cases, caseResult{Name: "real admission abandon", Scope: scope, Status: "fail", Detail: fmt.Sprintf("preparing submission not abandoned: %v", err)})
	}
	callCtx, cancel = context.WithTimeout(ctx, 15*time.Second)
	dispatched, err := client.RequestFor(callCtx, sessionID, "", map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]any{"kind": "local"}})
	cancel()
	if err != nil || json.Unmarshal(dispatched, &admitted) != nil || admitted.Receipt.State != "notSent" {
		return append(cases, caseResult{Name: "real abandoned dispatch stays notSent", Scope: scope, Status: "fail", Detail: fmt.Sprintf("abandoned input changed outcome: %v", err)})
	}
	cases = append(cases, caseResult{Name: "real abandoned dispatch stays notSent", Scope: scope, Status: "pass", Detail: "abandon wins before dispatch, no runtime or provider started"})
	if c, _, ok := request("real stale receipt checkpoint", checkpointCommand, nil); !ok {
		return append(cases, c)
	}
	detail = model.SessionDetail{}
	if c, _, ok := request("real stale receipt hydrate", hydrateCmd, &detail); !ok {
		return append(cases, c)
	}
	if detail.Session == nil || len(detail.Session.Turns) != 1 || json.Unmarshal(detail.Session.Turns[0].Submission, &admitted.Receipt) != nil || admitted.Receipt.ID != turnID || admitted.Receipt.InputID != inputID || admitted.Receipt.State != "notSent" {
		return append(cases, caseResult{Name: "real stale checkpoint preserves authoritative receipt", Scope: scope, Status: "fail", Detail: "old preparing checkpoint replaced the authoritative notSent receipt"})
	}
	cases = append(cases, caseResult{Name: "real stale checkpoint preserves authoritative receipt", Scope: scope, Status: "pass", Detail: "old preparing projection cannot overwrite the daemon submission table"})
	client.Close()
	if err := owned.Close(); err != nil {
		return append(cases, caseResult{Name: "real admission survives daemon restart", Scope: scope, Status: "fail", Detail: err.Error()})
	}
	restartDir := filepath.Join(roundDir, "daemon-restart")
	if err := os.MkdirAll(restartDir, 0o755); err != nil {
		return append(cases, caseResult{Name: "real admission survives daemon restart", Scope: scope, Status: "fail", Detail: err.Error()})
	}
	restarted, err := ownedprocess.Start(executable,
		[]string{"--bind", "127.0.0.1:0", "--data-dir", dataDir, "--parent-pid", strconv.Itoa(os.Getpid())},
		restartDir, map[string]string{"FINTWIND_DAEMON_TOKEN": token})
	if err != nil {
		return append(cases, caseResult{Name: "real admission survives daemon restart", Scope: scope, Status: "fail", Detail: err.Error()})
	}
	defer restarted.Close()
	restartedReady, err := waitReadyLine(ctx, restarted.StdoutPath, 20*time.Second)
	if err != nil || restartedReady.PID != int(restarted.PID) || restartedReady.EnvironmentID != ready.EnvironmentID {
		return append(cases, caseResult{Name: "real admission survives daemon restart", Scope: scope, Status: "fail", Detail: fmt.Sprintf("restart identity: %v", err)})
	}
	dialCtx, cancel = context.WithTimeout(ctx, 10*time.Second)
	restoredClient, err := daemon.Dial(dialCtx, restartedReady.Address, token)
	cancel()
	if err != nil {
		return append(cases, caseResult{Name: "real admission survives daemon restart", Scope: scope, Status: "fail", Detail: err.Error()})
	}
	defer restoredClient.Close()
	callCtx, cancel = context.WithTimeout(ctx, 10*time.Second)
	restoredPayload, err := restoredClient.Request(callCtx, hydrateCmd)
	cancel()
	detail = model.SessionDetail{}
	if err != nil || json.Unmarshal(restoredPayload, &detail) != nil || detail.Session == nil || len(detail.Session.Turns) != 1 || len(detail.Session.Messages) != 2 {
		return append(cases, caseResult{Name: "real admission survives daemon restart", Scope: scope, Status: "fail", Detail: fmt.Sprintf("restart hydrate: %v", err)})
	}
	if json.Unmarshal(detail.Session.Turns[0].Submission, &admitted.Receipt) != nil || admitted.Receipt.ID != turnID || admitted.Receipt.InputID != inputID || admitted.Receipt.State != "notSent" || detail.Session.Messages[1].Content != "admission body 6842" || len(detail.Session.Messages[0].Attachments) != 1 || detail.Session.Messages[0].Attachments[0].BlobReference == nil || *detail.Session.Messages[0].Attachments[0].BlobReference != reference {
		return append(cases, caseResult{Name: "real admission survives daemon restart", Scope: scope, Status: "fail", Detail: "restart lost receipt identity, saved input or staged attachment"})
	}
	callCtx, cancel = context.WithTimeout(ctx, 10*time.Second)
	replayed, err := restoredClient.RequestFor(callCtx, sessionID, "", map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]any{"kind": "local"}})
	cancel()
	if err != nil || json.Unmarshal(replayed, &admitted) != nil || admitted.Receipt.State != "notSent" {
		return append(cases, caseResult{Name: "real admission survives daemon restart", Scope: scope, Status: "fail", Detail: fmt.Sprintf("restart changed dispatch outcome: %v", err)})
	}
	cases = append(cases, caseResult{Name: "real admission survives daemon restart", Scope: scope, Status: "pass", Detail: "new owned daemon restores turn/input/notSent, body and attachment; dispatch still notSent"})
	return cases
}

type readyLine struct {
	Address         string `json:"address"`
	ProtocolVersion int    `json:"protocolVersion"`
	PID             int    `json:"pid"`
	EnvironmentID   string `json:"environmentId"`
}

func waitReadyLine(ctx context.Context, stdoutPath string, timeout time.Duration) (*readyLine, error) {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		default:
		}
		if ready, ok := readFirstLine(stdoutPath); ok {
			return ready, nil
		}
		time.Sleep(100 * time.Millisecond)
	}
	return nil, fmt.Errorf("no ready line in %s within %s", stdoutPath, timeout)
}

func readFirstLine(stdoutPath string) (*readyLine, bool) {
	file, err := os.Open(stdoutPath)
	if err != nil {
		return nil, false
	}
	defer file.Close()
	line, err := bufio.NewReader(file).ReadString('\n')
	if err != nil || len(line) == 0 {
		return nil, false
	}
	var raw struct {
		Address              string `json:"address"`
		ProtocolVersion      int    `json:"protocolVersion"`
		ProtocolVersionSnake int    `json:"protocol_version"`
		PID                  int    `json:"pid"`
		EnvironmentID        string `json:"environmentId"`
	}
	if err := json.Unmarshal([]byte(line), &raw); err != nil {
		return nil, false
	}
	if raw.Address == "" {
		return nil, false
	}
	version := raw.ProtocolVersion
	if version == 0 {
		version = raw.ProtocolVersionSnake
	}
	return &readyLine{Address: raw.Address, ProtocolVersion: version, PID: raw.PID, EnvironmentID: raw.EnvironmentID}, true
}

func randomToken() (string, error) {
	buffer := make([]byte, 16)
	if _, err := rand.Read(buffer); err != nil {
		return "", err
	}
	return hex.EncodeToString(buffer), nil
}

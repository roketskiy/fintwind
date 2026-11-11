// Admission wire verification against the fixture daemon only.
//
// It drives the true admission state machine inside an ownedprocess Job:
// Accept (snake_case record and receipt), Abandon, Dispatch (camelCase
// submissionId, workspace {kind:local}), Start (envelope runtime binding),
// plus the control-plane GET /submissions projection. No provider call,
// no prompt execution beyond the fixture stream, no real daemon traffic.
// Scope stays "admissionFixture".
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

const admissionScope = "admissionFixture"

func runAdmissionFixture(ctx context.Context, fixturePath, roundDir string) []caseResult {
	handshake := "admission fixture handshake and ready line"
	if _, err := os.Stat(fixturePath); err != nil {
		return []caseResult{{Name: handshake, Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("fixture binary missing: %s", fixturePath)}}
	}
	dataDir := filepath.Join(roundDir, "admission-data")
	if err := os.MkdirAll(dataDir, 0o755); err != nil {
		return []caseResult{{Name: handshake, Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("data dir: %v", err)}}
	}
	token, err := randomToken()
	if err != nil {
		return []caseResult{{Name: handshake, Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("token: %v", err)}}
	}
	owned, err := ownedprocess.Start(fixturePath,
		[]string{"--bind", "127.0.0.1:0", "--data-dir", dataDir, "--parent-pid", strconv.Itoa(os.Getpid())},
		roundDir,
		map[string]string{"FINTWIND_DAEMON_TOKEN": token})
	if err != nil {
		return []caseResult{{Name: handshake, Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("owned start: %v", err)}}
	}
	defer owned.Close()

	ready, err := waitReadyLine(ctx, owned.StdoutPath, 30*time.Second)
	if err != nil {
		return []caseResult{{Name: handshake, Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("ready line: %v", err)}}
	}
	if ready.ProtocolVersion != daemon.ProtocolVersion {
		return []caseResult{{Name: handshake, Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("fixture protocol %d, client %d", ready.ProtocolVersion, daemon.ProtocolVersion)}}
	}
	if ready.PID != int(owned.PID) || ready.EnvironmentID == "" {
		return []caseResult{{Name: handshake, Scope: admissionScope, Status: "fail",
			Detail: "ready identity does not match the owned fixture"}}
	}

	dialCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	client, err := daemon.Dial(dialCtx, ready.Address, token)
	cancel()
	if err != nil {
		return []caseResult{{Name: handshake, Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("dial: %v", err)}}
	}
	defer client.Close()

	pumpCtx, pumpStop := context.WithCancel(context.Background())
	var pumpWG sync.WaitGroup
	pumpWG.Add(1)
	go admissionPump(pumpCtx, client, &pumpWG)
	defer func() { pumpStop(); pumpWG.Wait() }()

	cases := []caseResult{{Name: handshake, Scope: admissionScope, Status: "pass",
		Detail: fmt.Sprintf("protocol %d, fixture owned by job", daemon.ProtocolVersion)}}
	for _, c := range runAdmissionCases(ctx, client, dataDir) {
		cases = append(cases, c)
		if c.Status != "pass" {
			return cases
		}
	}
	pumpStop()
	pumpWG.Wait()
	cases = append(cases, runAdmissionCancelCase(ctx, client, dataDir))
	return cases
}

func admissionPump(ctx context.Context, client *daemon.Client, wg *sync.WaitGroup) {
	defer wg.Done()
	for {
		select {
		case <-ctx.Done():
			return
		case event, ok := <-client.Events():
			if !ok {
				return
			}
			switch event.Kind {
			case "permission":
				var payload struct {
					RequestID string `json:"requestId"`
				}
				if json.Unmarshal(event.Payload, &payload) != nil || payload.RequestID == "" {
					continue
				}
				call, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				_, _ = client.RequestFor(call, event.SessionID, event.RuntimeID,
					map[string]any{"type": "respond", "requestId": payload.RequestID, "optionId": "allow"})
				cancel()
			case "userInputRequested":
				var payload struct {
					RequestID string `json:"requestId"`
				}
				if json.Unmarshal(event.Payload, &payload) != nil || payload.RequestID == "" {
					continue
				}
				call, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				_, _ = client.RequestFor(call, event.SessionID, event.RuntimeID,
					map[string]any{"type": "respondUserInput", "requestId": payload.RequestID, "answers": []any{}})
				cancel()
			}
		case <-client.Done():
			return
		}
	}
}

func admissionCall(ctx context.Context, client *daemon.Client, sessionID, runtimeID string, command map[string]any) (json.RawMessage, error) {
	call, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	return client.RequestFor(call, sessionID, runtimeID, command)
}

type admissionReceipt struct {
	Raw       json.RawMessage
	ID        string
	InputID   string
	State     string
	CreatedAt uint64
	UpdatedAt uint64
}

func decodeAdmissionReceipt(payload json.RawMessage) (admissionReceipt, error) {
	var envelope struct {
		Type    string          `json:"type"`
		Receipt json.RawMessage `json:"receipt"`
	}
	if err := json.Unmarshal(payload, &envelope); err != nil {
		return admissionReceipt{}, fmt.Errorf("decode submissionSaved: %w", err)
	}
	if envelope.Type != "submissionSaved" {
		return admissionReceipt{}, fmt.Errorf("want submissionSaved, got %q", envelope.Type)
	}
	var receipt struct {
		ID        string  `json:"id"`
		InputID   string  `json:"input_id"`
		State     string  `json:"state"`
		CreatedAt uint64  `json:"created_at"`
		UpdatedAt uint64  `json:"updated_at"`
		Native    *string `json:"native_session_id"`
	}
	if err := json.Unmarshal(envelope.Receipt, &receipt); err != nil {
		return admissionReceipt{}, fmt.Errorf("decode receipt: %w", err)
	}
	raw := string(envelope.Receipt)
	if !strings.Contains(raw, `"input_id"`) || strings.Contains(raw, `"inputId"`) {
		return admissionReceipt{}, fmt.Errorf("receipt wire keys must be snake_case")
	}
	return admissionReceipt{
		Raw: envelope.Receipt, ID: receipt.ID,
		InputID: receipt.InputID, State: receipt.State,
		CreatedAt: receipt.CreatedAt, UpdatedAt: receipt.UpdatedAt,
	}, nil
}

type admissionControl struct {
	Address string `json:"address"`
	Token   string `json:"token"`
}

type admissionStat struct {
	SessionID     string `json:"sessionId"`
	SubmissionID  string `json:"submissionId"`
	State         string `json:"state"`
	DispatchCount int    `json:"dispatchCount"`
	StreamActive  bool   `json:"streamActive"`
	Cancelled     bool   `json:"cancelled"`
}

func admissionStats(dataDir, sessionID string) ([]admissionStat, string, error) {
	raw, err := os.ReadFile(filepath.Join(dataDir, "fixture-control.json"))
	if err != nil {
		return nil, "", fmt.Errorf("control info: %w", err)
	}
	var control admissionControl
	if err := json.Unmarshal(raw, &control); err != nil || control.Address == "" || control.Token == "" {
		return nil, "", fmt.Errorf("control info malformed")
	}
	request, err := http.NewRequest(http.MethodGet,
		fmt.Sprintf("http://%s/submissions?sessionId=%s", control.Address, sessionID), nil)
	if err != nil {
		return nil, "", err
	}
	request.Header.Set("Authorization", "Bearer "+control.Token)
	call, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	response, err := http.DefaultClient.Do(request.WithContext(call))
	if err != nil {
		return nil, "", err
	}
	defer response.Body.Close()
	body, err := io.ReadAll(response.Body)
	if err != nil {
		return nil, "", err
	}
	if response.StatusCode != http.StatusOK {
		return nil, "", fmt.Errorf("submissions status %d", response.StatusCode)
	}
	if strings.Contains(string(body), `"prompt"`) {
		return nil, "", fmt.Errorf("submissions projection leaks prompt")
	}
	var decoded struct {
		Submissions []admissionStat `json:"submissions"`
	}
	if err := json.Unmarshal(body, &decoded); err != nil {
		return nil, "", fmt.Errorf("decode submissions: %w", err)
	}
	return decoded.Submissions, string(body), nil
}

func admissionStatFor(stats []admissionStat, submissionID string) (admissionStat, bool) {
	for _, stat := range stats {
		if stat.SubmissionID == submissionID {
			return stat, true
		}
	}
	return admissionStat{}, false
}

func admissionProject(dataDir string, now uint64) (string, map[string]any) {
	id := uuid.NewString()
	return id, map[string]any{
		"id": id, "name": "admission-verify", "path": dataDir,
		"environment_id": "", "created_at": now,
	}
}

func admissionReceiptMap(turnID, inputID string, nowMs uint64) map[string]any {
	return map[string]any{
		"id": turnID, "input_id": inputID, "state": "preparing",
		"created_at": nowMs, "updated_at": nowMs, "native_session_id": nil,
	}
}

func admissionSession(sessionID, projectID, turnID string, receipt map[string]any, prompt string, detailLoaded bool, now uint64) map[string]any {
	return map[string]any{
		"id": sessionID, "title": "admission draft", "project_id": projectID,
		"provider": "opencode", "runtime_mode": "fullAccess", "interaction_mode": "build",
		"status": "working", "created_at": now, "updated_at": now,
		"detail_loaded": detailLoaded, "mermaid_prompt_version": "v4",
		"turns": []any{map[string]any{
			"id": turnID, "turn_count": 1, "status": "running",
			"started_at": now, "completed_at": nil, "submission": receipt,
		}},
		"messages": []any{map[string]any{
			"id": uuid.NewString(), "role": "user", "content": prompt,
			"turn_id": turnID, "streaming": false, "created_at": now,
		}},
	}
}

func admissionAccept(project, session map[string]any, sessionID string, receipt map[string]any, prompt string) map[string]any {
	return map[string]any{
		"type": "acceptSubmission", "project": project, "session": session,
		"submission": map[string]any{
			"session_id": sessionID, "receipt": receipt,
			"prompt": prompt, "files": []any{},
		},
	}
}

func runAdmissionCancelCase(ctx context.Context, client *daemon.Client, dataDir string) caseResult {
	result := caseResult{Name: "admission cancel exits stream without completing turn", Scope: admissionScope, Status: "fail"}
	fail := func(err error) caseResult { result.Detail = err.Error(); return result }
	now := uint64(time.Now().Unix())
	_, project := admissionProject(dataDir, now)
	sessionID, turnID, runtimeID := uuid.NewString(), uuid.NewString(), uuid.NewString()
	inputID := model.NewSubmissionInputID(uint64(time.Now().UnixMilli()))
	receipt := admissionReceiptMap(turnID, inputID, now*1000)
	session := admissionSession(sessionID, project["id"].(string), turnID, receipt, "cancel body 7924", true, now)
	if _, err := admissionCall(ctx, client, sessionID, "", admissionAccept(project, session, sessionID, receipt, "cancel body 7924")); err != nil {
		return fail(err)
	}
	if _, err := admissionCall(ctx, client, sessionID, runtimeID, map[string]any{"type": "start"}); err != nil {
		return fail(err)
	}
	if _, err := admissionCall(ctx, client, sessionID, runtimeID, map[string]any{"type": "dispatchSubmission", "submissionId": turnID, "workspace": map[string]any{"kind": "local"}}); err != nil {
		return fail(err)
	}
	permissionCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	waiting := false
	for !waiting {
		select {
		case event, open := <-client.Events():
			if !open {
				return fail(fmt.Errorf("fixture closed before cancellation"))
			}
			waiting = event.SessionID == sessionID && event.Kind == "permission"
		case <-permissionCtx.Done():
			return fail(fmt.Errorf("permission barrier: %w", permissionCtx.Err()))
		}
	}
	if _, err := admissionCall(ctx, client, sessionID, runtimeID, map[string]any{"type": "cancel"}); err != nil {
		return fail(err)
	}
	deadline := time.NewTimer(5 * time.Second)
	defer deadline.Stop()
	tick := time.NewTicker(50 * time.Millisecond)
	defer tick.Stop()
	for {
		stats, _, err := admissionStats(dataDir, sessionID)
		if err != nil {
			return fail(err)
		}
		stat, exists := admissionStatFor(stats, turnID)
		if exists && stat.Cancelled && !stat.StreamActive {
			if stat.DispatchCount != 1 || stat.State != "accepted" {
				return fail(fmt.Errorf("cancelled submission changed outcome: %+v", stat))
			}
			break
		}
		select {
		case <-tick.C:
		case <-deadline.C:
			return fail(fmt.Errorf("cancelled stream did not exit: %+v", stat))
		case <-ctx.Done():
			return fail(ctx.Err())
		}
	}
	payload, err := admissionCall(ctx, client, sessionID, "", map[string]any{"type": "hydrateSession", "sessionId": sessionID, "environmentId": "00000000-0000-0000-0000-000000000000"})
	if err != nil {
		return fail(err)
	}
	var detail model.SessionDetail
	if json.Unmarshal(payload, &detail) != nil || detail.Session == nil || len(detail.Session.Turns) != 1 || detail.Session.Turns[0].ID != turnID || detail.Session.Turns[0].Status != "interrupted" {
		return fail(fmt.Errorf("cancel did not preserve interrupted turn"))
	}
	result.Status = "pass"
	result.Detail = "cancel at permission exits stream, turn stays interrupted, receipt accepted, dispatchCount 1"
	return result
}

func runAdmissionCases(ctx context.Context, client *daemon.Client, dataDir string) []caseResult {
	now := uint64(time.Now().Unix())
	nowMs := now * 1000
	_, project := admissionProject(dataDir, now)
	promptA := "admission body 6842"
	sessionA := uuid.NewString()
	turnA := uuid.NewString()
	inputA := fmt.Sprintf("msg_%012x%s", (uint64(time.Now().UnixMilli())*0x1000)&0xffffffffffff,
		strings.ReplaceAll(uuid.NewString(), "-", "")[:14])
	receiptA := admissionReceiptMap(turnA, inputA, nowMs)

	rejectDetail := admissionSession(sessionA, project["id"].(string), turnA, receiptA, promptA, false, now)
	if _, err := admissionCall(ctx, client, sessionA, "", admissionAccept(project, rejectDetail, sessionA, receiptA, promptA)); err == nil {
		return []caseResult{{Name: "admission rejects detail_loaded false", Scope: admissionScope, Status: "fail",
			Detail: "accept with detail_loaded false succeeded"}}
	}
	cases := []caseResult{{Name: "admission rejects detail_loaded false", Scope: admissionScope, Status: "pass",
		Detail: "skeleton session refused before any registry write"}}

	sessionFull := admissionSession(sessionA, project["id"].(string), turnA, receiptA, promptA, true, now)
	accepted, err := admissionCall(ctx, client, sessionA, "", admissionAccept(project, sessionFull, sessionA, receiptA, promptA))
	if err != nil {
		return append(cases, caseResult{Name: "admission accept freezes identity", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("accept: %v", err)})
	}
	saved, err := decodeAdmissionReceipt(accepted)
	if err != nil || saved.ID != turnA || saved.InputID != inputA || saved.State != "preparing" {
		return append(cases, caseResult{Name: "admission accept freezes identity", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("identity mismatch: %+v err=%v", saved, err)})
	}
	cases = append(cases, caseResult{Name: "admission accept freezes identity", Scope: admissionScope, Status: "pass",
		Detail: "snake_case receipt frozen at preparing"})

	duplicate, err := admissionCall(ctx, client, sessionA, "", admissionAccept(project, sessionFull, sessionA, receiptA, promptA))
	if err != nil {
		return append(cases, caseResult{Name: "admission duplicate accept returns stored receipt", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("duplicate: %v", err)})
	}
	again, err := decodeAdmissionReceipt(duplicate)
	if err != nil || again.ID != saved.ID || again.InputID != saved.InputID || again.State != saved.State {
		return append(cases, caseResult{Name: "admission duplicate accept returns stored receipt", Scope: admissionScope, Status: "fail",
			Detail: "duplicate did not return the stored receipt"})
	}
	cases = append(cases, caseResult{Name: "admission duplicate accept returns stored receipt", Scope: admissionScope, Status: "pass",
		Detail: "same identity replays the stored receipt"})

	conflict := admissionAccept(project, sessionFull, sessionA, receiptA, "conflicting body 9999")
	if _, err := admissionCall(ctx, client, sessionA, "", conflict); err == nil {
		return append(cases, caseResult{Name: "admission rejects changed prompt under same identity", Scope: admissionScope, Status: "fail",
			Detail: "changed prompt accepted under the same submission identity"})
	}
	cases = append(cases, caseResult{Name: "admission rejects changed prompt under same identity", Scope: admissionScope, Status: "pass",
		Detail: "same identity with changed prompt refused"})

	abandoned, err := admissionCall(ctx, client, sessionA, "",
		map[string]any{"type": "abandonSubmission", "submissionId": turnA})
	if err != nil {
		return append(cases, caseResult{Name: "admission abandon moves preparing to notSent", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("abandon: %v", err)})
	}
	left, err := decodeAdmissionReceipt(abandoned)
	if err != nil || left.State != "notSent" {
		return append(cases, caseResult{Name: "admission abandon moves preparing to notSent", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("want notSent, got %+v err=%v", left, err)})
	}
	cases = append(cases, caseResult{Name: "admission abandon moves preparing to notSent", Scope: admissionScope, Status: "pass",
		Detail: "preparing abandoned to notSent"})

	dispatched, err := admissionCall(ctx, client, sessionA, "",
		map[string]any{"type": "dispatchSubmission", "submissionId": turnA, "workspace": map[string]any{"kind": "local"}})
	if err != nil {
		return append(cases, caseResult{Name: "admission abandoned dispatch stays notSent", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("dispatch after abandon: %v", err)})
	}
	stuck, err := decodeAdmissionReceipt(dispatched)
	if err != nil || stuck.State != "notSent" {
		return append(cases, caseResult{Name: "admission abandoned dispatch stays notSent", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("abandoned input changed outcome: %+v err=%v", stuck, err)})
	}
	stats, _, err := admissionStats(dataDir, sessionA)
	if err != nil {
		return append(cases, caseResult{Name: "admission abandoned dispatch stays notSent", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("submissions: %v", err)})
	}
	stat, ok := admissionStatFor(stats, turnA)
	if !ok || stat.State != "notSent" || stat.DispatchCount != 0 {
		return append(cases, caseResult{Name: "admission abandoned dispatch stays notSent", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("projection %+v found=%v", stat, ok)})
	}
	cases = append(cases, caseResult{Name: "admission abandoned dispatch stays notSent", Scope: admissionScope, Status: "pass",
		Detail: "abandon wins before dispatch, dispatchCount 0, no prompt leaked"})

	sessionB := uuid.NewString()
	turnB := uuid.NewString()
	inputB := fmt.Sprintf("msg_%012x%s", (uint64(time.Now().UnixMilli())*0x1000)&0xffffffffffff,
		strings.ReplaceAll(uuid.NewString(), "-", "")[:14])
	promptB := "admission body 6853"
	receiptB := admissionReceiptMap(turnB, inputB, nowMs)
	sessionFullB := admissionSession(sessionB, project["id"].(string), turnB, receiptB, promptB, true, now)
	if _, err := admissionCall(ctx, client, sessionB, "", admissionAccept(project, sessionFullB, sessionB, receiptB, promptB)); err != nil {
		return append(cases, caseResult{Name: "admission dispatch claims once", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("second session accept: %v", err)})
	}
	runtimeB := uuid.NewString()
	if _, err := admissionCall(ctx, client, sessionB, runtimeB, map[string]any{"type": "start"}); err != nil {
		return append(cases, caseResult{Name: "admission dispatch claims once", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("start: %v", err)})
	}
	first, err := admissionCall(ctx, client, sessionB, runtimeB,
		map[string]any{"type": "dispatchSubmission", "submissionId": turnB, "workspace": map[string]any{"kind": "local"}})
	if err != nil {
		return append(cases, caseResult{Name: "admission dispatch claims once", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("first dispatch: %v", err)})
	}
	claimed, err := decodeAdmissionReceipt(first)
	if err != nil || (claimed.State != "dispatching" && claimed.State != "accepted") {
		return append(cases, caseResult{Name: "admission dispatch claims once", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("first dispatch state %+v err=%v", claimed, err)})
	}
	second, err := admissionCall(ctx, client, sessionB, runtimeB,
		map[string]any{"type": "dispatchSubmission", "submissionId": turnB, "workspace": map[string]any{"kind": "local"}})
	if err != nil {
		return append(cases, caseResult{Name: "admission dispatch claims once", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("second dispatch: %v", err)})
	}
	if _, err := decodeAdmissionReceipt(second); err != nil {
		return append(cases, caseResult{Name: "admission dispatch claims once", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("second dispatch receipt: %v", err)})
	}
	deadline := time.Now().Add(5 * time.Second)
	var statB admissionStat
	for {
		stats, _, err = admissionStats(dataDir, sessionB)
		if err != nil {
			return append(cases, caseResult{Name: "admission dispatch claims once", Scope: admissionScope, Status: "fail",
				Detail: fmt.Sprintf("submissions: %v", err)})
		}
		statB, ok = admissionStatFor(stats, turnB)
		if ok && statB.DispatchCount == 1 && (statB.State == "accepted" || statB.State == "dispatching") {
			break
		}
		if time.Now().After(deadline) {
			return append(cases, caseResult{Name: "admission dispatch claims once", Scope: admissionScope, Status: "fail",
				Detail: fmt.Sprintf("projection %+v found=%v", statB, ok)})
		}
		time.Sleep(100 * time.Millisecond)
	}
	cases = append(cases, caseResult{Name: "admission dispatch claims once", Scope: admissionScope, Status: "pass",
		Detail: fmt.Sprintf("repeat dispatch keeps dispatchCount 1, state %s", statB.State)})

	waitAccepted := time.Now().Add(5 * time.Second)
	for statB.State != "accepted" && time.Now().Before(waitAccepted) {
		time.Sleep(100 * time.Millisecond)
		stats, _, err = admissionStats(dataDir, sessionB)
		if err != nil {
			break
		}
		if next, ok := admissionStatFor(stats, turnB); ok {
			statB = next
		}
	}
	afterAbandon, err := admissionCall(ctx, client, sessionB, runtimeB,
		map[string]any{"type": "abandonSubmission", "submissionId": turnB})
	if err != nil {
		return append(cases, caseResult{Name: "admission abandon after claim keeps receipt", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("abandon after claim: %v", err)})
	}
	kept, err := decodeAdmissionReceipt(afterAbandon)
	if err != nil || kept.State == "notSent" {
		return append(cases, caseResult{Name: "admission abandon after claim keeps receipt", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("claimed receipt changed: %+v err=%v", kept, err)})
	}
	cases = append(cases, caseResult{Name: "admission abandon after claim keeps receipt", Scope: admissionScope, Status: "pass",
		Detail: fmt.Sprintf("abandon after claim keeps %s", kept.State)})

	stale := admissionSession(sessionB, project["id"].(string), turnB, receiptB, promptB, true, now)
	if _, err := admissionCall(ctx, client, sessionB, runtimeB, map[string]any{
		"type": "saveTaskStateCheckpoint", "projects": []any{project},
		"sessions": []any{stale}, "skeletonSessionIds": []string{},
	}); err != nil {
		return append(cases, caseResult{Name: "admission stale snapshot never downgrades registry", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("stale save: %v", err)})
	}
	forgedReceipt := admissionReceiptMap(turnB, inputB, nowMs)
	forgedReceipt["state"] = "rejected"
	forgedReceipt["updated_at"] = nowMs + 9000
	forged := admissionSession(sessionB, project["id"].(string), turnB, forgedReceipt, promptB, true, now)
	if _, err := admissionCall(ctx, client, sessionB, runtimeB, map[string]any{
		"type": "saveTaskStateCheckpoint", "projects": []any{project},
		"sessions": []any{forged}, "skeletonSessionIds": []string{},
	}); err != nil {
		return append(cases, caseResult{Name: "admission stale snapshot never downgrades registry", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("forged save: %v", err)})
	}
	stats, _, err = admissionStats(dataDir, sessionB)
	if err != nil {
		return append(cases, caseResult{Name: "admission stale snapshot never downgrades registry", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("submissions: %v", err)})
	}
	final, ok := admissionStatFor(stats, turnB)
	if !ok || final.State != "accepted" || final.DispatchCount != 1 {
		return append(cases, caseResult{Name: "admission stale snapshot never downgrades registry", Scope: admissionScope, Status: "fail",
			Detail: fmt.Sprintf("registry moved: %+v found=%v", final, ok)})
	}
	cases = append(cases, caseResult{Name: "admission stale snapshot never downgrades registry", Scope: admissionScope, Status: "pass",
		Detail: "preparing snapshot and forged rejected both keep accepted, dispatchCount 1"})

	return cases
}

package main

// Real daemon E2E for the top-level runtime fork command
// (fork{turnsToRemove}). The daemon uses its bound fake engine only;
// no real provider credentials or models are involved.

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

type opRecord struct {
	Name   string `json:"name"`
	OK     bool   `json:"ok"`
	Detail string `json:"detail"`
}

type msgView struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

type turnView struct {
	ID        string `json:"id"`
	Status    string `json:"status"`
	TurnCount int64  `json:"turn_count"`
}

type hydratedState struct {
	Messages    []msgView
	Turns       []turnView
	Status      string
	Native      string
	RawMessages []any
	RawTurns    []any
	RawBlocks   []any
}

type fakeCall struct {
	Method string          `json:"method"`
	Path   string          `json:"path"`
	Body   json.RawMessage `json:"body"`
}

func main() {
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		if err := serveFake(os.Args[1:]); err != nil {
			fmt.Fprintln(os.Stderr, "[X]", err)
			os.Exit(2)
		}
		return
	}
	daemonBinary := flag.String("daemon", "", "absolute daemon binary")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 240*time.Second)
	defer cancel()
	if err := run(ctx, *daemonBinary); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, daemonBinary string) (err error) {
	if !filepath.IsAbs(daemonBinary) {
		return errors.New("absolute daemon binary required")
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	ops := []opRecord{}
	note := func(name string, opErr error) {
		rec := opRecord{Name: name, OK: opErr == nil}
		if opErr != nil {
			rec.Detail = opErr.Error()
		}
		ops = append(ops, rec)
	}
	checks := map[string]bool{}
	counts := map[string]any{}
	report := map[string]any{
		"command":       "runtimeforkcheck --daemon " + daemonBinary,
		"provider":      "fixture-only",
		"model":         "fixture/runtime-fork",
		"round":         round,
		"realRequests":  0,
		"realProvider":  false,
		"credentialKey": false,
		"notes": map[string]string{
			"missingCount":  "absent turnsToRemove coerces to 0 via backendstore.Int convention (remove-all), not a refusal",
			"negativeCount": "negative JSON fails command.Int uint64 parse; ForkRuntime non-negative branch unreachable via wire",
			"decoys":        "empty-text user, attachment-only user, role-only message, synthetic type with text: none counted",
		},
		"uncovered": []string{
			"native history stall/page-budget/byte-budget defenses: fake serves bounded pages, never stalls",
			"revert/undo native operations: outside runtime fork scope",
			"restart durability: runtime fork persists nothing, no restart assertion applicable",
		},
	}
	defer func() {
		report["ops"] = ops
		report["checks"] = checks
		report["counts"] = counts
		report["passed"] = err == nil
		raw, _ := json.MarshalIndent(report, "", "  ")
		_ = os.WriteFile(filepath.Join(round, "runtimefork-result.json"), raw, 0o600)
		fmt.Println("[REPORT]", filepath.Join(round, "runtimefork-result.json"))
	}()
	daemonHash, err := fileSHA256(daemonBinary)
	if err != nil {
		return err
	}
	report["daemonSha256"] = daemonHash
	report["daemonBinary"] = daemonBinary
	if strings.TrimSpace(os.Getenv("FINTWIND_AGENT_REAL_PROVIDER")) != "" {
		return errors.New("real provider opt-in must stay unset for this fixture run")
	}
	if strings.TrimSpace(os.Getenv("DEEPSEEK_API_KEY")) != "" {
		return errors.New("provider credential must stay unset for this fixture run")
	}

	token := uuid.NewString()
	dataDir := filepath.Join(round, "data")
	workspace := filepath.Join(round, "workspace", "main", "test-proj")
	if err := os.MkdirAll(workspace, 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(workspace, "README.md"), []byte("# isolated runtime fork project\n"), 0o600); err != nil {
		return err
	}
	logs := filepath.Join(round, "daemon-logs")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(daemonBinary, []string{"--data-dir", dataDir, "--bind", "127.0.0.1:0"}, logs, round, ownedprocess.BuildEnv(round, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if err != nil {
		return err
	}
	defer child.Close()
	address, err := waitReady(ctx, child)
	if err != nil {
		return err
	}
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()

	self, err := os.Executable()
	if err != nil {
		return err
	}

	// Rejection suite: no runtime, wrong runtime, bad counts. Zero fake POSTs.
	outsider := uuid.NewString()
	_, err = client.RequestFor(ctx, outsider, uuid.NewString(), map[string]any{"type": "fork", "turnsToRemove": 0})
	note("fork-without-runtime", err)
	if err == nil || !strings.Contains(err.Error(), "session runtime missing") {
		return fmt.Errorf("fork without runtime was not refused: %v", err)
	}
	checks["fork-without-runtime-refused"] = true

	sid, rid := uuid.NewString(), uuid.NewString()
	options := map[string]any{
		"binary": self, "cwd": workspace,
		"mode": "ask", "interactionMode": "code",
		"model": "fixture/runtime-fork",
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": options}); err != nil {
		return fmt.Errorf("daemon start with fixture model failed: %w", err)
	}
	checks["daemon-start-fixture-accepted"] = true
	_, werr := client.RequestFor(ctx, sid, uuid.NewString(), map[string]any{"type": "fork", "turnsToRemove": 0})
	note("fork-wrong-runtime", werr)
	if werr == nil || !strings.Contains(werr.Error(), "runtime identity mismatch") {
		return fmt.Errorf("fork with wrong runtime was not refused: %v", werr)
	}
	checks["fork-wrong-runtime-refused"] = true

	// Empty native history: remove 0 returns the original cursor, no POST.
	// The native id is learned from that cursor (nothing persisted yet).
	empty, err := forkRaw(ctx, client, sid, rid, 0)
	note("fork-empty-history-zero", err)
	if err != nil {
		return fmt.Errorf("fork on empty history failed: %w", err)
	}
	if empty.Type != "cursor" || empty.Cursor.Provider != "openCode" || empty.Cursor.SessionID == "" {
		return fmt.Errorf("empty-history fork returned %+v", empty)
	}
	if strings.Contains(string(empty.Raw), "providerSessionForked") {
		return errors.New("empty-history fork used the task-level wrapper")
	}
	native := empty.Cursor.SessionID
	if ids := nativeIDs(round, native); len(ids) != 0 {
		return fmt.Errorf("fresh native session has %d messages", len(ids))
	}
	if posts := countCalls(round, "POST", "/fork"); posts != 0 {
		return fmt.Errorf("empty-history fork posted %d times", posts)
	}
	checks["fork-empty-history-original-cursor-no-post"] = true

	// Missing turnsToRemove coerces to 0 through the established
	// backendstore.Int convention (absent key reads as zero, and zero is
	// a valid removal meaning copy-all). Same wire result as remove 0.
	missingRaw, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "fork"})
	note("fork-missing-count", err)
	if err != nil {
		return fmt.Errorf("fork without turnsToRemove was not accepted as remove 0: %v", err)
	}
	var missingBody struct {
		Type   string `json:"type"`
		Cursor struct {
			Provider string `json:"provider"`
		} `json:"cursor"`
	}
	if err := json.Unmarshal(missingRaw, &missingBody); err != nil {
		return err
	}
	if missingBody.Type != "cursor" || missingBody.Cursor.Provider != "openCode" {
		return fmt.Errorf("missing-count fork reply %+v", missingBody)
	}
	if strings.Contains(string(missingRaw), "providerSessionForked") {
		return errors.New("missing-count fork used the task-level wrapper")
	}
	checks["fork-missing-count-means-zero"] = true

	_, err = client.RequestFor(ctx, sid, rid, map[string]any{"type": "fork", "turnsToRemove": -1})
	note("fork-negative-count", err)
	if err == nil || !strings.Contains(err.Error(), "invalid integer field turnsToRemove") {
		return fmt.Errorf("fork with negative turnsToRemove was not refused: %v", err)
	}
	checks["fork-negative-count-refused"] = true

	if posts := countCalls(round, "POST", "/fork"); posts != 0 {
		return fmt.Errorf("rejected forks reached the native engine %d times", posts)
	}
	checks["rejected-forks-no-native-post"] = true

	// Empty native history: remove beyond zero is refused, still no POST.
	_, err = client.RequestFor(ctx, sid, rid, map[string]any{"type": "fork", "turnsToRemove": 1})
	note("fork-empty-history-exceeds", err)
	if err == nil || !strings.Contains(err.Error(), "exceeds native user history") {
		return fmt.Errorf("fork beyond empty history was not refused: %v", err)
	}
	if posts := countCalls(round, "POST", "/fork"); posts != 0 {
		return fmt.Errorf("refused exceeds fork posted %d times", posts)
	}
	checks["fork-empty-history-exceeds-refused"] = true

	// Build real history with interleaved decoys. All prompts succeed.
	now := uint64(time.Now().Unix())
	project := map[string]any{"id": uuid.NewString(), "name": "runtime-fork", "path": workspace, "created_at": now}
	turnCount := 0
	turn := func(prompt string) error {
		turnCount++
		tid := uuid.NewString()
		record := model.NewSubmissionRecord(sid, tid, prompt, nil)
		userMsg := map[string]any{"id": uuid.NewString(), "turn_id": tid, "role": "user", "content": prompt, "created_at": now, "attachments": []any{}, "streaming": false}
		state, hErr := hydrateState(ctx, client, sid)
		if hErr != nil {
			return hErr
		}
		session := map[string]any{"id": sid, "project_id": project["id"], "title": "runtime-fork", "provider": "opencode", "status": "working", "created_at": now, "updated_at": now, "detail_loaded": true,
			"messages":          append(append([]any{}, state.RawMessages...), userMsg),
			"turns":             append(append([]any{}, state.RawTurns...), map[string]any{"id": tid, "status": "running", "turn_count": turnCount, "started_at": now, "submission": record.Receipt}),
			"transcript_blocks": state.RawBlocks}
		if _, aErr := client.RequestFor(ctx, sid, "", map[string]any{"type": "acceptSubmission", "project": project, "session": session, "submission": record}); aErr != nil {
			return fmt.Errorf("accept %q: %w", prompt, aErr)
		}
		if _, dErr := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": tid, "workspace": map[string]string{"kind": "local"}}); dErr != nil {
			return fmt.Errorf("dispatch %q: %w", prompt, dErr)
		}
		text, _, success, wErr := waitTurn(ctx, client)
		if wErr != nil {
			return fmt.Errorf("wait %q: %w", prompt, wErr)
		}
		if !success || strings.TrimSpace(text) != "rk" {
			return fmt.Errorf("turn %q settled success=%v text=%q", prompt, success, text)
		}
		return nil
	}
	for _, prompt := range []string{"first", "note-empty", "second", "note-attach", "note-role", "note-synth", "third-flat"} {
		if err := turn(prompt); err != nil {
			chErr := err
			note("build-history", chErr)
			return chErr
		}
	}
	note("build-history", nil)
	checks["history-built-with-decoys"] = true

	snapGo, err := hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	if len(snapGo.Turns) != 7 {
		return fmt.Errorf("history turn count %d", len(snapGo.Turns))
	}
	snapNative := nativeIDs(round, native)
	createsBefore := countCalls(round, "POST", "/api/session")
	getsBeforeForks := countCalls(round, "GET", "/message")

	// Fork math over decoy-laden history: users are u1,u2,u3 oldest-first.
	forkOne, err := forkRaw(ctx, client, sid, rid, 1)
	note("fork-remove-one", err)
	if err != nil {
		return fmt.Errorf("fork remove 1 failed: %w", err)
	}
	assertForkShape(forkOne, native)
	if body := lastForkBody(round); body != `{"before":"u3"}` {
		return fmt.Errorf("fork remove 1 body %s, want before u3", body)
	}
	if kept := forkedIDs(round, forkOne.Cursor.SessionID); strings.Join(kept, ",") != "u1,a1,d1,a2,u2,a3,d2,a4,d3,a5,d4,a6" {
		return fmt.Errorf("fork remove 1 kept %v", kept)
	}
	checks["fork-remove-one-before-u3"] = true

	forkZero, err := forkRaw(ctx, client, sid, rid, 0)
	note("fork-remove-zero", err)
	if err != nil {
		return fmt.Errorf("fork remove 0 failed: %w", err)
	}
	assertForkShape(forkZero, native)
	if body := lastForkBody(round); body != `{}` {
		return fmt.Errorf("fork remove 0 body %s, want empty object", body)
	}
	checks["fork-remove-zero-empty-body"] = true

	forkTwo, err := forkRaw(ctx, client, sid, rid, 2)
	note("fork-remove-two", err)
	if err != nil {
		return fmt.Errorf("fork remove 2 failed: %w", err)
	}
	assertForkShape(forkTwo, native)
	if body := lastForkBody(round); body != `{"before":"u2"}` {
		return fmt.Errorf("fork remove 2 body %s, want before u2", body)
	}
	checks["fork-remove-two-before-u2"] = true

	forkThree, err := forkRaw(ctx, client, sid, rid, 3)
	note("fork-remove-three", err)
	if err != nil {
		return fmt.Errorf("fork remove 3 failed: %w", err)
	}
	assertForkShape(forkThree, native)
	if body := lastForkBody(round); body != `{"before":"u1"}` {
		return fmt.Errorf("fork remove 3 body %s, want before u1", body)
	}
	checks["fork-remove-all-before-first-user"] = true

	beforeGets := countCalls(round, "GET", "/message")
	if got := beforeGets - getsBeforeForks; got != 28 {
		return fmt.Errorf("four forks walked %d message pages, want 28", got)
	}
	checks["fork-walks-real-pagination"] = true
	_, err = client.RequestFor(ctx, sid, rid, map[string]any{"type": "fork", "turnsToRemove": 4})
	note("fork-remove-exceeds", err)
	if err == nil || !strings.Contains(err.Error(), "exceeds native user history") {
		return fmt.Errorf("fork remove 4 was not refused: %v", err)
	}
	if posts := countCalls(round, "POST", "/fork"); posts != 4 {
		return fmt.Errorf("refused exceeds fork changed post count to %d", posts)
	}
	checks["fork-remove-exceeds-refused"] = true
	counts["forkPosts"] = 4
	counts["messageGetsFourForks"] = beforeGets - getsBeforeForks

	// Source unchanged by every fork: Go transcript, native history, engines.
	afterGo, err := hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	if !sameTranscript(snapGo, afterGo) {
		return errors.New("source Go transcript changed across forks")
	}
	if strings.Join(nativeIDs(round, native), ",") != strings.Join(snapNative, ",") {
		return errors.New("source native history changed across forks")
	}
	if creates := countCalls(round, "POST", "/api/session"); creates != createsBefore {
		return fmt.Errorf("fork started a new engine %d times", creates-createsBefore)
	}
	checks["fork-source-unchanged"] = true

	// Busy source forks without touching source state; cancel settles it.
	turnCount++
	holdID := uuid.NewString()
	holdRecord := model.NewSubmissionRecord(sid, holdID, "hold-fork", nil)
	holdUser := map[string]any{"id": uuid.NewString(), "turn_id": holdID, "role": "user", "content": "hold-fork", "created_at": now, "attachments": []any{}, "streaming": false}
	holdState, err := hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	holdSession := map[string]any{"id": sid, "project_id": project["id"], "title": "runtime-fork", "provider": "opencode", "status": "working", "created_at": now, "updated_at": now, "detail_loaded": true,
		"messages":          append(append([]any{}, holdState.RawMessages...), holdUser),
		"turns":             append(append([]any{}, holdState.RawTurns...), map[string]any{"id": holdID, "status": "running", "turn_count": turnCount, "started_at": now, "submission": holdRecord.Receipt}),
		"transcript_blocks": holdState.RawBlocks}
	if _, err := client.RequestFor(ctx, sid, "", map[string]any{"type": "acceptSubmission", "project": project, "session": holdSession, "submission": holdRecord}); err != nil {
		return fmt.Errorf("accept hold: %w", err)
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": holdID, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return fmt.Errorf("dispatch hold: %w", err)
	}
	busyNative := nativeIDs(round, native)
	if strings.Join(busyNative, ",") == strings.Join(snapNative, ",") {
		return errors.New("hold prompt did not extend native history")
	}
	busyGo, err := hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	forkBusy, err := forkRaw(ctx, client, sid, rid, 1)
	note("fork-busy-source", err)
	if err != nil {
		return fmt.Errorf("fork on busy source failed: %w", err)
	}
	assertForkShape(forkBusy, native)
	if body := lastForkBody(round); body != `{"before":"u4"}` {
		return fmt.Errorf("busy fork body %s, want before u4", body)
	}
	settledGo, err := hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	if !sameTranscript(busyGo, settledGo) {
		return errors.New("fork changed busy Go transcript")
	}
	if strings.Join(nativeIDs(round, native), ",") != strings.Join(busyNative, ",") {
		return errors.New("fork changed busy native history")
	}
	if kept := forkedIDs(round, forkBusy.Cursor.SessionID); strings.Join(kept, ",") != "u1,a1,d1,a2,u2,a3,d2,a4,d3,a5,d4,a6,u3,a7" {
		return fmt.Errorf("busy fork kept %v", kept)
	}
	checks["fork-busy-source-unchanged"] = true

	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "cancel"}); err != nil {
		return fmt.Errorf("cancel busy turn: %w", err)
	}
	_, _, success, err := waitTurn(ctx, client)
	note("cancel-busy-turn", err)
	if err != nil {
		return fmt.Errorf("wait cancel: %w", err)
	}
	if success {
		return errors.New("cancelled turn reported success")
	}
	finalGo, err := hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	if finalGo.Turns[len(finalGo.Turns)-1].Status != "interrupted" {
		return fmt.Errorf("busy turn status %s", finalGo.Turns[len(finalGo.Turns)-1].Status)
	}
	checks["cancel-settles-busy-turn"] = true

	// Injected native 500: single POST, no retry, Go transcript intact.
	if err := os.WriteFile(filepath.Join(round, "fixture-fail-fork"), []byte("1"), 0o600); err != nil {
		return err
	}
	postsBefore := countCalls(round, "POST", "/fork")
	_, err = client.RequestFor(ctx, sid, rid, map[string]any{"type": "fork", "turnsToRemove": 0})
	chErr := err
	note("fork-native-500", chErr)
	if chErr == nil || !strings.Contains(chErr.Error(), "500") {
		return fmt.Errorf("injected 500 was not surfaced: %v", chErr)
	}
	if posts := countCalls(round, "POST", "/fork"); posts-postsBefore != 1 {
		return fmt.Errorf("500 fork retried, delta %d", posts-postsBefore)
	}
	if err := os.Remove(filepath.Join(round, "fixture-fail-fork")); err != nil {
		return err
	}
	after500, err := hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	if !sameTranscript(finalGo, after500) {
		return errors.New("failed fork changed Go transcript")
	}
	checks["fork-native-500-no-retry"] = true

	counts["creates"] = countCalls(round, "POST", "/api/session")
	counts["forkPosts"] = countCalls(round, "POST", "/fork")
	counts["messageGets"] = countCalls(round, "GET", "/message")
	counts["prompts"] = countCalls(round, "POST", "/prompt")
	return nil
}

type forkResult struct {
	Type   string `json:"type"`
	Cursor struct {
		Provider  string `json:"provider"`
		SessionID string `json:"sessionId"`
	} `json:"cursor"`
	Raw json.RawMessage
}

func forkRaw(ctx context.Context, client *daemon.Client, sid, rid string, remove int) (forkResult, error) {
	var out forkResult
	raw, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "fork", "turnsToRemove": remove})
	if err != nil {
		return out, err
	}
	out.Raw = raw
	var body struct {
		Type   string `json:"type"`
		Cursor struct {
			Provider  string `json:"provider"`
			SessionID string `json:"sessionId"`
		} `json:"cursor"`
	}
	if err := json.Unmarshal(raw, &body); err != nil {
		return out, err
	}
	out.Type = body.Type
	out.Cursor = body.Cursor
	return out, nil
}

func assertForkShape(result forkResult, native string) error {
	if result.Type != "cursor" {
		return fmt.Errorf("fork reply type %q", result.Type)
	}
	if result.Cursor.Provider != "openCode" || result.Cursor.SessionID == "" {
		return fmt.Errorf("fork cursor %+v", result.Cursor)
	}
	if result.Cursor.SessionID == native {
		return fmt.Errorf("fork cursor did not diverge %q", native)
	}
	if strings.Contains(string(result.Raw), "providerSessionForked") {
		return errors.New("fork used the task-level wrapper")
	}
	return nil
}

func sameTranscript(a, b hydratedState) bool {
	if a.Status != b.Status || len(a.Messages) != len(b.Messages) || len(a.Turns) != len(b.Turns) {
		return false
	}
	for i := range a.Messages {
		if a.Messages[i] != b.Messages[i] {
			return false
		}
	}
	for i := range a.Turns {
		if a.Turns[i] != b.Turns[i] {
			return false
		}
	}
	return true
}

func readCalls(round string) []fakeCall {
	raw, _ := os.ReadFile(filepath.Join(round, "fixture-calls.jsonl"))
	calls := []fakeCall{}
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		var call fakeCall
		if json.Unmarshal([]byte(line), &call) == nil {
			calls = append(calls, call)
		}
	}
	return calls
}

func countCalls(round, method, suffix string) int {
	total := 0
	for _, call := range readCalls(round) {
		if call.Method == method && strings.HasSuffix(call.Path, suffix) {
			total++
		}
	}
	return total
}

func lastForkBody(round string) string {
	body := ""
	for _, call := range readCalls(round) {
		if call.Method == "POST" && strings.HasSuffix(call.Path, "/fork") {
			body = strings.TrimSpace(string(call.Body))
		}
	}
	return body
}

func nativeIDs(round, native string) []string {
	raw, _ := os.ReadFile(filepath.Join(round, "fixture-native.json"))
	var state struct {
		Sessions map[string]struct {
			Messages []map[string]any `json:"messages"`
		} `json:"sessions"`
	}
	if json.Unmarshal(raw, &state) != nil {
		return nil
	}
	session := state.Sessions[native]
	ids := []string{}
	for _, message := range session.Messages {
		id, _ := message["id"].(string)
		ids = append(ids, id)
	}
	return ids
}

func forkedIDs(round, forkID string) []string {
	return nativeIDs(round, forkID)
}

func waitReady(ctx context.Context, child *ownedprocess.Owned) (string, error) {
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		raw, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
			PID     uint32 `json:"pid"`
		}
		if json.Unmarshal(raw, &ready) == nil && ready.PID == child.PID && ready.Address != "" {
			return ready.Address, nil
		}
		if done, _ := child.Exited(0); done {
			return "", errors.New("daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-tick.C:
		}
	}
}

func hydrateState(ctx context.Context, client *daemon.Client, sid string) (hydratedState, error) {
	var out hydratedState
	raw, err := client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sid})
	if err != nil {
		return out, err
	}
	var payload struct {
		Session struct {
			Messages []any  `json:"messages"`
			Turns    []any  `json:"turns"`
			Status   string `json:"status"`
			Native   string `json:"native_session_id"`
			Blocks   []any  `json:"transcript_blocks"`
		} `json:"session"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return out, err
	}
	out.RawMessages = payload.Session.Messages
	out.RawTurns = payload.Session.Turns
	out.RawBlocks = payload.Session.Blocks
	if out.RawBlocks == nil {
		out.RawBlocks = []any{}
	}
	for _, item := range payload.Session.Messages {
		encoded, _ := json.Marshal(item)
		var view msgView
		if err := json.Unmarshal(encoded, &view); err != nil {
			return out, err
		}
		out.Messages = append(out.Messages, view)
	}
	for _, item := range payload.Session.Turns {
		encoded, _ := json.Marshal(item)
		var view turnView
		if err := json.Unmarshal(encoded, &view); err != nil {
			return out, err
		}
		out.Turns = append(out.Turns, view)
	}
	out.Status = payload.Session.Status
	out.Native = payload.Session.Native
	return out, nil
}

func waitTurn(ctx context.Context, client *daemon.Client) (string, []string, bool, error) {
	text := ""
	kinds := []string{}
	seen := map[string]bool{}
	for {
		select {
		case <-ctx.Done():
			return "", kinds, false, ctx.Err()
		case event, ok := <-client.Events():
			if !ok {
				return "", kinds, false, errors.New("client stream closed")
			}
			if !seen[event.Kind] {
				seen[event.Kind] = true
				kinds = append(kinds, event.Kind)
			}
			if event.Kind == "textDelta" {
				var delta struct {
					Delta string `json:"delta"`
				}
				if err := json.Unmarshal(event.Payload, &delta); err != nil {
					return "", kinds, false, err
				}
				text += delta.Delta
			}
			if event.Kind == "turnFinished" {
				var done struct {
					Success bool `json:"success"`
				}
				if err := json.Unmarshal(event.Payload, &done); err != nil {
					return "", kinds, false, err
				}
				if event.Type == "event" {
					client.Ack(event)
				}
				return text, kinds, done.Success, nil
			}
			if event.Type == "event" {
				client.Ack(event)
			}
		}
	}
}

func fileSHA256(path string) (string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:]), nil
}

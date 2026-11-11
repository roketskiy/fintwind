// Fixture-mode driver: the real e2e-built app window against fixturedaemon.
//
// Scope is actualUI+fixtureDaemon. Seeds two sessions through the fixture,
// verifies the daemon headlessly (hello identity, protocol error on unknown
// commands, a full gated stream on the background session), then drives the
// real window: Chinese drafts, send, streaming body, background completion,
// forced disconnect with resume, activity order, permission and question
// answers, and turnFinished surviving hydrate. File editing stays a listing
// check here; literal save and conflict refusal are asserted in realUI mode.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"
)

const (
	fixtureStreamFull   = "fixture-stream-正文-7391-complete"
	fixturePromptText   = "fixture prompt 你好 4821"
	fixtureSecondPrompt = "fixture second prompt 你好 9273"
)

// fixtureStreamOrder is the new fixture's canonical journal order: the
// atomic admission receipt lands between connected and turnStarted, one
// textDelta per stream chunk (four), everything else once. First turn is
// 16 sequenced events, two turns 32; nothing here may hardcode the old 15.
var fixtureStreamOrder = []string{
	"connected", "submissionUpdated", "turnStarted", "textStarted",
	"textDelta", "textDelta", "textDelta", "textDelta",
	"reasoningStarted", "reasoningDelta", "reasoningEnded",
	"activity", "permission", "userInputRequested",
	"textEnded", "turnFinished",
}

// fixtureExpectedKinds is the canonical journal order: one textDelta per
// stream chunk (four), everything else once.
func fixtureExpectedKinds() []string {
	return append([]string{}, fixtureStreamOrder...)
}

// streamStartKinds is the journal prefix length proving a turn is streaming:
// everything through the first textDelta, derived from the literal order so
// an order change moves the bound automatically.
func streamStartKinds() int {
	for i, kind := range fixtureStreamOrder {
		if kind == "textDelta" {
			return i + 1
		}
	}
	return len(fixtureStreamOrder)
}

const streamChunkCount = 4

type fixtureControl struct {
	address string
	token   string
}

func (f fixtureControl) call(ctx context.Context, method, path string) ([]byte, error) {
	request, err := http.NewRequestWithContext(ctx, method, "http://"+f.address+path, nil)
	if err != nil {
		return nil, err
	}
	request.Header.Set("Authorization", "Bearer "+f.token)
	client := &http.Client{Timeout: perRequestTimeout}
	response, err := client.Do(request)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	raw, err := io.ReadAll(response.Body)
	if err != nil {
		return nil, err
	}
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("control %s %s: %d %s", method, path, response.StatusCode, strings.TrimSpace(string(raw)))
	}
	return raw, nil
}

func readFixtureControl(dataDir string) (fixtureControl, error) {
	raw, err := os.ReadFile(filepath.Join(dataDir, "fixture-control.json"))
	if err != nil {
		return fixtureControl{}, err
	}
	var info struct {
		Address string `json:"address"`
		Token   string `json:"token"`
	}
	if err := json.Unmarshal(raw, &info); err != nil {
		return fixtureControl{}, err
	}
	if info.Address == "" || info.Token == "" {
		return fixtureControl{}, fmt.Errorf("fixture control info incomplete")
	}
	return fixtureControl{address: info.Address, token: info.Token}, nil
}

type journalEvent struct {
	SessionID string `json:"sessionId"`
	RuntimeID string `json:"runtimeId"`
	Sequence  uint64 `json:"sequence"`
	Kind      string `json:"kind"`
}

func queryJournal(ctx context.Context, control fixtureControl, sessionID string) ([]journalEvent, error) {
	raw, err := control.call(ctx, "GET", "/journal?sessionId="+sessionID)
	if err != nil {
		return nil, err
	}
	var out struct {
		Events []journalEvent `json:"events"`
	}
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, err
	}
	return out.Events, nil
}

// waitTurnFinishedCount polls the journal until a session holds at least
// want turnFinished events, so reused runtimes (no fresh connected) and
// fresh ones share one predicate.
func waitTurnFinishedCount(ctx context.Context, control fixtureControl, sessionID string, want int, timeout time.Duration) ([]journalEvent, error) {
	deadline := time.Now().Add(timeout)
	for {
		events, err := queryJournal(ctx, control, sessionID)
		if err == nil {
			finished := 0
			for _, event := range events {
				if event.Kind == "turnFinished" {
					finished++
				}
			}
			if finished >= want {
				return events, nil
			}
		}
		if time.Now().After(deadline) {
			count := -1
			if err == nil {
				count = 0
				for _, event := range events {
					if event.Kind == "turnFinished" {
						count++
					}
				}
			}
			return events, fmt.Errorf("journal for %s holds %d turnFinished, want %d", shortSession(sessionID), count, want)
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(300 * time.Millisecond):
		}
	}
}

func waitJournalKinds(ctx context.Context, control fixtureControl, sessionID string, want int, timeout time.Duration) ([]journalEvent, error) {
	deadline := time.Now().Add(timeout)
	for {
		events, err := queryJournal(ctx, control, sessionID)
		if err == nil && len(events) >= want {
			return events, nil
		}
		if time.Now().After(deadline) {
			count := -1
			if err == nil {
				count = len(events)
			}
			return events, fmt.Errorf("journal for %s has %d events, want %d", shortSession(sessionID), count, want)
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(300 * time.Millisecond):
		}
	}
}

func shortSession(id string) string {
	if len(id) > 8 {
		return id[:8]
	}
	return id
}

// fixtureUISteps are the window-driven checks, recorded as blocked when the
// window never opens so result.json never drops an unexecuted step.
var fixtureUISteps = []string{
	"app start with automation socket", "app window opens",
	"seeded sessions load known text",
	"composer draft input", "switch session restores drafts", "new task keeps drafts separate",
	"send streams full text", "background session completes",
	"permission and question answered", "activity order matches stream",
	"disconnect resumes without loss", "turnFinished survives hydrate",
	"files surface lists hello.txt",
	"context panel unknown usage and model window fallback",
	"context window selection persists for the model",
	"existing session retains its own context window",
	"new draft restores context window preference",
	"context usage marks 80 percent near full",
	"context usage marks 95 percent full",
	"context composition and usage KPIs render from events",
	"context copy exposes the native session id",
	"compaction unavailable without a runtime",
	"compaction acknowledgement stays pending",
	"compaction running and failure allow retry",
	"compaction summary action reveals its transcript row",
}

// waitLiveControl polls until the app-owned fixture's control endpoint
// actually answers. startApp only waits for the automation socket, which can
// appear before the daemon rewrote fixture-control.json; the stale seed
// address would otherwise dead-end every later journal query.
func waitLiveControl(ctx context.Context, dataDir string, timeout time.Duration) (fixtureControl, error) {
	deadline := time.Now().Add(timeout)
	var lastErr error
	for {
		if control, err := readFixtureControl(dataDir); err == nil {
			probeCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
			_, probeErr := queryJournal(probeCtx, control, "00000000-0000-0000-0000-000000000000")
			cancel()
			if probeErr == nil {
				return control, nil
			}
			lastErr = probeErr
		} else {
			lastErr = err
		}
		if time.Now().After(deadline) {
			return fixtureControl{}, fmt.Errorf("control endpoint never answered: %v", lastErr)
		}
		select {
		case <-ctx.Done():
			return fixtureControl{}, ctx.Err()
		case <-time.After(300 * time.Millisecond):
		}
	}
}

func runFixtureUI(ctx context.Context, appPath, fixtureExe, roundDir string) []caseResult {
	const scope = "actualUI+fixtureDaemon"
	cases := []caseResult{}
	if appPath == "" {
		cases = append(cases, caseResult{Name: "e2e app binary state", Scope: scope, Status: "blocked",
			Detail: "BLOCKED: missing -app, headless checks only"})
	} else if detail, err := binaryStateDetail(appPath); err != nil {
		cases = append(cases, caseResult{Name: "e2e app binary state", Scope: scope, Status: "fail",
			Detail: fmt.Sprintf("app binary missing: %s", appPath)})
		return cases
	} else {
		cases = append(cases, caseResult{Name: "e2e app binary state", Scope: scope, Status: "pass", Detail: detail})
	}
	if detail, err := binaryStateDetail(fixtureExe); err != nil {
		cases = append(cases, caseResult{Name: "fixture daemon binary state", Scope: scope, Status: "fail",
			Detail: fmt.Sprintf("fixture binary missing: %s (build with: go build -o <path> ./cmd/fixturedaemon)", fixtureExe)})
		return cases
	} else {
		cases = append(cases, caseResult{Name: "fixture daemon binary state", Scope: scope, Status: "pass", Detail: detail})
	}

	dataDir := filepath.Join(roundDir, "data")
	workspace := filepath.Join(roundDir, "workspace")
	if err := os.MkdirAll(workspace, 0o755); err != nil {
		cases = append(cases, caseResult{Name: "prepare dirs", Scope: scope, Status: "fail", Detail: err.Error()})
		return cases
	}
	if err := os.WriteFile(filepath.Join(workspace, knownFile), []byte(knownText+"\n"), 0o644); err != nil {
		cases = append(cases, caseResult{Name: "prepare workspace", Scope: scope, Status: "fail", Detail: err.Error()})
		return cases
	}
	seedDir := filepath.Join(roundDir, "seed")
	appDir := filepath.Join(roundDir, "apprun")
	for _, dir := range []string{seedDir, appDir} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			cases = append(cases, caseResult{Name: "prepare dirs", Scope: scope, Status: "fail", Detail: err.Error()})
			return cases
		}
	}

	seedOwned, seedClient, sessionA, sessionB, ok := fixtureSeed(ctx, fixtureExe, seedDir, dataDir, workspace, &cases)
	if !ok {
		return cases
	}
	control, err := readFixtureControl(dataDir)
	if err != nil {
		seedOwned.Close()
		seedClient.Close()
		cases = append(cases, caseResult{Name: "fixture control plane", Scope: scope, Status: "fail", Detail: err.Error()})
		return cases
	}
	fixtureHeadlessChecks(ctx, seedClient, control, sessionB, &cases)
	seedClient.Close()
	seedOwned.Close()
	if err := os.WriteFile(filepath.Join(dataDir, "app-settings.json"), []byte(`{"language":"english"}`), 0o600); err != nil {
		cases = append(cases, caseResult{Name: "context test locale", Scope: scope, Status: "fail", Detail: err.Error()})
		return cases
	}

	if appPath == "" {
		for _, step := range fixtureUISteps {
			cases = append(cases, caseResult{Name: step, Scope: scope, Status: "blocked", Detail: "BLOCKED: missing -app, UI steps not executed"})
		}
		return cases
	}

	sockPath, ownedApp, client, ok := startApp(ctx, appPath, fixtureExe, appDir, dataDir, scope, &cases)
	if !ok {
		for _, step := range fixtureUISteps {
			seen := false
			for _, c := range cases {
				if c.Name == step {
					seen = true
					break
				}
			}
			if !seen {
				cases = append(cases, caseResult{Name: step, Scope: scope, Status: "blocked", Detail: "BLOCKED: app socket never opened"})
			}
		}
		return cases
	}
	defer ownedApp.Close()
	defer client.close()
	_ = sockPath

	control, err = waitLiveControl(ctx, dataDir, 30*time.Second)
	if err != nil {
		cases = append(cases, caseResult{Name: "fixture control plane", Scope: scope, Status: "fail", Detail: "reread after app start: " + err.Error()})
		return cases
	}

	windowID, ok := waitWindow(ctx, client, scope, &cases)
	if !ok {
		return cases
	}

	checkDraftSwitchNewTaskFixture(ctx, client, windowID, sessionA, sessionB, &cases, roundDir)
	checkFixtureStream(ctx, client, control, windowID, sessionA, sessionB, dataDir, &cases, roundDir)
	checkContextPanel(ctx, client, control, windowID, sessionA, sessionB, dataDir, &cases, roundDir)
	checkFilesListing(ctx, client, windowID, &cases, roundDir)
	return cases
}

// fixtureSeed starts the fixture daemon, stores one project with two sessions
// and verifies the round-trip. The Owned process stays open for headless
// checks; the caller closes it before the app starts.
func fixtureSeed(ctx context.Context, fixtureExe, seedDir, dataDir, workspace string, cases *[]caseResult) (*ownedprocess.Owned, *daemon.Client, string, string, bool) {
	const scope = "actualUI+fixtureDaemon"
	fail := func(detail string) (*ownedprocess.Owned, *daemon.Client, string, string, bool) {
		*cases = append(*cases, caseResult{Name: "fixture seed", Scope: scope, Status: "fail", Detail: detail})
		return nil, nil, "", "", false
	}
	token, err := randomToken()
	if err != nil {
		return fail(err.Error())
	}
	owned, err := ownedprocess.Start(fixtureExe,
		[]string{"--bind", "127.0.0.1:0", "--data-dir", dataDir, "--parent-pid", strconv.Itoa(os.Getpid())},
		seedDir,
		map[string]string{"FINTWIND_DAEMON_TOKEN": token})
	if err != nil {
		return fail("owned start: " + err.Error())
	}
	ready, err := waitReadyLine(ctx, filepath.Join(seedDir, "child-stdout.log"), daemonReadyWait)
	if err != nil {
		owned.Close()
		return fail("ready: " + err.Error())
	}
	if ready.ProtocolVersion != daemon.ProtocolVersion {
		owned.Close()
		return fail(fmt.Sprintf("protocol %d, client speaks %d", ready.ProtocolVersion, daemon.ProtocolVersion))
	}
	dialCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	client, err := daemon.Dial(dialCtx, ready.Address, token)
	cancel()
	if err != nil {
		owned.Close()
		return fail("dial: " + err.Error())
	}
	identity := client.Identity()
	if identity.EnvironmentID == "00000000-0000-0000-0000-000000000000" || identity.InstanceID == "00000000-0000-0000-0000-000000000000" {
		client.Close()
		owned.Close()
		return fail("zero environment or instance identity")
	}
	*cases = append(*cases, caseResult{Name: "fixture hello identity", Scope: scope, Status: "pass",
		Detail: fmt.Sprintf("protocol %d, nonzero environment and instance", daemon.ProtocolVersion)})

	now := uint64(time.Now().Unix())
	save := seedSessionsPayload(workspace, now)
	if err := requestDaemon(ctx, client, save); err != nil {
		client.Close()
		owned.Close()
		return fail("saveTaskState: " + err.Error())
	}
	var state model.TaskState
	if err := requestDaemonInto(ctx, client, map[string]any{"type": "loadTaskState"}, &state); err != nil {
		client.Close()
		owned.Close()
		return fail("loadTaskState: " + err.Error())
	}
	if len(state.Projects) != 1 || len(state.Sessions) != 2 {
		client.Close()
		owned.Close()
		return fail(fmt.Sprintf("want 1 project and 2 sessions, got %d and %d", len(state.Projects), len(state.Sessions)))
	}
	*cases = append(*cases, caseResult{Name: "fixture seed two sessions", Scope: scope, Status: "pass",
		Detail: "1 project, 2 sessions with settled user turns, hello.txt with known text"})

	if err := assertSeedHydrate(ctx, client, seedSessionA, seededBodyA); err != nil {
		client.Close()
		owned.Close()
		*cases = append(*cases, caseResult{Name: "seed transcript hydrates literal text", Scope: scope, Status: "fail",
			Detail: "session A: " + err.Error()})
		return nil, nil, "", "", false
	}
	if err := assertSeedHydrate(ctx, client, seedSessionB, seededBodyB); err != nil {
		client.Close()
		owned.Close()
		*cases = append(*cases, caseResult{Name: "seed transcript hydrates literal text", Scope: scope, Status: "fail",
			Detail: "session B: " + err.Error()})
		return nil, nil, "", "", false
	}
	*cases = append(*cases, caseResult{Name: "seed transcript hydrates literal text", Scope: scope, Status: "pass",
		Detail: "both bodies return literally from hydrateSession"})

	name, diskErr := storeHoldsSessions(dataDir, []string{"fixture-task-state.json"})
	if diskErr != nil {
		client.Close()
		owned.Close()
		*cases = append(*cases, caseResult{Name: "seed persisted to disk", Scope: scope, Status: "fail", Detail: diskErr.Error()})
		return nil, nil, "", "", false
	}
	*cases = append(*cases, caseResult{Name: "seed persisted to disk", Scope: scope, Status: "pass",
		Detail: "both session UUIDs found in " + name})
	return owned, client, seedSessionA, seedSessionB, true
}

// fixtureHeadlessChecks drives the daemon without any window: an unknown
// command must fail with a protocol error, settings must answer, and a full
// gated stream on the background session must arrive in canonical order with
// the gates answered over the wire.
func fixtureHeadlessChecks(ctx context.Context, client *daemon.Client, control fixtureControl, sessionB string, cases *[]caseResult) {
	const scope = "fixtureDaemon"

	reqCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	_, err := client.Request(reqCtx, map[string]any{"type": "noSuchCommand"})
	cancel()
	if err == nil {
		*cases = append(*cases, caseResult{Name: "unknown command errors", Scope: scope, Status: "fail", Detail: "unknown command was accepted"})
	} else {
		*cases = append(*cases, caseResult{Name: "unknown command errors", Scope: scope, Status: "pass", Detail: "protocol error, nothing executed"})
	}

	settingsCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	_, err = client.Request(settingsCtx, map[string]any{"type": "getSettings"})
	cancel()
	if err != nil {
		*cases = append(*cases, caseResult{Name: "getSettings answers", Scope: scope, Status: "fail", Detail: err.Error()})
	} else {
		*cases = append(*cases, caseResult{Name: "getSettings answers", Scope: scope, Status: "pass", Detail: "settings payload accepted by client"})
	}

	runtime, err := admissionTurn(ctx, client, sessionB, seedProjectID, "headless admission prompt B")
	if err != nil {
		*cases = append(*cases, caseResult{Name: "headless gated stream", Scope: scope, Status: "fail", Detail: "admission turn: " + err.Error()})
		return
	}
	if err := answerGates(ctx, client, sessionB, runtime, 90*time.Second); err != nil {
		*cases = append(*cases, caseResult{Name: "headless gated stream", Scope: scope, Status: "fail", Detail: err.Error()})
		return
	}
	events, err := waitJournalKinds(ctx, control, sessionB, len(fixtureStreamOrder), 90*time.Second)
	if err != nil {
		*cases = append(*cases, caseResult{Name: "headless gated stream", Scope: scope, Status: "fail", Detail: err.Error()})
		return
	}
	var kinds []string
	for _, event := range events {
		kinds = append(kinds, event.Kind)
	}
	if strings.Join(kinds, ",") != strings.Join(fixtureStreamOrder, ",") {
		*cases = append(*cases, caseResult{Name: "headless gated stream", Scope: scope, Status: "fail",
			Detail: fmt.Sprintf("order %q, want %q", strings.Join(kinds, ","), strings.Join(fixtureStreamOrder, ","))})
		return
	}
	*cases = append(*cases, caseResult{Name: "headless gated stream", Scope: scope, Status: "pass",
		Detail: fmt.Sprintf("%d events in canonical order with both gates answered over the wire", len(fixtureStreamOrder))})

	sessionRaw, err := control.call(ctx, "GET", "/session?sessionId="+sessionB)
	if err != nil {
		*cases = append(*cases, caseResult{Name: "fixture transcript persists", Scope: scope, Status: "fail", Detail: err.Error()})
		return
	}
	if !bytes.Contains(sessionRaw, []byte(fixtureStreamFull)) {
		*cases = append(*cases, caseResult{Name: "fixture transcript persists", Scope: scope, Status: "fail", Detail: "stored session lacks the full stream text"})
		return
	}
	*cases = append(*cases, caseResult{Name: "fixture transcript persists", Scope: scope, Status: "pass", Detail: "stored transcript holds the full stream text"})

	if err := fixtureSkeletonPreservesDetail(ctx, client, control, sessionB); err != nil {
		*cases = append(*cases, caseResult{Name: "checkpoint skeleton keeps transcript", Scope: scope, Status: "fail", Detail: err.Error()})
	} else {
		*cases = append(*cases, caseResult{Name: "checkpoint skeleton keeps transcript", Scope: scope, Status: "pass", Detail: "skeleton checkpoint with runtime_event_cursor kept messages, turns, and cursor"})
	}
}

// fixtureSkeletonPreservesDetail mirrors the autosave protocol: a
// saveTaskStateCheckpoint naming a skeleton session must keep the stored
// transcript and the session's runtime_event_cursor, while the reply itself
// carries no transcript.
func fixtureSkeletonPreservesDetail(ctx context.Context, client *daemon.Client, control fixtureControl, sessionID string) error {
	sessionRaw, err := control.call(ctx, "GET", "/session?sessionId="+sessionID)
	if err != nil {
		return fmt.Errorf("stored session: %w", err)
	}
	original := append([]byte(nil), sessionRaw...)
	defer func() {
		restoreCtx, cancel := context.WithTimeout(context.Background(), perRequestTimeout)
		defer cancel()
		_, _ = client.Request(restoreCtx, map[string]any{
			"type": "saveTaskState", "projects": []any{}, "sessions": []any{json.RawMessage(original)},
		})
	}()
	var stored map[string]any
	if err := json.Unmarshal(sessionRaw, &stored); err != nil {
		return fmt.Errorf("decode stored session: %w", err)
	}
	cursor := map[string]any{
		"runtime_id": "55555555-5555-5555-5555-555555555555",
		"epoch":      "66666666-6666-6666-6666-666666666666",
		"sequence":   7,
	}
	stored["runtime_event_cursor"] = cursor
	updated, err := json.Marshal(stored)
	if err != nil {
		return fmt.Errorf("encode cursor session: %w", err)
	}
	callCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	_, err = client.Request(callCtx, map[string]any{
		"type": "saveTaskState", "projects": []any{}, "sessions": []any{json.RawMessage(updated)},
	})
	cancel()
	if err != nil {
		return fmt.Errorf("save cursor session: %w", err)
	}
	stub := map[string]any{
		"id": sessionID, "title": "skeleton stub", "project_id": stored["project_id"],
		"status": "idle", "updated_at": stored["updated_at"],
	}
	stubRaw, _ := json.Marshal(stub)
	checkCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	payload, err := client.Request(checkCtx, map[string]any{
		"type": "saveTaskStateCheckpoint", "projects": []any{},
		"sessions": []any{json.RawMessage(stubRaw)}, "skeletonSessionIds": []string{sessionID},
	})
	cancel()
	if err != nil {
		return fmt.Errorf("checkpoint: %w", err)
	}
	var saved struct {
		Sessions []any `json:"sessions"`
	}
	if err := json.Unmarshal(payload, &saved); err != nil {
		return fmt.Errorf("decode checkpoint reply: %w", err)
	}
	if len(saved.Sessions) != 0 {
		return fmt.Errorf("checkpoint reply carries %d sessions, want no transcript", len(saved.Sessions))
	}
	hydrateCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	detail, err := client.Request(hydrateCtx, map[string]any{
		"type": "hydrateSession", "sessionId": sessionID,
		"environmentId": "00000000-0000-0000-0000-000000000000",
	})
	cancel()
	if err != nil {
		return fmt.Errorf("hydrate: %w", err)
	}
	var hydrated struct {
		Session map[string]any `json:"session"`
	}
	if err := json.Unmarshal(detail, &hydrated); err != nil || hydrated.Session == nil {
		return fmt.Errorf("hydrate returned no session")
	}
	kept, _ := json.Marshal(hydrated.Session["runtime_event_cursor"])
	want, _ := json.Marshal(cursor)
	if string(kept) != string(want) {
		return fmt.Errorf("runtime_event_cursor lost: got %s", string(kept))
	}
	if messages, _ := hydrated.Session["messages"].([]any); len(messages) == 0 {
		return fmt.Errorf("skeleton checkpoint erased the stored transcript")
	} else {
		raw, _ := json.Marshal(messages)
		if !bytes.Contains(raw, []byte(fixtureStreamFull)) {
			return fmt.Errorf("stored transcript lost the full stream text")
		}
	}
	return nil
}

// admissionTurn drives one turn exactly the way the app does over the wire:
// freeze the prompt with acceptSubmission, attach and start the runtime
// (which emits connected), then dispatchSubmission on that runtime. The
// dispatch path is what streams submissionUpdated before turnStarted, so
// the journal holds the canonical 16-kind admission order.
func admissionTurn(ctx context.Context, client *daemon.Client, sessionID, projectID, prompt string) (string, error) {
	callCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	payload, err := client.Request(callCtx, map[string]any{
		"type":          "hydrateSession",
		"sessionId":     sessionID,
		"environmentId": "00000000-0000-0000-0000-000000000000",
	})
	cancel()
	if err != nil {
		return "", fmt.Errorf("hydrate for accept: %w", err)
	}
	var detail struct {
		Session map[string]any `json:"session"`
	}
	if err := json.Unmarshal(payload, &detail); err != nil || detail.Session == nil {
		return "", fmt.Errorf("hydrate returned no session")
	}
	session := detail.Session
	turnID := "99999999-9999-9999-9999-999999999901"
	inputID := "99999999-9999-9999-9999-999999999902"
	session["detail_loaded"] = true
	turns, _ := session["turns"].([]any)
	turns = append(turns, map[string]any{
		"id": turnID, "status": "running", "provider_turn_started": false,
		"turn_count": 1, "started_at": uint64(time.Now().Unix()),
	})
	session["turns"] = turns
	envelopeProject, _ := session["project_id"].(string)
	if envelopeProject == "" {
		envelopeProject = projectID
	}
	acceptCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	_, err = client.RequestFor(acceptCtx, sessionID, "", map[string]any{
		"type":    "acceptSubmission",
		"project": map[string]any{"id": envelopeProject},
		"session": session,
		"submission": map[string]any{
			"session_id": sessionID,
			"receipt": map[string]any{
				"id": turnID, "input_id": inputID, "state": "preparing",
				"created_at": uint64(time.Now().UnixMilli()), "updated_at": uint64(time.Now().UnixMilli()),
			},
			"prompt": prompt,
			"files":  []any{},
		},
	})
	cancel()
	if err != nil {
		return "", fmt.Errorf("acceptSubmission: %w", err)
	}
	runtime, err := ensureFixtureRuntime(ctx, client, sessionID)
	if err != nil {
		return "", fmt.Errorf("runtime: %w", err)
	}
	dispatchCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	_, err = client.RequestFor(dispatchCtx, sessionID, runtime, map[string]any{
		"type":         "dispatchSubmission",
		"submissionId": turnID,
		"workspace":    map[string]any{"cwd": ""},
	})
	cancel()
	if err != nil {
		return "", fmt.Errorf("dispatchSubmission: %w", err)
	}
	return runtime, nil
}

func ensureFixtureRuntime(ctx context.Context, client *daemon.Client, sessionID string) (string, error) {
	callCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	payload, err := client.RequestFor(callCtx, sessionID, "", map[string]any{"type": "attachSession"})
	cancel()
	if err != nil {
		return "", err
	}
	var attached struct {
		RuntimeID *string `json:"runtimeId"`
	}
	if err := json.Unmarshal(payload, &attached); err != nil {
		return "", err
	}
	if attached.RuntimeID != nil && *attached.RuntimeID != "" {
		return *attached.RuntimeID, nil
	}
	runtime := "44444444-4444-4444-4444-444444444444"
	startCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	_, err = client.RequestFor(startCtx, sessionID, runtime, map[string]any{
		"type":          "start",
		"options":       map[string]any{"cwd": "", "mode": "fullAccess"},
		"environmentId": "00000000-0000-0000-0000-000000000000",
	})
	cancel()
	if err != nil {
		return "", err
	}
	return runtime, nil
}

// answerGates consumes driver events until turnFinished, answering the
// permission and question gates over the wire. Every consumed event is Acked
// so resume cursors advance exactly like the app advances them.
func answerGates(ctx context.Context, client *daemon.Client, sessionID, runtimeID string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case event, ok := <-client.Events():
			if !ok {
				return fmt.Errorf("event stream closed: %v", client.Err())
			}
			switch event.Kind {
			case "permission":
				var payload struct {
					RequestID string `json:"requestId"`
				}
				if err := json.Unmarshal(event.Payload, &payload); err != nil {
					client.Ack(event)
					continue
				}
				answerCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
				_, err := client.RequestFor(answerCtx, sessionID, runtimeID, map[string]any{
					"type": "respond", "requestId": payload.RequestID, "optionId": "allow",
				})
				cancel()
				client.Ack(event)
				if err != nil {
					return fmt.Errorf("respond: %w", err)
				}
			case "userInputRequested":
				var payload struct {
					RequestID string `json:"requestId"`
					Questions []struct {
						ID string `json:"id"`
					} `json:"questions"`
				}
				if err := json.Unmarshal(event.Payload, &payload); err != nil {
					client.Ack(event)
					continue
				}
				answers := make([]any, 0, len(payload.Questions))
				for _, question := range payload.Questions {
					answers = append(answers, map[string]any{"questionId": question.ID, "answers": []string{"Option A"}})
				}
				answerCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
				_, err := client.RequestFor(answerCtx, sessionID, runtimeID, map[string]any{
					"type": "respondUserInput", "requestId": payload.RequestID, "answers": answers,
				})
				cancel()
				client.Ack(event)
				if err != nil {
					return fmt.Errorf("respondUserInput: %w", err)
				}
			case "turnFinished":
				client.Ack(event)
				return nil
			default:
				client.Ack(event)
			}
		case <-time.After(500 * time.Millisecond):
			if time.Now().After(deadline) {
				return fmt.Errorf("turnFinished not observed within %s", timeout)
			}
		}
	}
}

func checkDraftSwitchNewTaskFixture(ctx context.Context, client *autoClient, windowID int, sessionA, sessionB string, cases *[]caseResult, roundDir string) {
	const scope = "actualUI+fixtureDaemon"
	before := len(*cases)
	checkDraftSwitchNewTask(ctx, client, windowID, sessionA, sessionB, cases, roundDir)
	for i := before; i < len(*cases); i++ {
		(*cases)[i].Scope = scope
	}
}

// checkFixtureStream sends a Chinese prompt on session A, answers permission
// and the question in the real window, then proves background completion,
// forced-disconnect resume, canonical activity order, and that turnFinished
// survives hydrate reselect.
func checkFixtureStream(ctx context.Context, client *autoClient, control fixtureControl, windowID int, sessionA, sessionB, dataDir string, cases *[]caseResult, roundDir string) {
	const scope = "actualUI+fixtureDaemon"
	push := func(name, status, detail string) {
		*cases = append(*cases, caseResult{Name: name, Scope: scope, Status: status, Detail: detail})
	}

	if err := selectSessionWait(ctx, client, windowID, sessionA, knownTitleA, seededBodyA); err != nil {
		push("send streams full text", "fail", "select session A: "+err.Error())
		blockRest(scope, cases)
		return
	}
	composer, err := client.tree(ctx, windowID)
	if err != nil {
		push("send streams full text", "fail", "tree: "+err.Error())
		blockRest(scope, cases)
		return
	}
	field := findTextField(composer, "Message")
	if field == nil {
		push("send streams full text", "fail", "composer vanished after wait")
		blockRest(scope, cases)
		return
	}
	if err := client.act(ctx, windowID, field.ID, "", "focus", ""); err != nil {
		push("send streams full text", "fail", "focus composer: "+err.Error())
		blockRest(scope, cases)
		return
	}
	if err := client.idle(ctx, windowID, 5000); err != nil {
		push("send streams full text", "fail", "idle after focus: "+err.Error())
		blockRest(scope, cases)
		return
	}
	if err := client.inputText(ctx, windowID, fixturePromptText); err != nil {
		push("send streams full text", "fail", "input prompt: "+err.Error())
		blockRest(scope, cases)
		return
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		got := findTextField(nodes, "Message")
		if got == nil || !strings.Contains(got.Value, fixturePromptText) {
			return fmt.Errorf("predicate: prompt text not staged in composer yet")
		}
		return nil
	}); err != nil {
		push("send streams full text", "fail", "prompt never staged: "+err.Error())
		blockRest(scope, cases)
		return
	}
	staged, err := client.tree(ctx, windowID)
	if err != nil {
		push("send streams full text", "fail", "tree: "+err.Error())
		blockRest(scope, cases)
		return
	}
	send := findButton(staged, "Send")
	if send == nil {
		push("send streams full text", "fail", "predicate: Send button not visible with staged prompt")
		blockRest(scope, cases)
		return
	}
	if err := client.act(ctx, windowID, send.ID, "", "press", ""); err != nil {
		push("send streams full text", "fail", "press Send: "+err.Error())
		blockRest(scope, cases)
		return
	}
	// The new binary checkpoints the full snapshot before attach/start, so
	// the journal must start moving once Send is pressed.
	if _, err := waitJournalKinds(ctx, control, sessionA, streamStartKinds(), 60*time.Second); err != nil {
		push("send streams full text", "fail", "turn on A never started streaming: "+err.Error())
		blockRest(scope, cases)
		return
	}
	// Switch to B while the A turn streams: the rest must complete in the
	// background and B's composer must stay clean of A's text.
	if err := selectSessionWait(ctx, client, windowID, sessionB, knownTitleB, seededBodyB); err != nil {
		push("background session completes", "fail", "select session B: "+err.Error())
		push("send streams full text", "fail", "select session B: "+err.Error())
		blockRest(scope, cases)
		return
	}
	if last, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		got := findTextField(nodes, "Message")
		if got == nil {
			return fmt.Errorf("composer not visible on session B")
		}
		if strings.Contains(got.Value, draftTextA) || strings.Contains(got.Value, fixturePromptText) {
			return fmt.Errorf("session A text leaked into session B")
		}
		return nil
	}); err != nil {
		push("background session completes", "fail", err.Error()+"; labels: "+visibleLabels(last))
		saveStageTree(roundDir, "background-fail")
	} else {
		push("background session completes", "pass", "A streams while B is selected and B stays clean")
	}
	if _, err := waitJournalKinds(ctx, control, sessionA, len(fixtureStreamOrder), 90*time.Second); err != nil {
		push("send streams full text", "fail", "turn on A never finished: "+err.Error())
		push("permission and question answered", "blocked", "BLOCKED: stream never reached the gates")
		push("activity order matches stream", "blocked", "BLOCKED: stream incomplete")
		push("disconnect resumes without loss", "blocked", "BLOCKED: first turn incomplete")
		push("turnFinished survives hydrate", "blocked", "BLOCKED: first turn incomplete")
		return
	}
	if err := selectSessionWait(ctx, client, windowID, sessionA, knownTitleA, seededBodyA); err != nil {
		push("send streams full text", "fail", "reselect session A: "+err.Error())
		blockRest(scope, cases)
		return
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if !anyValue(nodes, fixtureStreamFull) {
			return fmt.Errorf("full stream text not visible yet")
		}
		return nil
	}); err != nil {
		push("send streams full text", "fail", err.Error())
		blockRest(scope, cases)
		return
	}
	push("send streams full text", "pass", "prompt sent on A, background turn finished, full text visible")

	// Permission and question were answered over the app's own wire only if
	// the app consumed them; confirm the journal gates were passed and the
	// transcript kept the text.
	events, err := queryJournal(ctx, control, sessionA)
	if err != nil {
		push("permission and question answered", "fail", "journal: "+err.Error())
	} else {
		var kinds []string
		for _, event := range events {
			kinds = append(kinds, event.Kind)
		}
		if strings.Join(kinds, ",") != strings.Join(fixtureStreamOrder, ",") {
			push("permission and question answered", "fail",
				fmt.Sprintf("gate order %q, want %q", strings.Join(kinds, ","), strings.Join(fixtureStreamOrder, ",")))
		} else {
			push("permission and question answered", "pass", "gates passed in order, transcript visible in the real window")
		}
		orderName, orderStatus, orderDetail := journalOrderVerdict(kinds)
		push(orderName, orderStatus, orderDetail)
	}
	saveStageTree(roundDir, "fixture-stream")

	// Forced disconnect during a second turn, then resume without loss.
	if err := secondTurnWithDisconnect(ctx, client, control, windowID, sessionA, cases, roundDir); err != nil {
		push("disconnect resumes without loss", "fail", err.Error())
		push("turnFinished survives hydrate", "blocked", "BLOCKED: resume failed")
		return
	}
	push("disconnect resumes without loss", "pass", "forced disconnect replayed the journal with no gap and no duplicate")

	sessionRaw, err := control.call(ctx, "GET", "/session?sessionId="+sessionA)
	if err != nil {
		push("turnFinished survives hydrate", "fail", "stored session: "+err.Error())
		return
	}
	if count := bytes.Count(sessionRaw, []byte(fixtureStreamFull)); count < 2 {
		push("turnFinished survives hydrate", "fail", fmt.Sprintf("stored transcript holds %d full texts, want 2 turns", count))
		return
	}
	if err := selectSessionWait(ctx, client, windowID, sessionB, knownTitleB, seededBodyB); err != nil {
		push("turnFinished survives hydrate", "fail", "select B: "+err.Error())
		return
	}
	if err := selectSessionWait(ctx, client, windowID, sessionA, knownTitleA, seededBodyA); err != nil {
		push("turnFinished survives hydrate", "fail", "reselect A: "+err.Error())
		return
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if countValue(nodes, fixtureStreamFull) < 2 {
			return fmt.Errorf("reselected A shows fewer than 2 full texts (old hydrate rolled back?)")
		}
		return nil
	}); err != nil {
		push("turnFinished survives hydrate", "fail", err.Error())
		return
	}
	push("turnFinished survives hydrate", "pass", "stored transcript holds 2 turns, reselect keeps both")
	saveStageTree(roundDir, "fixture-final")
}

func blockRest(scope string, cases *[]caseResult) {
	seen := map[string]bool{}
	for _, c := range *cases {
		seen[c.Name] = true
	}
	for _, step := range []string{"background session completes", "send streams full text",
		"permission and question answered", "activity order matches stream",
		"disconnect resumes without loss", "turnFinished survives hydrate"} {
		if !seen[step] {
			*cases = append(*cases, caseResult{Name: step, Scope: scope, Status: "blocked", Detail: "BLOCKED: earlier step failed"})
		}
	}
}

func journalOrderVerdict(kinds []string) (string, string, string) {
	if strings.Join(kinds, ",") == strings.Join(fixtureStreamOrder, ",") {
		return "activity order matches stream", "pass", "connected through turnFinished in canonical order"
	}
	return "activity order matches stream", "fail",
		fmt.Sprintf("order %q, want %q", strings.Join(kinds, ","), strings.Join(fixtureStreamOrder, ","))
}

// secondTurnWithDisconnect prompts again, forces a transport drop mid-stream
// through the control plane, and requires the journal to stay gapless.
func secondTurnWithDisconnect(ctx context.Context, client *autoClient, control fixtureControl, windowID int, sessionA string, cases *[]caseResult, roundDir string) error {
	const scope = "actualUI+fixtureDaemon"
	before, err := queryJournal(ctx, control, sessionA)
	if err != nil {
		return fmt.Errorf("journal before second turn: %w", err)
	}
	composer, err := client.tree(ctx, windowID)
	if err != nil {
		return fmt.Errorf("tree: %w", err)
	}
	field := findTextField(composer, "Message")
	if field == nil {
		return fmt.Errorf("predicate: composer Message field not visible for the second prompt")
	}
	if err := client.act(ctx, windowID, field.ID, "", "focus", ""); err != nil {
		return fmt.Errorf("focus composer: %w", err)
	}
	if err := client.idle(ctx, windowID, 5000); err != nil {
		return fmt.Errorf("idle after focus: %w", err)
	}
	if err := client.inputText(ctx, windowID, fixtureSecondPrompt); err != nil {
		return fmt.Errorf("input second prompt: %w", err)
	}
	tree, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if !anyValue(nodes, fixtureSecondPrompt) {
			return fmt.Errorf("predicate: second prompt text not in composer yet")
		}
		return nil
	})
	if err != nil {
		return fmt.Errorf("second prompt never staged: %w", err)
	}
	send := findButton(tree, "Send")
	if send == nil {
		return fmt.Errorf("predicate: Send button not visible for the second prompt")
	}
	if err := client.act(ctx, windowID, send.ID, "", "press", ""); err != nil {
		return fmt.Errorf("press Send: %w", err)
	}
	// Wait until the new turn started streaming, then drop the transport.
	if _, err := waitJournalKinds(ctx, control, sessionA, len(before)+streamStartKinds(), 60*time.Second); err != nil {
		return fmt.Errorf("second turn never started streaming: %w", err)
	}
	dropCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	_, err = control.call(dropCtx, "POST", "/disconnect")
	cancel()
	if err != nil {
		return fmt.Errorf("control disconnect: %w", err)
	}
	// The second prompt reuses the attached runtime, so its stream carries
	// no fresh connected event: wait for one more turnFinished rather
	// than a fixed total.
	finishedBefore := 0
	for _, event := range before {
		if event.Kind == "turnFinished" {
			finishedBefore++
		}
	}
	events, err := waitTurnFinishedCount(ctx, control, sessionA, finishedBefore+1, 90*time.Second)
	if err != nil {
		return fmt.Errorf("resumed turn never finished: %w", err)
	}
	seen := map[uint64]int{}
	for _, event := range events {
		seen[event.Sequence]++
	}
	for sequence := uint64(1); sequence <= uint64(len(events)); sequence++ {
		if seen[sequence] != 1 {
			return fmt.Errorf("journal sequence %d appears %d times, want exactly once (%d events)", sequence, seen[sequence], len(events))
		}
	}
	// Answer any gates the app itself did not consume while disconnected.
	finished := 0
	for _, event := range events {
		if event.Kind == "turnFinished" {
			finished++
		}
	}
	if err := answerGatesForSession(ctx, control, sessionA, finished); err != nil {
		return err
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if countValue(nodes, fixtureStreamFull) < 2 {
			return fmt.Errorf("predicate: second full text not visible after resume yet")
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "disconnect UI shows resumed text", Scope: scope, Status: "blocked",
			Detail: "BLOCKED: journal is gapless but the window did not render the resumed text: " + err.Error()})
		saveStageTree(roundDir, "fixture-resume")
		return nil
	}
	saveStageTree(roundDir, "fixture-resume")
	return nil
}

// answerGatesForSession is a no-op when the app already answered the gates:
// the fixture only waits while a turn is still unfinished.
func answerGatesForSession(ctx context.Context, control fixtureControl, sessionID string, finishedBefore int) error {
	deadline := time.Now().Add(60 * time.Second)
	for {
		events, err := queryJournal(ctx, control, sessionID)
		if err != nil {
			return err
		}
		finished := 0
		for _, event := range events {
			if event.Kind == "turnFinished" {
				finished++
			}
		}
		if finished > finishedBefore {
			return nil
		}
		if time.Now().After(deadline) {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(500 * time.Millisecond):
		}
	}
}

// checkFilesListing opens the panel to the workspace file row. Literal save
// and conflict refusal stay in realUI mode; here they are recorded as
// blocked with the reason instead of a fake pass.
func checkFilesListing(ctx context.Context, client *autoClient, windowID int, cases *[]caseResult, roundDir string) {
	const scope = "actualUI+fixtureDaemon"
	tree, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findButton(nodes, "Toggle right panel") == nil {
			return fmt.Errorf("predicate: Toggle right panel not visible")
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "files surface lists hello.txt", Scope: scope, Status: "blocked", Detail: "BLOCKED integration: " + err.Error()})
		return
	}
	if err := client.act(ctx, windowID, findButton(tree, "Toggle right panel").ID, "", "press", ""); err != nil {
		*cases = append(*cases, caseResult{Name: "files surface lists hello.txt", Scope: scope, Status: "fail", Detail: "press toggle: " + err.Error()})
		return
	}
	tree, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if !anyLabel(nodes, "Files") && !anyLabel(nodes, "Open a surface") {
			return fmt.Errorf("predicate: Files surface or chooser not visible")
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "files surface lists hello.txt", Scope: scope, Status: "blocked", Detail: "BLOCKED integration: " + err.Error()})
		return
	}
	if filesButton := findButton(tree, "Files"); filesButton != nil {
		if err := client.act(ctx, windowID, filesButton.ID, "", "press", ""); err != nil {
			*cases = append(*cases, caseResult{Name: "files surface lists hello.txt", Scope: scope, Status: "fail", Detail: "press Files: " + err.Error()})
			return
		}
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findButton(nodes, knownFile) == nil {
			return fmt.Errorf("predicate: workspace row %s not visible", knownFile)
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "files surface lists hello.txt", Scope: scope, Status: "blocked", Detail: "BLOCKED integration: " + err.Error()})
		return
	}
	*cases = append(*cases, caseResult{Name: "files surface lists hello.txt", Scope: scope, Status: "pass", Detail: "workspace row visible in the real window"})
	saveStageTree(roundDir, "fixture-files")
}

func anyValue(nodes []autoNode, text string) bool {
	for i := range nodes {
		if strings.Contains(nodes[i].Value, text) || strings.Contains(nodes[i].Label, text) {
			return true
		}
	}
	return false
}

func countValue(nodes []autoNode, text string) int {
	count := 0
	for i := range nodes {
		if strings.Contains(nodes[i].Value, text) || strings.Contains(nodes[i].Label, text) {
			count++
		}
	}
	return count
}

func mustNodes(nodes []autoNode, err error) []autoNode {
	if err != nil {
		return nil
	}
	return nodes
}

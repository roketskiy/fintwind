// Command uiverify drives the real e2e-built app window from outside over
// the MyGo automation socket (tree/act/input only, never capture). It seeds
// two sessions plus one workspace file with a real daemon, stops that seed,
// then opens the app on the same data directory. Scope stays "realUI".
// Right-panel and file-editor checks report BLOCKED while that integration
// lands elsewhere; this runner never edits product code.
package main

import (
	"bufio"
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net"
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
	perRequestTimeout = 15 * time.Second
	socketWaitTimeout = 60 * time.Second
	treePollTimeout   = 45 * time.Second
	daemonReadyWait   = 30 * time.Second
)

const (
	knownTitleA = "Alpha task uiverify"
	knownTitleB = "Beta task uiverify"
	knownFile   = "hello.txt"
	knownText   = "uiverify known workspace text 7391"
	draftTextA  = "草稿-alpha-uiverify-4821"

	// Seed ids are fixed so every artifact names the same sessions.
	seedProjectID = "11111111-1111-1111-1111-111111111111"
	seedSessionA  = "22222222-2222-2222-2222-222222222222"
	seedSessionB  = "33333333-3333-3333-3333-333333333333"

	// Seeded turn bodies the UI must show literally after hydrate; titles
	// alone prove nothing about the transcript.
	seededBodyA = "uiverify-seeded-alpha-body-7391"
	seededBodyB = "uiverify-seeded-beta-body-9273"

	// crlf.txt opens the line-ending preservation checks: a CRLF file must
	// open clean (no save button) and a saved edit must keep CRLF bytes.
	crlfFile    = "crlf.txt"
	crlfTextLF  = "crlf line one uiverify 5311\nline two stays crlf\n"
	crlfTextCRL = "crlf line one uiverify 5311\r\nline two stays crlf\r\n"
	crlfEditLF  = "crlf line one uiverify 5311 edited\nline two stays crlf\n"
	crlfEditCRL = "crlf line one uiverify 5311 edited\r\nline two stays crlf\r\n"
)

type caseResult struct {
	Name   string `json:"name"`
	Scope  string `json:"scope"`
	Status string `json:"status"`
	Detail string `json:"detail"`
}

type verifyResult struct {
	Mode  string       `json:"mode"`
	Cases []caseResult `json:"cases"`
}

func main() {
	appPath := flag.String("app", "", "path to e2e-built app exe (go build -tags e2e ./cmd/app)")
	daemonPath := flag.String("daemon", "", "path to daemon exe (default: real daemon in real mode, fixturedaemon in fixture mode)")
	mode := flag.String("mode", "real", "verify mode: real (realUI) or fixture (actualUI+fixtureDaemon)")
	total := flag.Duration("timeout", 180*time.Second, "total run deadline")
	out := flag.String("out", "", "result.json path (defaults to <roundDir>/result.json)")
	flag.Parse()

	if *mode != "real" && *mode != "fixture" {
		fail("unknown -mode %q: want real or fixture", *mode)
	}
	daemonExe := *daemonPath
	if daemonExe == "" {
		if *mode == "fixture" {
			daemonExe = defaultFixturePath()
		} else {
			daemonExe = defaultDaemonPath()
		}
	}

	ctx, cancel := context.WithTimeout(context.Background(), *total)
	defer cancel()

	roundDir, err := ownedprocess.NewRoundDir()
	if err != nil {
		fail("create round dir: %v", err)
	}
	fmt.Printf("round dir: %s\n", roundDir)
	fmt.Printf("mode: %s\n", *mode)

	result := &verifyResult{Mode: modeName(*mode)}
	var cases []caseResult
	if *mode == "fixture" {
		cases = fillMissing("actualUI+fixtureDaemon", runFixtureUI(ctx, *appPath, daemonExe, roundDir), fixtureFlowSteps)
	} else {
		cases = fillMissing("realUI", runRealUI(ctx, *appPath, daemonExe, roundDir), realFlowSteps)
	}
	cases = append(cases, binaryStability(*appPath, cases))
	for _, c := range cases {
		report(c)
		result.Cases = append(result.Cases, c)
	}

	treePath := filepath.Join(roundDir, "automation-tree.json")
	if lastTreeJSON != "" {
		_ = os.WriteFile(treePath, []byte(lastTreeJSON+"\n"), 0o644)
		fmt.Printf("automation tree: %s\n", treePath)
	}

	path := *out
	if path == "" {
		path = filepath.Join(roundDir, "result.json")
	}
	raw, _ := json.MarshalIndent(result, "", "  ")
	if err := os.WriteFile(path, append(raw, '\n'), 0o644); err != nil {
		fail("write result: %v", err)
	}
	fmt.Printf("result: %s\n", path)
	fmt.Printf("seed logs: %s %s\n", filepath.Join(roundDir, "seed", "child-stdout.log"), filepath.Join(roundDir, "seed", "child-stderr.log"))
	fmt.Printf("app logs: %s %s\n", filepath.Join(roundDir, "apprun", "child-stdout.log"), filepath.Join(roundDir, "apprun", "child-stderr.log"))

	for _, c := range result.Cases {
		if c.Status != "pass" {
			os.Exit(1)
		}
	}
}

func report(c caseResult) {
	status := "PASS"
	if c.Status == "blocked" {
		status = "BLOCKED"
	} else if c.Status != "pass" {
		status = "FAIL"
	}
	fmt.Printf("%s [%s] %s: %s\n", status, c.Scope, c.Name, c.Detail)
}

func fail(format string, args ...any) {
	fmt.Printf("FAIL %s\n", fmt.Sprintf(format, args...))
	os.Exit(1)
}

var realFlowSteps = []string{
	"e2e app binary state", "real daemon binary state",
	"seed two sessions and workspace file",
	"seed transcript hydrates literal text", "seed persisted to disk",
	"app start with automation socket", "app window opens",
	"seeded sessions load known text",
	"composer draft input", "switch session restores drafts", "new task keeps drafts separate",
	"right panel opens", "files surface opens hello.txt",
	"file edit saves to disk", "external same-size edit blocks save",
	"crlf file opens clean", "crlf file save keeps line endings",
}

var fixtureFlowSteps = []string{
	"e2e app binary state", "fixture daemon binary state",
	"fixture hello identity", "fixture seed two sessions",
	"seed transcript hydrates literal text", "seed persisted to disk",
	"unknown command errors", "getSettings answers",
	"headless gated stream", "fixture transcript persists",
	"checkpoint skeleton keeps transcript",
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

// fillMissing records every ordered step that never ran as blocked, so
// result.json never silently drops an unexecuted step.
func fillMissing(scope string, cases []caseResult, ordered []string) []caseResult {
	seen := map[string]bool{}
	for _, c := range cases {
		seen[c.Name] = true
	}
	for _, name := range ordered {
		if !seen[name] {
			cases = append(cases, caseResult{Name: name, Scope: scope, Status: "blocked",
				Detail: "BLOCKED: earlier step failed, not executed"})
		}
	}
	return cases
}

func binaryStateDetail(path string) (string, error) {
	info, err := os.Stat(path)
	if err != nil {
		return "", err
	}
	digest, err := binaryHash(path)
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("size=%d mtime=%s sha256=%s", info.Size(), info.ModTime().UTC().Format(time.RFC3339), digest), nil
}

// binaryHash identifies the exact binary bytes behind a run.
func binaryHash(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	sum := sha256.New()
	if _, err := io.Copy(sum, file); err != nil {
		return "", err
	}
	return hex.EncodeToString(sum.Sum(nil))[:16], nil
}

// binaryStability re-stats the app binary after the run: a rebuild midway
// invalidates every UI observation, so the round must say so instead of
// reporting the old binary's behavior as current.
func binaryStability(appPath string, cases []caseResult) caseResult {
	scope := "realUI"
	for _, c := range cases {
		if c.Name == "e2e app binary state" {
			scope = c.Scope
			if c.Status != "pass" {
				return caseResult{Name: "binary stable across run", Scope: scope, Status: "blocked",
					Detail: "BLOCKED: no baseline binary state"}
			}
			detail, err := binaryStateDetail(appPath)
			if err != nil {
				return caseResult{Name: "binary stable across run", Scope: scope, Status: "fail",
					Detail: "app binary vanished mid-run: " + err.Error()}
			}
			if detail != c.Detail {
				return caseResult{Name: "binary stable across run", Scope: scope, Status: "fail",
					Detail: "app binary changed mid-run (" + c.Detail + " -> " + detail + "): round is stale, rerun"}
			}
			return caseResult{Name: "binary stable across run", Scope: scope, Status: "pass", Detail: detail}
		}
	}
	return caseResult{Name: "binary stable across run", Scope: scope, Status: "blocked",
		Detail: "BLOCKED: no baseline binary state"}
}

func modeName(mode string) string {
	if mode == "fixture" {
		return "actualUI+fixtureDaemon"
	}
	return "realUI"
}

func defaultFixturePath() string {
	if executable, err := os.Executable(); err == nil {
		candidate := filepath.Join(filepath.Dir(executable), "..", "fixturedaemon", "fixturedaemon.exe")
		if absolute, err := filepath.Abs(candidate); err == nil {
			if _, err := os.Stat(absolute); err == nil {
				return absolute
			}
		}
	}
	candidates := []string{
		filepath.Join("..", "fixturedaemon", "fixturedaemon.exe"),
		filepath.Join("cmd", "fixturedaemon", "fixturedaemon.exe"),
	}
	for _, candidate := range candidates {
		if _, err := os.Stat(candidate); err == nil {
			absolute, _ := filepath.Abs(candidate)
			return absolute
		}
	}
	return candidates[0]
}

func defaultDaemonPath() string {
	candidates := []string{
		filepath.Join("..", "..", "target", "release", "fintwind-daemon.exe"),
		filepath.Join("..", "target", "release", "fintwind-daemon.exe"),
	}
	for _, candidate := range candidates {
		if _, err := os.Stat(candidate); err == nil {
			absolute, _ := filepath.Abs(candidate)
			return absolute
		}
	}
	return candidates[0]
}

func runRealUI(ctx context.Context, appPath, daemonExe, roundDir string) []caseResult {
	const scope = "realUI"
	cases := []caseResult{}
	if detail, err := binaryStateDetail(appPath); err != nil {
		cases = append(cases, caseResult{Name: "e2e app binary state", Scope: scope, Status: "fail",
			Detail: fmt.Sprintf("app binary missing: %s", appPath)})
		return cases
	} else {
		cases = append(cases, caseResult{Name: "e2e app binary state", Scope: scope, Status: "pass", Detail: detail})
	}
	if detail, err := binaryStateDetail(daemonExe); err != nil {
		cases = append(cases, caseResult{Name: "real daemon binary state", Scope: scope, Status: "fail",
			Detail: fmt.Sprintf("daemon binary missing: %s", daemonExe)})
		return cases
	} else {
		cases = append(cases, caseResult{Name: "real daemon binary state", Scope: scope, Status: "pass", Detail: detail})
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
	if err := os.WriteFile(filepath.Join(workspace, crlfFile), []byte(crlfTextCRL), 0o644); err != nil {
		cases = append(cases, caseResult{Name: "prepare workspace", Scope: scope, Status: "fail", Detail: err.Error()})
		return cases
	}
	seedDir := filepath.Join(roundDir, "seed")
	appDir := filepath.Join(roundDir, "apprun")
	if err := os.MkdirAll(seedDir, 0o755); err != nil {
		cases = append(cases, caseResult{Name: "prepare dirs", Scope: scope, Status: "fail", Detail: err.Error()})
		return cases
	}
	if err := os.MkdirAll(appDir, 0o755); err != nil {
		cases = append(cases, caseResult{Name: "prepare dirs", Scope: scope, Status: "fail", Detail: err.Error()})
		return cases
	}

	sessionA, sessionB, ok := seedSessions(ctx, daemonExe, seedDir, dataDir, workspace, scope, &cases)
	if !ok {
		return cases
	}

	sockPath, ownedApp, client, ok := startApp(ctx, appPath, daemonExe, appDir, dataDir, scope, &cases)
	if !ok {
		return cases
	}
	defer ownedApp.Close()
	defer client.close()
	_ = sockPath

	windowID, ok := waitWindow(ctx, client, scope, &cases)
	if !ok {
		return cases
	}

	checkDraftSwitchNewTask(ctx, client, windowID, sessionA, sessionB, &cases, roundDir)
	checkRightPanelAndFiles(ctx, client, windowID, workspace, &cases, roundDir)
	return cases
}

// seedSessionsPayload builds the saveTaskState command: one project, two
// sessions that each carry one settled user turn with a literal body, so
// hydrate has real text to show. A's stamp is newer, fixing the initial
// selection and the sidebar order deterministically.
func seedSessionsPayload(workspace string, now uint64) map[string]any {
	session := func(id, title, body, turnID, messageID string, stamp uint64) map[string]any {
		return map[string]any{
			"id": id, "title": title, "project_id": seedProjectID,
			"provider": "opencode", "model": "fixture/context-model",
			"native_session_id": "native-session-" + strings.ToLower(strings.Fields(title)[0]),
			"context_window":    "removed-window-option",
			"context_usage":     map[string]any{"tokens": 0, "measured": false},
			"runtime_mode":      "fullAccess", "interaction_mode": "build",
			"status": "idle", "created_at": now, "updated_at": stamp,
			"last_reply_at": stamp, "mermaid_prompt_version": "v4",
			"turns": []any{map[string]any{
				"id": turnID, "turn_count": 1, "status": "completed",
				"started_at": now, "completed_at": stamp,
			}},
			"messages": []any{map[string]any{
				"id": messageID, "turn_id": turnID, "role": "user",
				"content": body, "created_at": now, "streaming": false,
			}},
		}
	}
	return map[string]any{
		"type": "saveTaskState",
		"projects": []any{map[string]any{
			"id": seedProjectID, "name": "uiverify", "path": workspace,
			"environment_id": "", "created_at": now,
		}},
		"liveSessionIds": []string{seedSessionA},
		"sessions": []any{
			session(seedSessionA, knownTitleA, seededBodyA,
				"77777777-7777-7777-7777-777777777777", "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaa1", now+30),
			session(seedSessionB, knownTitleB, seededBodyB,
				"88888888-8888-8888-8888-888888888888", "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbb1", now+29),
		},
	}
}

// assertSeedHydrate proves the daemon stored the literal body: hydrate must
// return the text the UI will later have to show.
func assertSeedHydrate(ctx context.Context, client *daemon.Client, sessionID, body string) error {
	callCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	defer cancel()
	payload, err := client.Request(callCtx, map[string]any{
		"type":          "hydrateSession",
		"sessionId":     sessionID,
		"environmentId": "00000000-0000-0000-0000-000000000000",
	})
	if err != nil {
		return err
	}
	if !bytes.Contains(payload, []byte(body)) {
		return fmt.Errorf("hydrated session %s lacks body %q", shortSession(sessionID), body)
	}
	return nil
}

// storeHoldsSessions checks the daemon's own store files for both seeded
// session UUID bytes and names the file that held them. SQLite keeps fresh
// writes in the -wal journal until a checkpoint, so the journal is scanned
// alongside the main database; it reads only the current round's data
// directory.
func storeHoldsSessions(dataDir string, names []string) (string, error) {
	partial := ""
	for _, name := range names {
		raw, err := os.ReadFile(filepath.Join(dataDir, name))
		if err != nil {
			continue
		}
		gotA := bytes.Contains(raw, []byte(seedSessionA))
		gotB := bytes.Contains(raw, []byte(seedSessionB))
		if gotA && gotB {
			return name, nil
		}
		if gotA || gotB {
			partial += fmt.Sprintf("%s(A=%v,B=%v) ", name, gotA, gotB)
		}
	}
	if partial != "" {
		return "", fmt.Errorf("sessions split across stores: %s", partial)
	}
	return "", fmt.Errorf("no readable store among %v holds the sessions", names)
}

// seedDiagnosis summarizes on-disk truth and daemon stderr when the UI shows
// no seeded rows, so the failure names its layer instead of a guess.
func seedDiagnosis(roundDir string) string {
	store, storeErr := storeHoldsSessions(filepath.Join(roundDir, "data"),
		[]string{"app.db", "app.db-wal"})
	detail := fmt.Sprintf("store evidence: %q, err: %v", store, storeErr)
	if tail := readTail(filepath.Join(roundDir, "apprun", "child-stderr.log"), 1200); tail != "" {
		detail += "; daemon stderr tail: " + tail
	}
	return detail
}

// readTail returns at most the last maxBytes of a log file as one line.
func readTail(path string, maxBytes int64) string {
	file, err := os.Open(path)
	if err != nil {
		return ""
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return ""
	}
	offset := info.Size() - maxBytes
	if offset < 0 {
		offset = 0
	}
	raw := make([]byte, info.Size()-offset)
	if _, err := file.ReadAt(raw, offset); err != nil {
		return ""
	}
	return strings.ReplaceAll(strings.TrimSpace(string(raw)), "\n", " | ")
}

// seedSessions starts the true daemon in its own Job, saves one project with
// two sessions carrying literal turn bodies and a workspace file, verifies
// the round-trip and the stored transcript, proves the store on disk, then
// stops the seed so the app later owns the same data directory alone.
func seedSessions(ctx context.Context, daemonExe, seedDir, dataDir, workspace, scope string, cases *[]caseResult) (string, string, bool) {
	if scope == "" {
		scope = "realUI"
	}
	token, err := randomToken()
	if err != nil {
		*cases = append(*cases, caseResult{Name: "seed token", Scope: scope, Status: "fail", Detail: err.Error()})
		return "", "", false
	}
	owned, err := ownedprocess.Start(daemonExe,
		[]string{"--bind", "127.0.0.1:0", "--data-dir", dataDir, "--parent-pid", strconv.Itoa(os.Getpid())},
		seedDir,
		map[string]string{"FINTWIND_DAEMON_TOKEN": token})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "seed daemon start (owned)", Scope: scope, Status: "fail", Detail: err.Error()})
		return "", "", false
	}
	defer owned.Close()

	ready, err := waitReadyLine(ctx, filepath.Join(seedDir, "child-stdout.log"), daemonReadyWait)
	if err != nil {
		*cases = append(*cases, caseResult{Name: "seed daemon ready", Scope: scope, Status: "fail", Detail: err.Error()})
		return "", "", false
	}
	dialCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	client, err := daemon.Dial(dialCtx, ready.Address, token)
	cancel()
	if err != nil {
		*cases = append(*cases, caseResult{Name: "seed daemon dial", Scope: scope, Status: "fail", Detail: err.Error()})
		return "", "", false
	}
	defer client.Close()

	now := uint64(time.Now().Unix())
	save := seedSessionsPayload(workspace, now)
	if err := requestDaemon(ctx, client, save); err != nil {
		*cases = append(*cases, caseResult{Name: "seed saveTaskState", Scope: scope, Status: "fail", Detail: err.Error()})
		return "", "", false
	}
	var state model.TaskState
	if err := requestDaemonInto(ctx, client, map[string]any{"type": "loadTaskState"}, &state); err != nil {
		*cases = append(*cases, caseResult{Name: "seed loadTaskState", Scope: scope, Status: "fail", Detail: err.Error()})
		return "", "", false
	}
	if len(state.Projects) != 1 || len(state.Sessions) != 2 {
		*cases = append(*cases, caseResult{Name: "seed round-trip", Scope: scope, Status: "fail",
			Detail: fmt.Sprintf("want 1 project and 2 sessions, got %d and %d", len(state.Projects), len(state.Sessions))})
		return "", "", false
	}
	*cases = append(*cases, caseResult{Name: "seed two sessions and workspace file", Scope: scope, Status: "pass",
		Detail: "1 project, 2 sessions with settled user turns, hello.txt and crlf.txt with known text"})

	if err := assertSeedHydrate(ctx, client, seedSessionA, seededBodyA); err != nil {
		*cases = append(*cases, caseResult{Name: "seed transcript hydrates literal text", Scope: scope, Status: "fail",
			Detail: "session A: " + err.Error()})
		return "", "", false
	}
	if err := assertSeedHydrate(ctx, client, seedSessionB, seededBodyB); err != nil {
		*cases = append(*cases, caseResult{Name: "seed transcript hydrates literal text", Scope: scope, Status: "fail",
			Detail: "session B: " + err.Error()})
		return "", "", false
	}
	*cases = append(*cases, caseResult{Name: "seed transcript hydrates literal text", Scope: scope, Status: "pass",
		Detail: "both bodies return literally from hydrateSession"})

	owned.Close()
	name, err := storeHoldsSessions(dataDir, []string{"app.db", "app.db-wal"})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "seed persisted to disk", Scope: scope, Status: "fail", Detail: err.Error()})
		return "", "", false
	}
	*cases = append(*cases, caseResult{Name: "seed persisted to disk", Scope: scope, Status: "pass",
		Detail: "both session UUIDs found in " + name})
	return seedSessionA, seedSessionB, true
}

// startApp launches the e2e-built app in its own Job with MYGO_AUTOMATION
// pointing at a fresh socket, then dials it. The seed daemon is already
// closed, so only this Job's daemon touches the data directory.
func startApp(ctx context.Context, appPath, daemonExe, appDir, dataDir, scope string, cases *[]caseResult) (string, *ownedprocess.Owned, *autoClient, bool) {
	if scope == "" {
		scope = "realUI"
	}
	sockPath := filepath.Join(appDir, "a.sock")
	_ = os.Remove(sockPath)
	owned, err := ownedprocess.Start(appPath,
		[]string{"-data-dir", dataDir, "-daemon", daemonExe},
		appDir,
		map[string]string{"MYGO_AUTOMATION": sockPath})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "app start (owned)", Scope: scope, Status: "fail", Detail: err.Error()})
		return "", nil, nil, false
	}
	deadline := time.Now().Add(socketWaitTimeout)
	for {
		if _, err := os.Stat(sockPath); err == nil {
			break
		}
		if time.Now().After(deadline) {
			owned.Close()
			*cases = append(*cases, caseResult{Name: "automation socket", Scope: scope, Status: "fail",
				Detail: fmt.Sprintf("no socket at %s within %s (app not e2e-built?); see apprun logs", sockPath, socketWaitTimeout)})
			return "", nil, nil, false
		}
		select {
		case <-ctx.Done():
			owned.Close()
			*cases = append(*cases, caseResult{Name: "automation socket", Scope: scope, Status: "fail", Detail: ctx.Err().Error()})
			return "", nil, nil, false
		case <-time.After(200 * time.Millisecond):
		}
	}
	client, err := dialAuto(sockPath)
	if err != nil {
		owned.Close()
		*cases = append(*cases, caseResult{Name: "automation dial", Scope: scope, Status: "fail", Detail: err.Error()})
		return "", nil, nil, false
	}
	*cases = append(*cases, caseResult{Name: "app start with automation socket", Scope: scope, Status: "pass",
		Detail: "owned job, isolated data dir, socket dialed"})
	return sockPath, owned, client, true
}

func waitWindow(ctx context.Context, client *autoClient, scope string, cases *[]caseResult) (int, bool) {
	if scope == "" {
		scope = "realUI"
	}
	deadline := time.Now().Add(treePollTimeout)
	for {
		windows, err := client.windows(ctx)
		if err == nil && len(windows) > 0 {
			*cases = append(*cases, caseResult{Name: "app window opens", Scope: scope, Status: "pass",
				Detail: fmt.Sprintf("%d window(s), first title %q", len(windows), windows[0].Title)})
			return windows[0].ID, true
		}
		if time.Now().After(deadline) {
			detail := "no window within deadline"
			if err != nil {
				detail = err.Error()
			}
			*cases = append(*cases, caseResult{Name: "app window opens", Scope: scope, Status: "fail", Detail: detail})
			return 0, false
		}
		select {
		case <-ctx.Done():
			*cases = append(*cases, caseResult{Name: "app window opens", Scope: scope, Status: "fail", Detail: ctx.Err().Error()})
			return 0, false
		case <-time.After(500 * time.Millisecond):
		}
	}
}

// selectSession opens a session row by its stable automation key
// ("session:<id>"), falling back to the bare id key and then to the title
// label. The label fallback refuses menu items and off-screen nodes so a
// context menu entry can never take the press. It reports which address
// matched so a miss names the exact miss; callers then prove the selection
// settled with waitSessionSelected.
func selectSession(ctx context.Context, client *autoClient, windowID int, sessionID, title string) error {
	nodes, err := client.tree(ctx, windowID)
	if err != nil {
		return fmt.Errorf("tree: %w", err)
	}
	lastTreeJSON = desensitizedTree(windowID, nodes)
	for _, node := range nodes {
		if keyString(node.Key) == "session:"+sessionID && strings.Contains(node.Role, "button") {
			if err := client.act(ctx, windowID, node.ID, "", "press", ""); err == nil {
				return nil
			}
		}
	}
	for _, node := range nodes {
		if keyString(node.Key) == "session:"+sessionID || (sessionID != "" && keyString(node.Key) == sessionID) {
			if err := client.act(ctx, windowID, node.ID, "", "press", ""); err == nil {
				return nil
			}
		}
	}
	for _, node := range nodes {
		if node.Label == title && strings.Contains(node.Role, "button") &&
			!strings.Contains(node.Role, "menu") && node.Bounds != nil {
			if err := client.act(ctx, windowID, node.ID, "", "press", ""); err == nil {
				return nil
			}
		}
	}
	return fmt.Errorf("no session element for key session:%s, key %s, or title %q (no_element)", shortSession(sessionID), shortSession(sessionID), title)
}

// findRow locates the sidebar list row of one session by its stable key.
func findRow(nodes []autoNode, sessionID string) *autoNode {
	want := "session:" + sessionID
	for i := range nodes {
		if keyString(nodes[i].Key) == want {
			return &nodes[i]
		}
	}
	return nil
}

// hasAction reports whether the node advertises an automation action such
// as "setValue"; a group without it silently no-ops a setValue.
func hasAction(n *autoNode, action string) bool {
	if n == nil {
		return false
	}
	for _, name := range n.Actions {
		if name == action {
			return true
		}
	}
	return false
}

// findTextContaining locates the first text-role node whose value or label
// holds probe; the transcript and the editor buffer both expose their text
// this way.
func findTextContaining(nodes []autoNode, probe string) *autoNode {
	for i := range nodes {
		n := &nodes[i]
		if strings.Contains(n.Role, "text") &&
			(strings.Contains(n.Value, probe) || strings.Contains(n.Label, probe)) {
			return n
		}
	}
	return nil
}

// waitSessionSelected proves one session is open and settled: its sidebar
// row is in the tree, the composer is visible, and the seeded body text is
// on screen. Sidebar rows carry no selected state bit, so the literal body
// is the evidence that hydration finished; a press that never opened the
// session times out here with the last tree saved for review.
func waitSessionSelected(ctx context.Context, client *autoClient, windowID int, sessionID, body string) ([]autoNode, error) {
	return waitTreeFor(ctx, client, windowID, 30*time.Second, func(nodes []autoNode) error {
		row := findRow(nodes, sessionID)
		if row == nil {
			return fmt.Errorf("session row %s not in tree", shortSession(sessionID))
		}
		if findTextField(nodes, "Message") == nil {
			return fmt.Errorf("composer Message field not visible")
		}
		if !anyValue(nodes, body) {
			return fmt.Errorf("seeded body %q not visible yet", body)
		}
		return nil
	})
}

// selectSessionWait presses a session row and returns only once the app has
// settled on it: hydration finished, the transcript holds the seeded body,
// and pending frames drained. Typing or asserting before this point races
// the in-flight hydrate and loses drafts.
func selectSessionWait(ctx context.Context, client *autoClient, windowID int, sessionID, title, body string) error {
	if err := selectSession(ctx, client, windowID, sessionID, title); err != nil {
		return err
	}
	nodes, err := waitSessionSelected(ctx, client, windowID, sessionID, body)
	if err != nil {
		return err
	}
	lastTreeJSON = desensitizedTree(windowID, nodes)
	if err := client.idle(ctx, windowID, 5000); err != nil {
		return fmt.Errorf("idle after selection: %w", err)
	}
	return nil
}

// checkDraftSwitchNewTask covers: known text loads, composer draft input
// (Chinese via automation input text; real IME composition is platform
// unverified), session switch restore, and new-task draft isolation. Every
// selection goes through selectSessionWait: the app hydrates sessions
// asynchronously, and typing into an in-flight selection loses the draft.
func checkDraftSwitchNewTask(ctx context.Context, client *autoClient, windowID int, sessionA, sessionB string, cases *[]caseResult, roundDir string) {
	const scope = "realUI"

	tree, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if !anyLabel(nodes, knownTitleA) || !anyLabel(nodes, knownTitleB) {
			return fmt.Errorf("seeded titles not both visible yet")
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "seeded sessions load known text", Scope: scope, Status: "fail",
			Detail: err.Error() + "; visible labels: " + visibleLabels(tree) + "; " + seedDiagnosis(roundDir)})
		saveStageTree(roundDir, "boot-no-rows")
		return
	}
	saveStageTree(roundDir, "boot")

	// Select A explicitly and wait for the settled evidence: the initial
	// selection follows the newest stamp, never assume it landed on A.
	if err := selectSessionWait(ctx, client, windowID, sessionA, knownTitleA, seededBodyA); err != nil {
		*cases = append(*cases, caseResult{Name: "seeded sessions load known text", Scope: scope, Status: "fail",
			Detail: "session A never settled: " + err.Error() + "; visible labels: " + visibleLabels(tree) + "; " + seedDiagnosis(roundDir)})
		saveStageTree(roundDir, "boot-no-rows")
		return
	}
	*cases = append(*cases, caseResult{Name: "seeded sessions load known text", Scope: scope, Status: "pass",
		Detail: fmt.Sprintf("%d nodes, both titles visible, session A row selected with literal body", len(tree))})

	composer, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findTextField(nodes, "Message") == nil {
			return fmt.Errorf("composer Message field not visible yet")
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "composer draft input", Scope: scope, Status: "fail", Detail: err.Error()})
		return
	}
	field := findTextField(composer, "Message")
	if err := client.act(ctx, windowID, field.ID, "", "focus", ""); err != nil {
		*cases = append(*cases, caseResult{Name: "composer draft input", Scope: scope, Status: "fail", Detail: "focus composer: " + err.Error()})
		return
	}
	if err := client.idle(ctx, windowID, 5000); err != nil {
		*cases = append(*cases, caseResult{Name: "composer draft input", Scope: scope, Status: "fail", Detail: "idle after focus: " + err.Error()})
		return
	}
	if err := client.inputText(ctx, windowID, draftTextA); err != nil {
		*cases = append(*cases, caseResult{Name: "composer draft input", Scope: scope, Status: "fail", Detail: "input text: " + err.Error()})
		return
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		got := findTextField(nodes, "Message")
		if got == nil || !strings.Contains(got.Value, draftTextA) {
			return fmt.Errorf("draft text not in composer yet")
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "composer draft input", Scope: scope, Status: "fail",
			Detail: err.Error() + " (input method: automation text event; real IME composition unverified)"})
		return
	}
	*cases = append(*cases, caseResult{Name: "composer draft input", Scope: scope, Status: "pass",
		Detail: "typed draft visible via automation text event (real IME composition unverified)"})

	if err := selectSessionWait(ctx, client, windowID, sessionB, knownTitleB, seededBodyB); err != nil {
		*cases = append(*cases, caseResult{Name: "switch session restores drafts", Scope: scope, Status: "fail", Detail: "open session B: " + err.Error()})
		saveStageTree(roundDir, "drafts-switch-fail")
		return
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		got := findTextField(nodes, "Message")
		if got == nil {
			return fmt.Errorf("composer not visible after switch")
		}
		if strings.Contains(got.Value, draftTextA) {
			return fmt.Errorf("session A draft leaked into session B")
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "switch session restores drafts", Scope: scope, Status: "fail", Detail: err.Error()})
		saveStageTree(roundDir, "drafts-switch-fail")
		return
	}
	if err := selectSessionWait(ctx, client, windowID, sessionA, knownTitleA, seededBodyA); err != nil {
		*cases = append(*cases, caseResult{Name: "switch session restores drafts", Scope: scope, Status: "fail", Detail: "reopen session A: " + err.Error()})
		saveStageTree(roundDir, "drafts-switch-fail")
		return
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		got := findTextField(nodes, "Message")
		if got == nil || !strings.Contains(got.Value, draftTextA) {
			return fmt.Errorf("session A draft not restored yet")
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "switch session restores drafts", Scope: scope, Status: "fail", Detail: err.Error()})
		saveStageTree(roundDir, "drafts-restore-fail")
		return
	}
	*cases = append(*cases, caseResult{Name: "switch session restores drafts", Scope: scope, Status: "pass",
		Detail: "B composer clean, A draft restored, no cross-draft leak"})

	tree, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findSmallButton(nodes, "New task") == nil {
			return fmt.Errorf("New task button not visible yet")
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "new task keeps drafts separate", Scope: scope, Status: "fail", Detail: err.Error()})
		return
	}
	newTask := findSmallButton(tree, "New task")
	if err := client.act(ctx, windowID, newTask.ID, "", "press", ""); err != nil {
		*cases = append(*cases, caseResult{Name: "new task keeps drafts separate", Scope: scope, Status: "fail", Detail: "press New task: " + err.Error()})
		return
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		got := findTextField(nodes, "Message")
		if got == nil {
			return fmt.Errorf("composer not visible after new task")
		}
		if strings.Contains(got.Value, draftTextA) {
			return fmt.Errorf("old draft leaked into the new task")
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "new task keeps drafts separate", Scope: scope, Status: "fail", Detail: err.Error()})
		return
	}
	if err := selectSessionWait(ctx, client, windowID, sessionA, knownTitleA, seededBodyA); err != nil {
		*cases = append(*cases, caseResult{Name: "new task keeps drafts separate", Scope: scope, Status: "fail", Detail: "reopen session A: " + err.Error()})
		return
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		got := findTextField(nodes, "Message")
		if got == nil || !strings.Contains(got.Value, draftTextA) {
			return fmt.Errorf("session A draft lost after new task")
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "new task keeps drafts separate", Scope: scope, Status: "fail", Detail: err.Error()})
		return
	}
	*cases = append(*cases, caseResult{Name: "new task keeps drafts separate", Scope: scope, Status: "pass",
		Detail: "new task composer clean, session A draft intact"})
	saveStageTree(roundDir, "drafts")
}

// checkRightPanelAndFiles walks the real panel: Toggle right panel opens the
// surfaces, Files lists the workspace, hello.txt opens a file tab, and —
// once the file-editor integration lands — editing saves literal text while
// an external same-size write forces a refusal that keeps the external
// bytes. Missing product labels report BLOCKED without touching product
// code; only file bytes on disk decide save and conflict, never button
// presence alone.
func checkRightPanelAndFiles(ctx context.Context, client *autoClient, windowID int, workspace string, cases *[]caseResult, roundDir string) {
	const scope = "realUI"
	tree, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findButton(nodes, "Toggle right panel") == nil {
			return fmt.Errorf("right panel toggle not visible yet")
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "right panel opens", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: " + err.Error()})
		*cases = append(*cases, caseResult{Name: "files surface opens hello.txt", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: right panel unreachable"})
		*cases = append(*cases, caseResult{Name: "file edit saves to disk", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: right panel unreachable"})
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: right panel unreachable"})
		return
	}
	toggle := findButton(tree, "Toggle right panel")
	if err := client.act(ctx, windowID, toggle.ID, "", "press", ""); err != nil {
		*cases = append(*cases, caseResult{Name: "right panel opens", Scope: scope, Status: "fail", Detail: "press toggle: " + err.Error()})
		return
	}
	tree, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if !anyLabel(nodes, "Files") && !anyLabel(nodes, "Open a surface") {
			return fmt.Errorf("right panel surfaces not visible yet (integration pending?)")
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "right panel opens", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: " + err.Error()})
		*cases = append(*cases, caseResult{Name: "files surface opens hello.txt", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: Files surface unreachable"})
		*cases = append(*cases, caseResult{Name: "file edit saves to disk", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: Files surface unreachable"})
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: Files surface unreachable"})
		return
	}
	*cases = append(*cases, caseResult{Name: "right panel opens", Scope: scope, Status: "pass", Detail: "right panel surfaces visible"})
	saveStageTree(roundDir, "rightpanel")

	filesButton := findButton(tree, "Files")
	if filesButton == nil {
		*cases = append(*cases, caseResult{Name: "files surface opens hello.txt", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: Files card not visible yet"})
		*cases = append(*cases, caseResult{Name: "file edit saves to disk", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: Files surface unreachable"})
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: Files surface unreachable"})
		return
	}
	if err := client.act(ctx, windowID, filesButton.ID, "", "press", ""); err != nil {
		*cases = append(*cases, caseResult{Name: "files surface opens hello.txt", Scope: scope, Status: "fail", Detail: "press Files: " + err.Error()})
		return
	}
	tree, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findButton(nodes, knownFile) == nil {
			return fmt.Errorf("workspace row %s not visible yet", knownFile)
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "files surface opens hello.txt", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: " + err.Error()})
		*cases = append(*cases, caseResult{Name: "file edit saves to disk", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: file tab unreachable"})
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: file tab unreachable"})
		return
	}
	row := findButton(tree, knownFile)
	if err := client.act(ctx, windowID, row.ID, "", "press", ""); err != nil {
		*cases = append(*cases, caseResult{Name: "files surface opens hello.txt", Scope: scope, Status: "fail", Detail: "press hello.txt: " + err.Error()})
		return
	}
	tree, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findEditableEditor(nodes, knownText) == nil {
			return fmt.Errorf("editable file editor not present yet (group label alone is not a target)")
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "files surface opens hello.txt", Scope: scope, Status: "pass", Detail: "hello.txt row opened"})
		*cases = append(*cases, caseResult{Name: "file edit saves to disk", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: " + err.Error()})
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: editor unreachable"})
		saveStageTree(roundDir, "filetab")
		return
	}
	*cases = append(*cases, caseResult{Name: "files surface opens hello.txt", Scope: scope, Status: "pass", Detail: "hello.txt file tab opened"})
	saveStageTree(roundDir, "filetab")

	edited := "uiverify edited text 5813\nsecond line fixture\n"
	editPath, editErr := editEditorBuffer(ctx, client, windowID, tree, knownText, edited)
	if editErr != nil {
		*cases = append(*cases, caseResult{Name: "file edit saves to disk", Scope: scope, Status: "fail", Detail: editErr.Error()})
		saveStageTree(roundDir, "editorfail")
		return
	}
	saveStageTree(roundDir, "fileedit")

	var saveButton *autoNode
	if _, err := waitTreeFor(ctx, client, windowID, 10*time.Second, func(nodes []autoNode) error {
		saveButton = findSaveButton(nodes)
		if saveButton == nil {
			return fmt.Errorf("save button not visible yet (edit may not have marked the buffer dirty)")
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "file edit saves to disk", Scope: scope, Status: "fail",
			Detail: err.Error() + " via " + editPath})
		return
	}
	if err := client.act(ctx, windowID, saveButton.ID, "", "press", ""); err != nil {
		*cases = append(*cases, caseResult{Name: "file edit saves to disk", Scope: scope, Status: "fail", Detail: "press save: " + err.Error()})
		return
	}
	diskPath := filepath.Join(workspace, knownFile)
	if err := waitFileLiteral(ctx, diskPath, edited, 15*time.Second); err != nil {
		*cases = append(*cases, caseResult{Name: "file edit saves to disk", Scope: scope, Status: "fail",
			Detail: err.Error() + " via " + editPath})
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "blocked",
			Detail: "BLOCKED on save: conflict step not reached"})
		return
	}
	*cases = append(*cases, caseResult{Name: "file edit saves to disk", Scope: scope, Status: "pass",
		Detail: "workspace file holds the literal edited text via " + editPath})
	saveStageTree(roundDir, "fileedit")

	// A conflict needs a dirty buffer over a foreign disk copy: round two
	// re-edits so the Save button exists, then an external same-size write
	// lands before the press. The app verifies the disk hash at save time.
	editedRoundTwo := "uiverify edited text 5813 round two\nsecond line fixture\n"
	if _, err := editEditorBuffer(ctx, client, windowID, tree, "uiverify edited text 5813", editedRoundTwo); err != nil {
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "fail",
			Detail: "round-two edit failed: " + err.Error()})
		saveStageTree(roundDir, "editorfail2")
		return
	}
	conflict := "X" + edited[1:]
	if len(conflict) != len(edited) {
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "fail", Detail: "test bug: conflict length drifted"})
		return
	}
	if err := os.WriteFile(diskPath, []byte(conflict), 0o644); err != nil {
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "fail", Detail: "external write: " + err.Error()})
		return
	}
	var conflictSave *autoNode
	if _, err := waitTreeFor(ctx, client, windowID, 10*time.Second, func(nodes []autoNode) error {
		conflictSave = findSaveButton(nodes)
		if conflictSave == nil {
			return fmt.Errorf("save button not visible for the conflict press")
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "fail", Detail: err.Error()})
		return
	}
	if err := client.act(ctx, windowID, conflictSave.ID, "", "press", ""); err != nil {
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "fail", Detail: "press save after conflict: " + err.Error()})
		return
	}
	if err := client.idle(ctx, windowID, 5000); err != nil {
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "fail", Detail: "idle after conflict save: " + err.Error()})
		return
	}
	kept, err := os.ReadFile(diskPath)
	if err != nil {
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "fail", Detail: "read after conflict save: " + err.Error()})
		return
	}
	if string(kept) != conflict {
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "fail",
			Detail: "UI overwrote the external same-size edit instead of refusing"})
		return
	}
	if _, err := waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if !anyLabel(nodes, fileConflictText) {
			return fmt.Errorf("conflict dialog %q not visible yet", fileConflictText)
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "fail",
			Detail: "disk kept external text but no conflict dialog: " + err.Error()})
		saveStageTree(roundDir, "fileconflict-fail")
		return
	}
	*cases = append(*cases, caseResult{Name: "external same-size edit blocks save", Scope: scope, Status: "pass",
		Detail: "save refused, external text kept, conflict dialog visible"})
	saveStageTree(roundDir, "fileconflict")

	// crlf.txt: the first opened file replaced the Files tab, so reopen the
	// surface through "Open a surface" before picking the second file.
	if err := openFilesSurface(ctx, client, windowID); err != nil {
		*cases = append(*cases, caseResult{Name: "crlf file opens clean", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: Files surface unreachable again: " + err.Error()})
		*cases = append(*cases, caseResult{Name: "crlf file save keeps line endings", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: Files surface unreachable again"})
		saveStageTree(roundDir, "crlf-blocked")
		return
	}
	tree, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findButton(nodes, crlfFile) == nil {
			return fmt.Errorf("workspace row %s not visible yet", crlfFile)
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "crlf file opens clean", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: " + err.Error()})
		*cases = append(*cases, caseResult{Name: "crlf file save keeps line endings", Scope: scope, Status: "blocked",
			Detail: "BLOCKED integration: file tab unreachable"})
		saveStageTree(roundDir, "crlf-blocked")
		return
	}
	if err := client.act(ctx, windowID, findButton(tree, crlfFile).ID, "", "press", ""); err != nil {
		*cases = append(*cases, caseResult{Name: "crlf file opens clean", Scope: scope, Status: "fail", Detail: "press crlf.txt: " + err.Error()})
		return
	}
	crlfProbe := "crlf line one uiverify 5311"
	tree, err = waitTree(ctx, client, windowID, func(nodes []autoNode) error {
		if findTextContaining(nodes, crlfProbe) == nil {
			return fmt.Errorf("crlf editor content not visible yet")
		}
		return nil
	})
	if err != nil {
		*cases = append(*cases, caseResult{Name: "crlf file opens clean", Scope: scope, Status: "fail", Detail: err.Error()})
		*cases = append(*cases, caseResult{Name: "crlf file save keeps line endings", Scope: scope, Status: "blocked",
			Detail: "BLOCKED: editor never showed the file"})
		saveStageTree(roundDir, "crlf-blocked")
		return
	}
	// Clean open: the buffer holds the normalized text and the header shows
	// no Save control, because the CRLF file must not read as dirty.
	if findSaveButton(tree) != nil {
		*cases = append(*cases, caseResult{Name: "crlf file opens clean", Scope: scope, Status: "fail",
			Detail: "save button visible right after opening crlf.txt: the CRLF file reads as dirty"})
		*cases = append(*cases, caseResult{Name: "crlf file save keeps line endings", Scope: scope, Status: "blocked",
			Detail: "BLOCKED: clean-open assertion failed"})
		saveStageTree(roundDir, "crlf-dirty-open")
		return
	}
	*cases = append(*cases, caseResult{Name: "crlf file opens clean", Scope: scope, Status: "pass",
		Detail: "crlf.txt content visible, no save button, buffer not dirty on open"})

	if _, err := editEditorBuffer(ctx, client, windowID, tree, crlfProbe, crlfEditLF); err != nil {
		*cases = append(*cases, caseResult{Name: "crlf file save keeps line endings", Scope: scope, Status: "fail", Detail: err.Error()})
		saveStageTree(roundDir, "crlf-editorfail")
		return
	}
	var crlfSave *autoNode
	if _, err := waitTreeFor(ctx, client, windowID, 10*time.Second, func(nodes []autoNode) error {
		crlfSave = findSaveButton(nodes)
		if crlfSave == nil {
			return fmt.Errorf("save button not visible after editing crlf.txt")
		}
		return nil
	}); err != nil {
		*cases = append(*cases, caseResult{Name: "crlf file save keeps line endings", Scope: scope, Status: "fail", Detail: err.Error()})
		return
	}
	if err := client.act(ctx, windowID, crlfSave.ID, "", "press", ""); err != nil {
		*cases = append(*cases, caseResult{Name: "crlf file save keeps line endings", Scope: scope, Status: "fail", Detail: "press save: " + err.Error()})
		return
	}
	crlfPath := filepath.Join(workspace, crlfFile)
	if err := waitFileLiteral(ctx, crlfPath, crlfEditCRL, 15*time.Second); err != nil {
		*cases = append(*cases, caseResult{Name: "crlf file save keeps line endings", Scope: scope, Status: "fail", Detail: err.Error()})
		return
	}
	*cases = append(*cases, caseResult{Name: "crlf file save keeps line endings", Scope: scope, Status: "pass",
		Detail: "saved bytes keep CRLF line endings exactly"})
	saveStageTree(roundDir, "crlf")
}

// openFilesSurface brings the right panel back to the Files list through the
// "Open a surface" popup.
func openFilesSurface(ctx context.Context, client *autoClient, windowID int) error {
	tree, err := waitTreeFor(ctx, client, windowID, 15*time.Second, func(nodes []autoNode) error {
		if findSmallButton(nodes, "Open a surface") == nil && findButton(nodes, "Open a surface") == nil {
			return fmt.Errorf("Open a surface button not visible")
		}
		return nil
	})
	if err != nil {
		return err
	}
	plus := findSmallButton(tree, "Open a surface")
	if plus == nil {
		plus = findButton(tree, "Open a surface")
	}
	if err := client.act(ctx, windowID, plus.ID, "", "press", ""); err != nil {
		return fmt.Errorf("press Open a surface: %w", err)
	}
	tree, err = waitTreeFor(ctx, client, windowID, 15*time.Second, func(nodes []autoNode) error {
		if findButton(nodes, "Files") == nil {
			return fmt.Errorf("Files card not visible in the surface popup")
		}
		return nil
	})
	if err != nil {
		return err
	}
	if err := client.act(ctx, windowID, findButton(tree, "Files").ID, "", "press", ""); err != nil {
		return fmt.Errorf("press Files card: %w", err)
	}
	return nil
}

const (
	nativeFileEditorLabel = "native-file-editor"
	nativeFileSaveLabel   = "native-file-save"
	nativeFileErrorLabel  = "native-file-error"

	// fileEditorSurface renders the save control with key "file-save-<path>"
	// and label native-file-save; the conflict dialog reuses
	// native-file-save on "Save anyway" and shows this text.
	fileSaveKeyPrefix     = "file-save-"
	fileConflictText      = "The file changed on disk"
	fileConflictSaveLabel = "Save anyway"
	fileConflictKeepLabel = "Keep editing"
)

// keyString renders an automation key for prefix matching and artifacts.
// Protocol keys are strings, booleans, or integers; anything else prints
// as Go would.
func keyString(key any) string {
	switch value := key.(type) {
	case nil:
		return ""
	case string:
		return value
	case bool:
		if value {
			return "true"
		}
		return "false"
	case float64:
		if value == float64(int64(value)) {
			return strconv.FormatInt(int64(value), 10)
		}
		return strconv.FormatFloat(value, 'f', -1, 64)
	default:
		return fmt.Sprintf("%v", value)
	}
}

// findSaveButton locates the header Save control by its label or by its
// file-save key prefix, preferring a button role. The conflict dialog's
// "Save anyway" reuses the same label, so callers check dialog text first
// when a conflict is expected.
func findSaveButton(nodes []autoNode) *autoNode {
	for i := range nodes {
		if (nodes[i].Label == nativeFileSaveLabel || strings.HasPrefix(keyString(nodes[i].Key), fileSaveKeyPrefix)) &&
			strings.Contains(nodes[i].Role, "button") {
			return &nodes[i]
		}
	}
	for i := range nodes {
		if nodes[i].Label == nativeFileSaveLabel || strings.HasPrefix(keyString(nodes[i].Key), fileSaveKeyPrefix) {
			return &nodes[i]
		}
	}
	return nil
}

// findEditableEditor picks the real input target for a file editor: the
// text-role node holding the file's text, else the labeled text field that
// advertises setValue. A group label alone is never a target: setValue on a
// group is a silent no-op in the automation layer.
func findEditableEditor(nodes []autoNode, contentProbe string) *autoNode {
	if n := findTextContaining(nodes, contentProbe); n != nil && hasAction(n, "setValue") {
		return n
	}
	for i := range nodes {
		n := &nodes[i]
		if n.Label == nativeFileEditorLabel && strings.Contains(n.Role, "text") && hasAction(n, "setValue") {
			return n
		}
	}
	return nil
}

// editEditorBuffer lands edited in the editor through real input paths and
// returns the path the buffer verifiably accepted. setValue runs only on a
// node that advertises it; the keyboard path is real focus, select-all, and
// text input. Every attempt is verified against the tree value, never
// against the act's success alone.
func editEditorBuffer(ctx context.Context, client *autoClient, windowID int, nodes []autoNode, contentProbe, edited string) (string, error) {
	editor := findEditableEditor(nodes, contentProbe)
	if editor == nil {
		return "", fmt.Errorf("no editable editor target in tree (probe %q)", contentProbe)
	}
	marker := edited
	if cut := strings.IndexByte(edited, '\n'); cut >= 0 {
		marker = edited[:cut]
	}
	verified := func() error {
		_, err := waitTreeFor(ctx, client, windowID, 10*time.Second, func(current []autoNode) error {
			if findTextContaining(current, marker) == nil {
				return fmt.Errorf("edited text %q not in editor buffer yet", marker)
			}
			return nil
		})
		return err
	}
	var setValueErr, keyboardErr error
	if hasAction(editor, "setValue") {
		if err := client.act(ctx, windowID, editor.ID, "", "setValue", edited); err == nil {
			if verifyErr := verified(); verifyErr == nil {
				return "setValue", nil
			} else {
				setValueErr = verifyErr
			}
		} else {
			setValueErr = err
		}
	}
	if err := client.act(ctx, windowID, editor.ID, "", "focus", ""); err != nil {
		keyboardErr = fmt.Errorf("focus: %w", err)
	} else if err := client.idle(ctx, windowID, 5000); err != nil {
		keyboardErr = fmt.Errorf("idle after focus: %w", err)
	} else if err := client.inputKey(ctx, windowID, "Ctrl+A"); err != nil {
		keyboardErr = fmt.Errorf("Ctrl+A: %w", err)
	} else if err := client.inputText(ctx, windowID, edited); err != nil {
		keyboardErr = fmt.Errorf("inputText: %w", err)
	} else if verifyErr := verified(); verifyErr != nil {
		keyboardErr = verifyErr
	} else {
		return "focus+Ctrl+A+inputText", nil
	}
	detail := "buffer never took the edit"
	if setValueErr != nil {
		detail += fmt.Sprintf("; setValue path: %v", setValueErr)
	}
	if keyboardErr != nil {
		detail += fmt.Sprintf("; keyboard path: %v", keyboardErr)
	}
	return "", fmt.Errorf("%s", detail)
}

// waitFileLiteral polls the workspace file until its bytes equal the want
// text. Disk bytes decide save and conflict, never widget state alone.
func waitFileLiteral(ctx context.Context, path, want string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for {
		raw, err := os.ReadFile(path)
		if err == nil && string(raw) == want {
			return nil
		}
		if time.Now().After(deadline) {
			detail := "file never held the wanted text"
			if err != nil {
				detail = "read: " + err.Error()
			} else if len(raw) > 120 {
				detail = fmt.Sprintf("got %d bytes starting %q", len(raw), string(raw[:120]))
			} else {
				detail = fmt.Sprintf("got %q", string(raw))
			}
			return fmt.Errorf("%s within %s", detail, timeout)
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(200 * time.Millisecond):
		}
	}
}

// autoClient speaks the MyGo automation protocol: newline-delimited JSON
// over a Unix socket. Every call carries its own deadline.
type autoClient struct {
	conn   net.Conn
	reader *bufio.Reader
	nextID int
}

func dialAuto(sockPath string) (*autoClient, error) {
	conn, err := net.DialTimeout("unix", sockPath, perRequestTimeout)
	if err != nil {
		return nil, err
	}
	return &autoClient{conn: conn, reader: bufio.NewReader(conn)}, nil
}

func (c *autoClient) close() {
	if c != nil && c.conn != nil {
		_ = c.conn.Close()
	}
}

func (c *autoClient) call(ctx context.Context, method string, params map[string]any) (json.RawMessage, error) {
	deadline, ok := ctx.Deadline()
	if !ok {
		deadline = time.Now().Add(perRequestTimeout)
	}
	if err := c.conn.SetDeadline(deadline); err != nil {
		return nil, err
	}
	c.nextID++
	line, err := json.Marshal(map[string]any{"id": c.nextID, "method": method, "params": params})
	if err != nil {
		return nil, err
	}
	if _, err := c.conn.Write(append(line, '\n')); err != nil {
		return nil, err
	}
	answer, err := c.reader.ReadBytes('\n')
	if err != nil {
		return nil, err
	}
	var envelope struct {
		Result json.RawMessage `json:"result"`
		Error  *struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(answer, &envelope); err != nil {
		return nil, fmt.Errorf("decode automation answer: %w", err)
	}
	if envelope.Error != nil {
		return nil, fmt.Errorf("automation %s: %s (%s)", method, envelope.Error.Message, envelope.Error.Code)
	}
	return envelope.Result, nil
}

type autoWindow struct {
	ID    int    `json:"id"`
	Title string `json:"title"`
}

func (c *autoClient) windows(ctx context.Context) ([]autoWindow, error) {
	raw, err := c.call(ctx, "windows", nil)
	if err != nil {
		return nil, err
	}
	var out struct {
		Windows []autoWindow `json:"windows"`
	}
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, err
	}
	return out.Windows, nil
}

type autoNode struct {
	ID      string   `json:"id"`
	Role    string   `json:"role"`
	Label   string   `json:"label"`
	Value   string   `json:"value"`
	Key     any      `json:"key"`
	States  []string `json:"states"`
	Actions []string `json:"actions"`
	Bounds  *struct {
		X      float64 `json:"x"`
		Y      float64 `json:"y"`
		Width  float64 `json:"width"`
		Height float64 `json:"height"`
	} `json:"bounds"`
}

func (c *autoClient) tree(ctx context.Context, windowID int) ([]autoNode, error) {
	raw, err := c.call(ctx, "tree", map[string]any{"window": windowID})
	if err != nil {
		return nil, err
	}
	var out struct {
		Nodes []autoNode `json:"nodes"`
	}
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, err
	}
	return out.Nodes, nil
}

func (c *autoClient) act(ctx context.Context, windowID int, id, key, action, text string) error {
	params := map[string]any{"window": windowID, "action": action}
	if id != "" {
		params["id"] = id
	} else {
		params["key"] = key
	}
	if text != "" {
		params["text"] = text
	}
	_, err := c.call(ctx, "act", params)
	return err
}

func (c *autoClient) actKey(ctx context.Context, windowID int, key, action string) error {
	return c.act(ctx, windowID, "", key, action, "")
}

func (c *autoClient) inputText(ctx context.Context, windowID int, text string) error {
	_, err := c.call(ctx, "input", map[string]any{
		"window": windowID,
		"events": []any{map[string]any{"type": "text", "text": text}},
	})
	return err
}

func (c *autoClient) inputKey(ctx context.Context, windowID int, key string) error {
	_, err := c.call(ctx, "input", map[string]any{
		"window": windowID,
		"events": []any{map[string]any{"type": "key", "key": key}},
	})
	return err
}

func (c *autoClient) idle(ctx context.Context, windowID int, timeoutMs int) error {
	_, err := c.call(ctx, "idle", map[string]any{"window": windowID, "timeout": timeoutMs})
	return err
}

// lastTreeJSON holds the latest desensitized tree for the artifact file.
var lastTreeJSON string

func waitTree(ctx context.Context, client *autoClient, windowID int, predicate func([]autoNode) error) ([]autoNode, error) {
	return waitTreeFor(ctx, client, windowID, treePollTimeout, predicate)
}

// waitTreeFor polls the automation tree until predicate passes or its own
// budget ends, so one missed predicate cannot consume the whole round.
func waitTreeFor(ctx context.Context, client *autoClient, windowID int, timeout time.Duration, predicate func([]autoNode) error) ([]autoNode, error) {
	deadline := time.Now().Add(timeout)
	var last []autoNode
	var lastErr error
	for {
		nodes, err := client.tree(ctx, windowID)
		if err == nil {
			last = nodes
			lastTreeJSON = desensitizedTree(windowID, nodes)
			if perr := predicate(nodes); perr == nil {
				return nodes, nil
			} else {
				lastErr = perr
			}
		} else {
			lastErr = err
		}
		if time.Now().After(deadline) {
			return last, fmt.Errorf("tree predicate timeout: %v", lastErr)
		}
		select {
		case <-ctx.Done():
			return last, ctx.Err()
		case <-time.After(400 * time.Millisecond):
		}
	}
}

// saveStageTree writes the latest desensitized tree to a per-stage artifact
// so the run leaves real UI tree evidence even when a later step fails.
func saveStageTree(roundDir, stage string) {
	if lastTreeJSON == "" {
		return
	}
	_ = os.WriteFile(filepath.Join(roundDir, "automation-tree-"+stage+".json"), []byte(lastTreeJSON+"\n"), 0o644)
}

// desensitizedTree keeps roles, labels, keys, states and bounds for review
// while truncating free-form values that may carry draft content.
func desensitizedTree(windowID int, nodes []autoNode) string {
	type slim struct {
		ID      string   `json:"id"`
		Role    string   `json:"role,omitempty"`
		Label   string   `json:"label,omitempty"`
		Value   string   `json:"value,omitempty"`
		Key     string   `json:"key,omitempty"`
		States  []string `json:"states,omitempty"`
		Actions []string `json:"actions,omitempty"`
		Bounds  *struct {
			X      float64 `json:"x"`
			Y      float64 `json:"y"`
			Width  float64 `json:"width"`
			Height float64 `json:"height"`
		} `json:"bounds,omitempty"`
	}
	slims := make([]slim, 0, len(nodes))
	for _, n := range nodes {
		value := n.Value
		if len([]rune(value)) > 80 {
			value = string([]rune(value)[:80]) + "…[truncated]"
		}
		slims = append(slims, slim{
			ID: n.ID, Role: n.Role, Label: n.Label, Value: value,
			Key: keyString(n.Key), States: n.States, Actions: n.Actions, Bounds: n.Bounds,
		})
	}
	raw, _ := json.MarshalIndent(map[string]any{"window": windowID, "nodes": slims}, "", "  ")
	return string(raw)
}

// visibleLabels compresses a tree snapshot to its labels for failure detail.
func visibleLabels(nodes []autoNode) string {
	seen := map[string]bool{}
	var out []string
	for _, n := range nodes {
		if n.Label == "" || seen[n.Label] {
			continue
		}
		seen[n.Label] = true
		out = append(out, n.Label)
		if len(out) >= 40 {
			out = append(out, "...")
			break
		}
	}
	raw, _ := json.Marshal(out)
	return string(raw)
}

func anyLabel(nodes []autoNode, label string) bool {
	for i := range nodes {
		if nodes[i].Label == label {
			return true
		}
	}
	return false
}

func findTextField(nodes []autoNode, label string) *autoNode {
	for i := range nodes {
		if nodes[i].Label == label && strings.Contains(nodes[i].Role, "text") {
			return &nodes[i]
		}
	}
	for i := range nodes {
		if nodes[i].Label == label {
			return &nodes[i]
		}
	}
	return nil
}

func findButton(nodes []autoNode, label string) *autoNode {
	for i := range nodes {
		if nodes[i].Label == label && strings.Contains(nodes[i].Role, "button") {
			return &nodes[i]
		}
	}
	return nil
}

// findSmallButton picks the compact "+" style button when several nodes share
// a label (for example a 28x28 New task button versus full-size rows).
func findSmallButton(nodes []autoNode, label string) *autoNode {
	var best *autoNode
	for i := range nodes {
		n := &nodes[i]
		if n.Label != label || !strings.Contains(n.Role, "button") {
			continue
		}
		if best == nil {
			best = n
			continue
		}
		if area(n) < area(best) {
			best = n
		}
	}
	return best
}

func area(n *autoNode) float64 {
	if n == nil || n.Bounds == nil {
		return 1e18
	}
	return n.Bounds.Width * n.Bounds.Height
}

func requestDaemon(ctx context.Context, client *daemon.Client, command map[string]any) error {
	callCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	defer cancel()
	_, err := client.Request(callCtx, command)
	return err
}

func requestDaemonInto(ctx context.Context, client *daemon.Client, command map[string]any, out any) error {
	callCtx, cancel := context.WithTimeout(ctx, perRequestTimeout)
	defer cancel()
	payload, err := client.Request(callCtx, command)
	if err != nil {
		return err
	}
	return json.Unmarshal(payload, out)
}

type readyLine struct {
	Address         string `json:"address"`
	ProtocolVersion int    `json:"protocolVersion"`
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
	return &readyLine{Address: raw.Address, ProtocolVersion: version}, true
}

func randomToken() (string, error) {
	buffer := make([]byte, 16)
	if _, err := rand.Read(buffer); err != nil {
		return "", err
	}
	return hex.EncodeToString(buffer), nil
}

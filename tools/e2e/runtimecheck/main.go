package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

func main() {
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		fixture()
		return
	}
	binary := flag.String("daemon", "", "absolute daemon binary")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()
	if err := run(ctx, *binary); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, binary string) (err error) {
	if !filepath.IsAbs(binary) {
		return errors.New("absolute daemon required")
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	defer func() {
		raw, _ := json.MarshalIndent(map[string]any{"checks": checks, "passed": err == nil, "realModelTested": false}, "", "  ")
		_ = os.WriteFile(filepath.Join(round, "runtime-result.json"), raw, 0o600)
		fmt.Println("[REPORT]", filepath.Join(round, "runtime-result.json"))
	}()
	self, err := os.Executable()
	if err != nil {
		return err
	}
	token := uuid.NewString()
	logs := filepath.Join(round, "daemon-logs")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return err
	}
	workspace := filepath.Join(round, "workspace", "main")
	if err := os.MkdirAll(workspace, 0o700); err != nil {
		return err
	}
	dataDir := filepath.Join(round, "data")
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		return err
	}
	cachedCatalog, _ := json.Marshal(map[string]any{
		"fetched_at": time.Now().Unix(),
		"catalog":    map[string]any{"fixture": map[string]any{"models": map[string]any{"literal": map[string]any{"limit": map[string]any{"context": 8192}}}}},
	})
	if err := os.WriteFile(filepath.Join(dataDir, "models-dev-catalog.json"), cachedCatalog, 0o600); err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", dataDir, "--bind", "127.0.0.1:0"}, logs, round, ownedprocess.BuildEnv(round, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if err != nil {
		return err
	}
	defer child.Close()
	var address string
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for address == "" {
		raw, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
			PID     uint32 `json:"pid"`
		}
		if json.Unmarshal(raw, &ready) == nil && ready.PID == child.PID {
			address = ready.Address
		}
		if address != "" {
			break
		}
		if done, _ := child.Exited(0); done {
			return errors.New("daemon exited")
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-tick.C:
		}
	}
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()
	cacheReply, err := client.Request(ctx, map[string]any{"type": "fetchModelsDevCatalog"})
	if err != nil {
		return fmt.Errorf("read offline model catalog: %w", err)
	}
	var cached struct {
		Catalog map[string]struct {
			Models map[string]struct {
				Limit struct {
					Context int `json:"context"`
				} `json:"limit"`
			} `json:"models"`
		} `json:"catalog"`
	}
	if json.Unmarshal(cacheReply, &cached) != nil || cached.Catalog["fixture"].Models["literal"].Limit.Context != 8192 {
		return fmt.Errorf("offline model catalog mismatch: %s", cacheReply)
	}
	checks["offline-model-catalog"] = true
	probe, err := client.Request(ctx, map[string]any{"type": "probeProvider", "binaryOverride": self, "directory": workspace, "discoverModels": true})
	if err != nil {
		return fmt.Errorf("probe model variants: %w", err)
	}
	var catalog struct {
		Probe struct {
			Models []struct {
				ID              string `json:"id"`
				ReasoningEffort []struct {
					ID string `json:"id"`
				} `json:"reasoning_efforts"`
				Default *string `json:"default_reasoning_effort"`
			} `json:"models"`
		} `json:"probe"`
	}
	if json.Unmarshal(probe, &catalog) != nil || len(catalog.Probe.Models) != 1 || catalog.Probe.Models[0].ID != "fixture/literal" || len(catalog.Probe.Models[0].ReasoningEffort) != 2 || catalog.Probe.Models[0].ReasoningEffort[0].ID != "low" || catalog.Probe.Models[0].ReasoningEffort[1].ID != "high" || catalog.Probe.Models[0].Default == nil || *catalog.Probe.Models[0].Default != "high" {
		return fmt.Errorf("model variants not exposed: %s", probe)
	}
	checks["provider-model-variants"] = true
	integrationCommand := map[string]any{"type": "fetchIntegrations", "binary": self, "directory": workspace}
	readIntegrations := func() (map[string]bool, error) {
		raw, err := client.Request(ctx, integrationCommand)
		if err != nil {
			return nil, err
		}
		var response struct {
			Integrations []struct {
				ID        string `json:"id"`
				Connected bool   `json:"connected"`
			} `json:"integrations"`
		}
		if err := json.Unmarshal(raw, &response); err != nil {
			return nil, err
		}
		result := map[string]bool{}
		for _, item := range response.Integrations {
			result[item.ID] = item.Connected
		}
		return result, nil
	}
	beforeLogout, err := readIntegrations()
	if err != nil || !beforeLogout["fixture"] || !beforeLogout["other"] {
		return fmt.Errorf("fixture integrations unavailable: %v (%v)", beforeLogout, err)
	}
	if _, err := client.Request(ctx, map[string]any{"type": "logoutProvider", "binary": self, "directory": workspace, "providerId": "fixture"}); err != nil {
		return fmt.Errorf("log out provider: %w", err)
	}
	afterLogout, err := readIntegrations()
	if err != nil || afterLogout["fixture"] || !afterLogout["other"] {
		return fmt.Errorf("logout did not remove only the selected provider: %v (%v)", afterLogout, err)
	}
	checks["builtin-provider-logout"] = true
	sid, rid, pid := uuid.NewString(), uuid.NewString(), uuid.NewString()
	options := map[string]any{"binary": self, "cwd": workspace, "mode": "ask", "interactionMode": "code", "model": "fixture/literal"}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": options}); err != nil {
		return err
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "applyOptions", "options": map[string]any{"model": "fixture/literal", "reasoningEffort": "low", "interactionMode": "build"}}); err != nil {
		return fmt.Errorf("apply model variant: %w", err)
	}
	var selected struct {
		Model struct {
			Provider string `json:"providerID"`
			ID       string `json:"id"`
			Variant  string `json:"variant"`
		} `json:"model"`
	}
	selection, err := os.ReadFile(filepath.Join(round, "home", "selected-model.json"))
	if err != nil || json.Unmarshal(selection, &selected) != nil || selected.Model.Provider != "fixture" || selected.Model.ID != "literal" || selected.Model.Variant != "low" {
		return fmt.Errorf("model variant did not reach OpenCode: %s (%v)", selection, err)
	}
	checks["provider-model-variant-selected"] = true
	checks["owned-engine-start"] = true
	outsider := uuid.NewString()
	if _, err := client.RequestFor(ctx, outsider, uuid.NewString(), map[string]any{"type": "start", "options": map[string]any{"binary": self, "cwd": round, "mode": "ask", "interactionMode": "code", "model": "fixture/literal"}}); err == nil {
		return errors.New("engine start outside the isolated workspace was accepted")
	} else if !strings.Contains(err.Error(), "workspace") {
		return fmt.Errorf("engine cwd refusal named the wrong boundary: %v", err)
	}
	checks["engine-cwd-outside-workspace-refused"] = true
	now := uint64(time.Now().Unix())
	project := map[string]any{"id": pid, "name": "fixture", "path": workspace, "created_at": now}
	newTurn := func(tid, prompt string, count int) (map[string]any, model.SubmissionRecord) {
		record := model.NewSubmissionRecord(sid, tid, prompt, nil)
		return map[string]any{"id": tid, "status": "running", "turn_count": count, "started_at": now, "submission": record.Receipt}, record
	}
	newUser := func(tid, content string) map[string]any {
		return map[string]any{"id": uuid.NewString(), "turn_id": tid, "role": "user", "content": content, "created_at": now, "attachments": []any{}, "streaming": false}
	}
	admit := func(session map[string]any, record model.SubmissionRecord) error {
		accept := map[string]any{"type": "acceptSubmission", "project": project, "session": session, "submission": record}
		_, err := client.RequestFor(ctx, sid, "", accept)
		return err
	}
	tid := uuid.NewString()
	turn, record := newTurn(tid, "literal request", 1)
	session := map[string]any{"id": sid, "project_id": pid, "title": "New task", "provider": "openCode", "status": "working", "created_at": now, "updated_at": now, "detail_loaded": true,
		"messages": []any{newUser(tid, "literal request")},
		"turns":    []any{turn}, "transcript_blocks": []any{}}
	if err := admit(session, record); err != nil {
		return err
	}
	if err := admit(session, record); err != nil {
		return err
	}
	checks["durable-admission-idempotent"] = true
	for range 2 {
		if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": tid, "workspace": map[string]string{"kind": "local"}}); err != nil {
			return err
		}
	}
	checks["dispatch-cas-no-repost"] = true
	text, success, err := waitTurn(ctx, client, "literal conversation")
	if err != nil {
		return err
	}
	if text != "literal answer" || !success {
		return errors.New("stream literal mismatch")
	}
	checks["stream-order-and-finish"] = true
	titleRaw, err := client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sid})
	if err != nil {
		return err
	}
	var titleSaved struct {
		Session model.Session `json:"session"`
	}
	if json.Unmarshal(titleRaw, &titleSaved) != nil || titleSaved.Session.DisplayTitle() != "literal conversation" {
		return fmt.Errorf("provider title was not persisted: %s", titleRaw)
	}
	checks["native-title-event-and-persistence"] = true
	state, err := hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	if len(state.Messages) != 2 || state.Messages[1].Content != "literal answer" || state.Status != "idle" {
		return errors.New("daemon did not persist settled transcript")
	}
	if len(state.Blocks) != 1 || state.Blocks[0] != "literal reasoning" {
		return errors.New("reasoning channel was not persisted")
	}
	checks["daemon-owned-transcript-durable"] = true
	checks["reasoning-channel-durable"] = true
	cursorRaw, err := client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sid})
	if err != nil {
		return err
	}
	var savedCursor struct {
		Session struct {
			Cursor *model.RuntimeEventCursor `json:"runtime_event_cursor"`
		} `json:"session"`
	}
	if err := json.Unmarshal(cursorRaw, &savedCursor); err != nil {
		return err
	}
	wireMatched := false
	for _, cursor := range client.Cursors() {
		if cursor.SessionID == sid && cursor.RuntimeID == rid && savedCursor.Session.Cursor != nil {
			stored := savedCursor.Session.Cursor
			wireMatched = stored.RuntimeID == rid && stored.Epoch == cursor.Epoch && uuid.Validate(stored.Epoch) == nil && stored.Sequence+1 == cursor.Sequence
		}
	}
	if !wireMatched {
		return errors.New("stored event cursor is not the actual wire high-water mark")
	}
	checks["persisted-cursor-matches-wire"] = true
	if _, err := client.RequestFor(ctx, sid, uuid.NewString(), map[string]any{"type": "start", "options": options}); err == nil {
		return errors.New("start with a second runtime identity was admitted")
	} else if !strings.Contains(err.Error(), "already has a runtime") {
		return fmt.Errorf("runtime conflict refusal named the wrong boundary: %v", err)
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": options}); err != nil {
		return fmt.Errorf("repeated start with the same runtime identity failed: %w", err)
	}
	checks["failed-start-keeps-old-binding"] = true
	tid2 := uuid.NewString()
	turn2, record2 := newTurn(tid2, "second request", 2)
	session2 := map[string]any{"id": sid, "project_id": pid, "title": "fixture", "provider": "openCode", "status": "working", "created_at": now, "updated_at": now, "detail_loaded": true,
		"messages": append(append([]any{}, state.RawMessages...), newUser(tid2, "second request")),
		"turns":    append(append([]any{}, state.RawTurns...), turn2), "transcript_blocks": state.RawBlocks}
	if err := admit(session2, record2); err != nil {
		return fmt.Errorf("admit second round: %w", err)
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": tid2, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return fmt.Errorf("dispatch second round: %w", err)
	}
	text, success, err = waitTurn(ctx, client)
	if err != nil {
		return err
	}
	if text != "literal answer" || !success {
		return errors.New("second round stream mismatch")
	}
	state, err = hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	if len(state.Messages) != 4 || state.Messages[2].Role != "user" || state.Messages[2].Content != "second request" || state.Messages[3].Content != "literal answer" {
		return errors.New("multi-round transcript did not accumulate in order")
	}
	if len(state.Blocks) != 2 {
		return errors.New("second round reasoning was not appended")
	}
	checks["multi-round-transcript-durable"] = true
	titleRaw, err = client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sid})
	if err != nil {
		return err
	}
	if json.Unmarshal(titleRaw, &titleSaved) != nil || titleSaved.Session.DisplayTitle() != "fixture" || titleSaved.Session.AutoTitle == nil || *titleSaved.Session.AutoTitle != "literal conversation" {
		return errors.New("provider title did not preserve the user's custom title")
	}
	checks["native-title-preserves-user-title"] = true
	tid3 := uuid.NewString()
	turn3, record3 := newTurn(tid3, "hold for cancel", 3)
	session3 := map[string]any{"id": sid, "project_id": pid, "title": "fixture", "provider": "openCode", "status": "working", "created_at": now, "updated_at": now, "detail_loaded": true,
		"messages": append(append([]any{}, state.RawMessages...), newUser(tid3, "hold for cancel")),
		"turns":    append(append([]any{}, state.RawTurns...), turn3), "transcript_blocks": state.RawBlocks}
	if err := admit(session3, record3); err != nil {
		return fmt.Errorf("admit cancel round: %w", err)
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": tid3, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return fmt.Errorf("dispatch cancel round: %w", err)
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "cancel"}); err != nil {
		return fmt.Errorf("cancel: %w", err)
	}
	_, success, err = waitTurn(ctx, client)
	if err != nil {
		return err
	}
	if success {
		return errors.New("cancelled turn reported success")
	}
	state, err = hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	if state.Status != "idle" || state.TurnStatus(tid3) != "interrupted" {
		return errors.New("cancel did not settle the turn as interrupted")
	}
	checks["cancel-interrupts-turn"] = true
	tid4 := uuid.NewString()
	turn4, record4 := newTurn(tid4, "hold-after-text request", 4)
	stale := map[string]any{"id": sid, "project_id": pid, "title": "fixture", "provider": "openCode", "status": "working", "created_at": now, "updated_at": now, "detail_loaded": true,
		"messages": append(append([]any{}, state.RawMessages...), newUser(tid4, "hold-after-text request")),
		"turns":    append(append([]any{}, state.RawTurns...), turn4), "transcript_blocks": state.RawBlocks}
	if err := admit(stale, record4); err != nil {
		return fmt.Errorf("admit stale-probe round: %w", err)
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": tid4, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return fmt.Errorf("dispatch stale-probe round: %w", err)
	}
	if err := waitPartial(ctx, client, sid, len(state.Messages)+2, "literal partial"); err != nil {
		return err
	}
	if _, err := client.Request(ctx, map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{project}, "sessions": []any{stale}, "skeletonSessionIds": []any{}}); err != nil {
		return fmt.Errorf("save racing checkpoint: %w", err)
	}
	after, err := hydrateState(ctx, client, sid)
	if err != nil {
		return err
	}
	if len(after.Messages) != len(state.Messages)+2 || after.Messages[len(after.Messages)-1].Content != "literal partial" {
		return errors.New("racing checkpoint regressed the running transcript")
	}
	checks["racing-checkpoint-no-regress"] = true
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "cancel"}); err != nil {
		return fmt.Errorf("cancel stale-probe round: %w", err)
	}
	if _, success, err := waitTurn(ctx, client); err != nil || success {
		return errors.New("stale-probe turn did not interrupt")
	}
	extra := []string{}
	for _, extraID := range []string{uuid.NewString(), uuid.NewString(), uuid.NewString()} {
		if _, err := client.RequestFor(ctx, extraID, uuid.NewString(), map[string]any{"type": "start", "options": options}); err != nil {
			return fmt.Errorf("fill runtime slot: %w", err)
		}
		extra = append(extra, extraID)
	}
	if _, err := client.RequestFor(ctx, uuid.NewString(), uuid.NewString(), map[string]any{"type": "start", "options": options}); err == nil {
		return errors.New("fifth runtime was admitted past the isolated limit")
	} else if !strings.Contains(err.Error(), "limit") {
		return fmt.Errorf("runtime limit refusal named the wrong boundary: %v", err)
	}
	if _, err := client.RequestFor(ctx, extra[0], "", map[string]any{"type": "closeSession"}); err != nil {
		return fmt.Errorf("close session: %w", err)
	}
	if alive, err := attachedRuntime(ctx, client, extra[0]); err != nil || alive {
		return errors.New("closed session still reports a live runtime")
	}
	revived := uuid.NewString()
	if _, err := client.RequestFor(ctx, extra[0], revived, map[string]any{"type": "start", "options": options}); err != nil {
		return fmt.Errorf("start after close was refused: %w", err)
	}
	if alive, err := attachedRuntime(ctx, client, extra[0]); err != nil || !alive {
		return errors.New("reopened session did not attach its runtime")
	}
	checks["close-reopen-runtime"] = true
	checks["runtime-limit-reaps-closed"] = true
	replay, err := daemon.DialWithResume(ctx, address, token, client.Cursors())
	if err != nil {
		return err
	}
	defer replay.Close()
	if _, err := replay.RequestFor(ctx, sid, rid, map[string]string{"type": "attachSession"}); err != nil {
		return err
	}
	checks["same-runtime-reattach"] = true
	if err := checkScheduledDelivery(ctx, client, sid); err != nil {
		return err
	}
	checks["scheduled-manual-delivery-admission-order"] = true
	if err := checkRestartRecovery(ctx, client, child, binary, self, round, workspace, token, project, sid, pid, rid); err != nil {
		return err
	}
	checks["restart-interrupted-turn-can-continue"] = true
	return nil
}

type hydratedState struct {
	Messages    []messageView
	Blocks      []string
	Status      string
	RawMessages []any
	RawTurns    []any
	RawBlocks   []any
	Turns       []turnView
}

type messageView struct {
	Role      string `json:"role"`
	Content   string `json:"content"`
	Streaming bool   `json:"streaming"`
}

type turnView struct {
	ID     string `json:"id"`
	Status string `json:"status"`
}

func (s hydratedState) TurnStatus(id string) string {
	for _, turn := range s.Turns {
		if turn.ID == id {
			return turn.Status
		}
	}
	return ""
}

func hydrateState(ctx context.Context, client *daemon.Client, sid string) (hydratedState, error) {
	var out hydratedState
	raw, err := client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sid})
	if err != nil {
		return out, err
	}
	var payload struct {
		Session struct {
			Messages []any `json:"messages"`
			Turns    []any `json:"turns"`
			Blocks   []struct {
				Content struct {
					Data struct {
						Content string `json:"content"`
					} `json:"data"`
				} `json:"content"`
			} `json:"transcript_blocks"`
			Status string `json:"status"`
		} `json:"session"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return out, err
	}
	var envelope struct {
		Session map[string]json.RawMessage `json:"session"`
	}
	if err := json.Unmarshal(raw, &envelope); err != nil {
		return out, err
	}
	for _, item := range payload.Session.Messages {
		encoded, _ := json.Marshal(item)
		var view messageView
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
	for _, block := range payload.Session.Blocks {
		out.Blocks = append(out.Blocks, block.Content.Data.Content)
	}
	out.Status = payload.Session.Status
	out.RawMessages = payload.Session.Messages
	out.RawTurns = payload.Session.Turns
	if blocks, ok := envelope.Session["transcript_blocks"]; ok {
		_ = json.Unmarshal(blocks, &out.RawBlocks)
	}
	return out, nil
}

func waitTurn(ctx context.Context, client *daemon.Client, wantTitle ...string) (string, bool, error) {
	text := ""
	title := ""
	for {
		select {
		case <-ctx.Done():
			return "", false, ctx.Err()
		case event, ok := <-client.Events():
			if !ok {
				return "", false, errors.New("client stream closed")
			}
			if event.Kind == "autoTitleUpdated" {
				if err := json.Unmarshal(event.Payload, &title); err != nil {
					return "", false, err
				}
			}
			if event.Kind == "textDelta" {
				var delta struct {
					Delta string `json:"delta"`
				}
				if err := json.Unmarshal(event.Payload, &delta); err != nil {
					return "", false, err
				}
				text += delta.Delta
			}
			if event.Kind == "turnFinished" {
				if len(wantTitle) > 0 && title != wantTitle[0] {
					return "", false, fmt.Errorf("native title event mismatch: got %q, want %q", title, wantTitle[0])
				}
				var done struct {
					Success bool `json:"success"`
				}
				if err := json.Unmarshal(event.Payload, &done); err != nil {
					return "", false, err
				}
				if event.Type == "event" {
					client.Ack(event)
				}
				return text, done.Success, nil
			}
			if event.Type == "event" {
				client.Ack(event)
			}
		}
	}
}

func waitPartial(ctx context.Context, client *daemon.Client, sid string, count int, content string) error {
	tick := time.NewTicker(100 * time.Millisecond)
	defer tick.Stop()
	for {
		state, err := hydrateState(ctx, client, sid)
		if err != nil {
			return err
		}
		if len(state.Messages) == count && state.Messages[count-1].Content == content {
			return nil
		}
		select {
		case <-ctx.Done():
			return errors.New("partial transcript was never persisted")
		case <-tick.C:
		}
	}
}

func attachedRuntime(ctx context.Context, client *daemon.Client, sid string) (bool, error) {
	raw, err := client.RequestFor(ctx, sid, "", map[string]any{"type": "attachSession"})
	if err != nil {
		return false, err
	}
	var reply struct {
		RuntimeID *string `json:"runtimeId"`
	}
	if err := json.Unmarshal(raw, &reply); err != nil {
		return false, err
	}
	return reply.RuntimeID != nil, nil
}

func fixture() {
	flags := flag.NewFlagSet("serve", flag.ExitOnError)
	hostname := flags.String("hostname", "127.0.0.1", "")
	port := flags.String("port", "0", "")
	_ = flags.Parse(os.Args[2:])
	listener, err := net.Listen("tcp", net.JoinHostPort(*hostname, *port))
	if err != nil {
		os.Exit(2)
	}
	var mu sync.Mutex
	subscribers := map[chan string]bool{}
	credentials := map[string]bool{"account-one": true, "account-two": true, "other-account": true}
	var busy, hold bool
	emit := func(events []map[string]any) {
		for _, event := range events {
			raw, _ := json.Marshal(event)
			mu.Lock()
			for stream := range subscribers {
				stream <- string(raw)
			}
			mu.Unlock()
		}
	}
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		username, password, ok := r.BasicAuth()
		if !ok || username != "opencode" || password != os.Getenv("OPENCODE_SERVER_PASSWORD") {
			http.Error(w, "unauthorized", 401)
			return
		}
		if r.URL.Path == "/api/event" {
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(200)
			w.(http.Flusher).Flush()
			stream := make(chan string, 16)
			mu.Lock()
			subscribers[stream] = true
			mu.Unlock()
			defer func() { mu.Lock(); delete(subscribers, stream); mu.Unlock() }()
			for {
				select {
				case <-r.Context().Done():
					return
				case data := <-stream:
					fmt.Fprintf(w, "data: %s\n\n", data)
					w.(http.Flusher).Flush()
				}
			}
		}
		if r.URL.Path == "/api/session" {
			_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]string{"id": "ses_fixture"}})
			return
		}
		if r.URL.Path == "/api/model" {
			_, _ = w.Write([]byte(`{"data":[{"id":"literal","providerID":"fixture","name":"Literal","settings":{"reasoningEffort":"high"},"variants":[{"id":"low","settings":{"reasoningEffort":"low"}},{"id":"high","settings":{"reasoningEffort":"high"}}]}]}`))
			return
		}
		if r.Method == http.MethodGet && r.URL.Path == "/api/integration" {
			mu.Lock()
			connections := []map[string]string{}
			for _, id := range []string{"account-one", "account-two"} {
				if credentials[id] {
					connections = append(connections, map[string]string{"type": "credential", "id": id, "method": "key"})
				}
			}
			other := []map[string]string{}
			if credentials["other-account"] {
				other = append(other, map[string]string{"type": "credential", "id": "other-account", "method": "key"})
			}
			mu.Unlock()
			_ = json.NewEncoder(w).Encode(map[string]any{"data": []any{map[string]any{"id": "fixture", "connections": connections}, map[string]any{"id": "other", "connections": other}}})
			return
		}
		if r.Method == http.MethodDelete && strings.HasPrefix(r.URL.Path, "/api/credential/") {
			id := strings.TrimPrefix(r.URL.Path, "/api/credential/")
			mu.Lock()
			delete(credentials, id)
			mu.Unlock()
			_, _ = w.Write([]byte(`{"data":{}}`))
			return
		}
		if r.Method == http.MethodPost && r.URL.Path == "/api/session/ses_fixture/model" {
			var selection json.RawMessage
			if err := json.NewDecoder(r.Body).Decode(&selection); err != nil {
				http.Error(w, "invalid selection", http.StatusBadRequest)
				return
			}
			if err := os.WriteFile(filepath.Join(os.Getenv("OPENCODE_TEST_HOME"), "selected-model.json"), selection, 0o600); err != nil {
				http.Error(w, "cannot record selection", http.StatusInternalServerError)
				return
			}
			_, _ = w.Write([]byte(`{"data":{}}`))
			return
		}
		if r.URL.Path == "/api/session/ses_fixture/message" {
			if raw, err := os.ReadFile(filepath.Join(os.Getenv("OPENCODE_TEST_HOME"), "native-message-fixture.json")); err == nil {
				_, _ = w.Write(raw)
				return
			}
			_ = json.NewEncoder(w).Encode(map[string]any{"data": []any{}})
			return
		}
		if r.URL.Path == "/api/session/ses_fixture/prompt" {
			var body struct {
				Text string `json:"text"`
			}
			_ = json.NewDecoder(r.Body).Decode(&body)
			mu.Lock()
			if busy {
				mu.Unlock()
				http.Error(w, "duplicate prompt", 409)
				return
			}
			busy = true
			hold = strings.HasPrefix(body.Text, "hold")
			suspended := hold
			textOnly := strings.HasPrefix(body.Text, "hold-after-text") || strings.HasPrefix(body.Text, "hold-during-text")
			unfinished := strings.HasPrefix(body.Text, "hold-during-text")
			mu.Unlock()
			if textOnly {
				go func() {
					events := []map[string]any{
						{"type": "session.reasoning.started", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0}},
						{"type": "session.reasoning.delta", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0, "delta": "literal reasoning"}},
						{"type": "session.reasoning.ended", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0, "text": "literal reasoning"}},
						{"type": "session.text.started", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0}},
						{"type": "session.text.delta", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0, "delta": "literal partial"}},
					}
					if !unfinished {
						events = append(events, map[string]any{"type": "session.text.ended", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0, "text": "literal partial"}})
					}
					emit(events)
				}()
			} else if !suspended {
				go func() {
					emit([]map[string]any{
						{"type": "session.updated", "data": map[string]any{"info": map[string]any{"id": "ses_fixture", "title": "interim title"}}},
						{"type": "session.renamed", "data": map[string]any{"sessionID": "ses_fixture", "title": "literal conversation"}},
						{"type": "session.updated", "data": map[string]any{"sessionID": "ses_fixture", "status": "busy"}},
						{"type": "session.reasoning.started", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0}},
						{"type": "session.reasoning.delta", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0, "delta": "literal reasoning"}},
						{"type": "session.reasoning.ended", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0, "text": "literal reasoning"}},
						{"type": "session.execution.started", "data": map[string]any{"sessionID": "ses_fixture"}},
						{"type": "session.text.started", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0}},
						{"type": "session.text.delta", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0, "delta": "literal answer"}},
						{"type": "session.text.ended", "data": map[string]any{"sessionID": "ses_fixture", "assistantMessageID": "answer", "ordinal": 0, "text": "literal answer"}},
						{"type": "session.execution.succeeded", "data": map[string]any{"sessionID": "ses_fixture"}},
					})
					mu.Lock()
					busy = false
					mu.Unlock()
				}()
			}
		}
		if strings.HasSuffix(r.URL.Path, "/interrupt") {
			mu.Lock()
			suspended := hold
			hold = false
			mu.Unlock()
			if suspended {
				go func() {
					emit([]map[string]any{
						{"type": "session.execution.started", "data": map[string]any{"sessionID": "ses_fixture"}},
						{"type": "session.execution.interrupted", "data": map[string]any{"sessionID": "ses_fixture"}},
					})
					mu.Lock()
					busy = false
					mu.Unlock()
				}()
			}
			_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{}})
			return
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{}})
	})
	fmt.Printf("OpenCode server listening on http://%s\n", listener.Addr())
	_ = http.Serve(listener, handler)
}

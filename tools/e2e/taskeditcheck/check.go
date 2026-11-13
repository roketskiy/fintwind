package main

// Real Go daemon E2E for forkSessionFromResponse / rewindSessionToMessage.
// Wire shapes use camelCase commands (turnCount) with snake_case nested
// transcript rows (turn_count, provider_turn_started, turn_id, after_message).

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
	_ "modernc.org/sqlite"
)

type report struct {
	DaemonSHA256 string            `json:"daemonSha256"`
	Checks       map[string]bool   `json:"checks"`
	Notes        map[string]string `json:"notes"`
	Covered      []string          `json:"covered"`
	Uncovered    []string          `json:"uncovered"`
	Passed       bool              `json:"passed"`
	Error        string            `json:"error,omitempty"`
	Command      string            `json:"command"`
}

type harness struct {
	ctx       context.Context
	round     string
	dataDir   string
	projDir   string
	token     string
	daemonBin string
	self      string
	child     *ownedprocess.Owned
	client    *daemon.Client
	checks    map[string]bool
	notes     map[string]string
	projectID string
}

func run(ctx context.Context, daemonBin string) (err error) {
	if !filepath.IsAbs(daemonBin) {
		return errors.New("absolute daemon path required")
	}
	raw, err := os.ReadFile(daemonBin)
	if err != nil {
		return errors.New("daemon binary is not readable")
	}
	sum := sha256.Sum256(raw)
	out := report{DaemonSHA256: hex.EncodeToString(sum[:]), Checks: map[string]bool{}, Notes: map[string]string{}, Command: "taskeditcheck --daemon " + daemonBin}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	defer func() {
		out.Passed = err == nil
		if err != nil {
			out.Error = err.Error()
		}
		out.Covered = []string{
			"busy/invalid/missing-user/provider-mismatch/unsettled rejected without native POST",
			"fork keeps only selected provider turns, native before boundary, clears submission/queue/runtime cursor, remaps turn/message/block ids, source unchanged, independent native id, titles (2)(3)",
			"rewind clears selected user turn and later, native provider_started revert, actor stop gate, hydrate/restart, old UI save refused, future UUID save ok, native error not retried, CAS conflict, fence survives restart, SaveRuntime refuses stale snapshot",
			"submission row deletion verified through read-only DB",
		}
		out.Uncovered = []string{
			"live runtime actor stop ordering beyond the Stop gate: attachSession creates no actor without a real provider, so Stop ran as a no-op wait; ordering is structural (Stop before snapshot) plus hydrate/restart evidence",
			"scheduled delivery vs task edit mutex beyond the dispatch RLock gate: no deterministic wire interleaving without a real provider run",
		}
		data, marshalErr := json.MarshalIndent(out, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(round, "taskedit-result.json"), data, 0o600)
		}
		if marshalErr != nil {
			err = errors.Join(err, marshalErr)
		}
		fmt.Println("[REPORT]", filepath.Join(round, "taskedit-result.json"))
	}()
	self, err := os.Executable()
	if err != nil {
		return err
	}
	h := &harness{ctx: ctx, round: round, dataDir: filepath.Join(round, "data"), projDir: filepath.Join(round, "workspace", "proj"), token: uuid.NewString(), daemonBin: daemonBin, self: self, checks: out.Checks, notes: out.Notes, projectID: uuid.NewString()}
	if err := os.MkdirAll(h.dataDir, 0o700); err != nil {
		return err
	}
	if err := os.MkdirAll(h.projDir, 0o700); err != nil {
		return err
	}
	settings, _ := json.Marshal(map[string]string{"opencode_binary_path": self})
	if err := os.WriteFile(filepath.Join(h.dataDir, "daemon.json"), settings, 0o600); err != nil {
		return err
	}
	if err := h.startDaemon("first"); err != nil {
		return err
	}
	defer func() { err = errors.Join(err, h.closeDaemon()) }()
	if err := h.rejectionSuite(); err != nil {
		return err
	}
	if err := h.forkSuite(); err != nil {
		return err
	}
	if err := h.rewindSuite(); err != nil {
		return err
	}
	if err := h.failureInjectionSuite(); err != nil {
		return err
	}
	if err := h.restartSuite(); err != nil {
		return err
	}
	if err := h.storeCASSuite(); err != nil {
		return err
	}
	return nil
}

func (h *harness) startDaemon(name string) error {
	logs := filepath.Join(h.round, name)
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(h.daemonBin, []string{"--data-dir", h.dataDir, "--bind", "127.0.0.1:0"}, logs, h.round, ownedprocess.BuildEnv(h.round, map[string]string{"FINTWIND_DAEMON_TOKEN": h.token}))
	if err != nil {
		return err
	}
	address, err := waitDaemonReady(h.ctx, child)
	if err != nil {
		_ = child.Close()
		return err
	}
	client, err := daemon.Dial(h.ctx, address, h.token)
	if err != nil {
		_ = child.Close()
		return err
	}
	h.child, h.client = child, client
	return nil
}

func (h *harness) closeDaemon() error {
	if h.client != nil {
		h.client.Close()
		h.client = nil
	}
	if h.child != nil {
		err := h.child.Close()
		h.child = nil
		return err
	}
	return nil
}

func waitDaemonReady(ctx context.Context, child *ownedprocess.Owned) (string, error) {
	timer := time.NewTimer(15 * time.Second)
	defer timer.Stop()
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		data, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
		}
		if json.Unmarshal(data, &ready) == nil && ready.Address != "" {
			return ready.Address, nil
		}
		if done, err := child.Exited(0); err != nil || done {
			return "", errors.New("daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-timer.C:
			return "", errors.New("ready deadline exceeded")
		case <-tick.C:
		}
	}
}

func (h *harness) request(sessionID string, command map[string]any) (backendstore.Object, error) {
	raw, err := h.client.RequestFor(h.ctx, sessionID, "", command)
	if err != nil {
		return nil, err
	}
	return backendstore.Decode(raw)
}

func (h *harness) save(sessions ...backendstore.Object) error {
	project := backendstore.Fields(map[string]any{"id": h.projectID, "name": "proj", "path": h.projDir, "created_at": 1})
	list := make([]any, 0, len(sessions))
	for _, session := range sessions {
		raw, _ := json.Marshal(session)
		list = append(list, json.RawMessage(raw))
	}
	_, err := h.client.Request(h.ctx, map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{project}, "sessions": list, "skeletonSessionIds": []any{}})
	return err
}

func (h *harness) hydrate(id string) (backendstore.Object, error) {
	response, err := h.request(id, map[string]any{"type": "hydrateSession", "sessionId": id})
	if err != nil {
		return nil, err
	}
	if len(response["session"]) == 0 || string(response["session"]) == "null" {
		return nil, errors.New("hydration returned no session")
	}
	return backendstore.Decode(response["session"])
}

func (h *harness) edit(id, kind string, count int) (backendstore.Object, error) {
	return h.request(id, map[string]any{"type": kind, "turnCount": count})
}

func now() int64 { return time.Now().Unix() }

func message(id, turnID, role, content string) backendstore.Object {
	return backendstore.Fields(map[string]any{"id": id, "turn_id": turnID, "role": role, "content": content, "attachments": []any{}, "created_at": now(), "streaming": false})
}

func turn(id string, count int, started bool, status string) backendstore.Object {
	return backendstore.Fields(map[string]any{"id": id, "turn_count": count, "status": status, "provider_turn_started": started, "started_at": now()})
}

func block(id, turnID string, after int) backendstore.Object {
	return backendstore.Fields(map[string]any{"id": id, "turn_id": turnID, "after_message": after, "kind": "text", "content": "isolated " + id})
}

func cursor(native string) backendstore.Object {
	return backendstore.Fields(map[string]any{"provider": "openCode", "sessionId": native})
}

func baseSession(h *harness, id, title, status, native string, turns, messages, blocks []backendstore.Object) backendstore.Object {
	return backendstore.Fields(map[string]any{
		"id": id, "project_id": h.projectID, "title": title, "provider": "opencode", "status": status,
		"created_at": 1, "updated_at": now(), "last_reply_at": 0, "detail_loaded": true,
		"messages": messages, "turns": turns, "transcript_blocks": blocks, "queued_messages": []any{},
		"provider_cursor": cursor(native), "native_session_id": native,
	})
}

func nestedObjects(session backendstore.Object, key string) []backendstore.Object {
	var rows []backendstore.Object
	_ = json.Unmarshal(session[key], &rows)
	return rows
}

func (h *harness) callPath(name string) string {
	return filepath.Join(h.round, name)
}

func (h *harness) resetCalls() {
	_ = os.Remove(h.callPath("taskedit-calls.jsonl"))
}

type recordedCall struct {
	Method string          `json:"method"`
	Path   string          `json:"path"`
	Body   json.RawMessage `json:"body"`
}

func (h *harness) readCalls() []recordedCall {
	raw, err := os.ReadFile(h.callPath("taskedit-calls.jsonl"))
	if err != nil {
		return nil
	}
	var calls []recordedCall
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		var call recordedCall
		if json.Unmarshal([]byte(line), &call) == nil {
			calls = append(calls, call)
		}
	}
	return calls
}

func countCalls(calls []recordedCall, method, suffix string) int {
	total := 0
	for _, call := range calls {
		if call.Method == method && strings.HasSuffix(call.Path, suffix) {
			total++
		}
	}
	return total
}

func (h *harness) nativeUsers(native string) []string {
	raw, err := os.ReadFile(h.callPath("taskedit-native.json"))
	if err != nil {
		return nil
	}
	var state struct {
		Sessions map[string]struct {
			Messages []struct {
				ID   string `json:"id"`
				Role string `json:"role"`
			} `json:"messages"`
		} `json:"sessions"`
	}
	if json.Unmarshal(raw, &state) != nil {
		return nil
	}
	var users []string
	for _, message := range state.Sessions[native].Messages {
		if message.Role == "user" {
			users = append(users, message.ID)
		}
	}
	return users
}

func equalStrings(left, right []string) bool {
	if len(left) != len(right) {
		return false
	}
	for i := range left {
		if left[i] != right[i] {
			return false
		}
	}
	return true
}

func failIf(err error, contains string) error {
	if err == nil {
		return errors.New("expected failure containing " + contains + " but the call succeeded")
	}
	if !strings.Contains(err.Error(), contains) {
		return fmt.Errorf("expected error containing %q, got %q", contains, err.Error())
	}
	return nil
}

func (h *harness) readDB() (*sql.DB, error) {
	path := "file:" + filepath.ToSlash(filepath.Join(h.dataDir, "app.db")) + "?mode=ro"
	return sql.Open("sqlite", path)
}

func (h *harness) submissionIDs(session string) ([]string, error) {
	db, err := h.readDB()
	if err != nil {
		return nil, err
	}
	defer db.Close()
	rows, err := db.Query(`SELECT id FROM submissions WHERE session_id=? ORDER BY id`, session)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var ids []string
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			return nil, err
		}
		ids = append(ids, id)
	}
	return ids, rows.Err()
}

func (h *harness) removedTurnIDs(session string) ([]string, error) {
	db, err := h.readDB()
	if err != nil {
		return nil, err
	}
	defer db.Close()
	var raw string
	if err := db.QueryRow(`SELECT data FROM session_details WHERE session_id=?`, session).Scan(&raw); err != nil {
		return nil, err
	}
	detail, err := backendstore.Decode([]byte(raw))
	if err != nil {
		return nil, err
	}
	var removed []string
	if len(detail["_fintwind_removed_turn_ids"]) > 0 {
		if err := json.Unmarshal(detail["_fintwind_removed_turn_ids"], &removed); err != nil {
			return nil, err
		}
	}
	return removed, nil
}

func (h *harness) sessionCount() (int, error) {
	response, err := h.request(uuid.Nil.String(), map[string]any{"type": "loadTaskState"})
	if err != nil {
		return 0, err
	}
	var sessions []any
	if err := json.Unmarshal(response["sessions"], &sessions); err != nil {
		return 0, err
	}
	return len(sessions), nil
}

func execDB(path, query string, args ...any) error {
	db, err := sql.Open("sqlite", "file:"+filepath.ToSlash(path))
	if err != nil {
		return err
	}
	defer db.Close()
	_, err = db.Exec(query, args...)
	return err
}

func settleSubmission(dataDir, id string) error {
	record := model.NewSubmissionRecord("", id, "isolated prompt", nil)
	record.Receipt.State = model.SubmissionAccepted
	raw, err := json.Marshal(record)
	if err != nil {
		return err
	}
	return execDB(filepath.Join(dataDir, "app.db"), `UPDATE submissions SET state=?,data=? WHERE id=?`, model.SubmissionAccepted, string(raw), id)
}

func insertSubmission(dataDir, session, id, state string) error {
	record := model.NewSubmissionRecord(session, id, "isolated prompt", nil)
	record.Receipt.State = state
	raw, err := json.Marshal(record)
	if err != nil {
		return err
	}
	return execDB(filepath.Join(dataDir, "app.db"), `INSERT INTO submissions(id,session_id,state,data) VALUES(?,?,?,?)`, id, session, state, string(raw))
}

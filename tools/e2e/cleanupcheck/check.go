package main

// Harness: real Go daemon over WS, owned fixture binary via daemon.json,
// real SQLite. No models, no credentials, no production data.

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
	ctx        context.Context
	round      string
	dataDir    string
	projDir    string
	generalDir string
	token      string
	daemonBin  string
	self       string
	child      *ownedprocess.Owned
	client     *daemon.Client
	checks     map[string]bool
	notes      map[string]string
}

func oldSec() int64 { return time.Now().Unix() - 40*86400 - 7200 }
func oldMs() uint64 { return uint64(oldSec()) * 1000 }

// midSec is a retention cutoff strictly newer than oldSec fixtures but still
// beyond the 30-day minimum, so eligibility never depends on same-second
// timing. Equality with a fixture timestamp is only asserted on purpose.
func midSec() int64 { return time.Now().Unix() - 35*86400 }

func run(ctx context.Context, daemonBin string) (err error) {
	if !filepath.IsAbs(daemonBin) {
		return errors.New("absolute daemon path required")
	}
	raw, err := os.ReadFile(daemonBin)
	if err != nil {
		return errors.New("daemon binary is not readable")
	}
	sum := sha256.Sum256(raw)
	out := report{DaemonSHA256: hex.EncodeToString(sum[:]), Checks: map[string]bool{}, Notes: map[string]string{}, Command: "cleanupcheck --daemon " + daemonBin}
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
			"period Days30/Days90/Days180, preview.session_ids snake, outcome Deleted{session_ids}/Skipped",
			"30d threshold, busy/archived/queue/unsettled/runtime/draft blocked with zero native DELETE",
			"native active child, fresh child, ineligible tree child local, parentId-alias fresh child each retain whole tree",
			"inventory cycle/empty-page/duplicate/over-budget/50-page/invalid-active refused without local delete",
			"recheck id mismatch and recheck transport error fail; fresh recheck and late active skip safely; GET/DELETE 404 tolerated",
			"unknown DELETE sent exactly once on failure; no retry",
			"preview only visible; tree delete (incl parentId alias child) removes associated locals in one transaction with tombstones; normal and external projects retained; empty general, general-subdir and legacy-projects metadata GC keeps directories; accepted submission rows removed",
			"restart: deleted IDs not resurrected by old Save/Admit; ordinary removeSession/removeProject fenced after restart",
			"SQLite CAS conflict on RemoveCleaned in module store; tombstone table persists and reloads",
		}
		out.Uncovered = []string{
			"cleanup exclusive lock vs draft save/apply read lock interleaving: structural (dispatch gates) only, no deterministic wire contention",
			"scheduled delivery vs cleanup lock interleaving: structural only",
			"live runtime Stop ordering inside cleanup: live actor created via start, blocked via attachSession; deeper actor internals not exercised",
		}
		data, marshalErr := json.MarshalIndent(out, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(round, "cleanup-result.json"), data, 0o600)
		}
		if marshalErr != nil {
			err = errors.Join(err, marshalErr)
		}
		fmt.Println("[REPORT]", filepath.Join(round, "cleanup-result.json"))
	}()
	self, err := os.Executable()
	if err != nil {
		return err
	}
	h := &harness{ctx: ctx, round: round, dataDir: filepath.Join(round, "data"), projDir: filepath.Join(round, "workspace", "proj"), token: uuid.NewString(), daemonBin: daemonBin, self: self, checks: out.Checks, notes: out.Notes}
	if err := os.MkdirAll(h.dataDir, 0o700); err != nil {
		return err
	}
	if err := os.MkdirAll(h.projDir, 0o700); err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Join(round, "workspace", "projects"), 0o700); err != nil {
		return err
	}
	general, err := filepath.EvalSymlinks(filepath.Join(round, "workspace", "projects"))
	if err != nil {
		return err
	}
	h.generalDir = general
	settings, _ := json.Marshal(map[string]string{"opencode_binary_path": self})
	if err := os.WriteFile(filepath.Join(h.dataDir, "daemon.json"), settings, 0o600); err != nil {
		return err
	}
	if err := h.startDaemon("first"); err != nil {
		return err
	}
	defer func() { err = errors.Join(err, h.closeDaemon()) }()
	for _, suite := range []func() error{
		h.wireSuite, h.localBlocksSuite, h.treeSuite, h.inventorySuite,
		h.recheckSuite, h.successSuite, h.deleteFailSuite, h.restartSuite, h.storeSuite,
	} {
		if err := suite(); err != nil {
			return err
		}
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

func (h *harness) saveState(projects []any, sessions ...backendstore.Object) error {
	list := make([]any, 0, len(sessions))
	for _, session := range sessions {
		raw, _ := json.Marshal(session)
		list = append(list, json.RawMessage(raw))
	}
	_, err := h.client.Request(h.ctx, map[string]any{"type": "saveTaskStateCheckpoint", "projects": projects, "sessions": list, "skeletonSessionIds": []any{}})
	return err
}

func (h *harness) project(id, name, path string) backendstore.Object {
	return backendstore.Fields(map[string]any{"id": id, "name": name, "path": path, "created_at": 1})
}

func (h *harness) hydrate(id string) (backendstore.Object, error) {
	response, err := h.request(id, map[string]any{"type": "hydrateSession", "sessionId": id})
	if err != nil {
		return nil, err
	}
	if len(response["session"]) == 0 || string(response["session"]) == "null" {
		return nil, nil
	}
	return backendstore.Decode(response["session"])
}

func now() int64 { return time.Now().Unix() }

func oldTask(h *harness, id, projectID, title, native string) backendstore.Object {
	return backendstore.Fields(map[string]any{
		"id": id, "project_id": projectID, "title": title, "provider": "opencode", "status": "idle",
		"created_at": oldSec(), "updated_at": oldSec(), "last_reply_at": oldSec(), "detail_loaded": true,
		"messages": []any{map[string]any{"id": uuid.NewString(), "turn_id": uuid.NewString(), "role": "user", "content": "isolated", "attachments": []any{}, "created_at": oldSec(), "streaming": false}},
		"turns":    []any{}, "transcript_blocks": []any{}, "queued_messages": []any{},
		"provider_cursor": map[string]any{"provider": "openCode", "sessionId": native}, "native_session_id": native,
	})
}

func (h *harness) callPath(name string) string { return filepath.Join(h.round, name) }

func (h *harness) resetCalls() { _ = os.Remove(h.callPath("cleanup-calls.jsonl")) }

type recordedCall struct {
	Method string          `json:"method"`
	Path   string          `json:"path"`
	Body   json.RawMessage `json:"body"`
}

func (h *harness) readCalls() []recordedCall {
	raw, err := os.ReadFile(h.callPath("cleanup-calls.jsonl"))
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

func countMethod(calls []recordedCall, method string) int {
	total := 0
	for _, call := range calls {
		if call.Method == method {
			total++
		}
	}
	return total
}

func countCalls(calls []recordedCall, method, suffix string) int {
	total := 0
	for _, call := range calls {
		path := call.Path
		if index := strings.LastIndex(path, " -> "); index >= 0 {
			path = path[:index]
		}
		if call.Method == method && strings.HasSuffix(path, suffix) {
			total++
		}
	}
	return total
}

func (h *harness) writeSeed(seed any) error {
	raw, err := json.Marshal(seed)
	if err != nil {
		return err
	}
	return os.WriteFile(h.callPath("cleanup-seed.json"), raw, 0o600)
}

type seedRowWire struct {
	ID     string `json:"id"`
	Parent string `json:"parentID"`
	Time   struct {
		Created uint64 `json:"created"`
		Updated uint64 `json:"updated"`
	} `json:"time"`
}

func rowWire(id, parent string, updatedMs uint64) seedRowWire {
	row := seedRowWire{ID: id, Parent: parent}
	row.Time.Created = updatedMs
	row.Time.Updated = updatedMs
	return row
}

func pageSeedRaw(cursor string, rows []any, next string) map[string]any {
	wire := make([]any, 0, len(rows))
	wire = append(wire, rows...)
	return map[string]any{"cursor": cursor, "rows": wire, "next": next}
}

func seedInventory(pages []map[string]any, active map[string]any) map[string]any {
	if active == nil {
		active = map[string]any{}
	}
	return map[string]any{"pages": pages, "active": active}
}

func pageSeed(cursor string, rows []seedRowWire, next string) map[string]any {
	wire := make([]any, 0, len(rows))
	for _, row := range rows {
		wire = append(wire, row)
	}
	return map[string]any{"cursor": cursor, "rows": wire, "next": next}
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

func (h *harness) preview(period string, visible []string) (backendstore.Object, error) {
	return h.request(uuid.Nil.String(), map[string]any{"type": "previewSessionCleanup", "period": period, "visibleSessionIds": visible})
}

func (h *harness) cleanup(id string, before int64) (backendstore.Object, error) {
	return h.request(id, map[string]any{"type": "cleanupOldSession", "before": before})
}

func previewIDs(response backendstore.Object) ([]string, error) {
	preview, err := backendstore.Decode(response["preview"])
	if err != nil {
		return nil, err
	}
	var ids []string
	if err := json.Unmarshal(preview["session_ids"], &ids); err != nil {
		return nil, err
	}
	return ids, nil
}

func isSkipped(response backendstore.Object) bool {
	return response.String("type") == "sessionCleanupFinished" && string(response["outcome"]) == `"Skipped"`
}

func deletedIDs(response backendstore.Object) ([]string, error) {
	if response.String("type") != "sessionCleanupFinished" {
		return nil, errors.New("not a cleanup finished reply")
	}
	outcome, err := backendstore.Decode(response["outcome"])
	if err != nil {
		return nil, err
	}
	deleted, err := backendstore.Decode(outcome["Deleted"])
	if err != nil {
		return nil, err
	}
	var ids []string
	if err := json.Unmarshal(deleted["session_ids"], &ids); err != nil {
		return nil, err
	}
	return ids, nil
}

func (h *harness) readDB() (*sql.DB, error) {
	return sql.Open("sqlite", "file:"+filepath.ToSlash(filepath.Join(h.dataDir, "app.db"))+"?mode=ro")
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

func (h *harness) tombstones() (map[string]string, error) {
	db, err := h.readDB()
	if err != nil {
		return nil, err
	}
	defer db.Close()
	rows, err := db.Query(`SELECT kind,id FROM deleted_task_identities`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := map[string]string{}
	for rows.Next() {
		var kind, id string
		if err := rows.Scan(&kind, &id); err != nil {
			return nil, err
		}
		out[kind+"/"+id] = kind
	}
	return out, rows.Err()
}

func (h *harness) sessionIDs() ([]string, error) {
	response, err := h.request(uuid.Nil.String(), map[string]any{"type": "loadTaskState"})
	if err != nil {
		return nil, err
	}
	var sessions []struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(response["sessions"], &sessions); err != nil {
		return nil, err
	}
	ids := make([]string, 0, len(sessions))
	for _, session := range sessions {
		ids = append(ids, session.ID)
	}
	return ids, nil
}

func (h *harness) projectIDs() ([]string, error) {
	response, err := h.request(uuid.Nil.String(), map[string]any{"type": "loadTaskState"})
	if err != nil {
		return nil, err
	}
	var projects []struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(response["projects"], &projects); err != nil {
		return nil, err
	}
	ids := make([]string, 0, len(projects))
	for _, project := range projects {
		ids = append(ids, project.ID)
	}
	return ids, nil
}

func execDB(dataDir, query string, args ...any) error {
	db, err := sql.Open("sqlite", "file:"+filepath.ToSlash(filepath.Join(dataDir, "app.db")))
	if err != nil {
		return err
	}
	defer db.Close()
	_, err = db.Exec(query, args...)
	return err
}

func insertSubmission(dataDir, session, id, state string) error {
	record := model.NewSubmissionRecord(session, id, "isolated prompt", nil)
	record.Receipt.State = state
	raw, err := json.Marshal(record)
	if err != nil {
		return err
	}
	return execDB(dataDir, `INSERT INTO submissions(id,session_id,state,data) VALUES(?,?,?,?)`, id, session, state, string(raw))
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

func containsString(list []string, want string) bool {
	for _, item := range list {
		if item == want {
			return true
		}
	}
	return false
}

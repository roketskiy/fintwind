package main

// Suites: wire, local blocks, tree, inventory, recheck, success, delete
// failure, restart tombstones, module store CAS.

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

func (h *harness) runtimeRequest(sessionID, runtimeID string, command map[string]any) (backendstore.Object, error) {
	raw, err := h.client.RequestFor(h.ctx, sessionID, runtimeID, command)
	if err != nil {
		return nil, err
	}
	return backendstore.Decode(raw)
}

func (h *harness) wireSuite() error {
	if err := h.writeSeed(seedInventory([]map[string]any{}, nil)); err != nil {
		return err
	}
	if _, err := h.preview("Days7", []string{}); err == nil {
		return errors.New("unknown period was accepted")
	} else if err := failIf(err, "unknown cleanup retention period"); err != nil {
		return err
	}
	for _, tc := range []struct {
		period string
		days   int64
	}{
		{"Days30", 30}, {"Days90", 90}, {"Days180", 180},
	} {
		response, err := h.preview(tc.period, []string{})
		if err != nil {
			return fmt.Errorf("preview %s: %w", tc.period, err)
		}
		if response.String("type") != "sessionCleanupPreview" {
			return fmt.Errorf("preview %s wrong reply", tc.period)
		}
		preview, err := backendstore.Decode(response["preview"])
		if err != nil {
			return err
		}
		before, err := preview.Int("before")
		if err != nil {
			return fmt.Errorf("preview before not an integer: %w", err)
		}
		want := time.Now().Unix() - tc.days*86400
		if before < want-120 || before > want+120 {
			return fmt.Errorf("preview %s before=%d, want about %d", tc.period, before, want)
		}
		ids, err := previewIDs(response)
		if err != nil || len(ids) != 0 {
			return fmt.Errorf("preview %s ids %v: %v", tc.period, ids, err)
		}
	}
	h.checks["wire-period-before-snake"] = true

	now := time.Now().Unix()
	for _, before := range []int64{0, -5, now - 10*86400, now - 30*86400 + 3600} {
		if _, err := h.cleanup(uuid.NewString(), before); err == nil {
			return fmt.Errorf("before=%d was accepted", before)
		} else if err := failIf(err, "at least 30 days retention"); err != nil {
			return err
		}
	}
	if _, err := h.cleanup("00000000-0000-0000-0000-000000000000", now-30*86400); err == nil {
		return errors.New("zero identity cleanup was accepted")
	} else if err := failIf(err, "cleanup task identity required"); err != nil {
		return err
	}
	if _, err := h.preview("Days30", []string{"not-a-uuid"}); err == nil {
		return errors.New("invalid visible identity was accepted")
	} else if err := failIf(err, "invalid visible task identity"); err != nil {
		return err
	}
	if _, err := h.request(uuid.Nil.String(), map[string]any{"type": "previewSessionCleanup", "period": "Days30"}); err == nil {
		return errors.New("preview without visible set was accepted")
	}
	huge := make([]string, 100001)
	for i := range huge {
		huge[i] = uuid.NewString()
	}
	if _, err := h.preview("Days30", huge); err == nil {
		return errors.New("oversize visible set was accepted")
	} else if err := failIf(err, "exceeds budget"); err != nil {
		return err
	}
	h.checks["wire-validation"] = true
	return nil
}

func (h *harness) localBlocksSuite() error {
	projectID := uuid.NewString()
	project := h.project(projectID, "blocks", h.projDir)
	mk := func(id, native string) backendstore.Object { return oldTask(h, id, projectID, "block", native) }
	busy := mk(uuid.NewString(), "ses_b1")
	busy.Set("status", "working")
	archived := mk(uuid.NewString(), "ses_b2")
	archived.Set("archived", true)
	queued := mk(uuid.NewString(), "ses_b3")
	queued.Set("queued_messages", []any{map[string]any{"id": uuid.NewString()}})
	unsettled := mk(uuid.NewString(), "ses_b4")
	turnID := uuid.NewString()
	unsettled.Set("turns", []any{map[string]any{"id": turnID, "turn_count": 1, "status": "running", "provider_turn_started": false, "started_at": oldSec()}})
	eqT0 := oldSec() - 3600
	equal := mk(uuid.NewString(), "ses_b7")
	equal.Set("created_at", eqT0)
	equal.Set("updated_at", eqT0)
	equal.Set("last_reply_at", eqT0)
	runner := mk(uuid.NewString(), "ses_b5")
	drafted := mk(uuid.NewString(), "ses_b6")
	sessions := []backendstore.Object{busy, archived, queued, unsettled, equal, runner, drafted}
	if err := h.saveState([]any{project}, sessions...); err != nil {
		return fmt.Errorf("seed block sessions: %w", err)
	}
	record := model.NewSubmissionRecord(unsettled.String("id"), turnID, "isolated prompt", nil)
	submission, _ := json.Marshal(record)
	if _, err := h.request(unsettled.String("id"), map[string]any{"type": "acceptSubmission", "project": project, "session": unsettled, "submission": json.RawMessage(submission)}); err != nil {
		return fmt.Errorf("admit unsettled: %w", err)
	}
	runtimeID := uuid.NewString()
	if _, err := h.runtimeRequest(runner.String("id"), runtimeID, map[string]any{"type": "start", "options": map[string]any{"binary": h.self, "cwd": h.projDir}}); err != nil {
		return fmt.Errorf("start runtime: %w", err)
	}
	attached, err := h.runtimeRequest(runner.String("id"), "", map[string]any{"type": "attachSession"})
	if err != nil {
		return err
	}
	if attached.String("runtimeId") == "" {
		return errors.New("runtime did not attach")
	}
	if _, err := h.request(uuid.Nil.String(), map[string]any{"type": "saveComposerDrafts", "generation": 0, "drafts": map[string]any{"sessions": map[string]any{drafted.String("id"): map[string]any{"text": "isolated draft"}}}}); err != nil {
		return fmt.Errorf("save draft: %w", err)
	}
	rows := []seedRowWire{rowWire("ses_b1", "", oldMs()), rowWire("ses_b2", "", oldMs()), rowWire("ses_b3", "", oldMs()), rowWire("ses_b4", "", oldMs()), rowWire("ses_b5", "", oldMs()), rowWire("ses_b6", "", oldMs()), rowWire("ses_b7", "", oldMs())}
	if err := h.writeSeed(seedInventory([]map[string]any{pageSeed("", rows, "")}, nil)); err != nil {
		return err
	}
	h.resetCalls()
	ids := []string{}
	for _, session := range []backendstore.Object{busy, archived, queued, unsettled, runner, drafted} {
		id := session.String("id")
		ids = append(ids, id)
		response, err := h.cleanup(id, midSec())
		if err != nil {
			return fmt.Errorf("blocked cleanup %s: %w", id, err)
		}
		if !isSkipped(response) {
			return fmt.Errorf("blocked session %s was not skipped", id)
		}
	}
	previewed, err := h.preview("Days30", ids)
	if err != nil {
		return err
	}
	previewList, err := previewIDs(previewed)
	if err != nil {
		return err
	}
	if len(previewList) != 0 {
		return fmt.Errorf("blocked sessions leaked into preview: %v", previewList)
	}
	if calls := h.readCalls(); countMethod(calls, "DELETE") != 0 {
		raw, _ := json.Marshal(calls)
		return fmt.Errorf("blocked cleanup reached native delete: %s", raw)
	}
	for _, id := range ids {
		if session, err := h.hydrate(id); err != nil || session == nil {
			return fmt.Errorf("blocked session %s changed: %v", id, err)
		}
	}
	h.checks["local-blocks-skipped-no-delete"] = true
	equalResponse, err := h.cleanup(equal.String("id"), eqT0)
	if err != nil {
		return fmt.Errorf("equality boundary: %w", err)
	}
	if !isSkipped(equalResponse) {
		return errors.New("updated==before was not retained")
	}
	if session, err := h.hydrate(equal.String("id")); err != nil || session == nil {
		return fmt.Errorf("equality session changed: %v", err)
	}
	h.checks["equality-boundary-retained"] = true
	if _, err := h.runtimeRequest(runner.String("id"), runtimeID, map[string]any{"type": "closeSession"}); err != nil {
		return fmt.Errorf("close runtime: %w", err)
	}
	if _, err := h.request(uuid.Nil.String(), map[string]any{"type": "applyComposerDraftChanges", "changes": []any{map[string]any{"target": map[string]any{"type": "session", "sessionId": drafted.String("id")}, "draft": nil}}}); err != nil {
		return fmt.Errorf("clear draft: %w", err)
	}
	cleared, err := h.preview("Days30", []string{drafted.String("id")})
	if err != nil {
		return err
	}
	clearedList, err := previewIDs(cleared)
	if err != nil {
		return err
	}
	if len(clearedList) != 1 || clearedList[0] != drafted.String("id") {
		return fmt.Errorf("draft clear did not unblock preview: %v", clearedList)
	}
	h.checks["draft-clear-unblocks"] = true
	return nil
}

func (h *harness) treeSuite() error {
	projectID := uuid.NewString()
	project := h.project(projectID, "tree", h.projDir)
	rootA, childA := mkTreePair(h, projectID, "ses_ta_root", "ses_ta_child")
	rootB, childB := mkTreePair(h, projectID, "ses_tb_root", "ses_tb_child")
	rootC, childC := mkTreePair(h, projectID, "ses_tc_root", "ses_tc_child")
	childC.Set("status", "working")
	rootD := oldTask(h, uuid.NewString(), projectID, "root", "ses_rd")
	childD := oldTask(h, uuid.NewString(), projectID, "child", "ses_cd")
	if err := h.saveState([]any{project}, rootA, childA, rootB, childB, rootC, childC, rootD, childD); err != nil {
		return fmt.Errorf("seed tree sessions: %w", err)
	}
	aliasFresh := []any{
		map[string]any{"id": "ses_rd", "parentID": "", "time": map[string]any{"created": oldMs(), "updated": oldMs()}},
		map[string]any{"id": "ses_cd", "parentId": "ses_rd", "time": map[string]any{"created": oldMs(), "updated": uint64(time.Now().UnixMilli())}},
	}
	cases := []struct {
		name string
		root string
		seed map[string]any
	}{
		{"active-child", rootA.String("id"), seedInventory([]map[string]any{pageSeed("", []seedRowWire{rowWire("ses_ta_root", "", oldMs()), rowWire("ses_ta_child", "ses_ta_root", oldMs())}, "")}, map[string]any{"ses_ta_child": map[string]any{}})},
		{"fresh-child", rootB.String("id"), seedInventory([]map[string]any{pageSeed("", []seedRowWire{rowWire("ses_tb_root", "", oldMs()), rowWire("ses_tb_child", "ses_tb_root", uint64(time.Now().UnixMilli()))}, "")}, nil)},
		{"busy-child-local", rootC.String("id"), seedInventory([]map[string]any{pageSeed("", []seedRowWire{rowWire("ses_tc_root", "", oldMs()), rowWire("ses_tc_child", "ses_tc_root", oldMs())}, "")}, nil)},
		{"alias-fresh-child", rootD.String("id"), seedInventory([]map[string]any{pageSeedRaw("", aliasFresh, "")}, nil)},
	}
	h.resetCalls()
	for _, tc := range cases {
		if err := h.writeSeed(tc.seed); err != nil {
			return err
		}
		response, err := h.cleanup(tc.root, midSec())
		if err != nil {
			return fmt.Errorf("%s: %w", tc.name, err)
		}
		if !isSkipped(response) {
			return fmt.Errorf("%s: root was not skipped", tc.name)
		}
		previewed, err := h.preview("Days30", []string{tc.root})
		if err != nil {
			return fmt.Errorf("%s preview: %w", tc.name, err)
		}
		list, err := previewIDs(previewed)
		if err != nil {
			return err
		}
		if len(list) != 0 {
			return fmt.Errorf("%s: preview leaked %v", tc.name, list)
		}
		if session, err := h.hydrate(tc.root); err != nil || session == nil {
			return fmt.Errorf("%s: root changed: %v", tc.name, err)
		}
		h.notes["tree-"+tc.name] = "skipped"
	}
	if calls := h.readCalls(); countMethod(calls, "DELETE") != 0 {
		return errors.New("tree veto reached native delete")
	}
	h.checks["tree-veto-retains-root"] = true
	return nil
}

func mkTreePair(h *harness, projectID, rootNative, childNative string) (backendstore.Object, backendstore.Object) {
	root := oldTask(h, uuid.NewString(), projectID, "root", rootNative)
	child := oldTask(h, uuid.NewString(), projectID, "child", childNative)
	return root, child
}

func (h *harness) inventorySuite() error {
	projectID := uuid.NewString()
	project := h.project(projectID, "inventory", h.projDir)
	session := oldTask(h, uuid.NewString(), projectID, "inventory", "ses_inv")
	if err := h.saveState([]any{project}, session); err != nil {
		return err
	}
	id := session.String("id")
	huge := make([]seedRowWire, 0, 501)
	for i := 0; i < 501; i++ {
		huge = append(huge, rowWire(fmt.Sprintf("ses_huge_%d", i), "", oldMs()))
	}
	fifty := []map[string]any{}
	for i := 0; i < 50; i++ {
		cursor := fmt.Sprintf("p%d", i)
		if i == 0 {
			cursor = ""
		}
		fifty = append(fifty, pageSeed(cursor, []seedRowWire{rowWire(fmt.Sprintf("ses_fifty_%d", i), "", oldMs())}, fmt.Sprintf("p%d", i+1)))
	}
	seeds := []struct {
		name string
		seed map[string]any
	}{
		{"cycle", seedInventory([]map[string]any{
			pageSeed("", []seedRowWire{rowWire("ses_c1", "", oldMs()), rowWire("ses_c2", "", oldMs())}, "c1"),
			pageSeed("c1", []seedRowWire{rowWire("ses_c3", "", oldMs())}, "c1"),
		}, nil)},
		{"empty-page", seedInventory([]map[string]any{pageSeed("", []seedRowWire{}, "c1")}, nil)},
		{"duplicate", seedInventory([]map[string]any{
			pageSeed("", []seedRowWire{rowWire("ses_d1", "", oldMs())}, "c1"),
			pageSeed("c1", []seedRowWire{rowWire("ses_d1", "", oldMs())}, ""),
		}, nil)},
		{"over-budget", seedInventory([]map[string]any{pageSeed("", huge, "")}, nil)},
		{"fifty-pages", seedInventory(fifty, nil)},
		{"bad-active", func() map[string]any {
			seed := seedInventory([]map[string]any{pageSeed("", []seedRowWire{rowWire("ses_inv", "", oldMs())}, "")}, nil)
			seed["activeNull"] = true
			return seed
		}()},
	}
	h.resetCalls()
	for _, tc := range seeds {
		if err := h.writeSeed(tc.seed); err != nil {
			return err
		}
		if _, err := h.cleanup(id, midSec()); err == nil {
			return fmt.Errorf("inventory %s was accepted", tc.name)
		}
		if session, err := h.hydrate(id); err != nil || session == nil {
			return fmt.Errorf("inventory %s changed local: %v", tc.name, err)
		}
		h.notes["inventory-"+tc.name] = "refused"
	}
	if calls := h.readCalls(); countMethod(calls, "DELETE") != 0 {
		return errors.New("refused inventory reached native delete")
	}
	h.checks["inventory-failures-refused"] = true
	return nil
}

func (h *harness) recheckSuite() error {
	projectID := uuid.NewString()
	project := h.project(projectID, "recheck", h.projDir)
	mismatch := oldTask(h, uuid.NewString(), projectID, "mismatch", "ses_mm")
	fresh := oldTask(h, uuid.NewString(), projectID, "fresh", "ses_fr")
	broken := oldTask(h, uuid.NewString(), projectID, "broken", "ses_br")
	flipped := oldTask(h, uuid.NewString(), projectID, "flipped", "ses_fl")
	if err := h.saveState([]any{project}, mismatch, fresh, broken, flipped); err != nil {
		return err
	}
	withGet := func(native string, override map[string]any) map[string]any {
		seed := seedInventory([]map[string]any{pageSeed("", []seedRowWire{rowWire(native, "", oldMs())}, "")}, nil)
		seed["get"] = map[string]any{native: override}
		return seed
	}
	h.resetCalls()
	if err := h.writeSeed(withGet("ses_mm", map[string]any{"mismatch": "ses_other"})); err != nil {
		return err
	}
	if err := failIf(func() error { _, err := h.cleanup(mismatch.String("id"), midSec()); return err }(), "different session"); err != nil {
		return err
	}
	if err := h.writeSeed(withGet("ses_br", map[string]any{"status": 500})); err != nil {
		return err
	}
	if _, err := h.cleanup(broken.String("id"), midSec()); err == nil {
		return errors.New("recheck transport failure was hidden")
	}
	if err := h.writeSeed(withGet("ses_fr", map[string]any{"fresh": true})); err != nil {
		return err
	}
	response, err := h.cleanup(fresh.String("id"), midSec())
	if err != nil {
		return fmt.Errorf("fresh recheck: %w", err)
	}
	if !isSkipped(response) {
		return errors.New("fresh recheck was not skipped")
	}
	flip := seedInventory([]map[string]any{pageSeed("", []seedRowWire{rowWire("ses_fl", "", oldMs())}, "")}, nil)
	flip["activeFlip"] = []string{"ses_fl"}
	if err := h.writeSeed(flip); err != nil {
		return err
	}
	flippedResponse, err := h.cleanup(flipped.String("id"), midSec())
	if err != nil {
		return fmt.Errorf("late active: %w", err)
	}
	if !isSkipped(flippedResponse) {
		return errors.New("late active was not skipped")
	}
	if calls := h.readCalls(); countMethod(calls, "DELETE") != 0 {
		return errors.New("recheck gate reached native delete")
	}
	for _, session := range []backendstore.Object{mismatch, fresh, broken, flipped} {
		if current, err := h.hydrate(session.String("id")); err != nil || current == nil {
			return fmt.Errorf("recheck changed %s: %v", session.String("id"), err)
		}
	}
	h.checks["recheck-gates"] = true
	return nil
}

var (
	successT1Old []byte
	successT2Old []byte
	successT1ID  string
	successT2ID  string
)

func (h *harness) successSuite() error {
	normalID := uuid.NewString()
	generalID := uuid.NewString()
	subID := uuid.NewString()
	extID := uuid.NewString()
	legID := uuid.NewString()
	subDir := filepath.Join(h.generalDir, "sub")
	if err := os.MkdirAll(subDir, 0o700); err != nil {
		return err
	}
	extDir := filepath.Join(h.round, "external")
	if err := os.MkdirAll(extDir, 0o700); err != nil {
		return err
	}
	legacy := filepath.Join(filepath.Dir(h.dataDir), "home", ".fintwind")
	normal := h.project(normalID, "normal", h.projDir)
	general := h.project(generalID, "general", h.generalDir)
	sub := h.project(subID, "general-sub", subDir)
	ext := h.project(extID, "external", extDir)
	leg := h.project(legID, "legacy", filepath.Join(legacy, "projects", "old-proj"))
	t1 := oldTask(h, uuid.NewString(), normalID, "gone-one", "ses_root")
	t2 := oldTask(h, uuid.NewString(), generalID, "gone-two", "ses_child")
	t3 := oldTask(h, uuid.NewString(), subID, "gone-three", "ses_g")
	t4 := oldTask(h, uuid.NewString(), extID, "gone-four", "ses_g2")
	t5 := oldTask(h, uuid.NewString(), legID, "gone-five", "ses_g3")
	t6 := oldTask(h, uuid.NewString(), normalID, "gone-six", "ses_ca")
	keep := oldTask(h, uuid.NewString(), normalID, "keep", "ses_keep")
	keep.Set("created_at", now())
	keep.Set("updated_at", now())
	keep.Set("last_reply_at", now())
	if err := h.saveState([]any{normal, general, sub, ext, leg}, t1, t2, t3, t4, t5, t6, keep); err != nil {
		return fmt.Errorf("seed success: %w", err)
	}
	if err := insertSubmission(h.dataDir, t2.String("id"), uuid.NewString(), model.SubmissionAccepted); err != nil {
		return fmt.Errorf("seed submission: %w", err)
	}
	successT1ID, successT2ID = t1.String("id"), t2.String("id")
	raw1, _ := json.Marshal(t1)
	raw2, _ := json.Marshal(t2)
	successT1Old, successT2Old = raw1, raw2
	rows := []any{
		map[string]any{"id": "ses_root", "parentID": "", "time": map[string]any{"created": oldMs(), "updated": oldMs()}},
		map[string]any{"id": "ses_child", "parentID": "ses_root", "time": map[string]any{"created": oldMs(), "updated": oldMs()}},
		map[string]any{"id": "ses_g", "parentID": "ses_child", "time": map[string]any{"created": oldMs(), "updated": oldMs()}},
		map[string]any{"id": "ses_g2", "parentID": "ses_child", "time": map[string]any{"created": oldMs(), "updated": oldMs()}},
		map[string]any{"id": "ses_g3", "parentID": "ses_root", "time": map[string]any{"created": oldMs(), "updated": oldMs()}},
		map[string]any{"id": "ses_ca", "parentId": "ses_root", "time": map[string]any{"created": oldMs(), "updated": oldMs()}},
		map[string]any{"id": "ses_keep", "parentID": "", "time": map[string]any{"created": uint64(time.Now().UnixMilli()), "updated": uint64(time.Now().UnixMilli())}},
	}
	seed := seedInventory([]map[string]any{pageSeedRaw("", rows, "")}, nil)
	seed["get"] = map[string]any{"ses_child": map[string]any{"status": 404}}
	seed["delete"] = map[string]any{"ses_root": map[string]any{"status": 404}}
	if err := h.writeSeed(seed); err != nil {
		return err
	}
	gone := []string{t1.String("id"), t2.String("id"), t3.String("id"), t4.String("id"), t5.String("id"), t6.String("id")}
	previewed, err := h.preview("Days30", append(append([]string{}, gone...), keep.String("id")))
	if err != nil {
		return err
	}
	list, err := previewIDs(previewed)
	if err != nil {
		return err
	}
	sort.Strings(list)
	want := append([]string{}, gone...)
	sort.Strings(want)
	if !equalStrings(list, want) {
		return fmt.Errorf("preview = %v, want %v", list, want)
	}
	only, err := h.preview("Days30", []string{keep.String("id")})
	if err != nil {
		return err
	}
	onlyList, err := previewIDs(only)
	if err != nil {
		return err
	}
	if len(onlyList) != 0 {
		return fmt.Errorf("preview leaked fresh session: %v", onlyList)
	}
	h.checks["preview-visible-only"] = true
	h.resetCalls()
	deleted, err := h.cleanup(t1.String("id"), midSec())
	if err != nil {
		return fmt.Errorf("success cleanup: %w", err)
	}
	ids, err := deletedIDs(deleted)
	if err != nil {
		return err
	}
	if !equalStrings(ids, want) {
		return fmt.Errorf("deleted = %v, want %v", ids, want)
	}
	calls := h.readCalls()
	if countMethod(calls, "DELETE") != 1 || countCalls(calls, "DELETE", "/ses_root") != 1 {
		raw, _ := json.Marshal(calls)
		return fmt.Errorf("native delete sequence mismatch (want single root DELETE): %s", raw)
	}
	for _, id := range gone {
		if session, err := h.hydrate(id); err != nil || session != nil {
			return fmt.Errorf("deleted session %s still hydrates: %v", id, err)
		}
	}
	if kept, err := h.hydrate(keep.String("id")); err != nil || kept == nil {
		return fmt.Errorf("kept session changed: %v", err)
	}
	live, err := h.sessionIDs()
	if err != nil {
		return err
	}
	for _, id := range gone {
		if containsString(live, id) {
			return fmt.Errorf("deleted session listed: %s in %v", id, live)
		}
	}
	if !containsString(live, keep.String("id")) {
		return fmt.Errorf("kept session missing: %v", live)
	}
	projects, err := h.projectIDs()
	if err != nil {
		return err
	}
	for _, id := range []string{generalID, subID, legID} {
		if containsString(projects, id) {
			return fmt.Errorf("disposable project %s retained: %v", id, projects)
		}
	}
	for _, id := range []string{normalID, extID} {
		if !containsString(projects, id) {
			return fmt.Errorf("ordinary project %s removed: %v", id, projects)
		}
	}
	for _, dir := range []string{h.generalDir, subDir, extDir, h.projDir} {
		if info, err := os.Stat(dir); err != nil || !info.IsDir() {
			return fmt.Errorf("project directory was removed: %s: %v", dir, err)
		}
	}
	if rows, err := h.submissionIDs(t2.String("id")); err != nil || len(rows) != 0 {
		return fmt.Errorf("submission rows survived: %v %v", rows, err)
	}
	tombs, err := h.tombstones()
	if err != nil {
		return err
	}
	for _, key := range []string{"session/" + t1.String("id"), "session/" + t2.String("id"), "session/" + t3.String("id"), "session/" + t4.String("id"), "session/" + t5.String("id"), "session/" + t6.String("id"), "project/" + generalID, "project/" + subID, "project/" + legID} {
		if _, ok := tombs[key]; !ok {
			return fmt.Errorf("tombstone missing for %s: %v", key, tombs)
		}
	}
	for _, key := range []string{"project/" + normalID, "project/" + extID} {
		if _, ok := tombs[key]; ok {
			return fmt.Errorf("ordinary project was tombstoned: %s", key)
		}
	}
	h.notes["deleted-outcome"] = strings.Join(ids, ",")
	h.checks["tree-delete-transaction-gc"] = true
	return nil
}

func (h *harness) deleteFailSuite() error {
	project := h.project(uuid.NewString(), "delfail", h.projDir)
	session := oldTask(h, uuid.NewString(), project.String("id"), "delfail", "ses_dfail")
	if err := h.saveState([]any{project}, session); err != nil {
		return err
	}
	id := session.String("id")
	seed := seedInventory([]map[string]any{pageSeed("", []seedRowWire{rowWire("ses_dfail", "", oldMs())}, "")}, nil)
	seed["delete"] = map[string]any{"ses_dfail": map[string]any{"status": 500}}
	if err := h.writeSeed(seed); err != nil {
		return err
	}
	h.resetCalls()
	if _, err := h.cleanup(id, midSec()); err == nil {
		return errors.New("failed native delete was hidden")
	}
	if calls := h.readCalls(); countMethod(calls, "DELETE") != 1 {
		raw, _ := json.Marshal(calls)
		return fmt.Errorf("failed delete was retried or skipped: %s", raw)
	}
	if current, err := h.hydrate(id); err != nil || current == nil {
		return fmt.Errorf("failed delete changed local: %v", err)
	}
	if tombs, err := h.tombstones(); err != nil {
		return err
	} else if _, ok := tombs["session/"+id]; ok {
		return errors.New("failed delete left a tombstone")
	}
	h.checks["delete-error-no-retry-no-tombstone"] = true
	if err := h.writeSeed(seedInventory([]map[string]any{pageSeed("", []seedRowWire{rowWire("ses_dfail", "", oldMs())}, "")}, nil)); err != nil {
		return err
	}
	recovered, err := h.cleanup(id, midSec())
	if err != nil {
		return fmt.Errorf("cleanup after flag removal: %w", err)
	}
	recoveredIDs, err := deletedIDs(recovered)
	if err != nil {
		return err
	}
	if !equalStrings(recoveredIDs, []string{id}) {
		return fmt.Errorf("recovered delete = %v", recoveredIDs)
	}
	h.checks["delete-retry-after-error-succeeds"] = true
	return nil
}

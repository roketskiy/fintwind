package main

// E2E phases: rejections, fork, rewind, failure injection, restart, store CAS.

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

func (h *harness) rejectionSuite() error {
	assertSilent := func() error {
		if calls := h.readCalls(); len(calls) != 0 {
			raw, _ := json.Marshal(calls)
			return fmt.Errorf("rejected edit reached the native API: %s", raw)
		}
		return nil
	}
	newTurnSession := func(id, title, status string, native string, turns, messages, blocks []backendstore.Object) backendstore.Object {
		session := baseSession(h, id, title, status, native, turns, messages, blocks)
		return session
	}

	busyID, countID, wrongID := uuid.NewString(), uuid.NewString(), uuid.NewString()
	busyTurn, countTurn, wrongTurn := uuid.NewString(), uuid.NewString(), uuid.NewString()
	busy := newTurnSession(busyID, "Busy", "working", "ses_busy",
		[]backendstore.Object{turn(busyTurn, 1, true, "done")},
		[]backendstore.Object{message(uuid.NewString(), busyTurn, "user", "busy"), message(uuid.NewString(), busyTurn, "assistant", "busy answer")},
		[]backendstore.Object{block(uuid.NewString(), busyTurn, 2)})
	counted := newTurnSession(countID, "Counts", "idle", "ses_counts",
		[]backendstore.Object{turn(countTurn, 1, true, "done")},
		[]backendstore.Object{message(uuid.NewString(), countTurn, "user", "counted")},
		[]backendstore.Object{block(uuid.NewString(), countTurn, 1)})
	mismatched := newTurnSession(wrongID, "Mismatch", "idle", "ses_mismatch",
		[]backendstore.Object{turn(wrongTurn, 7, true, "done")},
		[]backendstore.Object{message(uuid.NewString(), wrongTurn, "user", "mismatch")},
		[]backendstore.Object{block(uuid.NewString(), wrongTurn, 1)})
	if err := h.save(busy, counted, mismatched); err != nil {
		return fmt.Errorf("seed rejection sessions: %w", err)
	}
	h.resetCalls()
	if _, err := h.edit(busyID, "forkSessionFromResponse", 1); err == nil {
		return errors.New("busy fork was not rejected")
	} else if !strings.Contains(err.Error(), "stop the task") {
		return fmt.Errorf("busy fork wrong error: %v", err)
	}
	if _, err := h.edit(busyID, "rewindSessionToMessage", 1); err == nil {
		return errors.New("busy rewind was not rejected")
	} else if !strings.Contains(err.Error(), "stop the task") {
		return fmt.Errorf("busy rewind wrong error: %v", err)
	}
	if err := assertSilent(); err != nil {
		return err
	}
	h.checks["reject-busy-no-native-post"] = true

	if _, err := h.edit("00000000-0000-0000-0000-000000000000", "forkSessionFromResponse", 1); err == nil {
		return errors.New("zero identity fork was not rejected")
	} else if !strings.Contains(err.Error(), "task identity required") {
		return fmt.Errorf("zero identity wrong error: %v", err)
	}
	h.checks["reject-zero-identity"] = true

	for _, count := range []int{0, -1, 99} {
		if _, err := h.edit(countID, "rewindSessionToMessage", count); err == nil {
			return fmt.Errorf("turnCount %d was not rejected", count)
		} else if count == 99 && !strings.Contains(err.Error(), "selected turn unavailable") {
			return fmt.Errorf("overflow wrong error: %v", err)
		} else if count != 99 && !strings.Contains(err.Error(), "positive turn count required") {
			return fmt.Errorf("count %d wrong error: %v", count, err)
		}
	}
	if _, err := h.edit(wrongID, "forkSessionFromResponse", 1); err == nil {
		return errors.New("turn_count mismatch was not rejected")
	} else if !strings.Contains(err.Error(), "selected turn unavailable") {
		return fmt.Errorf("mismatch wrong error: %v", err)
	}
	if err := assertSilent(); err != nil {
		return err
	}
	h.checks["reject-invalid-count-no-native-post"] = true

	missingID := uuid.NewString()
	m1, m2 := uuid.NewString(), uuid.NewString()
	missing := newTurnSession(missingID, "Missing", "idle", "ses_missing",
		[]backendstore.Object{turn(m1, 1, true, "done"), turn(m2, 2, true, "done")},
		[]backendstore.Object{message(uuid.NewString(), m1, "user", "first"), message(uuid.NewString(), m1, "assistant", "first answer"), message(uuid.NewString(), m2, "assistant", "orphan")},
		[]backendstore.Object{block(uuid.NewString(), m1, 2), block(uuid.NewString(), m2, 3)})
	if err := h.save(missing); err != nil {
		return fmt.Errorf("seed missing-user session: %w", err)
	}
	if err := failIf(func() error { _, err := h.edit(missingID, "rewindSessionToMessage", 2); return err }(), "selected user message unavailable"); err != nil {
		return err
	}
	if err := assertSilent(); err != nil {
		return err
	}
	h.checks["reject-missing-user-no-native-post"] = true

	providerID := uuid.NewString()
	p1 := uuid.NewString()
	provider := baseSession(h, providerID, "Provider", "idle", "ses_pm",
		[]backendstore.Object{turn(p1, 1, true, "done")},
		[]backendstore.Object{message(uuid.NewString(), p1, "user", "provider"), message(uuid.NewString(), p1, "assistant", "provider answer")},
		[]backendstore.Object{block(uuid.NewString(), p1, 2)})
	provider.Set("provider_cursor", map[string]any{"provider": "other", "sessionId": "ses_pm"})
	if err := h.save(provider); err != nil {
		return fmt.Errorf("seed provider session: %w", err)
	}
	if err := failIf(func() error { _, err := h.edit(providerID, "forkSessionFromResponse", 1); return err }(), "cannot be forked"); err != nil {
		return err
	}
	if err := failIf(func() error { _, err := h.edit(providerID, "rewindSessionToMessage", 1); return err }(), "provider conversation unavailable"); err != nil {
		return err
	}
	if err := assertSilent(); err != nil {
		return err
	}
	h.checks["reject-provider-mismatch-no-native-post"] = true

	unsettledID := uuid.NewString()
	u1 := uuid.NewString()
	unsettled := newTurnSession(unsettledID, "Unsettled", "idle", "ses_unsettled",
		[]backendstore.Object{turn(u1, 1, true, "running")},
		[]backendstore.Object{message(uuid.NewString(), u1, "user", "unsettled")},
		[]backendstore.Object{block(uuid.NewString(), u1, 1)})
	if err := h.save(unsettled); err != nil {
		return fmt.Errorf("seed unsettled session: %w", err)
	}
	record := model.NewSubmissionRecord(unsettledID, u1, "isolated prompt", nil)
	submission, err := json.Marshal(record)
	if err != nil {
		return err
	}
	project := backendstore.Fields(map[string]any{"id": h.projectID, "name": "proj", "path": h.projDir, "created_at": 1})
	if _, err := h.request(unsettledID, map[string]any{"type": "acceptSubmission", "project": project, "session": unsettled, "submission": json.RawMessage(submission)}); err != nil {
		return fmt.Errorf("admit unsettled submission: %w", err)
	}
	if err := failIf(func() error { _, err := h.edit(unsettledID, "forkSessionFromResponse", 1); return err }(), "unsettled"); err != nil {
		return err
	}
	if err := assertSilent(); err != nil {
		return err
	}
	h.checks["reject-unsettled-no-native-post"] = true
	if err := settleSubmission(h.dataDir, u1); err != nil {
		return fmt.Errorf("settle submission: %w", err)
	}
	h.resetCalls()
	forked, err := h.edit(unsettledID, "forkSessionFromResponse", 1)
	if err != nil {
		return fmt.Errorf("settled fork failed: %w", err)
	}
	if forked.String("type") != "sessionForked" {
		return errors.New("settled fork wrong reply type")
	}
	h.notes["settled-fork-native"] = fmt.Sprintf("calls=%d", len(h.readCalls()))
	h.checks["settled-unblocks-edit"] = true
	return nil
}

func sessionIDs(rows []backendstore.Object) []string {
	ids := make([]string, 0, len(rows))
	for _, row := range rows {
		ids = append(ids, row.String("id"))
	}
	return ids
}

func (h *harness) forkSuite() error {
	forkID := uuid.NewString()
	f1, f2, f3 := uuid.NewString(), uuid.NewString(), uuid.NewString()
	messages := []backendstore.Object{}
	blocks := []backendstore.Object{}
	turnIDs := []string{f1, f2, f3}
	for i, turnID := range turnIDs {
		messages = append(messages, message(uuid.NewString(), turnID, "user", fmt.Sprintf("ask %d", i+1)), message(uuid.NewString(), turnID, "assistant", fmt.Sprintf("answer %d", i+1)))
		blocks = append(blocks, block(uuid.NewString(), turnID, 2*(i+1)))
	}
	turns := []backendstore.Object{turn(f1, 1, true, "done"), turn(f2, 2, true, "done"), turn(f3, 3, true, "done")}
	turns[0].Set("checkpoint", map[string]any{"turn_count": 1, "git_ref": "refs/fintwind/session-" + forkID + "-turn-1", "status": "ready"})
	source := baseSession(h, forkID, "Fork me", "idle", "ses_fork_src", turns, messages, blocks)
	source.Set("queued_messages", []any{map[string]any{"id": uuid.NewString(), "content": "queued"}})
	source.Set("runtime_event_cursor", map[string]any{"runtime_id": "r1", "epoch": "e1", "sequence": 7})
	if err := h.save(source); err != nil {
		return fmt.Errorf("seed fork source: %w", err)
	}
	before, err := h.hydrate(forkID)
	if err != nil {
		return err
	}
	beforeTurns := sessionIDs(nestedObjects(before, "turns"))
	h.resetCalls()
	first, err := h.edit(forkID, "forkSessionFromResponse", 2)
	if err != nil {
		return fmt.Errorf("fork from response: %w", err)
	}
	if first.String("type") != "sessionForked" {
		return errors.New("fork wrong reply type")
	}
	h.notes["fork-checkpoint-warning"] = string(first["checkpointWarning"])
	forkSession, err := backendstore.Decode(first["session"])
	if err != nil {
		return err
	}
	forkID1 := forkSession.String("id")
	if forkID1 == "" || forkID1 == forkID {
		return errors.New("fork identity was not regenerated")
	}
	if forkSession.String("title") != "New task" || forkSession.String("auto_title") != "Fork me (2)" {
		return fmt.Errorf("fork title mismatch: %s / %s", forkSession.String("title"), forkSession.String("auto_title"))
	}
	forkTurns := nestedObjects(forkSession, "turns")
	if len(forkTurns) != 2 {
		return errors.New("fork turn window mismatch")
	}
	counts := []string{}
	for _, t := range forkTurns {
		n, _ := t.Int("turn_count")
		counts = append(counts, fmt.Sprintf("%d", n))
		if len(t["submission"]) > 0 && string(t["submission"]) != "null" {
			return errors.New("fork turn kept a submission")
		}
	}
	if strings.Join(counts, ",") != "1,2" {
		return fmt.Errorf("fork kept wrong turns: %s", strings.Join(counts, ","))
	}
	for _, id := range sessionIDs(forkTurns) {
		for _, old := range beforeTurns {
			if id == old {
				return errors.New("fork turn id was not regenerated")
			}
		}
	}
	kept := map[string]bool{}
	for _, t := range forkTurns {
		kept[t.String("id")] = true
	}
	for _, m := range nestedObjects(forkSession, "messages") {
		if !kept[m.String("turn_id")] {
			return errors.New("fork message references a dropped turn")
		}
	}
	for _, b := range nestedObjects(forkSession, "transcript_blocks") {
		if !kept[b.String("turn_id")] {
			return errors.New("fork block references a dropped turn")
		}
	}
	if len(nestedObjects(forkSession, "messages")) != 4 || len(nestedObjects(forkSession, "transcript_blocks")) != 2 {
		return errors.New("fork transcript window mismatch")
	}
	seen := map[string]bool{}
	for _, m := range nestedObjects(forkSession, "messages") {
		if seen[m.String("id")] {
			return errors.New("fork message id collision")
		}
		seen[m.String("id")] = true
	}
	var queued []any
	_ = json.Unmarshal(forkSession["queued_messages"], &queued)
	if len(queued) != 0 || len(forkSession["runtime_event_cursor"]) > 0 {
		return errors.New("fork kept queue or runtime cursor")
	}
	native, err := backendstore.Decode(forkSession["provider_cursor"])
	if err != nil {
		return err
	}
	if native.String("sessionId") == "" || native.String("sessionId") == "ses_fork_src" || native.String("provider") != "openCode" {
		return errors.New("fork native session is not independent")
	}
	ref, err := backendstore.Decode(forkTurns[0]["checkpoint"])
	if err != nil || ref.String("git_ref") != "refs/fintwind/session-"+forkID1+"-turn-1" {
		return errors.New("fork checkpoint ref was not rewritten")
	}
	calls := h.readCalls()
	if countCalls(calls, "POST", "/fork") != 1 {
		return fmt.Errorf("fork posted %d native forks, want exactly one", countCalls(calls, "POST", "/fork"))
	}
	var forkBody struct {
		Before string `json:"before"`
	}
	for _, call := range calls {
		if call.Method == "POST" && strings.HasSuffix(call.Path, "/fork") {
			_ = json.Unmarshal(call.Body, &forkBody)
		}
	}
	if forkBody.Before != "u3" {
		return fmt.Errorf("fork native before boundary = %q, want u3", forkBody.Before)
	}
	after, err := h.hydrate(forkID)
	if err != nil {
		return err
	}
	if !equalStrings(sessionIDs(nestedObjects(after, "turns")), beforeTurns) {
		return errors.New("fork mutated the source task")
	}
	afterCursor, err := backendstore.Decode(after["provider_cursor"])
	if err != nil || afterCursor.String("sessionId") != "ses_fork_src" {
		return errors.New("fork mutated the source cursor")
	}
	if len(nestedObjects(after, "messages")) != 6 {
		return errors.New("fork mutated source messages")
	}
	if users := h.nativeUsers("ses_fork_src"); !equalStrings(users, []string{"u1", "u2", "u3"}) {
		return fmt.Errorf("fork mutated source native history: %v", users)
	}
	if users := h.nativeUsers(native.String("sessionId")); !equalStrings(users, []string{"u1", "u2"}) {
		return fmt.Errorf("fork native window = %v, want [u1 u2]", users)
	}
	if ids, err := h.submissionIDs(forkID1); err != nil || len(ids) != 0 {
		return fmt.Errorf("fork session has submissions: %v %v", ids, err)
	}
	h.checks["fork-window-native-remap-source-intact"] = true

	countBefore, err := h.sessionCount()
	if err != nil {
		return err
	}
	h.resetCalls()
	second, err := h.edit(forkID, "forkSessionFromResponse", 3)
	if err != nil {
		return fmt.Errorf("second fork: %w", err)
	}
	secondSession, err := backendstore.Decode(second["session"])
	if err != nil {
		return err
	}
	if secondSession.String("auto_title") != "Fork me (3)" {
		return fmt.Errorf("second fork title = %q, want Fork me (3)", secondSession.String("auto_title"))
	}
	if secondSession.String("id") == forkID1 {
		return errors.New("second fork reused the first fork identity")
	}
	calls = h.readCalls()
	var fullBody struct {
		Before string `json:"before"`
	}
	found := false
	for _, call := range calls {
		if call.Method == "POST" && strings.HasSuffix(call.Path, "/fork") {
			_ = json.Unmarshal(call.Body, &fullBody)
			found = true
		}
	}
	if !found || fullBody.Before != "" {
		return fmt.Errorf("retain-all fork body = %q, want empty object", fullBody.Before)
	}
	secondNative, _ := backendstore.Decode(secondSession["provider_cursor"])
	if users := h.nativeUsers(secondNative.String("sessionId")); !equalStrings(users, []string{"u1", "u2", "u3"}) {
		return fmt.Errorf("retain-all fork native window = %v", users)
	}
	countAfter, err := h.sessionCount()
	if err != nil {
		return err
	}
	if countAfter != countBefore+1 {
		return fmt.Errorf("session count %d -> %d, want +1", countBefore, countAfter)
	}
	h.checks["fork-title-native-retain-all"] = true
	return nil
}

func (h *harness) rewindSuite() error {
	rewindID := uuid.NewString()
	r1, r2, r3 := uuid.NewString(), uuid.NewString(), uuid.NewString()
	messages := []backendstore.Object{}
	blocks := []backendstore.Object{}
	for i, turnID := range []string{r1, r2, r3} {
		messages = append(messages, message(uuid.NewString(), turnID, "user", fmt.Sprintf("ask %d", i+1)), message(uuid.NewString(), turnID, "assistant", fmt.Sprintf("answer %d", i+1)))
		blocks = append(blocks, block(uuid.NewString(), turnID, 2*(i+1)))
	}
	source := baseSession(h, rewindID, "Rewind", "idle", "ses_rewind",
		[]backendstore.Object{turn(r1, 1, true, "done"), turn(r2, 2, true, "done"), turn(r3, 3, true, "done")},
		messages, blocks)
	source.Set("runtime_event_cursor", map[string]any{"runtime_id": "r9", "epoch": "e9", "sequence": 3})
	if err := h.save(source); err != nil {
		return fmt.Errorf("seed rewind session: %w", err)
	}
	if err := insertSubmission(h.dataDir, rewindID, r1, model.SubmissionAccepted); err != nil {
		return fmt.Errorf("seed kept submission: %w", err)
	}
	if err := insertSubmission(h.dataDir, rewindID, r3, model.SubmissionAccepted); err != nil {
		return fmt.Errorf("seed dropped submission: %w", err)
	}
	staleRaw, _ := json.Marshal(source)
	var stale backendstore.Object
	if err := json.Unmarshal(staleRaw, &stale); err != nil {
		return err
	}
	if err := os.WriteFile(h.callPath("taskedit-stale.json"), staleRaw, 0o600); err != nil {
		return err
	}
	if _, err := h.edit(rewindID, "rewindSessionToMessage", 0); err == nil {
		return errors.New("rewind turnCount 0 was not rejected")
	}
	h.resetCalls()
	rewound, err := h.edit(rewindID, "rewindSessionToMessage", 2)
	if err != nil {
		return fmt.Errorf("rewind: %w", err)
	}
	if rewound.String("type") != "sessionRewound" {
		return errors.New("rewind wrong reply type")
	}
	h.notes["rewind-cleanup-warning"] = string(rewound["cleanupWarning"])
	calls := h.readCalls()
	if countCalls(calls, "DELETE", "/revert") != 1 || countCalls(calls, "POST", "/revert/stage") != 1 || countCalls(calls, "POST", "/revert/commit") != 1 {
		raw, _ := json.Marshal(calls)
		return fmt.Errorf("rewind native sequence mismatch: %s", raw)
	}
	var staged struct {
		MessageID string `json:"messageID"`
	}
	for _, call := range calls {
		if call.Method == "POST" && strings.HasSuffix(call.Path, "/revert/stage") {
			_ = json.Unmarshal(call.Body, &staged)
		}
	}
	if staged.MessageID != "u2" {
		return fmt.Errorf("rewind staged %q, want u2", staged.MessageID)
	}
	session, err := h.hydrate(rewindID)
	if err != nil {
		return err
	}
	turns := nestedObjects(session, "turns")
	if len(turns) != 1 || turns[0].String("id") != r1 {
		return errors.New("rewind kept the wrong turns")
	}
	if len(nestedObjects(session, "messages")) != 2 || len(nestedObjects(session, "transcript_blocks")) != 1 {
		return errors.New("rewind transcript window mismatch")
	}
	if len(session["runtime_event_cursor"]) > 0 {
		return errors.New("rewind kept the runtime cursor")
	}
	ids, err := h.submissionIDs(rewindID)
	if err != nil {
		return err
	}
	if !equalStrings(ids, []string{r1}) {
		return fmt.Errorf("rewind submissions = %v, want only the kept turn", ids)
	}
	removed, err := h.removedTurnIDs(rewindID)
	if err != nil {
		return err
	}
	has := map[string]bool{}
	for _, id := range removed {
		has[id] = true
	}
	if !has[r2] || !has[r3] {
		return fmt.Errorf("removed-turn fence = %v, want r2 and r3", removed)
	}
	if users := h.nativeUsers("ses_rewind"); !equalStrings(users, []string{"u1"}) {
		return fmt.Errorf("rewind native users = %v, want [u1]", users)
	}
	h.checks["rewind-window-native-fence-submissions"] = true

	if err := h.save(stale); err == nil {
		return errors.New("stale UI save resurrected removed turns")
	} else if !strings.Contains(err.Error(), "reload") {
		return fmt.Errorf("stale save wrong error: %v", err)
	}
	h.checks["stale-ui-save-refused"] = true

	fresh, err := h.hydrate(rewindID)
	if err != nil {
		return err
	}
	r4 := uuid.NewString()
	turnRows := nestedObjects(fresh, "turns")
	turnRows = append(turnRows, turn(r4, 2, false, "done"))
	fresh.Set("turns", turnRows)
	messageRows := nestedObjects(fresh, "messages")
	messageRows = append(messageRows, message(uuid.NewString(), r4, "user", "future"))
	fresh.Set("messages", messageRows)
	fresh.Set("updated_at", now())
	if err := h.save(fresh); err != nil {
		return fmt.Errorf("future UUID save: %w", err)
	}
	confirmed, err := h.hydrate(rewindID)
	if err != nil {
		return err
	}
	if len(nestedObjects(confirmed, "turns")) != 2 {
		return errors.New("future UUID save did not persist")
	}
	h.checks["future-uuid-save-accepted"] = true
	return nil
}

func (h *harness) failureInjectionSuite() error {
	failID := uuid.NewString()
	s1, s2 := uuid.NewString(), uuid.NewString()
	fail := baseSession(h, failID, "Fail", "idle", "ses_rfail",
		[]backendstore.Object{turn(s1, 1, true, "done"), turn(s2, 2, true, "done")},
		[]backendstore.Object{message(uuid.NewString(), s1, "user", "one"), message(uuid.NewString(), s1, "assistant", "one answer"), message(uuid.NewString(), s2, "user", "two"), message(uuid.NewString(), s2, "assistant", "two answer")},
		[]backendstore.Object{block(uuid.NewString(), s1, 2), block(uuid.NewString(), s2, 4)})
	if err := h.save(fail); err != nil {
		return fmt.Errorf("seed failure session: %w", err)
	}
	if err := os.WriteFile(h.callPath("taskedit-fail-commit"), []byte("fail"), 0o600); err != nil {
		return err
	}
	h.resetCalls()
	if _, err := h.edit(failID, "rewindSessionToMessage", 1); err == nil {
		return errors.New("failed native revert was hidden")
	}
	calls := h.readCalls()
	if countCalls(calls, "POST", "/revert/commit") != 1 || countCalls(calls, "POST", "/revert/stage") != 1 {
		raw, _ := json.Marshal(calls)
		return fmt.Errorf("failed revert was retried or skipped: %s", raw)
	}
	session, err := h.hydrate(failID)
	if err != nil {
		return err
	}
	if len(nestedObjects(session, "turns")) != 2 {
		return errors.New("failed revert mutated local history")
	}
	if users := h.nativeUsers("ses_rfail"); !equalStrings(users, []string{"u1", "u2", "u3"}) {
		return fmt.Errorf("failed revert mutated native history: %v", users)
	}
	h.checks["revert-error-no-retry-local-intact"] = true
	if err := os.Remove(h.callPath("taskedit-fail-commit")); err != nil {
		return err
	}
	h.resetCalls()
	if _, err := h.edit(failID, "rewindSessionToMessage", 1); err != nil {
		return fmt.Errorf("rewind after flag removal: %w", err)
	}
	cleared, err := h.hydrate(failID)
	if err != nil {
		return err
	}
	if len(nestedObjects(cleared, "turns")) != 0 || len(nestedObjects(cleared, "messages")) != 0 {
		return errors.New("retain-zero rewind did not clear the transcript")
	}
	if users := h.nativeUsers("ses_rfail"); len(users) != 0 {
		return fmt.Errorf("retain-zero native users = %v", users)
	}
	h.checks["rewind-retain-zero-clears"] = true

	before, err := h.sessionCount()
	if err != nil {
		return err
	}
	if err := h.forkFailureProbe(); err != nil {
		return err
	}
	after, err := h.sessionCount()
	if err != nil {
		return err
	}
	if after != before {
		return errors.New("failed fork created a session")
	}
	if err := os.Remove(h.callPath("taskedit-fail-fork")); err != nil {
		return err
	}
	h.checks["fork-error-no-retry-no-session"] = true
	return nil
}

func (h *harness) forkFailureProbe() error {
	if err := os.WriteFile(h.callPath("taskedit-fail-fork"), []byte("fail"), 0o600); err != nil {
		return err
	}
	h.resetCalls()
	var target string
	response, err := h.request(uuid.Nil.String(), map[string]any{"type": "loadTaskState"})
	if err != nil {
		return err
	}
	var listed []struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(response["sessions"], &listed); err != nil {
		return err
	}
	for _, session := range listed {
		full, err := h.hydrate(session.ID)
		if err != nil {
			continue
		}
		if full.String("title") == "Fork me" && len(nestedObjects(full, "turns")) == 3 {
			target = session.ID
			break
		}
	}
	if target == "" {
		return errors.New("fork source missing for failure probe")
	}
	if _, err := h.edit(target, "forkSessionFromResponse", 1); err == nil {
		return errors.New("failed native fork was hidden")
	}
	if calls := h.readCalls(); countCalls(calls, "POST", "/fork") != 1 {
		raw, _ := json.Marshal(calls)
		return fmt.Errorf("failed fork was retried or skipped: %s", raw)
	}
	return nil
}

func (h *harness) restartSuite() error {
	if err := h.closeDaemon(); err != nil {
		return err
	}
	if err := h.startDaemon("restart"); err != nil {
		return err
	}
	var rewindID string
	response, err := h.request(uuid.Nil.String(), map[string]any{"type": "loadTaskState"})
	if err != nil {
		return err
	}
	var listed []struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(response["sessions"], &listed); err != nil {
		return err
	}
	for _, session := range listed {
		full, err := h.hydrate(session.ID)
		if err != nil {
			continue
		}
		if full.String("title") == "Rewind" {
			rewindID = session.ID
			if len(nestedObjects(full, "turns")) != 2 {
				return errors.New("restart lost rewound history")
			}
		}
	}
	if rewindID == "" {
		return errors.New("rewound session missing after restart")
	}
	h.checks["restart-durable"] = true
	staleRaw, err := os.ReadFile(h.callPath("taskedit-stale.json"))
	if err != nil {
		return err
	}
	var stale backendstore.Object
	if err := json.Unmarshal(staleRaw, &stale); err != nil {
		return err
	}
	staleProject := backendstore.Fields(map[string]any{"id": stale.String("project_id"), "name": "proj", "path": h.projDir, "created_at": 1})
	_, err = h.client.Request(h.ctx, map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{staleProject}, "sessions": []any{json.RawMessage(staleRaw)}, "skeletonSessionIds": []any{}})
	if err == nil {
		return errors.New("stale save resurrected history after restart")
	} else if !strings.Contains(err.Error(), "reload") {
		return fmt.Errorf("post-restart stale save wrong error: %v", err)
	}
	_ = stale
	h.checks["fence-survives-restart"] = true
	fresh, err := h.hydrate(rewindID)
	if err != nil {
		return err
	}
	r5 := uuid.NewString()
	turnRows := append(nestedObjects(fresh, "turns"), turn(r5, 3, false, "done"))
	fresh.Set("turns", turnRows)
	fresh.Set("messages", append(nestedObjects(fresh, "messages"), message(uuid.NewString(), r5, "user", "post-restart")))
	fresh.Set("updated_at", now())
	if err := h.save(fresh); err != nil {
		return fmt.Errorf("post-restart save: %w", err)
	}
	h.checks["post-restart-save-accepted"] = true
	return nil
}

func (h *harness) storeCASSuite() error {
	dir := h.round + "-casstore"
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	store, err := backendstore.Open(h.ctx, dir)
	if err != nil {
		return fmt.Errorf("open cas store: %w", err)
	}
	defer store.Close()
	project := backendstore.Fields(map[string]any{"id": uuid.NewString(), "name": "cas", "path": dir, "created_at": 1})
	sessionID := uuid.NewString()
	c1, c2 := uuid.NewString(), uuid.NewString()
	mkSession := func(updated int64, extra []backendstore.Object) backendstore.Object {
		messages := []backendstore.Object{message(uuid.NewString(), c1, "user", "cas one"), message(uuid.NewString(), c2, "user", "cas two")}
		messages = append(messages, extra...)
		return backendstore.Fields(map[string]any{
			"id": sessionID, "project_id": project.String("id"), "title": "CAS", "provider": "opencode", "status": "idle",
			"created_at": 1, "updated_at": updated, "last_reply_at": 0, "detail_loaded": true,
			"messages": messages, "turns": []backendstore.Object{turn(c1, 1, true, "done"), turn(c2, 2, true, "done")},
			"transcript_blocks": []backendstore.Object{}, "queued_messages": []any{},
			"provider_cursor": cursor("ses_cas"), "native_session_id": "ses_cas",
		})
	}
	saveCmd := func(session backendstore.Object) backendstore.Object {
		return backendstore.Fields(map[string]any{"projects": []any{project}, "sessions": []any{session}})
	}
	first := mkSession(2, nil)
	if _, err := store.Save(h.ctx, saveCmd(first)); err != nil {
		return fmt.Errorf("cas seed save: %w", err)
	}
	snap, fingerprint, err := store.TaskEditSnapshot(h.ctx, sessionID)
	if err != nil {
		return fmt.Errorf("cas snapshot: %w", err)
	}
	if _, err := store.Save(h.ctx, saveCmd(mkSession(3, []backendstore.Object{message(uuid.NewString(), c2, "assistant", "late")}))); err != nil {
		return fmt.Errorf("cas concurrent save: %w", err)
	}
	if err := store.CommitTaskEdit(h.ctx, sessionID, fingerprint, snap, false); err == nil {
		return errors.New("stale CAS commit was accepted")
	} else if !strings.Contains(err.Error(), "changed during history edit") {
		return fmt.Errorf("CAS conflict wrong error: %v", err)
	}
	h.checks["cas-conflict-sqlite"] = true

	live, liveFingerprint, err := store.TaskEditSnapshot(h.ctx, sessionID)
	if err != nil {
		return err
	}
	liveTurns := nestedObjects(live, "turns")
	target := live
	target.Set("turns", liveTurns[:1])
	kept := map[string]bool{}
	for _, t := range liveTurns[:1] {
		kept[t.String("id")] = true
	}
	keptMessages := []backendstore.Object{}
	for _, m := range nestedObjects(live, "messages") {
		if kept[m.String("turn_id")] {
			keptMessages = append(keptMessages, m)
		}
	}
	target.Set("messages", keptMessages)
	target.Set("status", "idle")
	delete(target, "runtime_event_cursor")
	if err := store.CommitTaskEdit(h.ctx, sessionID, liveFingerprint, target, false); err != nil {
		return fmt.Errorf("cas rewind commit: %w", err)
	}
	if err := store.SaveRuntime(h.ctx, snap); err == nil {
		return errors.New("stale runtime save resurrected history")
	} else if !strings.Contains(err.Error(), "reload") {
		return fmt.Errorf("stale runtime save wrong error: %v", err)
	}
	h.checks["saveruntime-refuses-stale"] = true
	payload, err := store.Hydrate(h.ctx, sessionID)
	if err != nil {
		return err
	}
	current, err := backendstore.Decode(payload["session"])
	if err != nil {
		return err
	}
	if err := store.SaveRuntime(h.ctx, current); err != nil {
		return fmt.Errorf("fresh runtime save: %w", err)
	}
	h.checks["saveruntime-accepts-fresh"] = true
	return nil
}

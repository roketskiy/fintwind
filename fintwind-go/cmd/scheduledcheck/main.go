package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"time"

	sched "fintwind-go/internal/backendscheduled"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}
func run() (err error) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	defer func() {
		report := map[string]any{"checks": checks, "passed": err == nil}
		if err != nil {
			report["error"] = err.Error()
		}
		raw, e := json.MarshalIndent(report, "", "  ")
		if e == nil {
			e = os.WriteFile(filepath.Join(round, "scheduled-result.json"), raw, 0o600)
		}
		err = errors.Join(err, e)
		fmt.Println("[REPORT]", filepath.Join(round, "scheduled-result.json"))
	}()
	data := filepath.Join(round, "data")
	if err := os.Mkdir(data, 0o700); err != nil {
		return err
	}
	store, err := backendstore.Open(ctx, data)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, store.Close()) }()
	now := time.Now().Unix()
	session, runtime, owner := uuid.NewString(), uuid.NewString(), uuid.NewString()
	target, _ := json.Marshal(map[string]string{"kind": "existingSession", "sessionId": session})
	draft := sched.Draft{Title: "Literal task", Prompt: "Literal prompt", Enabled: true, Schedule: sched.Schedule{Kind: "every", IntervalSecs: 60}, Target: target}
	task, err := store.SaveScheduled(ctx, sched.Edit{Kind: "create", Draft: draft}, now)
	if err != nil {
		return err
	}
	if _, err := store.SaveScheduled(ctx, sched.Edit{Kind: "update", ID: task.ID, ExpectedRevision: 1, Draft: draft}, now); err == nil {
		return errors.New("stale update accepted")
	}
	if err := store.DeleteScheduled(ctx, task.ID, 1); err == nil {
		return errors.New("stale delete accepted")
	}
	checks["stale-cas-refused"] = true
	var wg sync.WaitGroup
	var mu sync.Mutex
	claimed := []sched.Occurrence{}
	for i := 0; i < 12; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			o, e := store.ClaimScheduled(ctx, task.ID, owner, now)
			if e == nil {
				mu.Lock()
				claimed = append(claimed, o)
				mu.Unlock()
			}
		}()
	}
	wg.Wait()
	if len(claimed) != 1 {
		return errors.New("concurrent claims did not produce exactly one occurrence")
	}
	checks["concurrent-claim-single-owner"] = true
	o := claimed[0]
	submissionID := uuid.NewString()
	submission, _ := json.Marshal(map[string]any{"session_id": session, "receipt": map[string]string{"id": submissionID}})
	if err := store.PrepareScheduled(ctx, o.ID, uuid.NewString(), runtime, submission); err == nil {
		return errors.New("foreign session preparation accepted")
	}
	checks["foreign-submission-refused"] = true
	if err := store.PrepareScheduled(ctx, o.ID, session, runtime, submission); err != nil {
		return err
	}
	if err := store.PrepareScheduled(ctx, o.ID, session, runtime, submission); err != nil {
		return err
	}
	checks["preparation-idempotent"] = true
	if err := store.Close(); err != nil {
		return err
	}
	store, err = backendstore.Open(ctx, data)
	if err != nil {
		return err
	}
	open, err := store.OpenScheduled(ctx)
	if err != nil {
		return err
	}
	if len(open) != 1 || open[0].ID != o.ID || open[0].State != "preparing" || string(open[0].Submission) != string(submission) {
		return errors.New("preparation not durable")
	}
	checks["restart-preparation-durable"] = true
	outcome := sched.Outcome{Kind: "unconfirmed", SessionID: &session, SubmissionID: &submissionID}
	if err := store.SettleScheduled(ctx, o.ID, outcome, now+1); err != nil {
		return err
	}
	if err := store.SettleScheduled(ctx, o.ID, outcome, now+2); err != nil {
		return err
	}
	list, err := store.ListScheduled(ctx)
	if err != nil {
		return err
	}
	if len(list) != 1 || list[0].RunCount != 1 || list[0].Pending != nil || list[0].NextDueAt == nil || *list[0].NextDueAt != now+60 {
		return errors.New("manual settlement changed due or repeated count")
	}
	checks["settle-idempotent-manual-due-preserved"] = true
	due, err := store.ClaimDueScheduled(ctx, owner, now+61, 4)
	if err != nil {
		return err
	}
	if len(due) != 1 || due[0].Manual {
		return errors.New("due occurrence not claimed")
	}
	draft.Enabled = false
	task, err = store.SaveScheduled(ctx, sched.Edit{Kind: "update", ID: task.ID, ExpectedRevision: 0, Draft: draft}, now+62)
	if err != nil {
		return err
	}
	open, err = store.OpenScheduled(ctx)
	if err != nil {
		return err
	}
	if len(open) != 0 {
		return errors.New("disabled task retained queued occurrence")
	}
	checks["disable-cancels-queued"] = true
	if err := store.DeleteScheduled(ctx, task.ID, task.Revision); err != nil {
		return err
	}
	list, err = store.ListScheduled(ctx)
	if err != nil {
		return err
	}
	if len(list) != 0 {
		return errors.New("deleted task still exists")
	}
	checks["delete-durable"] = true
	draft.Enabled = true
	draft.Schedule = sched.Schedule{Kind: "once", At: now + 1}
	task, err = store.SaveScheduled(ctx, sched.Edit{Kind: "create", Draft: draft}, now)
	if err != nil {
		return err
	}
	due, err = store.ClaimDueScheduled(ctx, owner, now+602, 4)
	if err != nil {
		return err
	}
	if len(due) != 0 {
		return errors.New("missed fixed occurrence dispatched")
	}
	list, err = store.ListScheduled(ctx)
	if err != nil {
		return err
	}
	if len(list) != 1 || list[0].Enabled || list[0].RunCount != 0 || list[0].Last == nil || list[0].Last.Outcome.Kind != "missed" {
		return errors.New("missed once not disabled")
	}
	checks["missed-once-disabled-without-dispatch"] = true
	for _, scenario := range []struct {
		name        string
		schedule    sched.Schedule
		after, want string
	}{
		{"dst-gap-forward", sched.Schedule{Kind: "atTime", At: 150, Weekdays: 127, Timezone: "America/New_York"}, "2026-03-08T05:00:00Z", "2026-03-08T07:00:00Z"},
		{"dst-fold-earliest", sched.Schedule{Kind: "atTime", At: 90, Weekdays: 127, Timezone: "America/New_York"}, "2026-11-01T04:00:00Z", "2026-11-01T05:30:00Z"},
		{"dst-fold-not-twice", sched.Schedule{Kind: "atTime", At: 90, Weekdays: 127, Timezone: "America/New_York"}, "2026-11-01T05:45:00Z", "2026-11-02T06:30:00Z"},
		{"weekdays-monday-bit", sched.Schedule{Kind: "atTime", At: 0, Weekdays: 2, Timezone: "UTC"}, "2026-11-03T00:00:00Z", "2026-11-09T00:00:00Z"},
		{"weekdays-sunday-bit", sched.Schedule{Kind: "atTime", At: 0, Weekdays: 1, Timezone: "UTC"}, "2026-11-03T00:00:00Z", "2026-11-08T00:00:00Z"},
	} {
		after, _ := time.Parse(time.RFC3339, scenario.after)
		want, _ := time.Parse(time.RFC3339, scenario.want)
		got, err := scenario.schedule.Next(after.Unix())
		if err != nil {
			return err
		}
		if got != want.Unix() {
			return fmt.Errorf("%s: got %s", scenario.name, time.Unix(got, 0).UTC())
		}
		checks[scenario.name] = true
	}
	return nil
}

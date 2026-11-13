package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"

	"github.com/google/uuid"
)

type eventReader struct {
	client  *daemon.Client
	seen    []daemon.Event
	claimed []bool
}

type workView struct {
	Type              string   `json:"type"`
	Key               workKey  `json:"key"`
	Title             string   `json:"title"`
	Command           *string  `json:"command"`
	Cwd               *string  `json:"cwd"`
	Output            *string  `json:"output"`
	OutputTruncated   bool     `json:"outputTruncated"`
	Background        bool     `json:"background"`
	CanStop           bool     `json:"canStop"`
	ControlID         *string  `json:"controlId"`
	OriginActivityIDs []string `json:"originActivityIds"`
	ParentID          *string  `json:"parentId"`
	Status            string   `json:"status"`
	ExitCode          *int32   `json:"exitCode"`
	Delta             string   `json:"delta"`
	Message           string   `json:"message"`
}

type workKey struct {
	Kind       string `json:"kind"`
	ProviderID string `json:"providerId"`
}

type fixtureReady struct {
	Address string `json:"address"`
	Token   string `json:"token"`
}

func (r *eventReader) next(ctx context.Context) (daemon.Event, error) {
	select {
	case <-ctx.Done():
		return daemon.Event{}, ctx.Err()
	case event, ok := <-r.client.Events():
		if !ok {
			return daemon.Event{}, errors.New("daemon event stream closed")
		}
		if event.Type == "event" {
			r.client.Ack(event)
		}
		r.seen = append(r.seen, event)
		r.claimed = append(r.claimed, false)
		return event, nil
	}
}

func (r *eventReader) wait(ctx context.Context, label string, accept func(daemon.Event) bool) (daemon.Event, error) {
	for {
		for i, event := range r.seen {
			if i < len(r.claimed) && !r.claimed[i] && accept(event) {
				r.claimed[i] = true
				return event, nil
			}
		}
		event, err := r.next(ctx)
		if err != nil {
			return daemon.Event{}, fmt.Errorf("wait %s: %w", label, err)
		}
		if accept(event) {
			r.claimed[len(r.claimed)-1] = true
			return event, nil
		}
	}
}

func runShellFlow(ctx context.Context, daemonBinary string, inst *round, client *daemon.Client, state *reportState) error {
	sid, rid, pid := uuid.NewString(), uuid.NewString(), uuid.NewString()
	options := map[string]any{"binary": state.Self, "cwd": state.Workspace, "mode": "ask", "interactionMode": "code", "model": "fixture/shellwork"}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": options}); err != nil {
		return fmt.Errorf("start root runtime: %w", err)
	}
	reader := &eventReader{client: client}
	connected, err := reader.wait(ctx, "root runtime connected", func(ev daemon.Event) bool { return ev.Kind == "connected" })
	if err != nil {
		return err
	}
	var connectedData struct {
		SessionID string `json:"sessionId"`
	}
	_ = json.Unmarshal(connected.Payload, &connectedData)
	if connectedData.SessionID != nativeRootSession {
		return fmt.Errorf("root native session=%q, want %q", connectedData.SessionID, nativeRootSession)
	}
	admin, err := waitFixtureAdmin(ctx, inst.root)
	if err != nil {
		return err
	}
	if err := configureShellFixtures(ctx, admin, state); err != nil {
		return err
	}

	turn := uuid.NewString()
	if err := admitSession(ctx, client, sid, pid, state.Workspace, turn, "shellwork-suite", 1, nil, uint64(time.Now().Unix())); err != nil {
		return fmt.Errorf("admit shell-work submission: %w", err)
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "dispatchSubmission", "submissionId": turn, "workspace": map[string]string{"kind": "local"}}); err != nil {
		return fmt.Errorf("dispatch shell-work submission: %w", err)
	}
	if _, err := reader.wait(ctx, "shell-work turn finished", func(ev daemon.Event) bool { return ev.Kind == "turnFinished" }); err != nil {
		return err
	}
	note(state, "fixture-root-turn-and-process-events", nil)
	for _, id := range []string{"poll-shell", "foreign-shell", "delete-shell", "missing-shell", "malformed-shell", "row500-shell", "root-stop-shell", "failed-stop-shell", "complete-shell", "tail-shell", "child-stop-shell"} {
		if _, err := lastWork(reader.seen, id); err != nil {
			return fmt.Errorf("process %s was not established: %w", id, err)
		}
	}
	if hasWorkOrigin(reader.seen, "activity-foreground") {
		return errors.New("ordinary foreground tool created a shell process")
	}
	if _, err := lastWork(reader.seen, "unknown-shell"); err == nil {
		return errors.New("global shell.created discovered an unknown process")
	}
	if err := verifyInitialWork(reader.seen, state.Workspace); err != nil {
		return err
	}
	note(state, "no-foreground-or-unknown-shell-discovery", nil)

	// The first poll intentionally blocks in the fake output endpoint. A normal
	// provider SSE event must still reach the root actor well before the HTTP
	// request returns.
	startedAt := time.Now()
	if err := waitFixture(ctx, admin, func(s shellFixtureStore) bool { return s.SlowStarted["poll-shell"] }, "slow shell output start"); err != nil {
		return err
	}
	fastCtx, fastCancel := context.WithTimeout(ctx, 1800*time.Millisecond)
	_, err = reader.wait(fastCtx, "root SSE while shell poll is blocked", func(ev daemon.Event) bool { return ev.Kind == "providerRetry" })
	fastCancel()
	if err != nil {
		return fmt.Errorf("root SSE stalled behind shell poll: %w", err)
	}
	if time.Since(startedAt) >= 1800*time.Millisecond {
		return fmt.Errorf("root SSE delivery took %s while fake output waits 2800ms", time.Since(startedAt))
	}
	note(state, "slow-poll-does-not-block-root-sse", nil)
	if _, err := reader.wait(ctx, "first shell output page", func(ev daemon.Event) bool { return isOutputDelta(ev, "poll-shell", "alpha-") }); err != nil {
		return err
	}

	// A foreign owner row is still a tracked process identity, but neither its
	// output nor a stop request may cross the ownership boundary.
	if err := waitFixture(ctx, admin, func(s shellFixtureStore) bool { return s.RowCalls["foreign-shell"] > 0 }, "foreign shell poll"); err != nil {
		return err
	}
	if err := expectStopRejected(ctx, client, sid, rid, "foreign-shell", "foreign-shell", "process"); err != nil {
		return err
	}
	if err := waitFixture(ctx, admin, func(s shellFixtureStore) bool {
		return !contains(s.DeleteRequests, "foreign-shell") && !hasOutputRequest(s.OutputRequests, "foreign-shell")
	}, "foreign owner checked before output/DELETE"); err != nil {
		return err
	}
	if containsOutput(reader.seen, "FOREIGN-OUTPUT-MUST-NOT-EMIT") {
		return errors.New("foreign shell output reached daemon events")
	}
	note(state, "foreign-owner-output-and-stop-rejected", nil)

	// The page at cursor 6 fails once and remains held until the terminal event
	// arrives. The retry must use cursor 6 again; its tail is then delivered even
	// though the old row still claims running.
	if err := waitFixture(ctx, admin, func(s shellFixtureStore) bool {
		return hasRequest(s.OutputRequests, "poll-shell", 6, http.StatusInternalServerError)
	}, "cursor-6 HTTP failure"); err != nil {
		return err
	}
	if err := waitFixture(ctx, admin, func(s shellFixtureStore) bool {
		return s.RowCalls["malformed-shell"] > 0 && s.RowCalls["row500-shell"] > 0
	}, "malformed/500 row polls"); err != nil {
		return err
	}
	if err := assertStatus(reader.seen, "malformed-shell", "running"); err != nil {
		return err
	}
	if err := assertStatus(reader.seen, "row500-shell", "running"); err != nil {
		return err
	}
	if _, err := admin.control(ctx, map[string]any{"type": "emit", "kind": "shell.exited", "data": map[string]any{"id": "poll-shell", "exit": 7, "sessionID": "ses_unrelated"}}); err != nil {
		return err
	}
	if _, err := reader.wait(ctx, "poll-shell failed exit", func(ev daemon.Event) bool { return isWorkStatus(ev, "poll-shell", "failed") }); err != nil {
		return err
	}
	if err := mutateShell(ctx, admin, "poll-shell", func(f *shellFixture) {
		f.Status = "running"
		f.Exit = 0
		f.SlowCursor = ""
		f.FailOutputRemaining = 0
		f.HoldOutput = false
	}); err != nil {
		return err
	}
	if _, err := reader.wait(ctx, "tail output after exit", func(ev daemon.Event) bool { return isOutputDelta(ev, "poll-shell", "omega") }); err != nil {
		return err
	}
	if err := assertStatus(reader.seen, "poll-shell", "failed"); err != nil {
		return fmt.Errorf("stale running row revived terminal shell: %w", err)
	}
	if err := waitFixture(ctx, admin, func(s shellFixtureStore) bool { return hasCursorRetry(s.OutputRequests, "poll-shell", 6, 200) }, "cursor retry after 500"); err != nil {
		return err
	}
	if err := mutateShell(ctx, admin, "poll-shell", func(f *shellFixture) { f.Status = "failed"; f.Exit = 7 }); err != nil {
		return err
	}
	if _, err := reader.wait(ctx, "poll-shell row settles failed", func(ev daemon.Event) bool { return isWorkStatus(ev, "poll-shell", "failed") }); err != nil {
		return err
	}
	note(state, "output-cursor-retry-tail-exit-and-no-resurrection", nil)

	if _, err := admin.control(ctx, map[string]any{"type": "emit", "kind": "shell.deleted", "data": map[string]any{"id": "delete-shell", "sessionID": "ses_unrelated"}}); err != nil {
		return err
	}
	if _, err := waitWorkStatus(ctx, reader, admin, "delete-shell", "lost"); err != nil {
		return err
	}
	if _, err := waitWorkStatus(ctx, reader, admin, "missing-shell", "lost"); err != nil {
		return err
	}
	if _, err := reader.wait(ctx, "completed shell row", func(ev daemon.Event) bool { return isWorkStatus(ev, "complete-shell", "completed") }); err != nil {
		return err
	}
	if _, err := reader.wait(ctx, "bounded tail shell row", func(ev daemon.Event) bool { return isWorkStatus(ev, "tail-shell", "completed") }); err != nil {
		return err
	}
	if err := verifyTail(reader.seen); err != nil {
		return err
	}
	note(state, "exit-delete-404-and-bounded-output-status", nil)

	if err := expectStopRejected(ctx, client, sid, rid, "unknown-process", "unknown-process", "process"); err != nil {
		return err
	}
	if err := expectStopRejected(ctx, client, sid, rid, "root-stop-shell", "not-root-stop-shell", "process"); err != nil {
		return err
	}
	if err := expectStopRejected(ctx, client, sid, rid, "root-stop-shell", "root-stop-shell", "subagent"); err != nil {
		return err
	}
	if err := waitFixture(ctx, admin, func(s shellFixtureStore) bool { return len(s.DeleteRequests) == 0 }, "invalid controls issue no DELETE"); err != nil {
		return err
	}
	if err := mutateShell(ctx, admin, "failed-stop-shell", func(f *shellFixture) { f.DeleteStatusCode = http.StatusInternalServerError }); err != nil {
		return err
	}
	if err := expectStopRejected(ctx, client, sid, rid, "failed-stop-shell", "failed-stop-shell", "process"); err != nil {
		return err
	}
	if _, err := reader.wait(ctx, "stop failure event", func(ev daemon.Event) bool { return isStopFailed(ev, "failed-stop-shell") }); err != nil {
		return err
	}
	if err := assertStatus(reader.seen, "failed-stop-shell", "running"); err != nil {
		return fmt.Errorf("failed stop changed process state: %w", err)
	}
	if err := mutateShell(ctx, admin, "failed-stop-shell", func(f *shellFixture) { f.DeleteStatusCode = 0 }); err != nil {
		return err
	}
	for _, id := range []string{"root-stop-shell", "failed-stop-shell", "child-stop-shell"} {
		if err := stopWork(ctx, client, sid, rid, id, id); err != nil {
			return fmt.Errorf("stop owned process %s: %w", id, err)
		}
		if _, err := reader.wait(ctx, "process "+id+" stopped", func(ev daemon.Event) bool { return isWorkStatus(ev, id, "stopped") }); err != nil {
			return err
		}
	}
	note(state, "stop-identity-owner-child-and-failure", nil)

	// Error rows are explicitly repaired by the fixture only after verifying
	// that 500/malformed responses did not clear them. This lets restart coverage
	// run with an all-terminal registry instead of hiding the failure.
	for _, id := range []string{"malformed-shell", "row500-shell"} {
		if err := mutateShell(ctx, admin, id, func(f *shellFixture) {
			f.Status = "completed"
			f.Exit = 0
			f.RowFailureRemaining = 0
			f.MalformedRowRemaining = 0
			f.RowStatusCode = 0
		}); err != nil {
			return err
		}
		if _, err := reader.wait(ctx, "fixture terminalize "+id, func(ev daemon.Event) bool { return isWorkStatus(ev, id, "completed") }); err != nil {
			return err
		}
	}
	if err := mutateShell(ctx, admin, "foreign-shell", func(f *shellFixture) { f.Missing = true }); err != nil {
		return err
	}
	if _, err := reader.wait(ctx, "foreign fixture terminalized after owner test", func(ev daemon.Event) bool { return isWorkStatus(ev, "foreign-shell", "lost") }); err != nil {
		return err
	}
	note(state, "http-error-and-malformed-row-preserve-work", nil)

	// A second runtime cannot reuse the root runtime's process control identity.
	beforeForeignState, err := admin.state(ctx)
	if err != nil {
		return err
	}
	rootDeletesBefore := countID(beforeForeignState.DeleteRequests, "root-stop-shell")
	otherSID, otherRID := uuid.NewString(), uuid.NewString()
	if _, err := client.RequestFor(ctx, otherSID, otherRID, map[string]any{"type": "start", "options": options}); err != nil {
		return fmt.Errorf("start foreign runtime: %w", err)
	}
	if _, err := reader.wait(ctx, "second runtime connected", func(ev daemon.Event) bool { return ev.Kind == "connected" }); err != nil {
		return err
	}
	if err := expectStopRejected(ctx, client, otherSID, otherRID, "root-stop-shell", "root-stop-shell", "process"); err != nil {
		return err
	}
	otherAdmin, err := waitFixtureAdmin(ctx, inst.root)
	if err != nil {
		return err
	}
	otherState, err := otherAdmin.state(ctx)
	if err != nil {
		return err
	}
	if countID(otherState.DeleteRequests, "root-stop-shell") != rootDeletesBefore {
		return errors.New("foreign runtime issued DELETE for root-owned shell")
	}
	_, _ = client.RequestFor(ctx, otherSID, otherRID, map[string]any{"type": "closeSession"})
	note(state, "other-runtime-cannot-control-root-process", nil)

	// Persist and inspect both public work and the private cursor before restart.
	snapshot, err := hydrateShellSession(ctx, client, sid)
	if err != nil {
		return err
	}
	state.Counts["backgroundWorkBeforeShutdown"] = summarizeBackgroundWork(snapshot)
	state.Counts["foreignShellEntriesBeforeShutdown"] = matchingWorkEntries(state.Counts["backgroundWorkBeforeShutdown"], "foreign-shell")
	state.Counts["foreignShellEntryCountBeforeShutdown"] = len(matchingWorkEntries(state.Counts["backgroundWorkBeforeShutdown"], "foreign-shell"))
	state.Counts["foreignShellBackgroundEventsBeforeShutdown"] = summarizeBackgroundEvents(reader.seen, "foreign-shell")
	beforeRestartState, err := admin.state(ctx)
	if err != nil {
		return err
	}
	if _, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "closeSession"}); err != nil {
		return err
	}
	shutdownSnapshot, err := hydrateShellSession(ctx, client, sid)
	if err != nil {
		return fmt.Errorf("hydrate after closeSession before daemon shutdown: %w", err)
	}
	state.Counts["backgroundWorkAfterCloseSessionBeforeRestart"] = summarizeBackgroundWork(shutdownSnapshot)
	state.Counts["foreignShellEntriesAfterCloseSessionBeforeRestart"] = matchingWorkEntries(state.Counts["backgroundWorkAfterCloseSessionBeforeRestart"], "foreign-shell")
	shutdownSnapshotErr := verifySnapshot(shutdownSnapshot)
	if shutdownSnapshotErr != nil {
		state.Counts["postCloseSnapshotError"] = shutdownSnapshotErr.Error()
	}
	client.Close()
	closeDaemon(inst)

	restarted, err := startDaemon(ctx, daemonBinary, inst.root, state.Workspace)
	if err != nil {
		return fmt.Errorf("restart daemon: %w", err)
	}
	state.LogDirs = append(state.LogDirs, restarted.logs)
	defer closeDaemon(restarted)
	restartedClient, err := daemon.Dial(ctx, restarted.address, restarted.token)
	if err != nil {
		return err
	}
	defer restartedClient.Close()
	resumeOptions := map[string]any{"binary": state.Self, "cwd": state.Workspace, "mode": "ask", "interactionMode": "code", "model": "fixture/shellwork", "providerCursor": map[string]string{"provider": "openCode", "sessionId": nativeRootSession}}
	if _, err := restartedClient.RequestFor(ctx, sid, rid, map[string]any{"type": "start", "options": resumeOptions}); err != nil {
		return fmt.Errorf("resume shell-work runtime: %w", err)
	}
	restartedEvents := &eventReader{client: restartedClient}
	if _, err := restartedEvents.wait(ctx, "resumed runtime connected", func(ev daemon.Event) bool { return ev.Kind == "connected" }); err != nil {
		return err
	}
	if err := waitDurationEvents(ctx, restartedEvents, 1300*time.Millisecond); err != nil {
		return err
	}
	for _, ev := range restartedEvents.seen {
		if isOutputDelta(ev, "poll-shell", "") {
			return errors.New("restart replayed previously consumed shell output")
		}
	}
	recovered, err := hydrateShellSession(ctx, restartedClient, sid)
	if err != nil {
		return err
	}
	state.Counts["backgroundWorkAfterRestart"] = summarizeBackgroundWork(recovered)
	state.Counts["foreignShellBackgroundEventsAfterRestart"] = summarizeBackgroundEvents(restartedEvents.seen, "foreign-shell")
	postSnapshotErr := verifySnapshot(recovered)
	if postSnapshotErr != nil {
		state.Counts["postRestartSnapshotError"] = postSnapshotErr.Error()
	}
	state.Counts["foreignShellEntriesAfterRestart"] = matchingWorkEntries(state.Counts["backgroundWorkAfterRestart"], "foreign-shell")
	state.Counts["foreignShellEntryCountAfterRestart"] = len(matchingWorkEntries(state.Counts["backgroundWorkAfterRestart"], "foreign-shell"))
	newFixture, err := waitFixtureAdmin(ctx, restarted.root)
	if err != nil {
		return err
	}
	newState, err := newFixture.state(ctx)
	if err != nil {
		return err
	}
	if newState.RowCalls["poll-shell"] != beforeRestartState.RowCalls["poll-shell"] {
		return fmt.Errorf("terminal process was polled again after restart: row calls %d -> %d", beforeRestartState.RowCalls["poll-shell"], newState.RowCalls["poll-shell"])
	}
	note(state, "restart-hydrate-cursor-and-no-output-replay", nil)
	state.Counts["rootActivities"] = 12
	state.Counts["liveBackgroundUpserts"] = countBackgroundUpserts(reader.seen)
	state.Counts["shellOutputPages"] = len(newState.OutputRequests)
	state.Counts["sessionProvider"] = "opencode"
	state.Counts["nativeSession"] = nativeRootSession
	_ = otherState
	if shutdownSnapshotErr != nil || postSnapshotErr != nil {
		return errors.Join(shutdownSnapshotErr, func() error {
			if postSnapshotErr == nil {
				return nil
			}
			return fmt.Errorf("post-restart process snapshot: %w", postSnapshotErr)
		}())
	}
	note(state, "sqlite-process-snapshot-and-cursor", nil)
	return nil
}

func configureShellFixtures(ctx context.Context, admin *fixtureAdmin, state *reportState) error {
	pages := map[string]fixturePage{"0": {Output: "alpha-", Cursor: 6, Size: 11}, "6": {Output: "omega", Cursor: 11, Size: 11}, "11": {Cursor: 11, Size: 11}}
	tail := strings.Repeat("x", 70<<10)
	fixtures := map[string]shellFixture{
		"poll-shell":        {Owner: nativeRootSession, Status: "running", Pages: pages, FailOutputCursor: "6", FailOutputRemaining: 1, HoldOutputCursor: "6", HoldOutput: true, SlowCursor: "0", SlowDelayMs: 2800},
		"foreign-shell":     {Owner: nativeRootSession, Status: "running", WrongOwnerRows: 100, Pages: map[string]fixturePage{"0": {Output: "FOREIGN-OUTPUT-MUST-NOT-EMIT", Cursor: 31, Size: 31}}},
		"delete-shell":      {Owner: nativeRootSession, Status: "running"},
		"missing-shell":     {Owner: nativeRootSession, Status: "running", Missing: true},
		"malformed-shell":   {Owner: nativeRootSession, Status: "running", MalformedRowRemaining: 100},
		"row500-shell":      {Owner: nativeRootSession, Status: "running", RowStatusCode: http.StatusInternalServerError, RowFailureRemaining: 100},
		"root-stop-shell":   {Owner: nativeRootSession, Status: "running"},
		"failed-stop-shell": {Owner: nativeRootSession, Status: "running"},
		"complete-shell":    {Owner: nativeRootSession, Status: "completed", Exit: 0},
		"tail-shell":        {Owner: nativeRootSession, Status: "completed", Exit: 0, Pages: map[string]fixturePage{"0": {Output: tail, Cursor: uint64(len(tail)), Size: uint64(len(tail))}}},
		"child-stop-shell":  {Owner: nativeChildSession, Status: "running"},
	}
	for id, fixture := range fixtures {
		if err := admin.configure(ctx, id, fixture); err != nil {
			return err
		}
	}
	state.Counts["fixtureProcessCount"] = len(fixtures)
	state.Counts["fixtureExecutedCommands"] = 0
	return nil
}

func verifyInitialWork(events []daemon.Event, cwd string) error {
	for id, command := range map[string]string{"poll-shell": "fixture poll job", "child-stop-shell": "fixture child stop"} {
		item, err := lastWork(events, id)
		if err != nil {
			return err
		}
		wantParent := nativeRootSession
		wantCwd := "fixture-root-cwd"
		wantOrigin := "activity-poll"
		if id == "child-stop-shell" {
			wantParent = nativeChildSession
			wantCwd = "fixture-child-cwd"
			wantOrigin = "activity-child-stop"
		}
		if item.Key.Kind != "process" || item.Title != command || item.Command == nil || *item.Command != command || item.Cwd == nil || *item.Cwd != wantCwd || !item.Background || item.Status != "running" || item.ParentID == nil || *item.ParentID != wantParent || item.ControlID == nil || *item.ControlID != id || !contains(item.OriginActivityIDs, wantOrigin) {
			return fmt.Errorf("process %s identity/owner/command projection mismatch: %+v", id, item)
		}
	}
	_ = cwd
	return nil
}

func lastWork(events []daemon.Event, id string) (workView, error) {
	for i := len(events) - 1; i >= 0; i-- {
		if events[i].Kind != "backgroundWork" {
			continue
		}
		var item workView
		if json.Unmarshal(events[i].Payload, &item) != nil {
			continue
		}
		if item.Type == "upsert" && item.Key.ProviderID == id {
			return item, nil
		}
	}
	return workView{}, fmt.Errorf("no background-work upsert for %s", id)
}

func hasWorkOrigin(events []daemon.Event, origin string) bool {
	for _, event := range events {
		if event.Kind != "backgroundWork" {
			continue
		}
		var item workView
		if json.Unmarshal(event.Payload, &item) == nil && item.Type == "upsert" && contains(item.OriginActivityIDs, origin) {
			return true
		}
	}
	return false
}

func isWorkStatus(event daemon.Event, id, status string) bool {
	if event.Kind != "backgroundWork" {
		return false
	}
	var item workView
	return json.Unmarshal(event.Payload, &item) == nil && item.Type == "upsert" && item.Key.ProviderID == id && item.Status == status
}

func waitWorkStatus(parent context.Context, reader *eventReader, admin *fixtureAdmin, id, status string) (daemon.Event, error) {
	ctx, cancel := context.WithTimeout(parent, 20*time.Second)
	defer cancel()
	event, err := reader.wait(ctx, id+" becomes "+status, func(ev daemon.Event) bool { return isWorkStatus(ev, id, status) })
	if err == nil {
		return event, nil
	}
	fixture, fixtureErr := admin.state(parent)
	observed := []string{}
	for _, candidate := range reader.seen {
		if candidate.Kind == "backgroundWork" && strings.Contains(string(candidate.Payload), id) {
			observed = append(observed, string(candidate.Payload))
		}
	}
	if fixtureErr != nil {
		return daemon.Event{}, fmt.Errorf("%w; matching background events=%v; fixture-state error=%v", err, observed, fixtureErr)
	}
	var requests []shellRequestRecord
	for _, request := range fixture.RowRequests {
		if request.ID == id {
			requests = append(requests, request)
		}
	}
	return daemon.Event{}, fmt.Errorf("%w; matching background events=%v; fixture row requests=%+v", err, observed, requests)
}

func isOutputDelta(event daemon.Event, id, delta string) bool {
	if event.Kind != "backgroundWork" {
		return false
	}
	var item workView
	if json.Unmarshal(event.Payload, &item) != nil || item.Type != "outputDelta" || item.Key.ProviderID != id {
		return false
	}
	return delta == "" || item.Delta == delta
}

func isStopFailed(event daemon.Event, id string) bool {
	if event.Kind != "backgroundWork" {
		return false
	}
	var item workView
	return json.Unmarshal(event.Payload, &item) == nil && item.Type == "stopFailed" && item.Key.ProviderID == id
}

func assertStatus(events []daemon.Event, id, status string) error {
	item, err := lastWork(events, id)
	if err != nil {
		return err
	}
	if item.Status != status {
		return fmt.Errorf("%s latest status=%s, want %s", id, item.Status, status)
	}
	return nil
}

func verifyTail(events []daemon.Event) error {
	item, err := lastWork(events, "tail-shell")
	if err != nil {
		return err
	}
	if !item.OutputTruncated || item.Output == nil || len(*item.Output) > 64<<10 || len(*item.Output) != 64<<10 {
		return fmt.Errorf("shell output tail bound mismatch: truncated=%v bytes=%d", item.OutputTruncated, func() int {
			if item.Output == nil {
				return 0
			}
			return len(*item.Output)
		}())
	}
	return nil
}

func verifySnapshot(snapshot map[string]any) error {
	background, ok := snapshot["background_work"].([]any)
	if !ok {
		return fmt.Errorf("hydrate omitted background_work: %T", snapshot["background_work"])
	}
	items := map[string]workView{}
	for _, raw := range background {
		encoded, _ := json.Marshal(raw)
		var saved struct {
			Item workView `json:"item"`
		}
		if json.Unmarshal(encoded, &saved) == nil && saved.Item.Key.Kind == "process" {
			items[saved.Item.Key.ProviderID] = saved.Item
		}
	}
	root, ok := items["poll-shell"]
	if !ok {
		return errors.New("hydrated background_work omitted poll-shell")
	}
	if root.Status != "failed" || root.ExitCode == nil || *root.ExitCode != 7 || root.Output == nil || *root.Output != "alpha-omega" {
		return fmt.Errorf("hydrated terminal shell lost tail/status/code: %+v", root)
	}
	for id, want := range map[string]string{"delete-shell": "lost", "missing-shell": "lost", "foreign-shell": "lost", "root-stop-shell": "stopped", "child-stop-shell": "stopped", "failed-stop-shell": "stopped", "complete-shell": "completed", "tail-shell": "completed", "malformed-shell": "completed", "row500-shell": "completed"} {
		if got := items[id].Status; got != want {
			return fmt.Errorf("hydrated %s status=%s, want %s", id, got, want)
		}
	}
	cursors, ok := snapshot["_fintwind_shell_cursors"]
	if !ok {
		return errors.New("hydrate omitted private shell output cursors")
	}
	var values map[string]uint64
	raw, _ := json.Marshal(cursors)
	if json.Unmarshal(raw, &values) != nil || values["poll-shell"] != 11 {
		return fmt.Errorf("hydrated output cursor for poll-shell=%v, want 11", cursors)
	}
	return nil
}

func summarizeBackgroundWork(snapshot map[string]any) []map[string]any {
	rows, _ := snapshot["background_work"].([]any)
	result := make([]map[string]any, 0, len(rows))
	for index, raw := range rows {
		entry, _ := raw.(map[string]any)
		item, _ := entry["item"].(map[string]any)
		key, _ := item["key"].(map[string]any)
		result = append(result, map[string]any{
			"index":      index,
			"kind":       key["kind"],
			"providerId": key["providerId"],
			"status":     item["status"],
			"parentId":   item["parentId"],
		})
	}
	return result
}

func matchingWorkEntries(raw any, providerID string) []map[string]any {
	rows, _ := raw.([]map[string]any)
	result := make([]map[string]any, 0, len(rows))
	for _, row := range rows {
		if row["kind"] == "process" && row["providerId"] == providerID {
			result = append(result, row)
		}
	}
	return result
}

func summarizeBackgroundEvents(events []daemon.Event, providerID string) []map[string]any {
	result := []map[string]any{}
	for index, event := range events {
		if event.Kind != "backgroundWork" {
			continue
		}
		var item workView
		if json.Unmarshal(event.Payload, &item) != nil || item.Type != "upsert" || item.Key.ProviderID != providerID {
			continue
		}
		result = append(result, map[string]any{
			"eventIndex": index,
			"kind":       item.Key.Kind,
			"providerId": item.Key.ProviderID,
			"status":     item.Status,
			"parentId":   item.ParentID,
		})
	}
	return result
}

func countBackgroundUpserts(events []daemon.Event) int {
	count := 0
	for _, event := range events {
		if event.Kind != "backgroundWork" {
			continue
		}
		var item workView
		if json.Unmarshal(event.Payload, &item) == nil && item.Type == "upsert" && item.Key.Kind == "process" {
			count++
		}
	}
	return count
}

func containsOutput(events []daemon.Event, needle string) bool {
	for _, event := range events {
		if event.Kind == "backgroundWork" && strings.Contains(string(event.Payload), needle) {
			return true
		}
	}
	return false
}

func expectStopRejected(ctx context.Context, client *daemon.Client, sid, rid, controlID, providerID, kind string) error {
	_, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "stopBackgroundWork", "controlId": controlID, "key": map[string]any{"kind": kind, "providerId": providerID}})
	if err == nil {
		return fmt.Errorf("stop control unexpectedly accepted: control=%s key=%s/%s", controlID, kind, providerID)
	}
	return nil
}

func stopWork(ctx context.Context, client *daemon.Client, sid, rid, id, keyID string) error {
	_, err := client.RequestFor(ctx, sid, rid, map[string]any{"type": "stopBackgroundWork", "controlId": id, "key": map[string]any{"kind": "process", "providerId": keyID}})
	return err
}

func waitDurationEvents(ctx context.Context, reader *eventReader, d time.Duration) error {
	deadline := time.NewTimer(d)
	defer deadline.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			return nil
		default:
		}
		short, cancel := context.WithTimeout(ctx, 50*time.Millisecond)
		_, err := reader.next(short)
		cancel()
		if err != nil && !errors.Is(err, context.DeadlineExceeded) {
			return err
		}
	}
}

func hydrateShellSession(ctx context.Context, client *daemon.Client, sid string) (map[string]any, error) {
	raw, err := client.RequestFor(ctx, sid, "", map[string]any{"type": "hydrateSession", "sessionId": sid})
	if err != nil {
		return nil, err
	}
	var result map[string]any
	if json.Unmarshal(raw, &result) != nil {
		return nil, errors.New("invalid hydrate response")
	}
	session, ok := result["session"].(map[string]any)
	if !ok {
		return nil, fmt.Errorf("hydrate omitted session: %s", raw)
	}
	return session, nil
}

func waitFixtureAdmin(ctx context.Context, root string) (*fixtureAdmin, error) {
	path := filepath.Join(root, "home", "shellwork-ready.json")
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		raw, err := os.ReadFile(path)
		if err == nil {
			var ready fixtureReady
			if json.Unmarshal(raw, &ready) == nil {
				host, _, splitErr := net.SplitHostPort(ready.Address)
				if splitErr == nil && host == "127.0.0.1" && ready.Token != "" {
					return &fixtureAdmin{ready: ready}, nil
				}
			}
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-tick.C:
		}
	}
}

type fixtureAdmin struct{ ready fixtureReady }

func (a *fixtureAdmin) control(ctx context.Context, body any) (json.RawMessage, error) {
	raw, err := json.Marshal(body)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, "http://"+a.ready.Address+"/_fixture/control", strings.NewReader(string(raw)))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("x-fixture-token", a.ready.Token)
	client := http.Client{Timeout: 3 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return errors.New("fixture redirects refused") }}
	response, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	var data json.RawMessage
	if err := json.NewDecoder(response.Body).Decode(&data); err != nil {
		return nil, err
	}
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return nil, fmt.Errorf("fixture control HTTP %d: %s", response.StatusCode, data)
	}
	return data, nil
}

func (a *fixtureAdmin) configure(ctx context.Context, id string, fixture shellFixture) error {
	_, err := a.control(ctx, map[string]any{"type": "configure", "id": id, "shell": fixture})
	return err
}

func (a *fixtureAdmin) state(ctx context.Context) (shellFixtureStore, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "http://"+a.ready.Address+"/_fixture/state", nil)
	if err != nil {
		return shellFixtureStore{}, err
	}
	req.Header.Set("x-fixture-token", a.ready.Token)
	client := http.Client{Timeout: 3 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return errors.New("fixture redirects refused") }}
	response, err := client.Do(req)
	if err != nil {
		return shellFixtureStore{}, err
	}
	defer response.Body.Close()
	var value shellFixtureStore
	if err := json.NewDecoder(response.Body).Decode(&value); err != nil {
		return value, err
	}
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return value, fmt.Errorf("fixture state HTTP %d", response.StatusCode)
	}
	return value, nil
}

func waitFixture(ctx context.Context, admin *fixtureAdmin, accept func(shellFixtureStore) bool, label string) error {
	tick := time.NewTicker(40 * time.Millisecond)
	defer tick.Stop()
	for {
		state, err := admin.state(ctx)
		if err == nil && accept(state) {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("wait fixture %s: %w", label, ctx.Err())
		case <-tick.C:
		}
	}
}

func mutateShell(ctx context.Context, admin *fixtureAdmin, id string, change func(*shellFixture)) error {
	state, err := admin.state(ctx)
	if err != nil {
		return err
	}
	fixture, ok := state.Shells[id]
	if !ok {
		return fmt.Errorf("fixture shell %s missing", id)
	}
	change(&fixture)
	return admin.configure(ctx, id, fixture)
}

func contains(values []string, want string) bool {
	for _, value := range values {
		if value == want {
			return true
		}
	}
	return false
}

func countID(values []string, want string) int {
	count := 0
	for _, value := range values {
		if value == want {
			count++
		}
	}
	return count
}

func hasRequest(rows []shellRequestRecord, id string, cursor uint64, status int) bool {
	for _, row := range rows {
		if row.ID == id && row.Cursor == cursor && row.Status == status {
			return true
		}
	}
	return false
}

func hasCursorRetry(rows []shellRequestRecord, id string, cursor uint64, success int) bool {
	seenFailure := false
	seenSuccess := false
	for _, row := range rows {
		if row.ID != id || row.Cursor != cursor {
			continue
		}
		if row.Status == http.StatusInternalServerError {
			seenFailure = true
		}
		if row.Status == success {
			seenSuccess = true
		}
	}
	return seenFailure && seenSuccess
}

func hasOutputRequest(rows []shellRequestRecord, id string) bool {
	for _, row := range rows {
		if row.ID == id {
			return true
		}
	}
	return false
}

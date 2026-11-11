package main

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/daemon"

	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

// streamTotal is the full delivery target every resume case must reach
// exactly once per sequence, across as many reconnects as needed.
const streamTotal = 5000

// runFixture checks the public Client behavior against loopback fake
// servers. Nothing here speaks to the true daemon: scope stays "fake".
// Pass means full delivery, or an explicit terminal failure followed by a
// resume from Applied cursors. The client never reconnects or retries on
// its own.
func runFixture(ctx context.Context) []caseResult {
	return []caseResult{
		fixtureResumeAcrossDisconnects(ctx),
		fixtureOverflowResumeFromApplied(ctx),
		fixtureNoticesNotReplayable(ctx),
		fixtureUnknownWriteNeverRetried(ctx),
		fixtureNotifications(ctx),
		fixtureProtocolMismatch(ctx),
		fixtureHandshakeRefused(ctx),
		fixtureIdentityZero(ctx),
	}
}

var upgrader = websocket.Upgrader{}

func fakeServer(handle func(conn *websocket.Conn) error) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()
		_ = handle(conn)
	}))
}

func fakeAddress(server *httptest.Server) string {
	return strings.TrimPrefix(server.URL, "http://")
}

func readFrame(conn *websocket.Conn) (map[string]any, error) {
	_ = conn.SetReadDeadline(time.Now().Add(15 * time.Second))
	var frame map[string]any
	if err := conn.ReadJSON(&frame); err != nil {
		return nil, err
	}
	return frame, nil
}

func readHello(conn *websocket.Conn) error {
	frame, err := readFrame(conn)
	if err != nil {
		return err
	}
	if frame["type"] != "hello" {
		return fmt.Errorf("expected hello, got %v", frame["type"])
	}
	return nil
}

func writeHello(conn *websocket.Conn, protocol int) error {
	return writeHelloIDs(conn, protocol, uuid.NewString(), uuid.NewString())
}

func writeHelloIDs(conn *websocket.Conn, protocol int, envID, instanceID string) error {
	return conn.WriteJSON(map[string]any{
		"type":            "hello",
		"protocolVersion": protocol,
		"daemonVersion":   "fake",
		"environmentId":   envID,
		"instanceId":      instanceID,
	})
}

func writeEvent(conn *websocket.Conn, sessionID, kind string, payload any) error {
	return conn.WriteJSON(map[string]any{
		"type":      "event",
		"sessionId": sessionID,
		"runtimeId": "00000000-0000-0000-0000-000000000000",
		"event":     map[string]any{"kind": kind, "payload": payload},
	})
}

func writeSequencedEvent(conn *websocket.Conn, sessionID, runtimeID, epoch string, sequence int, payload any) error {
	return conn.WriteJSON(map[string]any{
		"type":      "event",
		"sessionId": sessionID,
		"runtimeId": runtimeID,
		"epoch":     epoch,
		"sequence":  sequence,
		"event":     map[string]any{"kind": "textDelta", "payload": payload},
	})
}

// streamHarness replays one sequenced driver stream across reconnects. Each
// connection resumes after the client's Applied cursors, sends its share,
// then closes: a forced hangup while shares remain, a final close once the
// whole stream is delivered.
type streamHarness struct {
	mu         sync.Mutex
	sessionID  string
	runtimeID  string
	epoch      string
	envID      string
	instanceID string
	total      int
	chunk      int
	hangups    int
	hellos     []map[string]any
}

func (h *streamHarness) serve(conn *websocket.Conn) error {
	hello, err := readFrame(conn)
	if err != nil {
		return err
	}
	redacted := map[string]any{}
	for key, value := range hello {
		redacted[key] = value
	}
	redacted["token"] = "***"
	h.mu.Lock()
	h.hellos = append(h.hellos, redacted)
	h.mu.Unlock()
	if err := writeHelloIDs(conn, daemon.ProtocolVersion, h.envID, h.instanceID); err != nil {
		return err
	}
	sent := 0
	for sequence := h.resumeStart(hello); sequence <= h.total; sequence++ {
		if err := writeSequencedEvent(conn, h.sessionID, h.runtimeID, h.epoch, sequence, "x"); err != nil {
			return err
		}
		sent++
		if h.chunk > 0 && sent >= h.chunk {
			h.mu.Lock()
			hang := h.hangups > 0
			if hang {
				h.hangups--
			}
			h.mu.Unlock()
			if hang {
				return fmt.Errorf("forced hangup after %d events", sent)
			}
		}
	}
	return fmt.Errorf("stream complete")
}

func (h *streamHarness) resumeStart(hello map[string]any) int {
	list, _ := hello["resumeFrom"].([]any)
	for _, item := range list {
		cursor, _ := item.(map[string]any)
		if cursor == nil {
			continue
		}
		sessionID, _ := cursor["sessionId"].(string)
		runtimeID, _ := cursor["runtimeId"].(string)
		epoch, _ := cursor["epoch"].(string)
		sequence, _ := cursor["sequence"].(float64)
		if sessionID == h.sessionID && runtimeID == h.runtimeID && epoch == h.epoch {
			return int(sequence) + 1
		}
	}
	return 1
}

// streamCollector folds consumed events: driver sequences counted per
// sequence, every other server push counted as a notice.
type streamCollector struct {
	seqs    map[uint64]int
	notices int
}

func (c *streamCollector) add(event daemon.Event) {
	if c.seqs == nil {
		c.seqs = map[uint64]int{}
	}
	if event.Type == "event" {
		c.seqs[event.Sequence]++
		return
	}
	c.notices++
}

func (c *streamCollector) complete(total int) bool {
	if len(c.seqs) != total {
		return false
	}
	for sequence := 1; sequence <= total; sequence++ {
		if c.seqs[uint64(sequence)] != 1 {
			return false
		}
	}
	return true
}

// collectLive drains one connection while it runs: every consumed event is
// folded and Acked so Cursors advances. It returns once the connection ends
// or the full stream arrived.
func collectLive(ctx context.Context, client *daemon.Client, col *streamCollector, total int) {
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if total > 0 && col.complete(total) {
			return
		}
		select {
		case event, ok := <-client.Events():
			if !ok {
				return
			}
			col.add(event)
			client.Ack(event)
		case <-client.Done():
			drainRest(client, col)
			return
		case <-ctx.Done():
			return
		case <-time.After(100 * time.Millisecond):
		}
	}
}

// waitTerminalDrain waits for the connection to end without consuming, then
// folds and Acks everything still queued. Queued events stay drainable
// after Done; unblocking the reader early would hide an overflow.
func waitTerminalDrain(ctx context.Context, client *daemon.Client, col *streamCollector) {
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		select {
		case <-client.Done():
			drainRest(client, col)
			return
		case <-ctx.Done():
			return
		case <-time.After(20 * time.Millisecond):
		}
	}
}

func drainRest(client *daemon.Client, col *streamCollector) {
	timeout := time.Now().Add(10 * time.Second)
	for time.Now().Before(timeout) {
		select {
		case event, ok := <-client.Events():
			if !ok {
				return
			}
			col.add(event)
			client.Ack(event)
		case <-time.After(100 * time.Millisecond):
			return
		}
	}
}

// resumeStream redials with Applied cursors until the full stream arrives
// exactly once per sequence or attempts run out. The first dial carries the
// seed cursors; every later dial carries the previous dial's Cursors.
func resumeStream(ctx context.Context, address, token string, harness *streamHarness, col *streamCollector, live bool, seed []daemon.ReplayCursor) error {
	cursors := seed
	for attempt := 0; attempt < 8; attempt++ {
		if col.complete(harness.total) {
			return nil
		}
		dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
		client, err := daemon.DialWithResume(dialCtx, address, token, cursors)
		cancel()
		if err != nil {
			return fmt.Errorf("redial %d: %w", attempt, err)
		}
		if live {
			collectLive(ctx, client, col, harness.total)
		} else {
			waitTerminalDrain(ctx, client, col)
		}
		cursors = client.Cursors()
		client.Close()
	}
	if !col.complete(harness.total) {
		var missing []uint64
		dupes := 0
		for sequence := 1; sequence <= harness.total; sequence++ {
			count := col.seqs[uint64(sequence)]
			switch {
			case count == 0:
				if len(missing) < 10 {
					missing = append(missing, uint64(sequence))
				}
			case count > 1:
				dupes += count - 1
			}
		}
		return fmt.Errorf("stream incomplete after 8 dials: %d of %d sequences, missing=%v dupes=%d", len(col.seqs), harness.total, missing, dupes)
	}
	return nil
}

// fixtureResumeAcrossDisconnects forces two mid-stream hangups. The client
// must resume from Applied cursors until all 5000 sequences arrive exactly
// once.
func fixtureResumeAcrossDisconnects(ctx context.Context) caseResult {
	name := "two disconnects resume from applied cursors with no gaps or dupes"
	harness := &streamHarness{
		sessionID:  uuid.NewString(),
		runtimeID:  uuid.NewString(),
		epoch:      uuid.NewString(),
		envID:      uuid.NewString(),
		instanceID: uuid.NewString(),
		total:      streamTotal,
		chunk:      1500,
		hangups:    2,
	}
	server := fakeServer(harness.serve)
	defer server.Close()

	col := &streamCollector{}
	if err := resumeStream(ctx, fakeAddress(server), "fake-token", harness, col, true, nil); err != nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: err.Error()}
	}
	harness.mu.Lock()
	dials := len(harness.hellos)
	harness.mu.Unlock()
	return caseResult{Name: name, Scope: "fake", Status: "pass", Detail: fmt.Sprintf("%d sequences exactly once across %d dials", streamTotal, dials)}
}

// fixtureOverflowResumeFromApplied floods the whole stream at once without
// draining. The queue must fail the connection explicitly instead of
// dropping data, and the resume from Applied cursors must complete it.
func fixtureOverflowResumeFromApplied(ctx context.Context) caseResult {
	name := "overflow fails explicitly then resumes from applied cursors"
	harness := &streamHarness{
		sessionID:  uuid.NewString(),
		runtimeID:  uuid.NewString(),
		epoch:      uuid.NewString(),
		envID:      uuid.NewString(),
		instanceID: uuid.NewString(),
		total:      streamTotal,
	}
	server := fakeServer(harness.serve)
	defer server.Close()

	dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	client, err := daemon.Dial(dialCtx, fakeAddress(server), "fake-token")
	cancel()
	if err != nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("dial: %v", err)}
	}
	col := &streamCollector{}
	waitTerminalDrain(ctx, client, col)
	terminal := client.Err()
	cursors := client.Cursors()
	client.Close()
	if terminal == nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "connection survived a 5000-event flood; overflow must fail explicitly"}
	}
	if len(cursors) != 1 || cursors[0].Epoch != harness.epoch || cursors[0].Sequence != 4096 {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("applied cursors after overflow: %+v, want one mark at 4096", cursors)}
	}
	if err := resumeStream(ctx, fakeAddress(server), "fake-token", harness, col, true, cursors); err != nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: err.Error()}
	}
	return caseResult{Name: name, Scope: "fake", Status: "pass", Detail: "overflow failed openly, applied mark at 4096, resume completed 5000 exactly once"}
}

// fixtureNoticesNotReplayable checks notices arrive as events but never
// become Applied cursors and never appear in resumeFrom.
func fixtureNoticesNotReplayable(ctx context.Context) caseResult {
	name := "notices arrive but are never replayable cursors"
	sessionID := uuid.NewString()
	runtimeID := uuid.NewString()
	epoch := uuid.NewString()
	var hellos []map[string]any
	var mu sync.Mutex
	server := fakeServer(func(conn *websocket.Conn) error {
		hello, err := readFrame(conn)
		if err != nil {
			return err
		}
		mu.Lock()
		hellos = append(hellos, hello)
		mu.Unlock()
		if err := writeHello(conn, daemon.ProtocolVersion); err != nil {
			return err
		}
		for _, notice := range []string{"taskStateChanged", "scheduledTasksChanged"} {
			if err := conn.WriteJSON(map[string]any{"type": notice, "note": notice}); err != nil {
				return err
			}
		}
		if err := writeSequencedEvent(conn, sessionID, runtimeID, epoch, 1, "x"); err != nil {
			return err
		}
		_ = conn.SetReadDeadline(time.Now().Add(10 * time.Second))
		var frame map[string]any
		_ = conn.ReadJSON(&frame)
		return nil
	})
	defer server.Close()

	dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	client, err := daemon.Dial(dialCtx, fakeAddress(server), "fake-token")
	cancel()
	if err != nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("dial: %v", err)}
	}
	seenNotices := map[string]bool{}
	seenDriver := false
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) && (!seenDriver || len(seenNotices) != 2) {
		select {
		case event, ok := <-client.Events():
			if !ok {
				break
			}
			client.Ack(event)
			if event.Type == "event" {
				seenDriver = true
				continue
			}
			seenNotices[event.Type] = true
		case <-ctx.Done():
			break
		case <-time.After(100 * time.Millisecond):
		}
	}
	cursors := client.Cursors()
	client.Close()
	if !seenDriver || len(seenNotices) != 2 {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "notices or driver event missing"}
	}
	if len(cursors) != 1 || cursors[0].Epoch != epoch || cursors[0].Sequence != 1 {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("notices leaked into applied cursors: %+v", cursors)}
	}
	secondCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	second, err := daemon.DialWithResume(secondCtx, fakeAddress(server), "fake-token", cursors)
	cancel()
	if err != nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("resume dial: %v", err)}
	}
	second.Close()
	mu.Lock()
	defer mu.Unlock()
	if len(hellos) != 2 {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("want 2 hellos, got %d", len(hellos))}
	}
	resumed, _ := hellos[1]["resumeFrom"].([]any)
	if len(resumed) != 1 {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("resumeFrom carries %d cursors, want exactly the driver mark", len(resumed))}
	}
	return caseResult{Name: name, Scope: "fake", Status: "pass", Detail: "2 notices observed, only the driver mark applied and resumed"}
}

// fixtureUnknownWriteNeverRetried overflows the connection, then proves a
// later Request and Send surface errors instead of resending: the fake
// server must observe no request frame after the terminal failure.
func fixtureUnknownWriteNeverRetried(ctx context.Context) caseResult {
	name := "unknown write results surface errors, never resend"
	var mu sync.Mutex
	retried := false
	server := fakeServer(func(conn *websocket.Conn) error {
		if err := readHello(conn); err != nil {
			return err
		}
		if err := writeHello(conn, daemon.ProtocolVersion); err != nil {
			return err
		}
		for i := 0; i < 5000; i++ {
			if err := writeEvent(conn, uuid.NewString(), "textDelta", "x"); err != nil {
				break
			}
		}
		_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
		for {
			var frame map[string]any
			if err := conn.ReadJSON(&frame); err != nil {
				break
			}
			if frame["type"] == "request" {
				mu.Lock()
				retried = true
				mu.Unlock()
			}
		}
		return nil
	})
	defer server.Close()

	dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	client, err := daemon.Dial(dialCtx, fakeAddress(server), "fake-token")
	cancel()
	if err != nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("dial: %v", err)}
	}
	col := &streamCollector{}
	waitTerminalDrain(ctx, client, col)
	if client.Err() == nil {
		client.Close()
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "connection survived the flood"}
	}
	reqCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	_, reqErr := client.Request(reqCtx, map[string]any{"type": "loadTaskState"})
	cancel()
	sendErr := client.Send(ctx, map[string]any{"type": "ping"})
	client.Close()
	time.Sleep(2100 * time.Millisecond)
	mu.Lock()
	defer mu.Unlock()
	if reqErr == nil || sendErr == nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "request or send succeeded after terminal failure"}
	}
	if retried {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "client sent a frame after the terminal failure"}
	}
	return caseResult{Name: name, Scope: "fake", Status: "pass", Detail: "request and send failed openly, server observed no retry"}
}

// fixtureNotifications checks ordered delivery of pushed driver events and
// that Identity reports the handshake-confirmed remote side.
func fixtureNotifications(ctx context.Context) caseResult {
	name := "pushed notifications arrive in order with handshake identity"
	kinds := []string{"notify-a", "notify-b", "notify-c"}
	sessionID := uuid.NewString()
	envID := uuid.NewString()
	instanceID := uuid.NewString()
	server := fakeServer(func(conn *websocket.Conn) error {
		if err := readHello(conn); err != nil {
			return err
		}
		if err := writeHelloIDs(conn, daemon.ProtocolVersion, envID, instanceID); err != nil {
			return err
		}
		for _, kind := range kinds {
			if err := writeEvent(conn, sessionID, kind, map[string]any{"n": kind}); err != nil {
				return err
			}
		}
		var request struct {
			Type      string `json:"type"`
			RequestID string `json:"requestId"`
		}
		if err := conn.ReadJSON(&request); err != nil {
			return err
		}
		return conn.WriteJSON(map[string]any{
			"type":      "response",
			"requestId": request.RequestID,
			"outcome":   map[string]any{"status": "ok", "payload": map[string]any{}},
		})
	})
	defer server.Close()

	dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	client, err := daemon.Dial(dialCtx, fakeAddress(server), "fake-token")
	if err != nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("dial: %v", err)}
	}
	defer client.Close()

	identity := client.Identity()
	if identity.EnvironmentID != envID || identity.InstanceID != instanceID {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "identity does not match the handshake-confirmed remote side"}
	}
	for index, want := range kinds {
		select {
		case event, ok := <-client.Events():
			if !ok {
				return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "event stream closed"}
			}
			if event.Kind != want || event.SessionID != sessionID {
				return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("event %d: got %q, want %q", index, event.Kind, want)}
			}
		case <-ctx.Done():
			return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "timed out waiting for notifications"}
		}
	}
	reqCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	if _, err := client.Request(reqCtx, map[string]any{"type": "loadTaskState"}); err != nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("request after notifications: %v", err)}
	}
	return caseResult{Name: name, Scope: "fake", Status: "pass", Detail: "3 notifications in order, identity confirmed, connection healthy"}
}

// fixtureProtocolMismatch checks a wrong wire version fails the handshake.
func fixtureProtocolMismatch(ctx context.Context) caseResult {
	name := "wrong protocol version fails the handshake openly"
	server := fakeServer(func(conn *websocket.Conn) error {
		if err := readHello(conn); err != nil {
			return err
		}
		return writeHello(conn, daemon.ProtocolVersion+1)
	})
	defer server.Close()

	dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	_, err := daemon.Dial(dialCtx, fakeAddress(server), "fake-token")
	if err == nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "dial accepted a foreign protocol version"}
	}
	if !strings.Contains(err.Error(), "protocol") {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("error hides identity cause: %v", err)}
	}
	return caseResult{Name: name, Scope: "fake", Status: "pass", Detail: "dial refused: protocol named in error"}
}

// fixtureHandshakeRefused checks a non-hello first frame fails openly.
func fixtureHandshakeRefused(ctx context.Context) caseResult {
	name := "refused handshake fails the dial openly"
	server := fakeServer(func(conn *websocket.Conn) error {
		var hello map[string]any
		_ = conn.ReadJSON(&hello)
		return conn.WriteJSON(map[string]any{"type": "rejected", "message": "bad token"})
	})
	defer server.Close()

	dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	_, err := daemon.Dial(dialCtx, fakeAddress(server), "bad-token")
	if err == nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "dial accepted a rejected handshake"}
	}
	return caseResult{Name: name, Scope: "fake", Status: "pass", Detail: fmt.Sprintf("dial refused openly: %v", redact(err))}
}

// fixtureIdentityZero checks a zero environment identity fails the dial.
func fixtureIdentityZero(ctx context.Context) caseResult {
	name := "zero environment identity fails the dial openly"
	server := fakeServer(func(conn *websocket.Conn) error {
		if err := readHello(conn); err != nil {
			return err
		}
		return writeHelloIDs(conn, daemon.ProtocolVersion, "00000000-0000-0000-0000-000000000000", uuid.NewString())
	})
	defer server.Close()

	dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	_, err := daemon.Dial(dialCtx, fakeAddress(server), "fake-token")
	if err == nil {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: "dial accepted a zero environment identity"}
	}
	if !strings.Contains(err.Error(), "identity") {
		return caseResult{Name: name, Scope: "fake", Status: "fail", Detail: fmt.Sprintf("error hides identity cause: %v", err)}
	}
	return caseResult{Name: name, Scope: "fake", Status: "pass", Detail: "dial refused: zero identity named in error"}
}

func redact(err error) string {
	msg := err.Error()
	for _, secret := range []string{"bad-token", "bad token", "fake-token"} {
		msg = strings.ReplaceAll(msg, secret, "***")
	}
	return msg
}

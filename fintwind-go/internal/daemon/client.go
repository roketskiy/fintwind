// Package daemon speaks the Fintwind daemon protocol over its local WebSocket.
package daemon

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"sort"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

// ProtocolVersion is the wire version this client speaks. It must match the
// Rust daemon's fintwind-protocol PROTOCOL_VERSION; the launcher refuses a
// daemon that reports anything else.
const ProtocolVersion = 25

const zeroUUID = "00000000-0000-0000-0000-000000000000"

// ErrEventOverflow reports that the bounded event queue filled before the
// consumer drained it. The connection is closed; resume with DialWithResume
// and the cursors from Cursors, which only advance through Ack.
var ErrEventOverflow = errors.New("daemon event queue overflow")

// ErrSequenceGap reports a missing sequence inside one epoch. The connection
// is closed; resume with DialWithResume so the daemon replays from the last
// applied cursor instead of guessing the missing delta.
var ErrSequenceGap = errors.New("daemon event sequence gap")

// Event is one message the daemon pushed. Driver events carry Type "event"
// and keep the driver Kind. Every other server push (task state, scheduled
// tasks, recovery markers, unknown types) is a notice:
// Type and Kind both name the server message type and Payload holds the
// complete original JSON envelope so the upper layer decides what it means.
type Event struct {
	Type      string
	SessionID string
	RuntimeID string
	Epoch     string
	Sequence  uint64
	Kind      string
	Payload   json.RawMessage
}

// ReplayCursor is one applied high-water mark. JSON tags stay camelCase to
// match the hello resumeFrom wire shape.
type ReplayCursor struct {
	SessionID string `json:"sessionId"`
	RuntimeID string `json:"runtimeId"`
	Epoch     string `json:"epoch"`
	Sequence  uint64 `json:"sequence"`
}

// Identity is the handshake-confirmed remote side: the stable environment
// plus this daemon process instance. Both are nonzero UUIDs or Dial refuses.
type Identity struct {
	EnvironmentID string
	InstanceID    string
}

// Client is one authenticated connection. Requests and Send are safe to
// issue from any goroutine; the reader goroutine dispatches each response
// to its caller and queues each push on Events. The client never reconnects
// on its own: Done plus Err tell the owner when to DialWithResume again.
type Client struct {
	conn     *websocket.Conn
	identity Identity
	// version is the daemon's own version from the handshake; the settings
	// page shows it as the app version.
	version string

	writes  chan struct{}
	mu      sync.Mutex
	pending map[string]chan response
	// applied holds per session/runtime high-water marks. Only Ack advances
	// them; receiving an event never does.
	applied map[cursorKey]appliedCursor
	// seen tracks the last delivered sequence per session/runtime for gap
	// and epoch-change detection. It never feeds Cursors.
	seen   map[cursorKey]seenCursor
	closed chan struct{}
	once   sync.Once
	err    error
	events chan Event
}

type cursorKey struct {
	session string
	runtime string
}

type appliedCursor struct {
	epoch    string
	sequence uint64
}

type seenCursor struct {
	epoch       string
	sequence    uint64
	initialized bool
}

type response struct {
	status  string
	payload json.RawMessage
	rpcErr  string
	connErr error
}

// Dial opens a connection and completes the hello handshake.
func Dial(ctx context.Context, address, token string) (*Client, error) {
	return DialWithResume(ctx, address, token, nil)
}

// DialWithResume opens a connection and replays every event after resume.
// The resume slice is copied; later Ack calls never mutate the caller's copy.
func DialWithResume(ctx context.Context, address, token string, resume []ReplayCursor) (*Client, error) {
	if err := checkLoopbackAddress(address); err != nil {
		return nil, err
	}
	u := url.URL{Scheme: "ws", Host: address, Path: "/v1"}
	dialer := websocket.Dialer{HandshakeTimeout: 10 * time.Second}
	conn, _, err := dialer.DialContext(ctx, u.String(), nil)
	if err != nil {
		return nil, fmt.Errorf("connect to daemon: %w", err)
	}
	conn.SetReadLimit(MaxWireMessageBytes)
	stopHandshake := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stopHandshake()
	resumeFrom := make([]ReplayCursor, 0, len(resume))
	for _, cursor := range resume {
		if cursor.SessionID == "" && cursor.RuntimeID == "" && cursor.Epoch == "" && cursor.Sequence == 0 {
			continue
		}
		resumeFrom = append(resumeFrom, cursor)
	}
	hello := map[string]any{
		"type":            "hello",
		"protocolVersion": ProtocolVersion,
		"token":           token,
		"clientId":        uuid.NewString(),
		"resumeFrom":      resumeFrom,
	}
	client := &Client{
		conn:    conn,
		writes:  make(chan struct{}, 1),
		pending: make(map[string]chan response),
		applied: make(map[cursorKey]appliedCursor),
		seen:    make(map[cursorKey]seenCursor),
		closed:  make(chan struct{}),
		events:  make(chan Event, eventQueueSize),
	}
	for _, cursor := range resumeFrom {
		key := cursorKey{session: cursor.SessionID, runtime: cursor.RuntimeID}
		current, ok := client.applied[key]
		if !ok || cursor.Epoch != current.epoch || cursor.Sequence > current.sequence {
			client.applied[key] = appliedCursor{epoch: cursor.Epoch, sequence: cursor.Sequence}
		}
	}
	if err := client.writeJSON(ctx, hello); err != nil {
		conn.Close()
		return nil, fmt.Errorf("send hello: %w", err)
	}
	deadline := time.Now().Add(10 * time.Second)
	if value, ok := ctx.Deadline(); ok && value.Before(deadline) {
		deadline = value
	}
	_ = conn.SetReadDeadline(deadline)
	var welcome struct {
		Type            string `json:"type"`
		ProtocolVersion int    `json:"protocolVersion"`
		DaemonVersion   string `json:"daemonVersion"`
		EnvironmentID   string `json:"environmentId"`
		InstanceID      string `json:"instanceId"`
		Message         string `json:"message"`
	}
	if err := conn.ReadJSON(&welcome); err != nil {
		conn.Close()
		return nil, fmt.Errorf("read hello: %w", err)
	}
	if welcome.Type == "rejected" {
		conn.Close()
		return nil, fmt.Errorf("daemon rejected the connection: %s", firstNonEmpty(welcome.Message, welcome.Type))
	}
	if welcome.Type != "hello" {
		conn.Close()
		return nil, fmt.Errorf("daemon sent an invalid handshake response: %s", firstNonEmpty(welcome.Message, welcome.Type))
	}
	if welcome.ProtocolVersion != ProtocolVersion {
		conn.Close()
		return nil, fmt.Errorf("daemon speaks protocol %d, this client speaks %d", welcome.ProtocolVersion, ProtocolVersion)
	}
	if !validIdentity(welcome.EnvironmentID) || !validIdentity(welcome.InstanceID) {
		conn.Close()
		return nil, fmt.Errorf("daemon reported an invalid environment identity")
	}
	client.identity = Identity{EnvironmentID: welcome.EnvironmentID, InstanceID: welcome.InstanceID}
	client.version = welcome.DaemonVersion
	if err := ctx.Err(); err != nil {
		conn.Close()
		return nil, err
	}
	_ = conn.SetReadDeadline(time.Time{})
	conn.SetPingHandler(func(appData string) error {
		return client.conn.WriteControl(websocket.PongMessage, []byte(appData), time.Now().Add(writeTimeout))
	})
	go client.readLoop()
	return client, nil
}

// Request sends one command and returns the response payload. It never
// retries: a failure leaves the outcome unknown and says so.
func (c *Client) Request(ctx context.Context, command any) (json.RawMessage, error) {
	return c.RequestFor(ctx, zeroUUID, zeroUUID, command)
}

// RequestFor sends one command bound to a session and runtime. It never
// retries: a failure leaves the outcome unknown and says so.
func (c *Client) RequestFor(ctx context.Context, sessionID, runtimeID string, command any) (json.RawMessage, error) {
	if sessionID == "" {
		sessionID = zeroUUID
	}
	if runtimeID == "" {
		runtimeID = zeroUUID
	}
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("request not sent, context ended: %w", err)
	}
	requestID := uuid.NewString()
	ch := make(chan response, 1)
	c.mu.Lock()
	select {
	case <-c.closed:
		reason := c.err
		c.mu.Unlock()
		if reason == nil {
			reason = errors.New("daemon connection closed")
		}
		return nil, fmt.Errorf("request %s not sent, connection is closed: %w", requestID, reason)
	default:
	}
	c.pending[requestID] = ch
	c.mu.Unlock()
	defer func() {
		c.mu.Lock()
		delete(c.pending, requestID)
		c.mu.Unlock()
	}()
	envelope := map[string]any{
		"type":      "request",
		"requestId": requestID,
		"sessionId": sessionID,
		"runtimeId": runtimeID,
		"command":   command,
	}
	if err := c.writeJSON(ctx, envelope); err != nil {
		c.mu.Lock()
		delete(c.pending, requestID)
		c.mu.Unlock()
		return nil, fmt.Errorf("send request %s: %w (delivery unknown: the daemon may have received it; do not retry automatically)", requestID, err)
	}
	select {
	case <-ctx.Done():
		c.mu.Lock()
		delete(c.pending, requestID)
		c.mu.Unlock()
		return nil, fmt.Errorf("request %s cancelled while awaiting a response: result unknown, the daemon may still execute it, do not retry automatically: %w", requestID, ctx.Err())
	case <-c.closed:
		reason := c.Err()
		if reason == nil {
			reason = errors.New("daemon connection closed")
		}
		return nil, fmt.Errorf("request %s left without a response: result unknown, the daemon may still execute it, do not retry automatically: %w", requestID, reason)
	case msg := <-ch:
		if msg.connErr != nil {
			return nil, fmt.Errorf("request %s left without a response: result unknown, the daemon may still execute it, do not retry automatically: %w", requestID, msg.connErr)
		}
		if msg.rpcErr != "" || msg.status != "ok" {
			return nil, fmt.Errorf("daemon request %s failed: %s", requestID, firstNonEmpty(msg.rpcErr, msg.status))
		}
		return msg.payload, nil
	}
}

// Events is the stream of driver events and notices. Consumers that never
// Ack still observe every queued event in order; only Ack enables resume.
func (c *Client) Events() <-chan Event {
	return c.events
}

// Ack records one event as applied by the upper layer. Only driver events
// with a nonzero epoch and sequence move the per session/runtime high-water
// mark; notices and zero markers are ignored. Receiving an event never
// applies it.
func (c *Client) Ack(event Event) {
	if event.Epoch == "" || event.Epoch == zeroUUID || event.Sequence == 0 {
		return
	}
	if event.SessionID == "" && event.RuntimeID == "" {
		return
	}
	key := cursorKey{session: event.SessionID, runtime: event.RuntimeID}
	c.mu.Lock()
	defer c.mu.Unlock()
	current := c.applied[key]
	if event.Epoch != current.epoch {
		c.applied[key] = appliedCursor{epoch: event.Epoch, sequence: event.Sequence}
		return
	}
	if event.Sequence > current.sequence {
		c.applied[key] = appliedCursor{epoch: event.Epoch, sequence: event.Sequence}
	}
}

// Cursors returns a copy of the applied high-water marks for DialWithResume.
func (c *Client) Cursors() []ReplayCursor {
	c.mu.Lock()
	defer c.mu.Unlock()
	cursors := make([]ReplayCursor, 0, len(c.applied))
	for key, cursor := range c.applied {
		cursors = append(cursors, ReplayCursor{
			SessionID: key.session,
			RuntimeID: key.runtime,
			Epoch:     cursor.epoch,
			Sequence:  cursor.sequence,
		})
	}
	sort.Slice(cursors, func(i, j int) bool {
		if cursors[i].SessionID != cursors[j].SessionID {
			return cursors[i].SessionID < cursors[j].SessionID
		}
		if cursors[i].RuntimeID != cursors[j].RuntimeID {
			return cursors[i].RuntimeID < cursors[j].RuntimeID
		}
		return cursors[i].Epoch < cursors[j].Epoch
	})
	return cursors
}

// Err reports the terminal error, or nil while the connection runs.
func (c *Client) Err() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.err
}

// Done closes when the connection ends. Events already queued stay
// drainable after Done.
func (c *Client) Done() <-chan struct{} {
	return c.closed
}

// Identity returns the handshake-confirmed remote environment and instance.
func (c *Client) Identity() Identity {
	return c.identity
}

// Version returns the daemon version reported by the handshake, or "" when
// the daemon did not send one.
func (c *Client) Version() string {
	return c.version
}

// Send enqueues one fire-and-forget protocol message. It never retries:
// a failed send leaves delivery unknown and says so.
func (c *Client) Send(ctx context.Context, message any) error {
	if err := ctx.Err(); err != nil {
		return fmt.Errorf("message not sent, context ended: %w", err)
	}
	select {
	case <-c.closed:
		return fmt.Errorf("message not sent, connection is closed: %w", c.Err())
	case <-ctx.Done():
		return fmt.Errorf("message not sent, context ended: %w", ctx.Err())
	default:
	}
	if err := c.writeJSON(ctx, message); err != nil {
		return fmt.Errorf("send message: %w (delivery unknown: the daemon may have received it; do not retry automatically)", err)
	}
	return nil
}

// Close ends the connection and releases every pending waiter. The daemon
// process is stopped separately. Close is idempotent.
func (c *Client) Close() {
	c.fail(errors.New("client closed"))
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value != "" {
			return value
		}
	}
	return "unknown error"
}

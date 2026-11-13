// transport.go holds the daemon wire loop: loopback and identity guards,
// the reader goroutine, and the bounded event queue.

package daemon

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

// eventQueueSize bounds driver events plus notices. A full queue fails the
// connection instead of dropping data, so resume from applied cursors.
const eventQueueSize = 4096

// MaxWireMessageBytes caps one daemon wire message.
const MaxWireMessageBytes = 256 << 20

// writeTimeout bounds one outbound frame so a stuck peer cannot park a
// sender forever.
const writeTimeout = 15 * time.Second

type serverMessage struct {
	Type      string `json:"type"`
	RequestID string `json:"requestId"`
	Message   string `json:"message"`
	SessionID string `json:"sessionId"`
	RuntimeID string `json:"runtimeId"`
	Epoch     string `json:"epoch"`
	Sequence  uint64 `json:"sequence"`
	Event     struct {
		Kind    string          `json:"kind"`
		Payload json.RawMessage `json:"payload"`
	} `json:"event"`
	Outcome struct {
		Status  string          `json:"status"`
		Payload json.RawMessage `json:"payload"`
		Error   struct {
			Message string `json:"message"`
		} `json:"error"`
	} `json:"outcome"`
}

// checkLoopbackAddress refuses anything that is not a loopback destination
// before any token bytes leave this process.
func checkLoopbackAddress(address string) error {
	host := address
	if parsed, _, err := net.SplitHostPort(address); err == nil {
		host = parsed
	}
	host = strings.Trim(host, "[]")
	if strings.EqualFold(host, "localhost") {
		return nil
	}
	if ip := net.ParseIP(host); ip != nil && ip.IsLoopback() {
		return nil
	}
	return fmt.Errorf("refusing to send the daemon token to non-loopback address %q: loopback only", address)
}

// validIdentity accepts only a nonzero UUID, so a broken daemon never
// passes as a routable environment.
func validIdentity(value string) bool {
	id, err := uuid.Parse(value)
	if err != nil {
		return false
	}
	return id != uuid.Nil
}

func (c *Client) writeJSON(ctx context.Context, value any) error {
	select {
	case c.writes <- struct{}{}:
	case <-ctx.Done():
		return ctx.Err()
	case <-c.closed:
		return c.Err()
	}
	defer func() { <-c.writes }()
	if err := ctx.Err(); err != nil {
		return err
	}
	if c.conn == nil {
		return errors.New("connection is closed")
	}
	deadline := time.Now().Add(writeTimeout)
	if value, ok := ctx.Deadline(); ok && value.Before(deadline) {
		deadline = value
	}
	_ = c.conn.SetWriteDeadline(deadline)
	err := c.conn.WriteJSON(value)
	_ = c.conn.SetWriteDeadline(time.Time{})
	if err != nil {
		c.fail(fmt.Errorf("write daemon message: %w", err))
	}
	return err
}

// readLoop dispatches responses to their waiters without blocking and queues
// driver events plus notices in order. It never drops data silently: a full
// queue or a sequence gap fails the connection so the owner resumes from
// applied cursors. Exiting closes the event channel; queued events stay
// drainable first.
func (c *Client) readLoop() {
	defer close(c.events)
	for {
		kind, raw, err := c.conn.ReadMessage()
		if err != nil {
			c.fail(fmt.Errorf("read daemon message: %w", err))
			return
		}
		if kind != websocket.TextMessage {
			c.fail(fmt.Errorf("daemon sent an unsupported binary frame: message type %d", kind))
			return
		}
		var msg serverMessage
		if err := json.Unmarshal(raw, &msg); err != nil {
			c.fail(fmt.Errorf("decode daemon message: %w", err))
			return
		}
		switch msg.Type {
		case "response":
			c.dispatchResponse(msg)
		case "rejected":
			c.fail(fmt.Errorf("daemon rejected the connection: %s", firstNonEmpty(msg.Message, msg.Type)))
			return
		case "event":
			if !c.deliverDriverEvent(msg) {
				return
			}
		case "shuttingDown":
			c.enqueueEvent(Event{Type: msg.Type, Kind: msg.Type, Payload: append(json.RawMessage(nil), raw...)})
			c.fail(errors.New("daemon is shutting down"))
			return
		case "taskStateChanged", "scheduledTasksChanged":
			if !c.enqueueEvent(Event{Type: msg.Type, Kind: msg.Type, Payload: append(json.RawMessage(nil), raw...)}) {
				return
			}
		default:
			name := firstNonEmpty(msg.Type, "unknown")
			if !c.enqueueEvent(Event{Type: name, Kind: name, Payload: append(json.RawMessage(nil), raw...)}) {
				return
			}
		}
	}
}

func (c *Client) dispatchResponse(msg serverMessage) {
	c.mu.Lock()
	ch := c.pending[msg.RequestID]
	c.mu.Unlock()
	if ch == nil {
		return
	}
	out := response{
		status:  msg.Outcome.Status,
		payload: msg.Outcome.Payload,
		rpcErr:  msg.Outcome.Error.Message,
	}
	select {
	case ch <- out:
	default:
	}
}

// deliverDriverEvent queues one driver event in wire order. Replays of
// already applied sequences stay suppressed, a new epoch raises an explicit
// in-band marker without dropping its events, and a gap inside one epoch
// fails the connection. True reports the loop continues; false reports the
// connection already failed.
func (c *Client) deliverDriverEvent(msg serverMessage) bool {
	event := Event{
		Type:      "event",
		SessionID: msg.SessionID,
		RuntimeID: msg.RuntimeID,
		Epoch:     msg.Epoch,
		Sequence:  msg.Sequence,
		Kind:      msg.Event.Kind,
		Payload:   msg.Event.Payload,
	}
	key := cursorKey{session: msg.SessionID, runtime: msg.RuntimeID}
	c.mu.Lock()
	if applied, ok := c.applied[key]; ok && event.Epoch != "" && event.Epoch == applied.epoch && event.Sequence <= applied.sequence {
		c.mu.Unlock()
		return true
	}
	seen := c.seen[key]
	var marker *Event
	switch {
	case seen.initialized && event.Epoch != "" && seen.epoch == event.Epoch && event.Sequence <= seen.sequence:
		c.mu.Unlock()
		return true
	case seen.initialized && event.Epoch != "" && seen.epoch == event.Epoch && event.Sequence > seen.sequence+1:
		c.mu.Unlock()
		c.fail(fmt.Errorf("%w: session %q runtime %q epoch %q delivered sequence %d after %d",
			ErrSequenceGap, msg.SessionID, msg.RuntimeID, msg.Epoch, event.Sequence, seen.sequence))
		return false
	case seen.initialized && event.Epoch != "" && seen.epoch != "" && seen.epoch != event.Epoch:
		detail, _ := json.Marshal(map[string]any{
			"sessionId":     msg.SessionID,
			"runtimeId":     msg.RuntimeID,
			"previousEpoch": seen.epoch,
			"epoch":         event.Epoch,
			"sequence":      event.Sequence,
		})
		marker = &Event{
			Type:      "epochChanged",
			SessionID: msg.SessionID,
			RuntimeID: msg.RuntimeID,
			Epoch:     event.Epoch,
			Sequence:  event.Sequence,
			Kind:      "epochChanged",
			Payload:   detail,
		}
	}
	c.seen[key] = seenCursor{epoch: event.Epoch, sequence: event.Sequence, initialized: true}
	c.mu.Unlock()
	if marker != nil && !c.enqueueEvent(*marker) {
		return false
	}
	return c.enqueueEvent(event)
}

// enqueueEvent queues one event or notice. A full queue fails the connection
// with ErrEventOverflow so the owner resumes from applied cursors instead of
// running on silently dropped data.
func (c *Client) enqueueEvent(event Event) bool {
	select {
	case c.events <- event:
		return true
	default:
	}
	c.fail(fmt.Errorf("%w: %d events queued and the consumer did not drain them; resume from applied cursors",
		ErrEventOverflow, eventQueueSize))
	return false
}

// fail records the terminal error once, releases every pending waiter with
// that reason, wakes Done, and releases the socket. Queued events stay
// drainable on Events.
func (c *Client) fail(err error) {
	if err == nil {
		err = errors.New("daemon connection closed")
	}
	c.once.Do(func() {
		c.mu.Lock()
		c.err = err
		waiters := make([]chan response, 0, len(c.pending))
		for _, ch := range c.pending {
			waiters = append(waiters, ch)
		}
		c.pending = make(map[string]chan response)
		c.mu.Unlock()
		for _, ch := range waiters {
			select {
			case ch <- response{connErr: err}:
			default:
			}
		}
		close(c.closed)
		if c.conn != nil {
			_ = c.conn.Close()
		}
	})
}

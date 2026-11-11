package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
)

const streamFlushInterval = 120 * time.Millisecond

type connectionState struct {
	phase           string
	environmentID   string
	instanceID      string
	catalogLoading  bool
	catalogRevision uint64
}

func (a *app) onUI(update func() error) error {
	finished := make(chan error, 1)
	a.post(func() { finished <- update() })
	select {
	case err := <-finished:
		return err
	case <-a.done:
		return context.Canceled
	}
}

func (a *app) consumeConnection(client *daemon.Client, address, token string) {
	identity := client.Identity()
	for {
		err := a.consumeEvents(client)
		cursors := client.Cursors()
		client.Close()
		select {
		case <-a.done:
			return
		default:
		}
		if err != nil {
			_ = a.onUI(func() error {
				a.connection.phase = "recoveryRequired"
				a.status = err.Error()
				return nil
			})
			return
		}
		_ = a.onUI(func() error {
			a.connection.phase = "reconnecting"
			a.status = tr("Reconnecting to the daemon")
			return nil
		})
		delay := 250 * time.Millisecond
		for {
			timer := time.NewTimer(delay)
			select {
			case <-a.done:
				timer.Stop()
				return
			case <-timer.C:
			}
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			next, dialErr := daemon.DialWithResume(ctx, address, token, cursors)
			cancel()
			if dialErr != nil {
				delay = min(delay*2, 5*time.Second)
				continue
			}
			nextIdentity := next.Identity()
			if nextIdentity.EnvironmentID != identity.EnvironmentID || nextIdentity.InstanceID != identity.InstanceID {
				next.Close()
				_ = a.onUI(func() error {
					a.connection.phase = "recoveryRequired"
					a.status = tr("The daemon restarted. Reopen the app to restore saved sessions.")
					return nil
				})
				return
			}
			client = next
			if err := a.onUI(func() error {
				a.client = next
				a.connection.phase = "online"
				a.status = ""
				a.startNextSnapshot()
				return nil
			}); err != nil {
				next.Close()
				return
			}
			break
		}
	}
}

func (a *app) consumeEvents(client *daemon.Client) error {
	for {
		var first daemon.Event
		var ok bool
		select {
		case <-a.done:
			return nil
		case first, ok = <-client.Events():
			if !ok {
				return eventRecoveryError(client)
			}
		}
		batch := []daemon.Event{first}
		bytes := len(first.Payload)
		timer := time.NewTimer(streamFlushInterval)
		closed := false
	collect:
		for len(batch) < 256 && bytes < 4*1024*1024 {
			select {
			case <-a.done:
				timer.Stop()
				return nil
			case event, open := <-client.Events():
				if !open {
					closed = true
					break collect
				}
				batch = append(batch, event)
				bytes += len(event.Payload)
			case <-timer.C:
				break collect
			}
		}
		timer.Stop()
		if err := a.prepareEventBatch(client, batch); err != nil {
			return err
		}
		if err := a.onUI(func() error { return a.applyEventBatch(client, batch) }); err != nil {
			return err
		}
		if closed {
			return eventRecoveryError(client)
		}
	}
}

func eventRecoveryError(client *daemon.Client) error {
	if errors.Is(client.Err(), daemon.ErrSequenceGap) {
		return fmt.Errorf("Session event history is incomplete. Reconnect could not recover every event: %w", client.Err())
	}
	return nil
}

func (a *app) applyEventBatch(client *daemon.Client, batch []daemon.Event) error {
	a.applyingEvents = true
	defer func() {
		a.applyingEvents = false
		if a.transcriptDirty {
			a.transcriptDirty = false
			a.rebuildTranscript()
		}
		if a.sidebarDirty {
			a.sidebarDirty = false
			a.rebuildRows()
		}
	}()
	for _, event := range batch {
		if event.Type == "epochChanged" {
			return fmt.Errorf("The session runtime restarted. Saved history cannot recover every live event.")
		}
		if event.Type != "event" && event.Type != "" {
			a.handleDaemonNotice(event)
			continue
		}
		session := a.eventSession(event.SessionID)
		if session == nil {
			return fmt.Errorf("Cannot restore events for an unloaded session. Reopen the session before reconnecting.")
		}
		state := a.stateForSession(session)
		if a.historyRejectsEvent(event) {
			client.Ack(event)
			continue
		}
		if event.Epoch != "" && event.Sequence != 0 {
			previous := state.cursors[event.RuntimeID]
			if previous.Epoch == event.Epoch && event.Sequence <= previous.Sequence {
				client.Ack(event)
				continue
			}
			if previous.Epoch != "" && previous.Epoch != event.Epoch {
				return fmt.Errorf("The session runtime restarted. Saved history cannot recover every live event.")
			}
			if event.Sequence != previous.Sequence+1 {
				return fmt.Errorf("Session event history is incomplete. The last applied event is %d, but %d arrived.", previous.Sequence, event.Sequence)
			}
		}
		if err := a.applyEvent(event); err != nil {
			return err
		}
		if event.Epoch != "" && event.Sequence != 0 {
			state.cursors[event.RuntimeID] = daemon.ReplayCursor{
				SessionID: event.SessionID, RuntimeID: event.RuntimeID,
				Epoch: event.Epoch, Sequence: event.Sequence,
			}
			session.RuntimeEventCursor = &model.RuntimeEventCursor{RuntimeID: event.RuntimeID, Epoch: event.Epoch, Sequence: event.Sequence}
		}
		client.Ack(event)
		if event.Kind == "turnFinished" || event.Kind == "scheduledTurnAdmitted" {
			a.saveSessionSnapshot(session)
			if event.Kind == "turnFinished" && lastTurnSettledCleanly(session) {
				a.post(func() { a.drainQueuedSubmission(session) })
			}
		} else {
			a.maybeSaveStreaming(session)
		}
	}
	return nil
}

func (a *app) handleDaemonNotice(event daemon.Event) {
	switch event.Type {
	case "taskStateChanged":
		var notice struct {
			Revision uint64 `json:"revision"`
		}
		if json.Unmarshal(event.Payload, &notice) == nil && notice.Revision > a.connection.catalogRevision {
			a.connection.catalogRevision = notice.Revision
			a.refreshCatalog()
		}
	case "scheduledTasksChanged":
		// The daemon's scheduled catalog changed elsewhere; refresh the
		// roster when the page is open so enable/run/delete stay honest.
		if a.mode == modeScheduled {
			a.loadSettingsScheduled()
		}
	case "browserRequest":
		a.status = tr("Browser tools are not enabled")
	case "browserCancel", "browserShareRejected":
	case "shuttingDown":
		a.connection.phase = "reconnecting"
		a.status = tr("The daemon is shutting down")
	default:
		a.status = tr("Unsupported daemon message: ") + event.Type
	}
}

package main

import (
	"context"
	"time"

	"fintwind-go/internal/daemon"
	"github.com/egoist/mygo/ui"
)

type contextCompactRequest struct {
	client   *daemon.Client
	identity daemon.Identity
	runtime  string
	pending  bool
	err      string
}

func (a *app) currentContextCompact() *contextCompactRequest {
	if a.detail == nil {
		return nil
	}
	state := a.live[a.detail.ID]
	if state == nil {
		return nil
	}
	request := state.contextCompact
	if request == nil || request.client != a.client || a.client == nil || request.identity != a.client.Identity() || request.runtime != a.runtimes[a.detail.ID] {
		return nil
	}
	return request
}

func (a *app) contextCompactControl(c *ui.Context) {
	request := a.currentContextCompact()
	state := a.detail.Compaction
	running := state != nil && state.Status == "running"
	failed := state != nil && state.Status == "failed"
	label, message := "Compact context", ""
	if running || request != nil && request.pending {
		label = "Compacting context…"
	} else if failed || request != nil && request.err != "" {
		label = "Retry compaction"
		message = tr("Compaction failed")
		if failed && state.Error != nil {
			message = *state.Error
		}
		if request != nil && request.err != "" {
			message = request.err
		}
	}
	if message != "" {
		ui.Text(c, message).FontSize(uiPx(11)).TextColor(palette.warningText).Wrap()
	}
	contextAction(c, "context-compact", label).Disabled(!a.canCompactContext()).OnClick(a.requestContextCompaction)
	if a.runtimes[a.detail.ID] == "" {
		ui.Text(c, tr("Compaction unavailable")).FontSize(uiPx(11)).TextColor(palette.textTertiary)
	}
}

func (a *app) canCompactContext() bool {
	if a.detail == nil || a.client == nil || a.connection.phase != "online" || a.lifetime == nil || a.sessionActionBlocksInput(a.detail.ID) {
		return false
	}
	if a.runtimes[a.detail.ID] == "" || a.detail.Compaction != nil && a.detail.Compaction.Status == "running" {
		return false
	}
	if pending := a.currentContextCompact(); pending != nil && pending.pending {
		return false
	}
	return true
}

func (a *app) requestContextCompaction() {
	if !a.canCompactContext() {
		return
	}
	sessionID, runtime := a.detail.ID, a.runtimes[a.detail.ID]
	request := &contextCompactRequest{client: a.client, identity: a.client.Identity(), runtime: runtime, pending: true}
	a.stateForSession(a.detail).contextCompact = request
	lifetime := a.lifetime
	current := func() bool {
		state := a.live[sessionID]
		return state != nil && state.contextCompact == request && a.client == request.client &&
			request.client.Identity() == request.identity && a.runtimes[sessionID] == runtime
	}
	go func() {
		ctx, cancel := context.WithTimeout(lifetime, 15*time.Second)
		defer cancel()
		payload, err := request.client.RequestFor(ctx, sessionID, runtime, map[string]any{"type": "compactSession"})
		confirmed := err == nil && sessionOperationAck(payload)
		if confirmed {
			a.post(func() {
				if current() && a.selectedID == sessionID {
					a.status = tr("Context compaction requested")
				}
			})
			// The event is authoritative; the ack alone must not reopen the action.
			timer := time.NewTimer(15 * time.Second)
			defer timer.Stop()
			select {
			case <-lifetime.Done():
				return
			case <-timer.C:
			}
		}
		a.post(func() {
			if !current() {
				return
			}
			request.pending = false
			request.err = tr("Compaction was not confirmed. Check the connection before retrying.")
			if err != nil {
				request.err += " " + err.Error()
			}
			if a.selectedID == sessionID {
				a.status = request.err
			}
		})
	}()
}

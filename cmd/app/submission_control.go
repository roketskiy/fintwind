package main

import (
	"context"
	"encoding/json"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

// submission_control.go owns the worker half of atomic session submission.
// The UI half (startIdleTurn in submissions.go) builds the running turn, the
// frozen SubmissionRecord and the accept admission, then a worker drives
// this pipeline: wait for the accept future, apply the admitted receipt,
// ensure the runtime, recheck the turn on the UI thread, then dispatch once
// by submission id. No Prompt RPC exists on this path: the daemon admits by
// identity (AcceptSubmission) and executes at most once (DispatchSubmission
// with a Preparing CAS), so the old snapshot-then-Prompt TOCTOU is gone.
//
// Every UI mutation rides a.post or a.onUI. Every RPC carries a deadline
// and derives from a.lifetime, so closing the window cancels the flight.
// Nothing here retries a send: an unknown result keeps the turn visible and
// unconfirmed and asks the user to check the session.

const (
	submissionAcceptWaitTimeout = 90 * time.Second
	submissionDispatchTimeout   = 60 * time.Second
	submissionAbandonTimeout    = 30 * time.Second
	submissionCancelTimeout     = 15 * time.Second
)

// decodeSubmissionSaved reads a submissionSaved response payload and returns
// the receipt plus its raw bytes for the existing receipt helper. It only
// accepts the envelope for the expected turn with a non-empty input id.
func decodeSubmissionSaved(payload json.RawMessage, turnID string) (model.SubmissionReceipt, json.RawMessage, bool) {
	var receipt model.SubmissionReceipt
	var envelope struct {
		Type    string          `json:"type"`
		Receipt json.RawMessage `json:"receipt"`
	}
	if json.Unmarshal(payload, &envelope) != nil || envelope.Type != "submissionSaved" {
		return receipt, nil, false
	}
	if json.Unmarshal(envelope.Receipt, &receipt) != nil || receipt.ID != turnID || receipt.InputID == "" {
		return receipt, nil, false
	}
	switch receipt.State {
	case model.SubmissionPreparing, model.SubmissionDispatching, model.SubmissionAccepted, model.SubmissionUnknown, model.SubmissionNotSent, model.SubmissionRejected:
	default:
		return receipt, nil, false
	}
	return receipt, envelope.Receipt, true
}

// runSubmissionWorker drives one admitted send to a single dispatch. It is
// the only place that sends DispatchSubmission for a user submission.
func (a *app) runSubmissionWorker(client *daemon.Client, sessionID, turnID string, record model.SubmissionRecord, launch runtimeLaunch, admission *submissionAdmission) {
	if client == nil {
		return
	}
	_, receiptRaw, err := a.awaitSubmissionAccept(admission, turnID, record.Receipt.InputID)
	if err != nil {
		a.abandonUnclaimedSubmission(client, sessionID, turnID, admission, "The send was not admitted: "+err.Error())
		return
	}
	a.post(func() {
		if session := a.eventSession(sessionID); session != nil {
			a.applySubmissionUpdated(session, receiptRaw)
		}
	})
	runCtx, cancel := context.WithTimeout(a.lifetime, 120*time.Second)
	defer cancel()
	runtimeID, err := ensureRuntime(runCtx, client, launch)
	if err != nil {
		a.abandonUnclaimedSubmission(client, sessionID, turnID, admission, "Could not start the agent runtime: "+err.Error())
		return
	}
	claimed := false
	if err := a.onUI(func() error {
		target := a.eventSession(sessionID)
		if target == nil {
			return nil
		}
		if a.stateForSession(target).stopped || target.ActiveTurnID() != turnID {
			return nil
		}
		a.runtimes[sessionID] = runtimeID
		a.stateForSession(target).preparing = false
		claimed = true
		return nil
	}); err != nil || !claimed {
		a.abandonUnclaimedSubmission(client, sessionID, turnID, admission, "")
		return
	}
	dispatchCtx, cancelDispatch := context.WithTimeout(a.lifetime, submissionDispatchTimeout)
	defer cancelDispatch()
	payload, err := client.RequestFor(dispatchCtx, sessionID, runtimeID, map[string]any{
		"type":         "dispatchSubmission",
		"submissionId": turnID,
		"workspace":    map[string]any{"kind": "local"},
	})
	if err != nil {
		a.post(func() {
			a.markSubmissionUnconfirmed(sessionID, turnID, admission, "The send reached an unknown state and was not retried: "+err.Error()+". Check the session before sending again.")
		})
		return
	}
	if receipt, raw, ok := decodeSubmissionSaved(payload, turnID); ok && receipt.InputID == record.Receipt.InputID {
		a.post(func() {
			session := a.eventSession(sessionID)
			if session == nil {
				return
			}
			a.applySubmissionUpdated(session, raw)
			a.releaseSubmissionAdmission(session, admission)
		})
		return
	}
	a.post(func() {
		a.markSubmissionUnconfirmed(sessionID, turnID, admission, "The send reached an unknown state and was not retried. Check the session before sending again.")
	})
}

// awaitSubmissionAccept waits for the accept future, the watchdog or app
// close, then confirms the admitted identity: same turn, same input, still
// Preparing. Anything else leaves the outcome unknown to the caller.
func (a *app) awaitSubmissionAccept(admission *submissionAdmission, turnID, inputID string) (model.SubmissionReceipt, json.RawMessage, error) {
	var receipt model.SubmissionReceipt
	timer := time.NewTimer(submissionAcceptWaitTimeout)
	defer timer.Stop()
	select {
	case <-admission.done:
	case <-timer.C:
		return receipt, nil, context.DeadlineExceeded
	case <-a.done:
		return receipt, nil, context.Canceled
	}
	if admission.err != nil {
		return receipt, nil, admission.err
	}
	admitted, raw, ok := decodeSubmissionSaved(admission.payload, turnID)
	if !ok || admitted.InputID != inputID || admitted.State != model.SubmissionPreparing {
		return receipt, nil, context.DeadlineExceeded
	}
	return admitted, raw, nil
}

// abandonUnclaimedSubmission revokes an admission that never reached the
// provider. It is safe after a refused or unknown accept and after a stop:
// the daemon only transitions Preparing into NotSent, so a claimed dispatch
// is never undone here. An unknown abandon result never fabricates NotSent;
// the turn stays visible and unconfirmed. failText empty means the turn was
// already settled by a stop, so only the admission is released.
func (a *app) abandonUnclaimedSubmission(client *daemon.Client, sessionID, turnID string, admission *submissionAdmission, failText string) {
	if client == nil {
		if failText != "" {
			a.post(func() { a.failSubmissionTurnIfOurs(sessionID, turnID, admission, failText) })
		}
		return
	}
	ctx, cancel := context.WithTimeout(a.lifetime, submissionAbandonTimeout)
	defer cancel()
	payload, err := client.RequestFor(ctx, sessionID, "", map[string]any{
		"type":         "abandonSubmission",
		"submissionId": turnID,
	})
	if err != nil {
		a.post(func() {
			a.markSubmissionUnconfirmed(sessionID, turnID, admission, "Could not revoke the unclaimed send and was not retried: "+err.Error()+". Check the session before sending again.")
		})
		return
	}
	receipt, raw, ok := decodeSubmissionSaved(payload, turnID)
	if !ok {
		a.post(func() {
			a.markSubmissionUnconfirmed(sessionID, turnID, admission, "The send could not be confirmed. Check the session before sending again.")
		})
		return
	}
	a.post(func() {
		if session := a.eventSession(sessionID); session != nil {
			a.applySubmissionUpdated(session, raw)
			if failText != "" && !receipt.Terminal() {
				a.markSubmissionUnconfirmed(sessionID, turnID, admission, failText)
				return
			}
			if failText != "" && a.detail == session {
				a.status = failText
			}
			a.releaseSubmissionAdmission(session, admission)
		}
	})
}

// stopSessionSubmission revokes an in-flight admission after the UI already
// marked the turn stopped and interrupted. It returns immediately: the
// accept wait, the abandon and the conditional cancel all run on a worker.
// The abandon rides the fixed turn id without re-sending Accept, and a
// cancel follows in the same worker only when the receipt proves a dispatch
// was claimed (dispatching, accepted or unknown). A NotSent or Rejected
// receipt needs no cancel, and an unknown abandon never claims NotSent.
//
// Must be called on the UI thread: it reads the live admission pointer
// without a queue hop. cancelTurn is the only caller.
func (a *app) stopSessionSubmission(sessionID, turnID, runtimeID string) {
	client := a.client
	if client == nil || sessionID == "" {
		return
	}
	var admission *submissionAdmission
	if state := a.live[sessionID]; state != nil {
		admission = state.admission
	}
	go func() {
		if admission == nil || turnID == "" {
			a.cancelSubmissionRuntime(client, sessionID, runtimeID)
			return
		}
		ctx, cancel := context.WithTimeout(a.lifetime, submissionAcceptWaitTimeout+submissionAbandonTimeout)
		defer cancel()
		select {
		case <-admission.done:
		case <-ctx.Done():
			return
		case <-a.done:
			return
		}
		abandonCtx, cancelAbandon := context.WithTimeout(a.lifetime, submissionAbandonTimeout)
		payload, err := client.RequestFor(abandonCtx, sessionID, "", map[string]any{
			"type":         "abandonSubmission",
			"submissionId": turnID,
		})
		cancelAbandon()
		if err != nil {
			a.post(func() {
				a.markSubmissionUnconfirmed(sessionID, turnID, admission, "The stop reached an unknown state and was not retried: "+err.Error()+". Check the session before sending again.")
			})
			return
		}
		receipt, raw, ok := decodeSubmissionSaved(payload, turnID)
		if !ok {
			a.post(func() {
				a.markSubmissionUnconfirmed(sessionID, turnID, admission, "The stop reached an unknown state and was not retried. Check the session before sending again.")
			})
			return
		}
		a.post(func() {
			if session := a.eventSession(sessionID); session != nil {
				a.applySubmissionUpdated(session, raw)
				if receipt.Terminal() {
					a.releaseSubmissionAdmission(session, admission)
				}
			}
		})
		switch receipt.State {
		case model.SubmissionDispatching, model.SubmissionAccepted, model.SubmissionUnknown:
			a.cancelSubmissionRuntime(client, sessionID, runtimeID)
		}
	}()
}

// cancelSubmissionRuntime stops the provider runtime for a session. It never
// retries and never touches transcript state.
func (a *app) cancelSubmissionRuntime(client *daemon.Client, sessionID, runtimeID string) {
	if client == nil || sessionID == "" || runtimeID == "" {
		return
	}
	ctx, cancel := context.WithTimeout(a.lifetime, submissionCancelTimeout)
	defer cancel()
	if _, err := client.RequestFor(ctx, sessionID, runtimeID, map[string]any{"type": "cancel"}); err != nil {
		text := err.Error()
		a.post(func() {
			if a.detail != nil && a.detail.ID == sessionID {
				a.status = tr("Could not confirm the stop request: ") + text
			}
		})
	}
}

// failSubmissionTurnIfOurs settles the turn as failed only while it is still
// the session's last running turn with a matching id. A stale worker error
// never fails a newer turn, and a receipt that already left Preparing keeps
// the turn unconfirmed instead. Runs on the UI thread.
func (a *app) failSubmissionTurnIfOurs(sessionID, turnID string, admission *submissionAdmission, text string) {
	session := a.eventSession(sessionID)
	if session == nil {
		return
	}
	defer a.releaseSubmissionAdmission(session, admission)
	turn := session.TurnByID(turnID)
	if turn == nil || turn.Status != "running" {
		return
	}
	if last := len(session.Turns) - 1; last < 0 || session.Turns[last].ID != turnID {
		return
	}
	if len(turn.Submission) > 0 {
		var receipt model.SubmissionReceipt
		if json.Unmarshal(turn.Submission, &receipt) == nil && receipt.ID == turnID && receipt.Unconfirmed() {
			a.markSubmissionUnconfirmed(sessionID, turnID, admission, text)
			return
		}
	}
	completed := uint64(time.Now().Unix())
	turn.Status = "failed"
	turn.CompletedAt = &completed
	session.Status = "idle"
	session.Messages = append(session.Messages, model.Message{
		ID:        uuid.NewString(),
		Role:      "system",
		Content:   text,
		CreatedAt: uint64(time.Now().Unix()),
	})
	state := a.stateForSession(session)
	state.preparing = false
	if a.detail == session {
		a.rebuildTranscript()
	}
	a.syncSummary(session)
	a.rebuildRows()
}

// markSubmissionUnconfirmed keeps an unknown send visible without
// fabricating a terminal receipt: the turn stays where the events can still
// reconcile it, and the user is told not to resend. It posts the system
// message whenever the turn still exists, running or stopped. Runs on the
// UI thread.
func (a *app) markSubmissionUnconfirmed(sessionID, turnID string, admission *submissionAdmission, text string) {
	session := a.eventSession(sessionID)
	if session == nil || session.TurnByID(turnID) == nil {
		return
	}
	if last := len(session.Turns) - 1; last < 0 || session.Turns[last].ID != turnID {
		a.releaseSubmissionAdmission(session, admission)
		return
	}
	session.Messages = append(session.Messages, model.Message{
		ID:        uuid.NewString(),
		Role:      "system",
		Content:   text,
		CreatedAt: uint64(time.Now().Unix()),
	})
	a.tabUnread[sessionID] = a.detail != session
	if a.detail == session {
		a.status = text
		a.rebuildTranscript()
	}
	a.syncSummary(session)
	a.rebuildRows()
	a.releaseSubmissionAdmission(session, admission)
}

// releaseSubmissionAdmission drops the finished admission and replays a
// snapshot deferred behind it, but only while the admission still owns the
// session. A superseding admission keeps its own busy flag. Runs on the UI
// thread.
func (a *app) releaseSubmissionAdmission(session *model.Session, admission *submissionAdmission) {
	if session == nil {
		return
	}
	state := a.stateForSession(session)
	if admission != nil && state.admission != admission {
		return
	}
	state.admission = nil
	a.finishSubmissionAdmission(session)
}

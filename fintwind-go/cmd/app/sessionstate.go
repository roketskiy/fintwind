package main

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

type sessionState struct {
	session              *model.Session
	raw                  map[string]any
	draft                string
	attachments          []draftAttachment
	permission           *pendingPermission
	question             *userInputState
	list                 ui.ListState
	expandedTurns        map[string]bool
	clusterDisclosure    map[int]int
	expandedActivities   map[string]bool
	expandedMessages     map[string]bool
	expandedCompaction   map[string]bool
	expandedFiles        map[string]bool
	cursors              map[string]daemon.ReplayCursor
	lastSnapshot         time.Time
	unsaved              bool
	uploads              map[string]bool
	textParts            map[string]int
	reasoningParts       map[string]activityPosition
	extension            map[string]any
	continuationStart    *transcriptPosition
	preparing            bool
	admission            *submissionAdmission
	admissionBusy        bool
	stopped              bool
	reconciliationFailed bool
	turnStartedAt        time.Time
	firstContentAt       time.Time
	edit                 *messageEditState
	retiredRuntimes      map[string]bool
	contextCompact       *contextCompactRequest
}

type transcriptPosition struct{ messages, blocks int }

type activityPosition struct{ block, item int }

func (a *app) stateForSession(session *model.Session) *sessionState {
	if a.live == nil {
		a.live = make(map[string]*sessionState)
	}
	state := a.live[session.ID]
	if state == nil {
		state = &sessionState{
			cursors:            make(map[string]daemon.ReplayCursor),
			list:               ui.ListState{FollowEnd: true},
			expandedTurns:      make(map[string]bool),
			clusterDisclosure:  make(map[int]int),
			expandedActivities: make(map[string]bool),
			expandedMessages:   make(map[string]bool),
			expandedCompaction: make(map[string]bool),
			expandedFiles:      make(map[string]bool),
		}
		if cursor := session.RuntimeEventCursor; cursor != nil {
			state.cursors[cursor.RuntimeID] = daemon.ReplayCursor{SessionID: session.ID, RuntimeID: cursor.RuntimeID, Epoch: cursor.Epoch, Sequence: cursor.Sequence}
		}
		a.live[session.ID] = state
	}
	state.session = session
	return state
}

func (a *app) prepareEventBatch(client *daemon.Client, batch []daemon.Event) error {
	unknown := make(map[string]bool)
	if err := a.onUI(func() error {
		for _, event := range batch {
			if (event.Type == "event" || event.Type == "") && a.eventSession(event.SessionID) == nil {
				unknown[event.SessionID] = true
			}
		}
		return nil
	}); err != nil {
		return err
	}
	for id := range unknown {
		ctx, cancel := context.WithTimeout(a.lifetime, 30*time.Second)
		payload, err := client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": id, "environmentId": "00000000-0000-0000-0000-000000000000"})
		cancel()
		if err != nil {
			return fmt.Errorf("restore an incoming session: %w", err)
		}
		var detail model.SessionDetail
		if err := json.Unmarshal(payload, &detail); err != nil {
			return err
		}
		if detail.Session == nil {
			return fmt.Errorf("the daemon sent events for an unavailable session")
		}
		if err := a.onUI(func() error {
			if a.eventSession(id) != nil {
				return nil
			}
			state := a.stateForSession(detail.Session)
			var holder struct {
				Session map[string]any `json:"session"`
			}
			if json.Unmarshal(payload, &holder) == nil {
				state.raw = holder.Session
			}
			a.refreshCatalog()
			return nil
		}); err != nil {
			return err
		}
	}
	return nil
}

func (a *app) rememberSession() {
	if a.detail == nil {
		return
	}
	state := a.stateForSession(a.detail)
	if a.editMessage != nil {
		a.editMessage.Draft = a.editDraft
	}
	state.edit = a.editMessage
	state.raw = a.detailRaw
	state.draft = a.draft
	state.attachments = a.pendingAttachments
	state.permission = a.pendingPermission
	state.question = a.pendingUserInput
	state.list = a.messageList
	state.expandedTurns = a.expandedTurns
	state.clusterDisclosure = a.clusterDisclosure
	state.expandedActivities = a.expandedActivities
	state.expandedMessages = a.expandedUserMessages
	state.expandedCompaction = a.expandedCompaction
	state.expandedFiles = a.expandedChangedFiles
	state.unsaved = a.unsavedDraft == a.detail.ID
}

func (a *app) restoreSession(id string) bool {
	state := a.live[id]
	if state == nil || state.session == nil {
		return false
	}
	a.detail = state.session
	a.editMessage = state.edit
	a.editDraft = ""
	if state.edit != nil {
		a.editDraft = state.edit.Draft
	}
	a.detailRaw = state.raw
	a.draft = state.draft
	a.pendingAttachments = state.attachments
	a.pendingPermission = state.permission
	a.pendingUserInput = state.question
	a.messageList = state.list
	a.expandedTurns = state.expandedTurns
	a.clusterDisclosure = state.clusterDisclosure
	a.expandedActivities = state.expandedActivities
	a.expandedUserMessages = state.expandedMessages
	a.expandedCompaction = state.expandedCompaction
	a.expandedChangedFiles = state.expandedFiles
	a.unsavedDraft = ""
	if state.unsaved {
		a.unsavedDraft = id
	}
	return true
}

func (a *app) markSessionSaved(id string) {
	if state := a.live[id]; state != nil {
		state.unsaved = false
	}
	if a.unsavedDraft == id {
		a.unsavedDraft = ""
	}
}

func (a *app) resetSessionView() {
	a.detailRaw = nil
	a.draft = ""
	a.pendingAttachments = nil
	a.pendingPermission = nil
	a.pendingUserInput = nil
	a.editMessage = nil
	a.editDraft = ""
	a.unsavedDraft = ""
	a.expandedTurns = map[string]bool{}
	a.clusterDisclosure = map[int]int{}
	a.expandedActivities = map[string]bool{}
	a.expandedUserMessages = map[string]bool{}
	a.expandedCompaction = map[string]bool{}
	a.expandedChangedFiles = map[string]bool{}
	a.messageList = ui.ListState{FollowEnd: true}
}

func (a *app) eventSession(id string) *model.Session {
	if a.detail != nil && a.detail.ID == id {
		return a.detail
	}
	if state := a.live[id]; state != nil {
		return state.session
	}
	return nil
}

func (a *app) refreshCatalog() {
	if a.client == nil || a.connection.catalogLoading {
		return
	}
	a.connection.catalogLoading = true
	revision := a.connection.catalogRevision
	client := a.client
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		payload, err := client.Request(ctx, map[string]any{"type": "loadTaskState"})
		var state model.TaskState
		if err == nil {
			err = json.Unmarshal(payload, &state)
		}
		a.post(func() {
			a.connection.catalogLoading = false
			if err != nil {
				a.status = tr("Could not refresh sessions: ") + err.Error()
				return
			}
			if a.client != client {
				return
			}
			present := make(map[string]bool, len(state.Sessions))
			for _, session := range state.Sessions {
				present[session.ID] = true
			}
			for _, session := range a.sessions {
				if session.Draft && !present[session.ID] {
					state.Sessions = append(state.Sessions, session)
				}
			}
			a.projects = state.Projects
			a.sessions = state.Sessions
			a.projectlessRoot = state.ProjectlessRoot
			a.byID = make(map[string]*model.Session, len(a.sessions))
			for index := range a.sessions {
				a.byID[a.sessions[index].ID] = &a.sessions[index]
			}
			for _, live := range a.live {
				if live.session != nil {
					a.syncSummary(live.session)
				}
			}
			if a.detail != nil {
				a.syncSummary(a.detail)
			}
			a.rebuildRows()
			if revision != a.connection.catalogRevision {
				a.refreshCatalog()
			}
		})
	}()
}

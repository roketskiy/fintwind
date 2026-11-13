package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"strings"
	"time"
	"unicode/utf8"

	"fintwind-go/internal/backendremote"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

type remoteState struct {
	generation uint64
	activation *backendremote.Activation
	scope      backendremote.Scope
	status     string
}

func (a *app) startRemoteControl() error {
	if a.remote.activation != nil || a.client == nil || a.connection.phase != "online" {
		return errors.New("remote control is unavailable")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	a.remote.generation++
	generation := a.remote.generation
	a.remote.scope = backendremote.NewScope(a.projects)
	a.remote.status = "connecting"
	activation, err := backendremote.Start(a.lifetime, home, func(ctx context.Context, raw json.RawMessage) (any, error) {
		return a.remoteCommand(ctx, generation, raw)
	}, func(status string) {
		a.post(func() {
			if a.remote.generation == generation && a.remote.activation != nil {
				a.remote.status = status
			}
		})
	})
	if err != nil {
		a.remote.status = "failed"
		return err
	}
	a.remote.activation = activation
	return nil
}

func (a *app) stopRemoteControl() {
	a.remote.generation++
	if a.remote.activation != nil {
		a.remote.activation.Stop()
		a.remote.activation = nil
	}
	a.remote.status = "off"
}

func (a *app) onRemoteUI(ctx context.Context, update func() error) error {
	finished := make(chan error, 1)
	a.post(func() {
		if err := ctx.Err(); err != nil {
			finished <- err
			return
		}
		finished <- update()
	})
	select {
	case err := <-finished:
		return err
	case <-ctx.Done():
		return ctx.Err()
	case <-a.done:
		return context.Canceled
	}
}

func remoteText(text string, limit int) string {
	text = strings.ToValidUTF8(text, "�")
	if len(text) <= limit {
		return text
	}
	for limit > 0 && !utf8.RuneStart(text[limit]) {
		limit--
	}
	return text[:limit]
}

func (a *app) remoteCommand(ctx context.Context, generation uint64, raw json.RawMessage) (any, error) {
	var request struct {
		Type          string   `json:"type"`
		ProjectID     string   `json:"projectId"`
		SessionID     string   `json:"sessionId"`
		Text          string   `json:"text"`
		AttachmentIDs []string `json:"attachmentIds"`
		RequestID     string   `json:"requestId"`
		OptionID      string   `json:"optionId"`
		Answers       []struct {
			QuestionID string   `json:"questionId"`
			Answers    []string `json:"answers"`
		} `json:"answers"`
	}
	var fields map[string]json.RawMessage
	if len(raw) > 1<<20 || json.Unmarshal(raw, &request) != nil || json.Unmarshal(raw, &fields) != nil {
		return nil, errors.New("invalid request")
	}
	allowed := map[string]map[string]bool{
		"listWorkspaces":    {"type": true},
		"listSessions":      {"type": true, "projectId": true},
		"createSession":     {"type": true, "projectId": true},
		"getSession":        {"type": true, "sessionId": true},
		"sendPrompt":        {"type": true, "sessionId": true, "text": true, "attachmentIds": true},
		"stop":              {"type": true, "sessionId": true},
		"respondPermission": {"type": true, "sessionId": true, "requestId": true, "optionId": true},
		"answerQuestions":   {"type": true, "sessionId": true, "requestId": true, "answers": true},
	}
	allowedFields, ok := allowed[request.Type]
	if !ok {
		return nil, errors.New("unsupported remote command")
	}
	for field := range fields {
		if !allowedFields[field] {
			return nil, errors.New("invalid request")
		}
	}
	projectCommand := request.Type == "listSessions" || request.Type == "createSession"
	if (projectCommand && (request.ProjectID == "" || len(request.ProjectID) > 256)) ||
		(request.Type != "listWorkspaces" && !projectCommand && (request.SessionID == "" || len(request.SessionID) > 256)) ||
		(request.Type == "sendPrompt" && (strings.TrimSpace(request.Text) == "" || len(request.Text) > 32768 || len(request.AttachmentIDs) != 0)) ||
		((request.Type == "respondPermission" || request.Type == "answerQuestions") && (request.RequestID == "" || len(request.RequestID) > 256)) ||
		(request.Type == "respondPermission" && (request.OptionID == "" || len(request.OptionID) > 256)) {
		return nil, errors.New("invalid request")
	}
	var result any
	var client *daemon.Client
	var original *model.Session
	err := a.onRemoteUI(ctx, func() error {
		if ctx.Err() != nil || a.remote.generation != generation || a.remote.activation == nil {
			return errors.New("remote control is stopped")
		}
		switch request.Type {
		case "listWorkspaces":
			selected := ""
			if project := a.selectedProject(); project != nil {
				selected = project.ID
			}
			result = a.remote.scope.Workspaces(a.projects, selected)
			return nil
		case "listSessions":
			var err error
			result, err = a.remote.scope.Sessions(a.projects, a.sessions, request.ProjectID, time.Now())
			return err
		case "createSession":
			if _, err := a.remote.scope.Project(a.projects, request.ProjectID); err != nil {
				return err
			}
			if a.client == nil || a.connection.phase != "online" {
				return errors.New("session creation is unavailable")
			}
			now := uint64(time.Now().Unix())
			session := &model.Session{
				ID: uuid.NewString(), Title: "New task", ProjectID: request.ProjectID,
				Provider: "opencode", RuntimeMode: "fullAccess", InteractionMode: "build",
				Status: "idle", CreatedAt: now, UpdatedAt: now,
			}
			a.sessions = append(a.sessions, *session)
			a.byID = nil
			a.stateForSession(session)
			a.saveSessionSnapshot(session)
			a.rebuildRows()
			result = map[string]any{"sessionId": session.ID}
			return nil
		case "getSession":
			summary, err := a.remote.scope.Session(a.projects, a.sessions, request.SessionID, time.Now())
			if err != nil {
				return err
			}
			if session := a.eventSession(request.SessionID); session != nil {
				result = a.remoteSessionSnapshot(session)
				return nil
			}
			if a.client == nil || summary.Busy() {
				return errors.New("session history is unavailable")
			}
			copy := *summary
			original, client = &copy, a.client
			return nil
		case "sendPrompt", "stop", "respondPermission", "answerQuestions":
			if _, err := a.remote.scope.Session(a.projects, a.sessions, request.SessionID, time.Now()); err != nil {
				return err
			}
			session := a.eventSession(request.SessionID)
			if session == nil || a.client == nil || a.connection.phase != "online" {
				return errors.New("session is unavailable")
			}
			switch request.Type {
			case "sendPrompt":
				if !a.submitSessionSubmission(sessionSubmission{sessionID: session.ID, text: request.Text}) {
					return errors.New("submission was not accepted")
				}
			case "stop":
				if !session.Busy() || a.stateForSession(session).admissionBusy {
					return errors.New("session cannot be stopped")
				}
				a.cancelTurnForSession(session)
			case "respondPermission":
				pending := a.stateForSession(session).permission
				if pending == nil || pending.RequestID != request.RequestID || pending.Response.Phase != "" || a.runtimes[session.ID] == "" {
					return errors.New("permission request is unavailable")
				}
				valid := false
				for _, option := range pending.Options {
					valid = valid || option.ID == request.OptionID
				}
				if !valid {
					return errors.New("permission option is unavailable")
				}
				a.sendSessionReply(session.ID, a.runtimes[session.ID], &pending.Response, map[string]any{
					"type": "respond", "requestId": pending.RequestID, "optionId": request.OptionID,
				}, func() { a.clearSessionPermission(session.ID, pending) })
				if pending.Response.Phase != "sending" {
					return errors.New("permission response was not accepted")
				}
			case "answerQuestions":
				pending := a.stateForSession(session).question
				if pending == nil || pending.RequestID != request.RequestID || pending.Response.Phase != "" || a.runtimes[session.ID] == "" || len(pending.Questions) == 0 || len(pending.Questions) > 32 || len(request.Answers) != len(pending.Questions) {
					return errors.New("questions are unavailable")
				}
				answers := make([]map[string]any, 0, len(request.Answers))
				total := 0
				for index, question := range pending.Questions {
					answer := request.Answers[index]
					if answer.QuestionID != question.ID || len(answer.Answers) == 0 || len(answer.Answers) > 32 || (!question.MultiSelect && len(answer.Answers) != 1) {
						return errors.New("answers are invalid")
					}
					for _, text := range answer.Answers {
						total += len(text)
						if strings.TrimSpace(text) == "" || len(text) > 4096 || total > 65536 {
							return errors.New("answers are invalid")
						}
					}
					answers = append(answers, map[string]any{"questionId": question.ID, "answers": answer.Answers})
				}
				a.sendSessionReply(session.ID, a.runtimes[session.ID], &pending.Response, map[string]any{
					"type": "respondUserInput", "requestId": pending.RequestID, "answers": answers,
				}, func() { a.clearSessionQuestion(session.ID, pending) })
				if pending.Response.Phase != "sending" {
					return errors.New("answers were not accepted")
				}
			}
			result = map[string]any{"accepted": true}
			return nil
		default:
			return errors.New("unsupported remote command")
		}
	})
	if err != nil || original == nil {
		return result, err
	}
	session, rawSession, err := hydrateSessionOperation(ctx, client, original.ID)
	if err != nil {
		return nil, err
	}
	err = a.onRemoteUI(ctx, func() error {
		if ctx.Err() != nil || a.remote.generation != generation || a.remote.activation == nil || a.client != client {
			return errors.New("remote control is stopped")
		}
		summary, err := a.remote.scope.Session(a.projects, a.sessions, original.ID, time.Now())
		if err != nil {
			return err
		}
		if live := a.eventSession(original.ID); live != nil {
			result = a.remoteSessionSnapshot(live)
			return nil
		}
		if summary.ProjectID != original.ProjectID || summary.Status != original.Status || !summary.Updated().Equal(original.Updated()) || session.ProjectID != original.ProjectID {
			return errors.New("session changed while loading")
		}
		state := a.stateForSession(session)
		state.raw = rawSession
		result = a.remoteSessionSnapshot(session)
		return nil
	})
	return result, err
}

func (a *app) remoteSessionSnapshot(session *model.Session) map[string]any {
	messages, entries, truncated := backendremote.Transcript(session)
	state := a.stateForSession(session)
	var permission any
	desktopRequired := false
	var questions any
	if pending := state.question; pending != nil {
		valid := pending.RequestID != "" && len(pending.RequestID) <= 256 && len(pending.Questions) > 0 && len(pending.Questions) <= 32 && pending.Response.Phase == ""
		projected := make([]map[string]any, 0, min(32, len(pending.Questions)))
		for _, question := range pending.Questions[:min(32, len(pending.Questions))] {
			valid = valid && question.ID != "" && len(question.ID) <= 256 && len(question.Header) <= 1024 && len(question.Question) <= 4096 && len(question.Options) <= 64
			options := make([]map[string]any, 0, min(64, len(question.Options)))
			for _, option := range question.Options[:min(64, len(question.Options))] {
				description := ""
				if option.Description != nil {
					description = *option.Description
				}
				valid = valid && len(option.Label) <= 2048 && len(description) <= 4096
				options = append(options, map[string]any{"label": option.Label, "description": description})
			}
			projected = append(projected, map[string]any{"id": question.ID, "header": question.Header, "question": question.Question, "multiSelect": question.MultiSelect, "options": options})
		}
		if valid {
			questions = map[string]any{"requestId": pending.RequestID, "questions": projected}
		} else {
			desktopRequired = true
		}
	}
	if state.permission != nil {
		valid := state.permission.RequestID != "" && len(state.permission.RequestID) <= 256 && len(state.permission.Title) <= 4096 && len(state.permission.Options) <= 32
		options := make([]map[string]any, 0, min(32, len(state.permission.Options)))
		for _, option := range state.permission.Options[:min(32, len(state.permission.Options))] {
			valid = valid && option.ID != "" && len(option.ID) <= 256 && len(option.Label) <= 1024
			options = append(options, map[string]any{"id": option.ID, "label": option.Label, "allow": option.Allow})
		}
		if valid && state.permission.Response.Phase == "" {
			permission = map[string]any{"requestId": state.permission.RequestID, "title": state.permission.Title, "options": options}
		} else {
			desktopRequired = true
		}
	}
	modelName := ""
	if session.Model != nil {
		modelName = remoteText(*session.Model, 512)
	}
	return map[string]any{"id": session.ID, "projectId": session.ProjectID, "title": remoteText(session.DisplayTitle(), 1024), "status": session.Status,
		"messages": messages, "entries": entries, "features": []string{"orderedTranscript"}, "truncated": truncated,
		"permission": permission, "questions": questions, "desktopRequired": desktopRequired, "canSend": a.client != nil && a.connection.phase == "online" && !state.admissionBusy && !a.sessionActionBlocksInput(session.ID),
		"model": modelName, "modelName": modelName, "interactionMode": session.InteractionMode}
}

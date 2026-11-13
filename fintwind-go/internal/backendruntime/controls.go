package backendruntime

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"time"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendservices"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"
)

func uint64Now() uint64 { return uint64(time.Now().UnixMilli()) }

func (a *actor) controls(ctx context.Context, command backendstore.Object) (backendstore.Object, bool, error) {
	ack := backendstore.Reply("ack", nil)
	switch command.String("type") {
	case "fork":
		remove, err := command.Int("turnsToRemove")
		if err != nil {
			return nil, true, err
		}
		result, err := backendservices.ForkRuntime(ctx, a.process.Client, a.nativeID, remove)
		return result, true, err
	case "steer":
		var files []model.SubmissionFile
		if raw := command["files"]; len(raw) > 0 {
			if err := json.Unmarshal(raw, &files); err != nil {
				return nil, true, err
			}
		}
		prepared, err := a.promptFiles(files)
		if err != nil {
			return nil, true, err
		}
		state, err := a.process.Client.Prompt(ctx, a.nativeID, model.NewSubmissionInputID(uint64Now()), command.String("prompt"), prepared)
		if state == "accepted" {
			_ = a.emit("steerAccepted", map[string]string{"message": command.String("prompt")})
			return ack, true, nil
		}
		_ = a.emit("steerRejected", map[string]string{"message": command.String("prompt"), "reason": state})
		return nil, true, err
	case "respondUserInput":
		var answers []backendengine.Answer
		if err := json.Unmarshal(command["answers"], &answers); err != nil {
			return nil, true, err
		}
		request := command.String("requestId")
		scope := a.requestScopes[request]
		if scope == "" {
			return nil, true, errors.New("provider request expired or unknown")
		}
		if err := a.process.Client.Answer(ctx, scope, request, answers, a.forms[request]); err != nil {
			return nil, true, err
		}
		delete(a.forms, request)
		delete(a.requestScopes, request)
		return ack, true, nil
	case "cancelUserInput":
		request := command.String("requestId")
		scope := a.requestScopes[request]
		if scope == "" {
			return nil, true, errors.New("provider request expired or unknown")
		}
		if err := a.process.Client.CancelQuestion(ctx, scope, request); err != nil {
			return nil, true, err
		}
		delete(a.forms, request)
		delete(a.requestScopes, request)
		return ack, true, nil
	case "applyOptions":
		options, err := backendstore.Decode(command["options"])
		if err != nil {
			return nil, true, err
		}
		agent := "build"
		if options.String("interactionMode") == "plan" {
			agent = "plan"
		}
		if err := a.process.Client.Configure(ctx, a.nativeID, options.String("model"), options.String("reasoningEffort"), agent); err != nil {
			return nil, true, err
		}
		return backendstore.Reply("optionsApplied", backendstore.Fields(map[string]any{"applied": true})), true, nil
	case "goal":
		action, err := backendstore.Decode(command["action"])
		if err != nil {
			return nil, true, err
		}
		return ack, true, a.goalCommand(ctx, action)
	case "stopBackgroundWork":
		if child := a.children[command.String("controlId")]; child != nil {
			return ack, true, a.stopChild(ctx, command, child)
		}
		key, _ := backendstore.Decode(command["key"])
		if key.String("kind") == "subagent" {
			return nil, true, errors.New("unknown subagent control identity")
		}
		return ack, true, a.stopShell(ctx, command)
	case "compactChildSession":
		if a.children[command.String("controlId")] == nil {
			return nil, true, errors.New("unknown subagent control identity")
		}
		if err := a.process.Client.Compact(ctx, command.String("controlId")); err != nil {
			return nil, true, err
		}
		return ack, true, nil
	case "refreshBackgroundWork":
		return ack, true, a.refreshChildren(ctx)
	default:
		return nil, false, nil
	}
}

func (a *actor) formEvent(payload backendstore.Object) error {
	request := payload
	for _, key := range []string{"form", "request", "info"} {
		if nested, err := backendstore.Decode(payload[key]); err == nil {
			request = nested
			break
		}
	}
	id := request.String("id")
	if id == "" {
		return errors.New("question request identity missing")
	}
	var questions []backendstore.Object
	if raw := request["questions"]; len(raw) > 0 {
		if err := json.Unmarshal(raw, &questions); err != nil {
			return err
		}
	}
	if len(questions) == 0 {
		if raw := request["fields"]; len(raw) > 0 {
			if err := json.Unmarshal(raw, &questions); err != nil {
				return err
			}
		}
	}
	if len(questions) == 0 {
		return errors.New("question fields missing")
	}
	multi := map[string]bool{}
	public := []backendstore.Object{}
	for _, question := range questions {
		key := question.String("id")
		if key == "" {
			key = question.String("key")
			question.Set("id", key)
		}
		if strings.TrimSpace(key) == "" {
			continue
		}
		multiple := question.Bool("multiple") || question.Bool("multi_select") || strings.Contains(question.String("type"), "multi")
		multi[key] = multiple
		text := question.String("question")
		for _, alias := range []string{"description", "title", "label"} {
			if text == "" {
				text = question.String(alias)
			}
		}
		header := question.String("header")
		if header == "" {
			header = question.String("title")
		}
		if header == "" {
			header = "Question"
		}
		var options []backendstore.Object
		_ = json.Unmarshal(question["options"], &options)
		clean := []backendstore.Object{}
		for _, option := range options {
			if label := option.String("label"); strings.TrimSpace(label) != "" {
				clean = append(clean, backendstore.Fields(map[string]any{"label": label, "description": option.Nullable("description")}))
			}
		}
		public = append(public, backendstore.Fields(map[string]any{"id": key, "header": header, "question": text, "options": clean, "multi_select": multiple}))
	}
	a.forms[id] = multi
	if err := a.rememberRequest(id); err != nil {
		return err
	}
	return a.emit("userInputRequested", map[string]any{"requestId": id, "questions": public})
}

func (a *actor) rememberRequest(id string) error {
	owner := a
	if a.parent != nil {
		owner = a.parent
	}
	if owner.requestScopes == nil {
		owner.requestScopes = map[string]string{}
	}
	if scope := owner.requestScopes[id]; scope != "" {
		if scope != a.nativeID {
			return errors.New("ambiguous provider request identity")
		}
		return nil
	}
	if len(owner.requestScopes) >= 256 {
		return errors.New("provider request capacity reached")
	}
	owner.requestScopes[id] = a.nativeID
	return nil
}

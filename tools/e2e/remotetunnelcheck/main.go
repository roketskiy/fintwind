package main

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"time"

	"fintwind-go/internal/backendremote"
	"fintwind-go/internal/model"
)

func run() error {
	if len(os.Args) != 2 {
		return errors.New("local relay address required")
	}
	directory, err := os.MkdirTemp("", "fintwind-tunnel-check-")
	if err != nil {
		return err
	}
	identity, err := backendremote.LoadIdentity(directory)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	enrollment := backendremote.Enrollment{Origin: os.Args[1], Client: &http.Client{Timeout: 5 * time.Second}, Identity: identity}
	token, err := enrollment.Token(ctx)
	if err != nil {
		return err
	}
	tunnel := &backendremote.Tunnel{Origin: os.Args[1], Token: token}
	if _, err := rand.Read(tunnel.Route[:]); err != nil {
		return err
	}
	if _, err := rand.Read(tunnel.Secret[:]); err != nil {
		return err
	}
	tunnel.Status = func(status string) {
		if status == "waiting" {
			_ = json.NewEncoder(os.Stdout).Encode(map[string]string{"route": base64.RawURLEncoding.EncodeToString(tunnel.Route[:]), "key": base64.RawURLEncoding.EncodeToString(tunnel.Secret[:])})
		}
	}
	projects := []model.Project{{ID: "fixture-project", Name: "Fixture"}}
	scope := backendremote.NewScope(projects)
	projects = append(projects, model.Project{ID: "later-project", Name: "Later"})
	sessions := []model.Session{
		{ID: "recent-session", ProjectID: "fixture-project", Title: "Recent", Status: "idle", UpdatedAt: uint64(time.Now().Unix())},
		{ID: "old-session", ProjectID: "fixture-project", Title: "Old", Status: "idle", UpdatedAt: uint64(time.Now().Add(-73 * time.Hour).Unix())},
	}
	tunnel.Handle = func(ctx context.Context, raw json.RawMessage) (any, error) {
		var request struct {
			Type      string `json:"type"`
			ProjectID string `json:"projectId"`
		}
		if json.Unmarshal(raw, &request) != nil {
			return nil, errors.New("invalid fixture request")
		}
		switch request.Type {
		case "listWorkspaces":
			return scope.Workspaces(projects, "fixture-project"), nil
		case "listSessions":
			return scope.Sessions(projects, sessions, request.ProjectID, time.Now())
		case "sendPrompt":
			return map[string]any{"accepted": true}, nil
		case "getSession":
			turn := "fixture-turn"
			private := "secret-tool-output"
			empty := ""
			session := &model.Session{ID: "fixture-session", ProjectID: "fixture-project", Title: "Fixture", Status: "idle",
				Messages:         []model.Message{{ID: "user-message", Role: "user", Content: "secret-provider-prompt", DisplayContent: &empty}, {ID: "assistant-message", Role: "assistant", Content: "world", TurnID: &turn}},
				TranscriptBlocks: []model.TranscriptBlock{{AfterMessage: 1, TurnID: &turn, Activities: []model.ActivityItem{{ID: "tool", Kind: model.KindTool, Title: "Tool", Output: &private, Complete: true}}}},
				Turns:            []model.AgentTurn{{ID: turn, Status: "completed"}}}
			messages, entries, truncated := backendremote.Transcript(session)
			return map[string]any{"id": session.ID, "projectId": session.ProjectID, "title": session.Title, "status": session.Status,
				"messages": messages, "entries": entries, "features": []string{"orderedTranscript"}, "truncated": truncated,
				"permission": nil, "questions": nil, "desktopRequired": false, "canSend": true}, nil
		default:
			return nil, errors.New("invalid fixture request")
		}
	}
	return tunnel.Run(ctx)
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"fintwind-go/internal/daemon"
)

func checkWorkspaceEscape(ctx context.Context, client *daemon.Client, sessionID, profile string) error {
	operate := func(operation map[string]any) (json.RawMessage, error) {
		return client.RequestFor(ctx, sessionID, "", map[string]any{"type": "workspace", "operation": operation})
	}
	if _, err := operate(map[string]any{"type": "writeTextFile", "root": filepath.Join(profile, "data"), "relativePath": "app.db", "content": "pwned"}); err == nil {
		return errors.New("workspace write into the live data directory was allowed")
	} else if !strings.Contains(strings.ToLower(err.Error()), "outside") {
		return fmt.Errorf("workspace refusal named the wrong boundary: %v", err)
	}
	if _, err := operate(map[string]any{"type": "readTextFile", "root": "", "relativePath": "../data/app.db"}); err == nil {
		return errors.New("workspace parent traversal was allowed")
	}
	raw, err := operate(map[string]any{"type": "writeTextFile", "root": "", "relativePath": "marker-ok.txt", "content": "workspace-marker"})
	if err != nil {
		return fmt.Errorf("workspace write inside the allowed root failed: %w", err)
	}
	raw, err = operate(map[string]any{"type": "readTextFile", "root": "", "relativePath": "marker-ok.txt"})
	if err != nil {
		return fmt.Errorf("workspace read inside the allowed root failed: %w", err)
	}
	var wrapped struct {
		Result json.RawMessage `json:"result"`
	}
	var file struct {
		Type    string `json:"type"`
		Content string `json:"content"`
	}
	if err := json.Unmarshal(raw, &wrapped); err != nil {
		return fmt.Errorf("workspace result envelope: %w", err)
	}
	if err := json.Unmarshal(wrapped.Result, &file); err != nil || file.Type != "textFile" || file.Content != "workspace-marker" {
		return errors.New("workspace round trip inside the allowed root mismatch")
	}
	marker, err := os.ReadFile(filepath.Join(profile, "workspace", "marker-ok.txt"))
	if err != nil || string(marker) != "workspace-marker" {
		return errors.New("workspace write did not land in the isolated workspace")
	}
	header := make([]byte, 16)
	live, err := os.Open(filepath.Join(profile, "data", "app.db"))
	if err != nil {
		return fmt.Errorf("live database is not readable: %w", err)
	}
	_, err = live.Read(header)
	_ = live.Close()
	if err != nil || string(header) != "SQLite format 3\x00" {
		return errors.New("live database header is damaged")
	}
	catalog, err := client.Request(ctx, map[string]any{"type": "loadTaskState"})
	if err != nil {
		return err
	}
	var list struct {
		Sessions []struct {
			ID string `json:"id"`
		} `json:"sessions"`
	}
	if err := json.Unmarshal(catalog, &list); err != nil {
		return err
	}
	if len(list.Sessions) != 1 {
		return errors.New("task catalog changed after workspace probes")
	}
	state, err := client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": list.Sessions[0].ID})
	if err != nil {
		return err
	}
	var detail struct {
		Session struct {
			Messages []struct {
				Content string `json:"content"`
			} `json:"messages"`
		} `json:"session"`
	}
	if err := json.Unmarshal(state, &detail); err != nil {
		return err
	}
	if len(detail.Session.Messages) != 2000 || detail.Session.Messages[1999].Content != "literal-pressure-1999" {
		return errors.New("live database content changed after workspace probes")
	}
	return nil
}

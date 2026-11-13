package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"

	"fintwind-go/internal/daemon"
)

func checkProjectless(ctx context.Context, c *daemon.Client, round string) error {
	call := func(operation map[string]any) (string, error) {
		raw, err := c.Request(ctx, map[string]any{"type": "workspace", "operation": operation})
		if err != nil {
			return "", err
		}
		var response struct {
			Result struct {
				Cwd string `json:"cwd"`
			} `json:"result"`
		}
		err = json.Unmarshal(raw, &response)
		return response.Result.Cwd, err
	}
	root, err := call(map[string]any{"type": "createProjectlessWorkspace", "prompt": "literal"})
	if err != nil {
		return err
	}
	if root != filepath.Join(round, "workspace", "projects") {
		return errors.New("general workspace escaped isolated workspace")
	}
	if _, err := call(map[string]any{"type": "migrateProjectlessWorkspace", "path": round}); err == nil {
		return errors.New("foreign legacy path accepted")
	}
	legacy := filepath.Join(round, "home", ".fintwind", "2026-11-12", "literal")
	if err := os.MkdirAll(legacy, 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(legacy, "sentinel.txt"), []byte("preserve"), 0o600); err != nil {
		return err
	}
	path, err := call(map[string]any{"type": "migrateProjectlessWorkspace", "path": legacy})
	if err != nil {
		return err
	}
	if path != filepath.Join(root, "2026-11-12", "literal") {
		return errors.New("legacy destination mismatch")
	}
	raw, err := os.ReadFile(filepath.Join(path, "sentinel.txt"))
	if err != nil || string(raw) != "preserve" {
		return errors.New("legacy migration lost content")
	}
	if _, err := os.Stat(legacy); !errors.Is(err, os.ErrNotExist) {
		return errors.New("legacy migration did not move original")
	}
	again, err := call(map[string]any{"type": "migrateProjectlessWorkspace", "path": path})
	if err != nil || again != path {
		return errors.New("already migrated workspace not idempotent")
	}
	return nil
}

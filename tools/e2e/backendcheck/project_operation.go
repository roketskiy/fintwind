package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"

	"fintwind-go/internal/daemon"
	"github.com/google/uuid"
)

func checkProjectOperation(ctx context.Context, c *daemon.Client, round string) (string, error) {
	parent := filepath.Join(round, "workspace")
	id := uuid.NewString()
	call := func(operation map[string]any) (json.RawMessage, error) {
		return c.Request(ctx, map[string]any{"type": "workspace", "operation": operation})
	}
	for _, name := range []string{"trailing ", "trailing."} {
		if _, err := call(map[string]any{"type": "createEmptyProject", "operation_id": uuid.NewString(), "parent_dir": parent, "project_name": name}); err == nil {
			return "", errors.New("project name subject to Windows normalization accepted")
		}
	}
	if _, err := call(map[string]any{"type": "createEmptyProject", "operation_id": uuid.NewString(), "parent_dir": parent, "project_name": "../outside"}); err == nil {
		return "", errors.New("project name escape accepted")
	}
	if _, err := call(map[string]any{"type": "createEmptyProject", "operation_id": uuid.NewString(), "parent_dir": round, "project_name": "outside"}); err == nil {
		return "", errors.New("project parent escape accepted")
	}
	operation := map[string]any{"type": "createEmptyProject", "operation_id": id, "parent_dir": parent, "project_name": "literal project"}
	raw, err := call(operation)
	if err != nil {
		return "", err
	}
	var response struct {
		Result struct {
			Type    string `json:"type"`
			ID      string `json:"operation_id"`
			State   string `json:"state"`
			Path    string `json:"path"`
			Failure string `json:"failure_kind"`
		} `json:"result"`
	}
	if err := json.Unmarshal(raw, &response); err != nil {
		return "", err
	}
	if response.Result.Type != "projectOperation" || response.Result.ID != id || response.Result.State != "succeeded" || response.Result.Path != filepath.Join(parent, "literal project") {
		return "", errors.New("project creation result mismatch")
	}
	if _, err := call(operation); err == nil {
		return "", errors.New("duplicate project operation accepted")
	}
	if err := os.WriteFile(filepath.Join(response.Result.Path, "sentinel.txt"), []byte("must survive"), 0o600); err != nil {
		return "", err
	}
	operation["operation_id"] = uuid.NewString()
	raw, err = call(operation)
	if err != nil {
		return "", err
	}
	if err := json.Unmarshal(raw, &response); err != nil {
		return "", err
	}
	if response.Result.State != "failed" || response.Result.Failure != "target_exists" {
		return "", errors.New("existing project target adopted as success")
	}
	bytes, err := os.ReadFile(filepath.Join(parent, "literal project", "sentinel.txt"))
	if err != nil || string(bytes) != "must survive" {
		return "", errors.New("failed create changed existing directory")
	}
	return id, nil
}

func checkProjectOperationRestart(ctx context.Context, c *daemon.Client, id string) error {
	raw, err := c.Request(ctx, map[string]any{"type": "workspace", "operation": map[string]any{"type": "getProjectOperation", "operation_id": id}})
	if err != nil {
		return err
	}
	var response struct {
		Result struct {
			State string `json:"state"`
			ID    string `json:"operation_id"`
		} `json:"result"`
	}
	if err := json.Unmarshal(raw, &response); err != nil {
		return err
	}
	if response.Result.ID != id || response.Result.State != "succeeded" {
		return errors.New("project operation lost across restart")
	}
	return nil
}

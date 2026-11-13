package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
)

func checkReview(ctx context.Context, c *daemon.Client, round string) error {
	repo := filepath.Join(round, "workspace", "review")
	if err := os.Mkdir(repo, 0o700); err != nil {
		return err
	}
	git, err := exec.LookPath("git")
	if err != nil {
		return err
	}
	logs := filepath.Join(round, "review-init")
	if err := os.Mkdir(logs, 0o700); err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(git, []string{"init"}, logs, repo, ownedprocess.BuildEnv(round, nil))
	if err != nil {
		return err
	}
	defer child.Close()
	done, err := child.Exited(5000)
	if err != nil || !done {
		return errors.New("review fixture git init failed")
	}
	if err := os.WriteFile(filepath.Join(repo, "literal.txt"), []byte("literal review line\n"), 0o600); err != nil {
		return err
	}
	call := func(op map[string]any) (json.RawMessage, error) {
		op["cwd"] = repo
		return c.Request(ctx, map[string]any{"type": "workspace", "operation": op})
	}
	raw, err := call(map[string]any{"type": "collectReviewFileDiff", "source": "uncommitted", "path": "literal.txt", "untracked": true, "context_lines": 3, "query_id": uuid.NewString()})
	if err != nil {
		return err
	}
	var response struct {
		Result struct {
			Type string `json:"type"`
			Data struct {
				File struct {
					Additions int `json:"additions"`
				} `json:"file"`
			} `json:"data"`
		} `json:"result"`
	}
	if err := json.Unmarshal(raw, &response); err != nil {
		return err
	}
	if response.Result.Type != "reviewDiff" || response.Result.Data.File.Additions != 1 {
		return errors.New("real daemon review payload mismatch")
	}
	qid := uuid.NewString()
	if _, err := call(map[string]any{"type": "cancelGitQuery", "query_id": qid}); err != nil {
		return err
	}
	if _, err := call(map[string]any{"type": "collectReviewStats", "source": "uncommitted", "query_id": qid}); err == nil {
		return errors.New("daemon lost cancellation tombstone")
	}
	return nil
}

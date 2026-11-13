package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendworkspace"
	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}
func run() (err error) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	defer func() {
		report := map[string]any{"passed": err == nil, "checks": checks, "moduleOnly": true}
		if err != nil {
			report["error"] = err.Error()
		}
		raw, e := json.MarshalIndent(report, "", "  ")
		if e == nil {
			e = os.WriteFile(filepath.Join(round, "worktree-result.json"), raw, 0o600)
		}
		err = errors.Join(err, e)
		fmt.Println("[REPORT]", filepath.Join(round, "worktree-result.json"))
	}()
	repo := filepath.Join(round, "repo")
	nested := filepath.Join(repo, "project")
	if err := os.MkdirAll(nested, 0o700); err != nil {
		return err
	}
	git, err := exec.LookPath("git")
	if err != nil {
		return err
	}
	logs := filepath.Join(round, "init")
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
		return errors.New("git init failed")
	}
	call := func(kind string, fields map[string]any) (map[string]json.RawMessage, error) {
		fields["type"] = kind
		fields["cwd"] = repo
		raw, _ := json.Marshal(fields)
		response, err := backendworkspace.Dispatch(ctx, round, raw)
		if err != nil {
			return nil, err
		}
		var out map[string]json.RawMessage
		err = json.Unmarshal(response, &out)
		return out, err
	}
	projectID := uuid.NewString()
	newWorktree := func(base string) (map[string]json.RawMessage, error) {
		return call("createWorktree", map[string]any{"project_path": nested, "project_id": projectID, "session_id": uuid.NewString(), "prompt": "Fix literal task", "base_branch": base})
	}
	if _, err := newWorktree("--help"); err == nil {
		return errors.New("worktree option injection accepted")
	}
	checks["base-option-injection-refused"] = true
	if err := os.WriteFile(filepath.Join(nested, "sentinel.txt"), []byte("base\n"), 0o600); err != nil {
		return err
	}
	if _, err := call("stagePaths", map[string]any{"paths": []string{"project/sentinel.txt"}}); err != nil {
		return err
	}
	if _, err := call("commit", map[string]any{"message": "fixture"}); err != nil {
		return err
	}
	if _, err := newWorktree("missing-branch"); err == nil {
		return errors.New("missing base accepted")
	}
	checks["missing-base-refused"] = true
	if err := os.WriteFile(filepath.Join(nested, "sentinel.txt"), []byte("user dirty\n"), 0o600); err != nil {
		return err
	}
	branches := map[string]bool{}
	for i := 0; i < 2; i++ {
		response, err := newWorktree("HEAD")
		if err != nil {
			return err
		}
		var got struct {
			Path   string `json:"path"`
			Branch string `json:"branch"`
		}
		if err := json.Unmarshal(response["worktree"], &got); err != nil {
			return err
		}
		rel, err := filepath.Rel(round, got.Path)
		if err != nil || !filepath.IsLocal(rel) || !strings.HasSuffix(filepath.ToSlash(got.Path), "/project") {
			return errors.New("worktree project-relative path incorrect")
		}
		if branches[got.Branch] {
			return errors.New("worktree name collision")
		}
		branches[got.Branch] = true
		raw, err := os.ReadFile(filepath.Join(got.Path, "sentinel.txt"))
		if err != nil || string(raw) != "base\n" {
			return errors.New("worktree did not start from base commit")
		}
	}
	checks["collision-allocates-distinct-worktree"] = true
	checks["nested-project-path-preserved"] = true
	bytes, err := os.ReadFile(filepath.Join(nested, "sentinel.txt"))
	if err != nil || string(bytes) != "user dirty\n" {
		return errors.New("worktree creation touched original user edits")
	}
	checks["original-dirty-workspace-preserved"] = true
	return nil
}

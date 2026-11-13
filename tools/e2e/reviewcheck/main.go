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
		report := map[string]any{"checks": checks, "passed": err == nil, "moduleOnly": true}
		if err != nil {
			report["error"] = err.Error()
		}
		raw, e := json.MarshalIndent(report, "", "  ")
		if e == nil {
			e = os.WriteFile(filepath.Join(round, "review-result.json"), raw, 0o600)
		}
		err = errors.Join(err, e)
		fmt.Println("[REPORT]", filepath.Join(round, "review-result.json"))
	}()
	repo := filepath.Join(round, "repo")
	if err := os.Mkdir(repo, 0o700); err != nil {
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
	queries := &backendworkspace.Queries{}
	call := func(kind string, fields map[string]any) (map[string]json.RawMessage, error) {
		if fields == nil {
			fields = map[string]any{}
		}
		fields["type"] = kind
		fields["cwd"] = repo
		raw, _ := json.Marshal(fields)
		response, err := backendworkspace.DispatchWithQueries(ctx, round, raw, queries)
		if err != nil {
			return nil, err
		}
		var out map[string]json.RawMessage
		err = json.Unmarshal(response, &out)
		return out, err
	}
	for _, path := range []string{"../outside", filepath.Join(round, "outside")} {
		if _, err := call("collectReviewFileDiff", map[string]any{"source": "uncommitted", "path": path, "untracked": true}); err == nil {
			return errors.New("review path escape accepted")
		}
	}
	checks["review-path-escape-refused"] = true
	queryID := uuid.NewString()
	if _, err := call("cancelReviewFileDiff", map[string]any{"query_id": queryID}); err != nil {
		return err
	}
	if _, err := call("collectReviewStats", map[string]any{"source": "uncommitted", "query_id": queryID}); err == nil {
		return errors.New("pre-cancelled query started")
	}
	checks["cancel-before-registration"] = true
	if _, err := call("collectReviewStats", map[string]any{"source": map[string]any{"lastTurn": map[string]any{"session_id": uuid.NewString(), "turn_count": 0}}}); err == nil {
		return errors.New("baseline turn accepted as completed review")
	}
	checks["baseline-turn-refused"] = true
	if err := os.WriteFile(filepath.Join(repo, "literal.txt"), []byte("base\n"), 0o600); err != nil {
		return err
	}
	if _, err := call("stagePaths", map[string]any{"paths": []string{"literal.txt"}}); err != nil {
		return err
	}
	if _, err := call("commit", map[string]any{"message": "base fixture"}); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "literal.txt"), []byte("base\nstaged\n"), 0o600); err != nil {
		return err
	}
	if _, err := call("stagePaths", map[string]any{"paths": []string{"literal.txt"}}); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "literal.txt"), []byte("base\nstaged\nunstaged\n"), 0o600); err != nil {
		return err
	}
	for _, scenario := range []struct {
		source string
		want   uint64
	}{{"staged", 1}, {"unstaged", 1}, {"uncommitted", 2}} {
		response, err := call("collectReviewStats", map[string]any{"source": scenario.source})
		if err != nil {
			return err
		}
		var got uint64
		if err := json.Unmarshal(response["additions"], &got); err != nil {
			return err
		}
		if got != scenario.want {
			return fmt.Errorf("%s additions got %d expected %d", scenario.source, got, scenario.want)
		}
		checks["stats-"+scenario.source] = true
	}
	response, err := call("collectReviewFileDiff", map[string]any{"source": "staged", "path": "literal.txt", "context_lines": 3, "query_id": uuid.NewString()})
	if err != nil {
		return err
	}
	var diff struct {
		Patch string `json:"patch"`
		File  struct {
			Additions uint64 `json:"additions"`
			Binary    bool   `json:"binary"`
		} `json:"file"`
	}
	if err := json.Unmarshal(response["data"], &diff); err != nil {
		return err
	}
	if !strings.Contains(diff.Patch, "+staged") || strings.Contains(diff.Patch, "+unstaged") || diff.File.Additions != 1 {
		return errors.New("single-file staged diff mixed unstaged changes")
	}
	checks["single-file-source-boundary"] = true
	if err := os.WriteFile(filepath.Join(repo, "untracked.txt"), []byte("one\ntwo\n"), 0o600); err != nil {
		return err
	}
	response, err = call("collectReviewFileDiff", map[string]any{"source": "unstaged", "path": "untracked.txt", "untracked": true})
	if err != nil {
		return err
	}
	if err := json.Unmarshal(response["data"], &diff); err != nil {
		return err
	}
	if diff.File.Additions != 2 || !strings.Contains(diff.Patch, "+two") {
		return errors.New("untracked no-index text mismatch")
	}
	checks["untracked-text-no-index"] = true
	if err := os.WriteFile(filepath.Join(repo, "binary.bin"), []byte{1, 0, 2, 3}, 0o600); err != nil {
		return err
	}
	response, err = call("collectReviewFileDiff", map[string]any{"source": "unstaged", "path": "binary.bin", "untracked": true})
	if err != nil {
		return err
	}
	if err := json.Unmarshal(response["data"], &diff); err != nil {
		return err
	}
	if !diff.File.Binary {
		return errors.New("binary diff fabricated line counts")
	}
	checks["untracked-binary-detected"] = true
	response, err = call("collectReviewDiff", map[string]any{"source": "uncommitted"})
	if err != nil {
		return err
	}
	if err := json.Unmarshal(response["data"], &diff); err != nil {
		return err
	}
	if !strings.Contains(diff.Patch, "+unstaged") || !strings.Contains(diff.Patch, "+two") {
		return errors.New("snapshot review omitted live/untracked edits")
	}
	checks["stable-snapshot-review"] = true
	response, err = call("collectReviewStats", map[string]any{"source": "uncommitted"})
	if err != nil {
		return err
	}
	var additions uint64
	if err := json.Unmarshal(response["additions"], &additions); err != nil {
		return err
	}
	if additions != 4 {
		return errors.New("review stats omitted untracked additions")
	}
	checks["stats-include-untracked"] = true
	if err := pressure(ctx, round, repo, queries, checks); err != nil {
		return err
	}
	return nil
}

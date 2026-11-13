package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
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
	if len(os.Args) > 1 && os.Args[1] == "--git-heartbeat" {
		os.Exit(runGitHeartbeat(os.Args[2:]))
	}
	if strings.EqualFold(filepath.Base(os.Args[0]), "git.exe") {
		os.Exit(runFakeGitWrapper(os.Args[1:]))
	}
	daemonBinary := flag.String("daemon", "", "absolute daemon binary for the status WebSocket E2E")
	flag.Parse()
	restoreCredentials := clearCredentialEnvironment()
	defer restoreCredentials()
	err := run()
	if err == nil && *daemonBinary != "" {
		err = runStatusDaemon(*daemonBinary)
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func clearCredentialEnvironment() func() {
	keys := []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "FINTWIND_AGENT_REAL_PROVIDER"}
	saved := map[string]string{}
	for _, key := range keys {
		if value, ok := os.LookupEnv(key); ok {
			saved[key] = value
			_ = os.Unsetenv(key)
		}
	}
	return func() {
		for key, value := range saved {
			_ = os.Setenv(key, value)
		}
	}
}

func run() (err error) {
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	defer func() {
		raw, _ := json.MarshalIndent(map[string]any{"checks": checks, "passed": err == nil}, "", "  ")
		_ = os.WriteFile(filepath.Join(round, "git-result.json"), raw, 0o600)
		fmt.Println("[REPORT]", filepath.Join(round, "git-result.json"))
	}()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	repo := filepath.Join(round, "repo")
	if err := os.Mkdir(repo, 0o700); err != nil {
		return err
	}
	git, err := exec.LookPath("git")
	if err != nil {
		return err
	}
	logs := filepath.Join(round, "init")
	_ = os.Mkdir(logs, 0o700)
	child, err := ownedprocess.StartWithEnvironment(git, []string{"init"}, logs, repo, ownedprocess.BuildEnv(round, nil))
	if err != nil {
		return err
	}
	defer child.Close()
	done, err := child.Exited(5000)
	if err != nil || !done {
		return errors.New("git init did not exit")
	}
	call := func(kind string, fields map[string]any) (map[string]json.RawMessage, error) {
		if fields == nil {
			fields = map[string]any{}
		}
		fields["type"] = kind
		fields["cwd"] = repo
		raw, _ := json.Marshal(fields)
		response, err := backendworkspace.Dispatch(ctx, round, raw)
		if err != nil {
			return nil, err
		}
		var result map[string]json.RawMessage
		err = json.Unmarshal(response, &result)
		return result, err
	}
	if err := os.WriteFile(filepath.Join(repo, "literal.txt"), []byte("first\n"), 0o600); err != nil {
		return err
	}
	if _, err := call("stagePaths", map[string]any{"paths": []string{"../outside"}}); err == nil {
		return errors.New("stage escape accepted")
	}
	checks["stage-path-escape-refused"] = true
	if _, err := call("captureRef", map[string]any{"gitRef": "refs/fintwind/unborn"}); err != nil {
		return fmt.Errorf("unborn capture: %w", err)
	}
	checks["unborn-repository-capture"] = true
	if _, err := call("stagePaths", map[string]any{"paths": []string{"literal.txt"}}); err != nil {
		return err
	}
	if _, err := call("commit", map[string]any{"message": "fixture commit", "includeUnstaged": false, "push": false}); err != nil {
		return err
	}
	checks["stage-and-commit"] = true
	if _, err := call("checkoutBranch", map[string]any{"branch": "fixture", "create": true}); err != nil {
		return err
	}
	checks["create-checkout-branch"] = true
	ref := "refs/fintwind/check-literal"
	if _, err := call("captureRef", map[string]any{"gitRef": ref}); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "literal.txt"), []byte("second\n"), 0o600); err != nil {
		return err
	}
	if _, err := call("restoreRef", map[string]any{"gitRef": ref}); err != nil {
		return err
	}
	bytes, err := os.ReadFile(filepath.Join(repo, "literal.txt"))
	if err != nil || string(bytes) != "first\n" {
		return errors.New("checkpoint restore mismatch")
	}
	checks["capture-restore-ref"] = true
	if _, err := call("deleteRef", map[string]any{"gitRef": "refs/heads/fixture"}); err == nil {
		return errors.New("non-checkpoint delete permitted")
	}
	checks["noncheckpoint-ref-protected"] = true
	if err := checkTurns(repo, call, checks); err != nil {
		return err
	}
	return nil
}

func checkTurns(repo string, call func(string, map[string]any) (map[string]json.RawMessage, error), checks map[string]bool) error {
	session, target := uuid.NewString(), uuid.NewString()
	if err := os.WriteFile(filepath.Join(repo, "literal.txt"), []byte("user dirty\n"), 0o600); err != nil {
		return err
	}
	if _, err := call("captureTurnStart", map[string]any{"sessionId": session, "turnCount": 1}); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "response.txt"), []byte("agent response\n"), 0o600); err != nil {
		return err
	}
	response, err := call("captureTurn", map[string]any{"sessionId": session, "turnCount": 1})
	if err != nil {
		return err
	}
	var checkpoint struct {
		Status string `json:"status"`
		Files  []struct {
			Path string `json:"path"`
		} `json:"files"`
	}
	if err := json.Unmarshal(response["checkpoint"], &checkpoint); err != nil {
		return err
	}
	if checkpoint.Status != "ready" || len(checkpoint.Files) != 1 || checkpoint.Files[0].Path != "response.txt" {
		return errors.New("turn checkpoint attributed preexisting dirty edits")
	}
	checks["turn-start-excludes-user-dirty"] = true
	status, err := call("inspectStatus", nil)
	if err != nil {
		return err
	}
	// Read raw snake_case fields: an isolated capture must not stage anything.
	var rawStatus struct {
		Entries []struct {
			Index string `json:"index_status"`
		} `json:"entries"`
	}
	if err := json.Unmarshal(status["status"], &rawStatus); err != nil {
		return err
	}
	for _, entry := range rawStatus.Entries {
		if entry.Index != " " && entry.Index != "?" {
			return errors.New("snapshot altered live index")
		}
	}
	checks["capture-preserves-live-index"] = true
	if _, err := call("copySessionRefs", map[string]any{"sourceSessionId": session, "targetSessionId": target, "throughTurnCount": 1}); err != nil {
		return err
	}
	refs, err := call("sessionTurnRefs", map[string]any{"sessionId": target})
	if err != nil {
		return err
	}
	var turns []int
	if err := json.Unmarshal(refs["turn_counts"], &turns); err != nil {
		return err
	}
	if len(turns) != 2 || turns[0]+turns[1] != 1 {
		return errors.New("atomic ref copy missing turn")
	}
	checks["atomic-session-ref-copy"] = true
	if _, err := call("deleteAllSessionRefs", map[string]any{"sessionId": session}); err != nil {
		return err
	}
	refs, err = call("sessionTurnRefs", map[string]any{"sessionId": session})
	if err != nil {
		return err
	}
	if string(refs["turn_counts"]) != "[]" {
		return errors.New("session refs not removed")
	}
	refs, err = call("sessionTurnRefs", map[string]any{"sessionId": target})
	if err != nil {
		return err
	}
	if err := json.Unmarshal(refs["turn_counts"], &turns); err != nil {
		return err
	}
	if len(turns) != 2 || turns[0]+turns[1] != 1 {
		return errors.New("delete touched another session refs")
	}
	checks["session-ref-delete-isolation"] = true
	if err := os.WriteFile(filepath.Join(repo, "later.txt"), []byte("later\n"), 0o600); err != nil {
		return err
	}
	if _, err := call("restoreRef", map[string]any{"gitRef": fmt.Sprintf("refs/fintwind/session-%s-turn-1", target)}); err != nil {
		return err
	}
	if _, err := os.Stat(filepath.Join(repo, "later.txt")); !errors.Is(err, os.ErrNotExist) {
		return errors.New("restore retained post-checkpoint untracked file")
	}
	bytes, err := os.ReadFile(filepath.Join(repo, "literal.txt"))
	if err != nil || string(bytes) != "user dirty\n" {
		return errors.New("restore lost user dirty snapshot")
	}
	bytes, err = os.ReadFile(filepath.Join(repo, "response.txt"))
	if err != nil || string(bytes) != "agent response\n" {
		return errors.New("restore lost snapshot untracked file")
	}
	checks["restore-worktree-and-untracked"] = true
	if _, err := call("checkoutBranch", map[string]any{"branch": "other", "create": true}); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "other.txt"), []byte("other branch\n"), 0o600); err != nil {
		return err
	}
	if _, err := call("stagePaths", map[string]any{"paths": []string{"other.txt"}}); err != nil {
		return err
	}
	if _, err := call("commit", map[string]any{"message": "other base", "includeUnstaged": false}); err != nil {
		return err
	}
	if _, err := call("checkoutBranch", map[string]any{"branch": "fixture", "create": false}); err != nil {
		return err
	}
	if _, err := call("captureTurnStart", map[string]any{"sessionId": target, "turnCount": 2}); err != nil {
		return err
	}
	if _, err := call("checkoutBranch", map[string]any{"branch": "other", "create": false}); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "response-branch.txt"), []byte("branch response\n"), 0o600); err != nil {
		return err
	}
	response, err = call("captureTurn", map[string]any{"sessionId": target, "turnCount": 2})
	if err != nil {
		return err
	}
	if err := json.Unmarshal(response["checkpoint"], &checkpoint); err != nil {
		return err
	}
	if checkpoint.Status != "ready" || len(checkpoint.Files) != 1 || checkpoint.Files[0].Path != "response-branch.txt" {
		return errors.New("cross-branch checkpoint included preexisting edits or branch base")
	}
	checks["cross-branch-diff-base-preserves-user-dirty"] = true
	return nil
}

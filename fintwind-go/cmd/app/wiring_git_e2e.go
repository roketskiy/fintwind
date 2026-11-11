//go:build wiringe2e

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

	"fintwind-go/internal/ownedprocess"

	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/editor"
)

const wiringHistorySession = "30000000-0000-4000-8000-000000000003"
const wiringCheckpoint = "refs/fintwind/session-30000000-0000-4000-8000-000000000003-turn-1"

type wiringGitOperation struct {
	Type            string          `json:"type"`
	Cwd             string          `json:"cwd"`
	Paths           []string        `json:"paths"`
	Tracked         []string        `json:"tracked"`
	Untracked       []string        `json:"untracked"`
	Message         string          `json:"message"`
	IncludeUnstaged bool            `json:"include_unstaged"`
	Push            bool            `json:"push"`
	Branch          string          `json:"branch"`
	Create          bool            `json:"create"`
	Path            string          `json:"path"`
	Source          json.RawMessage `json:"source"`
}

func wiringGit(ctx context.Context, root, round string, args ...string) (string, error) {
	if root != filepath.Join(round, "repository") {
		return "", errors.New("Git root is outside owned round")
	}
	call, cancel := context.WithTimeout(ctx, 4*time.Second)
	defer cancel()
	fixed := []string{"-C", root, "-c", "core.hooksPath=" + filepath.Join(round, "no-hooks"), "-c", "core.autocrlf=false", "-c", "protocol.allow=never", "-c", "credential.helper=", "-c", "commit.gpgSign=false", "-c", "tag.gpgSign=false"}
	cmd := exec.CommandContext(call, "git", append(fixed, args...)...)
	cmd.Env = ownedprocess.BuildEnv(round, map[string]string{"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.DevNull, "GIT_TERMINAL_PROMPT": "0"})
	cmd.WaitDelay = time.Second
	data, err := cmd.CombinedOutput()
	if err != nil {
		return string(data), fmt.Errorf("isolated git %s: %w: %s", args[0], err, strings.TrimSpace(string(data)))
	}
	return string(data), nil
}

func wiringInitGit(ctx context.Context, root, round string) error {
	if err := os.Mkdir(root, 0o700); err != nil {
		return err
	}
	for _, args := range [][]string{{"init", "--initial-branch=main"}, {"config", "--local", "user.name", "Wiring Fixture"}, {"config", "--local", "user.email", "wiring@example.invalid"}} {
		if _, err := wiringGit(ctx, root, round, args...); err != nil {
			return err
		}
	}
	path := filepath.Join(root, "notes.txt")
	if err := os.WriteFile(path, []byte("base line\n"), 0o600); err != nil {
		return err
	}
	for _, args := range [][]string{{"add", "--", "notes.txt"}, {"commit", "--no-verify", "-m", "fixture base"}, {"branch", "fixture-feature"}} {
		if _, err := wiringGit(ctx, root, round, args...); err != nil {
			return err
		}
	}
	if err := os.WriteFile(path, []byte("checkpoint line\n"), 0o600); err != nil {
		return err
	}
	for _, args := range [][]string{{"add", "--", "notes.txt"}, {"commit", "--no-verify", "-m", "fixture checkpoint"}, {"update-ref", wiringCheckpoint, "HEAD"}} {
		if _, err := wiringGit(ctx, root, round, args...); err != nil {
			return err
		}
	}
	return os.WriteFile(path, []byte("live worktree line\n"), 0o600)
}

func (f *wiringFixture) gitOperation(op wiringGitOperation) (any, error) {
	if op.Cwd != f.root {
		return nil, errors.New("fixture rejects other workspaces")
	}
	round := filepath.Dir(f.root)
	git := func(args ...string) (string, error) { return wiringGit(f.ctx, f.root, round, args...) }
	ack := map[string]string{"type": "ack"}
	pathsAllowed := func(paths []string) bool {
		for _, path := range paths {
			if path != "notes.txt" {
				return false
			}
		}
		return true
	}
	switch op.Type {
	case "inspectStatus":
		status, err := git("status", "--porcelain=v1", "-z", "--untracked-files=all", "--")
		if err != nil {
			return nil, err
		}
		branch, err := git("symbolic-ref", "--short", "HEAD")
		if err != nil {
			return nil, err
		}
		entries := make([]map[string]any, 0)
		for _, line := range strings.Split(status, "\x00") {
			if len(line) >= 4 {
				entries = append(entries, map[string]any{"path": line[3:], "origin_path": nil, "index_status": line[:1], "worktree_status": line[1:2]})
			}
		}
		return map[string]any{"type": "worktreeStatus", "status": map[string]any{"branch": strings.TrimSpace(branch), "entries": entries, "ahead": 0, "behind": 0}}, nil
	case "stagePaths", "unstagePaths":
		if len(op.Paths) != 1 || !pathsAllowed(op.Paths) {
			return nil, errors.New("fixture path rejected")
		}
		args := []string{"add", "--", "notes.txt"}
		if op.Type == "unstagePaths" {
			args = []string{"restore", "--staged", "--", "notes.txt"}
		}
		_, err := git(args...)
		return ack, err
	case "commit":
		if op.Push || op.IncludeUnstaged || op.Message != "fixture UI commit" {
			return nil, errors.New("fixture commit policy rejected")
		}
		_, err := git("commit", "--no-verify", "-m", op.Message, "--")
		return ack, err
	case "discardWorktreePaths":
		if len(op.Tracked) != 1 || len(op.Untracked) != 0 || !pathsAllowed(op.Tracked) {
			return nil, errors.New("fixture discard policy rejected")
		}
		_, err := git("restore", "--worktree", "--", "notes.txt")
		return ack, err
	case "inspectBranchRefs":
		data, err := git("for-each-ref", "--format=%(refname:short)", "refs/heads/")
		if err != nil {
			return nil, err
		}
		branches := []map[string]any{}
		for _, name := range strings.Fields(data) {
			branches = append(branches, map[string]any{"name": name, "checked_out_elsewhere": false})
		}
		return map[string]any{"type": "branches", "snapshot": map[string]any{"branches": branches}}, nil
	case "checkoutBranch":
		if op.Create || op.Branch != "fixture-feature" {
			return nil, errors.New("fixture branch rejected")
		}
		_, err := git("switch", "--no-guess", "--", op.Branch)
		return map[string]string{"type": "branchChanged"}, err
	case "collectReviewFileDiff":
		if op.Path != "notes.txt" {
			return nil, errors.New("fixture diff path rejected")
		}
		var source string
		if json.Unmarshal(op.Source, &source) != nil || (source != "staged" && source != "unstaged") {
			return nil, errors.New("fixture diff source rejected")
		}
		args := []string{"diff", "--no-ext-diff", "--no-textconv", "--no-color", "--unified=20"}
		if source == "staged" {
			args = append(args, "--cached")
		}
		patch, err := git(append(args, "--", "notes.txt")...)
		return map[string]any{"type": "reviewDiff", "data": map[string]any{"source": source, "patch": patch, "completeContext": true, "truncated": false, "numstat": "1\t1\tnotes.txt\n", "file": map[string]any{"path": "notes.txt", "additions": 1, "deletions": 1, "binary": false}}}, err
	case "collectReviewDiff":
		var source struct {
			LastTurn *struct {
				SessionID string `json:"session_id"`
				TurnID    string `json:"turn_id"`
				TurnCount int    `json:"turn_count"`
			} `json:"lastTurn"`
		}
		if json.Unmarshal(op.Source, &source) != nil || source.LastTurn == nil || source.LastTurn.SessionID != wiringHistorySession || source.LastTurn.TurnID != "turn-1" || source.LastTurn.TurnCount != 1 {
			return nil, errors.New("fixture checkpoint source rejected")
		}
		if _, err := git("rev-parse", "--verify", wiringCheckpoint+"^{commit}"); err != nil {
			return nil, errors.New("Checkpoint ref missing; current worktree is not a fallback")
		}
		patch, err := git("diff", "--no-ext-diff", "--no-textconv", "--no-color", wiringCheckpoint+"^", wiringCheckpoint, "--", "notes.txt")
		return map[string]any{"type": "reviewDiff", "data": map[string]any{"source": json.RawMessage(op.Source), "patch": patch, "completeContext": true, "truncated": false, "numstat": "1\t1\tnotes.txt\n"}}, err
	default:
		f.mu.Lock()
		f.errors = append(f.errors, "unexpected workspace operation "+op.Type)
		f.mu.Unlock()
		return nil, errors.New("fixture workspace operation not allowed")
	}
}

func wiringInstallSC(ctx context.Context, r *wiringReport, a *app, f *wiringFixture) bool {
	var response struct {
		Type   string
		Status struct {
			Branch  string
			Entries []struct {
				Path     string
				Index    string `json:"index_status"`
				Worktree string `json:"worktree_status"`
			}
		}
	}
	call, cancel := context.WithTimeout(ctx, 4*time.Second)
	defer cancel()
	err := scRequest(call, a.client, map[string]any{"type": "inspectStatus", "cwd": f.root}, &response)
	if err != nil {
		r.check("status RPC decode", "success", err.Error())
		return false
	}
	r.check("real Git status response type", "worktreeStatus", response.Type)
	a.sourceControl = sourceControlState{Scope: a.scScope(), Root: f.root, Branch: response.Status.Branch, IsRepo: true, Loaded: true, LastRefresh: time.Now(), FilesByKey: map[string]scFile{}}
	st := &a.sourceControl
	for _, entry := range response.Status.Entries {
		group, letter := "changes", entry.Worktree
		if entry.Index != " " && entry.Index != "" {
			group, letter = "staged", entry.Index
		}
		file := scFile{Path: entry.Path, Group: group, Letter: letter}
		st.Files = append(st.Files, file)
		st.FilesByKey[file.key()] = file
		st.HasStaged = st.HasStaged || group == "staged"
	}
	st.rebuildRows()
	return true
}

func wiringSourceControl(ctx context.Context, r *wiringReport, a *app, f *wiringFixture, round string) {
	a.selectedID = ""
	if !wiringInstallSC(ctx, r, a, f) {
		return
	}
	r.check("real modified file decoded into source control", []string{"changes\x00notes.txt"}, wiringFileKeys(a.sourceControl.Files))
	tt := ui.NewTesterWith(a.sourceControlView, 1400, 1000, ui.TesterOptions{Preferences: ui.Preferences{ReduceMotion: true}})
	wiringClick(r, tt, "Stage")
	wiringWait(ctx, r, f, "workspace:stagePaths")
	if !wiringInstallSC(ctx, r, a, f) {
		return
	}
	tt.Frame()
	r.check("stage changes real Git index", []string{"staged\x00notes.txt"}, wiringFileKeys(a.sourceControl.Files))
	wiringClick(r, tt, "Unstage")
	wiringWait(ctx, r, f, "workspace:unstagePaths")
	if !wiringInstallSC(ctx, r, a, f) {
		return
	}
	tt.Frame()
	r.check("unstage preserves worktree change", []string{"changes\x00notes.txt"}, wiringFileKeys(a.sourceControl.Files))
	wiringClick(r, tt, "Discard…")
	r.check("discard opening does not write", 0, f.count("workspace:discardWorktreePaths"))
	r.check("discard asks named path confirmation", true, tt.HasText("Discard unstaged changes to notes.txt?"))
	wiringClick(r, tt, "Cancel")
	r.check("cancel discard retains edited file", "live worktree line\n", wiringRead(f.root, "notes.txt"))
	wiringClick(r, tt, "Discard…")
	wiringClick(r, tt, "Confirm discard")
	wiringWait(ctx, r, f, "workspace:discardWorktreePaths")
	r.check("confirmed discard restores tracked bytes", "checkpoint line\n", wiringRead(f.root, "notes.txt"))
	if err := os.WriteFile(filepath.Join(f.root, "notes.txt"), []byte("committed UI line\n"), 0o600); err != nil {
		r.check("prepare isolated commit", "success", err.Error())
		return
	}
	if !wiringInstallSC(ctx, r, a, f) {
		return
	}
	tt.Frame()
	wiringClick(r, tt, "Stage")
	wiringWait(ctx, r, f, "workspace:stagePaths")
	if !wiringInstallSC(ctx, r, a, f) {
		return
	}
	tt.Frame()
	wiringType(r, tt, "Commit message", "fixture UI commit")
	tt.Key(ui.Ctrl, ui.KeyEnter)
	wiringWait(ctx, r, f, "workspace:commit")
	subject, err := wiringGit(ctx, f.root, round, "log", "-1", "--format=%s", "--")
	if err != nil {
		r.check("read commit subject", "success", err.Error())
	} else {
		r.check("UI commit reached real Git history", "fixture UI commit", strings.TrimSpace(subject))
	}
	if !wiringInstallSC(ctx, r, a, f) {
		return
	}
	tt.Frame()
	r.check("commit leaves clean owned repository", []string{}, wiringFileKeys(a.sourceControl.Files))
	// The branch chip carries the checked-out branch as its name; clicking
	// it opens the picker.
	wiringClick(r, tt, "main")
	wiringWait(ctx, r, f, "workspace:inspectBranchRefs")
	var branches struct {
		Type     string
		Snapshot struct{ Branches []scBranch }
	}
	call, cancel := context.WithTimeout(ctx, 4*time.Second)
	err = scRequest(call, a.client, map[string]any{"type": "inspectBranchRefs", "cwd": f.root}, &branches)
	cancel()
	if err != nil {
		r.check("read actual branches", "success", err.Error())
		return
	}
	a.sourceControl.BranchLoading = false
	a.sourceControl.Branches = branches.Snapshot.Branches
	a.sourceControl.filterBranches()
	tt.Frame()
	wiringClick(r, tt, "fixture-feature")
	wiringWait(ctx, r, f, "workspace:checkoutBranch")
	if !wiringInstallSC(ctx, r, a, f) {
		return
	}
	r.check("branch picker changes actual branch", "fixture-feature", a.sourceControl.Branch)
	if err := os.WriteFile(filepath.Join(f.root, "notes.txt"), []byte("MUST NOT APPEAR IN CHECKPOINT\n"), 0o600); err != nil {
		r.check("prepare distinct live bytes", "success", err.Error())
		return
	}
	source := map[string]any{"lastTurn": map[string]any{"session_id": wiringHistorySession, "turn_id": "turn-1", "turn_count": 1}}
	var response reviewDiffResponse
	call, cancel = context.WithTimeout(ctx, 4*time.Second)
	err = scRequest(call, a.client, map[string]any{"type": "collectReviewDiff", "cwd": f.root, "source": source}, &response)
	cancel()
	if err != nil {
		r.check("checkpoint ref RPC", "success", err.Error())
		return
	}
	r.check("checkpoint excludes current worktree", []bool{true, true, false}, []bool{strings.Contains(response.Data.Patch, "-base line"), strings.Contains(response.Data.Patch, "+checkpoint line"), strings.Contains(response.Data.Patch, "MUST NOT APPEAR")})
	patches, err := editor.ParsePatch(response.Data.Patch)
	if err != nil || len(patches) != 1 {
		r.check("production checkpoint patch decode", 1, len(patches))
		return
	}
	reader := reviewReaderForPatch(patches[0], true, false, 3, false)
	r.check("production checkpoint reader literal change counts", []any{uint64(1), uint64(1), 1}, []any{reader.Additions, reader.Deletions, reader.Hunks})
	a.review = reviewState{Active: true, Scope: a.scScope(), TurnID: "turn-1", TurnCount: 1, Selected: "notes.txt", Files: []scFile{{Path: "notes.txt", Group: "checkpoint"}}, Patches: map[string]editor.FilePatch{"notes.txt": patches[0]}, Reader: reader}
	a.review.rebuildRows()
	tt = ui.NewTester(a.sourceControlView, 1400, 1000)
	r.check("history page labeled checkpoint read-only", true, tt.HasText("Turn 1") && tt.HasText("Read-only snapshot"))
	for _, label := range []string{"Stage", "Unstage", "Discard…", "Commit…"} {
		_, ok := tt.Access(label)
		r.check("checkpoint hides write "+label, false, ok)
	}
	before := f.count("workspace:stagePaths")
	a.scMutate("Stage", map[string]any{"type": "stagePaths", "paths": []string{"notes.txt"}})
	r.check("history cannot invoke live mutation", before, f.count("workspace:stagePaths"))
	if _, err := wiringGit(ctx, f.root, round, "update-ref", "-d", wiringCheckpoint); err != nil {
		r.check("remove owned checkpoint ref", "success", err.Error())
		return
	}
	call, cancel = context.WithTimeout(ctx, 4*time.Second)
	err = scRequest(call, a.client, map[string]any{"type": "collectReviewDiff", "cwd": f.root, "source": source}, &response)
	cancel()
	r.check("missing ref returns error instead of live diff", true, err != nil && strings.Contains(err.Error(), "Checkpoint ref missing"))
	a.review = reviewState{}
}

func wiringRead(root, path string) string {
	data, err := os.ReadFile(filepath.Join(root, path))
	if err != nil {
		return err.Error()
	}
	return string(data)
}
func wiringFileKeys(files []scFile) []string {
	keys := []string{}
	for _, file := range files {
		keys = append(keys, file.key())
	}
	return keys
}

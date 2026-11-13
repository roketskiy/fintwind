package backendworkspace

import (
	"context"
	"encoding/json"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"fintwind-go/internal/backendstore"
)

var mutationGate sync.Mutex

func dispatchGitMutation(ctx context.Context, root *confinedRoot, raw json.RawMessage) (json.RawMessage, bool, error) {
	c, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	kind := c.String("type")
	switch kind {
	case "stagePaths", "unstagePaths", "discardWorktreePaths", "checkoutBranch", "commit", "push", "fetchRemote", "pullFastForward", "captureRef", "restoreRef", "hasRef", "deleteRef":
	default:
		return nil, false, nil
	}
	defer root.close()
	cwd, _, err := root.resolveDir(c.String("cwd"))
	if err != nil {
		return nil, true, err
	}
	top, ok, err := gitToplevel(ctx, cwd)
	if err != nil {
		return nil, true, err
	}
	if !ok {
		return nil, true, errors.New("workspace is not a Git repository")
	}
	if _, _, err := root.resolveDir(top); err != nil {
		return nil, true, errors.New("repository root outside isolated workspace")
	}
	mutationGate.Lock()
	defer mutationGate.Unlock()
	ack := func() (json.RawMessage, bool, error) {
		raw, err := encodeResult(map[string]string{"type": "ack"})
		return raw, true, err
	}
	run := func(args ...string) (gitOutput, error) { return runGit(ctx, cwd, args) }
	paths := func(key string) ([]string, error) {
		var values []string
		if err := json.Unmarshal(c[key], &values); err != nil {
			return nil, err
		}
		if len(values) > 10000 {
			return nil, errors.New("too many Git paths")
		}
		for _, path := range values {
			if !fs.ValidPath(filepath.ToSlash(path)) || filepath.IsAbs(path) || strings.ContainsRune(path, 0) {
				return nil, errors.New("invalid repository-relative path")
			}
		}
		return values, nil
	}
	switch kind {
	case "stagePaths", "unstagePaths":
		values, err := paths("paths")
		if err != nil {
			return nil, true, err
		}
		if len(values) == 0 {
			return ack()
		}
		args := []string{"add", "--"}
		if kind == "unstagePaths" {
			args = []string{"reset", "-q", "HEAD", "--"}
		}
		for _, path := range values {
			args = append(args, ":(literal)"+path)
		}
		if _, err := run(args...); err != nil {
			return nil, true, err
		}
		return ack()
	case "discardWorktreePaths":
		tracked, err := paths("tracked")
		if err != nil {
			return nil, true, err
		}
		untracked, err := paths("untracked")
		if err != nil {
			return nil, true, err
		}
		if len(tracked) > 0 {
			args := []string{"restore", "--worktree", "--"}
			for _, path := range tracked {
				args = append(args, ":(literal)"+path)
			}
			if _, err := run(args...); err != nil {
				return nil, true, err
			}
		}
		handle, err := os.OpenRoot(cwd)
		if err != nil {
			return nil, true, err
		}
		defer handle.Close()
		for _, path := range untracked {
			if err := handle.RemoveAll(path); err != nil {
				return nil, true, err
			}
		}
		return ack()
	case "checkoutBranch":
		branch := c.String("branch")
		if branch == "" || strings.HasPrefix(branch, "-") {
			return nil, true, errors.New("invalid branch")
		}
		if _, err := run("check-ref-format", "--branch", branch); err != nil {
			return nil, true, err
		}
		args := []string{"switch"}
		if c.Bool("create") {
			args = append(args, "-c")
		}
		args = append(args, branch)
		if _, err := run(args...); err != nil {
			return nil, true, err
		}
		operation, _ := json.Marshal(map[string]string{"type": "inspectBranchRefs", "cwd": cwd})
		confined, err := confineRoot(root.path)
		if err != nil {
			return nil, true, err
		}
		response, err := dispatchInspectBranchRefs(ctx, confined, operation)
		if err != nil {
			return nil, true, err
		}
		object, err := backendstore.Decode(response)
		if err != nil {
			return nil, true, err
		}
		object.Set("type", "branchChanged")
		response, err = json.Marshal(object)
		return response, true, err
	case "commit":
		message := strings.TrimSpace(c.String("message"))
		if message == "" {
			return nil, true, errors.New("commit message required")
		}
		if c.Bool("includeUnstaged") {
			if _, err := run("add", "-A", "--", "."); err != nil {
				return nil, true, err
			}
		}
		if _, err := run("commit", "--no-gpg-sign", "-m", message); err != nil {
			return nil, true, err
		}
		if c.Bool("push") {
			if err := remoteOperation(ctx, root, cwd, "push"); err != nil {
				return nil, true, err
			}
		}
		return ack()
	case "fetchRemote", "pullFastForward", "push":
		if err := remoteOperation(ctx, root, cwd, kind); err != nil {
			return nil, true, err
		}
		return ack()
	case "captureRef", "restoreRef", "hasRef", "deleteRef":
		ref := c.String("gitRef")
		if !strings.HasPrefix(ref, "refs/fintwind/") || strings.ContainsAny(ref, "\r\n\x00") {
			return nil, true, errors.New("checkpoint ref required")
		}
		if _, err := run("check-ref-format", ref); err != nil {
			return nil, true, err
		}
		if kind == "hasRef" {
			_, err := run("rev-parse", "--verify", ref)
			response, encodeErr := encodeResult(map[string]any{"type": "bool", "value": err == nil})
			return response, true, encodeErr
		}
		if kind == "deleteRef" {
			if _, err := run("update-ref", "-d", ref); err != nil {
				return nil, true, err
			}
			return ack()
		}
		if kind == "restoreRef" {
			if err := restoreCheckpoint(ctx, cwd, ref); err != nil {
				return nil, true, err
			}
			return ack()
		}
		commit, err := snapshotCommit(ctx, root, cwd, resolveCommit(ctx, cwd, "HEAD"), "fintwind worktree snapshot", nil)
		if err != nil {
			return nil, true, err
		}
		if _, err := run("update-ref", ref, commit); err != nil {
			return nil, true, err
		}
		return ack()
	}
	return nil, true, errors.New("Git mutation missing")
}

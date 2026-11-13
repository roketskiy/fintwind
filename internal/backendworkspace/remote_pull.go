package backendworkspace

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"
)

type remoteFileLock struct {
	root      *os.Root
	path      string
	file      *os.File
	published bool
}

func (l *remoteFileLock) close() {
	if l.file != nil {
		l.file.Close()
		l.file = nil
	}
	if !l.published {
		_ = l.root.Remove(l.path)
	}
}

func remoteGitPath(ctx context.Context, root *confinedRoot, cwd, name string) (string, error) {
	path, err := gitText(ctx, cwd, "rev-parse", "--git-path", name)
	if err != nil {
		return "", err
	}
	path = strings.TrimSpace(path)
	if !filepath.IsAbs(path) {
		path = filepath.Join(cwd, path)
	}
	rel, err := filepath.Rel(root.path, path)
	if err != nil || !filepath.IsLocal(rel) {
		return "", errors.New("Git metadata outside isolated workspace")
	}
	return rel, nil
}

func lockRemoteFile(root *confinedRoot, path string) (*remoteFileLock, error) {
	if err := root.handle.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, err
	}
	file, err := root.handle.OpenFile(path+".lock", os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return nil, errors.New("Git metadata is locked; no fast-forward was applied")
	}
	return &remoteFileLock{root: root.handle, path: path + ".lock", file: file}, nil
}

func (l *remoteFileLock) publish(target string, data []byte) error {
	if _, err := l.file.Write(data); err != nil {
		return err
	}
	if err := l.file.Sync(); err != nil {
		return err
	}
	if err := l.file.Close(); err != nil {
		l.file = nil
		return err
	}
	l.file = nil
	if err := l.root.Rename(l.path, target); err != nil {
		return err
	}
	l.published = true
	return nil
}

// Hold the ordinary Git index, HEAD and branch lockfiles during checkout and
// publication. Other Git clients must fail before changing this worktree or
// branch; a daemon-local mutex alone cannot provide that guarantee.
func checkRemoteOperationState(ctx context.Context, root *confinedRoot, cwd string) error {
	for _, name := range []string{"MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "rebase-merge", "rebase-apply", "sequencer"} {
		path, e := remoteGitPath(ctx, root, cwd, name)
		if e != nil {
			return e
		}
		if _, e := root.handle.Stat(path); e == nil {
			return errors.New("another Git operation is in progress")
		} else if !errors.Is(e, os.ErrNotExist) {
			return errors.New("Git operation state unavailable")
		}
	}
	return nil
}

func remoteFastForward(ctx context.Context, root *confinedRoot, t *remoteTransport, cwd, branch, head, next string) (resultErr error) {
	if err := checkRemoteOperationState(ctx, root, cwd); err != nil {
		return err
	}
	index, err := remoteGitPath(ctx, root, cwd, "index")
	if err != nil {
		return err
	}
	headPath, err := remoteGitPath(ctx, root, cwd, "HEAD")
	if err != nil {
		return err
	}
	refPath, err := remoteGitPath(ctx, root, cwd, "refs/heads/"+branch)
	if err != nil {
		return err
	}
	indexLock, err := lockRemoteFile(root, index)
	if err != nil {
		return err
	}
	defer indexLock.close()
	headLock, err := lockRemoteFile(root, headPath)
	if err != nil {
		return err
	}
	defer headLock.close()
	refLock, err := lockRemoteFile(root, refPath)
	if err != nil {
		return err
	}
	defer refLock.close()
	currentBranch, err := gitText(ctx, cwd, "symbolic-ref", "--quiet", "HEAD")
	if err != nil || strings.TrimSpace(currentBranch) != "refs/heads/"+branch {
		return errors.New("branch changed during fast-forward")
	}
	currentHead, err := gitText(ctx, cwd, "rev-parse", "--verify", "HEAD")
	if err != nil || strings.TrimSpace(currentHead) != head {
		return errors.New("HEAD changed during fast-forward")
	}
	if err := checkRemoteOperationState(ctx, root, cwd); err != nil {
		return err
	}
	file, err := root.handle.Open(index)
	if err != nil {
		return err
	}
	before, err := io.ReadAll(io.LimitReader(file, (64<<20)+1))
	file.Close()
	if err != nil || len(before) > 64<<20 {
		return errors.New("Git index exceeds fast-forward budget")
	}
	temporary := filepath.Join(t.directory, "checkout-index")
	if err := os.WriteFile(temporary, before, 0o600); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(t.directory, "HEAD"), []byte(head+"\n"), 0o600); err != nil {
		return err
	}
	t.environment["GIT_WORK_TREE"] = cwd
	t.environment["GIT_INDEX_FILE"] = temporary
	t.environment["GIT_CONFIG_COUNT"] = "1"
	t.environment["GIT_CONFIG_KEY_0"] = "core.bare"
	t.environment["GIT_CONFIG_VALUE_0"] = "false"
	// Keep harmless checkout conversion settings consistent with the existing
	// index. Never copy filter programs, includes, hooks or external attributes.
	_, config, err := readRemotePlans(ctx, root, cwd)
	if err != nil {
		return err
	}
	for key := range config {
		if strings.HasPrefix(key, "filter.") || key == "core.attributesfile" || key == "include.path" || strings.HasPrefix(key, "includeif.") {
			return errors.New("fast-forward checkout configuration changed; inspect before retrying")
		}
	}
	count := 1
	for _, key := range []string{"core.autocrlf", "core.eol", "core.safecrlf", "core.filemode", "core.ignorecase", "core.symlinks"} {
		values := config[key]
		if len(values) == 0 {
			continue
		}
		if len(values) != 1 {
			return errors.New("ambiguous Git checkout configuration")
		}
		t.environment[fmt.Sprintf("GIT_CONFIG_KEY_%d", count)] = key
		t.environment[fmt.Sprintf("GIT_CONFIG_VALUE_%d", count)] = values[0]
		count++
	}
	t.environment["GIT_CONFIG_COUNT"] = fmt.Sprint(count)
	if err := rejectRemoteFilters(ctx, root, t, cwd, head, next); err != nil {
		return err
	}
	status, err := t.run(ctx, cwd, "status", "--porcelain=v1", "--untracked-files=no")
	if err != nil {
		return err
	}
	if strings.TrimSpace(string(status.stdout)) != "" {
		return errors.New("fast-forward requires a clean tracked worktree")
	}
	recovery, err := beginRemoteRecovery(ctx, root, cwd, branch, head, next, before)
	if err != nil {
		return err
	}
	complete := false
	defer func() {
		if complete {
			recovery.remove()
		} else if resultErr != nil {
			resultErr = recovery.failure(resultErr)
		}
	}()
	if _, err := t.run(ctx, cwd, "read-tree", "-m", "-u", head, next); err != nil {
		_ = recovery.stage("checkout-failed")
		return err
	}
	updatedIndex, err := os.Open(temporary)
	if err != nil {
		return errors.New("fast-forward checkout needs reconciliation")
	}
	after, err := io.ReadAll(io.LimitReader(updatedIndex, (64<<20)+1))
	updatedIndex.Close()
	if err != nil || len(after) > 64<<20 {
		return errors.New("fast-forward checkout needs reconciliation")
	}
	if err := recovery.write("after-index", after); err != nil {
		return errors.New("fast-forward recovery index could not be saved")
	}
	if err := recovery.stage("checkout-complete"); err != nil {
		return errors.New("fast-forward recovery state could not be saved")
	}
	// The ref lock excludes update-ref CAS races while the index lock excludes
	// commit/checkout races. Publish the new loose ref over any packed ref.
	if err := refLock.publish(refPath, []byte(next+"\n")); err != nil {
		return errors.New("fast-forward ref publication failed; inspect before retrying")
	}
	if err := recovery.stage("ref-published"); err != nil {
		return errors.New("fast-forward ref published but recovery state could not be saved")
	}
	if err := indexLock.publish(index, after); err != nil {
		return errors.New("fast-forward index publication failed; inspect before retrying")
	}
	if err := recovery.stage("index-published"); err != nil {
		return errors.New("fast-forward index published but recovery state could not be saved")
	}
	for _, name := range []string{"logs/HEAD", "logs/refs/heads/" + branch} {
		path, e := remoteGitPath(ctx, root, cwd, name)
		if e != nil {
			return errors.New("fast-forward succeeded but reflog needs reconciliation")
		}
		if e := root.handle.MkdirAll(filepath.Dir(path), 0o700); e != nil {
			return errors.New("fast-forward succeeded but reflog needs reconciliation")
		}
		file, e := root.handle.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
		if e != nil {
			return errors.New("fast-forward succeeded but reflog needs reconciliation")
		}
		_, e = fmt.Fprintf(file, "%s %s Fintwind <fintwind@localhost> %d +0000\tpull: fast-forward\n", head, next, time.Now().Unix())
		closeErr := file.Close()
		if e != nil || closeErr != nil {
			return errors.New("fast-forward succeeded but reflog needs reconciliation")
		}
	}
	if err := recovery.stage("completed"); err != nil {
		return errors.New("fast-forward completed but recovery state could not be finalized")
	}
	complete = true
	return nil
}

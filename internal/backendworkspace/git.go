package backendworkspace

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/ownedprocess"
)

const (
	gitTimeout     = 30 * time.Second
	gitStdoutCap   = 16 << 20
	gitStderrKeep  = 4096
	maxCommitLimit = 200
	// maxPagedCommitBound bounds how far a paging client may walk so one
	// window at a time cannot accumulate an unbounded history read.
	maxPagedCommitBound = 2000
)

// gitGate bounds Git concurrency: at most four Git children at once.
var gitGate = make(chan struct{}, 4)

var (
	gitPathOnce sync.Once
	gitPath     string
	gitPathErr  error
)

func findGit() (string, error) {
	gitPathOnce.Do(func() {
		found, err := exec.LookPath("git")
		if err != nil {
			gitPathErr = fmt.Errorf("git_not_available: git executable is not on PATH")
			return
		}
		absolute, err := filepath.Abs(found)
		if err != nil {
			gitPathErr = fmt.Errorf("git_not_available: %w", err)
			return
		}
		gitPath = absolute
	})
	if gitPathErr != nil {
		return "", gitPathErr
	}
	return gitPath, nil
}

type gitOutput struct {
	stdout []byte
	stderr string
}

type gitExitError struct {
	code       uint32
	diagnostic string
}

func (e *gitExitError) Error() string {
	if e.diagnostic == "" {
		return fmt.Sprintf("git exited with code %d", e.code)
	}
	return fmt.Sprintf("git exited with code %d: %s", e.code, e.diagnostic)
}

// gitArgsAllowlisted reports whether argv is one of the explicit
// invocations this module permits. Anything else never spawns.
func gitArgsAllowlisted(args []string) bool {
	if len(args) == 0 {
		return false
	}
	switch args[0] {
	case "ls-tree", "check-attr":
		for _, arg := range args {
			if strings.ContainsRune(arg, 0) {
				return false
			}
		}
		return true
	case "rev-parse", "branch", "for-each-ref", "symbolic-ref", "log", "rev-list", "remote", "show", "ls-files", "status", "diff", "add", "reset", "restore", "checkout", "switch", "commit", "fetch", "pull", "push", "read-tree", "write-tree", "commit-tree", "update-ref", "check-ref-format", "worktree", "merge-tree", "merge-base", "clean", "config", "check-ignore":
		for _, arg := range args {
			if strings.ContainsRune(arg, 0) {
				return false
			}
		}
		return true
	default:
		return false
	}
}

// runGit executes one allowlisted Git invocation inside an
// ownedprocess kill-on-close Job with an explicit environment and a
// timeout. Stdout lands in a scratch file under the system temp area
// that this call owns and removes. Mutating verbs are reachable only from
// explicit operations. ctx cancellation terminates the Job through its handle.
func runGit(ctx context.Context, cwd string, args []string) (gitOutput, error) {
	return runGitEnvironment(ctx, cwd, args, nil)
}

func runGitEnvironment(ctx context.Context, cwd string, args []string, extra map[string]string) (gitOutput, error) {
	return runGitInput(ctx, cwd, args, extra, nil)
}

func runGitInput(ctx context.Context, cwd string, args []string, extra map[string]string, input []byte) (gitOutput, error) {
	return runGitOutput(ctx, cwd, args, extra, input, false)
}

func runGitOutput(ctx context.Context, cwd string, args []string, extra map[string]string, input []byte, diffExit bool) (gitOutput, error) {
	if !gitArgsAllowlisted(args) {
		return gitOutput{}, fmt.Errorf("refusing to run non-allowlisted git invocation")
	}
	binary, err := findGit()
	if err != nil {
		return gitOutput{}, err
	}
	select {
	case gitGate <- struct{}{}:
	case <-ctx.Done():
		return gitOutput{}, ctx.Err()
	}
	defer func() { <-gitGate }()

	scratch, err := os.MkdirTemp("", "fintwind-ws-git-*")
	if err != nil {
		return gitOutput{}, fmt.Errorf("git scratch: %w", err)
	}
	defer os.RemoveAll(scratch)
	for _, sub := range []string{"tmp", "home"} {
		if err := os.MkdirAll(filepath.Join(scratch, sub), 0o700); err != nil {
			return gitOutput{}, fmt.Errorf("git scratch: %w", err)
		}
	}
	logs := filepath.Join(scratch, "logs")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return gitOutput{}, fmt.Errorf("git scratch: %w", err)
	}
	values := map[string]string{"LC_ALL": "C", "LANG": "C", "GIT_TERMINAL_PROMPT": "0", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.DevNull, "GIT_AUTHOR_NAME": "Fintwind", "GIT_AUTHOR_EMAIL": "fintwind@localhost", "GIT_COMMITTER_NAME": "Fintwind", "GIT_COMMITTER_EMAIL": "fintwind@localhost"}
	for key, value := range extra {
		values[key] = value
	}
	env := ownedprocess.BuildEnv(scratch, values)
	argv := append([]string{"-c", "core.hooksPath=" + os.DevNull, "-c", "protocol.ext.allow=never", "-c", "protocol.file.allow=always"}, args...)
	if os.Getenv("FINTWIND_MYGO_PROFILE") == "daily" {
		dailyValues := map[string]string{"LC_ALL": "C", "LANG": "C"}
		for key, value := range extra {
			dailyValues[key] = value
		}
		env = ownedprocess.InheritEnv(dailyValues)
		argv = args
	}
	var child *ownedprocess.Owned
	if input == nil {
		child, err = ownedprocess.StartWithEnvironment(binary, argv, logs, cwd, env)
	} else {
		child, err = ownedprocess.StartWithInput(binary, argv, logs, cwd, env, input)
	}
	if err != nil {
		return gitOutput{}, fmt.Errorf("start git: %w", err)
	}
	defer child.Close()
	budget := gitTimeout
	if len(args) > 0 && (args[0] == "fetch" || args[0] == "push" || args[0] == "pull") {
		budget = 120 * time.Second
	}
	deadline := time.Now().Add(budget)
	for {
		done, err := child.Exited(50)
		if err != nil {
			return gitOutput{}, fmt.Errorf("wait git: %w", err)
		}
		if done {
			break
		}
		for _, path := range []string{child.StdoutPath, child.StderrPath} {
			if info, e := os.Stat(path); e == nil && info.Size() > gitStdoutCap {
				return gitOutput{}, fmt.Errorf("git output exceeds the %d-byte local bound", gitStdoutCap)
			}
		}
		if ctx.Err() != nil {
			return gitOutput{}, ctx.Err()
		}
		if time.Now().After(deadline) {
			return gitOutput{}, fmt.Errorf("git timed out after %s", budget)
		}
	}
	if err := ctx.Err(); err != nil {
		return gitOutput{}, err
	}
	code, err := child.ExitCode()
	if err != nil {
		return gitOutput{}, fmt.Errorf("git exit code: %w", err)
	}
	stdout, err := readGitBounded(child.StdoutPath, gitStdoutCap+1)
	if err != nil {
		return gitOutput{}, fmt.Errorf("read git stdout: %w", err)
	}
	if len(stdout) > gitStdoutCap {
		return gitOutput{}, fmt.Errorf("git output exceeds the %d-byte local bound", gitStdoutCap)
	}
	stderr, _ := readGitBounded(child.StderrPath, gitStderrKeep)
	kept := strings.TrimSpace(string(stderr))
	if len(kept) > gitStderrKeep {
		kept = kept[:gitStderrKeep]
	}
	if code != 0 && !(diffExit && code == 1) {
		return gitOutput{}, &gitExitError{code: code, diagnostic: kept}
	}
	return gitOutput{stdout: stdout, stderr: kept}, nil
}

func readGitBounded(path string, limit int64) ([]byte, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	return io.ReadAll(io.LimitReader(file, limit))
}

// gitToplevel resolves cwd to its repository top level, or reports
// ok=false when cwd is not inside a Git repository.
func gitToplevel(ctx context.Context, cwd string) (string, bool, error) {
	out, err := runGit(ctx, cwd, []string{"rev-parse", "--show-toplevel"})
	if err != nil {
		if ctx.Err() != nil {
			return "", false, ctx.Err()
		}
		var exit *gitExitError
		if errors.As(err, &exit) && exit.code == 128 && strings.HasPrefix(exit.diagnostic, "fatal: not a git repository") {
			return "", false, nil
		}
		return "", false, err
	}
	top := strings.TrimSpace(string(out.stdout))
	if top == "" {
		return "", false, errors.New("Git repository probe returned an empty path")
	}
	return top, true, nil
}

// gitListedFiles is the fast path: tracked plus unignored untracked files
// from one subprocess. It applies only when the
// operation root is the repository top level, so reported paths stay
// relative to the requested root.
func gitListedFiles(ctx context.Context, root *confinedRoot, baseRel string, cap int) ([]string, error) {
	absBase := entryAbs(root.path, ifEmpty(baseRel, "."))
	top, ok, err := gitToplevel(ctx, absBase)
	if err != nil {
		return nil, err
	}
	if !ok {
		return nil, nil
	}
	topCanonical, err := filepath.EvalSymlinks(top)
	if err != nil {
		return nil, nil
	}
	baseCanonical, err := filepath.EvalSymlinks(absBase)
	if err != nil {
		return nil, fmt.Errorf("operation root is not usable: %w", err)
	}
	if !samePath(topCanonical, baseCanonical) {
		return nil, nil
	}
	out, err := runGit(ctx, absBase, []string{"ls-files", "--cached", "--others", "--exclude-standard", "-z"})
	if err != nil {
		return nil, nil
	}
	raw := out.stdout
	if len(raw) > 0 && raw[len(raw)-1] == 0 {
		raw = raw[:len(raw)-1]
	}
	files := []string{}
	for _, chunk := range strings.Split(string(raw), "\x00") {
		if chunk == "" {
			continue
		}
		files = append(files, filepath.ToSlash(chunk))
		if len(files) >= cap {
			break
		}
	}
	return files, nil
}

func samePath(a, b string) bool {
	if a == b {
		return true
	}
	rel, err := filepath.Rel(a, b)
	return err == nil && rel == "."
}

type currentBranchOp struct {
	Type string `json:"type"`
	Cwd  string `json:"cwd"`
}

type currentBranchResult struct {
	Type   string  `json:"type"`
	Branch *string `json:"branch"`
}

func dispatchCurrentBranch(ctx context.Context, root *confinedRoot, operation json.RawMessage) (json.RawMessage, error) {
	defer root.close()
	var op currentBranchOp
	if err := decodeOperation(operation, &op, opCurrentBranch); err != nil {
		return nil, err
	}
	cwd, _, err := root.resolveDir(op.Cwd)
	if err != nil {
		return nil, err
	}
	if _, ok, err := gitToplevel(ctx, cwd); err != nil {
		return nil, err
	} else if !ok {
		return encodeResult(currentBranchResult{Type: "currentBranch", Branch: nil})
	}
	if out, err := runGit(ctx, cwd, []string{"branch", "--show-current"}); err == nil {
		if branch := strings.TrimSpace(string(out.stdout)); branch != "" {
			return encodeResult(currentBranchResult{Type: "currentBranch", Branch: &branch})
		}
	}
	if out, err := runGit(ctx, cwd, []string{"rev-parse", "--short", "HEAD"}); err == nil {
		if head := strings.TrimSpace(string(out.stdout)); head != "" {
			return encodeResult(currentBranchResult{Type: "currentBranch", Branch: &head})
		}
	}
	return encodeResult(currentBranchResult{Type: "currentBranch", Branch: nil})
}

type branchEntry struct {
	Name               string `json:"name"`
	CheckedOutElsehere bool   `json:"checkedOutElsewhere"`
}

type branchSnapshot struct {
	Repository    string        `json:"repository"`
	Current       *string       `json:"current"`
	DetachedHead  *string       `json:"detachedHead"`
	DefaultBranch *string       `json:"defaultBranch"`
	Branches      []branchEntry `json:"branches"`
	Additions     uint64        `json:"additions"`
	Deletions     uint64        `json:"deletions"`
}

type branchesResult struct {
	Type     string          `json:"type"`
	Snapshot *branchSnapshot `json:"snapshot"`
}

type inspectBranchRefsOp struct {
	Type string `json:"type"`
	Cwd  string `json:"cwd"`
}

// dispatchInspectBranchRefs is the cheap branch menu read: refs plus
// current checkout, with line counts left at zero.
func dispatchInspectBranchRefs(ctx context.Context, root *confinedRoot, operation json.RawMessage) (json.RawMessage, error) {
	defer root.close()
	var op inspectBranchRefsOp
	if err := decodeOperation(operation, &op, opInspectBranchRefs); err != nil {
		return nil, err
	}
	cwd, _, err := root.resolveDir(op.Cwd)
	if err != nil {
		return nil, err
	}
	top, ok, err := gitToplevel(ctx, cwd)
	if err != nil {
		return nil, err
	}
	if !ok {
		return encodeResult(branchesResult{Type: "branches", Snapshot: nil})
	}
	repository := top
	if canonical, err := filepath.EvalSymlinks(top); err == nil {
		repository = canonical
	}
	var current *string
	if out, err := runGit(ctx, cwd, []string{"branch", "--show-current"}); err == nil {
		if branch := strings.TrimSpace(string(out.stdout)); branch != "" {
			current = &branch
		}
	}
	var detached *string
	if current == nil {
		if out, err := runGit(ctx, cwd, []string{"rev-parse", "--short", "HEAD"}); err == nil {
			if head := strings.TrimSpace(string(out.stdout)); head != "" {
				detached = &head
			}
		}
	}
	refsOut, err := runGit(ctx, cwd, []string{"for-each-ref", "--format=%(refname:short)%00%(worktreepath)", "refs/heads"})
	if err != nil {
		return nil, err
	}
	branches := []branchEntry{}
	for _, line := range strings.Split(string(refsOut.stdout), "\n") {
		line = strings.TrimSuffix(line, "\r")
		if line == "" {
			continue
		}
		name, worktree, _ := strings.Cut(line, "\x00")
		if name == "" {
			continue
		}
		checkedOut := false
		if worktree != "" {
			linked := worktree
			if canonical, err := filepath.EvalSymlinks(worktree); err == nil {
				linked = canonical
			}
			checkedOut = !samePath(linked, repository)
		}
		branches = append(branches, branchEntry{Name: name, CheckedOutElsehere: checkedOut})
	}
	sort.Slice(branches, func(i, j int) bool {
		iCurrent := current != nil && branches[i].Name == *current
		jCurrent := current != nil && branches[j].Name == *current
		if iCurrent != jCurrent {
			return iCurrent
		}
		return branches[i].Name < branches[j].Name
	})
	var fallback *string
	if out, err := runGit(ctx, cwd, []string{"symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"}); err == nil {
		if ref := strings.TrimSpace(string(out.stdout)); strings.HasPrefix(ref, "origin/") {
			candidate := strings.TrimPrefix(ref, "origin/")
			for _, branch := range branches {
				if branch.Name == candidate {
					fallback = &candidate
					break
				}
			}
		}
	}
	if fallback == nil {
		fallback = current
	}
	return encodeResult(branchesResult{Type: "branches", Snapshot: &branchSnapshot{
		Repository:    repository,
		Current:       current,
		DetachedHead:  detached,
		DefaultBranch: fallback,
		Branches:      branches,
	}})
}

type listCommitsOp struct {
	Type   string  `json:"type"`
	Cwd    string  `json:"cwd"`
	Limit  int     `json:"limit"`
	Skip   int     `json:"skip"`
	Branch *string `json:"branch"`
}

type commitRef struct {
	Label  string `json:"label"`
	Remote bool   `json:"remote"`
	Head   bool   `json:"head"`
}

type commitEntry struct {
	Hash      string      `json:"hash"`
	Parents   []string    `json:"parents"`
	ShortHash string      `json:"shortHash"`
	Subject   string      `json:"subject"`
	Author    string      `json:"author"`
	Timestamp uint64      `json:"timestamp"`
	Refs      []commitRef `json:"refs"`
	IsHead    bool        `json:"isHead"`
	Pushed    bool        `json:"pushed"`
}

type commitsResult struct {
	Type    string        `json:"type"`
	Commits []commitEntry `json:"commits"`
}

// dispatchListCommits reads newest-first history. The
// pushed flag classifies exactly the displayed window through one more
// local rev-list call; a repository without commits reads back empty.
func dispatchListCommits(ctx context.Context, root *confinedRoot, operation json.RawMessage) (json.RawMessage, error) {
	defer root.close()
	var op listCommitsOp
	if err := decodeOperation(operation, &op, opListCommits); err != nil {
		return nil, err
	}
	if op.Limit <= 0 {
		return nil, fmt.Errorf("limit must be positive")
	}
	if op.Limit > maxCommitLimit {
		return nil, fmt.Errorf("limit exceeds the %d-commit local history bound", maxCommitLimit)
	}
	// Paged reads stay bounded per call; the aggregate cap keeps a paging
	// client from walking an unbounded history one window at a time.
	if op.Skip < 0 || op.Skip+op.Limit > maxPagedCommitBound {
		return nil, fmt.Errorf("skip exceeds the %d-commit paged history bound", maxPagedCommitBound)
	}
	cwd, _, err := root.resolveDir(op.Cwd)
	if err != nil {
		return nil, err
	}
	if _, ok, err := gitToplevel(ctx, cwd); err != nil {
		return nil, err
	} else if !ok {
		return encodeResult(commitsResult{Type: "commits", Commits: nil})
	}
	var remotes []string
	if out, err := runGit(ctx, cwd, []string{"remote"}); err == nil {
		for _, line := range strings.Split(string(out.stdout), "\n") {
			if name := strings.TrimSpace(line); name != "" {
				remotes = append(remotes, name)
			}
		}
	}
	args := []string{"log", "--topo-order", "-n", strconv.Itoa(op.Limit)}
	if op.Skip > 0 {
		args = append(args, "--skip="+strconv.Itoa(op.Skip))
	}
	if op.Branch != nil && *op.Branch != "" {
		branch := *op.Branch
		if strings.HasPrefix(branch, "-") || strings.ContainsAny(branch, "\x00\n\r") {
			return nil, fmt.Errorf("invalid branch revision")
		}
		args = append(args, branch)
	} else {
		args = append(args, "--branches", "--remotes", "--tags")
	}
	args = append(args, "--pretty=format:%H%x1f%h%x1f%s%x1f%an%x1f%at%x1f%D%x1f%P%x1e", "--")
	out, err := runGit(ctx, cwd, args)
	if err != nil {
		if strings.Contains(err.Error(), "does not have any commits yet") {
			return encodeResult(commitsResult{Type: "commits", Commits: []commitEntry{}})
		}
		return nil, err
	}
	commits := []commitEntry{}
	for _, record := range strings.Split(string(out.stdout), "\x1e") {
		record = strings.Trim(record, "\n")
		if record == "" {
			continue
		}
		fields := strings.Split(record, "\x1f")
		if len(fields) != 7 {
			continue
		}
		timestamp, err := strconv.ParseUint(fields[4], 10, 64)
		if err != nil {
			continue
		}
		refs := []commitRef{}
		isHead := false
		for _, label := range strings.Split(fields[5], ", ") {
			label = strings.TrimSpace(label)
			if label == "" {
				continue
			}
			head := label == "HEAD" || strings.HasPrefix(label, "HEAD ->")
			remote := false
			for _, name := range remotes {
				if strings.HasPrefix(label, name+"/") {
					remote = true
					break
				}
			}
			refs = append(refs, commitRef{Label: label, Remote: remote, Head: head})
			isHead = isHead || head
		}
		if refs == nil {
			refs = []commitRef{}
		}
		parents := strings.Fields(fields[6])
		if parents == nil {
			parents = []string{}
		}
		commits = append(commits, commitEntry{
			Hash:      fields[0],
			Parents:   parents,
			ShortHash: fields[1],
			Subject:   fields[2],
			Author:    fields[3],
			Timestamp: timestamp,
			Refs:      refs,
			IsHead:    isHead,
		})
	}
	if len(commits) == 0 {
		return encodeResult(commitsResult{Type: "commits", Commits: []commitEntry{}})
	}
	pushed, err := classifyPushed(ctx, cwd, commits)
	if err != nil {
		return nil, err
	}
	for i := range commits {
		commits[i].Pushed = pushed[commits[i].Hash]
	}
	return encodeResult(commitsResult{Type: "commits", Commits: commits})
}

// classifyPushed marks window commits reachable from any remote ref.
// Boundary parents stay excluded with explicit "^", so the command-line
// --not applies only to --remotes and the window cannot grow.
func classifyPushed(ctx context.Context, cwd string, commits []commitEntry) (map[string]bool, error) {
	visible := map[string]bool{}
	for _, commit := range commits {
		visible[commit.Hash] = true
	}
	boundary := map[string]bool{}
	for _, commit := range commits {
		for _, parent := range commit.Parents {
			if !visible[parent] {
				boundary[parent] = true
			}
		}
	}
	args := []string{"rev-list"}
	for _, commit := range commits {
		args = append(args, commit.Hash)
	}
	for parent := range boundary {
		args = append(args, "^"+parent)
	}
	args = append(args, "--not", "--remotes")
	out, err := runGit(ctx, cwd, args)
	if err != nil {
		return nil, err
	}
	unpushed := map[string]bool{}
	for _, line := range strings.Split(string(out.stdout), "\n") {
		if hash := strings.TrimSpace(line); hash != "" {
			unpushed[hash] = true
		}
	}
	pushed := map[string]bool{}
	for _, commit := range commits {
		pushed[commit.Hash] = !unpushed[commit.Hash]
	}
	return pushed, nil
}

type readCommitStatsOp struct {
	Type string `json:"type"`
	Cwd  string `json:"cwd"`
	Hash string `json:"hash"`
}

type commitStats struct {
	Files     uint64 `json:"files"`
	Additions uint64 `json:"additions"`
	Deletions uint64 `json:"deletions"`
}

type commitStatsResult struct {
	Type  string      `json:"type"`
	Stats commitStats `json:"stats"`
}

// dispatchReadCommitStats reads one commit with numstat: NUL-delimited,
// binary files count as files without line counts.
func dispatchReadCommitStats(ctx context.Context, root *confinedRoot, operation json.RawMessage) (json.RawMessage, error) {
	defer root.close()
	var op readCommitStatsOp
	if err := decodeOperation(operation, &op, opReadCommitStats); err != nil {
		return nil, err
	}
	if len(op.Hash) != 40 && len(op.Hash) != 64 {
		return nil, fmt.Errorf("invalid commit hash")
	}
	for _, c := range op.Hash {
		if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')) {
			return nil, fmt.Errorf("invalid commit hash")
		}
	}
	cwd, _, err := root.resolveDir(op.Cwd)
	if err != nil {
		return nil, err
	}
	out, err := runGit(ctx, cwd, []string{
		"show", "--format=", "--numstat", "-z", "--root",
		"--diff-merges=first-parent", "--find-renames",
		"--no-ext-diff", "--no-textconv", op.Hash, "--",
	})
	if err != nil {
		return nil, err
	}
	stats := commitStats{}
	records := strings.Split(string(out.stdout), "\x00")
	for i := 0; i < len(records); i++ {
		record := records[i]
		if record == "" {
			continue
		}
		fields := strings.SplitN(record, "\t", 3)
		if len(fields) != 3 {
			return nil, fmt.Errorf("git returned an unreadable numstat record")
		}
		if fields[2] == "" {
			i += 2
			if i >= len(records) {
				return nil, fmt.Errorf("git returned a truncated rename record")
			}
		}
		stats.Files++
		if fields[0] == "-" || fields[1] == "-" {
			continue
		}
		added, err := strconv.ParseUint(fields[0], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("git returned an unreadable numstat record")
		}
		deleted, err := strconv.ParseUint(fields[1], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("git returned an unreadable numstat record")
		}
		stats.Additions += added
		stats.Deletions += deleted
	}
	return encodeResult(commitStatsResult{Type: "commitStats", Stats: stats})
}

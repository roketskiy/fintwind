package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
)

type harness struct {
	ctx       context.Context
	round     string
	daemonBin string
	client    *daemon.Client
	checks    map[string]bool
	skipped   []string
	git       string
	owner     interface{ Close() error }
	daemonSHA string
}

func main() {
	daemonBin := flag.String("daemon", "", "absolute isolated daemon binary")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 8*time.Minute)
	defer cancel()
	if err := run(ctx, *daemonBin); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, daemonBin string) (err error) {
	if !filepath.IsAbs(daemonBin) {
		return errors.New("absolute --daemon path required")
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	h := &harness{ctx: ctx, round: round, daemonBin: daemonBin, checks: map[string]bool{}}
	defer func() {
		report := map[string]any{"passed": err == nil, "checks": h.checks, "skipped": h.skipped, "realProvider": false, "oauth": false, "round": round, "daemonSHA256": h.daemonSHA}
		if err != nil {
			report["error"] = err.Error()
		}
		raw, marshalErr := json.MarshalIndent(report, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(round, "navigation-result.json"), raw, 0o600)
		}
		err = errors.Join(err, marshalErr)
		fmt.Println("[REPORT]", filepath.Join(round, "navigation-result.json"))
	}()
	if err := h.verifyNoCredentials(); err != nil {
		return err
	}
	if h.git, err = exec.LookPath("git"); err != nil {
		return err
	}
	if err = h.runGit(round, "version"); err != nil {
		return err
	}
	if err = h.startDaemon(); err != nil {
		return err
	}
	defer func() {
		if h.owner != nil {
			_ = h.owner.Close()
		}
	}()
	defer h.client.Close()
	if err = h.browseDirectories(); err != nil {
		return err
	}
	if err = h.nestedRepositories(); err != nil {
		return err
	}
	if err = h.nestedDiscoveryBounds(); err != nil {
		return err
	}
	if err = h.deleteSessionRefs(); err != nil {
		return err
	}
	return nil
}

func (h *harness) verifyNoCredentials() error {
	for _, entry := range ownedprocess.BuildEnv(h.round, map[string]string{"FINTWIND_DAEMON_TOKEN": "fixture-token"}) {
		key, _, _ := strings.Cut(entry, "=")
		switch strings.ToUpper(key) {
		case "FINTWIND_AGENT_REAL_PROVIDER", "DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN":
			return fmt.Errorf("credential variable %s crossed the owned child environment boundary", key)
		}
	}
	return nil
}

func (h *harness) startDaemon() error {
	token := uuid.NewString()
	logs := filepath.Join(h.round, "daemon-logs")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return err
	}
	data := filepath.Join(h.round, "data")
	child, err := ownedprocess.StartWithEnvironment(h.daemonBin, []string{"--data-dir", data, "--bind", "127.0.0.1:0"}, logs, h.round, ownedprocess.BuildEnv(h.round, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if err != nil {
		return err
	}
	readyCtx, cancel := context.WithTimeout(h.ctx, 20*time.Second)
	defer cancel()
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	for {
		content, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
			PID     uint32 `json:"pid"`
		}
		if json.Unmarshal(content, &ready) == nil && ready.PID == child.PID && ready.Address != "" {
			h.client, err = daemon.Dial(readyCtx, ready.Address, token)
			if err != nil {
				_ = child.Close()
				return err
			}
			h.checks["isolated-daemon-websocket-ready"] = true
			h.daemonSHA, err = sha256File(h.daemonBin)
			if err != nil {
				h.client.Close()
				_ = child.Close()
				return err
			}
			// Keep the owner alive until the harness and client have finished.
			h.owner = child
			return nil
		}
		if exited, _ := child.Exited(0); exited {
			_ = child.Close()
			return errors.New("isolated daemon exited before ready; inspect daemon-logs")
		}
		select {
		case <-readyCtx.Done():
			_ = child.Close()
			return fmt.Errorf("wait for isolated daemon: %w", readyCtx.Err())
		case <-ticker.C:
		}
	}
}

func (h *harness) runGit(directory string, args ...string) error {
	logDir, err := os.MkdirTemp(filepath.Join(h.round, "git-logs"), "git-")
	if err != nil {
		if mkdirErr := os.MkdirAll(filepath.Join(h.round, "git-logs"), 0o700); mkdirErr != nil {
			return mkdirErr
		}
		logDir, err = os.MkdirTemp(filepath.Join(h.round, "git-logs"), "git-")
		if err != nil {
			return err
		}
	}
	child, err := ownedprocess.StartWithEnvironment(h.git, args, logDir, directory, ownedprocess.BuildEnv(h.round, nil))
	if err != nil {
		return err
	}
	done, waitErr := child.Exited(10000)
	if waitErr != nil || !done {
		_ = child.Close()
		return fmt.Errorf("owned git command did not finish: %v", waitErr)
	}
	code, codeErr := child.ExitCode()
	stdout, _ := os.ReadFile(child.StdoutPath)
	stderr, _ := os.ReadFile(child.StderrPath)
	closeErr := child.Close()
	if codeErr != nil || code != 0 {
		return fmt.Errorf("git %s failed (exit %d): %s %s", strings.Join(args, " "), code, strings.TrimSpace(string(stdout)), strings.TrimSpace(string(stderr)))
	}
	return closeErr
}

func (h *harness) rpc(operation map[string]any) (json.RawMessage, error) {
	return h.client.Request(h.ctx, map[string]any{"type": "workspace", "operation": operation})
}

func unwrapWorkspace(raw json.RawMessage) (json.RawMessage, error) {
	var envelope map[string]json.RawMessage
	if err := json.Unmarshal(raw, &envelope); err != nil {
		return nil, err
	}
	result, ok := envelope["result"]
	if !ok {
		return raw, nil
	}
	return result, nil
}

func (h *harness) browseDirectories() error {
	root := filepath.Join(h.round, "workspace")
	inside := filepath.Join(root, "inside dir")
	if err := os.MkdirAll(inside, 0o700); err != nil {
		return err
	}
	if err := os.Mkdir(filepath.Join(root, ".hidden"), 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(root, "plain.txt"), []byte("fixture"), 0o600); err != nil {
		return err
	}
	baseReq := map[string]any{"type": "browseDirectories", "path": nil}
	raw, err := h.rpc(baseReq)
	if err != nil {
		return fmt.Errorf("browseDirectories(path=null): %w", err)
	}
	result, err := unwrapWorkspace(raw)
	if err != nil {
		return err
	}
	var rootResult struct {
		Type           string  `json:"type"`
		Path           string  `json:"path"`
		Parent         *string `json:"parent"`
		Home           string  `json:"home"`
		FilesystemRoot string  `json:"filesystem_root"`
		Entries        []struct {
			Name         string `json:"name"`
			RelativePath string `json:"relativePath"`
			AbsolutePath string `json:"absolutePath"`
			IsDir        bool   `json:"isDir"`
		} `json:"entries"`
	}
	if err := json.Unmarshal(result, &rootResult); err != nil {
		return err
	}
	if rootResult.Type != "directory" || rootResult.Path != root || rootResult.Home != root || rootResult.FilesystemRoot != root || rootResult.Parent != nil {
		return fmt.Errorf("null path did not resolve to isolated workspace root: %s", result)
	}
	seen := map[string]bool{}
	for _, entry := range rootResult.Entries {
		seen[entry.Name] = true
		if !entry.IsDir || entry.Name == "plain.txt" || entry.Name == ".hidden" {
			return fmt.Errorf("default directory listing exposed non-directory/hidden entry: %s", entry.Name)
		}
	}
	if !seen["inside dir"] {
		return fmt.Errorf("directory with spaces missing from browse result: %s", result)
	}
	h.checks["browse-null-isolated-home-hidden-and-directory-only"] = true
	if err := h.browseTilde(root, inside); err != nil {
		return err
	}
	absolute, err := h.rpc(map[string]any{"type": "browseDirectories", "path": inside})
	if err != nil {
		return fmt.Errorf("absolute internal path browse: %w", err)
	}
	absoluteResult, err := unwrapWorkspace(absolute)
	if err != nil {
		return err
	}
	var absoluteDirectory struct {
		Path string `json:"path"`
	}
	if err := json.Unmarshal(absoluteResult, &absoluteDirectory); err != nil || absoluteDirectory.Path != inside {
		return fmt.Errorf("absolute internal path changed: %s (%v)", absoluteResult, err)
	}
	h.checks["browse-absolute-internal-path"] = true
	parentRaw, err := h.rpc(map[string]any{"type": "browseDirectories", "path": "..", "base": inside})
	if err != nil {
		return fmt.Errorf("parent navigation within isolated root: %w", err)
	}
	parentResult, err := unwrapWorkspace(parentRaw)
	if err != nil {
		return err
	}
	var parentDirectory struct {
		Path string `json:"path"`
	}
	if err := json.Unmarshal(parentResult, &parentDirectory); err != nil || parentDirectory.Path != root {
		return fmt.Errorf("parent navigation did not return isolated root: %s (%v)", parentResult, err)
	}
	h.checks["browse-parent-navigation-inside-root"] = true
	if err := h.checkBrowseSymlinks(root); err != nil {
		return err
	}
	includeRaw, err := h.rpc(map[string]any{"type": "browseDirectories", "path": nil, "includeHidden": true})
	if err != nil {
		return err
	}
	includeResult, err := unwrapWorkspace(includeRaw)
	if err != nil {
		return err
	}
	var include struct {
		Entries []struct {
			Name string `json:"name"`
		} `json:"entries"`
	}
	if err := json.Unmarshal(includeResult, &include); err != nil {
		return err
	}
	hidden := false
	for _, entry := range include.Entries {
		hidden = hidden || entry.Name == ".hidden"
	}
	if !hidden {
		return errors.New("includeHidden=true omitted dot-directory")
	}
	h.checks["browse-include-hidden"] = true
	childRaw, err := h.rpc(map[string]any{"type": "browseDirectories", "path": "inside dir", "base": root})
	if err != nil {
		return fmt.Errorf("relative navigation inside isolated root: %w", err)
	}
	childResult, err := unwrapWorkspace(childRaw)
	if err != nil {
		return err
	}
	var child struct {
		Path string `json:"path"`
	}
	if err := json.Unmarshal(childResult, &child); err != nil || child.Path != inside {
		return fmt.Errorf("relative navigation resolved incorrectly: %s (%v)", childResult, err)
	}
	h.checks["browse-relative-base"] = true
	for _, tc := range []struct {
		name string
		args map[string]any
		kind string
	}{
		{"relative-no-base", map[string]any{"type": "browseDirectories", "path": "inside dir"}, "relative_without_base"},
		{"missing", map[string]any{"type": "browseDirectories", "path": filepath.Join(root, "missing")}, "directory_not_found"},
		{"not-directory", map[string]any{"type": "browseDirectories", "path": filepath.Join(root, "plain.txt")}, "not_a_directory"},
		{"escape", map[string]any{"type": "browseDirectories", "path": "..", "base": root}, ""},
		{"outside-root-fixture", map[string]any{"type": "browseDirectories", "path": filepath.Join(h.round, "outside")}, ""},
	} {
		_, callErr := h.rpc(tc.args)
		if callErr == nil {
			return fmt.Errorf("browse failure case %s was accepted", tc.name)
		}
		if tc.kind != "" && !strings.Contains(callErr.Error(), tc.kind) {
			return fmt.Errorf("browse failure %s had wrong kind: %v", tc.name, callErr)
		}
		if tc.name == "relative-no-base" && !strings.Contains(callErr.Error(), "relative_without_base") {
			return fmt.Errorf("relative path without base had wrong error kind: %v", callErr)
		}
		h.checks["browse-failure-"+tc.name] = true
	}
	h.skipped = append(h.skipped, "directory_not_readable: Windows ACL denial was not safely synthesized")
	return nil
}

func (h *harness) browseTilde(root, inside string) error {
	for _, tc := range []struct{ path, want string }{{"~", root}, {"~/inside dir", inside}} {
		raw, err := h.rpc(map[string]any{"type": "browseDirectories", "path": tc.path})
		if err != nil {
			return fmt.Errorf("browseDirectories(%q): %w", tc.path, err)
		}
		result, err := unwrapWorkspace(raw)
		if err != nil {
			return err
		}
		var directory struct {
			Path string `json:"path"`
		}
		if err := json.Unmarshal(result, &directory); err != nil || directory.Path != tc.want {
			return fmt.Errorf("tilde browse %q resolved to %q, want %q: %v", tc.path, directory.Path, tc.want, err)
		}
	}
	h.checks["browse-tilde-root-and-child"] = true
	return nil
}

func (h *harness) checkBrowseSymlinks(root string) error {
	target := filepath.Join(root, "inside dir")
	link := filepath.Join(root, "inside-link")
	if err := os.Symlink(target, link); err != nil {
		h.skipped = append(h.skipped, "symlink browse: current Windows account cannot create a directory symlink: "+err.Error())
		return nil
	}
	raw, err := h.rpc(map[string]any{"type": "browseDirectories", "path": nil})
	if err != nil {
		return err
	}
	result, err := unwrapWorkspace(raw)
	if err != nil {
		return err
	}
	var listing struct {
		Entries []struct {
			Name string `json:"name"`
		} `json:"entries"`
	}
	if err := json.Unmarshal(result, &listing); err != nil {
		return err
	}
	found := false
	for _, entry := range listing.Entries {
		found = found || entry.Name == "inside-link"
	}
	if !found {
		return errors.New("enterable internal directory symlink was omitted")
	}
	outside := filepath.Join(h.round, "outside")
	if err := os.Mkdir(outside, 0o700); err != nil {
		return err
	}
	if err := os.Symlink(outside, filepath.Join(root, "outside-link")); err != nil {
		h.skipped = append(h.skipped, "external symlink refusal: current Windows account cannot create a directory symlink: "+err.Error())
		return nil
	}
	if _, err := h.rpc(map[string]any{"type": "browseDirectories", "path": "outside-link", "base": root}); err == nil {
		return errors.New("external root symlink navigation was accepted")
	}
	h.checks["browse-internal-symlink-enterable-external-link-refused"] = true
	return nil
}

func (h *harness) initRepo(path string) error {
	if err := os.MkdirAll(path, 0o700); err != nil {
		return err
	}
	if err := h.runGit(path, "init", "-q"); err != nil {
		return err
	}
	if err := h.runGit(path, "config", "user.email", "navigation-e2e@example.invalid"); err != nil {
		return err
	}
	return h.runGit(path, "config", "user.name", "Navigation E2E")
}

func (h *harness) commitFile(repo, relative, content, message string) error {
	file := filepath.Join(repo, filepath.FromSlash(relative))
	if err := os.MkdirAll(filepath.Dir(file), 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(file, []byte(content), 0o600); err != nil {
		return err
	}
	if err := h.runGit(repo, "add", "--", relative); err != nil {
		return err
	}
	return h.runGit(repo, "commit", "-q", "-m", message)
}

func (h *harness) nestedRepositories() error {
	root := filepath.Join(h.round, "workspace", "repo")
	outer := filepath.Join(root, "outer")
	if err := h.initRepo(outer); err != nil {
		return err
	}
	if err := h.commitFile(outer, "tracked/keep.txt", "tracked", "outer base"); err != nil {
		return err
	}
	nested := filepath.Join(outer, "tracked", "nested")
	if err := h.initRepo(nested); err != nil {
		return err
	}
	if err := h.commitFile(nested, "nested.txt", "nested", "nested repo"); err != nil {
		return err
	}
	ignored := filepath.Join(outer, "ignored", "repo")
	if err := h.initRepo(ignored); err != nil {
		return err
	}
	if err := h.commitFile(ignored, "ignored.txt", "ignored", "ignored repo"); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(outer, ".gitignore"), []byte("ignored/\n"), 0o600); err != nil {
		return err
	}
	if err := h.runGit(outer, "add", ".gitignore"); err != nil {
		return err
	}
	if err := h.runGit(outer, "commit", "-q", "-m", "ignore nested fixture"); err != nil {
		return err
	}
	if err := h.addUninitializedSubmodule(outer); err != nil {
		return err
	}
	cwd := filepath.Join(outer, "tracked")
	query := uuid.NewString()
	raw, err := h.rpc(map[string]any{"type": "discoverNestedRepositories", "cwd": cwd, "candidates": []string{"tracked/nested"}, "queryId": query})
	if err != nil {
		return fmt.Errorf("discoverNestedRepositories: %w", err)
	}
	result, err := unwrapWorkspace(raw)
	if err != nil {
		return err
	}
	var found struct {
		Type         string `json:"type"`
		Complete     bool   `json:"complete"`
		Repositories []struct {
			RelativePath string `json:"relative_path"`
			Initialized  bool   `json:"initialized"`
		} `json:"repositories"`
	}
	if err := json.Unmarshal(result, &found); err != nil {
		return err
	}
	if found.Type != "nestedRepositories" || !found.Complete {
		return fmt.Errorf("nested repository result incomplete or wrong type: %s", result)
	}
	matched := false
	for _, repo := range found.Repositories {
		if repo.RelativePath == "tracked/nested" && repo.Initialized {
			matched = true
		}
		if strings.HasPrefix(repo.RelativePath, "ignored/") {
			return fmt.Errorf("ignored nested repository was traversed: %s", repo.RelativePath)
		}
	}
	if !matched {
		return fmt.Errorf("tracked nested repository candidate missing or not initialized: %s", result)
	}
	h.checks["nested-tracked-candidate-relative-to-toplevel"] = true
	h.checks["nested-ignored-directory-skipped"] = true
	if err := h.checkUninitializedSubmodule(outer); err != nil {
		return err
	}
	return nil
}

func (h *harness) addUninitializedSubmodule(outer string) error {
	source := filepath.Join(h.round, "submodule-source")
	if err := h.initRepo(source); err != nil {
		return err
	}
	if err := h.commitFile(source, "module.txt", "local module", "module"); err != nil {
		return err
	}
	if err := h.runGit(outer, "-c", "protocol.file.allow=always", "submodule", "add", "-q", source, "vendor/module"); err != nil {
		return err
	}
	if err := h.runGit(outer, "commit", "-q", "-m", "add local submodule"); err != nil {
		return err
	}
	return h.runGit(outer, "submodule", "deinit", "-f", "vendor/module")
}

func (h *harness) checkUninitializedSubmodule(outer string) error {
	raw, err := h.rpc(map[string]any{"type": "discoverNestedRepositories", "cwd": outer, "candidates": []string{}})
	if err != nil {
		return fmt.Errorf("discover uninitialized submodule: %w", err)
	}
	result, err := unwrapWorkspace(raw)
	if err != nil {
		return err
	}
	var found struct {
		Repositories []struct {
			RelativePath string `json:"relative_path"`
			Initialized  bool   `json:"initialized"`
		} `json:"repositories"`
	}
	if err := json.Unmarshal(result, &found); err != nil {
		return err
	}
	for _, repo := range found.Repositories {
		if repo.RelativePath == "vendor/module" {
			if repo.Initialized {
				return errors.New("deinitialized submodule was reported initialized")
			}
			h.checks["nested-gitmodules-uninitialized-preserved"] = true
			return nil
		}
	}
	return fmt.Errorf(".gitmodules submodule entry omitted: %s", result)
}

func (h *harness) nestedDiscoveryBounds() error {
	budgetRepo := filepath.Join(h.round, "workspace", "budget-repo")
	if err := h.initRepo(budgetRepo); err != nil {
		return err
	}
	for i := 0; i < 4001; i++ {
		path := filepath.Join(budgetRepo, fmt.Sprintf("directory-%04d", i))
		if err := os.Mkdir(path, 0o700); err != nil {
			return err
		}
	}
	budgetRaw, err := h.rpc(map[string]any{"type": "discoverNestedRepositories", "cwd": budgetRepo, "candidates": []string{}})
	if err != nil {
		return fmt.Errorf("bounded nested discovery: %w", err)
	}
	budgetResult, err := unwrapWorkspace(budgetRaw)
	if err != nil {
		return err
	}
	var budget struct {
		Complete bool `json:"complete"`
	}
	if err := json.Unmarshal(budgetResult, &budget); err != nil {
		return err
	}
	if budget.Complete {
		return errors.New("nested discovery exceeded directory bound without marking partial result")
	}
	h.checks["nested-directory-bound-reports-partial"] = true

	depthRepo := filepath.Join(h.round, "workspace", "depth-repo")
	if err := h.initRepo(depthRepo); err != nil {
		return err
	}
	deep := depthRepo
	for i := 0; i < 34; i++ {
		deep = filepath.Join(deep, fmt.Sprintf("level-%02d", i))
	}
	if err := os.MkdirAll(deep, 0o700); err != nil {
		return err
	}
	depthRaw, err := h.rpc(map[string]any{"type": "discoverNestedRepositories", "cwd": depthRepo, "candidates": []string{}})
	if err != nil {
		return fmt.Errorf("depth-bounded nested discovery: %w", err)
	}
	depthResult, err := unwrapWorkspace(depthRaw)
	if err != nil {
		return err
	}
	var depth struct {
		Complete bool `json:"complete"`
	}
	if err := json.Unmarshal(depthResult, &depth); err != nil {
		return err
	}
	if depth.Complete {
		return errors.New("nested discovery exceeded depth bound without marking partial result")
	}
	h.checks["nested-depth-bound-reports-partial"] = true

	query := uuid.NewString()
	if _, err := h.rpc(map[string]any{"type": "cancelGitQuery", "queryId": query}); err != nil {
		return fmt.Errorf("cancel pending discovery query: %w", err)
	}
	if _, err := h.rpc(map[string]any{"type": "discoverNestedRepositories", "cwd": depthRepo, "candidates": []string{}, "queryId": query}); err == nil {
		return errors.New("discovery started for a pre-cancelled query ID")
	}
	h.checks["nested-pre-cancelled-query-refused"] = true
	h.skipped = append(h.skipped,
		"nested in-flight cancellation timing was not stress-tested; deterministic pre-cancelled query rejection passed",
		"nested 256-repository cap, 100000-entry cap, and 10-second wall-clock cap were not stress-generated",
		"nested symlink/reparse traversal was not exercised because this account cannot create directory symlinks",
	)
	return nil
}

func (h *harness) deleteSessionRefs() error {
	repo := filepath.Join(h.round, "workspace", "repo", "refs")
	if err := h.initRepo(repo); err != nil {
		return err
	}
	if err := h.commitFile(repo, "tracked.txt", "base\n", "base"); err != nil {
		return err
	}
	// Create real turn refs through the daemon's workspace RPC, using distinct
	// session IDs for the target and the retained session.
	target, other := uuid.NewString(), uuid.NewString()
	if err := h.captureTurn(repo, target, "target"); err != nil {
		return err
	}
	if err := h.captureTurn(repo, other, "other"); err != nil {
		return err
	}
	commit, err := h.gitOutput(repo, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	for _, session := range []string{target, other} {
		if err := h.runGit(repo, "update-ref", "refs/fintwind/session-"+session+"-turn-diff-1", strings.TrimSpace(commit)); err != nil {
			return fmt.Errorf("create isolated diff ref fixture: %w", err)
		}
	}
	before, err := h.gitOutput(repo, "status", "--porcelain")
	if err != nil {
		return err
	}
	if _, err := h.rpc(map[string]any{"type": "deleteSessionRefs", "cwd": repo, "sessionId": target}); err != nil {
		return fmt.Errorf("deleteSessionRefs RPC: %w", err)
	}
	refs, err := h.gitOutput(repo, "for-each-ref", "--format=%(refname)", "refs/fintwind/session-"+target+"-*")
	if err != nil {
		return err
	}
	if strings.TrimSpace(refs) != "" {
		return fmt.Errorf("deleteSessionRefs left target refs: %s", refs)
	}
	otherRefs, err := h.gitOutput(repo, "for-each-ref", "--format=%(refname)", "refs/fintwind/session-"+other+"-*")
	if err != nil {
		return err
	}
	if strings.TrimSpace(otherRefs) == "" {
		return errors.New("deleteSessionRefs removed another session's refs")
	}
	for _, suffix := range []string{"turn-start-1", "turn-1", "turn-diff-1"} {
		if !strings.Contains(otherRefs, "session-"+other+"-"+suffix) {
			return fmt.Errorf("other session is missing expected %s ref: %s", suffix, otherRefs)
		}
	}
	status, err := h.gitOutput(repo, "status", "--porcelain")
	if err != nil {
		return err
	}
	if status != before {
		return fmt.Errorf("deleteSessionRefs changed worktree or index: before=%q after=%q", before, status)
	}
	h.checks["delete-session-all-refs-isolated-worktree-index-unchanged"] = true
	return nil
}

func (h *harness) rpcWorkspace(operation map[string]any) error {
	_, err := h.rpc(operation)
	return err
}

func (h *harness) captureTurn(repo, session, label string) error {
	if _, err := h.rpc(map[string]any{"type": "captureTurnStart", "cwd": repo, "sessionId": session, "turnCount": 1}); err != nil {
		return fmt.Errorf("captureTurnStart %s: %w", label, err)
	}
	file := filepath.Join(repo, label+"-response.txt")
	if err := os.WriteFile(file, []byte(label+" response\n"), 0o600); err != nil {
		return err
	}
	if _, err := h.rpc(map[string]any{"type": "captureTurn", "cwd": repo, "sessionId": session, "turnCount": 1}); err != nil {
		return fmt.Errorf("captureTurn %s: %w", label, err)
	}
	return nil
}

func (h *harness) gitOutput(directory string, args ...string) (string, error) {
	logRoot := filepath.Join(h.round, "git-logs")
	if err := os.MkdirAll(logRoot, 0o700); err != nil {
		return "", err
	}
	logDir, err := os.MkdirTemp(logRoot, "git-")
	if err != nil {
		return "", err
	}
	child, err := ownedprocess.StartWithEnvironment(h.git, args, logDir, directory, ownedprocess.BuildEnv(h.round, nil))
	if err != nil {
		return "", err
	}
	done, waitErr := child.Exited(10000)
	if waitErr != nil || !done {
		_ = child.Close()
		return "", fmt.Errorf("owned git query did not finish: %v", waitErr)
	}
	code, codeErr := child.ExitCode()
	stdout, _ := os.ReadFile(child.StdoutPath)
	stderr, _ := os.ReadFile(child.StderrPath)
	closeErr := child.Close()
	if codeErr != nil || code != 0 || closeErr != nil {
		return "", fmt.Errorf("git %s failed: %s", strings.Join(args, " "), strings.TrimSpace(string(stderr)))
	}
	return string(stdout), nil
}

func sha256File(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	h := sha256.New()
	if _, err := io.Copy(h, file); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

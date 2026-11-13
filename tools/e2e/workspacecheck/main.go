// Command workspacecheck exercises the local backendworkspace module
// end to end through its real Dispatch entry point.
//
// Failure scenarios run first: every escape, gap, and bound must fail
// explicitly instead of returning an empty success. Behavior checks
// follow, then pressure checks. All fixture data lives under one
// ownedprocess.NewRoundDir directory; no real project, no app, no
// production daemon, and no backup file is ever touched. Git fixtures
// use only local invocations through owned Job handles with an explicit
// environment and a timeout, same as the module under test.
//
// This verifies the local module only. A passing report does not claim
// daemon integration works.
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
)

type scenario struct {
	name string
	kind string // failure, behavior, or pressure
	run  func(ctx context.Context, env *fixtureEnv) error
}

type scenarioResult struct {
	Name   string `json:"name"`
	Kind   string `json:"kind"`
	Result string `json:"result"`
	Detail string `json:"detail,omitempty"`
}

type report struct {
	ModuleOnly   bool             `json:"moduleOnly"`
	Note         string           `json:"note"`
	GitAvailable bool             `json:"gitAvailable"`
	Scenarios    []scenarioResult `json:"scenarios"`
	Passed       bool             `json:"passed"`
	Error        string           `json:"error,omitempty"`
}

type fixtureEnv struct {
	round string
	ws    string
	git   string // empty when git is unavailable
}

func main() {
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	if err := run(ctx); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context) (err error) {
	scenarios := listScenarios()
	fmt.Println("[SCENARIOS] failure scenarios run first:")
	for _, scenario := range scenarios {
		fmt.Printf("  [%s] %s\n", scenario.kind, scenario.name)
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	env := &fixtureEnv{round: round, ws: filepath.Join(round, "ws")}
	if err := os.MkdirAll(env.ws, 0o755); err != nil {
		return err
	}
	if path, lookErr := exec.LookPath("git"); lookErr == nil {
		if absolute, absErr := filepath.Abs(path); absErr == nil {
			env.git = absolute
		}
	}
	rep := report{
		ModuleOnly:   true,
		Note:         "local backendworkspace module only; not a daemon integration result",
		GitAvailable: env.git != "",
	}
	defer func() {
		rep.Passed = err == nil
		if err != nil {
			rep.Error = err.Error()
		}
		data, marshalErr := json.MarshalIndent(rep, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(round, "report.json"), data, 0o600)
		}
		if marshalErr != nil {
			err = errors.Join(err, errors.New("could not save verification artifact"))
		}
		fmt.Println("[REPORT]", filepath.Join(round, "report.json"))
	}()
	for _, scenario := range scenarios {
		sctx, cancel := context.WithTimeout(ctx, 45*time.Second)
		runErr := scenario.run(sctx, env)
		cancel()
		result := scenarioResult{Name: scenario.name, Kind: scenario.kind, Result: "pass"}
		if runErr != nil {
			var skip errSkip
			if errors.As(runErr, &skip) {
				result.Result = "skip"
				result.Detail = runErr.Error()
			} else {
				result.Result = "fail"
				result.Detail = runErr.Error()
				rep.Scenarios = append(rep.Scenarios, result)
				return fmt.Errorf("scenario %q failed: %w", scenario.name, runErr)
			}
		}
		rep.Scenarios = append(rep.Scenarios, result)
		fmt.Printf("[%s] %s\n", map[string]string{"pass": "OK", "skip": "SKIP"}[result.Result], scenario.name)
	}
	return nil
}

type errSkip struct{ reason string }

func (e errSkip) Error() string { return "skipped: " + e.reason }

func listScenarios() []scenario {
	return []scenario{
		{name: "read-escape-relative-is-rejected", kind: "failure", run: checkReadEscapeRelative},
		{name: "read-escape-absolute-is-rejected", kind: "failure", run: checkReadEscapeAbsolute},
		{name: "read-unc-is-rejected", kind: "failure", run: checkReadUNC},
		{name: "write-escape-leaves-nothing-outside", kind: "failure", run: checkWriteEscape},
		{name: "symlink-escape-is-rejected", kind: "failure", run: checkSymlinkEscape},
		{name: "read-missing-file-errors", kind: "failure", run: checkReadMissing},
		{name: "unknown-operation-type-errors", kind: "failure", run: checkUnknownOp},
		{name: "fetch-on-non-repository-refused", kind: "failure", run: checkNetworkOpRefused},
		{name: "push-on-non-repository-refused", kind: "failure", run: checkPushRefused},
		{name: "discover-with-binary-errors", kind: "failure", run: checkDiscoverBinaryRefused},
		{name: "history-limit-over-bound-errors", kind: "failure", run: checkLimitBound},
		{name: "write-over-byte-cap-errors", kind: "failure", run: checkWriteCap},
		{name: "read-over-byte-cap-errors", kind: "failure", run: checkReadCap},
		{name: "stats-invalid-hash-errors", kind: "failure", run: checkStatsHash},
		{name: "write-then-read-roundtrip", kind: "behavior", run: checkRoundtrip},
		{name: "browse-lists-dirs-first", kind: "behavior", run: checkBrowse},
		{name: "browse-missing-dir-names-kind", kind: "behavior", run: checkBrowseMissing},
		{name: "list-project-files-shape", kind: "behavior", run: checkListFiles},
		{name: "discover-commands-shape", kind: "behavior", run: checkDiscover},
		{name: "non-repo-git-reads-return-null", kind: "behavior", run: checkNonRepo},
		{name: "repo-git-local-reads", kind: "behavior", run: checkRepoReads},
		{name: "many-files-stay-bounded", kind: "pressure", run: checkManyFiles},
		{name: "cancelled-context-aborts", kind: "pressure", run: checkCancelled},
	}
}

func dispatch(ctx context.Context, root string, payload map[string]any) (json.RawMessage, error) {
	raw, err := json.Marshal(payload)
	if err != nil {
		return nil, err
	}
	return backendworkspace.Dispatch(ctx, root, raw)
}

func expectError(err error, fragments ...string) error {
	if err == nil {
		return errors.New("expected an explicit error, got success")
	}
	lower := strings.ToLower(err.Error())
	for _, fragment := range fragments {
		if !strings.Contains(lower, strings.ToLower(fragment)) {
			return fmt.Errorf("error %q misses %q", err, fragment)
		}
	}
	return nil
}

func checkReadEscapeRelative(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "readTextFile", "root": env.ws, "relativePath": "../outside.txt",
	})
	return expectError(err, "relative")
}

func checkReadEscapeAbsolute(ctx context.Context, env *fixtureEnv) error {
	outside := filepath.Join(env.round, "outside.txt")
	if err := os.WriteFile(outside, []byte("secret"), 0o600); err != nil {
		return err
	}
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "readTextFile", "root": env.ws, "relativePath": outside,
	})
	// Absolute paths are rejected as relative paths; either wording proves
	// the escape did not happen.
	return expectError(err, "relative")
}

func checkReadUNC(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "readTextFile", "root": env.ws, "relativePath": `\\server\share\secret.txt`,
	})
	return expectError(err)
}

func checkWriteEscape(ctx context.Context, env *fixtureEnv) error {
	target := filepath.Join(env.round, "escape-planted.txt")
	_ = os.Remove(target)
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "writeTextFile", "root": env.ws, "relativePath": "../escape-planted.txt", "content": "x",
	})
	if check := expectError(err, "relative"); check != nil {
		return check
	}
	if _, statErr := os.Stat(target); !os.IsNotExist(statErr) {
		return errors.New("escape write planted a file outside root")
	}
	return nil
}

func checkSymlinkEscape(ctx context.Context, env *fixtureEnv) error {
	outside := filepath.Join(env.round, "outside.txt")
	if err := os.WriteFile(outside, []byte("secret"), 0o600); err != nil {
		return err
	}
	link := filepath.Join(env.ws, "link.txt")
	_ = os.Remove(link)
	if err := os.Symlink(outside, link); err != nil {
		return errSkip{reason: "symlinks need elevated rights on this machine"}
	}
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "readTextFile", "root": env.ws, "relativePath": "link.txt",
	})
	return expectError(err)
}

func checkReadMissing(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "readTextFile", "root": env.ws, "relativePath": "no-such-file.txt",
	})
	return expectError(err, "open file")
}

func checkUnknownOp(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{"type": "inventedOperation"})
	return expectError(err, "unknown operation")
}

func checkNetworkOpRefused(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{"type": "fetchRemote", "cwd": env.ws})
	return expectError(err, "not a Git repository")
}

func checkPushRefused(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{"type": "push", "cwd": env.ws})
	return expectError(err, "not a Git repository")
}

func checkDiscoverBinaryRefused(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "discoverSlashCommands", "projectRoot": env.ws, "binary": "opencode",
	})
	return expectError(err, "server registry")
}

func checkLimitBound(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "listCommits", "cwd": env.ws, "limit": 100000,
	})
	return expectError(err, "bound")
}

func checkWriteCap(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "writeTextFile", "root": env.ws, "relativePath": "big.txt",
		"content": strings.Repeat("a", backendworkspace.MaxTextFileBytes+1),
	})
	return expectError(err, "bound")
}

func checkReadCap(ctx context.Context, env *fixtureEnv) error {
	path := filepath.Join(env.ws, "big.bin")
	if err := os.WriteFile(path, make([]byte, backendworkspace.MaxTextFileBytes+1), 0o600); err != nil {
		return err
	}
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "readTextFile", "root": env.ws, "relativePath": "big.bin",
	})
	return expectError(err, "bound")
}

func checkStatsHash(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "readCommitStats", "cwd": env.ws, "hash": "not-a-hash",
	})
	return expectError(err, "hash")
}

func checkRoundtrip(ctx context.Context, env *fixtureEnv) error {
	want := "hello workspace"
	if _, err := dispatch(ctx, env.ws, map[string]any{
		"type": "writeTextFile", "root": env.ws, "relativePath": "note.txt", "content": want,
	}); err != nil {
		return err
	}
	raw, err := dispatch(ctx, env.ws, map[string]any{
		"type": "readTextFile", "root": env.ws, "relativePath": "note.txt",
	})
	if err != nil {
		return err
	}
	var got struct {
		Type    string `json:"type"`
		Content string `json:"content"`
	}
	if err := json.Unmarshal(raw, &got); err != nil {
		return err
	}
	if got.Type != "textFile" || got.Content != want {
		return fmt.Errorf("roundtrip mismatch: %+v", got)
	}
	return nil
}

func checkBrowse(ctx context.Context, env *fixtureEnv) error {
	dir := filepath.Join(env.ws, "proj")
	if err := os.MkdirAll(filepath.Join(dir, "sub"), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(dir, "a.txt"), []byte("a"), 0o600); err != nil {
		return err
	}
	raw, err := dispatch(ctx, env.ws, map[string]any{"type": "browseDirectory", "path": dir})
	if err != nil {
		return err
	}
	var got struct {
		Type    string `json:"type"`
		Path    string `json:"path"`
		Home    string `json:"home"`
		Entries []struct {
			Name  string `json:"name"`
			IsDir bool   `json:"isDir"`
		} `json:"entries"`
	}
	if err := json.Unmarshal(raw, &got); err != nil {
		return err
	}
	if got.Type != "directory" || len(got.Entries) != 2 {
		return fmt.Errorf("unexpected browse result: %+v", got)
	}
	if !got.Entries[0].IsDir || got.Entries[0].Name != "sub" {
		return fmt.Errorf("directories must sort first: %+v", got.Entries)
	}
	if got.Entries[1].Name != "a.txt" || got.Home == "" {
		return fmt.Errorf("unexpected browse entries: %+v", got)
	}
	return nil
}

func checkBrowseMissing(ctx context.Context, env *fixtureEnv) error {
	_, err := dispatch(ctx, env.ws, map[string]any{
		"type": "browseDirectory", "path": filepath.Join(env.ws, "no-such-dir"),
	})
	return expectError(err, "directory_not_found")
}

func checkListFiles(ctx context.Context, env *fixtureEnv) error {
	base := filepath.Join(env.ws, "files")
	if err := os.MkdirAll(filepath.Join(base, "src"), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(base, "src", "main.go"), []byte("package main"), 0o600); err != nil {
		return err
	}
	raw, err := dispatch(ctx, env.ws, map[string]any{
		"type": "listProjectFiles", "root": base, "cap": 1000,
	})
	if err != nil {
		return err
	}
	var got struct {
		Type    string `json:"type"`
		Entries []struct {
			Path  string `json:"path"`
			IsDir bool   `json:"isDir"`
		} `json:"entries"`
	}
	if err := json.Unmarshal(raw, &got); err != nil {
		return err
	}
	if got.Type != "projectFiles" {
		return fmt.Errorf("wrong result type: %s", got.Type)
	}
	want := map[string]bool{"src/": true, "src/main.go": true}
	for _, entry := range got.Entries {
		delete(want, entry.Path)
		if entry.IsDir != strings.HasSuffix(entry.Path, "/") {
			return fmt.Errorf("directory flag mismatches trailing slash: %+v", entry)
		}
	}
	if len(want) != 0 {
		return fmt.Errorf("missing entries: %v", want)
	}
	return nil
}

func checkDiscover(ctx context.Context, env *fixtureEnv) error {
	base := filepath.Join(env.ws, "cmds")
	cmdDir := filepath.Join(base, ".opencode", "commands")
	if err := os.MkdirAll(cmdDir, 0o755); err != nil {
		return err
	}
	body := "---\ndescription: Fixture command\n---\n\nDo the fixture thing.\n"
	if err := os.WriteFile(filepath.Join(cmdDir, "fix.md"), []byte(body), 0o600); err != nil {
		return err
	}
	raw, err := dispatch(ctx, env.ws, map[string]any{
		"type": "discoverSlashCommands", "projectRoot": base,
	})
	if err != nil {
		return err
	}
	var got struct {
		Type     string `json:"type"`
		Commands []struct {
			Name        string  `json:"name"`
			Description string  `json:"description"`
			Scope       string  `json:"scope"`
			Template    *string `json:"template"`
		} `json:"commands"`
	}
	if err := json.Unmarshal(raw, &got); err != nil {
		return err
	}
	seen := map[string]string{}
	for _, command := range got.Commands {
		seen[command.Name] = command.Scope
		if command.Name == "fix" {
			if command.Scope != "Project" || command.Description != "Fixture command" || command.Template == nil {
				return fmt.Errorf("fixture command misread: %+v", command)
			}
		}
	}
	if seen["fix"] == "" || seen["init"] != "Builtin" {
		return fmt.Errorf("discovery missed fixture or builtin commands: %v", seen)
	}
	return nil
}

func checkNonRepo(ctx context.Context, env *fixtureEnv) error {
	plain := filepath.Join(env.ws, "plain")
	if err := os.MkdirAll(plain, 0o755); err != nil {
		return err
	}
	raw, err := dispatch(ctx, env.ws, map[string]any{"type": "currentBranch", "cwd": plain})
	if err != nil {
		return err
	}
	var branch struct {
		Type   string  `json:"type"`
		Branch *string `json:"branch"`
	}
	if err := json.Unmarshal(raw, &branch); err != nil {
		return err
	}
	if branch.Type != "currentBranch" || branch.Branch != nil {
		return fmt.Errorf("non-repo branch must be null: %+v", branch)
	}
	raw, err = dispatch(ctx, env.ws, map[string]any{"type": "listCommits", "cwd": plain, "limit": 10})
	if err != nil {
		return err
	}
	var commits struct {
		Type    string            `json:"type"`
		Commits []json.RawMessage `json:"commits"`
	}
	if err := json.Unmarshal(raw, &commits); err != nil {
		return err
	}
	if commits.Type != "commits" || commits.Commits != nil {
		return fmt.Errorf("non-repo commits must be null: %+v", commits)
	}
	return nil
}

func checkRepoReads(ctx context.Context, env *fixtureEnv) error {
	if env.git == "" {
		return errSkip{reason: "git is not on PATH"}
	}
	repo := filepath.Join(env.ws, "repo")
	if err := os.MkdirAll(repo, 0o755); err != nil {
		return err
	}
	run := func(args ...string) error {
		_, err := runGitFixture(ctx, env.round, repo, args...)
		return err
	}
	if err := run("init", "-b", "main"); err != nil {
		if err := run("init"); err != nil {
			return err
		}
	}
	if err := run("-c", "user.name=t", "-c", "user.email=t@t", "config", "user.name", "t"); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "file.txt"), []byte("one\n"), 0o600); err != nil {
		return err
	}
	if err := run("add", "file.txt"); err != nil {
		return err
	}
	if err := run("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "first"); err != nil {
		return err
	}
	headOut, err := runGitFixture(ctx, env.round, repo, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	head := strings.TrimSpace(string(headOut))

	raw, err := dispatch(ctx, env.ws, map[string]any{"type": "currentBranch", "cwd": repo})
	if err != nil {
		return err
	}
	var branch struct {
		Type   string  `json:"type"`
		Branch *string `json:"branch"`
	}
	if err := json.Unmarshal(raw, &branch); err != nil {
		return err
	}
	if branch.Branch == nil || *branch.Branch == "" {
		return fmt.Errorf("repo branch must be set: %+v", branch)
	}
	raw, err = dispatch(ctx, env.ws, map[string]any{"type": "listCommits", "cwd": repo, "limit": 10})
	if err != nil {
		return err
	}
	var commits struct {
		Type    string `json:"type"`
		Commits []struct {
			Hash    string `json:"hash"`
			Subject string `json:"subject"`
			Pushed  bool   `json:"pushed"`
		} `json:"commits"`
	}
	if err := json.Unmarshal(raw, &commits); err != nil {
		return err
	}
	if len(commits.Commits) != 1 || commits.Commits[0].Hash != head {
		return fmt.Errorf("history must hold the one fixture commit: %+v", commits)
	}
	if commits.Commits[0].Pushed {
		return fmt.Errorf("a commit with no remotes must not read as pushed")
	}
	raw, err = dispatch(ctx, env.ws, map[string]any{"type": "readCommitStats", "cwd": repo, "hash": head})
	if err != nil {
		return err
	}
	var stats struct {
		Type  string `json:"type"`
		Stats struct {
			Files     uint64 `json:"files"`
			Additions uint64 `json:"additions"`
		} `json:"stats"`
	}
	if err := json.Unmarshal(raw, &stats); err != nil {
		return err
	}
	if stats.Stats.Files != 1 || stats.Stats.Additions != 1 {
		return fmt.Errorf("stats must count the fixture file: %+v", stats)
	}
	raw, err = dispatch(ctx, env.ws, map[string]any{"type": "inspectBranchRefs", "cwd": repo})
	if err != nil {
		return err
	}
	var refs struct {
		Type     string `json:"type"`
		Snapshot *struct {
			Current  *string `json:"current"`
			Branches []struct {
				Name string `json:"name"`
			} `json:"branches"`
		} `json:"snapshot"`
	}
	if err := json.Unmarshal(raw, &refs); err != nil {
		return err
	}
	if refs.Snapshot == nil || refs.Snapshot.Current == nil || len(refs.Snapshot.Branches) == 0 {
		return fmt.Errorf("branch refs must name the checkout: %+v", refs)
	}
	return nil
}

func checkManyFiles(ctx context.Context, env *fixtureEnv) error {
	base := filepath.Join(env.ws, "many")
	if err := os.MkdirAll(base, 0o755); err != nil {
		return err
	}
	for i := 0; i < 3000; i++ {
		if i%500 == 0 {
			if err := ctx.Err(); err != nil {
				return err
			}
		}
		name := fmt.Sprintf("file-%04d.txt", i)
		if err := os.WriteFile(filepath.Join(base, name), []byte("data"), 0o600); err != nil {
			return err
		}
	}
	raw, err := dispatch(ctx, env.ws, map[string]any{
		"type": "listProjectFiles", "root": base, "cap": 5000,
	})
	if err != nil {
		return err
	}
	var got struct {
		Entries []json.RawMessage `json:"entries"`
	}
	if err := json.Unmarshal(raw, &got); err != nil {
		return err
	}
	if len(got.Entries) == 0 || len(got.Entries) > 5000 {
		return fmt.Errorf("bounded listing returned %d entries", len(got.Entries))
	}
	return nil
}

func checkCancelled(ctx context.Context, env *fixtureEnv) error {
	cancelled, cancel := context.WithCancel(context.Background())
	cancel()
	_, err := dispatch(cancelled, env.ws, map[string]any{
		"type": "listProjectFiles", "root": env.ws, "cap": 100,
	})
	_ = ctx
	if err == nil {
		return errors.New("cancelled context must abort the walk")
	}
	return nil
}

// runGitFixture runs one local-only fixture Git command for test setup.
// Same discipline as the module: owned Job handle, explicit environment,
// timeout, no network verbs.
func runGitFixture(ctx context.Context, round, cwd string, args ...string) ([]byte, error) {
	if len(args) == 0 {
		return nil, errors.New("git needs arguments")
	}
	verb := args
	for len(verb) >= 2 && verb[0] == "-c" {
		verb = verb[2:]
	}
	if len(verb) == 0 {
		return nil, errors.New("git needs a subcommand")
	}
	switch verb[0] {
	case "init", "config", "add", "commit", "rev-parse":
	default:
		return nil, fmt.Errorf("fixture refuses git %s", verb[0])
	}
	path, err := exec.LookPath("git")
	if err != nil {
		return nil, err
	}
	absolute, err := filepath.Abs(path)
	if err != nil {
		return nil, err
	}
	logsBase := filepath.Join(round, "fixture-logs")
	if err := os.MkdirAll(logsBase, 0o755); err != nil {
		return nil, err
	}
	logs, err := os.MkdirTemp(logsBase, "call-*")
	if err != nil {
		return nil, err
	}
	child, err := ownedprocess.StartWithEnvironment(absolute, args, logs, cwd, ownedprocess.BuildEnv(round, map[string]string{
		"GIT_TERMINAL_PROMPT": "0",
	}))
	if err != nil {
		return nil, err
	}
	defer child.Close()
	deadline := time.Now().Add(30 * time.Second)
	for {
		done, err := child.Exited(50)
		if err != nil {
			return nil, err
		}
		if done {
			break
		}
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		if time.Now().After(deadline) {
			return nil, errors.New("fixture git timed out")
		}
	}
	code, err := child.ExitCode()
	if err != nil {
		return nil, err
	}
	out, _ := os.ReadFile(child.StdoutPath)
	if code != 0 {
		detail, _ := os.ReadFile(child.StderrPath)
		return nil, fmt.Errorf("fixture git %v failed: %s", args, strings.TrimSpace(string(detail)))
	}
	return out, nil
}

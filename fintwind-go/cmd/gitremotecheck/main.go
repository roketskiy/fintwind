// Command gitremotecheck exercises Git remote behavior through the real Go
// daemon WebSocket, using only repositories created in an owned round.
// Network transports are never contacted by this command.
package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

type result struct {
	Name   string `json:"name"`
	Status string `json:"status"`
	Detail string `json:"detail,omitempty"`
}

type report struct {
	Passed           bool              `json:"passed"`
	DaemonBinaryHash string            `json:"daemonBinaryHash"`
	Results          []result          `json:"results"`
	Coverage         map[string]string `json:"coverage"`
	Network          string            `json:"network"`
	Failure          string            `json:"failure,omitempty"`
}

const e2eKnownHosts = "fixture.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDu5h7zuvr+WB6JAd0g9vPbC8oW1sQFKycYvLrLJwM0t fintwind-e2e\n[127.0.0.1]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDu5h7zuvr+WB6JAd0g9vPbC8oW1sQFKycYvLrLJwM0t fintwind-e2e\n"

func main() {
	if strings.EqualFold(filepath.Base(os.Args[0]), "git.exe") {
		fakeGitMain()
		return
	}
	daemonPath := flag.String("daemon", "", "absolute Go daemon executable")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 4*time.Minute)
	defer cancel()
	if err := run(ctx, *daemonPath); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, daemonPath string) (err error) {
	if !filepath.IsAbs(daemonPath) {
		return errors.New("absolute daemon executable required")
	}
	for _, k := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "FINTWIND_AGENT_REAL_PROVIDER"} {
		if strings.TrimSpace(os.Getenv(k)) != "" {
			return fmt.Errorf("credential/marker %s must be unset", k)
		}
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	r := report{Coverage: map[string]string{
		"daemonWebSocket": "pending", "offlineBarePushFetchPull": "pending",
		"noUpstreamPush": "pending", "maliciousConfigMarkers": "pending",
		"safeHttpsSshArgv": "pending", "pullLockRace": "pending",
		"fileURLBoundary": "pending", "invalidMappings": "pending",
	}, Network: "No external Git/provider/credential/user-repository access. Bare transfers are local; HTTP/SSH destinations are intercepted by the owned fake Git wrapper and never contacted."}
	if sum, e := fileSHA(daemonPath); e == nil {
		r.DaemonBinaryHash = sum
	} else {
		return e
	}
	defer func() {
		r.Passed = err == nil
		if err != nil {
			r.Failure = err.Error()
		}
		b, _ := json.MarshalIndent(r, "", "  ")
		p := filepath.Join(round, "gitremote-result.json")
		if e := os.WriteFile(p, b, 0o600); e != nil {
			err = errors.Join(err, e)
			return
		}
		sum := sha256.Sum256(b)
		fmt.Printf("[REPORT] %s\n[SHA256] %s\n", p, hex.EncodeToString(sum[:]))
	}()

	git, err := execPath("git")
	if err != nil {
		return err
	}
	repo := filepath.Join(round, "workspace", "main", "repo")
	bare := filepath.Join(round, "workspace", "main", "remote.git")
	if err = os.MkdirAll(repo, 0o700); err != nil {
		return err
	}
	if err = os.MkdirAll(filepath.Join(round, "data"), 0o700); err != nil {
		return err
	}
	knownHostsPath := filepath.Join(round, "home", ".ssh", "known_hosts")
	if err = os.MkdirAll(filepath.Dir(knownHostsPath), 0o700); err != nil {
		return err
	}
	if err = os.WriteFile(knownHostsPath, []byte(e2eKnownHosts), 0o600); err != nil {
		return err
	}
	gitEnv := ownedprocess.BuildEnv(round, map[string]string{"GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0", "GIT_ATTR_NOSYSTEM": "1"})
	if err = gitRun(ctx, git, round, repo, gitEnv, "init", "-b", "main"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, gitEnv, "config", "user.name", "E2E Fixture"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, gitEnv, "config", "user.email", "e2e.invalid@example.invalid"); err != nil {
		return err
	}
	if err = os.WriteFile(filepath.Join(repo, "fixture.txt"), []byte("offline fixture\n"), 0o600); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, gitEnv, "add", "fixture.txt"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, gitEnv, "commit", "-m", "fixture"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, filepath.Dir(bare), gitEnv, "init", "--bare", bare); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, gitEnv, "remote", "add", "origin", fileURL(bare)); err != nil {
		return err
	}

	fakeGit, err := prepareFakeGit(round, git)
	if err != nil {
		return err
	}
	inst, err := launch(ctx, round, daemonPath, fakeGit)
	if err != nil {
		return err
	}
	defer inst.child.Close()
	r.Coverage["daemonWebSocket"] = "real owned daemon process and authenticated protocol WebSocket"
	conn, err := dial(inst.addr, inst.token)
	if err != nil {
		return err
	}
	defer conn.Close()
	callAt := func(op, cwd string) (string, error) {
		id := uuid.NewString()
		cmd := map[string]any{"type": "workspace", "operation": map[string]any{"type": op, "cwd": cwd}}
		if e := conn.WriteJSON(map[string]any{"type": "request", "requestId": id, "sessionId": uuid.NewString(), "runtimeId": uuid.NewString(), "command": cmd}); e != nil {
			return "", e
		}
		_ = conn.SetReadDeadline(time.Now().Add(80 * time.Second))
		for {
			var frame map[string]any
			if e := conn.ReadJSON(&frame); e != nil {
				return "", e
			}
			if frame["type"] != "response" || frame["requestId"] != id {
				continue
			}
			out, _ := frame["outcome"].(map[string]any)
			status, _ := out["status"].(string)
			if status != "ok" {
				er, _ := out["error"].(map[string]any)
				msg, _ := er["message"].(string)
				return "", errors.New(msg)
			}
			return status, nil
		}
	}
	call := func(op string) (string, error) { return callAt(op, repo) }
	if _, err = call("push"); err != nil {
		r.Results = append(r.Results, result{"no-upstream-explicit-push", "fail", err.Error()})
		return fmt.Errorf("no-upstream push: %w", err)
	}
	r.Results = append(r.Results, result{"no-upstream-explicit-push", "pass", ""})
	r.Coverage["noUpstreamPush"] = "pushed to explicit origin without existing upstream"
	if err = gitRun(ctx, git, round, repo, gitEnv, "remote", "set-url", "origin", fileURLLocalhost(bare)); err != nil {
		return err
	}
	if _, err = call("fetchRemote"); err != nil {
		return fmt.Errorf("localhost file URL fetch: %w", err)
	}
	r.Results = append(r.Results, result{"localhost-file-url", "pass", "localhost-host file URL accepted and confined to workspace root"})
	if err = checkFetchRefIsolation(ctx, git, round, gitEnv, repo, bare, callAt, &r); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, gitEnv, "--git-dir", bare, "symbolic-ref", "HEAD", "refs/heads/main"); err != nil {
		return fmt.Errorf("set isolated bare fixture HEAD: %w", err)
	}
	if err = gitRun(ctx, git, round, repo, gitEnv, "clone", bare, filepath.Join(round, "workspace", "main", "clone")); err != nil {
		return err
	}
	clone := filepath.Join(round, "workspace", "main", "clone")
	if err = os.WriteFile(filepath.Join(repo, "second.txt"), []byte("second offline commit\n"), 0o600); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, gitEnv, "add", "second.txt"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, gitEnv, "commit", "-m", "second"); err != nil {
		return err
	}
	if _, err = call("push"); err != nil {
		return fmt.Errorf("second push: %w", err)
	}
	if _, err = call("fetchRemote"); err != nil {
		return fmt.Errorf("source ref refresh: %w", err)
	}
	if err = assertTrackingAlias(ctx, git, round, gitEnv, repo); err != nil {
		return err
	}
	sourceOID, err := gitCapture(ctx, git, round, repo, gitEnv, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	trackingOID, err := gitCapture(ctx, git, round, repo, gitEnv, "rev-parse", "--verify", "refs/remotes/origin/main")
	if err != nil {
		return err
	}
	if strings.TrimSpace(string(sourceOID)) != strings.TrimSpace(string(trackingOID)) {
		return errors.New("fetch did not advance main tracking ref to pushed commit")
	}
	if err = gitRun(ctx, git, round, clone, gitEnv, "fetch", "origin"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, clone, gitEnv, "config", "user.name", "E2E Clone"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, clone, gitEnv, "config", "user.email", "clone-e2e.invalid@example.invalid"); err != nil {
		return err
	}
	if _, err = callAt("fetchRemote", clone); err != nil {
		return fmt.Errorf("daemon fetch: %w", err)
	}
	if _, err = callAt("pullFastForward", clone); err != nil {
		return fmt.Errorf("daemon fast-forward pull: %w", err)
	}
	if b, e := os.ReadFile(filepath.Join(clone, "second.txt")); e != nil || string(b) != "second offline commit\n" {
		return errors.New("offline roundtrip content mismatch")
	}
	r.Results = append(r.Results, result{"offline-bare-push-fetch-fast-forward", "pass", "real local bare Git object transfer; clone fast-forward verified"})
	r.Results = append(r.Results, result{"daemon-fetch-and-ff-pull", "pass", "both operations issued over the daemon WebSocket"})
	r.Coverage["offlineBarePushFetchPull"] = "daemon push/fetch/fast-forward pull with real local bare object transfer"
	if err = checkRejectedRemote(ctx, git, round, gitEnv, repo, filepath.Join(round, "outside.git"), bare, callAt, &r); err != nil {
		return err
	}
	if err = checkTransportWiring(ctx, git, round, gitEnv, fakeGit, callAt, &r); err != nil {
		return err
	}
	if err = checkPullLocks(ctx, git, round, gitEnv, repo, bare, clone, callAt, fakeGit, &r); err != nil {
		return err
	}
	if err = checkAttributeSafety(ctx, git, round, gitEnv, callAt, &r); err != nil {
		return err
	}
	if err = checkPushReject(ctx, git, round, gitEnv, callAt, &r); err != nil {
		return err
	}
	if err = checkPullRecovery(ctx, git, round, gitEnv, fakeGit, callAt, &r); err != nil {
		return err
	}
	return nil
}

func checkFetchRefIsolation(ctx context.Context, git, round string, env []string, repo, bare string, call func(string, string) (string, error), r *report) error {
	mainOIDBytes, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	mainOID := strings.TrimSpace(string(mainOIDBytes))
	if err = gitRun(ctx, git, round, repo, env, "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, env, "update-ref", "refs/fintwind/gitremote-e2e-preserve", mainOID); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, env, "--git-dir", bare, "update-ref", "refs/heads/side", mainOID); err != nil {
		return err
	}
	if _, err = call("fetchRemote", repo); err != nil {
		return fmt.Errorf("fetch side branch: %w", err)
	}
	side, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "--verify", "refs/remotes/origin/side")
	if err != nil || strings.TrimSpace(string(side)) != mainOID {
		return errors.New("fetch did not create the remote side tracking ref")
	}
	if err = gitRun(ctx, git, round, repo, env, "--git-dir", bare, "update-ref", "-d", "refs/heads/side"); err != nil {
		return err
	}
	if _, err = call("fetchRemote", repo); err != nil {
		return fmt.Errorf("prune deleted side branch: %w", err)
	}
	if _, err = gitCapture(ctx, git, round, repo, env, "show-ref", "--verify", "refs/remotes/origin/side"); err == nil {
		return errors.New("deleted remote branch was not pruned")
	}
	if err = assertTrackingAlias(ctx, git, round, env, repo); err != nil {
		return err
	}
	preserve, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "--verify", "refs/fintwind/gitremote-e2e-preserve")
	if err != nil || strings.TrimSpace(string(preserve)) != mainOID {
		return errors.New("fetch/prune changed a non-remote ref namespace")
	}
	r.Results = append(r.Results, result{"fetch-prune-preserves-symbolic-tracking-head", "pass", "remote HEAD alias preserved; deleted remote branch pruned without touching refs/fintwind"})
	r.Coverage["fetchPrune"] = "seeded tracking refs; fetched/pruned one remote namespace; preserved origin/HEAD alias, main target, and refs/fintwind"
	r.Coverage["fetchPrune"] = "seeded tracking refs; fetched/pruned one remote namespace; preserved origin/HEAD alias, main target, and refs/fintwind"
	return nil
}

func assertTrackingAlias(ctx context.Context, git, round string, env []string, repo string) error {
	alias, err := gitCapture(ctx, git, round, repo, env, "symbolic-ref", "--quiet", "refs/remotes/origin/HEAD")
	if err != nil {
		return fmt.Errorf("remote HEAD alias missing: %w", err)
	}
	if strings.TrimSpace(string(alias)) != "refs/remotes/origin/main" {
		return fmt.Errorf("remote HEAD alias changed: %q", alias)
	}
	main, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "--verify", "refs/remotes/origin/main")
	if err != nil {
		return err
	}
	resolved, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "--verify", "refs/remotes/origin/HEAD")
	if err != nil {
		return err
	}
	if strings.TrimSpace(string(main)) != strings.TrimSpace(string(resolved)) {
		return errors.New("remote HEAD alias target was deleted or changed")
	}
	return nil
}

func checkRejectedRemote(ctx context.Context, git, round string, env []string, repo, outsideBare, localBare string, call func(string, string) (string, error), r *report) error {
	before, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	indexBefore, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	headBefore, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	refsBefore, err := gitCapture(ctx, git, round, repo, env, "for-each-ref", "--format=%(refname) %(objectname)")
	if err != nil {
		return err
	}
	marker := filepath.Join(round, "credential-helper-ran.marker")
	helper := "!cmd /c echo triggered > " + marker
	if runtime.GOOS != "windows" {
		helper = "!sh -c 'touch " + marker + "'"
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "credential.helper", helper); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, env, "remote", "set-url", "origin", fileURL(outsideBare)); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, filepath.Dir(outsideBare), env, "init", "--bare", outsideBare); err != nil {
		return fmt.Errorf("create outside-root bare fixture: %w", err)
	}
	if _, err = call("fetchRemote", repo); err == nil {
		return errors.New("file:// remote outside workspace root was accepted")
	}
	if exists(marker) {
		return errors.New("credential helper marker was produced")
	}
	badURLs := []string{"https://user:secret@127.0.0.1:1/rejected", "ext::echo unsafe", "git://fixture.invalid/repo", "ftp://fixture.invalid/repo", "ssh://-bad.invalid/repo", "ssh://-bad@fixture.invalid/repo", "ssh://user:password@fixture.invalid/repo", "ssh://bad;user@fixture.invalid/repo", "ssh://user@bad_host.invalid/repo", "-user@fixture.invalid:repo", "-bad.invalid:repo", "fixture.invalid::repo"}
	for _, target := range badURLs {
		if err = gitRun(ctx, git, round, repo, env, "config", "--local", "--replace-all", "remote.origin.url", target); err != nil {
			return err
		}
		if _, err = call("fetchRemote", repo); err == nil {
			return fmt.Errorf("unsafe remote target was accepted: %s", target)
		}
		if exists(marker) {
			return fmt.Errorf("credential helper marker was produced for target %q", target)
		}
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "--unset-all", "remote.origin.url"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "remote.origin.url", fileURL(localBare)); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "--add", "remote.origin.fetch", "+refs/tags/*:refs/tags/*"); err != nil {
		return err
	}
	if _, err = call("fetchRemote", repo); err == nil {
		return errors.New("custom fetch mapping was accepted")
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "--unset-all", "remote.origin.fetch"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "--add", "remote.origin.url", "https://second.invalid/repo"); err != nil {
		return err
	}
	if _, err = call("fetchRemote", repo); err == nil {
		return errors.New("multiple remote URLs were accepted")
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "--unset-all", "remote.origin.url"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "remote.origin.url", fileURL(localBare)); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "--add", "remote.origin.pushurl", "https://first.invalid/repo"); err != nil {
		return err
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "--add", "remote.origin.pushurl", "https://second.invalid/repo"); err != nil {
		return err
	}
	if _, err = call("push", repo); err == nil {
		return errors.New("multiple push URLs were accepted")
	}
	if err = gitRun(ctx, git, round, repo, env, "config", "--local", "--unset-all", "remote.origin.pushurl"); err != nil {
		return err
	}
	after, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	indexAfter, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	headAfter, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	refsAfter, err := gitCapture(ctx, git, round, repo, env, "for-each-ref", "--format=%(refname) %(objectname)")
	if err != nil {
		return err
	}
	if string(before) != string(after) || indexBefore != indexAfter || strings.TrimSpace(string(headBefore)) != strings.TrimSpace(string(headAfter)) || string(refsBefore) != string(refsAfter) {
		return fmt.Errorf("rejected remote operation changed state: worktree=%t index=%t HEAD=%t refs=%t; beforeRefs=%q afterRefs=%q", string(before) != string(after), indexBefore != indexAfter, strings.TrimSpace(string(headBefore)) != strings.TrimSpace(string(headAfter)), string(refsBefore) != string(refsAfter), string(refsBefore), string(refsAfter))
	}
	r.Results = append(r.Results, result{"reject-outside-root-file-url-and-embedded-credentials", "pass", "outside-root file URL and embedded credentials rejected; root-contained file URL remains supported; worktree/index/HEAD/refs unchanged"})
	r.Results = append(r.Results, result{"reject-unsafe-protocols-mappings-and-urls", "pass", "credentials, helper protocols, invalid SSH destinations, custom mapping, multiple URLs and root escape rejected"})
	r.Coverage["invalidRemoteStatePreserved"] = "unsafe URLs/mappings rejected without worktree, index, HEAD or ref changes"
	r.Coverage["invalidMappings"] = "multiple URL and unsupported custom fetch mapping rejected"
	r.Coverage["fileURLBoundary"] = "root-contained file URL push succeeded; outside-root file URL fetch refused"
	return nil
}

type fakeGitConfig struct {
	RealGit           string   `json:"realGit"`
	Log               string   `json:"log"`
	PauseFlag         string   `json:"pauseFlag"`
	Entered           string   `json:"entered"`
	Release           string   `json:"release"`
	PostLockPauseFlag string   `json:"postLockPauseFlag"`
	PostLockEntered   string   `json:"postLockEntered"`
	PostLockRelease   string   `json:"postLockRelease"`
	ExpectedLocks     []string `json:"expectedLocks"`
	RecoveryFaultFlag string   `json:"recoveryFaultFlag"`
	RecoveryFaultDone string   `json:"recoveryFaultDone"`
	RecoveryRef       string   `json:"recoveryRef"`
	RecoveryRefBackup string   `json:"recoveryRefBackup"`
	RecoverySentinel  string   `json:"recoverySentinel"`
}

func prepareFakeGit(round, realGit string) (*fakeGitConfig, error) {
	bin := filepath.Join(round, "fake-bin")
	if err := os.MkdirAll(bin, 0o700); err != nil {
		return nil, err
	}
	self, err := os.Executable()
	if err != nil {
		return nil, err
	}
	image, err := os.ReadFile(self)
	if err != nil {
		return nil, err
	}
	if err = os.WriteFile(filepath.Join(bin, "git.exe"), image, 0o700); err != nil {
		return nil, err
	}
	cfg := &fakeGitConfig{RealGit: realGit, Log: filepath.Join(round, "fake-git-records.jsonl"), PauseFlag: filepath.Join(round, "pause-read-tree"), Entered: filepath.Join(round, "read-tree-entered"), Release: filepath.Join(round, "release-read-tree")}
	b, err := json.Marshal(cfg)
	if err != nil {
		return nil, err
	}
	if err = os.WriteFile(filepath.Join(bin, "gitremote-fake.json"), b, 0o600); err != nil {
		return nil, err
	}
	return cfg, nil
}

func fakeGitMain() {
	if err := runFakeGit(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func runFakeGit() error {
	bin, err := os.Executable()
	if err != nil {
		return err
	}
	raw, err := os.ReadFile(filepath.Join(filepath.Dir(bin), "gitremote-fake.json"))
	if err != nil {
		return err
	}
	var cfg fakeGitConfig
	if err = json.Unmarshal(raw, &cfg); err != nil {
		return err
	}
	args := os.Args[1:]
	verb := gitVerb(args)
	if verb == "read-tree" {
		if err = recordFakeCommand(cfg, args); err != nil {
			return err
		}
	}
	if verb == "fetch" || verb == "push" {
		url := transportArg(args)
		if url != "" {
			intercepted := isFakeTransport(url)
			if err := recordTransport(cfg, args, intercepted); err != nil {
				return err
			}
			if intercepted {
				fmt.Fprintln(os.Stdout, "fake transport accepted; no network performed")
				return nil
			}
		}
	}
	if verb == "read-tree" && exists(cfg.PauseFlag) {
		if err := fakePause(cfg.Entered, cfg.Release); err != nil {
			return err
		}
	}
	if verb == "rev-parse" && hasGitPath(args, "MERGE_HEAD") && exists(cfg.PostLockPauseFlag) && allExist(cfg.ExpectedLocks) {
		if err := fakePause(cfg.PostLockEntered, cfg.PostLockRelease); err != nil {
			return err
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, cfg.RealGit, args...)
	cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
	cmd.Stdin = os.Stdin
	cmd.Env = os.Environ()
	if err := cmd.Run(); err != nil {
		var exit *exec.ExitError
		if errors.As(err, &exit) {
			os.Exit(exit.ExitCode())
		}
		return err
	}
	if verb == "read-tree" && exists(cfg.RecoveryFaultFlag) && !exists(cfg.RecoveryFaultDone) {
		if err = injectOwnedRefPublicationFailure(cfg); err != nil {
			return err
		}
	}
	return nil
}

func recordFakeCommand(cfg fakeGitConfig, args []string) error {
	encoded, err := json.Marshal(map[string]any{"kind": "git-command", "argv": args})
	if err != nil {
		return err
	}
	file, err := os.OpenFile(cfg.Log, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	defer file.Close()
	_, err = file.Write(append(encoded, '\n'))
	return err
}

func injectOwnedRefPublicationFailure(cfg fakeGitConfig) error {
	if !filepath.IsAbs(cfg.RecoveryRef) || !filepath.IsAbs(cfg.RecoveryRefBackup) || !filepath.IsAbs(cfg.RecoverySentinel) {
		return errors.New("recovery fault paths must be absolute")
	}
	if !filepath.IsLocal(relativeTo(filepath.Dir(cfg.Log), cfg.RecoveryRef)) ||
		!filepath.IsLocal(relativeTo(filepath.Dir(cfg.Log), cfg.RecoveryRefBackup)) ||
		!filepath.IsLocal(relativeTo(filepath.Dir(cfg.Log), cfg.RecoverySentinel)) {
		return errors.New("recovery fault path escaped the owned round")
	}
	if err := os.Rename(cfg.RecoveryRef, cfg.RecoveryRefBackup); err != nil {
		return fmt.Errorf("save owned branch ref before fault: %w", err)
	}
	if err := os.Mkdir(cfg.RecoveryRef, 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(cfg.RecoverySentinel, []byte("owned publication fault\n"), 0o600); err != nil {
		return err
	}
	return os.WriteFile(cfg.RecoveryFaultDone, []byte("injected after successful read-tree\n"), 0o600)
}

func relativeTo(base, target string) string {
	rel, err := filepath.Rel(base, target)
	if err != nil {
		return ".."
	}
	return rel
}

func hasGitPath(args []string, target string) bool {
	for i := 0; i+1 < len(args); i++ {
		if args[i] == "--git-path" && args[i+1] == target {
			return true
		}
	}
	return false
}

func fakePause(entered, release string) error {
	if err := os.WriteFile(entered, []byte("entered\n"), 0o600); err != nil {
		return err
	}
	deadline := time.Now().Add(30 * time.Second)
	for !exists(release) {
		if time.Now().After(deadline) {
			return errors.New("fake Git pause timed out")
		}
		time.Sleep(20 * time.Millisecond)
	}
	return nil
}

func allExist(paths []string) bool {
	if len(paths) == 0 {
		return false
	}
	for _, path := range paths {
		if !exists(path) {
			return false
		}
	}
	return true
}

func gitVerb(args []string) string {
	for i := 0; i < len(args); i++ {
		if args[i] == "-c" {
			i++
			continue
		}
		return args[i]
	}
	return ""
}

func transportArg(args []string) string {
	verbIndex := -1
	for i := 0; i < len(args); i++ {
		if args[i] == "-c" {
			i++
			continue
		}
		verbIndex = i
		break
	}
	if verbIndex < 0 || (args[verbIndex] != "fetch" && args[verbIndex] != "push") {
		return ""
	}
	for _, arg := range args[verbIndex+1:] {
		if strings.HasPrefix(arg, "-") {
			continue
		}
		return arg
	}
	return ""
}

func isFakeTransport(target string) bool {
	return strings.HasPrefix(target, "http://") || strings.HasPrefix(target, "https://") || strings.HasPrefix(target, "ssh://") || isSCPStyle(target)
}

func isSCPStyle(target string) bool {
	colon := strings.IndexByte(target, ':')
	if colon <= 0 || colon == 1 && len(target) > 2 && (target[2] == '/' || target[2] == '\\') {
		return false
	}
	left, right := target[:colon], target[colon+1:]
	return right != "" && !strings.ContainsAny(left, "/\\") && !strings.Contains(right, "::")
}

func recordTransport(cfg fakeGitConfig, args []string, intercepted bool) error {
	env := map[string]string{}
	for _, kv := range os.Environ() {
		key, value, ok := strings.Cut(kv, "=")
		if !ok {
			continue
		}
		upper := strings.ToUpper(key)
		if upper == "GIT_DIR" || upper == "GIT_OBJECT_DIRECTORY" || upper == "GIT_COMMON_DIR" || upper == "GIT_CONFIG_NOSYSTEM" || upper == "GIT_CONFIG_GLOBAL" || upper == "GIT_TERMINAL_PROMPT" || upper == "GIT_ASKPASS" || upper == "SSH_ASKPASS" || upper == "GIT_SSH_COMMAND" || upper == "GIT_SSH_VARIANT" || strings.HasPrefix(upper, "GIT_CONFIG_") || upper == "GIT_WORK_TREE" || upper == "GIT_INDEX_FILE" || upper == "HTTP_PROXY" || upper == "HTTPS_PROXY" {
			env[upper] = value
		}
	}
	gitDir := env["GIT_DIR"]
	config, err := os.ReadFile(filepath.Join(gitDir, "config"))
	if err != nil {
		return fmt.Errorf("fake transport cannot read temp Git config: %w", err)
	}
	knownHostsPath := sshKnownHostsPath(env["GIT_SSH_COMMAND"])
	knownHosts, err := os.ReadFile(knownHostsPath)
	if err != nil {
		return fmt.Errorf("fake transport cannot read copied known_hosts: %w", err)
	}
	kind := "forwarded-local-transport"
	if intercepted {
		kind = "fake-transport"
	}
	record := map[string]any{"kind": kind, "argv": args, "target": transportArg(args), "environment": env, "gitdirConfig": string(config), "knownHostsPath": knownHostsPath, "knownHostsContent": string(knownHosts)}
	b, err := json.Marshal(record)
	if err != nil {
		return err
	}
	f, err := os.OpenFile(cfg.Log, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	defer f.Close()
	_, err = f.Write(append(b, '\n'))
	return err
}

func sshKnownHostsPath(command string) string {
	const key = "UserKnownHostsFile="
	index := strings.Index(command, key)
	if index < 0 {
		return ""
	}
	value := strings.TrimSpace(command[index+len(key):])
	return strings.Trim(value, "'\"")
}

func fileURL(path string) string {
	abs, err := filepath.Abs(path)
	if err == nil {
		path = abs
	}
	urlPath := filepath.ToSlash(path)
	if runtime.GOOS == "windows" && len(urlPath) >= 2 && urlPath[1] == ':' {
		urlPath = "/" + urlPath
	}
	return (&url.URL{Scheme: "file", Path: urlPath}).String()
}

func fileURLLocalhost(path string) string {
	abs, err := filepath.Abs(path)
	if err == nil {
		path = abs
	}
	urlPath := filepath.ToSlash(path)
	if runtime.GOOS == "windows" && len(urlPath) >= 2 && urlPath[1] == ':' {
		urlPath = "/" + urlPath
	}
	return (&url.URL{Scheme: "file", Host: "localhost", Path: urlPath}).String()
}

func newFixtureRepo(ctx context.Context, git, round, path string, env []string) error {
	if err := os.MkdirAll(path, 0o700); err != nil {
		return err
	}
	for _, args := range [][]string{{"init", "-b", "main"}, {"config", "user.name", "E2E Fixture"}, {"config", "user.email", "fixture-e2e.invalid@example.invalid"}} {
		if err := gitRun(ctx, git, round, path, env, args...); err != nil {
			return err
		}
	}
	if err := os.WriteFile(filepath.Join(path, "base.txt"), []byte("base\n"), 0o600); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, path, env, "add", "base.txt"); err != nil {
		return err
	}
	return gitRun(ctx, git, round, path, env, "commit", "-m", "base")
}

func checkAttributeSafety(ctx context.Context, git, round string, env []string, call func(string, string) (string, error), r *report) error {
	workspace := filepath.Join(round, "workspace", "main")
	filterSource := filepath.Join(workspace, "filter-target-source")
	filterBare := filepath.Join(workspace, "filter-target.git")
	filterClone := filepath.Join(workspace, "filter-target-clone")
	if err := prepareRemoteClone(ctx, git, round, env, filterSource, filterBare, filterClone, call); err != nil {
		return err
	}
	pointer := "version https://git-lfs.github.com/spec/v1\noid sha256:1111111111111111111111111111111111111111111111111111111111111111\nsize 21\n"
	if err := os.WriteFile(filepath.Join(filterSource, "payload.dat"), []byte(pointer), 0o600); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, filterSource, env, "add", "payload.dat"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, filterSource, env, "commit", "-m", "add pointer without filter attributes"); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(filterSource, ".gitattributes"), []byte("*.dat filter=lfs\n"), 0o600); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, filterSource, env, "add", ".gitattributes"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, filterSource, env, "commit", "-m", "declare target tree filter"); err != nil {
		return err
	}
	if _, err := call("push", filterSource); err != nil {
		return fmt.Errorf("push isolated filter fixture: %w", err)
	}
	if err := assertFilterPullRefused(ctx, git, round, env, filterClone, []string{"base.txt"}, []string{"payload.dat", ".gitattributes"}, call); err != nil {
		return fmt.Errorf("target tree filter: %w", err)
	}
	r.Results = append(r.Results, result{"target-tree-gitattributes-filter-refused", "pass", "LFS-style pointer stayed out of the worktree; HEAD/index unchanged"})

	workSource := filepath.Join(workspace, "filter-worktree-source")
	workBare := filepath.Join(workspace, "filter-worktree.git")
	workClone := filepath.Join(workspace, "filter-worktree-clone")
	if err := prepareRemoteClone(ctx, git, round, env, workSource, workBare, workClone, call); err != nil {
		return err
	}
	attrs := filepath.Join(workClone, ".gitattributes")
	attrsBytes := []byte("*.txt filter=lfs\n")
	if err := os.WriteFile(attrs, attrsBytes, 0o600); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(workSource, "next.txt"), []byte("next commit\n"), 0o600); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, workSource, env, "add", "next.txt"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, workSource, env, "commit", "-m", "advance without tree filter"); err != nil {
		return err
	}
	if _, err := call("push", workSource); err != nil {
		return fmt.Errorf("push isolated working-tree filter fixture: %w", err)
	}
	if err := assertFilterPullRefused(ctx, git, round, env, workClone, []string{"base.txt", ".gitattributes"}, []string{"next.txt"}, call); err != nil {
		return fmt.Errorf("working tree filter: %w", err)
	}
	gotAttrs, err := os.ReadFile(attrs)
	if err != nil || string(gotAttrs) != string(attrsBytes) {
		return errors.New("pull changed current untracked .gitattributes")
	}
	r.Results = append(r.Results, result{"current-working-tree-attributes-filter-refused", "pass", "untracked .gitattributes filter refused without changing worktree/index/HEAD"})
	r.Coverage["filterAttributeSafety"] = "target-tree LFS filter and current untracked working-tree filter rejected before checkout"
	return nil
}

func prepareRemoteClone(ctx context.Context, git, round string, env []string, source, bare, clone string, call func(string, string) (string, error)) error {
	if err := newFixtureRepo(ctx, git, round, source, env); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, filepath.Dir(bare), env, "init", "--bare", bare); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, source, env, "remote", "add", "origin", fileURL(bare)); err != nil {
		return err
	}
	if _, err := call("push", source); err != nil {
		return fmt.Errorf("baseline push: %w", err)
	}
	if err := gitRun(ctx, git, round, source, env, "--git-dir", bare, "symbolic-ref", "HEAD", "refs/heads/main"); err != nil {
		return err
	}
	return gitRun(ctx, git, round, filepath.Dir(clone), env, "clone", bare, clone)
}

func assertFilterPullRefused(ctx context.Context, git, round string, env []string, repo string, trackedFiles, absentFiles []string, call func(string, string) (string, error)) error {
	head, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	if _, err = gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z"); err != nil {
		return err
	}
	status, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	index, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	files, err := fileFingerprint(repo, trackedFiles)
	if err != nil {
		return err
	}
	_, pullErr := call("pullFastForward", repo)
	if pullErr == nil {
		return errors.New("pull with filter attributes was accepted")
	}
	message := strings.ToLower(pullErr.Error())
	if !strings.Contains(message, "filter") && !strings.Contains(message, "attribute") {
		return fmt.Errorf("pull refused for an unrelated reason: %w", pullErr)
	}
	afterHead, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	afterStatus, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	afterIndex, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	afterFiles, err := fileFingerprint(repo, trackedFiles)
	if err != nil {
		return err
	}
	if string(head) != string(afterHead) || index != afterIndex || string(status) != string(afterStatus) || files != afterFiles {
		return errors.New("filter refusal changed HEAD/index/worktree")
	}
	for _, name := range absentFiles {
		if exists(filepath.Join(repo, name)) {
			return fmt.Errorf("filter refusal materialized %s", name)
		}
	}
	return nil
}

func checkPushReject(ctx context.Context, git, round string, env []string, call func(string, string) (string, error), r *report) error {
	workspace := filepath.Join(round, "workspace", "main")
	source := filepath.Join(workspace, "push-reject-source")
	bare := filepath.Join(workspace, "push-reject.git")
	updater := filepath.Join(workspace, "push-reject-updater")
	if err := newFixtureRepo(ctx, git, round, source, env); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, workspace, env, "init", "--bare", bare); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, source, env, "remote", "add", "origin", fileURL(bare)); err != nil {
		return err
	}
	if _, err := call("push", source); err != nil {
		return fmt.Errorf("initial reject fixture push: %w", err)
	}
	if err := gitRun(ctx, git, round, source, env, "--git-dir", bare, "symbolic-ref", "HEAD", "refs/heads/main"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, workspace, env, "clone", bare, updater); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, updater, env, "config", "user.name", "E2E Remote Updater"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, updater, env, "config", "user.email", "updater-e2e.invalid@example.invalid"); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(updater, "remote-only.txt"), []byte("remote advance\n"), 0o600); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, updater, env, "add", "remote-only.txt"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, updater, env, "commit", "-m", "advance remote independently"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, updater, env, "push", "origin", "main"); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(source, "source-only.txt"), []byte("local divergent advance\n"), 0o600); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, source, env, "add", "source-only.txt"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, source, env, "commit", "-m", "diverge source"); err != nil {
		return err
	}
	if _, err := gitCapture(ctx, git, round, source, env, "status", "--porcelain=v1", "-z"); err != nil {
		return err
	}
	head, err := gitCapture(ctx, git, round, source, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	index, err := fileSHA(filepath.Join(source, ".git", "index"))
	if err != nil {
		return err
	}
	status, err := gitCapture(ctx, git, round, source, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	refs, err := gitCapture(ctx, git, round, source, env, "for-each-ref", "--format=%(refname) %(objectname)")
	if err != nil {
		return err
	}
	files, err := fileFingerprint(source, []string{"base.txt", "source-only.txt"})
	if err != nil {
		return err
	}
	remoteHead, err := gitCapture(ctx, git, round, source, env, "--git-dir", bare, "rev-parse", "refs/heads/main")
	if err != nil {
		return err
	}
	remote, err := gitCapture(ctx, git, round, source, env, "config", "--local", "--get", "branch.main.remote")
	if err != nil {
		return err
	}
	merge, err := gitCapture(ctx, git, round, source, env, "config", "--local", "--get", "branch.main.merge")
	if err != nil {
		return err
	}
	beforeCount, err := countTargetPushes(fakeLogPath(round), bare)
	if err != nil {
		return err
	}
	if _, err = call("push", source); err == nil {
		return errors.New("non-fast-forward local bare push was accepted")
	}
	if _, err = gitCapture(ctx, git, round, source, env, "status", "--porcelain=v1", "-z"); err != nil {
		return err
	}
	afterHead, err := gitCapture(ctx, git, round, source, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	afterIndex, err := fileSHA(filepath.Join(source, ".git", "index"))
	if err != nil {
		return err
	}
	afterStatus, err := gitCapture(ctx, git, round, source, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	afterRefs, err := gitCapture(ctx, git, round, source, env, "for-each-ref", "--format=%(refname) %(objectname)")
	if err != nil {
		return err
	}
	afterFiles, err := fileFingerprint(source, []string{"base.txt", "source-only.txt"})
	if err != nil {
		return err
	}
	afterRemote, err := gitCapture(ctx, git, round, source, env, "--git-dir", bare, "rev-parse", "refs/heads/main")
	if err != nil {
		return err
	}
	afterRemoteConfig, err := gitCapture(ctx, git, round, source, env, "config", "--local", "--get", "branch.main.remote")
	if err != nil {
		return err
	}
	afterMerge, err := gitCapture(ctx, git, round, source, env, "config", "--local", "--get", "branch.main.merge")
	if err != nil {
		return err
	}
	if string(head) != string(afterHead) || index != afterIndex || string(status) != string(afterStatus) || string(refs) != string(afterRefs) || files != afterFiles || string(remoteHead) != string(afterRemote) || string(remote) != string(afterRemoteConfig) || string(merge) != string(afterMerge) {
		return errors.New("rejected push changed local or remote refs/config/worktree")
	}
	afterCount, err := countTargetPushes(fakeLogPath(round), bare)
	if err != nil {
		return err
	}
	if afterCount-beforeCount != 1 {
		return fmt.Errorf("push result was uncertain but Git was invoked %d times; expected exactly one and no retry", afterCount-beforeCount)
	}
	r.Results = append(r.Results, result{"non-fast-forward-push-rejected-once-preserves-state", "pass", "bare remote rejected divergence; local/remote refs and upstream unchanged; one attempt only"})
	r.Coverage["pushReject"] = "non-fast-forward bare push rejected once without local/remote or upstream mutation; no retry"
	return nil
}

func checkPullRecovery(ctx context.Context, git, round string, env []string, fake *fakeGitConfig, call func(string, string) (string, error), r *report) error {
	workspace := filepath.Join(round, "workspace", "main")
	legacySource, legacyBare, legacyClone := filepath.Join(workspace, "recovery-success-source"), filepath.Join(workspace, "recovery-success.git"), filepath.Join(workspace, "recovery-success-clone")
	if err := prepareRemoteClone(ctx, git, round, env, legacySource, legacyBare, legacyClone, call); err != nil {
		return err
	}
	legacyDir := filepath.Join(legacyClone, ".git", "fintwind-recovery", "pre-existing-fixture")
	if err := os.MkdirAll(legacyDir, 0o700); err != nil {
		return err
	}
	legacyMarker := filepath.Join(legacyDir, "keep.txt")
	if err := os.WriteFile(legacyMarker, []byte("must remain untouched\n"), 0o600); err != nil {
		return err
	}
	if err := advanceBareFixture(ctx, git, round, env, legacySource, "success-target.txt", "successful recovery publication\n", call); err != nil {
		return err
	}
	if _, err := call("fetchRemote", legacyClone); err != nil {
		return fmt.Errorf("fetch successful recovery fixture: %w", err)
	}
	if _, err := call("pullFastForward", legacyClone); err != nil {
		return fmt.Errorf("successful recovery fixture pull: %w", err)
	}
	if err := assertCleanFile(ctx, git, round, env, legacyClone, "success-target.txt", "successful recovery publication\n"); err != nil {
		return err
	}
	entries, err := os.ReadDir(filepath.Dir(legacyDir))
	if err != nil {
		return err
	}
	if len(entries) != 1 || entries[0].Name() != "pre-existing-fixture" {
		return errors.New("successful pull left a pending recovery directory or changed pre-existing recovery data")
	}
	if data, err := os.ReadFile(legacyMarker); err != nil || string(data) != "must remain untouched\n" {
		return errors.New("successful pull modified pre-existing recovery data")
	}
	r.Results = append(r.Results, result{"pull-recovery-success-cleans-owned-artifact", "pass", "successful fast-forward left no pending UUID artifact and preserved the pre-existing recovery directory"})

	source, bare, clone := filepath.Join(workspace, "recovery-fault-source"), filepath.Join(workspace, "recovery-fault.git"), filepath.Join(workspace, "recovery-fault-clone")
	if err := prepareRemoteClone(ctx, git, round, env, source, bare, clone, call); err != nil {
		return err
	}
	if err := advanceBareFixture(ctx, git, round, env, source, "fault-target.txt", "worktree advanced before controlled publication failure\n", call); err != nil {
		return err
	}
	if _, err := call("fetchRemote", clone); err != nil {
		return fmt.Errorf("fetch recovery failure fixture: %w", err)
	}
	gitDir := filepath.Join(clone, ".git")
	refPath := filepath.Join(gitDir, "refs", "heads", "main")
	backupRef := filepath.Join(round, "recovery-fault-original-ref")
	sentinel := filepath.Join(refPath, "owned-failure-marker")
	fake.RecoveryFaultFlag = filepath.Join(round, "inject-ref-publication-failure")
	fake.RecoveryFaultDone = filepath.Join(round, "ref-publication-failure-injected")
	fake.RecoveryRef, fake.RecoveryRefBackup, fake.RecoverySentinel = refPath, backupRef, sentinel
	if err := writeFakeGitConfig(round, fake); err != nil {
		return err
	}
	if err := os.WriteFile(fake.RecoveryFaultFlag, []byte("inject once after read-tree\n"), 0o600); err != nil {
		return err
	}
	oldHead, err := gitCapture(ctx, git, round, clone, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	oldIndex, err := fileSHA(filepath.Join(gitDir, "index"))
	if err != nil {
		return err
	}
	readTreesBefore, err := countGitCommands(fake.Log, "read-tree")
	if err != nil {
		return err
	}
	_, pullErr := call("pullFastForward", clone)
	if pullErr == nil {
		return errors.New("controlled ref publication failure was not reported")
	}
	if !exists(fake.RecoveryFaultDone) {
		return errors.New("owned fake Git did not inject failure after successful read-tree")
	}
	if !strings.Contains(strings.ToLower(pullErr.Error()), "recovery") || !strings.Contains(strings.ToLower(pullErr.Error()), "publication") {
		return fmt.Errorf("pull returned no explicit recovery/publication error: %w", pullErr)
	}
	readTreesAfter, err := countGitCommands(fake.Log, "read-tree")
	if err != nil {
		return err
	}
	if readTreesAfter-readTreesBefore != 1 {
		return fmt.Errorf("publication failure caused %d read-tree attempts; expected one and no retry", readTreesAfter-readTreesBefore)
	}
	if !exists(sentinel) {
		return errors.New("controlled ref publication obstruction is missing")
	}
	backupBytes, err := os.ReadFile(backupRef)
	if err != nil {
		return err
	}
	if strings.TrimSpace(string(backupBytes)) != strings.TrimSpace(string(oldHead)) {
		return errors.New("owned fault fixture did not preserve the prior branch ref")
	}
	currentIndex, err := fileSHA(filepath.Join(gitDir, "index"))
	if err != nil {
		return err
	}
	if currentIndex != oldIndex {
		return errors.New("ref publication failure unexpectedly changed the live index")
	}
	if err := assertWorktreeFile(clone, "fault-target.txt", "worktree advanced before controlled publication failure\n"); err != nil {
		return err
	}

	recoveryRoot := filepath.Join(gitDir, "fintwind-recovery")
	recoveryEntries, err := os.ReadDir(recoveryRoot)
	if err != nil {
		return fmt.Errorf("retained recovery artifact missing: %w", err)
	}
	if len(recoveryEntries) != 1 || !recoveryEntries[0].IsDir() {
		return fmt.Errorf("expected one uniquely owned recovery artifact, got %d entries", len(recoveryEntries))
	}
	recoveryDir := filepath.Join(recoveryRoot, recoveryEntries[0].Name())
	stateBytes, err := os.ReadFile(filepath.Join(recoveryDir, "state.json"))
	if err != nil {
		return err
	}
	var state struct {
		Version int    `json:"version"`
		Branch  string `json:"branch"`
		Before  string `json:"before"`
		Target  string `json:"target"`
		Stage   string `json:"stage"`
	}
	if err := json.Unmarshal(stateBytes, &state); err != nil {
		return err
	}
	if state.Version != 1 || state.Branch != "main" || strings.TrimSpace(state.Before) != strings.TrimSpace(string(oldHead)) || state.Target == "" || state.Target == state.Before || state.Stage != "checkout-complete" {
		return fmt.Errorf("recovery state is incomplete or at wrong stage: %+v", state)
	}
	beforeIndex, err := os.ReadFile(filepath.Join(recoveryDir, "before-index"))
	if err != nil {
		return err
	}
	afterIndex, err := os.ReadFile(filepath.Join(recoveryDir, "after-index"))
	if err != nil {
		return err
	}
	if fileSHABytes(beforeIndex) != oldIndex || len(afterIndex) == 0 || fileSHABytes(afterIndex) == oldIndex {
		return errors.New("recovery before/after index snapshots are missing or inconsistent")
	}
	if !strings.Contains(pullErr.Error(), filepath.Join("fintwind-recovery", recoveryEntries[0].Name())) {
		return errors.New("returned error does not identify the retained recovery artifact")
	}

	// Restore only the obstruction and backup created above; keep the recovery
	// artifact until the owned round is collected so the report remains auditable.
	if err := os.Remove(sentinel); err != nil {
		return err
	}
	if err := os.Remove(refPath); err != nil {
		return err
	}
	if err := os.Rename(backupRef, refPath); err != nil {
		return err
	}
	restoredRef, err := os.ReadFile(refPath)
	if err != nil {
		return err
	}
	if strings.TrimSpace(string(restoredRef)) != strings.TrimSpace(string(oldHead)) {
		return errors.New("owned fault fixture failed to restore its original branch ref")
	}
	if err := assertWorktreeFile(clone, "fault-target.txt", "worktree advanced before controlled publication failure\n"); err != nil {
		return err
	}
	r.Results = append(r.Results, result{"pull-publication-failure-retains-recovery-artifact", "pass", "owned fake Git caused ref publication failure after read-tree; before/after indexes and OIDs retained, old ref/index and advanced worktree locatable, no retry"})
	r.Coverage["pullPublicationRecovery"] = "owned isolated fixture only: post-read-tree ref destination obstruction forced publication refusal; recovery stage, both indexes, OIDs, old ref/index and updated worktree verified; no retry"

	capacitySource := filepath.Join(workspace, "recovery-capacity-source")
	capacityBare := filepath.Join(workspace, "recovery-capacity.git")
	capacityClone := filepath.Join(workspace, "recovery-capacity-clone")
	if err := prepareRemoteClone(ctx, git, round, env, capacitySource, capacityBare, capacityClone, call); err != nil {
		return err
	}
	capacityRoot := filepath.Join(capacityClone, ".git", "fintwind-recovery")
	if err := os.MkdirAll(capacityRoot, 0o700); err != nil {
		return err
	}
	capacitySnapshot := make(map[string]string, 8)
	for i := 0; i < 8; i++ {
		name := uuid.NewString()
		directory := filepath.Join(capacityRoot, name)
		if err := os.Mkdir(directory, 0o700); err != nil {
			return err
		}
		contents := fmt.Sprintf("pre-existing recovery entry %d\n", i)
		if err := os.WriteFile(filepath.Join(directory, "state.json"), []byte(contents), 0o600); err != nil {
			return err
		}
		capacitySnapshot[name] = contents
	}
	if err := advanceBareFixture(ctx, git, round, env, capacitySource, "capacity-target.txt", "must not be checked out at capacity\n", call); err != nil {
		return err
	}
	if _, err := call("fetchRemote", capacityClone); err != nil {
		return fmt.Errorf("fetch recovery capacity fixture: %w", err)
	}
	capacityGitDir := filepath.Join(capacityClone, ".git")
	capacityHead, err := gitCapture(ctx, git, round, capacityClone, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	capacityIndex, err := fileSHA(filepath.Join(capacityGitDir, "index"))
	if err != nil {
		return err
	}
	capacityStatus, err := gitCapture(ctx, git, round, capacityClone, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	capacityRefs, err := gitCapture(ctx, git, round, capacityClone, env, "for-each-ref", "--format=%(refname) %(objectname)")
	if err != nil {
		return err
	}
	capacityWorktree, err := fileFingerprint(capacityClone, []string{"base.txt"})
	if err != nil {
		return err
	}
	readTreesBeforeCapacity, err := countGitCommands(fake.Log, "read-tree")
	if err != nil {
		return err
	}
	_, capacityErr := call("pullFastForward", capacityClone)
	if capacityErr == nil || !strings.Contains(strings.ToLower(capacityErr.Error()), "capacity") {
		return fmt.Errorf("eight retained recovery entries did not cause an explicit capacity refusal: %v", capacityErr)
	}
	readTreesAfterCapacity, err := countGitCommands(fake.Log, "read-tree")
	if err != nil {
		return err
	}
	if readTreesAfterCapacity != readTreesBeforeCapacity {
		return errors.New("capacity refusal reached read-tree")
	}
	afterCapacityHead, err := gitCapture(ctx, git, round, capacityClone, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	afterCapacityIndex, err := fileSHA(filepath.Join(capacityGitDir, "index"))
	if err != nil {
		return err
	}
	afterCapacityStatus, err := gitCapture(ctx, git, round, capacityClone, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	afterCapacityRefs, err := gitCapture(ctx, git, round, capacityClone, env, "for-each-ref", "--format=%(refname) %(objectname)")
	if err != nil {
		return err
	}
	afterCapacityWorktree, err := fileFingerprint(capacityClone, []string{"base.txt"})
	if err != nil {
		return err
	}
	capacityEntries, err := os.ReadDir(capacityRoot)
	if err != nil {
		return err
	}
	if len(capacityEntries) != 8 || string(capacityHead) != string(afterCapacityHead) || capacityIndex != afterCapacityIndex || string(capacityStatus) != string(afterCapacityStatus) || string(capacityRefs) != string(afterCapacityRefs) || capacityWorktree != afterCapacityWorktree || exists(filepath.Join(capacityClone, "capacity-target.txt")) {
		return errors.New("recovery capacity refusal changed repository state or recovery entry count")
	}
	for _, entry := range capacityEntries {
		want, ok := capacitySnapshot[entry.Name()]
		if !ok || !entry.IsDir() {
			return errors.New("recovery capacity refusal changed existing recovery names")
		}
		contents, err := os.ReadFile(filepath.Join(capacityRoot, entry.Name(), "state.json"))
		if err != nil {
			return err
		}
		if string(contents) != want {
			return fmt.Errorf("recovery capacity refusal changed existing entry %s", entry.Name())
		}
	}
	r.Results = append(r.Results, result{"pull-recovery-capacity-eight-refuses-before-checkout", "pass", "eight existing UUID recovery entries preserved; pull refused before read-tree with HEAD/index/refs/worktree unchanged"})
	r.Coverage["pullRecoveryCapacity"] = "exactly eight pre-existing UUID entries refused before read-tree; repository state and every existing entry preserved"
	return nil
}

func advanceBareFixture(ctx context.Context, git, round string, env []string, source, name, content string, call func(string, string) (string, error)) error {
	if err := os.WriteFile(filepath.Join(source, name), []byte(content), 0o600); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, source, env, "add", name); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, source, env, "commit", "-m", "advance recovery fixture"); err != nil {
		return err
	}
	if _, err := call("push", source); err != nil {
		return fmt.Errorf("push recovery fixture target: %w", err)
	}
	return nil
}

func assertCleanFile(ctx context.Context, git, round string, env []string, repo, name, content string) error {
	if err := assertWorktreeFile(repo, name, content); err != nil {
		return err
	}
	status, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	if len(status) != 0 {
		return errors.New("successful recovery fixture left a dirty worktree")
	}
	return nil
}

func assertWorktreeFile(repo, name, content string) error {
	data, err := os.ReadFile(filepath.Join(repo, name))
	if err != nil {
		return err
	}
	if string(data) != content {
		return fmt.Errorf("worktree file %s does not contain target content", name)
	}
	return nil
}

func fileSHABytes(data []byte) string { sum := sha256.Sum256(data); return hex.EncodeToString(sum[:]) }

func writeFakeGitConfig(round string, fake *fakeGitConfig) error {
	data, err := json.Marshal(fake)
	if err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(round, "fake-bin", "gitremote-fake.json"), data, 0o600)
}

func countGitCommands(path, wanted string) (int, error) {
	records, err := readFakeRecords(path)
	if err != nil {
		return 0, err
	}
	count := 0
	for _, record := range records {
		if record["kind"] == "git-command" && gitVerbFromRecord(record) == wanted {
			count++
		}
	}
	return count, nil
}

func fakeLogPath(round string) string { return filepath.Join(round, "fake-git-records.jsonl") }
func countTargetPushes(path, target string) (int, error) {
	records, err := readFakeRecords(path)
	if err != nil {
		return 0, err
	}
	count := 0
	for _, record := range records {
		kind, _ := record["kind"].(string)
		verb := gitVerbFromRecord(record)
		value, _ := record["target"].(string)
		if kind == "forwarded-local-transport" && verb == "push" && strings.EqualFold(filepath.Clean(value), filepath.Clean(target)) {
			count++
		}
	}
	return count, nil
}
func gitVerbFromRecord(record map[string]any) string {
	raw, _ := record["argv"].([]any)
	args := make([]string, 0, len(raw))
	for _, v := range raw {
		if s, ok := v.(string); ok {
			args = append(args, s)
		}
	}
	return gitVerb(args)
}

func checkTransportWiring(ctx context.Context, git, round string, env []string, fake *fakeGitConfig, call func(string, string) (string, error), r *report) error {
	workspace := filepath.Join(round, "workspace", "main")
	bare := filepath.Join(workspace, "remote-upstream.git")
	if err := gitRun(ctx, git, round, workspace, env, "init", "--bare", bare); err != nil {
		return err
	}
	probe := filepath.Join(workspace, "probe-upstream")
	if err := newFixtureRepo(ctx, git, round, probe, env); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, probe, env, "remote", "add", "origin", fileURL(bare)); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, probe, env, "config", "--local", "branch.main.remote", "origin"); err != nil {
		return err
	}
	if _, err := call("push", probe); err != nil {
		return fmt.Errorf("remote-only upstream push: %w", err)
	}
	remote, err := gitCapture(ctx, git, round, probe, env, "config", "--local", "--get", "branch.main.remote")
	if err != nil {
		return err
	}
	merge, err := gitCapture(ctx, git, round, probe, env, "config", "--local", "--get", "branch.main.merge")
	if err != nil {
		return err
	}
	if strings.TrimSpace(string(remote)) != "origin" || strings.TrimSpace(string(merge)) != "refs/heads/main" {
		return fmt.Errorf("push did not complete partial upstream: remote=%q merge=%q", remote, merge)
	}
	r.Results = append(r.Results, result{"remote-only-upstream-completed", "pass", "remote existed without merge; successful push wrote both upstream fields"})

	netRepo := filepath.Join(workspace, "probe-network")
	if err := newFixtureRepo(ctx, git, round, netRepo, env); err != nil {
		return err
	}
	markerHelper := filepath.Join(round, "network-helper.marker")
	markerRewrite := filepath.Join(round, "rewrite-helper.marker")
	markerSSH := filepath.Join(round, "ssh-command.marker")
	helper := "!cmd /c echo triggered > " + markerHelper
	sshCommand := "cmd /c echo triggered > " + markerSSH
	if runtime.GOOS != "windows" {
		helper = "!sh -c 'touch " + markerHelper + "'"
		sshCommand = "sh -c 'touch " + markerSSH + "'"
	}
	for _, pair := range [][2]string{{"credential.helper", helper}, {"core.sshCommand", sshCommand}} {
		if err := gitRun(ctx, git, round, netRepo, env, "config", "--local", pair[0], pair[1]); err != nil {
			return err
		}
	}
	// A hostile rewrite would point at an ext helper. The fake transport records
	// the original target before Git protocol dispatch; its config evidence must
	// be the sanitized temporary gitdir, not this source repo config.
	rewriteBase := "ext::cmd /c echo rewritten > " + markerRewrite
	if runtime.GOOS != "windows" {
		rewriteBase = "ext::sh -c 'touch " + markerRewrite + "'"
	}
	if err := gitRun(ctx, git, round, netRepo, env, "config", "--local", "url."+rewriteBase+".insteadOf", "https://wire.invalid/"); err != nil {
		return fmt.Errorf("configure isolated URL rewrite fixture: %w", err)
	}
	urls := []string{"https://wire.invalid/owner/repo.git", "ssh://fixtureuser@fixture.invalid/owner/repo.git", "ssh://fixtureuser@127.0.0.1:2222/owner/repo.git"}
	for i, target := range urls {
		if i == 0 {
			if err := gitRun(ctx, git, round, netRepo, env, "remote", "add", "origin", target); err != nil {
				return err
			}
		} else if err := gitRun(ctx, git, round, netRepo, env, "remote", "set-url", "origin", target); err != nil {
			return err
		}
		if _, err := call("push", netRepo); err != nil {
			return fmt.Errorf("fake transport push %s: %w", target, err)
		}
	}
	if err := gitRun(ctx, git, round, netRepo, env, "remote", "set-url", "origin", "fixtureuser@fixture.invalid:owner/repo.git"); err != nil {
		return err
	}
	if _, err := call("fetchRemote", netRepo); err != nil {
		return fmt.Errorf("fake scp-style fetch: %w", err)
	}
	if exists(markerHelper) || exists(markerRewrite) || exists(markerSSH) {
		return errors.New("malicious credential/helper/rewrite/SSH marker was produced")
	}
	records, err := readFakeRecords(fake.Log)
	if err != nil {
		return err
	}
	want := map[string]bool{"https://wire.invalid/owner/repo.git": false, "ssh://fixtureuser@fixture.invalid/owner/repo.git": false, "ssh://fixtureuser@127.0.0.1:2222/owner/repo.git": false, "fixtureuser@fixture.invalid:owner/repo.git": false}
	for _, record := range records {
		target, _ := record["target"].(string)
		if _, ok := want[target]; !ok {
			continue
		}
		want[target] = true
		envMap, _ := record["environment"].(map[string]any)
		config, _ := record["gitdirConfig"].(string)
		argv, _ := record["argv"].([]any)
		gitDir, _ := envMap["GIT_DIR"].(string)
		objectDir, _ := envMap["GIT_OBJECT_DIRECTORY"].(string)
		rel, e := filepath.Rel(filepath.Join(round, "tmp"), gitDir)
		if e != nil || !filepath.IsLocal(rel) {
			return fmt.Errorf("network gitdir escaped isolated temp: %q", gitDir)
		}
		if _, ok := envMap["GIT_COMMON_DIR"]; ok {
			return errors.New("network Git received GIT_COMMON_DIR")
		}
		if !strings.Contains(strings.ToLower(config), "[credential]\nhelper =") || strings.Contains(config, markerHelper) || strings.Contains(config, markerRewrite) || strings.Contains(config, markerSSH) {
			return errors.New("network gitdir config is not the expected sanitized config")
		}
		if !filepath.IsAbs(objectDir) || !strings.Contains(strings.ToLower(objectDir), strings.ToLower(filepath.Join(workspace, "probe-network", ".git", "objects"))) {
			return fmt.Errorf("network Git object directory is not probe repo objects: %q", objectDir)
		}
		flat := fmt.Sprint(argv)
		if strings.HasPrefix(target, "https://") || strings.HasPrefix(target, "ssh://") {
			if !strings.Contains(flat, target) {
				return fmt.Errorf("fake Git argv omitted exact target %q", target)
			}
			if !strings.Contains(flat, ":refs/heads/main") || strings.Contains(flat, "--force") {
				return fmt.Errorf("fake Git push was not explicit non-force refspec: %v", argv)
			}
		}
		if envMap["HTTP_PROXY"] != "http://127.0.0.1:7897" || envMap["HTTPS_PROXY"] != "http://127.0.0.1:7897" {
			return errors.New("fake transport did not receive the isolated explicit proxy environment")
		}
		if strings.HasPrefix(target, "ssh://") {
			ssh, _ := envMap["GIT_SSH_COMMAND"].(string)
			for _, required := range []string{"IdentityAgent=none", "IdentitiesOnly=yes", "StrictHostKeyChecking=yes"} {
				if !strings.Contains(ssh, required) {
					return fmt.Errorf("SSH env missing %s", required)
				}
			}
			known, _ := record["knownHostsContent"].(string)
			knownPath, _ := record["knownHostsPath"].(string)
			if known != e2eKnownHosts {
				return errors.New("fake SSH host trust did not match isolated profile fixture")
			}
			knownRel, e := filepath.Rel(gitDir, knownPath)
			if e != nil || !filepath.IsLocal(knownRel) {
				return errors.New("copied known_hosts escaped sanitized temporary gitdir")
			}
		}
	}
	for target, seen := range want {
		if !seen {
			return fmt.Errorf("fake transport did not record %s", target)
		}
	}
	r.Results = append(r.Results, result{"safe-http-ssh-scp-wiring-and-marker-isolation", "pass", "fake transport only: exact target argv and sanitized GIT_DIR/config/env verified; no external transfer"})
	r.Coverage["safeHttpsSshArgv"] = "HTTPS/SSH DNS+IP/scp argv recorded by owned fake Git; sanitized env/config and copied known_hosts bytes inspected; no network claim"
	r.Coverage["maliciousConfigMarkers"] = "credential helper, ext URL rewrite and core.sshCommand markers remained absent under fake transport"
	return nil
}

func readFakeRecords(path string) ([]map[string]any, error) {
	b, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	out := []map[string]any{}
	for _, line := range strings.Split(string(b), "\n") {
		if strings.TrimSpace(line) == "" {
			continue
		}
		var x map[string]any
		if err := json.Unmarshal([]byte(line), &x); err != nil {
			return nil, err
		}
		out = append(out, x)
	}
	return out, nil
}

func checkPullLocks(ctx context.Context, git, round string, env []string, source, bare, clone string, call func(string, string) (string, error), fake *fakeGitConfig, r *report) error {
	divergent := filepath.Join(filepath.Dir(source), "nonff-clone")
	if err := gitRun(ctx, git, round, filepath.Dir(source), env, "clone", bare, divergent); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, divergent, env, "config", "user.name", "E2E Divergent"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, divergent, env, "config", "user.email", "divergent-e2e.invalid@example.invalid"); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(divergent, "diverge.txt"), []byte("local divergent commit\n"), 0o600); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, divergent, env, "add", "diverge.txt"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, divergent, env, "commit", "-m", "divergent"); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(source, "third.txt"), []byte("third offline commit\n"), 0o600); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, source, env, "add", "third.txt"); err != nil {
		return err
	}
	if err := gitRun(ctx, git, round, source, env, "commit", "-m", "third"); err != nil {
		return err
	}
	if _, err := call("push", source); err != nil {
		return fmt.Errorf("prepare lock-race remote commit: %w", err)
	}
	if err := checkNonFastForward(ctx, git, round, env, divergent, call); err != nil {
		return err
	}
	r.Results = append(r.Results, result{"non-fast-forward-pull-preserves-state", "pass", "divergent local history refused without changing HEAD/index/worktree"})
	if err := checkDirtyPull(ctx, git, round, env, clone, call); err != nil {
		return err
	}
	r.Results = append(r.Results, result{"dirty-pull-preserves-state", "pass", "dirty tracked file, HEAD and index remained unchanged"})
	if err := checkFilteredPull(ctx, git, round, env, clone, call); err != nil {
		return err
	}
	r.Results = append(r.Results, result{"filter-attributes-include-pull-refused", "pass", "filter/include config rejected before checkout; marker absent and repository state unchanged"})
	if err := checkMergeMetadata(ctx, git, round, env, clone, call); err != nil {
		return err
	}
	r.Results = append(r.Results, result{"in-progress-merge-rebase-pull-refused", "pass", "MERGE_HEAD, cherry-pick, revert, rebase and sequencer markers refused"})
	if err := checkPostLockMetadataRace(ctx, git, round, env, clone, fake, call); err != nil {
		return err
	}
	beforeHead, err := os.ReadFile(filepath.Join(clone, ".git", "HEAD"))
	if err != nil {
		return err
	}
	beforeIndex, err := fileSHA(filepath.Join(clone, ".git", "index"))
	if err != nil {
		return err
	}
	beforeRef, err := os.ReadFile(filepath.Join(clone, ".git", "refs", "heads", "main"))
	if err != nil {
		return err
	}
	parent, err := gitCapture(ctx, git, round, clone, env, "rev-parse", "HEAD^")
	if err != nil {
		return err
	}
	parentOID := strings.TrimSpace(string(parent))
	beforeFiles, err := fileFingerprint(clone, []string{"fixture.txt", "second.txt"})
	if err != nil {
		return err
	}
	if err := os.WriteFile(fake.PauseFlag, []byte("pause\n"), 0o600); err != nil {
		return err
	}
	defer func() {
		if !exists(fake.Release) {
			_ = os.WriteFile(fake.Release, []byte("release\n"), 0o600)
		}
	}()
	response := make(chan error, 1)
	go func() { _, err := call("pullFastForward", clone); response <- err }()
	if err := waitForFile(ctx, fake.Entered, 20*time.Second); err != nil {
		_ = os.WriteFile(fake.Release, []byte("release\n"), 0o600)
		<-response
		return fmt.Errorf("read-tree pause not reached: %w", err)
	}
	locks := []string{filepath.Join(clone, ".git", "index.lock"), filepath.Join(clone, ".git", "HEAD.lock"), filepath.Join(clone, ".git", "refs", "heads", "main.lock")}
	for _, lock := range locks {
		if !exists(lock) {
			_ = os.WriteFile(fake.Release, []byte("release\n"), 0o600)
			<-response
			return fmt.Errorf("expected pull-owned lock missing: %s", lock)
		}
	}
	attempts := [][]string{{"commit", "--allow-empty", "-m", "lock-race-attempt"}, {"checkout", "--detach", parentOID}, {"update-ref", "refs/heads/main", parentOID}}
	for _, args := range attempts {
		e := gitRun(ctx, git, round, clone, env, args...)
		if e == nil {
			_ = os.WriteFile(fake.Release, []byte("release\n"), 0o600)
			<-response
			return fmt.Errorf("external git %s unexpectedly succeeded during pull lock hold", strings.Join(args, " "))
		}
		if !strings.Contains(strings.ToLower(e.Error()), "lock") {
			_ = os.WriteFile(fake.Release, []byte("release\n"), 0o600)
			<-response
			return fmt.Errorf("external git %s failed for a reason other than a lock: %w", strings.Join(args, " "), e)
		}
	}
	afterHead, err := os.ReadFile(filepath.Join(clone, ".git", "HEAD"))
	if err != nil {
		return err
	}
	afterRef, err := os.ReadFile(filepath.Join(clone, ".git", "refs", "heads", "main"))
	if err != nil {
		return err
	}
	afterIndex, err := fileSHA(filepath.Join(clone, ".git", "index"))
	if err != nil {
		return err
	}
	afterFiles, err := fileFingerprint(clone, []string{"fixture.txt", "second.txt"})
	if err != nil {
		return err
	}
	if string(beforeHead) != string(afterHead) || string(beforeRef) != string(afterRef) || beforeIndex != afterIndex || beforeFiles != afterFiles {
		_ = os.WriteFile(fake.Release, []byte("release\n"), 0o600)
		<-response
		return errors.New("external Git changed HEAD/branch/index/worktree while daemon locks were held")
	}
	if err = os.WriteFile(fake.Release, []byte("release\n"), 0o600); err != nil {
		return err
	}
	if err = <-response; err != nil {
		return fmt.Errorf("pull after releasing fake read-tree: %w", err)
	}
	got, err := gitCapture(ctx, git, round, clone, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	want, err := gitCapture(ctx, git, round, source, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	if strings.TrimSpace(string(got)) != strings.TrimSpace(string(want)) {
		return errors.New("locked pull completed with mismatched branch HEAD")
	}
	status, err := gitCapture(ctx, git, round, clone, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	if len(status) != 0 {
		return errors.New("locked pull left worktree/index dirty")
	}
	for _, lock := range locks {
		if exists(lock) {
			return fmt.Errorf("pull left owned lock behind: %s", lock)
		}
	}
	content, err := os.ReadFile(filepath.Join(clone, "third.txt"))
	if err != nil || string(content) != "third offline commit\n" {
		return errors.New("locked pull worktree content mismatch")
	}
	r.Results = append(r.Results, result{"external-git-blocked-by-pull-index-head-branch-locks", "pass", "owned commit/checkout/update-ref attempts failed while all three lockfiles existed; pull then completed consistently"})
	r.Coverage["pullLockRace"] = "paused read-tree lock race covered with actual owned Git commands; HEAD/index/worktree unchanged by rejected competitors"
	r.Coverage["pullFailureBoundaries"] = "non-fast-forward, dirty tracked worktree and active merge/rebase metadata refusals preserved local state"
	r.Coverage["pullLockStateRecheck"] = "MERGE_HEAD inserted while all three pull locks were held; pull rejected and released only its own locks"
	return nil
}

func checkPostLockMetadataRace(ctx context.Context, git, round string, env []string, repo string, fake *fakeGitConfig, call func(string, string) (string, error)) error {
	fake.ExpectedLocks = []string{filepath.Join(repo, ".git", "index.lock"), filepath.Join(repo, ".git", "HEAD.lock"), filepath.Join(repo, ".git", "refs", "heads", "main.lock")}
	fake.PostLockPauseFlag = filepath.Join(round, "pause-post-lock-state-check")
	fake.PostLockEntered = filepath.Join(round, "post-lock-state-check-entered")
	fake.PostLockRelease = filepath.Join(round, "release-post-lock-state-check")
	encoded, err := json.Marshal(fake)
	if err != nil {
		return err
	}
	if err = os.WriteFile(filepath.Join(filepath.Dir(fake.Log), "fake-bin", "gitremote-fake.json"), encoded, 0o600); err != nil {
		return err
	}
	head, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	if _, err = gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z"); err != nil {
		return err
	}
	index, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	files, err := fileFingerprint(repo, []string{"fixture.txt", "second.txt"})
	if err != nil {
		return err
	}
	if err = os.WriteFile(fake.PostLockPauseFlag, []byte("pause\n"), 0o600); err != nil {
		return err
	}
	response := make(chan error, 1)
	go func() { _, e := call("pullFastForward", repo); response <- e }()
	if err = waitForFile(ctx, fake.PostLockEntered, 20*time.Second); err != nil {
		_ = os.WriteFile(fake.PostLockRelease, []byte("release\n"), 0o600)
		<-response
		return fmt.Errorf("post-lock status pause not reached: %w", err)
	}
	if !allExist(fake.ExpectedLocks) {
		_ = os.WriteFile(fake.PostLockRelease, []byte("release\n"), 0o600)
		<-response
		return errors.New("post-lock recheck pause did not hold all Git locks")
	}
	mergeHead := filepath.Join(repo, ".git", "MERGE_HEAD")
	if exists(mergeHead) {
		_ = os.WriteFile(fake.PostLockRelease, []byte("release\n"), 0o600)
		<-response
		return errors.New("fixture MERGE_HEAD already exists")
	}
	if err = os.WriteFile(mergeHead, []byte(strings.TrimSpace(string(head))+"\n"), 0o600); err != nil {
		_ = os.WriteFile(fake.PostLockRelease, []byte("release\n"), 0o600)
		<-response
		return err
	}
	if err = os.WriteFile(fake.PostLockRelease, []byte("release\n"), 0o600); err != nil {
		return err
	}
	if err = <-response; err == nil {
		_ = os.Remove(mergeHead)
		return errors.New("pull ignored MERGE_HEAD introduced while its Git locks were held")
	}
	if err = os.Remove(mergeHead); err != nil {
		return err
	}
	if _, err = os.Stat(fake.PostLockPauseFlag); err == nil {
		if err = os.Remove(fake.PostLockPauseFlag); err != nil {
			return err
		}
	}
	afterHead, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	status, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	afterIndex, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	afterFiles, err := fileFingerprint(repo, []string{"fixture.txt", "second.txt"})
	if err != nil {
		return err
	}
	if string(head) != string(afterHead) || index != afterIndex || files != afterFiles || len(status) != 0 {
		return errors.New("post-lock metadata refusal changed HEAD/index/worktree")
	}
	for _, lock := range fake.ExpectedLocks {
		if exists(lock) {
			return fmt.Errorf("metadata refusal left owned lock behind: %s", lock)
		}
	}
	return nil
}

func checkNonFastForward(ctx context.Context, git, round string, env []string, repo string, call func(string, string) (string, error)) error {
	head, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	status, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	index, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	diverge, err := os.ReadFile(filepath.Join(repo, "diverge.txt"))
	if err != nil {
		return err
	}
	if _, err = call("pullFastForward", repo); err == nil {
		return errors.New("non-fast-forward pull was accepted")
	}
	afterHead, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	afterStatus, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	afterIndex, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	afterDiverge, err := os.ReadFile(filepath.Join(repo, "diverge.txt"))
	if err != nil {
		return err
	}
	if string(head) != string(afterHead) || index != afterIndex || string(status) != string(afterStatus) || string(diverge) != string(afterDiverge) {
		return errors.New("non-fast-forward pull changed local HEAD/index/worktree")
	}
	return nil
}

func checkDirtyPull(ctx context.Context, git, round string, env []string, repo string, call func(string, string) (string, error)) error {
	head, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	if _, err = gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z"); err != nil {
		return err
	}
	index, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	path := filepath.Join(repo, "fixture.txt")
	before, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	dirty := []byte("locally modified during pull test\n")
	if err = os.WriteFile(path, dirty, 0o600); err != nil {
		return err
	}
	if _, err = call("pullFastForward", repo); err == nil {
		_ = os.WriteFile(path, before, 0o600)
		return errors.New("dirty tracked worktree pull was accepted")
	}
	afterHead, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	if _, err = gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z"); err != nil {
		return err
	}
	afterIndex, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	content, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	if string(head) != string(afterHead) || index != afterIndex || string(content) != string(dirty) {
		return errors.New("dirty pull changed HEAD/index or overwrote the dirty worktree")
	}
	if err = os.WriteFile(path, before, 0o600); err != nil {
		return err
	}
	return nil
}

func checkMergeMetadata(ctx context.Context, git, round string, env []string, repo string, call func(string, string) (string, error)) error {
	head, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	status, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	index, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	gitDir := filepath.Join(repo, ".git")
	oid := strings.TrimSpace(string(head))
	markers := []string{"MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"}
	for _, name := range markers {
		path := filepath.Join(gitDir, name)
		if exists(path) {
			return fmt.Errorf("fixture metadata already exists: %s", name)
		}
		if err = os.WriteFile(path, []byte(oid+"\n"), 0o600); err != nil {
			return err
		}
		if _, err = call("pullFastForward", repo); err == nil {
			_ = os.Remove(path)
			return fmt.Errorf("pull accepted active metadata %s", name)
		}
		if err = os.Remove(path); err != nil {
			return err
		}
		if err = assertPullState(ctx, git, round, env, repo, head, index, status); err != nil {
			return fmt.Errorf("metadata %s: %w", name, err)
		}
	}
	for _, name := range []string{"rebase-merge", "rebase-apply", "sequencer"} {
		dir := filepath.Join(gitDir, name)
		if exists(dir) {
			return fmt.Errorf("fixture metadata directory already exists: %s", name)
		}
		if err = os.Mkdir(dir, 0o700); err != nil {
			return err
		}
		marker := filepath.Join(dir, "todo")
		if err = os.WriteFile(marker, []byte("pick fixture\n"), 0o600); err != nil {
			return err
		}
		if _, err = call("pullFastForward", repo); err == nil {
			_ = os.Remove(marker)
			_ = os.Remove(dir)
			return fmt.Errorf("pull accepted active metadata directory %s", name)
		}
		if err = os.Remove(marker); err != nil {
			return err
		}
		if err = os.Remove(dir); err != nil {
			return err
		}
		if err = assertPullState(ctx, git, round, env, repo, head, index, status); err != nil {
			return fmt.Errorf("metadata %s: %w", name, err)
		}
	}
	return nil
}

func checkFilteredPull(ctx context.Context, git, round string, env []string, repo string, call func(string, string) (string, error)) error {
	head, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	status, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	index, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	refs, err := gitCapture(ctx, git, round, repo, env, "for-each-ref", "--format=%(refname) %(objectname)")
	if err != nil {
		return err
	}
	marker := filepath.Join(round, "filter-smudge.marker")
	helper := "!cmd /c echo triggered > " + marker
	if runtime.GOOS != "windows" {
		helper = "!sh -c 'touch " + marker + "'"
	}
	include := filepath.Join(round, "unused-include.ini")
	if err = os.WriteFile(include, []byte("[user]\nname = Included Fixture\n"), 0o600); err != nil {
		return err
	}
	settings := [][2]string{{"filter.lfs.smudge", helper}, {"core.attributesFile", filepath.Join(round, "attributes")}, {"include.path", include}}
	for _, setting := range settings {
		if err = gitRun(ctx, git, round, repo, env, "config", "--local", setting[0], setting[1]); err != nil {
			return err
		}
		if _, err = call("pullFastForward", repo); err == nil {
			_, _ = gitCapture(ctx, git, round, repo, env, "config", "--local", "--unset-all", setting[0])
			return fmt.Errorf("pull accepted protected config %s", setting[0])
		}
		if exists(marker) {
			return fmt.Errorf("filter marker executed for %s", setting[0])
		}
		if err = gitRun(ctx, git, round, repo, env, "config", "--local", "--unset-all", setting[0]); err != nil {
			return err
		}
		if err = assertPullState(ctx, git, round, env, repo, head, index, status); err != nil {
			return fmt.Errorf("protected config %s: %w", setting[0], err)
		}
		afterRefs, e := gitCapture(ctx, git, round, repo, env, "for-each-ref", "--format=%(refname) %(objectname)")
		if e != nil {
			return e
		}
		if string(refs) != string(afterRefs) {
			return fmt.Errorf("protected config %s changed refs", setting[0])
		}
	}
	return nil
}

func assertPullState(ctx context.Context, git, round string, env []string, repo string, head []byte, index string, status []byte) error {
	afterHead, err := gitCapture(ctx, git, round, repo, env, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	afterStatus, err := gitCapture(ctx, git, round, repo, env, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	afterIndex, err := fileSHA(filepath.Join(repo, ".git", "index"))
	if err != nil {
		return err
	}
	if string(head) != string(afterHead) || index != afterIndex || string(status) != string(afterStatus) {
		return fmt.Errorf("HEAD/index/worktree status changed: head=%t index=%t status=%t before=%q after=%q", string(head) != string(afterHead), index != afterIndex, string(status) != string(afterStatus), string(status), string(afterStatus))
	}
	return nil
}

func waitForFile(ctx context.Context, path string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	tick := time.NewTicker(20 * time.Millisecond)
	defer tick.Stop()
	for {
		if exists(path) {
			return nil
		}
		if time.Now().After(deadline) {
			return errors.New("bounded wait expired")
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-tick.C:
		}
	}
}

func exists(path string) bool { _, err := os.Stat(path); return err == nil }

func gitCapture(ctx context.Context, git, round, cwd string, env []string, args ...string) ([]byte, error) {
	logDir, err := os.MkdirTemp(round, "git-capture-")
	if err != nil {
		return nil, err
	}
	child, err := ownedprocess.StartWithEnvironment(git, args, logDir, cwd, env)
	if err != nil {
		return nil, err
	}
	defer child.Close()
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		done, e := child.Exited(0)
		if e != nil {
			return nil, e
		}
		if done {
			code, e := child.ExitCode()
			if e != nil {
				return nil, e
			}
			out, _ := os.ReadFile(child.StdoutPath)
			if code != 0 {
				stderr, _ := os.ReadFile(child.StderrPath)
				return nil, fmt.Errorf("git %s exited %d: %s", strings.Join(args, " "), code, strings.TrimSpace(string(stderr)))
			}
			return out, nil
		}
		select {
		case <-ctx.Done():
			_ = child.Close()
			return nil, ctx.Err()
		case <-tick.C:
		}
	}
}

type daemonInstance struct {
	addr, token string
	child       *ownedprocess.Owned
}

func launch(ctx context.Context, round, binary string, fake *fakeGitConfig) (*daemonInstance, error) {
	token := uuid.NewString() + uuid.NewString()
	path := filepath.Join(filepath.Dir(fake.Log), "fake-bin")
	parentPath := os.Getenv("PATH")
	env := ownedprocess.BuildEnv(round, map[string]string{"FINTWIND_DAEMON_TOKEN": token, "PATH": path + string(os.PathListSeparator) + parentPath})
	logs := filepath.Join(round, "daemon-logs")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return nil, err
	}
	child, err := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", filepath.Join(round, "data"), "--bind", "127.0.0.1:0"}, logs, round, env)
	if err != nil {
		return nil, err
	}
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		raw, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
			PID     uint32 `json:"pid"`
		}
		if json.Unmarshal(raw, &ready) == nil && ready.PID == child.PID && ready.Address != "" {
			return &daemonInstance{ready.Address, token, child}, nil
		}
		if done, _ := child.Exited(0); done {
			_ = child.Close()
			return nil, errors.New("daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			_ = child.Close()
			return nil, ctx.Err()
		case <-tick.C:
		}
	}
}
func dial(addr, token string) (*websocket.Conn, error) {
	c, _, err := (&websocket.Dialer{HandshakeTimeout: 10 * time.Second}).Dial("ws://"+addr+"/v1", http.Header{})
	if err != nil {
		return nil, err
	}
	if err = c.WriteJSON(map[string]any{"type": "hello", "protocolVersion": daemon.ProtocolVersion, "token": token, "clientId": uuid.NewString(), "resumeFrom": []any{}}); err != nil {
		_ = c.Close()
		return nil, err
	}
	_ = c.SetReadDeadline(time.Now().Add(10 * time.Second))
	var frame map[string]any
	if err = c.ReadJSON(&frame); err != nil {
		_ = c.Close()
		return nil, err
	}
	if frame["type"] != "hello" {
		_ = c.Close()
		return nil, fmt.Errorf("daemon handshake rejected: %v", frame)
	}
	_ = c.SetReadDeadline(time.Time{})
	return c, nil
}
func execPath(name string) (string, error) { return exec.LookPath(name) }
func fileSHA(path string) (string, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:]), nil
}

func fileFingerprint(root string, relativePaths []string) (string, error) {
	h := sha256.New()
	for _, relative := range relativePaths {
		if !filepath.IsLocal(relative) {
			return "", fmt.Errorf("fixture fingerprint path escapes root: %q", relative)
		}
		content, err := os.ReadFile(filepath.Join(root, relative))
		if err != nil {
			return "", err
		}
		_, _ = h.Write([]byte(filepath.Clean(relative)))
		_, _ = h.Write([]byte{0})
		_, _ = h.Write(content)
		_, _ = h.Write([]byte{0})
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}
func gitRun(ctx context.Context, git, round, cwd string, env []string, args ...string) error {
	logDir, err := os.MkdirTemp(round, "git-run-")
	if err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(git, args, logDir, cwd, env)
	if err != nil {
		return err
	}
	defer child.Close()
	deadline := time.NewTicker(25 * time.Millisecond)
	defer deadline.Stop()
	for {
		done, e := child.Exited(0)
		if e != nil {
			return e
		}
		if done {
			code, e := child.ExitCode()
			if e != nil {
				return e
			}
			if code != 0 {
				b, _ := os.ReadFile(child.StderrPath)
				return fmt.Errorf("git %s exited %d: %s", strings.Join(args, " "), code, strings.TrimSpace(string(b)))
			}
			return nil
		}
		select {
		case <-ctx.Done():
			_ = child.Close()
			return ctx.Err()
		case <-deadline.C:
		}
	}
}

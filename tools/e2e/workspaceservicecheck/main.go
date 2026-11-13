package main

import (
	"bufio"
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
	"unicode/utf8"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
	_ "modernc.org/sqlite"
)

const fakeGithubToken = "fixture-gh-token-never-real"

type checkHarness struct {
	ctx       context.Context
	round     string
	self      string
	daemonBin string
	client    *daemon.Client
	owner     *ownedprocess.Owned
	git       string
	checks    map[string]bool
	metrics   map[string]any
	skipped   []string
	daemonSHA string
}

type openCodeRecord struct {
	Method    string          `json:"method"`
	Path      string          `json:"path"`
	Directory string          `json:"directory"`
	Body      json.RawMessage `json:"body,omitempty"`
}

type fakeConfig struct {
	CommandSequence  [][]map[string]any `json:"commandSequence"`
	SkillsMode       string             `json:"skillsMode"`
	Skills           []map[string]any   `json:"skills"`
	DefaultModelMode string             `json:"defaultModelMode"`
	DefaultModel     map[string]any     `json:"defaultModel"`
	GenerationText   string             `json:"generationText"`
	GenerationDelay  int                `json:"generationDelayMs"`
	GenerationError  bool               `json:"generationError"`
	GithubRows       []map[string]any   `json:"githubRows"`
	GithubExit       int                `json:"githubExit"`
	GithubStderr     string             `json:"githubStderr"`
	GithubDelay      int                `json:"githubDelayMs"`
}

func main() {
	if strings.EqualFold(filepath.Base(os.Args[0]), "gh.exe") || strings.EqualFold(filepath.Base(os.Args[0]), "gh") {
		os.Exit(runFakeGitHub(os.Args[1:]))
	}
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		if err := runFakeOpenCode(); err != nil {
			fmt.Fprintln(os.Stderr, "fake OpenCode:", err)
			os.Exit(1)
		}
		return
	}
	daemonBin := flag.String("daemon", "", "absolute isolated Go daemon")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Minute)
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
	self, err := os.Executable()
	if err != nil {
		return err
	}
	h := &checkHarness{ctx: ctx, round: round, self: self, daemonBin: daemonBin, checks: map[string]bool{}, metrics: map[string]any{}}
	defer func() {
		report := map[string]any{"passed": err == nil, "checks": h.checks, "metrics": h.metrics, "skipped": h.skipped, "round": round, "daemonSHA256": h.daemonSHA, "realProvider": false, "oauth": false, "externalNetwork": false}
		if err != nil {
			report["error"] = err.Error()
		}
		raw, marshalErr := json.MarshalIndent(report, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(round, "workspace-services-result.json"), raw, 0o600)
		}
		err = errors.Join(err, marshalErr)
		fmt.Println("[REPORT]", filepath.Join(round, "workspace-services-result.json"))
	}()
	if err := h.verifyChildEnvironment(); err != nil {
		return err
	}
	if h.git, err = exec.LookPath("git"); err != nil {
		return err
	}
	if err = h.prepareRound(); err != nil {
		return err
	}
	if err = h.startDaemon(); err != nil {
		return err
	}
	defer func() {
		if h.client != nil {
			h.client.Close()
		}
		if h.owner != nil {
			_ = h.owner.Close()
		}
	}()
	if err = h.testSlashCommands(); err != nil {
		return err
	}
	if err = h.testCommitMessage(); err != nil {
		return err
	}
	if err = h.testPullRequests(); err != nil {
		return err
	}
	h.skipped = append(h.skipped,
		"65-second slash registry total-budget boundary not waited out; stable-poll request sequence and config/skill routes were exercised",
		"PR 20-second timeout and 1 MiB/64 KiB output limits were not stress-triggered",
	)
	return nil
}

func (h *checkHarness) verifyChildEnvironment() error {
	for _, entry := range ownedprocess.BuildEnv(h.round, map[string]string{"FINTWIND_DAEMON_TOKEN": "fixture-token"}) {
		key, _, _ := strings.Cut(entry, "=")
		switch strings.ToUpper(key) {
		case "FINTWIND_AGENT_REAL_PROVIDER", "DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "GH_TOKEN", "GITHUB_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN":
			return fmt.Errorf("real credential crossed daemon child allowlist: %s", key)
		}
	}
	return nil
}

func (h *checkHarness) prepareRound() error {
	for _, name := range []string{"workspace", "home", "tmp", "fixture-bin", "logs", "home/.config/gh"} {
		if err := os.MkdirAll(filepath.Join(h.round, filepath.FromSlash(name)), 0o700); err != nil {
			return err
		}
	}
	if err := copyFile(h.self, filepath.Join(h.round, "fixture-bin", "gh.exe")); err != nil {
		return err
	}
	path := filepath.Join(h.round, "fixture-bin") + string(os.PathListSeparator) + os.Getenv("PATH")
	if err := os.Setenv("PATH", path); err != nil {
		return err
	}
	return h.writeConfig(fakeConfig{
		CommandSequence: [][]map[string]any{
			{{"name": "transient-only", "description": "not settled"}},
			{{"name": "local/description", "description": "reported description"}, {"name": "provider-only", "description": "live passthrough"}},
			{{"name": "local/description", "description": "reported description"}, {"name": "provider-only", "description": "live passthrough"}},
			{{"name": "local/description", "description": "reported description"}, {"name": "provider-only", "description": "live passthrough"}},
		},
		SkillsMode:       "list",
		Skills:           []map[string]any{{"id": "opencode/native-skill-id", "name": "native-skill", "description": "native skill"}},
		DefaultModelMode: "valid",
		DefaultModel:     map[string]any{"providerID": "fixture-default", "id": "model-default", "name": "Fixture default"},
		GenerationText:   "  feat: 修复 UTF-8 输出\r\nsecond line must not be returned  ",
		GithubRows:       fakePullRequestRows(),
	})
}

func (h *checkHarness) writeConfig(config fakeConfig) error {
	raw, err := json.Marshal(config)
	if err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(h.round, "home", "workspace-services-fixture.json"), raw, 0o600)
}

func (h *checkHarness) startDaemon() error {
	token := uuid.NewString()
	data := filepath.Join(h.round, "data")
	logs := filepath.Join(h.round, "logs", "daemon")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(h.daemonBin, []string{"--data-dir", data, "--bind", "127.0.0.1:0"}, logs, h.round, ownedprocess.BuildEnv(h.round, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(h.ctx, 20*time.Second)
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
			h.client, err = daemon.Dial(ctx, ready.Address, token)
			if err != nil {
				_ = child.Close()
				return err
			}
			h.owner = child
			h.daemonSHA, err = sha256File(h.daemonBin)
			if err != nil {
				h.client.Close()
				_ = child.Close()
				return err
			}
			h.checks["isolated-go-daemon-websocket"] = true
			return nil
		}
		if exited, _ := child.Exited(0); exited {
			_ = child.Close()
			return errors.New("isolated Go daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			_ = child.Close()
			return ctx.Err()
		case <-ticker.C:
		}
	}
}

func (h *checkHarness) rpc(operation map[string]any) (json.RawMessage, error) {
	return h.client.Request(h.ctx, map[string]any{"type": "workspace", "operation": operation})
}

func unwrapWorkspace(raw json.RawMessage) (json.RawMessage, error) {
	var outer map[string]json.RawMessage
	if err := json.Unmarshal(raw, &outer); err != nil {
		return nil, err
	}
	var reply map[string]json.RawMessage
	if value := outer["result"]; len(value) != 0 {
		if err := json.Unmarshal(value, &reply); err != nil {
			return nil, err
		}
	} else {
		reply = outer
	}
	if nested := reply["result"]; len(nested) != 0 {
		return nested, nil
	}
	if snapshot := reply["snapshot"]; len(snapshot) != 0 {
		return snapshot, nil
	}
	return json.Marshal(reply)
}

func (h *checkHarness) testSlashCommands() error {
	project := filepath.Join(h.round, "workspace", "slash")
	for _, dir := range []string{".fintwind/commands/local", ".agents/skills/fallback-skill"} {
		if err := os.MkdirAll(filepath.Join(project, filepath.FromSlash(dir)), 0o700); err != nil {
			return err
		}
	}
	if err := os.WriteFile(filepath.Join(project, ".fintwind", "commands", "local", "description.md"), []byte("---\ndescription: \n---\n# no description\n"), 0o600); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(project, ".fintwind", "commands", "local", "template.md"), []byte("---\ndescription: local template\nargument-hint: <topic>\n---\nKeep the local template $ARGUMENTS"), 0o600); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(project, ".agents", "skills", "fallback-skill", "SKILL.md"), []byte("---\ndescription: local fallback\n---\nlocal"), 0o600); err != nil {
		return err
	}
	local, err := h.rpc(map[string]any{"type": "discoverSlashCommands", "projectRoot": project})
	if err != nil {
		return fmt.Errorf("local-only slash discovery: %w", err)
	}
	localResult, err := unwrapWorkspace(local)
	if err != nil {
		return err
	}
	if err := verifyLocalSlashFixture(localResult); err != nil {
		return err
	}
	localRecords, err := readOpenCodeRecords(h.round)
	if err != nil {
		return err
	}
	if len(localRecords) != 0 {
		return errors.New("local-only slash discovery unexpectedly started OpenCode")
	}
	h.checks["slash-local-commands-fallback-and-builtins-without-server"] = true

	binary := h.self
	registry, err := h.rpc(map[string]any{"type": "discoverSlashCommands", "projectRoot": project, "binary": binary})
	if err != nil {
		return fmt.Errorf("live slash command discovery: %w", err)
	}
	result, err := unwrapWorkspace(registry)
	if err != nil {
		return err
	}
	commands, err := decodeSlashCommands(result)
	if err != nil {
		return err
	}
	if err := verifyLiveSlash(commands, "opencode/native-skill-id"); err != nil {
		return err
	}
	records, err := readOpenCodeRecords(h.round)
	if err != nil {
		return err
	}
	counts := map[string]int{}
	for _, record := range records {
		for _, route := range []string{"/api/config?location%5Bdirectory%5D=", "/api/command?location%5Bdirectory%5D=", "/api/skill?location%5Bdirectory%5D="} {
			if strings.HasPrefix(record.Path, route) {
				counts[route]++
				parsed, parseErr := url.Parse(record.Path)
				if parseErr != nil || parsed.Query().Get("location[directory]") != project {
					return fmt.Errorf("OpenCode request did not use the deepObject project location: %q", record.Path)
				}
			}
		}
	}
	if counts["/api/config?location%5Bdirectory%5D="] < 2 || counts["/api/command?location%5Bdirectory%5D="] < 4 || counts["/api/skill?location%5Bdirectory%5D="] != 1 {
		return fmt.Errorf("OpenCode registry request lifecycle mismatch: %+v", counts)
	}
	h.checks["slash-config-command-stabilization-and-authoritative-skills"] = true

	if err := h.writeConfig(fakeConfig{CommandSequence: [][]map[string]any{{{"name": "provider-only", "description": "live passthrough"}}}, SkillsMode: "empty"}); err != nil {
		return err
	}
	empty, err := h.rpc(map[string]any{"type": "discoverSlashCommands", "projectRoot": project, "binary": binary})
	if err != nil {
		return err
	}
	emptyResult, err := unwrapWorkspace(empty)
	if err != nil {
		return err
	}
	emptyCommands, err := decodeSlashCommands(emptyResult)
	if err != nil {
		return err
	}
	if hasSlash(emptyCommands, "fallback-skill") || hasSlash(emptyCommands, "opencode/native-skill-id") {
		return errors.New("successful empty OpenCode skill registry did not clear local fallback")
	}
	h.checks["slash-successful-empty-skills-clears-fallback"] = true

	if err := h.writeConfig(fakeConfig{CommandSequence: [][]map[string]any{{{"name": "provider-only", "description": "live passthrough"}}}, SkillsMode: "error"}); err != nil {
		return err
	}
	fallback, err := h.rpc(map[string]any{"type": "discoverSlashCommands", "projectRoot": project, "binary": binary})
	if err != nil {
		return err
	}
	fallbackResult, err := unwrapWorkspace(fallback)
	if err != nil {
		return err
	}
	fallbackCommands, err := decodeSlashCommands(fallbackResult)
	if err != nil {
		return err
	}
	if !hasSlash(fallbackCommands, "fallback-skill") {
		return errors.New("failed OpenCode skill request removed local fallback")
	}
	h.checks["slash-failed-skills-request-preserves-fallback"] = true
	return nil
}

type slashCommand struct {
	Name         string  `json:"name"`
	Description  string  `json:"description"`
	Scope        string  `json:"scope"`
	ArgumentHint *string `json:"argumentHint"`
	Template     *string `json:"template"`
}

func decodeSlashCommands(raw json.RawMessage) ([]slashCommand, error) {
	var result struct {
		Type     string         `json:"type"`
		Commands []slashCommand `json:"commands"`
	}
	if err := json.Unmarshal(raw, &result); err != nil {
		return nil, err
	}
	if result.Type != "slashCommands" {
		return nil, fmt.Errorf("unexpected slash result: %s", raw)
	}
	return result.Commands, nil
}

func verifyLocalSlashFixture(raw json.RawMessage) error {
	commands, err := decodeSlashCommands(raw)
	if err != nil {
		return err
	}
	if !hasSlash(commands, "local/template") || !hasSlash(commands, "fallback-skill") || !hasSlash(commands, "init") || !hasSlash(commands, "compact") {
		return errors.New("local slash command, skill fallback, or builtin is missing")
	}
	return nil
}

func verifyLiveSlash(commands []slashCommand, skillID string) error {
	var template, description, reported, skill, fallback *slashCommand
	for i := range commands {
		switch commands[i].Name {
		case "local/template":
			template = &commands[i]
		case "local/description":
			description = &commands[i]
		case "provider-only":
			reported = &commands[i]
		case skillID:
			skill = &commands[i]
		case "fallback-skill":
			fallback = &commands[i]
		}
	}
	if template == nil || template.Template == nil || !strings.Contains(*template.Template, "$ARGUMENTS") {
		return errors.New("local command template was not retained")
	}
	if description == nil || description.Description != "reported description" {
		return errors.New("live description did not fill blank local description")
	}
	if reported == nil || reported.Scope != "Builtin" || reported.Template != nil {
		return errors.New("reported-only command is not a passthrough builtin")
	}
	if skill == nil || skill.Scope != "Skill" || fallback != nil {
		return errors.New("successful native skill registry did not replace local skill fallback")
	}
	return nil
}

func hasSlash(commands []slashCommand, name string) bool {
	for _, command := range commands {
		if command.Name == name {
			return true
		}
	}
	return false
}

func (h *checkHarness) testCommitMessage() error {
	repo := filepath.Join(h.round, "workspace", "commit-message")
	if err := h.initRepo(repo, "commit-feature"); err != nil {
		return err
	}
	if err := h.writeCommitFixture(repo); err != nil {
		return err
	}
	sqliteBefore, err := h.sessionSQLiteCounts()
	if err != nil {
		return err
	}
	before, err := h.gitOutput(repo, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	head, err := h.gitOutput(repo, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	index, err := h.gitOutput(repo, "write-tree")
	if err != nil {
		return err
	}
	config, err := h.readConfig()
	if err != nil {
		return err
	}
	config.GenerationText = "  feat: 修复 UTF-8 输出\r\nsecond line must not be returned  " + strings.Repeat("界", 240)
	config.DefaultModelMode = "valid"
	config.DefaultModel = map[string]any{"providerID": "fixture-default", "id": "model-default", "name": "Fixture default"}
	if err := h.writeConfig(config); err != nil {
		return err
	}
	operation := map[string]any{"type": "generateCommitMessage", "cwd": repo, "includeUnstaged": true, "invocation": map[string]any{"binary": h.self, "model": "fixture/model-v2", "reasoning_effort": "high"}}
	resultRaw, err := h.rpc(operation)
	if err != nil {
		return fmt.Errorf("generateCommitMessage: %w", err)
	}
	result, err := unwrapWorkspace(resultRaw)
	if err != nil {
		return err
	}
	var message struct {
		Type    string `json:"type"`
		Message string `json:"message"`
	}
	if err := json.Unmarshal(result, &message); err != nil {
		return err
	}
	if message.Type != "commitMessage" || strings.ContainsAny(message.Message, "\r\n") || utf8.RuneCountInString(message.Message) > 200 || message.Message == "" {
		return fmt.Errorf("commit message was not normalized to a nonempty single line <=200 runes: %+v", message)
	}
	after, err := h.gitOutput(repo, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	afterHead, err := h.gitOutput(repo, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	afterIndex, err := h.gitOutput(repo, "write-tree")
	if err != nil {
		return err
	}
	if string(before) != string(after) || strings.TrimSpace(string(head)) != strings.TrimSpace(string(afterHead)) || strings.TrimSpace(string(index)) != strings.TrimSpace(string(afterIndex)) {
		return errors.New("commit message generation mutated repository status, HEAD, or index")
	}
	if err := os.WriteFile(filepath.Join(repo, "oversized.txt"), []byte(strings.Repeat("界", 40000)), 0o600); err != nil {
		return err
	}
	if err := h.runGit(repo, "add", "oversized.txt"); err != nil {
		return err
	}
	largeStatus, err := h.gitOutput(repo, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	largeHead, err := h.gitOutput(repo, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	largeIndex, err := h.gitOutput(repo, "write-tree")
	if err != nil {
		return err
	}
	largeRaw, err := h.rpc(map[string]any{"type": "generateCommitMessage", "cwd": repo, "includeUnstaged": false, "invocation": map[string]any{"binary": h.self, "model": "fixture/model-v2", "reasoning_effort": "high"}})
	if err != nil {
		return fmt.Errorf("bounded generation prompt: %w", err)
	}
	largeResult, err := unwrapWorkspace(largeRaw)
	if err != nil {
		return err
	}
	var largeMessage struct {
		Type    string `json:"type"`
		Message string `json:"message"`
	}
	if err := json.Unmarshal(largeResult, &largeMessage); err != nil || largeMessage.Type != "commitMessage" || largeMessage.Message == "" {
		return fmt.Errorf("bounded prompt generation returned invalid result: %s (%v)", largeResult, err)
	}
	if after, e := h.gitOutput(repo, "status", "--porcelain=v1", "-z"); e != nil || string(after) != string(largeStatus) {
		return fmt.Errorf("large generation mutated worktree/index status: %v", e)
	}
	if after, e := h.gitOutput(repo, "rev-parse", "HEAD"); e != nil || strings.TrimSpace(string(after)) != strings.TrimSpace(string(largeHead)) {
		return fmt.Errorf("large generation mutated HEAD: %v", e)
	}
	if after, e := h.gitOutput(repo, "write-tree"); e != nil || strings.TrimSpace(string(after)) != strings.TrimSpace(string(largeIndex)) {
		return fmt.Errorf("large generation mutated index: %v", e)
	}
	config, err = h.readConfig()
	if err != nil {
		return err
	}
	config.DefaultModelMode = "valid"
	if err := h.writeConfig(config); err != nil {
		return err
	}
	defaultRequest := map[string]any{"type": "generateCommitMessage", "cwd": repo, "includeUnstaged": false, "invocation": map[string]any{"binary": h.self, "reasoning_effort": "high"}}
	defaultRaw, err := h.rpc(defaultRequest)
	if err != nil {
		return fmt.Errorf("default-model generation: %w", err)
	}
	if defaultResult, e := unwrapWorkspace(defaultRaw); e != nil {
		return e
	} else {
		var generated struct {
			Type    string `json:"type"`
			Message string `json:"message"`
		}
		if json.Unmarshal(defaultResult, &generated) != nil || generated.Type != "commitMessage" || generated.Message == "" {
			return fmt.Errorf("default-model generation returned invalid result: %s", defaultResult)
		}
	}
	requestsBeforeFailures, err := readOpenCodeRecords(h.round)
	if err != nil {
		return err
	}
	postsBeforeFailures := countOpenCodeRequests(requestsBeforeFailures, "POST", "/api/experimental/generate/text")
	for _, scenario := range []struct{ name, mode string }{{"null", "null"}, {"error", "error"}} {
		config, err := h.readConfig()
		if err != nil {
			return err
		}
		config.DefaultModelMode = scenario.mode
		if err := h.writeConfig(config); err != nil {
			return err
		}
		if _, err := h.rpc(defaultRequest); err == nil {
			return fmt.Errorf("default model %s response unexpectedly allowed generation", scenario.name)
		}
		requests, err := readOpenCodeRecords(h.round)
		if err != nil {
			return err
		}
		if got := countOpenCodeRequests(requests, "POST", "/api/experimental/generate/text"); got != postsBeforeFailures {
			return fmt.Errorf("default model %s response issued generation POST: before=%d after=%d", scenario.name, postsBeforeFailures, got)
		}
	}
	failureRequest := map[string]any{"type": "generateCommitMessage", "cwd": repo, "includeUnstaged": false, "invocation": map[string]any{"binary": h.self, "model": "fixture/model-v2", "reasoning_effort": "high"}}
	config, err = h.readConfig()
	if err != nil {
		return err
	}
	config.GenerationError = true
	config.GenerationDelay = 0
	if err := h.writeConfig(config); err != nil {
		return err
	}
	if err := h.verifyGenerationFailure(repo, failureRequest, "generation-http-500"); err != nil {
		return err
	}
	config, err = h.readConfig()
	if err != nil {
		return err
	}
	config.GenerationError = false
	config.GenerationDelay = 185000
	if err := h.writeConfig(config); err != nil {
		return err
	}
	if err := h.verifyGenerationFailure(repo, failureRequest, "generation-server-180-second-deadline"); err != nil {
		return err
	}
	h.checks["commit-generation-180s-server-deadline-no-client-timeout"] = true
	sqliteAfter, err := h.sessionSQLiteCounts()
	if err != nil {
		return err
	}
	if sqliteBefore != sqliteAfter {
		return fmt.Errorf("commit generation mutated local session/history SQLite rows: before=%v after=%v", sqliteBefore, sqliteAfter)
	}
	requests, err := readOpenCodeRecords(h.round)
	if err != nil {
		return err
	}
	var generation []openCodeRecord
	defaultModelGets := 0
	for _, record := range requests {
		if record.Method == "GET" && record.Path == "/api/model/default" {
			defaultModelGets++
		}
		if record.Method == "POST" && record.Path == "/api/experimental/generate/text" {
			generation = append(generation, record)
		}
		if record.Method == "POST" && strings.HasPrefix(record.Path, "/api/session") {
			return errors.New("commit generation created or mutated a native session")
		}
	}
	if len(generation) != 5 {
		return fmt.Errorf("expected exactly one experimental generation POST for each of five generation attempts, got %d", len(generation))
	}
	if defaultModelGets != 3 {
		return fmt.Errorf("expected default model GET for success, null and error cases, got %d", defaultModelGets)
	}
	var body struct {
		Prompt string `json:"prompt"`
		Model  struct {
			Provider string `json:"providerID"`
			ID       string `json:"id"`
			Variant  string `json:"variant"`
		} `json:"model"`
	}
	if err := json.Unmarshal(generation[0].Body, &body); err != nil {
		return err
	}
	if body.Model.Provider != "fixture" || body.Model.ID != "model-v2" || body.Model.Variant != "high" || !utf8.ValidString(body.Prompt) || len(body.Prompt) > 96<<10+2048 || !strings.Contains(body.Prompt, "STAGED-SENTINEL") || !strings.Contains(body.Prompt, "UNSTAGED-SENTINEL") {
		return fmt.Errorf("generation request did not carry bounded UTF-8 staged/unstaged context and selected model: model=%+v promptBytes=%d", body.Model, len(body.Prompt))
	}
	var bounded struct {
		Prompt string `json:"prompt"`
		Model  struct {
			Provider string `json:"providerID"`
			ID       string `json:"id"`
			Variant  string `json:"variant"`
		} `json:"model"`
	}
	if err := json.Unmarshal(generation[1].Body, &bounded); err != nil {
		return err
	}
	if !utf8.ValidString(bounded.Prompt) || len(bounded.Prompt) > 96<<10+2048 || !strings.Contains(bounded.Prompt, "[diff truncated]") || bounded.Model.Provider != "fixture" || bounded.Model.ID != "model-v2" || bounded.Model.Variant != "high" {
		return fmt.Errorf("large generation prompt was not UTF-8 bounded/truncated or lost model selection: bytes=%d model=%+v", len(bounded.Prompt), bounded.Model)
	}
	var defaultBody struct {
		Model struct {
			Provider string `json:"providerID"`
			ID       string `json:"id"`
			Variant  string `json:"variant"`
		} `json:"model"`
	}
	if err := json.Unmarshal(generation[2].Body, &defaultBody); err != nil {
		return err
	}
	if defaultBody.Model.Provider != "fixture-default" || defaultBody.Model.ID != "model-default" || defaultBody.Model.Variant != "high" {
		return fmt.Errorf("default model or reasoning variant was not carried into generation request: %+v", defaultBody.Model)
	}
	h.checks["commit-generation-one-post-per-request-model-read-only"] = true
	h.checks["commit-prompt-96k-utf8-truncation"] = true
	h.checks["commit-generation-default-model-with-reasoning-variant"] = true
	h.checks["commit-generation-default-null-no-post"] = true
	h.checks["commit-generation-default-error-no-post"] = true
	h.checks["commit-generation-http-500-single-post-no-session-mutation"] = true
	h.checks["commit-generation-server-deadline-single-post-no-session-mutation"] = true
	return nil
}

func (h *checkHarness) verifyGenerationFailure(repo string, request map[string]any, name string) error {
	beforeRecords, err := readOpenCodeRecords(h.round)
	if err != nil {
		return err
	}
	postsBefore := countOpenCodeRequests(beforeRecords, "POST", "/api/experimental/generate/text")
	sqliteBefore, err := h.sessionSQLiteCounts()
	if err != nil {
		return err
	}
	if sqliteBefore != [3]int64{} {
		return fmt.Errorf("generation failure precondition requires empty isolated session/history tables: %v", sqliteBefore)
	}
	statusBefore, err := h.gitOutput(repo, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	headBefore, err := h.gitOutput(repo, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	indexBefore, err := h.gitOutput(repo, "write-tree")
	if err != nil {
		return err
	}
	started := time.Now()
	_, rpcErr := h.rpc(request)
	elapsed := time.Since(started)
	h.metrics[name+"RpcElapsed"] = elapsed.Round(time.Second).String()
	if rpcErr == nil {
		return fmt.Errorf("%s unexpectedly succeeded", name)
	}
	if name == "generation-server-180-second-deadline" && elapsed < 175*time.Second {
		return fmt.Errorf("server-deadline scenario returned too early (%s); this must exercise the 180-second server deadline, not a shorter client timeout", elapsed.Round(time.Second))
	}
	afterRecords, err := readOpenCodeRecords(h.round)
	if err != nil {
		return err
	}
	postsAfter := countOpenCodeRequests(afterRecords, "POST", "/api/experimental/generate/text")
	if postsAfter != postsBefore+1 {
		return fmt.Errorf("%s must issue exactly one generation POST: before=%d after=%d", name, postsBefore, postsAfter)
	}
	for _, record := range afterRecords {
		if record.Method == "POST" && strings.HasPrefix(record.Path, "/api/session") {
			return fmt.Errorf("%s created or mutated an OpenCode session at %s", name, record.Path)
		}
	}
	sqliteAfter, err := h.sessionSQLiteCounts()
	if err != nil {
		return err
	}
	if sqliteAfter != sqliteBefore {
		return fmt.Errorf("%s mutated isolated session/history SQLite rows: before=%v after=%v", name, sqliteBefore, sqliteAfter)
	}
	statusAfter, err := h.gitOutput(repo, "status", "--porcelain=v1", "-z")
	if err != nil {
		return err
	}
	headAfter, err := h.gitOutput(repo, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	indexAfter, err := h.gitOutput(repo, "write-tree")
	if err != nil {
		return err
	}
	if string(statusAfter) != string(statusBefore) || strings.TrimSpace(string(headAfter)) != strings.TrimSpace(string(headBefore)) || strings.TrimSpace(string(indexAfter)) != strings.TrimSpace(string(indexBefore)) {
		return fmt.Errorf("%s mutated Git worktree, HEAD, or index", name)
	}
	h.checks["commit-"+name+"-one-post-no-session-or-git-mutation"] = true
	return nil
}

func (h *checkHarness) writeCommitFixture(repo string) error {
	base := filepath.Join(repo, "change.txt")
	if err := os.WriteFile(base, []byte("base\n"), 0o600); err != nil {
		return err
	}
	if err := h.runGit(repo, "add", "change.txt"); err != nil {
		return err
	}
	if err := h.runGit(repo, "commit", "-q", "-m", "base"); err != nil {
		return err
	}
	if err := os.WriteFile(base, []byte("base\nSTAGED-SENTINEL\n"+strings.Repeat("界", 12000)+"\n"), 0o600); err != nil {
		return err
	}
	if err := h.runGit(repo, "add", "change.txt"); err != nil {
		return err
	}
	if err := os.WriteFile(base, []byte("base\nSTAGED-SENTINEL\n"+strings.Repeat("界", 12000)+"\nUNSTAGED-SENTINEL\n"), 0o600); err != nil {
		return err
	}
	return nil
}

func (h *checkHarness) testPullRequests() error {
	repo := filepath.Join(h.round, "workspace", "pull-requests")
	if err := h.initRepo(repo, "feature/pr-fixture"); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "README.md"), []byte("fixture"), 0o600); err != nil {
		return err
	}
	if err := h.runGit(repo, "add", "README.md"); err != nil {
		return err
	}
	if err := h.runGit(repo, "commit", "-q", "-m", "fixture"); err != nil {
		return err
	}
	if err := h.runGit(repo, "remote", "add", "origin", "https://github.com/fixture-owner/fixture-repo.git"); err != nil {
		return err
	}
	if err := os.Remove(filepath.Join(h.round, "fake-gh-record.json")); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	noToken, err := h.rpc(map[string]any{"type": "inspectPullRequests", "cwd": repo})
	if err != nil {
		return fmt.Errorf("inspectPullRequests without profile token: %w", err)
	}
	noTokenResult, err := unwrapWorkspace(noToken)
	if err != nil {
		return err
	}
	var unavailable struct {
		Type   string `json:"type"`
		Reason string `json:"reason"`
	}
	if err := json.Unmarshal(noTokenResult, &unavailable); err != nil {
		return err
	}
	if unavailable.Type != "unavailable" || unavailable.Reason != "authenticationRequired" {
		return fmt.Errorf("profile token absence had wrong response: %s", noTokenResult)
	}
	if _, err := os.Stat(filepath.Join(h.round, "fake-gh-record.json")); err == nil {
		return errors.New("fake gh started without explicit profile token")
	}
	h.checks["pull-requests-no-token-does-not-start-gh"] = true
	otherHost := "github.enterprise.example:\n    oauth_token: '" + fakeGithubToken + "'\n"
	if err := os.WriteFile(filepath.Join(h.round, "home", ".config", "gh", "hosts.yml"), []byte(otherHost), 0o600); err != nil {
		return err
	}
	otherToken, err := h.rpc(map[string]any{"type": "inspectPullRequests", "cwd": repo})
	if err != nil {
		return err
	}
	otherTokenResult, err := unwrapWorkspace(otherToken)
	if err != nil {
		return err
	}
	if err := json.Unmarshal(otherTokenResult, &unavailable); err != nil || unavailable.Type != "unavailable" || unavailable.Reason != "authenticationRequired" {
		return fmt.Errorf("non-GitHub profile token was accepted: %s (%v)", otherTokenResult, err)
	}
	if records, err := readFakeGithubRecords(h.round); err != nil || len(records) != 0 {
		return fmt.Errorf("non-GitHub token caused fake gh launch: records=%d err=%v", len(records), err)
	}
	h.checks["pull-requests-ignores-other-host-token"] = true

	hosts := "github.com:\n    oauth_token: '" + fakeGithubToken + "'\n"
	config, err := h.readConfig()
	if err != nil {
		return err
	}
	config.GithubRows = fakePullRequestRows()
	if err := h.writeConfig(config); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(h.round, "home", ".config", "gh", "hosts.yml"), []byte(hosts), 0o600); err != nil {
		return err
	}
	ready, err := h.rpc(map[string]any{"type": "inspectPullRequests", "cwd": repo})
	if err != nil {
		return fmt.Errorf("inspectPullRequests with profile-only fixture token: %w", err)
	}
	readyResult, err := unwrapWorkspace(ready)
	if err != nil {
		return err
	}
	if err := verifyPullRequestCards(readyResult); err != nil {
		return err
	}
	ghRecords, err := readFakeGithubRecords(h.round)
	if err != nil {
		return err
	}
	if len(ghRecords) != 1 {
		return fmt.Errorf("expected one fake gh invocation so far, got %d", len(ghRecords))
	}
	if err := verifyGithubInvocation(ghRecords[0], repo, filepath.Join(h.round, "fixture-bin")); err != nil {
		return err
	}
	h.checks["pull-request-owned-gh-argv-token-clean-home-and-cards"] = true

	for name, mutate := range map[string]func(map[string]any){
		"unexpected-url":      func(row map[string]any) { row["url"] = "https://evil.invalid/pull/1" },
		"missing-title":       func(row map[string]any) { row["title"] = nil },
		"missing-url":         func(row map[string]any) { row["url"] = nil },
		"missing-state":       func(row map[string]any) { row["state"] = nil },
		"missing-head-branch": func(row map[string]any) { row["headRefName"] = nil },
		"missing-base-branch": func(row map[string]any) { row["baseRefName"] = nil },
		"missing-updated-at":  func(row map[string]any) { row["updatedAt"] = nil },
		"missing-draft-flag":  func(row map[string]any) { row["isDraft"] = nil },
		"bad-author":          func(row map[string]any) { row["author"] = map[string]any{"name": "no-login"} },
		"bad-author-login":    func(row map[string]any) { row["author"] = map[string]any{"login": 5} },
		"null-comments":       func(row map[string]any) { row["comments"] = nil },
		"bad-check-row":       func(row map[string]any) { row["statusCheckRollup"] = []any{map[string]any{"state": 42}} },
		"bad-count":           func(row map[string]any) { row["additions"] = "many" },
		"negative-count":      func(row map[string]any) { row["deletions"] = -1 },
		"missing-file-count":  func(row map[string]any) { delete(row, "changedFiles") },
	} {
		config, err := h.readConfig()
		if err != nil {
			return err
		}
		config.GithubRows = fakePullRequestRows()
		mutate(config.GithubRows[0])
		if err := h.writeConfig(config); err != nil {
			return err
		}
		bad, err := h.rpc(map[string]any{"type": "inspectPullRequests", "cwd": repo})
		if err == nil {
			return fmt.Errorf("malformed GitHub row %s produced a successful reply: %s", name, bad)
		}
		h.checks["pull-request-malformed-"+name+"-rejects-whole-response"] = true
	}
	return nil
}

func verifyPullRequestCards(raw json.RawMessage) error {
	var snapshot struct {
		Type       string `json:"type"`
		Repository string `json:"repository"`
		Branch     string `json:"branch"`
		Requests   []struct {
			Number       int    `json:"number"`
			Title        string `json:"title"`
			URL          string `json:"url"`
			State        string `json:"state"`
			HeadBranch   string `json:"headBranch"`
			BaseBranch   string `json:"baseBranch"`
			Author       string `json:"author"`
			Additions    int    `json:"additions"`
			Deletions    int    `json:"deletions"`
			ChangedFiles int    `json:"changedFiles"`
			Comments     int    `json:"comments"`
			UpdatedAt    string `json:"updatedAt"`
			Checks       string `json:"checks"`
		} `json:"requests"`
	}
	if err := json.Unmarshal(raw, &snapshot); err != nil {
		return err
	}
	if snapshot.Type != "ready" || snapshot.Repository != "fixture-owner/fixture-repo" || snapshot.Branch != "feature/pr-fixture" || len(snapshot.Requests) != 4 {
		return fmt.Errorf("unexpected PR snapshot metadata: %s", raw)
	}
	wantStates := []string{"open", "draft", "merged", "closed"}
	wantChecks := []string{"none", "passed", "pending", "failed"}
	for i, card := range snapshot.Requests {
		wantAuthor := fmt.Sprintf("author-%d", i+1)
		if i == 3 {
			wantAuthor = ""
		}
		if card.Number != i+1 || card.Title != fmt.Sprintf("Fixture PR %d", i+1) || card.State != wantStates[i] || card.Checks != wantChecks[i] || card.URL != fmt.Sprintf("https://github.com/fixture-owner/fixture-repo/pull/%d", i+1) || card.HeadBranch != "feature/pr-fixture" || card.BaseBranch != "main" || card.Author != wantAuthor || card.Additions != i+2 || card.Deletions != i+3 || card.ChangedFiles != i+4 || card.Comments != i || card.UpdatedAt == "" {
			return fmt.Errorf("PR card %d mismatched Rust shape/status: %+v", i, card)
		}
	}
	return nil
}

type fakeGithubRecord struct {
	Args                  []string `json:"args"`
	Cwd                   string   `json:"cwd"`
	GHTokenMatchesFixture bool     `json:"ghTokenMatchesFixture"`
	GHConfigDir           string   `json:"ghConfigDir"`
	HomeIsIsolated        bool     `json:"homeIsIsolated"`
	ForbiddenCredential   bool     `json:"forbiddenCredential"`
	EnvKeys               []string `json:"envKeys"`
}

func verifyGithubInvocation(record fakeGithubRecord, repo, fakeBin string) error {
	want := []string{"pr", "list", "--repo", "fixture-owner/fixture-repo", "--head", "feature/pr-fixture", "--state", "all", "--limit", "8", "--json", "number,title,state,isDraft,url,headRefName,baseRefName,author,additions,deletions,changedFiles,updatedAt,comments,statusCheckRollup"}
	if strings.Join(record.Args, "\x00") != strings.Join(want, "\x00") || !samePath(record.Cwd, repo) || !record.GHTokenMatchesFixture || !record.HomeIsIsolated || record.ForbiddenCredential || !strings.HasPrefix(strings.ToLower(filepath.Clean(record.GHConfigDir)), strings.ToLower(filepath.Dir(fakeBin))) {
		return fmt.Errorf("fake gh command contract mismatch: args=%q cwd=%q tokenFixture=%v cleanHome=%v forbiddenEnv=%v configDir=%q", record.Args, record.Cwd, record.GHTokenMatchesFixture, record.HomeIsIsolated, record.ForbiddenCredential, record.GHConfigDir)
	}
	for _, key := range record.EnvKeys {
		if strings.EqualFold(key, "GITHUB_TOKEN") || strings.EqualFold(key, "GH_ENTERPRISE_TOKEN") || strings.EqualFold(key, "DEEPSEEK_API_KEY") || strings.EqualFold(key, "ANTHROPIC_API_KEY") {
			return fmt.Errorf("fake gh received forbidden environment key %s", key)
		}
	}
	return nil
}

func (h *checkHarness) readConfig() (fakeConfig, error) {
	raw, err := os.ReadFile(filepath.Join(h.round, "home", "workspace-services-fixture.json"))
	if err != nil {
		return fakeConfig{}, err
	}
	var config fakeConfig
	err = json.Unmarshal(raw, &config)
	return config, err
}

func (h *checkHarness) initRepo(repo, branch string) error {
	if err := os.MkdirAll(repo, 0o700); err != nil {
		return err
	}
	for _, args := range [][]string{{"init", "-q"}, {"config", "user.email", "workspace-services@example.invalid"}, {"config", "user.name", "Workspace Services E2E"}, {"branch", "-M", branch}} {
		if err := h.runGit(repo, args...); err != nil {
			return err
		}
	}
	return nil
}

func (h *checkHarness) runGit(directory string, args ...string) error {
	logRoot := filepath.Join(h.round, "logs", "git")
	if err := os.MkdirAll(logRoot, 0o700); err != nil {
		return err
	}
	logDir, err := os.MkdirTemp(logRoot, "git-")
	if err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(h.git, args, logDir, directory, ownedprocess.BuildEnv(h.round, nil))
	if err != nil {
		return err
	}
	done, waitErr := child.Exited(15000)
	if waitErr != nil || !done {
		_ = child.Close()
		return fmt.Errorf("owned git %s timed out: %v", strings.Join(args, " "), waitErr)
	}
	code, codeErr := child.ExitCode()
	stdout, _ := os.ReadFile(child.StdoutPath)
	stderr, _ := os.ReadFile(child.StderrPath)
	closeErr := child.Close()
	if codeErr != nil || code != 0 || closeErr != nil {
		return fmt.Errorf("owned git %s failed: %s %s", strings.Join(args, " "), strings.TrimSpace(string(stdout)), strings.TrimSpace(string(stderr)))
	}
	return nil
}

func (h *checkHarness) gitOutput(directory string, args ...string) ([]byte, error) {
	logRoot := filepath.Join(h.round, "logs", "git")
	if err := os.MkdirAll(logRoot, 0o700); err != nil {
		return nil, err
	}
	logDir, err := os.MkdirTemp(logRoot, "git-output-")
	if err != nil {
		return nil, err
	}
	child, err := ownedprocess.StartWithEnvironment(h.git, args, logDir, directory, ownedprocess.BuildEnv(h.round, nil))
	if err != nil {
		return nil, err
	}
	done, waitErr := child.Exited(15000)
	if waitErr != nil || !done {
		_ = child.Close()
		return nil, fmt.Errorf("owned git query %s timed out: %v", strings.Join(args, " "), waitErr)
	}
	code, codeErr := child.ExitCode()
	stdout, _ := os.ReadFile(child.StdoutPath)
	stderr, _ := os.ReadFile(child.StderrPath)
	closeErr := child.Close()
	if codeErr != nil || code != 0 || closeErr != nil {
		return nil, fmt.Errorf("owned git query %s failed: %s", strings.Join(args, " "), strings.TrimSpace(string(stderr)))
	}
	return stdout, nil
}

func readOpenCodeRecords(round string) ([]openCodeRecord, error) {
	file, err := os.Open(filepath.Join(round, "home", "fake-opencode-requests.jsonl"))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	defer file.Close()
	var records []openCodeRecord
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 4096), 2<<20)
	for scanner.Scan() {
		var record openCodeRecord
		if err := json.Unmarshal(scanner.Bytes(), &record); err != nil {
			return nil, err
		}
		records = append(records, record)
	}
	return records, scanner.Err()
}

func countOpenCodeRequests(records []openCodeRecord, method, path string) int {
	count := 0
	for _, record := range records {
		if record.Method == method && record.Path == path {
			count++
		}
	}
	return count
}

func (h *checkHarness) sessionSQLiteCounts() ([3]int64, error) {
	var counts [3]int64
	databasePath := filepath.Join(h.round, "data", "app.db")
	if _, err := os.Stat(databasePath); err != nil {
		return counts, err
	}
	database, err := sql.Open("sqlite", "file:"+filepath.ToSlash(databasePath)+"?mode=ro")
	if err != nil {
		return counts, err
	}
	defer database.Close()
	for i, table := range []string{"sessions", "messages", "session_details"} {
		if err := database.QueryRowContext(h.ctx, "SELECT count(*) FROM "+table).Scan(&counts[i]); err != nil {
			return counts, err
		}
	}
	return counts, nil
}

func copyFile(source, target string) error {
	in, err := os.Open(source)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(target, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0o700)
	if err != nil {
		return err
	}
	if _, err = io.Copy(out, in); err != nil {
		_ = out.Close()
		return err
	}
	return out.Close()
}

func samePath(a, b string) bool {
	left, _ := filepath.Abs(a)
	right, _ := filepath.Abs(b)
	return strings.EqualFold(filepath.Clean(left), filepath.Clean(right))
}

func sha256File(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	hash := sha256.New()
	if _, err := io.Copy(hash, file); err != nil {
		return "", err
	}
	return hex.EncodeToString(hash.Sum(nil)), nil
}

func fakePullRequestRows() []map[string]any {
	states := []string{"OPEN", "OPEN", "MERGED", "CLOSED"}
	checks := [][]map[string]any{nil, {{"state": "SUCCESS"}}, {{"status": "IN_PROGRESS"}}, {{"conclusion": "FAILURE"}}}
	rows := make([]map[string]any, 4)
	for i := range rows {
		var author any = map[string]any{"login": fmt.Sprintf("author-%d", i+1)}
		if i == 3 {
			author = nil
		}
		comments := make([]any, i)
		for j := range comments {
			comments[j] = map[string]any{"body": fmt.Sprintf("comment-%d", j+1)}
		}
		rows[i] = map[string]any{
			"number": i + 1, "title": fmt.Sprintf("Fixture PR %d", i+1), "state": states[i], "isDraft": i == 1,
			"url": fmt.Sprintf("https://github.com/fixture-owner/fixture-repo/pull/%d", i+1), "headRefName": "feature/pr-fixture", "baseRefName": "main",
			"author": author, "additions": i + 2, "deletions": i + 3,
			"changedFiles": i + 4, "updatedAt": "2026-11-12T10:00:00Z", "comments": comments, "statusCheckRollup": checks[i],
		}
	}
	return rows
}

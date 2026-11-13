package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

type result struct {
	DaemonSHA256 string          `json:"daemonSha256"`
	Checks       map[string]bool `json:"checks"`
	Passed       bool            `json:"passed"`
	Error        string          `json:"error,omitempty"`
	UserData     bool            `json:"realUserDataOpened"`
}

func main() {
	if len(os.Args) == 4 && os.Args[1] == "auth" && os.Args[2] == "status" && os.Args[3] == "--json" {
		fmt.Println(`{"loggedIn":false}`)
		return
	}
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		if err := serveGenerationFixture(); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		return
	}
	if len(os.Args) == 2 && os.Args[1] == "--version" {
		fmt.Println("OpenCode v2.0.99")
		return
	}
	binary := flag.String("daemon", "", "absolute Go daemon binary")
	exporter := flag.String("exporter", "", "absolute sanitized exporter binary")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	if err := run(ctx, *binary, *exporter); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, binary, exporter string) (err error) {
	if !filepath.IsAbs(binary) {
		return errors.New("absolute daemon path required")
	}
	bytes, err := os.ReadFile(binary)
	if err != nil {
		return errors.New("daemon binary is not readable")
	}
	sum := sha256.Sum256(bytes)
	report := result{DaemonSHA256: hex.EncodeToString(sum[:]), Checks: map[string]bool{}}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	defer func() {
		if err != nil {
			report.Error = err.Error()
		}
		report.Passed = err == nil
		data, marshalErr := json.MarshalIndent(report, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(round, "result.json"), data, 0o600)
		}
		if marshalErr != nil {
			err = errors.Join(err, errors.New("could not save verification artifact"))
		}
		fmt.Println("[REPORT]", filepath.Join(round, "result.json"))
	}()
	dataDir := filepath.Join(round, "data")
	fixtureBin := filepath.Join(round, "fixture-bin")
	if err := os.MkdirAll(fixtureBin, 0o700); err != nil {
		return err
	}
	fixtureExecutable, err := os.Executable()
	if err != nil {
		return err
	}
	fixtureBytes, err := os.ReadFile(fixtureExecutable)
	if err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(fixtureBin, "claude.exe"), fixtureBytes, 0o700); err != nil {
		return err
	}
	token := uuid.NewString()
	start := func(name string) (*ownedprocess.Owned, string, error) {
		logs := filepath.Join(round, name)
		if err := os.MkdirAll(logs, 0o700); err != nil {
			return nil, "", err
		}
		child, err := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", dataDir, "--bind", "127.0.0.1:0"}, logs, round, ownedprocess.BuildEnv(round, map[string]string{"FINTWIND_DAEMON_TOKEN": token, "PATH": fixtureBin + string(os.PathListSeparator) + os.Getenv("PATH")}))
		if err != nil {
			return nil, "", err
		}
		address, err := waitReady(ctx, child)
		if err != nil {
			_ = child.Close()
			return nil, "", err
		}
		return child, address, nil
	}
	child, address, err := start("first")
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, child.Close()) }()
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()
	report.Checks["handshake"] = true
	self, err := os.Executable()
	if err != nil {
		return err
	}
	for _, scenario := range []struct {
		binary    string
		installed bool
		version   string
	}{{filepath.Join(round, "missing-opencode.exe"), false, ""}, {self, true, "2.0.99"}} {
		raw, err := client.Request(ctx, map[string]any{"type": "probeProvider", "binaryOverride": scenario.binary, "discoverModels": false, "probeVersion": true})
		if err != nil {
			return err
		}
		var probe struct {
			Probe struct {
				Installed bool    `json:"installed"`
				Path      *string `json:"path"`
			} `json:"probe"`
			Version *string `json:"version"`
		}
		if err := json.Unmarshal(raw, &probe); err != nil {
			return err
		}
		if probe.Probe.Installed != scenario.installed {
			return errors.New("local binary probe installed mismatch")
		}
		if scenario.installed && (probe.Probe.Path == nil || probe.Version == nil || *probe.Version != scenario.version) {
			return errors.New("local version probe mismatch")
		}
		if !scenario.installed && (probe.Probe.Path != nil || probe.Version != nil) {
			return errors.New("missing binary fabricated version")
		}
	}
	report.Checks["local-provider-probe-without-engine"] = true
	bad, badErr := daemon.Dial(ctx, address, "wrong-token")
	if bad != nil {
		bad.Close()
	}
	if badErr == nil {
		return errors.New("wrong token was accepted")
	}
	report.Checks["reject-wrong-token"] = true
	if _, err := client.Request(ctx, map[string]any{"type": "nonexistentCommand"}); err == nil {
		return errors.New("unknown command reported success")
	}
	report.Checks["unknown-command-fails"] = true
	if err := checkNegativeStartup(ctx, binary); err != nil {
		return fmt.Errorf("negative startup: %w", err)
	}
	report.Checks["negative-startup-refused"] = true
	if err := checkProtocolMismatch(ctx, address, token); err != nil {
		return fmt.Errorf("protocol mismatch: %w", err)
	}
	report.Checks["protocol-mismatch-refused"] = true
	if err := checkSettings(ctx, client); err != nil {
		return err
	}
	report.Checks["settings-and-draft-generation"] = true
	if err := checkSkills(ctx, client, round); err != nil {
		return err
	}
	report.Checks["skills-real-daemon-catalog-toggle"] = true
	for _, provider := range []string{"unsupported-fixture", "anthropic", "openai"} {
		raw, err := client.Request(ctx, map[string]any{"type": "fetchProviderQuota", "providerId": provider, "binary": "must-not-start", "directory": "must-not-read"})
		if err != nil {
			return err
		}
		var response struct {
			Type  string `json:"type"`
			Quota struct {
				Status   string `json:"status"`
				Windows  []any  `json:"windows"`
				Balances []any  `json:"balances"`
			} `json:"quota"`
		}
		if err := json.Unmarshal(raw, &response); err != nil {
			return err
		}
		want := "unsupported"
		if provider == "anthropic" {
			want = "no_credential"
		}
		if provider == "openai" {
			want = "no_credential"
		}
		if response.Type != "providerQuota" || response.Quota.Status != want || len(response.Quota.Windows) != 0 || len(response.Quota.Balances) != 0 {
			return errors.New("quota local/unsupported boundary mismatch")
		}
	}
	report.Checks["quota-local-no-server-fallback"] = true
	planRaw, err := client.Request(ctx, map[string]any{"type": "fetchPlanUsage"})
	if err != nil {
		return err
	}
	var plan struct {
		Type  string          `json:"type"`
		Usage json.RawMessage `json:"usage"`
	}
	if err := json.Unmarshal(planRaw, &plan); err != nil {
		return err
	}
	if plan.Type != "planUsage" || string(plan.Usage) != "null" {
		return errors.New("missing plan credential contract mismatch")
	}
	report.Checks["plan-usage-no-credential-null"] = true
	projectOperationID, err := checkProjectOperation(ctx, client, round)
	if err != nil {
		return err
	}
	report.Checks["project-journal-idempotency-no-adoption"] = true
	if err := checkReview(ctx, client, round); err != nil {
		return err
	}
	report.Checks["review-real-daemon-diff-cancel"] = true
	if err := checkProjectless(ctx, client, round); err != nil {
		return err
	}
	report.Checks["projectless-shared-root-legacy-migration"] = true
	if err := checkGenerationDaemon(ctx, client, round); err != nil {
		return err
	}
	report.Checks["generation-real-daemon-same-connection-cancel-and-long-answer"] = true
	projectID, sessionID, messageID := uuid.NewString(), uuid.NewString(), uuid.NewString()
	project := map[string]any{"id": projectID, "name": "isolated fixture", "path": filepath.Join(round, "workspace"), "created_at": 1}
	session := map[string]any{
		"id": sessionID, "project_id": projectID, "title": "fixture", "provider": "openCode", "status": "idle", "created_at": 1, "updated_at": 2,
		"messages": []any{map[string]any{"id": messageID, "role": "user", "content": "literal test content", "attachments": []any{}, "created_at": 1, "streaming": false}},
		"turns":    []any{}, "transcript_blocks": []any{}, "future_extension": map[string]any{"mustSurvive": "literal"},
	}
	command := map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{project}, "sessions": []any{session}, "skeletonSessionIds": []any{}}
	if _, err := client.Request(ctx, command); err != nil {
		return fmt.Errorf("save fixture: %w", err)
	}
	report.Checks["save-snapshot"] = true
	scheduledID, err := checkScheduled(ctx, client, sessionID)
	if err != nil {
		return err
	}
	report.Checks["scheduled-crud-cas"] = true
	checkHydrate := func(c *daemon.Client) error {
		raw, err := c.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sessionID})
		if err != nil {
			return err
		}
		var got struct {
			Type    string `json:"type"`
			Session struct {
				Messages []struct {
					Content string `json:"content"`
				} `json:"messages"`
				Future struct {
					Value string `json:"mustSurvive"`
				} `json:"future_extension"`
			} `json:"session"`
		}
		if err := json.Unmarshal(raw, &got); err != nil {
			return err
		}
		if got.Type != "session" || len(got.Session.Messages) != 1 || got.Session.Messages[0].Content != "literal test content" || got.Session.Future.Value != "literal" {
			return errors.New("hydration lost content or unknown fields")
		}
		return nil
	}
	if err := checkHydrate(client); err != nil {
		return err
	}
	report.Checks["hydrate-and-unknown-fields"] = true
	var list struct {
		Sessions []map[string]json.RawMessage `json:"sessions"`
	}
	raw, err := client.Request(ctx, map[string]any{"type": "loadTaskState"})
	if err != nil {
		return err
	}
	if err := json.Unmarshal(raw, &list); err != nil {
		return err
	}
	if len(list.Sessions) != 1 || string(list.Sessions[0]["messages"]) != "[]" {
		return errors.New("startup must load list projection, not transcripts")
	}
	report.Checks["lazy-startup"] = true
	second, _, secondErr := start("lock-conflict")
	if second != nil {
		_ = second.Close()
	}
	if secondErr == nil {
		return errors.New("second daemon acquired the data directory")
	}
	report.Checks["exclusive-data-lock"] = true
	client.Close()
	if err := child.Close(); err != nil {
		return err
	}
	child, address, err = start("restart")
	if err != nil {
		return err
	}
	restarted, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer restarted.Close()
	if err := checkProjectOperationRestart(ctx, restarted, projectOperationID); err != nil {
		return err
	}
	report.Checks["project-journal-restart-durable"] = true
	if err := checkScheduledRestart(ctx, restarted, scheduledID); err != nil {
		return err
	}
	report.Checks["scheduled-restart-durable"] = true
	if err := checkHydrate(restarted); err != nil {
		return err
	}
	report.Checks["restart-durable"] = true
	if _, err := restarted.RequestFor(ctx, sessionID, "", map[string]any{"type": "removeSession"}); err != nil {
		return err
	}
	if _, err := restarted.Request(ctx, command); err != nil {
		return err
	}
	raw, err = restarted.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": sessionID})
	if err != nil {
		return err
	}
	var removed struct {
		Session json.RawMessage `json:"session"`
	}
	if err := json.Unmarshal(raw, &removed); err != nil {
		return err
	}
	if string(removed.Session) != "null" {
		return errors.New("stale snapshot resurrected a removed session")
	}
	report.Checks["no-stale-resurrection"] = true
	if err := pressure(ctx, address, token, round); err != nil {
		return fmt.Errorf("pressure: %w", err)
	}
	report.Checks["pressure-2000-messages-eight-readers"] = true
	if exporter != "" {
		if err := verifyExport(ctx, exporter, binary, dataDir, round); err != nil {
			return fmt.Errorf("export: %w", err)
		}
		report.Checks["online-backup-sanitized-reopen"] = true
	}
	if err := checkWorkspaceEscape(ctx, restarted, sessionID, round); err != nil {
		return fmt.Errorf("workspace escape: %w", err)
	}
	report.Checks["workspace-confined-to-profile-workspace"] = true
	if err := checkRemoveProject(ctx, restarted); err != nil {
		return fmt.Errorf("remove project: %w", err)
	}
	report.Checks["remove-project-cleans-children"] = true
	if err := checkSkeletonStale(ctx, restarted); err != nil {
		return fmt.Errorf("stale skeleton: %w", err)
	}
	report.Checks["skeleton-stale-no-regress"] = true
	return nil
}

func waitReady(ctx context.Context, child *ownedprocess.Owned) (string, error) {
	timer := time.NewTimer(10 * time.Second)
	defer timer.Stop()
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		data, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address     string `json:"address"`
			Protocol    int    `json:"protocolVersion"`
			PID         uint32 `json:"pid"`
			Environment string `json:"environmentId"`
			Instance    string `json:"instanceId"`
		}
		if json.Unmarshal(data, &ready) == nil && ready.Address != "" {
			if ready.Protocol != daemon.ProtocolVersion || ready.PID != child.PID || uuid.Validate(ready.Environment) != nil || uuid.Validate(ready.Instance) != nil || ready.Environment == uuid.Nil.String() || ready.Instance == uuid.Nil.String() {
				return "", errors.New("invalid ready identity")
			}
			return ready.Address, nil
		}
		if done, err := child.Exited(0); err != nil || done {
			return "", errors.New("daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-timer.C:
			return "", errors.New("ready deadline exceeded")
		case <-tick.C:
		}
	}
}

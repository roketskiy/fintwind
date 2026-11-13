package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/launchprofile"
	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
)

func verifyLauncher(ctx context.Context, root, binary string) (err error) {
	if selectedBackend != "go" {
		return errors.New("launcher verification currently requires --backend go")
	}
	if !filepath.IsAbs(binary) {
		return errors.New("absolute Go daemon path required")
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	evidence := map[string]any{}
	defer func() {
		report := map[string]any{"passed": err == nil, "checks": checks, "evidence": evidence, "visualTested": false, "dailyOpened": false}
		if err != nil {
			report["error"] = err.Error()
		}
		raw, e := json.MarshalIndent(report, "", "  ")
		if e == nil {
			e = os.WriteFile(filepath.Join(round, "launcher-result.json"), raw, 0o600)
		}
		err = errors.Join(err, e)
		fmt.Println("[REPORT]", filepath.Join(round, "launcher-result.json"))
	}()
	self, err := os.Executable()
	if err != nil {
		return err
	}
	mygoHash, err := sha256File(self)
	if err != nil {
		return err
	}
	daemonHash, err := sha256File(binary)
	if err != nil {
		return err
	}
	evidence["mygoBinary"] = self
	evidence["mygoSha256"] = mygoHash
	evidence["daemonBinary"] = binary
	evidence["daemonSha256"] = daemonHash
	for _, scenario := range []struct {
		name    string
		args    []string
		message string
	}{
		{"unknown-backend-refused", []string{"--root", root, "--profile", "agent", "--backend", "invalid", "--plan"}, "backend must be go"},
	} {
		logs := filepath.Join(round, scenario.name)
		if err := os.Mkdir(logs, 0o700); err != nil {
			return err
		}
		child, err := ownedprocess.StartWithEnvironment(self, scenario.args, logs, round, ownedprocess.BuildEnv(round, nil))
		if err != nil {
			return err
		}
		if err := waitPolicyRefusal(ctx, child, scenario.message); err != nil {
			child.Close()
			return err
		}
		if err := child.Close(); err != nil {
			return err
		}
		checks[scenario.name] = true
	}
	parent := ownedprocess.BuildEnv(round, map[string]string{"OPENAI_API_KEY": "sentinel-not-real", "OPENCODE_API_KEY": "sentinel-not-real", "FINTWIND_DAEMON_TOKEN": "sentinel-not-real"})
	plan, err := launchprofile.Resolve(launchprofile.Options{Profile: "agent", Name: "go-launcher-verify", Root: root, ParentEnv: parent})
	if err != nil {
		return err
	}
	for _, entry := range plan.Environment {
		key, _, _ := strings.Cut(entry, "=")
		switch strings.ToUpper(key) {
		case "OPENAI_API_KEY", "OPENCODE_API_KEY", "FINTWIND_DAEMON_TOKEN":
			return errors.New("credential sentinel crossed launcher environment boundary")
		}
	}
	checks["credential-environment-whitelist"] = true
	if err := verifyPlanCLI(ctx, self, root, binary, round, checks, evidence); err != nil {
		return err
	}
	if err := plan.Prepare(); err != nil {
		return err
	}
	nonce := uuid.NewString()
	marker := filepath.Join(round, "probe.json")
	plan.AppArgs = []string{"--launcher-probe"}
	plan.Environment = withEnvironment(plan.Environment, map[string]string{"MYGO_VERIFY_MARKER": marker, "MYGO_VERIFY_NONCE": nonce, "MYGO_VERIFY_DATA": plan.DataDir})
	for _, name := range []string{"first", "restart"} {
		logs := filepath.Join(round, name)
		if err := os.Mkdir(logs, 0o700); err != nil {
			return err
		}
		if name == "restart" {
			plan.Environment = withEnvironment(plan.Environment, map[string]string{"MYGO_VERIFY_EXPECT_SAVED": "1"})
			if err := os.Remove(marker); err != nil {
				return err
			}
		}
		if err := launch(ctx, plan, binaries{app: self, daemon: binary}, logs); err != nil {
			return err
		}
		raw, err := os.ReadFile(marker)
		if err != nil {
			return errors.New("launcher never ran the controlled frontend")
		}
		var got struct {
			Nonce string `json:"nonce"`
			Data  string `json:"dataDir"`
		}
		if err := json.Unmarshal(raw, &got); err != nil {
			return err
		}
		if got.Nonce != nonce || got.Data != plan.DataDir {
			return errors.New("frontend observed wrong launch identity")
		}
		checks[name+"-launch-handshake-shutdown"] = true
	}
	if err := os.Remove(marker); err != nil {
		return err
	}
	bad := plan
	bad.Environment = withEnvironment(plan.Environment, map[string]string{"MYGO_VERIFY_BAD_DAEMON": "1"})
	logs := filepath.Join(round, "bad-protocol")
	if err := os.Mkdir(logs, 0o700); err != nil {
		return err
	}
	if err := launch(ctx, bad, binaries{app: self, daemon: self}, logs); err == nil || !strings.Contains(err.Error(), "mismatch") {
		return errors.New("launcher accepted mismatched daemon ready identity")
	}
	if _, err := os.Stat(marker); !errors.Is(err, os.ErrNotExist) {
		return errors.New("frontend opened after invalid ready identity")
	}
	checks["invalid-ready-never-opens-frontend"] = true
	return nil
}

func sha256File(path string) (string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:]), nil
}

func verifyPlanCLI(ctx context.Context, self, root, daemonPath, round string, checks map[string]bool, evidence map[string]any) error {
	name := "plan-" + strings.ReplaceAll(uuid.NewString(), "-", "")
	args := []string{"--root", root, "--profile", "agent", "--backend", "go", "--plan", "--name", name}
	evidence["planArgv"] = append([]string{self}, args...)
	sentinels := map[string]string{
		"OPENAI_API_KEY":        "plan-provider-secret-sentinel",
		"OPENCODE_API_KEY":      "plan-opencode-secret-sentinel",
		"FINTWIND_DAEMON_TOKEN": "plan-daemon-token-sentinel",
		"ANTHROPIC_API_KEY":     "plan-anthropic-secret-sentinel",
	}
	env := ownedprocess.BuildEnv(round, sentinels)
	expected, err := launchprofile.Resolve(launchprofile.Options{Profile: "agent", Name: name, Root: root, ParentEnv: env})
	if err != nil {
		return fmt.Errorf("resolve expected plan paths: %w", err)
	}
	paths := []string{expected.DataDir, expected.PreferencesDir, expected.WorkDir, expected.BuildDir}
	for _, path := range paths {
		if _, statErr := os.Lstat(path); statErr == nil {
			return fmt.Errorf("unique plan path unexpectedly exists before CLI: %s", path)
		} else if !errors.Is(statErr, os.ErrNotExist) {
			return statErr
		}
	}
	logs := filepath.Join(round, "plan-cli")
	if err := os.Mkdir(logs, 0o700); err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(self, args, logs, round, env)
	if err != nil {
		return err
	}
	closed := false
	defer func() {
		if !closed {
			_ = child.Close()
		}
	}()
	deadline, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	for {
		done, waitErr := child.Exited(50)
		if waitErr != nil {
			return waitErr
		}
		if done {
			break
		}
		if err := deadline.Err(); err != nil {
			return fmt.Errorf("plan CLI did not exit: %w", err)
		}
	}
	code, err := child.ExitCode()
	if err != nil {
		return err
	}
	stdout, err := os.ReadFile(child.StdoutPath)
	if err != nil {
		return err
	}
	stderr, err := os.ReadFile(child.StderrPath)
	if err != nil {
		return err
	}
	if err := child.Close(); err != nil {
		return err
	}
	closed = true
	evidence["planStdout"] = string(stdout)
	evidence["planStderr"] = string(stderr)
	if code != 0 {
		return fmt.Errorf("plan CLI exited with code %d: %s", code, strings.TrimSpace(string(stderr)))
	}
	trimmed := strings.TrimSpace(string(stdout))
	if trimmed == "" || strings.Contains(trimmed, "\n") {
		return errors.New("plan CLI did not print exactly one JSON line")
	}
	var got struct {
		Profile         string `json:"profile"`
		Name            string `json:"name"`
		DataDir         string `json:"dataDir"`
		PreferencesDir  string `json:"preferencesDir"`
		WorkDir         string `json:"workDir"`
		BuildDir        string `json:"buildDir"`
		ProtocolVersion int    `json:"protocolVersion"`
	}
	if err := json.Unmarshal([]byte(trimmed), &got); err != nil {
		return fmt.Errorf("plan output is not one JSON object: %w", err)
	}
	if got.ProtocolVersion != daemon.ProtocolVersion {
		return fmt.Errorf("plan protocol version %d does not match daemon.ProtocolVersion %d", got.ProtocolVersion, daemon.ProtocolVersion)
	}
	if got.Profile != string(launchprofile.Agent) || got.Name != name || got.DataDir != expected.DataDir || got.PreferencesDir != expected.PreferencesDir || got.WorkDir != expected.WorkDir || got.BuildDir != expected.BuildDir {
		return errors.New("plan JSON does not match resolved isolated plan")
	}
	for _, secret := range sentinels {
		if strings.Contains(string(stdout), secret) {
			return errors.New("plan JSON exposed an environment secret sentinel")
		}
	}
	if strings.TrimSpace(string(stderr)) != "" {
		return errors.New("plan CLI wrote unexpected stderr")
	}
	for _, path := range paths {
		if _, statErr := os.Lstat(path); statErr == nil {
			return fmt.Errorf("--plan created application data path: %s", path)
		} else if !errors.Is(statErr, os.ErrNotExist) {
			return statErr
		}
	}
	if strings.Contains(strings.ToLower(string(stdout)), "daemon.exe") || strings.Contains(strings.ToLower(string(stdout)), "fintwind.exe") {
		return errors.New("plan output indicates an app or daemon launch")
	}
	evidence["planProtocolVersion"] = got.ProtocolVersion
	evidence["planName"] = name
	evidence["planDataDir"] = got.DataDir
	evidence["planPathsAbsentAfter"] = true
	checks["plan-cli-reports-current-protocol-and-resolved-paths"] = true
	checks["plan-cli-does-not-expose-secret-sentinels"] = true
	checks["plan-cli-does-not-prepare-or-start-app-daemon"] = true
	return nil
}

func waitPolicyRefusal(ctx context.Context, child *ownedprocess.Owned, message string) error {
	deadline, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	for {
		done, err := child.Exited(50)
		if err != nil {
			return err
		}
		if done {
			code, err := child.ExitCode()
			if err != nil {
				return err
			}
			raw, err := os.ReadFile(child.StderrPath)
			if err != nil {
				return err
			}
			if code == 0 || !strings.Contains(string(raw), message) {
				return errors.New("launcher policy did not refuse before preparation")
			}
			return nil
		}
		if err := deadline.Err(); err != nil {
			return err
		}
	}
}

func launcherProbe() error {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	for _, key := range []string{"OPENAI_API_KEY", "OPENCODE_API_KEY"} {
		if os.Getenv(key) != "" {
			return errors.New("provider credential leaked into controlled frontend")
		}
	}
	data := os.Getenv("MYGO_VERIFY_DATA")
	marker := os.Getenv("MYGO_VERIFY_MARKER")
	nonce := os.Getenv("MYGO_VERIFY_NONCE")
	if !filepath.IsAbs(data) || !filepath.IsAbs(marker) || nonce == "" {
		return errors.New("controlled frontend identity missing")
	}
	client, err := daemon.Dial(ctx, os.Getenv("FINTWIND_MYGO_DAEMON_ADDRESS"), os.Getenv("FINTWIND_MYGO_DAEMON_TOKEN"))
	if err != nil {
		return err
	}
	defer client.Close()
	raw, err := client.Request(ctx, map[string]any{"type": "getSettings"})
	if err != nil {
		return err
	}
	response, err := backendstore.Decode(raw)
	if err != nil {
		return err
	}
	settings, err := backendstore.Decode(response["settings"])
	if err != nil {
		return err
	}
	if os.Getenv("MYGO_VERIFY_EXPECT_SAVED") == "1" && settings.String("launcher_verify_nonce") != nonce {
		return errors.New("settings lost across launcher restart")
	}
	if _, err := client.Request(ctx, map[string]any{"type": "updateSettings", "settings": map[string]any{"launcher_verify_nonce": nonce}}); err != nil {
		return err
	}
	report, _ := json.Marshal(map[string]any{"nonce": nonce, "dataDir": data})
	return os.WriteFile(marker, report, 0o600)
}

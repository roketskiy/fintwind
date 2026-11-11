package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/launchprofile"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"
)

// This exercises real processes and SQLite, not the native window or providers.
// All runtime paths descend from a fresh, owned verification directory.
func verifyLauncher(ctx context.Context, root, executable string) (result error) {
	if !filepath.IsAbs(root) || !filepath.IsAbs(executable) {
		return errors.New("--verify requires absolute --root and --daemon paths")
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	type check struct {
		Name   string `json:"name"`
		Passed bool   `json:"passed"`
	}
	checks := []check{}
	record := func(name string, ok bool) error {
		checks = append(checks, check{name, ok})
		if !ok {
			return fmt.Errorf("launcher E2E failed: %s", name)
		}
		return nil
	}
	defer func() {
		report := map[string]any{"checks": checks, "passed": result == nil, "round": round, "protocolVersion": daemon.ProtocolVersion, "nativeWindowTested": false, "realUserDataOpened": false}
		if result != nil {
			report["error"] = result.Error()
		}
		digest, _ := fileHash(executable)
		report["daemonSha256"] = digest
		data, _ := json.MarshalIndent(report, "", "  ")
		path := filepath.Join(round, "result.json")
		if err := os.WriteFile(path, data, 0o600); err != nil && result == nil {
			result = err
		}
		fmt.Println("[VERIFY]", path)
	}()
	parent := ownedprocess.BuildEnv(round, map[string]string{"ANTHROPIC_API_KEY": "launcher-sentinel-not-a-key", "OPENCODE_AUTH_CONTENT": "launcher-sentinel", "FINTWIND_DAEMON_ADDRESS": "127.0.0.1:1", "FINTWIND_DAEMON_TOKEN": "launcher-sentinel"})
	daily, err := launchprofile.Resolve(launchprofile.Options{Profile: "daily", Root: root, ParentEnv: parent})
	if err != nil {
		return err
	}
	agent, err := launchprofile.Resolve(launchprofile.Options{Root: root, Name: "verify", ParentEnv: parent})
	if err != nil {
		return err
	}
	_, noWrite := os.Stat(daily.DataDir)
	if err := record("plan resolution does not create production data", os.IsNotExist(noWrite)); err != nil {
		return err
	}
	if err := daily.Prepare(); err != nil {
		return err
	}
	_, noWrite = os.Stat(daily.DataDir)
	if err := record("daily preparation creates outputs but not production data", os.IsNotExist(noWrite)); err != nil {
		return err
	}
	for _, name := range []string{"../daily", "C:\\Fintwind", "a/b", "a.b"} {
		_, err := launchprofile.Resolve(launchprofile.Options{Root: root, Name: name, ParentEnv: parent})
		if err := record("reject unsafe agent name "+name, err != nil); err != nil {
			return err
		}
	}
	if err := agent.Prepare(); err != nil {
		return err
	}
	self, err := os.Executable()
	if err != nil {
		return err
	}
	probePath := filepath.Join(round, "probe.json")
	agent.Environment = withEnvironment(agent.Environment, map[string]string{"MYGO_VERIFY_RESULT": probePath})
	agent.AppArgs = append([]string{"--launcher-probe"}, agent.AppArgs...)
	runDir := filepath.Join(round, "agent-launch")
	if err := os.MkdirAll(runDir, 0o700); err != nil {
		return err
	}
	if err := launch(ctx, agent, binaries{self, executable}, runDir); err != nil {
		return err
	}
	payload, err := os.ReadFile(probePath)
	if err != nil {
		return err
	}
	var probe struct {
		Connected, SecretPresent                    bool
		Home, Local, Config, Workspace, Projectless string
	}
	if err := json.Unmarshal(payload, &probe); err != nil {
		return err
	}
	if err := record("launcher connects a real child to the matching owned daemon", probe.Connected); err != nil {
		return err
	}
	if err := record("child does not inherit provider keys or old daemon capabilities", !probe.SecretPresent); err != nil {
		return err
	}
	if err := record("child home config and workspace are inside agent profile", strings.HasPrefix(probe.Home, filepath.Dir(agent.DataDir)) && strings.HasPrefix(probe.Local, filepath.Dir(agent.DataDir)) && strings.HasPrefix(probe.Config, filepath.Dir(agent.DataDir)) && probe.Workspace == agent.WorkDir); err != nil {
		return err
	}
	if err := record("Rust projectless path uses isolated home on Windows", strings.HasPrefix(strings.ToLower(probe.Projectless), strings.ToLower(filepath.Dir(agent.DataDir)))); err != nil {
		return err
	}
	if err := os.Remove(probePath); err != nil {
		return err
	}
	badDaemon := filepath.Join(round, "bad-version-daemon")
	if err := os.MkdirAll(badDaemon, 0o700); err != nil {
		return err
	}
	badPlan := agent
	badPlan.Environment = withEnvironment(agent.Environment, map[string]string{"MYGO_VERIFY_BAD_DAEMON": "1"})
	badErr := launch(ctx, badPlan, binaries{self, self}, badDaemon)
	_, probeErr := os.Stat(probePath)
	if err := record("launcher refuses a mismatched protocol without starting frontend", badErr != nil && strings.Contains(badErr.Error(), "mismatch") && os.IsNotExist(probeErr)); err != nil {
		return err
	}
	fakeHome := filepath.Join(round, "home")
	runtimeEnv := ownedprocess.BuildEnv(round, map[string]string{"FINTWIND_MYGO_PROFILE": "agent", "FINTWIND_MYGO_EXPECTED_DATA_DIR": daily.DataDir})
	if err := os.MkdirAll(daily.DataDir, 0o700); err != nil {
		return err
	}
	settingsPath := filepath.Join(fakeHome, ".fintwind", "settings.json")
	if err := os.MkdirAll(filepath.Dir(settingsPath), 0o700); err != nil {
		return err
	}
	settings := []byte(`{"auto_compact_enabled":false,"launcher_fixture":"real-settings-authority"}`)
	if err := os.WriteFile(settingsPath, settings, 0o600); err != nil {
		return err
	}
	legacySettings := filepath.Join(daily.DataDir, "daemon.json")
	if err := os.WriteFile(legacySettings, []byte(`{"launcher_fixture":"wrong-data-dir-settings"}`), 0o600); err != nil {
		return err
	}
	start := func(label, data string, production bool) (*ownedprocess.Owned, *daemon.Client, error) {
		run := filepath.Join(round, label)
		if err := os.MkdirAll(run, 0o700); err != nil {
			return nil, nil, err
		}
		args := []string{"--bind", "127.0.0.1:0", "--parent-pid", strconv.Itoa(os.Getpid())}
		if production {
			args = append(args, "--production-data")
		} else {
			args = append(args, "--data-dir", data)
		}
		const token = "isolated-launcher-e2e-token"
		process, err := ownedprocess.StartWithEnvironment(executable, args, run, round, withEnvironment(runtimeEnv, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
		if err != nil {
			return nil, nil, err
		}
		ready, err := waitReady(ctx, process, 10*time.Second)
		if err != nil {
			process.Close()
			return nil, nil, err
		}
		if ready.PID != process.PID || ready.Protocol != daemon.ProtocolVersion {
			process.Close()
			return nil, nil, errors.New("owned ready identity mismatch")
		}
		client, err := daemon.Dial(ctx, ready.Address, token)
		if err != nil {
			process.Close()
			return nil, nil, err
		}
		return process, client, nil
	}
	stop := func(process *ownedprocess.Owned, client *daemon.Client) error {
		call, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		defer cancel()
		_ = client.Send(call, map[string]any{"type": "shutdown"})
		client.Close()
		exited, err := process.Exited(3000)
		closeErr := process.Close()
		if err != nil {
			return err
		}
		if closeErr != nil {
			return closeErr
		}
		if !exited {
			return errors.New("owned daemon did not complete graceful shutdown")
		}
		return nil
	}
	seed, client, err := start("seed", daily.DataDir, false)
	if err != nil {
		return err
	}
	defer seed.Close()
	defer client.Close()
	projectID := "10000000-0000-4000-8000-000000000001"
	sessionID := "20000000-0000-4000-8000-000000000002"
	session := map[string]any{"id": sessionID, "title": "original history", "project_id": projectID, "provider": "opencode", "runtime_mode": "fullAccess", "interaction_mode": "build", "status": "idle", "created_at": 1700000000, "updated_at": 1700000000, "mermaid_prompt_version": "v4"}
	session["messages"] = []any{map[string]any{"id": "30000000-0000-4000-8000-000000000003", "role": "assistant", "content": "persisted fixture history", "created_at": 1700000000, "streaming": false}}
	project := map[string]any{"id": projectID, "name": "launcher fixture", "path": round, "environment_id": "", "created_at": 1700000000}
	save := map[string]any{"type": "saveTaskStateCheckpoint", "projects": []any{project}, "sessions": []any{session}, "skeletonSessionIds": []string{}}
	if _, err := client.Request(ctx, save); err != nil {
		return err
	}
	before, err := profileDigests(daily.DataDir, settingsPath)
	if err != nil {
		return err
	}
	_, _, conflict := start("conflict", daily.DataDir, true)
	if err := record("production startup refuses the existing legacy data lock", conflict != nil && strings.Contains(conflict.Error(), "Fintwind")); err != nil {
		return err
	}
	after, err := profileDigests(daily.DataDir, settingsPath)
	if err != nil {
		return err
	}
	if err := record("refused startup does not alter database settings identity or backup marker", reflect.DeepEqual(before, after)); err != nil {
		return err
	}
	if err := stop(seed, client); err != nil {
		return err
	}
	production, connection, err := start("production", daily.DataDir, true)
	if err != nil {
		return err
	}
	defer production.Close()
	defer connection.Close()
	raw, err := connection.Request(ctx, map[string]any{"type": "getSettings"})
	if err != nil {
		return err
	}
	if err := record("production reads home settings instead of data-dir daemon.json", strings.Contains(string(raw), "real-settings-authority") && !strings.Contains(string(raw), "wrong-data-dir-settings")); err != nil {
		return err
	}
	raw, err = connection.Request(ctx, map[string]any{"type": "loadTaskState"})
	if err != nil {
		return err
	}
	if err := record("production opens existing history rather than an empty database", strings.Contains(string(raw), "original history")); err != nil {
		return err
	}
	backupRoot := filepath.Join(fakeHome, "Fintwind Go", "backups")
	_, backupErr := os.Stat(backupRoot)
	_, markerErr := os.Stat(filepath.Join(daily.DataDir, ".mygo-initial-backup"))
	if err := record("direct startup creates no backup or backup marker", os.IsNotExist(backupErr) && os.IsNotExist(markerErr)); err != nil {
		return err
	}
	session["title"] = "saved by new frontend"
	if _, err := connection.Request(ctx, save); err != nil {
		return err
	}
	if _, err := connection.Request(ctx, map[string]any{"type": "updateSetting", "key": "auto_compact_enabled", "value": true}); err != nil {
		return err
	}
	if err := stop(production, connection); err != nil {
		return err
	}
	restarted, reconnected, err := start("production-restart", daily.DataDir, true)
	if err != nil {
		return err
	}
	defer restarted.Close()
	defer reconnected.Close()
	raw, err = reconnected.Request(ctx, map[string]any{"type": "loadTaskState"})
	if err != nil {
		return err
	}
	if err := record("direct profile writes persist in the same database after restart", strings.Contains(string(raw), "saved by new frontend")); err != nil {
		return err
	}
	updatedSettings, err := os.ReadFile(settingsPath)
	if err != nil {
		return err
	}
	if err := record("settings updates keep the original home settings authority", strings.Contains(string(updatedSettings), `"auto_compact_enabled": true`)); err != nil {
		return err
	}
	if err := stop(restarted, reconnected); err != nil {
		return err
	}
	_, backupErr = os.Stat(backupRoot)
	if err := record("restart still creates no automatic backup", os.IsNotExist(backupErr)); err != nil {
		return err
	}
	return nil
}

func profileDigests(directory, settings string) (map[string]string, error) {
	result := map[string]string{}
	for _, name := range []string{"app.db", "app.db-wal", "app.db-shm", "environment_id", ".mygo-initial-backup", "daemon.json"} {
		path := filepath.Join(directory, name)
		hash, err := fileHash(path)
		if os.IsNotExist(err) {
			result[name] = "absent"
			continue
		}
		if err != nil {
			return nil, err
		}
		result[name] = hash
	}
	hash, err := fileHash(settings)
	if err != nil {
		return nil, err
	}
	result["settings"] = hash
	return result, nil
}

func launcherProbe() error {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	client, err := daemon.Dial(ctx, os.Getenv("FINTWIND_MYGO_DAEMON_ADDRESS"), os.Getenv("FINTWIND_MYGO_DAEMON_TOKEN"))
	if err != nil {
		return err
	}
	defer client.Close()
	raw, err := client.Request(ctx, map[string]any{"type": "loadTaskState"})
	if err != nil {
		return err
	}
	var state model.TaskState
	if err := json.Unmarshal(raw, &state); err != nil {
		return err
	}
	workspace, err := os.Getwd()
	if err != nil {
		return err
	}
	report := map[string]any{"Connected": true, "SecretPresent": os.Getenv("ANTHROPIC_API_KEY") != "" || os.Getenv("OPENCODE_AUTH_CONTENT") != "" || os.Getenv("FINTWIND_DAEMON_ADDRESS") != "" || os.Getenv("FINTWIND_DAEMON_TOKEN") != "", "Home": os.Getenv("HOME"), "Local": os.Getenv("LOCALAPPDATA"), "Config": os.Getenv("OPENCODE_CONFIG_DIR"), "Workspace": workspace, "Projectless": state.ProjectlessRoot}
	data, err := json.MarshalIndent(report, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(os.Getenv("MYGO_VERIFY_RESULT"), data, 0o600)
}

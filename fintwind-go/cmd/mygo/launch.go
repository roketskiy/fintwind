package main

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/launchprofile"
	"fintwind-go/internal/ownedprocess"
)

type daemonReady struct {
	Address  string `json:"address"`
	Protocol int    `json:"protocolVersion"`
	PID      uint32 `json:"pid"`
}

func launch(ctx context.Context, plan launchprofile.Plan, binaries binaries, runDir string) error {
	tokenBytes := make([]byte, 32)
	if _, err := rand.Read(tokenBytes); err != nil {
		return err
	}
	token := hex.EncodeToString(tokenBytes)
	daemonDir := filepath.Join(runDir, "daemon")
	appDir := filepath.Join(runDir, "app")
	for _, dir := range []string{daemonDir, appDir} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
	}
	args := []string{"--bind", "127.0.0.1:0", "--parent-pid", strconv.Itoa(os.Getpid())}
	if string(plan.Profile) == "daily" {
		args = append(args, "--production-data")
	} else {
		args = append(args, "--data-dir", plan.DataDir)
	}
	backend, err := ownedprocess.StartWithEnvironment(binaries.daemon, args, daemonDir, plan.WorkDir, withEnvironment(plan.Environment, map[string]string{"FINTWIND_DAEMON_TOKEN": token, "FINTWIND_MYGO_EXPECTED_DATA_DIR": plan.DataDir}))
	if err != nil {
		return err
	}
	defer backend.Close()
	startupTimeout := 25 * time.Second
	if plan.Profile == launchprofile.Daily {
		fmt.Println("[OK] Opening existing Fintwind data directly.")
		startupTimeout = 3 * time.Minute
	}
	ready, err := waitReady(ctx, backend, startupTimeout)
	if err != nil {
		return err
	}
	if ready.Protocol != daemon.ProtocolVersion || ready.PID != backend.PID {
		return errors.New("daemon version or owned-process identity mismatch; frontend was not opened")
	}
	connectCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	client, err := daemon.Dial(connectCtx, ready.Address, token)
	cancel()
	if err != nil {
		return fmt.Errorf("daemon handshake failed; frontend was not opened: %w", err)
	}
	defer client.Close()
	client.Close() // The launcher's probe is not a long-lived event subscriber.
	defer func() {
		stopCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		connection, err := daemon.Dial(stopCtx, ready.Address, token)
		if err == nil {
			_ = connection.Send(stopCtx, map[string]any{"type": "shutdown"})
			connection.Close()
		}
		_, _ = backend.Exited(5000)
	}()
	appArgs := append(append([]string(nil), plan.AppArgs...), "--connect")
	frontend, err := ownedprocess.StartWithEnvironment(binaries.app, appArgs, appDir, plan.WorkDir, withEnvironment(plan.Environment, map[string]string{"FINTWIND_MYGO_DAEMON_ADDRESS": ready.Address, "FINTWIND_MYGO_DAEMON_TOKEN": token}))
	if err != nil {
		return err
	}
	defer frontend.Close()
	fmt.Println("[OK] MyGo started. Close the window to stop this launcher and its daemon.")
	tick := time.NewTicker(250 * time.Millisecond)
	defer tick.Stop()
	for {
		closed, err := frontend.Exited(0)
		if err != nil {
			return err
		}
		if closed {
			code, err := frontend.ExitCode()
			if err != nil {
				return err
			}
			if code != 0 {
				return fmt.Errorf("MyGo exited with code %d; see %s", code, frontend.StderrPath)
			}
			return nil
		}
		dead, err := backend.Exited(0)
		if err != nil {
			return err
		}
		if dead {
			return fmt.Errorf("launcher-owned daemon exited; see %s", backend.StderrPath)
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-tick.C:
		}
	}
}

func waitReady(ctx context.Context, process *ownedprocess.Owned, timeout time.Duration) (daemonReady, error) {
	deadline, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	tick := time.NewTicker(50 * time.Millisecond)
	defer tick.Stop()
	for {
		data, _ := os.ReadFile(process.StdoutPath)
		line, _, found := strings.Cut(string(data), "\n")
		var ready daemonReady
		if found && json.Unmarshal([]byte(line), &ready) == nil && ready.Address != "" {
			return ready, nil
		}
		exited, err := process.Exited(0)
		if err != nil {
			return ready, err
		}
		if exited {
			log, _ := os.ReadFile(process.StderrPath)
			if len(log) > 8192 {
				log = log[len(log)-8192:]
			}
			return ready, fmt.Errorf("daemon refused startup: %s", strings.TrimSpace(string(log)))
		}
		select {
		case <-deadline.Done():
			return ready, fmt.Errorf("daemon startup deadline reached; see %s", process.StderrPath)
		case <-tick.C:
		}
	}
}

func withEnvironment(base []string, overrides map[string]string) []string {
	var result []string
	for _, entry := range base {
		key, _, _ := strings.Cut(entry, "=")
		if _, replace := overrides[strings.ToUpper(key)]; !replace {
			result = append(result, entry)
		}
	}
	for key, value := range overrides {
		result = append(result, key+"="+value)
	}
	sort.Slice(result, func(i, j int) bool { return strings.ToUpper(result[i]) < strings.ToUpper(result[j]) })
	return result
}

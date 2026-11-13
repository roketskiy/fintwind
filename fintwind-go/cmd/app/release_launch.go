package main

import (
	"context"
	"crypto/rand"
	encodinghex "encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"
)

func startReleaseDaemon(binary, dataDir string) (address, token string, stop func(), err error) {
	if err = os.MkdirAll(dataDir, 0o700); err != nil {
		return
	}
	var random [32]byte
	if _, err = rand.Read(random[:]); err != nil {
		return
	}
	token = encodinghex.EncodeToString(random[:])
	logs, err := os.MkdirTemp(filepath.Join(dataDir), "daemon-run-")
	if err != nil {
		return "", "", nil, err
	}
	process, err := ownedprocess.StartWithEnvironment(binary,
		[]string{"--bind", "127.0.0.1:0", "--parent-pid", strconv.Itoa(os.Getpid()), "--data-dir", dataDir},
		logs, filepath.Dir(binary), ownedprocess.InheritEnv(map[string]string{
			"FINTWIND_MYGO_PROFILE": "daily", "FINTWIND_MYGO_EXPECTED_DATA_DIR": dataDir,
			"FINTWIND_DAEMON_TOKEN": token,
		}))
	if err != nil {
		return "", "", nil, err
	}
	success := false
	defer func() {
		if !success {
			_ = process.Close()
		}
	}()
	deadline := time.NewTimer(3 * time.Minute)
	defer deadline.Stop()
	tick := time.NewTicker(50 * time.Millisecond)
	defer tick.Stop()
	for {
		data, _ := os.ReadFile(process.StdoutPath)
		line, _, found := strings.Cut(string(data), "\n")
		var ready struct {
			Address  string `json:"address"`
			Protocol int    `json:"protocolVersion"`
			PID      uint32 `json:"pid"`
		}
		if found && json.Unmarshal([]byte(line), &ready) == nil && ready.Address != "" {
			if ready.Protocol != daemon.ProtocolVersion || ready.PID != process.PID {
				return "", "", nil, errors.New("daemon version or process identity mismatch")
			}
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			client, dialErr := daemon.Dial(ctx, ready.Address, token)
			cancel()
			if dialErr != nil {
				return "", "", nil, fmt.Errorf("daemon handshake: %w", dialErr)
			}
			client.Close()
			address = ready.Address
			stop = func() {
				ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				defer cancel()
				client, err := daemon.Dial(ctx, address, token)
				if err == nil {
					_ = client.Send(ctx, map[string]any{"type": "shutdown"})
					client.Close()
				}
				_, _ = process.Exited(5000)
				_ = process.Close()
			}
			success = true
			return address, token, stop, nil
		}
		exited, checkErr := process.Exited(0)
		if checkErr != nil {
			return "", "", nil, checkErr
		}
		if exited {
			data, _ := os.ReadFile(process.StderrPath)
			return "", "", nil, fmt.Errorf("daemon failed to start: %s", strings.TrimSpace(string(data)))
		}
		select {
		case <-deadline.C:
			return "", "", nil, fmt.Errorf("daemon startup timed out; see %s", process.StderrPath)
		case <-tick.C:
		}
	}
}

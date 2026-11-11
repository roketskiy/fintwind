package daemon

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
)

// Process is a daemon this client started and owns.
type Process struct {
	cmd           *exec.Cmd
	Address       string
	Token         string
	PID           int
	EnvironmentID string
	stopOnce      sync.Once
}

type readyLine struct {
	Address              string `json:"address"`
	ProtocolVersion      int    `json:"protocolVersion"`
	ProtocolVersionSnake int    `json:"protocol_version"`
	PID                  int    `json:"pid"`
	EnvironmentID        string `json:"environmentId"`
}

// Start launches an owned daemon and refuses production data directories.
func Start(ctx context.Context, executable, dataDir string) (*Process, error) {
	absolute, err := filepath.Abs(dataDir)
	if err != nil {
		return nil, fmt.Errorf("resolve data directory: %w", err)
	}
	if err := checkIsolatedDataDir(absolute); err != nil {
		return nil, err
	}
	if err := os.MkdirAll(absolute, 0o755); err != nil {
		return nil, fmt.Errorf("create data directory: %w", err)
	}
	token, err := randomToken()
	if err != nil {
		return nil, err
	}
	cmd := exec.Command(executable,
		"--bind", "127.0.0.1:0",
		"--data-dir", absolute,
		"--parent-pid", strconv.Itoa(os.Getpid()),
	)
	cmd.Env = append(os.Environ(), "FINTWIND_DAEMON_TOKEN="+token)
	hideWindow(cmd)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("daemon stdout: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, fmt.Errorf("daemon stderr: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start daemon: %w", err)
	}
	process := &Process{cmd: cmd, Token: token}
	ready := make(chan readyLine, 1)
	readErr := make(chan error, 1)
	go func() {
		reader := bufio.NewReader(stdout)
		line, err := reader.ReadString('\n')
		if err != nil {
			readErr <- err
			return
		}
		var value readyLine
		if err := json.Unmarshal([]byte(strings.TrimSpace(line)), &value); err != nil {
			readErr <- fmt.Errorf("parse daemon ready line: %w", err)
			return
		}
		ready <- value
		_, _ = io.Copy(io.Discard, reader)
	}()
	go func() { _, _ = io.Copy(os.Stderr, stderr) }()

	select {
	case value := <-ready:
		version := value.ProtocolVersion
		if version == 0 {
			version = value.ProtocolVersionSnake
		}
		process.Address = value.Address
		process.PID = value.PID
		process.EnvironmentID = value.EnvironmentID
		if version != ProtocolVersion {
			process.Stop()
			return nil, fmt.Errorf("daemon speaks protocol %d, this client speaks %d", version, ProtocolVersion)
		}
		if err := checkLoopbackAddress(value.Address); err != nil {
			process.Stop()
			return nil, err
		}
		if value.PID != cmd.Process.Pid || !validIdentity(value.EnvironmentID) {
			process.Stop()
			return nil, fmt.Errorf("daemon ready identity does not match the owned process")
		}
		return process, nil
	case err := <-readErr:
		process.Stop()
		return nil, err
	case <-ctx.Done():
		process.Stop()
		return nil, fmt.Errorf("daemon did not become ready: %w", ctx.Err())
	}
}

func checkIsolatedDataDir(path string) error {
	root := os.Getenv("LOCALAPPDATA")
	if root == "" {
		return nil
	}
	production := filepath.Join(root, "Fintwind")
	if resolved, err := filepath.EvalSymlinks(production); err == nil {
		production = resolved
	}
	resolved := resolvedDataPath(path)
	relative, err := filepath.Rel(strings.ToLower(production), strings.ToLower(resolved))
	if err == nil && relative != ".." && !strings.HasPrefix(relative, ".."+string(filepath.Separator)) && !filepath.IsAbs(relative) {
		return fmt.Errorf("the Go migration cannot start a daemon in the production Fintwind data directory")
	}
	return nil
}

// CheckIsolatedDataDir protects the frontend too, including connect-only use.
func CheckIsolatedDataDir(path string) error {
	absolute, err := filepath.Abs(path)
	if err != nil {
		return err
	}
	return checkIsolatedDataDir(absolute)
}

func resolvedDataPath(path string) string {
	ancestor := path
	for {
		if resolved, err := filepath.EvalSymlinks(ancestor); err == nil {
			relative, err := filepath.Rel(ancestor, path)
			if err == nil {
				return filepath.Join(resolved, relative)
			}
			return path
		}
		parent := filepath.Dir(ancestor)
		if parent == ancestor {
			return path
		}
		ancestor = parent
	}
}

// Stop ends the process this client started.
func (p *Process) Stop() {
	if p == nil || p.cmd == nil || p.cmd.Process == nil {
		return
	}
	p.stopOnce.Do(func() {
		_ = p.cmd.Process.Kill()
		_ = p.cmd.Wait()
	})
}

func randomToken() (string, error) {
	buffer := make([]byte, 16)
	if _, err := rand.Read(buffer); err != nil {
		return "", fmt.Errorf("generate daemon token: %w", err)
	}
	return hex.EncodeToString(buffer), nil
}

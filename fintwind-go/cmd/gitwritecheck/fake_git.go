package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/ownedprocess"
)

type fakeGitConfig struct {
	RealGit     string          `json:"realGit"`
	RoundRoot   string          `json:"roundRoot"`
	Mode        string          `json:"mode"`
	TargetCwd   string          `json:"targetCwd"`
	ExitCode    int             `json:"exitCode"`
	Diagnostic  string          `json:"diagnostic"`
	StartedPath string          `json:"startedPath"`
	ReleasePath string          `json:"releasePath"`
	Heartbeat   string          `json:"heartbeatPath"`
	Targets     []fakeGitTarget `json:"targets,omitempty"`
}

type fakeGitTarget struct {
	Cwd         string `json:"cwd"`
	StartedPath string `json:"startedPath"`
	ReleasePath string `json:"releasePath"`
	Heartbeat   string `json:"heartbeatPath"`
}

func runFakeGitWrapper(args []string) int {
	configPath := filepath.Join(filepath.Dir(os.Args[0]), "git-wrapper.json")
	raw, err := os.ReadFile(configPath)
	if err != nil {
		fmt.Fprintln(os.Stderr, "fake Git config unavailable:", err)
		return 125
	}
	var config fakeGitConfig
	if err := json.Unmarshal(raw, &config); err != nil || config.RealGit == "" {
		fmt.Fprintln(os.Stderr, "fake Git config invalid")
		return 125
	}
	cwd, _ := os.Getwd()
	if len(config.Targets) > 0 {
		config.TargetCwd = ""
		for _, target := range config.Targets {
			if strings.EqualFold(filepath.Clean(cwd), filepath.Clean(target.Cwd)) {
				config.TargetCwd = target.Cwd
				config.StartedPath = target.StartedPath
				config.ReleasePath = target.ReleasePath
				config.Heartbeat = target.Heartbeat
				break
			}
		}
	}
	if !strings.EqualFold(filepath.Clean(cwd), filepath.Clean(config.TargetCwd)) || !isGitTopLevelProbe(args) {
		return passThroughGit(config.RealGit, args)
	}
	switch config.Mode {
	case "exit":
		fmt.Fprintln(os.Stderr, config.Diagnostic)
		return config.ExitCode
	case "hold":
		return holdGitProbe(config)
	default:
		return passThroughGit(config.RealGit, args)
	}
}

func isGitTopLevelProbe(args []string) bool {
	i := 0
	for i < len(args) && args[i] == "-c" {
		if i+1 >= len(args) {
			return false
		}
		i += 2
	}
	return len(args)-i == 2 && args[i] == "rev-parse" && args[i+1] == "--show-toplevel"
}

func passThroughGit(binary string, args []string) int {
	cmd := exec.Command(binary, args...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Stdin = os.Stdin
	if err := cmd.Run(); err != nil {
		var exit *exec.ExitError
		if errors.As(err, &exit) {
			return exit.ExitCode()
		}
		fmt.Fprintln(os.Stderr, "fake Git could not start local Git:", err)
		return 127
	}
	return 0
}

func holdGitProbe(config fakeGitConfig) int {
	if config.StartedPath == "" || config.ReleasePath == "" || config.Heartbeat == "" {
		fmt.Fprintln(os.Stderr, "fake Git hold paths are required")
		return 125
	}
	if err := os.WriteFile(config.StartedPath, []byte("started\n"), 0o600); err != nil {
		fmt.Fprintln(os.Stderr, "fake Git could not record probe start:", err)
		return 125
	}
	executable, err := os.Executable()
	if err != nil {
		fmt.Fprintln(os.Stderr, "fake Git executable path unavailable:", err)
		return 125
	}
	logs := filepath.Join(config.RoundRoot, "fake-git-heartbeat-logs")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		fmt.Fprintln(os.Stderr, "fake Git could not create heartbeat log directory:", err)
		return 125
	}
	heartbeat, err := ownedprocess.StartWithEnvironment(executable, []string{"--git-heartbeat", config.Heartbeat}, logs, config.RoundRoot, ownedprocess.BuildEnv(config.RoundRoot, nil))
	if err != nil {
		fmt.Fprintln(os.Stderr, "fake Git could not start owned heartbeat child:", err)
		return 125
	}
	defer heartbeat.Close()
	deadline := time.Now().Add(25 * time.Second)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(config.ReleasePath); err == nil {
			return 0
		}
		time.Sleep(20 * time.Millisecond)
	}
	fmt.Fprintln(os.Stderr, "fake Git hold safety deadline reached")
	return 124
}

func runGitHeartbeat(args []string) int {
	if len(args) != 1 || args[0] == "" {
		return 2
	}
	var tick uint64
	for {
		tick++
		if err := os.WriteFile(args[0], []byte(strconv.FormatUint(tick, 10)), 0o600); err != nil {
			return 1
		}
		time.Sleep(20 * time.Millisecond)
	}
}

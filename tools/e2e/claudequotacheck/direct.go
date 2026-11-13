package main

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"time"

	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
)

func fixedPanelArgs(session string) []string {
	return []string{"--setting-sources", "", "--settings", `{"disableDeepLinkRegistration":"disable"}`, "--strict-mcp-config", "--tools", "", "--permission-mode", "default", "--allowedTools", "", "--no-chrome", "--session-id", session}
}

// directPTYProbe isolates the executable-to-ConPTY boundary from Local.Fetch
// and daemon RPC. The fake exits immediately after recording GetConsoleMode,
// exact argv, profile environment and cwd. The full /usage flow is exercised
// separately through Local.Fetch and the real daemon.
func (h *harness) directPTYProbe() error {
	root := filepath.Join(h.round, "direct-pty")
	home := filepath.Join(root, "home")
	config := filepath.Join(home, ".claude")
	for _, dir := range []string{config, filepath.Join(home, "AppData", "Roaming"), filepath.Join(home, "AppData", "Local"), filepath.Join(root, "tmp")} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
	}
	if err := os.WriteFile(filepath.Join(config, "e2e-seed.json"), []byte(`{"status":"valid","panel":"console-probe"}`), 0o600); err != nil {
		return err
	}
	session := uuid.NewString()
	args := fixedPanelArgs(session)
	env := ownedprocess.BuildEnv(root, map[string]string{
		"PATH": h.bin, "HOME": home, "USERPROFILE": home,
		"APPDATA": filepath.Join(home, "AppData", "Roaming"), "LOCALAPPDATA": filepath.Join(home, "AppData", "Local"),
		"CLAUDE_CONFIG_DIR": config, "TEMP": filepath.Join(root, "tmp"), "TMP": filepath.Join(root, "tmp"),
		"DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1", "DISABLE_AUTOUPDATER": "1", "TERM": "xterm-256color",
	})
	pty, err := ownedprocess.StartPTY(filepath.Join(h.bin, "claude.exe"), args, h.round, h.workspace, env)
	if err != nil {
		h.checks["direct-pty-console-argv"] = false
		return fmt.Errorf("direct StartPTY: %w", err)
	}
	defer pty.Close()
	outputDone := make(chan struct{})
	go func() {
		defer close(outputDone)
		for range pty.Output {
		}
	}()
	deadline := time.Now().Add(10 * time.Second)
	for {
		exited, waitErr := pty.Owned.Exited(50)
		if waitErr != nil {
			return waitErr
		}
		if exited {
			break
		}
		if time.Now().After(deadline) {
			return errors.New("direct fake ConPTY probe did not exit within 10s")
		}
		time.Sleep(25 * time.Millisecond)
	}
	code, err := pty.Owned.ExitCode()
	if err != nil {
		return err
	}
	if err := pty.Close(); err != nil {
		return fmt.Errorf("close direct PTY: %w", err)
	}
	<-outputDone
	calls, err := readCalls(config)
	if err != nil {
		return fmt.Errorf("direct probe has no recorded CLI invocation (exit=%d): %w", code, err)
	}
	var panel *invocation
	for i := range calls {
		if len(calls[i].Args) > 0 && calls[i].Args[0] == "--setting-sources" {
			panel = &calls[i]
		}
	}
	if panel == nil {
		h.checks["direct-pty-console-argv"] = false
		return fmt.Errorf("direct fake did not reach panel entry point (exit=%d, calls=%d)", code, len(calls))
	}
	if !panel.ConsoleIn || !panel.ConsoleOut {
		h.checks["direct-pty-console-argv"] = false
		return fmt.Errorf("direct fake GetConsoleMode failed: stdin=%v(type %d), stdout=%v(type %d), %s", panel.ConsoleIn, panel.StdinType, panel.ConsoleOut, panel.StdoutType, panel.ConsoleError)
	}
	if code != 0 {
		h.checks["direct-pty-console-argv"] = false
		return fmt.Errorf("direct fake exit code %d", code)
	}
	if !reflect.DeepEqual(panel.Args, args) {
		h.checks["direct-pty-console-argv"] = false
		return fmt.Errorf("direct fixed argv mismatch: got %#v want %#v", panel.Args, args)
	}
	if panel.Cwd != h.workspace {
		h.checks["direct-pty-console-argv"] = false
		return fmt.Errorf("direct PTY cwd %q want owned workspace %q", panel.Cwd, h.workspace)
	}
	if panel.Env["CLAUDE_CONFIG_DIR"] != config || panel.Env["HOME"] != home || panel.Env["PATH"] != h.bin {
		h.checks["direct-pty-console-argv"] = false
		return errors.New("direct PTY profile environment mismatch")
	}
	for key := range panel.Env {
		if len(key) >= 11 && key[:11] == "ANTHROPIC_" {
			h.checks["direct-pty-console-argv"] = false
			return fmt.Errorf("direct PTY inherited credential key %s", key)
		}
	}
	h.checks["direct-pty-console-argv"] = true
	h.notes["direct-pty"] = "round-owned claude.exe launched with fixed panel argv through StartPTY; fake GetConsoleMode true for stdin/stdout; argv, UUID, cwd and isolated profile env checked"
	return nil
}

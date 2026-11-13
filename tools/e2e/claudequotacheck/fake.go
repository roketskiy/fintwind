package main

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"golang.org/x/sys/windows"
)

type seed struct {
	Status    string `json:"status"`
	Panel     string `json:"panel"`
	Rotate    bool   `json:"rotate"`
	OverBytes int    `json:"overBytes"`
}

type invocation struct {
	Args         []string          `json:"args"`
	Cwd          string            `json:"cwd"`
	Env          map[string]string `json:"env"`
	ConsoleIn    bool              `json:"consoleIn"`
	ConsoleOut   bool              `json:"consoleOut"`
	StdinType    uint32            `json:"stdinType"`
	StdoutType   uint32            `json:"stdoutType"`
	ConsoleError string            `json:"consoleError,omitempty"`
	Input        string            `json:"input,omitempty"`
	EscapedChild bool              `json:"escapedChild,omitempty"`
}

type consoleModeEvidence struct {
	ConsoleIn          bool   `json:"consoleIn"`
	ConsoleOut         bool   `json:"consoleOut"`
	StdinBefore        uint32 `json:"stdinBefore"`
	StdinAfter         uint32 `json:"stdinAfter"`
	StdoutMode         uint32 `json:"stdoutMode"`
	StdinBeforeError   string `json:"stdinBeforeError,omitempty"`
	StdinAfterError    string `json:"stdinAfterError,omitempty"`
	StdoutModeError    string `json:"stdoutModeError,omitempty"`
	LineInputBefore    bool   `json:"lineInputBefore"`
	LineInputAfter     bool   `json:"lineInputAfter"`
	EchoInputBefore    bool   `json:"echoInputBefore"`
	EchoInputAfter     bool   `json:"echoInputAfter"`
	ProcessedBefore    bool   `json:"processedInputBefore"`
	ProcessedAfter     bool   `json:"processedInputAfter"`
	VirtualInputBefore bool   `json:"virtualTerminalInputBefore"`
	VirtualInputAfter  bool   `json:"virtualTerminalInputAfter"`
}

func fakeMain(args []string) error {
	if len(args) > 0 && args[0] == "fake-child" {
		if len(args) != 3 || args[1] != "--owned" {
			return errors.New("fake child needs a round config path")
		}
		path := filepath.Join(args[2], "child-heartbeat.txt")
		for i := 0; ; i++ {
			_ = os.WriteFile(path, []byte(fmt.Sprint(i)), 0o600)
			time.Sleep(100 * time.Millisecond)
		}
	}
	if len(args) > 0 && args[0] == "--setting-sources" {
		config := os.Getenv("CLAUDE_CONFIG_DIR")
		if config == "" {
			return errors.New("CLAUDE_CONFIG_DIR is missing")
		}
		return fakePanel(config, args)
	}
	if len(args) > 0 && args[0] == "shim-record" {
		config := os.Getenv("CLAUDE_CONFIG_DIR")
		if config == "" {
			return errors.New("shim fixture config missing")
		}
		return appendInvocation(config, invocation{Args: append([]string(nil), args[1:]...), Cwd: mustGetwd(), Env: safeEnv()})
	}
	if len(args) != 3 || args[0] != "auth" || args[1] != "status" || args[2] != "--json" {
		if config := os.Getenv("CLAUDE_CONFIG_DIR"); config != "" {
			_ = appendInvocation(config, invocation{Args: append([]string(nil), args...), Cwd: mustGetwd(), Env: safeEnv()})
		}
		return fmt.Errorf("unexpected fake CLI argv: %q", args)
	}
	config := os.Getenv("CLAUDE_CONFIG_DIR")
	if config == "" {
		return errors.New("CLAUDE_CONFIG_DIR is missing")
	}
	fixture, err := readSeed(config)
	if err != nil {
		return err
	}
	consoleIn, consoleOut := isConsole(os.Stdin), isConsole(os.Stdout)
	call := invocation{Args: append([]string(nil), args...), Cwd: mustGetwd(), Env: safeEnv(), ConsoleIn: consoleIn, ConsoleOut: consoleOut, StdinType: fileType(os.Stdin), StdoutType: fileType(os.Stdout)}
	if err := appendInvocation(config, call); err != nil {
		return err
	}
	switch fixture.Status {
	case "hang":
		spawnOwnedHolder(config)
		for {
			time.Sleep(time.Second)
		}
	case "malformed":
		fmt.Print("{not-json")
	case "false":
		fmt.Print(`{"loggedIn":false}`)
	case "flood":
		_, _ = os.Stdout.Write(make([]byte, 2<<20))
	case "fail":
		fmt.Fprintln(os.Stderr, "private-fake-error-token-do-not-leak")
		os.Exit(7)
	default:
		fmt.Printf("{\"loggedIn\":true,\"email\":\"fixture@example.invalid\",\"subscriptionType\":\"max\"}")
	}
	return nil
}

func readSeed(config string) (seed, error) {
	raw, err := os.ReadFile(filepath.Join(config, "e2e-seed.json"))
	if err != nil {
		return seed{}, fmt.Errorf("fixture seed unavailable: %w", err)
	}
	var value seed
	if err := json.Unmarshal(raw, &value); err != nil {
		return seed{}, err
	}
	return value, nil
}

func isConsole(f *os.File) bool {
	var mode uint32
	return windows.GetConsoleMode(windows.Handle(f.Fd()), &mode) == nil
}

func fileType(f *os.File) uint32 { kind, _ := windows.GetFileType(windows.Handle(f.Fd())); return kind }

func mustGetwd() string { v, _ := os.Getwd(); return v }

func safeEnv() map[string]string {
	out := map[string]string{}
	for _, key := range []string{"HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "CLAUDE_CONFIG_DIR", "TEMP", "TMP", "PATH", "SystemRoot", "WINDIR", "COMSPEC"} {
		if value, ok := os.LookupEnv(key); ok {
			out[key] = value
		}
	}
	for _, pair := range os.Environ() {
		key, _, ok := strings.Cut(pair, "=")
		if ok && strings.HasPrefix(strings.ToUpper(key), "ANTHROPIC_") {
			out["ANTHROPIC_PRESENT:"+strings.ToUpper(key)] = "true"
		}
	}
	return out
}

func appendInvocation(config string, call invocation) error {
	raw, _ := json.Marshal(call)
	file, err := os.OpenFile(filepath.Join(config, "e2e-calls.jsonl"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	defer file.Close()
	_, err = file.Write(append(raw, '\n'))
	return err
}

func spawnOwnedHolder(config string) {
	self, err := os.Executable()
	if err != nil {
		return
	}
	child := exec.Command(self, "fake-child", "--owned", config)
	child.Env = os.Environ()
	child.Stdout, child.Stderr = os.Stdout, os.Stderr
	if child.Start() == nil {
		_ = os.WriteFile(filepath.Join(config, "holder-started.marker"), []byte("owned-child"), 0o600)
	}
}

// fakePanel runs only under the real ConPTY and consumes actual input written
// by backendquota. Console raw mode lets the fake acknowledge the CLI's key
// prompts and receive the literal /usage command without a human or browser.
func fakePanel(config string, args []string) error {
	fixture, err := readSeed(config)
	if err != nil {
		return err
	}
	stdinConsole, stdoutConsole := isConsole(os.Stdin), isConsole(os.Stdout)
	if !stdinConsole || !stdoutConsole {
		stdinErr := windows.GetConsoleMode(windows.Handle(os.Stdin.Fd()), new(uint32))
		stdoutErr := windows.GetConsoleMode(windows.Handle(os.Stdout.Fd()), new(uint32))
		call := invocation{Args: args, Cwd: mustGetwd(), Env: safeEnv(), ConsoleIn: stdinConsole, ConsoleOut: stdoutConsole, StdinType: fileType(os.Stdin), StdoutType: fileType(os.Stdout), ConsoleError: fmt.Sprintf("stdin=%v stdout=%v", stdinErr, stdoutErr)}
		_ = appendInvocation(config, call)
		return fmt.Errorf("fake panel did not receive console handles (stdin=%v type=%d; stdout=%v type=%d)", stdinConsole, call.StdinType, stdoutConsole, call.StdoutType)
	}
	call := invocation{Args: append([]string(nil), args...), Cwd: mustGetwd(), Env: safeEnv(), ConsoleIn: stdinConsole, ConsoleOut: stdoutConsole, StdinType: fileType(os.Stdin), StdoutType: fileType(os.Stdout)}
	var received strings.Builder
	recorded := false
	inputTrace := filepath.Join(config, "e2e-panel-input.bin")
	if err := os.WriteFile(inputTrace, nil, 0o600); err != nil {
		return err
	}
	acceptInput := func(b byte) error {
		received.WriteByte(b)
		return os.WriteFile(inputTrace, []byte(received.String()), 0o600)
	}
	defer func() {
		if !recorded {
			call.Input = received.String()
			_ = appendInvocation(config, call)
		}
	}()
	var mode, stdoutMode uint32
	stdinModeErr := windows.GetConsoleMode(windows.Handle(os.Stdin.Fd()), &mode)
	stdoutModeErr := windows.GetConsoleMode(windows.Handle(os.Stdout.Fd()), &stdoutMode)
	if stdinModeErr != nil {
		return stdinModeErr
	}
	if stdoutModeErr != nil {
		return stdoutModeErr
	}
	const (
		enableProcessedInput       = 0x0001
		enableLineInput            = 0x0002
		enableEchoInput            = 0x0004
		enableVirtualTerminalInput = 0x0200
	)
	modeAfter := (mode &^ (enableProcessedInput | enableLineInput | enableEchoInput)) | enableVirtualTerminalInput
	setModeErr := windows.SetConsoleMode(windows.Handle(os.Stdin.Fd()), modeAfter)
	defer windows.SetConsoleMode(windows.Handle(os.Stdin.Fd()), mode)
	var actualModeAfter uint32
	getModeAfterErr := windows.GetConsoleMode(windows.Handle(os.Stdin.Fd()), &actualModeAfter)
	evidence := consoleModeEvidence{
		ConsoleIn: stdinConsole, ConsoleOut: stdoutConsole,
		StdinBefore: mode, StdinAfter: actualModeAfter, StdoutMode: stdoutMode,
		LineInputBefore: mode&enableLineInput != 0, LineInputAfter: actualModeAfter&enableLineInput != 0,
		EchoInputBefore: mode&enableEchoInput != 0, EchoInputAfter: actualModeAfter&enableEchoInput != 0,
		ProcessedBefore: mode&enableProcessedInput != 0, ProcessedAfter: actualModeAfter&enableProcessedInput != 0,
		VirtualInputBefore: mode&enableVirtualTerminalInput != 0, VirtualInputAfter: actualModeAfter&enableVirtualTerminalInput != 0,
	}
	if setModeErr != nil {
		evidence.StdinAfterError = setModeErr.Error()
	}
	if getModeAfterErr != nil {
		if evidence.StdinAfterError == "" {
			evidence.StdinAfterError = getModeAfterErr.Error()
		} else {
			evidence.StdinAfterError += "; reread: " + getModeAfterErr.Error()
		}
	}
	modeJSON, err := json.MarshalIndent(evidence, "", "  ")
	if err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(config, "e2e-console-modes.json"), modeJSON, 0o600); err != nil {
		return err
	}
	if setModeErr != nil {
		return fmt.Errorf("set fake console input to raw mode: %w", setModeErr)
	}
	if getModeAfterErr != nil {
		return fmt.Errorf("verify fake console raw mode: %w", getModeAfterErr)
	}
	_ = os.WriteFile(filepath.Join(config, "e2e-panel-ready.marker"), []byte("console-ready"), 0o600)
	reader := bufio.NewReader(os.Stdin)
	write := func(s string) { _, _ = os.Stdout.WriteString(s) }
	if fixture.Panel == "console-probe" {
		if err := appendInvocation(config, call); err != nil {
			return err
		}
		recorded = true
		return nil
	}
	key := func(prompt string, expected byte) error {
		write(prompt)
		b, e := reader.ReadByte()
		if e != nil {
			return e
		}
		if err := acceptInput(b); err != nil {
			return err
		}
		if b != expected {
			return fmt.Errorf("unexpected onboarding byte %q", b)
		}
		return nil
	}
	if fixture.Panel == "startup-fail" {
		return errors.New("fake PTY startup failure")
	}
	if err := key("Security notes\r\nPress Enter to continue\r\n", '\r'); err != nil {
		return err
	}
	if err := key("Terminal setup\r\nESC to skip\r\n", 0x1b); err != nil {
		return err
	}
	if err := key("Choose the text style\r\nPress Enter\r\n", '\r'); err != nil {
		return err
	}
	if err := key("Keep browser tools off\r\nPress Escape\r\n", 0x1b); err != nil {
		return err
	}
	cwd := mustGetwd()
	write("Trust this folder? " + cwd + "\r\n❯ No, exit\r\n  Yes, I trust this folder\r\n")
	for _, want := range []byte{0x1b, '[', 'B'} {
		b, e := reader.ReadByte()
		if e != nil {
			return e
		}
		if err := acceptInput(b); err != nil {
			return err
		}
		if b != want {
			return fmt.Errorf("trust must move default No down, got %q", received.String())
		}
	}
	write("\r\nTrust this folder? " + cwd + "\r\n  No, exit\r\n❯ Yes, I trust this folder\r\n")
	b, e := reader.ReadByte()
	if e != nil {
		return e
	}
	if err := acceptInput(b); err != nil {
		return err
	}
	if b != '\r' {
		return fmt.Errorf("trust confirmation expected Enter after Down, got %q", b)
	}
	if !strings.Contains(mustGetwd(), "fintwind-claude-quota") {
		return errors.New("fake CLI is not running in the isolated owned workspace")
	}
	write("Welcome\r\n❯ ")
	var usage strings.Builder
	for {
		b, e := reader.ReadByte()
		if e != nil {
			return e
		}
		if err := acceptInput(b); err != nil {
			return err
		}
		if b == '\r' {
			break
		}
		if usage.Len() < 64 {
			usage.WriteByte(b)
		}
	}
	call.Input = received.String()
	if err := appendInvocation(config, call); err != nil {
		return err
	}
	recorded = true
	if usage.String() != "/usage" {
		return fmt.Errorf("wanted /usage input, received %q", usage.String())
	}
	if fixture.Panel == "hang" {
		spawnOwnedHolder(config)
		for {
			time.Sleep(time.Second)
		}
	}
	if fixture.OverBytes > 0 {
		_, _ = os.Stdout.Write(bytesOf(fixture.OverBytes))
		return waitPanelClose(reader)
	}
	switch fixture.Panel {
	case "denied":
		write("\r\nUsage unavailable: 403 forbidden\r\n")
	case "cost-only":
		write("\r\nCurrent session cost $0.12\r\n")
	case "unknown":
		write("\r\nNew allowance surface\r\nNo known quota window\r\n")
	case "unknown-reset":
		write("\x1b[2J\x1b[H")
		weeklyReset := time.Now().UTC().Add(72 * time.Hour).Format("Jan 2 at 3:04pm (UTC)")
		write("Current session: 42% used Resets after the next blue moon (lunar cycle)\r\n")
		write(fmt.Sprintf("Current week (all models): 62%% used Resets %s\r\n", weeklyReset))
		write(fmt.Sprintf("Current week (Sonnet only): 25%% used Resets %s\r\n", weeklyReset))
		write("Using your overages\r\n")
	default:
		// Start the fake quota panel from a known physical screen origin. The
		// preceding onboarding prompts intentionally occupy multiple rows.
		write("\x1b[2J\x1b[H")
		sessionReset := time.Now().UTC().Add(time.Hour).Format("Jan 2 at 3:04pm (UTC)")
		weeklyReset := time.Now().UTC().Add(72 * time.Hour).Format("Jan 2 at 3:04pm (UTC)")
		write(fmt.Sprintf("Current session: 81%% used Resets %s\r\n", sessionReset))
		write(fmt.Sprintf("Current week (all models): 62%% used Resets %s\r\n", weeklyReset))
		write(fmt.Sprintf("Current week (Sonnet only): 25%% used Resets %s\r\n", weeklyReset))
		write("Using your overages\r\n")
		if fixture.Panel == "quota-redraw-probe" {
			continuePath := filepath.Join(config, "e2e-redraw-continue.marker")
			deadline := time.Now().Add(15 * time.Second)
			for {
				if _, err := os.Stat(continuePath); err == nil {
					break
				} else if !errors.Is(err, os.ErrNotExist) {
					return fmt.Errorf("stat redraw-probe continuation: %w", err)
				}
				if time.Now().After(deadline) {
					return errors.New("redraw-probe timed out waiting for initial physical-row capture")
				}
				time.Sleep(10 * time.Millisecond)
			}
		}
		// Replace the first line, not append to it: the screen projection must
		// discard the stale 81% value and expose only the final 42% reading.
		write(fmt.Sprintf("\x1b[1;1H\x1b[2KCurrent session: 42%% used Resets %s\r\n", sessionReset))
		if fixture.Rotate {
			credential := filepath.Join(config, ".credentials.json")
			_ = os.WriteFile(credential, []byte(`{"fixture":"rotated"}`), 0o600)
		}
	}
	return waitPanelClose(reader)
}

// Real Claude keeps its panel process alive after drawing /usage. Stay in the
// round-owned ConPTY until the parent closes/cancels it, rather than racing the
// product's output polling with an immediate fake-process exit.
func waitPanelClose(reader *bufio.Reader) error {
	for {
		if _, err := reader.ReadByte(); err != nil {
			if errors.Is(err, io.EOF) {
				return nil
			}
			return err
		}
	}
}

func bytesOf(n int) []byte {
	if n < 0 {
		n = 0
	}
	if n > 2<<20 {
		n = 2 << 20
	}
	return make([]byte, n)
}

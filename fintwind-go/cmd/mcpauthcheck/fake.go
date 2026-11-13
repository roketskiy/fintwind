package main

// Fake `mcp auth` CLI. The daemon spawns this executable with argv
// ["mcp", "auth", name]; behavior is keyed by server name through the round
// seed found via OPENCODE_TEST_HOME (which the product always sets for the
// child). The fixture never opens anything and, at the daemon layer, never
// prints an https URL.

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

func fakeRoot() (string, error) {
	home := os.Getenv("OPENCODE_TEST_HOME")
	if home == "" {
		return "", errors.New("MCP test fixture requires OPENCODE_TEST_HOME")
	}
	return filepath.Dir(home), nil
}

type fakeCall struct {
	Name string            `json:"name"`
	Argv []string          `json:"argv"`
	Cwd  string            `json:"cwd"`
	Env  map[string]string `json:"env"`
}

var fakeEnvKeys = []string{
	"OPENCODE_CONFIG_DIR", "OPENCODE_DB", "OPENCODE_TEST_HOME", "OPENCODE_DISABLE_PROJECT_CONFIG",
	"HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP",
	"PATH", "SystemRoot", "WINDIR", "COMSPEC",
	"HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY",
	"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "OPENCODE_API_KEY",
}

func (h *harness) writeSeed(seed any) error {
	raw, err := json.Marshal(seed)
	if err != nil {
		return err
	}
	return os.WriteFile(h.seedPath(), raw, 0o600)
}

func (h *harness) seedPath() string { return filepath.Join(h.round, "mcp-auth-seed.json") }

func (h *harness) readCalls() []fakeCall {
	raw, err := os.ReadFile(filepath.Join(h.round, "mcp-auth-calls.jsonl"))
	if err != nil {
		return nil
	}
	var out []fakeCall
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		var call fakeCall
		if json.Unmarshal([]byte(line), &call) == nil {
			out = append(out, call)
		}
	}
	return out
}

func fakeBehavior(root, name string) string {
	raw, err := os.ReadFile(filepath.Join(root, "mcp-auth-seed.json"))
	if err != nil {
		return "silent-ok"
	}
	var seed struct {
		Behaviors map[string]string `json:"behaviors"`
	}
	if json.Unmarshal(raw, &seed) != nil || seed.Behaviors == nil {
		return "silent-ok"
	}
	if mode, ok := seed.Behaviors[name]; ok {
		return mode
	}
	return "silent-ok"
}

func logFakeCall(root, name string) {
	cwd, _ := os.Getwd()
	// Case-insensitive match: Windows preserves the caller's original case.
	actual := map[string]string{}
	for _, kv := range os.Environ() {
		if index := strings.IndexByte(kv, '='); index > 0 {
			actual[strings.ToUpper(kv[:index])] = kv[index+1:]
		}
	}
	env := map[string]string{}
	for _, key := range fakeEnvKeys {
		if value, ok := actual[strings.ToUpper(key)]; ok {
			env[key] = value
		}
	}
	raw, _ := json.Marshal(fakeCall{Name: name, Argv: append([]string{"mcp"}, os.Args[2:]...), Cwd: cwd, Env: env})
	file, err := os.OpenFile(filepath.Join(root, "mcp-auth-calls.jsonl"), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return
	}
	defer file.Close()
	raw = append(raw, '\n')
	_, _ = file.Write(raw)
}

func fakeMain(args []string) error {
	if len(args) != 2 || args[0] != "auth" {
		return errors.New("fake mcp auth usage: mcp auth <name>")
	}
	name := args[1]
	root, err := fakeRoot()
	if err != nil {
		fmt.Fprintln(os.Stderr, err.Error())
		os.Exit(2)
	}
	logFakeCall(root, name)
	switch fakeBehavior(root, name) {
	case "text-ok":
		fmt.Println("mcp auth starting")
		fmt.Println("ready")
		return nil
	case "fail":
		fmt.Println("starting")
		fmt.Fprintln(os.Stderr, "boom: explode")
		os.Exit(3)
	case "hang":
		for range 3000 {
			time.Sleep(100 * time.Millisecond)
		}
		return nil
	case "holder":
		self, err := os.Executable()
		if err != nil {
			fmt.Fprintln(os.Stderr, err.Error())
			os.Exit(2)
		}
		child := exec.Command(self, "mcp-fake-hold", root, name)
		child.Env = os.Environ()
		child.Stdout = os.Stdout
		child.Stderr = os.Stderr
		if err := child.Start(); err != nil {
			fmt.Fprintln(os.Stderr, err.Error())
			os.Exit(2)
		}
		return nil
	case "holder-hang":
		self, err := os.Executable()
		if err != nil {
			fmt.Fprintln(os.Stderr, err.Error())
			os.Exit(2)
		}
		child := exec.Command(self, "mcp-fake-hold", root, name)
		child.Env = os.Environ()
		child.Stdout = os.Stdout
		child.Stderr = os.Stderr
		if err := child.Start(); err != nil {
			fmt.Fprintln(os.Stderr, err.Error())
			os.Exit(2)
		}
		for range 36000 {
			time.Sleep(100 * time.Millisecond)
		}
		return nil
	case "flood":
		chunk := bytes.Repeat([]byte("x"), 64<<10)
		for range 24 {
			if _, err := os.Stdout.Write(chunk); err != nil {
				break
			}
		}
		return nil
	case "url":
		fmt.Println("Visit https://auth.example.test/start?x=1 to continue")
		return nil
	case "multi":
		fmt.Println("first https://auth.example.test/first open this")
		fmt.Println("second https://auth.example.test/second ignored")
		return nil
	case "stderr-url":
		fmt.Fprintln(os.Stderr, "only https://auth.example.test/stderr here")
		return nil
	case "cross":
		fmt.Println("out https://auth.example.test/out")
		fmt.Fprintln(os.Stderr, "err https://auth.example.test/err")
		time.Sleep(300 * time.Millisecond)
		return nil
	case "ansi":
		fmt.Println("\x1b[36mhttps://auth.example.test/ansi\x1b[0m open it")
		return nil
	case "osc8":
		fmt.Println("\x1b]8;;https://auth.example.test/real\x07https://decoy.example.test/text\x1b]8;;\x07 open it")
		return nil
	case "userinfo":
		fmt.Println("skip https://user:pass@auth.example.test/x please")
		return nil
	case "nonhttps":
		fmt.Println("skip http://auth.example.test/x please")
		return nil
	case "trailing":
		fmt.Print("finish https://auth.example.test/trailing-now")
		return nil
	case "giant":
		fmt.Println(strings.Repeat("A", 70<<10))
		fmt.Println("after giant https://auth.example.test/after-giant here")
		return nil
	default: // silent-ok
		return nil
	}
	return nil
}

func holdMain(args []string) {
	if len(args) != 2 {
		os.Exit(2)
	}
	marker := filepath.Join(args[0], "holder-ready-"+args[1]+".json")
	if err := os.WriteFile(marker, []byte(`{"ready":true}`), 0o600); err != nil {
		os.Exit(2)
	}
	for range 36000 {
		time.Sleep(100 * time.Millisecond)
	}
}

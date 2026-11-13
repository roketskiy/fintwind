package main

// Isolated bun/package fixture. One binary plays three roles by executable
// name: bun (records install args/env, copies itself as the managed
// opencode plus version file and package.json), opencode (--version from
// the sibling version file, or a minimal serve fixture for pool engines).

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"
)

type fakeCall struct {
	Method string            `json:"method"`
	Path   string            `json:"path"`
	Body   json.RawMessage   `json:"body"`
	Env    map[string]string `json:"env,omitempty"`
}

func fixturePath(name string) string {
	cwd, _ := os.Getwd()
	return filepath.Join(cwd, name)
}

func logFakeCall(method, path string, body json.RawMessage, env map[string]string) {
	file, err := os.OpenFile(fixturePath("fixture-calls.jsonl"), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return
	}
	defer file.Close()
	raw, _ := json.Marshal(fakeCall{Method: method, Path: path, Body: body, Env: env})
	raw = append(raw, '\n')
	_, _ = file.Write(raw)
}

func bunEnv() map[string]string {
	env := map[string]string{}
	for _, key := range []string{"BUN_INSTALL", "BUN_INSTALL_CACHE_DIR", "HOME", "HTTP_PROXY", "HTTPS_PROXY"} {
		env[key] = os.Getenv(key)
	}
	return env
}

func parseRequestedVersion(args []string) string {
	for _, arg := range args {
		if index := strings.LastIndex(arg, "@opencode/cli@"); index >= 0 {
			return arg[index+len("@opencode/cli@"):]
		}
	}
	return ""
}

func runBunInstaller(args []string) error {
	body, _ := json.Marshal(args)
	logFakeCall("bun", "install", body, bunEnv())
	version := parseRequestedVersion(args)
	if version == "" {
		fmt.Fprintln(os.Stderr, "missing @opencode/cli@version")
		os.Exit(2)
	}
	if _, err := os.Stat(fixturePath("fixture-slow-install")); err == nil {
		time.Sleep(35 * time.Second)
	} else {
		time.Sleep(2 * time.Second)
	}
	if _, err := os.Stat(fixturePath("fixture-fail-install")); err == nil {
		fmt.Fprintln(os.Stderr, "injected installer failure")
		os.Exit(3)
	}
	home := os.Getenv("BUN_INSTALL")
	if home == "" || !filepath.IsAbs(home) {
		fmt.Fprintln(os.Stderr, "BUN_INSTALL missing")
		os.Exit(2)
	}
	self, err := os.Executable()
	if err != nil {
		fmt.Fprintln(os.Stderr, "self path unavailable")
		os.Exit(2)
	}
	binDir := filepath.Join(home, "bin")
	if err := os.MkdirAll(binDir, 0o700); err != nil {
		fmt.Fprintln(os.Stderr, "bin dir unavailable")
		os.Exit(2)
	}
	name := "opencode.exe"
	installed := filepath.Join(binDir, name)
	raw, err := os.ReadFile(self)
	if err != nil {
		fmt.Fprintln(os.Stderr, "self copy failed")
		os.Exit(2)
	}
	if err := os.WriteFile(installed, raw, 0o700); err != nil {
		fmt.Fprintln(os.Stderr, "install copy failed")
		os.Exit(2)
	}
	stored := version
	if _, err := os.Stat(fixturePath("fixture-wrong-version")); err == nil {
		stored = "0.0.0"
	}
	if err := os.WriteFile(filepath.Join(binDir, "opencode.version"), []byte(stored+"\n"), 0o600); err != nil {
		fmt.Fprintln(os.Stderr, "version record failed")
		os.Exit(2)
	}
	packageDir := filepath.Join(home, "install", "global", "node_modules", "@opencode", "cli")
	if err := os.MkdirAll(packageDir, 0o700); err != nil {
		fmt.Fprintln(os.Stderr, "package dir failed")
		os.Exit(2)
	}
	packageJSON, _ := json.Marshal(map[string]any{"name": "@opencode/cli", "version": stored})
	if err := os.WriteFile(filepath.Join(packageDir, "package.json"), packageJSON, 0o600); err != nil {
		fmt.Fprintln(os.Stderr, "package manifest failed")
		os.Exit(2)
	}
	return nil
}

func runOpencode(args []string) error {
	for _, arg := range args {
		if arg == "--version" {
			self, err := os.Executable()
			if err != nil {
				return err
			}
			raw, err := os.ReadFile(filepath.Join(filepath.Dir(self), "opencode.version"))
			if err != nil {
				return err
			}
			fmt.Printf("opencode v%s", strings.TrimSpace(string(raw)))
			return nil
		}
	}
	return errors.New("managed opencode fixture supports --version and serve only")
}

func runServeFixture(args []string) error {
	password := os.Getenv("OPENCODE_SERVER_PASSWORD")
	if password == "" || os.Getenv("OPENCODE_TEST_HOME") == "" {
		return errors.New("isolated fixture environment required")
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	defer listener.Close()
	server := &http.Server{ReadHeaderTimeout: 5 * time.Second, Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		user, key, ok := r.BasicAuth()
		if !ok || user != "opencode" || key != password {
			http.Error(w, "unauthorized", 401)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		if r.URL.Path == "/api/event" && r.Method == "GET" {
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(200)
			w.(http.Flusher).Flush()
			<-r.Context().Done()
			return
		}
		switch {
		case r.URL.Path == "/api/info":
			_, _ = w.Write([]byte(`{"data":{}}`))
			return
		case r.URL.Path == "/api/model":
			_, _ = w.Write([]byte(`{"data":[]}`))
			return
		case r.Method == "POST" && r.URL.Path == "/api/session":
			id := fmt.Sprintf("ses_pool_%d", time.Now().UnixNano())
			raw, _ := json.Marshal(map[string]any{"data": map[string]any{"id": id}})
			_, _ = w.Write(raw)
			return
		case r.Method == "POST" && (strings.HasSuffix(r.URL.Path, "/agent") || strings.HasSuffix(r.URL.Path, "/model")):
			_, _ = w.Write([]byte(`{}`))
			return
		}
		http.Error(w, "unexpected operation", 400)
	})}
	deadline, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()
	go func() { <-deadline.Done(); _ = server.Close() }()
	fmt.Println("http://" + listener.Addr().String())
	err = server.Serve(listener)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

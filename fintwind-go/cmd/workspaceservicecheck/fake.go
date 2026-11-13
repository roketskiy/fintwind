package main

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"
)

var fakeLogMu sync.Mutex
var fakeCommandPoll int

func runFakeOpenCode() error {
	home := os.Getenv("OPENCODE_TEST_HOME")
	if !filepath.IsAbs(home) {
		return errors.New("isolated fake home missing")
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	server := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		username, password, ok := r.BasicAuth()
		if !ok || username != "opencode" || password == "" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		body, readErr := readRequestBody(r)
		if readErr != nil {
			http.Error(w, "request body invalid", http.StatusBadRequest)
			return
		}
		if err := appendJSONLine(filepath.Join(home, "fake-opencode-requests.jsonl"), openCodeRecord{
			Method: r.Method, Path: r.URL.RequestURI(), Directory: r.Header.Get("x-opencode-directory"), Body: body,
		}); err != nil {
			http.Error(w, "fixture log failed", http.StatusInternalServerError)
			return
		}
		config, err := readFakeConfig(home)
		if err != nil {
			http.Error(w, "fixture config missing", http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/api/info":
			writeJSON(w, http.StatusOK, map[string]any{"data": map[string]any{"version": "fixture-v2"}})
		case r.Method == http.MethodGet && r.URL.Path == "/api/config":
			writeJSON(w, http.StatusOK, map[string]any{"data": map[string]any{"location": "loaded"}})
		case r.Method == http.MethodGet && r.URL.Path == "/api/command":
			commands := pollCommands(config.CommandSequence)
			writeJSON(w, http.StatusOK, map[string]any{"data": commands})
		case r.Method == http.MethodGet && r.URL.Path == "/api/skill":
			switch config.SkillsMode {
			case "error":
				http.Error(w, "fixture skills unavailable", http.StatusServiceUnavailable)
			case "empty":
				writeJSON(w, http.StatusOK, map[string]any{"data": []any{}})
			default:
				writeJSON(w, http.StatusOK, map[string]any{"data": config.Skills})
			}
		case r.Method == http.MethodGet && r.URL.Path == "/api/model/default":
			switch config.DefaultModelMode {
			case "error":
				http.Error(w, "fixture default model unavailable", http.StatusServiceUnavailable)
			case "null":
				writeJSON(w, http.StatusOK, map[string]any{"data": nil})
			default:
				writeJSON(w, http.StatusOK, map[string]any{"data": config.DefaultModel})
			}
		case r.Method == http.MethodPost && r.URL.Path == "/api/experimental/generate/text":
			if config.GenerationDelay > 0 {
				time.Sleep(time.Duration(config.GenerationDelay) * time.Millisecond)
			}
			if config.GenerationError {
				http.Error(w, "fixture generation failure", http.StatusBadGateway)
				return
			}
			writeJSON(w, http.StatusOK, map[string]any{"data": map[string]any{"text": config.GenerationText}})
		default:
			http.Error(w, "unexpected fake OpenCode route", http.StatusNotFound)
		}
	})}
	go func() { _ = server.Serve(listener) }()
	fmt.Printf("http://%s\n", listener.Addr().String())
	select {}
}

func pollCommands(sequence [][]map[string]any) []map[string]any {
	fakeLogMu.Lock()
	defer fakeLogMu.Unlock()
	index := fakeCommandPoll
	fakeCommandPoll++
	if len(sequence) == 0 {
		return []map[string]any{}
	}
	if index >= len(sequence) {
		index = len(sequence) - 1
	}
	return sequence[index]
}

func readRequestBody(r *http.Request) (json.RawMessage, error) {
	defer r.Body.Close()
	data, err := io.ReadAll(io.LimitReader(r.Body, (2<<20)+1))
	if err != nil {
		return nil, err
	}
	if len(data) > 2<<20 {
		return nil, errors.New("request exceeds fixture body limit")
	}
	if len(data) == 0 {
		return nil, nil
	}
	if !json.Valid(data) {
		return nil, errors.New("non-JSON body")
	}
	return json.RawMessage(data), nil
}

func runFakeGitHub(args []string) int {
	exe, err := os.Executable()
	if err != nil {
		fmt.Fprintln(os.Stderr, "fake gh could not resolve fixture executable")
		return 2
	}
	round := filepath.Dir(filepath.Dir(exe))
	config, err := readFakeConfig(filepath.Join(round, "home"))
	if err != nil {
		fmt.Fprintln(os.Stderr, "fake gh fixture unavailable")
		return 2
	}
	cwd, _ := os.Getwd()
	configDir := os.Getenv("GH_CONFIG_DIR")
	home := os.Getenv("HOME")
	if home == "" {
		home = os.Getenv("USERPROFILE")
	}
	envKeys := make([]string, 0)
	for _, entry := range os.Environ() {
		key, _, ok := strings.Cut(entry, "=")
		if ok {
			envKeys = append(envKeys, key)
		}
	}
	sort.Strings(envKeys)
	forbidden := false
	for _, key := range envKeys {
		switch strings.ToUpper(key) {
		case "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN", "DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN":
			forbidden = true
		}
	}
	cleanDir := filepath.IsAbs(configDir) && filepath.IsAbs(home) && samePath(configDir, home) && strings.HasPrefix(strings.ToLower(filepath.Clean(configDir)), strings.ToLower(filepath.Join(round, "tmp"))+string(os.PathSeparator))
	if cleanDir {
		entries, readErr := os.ReadDir(configDir)
		cleanDir = readErr == nil && len(entries) == 0
	}
	record := fakeGithubRecord{
		Args: append([]string(nil), args...), Cwd: cwd,
		GHTokenMatchesFixture: os.Getenv("GH_TOKEN") == fakeGithubToken,
		GHConfigDir:           configDir, HomeIsIsolated: cleanDir,
		ForbiddenCredential: forbidden, EnvKeys: envKeys,
	}
	if err := appendJSONLine(filepath.Join(round, "fake-gh-records.jsonl"), record); err != nil {
		fmt.Fprintln(os.Stderr, "fake gh could not record invocation")
		return 2
	}
	if config.GithubDelay > 0 {
		time.Sleep(time.Duration(config.GithubDelay) * time.Millisecond)
	}
	if config.GithubStderr != "" {
		fmt.Fprintln(os.Stderr, config.GithubStderr)
	}
	if config.GithubExit != 0 {
		return config.GithubExit
	}
	raw, err := json.Marshal(config.GithubRows)
	if err != nil {
		fmt.Fprintln(os.Stderr, "fake gh response could not encode")
		return 2
	}
	_, _ = os.Stdout.Write(append(raw, '\n'))
	return 0
}

func readFakeConfig(home string) (fakeConfig, error) {
	raw, err := os.ReadFile(filepath.Join(home, "workspace-services-fixture.json"))
	if err != nil {
		return fakeConfig{}, err
	}
	var config fakeConfig
	err = json.Unmarshal(raw, &config)
	return config, err
}

func appendJSONLine(path string, value any) error {
	raw, err := json.Marshal(value)
	if err != nil {
		return err
	}
	fakeLogMu.Lock()
	defer fakeLogMu.Unlock()
	file, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	if _, err = file.Write(append(raw, '\n')); err != nil {
		_ = file.Close()
		return err
	}
	return file.Close()
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	raw, err := json.Marshal(value)
	if err != nil {
		http.Error(w, "fixture response encode failed", http.StatusInternalServerError)
		return
	}
	w.WriteHeader(status)
	_, _ = w.Write(raw)
}

func readFakeGithubRecords(round string) ([]fakeGithubRecord, error) {
	file, err := os.Open(filepath.Join(round, "fake-gh-records.jsonl"))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	defer file.Close()
	var records []fakeGithubRecord
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 4096), 1<<20)
	for scanner.Scan() {
		var record fakeGithubRecord
		if err := json.Unmarshal(scanner.Bytes(), &record); err != nil {
			return nil, err
		}
		records = append(records, record)
	}
	return records, scanner.Err()
}

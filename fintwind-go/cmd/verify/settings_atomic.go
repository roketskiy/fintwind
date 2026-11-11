package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"sync"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"
)

func runAtomicSettings(ctx context.Context, executable, round string) []caseResult {
	const scope = "realDaemonAtomicSettings"
	cases := []caseResult{}
	check := func(name string, expected, actual any) bool {
		status := "pass"
		if !reflect.DeepEqual(expected, actual) {
			status = "fail"
		}
		cases = append(cases, caseResult{Name: name, Scope: scope, Status: status, Detail: fmt.Sprintf("expected %v; observed %v", expected, actual)})
		return status == "pass"
	}
	data := filepath.Join(round, "atomic-settings-data")
	if err := os.MkdirAll(data, 0o700); err != nil {
		check("create isolated data", "success", err.Error())
		return cases
	}
	seed := []byte(`{"future_atomic":{"integer":9007199254740993},"auto_compact_enabled":false,"auto_compact_threshold_tokens":500000}`)
	if err := os.WriteFile(filepath.Join(data, "daemon.json"), seed, 0o600); err != nil {
		check("seed isolated settings", "success", err.Error())
		return cases
	}
	token, err := randomToken()
	if err != nil {
		check("random authentication", "success", err.Error())
		return cases
	}
	start := func(name string) (*ownedprocess.Owned, *daemon.Client, string, error) {
		dir := filepath.Join(round, name)
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return nil, nil, "", err
		}
		owned, err := ownedprocess.Start(executable, []string{"--bind", "127.0.0.1:0", "--data-dir", data, "--parent-pid", strconv.Itoa(os.Getpid())}, dir, map[string]string{"FINTWIND_DAEMON_TOKEN": token})
		if err != nil {
			return nil, nil, "", err
		}
		ready, err := waitReadyLine(ctx, owned.StdoutPath, 15*time.Second)
		if err != nil {
			owned.Close()
			return nil, nil, "", err
		}
		if ready.PID != int(owned.PID) || ready.ProtocolVersion != daemon.ProtocolVersion {
			owned.Close()
			return nil, nil, "", fmt.Errorf("owned readiness identity mismatch")
		}
		client, err := daemon.Dial(ctx, ready.Address, token)
		if err != nil {
			owned.Close()
			return nil, nil, "", err
		}
		return owned, client, ready.Address, nil
	}
	owned, first, address, err := start("atomic-first")
	if err != nil {
		check("start owned real daemon", "success", err.Error())
		return cases
	}
	defer owned.Close()
	defer first.Close()
	second, err := daemon.Dial(ctx, address, token)
	if err != nil {
		check("second independent client", "success", err.Error())
		return cases
	}
	defer second.Close()
	request := func(client *daemon.Client, command any) (map[string]json.RawMessage, error) {
		call, cancel := context.WithTimeout(ctx, 8*time.Second)
		defer cancel()
		payload, err := client.Request(call, command)
		if err != nil {
			return nil, err
		}
		var result struct {
			Type     string                     `json:"type"`
			Settings map[string]json.RawMessage `json:"settings"`
		}
		if json.Unmarshal(payload, &result) != nil || result.Type != "settings" || result.Settings == nil {
			return nil, fmt.Errorf("unexpected settings response")
		}
		return result.Settings, nil
	}
	update := func(client *daemon.Client, key string, value any) (map[string]json.RawMessage, error) {
		return request(client, map[string]any{"type": "updateSetting", "key": key, "value": value})
	}
	_, err = request(first, map[string]any{"type": "getSettings"})
	if err != nil {
		check("first stale settings read", "success", err.Error())
		return cases
	}
	_, err = update(second, "auto_compact_enabled", true)
	if err != nil {
		check("second client update", "success", err.Error())
		return cases
	}
	settings, err := update(first, "auto_compact_threshold_tokens", uint64(12345))
	if err != nil {
		check("first client update after stale read", "success", err.Error())
		return cases
	}
	check("stale reader cannot overwrite another key", "true", string(settings["auto_compact_enabled"]))
	check("changed threshold returned from server", "12345", string(settings["auto_compact_threshold_tokens"]))
	check("unknown large integer retained", `{"integer":9007199254740993}`, string(settings["future_atomic"]))
	var wg sync.WaitGroup
	errors := make(chan error, 2)
	wg.Add(2)
	go func() {
		defer wg.Done()
		for i := 0; i < 24; i++ {
			if _, err := update(first, "auto_compact_enabled", i == 23); err != nil {
				errors <- err
				return
			}
		}
	}()
	go func() {
		defer wg.Done()
		for i := 0; i < 24; i++ {
			if _, err := update(second, "auto_compact_threshold_tokens", uint64(20000+i)); err != nil {
				errors <- err
				return
			}
		}
	}()
	wg.Wait()
	close(errors)
	for err := range errors {
		check("parallel setting writes", "success", err.Error())
		return cases
	}
	settings, err = request(first, map[string]any{"type": "getSettings"})
	if err != nil {
		check("read parallel results", "success", err.Error())
		return cases
	}
	check("parallel disjoint updates both survive", []string{"true", "20023"}, []string{string(settings["auto_compact_enabled"]), string(settings["auto_compact_threshold_tokens"])})
	before, _ := os.ReadFile(filepath.Join(data, "daemon.json"))
	for _, invalid := range []struct {
		key   string
		value any
	}{
		{"auto_compact_enabled", "false"}, {"auto_compact_threshold_tokens", 9999}, {"general_workspace_path", "relative-path"}, {"opencode_binary_path", filepath.Join(round, "missing.exe")},
	} {
		_, err := update(first, invalid.key, invalid.value)
		after, _ := os.ReadFile(filepath.Join(data, "daemon.json"))
		check("invalid value preserves file "+invalid.key, true, err != nil && string(before) == string(after))
	}
	root := filepath.Join(round, "workspace-chosen")
	settings, err = update(first, "general_workspace_path", root)
	if err != nil {
		check("atomic workspace path", "success", err.Error())
		return cases
	}
	_, statErr := os.Stat(root)
	check("workspace path prepared before confirmation", true, statErr == nil)
	settings, err = update(first, "general_workspace_path", nil)
	if err != nil {
		check("clear workspace path", "success", err.Error())
		return cases
	}
	_, exists := settings["general_workspace_path"]
	check("null clears only selected key", false, exists)
	check("clear keeps unrelated setting", "true", string(settings["auto_compact_enabled"]))
	first.Close()
	second.Close()
	if err := owned.Close(); err != nil {
		check("close only owned daemon", "success", err.Error())
		return cases
	}
	restarted, client, _, err := start("atomic-restart")
	if err != nil {
		check("restart isolated daemon", "success", err.Error())
		return cases
	}
	defer restarted.Close()
	defer client.Close()
	settings, err = request(client, map[string]any{"type": "getSettings"})
	if err != nil {
		check("restart reads persisted values", "success", err.Error())
		return cases
	}
	check("restart preserves all atomic changes", []string{"true", "20023", `{"integer":9007199254740993}`}, []string{string(settings["auto_compact_enabled"]), string(settings["auto_compact_threshold_tokens"]), string(settings["future_atomic"])})
	return cases
}

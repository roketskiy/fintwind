// compositioncheck exercises the real OpenCode context hook plugin: it starts
// an owned engine with the fintwind.context plugin materialized, dispatches a
// prompt without provider credentials, and asserts the plugin wrote the
// composition snapshot while the request was assembled.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendengine"

	"github.com/google/uuid"
)

func main() {
	binary := flag.String("opencode", "", "absolute OpenCode binary (default: lookup in PATH)")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()
	if err := run(ctx, *binary); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
	fmt.Println("PASS: real OpenCode wrote the context composition snapshot")
}

func run(ctx context.Context, binary string) error {
	if binary == "" {
		found, err := exec.LookPath("opencode")
		if err != nil {
			return errors.New("OpenCode binary not found in PATH; pass --opencode")
		}
		binary = found
	}
	if !filepath.IsAbs(binary) {
		absolute, err := filepath.Abs(binary)
		if err != nil {
			return err
		}
		binary = absolute
	}
	root, err := os.MkdirTemp("", "fintwind-composition-")
	if err != nil {
		return err
	}
	keep := false
	defer func() {
		if !keep {
			os.RemoveAll(root)
		} else {
			fmt.Println("[ROUND]", root)
		}
	}()
	workspace := filepath.Join(root, "workspace")
	if err := os.MkdirAll(workspace, 0o700); err != nil {
		return err
	}
	process, err := backendengine.Start(ctx, binary, root, workspace, filepath.Join(root, "settings.json"), false)
	if err != nil {
		return fmt.Errorf("engine start: %w", err)
	}
	defer process.Close()
	native, err := process.Client.CreateSession(ctx)
	if err != nil {
		return fmt.Errorf("create session: %w", err)
	}
	// No provider credentials are configured: dispatch is expected to fail at
	// the provider, but the context hook runs while the request is assembled,
	// before any provider traffic.
	_, _ = process.Client.Prompt(ctx, native, "msg_"+strings.ReplaceAll(uuid.NewString(), "-", ""), "Hello", nil)
	snapshot := filepath.Join(root, "data", "context", native+".json")
	deadline := time.Now().Add(20 * time.Second)
	for {
		raw, readErr := os.ReadFile(snapshot)
		if readErr == nil {
			return validate(raw, snapshot)
		}
		if time.Now().After(deadline) {
			keep = true
			return fmt.Errorf("composition snapshot never appeared: %w", readErr)
		}
		select {
		case <-ctx.Done():
			keep = true
			return ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
}

func validate(raw []byte, path string) error {
	var snapshot struct {
		Version      uint32 `json:"version"`
		Model        string `json:"model"`
		Agent        string `json:"agent"`
		System       uint64 `json:"system"`
		Tools        uint64 `json:"tools"`
		Messages     uint64 `json:"messages"`
		Reasoning    uint64 `json:"reasoning"`
		ToolCalls    uint64 `json:"toolCalls"`
		Files        uint64 `json:"files"`
		ToolResults  uint64 `json:"toolResults"`
		Memory       uint64 `json:"memory"`
		OpaqueReason bool   `json:"opaqueReasoning"`
		OpaqueMemory bool   `json:"opaqueMemory"`
	}
	if json.Unmarshal(raw, &snapshot) != nil {
		return fmt.Errorf("malformed composition snapshot: %s", raw)
	}
	if snapshot.Version != 1 {
		return fmt.Errorf("unexpected snapshot version %d", snapshot.Version)
	}
	if snapshot.System == 0 || snapshot.Tools == 0 {
		return fmt.Errorf("system prompt or tool definitions were not estimated: %+v", snapshot)
	}
	if !strings.Contains(snapshot.Model, "/") {
		return fmt.Errorf("snapshot model reference is malformed: %q", snapshot.Model)
	}
	fmt.Printf("[SNAPSHOT] %s\n", path)
	fmt.Printf("[TOTALS] model=%s agent=%s system=%d tools=%d messages=%d reasoning=%d toolCalls=%d files=%d toolResults=%d memory=%d opaqueReasoning=%v opaqueMemory=%v\n",
		snapshot.Model, snapshot.Agent, snapshot.System, snapshot.Tools, snapshot.Messages, snapshot.Reasoning, snapshot.ToolCalls, snapshot.Files, snapshot.ToolResults, snapshot.Memory, snapshot.OpaqueReason, snapshot.OpaqueMemory)
	return nil
}

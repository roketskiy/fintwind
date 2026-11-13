package main

// goalcheck is an isolated Go-daemon Goal/retry E2E. Its OpenCode HTTP/SSE
// peer is this binary in fixture mode, running in a separate Owned process.

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/ownedprocess"
)

const failureCases = "Goal Get is RPC-only; required native rounds/maxRounds/updatedAt reject missing, null and out-of-range values; objective rejects missing, null and non-string values; invalid snapshots preserve the saved Goal; actions, attachments and pre-POST rejection; single-POST 400/422 and ambiguous outcomes; modern/legacy retry; idle read failure and terminal dedup; scoped child idle; async Goal polling; cancel pause-before-interrupt; SQLite restart hydration; zero credentials and loopback-only fixture."

func main() {
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		if err := runFakeServer(); err != nil {
			fmt.Fprintln(os.Stderr, "[X] fake fixture:", err)
			os.Exit(2)
		}
		return
	}
	daemonPath := flag.String("daemon", "", "absolute isolated daemon executable")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 560*time.Second)
	defer cancel()
	if err := run(ctx, *daemonPath); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, daemonPath string) (err error) {
	if !filepath.IsAbs(daemonPath) {
		return errors.New("absolute daemon path required")
	}
	for _, key := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "FINTWIND_AGENT_REAL_PROVIDER"} {
		if strings.TrimSpace(os.Getenv(key)) != "" {
			return fmt.Errorf("credential/provider opt-in %s must be unset", key)
		}
	}
	self, err := os.Executable()
	if err != nil {
		return errors.New("cannot resolve fixture executable")
	}
	daemonHash, err := fileSHA256(daemonPath)
	if err != nil {
		return errors.New("cannot read isolated daemon binary")
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return errors.New("cannot create owned E2E round")
	}
	result := map[string]any{
		"command":            "goalcheck --daemon " + daemonPath,
		"provider":           "synthetic loopback OpenCode HTTP/SSE fixture",
		"realRequests":       0,
		"protocolVersion":    25,
		"daemonBinary":       daemonPath,
		"daemonSha256":       daemonHash,
		"harnessBinary":      self,
		"round":              round,
		"failureCases":       "docs/dev-notes/go-backend/goal-tester-failures.md",
		"failureCasesInline": failureCases,
		"uncovered":          []string{"The HTTP/SSE payloads are deterministic fixtures, not captures from a live OpenCode provider", "Subagent lifecycle is a synthetic child-scoped SSE event; full provider tool-created subagent lifecycle is not exercised"},
	}
	checks := map[string]bool{}
	defer func() {
		result["checks"] = checks
		result["passed"] = err == nil
		if err != nil {
			result["error"] = err.Error()
		}
		raw, _ := json.MarshalIndent(result, "", "  ")
		path := filepath.Join(round, "goalcheck-result.json")
		_ = os.WriteFile(path, raw, 0o600)
		fmt.Println("[REPORT]", path)
	}()

	h, err := newHarness(ctx, round, self, daemonPath, result, checks)
	if err != nil {
		return err
	}
	defer func() {
		if closeErr := h.closeOwned(); closeErr != nil && err == nil {
			err = closeErr
		}
	}()
	if err := h.runScenario(); err != nil {
		result["observedEvents"] = h.events
		if stats, statsErr := h.fixtureStats(); statsErr == nil {
			result["syntheticFixtureStats"] = stats
		}
		return err
	}
	result["observedEvents"] = h.events
	if stats, statsErr := h.fixtureStats(); statsErr == nil {
		result["syntheticFixtureStats"] = stats
	}
	return nil
}

func fileSHA256(path string) (string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:]), nil
}

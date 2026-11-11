// Command verify is the MyGo migration safety gate. It never runs unit
// tests: baseline mode drives the public daemon.Client API against loopback
// fake servers, and real mode drives the true fintwind-daemon.exe inside an
// ownedprocess Job. Fake and real results stay in separate scopes.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/ownedprocess"
)

type caseResult struct {
	Name   string `json:"name"`
	Scope  string `json:"scope"`
	Status string `json:"status"`
	Detail string `json:"detail"`
}

type verifyResult struct {
	Mode  string       `json:"mode"`
	Cases []caseResult `json:"cases"`
}

func main() {
	mode := flag.String("mode", "baseline", "verify scope: baseline, real, admission, settings, runtime-config, all")
	daemonPath := flag.String("daemon", defaultDaemonPath(), "path to fintwind-daemon.exe (real mode) or fixture daemon binary (admission mode)")
	total := flag.Duration("timeout", 90*time.Second, "total verify deadline")
	out := flag.String("out", "", "result.json path (defaults to <roundDir>/result.json)")
	flag.Parse()

	ctx, cancel := context.WithTimeout(context.Background(), *total)
	defer cancel()

	roundDir, err := ownedprocess.NewRoundDir()
	if err != nil {
		fail("create round dir: %v", err)
	}
	fmt.Printf("round dir: %s\n", roundDir)

	result := &verifyResult{Mode: *mode}
	runBaseline := *mode == "baseline" || *mode == "all"
	runReal := *mode == "real" || *mode == "all"
	runAdmission := *mode == "admission"
	runSettings := *mode == "settings"
	runConfig := *mode == "runtime-config"

	if runBaseline {
		for _, c := range runFixture(ctx) {
			report(c)
			result.Cases = append(result.Cases, c)
		}
	}
	if runReal {
		for _, c := range runRealDaemon(ctx, *daemonPath, roundDir) {
			report(c)
			result.Cases = append(result.Cases, c)
		}
	}
	if runAdmission {
		for _, c := range runAdmissionFixture(ctx, *daemonPath, roundDir) {
			report(c)
			result.Cases = append(result.Cases, c)
		}
	}
	if runSettings {
		result.Cases = append(result.Cases, runAtomicSettings(ctx, *daemonPath, roundDir)...)
	}
	if runConfig {
		result.Cases = append(result.Cases, runRuntimeConfig(ctx, *daemonPath, roundDir)...)
	}
	if !runBaseline && !runReal && !runAdmission && !runSettings && !runConfig {
		fail("unknown -mode %q: baseline, real, admission, settings, runtime-config, all", *mode)
	}

	path := *out
	if path == "" {
		path = filepath.Join(roundDir, "result.json")
	}
	raw, _ := json.MarshalIndent(result, "", "  ")
	if err := os.WriteFile(path, append(raw, '\n'), 0o644); err != nil {
		fail("write result: %v", err)
	}
	fmt.Printf("result: %s\n", path)

	for _, c := range result.Cases {
		if c.Status != "pass" {
			os.Exit(1)
		}
	}
}

func report(c caseResult) {
	status := "PASS"
	if c.Status != "pass" {
		status = "FAIL"
	}
	fmt.Printf("%s [%s] %s: %s\n", status, c.Scope, c.Name, c.Detail)
}

func fail(format string, args ...any) {
	fmt.Printf("FAIL %s\n", fmt.Sprintf(format, args...))
	os.Exit(1)
}

func defaultDaemonPath() string {
	candidates := []string{
		filepath.Join("..", "..", "target", "release", "fintwind-daemon.exe"),
		filepath.Join("..", "target", "release", "fintwind-daemon.exe"),
	}
	for _, candidate := range candidates {
		if _, err := os.Stat(candidate); err == nil {
			absolute, _ := filepath.Abs(candidate)
			return absolute
		}
	}
	return candidates[0]
}

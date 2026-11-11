package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"time"

	"fintwind-go/internal/launchprofile"
)

func main() {
	if len(os.Args) > 1 && os.Args[1] == "--bind" && os.Getenv("MYGO_VERIFY_BAD_DAEMON") == "1" {
		_ = json.NewEncoder(os.Stdout).Encode(daemonReady{Address: "127.0.0.1:1", Protocol: 43, PID: uint32(os.Getpid())})
		time.Sleep(10 * time.Second)
		return
	}
	if len(os.Args) > 1 && os.Args[1] == "--launcher-probe" {
		if err := launcherProbe(); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(7)
		}
		return
	}
	profile := flag.String("profile", "agent", "daily uses real Fintwind data; agent is isolated")
	name := flag.String("name", "default", "isolated agent environment name")
	root := flag.String("root", "", "repository directory")
	planOnly := flag.Bool("plan", false, "print paths without building, creating data, or starting processes")
	buildOnly := flag.Bool("build-only", false, "build matching binaries without starting the app or daemon")
	verify := flag.Bool("verify", false, "run isolated launcher process E2E without opening a window")
	daemonPath := flag.String("daemon", "", "daemon binary for --verify only")
	flag.Parse()
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()
	if *verify {
		// Six sequential daemon spawns plus graceful shutdowns; a cold
		// machine needs more than two minutes even with release binaries.
		verifyCtx, cancel := context.WithTimeout(ctx, 5*time.Minute)
		defer cancel()
		if err := verifyLauncher(verifyCtx, *root, *daemonPath); err != nil {
			fmt.Fprintln(os.Stderr, "[X]", err)
			os.Exit(1)
		}
		return
	}
	if err := run(ctx, *root, *profile, *name, *planOnly, *buildOnly); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, root, profile, name string, planOnly, buildOnly bool) error {
	if root == "" {
		return errors.New("repository root is required; use MyGo.cmd or MyGo-Agent.cmd")
	}
	plan, err := launchprofile.Resolve(launchprofile.Options{Profile: profile, Root: root, Name: name, ParentEnv: os.Environ()})
	if err != nil {
		return err
	}
	if planOnly {
		// Daily credentials remain in memory; plan output never serializes them.
		return json.NewEncoder(os.Stdout).Encode(map[string]any{"profile": plan.Profile, "name": plan.Name, "dataDir": plan.DataDir, "preferencesDir": plan.PreferencesDir, "workDir": plan.WorkDir, "buildDir": plan.BuildDir, "protocolVersion": 24})
	}
	if err := plan.Prepare(); err != nil {
		return err
	}
	runDir, err := os.MkdirTemp(plan.BuildDir, "run-")
	if err != nil {
		return err
	}
	fmt.Println("[OK] Profile:", plan.Profile)
	fmt.Println("[OK] Data:", plan.DataDir)
	fmt.Println("[OK] Logs:", runDir)
	buildCtx, cancel := context.WithTimeout(ctx, 10*time.Minute)
	defer cancel()
	unlock, err := buildLock(buildCtx, plan.BuildDir)
	if err != nil {
		return err
	}
	defer unlock()
	binaries, err := build(buildCtx, root, plan.BuildDir, runDir)
	unlock()
	cancel()
	if err != nil {
		return err
	}
	if buildOnly {
		fmt.Println("[OK] App:", binaries.app)
		fmt.Println("[OK] Daemon:", binaries.daemon)
		return nil
	}
	if plan.Profile == launchprofile.Daily {
		info, err := os.Stat(filepath.Join(plan.DataDir, "app.db"))
		if err != nil || !info.Mode().IsRegular() {
			return errors.New("existing Fintwind app.db was not found; daily startup will not create a replacement database")
		}
		fmt.Println("[WARN] Real Fintwind data is enabled. Close the other Fintwind window first.")
	}
	return launch(ctx, plan, binaries, runDir)
}

package main

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"fintwind-go/internal/ownedprocess"
)

type binaries struct{ app, daemon string }

func build(ctx context.Context, root, buildDir, runDir string) (binaries, error) {
	result := binaries{app: filepath.Join(runDir, "fintwind-mygo.exe")}
	if err := command(ctx, root, "go", "build", "-mod=readonly", "-ldflags=-H=windowsgui", "-o", result.app, "./cmd/app"); err != nil {
		return result, err
	}
	result.daemon = filepath.Join(runDir, "fintwind-daemon.exe")
	if err := command(ctx, root, "go", "build", "-mod=readonly", "-o", result.daemon, "./cmd/daemon"); err != nil {
		return result, err
	}
	return result, nil
}

func command(ctx context.Context, directory, name string, args ...string) error {
	fmt.Println("[BUILD]", name, strings.Join(args, " "))
	_, err := commandOutput(ctx, directory, name, args...)
	return err
}

func commandOutput(ctx context.Context, directory, name string, args ...string) ([]byte, error) {
	executable, err := exec.LookPath(name)
	if err != nil {
		return nil, err
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return nil, err
	}
	process, err := ownedprocess.StartWithEnvironment(executable, args, round, directory, buildEnvironment())
	if err != nil {
		return nil, err
	}
	defer process.Close()
	for {
		done, err := process.Exited(50)
		if err != nil {
			return nil, err
		}
		if done {
			code, err := process.ExitCode()
			if err != nil {
				return nil, err
			}
			stderr, _ := os.ReadFile(process.StderrPath)
			if code != 0 {
				return nil, fmt.Errorf("%s failed (exit %d); no previous binary was launched. %s\n%s", name, code, process.StderrPath, strings.TrimSpace(string(stderr)))
			}
			if len(stderr) > 0 {
				fmt.Fprint(os.Stderr, string(stderr))
			}
			return os.ReadFile(process.StdoutPath)
		}
		if err := ctx.Err(); err != nil {
			return nil, err
		}
	}
}

func buildEnvironment() []string {
	var result []string
	for _, entry := range os.Environ() {
		key, _, _ := strings.Cut(entry, "=")
		if key == "GOFLAGS" || key == "RUSTFLAGS" || key == "CARGO_ENCODED_RUSTFLAGS" || key == "GOOS" || key == "GOARCH" {
			continue
		}
		result = append(result, entry)
	}
	return result
}

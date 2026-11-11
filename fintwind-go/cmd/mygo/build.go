package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"

	"fintwind-go/internal/ownedprocess"
)

type binaries struct{ app, daemon string }
type buildReceipt struct {
	Source string `json:"source"`
	Binary string `json:"binary"`
}

func build(ctx context.Context, root, buildDir, runDir string) (binaries, error) {
	result := binaries{app: filepath.Join(runDir, "fintwind-mygo.exe")}
	if err := command(ctx, filepath.Join(root, "fintwind-go"), "go", "build", "-mod=readonly", "-ldflags=-H=windowsgui", "-o", result.app, "./cmd/app"); err != nil {
		return result, err
	}
	fingerprint, err := rustFingerprint(ctx, root)
	if err != nil {
		return result, err
	}
	cache := filepath.Join(buildDir, "daemon", fingerprint)
	result.daemon = filepath.Join(cache, "fintwind-daemon.exe")
	receiptPath := filepath.Join(cache, "build.json")
	var receipt buildReceipt
	if data, err := os.ReadFile(receiptPath); err == nil && json.Unmarshal(data, &receipt) == nil && receipt.Source == fingerprint {
		if digest, err := fileHash(result.daemon); err == nil && digest == receipt.Binary {
			fmt.Println("[OK] Reusing matching Rust daemon")
			return result, nil
		}
	}
	fmt.Println("[BUILD] cargo build --locked --offline --package fintwind-daemon --bin fintwind-daemon")
	output, err := commandOutput(ctx, root, "cargo", "build", "--locked", "--offline", "--package", "fintwind-daemon", "--bin", "fintwind-daemon", "--message-format=json-render-diagnostics")
	if err != nil {
		return result, err
	}
	var builtPath string
	for _, line := range strings.Split(string(output), "\n") {
		var artifact struct {
			Reason     string `json:"reason"`
			Executable string `json:"executable"`
			Target     struct {
				Name string `json:"name"`
			} `json:"target"`
		}
		if json.Unmarshal([]byte(line), &artifact) == nil && artifact.Reason == "compiler-artifact" && artifact.Target.Name == "fintwind-daemon" && artifact.Executable != "" {
			builtPath = artifact.Executable
		}
	}
	if builtPath == "" {
		return result, fmt.Errorf("Cargo did not report the executable built in this invocation")
	}
	after, err := rustFingerprint(ctx, root)
	if err != nil {
		return result, err
	}
	if after != fingerprint {
		return result, fmt.Errorf("Rust sources changed during the build; run the launcher again")
	}
	if err := os.MkdirAll(cache, 0o700); err != nil {
		return result, err
	}
	if err := copyExecutable(builtPath, result.daemon); err != nil {
		return result, err
	}
	digest, err := fileHash(result.daemon)
	if err != nil {
		return result, err
	}
	data, _ := json.MarshalIndent(buildReceipt{Source: fingerprint, Binary: digest}, "", "  ")
	if err := os.WriteFile(receiptPath, data, 0o600); err != nil {
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

func rustFingerprint(ctx context.Context, root string) (string, error) {
	var files []string
	for _, name := range []string{"Cargo.toml", "Cargo.lock"} {
		files = append(files, filepath.Join(root, name))
	}
	for _, directory := range []string{"crates", "locales", "resources"} {
		err := filepath.WalkDir(filepath.Join(root, directory), func(path string, entry fs.DirEntry, err error) error {
			if err != nil {
				return err
			}
			if entry.IsDir() {
				if entry.Name() == "target" || entry.Name() == ".git" {
					return filepath.SkipDir
				}
				return nil
			}
			if entry.Type().IsRegular() {
				files = append(files, path)
			}
			return nil
		})
		if err != nil {
			return "", err
		}
	}
	sort.Strings(files)
	hash := sha256.New()
	for _, tool := range []string{"rustc", "cargo"} {
		version, err := commandOutput(ctx, root, tool, "-vV")
		if err != nil {
			return "", err
		}
		hash.Write(version)
	}
	for _, entry := range buildEnvironment() {
		key, _, _ := strings.Cut(entry, "=")
		if strings.HasPrefix(strings.ToUpper(key), "CARGO_") || strings.HasPrefix(strings.ToUpper(key), "RUST") {
			fmt.Fprintln(hash, entry)
		}
	}
	for directory := root; ; directory = filepath.Dir(directory) {
		for _, name := range []string{filepath.Join(".cargo", "config"), filepath.Join(".cargo", "config.toml"), "rust-toolchain", "rust-toolchain.toml"} {
			path := filepath.Join(directory, name)
			if data, err := os.ReadFile(path); err == nil {
				fmt.Fprintln(hash, path)
				hash.Write(data)
			} else if !os.IsNotExist(err) {
				return "", err
			}
		}
		if filepath.Dir(directory) == directory {
			break
		}
	}
	cargoHome := os.Getenv("CARGO_HOME")
	if cargoHome == "" {
		home, _ := os.UserHomeDir()
		cargoHome = filepath.Join(home, ".cargo")
	}
	for _, name := range []string{"config", "config.toml"} {
		path := filepath.Join(cargoHome, name)
		if data, err := os.ReadFile(path); err == nil {
			fmt.Fprintln(hash, path)
			hash.Write(data)
		} else if !os.IsNotExist(err) {
			return "", err
		}
	}
	for _, path := range files {
		relative, _ := filepath.Rel(root, path)
		fmt.Fprintln(hash, filepath.ToSlash(relative))
		digest, err := fileHash(path)
		if err != nil {
			return "", err
		}
		fmt.Fprintln(hash, digest)
	}
	return hex.EncodeToString(hash.Sum(nil)), nil
}

func fileHash(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	hash := sha256.New()
	if _, err := io.Copy(hash, file); err != nil {
		return "", err
	}
	return hex.EncodeToString(hash.Sum(nil)), nil
}

func copyExecutable(source, destination string) error {
	input, err := os.Open(source)
	if err != nil {
		return err
	}
	defer input.Close()
	temporary, err := os.CreateTemp(filepath.Dir(destination), "daemon-*.tmp")
	if err != nil {
		return err
	}
	defer os.Remove(temporary.Name())
	_, err = io.Copy(temporary, input)
	if err == nil {
		err = temporary.Sync()
	}
	closeErr := temporary.Close()
	if err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	return os.Rename(temporary.Name(), destination)
}

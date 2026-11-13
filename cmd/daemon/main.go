package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backend"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/daemon"

	"github.com/google/uuid"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "Go backend startup or runtime failed:", err)
		os.Exit(1)
	}
}

func run() error {
	bind := flag.String("bind", "127.0.0.1:0", "loopback bind address")
	data := flag.String("data-dir", "", "isolated data directory")
	parent := flag.Uint("parent-pid", 0, "launcher identity; process lifetime is contained by the launcher's owned Job")
	flag.Parse()
	_ = parent
	if flag.NArg() != 0 {
		return errors.New("unexpected arguments")
	}
	token := os.Getenv("FINTWIND_DAEMON_TOKEN")
	_ = os.Unsetenv("FINTWIND_DAEMON_TOKEN")
	if len(token) < 16 {
		return errors.New("daemon token required")
	}
	host, _, err := net.SplitHostPort(*bind)
	if err != nil {
		return errors.New("invalid bind address")
	}
	ip := net.ParseIP(host)
	if ip == nil || !ip.IsLoopback() {
		return errors.New("loopback bind required")
	}
	daily := os.Getenv("FINTWIND_MYGO_PROFILE") == "daily"
	directory := ""
	if daily {
		directory, err = dailyDirectory(*data)
	} else {
		directory, err = isolatedDirectory(*data)
	}
	if err != nil {
		return err
	}
	lock, err := lockDirectory(directory)
	if err != nil {
		return err
	}
	defer lock.Close()
	identity, err := claimIdentity(directory)
	if err != nil {
		return err
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	workspace := filepath.Join(filepath.Dir(directory), "workspace")
	settings := filepath.Join(directory, "daemon.json")
	if daily {
		workspace = filepath.Join(directory, "projects")
		home, e := os.UserHomeDir()
		if e != nil {
			return e
		}
		settings = filepath.Join(home, ".fintwind", "settings.json")
	}
	if err := os.MkdirAll(workspace, 0o700); err != nil {
		return err
	}
	store, err := backendstore.Open(ctx, directory)
	if err != nil {
		return err
	}
	defer store.Close()
	if err := store.RecoverInterruptedSessions(ctx); err != nil {
		return fmt.Errorf("recover interrupted sessions: %w", err)
	}
	listener, err := net.Listen("tcp", *bind)
	if err != nil {
		return err
	}
	defer listener.Close()
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}
	service := backend.New(ctx, store, identity, directory, settings, cwd, token, daily, cancel)
	httpServer := &http.Server{Handler: service, ReadHeaderTimeout: 5 * time.Second, IdleTimeout: 60 * time.Second, MaxHeaderBytes: 16 << 10}
	serveErr := make(chan error, 1)
	go func() { serveErr <- httpServer.Serve(listener) }()
	ready := map[string]any{"address": listener.Addr().String(), "protocolVersion": daemon.ProtocolVersion, "pid": os.Getpid(), "environmentId": identity.EnvironmentID, "instanceId": identity.InstanceID}
	if err := json.NewEncoder(os.Stdout).Encode(ready); err != nil {
		_ = httpServer.Close()
		service.Close()
		return err
	}
	select {
	case <-ctx.Done():
	case err = <-serveErr:
	}
	service.Close()
	stopCtx, stop := context.WithTimeout(context.Background(), 5*time.Second)
	defer stop()
	_ = httpServer.Shutdown(stopCtx)
	if err != nil && !errors.Is(err, http.ErrServerClosed) {
		return err
	}
	return nil
}

func dailyDirectory(data string) (string, error) {
	if !filepath.IsAbs(data) {
		return "", errors.New("absolute data directory required")
	}
	expected := os.Getenv("FINTWIND_MYGO_EXPECTED_DATA_DIR")
	if expected == "" {
		return "", errors.New("launcher data directory required")
	}
	directory, err := filepath.EvalSymlinks(data)
	if err != nil {
		return "", err
	}
	want, err := filepath.EvalSymlinks(expected)
	if err != nil || !strings.EqualFold(directory, want) {
		return "", errors.New("launcher and daemon data directories differ")
	}
	info, err := os.Stat(filepath.Join(directory, "app.db"))
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return "", err
	}
	if err == nil && !info.Mode().IsRegular() {
		return "", errors.New("Fintwind database is not a regular file")
	}
	return directory, nil
}

func isolatedDirectory(data string) (string, error) {
	if os.Getenv("FINTWIND_MYGO_PROFILE") != "agent" || !filepath.IsAbs(data) {
		return "", errors.New("Go backend currently requires an isolated agent profile")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	home, err = filepath.EvalSymlinks(home)
	if err != nil {
		return "", errors.New("isolated home missing")
	}
	if !strings.EqualFold(filepath.Base(home), "home") {
		return "", errors.New("isolated home required")
	}
	root := filepath.Dir(home)
	expected := filepath.Join(root, "data")
	if !strings.EqualFold(filepath.Clean(data), expected) {
		return "", errors.New("data directory must belong to isolated profile")
	}
	if err := os.MkdirAll(data, 0o700); err != nil {
		return "", err
	}
	resolved, err := filepath.EvalSymlinks(data)
	if err != nil {
		return "", err
	}
	if !strings.EqualFold(resolved, expected) {
		return "", errors.New("data directory escapes isolated profile")
	}
	return resolved, nil
}

func claimIdentity(directory string) (backend.Identity, error) {
	path := filepath.Join(directory, "environment_id")
	raw, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		raw = []byte(uuid.NewString())
		if err := os.WriteFile(path, raw, 0o600); err != nil {
			return backend.Identity{}, err
		}
	} else if err != nil {
		return backend.Identity{}, err
	}
	id, err := uuid.Parse(strings.TrimSpace(string(raw)))
	if err != nil || id == uuid.Nil {
		return backend.Identity{}, errors.New("invalid environment identity")
	}
	return backend.Identity{EnvironmentID: id.String(), InstanceID: uuid.NewString()}, nil
}

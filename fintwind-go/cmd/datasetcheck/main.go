package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/backendprivate"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

func main() {
	source := flag.String("source", "", "absolute task database to export read-only")
	exporter := flag.String("exporter", "", "absolute sanitized exporter")
	binary := flag.String("daemon", "", "absolute isolated Go daemon")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	if err := run(ctx, *source, *exporter, *binary); err != nil {
		fmt.Fprintln(os.Stderr, "[X] Backup dataset verification failed; no source contents logged")
		os.Exit(1)
	}
}

func run(ctx context.Context, source, exporter, binary string) (err error) {
	if !filepath.IsAbs(source) || !filepath.IsAbs(exporter) || !filepath.IsAbs(binary) {
		return errors.New("absolute paths required")
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	if err := backendprivate.Directory(round); err != nil {
		return err
	}
	checks := map[string]bool{}
	counts := map[string]int{}
	defer func() {
		raw, _ := json.MarshalIndent(map[string]any{"passed": err == nil, "checks": checks, "counts": counts, "sourceReadOnly": true, "productionDaemonStarted": false, "credentialsCopied": false}, "", "  ")
		_ = os.WriteFile(filepath.Join(round, "dataset-result.json"), raw, 0o600)
		fmt.Println("[REPORT]", filepath.Join(round, "dataset-result.json"))
	}()
	launch := func(binary string, args []string, name string, extra map[string]string) (*ownedprocess.Owned, error) {
		logs := filepath.Join(round, name)
		if err := os.Mkdir(logs, 0o700); err != nil {
			return nil, err
		}
		return ownedprocess.StartWithEnvironment(binary, args, logs, round, ownedprocess.BuildEnv(round, extra))
	}
	output := filepath.Join(round, "data")
	copy, err := launch(exporter, []string{"--source", source, "--output", output}, "export", nil)
	if err != nil {
		return err
	}
	defer copy.Close()
	if err := wait(ctx, copy); err != nil {
		return err
	}
	raw, err := os.ReadFile(filepath.Join(output, "sanitized-export.json"))
	if err != nil {
		return err
	}
	var exported backendstore.ExportReport
	if err := json.Unmarshal(raw, &exported); err != nil {
		return err
	}
	if !exported.Sanitized || !exported.SourceReadOnly || exported.CredentialsCopied {
		return errors.New("export not sanitized")
	}
	checks["read-only-online-export"] = true
	counts["projects"] = exported.Projects
	counts["sessions"] = exported.Sessions
	token := uuid.NewString()
	child, err := launch(binary, []string{"--data-dir", output, "--bind", "127.0.0.1:0"}, "backend", map[string]string{"FINTWIND_DAEMON_TOKEN": token})
	if err != nil {
		return err
	}
	defer child.Close()
	tick := time.NewTicker(50 * time.Millisecond)
	defer tick.Stop()
	address := ""
	for address == "" {
		raw, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
			PID     uint32 `json:"pid"`
		}
		if json.Unmarshal(raw, &ready) == nil && ready.PID == child.PID {
			address = ready.Address
		}
		if address != "" {
			break
		}
		if done, _ := child.Exited(0); done {
			return errors.New("isolated daemon exited")
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-tick.C:
		}
	}
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		return err
	}
	defer client.Close()
	raw, err = client.Request(ctx, map[string]any{"type": "loadTaskState"})
	if err != nil {
		return err
	}
	var catalog model.TaskState
	if err := json.Unmarshal(raw, &catalog); err != nil {
		return err
	}
	if len(catalog.Projects) != exported.Projects || len(catalog.Sessions) != exported.Sessions {
		return errors.New("catalog count mismatch")
	}
	checks["isolated-catalog-from-backup"] = true
	for _, session := range catalog.Sessions {
		raw, err := client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": session.ID})
		if err != nil {
			return err
		}
		var payload model.SessionDetail
		if err := json.Unmarshal(raw, &payload); err != nil || payload.Session == nil {
			return errors.New("hydration not readable by existing Go model")
		}
		counts["messages"] += len(payload.Session.Messages)
		counts["hydrated"]++
	}
	checks["all-backup-sessions-hydrated"] = true
	return nil
}

func wait(ctx context.Context, child *ownedprocess.Owned) error {
	tick := time.NewTicker(50 * time.Millisecond)
	defer tick.Stop()
	for {
		if done, err := child.Exited(0); err != nil {
			return err
		} else if done {
			code, err := child.ExitCode()
			if err != nil {
				return err
			}
			if code != 0 {
				return errors.New("export process failed")
			}
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-tick.C:
		}
	}
}

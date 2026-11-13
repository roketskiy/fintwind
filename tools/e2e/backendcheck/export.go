package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
	_ "modernc.org/sqlite"
)

func waitExit(ctx context.Context, child *ownedprocess.Owned) error {
	tick := time.NewTicker(25 * time.Millisecond)
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
				return errors.New("owned child failed")
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

func verifyExport(ctx context.Context, exporter, binary, source, round string) error {
	if !filepath.IsAbs(exporter) {
		return errors.New("absolute exporter required")
	}
	root := filepath.Join(round, "exported")
	logs := filepath.Join(root, "export-logs")
	for _, dir := range []string{logs, filepath.Join(root, "home"), filepath.Join(root, "tmp"), filepath.Join(root, "daemon-logs")} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
	}
	output := filepath.Join(root, "data")
	child, err := ownedprocess.StartWithEnvironment(exporter, []string{"--source", filepath.Join(source, "app.db"), "--output", output}, logs, root, ownedprocess.BuildEnv(root, nil))
	if err != nil {
		return err
	}
	defer child.Close()
	if err := waitExit(ctx, child); err != nil {
		return err
	}
	raw, err := os.ReadFile(filepath.Join(output, "sanitized-export.json"))
	if err != nil {
		return err
	}
	var report struct {
		Sanitized   bool `json:"sanitized"`
		Credentials bool `json:"credentialsCopied"`
		ReadOnly    bool `json:"sourceReadOnly"`
		Sessions    int  `json:"sessions"`
	}
	if err := json.Unmarshal(raw, &report); err != nil {
		return err
	}
	if !report.Sanitized || report.Credentials || !report.ReadOnly || report.Sessions != 1 {
		return errors.New("export report does not prove sanitized fixture")
	}
	db, err := sql.Open("sqlite", "file:"+filepath.ToSlash(filepath.Join(output, "app.db"))+"?mode=ro")
	if err != nil {
		return err
	}
	defer db.Close()
	rows, err := db.QueryContext(ctx, `SELECT data FROM session_details`)
	if err != nil {
		return err
	}
	for rows.Next() {
		var data string
		if err := rows.Scan(&data); err != nil {
			rows.Close()
			return err
		}
		if strings.Contains(data, "SENTINEL_ONLY_NOT_REAL_SECRET") || strings.Contains(data, "test_secret") {
			rows.Close()
			return errors.New("export retained synthetic secret")
		}
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return err
	}
	token := uuid.NewString()
	backend, err := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", output, "--bind", "127.0.0.1:0"}, filepath.Join(root, "daemon-logs"), root, ownedprocess.BuildEnv(root, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if err != nil {
		return err
	}
	defer backend.Close()
	address, err := waitReady(ctx, backend)
	if err != nil {
		return err
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
	var state struct {
		Sessions []struct {
			ID string `json:"id"`
		} `json:"sessions"`
	}
	if err := json.Unmarshal(raw, &state); err != nil {
		return err
	}
	if len(state.Sessions) != 1 {
		return errors.New("sanitized database lost session catalog")
	}
	raw, err = client.Request(ctx, map[string]any{"type": "hydrateSession", "sessionId": state.Sessions[0].ID})
	if err != nil {
		return err
	}
	var detail struct {
		Session struct {
			Messages []struct {
				Content string `json:"content"`
			} `json:"messages"`
		} `json:"session"`
	}
	if err := json.Unmarshal(raw, &detail); err != nil {
		return err
	}
	if len(detail.Session.Messages) != 2000 || detail.Session.Messages[1999].Content != strings.Repeat("x", len("literal-pressure-1999")) {
		return errors.New("sanitized database changed message count, order or shape")
	}
	return nil
}

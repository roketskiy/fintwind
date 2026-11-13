// realdataread inspects an existing database without starting a daemon or
// writing any session contents to disk or stdout.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"
)

func main() {
	path := flag.String("database", "", "absolute path to an existing app.db")
	flag.Parse()
	if !filepath.IsAbs(*path) || flag.NArg() != 0 {
		fmt.Fprintln(os.Stderr, "absolute database path required")
		os.Exit(1)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	if err := run(ctx, *path); err != nil {
		fmt.Fprintln(os.Stderr, "read-only compatibility check failed:", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, path string) error {
	store, err := backendstore.OpenReadOnly(ctx, path)
	if err != nil {
		return err
	}
	defer store.Close()
	raw, err := store.Load(ctx)
	if err != nil {
		return err
	}
	encoded, err := json.Marshal(raw)
	if err != nil {
		return err
	}
	var catalog model.TaskState
	if err := json.Unmarshal(encoded, &catalog); err != nil {
		return err
	}
	counts := struct {
		Projects int `json:"projects"`
		Sessions int `json:"sessions"`
		Hydrated int `json:"hydrated"`
		Messages int `json:"messages"`
		Failures int `json:"failures"`
	}{Projects: len(catalog.Projects), Sessions: len(catalog.Sessions)}
	for _, session := range catalog.Sessions {
		result, err := store.Hydrate(ctx, session.ID)
		if err != nil {
			counts.Failures++
			continue
		}
		encoded, err := json.Marshal(result)
		if err != nil {
			counts.Failures++
			continue
		}
		var detail model.SessionDetail
		if json.Unmarshal(encoded, &detail) != nil || detail.Session == nil {
			counts.Failures++
			continue
		}
		counts.Hydrated++
		counts.Messages += len(detail.Session.Messages)
	}
	if err := json.NewEncoder(os.Stdout).Encode(counts); err != nil {
		return err
	}
	if counts.Failures != 0 {
		return fmt.Errorf("%d sessions could not be hydrated", counts.Failures)
	}
	return nil
}

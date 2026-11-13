package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendworkspace"
	"github.com/google/uuid"
)

func pressure(ctx context.Context, round, repo string, queries *backendworkspace.Queries, checks map[string]bool) error {
	call := func(kind string, fields map[string]any) (map[string]json.RawMessage, error) {
		fields["type"] = kind
		fields["cwd"] = repo
		raw, _ := json.Marshal(fields)
		response, err := backendworkspace.DispatchWithQueries(ctx, round, raw, queries)
		if err != nil {
			return nil, err
		}
		var out map[string]json.RawMessage
		err = json.Unmarshal(response, &out)
		return out, err
	}
	// Rename is a real staged rename with exact content, not delete+add counts.
	if _, err := call("commit", map[string]any{"message": "rename baseline", "include_unstaged": true}); err != nil {
		return err
	}
	if err := os.Rename(filepath.Join(repo, "literal.txt"), filepath.Join(repo, "renamed.txt")); err != nil {
		return err
	}
	if _, err := call("stagePaths", map[string]any{"paths": []string{"literal.txt", "renamed.txt"}}); err != nil {
		return err
	}
	response, err := call("collectReviewFileDiff", map[string]any{"source": "staged", "path": "renamed.txt", "origin_path": "literal.txt", "context_lines": 3})
	if err != nil {
		return err
	}
	var diff struct {
		Patch     string `json:"patch"`
		Complete  bool   `json:"completeContext"`
		Truncated bool   `json:"truncated"`
	}
	if err := json.Unmarshal(response["data"], &diff); err != nil {
		return err
	}
	// Git's similarity detection must present one renamed path pair.
	if !strings.Contains(diff.Patch, "rename from literal.txt") || !strings.Contains(diff.Patch, "rename to renamed.txt") {
		return errors.New("single-file rename lost origin identity")
	}
	checks["single-file-rename-origin-preserved"] = true
	content := strings.Repeat("bounded long-context fixture line\n", 600000)
	if err := os.WriteFile(filepath.Join(repo, "large.txt"), []byte(content), 0o600); err != nil {
		return err
	}
	if _, err := call("stagePaths", map[string]any{"paths": []string{"large.txt"}}); err != nil {
		return err
	}
	if _, err := call("commit", map[string]any{"message": "pressure baseline"}); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "large.txt"), []byte("changed pressure line\n"+content[len("bounded long-context fixture line\n"):]), 0o600); err != nil {
		return err
	}
	start := time.Now()
	response, err = call("collectReviewDiff", map[string]any{"source": "uncommitted"})
	if err != nil {
		return err
	}
	if err := json.Unmarshal(response["data"], &diff); err != nil {
		return err
	}
	if diff.Complete || !diff.Truncated || !strings.Contains(diff.Patch, "+changed pressure line") || len(diff.Patch) > 1<<20 {
		return errors.New("large hydrated diff failed bounded fallback")
	}
	artifact, _ := json.Marshal(map[string]any{"fixtureBytes": len(content), "fallbackPatchBytes": len(diff.Patch), "elapsedMs": time.Since(start).Milliseconds()})
	if err := os.WriteFile(filepath.Join(round, "pressure.json"), artifact, 0o600); err != nil {
		return err
	}
	checks["large-output-bounded-context-fallback"] = true
	// Cancel a long-context query after it has entered the real dispatch flow.
	// Context is shared only by this query and its owned Git children.
	queryID := uuid.NewString()
	finished := make(chan error, 1)
	go func() {
		_, err := call("collectReviewDiff", map[string]any{"source": "uncommitted", "query_id": queryID})
		finished <- err
	}()
	timer := time.NewTimer(100 * time.Millisecond)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-finished:
		return errors.New("pressure cancellation query ended before cancellation: " + errorText(err))
	case <-timer.C:
	}
	if err := queries.Cancel(queryID); err != nil {
		return err
	}
	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-finished:
		if err == nil {
			return errors.New("cancelled pressure query reported success")
		}
		if !errors.Is(err, context.Canceled) && !strings.Contains(err.Error(), "cancelled") {
			return errors.New("pressure query failed for a reason other than cancellation: " + err.Error())
		}
		checks["pressure-query-cancellation"] = true
	}
	return nil
}
func errorText(err error) string {
	if err == nil {
		return "success"
	}
	return err.Error()
}

package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"

	"fintwind-go/internal/backendworkspace"
	"fintwind-go/internal/ownedprocess"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}
func run() (err error) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	defer func() {
		report := map[string]any{"passed": err == nil, "checks": checks, "moduleOnly": true}
		if err != nil {
			report["error"] = err.Error()
		}
		raw, e := json.MarshalIndent(report, "", "  ")
		if e == nil {
			e = os.WriteFile(filepath.Join(round, "listing-result.json"), raw, 0o600)
		}
		err = errors.Join(err, e)
		fmt.Println("[REPORT]", filepath.Join(round, "listing-result.json"))
	}()
	repo := filepath.Join(round, "repo")
	if err := os.Mkdir(repo, 0o700); err != nil {
		return err
	}
	git, err := exec.LookPath("git")
	if err != nil {
		return err
	}
	logs := filepath.Join(round, "init")
	if err := os.Mkdir(logs, 0o700); err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(git, []string{"init"}, logs, repo, ownedprocess.BuildEnv(round, nil))
	if err != nil {
		return err
	}
	defer child.Close()
	done, err := child.Exited(5000)
	if err != nil || !done {
		return errors.New("git init failed")
	}
	call := func(kind string, fields map[string]any) (map[string]json.RawMessage, error) {
		if fields == nil {
			fields = map[string]any{}
		}
		fields["type"] = kind
		fields["cwd"] = repo
		raw, _ := json.Marshal(fields)
		response, err := backendworkspace.Dispatch(ctx, round, raw)
		if err != nil {
			return nil, err
		}
		var out map[string]json.RawMessage
		err = json.Unmarshal(response, &out)
		return out, err
	}
	if _, err := call("listTree", map[string]any{"root": repo, "expanded_paths": []string{round}}); err == nil {
		return errors.New("tree expanded path escape accepted")
	}
	checks["tree-expansion-escape-refused"] = true
	if _, err := call("listUntracked", map[string]any{"directory": "../outside"}); err == nil {
		return errors.New("listing directory escape accepted")
	}
	checks["listing-directory-escape-refused"] = true
	if err := os.WriteFile(filepath.Join(repo, "tracked.txt"), []byte("base\n"), 0o600); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, ".gitignore"), []byte("ignored.txt\n"), 0o600); err != nil {
		return err
	}
	if _, err := call("stagePaths", map[string]any{"paths": []string{"tracked.txt", ".gitignore"}}); err != nil {
		return err
	}
	if _, err := call("commit", map[string]any{"message": "base"}); err != nil {
		return err
	}
	for name, body := range map[string]string{"ignored.txt": "ignored", "loose.txt": "loose"} {
		if err := os.WriteFile(filepath.Join(repo, name), []byte(body), 0o600); err != nil {
			return err
		}
	}
	if err := os.Mkdir(filepath.Join(repo, "folder"), 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "folder", "child.txt"), []byte("child"), 0o600); err != nil {
		return err
	}
	for _, scenario := range []struct {
		kind string
		want map[string]bool
	}{{"listIgnored", map[string]bool{"ignored.txt": true}}, {"listUntracked", map[string]bool{"loose.txt": true, "folder/": true}}} {
		response, err := call(scenario.kind, nil)
		if err != nil {
			return err
		}
		var entries []struct {
			Path string `json:"path"`
		}
		if err := json.Unmarshal(response["entries"], &entries); err != nil {
			return err
		}
		if len(entries) != len(scenario.want) {
			return fmt.Errorf("%s unexpected entries %s", scenario.kind, response["entries"])
		}
		for _, entry := range entries {
			if !scenario.want[entry.Path] {
				return errors.New("listing included wrong child")
			}
		}
		checks[scenario.kind+"-direct-children"] = true
	}
	response, err := call("listTree", map[string]any{"root": repo, "expanded_paths": []string{filepath.Join(repo, "folder")}})
	if err != nil {
		return err
	}
	var tree []struct {
		Path  string `json:"relativePath"`
		Depth int    `json:"depth"`
	}
	if err := json.Unmarshal(response["entries"], &tree); err != nil {
		return err
	}
	found := false
	for _, entry := range tree {
		if entry.Path == ".git" {
			return errors.New("tree exposed Git internals")
		}
		if entry.Path == "folder/child.txt" && entry.Depth == 1 {
			found = true
		}
	}
	if !found {
		return errors.New("expanded tree did not enumerate child")
	}
	checks["tree-expanded-only-no-git-internals"] = true
	if err := os.WriteFile(filepath.Join(repo, "tracked.txt"), []byte("base\nstaged\n"), 0o600); err != nil {
		return err
	}
	if _, err := call("stagePaths", map[string]any{"paths": []string{"tracked.txt"}}); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(repo, "tracked.txt"), []byte("base\nstaged\nunstaged\n"), 0o600); err != nil {
		return err
	}
	response, err = call("inspectCommit", nil)
	if err != nil {
		return err
	}
	var snapshot struct {
		Staged          bool `json:"has_staged"`
		Unstaged        bool `json:"has_unstaged"`
		StagedAdditions int  `json:"staged_additions"`
	}
	if err := json.Unmarshal(response["snapshot"], &snapshot); err != nil {
		return err
	}
	if !snapshot.Staged || !snapshot.Unstaged || snapshot.StagedAdditions != 1 {
		return errors.New("commit snapshot flags/counts mismatch")
	}
	checks["commit-snapshot-staged-unstaged"] = true
	paged := filepath.Join(repo, "paged")
	if err := os.Mkdir(paged, 0o700); err != nil {
		return err
	}
	for i := 0; i < 620; i++ {
		if err := os.WriteFile(filepath.Join(paged, fmt.Sprintf("file-%04d.txt", i)), []byte("fixture"), 0o600); err != nil {
			return err
		}
	}
	after := ""
	seen := map[string]bool{}
	pages := 0
	for {
		response, err := call("listUntracked", map[string]any{"directory": "paged", "after": after})
		if err != nil {
			return err
		}
		var entries []struct {
			Path string `json:"path"`
		}
		if err := json.Unmarshal(response["entries"], &entries); err != nil {
			return err
		}
		for _, entry := range entries {
			if seen[entry.Path] {
				return errors.New("pagination repeated file")
			}
			seen[entry.Path] = true
		}
		pages++
		var next *string
		if err := json.Unmarshal(response["next"], &next); err != nil {
			return err
		}
		if next == nil {
			break
		}
		if *next == after || pages > 10 {
			return errors.New("pagination did not advance")
		}
		after = *next
	}
	if pages != 2 || len(seen) != 620 {
		return errors.New("paged listing omitted direct children")
	}
	checks["untracked-620-files-stable-pagination"] = true
	return nil
}

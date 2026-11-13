package backendworkspace

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
)

// Attribute inspection does not run filter drivers. Check both committed trees
// and the current worktree so global-only LFS/filter installations cannot turn
// an apparently successful isolated checkout into raw pointer files.
func rejectRemoteFilters(ctx context.Context, root *confinedRoot, t *remoteTransport, cwd, head, next string) error {
	attributes, err := remoteGitPath(ctx, root, cwd, "info/attributes")
	if err != nil {
		return err
	}
	file, err := root.handle.Open(attributes)
	if err == nil {
		raw, e := io.ReadAll(io.LimitReader(file, (1<<20)+1))
		file.Close()
		if e != nil || len(raw) > 1<<20 {
			return errors.New("Git attributes exceed checkout budget")
		}
		if e := os.MkdirAll(filepath.Join(t.directory, "info"), 0o700); e != nil {
			return e
		}
		if e := os.WriteFile(filepath.Join(t.directory, "info", "attributes"), raw, 0o600); e != nil {
			return e
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return errors.New("Git attributes unavailable")
	}
	paths := []string{}
	seen := map[string]bool{}
	for _, commit := range []string{head, next} {
		out, e := t.run(ctx, cwd, "ls-tree", "-r", "-z", "--name-only", commit)
		if e != nil {
			return e
		}
		for _, path := range strings.Split(string(out.stdout), "\x00") {
			if path != "" && !seen[path] {
				if len(paths) >= 100000 {
					return errors.New("Git attribute scan exceeds path budget")
				}
				paths = append(paths, path)
				seen[path] = true
			}
		}
	}
	for start := 0; start < len(paths); start += 512 {
		if err := ctx.Err(); err != nil {
			return err
		}
		batch := paths[start:min(start+512, len(paths))]
		input := []byte(strings.Join(batch, "\x00") + "\x00")
		for _, source := range []string{head, next, ""} {
			args := []string{"check-attr", "-z", "--stdin"}
			if source != "" {
				args = append(args, "--source="+source)
			}
			args = append(args, "filter")
			out, e := runGitInput(ctx, cwd, args, t.environment, input)
			if e != nil {
				return e
			}
			records := strings.Split(string(out.stdout), "\x00")
			if len(records) != len(batch)*3+1 || records[len(records)-1] != "" {
				return errors.New("invalid Git filter attribute response")
			}
			for i := 0; i < len(batch); i++ {
				if records[i*3] != batch[i] || records[i*3+1] != "filter" {
					return errors.New("invalid Git filter attribute response")
				}
				value := records[i*3+2]
				if value != "unspecified" && value != "unset" {
					return errors.New("fast-forward with filter attributes requires reconciliation")
				}
			}
		}
	}
	return nil
}

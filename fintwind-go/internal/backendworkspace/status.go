package backendworkspace

import (
	"context"
	"encoding/json"
	"errors"
	"strconv"
	"strings"

	"fintwind-go/internal/backendstore"
)

func dispatchStatus(ctx context.Context, root *confinedRoot, raw json.RawMessage, queries *Queries) (json.RawMessage, bool, error) {
	c, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	kind := c.String("type")
	if kind != "inspectStatus" && kind != "inspectTrackedStatus" {
		return nil, false, nil
	}
	defer root.close()
	if id := c.String("queryId"); id != "" {
		if queries == nil {
			return nil, true, errors.New("daemon query registry required")
		}
		child, finish, e := queries.Begin(ctx, id)
		if e != nil {
			return nil, true, e
		}
		defer finish()
		ctx = child
	}
	cwd, _, err := root.resolveDir(c.String("cwd"))
	if err != nil {
		return nil, true, err
	}
	top, repo, err := gitToplevel(ctx, cwd)
	if err != nil {
		return nil, true, err
	}
	if !repo {
		response, e := encodeResult(map[string]any{"type": "worktreeStatus", "status": nil})
		return response, true, e
	}
	if _, _, err := root.resolveDir(top); err != nil {
		return nil, true, errors.New("repository root outside isolated workspace")
	}
	args := []string{"status", "--porcelain=v1", "-z", "--branch", "--untracked-files=all"}
	if kind == "inspectTrackedStatus" {
		args[len(args)-1] = "--untracked-files=no"
	}
	out, err := runGit(ctx, cwd, args)
	if err != nil {
		return nil, true, err
	}
	entries := []map[string]any{}
	var branch, upstream *string
	var ahead, behind uint64
	parts := strings.Split(string(out.stdout), "\x00")
	for i := 0; i < len(parts); i++ {
		if err := ctx.Err(); err != nil {
			return nil, true, err
		}
		line := parts[i]
		if line == "" {
			continue
		}
		if header, ok := strings.CutPrefix(line, "## "); ok {
			if base, counts, has := strings.Cut(header, " ["); has {
				header = base
				for _, part := range strings.Split(strings.TrimSuffix(counts, "]"), ",") {
					key, value, _ := strings.Cut(strings.TrimSpace(part), " ")
					n, _ := strconv.ParseUint(value, 10, 64)
					if key == "ahead" {
						ahead = n
					}
					if key == "behind" {
						behind = n
					}
				}
			}
			if name, ok := strings.CutPrefix(header, "No commits yet on "); ok {
				value := strings.TrimSpace(name)
				branch = &value
				continue
			}
			if header == "HEAD (no branch)" {
				continue
			}
			name, tracking, has := strings.Cut(header, "...")
			if value := strings.TrimSpace(name); value != "" {
				branch = &value
			}
			if has {
				value := strings.TrimSpace(tracking)
				upstream = &value
			}
			continue
		}
		if len(line) < 4 || line[2] != ' ' {
			return nil, true, errors.New("Git status returned an invalid record")
		}
		entry := map[string]any{"path": line[3:], "index_status": string(line[0]), "worktree_status": string(line[1]), "origin_path": nil}
		if strings.ContainsAny(line[:2], "RC") {
			if i+1 >= len(parts) || parts[i+1] == "" {
				return nil, true, errors.New("Git status omitted rename origin")
			}
			i++
			entry["origin_path"] = parts[i]
		}
		entries = append(entries, entry)
	}
	response, err := encodeResult(map[string]any{"type": "worktreeStatus", "status": map[string]any{"branch": branch, "upstream": upstream, "ahead": ahead, "behind": behind, "entries": entries}})
	return response, true, err
}

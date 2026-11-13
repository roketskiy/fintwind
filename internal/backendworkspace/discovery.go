package backendworkspace

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"fintwind-go/internal/backendstore"
)

const (
	maxDiscoveryDirectories  = 4000
	maxDiscoveryRepositories = 256
	maxDiscoveryDepth        = 32
	maxDiscoveryEntries      = 100000
)

type nestedRepository struct {
	Path         string `json:"path"`
	RelativePath string `json:"relative_path"`
	Initialized  bool   `json:"initialized"`
}

func dispatchNestedRepositories(ctx context.Context, root *confinedRoot, raw json.RawMessage, queries *Queries) (json.RawMessage, error) {
	c, err := backendstore.Decode(raw)
	if err != nil {
		return nil, err
	}
	if id := c.String("queryId"); id != "" {
		if queries == nil {
			return nil, errors.New("daemon query registry required")
		}
		child, finish, e := queries.Begin(ctx, id)
		if e != nil {
			return nil, e
		}
		defer finish()
		ctx = child
	}
	cwd, _, err := root.resolveDir(c.String("cwd"))
	if err != nil {
		return nil, err
	}
	top, repo, err := gitToplevel(ctx, cwd)
	if err != nil {
		return nil, err
	}
	if !repo {
		return nil, errors.New("workspace is not a Git repository")
	}
	top, relative, err := root.resolveDir(top)
	if err != nil {
		return nil, errors.New("repository outside isolated workspace")
	}
	handle, err := root.handle.OpenRoot(ifEmpty(relative, "."))
	if err != nil {
		return nil, err
	}
	defer handle.Close()
	var candidates []string
	if err := json.Unmarshal(c["candidates"], &candidates); err != nil {
		return nil, err
	}
	if len(candidates) > maxDiscoveryDirectories {
		return nil, errors.New("too many nested repository candidates")
	}
	work, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	complete := true
	paths := map[string]bool{}
	add := func(path string) {
		path = strings.TrimRight(filepath.ToSlash(path), "/")
		rel, e := cleanRel(path)
		if e != nil {
			return
		}
		path = filepath.ToSlash(rel)
		if paths[path] {
			return
		}
		if len(paths) >= maxDiscoveryRepositories {
			complete = false
			return
		}
		paths[path] = true
	}
	// Git parses config syntax, but only bytes already read through the confined
	// root reach it. An include or an external .gitmodules link is not followed.
	if file, e := handle.Open(".gitmodules"); e == nil {
		data, readErr := io.ReadAll(io.LimitReader(file, (1<<20)+1))
		file.Close()
		if readErr != nil || len(data) > 1<<20 {
			return nil, errors.New("submodule configuration exceeds discovery bound")
		}
		output, e := runGitOutput(work, top, []string{"config", "--no-includes", "--null", "--file", "-", "--get-regexp", `^submodule\..*\.path$`}, nil, data, true)
		if e != nil {
			if ctx.Err() != nil {
				return nil, ctx.Err()
			}
			if work.Err() != nil {
				complete = false
			} else {
				return nil, e
			}
		} else {
			for _, record := range strings.Split(string(output.stdout), "\x00") {
				if _, path, ok := strings.Cut(record, "\n"); ok {
					add(path)
				}
			}
		}
	} else if !errors.Is(e, os.ErrNotExist) {
		complete = false
	}
	for _, candidate := range candidates {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		candidate = strings.TrimRight(filepath.ToSlash(candidate), "/")
		rel, e := cleanRel(candidate)
		if e != nil {
			continue
		}
		info, e := handle.Lstat(rel)
		if e != nil || !info.IsDir() || discoveryLink(info) {
			continue
		}
		if _, e := handle.Stat(filepath.Join(rel, ".git")); e == nil {
			add(candidate)
		}
	}
	queue := []string{"."}
	visited, entries := 0, 0
	for depth := 0; len(queue) > 0; depth++ {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		if work.Err() != nil || depth >= maxDiscoveryDepth {
			complete = false
			break
		}
		children := []string{}
		for _, directory := range queue {
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			if work.Err() != nil || visited >= maxDiscoveryDirectories {
				complete = false
				break
			}
			visited++
			file, e := handle.Open(directory)
			if e != nil {
				complete = false
				continue
			}
			items, e := file.ReadDir(MaxDirEntries + 1)
			file.Close()
			if e != nil && e != io.EOF {
				complete = false
				continue
			}
			if len(items) > MaxDirEntries {
				items = items[:MaxDirEntries]
				complete = false
			}
			sort.Slice(items, func(i, j int) bool { return items[i].Name() < items[j].Name() })
			for _, item := range items {
				if err := ctx.Err(); err != nil {
					return nil, err
				}
				entries++
				if entries > maxDiscoveryEntries || len(children) >= maxDiscoveryDirectories {
					complete = false
					break
				}
				if item.Name() == ".git" {
					continue
				}
				rel := filepath.Join(directory, item.Name())
				info, e := handle.Lstat(rel)
				if e != nil {
					complete = false
					continue
				}
				if !info.IsDir() || discoveryLink(info) {
					continue
				}
				children = append(children, filepath.ToSlash(rel))
			}
			if entries > maxDiscoveryEntries || len(children) >= maxDiscoveryDirectories {
				break
			}
		}
		if len(children) == 0 {
			break
		}
		input := []byte(strings.Join(children, "\x00") + "\x00")
		output, e := runGitOutput(work, top, []string{"check-ignore", "-z", "--stdin"}, nil, input, true)
		if e != nil {
			if ctx.Err() != nil {
				return nil, ctx.Err()
			}
			if work.Err() != nil {
				complete = false
				break
			}
			return nil, e
		}
		ignored := map[string]bool{}
		for _, path := range strings.Split(string(output.stdout), "\x00") {
			ignored[filepath.ToSlash(path)] = true
		}
		next := []string{}
		for _, child := range children {
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			if ignored[child] {
				continue
			}
			if _, e := handle.Stat(filepath.Join(filepath.FromSlash(child), ".git")); e == nil {
				add(child)
			} else {
				next = append(next, child)
			}
		}
		queue = next
		if len(paths) >= maxDiscoveryRepositories || entries > maxDiscoveryEntries {
			complete = false
			break
		}
	}
	keys := make([]string, 0, len(paths))
	for path := range paths {
		keys = append(keys, path)
	}
	sort.Strings(keys)
	repositories := []nestedRepository{}
	for _, path := range keys {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		rel := filepath.FromSlash(path)
		initialized := false
		info, e := handle.Lstat(rel)
		if e == nil {
			if !info.IsDir() || discoveryLink(info) {
				continue
			}
			// Stat through the root also rejects a linked ancestor escaping it.
			if _, e := handle.Stat(rel); e != nil {
				continue
			}
			_, e = handle.Stat(filepath.Join(rel, ".git"))
			initialized = e == nil
		} else if !errors.Is(e, os.ErrNotExist) {
			continue
		}
		repositories = append(repositories, nestedRepository{Path: filepath.Join(top, rel), RelativePath: path, Initialized: initialized})
	}
	return encodeResult(map[string]any{"type": "nestedRepositories", "repositories": repositories, "complete": complete})
}

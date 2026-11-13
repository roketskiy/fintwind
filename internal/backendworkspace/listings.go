package backendworkspace

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"fintwind-go/internal/backendstore"
)

func dispatchListings(ctx context.Context, root *confinedRoot, raw json.RawMessage, queries *Queries) (json.RawMessage, bool, error) {
	c, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	kind := c.String("type")
	switch kind {
	case "listTree", "listUntracked", "listIgnored", "inspectCommit", "inspectBranches":
	default:
		return nil, false, nil
	}
	defer root.close()
	if id := c.String("queryId"); id != "" {
		if queries == nil {
			return nil, true, errors.New("daemon query registry required")
		}
		child, finish, err := queries.Begin(ctx, id)
		if err != nil {
			return nil, true, err
		}
		defer finish()
		ctx = child
	}
	if kind == "listTree" {
		response, err := listExpandedTree(ctx, root, c)
		return response, true, err
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
		return nil, true, errors.New("workspace is not a Git repository")
	}
	if _, _, err := root.resolveDir(top); err != nil {
		return nil, true, errors.New("repository outside isolated workspace")
	}
	cwd = top
	if kind == "listUntracked" || kind == "listIgnored" {
		response, err := listGitDirectory(ctx, cwd, c, kind == "listIgnored")
		return response, true, err
	}
	if kind == "inspectBranches" {
		operation, _ := json.Marshal(map[string]any{"type": "inspectBranchRefs", "cwd": cwd})
		handle, err := confineRoot(root.path)
		if err != nil {
			return nil, true, err
		}
		raw, err := dispatchInspectBranchRefs(ctx, handle, operation)
		if err != nil {
			return nil, true, err
		}
		response, err := backendstore.Decode(raw)
		if err != nil {
			return nil, true, err
		}
		snapshot, err := backendstore.Decode(response["snapshot"])
		if err != nil {
			return nil, true, err
		}
		stats, err := reviewStats(ctx, root, cwd, diffSource{kind: "branch"})
		if err != nil {
			return nil, true, err
		}
		counts, err := backendstore.Decode(stats)
		if err != nil {
			return nil, true, err
		}
		snapshot["additions"] = counts["additions"]
		snapshot["deletions"] = counts["deletions"]
		response.Set("snapshot", snapshot)
		result, err := json.Marshal(response)
		return result, true, err
	}
	response, err := inspectCommit(ctx, root, cwd)
	return response, true, err
}

func listExpandedTree(ctx context.Context, root *confinedRoot, c backendstore.Object) (json.RawMessage, error) {
	base, _, err := root.resolveDir(c.String("root"))
	if err != nil {
		return nil, err
	}
	handle, err := os.OpenRoot(base)
	if err != nil {
		return nil, err
	}
	defer handle.Close()
	var paths []string
	if err := json.Unmarshal(c["expandedPaths"], &paths); err != nil {
		return nil, err
	}
	if len(paths) > 10000 {
		return nil, errors.New("too many expanded directories")
	}
	expanded := map[string]bool{}
	for _, path := range paths {
		if !filepath.IsAbs(path) {
			relative, err := cleanRel(path)
			if err != nil {
				return nil, err
			}
			path = filepath.Join(base, relative)
		}
		rel, err := filepath.Rel(base, path)
		if err != nil || !filepath.IsLocal(rel) {
			return nil, errors.New("expanded path outside tree root")
		}
		expanded[filepath.Clean(path)] = true
	}
	entries := []map[string]any{}
	var walk func(string, int) error
	walk = func(relative string, depth int) error {
		if err := ctx.Err(); err != nil {
			return err
		}
		if depth > 32 {
			return errors.New("tree depth exceeds bound")
		}
		file, err := handle.Open(relative)
		if err != nil {
			return err
		}
		children, err := file.ReadDir(MaxDirEntries + 1)
		file.Close()
		if err != nil && err != io.EOF {
			return err
		}
		if len(children) > MaxDirEntries {
			return errors.New("tree directory exceeds entry bound")
		}
		sort.Slice(children, func(i, j int) bool {
			if children[i].IsDir() != children[j].IsDir() {
				return children[i].IsDir()
			}
			return strings.ToLower(children[i].Name()) < strings.ToLower(children[j].Name())
		})
		for _, child := range children {
			if err := ctx.Err(); err != nil {
				return err
			}
			if child.Name() == ".git" {
				continue
			}
			if len(entries) >= 50000 {
				return errors.New("expanded tree exceeds entry bound")
			}
			rel := filepath.Join(relative, child.Name())
			absolute := filepath.Join(base, rel)
			isDir := child.IsDir()
			isExpanded := isDir && expanded[absolute]
			entries = append(entries, map[string]any{"relativePath": filepath.ToSlash(rel), "absolutePath": absolute, "name": child.Name(), "isDir": isDir, "expanded": isExpanded, "depth": depth})
			if isExpanded {
				if err := walk(rel, depth+1); err != nil {
					return err
				}
			}
		}
		return nil
	}
	if err := walk(".", 0); err != nil {
		return nil, err
	}
	return encodeResult(map[string]any{"type": "workingTree", "entries": entries})
}

func listGitDirectory(ctx context.Context, cwd string, c backendstore.Object, ignored bool) (json.RawMessage, error) {
	directory := c.String("directory")
	if directory != "" {
		var err error
		directory, err = cleanRel(directory)
		if err != nil {
			return nil, err
		}
	}
	handle, err := os.OpenRoot(cwd)
	if err != nil {
		return nil, err
	}
	defer handle.Close()
	if directory != "" {
		canonical, err := filepath.EvalSymlinks(filepath.Join(cwd, directory))
		if err != nil {
			return nil, err
		}
		rel, err := filepath.Rel(cwd, canonical)
		if err != nil || !filepath.IsLocal(rel) {
			return nil, errors.New("listing outside repository")
		}
		if _, err := handle.Stat(filepath.Join(directory, ".git")); err == nil {
			return nil, errors.New("directory is a separate Git repository")
		}
	}
	file, err := handle.Open(ifEmpty(directory, "."))
	if err != nil {
		return nil, err
	}
	children, err := file.ReadDir(20001)
	file.Close()
	if err != nil && err != io.EOF {
		return nil, err
	}
	incomplete := len(children) > 20000
	if incomplete {
		children = children[:20000]
	}
	sort.Slice(children, func(i, j int) bool { return children[i].Name() < children[j].Name() })
	prefix := filepath.ToSlash(directory)
	if prefix != "" {
		prefix += "/"
	}
	after := c.String("after")
	cursor := sort.Search(len(children), func(i int) bool { return prefix+children[i].Name() > after })
	args := []string{"ls-files", "--others", "--exclude-standard", "-z"}
	if ignored {
		args = append(args, "--ignored")
	}
	args = append(args, "--")
	if directory != "" {
		args = append(args, ":(literal)"+directory)
	} else {
		args = append(args, ".")
	}
	out, err := runGit(ctx, cwd, args)
	if err != nil {
		return nil, err
	}
	matches := map[string]bool{}
	for _, path := range strings.Split(string(out.stdout), "\x00") {
		if !strings.HasPrefix(path, prefix) {
			continue
		}
		rest := strings.TrimPrefix(path, prefix)
		if rest == "" {
			continue
		}
		name, _, _ := strings.Cut(rest, "/")
		matches[name] = true
	}
	entries := []map[string]any{}
	end := cursor
	for rounds := 0; end < len(children) && rounds < 8; rounds++ {
		limit := min(end+512, len(children))
		for _, child := range children[end:limit] {
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			if child.Name() == ".git" || !matches[child.Name()] {
				continue
			}
			path := prefix + child.Name()
			if child.IsDir() {
				path += "/"
			}
			mark := "?"
			if ignored {
				mark = "!"
			}
			entries = append(entries, map[string]any{"path": path, "origin_path": nil, "index_status": mark, "worktree_status": mark})
		}
		end = limit
		if len(entries) > 0 {
			break
		}
	}
	var next *string
	if end < len(children) && end > 0 {
		value := prefix + children[end-1].Name()
		next = &value
	}
	kind := "untrackedFiles"
	if ignored {
		kind = "ignoredFiles"
	}
	return encodeResult(map[string]any{"type": kind, "entries": entries, "next": next, "incomplete": incomplete && next == nil})
}

func inspectCommit(ctx context.Context, root *confinedRoot, cwd string) (json.RawMessage, error) {
	branch, err := gitText(ctx, cwd, "branch", "--show-current")
	if err != nil {
		return nil, err
	}
	if branch == "" {
		branch, err = gitText(ctx, cwd, "rev-parse", "--short", "HEAD")
		if err != nil {
			branch = "HEAD"
		}
	}
	status, err := runGit(ctx, cwd, []string{"status", "--porcelain=v1", "-z", "--untracked-files=all"})
	if err != nil {
		return nil, err
	}
	staged, unstaged := false, false
	parts := strings.Split(string(status.stdout), "\x00")
	for i := 0; i < len(parts); i++ {
		line := parts[i]
		if len(line) < 3 {
			continue
		}
		staged = staged || line[0] != ' ' && line[0] != '?'
		unstaged = unstaged || line[1] != ' '
		if line[0] == 'R' || line[0] == 'C' {
			i++
		}
	}
	stats, err := reviewStats(ctx, root, cwd, diffSource{kind: "uncommitted"})
	if err != nil {
		return nil, err
	}
	all, err := backendstore.Decode(stats)
	if err != nil {
		return nil, err
	}
	stats, err = reviewStats(ctx, root, cwd, diffSource{kind: "staged"})
	if err != nil {
		return nil, err
	}
	index, err := backendstore.Decode(stats)
	if err != nil {
		return nil, err
	}
	canPush := false
	if upstream, err := gitText(ctx, cwd, "rev-parse", "--abbrev-ref", "@{upstream}"); err == nil && upstream != "" {
		count, err := gitText(ctx, cwd, "rev-list", "--count", upstream+"..HEAD")
		if err == nil {
			n, _ := strconv.ParseUint(count, 10, 64)
			canPush = n > 0
		}
	} else if remotes, err := gitText(ctx, cwd, "remote"); err == nil {
		canPush = remotes != ""
	}
	return encodeResult(map[string]any{"type": "commitSnapshot", "snapshot": map[string]any{"branch": branch, "additions": all["additions"], "deletions": all["deletions"], "staged_additions": index["additions"], "staged_deletions": index["deletions"], "has_staged": staged, "has_unstaged": unstaged, "can_push": canPush}})
}

package backendworkspace

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

type browseDirectoriesOp struct {
	Type          string  `json:"type"`
	Path          *string `json:"path"`
	Base          *string `json:"base"`
	IncludeHidden bool    `json:"includeHidden"`
}

func browseError(path string, err error) error {
	kind := "directory_not_readable"
	if errors.Is(err, os.ErrNotExist) {
		kind = "directory_not_found"
	}
	return fmt.Errorf("%s: %s", kind, path)
}

// Directory pickers have the same filesystem capability as other workspace
// operations. In an agent profile their home and navigation root are isolated.
func browseTarget(root *confinedRoot, path string) (string, string, error) {
	if strings.ContainsRune(path, 0) || !filepath.IsAbs(path) {
		return "", "", fmt.Errorf("directory_not_readable: invalid directory")
	}
	rel, err := filepath.Rel(root.path, path)
	if err != nil || !filepath.IsLocal(rel) {
		return "", "", fmt.Errorf("directory_not_readable: directory outside isolated workspace")
	}
	file, err := root.handle.Open(rel)
	if err != nil {
		return "", "", browseError(path, err)
	}
	info, err := file.Stat()
	file.Close()
	if err != nil {
		return "", "", browseError(path, err)
	}
	if !info.IsDir() {
		return "", "", fmt.Errorf("not_a_directory: %s", path)
	}
	canonical, err := filepath.EvalSymlinks(path)
	if err != nil {
		return "", "", browseError(path, err)
	}
	rel, err = filepath.Rel(root.path, canonical)
	if err != nil || !filepath.IsLocal(rel) {
		return "", "", fmt.Errorf("directory_not_readable: directory outside isolated workspace")
	}
	return canonical, rel, nil
}

func dispatchBrowseDirectories(ctx context.Context, root *confinedRoot, raw json.RawMessage) (json.RawMessage, error) {
	var op browseDirectoriesOp
	if err := decodeOperation(raw, &op, "browseDirectories"); err != nil {
		return nil, err
	}
	target := root.path
	if op.Path != nil {
		path := *op.Path
		slashed := filepath.ToSlash(path)
		switch {
		case slashed == "~":
			target = root.path
		case strings.HasPrefix(slashed, "~/"):
			target = filepath.Join(root.path, filepath.FromSlash(strings.TrimPrefix(slashed, "~/")))
		case filepath.IsAbs(path):
			target = path
		default:
			if op.Base == nil || !filepath.IsAbs(*op.Base) {
				return nil, fmt.Errorf("relative_without_base: relative navigation needs an absolute base directory")
			}
			base, _, err := browseTarget(root, *op.Base)
			if err != nil {
				return nil, err
			}
			target = filepath.Join(base, path)
		}
	}
	target, rel, err := browseTarget(root, target)
	if err != nil {
		return nil, err
	}
	file, err := root.handle.Open(rel)
	if err != nil {
		return nil, browseError(target, err)
	}
	children, err := file.ReadDir(MaxDirEntries + 1)
	file.Close()
	if err != nil && err != io.EOF {
		return nil, browseError(target, err)
	}
	if len(children) > MaxDirEntries {
		return nil, fmt.Errorf("directory_not_readable: directory exceeds entry bound")
	}
	entries := []workingTreeEntry{}
	for _, child := range children {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		name := child.Name()
		if !op.IncludeHidden && strings.HasPrefix(name, ".") {
			continue
		}
		info, err := root.handle.Stat(filepath.Join(rel, name))
		if err != nil || !info.IsDir() {
			continue
		}
		entries = append(entries, workingTreeEntry{RelativePath: name, AbsolutePath: filepath.Join(target, name), Name: name, IsDir: true})
	}
	sort.Slice(entries, func(i, j int) bool {
		a, b := strings.ToLower(entries[i].Name), strings.ToLower(entries[j].Name)
		if a == b {
			return entries[i].Name < entries[j].Name
		}
		return a < b
	})
	var parent *string
	if target != root.path {
		value := filepath.Dir(target)
		parent = &value
	}
	return encodeResult(map[string]any{"type": "directory", "path": target, "parent": parent, "home": root.path, "filesystem_root": root.path, "entries": entries})
}

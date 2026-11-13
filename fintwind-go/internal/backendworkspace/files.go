package backendworkspace

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"unicode/utf8"
)

const (
	// MaxTextFileBytes bounds one readTextFile answer and one
	// writeTextFile payload. Over-limit transfers fail explicitly.
	MaxTextFileBytes = 2 << 20
	// MaxDirEntries bounds one browseDirectory answer.
	MaxDirEntries = 10000
	// MaxListCap bounds listProjectFiles caller caps (FILE_INDEX_CAP).
	MaxListCap = 50000
	// maxWalkDepth mirrors the daemon non-git walk.
	maxWalkDepth = 8
)

// confinedRoot is the canonical isolation root every path resolves under.
type confinedRoot struct {
	path   string
	handle *os.Root
}

func (r *confinedRoot) close() error {
	if r == nil || r.handle == nil {
		return nil
	}
	return r.handle.Close()
}

// confineRoot canonicalizes root and rejects UNC, relative, and missing
// directories before any operation runs.
func confineRoot(root string) (*confinedRoot, error) {
	if strings.ContainsRune(root, 0) {
		return nil, fmt.Errorf("root contains NUL")
	}
	if strings.HasPrefix(root, `\\`) || strings.HasPrefix(root, "//") {
		return nil, fmt.Errorf("UNC roots are not allowed")
	}
	if !filepath.IsAbs(root) {
		return nil, fmt.Errorf("root must be absolute")
	}
	canonical, err := filepath.EvalSymlinks(root)
	if err != nil {
		return nil, fmt.Errorf("root is not usable: %w", err)
	}
	info, err := os.Stat(canonical)
	if err != nil {
		return nil, fmt.Errorf("root is not usable: %w", err)
	}
	if !info.IsDir() {
		return nil, fmt.Errorf("root is not a directory")
	}
	handle, err := os.OpenRoot(canonical)
	if err != nil {
		return nil, fmt.Errorf("confine root: %w", err)
	}
	return &confinedRoot{path: canonical, handle: handle}, nil
}

// cleanRel validates one caller-supplied relative path the way the daemon
// resolver does: every component must be normal, so ".", "..", absolute
// paths, and empty segments never reach the filesystem. The actual open
// still goes through os.Root, which enforces containment at call time.
func cleanRel(raw string) (string, error) {
	if raw == "" {
		return "", fmt.Errorf("workspace path must be a non-empty relative path")
	}
	if strings.ContainsRune(raw, 0) {
		return "", fmt.Errorf("workspace path contains NUL")
	}
	slashed := filepath.ToSlash(raw)
	if filepath.IsAbs(raw) || strings.HasPrefix(slashed, "//") {
		return "", fmt.Errorf("workspace path must be a non-empty relative path")
	}
	parts := strings.Split(slashed, "/")
	for _, part := range parts {
		if part == "" || part == "." || part == ".." {
			return "", fmt.Errorf("workspace path must be a non-empty relative path")
		}
	}
	return filepath.FromSlash(strings.Join(parts, "/")), nil
}

// resolveDir maps an absolute caller directory into a root-relative path
// for Git working directories. Anything outside root fails.
func (r *confinedRoot) resolveDir(raw string) (string, string, error) {
	if strings.ContainsRune(raw, 0) {
		return "", "", fmt.Errorf("directory contains NUL")
	}
	abs := raw
	if !filepath.IsAbs(abs) {
		rel, err := cleanRel(raw)
		if err != nil {
			return "", "", err
		}
		abs = filepath.Join(r.path, rel)
	}
	canonical, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return "", "", fmt.Errorf("directory is not usable: %w", err)
	}
	rel, err := filepath.Rel(r.path, canonical)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return "", "", fmt.Errorf("directory is outside the allowed root")
	}
	if rel == "." {
		return r.path, "", nil
	}
	return canonical, rel, nil
}

// joinRel joins validated relative segments for use with the root handle.
func joinRel(parts ...string) (string, error) {
	cleaned := make([]string, 0, len(parts))
	for _, part := range parts {
		rel, err := cleanRel(part)
		if err != nil {
			return "", err
		}
		cleaned = append(cleaned, rel)
	}
	return filepath.Join(cleaned...), nil
}

func checkCtx(ctx context.Context) error {
	return ctx.Err()
}

// workingTreeEntry mirrors protocol WorkingTreeEntry on the wire.
type workingTreeEntry struct {
	RelativePath string `json:"relativePath"`
	AbsolutePath string `json:"absolutePath"`
	Name         string `json:"name"`
	IsDir        bool   `json:"isDir"`
	Expanded     bool   `json:"expanded"`
	Depth        int    `json:"depth"`
}

func entryAbs(root, rel string) string {
	if rel == "" || rel == "." {
		return root
	}
	return filepath.Join(root, rel)
}

// readRel reads one root-relative file with a byte cap. Limit+1 detects
// overflow so an over-limit file fails instead of truncating silently.
func (r *confinedRoot) readRel(ctx context.Context, rel string) (string, error) {
	if err := checkCtx(ctx); err != nil {
		return "", err
	}
	file, err := r.handle.OpenFile(rel, os.O_RDONLY, 0)
	if err != nil {
		return "", fmt.Errorf("open file: %w", err)
	}
	defer file.Close()
	limited := io.LimitReader(file, MaxTextFileBytes+1)
	data, err := io.ReadAll(limited)
	if err != nil {
		return "", fmt.Errorf("read file: %w", err)
	}
	if len(data) > MaxTextFileBytes {
		return "", fmt.Errorf("file exceeds the %d-byte local read bound", MaxTextFileBytes)
	}
	if !utf8.Valid(data) {
		return "", fmt.Errorf("file is not valid UTF-8")
	}
	return string(data), nil
}

type browseDirectoryOp struct {
	Type string  `json:"type"`
	Path *string `json:"path"`
}

type directoryResult struct {
	Type           string             `json:"type"`
	Path           string             `json:"path"`
	Parent         *string            `json:"parent"`
	Home           string             `json:"home"`
	FilesystemRoot string             `json:"filesystemRoot"`
	Entries        []workingTreeEntry `json:"entries"`
}

func dispatchBrowseDirectory(ctx context.Context, root *confinedRoot, operation json.RawMessage) (json.RawMessage, error) {
	defer root.close()
	var op browseDirectoryOp
	if err := decodeOperation(operation, &op, opBrowseDirectory); err != nil {
		return nil, err
	}
	target := root.path
	rel := ""
	if op.Path != nil && *op.Path != "" {
		raw := *op.Path
		if !filepath.IsAbs(raw) {
			joined, err := joinRel(raw)
			if err != nil {
				return nil, err
			}
			rel = joined
			target = entryAbs(root.path, rel)
			canonical, err := filepath.EvalSymlinks(target)
			if err != nil {
				return nil, fmt.Errorf("directory_not_found: %s", target)
			}
			inside, err := filepath.Rel(root.path, canonical)
			if err != nil || inside == ".." || strings.HasPrefix(inside, ".."+string(filepath.Separator)) {
				return nil, fmt.Errorf("directory is outside the allowed root")
			}
			target = canonical
		} else {
			if strings.HasPrefix(filepath.ToSlash(raw), "//") || strings.HasPrefix(raw, `\\`) {
				return nil, fmt.Errorf("UNC paths are not allowed")
			}
			canonical, err := filepath.EvalSymlinks(raw)
			if err != nil {
				return nil, fmt.Errorf("directory_not_found: %s", raw)
			}
			inside, err := filepath.Rel(root.path, canonical)
			if err != nil || inside == ".." || strings.HasPrefix(inside, ".."+string(filepath.Separator)) {
				return nil, fmt.Errorf("directory is outside the allowed root")
			}
			target = canonical
			rel = inside
			if rel == "." {
				rel = ""
			}
		}
	}
	info, err := os.Stat(target)
	if err != nil {
		return nil, fmt.Errorf("directory_not_found: %s", target)
	}
	if !info.IsDir() {
		return nil, fmt.Errorf("not_a_directory: %s", target)
	}
	dirFile, err := root.handle.Open(ifEmpty(rel, "."))
	if err != nil {
		return nil, fmt.Errorf("directory_not_readable: %s", target)
	}
	names, err := dirFile.ReadDir(-1)
	closeErr := dirFile.Close()
	if err != nil {
		return nil, fmt.Errorf("directory_not_readable: %s", target)
	}
	if closeErr != nil {
		return nil, fmt.Errorf("read directory: %w", closeErr)
	}
	if len(names) > MaxDirEntries {
		return nil, fmt.Errorf("directory exceeds the %d-entry local bound", MaxDirEntries)
	}
	entries := make([]workingTreeEntry, 0, len(names))
	for _, item := range names {
		if err := checkCtx(ctx); err != nil {
			return nil, err
		}
		name := item.Name()
		childRel := name
		if rel != "" {
			childRel = filepath.Join(rel, name)
		}
		isDir := false
		if item.IsDir() {
			isDir = true
		} else if item.Type()&os.ModeSymlink != 0 {
			// Follow one link the way the daemon metadata check does;
			// os.Root containment still applies below on open.
			if stat, err := root.handle.Stat(childRel); err == nil {
				isDir = stat.IsDir()
			} else {
				continue
			}
		} else {
			if stat, err := root.handle.Stat(childRel); err == nil {
				isDir = stat.IsDir()
			} else {
				continue
			}
		}
		entries = append(entries, workingTreeEntry{
			RelativePath: name,
			AbsolutePath: entryAbs(root.path, childRel),
			Name:         name,
			IsDir:        isDir,
		})
	}
	sort.Slice(entries, func(i, j int) bool {
		if entries[i].IsDir != entries[j].IsDir {
			return entries[i].IsDir && !entries[j].IsDir
		}
		return strings.ToLower(entries[i].Name) < strings.ToLower(entries[j].Name)
	})
	volume := filepath.VolumeName(target)
	fsRoot := volume + string(filepath.Separator)
	if volume == "" {
		fsRoot = string(filepath.Separator)
	}
	var parent *string
	if target != fsRoot {
		dir := filepath.Dir(target)
		parent = &dir
	}
	return encodeResult(directoryResult{
		Type:           "directory",
		Path:           target,
		Parent:         parent,
		Home:           root.path,
		FilesystemRoot: fsRoot,
		Entries:        entries,
	})
}

func ifEmpty(value, fallback string) string {
	if value == "" {
		return fallback
	}
	return value
}

type readTextFileOp struct {
	Type         string `json:"type"`
	Root         string `json:"root"`
	RelativePath string `json:"relativePath"`
}

type textFileResult struct {
	Type    string `json:"type"`
	Content string `json:"content"`
}

func dispatchReadTextFile(ctx context.Context, root *confinedRoot, operation json.RawMessage) (json.RawMessage, error) {
	defer root.close()
	var op readTextFileOp
	if err := decodeOperation(operation, &op, opReadTextFile); err != nil {
		return nil, err
	}
	rel, err := scopedFileRel(root, op.Root, op.RelativePath)
	if err != nil {
		return nil, err
	}
	content, err := root.readRel(ctx, rel)
	if err != nil {
		return nil, err
	}
	return encodeResult(textFileResult{Type: "textFile", Content: content})
}

type writeTextFileOp struct {
	Type         string `json:"type"`
	Root         string `json:"root"`
	RelativePath string `json:"relativePath"`
	Content      string `json:"content"`
}

type ackResult struct {
	Type string `json:"type"`
}

func dispatchWriteTextFile(ctx context.Context, root *confinedRoot, operation json.RawMessage) (json.RawMessage, error) {
	defer root.close()
	var op writeTextFileOp
	if err := decodeOperation(operation, &op, opWriteTextFile); err != nil {
		return nil, err
	}
	if len(op.Content) > MaxTextFileBytes {
		return nil, fmt.Errorf("content exceeds the %d-byte local write bound", MaxTextFileBytes)
	}
	rel, err := scopedFileRel(root, op.Root, op.RelativePath)
	if err != nil {
		return nil, err
	}
	if err := checkCtx(ctx); err != nil {
		return nil, err
	}
	file, err := root.handle.OpenFile(rel, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o666)
	if err != nil {
		return nil, fmt.Errorf("write file: %w", err)
	}
	_, writeErr := io.WriteString(file, op.Content)
	closeErr := file.Close()
	if writeErr != nil {
		return nil, fmt.Errorf("write file: %w", writeErr)
	}
	if closeErr != nil {
		return nil, fmt.Errorf("write file: %w", closeErr)
	}
	return encodeResult(ackResult{Type: "ack"})
}

// scopedFileRel constrains an operation root inside the Dispatch root and
// appends a validated relative path. The open itself goes through os.Root.
func scopedFileRel(root *confinedRoot, opRoot, relativePath string) (string, error) {
	base := strings.TrimSpace(opRoot)
	var baseRel string
	if base == "" || base == root.path {
		baseRel = ""
	} else if filepath.IsAbs(base) {
		canonical, err := filepath.EvalSymlinks(base)
		if err != nil {
			return "", fmt.Errorf("operation root is not usable: %w", err)
		}
		inside, err := filepath.Rel(root.path, canonical)
		if err != nil || inside == ".." || strings.HasPrefix(inside, ".."+string(filepath.Separator)) {
			return "", fmt.Errorf("operation root is outside the allowed root")
		}
		if inside == "." {
			baseRel = ""
		} else {
			baseRel = inside
		}
	} else {
		rel, err := cleanRel(base)
		if err != nil {
			return "", err
		}
		baseRel = rel
	}
	rel, err := cleanRel(relativePath)
	if err != nil {
		return "", err
	}
	if baseRel == "" {
		return rel, nil
	}
	return filepath.Join(baseRel, rel), nil
}

type listProjectFilesOp struct {
	Type string `json:"type"`
	Root string `json:"root"`
	Cap  int    `json:"cap"`
}

type fileEntry struct {
	Path  string `json:"path"`
	IsDir bool   `json:"isDir"`
}

type projectFilesResult struct {
	Type    string      `json:"type"`
	Entries []fileEntry `json:"entries"`
}

func dispatchListProjectFiles(ctx context.Context, root *confinedRoot, operation json.RawMessage) (json.RawMessage, error) {
	defer root.close()
	var op listProjectFilesOp
	if err := decodeOperation(operation, &op, opListProjectFiles); err != nil {
		return nil, err
	}
	if op.Cap <= 0 {
		return nil, fmt.Errorf("cap must be positive")
	}
	if op.Cap > MaxListCap {
		return nil, fmt.Errorf("cap exceeds the %d-entry local file index bound", MaxListCap)
	}
	baseRel, err := scopedBaseRel(root, op.Root)
	if err != nil {
		return nil, err
	}
	files, err := gitListedFiles(ctx, root, baseRel, op.Cap)
	if err != nil {
		return nil, err
	}
	if files == nil {
		files, err = walkedFiles(ctx, root, baseRel, op.Cap)
		if err != nil {
			return nil, err
		}
	}
	entries := expandWithDirectories(files)
	if len(entries) > op.Cap {
		entries = entries[:op.Cap]
	}
	if entries == nil {
		entries = []fileEntry{}
	}
	return encodeResult(projectFilesResult{Type: "projectFiles", Entries: entries})
}

// scopedBaseRel constrains an operation root directory for whole-tree reads.
func scopedBaseRel(root *confinedRoot, opRoot string) (string, error) {
	base := strings.TrimSpace(opRoot)
	if base == "" || base == root.path {
		return "", nil
	}
	if !filepath.IsAbs(base) {
		return cleanRel(base)
	}
	canonical, err := filepath.EvalSymlinks(base)
	if err != nil {
		return "", fmt.Errorf("operation root is not usable: %w", err)
	}
	inside, err := filepath.Rel(root.path, canonical)
	if err != nil || inside == ".." || strings.HasPrefix(inside, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("operation root is outside the allowed root")
	}
	if inside == "." {
		return "", nil
	}
	return inside, nil
}

// expandWithDirectories adds parent directory rows and sorts shallow
// entries first, mirroring the daemon project file index.
func expandWithDirectories(files []string) []fileEntry {
	seen := map[string]bool{}
	dirs := []string{}
	for _, file := range files {
		path := file
		for {
			index := strings.LastIndex(path, "/")
			if index < 0 {
				break
			}
			path = path[:index]
			if seen[path] {
				break
			}
			seen[path] = true
			dirs = append(dirs, path)
		}
	}
	entries := make([]fileEntry, 0, len(dirs)+len(files))
	for _, dir := range dirs {
		entries = append(entries, fileEntry{Path: dir + "/", IsDir: true})
	}
	for _, file := range files {
		entries = append(entries, fileEntry{Path: file})
	}
	depth := func(entry fileEntry) int {
		count := strings.Count(entry.Path, "/")
		if entry.IsDir {
			count--
		}
		return count
	}
	sort.Slice(entries, func(i, j int) bool {
		if depth(entries[i]) != depth(entries[j]) {
			return depth(entries[i]) < depth(entries[j])
		}
		return entries[i].Path < entries[j].Path
	})
	return entries
}

var walkSkip = map[string]bool{
	"node_modules": true,
	"target":       true,
	"dist":         true,
	"build":        true,
	"out":          true,
	"vendor":       true,
	"__pycache__":  true,
}

// walkedFiles is the bounded non-git fallback walk. Symlinks resolve
// through os.Root, so a link pointing outside root can never be followed
// out; descended directories stay under root by construction.
func walkedFiles(ctx context.Context, root *confinedRoot, baseRel string, cap int) ([]string, error) {
	type frame struct {
		rel    string
		prefix string
		depth  int
	}
	start := ifEmpty(baseRel, ".")
	files := []string{}
	stack := []frame{{rel: start, prefix: "", depth: 0}}
	for len(stack) > 0 && len(files) < cap {
		if err := checkCtx(ctx); err != nil {
			return nil, err
		}
		top := stack[len(stack)-1]
		stack = stack[:len(stack)-1]
		if top.depth > maxWalkDepth {
			continue
		}
		dir, err := root.handle.Open(top.rel)
		if err != nil {
			continue
		}
		items, err := dir.ReadDir(-1)
		closeErr := dir.Close()
		if err != nil {
			continue
		}
		if closeErr != nil {
			return nil, fmt.Errorf("walk directory: %w", closeErr)
		}
		for _, item := range items {
			if len(files) >= cap {
				break
			}
			name := item.Name()
			if strings.HasPrefix(name, ".") || walkSkip[name] {
				continue
			}
			rel := name
			if top.rel != "." {
				rel = filepath.Join(top.rel, name)
			}
			display := name
			if top.prefix != "" {
				display = top.prefix + "/" + name
			}
			child, err := root.handle.Lstat(rel)
			if err != nil {
				continue
			}
			if child.Mode()&os.ModeSymlink != 0 {
				target, err := root.handle.Stat(rel)
				if err != nil {
					continue
				}
				if target.IsDir() {
					stack = append(stack, frame{rel: rel, prefix: display, depth: top.depth + 1})
				} else if target.Mode().IsRegular() {
					files = append(files, display)
				}
				continue
			}
			if item.IsDir() {
				stack = append(stack, frame{rel: rel, prefix: display, depth: top.depth + 1})
			} else if child.Mode().IsRegular() {
				files = append(files, display)
			}
		}
	}
	return files, nil
}

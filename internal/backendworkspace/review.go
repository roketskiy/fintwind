package backendworkspace

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"fintwind-go/internal/backendstore"
	"github.com/google/uuid"
)

type diffSource struct {
	kind, session string
	turn          int64
	wire          json.RawMessage
}

func parseDiffSource(raw json.RawMessage) (diffSource, error) {
	s := diffSource{wire: raw}
	_ = json.Unmarshal(raw, &s.kind)
	if s.kind == "" {
		var tagged map[string]json.RawMessage
		if err := json.Unmarshal(raw, &tagged); err != nil {
			return s, errors.New("invalid review source")
		}
		body := tagged["lastTurn"]
		if len(body) == 0 {
			return s, errors.New("invalid review source")
		}
		c, err := backendstore.Decode(body)
		if err != nil {
			return s, err
		}
		s.kind = "lastTurn"
		s.session = c.String("session_id")
		if s.session == "" {
			s.session = c.String("sessionId")
		}
		key := "turn_count"
		if c[key] == nil {
			key = "turnCount"
		}
		s.turn, err = c.Int(key)
		if err != nil || s.turn <= 0 || uuid.Validate(s.session) != nil {
			return s, errors.New("completed turn identity required")
		}
	}
	switch s.kind {
	case "uncommitted", "unstaged", "staged", "committed", "branch", "lastTurn":
		return s, nil
	default:
		return s, errors.New("unknown review source")
	}
}
func branchBase(ctx context.Context, cwd string) (string, error) {
	head := resolveCommit(ctx, cwd, "HEAD")
	if head == nil {
		return emptyGitTree, nil
	}
	current := gitBranch(ctx, cwd)
	candidates := []string{}
	if remote, err := gitText(ctx, cwd, "symbolic-ref", "--short", "refs/remotes/origin/HEAD"); err == nil && remote != "" {
		candidates = append(candidates, strings.TrimPrefix(remote, "origin/"))
	}
	candidates = append(candidates, "main", "master")
	for _, branch := range candidates {
		if current != nil && *current == "refs/heads/"+branch {
			continue
		}
		if resolveCommit(ctx, cwd, "refs/heads/"+branch) == nil {
			continue
		}
		return gitText(ctx, cwd, "merge-base", "HEAD", branch)
	}
	return *head, nil
}
func reviewRange(ctx context.Context, root *confinedRoot, cwd string, s diffSource) (string, string, error) {
	head := emptyGitTree
	if resolved := resolveCommit(ctx, cwd, "HEAD"); resolved != nil {
		head = *resolved
	}
	if s.kind == "lastTurn" {
		from := ""
		for _, ref := range []string{checkpointRef(s.session, "diff-", s.turn), checkpointRef(s.session, "start-", s.turn), checkpointRef(s.session, "", s.turn-1)} {
			if value := resolveCommit(ctx, cwd, ref); value != nil {
				from = *value
				break
			}
		}
		to := resolveCommit(ctx, cwd, checkpointRef(s.session, "", s.turn))
		if from == "" || to == nil {
			return "", "", errors.New("turn checkpoint unavailable")
		}
		return from, *to, nil
	}
	from := head
	if s.kind == "branch" || s.kind == "committed" {
		var err error
		from, err = branchBase(ctx, cwd)
		if err != nil {
			return "", "", err
		}
	}
	if s.kind == "committed" {
		return from, head, nil
	}
	if s.kind == "staged" || s.kind == "unstaged" {
		index, err := gitText(ctx, cwd, "write-tree")
		if err != nil {
			return "", "", err
		}
		if s.kind == "staged" {
			return from, index, nil
		}
		from = index
	}
	to, err := snapshotCommit(ctx, root, cwd, resolveCommit(ctx, cwd, "HEAD"), "fintwind review snapshot", nil)
	return from, to, err
}
func diffArgs() []string {
	return []string{"diff", "--no-ext-diff", "--no-textconv", "--no-color", "--no-renames"}
}

func dispatchReview(ctx context.Context, root *confinedRoot, raw json.RawMessage, queries *Queries) (json.RawMessage, bool, error) {
	c, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	kind := c.String("type")
	switch kind {
	case "collectReviewDiff", "collectReviewFileDiff", "collectReviewStats", "cancelGitQuery", "cancelReviewFileDiff":
	default:
		return nil, false, nil
	}
	defer root.close()
	if kind == "cancelGitQuery" || kind == "cancelReviewFileDiff" {
		if queries == nil {
			return nil, true, errors.New("daemon query registry required")
		}
		if err := queries.Cancel(c.String("queryId")); err != nil {
			return nil, true, err
		}
		response, err := encodeResult(map[string]string{"type": "ack"})
		return response, true, err
	}
	if c.String("queryId") != "" {
		if queries == nil {
			return nil, true, errors.New("daemon query registry required")
		}
		child, finish, err := queries.Begin(ctx, c.String("queryId"))
		if err != nil {
			return nil, true, err
		}
		defer finish()
		ctx = child
	}
	source, err := parseDiffSource(c["source"])
	if err != nil {
		return nil, true, err
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
		return nil, true, errors.New("repository root outside isolated workspace")
	}
	cwd = top
	if kind == "collectReviewFileDiff" {
		response, err := reviewFile(ctx, root, cwd, source, c)
		return response, true, err
	}
	if kind == "collectReviewStats" {
		response, err := reviewStats(ctx, root, cwd, source)
		return response, true, err
	}
	from, to, err := reviewRange(ctx, root, cwd, source)
	if err != nil {
		return nil, true, err
	}
	run := func(mode string) (gitOutput, error) {
		args := append(diffArgs(), mode, from, to, "--", ".")
		return runGit(ctx, cwd, args)
	}
	stat, err := run("--numstat")
	if err != nil {
		return nil, true, err
	}
	patch, err := run("--unified=2147483647")
	complete := err == nil
	if err != nil {
		if !strings.Contains(err.Error(), "exceeds") {
			return nil, true, err
		}
		patch, err = run("--unified=3")
		if err != nil {
			return nil, true, err
		}
	}
	response, err := encodeResult(map[string]any{"type": "reviewDiff", "data": map[string]any{"source": source.wire, "numstat": string(stat.stdout), "patch": string(patch.stdout), "completeContext": complete, "truncated": !complete, "file": nil}})
	return response, true, err
}

func reviewFile(ctx context.Context, root *confinedRoot, cwd string, s diffSource, c backendstore.Object) (json.RawMessage, error) {
	if s.kind != "uncommitted" && s.kind != "unstaged" && s.kind != "staged" {
		return nil, errors.New("single-file diff requires live source")
	}
	path, err := cleanRel(c.String("path"))
	if err != nil {
		return nil, err
	}
	origin := c.String("originPath")
	if origin != "" {
		if _, err := cleanRel(origin); err != nil {
			return nil, err
		}
	}
	// Any existing path must resolve inside this repository, even if the caller
	// lies about untracked. Missing paths remain valid for deletion diffs.
	for _, relative := range []string{path, origin} {
		if relative == "" {
			continue
		}
		absolute := filepath.Join(cwd, relative)
		canonical, err := filepath.EvalSymlinks(absolute)
		if err == nil {
			rel, err := filepath.Rel(cwd, canonical)
			if err != nil || !filepath.IsLocal(rel) {
				return nil, errors.New("review file outside repository")
			}
		} else if !errors.Is(err, os.ErrNotExist) {
			return nil, err
		}
	}
	noIndex := c.Bool("untracked") && s.kind != "staged" || s.kind == "uncommitted" && resolveCommit(ctx, cwd, "HEAD") == nil
	contextLines := int64(3)
	if c["contextLines"] != nil {
		contextLines, err = c.Int("contextLines")
		if err != nil {
			return nil, err
		}
	}
	contextLines = max(3, min(contextLines, 100000))
	run := func(mode string) (gitOutput, error) {
		args := []string{"diff", "--no-ext-diff", "--no-textconv", "--no-color", "--find-renames", mode}
		if noIndex {
			args = append(args, "--no-index", "--", os.DevNull, filepath.ToSlash(path))
		} else {
			switch s.kind {
			case "staged":
				args = append(args, "--cached")
				if resolveCommit(ctx, cwd, "HEAD") == nil {
					args = append(args, emptyGitTree)
				}
			case "uncommitted":
				args = append(args, "HEAD")
			}
			args = append(args, "--")
			if origin != "" {
				args = append(args, ":(literal)"+origin)
			}
			args = append(args, ":(literal)"+path)
		}
		return runGitOutput(ctx, cwd, args, nil, nil, noIndex)
	}
	stat, err := run("--numstat")
	if err != nil {
		return nil, err
	}
	a, d, binary := uint64(0), uint64(0), false
	first, _, _ := strings.Cut(string(stat.stdout), "\n")
	columns := strings.SplitN(first, "\t", 3)
	if len(columns) == 3 {
		a, _ = strconv.ParseUint(columns[0], 10, 64)
		d, _ = strconv.ParseUint(columns[1], 10, 64)
		binary = columns[0] == "-" || columns[1] == "-"
	}
	patch, err := run(fmt.Sprintf("--unified=%d", contextLines))
	if err != nil {
		return nil, err
	}
	return encodeResult(map[string]any{"type": "reviewDiff", "data": map[string]any{"source": s.wire, "numstat": "", "patch": string(patch.stdout), "completeContext": false, "truncated": false, "file": map[string]any{"path": c.String("path"), "additions": a, "deletions": d, "binary": binary}}})
}

func reviewStats(ctx context.Context, root *confinedRoot, cwd string, s diffSource) (json.RawMessage, error) {
	head := emptyGitTree
	if resolved := resolveCommit(ctx, cwd, "HEAD"); resolved != nil {
		head = *resolved
	}
	args := append(diffArgs(), "--numstat", "-z")
	switch s.kind {
	case "uncommitted":
		args = append(args, head)
	case "staged":
		args = append(args, "--cached", head)
	case "committed", "branch":
		base, err := branchBase(ctx, cwd)
		if err != nil {
			return nil, err
		}
		args = append(args, base)
		if s.kind == "committed" {
			args = append(args, head)
		}
	case "lastTurn":
		from, to, err := reviewRange(ctx, root, cwd, s)
		if err != nil {
			return nil, err
		}
		args = append(args, from, to)
	}
	args = append(args, "--", ".")
	out, err := runGit(ctx, cwd, args)
	if err != nil {
		return nil, err
	}
	var additions, deletions, files uint64
	truncated := false
	for _, row := range strings.Split(string(out.stdout), "\x00") {
		parts := strings.SplitN(row, "\t", 3)
		if len(parts) != 3 {
			continue
		}
		a, _ := strconv.ParseUint(parts[0], 10, 64)
		d, _ := strconv.ParseUint(parts[1], 10, 64)
		additions += a
		deletions += d
		files++
	}
	if s.kind == "uncommitted" || s.kind == "unstaged" || s.kind == "branch" {
		out, err := runGit(ctx, cwd, []string{"ls-files", "--others", "--exclude-standard", "-z"})
		if err != nil {
			return nil, err
		}
		handle, err := os.OpenRoot(cwd)
		if err != nil {
			return nil, err
		}
		defer handle.Close()
		total := 0
		for _, path := range strings.Split(string(out.stdout), "\x00") {
			if path == "" {
				continue
			}
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			if files >= 5000 || total >= 32<<20 {
				truncated = true
				break
			}
			if _, err := cleanRel(path); err != nil {
				truncated = true
				continue
			}
			file, err := handle.Open(path)
			if err != nil {
				truncated = true
				continue
			}
			info, err := file.Stat()
			if err != nil || !info.Mode().IsRegular() {
				file.Close()
				truncated = true
				continue
			}
			raw, err := io.ReadAll(io.LimitReader(file, (2<<20)+1))
			file.Close()
			if err != nil || len(raw) > 2<<20 {
				truncated = true
				continue
			}
			total += len(raw)
			files++
			if strings.IndexByte(string(raw), 0) >= 0 {
				continue
			}
			for _, b := range raw {
				if b == '\n' {
					additions++
				}
			}
			if len(raw) > 0 && raw[len(raw)-1] != '\n' {
				additions++
			}
		}
	}
	return encodeResult(map[string]any{"type": "reviewStats", "additions": additions, "deletions": deletions, "files": files, "truncated": truncated})
}

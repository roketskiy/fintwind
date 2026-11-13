package backendworkspace

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"fintwind-go/internal/backendstore"
	"github.com/google/uuid"
)

func dispatchWorktree(ctx context.Context, root *confinedRoot, raw json.RawMessage) (json.RawMessage, bool, error) {
	c, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	if c.String("type") != "createWorktree" {
		return nil, false, nil
	}
	defer root.close()
	project, session := c.String("projectId"), c.String("sessionId")
	if uuid.Validate(project) != nil || uuid.Validate(session) != nil {
		return nil, true, errors.New("valid project and session required")
	}
	cwd, _, err := root.resolveDir(c.String("projectPath"))
	if err != nil {
		return nil, true, err
	}
	repository, repo, err := gitToplevel(ctx, cwd)
	if err != nil {
		return nil, true, err
	}
	if !repo {
		return nil, true, errors.New("new worktrees require a Git repository")
	}
	if _, _, err := root.resolveDir(repository); err != nil {
		return nil, true, errors.New("repository outside isolated workspace")
	}
	projectRelative, err := filepath.Rel(repository, cwd)
	if err != nil || !filepath.IsLocal(projectRelative) {
		return nil, true, errors.New("project outside repository")
	}
	base := strings.TrimSpace(c.String("baseBranch"))
	if strings.HasPrefix(base, "-") || strings.ContainsAny(base, "\x00\r\n") {
		return nil, true, errors.New("invalid worktree base")
	}
	if base == "" {
		if remote, err := gitText(ctx, repository, "symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"); err == nil && remote != "" {
			base = remote
			local := strings.TrimPrefix(remote, "origin/")
			if resolveCommit(ctx, repository, "refs/heads/"+local) != nil {
				base = local
			}
		}
		if base == "" {
			base, _ = gitText(ctx, repository, "branch", "--show-current")
		}
		if base == "" {
			base = "HEAD"
		}
	}
	commit := resolveCommit(ctx, repository, base)
	if commit == nil {
		return nil, true, errors.New("worktree base unavailable")
	}
	words := strings.FieldsFunc(strings.ToLower(c.String("prompt")), func(r rune) bool { return !(r >= 'a' && r <= 'z' || r >= '0' && r <= '9') })
	if len(words) > 6 {
		words = words[:6]
	}
	slug := strings.Join(words, "-")
	if len(slug) > 48 {
		slug = slug[:48]
	}
	if slug == "" {
		slug = "new-worktree"
	}
	mutationGate.Lock()
	defer mutationGate.Unlock()
	relativeRoot := filepath.Join(".fintwind", "worktrees", project)
	if err := root.handle.MkdirAll(relativeRoot, 0o700); err != nil {
		return nil, true, err
	}
	worktreeRoot, _, err := root.resolveDir(filepath.Join(root.path, relativeRoot))
	if err != nil {
		return nil, true, err
	}
	for index := 0; index <= 100; index++ {
		if err := ctx.Err(); err != nil {
			return nil, true, err
		}
		name := slug
		if index > 0 {
			name = fmt.Sprintf("%s-%d", slug, index+1)
		}
		if index == 100 {
			name = slug + "-" + strings.ReplaceAll(session, "-", "")[:8]
		}
		branch := "fintwind/" + name
		path := filepath.Join(worktreeRoot, name)
		if _, err := os.Lstat(path); err == nil {
			continue
		} else if !errors.Is(err, os.ErrNotExist) {
			return nil, true, err
		}
		if resolveCommit(ctx, repository, "refs/heads/"+branch) != nil {
			continue
		}
		// Resolve the explicit base to its commit before crossing the mutation
		// boundary. User ref text never becomes an option or a shell command.
		if _, err := runGit(ctx, repository, []string{"worktree", "add", "-b", branch, path, *commit}); err != nil {
			return nil, true, err
		}
		result := filepath.Join(path, projectRelative)
		if info, err := os.Stat(result); err != nil || !info.IsDir() {
			return nil, true, errors.New("worktree project directory missing after creation")
		}
		response, err := encodeResult(map[string]any{"type": "worktreeCreated", "worktree": map[string]string{"path": result, "branch": branch}})
		return response, true, err
	}
	return nil, true, errors.New("no unique worktree name available")
}

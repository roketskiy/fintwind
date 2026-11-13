package backendworkspace

import (
	"context"
	"errors"
	"strings"
	"unicode/utf8"
)

func CommitPrompt(ctx context.Context, workspace, directory string, includeUnstaged bool) (string, error) {
	root, err := confineRoot(workspace)
	if err != nil {
		return "", err
	}
	defer root.close()
	cwd, _, err := root.resolveDir(directory)
	if err != nil {
		return "", err
	}
	top, repo, err := gitToplevel(ctx, cwd)
	if err != nil {
		return "", err
	}
	if !repo {
		return "", errors.New("workspace is not a Git repository")
	}
	if _, _, err := root.resolveDir(top); err != nil {
		return "", err
	}
	mutationGate.Lock()
	defer mutationGate.Unlock()
	status, err := gitText(ctx, cwd, "diff", "--cached", "--name-status", "--")
	if err != nil {
		return "", err
	}
	staged, err := gitText(ctx, cwd, "diff", "--cached", "--no-ext-diff", "--no-color", "--no-textconv", "--")
	if err != nil {
		return "", err
	}
	unstaged := ""
	if includeUnstaged {
		status, err = gitText(ctx, cwd, "status", "--short", "--untracked-files=all")
		if err != nil {
			return "", err
		}
		unstaged, err = gitText(ctx, cwd, "diff", "--no-ext-diff", "--no-color", "--no-textconv", "--")
		if err != nil {
			return "", err
		}
	}
	if strings.TrimSpace(status+staged+unstaged) == "" {
		return "", errors.New("there are no changes to describe")
	}
	content := "Git status:\n" + status + "\n\nStaged diff:\n" + staged
	if includeUnstaged {
		content += "\n\nUnstaged diff (will be included):\n" + unstaged
	}
	note := ""
	if len(content) > 96<<10 {
		end := 96 << 10
		for end > 0 && !utf8.RuneStart(content[end]) {
			end--
		}
		content = content[:end] + "\n\n[diff truncated]"
		note = " The diff was truncated, so summarize only what is supported by the visible context."
	}
	return "Generate a concise Git commit subject for the changes below.\nReturn exactly one line and nothing else: no quotes, Markdown, prefix, explanation, or trailing period.\nUse imperative mood and at most 72 characters. Do not call tools; all context is included here." + note + "\n\n" + content, nil
}

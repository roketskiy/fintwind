package backendworkspace

import (
	"context"
	"strings"
)

func PullRequestContext(ctx context.Context, workspace, directory string) (string, string, [][2]string, bool, error) {
	root, err := confineRoot(workspace)
	if err != nil {
		return "", "", nil, false, err
	}
	defer root.close()
	cwd, _, err := root.resolveDir(directory)
	if err != nil {
		return "", "", nil, false, err
	}
	top, repo, err := gitToplevel(ctx, cwd)
	if err != nil || !repo {
		return cwd, "", nil, repo, err
	}
	if _, _, err := root.resolveDir(top); err != nil {
		return "", "", nil, false, err
	}
	branch, err := gitText(ctx, cwd, "branch", "--show-current")
	if err != nil {
		return "", "", nil, false, err
	}
	output, err := gitText(ctx, cwd, "remote", "-v")
	if err != nil {
		return "", "", nil, false, err
	}
	remotes := [][2]string{}
	for _, line := range strings.Split(output, "\n") {
		parts := strings.Fields(line)
		if len(parts) == 3 && parts[2] == "(fetch)" {
			remotes = append(remotes, [2]string{parts[0], parts[1]})
		}
	}
	return cwd, strings.TrimSpace(branch), remotes, true, nil
}

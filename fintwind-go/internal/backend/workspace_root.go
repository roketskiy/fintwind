package backend

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/backendworkspace"
)

// workspaceOperation chooses the filesystem volume for each daily operation;
// projects and worktrees can reside on different drives.
func (s *Server) workspaceOperation(raw json.RawMessage) (string, json.RawMessage, error) {
	if !s.daily {
		return s.WorkspaceRoot, raw, nil
	}
	normalized, err := backendworkspace.NormalizeOperation(raw)
	if err != nil {
		return "", nil, err
	}
	op, err := backendstore.Decode(normalized)
	if err != nil {
		return "", nil, err
	}
	path := ""
	switch op.String("type") {
	case "browseDirectory", "browseDirectories":
		path = op.String("path")
		home, err := os.UserHomeDir()
		if err != nil {
			return "", nil, err
		}
		if path == "" || path == "~" {
			path = home
		} else if strings.HasPrefix(path, "~/") || strings.HasPrefix(path, `~\`) {
			path = filepath.Join(home, path[2:])
		} else if !filepath.IsAbs(path) && op.String("base") != "" {
			path = filepath.Join(op.String("base"), path)
		}
		op.Set("path", path)
		normalized, err = json.Marshal(op)
		if err != nil {
			return "", nil, err
		}
	case "readTextFile", "writeTextFile", "listProjectFiles", "listTree":
		path = op.String("root")
	case "discoverSlashCommands":
		path = op.String("projectRoot")
	case "createWorktree":
		path = op.String("projectPath")
		if filepath.IsAbs(path) {
			return filepath.Dir(path), normalized, nil
		}
	default:
		path = op.String("cwd")
	}
	if !filepath.IsAbs(path) {
		path = s.Cwd
	}
	return s.workspaceForPath(path), normalized, nil
}

func (s *Server) workspaceForPath(path string) string {
	if !s.daily {
		return s.WorkspaceRoot
	}
	if !filepath.IsAbs(path) {
		path = s.Cwd
	}
	return filepath.VolumeName(path) + string(filepath.Separator)
}

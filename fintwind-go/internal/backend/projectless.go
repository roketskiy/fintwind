package backend

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"fintwind-go/internal/backendstore"
)

func (s *Server) generalWorkspace(prepare bool) (string, error) {
	s.fileMu.Lock()
	settings, err := readObject(s.settingsPath)
	s.fileMu.Unlock()
	if err != nil {
		return "", err
	}
	path := settings.String("general_workspace_path")
	if path == "" {
		path = filepath.Join(s.WorkspaceRoot, "projects")
	}
	if !filepath.IsAbs(path) {
		return "", errors.New("general workspace requires absolute path")
	}
	if s.daily {
		if prepare {
			if err := os.MkdirAll(path, 0o700); err != nil {
				return "", err
			}
			return filepath.EvalSymlinks(path)
		}
		return path, nil
	}
	rel, err := filepath.Rel(s.WorkspaceRoot, path)
	if err != nil || !filepath.IsLocal(rel) {
		return "", errors.New("general workspace outside isolated workspace")
	}
	if prepare {
		root, err := os.OpenRoot(s.WorkspaceRoot)
		if err != nil {
			return "", err
		}
		defer root.Close()
		if err := root.MkdirAll(rel, 0o700); err != nil {
			return "", err
		}
		info, err := root.Lstat(rel)
		if err != nil || !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
			return "", errors.New("general workspace must be a real directory")
		}
		canonical, err := filepath.EvalSymlinks(path)
		if err != nil {
			return "", err
		}
		check, err := filepath.Rel(s.WorkspaceRoot, canonical)
		if err != nil || !filepath.IsLocal(check) {
			return "", errors.New("general workspace escapes isolation")
		}
		return canonical, nil
	}
	return path, nil
}

func isLegacyDate(value string) bool {
	if len(value) != 10 || value[4] != '-' || value[7] != '-' {
		return false
	}
	for index, b := range []byte(value) {
		if index == 4 || index == 7 {
			continue
		}
		if b < '0' || b > '9' {
			return false
		}
	}
	return true
}

func (s *Server) projectlessOperation(ctx context.Context, raw json.RawMessage) (backendstore.Object, bool, error) {
	c, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	kind := c.String("type")
	if kind != "createProjectlessWorkspace" && kind != "migrateProjectlessWorkspace" {
		return nil, false, nil
	}
	if err := ctx.Err(); err != nil {
		return nil, true, err
	}
	root, err := s.generalWorkspace(true)
	if err != nil {
		return nil, true, err
	}
	reply := func(path string) (backendstore.Object, bool, error) {
		return backendstore.Reply("workspace", backendstore.Fields(map[string]any{"result": map[string]any{"type": "projectlessWorkspace", "cwd": path}})), true, nil
	}
	if kind == "createProjectlessWorkspace" {
		return reply(root)
	}
	path := c.String("path")
	if !filepath.IsAbs(path) {
		return nil, true, errors.New("legacy workspace requires absolute path")
	}
	profile := filepath.Dir(s.Directory)
	legacy := filepath.Join(profile, "home", ".fintwind")
	validate := func(path string) error {
		canonical, err := filepath.EvalSymlinks(path)
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(profile, canonical)
		if err != nil || !filepath.IsLocal(rel) {
			return errors.New("legacy workspace outside isolated profile")
		}
		info, err := os.Lstat(path)
		if err != nil {
			return err
		}
		if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
			return errors.New("legacy workspace must be a real directory")
		}
		return nil
	}
	if rel, err := filepath.Rel(root, path); err == nil && filepath.IsLocal(rel) {
		if err := validate(path); err != nil {
			return nil, true, err
		}
		return reply(path)
	}
	if filepath.Clean(path) == legacy {
		return reply(root)
	}
	rel, err := filepath.Rel(legacy, path)
	if err != nil || !filepath.IsLocal(rel) {
		return nil, true, errors.New("path is not an isolated legacy workspace")
	}
	parts := strings.Split(filepath.ToSlash(rel), "/")
	if len(parts) != 2 || !isLegacyDate(parts[0]) || parts[1] == "." || parts[1] == ".." {
		return nil, true, errors.New("path is not a dated legacy workspace")
	}
	if err := validate(path); err != nil {
		return nil, true, err
	}
	handle, err := os.OpenRoot(profile)
	if err != nil {
		return nil, true, err
	}
	defer handle.Close()
	sourceRel, err := filepath.Rel(profile, path)
	if err != nil {
		return nil, true, err
	}
	datePath := filepath.Join(root, parts[0])
	destinationRel, err := filepath.Rel(profile, datePath)
	if err != nil || !filepath.IsLocal(destinationRel) {
		return nil, true, errors.New("migration destination outside profile")
	}
	if err := handle.MkdirAll(destinationRel, 0o700); err != nil {
		return nil, true, err
	}
	for index := 0; index < 100; index++ {
		if err := ctx.Err(); err != nil {
			return nil, true, err
		}
		name := parts[1]
		if index > 0 {
			name = fmt.Sprintf("%s-%d", name, index+1)
		}
		dest := filepath.Join(destinationRel, name)
		if _, err := handle.Lstat(dest); err == nil {
			continue
		} else if !errors.Is(err, os.ErrNotExist) {
			return nil, true, err
		}
		if err := handle.Rename(sourceRel, dest); err != nil {
			if errors.Is(err, os.ErrExist) {
				continue
			}
			return nil, true, err
		}
		return reply(filepath.Join(profile, dest))
	}
	return nil, true, errors.New("no unique legacy workspace destination")
}

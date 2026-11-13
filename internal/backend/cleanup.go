package backend

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"fintwind-go/internal/backendservices"
	"fintwind-go/internal/backendstore"
	"github.com/google/uuid"
)

func (s *Server) cleanupBlocked(ctx context.Context, locals []backendstore.Object) (map[string]bool, error) {
	blocked := map[string]bool{}
	s.fileMu.Lock()
	drafts, err := readObject(filepath.Join(s.Directory, "composer-drafts.json"))
	s.fileMu.Unlock()
	if err != nil {
		return nil, err
	}
	if raw := drafts["sessions"]; len(raw) > 0 && string(raw) != "null" {
		var entries map[string]json.RawMessage
		if err := json.Unmarshal(raw, &entries); err != nil {
			return nil, err
		}
		for id := range entries {
			blocked[id] = true
		}
	}
	for _, session := range locals {
		id := session.String("id")
		attached, err := s.runtime.Handle(ctx, id, "", backendstore.Fields(map[string]any{"type": "attachSession"}))
		if err != nil {
			return nil, err
		}
		if attached.String("runtimeId") != "" {
			blocked[id] = true
		}
	}
	return blocked, nil
}

func (s *Server) cleanupCommand(ctx context.Context, sessionID string, c backendstore.Object) (backendstore.Object, error) {
	preview := c.String("type") == "previewSessionCleanup"
	before := int64(0)
	if preview {
		days := int64(0)
		switch c.String("period") {
		case "Days30":
			days = 30
		case "Days90":
			days = 90
		case "Days180":
			days = 180
		default:
			return nil, errors.New("unknown cleanup retention period")
		}
		before = time.Now().Unix() - days*86400
	} else {
		var err error
		before, err = c.Int("before")
		if err != nil || before <= 0 || before > time.Now().Unix()-30*86400 {
			return nil, errors.New("cleanup requires at least 30 days retention")
		}
		if uuid.Validate(sessionID) != nil || sessionID == uuid.Nil.String() {
			return nil, errors.New("cleanup task identity required")
		}
	}
	skip := func() backendstore.Object {
		return backendstore.Reply("sessionCleanupFinished", backendstore.Fields(map[string]any{"outcome": "Skipped"}))
	}
	state, err := s.Store.Load(ctx, s.Cwd)
	if err != nil {
		return nil, err
	}
	locals, err := taskRows(state, "sessions")
	if err != nil {
		return nil, err
	}
	blocked, err := s.cleanupBlocked(ctx, locals)
	if err != nil {
		return nil, err
	}
	var visible []string
	wanted := map[string]bool{}
	if preview {
		if err := json.Unmarshal(c["visibleSessionIds"], &visible); err != nil {
			return nil, err
		}
		if len(visible) > 100000 {
			return nil, errors.New("cleanup visible set exceeds budget")
		}
		for _, id := range visible {
			if uuid.Validate(id) != nil {
				return nil, errors.New("invalid visible task identity")
			}
			wanted[id] = true
		}
	} else {
		wanted[sessionID] = true
	}
	// Load only potentially old details, one at a time. A detail rejected by
	// TaskEditSnapshot has an unsettled receipt and must block its entire tree.
	fingerprints := map[string]string{}
	for i, row := range locals {
		if !backendservices.CleanupLocalEligible(row, uint64(before)) {
			continue
		}
		detail, fingerprint, err := s.Store.TaskEditSnapshot(ctx, row.String("id"))
		if err != nil {
			blocked[row.String("id")] = true
			continue
		}
		locals[i] = detail
		fingerprints[row.String("id")] = fingerprint
	}
	if !preview {
		found := false
		for _, row := range locals {
			if row.String("id") == sessionID && backendservices.CleanupLocalEligible(row, uint64(before)) && !blocked[sessionID] {
				found = true
			}
		}
		if !found {
			return skip(), nil
		}
	}
	directory, err := s.generalWorkspace(true)
	if err != nil {
		return nil, err
	}
	api, err := s.servicePool.acquire(ctx, backendstore.Fields(map[string]any{"binary": c.String("binary"), "directory": directory}))
	if err != nil {
		return nil, err
	}
	inventory, err := backendservices.ReadCleanupCatalog(ctx, api)
	if err != nil {
		return nil, err
	}
	if preview {
		ids := []string{}
		for _, row := range locals {
			if wanted[row.String("id")] && inventory.Eligible(row, locals, blocked, uint64(before)) != nil {
				ids = append(ids, row.String("id"))
			}
		}
		return backendstore.Reply("sessionCleanupPreview", backendstore.Fields(map[string]any{"preview": map[string]any{"before": before, "session_ids": ids}})), nil
	}
	var source backendstore.Object
	for _, row := range locals {
		if row.String("id") == sessionID {
			source = row
			break
		}
	}
	tree := inventory.Eligible(source, locals, blocked, uint64(before))
	if tree == nil {
		return skip(), nil
	}
	expected := map[string]string{}
	deleted := []string{}
	projectIDs := map[string]bool{}
	for _, row := range locals {
		if tree[row.String("native_session_id")] {
			id := row.String("id")
			if fingerprints[id] == "" {
				return skip(), nil
			}
			expected[id] = fingerprints[id]
			deleted = append(deleted, id)
			projectIDs[row.String("project_id")] = true
		}
	}
	ok, err := backendservices.RecheckCleanup(ctx, api, tree, uint64(before))
	if err != nil {
		return nil, err
	}
	if !ok {
		return skip(), nil
	}
	if err := backendservices.DeleteCleanupRoot(ctx, api, source.String("native_session_id")); err != nil {
		return nil, err
	}
	// Only daemon-owned general-workspace projects are disposable metadata;
	// a user's ordinary project remains even after its final task is removed.
	projects, err := taskRows(state, "projects")
	if err != nil {
		return nil, err
	}
	empty := []string{}
	for _, project := range projects {
		path := filepath.Clean(project.String("path"))
		legacy := filepath.Join(filepath.Dir(s.Directory), "home", ".fintwind")
		if s.daily {
			if home, err := os.UserHomeDir(); err == nil {
				legacy = filepath.Join(home, ".fintwind")
			}
		}
		if projectIDs[project.String("id")] && cleanupGeneralPath(path, directory, legacy) {
			empty = append(empty, project.String("id"))
		}
	}
	if err := s.Store.RemoveCleaned(ctx, expected, empty); err != nil {
		return nil, err
	}
	sort.Strings(deleted)
	s.notify("taskStateChanged")
	return backendstore.Reply("sessionCleanupFinished", backendstore.Fields(map[string]any{"outcome": map[string]any{"Deleted": map[string]any{"session_ids": deleted}}})), nil
}

func cleanupGeneralPath(path, root, legacy string) bool {
	if relative, err := filepath.Rel(root, path); err == nil && filepath.IsLocal(relative) {
		return true
	}
	if path == legacy {
		return true
	}
	relative, err := filepath.Rel(legacy, path)
	if err != nil || !filepath.IsLocal(relative) {
		return false
	}
	first, _, _ := strings.Cut(filepath.ToSlash(relative), "/")
	return first == "projects" || isLegacyDate(first)
}

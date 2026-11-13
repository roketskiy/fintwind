package backend

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"time"
	"unicode/utf8"

	"fintwind-go/internal/backendstore"
)

func projectOperationReply(o backendstore.ProjectOperation) backendstore.Object {
	state := o.State
	if state == "preparing" || state == "running" {
		state = "needsReconciliation"
	}
	fields := map[string]any{"type": "projectOperation", "operation_id": o.ID, "state": state, "path": o.Path}
	if o.Failure != nil {
		fields["failure_kind"] = *o.Failure
	}
	return backendstore.Reply("workspace", backendstore.Fields(map[string]any{"result": fields}))
}

func (s *Server) projectOperation(ctx context.Context, raw json.RawMessage) (backendstore.Object, bool, error) {
	c, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	kind := c.String("type")
	if kind != "createEmptyProject" && kind != "getProjectOperation" {
		return nil, false, nil
	}
	idKey := "operation_id"
	if c[idKey] == nil {
		idKey = "operationId"
	}
	id, err := c.ID(idKey)
	if err != nil {
		return nil, true, errors.New("invalid_target: invalid operation identity")
	}
	if kind == "getProjectOperation" {
		o, err := s.Store.GetProjectOperation(ctx, id)
		if err != nil {
			return nil, true, err
		}
		return projectOperationReply(o), true, nil
	}
	name := c.String("project_name")
	if c["project_name"] == nil {
		name = c.String("projectName")
	}
	if name == "" || name == "." || name == ".." || strings.HasSuffix(name, " ") || strings.HasSuffix(name, ".") || !utf8.ValidString(name) || strings.ContainsAny(name, "/\\\x00") {
		return nil, true, errors.New("invalid_name: project name must be one path segment")
	}
	parent := c.String("parent_dir")
	if c["parent_dir"] == nil {
		parent = c.String("parentDir")
	}
	if !filepath.IsAbs(parent) {
		return nil, true, errors.New("invalid_parent: absolute parent directory required")
	}
	canonical, err := filepath.EvalSymlinks(parent)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, true, errors.New("parent_missing: parent directory does not exist")
		}
		return nil, true, errors.New("invalid_parent: parent directory unavailable")
	}
	if !s.daily {
		rel, err := filepath.Rel(s.WorkspaceRoot, canonical)
		if err != nil || !filepath.IsLocal(rel) {
			return nil, true, errors.New("invalid_parent: parent outside isolated workspace")
		}
	}
	handle, err := os.OpenRoot(canonical)
	if err != nil {
		return nil, true, errors.New("parent_not_writable: parent directory unavailable")
	}
	defer handle.Close()
	target := filepath.Join(canonical, name)
	if filepath.Dir(target) != canonical {
		return nil, true, errors.New("invalid_target: target must belong to parent")
	}
	o, err := s.Store.RegisterProjectOperation(ctx, id, s.Identity.InstanceID, target)
	if err != nil {
		return nil, true, err
	}
	o, err = s.Store.TransitionProjectOperation(ctx, o, "running", nil)
	if err != nil {
		return projectOperationReply(o), true, nil
	}
	// Any outcome after the durable registration is reported through the
	// journal. A canceled RPC must not erase evidence or repeat mkdir.
	failure := ""
	rechecked, err := filepath.EvalSymlinks(parent)
	if err != nil {
		failure = "parent_missing"
	} else if rechecked != canonical {
		failure = "internal"
	} else if err := ctx.Err(); err != nil {
		failure = "interrupted"
	} else if err := handle.Mkdir(name, 0o700); err != nil {
		switch {
		case errors.Is(err, os.ErrExist):
			failure = "target_exists"
		case errors.Is(err, os.ErrNotExist):
			failure = "parent_missing"
		case errors.Is(err, os.ErrPermission):
			failure = "parent_not_writable"
		default:
			failure = "internal"
		}
	}
	next := "succeeded"
	var fixed *string
	if failure != "" {
		next = "failed"
		fixed = &failure
	}
	settleCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	settled, err := s.Store.TransitionProjectOperation(settleCtx, o, next, fixed)
	if err == nil {
		o = settled
	}
	return projectOperationReply(o), true, nil
}

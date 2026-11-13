package backendstore

import (
	"context"
	"database/sql"
	"errors"
	"time"

	"github.com/google/uuid"
)

type ProjectOperation struct {
	ID, Owner, Path, State string
	Revision               int64
	Failure                *string
}

func (s *Store) RegisterProjectOperation(ctx context.Context, id, owner, path string) (ProjectOperation, error) {
	o := ProjectOperation{ID: id, Owner: owner, Path: path, State: "preparing"}
	if uuid.Validate(id) != nil || uuid.Validate(owner) != nil || len(path) == 0 || len(path) > 32768 {
		return o, errors.New("invalid_target: invalid operation identity or target")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return o, errors.New("journal_unavailable: intent could not be saved")
	}
	defer tx.Rollback()
	var exists bool
	if err := tx.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM filesystem_operations WHERE id=?)`, id).Scan(&exists); err != nil {
		return o, errors.New("journal_unavailable: intent could not be saved")
	}
	if exists {
		return o, errors.New("duplicate_operation: use a new operation identity after reconciliation")
	}
	var count int
	if err := tx.QueryRowContext(ctx, `SELECT COUNT(*) FROM filesystem_operations`).Scan(&count); err != nil || count >= 10000 {
		return o, errors.New("journal_unavailable: operation journal unavailable")
	}
	now := time.Now().Unix()
	if _, err := tx.ExecContext(ctx, `INSERT INTO filesystem_operations(id,environment_id,owner_id,kind,target_path,state,revision,created_at,updated_at) VALUES(?,?,?,'mkdir',?,'preparing',0,?,?)`, id, uuid.Nil.String(), owner, path, now, now); err != nil {
		return o, errors.New("journal_unavailable: intent could not be saved")
	}
	if err := tx.Commit(); err != nil {
		return o, errors.New("journal_unavailable: intent could not be confirmed")
	}
	return o, nil
}

func (s *Store) GetProjectOperation(ctx context.Context, id string) (ProjectOperation, error) {
	var o ProjectOperation
	var failure sql.NullString
	err := s.read.QueryRowContext(ctx, `SELECT id,owner_id,target_path,state,revision,failure_kind FROM filesystem_operations WHERE id=? AND environment_id=?`, id, uuid.Nil.String()).Scan(&o.ID, &o.Owner, &o.Path, &o.State, &o.Revision, &failure)
	if errors.Is(err, sql.ErrNoRows) {
		return o, errors.New("operation_not_registered: no local operation is registered")
	}
	if err != nil {
		return o, errors.New("journal_unavailable: operation could not be read")
	}
	if failure.Valid {
		o.Failure = &failure.String
	}
	return o, nil
}

func (s *Store) TransitionProjectOperation(ctx context.Context, o ProjectOperation, next string, failure *string) (ProjectOperation, error) {
	allowed := o.State == "preparing" && next == "running" || o.State == "running" && (next == "succeeded" || next == "failed" || next == "unknown" || next == "cancelled")
	if !allowed {
		return o, errors.New("invalid operation transition")
	}
	if failure != nil {
		switch *failure {
		case "target_exists", "parent_missing", "parent_not_writable", "git_not_available", "auth_required", "network_unreachable", "remote_failed", "interrupted", "internal":
		default:
			return o, errors.New("invalid operation failure category")
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	result, err := s.db.ExecContext(ctx, `UPDATE filesystem_operations SET state=?,revision=revision+1,updated_at=?,failure_kind=? WHERE id=? AND environment_id=? AND owner_id=? AND state=? AND revision=?`, next, time.Now().Unix(), failure, o.ID, uuid.Nil.String(), o.Owner, o.State, o.Revision)
	if err != nil {
		return o, err
	}
	changed, err := result.RowsAffected()
	if err != nil {
		return o, err
	}
	if changed != 1 {
		return o, errors.New("operation state changed")
	}
	o.State = next
	o.Revision++
	o.Failure = failure
	return o, nil
}

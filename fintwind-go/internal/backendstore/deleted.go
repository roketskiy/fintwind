package backendstore

import (
	"context"
	"database/sql"
	"errors"
	"time"
)

func (s *Store) loadDeleted(ctx context.Context) error {
	rows, err := s.db.QueryContext(ctx, `SELECT kind,id FROM deleted_task_identities`)
	if err != nil {
		return err
	}
	defer rows.Close()
	for rows.Next() {
		var kind, id string
		if err := rows.Scan(&kind, &id); err != nil {
			return err
		}
		switch kind {
		case "session":
			s.removedSessions[id] = true
		case "project":
			s.removedProjects[id] = true
		}
	}
	return rows.Err()
}

func markDeleted(ctx context.Context, tx *sql.Tx, kind, id string) error {
	_, err := tx.ExecContext(ctx, `INSERT INTO deleted_task_identities(kind,id,deleted_at) VALUES(?,?,?) ON CONFLICT(kind,id) DO NOTHING`, kind, id, time.Now().Unix())
	return err
}

// RemoveCleaned deletes a verified native tree's local identities in one
// transaction. expected is captured before the provider delete; no stale
// local snapshot can silently be erased after a concurrent write.
func (s *Store) RemoveCleaned(ctx context.Context, expected map[string]string, emptyProjectIDs []string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	for id, want := range expected {
		got, err := editFingerprint(ctx, tx, id)
		if err != nil {
			return err
		}
		if got != want {
			return errors.New("task changed during retention cleanup")
		}
	}
	for id := range expected {
		if err := markDeleted(ctx, tx, "session", id); err != nil {
			return err
		}
		for _, table := range []string{"messages", "session_details", "submissions", "sessions"} {
			column := "session_id"
			if table == "sessions" {
				column = "id"
			}
			if _, err := tx.ExecContext(ctx, `DELETE FROM `+table+` WHERE `+column+`=?`, id); err != nil {
				return err
			}
		}
	}
	deletedProjects := []string{}
	for _, id := range emptyProjectIDs {
		var used bool
		if err := tx.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM sessions WHERE project_id=?)`, id).Scan(&used); err != nil {
			return err
		}
		if used {
			continue
		}
		if err := markDeleted(ctx, tx, "project", id); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, `DELETE FROM projects WHERE id=?`, id); err != nil {
			return err
		}
		deletedProjects = append(deletedProjects, id)
	}
	if err := tx.Commit(); err != nil {
		return err
	}
	for id := range expected {
		s.removedSessions[id] = true
		delete(s.saved, id)
	}
	for _, id := range deletedProjects {
		s.removedProjects[id] = true
	}
	return nil
}

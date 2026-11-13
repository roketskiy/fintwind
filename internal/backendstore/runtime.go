package backendstore

import (
	"context"
	"errors"
)

func (s *Store) Removed(sessionID string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.removedSessions[sessionID]
}

func (s *Store) SaveRuntime(ctx context.Context, session Object) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	id := session.String("id")
	if s.removedSessions[id] || s.removedProjects[session.String("project_id")] {
		return errors.New("runtime session removed")
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if err := writeSession(ctx, tx, session, false); err != nil {
		return err
	}
	if err := tx.Commit(); err != nil {
		return err
	}
	delete(s.saved, id)
	return nil
}

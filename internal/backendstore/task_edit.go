package backendstore

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
)

const removedTurnsKey = "_fintwind_removed_turn_ids"

// Preserve a daemon-owned fence in the existing detail record. It never
// depends on clients round-tripping unknown JSON fields.
func fenceHistory(ctx context.Context, tx *sql.Tx, session Object) error {
	var raw string
	err := tx.QueryRowContext(ctx, `SELECT data FROM session_details WHERE session_id=?`, session.String("id")).Scan(&raw)
	if errors.Is(err, sql.ErrNoRows) {
		delete(session, removedTurnsKey)
		return nil
	}
	if err != nil {
		return err
	}
	old, err := Decode([]byte(raw))
	if err != nil {
		return err
	}
	var removed []string
	if len(old[removedTurnsKey]) > 0 {
		if err := json.Unmarshal(old[removedTurnsKey], &removed); err != nil {
			return err
		}
	}
	if len(removed) == 0 {
		delete(session, removedTurnsKey)
		return nil
	}
	blocked := map[string]bool{}
	for _, id := range removed {
		blocked[id] = true
	}
	for _, field := range []string{"turns", "messages", "transcript_blocks"} {
		var entries []Object
		if len(session[field]) > 0 {
			if err := json.Unmarshal(session[field], &entries); err != nil {
				return err
			}
		}
		for _, entry := range entries {
			id := entry.String("turn_id")
			if field == "turns" {
				id = entry.String("id")
			}
			if blocked[id] {
				return errors.New("task history changed; reload the task before saving")
			}
		}
	}
	session[removedTurnsKey] = old[removedTurnsKey]
	return nil
}

func editFingerprint(ctx context.Context, tx *sql.Tx, id string) (string, error) {
	h := sha256.New()
	for _, query := range []string{`SELECT * FROM sessions WHERE id=?`, `SELECT data FROM session_details WHERE session_id=?`, `SELECT * FROM messages WHERE session_id=? ORDER BY position,id`, `SELECT * FROM submissions WHERE session_id=? ORDER BY id`} {
		rows, err := tx.QueryContext(ctx, query, id)
		if err != nil {
			return "", err
		}
		cols, err := rows.Columns()
		if err != nil {
			rows.Close()
			return "", err
		}
		for rows.Next() {
			values := make([]any, len(cols))
			refs := make([]any, len(cols))
			for i := range values {
				refs[i] = &values[i]
			}
			if err := rows.Scan(refs...); err != nil {
				rows.Close()
				return "", err
			}
			raw, err := json.Marshal(values)
			if err != nil {
				rows.Close()
				return "", err
			}
			h.Write(raw)
			h.Write([]byte{0})
		}
		err = rows.Err()
		rows.Close()
		if err != nil {
			return "", err
		}
		h.Write([]byte{1})
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

func (s *Store) TaskEditSnapshot(ctx context.Context, id string) (Object, string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	payload, err := s.Hydrate(ctx, id)
	if err != nil {
		return nil, "", err
	}
	source, err := Decode(payload["session"])
	if err != nil {
		return nil, "", errors.New("task unavailable")
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, "", err
	}
	defer tx.Rollback()
	var active bool
	if err := tx.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM submissions WHERE session_id=? AND state IN ('preparing','dispatching','unknown'))`, id).Scan(&active); err != nil {
		return nil, "", err
	}
	if active {
		return nil, "", errors.New("task has an unsettled submission")
	}
	fingerprint, err := editFingerprint(ctx, tx, id)
	return source, fingerprint, err
}

// CommitTaskEdit is not Save: it atomically replaces the authoritative
// transcript and its submission rows after a full durable-state CAS.
func (s *Store) CommitTaskEdit(ctx context.Context, sourceID, expected string, target Object, fork bool) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.removedSessions[sourceID] || s.removedProjects[target.String("project_id")] {
		return errors.New("task removed during edit")
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	fingerprint, err := editFingerprint(ctx, tx, sourceID)
	if err != nil {
		return err
	}
	if fingerprint != expected {
		return errors.New("task changed during history edit")
	}
	id := target.String("id")
	if fork {
		if id == sourceID || s.removedSessions[id] {
			return errors.New("invalid fork identity")
		}
		var exists bool
		if err := tx.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)`, id).Scan(&exists); err != nil {
			return err
		}
		if exists {
			return errors.New("fork identity already exists")
		}
		delete(target, removedTurnsKey)
	} else {
		if id != sourceID {
			return errors.New("rewind cannot change task identity")
		}
		var raw string
		if err := tx.QueryRowContext(ctx, `SELECT data FROM session_details WHERE session_id=?`, id).Scan(&raw); err != nil {
			return err
		}
		old, err := Decode([]byte(raw))
		if err != nil {
			return err
		}
		var oldTurns, newTurns []Object
		var removed []string
		if err := json.Unmarshal(old["turns"], &oldTurns); err != nil {
			return err
		}
		if err := json.Unmarshal(target["turns"], &newTurns); err != nil {
			return err
		}
		if len(old[removedTurnsKey]) > 0 {
			if err := json.Unmarshal(old[removedTurnsKey], &removed); err != nil {
				return err
			}
		}
		keep := map[string]bool{}
		ids := []string{}
		for _, turn := range newTurns {
			keep[turn.String("id")] = true
			ids = append(ids, turn.String("id"))
		}
		for _, turn := range oldTurns {
			if !keep[turn.String("id")] {
				removed = append(removed, turn.String("id"))
			}
		}
		old.Set(removedTurnsKey, removed)
		encoded, _ := json.Marshal(old)
		if _, err := tx.ExecContext(ctx, `UPDATE session_details SET data=? WHERE session_id=?`, string(encoded), id); err != nil {
			return err
		}
		encodedIDs, _ := json.Marshal(ids)
		if _, err := tx.ExecContext(ctx, `DELETE FROM submissions WHERE session_id=? AND id NOT IN (SELECT value FROM json_each(?))`, id, string(encodedIDs)); err != nil {
			return err
		}
	}
	if err := writeSession(ctx, tx, target, false); err != nil {
		return err
	}
	if err := tx.Commit(); err != nil {
		return err
	}
	delete(s.saved, sourceID)
	delete(s.saved, id)
	return nil
}

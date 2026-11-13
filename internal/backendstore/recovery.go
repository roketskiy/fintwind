package backendstore

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"fintwind-go/internal/model"
)

func (s *Store) RecoverInterruptedSessions(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	rows, err := tx.QueryContext(ctx, `SELECT s.id,s.updated_at,d.data FROM sessions s LEFT JOIN session_details d ON d.session_id=s.id WHERE s.status IN ('connecting','working','waiting')`)
	if err != nil {
		return err
	}
	type savedSession struct {
		id      string
		updated int64
		detail  sql.NullString
	}
	var sessions []savedSession
	for rows.Next() {
		var session savedSession
		if err := rows.Scan(&session.id, &session.updated, &session.detail); err != nil {
			rows.Close()
			return err
		}
		sessions = append(sessions, session)
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return err
	}
	for _, session := range sessions {
		if session.detail.Valid {
			detail, err := Decode([]byte(session.detail.String))
			if err != nil {
				return fmt.Errorf("recover session %s: %w", session.id, err)
			}
			var turns []Object
			if raw := detail["turns"]; len(raw) > 0 {
				if err := json.Unmarshal(raw, &turns); err != nil {
					return fmt.Errorf("recover session %s turns: %w", session.id, err)
				}
			}
			for _, turn := range turns {
				if turn.String("status") != "running" {
					continue
				}
				turn.Set("status", "interrupted")
				started, err := turn.Int("started_at")
				if err != nil {
					return err
				}
				turn.Set("completed_at", max(started, session.updated))
				var raw string
				err = tx.QueryRowContext(ctx, `SELECT data FROM submissions WHERE id=? AND session_id=?`, turn.String("id"), session.id).Scan(&raw)
				if errors.Is(err, sql.ErrNoRows) {
					continue
				}
				if err != nil {
					return err
				}
				record, err := Decode([]byte(raw))
				if err != nil {
					return err
				}
				receipt, err := Decode(record["receipt"])
				if err != nil {
					return err
				}
				state := receipt.String("state")
				switch state {
				case model.SubmissionPreparing:
					state = model.SubmissionNotSent
				case model.SubmissionDispatching:
					state = model.SubmissionUnknown
				}
				if state != receipt.String("state") {
					receipt.Set("state", state)
					receipt.Set("updated_at", time.Now().UnixMilli())
					record.Set("receipt", receipt)
					encoded, err := json.Marshal(record)
					if err != nil {
						return err
					}
					if _, err := tx.ExecContext(ctx, `UPDATE submissions SET state=?,data=? WHERE id=? AND session_id=?`, state, string(encoded), turn.String("id"), session.id); err != nil {
						return err
					}
				}
				turn.Set("submission", receipt)
			}
			if turns != nil {
				detail.Set("turns", turns)
			}
			if compaction, err := Decode(detail["compaction"]); err == nil && compaction.String("status") == "running" {
				compaction.Set("status", "cancelled")
				detail.Set("compaction", compaction)
			}
			detail.Set("status", "idle")
			encoded, err := json.Marshal(detail)
			if err != nil {
				return err
			}
			if _, err := tx.ExecContext(ctx, `UPDATE session_details SET data=? WHERE session_id=?`, string(encoded), session.id); err != nil {
				return err
			}
		}
		if _, err := tx.ExecContext(ctx, `UPDATE sessions SET status='idle' WHERE id=?`, session.id); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, `UPDATE messages SET streaming=0 WHERE session_id=? AND streaming<>0`, session.id); err != nil {
			return err
		}
	}
	return tx.Commit()
}

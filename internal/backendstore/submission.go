package backendstore

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"strings"
	"time"

	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

func (s *Store) Submission(ctx context.Context, id string) (model.SubmissionRecord, error) {
	var raw string
	var record model.SubmissionRecord
	if err := s.read.QueryRowContext(ctx, `SELECT data FROM submissions WHERE id=?`, id).Scan(&raw); err != nil {
		return record, err
	}
	return record, json.Unmarshal([]byte(raw), &record)
}

func (s *Store) Admit(ctx context.Context, sessionID string, command Object) (model.SubmissionReceipt, error) {
	var empty model.SubmissionReceipt
	project, err := Decode(command["project"])
	if err != nil {
		return empty, err
	}
	session, err := Decode(command["session"])
	if err != nil {
		return empty, err
	}
	projectID, err := project.ID("id")
	if err != nil {
		return empty, err
	}
	id, err := session.ID("id")
	if err != nil || id != sessionID || session.String("project_id") != projectID {
		return empty, errors.New("invalid submission session")
	}
	var record model.SubmissionRecord
	if err := json.Unmarshal(command["submission"], &record); err != nil {
		return empty, err
	}
	turnID, err := uuid.Parse(record.Receipt.ID)
	if err != nil || turnID == uuid.Nil || record.SessionID != id || record.Receipt.State != model.SubmissionPreparing || !strings.HasPrefix(record.Receipt.InputID, "msg_") || len(record.Receipt.InputID) > 128 || record.Receipt.CreatedAt == 0 || !session.Bool("detail_loaded") {
		return empty, errors.New("invalid submission identity")
	}
	var turns []Object
	if err := json.Unmarshal(session["turns"], &turns); err != nil || len(turns) == 0 {
		return empty, errors.New("running submission turn required")
	}
	last := turns[len(turns)-1]
	if last.String("id") != record.Receipt.ID || last.String("status") != "running" {
		return empty, errors.New("submission turn mismatch")
	}
	for _, key := range []string{"created_at", "updated_at", "last_reply_at"} {
		if _, err := session.Int(key); err != nil {
			return empty, err
		}
	}
	created, err := project.Int("created_at")
	if err != nil {
		return empty, err
	}
	var messages []Object
	if err := json.Unmarshal(session["messages"], &messages); err != nil {
		return empty, errors.New("invalid submission messages")
	}
	seen := map[string]bool{}
	for _, message := range messages {
		mid, err := message.ID("id")
		if err != nil {
			return empty, err
		}
		if seen[mid] {
			return empty, errors.New("duplicate submission message")
		}
		seen[mid] = true
		if _, err := message.Int("created_at"); err != nil {
			return empty, err
		}
		if raw := message["attachments"]; len(raw) > 0 {
			var attachments []json.RawMessage
			if err := json.Unmarshal(raw, &attachments); err != nil {
				return empty, err
			}
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.removedSessions[id] || s.removedProjects[projectID] {
		return empty, errors.New("submission target removed")
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return empty, err
	}
	defer tx.Rollback()
	var previous string
	err = tx.QueryRowContext(ctx, `SELECT data FROM submissions WHERE id=?`, record.Receipt.ID).Scan(&previous)
	if err == nil {
		var saved model.SubmissionRecord
		if err := json.Unmarshal([]byte(previous), &saved); err != nil {
			return empty, err
		}
		oldFiles, _ := json.Marshal(saved.Files)
		newFiles, _ := json.Marshal(record.Files)
		if saved.SessionID != id || saved.Receipt.InputID != record.Receipt.InputID || saved.Prompt != record.Prompt || !bytes.Equal(oldFiles, newFiles) {
			return empty, errors.New("submission identity conflicts with saved input")
		}
		return saved.Receipt, nil
	}
	if !errors.Is(err, sql.ErrNoRows) {
		return empty, err
	}
	var active bool
	if err := tx.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM submissions WHERE session_id=? AND state IN ('preparing','dispatching','unknown'))`, id).Scan(&active); err != nil {
		return empty, err
	}
	if active {
		return empty, errors.New("session already has an unsettled submission")
	}
	if _, err := tx.ExecContext(ctx, `INSERT INTO projects(id,name,path,position,created_at) VALUES(?,?,?,COALESCE((SELECT MAX(position)+1 FROM projects),0),?) ON CONFLICT(id) DO NOTHING`, projectID, project.String("name"), project.String("path"), created); err != nil {
		return empty, err
	}
	if err := writeSession(ctx, tx, session, false); err != nil {
		return empty, err
	}
	raw, err := json.Marshal(record)
	if err != nil {
		return empty, err
	}
	if _, err := tx.ExecContext(ctx, `INSERT INTO submissions(id,session_id,state,data) VALUES(?,?,?,?)`, record.Receipt.ID, id, record.Receipt.State, string(raw)); err != nil {
		return empty, err
	}
	if err := tx.Commit(); err != nil {
		return empty, err
	}
	delete(s.saved, id)
	return record.Receipt, nil
}

func (s *Store) Transition(ctx context.Context, sessionID, id, from, to string, native *string) (model.SubmissionRecord, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return model.SubmissionRecord{}, false, err
	}
	defer tx.Rollback()
	var raw string
	if err := tx.QueryRowContext(ctx, `SELECT data FROM submissions WHERE id=? AND session_id=?`, id, sessionID).Scan(&raw); err != nil {
		return model.SubmissionRecord{}, false, err
	}
	var record model.SubmissionRecord
	if err := json.Unmarshal([]byte(raw), &record); err != nil {
		return record, false, err
	}
	if record.Receipt.State != from {
		return record, false, nil
	}
	record.Receipt.State = to
	record.Receipt.UpdatedAt = uint64(time.Now().UnixMilli())
	if native != nil {
		record.Receipt.NativeSessionID = native
	}
	encoded, err := json.Marshal(record)
	if err != nil {
		return record, false, err
	}
	if _, err := tx.ExecContext(ctx, `UPDATE submissions SET state=?,data=? WHERE id=? AND state=?`, to, string(encoded), id, from); err != nil {
		return record, false, err
	}
	if err := tx.Commit(); err != nil {
		return record, false, err
	}
	return record, true, nil
}

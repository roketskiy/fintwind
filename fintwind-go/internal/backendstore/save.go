package backendstore

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
)

const upsertSession = `INSERT INTO sessions(` + sessionColumns + `) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET project_id=excluded.project_id,title=excluded.title,auto_title=excluded.auto_title,provider=excluded.provider,model=excluded.model,status=excluded.status,imported=excluded.imported,native_session_id=excluded.native_session_id,created_at=excluded.created_at,updated_at=excluded.updated_at,last_reply_at=excluded.last_reply_at,archived=excluded.archived,mermaid_prompt_version=COALESCE(sessions.mermaid_prompt_version,excluded.mermaid_prompt_version)`

const upsertMessage = `INSERT INTO messages(id,session_id,turn_id,position,role,content,display_content,attachments,created_at,streaming) VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET turn_id=excluded.turn_id,position=excluded.position,role=excluded.role,content=excluded.content,display_content=excluded.display_content,attachments=excluded.attachments,created_at=excluded.created_at,streaming=excluded.streaming WHERE messages.session_id=excluded.session_id AND (messages.turn_id IS NOT excluded.turn_id OR messages.position IS NOT excluded.position OR messages.role IS NOT excluded.role OR messages.content IS NOT excluded.content OR messages.display_content IS NOT excluded.display_content OR messages.attachments IS NOT excluded.attachments OR messages.created_at IS NOT excluded.created_at OR messages.streaming IS NOT excluded.streaming)`

func (s *Store) Save(ctx context.Context, command Object) (Object, error) {
	var projects, sessions []Object
	if err := json.Unmarshal(command["projects"], &projects); err != nil {
		return nil, errors.New("invalid projects")
	}
	if err := json.Unmarshal(command["sessions"], &sessions); err != nil {
		return nil, errors.New("invalid sessions")
	}
	if len(projects) > 10000 || len(sessions) > 10000 {
		return nil, errors.New("snapshot item limit exceeded")
	}
	var skeletonIDs []string
	if raw := command["skeletonSessionIds"]; len(raw) > 0 {
		if err := json.Unmarshal(raw, &skeletonIDs); err != nil {
			return nil, errors.New("invalid skeleton IDs")
		}
	}
	skeletons := map[string]bool{}
	for _, id := range skeletonIDs {
		skeletons[id] = true
	}
	seenProjects, seenSessions, seenMessages := map[string]bool{}, map[string]bool{}, map[string]bool{}
	for _, project := range projects {
		id, err := project.ID("id")
		if err != nil {
			return nil, err
		}
		if seenProjects[id] {
			return nil, errors.New("duplicate project")
		}
		seenProjects[id] = true
		if _, err := project.Int("created_at"); err != nil {
			return nil, err
		}
	}
	for _, session := range sessions {
		id, err := session.ID("id")
		if err != nil {
			return nil, err
		}
		if seenSessions[id] {
			return nil, errors.New("duplicate session")
		}
		seenSessions[id] = true
		if _, err := session.ID("project_id"); err != nil {
			return nil, err
		}
		switch session.String("status") {
		case "idle", "connecting", "working", "waiting", "failed":
		default:
			return nil, errors.New("invalid session status")
		}
		for _, key := range []string{"created_at", "updated_at", "last_reply_at"} {
			if _, err := session.Int(key); err != nil {
				return nil, err
			}
		}
		if skeletons[id] {
			continue
		}
		var messages []Object
		if err := json.Unmarshal(session["messages"], &messages); err != nil {
			return nil, errors.New("invalid messages")
		}
		for _, message := range messages {
			mid, err := message.ID("id")
			if err != nil {
				return nil, err
			}
			if seenMessages[mid] {
				return nil, errors.New("duplicate message")
			}
			seenMessages[mid] = true
			if _, err := message.Int("created_at"); err != nil {
				return nil, err
			}
			var attachments []json.RawMessage
			if raw := message["attachments"]; len(raw) > 0 {
				if err := json.Unmarshal(raw, &attachments); err != nil {
					return nil, errors.New("invalid attachments")
				}
			}
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, err
	}
	defer tx.Rollback()
	for _, project := range projects {
		id := project.String("id")
		if s.removedProjects[id] {
			continue
		}
		created, _ := project.Int("created_at")
		if _, err := tx.ExecContext(ctx, `INSERT INTO projects(id,name,path,position,created_at) VALUES(?,?,?,COALESCE((SELECT MAX(position)+1 FROM projects),0),?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,path=excluded.path,created_at=excluded.created_at`, id, project.String("name"), project.String("path"), created); err != nil {
			return nil, err
		}
	}
	projections := []Object{}
	pending := map[string][32]byte{}
	for _, session := range sessions {
		id := session.String("id")
		if s.removedSessions[id] || s.removedProjects[session.String("project_id")] {
			continue
		}
		if skeletons[id] {
			updated, _ := session.Int("updated_at")
			if _, err := tx.ExecContext(ctx, `UPDATE sessions SET title=?,auto_title=?,model=?,updated_at=? WHERE id=? AND updated_at<=?`, session.String("title"), session.Nullable("auto_title"), session.Nullable("model"), updated, id, updated); err != nil {
				return nil, err
			}
			projection, err := scanSession(tx.QueryRowContext(ctx, `SELECT `+sessionColumns+` FROM sessions WHERE id=?`, id))
			if err != nil {
				return nil, err
			}
			projections = append(projections, projection)
			continue
		}
		merged, skip, err := mergeSnapshot(ctx, tx, session)
		if err != nil {
			return nil, err
		}
		if skip {
			delete(s.saved, id)
			projection, err := scanSession(tx.QueryRowContext(ctx, `SELECT `+sessionColumns+` FROM sessions WHERE id=?`, id))
			if err != nil {
				return nil, err
			}
			projections = append(projections, projection)
			continue
		}
		session = merged
		encoded, err := json.Marshal(session)
		if err != nil {
			return nil, err
		}
		hash := digest(encoded)
		if previous, ok := s.saved[id]; !ok || previous != hash || skeletons[id] {
			if err := writeSession(ctx, tx, session, skeletons[id]); err != nil {
				return nil, err
			}
			if !skeletons[id] {
				pending[id] = hash
			}
		}
		projection, err := scanSession(tx.QueryRowContext(ctx, `SELECT `+sessionColumns+` FROM sessions WHERE id=?`, id))
		if err != nil {
			return nil, err
		}
		projections = append(projections, projection)
	}
	if err := tx.Commit(); err != nil {
		return nil, err
	}
	if len(s.saved)+len(pending) > 256 {
		s.saved = map[string][32]byte{}
	}
	for id, hash := range pending {
		s.saved[id] = hash
	}
	return Reply("taskStateSaved", Fields(map[string]any{"sessions": projections})), nil
}

func writeSession(ctx context.Context, tx *sql.Tx, session Object, skeleton bool) error {
	if err := fenceHistory(ctx, tx, session); err != nil {
		return err
	}
	id := session.String("id")
	created, _ := session.Int("created_at")
	updated, _ := session.Int("updated_at")
	var last any
	if raw := session["last_reply_at"]; len(raw) > 0 && string(raw) != "null" {
		last, _ = session.Int("last_reply_at")
	}
	if _, err := tx.ExecContext(ctx, upsertSession, id, session.String("project_id"), session.String("title"), session.Nullable("auto_title"), session.String("provider"), session.Nullable("model"), session.String("status"), session.Bool("imported"), session.Nullable("native_session_id"), created, updated, last, session.Bool("archived"), session.Nullable("mermaid_prompt_version")); err != nil {
		return err
	}
	if skeleton {
		return nil
	}
	detail := Object{}
	for key, value := range session {
		if key != "messages" {
			detail[key] = value
		}
	}
	var frozen sql.NullString
	if err := tx.QueryRowContext(ctx, `SELECT mermaid_prompt_version FROM sessions WHERE id=?`, id).Scan(&frozen); err != nil {
		return err
	}
	if frozen.Valid {
		detail.Set("mermaid_prompt_version", frozen.String)
	}
	encoded, err := json.Marshal(detail)
	if err != nil {
		return err
	}
	if _, err := tx.ExecContext(ctx, `INSERT INTO session_details(session_id,data) VALUES(?,?) ON CONFLICT(session_id) DO UPDATE SET data=excluded.data WHERE session_details.data IS NOT excluded.data`, id, string(encoded)); err != nil {
		return err
	}
	var messages []Object
	if err := json.Unmarshal(session["messages"], &messages); err != nil {
		return err
	}
	ids := make([]string, 0, len(messages))
	statement, err := tx.PrepareContext(ctx, upsertMessage)
	if err != nil {
		return err
	}
	defer statement.Close()
	for position, message := range messages {
		mid := message.String("id")
		ids = append(ids, mid)
		var owner string
		err := tx.QueryRowContext(ctx, `SELECT session_id FROM messages WHERE id=?`, mid).Scan(&owner)
		if err != nil && !errors.Is(err, sql.ErrNoRows) {
			return err
		}
		if err == nil && owner != id {
			return errors.New("message belongs to another session")
		}
		created, _ := message.Int("created_at")
		attachments := message["attachments"]
		if len(attachments) == 0 || string(attachments) == "null" {
			attachments = json.RawMessage(`[]`)
		}
		if _, err := statement.ExecContext(ctx, mid, id, message.Nullable("turn_id"), position, message.String("role"), message.String("content"), message.Nullable("display_content"), string(attachments), created, message.Bool("streaming")); err != nil {
			return fmt.Errorf("write message: %w", err)
		}
	}
	encodedIDs, err := json.Marshal(ids)
	if err != nil {
		return err
	}
	_, err = tx.ExecContext(ctx, `DELETE FROM messages WHERE session_id=? AND id NOT IN (SELECT value FROM json_each(?))`, id, string(encodedIDs))
	return err
}

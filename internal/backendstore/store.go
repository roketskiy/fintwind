package backendstore

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"path/filepath"
	"sync"
	"time"

	_ "modernc.org/sqlite"
)

type Store struct {
	db              *sql.DB
	read            *sql.DB
	mu              sync.Mutex
	removedSessions map[string]bool
	removedProjects map[string]bool
	saved           map[string][32]byte
}

func Open(ctx context.Context, directory string) (*Store, error) {
	path := filepath.Join(directory, "app.db")
	u := "file:" + filepath.ToSlash(path)
	query := url.Values{}
	query.Add("_pragma", "busy_timeout(5000)")
	query.Add("_pragma", "foreign_keys(1)")
	query.Add("_pragma", "journal_mode(WAL)")
	query.Add("_pragma", "synchronous(FULL)")
	query.Set("_txlock", "immediate")
	db, err := sql.Open("sqlite", u+"?"+query.Encode())
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	if err := migrate(ctx, db); err != nil {
		db.Close()
		return nil, err
	}
	query = url.Values{}
	query.Set("mode", "ro")
	query.Add("_pragma", "busy_timeout(5000)")
	read, err := sql.Open("sqlite", u+"?"+query.Encode())
	if err != nil {
		db.Close()
		return nil, err
	}
	read.SetMaxOpenConns(4)
	read.SetMaxIdleConns(4)
	if err := read.PingContext(ctx); err != nil {
		read.Close()
		db.Close()
		return nil, err
	}
	store := &Store{db: db, read: read, removedSessions: map[string]bool{}, removedProjects: map[string]bool{}, saved: map[string][32]byte{}}
	if err := store.loadDeleted(ctx); err != nil {
		read.Close()
		db.Close()
		return nil, err
	}
	return store, nil
}

func (s *Store) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return errors.Join(s.read.Close(), s.db.Close())
}

func migrate(ctx context.Context, db *sql.DB) error {
	if _, err := db.ExecContext(ctx, `CREATE TABLE IF NOT EXISTS migrations(tag TEXT PRIMARY KEY, applied_at INTEGER NOT NULL)`); err != nil {
		return err
	}
	for _, migration := range migrations {
		var exists bool
		if err := db.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM migrations WHERE tag=?)`, migration.tag).Scan(&exists); err != nil {
			return err
		}
		if exists {
			continue
		}
		tx, err := db.BeginTx(ctx, nil)
		if err != nil {
			return err
		}
		err = func() error {
			if migration.tag == "0009_big_starbolt" {
				var retired bool
				if err := tx.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM migrations WHERE tag='0009_true_lockheed') AND EXISTS(SELECT 1 FROM pragma_table_info('scheduled_tasks') WHERE name='schedule_json') AND NOT EXISTS(SELECT 1 FROM pragma_table_info('scheduled_tasks') WHERE name='revision')`).Scan(&retired); err != nil {
					return err
				}
				if retired {
					if _, err := tx.ExecContext(ctx, `ALTER TABLE scheduled_tasks RENAME TO retired_scheduled_tasks`); err != nil {
						return err
					}
				}
			}
			if _, err := tx.ExecContext(ctx, migration.sql); err != nil {
				return fmt.Errorf("migration %s failed: %w", migration.tag, err)
			}
			_, err := tx.ExecContext(ctx, `INSERT INTO migrations(tag,applied_at) VALUES(?,?)`, migration.tag, time.Now().Unix())
			return err
		}()
		if err != nil {
			_ = tx.Rollback()
			return err
		}
		if err := tx.Commit(); err != nil {
			return err
		}
	}
	var renamed bool
	if err := db.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM pragma_table_info('projects') WHERE name='project_path') AND NOT EXISTS(SELECT 1 FROM pragma_table_info('projects') WHERE name='path')`).Scan(&renamed); err != nil {
		return err
	}
	if renamed {
		_, err := db.ExecContext(ctx, `ALTER TABLE projects RENAME COLUMN project_path TO path`)
		return err
	}
	return nil
}

const sessionColumns = `id,project_id,title,auto_title,provider,model,status,imported,native_session_id,created_at,updated_at,last_reply_at,archived,mermaid_prompt_version`

type scanner interface{ Scan(...any) error }

func scanSession(row scanner) (Object, error) {
	var id, project, title, provider, status string
	var autoTitle, model, native, mermaid sql.NullString
	var created, updated int64
	var reply sql.NullInt64
	var imported, archived bool
	err := row.Scan(&id, &project, &title, &autoTitle, &provider, &model, &status, &imported, &native, &created, &updated, &reply, &archived, &mermaid)
	if err != nil {
		return nil, err
	}
	nullString := func(s sql.NullString) any {
		if s.Valid {
			return s.String
		}
		return nil
	}
	var last any
	if reply.Valid {
		last = reply.Int64
	}
	return Fields(map[string]any{
		"id": id, "project_id": project, "title": title, "auto_title": nullString(autoTitle), "provider": provider, "model": nullString(model), "status": status,
		"imported": imported, "native_session_id": nullString(native), "created_at": created, "updated_at": updated, "last_reply_at": last, "archived": archived, "mermaid_prompt_version": nullString(mermaid),
		"messages": []any{}, "turns": []any{}, "transcript_blocks": []any{}, "queued_messages": []any{}, "runtime_mode": "ask", "interaction_mode": "build",
	}), nil
}

func (s *Store) Load(ctx context.Context, cwd string) (Object, error) {
	projects := []Object{}
	rows, err := s.read.QueryContext(ctx, `SELECT id,name,path,created_at FROM projects ORDER BY position`)
	if err != nil {
		return nil, err
	}
	for rows.Next() {
		var id, name, path string
		var created int64
		if err := rows.Scan(&id, &name, &path, &created); err != nil {
			rows.Close()
			return nil, err
		}
		projects = append(projects, Fields(map[string]any{"id": id, "name": name, "path": path, "created_at": created}))
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return nil, err
	}
	sessions := []Object{}
	rows, err = s.read.QueryContext(ctx, `SELECT `+sessionColumns+` FROM sessions ORDER BY updated_at`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	for rows.Next() {
		session, err := scanSession(rows)
		if err != nil {
			return nil, err
		}
		sessions = append(sessions, session)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return Reply("taskState", Fields(map[string]any{"projects": projects, "sessions": sessions, "defaultCwd": cwd, "projectlessRoot": nil, "projectlessLegacyRoot": nil})), nil
}

func (s *Store) Hydrate(ctx context.Context, id string) (Object, error) {
	tx, err := s.read.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return nil, err
	}
	defer tx.Rollback()
	list, err := scanSession(tx.QueryRowContext(ctx, `SELECT `+sessionColumns+` FROM sessions WHERE id=?`, id))
	if errors.Is(err, sql.ErrNoRows) {
		return Reply("session", Fields(map[string]any{"session": nil})), nil
	}
	if err != nil {
		return nil, err
	}
	var raw string
	err = tx.QueryRowContext(ctx, `SELECT data FROM session_details WHERE session_id=?`, id).Scan(&raw)
	if err != nil && !errors.Is(err, sql.ErrNoRows) {
		return nil, err
	}
	session := Object{}
	if raw != "" {
		session, err = Decode([]byte(raw))
		if err != nil {
			return nil, err
		}
	}
	for key, value := range list {
		if key != "turns" && key != "transcript_blocks" && key != "queued_messages" && key != "runtime_mode" && key != "interaction_mode" {
			session[key] = value
		}
	}
	for _, key := range []string{"turns", "transcript_blocks", "queued_messages"} {
		if session[key] == nil {
			session.Set(key, []any{})
		}
	}
	rows, err := tx.QueryContext(ctx, `SELECT id,turn_id,role,content,display_content,attachments,created_at,streaming FROM messages WHERE session_id=? ORDER BY position`, id)
	if err != nil {
		return nil, err
	}
	messages := []Object{}
	for rows.Next() {
		var id, role, content, attachments string
		var turn, display sql.NullString
		var created int64
		var streaming bool
		if err := rows.Scan(&id, &turn, &role, &content, &display, &attachments, &created, &streaming); err != nil {
			rows.Close()
			return nil, err
		}
		var turnID, displayContent any
		if turn.Valid {
			turnID = turn.String
		}
		if display.Valid {
			displayContent = display.String
		}
		if !json.Valid([]byte(attachments)) {
			rows.Close()
			return nil, errors.New("invalid stored attachment data")
		}
		message := Fields(map[string]any{"id": id, "turn_id": turnID, "role": role, "content": content, "display_content": displayContent, "created_at": created, "streaming": streaming})
		message["attachments"] = json.RawMessage(attachments)
		messages = append(messages, message)
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return nil, err
	}
	session.Set("messages", messages)
	if err := tx.Commit(); err != nil {
		return nil, err
	}
	return Reply("session", Fields(map[string]any{"session": session})), nil
}

func (s *Store) Remove(ctx context.Context, session, project string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if project != "" {
		rows, err := tx.QueryContext(ctx, `SELECT id FROM sessions WHERE project_id=?`, project)
		if err != nil {
			return err
		}
		ids := []string{}
		for rows.Next() {
			var id string
			if err := rows.Scan(&id); err != nil {
				rows.Close()
				return err
			}
			ids = append(ids, id)
		}
		err = rows.Err()
		rows.Close()
		if err != nil {
			return err
		}
		for _, table := range []string{"messages", "session_details", "submissions"} {
			if _, err := tx.ExecContext(ctx, `DELETE FROM `+table+` WHERE session_id IN(SELECT id FROM sessions WHERE project_id=?)`, project); err != nil {
				return err
			}
		}
		for _, id := range ids {
			if err := markDeleted(ctx, tx, "session", id); err != nil {
				return err
			}
		}
		if err := markDeleted(ctx, tx, "project", project); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, `DELETE FROM sessions WHERE project_id=?`, project); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, `DELETE FROM projects WHERE id=?`, project); err != nil {
			return err
		}
		if err := tx.Commit(); err != nil {
			return err
		}
		s.removedProjects[project] = true
		for _, id := range ids {
			s.removedSessions[id] = true
			delete(s.saved, id)
		}
		return nil
	} else {
		if err := markDeleted(ctx, tx, "session", session); err != nil {
			return err
		}
		for _, table := range []string{"messages", "session_details", "submissions"} {
			if _, err := tx.ExecContext(ctx, `DELETE FROM `+table+` WHERE session_id=?`, session); err != nil {
				return err
			}
		}
		if _, err := tx.ExecContext(ctx, `DELETE FROM sessions WHERE id=?`, session); err != nil {
			return err
		}
	}
	if err := tx.Commit(); err != nil {
		return err
	}
	if project != "" {
		s.removedProjects[project] = true
	} else {
		s.removedSessions[session] = true
		delete(s.saved, session)
	}
	return nil
}

func (s *Store) SessionsForProject(ctx context.Context, project string) ([]string, error) {
	rows, err := s.read.QueryContext(ctx, `SELECT id FROM sessions WHERE project_id=?`, project)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	ids := []string{}
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			return nil, err
		}
		ids = append(ids, id)
	}
	return ids, rows.Err()
}

func (s *Store) Search(ctx context.Context, query string, limit int) (Object, error) {
	if limit < 1 || limit > 200 {
		limit = 50
	}
	matches := []Object{}
	if query == "" {
		return Reply("sessionMessageMatches", Fields(map[string]any{"matches": matches})), nil
	}
	rows, err := s.read.QueryContext(ctx, `WITH ranked AS (SELECT m.session_id,m.role,m.content,s.updated_at,CASE WHEN m.role='user' THEN 0 ELSE 1 END AS source_rank,ROW_NUMBER() OVER(PARTITION BY m.session_id ORDER BY CASE WHEN m.role='user' THEN 0 ELSE 1 END,m.position) AS rn FROM messages m JOIN sessions s ON s.id=m.session_id WHERE m.streaming=0 AND m.role IN ('user','assistant') AND instr(lower(m.content),lower(?))>0) SELECT session_id,role,content FROM ranked WHERE rn=1 ORDER BY source_rank,updated_at DESC LIMIT ?`, query, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	for rows.Next() {
		var id, role, content string
		if err := rows.Scan(&id, &role, &content); err != nil {
			return nil, err
		}
		runes := []rune(content)
		if len(runes) > 240 {
			content = string(runes[:240])
		}
		matches = append(matches, Fields(map[string]any{"session_id": id, "source": role, "snippet": content}))
	}
	return Reply("sessionMessageMatches", Fields(map[string]any{"matches": matches})), rows.Err()
}

func digest(raw []byte) [32]byte { return sha256.Sum256(raw) }

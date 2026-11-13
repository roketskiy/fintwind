package backendstore

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"sort"
	"time"

	sched "fintwind-go/internal/backendscheduled"
	"fintwind-go/internal/model"
	"github.com/google/uuid"
)

type scheduledReader interface {
	QueryRowContext(context.Context, string, ...any) *sql.Row
	QueryContext(context.Context, string, ...any) (*sql.Rows, error)
}

func (s *Store) Project(ctx context.Context, id string) (Object, error) {
	var name, path string
	var created int64
	err := s.read.QueryRowContext(ctx, `SELECT name,path,created_at FROM projects WHERE id=?`, id).Scan(&name, &path, &created)
	if err != nil {
		return nil, err
	}
	return Fields(map[string]any{"id": id, "name": name, "path": path, "created_at": created}), nil
}

func scheduledTask(ctx context.Context, db scheduledReader, id string) (sched.Task, error) {
	var task sched.Task
	var raw string
	var due sql.NullInt64
	err := db.QueryRowContext(ctx, `SELECT revision,enabled,next_due_at,data FROM scheduled_tasks WHERE id=?`, id).Scan(&task.Revision, &task.Enabled, &due, &raw)
	if err != nil {
		return task, err
	}
	enabled, revision := task.Enabled, task.Revision
	if err := json.Unmarshal([]byte(raw), &task); err != nil {
		return task, err
	}
	task.ID = id
	task.Enabled = enabled
	task.Revision = revision
	task.Pending = nil
	if due.Valid {
		task.NextDueAt = &due.Int64
	} else {
		task.NextDueAt = nil
	}
	var pending sched.Pending
	var state string
	var session sql.NullString
	err = db.QueryRowContext(ctx, `SELECT id,due_at,state,json_extract(data,'$.sessionId') FROM scheduled_occurrences WHERE task_id=? AND state<>'settled'`, id).Scan(&pending.ID, &pending.DueAt, &state, &session)
	if err != nil && !errors.Is(err, sql.ErrNoRows) {
		return task, err
	}
	if err == nil {
		pending.Preparing = state == "preparing"
		if session.Valid {
			pending.SessionID = &session.String
		}
		task.Pending = &pending
	}
	return task, nil
}

// The JSON column keeps the stored task shape. Operational
// columns are authoritative and must never be copied from a stale JSON value.
func scheduledData(task sched.Task) (string, error) {
	raw, err := json.Marshal(struct {
		Title     string          `json:"title"`
		Prompt    string          `json:"prompt"`
		Schedule  sched.Schedule  `json:"schedule"`
		Target    json.RawMessage `json:"target"`
		CreatedAt int64           `json:"createdAt"`
		UpdatedAt int64           `json:"updatedAt"`
		Last      *sched.Record   `json:"last"`
		RunCount  uint64          `json:"runCount"`
	}{task.Title, task.Prompt, task.Schedule, task.Target, task.CreatedAt, task.UpdatedAt, task.Last, task.RunCount})
	return string(raw), err
}

func (s *Store) ListScheduled(ctx context.Context) ([]sched.Task, error) {
	tx, err := s.read.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return nil, err
	}
	defer tx.Rollback()
	rows, err := tx.QueryContext(ctx, `SELECT id FROM scheduled_tasks`)
	if err != nil {
		return nil, err
	}
	ids := []string{}
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			rows.Close()
			return nil, err
		}
		ids = append(ids, id)
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return nil, err
	}
	result := make([]sched.Task, 0, len(ids))
	for _, id := range ids {
		task, err := scheduledTask(ctx, tx, id)
		if err != nil {
			return nil, err
		}
		result = append(result, task)
	}
	sort.Slice(result, func(i, j int) bool {
		if result[i].CreatedAt == result[j].CreatedAt {
			return result[i].ID < result[j].ID
		}
		return result[i].CreatedAt < result[j].CreatedAt
	})
	return result, nil
}

func (s *Store) SaveScheduled(ctx context.Context, edit sched.Edit, now int64) (sched.Task, error) {
	var task sched.Task
	if err := edit.Draft.Validate(); err != nil {
		return task, err
	}
	if now < 0 || edit.ExpectedRevision < 0 {
		return task, errors.New("invalid task timestamp or revision")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return task, err
	}
	defer tx.Rollback()
	creating := edit.Kind == "create"
	if !creating && edit.Kind != "update" {
		return task, errors.New("invalid scheduled edit")
	}
	if creating {
		task.ID = uuid.NewString()
		task.CreatedAt = now
	} else {
		if err := uuid.Validate(edit.ID); err != nil {
			return task, err
		}
		task, err = scheduledTask(ctx, tx, edit.ID)
		if err != nil {
			return task, err
		}
		if task.Revision != edit.ExpectedRevision {
			return task, errors.New("scheduled task changed by another client")
		}
		if edit.Draft.Schedule.Kind == "atTime" && edit.Draft.Schedule.Timezone == "" && task.Schedule.Kind == "atTime" {
			edit.Draft.Schedule.Timezone = task.Schedule.Timezone
		}
	}
	if creating && edit.Draft.Schedule.Kind == "atTime" && edit.Draft.Schedule.Timezone == "" {
		name := time.Local.String()
		if name != "Local" {
			if _, err := time.LoadLocation(name); err == nil {
				edit.Draft.Schedule.Timezone = name
			}
		}
	}
	if !edit.Draft.Enabled {
		task.NextDueAt = nil
	} else if creating || !task.Enabled || task.Schedule != edit.Draft.Schedule {
		due, err := edit.Draft.Schedule.Next(now)
		if err != nil {
			return task, err
		}
		task.NextDueAt = &due
	}
	task.Draft = edit.Draft
	task.UpdatedAt = now
	data, err := scheduledData(task)
	if err != nil {
		return task, err
	}
	if creating {
		_, err = tx.ExecContext(ctx, `INSERT INTO scheduled_tasks(id,revision,enabled,next_due_at,data) VALUES(?,0,?,?,?)`, task.ID, task.Enabled, task.NextDueAt, data)
	} else {
		var result sql.Result
		result, err = tx.ExecContext(ctx, `UPDATE scheduled_tasks SET revision=revision+1,enabled=?,next_due_at=?,data=? WHERE id=? AND revision=?`, task.Enabled, task.NextDueAt, data, task.ID, edit.ExpectedRevision)
		if err == nil {
			changed, e := result.RowsAffected()
			if e != nil {
				return task, e
			}
			if changed != 1 {
				return task, errors.New("scheduled revision conflict")
			}
		}
		if err == nil && !task.Enabled {
			_, err = tx.ExecContext(ctx, `DELETE FROM scheduled_occurrences WHERE task_id=? AND state='queued'`, task.ID)
		}
	}
	if err != nil {
		return task, err
	}
	task, err = scheduledTask(ctx, tx, task.ID)
	if err != nil {
		return task, err
	}
	return task, tx.Commit()
}

func (s *Store) DeleteScheduled(ctx context.Context, id string, revision int64) error {
	if err := uuid.Validate(id); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	result, err := tx.ExecContext(ctx, `DELETE FROM scheduled_tasks WHERE id=? AND revision=?`, id, revision)
	if err != nil {
		return err
	}
	changed, err := result.RowsAffected()
	if err != nil {
		return err
	}
	if changed != 1 {
		return errors.New("scheduled task missing or revision changed")
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM scheduled_occurrences WHERE task_id=? AND state<>'preparing'`, id); err != nil {
		return err
	}
	return tx.Commit()
}

func insertScheduledOccurrence(ctx context.Context, tx *sql.Tx, o sched.Occurrence) error {
	raw, err := json.Marshal(o)
	if err != nil {
		return err
	}
	_, err = tx.ExecContext(ctx, `INSERT INTO scheduled_occurrences(id,task_id,due_at,state,manual,data) VALUES(?,?,?,?,?,?)`, o.ID, o.TaskID, o.DueAt, o.State, o.Manual, string(raw))
	return err
}

func (s *Store) ClaimScheduled(ctx context.Context, id, owner string, now int64) (sched.Occurrence, error) {
	var result sched.Occurrence
	if uuid.Validate(owner) != nil || now < 0 {
		return result, errors.New("invalid occurrence owner or timestamp")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return result, err
	}
	defer tx.Rollback()
	task, err := scheduledTask(ctx, tx, id)
	if err != nil {
		return result, err
	}
	if task.Pending != nil {
		return result, errors.New("scheduled task already has a pending run")
	}
	result = sched.Occurrence{ID: uuid.NewString(), TaskID: id, DueAt: now, Manual: true, Owner: owner, Draft: task.Draft, State: "queued", Submission: json.RawMessage("null")}
	if err := insertScheduledOccurrence(ctx, tx, result); err != nil {
		return result, err
	}
	return result, tx.Commit()
}

func (s *Store) OpenScheduled(ctx context.Context) ([]sched.Occurrence, error) {
	rows, err := s.read.QueryContext(ctx, `SELECT data FROM scheduled_occurrences WHERE state<>'settled' ORDER BY due_at,id`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	result := []sched.Occurrence{}
	for rows.Next() {
		var raw string
		var occurrence sched.Occurrence
		if err := rows.Scan(&raw); err != nil {
			return nil, err
		}
		if err := json.Unmarshal([]byte(raw), &occurrence); err != nil {
			return nil, err
		}
		result = append(result, occurrence)
	}
	return result, rows.Err()
}

func scheduledOccurrence(ctx context.Context, tx *sql.Tx, id string) (sched.Occurrence, error) {
	var raw string
	var o sched.Occurrence
	err := tx.QueryRowContext(ctx, `SELECT data FROM scheduled_occurrences WHERE id=?`, id).Scan(&raw)
	if err != nil {
		return o, err
	}
	err = json.Unmarshal([]byte(raw), &o)
	return o, err
}

func (s *Store) PrepareScheduled(ctx context.Context, id, session, runtime string, submission json.RawMessage) error {
	if uuid.Validate(session) != nil || uuid.Validate(runtime) != nil {
		return errors.New("invalid preparation identities")
	}
	var record model.SubmissionRecord
	if err := json.Unmarshal(submission, &record); err != nil {
		return err
	}
	if record.SessionID != session || uuid.Validate(record.Receipt.ID) != nil {
		return errors.New("submission belongs to another session or is invalid")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	o, err := scheduledOccurrence(ctx, tx, id)
	if err != nil {
		return err
	}
	if o.State == "preparing" && o.SessionID != nil && *o.SessionID == session {
		var saved struct {
			Receipt struct {
				ID string `json:"id"`
			} `json:"receipt"`
		}
		if err := json.Unmarshal(o.Submission, &saved); err != nil {
			return err
		}
		if saved.Receipt.ID == record.Receipt.ID {
			return nil
		}
	}
	if o.State != "queued" {
		return errors.New("scheduled run already started")
	}
	o.State = "preparing"
	o.SessionID = &session
	o.RuntimeID = &runtime
	o.Submission = submission
	raw, err := json.Marshal(o)
	if err != nil {
		return err
	}
	_, err = tx.ExecContext(ctx, `UPDATE scheduled_occurrences SET state='preparing',data=? WHERE id=? AND state='queued'`, string(raw), id)
	if err != nil {
		return err
	}
	return tx.Commit()
}

func settleScheduled(ctx context.Context, tx *sql.Tx, id string, outcome sched.Outcome, now int64) error {
	o, err := scheduledOccurrence(ctx, tx, id)
	if errors.Is(err, sql.ErrNoRows) {
		return nil
	}
	if err != nil {
		return err
	}
	if o.State == "settled" {
		return nil
	}
	o.State = "settled"
	raw, err := json.Marshal(o)
	if err != nil {
		return err
	}
	if _, err := tx.ExecContext(ctx, `UPDATE scheduled_occurrences SET state='settled',data=? WHERE id=?`, string(raw), id); err != nil {
		return err
	}
	task, err := scheduledTask(ctx, tx, o.TaskID)
	if errors.Is(err, sql.ErrNoRows) {
		_, err = tx.ExecContext(ctx, `DELETE FROM scheduled_occurrences WHERE id=?`, id)
		return err
	}
	if err != nil {
		return err
	}
	if outcome.Kind != "missed" {
		task.RunCount++
	}
	task.Last = &sched.Record{DueAt: o.DueAt, At: now, Outcome: outcome}
	if !o.Manual {
		task.NextDueAt = nil
		if task.Enabled && task.Schedule.Kind != "once" {
			due, err := task.Schedule.Next(now)
			if err == nil {
				task.NextDueAt = &due
			}
		}
		if task.Schedule.Kind == "once" {
			task.Enabled = false
		}
	}
	data, err := scheduledData(task)
	if err != nil {
		return err
	}
	_, err = tx.ExecContext(ctx, `UPDATE scheduled_tasks SET enabled=?,next_due_at=?,data=? WHERE id=?`, task.Enabled, task.NextDueAt, data, task.ID)
	return err
}

func (s *Store) SettleScheduled(ctx context.Context, id string, outcome sched.Outcome, now int64) error {
	switch outcome.Kind {
	case "accepted", "unconfirmed":
		if outcome.SessionID == nil || outcome.SubmissionID == nil || uuid.Validate(*outcome.SessionID) != nil || uuid.Validate(*outcome.SubmissionID) != nil {
			return errors.New("invalid scheduled outcome identity")
		}
	case "notSent", "rejected", "missed":
	default:
		return errors.New("invalid scheduled outcome")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if err := settleScheduled(ctx, tx, id, outcome, now); err != nil {
		return err
	}
	return tx.Commit()
}

func (s *Store) ClaimDueScheduled(ctx context.Context, owner string, now int64, limit int) ([]sched.Occurrence, error) {
	if uuid.Validate(owner) != nil || now < 0 || limit < 0 || limit > 1000 {
		return nil, errors.New("invalid due claim")
	}
	result := []sched.Occurrence{}
	if limit == 0 {
		return result, nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, err
	}
	defer tx.Rollback()
	rows, err := tx.QueryContext(ctx, `SELECT id FROM scheduled_tasks WHERE enabled=1 AND next_due_at<=? ORDER BY next_due_at,id`, now)
	if err != nil {
		return nil, err
	}
	ids := []string{}
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			rows.Close()
			return nil, err
		}
		ids = append(ids, id)
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return nil, err
	}
	for _, id := range ids {
		if len(result) >= limit {
			break
		}
		task, err := scheduledTask(ctx, tx, id)
		if err != nil {
			return nil, err
		}
		if task.Pending != nil || task.NextDueAt == nil {
			continue
		}
		o := sched.Occurrence{ID: uuid.NewString(), TaskID: id, DueAt: *task.NextDueAt, Owner: owner, Draft: task.Draft, State: "queued", Submission: json.RawMessage("null")}
		if err := insertScheduledOccurrence(ctx, tx, o); err != nil {
			return nil, err
		}
		if task.Schedule.Kind != "every" && now-o.DueAt > 600 {
			if err := settleScheduled(ctx, tx, o.ID, sched.Outcome{Kind: "missed"}, now); err != nil {
				return nil, err
			}
			continue
		}
		result = append(result, o)
	}
	return result, tx.Commit()
}

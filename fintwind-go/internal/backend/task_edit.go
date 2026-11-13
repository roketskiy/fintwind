package backend

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/backendworkspace"
	"fintwind-go/internal/model"
	"github.com/google/uuid"
)

func taskRows(source backendstore.Object, key string) ([]backendstore.Object, error) {
	var rows []backendstore.Object
	err := json.Unmarshal(source[key], &rows)
	return rows, err
}

func validateTaskEdit(source backendstore.Object, count int, fork bool) error {
	if source.String("status") != "idle" && source.String("status") != "failed" {
		return errors.New("stop the task before editing history")
	}
	var work []model.BackgroundWorkSnapshot
	if raw := source["background_work"]; len(raw) > 0 && string(raw) != "null" {
		if err := json.Unmarshal(raw, &work); err != nil {
			return errors.New("invalid background snapshot")
		}
		for _, child := range work {
			if child.Item.Live() {
				return errors.New("stop background work before editing history")
			}
		}
	}
	turns, err := taskRows(source, "turns")
	if err != nil {
		return err
	}
	if count <= 0 || count > len(turns) {
		return errors.New("selected turn unavailable")
	}
	turn := turns[count-1]
	number, err := turn.Int("turn_count")
	if err != nil || number != int64(count) {
		return errors.New("selected turn unavailable")
	}
	cursor, _ := backendstore.Decode(source["provider_cursor"])
	if fork {
		if source.String("provider") != "opencode" || cursor.String("provider") != "openCode" || cursor.String("sessionId") == "" || !turn.Bool("provider_turn_started") {
			return errors.New("selected response cannot be forked")
		}
	} else {
		messages, err := taskRows(source, "messages")
		if err != nil {
			return err
		}
		found := false
		for _, message := range messages {
			if message.String("turn_id") == turn.String("id") && message.String("role") == "user" {
				found = true
			}
		}
		if !found {
			return errors.New("selected user message unavailable")
		}
		for _, item := range turns[count-1:] {
			if item.Bool("provider_turn_started") && (source.String("provider") != "opencode" || cursor.String("provider") != "openCode" || cursor.String("sessionId") == "") {
				return errors.New("provider conversation unavailable")
			}
		}
	}
	return nil
}

func truncateTask(source backendstore.Object, count int) error {
	turns, err := taskRows(source, "turns")
	if err != nil {
		return err
	}
	keep := map[string]bool{}
	for _, turn := range turns[:count] {
		keep[turn.String("id")] = true
	}
	source.Set("turns", turns[:count])
	messageCount := 0
	for _, key := range []string{"messages", "transcript_blocks"} {
		rows, err := taskRows(source, key)
		if err != nil {
			return err
		}
		retained := []backendstore.Object{}
		for _, row := range rows {
			if id := row.String("turn_id"); id != "" && !keep[id] {
				continue
			}
			if key == "transcript_blocks" {
				position, err := row.Int("after_message")
				if err != nil {
					return err
				}
				if position > int64(messageCount) {
					row.Set("after_message", messageCount)
				}
			}
			retained = append(retained, row)
		}
		source.Set(key, retained)
		if key == "messages" {
			messageCount = len(retained)
		}
	}
	source.Set("updated_at", time.Now().Unix())
	if raw := source["background_work"]; len(raw) > 0 && string(raw) != "null" {
		var work []model.BackgroundWorkSnapshot
		if err := json.Unmarshal(raw, &work); err != nil {
			return err
		}
		origins := map[string]bool{}
		blocks, _ := taskRows(source, "transcript_blocks")
		for _, block := range blocks {
			content, _ := backendstore.Decode(block["content"])
			if content.String("kind") == "activities" {
				var activities []model.ActivityItem
				_ = json.Unmarshal(content["data"], &activities)
				for _, activity := range activities {
					if activity.SourceID != nil {
						origins[*activity.SourceID] = true
					}
				}
			}
		}
		retained := []model.BackgroundWorkSnapshot{}
		for _, child := range work {
			ids := []string{}
			for _, id := range child.Item.OriginActivityIDs {
				if origins[id] {
					ids = append(ids, id)
				}
			}
			child.Item.OriginActivityIDs = ids
			if len(ids) > 0 || child.Item.Live() {
				retained = append(retained, child)
			}
		}
		source.Set("background_work", retained)
	}
	return nil
}

func displayTaskTitle(source backendstore.Object) string {
	if title := source.String("title"); title != "" && title != "New task" {
		return title
	}
	if title := source.String("auto_title"); title != "" {
		return title
	}
	return source.String("title")
}
func forkTitle(source backendstore.Object, sessions []backendstore.Object) string {
	base := displayTaskTitle(source)
	titles := map[string]bool{}
	for _, session := range sessions {
		if session.String("project_id") == source.String("project_id") {
			titles[displayTaskTitle(session)] = true
		}
	}
	if i := strings.LastIndex(base, " ("); i > 0 && strings.HasSuffix(base, ")") {
		if n, err := strconv.Atoi(base[i+2 : len(base)-1]); err == nil && n >= 2 && titles[base[:i]] {
			base = base[:i]
		}
	}
	highest := 1
	for title := range titles {
		if strings.HasPrefix(title, base+" (") && strings.HasSuffix(title, ")") {
			if n, err := strconv.Atoi(title[len(base)+2 : len(title)-1]); err == nil && n > highest {
				highest = n
			}
		}
	}
	return fmt.Sprintf("%s (%d)", base, highest+1)
}

func cloneFork(source backendstore.Object, cursor json.RawMessage, count int, title string) (backendstore.Object, error) {
	raw, err := json.Marshal(source)
	if err != nil {
		return nil, err
	}
	fork, err := backendstore.Decode(raw)
	if err != nil {
		return nil, err
	}
	if err := truncateTask(fork, count); err != nil {
		return nil, err
	}
	id := uuid.NewString()
	turns, err := taskRows(fork, "turns")
	if err != nil {
		return nil, err
	}
	ids := map[string]string{}
	for _, turn := range turns {
		old := turn.String("id")
		ids[old] = uuid.NewString()
		turn.Set("id", ids[old])
		delete(turn, "submission")
		if checkpoint, e := backendstore.Decode(turn["checkpoint"]); e == nil {
			n, _ := checkpoint.Int("turn_count")
			checkpoint.Set("git_ref", fmt.Sprintf("refs/fintwind/session-%s-turn-%d", id, n))
			turn.Set("checkpoint", checkpoint)
		}
	}
	fork.Set("turns", turns)
	for _, key := range []string{"messages", "transcript_blocks"} {
		rows, err := taskRows(fork, key)
		if err != nil {
			return nil, err
		}
		for _, row := range rows {
			if old := row.String("turn_id"); old != "" {
				row.Set("turn_id", ids[old])
			}
			if key == "messages" {
				row.Set("id", uuid.NewString())
				row.Set("streaming", false)
			}
		}
		fork.Set(key, rows)
	}
	native, err := backendstore.Decode(cursor)
	if err != nil {
		return nil, err
	}
	fork.Set("id", id)
	fork.Set("title", "New task")
	fork.Set("auto_title", title)
	fork.Set("status", "idle")
	fork.Set("created_at", time.Now().Unix())
	fork.Set("updated_at", time.Now().Unix())
	fork.Set("native_session_id", native.String("sessionId"))
	fork["provider_cursor"] = cursor
	for _, key := range []string{"provider_session_id", "runtime_event_cursor", "_fintwind_removed_turn_ids", "_fintwind_shell_cursors"} {
		delete(fork, key)
	}
	fork.Set("queued_messages", []any{})
	fork.Set("background_work", []any{})
	return fork, nil
}

func (s *Server) taskEdit(ctx context.Context, id string, c backendstore.Object) (backendstore.Object, error) {
	if uuid.Validate(id) != nil || id == uuid.Nil.String() {
		return nil, errors.New("task identity required")
	}
	n, err := c.Int("turnCount")
	if err != nil || n <= 0 || n > 1000000 {
		return nil, errors.New("positive turn count required")
	}
	count := int(n)
	isFork := c.String("type") == "forkSessionFromResponse"
	source, _, err := s.Store.TaskEditSnapshot(ctx, id)
	if err != nil {
		return nil, err
	}
	if err := validateTaskEdit(source, count, isFork); err != nil {
		return nil, err
	}
	// The server edit gate excludes starts/admissions, and Stop waits for the
	// old actor's final flush before the authoritative snapshot is captured.
	s.runtime.Stop(id)
	source, expected, err := s.Store.TaskEditSnapshot(ctx, id)
	if err != nil {
		return nil, err
	}
	if err := validateTaskEdit(source, count, isFork); err != nil {
		return nil, err
	}
	project, err := s.Store.Project(ctx, source.String("project_id"))
	if err != nil {
		return nil, err
	}
	cwd := project.String("path")
	if workspace, e := backendstore.Decode(source["workspace"]); e == nil && workspace.String("path") != "" {
		cwd = workspace.String("path")
	}
	turns, err := taskRows(source, "turns")
	if err != nil {
		return nil, err
	}
	retained := count
	if !isFork {
		retained--
	}
	providerCount, rollback := 0, 0
	for i, turn := range turns {
		if turn.Bool("provider_turn_started") {
			if i < retained {
				providerCount++
			} else {
				rollback++
			}
		}
	}
	cursor := source["provider_cursor"]
	if native, e := backendstore.Decode(cursor); e == nil && native.String("provider") == "openCode" && native.String("sessionId") != "" {
		service, e := s.servicePool.acquire(ctx, backendstore.Fields(map[string]any{"directory": cwd}))
		if e != nil {
			return nil, e
		}
		client, ok := service.(*backendengine.Client)
		if !ok {
			return nil, errors.New("background session status unavailable")
		}
		rows, active, e := client.Children(ctx, native.String("sessionId"))
		if e != nil {
			return nil, errors.New("background session status unavailable; history was not changed")
		}
		for _, child := range rows {
			if _, running := active[child.String("id")]; running {
				return nil, errors.New("stop background work before editing history")
			}
		}
	}
	if isFork || rollback > 0 {
		binary, err := s.providerBinary("")
		if err != nil {
			return nil, err
		}
		native, err := backendstore.Decode(cursor)
		if err != nil {
			return nil, err
		}
		kind := "openCodeRevert"
		if isFork {
			kind = "openCode"
		}
		raw, _ := json.Marshal(map[string]any{"type": "forkProviderSession", "request": map[string]any{"provider": kind, "binary": binary, "cwd": cwd, "session_id": native.String("sessionId"), "turn_count": providerCount}})
		response, _, err := s.services.Handle(ctx, raw)
		if err != nil {
			return nil, err
		}
		out, err := backendstore.Decode(response)
		if err != nil {
			return nil, err
		}
		result, err := backendstore.Decode(out["result"])
		if err != nil {
			return nil, err
		}
		cursor = result["cursor"]
	}
	if !isFork {
		if err := truncateTask(source, retained); err != nil {
			return nil, err
		}
		source.Set("status", "idle")
		delete(source, "runtime_event_cursor")
		if err := s.Store.CommitTaskEdit(ctx, id, expected, source, false); err != nil {
			return nil, err
		}
		s.notify("taskStateChanged")
		return backendstore.Reply("sessionRewound", backendstore.Fields(map[string]any{"session": source, "cleanupWarning": nil})), nil
	}
	nativeFork, err := backendstore.Decode(cursor)
	if err != nil {
		return nil, err
	}
	original, _ := backendstore.Decode(source["provider_cursor"])
	if nativeFork.String("sessionId") == "" || nativeFork.String("sessionId") == original.String("sessionId") {
		return nil, errors.New("provider did not create an independent fork")
	}
	state, err := s.Store.Load(ctx, s.Cwd)
	if err != nil {
		return nil, err
	}
	sessions, err := taskRows(state, "sessions")
	if err != nil {
		return nil, err
	}
	fork, err := cloneFork(source, cursor, count, forkTitle(source, sessions))
	if err != nil {
		return nil, err
	}
	refs := func(kind string) error {
		raw, _ := json.Marshal(map[string]any{"type": kind, "cwd": cwd, "session_id": fork.String("id"), "source_session_id": id, "target_session_id": fork.String("id"), "through_turn_count": count})
		_, err := backendworkspace.DispatchWithQueries(ctx, s.workspaceForPath(cwd), raw, &s.queries)
		return err
	}
	var warning any
	if err := refs("copySessionRefs"); err != nil {
		warning = err.Error()
	}
	if err := s.Store.CommitTaskEdit(ctx, id, expected, fork, true); err != nil {
		cleanup, stop := context.WithTimeout(context.Background(), 5*time.Second)
		defer stop()
		raw, _ := json.Marshal(map[string]any{"type": "deleteAllSessionRefs", "cwd": cwd, "session_id": fork.String("id")})
		_, _ = backendworkspace.DispatchWithQueries(cleanup, s.workspaceForPath(cwd), raw, &s.queries)
		return nil, err
	}
	s.notify("taskStateChanged")
	return backendstore.Reply("sessionForked", backendstore.Fields(map[string]any{"session": fork, "checkpointWarning": warning})), nil
}

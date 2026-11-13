package backendruntime

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"time"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"
)

type shellState struct {
	item       model.BackgroundWorkItem
	cursor     uint64
	generation uint64
	settled    bool
	history    bool
}
type shellResult struct {
	id         string
	generation uint64
	page       backendengine.ShellPage
	err        error
}

func (a *actor) observeShell(item model.ActivityItem, payload backendstore.Object) error {
	metadata, err := backendstore.Decode(payload["metadata"])
	if err != nil {
		return nil
	}
	id := strings.TrimSpace(metadata.String("shellID"))
	if id == "" {
		return nil
	}
	owner := a
	if a.parent != nil {
		owner = a.parent
	}
	if owner.shells == nil {
		owner.shells = map[string]*shellState{}
	}
	if existing := owner.shells[id]; existing != nil {
		return nil
	}
	if len(owner.shells)+len(owner.children)+len(owner.otherBackground) >= 256 {
		return owner.emit("error", "Background process capacity reached")
	}
	command := ""
	cwd := ""
	if item.Arguments != nil {
		input, _ := backendstore.Decode([]byte(*item.Arguments))
		command = input.String("command")
		if command == "" {
			command = input.String("cmd")
		}
		cwd = input.String("workdir")
		if cwd == "" {
			cwd = input.String("cwd")
		}
	}
	parent := a.nativeID
	title := command
	if title == "" {
		title = "Background process"
	}
	origins := []string{}
	if item.SourceID != nil {
		origins = append(origins, *item.SourceID)
	}
	work := model.BackgroundWorkItem{Key: model.BackgroundWorkKey{Kind: "process", ProviderID: id}, Title: title, Command: &command, ParentID: &parent, ControlID: &id, Background: true, CanStop: true, Status: "running", StartedAtMs: uint64Now(), UpdatedAtMs: uint64Now(), OriginActivityIDs: origins}
	if cwd != "" {
		work.Cwd = &cwd
	}
	owner.shells[id] = &shellState{item: work, generation: 1}
	owner.dirty = true
	return owner.backgroundEvent("upsert", work)
}
func (a *actor) pollShells() {
	if a.shellReading || len(a.shells) == 0 {
		return
	}
	if a.shellResults == nil {
		a.shellResults = make(chan []shellResult, 1)
	}
	jobs := []struct {
		id                 string
		owner              string
		cursor, generation uint64
	}{}
	for id, shell := range a.shells {
		if !shell.settled {
			jobs = append(jobs, struct {
				id                 string
				owner              string
				cursor, generation uint64
			}{id, *shell.item.ParentID, shell.cursor, shell.generation})
		}
	}
	if len(jobs) == 0 {
		return
	}
	a.shellReading = true
	ctx, cancel := context.WithTimeout(a.ctx, 15*time.Second)
	client, results := a.process.Client, a.shellResults
	go func() {
		defer cancel()
		out := []shellResult{}
		for _, job := range jobs {
			if ctx.Err() != nil {
				break
			}
			page, err := client.Shell(ctx, job.id, job.cursor, job.owner)
			out = append(out, shellResult{id: job.id, generation: job.generation, page: page, err: err})
		}
		select {
		case results <- out:
		case <-a.ctx.Done():
		}
	}()
}
func shellStatus(row backendstore.Object) string {
	switch row.String("status") {
	case "running":
		return "running"
	case "timeout":
		return "failed"
	case "killed":
		return "stopped"
	}
	code, _ := row.Int("exit")
	if code != 0 {
		return "failed"
	}
	return "completed"
}
func (a *actor) applyShells(results []shellResult) error {
	for _, result := range results {
		s := a.shells[result.id]
		if s == nil || s.generation != result.generation || result.err != nil {
			continue
		}
		page := result.page
		if page.Missing {
			if s.item.Live() {
				s.item.Status = "lost"
			}
			s.item.CanStop = false
			s.settled = true
			s.generation++
			if err := a.backgroundEvent("upsert", s.item); err != nil {
				return err
			}
			a.dirty = true
			continue
		}
		metadata, _ := backendstore.Decode(page.Row["metadata"])
		if s.item.ParentID == nil || metadata.String("sessionID") != *s.item.ParentID {
			continue
		}
		if s.history {
			if shellStatus(page.Row) != "running" {
				delete(a.shells, result.id)
				continue
			}
			s.history = false
			s.item.StartedAtMs = uint64Now()
		}
		if page.Cursor < s.cursor {
			continue
		}
		if page.Output != "" && page.Cursor > s.cursor {
			tail := ""
			if s.item.Output != nil {
				tail = *s.item.Output
			}
			previous := len(tail)
			tail += page.Output
			if len(tail) > 64<<10 {
				tail = tail[len(tail)-(64<<10):]
				s.item.OutputTruncated = true
			}
			remaining := (8 << 20) - a.backgroundBytes + previous - 4096
			if remaining < 0 {
				remaining = 0
			}
			if len(tail) > remaining {
				tail = tail[len(tail)-remaining:]
				s.item.OutputTruncated = true
			}
			a.backgroundBytes += len(tail) - previous
			s.item.Output = &tail
			if err := a.emit("backgroundWork", map[string]any{"type": "outputDelta", "key": s.item.Key, "delta": page.Output}); err != nil {
				return err
			}
		}
		s.cursor = page.Cursor
		status := shellStatus(page.Row)
		if !s.item.Live() && status == "running" {
			continue
		}
		s.item.Status = status
		s.item.CanStop = s.item.Live()
		s.item.UpdatedAtMs = uint64Now()
		if exit, err := page.Row.Int("exit"); err == nil {
			code := int32(exit)
			s.item.ExitCode = &code
		}
		if !s.item.Live() && page.Cursor >= page.Size {
			s.settled = true
		}
		if err := a.backgroundEvent("upsert", s.item); err != nil {
			return err
		}
		a.dirty = true
	}
	return nil
}

func (a *actor) restoreNativeShells(items []model.BackgroundWorkItem) {
	if a.shells == nil {
		a.shells = map[string]*shellState{}
	}
	for _, item := range items {
		if len(a.shells)+len(a.children) >= 256 {
			break
		}
		if a.shells[item.Key.ProviderID] == nil {
			a.shells[item.Key.ProviderID] = &shellState{item: item, generation: 1, history: true}
		}
	}
}
func (a *actor) shellEvent(event backendengine.Event) error {
	payload, err := backendstore.Decode(event.Payload)
	if err != nil {
		return err
	}
	s := a.shells[payload.String("id")]
	if s == nil {
		return nil
	}
	switch event.Kind {
	case "shell.exited":
		s.item.Status = shellStatus(payload)
		s.item.CanStop = false
		if exit, err := payload.Int("exit"); err == nil {
			value := int32(exit)
			s.item.ExitCode = &value
		}
	case "shell.deleted":
		if s.item.Live() {
			s.item.Status = "lost"
		}
		s.item.CanStop = false
		s.settled = true
		s.generation++
	default:
		return nil
	}
	s.item.UpdatedAtMs = uint64Now()
	a.dirty = true
	return a.backgroundEvent("upsert", s.item)
}
func (a *actor) stopShell(ctx context.Context, command backendstore.Object) error {
	id := command.String("controlId")
	s := a.shells[id]
	key, _ := backendstore.Decode(command["key"])
	if s == nil || key.String("kind") != "process" || key.String("providerId") != id || s.item.ParentID == nil {
		return errors.New("unknown background process control identity")
	}
	page, err := a.process.Client.Shell(ctx, id, s.cursor, *s.item.ParentID)
	if err != nil {
		return err
	}
	meta, _ := backendstore.Decode(page.Row["metadata"])
	if page.Missing || meta.String("sessionID") != *s.item.ParentID {
		return errors.New("background process ownership unavailable")
	}
	if err := a.process.Client.StopWork(ctx, id); err != nil {
		_ = a.emit("backgroundWork", map[string]any{"type": "stopFailed", "key": s.item.Key, "message": "Could not stop background process"})
		return err
	}
	s.item.Status = "stopped"
	s.item.CanStop = false
	s.generation++
	a.dirty = true
	return a.backgroundEvent("upsert", s.item)
}
func (a *actor) restoreShells() {
	if a.shells != nil {
		return
	}
	a.shells = map[string]*shellState{}
	var cursors map[string]uint64
	_ = json.Unmarshal(a.snapshot["_fintwind_shell_cursors"], &cursors)
	retained := []model.BackgroundWorkSnapshot{}
	for _, saved := range a.otherBackground {
		item := saved.Item
		if item.Key.Kind != "process" {
			retained = append(retained, saved)
			continue
		}
		if item.Key.ProviderID == "" || item.ParentID == nil || (*item.ParentID != a.nativeID && a.children[*item.ParentID] == nil) {
			continue
		}
		id := item.Key.ProviderID
		item.ControlID = &id
		a.shells[id] = &shellState{item: item, cursor: cursors[id], generation: 1, settled: !item.Live()}
		if item.Output != nil {
			a.backgroundBytes += len(*item.Output)
		}
	}
	a.otherBackground = retained
}

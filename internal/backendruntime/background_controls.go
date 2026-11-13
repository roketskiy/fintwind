package backendruntime

import (
	"context"
	"encoding/json"
	"errors"
	"time"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"
)

func (a *actor) stopChild(ctx context.Context, command backendstore.Object, c *childState) error {
	key, err := backendstore.Decode(command["key"])
	if err != nil || key.String("kind") != "subagent" || key.String("providerId") != c.item.Key.ProviderID || c.item.ParentID == nil || *c.item.ParentID != a.nativeID || c.item.ControlID == nil || *c.item.ControlID != command.String("controlId") {
		return errors.New("invalid subagent control identity")
	}
	if !c.item.Live() {
		return nil
	}
	if err := a.process.Client.Interrupt(ctx, c.item.Key.ProviderID); err != nil {
		_ = a.emit("backgroundWork", map[string]any{"type": "stopFailed", "key": c.item.Key, "message": "Could not stop subagent"})
		return err
	}
	c.item.Status = "stopped"
	a.backgroundRevision++
	a.clearChildRequests(c.item.Key.ProviderID)
	c.actor.finish(false, "interrupted")
	if err := a.childUpsert(c); err != nil {
		return err
	}
	return a.childTranscript(c, "finished", map[string]any{"success": false})
}

type backgroundResult struct {
	generation, revision uint64
	rows                 []backendstore.Object
	active               map[string]json.RawMessage
	transcripts          map[string]model.NativeTranscript
	err                  error
	truncated            map[string]bool
}

func (a *actor) refreshChildren(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if a.backgroundCancel != nil {
		a.backgroundCancel()
	}
	if a.backgroundResults == nil {
		a.backgroundResults = make(chan backgroundResult, 1)
	}
	a.backgroundGeneration++
	generation, revision := a.backgroundGeneration, a.backgroundRevision
	work, cancel := context.WithTimeout(a.ctx, 30*time.Second)
	a.backgroundCancel = cancel
	client, parent, results := a.process.Client, a.nativeID, a.backgroundResults
	go func() {
		defer cancel()
		rows, active, err := client.Children(work, parent)
		result := backgroundResult{generation: generation, revision: revision, rows: rows, active: active, err: err, transcripts: map[string]model.NativeTranscript{}, truncated: map[string]bool{}}
		if err == nil {
			total := 0
			for _, row := range rows {
				if work.Err() != nil {
					result.err = work.Err()
					break
				}
				id := row.String("id")
				if id == "" || id == parent {
					result.err = errors.New("invalid child identity")
					break
				}
				native, e := client.Transcript(work, id)
				if e != nil {
					continue
				}
				raw, _ := json.Marshal(native)
				if len(raw) > 1<<20 || total+len(raw) > 8<<20 {
					result.truncated[id] = true
					continue
				}
				total += len(raw)
				result.transcripts[id] = native
			}
		}
		select {
		case results <- result:
		case <-work.Done():
		}
	}()
	return nil
}

func (a *actor) applyChildren(result backgroundResult) error {
	rows, active := result.rows, result.active
	if a.children == nil {
		a.children = map[string]*childState{}
	}
	items := []model.BackgroundWorkItem{}
	for _, shell := range a.shells {
		if shell.item.Live() && !shell.history {
			items = append(items, shell.item)
		}
	}
	for _, saved := range a.otherBackground {
		if saved.Item.Live() {
			items = append(items, saved.Item)
		}
	}
	seen := map[string]bool{}
	for _, row := range rows {
		id := row.String("id")
		if id == "" || id == a.nativeID || seen[id] {
			return errors.New("invalid child session identity")
		}
		seen[id] = true
		c := a.children[id]
		if c == nil {
			if len(a.children)+len(a.shells)+len(a.otherBackground) >= maxChildren {
				_ = a.emit("error", "Background session capacity reached")
				continue
			}
			parent := a.nativeID
			title := row.String("title")
			if title == "" {
				title = "Subagent"
			}
			c = a.newChild(model.BackgroundWorkItem{Key: model.BackgroundWorkKey{Kind: "subagent", ProviderID: id}, Title: title, ParentID: &parent, ControlID: &id, Background: true, OriginActivityIDs: []string{}, StartedAtMs: uint64Now()})
			a.children[id] = c
		}
		updateChildMetadata(&c.item, row)
		_, running := active[id]
		c.item.Status = restoredChildStatus(row, running)
		c.item.CanStop = c.item.Live()
		if result.truncated[id] {
			c.truncated = true
			c.item.OutputTruncated = true
			detail := "Subagent transcript reached its size limit"
			c.item.Detail = &detail
		}
		c.item.UpdatedAtMs = uint64Now()
		items = append(items, c.item)
	}
	for id, c := range a.children {
		if !seen[id] && c.item.Live() {
			c.item.Status = "lost"
			c.item.CanStop = false
			items = append(items, c.item)
		}
	}
	if err := a.backgroundEvent("reconcileLive", items); err != nil {
		return err
	}
	a.dirty = true
	for _, row := range rows {
		c := a.children[row.String("id")]
		if c == nil {
			continue
		}
		native, ok := result.transcripts[c.item.Key.ProviderID]
		if !ok {
			if err := a.activatePendingChild(c); err != nil {
				return err
			}
			continue
		}
		if len(native.Messages) == 0 && len(native.Blocks) == 0 && len(native.Turns) == 0 {
			if err := a.activatePendingChild(c); err != nil {
				return err
			}
			continue
		}
		transcript := model.BackgroundTranscript(native)
		raw, _ := json.Marshal(transcript)
		if len(raw) > 1<<20 || a.backgroundBytes-c.bytes+len(raw) > 8<<20 {
			c.truncated = true
			c.item.OutputTruncated = true
			text := "Subagent transcript reached its size limit"
			c.item.Detail = &text
			_ = a.childUpsert(c)
			continue
		}
		a.backgroundBytes += len(raw) - c.bytes
		c.bytes = len(raw)
		c.actor.snapshot.Set("messages", native.Messages)
		c.actor.snapshot.Set("transcript_blocks", transcript.TranscriptBlocks)
		c.actor.snapshot.Set("turns", native.Turns)
		if err := c.actor.loadSnapshot(); err != nil {
			return err
		}
		if len(c.actor.turns) > 0 {
			c.actor.turnID = c.actor.turns[len(c.actor.turns)-1].String("id")
		}
		c.actor.started = c.item.Live()
		if err := a.childTranscript(c, "snapshot", map[string]any{"transcript": transcript}); err != nil {
			return err
		}
		if err := a.activatePendingChild(c); err != nil {
			return err
		}
	}
	for id, events := range a.unresolvedChildEvents {
		if !seen[id] {
			for _, event := range events {
				a.unresolvedChildBytes -= len(event.Payload)
			}
			delete(a.unresolvedChildEvents, id)
		}
	}
	return a.flush()
}

func (a *actor) activatePendingChild(c *childState) error {
	id := c.item.Key.ProviderID
	queued := a.unresolvedChildEvents[id]
	if len(queued) == 0 {
		return nil
	}
	delete(a.unresolvedChildEvents, id)
	for _, event := range queued {
		a.unresolvedChildBytes -= len(event.Payload)
	}
	remaining := a.pendingChildren[:0]
	for _, call := range a.pendingChildren {
		if call.target == id {
			if err := a.armChild(c, call); err != nil {
				return err
			}
		} else {
			remaining = append(remaining, call)
		}
	}
	a.pendingChildren = remaining
	firstStart := true
	for _, event := range queued {
		if event.Kind == "session.execution.started" && firstStart && len(c.revives) > 0 {
			c.item.Status = "completed"
			c.actor.started = false
			firstStart = false
		}
		if err := a.childEvent(event); err != nil {
			return err
		}
	}
	return nil
}

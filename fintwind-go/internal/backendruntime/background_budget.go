package backendruntime

import (
	"bytes"
	"encoding/json"
	"errors"
	"sort"

	"fintwind-go/internal/model"
)

// The persisted envelope has the same bound as restoration, including metadata,
// JSON escaping and shell output. Budget pressure never writes an unreadable
// snapshot. Only background output is shortened; the root task is unchanged.
func boundedBackgroundSnapshot(snapshots []model.BackgroundWorkSnapshot) (json.RawMessage, error) {
	if len(snapshots) > maxChildren {
		return nil, errors.New("background registry exceeds budget")
	}
	parts := make([]json.RawMessage, len(snapshots))
	total := 2
	if len(parts) > 0 {
		total += len(parts) - 1
	}
	mark := func(item *model.BackgroundWorkItem) {
		item.OutputTruncated = true
		detail := "Background output reached its snapshot size limit"
		item.Detail = &detail
	}
	for i := range snapshots {
		item := &snapshots[i].Item
		bound := func(value *string, limit int) {
			if value == nil {
				return
			}
			text, truncated := boundedBackgroundText(*value, limit)
			if truncated {
				*value = text
				mark(item)
			}
		}
		// Copy pointers before trimming; this view must not change live controls.
		copyBound := func(value **string, limit int) {
			if *value == nil {
				return
			}
			copy := **value
			bound(&copy, limit)
			*value = &copy
		}
		bound(&item.Title, 2048)
		copyBound(&item.Command, 16000)
		copyBound(&item.Cwd, 4096)
		copyBound(&item.Role, 256)
		copyBound(&item.Model, 1024)
		copyBound(&item.Detail, 2048)
		origins := []string{}
		start := max(0, len(item.OriginActivityIDs)-128)
		if start > 0 {
			mark(item)
		}
		for _, id := range item.OriginActivityIDs[start:] {
			if len(id) > 256 {
				mark(item)
				continue
			}
			origins = append(origins, id)
		}
		item.OriginActivityIDs = origins
		raw, err := json.Marshal(snapshots[i])
		if err != nil {
			return nil, err
		}
		parts[i] = raw
		total += len(raw)
	}
	order := make([]int, len(parts))
	for i := range order {
		order[i] = i
	}
	sort.Slice(order, func(i, j int) bool { return len(parts[order[i]]) > len(parts[order[j]]) })
	for _, i := range order {
		if total <= maxBackgroundSnapshotBytes {
			break
		}
		entry := &snapshots[i]
		// A whole transcript is replaced by an explicit truncated view, rather
		// than leaving dangling turn/message references in partially cut JSON.
		entry.Transcript = model.BackgroundWorkTranscript{Messages: []model.Message{}, TranscriptBlocks: []json.RawMessage{}, Turns: []model.AgentTurn{}}
		entry.Item.Output = nil
		mark(&entry.Item)
		raw, err := json.Marshal(entry)
		if err != nil {
			return nil, err
		}
		total += len(raw) - len(parts[i])
		parts[i] = raw
	}
	if total > maxBackgroundSnapshotBytes {
		for _, i := range order {
			item := &snapshots[i].Item
			item.Title, _ = boundedBackgroundText(item.Title, 256)
			for _, field := range []**string{&item.Command, &item.Cwd, &item.Role, &item.Model} {
				if *field != nil {
					value, _ := boundedBackgroundText(**field, 512)
					*field = &value
				}
			}
			if len(item.OriginActivityIDs) > 8 {
				item.OriginActivityIDs = item.OriginActivityIDs[len(item.OriginActivityIDs)-8:]
			}
			mark(item)
			raw, err := json.Marshal(snapshots[i])
			if err != nil {
				return nil, err
			}
			total += len(raw) - len(parts[i])
			parts[i] = raw
			if total <= maxBackgroundSnapshotBytes {
				break
			}
		}
	}
	if total > maxBackgroundSnapshotBytes {
		return nil, errors.New("background identities exceed snapshot budget")
	}
	var result bytes.Buffer
	result.Grow(total)
	result.WriteByte('[')
	for i, part := range parts {
		if i > 0 {
			result.WriteByte(',')
		}
		result.Write(part)
	}
	result.WriteByte(']')
	return result.Bytes(), nil
}

// Package backgroundview projects a session's background work (subagents and
// detached processes) into the shape the task capsule renders. It owns one
// body of domain knowledge: folding live backgroundWork events, restoring the
// persisted snapshots a hydrated session carries, and building the synthetic
// transcript the capsule's surface displays. The daemon wire shapes live in
// internal/model; this package only interprets them for display.
package backgroundview

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"unicode/utf8"

	"fintwind-go/internal/model"
)

const (
	entryLimit = 200
	runLimit   = 32 * 1024
	tailLimit  = 16 * 1024
)

// ItemsMap returns the projection's item registry, keyed by "kind/providerId",
// creating it on first use.
func ItemsMap(ext map[string]any) map[string]any {
	items, _ := ext["backgroundItems"].(map[string]any)
	if items == nil {
		items = map[string]any{}
		ext["backgroundItems"] = items
	}
	return items
}

func transcriptsMap(ext map[string]any) map[string]any {
	transcripts, _ := ext["backgroundTranscripts"].(map[string]any)
	if transcripts == nil {
		transcripts = map[string]any{}
		ext["backgroundTranscripts"] = transcripts
	}
	return transcripts
}

func keyOf(value any) string {
	key, _ := value.(map[string]any)
	if key == nil {
		return ""
	}
	kind, _ := key["kind"].(string)
	provider, _ := key["providerId"].(string)
	if kind == "" || provider == "" {
		return ""
	}
	return kind + "/" + provider
}

// ApplyEvent folds one backgroundWork event into the projection. It handles
// the item and transcript events; the caller keeps the stop-request events,
// which drive UI state. It reports whether the event was recognized.
func ApplyEvent(ext map[string]any, payload json.RawMessage) bool {
	var decoded map[string]any
	if json.Unmarshal(payload, &decoded) != nil {
		return false
	}
	switch kind, _ := decoded["type"].(string); kind {
	case "upsert":
		items := ItemsMap(ext)
		if key := keyOf(decoded["key"]); key != "" {
			items[key] = decoded
		}
	case "reconcileProcesses", "reconcileLive":
		items := map[string]any{}
		if list, ok := decoded["items"].([]any); ok {
			for _, entry := range list {
				item, _ := entry.(map[string]any)
				if key := keyOf(item["key"]); key != "" {
					items[key] = item
				}
			}
		}
		ext["backgroundItems"] = items
	case "outputDelta":
		// Deltas for a key the provider never upserted are dropped, the way
		// the desktop's append_output does.
		key := keyOf(decoded["key"])
		item, _ := ItemsMap(ext)[key].(map[string]any)
		if item == nil {
			return true
		}
		delta, _ := decoded["delta"].(string)
		if delta == "" {
			return true
		}
		output, _ := item["output"].(string)
		if tail := boundedTail(output+delta, tailLimit); len(tail) != len(output)+len(delta) {
			item["output"] = tail
			item["outputTruncated"] = true
		} else {
			item["output"] = tail
		}
	case "transcript":
		applyTranscript(ext, decoded)
	default:
		return false
	}
	return true
}

// applyTranscript folds one nested transcript event (externally tagged inside
// the internally tagged backgroundWork payload).
func applyTranscript(ext map[string]any, decoded map[string]any) {
	var name string
	var body map[string]any
	for _, candidate := range []string{"started", "textDelta", "reasoningDelta", "activity", "snapshot", "finished"} {
		if value, ok := decoded[candidate].(map[string]any); ok {
			name, body = candidate, value
			break
		}
	}
	if name == "" {
		return
	}
	key := keyOf(body["key"])
	if key == "" {
		return
	}
	transcripts := transcriptsMap(ext)
	fold, _ := transcripts[key].(map[string]any)
	if fold == nil {
		fold = map[string]any{}
		transcripts[key] = fold
	}
	switch name {
	case "started":
		fold["entries"] = []any{}
		delete(fold, "prompt")
		if prompt, ok := body["prompt"].(string); ok && prompt != "" {
			fold["prompt"] = prompt
		}
	case "textDelta", "reasoningDelta":
		if delta, ok := body["delta"].(string); ok && delta != "" {
			fold["entries"] = appendRun(fold["entries"], name, delta)
		}
	case "activity":
		if activity, ok := body["activity"].(map[string]any); ok {
			fold["entries"] = appendEntry(fold["entries"], map[string]any{"activity": activity})
		}
	case "snapshot":
		// An authoritative snapshot replaces the accumulated runs; its own
		// message order is preserved as delivered.
		if transcript, ok := body["transcript"].(map[string]any); ok {
			fold["entries"] = []any{map[string]any{"snapshot": transcript}}
		}
	case "finished":
		if success, ok := body["success"].(bool); ok {
			fold["success"] = success
		}
	}
	fold["entries"] = capEntries(fold["entries"])
}

// Items decodes the projection into ordered items, sorted by start time so
// rows stay stable across frames.
func Items(ext map[string]any) []model.BackgroundWorkItem {
	raw, _ := ext["backgroundItems"].(map[string]any)
	items := make([]model.BackgroundWorkItem, 0, len(raw))
	for key, value := range raw {
		encoded, err := json.Marshal(value)
		if err != nil {
			continue
		}
		var item model.BackgroundWorkItem
		if json.Unmarshal(encoded, &item) != nil || item.Key.ProviderID == "" {
			continue
		}
		if item.Key.Kind == "" {
			if cut := strings.IndexByte(key, '/'); cut > 0 {
				item.Key.Kind = key[:cut]
			}
		}
		items = append(items, item)
	}
	sort.SliceStable(items, func(i, j int) bool {
		if items[i].StartedAtMs != items[j].StartedAtMs {
			return items[i].StartedAtMs < items[j].StartedAtMs
		}
		return items[i].Key.ProviderID < items[j].Key.ProviderID
	})
	return items
}

// Fold returns one item's transcript fold, or nil when nothing has arrived.
func Fold(ext map[string]any, key model.BackgroundWorkKey) map[string]any {
	transcripts, _ := ext["backgroundTranscripts"].(map[string]any)
	fold, _ := transcripts[key.Kind+"/"+key.ProviderID].(map[string]any)
	return fold
}

// FillSession projects one background fold into a synthetic session the main
// transcript builders can render. Message and activity IDs carry the key as a
// prefix, so their expansion state and markdown caches never collide with the
// selected session's.
func FillSession(syn *model.Session, prefix string, fold map[string]any, item *model.BackgroundWorkItem) {
	live := item != nil && item.Live()
	if prompt, ok := fold["prompt"].(string); ok && prompt != "" {
		syn.Messages = append(syn.Messages, model.Message{
			ID: prefix + "-prompt", Role: "user", Content: prompt, CreatedAt: item.StartedAtMs / 1000,
		})
	}
	entries, _ := fold["entries"].([]any)
	totalTextRuns := countTextRuns(entries)
	textRuns := 0
	for _, entryAny := range entries {
		entry, ok := entryAny.(map[string]any)
		if !ok {
			continue
		}
		if text, ok := entry["text"].(string); ok {
			textRuns++
			syn.Messages = append(syn.Messages, model.Message{
				ID:        fmt.Sprintf("%s-t%d", prefix, textRuns),
				Role:      "assistant",
				Content:   text,
				Streaming: live && textRuns == totalTextRuns,
				CreatedAt: item.UpdatedAtMs / 1000,
			})
			continue
		}
		if reasoning, ok := entry["reasoning"].(string); ok {
			syn.TranscriptBlocks = append(syn.TranscriptBlocks, model.TranscriptBlock{
				AfterMessage: max(0, len(syn.Messages)-1),
				Activities: []model.ActivityItem{{
					ID:   fmt.Sprintf("%s-r%d", prefix, len(syn.TranscriptBlocks)+1),
					Kind: model.KindReasoning, Title: "Reasoning", Complete: true,
					Reasoning: &model.ReasoningBlock{Content: reasoning},
				}},
			})
			continue
		}
		if activity, ok := entry["activity"].(map[string]any); ok {
			var activityItem model.ActivityItem
			if json.Unmarshal(marshalAny(activity), &activityItem) != nil {
				continue
			}
			if activityItem.ID != "" {
				activityItem.ID = prefix + "-a-" + activityItem.ID
			}
			syn.TranscriptBlocks = append(syn.TranscriptBlocks, model.TranscriptBlock{
				AfterMessage: max(0, len(syn.Messages)-1),
				Activities:   []model.ActivityItem{activityItem},
			})
			continue
		}
		if snapshot, ok := entry["snapshot"].(map[string]any); ok {
			var native model.NativeTranscript
			if json.Unmarshal(marshalAny(snapshot), &native) != nil {
				continue
			}
			base := len(syn.Messages)
			for index := range native.Messages {
				message := native.Messages[index]
				message.ID = fmt.Sprintf("%s-m%d", prefix, base+index)
				message.TurnID = turnRef(prefix, message.TurnID)
				syn.Messages = append(syn.Messages, message)
			}
			for _, block := range native.Blocks {
				for index := range block.Activities {
					if block.Activities[index].ID != "" {
						block.Activities[index].ID = prefix + "-a-" + block.Activities[index].ID
					}
				}
				block.TurnID = turnRef(prefix, block.TurnID)
				block.AfterMessage += base
				syn.TranscriptBlocks = append(syn.TranscriptBlocks, block)
			}
			for index := range native.Turns {
				turn := native.Turns[index]
				turn.ID = prefix + "-turn-" + turn.ID
				syn.Turns = append(syn.Turns, turn)
			}
		}
	}
	// A live run before its first snapshot still reads as a working turn, so
	// the tail row and the live cluster behave like the main panel's.
	if live && len(syn.Turns) == 0 {
		syn.Turns = append(syn.Turns, model.AgentTurn{
			ID:        prefix + "-live",
			Status:    "running",
			StartedAt: item.StartedAtMs / 1000,
		})
	}
	if live {
		syn.Status = "working"
	}
}

// turnRef namespaces one provider turn id under the surface's prefix, so the
// synthetic transcript's fold and footer state never collides with the
// selected session's.
func turnRef(prefix string, raw *string) *string {
	if raw == nil || *raw == "" {
		return nil
	}
	ref := prefix + "-turn-" + *raw
	return &ref
}

// countTextRuns precounts the text entries so the newest one can render as
// streaming while the item is live.
func countTextRuns(entries []any) int {
	count := 0
	for _, entryAny := range entries {
		if entry, ok := entryAny.(map[string]any); ok {
			if _, ok := entry["text"].(string); ok {
				count++
			}
		}
	}
	return count
}

// appendRun merges a delta into the trailing run of the same stream, so
// interleaved text, reasoning and activities keep their order.
func appendRun(entriesAny any, name, delta string) []any {
	entries, _ := entriesAny.([]any)
	kind := "text"
	if name == "reasoningDelta" {
		kind = "reasoning"
	}
	if len(entries) > 0 {
		if last, ok := entries[len(entries)-1].(map[string]any); ok {
			if run, ok := last[kind].(string); ok && len(run)+len(delta) <= runLimit {
				last[kind] = run + delta
				return entries
			}
		}
	}
	return appendEntry(entries, map[string]any{kind: delta})
}

func appendEntry(entriesAny any, entry map[string]any) []any {
	entries, _ := entriesAny.([]any)
	return append(entries, entry)
}

// capEntries bounds one transcript's history by dropping its oldest runs; the
// store is a projection, not the durable registry.
func capEntries(entriesAny any) []any {
	entries, _ := entriesAny.([]any)
	if len(entries) <= entryLimit {
		return entries
	}
	return append([]any(nil), entries[len(entries)-entryLimit:]...)
}

// boundedTail keeps the newest bytes of an output string without splitting a
// UTF-8 sequence.
func boundedTail(text string, limit int) string {
	if len(text) <= limit {
		return text
	}
	cut := len(text) - limit
	for cut < len(text) && !utf8.RuneStart(text[cut]) {
		cut++
	}
	return text[cut:]
}

func marshalAny(value any) []byte {
	encoded, err := json.Marshal(value)
	if err != nil {
		return nil
	}
	return encoded
}
package main

import (
	"encoding/json"
	"fmt"
	"strings"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

// The background work surface: the capsule's tasks tab opens one item here.
// A subagent renders as a full transcript through the same builders as the
// main session panel, over a synthetic session built from the fold the
// backgroundWork events keep; a process renders its command and output.

const backgroundSurfacePrefix = "background:"

func backgroundSurfaceKey(key model.BackgroundWorkKey) string {
	return backgroundSurfacePrefix + key.Kind + "/" + key.ProviderID
}

func backgroundKeyFromKind(kind string) (model.BackgroundWorkKey, bool) {
	if !strings.HasPrefix(kind, backgroundSurfacePrefix) {
		return model.BackgroundWorkKey{}, false
	}
	rest := kind[len(backgroundSurfacePrefix):]
	cut := strings.IndexByte(rest, '/')
	if cut <= 0 {
		return model.BackgroundWorkKey{}, false
	}
	return model.BackgroundWorkKey{Kind: rest[:cut], ProviderID: rest[cut+1:]}, true
}

func (a *app) openBackgroundSurface(key string) {
	a.rightOpen = true
	a.openSurface(backgroundSurfacePrefix + key)
}

// capsuleBackgroundItem finds one decoded item in the session's projection.
func (a *app) capsuleBackgroundItem(key model.BackgroundWorkKey) *model.BackgroundWorkItem {
	items := a.capsuleItemsFor()
	for index := range items {
		if items[index].Key == key {
			return &items[index]
		}
	}
	return nil
}

func (a *app) backgroundSurfaceTitle(kind string) string {
	key, ok := backgroundKeyFromKind(kind)
	if !ok {
		return tr("Background work")
	}
	if item := a.capsuleBackgroundItem(key); item != nil && item.Title != "" {
		return item.Title
	}
	if key.Kind == "subagent" {
		return tr("Subagent")
	}
	return tr("Process")
}

func backgroundSurfaceIcon(kind string) string {
	if key, ok := backgroundKeyFromKind(kind); ok && key.Kind != "subagent" {
		return "terminal-square"
	}
	return "bot"
}

// backgroundSurface renders the open item. The synthetic session rebuilds
// only after a fold or item change; rendering itself stays a read.
func (a *app) backgroundSurface(c *ui.Context, kind string) {
	key, ok := backgroundKeyFromKind(kind)
	if !ok {
		return
	}
	item := a.capsuleBackgroundItem(key)
	if a.capsule.AgentKey != kind {
		a.closeBackgroundSurface()
		a.capsule.AgentKey = kind
	}
	if a.capsule.AgentDirty || (a.capsule.AgentSession == nil && item != nil) {
		a.capsule.AgentDirty = false
		a.buildBackgroundSession(key, item)
	}
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		if item == nil {
			ui.Row(c).FillWidth().Padding(16, 12).Children(func() {
				ui.Text(c, tr("No background work")).FontSize(uiPx(12)).TextColor(palette.textTertiary)
			})
			return
		}
		ui.Row(c).FillWidth().Shrink(0).Height(36).Padding(0, 12).Gap(8).AlignItems(ui.Center).
			BorderWidth(0, 0, 1, 0).BorderColor(palette.border).Children(func() {
			icon(c, backgroundSurfaceIcon(kind), 14, palette.textSecondary)
			ui.Text(c, item.Title).FontSize(uiPx(12.5)).TextColor(palette.text).
				Grow(1).MinWidth(0).SingleLine().Ellipsis("…")
			ui.Text(c, backgroundStatusLabel(*item)).FontSize(uiPx(11)).TextColor(palette.textTertiary)
		})
		if item.Key.Kind == "subagent" {
			if a.capsule.AgentSession == nil || len(a.capsule.AgentRows) == 0 {
				ui.Row(c).FillWidth().Padding(16, 12).Children(func() {
					ui.Text(c, tr("Waiting for the agent's first output")).FontSize(uiPx(12)).
						TextColor(palette.textTertiary)
				})
				return
			}
			ui.List(c, &a.capsule.AgentList, len(a.capsule.AgentRows), func(index int) {
				a.capsuleAgentRowView(c, index)
			}).FillWidth().Grow(1)
			return
		}
		a.backgroundProcessBody(c, item)
	})
}

// backgroundProcessBody is a process surface: the command over its output.
func (a *app) backgroundProcessBody(c *ui.Context, item *model.BackgroundWorkItem) {
	ui.Scroll(c).FillWidth().Grow(1).Children(func() {
		ui.Column(c).FillWidth().Padding(12).Gap(8).Children(func() {
			if item.Command != nil && *item.Command != "" {
				ui.Row(c).FillWidth().Children(func() {
					ui.Text(c, *item.Command).Font(monoFont()).FontSize(uiPx(11)).
						TextColor(palette.textSecondary).Wrap().Grow(1).MinWidth(0)
				})
			}
			output := ""
			if item.Output != nil {
				output = *item.Output
			}
			if output == "" {
				ui.Text(c, tr("No output yet")).FontSize(uiPx(12)).TextColor(palette.textTertiary)
				return
			}
			if item.Live() {
				c.AnimationFrame()
			}
			ui.Text(c, output).Font(monoFont()).FontSize(uiPx(11)).
				TextColor(palette.text).Wrap().Grow(1).MinWidth(0)
			if item.OutputTruncated {
				ui.Text(c, tr("Earlier output was dropped")).FontSize(uiPx(10.5)).
					TextColor(palette.textTertiary)
			}
		})
	})
}

func backgroundStatusLabel(item model.BackgroundWorkItem) string {
	switch item.Status {
	case "starting":
		return tr("Starting")
	case "running", "monitoring":
		return tr("Running")
	case "stopping":
		return tr("Stopping")
	case "stopped":
		return tr("Stopped")
	case "completed":
		return tr("Completed")
	case "failed":
		return tr("Failed")
	case "lost":
		return tr("Lost")
	}
	return item.Status
}

// capsuleAgentRowView is the synthetic transcript's row dispatch, mirroring
// transcriptRowView: messages and activity blocks only, the shapes a
// background fold holds.
func (a *app) capsuleAgentRowView(c *ui.Context, index int) {
	if index < 0 || index >= len(a.capsule.AgentRows) || a.capsule.AgentSession == nil {
		return
	}
	session := a.capsule.AgentSession
	row := a.capsule.AgentRows[index]
	count := len(a.capsule.AgentRows)
	top := float32(8)
	bottom := float32(8)
	if index == 0 {
		top = 14
	}
	if row.Kind == transcriptMessage && messageStartsFollowupTurn(session.Messages, row.MessageIndex) {
		top = 28
	}
	if index+1 == count {
		bottom = 14
	}
	ui.Row(c).FillWidth().Justify(ui.Center).Padding(top, 12, bottom, 12).Children(func() {
		ui.Column(c).FillWidth().MinWidth(0).Children(func() {
			switch row.Kind {
			case transcriptMessage:
				if row.MessageIndex >= 0 && row.MessageIndex < len(session.Messages) {
					a.messageView(c, session, row.MessageIndex)
				}
			case transcriptBlock:
				if row.BlockIndex < 0 || row.BlockIndex >= len(session.TranscriptBlocks) {
					return
				}
				latest := row.BlockIndex+1 == len(session.TranscriptBlocks)
				a.activityCluster(c, session, a.capsule.AgentDisclosure, row.BlockIndex, false, latest)
			}
		})
	})
}

// buildBackgroundSession projects one background fold into a synthetic
// session the main transcript builders can render. Message and activity IDs
// carry the key as a prefix, so their expansion state and markdown caches
// never collide with the selected session's.
func (a *app) buildBackgroundSession(key model.BackgroundWorkKey, item *model.BackgroundWorkItem) {
	a.capsule.AgentDisclosure = map[int]int{}
	a.capsule.AgentRows = nil
	a.capsule.AgentSession = nil
	if item == nil {
		return
	}
	prefix := "bg-" + key.Kind + "-" + key.ProviderID
	syn := &model.Session{ID: prefix, Title: item.Title, Status: "idle"}
	if item.Model != nil {
		name := *item.Model
		syn.Model = &name
	}
	if item.Key.Kind == "subagent" && a.detail != nil {
		fold := backgroundFold(a.extension(a.detail), key)
		a.fillBackgroundSession(syn, prefix, fold, item)
	}
	a.capsule.AgentSession = syn
	a.capsule.AgentRows = buildTranscriptRows(syn, nil)
}

func backgroundFold(ext map[string]any, key model.BackgroundWorkKey) map[string]any {
	transcripts, _ := ext["backgroundTranscripts"].(map[string]any)
	fold, _ := transcripts[key.Kind+"/"+key.ProviderID].(map[string]any)
	return fold
}

func (a *app) fillBackgroundSession(syn *model.Session, prefix string, fold map[string]any, item *model.BackgroundWorkItem) {
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
				syn.Messages = append(syn.Messages, message)
			}
			for _, block := range native.Blocks {
				for index := range block.Activities {
					if block.Activities[index].ID != "" {
						block.Activities[index].ID = prefix + "-a-" + block.Activities[index].ID
					}
				}
				block.AfterMessage += base
				syn.TranscriptBlocks = append(syn.TranscriptBlocks, block)
			}
		}
	}
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

func marshalAny(value any) []byte {
	encoded, err := json.Marshal(value)
	if err != nil {
		return nil
	}
	return encoded
}

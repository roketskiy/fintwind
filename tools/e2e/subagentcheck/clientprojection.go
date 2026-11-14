package main

import (
	"encoding/json"
	"fmt"

	"fintwind-go/internal/backgroundview"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
)

// The daemon checks above prove the child roster and transcripts are durable.
// These checks drive the app's actual capsule projection
// (internal/backgroundview) over that same daemon output, because a persisted
// transcript the client cannot render is still an empty capsule to the user.

// projectChild renders one subagent through the capsule's synthetic-session
// builder and returns its message, block and activity counts.
func projectChild(ext map[string]any, providerID string) (messages, blocks, activities int, found bool) {
	for _, item := range backgroundview.Items(ext) {
		if item.Key.Kind != "subagent" || item.Key.ProviderID != providerID {
			continue
		}
		found = true
		prefix := "bg-" + item.Key.Kind + "-" + item.Key.ProviderID
		syn := &model.Session{ID: prefix, Title: item.Title, Status: "idle"}
		backgroundview.FillSession(syn, prefix, backgroundview.Fold(ext, item.Key), &item)
		messages = len(syn.Messages)
		blocks = len(syn.TranscriptBlocks)
		for _, b := range syn.TranscriptBlocks {
			activities += len(b.Activities)
		}
		return
	}
	return
}

// verifyClientLiveProjection replays the live backgroundWork events through the
// app's fold and asserts the child's conversation renders with its activity
// blocks, the way the capsule's surface shows a running subagent.
func verifyClientLiveProjection(events []daemon.Event, res *results) error {
	ext := map[string]any{}
	for _, ev := range events {
		if ev.Kind != "backgroundWork" {
			continue
		}
		backgroundview.ApplyEvent(ext, ev.Payload)
	}
	messages, blocks, activities, found := projectChild(ext, childOne)
	if !found {
		return fmt.Errorf("live projection lost child %s", childOne)
	}
	if messages == 0 || blocks == 0 || activities == 0 {
		return fmt.Errorf("live projection rendered no child conversation: messages=%d blocks=%d activities=%d", messages, blocks, activities)
	}
	res.add("client-live-projection-renders-child-conversation", nil)
	return nil
}

// verifyClientRestore seeds the projection from the persisted background_work
// exactly as a reopened session does, with no live runtime, and asserts the
// child's conversation survives the restart.
func verifyClientRestore(state map[string]json.RawMessage, res *results) error {
	ext := map[string]any{}
	if !backgroundview.Restore(ext, state["background_work"]) {
		return fmt.Errorf("restore did not seed the projection from persisted background_work")
	}
	messages, blocks, _, found := projectChild(ext, childOne)
	if !found {
		return fmt.Errorf("restored projection lost child %s", childOne)
	}
	if messages == 0 || blocks == 0 {
		return fmt.Errorf("restored projection rendered no child conversation: messages=%d blocks=%d", messages, blocks)
	}
	res.add("client-restore-renders-child-conversation-after-restart", nil)
	return nil
}
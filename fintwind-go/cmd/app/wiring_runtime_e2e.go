//go:build wiringe2e

package main

import (
	"context"
	"encoding/json"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
)

func wiringRuntimePreferences(ctx context.Context, r *wiringReport, client *daemon.Client, root string, f *wiringFixture) {
	settingsApp := wiringApp(ctx, client, root)
	settingsApp.saveDaemonSetting("auto_compact_enabled", false)
	wiringWait(ctx, r, f, "updateSetting")
	r.check("settings UI uses atomic single-key command", 1, f.count("updateSetting"))
	r.check("settings UI never falls back to full replacement", 0, f.count("updateSettings"))
	a := wiringApp(ctx, client, root)
	a.connection.phase = "online"
	a.settingsRaw = map[string]any{"auto_compact_enabled": true, "auto_compact_threshold_tokens": float64(10000)}
	a.settings.preferences.values = map[string]any{"busy_send_mode": "steer", "mermaid_visualization_enabled": true}
	a.detail = &model.Session{ID: "fixture-session", ProjectID: "fixture-project", Status: "working"}
	usage := func(tokens uint64) {
		data, _ := json.Marshal(map[string]any{"contextTokens": tokens})
		err := a.applyEvent(daemon.Event{SessionID: "fixture-session", RuntimeID: "fixture-runtime", Kind: "usageUpdated", Payload: data})
		if err != nil {
			r.check("usage event applied", "success", err.Error())
		}
	}
	usage(15000)
	wiringWait(ctx, r, f, "compactSession")
	usage(16000)
	r.check("one compaction for one threshold crossing", 1, f.count("compactSession"))
	usage(9000)
	usage(17000)
	wiringWait(ctx, r, f, "compactSession")
	r.check("falling below threshold rearms compaction", 2, f.count("compactSession"))
	a.settingsRaw["auto_compact_enabled"] = false
	usage(18000)
	r.check("disabled compaction emits no new request", 2, f.count("compactSession"))
	a.connection.phase = "offline"
	accepted := a.submitSessionSubmission(sessionSubmission{sessionID: "fixture-session", text: "Fixture steer only"})
	wiringWait(ctx, r, f, "steer")
	r.check("busy send uses steer transport", true, accepted && len(a.pendingSteerList(a.detail)) == 1)
	r.check("steer keeps durable queued text until echo", "Fixture steer only", a.detail.QueuedMessages[0].Content)
	r.check("new session freezes Mermaid from preference", "v4", a.sessionMermaidVersion(a.detail))
	a.stateForSession(a.detail).raw = map[string]any{"mermaid_prompt_version": nil}
	r.check("frozen disabled Mermaid is not silently enabled", "", a.sessionMermaidVersion(a.detail))
	requests, _ := f.snapshot()
	for _, request := range requests {
		if request.Command.Type == "steer" {
			r.check("steer wire text", "Fixture steer only", request.Command.Prompt)
		}
	}
}

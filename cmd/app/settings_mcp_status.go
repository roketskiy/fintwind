package main

import (
	"context"
	"errors"
	"time"
)

// All lifecycle fields belong to the UI thread. Cancellation stops the worker;
// generation checks also reject callbacks already queued before cancellation.
func (a *app) cancelSettingsMcpStatus() {
	st := &a.settings.backend
	if st.mcpStatusCancel != nil {
		st.mcpStatusCancel()
		st.mcpStatusCancel = nil
	}
	st.mcpStatusGeneration++
	st.mcpStatusLoading = false
}

func (a *app) refreshSettingsMcpStatuses() {
	a.cancelSettingsMcpStatus()
	st := &a.settings.backend
	if !st.mcp.loaded || st.mcp.writing || len(st.mcpServers) == 0 {
		return
	}
	source, generation, statusGeneration := st.source, st.generation, st.mcpStatusGeneration
	binary := a.settingsStr("opencode_binary_path", "opencode")
	ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
	st.mcpStatusCancel, st.mcpStatusLoading, st.mcpStatusError = cancel, true, ""
	go func() {
		defer cancel()
		// Unlike onUI, this wait is also bounded by this refresh's lifetime.
		apply := func(update func() bool) bool {
			result := make(chan bool, 1)
			a.post(func() {
				if ctx.Err() != nil || !a.settingsSourceMatches(source, generation) || a.settings.backend.mcpStatusGeneration != statusGeneration {
					result <- false
					return
				}
				result <- update()
			})
			select {
			case keep := <-result:
				return keep
			case <-ctx.Done():
				return false
			}
		}
		defer func() {
			a.post(func() {
				if !a.settingsSourceMatches(source, generation) || a.settings.backend.mcpStatusGeneration != statusGeneration {
					return
				}
				st := &a.settings.backend
				st.mcpStatusLoading, st.mcpStatusCancel = false, nil
				if errors.Is(ctx.Err(), context.DeadlineExceeded) {
					st.mcpStatusError = tr("MCP connection status refresh timed out.")
				}
			})
		}()
		for {
			if !apply(func() bool {
				return !a.settings.backend.mcp.writing && a.mode == modeSettings && (a.settings.page == "mcpServers" || a.settings.page == "mcpMarket")
			}) {
				return
			}
			var body struct {
				Statuses []settingsMcpStatus `json:"statuses"`
			}
			err := settingsRequest(ctx, source, map[string]any{"type": "listMcpServerStatuses", "binary": binary, "directory": source.directory}, "mcpServerStatuses", &body)
			if err == nil && body.Statuses == nil {
				err = errors.New("MCP connection status list is missing from the response")
			}
			if !apply(func() bool {
				st := &a.settings.backend
				if err != nil {
					st.mcpStatusError = tr("Connection status unavailable. ") + err.Error()
					return false
				}
				st.mcpStatuses, st.mcpStatusError = body.Statuses, ""
				// Missing enabled servers may not have registered with OpenCode yet.
				for _, server := range st.mcpServers {
					if !server.Enabled {
						continue
					}
					known := false
					for _, status := range body.Statuses {
						if status.Name == server.Name {
							known = status.Status != "pending"
							break
						}
					}
					if !known {
						return true
					}
				}
				return false
			}) {
				return
			}
			timer := time.NewTimer(2 * time.Second)
			select {
			case <-timer.C:
			case <-ctx.Done():
				timer.Stop()
				return
			}
		}
	}()
}

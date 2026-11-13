package main

import "fintwind-go/internal/model"

func (a *app) maybeAutoCompact(session *model.Session) {
	if session == nil || session.ContextUsage == nil {
		return
	}
	prefs := a.runtimePreferences()
	if !prefs.AutoCompact || session.ContextUsage.Tokens < prefs.AutoCompactThreshold {
		delete(a.autoCompacted, session.ID)
		return
	}
	if a.selectedID != session.ID || a.detail != session || a.client == nil || a.lifetime == nil || a.connection.phase != "online" || a.sessionActionBlocksInput(session.ID) {
		return
	}
	runtimeID := a.runtimes[session.ID]
	if runtimeID == "" || a.autoCompacted[session.ID] || (session.Compaction != nil && session.Compaction.Status == "running") {
		return
	}
	if request := a.currentContextCompact(); request != nil && request.pending {
		return
	}
	if a.autoCompacted == nil {
		a.autoCompacted = make(map[string]bool)
	}
	a.autoCompacted[session.ID] = true
	a.requestContextCompaction()
}

package main

import "fintwind-go/internal/model"

func (a *app) sessionMermaidVersion(session *model.Session) string {
	raw := a.stateForSession(session).raw
	if a.detail == session && a.detailRaw != nil {
		raw = a.detailRaw
	}
	if value, exists := raw["mermaid_prompt_version"]; exists {
		version, _ := value.(string)
		return version
	}
	if a.runtimePreferences().MermaidVisualization {
		return "v4"
	}
	return ""
}

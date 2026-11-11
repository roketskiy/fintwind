package main

import (
	"encoding/json"
	"strconv"
	"strings"
)

const contextModelPreferencesKey = "context_model_preferences"

func contextWindowSelection(selected *string, model *providerModel) (string, string) {
	for _, candidate := range []*string{selected, model.DefaultContextWindow} {
		if candidate == nil {
			continue
		}
		for _, option := range model.ContextWindows {
			if option.ID == *candidate {
				return option.ID, option.Label
			}
		}
	}
	if len(model.ContextWindows) > 0 {
		return model.ContextWindows[0].ID, model.ContextWindows[0].Label
	}
	return "", ""
}

func (a *app) configuredContextWindow() uint64 {
	if a.detail == nil || a.detail.Model == nil || a.settings.backend.source != a.currentSettingsSource() {
		return 0
	}
	providerID, modelID, ok := strings.Cut(*a.detail.Model, "/")
	if !ok {
		return 0
	}
	for _, provider := range a.settings.backend.providers {
		if !strings.EqualFold(provider.ID, providerID) {
			continue
		}
		models, _ := provider.Entry["models"].(map[string]any)
		for id, value := range models {
			if !strings.EqualFold(id, modelID) {
				continue
			}
			entry, _ := value.(map[string]any)
			limit, _ := entry["limit"].(map[string]any)
			switch value := limit["context"].(type) {
			case json.Number:
				window, _ := strconv.ParseUint(string(value), 10, 64)
				return window
			case float64:
				if value > 0 && value < 18446744073709551616.0 {
					return uint64(value)
				}
			}
		}
	}
	return 0
}

func (a *app) ensureContextSources() {
	a.syncSettingsSource()
	if a.client != nil && !a.settings.backend.provider.loaded {
		a.loadSettingsProviders()
	}
	a.requestModelProbe()
	a.reconcileContextWindow()
}

func (a *app) chooseContextWindow(value string) {
	if a.detail == nil || a.sessionActionBlocksInput(a.detail.ID) {
		return
	}
	model := a.currentProviderModel()
	if model == nil {
		return
	}
	for _, option := range model.ContextWindows {
		if option.ID == value {
			a.detail.ContextWindow = &value
			a.rememberContextWindow()
			a.applySessionOptions()
			return
		}
	}
}

func (a *app) rememberedContextWindow(modelID string) *string {
	prefs, _ := a.settings.preferences.values[contextModelPreferencesKey].(map[string]any)
	windows, _ := prefs["windows"].(map[string]any)
	if window, ok := windows[modelID].(string); ok && window != "" {
		if a.modelProbe != nil {
			for _, model := range a.modelProbe.Models {
				if model.ID == modelID {
					if len(model.ContextWindows) == 0 {
						return nil
					}
					value, _ := contextWindowSelection(&window, &model)
					return &value
				}
			}
			return nil
		}
		return &window
	}
	return nil
}

func (a *app) reconcileContextWindow() {
	if a.modelProbe == nil || a.detail == nil || a.detail.ContextWindow == nil || a.sessionActionBlocksInput(a.detail.ID) {
		return
	}
	model := a.currentProviderModel()
	if model != nil {
		for _, option := range model.ContextWindows {
			if option.ID == *a.detail.ContextWindow {
				return
			}
		}
	}
	a.detail.ContextWindow = nil
	if model != nil && len(model.ContextWindows) > 0 {
		value, _ := contextWindowSelection(nil, model)
		a.detail.ContextWindow = &value
	}
	a.rememberContextWindow()
	a.applySessionOptions()
}

func (a *app) rememberContextWindow() {
	if a.detail == nil {
		return
	}
	modelID := ""
	if a.detail.Model != nil {
		modelID = *a.detail.Model
	}
	prefs, _ := a.settings.preferences.values[contextModelPreferencesKey].(map[string]any)
	previous, _ := prefs["windows"].(map[string]any)
	windows := make(map[string]any, len(previous)+1)
	for id, window := range previous {
		windows[id] = window
	}
	if modelID != "" && a.detail.ContextWindow != nil {
		windows[modelID] = *a.detail.ContextWindow
	} else if modelID != "" {
		delete(windows, modelID)
	}
	a.savePreference(contextModelPreferencesKey, map[string]any{"lastModel": modelID, "windows": windows})
}

func (a *app) restoreDraftContextWindow() {
	if a.detail == nil {
		return
	}
	prefs, _ := a.settings.preferences.values[contextModelPreferencesKey].(map[string]any)
	if modelID, ok := prefs["lastModel"].(string); ok && modelID != "" {
		a.detail.Model = &modelID
		a.detail.ContextWindow = a.rememberedContextWindow(modelID)
	}
}

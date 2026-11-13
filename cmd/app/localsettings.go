package main

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
)

type layoutDefaults struct {
	sidebar, rightPanel, fileTree float32
	preferences                   appPreferences
}

func loadLocalSettings(dataDir string) layoutDefaults {
	return loadLocalSettingsFrom(dataDir, dataDir)
}

func loadLocalSettingsFrom(dataDir, preferencesDir string) layoutDefaults {
	defaults := layoutDefaults{sidebar: sidebarDefault, rightPanel: rightPanelWidth, fileTree: defaultFileTreeWidth}
	if dataDir == "" {
		return defaults
	}
	loadRustLayout(dataDir, &defaults)
	prefs := appPreferences{path: filepath.Join(preferencesDir, "app-settings.json"), values: map[string]any{}}
	data, err := os.ReadFile(prefs.path)
	if err == nil {
		err = json.Unmarshal(data, &prefs.values)
		if err == nil && prefs.values == nil {
			err = errors.New("Settings must be a JSON object")
		}
	}
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		prefs.loadError = "Local settings could not be read; the file will not be overwritten"
	}
	if prefs.values == nil {
		prefs.values = map[string]any{}
	}
	if width, ok := prefs.values[settingsKeyTranscriptWidth].(float64); ok {
		transcriptWidth = float32(normalizeTranscriptWidth(width))
	}
	defaults.preferences = prefs
	return defaults
}

func loadRustLayout(dataDir string, defaults *layoutDefaults) {
	paths := []string{filepath.Join(dataDir, "state.json")}
	for _, path := range paths {
		data, err := os.ReadFile(path)
		if err != nil {
			continue
		}
		var state struct {
			SidebarWidth    float32 `json:"sidebar_width"`
			RightPanelWidth float32 `json:"right_panel_width"`
			TranscriptWidth float32 `json:"transcript_width"`
		}
		if json.Unmarshal(data, &state) != nil {
			continue
		}
		if state.SidebarWidth >= sidebarMinWidth && state.SidebarWidth <= sidebarMaxWidth {
			defaults.sidebar = state.SidebarWidth
		}
		if state.RightPanelWidth >= rightPanelMinWidth && state.RightPanelWidth <= rightPanelMaxWidth {
			defaults.rightPanel = state.RightPanelWidth
		}
		if state.TranscriptWidth >= 560 && state.TranscriptWidth <= 1200 {
			transcriptWidth = float32(normalizeTranscriptWidth(float64(state.TranscriptWidth)))
		}
		return
	}
}

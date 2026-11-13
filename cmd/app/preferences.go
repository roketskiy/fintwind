package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
)

type appPreferences struct {
	path         string
	values       map[string]any
	loadError    string
	writes       chan preferenceWrite
	pending      uint64
	awake        *preferenceAwake
	runtimeError string
	writer       *preferenceWriterResult
}

type preferenceWriterResult struct {
	done chan struct{}
	err  error
}

type preferenceWrite struct {
	key      string
	value    any
	sequence uint64
	changes  map[string]any
}

// initPreferences runs once after loadLocalSettings and before the first view.
func (a *app) initPreferences(layout layoutDefaults) {
	a.settings.preferences = layout.preferences
	prefs := &a.settings.preferences
	if prefs.values == nil {
		prefs.values = map[string]any{}
	}
	prefs.writes = make(chan preferenceWrite, 64)
	result := &preferenceWriterResult{done: make(chan struct{})}
	prefs.writer = result
	path, loadError, writes := prefs.path, prefs.loadError, prefs.writes
	go func() {
		defer close(result.done)
		for write := range writes {
			err := writePreferenceFile(path, loadError, write)
			result.err = err
			a.post(func() {
				if write.sequence != a.settings.preferences.pending {
					return
				}
				if err != nil {
					a.settings.message = tr("Not saved. ") + err.Error()
				} else {
					a.settings.message = tr("Saved")
				}
			})
		}
	}()
	if value, ok := prefs.values["keep_screen_awake"].(bool); ok && value {
		a.setPreferenceAwake(true)
	}
	a.settings.keepAwake, _ = prefs.values["keep_screen_awake"].(bool)
	a.settings.mermaidVisual = settingsBool(prefs.values, "mermaid_visualization_enabled", false)
	a.settings.termCopySelect = settingsBool(prefs.values, settingsKeyTerminalCopyOnSelect, false)
	a.settings.termEnv, _ = prefs.values[settingsKeyTerminalEnv].(string)
	a.settings.busySendSteer = a.preferenceString("busy_send_mode", "queue") == "steer"
	SetLanguage(a.preferenceString(settingsKeyLanguage, languageValueSystem))
	if prefs.loadError != "" {
		a.settings.message = prefs.loadError
	}
}

func writePreferenceFile(path, loadError string, write preferenceWrite) error {
	if path == "" {
		return errors.New("Local settings location is unavailable")
	}
	if loadError != "" {
		return errors.New(loadError)
	}
	values := map[string]json.RawMessage{}
	data, err := os.ReadFile(path)
	if err == nil {
		if err = json.Unmarshal(data, &values); err != nil || values == nil {
			return errors.New("Local settings are invalid; the file was not changed")
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return errors.New("Could not read local settings")
	}
	changes := write.changes
	if changes == nil {
		changes = map[string]any{write.key: write.value}
	}
	for key, value := range changes {
		if !localPreferenceKey(key) {
			return errors.New("Unsupported local preference")
		}
		encoded, encodeErr := json.Marshal(value)
		if encodeErr != nil {
			return encodeErr
		}
		values[key] = encoded
	}
	data, err = json.MarshalIndent(values, "", "  ")
	if err != nil {
		return err
	}
	if err = os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return errors.New("Could not create local settings directory")
	}
	temporary, err := os.CreateTemp(filepath.Dir(path), "app-settings-*.tmp")
	if err != nil {
		return errors.New("Could not create settings temporary file")
	}
	name := temporary.Name()
	defer os.Remove(name)
	if _, err = temporary.Write(data); err == nil {
		err = temporary.Sync()
	}
	closeErr := temporary.Close()
	if err == nil {
		err = closeErr
	}
	if err != nil {
		return errors.New("Could not write local settings")
	}
	if err = os.Rename(name, path); err != nil {
		return errors.New("Could not replace local settings")
	}
	return nil
}

func (a *app) savePreference(key string, value any) {
	prefs := &a.settings.preferences
	if prefs.writes == nil || prefs.loadError != "" {
		a.settings.message = tr("Not saved. Local preferences are unavailable")
		return
	}
	prefs.pending++
	changes := map[string]any{}
	for currentKey, currentValue := range prefs.values {
		if localPreferenceKey(currentKey) {
			changes[currentKey] = currentValue
		}
	}
	changes[key] = value
	write := preferenceWrite{key: key, value: value, sequence: prefs.pending, changes: changes}
	select {
	case prefs.writes <- write:
		prefs.values[key] = value
		a.settings.message = tr("Saving...")
		a.mdReady = false
		if key == settingsKeyTranscriptWidth {
			if width, ok := value.(float64); ok {
				transcriptWidth = float32(normalizeTranscriptWidth(width))
			}
		}
		if key == settingsKeyLanguage {
			if language, ok := value.(string); ok {
				SetLanguage(language)
				a.closeSettingsMenus()
			}
		}
		switch key {
		case settingsKeyUIFontFamily, settingsKeyCodeFontFamily, settingsKeyUITextScale, settingsKeyCodeTextScale:
			// Rust applies font and size picks immediately; without this
			// the minute appearance watcher would leave the UI stale.
			a.applyAppearance()
		case settingsKeyTerminalCopyOnSelect:
			// Copy-on-select has a live setter; shell/env/right-click and
			// scrollback take on new tabs.
			a.retuneNativeTerminals()
		}
		if key == "keep_screen_awake" {
			if enabled, ok := value.(bool); ok {
				a.setPreferenceAwake(enabled)
			}
		}
	default:
		a.settings.message = tr("Not saved. Settings writer is busy")
	}
}

func localPreferenceKey(key string) bool {
	if key == contextModelPreferencesKey {
		return true
	}
	switch key {
	case settingsKeyTheme, settingsKeyThemeScheme, settingsKeyLanguage, settingsKeyUITextScale, settingsKeyCodeTextScale, settingsKeyUIFontFamily, settingsKeyCodeFontFamily, settingsKeyTranscriptWidth, settingsKeyTerminalShell, settingsKeyTerminalEnv, settingsKeyTerminalRightClick, settingsKeyTerminalCopyOnSelect, settingsKeyTerminalScrollback, "busy_send_mode", "mermaid_visualization_enabled", "keep_screen_awake":
		return true
	}
	return false
}

func (a *app) preferenceString(key, fallback string) string {
	value, ok := a.settings.preferences.values[key].(string)
	if !ok {
		return fallback
	}
	return value
}

func (a *app) preferenceScale(key string) float32 {
	value, ok := a.settings.preferences.values[key].(float64)
	if !ok || value < 0.9 || value > 1.5 {
		return 1
	}
	return float32(value)
}

func (a *app) preferenceUIFont() string {
	font := a.preferenceString(settingsKeyUIFontFamily, uiFontFamily)
	if font == defaultUIFontFamily || font == "" {
		return uiFontFamily
	}
	return font
}

func (a *app) preferenceCodeFont() string {
	font := a.preferenceString(settingsKeyCodeFontFamily, monoFontFamily)
	if font == systemMonoFontFamily {
		return "Consolas"
	}
	if font == "" {
		return monoFontFamily
	}
	return font
}

func (a *app) closePreferences() {
	if writes := a.settings.preferences.writes; writes != nil {
		a.settings.preferences.writes = nil
		close(writes)
	}
	if awake := a.settings.preferences.awake; awake != nil {
		awake.cancel()
		a.settings.preferences.awake = nil
	}
	a.cancelSettingsOAuth()
}

// Call after the application loop exits, never from a rendering callback.
func (a *app) flushPreferences(ctx context.Context) error {
	writer := a.settings.preferences.writer
	if writer == nil {
		return nil
	}
	select {
	case <-writer.done:
		return writer.err
	case <-ctx.Done():
		return ctx.Err()
	}
}

type preferenceAwake struct{ cancel context.CancelFunc }

func (a *app) setPreferenceAwake(enabled bool) {
	prefs := &a.settings.preferences
	prefs.runtimeError = ""
	if prefs.awake != nil {
		prefs.awake.cancel()
		prefs.awake = nil
	}
	if !enabled {
		return
	}
	ctx, cancel := context.WithCancel(a.lifetime)
	awake := &preferenceAwake{cancel: cancel}
	prefs.awake = awake
	go runPreferenceAwake(ctx, func(err error) {
		a.post(func() {
			if a.settings.preferences.awake != awake {
				return
			}
			if err != nil {
				a.settings.preferences.runtimeError = "Keep awake unavailable. " + err.Error()
			}
		})
	})
}

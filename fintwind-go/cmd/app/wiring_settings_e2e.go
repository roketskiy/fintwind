//go:build wiringe2e

package main

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"time"

	"github.com/egoist/mygo/ui"
)

func wiringPreferences(ctx context.Context, r *wiringReport, round string) {
	oldPalette, oldAppearance, oldWidth := palette, activeAppearance, transcriptWidth
	defer func() { palette, activeAppearance, transcriptWidth = oldPalette, oldAppearance, oldWidth }()
	directory := filepath.Join(round, "preferences")
	if err := os.MkdirAll(directory, 0o700); err != nil {
		r.check("create isolated preferences", "success", err.Error())
		return
	}
	path := filepath.Join(directory, "app-settings.json")
	seed := []byte(`{"future":{"number":9007199254740993},"theme":"dark","theme_scheme":"default","language":"english"}`)
	if err := os.WriteFile(path, seed, 0o600); err != nil {
		r.check("seed isolated preferences", "success", err.Error())
		return
	}
	lifetime, cancel := context.WithCancel(ctx)
	defer cancel()
	a := &app{lifetime: lifetime, done: make(chan struct{})}
	a.initPreferences(loadLocalSettings(directory))
	defer a.closePreferences()
	var zoom float32
	var markdownFonts []string
	tt := ui.NewTester(func(c *ui.Context) {
		a.applyAppearance()
		c.SetZoom(activeAppearance.textScale)
		c.SetTheme(uiText(c))
		zoom = c.Zoom()
		md := newMarkdownOptions(c, 13.5, 21, 11.5, 17.5)
		markdownFonts = []string{md.Theme.Font, md.Theme.CodeFont}
		a.appearanceSettings(c)
	}, 1200, 1500)
	r.check("saved dark preference applies to actual palette", ui.RGB(34, 34, 34), palette.canvas)
	wiringChoice(r, tt, "Theme", 1)
	var saved map[string]json.RawMessage
	read := func() bool {
		data, err := os.ReadFile(path)
		if err != nil || json.Unmarshal(data, &saved) != nil {
			return false
		}
		return string(saved["theme"]) == `"light"`
	}
	r.check("theme keyboard choice reaches local file", true, wiringWaitPreference(ctx, read))
	r.check("light preference applies to actual palette", ui.RGB(247, 247, 247), palette.canvas)
	r.check("unknown large integer survives preference write", `{"number":9007199254740993}`, wiringCompact(saved["future"]))
	a.savePreference(settingsKeyUIFontFamily, "Arial")
	a.savePreference(settingsKeyCodeFontFamily, "Consolas")
	a.savePreference(settingsKeyUITextScale, float64(1.25))
	a.savePreference(settingsKeyCodeTextScale, float64(1.5))
	a.savePreference(settingsKeyTranscriptWidth, float64(960))
	r.check("serialized preferences preserve consecutive changes", true, wiringWaitPreference(ctx, func() bool {
		data, err := os.ReadFile(path)
		if err != nil || json.Unmarshal(data, &saved) != nil {
			return false
		}
		return string(saved[settingsKeyTranscriptWidth]) == "960" && string(saved[settingsKeyUIFontFamily]) == `"Arial"` && string(saved[settingsKeyCodeFontFamily]) == `"Consolas"`
	}))
	tt.Frame()
	r.check("interface scale drives native zoom", float32(1.25), zoom)
	r.check("interface and code fonts consumed", []string{"Arial", "Consolas"}, []string{activeAppearance.font, monoFont()})
	r.check("independent code scale compensates interface zoom", float32(14.4), codeMetric(12))
	r.check("transcript width is applied", float32(960), transcriptWidth)
	r.check("markdown consumes chosen families", []string{"Arial", "Consolas"}, markdownFonts)
	restored := loadLocalSettings(directory)
	r.check("preferences restore from actual saved file", "Arial", restored.preferences.values[settingsKeyUIFontFamily])
	bad := filepath.Join(round, "invalid-preferences.json")
	_ = os.WriteFile(bad, []byte("{invalid"), 0o600)
	err := writePreferenceFile(bad, "", preferenceWrite{key: settingsKeyTheme, value: "dark"})
	bytes, _ := os.ReadFile(bad)
	r.check("malformed preferences refused without replacement", true, err != nil && string(bytes) == "{invalid")
}

func wiringWaitPreference(ctx context.Context, ready func() bool) bool {
	limit, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()
	tick := time.NewTicker(5 * time.Millisecond)
	defer tick.Stop()
	for {
		if ready() {
			return true
		}
		select {
		case <-limit.Done():
			return false
		case <-tick.C:
		}
	}
}

func wiringCompact(raw json.RawMessage) string {
	var value any
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if decoder.Decode(&value) != nil {
		return string(raw)
	}
	data, _ := json.Marshal(value)
	return string(data)
}

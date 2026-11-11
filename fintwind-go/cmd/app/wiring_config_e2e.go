//go:build wiringe2e

package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"fintwind-go/internal/daemon"

	"github.com/egoist/mygo/ui"
)

func wiringRuntimeConfig(ctx context.Context, r *wiringReport, round string) {
	f, err := wiringNewFixture(ctx, round)
	if err != nil {
		r.check("runtime configuration fixture", "success", err.Error())
		return
	}
	defer f.close()
	defer f.stop()
	var mu sync.Mutex
	f.extended = func(envelope wiringEnvelope) (any, error, bool, bool) {
		mu.Lock()
		defer mu.Unlock()
		if envelope.Command.Type == "fetchImageModels" {
			return map[string]any{"type": "json", "value": []runtimeImageModel{{Provider: "fixture", ID: "image", Label: "Fixture image model"}}}, nil, false, true
		}
		return nil, nil, false, false
	}
	client, err := daemon.Dial(ctx, f.server.Listener.Addr().String(), "")
	if err != nil {
		r.check("runtime configuration handshake", "success", err.Error())
		return
	}
	defer client.Close()
	// The desktop writes the OpenCode config file itself, exactly like the
	// Rust client helpers, so the fixture cannot stand in for it: a real
	// temporary OPENCODE_CONFIG_DIR carries the assertions.
	directory := filepath.Join(round, "opencode-config")
	if err := os.MkdirAll(directory, 0o700); err != nil {
		r.check("runtime configuration directory", "success", err.Error())
		return
	}
	os.Setenv("OPENCODE_CONFIG_DIR", directory)
	defer os.Unsetenv("OPENCODE_CONFIG_DIR")
	path := filepath.Join(directory, "opencode.json")
	seed := "{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  // kept comment\n  \"model\": \"fixture/base\"\n}\n"
	if err := os.WriteFile(path, []byte(seed), 0o600); err != nil {
		r.check("runtime configuration seed", "success", err.Error())
		return
	}
	a := wiringApp(ctx, client, round)
	queue, release := installHeadlessDispatcher(a)
	defer release()
	// Writes clear their busy flag through a.post, so every write check
	// drains the queue before the next row is touched.
	waitWritten := func(ready func() bool) bool {
		ok := wiringWaitPreference(ctx, ready)
		wiringApplyUntil(ctx, queue, func() bool { return !a.settings.runtime.writeBusy })
		return ok
	}
	tt := ui.NewTester(a.generalSettings, 1200, 1500)
	a.loadRuntimeConfig()
	r.check("runtime configuration loads the real file", true, wiringApplyUntil(ctx, queue, func() bool { return a.settings.runtime.loaded }))
	tt.Frame()
	wiringRoleClick(r, tt, ui.RoleSwitch, "Todo list")
	r.check("runtime todo toggle writes the config", true, waitWritten(func() bool {
		todo, _ := opencodeLoadTodo()
		return todo
	}))
	r.check("runtime edits keep unrelated lines", true, strings.Contains(wiringReadFile(path), "// kept comment"))
	r.check("runtime edits keep unrelated values", true, strings.Contains(wiringReadFile(path), `"model": "fixture/base"`))
	wiringRoleClick(r, tt, ui.RoleSwitch, "Image generation")
	r.check("runtime image toggle writes the config", true, waitWritten(func() bool {
		image, _, _ := opencodeLoadImageGenerate()
		return image
	}))
	tt.Frame()
	a.loadRuntimeImageModels()
	r.check("runtime image model inventory applies", true, wiringApplyUntil(ctx, queue, func() bool { return !a.settings.runtime.modelsLoading }))
	r.check("runtime model inventory comes from live daemon query", "fixture", a.settings.runtime.models[0].Provider)
	wiringChoice(r, tt, "Default model", 1)
	r.check("runtime model choice writes the object form", true, waitWritten(func() bool {
		_, model, _ := opencodeLoadImageGenerate()
		return model == "fixture/image"
	}))
	a.detectSettingsShells()
	r.check("runtime shell detection lands", true, wiringApplyUntil(ctx, queue, func() bool { return !a.settings.shellsLoading && len(a.settings.shells) > 0 }))
	tt.Frame()
	wiringChoice(r, tt, "OpenCode shell", 1)
	shellPath := a.settings.shells[0].path
	r.check("runtime shell choice writes the config", true, waitWritten(func() bool {
		shell, _ := opencodeLoadShell()
		return shell == shellPath
	}))
	tt.Frame()
	wiringChoice(r, tt, "OpenCode shell", 0)
	r.check("runtime system default removes the key", true, waitWritten(func() bool {
		shell, _ := opencodeLoadShell()
		return shell == ""
	}))
	r.check("runtime write kept the shell removal clean", true, strings.Contains(wiringReadFile(path), "opencode.ai"))
	a.loadRuntimeConfig()
	r.check("runtime reload reads the written values", true, wiringApplyUntil(ctx, queue, func() bool {
		st := &a.settings.runtime
		return st.loaded && st.todo && st.image && st.model == "fixture/image" && st.shell == ""
	}))
}

func wiringReadFile(path string) string {
	data, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	return string(data)
}

func wiringLanguage(ctx context.Context, r *wiringReport, round string) {
	defer SetLanguage("english")
	directory := filepath.Join(round, "language-preferences")
	if err := os.MkdirAll(directory, 0o700); err != nil {
		r.check("language preferences directory", "success", err.Error())
		return
	}
	path := filepath.Join(directory, "app-settings.json")
	_ = os.WriteFile(path, []byte(`{"language":"english","futureLanguage":{"n":9007199254740993}}`), 0o600)
	a := &app{lifetime: ctx, done: make(chan struct{})}
	a.initPreferences(loadLocalSettings(directory))
	defer a.closePreferences()
	tt := ui.NewTester(a.appearanceSettings, 1200, 1500)
	wiringChoice(r, tt, "Language", 2)
	_, chinese := tt.AccessByRole(ui.RolePopUpButton, "语言")
	r.check("language selector changes existing Chinese UI", true, chinese)
	r.check("language canonical choice remains stable", "simplified-chinese", a.preferenceString(settingsKeyLanguage, ""))
	wiringChoice(r, tt, "语言", 3)
	_, japanese := tt.AccessByRole(ui.RolePopUpButton, "言語")
	r.check("language selector changes existing Japanese UI", true, japanese)
	r.check("language saved through real preference writer", true, wiringWaitPreference(ctx, func() bool {
		data, err := os.ReadFile(path)
		var values map[string]json.RawMessage
		return err == nil && json.Unmarshal(data, &values) == nil && string(values["language"]) == `"japanese"`
	}))
	restored := loadLocalSettings(directory)
	b := &app{lifetime: ctx, done: make(chan struct{})}
	b.initPreferences(restored)
	defer b.closePreferences()
	newView := ui.NewTester(b.appearanceSettings, 1200, 1500)
	_, restoredJapanese := newView.AccessByRole(ui.RolePopUpButton, "言語")
	r.check("language restores after fresh app state", true, restoredJapanese)
	r.check("language leaves unknown preferences exact", true, wiringWaitPreference(ctx, func() bool {
		data, err := os.ReadFile(path)
		var values map[string]json.RawMessage
		return err == nil && json.Unmarshal(data, &values) == nil && wiringCompact(values["futureLanguage"]) == `{"n":9007199254740993}`
	}))
}

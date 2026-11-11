package main

import (
	"context"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
)

// runtimeImageModel is one fetchable image model, from the daemon's
// fetchImageModels query.
type runtimeImageModel struct {
	Provider string `json:"provider"`
	ID       string `json:"id"`
	Label    string `json:"label"`
}

// runtimeConfigState is the OpenCode configuration the desktop edits in
// place: the shell, the experimental switches, and their default model, as
// read from the OpenCode config file plus the model inventory the daemon
// fetches.
type runtimeConfigState struct {
	loaded         bool
	loadGeneration uint64
	shell          string
	todo, image    bool
	model          string
	writeBusy      bool
	models         []runtimeImageModel
	modelsLoading  bool
	modelsError    string
}

// loadRuntimeConfig reads the OpenCode config file once in the background,
// the entry-time load Rust's settings rows do. A read failure keeps the
// defaults, like the Rust rows do.
func (a *app) loadRuntimeConfig() {
	st := &a.settings.runtime
	st.loadGeneration++
	generation := st.loadGeneration
	go func() {
		shell, shellErr := opencodeLoadShell()
		todo, todoErr := opencodeLoadTodo()
		image, model, imageErr := opencodeLoadImageGenerate()
		a.post(func() {
			st := &a.settings.runtime
			if st.loadGeneration != generation {
				return
			}
			st.loaded = true
			if shellErr == nil {
				st.shell = shell
			}
			if todoErr == nil {
				st.todo = todo
			}
			if imageErr == nil {
				st.image, st.model = image, model
			}
		})
	}()
}

// saveRuntimeShell writes the shell choice into the OpenCode config; the
// pick applies at once and snaps back when the write fails.
func (a *app) saveRuntimeShell(shell string) {
	st := &a.settings.runtime
	if st.writeBusy {
		return
	}
	previous := st.shell
	st.shell, st.writeBusy = shell, true
	go func() {
		err := opencodeSaveShell(shell)
		a.post(func() {
			st := &a.settings.runtime
			st.writeBusy = false
			if err != nil {
				st.shell = previous
			}
		})
	}()
}

// saveRuntimeTodo writes the experimental todo switch, optimistic like
// Rust's set_todo_enabled: the toggle lands first, a failed write reverts.
func (a *app) saveRuntimeTodo(enabled bool) {
	st := &a.settings.runtime
	if st.writeBusy {
		return
	}
	st.todo, st.writeBusy = enabled, true
	go func() {
		err := opencodeSaveTodo(enabled)
		a.post(func() {
			st := &a.settings.runtime
			st.writeBusy = false
			if err != nil {
				st.todo = !enabled
			}
		})
	}()
}

// saveRuntimeImage writes the experimental image_generate switch together
// with its current default model.
func (a *app) saveRuntimeImage(enabled bool, model string) {
	st := &a.settings.runtime
	if st.writeBusy {
		return
	}
	previousImage, previousModel := st.image, st.model
	st.image, st.model, st.writeBusy = enabled, model, true
	go func() {
		err := opencodeSaveImageGenerate(enabled, model)
		a.post(func() {
			st := &a.settings.runtime
			st.writeBusy = false
			if err != nil {
				st.image, st.model = previousImage, previousModel
			}
		})
	}()
}

// loadRuntimeImageModels queries the daemon for the image models the
// Default model dropdown offers.
func (a *app) loadRuntimeImageModels() {
	st := &a.settings.runtime
	if st.modelsLoading || a.client == nil {
		return
	}
	source := a.currentSettingsSource()
	binary := a.settingsStr("opencode_binary_path", "opencode")
	st.modelsLoading, st.modelsError = true, ""
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
		defer cancel()
		var body struct {
			Value []runtimeImageModel `json:"value"`
		}
		err := settingsRequest(ctx, source, map[string]any{"type": "fetchImageModels", "binary": binary, "directory": source.directory}, "json", &body)
		a.post(func() {
			st := &a.settings.runtime
			if a.currentSettingsSource().client != source.client {
				return
			}
			st.modelsLoading = false
			if err != nil {
				st.modelsError = "Could not load image models. " + err.Error()
				return
			}
			st.models = body.Value
		})
	}()
}

// runtimeShellOptions builds the shell dropdown: System default plus the
// detected shells, then the stored value when it is not in the list, so the
// chip never lies.
func (a *app) runtimeShellOptions(current string) []settingsOption {
	options := []settingsOption{{label: "System default", value: ""}}
	for _, shell := range a.settings.shells {
		options = append(options, settingsOption{label: shell.label, value: shell.path})
	}
	if current == "" {
		return options
	}
	for _, option := range options {
		if option.value == current {
			return options
		}
	}
	return append(options, settingsOption{label: current, value: current})
}

// runtimeShellDropdown is the OpenCode shell control: a dropdown of System
// default and the detected shells, like Rust's opencode-shell-selector.
func (a *app) runtimeShellDropdown(c *ui.Context) {
	st := &a.settings.runtime
	a.settingsChoice(c, "runtime-shell", tr("OpenCode shell"), st.shell, 220, a.runtimeShellOptions(st.shell), func(option settingsOption) {
		if option.value != st.shell {
			a.saveRuntimeShell(option.value)
		}
	}).Disabled(st.writeBusy || !st.loaded)
}

// runtimeImageModelOptions builds the Default model dropdown: Automatic plus
// the fetched models labeled "provider · label", like Rust's menu. The value
// stored in the config is "provider/model".
func (a *app) runtimeImageModelOptions() []settingsOption {
	st := &a.settings.runtime
	options := []settingsOption{{label: "Automatic", value: ""}}
	for _, model := range st.models {
		id := model.ID
		if !strings.HasPrefix(id, model.Provider+"/") {
			id = model.Provider + "/" + id
		}
		label := model.Label
		if label == "" {
			label = id
		}
		options = append(options, settingsOption{label: model.Provider + " · " + label, value: id})
	}
	return options
}

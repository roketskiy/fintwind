package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
)

// Appearance preferences belong to the desktop, not daemon updateSettings.
const (
	settingsKeyTheme           = "theme"
	settingsKeyThemeScheme     = "theme_scheme"
	settingsKeyLanguage        = "language"
	settingsKeyUITextScale     = "ui_text_scale"
	settingsKeyCodeTextScale   = "code_text_scale"
	settingsKeyUIFontFamily    = "ui_font_family"
	settingsKeyCodeFontFamily  = "code_font_family"
	settingsKeyTranscriptWidth = "transcript_width"
	// Terminal preferences ported from kero's settings: shell, extra env,
	// right-click behavior, copy-on-select and scrollback. They are local
	// like the other appearance preferences, not daemon settings.
	settingsKeyTerminalShell        = "terminal_shell"
	settingsKeyTerminalEnv          = "terminal_env"
	settingsKeyTerminalRightClick   = "terminal_right_click"
	settingsKeyTerminalCopyOnSelect = "terminal_copy_on_select"
	settingsKeyTerminalScrollback   = "terminal_scrollback"
)

// Serialized values from the Rust sources:
// ThemePreference (snake_case, theme.rs), ThemeScheme (snake_case, theme.rs),
// AppLanguage (kebab-case, i18n.rs). Labels are the English strings from
// locales/app.yml.
const (
	themeValueSystem = "system"
	themeValueLight  = "light"
	themeValueDark   = "dark"

	languageValueSystem = "system"
	languageValueEn     = "english"
	languageValueZh     = "simplified-chinese"
	languageValueJa     = "japanese"

	defaultUIFontFamily   = ".SystemUIFont"
	defaultCodeFontFamily = "JetBrains Mono"
	systemMonoFontFamily  = ".SystemUIFontMonospaced"
)

// settingsOption is one dropdown row: label is shown, value is the canonical
// string used for selected comparison, raw is written to settingsRaw.
type settingsOption struct {
	label string
	value string
	raw   any
}

func themeOptions() []settingsOption {
	return []settingsOption{
		{"Automatic", themeValueSystem, themeValueSystem},
		{"Light", themeValueLight, themeValueLight},
		{"Dark", themeValueDark, themeValueDark},
	}
}

// schemeOptions mirrors ThemeScheme::ALL with the English labels from
// locales/app.yml; values are the snake_case serde names.
func schemeOptions() []settingsOption {
	return []settingsOption{
		{"Fintwind", "default", "default"},
		{"VS Code Modern", "vs_code", "vs_code"},
		{"Codex", "codex", "codex"},
		{"Claude", "claude", "claude"},
		{"Nord", "nord", "nord"},
		{"Linear", "linear", "linear"},
		{"Notion", "notion", "notion"},
		{"One", "one", "one"},
		{"Proof", "proof", "proof"},
		{"Raycast", "raycast", "raycast"},
		{"Rose Pine", "rose_pine", "rose_pine"},
		{"Solarized", "solarized", "solarized"},
		{"Vercel", "vercel", "vercel"},
		{"VS Code Plus", "vs_code_plus", "vs_code_plus"},
	}
}

// languageOptions mirrors AppLanguage::ALL; explicit names are autonyms so
// the selector stays readable under any locale.
func languageOptions() []settingsOption {
	return []settingsOption{
		{"System", languageValueSystem, languageValueSystem},
		{"English", languageValueEn, languageValueEn},
		{"简体中文", languageValueZh, languageValueZh},
		{"日本語", languageValueJa, languageValueJa},
	}
}

// textScaleOptions mirrors TextSizePreset: scales with English labels.
func textScaleOptions() []settingsOption {
	return []settingsOption{
		{"Small", "0.9", 0.9},
		{"Default", "1", 1.0},
		{"Large", "1.1", 1.1},
		{"Extra large", "1.25", 1.25},
		{"Huge", "1.5", 1.5},
	}
}

// transcriptWidthOptions mirrors TranscriptWidthPreset: 560/736/960/1200.
func transcriptWidthOptions() []settingsOption {
	return []settingsOption{
		{"Compact", "560", 560.0},
		{"Default", "736", 736.0},
		{"Wide", "960", 960.0},
		{"Extra wide", "1200", 1200.0},
	}
}

// pinFontOptions mirrors the Rust font menus: pinned defaults first in keep
// order, then every installed family sorted. Dot-prefixed synthetic entries
// never come out of enumeration, so no hiding pass is needed.
func pinFontOptions(pins []settingsOption, names []string) []settingsOption {
	seen := map[string]bool{}
	out := make([]settingsOption, 0, len(pins)+len(names))
	for _, pin := range pins {
		out = append(out, pin)
		seen[pin.value] = true
	}
	for _, name := range names {
		if seen[name] {
			continue
		}
		seen[name] = true
		out = append(out, settingsOption{label: name, value: name, raw: name})
	}
	return out
}

// uiFontOptions mirrors the Rust interface-font menu: the System default
// pinned first, then every installed family. The fixed list below only
// applies where enumeration is unavailable.
func uiFontOptions() []settingsOption {
	if names := systemFontFamilies(); len(names) > 0 {
		return pinFontOptions([]settingsOption{
			{"System", defaultUIFontFamily, defaultUIFontFamily},
		}, names)
	}
	return []settingsOption{
		{"System", defaultUIFontFamily, defaultUIFontFamily},
		{"Segoe UI", "Segoe UI", "Segoe UI"},
		{"Microsoft YaHei", "Microsoft YaHei", "Microsoft YaHei"},
		{"Arial", "Arial", "Arial"},
	}
}

// codeFontOptions mirrors the Rust code-font menu: JetBrains Mono and System
// monospace pinned first, then every installed family. The fixed list below
// only applies where enumeration is unavailable.
func codeFontOptions() []settingsOption {
	if names := systemFontFamilies(); len(names) > 0 {
		return pinFontOptions([]settingsOption{
			{"JetBrains Mono", defaultCodeFontFamily, defaultCodeFontFamily},
			{"System monospace", systemMonoFontFamily, systemMonoFontFamily},
		}, names)
	}
	return []settingsOption{
		{"System monospace", systemMonoFontFamily, systemMonoFontFamily},
		{"JetBrains Mono", defaultCodeFontFamily, defaultCodeFontFamily},
		{"Cascadia Mono", "Cascadia Mono", "Cascadia Mono"},
		{"Consolas", "Consolas", "Consolas"},
	}
}

// settingsShell is one detected OpenCode shell, mirroring opencode_shell.rs's
// ShellOption: a display label and the executable path written to the config.
type settingsShell struct {
	label string
	path  string
}

// settingsDetectShells enumerates the OpenCode shell candidates the way
// opencode_shell.rs's detect() does: a bounded list over PATH and known
// install directories, stat only, never executing a candidate.
func settingsDetectShells() []settingsShell {
	directories := []string{}
	if search := os.Getenv("PATH"); search != "" {
		directories = append(directories, filepath.SplitList(search)...)
	}
	var names []struct{ label, name string }
	if runtime.GOOS == "windows" {
		for _, variable := range []string{"ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"} {
			if base := os.Getenv(variable); base != "" {
				directories = append(directories,
					filepath.Join(base, "PowerShell", "7"),
					filepath.Join(base, "Git", "bin"),
					filepath.Join(base, "Programs", "Git", "bin"))
			}
		}
		names = []struct{ label, name string }{
			{"PowerShell 7", "pwsh.exe"},
			{"Windows PowerShell", "powershell.exe"},
			{"CMD", "cmd.exe"},
			{"Git Bash", "bash.exe"},
		}
	} else {
		names = []struct{ label, name string }{
			{"Zsh", "zsh"},
			{"Bash", "bash"},
			{"Fish", "fish"},
			{"PowerShell", "pwsh"},
		}
	}
	shells := []settingsShell{}
	for _, candidate := range names {
		for _, directory := range directories {
			if directory == "" {
				continue
			}
			path := filepath.Join(directory, candidate.name)
			info, err := os.Stat(path)
			if err != nil || info.IsDir() {
				continue
			}
			if runtime.GOOS != "windows" && info.Mode()&0o111 == 0 {
				continue
			}
			// Windows' System32/bash.exe is a legacy WSL launcher, not Git Bash.
			if candidate.name == "bash.exe" && !strings.Contains(strings.ToLower(path), "git") {
				continue
			}
			shells = append(shells, settingsShell{label: candidate.label, path: path})
			break
		}
	}
	return shells
}

// detectSettingsShells enumerates the shell candidates once in the
// background; the dropdown re-renders when the list lands.
func (a *app) detectSettingsShells() {
	st := &a.settings
	if st.shellsLoading || len(st.shells) != 0 {
		return
	}
	st.shellsLoading = true
	st.shellsGeneration++
	generation := st.shellsGeneration
	go func() {
		shells := settingsDetectShells()
		a.post(func() {
			if a.settings.shellsGeneration != generation {
				return
			}
			a.settings.shells, a.settings.shellsLoading = shells, false
		})
	}()
}

// settingsStr reads a string key from the daemon settings, defaulting when
// missing or mistyped.
func (a *app) settingsStr(key, def string) string {
	if value, ok := a.settings.preferences.values[key].(string); ok {
		return value
	}
	if a.settingsRaw == nil {
		return def
	}
	value, ok := a.settingsRaw[key].(string)
	if !ok {
		return def
	}
	return value
}

// settingsFloat reads a numeric key from the daemon settings, defaulting
// when missing or mistyped. JSON numbers decode as float64.
func (a *app) settingsFloat(key string, def float64) float64 {
	if value, ok := a.settings.preferences.values[key].(float64); ok {
		return value
	}
	if a.settingsRaw == nil {
		return def
	}
	switch value := a.settingsRaw[key].(type) {
	case float64:
		return value
	case float32:
		return float64(value)
	case int:
		return float64(value)
	case json.Number:
		if parsed, err := value.Float64(); err == nil {
			return parsed
		}
	}
	return def
}

// settingsValueIn returns value when it is one of the dropdown's canonical
// values, else def.
func settingsValueIn(value, def string, options []settingsOption) string {
	for _, option := range options {
		if option.value == value {
			return value
		}
	}
	return def
}

// settingsNearestValue snaps a stored float to the closest option, so a
// hand-edited setting labels with its nearest preset (like for_scale).
func settingsNearestValue(stored float64, options []settingsOption) string {
	best := options[0].value
	bestDist := -1.0
	for _, option := range options {
		scale, ok := option.raw.(float64)
		if !ok {
			continue
		}
		dist := scale - stored
		if dist < 0 {
			dist = -dist
		}
		if bestDist < 0 || dist < bestDist {
			bestDist = dist
			best = option.value
		}
	}
	return best
}

func (a *app) themeValue() string {
	return settingsValueIn(a.settingsStr(settingsKeyTheme, themeValueSystem), themeValueSystem, themeOptions())
}

func (a *app) schemeValue() string {
	return settingsValueIn(a.settingsStr(settingsKeyThemeScheme, "default"), "default", schemeOptions())
}

func (a *app) languageValue() string {
	return settingsValueIn(a.settingsStr(settingsKeyLanguage, languageValueSystem), languageValueSystem, languageOptions())
}

func (a *app) uiScaleValue() string {
	return settingsNearestValue(a.settingsFloat(settingsKeyUITextScale, 1.0), textScaleOptions())
}

func (a *app) codeScaleValue() string {
	return settingsNearestValue(a.settingsFloat(settingsKeyCodeTextScale, 1.0), textScaleOptions())
}

func (a *app) transcriptWidthValue() string {
	return settingsNearestValue(a.settingsFloat(settingsKeyTranscriptWidth, 736.0), transcriptWidthOptions())
}

func (a *app) uiFontValue() string {
	return a.settingsStr(settingsKeyUIFontFamily, defaultUIFontFamily)
}

func (a *app) codeFontValue() string {
	return a.settingsStr(settingsKeyCodeFontFamily, defaultCodeFontFamily)
}

// fontOptionsWithCurrent mirrors the Rust font menus: a stored family that
// is not in the fixed list is shown as its own row so the chip never lies.
func fontOptionsWithCurrent(current string, options []settingsOption) []settingsOption {
	for _, option := range options {
		if option.value == current {
			return options
		}
	}
	if current == "" {
		return options
	}
	return append([]settingsOption{{current, current, current}}, options...)
}

func settingsOptionLabel(current string, options []settingsOption) string {
	for _, option := range options {
		if option.value == current {
			return tr(option.label)
		}
	}
	return current
}

// Failed reads retry when the settings page is opened again.
func (a *app) ensureSettingsLoaded() {
	a.syncSettingsSource()
	if a.settingsLoaded || a.settings.loading || a.client == nil {
		return
	}
	a.settings.loading = true
	client := a.client
	source, generation := a.settings.backend.source, a.settings.backend.generation
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 15*time.Second)
		defer cancel()
		payload, err := client.Request(ctx, map[string]any{"type": "getSettings"})
		if err != nil {
			a.post(func() {
				if a.settingsSourceMatches(source, generation) {
					a.settings.loading = false
					a.settings.message = tr("Could not load settings. ") + err.Error()
				}
			})
			return
		}
		var body struct {
			Settings map[string]any `json:"settings"`
		}
		if err := json.Unmarshal(payload, &body); err != nil || body.Settings == nil {
			a.post(func() {
				if a.settingsSourceMatches(source, generation) {
					a.settings.loading = false
					a.settings.message = tr("Could not decode daemon settings")
				}
			})
			return
		}
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			a.settings.loading = false
			a.settingsRaw = body.Settings
			a.settingsLoaded = true
			a.applyDaemonSettings(body.Settings)
		})
	}()
}

func (a *app) settingsDropdown(c *ui.Context, id, label, key, current string, width float32, options []settingsOption) {
	a.settingsChoice(c, id, tr(label), current, width, options, func(option settingsOption) {
		if option.value == current {
			return
		}
		a.savePreference(key, option.raw)
	})
}

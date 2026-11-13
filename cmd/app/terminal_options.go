package main

import (
	"strings"

	"github.com/egoist/mygo/plugins/terminal"
	"github.com/egoist/mygo/ui"
)

// Terminal look ported from kero (app.go termOptions, theme.go
// lightTermTheme/darkTermTheme, internal/settings defaults): full font,
// Apple-tuned 16-color palettes, padding, cursor, right-click, scrolling
// and scrollback. The kerod daemon behind kero (sessions, restore,
// splits) is not ported: tabs stay local plugin ptys owned by this file.

const (
	// terminalLineHeight and terminalCellWidth are kero's text metrics:
	// 1.22 row height, 1.0 column width.
	terminalLineHeight = 1.22
	terminalCellWidth  = 1.0
	// terminalMinContrast is kero's WCAG floor keeping themed grays
	// legible on either background.
	terminalMinContrast = 3.0
	// terminalScrollbackMB is kero's default scrollback per pane.
	terminalScrollbackMB = 4
)

// keroLightTerminalPalette and keroDarkTerminalPalette are kero's program
// colors: Apple's system colors, deepened slightly for the light theme.
func keroTerminalPalettes() (light, dark [16]ui.Color) {
	light = [16]ui.Color{
		ui.Hex("#1d1d1f"), ui.Hex("#d3302f"), ui.Hex("#22863a"), ui.Hex("#b08800"),
		ui.Hex("#2f5bd8"), ui.Hex("#a033c4"), ui.Hex("#0f8199"), ui.Hex("#8e8e93"),
		ui.Hex("#5f5f66"), ui.Hex("#ef4444"), ui.Hex("#2fa84f"), ui.Hex("#d4a017"),
		ui.Hex("#3b82f6"), ui.Hex("#c04ee0"), ui.Hex("#14a3bf"), ui.Hex("#b4b4bb"),
	}
	dark = [16]ui.Color{
		ui.Hex("#3a3a40"), ui.Hex("#ff6b64"), ui.Hex("#4fd17a"), ui.Hex("#f2c14e"),
		ui.Hex("#6b9bff"), ui.Hex("#d07cf0"), ui.Hex("#4cc9e0"), ui.Hex("#c9c9cf"),
		ui.Hex("#6c6c78"), ui.Hex("#ff8a84"), ui.Hex("#74e397"), ui.Hex("#ffd479"),
		ui.Hex("#8fb3ff"), ui.Hex("#e09cf7"), ui.Hex("#7fdcec"), ui.Hex("#f5f5f7"),
	}
	return light, dark
}

// terminalTheme builds one appearance's terminal colors on the panel's
// own background so the view blends into the right panel, like kero's
// card-colored terminal background.
func terminalTheme(dark bool) *terminal.Theme {
	light, darkPal := keroTerminalPalettes()
	if dark {
		return &terminal.Theme{
			Foreground: ui.Hex("#e6e6ea"), Background: palette.terminal,
			Cursor: ui.Hex("#e6e6ea"), CursorText: ui.Hex("#161618"),
			Selection: ui.Hex("#2f4170"), SelectionText: ui.Hex("#ffffff"),
			Palette: darkPal,
		}
	}
	return &terminal.Theme{
		Foreground: ui.Hex("#1d1d1f"), Background: palette.terminal,
		Cursor: ui.Hex("#1d1d1f"), CursorText: ui.Hex("#ffffff"),
		Selection: ui.Hex("#cfdcfb"), SelectionText: ui.Hex("#1d1d1f"),
		Palette: light,
	}
}

// terminalFont is the full terminal font: the code font at the code scale,
// kero's metrics on top. Previously only family and size were set.
func terminalFont() terminal.Font {
	return terminal.Font{
		Family: terminalFontStack(monoFont()), Size: codePx(13),
		Weight: 400, CellWidth: terminalCellWidth, LineHeight: terminalLineHeight,
	}
}

// terminalScrollbackOptions mirrors kero's scrollback-limit choices in MB;
// 0 keeps none.
func terminalScrollbackOptions() []settingsOption {
	return []settingsOption{
		{"None", "0", 0.0},
		{"1 MB", "1", 1.0},
		{"4 MB", "4", 4.0},
		{"16 MB", "16", 16.0},
		{"64 MB", "64", 64.0},
	}
}

func (a *app) terminalScrollbackValue() string {
	return settingsNearestValue(a.settingsFloat(settingsKeyTerminalScrollback, terminalScrollbackMB), terminalScrollbackOptions())
}

// terminalScrollbackBytes resolves the scrollback preference: negative
// keeps none, matching the plugin's convention.
func (a *app) terminalScrollbackBytes() int {
	mb := int(a.settingsFloat(settingsKeyTerminalScrollback, terminalScrollbackMB))
	if mb <= 0 {
		return -1
	}
	return mb << 20
}

// terminalRightClickOptions mirrors kero's right-click-action choices.
func terminalRightClickOptions() []settingsOption {
	return []settingsOption{
		{"Menu", "menu", "menu"},
		{"Paste", "paste", "paste"},
		{"Copy or paste", "copyOrPaste", "copyOrPaste"},
	}
}

func (a *app) terminalRightClickValue() string {
	return settingsValueIn(a.settingsStr(settingsKeyTerminalRightClick, "menu"), "menu", terminalRightClickOptions())
}

// terminalRightClickAction maps the preference to the plugin action.
func (a *app) terminalRightClickAction() terminal.RightClickAction {
	switch a.terminalRightClickValue() {
	case "paste":
		return terminal.RightClickPaste
	case "copyOrPaste":
		return terminal.RightClickCopyOrPaste
	}
	return terminal.RightClickContextMenu
}

// terminalCopyOnSelect reports the copy-on-select preference.
func (a *app) terminalCopyOnSelect() terminal.CopyOnSelect {
	if settingsBool(a.settings.preferences.values, settingsKeyTerminalCopyOnSelect, false) {
		return terminal.CopyOnSelectClipboard
	}
	return terminal.CopyOnSelectNone
}

// terminalShellCommand resolves the shell preference: empty runs the
// plugin's default login shell, else the chosen executable.
func (a *app) terminalShellCommand() []string {
	if shell := strings.TrimSpace(a.settingsStr(settingsKeyTerminalShell, "")); shell != "" {
		return []string{shell}
	}
	return nil
}

// terminalExtraEnv parses the env preference: KEY=value entries separated
// by newlines or semicolons, "#" comments and blank lines skipped.
func (a *app) terminalExtraEnv() []string {
	raw := a.settingsStr(settingsKeyTerminalEnv, "")
	if strings.TrimSpace(raw) == "" {
		return nil
	}
	var env []string
	for _, entry := range strings.FieldsFunc(raw, func(r rune) bool { return r == '\n' || r == ';' }) {
		entry = strings.TrimSpace(strings.TrimRight(entry, "\r"))
		if entry == "" || strings.HasPrefix(entry, "#") {
			continue
		}
		if !strings.Contains(entry, "=") {
			continue
		}
		env = append(env, entry)
	}
	return env
}

// terminalShellOptions builds the shell dropdown: System default plus the
// detected shells, then the stored path when it is not in the list.
func (a *app) terminalShellOptions(current string) []settingsOption {
	options := []settingsOption{{label: "System default", value: ""}}
	for _, shell := range a.settings.shells {
		options = append(options, settingsOption{label: shell.label, value: shell.path})
	}
	if current != "" {
		options = fontOptionsWithCurrent(current, options)
	}
	return options
}

func (a *app) terminalShellValue() string {
	return a.settingsStr(settingsKeyTerminalShell, "")
}

// terminalOptions builds a tab's plugin options the way kero's termOptions
// does: themed, padded, transparent, with scroll/zoom/copy hooks.
func (a *app) terminalOptions(dir string) terminal.Options {
	return terminal.Options{
		Command:          a.terminalShellCommand(),
		Dir:              dir,
		Env:              a.terminalExtraEnv(),
		ShellIntegration: true,
		Font:             terminalFont(),
		Theme:            terminalTheme(false),
		DarkTheme:        terminalTheme(true),
		Cursor:           terminal.CursorBlock,
		ClipboardRead:    terminal.ClipboardAsk,
		RightClick:       a.terminalRightClickAction(),
		Transparent:      true,
		Padding:          terminal.Padding{X: 16, Y: 6},
		PaddingColor:     terminal.PaddingExtend,
		MinimumContrast:  terminalMinContrast,
		ScrollMultiplier: terminal.ScrollMultiplier{Precision: 1, Discrete: 3},
		Scrollback:       a.terminalScrollbackBytes(),
		CopyOnSelect:     a.terminalCopyOnSelect(),
		// Zoom stays on the code text size: swallowing the gesture keeps
		// every terminal at the shared size instead of drifting per tab.
		OnZoom: func(terminal.Zoom) bool { return true },
		OnCopy: func(_ string, by terminal.CopySource) {
			if by == terminal.CopySelect {
				a.post(func() { a.status = tr("Copied to clipboard") })
			}
		},
	}
}

// retuneNativeTerminals applies font, theme and copy behavior to live tabs,
// like kero's apply retune pass. Shell, env, right-click and scrollback
// only take on new tabs (the plugin has no setters for them).
func (a *app) retuneNativeTerminals() {
	font := terminalFont()
	light, dark := terminalTheme(false), terminalTheme(true)
	copyMode := a.terminalCopyOnSelect()
	a.post(func() {
		for _, session := range a.native.sessions {
			for _, tab := range session.terminals {
				if tab.term == nil || tab.closed {
					continue
				}
				tab.term.SetFont(font)
				tab.term.SetTheme(light, dark)
				tab.term.SetCopyOnSelect(copyMode)
				tab.term.SetScrollMultiplier(terminal.ScrollMultiplier{Precision: 1, Discrete: 3})
				tab.term.SetMinimumContrast(terminalMinContrast)
			}
		}
	})
}

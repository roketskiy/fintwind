package main

import (
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

type appearanceState struct {
	ready                  bool
	dark                   bool
	scheme, font, codeFont string
	textScale, codeScale   float32
}

var baseDarkPalette = palette
var activeAppearance = appearanceState{dark: true, font: uiFontFamily, codeFont: monoFontFamily, textScale: 1, codeScale: 1}

func (a *app) applyAppearance() {
	mode := a.preferenceString(settingsKeyTheme, themeValueSystem)
	hour := time.Now().Hour()
	dark := mode == themeValueDark || (mode != themeValueLight && (hour < 7 || hour >= 19))
	next := appearanceState{ready: true, dark: dark,
		scheme: a.preferenceString(settingsKeyThemeScheme, "default"),
		font:   a.preferenceUIFont(), codeFont: a.preferenceCodeFont(),
		textScale: a.preferenceScale(settingsKeyUITextScale), codeScale: a.preferenceScale(settingsKeyCodeTextScale)}
	if a.appearance == next {
		return
	}
	a.appearance, activeAppearance = next, next
	palette = baseDarkPalette
	if !dark {
		applyLightPalette()
	}
	if schemes, ok := appearanceSchemes[next.scheme]; ok {
		index := 0
		if dark {
			index = 1
		}
		for role, value := range schemes[index] {
			applyPaletteRole(role, hex(value))
		}
		palette.surface = palette.canvas
		palette.rail = palette.sidebar.Mix(hex(0), choice(dark, float32(0.08), float32(0.035)))
		palette.sidebarBorder = palette.border
	}
	if next.scheme == "vs_code" || next.scheme == "claude" || next.scheme == "rose_pine" || next.scheme == "vercel" {
		palette.inverse, palette.onInverse = palette.accentFill, palette.onAccent
		if next.scheme == "vs_code" {
			palette.inverse, palette.onInverse = palette.accent, hex(0xFFFFFF)
		}
	}
	a.mdReady = false
	a.retuneNativeTerminals()
	if a.win != nil {
		mygo.Theme.SetSource(choice(dark, mygo.ThemeDark, mygo.ThemeLight))
	}
}

// schemeSwatchColors resolves one scheme's three swatch colors (canvas,
// raised, accent) against the appearance currently in force — exactly what
// choosing it would apply. The default scheme has no appearanceSchemes
// entry; it resolves from the base palettes.
func schemeSwatchColors(scheme string, dark bool) [3]ui.Color {
	if scheme != "default" {
		if schemes, ok := appearanceSchemes[scheme]; ok {
			index := 0
			if dark {
				index = 1
			}
			var out [3]ui.Color
			keys := [3]string{"canvas", "raised", "accent"}
			for i, key := range keys {
				if value, ok := schemes[index][key]; ok {
					out[i] = hex(value)
				}
			}
			return out
		}
	}
	if dark {
		return [3]ui.Color{baseDarkPalette.canvas, baseDarkPalette.raised, baseDarkPalette.accent}
	}
	return [3]ui.Color{hex(lightBaseCanvas), hex(lightBaseRaised), hex(lightBaseAccent)}
}

// Light base values shared by applyLightPalette and the scheme menu's
// swatch previews.
const (
	lightBaseCanvas = 0xF7F7F7
	lightBaseRaised = 0xF4F4F4
	lightBaseAccent = 0xB25A39
	// lightSidebar keeps every sidebar (sessions, settings, rail) one
	// whiter white so they read as a single chrome.
	lightSidebar = 0xF8F8F8
)

func applyLightPalette() {
	palette.canvas, palette.surface = hex(lightBaseCanvas), hex(lightBaseCanvas)
	palette.sidebar, palette.rail = hex(lightSidebar), hex(lightSidebar)
	palette.raised, palette.composer = hex(lightBaseRaised), hex(0xFAFAFA)
	palette.inset, palette.terminal = hex(0xEFEFEF), hex(0xFFFFFF)
	palette.border, palette.borderStrong = ui.RGBA(31, 31, 31, 0.12), ui.RGBA(31, 31, 31, 0.18)
	palette.cardBorder, palette.sidebarBorder = palette.border, ui.RGBA(20, 20, 20, 0.10)
	palette.text, palette.textSecondary = hex(0x141414), hex(0x5C5C5C)
	palette.textTertiary, palette.textMuted, palette.textGhost = hex(0x696969), hex(0x686868), hex(0xA3A3A3)
	palette.accent, palette.accentText, palette.accentFill = hex(lightBaseAccent), hex(0xA75030), hex(lightBaseAccent)
	palette.onAccent, palette.link = hex(0xFFFFFF), hex(0x1769AA)
	palette.overlay, palette.overlayStrong = ui.RGBA(31, 31, 31, 0.05), ui.RGBA(31, 31, 31, 0.09)
	palette.selection, palette.codeWash = ui.RGBA(0, 123, 255, 0.35), ui.RGBA(31, 31, 31, 0.07)
	palette.codeKeyword, palette.codeLiteral, palette.codeString = hex(0x9A4B92), hex(0x9A6019), hex(0x3F7A36)
	palette.codeNumber, palette.codeType, palette.codeFunction = hex(0x9A6019), hex(0x2F6690), hex(0x2F6690)
	palette.inverse, palette.onInverse = hex(0x202227), hex(0xF8F8F9)
	palette.neutral = hex(0x343434)
	palette.warning, palette.warningText = hex(0xA66B20), hex(0x8A5A00)
	palette.success, palette.successText = hex(0x2F8F52), hex(0x236B3C)
	palette.favorite, palette.danger, palette.dangerText = hex(0xCA8A04), hex(0xC64A42), hex(0xB43A34)
	palette.dangerSoft, palette.sidebarItem = ui.RGBA(200, 74, 66, 0.10), ui.RGBA(20, 20, 20, 0.10)
}

func applyPaletteRole(role string, color ui.Color) {
	switch role {
	case "canvas":
		palette.canvas = color
	case "sidebar":
		palette.sidebar = color
	case "raised":
		palette.raised = color
	case "composer":
		palette.composer = color
	case "inset":
		palette.inset = color
	case "terminal":
		palette.terminal = color
	case "border":
		palette.border = color
	case "border_strong":
		palette.borderStrong = color
	case "text":
		palette.text = color
	case "text_secondary":
		palette.textSecondary = color
	case "text_tertiary":
		palette.textTertiary = color
	case "text_muted":
		palette.textMuted = color
	case "text_ghost":
		palette.textGhost = color
	case "accent":
		palette.accent = color
	case "accent_text":
		palette.accentText = color
	case "accent_fill":
		palette.accentFill = color
	case "on_accent":
		palette.onAccent = color
	case "link":
		palette.link = color
	case "warning":
		palette.warning = color
	case "warning_text":
		palette.warningText = color
	case "success":
		palette.success = color
	case "success_text":
		palette.successText = color
	case "favorite":
		palette.favorite = color
	case "danger":
		palette.danger = color
	case "danger_text":
		palette.dangerText = color
	case "code_keyword":
		palette.codeKeyword = color
	case "code_literal":
		palette.codeLiteral = color
	case "code_string":
		palette.codeString = color
	case "code_number":
		palette.codeNumber = color
	case "code_type":
		palette.codeType = color
	case "code_function":
		palette.codeFunction = color
	}
}

func (a *app) watchAppearance() {
	timer := time.NewTicker(time.Minute)
	defer timer.Stop()
	for {
		select {
		case <-a.lifetime.Done():
			return
		case <-timer.C:
			a.post(func() { a.applyAppearance() })
		}
	}
}

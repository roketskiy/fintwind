package main

import (
	"math"

	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/markdown"
	lobetheme "github.com/lobehub/lobe-mygo/pkg/theme"
)

func hex(v uint32) ui.Color {
	return ui.RGB(uint8(v>>16), uint8(v>>8), uint8(v))
}

func alpha(c ui.Color, a float32) ui.Color {
	return ui.RGBA(c.R, c.G, c.B, a)
}

// Layout constants are copied from Fintwind's app.rs and persistence.rs.
const (
	topbarHeight    = 38.0
	modeRailWidth   = 48.0
	sidebarDefault  = 252.0
	rightPanelWidth = 460.0
	workspaceRadius = 10.0
	cardRadius      = 14.0
	tabRowHeight    = 36.0
	sidebarCardH    = 52.0
	sidebarCardGap  = 2.0
	sidebarActionH  = 32.0
	sidebarProjectH = 28.0
	sidebarPadding  = 10.0
	uiFontFamily    = "Segoe UI"
	monoFontFamily  = "JetBrains Mono"
)

// transcriptWidth is the content column width. Rust keeps it on the theme
// (theme::transcript_width) so Settings -> Appearance can change it live; the
// default is DEFAULT_TRANSCRIPT_WIDTH from persistence.rs.
var transcriptWidth float32 = 736

// normalizeTranscriptWidth mirrors persistence.rs: clamp 560..1200, then snap
// to the 16 px step.
func normalizeTranscriptWidth(width float64) float64 {
	if width < 560 {
		width = 560
	}
	if width > 1200 {
		width = 1200
	}
	return 560 + math.Round((width-560)/16)*16
}

const (
	changedFilesPreviewLimit  = 3
	changedFilesExpandedLimit = 12
)

// palette mirrors Fintwind's base_dark theme in src/theme.rs.
var palette = struct {
	canvas, sidebar, rail, surface, raised, composer, inset, terminal ui.Color
	border, borderStrong, cardBorder, sidebarBorder                   ui.Color
	text, textSecondary, textTertiary, textMuted, textGhost           ui.Color
	accent, accentText, accentFill, onAccent, link                    ui.Color
	overlay, overlayStrong, selection, codeWash                       ui.Color
	codeKeyword, codeLiteral, codeString, codeNumber, codeType        ui.Color
	codeFunction                                                      ui.Color
	inverse, onInverse                                                ui.Color
	neutral                                                           ui.Color
	warning, success, favorite, danger, dangerSoft                    ui.Color
	successText, dangerText, warningText                              ui.Color
	sidebarItem                                                       ui.Color
}{
	canvas:        hex(0x222222),
	sidebar:       hex(0x1A1A1A),
	rail:          hex(0x141414),
	surface:       hex(0x222222),
	raised:        hex(0x2B2B2B),
	composer:      hex(0x282828),
	inset:         hex(0x151515),
	terminal:      hex(0x151515),
	border:        ui.RGBA(227, 229, 232, 0.07),
	borderStrong:  ui.RGBA(227, 229, 232, 0.14),
	cardBorder:    ui.RGBA(227, 229, 232, 0.08),
	sidebarBorder: hex(0x292929),
	text:          hex(0xE2E2E2),
	textSecondary: hex(0xA3A3A3),
	textTertiary:  hex(0x929292),
	textMuted:     hex(0x929292),
	textGhost:     hex(0x575757),
	accent:        hex(0xD97757),
	accentText:    hex(0xD97757),
	accentFill:    hex(0xD97757),
	onAccent:      hex(0x201814),
	link:          hex(0x8ACFF8),
	overlay:       ui.RGBA(227, 229, 232, 0.05),
	overlayStrong: ui.RGBA(227, 229, 232, 0.09),
	selection:     ui.RGBA(0, 123, 255, 0.55),
	codeWash:      ui.RGBA(227, 229, 232, 0.08),
	codeKeyword:   hex(0xC98BC0),
	codeLiteral:   hex(0xD9A05B),
	codeString:    hex(0x94C08A),
	codeNumber:    hex(0xD9A05B),
	codeType:      hex(0x8FB8D9),
	codeFunction:  hex(0x8FB8D9),
	inverse:       hex(0xE7E9EC),
	onInverse:     hex(0x17181C),
	// Neutral emphasis for selected controls, mirroring neutral_fill in
	// src/theme.rs (light gray on dark, dark gray on light).
	neutral:     hex(0xD6D6D6),
	warning:     hex(0xE0B36A),
	success:     hex(0x62C987),
	favorite:    hex(0xEAB308),
	danger:      hex(0xE2726A),
	dangerSoft:  ui.RGBA(212, 116, 109, 0.10),
	successText: hex(0x62C987),
	dangerText:  hex(0xE2726A),
	warningText: hex(0xE0B36A),
	sidebarItem: ui.RGBA(240, 240, 240, 0.06),
}

func monoFont() string {
	return activeAppearance.codeFont
}

// newMarkdownOptions builds the transcript's markdown metrics: body, user
// bubble, or compact tool detail.
func newMarkdownOptions(c *ui.Context, fontSize, lineHeight, codeSize, codeLineHeight float32) markdown.Options {
	codeScale := activeAppearance.codeScale / activeAppearance.textScale
	codeSize *= codeScale
	codeLineHeight *= codeScale
	base := markdown.DefaultTheme(c.Theme())
	theme := *base
	theme.FontSize = fontSize
	theme.Font = activeAppearance.font
	theme.CodeFont = activeAppearance.codeFont
	theme.LineHeight = lineHeight / fontSize
	theme.CodeSize = codeSize / fontSize
	theme.CodeLineHeight = codeLineHeight / codeSize
	theme.Text = palette.text
	theme.Muted = palette.textSecondary
	theme.Link = palette.link
	theme.Strong = palette.text
	theme.Code = palette.text
	theme.CodeBackground = palette.codeWash
	theme.QuoteBar = palette.border
	theme.QuoteText = palette.textSecondary
	theme.CodeBlockBackground = palette.inset
	theme.CodeBlockBorder = palette.border
	theme.BlockSpacing = 10.0 / fontSize
	return markdown.Options{Theme: &theme, Reveal: &markdown.Reveal{Unit: markdown.AtOnce, Fade: -1}}
}

// cardShadow is Fintwind's three outer lift layers: contact, mid, ambient.
func cardShadow(el ui.Element) ui.Element {
	return el.
		Shadow(0, 1, 1, 0, ui.RGBA(0, 0, 0, 0.30)).
		Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.18)).
		Shadow(0, 12, 28, 0, ui.RGBA(0, 0, 0, 0.12))
}

func uiText(c *ui.Context) *ui.Theme {
	t := *c.Theme()
	t.Dark = activeAppearance.dark
	t.Background = palette.canvas
	t.Surface = palette.raised
	t.SurfaceHover = palette.overlay
	t.SurfacePressed = palette.overlayStrong
	if t.Dark {
		t.Surface = palette.raised.Mix(palette.text, 0.04)
		t.SurfaceHover = palette.raised.Mix(palette.text, 0.09)
		t.SurfacePressed = palette.raised.Mix(palette.text, 0.14)
	}
	t.Border = palette.border
	t.Text = palette.text
	t.TextMuted = palette.textSecondary
	t.Accent = palette.accent
	t.AccentHover = palette.accent.Mix(palette.text, 0.10)
	t.AccentPressed = palette.accent.Mix(palette.text, 0.18)
	t.AccentText = palette.onAccent
	t.Focus = palette.accent
	t.Danger = palette.danger
	t.Warning = palette.warning
	t.Success = palette.success
	t.Selection = palette.selection
	t.Font = activeAppearance.font
	t.FontSize = 13.5
	t.Radius = 8
	return componentTheme(t)
}

// Keep the registered theme pointer: Lobe resolves tokens by pointer identity.
// Cache immutable themes, not one new Config per frame (Lobe retains Configs).
// This is used only on the UI thread, like the palette itself.
var componentThemes = make(map[ui.Theme]*ui.Theme)

func componentTheme(t ui.Theme) *ui.Theme {
	if cached := componentThemes[t]; cached != nil {
		return cached
	}
	light, dark := lobetheme.Light(), lobetheme.Dark()
	if t.Dark {
		// Share opaque control surfaces with MyGo/Crychic instead of Lobe's
		// near-black container and translucent neutral button fills.
		dark.BgContainer = t.Surface
		dark.FillTertiary = t.Surface
		dark.FillSecondary = t.SurfaceHover
		dark.Fill = t.SurfacePressed
		dark.Border = t.TextMuted.Alpha(0.28)
		dark.Text = t.Text
		dark.TextSecondary = t.TextMuted
		dark.TextTertiary = t.TextMuted
		dark.TextQuaternary = t.TextMuted.Alpha(0.7)
		dark.TextDisabled = t.TextMuted.Alpha(0.5)
	}
	config := lobetheme.NewConfig(*light, *dark)
	registered := config.Theme(t.Dark)
	// Configure our own theme once, before publishing it. Do not call Apply:
	// appearance selection and frame-wide theme ownership stay with the app.
	*registered = t
	componentThemes[t] = registered
	return registered
}

func choice[T any](cond bool, yes, no T) T {
	if cond {
		return yes
	}
	return no
}

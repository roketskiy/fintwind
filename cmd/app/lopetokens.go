package main

import (
	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

// lobeConfig exposes the current palette to lobe-mygo components. The app
// renders everything from its own palette; lobe widgets read theme.Tokens
// instead, so a Config made from the palette keeps both in step. Configs are
// remembered for the life of the app, one per appearance scheme and mode:
// applyAppearance rebuilds the palette deterministically per pair, so a
// cached Config stays valid, and a mode or scheme change misses the cache
// and makes a fresh one. Only the half of a Config matching the current
// window mode is ever applied, so the other half mirrors the palette it was
// built from and is never read.
var lobeConfigs = map[lobeConfigKey]*theme.Config{}

type lobeConfigKey struct {
	scheme string
	dark   bool
}

func lobeConfig(dark bool) *theme.Config {
	key := lobeConfigKey{activeAppearance.scheme, dark}
	if cfg, ok := lobeConfigs[key]; ok {
		return cfg
	}
	tokens := lobeTokens(dark)
	cfg := theme.NewConfig(tokens, tokens)
	lobeConfigs[key] = cfg
	return cfg
}

// lobeTokens maps the palette onto the tokens lobe widgets read. It starts
// from Lobe's own set for the current mode so unmapped fields keep sane
// values, then overrides every family the source control page touches.
func lobeTokens(dark bool) theme.Tokens {
	t := *theme.Dark()
	if !dark {
		t = *theme.Light()
	}
	// Surfaces and chrome.
	t.BgBase = palette.canvas
	t.BgLayout = palette.canvas
	t.BgContainer = palette.surface
	t.BgContainerSecondary = palette.raised
	t.BgContainerDisabled = palette.overlay
	t.BgElevated = palette.raised
	t.BgSpotlight = palette.raised
	t.BgBlur = palette.canvas
	t.BgTextHover = palette.overlay
	t.BgTextActive = palette.overlayStrong
	// Text.
	t.Text = palette.text
	t.TextBase = palette.text
	t.TextHeading = palette.text
	t.TextSecondary = palette.textSecondary
	t.TextDescription = palette.textSecondary
	t.TextTertiary = palette.textTertiary
	t.TextQuaternary = palette.textGhost
	t.TextLabel = palette.textTertiary
	t.TextPlaceholder = palette.textGhost
	t.TextDisabled = palette.textGhost
	t.TextLightSolid = palette.onAccent
	// Borders and fills.
	t.Border = palette.border
	t.BorderSecondary = palette.border
	t.BorderBg = palette.border
	t.BorderDisabled = alpha(palette.border, 0.04)
	t.Fill = palette.overlay
	t.FillSecondary = palette.overlayStrong
	t.FillTertiary = palette.overlay
	t.FillQuaternary = palette.overlay
	t.FillContent = palette.overlayStrong
	t.FillContentHover = palette.overlayStrong
	t.FillAlter = palette.overlay
	t.Split = palette.border
	// Accent.
	t.Primary = palette.accentFill
	t.PrimaryHover = palette.accent
	t.PrimaryActive = palette.accentText
	t.PrimaryBg = palette.accentFill
	t.PrimaryBgHover = palette.accent
	t.PrimaryBorder = palette.accentFill
	t.PrimaryBorderHover = palette.accent
	t.PrimaryText = palette.accentText
	t.PrimaryTextHover = palette.accentText
	t.PrimaryTextActive = palette.accentText
	t.PrimaryForeground = palette.onAccent
	t.PrimaryFill = alpha(palette.accentFill, 0.16)
	t.PrimaryFillSecondary = alpha(palette.accentFill, 0.24)
	t.PrimaryFillTertiary = alpha(palette.accentFill, 0.12)
	t.PrimaryFillQuaternary = alpha(palette.accentFill, 0.06)
	// Status.
	t.Success = palette.success
	t.SuccessHover = palette.success
	t.SuccessText = palette.successText
	t.SuccessBg = alpha(palette.success, 0.12)
	t.SuccessBorder = palette.success
	t.SuccessFill = alpha(palette.success, 0.16)
	t.SuccessFillSecondary = alpha(palette.success, 0.24)
	t.SuccessFillTertiary = alpha(palette.success, 0.12)
	t.SuccessFillQuaternary = alpha(palette.success, 0.06)
	t.Error = palette.danger
	t.ErrorHover = palette.danger
	t.ErrorText = palette.dangerText
	t.ErrorBg = alpha(palette.danger, 0.12)
	t.ErrorBorder = palette.danger
	t.ErrorFill = alpha(palette.danger, 0.16)
	t.ErrorFillSecondary = alpha(palette.danger, 0.24)
	t.ErrorFillTertiary = alpha(palette.danger, 0.12)
	t.ErrorFillQuaternary = alpha(palette.danger, 0.06)
	t.Warning = palette.warning
	t.WarningHover = palette.warning
	t.WarningText = palette.warningText
	t.WarningBg = alpha(palette.warning, 0.12)
	t.WarningBorder = palette.warning
	t.WarningFill = alpha(palette.warning, 0.16)
	t.WarningFillSecondary = alpha(palette.warning, 0.24)
	t.WarningFillTertiary = alpha(palette.warning, 0.12)
	t.WarningFillQuaternary = alpha(palette.warning, 0.06)
	t.Info = palette.link
	t.InfoHover = palette.link
	t.InfoText = palette.link
	t.InfoBg = alpha(palette.link, 0.12)
	t.InfoFill = alpha(palette.link, 0.16)
	t.InfoFillSecondary = alpha(palette.link, 0.24)
	t.InfoFillTertiary = alpha(palette.link, 0.12)
	t.InfoFillQuaternary = alpha(palette.link, 0.06)
	t.Highlight = palette.selection
	t.Icon = palette.textSecondary
	t.IconHover = palette.text
	// Metrics, matching uiText and the source control rows.
	t.FontSize = 13.5
	t.FontSizeSM = 11.5
	t.FontSizeLG = 15
	t.FontWeightStrong = 600
	t.BorderRadius = 8
	t.BorderRadiusSM = 6
	t.BorderRadiusLG = 10
	t.BorderRadiusXS = 4
	t.ControlHeight = 28
	t.ControlHeightSM = 24
	t.ControlHeightLG = 32
	t.ControlHeightXS = 22
	t.LineWidth = 1
	return t
}

// lobeThemed wraps a subtree so lobe widgets inside read tokens made from
// the palette. It stays a subtree: lobe theme.Apply would retarget the whole
// frame and follow the system appearance, which agitates sibling views.
func lobeThemed(c *ui.Context, body func()) ui.Element {
	cfg := lobeConfig(activeAppearance.dark)
	el := ui.Themed(c, cfg.Theme(activeAppearance.dark))
	el.Children(body)
	return el
}

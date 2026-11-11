package main

import (
	"fmt"
	"time"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/action"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

// Esc-stop confirmation and the composer usage meter.
//
// Rust parity:
//   - src/app.rs: ESCAPE_STOP_CONFIRMATION_TIMEOUT (3s), EscapeStopTarget
//     { session_id, turn_id }, EscapeStopConfirmation press/arm/expire.
//   - src/app/sessions.rs: cancel_turn_action arms on the first press while
//     the selected session is busy and cancels on a matching second press.
//   - src/app/composer.rs: the stop button shows "Esc" at 10.5 semibold
//     while armed, the stop icon otherwise.
//   - src/app/usage_meter.rs: render_usage_meter — h24 px6 rounded5 pill,
//     13px ring (2.5 stroke) on the border_strong track, fill gauge
//     normally, warning at >= 80%, danger at >= 95%.

// escapeStopTimeout matches ESCAPE_STOP_CONFIRMATION_TIMEOUT in src/app.rs.
const escapeStopTimeout = 3 * time.Second

// Esc-stop confirmation state. Single window, so package-level state is
// enough; every access runs on the UI thread except the expiry timer, which
// re-enters through a.post and is fenced by escapeStopGen.
var (
	escapeStopArmed     bool
	escapeStopSessionID string
	escapeStopTurnID    string
	escapeStopGen       uint64
)

// escapeStopTarget mirrors EscapeStopTarget::for_session: the confirmation
// only applies while the open session is busy, keyed by session and turn so
// a finished turn never inherits a stale arm.
func escapeStopTarget(a *app) (sessionID, turnID string, ok bool) {
	if a.detail == nil || !a.detail.Busy() {
		return "", "", false
	}
	return a.detail.ID, a.detail.ActiveTurnID(), true
}

// pressEscapeStop handles one Esc press in the composer input. It reports
// whether the key was consumed: false when idle (default behavior kept),
// true when busy — arming on the first press, cancelling on a matching
// second press within the timeout.
func (a *app) pressEscapeStop() bool {
	sessionID, turnID, ok := escapeStopTarget(a)
	if !ok {
		return false
	}
	if escapeStopArmed && escapeStopSessionID == sessionID && escapeStopTurnID == turnID {
		a.disarmEscapeStop()
		a.cancelTurn()
		return true
	}
	escapeStopArmed = true
	escapeStopSessionID = sessionID
	escapeStopTurnID = turnID
	escapeStopGen++
	gen := escapeStopGen
	time.AfterFunc(escapeStopTimeout, func() {
		a.post(func() {
			if escapeStopGen == gen {
				escapeStopArmed = false
			}
		})
	})
	return true
}

// escapeStopArmedForCurrent mirrors the escape_stop_armed flag in
// render_composer: true only while the open busy turn matches the arm.
func (a *app) escapeStopArmedForCurrent() bool {
	sessionID, turnID, ok := escapeStopTarget(a)
	return ok && escapeStopArmed &&
		escapeStopSessionID == sessionID && escapeStopTurnID == turnID
}

// disarmEscapeStop clears the confirmation, mirroring cancel_turn's clear.
func (a *app) disarmEscapeStop() {
	escapeStopArmed = false
}

// usageGaugeFill is theme.gauge (0x3B82F6 in src/theme.rs). The Go palette
// has no gauge slot, so it lives here next to its only use.
var usageGaugeFill = hex(0x3B82F6)

// usageMeterBarWidth is the mini bar standing in for the 13px footer ring;
// mygo has no canvas arc here, so the same track/fill colors paint a bar.
const usageMeterBarWidth = 40.0

// contextPercent mirrors context_percent in usage_meter.rs: no percent
// until a call is measured and the window is known.
func contextPercent(usage *model.ContextUsage) (float64, bool) {
	if usage == nil {
		return 0, false
	}
	if !usage.Measured && usage.Tokens == 0 {
		return 0, false
	}
	if usage.Window == nil || *usage.Window == 0 {
		return 0, false
	}
	return float64(usage.Tokens) * 100.0 / float64(*usage.Window), true
}

// formatTokens mirrors format_tokens in crates/fintwind-protocol/src/usage.rs.
func formatTokens(tokens uint64) string {
	if tokens >= 999500 {
		return fmt.Sprintf("%.1fM", float64(tokens)/1000000.0)
	}
	if tokens >= 1000 {
		return fmt.Sprintf("%.1fk", float64(tokens)/1000.0)
	}
	return fmt.Sprintf("%d", tokens)
}

// formatPercent mirrors format_percent: one decimal place.
func formatPercent(percent float64) string {
	return fmt.Sprintf("%.1f%%", percent)
}

// usageFillColor mirrors render_usage_meter's fill choice.
func usageFillColor(percent float64, ok bool) ui.Color {
	switch {
	case ok && percent >= 95:
		return palette.danger
	case ok && percent >= 80:
		return palette.warning
	default:
		return usageGaugeFill
	}
}

func (a *app) usageMeter(c *ui.Context) {
	if a.detail == nil {
		return
	}
	usage := a.effectiveContextUsage()
	percent, ok := contextPercent(usage)
	fill := usageFillColor(percent, ok)
	label := "—"
	if usage != nil && (usage.Measured || usage.Tokens > 0) {
		label = formatTokens(usage.Tokens)
	}
	if usage != nil && usage.Window != nil && *usage.Window > 0 {
		label += "/" + formatTokens(*usage.Window)
	}
	tip := tr("Context") + " (Ctrl+U)"
	if ok {
		tip = tr("Context used") + ": " + formatPercent(percent) + " (Ctrl+U)"
	}
	c.Key("composer-usage")
	action.Button(c, "", action.ButtonProps{Variant: action.Text, Size: theme.Small}).Height(24).Padding(0, 6).Radius(5).Gap(6).
		AlignItems(ui.Center).Label(tr("Context")).Tooltip(tip).OnClick(a.toggleContextPanel).Children(func() {
		ui.Row(c).Width(usageMeterBarWidth).Height(4).Radius(2).
			Background(palette.borderStrong).Children(func() {
			if !ok {
				return
			}
			frac := percent / 100.0
			if frac < 0 {
				frac = 0
			}
			if frac > 1 {
				frac = 1
			}
			w := float32(usageMeterBarWidth * frac)
			if w < 2 {
				w = 2
			}
			ui.Row(c).Width(w).Height(4).Radius(2).Background(fill)
		})
		ui.Text(c, label).FontSize(uiPx(11.5)).TextColor(palette.textSecondary)
	})
}

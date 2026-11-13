// Package diagramview presents cached vector diagrams inline and in a zoomable preview.
package diagramview

import (
	"fmt"
	"math"

	"github.com/egoist/mygo/ui"
	"github.com/firede/limu/limumygo"
	"github.com/firede/limu/scene"
	"github.com/firede/limu/style"
	"github.com/lobehub/lobe-mygo/pkg/action"
	"github.com/lobehub/lobe-mygo/pkg/feedback"
)

type Labels struct{ Open, ZoomIn, ZoomOut, Fit string }

type Viewer struct {
	scene  *scene.Scene
	open   bool
	scale  float64 // zero fits the viewport
	scroll ui.ScrollState
}

// Inline derives geometry from the current layout, without keeping last frame's
// width or rebuilding the scene when its containing pane changes size.
func (v *Viewer) Inline(c *ui.Context, sc *scene.Scene, labels Labels) {
	ui.Row(c).FillWidth().MinWidth(0).Justify(ui.Center).Children(func() {
		ui.Box(c).FillWidth().MaxWidth(float32(sc.Width)).MinWidth(0).
			AspectRatio(float32(sc.Width / sc.Height)).Shrink(0).Children(func() {
			button := action.Button(c, "", action.ButtonProps{Variant: action.Text}).Label(labels.Open).
				FillWidth().FillHeight().Padding(0).Border(0, ui.Transparent).
				Background(ui.Transparent)
			paint(button, sc, diagramTheme(c), 0)
			button.OnClick(func() {
				v.scene, v.open, v.scale = sc, true, 0
				v.scroll = ui.ScrollState{}
			})
		})
	})
}

func (v *Viewer) Layer(c *ui.Context, labels Labels) {
	if !v.open || v.scene == nil {
		return
	}
	windowW, windowH := c.Size()
	panelW := min(float32(1280), max(float32(120), windowW-64))
	viewW := panelW - 32
	viewH := max(float32(80), min(float32(760), windowH-200))
	sc := v.scene
	fit := math.Min(float64(viewW)/sc.Width, float64(viewH)/sc.Height)
	scale := v.scale
	if scale == 0 {
		scale = fit
	}
	feedback.Modal(c.Key("diagram-preview"), &v.open, feedback.ModalProps{
		Title: "Mermaid", Width: panelW, NoFooter: true,
	}, func() {
		ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Margin(0, 0, 8, 0).Children(func() {
			if action.Button(c.Key("zoom-out"), "−", action.ButtonProps{Variant: action.Text}).Label(labels.ZoomOut).Clicked() {
				v.zoom(scale / 1.25)
			}
			if action.Button(c.Key("actual-size"), fmt.Sprintf("%.0f%%", scale*100), action.ButtonProps{Variant: action.Text}).Label("100%").Clicked() {
				v.zoom(1)
			}
			if action.Button(c.Key("zoom-in"), "+", action.ButtonProps{Variant: action.Text}).Label(labels.ZoomIn).Clicked() {
				v.zoom(scale * 1.25)
			}
			if action.Button(c.Key("fit"), labels.Fit, action.ButtonProps{Variant: action.Text}).Clicked() {
				v.scale, v.scroll.X, v.scroll.Y = 0, 0, 0
			}
		})
		viewport := ui.ScrollBoth(c.Key("diagram-viewport")).Size(viewW, viewH).
			MinWidth(0).TrackScroll(&v.scroll).Focusable().Label("Mermaid")
		viewport.HandleInput(func(ev ui.InputEvent) bool {
			switch {
			case ev.Kind == ui.InputZoom:
				v.zoom(scale * float64(ev.Scale))
				return true
			case ev.Kind == ui.InputScroll && ev.Mods&ui.Ctrl != 0:
				v.zoom(scale * math.Pow(1.1, -float64(ev.DY)/120))
				return true
			}
			return false
		})
		viewport.Children(func() {
			canvas := ui.Box(c).Size(max(viewW, float32(sc.Width*scale)), max(viewH, float32(sc.Height*scale))).Shrink(0).
				Role(ui.RoleImage).Label("Mermaid")
			paint(canvas, sc, diagramTheme(c), scale)
		})
	})
	if !v.open {
		v.scene = nil
	}
}

func (v *Viewer) zoom(scale float64) { v.scale = max(0.1, min(8, scale)) }

func diagramTheme(c *ui.Context) *style.Theme {
	if c.Theme().Dark {
		return style.KohuDark()
	}
	return style.Kohu()
}

func paint(element ui.Element, sc *scene.Scene, theme *style.Theme, scale float64) {
	element.Draw(func(p *ui.Painter, r ui.Rect) {
		drawScale := scale
		if drawScale == 0 {
			drawScale = math.Min(float64(r.W)/sc.Width, float64(r.H)/sc.Height)
		}
		if drawScale <= 0 {
			return
		}
		x := r.X + (r.W-float32(sc.Width*drawScale))/2
		y := r.Y + (r.H-float32(sc.Height*drawScale))/2
		limumygo.Paint(p, sc, x, y, &limumygo.Options{Fonts: limumygo.DefaultFonts, Look: style.Look{Theme: theme}, Scale: drawScale})
	})
}

// diagramcheck exercises the native Mermaid view without a window or provider.
package main

import (
	"fmt"
	"math"
	"os"
	"strings"

	"fintwind-go/internal/diagramview"

	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/markdown"
	"github.com/firede/limu"
	"github.com/firede/limu/layout"
	"github.com/firede/limu/limumygo"
)

func require(ok bool, message string) {
	if !ok {
		fmt.Fprintln(os.Stderr, message)
		os.Exit(1)
	}
}

func percent(t *ui.Tester) string {
	for _, text := range t.Texts() {
		if strings.HasSuffix(text, "%") && text != "100%" {
			return text
		}
	}
	return "100%"
}

func main() {
	source := "flowchart LR\nA[Receive request] --> B[Parse input] --> C[Validate] --> D[Run task] --> E[Publish result]"
	sc, err := limu.Layout(source, &layout.Options{
		Measurer: limumygo.DefaultFonts, MaxWidth: 680, NoImages: true, NoLinks: true,
	})
	require(err == nil, fmt.Sprint(err))
	labels := diagramview.Labels{Open: "Open diagram", ZoomIn: "Zoom in", ZoomOut: "Zoom out", Fit: "Fit to window"}
	for _, dark := range []bool{false, true} {
		var viewer diagramview.Viewer
		var sidePanel float32
		t := ui.NewTesterWith(func(c *ui.Context) {
			width, _ := c.Size()
			ui.Row(c).Fill().Children(func() {
				ui.Column(c).Width(width - sidePanel).FillHeight().Padding(20).Children(func() {
					markdown.View(c, "```mermaid\n"+source+"\n```", &markdown.Options{
						Code: map[string]func(*ui.Context, *markdown.CodeBlockView){
							"mermaid": func(c *ui.Context, _ *markdown.CodeBlockView) { viewer.Inline(c, sc, labels) },
						},
					})
				})
				ui.Row(c).Width(sidePanel)
			})
			viewer.Layer(c, labels)
		}, 960, 900, ui.TesterOptions{Dark: dark, Preferences: ui.Preferences{ReduceMotion: true}})
		wide, found := t.Find(labels.Open)
		require(found && wide.W > 0 && wide.H > 0, fmt.Sprintf("diagram has no inline geometry: found=%v bounds=%+v scene=%gx%g", found, wide, sc.Width, sc.Height))
		require(math.Abs(float64(wide.X+wide.W/2-480)) < 1, fmt.Sprintf("diagram is not centered: %+v", wide))
		require(math.Abs(float64(wide.W/wide.H)-sc.Width/sc.Height) < 0.01, "diagram aspect ratio changed")
		t.SetSize(360, 900)
		narrow, _ := t.Find(labels.Open)
		require(narrow.W <= 320 && narrow.W < wide.W && narrow.H < wide.H, fmt.Sprintf("diagram did not shrink with its pane: wide %+v narrow %+v", wide, narrow))
		require(math.Abs(float64(narrow.X+narrow.W/2-180)) < 1, "narrow diagram is not centered")
		t.SetSize(960, 900)
		restored, _ := t.Find(labels.Open)
		require(math.Abs(float64(restored.W-wide.W)) < 1, "diagram did not grow back with its pane")
		sidePanel = 640
		t.Frame()
		squeezed, _ := t.Find(labels.Open)
		require(squeezed.W <= 280 && math.Abs(float64(squeezed.X+squeezed.W/2-160)) < 1, "side panel did not resize and center the Markdown diagram")
		sidePanel = 0
		t.Frame()
		require(t.Click(labels.Open) == nil, "inline diagram is not clickable")
		require(t.HasText(labels.Fit), "click did not open the preview")
		before := percent(t)
		require(t.Click(labels.ZoomIn) == nil, "preview has no zoom control")
		require(percent(t) != before, "zoom control did not change scale")
		require(t.Click("100%") == nil, "preview cannot restore actual size")
		require(t.Click(labels.Fit) == nil, "preview cannot fit its viewport")
		t.SetSize(520, 640)
		require(t.HasText(labels.Fit), "resizing closed the preview")
		t.Key(0, ui.KeyEscape)
		require(!t.HasText(labels.Fit), "Escape did not dismiss the preview")
		// The modal restores focus to its trigger; keyboard activation must
		// reopen the same scene without needing a mouse.
		t.Key(0, ui.KeyEnter)
		require(t.HasText(labels.Fit), "keyboard activation did not reopen the preview")
		t.Key(0, ui.KeyEscape)
		fmt.Printf("PASS dark=%v: centered aspect fit, resize, click preview, zoom, Escape and keyboard reopen\n", dark)
	}
}

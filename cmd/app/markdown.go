package main

import (
	"crypto/sha256"
	"strings"

	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/markdown"
	"github.com/firede/limu"
	"github.com/firede/limu/layout"
	"github.com/firede/limu/limumygo"
	"github.com/firede/limu/scene"
	"github.com/firede/limu/style"
)

func (a *app) renderMarkdown(c *ui.Context, id, text string, options *markdown.Options, streaming bool) {
	c.Key(id)
	if streaming {
		markdown.Stream(c, text, options)
	} else {
		markdown.View(c, text, options)
	}
}

type diagramState struct {
	scene  *scene.Scene
	failed bool
}

func (a *app) renderMarkdownCode(c *ui.Context, block *markdown.CodeBlockView) {
	text := strings.TrimSpace(block.Code())
	if strings.HasPrefix(text, "```mermaid\n") && strings.HasSuffix(text, "```") {
		a.renderDiagramSource(c, block, strings.TrimSpace(strings.TrimSuffix(strings.TrimPrefix(text, "```mermaid\n"), "```")))
		return
	}
	markdown.DefaultCodeBlock(c, block)
}

func (a *app) renderDiagram(c *ui.Context, block *markdown.CodeBlockView) {
	a.renderDiagramSource(c, block, block.Code())
}

func (a *app) renderDiagramSource(c *ui.Context, block *markdown.CodeBlockView, source string) {
	if block.Open || len(source) > 32<<10 || strings.TrimSpace(source) == "" {
		markdown.DefaultCodeBlock(c, block)
		return
	}
	key := sha256.Sum256([]byte(source))
	if a.diagrams == nil {
		a.diagrams = map[[32]byte]*diagramState{}
	}
	entry := a.diagrams[key]
	if entry == nil {
		if len(a.diagrams) >= 128 {
			markdown.DefaultCodeBlock(c, block)
			return
		}
		if a.diagramWorkers == nil {
			a.diagramWorkers = make(chan struct{}, 2)
		}
		entry = &diagramState{}
		a.diagrams[key] = entry
		workers := a.diagramWorkers
		go func() {
			workers <- struct{}{}
			value, err := limu.Layout(source, &layout.Options{Measurer: limumygo.DefaultFonts, MaxWidth: 680, NoImages: true, NoLinks: true})
			<-workers
			a.post(func() { entry.scene, entry.failed = value, err != nil })
		}()
	}
	if entry.scene == nil || entry.failed {
		markdown.DefaultCodeBlock(c, block)
		return
	}
	theme := style.Kohu()
	if c.Theme().Dark {
		theme = style.KohuDark()
	}
	scale := float64(1)
	if entry.scene.Width > 680 {
		scale = 680 / entry.scene.Width
	}
	limumygo.View(c.Key("mermaid"), entry.scene, &limumygo.Options{Fonts: limumygo.DefaultFonts, Look: style.Look{Theme: theme}, Scale: scale})
}

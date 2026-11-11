package main

import (
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/markdown"
)

func (a *app) renderMarkdown(c *ui.Context, id, text string, options *markdown.Options, streaming bool) {
	c.Key(id)
	if streaming {
		markdown.Stream(c, text, options)
	} else {
		markdown.View(c, text, options)
	}
}

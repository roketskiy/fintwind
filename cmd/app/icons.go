package main

import (
	"strings"

	"fintwind-go/assets"

	"github.com/egoist/mygo/ui"
)

func icon(c *ui.Context, name string, size float32, color ui.Color) ui.Element {
	svg := assets.SVG(name)
	if svg == nil {
		return ui.Text(c, "").Size(size, size)
	}
	return ui.Icon(c, svg).Size(size, size).TextColor(color)
}

// brandIcon draws a company or file-type asset in its own colors, mirroring
// the Rust split between ui::icon (tinted monochrome glyphs) and
// ui::file_icon / model_mark_element's Company arm (untinted images).
// Monochrome UI glyphs keep going through icon().
func brandIcon(c *ui.Context, name string, size float32) ui.Element {
	if name == "company-openai" || name == "company-openai-dark" {
		name = "company-openai"
		if activeAppearance.dark {
			name = "company-openai-dark"
		}
	}
	svg := assets.SVG(name)
	if svg == nil {
		return ui.Text(c, "").Size(size, size)
	}
	return ui.Image(c, svg).Size(size, size)
}

// markIcon draws a model/provider mark: company and file-type assets keep
// their authored colors (Rust renders them as images), while fallback glyphs
// take the tint.
func markIcon(c *ui.Context, name string, size float32, tint ui.Color) ui.Element {
	if strings.HasPrefix(name, "company-") || strings.HasPrefix(name, "file-types/") {
		return brandIcon(c, name, size)
	}
	return icon(c, name, size, tint)
}

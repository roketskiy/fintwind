// Package assets embeds the icon set the client draws.
package assets

import (
	"embed"
	"path"
	"strings"
	"sync"

	"github.com/egoist/mygo/ui"
)

//go:embed icons/*.svg icons/file-types/*.svg
var files embed.FS

var cache sync.Map

// SVG returns a parsed icon, or nil when the name is missing. A name may be a
// bare icon, a Fintwind path like "icons/chevron-down.svg", or a file-type
// icon addressed as "file-types/rust".
func SVG(name string) *ui.SVG {
	if value, ok := cache.Load(name); ok {
		return value.(*ui.SVG)
	}
	key := path.Base(strings.TrimSuffix(name, ".svg"))
	candidates := []string{"icons/" + key + ".svg", "icons/file-types/" + key + ".svg"}
	if strings.HasPrefix(name, "file-types/") {
		candidates[0], candidates[1] = candidates[1], candidates[0]
	}
	var svg *ui.SVG
	for _, candidate := range candidates {
		data, err := files.ReadFile(candidate)
		if err != nil {
			continue
		}
		parsed, err := ui.ParseSVG(data)
		if err != nil {
			continue
		}
		svg = parsed
		break
	}
	if svg == nil {
		return nil
	}
	cache.Store(name, svg)
	return svg
}

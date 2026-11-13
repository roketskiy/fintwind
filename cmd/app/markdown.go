package main

import (
	"crypto/sha256"
	"strings"

	"fintwind-go/internal/diagramview"

	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/markdown"
	"github.com/firede/limu"
	"github.com/firede/limu/layout"
	"github.com/firede/limu/limumygo"
	"github.com/firede/limu/scene"
)

func (a *app) renderMarkdown(c *ui.Context, id, text string, options *markdown.Options, streaming bool) {
	c.Key(id)
	if streaming {
		markdown.Stream(c, text, options)
	} else {
		markdown.View(c, text, options)
	}
}

// Diagram cache quotas. Footprints are bitmap-equivalent estimates
// (width × height × 4) because a laid-out scene stores vectors, not pixels.
// Entries beyond the quotas are evicted least recently seen, so a full cache
// never permanently blocks new diagrams.
const (
	diagramMaxEntryBytes = 8 << 20
	diagramCacheMaxBytes = 64 << 20
	diagramMaxEntries    = 128
	diagramMaxPerSession = 24
	diagramLayoutWorkers = 2
)

type diagramState struct {
	scene   *scene.Scene
	failed  bool
	dead    bool
	session string
	seen    uint64
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
		if a.diagramWorkers == nil {
			a.diagramWorkers = make(chan struct{}, diagramLayoutWorkers)
		}
		entry = &diagramState{session: a.selectedID, seen: a.diagramClock}
		a.diagrams[key] = entry
		workers := a.diagramWorkers
		go func() {
			workers <- struct{}{}
			value, err := limu.Layout(source, &layout.Options{Measurer: limumygo.DefaultFonts, MaxWidth: 680, NoImages: true, NoLinks: true})
			<-workers
			a.post(func() {
				if entry.dead {
					return
				}
				if err != nil || value == nil {
					entry.failed = true
					return
				}
				if diagramBytes(value) > diagramMaxEntryBytes {
					// Keep the failed marker so an oversized source lays out
					// only once; its slot leaves with ordinary eviction.
					entry.failed = true
					return
				}
				entry.scene = value
			})
		}()
		a.trimDiagrams(entry)
	}
	if entry.scene == nil || entry.failed {
		markdown.DefaultCodeBlock(c, block)
		return
	}
	entry.seen = a.diagramClock
	a.diagramViewer.Inline(c.Key("mermaid"), entry.scene, diagramLabels())
}

// trimDiagrams enforces the cache quotas after a new entry joins, evicting
// least recently seen entries. Entries stamped with the current frame's clock
// survive so one paint never evicts the diagrams it is about to show.
func (a *app) trimDiagrams(current *diagramState) {
	for {
		sessionOver := a.sessionDiagramCount(current.session) > diagramMaxPerSession
		globalOver := len(a.diagrams) > diagramMaxEntries || a.diagramCacheBytes() > diagramCacheMaxBytes
		if !sessionOver && !globalOver {
			return
		}
		var victimKey [32]byte
		var victim *diagramState
		for key, entry := range a.diagrams {
			if entry == current || entry.seen == a.diagramClock {
				continue
			}
			if sessionOver && entry.session != current.session {
				continue
			}
			if victim == nil || entry.seen < victim.seen {
				victimKey, victim = key, entry
			}
		}
		if victim == nil {
			return // everything left was painted this frame; transient overshoot
		}
		victim.dead = true
		victim.scene = nil
		delete(a.diagrams, victimKey)
	}
}

// forgetDiagrams drops a deleted session's cached scenes, including any with
// a layout still in flight.
func (a *app) forgetDiagrams(sessionID string) {
	for key, entry := range a.diagrams {
		if entry.session == sessionID {
			entry.dead = true
			entry.scene = nil
			delete(a.diagrams, key)
		}
	}
}

func (a *app) sessionDiagramCount(session string) int {
	count := 0
	for _, entry := range a.diagrams {
		if entry.session == session {
			count++
		}
	}
	return count
}

func (a *app) diagramCacheBytes() uint64 {
	total := uint64(0)
	for _, entry := range a.diagrams {
		if entry.scene != nil {
			total += diagramBytes(entry.scene)
		}
	}
	return total
}

// diagramBytes estimates a scene's footprint as its bitmap-equivalent size.
func diagramBytes(sc *scene.Scene) uint64 {
	return uint64(sc.Width) * uint64(sc.Height) * 4
}

func diagramLabels() diagramview.Labels {
	return diagramview.Labels{Open: tr("View diagram"), ZoomIn: tr("Zoom in"), ZoomOut: tr("Zoom out"), Fit: tr("Fit to window")}
}

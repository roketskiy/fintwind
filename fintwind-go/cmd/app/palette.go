package main

import (
	"sort"
	"strings"

	"github.com/egoist/mygo/ui"
)

// Geometry mirrors render_command_palette in src/app/command_palette.rs,
// with the task's fixed numbers: 560 wide card, 14 radius, border_strong,
// raised background, the shared three-layer lift shadow (cardShadow), a 44
// high search strip, 40 high rows, and 10.5 uppercase group headers. The
// Rust original uses max_w 680, radius 15, a 60 high search strip, and 44
// high result rows; the values below follow the task spec instead.
const (
	paletteWidth      = 560.0
	paletteTopPad     = 64.0
	paletteSearchH    = 44.0
	paletteRowH       = 40.0
	paletteRowR       = 7.0
	paletteSearchText = 14.0
	paletteRowText    = 12.5
	paletteGroupText  = 10.5
	paletteIconSize   = 15.0
	paletteListMaxH   = 380.0
)

// paletteEntry is one filtered row. run must only call existing app methods.
type paletteEntry struct {
	group  string
	label  string
	detail string
	icon   string
	keys   string
	run    func()
}

// paletteItems builds the full command set in source order, then keeps the
// rows whose label/detail/keywords contain every whitespace-separated query
// token (case-insensitive), mirroring visibleModels' token matching in
// modelpicker.go. Groups follow command_palette.rs (Suggested / Tasks /
// Commands / Settings); Tasks is labeled Sessions after the Go mode name.
func (a *app) paletteItems() []paletteEntry {
	entries := []paletteEntry{
		{group: "Suggested", label: "New task", icon: "compose", keys: "new task session chat conversation start ctrl n",
			run: func() { a.newTab() }},
		{group: "Suggested", label: "Add project", icon: "folder", keys: "open add folder project workspace repository repo ctrl o",
			run: func() { a.pickProjectFolder() }},
	}
	entries = append(entries, a.paletteSessionEntries()...)
	entries = append(entries,
		paletteEntry{group: "Commands", label: "Toggle sidebar", icon: "panel-left", keys: "toggle show hide left sidebar history tasks ctrl b",
			run: func() { a.sidebarOpen = !a.sidebarOpen }},
		paletteEntry{group: "Commands", label: "Toggle right panel", icon: "panel-right", keys: "toggle show hide right panel files diff terminal browser",
			run: func() { a.rightOpen = !a.rightOpen }},
		paletteEntry{group: "Commands", label: "Sessions", icon: "message-square-text", keys: "sessions tasks list history",
			run: func() { a.mode = modeSessions }},
		paletteEntry{group: "Commands", label: "Source control", icon: "file-diff", keys: "source control git changes diff files",
			run: func() { a.openSourceControl() }},
		paletteEntry{group: "Commands", label: "Usage", icon: "chart-column", keys: "usage statistics tokens models activity",
			run: func() { a.mode = modeUsage }},
	)
	if a.detail != nil && a.mode != modeSettings && a.mode != modeUsage {
		entries = append(entries, paletteEntry{group: "Commands", label: tr("Context"), icon: "gauge", keys: "context usage tokens window ctrl u 上下文", run: a.toggleContextPanel})
		if a.canCompactContext() {
			entries = append(entries, paletteEntry{group: "Commands", label: tr("Compact context"), icon: "gauge", keys: "compact context summary 压缩 上下文", run: a.requestContextCompaction})
		}
	}
	for _, page := range settingsPages {
		page := page
		entries = append(entries, paletteEntry{
			group: "Settings", label: page.label, icon: page.icon,
			keys: "settings preferences " + strings.ToLower(page.label),
			run: func() {
				a.openSettingsPage(page.key)
			},
		})
	}
	query := strings.ToLower(strings.TrimSpace(a.paletteQuery))
	if query == "" {
		return entries
	}
	tokens := strings.Fields(query)
	filtered := entries[:0:0]
	for _, entry := range entries {
		haystack := strings.ToLower(entry.group + " " + entry.label + " " + entry.detail + " " + entry.keys)
		matched := true
		for _, token := range tokens {
			if !strings.Contains(haystack, token) {
				matched = false
				break
			}
		}
		if matched {
			filtered = append(filtered, entry)
		}
	}
	return filtered
}

// Search covers all started sessions, including collapsed groups and history
// beyond the sidebar's show-more limit.
func (a *app) paletteSessionEntries() []paletteEntry {
	names := make(map[string]string, len(a.projects))
	for i := range a.projects {
		names[a.projects[i].ID] = a.projects[i].Name
	}
	var entries []paletteEntry
	sessions := make([]int, 0, len(a.sessions))
	for i := range a.sessions {
		if session := &a.sessions[i]; !session.Archived && session.HasStarted() {
			sessions = append(sessions, i)
		}
	}
	sort.SliceStable(sessions, func(i, j int) bool {
		return sessionStamp(&a.sessions[sessions[i]]) > sessionStamp(&a.sessions[sessions[j]])
	})
	for _, index := range sessions {
		session := &a.sessions[index]
		if session.ID == "" {
			continue
		}
		id := session.ID
		detail := names[session.ProjectID]
		if id == a.selectedID {
			if detail != "" {
				detail += " · Current"
			} else {
				detail = "Current"
			}
		}
		entries = append(entries, paletteEntry{
			group:  "Sessions",
			label:  session.DisplayTitle(),
			detail: detail,
			icon:   "message-square-text",
			keys:   "task session chat conversation",
			run: func() {
				a.mode = modeSessions
				a.openSession(id)
			},
		})
	}
	return entries
}

// closePalette hides the layer and clears the query so focus cannot stick
// to the unmounted input on the next frame.
func (a *app) closePalette() {
	a.paletteOpen = false
	a.paletteQuery = ""
	a.paletteIndex = 0
}

// runPalette executes one filtered row, closing the layer first like
// execute_command_palette_selection does.
func (a *app) runPalette(items []paletteEntry, index int) {
	if index < 0 || index >= len(items) {
		return
	}
	entry := items[index]
	a.closePalette()
	entry.run()
}

// commandPaletteLayer draws the Ctrl+K palette over the workspace: a full
// window scrim (click closes) with the 560 wide card pinned near the top.
func (a *app) commandPaletteLayer(c *ui.Context) {
	if !a.paletteOpen {
		return
	}
	items := a.paletteItems()
	if a.paletteIndex < 0 {
		a.paletteIndex = 0
	}
	if len(items) > 0 && a.paletteIndex >= len(items) {
		a.paletteIndex = len(items) - 1
	}
	layer := ui.Column(c).Absolute().Left(0).Top(0).Right(0).Bottom(0)
	layer.Children(func() {
		scrim := ui.Row(c).Absolute().Left(0).Top(0).Right(0).Bottom(0).
			Background(ui.RGBA(0, 0, 0, 0.26)).Label(tr("Close command palette"))
		scrim.Children(func() {})
		if scrim.Clicked() {
			a.closePalette()
			return
		}
		ui.Row(c).FillWidth().Justify(ui.Center).Padding(paletteTopPad, 24, 0, 24).
			Children(func() {
				card := ui.Column(c).Width(paletteWidth).Radius(14).
					Border(1, palette.borderStrong).Background(palette.raised).Clip()
				card = cardShadow(card)
				card.Children(func() {
					a.paletteSearch(c, items)
					ui.Row(c).FillWidth().Height(1).Background(palette.border)
					a.paletteResults(c, items)
				})
			})
	})
}

// paletteSearch is the 44 high filter strip. Arrow keys move a.paletteIndex,
// Enter runs the row, Esc closes and clears the query; the input keeps real
// focus while open and unmounts on close so focus cannot stick to it.
func (a *app) paletteSearch(c *ui.Context, items []paletteEntry) {
	ui.Row(c).FillWidth().Height(paletteSearchH).Padding(0, 14).Gap(8).
		AlignItems(ui.Center).Children(func() {
		icon(c, "search", paletteIconSize, palette.textSecondary)
		ui.Row(c).Grow(1).MinWidth(0).Children(func() {
			ui.TextInputBase(c, &a.paletteQuery).FillWidth().
				FontSize(paletteSearchText).FixedLineHeight(20).
				TextColor(palette.text).Placeholder(tr("Type a command…")).
				Label(tr("Command palette")).AutoFocus().
				OnChange(func() { a.paletteIndex = 0 }).
				OnTextKey(func(k ui.TextKey) bool {
					switch k.Key {
					case ui.KeyEscape:
						a.closePalette()
						return true
					case ui.KeyEnter:
						a.runPalette(items, a.paletteIndex)
						return true
					case ui.KeyUp:
						if len(items) > 0 {
							a.paletteIndex = (a.paletteIndex - 1 + len(items)) % len(items)
						}
						return true
					case ui.KeyDown:
						if len(items) > 0 {
							a.paletteIndex = (a.paletteIndex + 1) % len(items)
						}
						return true
					}
					return false
				})
		})
	})
}

// paletteResults renders the grouped rows: 40 high, 12 px insets, 15 icon,
// 12.5 label, selected rows on overlayStrong with a border_strong outline.
func (a *app) paletteResults(c *ui.Context, items []paletteEntry) {
	if len(items) == 0 {
		ui.Column(c).FillWidth().MinHeight(180).Center().Gap(6).Children(func() {
			icon(c, "search", 18, palette.textGhost)
			ui.Text(c, tr("No matching tasks or commands")).FontSize(13).
				TextColor(palette.textSecondary)
			ui.Text(c, tr("Try a task title, project, setting, or command")).FontSize(11.5).
				TextColor(palette.textTertiary)
		})
		return
	}
	ui.Scroll(c).FillWidth().MaxHeight(paletteListMaxH).Children(func() {
		ui.Column(c).FillWidth().Padding(8, 8, 8, 8).Children(func() {
			lastGroup := ""
			for i, item := range items {
				i, item := i, item
				if item.group != lastGroup {
					lastGroup = item.group
					ui.Row(c).FillWidth().Padding(8, 12, 4, 12).Children(func() {
						ui.Text(c, strings.ToUpper(item.group)).
							FontSize(paletteGroupText).TextColor(palette.textTertiary)
					})
				}
				selected := i == a.paletteIndex
				row := ui.ButtonBase(c).Label(item.label).FillWidth().
					Height(paletteRowH).Padding(0, 12).Radius(paletteRowR).
					Gap(10).Justify(ui.Start).AlignItems(ui.Center)
				if row.Hovered() && !selected {
					a.paletteIndex = i
					selected = true
				}
				if selected {
					row.Background(palette.overlayStrong)
					row.Border(1, palette.borderStrong)
				} else if row.Hovered() {
					row.Background(palette.overlay)
				}
				if row.Pressed() {
					row.Opacity(0.85)
				}
				row.Children(func() {
					icon(c, item.icon, paletteIconSize, palette.textSecondary)
					ui.Column(c).Grow(1).MinWidth(0).Justify(ui.Center).Children(func() {
						ui.Text(c, item.label).FontSize(paletteRowText).
							TextColor(choice(selected, palette.text, palette.textSecondary)).
							SingleLine().Ellipsis("...")
						if item.detail != "" {
							ui.Text(c, item.detail).FontSize(11.5).
								TextColor(palette.textTertiary).
								SingleLine().Ellipsis("...")
						}
					})
				})
				if row.Clicked() {
					a.runPalette(items, i)
				}
			}
		})
	})
}

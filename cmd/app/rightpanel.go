package main

import (
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/egoist/mygo/ui"
)

// Tab strip matches the session workspace tab row (tabRow/sessionTab);
// the other values come from app.rs and right_panel.rs.
const (
	rightPanelHeaderHeight     = 36.0
	rightPanelTabHeight        = 30.0
	rightPanelTabMinWidth      = 100.0
	rightPanelTabMaxWidth      = 176.0
	rightPanelFileHeaderHeight = 42.0
	defaultFileTreeWidth       = 184.0
	fileTreeMinWidth           = 140.0
	fileTreeMaxWidth           = 360.0
	fileEditorMinWidth         = 140.0
	treeRowHeight              = 30.0
	treeRowInset               = 8.0
	treeRowDepth               = 16.0
)

// fileTabPrefix marks file-editor tabs inside surfaces: "file:<relative>".
// Rust keeps Files and each open File as separate RightPanelSurface tabs;
// the portable port encodes them as strings because app is frozen.
const fileTabPrefix = "file:"

type treeRow struct {
	Name     string
	Relative string
	Absolute string
	Dir      bool
	Expanded bool
	Depth    int
	Icon     string
}

func (a *app) rightPanel(c *ui.Context) {
	ui.Column(c).Width(a.rightPanelWidthValue).FillHeight().Shrink(0).Background(palette.surface).
		BorderWidth(0, 0, 0, 1).BorderColor(palette.borderStrong).Children(func() {
		// Same strip treatment as the session workspace tab row: a shaded
		// bar behind the tabs, shown only while tabs exist.
		header := ui.Row(c).FillWidth().Height(rightPanelHeaderHeight).Padding(0, 0, 0, 10).
			Gap(6).AlignItems(ui.Center)
		if len(a.surfaces) > 0 {
			header.Background(sessionTabBarBackground())
		}
		header.Children(func() {
			tabs := ui.Row(c).Grow(1).MinWidth(0).FillHeight().Gap(4).AlignItems(ui.Center).Clip()
			tabs.Children(func() {
				for index, kind := range a.surfaces {
					a.surfaceTab(c, index, kind)
				}
			})
			// render_right_panel_header only offers the "+" menu while tabs
			// exist; the empty chooser below is the entry point instead.
			if len(a.surfaces) > 0 {
				a.addSurfaceButton(c)
			}
		})
		if len(a.surfaces) == 0 {
			a.surfaceChooser(c)
			a.rightPanelResizeHandle(c)
			return
		}
		if path, ok := fileTabPath(a.activeKind()); ok {
			a.fileSurface(c, path)
			a.rightPanelResizeHandle(c)
			return
		}
		if _, ok := backgroundKeyFromKind(a.activeKind()); ok {
			a.backgroundSurface(c, a.activeKind())
			a.rightPanelResizeHandle(c)
			return
		}
		switch a.activeKind() {
		case rightPanelContext:
			a.contextSurface(c)
		case rightPanelFiles:
			a.filesSurface(c)
		default:
			kind := a.activeKind()
			if tabID, ok := terminalTabID(kind); ok {
				a.terminalSurface(c, a.lookupNativeTerminalTab(a.selectedID, tabID))
			} else {
				a.filesSurface(c)
			}
		}
		a.rightPanelResizeHandle(c)
	})
}

// surfaceTab draws one open right-panel surface with the session tab's
// look: 30 high, 100-176 wide, 8 px insets, a 7 radius, and an 18 px
// close button; the active tab sits on palette.surface over the shaded
// strip.
func (a *app) surfaceTab(c *ui.Context, index int, kind string) {
	active := index == a.activeSurface
	title := a.surfaceTabTitle(kind)
	tab := ui.ButtonBase(c).Label(title).Height(rightPanelTabHeight).
		MinWidth(rightPanelTabMinWidth).MaxWidth(rightPanelTabMaxWidth).
		Padding(0, 8).Radius(7).Gap(6).AlignItems(ui.Center).Shrink(0)
	if active {
		tab.Background(palette.surface)
	} else {
		if tab.Hovered() {
			tab.Background(palette.overlay)
		}
		if tab.Pressed() {
			tab.Background(palette.overlayStrong)
		}
	}
	tab.Children(func() {
		icon(c, surfaceIcon(kind), 12, palette.textSecondary)
		ui.Row(c).Grow(1).MinWidth(0).Children(func() {
			ui.Text(c, title).FontSize(uiPx(12)).
				TextColor(choice(active, palette.text, palette.textSecondary)).
				SingleLine().Ellipsis("...")
		})
		closeTab := ui.ButtonBase(c).Label(tr("Close")).Size(18, 18).Radius(5).Center().
			Margin(0, -2, 0, -2)
		if closeTab.Hovered() {
			closeTab.Background(palette.overlayStrong)
		}
		if closeTab.Pressed() {
			closeTab.Opacity(0.7)
		}
		closeTab.Children(func() {
			icon(c, "x", 10, palette.textTertiary)
		})
		if closeTab.Clicked() {
			a.closeSurface(index)
		}
	})
	if tab.Clicked() {
		a.activateSurface(index)
	}
}

// addSurfaceMenuOpen holds the header "+" dropdown while it is open. It
// lives at package level because main.go is frozen; the window is single,
// so one flag is enough.
var addSurfaceMenuOpen bool

// addSurfaceButton is the header's "+" control, styled like the session
// tab row's new-tab button: a 28 box, 7 radius and 14 tertiary glyph on
// palette.overlay, opening the same surfaces the chooser cards offer,
// with a check on the single-instance ones already open —
// render_right_panel_header's dropdown_menu.
func (a *app) addSurfaceButton(c *ui.Context) {
	plus := ui.ButtonBase(c).Label(tr("Open a surface")).
		Size(28, 28).Center().Radius(7).Background(palette.overlay)
	if plus.Hovered() {
		plus.Background(palette.overlayStrong)
	}
	if plus.Pressed() {
		plus.Background(palette.overlayStrong)
	}
	plus.Children(func() {
		icon(c, "plus", 14, palette.textTertiary)
	})
	if plus.Clicked() {
		addSurfaceMenuOpen = !addSurfaceMenuOpen
	}
	ui.PopoverBase(c, plus, &addSurfaceMenuOpen, func(panel ui.Element) {
		panel.Background(palette.raised).Border(1, palette.borderStrong).Radius(10).
			Padding(4).Width(240).
			Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.28)).
			Shadow(0, 12, 28, 0, ui.RGBA(0, 0, 0, 0.22)).
			Children(func() {
				for _, kind := range []string{
					rightPanelTerminal, rightPanelFiles,
				} {
					kind := kind
					alreadyOpen := kind != rightPanelTerminal &&
						a.hasSurface(kind)
					row := ui.ButtonBase(c).Label(surfaceLabel(kind)).FillWidth().
						Padding(5, 8).Radius(7).Gap(10).AlignItems(ui.Center)
					if row.Hovered() {
						row.Background(palette.overlay)
					}
					row.Children(func() {
						icon(c, surfaceIcon(kind), 15, palette.textTertiary)
						ui.Row(c).Grow(1).MinWidth(0).Children(func() {
							ui.Text(c, surfaceLabel(kind)).FontSize(uiPx(12.5)).
								TextColor(palette.text).SingleLine().Ellipsis("...")
						})
						if alreadyOpen {
							icon(c, "check", 12, palette.textTertiary)
						}
					})
					if row.Clicked() {
						addSurfaceMenuOpen = false
						a.openSurface(kind)
					}
				}
			})
	})
}

func (a *app) surfaceChooser(c *ui.Context) {
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Center().Padding(0, 20, 32, 20).
		Children(func() {
			ui.Column(c).FillWidth().MaxWidth(420).AlignItems(ui.Center).Children(func() {
				ui.Text(c, tr("Open a surface")).FontSize(uiPx(13)).FontWeight(500).TextColor(palette.text)
				ui.Text(c, tr("Choose what to show in the right panel")).FontSize(uiPx(11)).
					TextColor(palette.textTertiary).Margin(5, 0, 0, 0)
				ui.Row(c).FillWidth().Gap(8).Margin(18, 0, 0, 0).Children(func() {
					a.surfaceCard(c, rightPanelTerminal)
					a.surfaceCard(c, rightPanelFiles)
				})
			})
		})
}

func (a *app) surfaceCard(c *ui.Context, kind string) {
	card := ui.ButtonBase(c).Label(surfaceLabel(kind)).Height(84).Grow(1).
		MinWidth(0).Padding(14).Radius(8).Border(1, palette.borderStrong).
		Background(palette.composer).Justify(ui.Start).AlignItems(ui.Stretch)
	if card.Pressed() {
		card.Background(palette.overlayStrong)
	} else if card.Hovered() {
		card.Background(palette.raised).Border(1, palette.textGhost)
	}
	card.Children(func() {
		ui.Column(c).FillWidth().Grow(1).MinHeight(0).AlignItems(ui.Start).
			Justify(ui.Start).Children(func() {
			icon(c, surfaceIcon(kind), 18, palette.textSecondary)
			ui.Text(c, surfaceLabel(kind)).FontSize(uiPx(12.5)).FontWeight(500).
				TextColor(palette.text).Margin(12, 0, 0, 0)
		})
	})
	if card.Clicked() {
		a.openSurface(kind)
	}
}

func (a *app) surfacePlaceholder(c *ui.Context, title, description string) {
	ui.Column(c).FillWidth().Grow(1).Center().Padding(0, 20, 32, 20).Children(func() {
		ui.Text(c, title).FontSize(uiPx(13)).FontWeight(500).TextColor(palette.text)
		ui.Text(c, description).FontSize(uiPx(11)).FixedLineHeight(uiPx(17)).
			TextColor(palette.textTertiary).MaxWidth(300).TextAlign(ui.Center).
			Margin(6, 0, 0, 0)
	})
}

const (
	rightPanelFiles    = "files"
	rightPanelTerminal = "terminal"
	rightPanelContext  = "context"
	rightPanelHistory  = "history"
)

func surfaceLabel(kind string) string {
	// File tabs show the base name, like RightPanelSurface::File's label.
	if path, ok := fileTabPath(kind); ok {
		return path[strings.LastIndex(path, "/")+1:]
	}
	switch kind {
	case rightPanelContext:
		return tr("Context")
	case rightPanelFiles:
		return tr("Files")
	case rightPanelTerminal:
		return surfaceBaseLabel(kind)
	}
	if strings.HasPrefix(kind, terminalTabPrefix) {
		return surfaceBaseLabel(kind)
	}
	return kind
}

func surfaceBaseLabel(kind string) string {
	switch {
	case kind == rightPanelContext:
		return tr("Context")
	case kind == rightPanelTerminal || strings.HasPrefix(kind, terminalTabPrefix):
		return tr("Terminal")
	}
	return kind
}

func surfaceIcon(kind string) string {
	// File tabs use the workspace file-type icon, like icon_path().
	if path, ok := fileTabPath(kind); ok {
		return fileTypeIcon(path)
	}
	switch {
	case kind == rightPanelContext:
		return "gauge"
	case kind == rightPanelTerminal || strings.HasPrefix(kind, terminalTabPrefix):
		return "terminal"
	case kind == rightPanelFiles:
		return "folder"
	}
	if _, ok := backgroundKeyFromKind(kind); ok {
		return backgroundSurfaceIcon(kind)
	}
	return "file"
}

// surfaceTabTitle names one tab: the live terminal title when the
// tab is known, else the generic surface label.
func (a *app) surfaceTabTitle(kind string) string {
	if tabID, ok := terminalTabID(kind); ok {
		if tab := a.lookupNativeTerminalTab(a.selectedID, tabID); tab != nil && tab.title != "" {
			return tab.title
		}
		return "Terminal"
	}
	if _, ok := backgroundKeyFromKind(kind); ok {
		return a.backgroundSurfaceTitle(kind)
	}
	return surfaceLabel(kind)
}

// fileTabPath splits a "file:<relative>" tab kind into its path.
func fileTabPath(kind string) (string, bool) {
	if !strings.HasPrefix(kind, fileTabPrefix) {
		return "", false
	}
	path := strings.TrimPrefix(kind, fileTabPrefix)
	if path == "" {
		return "", false
	}
	return path, true
}

func (a *app) hasSurface(kind string) bool {
	for _, open := range a.surfaces {
		if open == kind {
			return true
		}
	}
	return false
}

func hasFileTab(surfaces []string) bool {
	for _, kind := range surfaces {
		if _, ok := fileTabPath(kind); ok {
			return true
		}
	}
	return false
}

// nearestFileTab is close_right_panel_surface's fallback: the remaining
// file tab closest to the closed index, lowest index winning ties.
func nearestFileTab(surfaces []string, index int) (int, bool) {
	best := -1
	for candidate, kind := range surfaces {
		if _, ok := fileTabPath(kind); !ok {
			continue
		}
		if best == -1 || abs(candidate-index) < abs(best-index) {
			best = candidate
		}
	}
	return best, best != -1
}

func abs(value int) int {
	if value < 0 {
		return -value
	}
	return value
}

// fittedFileTreeWidth mirrors app.rs: the tree keeps 140..360 px while the
// editor never drops below 140.
func fittedFileTreeWidth(panelWidth, storedWidth float32) float32 {
	maximum := min(fileTreeMaxWidth, panelWidth-fileEditorMinWidth)
	maximum = max(maximum, fileTreeMinWidth)
	if storedWidth != storedWidth {
		return min(max(defaultFileTreeWidth, fileTreeMinWidth), maximum)
	}
	return min(max(storedWidth, fileTreeMinWidth), maximum)
}

// activateSurface selects a tab and points the editor at its file, so tab
// switches and closes keep the editor in sync without main.go changes.
func (a *app) activateSurface(index int) {
	if index < 0 || index >= len(a.surfaces) {
		return
	}
	a.activeSurface = index
	a.syncSelectedFile()
	if a.activeKind() == rightPanelContext {
		a.contextPanel.focusOnce = true
		a.focusComposerOnce = false
	}
}

// syncSelectedFile points the editor at the active file tab, clearing it
// when no file tab is active.
func (a *app) syncSelectedFile() {
	path, ok := fileTabPath(a.activeKind())
	if !ok {
		a.selectedFile = ""
		return
	}
	if a.selectedFile != path {
		a.switchToFile(path)
	}
}

// openSurface shows a surface. Terminal tabs always open fresh,
// like reusable_surface_index, with one surface string per native tab
// ("terminal:<tabID>"); the single-instance kinds reuse
// their tab.
func (a *app) openSurface(kind string) {
	if kind == rightPanelContext {
		if a.detail == nil {
			return
		}
		a.ensureContextSources()
	}
	switch kind {
	case rightPanelTerminal:
		if a.selectedID == "" {
			a.status = tr("Select a task before opening a terminal.")
			return
		}
		tab := a.startNativeTerminal(a.selectedID)
		kind = terminalTabPrefix + tab.tabID
	default:
		for index, open := range a.surfaces {
			if open == kind {
				a.activateSurface(index)
				return
			}
		}
	}
	a.surfaces = append(a.surfaces, kind)
	a.activateSurface(len(a.surfaces) - 1)
	if kind == rightPanelFiles {
		a.refreshTree()
	}
}

// removeSurfaceKind drops the first surface with exactly this kind, closing
// its native resources through closeSurface. Used by tool-driven closes.
func (a *app) removeSurfaceKind(kind string) {
	for index, open := range a.surfaces {
		if open == kind {
			a.closeSurface(index)
			return
		}
	}
}

// closeSurface removes a tab. Closing the active tab selects the previous
// one; a closed file tab hands focus to the nearest remaining file, and
// when none is left the Files tab returns where the file was —
// close_right_panel_surface. Terminal tabs dispose their shell;
// file tabs drop their editor draft.
func (a *app) closeSurface(index int) {
	if index < 0 || index >= len(a.surfaces) {
		return
	}
	kind := a.surfaces[index]
	if kind == rightPanelContext && index == a.activeSurface {
		a.focusComposerOnce = true
	}
	if tabID, ok := terminalTabID(kind); ok {
		a.closeNativeTerminalTab(a.selectedID, tabID)
	} else if path, ok := fileTabPath(kind); ok {
		a.closeNativeFile(a.selectedID, path)
	}
	_, closedFile := fileTabPath(a.surfaces[index])
	wasActive := index == a.activeSurface
	a.surfaces = append(a.surfaces[:index], a.surfaces[index+1:]...)
	if len(a.surfaces) == 0 {
		a.activeSurface = -1
		a.selectedFile = ""
		a.rightOpen = false
		return
	}
	if wasActive && closedFile {
		if nearest, ok := nearestFileTab(a.surfaces, index); ok {
			a.activeSurface = nearest
		} else {
			insertAt := min(index, len(a.surfaces))
			a.surfaces = append(a.surfaces[:insertAt], append([]string{rightPanelFiles}, a.surfaces[insertAt:]...)...)
			a.activeSurface = insertAt
		}
	} else {
		switch {
		case a.activeSurface > index:
			a.activeSurface--
		case wasActive:
			a.activeSurface = max(min(index-1, len(a.surfaces)-1), 0)
		}
		if closedFile && !hasFileTab(a.surfaces) {
			insertAt := min(index, len(a.surfaces))
			a.surfaces = append(a.surfaces[:insertAt], append([]string{rightPanelFiles}, a.surfaces[insertAt:]...)...)
			if a.activeSurface >= insertAt {
				a.activeSurface++
			}
		}
	}
	a.syncSelectedFile()
}

func (a *app) activeKind() string {
	if a.activeSurface < 0 || a.activeSurface >= len(a.surfaces) {
		return ""
	}
	return a.surfaces[a.activeSurface]
}

// filesSurface is render_right_panel_files: a project header over the
// working tree. Open files render as their own tabs beside the tree
// (see fileSurface), so this only draws the tree itself.
func (a *app) filesSurface(c *ui.Context) {
	project := a.selectedProject()
	if project == nil {
		a.rightPanelFilesEmpty(c)
		return
	}
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		header := ui.Row(c).FillWidth().Height(rightPanelFileHeaderHeight).
			Padding(0, 16).Gap(8).AlignItems(ui.Center).
			BorderWidth(0, 0, 1, 0).BorderColor(palette.border)
		header.Children(func() {
			icon(c, "folder", 13, palette.textTertiary)
			ui.Row(c).Grow(1).MinWidth(0).Children(func() {
				ui.Text(c, project.Name).FontSize(uiPx(11.5)).FontWeight(500).
					TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
			})
		})
		a.treeList(c)
	})
}

func (a *app) treeList(c *ui.Context) {
	ui.Scroll(c).FillWidth().Grow(1).Children(func() {
		ui.Column(c).FillWidth().PaddingY(6).Gap(0).Children(func() {
			for index := range a.treeRows {
				a.treeRow(c, &a.treeRows[index])
			}
		})
	})
}

// fileSurface is render_right_panel_file: the open file's editable surface
// with the working tree beside it at the fitted width, like the Rust split.
func (a *app) fileSurface(c *ui.Context, path string) {
	if a.selectedFile != path {
		a.switchToFile(path)
	}
	project := a.selectedProject()
	if project == nil {
		a.rightPanelFilesEmpty(c)
		return
	}
	editor := a.openNativeFile(a.selectedID, path, a.absoluteFilePath(path))
	treeWidth := fittedFileTreeWidth(a.rightPanelWidthValue, a.fileTreeWidth)
	ui.Row(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		a.fileEditorSurface(c, editor)
		tree := ui.Column(c).Width(treeWidth).MinWidth(fileTreeMinWidth).FillHeight().
			Shrink(0).BorderWidth(0, 0, 0, 1).BorderColor(palette.borderStrong)
		tree.Children(func() {
			a.treeList(c)
			a.fileTreeResizeHandle(c)
		})
	})
}

// absoluteFilePath resolves one workspace-relative path against the tree or
// the selected project, on the main thread before any background read.
func (a *app) absoluteFilePath(relative string) string {
	if entry := a.openTreeEntry(relative); entry != nil {
		return entry.Absolute
	}
	if project := a.selectedProject(); project != nil && project.Path != "" {
		return filepath.Join(project.Path, filepath.FromSlash(relative))
	}
	return relative
}

// openFile opens a tree file the way open_right_panel_file does: the first
// file replaces the Files tab in place, later ones append their own tabs.
// The editor content comes from the per-session native file editor, so a
// reopened tab keeps its unsaved draft instead of re-reading.
func (a *app) openFile(entry *treeRow) {
	if entry == nil {
		return
	}
	relative := entry.Relative
	for index, open := range a.surfaces {
		if open == fileTabPrefix+relative {
			a.activateSurface(index)
			return
		}
	}
	if a.activeKind() == rightPanelFiles {
		a.surfaces[a.activeSurface] = fileTabPrefix + relative
	} else {
		a.surfaces = append(a.surfaces, fileTabPrefix+relative)
		a.activeSurface = len(a.surfaces) - 1
	}
	a.refreshTree()
	a.switchToFile(relative)
}

// switchToFile points the editor at a file tab. The native editor keeps its
// draft across switches and only reads on first open, so this never
// overwrites unsaved text with a fresh read.
func (a *app) switchToFile(relative string) {
	a.selectedFile = relative
	if a.selectedID == "" {
		return
	}
	a.openNativeFile(a.selectedID, relative, a.absoluteFilePath(relative))
}

func (a *app) openTreeEntry(path string) *treeRow {
	for index := range a.treeRows {
		if a.treeRows[index].Relative == path {
			return &a.treeRows[index]
		}
	}
	return nil
}

func (a *app) rightPanelFilesEmpty(c *ui.Context) {
	ui.Column(c).FillWidth().Grow(1).Center().Padding(0, 0, 32, 0).Children(func() {
		ui.Text(c, tr("No project open")).FontSize(uiPx(13)).FontWeight(500).TextColor(palette.text)
		ui.Text(c, tr("Open a project to browse its files.")).FontSize(uiPx(11)).
			FixedLineHeight(uiPx(17)).TextColor(palette.textTertiary).MaxWidth(300).TextAlign(ui.Center)
	})
}

func (a *app) rightPanelEmpty(c *ui.Context) {
	ui.Column(c).FillWidth().Grow(1).Center().Children(func() {
		ui.Text(c, tr("Not ported yet")).FontSize(uiPx(13)).TextColor(palette.textMuted)
	})
}

// treeRow draws one file-tree row: 30 high, 8 margins, 8+depth*16 inset.
func (a *app) treeRow(c *ui.Context, entry *treeRow) {
	activeFile, _ := fileTabPath(a.activeKind())
	selected := !entry.Dir && entry.Relative == activeFile
	highlighted := !entry.Dir && a.reviewHighlighted(entry.Relative)
	row := ui.ButtonBase(c).Label(entry.Name).FillWidth().Height(treeRowHeight).
		Margin(0, treeRowInset, 0, treeRowInset).
		Padding(0, treeRowInset, 0, treeRowInset+float32(entry.Depth)*treeRowDepth).
		Radius(6).Gap(6).Justify(ui.Start).AlignItems(ui.Center)
	if row.Pressed() {
		row.Background(palette.overlayStrong)
	} else if row.Hovered() {
		row.Background(palette.overlay)
	} else if selected || highlighted {
		row.Background(palette.overlayStrong)
	}
	row.Children(func() {
		if entry.Dir {
			chevron := "chevron-right"
			if entry.Expanded {
				chevron = "chevron-down"
			}
			icon(c, chevron, 10, palette.textGhost)
		} else {
			ui.Row(c).Size(10, 10).Shrink(0)
			if entry.Icon != "" {
				brandIcon(c, entry.Icon, 14)
			}
		}
		ui.Row(c).Grow(1).MinWidth(0).Children(func() {
			ui.Text(c, entry.Name).FontSize(uiPx(11.5)).
				TextColor(choice(highlighted, palette.text, palette.textSecondary)).
				SingleLine().Ellipsis("...")
		})
	})
	if row.Clicked() {
		if entry.Dir {
			if a.treeExpanded == nil {
				a.treeExpanded = map[string]bool{}
			}
			a.treeExpanded[entry.Absolute] = !a.treeExpanded[entry.Absolute]
			a.refreshTree()
		} else {
			a.openFile(entry)
		}
	}
}

// refreshTree walks the project's working tree off the frame path; the panel
// keeps drawing the previous listing until the new one lands.
func (a *app) refreshTree() {
	project := a.selectedProject()
	if project == nil || project.Path == "" {
		a.treeRows = nil
		return
	}
	root := project.Path
	expanded := make(map[string]bool, len(a.treeExpanded))
	for path, open := range a.treeExpanded {
		expanded[path] = open
	}
	go func() {
		rows := buildTreeRows(root, expanded)
		a.post(func() {
			if a.selectedProject() == nil || a.selectedProject().Path != root {
				return
			}
			a.treeRows = rows
		})
	}()
}

func buildTreeRows(root string, expanded map[string]bool) []treeRow {
	rows := make([]treeRow, 0, 256)
	var visit func(directory, relative string, depth int)
	visit = func(directory, relative string, depth int) {
		entries, err := os.ReadDir(directory)
		if err != nil {
			return
		}
		children := make([]os.DirEntry, 0, len(entries))
		for _, entry := range entries {
			if entry.Name() == ".git" {
				continue
			}
			children = append(children, entry)
		}
		sort.SliceStable(children, func(i, j int) bool {
			leftDir := children[i].IsDir()
			rightDir := children[j].IsDir()
			if leftDir != rightDir {
				return leftDir
			}
			return strings.ToLower(children[i].Name()) < strings.ToLower(children[j].Name())
		})
		for _, entry := range children {
			isDir := entry.IsDir()
			absolute := filepath.Join(directory, entry.Name())
			relativePath := entry.Name()
			if relative != "" {
				relativePath = relative + "/" + entry.Name()
			}
			open := isDir && expanded[absolute]
			iconName := ""
			if !isDir {
				iconName = fileTypeIcon(entry.Name())
			}
			rows = append(rows, treeRow{
				Name:     entry.Name(),
				Relative: relativePath,
				Absolute: absolute,
				Dir:      isDir,
				Expanded: open,
				Depth:    depth,
				Icon:     iconName,
			})
			if open {
				visit(absolute, relativePath, depth+1)
			}
		}
	}
	visit(root, "", 0)
	return rows
}

// fileTypeIcon mirrors right_panel.rs's file_icon_for_path: the name match
// runs on the file name, not the full relative path.
func fileTypeIcon(name string) string {
	if i := strings.LastIndexAny(name, "/\\"); i >= 0 {
		name = name[i+1:]
	}
	lower := strings.ToLower(name)
	named := ""
	switch {
	case strings.HasPrefix(lower, "readme"):
		named = "file-types/readme"
	case strings.HasPrefix(lower, "license") || strings.HasPrefix(lower, "licence") ||
		strings.HasPrefix(lower, "copying"):
		named = "file-types/certificate"
	case strings.HasPrefix(lower, "dockerfile") || strings.HasPrefix(lower, "compose."):
		named = "file-types/docker"
	case lower == "cmakelists.txt" || strings.HasPrefix(lower, "cmake."):
		named = "file-types/cmake"
	case lower == "makefile" || strings.HasPrefix(lower, "makefile.") || lower == "justfile":
		named = "file-types/makefile"
	case lower == "cargo.toml" || lower == "cargo.lock" || lower == "rust-toolchain.toml":
		named = "file-types/rust"
	case lower == "go.mod" || lower == "go.sum" || lower == "go.work":
		named = "file-types/go"
	case lower == "pyproject.toml" || lower == "pipfile" || strings.HasPrefix(lower, "requirements"):
		named = "file-types/python"
	case lower == "bun.lock" || lower == "bun.lockb" || lower == "bunfig.toml":
		named = "file-types/bun"
	case strings.HasPrefix(lower, "pnpm-") || lower == ".pnpmfile.cjs":
		named = "file-types/pnpm"
	case lower == "yarn.lock" || strings.HasPrefix(lower, ".yarnrc"):
		named = "file-types/yarn"
	case lower == "package.json":
		named = "file-types/nodejs"
	case lower == "package-lock.json":
		named = "file-types/npm"
	case strings.HasPrefix(lower, "tsconfig.") || lower == "tsconfig.json":
		named = "file-types/typescript"
	case strings.HasPrefix(lower, "jsconfig.") || lower == "jsconfig.json":
		named = "file-types/javascript"
	case lower == ".gitignore" || lower == ".gitattributes" || lower == ".gitmodules" ||
		lower == ".gitconfig":
		named = "file-types/git"
	case lower == ".editorconfig":
		named = "file-types/editorconfig"
	case strings.HasPrefix(lower, ".env"):
		named = "file-types/settings"
	case strings.HasPrefix(lower, ".prettier") || strings.HasPrefix(lower, "prettier.config."):
		named = "file-types/prettier"
	case strings.HasPrefix(lower, ".eslint") || strings.HasPrefix(lower, "eslint.config."):
		named = "file-types/eslint"
	case strings.HasPrefix(lower, "biome.json"):
		named = "file-types/biome"
	case strings.HasPrefix(lower, ".babel") || strings.HasPrefix(lower, "babel.config."):
		named = "file-types/babel"
	case strings.HasPrefix(lower, ".stylelint") || strings.HasPrefix(lower, "stylelint.config."):
		named = "file-types/stylelint"
	case strings.HasPrefix(lower, "vite.config."):
		named = "file-types/vite"
	case strings.HasPrefix(lower, "vitest.config.") || strings.HasPrefix(lower, "vitest.workspace."):
		named = "file-types/vitest"
	case strings.HasPrefix(lower, "webpack."):
		named = "file-types/webpack"
	case strings.HasPrefix(lower, "rollup.config."):
		named = "file-types/rollup"
	case strings.HasPrefix(lower, "next.config.") || lower == "next-env.d.ts":
		named = "file-types/next"
	case strings.HasPrefix(lower, "nuxt.config.") || lower == ".nuxtrc":
		named = "file-types/nuxt"
	case strings.HasPrefix(lower, "astro.config."):
		named = "file-types/astro"
	case lower == "angular.json" || strings.HasSuffix(lower, ".component.ts"):
		named = "file-types/angular"
	case lower == "nest-cli.json":
		named = "file-types/nest"
	case strings.HasPrefix(lower, "tailwind.config."):
		named = "file-types/tailwindcss"
	case strings.HasPrefix(lower, "svelte.config."):
		named = "file-types/svelte"
	case strings.HasPrefix(lower, "vue.config."):
		named = "file-types/vue"
	case lower == "firebase.json" || lower == ".firebaserc":
		named = "file-types/firebase"
	case lower == "supabase.toml":
		named = "file-types/supabase"
	case strings.HasPrefix(lower, "prisma.config."):
		named = "file-types/prisma"
	case lower == "turbo.json":
		named = "file-types/turborepo"
	case strings.HasPrefix(lower, "deno.json") || lower == "deno.lock":
		named = "file-types/deno"
	case lower == ".gitlab-ci.yml" || lower == ".gitlab-ci.yaml":
		named = "file-types/gitlab"
	case lower == "kustomization.yaml" || lower == "kustomization.yml":
		named = "file-types/kubernetes"
	case lower == "chart.yaml" || lower == "values.yaml":
		named = "file-types/helm"
	case lower == "nginx.conf":
		named = "file-types/nginx"
	case lower == ".nvmrc" || lower == ".node-version":
		named = "file-types/nodejs"
	case lower == "build.gradle" || lower == "settings.gradle" || lower == "gradlew" ||
		lower == "gradlew.bat":
		named = "file-types/gradle"
	case strings.Contains(lower, ".stories.") || strings.Contains(lower, ".story."):
		named = "file-types/storybook"
	case lower == "gemfile" || lower == "gemfile.lock":
		named = "file-types/ruby"
	case lower == "pom.xml":
		named = "file-types/java"
	}
	if named != "" {
		return named
	}
	extension := ""
	if index := strings.LastIndex(lower, "."); index >= 0 {
		extension = lower[index+1:]
	}
	switch extension {
	case "rs":
		return "file-types/rust"
	case "js", "mjs", "cjs":
		return "file-types/javascript"
	case "ts", "mts", "cts":
		return "file-types/typescript"
	case "jsx", "tsx":
		return "file-types/react"
	case "py", "pyi", "pyw":
		return "file-types/python"
	case "go":
		return "file-types/go"
	case "c", "h", "m":
		return "file-types/c"
	case "cc", "cpp", "cxx", "hh", "hpp", "hxx", "mm":
		return "file-types/cpp"
	case "cs":
		return "file-types/csharp"
	case "swift":
		return "file-types/swift"
	case "kt", "kts":
		return "file-types/kotlin"
	case "java", "class":
		return "file-types/java"
	case "rb":
		return "file-types/ruby"
	case "php":
		return "file-types/php"
	case "html", "htm":
		return "file-types/html"
	case "css", "less":
		return "file-types/css"
	case "scss", "sass":
		return "file-types/sass"
	case "json", "jsonc", "jsonl":
		return "file-types/json"
	case "yaml", "yml":
		return "file-types/yaml"
	case "toml", "ini", "cfg", "conf", "config":
		return "file-types/settings"
	case "xml", "xsl", "plist":
		return "file-types/xml"
	case "md", "mdx", "markdown":
		return "file-types/markdown"
	case "sh", "bash", "zsh", "fish":
		return "file-types/console"
	case "ps1", "psm1":
		return "file-types/powershell"
	case "sql", "db", "sqlite", "sqlite3", "csv", "xls", "xlsx":
		return "file-types/database"
	case "png", "jpg", "jpeg", "gif", "webp", "avif", "ico", "tiff":
		return "file-types/image"
	case "svg":
		return "file-types/svg"
	case "pdf":
		return "file-types/pdf"
	case "mp3", "wav", "flac", "ogg", "m4a":
		return "file-types/audio"
	case "mp4", "mov", "avi", "webm", "mkv":
		return "file-types/video"
	case "zip", "gz", "tgz", "bz2", "xz", "7z", "rar", "tar", "jar":
		return "file-types/zip"
	case "wasm", "wat":
		return "file-types/webassembly"
	case "svelte":
		return "file-types/svelte"
	case "vue":
		return "file-types/vue"
	case "tf", "tfvars":
		return "file-types/terraform"
	case "graphql", "gql":
		return "file-types/graphql"
	case "lua":
		return "file-types/lua"
	case "dart":
		return "file-types/dart"
	case "astro":
		return "file-types/astro"
	case "coffee", "cson":
		return "file-types/coffee"
	case "cr":
		return "file-types/crystal"
	case "ex", "exs":
		return "file-types/elixir"
	case "elm":
		return "file-types/elm"
	case "erl", "hrl":
		return "file-types/erlang"
	case "clj", "cljs", "cljc", "edn":
		return "file-types/clojure"
	case "hs", "lhs":
		return "file-types/haskell"
	case "hx", "hxml":
		return "file-types/haxe"
	case "jinja", "jinja2", "j2":
		return "file-types/jinja"
	case "jl":
		return "file-types/julia"
	case "ml", "mli":
		return "file-types/ocaml"
	case "pl", "pm":
		return "file-types/perl"
	case "prisma":
		return "file-types/prisma"
	case "pug", "jade":
		return "file-types/pug"
	case "scala", "sbt", "sc":
		return "file-types/scala"
	case "sol":
		return "file-types/solidity"
	case "tex", "sty", "cls":
		return "file-types/tex"
	case "xaml":
		return "file-types/xaml"
	case "zig":
		return "file-types/zig"
	case "nix":
		return "file-types/nix"
	case "proto":
		return "file-types/proto"
	case "diff", "patch":
		return "file-types/diff"
	case "exe", "dll", "so", "dylib":
		return "file-types/exe"
	case "lock":
		return "file-types/lock"
	}
	return "file-types/file"
}

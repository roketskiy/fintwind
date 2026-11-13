package main

import (
	"unicode/utf8"

	"github.com/egoist/mygo/plugins/terminal"
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/editor"
)

// CrychicUI adapter points (vendor/CrychicUI, MyGo v0.8.3 compat). The pixel
// source stays the Rust right panel: fixed heights (48 header, 28 tabs, 42
// file header), the palette.* ramp, and 10.5/16 mono body text. Library
// pieces slot in only where their chrome can be overridden to those pixels:
//
//   - File Save/Reload/Cancel buttons use componentTextButton
//     (components.go): button.Ghost with Scale(1,1), height 30, padding 11,
//     radius 7, font 12.5 chained over, so kit.Press scale animation and the
//     theme look never leak through. Stable c.Key first, automation .Label
//     second for the non-visual E2E.
//   - File editor uses editor.View directly: code.CodeEditor adds a themed
//     header plus status bar at a fixed t.Space(80) height and cannot fill
//     the panel; the Rust header (42px path row) plus dirty/conflict states
//     stay local. Language highlighting comes from editor.LanguageFor.
//   - Terminal uses terminal.View on the tab's live pty: code.Terminal only
//     renders static lines and cannot carry input, scrollback, or a remote
//     Conn.

// Extra automation labels for file controls.
const (
	nativeFileReloadLabel = "native-file-reload"
	nativeFileCancelLabel = "native-file-cancel"
)

// terminalSurface is one open terminal tab: the live pty filling the
// panel, or the tab's error state. No header: the tab itself carries the
// live shell title. The find bar floats over the top right like kero's,
// and an exited tab keeps its scrollback under a restart bar.
func (a *app) terminalSurface(c *ui.Context, tab *nativeTerminalTab) {
	if tab == nil {
		a.surfacePlaceholder(c, "Terminal", "Start a shell in this workspace")
		return
	}
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Label(nativeTerminalLabel).Children(func() {
		ui.Column(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
			switch {
			case tab.starting || tab.term == nil && tab.err == "":
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, tr("Starting")).FontSize(uiPx(10.5)).TextColor(palette.textMuted)
				})
			case tab.term == nil:
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, tab.err).FontSize(uiPx(10.5)).TextColor(palette.danger).
						Label(nativeTerminalErrorLabel)
				})
			default:
				c.Key(nativeTabKey(tab.tabID))
				terminal.View(c, tab.term).Grow(1).FillWidth()
				if tab.findOpen {
					a.terminalFindBar(c, tab)
				} else if c.Shortcut(ui.Ctrl, ui.KeyF) {
					tab.findOpen, tab.findFocus = true, true
				}
				if tab.exited {
					a.terminalExitedBar(c, tab)
				}
			}
		})
	})
}

// terminalFindBar floats over the tab's top right: the query, its match
// count, and the buttons stepping through matches. Ported from kero's
// findBar; Enter steps forward, Shift+Enter back, Escape closes.
func (a *app) terminalFindBar(c *ui.Context, tab *nativeTerminalTab) {
	term := tab.term
	if term == nil {
		return
	}
	if tab.findQuery != "" && term.SearchStatus().Text != tab.findQuery {
		term.Search(tab.findQuery)
	}
	bar := ui.Row(c).Key(nativeTerminalFindLabel).Absolute().Top(8).Right(12).Height(36).
		Padding(0, 4, 0, 12).Gap(4).AlignItems(ui.Center).Radius(10).
		Background(palette.raised).Border(1, palette.borderStrong).
		Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.28)).Label(nativeTerminalFindLabel)
	bar.Children(func() {
		icon(c, "search", 13, palette.textMuted)
		c.Key("terminal-find-" + tab.tabID)
		in := ui.TextInputBase(c, &tab.findQuery).Width(180).FontSize(uiPx(13)).
			Placeholder(tr("Find")).Bind(&tab.findInput)
		if tab.findFocus {
			tab.findFocus = false
			in.Focus().SetTextSelection(0, utf8.RuneCountInString(tab.findQuery))
		}
		if in.Changed() {
			term.Search(tab.findQuery)
		}
		switch {
		case in.Submitted() && c.Modifiers()&ui.Shift != 0:
			term.SearchPrevious()
		case in.Submitted():
			term.SearchNext()
		}
		count := ""
		switch status := term.SearchStatus(); {
		case tab.findQuery == "":
		case status.Total == 0 && !status.Searching:
			count = tr("No matches")
		case status.Total > 0:
			count = trf("%d of %d", status.Selected, status.Total)
		}
		ui.Text(c, count).FontSize(uiPx(12)).TextColor(palette.textMuted).
			SingleLine().MinWidth(56).TextAlign(ui.End).Margin(0, 4, 0, 0)
		if componentIconButton(c, "terminal-find-prev-"+tab.tabID, "chevron-up", true).Label(tr("Previous match")).Clicked() {
			term.SearchPrevious()
		}
		if componentIconButton(c, "terminal-find-next-"+tab.tabID, "chevron-down", true).Label(tr("Next match")).Clicked() {
			term.SearchNext()
		}
		if componentIconButton(c, "terminal-find-close-"+tab.tabID, "x", true).Label(tr("Close find")).Clicked() {
			a.closeTerminalFind(tab)
		}
	})
	if bar.Shortcut(0, ui.KeyEscape) {
		a.closeTerminalFind(tab)
	}
}

// closeTerminalFind ends the tab's search and returns focus to the shell.
func (a *app) closeTerminalFind(tab *nativeTerminalTab) {
	if tab.term != nil {
		tab.term.EndSearch()
	}
	tab.findOpen, tab.findQuery, tab.findFocus = false, "", false
}

// terminalExitedBar keeps an exited tab's scrollback readable with its
// exit code and a restart, like kero's Exited chip plus restart.
func (a *app) terminalExitedBar(c *ui.Context, tab *nativeTerminalTab) {
	ui.Row(c).FillWidth().Padding(6, 12).Gap(8).AlignItems(ui.Center).
		Background(palette.overlay).BorderWidth(1, 0, 0, 0).BorderColor(palette.border).Children(func() {
		ui.Row(c).Grow(1).MinWidth(0).Children(func() {
			ui.Text(c, trf("Process exited with code %d", tab.exitCode)).FontSize(uiPx(11.5)).
				TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
		})
		if restart := componentTextButton(c, "terminal-restart-"+tab.tabID, tr("Restart"), false).
			Label("native-terminal-restart"); restart.Clicked() {
			a.restartNativeTerminal(tab.sessionID, tab.tabID)
		}
	})
}

// fileEditorSurface is one open file tab: the Rust 42px path header with
// Save state, an explicit disk-conflict dialog, and the editable MyGo editor
// widget filling the rest.
func (a *app) fileEditorSurface(c *ui.Context, ed *nativeFileEditor) {
	ui.Column(c).Grow(1).MinWidth(0).FillHeight().Label(nativeFileEditorLabel).Children(func() {
		header := ui.Row(c).FillWidth().Height(rightPanelFileHeaderHeight).
			Padding(0, 16).Gap(8).AlignItems(ui.Center).
			BorderWidth(0, 0, 1, 0).BorderColor(palette.border)
		header.Children(func() {
			if entry := a.openTreeEntry(ed.relative); entry != nil && entry.Icon != "" {
				icon(c, entry.Icon, 13, palette.textSecondary)
			} else {
				ui.Row(c).Size(13, 13).Shrink(0)
			}
			ui.Row(c).Grow(1).MinWidth(0).Children(func() {
				ui.Text(c, ed.relative).FontSize(uiPx(11)).TextColor(palette.textSecondary).
					SingleLine().Ellipsis("...")
			})
			switch {
			case ed.saving:
				ui.Text(c, tr("Saving")).FontSize(uiPx(11)).TextColor(palette.textMuted)
			case ed.nativeFileDirty():
				if save := componentTextButton(c, "file-save-"+ed.relative, tr("Save"), true).
					Label(nativeFileSaveLabel); save.Clicked() {
					a.saveNativeFile(ed.sessionID, ed.relative)
				}
			}
		})
		if ed.saveErr != "" {
			ui.Row(c).FillWidth().Padding(4, 16).Children(func() {
				ui.Text(c, ed.saveErr).FontSize(uiPx(11)).TextColor(palette.danger).
					Label(nativeFileErrorLabel)
			})
		}
		if ed.hasConflict {
			a.fileConflictDialog(c, ed)
		}
		body := ui.Column(c).FillWidth().Grow(1).MinHeight(0).Background(palette.surface)
		body.Children(func() {
			switch {
			case ed.loading:
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, tr("Loading")).FontSize(uiPx(10.5)).TextColor(palette.textMuted)
				})
			case ed.readErr != "":
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, ed.readErr).FontSize(uiPx(10.5)).TextColor(palette.danger).
						Label(nativeFileErrorLabel)
				})
			case !ed.loaded:
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, tr("No content")).FontSize(uiPx(10.5)).TextColor(palette.textMuted)
				})
			default:
				ed.syncNativeFileBuffer()
				c.Key(nativeTabKey("file-" + ed.relative))
				state := ed.editorState()
				state.ReadOnly = false
				state.Font = ui.Font{Family: monoFont(), Size: codePx(13)}
				state.LineHeight = 20.0 / 13.0
				// The source's code editor menu ends with Quote selection:
				// the editor's own Cut/Copy/Paste stay first, this stages
				// the selection as a composer reference chip.
				state.ContextMenu = func(m *ui.Menu) {
					if m.Item(tr("Quote selection")).Disabled(state.SelectedText() == "").Chosen() {
						a.quoteFileEditorSelection(ed)
					}
				}
				editor.View(c, state).Grow(1).FillWidth().Label(nativeFileEditorLabel)
				ed.syncNativeFileBuffer()
			}
		})
	})
}

// fileConflictDialog is the explicit disk-guard choice: the disk moved under
// an unsaved draft, so nothing is written until the user picks a side.
func (a *app) fileConflictDialog(c *ui.Context, ed *nativeFileEditor) {
	ui.Column(c).FillWidth().Padding(8, 16).Gap(6).
		Background(palette.overlay).BorderWidth(0, 0, 1, 0).BorderColor(palette.border).Children(func() {
		ui.Text(c, tr("The file changed on disk")).FontSize(uiPx(12)).FontWeight(500).
			TextColor(palette.text)
		ui.Text(c, tr("Save would overwrite another change. Pick which copy wins.")).
			FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
		ui.Row(c).FillWidth().Gap(6).Children(func() {
			if reload := componentTextButton(c, "file-reload-"+ed.relative, tr("Reload"), false).
				Label(nativeFileReloadLabel); reload.Clicked() {
				a.reloadNativeFileFromDisk(ed.sessionID, ed.relative)
			}
			if overwrite := componentTextButton(c, "file-overwrite-"+ed.relative, tr("Save anyway"), true).
				Label(nativeFileSaveLabel); overwrite.Clicked() {
				a.saveAnywayNativeFile(ed.sessionID, ed.relative)
			}
			if keep := componentTextButton(c, "file-keep-"+ed.relative, tr("Keep editing"), false).
				Label(nativeFileCancelLabel); keep.Clicked() {
				a.dismissNativeFileConflict(ed.sessionID, ed.relative)
			}
		})
	})
}

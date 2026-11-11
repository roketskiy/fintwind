package main

import (
	"strings"

	"github.com/egoist/mygo/plugins/terminal"
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/editor"
)

// CrychicUI adapter points (vendor/CrychicUI, MyGo v0.8.3 compat). The pixel
// source stays the Rust right panel: fixed heights (48 header, 28 tabs, 42
// file header), the palette.* ramp, and 10.5/16 mono body text. Library
// pieces slot in only where their chrome can be overridden to those pixels:
//
//   - File Save/Reload/Cancel/Go/Share buttons use componentTextButton
//     (components.go): button.Ghost with Scale(1,1), height 30, padding 11,
//     radius 7, font 12.5 chained over, so kit.Press scale animation and the
//     theme look never leak through. Stable c.Key first, automation .Label
//     second for the non-visual E2E.
//   - Address bar stays ui.TextInputBase: input.URLField adds a label row,
//     a scheme prefix, and blur validation the Rust toolbar has no room for.
//   - Browser toolbar stays a local row: desktop.UnifiedToolbar brings
//     theme spacing plus a Tap cue, and its search field is not a URL bar.
//   - File editor uses editor.View directly: code.CodeEditor adds a themed
//     header plus status bar at a fixed t.Space(80) height and cannot fill
//     the panel; the Rust header (42px path row) plus dirty/conflict states
//     stay local. Language highlighting comes from editor.LanguageFor.
//   - Terminal uses terminal.View on the tab's live pty: code.Terminal only
//     renders static lines and cannot carry input, scrollback, or a remote
//     Conn.

// Extra automation labels for the browser and terminal controls.
const (
	nativeBrowserBackLabel    = "native-browser-back"
	nativeBrowserForwardLabel = "native-browser-forward"
	nativeBrowserReloadLabel  = "native-browser-reload"
	nativeBrowserStopLabel    = "native-browser-stop"
	nativeBrowserOpenLabel    = "native-browser-open"
	nativeFileReloadLabel     = "native-file-reload"
	nativeFileCancelLabel     = "native-file-cancel"
)

func (a *app) browserSurface(c *ui.Context, tab *nativeBrowserTab) {
	if tab == nil {
		a.surfacePlaceholder(c, "Browser", "Open a local app or URL")
		return
	}
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Label(nativeBrowserLabel).Children(func() {
		toolbar := ui.Row(c).FillWidth().Height(rightPanelFileHeaderHeight).
			Padding(0, 10).Gap(2).AlignItems(ui.Center).
			BorderWidth(0, 0, 1, 0).BorderColor(palette.border)
		toolbar.Children(func() {
			if back := componentIconButton(c, "browser-back-"+tab.tabID, "arrow-left", tab.view != nil && tab.canBack).
				Label(nativeBrowserBackLabel); back.Clicked() {
				a.nativeBrowserBack(tab)
			}
			if fwd := componentIconButton(c, "browser-fwd-"+tab.tabID, "arrow-right", tab.view != nil && tab.canFwd).
				Label(nativeBrowserForwardLabel); fwd.Clicked() {
				a.nativeBrowserForward(tab)
			}
			if tab.loading && tab.view != nil {
				if stop := componentIconButton(c, "browser-stop-"+tab.tabID, "x", true).
					Label(nativeBrowserStopLabel); stop.Clicked() {
					a.stopNativeBrowser(tab)
				}
			} else if reload := componentIconButton(c, "browser-reload-"+tab.tabID, "rotate-cw", tab.view != nil && tab.committed != "").
				Label(nativeBrowserReloadLabel); reload.Clicked() {
				a.nativeBrowserReload(tab)
			}
			secureURL := tab.committed
			if secureURL == "" {
				secureURL = tab.address
			}
			secureIcon := "globe"
			if strings.HasPrefix(secureURL, "https://") {
				secureIcon = "lock"
			}
			icon(c, secureIcon, 11, palette.textSecondary)
			c.Key("browser-addr-" + tab.tabID)
			addr := ui.TextInputBase(c, &tab.address).Grow(1).MinWidth(0).
				FontSize(12).Label(nativeBrowserAddressLabel)
			if addr.Focused() && c.Shortcut(0, ui.KeyEnter) {
				_ = a.navigateNativeBrowser(tab, tab.address)
			}
			if openExt := componentIconButton(c, "browser-open-"+tab.tabID, "external-link", tab.view != nil && secureURL != "").
				Label(nativeBrowserOpenLabel); openExt.Clicked() {
				a.openNativeBrowserExternal(tab)
			}
		})
		if tab.title != "" {
			ui.Row(c).FillWidth().Padding(0, 8, 0, 8).Children(func() {
				ui.Text(c, tab.title).FontSize(11).TextColor(palette.textSecondary).
					SingleLine().Ellipsis("...").Label(nativeBrowserTitleLabel)
			})
		}
		ui.Column(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
			switch {
			case tab.creating || tab.view == nil && tab.loadErr == "":
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, tr("Starting")).FontSize(10.5).TextColor(palette.textMuted)
				})
			case tab.view == nil:
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, tab.loadErr).FontSize(10.5).TextColor(palette.danger).
						Label(nativeBrowserErrorLabel)
				})
			default:
				c.Key("browser-view-" + tab.tabID)
				ui.WebView(c, tab.view).Grow(1).FillWidth()
			}
		})
	})
}

// terminalSurface is one open terminal tab: the live pty under a 42px
// header, or the tab's error state.
func (a *app) terminalSurface(c *ui.Context, tab *nativeTerminalTab) {
	if tab == nil {
		a.surfacePlaceholder(c, "Terminal", "Start a shell in this workspace")
		return
	}
	ui.Column(c).FillWidth().Grow(1).MinHeight(0).Label(nativeTerminalLabel).Children(func() {
		header := ui.Row(c).FillWidth().Height(rightPanelFileHeaderHeight).
			Padding(0, 16).Gap(8).AlignItems(ui.Center).
			BorderWidth(0, 0, 1, 0).BorderColor(palette.border)
		header.Children(func() {
			icon(c, "terminal", 13, palette.textSecondary)
			title := tab.title
			if title == "" {
				title = tr("Terminal")
			}
			if tab.exited {
				title += tr(" (exited)")
			}
			ui.Row(c).Grow(1).MinWidth(0).Children(func() {
				ui.Text(c, title).FontSize(11).TextColor(palette.textSecondary).
					SingleLine().Ellipsis("...").Label(nativeTerminalTitleLabel)
			})
		})
		ui.Column(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
			switch {
			case tab.starting || tab.term == nil && tab.err == "":
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, tr("Starting")).FontSize(10.5).TextColor(palette.textMuted)
				})
			case tab.term == nil:
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, tab.err).FontSize(10.5).TextColor(palette.danger).
						Label(nativeTerminalErrorLabel)
				})
			default:
				c.Key(nativeTabKey(tab.tabID))
				terminal.View(c, tab.term).Grow(1).FillWidth()
			}
		})
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
				ui.Text(c, ed.relative).FontSize(11).TextColor(palette.textSecondary).
					SingleLine().Ellipsis("...")
			})
			switch {
			case ed.saving:
				ui.Text(c, tr("Saving")).FontSize(11).TextColor(palette.textMuted)
			case ed.nativeFileDirty():
				if save := componentTextButton(c, "file-save-"+ed.relative, tr("Save"), true).
					Label(nativeFileSaveLabel); save.Clicked() {
					a.saveNativeFile(ed.sessionID, ed.relative)
				}
			}
		})
		if ed.saveErr != "" {
			ui.Row(c).FillWidth().Padding(4, 16).Children(func() {
				ui.Text(c, ed.saveErr).FontSize(11).TextColor(palette.danger).
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
					ui.Text(c, tr("Loading")).FontSize(10.5).TextColor(palette.textMuted)
				})
			case ed.readErr != "":
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, ed.readErr).FontSize(10.5).TextColor(palette.danger).
						Label(nativeFileErrorLabel)
				})
			case !ed.loaded:
				ui.Row(c).Padding(6, 16).Children(func() {
					ui.Text(c, tr("No content")).FontSize(10.5).TextColor(palette.textMuted)
				})
			default:
				ed.syncNativeFileBuffer()
				c.Key(nativeTabKey("file-" + ed.relative))
				state := ed.editorState()
				state.ReadOnly = false
				state.Font = ui.Font{Family: monoFont(), Size: codeMetric(13)}
				state.LineHeight = 20.0 / 13.0
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
		ui.Text(c, tr("The file changed on disk")).FontSize(12).FontWeight(500).
			TextColor(palette.text)
		ui.Text(c, tr("Save would overwrite another change. Pick which copy wins.")).
			FontSize(11).TextColor(palette.textSecondary).Wrap()
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

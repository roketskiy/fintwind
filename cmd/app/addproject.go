package main

import (
	"os"
	"path/filepath"
	"strings"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

// Add Project dialog state lives in package-level variables instead of the
// app struct because main.go is off-limits. Single window, so this is fine.
var (
	addProjectOpen        bool
	addProjectPath        string
	addProjectName        string
	addProjectNameTouched bool
	addProjectErr         string
	addProjectBusy        bool
)

// Geometry mirrors render_add_project_dialog and render_add_project_footer in
// src/app/add_project.rs: card max 704x660 rounded 14 on the composer
// surface (L2049-2058), header 48 high with 16 px insets and 14 px medium
// title (L2100-2114), divider with 16 px margins (L2115), body padding 16
// gap 10 (L2147-2155), footer buttons right-aligned with gap 8 (L3414-3457).
// The scrim is black at 0.34 for the (always dark) theme (L2117-2121).
const (
	addProjectCardWidth = 704.0
	addProjectScrimA    = 0.34
)

// openAddProject is the single entry for adding a project: the sidebar's
// "Add project" action and the empty states all funnel through
// pickProjectFolder into here. Typed text is preserved across opens, matching
// the source; only the error and busy states reset.
func (a *app) openAddProject() {
	if addProjectOpen {
		return
	}
	addProjectOpen = true
	addProjectErr = ""
	addProjectBusy = false
	if strings.TrimSpace(addProjectPath) == "" {
		if home, err := os.UserHomeDir(); err == nil && home != "" {
			addProjectPath = home
			addProjectName = addProjectDefaultName(home)
			addProjectNameTouched = false
		}
	}
}

// closeAddProject dismisses the dialog. It is a no-op while a create is in
// flight; the DialogBase dismiss path re-opens instead (see
// addProjectDialog).
func (a *app) closeAddProject() {
	if addProjectBusy {
		return
	}
	addProjectOpen = false
	addProjectBusy = false
}

// browseAddProjectFolder runs the native directory picker behind the
// dialog's Browse button (the same chooser pickProjectFolder used to open
// directly) and fills the path back into the form. The name auto-follows the
// directory base name until the user edits it by hand.
func (a *app) browseAddProjectFolder() {
	if addProjectBusy {
		return
	}
	go func() {
		paths, err := mygo.Dialog.Open(mygo.OpenDialogOptions{
			Parent:    a.win,
			Title:     "Open project folder",
			Directory: true,
		})
		if err != nil || len(paths) == 0 {
			return
		}
		a.post(func() {
			if !addProjectOpen {
				return
			}
			addProjectPath = paths[0]
			if !addProjectNameTouched || strings.TrimSpace(addProjectName) == "" {
				addProjectName = addProjectDefaultName(paths[0])
			}
			addProjectErr = ""
		})
	}()
}

// addProjectDefaultName mirrors confirm_add_project's naming: the directory
// base name, falling back to the "Project" unnamed string.
func addProjectDefaultName(path string) string {
	base := strings.TrimRight(filepath.Base(path), `/\`)
	if base == "" || base == "." {
		return "Project"
	}
	return base
}

// addProjectPathValid reports whether the typed path selects a readable
// folder, without touching the error line. Used for the Create button state.
func addProjectPathValid() bool {
	path := strings.TrimSpace(addProjectPath)
	if path == "" {
		return false
	}
	info, err := os.Stat(path)
	return err == nil && info.IsDir()
}

// submitAddProject validates the form like confirm_add_project does (verified
// folder required; error strings mirror add_project.error_not_found and
// add_project.error_not_directory) and hands the save to addProjectNamed,
// whose post-save callbacks close the dialog.
func (a *app) submitAddProject() {
	if addProjectBusy {
		return
	}
	path := strings.TrimSpace(addProjectPath)
	name := strings.TrimSpace(addProjectName)
	switch {
	case path == "":
		addProjectErr = "No folder selected"
		return
	case !addProjectPathValid():
		if _, err := os.Stat(path); err != nil {
			addProjectErr = "Folder not found: " + path
		} else {
			addProjectErr = "Not a folder: " + path
		}
		return
	}
	if name == "" {
		name = addProjectDefaultName(path)
	}
	if a.client == nil {
		addProjectErr = "The local service is unavailable"
		return
	}
	addProjectErr = ""
	addProjectBusy = true
	a.addProjectNamed(path, name)
}

// addProjectDialog draws the modal over the window via DialogBase, so it can
// be called from any child view: the backdrop centers the card, and backdrop
// click plus Escape dismiss it. Busy creation locks dismissal instead of the
// source's close-confirm step.
func (a *app) addProjectDialog(c *ui.Context) {
	if !addProjectOpen {
		return
	}
	panel := ui.DialogBase(c, &addProjectOpen, func(backdrop, card ui.Element) {
		backdrop.Background(ui.RGBA(0, 0, 0, addProjectScrimA))
		card.Width(addProjectCardWidth).Background(palette.composer).Radius(14)
		card = cardShadow(card)
		card.Children(func() {
			ui.Row(c).FillWidth().Height(48).Padding(0, 16).Gap(8).
				AlignItems(ui.Center).Children(func() {
				icon(c, "folder-plus", 15, palette.text)
				ui.Row(c).Grow(1).MinWidth(0).Children(func() {
					ui.Text(c, tr("Add project")).FontSize(uiPx(14)).FontWeight(500).
						TextColor(palette.text).SingleLine().Ellipsis("...")
				})
				close := ui.ButtonBase(c).Label(tr("Close")).Size(28, 28).
					Radius(7).Center()
				if close.Hovered() {
					close.Background(palette.overlay)
				}
				if close.Pressed() {
					close.Background(palette.overlayStrong)
				}
				close.Children(func() {
					icon(c, "x", 14, palette.textSecondary)
				})
				if close.Clicked() {
					a.closeAddProject()
				}
			})
			ui.Row(c).FillWidth().Height(1).Margin(0, 16, 0, 16).
				Background(palette.border)
			ui.Column(c).FillWidth().Padding(16).Gap(10).Children(func() {
				a.addProjectLocationRow(c)
				a.addProjectPathRow(c)
				a.addProjectNameField(c, "Project name", &addProjectName, true)
				a.addProjectTargetRow(c)
				if addProjectErr != "" {
					ui.Text(c, addProjectErr).FontSize(uiPx(12)).FixedLineHeight(uiPx(17)).
						TextColor(palette.dangerText).Wrap()
				}
				a.addProjectFooter(c)
			})
		})
	})
	_ = panel
	if addProjectBusy {
		addProjectOpen = true
	}
}

// addProjectLocationRow is the source's location block reduced to the Local
// flow: a "Location" label plus a static "This machine" chip (mirroring the
// footer's Local chip). No remote tabs are ported.
func (a *app) addProjectLocationRow(c *ui.Context) {
	ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
		ui.Text(c, tr("Location")).FontSize(uiPx(12)).FontWeight(500).
			TextColor(palette.textSecondary)
		ui.Row(c).Height(28).Padding(0, 10).Radius(7).Gap(7).
			AlignItems(ui.Center).Background(palette.overlay).Children(func() {
			icon(c, "laptop", 12, palette.textTertiary)
			ui.Text(c, tr("This machine")).FontSize(uiPx(12.5)).
				TextColor(palette.textSecondary)
		})
	})
}

// addProjectPathRow is the source's path row with the Go button replaced by
// Browse: the mono path input plus a button opening the native picker.
func (a *app) addProjectPathRow(c *ui.Context) {
	ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
		field := ui.Row(c).Grow(1).MinWidth(0).Height(32).Padding(0, 9).
			Radius(7).Border(1, palette.borderStrong).Background(palette.inset).
			AlignItems(ui.Center)
		field.Children(func() {
			ui.TextInputBase(c, &addProjectPath).FillWidth().Font(monoFontFamily).
				FontSize(uiPx(12.5)).FixedLineHeight(uiPx(17)).TextColor(palette.text).
				Placeholder(tr("Enter a path, or Browse for a folder")).
				Label(tr("Project folder")).AutoFocus().OnTextKey(func(k ui.TextKey) bool {
				if k.Key == ui.KeyEscape {
					a.closeAddProject()
					return true
				}
				if k.Key != ui.KeyEnter || k.Mods != 0 {
					return false
				}
				a.submitAddProject()
				return true
			})
		})
		browse := ui.ButtonBase(c).Label(tr("Browse")).Height(32).Padding(0, 11).
			Radius(7).Border(1, palette.borderStrong).AlignItems(ui.Center)
		if addProjectBusy {
			browse.Opacity(0.45)
		} else {
			if browse.Hovered() {
				browse.Background(palette.overlay)
			}
			if browse.Pressed() {
				browse.Background(palette.overlayStrong)
			}
		}
		browse.Children(func() {
			ui.Text(c, tr("Browse")).FontSize(uiPx(12.5)).TextColor(palette.textSecondary)
		})
		if browse.Clicked() {
			a.browseAddProjectFolder()
		}
	})
}

// addProjectNameField is one labeled input shell; the name defaults to the
// directory base name. Any hand edit stops the Browse autofill.
func (a *app) addProjectNameField(c *ui.Context, label string, value *string, markTouched bool) {
	ui.Column(c).FillWidth().Gap(4).Children(func() {
		ui.Text(c, label).FontSize(uiPx(12)).FontWeight(500).
			TextColor(palette.textSecondary)
		field := ui.Row(c).FillWidth().Height(32).Padding(0, 9).Radius(7).
			Border(1, palette.borderStrong).Background(palette.inset).
			AlignItems(ui.Center)
		field.Children(func() {
			ui.TextInputBase(c, value).FillWidth().FontSize(uiPx(12.5)).
				FixedLineHeight(uiPx(17)).TextColor(palette.text).
				Placeholder(tr("Project name")).Label(label).
				OnTextKey(func(k ui.TextKey) bool {
					if k.Key == ui.KeyEscape {
						a.closeAddProject()
						return true
					}
					if k.Key != ui.KeyEnter || k.Mods != 0 {
						if markTouched {
							addProjectNameTouched = true
						}
						return false
					}
					a.submitAddProject()
					return true
				})
		})
	})
}

// addProjectTargetRow is the footer's target line: the "Add" label plus the
// mono path, or the no_target text while empty.
func (a *app) addProjectTargetRow(c *ui.Context) {
	target := strings.TrimSpace(addProjectPath)
	ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
		ui.Text(c, tr("Add")).FontSize(uiPx(12)).TextColor(palette.textSecondary).Shrink(0)
		ui.Row(c).Grow(1).MinWidth(0).Children(func() {
			if target == "" {
				ui.Text(c, tr("No folder selected")).FontSize(uiPx(12)).
					TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
				return
			}
			ui.Text(c, target).FontSize(uiPx(12)).Font(monoFontFamily).
				TextColor(palette.text).SingleLine().Ellipsis("...")
		})
	})
}

// addProjectFooter is the Cancel plus primary Create row. Create is dimmed
// without a valid folder, shows a spinner while saving, and submits on click.
func (a *app) addProjectFooter(c *ui.Context) {
	valid := addProjectPathValid() && !addProjectBusy
	ui.Row(c).FillWidth().Justify(ui.End).Gap(8).AlignItems(ui.Center).
		Children(func() {
			cancel := ui.ButtonBase(c).Label(tr("Cancel")).Height(30).Padding(0, 11).
				Radius(7).Border(1, palette.borderStrong).AlignItems(ui.Center)
			if addProjectBusy {
				cancel.Opacity(0.45)
			} else {
				if cancel.Hovered() {
					cancel.Background(palette.overlay)
				}
				if cancel.Pressed() {
					cancel.Background(palette.overlayStrong)
				}
			}
			cancel.Children(func() {
				ui.Text(c, tr("Cancel")).FontSize(uiPx(12.5)).TextColor(palette.textSecondary)
			})
			if cancel.Clicked() {
				a.closeAddProject()
			}
			create := ui.ButtonBase(c).Label(tr("Add this folder")).Height(30).
				Padding(0, 12).Radius(7).AlignItems(ui.Center)
			if valid || addProjectBusy {
				create.Background(palette.inverse)
				if create.Hovered() {
					create.Opacity(0.9)
				}
				if create.Pressed() {
					create.Opacity(0.8)
				}
			} else {
				create.Background(palette.overlayStrong)
			}
			create.Children(func() {
				if addProjectBusy {
					c.AnimationFrame()
					icon(c, "loader-circle", 14, palette.onInverse).
						Rotate(float32(c.Now().UnixMilli()%1000) * 0.36)
				}
				color := palette.textGhost
				if valid || addProjectBusy {
					color = palette.onInverse
				}
				label := tr("Add this folder")
				if addProjectBusy {
					label = tr("Adding…")
				}
				ui.Text(c, label).FontSize(uiPx(12.5)).FontWeight(600).TextColor(color)
			})
			if create.Clicked() {
				a.submitAddProject()
			}
		})
}

package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/feedback"
)

type helpPanel uint8

const (
	helpPanelNone helpPanel = iota
	helpPanelChangelog
	helpPanelPerformance
)

type helpState struct {
	menuOpen bool
	panel    helpPanel

	changelog        string
	changelogErr     string
	changelogLoading bool
	changelogLoaded  bool

	perfRows       []helpProcessRow
	perfTotalCPU   *float64
	perfPrivateMB  *float64
	perfIOReadMB   *float64
	perfIOWriteMB  *float64
	perfErr        string
	perfSelected   int
	perfList       ui.ListState
	perfCancel     context.CancelFunc
	perfGeneration uint64
}

func (a *app) helpRailEntry(c *ui.Context) {
	c.Key("mode-rail-info")
	tip := tr("Help")
	entry := ui.ButtonBase(c).Size(modeRailWidth, modeRailWidth).Justify(ui.Center).
		AlignItems(ui.Center).Cursor(ui.CursorPointer).Tooltip(tip).Label(tip)
	entry.Children(func() {
		color := palette.textTertiary
		if entry.Hovered() {
			color = palette.text
		}
		icon(c, "info", 20, color)
	})
	services := c.Services()
	entry.PopoverMenuButton(func(m *ui.Menu) {
		if m.Item(tr("Changelog")).Chosen() {
			a.openHelpChangelog()
		}
		if m.Item(tr("Performance Overhead")).Chosen() {
			a.openHelpPerformance()
		}
		if m.Item(tr("Submit issue")).Chosen() {
			services.OpenURL("https://github.com/roketskiy/fintwind/issues/new")
		}
	}, ui.PopoverMenuButtonOptions{
		At:   ui.AnchorTopStart,
		Self: ui.AnchorBottomStart,
		Open: &a.help.menuOpen,
	})
}

func (a *app) openHelpChangelog() {
	a.paletteOpen = false
	a.help.panel = helpPanelChangelog
	if a.help.changelogLoaded || a.help.changelogLoading {
		return
	}
	a.help.changelogLoading = true
	go func() {
		notes, err := readHelpChangelog()
		a.post(func() {
			a.help.changelogLoading = false
			a.help.changelogLoaded = true
			a.help.changelog = notes
			a.help.changelogErr = ""
			if err != nil {
				a.help.changelogErr = err.Error()
			}
		})
	}()
}

func (a *app) openHelpPerformance() {
	a.paletteOpen = false
	a.help.panel = helpPanelPerformance
	a.help.perfSelected = -1
	a.startHelpSampling()
}

func (a *app) closeHelpPanel() {
	if a.help.panel == helpPanelPerformance {
		a.stopHelpSampling()
		a.help.perfRows = nil
		a.help.perfTotalCPU = nil
		a.help.perfPrivateMB = nil
		a.help.perfIOReadMB = nil
		a.help.perfIOWriteMB = nil
		a.help.perfErr = ""
		a.help.perfSelected = -1
		a.help.perfList = ui.ListState{}
	}
	a.help.panel = helpPanelNone
}

func (a *app) startHelpSampling() {
	if a.help.perfCancel != nil {
		return
	}
	lifetime := a.lifetime
	if lifetime == nil {
		lifetime = context.Background()
	}
	ctx, cancel := context.WithCancel(lifetime)
	a.help.perfCancel = cancel
	a.help.perfGeneration++
	generation := a.help.perfGeneration
	a.help.perfErr = ""
	go func() {
		sampler := newHelpProcessSampler()
		for {
			sample, err := sampler.sample()
			a.post(func() {
				if a.help.perfGeneration != generation || a.help.panel != helpPanelPerformance {
					return
				}
				if err != nil {
					a.help.perfErr = err.Error()
					return
				}
				a.help.perfRows = sample.Rows
				a.help.perfTotalCPU = sample.TotalCPU
				a.help.perfPrivateMB = sample.TotalPrivateMB
				a.help.perfIOReadMB = sample.TotalIOReadMB
				a.help.perfIOWriteMB = sample.TotalIOWriteMB
				a.help.perfErr = ""
			})
			select {
			case <-ctx.Done():
				return
			case <-time.After(time.Second):
			}
		}
	}()
}

func (a *app) stopHelpSampling() {
	if a.help.perfCancel != nil {
		a.help.perfCancel()
		a.help.perfCancel = nil
	}
	a.help.perfGeneration++
}

func (a *app) helpLayer(c *ui.Context) {
	switch a.help.panel {
	case helpPanelChangelog:
		a.helpChangelogModal(c)
	case helpPanelPerformance:
		a.helpPerformanceModal(c)
	}
}

func (a *app) helpChangelogModal(c *ui.Context) {
	open := a.help.panel == helpPanelChangelog
	if !open {
		return
	}
	feedback.Modal(c.Key("help-changelog-modal"), &open, feedback.ModalProps{
		Title:    tr("Changelog"),
		Width:    640,
		NoFooter: true,
	}, func() {
		if version := a.helpVersion(); version != "" && version != "development" {
			ui.Text(c, version).FontSize(uiPx(11)).TextColor(palette.textTertiary).
				Margin(0, 0, 8, 0)
		}
		switch {
		case a.help.changelogLoading:
		case a.help.changelogErr != "":
			ui.Text(c, tr("Changelog unavailable")).FontSize(uiPx(12)).
				TextColor(palette.dangerText).Wrap()
		case a.help.changelog == "":
			ui.Text(c, tr("No release notes yet.")).FontSize(uiPx(12)).
				TextColor(palette.textSecondary)
		default:
			a.renderMarkdown(c, "help-changelog-content", a.help.changelog, &a.mdCompact, false)
		}
	})
	if !open && a.help.panel == helpPanelChangelog {
		a.closeHelpPanel()
	}
}

func (a *app) helpPerformanceModal(c *ui.Context) {
	open := a.help.panel == helpPanelPerformance
	if !open {
		return
	}
	_, windowHeight := c.Size()
	tableHeight := min(float32(480), max(float32(220), windowHeight-240))
	feedback.Modal(c.Key("help-performance-modal"), &open, feedback.ModalProps{
		Title:    tr("Performance Overhead"),
		Width:    940,
		NoFooter: true,
	}, func() {
		columns := []ui.TableColumn{
			{Title: tr("Process"), Width: 250, Fixed: true},
			{Title: "PID", Width: 72, Align: ui.End, Fixed: true},
			{Title: tr("CPU"), Width: 88, Align: ui.End, Fixed: true},
			{Title: tr("Private"), Width: 110, Align: ui.End, Fixed: true},
			{Title: tr("Working set"), Width: 110, Align: ui.End, Fixed: true},
			{Title: tr("I/O read"), Width: 118, Align: ui.End, Fixed: true},
			{Title: tr("I/O write"), Width: 118, Align: ui.End, Fixed: true},
		}
		a.help.perfList.Key = func(i int) any {
			row := a.help.perfRows[i]
			return fmt.Sprintf("%d:%d", row.PID, row.CreateTime)
		}
		a.help.perfList.Selected = &a.help.perfSelected
		ui.Table(c.Key("help-process-table"), &a.help.perfList, columns, len(a.help.perfRows), func(rowIndex, column int) {
			row := a.help.perfRows[rowIndex]
			switch column {
			case 0:
				ui.Text(c, row.Name).SingleLine().Ellipsis("…")
			case 1:
				ui.Textf(c, "%d", row.PID).FontFeatures("tnum")
			case 2:
				ui.Text(c, helpFormatCPU(row.CPUPercent)).FontFeatures("tnum")
			case 3:
				ui.Text(c, helpFormatValue(row.PrivateMB, "MB")).FontFeatures("tnum")
			case 4:
				ui.Text(c, helpFormatValue(row.WorkingSetMB, "MB")).FontFeatures("tnum")
			case 5:
				ui.Text(c, helpFormatValue(row.IOReadMB, "MB")).FontFeatures("tnum")
			case 6:
				ui.Text(c, helpFormatValue(row.IOWriteMB, "MB")).FontFeatures("tnum")
			}
		}).FillWidth().Height(tableHeight)
		if a.help.perfErr != "" {
			ui.Text(c, tr("Could not sample processes")+": "+a.help.perfErr).
				FontSize(uiPx(11)).TextColor(palette.dangerText).Wrap().
				Padding(6, 0, 0, 0)
		}
		if name := a.selectedProcessName(); name != "" {
			ui.Text(c, name).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).
				SingleLine().Ellipsis("…").Padding(6, 0, 0, 0)
		}
		a.helpPerformanceTotals(c)
	})
	if !open && a.help.panel == helpPanelPerformance {
		a.closeHelpPanel()
	}
}

func (a *app) helpPerformanceTotals(c *ui.Context) {
	ui.Row(c).FillWidth().Wrap().Gap(16).AlignItems(ui.Baseline).
		Padding(8, 0, 0, 0).Children(func() {
		ui.Text(c, tr("Total")).FontSize(uiPx(11)).FontWeight(500).
			TextColor(palette.textSecondary)
		helpTotal(c, tr("CPU"), helpFormatCPU(a.help.perfTotalCPU))
		helpTotal(c, tr("Private"), helpFormatValue(a.help.perfPrivateMB, "MB"))
		helpTotal(c, tr("I/O read"), helpFormatValue(a.help.perfIOReadMB, "MB"))
		helpTotal(c, tr("I/O write"), helpFormatValue(a.help.perfIOWriteMB, "MB"))
	})
}

func helpTotal(c *ui.Context, label, value string) {
	ui.Row(c).AlignItems(ui.Baseline).Gap(4).Children(func() {
		ui.Text(c, label).FontSize(uiPx(11)).TextColor(palette.textTertiary)
		ui.Text(c, value).FontSize(uiPx(11)).FontFeatures("tnum").
			TextColor(palette.textSecondary)
	})
}

func (a *app) helpVersion() string {
	if a.client == nil {
		return ""
	}
	return strings.TrimSpace(a.client.Version())
}

func (a *app) selectedProcessName() string {
	if a.help.perfSelected < 0 || a.help.perfSelected >= len(a.help.perfRows) {
		return ""
	}
	return a.help.perfRows[a.help.perfSelected].Name
}

func readHelpChangelog() (string, error) {
	var candidates []string
	if executable, err := os.Executable(); err == nil {
		candidates = append(candidates, filepath.Join(filepath.Dir(executable), "CHANGELOG.md"))
	}
	if cwd, err := os.Getwd(); err == nil {
		candidates = append(candidates, filepath.Join(cwd, "CHANGELOG.md"))
	}
	for _, candidate := range candidates {
		data, err := os.ReadFile(candidate)
		if err != nil {
			continue
		}
		return releasedChangelog(string(data)), nil
	}
	return "", fmt.Errorf("CHANGELOG.md is unavailable")
}

func releasedChangelog(source string) string {
	var output strings.Builder
	inRelease := false
	for _, line := range strings.Split(source, "\n") {
		line = strings.TrimSuffix(line, "\r")
		if strings.HasPrefix(line, "## [") {
			rest := strings.TrimPrefix(line, "## [")
			version, _, found := strings.Cut(rest, "]")
			inRelease = found && version != "" && version != "unreleased"
			if inRelease {
				output.WriteString("## ")
				output.WriteString(version)
				output.WriteByte('\n')
			}
			continue
		}
		if !inRelease {
			continue
		}
		if strings.HasPrefix(line, "[") && strings.Contains(line, "]:") {
			continue
		}
		output.WriteString(line)
		output.WriteByte('\n')
	}
	return strings.TrimSpace(output.String())
}

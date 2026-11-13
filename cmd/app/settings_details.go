package main

import (
	"fmt"
	"os/exec"
	"strings"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
	lobechoice "github.com/lobehub/lobe-mygo/pkg/choice"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

func (a *app) settingsCustomProviderDetail(c *ui.Context) {
	st := &a.settings.backend
	var selected *settingsProvider
	for i := range st.providers {
		if st.providers[i].ID == st.providerSelected {
			selected = &st.providers[i]
			break
		}
	}
	if selected == nil {
		mailDetailPlaceholder(c, "bot", "Select a provider")
		return
	}
	provider := *selected
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24).Gap(14).Children(func() {
			formTitle(c, settingsProviderName(provider))
			ui.Text(c, provider.ID).FontSize(uiPx(11)).TextColor(palette.textSecondary)
			mailEnabledBadge(c, provider.Enabled, choice(provider.Enabled, "Enabled", "Disabled"))
			ui.Row(c).Gap(8).Children(func() {
				componentTextButton(c, "provider-edit", tr("Edit"), false).Disabled(st.provider.writing).OnClick(func() { a.beginSettingsProvider(&provider) })
				componentTextButton(c, "provider-toggle", choice(provider.Enabled, tr("Disable"), tr("Enable")), false).Disabled(st.provider.writing || st.provider.uncertain).OnClick(func() {
					roster := append([]settingsProvider{}, st.providers...)
					for i := range roster {
						if roster[i].ID == provider.ID {
							roster[i].Enabled = !provider.Enabled
						}
					}
					a.commitSettingsProviders(roster, provider.ID, "")
				})
				componentTextButton(c, "provider-remove", tr("Remove"), false).Disabled(st.provider.writing || st.provider.uncertain).OnClick(func() { st.removeConfirm = "provider:" + provider.ID })
			})
			if st.removeConfirm == "provider:"+provider.ID {
				ui.Text(c, tr("Remove this provider configuration?")).FontSize(uiPx(12)).TextColor(palette.text)
				ui.Row(c).Gap(8).Children(func() {
					componentTextButton(c, "provider-remove-cancel", tr("Cancel"), false).OnClick(func() { st.removeConfirm = "" })
					componentTextButton(c, "provider-remove-confirm", tr("Remove provider"), true).Disabled(st.provider.writing).OnClick(func() {
						roster := make([]settingsProvider, 0, len(st.providers))
						for _, item := range st.providers {
							if item.ID != provider.ID {
								roster = append(roster, item)
							}
						}
						a.commitSettingsProviders(roster, "", "")
					})
				})
			}
		})
	})
}

func (a *app) settingsSkillDetail(c *ui.Context) {
	st := &a.settings.backend
	visible := skillsVisible(st.skills, a.settings.catalog.skillQuery, a.settings.catalog.skillSource)
	selected := skillsEffectiveSelection(visible, st.skillSelected)
	if selected == nil {
		if !st.skill.loaded {
			mailStatusRow(c, tr("Scanning skill folders…"))
			return
		}
		ui.Column(c).FillWidth().Grow(1).MinHeight(0).Justify(ui.Center).AlignItems(ui.Center).Gap(10).Padding(40, 40).Children(func() {
			ui.Row(c).Size(44, 44).Radius(11).Background(palette.overlay).Justify(ui.Center).AlignItems(ui.Center).Children(func() {
				icon(c, "package", 21, palette.textTertiary)
			})
			ui.Text(c, tr("No skills yet")).FontSize(uiPx(13)).FontWeight(500).TextColor(palette.text)
			ui.Text(c, tr("A skill is a folder with a SKILL.md — reusable instructions any agent can load, like a deploy runbook or a review checklist. Skills installed for Claude Code, Codex, and every other agent show up here, invoked as /name.")).FontSize(uiPx(11.5)).FixedLineHeight(uiPx(17)).TextColor(palette.textSecondary).Wrap()
		})
		return
	}
	skill := *selected
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 20, 24).Gap(0).Children(func() {
			ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
				mailDetailTile(c, skillIconName(skill), 18)
				ui.Column(c).Grow(1).MinWidth(0).Children(func() {
					ui.Row(c).FillWidth().Gap(7).AlignItems(ui.Center).Children(func() {
						name := ui.Text(c, skill.Name).FontSize(uiPx(15)).FontWeight(500).SingleLine().Ellipsis("...")
						if skill.Enabled {
							name.TextColor(palette.text)
						} else {
							name.TextColor(palette.textSecondary)
						}
						if !skill.Enabled {
							mailEnabledBadge(c, false, tr("Disabled"))
						}
					})
					ui.Text(c, skillScopeCaption(skill)).FontSize(uiPx(10.5)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...").Margin(2, 0, 0, 0)
				})
				c.Key("skill-enabled-" + skill.key())
				enabled := skill.Enabled
				toggleLabel := strings.ReplaceAll(tr("Enable %{name}"), "%{name}", skill.Name)
				if skill.Enabled {
					toggleLabel = strings.ReplaceAll(tr("Disable %{name}"), "%{name}", skill.Name)
				}
				lobechoice.Switch(c, &enabled, lobechoice.SwitchProps{Size: theme.Small}).Label(toggleLabel).Disabled(st.skill.writing || st.skill.uncertain).OnChange(func() {
					a.toggleSettingsSkill(skill, enabled)
				})
			})
			description := skill.Description
			if strings.TrimSpace(description) == "" {
				description = tr("No description")
			}
			ui.Text(c, description).FontSize(uiPx(11.5)).FixedLineHeight(uiPx(17)).TextColor(palette.textSecondary).Wrap().Margin(14, 0, 0, 0)
			a.skillInfoTable(c, skill)
			if skill.Duplicates > 0 {
				ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Margin(12, 0, 0, 0).Children(func() {
					icon(c, "alert", 11, palette.warning)
					ui.Text(c, skillDuplicateLabel(skill.Duplicates)).FontSize(uiPx(10)).TextColor(palette.warningText).Wrap()
				})
			}
			ui.Row(c).FillWidth().Wrap().Gap(8).AlignItems(ui.Center).Margin(16, 0, 0, 0).Children(func() {
				skillFile := ""
				if len(skill.Installs) > 0 {
					skillFile = skill.Installs[0].SkillFile
				}
				componentTextButton(c, "skill-open", tr("Open SKILL.md"), false).Disabled(st.skill.writing || skillFile == "").OnClick(func() {
					if skillFile != "" {
						_ = mygo.Shell.OpenPath(skillFile)
					}
				})
				componentTextButton(c, "skill-reveal", tr("Show in File Manager"), false).Disabled(st.skill.writing || skillFile == "").OnClick(func() {
					if skillFile == "" {
						return
					}
					_ = exec.Command("explorer.exe", "/select,", skillFile).Start()
				})
				copyID := "skill-copy-" + fmt.Sprint(skill.RowKey)
				copied := a.copiedMessages[copyID] != 0
				copyLabel := tr("Copy Path")
				if copied {
					copyLabel = tr("Copied")
				}
				componentTextButton(c, "skill-copy", copyLabel, false).Disabled(st.skill.writing).OnClick(func() {
					c.WriteClipboard(skill.key())
					a.showCopied(copyID)
				})
				ui.Row(c).Grow(1).MinWidth(0)
				componentTextButton(c, "skill-remove", tr("Delete"), false).Disabled(st.skill.writing || st.skill.uncertain).OnClick(func() { st.skillTrashConfirm = true })
			})
			if st.skillTrashConfirm {
				ui.Text(c, strings.ReplaceAll(tr("Move these %{count} selected installs to Trash?"), "%{count}", fmt.Sprint(len(skill.Installs)))).FontSize(uiPx(12)).TextColor(palette.text).Wrap().Margin(8, 0, 0, 0)
				ui.Row(c).FillWidth().Gap(8).Margin(8, 0, 0, 0).Children(func() {
					componentTextButton(c, "skill-remove-cancel", tr("Cancel"), false).OnClick(func() { st.skillTrashConfirm = false })
					componentTextButton(c, "skill-remove-confirm", tr("Move to Trash"), true).Disabled(st.skill.writing).OnClick(func() { a.trashSettingsSkill(skill) })
				})
			}
			if strings.TrimSpace(skill.Body) != "" {
				ui.Column(c).FillWidth().Margin(18, 0, 0, 0).Padding(14, 0, 0, 0).BorderWidth(1, 0, 0, 0).BorderColor(palette.border).Children(func() {
					ui.Text(c, "SKILL.md").Font(monoFont()).FontSize(uiPx(9.5)).TextColor(palette.textMuted)
					ui.Row(c).FillWidth().Margin(10, 0, 0, 0).Children(func() {
						a.renderMarkdown(c, "skill:"+skill.key(), skill.Body, &a.mdCompact, false)
					})
				})
			}
		})
	})
}

// skillInfoTable mirrors the GPUI detail info table: Invoke, one Location
// line per copy, Contents, Updated, and Tools over divider hairlines.
func (a *app) skillInfoTable(c *ui.Context, skill settingsSkill) {
	type infoRow struct {
		label, value string
		mono         bool
		size         float32
	}
	rows := []infoRow{{label: tr("Invoke"), value: "/" + skill.Name, mono: true, size: 10.5}}
	grouped := len(skill.Installs) > 1
	for _, install := range skill.Installs {
		label := tr("Location")
		if grouped {
			label = skillSourceLabel(install.Source)
		}
		rows = append(rows, infoRow{label: label, value: skillCompactDir(install.Dir), mono: true, size: 10})
	}
	rows = append(rows, infoRow{label: tr("Contents"), value: skillContentsLabel(skill)})
	if updated := skillUpdatedLabel(skill.ModifiedAt); updated != "" {
		rows = append(rows, infoRow{label: tr("Updated"), value: updated})
	}
	if skill.AllowedTools != nil && strings.TrimSpace(*skill.AllowedTools) != "" {
		rows = append(rows, infoRow{label: tr("Tools"), value: *skill.AllowedTools, mono: true, size: 10})
	}
	ui.Column(c).FillWidth().Margin(16, 0, 0, 0).Children(func() {
		for i, row := range rows {
			last := i+1 == len(rows)
			rowBox := ui.Row(c).FillWidth().Padding(8, 0).Gap(12).AlignItems(ui.Start)
			if !last {
				rowBox.BorderWidth(0, 0, 1, 0).BorderColor(palette.border)
			}
			rowBox.Children(func() {
				ui.Text(c, row.label).Width(84).Shrink(0).FontSize(uiPx(10.5)).TextColor(palette.textTertiary)
				value := ui.Text(c, row.value).Grow(1).MinWidth(0).FontSize(uiPx(10.5)).TextColor(palette.textSecondary).Wrap()
				if row.mono {
					size := row.size
					if size == 0 {
						size = 10.5
					}
					value.Font(monoFont()).FontSize(uiPx(size))
				}
			})
		}
	})
}

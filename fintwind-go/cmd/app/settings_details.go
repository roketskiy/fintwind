package main

import (
	"fmt"
	"github.com/egoist/mygo/ui"
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
				componentTextButton(c, "provider-probe", tr("Probe models"), false).Disabled(st.provider.writing).OnClick(func() { a.probeSettingsProvider(provider.ID) })
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
			settingsOperationStatus(c, st.provider, "")
		})
	})
}

func (a *app) settingsSkillDetail(c *ui.Context) {
	st := &a.settings.backend
	var selected *settingsSkill
	for i := range st.skills {
		if st.skills[i].key() == st.skillSelected {
			selected = &st.skills[i]
			break
		}
	}
	if selected == nil {
		mailDetailPlaceholder(c, "package", "Select a skill to preview it")
		return
	}
	skill := *selected
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24).Gap(14).Children(func() {
			formTitle(c, skill.Name)
			ui.Text(c, skill.Description).FontSize(uiPx(12.5)).TextColor(palette.textSecondary).Wrap()
			ui.Text(c, skill.Scope+tr(" · ")+skill.Project).FontSize(uiPx(11)).TextColor(palette.textTertiary)
			for _, install := range skill.Installs {
				ui.Text(c, install.Source+tr(" · ")+install.SkillFile).Font(monoFont()).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
			}
			ui.Row(c).Gap(8).Children(func() {
				componentTextButton(c, "skill-toggle", choice(skill.Enabled, tr("Disable"), tr("Enable")), false).Disabled(st.skill.writing || st.skill.uncertain).OnClick(func() { a.mutateSettingsSkill(skill, false) })
				componentTextButton(c, "skill-remove", tr("Move to Trash"), false).Disabled(st.skill.writing || st.skill.uncertain).OnClick(func() { st.skillTrashConfirm = true })
			})
			if st.skillTrashConfirm {
				ui.Text(c, fmt.Sprintf(tr("Move these %d selected installs to Trash?"), len(skill.Installs))).FontSize(uiPx(12)).TextColor(palette.text)
				ui.Row(c).Gap(8).Children(func() {
					componentTextButton(c, "skill-remove-cancel", tr("Cancel"), false).OnClick(func() { st.skillTrashConfirm = false })
					componentTextButton(c, "skill-remove-confirm", tr("Move to Trash"), true).Disabled(st.skill.writing).OnClick(func() { a.mutateSettingsSkill(skill, true) })
				})
			}
			settingsOperationStatus(c, st.skill, "")
			ui.Text(c, skill.Body).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(19)).TextColor(palette.text).Wrap()
		})
	})
}

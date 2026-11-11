package main

import (
	"fmt"

	"github.com/egoist/mygo/ui"
)

type settingsProviderModelDraft struct {
	key           uint64
	id, context   string
	input, output map[string]bool
}

func (st *settingsCatalogUIState) addProviderModel() {
	st.pfModelSerial++
	st.pfModels = append(st.pfModels, &settingsProviderModelDraft{
		key: st.pfModelSerial, input: map[string]bool{"text": true}, output: map[string]bool{"text": true},
	})
}

func (a *app) settingsProviderModels(c *ui.Context) {
	st := &a.settings.catalog
	if !st.pfModelsReady {
		st.pfModelsReady = true
		st.addProviderModel()
	}
	ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
		ui.Text(c, tr("Models")).Grow(1).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
		componentTextButton(c, "provider-model-add", tr("Add model"), false).Height(26).
			OnClick(func() { st.addProviderModel() })
	})
	if len(st.pfModels) == 0 {
		mailStatusRow(c, "No models added")
		return
	}
	c.Key("provider-form-models")
	ui.List(c, &st.pfModelList, len(st.pfModels)).FillWidth().Height(float32(min(len(st.pfModels)*142, 426))).
		Border(1, palette.border).Radius(9).
		ItemKey(func(index int) any { return st.pfModels[index].key }).
		Rows(func(row ui.ListRow) {
			c := row.Context
			draft := st.pfModels[row.Index]
			key := fmt.Sprintf("provider-model-%d", draft.key)
			ui.Column(c).FillWidth().Padding(10).Gap(6).Children(func() {
				ui.Row(c).FillWidth().Gap(8).AlignItems(ui.End).Children(func() {
					ui.Column(c).Grow(1).MinWidth(0).Children(func() {
						settingsFormField(c, key+"-id", tr("Model ID"), &draft.id, "model-id")
					})
					ui.Column(c).Width(120).Shrink(0).Children(func() {
						settingsFormField(c, key+"-context", tr("Context window"), &draft.context, "Optional")
					})
					componentIconButton(c, key+"-remove", "trash", true).Label(tr("Remove model")).Tooltip(tr("Remove model")).
						OnClick(func() {
							for index, candidate := range st.pfModels {
								if candidate == draft {
									st.pfModels = append(st.pfModels[:index], st.pfModels[index+1:]...)
									return
								}
							}
						})
				})
				settingsModalityRow(c, key+"-input", "Input", []string{"text", "image", "audio", "video", "pdf"}, draft.input)
				settingsModalityRow(c, key+"-output", "Output", []string{"text", "image", "audio", "video"}, draft.output)
			})
		})
}

func settingsModalityRow(c *ui.Context, key, label string, options []string, selected map[string]bool) {
	ui.Row(c).FillWidth().Gap(4).AlignItems(ui.Center).FocusGroup(ui.Horizontal).Children(func() {
		ui.Text(c, label).Width(40).Shrink(0).FontSize(uiPx(10)).TextColor(palette.textTertiary)
		for _, option := range options {
			control := componentTextButton(c, key+"-"+option, option, false).
				Height(22).Padding(0, 6).Radius(5).FontSize(uiPx(10))
			if selected[option] {
				control.Background(palette.overlayStrong).TextColor(palette.text)
			}
			control.Label(label + " " + option).OnClick(func() { selected[option] = !selected[option] })
		}
	})
}

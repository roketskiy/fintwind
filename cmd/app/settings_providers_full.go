package main

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
)

type jsonNumber = json.Number

// Effective selection: stored id while it resolves, else OpenCode.
func (a *app) effectiveProviderID() string {
	st := &a.settings.backend
	id := st.providerSelected
	if id == "" || id == "opencode" {
		return "opencode"
	}
	if isBuiltinSelection(id) {
		return id
	}
	for _, provider := range st.providers {
		if provider.ID == id {
			return id
		}
	}
	return "opencode"
}

func (a *app) selectedBuiltinCatalogID() string {
	id := a.settings.backend.providerSelected
	if isBuiltinSelection(id) {
		return strings.TrimPrefix(id, "builtin:")
	}
	return ""
}

func (a *app) providerIsAuthorized(id string) bool {
	for _, integration := range a.settings.backend.integrations {
		if integration.ID == id && integration.Connected {
			return true
		}
	}
	return false
}

func (a *app) providerNeedsAuth(id string) bool {
	for _, integration := range a.settings.backend.integrations {
		if integration.ID == id && integration.Connected && integration.NeedsAuth {
			return true
		}
	}
	return false
}

func (a *app) providerEnvNames(id string) []string {
	for _, integration := range a.settings.backend.integrations {
		if integration.ID == id {
			return integration.EnvNames
		}
	}
	return nil
}

func (a *app) providerSupportsKey(id string) bool {
	hasIntegrations := len(a.settings.backend.integrations) > 0
	for _, integration := range a.settings.backend.integrations {
		if integration.ID == id {
			return integration.SupportsKey
		}
	}
	if hasIntegrations {
		return false
	}
	if provider := a.builtinProvider(id); provider != nil {
		return provider.API != nil
	}
	return true
}

func (a *app) providerOAuthMethods(id string) []settingsOAuthMethod {
	for _, integration := range a.settings.backend.integrations {
		if integration.ID == id {
			return integration.OAuthMethods
		}
	}
	return nil
}

func (a *app) builtinProvider(id string) *modelsDevProvider {
	if a.settings.backend.builtinProviders == nil {
		return nil
	}
	return a.settings.backend.builtinProviders.get(id)
}

func (a *app) selectedCustomProvider() *settingsProvider {
	id := a.settings.backend.providerSelected
	for i := range a.settings.backend.providers {
		if a.settings.backend.providers[i].ID == id {
			provider := a.settings.backend.providers[i]
			return &provider
		}
	}
	return nil
}

func (a *app) selectProvider(id string) {
	st := &a.settings.backend
	st.providerSelected = id
	st.providerRenaming = false
	st.providerKeyRevealed = false
	st.removeConfirm = ""
	st.providerModelEditor = nil
	if st.providerBuiltinCheckID != strings.TrimPrefix(id, "builtin:") {
		st.providerBuiltinCheck = nil
		st.providerBuiltinCheckID = ""
	}
	a.loadProviderFields(id)
}

func (a *app) loadProviderFields(id string) {
	// Key draft is loaded on selection; base URL edits go through the draft form.
	st := &a.settings.backend
	st.providerKeyDraft = ""
	for _, provider := range st.providers {
		if provider.ID == id {
			if options, ok := provider.Entry["options"].(map[string]any); ok {
				if key, ok := options["apiKey"].(string); ok {
					st.providerKeyDraft = key
				}
			}
			if options, ok := provider.Entry["settings"].(map[string]any); ok {
				if key, ok := options["apiKey"].(string); ok {
					st.providerKeyDraft = key
				}
			}
			break
		}
	}
}

func (a *app) resetProvidersPage() {
	st := &a.settings.backend
	a.cancelProviderAdd()
	st.providerRenaming = false
	st.providerKeyRevealed = false
	st.removeConfirm = ""
	st.providerModelEditor = nil
	st.providerBuiltinCheck = nil
	st.providerBuiltinCheckID = ""
	if id := st.providerSelected; id != "" {
		a.loadProviderFields(id)
	}
	a.loadSettingsProviders()
	a.ensureBuiltinProviders()
}

func (a *app) ensureBuiltinProviders() {
	st := &a.settings.backend
	if st.builtinProviders == nil && !st.builtinLoading {
		st.builtinLoading = true
		st.builtinGeneration++
		gen := st.builtinGeneration
		go func() {
			builtin, _ := fetchModelsDevProvidersWithFallback(nil)
			table := fetchModelsDevCatalogWithFallback(st.modelsTable)
			a.post(func() {
				st := &a.settings.backend
				if st.builtinGeneration != gen {
					return
				}
				st.builtinLoading = false
				if builtin != nil {
					st.builtinProviders = builtin
				}
				if table != nil {
					st.modelsTable = table
				}
			})
		}()
	}
	if !st.integration.loading && !st.integration.loaded {
		a.loadSettingsIntegrations()
	} else if !st.integration.loading {
		a.refreshSettingsIntegrations()
	}
}

func (a *app) clearProviderProbeResults(id string) {
	st := &a.settings.backend
	delete(st.providerConnectivity, id)
	for key := range st.providerLatency {
		if strings.HasPrefix(key, id+"\x00") {
			delete(st.providerLatency, key)
		}
	}
}

// --- list pane ---

func (a *app) providersListPaneFull(c *ui.Context) {
	st := &a.settings.catalog
	backend := &a.settings.backend
	mailPaneHeader(c, "Providers")
	ui.Column(c).FillWidth().Padding(6, 8, 0, 8).Gap(1).Children(func() {
		mailSectionLabel(c, tr("Built-in"), true)
		selected := a.effectiveProviderID()
		a.mailRow(c, "OpenCode", !st.providerAdding && selected == "opencode", func() {
			a.cancelProviderAdd()
			a.selectProvider("opencode")
		}, func() {
			providerTile(c, "opencode", !st.providerAdding && selected == "opencode")
		}, func() {
			mailStatusDot(c, a.modelProbe != nil && a.modelProbe.Installed)
		})
		if backend.builtinProviders != nil {
			for _, provider := range backend.builtinProviders.sorted() {
				if !a.providerIsAuthorized(provider.ID) {
					continue
				}
				id := builtinSelectionID(provider.ID)
				provider := provider
				a.mailRow(c, provider.Name, !st.providerAdding && selected == id, func() {
					a.cancelProviderAdd()
					a.selectProvider(id)
				}, func() {
					providerTile(c, provider.Name, !st.providerAdding && selected == id)
				}, func() {
					mailStatusDot(c, !a.providerNeedsAuth(provider.ID))
				})
			}
		}
		mailSectionLabel(c, tr("Custom providers"), false)
		settingsOperationStatus(c, backend.provider, "")
		if backend.provider.loaded && len(backend.providers) == 0 {
			mailStatusRow(c, tr("No custom providers yet"))
		}
	})
	c.Key("settings-custom-provider-list")
	ui.List(c, &backend.providerList, len(backend.providers)).FillWidth().Grow(1).MinHeight(0).ItemKey(func(i int) any { return backend.providers[i].ID }).Rows(func(row ui.ListRow) {
		provider, c := backend.providers[row.Index], row.Context
		selected := a.effectiveProviderID() == provider.ID && !a.settings.catalog.providerAdding
		ui.Column(c).FillWidth().Padding(0, 8).Children(func() {
			a.mailRow(c, settingsProviderName(provider), selected, func() {
				a.cancelProviderAdd()
				a.selectProvider(provider.ID)
			}, func() { providerTile(c, settingsProviderName(provider), selected) }, func() {
				mailStatusDot(c, provider.Enabled)
			})
		})
	})
	ui.Column(c).FillWidth().Padding(0, 8, 8, 8).Gap(4).Children(func() {
		if mailAddRow(c, tr("Add provider"), st.providerAdding) {
			a.beginProviderForm()
		}
	})
}

// --- detail dispatch ---

func (a *app) providersDetailFull(c *ui.Context) {
	st := &a.settings.catalog
	backend := &a.settings.backend
	if st.providerAdding {
		a.providerAddForm(c)
		return
	}
	if catalogID := a.selectedBuiltinCatalogID(); catalogID != "" {
		if provider := a.builtinProvider(catalogID); provider != nil {
			a.builtinProviderDetail(c, *provider)
			return
		}
	}
	if backend.providerSelected != "" && backend.providerSelected != "opencode" {
		if provider := a.selectedCustomProvider(); provider != nil {
			a.customProviderDetail(c, *provider)
			return
		}
	}
	a.opencodeProviderDetail(c)
}

func (a *app) opencodeProviderDetail(c *ui.Context) {
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
				mailDetailTile(c, "bot", 17)
				ui.Column(c).Grow(1).MinWidth(0).Children(func() {
					ui.Text(c, tr("OpenCode")).FontSize(uiPx(15)).FontWeight(500).TextColor(palette.text)
					ui.Text(c, tr("Built-in · managed by the OpenCode CLI")).FontSize(uiPx(10.5)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
				})
				status := tr("Not checked")
				installed := a.modelProbe != nil && a.modelProbe.Installed
				if a.modelProbeLoad {
					status = tr("Checking")
				} else if a.modelProbe != nil {
					if installed {
						status = tr("Installed")
					} else {
						status = tr("Not installed")
					}
				}
				mailEnabledBadge(c, installed, status)
			})
			providerInfoCard(c, "opencode-login", "key", "Login information", func() { a.providerRefreshButton(c) }, func() {
				ui.Text(c, tr("fintwind drives agent CLIs installed on this computer. Install or sign in with each agent's own CLI, then refresh")).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).Wrap().Margin(14, 0, 0, 0)
				ui.Row(c).FillWidth().Gap(8).Margin(12, 0, 0, 0).AlignItems(ui.Center).Children(func() {
					icon(c, "info", 12, palette.textTertiary)
					ui.Text(c, tr("The endpoint and API key are managed by the opencode CLI (opencode auth login); this roster is OpenCode's own configuration file.")).FontSize(uiPx(10.5)).TextColor(palette.textSecondary).Wrap()
				})
			})
			providerInfoCard(c, "opencode-models", "package", "Model information", nil, func() {
				a.providerModelSection(c)
			})
		})
	})
}

// --- custom detail ---

func (a *app) customProviderDetail(c *ui.Context, provider settingsProvider) {
	st := &a.settings.backend
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
				mailDetailTile(c, "bot", 17)
				ui.Column(c).Grow(1).MinWidth(0).Children(func() {
					if st.providerRenaming {
						settingsTextInput(c, "provider-rename", tr("Name"), &st.providerRename, 220, settingsProviderName(provider))
					} else {
						ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
							ui.Text(c, settingsProviderName(provider)).FontSize(uiPx(15)).FontWeight(500).TextColor(palette.text).SingleLine().Ellipsis("...")
							if componentIconButton(c, "provider-rename-open", "pencil", true).Label(tr("Rename")).Clicked() {
								st.providerRenaming = true
								st.providerRename = settingsProviderName(provider)
							}
						})
					}
					ui.Text(c, provider.ID).FontSize(uiPx(10.5)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
				})
				a.providerEnabledSwitch(c, provider)
				if componentIconButton(c, "provider-remove", "trash", true).Label(tr("Delete provider")).Disabled(st.provider.writing).Clicked() {
					st.removeConfirm = "provider:" + provider.ID
				}
			})
			if st.providerRenaming {
				ui.Row(c).Gap(8).Children(func() {
					if componentTextButton(c, "provider-rename-save", tr("Save"), true).Clicked() {
						a.confirmProviderRename(provider.ID)
					}
					if componentTextButton(c, "provider-rename-cancel", tr("Cancel"), false).Clicked() {
						st.providerRenaming = false
					}
				})
			}
			providerInfoCard(c, "provider-login", "key", "Login information", func() {
				if componentTextButton(c, "provider-probe", tr("Test connection"), false).Disabled(st.provider.writing).Clicked() {
					a.probeCustomConnectivity(provider.ID)
				}
			}, func() {
				a.customProviderFields(c, provider)
				a.providerLatencyControls(c, provider)
				if st.removeConfirm == "provider:"+provider.ID {
					ui.Text(c, tr("Remove this provider configuration?")).FontSize(uiPx(12)).TextColor(palette.text)
					ui.Row(c).Gap(8).Children(func() {
						if componentTextButton(c, "provider-remove-cancel", tr("Cancel"), false).Clicked() {
							st.removeConfirm = ""
						}
						if componentTextButton(c, "provider-remove-confirm", tr("Delete provider"), true).Disabled(st.provider.writing).Clicked() {
							a.deleteCustomProvider(provider.ID)
						}
					})
				}
				if state, ok := st.providerConnectivity[provider.ID]; ok {
					if state.testing {
						ui.Text(c, tr("Testing…")).FontSize(uiPx(11)).TextColor(palette.textSecondary)
					} else if state.reachable {
						ui.Text(c, trf("Reachable · %s · %d models", formatProbeLatency(time.Duration(state.latencyMs)*time.Millisecond), state.models)).FontSize(uiPx(11)).TextColor(palette.textSecondary)
					} else if state.err != "" {
						ui.Text(c, trf("Unreachable: %s", state.err)).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
					}
				}
			})
			providerInfoCard(c, "provider-models", "package", "Model information", func() {
				componentTextButton(c, "provider-fetch", tr("Fetch models"), false).Disabled(st.provider.writing).OnClick(func() { a.fetchCustomModels(provider.ID) })
				componentTextButton(c, "provider-model-add", tr("Add model"), false).Disabled(st.provider.writing).OnClick(func() {
					st.providerModelEditor = &providerModelEditorState{add: true, input: map[string]bool{"text": true}, output: map[string]bool{"text": true}}
				})
			}, func() { a.customProviderModelsSection(c, provider) })
			settingsOperationStatus(c, st.provider, "")
		})
	})
}

func (a *app) customProviderFields(c *ui.Context, provider settingsProvider) {
	st := &a.settings.backend
	native := provider.Native
	optionsKey := "options"
	if native {
		optionsKey = "settings"
	}
	options, _ := provider.Entry[optionsKey].(map[string]any)
	if options == nil {
		options = map[string]any{}
	}
	baseURL, _ := options["baseURL"].(string)
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, tr("Base URL")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
		settingsTextInput(c, "provider-base-url", tr("Base URL"), &baseURL, 0, "https://api.example.com/v1")
		if baseURL != settingsProviderBaseURL(provider.Entry, native) {
			ui.Row(c).Gap(8).Children(func() {
				if componentTextButton(c, "provider-base-url-save", tr("Save"), true).Clicked() {
					a.saveCustomBaseURL(provider.ID, baseURL)
				}
			})
		}
	})
	// API key with reveal/save/logout/account selector parity.
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, tr("API Key")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
		authorized := a.providerIsAuthorized(provider.ID)
		a.providerPasswordInput(c, provider, authorized)
		dirty := st.providerKeyDraft != settingsProviderAPIKey(provider.Entry, native)
		if dirty || authorized {
			ui.Row(c).Gap(8).Children(func() {
				if dirty {
					componentTextButton(c, "provider-key-save", tr("Save"), true).Disabled(st.provider.writing || strings.TrimSpace(st.providerKeyDraft) == "").OnClick(func() {
						a.saveCustomProviderKey(provider.ID, strings.TrimSpace(st.providerKeyDraft))
					})
					componentTextButton(c, "provider-key-hide", tr("Cancel"), false).OnClick(func() { a.loadProviderFields(provider.ID) })
				}
				if authorized {
					componentTextButton(c, "provider-logout", tr("Log out"), false).Disabled(st.auth.busy).OnClick(func() { a.disconnectSettingsIntegration(provider.ID) })
				}
			})
		}
		// Account selector when multiple key accounts exist and no draft key.
		for _, integration := range st.integrations {
			if integration.ID != provider.ID || len(integration.KeyAccounts) <= 1 || dirty {
				continue
			}
			active := tr("Account")
			for _, account := range integration.KeyAccounts {
				if account.Active {
					if account.Label != "" {
						active = account.Label
					} else {
						active = account.ID
					}
				}
			}
			ui.Text(c, tr("Account")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
			a.settingsChoice(c, "custom-provider-account", tr("Account"), active, formPickerWidth, providerAccountOptions(integration), func(option settingsOption) {
				for _, account := range integration.KeyAccounts {
					label := account.Label
					if label == "" {
						label = account.ID
					}
					if option.value == account.ID || option.label == label {
						a.activateSettingsKeyAccount(provider.ID, account.ID)
						return
					}
				}
			})
		}
	})
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, tr("API format")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
		current := settingsProviderFormatForEntry(provider.Entry, native)
		a.settingsChoice(c, "provider-format", tr("API format"), current, formPickerWidth, providerFormatOptions(), func(option settingsOption) {
			a.setCustomProviderFormat(provider.ID, option.value)
		})
	})
}

func providerAccountOptions(integration settingsIntegration) []settingsOption {
	var out []settingsOption
	for _, account := range integration.KeyAccounts {
		label := account.Label
		if label == "" {
			label = account.ID
		}
		out = append(out, settingsOption{label: label, value: account.ID})
	}
	return out
}

func (a *app) customProviderModelsSection(c *ui.Context, provider settingsProvider) {
	st := &a.settings.backend
	models := providerModelsList(provider)
	if editor := st.providerModelEditor; editor != nil && editor.add {
		a.modelEditorRow(c, provider.ID)
	}
	if len(models) == 0 {
		mailStatusRow(c, tr("No models yet. Add one to make this provider usable in chat."))
		return
	}
	ui.Column(c).FillWidth().Gap(4).Children(func() {
		for index, model := range models {
			index, model := index, model
			if editor := st.providerModelEditor; editor != nil && !editor.add && editor.index == index {
				a.modelEditorRow(c, provider.ID)
				continue
			}
			name := model.id
			if model.name != nil && strings.TrimSpace(*model.name) != "" {
				name = *model.name
			}
			edit := func() {
				st.providerModelEditor = &providerModelEditorState{index: index, id: model.id, name: name, context: formatContextPtr(model.contextWindow), input: sliceToSet(model.input, "text"), output: sliceToSet(model.output, "text")}
			}
			input, output := a.providerModelModalities(model.id, provider.ID, model.input, model.output)
			providerModelListRow(c, provider.ID+"/"+model.id, model.id, name, provider.ID, model.contextWindow, input, output, false, edit, func() {
				componentIconButton(c, fmt.Sprintf("model-edit-%d", index), "pencil", true).Label(tr("Edit model")).Disabled(st.provider.writing).OnClick(edit)
				componentIconButton(c, fmt.Sprintf("model-delete-%d", index), "trash", true).Label(tr("Delete model")).Disabled(st.provider.writing).OnClick(func() { a.deleteCustomModel(provider.ID, index) })
			})
		}
	})
}

type providerModelView struct {
	id            string
	name          *string
	contextWindow *uint64
	outputLimit   *uint64
	input         []string
	output        []string
}

func providerModelsList(provider settingsProvider) []providerModelView {
	entryModels, _ := provider.Entry["models"].(map[string]any)
	ids := make([]string, 0, len(entryModels))
	for id := range entryModels {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	var out []providerModelView
	for _, id := range ids {
		spec, _ := entryModels[id].(map[string]any)
		view := providerModelView{id: id}
		if spec != nil {
			if name, _ := spec["name"].(string); name != "" {
				n := name
				view.name = &n
			}
			if limit, ok := spec["limit"].(map[string]any); ok {
				view.contextWindow = jsonNumberToUint(limit["context"])
				view.outputLimit = jsonNumberToUint(limit["output"])
			}
			modKey := "modalities"
			if provider.Native {
				modKey = "capabilities"
			}
			if modalities, ok := spec[modKey].(map[string]any); ok {
				view.input = stringList(modalities["input"])
				view.output = stringList(modalities["output"])
			}
		}
		out = append(out, view)
	}
	return out
}

func (a *app) modelEditorRow(c *ui.Context, providerID string) {
	st := &a.settings.backend
	editor := st.providerModelEditor
	if editor == nil {
		return
	}
	ui.Column(c.Key("provider-model-editor")).FillWidth().Padding(14).Gap(12).Radius(8).Background(palette.overlay).Border(1, palette.border).Children(func() {
		ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
			providerModelMark(c, editor.id, editor.name, providerID)
			ui.Text(c, choice(editor.add, tr("Add model"), tr("Edit model"))).FontSize(uiPx(12)).FontWeight(500).TextColor(palette.text)
		})
		settingsFormField(c, "provider-model-id", tr("Model ID"), &editor.id, "deepseek-chat")
		ui.Row(c).FillWidth().Wrap().Gap(12).Children(func() {
			ui.Column(c).Grow(1).Basis(200).MinWidth(0).Children(func() {
				settingsFormField(c, "provider-model-name", tr("Name"), &editor.name, tr("Optional"))
			})
			ui.Column(c).Grow(1).Basis(140).MinWidth(0).Children(func() {
				settingsFormField(c, "provider-model-context", tr("Context window"), &editor.context, "128K / 1M")
			})
		})
		settingsModalityRow(c, "model-editor-input", "Input", providerInputModalities, editor.input)
		settingsModalityRow(c, "model-editor-output", "Output", providerOutputModalities, editor.output)
		ui.Row(c).FillWidth().Gap(8).Justify(ui.End).Children(func() {
			valid := strings.TrimSpace(editor.id) != "" && (strings.TrimSpace(editor.context) == "" || parseContextWindow(editor.context) != nil)
			if componentTextButton(c, "provider-model-confirm", tr("Save"), true).Disabled(!valid || st.provider.writing).Clicked() {
				a.confirmModelEditor(providerID)
			}
			if componentTextButton(c, "provider-model-cancel", tr("Cancel"), false).Clicked() {
				st.providerModelEditor = nil
			}
		})
	})
}

// --- actions ---

func (a *app) toggleCustomProvider(id string) {
	st := &a.settings.backend
	roster := append([]settingsProvider{}, st.providers...)
	for i := range roster {
		if roster[i].ID == id {
			roster[i].Enabled = !roster[i].Enabled
		}
	}
	a.commitSettingsProviders(roster, id, "")
}

func (a *app) deleteCustomProvider(id string) {
	st := &a.settings.backend
	roster := make([]settingsProvider, 0, len(st.providers))
	for _, item := range st.providers {
		if item.ID != id {
			roster = append(roster, item)
		}
	}
	st.removeConfirm = ""
	a.clearProviderProbeResults(id)
	a.commitSettingsProviders(roster, "", "")
	if st.providerSelected == id {
		a.selectProvider("opencode")
	}
}

func (a *app) confirmProviderRename(id string) {
	st := &a.settings.backend
	name := strings.TrimSpace(st.providerRename)
	st.providerRenaming = false
	if name == "" {
		return
	}
	roster := append([]settingsProvider{}, st.providers...)
	for i := range roster {
		if roster[i].ID == id {
			entry := cloneJSONMap(roster[i].Entry)
			entry["name"] = name
			roster[i].Entry = entry
		}
	}
	a.commitSettingsProviders(roster, id, "")
}

func (a *app) saveCustomBaseURL(id, baseURL string) {
	st := &a.settings.backend
	roster := append([]settingsProvider{}, st.providers...)
	for i := range roster {
		if roster[i].ID == id {
			entry := cloneJSONMap(roster[i].Entry)
			key := "options"
			if roster[i].Native {
				key = "settings"
			}
			options := jsonObject(entry, key)
			options["baseURL"] = strings.TrimSpace(baseURL)
			roster[i].Entry = entry
		}
	}
	a.clearProviderProbeResults(id)
	a.commitSettingsProviders(roster, id, "")
}

func (a *app) saveCustomProviderKey(id, key string) {
	st := &a.settings.backend
	if strings.TrimSpace(key) == "" {
		return
	}
	roster := append([]settingsProvider{}, st.providers...)
	for i := range roster {
		if roster[i].ID == id {
			entry := cloneJSONMap(roster[i].Entry)
			keyName := "options"
			if roster[i].Native {
				keyName = "settings"
			}
			// Keep the roster free of secrets; the key goes to the server store.
			if options, ok := entry[keyName].(map[string]any); ok {
				delete(options, "apiKey")
			}
			roster[i].Entry = entry
		}
	}
	st.providerKeyRevealed = false
	a.commitSettingsProviders(roster, id, strings.TrimSpace(key))
	a.post(func() {
		st := &a.settings.backend
		st.providerKeyDraft = ""
	})
}

func (a *app) setCustomProviderFormat(id, format string) {
	st := &a.settings.backend
	roster := append([]settingsProvider{}, st.providers...)
	for i := range roster {
		if roster[i].ID == id {
			entry := cloneJSONMap(roster[i].Entry)
			if roster[i].Native {
				entry["package"] = settingsProviderPackage(format, true)
			} else {
				entry["npm"] = settingsProviderPackage(format, false)
			}
			roster[i].Entry = entry
		}
	}
	a.commitSettingsProviders(roster, id, "")
}

func (a *app) deleteCustomModel(providerID string, index int) {
	st := &a.settings.backend
	roster := append([]settingsProvider{}, st.providers...)
	for i := range roster {
		if roster[i].ID != providerID {
			continue
		}
		entry := cloneJSONMap(roster[i].Entry)
		models, _ := entry["models"].(map[string]any)
		if models == nil {
			return
		}
		ids := make([]string, 0, len(models))
		for id := range models {
			ids = append(ids, id)
		}
		sort.Strings(ids)
		if index < 0 || index >= len(ids) {
			return
		}
		if len(ids) == 1 {
			st.provider.err = tr("Add at least one model before saving the provider.")
			return
		}
		delete(models, ids[index])
		entry["models"] = models
		roster[i].Entry = entry
	}
	a.commitSettingsProviders(roster, providerID, "")
}

func (a *app) confirmModelEditor(providerID string) {
	st := &a.settings.backend
	editor := st.providerModelEditor
	if editor == nil {
		return
	}
	modelID := strings.TrimSpace(editor.id)
	if modelID == "" {
		return
	}
	models := providerModelsList(settingsProvider{ID: providerID, Entry: providerEntryByID(st.providers, providerID)})
	for i, model := range models {
		if model.id == modelID && (editor.add || i != editor.index) {
			st.provider.err = tr("This model ID already exists")
			return
		}
	}
	contextText := strings.TrimSpace(editor.context)
	var contextWindow *uint64
	if contextText != "" {
		contextWindow = parseContextWindow(contextText)
		if contextWindow == nil {
			st.provider.err = tr("The context window format is not recognized.")
			return
		}
	}
	roster := append([]settingsProvider{}, st.providers...)
	for i := range roster {
		if roster[i].ID != providerID {
			continue
		}
		entry := cloneJSONMap(roster[i].Entry)
		modelsMap := jsonObject(entry, "models")
		if !editor.add {
			oldIDs := make([]string, 0, len(modelsMap))
			for id := range modelsMap {
				oldIDs = append(oldIDs, id)
			}
			sort.Strings(oldIDs)
			if editor.index >= 0 && editor.index < len(oldIDs) {
				oldID := oldIDs[editor.index]
				if oldID != modelID {
					if spec, ok := modelsMap[oldID]; ok {
						modelsMap[modelID] = spec
					}
					delete(modelsMap, oldID)
				}
			}
		}
		spec, _ := modelsMap[modelID].(map[string]any)
		if spec == nil {
			spec = map[string]any{}
		}
		name := strings.TrimSpace(editor.name)
		if name == "" {
			name = modelID
		}
		spec["name"] = name
		limit := jsonObject(spec, "limit")
		if contextWindow != nil {
			limit["context"] = *contextWindow
			if _, ok := limit["output"]; !ok {
				limit["output"] = uint64(32000)
			}
		} else {
			delete(limit, "context")
		}
		modKey := "modalities"
		if roster[i].Native {
			modKey = "capabilities"
		}
		modalities := jsonObject(spec, modKey)
		if input := settingsModalities(editor.input); len(input) > 0 {
			modalities["input"] = input
		} else {
			delete(modalities, "input")
		}
		if output := settingsModalities(editor.output); len(output) > 0 {
			modalities["output"] = output
		} else {
			delete(modalities, "output")
		}
		modelsMap[modelID] = spec
		entry["models"] = modelsMap
		roster[i].Entry = entry
	}
	st.providerModelEditor = nil
	a.commitSettingsProviders(roster, providerID, "")
}

func providerEntryByID(providers []settingsProvider, id string) map[string]any {
	for _, provider := range providers {
		if provider.ID == id {
			return provider.Entry
		}
	}
	return nil
}

func (a *app) probeCustomConnectivity(id string) {
	st := &a.settings.backend
	var target *settingsProvider
	for i := range st.providers {
		if st.providers[i].ID == id {
			target = &st.providers[i]
			break
		}
	}
	if target == nil {
		return
	}
	format := settingsProviderFormatForEntry(target.Entry, target.Native)
	baseURL := settingsProviderBaseURL(target.Entry, target.Native)
	apiKey := settingsProviderAPIKey(target.Entry, target.Native)
	if strings.TrimSpace(apiKey) == "" {
		apiKey = a.resolveProviderKeyForProbe(id)
	}
	if st.providerConnectivity == nil {
		st.providerConnectivity = map[string]providerConnectivityState{}
	}
	st.providerConnectivity[id] = providerConnectivityState{testing: true}
	go func() {
		outcome := probeProviderConnectivity(format, baseURL, apiKey)
		a.post(func() {
			st := &a.settings.backend
			if outcome.reachable {
				st.providerConnectivity[id] = providerConnectivityState{reachable: true, models: outcome.models, latencyMs: outcome.latency.Milliseconds()}
			} else {
				msg := ""
				if outcome.err != nil {
					msg = outcome.err.text()
				}
				st.providerConnectivity[id] = providerConnectivityState{err: msg}
			}
		})
	}()
}

func (a *app) fetchCustomModels(id string) {
	st := &a.settings.backend
	var target *settingsProvider
	for i := range st.providers {
		if st.providers[i].ID == id {
			target = &st.providers[i]
			break
		}
	}
	if target == nil {
		return
	}
	format := settingsProviderFormatForEntry(target.Entry, target.Native)
	baseURL := settingsProviderBaseURL(target.Entry, target.Native)
	apiKey := settingsProviderAPIKey(target.Entry, target.Native)
	if strings.TrimSpace(apiKey) == "" {
		apiKey = a.resolveProviderKeyForProbe(id)
	}
	st.provider.writing = true
	go func() {
		models, listErr := fetchProviderAPIModels(format, baseURL, apiKey, providerListTimeout)
		table := fetchModelsDevCatalogWithFallback(st.modelsTable)
		a.post(func() {
			st := &a.settings.backend
			st.provider.writing = false
			if listErr != nil {
				st.provider.err = tr("Could not fetch the model list: ") + listErr.text()
				return
			}
			if table != nil {
				st.modelsTable = table
			}
			roster := append([]settingsProvider{}, st.providers...)
			added, filled := 0, 0
			for i := range roster {
				if roster[i].ID != id {
					continue
				}
				entry := cloneJSONMap(roster[i].Entry)
				modelsMap := jsonObject(entry, "models")
				for _, apiModel := range models {
					if _, ok := modelsMap[apiModel.id]; ok {
						continue
					}
					spec := map[string]any{"name": apiModel.id}
					if apiModel.name != nil {
						spec["name"] = *apiModel.name
					}
					if table != nil {
						if basics := table.resolveBasics(apiModel.id, &roster[i].ID); basics.name != nil {
							spec["name"] = *basics.name
						} else if basics := table.resolveBasics(apiModel.id, nil); basics.name != nil {
							spec["name"] = *basics.name
						}
						if basics := table.resolveBasics(apiModel.id, nil); basics.contextWindow != nil {
							spec["limit"] = map[string]any{"context": *basics.contextWindow, "output": uint64(32000)}
							filled++
						}
					}
					if apiModel.contextWindow != nil {
						limit, _ := spec["limit"].(map[string]any)
						if limit == nil {
							limit = map[string]any{}
						}
						limit["context"] = *apiModel.contextWindow
						spec["limit"] = limit
					}
					modelsMap[apiModel.id] = spec
					added++
				}
				entry["models"] = modelsMap
				roster[i].Entry = entry
			}
			if added == 0 && filled == 0 {
				st.provider.message = tr("Models already match the provider's list")
				return
			}
			a.commitSettingsProviders(roster, id, "")
			st.provider.message = trf("Fetched %d models: %d added, %d filled in", len(models), added, filled)
		})
	}()
}

func (a *app) probeModelLatency(providerID, modelID string) {
	st := &a.settings.backend
	if st.providerLatency == nil {
		st.providerLatency = map[string]providerLatencyState{}
	}
	key := providerID + "\x00" + modelID
	st.providerLatency[key] = providerLatencyState{testing: true}
	var target *settingsProvider
	for i := range st.providers {
		if st.providers[i].ID == providerID {
			target = &st.providers[i]
			break
		}
	}
	if target == nil {
		return
	}
	format := settingsProviderFormatForEntry(target.Entry, target.Native)
	baseURL := settingsProviderBaseURL(target.Entry, target.Native)
	apiKey := settingsProviderAPIKey(target.Entry, target.Native)
	if strings.TrimSpace(apiKey) == "" {
		apiKey = a.resolveProviderKeyForProbe(providerID)
	}
	go func() {
		latency, firstErr := probeProviderFirstToken(format, baseURL, modelID, apiKey)
		a.post(func() {
			st := &a.settings.backend
			if firstErr != nil {
				st.providerLatency[key] = providerLatencyState{err: firstErr.text()}
				return
			}
			st.providerLatency[key] = providerLatencyState{latencyMs: latency.Milliseconds()}
		})
	}()
}

func providerMin(a, b int) int {
	if a < b {
		return a
	}
	return b
}

func sliceToSet(values []string, fallback string) map[string]bool {
	out := map[string]bool{}
	for _, value := range values {
		out[value] = true
	}
	if len(out) == 0 && fallback != "" {
		out[fallback] = true
	}
	return out
}

func formatContextPtr(window *uint64) string {
	if window == nil {
		return ""
	}
	return formatContextWindow(*window)
}

func stringList(value any) []string {
	items, _ := value.([]any)
	var out []string
	for _, item := range items {
		if text, ok := item.(string); ok && text != "" {
			out = append(out, text)
		}
	}
	return out
}

func jsonNumberToUint(value any) *uint64 {
	switch v := value.(type) {
	case float64:
		if v > 0 {
			n := uint64(v)
			return &n
		}
	case int:
		if v > 0 {
			n := uint64(v)
			return &n
		}
	case int64:
		if v > 0 {
			n := uint64(v)
			return &n
		}
	case uint64:
		if v > 0 {
			n := v
			return &n
		}
	case jsonNumber:
		if n, err := v.Int64(); err == nil && n > 0 {
			u := uint64(n)
			return &u
		}
	}
	return nil
}

func (a *app) beginProviderForm() {
	st := &a.settings.catalog
	backend := &a.settings.backend
	backend.providerSelected = ""
	backend.providerRenaming = false
	backend.providerKeyRevealed = false
	backend.removeConfirm = ""
	backend.providerModelEditor = nil
	backend.providerFormStage = "picking"
	backend.providerFormBuiltin = ""
	backend.providerFormSearch = ""
	st.providerAdding = true
	st.pfName, st.pfBaseURL, st.pfAPIKey, st.pfFormat = "", "", "", "openai"
	st.pfModels, st.pfModelsReady = nil, true
	st.addProviderModel()
	backend.providerFormatTouched = false
	backend.providerFormConnectivity = nil
	backend.providerFormFetching = false
	backend.providerFormFetchGen++
	backend.providerFormProbeGen++
	a.ensureBuiltinProviders()
}

func (a *app) providerAddFormFull(c *ui.Context) {
	backend := &a.settings.backend
	stage := backend.providerFormStage
	if stage == "" {
		stage = "picking"
	}
	switch stage {
	case "builtin":
		a.builtinAddForm(c, backend.providerFormBuiltin)
	case "custom":
		a.customAddForm(c)
	default:
		a.providerPickerForm(c)
	}
}

func (a *app) providerPickerForm(c *ui.Context) {
	backend := &a.settings.backend
	st := &a.settings.catalog
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			formTitle(c, tr("Add a model provider"))
			ui.Text(c, tr("Pick a provider to authorize, or choose Custom for your own endpoint.")).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).Wrap()
			settingsSearchInput(c, "provider-builtin-search", tr("Search providers…"), &backend.providerFormSearch)
			ui.Column(c).FillWidth().Gap(1).Children(func() {
				a.mailRow(c, tr("Custom"), backend.providerFormStage == "custom", func() {
					backend.providerFormStage = "custom"
					st.pfName, st.pfBaseURL, st.pfFormat = "", "", "openai"
					st.pfModels, st.pfModelsReady = nil, true
					st.addProviderModel()
				}, func() { mailTile(c, "plus", backend.providerFormStage == "custom") }, nil)
				if backend.builtinProviders != nil {
					query := strings.ToLower(strings.TrimSpace(backend.providerFormSearch))
					for _, provider := range backend.builtinProviders.sorted() {
						if query != "" && !strings.Contains(strings.ToLower(provider.Name), query) && !strings.Contains(strings.ToLower(provider.ID), query) {
							continue
						}
						provider := provider
						badge := ""
						if a.providerIsAuthorized(provider.ID) {
							if a.providerNeedsAuth(provider.ID) {
								badge = tr("Re-login needed")
							} else {
								badge = tr("Authorized")
							}
						} else if !a.providerSupportsKey(provider.ID) {
							badge = tr("OAuth")
						}
						label := provider.Name
						if badge != "" {
							label += " · " + badge
						}
						a.mailRow(c, label, false, func() {
							backend.providerFormStage = "builtin"
							backend.providerFormBuiltin = provider.ID
						}, func() { providerTile(c, provider.Name, false) }, nil)
					}
				}
			})
			settingsOperationStatus(c, backend.provider, "")
			formActions(c, "provider-form", tr("Cancel"), false, a.cancelProviderAdd, nil)
		})
	})
}

func (a *app) builtinAddForm(c *ui.Context, providerID string) {
	backend := &a.settings.backend
	provider := a.builtinProvider(providerID)
	name := providerID
	if provider != nil {
		name = provider.Name
	}
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			formTitle(c, tr("Add a model provider"))
			ui.Row(c).Gap(8).Children(func() {
				if componentTextButton(c, "provider-form-back", tr("Back to provider list"), false).Clicked() {
					backend.providerFormStage = "picking"
				}
			})
			ui.Text(c, name).FontSize(uiPx(13)).FontWeight(500).TextColor(palette.text)
			if provider != nil && provider.API != nil {
				ui.Text(c, *provider.API).FontSize(uiPx(10.5)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
			}
			if a.providerSupportsKey(providerID) {
				formSecretField(c, "provider-builtin-key", tr("API key"), &a.settings.catalog.pfAPIKey, &a.settings.catalog.pfShowKey)
			} else {
				a.providerOAuthSection(c, providerID)
			}
			if len(a.providerOAuthMethods(providerID)) > 0 && a.providerSupportsKey(providerID) {
				a.providerOAuthSection(c, providerID)
			}
			if !a.providerSupportsKey(providerID) && len(a.providerOAuthMethods(providerID)) == 0 {
				ui.Text(c, tr("Run `opencode auth login` in a terminal or use /connect in the TUI, then refresh this page.")).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
			}
			settingsOperationStatus(c, backend.provider, "")
			ui.Row(c).Gap(8).Children(func() {
				if componentTextButton(c, "provider-form-cancel", tr("Cancel"), false).Clicked() {
					a.cancelProviderAdd()
				}
				if a.providerSupportsKey(providerID) {
					if componentTextButton(c, "provider-form-authorize", tr("Authorize"), true).Disabled(strings.TrimSpace(a.settings.catalog.pfAPIKey) == "").Clicked() {
						a.authorizeBuiltinFromForm(providerID)
					}
				}
			})
			if backend.auth.kind == "provider" && backend.auth.providerID == providerID {
				a.settingsOAuthControls(c)
			}
			if form := backend.oauthForm; form != nil && form.providerID == providerID {
				a.settingsOAuthForm(c, form)
			}
		})
	})
}

func (a *app) customAddForm(c *ui.Context) {
	st := &a.settings.catalog
	backend := &a.settings.backend
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			formTitle(c, tr("Add a model provider"))
			ui.Text(c, tr("A fully custom API endpoint and its first models.")).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).Wrap()
			ui.Row(c).Gap(8).Children(func() {
				if componentTextButton(c, "provider-form-back", tr("Back to provider list"), false).Clicked() {
					backend.providerFormStage = "picking"
				}
			})
			settingsFormField(c, "provider-form-name", tr("Name"), &st.pfName, "e.g. Zhipu GLM")
			settingsFormField(c, "provider-form-url", tr("Base URL"), &st.pfBaseURL, "https://api.example.com/v1")
			formSecretField(c, "provider-form-key", tr("API key"), &st.pfAPIKey, &st.pfShowKey)
			ui.Column(c).FillWidth().Gap(6).Children(func() {
				ui.Text(c, tr("API format")).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
				a.settingsChoice(c, "provider-form-format", tr("API format"), st.pfFormat, formPickerWidth, providerFormatOptions(), func(option settingsOption) {
					st.pfFormat = option.value
					backend.providerFormatTouched = true
					backend.providerFormConnectivity = nil
				})
			})
			ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
				if componentTextButton(c, "provider-form-test", tr("Test connection"), false).Clicked() {
					a.probeAddForm()
				}
				if conn := backend.providerFormConnectivity; conn != nil {
					if conn.testing {
						ui.Text(c, tr("Testing…")).FontSize(uiPx(11)).TextColor(palette.textSecondary)
					} else if conn.reachable {
						ui.Text(c, trf("Reachable · %s · %d models", formatProbeLatency(time.Duration(conn.latencyMs)*time.Millisecond), conn.models)).FontSize(uiPx(11)).TextColor(palette.textSecondary)
					} else if conn.err != "" {
						ui.Text(c, trf("Unreachable: %s", conn.err)).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
					}
				}
			})
			ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
				ui.Text(c, tr("Models")).Grow(1).FontSize(uiPx(11)).FontWeight(500).TextColor(palette.text)
				if componentTextButton(c, "provider-form-fetch", tr("Fetch models"), false).Disabled(backend.providerFormFetching).Clicked() {
					a.fetchAddFormModels()
				}
			})
			a.settingsProviderModels(c)
			if strings.TrimSpace(st.pfBaseURL) != "" && !baseURLValid(strings.TrimSpace(st.pfBaseURL)) {
				ui.Text(c, tr("The Base URL must start with http:// or https://.")).FontSize(uiPx(10.5)).TextColor(palette.textSecondary)
			}
			if strings.TrimSpace(st.pfName) != "" && baseURLValid(strings.TrimSpace(st.pfBaseURL)) && len(st.pfModels) == 0 {
				ui.Text(c, tr("Add at least one model before saving the provider.")).FontSize(uiPx(10.5)).TextColor(palette.textSecondary)
			}
			settingsOperationStatus(c, backend.provider, "")
			valid := strings.TrimSpace(st.pfName) != "" && baseURLValid(strings.TrimSpace(st.pfBaseURL)) && len(providerDraftModelIDs(st)) > 0
			formActions(c, "provider-form", tr("Add provider"), !valid || !backend.provider.loaded || backend.provider.writing, a.cancelProviderAdd, a.saveSettingsProvider)
		})
	})
}

func providerDraftModelIDs(st *settingsCatalogUIState) []string {
	var out []string
	for _, draft := range st.pfModels {
		if strings.TrimSpace(draft.id) != "" {
			out = append(out, strings.TrimSpace(draft.id))
		}
	}
	return out
}

func (a *app) probeAddForm() {
	backend := &a.settings.backend
	st := &a.settings.catalog
	baseURL := strings.TrimSpace(st.pfBaseURL)
	if !baseURLValid(baseURL) {
		backend.provider.err = tr("The Base URL is not an http(s) address")
		return
	}
	format := st.pfFormat
	if format == "" {
		format = "openai"
	}
	apiKey := strings.TrimSpace(st.pfAPIKey)
	backend.providerFormProbeGen++
	gen := backend.providerFormProbeGen
	backend.providerFormConnectivity = &providerConnectivityState{testing: true}
	go func() {
		outcome := probeProviderConnectivity(format, baseURL, apiKey)
		a.post(func() {
			st := &a.settings.backend
			if st.providerFormProbeGen != gen {
				return
			}
			if outcome.reachable {
				st.providerFormConnectivity = &providerConnectivityState{reachable: true, models: outcome.models, latencyMs: outcome.latency.Milliseconds()}
			} else {
				msg := ""
				if outcome.err != nil {
					msg = outcome.err.text()
				}
				st.providerFormConnectivity = &providerConnectivityState{err: msg}
			}
		})
	}()
}

func (a *app) fetchAddFormModels() {
	backend := &a.settings.backend
	st := &a.settings.catalog
	baseURL := strings.TrimSpace(st.pfBaseURL)
	if !baseURLValid(baseURL) {
		backend.provider.err = tr("The Base URL is not an http(s) address")
		return
	}
	format := st.pfFormat
	if format == "" {
		format = "openai"
	}
	apiKey := strings.TrimSpace(st.pfAPIKey)
	backend.providerFormFetching = true
	backend.providerFormFetchGen++
	gen := backend.providerFormFetchGen
	go func() {
		models, listErr := fetchProviderAPIModels(format, baseURL, apiKey, providerListTimeout)
		table := fetchModelsDevCatalogWithFallback(backend.modelsTable)
		a.post(func() {
			st := &a.settings.backend
			catalog := &a.settings.catalog
			st.providerFormFetching = false
			if st.providerFormFetchGen != gen {
				return
			}
			if listErr != nil {
				st.provider.err = tr("Could not fetch the model list: ") + listErr.text()
				return
			}
			if table != nil {
				st.modelsTable = table
			}
			catalog.pfModels = nil
			for _, apiModel := range models {
				catalog.addProviderModel()
				draft := catalog.pfModels[len(catalog.pfModels)-1]
				draft.id = apiModel.id
				if apiModel.contextWindow != nil {
					draft.context = formatContextWindow(*apiModel.contextWindow)
				} else if table != nil {
					if basics := table.resolveBasics(apiModel.id, nil); basics.contextWindow != nil {
						draft.context = formatContextWindow(*basics.contextWindow)
					}
				}
			}
			if len(catalog.pfModels) == 0 {
				catalog.addProviderModel()
			}
			st.provider.message = trf("Fetched %d models: %d added, %d filled in", len(models), len(models), 0)
		})
	}()
}

func (a *app) authorizeBuiltinFromForm(providerID string) {
	key := strings.TrimSpace(a.settings.catalog.pfAPIKey)
	if key == "" {
		return
	}
	a.authorizeSettingsIntegration(providerID)
	a.post(func() {
		a.settings.catalog.pfAPIKey = ""
	})
	go func() {
		time.Sleep(2 * time.Second)
		a.post(func() {
			if a.providerIsAuthorized(providerID) {
				a.cancelProviderAdd()
				a.selectProvider(builtinSelectionID(providerID))
			}
		})
	}()
}

func (a *app) providerOAuthSection(c *ui.Context, providerID string) {
	methods := a.providerOAuthMethods(providerID)
	if len(methods) == 0 {
		return
	}
	ui.Column(c).FillWidth().Gap(8).Children(func() {
		for _, method := range methods {
			method := method
			if componentTextButton(c, "provider-oauth-"+providerID+"-"+method.ID, method.Label, true).Clicked() {
				if len(method.Form) > 0 {
					a.beginSettingsOAuthForm(providerID, method)
					return
				}
				a.cancelSettingsOAuthForm()
				a.startSettingsOAuth("provider", providerID, method.ID)
			}
		}
	})
}

func (a *app) builtinProviderDetail(c *ui.Context, provider modelsDevProvider) {
	backend := &a.settings.backend
	authorized := a.providerIsAuthorized(provider.ID)
	needsAuth := authorized && a.providerNeedsAuth(provider.ID)
	envNames := a.providerEnvNames(provider.ID)
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Gap(14).Children(func() {
			ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
				mailDetailTile(c, "bot", 17)
				ui.Column(c).Grow(1).MinWidth(0).Children(func() {
					ui.Text(c, provider.Name).FontSize(uiPx(15)).FontWeight(500).TextColor(palette.text).SingleLine().Ellipsis("...")
					ui.Text(c, provider.ID).FontSize(uiPx(10)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
				})
				badge := tr("Not authorized")
				if needsAuth {
					badge = tr("Re-login needed")
				} else if authorized {
					badge = tr("Authorized")
				}
				mailEnabledBadge(c, authorized && !needsAuth, badge)
				if len(envNames) > 0 {
					mailEnabledBadge(c, false, tr("Env variable"))
				}
			})
			providerInfoCard(c, "builtin-login", "key", "Login information", func() {
				if authorized {
					componentTextButton(c, "provider-logout", tr("Log out"), false).Disabled(backend.auth.busy).OnClick(func() { a.disconnectSettingsIntegration(provider.ID) })
				}
			}, func() {
				if provider.API != nil {
					ui.Text(c, *provider.API).FontSize(uiPx(10.5)).TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
				}
				if needsAuth {
					ui.Text(c, tr("The provider rejected the stored credential. Sign in again — in the CLI (opencode auth login), the TUI, or by updating the key here — then refresh this page.")).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
					if len(a.providerOAuthMethods(provider.ID)) > 0 {
						if componentTextButton(c, "provider-relogin", tr("Sign in again"), true).Clicked() {
							a.beginProviderForm()
							backend.providerFormStage = "builtin"
							backend.providerFormBuiltin = provider.ID
						}
					}
				}
				if len(envNames) > 0 {
					ui.Text(c, trf("This provider's credential comes from the environment variable %s. Env credentials are managed by the system — the key is never shown here, and logging out cannot remove the connection.", strings.Join(envNames, ", "))).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
				}
				ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
					icon(c, "info", 12, palette.textTertiary)
					ui.Text(c, tr("The API key is stored in OpenCode's own credential store and managed by the opencode CLI; it is never shown here.")).FontSize(uiPx(10.5)).TextColor(palette.textSecondary).Wrap()
				})
				a.providerOAuthSection(c, provider.ID)
				if backend.auth.kind == "provider" && backend.auth.providerID == provider.ID {
					a.settingsOAuthControls(c)
				}
				if form := backend.oauthForm; form != nil && form.providerID == provider.ID {
					a.settingsOAuthForm(c, form)
				}
			})
			providerInfoCard(c, "builtin-models", "package", "Model information", func() {
				componentTextButton(c, "provider-catalog-check", tr("Check model catalog"), false).Disabled(!authorized).OnClick(func() { a.checkBuiltinCatalog(provider.ID) })
			}, func() {
				if check := backend.providerBuiltinCheck; check != nil && backend.providerBuiltinCheckID == provider.ID {
					status := tr("Checking catalog…")
					if !check.testing {
						status = trf("Catalog available · %d models", check.models)
						if check.err != "" {
							status = trf("Catalog query failed: %s", check.err)
						}
					}
					ui.Text(c, status).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
				}
				models := provider.Models
				if len(models) == 0 {
					mailStatusRow(c, tr("No models yet. Add one to make this provider usable in chat."))
				} else {
					shown := models
					if len(shown) > 200 {
						shown = shown[:200]
					}
					ui.Column(c).FillWidth().Gap(4).Children(func() {
						for _, model := range shown {
							name := model.ID
							if model.Name != nil && *model.Name != "" {
								name = *model.Name
							}
							input, output := a.providerModelModalities(model.ID, provider.ID, model.InputModalities, model.OutputModalities)
							providerModelListRow(c, provider.ID+"/"+model.ID, model.ID, name, provider.ID, model.ContextWindow, input, output, false, nil, nil)
						}
					})
					if hidden := len(models) - len(shown); hidden > 0 {
						ui.Text(c, trf("%d more models not shown", hidden)).FontSize(uiPx(10)).TextColor(palette.textTertiary)
					}
				}
			})
			settingsOperationStatus(c, backend.provider, "")
		})
	})
}

func (a *app) checkBuiltinCatalog(providerID string) {
	backend := &a.settings.backend
	if !a.providerIsAuthorized(providerID) {
		return
	}
	backend.providerBuiltinCheck = &providerBuiltinCheckState{testing: true}
	backend.providerBuiltinCheckID = providerID
	source := backend.source
	binary := a.settingsStr("opencode_binary_path", "opencode")
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
		defer cancel()
		var body struct {
			Count  *int `json:"count"`
			Models []struct {
				ID string `json:"id"`
			} `json:"models"`
		}
		err := settingsRequest(ctx, source, map[string]any{"type": "probeBuiltinProvider", "binary": binary, "directory": source.directory, "providerId": providerID}, "builtinProviderProbed", &body)
		a.post(func() {
			st := &a.settings.backend
			if st.providerBuiltinCheckID != providerID {
				return
			}
			if err != nil {
				st.providerBuiltinCheck = &providerBuiltinCheckState{err: err.Error()}
				return
			}
			count := len(body.Models)
			if body.Count != nil {
				count = *body.Count
			}
			st.providerBuiltinCheck = &providerBuiltinCheckState{models: count}
		})
	}()
}

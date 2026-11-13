package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"

	"github.com/egoist/mygo/ui"
)

// providerProbe mirrors legacy/rust/crates/fintwind-protocol/src/model.rs ProviderProbe.
// The daemon returns it inside the probeProvider payload as {"probe": {...}}.
type providerProbe struct {
	Installed    bool                  `json:"installed"`
	Path         *string               `json:"path"`
	Models       []providerModel       `json:"models"`
	AgentPresets []providerAgentPreset `json:"agentPresets"`
}

// providerModel mirrors legacy/rust/crates/fintwind-protocol/src/model.rs ProviderModel
// (no serde rename there, so the wire keys stay snake_case).
type providerModel struct {
	ID                     string                `json:"id"`
	Name                   string                `json:"name"`
	SubProvider            *string               `json:"sub_provider"`
	IsDefault              bool                  `json:"is_default"`
	ReasoningEfforts       []providerModelOption `json:"reasoning_efforts"`
	DefaultReasoningEffort *string               `json:"default_reasoning_effort"`
	ServiceTiers           []providerModelOption `json:"service_tiers"`
	DefaultServiceTier     *string               `json:"default_service_tier"`
	ContextWindows         []providerModelOption `json:"context_windows"`
	DefaultContextWindow   *string               `json:"default_context_window"`
}

// providerModelOption is ProviderModelOption: the reasoning-effort and
// service-tier rows the composer's secondary chips offer.
type providerModelOption struct {
	ID          string  `json:"id"`
	Label       string  `json:"label"`
	Description *string `json:"description"`
}

type providerAgentPreset struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

// modelPickerDefaultID is the pseudo-row that clears the session model back
// to the provider default (a.detail.Model = nil).
const modelPickerDefaultID = "default"

// Panel geometry from render_provider_model_control's popover body in
// src/app/composer.rs: w460 h390 r13; the list below subtracts the 54-high
// search strip.
const (
	modelPickerWidth      = 460.0
	modelPickerHeight     = 390.0
	modelPickerSearchH    = 54.0
	modelPickerFieldH     = 36.0
	modelPickerFieldR     = 9.0
	modelPickerRowH       = 60.0
	modelPickerRowR       = 9.0
	modelPickerNameText   = 13.0
	modelPickerSubText    = 11.5
	modelPickerQueryText  = 12.5
	modelPickerQueryLine  = 17.0
	modelPickerSearchIcon = 15.0
	modelPickerCheckIcon  = 12.0
	// modelChipLabelWidth caps the chip label per MODEL_CHIP_MAX_LABEL in
	// src/app/composer.rs; longer names ellipsize instead of pushing the
	// sibling chips off the row.
	modelChipLabelWidth = 210.0
)

// modelDisplayName mirrors App::model_display_name in src/app/runtime.rs:
// no model reads "OpenCode"; a catalog hit shows its name, otherwise the
// raw id. (a.modelLabel in main.go stays untouched.)
func (a *app) modelDisplayName() string {
	if a.detail == nil || a.detail.Model == nil || *a.detail.Model == "" {
		return "OpenCode"
	}
	id := *a.detail.Model
	if a.modelProbe != nil {
		for i := range a.modelProbe.Models {
			if a.modelProbe.Models[i].ID == id {
				return a.modelProbe.Models[i].Name
			}
		}
	}
	return id
}

// pickerProviderLabel mirrors model_picker_provider_label in
// src/app/composer.rs: empty and "opencode" read as "OpenCode".
func pickerProviderLabel(subProvider *string) string {
	name := ""
	if subProvider != nil {
		name = strings.TrimSpace(*subProvider)
	}
	if name == "" || strings.EqualFold(name, "opencode") {
		return "OpenCode"
	}
	return name
}

// pickerTab is one sidebar entry: the Favorites star or one provider.
type pickerTab struct {
	favorites bool
	label     string
}

// visiblePickerTabs mirrors visible_picker_tabs: Favorites first, then one
// tab per distinct provider label in catalog order.
func (a *app) visiblePickerTabs() []pickerTab {
	tabs := []pickerTab{{favorites: true}}
	if a.modelProbe == nil {
		return tabs
	}
	for _, model := range a.modelProbe.Models {
		label := pickerProviderLabel(model.SubProvider)
		duplicate := false
		for _, tab := range tabs {
			if !tab.favorites && tab.label == label {
				duplicate = true
				break
			}
		}
		if !duplicate {
			tabs = append(tabs, pickerTab{label: label})
		}
	}
	return tabs
}

// currentPickerTab mirrors selected_model_picker_tab: the tab holding the
// current model, defaulting to its provider label ("OpenCode" for the
// default/unset model or while the catalog is still loading).
func (a *app) currentPickerTab() string {
	if model := a.currentProviderModel(); model != nil {
		return "provider:" + pickerProviderLabel(model.SubProvider)
	}
	return "provider:OpenCode"
}

// isFavoriteModel reports whether id is starred.
func (a *app) isFavoriteModel(id string) bool {
	for _, favorite := range a.favoriteModels {
		if favorite == id {
			return true
		}
	}
	return false
}

// cyclePickerTab steps the sidebar rail to the adjacent tab, wrapping at
// both ends, mirroring cycle_model_picker_tab. A live query hides which tab
// is selected and searches across all of them, so cycling waits until the
// field is cleared.
func (a *app) cyclePickerTab(previous bool) {
	if strings.TrimSpace(a.modelQuery) != "" {
		return
	}
	tabs := a.visiblePickerTabs()
	if len(tabs) == 0 {
		return
	}
	current := -1
	for i, tab := range tabs {
		if a.tabKey(tab) == a.modelPickerTab {
			current = i
			break
		}
	}
	next := 0
	if current < 0 {
		next = 0
	} else if previous {
		next = current - 1
		if next < 0 {
			next = len(tabs) - 1
		}
	} else {
		next = current + 1
		if next >= len(tabs) {
			next = 0
		}
	}
	a.selectPickerTab(a.tabKey(tabs[next]))
}

// tabKey encodes one sidebar tab for modelPickerTab.
func (a *app) tabKey(tab pickerTab) string {
	if tab.favorites {
		return "favorites"
	}
	return "provider:" + tab.label
}

// selectPickerTab mirrors select_model_picker_tab: switching tabs refreshes
// the catalog, renumbers the rows under the keyboard cursor, and reveals
// the current model instead of inheriting the old tab's offset.
func (a *app) selectPickerTab(tab string) {
	if a.modelPickerTab == tab {
		return
	}
	a.modelPickerTab = tab
	a.requestModelProbe()
	a.revealPickerSelected()
}

// revealPickerSelected mirrors reveal_selected_picker_model: highlight
// follows the current model in the unfiltered list, falling back to the
// top so a stale offset never leaks into a fresh list.
func (a *app) revealPickerSelected() {
	models := a.unfilteredPickerModels()
	current := ""
	if a.detail != nil && a.detail.Model != nil {
		current = *a.detail.Model
	}
	searching := strings.TrimSpace(a.modelQuery) != ""
	selected := 0
	if !searching {
		// Row 0 is the Default row when not searching.
		selected = len(models) + 1
		for i, candidate := range models {
			if candidate.ID == current {
				selected = i + 1
				break
			}
		}
		if current == "" {
			selected = 0
		}
		if total := len(models) + 1; selected >= total {
			selected = 0
		}
	} else {
		for i, candidate := range models {
			if candidate.ID == current {
				selected = i
				break
			}
		}
		if selected >= len(models) {
			selected = 0
		}
	}
	a.modelHighlight = selected
	a.modelListScroll.Y = float32(selected) * modelPickerRowH
}

// unfilteredPickerModels lists the catalog rows for the selected tab with an
// empty query: starred ids in starred order under Favorites, label matches
// under a provider. Searching spans every tab instead.
func (a *app) unfilteredPickerModels() []providerModel {
	if a.modelProbe == nil {
		return nil
	}
	if a.modelPickerTab == "favorites" {
		byID := make(map[string]providerModel, len(a.modelProbe.Models))
		for _, model := range a.modelProbe.Models {
			byID[model.ID] = model
		}
		out := make([]providerModel, 0, len(a.favoriteModels))
		for _, id := range a.favoriteModels {
			if model, ok := byID[id]; ok {
				out = append(out, model)
			}
		}
		return out
	}
	label := ""
	if strings.HasPrefix(a.modelPickerTab, "provider:") {
		label = a.modelPickerTab[len("provider:"):]
	}
	out := make([]providerModel, 0, len(a.modelProbe.Models))
	for _, model := range a.modelProbe.Models {
		if label == "" || pickerProviderLabel(model.SubProvider) == label {
			out = append(out, model)
		}
	}
	return out
}

// visibleModels mirrors visible_picker_models: every whitespace-separated
// query token must appear in name/id/sub_provider, searching across every
// tab; with an empty query the selected tab filters the list.
func (a *app) visibleModels() []providerModel {
	if a.modelProbe == nil {
		return nil
	}
	query := strings.ToLower(strings.TrimSpace(a.modelQuery))
	if query == "" {
		return a.unfilteredPickerModels()
	}
	tokens := strings.Fields(query)
	visible := make([]providerModel, 0, len(a.modelProbe.Models))
	for _, candidate := range a.modelProbe.Models {
		sub := ""
		if candidate.SubProvider != nil {
			sub = *candidate.SubProvider
		}
		haystack := strings.ToLower(candidate.Name + " " + candidate.ID + " " + sub)
		matched := true
		for _, token := range tokens {
			if !strings.Contains(haystack, token) {
				matched = false
				break
			}
		}
		if matched {
			visible = append(visible, candidate)
		}
	}
	return visible
}

type modelDiscoveryState struct {
	client     *daemon.Client
	identity   daemon.Identity
	directory  string
	generation uint64
	refresh    uint64
	retryAt    time.Time
}

var modelProbeRefresh uint64

// resetModelProbeRetry lets an explicit Refresh ignore the backoff.
func resetModelProbeRetry() { modelProbeRefresh++ }

// Catalogs belong to a connection and workspace. A late response cannot
// replace another source's catalog or change the user's selected model.
func (a *app) requestModelProbe() {
	if a.client == nil {
		return
	}
	project := a.selectedProject()
	if project == nil || project.Path == "" || !sessionProjectIsLocal(project) {
		a.modelDiscovery.generation++
		a.modelDiscovery.directory = ""
		a.modelProbe, a.modelProbeLoad = nil, false
		return
	}
	client := a.client
	identity := client.Identity()
	directory := project.Path
	state := &a.modelDiscovery
	if state.client != client || state.identity != identity || state.directory != directory || state.refresh != modelProbeRefresh {
		state.generation++
		state.client, state.identity, state.directory, state.refresh = client, identity, directory, modelProbeRefresh
		state.retryAt = time.Time{}
		a.modelProbe, a.modelProbeLoad = nil, false
	}
	if a.modelProbe != nil || a.modelProbeLoad || time.Now().Before(state.retryAt) {
		return
	}
	a.modelProbeLoad = true
	state.generation++
	generation := state.generation
	current := func() bool {
		project := a.selectedProject()
		return a.client == client && client.Identity() == identity && a.modelDiscovery.generation == generation &&
			a.modelDiscovery.refresh == modelProbeRefresh && project != nil && sessionProjectIsLocal(project) && project.Path == directory
	}
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 30*time.Second)
		defer cancel()
		payload, err := client.Request(ctx, map[string]any{
			"type":           "probeProvider",
			"binaryOverride": nil,
			"directory":      directory,
			"discoverModels": true,
			"probeVersion":   true,
		})
		if err != nil {
			a.post(func() {
				if !current() {
					return
				}
				a.modelProbeLoad = false
				a.modelDiscovery.retryAt = time.Now().Add(30 * time.Second)
			})
			return
		}
		var response struct {
			Probe *providerProbe `json:"probe"`
		}
		if err := json.Unmarshal(payload, &response); err != nil || response.Probe == nil {
			a.post(func() {
				if !current() {
					return
				}
				a.modelProbeLoad = false
				a.modelDiscovery.retryAt = time.Now().Add(30 * time.Second)
			})
			return
		}
		probe := response.Probe
		a.post(func() {
			if !current() {
				return
			}
			a.modelProbe = probe
			a.modelProbeLoad = false
			a.reconcileContextWindow()
		})
	}()
}

// chooseModel mirrors App::choose_model in src/app/sessions.rs: the default
// row clears the model to nil, any other row writes its id, then the choice
// is applied to the live runtime and persisted. Like
// remember_selected_model_traits, switching models drops the old model's
// explicit traits so the chips follow the new model's defaults.
func (a *app) chooseModel(id string) {
	if a.detail == nil {
		a.modelMenuOpen = false
		return
	}
	if id == modelPickerDefaultID {
		if a.detail.Model != nil {
			a.rememberContextWindow()
			a.detail.Model = nil
			a.detail.ContextWindow = nil
			a.syncSummary(a.detail)
			a.applySessionOptions()
		}
	} else if a.detail.Model == nil || *a.detail.Model != id {
		a.rememberContextWindow()
		model := id
		a.detail.Model = &model
		a.detail.ReasoningEffort = nil
		a.detail.ServiceTier = nil
		a.detail.ContextWindow = a.rememberedContextWindow(id)
		a.syncSummary(a.detail)
		a.applySessionOptions()
	}
	a.rememberContextWindow()
	a.modelMenuOpen = false
	a.focusComposerOnce = true
}

// modelPickerMenu draws the popover panel: a 54-high search strip over the
// catalog rows, mirroring the search_input/rows divs in
// render_provider_model_control. The search field keeps focus the whole time
// the menu is open and rows are only drawn, never focused — the Zed picker
// split: Up/Down move the drawn cursor, Enter chooses it, Esc closes.
func (a *app) modelPickerMenu(c *ui.Context) {
	// The tab is resolved before the list: opening resets it to the
	// current model's provider tab, mirroring selected_model_picker_tab.
	opened := a.modelMenuJustOpened
	a.modelMenuJustOpened = false
	if opened || a.modelPickerTab == "" {
		a.modelPickerTab = a.currentPickerTab()
	}
	if a.favoriteModels == nil {
		a.favoriteModels = a.loadFavoriteModels()
	}
	searching := strings.TrimSpace(a.modelQuery) != ""
	models := a.visibleModels()
	current := ""
	if a.detail != nil && a.detail.Model != nil {
		current = *a.detail.Model
	}
	// Row order mirrors rendering: Default first when not searching, then
	// the filtered catalog. The highlight is an index into that order.
	total := len(models)
	if !searching {
		total++
	}
	if a.modelHighlight < 0 {
		a.modelHighlight = 0
	}
	if total > 0 && a.modelHighlight >= total {
		a.modelHighlight = 0
	}
	// Opening and a cleared query reveal the current model, mirroring
	// reveal_selected_picker_model: highlight follows it and the list
	// scrolls it into view once. User scrolling after that is never fought.
	if opened || (a.modelPickerWasSearching && !searching) {
		a.revealPickerSelected()
	}
	a.modelPickerWasSearching = searching
	tabs := a.visiblePickerTabs()
	favoritesSelected := a.modelPickerTab == "favorites" && !searching
	ui.Row(c).FillWidth().FillHeight().Children(func() {
		a.modelPickerSidebar(c, tabs, favoritesSelected, searching)
		ui.Column(c).Grow(1).MinWidth(0).FillHeight().Background(palette.surface).Radius(0, 12, 12, 0).Children(func() {
			ui.Row(c).FillWidth().Height(modelPickerSearchH).
				Padding(10, 12, 8, 12).AlignItems(ui.Center).Children(func() {
				field := ui.Row(c).FillWidth().Height(modelPickerFieldH).
					Padding(0, 10).Radius(modelPickerRowR).Gap(8).
					Background(palette.raised).AlignItems(ui.Center)
				field.Children(func() {
					icon(c, "search", modelPickerSearchIcon, palette.textSecondary)
					ui.Row(c).Grow(1).MinWidth(0).Children(func() {
						input := ui.TextAreaBase(c, &a.modelQuery).FillWidth().Lines(1, 1).
							FontSize(uiPx(modelPickerQueryText)).FixedLineHeight(uiPx(modelPickerQueryLine)).
							TextColor(palette.text).Placeholder(tr("Search models"))
						// The filter keeps focus while open so typing always
						// narrows; rows stay pointer-only.
						input.Focus()
						input.OnTextKey(func(k ui.TextKey) bool {
							switch k.Key {
							case ui.KeyUp:
								if total > 0 {
									if a.modelHighlight <= 0 {
										a.modelHighlight = total - 1
									} else {
										a.modelHighlight--
									}
								}
								return true
							case ui.KeyDown:
								if total > 0 {
									a.modelHighlight++
									if a.modelHighlight >= total {
										a.modelHighlight = 0
									}
								}
								return true
							case ui.KeyTab:
								// Tab cycles the sidebar rail while the field is
								// empty, mirroring cycle_model_picker_tab; a live
								// query searches across every tab, so cycling
								// waits until it is cleared. Either way Tab never
								// leaves the picker.
								if !searching {
									a.cyclePickerTab((k.Mods & ui.Shift) != 0)
								}
								return true
							case ui.KeyEnter:
								if k.Mods != 0 {
									return false
								}
								if total == 0 {
									return true
								}
								if !searching {
									if a.modelHighlight == 0 {
										a.chooseModel(modelPickerDefaultID)
										return true
									}
									if a.modelHighlight-1 < len(models) {
										a.chooseModel(models[a.modelHighlight-1].ID)
										return true
									}
									return true
								}
								if a.modelHighlight < len(models) {
									a.chooseModel(models[a.modelHighlight].ID)
								}
								return true
							case ui.KeyEscape:
								a.modelMenuOpen = false
								a.focusComposerOnce = true
								return true
							}
							return false
						})
					})
				})
			})
			ui.Scroll(c).FillWidth().Grow(1).TrackScroll(&a.modelListScroll).Children(func() {
				ui.Column(c).FillWidth().Padding(9, 9, 9, 9).Children(func() {
					index := 0
					if !searching {
						a.modelPickerRow(c, modelPickerDefaultID, "Default",
							"OpenCode default model", current == "", index == a.modelHighlight, false)
						index++
					}
					for _, candidate := range models {
						candidate := candidate
						highlighted := index == a.modelHighlight
						a.modelPickerRow(c, candidate.ID, candidate.Name,
							candidate.ID, candidate.ID == current, highlighted, true)
						index++
					}
					if len(models) == 0 {
						a.modelPickerEmpty(c, searching)
					}
				})
			})
		})
	})
}

// modelPickerSidebar is the picker's 50-wide rail: the Favorites star over a
// divider over one 40x40 provider mark per tab, mirroring the sidebar div in
// render_provider_model_control. While searching the rail shows no selected
// tab and the list spans every tab.
func (a *app) modelPickerSidebar(c *ui.Context, tabs []pickerTab, favoritesSelected, searching bool) {
	ui.Column(c).Width(50).FillHeight().Padding(5).Gap(4).AlignItems(ui.Center).
		Background(palette.canvas).BorderWidth(0, 1, 0, 0).BorderColor(palette.border).
		Radius(12, 0, 0, 12).Children(func() {
		star := ui.ButtonBase(c).Label(tr("Favorites")).Size(40, 40).Radius(8).Center()
		starColor := palette.textTertiary
		if star.Pressed() {
			star.Background(palette.overlayStrong)
			starColor = palette.text
		} else if favoritesSelected {
			star.Background(alpha(palette.neutral, 0.12))
			starColor = palette.neutral
		} else if star.Hovered() {
			star.Background(palette.overlay)
		}
		star.Children(func() {
			icon(c, "star", 18, starColor)
		})
		if star.Clicked() {
			a.selectPickerTab("favorites")
		}
		ui.Row(c).Width(34).Height(1).Background(palette.border).Margin(3, 0, 3, 0)
		ui.Scroll(c).FillWidth().Grow(1).Children(func() {
			ui.Column(c).FillWidth().Gap(4).AlignItems(ui.Center).Children(func() {
				for _, tab := range tabs {
					if tab.favorites {
						continue
					}
					tab := tab
					key := a.tabKey(tab)
					selected := key == a.modelPickerTab && !searching
					mark := companyIconFor(tab.label)
					if mark == "" {
						mark = "bot"
					}
					btn := ui.ButtonBase(c).Label(tab.label).Size(40, 40).Radius(8).Center()
					iconColor := alpha(palette.text, 0.82)
					if btn.Pressed() {
						btn.Background(palette.overlayStrong)
						iconColor = palette.text
					} else if selected {
						btn.Background(alpha(palette.neutral, 0.12))
						iconColor = palette.neutral
					} else if btn.Hovered() {
						btn.Background(palette.overlay)
					}
					btn.Children(func() {
						markIcon(c, mark, 18, iconColor)
					})
					if btn.Clicked() {
						a.selectPickerTab(key)
					}
				}
			})
		})
	})
}

// modelPickerEmpty mirrors the picker's empty labels: a query with no hit,
// the favorites hint, the loading state, or an empty catalog.
func (a *app) modelPickerEmpty(c *ui.Context, searching bool) {
	label := tr("No models reported")
	switch {
	case searching:
		label = tr("No models found")
	case a.modelPickerTab == "favorites":
		label = tr("Star a model to keep it here")
	case a.modelProbeLoad:
		label = tr("Loading…")
	}
	ui.Row(c).FillWidth().MinHeight(modelPickerRowH).Center().Children(func() {
		ui.Text(c, label).FontSize(uiPx(modelPickerSubText)).TextColor(palette.textMuted)
	})
}

// modelPickerRow is one 60-high catalog row: semibold 13 name over the
// 11.5 id line (composer.rs model rows), selected rows carry the
// neutral 0.34 border over the 0.10 fill plus a check, hover reads overlay.
// The keyboard cursor reads as a ring rather than a fill so it stays legible
// on the current model's already-filled row. The star is a sibling button,
// not nested, so toggling a favorite never chooses the model.
func (a *app) modelPickerRow(c *ui.Context, id, name, secondary string, selected, highlighted, favoritable bool) {
	c.Key("model-" + id)
	row := ui.Row(c).FillWidth().Height(modelPickerRowH).
		Padding(0, 4, 0, 12).Radius(modelPickerRowR).Gap(10).AlignItems(ui.Center)
	// Border is reserved on every row so highlighting one cannot resize it.
	row.Border(1, ui.Transparent)
	if selected {
		row.Background(alpha(palette.neutral, 0.10))
		row.Border(1, alpha(palette.neutral, 0.34))
	}
	if highlighted {
		row.Background(alpha(palette.neutral, 0.14))
		row.Border(1, palette.neutral)
	} else if row.Hovered() {
		row.Background(palette.overlay)
	}
	row.Children(func() {
		choose := ui.ButtonBase(c).Label(name).Tooltip(secondary).Grow(1).MinWidth(0).
			Height(modelPickerRowH).Radius(modelPickerRowR).Gap(10).
			Justify(ui.Start).AlignItems(ui.Center)
		if choose.Pressed() {
			choose.Opacity(0.85)
		}
		choose.Children(func() {
			ui.Column(c).Grow(1).MinWidth(0).Children(func() {
				ui.Text(c, name).FontSize(uiPx(modelPickerNameText)).FontWeight(600).
					TextColor(palette.text).SingleLine().Ellipsis("...")
				ui.Text(c, secondary).FontSize(uiPx(modelPickerSubText)).FixedLineHeight(uiPx(16)).
					TextColor(palette.textTertiary).SingleLine().Ellipsis("...").Margin(4, 0, 0, 0)
			})
			if selected {
				icon(c, "check", modelPickerCheckIcon, palette.textTertiary)
			}
		})
		if choose.Clicked() {
			a.chooseModel(id)
		}
		if favoritable {
			favorite := a.isFavoriteModel(id)
			tip := tr("Add to favorites")
			starIcon := "star"
			starColor := palette.textGhost
			if favorite {
				tip = tr("Remove from favorites")
				starIcon = "star-filled"
				starColor = palette.neutral
			}
			star := ui.ButtonBase(c).Label(tip).Size(30, 30).
				Radius(7).Center()
			if star.Pressed() {
				star.Opacity(0.8)
			} else if star.Hovered() {
				star.Background(palette.overlayStrong)
			}
			star.Children(func() {
				icon(c, starIcon, 15, starColor)
			})
			if star.Clicked() {
				a.toggleFavoriteModel(id)
			}
		}
	})
}

// favoriteModelsPath is the on-disk home of starred model ids. Only starred
// ids are stored; unstarred sessions read as empty. Mirrors the
// queued-collapsed persistence pattern.
func favoriteModelsPath() string {
	if root := os.Getenv("LOCALAPPDATA"); root != "" {
		return filepath.Join(root, "Fintwind Go", "ui-state", "favorite-models.json")
	}
	if dir, err := os.UserCacheDir(); err == nil && dir != "" {
		return filepath.Join(dir, "Fintwind Go", "ui-state", "favorite-models.json")
	}
	return ""
}

func (a *app) loadFavoriteModels() []string {
	favorites := []string{}
	path := favoriteModelsPath()
	if path == "" {
		return favorites
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return favorites
	}
	var ids []string
	if json.Unmarshal(data, &ids) != nil {
		return favorites
	}
	seen := map[string]bool{}
	for _, id := range ids {
		if id != "" && id != modelPickerDefaultID && !seen[id] {
			seen[id] = true
			favorites = append(favorites, id)
		}
	}
	return favorites
}

func (a *app) saveFavoriteModels() {
	path := favoriteModelsPath()
	if path == "" {
		return
	}
	ids := append([]string(nil), a.favoriteModels...)
	if ids == nil {
		ids = []string{}
	}
	data, err := json.Marshal(ids)
	if err != nil {
		return
	}
	_ = os.MkdirAll(filepath.Dir(path), 0o755)
	_ = os.WriteFile(path, data, 0o600)
}

// toggleFavoriteModel stars or unstars one catalog model, mirroring
// toggle_favorite_model. Render-time order, never selection state.
func (a *app) toggleFavoriteModel(id string) {
	if a.favoriteModels == nil {
		a.favoriteModels = a.loadFavoriteModels()
	}
	if id == "" || id == modelPickerDefaultID {
		return
	}
	for i, favorite := range a.favoriteModels {
		if favorite == id {
			a.favoriteModels = append(a.favoriteModels[:i], a.favoriteModels[i+1:]...)
			a.saveFavoriteModels()
			return
		}
	}
	a.favoriteModels = append(a.favoriteModels, id)
	a.saveFavoriteModels()
}

// currentProviderModel returns the catalog row backing the session's model, so
// the secondary chips can read its reasoning efforts and service tiers.
func (a *app) currentProviderModel() *providerModel {
	if a.modelProbe == nil || a.detail == nil || a.detail.Model == nil {
		return nil
	}
	for i := range a.modelProbe.Models {
		if a.modelProbe.Models[i].ID == *a.detail.Model {
			return &a.modelProbe.Models[i]
		}
	}
	return nil
}

// modelOptionState resolves the chip label for an optional per-model setting:
// explicit value, else the model default, else the first option. Empty id
// means the provider picks.
func modelOptionState(explicit, def *string, options []providerModelOption) (string, string, bool) {
	if len(options) == 0 {
		return "", "", false
	}
	id := ""
	if explicit != nil {
		id = *explicit
	} else if def != nil {
		id = *def
	}
	if id == "" {
		return "", options[0].Label, true
	}
	for _, option := range options {
		if option.ID == id {
			return id, option.Label, true
		}
	}
	return id, id, true
}

// modelOptionMenuOpen remembers which per-model chip menu is open. Package
// level because main.go carries no field for them and the window is single.
var modelOptionMenuOpen = map[string]*bool{}

func modelOptionMenu(id string) *bool {
	if open, ok := modelOptionMenuOpen[id]; ok {
		return open
	}
	open := new(bool)
	modelOptionMenuOpen[id] = open
	return open
}

// modelOptionChip draws one secondary chip (reasoning effort or service tier)
// with its dropdown. Mirrors render_provider_model_control's smaller chips:
// same 28-high MenuChip geometry as the model chip. The effective value
// (explicit, else model default) carries the check; the Default row is
// checked only while the session leaves the value unset.
func (a *app) modelOptionChip(c *ui.Context, id, label string, options []providerModelOption, clear bool, choose func(string)) {
	chip := ui.ButtonBase(c).Label(label).Height(composerChipHeight).
		Padding(0, composerChipInset).Radius(composerChipRadius).
		Gap(composerChipGap).AlignItems(ui.Center).Disabled(a.detail == nil)
	open := modelOptionMenu(id)
	if *open || chip.Hovered() {
		chip.Background(palette.overlay)
	}
	if chip.Pressed() {
		chip.Background(palette.overlayStrong)
	}
	chip.Children(func() {
		ui.Text(c, label).FontSize(uiPx(composerChipText)).FixedLineHeight(uiPx(16)).
			TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
	})
	if chip.Clicked() {
		for _, key := range modelOptionMenuOpen {
			*key = false
		}
		*open = !*open
	}
	ui.PopoverBase(c, chip, open, func(panel ui.Element) {
		panel.Background(palette.raised).Border(1, palette.borderStrong).Radius(9).
			Padding(4).Width(232).
			Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.28)).
			Shadow(0, 12, 28, 0, ui.RGBA(0, 0, 0, 0.22)).
			Children(func() {
				explicit := a.detailReasoningValue(id)
				effective := explicit
				if effective == "" {
					if model := a.currentProviderModel(); model != nil {
						if id == "service-tier" && model.DefaultServiceTier != nil {
							effective = *model.DefaultServiceTier
						} else if id != "service-tier" && model.DefaultReasoningEffort != nil {
							effective = *model.DefaultReasoningEffort
						}
					}
				}
				if clear {
					defaultSelected := explicit == ""
					row := ui.ButtonBase(c).Label(tr("Default")).FillWidth().Height(28).
						Padding(0, 8).Radius(7).Gap(8).Justify(ui.Start).AlignItems(ui.Center)
					if defaultSelected {
						row.Background(alpha(palette.inverse, 0.10))
					} else if row.Hovered() {
						row.Background(palette.overlay)
					}
					row.Children(func() {
						color := palette.textSecondary
						if defaultSelected {
							color = palette.text
						}
						ui.Row(c).Grow(1).MinWidth(0).Children(func() {
							text := ui.Text(c, tr("Default")).FontSize(uiPx(12.5)).TextColor(color)
							if defaultSelected {
								text.FontWeight(500)
							}
						})
						if defaultSelected {
							icon(c, "check", 12, palette.textTertiary)
						}
					})
					if row.Clicked() {
						*open = false
						choose("")
					}
				}
				for _, option := range options {
					option := option
					selected := effective != "" && effective == option.ID
					tip := option.Label
					if option.Description != nil && *option.Description != "" {
						tip = option.Label + " — " + *option.Description
					}
					row := ui.ButtonBase(c).Label(option.Label).Tooltip(tip).FillWidth().Height(28).
						Padding(0, 8).Radius(7).Gap(8).Justify(ui.Start).AlignItems(ui.Center)
					if row.Hovered() {
						row.Background(palette.overlay)
					}
					row.Children(func() {
						color := palette.textSecondary
						if selected {
							color = palette.text
						}
						ui.Row(c).Grow(1).MinWidth(0).Children(func() {
							text := ui.Text(c, option.Label).FontSize(uiPx(12.5)).TextColor(color)
							if selected {
								text.FontWeight(500)
							}
						})
						if selected {
							icon(c, "check", 12, palette.textTertiary)
						}
					})
					if row.Clicked() {
						*open = false
						choose(option.ID)
					}
				}
			})
	})
}

// detailReasoningValue reads the session's explicit value for either chip.
func (a *app) detailReasoningValue(id string) string {
	if a.detail == nil {
		return ""
	}
	if id == "service-tier" {
		if a.detail.ServiceTier == nil {
			return ""
		}
		return *a.detail.ServiceTier
	}
	if a.detail.ReasoningEffort == nil {
		return ""
	}
	return *a.detail.ReasoningEffort
}

// reasoningEffortChip and serviceTierChip only render when the catalog lists
// choices for the current model, matching the Rust composer's conditional
// secondary chips.
func (a *app) reasoningEffortChip(c *ui.Context) {
	model := a.currentProviderModel()
	if model == nil || len(model.ReasoningEfforts) == 0 {
		return
	}
	_, label, ok := modelOptionState(a.detail.ReasoningEffort, model.DefaultReasoningEffort, model.ReasoningEfforts)
	if !ok {
		return
	}
	a.modelOptionChip(c, "reasoning-effort", label, model.ReasoningEfforts, true,
		func(value string) { a.chooseReasoningEffort(value) })
}

func (a *app) serviceTierChip(c *ui.Context) {
	model := a.currentProviderModel()
	if model == nil || len(model.ServiceTiers) == 0 {
		return
	}
	_, label, ok := modelOptionState(a.detail.ServiceTier, model.DefaultServiceTier, model.ServiceTiers)
	if !ok {
		return
	}
	a.modelOptionChip(c, "service-tier", label, model.ServiceTiers, true,
		func(value string) { a.chooseServiceTier(value) })
}

func (a *app) chooseReasoningEffort(value string) {
	if a.detail == nil {
		return
	}
	if value == "" {
		a.detail.ReasoningEffort = nil
	} else {
		effort := value
		a.detail.ReasoningEffort = &effort
	}
	a.applySessionOptions()
}

func (a *app) chooseServiceTier(value string) {
	if a.detail == nil {
		return
	}
	if value == "" {
		a.detail.ServiceTier = nil
	} else {
		tier := value
		a.detail.ServiceTier = &tier
	}
	a.applySessionOptions()
}

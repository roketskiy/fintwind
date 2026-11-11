package main

import (
	"strconv"
	"strings"

	"github.com/egoist/mygo/ui"
)

// settingsMarketGroupOrder mirrors the Rust grouped sections: every
// category except All, in FILTERS order.
var settingsMarketGroupOrder = []string{
	"web", "academic", "code", "office", "design",
	"data", "automation", "devops", "business",
}

// settingsMarketCatLabel reuses the shared filter options so chips,
// captions, and section labels stay in sync.
func settingsMarketCatLabel(cat string) string {
	for _, option := range marketCategoryOptions() {
		if option.value == cat {
			return option.label
		}
	}
	return cat
}

// settingsMarketVisible filters the static catalog in memory by category
// plus every query token over name, id, description, and category.
func settingsMarketVisible(st *settingsCatalogUIState) []settingsMarketEntry {
	query := strings.ToLower(strings.TrimSpace(st.marketQuery))
	visible := make([]settingsMarketEntry, 0, len(settingsMarketCatalog))
	for _, entry := range settingsMarketCatalog {
		if st.marketCat != "" && entry.category != st.marketCat {
			continue
		}
		if query != "" {
			haystack := strings.ToLower(entry.name + "\n" + entry.id + "\n" + entry.description + "\n" + settingsMarketCatLabel(entry.category))
			matched := true
			for _, token := range strings.Fields(query) {
				if !strings.Contains(haystack, token) {
					matched = false
					break
				}
			}
			if !matched {
				continue
			}
		}
		visible = append(visible, entry)
	}
	return visible
}

// settingsMarketSelected keeps a valid selection, else the first row. It
// never writes state: callers highlight and preview from the result.
func settingsMarketSelected(st *settingsCatalogUIState, visible []settingsMarketEntry) *settingsMarketEntry {
	for i := range visible {
		if visible[i].id == st.marketSel {
			return &visible[i]
		}
	}
	if len(visible) > 0 {
		return &visible[0]
	}
	return nil
}

func settingsMarketFooterCaption(shown, total int) string {
	if shown == total {
		return strconv.Itoa(total) + " servers"
	}
	return strconv.Itoa(shown) + " of " + strconv.Itoa(total)
}

// settingsMarketList is the marketplace roster: header, h32 search, wrap
// chips in FILTERS order, grouped or flat rows, and the real count.
func (a *app) settingsMarketList(c *ui.Context) {
	st := &a.settings.catalog
	mailPaneHeader(c, "MCP Marketplace")
	ui.Column(c).FillWidth().Padding(10, 12, 0, 12).Gap(8).Children(func() {
		settingsSearchInput(c, "market-search", tr("Search marketplace"), &st.marketQuery)
		ui.Row(c).FillWidth().Wrap().Gap(4).Children(func() {
			for _, option := range marketCategoryOptions() {
				key := option.value
				if key == "" {
					key = "all"
				}
				chip := componentTextButton(c, "settings-market-cat-"+key, option.label, false).
					Height(24).Padding(0, 8).Radius(7).FontSize(10.5).Border(0, ui.Transparent)
				if st.marketCat == option.value {
					chip.Background(palette.sidebarItem).TextColor(palette.text).Label(option.label + tr(" selected"))
				}
				if chip.Clicked() {
					st.marketCat = option.value
					st.marketList = ui.ListState{}
				}
			}
		})
	})
	visible := settingsMarketVisible(st)
	selected := settingsMarketSelected(st, visible)
	type marketRow struct {
		key, section string
		first        bool
		entry        settingsMarketEntry
	}
	rows := make([]marketRow, 0, len(visible)+len(settingsMarketGroupOrder))
	for _, cat := range settingsMarketGroupOrder {
		count := 0
		for _, entry := range visible {
			if entry.category == cat {
				count++
			}
		}
		if count == 0 {
			continue
		}
		if st.marketCat == "" {
			rows = append(rows, marketRow{key: "section-" + cat, section: settingsMarketCatLabel(cat) + " " + strconv.Itoa(count), first: len(rows) == 0})
		}
		for _, entry := range visible {
			if entry.category == cat {
				rows = append(rows, marketRow{key: entry.id, entry: entry})
			}
		}
	}
	if len(rows) == 0 {
		ui.Column(c).FillWidth().Grow(1).Children(func() { mailStatusRow(c, "No matching servers") })
	} else {
		c.Key("settings-market-list")
		ui.List(c, &st.marketList, len(rows)).FillWidth().Grow(1).MinHeight(0).
			ItemKey(func(index int) any { return rows[index].key }).Rows(func(row ui.ListRow) {
			item := rows[row.Index]
			c := row.Context
			ui.Column(c).FillWidth().Padding(0, 8).Children(func() {
				if item.section != "" {
					mailSectionLabel(c, item.section, item.first)
					return
				}
				a.settingsMarketRow(c, item.entry, selected != nil && selected.id == item.entry.id)
			})
		})
	}
	mailFooter(c, settingsMarketFooterCaption(len(visible), len(settingsMarketCatalog)), true)
}

// settingsMarketRow is one roster entry: 28 tile, 12.5 title, 10.5
// category caption. A click selects immediately with a stable key.
func (a *app) settingsMarketRow(c *ui.Context, entry settingsMarketEntry, selected bool) {
	st := &a.settings.catalog
	c.Key("settings-market-row-" + entry.id)
	row := ui.ButtonBase(c).Label(entry.name).FillWidth().Padding(7, 9).
		Radius(8).Gap(9).Justify(ui.Start).AlignItems(ui.Center)
	if selected {
		row.Background(palette.sidebarItem)
	} else {
		if row.Hovered() {
			row.Background(palette.overlay)
		}
		if row.Pressed() {
			row.Background(palette.overlayStrong)
		}
	}
	row.Children(func() {
		mailTile(c, entry.icon, selected)
		ui.Column(c).Grow(1).MinWidth(0).Children(func() {
			ui.Text(c, entry.name).FontSize(12.5).FontWeight(500).
				TextColor(palette.text).SingleLine().Ellipsis("...")
			ui.Text(c, settingsMarketCatLabel(entry.category)).FontSize(10.5).
				TextColor(palette.textTertiary).SingleLine().Ellipsis("...").Margin(1, 0, 0, 0)
		})
	})
	if row.Clicked() {
		st.marketSel = entry.id
	}
}

// settingsMarketBadge is a quiet 9.5 tag with text status, never color-only.
func settingsMarketBadge(c *ui.Context, label string) {
	badge := ui.Row(c).Padding(2, 7).Radius(7).AlignItems(ui.Center).
		Background(palette.overlay)
	badge.Children(func() {
		ui.Text(c, label).FontSize(9.5).TextColor(palette.textTertiary)
	})
}

// settingsMarketDetail previews the effective entry: 38 tile, 15 title,
// text badges for availability, source, and auth, the description, a
// bordered source box, and the disabled Add plus click-to-open source.
func (a *app) settingsMarketDetail(c *ui.Context) {
	st := &a.settings.catalog
	entry := settingsMarketSelected(st, settingsMarketVisible(st))
	if entry == nil {
		mailDetailPlaceholder(c, "search", "No matching servers")
		return
	}
	sourceLabel := "Open official page"
	if !entry.official {
		sourceLabel = "Open on Smithery"
	}
	ui.Scroll(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
		ui.Column(c).FillWidth().Padding(18, 24, 24, 24).Children(func() {
			ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Start).Children(func() {
				mailDetailTile(c, entry.icon, 17)
				ui.Column(c).Grow(1).MinWidth(0).Gap(6).Children(func() {
					ui.Text(c, entry.name).FontSize(15).FontWeight(500).
						TextColor(palette.text)
					ui.Row(c).FillWidth().Wrap().Gap(6).AlignItems(ui.Center).Children(func() {
						mailEnabledBadge(c, false, "Catalog entry")
						settingsMarketBadge(c, settingsMarketCatLabel(entry.category))
						if entry.official {
							settingsMarketBadge(c, "Official")
						} else {
							settingsMarketBadge(c, "Hosted")
						}
						if entry.needsAuth {
							auth := ui.Row(c).Padding(2, 7).Radius(7).AlignItems(ui.Center).
								Background(palette.overlay)
							auth.Children(func() {
								icon(c, "lock", 10, palette.textTertiary)
								ui.Text(c, tr(" Needs auth")).FontSize(9.5).
									TextColor(palette.textTertiary)
							})
						}
					})
				})
			})
			ui.Text(c, entry.description).FontSize(13).FixedLineHeight(20).
				TextColor(palette.textSecondary).Wrap().Margin(14, 0, 0, 0)
			ui.Column(c).FillWidth().Gap(4).Margin(12, 0, 0, 0).Children(func() {
				ui.Text(c, tr("SOURCE")).FontSize(9.5).FontWeight(600).
					TextColor(palette.textTertiary)
				ui.Column(c).FillWidth().Gap(2).Radius(7).Padding(8, 10).
					Border(1, palette.border).Children(func() {
					ui.Text(c, entry.sourceLabel).Font(monoFont()).FontSize(11.5).
						TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
					ui.Text(c, entry.url).Font(monoFont()).FontSize(11).
						TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
				})
			})
			ui.Row(c).FillWidth().Wrap().Gap(8).AlignItems(ui.Center).
				Margin(18, 0, 0, 0).Padding(14, 0, 0, 0).
				BorderWidth(1, 0, 0, 0).BorderColor(palette.border).Children(func() {
				installed := false
				for _, server := range a.settings.backend.mcpServers {
					if server.Name == entry.id {
						installed = true
						break
					}
				}
				componentTextButton(c, "settings-market-install-"+entry.id, choice(installed, tr("Open server"), tr("Add")), false).
					Height(28).Padding(0, 10).Radius(6).Disabled(!a.settings.backend.mcp.loaded || a.settings.backend.mcp.writing || a.settings.backend.mcp.uncertain).OnClick(func() { a.installSettingsMarket(*entry) })
				source := componentTextButton(c, "settings-market-source-"+entry.id, sourceLabel, false).
					Height(28).Padding(0, 10).Radius(6)
				if source.Clicked() {
					c.Services().OpenURL(entry.sourceURL)
				}
			})
			settingsOperationStatus(c, a.settings.backend.mcp, "")
			if entry.needsAuth {
				ui.Text(c, tr("Authentication may require OAuth or provider headers. Configure them in MCP Servers.")).FontSize(11).TextColor(palette.textSecondary).Wrap()
			}
		})
	})
}

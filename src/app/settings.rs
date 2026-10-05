use crate::theme::{ThemeScheme, ui_px};

use gpui::actions;

use super::composer::next_picker_highlight;
use super::*;

actions!(fintwind_settings, [ClearSearch]);

const SETTINGS_CONTENT_MAX_WIDTH: f32 = 760.0;

/// Key context the settings sidebar declares around its search field.
const SETTINGS_SIDEBAR_CONTEXT: &str = "SettingsSidebar";

/// The search field while focused inside the sidebar. The field holds real
/// focus the whole time — the sidebar's selection is only drawn — so `up` and
/// `down` have to be claimed from under it, and only a binding can do that:
/// they arrive as actions, which consume the keystroke before the field sees
/// it.
const SETTINGS_SEARCH_CONTEXT: &str = "SettingsSidebar > ComposerInput";

/// The sidebar's rows in display order, each with the keyword haystack the
/// search field filters against.
const SETTINGS_PAGES: [(SettingsPage, &str, &str, &str); 7] = [
    (
        SettingsPage::General,
        "settings.general",
        "icons/settings.svg",
        "settings.general_keywords",
    ),
    (
        SettingsPage::Appearance,
        "settings.appearance",
        "icons/appearance.svg",
        "settings.appearance_keywords",
    ),
    (
        SettingsPage::Providers,
        "settings.providers",
        "icons/bot.svg",
        "settings.providers_keywords",
    ),
    (
        SettingsPage::Skills,
        "settings.skills",
        "icons/package.svg",
        "settings.skills_keywords",
    ),
    (
        SettingsPage::McpServers,
        "settings.mcp_servers",
        "icons/wrench.svg",
        "settings.mcp_servers_keywords",
    ),
    (
        SettingsPage::McpMarket,
        "settings.mcp_market",
        "icons/sparkle.svg",
        "settings.mcp_market_keywords",
    ),
    (
        SettingsPage::Usage,
        "settings.usage",
        "icons/chart-column.svg",
        "settings.usage_keywords",
    ),
];

/// Bind the search field's list-navigation keys. Called once at startup.
pub fn init(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("down", SelectNextEntry, Some(SETTINGS_SEARCH_CONTEXT)),
        KeyBinding::new("up", SelectPreviousEntry, Some(SETTINGS_SEARCH_CONTEXT)),
        // Two-stage escape: the first press clears the query, and on an empty
        // field the handler propagates, so the keystroke falls through to
        // `CancelTurn`, which closes settings.
        KeyBinding::new("escape", ClearSearch, Some(SETTINGS_SEARCH_CONTEXT)),
    ]);
}

fn font_family_label(family: &str) -> String {
    match family {
        crate::theme::DEFAULT_UI_FONT_FAMILY => tr!("settings.font_system"),
        ".SystemUIFontMonospaced" => tr!("settings.font_system_mono"),
        _ => family.to_owned(),
    }
}

fn available_font_families(cx: &App, keep: &[&str]) -> Vec<String> {
    let mut names = cx.text_system().all_font_names();
    names.extend(keep.iter().map(|family| (*family).to_owned()));
    names.retain(|name| !name.starts_with('.') || keep.contains(&name.as_str()));
    names.sort();
    names.dedup();
    for (index, family) in keep.iter().enumerate() {
        if let Some(pos) = names.iter().position(|name| name == family) {
            let item = names.remove(pos);
            names.insert(index, item);
        }
    }
    names
}

/// One dot of a scheme preview, ringed by readable menu text so even a
/// near-white or near-black surface stays visible against the card.
fn scheme_swatch_dot(color: Hsla, ring: Hsla) -> Div {
    div()
        .size(px(10.0))
        .flex_none()
        .rounded_full()
        .bg(color)
        .border_1()
        .border_color(ring)
}

/// One row of the scheme menu. A plain entry can only name a palette, and a
/// dozen names alone ask the user to remember colors; the swatch shows the
/// palette resolved against the appearance currently in force — exactly what
/// choosing it would apply — and the trailing check mirrors the plain entries
/// around it.
fn scheme_choice(
    scheme: ThemeScheme,
    selected_scheme: ThemeScheme,
    is_dark: bool,
    theme: Theme,
) -> MenuItem {
    let palette = Theme::for_scheme(scheme, is_dark);
    let selected = scheme == selected_scheme;
    MenuItem::custom(move |_, _| {
        div()
            .w(px(228.0))
            .py(px(2.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .child(scheme_swatch_dot(palette.canvas, theme.text_secondary))
                    .child(scheme_swatch_dot(palette.raised, theme.text_secondary))
                    .child(scheme_swatch_dot(palette.accent, theme.text_secondary)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .font_weight(if selected {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
                    .child(scheme.label()),
            )
            .when(selected, |element| {
                element.child(icon("icons/check.svg", 12.0, theme.text_tertiary))
            })
            .into_any_element()
    })
}

/// The sidebar rows the query leaves visible, in display order. `query` must
/// already be trimmed and lowercased; when it is empty every page matches.
pub(super) fn visible_settings_pages(
    query: &str,
) -> impl Iterator<Item = (SettingsPage, String, &'static str)> + '_ {
    SETTINGS_PAGES
        .into_iter()
        .filter_map(move |(page, label_key, icon, keywords_key)| {
            let label = crate::i18n::translate(label_key);
            let keywords = crate::i18n::translate(keywords_key).to_lowercase();
            (query.is_empty() || keywords.contains(query)).then_some((page, label, icon))
        })
}

impl Fintwind {
    /// Switch the workspace to the settings mode and show `page` in it.
    /// Every entry point — the rail, the shortcut, the command palette —
    /// funnels through here, so the mode is the single navigation state.
    pub(super) fn open_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.mode = WorkspaceMode::Settings;
        self.settings_page = page;
        // Each page starts at its own top; a scroll position carried over
        // from the previous page would land mid-content.
        self.settings_scroll.set_offset(gpui::Point::default());
        if page == SettingsPage::Skills {
            self.ensure_skills_catalog(false, cx);
        }
        if page == SettingsPage::Providers {
            // A half-finished rename, model edit, or add form never survives
            // the visit; the roster selection itself does.
            self.reset_providers_page(cx);
        }
        if page == SettingsPage::McpServers {
            // Same contract as Providers: transient editors drop, the roster
            // selection survives, and the config is re-read so entries added
            // with the CLI are already on the list.
            self.reset_mcp_page(cx);
        }
        if page == SettingsPage::McpMarket {
            self.mcp_list_scroll.set_offset(gpui::Point::default());
            self.mcp_detail_scroll.set_offset(gpui::Point::default());
            self.load_mcp_servers_from_config(cx);
        }
        if page == SettingsPage::Usage {
            // A stored scan inside the staleness window serves immediately;
            // an expired one refreshes in the background. While the page
            // stays open it re-checks on its own, so a session running
            // elsewhere shows up without a manual refresh.
            self.ensure_usage_stats(false, cx);
            self.start_usage_auto_refresh(cx);
        }
        cx.notify();
    }

    /// The settings mode's second column: search field plus the category
    /// list. The window's main area renders the selected page's content; the
    /// fixed mode rail is the way back to the session, so there is no
    /// separate back button here, and the unified top bar owns the window
    /// chrome above.
    pub(super) fn render_settings_secondary(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let current_page = self.settings_page;
        let query = self.settings_search_query(cx);
        let mut navigation = div().flex().flex_col().gap(px(3.0));

        for (page, label, icon_path) in visible_settings_pages(&query) {
            let selected = current_page == page;
            navigation = navigation.child(
                div()
                    .id(SharedString::from(format!(
                        "settings-tab-{}",
                        label.to_lowercase()
                    )))
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
                    .h(px(38.0))
                    .px(px(11.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .cursor_default()
                    .text_size(ui_px(13.5))
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(selected, |element| {
                        element.bg(theme.sidebar_item_background)
                    })
                    .hover(|element| element.bg(theme.sidebar_item_background))
                    .active(|element| element.bg(theme.sidebar_item_background))
                    .child(icon(
                        icon_path,
                        16.0,
                        if selected {
                            theme.text_secondary
                        } else {
                            theme.text_tertiary
                        },
                    ))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_settings_page(page, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.open_settings_page(page, cx);
                            cx.stop_propagation();
                        }
                    })),
            );
        }

        div()
            .key_context(SETTINGS_SIDEBAR_CONTEXT)
            .track_focus(&self.settings_focus)
            .on_action(cx.listener(|this, _: &SelectNextEntry, _, cx| {
                this.cycle_settings_page("down", cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousEntry, _, cx| {
                this.cycle_settings_page("up", cx);
            }))
            .on_action(cx.listener(|this, _: &ClearSearch, _, cx| {
                if this.settings_search.read(cx).content().is_empty() {
                    cx.propagate();
                    return;
                }
                // `clear` emits `Edited`, and the app's subscription turns
                // that into the notify that re-expands the filtered list.
                this.settings_search.update(cx, |input, cx| input.clear(cx));
            }))
            .w(px(self.settings_width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .child(
                div().px(px(12.0)).pt(px(12.0)).child(
                    TextField::new("settings-search-field", self.settings_search.clone())
                        .icon("icons/search.svg", 13.0),
                ),
            )
            .child(div().h(px(18.0)))
            .child(div().px(px(12.0)).child(navigation))
    }

    /// The search field's content, normalized the way the page filter expects.
    fn settings_search_query(&self, cx: &App) -> String {
        self.settings_search
            .read(cx)
            .content()
            .trim()
            .to_lowercase()
    }

    /// Step the selected page through the rows the search leaves visible,
    /// wrapping at both ends. The field keeps focus so typing keeps narrowing
    /// the list; the landing page renders immediately, so there is no separate
    /// confirm step. A selection filtered out by the query re-enters the list
    /// from whichever end matches the key.
    fn cycle_settings_page(&mut self, key: &str, cx: &mut Context<Self>) {
        let query = self.settings_search_query(cx);
        let pages = visible_settings_pages(&query)
            .map(|(page, ..)| page)
            .collect::<Vec<_>>();
        let current_page = self.settings_page;
        let current = pages.iter().position(|page| *page == current_page);
        let Some(next) = next_picker_highlight(current, pages.len(), key) else {
            return;
        };
        self.open_settings_page(pages[next], cx);
    }

    pub(super) fn render_settings_content(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let page = self.settings_page;
        // The Skills, Providers, and MCP pages are mail-style splits that own
        // the whole content column — no page title, no titlebar strip, no
        // width cap, no card. Window chrome stays in the unified top bar.
        if matches!(
            page,
            SettingsPage::Skills
                | SettingsPage::Providers
                | SettingsPage::McpServers
                | SettingsPage::McpMarket
        ) {
            return div()
                .flex_1()
                .h_full()
                .min_w_0()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme.sidebar_border)
                .bg(theme.surface)
                .child(div().flex_1().min_h_0().child(match page {
                    SettingsPage::Skills => self.render_skills_settings(window, cx),
                    SettingsPage::McpServers => self.render_mcp_page(window, cx),
                    SettingsPage::McpMarket => self.render_mcp_market_page(cx),
                    _ => self.render_providers_page(cx),
                }));
        }
        // Once content scrolls under the unified top bar, a hairline marks
        // the boundary so the clip edge reads as a header rather than a
        // glitch.
        let content_scrolled = self.settings_scroll.offset().y < px(-1.0);

        let inner = div()
            .w_full()
            .max_w(px(SETTINGS_CONTENT_MAX_WIDTH))
            .mx_auto()
            .child(
                div()
                    .pt(px(2.0))
                    .flex_none()
                    .text_size(ui_px(18.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(match page {
                        SettingsPage::General => tr!("settings.general"),
                        SettingsPage::Providers => tr!("settings.providers"),
                        SettingsPage::Skills => tr!("settings.skills"),
                        SettingsPage::McpServers => tr!("settings.mcp_servers"),
                        SettingsPage::McpMarket => tr!("settings.mcp_market"),
                        SettingsPage::Usage => tr!("settings.usage"),
                        SettingsPage::Appearance => tr!("settings.appearance"),
                    }),
            )
            .child(match page {
                SettingsPage::General => self.render_general_settings(window, cx),
                SettingsPage::Providers => self.render_providers_page(cx),
                SettingsPage::Skills => self.render_skills_settings(window, cx),
                SettingsPage::McpServers => self.render_mcp_page(window, cx),
                SettingsPage::McpMarket => self.render_mcp_market_page(cx),
                SettingsPage::Usage => self.render_usage_page(cx),
                SettingsPage::Appearance => self.render_appearance_settings(cx),
            });

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .relative()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.sidebar_border)
            .bg(theme.surface)
            .when(content_scrolled, |element| {
                element.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(1.0))
                        .bg(theme.border),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("settings-content-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.settings_scroll)
                            .pb(px(48.0))
                            .px(px(32.0))
                            .child(inner),
                    )
                    .child(scrollbar::vertical(
                        &self.settings_scroll,
                        &self.settings_scrollbar,
                    )),
            )
    }

    fn render_general_settings(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        div()
            .child(
                div()
                    .mt(px(15.0))
                    .w_full()
                    .px(px(20.0))
                    .py(px(14.0))
                    .rounded(px(13.0))
                    .bg(theme.raised)
                    .child(
                        div()
                            .text_size(ui_px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(tr!("settings.local_by_default")),
                    )
                    .child(
                        div()
                            .mt(px(5.0))
                            .text_size(ui_px(12.5))
                            .line_height(ui_px(18.0))
                            .text_color(theme.text_secondary)
                            .child(tr!("settings.local_by_default_description")),
                    ),
            )
            .child(
                div()
                    .mt(px(12.0))
                    .w_full()
                    .min_h(px(60.0))
                    .px(px(20.0))
                    .py(px(12.0))
                    .rounded(px(13.0))
                    .bg(theme.raised)
                    .flex()
                    .items_center()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(tr!("settings.browser_tools")),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(ui_px(12.5))
                                    .line_height(ui_px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("settings.browser_tools_description")),
                            ),
                    )
                    .child(toggle_switch(
                        "browser-tools-toggle",
                        self.state.browser_tools_enabled,
                        false,
                        theme,
                        window,
                        cx,
                        move |this: &mut Self, _, cx| {
                            this.set_browser_tools_enabled(
                                !this.state.browser_tools_enabled,
                                cx,
                            );
                        },
                    )),
            )
            .child(
                div()
                    .mt(px(12.0))
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_tertiary)
                    .child(tr!(
                        "settings.app_version",
                        version = crate::update::APP_VERSION
                    )),
            )
            .into_any_element()
    }

    fn render_appearance_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let selected_theme = self.state.theme;
        let selected_scheme = self.state.theme_scheme;
        let selected_language = self.state.language;
        let weak = cx.entity().downgrade();
        let theme_handle = self.menu_handle("theme-selector", cx);
        let theme_selector = dropdown_menu(
            MenuChip::new("theme-selector")
                .label(selected_theme.label())
                .outlined()
                .selected(theme_handle.is_open())
                .w(px(116.0))
                .justify_between(),
            "theme-selector-menu",
            &theme_handle,
            MenuAlign::BelowRight,
            move |_| {
                ThemePreference::ALL
                    .into_iter()
                    .map(|preference| {
                        let weak = weak.clone();
                        MenuItem::new(preference.label(), move |window, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.set_theme_preference(preference, window, cx);
                            });
                        })
                        .selected(preference == selected_theme)
                    })
                    .collect()
            },
        );

        let weak = cx.entity().downgrade();
        let scheme_handle = self.menu_handle("scheme-selector", cx);
        // A dozen schemes outgrow the 160px the other chips use: the wider chip
        // keeps a long palette name on one line before it ellipsizes.
        let scheme_selector = dropdown_menu(
            MenuChip::new("scheme-selector")
                .label(selected_scheme.label())
                .outlined()
                .selected(scheme_handle.is_open())
                .w(px(200.0))
                .justify_between(),
            "scheme-selector-menu",
            &scheme_handle,
            MenuAlign::BelowRight,
            move |_| {
                ThemeScheme::ALL
                    .into_iter()
                    .map(|scheme| {
                        let weak = weak.clone();
                        let choice = scheme_choice(scheme, selected_scheme, theme.is_dark, theme);
                        choice.on_click(move |window, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.set_theme_scheme(scheme, window, cx);
                            });
                        })
                    })
                    .collect()
            },
        );

        let weak = cx.entity().downgrade();
        let language_handle = self.menu_handle("language-selector", cx);
        let language_selector = dropdown_menu(
            MenuChip::new("language-selector")
                .label(selected_language.label())
                .outlined()
                .selected(language_handle.is_open())
                .w(px(116.0))
                .justify_between(),
            "language-selector-menu",
            &language_handle,
            MenuAlign::BelowRight,
            move |_| {
                crate::i18n::AppLanguage::ALL
                    .into_iter()
                    .map(|language| {
                        let weak = weak.clone();
                        MenuItem::new(language.label(), move |window, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.set_language(language, window, cx);
                            });
                        })
                        .selected(language == selected_language)
                    })
                    .collect()
            },
        );

        let selected_ui_text_scale = self.state.ui_text_scale;
        let weak = cx.entity().downgrade();
        let ui_text_size_handle = self.menu_handle("ui-text-size-selector", cx);
        let ui_text_size_selector = dropdown_menu(
            MenuChip::new("ui-text-size-selector")
                .label(TextSizePreset::for_scale(selected_ui_text_scale).label())
                .outlined()
                .selected(ui_text_size_handle.is_open())
                .w(px(116.0))
                .justify_between(),
            "ui-text-size-selector-menu",
            &ui_text_size_handle,
            MenuAlign::BelowRight,
            move |_| {
                TextSizePreset::ALL
                    .into_iter()
                    .map(|preset| {
                        let weak = weak.clone();
                        MenuItem::new(preset.label(), move |window, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.set_ui_text_scale(preset.scale(), window, cx);
                            });
                        })
                        .selected(preset.scale() == selected_ui_text_scale)
                    })
                    .collect()
            },
        );

        let selected_code_text_scale = self.state.code_text_scale;
        let weak = cx.entity().downgrade();
        let code_text_size_handle = self.menu_handle("code-text-size-selector", cx);
        let code_text_size_selector = dropdown_menu(
            MenuChip::new("code-text-size-selector")
                .label(TextSizePreset::for_scale(selected_code_text_scale).label())
                .outlined()
                .selected(code_text_size_handle.is_open())
                .w(px(116.0))
                .justify_between(),
            "code-text-size-selector-menu",
            &code_text_size_handle,
            MenuAlign::BelowRight,
            move |_| {
                TextSizePreset::ALL
                    .into_iter()
                    .map(|preset| {
                        let weak = weak.clone();
                        MenuItem::new(preset.label(), move |window, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.set_code_text_scale(preset.scale(), window, cx);
                            });
                        })
                        .selected(preset.scale() == selected_code_text_scale)
                    })
                    .collect()
            },
        );

        let selected_ui_font = self.state.ui_font_family.clone();
        let weak = cx.entity().downgrade();
        let ui_font_handle = self.menu_handle("ui-font-selector", cx);
        let ui_font_selector = dropdown_menu(
            MenuChip::new("ui-font-selector")
                .label(font_family_label(&selected_ui_font))
                .outlined()
                .selected(ui_font_handle.is_open())
                .w(px(200.0))
                .justify_between(),
            "ui-font-selector-menu",
            &ui_font_handle,
            MenuAlign::BelowRight,
            {
                let selected_ui_font = selected_ui_font.clone();
                move |cx| {
                    let mut names =
                        available_font_families(cx, &[crate::theme::DEFAULT_UI_FONT_FAMILY]);
                    if !names.iter().any(|name| name == &selected_ui_font) {
                        names.insert(1.min(names.len()), selected_ui_font.clone());
                    }
                    names
                        .into_iter()
                        .map(|family| {
                            let weak = weak.clone();
                            let selected = family == selected_ui_font;
                            MenuItem::new(font_family_label(&family), move |window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.set_ui_font_family(family.clone(), window, cx);
                                });
                            })
                            .selected(selected)
                        })
                        .collect()
                }
            },
        );

        let selected_code_font = self.state.code_font_family.clone();
        let weak = cx.entity().downgrade();
        let code_font_handle = self.menu_handle("code-font-selector", cx);
        let code_font_selector = dropdown_menu(
            MenuChip::new("code-font-selector")
                .label(font_family_label(&selected_code_font))
                .outlined()
                .selected(code_font_handle.is_open())
                .w(px(200.0))
                .justify_between(),
            "code-font-selector-menu",
            &code_font_handle,
            MenuAlign::BelowRight,
            {
                let selected_code_font = selected_code_font.clone();
                move |cx| {
                    let keep = [
                        crate::theme::DEFAULT_CODE_FONT_FAMILY,
                        ".SystemUIFontMonospaced",
                    ];
                    let mut names = available_font_families(cx, &keep);
                    if !names.iter().any(|name| name == &selected_code_font) {
                        names.insert(keep.len().min(names.len()), selected_code_font.clone());
                    }
                    names
                        .into_iter()
                        .map(|family| {
                            let weak = weak.clone();
                            let selected = family == selected_code_font;
                            MenuItem::new(font_family_label(&family), move |window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.set_code_font_family(family.clone(), window, cx);
                                });
                            })
                            .selected(selected)
                        })
                        .collect()
                }
            },
        );

        div()
            .mt(px(15.0))
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(13.0))
            .overflow_hidden()
            .bg(theme.raised)
            .child(
                div()
                    .w_full()
                    .min_h(px(60.0))
                    .px(px(20.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(tr!("settings.theme")),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(ui_px(12.5))
                                    .line_height(ui_px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("settings.theme_description")),
                            ),
                    )
                    .child(theme_selector),
            )
            .child(div().mx(px(20.0)).h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .w_full()
                    .min_h(px(60.0))
                    .px(px(20.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(tr!("settings.scheme")),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(ui_px(12.5))
                                    .line_height(ui_px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("settings.scheme_description")),
                            ),
                    )
                    .child(scheme_selector),
            )
            .child(div().mx(px(20.0)).h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .w_full()
                    .min_h(px(60.0))
                    .px(px(20.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(tr!("language.title")),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(ui_px(12.5))
                                    .line_height(ui_px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("language.description")),
                            ),
                    )
                    .child(language_selector),
            )
            .child(div().mx(px(20.0)).h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .w_full()
                    .min_h(px(60.0))
                    .px(px(20.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(tr!("settings.ui_text_size")),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(ui_px(12.5))
                                    .line_height(ui_px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("settings.ui_text_size_description")),
                            ),
                    )
                    .child(ui_text_size_selector),
            )
            .child(div().mx(px(20.0)).h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .w_full()
                    .min_h(px(60.0))
                    .px(px(20.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(tr!("settings.ui_font")),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(ui_px(12.5))
                                    .line_height(ui_px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("settings.ui_font_description")),
                            ),
                    )
                    .child(ui_font_selector),
            )
            .child(div().mx(px(20.0)).h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .w_full()
                    .min_h(px(60.0))
                    .px(px(20.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(tr!("settings.code_text_size")),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(ui_px(12.5))
                                    .line_height(ui_px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("settings.code_text_size_description")),
                            ),
                    )
                    .child(code_text_size_selector),
            )
            .child(div().mx(px(20.0)).h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .w_full()
                    .min_h(px(60.0))
                    .px(px(20.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(tr!("settings.code_font")),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(ui_px(12.5))
                                    .line_height(ui_px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("settings.code_font_description")),
                            ),
                    )
                    .child(code_font_selector),
            )
            .into_any_element()
    }

    /// While off, the plugin keeps its tools and instruction out of model
    /// context entirely. Saving pushes the daemon setting, which forwards the
    /// new state to connected plugins in time for the next request.
    fn set_browser_tools_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.state.browser_tools_enabled == enabled {
            return;
        }
        self.state.browser_tools_enabled = enabled;
        self.save();
        cx.notify();
    }

    /// Switch the whole chrome to `scale`. A window refresh re-renders every
    /// view — including child entities a plain notify would leave with their
    /// cached elements — so no surface keeps a stale text size.
    fn set_ui_text_scale(&mut self, scale: f32, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.ui_text_scale == scale {
            return;
        }
        self.state.ui_text_scale = scale;
        crate::theme::set_ui_text_scale(scale);
        self.save();
        window.refresh();
        cx.notify();
    }

    /// Switch every code surface to `scale`. See [`Self::set_ui_text_scale`].
    fn set_code_text_scale(&mut self, scale: f32, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.code_text_scale == scale {
            return;
        }
        self.state.code_text_scale = scale;
        crate::theme::set_code_text_scale(scale);
        self.save();
        window.refresh();
        cx.notify();
    }

    fn set_ui_font_family(&mut self, family: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.ui_font_family == family {
            return;
        }
        self.state.ui_font_family = family.clone();
        crate::theme::set_ui_font_family(family);
        self.save();
        window.refresh();
        cx.notify();
    }

    fn set_code_font_family(
        &mut self,
        family: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.code_font_family == family {
            return;
        }
        self.state.code_font_family = family.clone();
        crate::theme::set_code_font_family(family);
        self.save();
        window.refresh();
        cx.notify();
    }

    fn set_theme_preference(
        &mut self,
        preference: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.theme == preference {
            return;
        }
        self.state.theme = preference;
        crate::theme::apply_theme_preference(preference, self.state.theme_scheme, window, cx);
        self.save();
        cx.notify();
    }

    fn set_theme_scheme(
        &mut self,
        scheme: ThemeScheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.theme_scheme == scheme {
            return;
        }
        self.state.theme_scheme = scheme;
        crate::theme::apply_theme_preference(self.state.theme, scheme, window, cx);
        self.save();
        cx.notify();
    }

    fn set_language(
        &mut self,
        language: crate::i18n::AppLanguage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.language == language {
            return;
        }

        self.state.language = language;
        crate::i18n::set_language(language);

        self.composer.update(cx, |input, cx| {
            input.set_placeholder(tr!("input.do_anything"), cx)
        });
        self.model_search.update(cx, |input, cx| {
            input.set_placeholder(tr!("input.search_models"), cx)
        });
        self.branch_search.update(cx, |input, cx| {
            input.set_placeholder(tr!("input.search_branches"), cx)
        });
        self.branch_create_input.update(cx, |input, cx| {
            input.set_placeholder(tr!("input.new_branch_name"), cx)
        });
        self.settings_search.update(cx, |input, cx| {
            input.set_placeholder(tr!("settings.search"), cx)
        });
        self.skills_search.update(cx, |input, cx| {
            input.set_placeholder(tr!("skills.search"), cx)
        });
        self.refresh_command_palette_localized_text(cx);
        self.refresh_file_search_localized_text(cx);
        for browser in self.right_panel_browsers.values() {
            browser.update(cx, |browser, cx| browser.refresh_localized_text(cx));
        }
        for terminal in self.right_panel_terminals.values() {
            terminal.update(cx, |terminal, cx| terminal.refresh_localized_text(cx));
        }
        for probe in &mut self.probes {
            probe.models = crate::model_catalog::fallback_models();
        }
        self.refresh_provider_detection();
        self.invalidate_composer_sources(cx);

        crate::set_app_menus(cx);
        self.save();
        window.refresh();
        cx.notify();
    }
}

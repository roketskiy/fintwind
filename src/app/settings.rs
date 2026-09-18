use crate::theme::ui_px;

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
const SETTINGS_PAGES: [(SettingsPage, &str, &str, &str); 8] = [
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
    (
        SettingsPage::Daemon,
        "settings.daemon",
        "icons/server.svg",
        "settings.daemon_keywords",
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
    /// Switch the settings view to `page`.
    pub(super) fn open_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        // Secrets are revealed only for the current visit to the page. This
        // also masks the token again when the Daemon row is reselected.
        self.daemon_token_revealed = false;
        self.settings_page = Some(page);
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
            // an expired one refreshes in the background.
            self.ensure_usage_stats(false, cx);
        }
        cx.notify();
    }

    pub(super) fn render_settings(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);

        div()
            .key_context("Fintwind")
            .track_focus(&self.settings_focus)
            .on_action(|_: &CloseWindow, window, _| crate::platform::hide_window(window))
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::new_project_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::toggle_right_panel_action))
            .on_action(cx.listener(Self::toggle_command_palette_action))
            .on_action(cx.listener(Self::toggle_fps_counter_action))
            .on_action(cx.listener(Self::navigate_back_action))
            .on_action(cx.listener(Self::navigate_forward_action))
            .on_action(cx.listener(Self::focus_composer_action))
            .on_action(cx.listener(Self::cancel_turn_action))
            .capture_any_mouse_down(cx.listener(Self::navigation_mouse_down))
            .size_full()
            .flex()
            .bg(theme.canvas)
            .text_color(theme.text)
            .font_family(crate::theme::ui_font_family())
            .child(self.render_settings_sidebar(window, cx))
            .child(self.render_settings_content(window, cx))
            .into_any_element()
    }

    fn render_settings_sidebar(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let current_page = self.settings_page.unwrap_or(SettingsPage::General);
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
                    })),
            );
        }

        div()
            .key_context(SETTINGS_SIDEBAR_CONTEXT)
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
            .w(px(DEFAULT_SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .child(self.render_settings_sidebar_titlebar(window, cx))
            .child(
                div().px(px(12.0)).child(
                    div()
                        .id("settings-back")
                        .h(px(36.0))
                        .px(px(9.0))
                        .rounded(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(9.0))
                        .cursor_default()
                        .text_size(ui_px(13.5))
                        .text_color(theme.text_secondary)
                        .hover(|element| element.bg(theme.overlay))
                        .active(|element| element.bg(theme.overlay_strong))
                        .child(icon("icons/arrow-left.svg", 16.0, theme.text_tertiary))
                        .child(tr!("settings.back"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.settings_page = None;
                            let focus_handle = this.composer_focus(cx);
                            window.focus(&focus_handle, cx);
                            cx.notify();
                        })),
                ),
            )
            .child(
                div().px(px(12.0)).pt(px(8.0)).child(
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
        let current_page = self.settings_page.unwrap_or(SettingsPage::General);
        let current = pages.iter().position(|page| *page == current_page);
        let Some(next) = next_picker_highlight(current, pages.len(), key) else {
            return;
        };
        self.open_settings_page(pages[next], cx);
    }

    fn render_settings_sidebar_titlebar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let left_window_controls = self.render_client_window_controls(
            super::window_chrome::WindowControlSide::Left,
            window,
            cx,
        );
        // Windows keeps all three caption buttons on the far side, so this
        // strip is only somewhere to drag the window by — the content
        // column's own titlebar carries the rest of that job.
        let height = if left_window_controls.is_some() {
            48.0
        } else {
            12.0
        };

        div()
            .id("settings-sidebar-titlebar")
            .h(px(height))
            .flex_none()
            .flex()
            .items_center()
            .children(left_window_controls)
            .child(
                self.window_drag_region(
                    div()
                        .id("settings-sidebar-traffic-light-drag-region")
                        .w(px(TRAFFIC_LIGHT_CLEARANCE))
                        .h_full()
                        .flex_none(),
                    cx,
                ),
            )
            .child(
                self.render_settings_drag_region("settings-sidebar-titlebar-drag-region", cx)
                    .h(px(height))
                    .flex_1(),
            )
    }

    fn render_settings_content(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let page = self.settings_page.unwrap_or(SettingsPage::General);
        let right_window_controls = self.render_client_window_controls(
            super::window_chrome::WindowControlSide::Right,
            window,
            cx,
        );
        // The Skills, Providers, and MCP pages are mail-style splits that own
        // the whole content column — no page title, no titlebar strip, no
        // width cap, no card. Window dragging stays with the sidebar's own
        // titlebar region.
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
                .children(right_window_controls.map(|controls| {
                    self.render_settings_drag_region("settings-split-titlebar", cx)
                        .flex()
                        .items_center()
                        .justify_end()
                        .child(controls)
                }))
                .child(div().flex_1().min_h_0().child(match page {
                    SettingsPage::Skills => self.render_skills_settings(cx),
                    SettingsPage::McpServers => self.render_mcp_page(cx),
                    SettingsPage::McpMarket => self.render_mcp_market_page(cx),
                    _ => self.render_providers_page(cx),
                }));
        }
        // The titlebar strip is transparent; once content slides under it, a
        // hairline marks the boundary so the clip edge reads as a header
        // rather than a glitch.
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
                        SettingsPage::Daemon => tr!("settings.daemon"),
                        SettingsPage::Appearance => tr!("settings.appearance"),
                    }),
            )
            .child(match page {
                SettingsPage::General => self.render_general_settings(cx),
                SettingsPage::Providers => self.render_providers_page(cx),
                SettingsPage::Skills => self.render_skills_settings(cx),
                SettingsPage::McpServers => self.render_mcp_page(cx),
                SettingsPage::McpMarket => self.render_mcp_market_page(cx),
                SettingsPage::Usage => self.render_usage_page(cx),
                SettingsPage::Daemon => self.render_daemon_settings(cx),
                SettingsPage::Appearance => self.render_appearance_settings(cx),
            });

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.sidebar_border)
            .bg(theme.surface)
            .child(
                self.render_settings_drag_region("settings-content-titlebar", cx)
                    .flex()
                    .items_center()
                    .justify_end()
                    .children(right_window_controls)
                    .when(content_scrolled, |element| {
                        element.border_b_1().border_color(theme.border)
                    }),
            )
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

    fn render_general_settings(&self, cx: &mut Context<Self>) -> AnyElement {
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
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_tertiary)
                    .child(tr!(
                        "settings.app_version",
                        version = crate::update::APP_VERSION
                    )),
            )
            .into_any_element()
    }

    fn render_daemon_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        if self.daemon.is_remote() {
            return div()
                .mt(px(15.0))
                .w_full()
                .px(px(20.0))
                .py(px(16.0))
                .rounded(px(13.0))
                .bg(theme.raised)
                .child(
                    div()
                        .text_size(ui_px(13.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(tr!("daemon.external_title")),
                )
                .child(
                    div()
                        .mt(px(5.0))
                        .text_size(ui_px(12.5))
                        .line_height(ui_px(18.0))
                        .text_color(theme.text_secondary)
                        .child(tr!("daemon.external_description")),
                )
                .into_any_element();
        }

        let enabled = self.state.daemon_exposure.enabled;
        let pending = self.daemon_reconfigure_pending;
        let fields_dirty = self.daemon_exposure_fields_dirty(cx);
        let port = self.state.daemon_exposure.port;
        let websocket_url = format!("ws://{}:{port}", self.daemon_hostname);
        let token = self.state.daemon_exposure.token.clone();

        let exposure_toggle = toggle_switch(
            "daemon-exposure-toggle",
            enabled,
            pending,
            theme,
            cx,
            move |this, _, cx| this.set_daemon_exposure_enabled(!enabled, cx),
        );

        let apply_disabled = pending || !fields_dirty;
        let apply_button = div()
            .id("apply-daemon-settings")
            .tab_index(0)
            .h(px(32.0))
            .px(px(13.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .text_size(ui_px(12.0))
            .text_color(theme.text_secondary)
            .opacity(if apply_disabled { 0.55 } else { 1.0 })
            .focus_visible(|style| style.border_color(theme.accent))
            .when(!apply_disabled, |element| {
                element
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.apply_daemon_exposure_fields(cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.apply_daemon_exposure_fields(cx);
                            cx.stop_propagation();
                        }
                    }))
            })
            .child(if pending {
                tr!("daemon.restarting")
            } else {
                tr!("daemon.apply")
            });

        let copy_url_feedback_id = "daemon-url";
        let url_copied = self.control_was_copied(copy_url_feedback_id);
        let copy_url = websocket_url.clone();
        let copy_url_button = div()
            .id("copy-daemon-url")
            .tab_index(0)
            .h(px(30.0))
            .px(px(10.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(ui_px(12.0))
            .text_color(theme.text_secondary)
            .focus_visible(|style| style.border_color(theme.accent))
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon(
                if url_copied {
                    "icons/check.svg"
                } else {
                    "icons/copy.svg"
                },
                11.0,
                theme.text_tertiary,
            ))
            .child(if url_copied {
                tr!("common.copied")
            } else {
                tr!("common.copy")
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                this.show_control_copied(copy_url_feedback_id, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(websocket_url.clone()));
                    this.show_control_copied(copy_url_feedback_id, cx);
                    cx.stop_propagation();
                }
            }));

        let copy_token_feedback_id = "daemon-token";
        let token_copied = self.control_was_copied(copy_token_feedback_id);
        let click_token = token.clone();
        let key_token = token.clone();
        let token_revealed = self.daemon_token_revealed;
        let reveal_token_button = div()
            .id("reveal-daemon-token")
            .tab_index(0)
            .size(px(30.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .text_color(theme.text_secondary)
            .focus_visible(|style| style.border_color(theme.accent))
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon(
                if token_revealed {
                    "icons/eye-off.svg"
                } else {
                    "icons/eye.svg"
                },
                13.5,
                theme.text_tertiary,
            ))
            .tooltip(Tooltip::text(if token_revealed {
                tr!("daemon.hide_token")
            } else {
                tr!("daemon.reveal_token")
            }))
            .on_click(cx.listener(|this, _, _, cx| {
                this.daemon_token_revealed = !this.daemon_token_revealed;
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    this.daemon_token_revealed = !this.daemon_token_revealed;
                    cx.stop_propagation();
                    cx.notify();
                }
            }));
        let copy_token_button = div()
            .id("copy-daemon-token")
            .tab_index(0)
            .h(px(30.0))
            .px(px(10.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(ui_px(12.0))
            .text_color(theme.text_secondary)
            .focus_visible(|style| style.border_color(theme.accent))
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon(
                if token_copied {
                    "icons/check.svg"
                } else {
                    "icons/copy.svg"
                },
                11.0,
                theme.text_tertiary,
            ))
            .child(if token_copied {
                tr!("common.copied")
            } else {
                tr!("common.copy")
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(click_token.clone()));
                this.show_control_copied(copy_token_feedback_id, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(key_token.clone()));
                    this.show_control_copied(copy_token_feedback_id, cx);
                    cx.stop_propagation();
                }
            }));

        let regenerate_button = div()
            .id("regenerate-daemon-token")
            .tab_index(0)
            .h(px(30.0))
            .px(px(10.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .cursor_default()
            .text_size(ui_px(12.0))
            .text_color(theme.text_secondary)
            .opacity(if pending { 0.55 } else { 1.0 })
            .focus_visible(|style| style.border_color(theme.accent))
            .when(!pending, |element| {
                element
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.regenerate_daemon_token(cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.regenerate_daemon_token(cx);
                            cx.stop_propagation();
                        }
                    }))
            })
            .child(tr!("daemon.regenerate_token"));

        div()
            .mt(px(15.0))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .min_h(px(66.0))
                    .px(px(20.0))
                    .py(px(13.0))
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
                                    .flex()
                                    .items_center()
                                    .gap(px(7.0))
                                    .child(
                                        div()
                                            .text_size(ui_px(13.5))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.text)
                                            .child(tr!("daemon.expose_title")),
                                    )
                                    .child(
                                        div()
                                            .px(px(6.0))
                                            .py(px(2.0))
                                            .rounded_full()
                                            .text_size(ui_px(9.5))
                                            .text_color(if enabled {
                                                theme.success
                                            } else {
                                                theme.text_tertiary
                                            })
                                            .bg(theme.overlay)
                                            .child(if pending {
                                                tr!("daemon.status_restarting")
                                            } else if enabled {
                                                tr!("daemon.status_exposed")
                                            } else {
                                                tr!("daemon.status_local")
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_size(ui_px(12.0))
                                    .line_height(ui_px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("daemon.expose_description")),
                            ),
                    )
                    .child(exposure_toggle),
            )
            .when(enabled, |column| {
                column.child(
                    div()
                        .px(px(20.0))
                        .py(px(15.0))
                        .rounded(px(13.0))
                        .bg(theme.raised)
                        .child(
                            div()
                                .text_size(ui_px(13.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(tr!("daemon.connection_title")),
                        )
                        .child(
                            div()
                                .mt(px(4.0))
                                .min_w_0()
                                .whitespace_normal()
                                .text_size(ui_px(11.5))
                                .line_height(ui_px(16.0))
                                .text_color(theme.text_secondary)
                                .child(tr!("daemon.connection_description")),
                        )
                        .child(
                            div()
                                .mt(px(14.0))
                                .flex()
                                .items_start()
                                .gap(px(24.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_size(ui_px(11.0))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme.text)
                                                .child(tr!("daemon.port")),
                                        )
                                        .child(
                                            div()
                                                .mt(px(3.0))
                                                .whitespace_normal()
                                                .text_size(ui_px(10.0))
                                                .line_height(ui_px(14.0))
                                                .text_color(theme.text_tertiary)
                                                .child(tr!("daemon.port_description")),
                                        ),
                                )
                                .child(
                                    div().flex_1().min_w_0().flex().justify_end().child(
                                        TextField::new(
                                            "daemon-port-field",
                                            self.daemon_port_input.clone(),
                                        )
                                        .w(px(150.0)),
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .mt(px(14.0))
                                .flex()
                                .items_start()
                                .gap(px(24.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_size(ui_px(11.0))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme.text)
                                                .child(tr!("daemon.allowed_origins")),
                                        )
                                        .child(
                                            div()
                                                .mt(px(3.0))
                                                .whitespace_normal()
                                                .text_size(ui_px(10.0))
                                                .line_height(ui_px(14.0))
                                                .text_color(theme.text_tertiary)
                                                .child(tr!("daemon.allowed_origins_description")),
                                        ),
                                )
                                .child(
                                    div().flex_1().min_w_0().flex().justify_end().child(
                                        TextField::new(
                                            "daemon-origins-field",
                                            self.daemon_origins_input.clone(),
                                        )
                                        .w_full()
                                        .max_w(px(360.0)),
                                    ),
                                ),
                        )
                        .child(div().mt(px(13.0)).flex().justify_end().child(apply_button)),
                )
            })
            .when(enabled, |column| {
                column.child(
                    div()
                        .px(px(20.0))
                        .py(px(15.0))
                        .rounded(px(13.0))
                        .bg(theme.raised)
                        .child(
                            div()
                                .text_size(ui_px(13.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(tr!("daemon.credentials_title")),
                        )
                        .child(
                            div()
                                .mt(px(4.0))
                                .min_w_0()
                                .whitespace_normal()
                                .text_size(ui_px(11.5))
                                .line_height(ui_px(16.0))
                                .text_color(theme.text_secondary)
                                .child(tr!("daemon.credentials_description")),
                        )
                        .child(
                            div()
                                .mt(px(13.0))
                                .py(px(8.0))
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .child(
                                    div()
                                        .w(px(80.0))
                                        .flex_none()
                                        .text_size(ui_px(10.5))
                                        .text_color(theme.text_tertiary)
                                        .child(tr!("daemon.websocket_url")),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(".SystemUIFontMonospaced")
                                        .text_size(ui_px(11.0))
                                        .text_color(theme.text)
                                        .child(SharedString::from(format!(
                                            "ws://{}:{port}",
                                            self.daemon_hostname
                                        ))),
                                )
                                .child(copy_url_button),
                        )
                        .child(
                            div()
                                .py(px(8.0))
                                .border_t_1()
                                .border_color(theme.border)
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .child(
                                    div()
                                        .w(px(80.0))
                                        .flex_none()
                                        .text_size(ui_px(10.5))
                                        .text_color(theme.text_tertiary)
                                        .child(tr!("daemon.token")),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(".SystemUIFontMonospaced")
                                        .text_size(ui_px(11.0))
                                        .text_color(theme.text)
                                        .child(SharedString::from(if token_revealed {
                                            token.clone()
                                        } else {
                                            "••••••••••••••••••••••••••••••••".to_owned()
                                        })),
                                )
                                .child(reveal_token_button)
                                .child(copy_token_button)
                                .child(regenerate_button),
                        )
                        .child(
                            div()
                                .mt(px(7.0))
                                .px(px(10.0))
                                .py(px(8.0))
                                .rounded(px(8.0))
                                .bg(theme.inset)
                                .w_full()
                                .min_w_0()
                                .flex()
                                .gap(px(8.0))
                                .child(icon("icons/alert.svg", 13.0, theme.warning))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .text_size(ui_px(10.5))
                                        .line_height(ui_px(15.0))
                                        .text_color(theme.text_secondary)
                                        .child(tr!("daemon.security_warning")),
                                ),
                        ),
                )
            })
            .into_any_element()
    }

    fn daemon_exposure_from_fields(
        &self,
        cx: &App,
    ) -> Result<fintwind_client::DaemonExposureSettings, String> {
        let port = self
            .daemon_port_input
            .read(cx)
            .content()
            .trim()
            .parse::<u16>()
            .map_err(|_| tr!("daemon.invalid_port"))?;
        if port == 0 {
            return Err(tr!("daemon.invalid_port"));
        }
        let origins = self.daemon_origins_input.read(cx).content().to_owned();
        let mut settings = self.state.daemon_exposure.clone();
        settings.port = port;
        settings
            .with_allowed_origins_text(&origins)
            .and_then(fintwind_client::DaemonExposureSettings::validate)
            .map_err(|error| error.to_string())
    }

    fn daemon_exposure_fields_dirty(&self, cx: &App) -> bool {
        self.daemon_exposure_from_fields(cx)
            .map(|settings| {
                settings.port != self.state.daemon_exposure.port
                    || settings.allowed_origins != self.state.daemon_exposure.allowed_origins
            })
            .unwrap_or(true)
    }

    fn set_daemon_exposure_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if !enabled {
            self.daemon_token_revealed = false;
        }
        let settings = if enabled {
            match self.daemon_exposure_from_fields(cx) {
                Ok(mut settings) => {
                    settings.enabled = true;
                    settings
                }
                Err(error) => {
                    self.show_toast(tr!("daemon.invalid_settings", error = error));
                    return;
                }
            }
        } else {
            let mut settings = self.state.daemon_exposure.clone();
            settings.enabled = false;
            settings
        };
        self.apply_daemon_exposure(settings, cx);
    }

    pub(super) fn apply_daemon_exposure_fields(&mut self, cx: &mut Context<Self>) {
        let settings = match self.daemon_exposure_from_fields(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.show_toast(tr!("daemon.invalid_settings", error = error));
                return;
            }
        };
        self.apply_daemon_exposure(settings, cx);
    }

    fn regenerate_daemon_token(&mut self, cx: &mut Context<Self>) {
        let mut settings = match self.daemon_exposure_from_fields(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.show_toast(tr!("daemon.invalid_settings", error = error));
                return;
            }
        };
        settings.token = fintwind_client::DaemonExposureSettings::new_token();
        self.daemon_token_revealed = false;
        self.apply_daemon_exposure(settings, cx);
    }

    fn apply_daemon_exposure(
        &mut self,
        settings: fintwind_client::DaemonExposureSettings,
        cx: &mut Context<Self>,
    ) {
        if self.daemon_reconfigure_pending || settings == self.state.daemon_exposure {
            return;
        }
        if self.daemon.is_remote() {
            self.show_toast(tr!("daemon.external_description"));
            return;
        }
        if self
            .state
            .sessions
            .iter()
            .any(|session| !matches!(session.status, SessionStatus::Idle | SessionStatus::Failed))
        {
            self.show_toast(tr!("daemon.stop_active_tasks"));
            return;
        }

        let needs_restart = self.state.daemon_exposure.enabled || settings.enabled;
        if !needs_restart {
            self.state.daemon_exposure = settings;
            self.save();
            cx.notify();
            return;
        }

        self.daemon_reconfigure_pending = true;
        let daemon = self.daemon.clone();
        let applied = settings.clone();
        let restart = cx
            .background_executor()
            .spawn(async move { daemon.reconfigure(settings) });
        cx.spawn(async move |this, cx| {
            let result = restart.await;
            let _ = this.update(cx, |this, cx| {
                this.daemon_reconfigure_pending = false;
                match result {
                    Ok(()) => {
                        this.state.daemon_exposure = applied.clone();
                        this.runtimes.clear();
                        this.daemon_port_input.update(cx, |input, cx| {
                            input.set_content(applied.port.to_string(), cx)
                        });
                        this.daemon_origins_input.update(cx, |input, cx| {
                            input.set_content(applied.allowed_origins_text(), cx)
                        });
                        this.save();
                        this.show_success_toast(tr!("daemon.settings_applied"));
                    }
                    Err(error) => {
                        this.show_toast(tr!("daemon.restart_failed", error = error.to_string()))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_appearance_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let selected_theme = self.state.theme;
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

    fn render_settings_drag_region(
        &self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let region = div().id(id);
        // Windows drags from the hit test rather than a mouse-move handler.
        #[cfg(target_os = "windows")]
        let region = region.window_control_area(gpui::WindowControlArea::Drag);

        region
            .h(px(48.0))
            .flex_none()
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    crate::platform::titlebar_double_click(window);
                }
            })
            .on_mouse_down_out(cx.listener(|this, _, _, _| {
                this.header_drag_armed = false;
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.header_drag_armed = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.header_drag_armed = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.header_drag_armed {
                    this.header_drag_armed = false;
                    crate::platform::start_window_move(window);
                }
            }))
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
        crate::theme::apply_theme_preference(preference, window, cx);
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

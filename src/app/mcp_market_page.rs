//! A curated MCP marketplace in settings: web search, academic research, and
//! code servers — installable as remote servers in OpenCode's `mcp` map.
//!
//! Every listing points at the vendor's own MCP endpoint wherever the vendor
//! runs one. Only the servers no vendor hosts point at a hosting platform's
//! copy, and those say so.

use crate::theme::ui_px;

use gpui::{KeyBinding, KeyDownEvent, actions};

use fintwind_client::custom_providers::unique_provider_slug;
use fintwind_client::opencode_config::{McpServer, McpServerKind};

use super::composer::next_picker_highlight;
use super::providers_page::{
    info_note, outline_button, provider_tile, section_label, small_action_button,
};

use super::*;

actions!(fintwind_mcp_market, [ClearMcpMarketSearch]);

const MARKET_PANE_CONTEXT: &str = "McpMarketPane";
const MARKET_SEARCH_CONTEXT: &str = "McpMarketPane > ComposerInput";
const MARKET_LIST_WIDTH: f32 = 264.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum McpMarketCategory {
    #[default]
    All,
    WebSearch,
    Academic,
    Code,
}

impl McpMarketCategory {
    const FILTERS: [Self; 4] = [Self::All, Self::WebSearch, Self::Academic, Self::Code];

    fn id(self) -> &'static str {
        match self {
            Self::All => "mcp-market-filter-all",
            Self::WebSearch => "mcp-market-filter-web",
            Self::Academic => "mcp-market-filter-academic",
            Self::Code => "mcp-market-filter-code",
        }
    }

    fn label(self) -> String {
        match self {
            Self::All => tr!("mcp_market.filter_all"),
            Self::WebSearch => tr!("mcp_market.filter_web"),
            Self::Academic => tr!("mcp_market.filter_academic"),
            Self::Code => tr!("mcp_market.filter_code"),
        }
    }

    fn section_label(self) -> String {
        match self {
            Self::All => String::new(),
            Self::WebSearch => tr!("mcp_market.filter_web"),
            Self::Academic => tr!("mcp_market.filter_academic"),
            Self::Code => tr!("mcp_market.filter_code"),
        }
    }
}

/// Where a listing's endpoint comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum McpMarketSource {
    /// The vendor runs this MCP server itself.
    Official,
    /// A hosting platform's copy of an upstream project, because no vendor
    /// publishes an endpoint of its own.
    Hosted,
}

struct McpMarketEntry {
    id: &'static str,
    display_name: &'static str,
    category: McpMarketCategory,
    /// The endpoint OpenCode connects to.
    url: &'static str,
    source: McpMarketSource,
    /// The host that serves the endpoint, or the hosting platform for a
    /// hosted listing.
    source_label: &'static str,
    /// The page the source button opens: the vendor's project page for an
    /// official listing, the hosting platform's listing otherwise.
    source_url: &'static str,
    icon: &'static str,
    description_key: &'static str,
    needs_auth: bool,
}

const CATALOG: &[McpMarketEntry] = &[
    McpMarketEntry {
        id: "brave",
        display_name: "Brave Search",
        category: McpMarketCategory::WebSearch,
        url: "https://api.search.brave.com/mcp",
        source: McpMarketSource::Official,
        source_label: "api.search.brave.com",
        source_url: "https://github.com/brave/brave-search-mcp-server",
        icon: "icons/search.svg",
        description_key: "mcp_market.desc.brave",
        needs_auth: true,
    },
    McpMarketEntry {
        id: "exa",
        display_name: "Exa Search",
        category: McpMarketCategory::WebSearch,
        url: "https://mcp.exa.ai/mcp",
        source: McpMarketSource::Official,
        source_label: "mcp.exa.ai",
        source_url: "https://github.com/exa-labs/exa-mcp-server",
        icon: "icons/search.svg",
        description_key: "mcp_market.desc.exa",
        needs_auth: true,
    },
    McpMarketEntry {
        id: "tavily",
        display_name: "Tavily",
        category: McpMarketCategory::WebSearch,
        url: "https://mcp.tavily.com/mcp/",
        source: McpMarketSource::Official,
        source_label: "mcp.tavily.com",
        source_url: "https://github.com/tavily-ai/tavily-mcp",
        icon: "icons/search.svg",
        description_key: "mcp_market.desc.tavily",
        needs_auth: true,
    },
    McpMarketEntry {
        id: "keenable-web-search",
        display_name: "Keenable Web Search",
        category: McpMarketCategory::WebSearch,
        url: "https://api.keenable.ai/mcp",
        source: McpMarketSource::Official,
        source_label: "api.keenable.ai",
        source_url: "https://github.com/keenableai/keenable-mcp",
        icon: "icons/globe.svg",
        description_key: "mcp_market.desc.keenable",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "parallel-search",
        display_name: "Parallel Web Search",
        category: McpMarketCategory::WebSearch,
        url: "https://mcp.parallel.ai/mcp",
        source: McpMarketSource::Official,
        source_label: "mcp.parallel.ai",
        source_url: "https://github.com/parallel-web/search-mcp",
        icon: "icons/search.svg",
        description_key: "mcp_market.desc.parallel",
        needs_auth: true,
    },
    McpMarketEntry {
        id: "linkup",
        display_name: "Linkup",
        category: McpMarketCategory::WebSearch,
        url: "https://mcp.linkup.so/mcp",
        source: McpMarketSource::Official,
        source_label: "mcp.linkup.so",
        source_url: "https://github.com/LinkupPlatform/linkup-mcp-server",
        icon: "icons/globe.svg",
        description_key: "mcp_market.desc.linkup",
        needs_auth: true,
    },
    McpMarketEntry {
        id: "brightdata",
        display_name: "Bright Data",
        category: McpMarketCategory::WebSearch,
        url: "https://mcp.brightdata.com/mcp",
        source: McpMarketSource::Official,
        source_label: "mcp.brightdata.com",
        source_url: "https://github.com/brightdata/brightdata-mcp",
        icon: "icons/globe.svg",
        description_key: "mcp_market.desc.brightdata",
        needs_auth: true,
    },
    McpMarketEntry {
        id: "pubmed",
        display_name: "PubMed",
        category: McpMarketCategory::Academic,
        url: "https://pubmed.run.tools",
        source: McpMarketSource::Hosted,
        source_label: "Smithery",
        source_url: "https://smithery.ai/servers/pubmed",
        icon: "icons/file.svg",
        description_key: "mcp_market.desc.pubmed",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "arxiv",
        display_name: "arXiv",
        category: McpMarketCategory::Academic,
        url: "https://arxiv.run.tools",
        source: McpMarketSource::Hosted,
        source_label: "Smithery",
        source_url: "https://smithery.ai/servers/arxiv",
        icon: "icons/file.svg",
        description_key: "mcp_market.desc.arxiv",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "consensus",
        display_name: "Consensus",
        category: McpMarketCategory::Academic,
        url: "https://ai.consensus-analytics.com/mcp",
        source: McpMarketSource::Official,
        source_label: "ai.consensus-analytics.com",
        source_url: "https://ai.consensus-analytics.com/mcp",
        icon: "icons/file.svg",
        description_key: "mcp_market.desc.consensus",
        needs_auth: true,
    },
    McpMarketEntry {
        id: "paper-search",
        display_name: "Paper Search",
        category: McpMarketCategory::Academic,
        url: "https://server.smithery.ai/adamamer20/paper-search-mcp-openai/mcp",
        source: McpMarketSource::Hosted,
        source_label: "Smithery",
        source_url: "https://smithery.ai/servers/adamamer20/paper-search-mcp-openai",
        icon: "icons/file.svg",
        description_key: "mcp_market.desc.paper_search",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "semantic-scholar",
        display_name: "Semantic Scholar",
        category: McpMarketCategory::Academic,
        url: "https://server.smithery.ai/hamid-vakilzadeh/mcpsemanticscholar/mcp",
        source: McpMarketSource::Hosted,
        source_label: "Smithery",
        source_url: "https://smithery.ai/servers/hamid-vakilzadeh/mcpsemanticscholar",
        icon: "icons/file.svg",
        description_key: "mcp_market.desc.semantic_scholar",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "huggingface",
        display_name: "Hugging Face",
        category: McpMarketCategory::Academic,
        url: "https://huggingface.co/mcp",
        source: McpMarketSource::Official,
        source_label: "huggingface.co",
        source_url: "https://huggingface.co/mcp",
        icon: "icons/sparkle.svg",
        description_key: "mcp_market.desc.huggingface",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "wikipedia",
        display_name: "Wikipedia",
        category: McpMarketCategory::Academic,
        url: "https://wikipedia-mcp-server--cyanheads.run.tools",
        source: McpMarketSource::Hosted,
        source_label: "Smithery",
        source_url: "https://smithery.ai/servers/cyanheads/wikipedia-mcp-server",
        icon: "icons/globe.svg",
        description_key: "mcp_market.desc.wikipedia",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "context7",
        display_name: "Context7",
        category: McpMarketCategory::Code,
        url: "https://mcp.context7.com/mcp",
        source: McpMarketSource::Official,
        source_label: "mcp.context7.com",
        source_url: "https://github.com/upstash/context7",
        icon: "icons/package.svg",
        description_key: "mcp_market.desc.context7",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "github",
        display_name: "GitHub",
        category: McpMarketCategory::Code,
        url: "https://api.githubcopilot.com/mcp/",
        source: McpMarketSource::Official,
        source_label: "api.githubcopilot.com",
        source_url: "https://github.com/github/github-mcp-server",
        icon: "icons/github.svg",
        description_key: "mcp_market.desc.github",
        needs_auth: true,
    },
    McpMarketEntry {
        id: "deepwiki",
        display_name: "DeepWiki",
        category: McpMarketCategory::Code,
        url: "https://mcp.deepwiki.com/mcp",
        source: McpMarketSource::Official,
        source_label: "mcp.deepwiki.com",
        source_url: "https://mcp.deepwiki.com/mcp",
        icon: "icons/github.svg",
        description_key: "mcp_market.desc.deepwiki",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "vercel-grep",
        display_name: "Vercel Grep",
        category: McpMarketCategory::Code,
        url: "https://mcp.grep.app",
        source: McpMarketSource::Official,
        source_label: "mcp.grep.app",
        source_url: "https://github.com/vercel/grep",
        icon: "icons/search.svg",
        description_key: "mcp_market.desc.grep",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "microsoft-learn",
        display_name: "Microsoft Learn",
        category: McpMarketCategory::Code,
        url: "https://learn.microsoft.com/api/mcp",
        source: McpMarketSource::Official,
        source_label: "learn.microsoft.com",
        source_url: "https://github.com/microsoftdocs/mcp",
        icon: "icons/file.svg",
        description_key: "mcp_market.desc.microsoft_learn",
        needs_auth: false,
    },
    McpMarketEntry {
        id: "gread",
        display_name: "Gread",
        category: McpMarketCategory::Code,
        url: "https://api.gread.dev/mcp",
        source: McpMarketSource::Official,
        source_label: "api.gread.dev",
        source_url: "https://github.com/NitroRCr/gread",
        icon: "icons/github.svg",
        description_key: "mcp_market.desc.gread",
        needs_auth: false,
    },
];

/// The endpoints the marketplace shipped before it listed the vendors' own
/// servers. A server still on one of them moves to the vendor's endpoint in
/// place when its listing is added again, instead of leaving a duplicate
/// behind.
const LEGACY_URLS: &[(&str, &str)] = &[
    ("brave", "https://brave.run.tools"),
    ("exa", "https://exa.run.tools"),
    ("tavily", "https://tavily.run.tools"),
    (
        "keenable-web-search",
        "https://web-search-keenable.run.tools",
    ),
    (
        "parallel-search",
        "https://server.smithery.ai/parallel/search/mcp",
    ),
    (
        "linkup",
        "https://server.smithery.ai/LinkupPlatform/linkup-mcp-server/mcp",
    ),
    ("brightdata", "https://server.smithery.ai/brightdata/mcp"),
    ("consensus", "https://server.smithery.ai/consensus/mcp"),
    ("huggingface", "https://server.smithery.ai/huggingface/mcp"),
    ("github", "https://server.smithery.ai/github/mcp"),
    ("deepwiki", "https://server.smithery.ai/deepwiki/mcp"),
    (
        "microsoft-learn",
        "https://server.smithery.ai/microsoft/learn_mcp/mcp",
    ),
    ("gread", "https://server.smithery.ai/nitrofire-q/gread/mcp"),
];

fn activate_key(event: &KeyDownEvent) -> bool {
    !event.keystroke.modifiers.modified()
        && matches!(event.keystroke.key.as_str(), "enter" | "space")
}

fn market_accent(theme: &Theme) -> Hsla {
    theme.accent_text
}

fn catalog_entry(id: &str) -> Option<&'static McpMarketEntry> {
    CATALOG.iter().find(|entry| entry.id == id)
}

fn installed_server<'a>(servers: &'a [McpServer], entry: &McpMarketEntry) -> Option<&'a McpServer> {
    servers.iter().find(|server| server.url == entry.url)
}

/// The roster slot of a server that still points at the listing's old
/// endpoint.
fn legacy_server_index(servers: &[McpServer], entry: &McpMarketEntry) -> Option<usize> {
    let legacy = LEGACY_URLS
        .iter()
        .find(|(id, _)| *id == entry.id)
        .map(|(_, url)| *url)?;
    servers.iter().position(|server| server.url == legacy)
}

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNextEntry, Some(MARKET_SEARCH_CONTEXT)),
        KeyBinding::new("up", SelectPreviousEntry, Some(MARKET_SEARCH_CONTEXT)),
        KeyBinding::new("escape", ClearMcpMarketSearch, Some(MARKET_SEARCH_CONTEXT)),
    ]);
}

impl Fintwind {
    fn visible_market_entries(&self, cx: &App) -> Vec<&'static McpMarketEntry> {
        let query = self
            .mcp_market_search
            .read(cx)
            .content()
            .trim()
            .to_lowercase();
        let category = self.mcp_market_category;
        CATALOG
            .iter()
            .filter(|entry| {
                (category == McpMarketCategory::All || entry.category == category)
                    && (query.is_empty()
                        || entry.display_name.to_lowercase().contains(&query)
                        || entry.id.contains(query.as_str())
                        || crate::i18n::translate(entry.description_key)
                            .to_lowercase()
                            .contains(&query)
                        || entry.source_label.to_lowercase().contains(&query)
                        || entry.source_url.to_lowercase().contains(&query))
            })
            .collect()
    }

    fn effective_market_entry(&self, cx: &App) -> Option<&'static McpMarketEntry> {
        let visible = self.visible_market_entries(cx);
        self.mcp_market_selected
            .as_deref()
            .and_then(catalog_entry)
            .filter(|entry| visible.iter().any(|visible| visible.id == entry.id))
            .or_else(|| visible.first().copied())
    }

    fn select_market_entry(&mut self, id: String, cx: &mut Context<Self>) {
        self.mcp_market_selected = Some(id);
        self.mcp_detail_scroll.set_offset(gpui::Point::default());
        cx.notify();
    }

    fn step_market_selection(&mut self, key: &str, cx: &mut Context<Self>) {
        let visible = self.visible_market_entries(cx);
        if visible.is_empty() {
            return;
        }
        let current = self
            .mcp_market_selected
            .as_deref()
            .and_then(|selected| visible.iter().position(|entry| entry.id == selected));
        let Some(next) = next_picker_highlight(current, visible.len(), key) else {
            return;
        };
        self.select_market_entry(visible[next].id.to_owned(), cx);
    }

    fn set_market_category(&mut self, category: McpMarketCategory, cx: &mut Context<Self>) {
        self.mcp_market_category = category;
        cx.notify();
    }

    fn install_market_entry(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(entry) = catalog_entry(&id) else {
            return;
        };
        if installed_server(&self.mcp_servers, entry).is_some() {
            return;
        }
        // A server still on the listing's old endpoint moves to the vendor's
        // endpoint in place: its name, headers, and OAuth survive, and no
        // duplicate appears.
        if let Some(index) = legacy_server_index(&self.mcp_servers, entry) {
            self.mcp_servers[index].url = entry.url.to_owned();
            self.commit_mcp_servers(cx);
            self.show_success_toast(tr!("mcp_market.upgraded_toast", name = entry.display_name));
            return;
        }
        let taken: Vec<String> = self
            .mcp_servers
            .iter()
            .map(|server| server.name.clone())
            .collect();
        let name = unique_provider_slug(entry.display_name, &taken);
        self.mcp_servers.push(McpServer {
            name: name.clone(),
            kind: McpServerKind::Remote,
            command: Vec::new(),
            url: entry.url.to_owned(),
            environment: Vec::new(),
            headers: Vec::new(),
            oauth: Default::default(),
            enabled: true,
            raw: serde_json::Value::Null,
        });
        self.commit_mcp_servers(cx);
        self.show_success_toast(tr!("mcp_market.added_toast", name = entry.display_name));
    }

    fn remove_market_entry(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(entry) = catalog_entry(&id) else {
            return;
        };
        let Some(name) =
            installed_server(&self.mcp_servers, entry).map(|server| server.name.clone())
        else {
            return;
        };
        self.mcp_servers.retain(|server| server.name != name);
        if self.mcp_selected.as_deref() == Some(name.as_str()) {
            self.mcp_selected = None;
        }
        self.commit_mcp_servers(cx);
        self.show_success_toast(tr!("mcp_market.removed_toast", name = entry.display_name));
    }

    fn open_installed_market_entry(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(entry) = catalog_entry(&id) else {
            return;
        };
        let Some(name) =
            installed_server(&self.mcp_servers, entry).map(|server| server.name.clone())
        else {
            return;
        };
        self.mcp_selected = Some(name);
        self.open_settings_page(SettingsPage::McpServers, cx);
    }

    pub(super) fn render_mcp_market_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        div()
            .size_full()
            .min_h_0()
            .flex()
            .child(self.render_market_list_pane(&theme, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.render_market_detail(&theme, cx)),
            )
            .into_any_element()
    }

    fn render_market_list_pane(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let accent = theme.neutral_fill;
        let visible = self.visible_market_entries(cx);
        let selected = self.effective_market_entry(cx);
        let grouped = self.mcp_market_category == McpMarketCategory::All;
        let mut rows = div().px(px(8.0)).flex().flex_col();
        if visible.is_empty() {
            rows = rows.child(
                div()
                    .px(px(9.0))
                    .py(px(12.0))
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_tertiary)
                    .child(tr!("mcp_market.empty")),
            );
        } else if grouped {
            let mut first_section = true;
            for category in [
                McpMarketCategory::WebSearch,
                McpMarketCategory::Academic,
                McpMarketCategory::Code,
            ] {
                let matches: Vec<_> = visible
                    .iter()
                    .copied()
                    .filter(|entry| entry.category == category)
                    .collect();
                if matches.is_empty() {
                    continue;
                }
                rows = rows.child(section_label(
                    theme,
                    format!("{} {}", category.section_label(), matches.len()),
                    first_section,
                ));
                first_section = false;
                for entry in matches {
                    let is_selected = selected.is_some_and(|selected| selected.id == entry.id);
                    rows = rows.child(self.render_market_list_row(
                        entry,
                        is_selected,
                        theme,
                        accent,
                        cx,
                    ));
                }
            }
        } else {
            for entry in visible {
                let is_selected = selected.is_some_and(|selected| selected.id == entry.id);
                rows =
                    rows.child(self.render_market_list_row(entry, is_selected, theme, accent, cx));
            }
        }

        div()
            .key_context(MARKET_PANE_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectNextEntry, _, cx| {
                this.step_market_selection("down", cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousEntry, _, cx| {
                this.step_market_selection("up", cx);
            }))
            .on_action(cx.listener(|this, _: &ClearMcpMarketSearch, _, cx| {
                if this.mcp_market_search.read(cx).content().is_empty() {
                    cx.propagate();
                    return;
                }
                this.mcp_market_search
                    .update(cx, |input, cx| input.clear(cx));
            }))
            .w(px(MARKET_LIST_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .pt(px(14.0))
                    .px(px(16.0))
                    .flex_none()
                    .text_size(ui_px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("settings.mcp_market")),
            )
            .child(
                div()
                    .px(px(12.0))
                    .pt(px(10.0))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        TextField::new("mcp-market-search-field", self.mcp_market_search.clone())
                            .icon("icons/search.svg", 13.0),
                    )
                    .child(self.render_market_category_filter(theme, accent, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mt(px(8.0))
                    .relative()
                    .child(
                        div()
                            .id("mcp-market-list-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.mcp_list_scroll)
                            .pb(px(8.0))
                            .child(rows),
                    )
                    .child(scrollbar::vertical(
                        &self.mcp_list_scroll,
                        &self.mcp_list_scrollbar,
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(26.0))
                    .px(px(12.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(ui_px(9.5))
                    .text_color(theme.text_muted)
                    .child(self.market_footer_caption(cx)),
            )
    }

    fn market_footer_caption(&self, cx: &App) -> SharedString {
        let shown = self.visible_market_entries(cx).len();
        let total = CATALOG.len();
        SharedString::from(if shown == total {
            tr!("mcp_market.count", count = total)
        } else {
            tr!("mcp_market.filter_caption", shown = shown, total = total)
        })
    }

    fn render_market_category_filter(
        &self,
        theme: &Theme,
        accent: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        let current = self.mcp_market_category;
        let mut row = div().flex().flex_wrap().gap(px(4.0));
        for category in McpMarketCategory::FILTERS {
            let selected = current == category;
            row = row.child(
                div()
                    .id(category.id())
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(accent))
                    .h(px(24.0))
                    .px(px(8.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(ui_px(10.5))
                    .when(selected, |element| {
                        element
                            .text_color(theme.neutral_fill)
                            .bg(theme.neutral_fill.opacity(0.14))
                    })
                    .when(!selected, |element| {
                        element
                            .text_color(theme.text_tertiary)
                            .bg(theme.overlay)
                            .hover(|element| {
                                element.bg(theme.overlay_strong).text_color(theme.text)
                            })
                    })
                    .child(category.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_market_category(category, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if activate_key(event) {
                            this.set_market_category(category, cx);
                            cx.stop_propagation();
                        }
                    })),
            );
        }
        row
    }

    fn render_market_list_row(
        &self,
        entry: &McpMarketEntry,
        selected: bool,
        theme: &Theme,
        accent: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = entry.id.to_owned();
        let installed = installed_server(&self.mcp_servers, entry).is_some();
        div()
            .child(
                div()
                    .id(SharedString::from(format!("mcp-market-row-{}", entry.id)))
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(accent))
                    .w_full()
                    .px(px(9.0))
                    .py(px(7.0))
                    .mb(px(1.0))
                    .rounded(px(8.0))
                    .cursor_default()
                    .when(selected, |element| element.bg(accent.opacity(0.13)))
                    .when(!selected, |element| {
                        element
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.bg(theme.overlay_strong))
                    })
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .child(
                        div()
                            .w(px(28.0))
                            .h(px(28.0))
                            .flex_none()
                            .rounded(px(2.0))
                            .bg(theme.overlay)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(
                                entry.icon,
                                14.0,
                                if selected {
                                    accent
                                } else {
                                    theme.text_secondary
                                },
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(SharedString::from(entry.display_name)),
                            )
                            .child(
                                div()
                                    .mt(px(1.0))
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_px(10.5))
                                    .text_color(theme.text_tertiary)
                                    .child(entry.category.section_label()),
                            ),
                    )
                    .when(installed, |element| {
                        element.child(icon("icons/check.svg", 12.0, accent))
                    })
                    .on_click({
                        let id = id.clone();
                        cx.listener(move |this, _, _, cx| {
                            this.select_market_entry(id.clone(), cx);
                        })
                    })
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if activate_key(event) {
                            this.select_market_entry(id.clone(), cx);
                            cx.stop_propagation();
                        }
                    })),
            )
            .into_any_element()
    }

    fn render_market_detail(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.effective_market_entry(cx) else {
            return self.mcp_scrollable_detail(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(ui_px(16.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(tr!("mcp_market.empty")),
                    )
                    .child(
                        div()
                            .text_size(ui_px(12.5))
                            .text_color(theme.text_secondary)
                            .child(tr!("mcp_market.subtitle")),
                    ),
            );
        };
        let accent = market_accent(theme);
        let installed = installed_server(&self.mcp_servers, entry).is_some();
        let id = entry.id.to_owned();
        let source_url = entry.source_url;
        let source_button_label = match entry.source {
            McpMarketSource::Official => tr!("mcp_market.open_official"),
            McpMarketSource::Hosted => tr!("mcp_market.open_smithery"),
        };
        let actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .when(installed, |element| {
                let id = id.clone();
                let open_id = id.clone();
                element
                    .child(
                        small_action_button(
                            "mcp-market-open-servers",
                            "icons/wrench.svg",
                            tr!("mcp_market.open_servers"),
                            theme.text_secondary,
                            theme,
                        )
                        .on_click({
                            let open_id = open_id.clone();
                            cx.listener(move |this, _, _, cx| {
                                this.open_installed_market_entry(open_id.clone(), cx);
                            })
                        })
                        .on_key_down(cx.listener(
                            move |this, event: &KeyDownEvent, _, cx| {
                                if activate_key(event) {
                                    this.open_installed_market_entry(open_id.clone(), cx);
                                    cx.stop_propagation();
                                }
                            },
                        )),
                    )
                    .child(
                        outline_button(
                            "mcp-market-remove",
                            tr!("mcp_market.remove"),
                            Some("icons/trash.svg"),
                            theme,
                        )
                        .on_click({
                            let id = id.clone();
                            cx.listener(move |this, _, _, cx| {
                                this.remove_market_entry(id.clone(), cx);
                            })
                        })
                        .on_key_down(cx.listener(
                            move |this, event: &KeyDownEvent, _, cx| {
                                if activate_key(event) {
                                    this.remove_market_entry(id.clone(), cx);
                                    cx.stop_propagation();
                                }
                            },
                        )),
                    )
            })
            .when(!installed, |element| {
                let id = id.clone();
                element.child(
                    small_action_button(
                        "mcp-market-install",
                        "icons/plus.svg",
                        tr!("mcp_market.install"),
                        accent,
                        theme,
                    )
                    .on_click({
                        let id = id.clone();
                        cx.listener(move |this, _, _, cx| {
                            this.install_market_entry(id.clone(), cx);
                        })
                    })
                    .on_key_down(cx.listener(
                        move |this, event: &KeyDownEvent, _, cx| {
                            if activate_key(event) {
                                this.install_market_entry(id.clone(), cx);
                                cx.stop_propagation();
                            }
                        },
                    )),
                )
            })
            .child(
                outline_button(
                    "mcp-market-source",
                    source_button_label,
                    Some("icons/external-link.svg"),
                    theme,
                )
                .on_click({
                    let source_url = source_url.to_owned();
                    cx.listener(move |_, _, _, cx| {
                        cx.open_url(&source_url);
                    })
                })
                .on_key_down({
                    let source_url = source_url.to_owned();
                    cx.listener(move |_, event: &KeyDownEvent, _, cx| {
                        if activate_key(event) {
                            cx.open_url(&source_url);
                            cx.stop_propagation();
                        }
                    })
                }),
            );

        self.mcp_scrollable_detail(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(12.0))
                        .child(provider_tile(theme, entry.icon, true))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .text_size(ui_px(18.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.text)
                                        .child(SharedString::from(entry.display_name)),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .items_center()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .px(px(7.0))
                                                .py(px(2.0))
                                                .rounded_full()
                                                .text_size(ui_px(9.5))
                                                .when(installed, |element| {
                                                    element
                                                        .text_color(theme.accent_text)
                                                        .bg(theme.accent_fill.opacity(0.14))
                                                        .child(tr!("mcp_market.installed"))
                                                })
                                                .when(!installed, |element| {
                                                    element
                                                        .text_color(theme.text_tertiary)
                                                        .bg(theme.overlay)
                                                        .child(tr!("mcp_market.available"))
                                                }),
                                        )
                                        .child(
                                            div()
                                                .px(px(7.0))
                                                .py(px(2.0))
                                                .rounded_full()
                                                .text_size(ui_px(9.5))
                                                .text_color(theme.text_tertiary)
                                                .bg(theme.overlay)
                                                .child(entry.category.section_label()),
                                        ),
                                ),
                        ),
                )
                .child(
                    div()
                        .mt(px(14.0))
                        .text_size(ui_px(13.0))
                        .line_height(ui_px(20.0))
                        .text_color(theme.text_secondary)
                        .child(crate::i18n::translate(entry.description_key)),
                )
                .child(
                    div()
                        .mt(px(12.0))
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_size(ui_px(9.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text_tertiary)
                                .child(tr!("mcp_market.source").to_uppercase()),
                        )
                        .child(
                            div()
                                .font_family(crate::md::render::mono_family())
                                .text_size(ui_px(11.5))
                                .text_color(theme.text_secondary)
                                .child(SharedString::from(entry.source_label)),
                        )
                        .child(
                            div()
                                .font_family(crate::md::render::mono_family())
                                .text_size(ui_px(11.0))
                                .text_color(theme.text_tertiary)
                                .child(SharedString::from(entry.url)),
                        ),
                )
                .when(entry.needs_auth, |element| {
                    element.child(info_note(
                        theme,
                        "icons/lock.svg",
                        tr!("mcp_market.auth_note"),
                    ))
                })
                .child(info_note(theme, "icons/info.svg", tr!("mcp_market.note")))
                .child(
                    div()
                        .mt(px(18.0))
                        .pt(px(14.0))
                        .border_t_1()
                        .border_color(theme.border)
                        .child(actions),
                ),
        )
    }
}

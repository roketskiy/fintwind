//! Fintwind host bridge for [`fintwind_integrations_ui::McpMarketPage`].

use std::rc::Rc;

use fintwind_integrations_ui::McpMarketHost;
use fintwind_opencode_config::opencode_config::McpServer;
use gpui::WeakEntity;

pub use fintwind_integrations_ui::mcp_market::init;

use super::*;

struct FintwindMcpMarketHost(WeakEntity<Fintwind>);

pub(super) fn host(weak: WeakEntity<Fintwind>) -> Rc<dyn McpMarketHost> {
    Rc::new(FintwindMcpMarketHost(weak))
}

impl McpMarketHost for FintwindMcpMarketHost {
    fn mcp_servers(&self, cx: &App) -> Vec<McpServer> {
        self.0
            .read_with(cx, |this, _| this.mcp_servers.clone())
            .unwrap_or_default()
    }

    fn install_server(
        &self,
        upgrade_index: Option<usize>,
        server: McpServer,
        toast: String,
        cx: &mut App,
    ) {
        let _ = self.0.update(cx, |this, cx| {
            if let Some(index) = upgrade_index {
                if let Some(slot) = this.mcp_servers.get_mut(index) {
                    *slot = server;
                }
            } else {
                this.mcp_servers.push(server);
            }
            this.commit_mcp_servers(cx);
            this.show_success_toast(toast);
        });
    }

    fn remove_server(&self, name: &str, toast: String, cx: &mut App) {
        let name = name.to_owned();
        let _ = self.0.update(cx, |this, cx| {
            this.mcp_servers.retain(|server| server.name != name);
            if this.mcp_selected.as_deref() == Some(name.as_str()) {
                this.mcp_selected = None;
            }
            this.commit_mcp_servers(cx);
            this.show_success_toast(toast);
        });
    }

    fn open_server(&self, name: String, cx: &mut App) {
        let _ = self.0.update(cx, |this, cx| {
            this.mcp_selected = Some(name);
            this.open_settings_page(SettingsPage::McpServers, cx);
        });
    }
}

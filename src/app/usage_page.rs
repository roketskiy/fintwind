//! Fintwind host bridge for [`fintwind_usage_ui::UsagePage`].

use std::path::PathBuf;
use std::rc::Rc;

use fintwind_client::DaemonSupervisor;
use fintwind_protocol::provider_quota::ProviderQuota;
use fintwind_usage_ui::UsageHost;
use gpui::{FocusHandle, WeakEntity};

use super::*;

struct FintwindUsageHost(WeakEntity<Fintwind>);

pub(super) fn host(weak: WeakEntity<Fintwind>) -> Rc<dyn UsageHost> {
    Rc::new(FintwindUsageHost(weak))
}

impl UsageHost for FintwindUsageHost {
    fn is_active(&self, cx: &App) -> bool {
        self.0
            .read_with(cx, |this, _| this.mode == WorkspaceMode::Usage)
            .unwrap_or(false)
    }

    fn scan_target(&self, cx: &App) -> Option<(DaemonSupervisor, PathBuf, PathBuf)> {
        self.0
            .read_with(cx, |this, _| {
                let binary = this.native_binary_path()?;
                let project_id = this.state.selected_project?;
                let directory = this
                    .state
                    .projects
                    .iter()
                    .find(|project| project.id == project_id)
                    .map(|project| project.path.clone())?;
                Some((this.daemon.clone(), binary, directory))
            })
            .ok()
            .flatten()
    }

    fn subscriptions_generation(&self, cx: &App) -> u64 {
        self.0
            .read_with(cx, |this, _| this.subscriptions_generation())
            .unwrap_or(0)
    }

    fn subscription_cards(&self, cx: &App) -> Vec<(String, bool, Option<ProviderQuota>)> {
        self.0
            .read_with(cx, |this, _| this.subscription_cards())
            .unwrap_or_default()
    }

    fn subscriptions_loading(&self, cx: &App) -> bool {
        self.0
            .read_with(cx, |this, _| this.subscriptions_loading())
            .unwrap_or(false)
    }

    fn subscriptions_connection_error(&self, cx: &App) -> bool {
        self.0
            .read_with(cx, |this, _| this.subscriptions_connection_error())
            .unwrap_or(false)
    }

    fn subscriptions_connections_loaded(&self, cx: &App) -> bool {
        self.0
            .read_with(cx, |this, _| this.subscriptions_connections_loaded())
            .unwrap_or(false)
    }

    fn subscription_display_name(&self, provider: &str, cx: &App) -> String {
        self.0
            .read_with(cx, |this, _| this.subscription_display_name(provider))
            .unwrap_or_else(|_| provider.to_owned())
    }

    fn load_subscriptions(&self, force: bool, cx: &mut App) {
        let _ = self.0.update(cx, |this, cx| {
            this.load_subscriptions(force, cx);
        });
    }

    fn refresh_subscription(&self, provider: &str, cx: &mut App) {
        let _ = self.0.update(cx, |this, cx| {
            this.refresh_subscription(provider, cx);
        });
    }

    fn control_focus(&self, key: &str, cx: &mut App) -> FocusHandle {
        self.0
            .update(cx, |this, cx| this.transcript_control_focus(key, cx))
            .unwrap_or_else(|_| cx.focus_handle())
    }
}

impl Fintwind {
    pub(super) fn notify_usage_page(&self, cx: &mut App) {
        let page = self.usage_page.clone();
        cx.defer(move |cx| {
            page.update(cx, |_, cx| cx.notify());
        });
    }
}

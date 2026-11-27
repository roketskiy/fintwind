//! Subscription and balance board on the usage page. Rendering reads stored
//! snapshots only; every query runs on the background executor with its own
//! generation guard, so a slow vendor never blocks the others.

use fintwind_protocol::provider_quota::{
    ProviderQuota, QuotaStatus, is_local_quota_source, supports_provider_quota,
};

use super::*;

pub(super) use fintwind_providers_ui::{SUBSCRIPTION_ORDER, SubscriptionBoard};

impl Fintwind {
    /// Drop in-flight subscription reads; the next load repopulates.
    pub(super) fn invalidate_subscriptions(&mut self) {
        self.provider_quota.generation = self.provider_quota.generation.wrapping_add(1);
        self.provider_quota.entries.clear();
        self.provider_quota.connections_loaded = false;
        self.provider_quota.refresh_spins.get_mut().clear();
        self.provider_quota.workspace = None;
    }

    pub(super) fn subscriptions_generation(&self) -> u64 {
        self.provider_quota.generation
    }

    fn sync_subscription_workspace(&mut self) {
        let workspace = self.provider_workspace();
        if self.provider_quota.workspace != workspace {
            self.invalidate_subscriptions();
            self.provider_quota.workspace = workspace;
        }
    }

    /// OpenCode's connected integrations own visibility, including for local
    /// Claude/Codex readers. Expired connections remain visible as such.
    fn subscription_visible(&self, provider: &str) -> bool {
        supports_provider_quota(provider) && self.provider_is_authorized(provider)
    }

    /// Resolve connections before reading quotas. A request made while that
    /// fetch is in flight joins it without losing a manual refresh.
    pub(super) fn load_subscriptions(&mut self, force: bool, cx: &mut Context<Self>) {
        self.sync_subscription_workspace();
        self.provider_quota.connection_error = false;
        self.provider_quota.refresh = Some(force || self.provider_quota.refresh.unwrap_or(false));
        self.ensure_builtin_providers(true, cx);
    }

    /// Only a successful connection snapshot can repopulate the board.
    pub(super) fn subscriptions_connections_updated(&mut self, cx: &mut Context<Self>) {
        self.provider_quota.connection_error = false;
        let force = self.provider_quota.refresh.take().unwrap_or(false);
        self.sync_subscription_workspace();
        self.provider_quota.connections_loaded = true;
        self.provider_quota.generation = self.provider_quota.generation.wrapping_add(1);
        let visible: Vec<_> = SUBSCRIPTION_ORDER
            .iter()
            .copied()
            .filter(|id| self.subscription_visible(id))
            .collect();
        self.provider_quota
            .entries
            .retain(|id, _| visible.contains(&id.as_str()));
        for entry in self.provider_quota.entries.values_mut() {
            entry.loading = false;
        }
        if self.mode != WorkspaceMode::Usage {
            return;
        }
        for provider in visible {
            self.load_one_subscription(provider, force, cx);
        }
        self.notify_usage_page(cx);
        cx.notify();
    }

    pub(super) fn subscriptions_connections_failed(&mut self) {
        self.provider_quota.refresh = None;
        self.provider_quota.connection_error = true;
    }

    pub(super) fn subscriptions_loading(&self) -> bool {
        self.provider_quota.refresh.is_some()
            || self
                .provider_quota
                .entries
                .values()
                .any(|entry| entry.loading)
    }

    pub(super) fn subscriptions_connection_error(&self) -> bool {
        self.provider_quota.connection_error
    }

    pub(super) fn subscriptions_connections_loaded(&self) -> bool {
        self.provider_quota.connections_loaded
    }

    pub(super) fn load_one_subscription(
        &mut self,
        provider: &str,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        self.sync_subscription_workspace();
        if !self.subscription_visible(provider) {
            self.provider_quota.entries.remove(provider);
            return;
        }
        let needs_auth = self.provider_needs_auth(provider);
        let provider = provider.to_owned();
        let board_generation = self.provider_quota.generation;
        let entry = self
            .provider_quota
            .entries
            .entry(provider.clone())
            .or_default();
        entry.generation = entry.generation.wrapping_add(1);
        if needs_auth {
            entry.loading = false;
            entry.reading = Some(ProviderQuota::empty(QuotaStatus::NeedsAuth));
            cx.notify();
            return;
        }
        let entry_generation = entry.generation;
        entry.loading = true;
        // Local agents read the desktop machine's own sign-in in this
        // process, never through the daemon: a remote daemon must not stand
        // in for the desktop. Server providers resolve the workspace's
        // OpenCode server through the daemon.
        let workspace = self.provider_workspace();
        let requested_workspace = workspace.clone();
        let daemon = self.daemon.clone();
        let local_cache = self.local_quota_cache.clone();
        cx.notify();
        let provider_for_fetch = provider.clone();
        cx.spawn(async move |this, cx| {
            let reading = cx
                .background_executor()
                .spawn(async move {
                    if is_local_quota_source(&provider_for_fetch) {
                        local_cache.fetch(&provider_for_fetch, force)
                    } else {
                        match workspace {
                            Some((binary, directory)) => StateStore::remote(daemon)
                                .fetch_provider_quota(binary, directory, provider_for_fetch, force)
                                .unwrap_or_else(|_| ProviderQuota::empty(QuotaStatus::Unavailable)),
                            None => ProviderQuota::empty(QuotaStatus::Unavailable),
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.provider_quota.generation != board_generation {
                    return;
                }
                if this.provider_workspace() != requested_workspace {
                    this.invalidate_subscriptions();
                    if this.mode == WorkspaceMode::Usage {
                        this.load_subscriptions(false, cx);
                    }
                    cx.notify();
                    return;
                }
                if !this.subscription_visible(&provider) {
                    this.provider_quota.entries.remove(&provider);
                    cx.notify();
                    return;
                }
                let Some(entry) = this.provider_quota.entries.get_mut(&provider) else {
                    return;
                };
                if entry.generation != entry_generation {
                    return;
                }
                entry.loading = false;
                // A connected service must not silently disappear just
                // because its quota credentials or endpoint are unavailable.
                let mut reading = reading;
                if matches!(
                    reading.status,
                    QuotaStatus::Unavailable | QuotaStatus::RateLimited
                ) && reading.windows.is_empty()
                    && reading.balances.is_empty()
                    && let Some(mut cached) = entry.reading.take()
                    && (!cached.windows.is_empty() || !cached.balances.is_empty())
                {
                    cached.status = reading.status;
                    cached.retry_at = reading.retry_at;
                    reading = cached;
                }
                entry.reading = Some(reading);
                this.notify_usage_page(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Connected services in stable display order, with their stored snapshots.
    pub(super) fn subscription_cards(&self) -> Vec<(String, bool, Option<ProviderQuota>)> {
        SUBSCRIPTION_ORDER
            .iter()
            .filter(|id| self.subscription_visible(id))
            .filter_map(|id| {
                self.provider_quota
                    .entries
                    .get(*id)
                    .map(|entry| ((*id).to_owned(), entry.loading, entry.reading.clone()))
            })
            .collect()
    }

    pub(super) fn subscription_display_name(&self, provider: &str) -> String {
        if let Some(service) = self
            .provider_quota
            .entries
            .get(provider)
            .and_then(|entry| entry.reading.as_ref())
            .and_then(|reading| reading.service.clone())
            .filter(|service| !service.is_empty())
        {
            return service;
        }
        self.builtin_provider(provider)
            .and_then(|record| {
                if record.name.is_empty() {
                    None
                } else {
                    Some(record.name.clone())
                }
            })
            .unwrap_or_else(|| provider.to_owned())
    }

    pub(super) fn refresh_subscription(&mut self, provider: &str, cx: &mut Context<Self>) {
        if self
            .provider_quota
            .entries
            .get(provider)
            .is_some_and(|entry| entry.loading)
        {
            return;
        }
        self.provider_quota
            .refresh_spins
            .get_mut()
            .insert(provider.to_owned(), motion::ActionSpin::start());
        self.load_one_subscription(provider, true, cx);
    }
}

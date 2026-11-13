//! Daemon adapter for the shared machine-local subscription reader.

pub use fintwind_client::local_quota::{CommandOutput, LocalBackend, SystemBackend};

use fintwind_protocol::provider_quota::ProviderQuota;

use crate::provider_quota::QuotaService;

impl QuotaService {
    pub fn fetch_local_with(
        &self,
        backend: &impl LocalBackend,
        provider: &str,
        force: bool,
    ) -> ProviderQuota {
        self.local.fetch_with(backend, provider, force)
    }

    pub fn fetch_local(&self, provider: &str, force: bool) -> ProviderQuota {
        self.local.fetch(provider, force)
    }
}

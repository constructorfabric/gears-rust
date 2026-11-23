//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-intake:p1
//! The discovery plugin client of a target (P-D-268): the scoped client an adapter registered in
//! the `ClientHub` under its plugin instance id, read on every discovery. No adapter is built in
//! this version, so production resolves every target to closed, and discovery refuses 501
//! `QUOTA_DISCOVERY_CLOSED` before any read or write.
use std::sync::Arc;

use bss_products_sdk::ProductsQuotaDiscoveryPluginClientV1;
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;

use crate::domain::quota::ports::{QuotaDiscoveryResolver, ResolvedDiscovery};

/// The log target of every discovery resolution.
const LOG_TARGET: &str = "bss_products.quota.discovery";

/// Resolves a plugin instance to its scoped client in the hub, as the ledger resolves its rate
/// provider.
pub struct HubQuotaDiscovery {
    hub: Arc<ClientHub>,
}

impl HubQuotaDiscovery {
    /// A resolver over whatever scoped clients the hub holds at each discovery.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }
}

impl QuotaDiscoveryResolver for HubQuotaDiscovery {
    fn resolve(&self, plugin_instance_id: &str) -> ResolvedDiscovery {
        let scope = ClientScope::gts_id(plugin_instance_id);
        let client = self
            .hub
            .try_get_scoped::<dyn ProductsQuotaDiscoveryPluginClientV1>(&scope);
        if let Some(client) = client {
            return ResolvedDiscovery::Client(client);
        }
        tracing::info!(
            target: LOG_TARGET,
            plugin_instance_id,
            "bss-products: no quota discovery client is registered; discovery is closed"
        );
        ResolvedDiscovery::Closed
    }
}

/// Every plugin instance is closed: production without an adapter, and the tests of its refusal.
pub struct ClosedQuotaDiscovery;

impl QuotaDiscoveryResolver for ClosedQuotaDiscovery {
    fn resolve(&self, _plugin_instance_id: &str) -> ResolvedDiscovery {
        ResolvedDiscovery::Closed
    }
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod discovery_tests;

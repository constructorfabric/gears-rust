// Created: 2026-06-15 by Constructor Tech
//! The provider registry the config-driven wiring dispatches on.
//!
//! The [`ClusterCacheProvider`] trait itself lives in the SDK (so plugins
//! implement it depending on the SDK only). This registry is the wiring-side
//! lookup: a host assembles it from the provider impls in the plugin crates it
//! links, and [`ClusterWiring::from_config`](crate::ClusterWiring::from_config)
//! resolves each profile's `provider` string against it.

use std::collections::HashMap;
use std::sync::Arc;

use cluster_sdk::ClusterCacheProvider;

/// A name → [`ClusterCacheProvider`] lookup, assembled once at startup and passed
/// to [`ClusterWiring::from_config`](crate::ClusterWiring::from_config).
#[derive(Default)]
pub struct ProviderRegistry {
    cache_providers: HashMap<&'static str, Arc<dyn ClusterCacheProvider>>,
}

impl ProviderRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a cache provider, keyed by its
    /// [`provider`](ClusterCacheProvider::provider) name. A later registration for
    /// the same name replaces the earlier one.
    #[must_use]
    pub fn with_cache_provider(mut self, provider: Arc<dyn ClusterCacheProvider>) -> Self {
        self.cache_providers.insert(provider.provider(), provider);
        self
    }

    /// Looks up the cache provider for `name`, if registered.
    pub(crate) fn cache_provider(&self, name: &str) -> Option<&Arc<dyn ClusterCacheProvider>> {
        self.cache_providers.get(name)
    }
}

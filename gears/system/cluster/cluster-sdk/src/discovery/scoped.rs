// Created: 2026-06-10 by Constructor Tech
//! The per-primitive scoping wrapper for service discovery (DESIGN §3.8).
//!
//! Scoping applies to the service **`name` only**. Metadata is a per-instance
//! attribute namespace, not a coordination namespace, so
//! [`ServiceRegistration::metadata`], [`DiscoveryFilter`] metadata predicates,
//! and [`ServiceInstance::metadata`] pass through verbatim. There is no
//! read-path strip: a [`ServiceInstance`] carries no service name.

use std::sync::Arc;

use async_trait::async_trait;

use crate::discovery::backend::ServiceDiscoveryBackend;
use crate::discovery::handle::ServiceHandle;
use crate::discovery::types::{
    DiscoveryFilter, ServiceDiscoveryFeatures, ServiceInstance, ServiceRegistration,
};
use crate::discovery::watch::ServiceWatch;
use crate::error::ClusterError;
use crate::scope;

/// A delegating [`ServiceDiscoveryBackend`] that prepends a validated scope
/// prefix to the service `name` on the write path (registration name, discover
/// name, watch name) and leaves all metadata unchanged. Scoping composes by
/// stacking wrappers.
pub struct ScopedServiceDiscoveryBackend {
    inner: Arc<dyn ServiceDiscoveryBackend>,
    prefix: String,
}

impl ScopedServiceDiscoveryBackend {
    /// Wraps `inner` with the effective `prefix` (already validated and
    /// separator-terminated by [`scope::validated_prefix`]).
    pub fn new(inner: Arc<dyn ServiceDiscoveryBackend>, prefix: String) -> Self {
        Self { inner, prefix }
    }
}

#[async_trait]
impl ServiceDiscoveryBackend for ScopedServiceDiscoveryBackend {
    fn features(&self) -> ServiceDiscoveryFeatures {
        self.inner.features()
    }

    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }

    async fn register(&self, mut reg: ServiceRegistration) -> Result<ServiceHandle, ClusterError> {
        // Scope the service name only; metadata keys and values are untouched.
        reg.name = scope::apply(&self.prefix, &reg.name);
        self.inner.register(reg).await
    }

    async fn discover(
        &self,
        name: &str,
        filter: DiscoveryFilter,
    ) -> Result<Vec<ServiceInstance>, ClusterError> {
        // The filter (including its metadata predicates) passes through unchanged.
        self.inner
            .discover(&scope::apply(&self.prefix, name), filter)
            .await
    }

    async fn watch(&self, name: &str) -> Result<ServiceWatch, ClusterError> {
        self.inner.watch(&scope::apply(&self.prefix, name)).await
    }
}

#[cfg(test)]
#[path = "scoped_tests.rs"]
mod scoped_tests;

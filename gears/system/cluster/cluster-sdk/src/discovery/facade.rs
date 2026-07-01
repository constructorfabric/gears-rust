// Created: 2026-06-04 by Constructor Tech
//! The public `ServiceDiscoveryV1` facade — a thin, cloneable handle delegating
//! to the resolved `Arc<dyn ServiceDiscoveryBackend>`.

use std::sync::Arc;

use toolkit::client_hub::ClientHub;

use crate::discovery::backend::ServiceDiscoveryBackend;
use crate::discovery::handle::ServiceHandle;
use crate::discovery::resolver::ServiceDiscoveryResolverBuilder;
use crate::discovery::types::{
    DiscoveryFilter, ServiceDiscoveryFeatures, ServiceInstance, ServiceRegistration,
};
use crate::discovery::watch::ServiceWatch;
use crate::error::ClusterError;
use crate::restart::ResubscribeFuture;

/// The public service-discovery facade. Construct via
/// [`ServiceDiscoveryV1::resolver`]; cloning is cheap (an `Arc` bump).
///
/// Provides instance registration with metadata, filtered discovery, and a
/// reactive topology watch. A returned [`ServiceHandle`] updates metadata, flips
/// serving intent, and deregisters explicitly.
///
/// Use [`scoped`](Self::scoped) to carve a composable sub-namespace. Per
/// DESIGN §3.8, scoping applies to the service **`name` only**:
/// `ServiceRegistration::metadata`, `DiscoveryFilter` metadata predicates, and
/// `ServiceInstance::metadata` keys and values pass through unchanged, because
/// metadata is a per-instance attribute namespace, not a coordination namespace.
#[derive(Clone)]
pub struct ServiceDiscoveryV1 {
    inner: Arc<dyn ServiceDiscoveryBackend>,
}

impl ServiceDiscoveryV1 {
    /// Wraps a resolved backend. Crate-internal: consumers obtain a facade
    /// through the resolver.
    pub(crate) fn from_backend(inner: Arc<dyn ServiceDiscoveryBackend>) -> Self {
        Self { inner }
    }

    /// Static entry point: returns a fluent resolver bound to `hub`.
    pub fn resolver(hub: &ClientHub) -> ServiceDiscoveryResolverBuilder<'_> {
        ServiceDiscoveryResolverBuilder::new(hub)
    }

    /// Returns a sub-namespaced view: the service `name` (on register/discover/
    /// watch) is auto-prefixed with `prefix + "/"` (DESIGN §3.8). Metadata keys
    /// and values are never scoped. Scoping composes.
    ///
    /// # Errors
    /// Returns [`ClusterError::InvalidName`] if `prefix` violates the scope-prefix
    /// rule (`[a-zA-Z0-9_/-]+`).
    pub fn scoped(&self, prefix: &str) -> Result<Self, ClusterError> {
        let prefix = crate::scope::validated_prefix(prefix)?;
        Ok(Self::from_backend(Arc::new(
            crate::discovery::ScopedServiceDiscoveryBackend::new(Arc::clone(&self.inner), prefix),
        )))
    }

    /// The bound backend's native capability flags.
    #[must_use]
    pub fn features(&self) -> ServiceDiscoveryFeatures {
        self.inner.features()
    }

    /// Registers an instance, returning its [`ServiceHandle`]. The backend
    /// assigns an `instance_id` when `reg.instance_id` is `None`; the instance
    /// defaults to enabled.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend.
    pub async fn register(&self, reg: ServiceRegistration) -> Result<ServiceHandle, ClusterError> {
        self.inner.register(reg).await
    }

    /// Returns the instances of `name` matching `filter` (serving state AND every
    /// metadata predicate). The result order is **unspecified** — consumers
    /// needing deterministic selection sort client-side, typically by
    /// `instance_id`.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend. An empty match is
    /// `Ok(vec![])`, not an error.
    pub async fn discover(
        &self,
        name: &str,
        filter: DiscoveryFilter,
    ) -> Result<Vec<ServiceInstance>, ClusterError> {
        self.inner.discover(name, filter).await
    }

    /// Subscribes to the unfiltered topology watch for `name`. Consumers apply
    /// their own [`DiscoveryFilter`] client-side to each change event.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend.
    pub async fn watch(&self, name: &str) -> Result<ServiceWatch, ClusterError> {
        let mut watch = self.inner.watch(name).await?;
        install_service_watch_seam(Arc::clone(&self.inner), name.to_owned(), &mut watch);
        Ok(watch)
    }
}

/// Installs a self-reinstalling resubscribe seam that re-runs `watch(name)` on
/// the bound backend. Each reconnected watch is re-seamed, so
/// [`ServiceWatch::auto_restart`] reconnects *repeatedly*, not just once.
/// Capturing the backend (whose `async_trait` methods return a concretely-`Send`
/// boxed future) rather than the facade avoids a `Send` inference cycle.
fn install_service_watch_seam(
    backend: Arc<dyn ServiceDiscoveryBackend>,
    name: String,
    watch: &mut ServiceWatch,
) {
    watch.set_resubscribe(move || -> ResubscribeFuture<ServiceWatch> {
        let backend = Arc::clone(&backend);
        let name = name.clone();
        Box::pin(async move {
            let mut fresh = backend.watch(&name).await?;
            install_service_watch_seam(Arc::clone(&backend), name, &mut fresh);
            Ok(fresh)
        })
    });
}

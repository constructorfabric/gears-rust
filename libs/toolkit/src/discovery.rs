//! Consumer-side discovery wiring for eventual readiness.
//!
//! Bridges the service [`DirectoryClient`](crate::DirectoryClient) into the
//! contract layer's [`EndpointResolver`], and carries the
//! [`ConsumerRegistration`]s emitted by `#[toolkit::consumes]` that the
//! runtime's proxy-wiring phase replays at startup.
//!
//! Gated behind the `contract-directory-rest-client` feature so the HTTP/
//! discovery plumbing is only compiled into builds that actually consume a
//! contract over a discovered REST transport.

use std::sync::Arc;

use async_trait::async_trait;

use cf_system_sdks::directory::DirectoryNotFound;

use crate::DirectoryClient;
use crate::client_hub::ClientHub;

/// Re-export so the `#[toolkit::consumes]`-generated wire fn can name the
/// resolver type as `toolkit::discovery::EndpointResolver` without the consuming
/// gear crate needing a direct `toolkit-contract` dependency.
pub use toolkit_contract::runtime::resolving::{EndpointResolver, ResolveError};

/// Adapts the service [`DirectoryClient`] into the contract-layer
/// [`EndpointResolver`] consumed by `DirectoryResolvingClient`.
///
/// Keeps `toolkit-contract` free of a dependency on the directory SDK: the
/// adapter lives here in `toolkit`, which already owns `DirectoryClient`.
///
/// Successful resolutions are memoized for a short TTL ([`Self::TTL`]) so a
/// per-call resolving client does not incur a directory round-trip on every
/// business call (the `OoP` directory is a network hop). Only `Ok(Some(uri))` is
/// cached; `Ok(None)` / `Err` always re-resolve. The TTL is deliberately short
/// so endpoint churn self-heals within ~TTL.
pub struct DirectoryEndpointResolver {
    client: Arc<dyn DirectoryClient>,
    cache: parking_lot::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>,
}

impl DirectoryEndpointResolver {
    /// Memoization window for successful resolutions.
    const TTL: std::time::Duration = std::time::Duration::from_millis(1500);

    /// Wrap a [`DirectoryClient`] with a short-TTL resolution cache.
    #[must_use]
    pub fn new(client: Arc<dyn DirectoryClient>) -> Self {
        Self {
            client,
            cache: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn cached(&self, gear: &str) -> Option<String> {
        self.cache
            .lock()
            .get(gear)
            .filter(|(_, at)| at.elapsed() < Self::TTL)
            .map(|(uri, _)| uri.clone())
    }

    fn store(&self, gear: &str, uri: &str) {
        self.cache
            .lock()
            .insert(gear.to_owned(), (uri.to_owned(), std::time::Instant::now()));
    }
}

#[async_trait]
impl EndpointResolver for DirectoryEndpointResolver {
    async fn resolve_endpoint(&self, gear: &str) -> Result<Option<String>, ResolveError> {
        // Fresh cache hit avoids the directory round-trip.
        if let Some(uri) = self.cached(gear) {
            return Ok(Some(uri));
        }
        // Preserve the trait's `Ok(None)` (no live instance / not ready) vs
        // `Err` (directory backend unreachable) distinction so a real directory
        // outage surfaces at `warn` rather than being silently treated as
        // "provider not ready". The directory returns the typed
        // `DirectoryNotFound` sentinel for the not-found case; anything else is
        // a genuine backend/transport failure (relevant for the OoP gRPC
        // directory client).
        match self.client.resolve_rest_service(gear).await {
            Ok(ep) => {
                self.store(gear, &ep.uri);
                Ok(Some(ep.uri))
            }
            Err(e) if e.downcast_ref::<DirectoryNotFound>().is_some() => Ok(None),
            Err(e) => Err(ResolveError::new(gear, e)),
        }
    }
}

/// An [`EndpointResolver`] that never resolves any gear (`Ok(None)` for every
/// lookup). Used by the proxy-wiring phase when no [`DirectoryClient`] is present
/// in the [`ClientHub`]: co-located (local) consumers still short-circuit and
/// wire successfully, while remote consumers register as *unresolved* readiness
/// gates (so `/readyz` correctly stays 503) rather than being silently skipped.
pub struct NullEndpointResolver;

#[async_trait]
impl EndpointResolver for NullEndpointResolver {
    async fn resolve_endpoint(&self, _gear: &str) -> Result<Option<String>, ResolveError> {
        Ok(None)
    }
}

/// An [`EndpointResolver`] that always resolves to a single, fixed endpoint —
/// the ADR-0004 static-endpoint override escape hatch for local development and
/// integration testing. It bypasses the service directory: a consumer configured
/// with `gears.<owner>.config.consumer_wiring.<dep> = "http://host:port"` wires a
/// resolving client whose endpoint is this constant. Not for production use.
pub struct StaticEndpointResolver {
    endpoint: String,
}

impl StaticEndpointResolver {
    /// Wrap a fixed base endpoint URI.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl EndpointResolver for StaticEndpointResolver {
    async fn resolve_endpoint(&self, _gear: &str) -> Result<Option<String>, ResolveError> {
        Ok(Some(self.endpoint.clone()))
    }
}

/// Outcome of a [`ConsumerRegistration::wire`] call: whether the dependency was
/// satisfied by a co-located compile-time impl (no directory dependency) or by
/// a directory-resolving REST client that still needs endpoint resolution.
///
/// The runtime uses this to gate readiness: `Local` deps are readiness-resolved
/// immediately (no `/readyz` gating, no probe loop), while `Remote` deps gate
/// readiness until the directory resolves them (ADR-0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireOutcome {
    /// A compile-time (in-process) impl was already registered — it won the
    /// `hub.try_get` short-circuit; no remote binding was created.
    Local,
    /// A directory-resolving REST client was registered; its endpoint is
    /// resolved lazily/at readiness-probe time.
    Remote,
}

/// A consumer→provider contract wiring, registered by `#[toolkit::consumes]`
/// via `inventory::submit!` and replayed by the runtime's proxy-wiring phase.
///
/// `wire` is a non-capturing `fn` (the provider gear name is inlined into the
/// generated body). It must:
///   1. short-circuit returning [`WireOutcome::Local`] if a compile-time (local)
///      impl is already registered (`hub.try_get::<dyn Trait>().is_some()`), so
///      in-process providers win;
///   2. otherwise register a directory-resolving REST client under the contract
///      trait (using the supplied [`EndpointResolver`]) and return
///      [`WireOutcome::Remote`].
pub struct ConsumerRegistration {
    /// Gear that declares the dependency (diagnostics).
    pub owner_gear: &'static str,
    /// Provider gear name the contract resolves against (diagnostics).
    pub dep_gear: &'static str,
    /// Wiring action: register the client into the hub; reports whether the
    /// dependency was satisfied locally or bound to a remote client.
    pub wire: fn(&ClientHub, Arc<dyn EndpointResolver>) -> anyhow::Result<WireOutcome>,
}

inventory::collect!(ConsumerRegistration);

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use cf_system_sdks::directory::{RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts `resolve_rest_service` calls; all other methods are unused here.
    struct CountingDirectory {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl DirectoryClient for CountingDirectory {
        async fn resolve_grpc_service(&self, _: &str) -> anyhow::Result<ServiceEndpoint> {
            unimplemented!()
        }
        async fn resolve_rest_service(&self, gear: &str) -> anyhow::Result<ServiceEndpoint> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ServiceEndpoint::new(format!("http://{gear}.local")))
        }
        async fn get_openapi_spec(&self, _: &str) -> anyhow::Result<String> {
            unimplemented!()
        }
        async fn list_instances(&self, _: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
            unimplemented!()
        }
        async fn register_instance(&self, _: RegisterInstanceInfo) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn deregister_instance(&self, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn send_heartbeat(&self, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn memoizes_successful_resolution_within_ttl() {
        let dir = Arc::new(CountingDirectory {
            calls: AtomicUsize::new(0),
        });
        let resolver = DirectoryEndpointResolver::new(dir.clone());

        for _ in 0..3 {
            let ep = resolver.resolve_endpoint("billing").await.unwrap();
            assert_eq!(ep.as_deref(), Some("http://billing.local"));
        }

        assert_eq!(
            dir.calls.load(Ordering::SeqCst),
            1,
            "successful resolution must be memoized within the TTL (one directory lookup)"
        );
    }

    #[tokio::test]
    async fn static_resolver_always_returns_fixed_endpoint() {
        let resolver = StaticEndpointResolver::new("http://localhost:8081");
        assert_eq!(
            resolver
                .resolve_endpoint("anything")
                .await
                .unwrap()
                .as_deref(),
            Some("http://localhost:8081")
        );
        assert_eq!(
            resolver.resolve_endpoint("other").await.unwrap().as_deref(),
            Some("http://localhost:8081")
        );
    }

    #[tokio::test]
    async fn null_resolver_never_resolves() {
        assert_eq!(
            NullEndpointResolver
                .resolve_endpoint("billing")
                .await
                .unwrap(),
            None
        );
    }
}

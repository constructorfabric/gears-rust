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
pub struct DirectoryEndpointResolver(pub Arc<dyn DirectoryClient>);

#[async_trait]
impl EndpointResolver for DirectoryEndpointResolver {
    async fn resolve_endpoint(&self, gear: &str) -> Result<Option<String>, ResolveError> {
        // Preserve the trait's `Ok(None)` (no live instance / not ready) vs
        // `Err` (directory backend unreachable) distinction so a real directory
        // outage surfaces at `warn` rather than being silently treated as
        // "provider not ready". The directory returns the typed
        // `DirectoryNotFound` sentinel for the not-found case; anything else is
        // a genuine backend/transport failure (relevant for the OoP gRPC
        // directory client).
        match self.0.resolve_rest_service(gear).await {
            Ok(ep) => Ok(Some(ep.uri)),
            Err(e) if e.downcast_ref::<DirectoryNotFound>().is_some() => Ok(None),
            Err(e) => Err(ResolveError::new(gear, e)),
        }
    }
}

/// A consumer→provider contract wiring, registered by `#[toolkit::consumes]`
/// via `inventory::submit!` and replayed by the runtime's proxy-wiring phase.
///
/// `wire` is a non-capturing `fn` (the provider gear name is inlined into the
/// generated body). It must:
///   1. short-circuit if a compile-time (local) impl is already registered
///      (`hub.try_get::<dyn Trait>().is_some()`), so in-process providers win;
///   2. otherwise register a directory-resolving REST client under the
///      contract trait, using the supplied [`EndpointResolver`].
pub struct ConsumerRegistration {
    /// Gear that declares the dependency (diagnostics).
    pub owner_gear: &'static str,
    /// Provider gear name the contract resolves against (diagnostics).
    pub dep_gear: &'static str,
    /// Wiring action: register the client into the hub.
    pub wire: fn(&ClientHub, Arc<dyn EndpointResolver>) -> anyhow::Result<()>,
}

inventory::collect!(ConsumerRegistration);

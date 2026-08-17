//! A wired gear, in one call, for the service tests.
//!
//! The profiles come from the **real wiring** rather than a fabricated
//! [`BoundProfile`](crate::BoundProfile), for the same reason `registry_tests`
//! does it: the bound set `ClusterWiring::from_config` returns is the registry's
//! only data source, so publishing anything else would test a shape nothing
//! produces. The standalone plugin backs it, which keeps these hermetic — Postgres
//! belongs in provider-specific paths (§7.6).

use std::fmt::Write as _;
use std::sync::Arc;

use standalone_cluster_plugin::StandaloneCacheProvider;
use tonic::Request;
use tonic::metadata::MetadataValue;
use toolkit::client_hub::ClientHub;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;

use super::subscriptions::{ElectionSubscriptions, SharedSubscriptions};
use super::{CallerResolver, ServiceContext};
use crate::domain::registry::ProfileRegistry;
use crate::{ClusterConfig, ClusterHandle, ClusterWiring, ProviderRegistry};

/// A running gear: wired backends, a published registry, and the shared context
/// the four services are built from.
pub(super) struct Harness {
    pub(super) ctx: ServiceContext,
    pub(super) subscriptions: SharedSubscriptions,
    pub(super) registry: Arc<ProfileRegistry>,
    handle: ClusterHandle,
}

impl Harness {
    /// Wires `profiles` as standalone-cache profiles and publishes them, exactly
    /// as the gear's `start` does.
    pub(super) async fn wired(profiles: &[&str]) -> Self {
        let mut yaml = String::from("profiles:\n");
        for name in profiles {
            writeln!(yaml, "  {name}:\n    cache: {{ provider: standalone }}")
                .expect("writing to a String cannot fail");
        }
        let cfg: ClusterConfig = serde_saphyr::from_str(&yaml).expect("config parses");
        let providers =
            ProviderRegistry::new().with_cache_provider(Arc::new(StandaloneCacheProvider));
        let (handle, bound) =
            ClusterWiring::from_config(Arc::new(ClientHub::new()), &cfg, &providers)
                .await
                .expect("wiring starts");

        let registry = Arc::new(ProfileRegistry::new());
        registry.publish(bound);

        Self::over(registry, handle)
    }

    /// A harness whose registry is **empty**, as it is between the gear's `init`
    /// and its `start`. Every request against it must resolve to
    /// `ProfileNotBound`.
    pub(super) async fn unpublished() -> Self {
        // Still wired, so the handle is real and `stop` has something to drain;
        // the registry simply never sees the bound set.
        let cfg: ClusterConfig =
            serde_saphyr::from_str("profiles:\n  orders:\n    cache: { provider: standalone }\n")
                .expect("config parses");
        let providers =
            ProviderRegistry::new().with_cache_provider(Arc::new(StandaloneCacheProvider));
        let (handle, _bound) =
            ClusterWiring::from_config(Arc::new(ClientHub::new()), &cfg, &providers)
                .await
                .expect("wiring starts");

        Self::over(Arc::new(ProfileRegistry::new()), handle)
    }

    fn over(registry: Arc<ProfileRegistry>, handle: ClusterHandle) -> Self {
        let subscriptions: SharedSubscriptions = Arc::new(ElectionSubscriptions::new());
        let ctx = ServiceContext::new(
            Arc::clone(&registry),
            // v1's mode. The identity tests cover the validated mode; these
            // cover the services, so the caller is held constant.
            CallerResolver::trusted_network(),
        );
        Self {
            ctx,
            subscriptions,
            registry,
            handle,
        }
    }

    /// Drains the wiring. Called explicitly so a test that leaks it fails loudly
    /// under a runtime shutdown rather than quietly.
    pub(super) async fn stop(self) {
        self.handle.stop().await;
    }
}

/// A request carrying a platform-plane credential, as an inbound call has one.
pub(super) fn request<T>(body: T) -> Request<T> {
    let mut request = Request::new(body);
    request.metadata_mut().insert(
        INTERNAL_TOKEN_HEADER,
        MetadataValue::from_static("a-projected-sa-token"),
    );
    request
}

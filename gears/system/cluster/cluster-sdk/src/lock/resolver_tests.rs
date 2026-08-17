// Created: 2026-06-11 by Constructor Tech
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use toolkit::client_hub::ClientHub;

use super::{LockResolverBuilder, validate_lock_capabilities};
use crate::error::ClusterError;
use crate::lock::backend::DistributedLockBackend;
use crate::lock::facade::DistributedLockV1;
use crate::lock::guard::LockGuard;
use crate::lock::types::{LockCapability, LockFeatures};
use crate::profile::ClusterProfile;
use crate::test_support::StubClusterClient;
use crate::test_support::with_nothing_derivable;

struct StubBackend {
    linearizable: bool,
}

#[async_trait]
impl DistributedLockBackend for StubBackend {
    fn features(&self) -> LockFeatures {
        LockFeatures::new(self.linearizable)
    }
    async fn try_lock(&self, name: &str, _ttl: Duration) -> Result<LockGuard, ClusterError> {
        let (_rx, guard) = LockGuard::channel(name.to_owned(), 1);
        Ok(guard)
    }
    async fn lock(
        &self,
        name: &str,
        _ttl: Duration,
        _timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        let (_rx, guard) = LockGuard::channel(name.to_owned(), 1);
        Ok(guard)
    }
}

#[derive(Clone, Copy)]
struct RateLimiterProfile;
impl ClusterProfile for RateLimiterProfile {
    const NAME: &'static str = "rate-limiter";
}

#[test]
fn validate_passes_when_capability_met() {
    let backend = StubBackend { linearizable: true };
    assert!(validate_lock_capabilities(&backend, &[LockCapability::Linearizable]).is_ok());
}

#[test]
fn validate_rejects_unmet_linearizable() {
    let backend = StubBackend {
        linearizable: false,
    };
    let Err(ClusterError::CapabilityNotMet {
        primitive,
        capability,
        provider,
    }) = validate_lock_capabilities(&backend, &[LockCapability::Linearizable])
    else {
        panic!("an unmet linearizable requirement must be rejected");
    };
    assert_eq!(primitive, "DistributedLockV1");
    assert_eq!(capability, "Linearizable");
    // The error names the concrete backend, not the erased `dyn` trait type.
    assert!(
        provider.contains("StubBackend"),
        "provider should name the concrete backend, got `{provider}`"
    );
}

#[tokio::test]
async fn resolve_without_profile_errors() {
    let hub = ClientHub::new();
    let result = LockResolverBuilder::new(&hub).resolve().await;
    assert!(matches!(result, Err(ClusterError::ProfileNotSpecified)));
}

/// Registers a cluster client binding `backend` to the rate-limiter profile —
/// since `K4` the resolvers bind through `dyn ClusterClient` rather than through a
/// per-profile hub registration (DESIGN-DEPLOYABLE-GEAR §4.9.3).
fn client_with(hub: &ClientHub, backend: StubBackend) {
    StubClusterClient::for_profile(RateLimiterProfile::NAME)
        .with_lock(Arc::new(backend))
        .register(hub);
}

#[tokio::test]
async fn resolve_unbound_profile_errors() {
    let hub = ClientHub::new();
    // Cluster is reachable and binds a different profile: a permanent config
    // error, returned by `resolve()` itself (DESIGN §4.7).
    StubClusterClient::for_profile("other")
        .with_lock(Arc::new(StubBackend { linearizable: true }))
        .register(&hub);

    let result = DistributedLockV1::resolver(&hub)
        .profile(RateLimiterProfile)
        .resolve()
        .await;
    assert!(matches!(
        result,
        Err(ClusterError::ProfileNotBound {
            profile: "rate-limiter"
        })
    ));
}

/// Nothing wired: `resolve()` succeeds and the first call reports it (§4.9.1).
#[tokio::test]
async fn resolve_with_nothing_wired_succeeds_and_the_first_call_reports_it() {
    let hub = ClientHub::new();

    // See `with_nothing_derivable`: without it, a concurrent test's `POD_NAMESPACE`
    // would let the resolve path self-construct a client and bind this facade.
    let Ok(lock) = with_nothing_derivable(
        DistributedLockV1::resolver(&hub)
            .profile(RateLimiterProfile)
            .resolve(),
    )
    .await
    else {
        panic!("an empty hub must not fail resolution");
    };

    assert!(!lock.features().linearizable);
    assert!(matches!(
        lock.try_lock("ledger", Duration::from_secs(1)).await,
        Err(ClusterError::ProfileNotBound {
            profile: "rate-limiter"
        })
    ));
}

#[tokio::test]
async fn resolve_happy_path_returns_facade() {
    let hub = ClientHub::new();
    client_with(&hub, StubBackend { linearizable: true });

    let Ok(lock) = DistributedLockV1::resolver(&hub)
        .profile(RateLimiterProfile)
        .require(LockCapability::Linearizable)
        .resolve()
        .await
    else {
        panic!("resolution against a matching backend must succeed");
    };
    assert!(lock.features().linearizable);
}

#[tokio::test]
async fn resolve_rejects_capability_mismatch_at_startup() {
    let hub = ClientHub::new();
    client_with(
        &hub,
        StubBackend {
            linearizable: false,
        },
    );

    let result = DistributedLockV1::resolver(&hub)
        .profile(RateLimiterProfile)
        .require(LockCapability::Linearizable)
        .resolve()
        .await;
    assert!(matches!(
        result,
        Err(ClusterError::CapabilityNotMet {
            capability: "Linearizable",
            ..
        })
    ));
}

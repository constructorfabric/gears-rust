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
use crate::profile::{ClusterProfile, profile_scope};

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

#[test]
fn resolve_without_profile_errors() {
    let hub = ClientHub::new();
    let result = LockResolverBuilder::new(&hub).resolve();
    assert!(matches!(result, Err(ClusterError::ProfileNotSpecified)));
}

#[test]
fn resolve_unbound_profile_errors() {
    let hub = ClientHub::new();
    let result = DistributedLockV1::resolver(&hub)
        .profile(RateLimiterProfile)
        .resolve();
    assert!(matches!(
        result,
        Err(ClusterError::ProfileNotBound {
            profile: "rate-limiter"
        })
    ));
}

#[test]
fn resolve_happy_path_returns_facade() {
    let hub = ClientHub::new();
    let Ok(scope) = profile_scope(RateLimiterProfile::NAME) else {
        panic!("valid profile name must produce a scope");
    };
    let backend: Arc<dyn DistributedLockBackend> = Arc::new(StubBackend { linearizable: true });
    hub.register_scoped::<dyn DistributedLockBackend>(scope, backend);

    let Ok(lock) = DistributedLockV1::resolver(&hub)
        .profile(RateLimiterProfile)
        .require(LockCapability::Linearizable)
        .resolve()
    else {
        panic!("resolution against a matching backend must succeed");
    };
    assert!(lock.features().linearizable);
}

#[test]
fn resolve_rejects_capability_mismatch_at_startup() {
    let hub = ClientHub::new();
    let Ok(scope) = profile_scope(RateLimiterProfile::NAME) else {
        panic!("valid profile name must produce a scope");
    };
    let backend: Arc<dyn DistributedLockBackend> = Arc::new(StubBackend {
        linearizable: false,
    });
    hub.register_scoped::<dyn DistributedLockBackend>(scope, backend);

    let result = DistributedLockV1::resolver(&hub)
        .profile(RateLimiterProfile)
        .require(LockCapability::Linearizable)
        .resolve();
    assert!(matches!(
        result,
        Err(ClusterError::CapabilityNotMet {
            capability: "Linearizable",
            ..
        })
    ));
}

// Created: 2026-06-11 by Constructor Tech
use std::sync::Arc;

use async_trait::async_trait;
use toolkit::client_hub::ClientHub;

use super::{CacheResolverBuilder, validate_cache_capabilities};
use crate::cache::backend::ClusterCacheBackend;
use crate::cache::facade::ClusterCacheV1;
use crate::cache::types::{
    CacheCapability, CacheConsistency, CacheEntry, CacheFeatures, PutRequest, Ttl,
};
use crate::cache::watch::CacheWatch;
use crate::error::ClusterError;
use crate::profile::{ClusterProfile, profile_scope};

struct StubBackend {
    consistency: CacheConsistency,
    prefix_watch: bool,
}

#[async_trait]
impl ClusterCacheBackend for StubBackend {
    fn consistency(&self) -> CacheConsistency {
        self.consistency
    }
    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(self.prefix_watch)
    }
    async fn get(&self, _key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        Ok(None)
    }
    async fn put(&self, _req: PutRequest<'_>) -> Result<(), ClusterError> {
        Ok(())
    }
    async fn delete(&self, _key: &str) -> Result<bool, ClusterError> {
        Ok(false)
    }
    async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
        Ok(false)
    }
    async fn put_if_absent(
        &self,
        _req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        Ok(None)
    }
    async fn compare_and_swap(
        &self,
        _key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        Ok(CacheEntry {
            value: Vec::new(),
            version: 1,
        })
    }
    async fn watch(&self, _key: &str) -> Result<CacheWatch, ClusterError> {
        let (_tx, watch) = CacheWatch::channel(1);
        Ok(watch)
    }
    async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
        // Honest declaration: track whatever `features()` advertises. A stub that
        // claims `prefix_watch` must actually return a watch, not `Unsupported`.
        if self.prefix_watch {
            let (_tx, watch) = CacheWatch::channel(1);
            Ok(watch)
        } else {
            Err(ClusterError::Unsupported {
                feature: "prefix_watch",
            })
        }
    }
}

#[derive(Clone, Copy)]
struct OrdersProfile;
impl ClusterProfile for OrdersProfile {
    const NAME: &'static str = "orders";
}

fn linearizable_backend() -> StubBackend {
    StubBackend {
        consistency: CacheConsistency::Linearizable,
        prefix_watch: true,
    }
}

#[test]
fn validate_passes_when_capabilities_met() {
    let backend = linearizable_backend();
    assert!(
        validate_cache_capabilities(
            &backend,
            &[CacheCapability::Linearizable, CacheCapability::PrefixWatch]
        )
        .is_ok()
    );
}

#[test]
fn validate_rejects_unmet_linearizable() {
    let backend = StubBackend {
        consistency: CacheConsistency::EventuallyConsistent,
        prefix_watch: true,
    };
    let Err(ClusterError::CapabilityNotMet {
        capability,
        provider,
        ..
    }) = validate_cache_capabilities(&backend, &[CacheCapability::Linearizable])
    else {
        panic!("an unmet linearizable requirement must be rejected");
    };
    assert_eq!(capability, "Linearizable");
    // The error names the concrete backend, not the erased `dyn` trait type.
    assert!(
        provider.contains("StubBackend"),
        "provider should name the concrete backend, got `{provider}`"
    );
}

#[test]
fn validate_rejects_unmet_prefix_watch() {
    let backend = StubBackend {
        consistency: CacheConsistency::Linearizable,
        prefix_watch: false,
    };
    assert!(matches!(
        validate_cache_capabilities(&backend, &[CacheCapability::PrefixWatch]),
        Err(ClusterError::CapabilityNotMet {
            capability: "PrefixWatch",
            ..
        })
    ));
}

#[test]
fn resolve_without_profile_errors() {
    let hub = ClientHub::new();
    let result = CacheResolverBuilder::new(&hub).resolve();
    assert!(matches!(result, Err(ClusterError::ProfileNotSpecified)));
}

#[test]
fn resolve_unbound_profile_errors() {
    let hub = ClientHub::new();
    let result = ClusterCacheV1::resolver(&hub)
        .profile(OrdersProfile)
        .resolve();
    assert!(matches!(
        result,
        Err(ClusterError::ProfileNotBound { profile: "orders" })
    ));
}

#[test]
fn resolve_happy_path_returns_facade() {
    let hub = ClientHub::new();
    let Ok(scope) = profile_scope(OrdersProfile::NAME) else {
        panic!("valid profile name must produce a scope");
    };
    let backend: Arc<dyn ClusterCacheBackend> = Arc::new(linearizable_backend());
    hub.register_scoped::<dyn ClusterCacheBackend>(scope, backend);

    let Ok(cache) = ClusterCacheV1::resolver(&hub)
        .profile(OrdersProfile)
        .require(CacheCapability::Linearizable)
        .resolve()
    else {
        panic!("resolution against a matching backend must succeed");
    };
    assert_eq!(cache.consistency(), CacheConsistency::Linearizable);
}

#[test]
fn resolve_rejects_capability_mismatch_at_startup() {
    let hub = ClientHub::new();
    let Ok(scope) = profile_scope(OrdersProfile::NAME) else {
        panic!("valid profile name must produce a scope");
    };
    let backend: Arc<dyn ClusterCacheBackend> = Arc::new(StubBackend {
        consistency: CacheConsistency::EventuallyConsistent,
        prefix_watch: true,
    });
    hub.register_scoped::<dyn ClusterCacheBackend>(scope, backend);

    let result = ClusterCacheV1::resolver(&hub)
        .profile(OrdersProfile)
        .require(CacheCapability::Linearizable)
        .resolve();
    assert!(matches!(
        result,
        Err(ClusterError::CapabilityNotMet {
            capability: "Linearizable",
            ..
        })
    ));
}

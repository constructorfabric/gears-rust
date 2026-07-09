// Created: 2026-06-11 by Constructor Tech
use std::sync::Arc;

use async_trait::async_trait;
use toolkit::client_hub::ClientHub;

use super::{deregister_cache_backend, register_cache_backend};
use crate::cache::types::{CacheConsistency, CacheEntry, CacheFeatures, PutRequest, Ttl};
use crate::cache::watch::CacheWatch;
use crate::error::ClusterError;
use crate::profile::ClusterProfile;
use crate::{ClusterCacheBackend, ClusterCacheV1};

struct StubCache;

#[async_trait]
impl ClusterCacheBackend for StubCache {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }
    fn features(&self) -> CacheFeatures {
        // Honest declaration: `watch_prefix` below returns `Unsupported`, so the
        // stub must not advertise `prefix_watch`.
        CacheFeatures::new(false)
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
        Err(ClusterError::Unsupported {
            feature: "prefix_watch",
        })
    }
}

#[derive(Clone, Copy)]
struct OrdersProfile;
impl ClusterProfile for OrdersProfile {
    const NAME: &'static str = "orders";
}

#[test]
fn register_then_resolve_round_trips() {
    let hub = ClientHub::new();
    let backend: Arc<dyn ClusterCacheBackend> = Arc::new(StubCache);

    assert!(register_cache_backend(&hub, OrdersProfile::NAME, backend).is_ok());

    // A consumer resolving the cache for the profile receives the backend.
    let resolved = ClusterCacheV1::resolver(&hub)
        .profile(OrdersProfile)
        .resolve();
    assert!(resolved.is_ok(), "registered backend must resolve");
}

#[test]
fn deregister_unbinds_so_later_resolve_reports_profile_not_bound() {
    let hub = ClientHub::new();
    let backend: Arc<dyn ClusterCacheBackend> = Arc::new(StubCache);
    register_cache_backend(&hub, OrdersProfile::NAME, backend)
        .expect("registration of a valid profile must succeed");

    // Deregistration reports that an entry was actually removed.
    assert!(
        deregister_cache_backend(&hub, OrdersProfile::NAME).expect("valid profile name"),
        "deregister must report the removed backend"
    );

    let resolved = ClusterCacheV1::resolver(&hub)
        .profile(OrdersProfile)
        .resolve();
    assert!(matches!(
        resolved,
        Err(ClusterError::ProfileNotBound { profile: "orders" })
    ));
}

#[test]
fn deregister_absent_profile_reports_false() {
    let hub = ClientHub::new();
    assert!(
        !deregister_cache_backend(&hub, OrdersProfile::NAME).expect("valid profile name"),
        "deregister of an absent profile must report nothing removed"
    );
}

#[test]
fn register_rejects_invalid_profile_name_without_mutating_hub() {
    let hub = ClientHub::new();
    let backend: Arc<dyn ClusterCacheBackend> = Arc::new(StubCache);

    let result = register_cache_backend(&hub, "bad:name", backend);
    assert!(matches!(result, Err(ClusterError::InvalidName { .. })));

    // The rejected registration must not have mutated the hub: nothing was
    // inserted, so deregistering any valid profile reports nothing removed.
    assert!(
        !deregister_cache_backend(&hub, OrdersProfile::NAME).expect("valid profile name"),
        "a rejected registration must leave the hub unmodified",
    );
}

use std::sync::Arc;

use async_trait::async_trait;
use toolkit::client_hub::ClientHub;

use super::{deregister_cache_backend, register_cache_backend};
use crate::ClusterCacheBackend;
use crate::cache::types::{CacheConsistency, CacheEntry, CacheFeatures, PutRequest, Ttl};
use crate::cache::watch::CacheWatch;
use crate::error::ClusterError;
use crate::profile::{ClusterProfile, profile_scope};

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

/// The hub entry these helpers write and remove.
///
/// Since `K4` a consumer's `resolve()` goes through the process's
/// `dyn ClusterClient` rather than through this scope, so the round trip is
/// asserted where it now lives — against the hub — rather than through a
/// resolver, which would no longer be testing these functions at all.
fn bound_backend(hub: &ClientHub) -> Option<Arc<dyn ClusterCacheBackend>> {
    let Ok(scope) = profile_scope(OrdersProfile::NAME) else {
        panic!("a valid profile name must produce a scope");
    };
    hub.try_get_scoped::<dyn ClusterCacheBackend>(&scope)
}

#[test]
fn register_then_look_up_round_trips() {
    let hub = ClientHub::new();
    let backend: Arc<dyn ClusterCacheBackend> = Arc::new(StubCache);

    assert!(register_cache_backend(&hub, OrdersProfile::NAME, Arc::clone(&backend)).is_ok());

    let Some(bound) = bound_backend(&hub) else {
        panic!("a registered backend must be bound under the profile scope");
    };
    // The hub receives the instance itself: the gear's local client hands the
    // same `Arc` back out, and its own test asserts the two are one (I14).
    assert!(Arc::ptr_eq(&backend, &bound));
}

#[test]
fn deregister_unbinds_the_profile_scope() {
    let hub = ClientHub::new();
    let backend: Arc<dyn ClusterCacheBackend> = Arc::new(StubCache);
    register_cache_backend(&hub, OrdersProfile::NAME, backend)
        .expect("registration of a valid profile must succeed");

    // Deregistration reports that an entry was actually removed.
    assert!(
        deregister_cache_backend(&hub, OrdersProfile::NAME).expect("valid profile name"),
        "deregister must report the removed backend"
    );

    assert!(
        bound_backend(&hub).is_none(),
        "a deregistered profile must leave no binding behind"
    );
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

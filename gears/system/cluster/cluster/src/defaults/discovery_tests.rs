use std::collections::HashMap;
use std::time::Duration;

use super::CacheBasedServiceDiscoveryBackend;
use crate::defaults::ShutdownRevoke;
use crate::defaults::test_cache::MemoryCache;
use cluster_sdk::discovery::{
    DiscoveryFilter, InstanceState, MetaMatch, ServiceDiscoveryBackend, ServiceRegistration,
    ServiceWatchEvent, StateFilter, TopologyChange,
};
use cluster_sdk::error::ClusterError;

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

fn registration(name: &str, metadata: &[(&str, &str)]) -> ServiceRegistration {
    ServiceRegistration {
        name: name.to_owned(),
        instance_id: None,
        address: "10.0.0.1:9000".to_owned(),
        metadata: metadata
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

#[tokio::test]
async fn features_report_no_metadata_pushdown() {
    let backend = CacheBasedServiceDiscoveryBackend::new(MemoryCache::linearizable());
    assert!(!backend.features().metadata_pushdown);
}

#[tokio::test]
async fn register_then_discover_finds_the_instance() {
    let backend = CacheBasedServiceDiscoveryBackend::new(MemoryCache::linearizable());
    let Ok(handle) = backend.register(registration("delivery", &[])).await else {
        panic!("registration must succeed");
    };
    // `registration(..)` omits the instance id, so the real backend must assign
    // one (cpt-cf-clst-fr-sd-register) rather than leaving it blank.
    assert!(
        !handle.instance_id().is_empty(),
        "backend must assign an instance id when the registration omits one"
    );
    settle().await;
    let Ok(found) = backend
        .discover("delivery", DiscoveryFilter::default())
        .await
    else {
        panic!("discover must succeed");
    };
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].instance_id, handle.instance_id());
}

#[tokio::test]
async fn discover_applies_client_side_metadata_filter() {
    let backend = CacheBasedServiceDiscoveryBackend::new(MemoryCache::linearizable());
    let Ok(_east) = backend
        .register(registration("delivery", &[("region", "us-east")]))
        .await
    else {
        panic!("register east");
    };
    let Ok(_west) = backend
        .register(registration("delivery", &[("region", "us-west")]))
        .await
    else {
        panic!("register west");
    };
    settle().await;
    let filter = DiscoveryFilter::default()
        .require_metadata("region", MetaMatch::Equals("us-east".to_owned()));
    let Ok(found) = backend.discover("delivery", filter).await else {
        panic!("discover must succeed");
    };
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].metadata.get("region").map(String::as_str),
        Some("us-east")
    );
}

#[tokio::test]
async fn watch_observes_join_then_leave() {
    let cache = MemoryCache::linearizable();
    let backend = CacheBasedServiceDiscoveryBackend::new(cache);
    let Ok(mut watch) = backend.watch("delivery").await else {
        panic!("watch must establish");
    };
    let Ok(handle) = backend.register(registration("delivery", &[])).await else {
        panic!("register");
    };
    // The registration's `put` surfaces as a join.
    assert!(matches!(
        watch.recv().await,
        Some(ServiceWatchEvent::Change(TopologyChange::Joined(instance)))
            if instance.instance_id == handle.instance_id()
    ));
    // Deregistration surfaces as a leave.
    let expected_id = handle.instance_id().to_owned();
    assert!(handle.deregister().await.is_ok());
    assert!(matches!(
        watch.recv().await,
        Some(ServiceWatchEvent::Change(TopologyChange::Left { instance_id }))
            if instance_id == expected_id
    ));
}

#[tokio::test]
async fn revoke_closes_an_active_watch_with_shutdown() {
    let cache = MemoryCache::linearizable();
    let backend = CacheBasedServiceDiscoveryBackend::new(cache);
    let Ok(mut watch) = backend.watch("delivery").await else {
        panic!("watch must establish");
    };
    // Let the translator task reach its wait, then revoke the backend.
    settle().await;
    backend.revoke().await;
    // The active watch observes a terminal Closed(Shutdown).
    assert!(
        matches!(
            watch.recv().await,
            Some(ServiceWatchEvent::Closed(ClusterError::Shutdown))
        ),
        "an active service-discovery watch must observe Closed(Shutdown) on revoke"
    );
}

#[tokio::test]
async fn set_state_flip_is_reflected_in_discovery() {
    let backend = CacheBasedServiceDiscoveryBackend::new(MemoryCache::linearizable());
    let Ok(handle) = backend.register(registration("delivery", &[])).await else {
        panic!("register");
    };
    settle().await;
    // Drain out of rotation.
    assert!(handle.set_state(InstanceState::Disabled).await.is_ok());
    settle().await;
    // The default (enabled-only) filter now excludes it.
    let Ok(enabled_only) = backend
        .discover("delivery", DiscoveryFilter::default())
        .await
    else {
        panic!("discover must succeed");
    };
    assert!(enabled_only.is_empty());
    // An any-state filter still finds it, now disabled.
    let Ok(any) = backend
        .discover(
            "delivery",
            DiscoveryFilter::any().with_state(StateFilter::Any),
        )
        .await
    else {
        panic!("discover must succeed");
    };
    assert_eq!(any.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn heartbeat_keeps_instance_alive_then_lapses_on_drop() {
    let backend = CacheBasedServiceDiscoveryBackend::new(MemoryCache::linearizable());
    let Ok(handle) = backend.register(registration("delivery", &[])).await else {
        panic!("register");
    };
    // Heartbeat renews every 10s; advance past the 30s TTL — still present.
    for _ in 0..5 {
        tokio::time::advance(Duration::from_secs(11)).await;
        settle().await;
    }
    let Ok(alive) = backend
        .discover("delivery", DiscoveryFilter::default())
        .await
    else {
        panic!("discover must succeed");
    };
    assert_eq!(
        alive.len(),
        1,
        "heartbeat must keep the instance discoverable"
    );
    // Drop the handle: heartbeating stops and the instance lapses via TTL.
    drop(handle);
    tokio::time::advance(Duration::from_secs(31)).await;
    settle().await;
    let Ok(gone) = backend
        .discover("delivery", DiscoveryFilter::default())
        .await
    else {
        panic!("discover must succeed");
    };
    assert!(
        gone.is_empty(),
        "a dropped handle's instance must lapse via TTL"
    );
}

#[tokio::test(start_paused = true)]
async fn revoke_stops_heartbeat_renewal() {
    use cluster_sdk::cache::ClusterCacheBackend;

    let cache = MemoryCache::linearizable();
    let dyn_cache: std::sync::Arc<dyn ClusterCacheBackend> = cache.clone();
    let backend = CacheBasedServiceDiscoveryBackend::new(dyn_cache);
    let Ok(handle) = backend.register(registration("delivery", &[])).await else {
        panic!("register");
    };
    // Keys are namespaced under `svc/` (ADR-001) so SD cannot collide with the
    // leader (`election/`) or lock (`lock/`) keyspaces on a shared cache.
    let key = format!("svc/delivery/{}", handle.instance_id());
    settle().await;
    // Sanity: the entry is live right after registration.
    assert!(
        matches!(cache.get(&key).await, Ok(Some(_))),
        "the registered entry must exist before revoke"
    );
    // Revoke before the handle is dropped: the heartbeat task must observe the
    // shutdown token and stop renewing (the old behavior kept renewing until the
    // handle dropped). `revoke` also awaits the now-tracked heartbeat task.
    backend.revoke().await;
    // Advance past the 30s TTL. With renewal halted nothing re-puts the entry, so
    // it lapses; if the heartbeat were still running it would have renewed at 10s.
    tokio::time::advance(Duration::from_secs(31)).await;
    settle().await;
    assert!(
        matches!(cache.get(&key).await, Ok(None)),
        "revoke must stop heartbeat renewal so the entry lapses via TTL"
    );
    // The handle outlives the assertions to prove the task stopped on the token,
    // not because the handle was dropped.
    drop(handle);
}

#[tokio::test]
async fn registration_is_namespaced_under_svc_and_cannot_collide_with_election_keys() {
    use cluster_sdk::cache::ClusterCacheBackend;

    let cache = MemoryCache::linearizable();
    let dyn_cache: std::sync::Arc<dyn ClusterCacheBackend> = cache.clone();
    let backend = CacheBasedServiceDiscoveryBackend::new(dyn_cache);
    // A service literally named `election` must not land in the leader-election
    // keyspace (`election/...`) when both defaults share one cache (ADR-001).
    let Ok(handle) = backend.register(registration("election", &[])).await else {
        panic!("register");
    };
    let id = handle.instance_id();
    assert!(
        matches!(cache.get(&format!("svc/election/{id}")).await, Ok(Some(_))),
        "the registration must live under the svc/ namespace"
    );
    assert!(
        matches!(cache.get(&format!("election/{id}")).await, Ok(None)),
        "the registration must not collide with the leader-election keyspace"
    );
    drop(handle);
}

#[tokio::test(start_paused = true)]
async fn revoke_stops_the_store_maintainer() {
    // `discover` spawns a StoreMaintainer driven by a `watch_prefix` stream that
    // never closes on its own. The maintainer is tracked and selects on the
    // shutdown token, so revoke() — which cancels the token then awaits the
    // tracked tasks — completes instead of hanging forever on the open stream.
    let backend = CacheBasedServiceDiscoveryBackend::new(MemoryCache::linearizable());
    let _found = backend
        .discover("delivery", DiscoveryFilter::default())
        .await;
    settle().await;
    tokio::time::timeout(Duration::from_secs(5), backend.revoke())
        .await
        .expect("revoke must stop the tracked store maintainer, not hang on the open watch stream");
}

#[tokio::test]
async fn watch_surfaces_unsupported_prefix_watch() {
    let backend =
        CacheBasedServiceDiscoveryBackend::new(MemoryCache::linearizable_without_prefix_watch());
    assert!(matches!(
        backend.watch("delivery").await,
        Err(ClusterError::Unsupported {
            feature: "prefix_watch"
        })
    ));
    // Registration still succeeds — only the cross-process view degrades.
    assert!(
        backend
            .register(registration("delivery", &[]))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn degraded_register_does_not_leak_an_unreapable_local_instance() {
    // Without native prefix watch no maintainer runs, so nothing would reap a
    // pre-inserted instance on TTL expiry. `register` must therefore skip the
    // pre-insert and `discover` must stay best-effort empty — not surface a
    // local instance that would read as live forever.
    let backend =
        CacheBasedServiceDiscoveryBackend::new(MemoryCache::linearizable_without_prefix_watch());
    assert!(
        backend
            .register(registration("delivery", &[]))
            .await
            .is_ok()
    );
    settle().await;
    let Ok(found) = backend
        .discover("delivery", DiscoveryFilter::default())
        .await
    else {
        panic!("discover must succeed (best-effort) even in degraded mode");
    };
    assert!(
        found.is_empty(),
        "degraded discover must be best-effort empty, not a stale unreapable instance"
    );
}

#[tokio::test]
async fn concurrent_degraded_registers_do_not_leak_an_unreapable_local_instance() {
    // Regression: `ensure_maintainer` marked a name maintained *before* awaiting
    // `watch_prefix`. With a degraded cache two concurrent `register`s for the
    // same name could interleave so the second observed the first's optimistic
    // mark, pre-inserted an instance, and then the first's `watch_prefix` failed
    // and rolled the mark back — leaving an instance no maintainer would ever
    // reap. The slow `watch_prefix` makes that interleaving deterministic.
    let backend = CacheBasedServiceDiscoveryBackend::new(
        MemoryCache::linearizable_without_prefix_watch_slow(),
    );
    let (first, second) = tokio::join!(
        backend.register(registration("delivery", &[])),
        backend.register(registration("delivery", &[])),
    );
    assert!(
        first.is_ok() && second.is_ok(),
        "both registrations succeed"
    );
    settle().await;
    let Ok(found) = backend
        .discover("delivery", DiscoveryFilter::default())
        .await
    else {
        panic!("discover must succeed (best-effort) even in degraded mode");
    };
    assert!(
        found.is_empty(),
        "no maintainer is running, so degraded discover must stay best-effort \
         empty even under concurrent registration, not leak an unreapable instance"
    );
}

#[test]
fn instance_record_round_trips_through_the_codec() {
    use super::InstanceRecord;
    use cluster_sdk::discovery::InstanceState;
    use std::time::SystemTime;

    let mut metadata = HashMap::new();
    metadata.insert("region".to_owned(), "us-east".to_owned());
    metadata.insert("shard".to_owned(), "3".to_owned());
    let record = InstanceRecord {
        address: "10.0.0.5:8080".to_owned(),
        metadata: metadata.clone(),
        state: InstanceState::Disabled,
        registered_at: SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 123),
    };
    let Some(decoded) = InstanceRecord::decode(&record.encode()) else {
        panic!("a well-formed value must decode");
    };
    assert_eq!(decoded.address, "10.0.0.5:8080");
    assert_eq!(decoded.metadata, metadata);
    assert_eq!(decoded.state, InstanceState::Disabled);
    assert_eq!(decoded.registered_at, record.registered_at);
}

#[test]
fn malformed_value_decodes_to_none() {
    use super::InstanceRecord;
    assert!(InstanceRecord::decode(&[]).is_none());
    // A truncated length-prefixed field is rejected rather than panicking.
    assert!(InstanceRecord::decode(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255]).is_none());
}

// @cpt-dod:cpt-cf-clst-dod-smoke-tests-coordination:p1
//! Contract smoke tests: coordination behavior across the primitives
//! (`cpt-cf-clst-dod-smoke-tests-coordination`).
//!
//! Exercises the behaviors a conforming backend must reproduce: CAS conflict
//! surfacing, single-leader under multi-task contention, lock release on a
//! timed-out acquisition and on explicit release, composable scoping prefix
//! translation, and the prefix-watch polyfill diff — all over the in-process
//! fixture with no external infrastructure.

mod common;

use std::sync::Arc;
use std::time::Duration;

use cluster::defaults::{CasBasedDistributedLockBackend, CasBasedLeaderElectionBackend};
use cluster_sdk::cache::{
    CacheEvent, CacheWatchEvent, ClusterCacheBackend, ClusterCacheV1, PutRequest, Ttl,
};
use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{LeaderElectionV1, LeaderStatus, LeaderWatch, LeaderWatchEvent};
use cluster_sdk::lock::DistributedLockV1;
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::registration::{
    register_cache_backend, register_leader_election_backend, register_lock_backend,
};
use common::{MemCacheBackend, SmokeProfile};
use toolkit::client_hub::ClientHub;

/// Registers `backend` as the cache for the smoke profile and resolves the
/// facade. Panics on setup failure (a fixture wiring bug is a test bug).
fn cache_facade(hub: &ClientHub, backend: Arc<dyn ClusterCacheBackend>) -> ClusterCacheV1 {
    assert!(register_cache_backend(hub, SmokeProfile::NAME, backend).is_ok());
    let Ok(cache) = ClusterCacheV1::resolver(hub)
        .profile(SmokeProfile)
        .resolve()
    else {
        panic!("cache must resolve against the bound backend");
    };
    cache
}

/// Awaits the watch's first leadership status, skipping non-status signals.
async fn first_status(watch: &mut LeaderWatch) -> LeaderStatus {
    loop {
        match watch.changed().await {
            LeaderWatchEvent::Status(status) => return status,
            LeaderWatchEvent::Closed(err) => panic!("watch closed before reporting status: {err}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn cas_conflict_surfaces_on_stale_version() {
    let hub = ClientHub::new();
    let cache = cache_facade(&hub, MemCacheBackend::linearizable());

    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-get
    let Ok(Some(created)) = cache
        .put_if_absent(PutRequest {
            key: "cas",
            value: b"a",
            ttl: Ttl::Indefinite,
        })
        .await
    else {
        panic!("initial create must succeed");
    };
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-get
    // A stale expected version conflicts.
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-compute
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-swap
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-conflict
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-conflict-return
    assert!(matches!(
        cache
            .compare_and_swap("cas", created.version + 5, b"z", Ttl::Indefinite)
            .await,
        Err(ClusterError::CasConflict { .. })
    ));
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-conflict-return
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-conflict
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-swap
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-compute
    // The matching version swaps and bumps the version.
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-retry
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-return
    let Ok(swapped) = cache
        .compare_and_swap("cas", created.version, b"b", Ttl::Indefinite)
        .await
    else {
        panic!("a matching compare-and-swap must succeed");
    };
    assert_eq!(swapped.version, created.version + 1);
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-return
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-cas-update:p1:inst-cas-retry
}

#[tokio::test]
async fn single_leader_under_contention() {
    let hub = ClientHub::new();
    let cache: Arc<dyn ClusterCacheBackend> = MemCacheBackend::linearizable();
    let Ok(backend) = CasBasedLeaderElectionBackend::new(Arc::clone(&cache)) else {
        panic!("leader backend must construct over a linearizable cache");
    };
    assert!(register_leader_election_backend(&hub, SmokeProfile::NAME, Arc::new(backend)).is_ok());
    let Ok(leader) = LeaderElectionV1::resolver(&hub)
        .profile(SmokeProfile)
        .resolve()
    else {
        panic!("leader election must resolve");
    };

    // Two candidates contend for the same election over the shared cache.
    let Ok(mut first) = leader.elect("svc").await else {
        panic!("first candidate must enroll");
    };
    let Ok(mut second) = leader.elect("svc").await else {
        panic!("second candidate must enroll");
    };

    let s1 = first_status(&mut first).await;
    let s2 = first_status(&mut second).await;
    let mut leaders = 0;
    if s1 == LeaderStatus::Leader {
        leaders += 1;
    }
    if s2 == LeaderStatus::Leader {
        leaders += 1;
    }
    assert_eq!(
        leaders, 1,
        "exactly one candidate may lead (got {s1:?}, {s2:?})"
    );
}

#[tokio::test]
async fn lock_contended_times_out_then_releases_for_reacquire() {
    let hub = ClientHub::new();
    let cache: Arc<dyn ClusterCacheBackend> = MemCacheBackend::linearizable();
    let Ok(backend) = CasBasedDistributedLockBackend::new(Arc::clone(&cache)) else {
        panic!("lock backend must construct over a linearizable cache");
    };
    assert!(register_lock_backend(&hub, SmokeProfile::NAME, Arc::new(backend)).is_ok());
    let Ok(lock) = DistributedLockV1::resolver(&hub)
        .profile(SmokeProfile)
        .resolve()
    else {
        panic!("distributed lock must resolve");
    };

    let ttl = Duration::from_secs(30);
    let Ok(guard) = lock.try_lock("L", ttl).await else {
        panic!("the first acquisition must succeed");
    };

    // Non-blocking acquisition of a held lock is refused.
    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-held
    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-contended
    assert!(matches!(
        lock.try_lock("L", ttl).await,
        Err(ClusterError::LockContended { .. })
    ));
    // @cpt-end:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-contended
    // @cpt-end:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-held

    // Blocking acquisition gives up after the timeout while the lock stays held.
    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-lock
    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-timeout
    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-timeout-return
    let Err(ClusterError::LockTimeout { name, .. }) =
        lock.lock("L", ttl, Duration::from_millis(100)).await
    else {
        panic!("a blocking acquisition of a held lock must time out");
    };
    assert_eq!(name, "L");
    // @cpt-end:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-timeout-return
    // @cpt-end:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-timeout
    // @cpt-end:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-lock

    // Explicit release frees the lock; a subsequent acquisition then succeeds.
    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-release
    assert!(guard.release().await.is_ok());
    // @cpt-end:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-release
    let Ok(reacquired) = lock.try_lock("L", ttl).await else {
        panic!("re-acquisition after an explicit release must succeed");
    };
    assert!(reacquired.release().await.is_ok());
}

#[tokio::test]
async fn composable_scoping_translates_prefixes_on_write_and_read() {
    let hub = ClientHub::new();
    let backend = MemCacheBackend::linearizable();
    let cache = cache_facade(&hub, Arc::clone(&backend) as Arc<dyn ClusterCacheBackend>);

    // Composed scopes prefix the key as `a/b/<key>` on the backend (DESIGN §3.8).
    let Ok(scoped) = cache.scoped("a").and_then(|inner| inner.scoped("b")) else {
        panic!("composing scopes `a/b` must validate");
    };
    assert!(
        scoped
            .put(PutRequest {
                key: "key",
                value: b"v",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );

    // The underlying backend observes the fully-prefixed key.
    let Ok(keys) = backend.scan_prefix("").await else {
        panic!("backend scan must succeed");
    };
    assert!(
        keys.iter().any(|k| k == "a/b/key"),
        "backend must observe the composed prefix, got {keys:?}"
    );

    // A scoped read strips the prefix transparently.
    let Ok(Some(entry)) = scoped.get("key").await else {
        panic!("a scoped read must observe its own scoped write");
    };
    assert_eq!(entry.value, b"v");

    // The unscoped facade does not see the bare key (it lives under the prefix).
    let Ok(None) = cache.get("key").await else {
        panic!("the unscoped facade must not see the scoped key");
    };
}

#[tokio::test(start_paused = true)]
async fn prefix_watch_polyfill_detects_a_new_key() {
    let hub = ClientHub::new();
    let backend = MemCacheBackend::linearizable_without_prefix_watch();
    let cache = cache_facade(&hub, Arc::clone(&backend) as Arc<dyn ClusterCacheBackend>);

    // The backend declares no native prefix watch.
    assert!(matches!(
        cache.watch_prefix("svc/").await,
        Err(ClusterError::Unsupported {
            feature: "prefix_watch"
        })
    ));

    // The polling polyfill synthesizes prefix-watch semantics by diffing scans.
    let interval = Duration::from_millis(20);
    let mut watch = cache.watch_prefix_polling("svc/", interval);
    // Let the first (immediate) tick establish an empty baseline.
    tokio::task::yield_now().await;

    assert!(
        cache
            .put(PutRequest {
                key: "svc/a",
                value: b"1",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    // Advance to the next poll; the diff detects the new key.
    tokio::time::advance(interval).await;

    let Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) = watch.recv().await else {
        panic!("the polyfill must surface the new key as a Changed event");
    };
    assert_eq!(key, "svc/a");
}

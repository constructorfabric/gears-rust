// Created: 2026-06-24 by Constructor Tech
//! Cache conformance scenarios (`SC-CACHE-*`).
//!
//! See the [scenario catalog](../docs/scenarios/cache.md) for per-scenario
//! intent/steps/expected/done-when. Each `scenario_cache_*` is a `pub async fn`
//! a plugin may call directly; [`run_cache_conformance`] drives the full L2 set,
//! building a fresh backend per scenario via `make` so state never leaks.

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::cache::{
    CacheConsistency, CacheEvent, CacheWatchEvent, ClusterCacheBackend, PollingPrefixWatch,
    PutRequest, Ttl,
};
use cluster_sdk::error::ClusterError;

/// Runs every implemented L2 cache scenario against a fresh backend from `make`.
///
/// Capability-gated scenarios read `consistency()`/`features()` off the backend
/// and assert strict guarantees only where the backend claims them.
pub async fn run_cache_conformance<F>(make: F)
where
    F: Fn() -> Arc<dyn ClusterCacheBackend>,
{
    scenario_cache_001(make()).await;
    scenario_cache_002(make()).await;
    scenario_cache_003(make()).await;
    scenario_cache_004(make()).await;
    scenario_cache_005(make()).await;
    scenario_cache_006(make()).await;
    scenario_cache_007(make()).await;
    scenario_cache_008(make()).await;
    scenario_cache_009(make()).await;
    scenario_cache_010(make()).await;
    scenario_cache_011(make()).await;
    scenario_cache_012(make()).await;
    scenario_cache_013(make()).await;
    scenario_cache_014(make()).await;
    scenario_cache_015(make()).await;
}

/// SC-CACHE-001: `get` on an absent key returns `Ok(None)`, never an error.
pub async fn scenario_cache_001(backend: Arc<dyn ClusterCacheBackend>) {
    let got = backend.get("absent").await;
    assert!(
        matches!(got, Ok(None)),
        "SC-CACHE-001: get on absent key must be Ok(None), got {got:?}"
    );
}

/// SC-CACHE-002: `put` then `get` returns the stored value at version 1.
pub async fn scenario_cache_002(backend: Arc<dyn ClusterCacheBackend>) {
    backend
        .put(PutRequest {
            key: "k",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put must succeed");
    let entry = backend
        .get("k")
        .await
        .expect("get must succeed")
        .expect("entry must be present");
    assert_eq!(
        entry.value, b"v",
        "SC-CACHE-002: stored value must round-trip"
    );
    assert_eq!(entry.version, 1, "SC-CACHE-002: first write is version 1");
}

/// SC-CACHE-003: each overwrite strictly increments the version; version 0 is
/// never observed (it is the reserved sentinel).
pub async fn scenario_cache_003(backend: Arc<dyn ClusterCacheBackend>) {
    backend
        .put(PutRequest {
            key: "k",
            value: b"a",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    let v1 = backend
        .get("k")
        .await
        .expect("get")
        .expect("present")
        .version;
    backend
        .put(PutRequest {
            key: "k",
            value: b"b",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    let v2 = backend
        .get("k")
        .await
        .expect("get")
        .expect("present")
        .version;
    assert_ne!(v1, 0, "SC-CACHE-003: version 0 is reserved as a sentinel");
    assert!(
        v2 > v1,
        "SC-CACHE-003: each overwrite must strictly increment version ({v1} -> {v2})"
    );
}

/// SC-CACHE-004: `put_if_absent` returns `Some(entry)` on create, `None` when
/// already present (atomic create).
pub async fn scenario_cache_004(backend: Arc<dyn ClusterCacheBackend>) {
    let created = backend
        .put_if_absent(PutRequest {
            key: "k",
            value: b"a",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("pia");
    assert!(created.is_some(), "SC-CACHE-004: first create returns Some");
    let again = backend
        .put_if_absent(PutRequest {
            key: "k",
            value: b"b",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("pia");
    assert!(
        again.is_none(),
        "SC-CACHE-004: create when present returns None"
    );
}

/// SC-CACHE-005: `compare_and_swap` succeeds iff `expected_version == current`.
pub async fn scenario_cache_005(backend: Arc<dyn ClusterCacheBackend>) {
    let created = backend
        .put_if_absent(PutRequest {
            key: "k",
            value: b"a",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("pia")
        .expect("created");
    let swapped = backend
        .compare_and_swap("k", created.version, b"b", Ttl::Indefinite)
        .await
        .expect("CAS at current version must succeed");
    assert_eq!(
        swapped.value, b"b",
        "SC-CACHE-005: CAS applies the new value"
    );
    assert!(
        swapped.version > created.version,
        "SC-CACHE-005: successful CAS bumps the version"
    );
}

/// SC-CACHE-006: CAS on a stale version returns `CasConflict { key, current }`
/// carrying the current entry.
pub async fn scenario_cache_006(backend: Arc<dyn ClusterCacheBackend>) {
    let created = backend
        .put_if_absent(PutRequest {
            key: "k",
            value: b"a",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("pia")
        .expect("created");
    // Use an unambiguously stale version (far ahead of 1) — avoids the reserved
    // sentinel 0 that wrapping_sub(1) would produce for a freshly-created entry.
    let stale = created.version.wrapping_add(100);
    let err = backend
        .compare_and_swap("k", stale, b"b", Ttl::Indefinite)
        .await
        .expect_err("CAS on a stale version must conflict");
    match err {
        ClusterError::CasConflict { key, current } => {
            assert_eq!(key, "k", "SC-CACHE-006: conflict reports the key");
            let current = current.expect("SC-CACHE-006: conflict carries the current entry");
            assert_eq!(
                current.version, created.version,
                "SC-CACHE-006: conflict carries the live version"
            );
        }
        other => panic!("SC-CACHE-006: expected CasConflict, got {other:?}"),
    }
}

/// SC-CACHE-007: `delete` removes the entry and reports prior existence.
pub async fn scenario_cache_007(backend: Arc<dyn ClusterCacheBackend>) {
    backend
        .put(PutRequest {
            key: "k",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    assert!(
        backend.delete("k").await.expect("delete"),
        "SC-CACHE-007: deleting a present key reports true"
    );
    assert!(
        backend.get("k").await.expect("get").is_none(),
        "SC-CACHE-007: deleted key reads as absent"
    );
    assert!(
        !backend.delete("k").await.expect("delete"),
        "SC-CACHE-007: deleting an absent key reports false"
    );
}

/// SC-CACHE-008: `compare_and_delete` removes only when the owner token matches;
/// a mismatch or absent key returns `Ok(false)`, never an error.
pub async fn scenario_cache_008(backend: Arc<dyn ClusterCacheBackend>) {
    backend
        .put(PutRequest {
            key: "k",
            value: b"owner-a",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    assert!(
        !backend
            .compare_and_delete("k", b"owner-b")
            .await
            .expect("c&d must not error on mismatch"),
        "SC-CACHE-008: mismatched owner token must not delete"
    );
    assert!(
        backend
            .compare_and_delete("k", b"owner-a")
            .await
            .expect("c&d"),
        "SC-CACHE-008: matching owner token deletes"
    );
    assert!(
        !backend
            .compare_and_delete("absent", b"x")
            .await
            .expect("c&d on absent must be Ok(false)"),
        "SC-CACHE-008: absent key returns Ok(false)"
    );
}

/// SC-CACHE-009: a key deleted and re-created resets to version 1, and a
/// value/owner guard still distinguishes the successor — a named regression for
/// the version-reset caveat (a version guard would alias a successor's fresh
/// claim).
pub async fn scenario_cache_009(backend: Arc<dyn ClusterCacheBackend>) {
    // Holder A claims, then its claim lapses and it is deleted.
    backend
        .put(PutRequest {
            key: "lock",
            value: b"holder-a",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    backend.delete("lock").await.expect("delete");
    // Successor B re-creates the key — version resets to 1.
    let b = backend
        .put_if_absent(PutRequest {
            key: "lock",
            value: b"holder-b",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("pia")
        .expect("created");
    assert_eq!(b.version, 1, "SC-CACHE-009: re-create resets version to 1");
    // A's late guarded release must NOT remove B's fresh claim.
    assert!(
        !backend
            .compare_and_delete("lock", b"holder-a")
            .await
            .expect("c&d"),
        "SC-CACHE-009: stale owner token must not delete the successor's claim"
    );
    assert_eq!(
        backend
            .get("lock")
            .await
            .expect("get")
            .expect("present")
            .value,
        b"holder-b",
        "SC-CACHE-009: successor's claim survives the stale release"
    );
}

/// SC-CACHE-010: TTL expiry removes the entry and emits `CacheEvent::Expired` to
/// watchers.
pub async fn scenario_cache_010(backend: Arc<dyn ClusterCacheBackend>) {
    tokio::time::pause();
    let mut watch = backend.watch("k").await.expect("watch");
    backend
        .put(PutRequest {
            key: "k",
            value: b"v",
            ttl: Ttl::Of(Duration::from_millis(50)),
        })
        .await
        .expect("put with ttl");
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;
    // Drain the initial Changed, then wait for the Expired emission.
    let expired = wait_for(
        &mut watch,
        |event| matches!(event, CacheEvent::Expired { key } if key == "k"),
    )
    .await;
    assert!(expired, "SC-CACHE-010: TTL expiry must emit Expired");
    assert!(
        backend.get("k").await.expect("get").is_none(),
        "SC-CACHE-010: expired entry reads as absent"
    );
    tokio::time::resume();
}

/// SC-CACHE-012: exact `watch` yields `Changed`/`Deleted` for the key,
/// preserving per-key order.
pub async fn scenario_cache_012(backend: Arc<dyn ClusterCacheBackend>) {
    let mut watch = backend.watch("k").await.expect("watch");
    backend
        .put(PutRequest {
            key: "k",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    backend.delete("k").await.expect("delete");
    // Enforce order: Changed must precede Deleted on the same watch.
    let mut saw_changed = false;
    for _ in 0..64 {
        match watch.recv().await {
            Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) if key == "k" => {
                assert!(!saw_changed, "SC-CACHE-012: duplicate Changed event");
                saw_changed = true;
            }
            Some(CacheWatchEvent::Event(CacheEvent::Deleted { key })) if key == "k" => {
                assert!(
                    saw_changed,
                    "SC-CACHE-012: Deleted arrived before Changed -- order violated"
                );
                return; // Both events received in correct order.
            }
            Some(CacheWatchEvent::Closed(_)) | None => break,
            _ => {}
        }
    }
    panic!("SC-CACHE-012: did not observe both Changed then Deleted within the event bound");
}

/// SC-CACHE-013: `watch_prefix` yields events for matching keys when the backend
/// declares the `prefix_watch` feature; backends without it return
/// `Unsupported` (capability-gated).
pub async fn scenario_cache_013(backend: Arc<dyn ClusterCacheBackend>) {
    let watch = backend.watch_prefix("p/").await;
    if backend.features().prefix_watch {
        let mut watch = watch.expect("SC-CACHE-013: prefix-watch backend must establish a watch");
        backend
            .put(PutRequest {
                key: "p/a",
                value: b"v",
                ttl: Ttl::Indefinite,
            })
            .await
            .expect("put");
        assert!(
            wait_for(
                &mut watch,
                |e| matches!(e, CacheEvent::Changed { key } if key == "p/a")
            )
            .await,
            "SC-CACHE-013: prefix watch yields events for matching keys"
        );
    } else {
        assert!(
            matches!(
                watch,
                Err(ClusterError::Unsupported {
                    feature: "prefix_watch"
                })
            ),
            "SC-CACHE-013: backend without prefix_watch must return Unsupported"
        );
    }
}

/// SC-CACHE-011: a `Ttl::Indefinite` entry persists well past any default TTL;
/// it is not removed by the TTL sweeper until explicitly deleted.
pub async fn scenario_cache_011(backend: Arc<dyn ClusterCacheBackend>) {
    tokio::time::pause();
    backend
        .put(PutRequest {
            key: "k",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    tokio::time::advance(Duration::from_hours(1)).await;
    // Yield so the sweeper (if any) gets a chance to erroneously remove it.
    tokio::task::yield_now().await;
    let entry = backend.get("k").await.expect("get");
    assert!(
        entry.is_some(),
        "SC-CACHE-011: an indefinite entry must survive a large time advance"
    );
    tokio::time::resume();
}

/// SC-CACHE-014: `PollingPrefixWatch` synthesizes `Changed`/`Deleted` diffs for
/// a backend that does not natively support prefix watches.
/// Capability-gated on `!features().prefix_watch`.
pub async fn scenario_cache_014(backend: Arc<dyn ClusterCacheBackend>) {
    if backend.features().prefix_watch {
        return; // native prefix watch; polyfill is not the subject here
    }
    tokio::time::pause();
    let mut watch = PollingPrefixWatch::spawn(backend.clone(), "p/", Duration::from_millis(25));

    // A put under the prefix must be diffed as Changed.
    backend
        .put(PutRequest {
            key: "p/a",
            value: b"1",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put p/a");
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;
    assert!(
        wait_for(
            &mut watch,
            |e| matches!(e, CacheEvent::Changed { key } if key == "p/a")
        )
        .await,
        "SC-CACHE-014: put under prefix must yield a Changed event"
    );

    // A delete must be diffed as Deleted.
    backend.delete("p/a").await.expect("delete p/a");
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;
    assert!(
        wait_for(
            &mut watch,
            |e| matches!(e, CacheEvent::Deleted { key } if key == "p/a")
        )
        .await,
        "SC-CACHE-014: delete under prefix must yield a Deleted event"
    );
    tokio::time::resume();
}

/// SC-CACHE-015: watch delivery is at-most-once per mutation — a single `put`
/// must not cause more than one `Changed` event for the same key on one watch.
pub async fn scenario_cache_015(backend: Arc<dyn ClusterCacheBackend>) {
    tokio::time::pause();
    let mut watch = backend.watch("k").await.expect("watch");
    backend
        .put(PutRequest {
            key: "k",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");

    let mut changed_count = 0u32;
    // Drain all immediately queued events without advancing time — any duplicate
    // that would arrive synchronously will be in the channel already.
    for _ in 0..64 {
        match tokio::time::timeout(Duration::from_millis(0), watch.recv()).await {
            Ok(Some(CacheWatchEvent::Event(CacheEvent::Changed { key }))) if key == "k" => {
                changed_count += 1;
            }
            Ok(Some(CacheWatchEvent::Closed(_)) | None) | Err(_) => break,
            _ => {}
        }
    }
    assert_eq!(
        changed_count, 1,
        "SC-CACHE-015: a single put must deliver exactly one Changed event (got {changed_count})"
    );
    tokio::time::resume();
}

// TODO(SC-CACHE-016/017) [L4]: slow-subscriber `Lagged` and connection-loss
//   `Reset` are fault-injection scenarios delivered with the L4 harness, not the
//   in-process suite.

/// Polls a watch for up to a bounded number of events, returning `true` once one
/// satisfies `pred`. Bounded so a missing event fails fast rather than hanging.
async fn wait_for<P>(watch: &mut cluster_sdk::cache::CacheWatch, pred: P) -> bool
where
    P: Fn(&CacheEvent) -> bool,
{
    for _ in 0..64 {
        match watch.recv().await {
            Some(CacheWatchEvent::Event(event)) => {
                if pred(&event) {
                    return true;
                }
            }
            Some(CacheWatchEvent::Closed(_)) | None => return false,
            Some(_) => {}
        }
    }
    false
}

/// A helper plugins may use to assert a backend's self-declared consistency
/// before handing it to the consistency-sensitive default backends.
#[must_use]
pub fn is_linearizable(backend: &dyn ClusterCacheBackend) -> bool {
    matches!(backend.consistency(), CacheConsistency::Linearizable)
}

//! Layer 3 — cache integration scenarios (docs/TESTING.md §4.2, `PG-CACHE-001..008`).

#![cfg(feature = "integration")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

mod common;

use std::time::Duration;

use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::error::{ClusterError, ProviderErrorKind};
use postgres_cluster_plugin::PostgresClusterPlugin;
use serde_json::json;

/// `PG-CACHE-001`: `put` + `get` round-trip, verified both through the
/// backend API and directly against the `cluster_cache` table.
#[tokio::test]
async fn pg_cache_001_put_get_round_trip() {
    let (_container, config) = common::start_postgres().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    cache
        .put(PutRequest {
            key: "k1",
            value: b"v1",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put succeeds");

    let entry = cache
        .get("k1")
        .await
        .expect("get succeeds")
        .expect("present");
    assert_eq!(entry.value, b"v1");
    assert_eq!(entry.version, 1);

    let pool = common::raw_pool(&connection_string).await;
    let (value, version): (Vec<u8>, i64) =
        sqlx::query_as("SELECT value, version FROM public.cluster_cache WHERE key = $1")
            .bind("k1")
            .fetch_one(&pool)
            .await
            .expect("row exists");
    assert_eq!(value, b"v1");
    assert_eq!(version, 1);

    handle.stop().await;
}

/// `PG-CACHE-002`: each `put` increments `version` by exactly 1;
/// `put_if_absent` sets version to 1.
#[tokio::test]
async fn pg_cache_002_version_increments_by_one() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let first = cache
        .put_if_absent(PutRequest {
            key: "k2",
            value: b"a",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put_if_absent succeeds")
        .expect("key was absent");
    assert_eq!(
        first.version, 1,
        "PG-CACHE-002: put_if_absent sets version 1"
    );

    for expected_version in 2_u64..=4 {
        cache
            .put(PutRequest {
                key: "k2",
                value: b"b",
                ttl: Ttl::Indefinite,
            })
            .await
            .expect("put succeeds");
        let entry = cache.get("k2").await.expect("get").expect("present");
        assert_eq!(
            entry.version, expected_version,
            "PG-CACHE-002: version must increment by exactly 1 per put"
        );
    }

    // A second `put_if_absent` against an occupied key is a no-op returning
    // `None`, not a fresh version-1 entry.
    let second = cache
        .put_if_absent(PutRequest {
            key: "k2",
            value: b"c",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put_if_absent succeeds");
    assert!(
        second.is_none(),
        "PG-CACHE-002: put_if_absent on an occupied key is a no-op"
    );

    handle.stop().await;
}

/// `PG-CACHE-003`: two concurrent `compare_and_swap` writers race the same
/// key; exactly one wins, the other observes `CasConflict`.
#[tokio::test]
async fn pg_cache_003_cas_atomicity_under_concurrent_writers() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let created = cache
        .put_if_absent(PutRequest {
            key: "k3",
            value: b"base",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put_if_absent")
        .expect("absent");

    let a = {
        let cache = cache.clone();
        tokio::spawn(async move {
            cache
                .compare_and_swap("k3", created.version, b"from-a", Ttl::Indefinite)
                .await
        })
    };
    let b = {
        let cache = cache.clone();
        tokio::spawn(async move {
            cache
                .compare_and_swap("k3", created.version, b"from-b", Ttl::Indefinite)
                .await
        })
    };

    let (result_a, result_b) = (a.await.unwrap(), b.await.unwrap());
    let outcomes = [result_a.is_ok(), result_b.is_ok()];
    assert_eq!(
        outcomes.iter().filter(|ok| **ok).count(),
        1,
        "PG-CACHE-003: exactly one concurrent CAS writer must win, got {outcomes:?}"
    );
    let loser = if result_a.is_ok() { result_b } else { result_a };
    assert!(
        matches!(loser, Err(ClusterError::CasConflict { .. })),
        "PG-CACHE-003: the losing writer must observe CasConflict, got {loser:?}"
    );

    handle.stop().await;
}

/// `PG-CACHE-004`: an entry past its TTL is absent after the reaper runs, and
/// an active watcher observes `Expired`.
#[tokio::test]
async fn pg_cache_004_ttl_expiry_via_reaper() {
    let (_container, config) =
        common::start_postgres_with(json!({ "cache_reaper_interval_ms": 100 })).await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let mut watch = cache.watch("k4").await.expect("watch succeeds");
    cache
        .put(PutRequest {
            key: "k4",
            value: b"soon-gone",
            ttl: Ttl::Of(Duration::from_millis(50)),
        })
        .await
        .expect("put succeeds");

    // The `put` itself round-trips a `Changed` event through this same
    // watch (NOTIFY self-delivery, DESIGN.md §4.3) before the reaper ever
    // runs — drain it first so the assertion below is actually checking the
    // *second* event, not racing the first one.
    let changed = tokio::time::timeout(Duration::from_secs(2), watch.recv())
        .await
        .expect("the put's own Changed event arrives before the timeout");
    assert!(
        matches!(
            changed,
            Some(cluster_sdk::CacheWatchEvent::Event(cluster_sdk::CacheEvent::Changed { ref key }))
                if key == "k4"
        ),
        "PG-CACHE-004 setup: expected the put's own Changed event first, got {changed:?}"
    );

    let expired = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async { cache.get("k4").await.expect("get succeeds").is_none() },
    )
    .await;
    assert!(
        expired,
        "PG-CACHE-004: entry must be gone once the reaper runs past expiry"
    );

    let event = tokio::time::timeout(Duration::from_secs(2), watch.recv())
        .await
        .expect("an event arrives before the timeout");
    assert!(
        matches!(
            event,
            Some(cluster_sdk::CacheWatchEvent::Event(cluster_sdk::CacheEvent::Expired { ref key }))
                if key == "k4"
        ),
        "PG-CACHE-004: watcher must receive Expired{{key: \"k4\"}}, got {event:?}"
    );

    handle.stop().await;
}

/// `PG-CACHE-005`: `compare_and_delete` survives the delete+recreate
/// version-reset scenario — a stale `compare_and_delete` against the old
/// value is a safe no-op against a successor's claim.
#[tokio::test]
async fn pg_cache_005_compare_and_delete_survives_version_reset() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    cache
        .put(PutRequest {
            key: "k5",
            value: b"holder-a",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("initial claim");
    assert!(cache.delete("k5").await.expect("delete succeeds"));
    cache
        .put(PutRequest {
            key: "k5",
            value: b"holder-b",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("successor re-claims");

    // Holder A's stale compare_and_delete against its own old value must be a
    // no-op, never wiping holder B's claim.
    let stale_delete = cache
        .compare_and_delete("k5", b"holder-a")
        .await
        .expect("compare_and_delete succeeds");
    assert!(
        !stale_delete,
        "PG-CACHE-005: stale compare_and_delete must be a no-op"
    );

    let entry = cache.get("k5").await.expect("get").expect("still present");
    assert_eq!(
        entry.value, b"holder-b",
        "PG-CACHE-005: successor's claim must survive"
    );

    handle.stop().await;
}

/// `PG-CACHE-006`: `scan_prefix` returns every key under the prefix,
/// excludes keys outside it and expired keys, and correctly escapes a
/// literal `%`/`_` in the prefix (`cache::escape_like`).
#[tokio::test]
async fn pg_cache_006_scan_prefix_correctness() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    for key in ["svc/a", "svc/b", "other/c"] {
        cache
            .put(PutRequest {
                key,
                value: b"v",
                ttl: Ttl::Indefinite,
            })
            .await
            .expect("put succeeds");
    }
    cache
        .put(PutRequest {
            key: "svc/expired",
            value: b"v",
            ttl: Ttl::Of(Duration::from_millis(1)),
        })
        .await
        .expect("put succeeds");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut found = cache
        .scan_prefix("svc/")
        .await
        .expect("scan_prefix succeeds");
    found.sort();
    assert_eq!(
        found,
        vec!["svc/a".to_owned(), "svc/b".to_owned()],
        "PG-CACHE-006: scan_prefix must include only non-expired keys under the prefix"
    );

    // A prefix containing a literal LIKE metacharacter must be matched
    // literally, not as a wildcard (`cache::escape_like`).
    cache
        .put(PutRequest {
            key: "100%-done",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put succeeds");
    cache
        .put(PutRequest {
            key: "100X-done",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put succeeds");
    let percent_scan = cache
        .scan_prefix("100%")
        .await
        .expect("scan_prefix succeeds");
    assert_eq!(
        percent_scan,
        vec!["100%-done".to_owned()],
        "PG-CACHE-006: a literal '%' in the prefix must be escaped, not treated as a wildcard"
    );

    handle.stop().await;
}

/// `PG-CACHE-007`: `build_and_start` against a database that already has the
/// plugin's tables succeeds without error (migration idempotency).
#[tokio::test]
async fn pg_cache_007_migration_idempotency() {
    let (_container, config) = common::start_postgres().await;
    let handle_one = PostgresClusterPlugin::builder(config.clone())
        .build_and_start()
        .await
        .expect("first build_and_start succeeds");
    handle_one.stop().await;

    let handle_two = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .expect("PG-CACHE-007: build_and_start against an already-migrated database must succeed");
    handle_two.stop().await;
}

/// `PG-CACHE-008`: with the write pool constrained to a single connection and
/// that one connection pinned by a held lock (the combined plugin shares one
/// pool across cache and lock — DESIGN.md §3.3), a concurrent cache `put`
/// *genuinely blocks* waiting for a connection, then completes once the lock
/// releases the connection back to the pool. Pinning via a real held lock —
/// rather than firing N short puts that merely serialize in microseconds —
/// is what actually forces the "caller waits for a connection" behaviour this
/// scenario is about.
#[tokio::test]
async fn pg_cache_008_blocked_acquire_succeeds_once_connection_frees() {
    let (_container, config) = common::start_postgres_with(json!({
        "pool_max_size": 1,
        "pool_acquire_timeout_ms": 5_000,
    }))
    .await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();
    let lock = handle.lock();

    // Pin the single pool connection for the held lock's duration (§3.3).
    let guard = lock
        .try_lock("cache8-blocker", Duration::from_secs(30))
        .await
        .expect("acquiring the lock pins the only pool connection");

    // A cache `put` now has no connection available and must block.
    let put_cache = cache.clone();
    let put = tokio::spawn(async move {
        put_cache
            .put(PutRequest {
                key: "k8",
                value: b"v",
                ttl: Ttl::Indefinite,
            })
            .await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !put.is_finished(),
        "PG-CACHE-008: the put must genuinely block while the only connection is pinned"
    );

    // Releasing the lock returns the connection; the blocked put then completes.
    guard.release().await.expect("release frees the connection");
    let result = tokio::time::timeout(Duration::from_secs(5), put)
        .await
        .expect("the unblocked put resolves within the timeout")
        .expect("put task must not panic");
    assert!(
        result.is_ok(),
        "PG-CACHE-008: the queued put must succeed once a connection frees, got {result:?}"
    );

    handle.stop().await;
}

/// `PG-CACHE-008b`: pool exhaustion that outlasts `pool_acquire_timeout_ms`
/// surfaces as `Provider { kind: Timeout }` (the timeout-exceeded error path
/// the happy-path scenario above does not cover). Same single-connection,
/// lock-pinned setup, but a short acquire timeout the blocked op cannot beat.
#[tokio::test]
async fn pg_cache_008b_pool_exhaustion_times_out_as_provider_timeout() {
    let (_container, config) = common::start_postgres_with(json!({
        "pool_max_size": 1,
        "pool_acquire_timeout_ms": 250,
    }))
    .await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();
    let lock = handle.lock();

    let guard = lock
        .try_lock("cache8b-blocker", Duration::from_secs(30))
        .await
        .expect("acquiring the lock pins the only pool connection");

    // No connection can be obtained within 250ms → sqlx `PoolTimedOut` →
    // `Provider { Timeout }` (DESIGN.md §9).
    let result = cache
        .put(PutRequest {
            key: "k8b",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(ClusterError::Provider {
                kind: ProviderErrorKind::Timeout,
                ..
            })
        ),
        "PG-CACHE-008b: a cache op that cannot get a pool connection within \
         pool_acquire_timeout_ms must return Provider{{Timeout}}, got {result:?}"
    );

    guard.release().await.expect("release");
    handle.stop().await;
}

/// `PG-CACHE-009`: `put_if_absent` over an expired-but-not-yet-reaped row
/// treats the key as absent and re-creates it at version 1, rather than
/// reporting it present until the TTL reaper physically deletes the row. This
/// is the leader-election failover path (`CasBasedLeaderElectionBackend::claim`
/// re-claims an expired election lease via `put_if_absent`): a slow reaper must
/// not delay failover. The reaper interval is left at its 10s default and the
/// TTL is 50ms, so the row is guaranteed still-present-but-expired when the
/// second `put_if_absent` runs (regression test for the `ON CONFLICT DO
/// NOTHING` bug — DESIGN.md §4.1).
#[tokio::test]
async fn pg_cache_009_put_if_absent_reclaims_expired_row() {
    let (_container, config) = common::start_postgres().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let first = cache
        .put_if_absent(PutRequest {
            key: "k9",
            value: b"incumbent",
            ttl: Ttl::Of(Duration::from_millis(50)),
        })
        .await
        .expect("put_if_absent succeeds")
        .expect("key was absent");
    assert_eq!(first.version, 1, "PG-CACHE-009: first claim is version 1");

    // Wait past the TTL. The default 10s reaper cannot have run yet, so the
    // expired row physically lingers — the exact state the bug mishandled.
    let expired = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(25),
        || async { cache.get("k9").await.expect("get succeeds").is_none() },
    )
    .await;
    assert!(
        expired,
        "PG-CACHE-009: entry must read as absent once past its TTL"
    );

    // The row is still physically present (reaper hasn't swept it): confirm
    // the scenario is actually exercising the expired-but-unreaped window.
    let pool = common::raw_pool(&connection_string).await;
    let still_present: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM public.cluster_cache WHERE key = $1)")
            .bind("k9")
            .fetch_one(&pool)
            .await
            .expect("existence query succeeds");
    assert!(
        still_present,
        "PG-CACHE-009: the expired row must still physically exist (reaper has not run)"
    );

    let reclaimed = cache
        .put_if_absent(PutRequest {
            key: "k9",
            value: b"successor",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put_if_absent succeeds")
        .expect("PG-CACHE-009: put_if_absent must treat an expired row as absent and re-create it");
    assert_eq!(
        reclaimed.version, 1,
        "PG-CACHE-009: reclaimed entry is a fresh version 1"
    );

    let entry = cache.get("k9").await.expect("get").expect("present");
    assert_eq!(
        entry.value, b"successor",
        "PG-CACHE-009: successor's value must win"
    );

    handle.stop().await;
}

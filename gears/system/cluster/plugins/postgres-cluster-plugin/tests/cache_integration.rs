//! Layer 3 — cache integration scenarios (docs/TESTING.md §4.2, `PG-CACHE-001..010`).

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
/// that one connection checked out, a concurrent cache `put` *genuinely blocks*
/// waiting for a connection, then completes once it is returned. Holding a real
/// checked-out connection — rather than firing N short puts that merely serialize
/// in microseconds — is what actually forces the "caller waits for a connection"
/// behaviour this scenario is about.
///
/// The connection is held via the `__test_pool` seam. An earlier version pinned
/// the pool by *holding a lock*, which only worked because a held lock owned a
/// pool connection; a held lock is now a `cluster_lock` row holding no connection
/// at all (DESIGN.md §3.3), so it cannot exhaust the pool. Exercising pool
/// exhaustion directly is also the better test — it no longer depends on the lock
/// primitive to set up a cache-side condition.
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

    // Check the single pool connection out and hold it.
    let pinned = handle
        .__test_pool()
        .acquire()
        .await
        .expect("checking out the only pool connection succeeds");

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
        "PG-CACHE-008: the put must genuinely block while the only connection is checked out"
    );

    // Returning the connection unblocks the queued put.
    drop(pinned);
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
/// surfaces as `Provider { kind: Timeout }` (the timeout-exceeded error path the
/// happy-path scenario above does not cover). Same single-connection,
/// checked-out setup, but a short acquire timeout the blocked op cannot beat.
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

    let pinned = handle
        .__test_pool()
        .acquire()
        .await
        .expect("checking out the only pool connection succeeds");

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

    drop(pinned);
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

/// Rows planted for `PG-CACHE-010`/`010b`, deliberately more than one
/// `SWEEP_BATCH` (512, `cache::reaper`), so clearing them cannot be done by a
/// single chunk.
const BACKLOG_ROWS: i32 = 600;

/// Plants `BACKLOG_ROWS` already-expired rows in one statement.
///
/// Planted directly rather than through `put`: this is about the reaper's
/// behaviour on a pre-existing backlog, and `BACKLOG_ROWS` round-trips through
/// the API would both be slow and let a fast reaper nibble the early rows away
/// while the later ones were still being written. One statement commits
/// atomically, so the reaper sees the whole backlog or none of it.
///
/// `expires_at = now() - (BACKLOG_ROWS + 1 - i)s` staggers the rows already into
/// the past, longest-expired first, so under the sweep's `ORDER BY expires_at`
/// `backlog-1` heads the first chunk and `backlog-<BACKLOG_ROWS>` is last in
/// line — which is what lets `010b` name a key that can only fall in a later
/// chunk.
async fn plant_expired_backlog(pool: &sqlx::PgPool) {
    sqlx::query(
        "INSERT INTO public.cluster_cache (key, value, expires_at) \
         SELECT 'backlog-' || i, '\\x2a'::bytea, now() - (($1 + 1 - i) * interval '1 second') \
         FROM generate_series(1, $1) AS s(i)",
    )
    .bind(BACKLOG_ROWS)
    .execute(pool)
    .await
    .expect("planting the expired backlog succeeds");
}

/// `PG-CACHE-010`: **one** sweep clears an expired backlog larger than a single
/// chunk, and deletes only expired rows.
///
/// Driven through the `__test_sweep_once` seam rather than by waiting on the
/// reaper's own timer, for the same reason `PG-SPEC-009` does it for the lock
/// reaper: what's under test is that a *single* sweep loops until the table is
/// drained, and reaper-interval timing cannot distinguish that from "several
/// intervals elapsed" — the latter being exactly the 512-rows-per-tick behaviour
/// the chunking exists to avoid. `cache_reaper_interval_ms` is set long so the
/// reaper's own sweeps never race these assertions.
#[tokio::test]
async fn pg_cache_010_one_sweep_clears_a_multi_chunk_backlog() {
    let (_container, config) =
        common::start_postgres_with(json!({ "cache_reaper_interval_ms": 600_000 })).await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    // The seam lives on the concrete backend, as in PG-SPEC-005/009.
    let cache = handle.__test_cache();
    let pool = common::raw_pool(&connection_string).await;

    plant_expired_backlog(&pool).await;
    // The plugin's own reaper ticks once immediately at startup (before the rows
    // above exist) and then not again for ten minutes, so it cannot be the thing
    // that drains this backlog. Asserted rather than assumed: were that ordering
    // ever to change, the sweep below would return a short count and read as a
    // chunk-loop bug instead of the setup race it actually is.
    let planted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM public.cluster_cache WHERE key LIKE 'backlog-%'")
            .fetch_one(&pool)
            .await
            .expect("count query succeeds");
    assert_eq!(
        planted,
        i64::from(BACKLOG_ROWS),
        "PG-CACHE-010 setup: the backlog must be intact before the sweep under test runs"
    );

    // Two rows the sweep must leave alone: one with a live TTL, one indefinite.
    handle
        .cache()
        .put(PutRequest {
            key: "backlog-live",
            value: b"live",
            ttl: Ttl::Of(Duration::from_mins(10)),
        })
        .await
        .expect("put succeeds");
    handle
        .cache()
        .put(PutRequest {
            key: "backlog-forever",
            value: b"forever",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put succeeds");

    let swept = cache.__test_sweep_once().await.expect("sweep");
    assert_eq!(
        swept,
        usize::try_from(BACKLOG_ROWS).unwrap(),
        "PG-CACHE-010: a single sweep must keep chunking until the whole expired backlog is \
         gone, not stop after one bounded DELETE"
    );

    let mut remaining: Vec<String> =
        sqlx::query_scalar("SELECT key FROM public.cluster_cache WHERE key LIKE 'backlog-%'")
            .fetch_all(&pool)
            .await
            .expect("listing remaining rows succeeds");
    remaining.sort();
    assert_eq!(
        remaining,
        vec!["backlog-forever".to_owned(), "backlog-live".to_owned()],
        "PG-CACHE-010: the sweep must delete every expired row and only those: a live TTL and \
         an indefinite entry both survive"
    );

    // A second sweep over a table with nothing expired left is a no-op, so the
    // loop's short-chunk exit is what ended the first one, not the row supply.
    assert_eq!(
        cache.__test_sweep_once().await.expect("sweep"),
        0,
        "PG-CACHE-010: a sweep with nothing expired must reap nothing"
    );

    handle.stop().await;
}

/// `PG-CACHE-010b`: keys reaped by a chunk *after* the first still get their
/// `Expired` event.
///
/// Per-chunk transactions mean the `NOTIFY`s are committed in several batches
/// rather than one, so this pins that no batch past the first is dropped —
/// `PG-CACHE-004` only ever covers a single-row sweep. The watched key is the
/// least-expired row of the backlog, so `ORDER BY expires_at` guarantees it is
/// not in the first chunk. A fast reaper here (rather than `010`'s deliberately
/// long one) so the watcher can be registered before any sweep runs.
#[tokio::test]
async fn pg_cache_010b_later_chunks_still_notify_their_keys() {
    let (_container, config) =
        common::start_postgres_with(json!({ "cache_reaper_interval_ms": 100 })).await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();

    // Registered before the rows exist: a watcher only receives events from the
    // point it subscribes (DESIGN.md §4.3), and the reaper may sweep as soon as
    // the insert commits.
    let last_key = format!("backlog-{BACKLOG_ROWS}");
    let mut watch = handle
        .cache()
        .watch(&last_key)
        .await
        .expect("watch succeeds");

    let pool = common::raw_pool(&connection_string).await;
    plant_expired_backlog(&pool).await;

    let event = tokio::time::timeout(Duration::from_secs(30), watch.recv())
        .await
        .expect("an event arrives before the timeout");
    assert!(
        matches!(
            event,
            Some(cluster_sdk::CacheWatchEvent::Event(cluster_sdk::CacheEvent::Expired { ref key }))
                if *key == last_key
        ),
        "PG-CACHE-010b: a later chunk's keys must still get their Expired event, got {event:?}"
    );

    handle.stop().await;
}

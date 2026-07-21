//! Layer 3 — lock integration scenarios (docs/TESTING.md §4.3, `PG-LOCK-001..011`).

#![cfg(feature = "integration")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::error::ClusterError;
use postgres_cluster_plugin::PostgresLockPlugin;
use serde_json::json;

/// `PG-LOCK-001`: `try_lock` acquires and holds the advisory lock; a second
/// `try_lock` returns `LockContended`; after `release`, it succeeds again.
#[tokio::test]
async fn pg_lock_001_try_lock_acquires_and_release_frees() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let guard = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("first acquire");
    let contended = lock.try_lock("res", Duration::from_secs(30)).await;
    assert!(
        matches!(contended, Err(ClusterError::LockContended { .. })),
        "PG-LOCK-001: a held lock must contend, got {contended:?}"
    );
    guard.release().await.expect("release succeeds");
    let reacquired = lock.try_lock("res", Duration::from_secs(30)).await;
    assert!(
        reacquired.is_ok(),
        "PG-LOCK-001: lock must be free again after release"
    );

    handle.stop().await;
}

/// `PG-LOCK-002`: a blocked `lock()` returns `LockTimeout` once its timeout
/// elapses, and the advisory lock is not left held by the timed-out waiter.
#[tokio::test]
async fn pg_lock_002_lock_with_timeout() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let holder = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("holder acquires");
    let started = tokio::time::Instant::now();
    let timed_out = lock
        .lock("res", Duration::from_secs(30), Duration::from_millis(200))
        .await;
    assert!(
        matches!(timed_out, Err(ClusterError::LockTimeout { .. })),
        "PG-LOCK-002: expected LockTimeout, got {timed_out:?}"
    );
    assert!(started.elapsed() >= Duration::from_millis(200));

    holder.release().await.expect("holder releases");
    let now_free = lock.try_lock("res", Duration::from_secs(30)).await;
    assert!(
        now_free.is_ok(),
        "PG-LOCK-002: the timed-out waiter must not have left the advisory lock held"
    );

    handle.stop().await;
}

/// `PG-LOCK-003`: a blocked `lock()` wakes promptly (well under the
/// heartbeat fallback's 250ms) after the holder calls `release`, confirming
/// the NOTIFY-driven wake path (not just the heartbeat).
#[tokio::test]
async fn pg_lock_003_lock_wakes_on_release_notify() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let guard = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("holder acquires");

    let waiter_lock = Arc::clone(&lock);
    let waiter = tokio::spawn(async move {
        let started = tokio::time::Instant::now();
        let result = waiter_lock
            .lock("res", Duration::from_secs(30), Duration::from_secs(5))
            .await;
        (started.elapsed(), result)
    });

    // Synchronize on the waiter having actually registered as a release-NOTIFY
    // waiter (i.e. its first `try_acquire` contended and it parked in `wait_for`)
    // before releasing — otherwise `!is_finished()` alone only proves the task
    // has not returned, so an unscheduled task could acquire immediately after
    // release and satisfy the latency assertion without ever exercising the
    // NOTIFY wake path (PGR-E3).
    let pg_lock = handle.__test_lock();
    let registered = common::wait_until(Duration::from_secs(5), Duration::from_millis(5), || {
        let pg_lock = std::sync::Arc::clone(&pg_lock);
        async move { pg_lock.__test_release_waiter_count("res") > 0 }
    })
    .await;
    assert!(
        registered,
        "setup: waiter must register as a release-NOTIFY waiter before release"
    );
    assert!(!waiter.is_finished(), "setup: waiter must still be blocked");

    guard.release().await.expect("release succeeds");
    let (elapsed, result) = waiter.await.expect("waiter task must not panic");
    assert!(
        result.is_ok(),
        "PG-LOCK-003: waiter must acquire after release, got {result:?}"
    );
    // A NOTIFY-driven wake must land comfortably below the 250ms heartbeat
    // fallback, with enough margin (50ms) that a wake at ~one heartbeat cannot
    // masquerade as a NOTIFY wake — the previous 230ms bound sat so close to
    // 250ms it barely distinguished the two (PGR-L5). Still not the DESIGN §5.3
    // "well under 100ms" ideal, to leave headroom for container/CI scheduling
    // jitter, but tight enough that only the NOTIFY path can satisfy it.
    assert!(
        elapsed < Duration::from_millis(200),
        "PG-LOCK-003: wake latency {elapsed:?} is too close to the 250ms heartbeat fallback; \
         the NOTIFY-driven wake should be well under it"
    );

    handle.stop().await;
}

/// `PG-LOCK-004`: once the TTL reaper sweeps an expired lock, the advisory
/// lock is released and a subsequent `try_lock` succeeds.
#[tokio::test]
async fn pg_lock_004_ttl_reaper_releases_expired_lock() {
    let (_container, config) =
        common::start_postgres_lock_only_with(json!({ "lock_reaper_interval_ms": 100 })).await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let guard = lock
        .try_lock("res", Duration::from_millis(150))
        .await
        .expect("acquire");
    // Deliberately never released — simulates a crashed holder; the TTL
    // reaper is the only thing that can free this.
    std::mem::forget(guard);

    let reacquired = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async { lock.try_lock("res", Duration::from_secs(30)).await.is_ok() },
    )
    .await;
    assert!(
        reacquired,
        "PG-LOCK-004: reaper must release the expired lock"
    );

    handle.stop().await;
}

/// `PG-LOCK-005`: `renew` resets the TTL clock; the reaper does not release
/// the lock while renewals keep it alive.
#[tokio::test]
async fn pg_lock_005_renew_extends_ttl() {
    let (_container, config) =
        common::start_postgres_lock_only_with(json!({ "lock_reaper_interval_ms": 250 })).await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    // Deliberately generous margins: this runs under real wall-clock time (a
    // real `sqlx` pool can't use a paused clock — see `conformance.rs`), so the
    // renew interval must sit well under the TTL even when `sleep` overshoots by
    // tens of ms under CPU contention in a full parallel run. Renew every 300ms
    // against a 1000ms TTL (700ms slack); 4 renews span ~1.2s, comfortably past
    // the original 1000ms the lock would have survived unrenewed — which is the
    // property under test.
    let guard = lock
        .try_lock("res", Duration::from_secs(1))
        .await
        .expect("acquire");
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        guard
            .renew(Duration::from_secs(1))
            .await
            .expect("PG-LOCK-005: renew before expiry must succeed");
    }
    let still_contended = lock.try_lock("res", Duration::from_secs(30)).await;
    assert!(
        matches!(still_contended, Err(ClusterError::LockContended { .. })),
        "PG-LOCK-005: renewed lock must still be held, got {still_contended:?}"
    );

    guard.release().await.expect("release succeeds");
    handle.stop().await;
}

/// `PG-LOCK-006`: once the reaper has actually reclaimed an expired lock,
/// `renew` on the stale guard returns `LockExpired`.
#[tokio::test]
async fn pg_lock_006_lock_expired_on_renew_past_ttl() {
    let (_container, config) =
        common::start_postgres_lock_only_with(json!({ "lock_reaper_interval_ms": 100 })).await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let guard = lock
        .try_lock("res", Duration::from_millis(150))
        .await
        .expect("acquire");
    // Wait past both the TTL and at least one reaper sweep so the row is
    // actually reclaimed, not merely virtually past its TTL.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let err = guard
        .renew(Duration::from_millis(200))
        .await
        .expect_err("PG-LOCK-006: renewing a reaper-reclaimed lock must fail");
    assert!(
        matches!(err, ClusterError::LockExpired { .. }),
        "PG-LOCK-006: expected LockExpired, got {err:?}"
    );

    handle.stop().await;
}

/// `PG-LOCK-007`: forcibly closing the Postgres backend holding a lock's
/// pinned connection (`pg_terminate_backend`) auto-releases the advisory
/// lock; a subsequent `try_lock` succeeds.
///
/// The advisory lock's auto-release on session disconnect and the
/// re-acquisition afterward both work: `try_acquire`'s `ON CONFLICT (name) DO
/// UPDATE` (justified inline in `lock/mod.rs`) overwrites the stale
/// `cluster_lock` row a `pg_terminate_backend`-killed holder left behind.
///
/// This previously hung — root cause found and fixed: the hang was not in the
/// re-acquire (which succeeds) but in `handle.stop()` → `pool.close()`. The
/// re-acquired lock's connection is pinned in `PostgresLock`'s `held` map and,
/// because this test `std::mem::forget`s the guard, is never returned to the
/// pool; `sqlx::Pool::close()` blocks until every checked-out connection is
/// returned, so it hung forever. `stop()` now drains `held` first (see
/// `PostgresLock::drain_held` / `PG-LIFE-003`), so it completes cleanly.
#[tokio::test]
async fn pg_lock_007_connection_drop_releases_advisory_lock() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let guard = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("acquire");

    let control_pool = common::raw_pool(&connection_string).await;
    let pid: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_locks WHERE locktype = 'advisory' AND granted = true LIMIT 1",
    )
    .fetch_one(&control_pool)
    .await
    .expect("exactly one advisory lock is held by this test");
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&control_pool)
        .await
        .expect("pg_terminate_backend succeeds");
    assert!(
        terminated,
        "PG-LOCK-007: pg_terminate_backend must report success"
    );

    let released = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async { lock.try_lock("res", Duration::from_secs(30)).await.is_ok() },
    )
    .await;
    assert!(
        released,
        "PG-LOCK-007: advisory lock must auto-release on session disconnect"
    );

    // The guard's own background task has lost its connection; drop it
    // without calling `release()` (there is nothing left to release).
    std::mem::forget(guard);
    handle.stop().await;
}

/// `PG-LOCK-008`: of 20 concurrent `try_lock` callers on the same name,
/// exactly one succeeds and every other returns `LockContended`.
#[tokio::test]
async fn pg_lock_008_concurrent_lockers_at_most_one_holder() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let mut tasks = Vec::new();
    for _ in 0..20 {
        let lock = Arc::clone(&lock);
        tasks.push(tokio::spawn(async move {
            lock.try_lock("shared", Duration::from_secs(30)).await
        }));
    }
    let mut successes = 0;
    for task in tasks {
        if task.await.unwrap().is_ok() {
            successes += 1;
        }
    }
    assert_eq!(
        successes, 1,
        "PG-LOCK-008: exactly one of 20 concurrent try_lock callers must win"
    );

    handle.stop().await;
}

/// `PG-LOCK-009`: `synchronous_commit` is enforced even when the database's
/// own default is `off`. The precondition (fresh sessions really do inherit
/// `off`) is confirmed directly, and — via the `__test_synchronous_commit`
/// seam (the same one `pg_spec_005` uses to read a pinned lock connection's
/// GUC) — the *distinguishing* observable is asserted: the held lock's own
/// pinned connection reports `on`. Asserting the GUC directly (rather than
/// only that acquire/release succeeds, which they would even if enforcement
/// no-oped) is what actually exercises the `after_connect`/`before_acquire`
/// enforcement (DESIGN.md §3.4).
#[tokio::test]
async fn pg_lock_009_synchronous_commit_enforced_over_off_default() {
    use postgres_cluster_plugin::PostgresLock;

    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();

    let control_pool = common::raw_pool(&connection_string).await;
    sqlx::query("ALTER DATABASE cluster_test SET synchronous_commit = off")
        .execute(&control_pool)
        .await
        .expect("can set the database-level default");
    // A brand-new session (this plugin hasn't connected yet) must pick up
    // the database default, confirming the precondition this scenario is
    // about actually holds.
    let fresh_pool = common::raw_pool(&connection_string).await;
    let default_setting: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(&fresh_pool)
        .await
        .expect("SHOW succeeds");
    assert_eq!(
        default_setting, "off",
        "setup: database default must actually be off"
    );

    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .expect("PG-LOCK-009: build_and_start must succeed despite the off database default");
    let lock = handle.lock();
    let concrete: Arc<PostgresLock> = handle.__test_lock();
    let guard = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("PG-LOCK-009: lock acquire must succeed under enforced synchronous_commit");

    // The held lock's pinned connection must report `on`, proving the
    // `before_acquire`/`after_connect` hooks actually overrode the `off`
    // database default (not merely that the ops didn't error).
    assert_eq!(
        concrete.__test_synchronous_commit("res").await.as_deref(),
        Some("on"),
        "PG-LOCK-009: the pinned lock connection must be synchronous_commit=on despite the \
         off database default; enforcement must override it, not no-op"
    );

    guard.release().await.expect("release succeeds");

    handle.stop().await;
}

/// `PG-LOCK-010`: the standalone `PostgresLockPlugin` migrates only
/// `cluster_lock` — never `cluster_cache` — and its `try_lock`/`release`
/// behave identically to the combined plugin's lock.
#[tokio::test]
async fn pg_lock_010_standalone_plugin_creates_only_lock_table() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .expect("standalone plugin starts");

    let pool = common::raw_pool(&connection_string).await;
    assert!(
        common::table_exists(&pool, "public", "cluster_lock").await,
        "PG-LOCK-010: cluster_lock must exist"
    );
    assert!(
        !common::table_exists(&pool, "public", "cluster_cache").await,
        "PG-LOCK-010: a lock-only deployment must never create cluster_cache"
    );

    let lock = handle.lock();
    let guard = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("try_lock works");
    let contended = lock.try_lock("res", Duration::from_secs(30)).await;
    assert!(matches!(contended, Err(ClusterError::LockContended { .. })));
    guard.release().await.expect("release works");

    handle.stop().await;
}

/// `PG-LOCK-011`: end-to-end YAML routing — `lock: { provider: postgres }`
/// resolves to real advisory locks in the test container while
/// `cache: { provider: standalone }` in the same profile is the in-process
/// backend, confirming `ClusterLockProvider` registration makes
/// `provider: postgres` independently resolvable for the `lock` primitive
/// (DESIGN.md §3.5) via the wiring crate's per-primitive routing, not merely
/// callable directly off this plugin's own builder.
#[tokio::test]
async fn pg_lock_011_end_to_end_yaml_routing_lock_postgres_cache_standalone() {
    use cluster::{ClusterConfig, ClusterWiring, ProviderRegistry};
    use cluster_sdk::lock::DistributedLockV1;
    use cluster_sdk::profile::ClusterProfile;
    use postgres_cluster_plugin::PostgresLockProvider;
    use standalone_cluster_plugin::StandaloneCacheProvider;
    use toolkit::client_hub::ClientHub;

    #[derive(Clone, Copy)]
    struct RoutingProfile;
    impl ClusterProfile for RoutingProfile {
        const NAME: &'static str = "pglockrouting";
    }

    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();

    // Operator config is normally YAML (`serde-saphyr`), but `ClusterConfig`
    // is a plain `serde::Deserialize` type — building it from an equivalent
    // JSON value exercises the exact same `BackendBinding`/per-provider
    // `options` flattening (DESIGN.md §3.5's "Design A") without adding a
    // YAML-parsing dev-dependency just for this one test.
    let mut profiles = serde_json::Map::new();
    profiles.insert(
        RoutingProfile::NAME.to_owned(),
        serde_json::json!({
            "cache": { "provider": "standalone" },
            "lock": {
                "provider": "postgres",
                "connection_string": connection_string,
                "pool_max_size": 5,
            },
        }),
    );
    let cluster_config: ClusterConfig =
        serde_json::from_value(serde_json::json!({ "profiles": profiles }))
            .expect("routing profile config parses");

    let providers = ProviderRegistry::new()
        .with_cache_provider(Arc::new(StandaloneCacheProvider))
        .with_lock_provider(Arc::new(PostgresLockProvider));
    let hub = Arc::new(ClientHub::new());
    let handle = ClusterWiring::from_config(Arc::clone(&hub), &cluster_config, &providers)
        .await
        .expect(
            "PG-LOCK-011: wiring must resolve lock: postgres independently of cache: standalone",
        );

    let lock = DistributedLockV1::resolver(&hub)
        .profile(RoutingProfile)
        .resolve()
        .expect("lock facade resolves for the routing profile");

    let guard = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("try_lock succeeds through the resolved facade");

    // Confirm this is a *real* Postgres advisory lock, not the standalone
    // in-process lock — a second, direct connection must see it held.
    let control_pool = common::raw_pool(&connection_string).await;
    let advisory_locks_held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND granted = true",
    )
    .fetch_one(&control_pool)
    .await
    .expect("pg_locks query succeeds");
    assert_eq!(
        advisory_locks_held, 1,
        "PG-LOCK-011: the resolved lock facade must be backed by a real Postgres advisory lock"
    );

    guard.release().await.expect("release succeeds");
    handle.stop().await;
}

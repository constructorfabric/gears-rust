//! Layer 3 — lock integration scenarios (docs/TESTING.md §4.3, `PG-LOCK-001..022`).

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

/// `PG-LOCK-007`: forcibly closing the Postgres backend that holds this
/// instance's **liveness beacon** (`pg_terminate_backend`) frees every lock the
/// instance held, without waiting out any TTL.
///
/// The mechanism this asserts is the whole reason the beacon exists. Postgres
/// releases the beacon's advisory lock the instant that connection dies, so the
/// acquire predicate's `NOT EXISTS (SELECT 1 FROM pg_locks ...)` branch turns
/// true for every row the dead incarnation wrote — and any acquirer, here or
/// elsewhere, takes those names on its own next attempt. Nothing has to notice
/// on the dead instance's behalf.
///
/// The TTL is 10 minutes precisely so a pass cannot be the TTL sweep doing its
/// ordinary job: reacquisition inside a few seconds can only come from the
/// liveness branch of the predicate.
#[tokio::test]
async fn pg_lock_007_beacon_loss_frees_every_lock_before_the_ttl() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let guard = lock
        .try_lock("res", Duration::from_mins(10))
        .await
        .expect("acquire");

    let control_pool = common::raw_pool(&connection_string).await;
    let pid = handle.__test_lock().__test_beacon_backend_pid();
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
        Duration::from_secs(15),
        Duration::from_millis(50),
        || async { lock.try_lock("res", Duration::from_secs(30)).await.is_ok() },
    )
    .await;
    assert!(
        released,
        "PG-LOCK-007: a lock whose holder's beacon died must become acquirable at once, not at \
         its 10-minute TTL"
    );

    // The original acquisition is gone; drop its guard without releasing (the
    // `holder_id` fence makes a release a no-op against the successor's row).
    std::mem::forget(guard);
    control_pool.close().await;
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

/// `PG-LOCK-009`: `synchronous_commit` is enforced even when the database's own
/// default is `off`. The precondition (fresh sessions really do inherit `off`) is
/// confirmed directly, and then the *distinguishing* observable is asserted: a
/// connection from the plugin's own pool reports `on`.
///
/// The pool is the whole surface for this now. Every statement a lock issues —
/// acquire, renew, release, the sweeps, the drain — is a pooled one, so the
/// `after_connect`/`before_acquire` hooks (DESIGN.md §3.4) cover all of it; the
/// beacon, the only long-lived connection left, writes nothing and needs no such
/// guarantee. Asserting the GUC directly, rather than only that acquire/release
/// succeed (which they would even if enforcement no-oped), is what actually
/// exercises the enforcement.
#[tokio::test]
async fn pg_lock_009_synchronous_commit_enforced_over_off_default() {
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
    let guard = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("PG-LOCK-009: lock acquire must succeed under enforced synchronous_commit");

    // A pooled connection must report `on`, proving enforcement actually overrode
    // the `off` database default (not merely that the ops didn't error).
    let pooled: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(&handle.__test_pool())
        .await
        .expect("SHOW on a pooled connection succeeds");
    assert_eq!(
        pooled, "on",
        "PG-LOCK-009: every pooled connection must be synchronous_commit=on despite the off \
         database default; enforcement must override it, not no-op"
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

    // Confirm this is a *real* Postgres lock, not the standalone in-process one —
    // a second, direct connection must see the lease row. Asserted against
    // `cluster_lock` rather than against a global `pg_locks` count: this instance
    // also holds its liveness beacon (DESIGN.md §5.1), so a bare count of advisory
    // locks no longer identifies the lock under test, and the row is the ownership
    // surface anyway.
    let control_pool = common::raw_pool(&connection_string).await;
    let held_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM cluster_lock WHERE name = 'res'")
        .fetch_one(&control_pool)
        .await
        .expect("cluster_lock query succeeds");
    assert_eq!(
        held_rows, 1,
        "PG-LOCK-011: the resolved lock facade must be backed by a real Postgres lock"
    );

    guard.release().await.expect("release succeeds");
    handle.stop().await;
}

/// `PG-LOCK-012`: the number of locks one instance can hold concurrently is
/// independent of `pool_max_size` — a held lock is a `cluster_lock` lease row
/// (DESIGN.md §3.3), not an advisory lock on a connection, so it consumes no pool
/// connection and takes no advisory lock beyond the instance's single beacon.
///
/// Direct regression test for the model this replaced. With `pool_max_size: 2`,
/// the previous one-pinned-connection-per-lock design could hold at most two
/// locks, and reaching that limit was worse than a stall: the TTL reaper needs a
/// *pool* connection to sweep, and only the holder's own session can unlock, so a
/// saturated pool starved the one task able to reclaim those locks and wedged
/// their names until shutdown. Here 12 locks are held at once over a
/// 2-connection pool, and the pool must still be serving `cluster_lock` writes
/// (`renew`) throughout.
#[tokio::test]
async fn pg_lock_012_held_locks_do_not_consume_pool_connections() {
    const HELD: usize = 12;

    let (_container, config) =
        common::start_postgres_lock_only_with(json!({ "pool_max_size": 2 })).await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let mut guards = Vec::with_capacity(HELD);
    for index in 0..HELD {
        let name = format!("res{index}");
        guards.push(
            lock.try_lock(&name, Duration::from_secs(30))
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "PG-LOCK-012: holding {HELD} locks over a 2-connection pool must not \
                         exhaust it; lock {name} failed with {err:?}"
                    )
                }),
        );
    }

    let control_pool = common::raw_pool(&connection_string).await;
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM cluster_lock")
        .fetch_one(&control_pool)
        .await
        .expect("count query succeeds");
    assert_eq!(
        usize::try_from(rows).expect("a row count fits in usize"),
        HELD,
        "PG-LOCK-012: all {HELD} locks must be genuinely held, not silently coalesced"
    );

    // And the fleet-wide advisory-lock population is *one* — this instance's
    // beacon — however many locks are held. That is the invariant the whole model
    // rests on (nothing per-lock is ever locked, which is why
    // `pg_advisory_unlock` is never called anywhere in the plugin), so assert it
    // directly rather than inferring it from the pool not being exhausted.
    let beacon_pid = handle.__test_lock().__test_beacon_backend_pid();
    let advisory_holders: Vec<i32> = sqlx::query_scalar(
        "SELECT DISTINCT pid FROM pg_locks WHERE locktype = 'advisory' AND granted = true",
    )
    .fetch_all(&control_pool)
    .await
    .expect("pg_locks query succeeds");
    assert_eq!(
        advisory_holders,
        vec![beacon_pid],
        "PG-LOCK-012: holding {HELD} locks must take no advisory lock beyond the instance's own \
         beacon"
    );

    // The pool is still usable while all 12 locks are held — `renew` writes to
    // `cluster_lock` through it, which is what the old model could not do here.
    guards[0]
        .renew(Duration::from_secs(30))
        .await
        .expect("PG-LOCK-012: a metadata write must still get a pool connection");

    for guard in guards {
        guard.release().await.expect("release succeeds");
    }
    handle.stop().await;
}

/// `PG-LOCK-013`: losing the beacon invalidates **every** lock this instance
/// holds at once — including one whose consumer is sitting quietly in its
/// critical section and never touches it again.
///
/// One beacon per instance means one blast radius, deliberately (DESIGN.md §5.1):
/// a connection blip has one predictable outcome rather than a per-lock recovery
/// path to reason about.
///
/// Three assertions, each covering a different way this could go wrong:
///
/// * **`renew` → `LockExpired`.** The rows are untouched by the beacon's death,
///   so an implementation missing the `holder_beacon_*` fence would find the row,
///   match the `holder_id`, and report a *successful* renewal for a lock every
///   other instance can already steal — silently losing mutual exclusion rather
///   than failing.
/// * **`local_holders` empties without being asked.** The quiet holder generates
///   no traffic at all, so nothing would purge it lazily. This is what the
///   once-per-second ping is for.
/// * **The negative**: a lock is not silently *retained* across the loss just
///   because nobody else happens to want it. Without local detection the
///   semantics would quietly weaken to "you remain the holder until someone
///   takes it from you", which is precisely what the ping exists to prevent.
#[tokio::test]
async fn pg_lock_013_beacon_loss_invalidates_every_held_lock() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();
    let concrete = handle.__test_lock();

    let first = lock
        .try_lock("res", Duration::from_mins(10))
        .await
        .expect("first acquire");
    let second = lock
        .try_lock("res2", Duration::from_mins(10))
        .await
        .expect("second acquire");
    assert_eq!(
        concrete.__test_local_holder_count(),
        2,
        "setup: both locks must be registered locally"
    );
    let epoch_before = concrete.__test_beacon_epoch();

    let control_pool = common::raw_pool(&connection_string).await;
    let beacon_pid = concrete.__test_beacon_backend_pid();
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(beacon_pid)
        .fetch_one(&control_pool)
        .await
        .expect("pg_terminate_backend succeeds");
    assert!(terminated, "setup: terminating the beacon must succeed");

    // Nothing here touches either lock: the purge and the fresh beacon have to
    // come from the ping noticing, not from a consumer asking.
    let purged = common::wait_until(
        Duration::from_secs(15),
        Duration::from_millis(25),
        || async {
            concrete.__test_local_holder_count() == 0
                && concrete.__test_beacon_epoch() > epoch_before
        },
    )
    .await;
    assert!(
        purged,
        "PG-LOCK-013: the ping must notice the loss and purge every local holder, including the \
         one whose consumer never touched it again"
    );

    let renewed = first.renew(Duration::from_secs(30)).await;
    assert!(
        matches!(renewed, Err(ClusterError::LockExpired { .. })),
        "PG-LOCK-013: renew after a beacon loss must report LockExpired, got {renewed:?}"
    );

    // Both names are genuinely free — not merely forgotten locally — well inside
    // their 10-minute TTLs.
    for name in ["res", "res2"] {
        let reacquired = common::wait_until(
            Duration::from_secs(15),
            Duration::from_millis(50),
            || async { lock.try_lock(name, Duration::from_secs(30)).await.is_ok() },
        )
        .await;
        assert!(
            reacquired,
            "PG-LOCK-013: {name} must be acquirable again once the beacon that vouched for it is \
             gone"
        );
    }

    // Both guards belong to the dead incarnation; releasing either is a fenced
    // no-op against the successor's row.
    std::mem::forget(first);
    std::mem::forget(second);
    control_pool.close().await;
    handle.stop().await;
}

/// `PG-LOCK-014`: a holder that stops renewing **and whose reaper never runs**
/// still loses its lock at `expires_at` — a second instance acquires the name
/// without any help from the owner.
///
/// This is the sharpest statement of what the lease-row model buys, and it is
/// meaningless against the design it replaces. Under session-scoped advisory
/// locks, reclaiming an expired lock had to route back through the owning
/// instance — its own sweep, a `cluster_lock_released` NOTIFY it happened to
/// hear, or its `audit_held` backstop — so an owner whose reaper was stalled kept
/// the name wedged fleet-wide past its TTL, with nothing else able to help.
///
/// Instance A is given a 10-minute reaper interval, so within this test its
/// reaper effectively does not run; A's own `try_lock` is never called again
/// either. B acquires the name purely on its own acquire predicate.
#[tokio::test]
async fn pg_lock_014_expired_lock_is_reclaimed_without_its_owner() {
    let (_container, config) =
        common::start_postgres_lock_only_with(json!({ "lock_reaper_interval_ms": 600_000 })).await;
    let connection_string = config.connection_string.clone();

    let owner = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .expect("instance A starts");
    let rival = PostgresLockPlugin::builder(common::lock_config_json(
        &connection_string,
        json!({ "lock_reaper_interval_ms": 600_000 }),
    ))
    .build_and_start()
    .await
    .expect("instance B starts");

    // Short TTL, never renewed. The guard is deliberately kept alive, so A still
    // believes it holds the lock throughout.
    let held = owner
        .lock()
        .try_lock("orphaned", Duration::from_secs(1))
        .await
        .expect("instance A acquires");

    let taken_over =
        common::wait_until(Duration::from_secs(15), Duration::from_millis(100), || {
            let rival_lock = rival.lock();
            async move {
                rival_lock
                    .try_lock("orphaned", Duration::from_secs(30))
                    .await
                    .is_ok()
            }
        })
        .await;
    assert!(
        taken_over,
        "PG-LOCK-014: an expired lease must be takeable by any instance's own acquire, with no \
         reaper on the owning instance having run at all"
    );

    // And the original holder is told, on the only channel the SDK's guard has.
    let renewed = held.renew(Duration::from_secs(30)).await;
    assert!(
        matches!(renewed, Err(ClusterError::LockExpired { .. })),
        "PG-LOCK-014: the superseded holder's renew must report LockExpired, got {renewed:?}"
    );

    std::mem::forget(held);
    rival.stop().await;
    owner.stop().await;
}

/// `PG-LOCK-015`: the reaper's orphan sweep reclaims a row this instance wrote
/// but holds no guard for, and leaves a legitimately-held row alone.
///
/// An orphan is what a `try_acquire` cancelled between its committed INSERT and
/// its `local_holders` registration leaves behind — routine, since `lock()` wraps
/// every attempt in a timeout. It is the one state nothing else can resolve: the
/// row is unexpired *and* vouched for by a live beacon, so no instance will steal
/// it, **including this one**, whose own next acquire reads its own orphan as a
/// live holder. The name is wedged for everyone until the TTL.
///
/// Seam-driven on both sides: the orphan is planted directly rather than by
/// racing a microsecond-wide cancellation window, and the sweep is run on demand
/// rather than by waiting out reaper intervals. What is under test is the sweep's
/// selectivity, not the timing of either.
#[tokio::test]
async fn pg_lock_015_orphan_sweep_reclaims_an_unregistered_row() {
    let (_container, config) =
        common::start_postgres_lock_only_with(json!({ "lock_reaper_interval_ms": 3_600_000 }))
            .await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();
    let concrete = handle.__test_lock();
    let control_pool = common::raw_pool(&connection_string).await;

    let kept = lock
        .try_lock("keep", Duration::from_mins(10))
        .await
        .expect("the accounted lock acquires");
    // Backdated well past the fence below, as a row abandoned an interval ago
    // would be.
    concrete
        .__test_orphan_row("orphan", Duration::from_mins(1))
        .await
        .expect("the orphan seam runs");

    // The orphan really does wedge the name — for this instance too. This is the
    // damage being repaired, so assert it rather than assuming it.
    let wedged = lock.try_lock("orphan", Duration::from_secs(30)).await;
    assert!(
        matches!(wedged, Err(ClusterError::LockContended { .. })),
        "setup: an orphaned row must read as a live holder even to its own writer, got {wedged:?}"
    );

    let reclaimed = concrete
        .__test_sweep_orphans_once(Duration::from_secs(5))
        .await
        .expect("the sweep runs");
    assert_eq!(
        reclaimed, 1,
        "PG-LOCK-015: the sweep must reclaim exactly the one unregistered row"
    );

    let remaining: Vec<String> = sqlx::query_scalar("SELECT name FROM cluster_lock ORDER BY name")
        .fetch_all(&control_pool)
        .await
        .expect("cluster_lock query succeeds");
    assert_eq!(
        remaining,
        vec!["keep".to_owned()],
        "PG-LOCK-015: the held row must survive and the orphan must be gone - a sweep that took \
         neither, or both, would be equally wrong"
    );

    let reacquired = lock
        .try_lock("orphan", Duration::from_secs(30))
        .await
        .expect("PG-LOCK-015: the freed name must be acquirable once the sweep reclaimed it");

    kept.release().await.expect("release succeeds");
    reacquired.release().await.expect("release succeeds");
    control_pool.close().await;
    handle.stop().await;
}

/// `PG-LOCK-017`: the orphan sweep never touches rows that are not its business —
/// another instance's rows, and rows bearing a *previous* incarnation's beacon.
///
/// Its `WHERE` is keyed on this incarnation's beacon, which is what makes
/// detection exact rather than heuristic. Getting that wrong in the permissive
/// direction is the dangerous failure: deleting a foreign instance's row hands
/// that instance's live lock to someone else.
#[tokio::test]
async fn pg_lock_017_orphan_sweep_only_touches_this_incarnations_rows() {
    let (_container, config) =
        common::start_postgres_lock_only_with(json!({ "lock_reaper_interval_ms": 3_600_000 }))
            .await;
    let connection_string = config.connection_string.clone();
    let sweeper = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .expect("the sweeping instance starts");
    let foreign = PostgresLockPlugin::builder(common::lock_config_json(
        &connection_string,
        json!({ "lock_reaper_interval_ms": 3_600_000 }),
    ))
    .build_and_start()
    .await
    .expect("the foreign instance starts");
    let control_pool = common::raw_pool(&connection_string).await;

    // A foreign instance's live lock, and a row bearing a beacon key no live
    // instance holds (what a previous incarnation of *this* process leaves).
    let foreign_held = foreign
        .lock()
        .try_lock("foreign", Duration::from_mins(10))
        .await
        .expect("the foreign instance acquires");
    sqlx::query(
        "INSERT INTO cluster_lock \
         (name, holder_id, acquired_at, expires_at, holder_beacon_hi, holder_beacon_lo) \
         VALUES ('previous', md5('previous')::uuid, now() - interval '1 minute', \
                 now() + interval '10 minutes', 7, 7)",
    )
    .execute(&control_pool)
    .await
    .expect("seed a previous incarnation's row");

    let reclaimed = sweeper
        .__test_lock()
        .__test_sweep_orphans_once(Duration::from_secs(5))
        .await
        .expect("the sweep runs");
    assert_eq!(
        reclaimed, 0,
        "PG-LOCK-017: neither a foreign instance's row nor a previous incarnation's is this \
         sweep's business"
    );

    let remaining: Vec<String> = sqlx::query_scalar("SELECT name FROM cluster_lock ORDER BY name")
        .fetch_all(&control_pool)
        .await
        .expect("cluster_lock query succeeds");
    assert_eq!(
        remaining,
        vec!["foreign".to_owned(), "previous".to_owned()],
        "PG-LOCK-017: both rows must survive a sweep keyed on a different beacon"
    );

    foreign_held.release().await.expect("release succeeds");
    control_pool.close().await;
    foreign.stop().await;
    sweeper.stop().await;
}

/// `PG-LOCK-018`: the sweep's `acquired_at` fence exempts an acquisition
/// registered between the sweep's snapshot of `local_holders` and its `DELETE`.
///
/// Without the fence this races every live acquisition: the known-`holder_id` set
/// is read in Rust *before* the statement executes, so a row committing in
/// between would be read as an orphan and deleted out from under its own guard.
/// The fence is a database timestamp from the *previous* reaper wake, so a row is
/// only ever deleted if it was already unregistered one full wake earlier.
///
/// Driven at the fence rather than at the clock: a freshly-acquired lock is swept
/// with a fence in the *future* (which must reclaim nothing — the fence is what
/// protects it) and then, as a positive control, the same row unregistered and
/// swept with a fence past its `acquired_at`.
#[tokio::test]
async fn pg_lock_018_orphan_sweep_fence_exempts_a_fresh_acquisition() {
    let (_container, config) =
        common::start_postgres_lock_only_with(json!({ "lock_reaper_interval_ms": 3_600_000 }))
            .await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let concrete = handle.__test_lock();

    // An orphan too *young* for the fence: unregistered, but written after the
    // previous wake. Exactly the shape of an acquisition still on its way to
    // registering, and the sweep must leave it alone.
    concrete
        .__test_orphan_row("fresh", Duration::ZERO)
        .await
        .expect("the orphan seam runs");
    let reclaimed = concrete
        .__test_sweep_orphans_once(Duration::from_secs(5))
        .await
        .expect("the sweep runs");
    assert_eq!(
        reclaimed, 0,
        "PG-LOCK-018: a row written since the previous wake must be exempt - it is \
         indistinguishable from an acquisition that has committed but not yet registered"
    );

    // Positive control: the same row, one wake later, is reclaimed. Without this
    // the assertion above would also pass for a sweep that never deletes anything.
    let reclaimed = concrete
        .__test_sweep_orphans_once(Duration::ZERO)
        .await
        .expect("the sweep runs");
    assert_eq!(
        reclaimed, 1,
        "PG-LOCK-018: once the fence has moved past it, the same row is reclaimed"
    );

    handle.stop().await;
}

/// `PG-LOCK-016`: two separate plugin instances on the same database cannot hold
/// the same lock at once — the cross-instance guarantee the whole primitive rests
/// on, arbitrated by Postgres rather than by any in-process bookkeeping.
///
/// Kept distinct from `PG-LOCK-008` (20 concurrent callers inside *one*
/// instance) even though both now exercise the same mechanism, and deliberately
/// so: the lease row arbitrates local and cross-instance contention identically,
/// which is a claim worth holding both halves to. Under the advisory-lock design
/// these two proved genuinely different things — `008` the in-process claim
/// registry, this the database — and a regression that reintroduced any local
/// short-circuit would show up here first.
#[tokio::test]
async fn pg_lock_016_two_instances_cannot_hold_the_same_lock() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();

    let instance_a = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .expect("instance A starts");
    let instance_b =
        PostgresLockPlugin::builder(common::lock_config_json(&connection_string, json!({})))
            .build_and_start()
            .await
            .expect("instance B starts");

    let held_by_a = instance_a
        .lock()
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("instance A acquires");

    let contended = instance_b
        .lock()
        .try_lock("res", Duration::from_secs(30))
        .await;
    assert!(
        matches!(contended, Err(ClusterError::LockContended { .. })),
        "PG-LOCK-016: a second instance must not acquire a lock another instance holds, got \
         {contended:?}"
    );

    // Exactly one lease row, and its beacon is A's — the ownership surface is the
    // row, so that is where the assertion belongs (DESIGN.md §5.1).
    let control_pool = common::raw_pool(&connection_string).await;
    let holders: Vec<(i32, i32)> =
        sqlx::query_as("SELECT holder_beacon_hi, holder_beacon_lo FROM cluster_lock")
            .fetch_all(&control_pool)
            .await
            .expect("cluster_lock query succeeds");
    let beacon_a = instance_a
        .__test_lock()
        .__test_beacon_key()
        .expect("instance A has a live beacon");
    assert_eq!(
        holders,
        vec![beacon_a],
        "PG-LOCK-016: the lock must be held once, by instance A"
    );

    // Handing over across instances works: B gets it as soon as A releases.
    held_by_a.release().await.expect("instance A releases");
    let held_by_b = instance_b
        .lock()
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("PG-LOCK-016: instance B must acquire once instance A has released");

    held_by_b.release().await.expect("instance B releases");
    instance_b.stop().await;
    instance_a.stop().await;
}

/// `PG-LOCK-019`: `stop()` terminates when Postgres is *reachable but
/// unresponsive* — the socket is open and writes succeed, but nothing ever
/// answers (a paused container here; a partition, a blackholed route, or a
/// frozen host in production).
///
/// Two things have to be bounded for this to hold, and both are the kind a
/// server-side `statement_timeout` cannot cover, since the peer that would
/// enforce it is the one that stopped answering — and `sqlx` applies no read
/// timeout of its own. The beacon task must not sit forever in an untimed `ping`
/// or an untimed connect (`beacon::STATEMENT_TIMEOUT`, `CONNECT_TIMEOUT`, and a
/// cancellable reconnect), and the drain must not wait unboundedly on its pool
/// checkout.
///
/// Scoped to those, deliberately. Pool statements remain unbounded once their
/// connection is checked out, so `stop()` as a whole is bounded here only by
/// `pool_acquire_timeout` arithmetic — a freeze landing *after* a successful
/// `pool.acquire()` can still block a background task's join. This scenario does
/// not construct that ordering and must not be read as excluding it
/// (DESIGN.md §11).
///
/// The locks are deliberately never released, so `stop()` takes the drain path
/// rather than short-circuiting. Four of them, though the drain is now one
/// statement regardless: a per-lock regression would cost four
/// `pool_acquire_timeout`s here and blow the budget.
#[tokio::test]
async fn pg_lock_019_stop_terminates_against_an_unresponsive_database() {
    let (container, config) = common::start_postgres_lock_only().await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    for name in ["wedged-a", "wedged-b", "wedged-c", "wedged-d"] {
        let guard = handle
            .lock()
            .try_lock(name, Duration::from_mins(1))
            .await
            .expect("setup: every lock must be acquired while the database is healthy");
        // Consumer never releases: the drain is what has to reclaim these.
        std::mem::forget(guard);
    }

    container.pause().await.expect("setup: pause the container");
    // Long enough for the beacon's 1s ping to enter its round-trip against the
    // frozen backend, so `stop()` meets that task already blocked rather than idle.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let stopping = tokio::spawn(async move { handle.stop().await });
    let stopped = tokio::time::timeout(Duration::from_secs(30), stopping).await;

    // Unpause before asserting, so a failure still leaves the container reapable.
    container
        .unpause()
        .await
        .expect("teardown: unpause the container");
    assert!(
        stopped.is_ok(),
        "PG-LOCK-019: stop() must terminate against an unresponsive database, not block on it"
    );
}

/// `PG-LOCK-020`: once the plugin has stopped, `lock()` says so immediately
/// instead of spending the caller's whole budget and then reporting
/// `LockTimeout`.
///
/// `stop()` cancels the shared token before it closes anything, so an
/// acquisition arriving afterwards used to reach the pool, fail with
/// `Provider { ConnectionLost }`, and — since `lock()` retries that as a
/// transient outage — burn the full `timeout` before returning
/// `LockTimeout`. That is the error ordinary contention produces, so a caller
/// could not tell "someone else holds it" from "this backend is gone", and paid
/// its entire patience budget to be told the wrong one.
///
/// `try_lock` is asserted alongside it because the two must now agree: both take
/// the same pre-work shutdown check, so both report `Shutdown` deterministically
/// rather than one answer or the other depending on how far shutdown had run.
#[tokio::test]
async fn pg_lock_020_lock_after_stop_reports_shutdown_without_waiting() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    // Taken before `stop()` consumes the handle; the backend outlives it.
    let lock = handle.lock();
    handle.stop().await;

    let budget = Duration::from_secs(30);
    let started = std::time::Instant::now();
    let outcome = lock
        .lock("after-stop", Duration::from_secs(30), budget)
        .await;
    let waited = started.elapsed();

    assert!(
        matches!(outcome, Err(ClusterError::Shutdown)),
        "PG-LOCK-020: lock() after stop() must report Shutdown, got {outcome:?}"
    );
    assert!(
        waited < Duration::from_secs(1),
        "PG-LOCK-020: lock() after stop() must answer immediately, not burn its {budget:?} \
         budget; it took {waited:?}"
    );
    let attempted = lock.try_lock("after-stop", Duration::from_secs(30)).await;
    assert!(
        matches!(attempted, Err(ClusterError::Shutdown)),
        "PG-LOCK-020: try_lock() after stop() must agree with lock(), got {attempted:?}"
    );
}

/// `PG-LOCK-021`: a clean `stop()` leaves no `cluster_lock` row behind — for the
/// locks it held **or** for one it had orphaned.
///
/// The drain is a single `DELETE ... WHERE holder_beacon_* = $ours`, which is
/// what makes the orphan half true: the old map-iterating drain could not see a
/// row with no local guard at all, so an acquisition cancelled after its INSERT
/// committed left its row for the TTL even across a clean shutdown. Keying on the
/// beacon rather than on local state covers both in one statement.
///
/// A 10-minute TTL, so only the drain can be what removes any of them, and the
/// orphan is planted through the seam rather than by racing a cancellation
/// window.
#[tokio::test]
async fn pg_lock_021_stop_leaves_no_metadata_rows_behind() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    for name in ["drained-a", "drained-b", "drained-c"] {
        let guard = handle
            .lock()
            .try_lock(name, Duration::from_mins(10))
            .await
            .expect("setup: acquire");
        // Never released, and a TTL far past the end of this test, so only the
        // drain can be what removes these rows.
        std::mem::forget(guard);
    }
    handle
        .__test_lock()
        .__test_orphan_row("drained-orphan", Duration::ZERO)
        .await
        .expect("setup: plant an orphaned row");

    let control_pool = common::raw_pool(&connection_string).await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM cluster_lock")
        .fetch_one(&control_pool)
        .await
        .expect("count rows before stop");
    assert_eq!(
        before, 4,
        "setup: three held locks plus the orphan must all have rows"
    );

    handle.stop().await;

    let after: Vec<String> = sqlx::query_scalar("SELECT name FROM cluster_lock ORDER BY name")
        .fetch_all(&control_pool)
        .await
        .expect("count rows after stop");
    assert!(
        after.is_empty(),
        "PG-LOCK-021: stop() must delete every row this instance's beacon vouched for, orphans \
         included; left behind: {after:?}"
    );
    control_pool.close().await;
}

/// `PG-LOCK-022`: the liveness beacon (DESIGN.md §5.1) is established at startup
/// on its own connection, every acquisition stamps its key onto the row, and a
/// beacon lost to a terminated backend is replaced by one under a *different*
/// key.
///
/// The three properties this asserts are the ones everything downstream rests on:
///
/// * **One advisory lock, on a connection of its own.** Not a pool connection —
///   the beacon exists to be released by exactly one
///   socket closing, so sharing a connection with anything else would couple its
///   lifetime to unrelated traffic.
/// * **The row records it.** `holder_beacon_hi`/`lo` must match the live key, or
///   nothing in the fleet can judge whether the row's owner is still alive.
/// * **A reconnect draws a fresh key.** Per-incarnation is what makes "this row
///   was written by this process, on this connection" provable, and it is why a
///   pre-disconnect row can never be re-vouched by the successor beacon.
#[tokio::test]
async fn pg_lock_022_beacon_is_established_stamped_and_replaced_on_loss() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();
    let concrete = handle.__test_lock();
    let control_pool = common::raw_pool(&connection_string).await;

    let key = concrete
        .__test_beacon_key()
        .expect("PG-LOCK-022: build_and_start must establish the beacon eagerly");
    assert_eq!(
        concrete.__test_beacon_epoch(),
        1,
        "setup: exactly one beacon established so far"
    );

    // Its own connection, holding exactly its own key — asserted by key, not by
    // count, so no other advisory lock on the server can be mistaken for it.
    let beacon_pid = concrete.__test_beacon_backend_pid();
    let beacon_locks: Vec<i32> = sqlx::query_scalar(
        "SELECT pid FROM pg_locks \
         WHERE locktype = 'advisory' AND granted AND objsubid = 2 \
           AND classid = $1::oid AND objid = $2::oid",
    )
    .bind(key.0)
    .bind(key.1)
    .fetch_all(&control_pool)
    .await
    .expect("pg_locks query succeeds");
    assert_eq!(
        beacon_locks,
        vec![beacon_pid],
        "PG-LOCK-022: the beacon key must be granted exactly once, on the beacon's own connection"
    );

    let guard = lock
        .try_lock("beacon-stamped", Duration::from_secs(30))
        .await
        .expect("acquire");
    let stamped: (i32, i32) = sqlx::query_as(
        "SELECT holder_beacon_hi, holder_beacon_lo FROM cluster_lock WHERE name = 'beacon-stamped'",
    )
    .fetch_one(&control_pool)
    .await
    .expect("the acquired lock wrote a row");
    assert_eq!(
        stamped, key,
        "PG-LOCK-022: an acquisition must stamp the live beacon key onto its row"
    );

    // Kill the beacon's backend. The once-per-second ping is what notices — the
    // server released the beacon at the disconnect regardless, but nothing else
    // in this process would ever touch that connection to find out.
    let _terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(beacon_pid)
        .fetch_one(&control_pool)
        .await
        .expect("terminate succeeds");

    let replaced = common::wait_until(Duration::from_secs(15), Duration::from_millis(100), || {
        let concrete = std::sync::Arc::clone(&concrete);
        async move { concrete.__test_beacon_epoch() >= 2 }
    })
    .await;
    assert!(
        replaced,
        "PG-LOCK-022: the ping must notice the terminated backend and establish a fresh beacon"
    );
    assert_ne!(
        concrete.__test_beacon_key(),
        Some(key),
        "PG-LOCK-022: the replacement beacon must carry a different key, or a pre-disconnect row \
         would still look vouched for"
    );

    drop(guard);
    control_pool.close().await;
    handle.stop().await;
}

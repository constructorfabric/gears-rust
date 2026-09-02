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
    use cluster::{ClusterConfig, ClusterWiring, ProfileRegistry, ProviderRegistry};
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
    let (mut handle, bound) = ClusterWiring::from_config(
        Arc::clone(&hub),
        &cluster_config,
        &providers,
    )
    .await
    .expect("PG-LOCK-011: wiring must resolve lock: postgres independently of cache: standalone");
    // The step the gear's `start` takes next: publish the bound set and register
    // the local cluster client a facade resolves through (DESIGN.md
    // section 4.9.3). Without it nothing in this process can reach the profile.
    handle.publish(&Arc::new(ProfileRegistry::new()), bound);

    let lock = DistributedLockV1::resolver(&hub)
        .profile(RoutingProfile)
        .resolve()
        .await
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

    // And the fleet-wide advisory-lock population is *zero*, however many locks are
    // held. That is the invariant the whole model rests on (nothing per-lock is ever
    // locked, so `pg_advisory_unlock` is never called anywhere in the
    // plugin), so assert it directly rather than inferring it from the pool not being
    // exhausted.
    //
    // It used to be *one* — the instance's liveness beacon, the single advisory lock
    // this plugin took. With the beacon removed (DESIGN.md) the
    // plugin takes none at all, so the assertion tightens to the empty set and needs
    // no exemption for a pid of its own.
    let advisory_holders: Vec<i32> = sqlx::query_scalar(
        "SELECT DISTINCT pid FROM pg_locks WHERE locktype = 'advisory' AND granted = true",
    )
    .fetch_all(&control_pool)
    .await
    .expect("pg_locks query succeeds");
    assert!(
        advisory_holders.is_empty(),
        "PG-LOCK-012: holding {HELD} locks must take no advisory lock at all, found holders on \
         pids {advisory_holders:?}"
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

    // Exactly one lease row, at the first fence — the row *is* the ownership surface
    // (DESIGN.md), so that is where the assertion belongs. B's
    // failed attempt must not have stolen, restamped or bumped anything.
    //
    // Asserted on `fence` rather than on the holder identity because A acquired
    // through the guard path, which mints its owner internally per acquisition (and
    // deliberately does not surface it — `LockGuard` cannot carry a token). The
    // owner-side assertion lives in `pg_lock_024`, which acquires through the token
    // half and therefore names its own owner.
    let control_pool = common::raw_pool(&connection_string).await;
    let fences: Vec<i64> = sqlx::query_scalar("SELECT fence FROM cluster_lock")
        .fetch_all(&control_pool)
        .await
        .expect("cluster_lock query succeeds");
    assert_eq!(
        fences,
        vec![1_i64],
        "PG-LOCK-016: the lock must be held once, at the first fence - a contended attempt must \
         not bump it"
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

/// `PG-LOCK-021`: a clean `stop()` leaves every held lease row **in place**, and the
/// lease stays renewable through a handle that never saw the acquire.
///
/// **This assertion is the exact inverse of what it used to be**, and the inversion
/// is the change `L2` exists to make. It previously asserted that `stop()` left *no*
/// `cluster_lock` row behind: a shutdown drain deleted every row keyed on the
/// outgoing incarnation's beacon, so a clean shutdown handed each name back to the
/// fleet immediately. That is a clean handover while the process holding a lock is
/// the process using it, and a fleet-wide revocation the moment locks are brokered —
/// so the drain is gone and this test now asserts the property that replaced it
/// (DESIGN.md, invariant I7).
///
/// Two halves, and the second is the one that matters:
///
/// * The rows survive `stop()`. Necessary but weak on its own — an unswept row
///   nothing can use would satisfy it too.
/// * A **second handle**, built after the first has stopped, renews the lease using
///   the token the first handle issued. No process vouches for a lease, so no
///   process's death ends one, and any replica serves any lease operation. That is
///   invariant I7 stated as a test, and it is the property the whole store-owned
///   lease model was adopted for.
///
/// Acquired through the token half (`acquire`), because that is the half a brokered
/// caller uses and the only one that can hand a token across a restart — a
/// `LockGuard` cannot carry one (§6.5). A 10-minute TTL, so nothing here can pass by
/// the lease merely lapsing.
#[tokio::test]
async fn pg_lock_021_stop_leaves_held_leases_renewable_by_another_handle() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();
    let first = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();

    // Two guard-path leases that are never released, plus one token-path lease whose
    // token outlives the handle that issued it.
    for name in ["survives-a", "survives-b"] {
        let guard = first
            .lock()
            .try_lock(name, Duration::from_mins(10))
            .await
            .expect("setup: acquire");
        std::mem::forget(guard);
    }
    let token = first
        .lock()
        .acquire(
            "survives-c",
            "owner-across-restart",
            Duration::from_mins(10),
        )
        .await
        .expect("setup: acquire a lease by token");

    let control_pool = common::raw_pool(&connection_string).await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM cluster_lock")
        .fetch_one(&control_pool)
        .await
        .expect("count rows before stop");
    assert_eq!(before, 3, "setup: all three leases must have rows");

    first.stop().await;

    let after: Vec<String> = sqlx::query_scalar("SELECT name FROM cluster_lock ORDER BY name")
        .fetch_all(&control_pool)
        .await
        .expect("count rows after stop");
    assert_eq!(
        after,
        vec![
            "survives-a".to_owned(),
            "survives-b".to_owned(),
            "survives-c".to_owned()
        ],
        "PG-LOCK-021: stop() must revoke nothing - a restart is not a lease event"
    );

    // The half that matters: a handle that never saw the acquire serves the renew.
    let second =
        PostgresLockPlugin::builder(common::lock_config_json(&connection_string, json!({})))
            .build_and_start()
            .await
            .expect("a replacement instance starts");
    second
        .lock()
        .renew(&token, Duration::from_mins(10))
        .await
        .expect(
            "PG-LOCK-021: a lease must be renewable through a handle that never saw its acquire - \
             no LockExpired, no re-acquire",
        );
    // And it is still exclusive afterwards, so the renew did not quietly release it.
    let contended = second
        .lock()
        .try_lock("survives-c", Duration::from_secs(30))
        .await;
    assert!(
        matches!(contended, Err(ClusterError::LockContended { .. })),
        "PG-LOCK-021: the renewed lease must still exclude, got {contended:?}"
    );

    second
        .lock()
        .release(&token)
        .await
        .expect("the second handle releases the lease it renewed");

    control_pool.close().await;
    second.stop().await;
}

/// `PG-LOCK-023`: **uniform expiry** — a killed holder's lock is reclaimed at its TTL
/// and *not before*.
///
/// This is `L2`'s headline exit criterion and the assertion that the beacon removal
/// was complete (plan §6 "Uniform expiry", DESIGN.md). It has a
/// negative half and a positive half, and the negative half is the new one:
///
/// * **Not before.** The holder is killed outright — its handle stopped, its guard
///   task gone, its pool closed — and the lock must still be *unacquirable* for the
///   remainder of its TTL. Under the beacon this was false by design: Postgres
///   dropped the beacon's advisory lock the instant the connection died, so the lock
///   became stealable in milliseconds. That sub-TTL reclaim is the capability `L2`
///   deliberately removes, and this half is what would fail if any second liveness
///   mechanism were reintroduced.
/// * **At its TTL.** The lock must then become acquirable, so the removal did not
///   simply wedge the name.
///
/// The TTL is short (2s) because the test has to *wait out* the whole of it to prove
/// the negative, and the "not before" window is sampled repeatedly rather than once
/// so a single lucky read cannot pass it.
///
/// Both profiles now share this timing, which is the point: keeping the beacon for
/// in-process acquisitions and dropping it for brokered ones would have meant one
/// deployment reclaiming a dead holder's lock in milliseconds and another waiting out
/// the TTL — the same code, the same config, two timings (§5.8.2, Goal 2).
#[tokio::test]
async fn pg_lock_023_a_killed_holders_lock_is_reclaimed_at_its_ttl_and_not_before() {
    const TTL: Duration = Duration::from_secs(2);

    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();

    // A long reaper interval, so nothing here can be the reaper being prompt: the
    // survivor takes the name on its own acquire predicate or not at all.
    let victim = PostgresLockPlugin::builder(common::lock_config_json(
        &connection_string,
        json!({ "lock_reaper_interval_ms": 600_000 }),
    ))
    .build_and_start()
    .await
    .expect("the victim instance starts");
    let survivor = PostgresLockPlugin::builder(common::lock_config_json(
        &connection_string,
        json!({ "lock_reaper_interval_ms": 600_000 }),
    ))
    .build_and_start()
    .await
    .expect("the surviving instance starts");

    let guard = victim
        .lock()
        .try_lock("uniform", TTL)
        .await
        .expect("the victim acquires");
    // Never released: the guard is leaked so no `Drop` can turn this into a voluntary
    // release, which would test nothing.
    std::mem::forget(guard);

    let killed_at = tokio::time::Instant::now();
    // Kill the holder as completely as a process death would: tasks cancelled, pool
    // closed, nothing left running that could renew.
    victim.stop().await;

    // The negative half. Sample across most of the remaining TTL rather than once.
    let mut samples = 0_u32;
    while killed_at.elapsed() < TTL.mul_f64(0.75) {
        let attempted = survivor
            .lock()
            .try_lock("uniform", Duration::from_secs(30))
            .await;
        assert!(
            matches!(attempted, Err(ClusterError::LockContended { .. })),
            "PG-LOCK-023: a killed holder's lock must stay held for its whole TTL - it became \
             acquirable after {elapsed:?} of a {TTL:?} lease, which means some liveness mechanism \
             other than the deadline reclaimed it; got {attempted:?}",
            elapsed = killed_at.elapsed()
        );
        samples += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        samples >= 5,
        "PG-LOCK-023: the not-before window must actually be sampled, got {samples} samples"
    );

    // The positive half: it does lapse, on the deadline and nothing else.
    let reclaimed = common::wait_until(TTL * 4, Duration::from_millis(50), || {
        let lock = survivor.lock();
        async move {
            lock.try_lock("uniform", Duration::from_secs(30))
                .await
                .is_ok()
        }
    })
    .await;
    assert!(
        reclaimed,
        "PG-LOCK-023: the lease must lapse at its deadline - removing the beacon must not wedge \
         the name instead"
    );

    survivor.stop().await;
}

/// `PG-LOCK-024`: the lease token is the whole of the authority, and a **steal fences
/// its predecessor**.
///
/// Four properties of §5.8.1, asserted against the row rather than against anything a
/// process remembers:
///
/// * The row records the owner the caller named. The token half passes a `ClientId`
///   straight through (§5.4), unlike the guard half which mints one internally.
/// * A steal-on-expiry strictly *increases* the fence, so the counter is not merely
///   different but ordered — which makes "steal on expiry" safe rather than
///   just detectable.
/// * The superseded holder's `renew` fails. It is `LockExpired` whichever fence
///   missed: lapsed, stolen and never-yours are indistinguishable and all three mean
///   the caller must stop acting as the holder (§6.9).
/// * The superseded holder's `release` is a **no-op `Ok`** that leaves the successor's
///   lease untouched. This is the one that would be a mutual-exclusion break if the
///   predicate were `name` alone: a stale token would delete a live holder's lease.
#[tokio::test]
async fn pg_lock_024_a_stolen_lease_fences_its_predecessor() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresLockPlugin::builder(common::lock_config_json(
        &connection_string,
        json!({ "lock_reaper_interval_ms": 600_000 }),
    ))
    .build_and_start()
    .await
    .unwrap();
    let lock = handle.lock();
    let concrete = handle.__test_lock();

    let first = lock
        .acquire("fenced", "owner-first", Duration::from_mins(10))
        .await
        .expect("the first owner acquires");
    assert_eq!(
        concrete.__test_lease_row("fenced").await.unwrap(),
        Some(("owner-first".to_owned(), 1)),
        "PG-LOCK-024: the row must record the owner the caller named, at the first fence"
    );

    // Lapse the lease **by moving its deadline into the past**, rather than by
    // sleeping out a short TTL.
    //
    // Not merely for speed: the fence only survives a lapse while the *row* does, and
    // a short TTL guarantees it will not. `try_acquire` signals the reaper's
    // `deadline_hint` whenever the TTL it writes is shorter than the reaper's interval
    // (`lock::should_hint`), so a sub-interval TTL wakes the reaper within its 100ms
    // floor and the lapsed row is swept — after which a re-acquire is a fresh INSERT
    // at `FIRST_FENCE` and the counter has reset. That reset is the known gap item
    // `L3` closes with `fence_retention` (DESIGN.md); see
    // ADR-012 for why it is not a mutual-exclusion break in the meantime, and note
    // that `L3` must reach this table's reaper and not only the cache's.
    //
    // So: a 10-minute TTL at acquire (no hint, and the reaper is on a 600s interval
    // besides), then an out-of-band expiry that notifies nothing. The row is still
    // there when the steal runs, which lets this test assert the acquire
    // statement's own fence arithmetic rather than the reaper's timing.
    let control_pool = common::raw_pool(&connection_string).await;
    sqlx::query("UPDATE cluster_lock SET expires_at = now() - interval '1 second' WHERE name = $1")
        .bind("fenced")
        .execute(&control_pool)
        .await
        .expect("setup: lapse the lease out of band");

    let second = lock
        .acquire("fenced", "owner-second", Duration::from_mins(10))
        .await
        .expect("a lapsed lease must be stealable");
    assert!(
        second.fence > first.fence,
        "PG-LOCK-024: a steal must strictly increase the fence, went {} -> {}",
        first.fence,
        second.fence
    );
    assert_eq!(
        concrete.__test_lease_row("fenced").await.unwrap(),
        Some(("owner-second".to_owned(), 2)),
        "PG-LOCK-024: the row must carry the successor's owner and the bumped fence"
    );

    // The predecessor is fenced out of both operations.
    let renewed = lock.renew(&first, Duration::from_mins(10)).await;
    assert!(
        matches!(renewed, Err(ClusterError::LockExpired { .. })),
        "PG-LOCK-024: a fenced-out token must not renew, got {renewed:?}"
    );
    lock.release(&first)
        .await
        .expect("PG-LOCK-024: releasing a fenced-out token is Ok by absence, never an error");
    assert_eq!(
        concrete.__test_lease_row("fenced").await.unwrap(),
        Some(("owner-second".to_owned(), 2)),
        "PG-LOCK-024: the predecessor's release must not touch the successor's lease"
    );

    // And the successor really still holds it.
    lock.renew(&second, Duration::from_mins(10))
        .await
        .expect("the live holder renews");
    lock.release(&second)
        .await
        .expect("the live holder releases");

    control_pool.close().await;
    handle.stop().await;
}

/// `PG-LOCK-025`: `release` is idempotent by absence, and `renew` after it is
/// `LockExpired`.
///
/// The trait's contract, and worth its own test because the old implementation
/// reached this answer through a *local* registry — a release whose `local_holders`
/// entry was gone returned `Ok` without issuing a statement. Nothing local is
/// consulted now: the SQL predicate is the whole check, so absence has to produce
/// `Ok` from the statement matching zero rows (§6.10).
#[tokio::test]
async fn pg_lock_025_release_is_idempotent_by_absence() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let token = lock
        .acquire("idem", "owner-a", Duration::from_mins(10))
        .await
        .expect("acquire");
    lock.release(&token).await.expect("the first release");
    lock.release(&token)
        .await
        .expect("PG-LOCK-025: a retried release must be Ok, never a not-found");
    // A third time, against a name that never existed at all.
    let never = cluster_sdk::LeaseToken::new("never-held", "owner-a", 1);
    lock.release(&never)
        .await
        .expect("PG-LOCK-025: releasing a lease that never existed must be Ok");

    let renewed = lock.renew(&token, Duration::from_mins(10)).await;
    assert!(
        matches!(renewed, Err(ClusterError::LockExpired { .. })),
        "PG-LOCK-025: renewing a released lease must be LockExpired, got {renewed:?}"
    );

    handle.stop().await;
}

/// `PG-LOCK-026`: the fence-retention window (item `L3`,
/// DESIGN.md). A lease that **lapses**, sits there long
/// enough for many reaper sweeps, and is then re-acquired **by the same owner**
/// gets a strictly greater fence.
///
/// Same owner is the whole point. A different owner is fenced by the `owner`
/// column on its own; it is the same one that a restarted counter would hand a
/// matching predicate, because its stale token carries the identity that
/// survives. Before `L3` the reaper deleted the row at its deadline and the
/// re-acquire was a fresh INSERT at fence 1 — see `PG-LOCK-024`'s note.
///
/// The reaper runs at 100 ms against a real 300 ms TTL, so this asserts against a
/// sweep loop that certainly ran, rather than one that had no time to.
#[tokio::test]
async fn pg_lock_026_the_fence_survives_a_lapse_and_its_sweeps() {
    let (_container, config) =
        common::start_postgres_lock_only_with(json!({ "lock_reaper_interval_ms": 100 })).await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();
    let concrete = handle.__test_lock();

    let stale = lock
        .acquire("retained", "owner-a", Duration::from_millis(300))
        .await
        .expect("the first acquisition");
    assert_eq!(stale.fence, 1);

    // Past the lease and past a dozen sweeps, but nowhere near the default
    // hour-long retention window.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert_eq!(
        concrete.__test_lease_row("retained").await.unwrap(),
        Some(("owner-a".to_owned(), 1)),
        "PG-LOCK-026: the lapsed row must still be there - the sweep predicate is \
         expires_at <= now() - retention, not expires_at <= now()"
    );

    let fresh = lock
        .acquire("retained", "owner-a", Duration::from_mins(10))
        .await
        .expect("a lapsed lease must be stealable by anyone, including its last owner");
    assert!(
        fresh.fence > stale.fence,
        "PG-LOCK-026: the same owner re-acquiring must be fenced against its own stale \
         token, went {} -> {}",
        stale.fence,
        fresh.fence
    );

    // The consequence that matters: the stale token is inert.
    let renewed = lock.renew(&stale, Duration::from_mins(10)).await;
    assert!(
        matches!(renewed, Err(ClusterError::LockExpired { .. })),
        "PG-LOCK-026: the stale token must not renew the lease that replaced it, got {renewed:?}"
    );

    lock.release(&fresh)
        .await
        .expect("the live holder releases");
    handle.stop().await;
}

/// `PG-LOCK-027`: the other half of the window, and the negative control for
/// `PG-LOCK-026` — shorten `fence_retention_ms` below the lapse and the row
/// really is swept, the counter really does restart, and the stale token really
/// does match again.
///
/// Without this, `PG-LOCK-026` would also pass against a reaper that simply never
/// managed to run. With it, the only difference between the two tests is the
/// window, so the window is what the pair measures.
#[tokio::test]
async fn pg_lock_027_a_row_is_swept_once_its_window_passes() {
    let (_container, config) = common::start_postgres_lock_only_with(json!({
        "lock_reaper_interval_ms": 100,
        "fence_retention_ms": 200,
    }))
    .await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();
    let concrete = handle.__test_lock();

    let stale = lock
        .acquire("swept", "owner-a", Duration::from_millis(300))
        .await
        .expect("the first acquisition");
    assert_eq!(stale.fence, 1);

    // Past the lease (300 ms), past the window (200 ms more), and past several
    // sweeps at 100 ms.
    let gone = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async { concrete.__test_lease_row("swept").await.unwrap().is_none() },
    )
    .await;
    assert!(
        gone,
        "PG-LOCK-027: a row past both its deadline and its window must be swept"
    );

    let fresh = lock
        .acquire("swept", "owner-a", Duration::from_mins(10))
        .await
        .expect("acquire a free name");
    assert_eq!(
        fresh.fence, 1,
        "PG-LOCK-027: with no row left there is no counter to carry"
    );

    lock.release(&fresh)
        .await
        .expect("the live holder releases");
    handle.stop().await;
}

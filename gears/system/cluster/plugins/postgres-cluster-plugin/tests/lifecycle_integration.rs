//! Layer 3 — lifecycle integration scenarios (docs/TESTING.md §4.5,
//! `PG-LIFE-001..008`).

#![cfg(feature = "integration")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

mod common;

use std::time::Duration;

use cluster_sdk::error::ClusterError;
use postgres_cluster_plugin::{PostgresClusterPlugin, PostgresLockPlugin};
use serde_json::json;

/// A `Write` sink appending to a shared buffer, for capturing `tracing` output
/// in the `PG-LIFE-007/008` handle-`Drop` scenarios.
#[derive(Clone)]
struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Installs a thread-local WARN-level subscriber for the current test.
/// `#[tokio::test]` uses a current-thread runtime, so a `Drop` running during a
/// spawned task's unwind executes on this same thread and its warn is captured.
#[allow(dead_code)] // only used by the debug-vs-release-gated variants below
fn scoped_warn_capture() -> (
    tracing::subscriber::DefaultGuard,
    std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
) {
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(SharedWriter(std::sync::Arc::clone(&buf)))
        .with_max_level(tracing::Level::WARN)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (guard, buf)
}

#[allow(dead_code)]
fn captured(buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock().unwrap()).into_owned()
}

/// Downcasts a `catch_unwind` panic payload to its string message.
#[allow(dead_code)]
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

/// `PG-LIFE-001`: `build_and_start` against a fresh database creates the
/// plugin's tables and returns `Ok`.
#[tokio::test]
async fn pg_life_001_build_and_start_runs_migrations_on_fresh_db() {
    let (_container, config) = common::start_postgres().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .expect("PG-LIFE-001: build_and_start must succeed against a fresh database");

    let pool = common::raw_pool(&connection_string).await;
    assert!(common::table_exists(&pool, "public", "cluster_cache").await);
    assert!(common::table_exists(&pool, "public", "cluster_lock").await);

    handle.stop().await;
}

/// `PG-LIFE-002`: calling `build_and_start` twice against the same database
/// does not fail or double-create tables (mirrors `PG-CACHE-007` at the
/// lifecycle level, additionally checking `_sqlx_migrations` bookkeeping).
#[tokio::test]
async fn pg_life_002_build_and_start_is_idempotent() {
    let (_container, config) = common::start_postgres().await;
    let connection_string = config.connection_string.clone();

    let handle_one = PostgresClusterPlugin::builder(config.clone())
        .build_and_start()
        .await
        .expect("first build_and_start succeeds");
    handle_one.stop().await;

    let handle_two = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .expect("PG-LIFE-002: second build_and_start must not fail");

    let pool = common::raw_pool(&connection_string).await;
    let migration_count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("migrations table query succeeds");
    assert_eq!(
        migration_count, 2,
        "PG-LIFE-002: exactly the two plugin migrations must be recorded, not duplicated"
    );

    handle_two.stop().await;
}

/// `PG-LIFE-003`: after `stop`, the Postgres server shows zero connections
/// from the plugin and any advisory locks it held are released.
///
/// Regression guard for the `held`-drain fix (DESIGN.md §10 step 4): a lock's
/// pinned connection lives in `PostgresLock`'s `held: DashMap`, checked out of
/// the pool for the lock's full duration and outside the pool's own tracking.
/// `sqlx::Pool::close()` blocks until every checked-out connection is returned,
/// so before the fix a lock still held at `stop()` time made `stop()` hang
/// forever (this was also the root cause of the previously-unresolved
/// `PG-LOCK-007`/`PG-SPEC-004` hang). `stop()` now calls
/// `PostgresLock::drain_held` (unlock + drop each pinned connection, reusing the
/// TTL reaper's `reclaim`) before `pool.close()`, so a still-held lock is
/// released and its connection returned — leaving zero plugin connections.
#[tokio::test]
async fn pg_life_003_stop_closes_pool_and_listen_connection() {
    let (_container, config) = common::start_postgres().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();
    let guard = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("acquire");
    std::mem::forget(guard); // simulate a still-held lock at shutdown time

    let control_pool = common::raw_pool(&connection_string).await;
    let before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity WHERE datname = 'cluster_test' AND pid <> pg_backend_pid()",
    )
    .fetch_one(&control_pool)
    .await
    .expect("pg_stat_activity query succeeds");
    assert!(
        before > 0,
        "setup: the plugin must actually hold connections before stop()"
    );

    handle.stop().await;

    // `pool.close()` returns once the client has closed its sockets, but
    // Postgres tears the backend processes down asynchronously — they leave
    // `pg_stat_activity` a beat later. Poll for the zero rather than sampling
    // once: a real connection leak still times out and fails, this only
    // tolerates the server-side exit lag (which widens under CPU contention in
    // a full parallel run).
    let drained = common::wait_until(Duration::from_secs(5), Duration::from_millis(25), || async {
        let after: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = 'cluster_test' AND pid <> pg_backend_pid()",
        )
        .fetch_one(&control_pool)
        .await
        .expect("pg_stat_activity query succeeds");
        after == 0
    })
    .await;
    assert!(
        drained,
        "PG-LIFE-003: stop() must leave zero plugin connections open"
    );

    // The advisory unlock runs synchronously inside `drain_held` *before*
    // `pool.close()`, so unlike the connection count this is not subject to the
    // backend-exit lag — assert it directly.
    let advisory_locks: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE locktype = 'advisory'")
            .fetch_one(&control_pool)
            .await
            .expect("pg_locks query succeeds");
    assert_eq!(
        advisory_locks, 0,
        "PG-LIFE-003: stop() must release any held advisory locks"
    );
}

/// `PG-LIFE-004`: every active watch observes `Closed(Shutdown)` before
/// `stop()` returns (see also `PG-WATCH-005`; here with multiple concurrent
/// watches to confirm `stop()` drains all of them, not just one).
#[tokio::test]
async fn pg_life_004_stop_delivers_closed_shutdown_to_all_watches() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let mut watches = Vec::new();
    for key in ["l4-a", "l4-b", "l4-c"] {
        watches.push(cache.watch(key).await.expect("watch succeeds"));
    }

    handle.stop().await;

    for mut watch in watches {
        let event = tokio::time::timeout(Duration::from_millis(200), watch.recv())
            .await
            .expect("event arrives")
            .expect("channel carries a terminal event, not a bare close");
        assert!(
            matches!(
                event,
                cluster_sdk::CacheWatchEvent::Closed(ClusterError::Shutdown)
            ),
            "PG-LIFE-004: every watch must observe Closed(Shutdown), got {event:?}"
        );
    }
}

/// `PG-LIFE-005`: `pgbouncer_transaction_mode: true` is rejected with
/// `InvalidConfig` at startup, before any connection attempt.
#[tokio::test]
async fn pg_life_005_pgbouncer_transaction_mode_rejected() {
    // No real container needed: the rejection runs before the pool ever
    // connects (`plugin.rs::build_and_start` calls
    // `reject_pgbouncer_transaction_mode` first), so an unreachable
    // connection string still exercises the same code path.
    let config: postgres_cluster_plugin::PostgresClusterConfig = serde_json::from_value(json!({
        "connection_string": "postgres://postgres:postgres@127.0.0.1:1/unreachable",
        "pgbouncer_transaction_mode": true,
    }))
    .expect("config parses");

    // `PostgresClusterHandle` (the `Ok` payload) does not implement `Debug`
    // (DESIGN.md never requires it, and it holds a live pool/task handles),
    // so match explicitly rather than formatting `result` as a whole.
    match PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
    {
        Err(ClusterError::InvalidConfig { .. }) => {}
        Err(other) => panic!("PG-LIFE-005: expected InvalidConfig, got {other:?}"),
        Ok(_handle) => {
            panic!("PG-LIFE-005: expected pgbouncer_transaction_mode to be rejected, got Ok")
        }
    }
}

/// `PG-LIFE-006`: an invalid connection string is rejected immediately with
/// `ClusterError::InvalidConfig` (DESIGN.md §3.2 / §9), not after a
/// connection-timeout wait and not as an opaque `Provider` error. A malformed
/// DSN produces `sqlx::Error::Configuration`, which `map_sqlx_error` maps to
/// `InvalidConfig` (PGR-M3).
#[tokio::test]
async fn pg_life_006_invalid_connection_string_rejected_promptly() {
    let config: postgres_cluster_plugin::PostgresClusterConfig = serde_json::from_value(json!({
        "connection_string": "not a valid connection string",
    }))
    .expect("config parses");

    let started = tokio::time::Instant::now();
    match PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
    {
        Err(ClusterError::InvalidConfig { .. }) => {}
        Err(other) => panic!(
            "PG-LIFE-006: expected InvalidConfig for a malformed connection string, got {other:?}"
        ),
        Ok(_handle) => panic!("PG-LIFE-006: expected rejection, got Ok"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "PG-LIFE-006: rejection must be prompt, not a connection-timeout wait"
    );
}

/// `PG-LIFE-007`: dropping a handle without calling `stop()` panics loudly in
/// a debug build (ADR-006); calling `stop()` first, then dropping, does not.
/// Exercised for both the combined `PostgresClusterHandle` and the
/// standalone `PostgresLockHandle` (DESIGN.md §3.5).
#[cfg(debug_assertions)]
#[tokio::test]
async fn pg_life_007_drop_without_stop_panics_in_debug_combined() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(handle)));
    let payload = result.expect_err(
        "PG-LIFE-007: dropping an un-stopped PostgresClusterHandle must panic in debug",
    );
    let message = panic_message(&*payload);
    assert!(
        message.contains("PostgresClusterHandle dropped without stop()"),
        "PG-LIFE-007: the debug panic must name the forgotten-stop programming error, got {message:?}"
    );
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn pg_life_007_drop_without_stop_panics_in_debug_standalone() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(handle)));
    let payload = result
        .expect_err("PG-LIFE-007: dropping an un-stopped PostgresLockHandle must panic in debug");
    let message = panic_message(&*payload);
    assert!(
        message.contains("PostgresLockHandle dropped without stop()"),
        "PG-LIFE-007: the debug panic must name the forgotten-stop programming error, got {message:?}"
    );
}

/// `PG-LIFE-007` (release branch): in a **release** build the `Drop` guard does
/// not panic — it logs the programming-error WARN and lets the drop proceed
/// (ADR-006). Compiled only when `debug_assertions` is off, so this covers the
/// `#[cfg(not(debug_assertions))]` arm that the debug tests above cannot reach.
#[cfg(not(debug_assertions))]
#[tokio::test]
async fn pg_life_007_drop_without_stop_warns_not_panics_in_release() {
    let (_guard, warns) = scoped_warn_capture();
    {
        let (_container, config) = common::start_postgres().await;
        let handle = PostgresClusterPlugin::builder(config)
            .build_and_start()
            .await
            .unwrap();
        // No panic in release — dropping just logs and proceeds.
        drop(handle);
    }
    assert!(
        captured(&warns).contains("PostgresClusterHandle dropped without stop()"),
        "PG-LIFE-007: a release-build drop-without-stop must log the programming-error WARN"
    );
}

#[tokio::test]
async fn pg_life_007_stop_then_drop_panics_neither() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    // `stop(mut self)` consumes `handle`; `self.stopped = true` is set as its
    // last step (DESIGN.md §3.2 step 4) before `self` falls out of scope at
    // the end of `stop`'s own body, so calling `stop()` at all — reaching the
    // line below without a panic — already is "stop() then drop, panics
    // neither"; there is no separate external drop for this test to add.
    handle.stop().await;

    let (_container2, config2) = common::start_postgres_lock_only().await;
    let handle2 = PostgresLockPlugin::builder(config2)
        .build_and_start()
        .await
        .unwrap();
    handle2.stop().await; // same property, standalone handle
}

/// `PG-LIFE-008`: a panic inside a task that owns an un-stopped handle must
/// not abort the process — the handle's `Drop` impl checks
/// `std::thread::panicking()` and degrades to a warning log instead of its
/// own debug-build panic, avoiding a double-panic abort.
///
/// This asserts both process survival (reaching the assertion below at all is
/// only possible if the runtime did not abort) **and** — via a thread-local
/// WARN capture — that the degrade path actually logged its double-panic-avoidance
/// message rather than silently swallowing the forgotten stop (PGR-L5). The
/// spawned task runs on the current-thread runtime's single worker (this test's
/// thread), so its unwind-time `Drop` warn is captured here.
#[tokio::test]
async fn pg_life_008_drop_during_panic_unwind_degrades_to_warning() {
    let (_guard, warns) = scoped_warn_capture();
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();

    let outcome = tokio::spawn(async move {
        let _still_owns_handle = handle;
        panic!("PG-LIFE-008: intentional panic while holding an un-stopped handle");
    })
    .await;

    assert!(
        outcome.is_err(),
        "the spawned task must have panicked (and the handle must have dropped during that unwind)"
    );
    // Reaching here at all already proves no double-panic abort. Additionally
    // assert the degrade path logged its intent, so a regression that dropped
    // the `std::thread::panicking()` guard (and thus re-introduced the abort
    // risk) would fail here rather than pass silently.
    assert!(
        captured(&warns).contains("skipping debug panic to avoid double-panic abort"),
        "PG-LIFE-008: the panic-unwind drop must log the double-panic-avoidance WARN"
    );
}

//! Layer 3 — Postgres-specific scenarios (docs/TESTING.md §4.6,
//! `PG-SPEC-001..014`): behaviours unique to this backend that the
//! conformance suite (Layer 2) cannot reach.

#![cfg(feature = "integration")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

mod common;

use std::time::Duration;

use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::error::ClusterError;
use postgres_cluster_plugin::{PostgresClusterPlugin, PostgresLockPlugin};
use serde_json::json;
use sqlx::AssertSqlSafe;

/// `PG-SPEC-001`: an empty-payload `NOTIFY` on `cluster_cache_changes`
/// (Postgres's own overflow signal, DESIGN.md §2.3/§4.3) is interpreted as
/// `Reset` and delivered to every active watcher — injected directly here
/// rather than via a real NOTIFY-queue overflow, which isn't reproducible on
/// demand.
#[tokio::test]
async fn pg_spec_001_empty_payload_notify_maps_to_reset() {
    let (_container, config) = common::start_postgres().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let mut watch_a = cache.watch("spec1-a").await.expect("watch a");
    let mut watch_b = cache.watch("spec1-b").await.expect("watch b");

    let control_pool = common::raw_pool(&connection_string).await;
    sqlx::query("SELECT pg_notify('cluster_cache_changes', '')")
        .execute(&control_pool)
        .await
        .expect("empty-payload NOTIFY succeeds");

    for watch in [&mut watch_a, &mut watch_b] {
        // Match the neighbouring LISTEN/NOTIFY watch-delivery timeout (2-3s):
        // this asserts arrival, not latency, so a 500ms deadline only invited CI
        // flakiness under load (PGR-E4).
        let event = tokio::time::timeout(Duration::from_secs(3), watch.recv())
            .await
            .expect("event arrives within 3s");
        assert!(
            matches!(event, Some(cluster_sdk::CacheWatchEvent::Reset)),
            "PG-SPEC-001: an empty NOTIFY payload must map to Reset, got {event:?}"
        );
    }

    handle.stop().await;
}

/// `PG-SPEC-002`: a `put` with a key exceeding `cache::watch::MAX_KEY_BYTES` is
/// rejected with `InvalidName`.
///
/// The boundary is 2048 bytes. Two Postgres limits bear on a cache key and the
/// tighter wins. The NOTIFY payload budget is the looser one: Postgres's hard
/// payload limit is 7999 bytes (confirmed empirically — `pg_notify('x',
/// repeat('a', 8000))` fails with `payload string too long`, not the 8192 or the
/// 8190 earlier notes assumed), leaving 7997 after the two-byte `<event>:`
/// prefix. But `cluster_cache.key` is a `PRIMARY KEY`, so every key also has to
/// fit a btree index tuple (~2704 bytes). Bounding only by NOTIFY left keys in
/// roughly 2705..=7997 passing this guard and then failing mid-write with
/// SQLSTATE `54000`; the effective bound is now `limits::MAX_INDEXED_KEY_BYTES`.
///
/// The accept case is the load-bearing half: 2048 bytes must round-trip through
/// a real server, proving the bound clears both the btree tuple limit and the
/// `cluster_cache_key_len_check` CHECK the migration declares.
#[tokio::test]
async fn pg_spec_002_key_length_over_2048_bytes_rejected() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let too_long_key = "k".repeat(2_049);
    let result = cache
        .put(PutRequest {
            key: &too_long_key,
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await;
    assert!(
        matches!(result, Err(ClusterError::InvalidName { .. })),
        "PG-SPEC-002: a key over 2048 bytes must be rejected as InvalidName, got {result:?}"
    );

    // The boundary itself must still be accepted — and actually reach the table,
    // which is what proves 2048 is under the btree limit rather than merely
    // under the NOTIFY budget.
    let boundary_key = "k".repeat(2_048);
    let ok = cache
        .put(PutRequest {
            key: &boundary_key,
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await;
    assert!(
        ok.is_ok(),
        "PG-SPEC-002: a key at exactly 2048 bytes must be accepted, got {ok:?}"
    );
    assert!(
        cache.get(&boundary_key).await.unwrap().is_some(),
        "PG-SPEC-002: a key at exactly 2048 bytes must survive the round-trip"
    );

    handle.stop().await;
}

/// `PG-SPEC-005`: a mid-session `synchronous_commit` mutation is corrected on the
/// next pool checkout (DESIGN.md §3.4).
///
/// The GUC is `USERSET`, so anything sharing a connection can flip it — which is
/// why enforcement is on `before_acquire` (every checkout) and not only
/// `after_connect` (once per connection). This drives that directly: flip the
/// setting on a checked-out connection, return it, and take it again.
///
/// `pool_max_size: 1` is what makes the assertion meaningful rather than
/// accidental — with one connection in the pool, the checkout after the flip is
/// necessarily the *same* connection, so a pass cannot come from having been
/// handed a fresh one.
///
/// This scenario used to have a second half, asserting the same correction on the
/// dedicated lock session's own re-assertion timer. That connection is gone: the
/// lock opens no long-lived connection of its own at all now that the liveness
/// beacon is gone, so there is no off-pool session with a durability setting to
/// maintain.
#[tokio::test]
async fn pg_spec_005_mid_checkout_synchronous_commit_mutation_corrected() {
    let (_container, config) =
        common::start_postgres_lock_only_with(json!({ "pool_max_size": 1 })).await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let pool = handle.__test_pool();

    {
        let mut conn = pool.acquire().await.expect("checkout");
        let baseline: String = sqlx::query_scalar("SHOW synchronous_commit")
            .fetch_one(&mut *conn)
            .await
            .expect("SHOW succeeds");
        assert_eq!(
            baseline, "on",
            "PG-SPEC-005: a pooled connection must start at synchronous_commit=on"
        );

        // Simulate an external mid-session flip, then return the connection.
        sqlx::query("SET synchronous_commit = off")
            .execute(&mut *conn)
            .await
            .expect("the flip succeeds");
        let flipped: String = sqlx::query_scalar("SHOW synchronous_commit")
            .fetch_one(&mut *conn)
            .await
            .expect("SHOW succeeds");
        assert_eq!(
            flipped, "off",
            "PG-SPEC-005: the flip must actually take effect on that connection"
        );
    }

    // Same connection, taken again: `before_acquire` must have corrected it.
    let mut conn = pool.acquire().await.expect("re-checkout");
    let corrected: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(&mut *conn)
        .await
        .expect("SHOW succeeds");
    assert_eq!(
        corrected, "on",
        "PG-SPEC-005: the before_acquire hook must restore synchronous_commit=on on checkout"
    );
    drop(conn);

    handle.stop().await;
}

/// A `Write` sink that appends to a shared byte buffer, so a process-global
/// `tracing` subscriber can capture events emitted from any thread (including
/// the reaper's spawned task, which thread-local capture would miss).
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

/// Installs a process-global WARN-level `tracing` subscriber (once) writing to a
/// shared buffer and returns that buffer. Global - not thread-local - so events
/// from the reaper's spawned task are captured regardless of runtime thread.
/// Safe to share across tests: `pg_spec_006` asserts only on a message unique
/// to the over-threshold cardinality condition, so other tests' WARNs cannot
/// false-positive it.
fn install_global_warn_capture() -> std::sync::Arc<std::sync::Mutex<Vec<u8>>> {
    use std::sync::OnceLock;
    static BUF: OnceLock<std::sync::Arc<std::sync::Mutex<Vec<u8>>>> = OnceLock::new();
    std::sync::Arc::clone(BUF.get_or_init(|| {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(SharedWriter(std::sync::Arc::clone(&buf)))
            .with_max_level(tracing::Level::WARN)
            .finish();
        // Ignore an already-installed global (nothing else in this binary sets one).
        let _installed = tracing::subscriber::set_global_default(subscriber);
        buf
    }))
}

/// Installs a **thread-local** WARN-level `tracing` subscriber for the current
/// test, returning its uninstall guard and capture buffer. Uses `set_default`
/// (thread-local), not `set_global_default`, so each test's capture is isolated
/// from every other test's plugin — required for asserting the *absence* of a
/// WARN (`pg_spec_008`), which a shared process-global buffer (polluted by other
/// tests' plugins) could never do. `#[tokio::test]` runs on a current-thread
/// runtime, so the WARN emitted inline by `build_and_start`'s replication
/// detection lands on this thread and is captured.
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

/// Number of times `needle` appears in a capture buffer.
fn count_occurrences(buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>, needle: &str) -> usize {
    let bytes = buf.lock().unwrap();
    String::from_utf8_lossy(&bytes).matches(needle).count()
}

/// Reads the most-recent value of an `i64` gauge named `name` back out of an
/// in-memory metric exporter, scanning every accumulated `ResourceMetrics` and
/// returning the last matching data point (the newest recorded value). Requires
/// a prior `force_flush` on the provider.
fn gauge_value(
    exporter: &opentelemetry_sdk::metrics::InMemoryMetricExporter,
    name: &str,
) -> Option<i64> {
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    let metrics = exporter.get_finished_metrics().ok()?;
    let mut latest = None;
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::I64(MetricData::Gauge(gauge)) = metric.data()
                    && let Some(dp) = gauge.data_points().last()
                {
                    latest = Some(dp.value());
                }
            }
        }
    }
    latest
}

/// Total recorded sample count of an `f64` histogram named `name` whose data
/// point carries `primitive = <primitive>`, summed across all accumulated
/// `ResourceMetrics`. Requires a prior `force_flush`.
fn histogram_sample_count(
    exporter: &opentelemetry_sdk::metrics::InMemoryMetricExporter,
    name: &str,
    primitive: &str,
) -> u64 {
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    let Ok(metrics) = exporter.get_finished_metrics() else {
        return 0;
    };
    let mut total = 0;
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data()
                {
                    for dp in hist.data_points() {
                        if dp.attributes().any(|kv| {
                            kv.key.as_str() == "primitive"
                                && kv.value.as_str().as_ref() == primitive
                        }) {
                            total += dp.count();
                        }
                    }
                }
            }
        }
    }
    total
}

/// `PG-SPEC-006`: the `cluster_postgres_lock_active_names` gauge tracks the
/// cluster-wide distinct-held-name count (`= count(*)` of `cluster_lock`, not
/// `held.len()`), and the lock reaper logs `cluster.lock.name_cardinality_high`
/// (WARN, once per sweep) while that count is over
/// `lock_name_cardinality_warn_threshold` (DESIGN.md §8).
///
/// The gauge is a plugin-local, non-ADR-004 metric emitted through a meter this
/// plugin owns directly (not `ClusterMetrics`, which has no gauge method). The
/// test injects its own meter over an in-memory reader (via
/// `__with_reaper_meter`) so the readback is isolated from any other test's
/// reaper. The WARN is captured with a process-global subscriber (the reaper's
/// spawned task may run on any thread), asserting on a message unique to this
/// over-threshold condition.
#[tokio::test]
async fn pg_spec_006_lock_name_cardinality_gauge_and_warn_threshold() {
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    let warn_log = install_global_warn_capture();

    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let meter = provider.meter("pg-spec-006");

    // threshold 5 so 6 distinct held names trip the WARN; a short reaper
    // interval so a sweep records the gauge/WARN quickly. The pool is left at
    // its default: a held lock is a `cluster_lock` row and consumes no connection
    // at all (DESIGN.md §3.3), so the only pool users here are the acquiring
    // writes and the reaper's own `count(*)`.
    let (_container, config) = common::start_postgres_lock_only_with(json!({
        "lock_name_cardinality_warn_threshold": 5,
        "lock_reaper_interval_ms": 100,
    }))
    .await;
    let handle = PostgresLockPlugin::builder(config)
        .__with_reaper_meter(meter)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    // Acquire 6 distinct names → 6 rows in cluster_lock → gauge 6 (> threshold 5).
    let mut guards = Vec::new();
    for i in 0..6 {
        guards.push(
            lock.try_lock(&format!("card-{i}"), Duration::from_secs(30))
                .await
                .expect("acquire"),
        );
    }

    // A sweep must record the gauge at 6.
    let reached = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async {
            provider.force_flush().ok();
            gauge_value(&exporter, "cluster_postgres_lock_active_names") == Some(6)
        },
    )
    .await;
    assert!(
        reached,
        "PG-SPEC-006: gauge must reach the 6 distinct held names"
    );
    let warned = {
        let bytes = warn_log.lock().unwrap();
        String::from_utf8_lossy(&bytes).contains("cluster.lock.name_cardinality_high")
    };
    assert!(
        warned,
        "PG-SPEC-006: an over-threshold distinct-name count must emit the \
         cluster.lock.name_cardinality_high WARN"
    );

    // The same sweeps must record the lock reaper's sweep-duration histogram
    // (DESIGN.md §8, primitive=lock).
    provider.force_flush().ok();
    assert!(
        histogram_sample_count(
            &exporter,
            "cluster_postgres_reaper_sweep_duration_seconds",
            "lock",
        ) >= 1,
        "PG-SPEC-006: the lock reaper must record sweep-duration samples (primitive=lock)"
    );

    // Release all → count drops to 0 (≤ threshold) → gauge clears, WARN stops.
    for guard in guards {
        guard.release().await.expect("release");
    }
    let cleared = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async {
            provider.force_flush().ok();
            gauge_value(&exporter, "cluster_postgres_lock_active_names") == Some(0)
        },
    )
    .await;
    assert!(
        cleared,
        "PG-SPEC-006: gauge must clear to 0 once every lock is released"
    );

    // The WARN must *stop* once the count is back under threshold (PGR-L5):
    // snapshot the occurrence count now that the gauge reads 0, then confirm a
    // handful more sweep intervals (100ms each) add none — the message is
    // unique to this over-threshold condition, so its count reflects only this
    // test's reaper.
    let count_at_clear = count_occurrences(&warn_log, "cluster.lock.name_cardinality_high");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        count_occurrences(&warn_log, "cluster.lock.name_cardinality_high"),
        count_at_clear,
        "PG-SPEC-006: the cardinality WARN must stop firing once the distinct-name count drops \
         back under the threshold"
    );

    handle.stop().await;
    let _shutdown = provider.shutdown();
}

/// `PG-SPEC-006` (histogram half): the combined plugin's **cache** and **lock**
/// TTL reapers both record `cluster_postgres_reaper_sweep_duration_seconds`
/// (DESIGN.md §8) under `primitive={cache,lock}`. Each reaper records on every
/// tick regardless of whether the sweep deletes anything, so short intervals +
/// a brief wait produce samples for both primitives. Uses the combined-plugin
/// `__with_reaper_meter` seam so the readback is isolated from other tests.
#[tokio::test]
async fn pg_spec_006b_reaper_sweep_duration_histograms_recorded() {
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let meter = provider.meter("pg-spec-006b");

    let (_container, config) = common::start_postgres_with(
        json!({ "cache_reaper_interval_ms": 50, "lock_reaper_interval_ms": 50 }),
    )
    .await;
    let handle = PostgresClusterPlugin::builder(config)
        .__with_reaper_meter(meter)
        .build_and_start()
        .await
        .unwrap();

    let histogram = "cluster_postgres_reaper_sweep_duration_seconds";
    let both = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async {
            provider.force_flush().ok();
            histogram_sample_count(&exporter, histogram, "cache") >= 1
                && histogram_sample_count(&exporter, histogram, "lock") >= 1
        },
    )
    .await;
    assert!(
        both,
        "PG-SPEC-006: both reapers must record cluster_postgres_reaper_sweep_duration_seconds \
         (primitive=cache and primitive=lock)"
    );

    handle.stop().await;
    let _shutdown = provider.shutdown();
}

/// `PG-SPEC-007`: with no `synchronous_standby_names` configured (the
/// container's default) and `replication_mode` omitted, both the combined and
/// standalone builders detect `Async`, log `cluster.provider.replication_async`
/// (WARN) **exactly once**, and still return `Ok` (never block startup —
/// TESTING.md §4.6). Each build runs under its own thread-local capture scope,
/// so the "exactly once" count is per-build.
#[tokio::test]
async fn pg_spec_007_async_replication_detected_and_warned_combined_and_standalone() {
    {
        let (_guard, warns) = scoped_warn_capture();
        let (_container, config) = common::start_postgres().await;
        let handle = PostgresClusterPlugin::builder(config)
            .build_and_start()
            .await
            .expect(
                "PG-SPEC-007: async replication must only warn, never block startup (combined)",
            );
        assert_eq!(
            count_occurrences(&warns, "cluster.provider.replication_async"),
            1,
            "PG-SPEC-007: the combined plugin must log cluster.provider.replication_async exactly once"
        );
        handle.stop().await;
    }

    {
        let (_guard, warns) = scoped_warn_capture();
        let (_container2, lock_config) = common::start_postgres_lock_only().await;
        let lock_handle = PostgresLockPlugin::builder(lock_config)
            .build_and_start()
            .await
            .expect(
                "PG-SPEC-007: async replication must only warn, never block startup (standalone)",
            );
        assert_eq!(
            count_occurrences(&warns, "cluster.provider.replication_async"),
            1,
            "PG-SPEC-007: the standalone plugin must log cluster.provider.replication_async exactly once"
        );
        lock_handle.stop().await;
    }
}

/// `PG-SPEC-008`: an explicit `replication_mode: sync` short-circuits detection
/// (DESIGN.md §3.6). The container has **no** synchronous standby configured, so
/// had detection run it would have found `Async` and logged
/// `cluster.provider.replication_async`. Asserting that WARN is *absent* is the
/// distinguishing observable that the detection path was skipped — not merely
/// that `build_and_start` returned `Ok` (which it would either way). A
/// thread-local capture scope (not the process-global one) is required so
/// another test's plugin cannot pollute the "absence" assertion.
#[tokio::test]
async fn pg_spec_008_explicit_replication_mode_skips_detection() {
    let (_guard, warns) = scoped_warn_capture();
    let (_container, config) =
        common::start_postgres_with(json!({ "replication_mode": "sync" })).await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .expect(
            "PG-SPEC-008: an explicit replication_mode must not be second-guessed by detection, \
         even though this container has no synchronous standby actually configured",
        );
    assert_eq!(
        count_occurrences(&warns, "cluster.provider.replication_async"),
        0,
        "PG-SPEC-008: an explicit replication_mode must skip Async detection; no \
         cluster.provider.replication_async WARN expected"
    );
    handle.stop().await;
}

/// `PG-SPEC-009`: one lock-reaper sweep clears an expired backlog larger than a
/// single `DELETE` batch, and the wake schedule's next-deadline probe reports the
/// earliest live deadline (DESIGN.md §5.2).
///
/// Driven through the `__test_sweep_once` / `__test_seconds_until_next_expiry`
/// seams rather than by waiting on the reaper's own timer: what's under test is
/// that *one* sweep loops until the table is drained (an unbounded single-statement
/// `DELETE`, or a bounded one without the loop, would both differ observably here),
/// which reaper-interval timing cannot distinguish from "several intervals
/// elapsed". `lock_reaper_interval_ms` is set long so the reaper's own sweeps
/// never race these assertions.
///
/// `fence_retention_ms` is set to a second, and that is not tuning: since item
/// `L3` a lapsed row is *not* sweepable until `expires_at + fence_retention` has
/// passed (§5.8.1), so under the shipped hour-long default the backlog seeded
/// below is a minute expired and correctly untouchable. A short explicit window
/// keeps this a test about batching. It also moves the next-expiry probe, which
/// now reports when the earliest row becomes **reapable** rather than when its
/// lease lapses - asserted with the window added below.
#[tokio::test]
async fn pg_spec_009_expired_backlog_swept_in_bounded_batches() {
    use postgres_cluster_plugin::PostgresLock;
    use std::sync::Arc;

    // 1500 rows spans several `reaper::SWEEP_BATCH` (512) batches, with the last
    // one deliberately partial.
    const BACKLOG: i64 = 1500;

    // One second of retention: long enough that nothing races, short enough that
    // the minute-expired backlog below is genuinely past its window.
    const RETENTION_SECS: f64 = 1.0;

    let (_container, config) = common::start_postgres_lock_only_with(json!({
        "lock_reaper_interval_ms": 600_000,
        "fence_retention_ms": 1_000,
    }))
    .await;
    let connection_string = config.connection_string.clone();
    let schema = config.schema.clone();
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    // The seams live on the concrete backend, as in PG-SPEC-005.
    let lock: Arc<PostgresLock> = handle.__test_lock();
    let lock_backend = handle.lock();
    let control_pool = common::raw_pool(&connection_string).await;

    // An empty table has no next deadline at all.
    assert_eq!(
        lock.__test_seconds_until_next_expiry()
            .await
            .expect("next-expiry probe"),
        None,
        "PG-SPEC-009: with no locks there is no deadline to wake for"
    );

    // Seed already-expired rows as a *foreign* holder would leave them behind — the
    // owner is an identity no live instance here uses, which is exactly what a dead
    // holder's lapsed leases look like. Nothing local accounts for them, and nothing
    // needs to: the sweep's job is purely to drain the metadata table.
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {schema}.cluster_lock \
         (name, owner, fence, acquired_at, expires_at) \
         SELECT 'spec9-' || g, 'spec9-foreign-owner', 1, now() - interval '2 minutes', \
                now() - interval '1 minute' \
         FROM generate_series(1, {BACKLOG}) AS g"
    )))
    .execute(&control_pool)
    .await
    .expect("seed the expired backlog");

    // A row that is *not* expired must survive the sweep and then be what the
    // next-deadline probe reports.
    let _guard = lock_backend
        .try_lock("spec9-live", Duration::from_mins(5))
        .await
        .expect("acquire a live lock");

    let swept = lock.__test_sweep_once().await.expect("sweep");
    assert_eq!(
        swept,
        usize::try_from(BACKLOG).unwrap(),
        "PG-SPEC-009: a single sweep must keep batching until the whole expired \
         backlog is gone, not stop after one bounded DELETE"
    );

    let remaining: Vec<String> = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT name FROM {schema}.cluster_lock"
    )))
    .fetch_all(&control_pool)
    .await
    .expect("count remaining rows");
    assert_eq!(
        remaining,
        vec!["spec9-live".to_owned()],
        "PG-SPEC-009: the sweep must delete every expired row and only those"
    );

    // The live lock's 5-minute TTL is now the earliest (only) deadline, and the probe
    // must report when that row becomes *reapable* - its deadline plus the window -
    // as a delay measured on the database clock.
    let next = lock
        .__test_seconds_until_next_expiry()
        .await
        .expect("next-expiry probe")
        .expect("a live lock must have a deadline");
    assert!(
        (240.0 + RETENTION_SECS..=300.0 + RETENTION_SECS).contains(&next),
        "PG-SPEC-009: the probe must report the live lock's deadline plus the fence \
         retention window, in seconds from the database clock's now(); got {next}"
    );

    handle.stop().await;
}

/// `PG-SPEC-010`: a lock acquired by *this* process is reclaimed at its own TTL
/// even when that TTL is far shorter than `lock_reaper_interval_ms`, because
/// `try_acquire` signals the reaper's `deadline_hint` (DESIGN.md §5.2, "Wake
/// schedule").
///
/// The reaper's sleep is computed from `min(expires_at)` as the table looked at
/// its last wake, so without that signal a lock whose whole lifetime fits inside
/// one sleep would sit expired until the sleep ended. The 10-minute interval here
/// is what makes the assertion meaningful: it is the *only* other thing that could
/// wake the reaper, so a pass means the acquisition itself did.
///
/// This is the case that actually costs something — the holder's session is alive,
/// so its advisory lock is only releasable by this process's own reaper, and until
/// that runs every `try_lock` on the name fails. The guard is deliberately never
/// released: the reaper reclaiming it out from under a live guard is the scenario.
///
/// The 100 ms `fence_retention_ms` is what keeps this a test about the hint. Since
/// item `L3` the gate weighs `ttl + retention` against the interval, because a row
/// is not the reaper's work until its window has also passed — so under the shipped
/// hour-long default no TTL can open it, and this test would be asserting only that
/// acquire steals a lapsed row (which it does, with or without a reaper). A short
/// window puts the mechanism back in play.
#[tokio::test]
async fn pg_spec_010_local_acquire_wakes_the_reaper_before_its_interval() {
    let (_container, config) = common::start_postgres_lock_only_with(json!({
        "lock_reaper_interval_ms": 600_000,
        "fence_retention_ms": 100,
    }))
    .await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    // Let the reaper finish its startup sweep against the still-empty table and
    // commit to its 600s sleep *before* the acquisition below. Without this the
    // test proves nothing: `#[tokio::test]` is a current-thread runtime, so the
    // reaper task first gets scheduled at the test's next await point, which can
    // be inside `try_lock` — its startup probe would then already see this lock's
    // deadline and shorten the sleep on its own.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Acquired *after* the reaper's startup sweep, so its deadline was not in the
    // table when the current sleep was computed.
    let _guard = lock
        .try_lock("spec10", Duration::from_secs(1))
        .await
        .expect("acquire");

    // Well inside the 600s interval: only the acquire-time signal can get the
    // reaper to reclaim this within the window.
    let reclaimed = common::wait_until(Duration::from_secs(15), Duration::from_millis(100), || {
        let lock = std::sync::Arc::clone(&lock);
        async move {
            lock.try_lock("spec10", Duration::from_secs(30))
                .await
                .is_ok()
        }
    })
    .await;
    assert!(
        reclaimed,
        "PG-SPEC-010: a 1s-TTL lock must be reclaimed at its own deadline, not held \
         until the 600s reaper interval elapses; the acquire must wake the reaper"
    );

    handle.stop().await;
}

/// `PG-SPEC-010` (renew half): `renew` also signals the reaper's `deadline_hint`,
/// so shortening a lock's TTL below the reaper's current sleep still gets it
/// reclaimed at the new deadline (DESIGN.md §5.2, "Wake schedule").
///
/// `renew` takes an arbitrary `new_ttl`, so it can move the earliest deadline
/// *earlier* than the sleep in flight was computed with — the acquire-time signal
/// alone would not cover this. Here the lock is acquired with a 5-minute TTL (the
/// reaper commits to sleeping until roughly that), then renewed down to 1s.
///
/// `fence_retention_ms` is short for the same reason as in `PG-SPEC-010`.
#[tokio::test]
async fn pg_spec_010b_renew_wakes_the_reaper_when_it_shortens_a_ttl() {
    let (_container, config) = common::start_postgres_lock_only_with(json!({
        "lock_reaper_interval_ms": 600_000,
        "fence_retention_ms": 100,
    }))
    .await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let lock = handle.lock();

    let guard = lock
        .try_lock("spec10b", Duration::from_mins(5))
        .await
        .expect("acquire");

    // Let the reaper observe the 5-minute deadline and commit to sleeping on it,
    // so the renew below is the only thing that can pull the wake forward (see
    // the current-thread scheduling note in PG-SPEC-010).
    tokio::time::sleep(Duration::from_millis(500)).await;

    guard
        .renew(Duration::from_secs(1))
        .await
        .expect("renew to a shorter TTL");

    let reclaimed = common::wait_until(Duration::from_secs(15), Duration::from_millis(100), || {
        let lock = std::sync::Arc::clone(&lock);
        async move {
            lock.try_lock("spec10b", Duration::from_secs(30))
                .await
                .is_ok()
        }
    })
    .await;
    assert!(
        reclaimed,
        "PG-SPEC-010: renewing down to a 1s TTL must be reclaimed at that new deadline, \
         not at the 5-minute one the reaper was already sleeping on"
    );

    handle.stop().await;
}

/// `PG-SPEC-011`: a server whose default transaction isolation is stricter than
/// `READ COMMITTED` fails `build_and_start` with `InvalidConfig`, and an ordinary
/// `read committed` server starts normally (DESIGN.md §3.2).
///
/// Both primitives arbitrate a claim with a guarded `ON CONFLICT DO UPDATE` whose
/// losing side must re-read the winner's *already-committed* row — `READ COMMITTED`
/// behaviour. Under `REPEATABLE READ` the snapshot cannot advance, so Postgres
/// answers SQLSTATE `40001` instead, and neither the lock's acquire nor the cache's
/// `put_if_absent` retries. The assertion is deliberately at startup: this is the
/// same fail-fast-on-an-incompatible-deployment precedent `PG-LIFE-005` sets for
/// transaction-mode `PgBouncer`.
///
/// Run against the *combined* plugin, so the coverage includes the cache half —
/// the check is shared (`pg_setup::assert_read_committed`) precisely because
/// `put_if_absent` carried this dependency unguarded before the lock did.
#[tokio::test]
async fn pg_spec_011_stricter_isolation_default_rejected_at_startup() {
    let (_container, config) = common::start_postgres().await;
    let connection_string = config.connection_string.clone();

    // A `read committed` server (the stock default) must start normally — asserted
    // first, so a failure below is attributable to the isolation level rather than
    // to anything else about this container.
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .expect("PG-SPEC-011: the stock read-committed default must start normally");
    handle.stop().await;

    let control_pool = common::raw_pool(&connection_string).await;
    sqlx::query(
        "ALTER DATABASE cluster_test SET default_transaction_isolation = 'repeatable read'",
    )
    .execute(&control_pool)
    .await
    .expect("can set the database-level default");
    // Confirm the precondition on a session opened after the ALTER: this scenario
    // is only meaningful if a fresh connection really does inherit the default.
    let fresh_pool = common::raw_pool(&connection_string).await;
    let inherited: String = sqlx::query_scalar("SHOW transaction_isolation")
        .fetch_one(&fresh_pool)
        .await
        .expect("SHOW succeeds");
    assert_eq!(
        inherited, "repeatable read",
        "setup: a fresh session must actually inherit the stricter default"
    );

    let rejected =
        PostgresClusterPlugin::builder(common::cluster_config_json(&connection_string, json!({})))
            .build_and_start()
            .await;
    match rejected {
        Err(ClusterError::InvalidConfig { reason }) => assert!(
            reason.contains("repeatable read"),
            "PG-SPEC-011: the error must name the level it actually found, got {reason:?}"
        ),
        Err(other) => panic!("PG-SPEC-011: expected InvalidConfig, got {other:?}"),
        Ok(_started) => {
            panic!("PG-SPEC-011: a repeatable-read server default must fail build_and_start")
        }
    }
}

/// `PG-SPEC-012`: the acquire path scans `pg_locks` on **no** path — one of `L2`'s
/// exit criteria, asserted against a real query plan rather than against source
/// text (DESIGN.md).
///
/// This assertion inverted when the liveness beacon was removed, and the inversion
/// is the point. It used to check that the predicate's `pg_locks` subplan was
/// *skipped* on the uncontended path — the predicate was written as `CASE`, not
/// `OR`, precisely because SQL does not guarantee left-to-right evaluation of `OR`
/// operands, and the old third case here was a **control** proving the subplan
/// really existed to be skipped. There is no subplan now: the predicate is
/// `expires_at <= now()` alone, so every path must come back clean, and the former
/// control is now the strongest of the three assertions.
///
/// `pg_locks` is a function scan over `pg_lock_status()` with no index, so what this
/// buys is that a contended acquire is no longer `O(advisory locks in the cluster)`
/// (§7.2.4). The scaling risk that cost carried had its own baseline test; it is
/// retired along with the scan.
///
/// Verified with `EXPLAIN ANALYZE` against the statement acquisition actually runs
/// (`__test_explain_acquire` plans the same string), so a regression that
/// reintroduced a liveness join would fail here rather than merely reading oddly.
#[tokio::test]
async fn pg_spec_012_the_acquire_path_never_scans_pg_locks() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let concrete = handle.__test_lock();

    // First acquire: no conflicting row at all, so `ON CONFLICT` never fires.
    let plan = concrete
        .__test_explain_acquire("spec12", Duration::from_millis(1))
        .await
        .expect("EXPLAIN succeeds");
    assert!(
        !pg_locks_was_scanned(&plan),
        "PG-SPEC-012: an uncontended acquire must not scan pg_locks; plan was:\n{plan}"
    );

    // Second acquire: the row above has lapsed (1ms TTL), so `ON CONFLICT` fires and
    // the steal branch runs.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let plan = concrete
        .__test_explain_acquire("spec12", Duration::from_mins(10))
        .await
        .expect("EXPLAIN succeeds");
    assert!(
        !pg_locks_was_scanned(&plan),
        "PG-SPEC-012: a lapsed row must be judged by expires_at alone; plan was:\n{plan}"
    );

    // Third: contending for a *live* row. This is the case that used to reach the
    // beacon liveness branch, so it is the one where a reintroduced join would show
    // up first.
    let plan = concrete
        .__test_explain_acquire("spec12", Duration::from_mins(10))
        .await
        .expect("EXPLAIN succeeds");
    assert!(
        !pg_locks_was_scanned(&plan),
        "PG-SPEC-012: contending for a live row must not consult pg_locks either - the deadline \
         is the only liveness authority; plan was:\n{plan}"
    );

    handle.stop().await;
}

/// Whether an `EXPLAIN ANALYZE` plan shows a `pg_lock_status` scan that actually
/// ran, as opposed to one planned but reported "(never executed)".
///
/// Postgres emits both alternatives of a correlated `NOT EXISTS` subplan, so
/// presence of the scan in the plan text says nothing on its own; only the
/// per-node timing does. A node that ran carries "actual time=". Kept in that exact
/// form even though no such subplan is planned any more: it is what makes the
/// assertion above robust against a regression that reintroduces one.
fn pg_locks_was_scanned(plan: &str) -> bool {
    plan.lines()
        .filter(|line| line.contains("pg_lock_status"))
        .any(|line| line.contains("actual time="))
}

/// `PG-SPEC-013`: `cluster_lock_fence_positive_check` rejects a non-positive fence.
///
/// The Rust side only ever writes `1` (a fresh acquisition) or `fence + 1` (a
/// steal), so this CHECK exists to keep *any other writer* honest — an operator with
/// `psql`, or a future migration. It matters because a zero or negative fence is not
/// merely odd: `FIRST_FENCE` is 1 precisely so that 0 stays available to mean "no
/// lease held", and a stored fence outside the token's `u64` is reported as a
/// provider error rather than silently coerced (`fence_to_u64`).
///
/// It replaces the beacon non-negative CHECK this ID used to cover, whose columns are
/// gone (DESIGN.md).
#[tokio::test]
async fn pg_spec_013_non_positive_fence_is_rejected_by_the_check() {
    let (_container, config) = common::start_postgres_lock_only().await;
    let connection_string = config.connection_string.clone();
    let handle = PostgresLockPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let control_pool = common::raw_pool(&connection_string).await;

    for fence in [0_i64, -1] {
        let rejected = sqlx::query(
            "INSERT INTO cluster_lock (name, owner, fence, expires_at) \
             VALUES ($1, 'spec13-owner', $2, now() + interval '1 minute')",
        )
        .bind(format!("spec13-{fence}"))
        .bind(fence)
        .execute(&control_pool)
        .await;
        let err = rejected.expect_err("PG-SPEC-013: a non-positive fence must be rejected");
        assert_eq!(
            err.as_database_error()
                .and_then(sqlx::error::DatabaseError::code),
            Some(std::borrow::Cow::Borrowed("23514")),
            "PG-SPEC-013: expected a CHECK violation for fence {fence}, got {err:?}"
        );
    }

    control_pool.close().await;
    handle.stop().await;
}

/// `PG-SPEC-014`: `probe()` answers from the real pool on both primitives — `Ok`
/// while the database is reachable, `Err` once it is not.
///
/// Run against a container because the failure direction is the point, and the
/// only honest way to produce it is to take the pool away. The two halves are
/// separate handles on purpose: a `lock: { provider: postgres }` binding opens its
/// own pool and never shares a co-located cache one (DESIGN.md §3.5), so a single
/// probe cannot speak for both, and this is the scenario that proves it.
#[tokio::test]
async fn pg_spec_014_probe_reports_pool_reachability_on_both_primitives() {
    // Cache half, through the combined plugin.
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();
    cache
        .probe()
        .await
        .expect("PG-SPEC-014: probe must succeed against a reachable database");

    // `stop()` closes the pool, so the same backend `Arc` now has nothing to
    // acquire a connection from — the readiness healthcheck's degraded path.
    handle.stop().await;
    let err = cache
        .probe()
        .await
        .expect_err("PG-SPEC-014: probe must fail once the pool is closed");
    assert!(
        matches!(err, ClusterError::Provider { .. }),
        "PG-SPEC-014: a closed pool is a provider error, got {err:?}"
    );

    // Lock half, through the standalone lock-only plugin and its own pool.
    let (_lock_container, lock_config) = common::start_postgres_lock_only().await;
    let lock_handle = PostgresLockPlugin::builder(lock_config)
        .build_and_start()
        .await
        .unwrap();
    let lock = lock_handle.lock();
    lock.probe()
        .await
        .expect("PG-SPEC-014: the standalone lock's own pool must probe Ok");

    lock_handle.stop().await;
    let lock_err = lock
        .probe()
        .await
        .expect_err("PG-SPEC-014: probe must fail once the lock pool is closed");
    assert!(
        matches!(lock_err, ClusterError::Provider { .. }),
        "PG-SPEC-014: a closed lock pool is a provider error, got {lock_err:?}"
    );
}

//! The combined `PostgresClusterPlugin` (cache + lock) builder and lifecycle
//! handle (DESIGN.md §3.2), following the outbox-style builder/handle pattern
//! (ADR-006). Not a `RunnableCapability` — the cluster gear
//! (`cf-gears-cluster`) owns its lifecycle via `build_and_start`/`stop`.

use std::sync::Arc;

use cluster_sdk::observability::otel::OtelClusterMetrics;
use cluster_sdk::{ClusterCacheBackend, ClusterError, ClusterMetrics, InstrumentedCache};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::cache::PostgresCache;
use crate::config::PostgresClusterConfig;
use crate::lock::PostgresLock;
use crate::pg_error::map_sqlx_error;
use crate::pg_setup::{
    assert_read_committed, base_pool_options, cache_migrator, ensure_schema, lock_migrator,
    reject_pgbouncer_transaction_mode, run_migrator, warn_if_async_replication,
};
use crate::provider::PROVIDER_NAME;
use crate::shutdown::{DropDiagnosis, cancel_and_diagnose_drop, close_pool};

/// Entry point for constructing the combined Postgres cluster plugin.
///
/// ```no_run
/// # async fn doc(config: postgres_cluster_plugin::PostgresClusterConfig) -> Result<(), cluster_sdk::ClusterError> {
/// use postgres_cluster_plugin::PostgresClusterPlugin;
/// let handle = PostgresClusterPlugin::builder(config).build_and_start().await?;
/// handle.stop().await;
/// # Ok(())
/// # }
/// ```
pub struct PostgresClusterPlugin;

impl PostgresClusterPlugin {
    // No `#[must_use]` here: `PostgresClusterBuilder` itself already carries a
    // `#[must_use = "..."]` message, so a bare attribute on this function
    // would be a `clippy::double_must_use` no-op.
    pub fn builder(config: PostgresClusterConfig) -> PostgresClusterBuilder {
        PostgresClusterBuilder {
            config,
            reaper_meter: None,
        }
    }
}

/// Fluent builder for [`PostgresClusterPlugin`].
#[must_use = "a builder starts nothing until `.build_and_start()` is called"]
pub struct PostgresClusterBuilder {
    config: PostgresClusterConfig,
    /// Optional override for the meter both TTL reapers emit their plugin-local
    /// gauge/histogram through (DESIGN.md §8). `None` in production (uses the
    /// process-global meter); tests inject a meter over an in-memory reader.
    reaper_meter: Option<opentelemetry::metrics::Meter>,
}

impl PostgresClusterBuilder {
    /// Test-only: routes both reapers' plugin-local metrics through `meter`
    /// instead of the process-global meter, so a test can attach an in-memory
    /// reader and observe the reaper-sweep-duration histogram
    /// (`primitive={cache,lock}`) and the lock active-names gauge in isolation.
    ///
    /// Gated behind `--features integration` (the `tests/` suite's feature) so
    /// this seam is compiled out of release builds entirely (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub fn __with_reaper_meter(mut self, meter: opentelemetry::metrics::Meter) -> Self {
        self.reaper_meter = Some(meter);
        self
    }

    /// Builds the plugin (DESIGN.md §3.2):
    /// 1. Opens `sqlx::PgPool` with `PgPoolOptions` `after_connect`/
    ///    `before_acquire` hooks enforcing `synchronous_commit = on` (§3.4).
    /// 2. Runs **two** embedded `Migrator`s in order — one over
    ///    `migrations/cache/` (`0001_cluster_cache.sql`), one over
    ///    `migrations/lock/` (`0002_cluster_lock.sql`) — each constructed with
    ///    `.set_ignore_missing(true)`. A single `Migrator` over one shared
    ///    folder can't support §3.5's "lock-only migrates only its own table"
    ///    requirement: `Migrator::run` applies every migration it knows about,
    ///    and without `ignore_missing` each `Migrator` would fail validation
    ///    the moment the *other* plugin's version shows up in the shared
    ///    `_sqlx_migrations` tracking table.
    /// 3. Opens a dedicated LISTEN connection outside the pool (§4.3).
    /// 4. Spawns the cache TTL reaper, the lock TTL reaper, and the LISTEN
    ///    fan-out task.
    /// 5. Detects/logs replication topology per §3.6.
    ///
    /// # Errors
    /// - [`ClusterError::InvalidConfig`] if `pgbouncer_transaction_mode: true`
    ///   is set (§5.4) or the connection string is invalid (`PG-LIFE-006`).
    pub async fn build_and_start(self) -> Result<PostgresClusterHandle, ClusterError> {
        let config = self.config;
        let reaper_meter_override = self.reaper_meter;
        reject_pgbouncer_transaction_mode(config.pgbouncer_transaction_mode)?;
        // Reject an unsafe schema (PGR-L4) and any zero reaper/poll interval
        // (PGR-E2) before opening the pool or spawning any reaper.
        config.validate()?;

        let pool = base_pool_options(&config.schema)
            .max_connections(config.pool_max_size)
            .acquire_timeout(config.pool_acquire_timeout())
            .connect(&config.connection_string)
            .await
            .map_err(map_sqlx_error)?;

        // Before any DDL: both primitives' guarded upserts are `READ COMMITTED`
        // idioms, so a stricter server default is a startup error rather than a
        // per-operation serialization failure later (§3.2).
        assert_read_committed(&pool).await?;

        // Create the configured schema (if non-`public`) before the migrators'
        // unqualified `CREATE TABLE`s run — the pool's `search_path` already
        // points every connection at it (PGR-L4).
        ensure_schema(&pool, &config.schema).await?;
        run_migrator(cache_migrator(), &pool).await?;
        run_migrator(lock_migrator(), &pool).await?;
        warn_if_async_replication(&pool, config.replication_mode).await?;

        // The single ADR-004 metrics sink shared across this plugin: the
        // `InstrumentedCache` decorator, the native `PostgresLock` backend
        // (its `try_lock`/`lock`/`renew`/`release` signals, DESIGN.md §8), and
        // both TTL reapers' `emit_provider_error` failure signals (PGR-M1).
        let metrics: Arc<dyn ClusterMetrics> =
            Arc::new(OtelClusterMetrics::from_global_meter(PROVIDER_NAME));

        // Created before the lock so its guard tasks observe the same shutdown
        // signal the reapers/LISTEN tasks do (PGR-L2).
        let shutdown = CancellationToken::new();

        let cache = PostgresCache::new(pool.clone(), &config.schema);
        let lock = PostgresLock::new(crate::lock::LockInit {
            pool: pool.clone(),
            schema: config.schema.clone(),
            reaper_interval: config.lock_reaper_interval(),
            fence_retention: config.fence_retention(),
            metrics: Arc::clone(&metrics),
            provider: PROVIDER_NAME,
            guard_shutdown: shutdown.clone(),
        });
        let cache_dyn = instrumented_cache(&cache, Arc::clone(&metrics));
        // Both TTL reapers emit the plugin-local, non-contract signals of
        // DESIGN.md §8 (the reaper-sweep-duration histogram, plus the lock
        // reaper's active-names gauge) through a meter this plugin owns
        // directly — not the `ClusterMetrics` contract sink (which has no gauge
        // method). One meter, shared, so the `primitive={cache,lock}` histogram
        // is a single instrument.
        let reaper_meter = reaper_meter_override.unwrap_or_else(crate::lock::reaper::reaper_meter);
        let cache_reaper = crate::cache::reaper::spawn_cache_reaper(
            cache.pool(),
            cache.table(),
            config.cache_reaper_interval(),
            crate::lock::reaper::sweep_duration_histogram(&reaper_meter),
            Arc::clone(&metrics),
            PROVIDER_NAME,
            shutdown.clone(),
        );
        let lock_reaper = lock.spawn_reaper(
            crate::lock::reaper::LockReaperMetrics::new(
                &reaper_meter,
                PROVIDER_NAME,
                Arc::clone(&metrics),
            ),
            i64::from(config.lock_name_cardinality_warn_threshold),
            shutdown.clone(),
        );
        // Awaited, not fire-and-forgotten: the LISTEN connection must be
        // confirmed live before `build_and_start` returns (DESIGN.md §3.2
        // step 5's "no readiness gate" guarantee — see
        // `cache::watch::spawn_listen_task`'s doc for the race this closes).
        //
        // If it fails, the cache/lock reapers spawned just above are already
        // running and holding `PgPool` clones; returning the error directly
        // would detach them (nothing cancels `shutdown` on this path, and no
        // handle whose `Drop` calls `stop()` is ever constructed), leaking the
        // tasks and keeping the pool alive until process exit (PGR-L5). Cancel
        // the shared token and await both reapers before propagating.
        let listen_task = match crate::cache::watch::spawn_listen_task(
            config.connection_string.clone(),
            cache.watch_registry(),
            Arc::clone(&metrics),
            PROVIDER_NAME,
            shutdown.clone(),
        )
        .await
        {
            Ok(task) => task,
            Err(err) => {
                shutdown.cancel();
                let _cache_reaper_exited = cache_reaper.await;
                let _lock_reaper_exited = lock_reaper.await;
                // Bounded for the same reason as `stop()`'s close below: this
                // path is reached because the database is already misbehaving.
                close_pool(&pool).await;
                return Err(err);
            }
        };
        let release_listener = lock
            .spawn_release_listener(config.connection_string.clone(), shutdown.clone())
            .await;

        Ok(PostgresClusterHandle {
            cache,
            cache_dyn,
            lock,
            pool,
            cache_reaper: Some(cache_reaper),
            lock_reaper: Some(lock_reaper),
            listen_task: Some(listen_task),
            release_listener: Some(release_listener),
            shutdown,
            stopped: false,
        })
    }
}

/// The running combined plugin. Hands its cache and lock backends to the
/// wiring crate for `ClientHub` registration.
///
/// Call [`stop`](Self::stop) on graceful shutdown (DESIGN.md §10).
pub struct PostgresClusterHandle {
    /// The concrete cache, retained so `stop` can close active watches via the
    /// native watch registry (mirrors `StandaloneClusterHandle`).
    cache: Arc<PostgresCache>,
    /// The same cache as an instrumented trait object, handed to the wiring
    /// crate (DESIGN.md §8).
    cache_dyn: Arc<dyn ClusterCacheBackend>,
    lock: Arc<PostgresLock>,
    pool: PgPool,
    // `Option`, not bare `JoinHandle`s: `PostgresClusterHandle` owns a `Drop`
    // impl below, and you cannot move a field out of a type that implements
    // `Drop` — `stop` uses `.take()` to drain each in place, mirroring
    // `ClusterHandle::stop`'s `std::mem::take` (`cluster/src/wiring.rs`).
    cache_reaper: Option<tokio::task::JoinHandle<()>>,
    lock_reaper: Option<tokio::task::JoinHandle<()>>,
    listen_task: Option<tokio::task::JoinHandle<()>>,
    release_listener: Option<tokio::task::JoinHandle<()>>,
    shutdown: CancellationToken,
    /// Set by `stop` so the `Drop` guard can tell a graceful shutdown apart
    /// from a forgotten one (ADR-006 §Confirmation).
    stopped: bool,
}

impl PostgresClusterHandle {
    /// The instrumented cache backend (DESIGN.md §8).
    #[must_use]
    pub fn cache(&self) -> Arc<dyn ClusterCacheBackend> {
        Arc::clone(&self.cache_dyn)
    }

    /// The native lock backend.
    #[must_use]
    pub fn lock(&self) -> Arc<dyn cluster_sdk::DistributedLockBackend> {
        Arc::clone(&self.lock) as Arc<dyn cluster_sdk::DistributedLockBackend>
    }

    /// Test-only access to the concrete [`PostgresCache`], for `PG-CACHE-010`'s
    /// sweep seam (the `dyn` [`cache`](Self::cache) has no such seam). Mirrors
    /// `PostgresLockHandle::__test_lock`. Gated behind `--features integration`
    /// (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_cache(&self) -> Arc<PostgresCache> {
        Arc::clone(&self.cache)
    }

    /// Test-only access to the write pool, so `PG-CACHE-008`/`008b` can exhaust it
    /// by checking a connection out and holding it.
    ///
    /// Those scenarios used to pin the pool by *holding a lock*, which worked only
    /// because a held lock owned a pool connection. It no longer does (DESIGN.md
    /// §3.3) — and a pool-exhaustion test that depends on the lock primitive to
    /// create the exhaustion was conflating two unrelated things anyway.
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Shuts the plugin down (DESIGN.md §10):
    /// 1. Cancels the shared `CancellationToken`; awaits each background task.
    /// 2. Sends `CacheWatchEvent::Closed(ClusterError::Shutdown)` to all active
    ///    watchers.
    /// 3. Closes both dedicated LISTEN connections — awaiting the cancelled tasks
    ///    in step 1 drops each `PgListener`, which closes its socket. No explicit
    ///    `UNLISTEN *` is issued, because ending the session is equivalent: a
    ///    closed backend cannot deliver further notifications (§10 step 3).
    /// 4. Closes the `sqlx::PgPool`.
    ///
    /// **No lock drain, deliberately.** A step 4 used to hand back every still-held
    /// lock in one beacon-scoped `DELETE` before closing the beacon connection.
    /// Deleting this instance's lease rows on the way out *is* the revocation
    /// DESIGN-DEPLOYABLE-GEAR §5.8.2 exists to prevent, so held leases are now left
    /// exactly where they are and lapse on their own deadlines (invariant I7). The
    /// cost is that a name this instance held stays taken until its TTL instead of
    /// being announced on `cluster_lock_released` the moment we let go — see
    /// `PostgresLockHandle::stop`.
    pub async fn stop(mut self) {
        self.shutdown.cancel();
        // Close all active cache watches terminally *before* awaiting the
        // listen task below, so every watcher observes `Closed(Shutdown)`
        // prior to `stop()` returning (§10 step 2) — `close_all` dispatches
        // directly against the registry, independent of whether the listen
        // task has noticed `cancel` yet.
        self.cache.watch_registry().close_all().await;
        for task in [
            self.cache_reaper.take(),
            self.lock_reaper.take(),
            self.listen_task.take(),
            self.release_listener.take(),
        ]
        .into_iter()
        .flatten()
        {
            let _exited = task.await;
        }
        // Bounded, not a bare `close()`: the guard tasks are detached and share
        // this pool with every cache operation, so an unresponsive backend could
        // otherwise hold `stop()` here indefinitely — see
        // `shutdown::POOL_CLOSE_TIMEOUT`.
        close_pool(&self.pool).await;
        self.stopped = true;
    }
}

/// Diagnostic guard (ADR-006 §Confirmation), mirroring `ClusterHandle`'s own
/// guard (`cluster/src/wiring.rs`) field-for-field: dropping a
/// `PostgresClusterHandle` without calling `stop()` leaks its background
/// tasks (cache TTL reaper, lock TTL reaper, LISTEN fan-out task) — surfaced
/// loudly (debug-build panic / release-build warn-log) rather than silently.
impl Drop for PostgresClusterHandle {
    fn drop(&mut self) {
        // Cancels the shared token *before* the diagnosis below — which this
        // handle previously did not do at all, so a `stop()` future dropped by a
        // supervisor's `timeout(D, handle.stop())` left both reapers and both
        // LISTEN tasks running for the life of the process, each pinning a
        // `PgPool` clone so the pool never closed either. See
        // `shutdown::cancel_and_diagnose_drop` for the full argument; the beacon
        // is deliberately not on this token and is ended by `BeaconHandle::drop`.
        match cancel_and_diagnose_drop(self.stopped, &self.shutdown) {
            DropDiagnosis::StoppedCleanly => {}
            DropDiagnosis::DuringPanic => tracing::warn!(
                "PostgresClusterHandle dropped during panic unwind without stop(); \
                 skipping debug panic to avoid double-panic abort"
            ),
            DropDiagnosis::Unstopped => {
                #[cfg(debug_assertions)]
                panic!("PostgresClusterHandle dropped without stop() - programming error");
                #[cfg(not(debug_assertions))]
                tracing::warn!(
                    "PostgresClusterHandle dropped without stop() - programming error; \
                     background tasks may leak"
                );
            }
        }
    }
}

/// Wraps a native cache in the SDK's `InstrumentedCache` decorator so its
/// operations emit the contracted `cluster.cache.*` signals (DESIGN.md §8),
/// mirroring `StandaloneClusterHandle`'s construction
/// (`standalone-cluster-plugin/src/plugin.rs`).
fn instrumented_cache(
    cache: &Arc<PostgresCache>,
    metrics: Arc<dyn ClusterMetrics>,
) -> Arc<dyn ClusterCacheBackend> {
    Arc::new(InstrumentedCache::new(
        Arc::clone(cache) as Arc<dyn ClusterCacheBackend>,
        PROVIDER_NAME,
        metrics,
    ))
}

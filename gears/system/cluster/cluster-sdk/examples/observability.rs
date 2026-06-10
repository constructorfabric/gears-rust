// Created: 2026-06-18 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-showcase-audit-examples:p1
//! Showcase: observability — emitting the metrics contract *and* custom metrics.
//!
//! Two things in one example:
//!
//! 1. **Wiring the contract.** The raw cache backend is wrapped in the SDK's
//!    [`InstrumentedCache`] decorator, and the default lock / leader backends are
//!    given a metrics sink via `with_observability`. Both carry the bounded
//!    `provider` label (`"example"`). After this, every facade call emits the
//!    contracted `cluster.*` spans and `cluster_*` metrics with no extra work at
//!    the call sites — exactly what a plugin does (the standalone plugin wires
//!    `OtelClusterMetrics` the same way).
//!
//! 2. **Custom, app-defined metrics.** The metrics *sink* is a
//!    [`ClusterMetrics`] implementation the app owns. Because every contract
//!    signal flows through it, the app can derive and emit its own metrics in the
//!    same place — here `app_cache_writes_total`, `app_cas_conflicts_total`, and a
//!    summed `app_cache_latency_seconds_sum`. These are NOT part of the cluster
//!    contract (which is fixed/versioned, ADR-004); they live entirely in the
//!    app's sink.
//!
//! The contract method set on [`ClusterMetrics`] is closed on purpose, so this is
//! the supported way to *augment* contract events with extra signals. To emit a
//! metric driven by genuinely app-specific logic (not a contract event), an app
//! would stand up its own meter alongside — independent of this port.
//!
//! This example uses a custom in-memory sink (printed at the end) rather than the
//! `otel` feature's `OtelClusterMetrics`, so it runs with no exporter wired.
//!
//! Run with: `cargo run --example observability`

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;

use cluster_sdk::cache::{
    CacheConsistency, CacheEntry, CacheFeatures, CacheWatch, CacheWatchEvent,
};
use cluster_sdk::error::{ClusterError, ProviderErrorKind};
use cluster_sdk::leader::{LeaderWatch, LeaderWatchEvent};
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::registration::{
    register_cache_backend, register_leader_election_backend, register_lock_backend,
};
use cluster_sdk::{
    CasBasedDistributedLockBackend, CasBasedLeaderElectionBackend, ClusterCacheBackend,
    ClusterCacheV1, ClusterMetrics, DistributedLockV1, InstrumentedCache, LeaderElectionV1,
    RetryPolicy,
};
use common::MemCacheBackend;
use toolkit::client_hub::ClientHub;

/// The bounded `provider` label for everything this app emits. A `&'static str`,
/// as the SDK's observability seams require.
const PROVIDER: &str = "example";

/// The typed profile the backends are bound under.
#[derive(Clone, Copy)]
struct AppProfile;

impl ClusterProfile for AppProfile {
    const NAME: &'static str = "app";
}

/// A second profile for the watch auto-restart demo, bound to a deliberately
/// flaky cache so its first subscription reconnects.
#[derive(Clone, Copy)]
struct WatchProfile;

impl ClusterProfile for WatchProfile {
    const NAME: &'static str = "watch-demo";
}

/// A [`ClusterMetrics`] sink the app owns. It records the cluster *contract*
/// metrics and, in the same methods, derives a few *custom* app metrics from the
/// contract events. A real deployment would back the contract side with
/// `cluster_sdk::observability::otel::OtelClusterMetrics` (the `otel` feature)
/// and keep its custom counters alongside; this in-memory version just tallies so
/// the example can print them.
#[derive(Default)]
struct AppMetrics {
    // --- contract metrics (the cluster catalog) ---
    cache_ops: Mutex<BTreeMap<(String, String), u64>>,
    lock_ops: Mutex<BTreeMap<(String, String), u64>>,
    leader_transitions: Mutex<BTreeMap<String, u64>>,
    watch_resets: Mutex<BTreeMap<String, u64>>,
    // --- custom, app-defined metrics (NOT part of the contract) ---
    /// Successful mutating cache operations.
    cache_writes: Mutex<u64>,
    /// Optimistic-concurrency conflicts observed (a useful business signal).
    cas_conflicts: Mutex<u64>,
    /// Summed cache-operation latency (a cheap stand-in for a histogram sum).
    cache_latency_total_s: Mutex<f64>,
}

impl AppMetrics {
    fn bump(map: &Mutex<BTreeMap<(String, String), u64>>, op: &str, result: &str) {
        *map.lock()
            .entry((op.to_owned(), result.to_owned()))
            .or_default() += 1;
    }

    fn report(&self) {
        println!("\n=== contract metrics (cluster catalog) ===");
        for ((op, result), n) in self.cache_ops.lock().iter() {
            println!(
                "  cluster_cache_ops_total{{provider=\"{PROVIDER}\", op=\"{op}\", result=\"{result}\"}} = {n}"
            );
        }
        for ((op, result), n) in self.lock_ops.lock().iter() {
            println!(
                "  cluster_lock_ops_total{{provider=\"{PROVIDER}\", op=\"{op}\", result=\"{result}\"}} = {n}"
            );
        }
        for (transition, n) in self.leader_transitions.lock().iter() {
            println!(
                "  cluster_leader_transitions_total{{provider=\"{PROVIDER}\", transition=\"{transition}\"}} = {n}"
            );
        }
        for (primitive, n) in self.watch_resets.lock().iter() {
            println!(
                "  cluster_watch_resets_total{{provider=\"{PROVIDER}\", primitive=\"{primitive}\"}} = {n}"
            );
        }

        println!("\n=== custom app metrics (emitted through the same sink) ===");
        println!("  app_cache_writes_total = {}", *self.cache_writes.lock());
        println!("  app_cas_conflicts_total = {}", *self.cas_conflicts.lock());
        println!(
            "  app_cache_latency_seconds_sum = {:.6}",
            *self.cache_latency_total_s.lock()
        );
    }
}

impl ClusterMetrics for AppMetrics {
    fn cache_op(&self, op: &str, result: &str) {
        // Contract metric.
        Self::bump(&self.cache_ops, op, result);
        // Custom metrics derived from the same contract event.
        if result == "ok" && matches!(op, "put" | "put_if_absent" | "compare_and_swap" | "delete") {
            *self.cache_writes.lock() += 1;
        }
        if result == "conflict" {
            *self.cas_conflicts.lock() += 1;
        }
    }

    fn cache_op_duration(&self, _op: &str, seconds: f64) {
        // Contract metric is a histogram; here we keep a custom running sum.
        *self.cache_latency_total_s.lock() += seconds;
    }

    fn lock_op(&self, op: &str, result: &str) {
        Self::bump(&self.lock_ops, op, result);
    }

    fn lock_op_duration(&self, _op: &str, _seconds: f64) {}

    fn leader_transition(&self, transition: &str) {
        *self
            .leader_transitions
            .lock()
            .entry(transition.to_owned())
            .or_default() += 1;
    }

    fn discovery_op(&self, _op: &str, _result: &str) {}

    fn watch_reset(&self, primitive: &str) {
        *self
            .watch_resets
            .lock()
            .entry(primitive.to_owned())
            .or_default() += 1;
    }

    fn provider_error(&self, _kind: &str) {}
}

/// A cache wrapper whose **first** `watch` hands back a subscription that closes
/// with a *retryable* error, so an `auto_restart`ed consumer reconnects once —
/// driving the `cluster_watch_resets_total` / `cluster.watch.reset` signals.
/// Every other call (including later `watch`es) delegates to the inner backend.
/// A compact stand-in for a real backend's transient connection drop.
struct FlakyWatchOnce {
    inner: Arc<dyn ClusterCacheBackend>,
    tripped: AtomicBool,
}

impl FlakyWatchOnce {
    fn new(inner: Arc<dyn ClusterCacheBackend>) -> Self {
        Self {
            inner,
            tripped: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl ClusterCacheBackend for FlakyWatchOnce {
    fn consistency(&self) -> CacheConsistency {
        self.inner.consistency()
    }
    fn features(&self) -> CacheFeatures {
        self.inner.features()
    }
    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<(), ClusterError> {
        self.inner.put(key, value, ttl).await
    }
    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        self.inner.delete(key).await
    }
    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        self.inner.contains(key).await
    }
    async fn put_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        self.inner.put_if_absent(key, value, ttl).await
    }
    async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<CacheEntry, ClusterError> {
        self.inner
            .compare_and_swap(key, expected_version, new_value, ttl)
            .await
    }
    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        if self.tripped.swap(true, Ordering::SeqCst) {
            // Already tripped once — hand out a real subscription.
            self.inner.watch(key).await
        } else {
            // First subscription: a retryable terminal close so the combinator
            // reconnects (and the reset signals fire).
            let (tx, watch) = CacheWatch::channel(8);
            tx.send(CacheWatchEvent::Closed(ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                message: "demo: transient watch drop".to_owned(),
            }))
            .await
            .ok();
            Ok(watch)
        }
    }
    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        self.inner.watch_prefix(prefix).await
    }
}

#[tokio::main]
async fn main() -> Result<(), ClusterError> {
    let metrics = Arc::new(AppMetrics::default());
    let hub = ClientHub::new();

    // 1. Wrap the raw cache backend in the SDK's InstrumentedCache decorator,
    //    routing its signals to our sink under the `example` provider label.
    let raw: Arc<dyn ClusterCacheBackend> = MemCacheBackend::linearizable();
    let cache_backend: Arc<dyn ClusterCacheBackend> =
        Arc::new(InstrumentedCache::new(raw, PROVIDER, metrics.clone()));

    // 2. Build the default lock + leader backends over the instrumented cache,
    //    each given the same provider label and sink via with_observability. (The
    //    discovery default is wired the same way; omitted here to keep it short.)
    let leader = CasBasedLeaderElectionBackend::new(Arc::clone(&cache_backend))?
        .with_observability(PROVIDER, metrics.clone());
    let lock = CasBasedDistributedLockBackend::new(Arc::clone(&cache_backend))?
        .with_observability(PROVIDER, metrics.clone());

    register_cache_backend(&hub, AppProfile::NAME, Arc::clone(&cache_backend))?;
    register_leader_election_backend(&hub, AppProfile::NAME, Arc::new(leader))?;
    register_lock_backend(&hub, AppProfile::NAME, Arc::new(lock))?;

    // ---- workload: consumers use the facades exactly as normal; emission is
    //      entirely transparent to them. ----

    let cache = ClusterCacheV1::resolver(&hub)
        .profile(AppProfile)
        .resolve()?;
    cache.put("config/region", b"us-east", None).await?;
    let _region = cache.get("config/region").await?;
    let created = cache
        .put_if_absent("counter", b"1", None)
        .await?
        .ok_or_else(|| ClusterError::InvalidConfig {
            reason: "counter unexpectedly already present".to_owned(),
        })?;
    cache
        .compare_and_swap("counter", created.version, b"2", None)
        .await?;
    // A deliberately stale compare-and-swap (the version moved on) — a normal
    // `conflict` outcome that drives the custom `app_cas_conflicts_total`.
    let _stale = cache
        .compare_and_swap("counter", created.version, b"3", None)
        .await;
    println!("[cache] ran put / get / put_if_absent / compare-and-swap (+1 conflict)");

    let lock = DistributedLockV1::resolver(&hub)
        .profile(AppProfile)
        .resolve()?;
    let guard = lock
        .try_lock("rebuild-index", Duration::from_secs(30))
        .await?;
    // A second acquisition of the held lock is contended (a `contended` outcome).
    let _contended = lock
        .try_lock("rebuild-index", Duration::from_secs(30))
        .await;
    guard.release().await?;
    println!("[lock] acquired + contended + released");

    let leader = LeaderElectionV1::resolver(&hub)
        .profile(AppProfile)
        .resolve()?;
    let mut watch = leader.elect("scheduler").await?;
    // Drain the initial status (records the `acquired` transition), then resign
    // (records `resigned`).
    first_status(&mut watch).await?;
    watch.resign().await?;
    println!("[leader] elected + resigned");

    // Watch auto-restart: a transient (retryable) close is absorbed by the
    // combinator, which reconnects and emits `cluster_watch_resets_total` +
    // `cluster.watch.reset`. The flaky cache lives under its own profile so the
    // coordination above is unaffected.
    let watch_cache: Arc<dyn ClusterCacheBackend> = Arc::new(InstrumentedCache::new(
        Arc::new(FlakyWatchOnce::new(MemCacheBackend::linearizable())),
        PROVIDER,
        metrics.clone(),
    ));
    register_cache_backend(&hub, WatchProfile::NAME, watch_cache)?;
    let cache = ClusterCacheV1::resolver(&hub)
        .profile(WatchProfile)
        .resolve()?;
    // A fast policy so the demo doesn't wait the default 1s reconnect backoff.
    let fast = RetryPolicy {
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(1),
        jitter_factor: 0.0,
        max_retries: None,
    };
    let mut restarting = cache.watch("topic").await?.auto_restart(fast);
    if let Some(CacheWatchEvent::Reset) = restarting.recv().await {
        println!("[watch] subscription auto-restarted after a transient close");
    }

    // The contract metrics and the custom app metrics, side by side. Note the
    // cache counts include the lock/leader defaults' *internal* coordination
    // traffic (they run over the instrumented cache): e.g. `put_if_absent` and
    // `compare_and_delete` appear from lock acquisition and the leader's
    // value-guarded release, not just the explicit consumer calls above.
    metrics.report();
    Ok(())
}

/// Awaits the watch's first leadership status, bounded by a timeout so the
/// example never hangs.
async fn first_status(watch: &mut LeaderWatch) -> Result<(), ClusterError> {
    let wait = async {
        loop {
            match watch.changed().await {
                LeaderWatchEvent::Status(_) => return Ok(()),
                LeaderWatchEvent::Closed(err) => return Err(err),
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .map_err(|_elapsed| ClusterError::InvalidConfig {
            reason: "no leadership status within the demo deadline".to_owned(),
        })?
}

//! Layer 2 — conformance suite (docs/TESTING.md §3), wired against a real
//! Postgres container.
//!
//! # Single entry point per suite via an async factory
//!
//! Each suite goes through one shared `run_*_conformance(make, time)` entry
//! point. `make` is an **async** factory (`Fn() -> Future<Output =
//! ScenarioBackend<_>>`) the runner calls once per scenario, so a
//! Postgres-backed backend — whose construction is unavoidably async (opening a
//! pool, running migrations, opening the LISTEN connection) — is built fresh per
//! scenario, and its [`cluster_conformance::ScenarioBackend`] teardown `stop()`s
//! the handle before the next scenario is built.
//!
//! A genuinely fresh backend per scenario is required, not cosmetic — confirmed
//! empirically: a single shared backend failed `SC-CACHE-004`, exhausted the
//! lock pool via intentionally-leaked advisory locks, and left stale leader
//! candidate tasks running across scenarios. All three suites isolate each
//! scenario into its **own Postgres schema** on **one shared container**
//! (`PostgresClusterConfig::schema`, DESIGN.md §7 — routed to an isolated
//! `search_path` via `common::isolated_schema_connection_string`). The lock
//! suite additionally relies on per-scenario teardown draining held advisory
//! locks (`PostgresLock::drain_held`) before the next scenario's handle is
//! built, since `pg_advisory_lock`'s key space is server-wide, not schema-scoped
//! (see `lock_conformance`).
//!
//! # Time-sensitive scenarios run under `TimeControl::Real`, not virtual time
//!
//! A second, orthogonal problem surfaced once the fresh-backend-per-scenario
//! fix above was in place: `SC-CACHE-010/011/014/015`, `SC-LOCK-002/003/005/007`,
//! and `SC-LEAD-003` originally drove time with `tokio::time::pause()` +
//! `advance()`. Against this plugin's real `sqlx::PgPool` a paused virtual clock
//! reliably produced a spurious `Provider { kind: Timeout }` from
//! `pool.acquire()` — confirmed not to be resource exhaustion: the paused
//! runtime auto-advances the clock to the next pending timer deadline while a
//! real `pool.acquire()`'s network I/O is parked, so sqlx's own acquire
//! `tokio::time::timeout` (`sqlx-core/src/pool/inner.rs`) fires immediately even
//! on a free pool (full trace in `docs/GAP-SOLUTIONS.md` §3).
//!
//! The fix (that doc's §3 Proposal A) lives in `cluster-conformance`: the
//! affected scenarios now take a [`cluster_conformance::TimeControl`]. Fixture /
//! in-memory callers pass `Virtual` (unchanged, instant, deterministic); this
//! plugin, running over a real pool, passes `Real`, which swaps the virtual
//! `advance` for a real bounded `tokio::time::sleep` and never pauses the clock,
//! so sqlx's timers behave normally. The reaper-driven scenarios additionally
//! configure a short reaper/sweep interval (so the TTL reclaim actually fires
//! within the real wait), and the reclaim assertions poll rather than
//! single-shot to tolerate reaper-tick jitter.
//!
//! `SC-LEAD-006` is the one exception still not run here — see
//! `leader_conformance`'s doc comment: it is a *virtual-time fault-simulation*
//! scenario (it forces a renewal *miss* to assert `Status(Lost)` re-enrols),
//! which a healthy real backend never exhibits by merely waiting, so it maps to
//! real fault injection (L4/Toxiproxy, `PG-FAULT-007`), not a real sleep.
//!
//! # `discovery_conformance`: now runs (was an SDK gap, now fixed)
//!
//! This was previously `#[ignore]`d: `cluster::defaults::CacheBasedServiceDiscoveryBackend`
//! held a raw cache and called its trait `watch_prefix` directly, which this
//! plugin's `prefix_watch: false` cache answers with `Unsupported`, so `discover`
//! degraded to an always-empty set (`SC-DISC-001` failed on a fresh backend).
//! DESIGN.md §6 claimed the SDK "detects [`prefix_watch: false`] at construction
//! and initialises with `PollingPrefixWatch`", but nothing in the SDK actually
//! did that (GAP-SOLUTIONS.md §4).
//!
//! That is now implemented in the SDK (`cluster::defaults::discovery`): the
//! backend falls back to the `PollingPrefixWatch` polyfill over `scan_prefix`
//! when the cache declares no native prefix watch, so `discover`/`watch` work
//! over this plugin's cache and the suite runs. `SC-DISC-006` (a virtual-time
//! TTL-lapse) is still skipped by `run_discovery_conformance` under `Real`, as
//! for the other suites.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cluster_conformance::{ScenarioBackend, TimeControl};
use postgres_cluster_plugin::PostgresClusterPlugin;

/// Runs every `SC-CACHE-*` scenario through the shared `run_cache_conformance`
/// entry point under [`TimeControl::Real`] (a real `sqlx` pool cannot use a
/// paused clock — see this module's doc comment and `docs/GAP-SOLUTIONS.md` §3).
///
/// The async factory builds a fresh, fully-migrated combined-plugin cache in its
/// own schema on one shared container per scenario, and its
/// [`ScenarioBackend`] teardown `stop()`s that handle before the next scenario
/// is built. A short `cache_reaper_interval_ms` (50ms) makes the TTL sweeper
/// fire within `SC-CACHE-010`'s real wait.
#[tokio::test]
async fn cache_conformance() {
    use cluster_conformance::run_cache_conformance;

    let (_container, base_config) = common::start_postgres().await;
    let base_connection_string = base_config.connection_string;
    let scenario_index = AtomicUsize::new(0);

    run_cache_conformance(
        || {
            let base_connection_string = base_connection_string.clone();
            let index = scenario_index.fetch_add(1, Ordering::Relaxed);
            async move {
                let schema = format!("conformance_cache_{index}");
                let connection_string =
                    common::isolated_schema_connection_string(&base_connection_string, &schema)
                        .await;
                let config = common::cluster_config_for_schema_with(
                    &connection_string,
                    &schema,
                    serde_json::json!({ "cache_reaper_interval_ms": 50 }),
                );
                let handle = PostgresClusterPlugin::builder(config)
                    .build_and_start()
                    .await
                    .expect("fresh per-scenario schema starts");
                let cache = handle.cache();
                ScenarioBackend::with_teardown(cache, async move { handle.stop().await })
            }
        },
        TimeControl::Real,
    )
    .await;
}

/// Runs every `SC-LOCK-*` scenario through the shared `run_lock_conformance`
/// entry point under [`TimeControl::Real`].
///
/// The async factory shares **one** container across scenarios (like cache /
/// leader), each scenario in its own schema (`isolated_schema_connection_string`).
/// This previously used a fresh container **per scenario**: `pg_advisory_lock`'s
/// key space is server-wide (`SET search_path` has no effect on it), and the
/// scenarios reuse the same lock names (`"res"`, `"m"`), so a lock still held
/// past a scenario's teardown would collide with the next. That no longer
/// happens: `PostgresLockHandle::stop` now drains every held lock
/// (`PostgresLock::drain_held`, the `PG-LIFE-003`/§1 fix) — `pg_advisory_unlock`
/// on each pinned connection, synchronously, before `pool.close()` — so the
/// server's advisory locks are clear before the next scenario's fresh handle
/// acquires them. Per-scenario schemas isolate the `cluster_lock` **table**;
/// the drain isolates the server-wide advisory **locks**. Confirmed clean and
/// ~3× faster (1 container, not 6). A short `lock_reaper_interval_ms` (25ms)
/// lets the TTL-reclaim scenarios reclaim within their real waits (harmless for
/// the non-reclaim ones).
#[tokio::test]
async fn lock_conformance() {
    use cluster_conformance::run_lock_conformance;
    use postgres_cluster_plugin::PostgresLockPlugin;

    let (_container, base_config) = common::start_postgres_lock_only().await;
    let base_connection_string = base_config.connection_string;
    let scenario_index = AtomicUsize::new(0);

    run_lock_conformance(
        || {
            let base_connection_string = base_connection_string.clone();
            let index = scenario_index.fetch_add(1, Ordering::Relaxed);
            async move {
                let schema = format!("conformance_lock_{index}");
                let connection_string =
                    common::isolated_schema_connection_string(&base_connection_string, &schema)
                        .await;
                let config = common::lock_config_for_schema_with(
                    &connection_string,
                    &schema,
                    serde_json::json!({ "lock_reaper_interval_ms": 25 }),
                );
                let handle = PostgresLockPlugin::builder(config)
                    .build_and_start()
                    .await
                    .expect("fresh per-scenario schema starts");
                let lock = handle.lock();
                ScenarioBackend::with_teardown(lock, async move { handle.stop().await })
            }
        },
        TimeControl::Real,
    )
    .await;
}

/// Runs the `SC-LEAD-*` scenarios through the shared `run_leader_conformance`
/// entry point under [`TimeControl::Real`], against a fresh
/// `CasBasedLeaderElectionBackend` over its own fresh Postgres cache per scenario
/// (DESIGN.md §6: leader election is always the SDK default over this plugin's
/// cache, never a native implementation).
///
/// `run_leader_conformance` itself skips `SC-LEAD-006` under `Real` — it is a
/// virtual-time fault-simulation (it forces a lease-renewal *miss* to assert a
/// transient `Status(Lost)` re-enrols), which a healthy real backend never
/// exhibits by merely waiting; that property belongs to L4 fault injection
/// (`PG-FAULT-007`), not a real sleep.
#[tokio::test]
async fn leader_conformance() {
    use cluster::defaults::CasBasedLeaderElectionBackend;
    use cluster_conformance::run_leader_conformance;
    use cluster_sdk::LeaderElectionBackend;

    let (_container, base_config) = common::start_postgres().await;
    let base_connection_string = base_config.connection_string;
    let scenario_index = AtomicUsize::new(0);

    run_leader_conformance(
        || {
            let base_connection_string = base_connection_string.clone();
            let index = scenario_index.fetch_add(1, Ordering::Relaxed);
            async move {
                let schema = format!("conformance_leader_{index}");
                let connection_string =
                    common::isolated_schema_connection_string(&base_connection_string, &schema)
                        .await;
                let config = common::cluster_config_for_schema_with(
                    &connection_string,
                    &schema,
                    serde_json::json!({}),
                );
                let handle = PostgresClusterPlugin::builder(config)
                    .build_and_start()
                    .await
                    .expect("fresh per-scenario schema starts");
                let leader = Arc::new(CasBasedLeaderElectionBackend::new(handle.cache()).expect(
                    "SC-LEAD-008: the postgres cache is Linearizable, so the strict constructor must succeed",
                )) as Arc<dyn LeaderElectionBackend>;
                ScenarioBackend::with_teardown(leader, async move { handle.stop().await })
            }
        },
        TimeControl::Real,
    )
    .await;
}

/// `SC-DISC-*` against `CacheBasedServiceDiscoveryBackend` over the real
/// Postgres cache (DESIGN.md §6), under [`TimeControl::Real`].
///
/// Now runs (previously `#[ignore]`d): the SDK default SD backend detects this
/// plugin's `prefix_watch: false` cache and drives its topology watch through
/// the `PollingPrefixWatch` polyfill over `scan_prefix` (GAP-SOLUTIONS.md §4,
/// implemented in `cluster::defaults::discovery`). A short
/// `with_prefix_watch_polling` interval keeps the `watch`-driven scenarios
/// (`SC-DISC-005/007`) fast under real time — the pre-insert path already makes
/// `discover` immediate, but a `watch` genuinely needs a poll tick.
#[tokio::test]
async fn discovery_conformance() {
    use cluster::defaults::CacheBasedServiceDiscoveryBackend;
    use cluster_conformance::run_discovery_conformance;
    use cluster_sdk::ServiceDiscoveryBackend;

    let (_container, base_config) = common::start_postgres().await;
    let base_connection_string = base_config.connection_string;
    let scenario_index = AtomicUsize::new(0);

    run_discovery_conformance(
        || {
            let base_connection_string = base_connection_string.clone();
            let index = scenario_index.fetch_add(1, Ordering::Relaxed);
            async move {
                let schema = format!("conformance_discovery_{index}");
                let connection_string =
                    common::isolated_schema_connection_string(&base_connection_string, &schema)
                        .await;
                // A larger pool than the test default (2): the polyfill's
                // `scan_prefix` + N concurrent `get`s per tick, plus per-instance
                // heartbeat renewals and the cache reaper, would otherwise starve
                // it and make a `get` transiently miss (dropping an instance from
                // the polled view).
                let config = common::cluster_config_for_schema_with(
                    &connection_string,
                    &schema,
                    serde_json::json!({ "pool_max_size": 12 }),
                );
                let handle = PostgresClusterPlugin::builder(config)
                    .build_and_start()
                    .await
                    .expect("fresh per-scenario schema starts");
                let discovery = Arc::new(
                    CacheBasedServiceDiscoveryBackend::new(handle.cache())
                        .with_prefix_watch_polling(std::time::Duration::from_millis(100)),
                ) as Arc<dyn ServiceDiscoveryBackend>;
                ScenarioBackend::with_teardown(discovery, async move { handle.stop().await })
            }
        },
        TimeControl::Real,
    )
    .await;
}

/// End-to-end wiring: an operator who binds only `cache: { provider: postgres }`
/// and lets the omit-default auto-wrap supply service discovery
/// (`ClusterWiring::from_config` → `resolve_profile_backends`) gets a **working**
/// SD backed by the `PollingPrefixWatch` polyfill over the Postgres cache
/// (DESIGN.md §6). This exercises the wiring path that the direct-backend
/// `discovery_conformance` above does not — the auto-wrap constructs the SD
/// default itself, so this proves the fallback is reached through real wiring,
/// not only when a test constructs the backend by hand.
#[tokio::test]
async fn discovery_auto_wrap_over_postgres_cache_via_wiring() {
    use cluster::{ClusterConfig, ClusterWiring, ProviderRegistry};
    use cluster_sdk::discovery::{DiscoveryFilter, ServiceDiscoveryV1, ServiceRegistration};
    use cluster_sdk::profile::ClusterProfile;
    use postgres_cluster_plugin::PostgresCacheProvider;
    use std::collections::HashMap;
    use toolkit::client_hub::ClientHub;

    #[derive(Clone, Copy)]
    struct SdProfile;
    impl ClusterProfile for SdProfile {
        const NAME: &'static str = "sdautowrap";
    }

    let (_container, base_config) = common::start_postgres().await;
    let connection_string = base_config.connection_string;

    let mut profiles = serde_json::Map::new();
    profiles.insert(
        SdProfile::NAME.to_owned(),
        serde_json::json!({
            "cache": {
                "provider": "postgres",
                "connection_string": connection_string,
                "pool_max_size": 8,
            },
        }),
    );
    let cluster_config: ClusterConfig =
        serde_json::from_value(serde_json::json!({ "profiles": profiles }))
            .expect("sd auto-wrap profile config parses");

    let providers = ProviderRegistry::new().with_cache_provider(Arc::new(PostgresCacheProvider));
    let hub = Arc::new(ClientHub::new());
    let handle = ClusterWiring::from_config(Arc::clone(&hub), &cluster_config, &providers)
        .await
        .expect("wiring must resolve SD as the omit-default over the postgres cache");

    let sd = ServiceDiscoveryV1::resolver(&hub)
        .profile(SdProfile)
        .resolve()
        .expect("service-discovery facade resolves for the auto-wrapped profile");

    let reg = ServiceRegistration {
        name: "delivery".to_owned(),
        instance_id: None,
        address: "10.0.0.1:9000".to_owned(),
        metadata: HashMap::new(),
    };
    let svc_handle = sd
        .register(reg)
        .await
        .expect("register must succeed through the auto-wrapped SD");
    let found = sd
        .discover("delivery", DiscoveryFilter::default())
        .await
        .expect("discover must succeed through the auto-wrapped SD");
    assert!(
        found
            .iter()
            .any(|i| i.instance_id == svc_handle.instance_id()),
        "the auto-wrapped, polyfill-backed SD over the postgres cache must discover \
         a registered instance, proving DESIGN.md section 6's omit-default promise holds"
    );

    drop(svc_handle);
    handle.stop().await;
}

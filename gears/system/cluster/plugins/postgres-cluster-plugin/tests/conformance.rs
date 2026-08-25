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
//! empirically: a single shared backend failed `SC-CACHE-004`, accumulated
//! intentionally-leaked locks across scenarios, and left stale leader candidate
//! tasks running across scenarios. All three suites isolate each scenario into
//! its **own Postgres schema** on **one shared container**
//! (`PostgresClusterConfig::schema`, DESIGN.md §7 — routed to an isolated
//! `search_path` via `common::isolated_schema_connection_string`), which now
//! isolates the locks themselves: a lock is a row in that schema's
//! `cluster_lock` table (DESIGN.md §5.1), not an entry in a server-wide advisory
//! key space (see `lock_conformance`).
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
/// This previously used a fresh container **per scenario**, back when locks were
/// `pg_advisory_lock`s: that key space is server-wide (`SET search_path` has no
/// effect on it), and the scenarios reuse the same lock names (`"res"`, `"m"`),
/// so a lock still held past a scenario's teardown collided with the next. Two
/// things have since removed the hazard, in order: `PostgresLockHandle::stop`
/// hands back every held lock before returning (the `PG-LIFE-003`/§1 fix), and
/// locks are no longer keyed in a server-wide namespace at all — a lock is a row
/// in *this schema's* `cluster_lock` table (DESIGN.md §5.1), so per-scenario
/// schemas now isolate the locks themselves rather than only their metadata. Confirmed clean and
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

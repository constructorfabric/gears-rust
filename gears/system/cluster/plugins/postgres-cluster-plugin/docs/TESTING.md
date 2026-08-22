# Testing Strategy — Postgres Cluster Plugin

> **Companion documents:**
> - [DESIGN.md](./DESIGN.md) — implementation design for this plugin
> - [TESTING-STRATEGY.md](../../docs/TESTING-STRATEGY.md) — platform-wide cluster testing strategy (layers, tooling, CI cadence)
> - [Scenario Catalog](../../docs/scenarios/README.md) — `SC-*` IDs referenced below

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Layer 1 — Unit Tests (in-crate)](#2-layer-1--unit-tests-in-crate)
- [3. Layer 2 — Conformance Suite](#3-layer-2--conformance-suite)
- [4. Layer 3 — Integration Tests (testcontainers)](#4-layer-3--integration-tests-testcontainers)
  - [4.1 Container Setup](#41-container-setup)
  - [4.2 Cache Integration Scenarios](#42-cache-integration-scenarios)
  - [4.3 Lock Integration Scenarios](#43-lock-integration-scenarios)
  - [4.4 Watch Integration Scenarios](#44-watch-integration-scenarios)
  - [4.5 Lifecycle Integration Scenarios](#45-lifecycle-integration-scenarios)
  - [4.6 Postgres-specific Scenarios](#46-postgres-specific-scenarios)
- [5. Layer 4 — Fault Injection (Toxiproxy)](#5-layer-4--fault-injection-toxiproxy)
- [6. Static Analysis](#6-static-analysis)
- [7. CI Cadence](#7-ci-cadence)
- [8. Coverage Gaps and Follow-ups](#8-coverage-gaps-and-follow-ups)

<!-- /toc -->

## 1. Overview

> **Branch dependency — resolved:** `docs/TESTING-STRATEGY.md`, `docs/scenarios/`,
> and the `cf-gears-cluster-conformance` crate (`cluster_conformance`) referenced
> throughout this document originated on `feat/cluster-test-strategy`; that
> branch's commit has now been cherry-picked onto this plugin's base branch, so
> the `[dev-dependencies]` entry on `cf-gears-cluster-conformance` (§3) can be
> wired up. `SC-SCOP-001..006` (scoping) have no `cluster_conformance` functions
> and are not expected to gain any — see §3 for why that isn't a gap for this
> plugin. Scenario IDs cited below (`SC-*`) are drawn from `docs/scenarios/` as
> of its current state.

The Postgres plugin testing strategy follows the four-layer pyramid from the platform-wide [TESTING-STRATEGY.md](../../docs/TESTING-STRATEGY.md):

```
L4  Fault injection (Toxiproxy, controlled disconnects)  — nightly
L3  Integration tests (testcontainers Postgres)          — per-PR in this crate, nightly full
L2  Conformance suite (cluster-conformance crate)        — driven by L3 container
L1  Unit tests (co-located, no external dependencies)    — every PR, sub-second
```

The conformance suite (L2) is the keystone: it runs the same scenario body used by the standalone plugin and every other backend against a real Postgres container. Passing the conformance suite is the primary signal that this plugin correctly implements the `ClusterCacheBackend` and `DistributedLockBackend` contracts.

The Postgres-specific layer-3 and layer-4 tests cover behaviours the conformance suite cannot: NOTIFY overflow, the lock TTL and orphan sweeps, the liveness beacon, PgBouncer incompatibility, connection loss and reconnect, and `synchronous_commit` enforcement.

## 2. Layer 1 — Unit Tests (in-crate)

Co-located with source (`src/**/*_tests.rs`). No external dependencies; run with `cargo test -p cf-postgres-cluster-plugin --lib`.

| Module | What is tested |
|---|---|
| `config.rs` | `serde` round-trip for all config fields; default values (incl. `lock_name_cardinality_warn_threshold` defaulting to 1000, `replication_mode` defaulting to `None`); `pgbouncer_transaction_mode: true` rejected at startup; `connection_string` `${VAR}` / `${VAR:-default}` expansion via `ExpandVars`/`config_expanded()` resolves correctly; missing referenced env var surfaces as an error rather than a literal `${VAR}` in the connection string; `replication_mode: async \| sync` round-trips; unknown variant rejected |
| `cache/watch.rs` | Payload parser: `Changed`, `Deleted`, `Expired` round-trip; empty payload mapped to `Reset`; key > 2048 bytes mapped to `InvalidName`. Watch registry, via the **test-only awaited** `dispatch`: per-key fan-out; `Reset` broadcast + clear; a dropped watcher pruned; the typed terminal event still delivered to a full 64-slot buffer (PGR-C4). Watch registry, via the **production** `dispatch_from_listener` (which spawns the `Reset` broadcast rather than awaiting it): `Reset` is actually delivered and `cluster_watch_resets_total` counted once it has; per-key events stay inline; no `Reset` ever follows a terminal `Closed`; `close_all` still closes a watch registered after a `Reset` emptied the registry; a `watch()` after `close_all` gets its terminal event immediately instead of silence; a `Reset` after `close_all` is suppressed *and* uncounted; one terminal broadcast collects every watcher across every key |
| `pg_error.rs` | SQLSTATE `54000` and `23514`-on-a-`*_len_check` mapped to a `Provider` error naming the 2048-byte limit; an unrelated `23514` not mislabelled as a length problem; `28xxx` still mapped to `AuthFailure` |
| `lock/reaper.rs` | The reaper's wake-schedule policy (`next_delay`): with no locks it waits out the metrics cadence; an imminent `min(expires_at)` shortens the sleep; a distant one is still capped by the cadence so the `active_names` gauge cannot go quiet; an already-due or sub-floor deadline is floored at `MIN_WAKE` so staggered deadlines coalesce into one wake; a deadline unrepresentable as a `Duration` falls back rather than panicking |
| `lock/mod.rs` | The acquire predicate's *shape* (`stealable_predicate`): the expiry test is the `CASE`'s first branch and no `OR` appears anywhere in it, so the `pg_locks` scan cannot be evaluated off the uncontended path (`PG-SPEC-012` holds the runtime behaviour to the same thing with `EXPLAIN ANALYZE`); the liveness test correlates against the *conflicting row's own* beacon columns rather than a bind parameter; name-length validation counts UTF-8 bytes; `expires_at_sql` anchors on the database clock; the `deadline_hint` gate (`should_hint`) signals the reaper only for a TTL shorter than the reaper interval, and tracks a reconfigured interval — the unconditional signal it replaced pinned the reaper at its 100 ms wake floor permanently (DESIGN.md §5.2) |
| `lock/beacon.rs` | The beacon key's packed representation round-trips losslessly across the whole non-negative `i32` range; no key a generator can draw collides with the "no beacon" sentinel (a live beacon reading as absent would fail every acquisition); both halves are always non-negative, which is what lets the `pg_locks` comparison skip sign reinterpretation and what `cluster_lock_beacon_nonneg_check` enforces in SQL; two draws differ, since the key's whole job is to identify one *incarnation* |
| `provider.rs` | `ClusterCacheProvider::provider()` and `ClusterLockProvider::provider()` both return `"postgres"`; `build_cache` and `build_lock` each return `InvalidConfig` for an invalid connection string; `build_lock` never receives or depends on a cache backend argument (matches the SDK's "non-cache providers do not receive the cache backend" contract) |

No SQL is executed in layer-1 tests. SQL logic is covered at layer 3.

## 3. Layer 2 — Conformance Suite

`cf-gears-cluster-conformance` is added as a `[dev-dependencies]` entry. The integration test file `tests/conformance.rs` wires a real Postgres container (via the layer-3 fixture below) into every conformance entry point:

Each suite goes through one shared `run_*_conformance(factory, time)` entry point. `factory` is an **async** factory the runner calls once per scenario; it returns a [`cluster_conformance::ScenarioBackend`] that **owns the plugin handle** and tears it down via `stop()` before the next scenario is built. Retaining the handle is mandatory, not cosmetic: `PostgresClusterHandle`/`PostgresLockHandle` panic on `drop` if they were never `stop()`ed (a debug guard against leaking pools, LISTEN connections, and reaper tasks). Returning only `handle.cache()`/`handle.lock()` and dropping the handle would trip that panic and abandon background-task teardown — so the factory must move the handle into `ScenarioBackend::with_teardown`.

```rust
// tests/conformance.rs

use cluster::defaults::CasBasedLeaderElectionBackend;
use cluster_conformance::{
    run_cache_conformance, run_lock_conformance,
    run_leader_conformance, ScenarioBackend, TimeControl,
};
use postgres_cluster_plugin::{PostgresClusterPlugin, PostgresLockPlugin};

#[tokio::test]
async fn cache_conformance() {
    run_cache_conformance(
        || async {
            let handle = PostgresClusterPlugin::builder(test_config())
                .build_and_start()
                .await
                .expect("plugin starts against test container");
            let cache = handle.cache();
            // The fixture owns `handle`; the runner calls the teardown (which
            // `stop()`s it) after the scenario, so the handle is never dropped
            // un-stopped and its background tasks are terminated cleanly.
            ScenarioBackend::with_teardown(cache, async move { handle.stop().await })
        },
        // Real backend → real (bounded) time, never a paused clock (see below).
        TimeControl::Real,
    )
    .await;
}

#[tokio::test]
async fn lock_conformance() {
    run_lock_conformance(
        |_cache| async {
            // Standalone lock-only path (§3.5 DESIGN.md), the same one
            // ClusterLockProvider::build_lock uses in production — not the
            // combined cache+lock plugin. `_cache` is ignored: this exercises
            // the real "independently routable" shape, not a shared-pool
            // shortcut.
            let handle = PostgresLockPlugin::builder(test_lock_config())
                .build_and_start()
                .await
                .expect("standalone lock plugin starts");
            let lock = handle.lock();
            ScenarioBackend::with_teardown(lock, async move { handle.stop().await })
        },
        TimeControl::Real,
    )
    .await;
}

// Leader election here is always `CasBasedLeaderElectionBackend` over this
// plugin's own Postgres cache (DESIGN.md §6), so the fixture still owns the
// underlying cache handle and stops it on teardown.
#[tokio::test]
async fn leader_conformance() {
    run_leader_conformance(
        || async {
            let handle = PostgresClusterPlugin::builder(test_config())
                .build_and_start()
                .await
                .expect("plugin starts against test container");
            let leader = CasBasedLeaderElectionBackend::new(handle.cache()).expect(
                "SC-LEAD-008: the postgres cache is Linearizable, so the strict constructor succeeds",
            );
            ScenarioBackend::with_teardown(leader, async move { handle.stop().await })
        },
        TimeControl::Real,
    )
    .await;
}
```

Time-sensitive scenarios pass `TimeControl::Real` (a bounded real sleep), not virtual time: against this plugin's real `sqlx::PgPool` a paused/auto-advancing clock spuriously fires sqlx's own acquire timeout. In-memory/fixture callers still pass `TimeControl::Virtual`. The runner isolates each scenario into its own Postgres schema on a shared container; see the module docs in `tests/conformance.rs` for the full rationale.

Before this turn, `leader_conformance` was missing entirely — only the cache and lock suites were wired, even though `run_leader_conformance` (`SC-LEAD-001..007`) already exists in `cluster-conformance` and this plugin exposes that primitive (SDK-default-derived, §6). There was no reason not to run it; it's added above.

**Routing conformance is out of scope for this plugin.** `run_routing_conformance` does not exist in `cluster-conformance` and never will — per-primitive routing (`cpt-cf-clst-fr-routing-per-primitive`) is wiring-crate logic owned entirely by `cluster/src/wiring.rs` (`ClusterWiring::from_config` dispatching through `ProviderRegistry`), not backend logic any plugin implements or could meaningfully conformance-test in isolation. That coverage belongs to the `cluster` gear's own test suite (see `PG-LOCK-011` in §4.3 below for this plugin's one routing-adjacent integration test, which exercises the wiring crate end-to-end rather than a `cluster-conformance` entry point).

**Capability-gated assertions.** The conformance suite reads `features()` and `consistency()` from the constructed backend before running scenarios. For this plugin:
- `CacheConsistency::Linearizable` → single-leader and lock-contention correctness scenarios run.
- `CacheFeatures::prefix_watch == false` → `CacheCapability::PrefixWatch` mismatch scenario runs (expects `CapabilityNotMet`); `watch_prefix` returns `Unsupported`.
- `LockFeatures::linearizable == true` → strong-mutual-exclusion scenario runs.
- `LeaderElectionFeatures::linearizable == true` (inherited from the cache, §6) → `SC-LEAD-002`'s single-leader-among-contenders assertion runs, not skipped.

**Why `SC-SCOP-001..006` are not, and don't need to be, `cluster_conformance` functions.** The scenario catalog (`docs/scenarios/scoping.md`) marks these ☐ and `scenarios/README.md:237` lists "Scoping wrappers" as owned by `cluster-conformance`, which reads like a per-backend conformance gap. It isn't one, for this plugin or any other backend: `ScopedCacheBackend`, `ScopedDistributedLockBackend`, and `ScopedLeaderElectionBackend` (`cluster-sdk/src/{cache,lock,leader}/scoped.rs`) are pure decorators — each holds an `Arc<dyn ClusterCacheBackend>` (etc.) and only ever calls the generic trait interface (`scope::apply`/`scope::strip` around a delegated call). None of them touch any backend-specific code path; the wrapped `inner` could be Postgres, standalone, or a test stub, and the prefix-apply/strip/compose logic behaves identically either way. That's exactly why each one already has its own SDK-level unit tests against a `RecordingBackend`/`RecordingCache` stub (`cluster-sdk/src/cache/scoped_tests.rs` and the inline `#[cfg(test)]` modules in `lock/scoped.rs` and `leader/scoped.rs`) — covering prefix prepend, read-path strip, and nested composition — and why `TESTING-STRATEGY.md` §3 (Layer 1) already lists "scoping round-trips" as **implemented** at that layer. Running the identical decorator logic again through `cluster_conformance` against a real Postgres container would re-exercise the same string-manipulation code already proven backend-agnostic; it would not catch anything Postgres-specific, because the decorator never reaches Postgres-specific code. (The one genuinely Postgres-specific interaction — composed scope prefixes making a key long enough to hit this plugin's 2048-byte indexed-key limit, §2.1 DESIGN.md — is already covered directly by `PG-SPEC-002`, independent of whether the long key came from scoping or anywhere else. Note the SDK caps each prefix but not their composition, so that limit is reachable through legitimate nesting.) `scenarios/README.md`'s ownership table is the one worth correcting upstream; nothing is missing from this plugin's own test plan.

## 4. Layer 3 — Integration Tests (testcontainers)

### 4.1 Container Setup

```rust
// tests/common/mod.rs

use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

// Image tags are pinned workspace-wide in `libs/test-containers`; never call
// `Postgres::default()` here (see /docs/TESTING.md §4.4).

pub async fn start_postgres() -> (ContainerAsync<Postgres>, PostgresClusterConfig) {
    let container = test_containers::postgres_named("cluster_test")
        .start()
        .await
        .expect("Postgres container starts");

    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let config = PostgresClusterConfig {
        connection_string: format!(
            "postgres://postgres:postgres@127.0.0.1:{}/cluster_test",
            port
        ),
        pool_max_size: 5,
        ..PostgresClusterConfig::default()
    };
    (container, config)
}

/// Same container, but returns `PostgresLockConfig` (§3.5 DESIGN.md) for tests
/// exercising the standalone lock-only provider path.
pub async fn start_postgres_lock_only() -> (ContainerAsync<Postgres>, PostgresLockConfig) {
    let container = test_containers::postgres_named("cluster_test")
        .start()
        .await
        .expect("Postgres container starts");

    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let config = PostgresLockConfig {
        connection_string: format!(
            "postgres://postgres:postgres@127.0.0.1:{}/cluster_test",
            port
        ),
        pool_max_size: 5,
        ..PostgresLockConfig::default()
    };
    (container, config)
}
```

Each test function starts a fresh container (or reuses a shared one via `once_cell` for the suite run) and runs migrations via `build_and_start`. Containers are dropped at the end of the test function, which shuts down Postgres and takes every `cluster_lock` row and every beacon with it.

### 4.2 Cache Integration Scenarios

These mirror the conformance suite scenarios (§3) but add Postgres-specific assertions.

| ID | Scenario | What it verifies |
|---|---|---|
| `PG-CACHE-001` | `put` + `get` round-trip | Value and version stored and retrieved correctly from the `cluster_cache` table |
| `PG-CACHE-002` | Version increment monotonicity | Each `put` increments `version` by exactly 1; `put_if_absent` sets version to 1 |
| `PG-CACHE-003` | `compare_and_swap` atomicity under concurrent writers | Two goroutines CAS the same key; exactly one wins, one gets `CasConflict` |
| `PG-CACHE-004` | TTL expiry via reaper | Entry absent after reaper runs past `expires_at`; `Expired` event received on watch |
| `PG-CACHE-005` | `compare_and_delete` survives version reset | Delete+recreate sequence; `compare_and_delete` with old value is a no-op; new holder's claim intact |
| `PG-CACHE-006` | `scan_prefix` correctness | All keys under prefix returned; expired keys excluded; keys not under prefix excluded |
| `PG-CACHE-007` | Migration idempotency | `build_and_start` runs against a database that already has the plugin tables; succeeds without error |
| `PG-CACHE-008` | Connection pool exhaustion — blocks then succeeds | The single pool connection is checked out and held via the `__test_pool` seam; a concurrent `put` genuinely blocks, then completes once it is returned. (Previously pinned the pool by *holding a lock*, which only worked while a held lock owned a pool connection — a held lock now owns no connection at all, §3.3, and exercising exhaustion directly no longer conflates the two primitives) |
| `PG-CACHE-008b` | Connection pool exhaustion — timeout path | Same checked-out-connection setup with a short `pool_acquire_timeout_ms`; a cache op that cannot get a connection in time returns `Provider { kind: Timeout }` |
| `PG-CACHE-009` | `put_if_absent` reclaims an expired-but-unreaped row | With the default (slow) reaper, an entry past its TTL still physically present; `put_if_absent` treats it as absent and re-creates it at version 1 (leader-election failover must not wait for the reaper) |
| `PG-CACHE-010` | One cache sweep clears a multi-chunk expired backlog | Plant 600 already-expired rows (> one 512-row `SWEEP_BATCH`) plus a live-TTL and an indefinite entry, then run a single sweep through the `__test_sweep_once` seam: it must delete every expired row and only those, proving the chunk loop drains the table rather than stopping after one bounded `DELETE`. Uses the seam for the same reason as `PG-SPEC-009` — reaper timing cannot distinguish "one sweep looped" from "several intervals elapsed", which is precisely the 512-rows-per-tick behaviour the chunking exists to avoid. A follow-up sweep must reap 0 |
| `PG-CACHE-010b` | Chunks past the first still `NOTIFY` their keys | Per-chunk transactions commit the `NOTIFY`s in several batches, so a watcher on the backlog's *least*-expired key (guaranteed by `ORDER BY expires_at` not to be in the first chunk) must still receive `Expired` — `PG-CACHE-004` only covers a single-row sweep |

### 4.3 Lock Integration Scenarios

| ID | Scenario | What it verifies |
|---|---|---|
| `PG-LOCK-001` | `try_lock` acquires and `release` frees | Lease row written; second `try_lock` returns `LockContended`; after `release`, succeeds. The second `try_lock` comes from the *same* instance and is refused by the same mechanism a foreign one would be — the row lock plus the acquire predicate (DESIGN.md §5.1) |
| `PG-LOCK-002` | `lock` with timeout | Blocked `lock` returns `LockTimeout` after the timeout elapses, and the timed-out waiter leaves nothing behind — the name is acquirable as soon as the holder releases |
| `PG-LOCK-003` | `lock` wakes on explicit release NOTIFY | Blocked `lock` acquires promptly (well under the 250ms heartbeat fallback) after the holder calls `release`; NOTIFY-based wake confirmed. A wake measured at ~one heartbeat means the notification was *missed*, not merely slow — which is how a real ordering bug surfaced: the release-LISTEN task's initial `LISTEN` must be awaited before `build_and_start` returns, or a release landing in that startup window has no subscriber |
| `PG-LOCK-004` | TTL reaper reclaims an expired lock | Lock acquired with short TTL and never released; the row is swept and the name is acquirable again. Note the sweep is promptness, not the guarantee — `PG-LOCK-014` covers the same reclamation with no reaper running at all |
| `PG-LOCK-005` | `renew` extends TTL | Lock acquired; `renew(new_ttl)` pushes `expires_at` out to `now() + new_ttl`; reaper does not release until the new TTL elapses |
| `PG-LOCK-006` | `LockExpired` on renew past TTL | Lock acquired with short TTL; sleep past TTL; `renew` returns `LockExpired` |
| `PG-LOCK-007` | Beacon loss frees every lock before the TTL | The beacon's backend is forcibly closed (`pg_terminate_backend` on the pid read from the plugin itself); a lock held under a **10-minute** TTL becomes acquirable within seconds. The TTL is what makes the assertion sharp: reacquisition that fast can only come from the acquire predicate's liveness branch, not from the sweep doing its ordinary job. This is the mechanism the beacon exists for (DESIGN.md §5.1) |
| `PG-LOCK-008` | Concurrent lockers within one instance — at most one holder | 20 concurrent tasks `try_lock` the same name; exactly one succeeds; all others return `LockContended`. Kept distinct from `PG-LOCK-016` even though both now exercise the same mechanism (the row lock plus the predicate, DESIGN.md §5.1) — that local and cross-instance contention are arbitrated *identically* is the claim worth holding both halves to, and a regression reintroducing any local short-circuit would show up here first |
| `PG-LOCK-009` | `synchronous_commit` enforced on connect and checkout | Start the container with `synchronous_commit = off` as the role/database default; confirm a fresh session really inherits `off`; after `build_and_start`, `SHOW synchronous_commit` on a checked-out write-pool connection reports `on`. The pool is the whole surface now: every lock statement is a pooled one, and the beacon writes nothing (DESIGN.md §3.4) |
| `PG-LOCK-010` | Standalone `PostgresLockPlugin` runs without the cache half | `PostgresLockPlugin::builder(test_lock_config()).build_and_start()` against a fresh DB creates only `cluster_lock` (no `cluster_cache` table, no LISTEN connection for cache watch); `try_lock`/`release` work identically to the combined plugin's lock |
| `PG-LOCK-011` | End-to-end YAML routing: lock on Postgres, cache on a different provider | Via `ClusterWiring::from_config` with a profile binding `cache: { provider: standalone }` and `lock: { provider: postgres, connection_string: ... }`; resolved profile's lock backend writes a real `cluster_lock` row in the test container while its cache backend is the in-process standalone one — confirms `ClusterLockProvider` registration actually makes `provider: postgres` resolvable for `lock` independently of `cache` |
| `PG-LOCK-012` | Held locks consume no connections | With `pool_max_size: 2`, hold 12 locks at once: all 12 succeed, all 12 rows exist, a `renew` still gets a pool connection while they are held, and — the invariant the whole model rests on — the *only* advisory lock granted anywhere is this instance's beacon. Regression test for the pinned-connection model, under which this configuration could hold at most two locks; the advisory-lock assertion additionally guards the "nothing per-lock is ever locked" property (DESIGN.md §5.1) |
| `PG-LOCK-013` | Beacon loss invalidates every held lock, detectably | Hold two locks under 10-minute TTLs, `pg_terminate_backend` the beacon, then assert three things. **`renew` → `LockExpired`**: the rows are untouched by the beacon's death, so an implementation missing the `holder_beacon_*` fence would report a healthy renewal for a lock the fleet can already steal — the one silent-mutual-exclusion-loss path (§5.2). **`local_holders` empties and the epoch advances without being asked**: nothing in this scenario touches either lock, so only the once-per-second ping can discover the loss. **The negative**: both names really are acquirable again, i.e. a lock is not silently *retained* across the loss just because nobody else wants it |
| `PG-LOCK-014` | An expired lock is reclaimed **without its owner** | Two instances, both with a 10-minute reaper interval so neither reaper runs during the test: A holds a 1s-TTL lock and never renews or releases it, and B acquires the name purely on its own acquire predicate. The sharpest statement of what the lease-row model buys, and meaningless against the design it replaced — there, reclamation had to route back through the owning instance, so a stalled reaper on A wedged the name fleet-wide past its TTL with nothing else able to help (§5.1). Also asserts A's `renew` then reports `LockExpired` |
| `PG-LOCK-015` | Orphan sweep reclaims an unregistered row | Plant a row under this instance's beacon with no `local_holders` entry (the state a `try_acquire` cancelled between its committed INSERT and its registration leaves), alongside one legitimately held lock. First asserts the *damage*: the orphan reads as a live holder even to its own writer, wedging the name for the whole fleet until its TTL. Then one sweep pass must reclaim exactly the orphan and leave the held row alone (§5.2). Seam-driven on both sides — the assertion is about the sweep's selectivity, not about hitting a microsecond-wide cancellation window |
| `PG-LOCK-016` | Two instances cannot hold the same lock | Two independent plugin instances on one database: A acquires, B's `try_lock` returns `LockContended`, exactly one `cluster_lock` row exists and its `holder_beacon_*` is A's, and B acquires as soon as A releases. This is the cross-replica guarantee the primitive rests on; asserting on the row's beacon rather than on `pg_locks` puts the assertion on the ownership surface itself (DESIGN.md §5.1) |
| `PG-LOCK-017` | The orphan sweep touches only *this* incarnation's rows | A foreign instance's live lock and a row bearing a beacon key no live instance holds (what a previous incarnation leaves) must both survive a sweep. Getting this wrong in the permissive direction is the dangerous failure: deleting a foreign instance's row hands its live lock to someone else. The sweep's exactness comes entirely from the per-incarnation beacon key (§5.2) |
| `PG-LOCK-018` | The orphan sweep's `acquired_at` fence exempts a fresh acquisition | An unregistered row written *since* the previous wake must survive a sweep — it is indistinguishable from an acquisition that has committed but not yet registered, which is exactly the race the fence closes (§5.2). Driven at the fence rather than at the clock, with a positive control (the same row, swept with a fence past its `acquired_at`, *is* reclaimed) so the first assertion cannot pass for a sweep that never deletes anything |
| `PG-LOCK-019` | `stop()` terminates against an unresponsive database | Hold four locks, `pause` the container so the socket stays open but nothing answers, then `stop()` and assert it returns inside a 30s budget. The failure mode a server-side `statement_timeout` cannot cover — the peer that would enforce it is the one that stopped answering — and `sqlx` applies no read timeout of its own, so without `beacon::STATEMENT_TIMEOUT`, `CONNECT_TIMEOUT`, and a cancellable reconnect this blocks forever (DESIGN.md §3.3). Four locks, not one, so the budget also fails if the single-statement drain regresses to one `pool.acquire()` per lock. Scoped to those bounds: pool statements remain unbounded once checked out, so this does **not** assert that `stop()` is bounded in general (DESIGN.md §11) |
| `PG-LOCK-020` | `lock()` after `stop()` answers immediately with `Shutdown` | After a clean `stop()`, `lock(name, ttl, 30s)` must return `ClusterError::Shutdown` in well under a second rather than retrying a torn-down backend for its whole budget and reporting `LockTimeout` — the error ordinary contention produces, which would leave a caller unable to tell "someone else holds it" from "this backend is gone" (DESIGN.md §5.3). `try_lock` is asserted alongside it, since both now take the same pre-work shutdown check and must agree |
| `PG-LOCK-021` | `stop()` leaves no rows behind, orphans included | Hold three locks with a 10-minute TTL *and* plant one orphaned row, `stop()`, then assert `cluster_lock` is empty. The orphan half is what the beacon-scoped drain buys: a drain that iterates a local map cannot see a row with no live guard, so under the old design that row waited out its TTL even across a clean shutdown (DESIGN.md §10 step 4). It also means a restarted instance does not inherit its predecessor's rows |
| `PG-LOCK-022` | The beacon is established, stamped onto rows, and replaced on loss | After `build_and_start`: the beacon key is granted exactly once and on the beacon's *own* connection (asserted by key, so the assertion cannot be satisfied by some other advisory lock); an acquisition stamps that exact key into its row's `holder_beacon_*`; and after `pg_terminate_backend` on the beacon, a replacement is established under a **different** key. The last is what makes "this row was written by this process, on this connection" provable, and why a pre-disconnect row can never be re-vouched by the successor beacon (DESIGN.md §5.1) |

### 4.4 Watch Integration Scenarios

| ID | Scenario | What it verifies |
|---|---|---|
| `PG-WATCH-001` | `watch(key)` receives `Changed` on `put` | NOTIFY delivered; `CacheWatchEvent::Event(Changed { key })` received within 200ms |
| `PG-WATCH-002` | `watch(key)` receives `Deleted` | `delete` triggers NOTIFY; `Deleted` event received |
| `PG-WATCH-003` | `watch(key)` receives `Expired` | TTL reaper deletes key; `Expired` event received |
| `PG-WATCH-004` | `watch_prefix` returns `Unsupported` | `Err(ClusterError::Unsupported { feature: "prefix_watch" })` returned; `features().prefix_watch == false` |
| `PG-WATCH-005` | `Closed(Shutdown)` on `handle.stop()` | Active watch receives terminal `Closed(Shutdown)` before `stop()` returns |
| `PG-WATCH-006` | No events delivered for different key | Watcher on `"a"` receives no events when `"b"` is written |
| `PG-WATCH-007` | Multiple watchers on same key | Both receive the event; one watcher dropping does not affect the other |

### 4.5 Lifecycle Integration Scenarios

| ID | Scenario | What it verifies |
|---|---|---|
| `PG-LIFE-001` | `build_and_start` runs migrations on fresh DB | Tables created; `build_and_start` returns `Ok` |
| `PG-LIFE-002` | `build_and_start` is idempotent | Called twice against the same DB; second call does not fail or double-create tables |
| `PG-LIFE-003` | `stop` closes pool and LISTEN connection | After `stop`, the Postgres server shows zero connections from the plugin, and no advisory lock remains — including the beacon, which only the connection closing can release |
| `PG-LIFE-004` | `stop` delivers `Closed(Shutdown)` before returning | All active watches observe `Closed(Shutdown)` before `stop().await` resolves |
| `PG-LIFE-005` | PgBouncer transaction mode rejected | Config with `pgbouncer_transaction_mode: true` returns `InvalidConfig` at startup |
| `PG-LIFE-006` | Invalid connection string rejected | `build_and_start` returns `InvalidConfig` immediately, not a timeout |
| `PG-LIFE-007` | `Drop` without `stop()` surfaces loudly (ADR-006) | Build a `PostgresClusterHandle` (and, separately, a standalone `PostgresLockPlugin` handle, §3.5) and drop it without calling `stop()`; debug build panics with the "dropped without stop()" message, release build (`cfg(not(debug_assertions))`) logs the WARN instead; calling `stop()` first and then dropping does neither |
| `PG-LIFE-008` | `Drop` during panic unwind degrades to a warning | Panic inside a closure that owns an un-stopped handle; assert the process does not abort (would happen on a debug-build double panic) and `"skipping debug panic to avoid double-panic abort"` is logged instead of the handle's own panic |

### 4.6 Postgres-specific Scenarios

These cover behaviours unique to the Postgres backend not reachable via the conformance suite.

| ID | Scenario | What it verifies |
|---|---|---|
| `PG-SPEC-001` | NOTIFY empty-payload → `Reset` | Directly inject an empty-payload NOTIFY on `cluster_cache_changes`; verify `CacheWatchEvent::Reset` delivered to all active watchers |
| `PG-SPEC-002` | Key length > 2048 bytes rejected | `put` with a key exceeding the btree index-tuple limit on `cluster_cache.key` returns `InvalidName`; a key at exactly 2048 bytes round-trips through a real server, proving the bound clears both that limit and `cluster_cache_key_len_check` |
| `PG-SPEC-003` | *(retired)* Lock hash collision | Lock names are no longer hashed at all — the name is the `cluster_lock` primary key, compared as text (DESIGN.md §5.1), so the property this covered cannot fail. The ID is retained rather than reused so older references resolve to an explanation |
| `PG-SPEC-004` | Advisory lock released on session disconnect | The *server-side* contract the whole liveness model rests on, asserted at the catalog rather than through plugin behaviour (`PG-LOCK-007` covers that half): terminate the beacon's backend and its key disappears from `pg_locks`, with nothing in application code doing the removing. If Postgres ever stopped honouring this, every downstream assertion would still pass for the wrong reason — the TTL would quietly become the only bound on a crashed holder's lock. Also asserts the successor beacon carries a different key |
| `PG-SPEC-005` | Mid-checkout `synchronous_commit` mutation is corrected | With `pool_max_size: 1` — so the connection handed back is provably the same one — run `SET synchronous_commit = off` on a checked-out pool connection, return it, and take it again: `before_acquire` must have restored `on` (§3.4). The scenario's former second half, covering the dedicated lock session's own re-assertion timer, is retired with that connection: the only long-lived connection the lock opens now is the beacon, which writes nothing and has no durability setting to maintain. `consistency()` remains `Linearizable` throughout |
| `PG-SPEC-006` | Lock-name cardinality gauge and threshold WARN | Configure `lock_name_cardinality_warn_threshold: 5`; acquire locks under 6 distinct names concurrently; verify `cluster_postgres_lock_active_names{provider="postgres"}` reports 6 and `cluster.lock.name_cardinality_high` (WARN) is logged exactly once per reaper interval while the count stays above threshold; verify the gauge and log both clear once held-lock count drops back to or below the threshold |
| `PG-SPEC-007` | Async replication detected and warned | Container has no `synchronous_standby_names` configured (the default); `replication_mode` omitted from config; `build_and_start` (and, separately, standalone `PostgresLockPlugin::build_and_start`, §3.5) both return `Ok`, and `cluster.provider.replication_async` (WARN) is logged exactly once at startup, naming ADR-009 |
| `PG-SPEC-008` | Explicit `replication_mode` skips detection | Set `replication_mode: sync` against a container with no `synchronous_standby_names` configured (i.e. explicit config disagrees with what detection would find); verify no `SHOW synchronous_standby_names` query is issued (e.g. via `pg_stat_statements` — explicit config short-circuits detection) and no WARN is logged |
| `PG-SPEC-009` | Expired backlog swept in bounded batches, and the next-deadline probe | Seed an expired-row backlog larger than one sweep batch (1500 rows > 512) plus one live lock, then run a single sweep through the `__test_sweep_once` seam: it must delete every expired row and only those, proving the batch loop drains the table rather than stopping after one bounded `DELETE` (reaper timing cannot distinguish that from "several intervals elapsed", hence the seam). Also asserts `__test_seconds_until_next_expiry` — the wake schedule's `min(expires_at)` probe (DESIGN.md §5.2) — reports `None` on an empty table and the live lock's own deadline once one is held, since a regression there would otherwise be silent (the reaper just falls back to the fixed interval on error) |
| `PG-SPEC-011` | A stricter isolation default is rejected at startup | `ALTER DATABASE ... SET default_transaction_isolation = 'repeatable read'`, confirm a fresh session inherits it, then `build_and_start` must fail with `InvalidConfig` naming the level it found; the stock `read committed` default must start normally. Both primitives' guarded upserts need the losing side to re-read the winner's committed row, which stricter isolation answers with SQLSTATE `40001` instead (DESIGN.md §5.1). Run against the *combined* plugin, so the coverage includes the cache half — the check is shared precisely because `put_if_absent` carried this dependency unguarded before the lock did |
| `PG-SPEC-012` | The `pg_locks` subplan is skipped off the contended path | `EXPLAIN (ANALYZE, VERBOSE)` of the **real** acquire statement (same string acquisition runs, via a seam) for three cases: no conflicting row, an expired row, and a live contended row. The first two must show no executed `pg_lock_status` scan; the third is the control proving the subplan is there to be skipped rather than never planned. Reading the plan needs care — Postgres emits the correlated `NOT EXISTS` as a pair of alternatives and runs whichever it picks, so `never executed` appears against the unchosen one even on the contended path; the assertion therefore tests whether *any* such scan reports actual timings |
| `PG-SPEC-013` | A negative beacon half is rejected by the CHECK | `INSERT` with `holder_beacon_hi` or `holder_beacon_lo` negative must fail with SQLSTATE `23514`. Not cosmetic: the predicate compares these against `pg_locks`'s unsigned `oid` columns, so a negative half would compare as a large positive `oid`, never match, and make every row carrying one read as unvouched — stealable on sight |
| `PG-SPEC-014` | Contended-acquire cost against a seeded `pg_locks` population | Measures mean contended `try_lock` latency at ~0, 100, 1k, and 5k cluster-wide advisory locks, printed as an artefact rather than asserted against a threshold (a CI container's absolute timings are not a production predictor, and the risk is a *scaling* one). The §6/§11 baseline: read with `-- --nocapture`. On CI hardware it runs ~0.6 ms at a handful of locks to ~2.7 ms at 5000 — linear, as the un-indexed function scan predicts, and far below any plausible lock TTL |
| `PG-SPEC-010` | A local acquire/renew wakes the reaper before its interval | With `lock_reaper_interval_ms: 600000`, wait for the reaper to complete its startup sweep and commit to that sleep, then acquire a 1s-TTL lock: it must be reclaimed at its own deadline (re-acquirable within 15s), which only the acquire-time `deadline_hint` signal can achieve. The `010b` half does the same for `renew` shortening a 5-minute TTL down to 1s, the case the acquire-time signal does not cover. Both were confirmed to fail with the signal removed — note the 500ms settle before the write is load-bearing: `#[tokio::test]` is a current-thread runtime, so without it the reaper's startup probe can run after the write and shorten its own sleep, and the test would pass either way |

## 5. Layer 4 — Fault Injection (Toxiproxy)

These tests run nightly. They require a Toxiproxy sidecar alongside the Postgres container.

| ID | Scenario | Fault | Expected behaviour |
|---|---|---|---|
| `PG-FAULT-001` | LISTEN connection loss → `Reset` | Kill TCP connection to LISTEN connection | All watchers receive `Reset`; plugin reconnects; subsequent events delivered after reconnect |
| `PG-FAULT-002` | Write pool connection loss → `ConnectionLost` error | Kill pool connection mid-query | `get`/`put` returns `Provider { kind: ConnectionLost }`; pool retries on a new connection on next call |
| `PG-FAULT-003` | Latency spike → `PoolTimedOut` | Add 10s latency to all connections; `pool_acquire_timeout_ms = 500` | `get` returns `Provider { kind: Timeout }` after 500ms |
| `PG-FAULT-004` | Reconnect succeeds after transient loss | 2-second TCP blackhole, then restore | Watchers receive `Reset` on disconnect; after restore, receive new events without requiring consumer action |
| `PG-FAULT-005` | Reconnect fails past retry budget | Permanent TCP blackhole | Watchers receive `Closed(Provider { kind: ConnectionLost })` after the retry budget is exhausted |
| `PG-FAULT-006` | NOTIFY queue overflow aborts the writing txn | Generate a sustained NOTIFY flood via direct SQL until the async queue fills | The overflowing `NOTIFY`/commit fails with a queue-full error (Postgres emits no notification); the plugin's recovery is the LISTEN reconnect-then-`Reset` path, not an empty-payload `Reset`. `cluster_watch_resets_total{provider="postgres",primitive="cache"}` increments only when the LISTEN connection itself resets |
| `PG-FAULT-007` | No split-brain under partition (real backend) | 5 independent `CasBasedLeaderElectionBackend` instances, each with its own Postgres connection pool, all electing the same name concurrently; Toxiproxy partitions a random subset of connections mid-run for several TTL intervals, then restores | Sample every candidate's `status()` throughout the run (via `tokio::time`-driven polling, not wall-clock sleeps); at no sampled instant do two candidates report `Leader`. This is the real-backend counterpart to the cataloged (but non-runnable-against-Postgres) `SC-LEAD-010` — see §8 |

## 6. Static Analysis

- **`cargo check`** — must pass with no errors.
- **`cargo clippy`** — no warnings beyond the workspace allow-list.
- **`dylint`** — the workspace `no-remote-in-lock-critical-section` rule is enforced. No remote I/O (SQL queries, NOTIFY, pool acquire) inside a `LockGuard`'s lifetime scope.
- **No serde in SDK contract types** — enforced by the workspace dylint layer rule. The plugin's `config.rs` may use serde; the plugin does NOT add serde derives to any `cluster-sdk` type.
- **`cargo test --doc`** — all doc-test examples compile and pass.

## 7. CI Cadence

| Layer | Trigger | Approx. duration |
|---|---|---|
| L1 unit tests | Every PR | < 5 seconds |
| L2 + L3 integration (testcontainers) | Every PR touching Rust — `make test-cluster-pg`, in `ci.yml`'s `integration` job | ~15–20 seconds |
| L4 fault injection (Toxiproxy) | Nightly; manually triggered for pre-release | ~10–20 minutes |

L3 tests are gated behind the `integration` feature flag so they do not run in workspaces that have not provisioned a Docker daemon:

```toml
[features]
integration = ["testcontainers", "testcontainers-modules"]
```

Run locally with `make test-cluster-pg`, which is exactly what CI runs. Prefer it
over `cargo test -p cf-postgres-cluster-plugin --features integration`: nextest
parallelizes across the test binaries, so the same 150 tests take ~15s rather
than ~77s.

## 8. Coverage Gaps and Follow-ups

| Gap | Severity | Tracking |
|---|---|---|
| `Lagged` watch variant not producible from LISTEN/NOTIFY | No action needed — the LISTEN/NOTIFY path surfaces missed events as `Reset` (on reconnect, or an empty/unrecognized payload), never `Lagged` (DESIGN.md §4.3, ADR-003's overflow mapping); NOTIFY-queue overflow itself aborts the writing transaction rather than delivering any watch event. This is a permanent, backend-specific behavior difference, not a missing test. This row is the resolution | N/A — documented |
| The beacon index decision (DESIGN.md §2.1) | Warning — no index exists on `(holder_beacon_hi, holder_beacon_lo)`, which three statements filter on. Deferred deliberately: the table holds only active locks, so the decision wants real `cluster_postgres_lock_active_names` cardinality rather than a guess, and the index would be write amplification on the acquire path. Nothing tests the sequential scan's cost at high cardinality | Follow-up, gated on production cardinality |
| No signal when a holder's lock is reclaimed out from under it | Warning — reclamation no longer routes through the owning instance, so the `cluster.lock.row_vanished` WARN that used to fire without anyone asking is gone (DESIGN.md §11). Consumers are unaffected (`renew` reports `LockExpired` either way); an operator loses a passive signal. Deliberately not replaced, since reinstating it costs an indexed `SELECT` over every locally-held name on each reaper wake | Accepted |
| Multi-node split-brain test (L5) — **distinct from `PG-FAULT-007` (§5)** | Future — requires an actual multi-node Postgres deployment with streaming replication and a real failover (promote standby), to empirically verify the risk §3.6 only warns about: an async-replicated failover can lose the last few committed transactions, including a currently-held lock/leadership row. `PG-FAULT-007` only partitions client connections to a single, non-replicated node — it cannot exercise this at all, since there's no second node to fail over to | Out of initial scope |
| PgBouncer session-mode pooling integration test | Warning — currently only the transaction-mode rejection is tested (`PG-LIFE-005`); a session-mode round-trip test would validate the positive path — that the beacon and the LISTEN subscriptions actually survive for the connection's session lifetime under session-mode pooling, not just that transaction-mode is rejected | Follow-up |
| Full Postgres server restart/failover scenario — **distinct from `PG-FAULT-007` (§5) and the multi-node row above** | Warning — a single-node container restart (e.g. `docker restart`, not a Toxiproxy network fault and not a multi-node failover): does `build_and_start` recover cleanly against a server that restarted mid-session (migrations still idempotent, watches reconnect, in-flight locks correctly gone since the session died with the restart)? Toxiproxy (L4, §5) only blackholes/delays TCP; it never actually stops the Postgres process | L4/L5 follow-up |
| SC-LEAD-009/SC-LEAD-010 (partition, split-brain) cannot be run against this plugin via `turmoil`, as cataloged | Not this plugin's gap to close, and not fixable by "wiring it up" — neither scenario has a `cluster-conformance` function at all (only `SC-LEAD-001..007` are implemented, now run via `leader_conformance`, §3), and `turmoil`'s model (`TESTING-STRATEGY.md` §6: "3+ nodes... over a shared **simulated** backend") has no way to drive real external TCP to a containerized Postgres server in the first place. A future turmoil-based SC-LEAD-010 would validate `CasBasedLeaderElectionBackend`'s own election/renewal state machine against a mock backend — generic SDK logic, not anything Postgres-specific — so it wouldn't tell you whether *this plugin's* actual CAS implementation stays linearizable under real partition. `PG-FAULT-007` (§5) covers that property directly, against the real backend, using this plugin's existing Toxiproxy infrastructure instead | Covered by `PG-FAULT-007` (real-backend) + future SDK-level turmoil suite (generic-algorithm) — no plugin-side follow-up |
| Concurrency races behind several of the shutdown/terminal fixes have no deterministic seam | Warning — the *invariants* are unit-tested, but the interleavings that used to violate them are not reproducible without an injected pause inside the code under test, and a sleep-based approximation would be a test that cannot fail rather than one that can. Specifically: a `watch()` landing between `drain_senders`' collection and its per-key removal (`cache/watch.rs`); a `try_acquire` registering between the shutdown drain's `DELETE` and its return, which the beacon-scoped drain makes harmless rather than excluded — that row is unvouched the moment the beacon closes (`lock/mod.rs`); and which of a spawned `Reset` and `close_all` wins the terminal mutex. The tests assert the post-conditions that hold under *either* interleaving (no `Reset` after `Closed`; every watcher collected; no watcher registered-then-silently-dropped), which is the property that matters | Accepted — would need a `#[cfg(test)]` pause hook |
| Failure paths reachable only by faulting the database mid-operation | Warning — the shutdown drain failing its `DELETE` or its NOTIFY (`cluster.lock.drain_incomplete`, DESIGN.md §10), the beacon's post-reconnect hand-over failing, `POOL_CLOSE_TIMEOUT` elapsing, and `BeaconHandle::drop` ending the task after a dropped `stop()` future are all correct-by-construction but exercised by no test. Each needs a fault injected at a specific statement rather than at the connection, which the L4 Toxiproxy harness (§5) cannot target — it operates on TCP, not on individual statements | L4 follow-up |
| `scan_prefix` cost-at-scale test for `PollingPrefixWatch` | Warning — DESIGN.md §4.4/§11 flag that `LIKE prefix%` degrades with keyspace size and has no index support, but no integration or load test measures this cost against a realistic keyspace | L3/L4 follow-up |

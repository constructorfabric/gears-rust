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

The Postgres-specific layer-3 and layer-4 tests cover behaviours the conformance suite cannot: NOTIFY overflow, advisory lock TTL reaper, PgBouncer incompatibility, connection loss and reconnect, and `synchronous_commit` enforcement.

## 2. Layer 1 — Unit Tests (in-crate)

Co-located with source (`src/**/*_tests.rs`). No external dependencies; run with `cargo test -p cf-postgres-cluster-plugin --lib`.

| Module | What is tested |
|---|---|
| `config.rs` | `serde` round-trip for all config fields; default values (incl. `lock_name_cardinality_warn_threshold` defaulting to 1000, `replication_mode` defaulting to `None`); `pgbouncer_transaction_mode: true` rejected at startup; `connection_string` `${VAR}` / `${VAR:-default}` expansion via `ExpandVars`/`config_expanded()` resolves correctly; missing referenced env var surfaces as an error rather than a literal `${VAR}` in the connection string; `replication_mode: async \| sync` round-trips; unknown variant rejected |
| `cache/watch.rs` | Payload parser: `Changed`, `Deleted`, `Expired` round-trip; empty payload mapped to `Reset`; key > 7997 bytes mapped to `InvalidName` |
| `lock/mod.rs` | Name → `(key1, key2)` two-argument advisory-lock key mapping is stable across calls; lock name with special characters encodes correctly; `key1`/`key2` derivation exercises the full 64-bit hash (high/low `i32` halves) |
| `provider.rs` | `ClusterCacheProvider::provider()` and `ClusterLockProvider::provider()` both return `"postgres"`; `build_cache` and `build_lock` each return `InvalidConfig` for an invalid connection string; `build_lock` never receives or depends on a cache backend argument (matches the SDK's "non-cache providers do not receive the cache backend" contract) |

No SQL is executed in layer-1 tests. SQL logic is covered at layer 3.

## 3. Layer 2 — Conformance Suite

`cf-gears-cluster-conformance` is added as a `[dev-dependencies]` entry. The integration test file `tests/conformance.rs` wires a real Postgres container (via the layer-3 fixture below) into every conformance entry point:

Each suite goes through one shared `run_*_conformance(factory, time)` entry point. `factory` is an **async** factory the runner calls once per scenario; it returns a [`cluster_conformance::ScenarioBackend`] that **owns the plugin handle** and tears it down via `stop()` before the next scenario is built. Retaining the handle is mandatory, not cosmetic: `PostgresClusterHandle`/`PostgresLockHandle` panic on `drop` if they were never `stop()`ed (a debug guard against leaking pools, LISTEN connections, and reaper tasks). Returning only `handle.cache()`/`handle.lock()` and dropping the handle would trip that panic and abandon background-task teardown — so the factory must move the handle into `ScenarioBackend::with_teardown`.

```rust
// tests/conformance.rs

use cluster::defaults::{CacheBasedServiceDiscoveryBackend, CasBasedLeaderElectionBackend};
use cluster_conformance::{
    run_cache_conformance, run_discovery_conformance, run_lock_conformance,
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

// Service discovery is always `CacheBasedServiceDiscoveryBackend` over this
// plugin's own Postgres cache (DESIGN.md §6); the backend auto-selects
// `PollingPrefixWatch` internally because `prefix_watch == false` (§4.3).
#[tokio::test]
async fn discovery_conformance() {
    run_discovery_conformance(
        || async {
            let handle = PostgresClusterPlugin::builder(test_config())
                .build_and_start()
                .await
                .expect("plugin starts against test container");
            let discovery = CacheBasedServiceDiscoveryBackend::new(handle.cache());
            ScenarioBackend::with_teardown(discovery, async move { handle.stop().await })
        },
        TimeControl::Real,
    )
    .await;
}
```

Time-sensitive scenarios pass `TimeControl::Real` (a bounded real sleep), not virtual time: against this plugin's real `sqlx::PgPool` a paused/auto-advancing clock spuriously fires sqlx's own acquire timeout. In-memory/fixture callers still pass `TimeControl::Virtual`. The runner isolates each scenario into its own Postgres schema on a shared container; see the module docs in `tests/conformance.rs` for the full rationale.

Before this turn, `leader_conformance`/`discovery_conformance` were missing entirely — only the cache and lock suites were wired, even though `run_leader_conformance` (`SC-LEAD-001..007`) and `run_discovery_conformance` already exist in `cluster-conformance` and this plugin exposes both primitives (SDK-default-derived, §6). There was no reason not to run them; they're added above.

**Routing conformance is out of scope for this plugin.** `run_routing_conformance` does not exist in `cluster-conformance` and never will — per-primitive routing (`cpt-cf-clst-fr-routing-per-primitive`) is wiring-crate logic owned entirely by `cluster/src/wiring.rs` (`ClusterWiring::from_config` dispatching through `ProviderRegistry`), not backend logic any plugin implements or could meaningfully conformance-test in isolation. That coverage belongs to the `cluster` gear's own test suite (see `PG-LOCK-011` in §4.3 below for this plugin's one routing-adjacent integration test, which exercises the wiring crate end-to-end rather than a `cluster-conformance` entry point).

**Capability-gated assertions.** The conformance suite reads `features()` and `consistency()` from the constructed backend before running scenarios. For this plugin:
- `CacheConsistency::Linearizable` → single-leader and lock-contention correctness scenarios run.
- `CacheFeatures::prefix_watch == false` → `CacheCapability::PrefixWatch` mismatch scenario runs (expects `CapabilityNotMet`); `watch_prefix` returns `Unsupported`.
- `LockFeatures::linearizable == true` → strong-mutual-exclusion scenario runs.
- `LeaderElectionFeatures::linearizable == true` (inherited from the cache, §6) → `SC-LEAD-002`'s single-leader-among-contenders assertion runs, not skipped.
- `ServiceDiscoveryFeatures::metadata_pushdown == false` → confirmed by the discovery suite.

**Why `SC-SCOP-001..006` are not, and don't need to be, `cluster_conformance` functions.** The scenario catalog (`docs/scenarios/scoping.md`) marks these ☐ and `scenarios/README.md:237` lists "Scoping wrappers" as owned by `cluster-conformance`, which reads like a per-backend conformance gap. It isn't one, for this plugin or any other backend: `ScopedCacheBackend`, `ScopedDistributedLockBackend`, `ScopedLeaderElectionBackend`, and `ScopedServiceDiscoveryBackend` (`cluster-sdk/src/{cache,lock,leader,discovery}/scoped.rs`) are pure decorators — each holds an `Arc<dyn ClusterCacheBackend>` (etc.) and only ever calls the generic trait interface (`scope::apply`/`scope::strip` around a delegated call). None of them touch any backend-specific code path; the wrapped `inner` could be Postgres, standalone, or a test stub, and the prefix-apply/strip/compose logic behaves identically either way. That's exactly why each one already has its own SDK-level unit tests against a `RecordingBackend`/`RecordingCache` stub (`cluster-sdk/src/cache/scoped_tests.rs` and the inline `#[cfg(test)]` modules in `lock/scoped.rs` and `leader/scoped.rs`) — covering prefix prepend, read-path strip, and nested composition — and why `TESTING-STRATEGY.md` §3 (Layer 1) already lists "scoping round-trips" as **implemented** at that layer. Running the identical decorator logic again through `cluster_conformance` against a real Postgres container would re-exercise the same string-manipulation code already proven backend-agnostic; it would not catch anything Postgres-specific, because the decorator never reaches Postgres-specific code. (The one genuinely Postgres-specific interaction — a scope prefix making a key long enough to hit this plugin's 7997-byte NOTIFY-payload limit, §2.3 DESIGN.md — is already covered directly by `PG-SPEC-002`, independent of whether the long key came from scoping or anywhere else.) `scenarios/README.md`'s ownership table is the one worth correcting upstream; nothing is missing from this plugin's own test plan.

## 4. Layer 3 — Integration Tests (testcontainers)

### 4.1 Container Setup

```rust
// tests/common/mod.rs

use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

pub async fn start_postgres() -> (ContainerAsync<Postgres>, PostgresClusterConfig) {
    let container = Postgres::default()
        .with_db_name("cluster_test")
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
    let container = Postgres::default()
        .with_db_name("cluster_test")
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

Each test function starts a fresh container (or reuses a shared one via `once_cell` for the suite run) and runs migrations via `build_and_start`. Containers are dropped at the end of the test function, which shuts down Postgres and auto-releases any held advisory locks.

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
| `PG-CACHE-008` | Connection pool exhaustion — blocks then succeeds | The single pool connection is pinned by a held lock (shared pool, §3.3); a concurrent `put` genuinely blocks, then completes once the lock frees the connection |
| `PG-CACHE-008b` | Connection pool exhaustion — timeout path | Same pinned-connection setup with a short `pool_acquire_timeout_ms`; a cache op that cannot get a connection in time returns `Provider { kind: Timeout }` |
| `PG-CACHE-009` | `put_if_absent` reclaims an expired-but-unreaped row | With the default (slow) reaper, an entry past its TTL still physically present; `put_if_absent` treats it as absent and re-creates it at version 1 (leader-election failover must not wait for the reaper) |

### 4.3 Lock Integration Scenarios

| ID | Scenario | What it verifies |
|---|---|---|
| `PG-LOCK-001` | `try_lock` acquires and `release` frees | Advisory lock held; second `try_lock` returns `LockContended`; after `release`, succeeds |
| `PG-LOCK-002` | `lock` with timeout | Blocked `lock` returns `LockTimeout` after the timeout elapses; advisory lock is not held |
| `PG-LOCK-003` | `lock` wakes on explicit release NOTIFY | Blocked `lock` acquires promptly (< 100ms) after the holder calls `release`; NOTIFY-based wake confirmed |
| `PG-LOCK-004` | TTL reaper releases expired lock | Lock acquired with short TTL; reaper fires; advisory lock released; subsequent `try_lock` succeeds |
| `PG-LOCK-005` | `renew` extends TTL | Lock acquired; `renew(new_ttl)` resets `acquired_at`; reaper does not release until new TTL elapses |
| `PG-LOCK-006` | `LockExpired` on renew past TTL | Lock acquired with short TTL; sleep past TTL; `renew` returns `LockExpired` |
| `PG-LOCK-007` | Connection drop releases advisory lock | Pool connection holding a lock is forcibly closed (via `pg_terminate_backend`); advisory lock auto-released; subsequent `try_lock` succeeds |
| `PG-LOCK-008` | Concurrent lockers — at most one holder | 20 concurrent tasks `try_lock` the same name; exactly one succeeds; all others return `LockContended` |
| `PG-LOCK-009` | `synchronous_commit` enforced on connect and checkout | Start the container with `synchronous_commit = off` as the role/database default; after `build_and_start`, `SHOW synchronous_commit` on a checked-out write-pool connection and on a freshly acquired lock connection both report `on` |
| `PG-LOCK-010` | Standalone `PostgresLockPlugin` runs without the cache half | `PostgresLockPlugin::builder(test_lock_config()).build_and_start()` against a fresh DB creates only `cluster_lock` (no `cluster_cache` table, no LISTEN connection for cache watch); `try_lock`/`release` work identically to the combined plugin's lock |
| `PG-LOCK-011` | End-to-end YAML routing: lock on Postgres, cache on a different provider | Via `ClusterWiring::from_config` with a profile binding `cache: { provider: standalone }` and `lock: { provider: postgres, connection_string: ... }`; resolved profile's lock backend is backed by real advisory locks in the test container while its cache backend is the in-process standalone one — confirms `ClusterLockProvider` registration actually makes `provider: postgres` resolvable for `lock` independently of `cache` |

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
| `PG-LIFE-003` | `stop` closes pool and LISTEN connection | After `stop`, the Postgres server shows zero connections from the plugin; advisory locks released |
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
| `PG-SPEC-002` | Key length > 7997 bytes rejected | `put` with a key exceeding the NOTIFY payload limit returns `InvalidName` |
| `PG-SPEC-003` | Lock hash collision (advisory) | Two lock names with a synthetically forced identical `(key1, key2)` pair produce contention on the two-argument form; verify the collision is detected and documented (manual inspection, not a failure assertion) — real-world collisions are expected to be effectively unreachable given the full 64-bit hash space (§5.1), so this exercises the failure mode mechanically rather than via a naturally occurring collision |
| `PG-SPEC-004` | Advisory lock released on session disconnect | Drop the pool connection holding a `LockGuard` at the Postgres level; advisory lock gone; next `try_lock` succeeds |
| `PG-SPEC-005` | Mid-session `synchronous_commit` mutation is corrected | Directly run `SET synchronous_commit = off` on (a) a write-pool connection while it's idle in the pool, and (b) a pinned lock connection while its lock is held; verify (a) is corrected on its next `before_acquire` checkout and (b) is corrected within one `lock_reaper_interval_ms` sweep (§3.4); `consistency()` remains `Linearizable` throughout — the plugin never observes or reports `off` as a supported state |
| `PG-SPEC-006` | Lock-name cardinality gauge and threshold WARN | Configure `lock_name_cardinality_warn_threshold: 5`; acquire locks under 6 distinct names concurrently; verify `cluster_postgres_lock_active_names{provider="postgres"}` reports 6 and `cluster.lock.name_cardinality_high` (WARN) is logged exactly once per reaper interval while the count stays above threshold; verify the gauge and log both clear once held-lock count drops back to or below the threshold |
| `PG-SPEC-007` | Async replication detected and warned | Container has no `synchronous_standby_names` configured (the default); `replication_mode` omitted from config; `build_and_start` (and, separately, standalone `PostgresLockPlugin::build_and_start`, §3.5) both return `Ok`, and `cluster.provider.replication_async` (WARN) is logged exactly once at startup, naming ADR-009 |
| `PG-SPEC-008` | Explicit `replication_mode` skips detection | Set `replication_mode: sync` against a container with no `synchronous_standby_names` configured (i.e. explicit config disagrees with what detection would find); verify no `SHOW synchronous_standby_names` query is issued (e.g. via `pg_stat_statements` — explicit config short-circuits detection) and no WARN is logged |

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
| L2 + L3 integration (testcontainers) | Every PR in this crate; nightly for all cluster plugins | ~2–5 minutes |
| L4 fault injection (Toxiproxy) | Nightly; manually triggered for pre-release | ~10–20 minutes |

L3 tests are gated behind the `integration` feature flag so they do not run in workspaces that have not provisioned a Docker daemon:

```toml
[features]
integration = ["testcontainers", "testcontainers-modules"]
```

Run locally with: `cargo test -p cf-postgres-cluster-plugin --features integration`.

## 8. Coverage Gaps and Follow-ups

| Gap | Severity | Tracking |
|---|---|---|
| `Lagged` watch variant not producible from LISTEN/NOTIFY | No action needed — the LISTEN/NOTIFY path surfaces missed events as `Reset` (on reconnect, or an empty/unrecognized payload), never `Lagged` (DESIGN.md §4.3, ADR-003's overflow mapping); NOTIFY-queue overflow itself aborts the writing transaction rather than delivering any watch event. This is a permanent, backend-specific behavior difference, not a missing test. This row is the resolution | N/A — documented |
| Multi-node split-brain test (L5) — **distinct from `PG-FAULT-007` (§5)** | Future — requires an actual multi-node Postgres deployment with streaming replication and a real failover (promote standby), to empirically verify the risk §3.6 only warns about: an async-replicated failover can lose the last few committed transactions, including a currently-held lock/leadership row. `PG-FAULT-007` only partitions client connections to a single, non-replicated node — it cannot exercise this at all, since there's no second node to fail over to | Out of initial scope |
| PgBouncer session-mode pooling integration test | Warning — currently only the transaction-mode rejection is tested (`PG-LIFE-005`); a session-mode round-trip test would validate the positive path — that advisory locks actually survive for the connection's session lifetime under session-mode pooling, not just that transaction-mode is rejected | Follow-up |
| Full Postgres server restart/failover scenario — **distinct from `PG-FAULT-007` (§5) and the multi-node row above** | Warning — a single-node container restart (e.g. `docker restart`, not a Toxiproxy network fault and not a multi-node failover): does `build_and_start` recover cleanly against a server that restarted mid-session (migrations still idempotent, watches reconnect, in-flight locks correctly gone since the session died with the restart)? Toxiproxy (L4, §5) only blackholes/delays TCP; it never actually stops the Postgres process | L4/L5 follow-up |
| SC-LEAD-009/SC-LEAD-010 (partition, split-brain) cannot be run against this plugin via `turmoil`, as cataloged | Not this plugin's gap to close, and not fixable by "wiring it up" — neither scenario has a `cluster-conformance` function at all (only `SC-LEAD-001..007` are implemented, now run via `leader_conformance`, §3), and `turmoil`'s model (`TESTING-STRATEGY.md` §6: "3+ nodes... over a shared **simulated** backend") has no way to drive real external TCP to a containerized Postgres server in the first place. A future turmoil-based SC-LEAD-010 would validate `CasBasedLeaderElectionBackend`'s own election/renewal state machine against a mock backend — generic SDK logic, not anything Postgres-specific — so it wouldn't tell you whether *this plugin's* actual CAS implementation stays linearizable under real partition. `PG-FAULT-007` (§5) covers that property directly, against the real backend, using this plugin's existing Toxiproxy infrastructure instead | Covered by `PG-FAULT-007` (real-backend) + future SDK-level turmoil suite (generic-algorithm) — no plugin-side follow-up |
| `scan_prefix` cost-at-scale test for `PollingPrefixWatch` | Warning — DESIGN.md §4.4/§11 flag that `LIKE prefix%` degrades with keyspace size and has no index support, but no integration or load test measures this cost against a realistic keyspace | L3/L4 follow-up |

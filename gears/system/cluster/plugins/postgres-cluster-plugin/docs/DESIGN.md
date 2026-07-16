# Technical Design — Postgres Cluster Plugin

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Role in the Cluster Architecture](#11-role-in-the-cluster-architecture)
  - [1.2 Primitive Coverage](#12-primitive-coverage)
- [2. Domain Model](#2-domain-model)
  - [2.1 Database Tables](#21-database-tables)
  - [2.2 Version Semantics](#22-version-semantics)
  - [2.3 NOTIFY Payload Format](#23-notify-payload-format)
- [3. Component Model](#3-component-model)
  - [3.1 Crate Structure](#31-crate-structure)
  - [3.2 Builder / Handle Lifecycle](#32-builder--handle-lifecycle)
  - [3.3 Connection Pool Split](#33-connection-pool-split)
  - [3.4 synchronous_commit Enforcement](#34-synchronous_commit-enforcement)
  - [3.5 Standalone Lock Provider](#35-standalone-lock-provider)
  - [3.6 Replication Topology Warning](#36-replication-topology-warning)
- [4. Cache Implementation](#4-cache-implementation)
  - [4.1 SQL Contract per Operation](#41-sql-contract-per-operation)
  - [4.2 TTL Reaper](#42-ttl-reaper)
  - [4.3 Watch via LISTEN / NOTIFY](#43-watch-via-listen--notify)
  - [4.4 scan_prefix](#44-scan_prefix)
  - [4.5 Consistency Declaration](#45-consistency-declaration)
- [5. Distributed Lock Implementation](#5-distributed-lock-implementation)
  - [5.1 Advisory Lock Mapping](#51-advisory-lock-mapping)
  - [5.2 TTL Enforcement](#52-ttl-enforcement)
  - [5.3 Blocking lock()](#53-blocking-lock)
  - [5.4 PgBouncer Constraint](#54-pgbouncer-constraint)
- [6. Leader Election and Service Discovery](#6-leader-election-and-service-discovery)
- [7. Configuration](#7-configuration)
- [8. Observability](#8-observability)
- [9. ProviderErrorKind Mapping](#9-providererrorkind-mapping)
- [10. Shutdown Sequence](#10-shutdown-sequence)
- [11. Risks / Trade-offs](#11-risks--trade-offs)
- [12. Open Questions](#12-open-questions)

<!-- /toc -->

## 1. Overview

`cf-postgres-cluster-plugin` is the Postgres backend plugin for the cluster gear. It provides a native `ClusterCacheBackend` over a `sqlx::PgPool` and a native `DistributedLockBackend` over PostgreSQL session-level advisory locks. Leader election and service discovery are derived from the SDK default backends over the Postgres cache — no additional tables or connections are required for those two primitives.

The plugin is the recommended deployment for **multi-instance, no-K8s** environments (DESIGN §4.2): Postgres is already deployed in every Gears environment, zero new infrastructure is required, and the native `pg_advisory_lock` gives ACID-correct mutual exclusion without a distributed lock service.

### 1.1 Role in the Cluster Architecture

The plugin satisfies `cpt-cf-clst-component-plugins` for the Postgres backend. It:

- Implements `ClusterCacheProvider` (the provider trait from `cluster-sdk`) so the wiring crate can instantiate the cache from operator YAML (`cache: { provider: postgres }`).
- Implements `ClusterLockProvider` so the wiring crate can *independently* instantiate the native lock from operator YAML (`lock: { provider: postgres }`), whether or not `cache` in the same profile is also bound to postgres — see §3.5. This is what makes the native lock actually reachable via YAML; without it, the wiring's per-primitive routing (`cpt-cf-clst-fr-routing-per-primitive`, already implemented in `cluster/src/wiring.rs`) has nothing registered under `provider: postgres` for the `lock` primitive to dispatch to.
- Exposes a builder/handle pair (`PostgresClusterPlugin::builder(...).build_and_start() -> PostgresClusterHandle`) following the outbox-style lifecycle pattern (DESIGN §3.7, ADR-006). It is NOT a `RunnableCapability`; the cluster gear (`cf-gears-cluster`) owns its lifecycle.
- Returns a `StopHook` from `build_cache` (and, independently, from `build_lock` — §3.5) that shuts down the relevant connection pool and all background tasks it owns.

### 1.2 Primitive Coverage

| Primitive | Implementation | Consistency | `*Features` |
|---|---|---|---|
| `ClusterCacheBackend` | Native — `cluster_cache` table + LISTEN/NOTIFY | `Linearizable` | `prefix_watch: false` (LISTEN channel is key-exact; `watch_prefix` returns `Unsupported`) |
| `LeaderElectionBackend` | SDK default `CasBasedLeaderElectionBackend` over Postgres cache | Inherits cache — `linearizable: true` | — |
| `DistributedLockBackend` | Native — `pg_advisory_lock` + `cluster_lock` metadata table. Independently routable via `lock: { provider: postgres }` (§3.5), with its own pool/config — not required to be paired with the postgres cache provider | `linearizable: true` | — |
| `ServiceDiscoveryBackend` | SDK default `CacheBasedServiceDiscoveryBackend` over Postgres cache | — | `metadata_pushdown: false` |

`prefix_watch: false` means that consumers requiring `CacheCapability::PrefixWatch` cannot bind this backend without the polyfill. The service-discovery default backend uses `watch_prefix` internally and therefore falls back to `PollingPrefixWatch` on a prefix-watch-incapable cache; the wiring crate enables this fallback automatically (see §6).

## 2. Domain Model

### 2.1 Database Tables

Two tables are owned by this plugin, plus one virtual NOTIFY channel. All live in the schema specified by the plugin config (default: `public`). Migration is managed via `sqlx-macros` embedded migrations; the wiring crate runs them at startup before registering backends.

#### `cluster_cache`

```sql
CREATE TABLE cluster_cache (
    key        TEXT        NOT NULL,
    value      BYTEA       NOT NULL,
    version    BIGINT      NOT NULL DEFAULT 1,
    expires_at TIMESTAMPTZ,
    PRIMARY KEY (key)
);

CREATE INDEX cluster_cache_expires_idx ON cluster_cache (expires_at)
    WHERE expires_at IS NOT NULL;
```

`key` is the fully-qualified backend key (scope prefix already applied by `ScopedCacheBackend`). `version` starts at 1 on first insert and increments by 1 on every successful write (including CAS). `expires_at IS NULL` means no TTL. The partial index on `expires_at` makes the TTL reaper's scan efficient.

#### `cluster_lock`

```sql
CREATE TABLE cluster_lock (
    name        TEXT        NOT NULL,
    holder_id   TEXT        NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ttl_ms      BIGINT      NOT NULL,
    PRIMARY KEY (name)
);
```

Stores lock metadata alongside the advisory lock. `ttl_ms` lets the TTL reaper identify expired locks. `holder_id` is a random UUID generated at acquire time; `release()` guards on it to prevent a foreign holder from releasing another's lock.

#### `cluster_lock_notify` (virtual — no table)

The Postgres NOTIFY channel `cluster_lock_released` carries the lock name when a holder calls `release()` explicitly. Blocked `lock()` waiters LISTEN on this channel to wake immediately rather than polling.

### 2.2 Version Semantics

Version starts at 1 on first insert and increments by 1 on every successful write. This matches the SDK contract (DESIGN §3.1 `CacheEntry`): version 0 is reserved as the "absent" sentinel; `put_if_absent` returns version 1; each subsequent write increments by 1. The version column is a plain `BIGINT` updated via `version = version + 1` in the UPDATE path — it does not use a global `BIGSERIAL` sequence; each key's counter is independent.

The `compare_and_delete` operation is value-guarded (not version-guarded): `DELETE … WHERE key = $1 AND value = $2`. This survives the delete+recreate version-reset scenario documented in the SDK (DESIGN §3.3, `[cluster-cache-version-reset-caveat]`): a successor that re-claimed after a TTL lapse writes a different value, so the guarded delete is a safe no-op and never wipes the successor's claim.

### 2.3 NOTIFY Payload Format

Postgres caps a NOTIFY payload at 7999 bytes (`MAX_NOTIFY_PAYLOAD_LENGTH` in `src/backend/commands/async.c` — the "8 KB" of folklore rounds up to a nearby power of two but overstates the real hard limit by 193 bytes; verified empirically, see `PG-SPEC-002`). The plugin's cache watch events carry only the key and event type, never the value (DESIGN §2.1 Lightweight Notifications). Payload format:

```
<event_type>:<key>
```

Where `<event_type>` is one of `C` (Changed), `D` (Deleted), `E` (Expired). The key must therefore be ≤ 7997 bytes (7999-byte payload limit minus the two-byte `<event_type>:` prefix); the plugin validates this constraint at write time (`cache::watch::MAX_KEY_BYTES`) and returns `ClusterError::InvalidName` for keys that would exceed it.

An empty payload — a bare `NOTIFY cluster_cache_changes` (no payload) from an unrelated writer, or any value this plugin's own version never produces — is interpreted by the LISTEN task as a `Reset` signal, broadcasting `CacheWatchEvent::Reset` to all active watchers so consumers re-read their keys (ADR-003 §"NOTIFY overflow mapping"). Note this is *not* how NOTIFY queue overflow surfaces: Postgres does not emit an empty-payload notification on overflow — it aborts the committing *producer* transaction with an error ("too many notifications in the NOTIFY queue") and broadcasts nothing. Overflow does not inherently disconnect the LISTEN connection or increment `cluster_watch_resets_total`; it surfaces on the write side as the failing write's `Provider` error. Reserve reconnect/`Reset` for actual LISTEN connection gaps (below); monitor overflow via write/provider errors and PostgreSQL server logs.

## 3. Component Model

### 3.1 Crate Structure

```
cf-postgres-cluster-plugin/
  src/
    lib.rs          — public API re-exports
    config.rs       — PostgresClusterConfig, PostgresLockConfig, PostgresClusterOptions (serde)
    provider.rs     — ClusterCacheProvider impl ("postgres") + ClusterLockProvider impl ("postgres")
    plugin.rs       — PostgresClusterPlugin, builder, handle (combined cache+lock)
    cache/
      mod.rs        — PostgresCache (ClusterCacheBackend impl)
      watch.rs      — LISTEN connection + per-watcher fan-out
      reaper.rs     — TTL sweeper background task
    lock/
      mod.rs        — PostgresLock (DistributedLockBackend impl); PostgresLockPlugin, builder,
                       handle (standalone lock-only construction, §3.5)
      reaper.rs     — advisory lock TTL reaper
    migrations/     — two independent embedded `sqlx::migrate!()` Migrators, not
                       one shared Migrator over one folder — see below
      cache/
        0001_cluster_cache.sql
      lock/
        0002_cluster_lock.sql
  docs/
    DESIGN.md       — this document
    TESTING.md
```

`0002_cluster_lock.sql` is applied via its own `Migrator` (embedded from `migrations/lock/`, separately from `migrations/cache/`), run whether the plugin is started via the combined `PostgresClusterPlugin` (cache + lock, which runs both Migrators in order) or the standalone `PostgresLockPlugin` (§3.5, which runs only the lock one) — either path only ever runs the migrations its own tables need, so a lock-only deployment never creates `cluster_cache`.

This split is required, not cosmetic: `Migrator::run` unconditionally applies every migration it was embedded with, so a single `Migrator` over one shared folder containing both files cannot support "lock-only migrates only its own table" — running it from the standalone lock plugin would apply `0001_cluster_cache.sql` too. Both Migrators write into the same database's single `_sqlx_migrations` tracking table (there is one table per database, not per `Migrator`), so each is constructed with `.set_ignore_missing(true)`: without it, a `Migrator` that only knows about its own file fails `Migrator::run`'s built-in `validate_applied_migrations` check the moment the *other* plugin's version is already recorded there. `CREATE TABLE IF NOT EXISTS` is deliberately **not** used in either migration file — `sqlx::migrate!()`'s version tracking plus its per-run advisory lock (`Migrator::run`'s `conn.lock()`) already guarantee each file's SQL executes at most once per database, which is what backs `PG-LIFE-002`/`PG-CACHE-007`'s idempotency requirement; adding `IF NOT EXISTS` on top would silently mask a real schema-drift bug (e.g. a manually created table with a stale schema) instead of surfacing `MigrateError::VersionMismatch`.

**Why `sqlx` directly, not `libs/toolkit-db`.** This plugin uses `sqlx::PgPool`/`PgPoolOptions`/`sqlx::migrate!()` directly rather than going through `libs/toolkit-db`'s Sea-ORM/`SecureConn` abstraction — already designated at the SDK level (`cluster/docs/DESIGN.md` §3.5: "External backend libraries… belong to the follow-up plugin crates… and are NOT SDK dependencies"). This isn't a convenience shortcut around the platform's normal "route DB access through `SecureConn`" rule (`docs/toolkit_unified_system/11_database_patterns.md`); it's because three things this plugin needs have no `sea_orm::DatabaseConnection` equivalent to route through in the first place:
- **Session-pinned advisory locks** (§3.3, §5.1–5.3): `pg_advisory_lock`/`pg_advisory_unlock` must run on the exact same physical connection for the lock's full (arbitrary) duration. `DatabaseConnection`'s only own-a-connection primitive is a transaction, and abusing a long-lived transaction for this collides with the PgBouncer-transaction-mode incompatibility this plugin already rejects at startup (§5.4).
- **`LISTEN`/`NOTIFY` streaming** (§4.3): there is no Sea-ORM concept of a subscribed, long-lived notification stream; this is a raw `sqlx::postgres::PgListener`/`PgConnection` API with nothing to wrap.
- **`PgPoolOptions::after_connect`/`before_acquire` hooks** (§3.4, enforcing `synchronous_commit = on` per ADR-009): pool-lifecycle hooks are configured at `sqlx` pool-construction time — even Sea-ORM's own Postgres connector (`SqlxPostgresConnector::from_sqlx_postgres_pool`) takes an already-built `sqlx::PgPool` as input, so there's no lower layer to intercept this from Sea-ORM's side.

The repo's `DE0706_NO_DIRECT_SQLX` dylint lint (`Deny`-level, bans raw `sqlx` usage outside `libs/toolkit-db/`) carries a matching exclusion for `gears/system/cluster/plugins/postgres-cluster-plugin/` (`tools/dylint_lints/lint_utils::is_in_postgres_cluster_plugin_path`) with the same rationale, so this plugin's `sqlx` usage is a documented, lint-sanctioned exception rather than a violation to suppress case-by-case.

### 3.2 Builder / Handle Lifecycle

`ClusterCacheProvider::build_cache` (`cluster-sdk`) is `async fn` — the
provider traits are `#[async_trait]` precisely because most real backends
(Postgres, Redis, NATS, etcd) need genuinely async setup (connection pools,
migrations, subscribe handshakes) to build their backend. The wiring crate
calls every provider from an already-`async fn` context
(`RunnableCapability::start` → `ClusterWiring::from_config`), so
`build_cache`/`build_and_start` can simply `.await` that setup inline:

```rust
pub struct PostgresClusterPlugin;

impl PostgresClusterPlugin {
    pub fn builder(config: PostgresClusterConfig) -> PostgresClusterBuilder;
}

pub struct PostgresClusterBuilder { /* config */ }

impl PostgresClusterBuilder {
    pub async fn build_and_start(self) -> Result<PostgresClusterHandle, ClusterError>;
}

pub struct PostgresClusterHandle {
    cache:  Arc<PostgresCache>,
    lock:   Arc<PostgresLock>,
    /* pool, listen_conn, background tasks */
    /// Set by `stop` so the `Drop` guard can tell a graceful shutdown apart
    /// from a forgotten one (ADR-006 §Confirmation).
    stopped: bool,
}

impl PostgresClusterHandle {
    pub fn cache(&self)  -> Arc<dyn ClusterCacheBackend>;
    pub fn lock(&self)   -> Arc<dyn DistributedLockBackend>;
    pub async fn stop(mut self);
}

/// Diagnostic guard (ADR-006 §Confirmation), mirroring `ClusterHandle`'s own
/// guard (`cluster/src/wiring.rs`) field-for-field: dropping a
/// `PostgresClusterHandle` without calling `stop()` leaks its background
/// tasks (cache TTL reaper, lock TTL reaper, LISTEN fan-out task) — surfaced
/// loudly (debug-build panic / release-build warn-log) rather than silently.
/// The `std::thread::panicking()` check skips the debug panic during unwind
/// so a forgotten handle dropped *while already panicking* degrades to a
/// warning instead of a double-panic process abort (ADR-002).
impl Drop for PostgresClusterHandle {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        if std::thread::panicking() {
            tracing::warn!(
                "PostgresClusterHandle dropped during panic unwind without stop(); \
                 skipping debug panic to avoid double-panic abort"
            );
            return;
        }
        #[cfg(debug_assertions)]
        panic!("PostgresClusterHandle dropped without stop() - programming error");
        #[cfg(not(debug_assertions))]
        tracing::warn!(
            "PostgresClusterHandle dropped without stop() - programming error; \
             background tasks may leak"
        );
    }
}
```

`build_and_start`:
1. Opens `sqlx::PgPool` with the configured pool size (`PgPoolOptions::connect`,
   `.await`ed).
2. Runs the embedded migrations (`.await`ed, idempotent).
3. Opens a dedicated LISTEN connection outside the pool (`.await`ed).
4. Spawns the cache TTL reaper, the lock TTL reaper, and the LISTEN fan-out
   task.
5. Returns the handle. By the time `build_and_start` resolves, the schema
   exists and the LISTEN connection is live — there is no readiness gate or
   background-init race for callers to reason about, unlike a design built
   around a synchronous builder.

`stop`:
1. Cancels all `CancellationToken`s; awaits background tasks.
2. Sends `CacheWatchEvent::Closed(ClusterError::Shutdown)` to all active watchers.
3. Closes the LISTEN connection and the pool.
4. Sets `self.stopped = true` as the last step — graceful shutdown completed, so the `Drop` guard above must not fire.

### 3.3 Connection Pool Split

| Connection type | Purpose | Pool |
|---|---|---|
| Write pool (`PgPool`, default 5 connections) | All cache reads/writes, lock acquire/release, migrations | `sqlx::PgPool` |
| Cache-watch LISTEN connection (1 dedicated, combined plugin only) | Receives all `NOTIFY cluster_cache_changes` events; never used for queries | A dedicated `sqlx::PgListener`, outside the pool |
| Lock release-wake LISTEN connection (1 dedicated) | Receives all `NOTIFY cluster_lock_released` events, feeding the in-process `ReleaseWaiters` registry that wakes blocked `lock()` callers (§5.3) | A dedicated `sqlx::PgListener`, outside the pool |
| Lock connections (session-affine) | `pg_advisory_lock` is session-scoped; each acquired lock holds its advisory slot for the connection duration | Borrowed from the write pool but pinned to one connection per lock |

Advisory locks are session-scoped in Postgres, which means the connection that acquired a lock must be the same connection that releases it. The plugin maps each `LockGuard` to a specific pool connection that it pins (borrows exclusively) for the duration of the lock. The pool must therefore be sized to accommodate the maximum number of simultaneously held locks; the plugin warns at startup if `pool_max_size < expected_concurrent_locks` (a config-level advisory, not a hard failure).

**Total connection count.** Both LISTEN connections live *outside* the `PgPool` (`sqlx::PgListener` owns its own connection and cannot adopt an already-checked-out `PoolConnection`), so an instance's real steady-state connection count is `pool_max_size + 2` for the combined `PostgresClusterPlugin` (cache-watch + lock release-wake) and `pool_max_size + 1` for the standalone `PostgresLockPlugin` (release-wake only — no cache half, so no cache-watch connection).

### 3.4 synchronous_commit Enforcement

Per ADR-009 (`docs/ADR/009-leader-election-backend-safety.md`), this plugin **enforces** `synchronous_commit = on` on every connection it uses — it does not support running with `synchronous_commit = off`, and does not offer an `EventuallyConsistent` mode. `consistency()` unconditionally returns `CacheConsistency::Linearizable` (§4.5); there is no code path that downgrades it. `synchronous_commit = on` is Postgres's own default, so this is "enforce the safe default," not an unusual imposition — the case being closed off is an operator (or a co-tenant on a shared database/role) explicitly setting it to `off` for write-latency, which this plugin's lock and leader-election guarantees cannot tolerate.

Enforcement happens at two points in the connection lifecycle, using `sqlx::PgPoolOptions` hooks:

1. **`after_connect`** — runs `SET synchronous_commit = on` once when a new physical connection is established. Covers the common case (role/database default is `off`, or a session-level `ALTER ROLE ... SET synchronous_commit = off` applies at login).
2. **`before_acquire`** — re-runs `SET synchronous_commit = on` every time a connection is checked out of the pool for use, whether for a cache operation or a lock acquire. This closes the window ADR-009 flags: `synchronous_commit` is `USERSET` scope, so it can be mutated mid-session by anything sharing the connection (a misbehaving statement, a pooler-level session variable reset, `ALTER ROLE` applied after the connection was opened). Re-asserting on every checkout means a mutation can only affect the *current* checkout, never a later one.

**Residual gap: pinned lock connections.** `before_acquire` fires once, at the moment a connection is checked out — for the write pool's transient per-operation usage (cache reads/writes) that's on every use, but a lock connection is checked out *once* and then pinned (borrowed exclusively) for the entire lifetime of the held lock (§3.3), so `before_acquire` cannot re-assert on it while it's held. To close this, **each lock's own guard task** re-runs `SET synchronous_commit = on` on its pinned connection on its own interval (`lock_reaper_interval_ms`, default 5s), bounding the exposure window to that interval instead of leaving it open for the lock's full duration. The re-assertion lives on the guard task — *not* the lock TTL reaper's sweep — deliberately: the guard task is the sole owner of its key's `held` entry, so it re-asserts without taking a second `held.get_mut` across an `.await` on a live key (which a sweep-side re-assertion would, deadlocking the guard's own `renew`/`release` under the current-thread runtime — see `GAP-SOLUTIONS.md` §5). This is a defense-in-depth measure, not a hard guarantee within that window; see §11 for the accepted residual risk.

A connection on which `SET synchronous_commit = on` fails (e.g. insufficient privilege to alter the GUC) surfaces as a provider error at connect time (§9) rather than silently proceeding with an unverified durability setting.

### 3.5 Standalone Lock Provider

The cluster wiring crate (`cf-gears-cluster`) already implements config-driven per-primitive routing (`cpt-cf-clst-fr-routing-per-primitive`) — `cluster/src/wiring.rs`'s `ClusterWiring::from_config` dispatches a profile's `lock` binding through `ProviderRegistry::lock_provider(name)` and calls `ClusterLockProvider::build_lock` if a provider is registered under that name, completely independently of whichever provider serves that profile's `cache`. That mechanism is real and already works; what's been missing is a plugin that registers something under `lock_provider("postgres")`. This plugin now does, via a second, independent provider trait implementation.

**`PostgresLockProvider`** implements `ClusterLockProvider` (`provider() -> "postgres"`). Its `build_lock(options)` deserializes `options` into `PostgresLockConfig` — a config type scoped to only what the lock primitive needs (`connection_string`, `pool_max_size`, `pool_acquire_timeout_ms`, `schema`, `lock_reaper_interval_ms`, `lock_name_cardinality_warn_threshold`, `pgbouncer_transaction_mode`, `replication_mode`; no `cache_reaper_interval_ms`, `read_cache_capacity`, or `sd_poll_interval_ms` — those don't exist here since there's no cache half) — and constructs a **standalone** `PostgresLockPlugin` (§3.1: `lock/mod.rs`) with its own dedicated pool.

**Always standalone, never shared.** Per the SDK provider trait's own contract ("non-cache providers do not receive the cache backend" — `cluster-sdk/src/provider.rs`), `PostgresLockProvider` never attempts to detect or reuse a pool from a co-located `cache: { provider: postgres }` binding in the same profile, even when both point at the same `connection_string`. This is a deliberate simplicity/independence trade-off: sharing would couple two providers the SDK explicitly designed to be independent, and would need its own lifecycle-ownership story (which provider's `stop()` closes the shared pool?). The cost is a second small pool (default `pool_max_size: 5`) when both primitives happen to point at the same database — considered acceptable relative to the coupling avoided. An operator who wants combined cache+lock sharing one pool still has that option: bind `cache: { provider: postgres, ... }` and omit `lock` entirely, letting the omit-default auto-wrap use the SDK's `CasBasedDistributedLockBackend` over the shared cache instead of the native lock.

**What the standalone path builds, relative to the combined `PostgresClusterPlugin` (§3.2):**

| | Combined (`PostgresClusterPlugin`) | Standalone (`PostgresLockPlugin`) |
|---|---|---|
| Migrations run | `0001_cluster_cache.sql` + `0002_cluster_lock.sql` | `0002_cluster_lock.sql` only |
| Dedicated LISTEN connections | 2: cache watch (`cluster_cache_changes`) + lock release-wake (`cluster_lock_released`) | 1: lock release-wake (`cluster_lock_released`) only — no cache half, so no cache-watch connection |
| Background tasks | Cache TTL reaper, lock TTL reaper, cache-watch LISTEN task, lock release-wake LISTEN task | Lock TTL reaper + lock release-wake LISTEN task |
| `synchronous_commit` enforcement (§3.4) | Yes, on the shared pool | Yes, on its own pool |

Operator YAML example — Postgres lock routed independently of a non-Postgres cache:

```yaml
cluster:
  profiles:
    default:
      cache:
        provider: standalone
      lock:
        provider: postgres
        connection_string: "postgres://user:${DB_PASSWORD}@db:5432/gears"
        pool_max_size: 5
```

Registration mirrors the existing standalone plugin's pattern (`cluster/src/gear.rs:50-51`): the host registers both provider impls into the shared `ProviderRegistry` — `.with_cache_provider(Arc::new(PostgresCacheProvider))` and `.with_lock_provider(Arc::new(PostgresLockProvider))` — so either can be bound independently, or both, or neither.

`PostgresLockPlugin`'s own handle (`lock/mod.rs`) carries the same `stopped: bool` field and the same ADR-006 `Drop` guard as `PostgresClusterHandle` (§3.2) — it owns its own pool and its own lock TTL reaper, so it needs the same "forgotten `stop()` leaks background tasks" protection independently of the combined handle. It is not a special case exempted from ADR-006 just because it's the smaller of the two handles.

### 3.6 Replication Topology Warning

ADR-009's per-backend safety table conditions Postgres leader-election/lock safety on *synchronous* streaming replication — with the common default (async replication, no `synchronous_standby_names` configured), a failover can lose the last few committed transactions, including the row backing a currently-held lock or leadership claim, which is exactly the split-brain risk `synchronous_commit = on` (§3.4) is supposed to prevent. `synchronous_commit` and replication topology are two different knobs; enforcing the former (§3.4) says nothing about the latter, so this plugin also surfaces the latter rather than leaving it silently unaddressed.

Following the same shape as the `pgbouncer_transaction_mode` validation (§5.4/§7) — a config-level flag plus a startup check — but **warn rather than block**, because replication topology (unlike PgBouncer pooling mode) isn't something the plugin can always determine with certainty, and because it is a topology-level operational concern, not a per-request correctness violation the way an unenforced `synchronous_commit` would be:

- `replication_mode: Option<ReplicationMode>` (`ReplicationMode = Async | Sync`, config, §7) — an optional operator-supplied hint. If set, the plugin trusts it and skips the detection query entirely.
- If unset, `build_and_start` (combined plugin, §3.2) and `build_lock` (standalone lock provider, §3.5) each run `SHOW synchronous_standby_names` once at startup on the pool. An empty result is treated as `Async` (no synchronous standby configured); a non-empty result is treated as `Sync`.
- If the effective mode (explicit or detected) is `Async`, the plugin logs `cluster.provider.replication_async` (WARN, once at startup, not repeated) naming ADR-009's safety table and stating that leader-election/lock claims are not failover-safe under the current replication topology. `build_and_start`/`build_lock` still return `Ok` — this is advisory, not a startup failure, both because the plugin cannot always detect topology with full confidence (e.g. a synchronous standby configured but not currently connected still shows in `synchronous_standby_names`) and because some deployments (e.g. dev/single-instance) legitimately don't need HA and shouldn't be blocked by it.
- `Sync` does not upgrade `consistency()` or any `*Features` declaration — it only suppresses the WARN. The plugin's declared safety properties (§4.5, §5) are unaffected either way; this is purely an operational signal for the operator, layered on top of, not instead of, the enforcement in §3.4.

This closes the DESIGN §12 open question that previously flagged this plugin's docs as silent on replication topology — it's no longer silent, but it's also deliberately not a gate.

## 4. Cache Implementation

### 4.1 SQL Contract per Operation

`put` / `put_if_absent` take a `cluster_sdk::cache::PutRequest<'_> { key, value, ttl:
Ttl }` (`Ttl::Of(Duration) | Ttl::Indefinite`), not positional `key`/`value`/`ttl`
arguments; `$3`/`$4` below bind `NULL` for `Ttl::Indefinite` or `now() +
ttl_duration` for `Ttl::Of(d)`.

| Operation | SQL |
|---|---|
| `get(key) -> Option<CacheEntry>` | `SELECT value, version FROM cluster_cache WHERE key = $1 AND (expires_at IS NULL OR expires_at > now())` |
| `put(req: PutRequest) -> ()` | `INSERT INTO cluster_cache (key, value, version, expires_at) VALUES ($1, $2, 1, $3) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, version = cluster_cache.version + 1, expires_at = EXCLUDED.expires_at` |
| `delete(key) -> bool` | `DELETE FROM cluster_cache WHERE key = $1 AND (expires_at IS NULL OR expires_at > now()) RETURNING 1` — row returned → `true`; an expired-but-unreaped row is treated as already absent (→ `false`), consistent with `get`/`contains` |
| `contains(key) -> bool` | `SELECT 1 FROM cluster_cache WHERE key = $1 AND (expires_at IS NULL OR expires_at > now())` |
| `put_if_absent(req: PutRequest) -> Option<CacheEntry>` | `INSERT INTO cluster_cache (key, value, version, expires_at) VALUES ($1, $2, 1, $3) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, version = 1, expires_at = EXCLUDED.expires_at WHERE cluster_cache.expires_at IS NOT NULL AND cluster_cache.expires_at <= now() RETURNING value, version` — a row returned means the key was absent **or expired** (treated as a freshly-created version-1 entry); a *live* entry yields no row → `None` (already present). The `WHERE`-guarded overwrite treats an expired-but-unreaped row as logically absent, exactly as `get`/`contains`/`compare_and_swap` do, so leader-election failover (`put_if_absent` on the election key) does not stall on a lingering expired lease until the TTL reaper sweeps it. |
| `compare_and_swap(key, expected_version: u64, new_value, ttl: Ttl) -> CacheEntry` | `UPDATE cluster_cache SET value = $3, version = version + 1, expires_at = $4 WHERE key = $1 AND version = $2 AND (expires_at IS NULL OR expires_at > now()) RETURNING version` — zero rows → `CasConflict` |
| `compare_and_delete(key, expected_value) -> bool` | `DELETE FROM cluster_cache WHERE key = $1 AND value = $2 AND (expires_at IS NULL OR expires_at > now()) RETURNING 1` — an expired-but-unreaped row is treated as already absent, consistent with `get`/`contains` |
| `scan_prefix(prefix) -> Vec<String>` | `SELECT key FROM cluster_cache WHERE key LIKE $1 ESCAPE '\' AND (expires_at IS NULL OR expires_at > now())` — the plugin binds `$1` to the caller's prefix with `%`/`_`/`\` escaped and a `%` suffix appended (`escape_like`), so the caller's own text is matched literally as a prefix rather than interpreted as `LIKE` wildcards |

After every write that emits an observable event, the plugin executes `NOTIFY cluster_cache_changes, '<payload>'` in the same transaction (cache writes) or immediately after (post-commit). NOTIFY is transactional: it only reaches listeners if the transaction commits.

`CasConflict { key, current }` — when `compare_and_swap` finds the row but with a wrong version, the plugin re-reads the current entry to populate `current`. When the row is absent, `current` is `None`.

### 4.2 TTL Reaper

A background task wakes on a configurable interval (default: 10 seconds) and deletes all expired entries:

```sql
DELETE FROM cluster_cache
WHERE expires_at IS NOT NULL AND expires_at <= now()
RETURNING key;
```

For each deleted key, the task issues `NOTIFY cluster_cache_changes, 'E:<key>'` so watchers receive `CacheWatchEvent::Event(CacheEvent::Expired { key })`.

The reaper is driven by a `CancellationToken`; it self-terminates when cancelled. It uses one connection from the write pool per sweep, releasing it immediately after.

### 4.3 Watch via LISTEN / NOTIFY

The plugin maintains one dedicated Postgres connection that issues `LISTEN cluster_cache_changes` at startup. An async task reads notifications from this connection in a loop and fans them out to per-watcher channels.

```
Postgres NOTIFY ──► listen_task
                         │
                    parse payload
                         │
                    route to matching watchers
                         │
                   ┌─────┴──────┐
                   │ exact match │
                   │ key == notified_key
                   └────────────┘
```

**Exact watches only.** The native NOTIFY channel carries a single key per payload; routing by key prefix is not possible at the Postgres level without one channel per prefix (infeasible). Therefore:
- `watch(key)` → subscribe to notifications where `notified_key == key`. Returns `Ok(CacheWatch)`.
- `watch_prefix(prefix)` → returns `Err(ClusterError::Unsupported { feature: "prefix_watch" })`. Consumers use `PollingPrefixWatch` as the polyfill (DESIGN §3.12).

`features().prefix_watch` is `false`, so the capability resolver rejects `CacheCapability::PrefixWatch` at startup for this backend. The SDK-default service-discovery backend auto-selects `PollingPrefixWatch` when `prefix_watch == false` (see §6).

**Empty / unrecognized payload — Reset.** The listen_task interprets `payload.is_empty()` (or any payload not matching `<event>:<key>`) as a `Reset` signal, broadcasts `CacheWatchEvent::Reset` to every active watcher, and clears all watcher subscriptions (consumers must resubscribe). This matches ADR-003's overflow mapping for Postgres. It is the fallback for a bare `NOTIFY` from an external writer or a future format — **not** the NOTIFY-queue-overflow path: overflow aborts the committing *producer* transaction with an error and delivers no notification. Overflow does not disconnect the listener or emit a `Reset`; it surfaces to the *writer* as that write's `Provider` error and in the PostgreSQL server logs, not through this LISTEN-side recovery.

**Connection loss — Reset.** If the dedicated LISTEN connection drops, the listen_task attempts reconnect with exponential backoff. On successful reconnect, it broadcasts `CacheWatchEvent::Reset` before resuming event delivery, signalling that consumers may have missed events during the gap. If reconnect fails beyond the configured retry limit, it broadcasts `CacheWatchEvent::Closed(ClusterError::Provider { kind: ConnectionLost, .. })` and exits.

The Postgres cache is read-through: every `get` hits the database directly. There is no in-process read cache — consumers with hot-key, high-read, staleness-tolerant workloads should route that primitive to a backend built for it (e.g. Redis) rather than expect this plugin to double as a fast local cache; see §11 for the rationale.

### 4.4 scan_prefix

`scan_prefix(prefix)` is implemented via `LIKE prefix%`. The plugin appends `%` to the caller's prefix; the caller must not include a wildcard. This is used by `PollingPrefixWatch` to enumerate keys for diffing. Performance degrades with keyspace size; the partial index on `expires_at` does not help here. High-volume prefix scans should use a backend with native prefix watch (Redis, NATS, etcd).

### 4.5 Consistency Declaration

`consistency()` returns `CacheConsistency::Linearizable`. All cache operations run at Postgres's default `READ COMMITTED` isolation level, which provides linearizability for single-row operations (the only kind the cache uses). The CAS path uses an `UPDATE … WHERE version = $expected`, which is an atomic compare-and-set at the row level regardless of isolation level. Under `READ COMMITTED`, concurrent updates do not produce write skew on single rows.

## 5. Distributed Lock Implementation

### 5.1 Advisory Lock Mapping

The plugin uses **session-level advisory locks** via the **two-key form** (`pg_try_advisory_lock(key1, key2)` / `pg_advisory_unlock(key1, key2)`) from the start, rather than the single-`bigint` form. The lock name is hashed once, client-side in Rust, to a stable 64-bit value using a fixed, versioned hash function (e.g. FNV-1a-64 or SipHash-1-3 with a fixed key — the specific algorithm is an implementation detail as long as it is deterministic and documented), then split into two `i32` halves:

```sql
-- acquire (non-blocking)
SELECT pg_try_advisory_lock($key1, $key2);  -- returns TRUE if acquired

-- release
SELECT pg_advisory_unlock($key1, $key2);
```

Where `$key1` = high 32 bits and `$key2` = low 32 bits of the 64-bit name hash, computed by the plugin before the query is sent. This uses the full 64 bits of hash entropy (unlike `hashtext()`, which only produces 32 bits and would need casting/padding to fill a `bigint` argument), so the birthday-bound collision probability is negligible even at large lock-name cardinalities. It also places this plugin's locks in a namespace disjoint from any other code in the same database using the single-argument `pg_try_advisory_lock(bigint)` form for an unrelated purpose — Postgres treats the one-key and two-key advisory lock spaces as independent, so there is no cross-contention with other single-key advisory-lock users even in the (vanishingly unlikely) event of a hash collision. Residual collision risk between two *of this plugin's own* lock names is tracked as a monitored risk, not eliminated — see §11.

### 5.2 TTL Enforcement

Advisory locks have no native TTL. The plugin layers TTL on top:

1. **Metadata insert**: on `try_lock` success, insert `(name, holder_id, acquired_at, ttl_ms)` into `cluster_lock` in the same transaction.
2. **Background reaper**: scans `cluster_lock` every N seconds for rows where `acquired_at + ttl_ms * interval '1ms' < now()`. For each expired row, recomputes `(key1, key2)` from `name` and calls `pg_advisory_unlock(key1, key2)`, then deletes the row. The advisory unlock must run on the **same connection** that holds the lock (session-scoped). The reaper therefore tracks the connection ID per lock.
3. **Connection pinning**: `LockGuard` pins its pool connection for the duration of the lock. The reaper calls `pg_advisory_unlock` via the same pinned connection.
4. **Crash / disconnect**: advisory locks auto-release when the Postgres session disconnects. This is the safety net for process crash or network loss — the TTL-based reaper handles the "alive but forgot to release" scenario.

`LockGuard::renew(new_ttl)` updates `ttl_ms` and resets `acquired_at` in `cluster_lock`. It does not touch the advisory lock itself.

`LockGuard::release(self)` calls `pg_advisory_unlock` and deletes the `cluster_lock` row, then issues `NOTIFY cluster_lock_released, '<name>'` to wake blocked `lock()` waiters.

### 5.3 Blocking lock()

`lock(name, ttl, timeout)` retries `pg_try_advisory_lock` and, between attempts, waits on the in-process `ReleaseWaiters` registry for an early wake:

```
loop {
    try pg_try_advisory_lock → success? return LockGuard
    if past deadline → LockTimeout
    register interest in `name` with the ReleaseWaiters registry
    wait on (that registration resolving) OR a short heartbeat sleep (250ms)
}
```

The wait does **not** LISTEN on the acquiring connection. `sqlx`'s `PgListener` owns its own single connection and has no public way to adopt an already-checked-out `PoolConnection`, so instead a single **dedicated** `cluster_lock_released` LISTEN connection (opened at `build_and_start`, present in both the combined and standalone plugins — §3.3) runs a fan-out task that `notify()`s the in-process `ReleaseWaiters` registry; each blocked `lock()` caller registers a waiter there and is woken when a `NOTIFY cluster_lock_released` for its name arrives. The 250 ms heartbeat sleep is a safety net against a missed notification (registration racing an already-fired `NOTIFY`, or the listen task momentarily reconnecting): a lost wake only costs latency up to the heartbeat interval, never correctness — the loop always re-attempts `pg_try_advisory_lock` itself as the source of truth. A waiter that gives up (timeout or heartbeat-driven re-acquire) deregisters itself from the registry on drop, so no stale waiter accumulates.

This avoids busy-polling: waiters wake promptly when a holder explicitly releases. TTL-based reaper releases also trigger `NOTIFY cluster_lock_released` after unlocking.

### 5.4 PgBouncer Constraint

Session-level advisory locks are **incompatible with PgBouncer in transaction pooling mode**. A session advisory lock lives on the *server* session, but transaction pooling does not pin a client to one server session across transactions: returning the connection to the pool does not release the lock — it leaves it attached to that pooled server session, where it both outlives the holding `LockGuard`'s transactions and can leak to whichever client is next handed that server session. Either way mutual exclusion breaks. The plugin documents this prominently in config validation:

- If `pgbouncer_transaction_mode: true` is set in config, `build_and_start` returns `Err(ClusterError::InvalidConfig { reason: "pg_advisory_lock requires session-mode pooling or a direct connection; transaction-mode PgBouncer incompatible with distributed locks" })`.
- Operators using PgBouncer must either use session pooling mode for the cluster plugin's connection string, or use a different lock backend.

## 6. Leader Election and Service Discovery

Both primitives use SDK defaults over the Postgres cache backend.

**Leader election** — `CasBasedLeaderElectionBackend::new(Arc::clone(&cache))`. The cache backend is `Linearizable`, so the consistency guard passes. `LeaderElectionFeatures::linearizable == true`.

**Service discovery** — `CacheBasedServiceDiscoveryBackend::new(Arc::clone(&cache))`. The cache backend declares `prefix_watch: false`. The service-discovery default backend detects this when opening its topology watch (`ensure_maintainer`/`watch`) and falls back to `PollingPrefixWatch`, using `scan_prefix` to enumerate keys under the `svc/` prefix at each polling interval; `discover` additionally reconciles from a fresh `scan_prefix` sweep on each call over a polling cache, so it reflects current backend truth rather than lagging the poll interval. The interval is configurable via the backend's `with_prefix_watch_polling` (default 5s; the omit-default wiring path currently uses that backend default — plumbing the operator-config `sd_poll_interval_ms` through the SDK auto-wrap is a tracked follow-up). `ServiceDiscoveryFeatures::metadata_pushdown == false`.

The wiring crate's omit-default auto-wrap (DESIGN §3.11) wires these automatically when a profile declares `cache: { provider: postgres }` and omits `leader_election` and `service_discovery`.

## 7. Configuration

```rust
#[derive(Deserialize, toolkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct PostgresClusterConfig {
    /// sqlx connection string. Supports `${VAR}` / `${VAR:-default}` env-var
    /// expansion (e.g. `postgres://user:${DB_PASSWORD}@db:5432/gears`) via
    /// `toolkit_utils::var_expand`, resolved through `ctx.config_expanded()` —
    /// the same mechanism `libs/toolkit-db` uses for DB passwords/DSNs. A
    /// credstore-backed (`secret_ref`) resolution path is deferred to a
    /// future iteration; not implemented here.
    #[expand_vars]
    pub connection_string: String,

    /// Maximum pool size (write pool). Default: 5.
    #[serde(default = "default_pool_size")]
    pub pool_max_size: u32,

    /// Pool acquire timeout. Default: 5s.
    #[serde(default = "default_acquire_timeout")]
    pub pool_acquire_timeout_ms: u64,

    /// Schema for plugin tables. Default: "public".
    #[serde(default = "default_schema")]
    pub schema: String,

    /// TTL reaper interval for cluster_cache. Default: 10s.
    #[serde(default = "default_reaper_interval")]
    pub cache_reaper_interval_ms: u64,

    /// TTL reaper interval for cluster_lock. Default: 5s.
    #[serde(default = "default_lock_reaper_interval")]
    pub lock_reaper_interval_ms: u64,

    /// Polling interval for the service-discovery PollingPrefixWatch. Default: 5s.
    #[serde(default = "default_sd_poll_interval")]
    pub sd_poll_interval_ms: u64,

    /// Set to true to get an InvalidConfig error at startup rather than silent
    /// mis-behaviour if the connection string points to a PgBouncer in
    /// transaction mode. Default: false.
    #[serde(default)]
    pub pgbouncer_transaction_mode: bool,

    /// Distinct concurrently-held lock-name count past which the lock reaper
    /// logs `cluster.lock.name_cardinality_high` (WARN) and the
    /// `cluster_postgres_lock_active_names` gauge should be alerted on.
    /// Default: 1000 (see DESIGN §5.1/§8/§11 — advisory-lock collision risk).
    #[serde(default = "default_lock_name_cardinality_warn_threshold")]
    pub lock_name_cardinality_warn_threshold: u32,

    /// Operator hint for replication topology (`Async` | `Sync`). If omitted,
    /// detected at startup via `SHOW synchronous_standby_names` (empty →
    /// `Async`). `Async` logs `cluster.provider.replication_async` (WARN,
    /// once) per ADR-009's safety table (§3.6) but never fails startup.
    #[serde(default)]
    pub replication_mode: Option<ReplicationMode>,
}
```

```rust
#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationMode {
    Async,
    Sync,
}
```

Operator YAML example:

```yaml
cluster:
  profiles:
    default:
      cache:
        provider: postgres
        connection_string: "postgres://user:${DB_PASSWORD}@db:5432/gears"
        pool_max_size: 10
```

**`PostgresLockConfig`** (standalone lock provider, §3.5) is a separate, smaller config type — it only carries the fields the lock primitive actually uses, not the cache-only ones (`cache_reaper_interval_ms`, `sd_poll_interval_ms`):

```rust
#[derive(Deserialize, toolkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct PostgresLockConfig {
    #[expand_vars]
    pub connection_string: String,

    #[serde(default = "default_pool_size")]
    pub pool_max_size: u32,

    #[serde(default = "default_acquire_timeout")]
    pub pool_acquire_timeout_ms: u64,

    #[serde(default = "default_schema")]
    pub schema: String,

    #[serde(default = "default_lock_reaper_interval")]
    pub lock_reaper_interval_ms: u64,

    #[serde(default)]
    pub pgbouncer_transaction_mode: bool,

    #[serde(default = "default_lock_name_cardinality_warn_threshold")]
    pub lock_name_cardinality_warn_threshold: u32,

    #[serde(default)]
    pub replication_mode: Option<ReplicationMode>,
}
```

The field set is identical in name and default to the corresponding fields on `PostgresClusterConfig` above (implementation should factor the shared subset into one inner struct rather than duplicate the field definitions, to keep the two config types from drifting). `replication_mode`/the detection fallback applies here too — ADR-009's safety table is about leader-election/lock claims specifically, so the standalone lock provider needs the same warning, not just the combined plugin (§3.6).

## 8. Observability

The plugin satisfies the versioned observability contract (ADR-004,
`OBSERVABILITY.md`) verbatim — it emits no signal names beyond the catalog.
All metrics, spans, and log events use the label `provider = "postgres"`.

**Cache** — the native `PostgresCache` is wrapped in the SDK's
`cluster_sdk::observability::InstrumentedCache` decorator (the same mechanism
the standalone plugin uses), so it emits the full cache signal set for free:
spans `cluster.cache.get` / `put` / `delete` / `contains` / `put_if_absent` /
`compare_and_swap` / `watch` / `watch_prefix`; the counter
`cluster_cache_ops_total{provider,op,result}` and histogram
`cluster_cache_op_duration_seconds{provider,op}`.

**Lock** — `PostgresLock` is a native trait implementation (not a
decorator-wrapped default), so it emits lock signals directly at each
instrumentation site, mirroring the pattern
`CasBasedDistributedLockBackend::record_lock` uses (`cluster/src/defaults/lock.rs`):
spans `cluster.lock.try_lock` / `lock` / `renew` / `release` (via `tracing`,
one per `DistributedLockBackend`/`LockGuard` method); the counter
`cluster_lock_ops_total{provider,op,result}` and histogram
`cluster_lock_op_duration_seconds{provider,op}` via the injected
`cluster_sdk::observability::ClusterMetrics` sink.

**Shared signals** — both paths route backend failures through
`cluster_sdk::observability::emit_provider_error`, which increments
`cluster_provider_errors_total{provider,kind}` and logs `cluster.provider.error`
at ERROR with the `key`/`lock` resource field, `op`, `kind`, and `message`. The
LISTEN task's `Reset` broadcasts (§4.3 NOTIFY overflow and reconnect) call
`ClusterMetrics::watch_reset("cache")`, backing
`cluster_watch_resets_total{provider,primitive}`.

**Plugin-specific, non-contract metrics** — the TTL reapers additionally emit
`cluster_postgres_reaper_sweep_duration_seconds{provider,primitive}` (histogram,
`primitive={cache,lock}`), a plugin-local addition tracked outside the ADR-004
catalog. Per ADR-004, adding a signal is non-breaking; this one exists only to
let operators monitor reaper health and carries no cross-provider portability
requirement.

The lock reaper sweep (§5.2) also emits
`cluster_postgres_lock_active_names{provider}` (gauge) — the current row count
of `cluster_lock`, i.e. the number of distinct lock names concurrently held.
This is the operational counterpart to the advisory-lock collision risk
documented in §5.1/§11: since collision probability rises with the number of
distinct names in flight, this gauge is what a Grafana panel/alert would watch
to catch rising exposure before it manifests as an unexplained
`LockContended`. It is a plain count, not a per-name breakdown — lock names
are never used as label values (the cardinality rule below). When the count
exceeds `lock_name_cardinality_warn_threshold` (config, §7; default 1 000), the
plugin logs `cluster.lock.name_cardinality_high` (WARN, rate-limited to once
per reaper interval) so the same condition is visible in logs even without a
dashboard.

All emission is subject to the `METRIC_LABEL_ALLOWLIST` cardinality rule: keys
and lock names are NEVER used as metric label values, only as span attributes
and log fields.

Log events follow the `cluster.{primitive}.{event}` naming scheme
(`OBSERVABILITY.md` §6): `cluster.watch.reset` (WARN), `cluster.provider.error`
(ERROR), `cluster.lock.name_cardinality_high` (WARN, plugin-local), and
`cluster.provider.replication_async` (WARN, plugin-local, once at startup —
§3.6) are the four this plugin emits; it has no leadership transitions of its
own to report (leader election is the SDK default over this plugin's cache,
and emits `cluster.leader.transition` itself).

## 9. ProviderErrorKind Mapping

Matches the platform mapping table (`docs/DESIGN.md` §4.1, Postgres/sqlx column):

| `sqlx` error | `ClusterError` / `ProviderErrorKind` |
|---|---|
| `sqlx::Error::Configuration` | `InvalidConfig` — a malformed DSN / unparseable connection options is an operator config error, not a runtime backend fault, so it is *not* wrapped as a `Provider` error (`PG-LIFE-006`) |
| `sqlx::Error::Io` | `ConnectionLost` |
| `sqlx::Error::PoolTimedOut` | `Timeout` |
| `sqlx::Error::PoolClosed` | `ConnectionLost` |
| SQLSTATE `28xxx` (invalid auth) | `AuthFailure` |
| SQLSTATE `3D000` (invalid catalog/database does not exist) | `Other` — a missing database is a deployment/config problem, not an authentication failure; unlike `pgbouncer_transaction_mode`, this is not distinguishable from the connection string alone, so `build_and_start` cannot reject it as `InvalidConfig` up front and it surfaces at first-connect as a plain `Other` provider error |
| Any other `sqlx::Error` | `Other` |

Connection loss during a LISTEN reconnect loop is surfaced as `Provider { kind: ConnectionLost }` to affected watchers after the retry budget is exhausted.

## 10. Shutdown Sequence

`PostgresClusterHandle::stop()` follows DESIGN §3.13:

1. Cancel the `CancellationToken` shared by all background tasks (cache reaper, lock reaper, cache-watch LISTEN task, lock release-wake LISTEN task). Await each task's `JoinHandle`. Cancellation also unparks each held lock's guard task promptly (§3.4), rather than leaving it to run out its reassert interval.
2. Send `CacheWatchEvent::Closed(ClusterError::Shutdown)` to all active watcher channels (dispatched directly against the watch registry before the LISTEN task is awaited, so every watcher observes it prior to `stop()` returning).
3. Drop each dedicated `PgListener` (cancelling its task drops the listener, which closes its socket). No explicit `UNLISTEN *` is issued — dropping the connection ends the session, which is functionally equivalent (a closed backend cannot deliver further notifications).
4. Drain any lock still held at shutdown (`drain_held`: unlock each advisory lock on its own pinned connection, return the connection to the pool) *before* closing the pool, then close the `sqlx::PgPool` — which releases all remaining pooled connections, causing Postgres to auto-release any outstanding advisory locks.

No remote cleanup is performed on a best-effort basis: held claims and locks lapse via their TTL once the connections drop (`cpt-cf-clst-fr-shutdown-ttl-cleanup`).

## 11. Risks / Trade-offs

**[Risk: LISTEN/NOTIFY does not scale under high concurrent write rates]** NOTIFY acquires a global exclusive lock on commit. Under > ~1000 notifying transactions/sec, this becomes a bottleneck. Mitigation: the cache plugin is not recommended for high-throughput subscriber lease workloads (use Redis cache for those — DESIGN §4.2). Queue overflow aborts the *committing* transaction, so it surfaces as the failing write's `Provider` error (and in the PostgreSQL server logs) — monitor those rather than `cluster_watch_resets_total`, which counts LISTEN connection gaps, not overflow.

**[Risk: hash collision in advisory lock names]** The plugin uses the two-argument `pg_try_advisory_lock(key1, key2)` form from the start (§5.1), splitting a 64-bit name hash into two `i32` halves. This uses the full 64-bit hash space (vs. 32 bits from `hashtext()` alone), making collision probability negligible even at large lock-name cardinalities. Residual risk is not zero — any hash function has a nonzero collision probability — so the plugin emits `cluster_postgres_lock_active_names{provider}` (gauge, §8) and logs `cluster.lock.name_cardinality_high` (WARN) past `lock_name_cardinality_warn_threshold` (§7), so operators can build a Grafana panel/alert on the gauge instead of discovering rising exposure via an unexplained `LockContended`.

**[Risk: PgBouncer transaction mode mis-configuration]** Silent mis-behaviour if an operator uses transaction-mode PgBouncer without the `pgbouncer_transaction_mode: true` config flag. The `pg_advisory_lock` appears to succeed but, because transaction pooling does not pin a client to one server session, the lock is stranded on a pooled server session — outliving the guard and leaking to other clients that reuse that session (see §5.4). Mitigation: the startup validation flag; documentation.

**[Trade-off: prefix_watch is polling-based]** `watch_prefix` is serviced by `PollingPrefixWatch`, not a native LISTEN/NOTIFY subscription. This means prefix watch events have a latency of up to the poll interval (default 5s) and the poll cost is N `get` calls per interval. Service discovery use cases that require sub-second topology change propagation should use a backend with native prefix watch (etcd, NATS).

**[Trade-off: connection pinning for advisory locks]** Each simultaneously held lock consumes one pool connection. A large `max_concurrent_locks` relative to `pool_max_size` exhausts the pool and causes `LockTimeout` on otherwise-available locks. Operators must size the pool accordingly.

**[Trade-off: `synchronous_commit = on` enforced, no `off` mode]** The plugin enforces `synchronous_commit = on` on every connection (§3.4) and offers no `EventuallyConsistent`/weak-consistency mode. Operators who need `off`'s write-latency benefit and can tolerate its durability trade-off (risk of losing the last few commits on crash) cannot get it from this plugin — that use case belongs on a backend designed for it. Enforcement is via `after_connect` + `before_acquire` hooks (re-asserted on every checkout), except for pinned lock connections, which are only re-asserted once per `lock_reaper_interval_ms` (default 5s) rather than continuously, since they're checked out once and held for the lock's full duration (§3.3). This leaves a bounded, accepted residual window in which a lock connection's GUC could theoretically have been flipped by an external actor between reaper sweeps — mitigated by scope (this plugin's pool is dedicated, not shared with arbitrary application queries) but not eliminated by design.

**[Risk: async replication is warn-only, not enforced]** ADR-009 requires synchronous streaming replication for Postgres leader/lock safety under failover, but §3.6's `replication_mode` check only warns (`cluster.provider.replication_async`) when it detects or is told the topology is async — it never fails startup. An operator who ignores or doesn't monitor that log line can run indefinitely on an async-replicated, failover-unsafe topology. This is a deliberate choice (topology isn't always confidently detectable, and some deployments legitimately don't need HA), not an oversight — but it means this is an operational monitoring dependency, not a guarantee enforced by the plugin itself; pair the WARN log with an alert, not just a dashboard.

**[Design choice: no read-path cache]** `get` is always read-through to Postgres (§4.3) — the plugin deliberately does not layer an in-process read cache in front of it. An in-process cache here would be local to each service instance, not shared across a fleet: at N instances it multiplies rather than amortizes correctness risk (each instance's cache would independently race NOTIFY-driven invalidation against concurrent reads, so different instances could transiently observe different values for the same key), while doing nothing to relieve the actual write-side bottleneck above (NOTIFY volume is driven by writers, not readers). It would also risk silently reaching the leader-election and service-discovery primitives that ride on this same cache backend (§6) specifically *because* it declares `Linearizable` consistency — caching those reads would undermine the reason this backend was chosen for them. The intended pattern is per-primitive backend selection: route a given primitive to the backend suited to its access pattern (e.g. Redis for a hot, staleness-tolerant application cache; this plugin for Postgres-backed locks/coordination), rather than asking one backend to be good at everything.

## 12. Open Questions

| Question | Owner | Target Resolution | Recommendation |
|---|---|---|---|
| Credstore-backed credential resolution for the connection string | Postgres plugin owner + Platform OOP deployment design | Future iteration, once the OOP/credstore wiring contract (`docs/arch/toolkit-oop/DESIGN.md` §Platform Host Composition; parent cluster `DESIGN.md:41`) is committed | Decided for now: `connection_string` uses `${VAR}` / `${VAR:-default}` env-var expansion (`toolkit_utils::var_expand` via `#[derive(toolkit_macros::ExpandVars)]` + `#[expand_vars]`, §7), the same mechanism `libs/toolkit-db` uses for DB passwords/DSNs — no `secret_ref` field is exposed by this plugin's config in the meantime. When the credstore path is eventually added, reuse the wiring crate's existing `BackendBinding.secret_ref: Option<SecretRef>` (`cluster/src/config.rs:83`) rather than reintroducing a plugin-local field of the same name at a different layer — that duplication is exactly what was removed here |

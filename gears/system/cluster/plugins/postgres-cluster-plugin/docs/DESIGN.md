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
  - [5.1 The Lease Row](#51-the-lease-row)
  - [5.2 TTL Enforcement and Garbage Collection](#52-ttl-enforcement-and-garbage-collection)
  - [5.3 Blocking lock()](#53-blocking-lock)
  - [5.4 PgBouncer Constraint](#54-pgbouncer-constraint)
- [6. Leader Election](#6-leader-election)
- [7. Configuration](#7-configuration)
- [8. Observability](#8-observability)
- [9. ProviderErrorKind Mapping](#9-providererrorkind-mapping)
- [10. Shutdown Sequence](#10-shutdown-sequence)
- [11. Risks / Trade-offs](#11-risks--trade-offs)
- [12. Open Questions](#12-open-questions)

<!-- /toc -->

## 1. Overview

`cf-postgres-cluster-plugin` is the Postgres backend plugin for the cluster gear. It provides a native `ClusterCacheBackend` over a `sqlx::PgPool` and a native `DistributedLockBackend` over a `cluster_lock` lease row whose `expires_at` is its only liveness authority (§5.1). Leader election is derived from the SDK default backend over the Postgres cache — no additional tables or connections are required for that primitive.

The plugin is the recommended deployment for **multi-instance, no-K8s** environments (DESIGN §4.2): Postgres is already deployed in every Gears environment, zero new infrastructure is required, and a conditional upsert under `synchronous_commit = on` gives ACID-correct mutual exclusion without a distributed lock service.

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
| `DistributedLockBackend` | Native — the `cluster_lock` lease row as sole arbiter, with `expires_at` as the only liveness authority (§5.1). Independently routable via `lock: { provider: postgres }` (§3.5), with its own pool/config — not required to be paired with the postgres cache provider | `linearizable: true` | — |

`prefix_watch: false` means that consumers requiring `CacheCapability::PrefixWatch` cannot bind this backend without the polyfill. A consumer that needs prefix-watch semantics over this cache wraps it in `PollingPrefixWatch` (ADR-010), which synthesizes them over `scan_prefix`.

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
    PRIMARY KEY (key),
    CONSTRAINT cluster_cache_key_len_check CHECK (octet_length(key) <= 2048)
);

CREATE INDEX cluster_cache_expires_idx ON cluster_cache (expires_at)
    WHERE expires_at IS NOT NULL;
```

`key` is the fully-qualified backend key (scope prefix already applied by `ScopedCacheBackend`). `version` starts at 1 on first insert and increments by 1 on every successful write (including CAS). `expires_at IS NULL` means no TTL. The partial index on `expires_at` makes the TTL reaper's scan efficient.

##### Key length

`key` is `TEXT` rather than `VARCHAR(n)` because the two are not different storage: Postgres stores them identically on disk, and `VARCHAR(n)` is just `TEXT` plus a length check. The reason a bound is needed anyway is `PRIMARY KEY (key)` — the value lands in a btree, and a btree index tuple cannot exceed roughly one third of a page (~2704 bytes by default). Past that an `INSERT` fails outright with SQLSTATE `54000`; TOAST does not rescue an indexed key the way it would a non-indexed column.

The plugin therefore caps an indexed key at **2048 bytes** (`limits::MAX_INDEXED_KEY_BYTES`), enforced in two places:

- **In Rust, before the write** — `cache::watch::validate_key_len` on every mutation, returning `ClusterError::InvalidName`. This is the path consumers actually hit.
- **In SQL, as a backstop** — `cluster_cache_key_len_check`, so a value arriving another way (psql, a future code path) fails as a named constraint violation rather than an opaque btree error. `octet_length`, not `length`: the limit is on bytes, and a multi-byte key has more of them than characters.

#### `cluster_lock`

```sql
CREATE TABLE cluster_lock (
    name        TEXT        NOT NULL,
    owner       TEXT        NOT NULL,
    fence       BIGINT      NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (name),
    CONSTRAINT cluster_lock_name_len_check CHECK (octet_length(name) <= 2048),
    CONSTRAINT cluster_lock_fence_positive_check CHECK (fence > 0)
);

CREATE INDEX cluster_lock_expires_idx ON cluster_lock (expires_at);
```

This row **is** the lock (§5.1) — not metadata beside one. `expires_at` is the lock's absolute deadline, computed as `now() + ttl` on the **database** clock at insert and at every renew (PGR-C2, exactly as for `cluster_cache.expires_at`) — not a raw `acquired_at`/`ttl_ms` pair the TTL reaper re-derives for every row on every tick. A derived deadline could not be indexed at all: `timestamptz + interval` is `STABLE`, not `IMMUTABLE` (its result depends on the session `TimeZone`), so Postgres rejects an expression index on `acquired_at + ttl_ms * interval '1ms'`, and `now()` may not appear in a partial-index predicate either — leaving every sweep a guaranteed sequential scan with a per-row interval multiply. Storing the deadline makes the sweep an indexed `WHERE expires_at <= now()` and lets the reaper read `min(expires_at)` index-only, to wake when the next lock is actually due rather than polling blindly (§5.2). The index is unconditional rather than partial like the cache's: a lock TTL is mandatory (`DistributedLockBackend` takes a `Duration`, not a `Ttl`), so there is no `NULL` subset to exclude. `acquired_at` is restamped on every renew and is purely diagnostic — no query filters on it. (It used to carry one load-bearing role, the orphan sweep's fence; that sweep went with the beacon, §5.2.)

`owner` and `fence` are the **lease token**, and together they are the whole of the authority over the row (cluster DESIGN.md §3.19.1, ADR-012). `renew` and `release` are conditional writes predicated on `(name, owner, fence)` and on nothing any process remembers, which is what lets a lease acquired through one replica be renewed through another that never saw the acquire (invariant I7).

- `owner` is the holder's identity: a caller-supplied `ClientId` for a brokered acquisition, or a freshly minted UUID per in-process `try_lock`/`lock`, so two guards held concurrently in one process are distinct owners and neither can renew or release the other's lease. `TEXT`, not the `UUID` the superseded `holder_id` column used — a `ClientId` is an opaque caller string, so the column cannot be narrower than that. The comparison is equality-only against a row already located by primary key, so the collation-aware compare costs nothing measurable next to the 16-byte one it replaces.
- `fence` is per-`name` and strictly increases on every acquisition, including a steal-on-expiry: the acquire statement writes `fence = cluster_lock.fence + 1`, read off the row's own current value rather than bound by the acquirer, so the increment is atomic with the steal that performs it. That is what makes "steal on expiry" safe rather than merely detectable — a stale holder's operations fail their predicate instead of silently succeeding against a lease someone else now owns. `BIGINT` starting at 1, so 0 stays available to name the absence of a claim; `cluster_lock_fence_positive_check` is the backstop for the Rust-side `FIRST_FENCE`.

**The fence is not retained past the lease, and that is a known gap.** `release` deletes the row and the TTL reaper deletes a lapsed one, so a subsequent acquisition of that name starts again at 1 — the same counter reset `cluster_cache.version` has (§2.2). Closing it is item `L3`, which introduces `fence_retention` and must teach the reaper below to skip a row inside that window. Until then the guarantee this table offers is the narrower **"a fence value is never reused while the lease is live"**. The exposure is one owner re-acquiring a name it previously held while still holding a token from before the lapse; both tokens then name that owner's own live lease, so it is not a mutual-exclusion break. ADR-012 records why.

There is deliberately **no index** on `(owner, fence)`. Nothing filters on either without also filtering on `name`, which is the primary key, so every lease predicate is already a single-row lookup and an index on the token halves would be pure write amplification on the acquire path.

`name` is `PRIMARY KEY` and so carries the same btree index-tuple exposure as `cluster_cache.key` — identical bound, identical two-layer enforcement (`lock::validate_lock_name` in Rust, `cluster_lock_name_len_check` as the SQL backstop). Names are rejected at acquisition, before any lock state is mutated, so `release()` never reaches a lock whose metadata row could not be written.

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

Where `<event_type>` is one of `C` (Changed), `D` (Deleted), `E` (Expired). The payload budget alone would allow a key of ≤ 7997 bytes (7999-byte payload limit minus the two-byte `<event_type>:` prefix), but that is *not* the binding limit: `cluster_cache.key` is also a `PRIMARY KEY`, so the ~2704-byte btree index-tuple ceiling (§2.1) bites first. `cache::watch::MAX_KEY_BYTES` is the tighter of the two — 2048 bytes — validated at write time, returning `ClusterError::InvalidName` for keys that would exceed it.

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
      reaper.rs     — cluster_lock TTL sweep
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
- **Long-lived, owned connections for `LISTEN`** (§3.3, §4.3, §5.3): a subscription is session-scoped, so the plugin must own those connections outright for their lifetime rather than borrowing one per statement. `DatabaseConnection`'s only own-a-connection primitive is a transaction, and abusing a long-lived transaction for this collides with the PgBouncer-transaction-mode incompatibility this plugin already rejects at startup (§5.4).
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
3. Opens the
   dedicated LISTEN connections (`.await`ed).
4. Spawns the cache TTL reaper, the lock TTL reaper, and the LISTEN fan-out tasks.
5. Returns the handle. By the time `build_and_start` resolves, the schema
   exists and the LISTEN connections are live — there is
   no readiness gate or background-init race for callers to reason about, unlike
   a design built around a synchronous builder. A failure at any of these steps
   tears down whatever the earlier ones already started rather than detaching it.
   (The lock backend itself is now built synchronously and infallibly: it
   establishes nothing up front, since every lease operation is one statement on
   a pool the caller already opened.)

`stop`:
1. Cancels all `CancellationToken`s; awaits background tasks.
2. Sends `CacheWatchEvent::Closed(ClusterError::Shutdown)` to all active watchers.
3. Drops each dedicated `PgListener` — awaiting the cancelled LISTEN tasks in
   step 1 is what drops them, so the LISTEN connections are already closed by the
   time step 4 runs (§10 step 3).
4. Closes the pool (§10 step 4). **Held lease rows are left in place** — a
   restart is not a lease event — so there is no drain, and no ordering
   constraint here beyond closing the pool last.
5. Sets `self.stopped = true` as the last step — graceful shutdown completed, so the `Drop` guard above must not fire.

### 3.3 Connection Pool Split

| Connection type | Purpose | Pool |
|---|---|---|
| Write pool (`PgPool`, default 5 connections) | All cache reads/writes, **every** `cluster_lock` statement (acquire, renew, release, the TTL sweep), all `pg_notify`, migrations | `sqlx::PgPool` |
| Cache-watch LISTEN connection (1 dedicated, combined plugin only) | Receives all `NOTIFY cluster_cache_changes` events; never used for queries | A dedicated `sqlx::PgListener`, outside the pool |
| Lock release-wake LISTEN connection (1 dedicated) | Receives all `NOTIFY cluster_lock_released` events, feeding the in-process `ReleaseWaiters` registry that wakes blocked `lock()` callers (§5.3) | A dedicated `sqlx::PgListener`, outside the pool |

**The lock primitive opens no connection of its own.** It used to: one dedicated socket held the liveness beacon, whose death was the fleet's signal that this process was gone (§5.1). With the beacon removed the only off-pool connections left belong to `LISTEN`, and both are the cache's and the release-wake's rather than the lease's.

A held lock therefore consumes **no connection at all** — not a pooled one, and not a share of anything per-lock. The number of simultaneously held locks is bounded by `cluster_lock` cardinality (§8's `lock_name_cardinality_warn_threshold`), not by `pool_max_size` and not by Postgres's shared lock-manager table. This is why there is no "size the pool for your concurrent locks" advisory.

**No `pg_advisory_lock`, blocking or otherwise, anywhere in this plugin** (§5.1, §5.3). The blocking form in particular would park a task inside Postgres waiting for a key nobody can hand over. This was previously a property enforced *on* the beacon; it is now simply total.

**Total connection count.** The LISTEN connections do not live in the `PgPool` (`sqlx::PgListener` owns its own connection and cannot adopt a `PoolConnection`), so an instance's real steady-state connection count is `pool_max_size + 2` for the combined `PostgresClusterPlugin` (cache-watch + lock release-wake) and `pool_max_size + 1` for the standalone `PostgresLockPlugin` (release-wake only — no cache half, so no cache-watch connection). Each is one lower than it was, the beacon having been the difference. That total does not move with how many locks are held.

### 3.4 synchronous_commit Enforcement

Per ADR-009 (`docs/ADR/009-leader-election-backend-safety.md`), this plugin **enforces** `synchronous_commit = on` on every connection it uses — it does not support running with `synchronous_commit = off`, and does not offer an `EventuallyConsistent` mode. `consistency()` unconditionally returns `CacheConsistency::Linearizable` (§4.5); there is no code path that downgrades it. `synchronous_commit = on` is Postgres's own default, so this is "enforce the safe default," not an unusual imposition — the case being closed off is an operator (or a co-tenant on a shared database/role) explicitly setting it to `off` for write-latency, which this plugin's lock and leader-election guarantees cannot tolerate.

Enforcement happens at two points in the connection lifecycle, using `sqlx::PgPoolOptions` hooks:

1. **`after_connect`** — runs `SET synchronous_commit = on` once when a new physical connection is established. Covers the common case (role/database default is `off`, or a session-level `ALTER ROLE ... SET synchronous_commit = off` applies at login).
2. **`before_acquire`** — re-runs `SET synchronous_commit = on` every time a connection is checked out of the pool for use, whether for a cache operation or a lock acquire. This closes the window ADR-009 flags: `synchronous_commit` is `USERSET` scope, so it can be mutated mid-session by anything sharing the connection (a misbehaving statement, a pooler-level session variable reset, `ALTER ROLE` applied after the connection was opened). Re-asserting on every checkout means a mutation can only affect the *current* checkout, never a later one.

**No residual gap.** The pool hooks cover every connection the pool owns, and that is now *every statement this plugin issues against its own tables* — the `cluster_lock` INSERT/UPDATE/DELETEs ride the pool exactly like cache writes do, so they get `before_acquire` re-assertion on every checkout. The lock opens **no long-lived connection outside the pool at all** — the liveness beacon was the last one, and it is gone (§5.1) — so there is no off-pool session with a durability setting to maintain, and neither the assertion nor the interval re-assertion the old lock session carried is needed. The residual risk DESIGN §11 used to record for that session is retired rather than accepted.

`PG-LOCK-009` asserts the override against a database whose own default is `off`; `PG-SPEC-005` asserts the correction on the checkout *after* an external mid-session flip, with `pool_max_size: 1` so the connection handed back is provably the same one.

A connection on which `SET synchronous_commit = on` fails (e.g. insufficient privilege to alter the GUC) surfaces as a provider error at connect time (§9) rather than silently proceeding with an unverified durability setting.

### 3.5 Standalone Lock Provider

The cluster wiring crate (`cf-gears-cluster`) already implements config-driven per-primitive routing (`cpt-cf-clst-fr-routing-per-primitive`) — `cluster/src/wiring.rs`'s `ClusterWiring::from_config` dispatches a profile's `lock` binding through `ProviderRegistry::lock_provider(name)` and calls `ClusterLockProvider::build_lock` if a provider is registered under that name, completely independently of whichever provider serves that profile's `cache`. That mechanism is real and already works; what's been missing is a plugin that registers something under `lock_provider("postgres")`. This plugin now does, via a second, independent provider trait implementation.

**`PostgresLockProvider`** implements `ClusterLockProvider` (`provider() -> "postgres"`). Its `build_lock(options)` deserializes `options` into `PostgresLockConfig` — a config type scoped to only what the lock primitive needs (`connection_string`, `pool_max_size`, `pool_acquire_timeout_ms`, `schema`, `lock_reaper_interval_ms`, `lock_name_cardinality_warn_threshold`, `pgbouncer_transaction_mode`, `replication_mode`; no `cache_reaper_interval_ms` or `read_cache_capacity` — those don't exist here since there's no cache half) — and constructs a **standalone** `PostgresLockPlugin` (§3.1: `lock/mod.rs`) with its own dedicated pool.

**Always standalone, never shared.** Per the SDK provider trait's own contract ("non-cache providers do not receive the cache backend" — `cluster-sdk/src/provider.rs`), `PostgresLockProvider` never attempts to detect or reuse a pool from a co-located `cache: { provider: postgres }` binding in the same profile, even when both point at the same `connection_string`. This is a deliberate simplicity/independence trade-off: sharing would couple two providers the SDK explicitly designed to be independent, and would need its own lifecycle-ownership story (which provider's `stop()` closes the shared pool?). The cost is a second small pool (default `pool_max_size: 5`) when both primitives happen to point at the same database — considered acceptable relative to the coupling avoided. An operator who wants combined cache+lock sharing one pool still has that option: bind `cache: { provider: postgres, ... }` and omit `lock` entirely, letting the omit-default auto-wrap use the SDK's `CasBasedDistributedLockBackend` over the shared cache instead of the native lock.

**What the standalone path builds, relative to the combined `PostgresClusterPlugin` (§3.2):**

| | Combined (`PostgresClusterPlugin`) | Standalone (`PostgresLockPlugin`) |
|---|---|---|
| Migrations run | `0001_cluster_cache.sql` + `0002_cluster_lock.sql` | `0002_cluster_lock.sql` only |
| Dedicated LISTEN connections | 2: cache watch (`cluster_cache_changes`) + lock release-wake (`cluster_lock_released`) | 1: lock release-wake (`cluster_lock_released`) only — no cache half, so no cache-watch connection |
| Background tasks | Cache TTL reaper, lock TTL reaper, cache-watch LISTEN task, lock release-wake LISTEN task | Lock TTL reaper, lock release-wake LISTEN task |
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

A background task wakes on a configurable interval (default: 10 seconds) and deletes every expired entry, in bounded chunks:

```sql
DELETE FROM cluster_cache
WHERE key IN (
    SELECT key FROM cluster_cache
    WHERE expires_at IS NOT NULL AND expires_at <= now()
    ORDER BY expires_at LIMIT n FOR UPDATE SKIP LOCKED
)
RETURNING key;
```

For each deleted key, the task issues `NOTIFY cluster_cache_changes, 'E:<key>'` so watchers receive `CacheWatchEvent::Event(CacheEvent::Expired { key })`. Each chunk's delete and its `NOTIFY`s share one transaction, so a row is never deleted without its `Expired` event nor the reverse (§4.1).

A sweep loops chunks until one comes back short, so a large expired backlog is still cleared in full while no single transaction row-locks an unbounded number of rows or runs an unbounded `NOTIFY` burst — an unbounded `DELETE ... RETURNING` would make a concurrent `put`/`put_if_absent` on a key caught mid-batch wait out the remaining backlog's `NOTIFY` round-trips rather than a quick row lock, and would roll the whole batch back on any single failure, leaving the tick with zero forward progress. Committing per chunk means a failing chunk costs only its own rows. `SKIP LOCKED` keeps the per-instance reapers from serializing behind each other on the same chunk (and makes the outer delete skip, rather than clobber, a row whose `put` is in flight), and `cancel` is re-checked between chunks so shutdown is not held up mid-backlog.

The reaper is driven by a `CancellationToken`; it self-terminates when cancelled. It uses one connection from the write pool per chunk, releasing it immediately after. Its interval uses `MissedTickBehavior::Delay`, so a sweep that overruns the interval restarts the cadence from its completion instead of firing the missed ticks back-to-back.

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

`features().prefix_watch` is `false`, so the capability resolver rejects `CacheCapability::PrefixWatch` at startup for this backend. Consumers needing prefix-watch semantics use the `PollingPrefixWatch` polyfill over `scan_prefix` (ADR-010).

**Empty / unrecognized payload — Reset.** The listen_task interprets `payload.is_empty()` (or any payload not matching `<event>:<key>`) as a `Reset` signal, broadcasts `CacheWatchEvent::Reset` to every active watcher, and clears all watcher subscriptions (consumers must resubscribe). This matches ADR-003's overflow mapping for Postgres. It is the fallback for a bare `NOTIFY` from an external writer or a future format — **not** the NOTIFY-queue-overflow path: overflow aborts the committing *producer* transaction with an error and delivers no notification. Overflow does not disconnect the listener or emit a `Reset`; it surfaces to the *writer* as that write's `Provider` error and in the PostgreSQL server logs, not through this LISTEN-side recovery.

**Connection loss — Reset.** If the dedicated LISTEN connection drops, the listen_task attempts reconnect with exponential backoff. On successful reconnect, it broadcasts `CacheWatchEvent::Reset` before resuming event delivery, signalling that consumers may have missed events during the gap. If reconnect fails beyond the configured retry limit, it broadcasts `CacheWatchEvent::Closed(ClusterError::Provider { kind: ConnectionLost, .. })` and exits.

The Postgres cache is read-through: every `get` hits the database directly. There is no in-process read cache — consumers with hot-key, high-read, staleness-tolerant workloads should route that primitive to a backend built for it (e.g. Redis) rather than expect this plugin to double as a fast local cache; see §11 for the rationale.

### 4.4 scan_prefix

`scan_prefix(prefix)` is implemented via `LIKE prefix%`. The plugin escapes `%`, `_`, and `\` in the caller's prefix before appending `%`, so wildcard characters in `prefix` are matched literally rather than being interpreted by `LIKE`. This is used by `PollingPrefixWatch` to enumerate keys for diffing. Performance degrades with keyspace size; the partial index on `expires_at` does not help here. High-volume prefix scans should use a backend with native prefix watch (Redis, NATS, etcd).

### 4.5 Consistency Declaration

`consistency()` returns `CacheConsistency::Linearizable`. All cache operations run at Postgres's default `READ COMMITTED` isolation level, which provides linearizability for single-row operations (the only kind the cache uses). The CAS path uses an `UPDATE … WHERE version = $expected`, which is an atomic compare-and-set at the row level regardless of isolation level. Under `READ COMMITTED`, concurrent updates do not produce write skew on single rows.

## 5. Distributed Lock Implementation

### 5.1 The Lease Row

A lock is held **iff** a `cluster_lock` row exists whose `expires_at` is in the future. That is the whole predicate: `expires_at` is the only liveness authority, so no process vouches for a lease and no process's death ends one (invariant I7, cluster DESIGN.md §3.19.1). The row is the sole arbiter of ownership, and it *is* the lease record of cluster DESIGN.md §3.19.1 held in columns rather than in an encoded cache value — `owner` and `fence` are the token the holder presents, `expires_at` is its deadline (§2.1).

Both halves of `DistributedLockBackend` are served from that one lease. `acquire`/`acquire_waiting` hand the token back for a caller that must renew from somewhere other than the acquiring task — a remote one, or a different cluster replica; `try_lock`/`lock` take the same lease and wrap the token in a guard task, because `LockGuard`'s fields are private and cannot carry one. Every acquire, renew, and release is a single statement against the write pool, with no session affinity and no in-process state that is load-bearing for exclusion.

#### How mutual exclusion works

Three mechanisms cooperate inside the acquire statement, and the primary key does the least work of the three:

1. **`PRIMARY KEY (name)` detects the conflict.** It guarantees at most one row per lock name, giving `ON CONFLICT` something to fire on. It decides nothing.
2. **The row lock serializes.** On conflict Postgres takes an exclusive lock on the conflicting tuple, so a competing transaction holding it makes us *block* until it commits or aborts. This is the serialization point.
3. **The `WHERE` decides.** After taking the row lock, Postgres re-reads the **latest committed version** of the row and evaluates the predicate against it — not against the snapshot the statement started with.

Step 3 is what makes it correct: two acquirers cannot both observe the lock as free, because the loser re-evaluates against the winner's already-committed state. `RETURNING` is the answer — a row means acquired (whether by insert or by steal), zero rows means contended, with no third case. Two tasks in the *same* process race exactly as two instances do, which is why no in-process claim registry is needed to arbitrate them.

**This requires `READ COMMITTED`, and the plugin asserts it at startup** (`pg_setup::assert_read_committed`, §3.2). Step 3's re-read is `READ COMMITTED` behaviour; under `REPEATABLE READ` or `SERIALIZABLE` the transaction snapshot cannot advance, so instead of re-evaluating, Postgres raises SQLSTATE `40001` and the caller would have to retry. The check lives in shared startup validation rather than in the lock module because the cache's `put_if_absent` — and so leader-election failover via `CasBasedLeaderElectionBackend::claim` — already depended on exactly the same idiom, unguarded. Asserting rather than *enforcing* (one `SET SESSION CHARACTERISTICS` in `after_connect` would do it) is deliberate: silently overriding an isolation level an operator set on purpose hides a mismatch that failing fast surfaces. `PG-SPEC-011` covers both directions.

Three variants look equivalent and are not: a `SELECT` to check followed by an `INSERT` is a check-then-act race; letting the primary key's unique violation *be* the contention signal cannot express "steal if expired", making a lapsed lock permanently unacquirable; and `SELECT … FOR UPDATE` then `UPDATE` needs an explicit transaction and locks nothing when no row exists yet, so two first-time acquirers both proceed and one takes a unique violation.

#### What the liveness beacon was, and why it is gone

An earlier revision carried one per-incarnation advisory lock on a dedicated connection outside the pool — a **liveness beacon**, stamped onto every row this instance wrote, whose disappearance from `pg_locks` published "the process that took this lock is gone". The acquire predicate joined against it, so a crashed holder's lock became stealable *before* its TTL, and an advisory lock was the only Postgres primitive that was simultaneously established in one statement, readable from any other session in SQL, and deleted by the server the instant the session ended.

It was sound precisely when the process holding the beacon was the process using the lock. Brokered, that stops being true: the cluster gear's beacon would vouch for locks held by other, live consumers, so its restart would revoke the fleet's locks. The predicate therefore had to change for remote clients — and **it changed for everyone**, because keeping it for in-process acquisitions and dropping it for brokered ones would mean the same code and the same config reclaim a dead holder's lock in milliseconds in one deployment and at the TTL in another: two timings, and a class of bug that reproduces in only one profile (cluster DESIGN.md §3.19.2, Goal 2).

**The price is explicit, not buried:** a crashed holder's lock now lingers until its TTL, in every profile. That is the same bound every non-Postgres backend already had, and it is a reason to keep lock TTLs tight rather than to keep two mechanisms. ADR-012 records the decision and both rejected alternatives; `PG-LOCK-023` asserts the resulting timing in both directions — reclaimed at the TTL, and *not before*.

Three things went with it, each with its own cost stated where it lands: the sub-TTL reclaim above, the shutdown drain (§10), and the incarnation-keyed orphan sweep (§5.2).

#### SQL contract per operation

**Acquire** — one statement, any pool connection:

```sql
INSERT INTO cluster_lock (name, owner, fence, acquired_at, expires_at)
VALUES ($1, $2, 1, now(), now() + ($3::bigint * interval '1 millisecond'))
ON CONFLICT (name) DO UPDATE
   SET owner       = EXCLUDED.owner,
       fence       = cluster_lock.fence + 1,
       acquired_at = EXCLUDED.acquired_at,
       expires_at  = EXCLUDED.expires_at
 WHERE cluster_lock.expires_at <= now()
RETURNING fence;
```

**One branch, one indexed comparison.** The predicate used to be a `CASE` — not an `OR`, whose operand evaluation order SQL does not guarantee — purely so the cheap comparison could short-circuit a `pg_locks` scan off the uncontended path. With the beacon gone there is no scan to short-circuit, and the acquire path loses its only unindexed access: `pg_locks` is a function scan over `pg_lock_status()` with no index, so a contended acquire was `O(advisory locks on the server)`. `PG-SPEC-012` now holds the *plan* to issuing no such scan on any path — uncontended, steal, or contended — which is `L2`'s exit criterion and a stronger property than the short-circuit it replaces.

**`fence` is read off the row, not bound by the acquirer.** Postgres evaluates `cluster_lock.fence + 1` against the latest committed version of the conflicting tuple (step 3 above), so two racing stealers cannot land on the same fence: the loser blocks on the winner's row lock, re-evaluates `WHERE cluster_lock.expires_at <= now()` against the winner's committed row, and matches nothing. `RETURNING fence` is what makes the token mintable from the same statement — the INSERT path returns 1, the steal path the incremented value, zero rows means contended. This is what the cache-backed default needs an explicit CAS on `CacheEntry.version` to achieve; here the guarded upsert *is* the CAS.

**Renew** — authoritative against a single truth, no probe:

```sql
UPDATE cluster_lock
   SET acquired_at = now(),
       expires_at  = now() + ($1::bigint * interval '1 millisecond')
 WHERE name = $2 AND owner = $3 AND fence = $4
   AND expires_at > now()
RETURNING 1;
```

Zero rows is `ClusterError::LockExpired`, whichever fence failed — lapsed, stolen and never-yours are indistinguishable and all three mean the caller must stop acting as the holder. `owner` keeps one holder from renewing another's lease; `fence` guards against a **successor**, which stole at `fence + 1`; `expires_at > now()` refuses to resurrect a lease the fleet is already entitled to treat as free.

The predicate is entirely over stored state, so **every replica gives the same answer** — that is invariant I7, and `PG-LOCK-021` asserts it by renewing through a handle built after the acquiring one had stopped. The third fence that used to be here, "has *this instance's* beacon been replaced since the acquisition?", is gone with the beacon: a renew now fails only because the lease moved on, never because the process that took it had a bad moment.

**Release** — one statement: a `DELETE … WHERE name = $1 AND owner = $2 AND fence = $3` and the `pg_notify` in a single data-modifying CTE, so releasing costs one pool checkout and the wake is atomic with the row's disappearance. Liveness is deliberately *not* in the predicate: a lapsed row still bearing this token is still this holder's, and removing it frees the name immediately instead of making the next acquirer steal it. **Absence is `Ok`** — a retried release, one bearing a fenced-out token, and one whose lease the sweep already reclaimed all delete nothing and all succeed (`PG-LOCK-025`). Selecting the notify `FROM` the delete's CTE is what keeps that quiet: a no-op release sends nothing.

**This plugin takes no advisory lock at all, anywhere** — the single sharpest way to state the design, and a useful invariant to check any change against. `PG-LOCK-012` and `PG-LIFE-003` both assert the empty set directly, so a reintroduced one shows up as a test failure rather than as design drift.

Be exact about what that does and does not mean, since "lock" names two different things here. Releasing a lock still means **deleting its row**, and two paths do that: `release` (fenced on the token) and the TTL sweep. What is gone is the *advisory-lock* release — nothing per-lock was ever `pg_advisory_lock`ed, so there is no session-scoped unlock to pair with it, and since the beacon was removed there is no advisory lock outside the pool either.

The consequence is the point: a row delete is something **any** instance can perform, whereas an advisory unlock could only ever be issued by the session that took it. An expired or unvouched row is therefore stealable by the acquire predicate itself, evaluated by whoever asks — no reclaim step, and no reason reclamation has to route back to the instance that held the lock. That is what lets a crashed *or merely wedged* holder's lock be taken by anyone rather than only by a healthy reaper on the owning instance (`PG-LOCK-014`).

#### No in-process registry survives

There used to be one — `local_holders`, name to `holder_id` — kept for exactly one consumer, §5.2's orphan sweep, which had to distinguish a row with a live local guard from a row whose acquirer went away. Both went together: the sweep was keyed on the beacon, and with no incarnation key to filter on there is nothing for the registry to exempt.

Nothing here now remembers which locks this process holds, because nothing needs to: every predicate is over stored state. That is also what makes any replica able to serve any lease operation.

### 5.2 TTL Enforcement and Garbage Collection

`expires_at` is the lease deadline, computed in SQL against the **database** clock at insert and at every renew (§2.1). Reclamation happens on two independent paths, and only the first is load-bearing for exclusion:

1. **Any acquirer's own predicate.** A lapsed row is taken in the acquiring statement itself (§5.1). No sweep has to have run, and no instance has to cooperate. This is the whole guarantee.
2. **The background reaper**, which is garbage collection plus a promptness optimisation: it deletes expired rows so the table does not grow, and NOTIFYs their names so blocked waiters wake instead of sitting out a heartbeat. A sweep that never runs costs table growth and slower wake-ups, never a double-hold.

    Each sweep deletes in bounded batches (`DELETE ... WHERE name IN (SELECT name ... ORDER BY expires_at LIMIT n FOR UPDATE SKIP LOCKED)`), looping until a batch comes back short. `SKIP LOCKED` keeps the per-instance reapers from serializing behind each other, and `cancel` is re-checked between batches so shutdown is not held up mid-backlog.

    **Wake schedule.** After each sweep the reaper sleeps until the earlier of the next metrics tick (`lock_reaper_interval_ms`) and the next row's deadline, read as `SELECT extract(epoch FROM (min(expires_at) - now()))` — an index-only read, with the subtraction done in Postgres so the delay never depends on this instance's wall clock. (It used to carry `now()` back on the same select list, as the orphan sweep's fence; that sweep is gone, and with it the only reader of the database clock here.) The interval is the *cap*: it keeps `cluster_postgres_lock_active_names` and the cardinality WARN on their configured cadence, and only these interval-boundary wakes do the gauge work. `min(expires_at)` only *shortens* an individual sleep.

A sleep is computed from the table as it looked at wake time, so on its own that shortening would miss a lock whose entire lifetime fits inside one sleep (TTL ≲ `lock_reaper_interval_ms`). `try_acquire` and `renew` therefore signal the reaper (an in-process `tokio::sync::Notify`) once their write is committed — but **only when the TTL they wrote is shorter than `lock_reaper_interval_ms`**, which is exactly that condition. The signal is in-process only, and that is sufficient rather than partial: the sweep is promptness only, so a hint no other instance hears costs at most a waiter's heartbeat. Both the expiry-driven and the signalled wake are floored at 100 ms (or at `lock_reaper_interval_ms` when that is shorter), so many staggered deadlines — or a burst of acquisitions — coalesce into one wake instead of one each. A **lost** signal costs at most one late sweep; a **spurious** one is not symmetric, which is why the gating is not merely an optimisation. `Notify` holds a single permit, so signalling on every write keeps the `notified()` branch permanently ready and collapses every subsequent sleep to the floor — an instance renewing a couple of hundred leases a second would run a full iteration every 100 ms instead of every interval, roughly fifty times the intended database load, permanently, on every instance in the fleet. Expiry is deliberately **not** bucketed into coarse slices: for a lock the TTL is the crash safety net, so rounding deadlines up to a shared boundary would let a stale lock block waiters for up to a full bucket past its TTL.

#### What garbage collection no longer does

Two liveness responsibilities used to live on this wake loop. Both went with the beacon (§5.1), and stating what they cost is the point of this subsection rather than a footnote to it.

**The orphan sweep.** Acquire is a single statement, so there is no compensating unlock to issue if the caller goes away. A `try_acquire` future dropped after its INSERT committed — `lock()`'s per-attempt timeout elapsing mid-acquire, a cancelled consumer task, a runtime shutting down — leaves a lease this process owns and no longer has a token for. Under the beacon this was *worse* than a lapsed lease and needed its own mechanism: the row was unexpired **and** vouched for by a live beacon, so nothing in the fleet would steal it — including this instance, whose own next acquire of that name read its own orphan as a live holder. The name was wedged for both sides until the TTL, and an incarnation-keyed `DELETE` (every row bearing this beacon whose `holder_id` was not in `local_holders`, fenced on an `acquired_at` from the previous reaper wake) reclaimed it early.

With no beacon there is no incarnation key to filter on and no local registry to exempt, and — more to the point — **an abandoned row is now just a lapsed lease at its deadline**, reclaimed by the TTL sweep above like any other. The cost is that its name is taken until its TTL rather than until the next interval wake. That is the same trade as the sub-TTL reclaim of a crashed holder (§5.1), applied to a local mishap rather than a remote crash, and it is bounded by the same thing: the TTL the caller chose.

**Detecting a dead holder locally.** The beacon task pinged its own connection once a second, so an instance learned within about a second that it could no longer defend what it held, and purged `local_holders`. Nothing replaces that, and nothing needs to: there is no local state to purge, and a holder learns its lease is gone the way the SDK has always specified — at its next `renew`, which returns `LockExpired` (`LockGuard` offers no asynchronous lost-lock signal, by design). The instance can no longer be *wrong* about what it holds in a way that matters, because holding is no longer a local fact.

**And the "immediate on clean disconnect" bound is gone with them.** The honest statement of crash recovery used to be "immediate on clean disconnect, keepalive-bounded otherwise, TTL-bounded in the worst case", which is why the beacon set `tcp_keepalives_*` on its own session. It is now simply **TTL-bounded, always**, in every profile — no socket timers, no server-side levers, nothing to tune. Recovery promptness is under caller control through the lock TTL, which is a per-acquisition parameter on the trait: an operator wanting faster reclamation shortens the TTL. `PG-LOCK-023` asserts both directions of that bound.

### 5.3 Blocking lock()

`lock(name, ttl, timeout)` retries the acquire statement and, between attempts, waits on the in-process `ReleaseWaiters` registry for an early wake:

```
loop {
    try the conditional upsert (§5.1) → a row back? return LockGuard
    if past deadline → LockTimeout
    register interest in `name` with the ReleaseWaiters registry
    wait on (that registration resolving) OR a short heartbeat sleep (250ms)
}
```

No server-side wait is ever issued: no blocking `pg_advisory_lock`, and no `SELECT … FOR UPDATE` held across the attempt. The retry-plus-wake loop is what makes a blocking `lock()` API out of a non-blocking primitive, and it is also what keeps a waiter cheap — a blocked caller holds no connection between attempts.

**What `lock()` reports when it cannot acquire.** Three outcomes, deliberately distinguished, because a caller's response to each differs:

- `ClusterError::Shutdown` — checked before any lock work, so an acquisition arriving after `stop()` has cancelled the shared token answers immediately instead of retrying a backend that is being torn down. `try_lock` takes the same check, so the two agree rather than one reporting `Shutdown` and the other `Provider { ConnectionLost }` depending on how far shutdown had progressed (`PG-LOCK-020`).
- `Provider { ConnectionLost }` — the budget ran out while this instance could not reach the pool. Retried inside the caller's budget, which is what carries a `lock()` through a Postgres failover; this is now the *only* transient case, since an acquisition can no longer fail for a reason local to this process (it used to also cover "no live beacon to stamp a row with", which was the common case rather than the rare one); but if it never clears, the caller is told *that* rather than being handed the `LockTimeout` ordinary contention produces. Retrying is deliberately not given a shorter give-up budget: failover commonly takes 10–30s, and cutting the retry short to improve an error code would trade a real availability property for a cosmetic one.
- `LockTimeout` — the budget ran out while Postgres was answering normally and saying the lock was held. This is genuine contention, and only this case reports it.

The wait does **not** LISTEN on the acquiring connection. `sqlx`'s `PgListener` owns its own single connection and has no public way to adopt an already-checked-out `PoolConnection`, so instead a single **dedicated** `cluster_lock_released` LISTEN connection (opened at `build_and_start`, present in both the combined and standalone plugins — §3.3) runs a fan-out task that `notify()`s the in-process `ReleaseWaiters` registry; each blocked `lock()` caller registers a waiter there and is woken when a `NOTIFY cluster_lock_released` for its name arrives. The 250 ms heartbeat sleep is a safety net against a missed notification (registration racing an already-fired `NOTIFY`, or the listen task momentarily reconnecting): a lost wake only costs latency up to the heartbeat interval, never correctness — the loop always re-attempts the acquire statement itself as the source of truth. A waiter that gives up (timeout or heartbeat-driven re-acquire) deregisters itself from the registry on drop, so no stale waiter accumulates.

This avoids busy-polling: waiters wake promptly when a holder explicitly releases. The TTL sweep also NOTIFYs the names it reclaims, in one batched statement. (It is the only other NOTIFY source left — the orphan sweep, the shutdown drain and the beacon's post-reconnect cleanup were the others, and all three are gone with the beacon.)

**A lapsing lease announces nothing**, which is worth stating because it is the one gap the heartbeat covers: expiry is logical rather than physical, so nothing is written at the deadline and no `NOTIFY` fires until the reaper happens to sweep the row. A waiter listening only to the release channel would sleep past a lease it could have taken, so the retry loop's 250 ms heartbeat is load-bearing rather than merely a safety net against a dropped wake.

### 5.4 PgBouncer Constraint

**Narrower than it once was, but not gone.** Every lock *operation* is now a single statement on the pool (§5.1), which transaction-mode pooling would serve perfectly well; the constraint no longer touches acquire, renew, release, or either sweep. What still needs session affinity is the pair of things this plugin opens outside the pool:

- **The two `LISTEN` connections** (cache watch, and the release wake-up that unblocks a waiting `lock()`), whose subscriptions are session-scoped. Transaction-mode pooling would silently detach them between transactions, leaving watchers and blocked `lock()` callers permanently unwoken.

Narrower again since the liveness beacon was removed (§5.1). The beacon was the other case, and the more dangerous one: transaction pooling would have released its advisory lock between transactions, asserting to the entire fleet that this instance was dead while it still held live locks. That failure mode no longer exists.

The LISTEN connections are opened directly rather than through the pool, so in practice this guards an operator who has put PgBouncer in front of the DSN itself:

- If `pgbouncer_transaction_mode: true` is set in config, `build_and_start` returns `Err(ClusterError::InvalidConfig { … })` naming the LISTEN subscriptions.
- Operators using PgBouncer must either use session pooling mode for the cluster plugin's connection string, or use a different lock backend.

### 5.5 Inspecting Locks (operators)

`cluster_lock` is the supported inspection surface. `pg_locks` is not, and was a strictly worse one anyway: it only ever exposed the two halves of a name hash, which is irreversible, so identifying the lock behind a row meant enumerating candidate names and hashing each.

```sql
SELECT name, owner, fence,
       acquired_at, expires_at, expires_at - now() AS remaining
  FROM cluster_lock
 WHERE expires_at > now()
 ORDER BY name;
```

**This query is now the whole answer**, which it was not before: a row whose `expires_at` was in the future used to be only *possibly* held, because its beacon might have vanished, so establishing whether anyone actually held a lock meant a second join against `pg_locks`. `expires_at` is the only liveness authority now (§5.1), so `expires_at > now()` is held and everything else is not.

One limit to be clear about: what `owner` means depends on how the lease was taken. A brokered acquisition carries the caller's `ClientId`, which is meaningful. An in-process `try_lock`/`lock` mints a fresh UUID per acquisition (§2.1), which identifies the *acquisition* and is not resolvable to a pod, host, or process on its own — deliberately cheaper than a `holder_instance` column duplicating identity already present in log context. `fence` is useful directly: a name whose fence is well above 1 has been stolen after lapsing that many times, which is a lock whose TTL is too short for its critical section.

## 6. Leader Election

This primitive uses the SDK default over the Postgres cache backend.

**Leader election** — `CasBasedLeaderElectionBackend::new(Arc::clone(&cache))`. The cache backend is `Linearizable`, so the consistency guard passes. `LeaderElectionFeatures::linearizable == true`.

The wiring crate's omit-default auto-wrap (DESIGN §3.11) wires this automatically when a profile declares `cache: { provider: postgres }` and omits `leader_election`.

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

    /// TTL reaper interval for cluster_lock — upper bound on the reaper's sleep
    /// and the cadence of its gauge; an imminent expires_at shortens an
    /// individual sleep (§5.2). Default: 5s.
    #[serde(default = "default_lock_reaper_interval")]
    pub lock_reaper_interval_ms: u64,

    /// Set to true to get an InvalidConfig error at startup rather than silent
    /// mis-behaviour if the connection string points to a PgBouncer in
    /// transaction mode. Default: false.
    #[serde(default)]
    pub pgbouncer_transaction_mode: bool,

    /// Distinct concurrently-held lock-name count past which the lock reaper
    /// logs `cluster.lock.name_cardinality_high` (WARN) and the
    /// `cluster_postgres_lock_active_names` gauge should be alerted on.
    /// Default: 1000 (see DESIGN §8/§11 — a cardinality signal, and the
    /// §2.1).
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

**`PostgresLockConfig`** (standalone lock provider, §3.5) is a separate, smaller config type — it only carries the fields the lock primitive actually uses, not the cache-only ones (`cache_reaper_interval_ms`, `read_cache_capacity`):

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
This is the operational counterpart to the `pg_locks` scan-cost risk
documented in §11: that scan is `O(advisory locks in the cluster)` and is paid
only on contended acquires, so this gauge is the load proxy a Grafana
panel/alert reads `cluster_lock_op_duration_seconds{op="try_lock"}` p99
against. It is a plain count, not a per-name breakdown — lock names
are never used as label values (the cardinality rule below). When the count
exceeds `lock_name_cardinality_warn_threshold` (config, §7; default 1 000), the
plugin logs `cluster.lock.name_cardinality_high` (WARN, rate-limited to once
per reaper interval) so the same condition is visible in logs even without a
dashboard.

The lock reaper's interval wake also samples `pg_notification_queue_usage()` and
emits `cluster_postgres_notify_queue_usage{provider}` (gauge, `0.0..=1.0`) — the
fraction of Postgres's notify queue in use. The queue is **cluster-wide**, shared
by every database on the server, and at 100% it does not shed load: it fails the
committing transaction of every `NOTIFY` on the server (§11). Past 25% the reaper
also logs `cluster.provider.notify_queue_high` (WARN). Both live on the *lock*
reaper's cadence deliberately: the value is a property of the whole server rather
than of either primitive, so it wants exactly one sampler per instance, and the
lock reaper is the one that runs in both plugin shapes (the standalone lock plugin
has no cache half). This is the only signal that names the *cause* of a filling
queue — because the tail advances only as fast as the slowest listener anywhere on
the server, the deployment that fills it is frequently not the one that first
fails, and §11's advice to watch write/provider errors only reports the victims.

All emission is subject to the `METRIC_LABEL_ALLOWLIST` cardinality rule: keys
and lock names are NEVER used as metric label values, only as span attributes
and log fields.

Log events follow the `cluster.{primitive}.{event}` naming scheme
(`OBSERVABILITY.md` §6). This plugin emits `cluster.watch.reset` (WARN),
`cluster.provider.error` (ERROR), and — all plugin-local —
`cluster.lock.name_cardinality_high` (WARN),
`cluster.provider.replication_async` (WARN, once at startup, §3.6),
`cluster.provider.notify_queue_high` (WARN, §11),
`cluster.provider.notify_queue_readable` (INFO, §11), plus the
garbage-collection events below. It has no leadership transitions of
its own to report (leader election is the SDK default over this plugin's cache,
and emits `cluster.leader.transition` itself).

**Garbage-collection events** (all plugin-local, all carrying the affected
`lock` as a log *field*, never as a metric label). The four
`cluster.lock.beacon_*` lines this table used to carry are gone with the beacon
they reported on (§5.1), and so is `cluster.lock.drain_incomplete` with the
shutdown drain (§10) — an operator alerting on any of them should drop those
alerts rather than expect silence to mean health:

| Event | Level | Meaning |
|---|---|---|
| `cluster.lock.orphan_rows_reclaimed` | WARN | The reaper's orphan sweep deleted rows this instance wrote but holds no guard for — acquisitions cancelled after their row committed (§5.2). Each was wedging its name for the whole fleet until its TTL. A steady stream means `lock()` timeouts are landing mid-acquire often enough to be worth widening |

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
| SQLSTATE `54000` (`program_limit_exceeded`), and `23514` (`check_violation`) on `cluster_cache_key_len_check` / `cluster_lock_name_len_check` | `Other`, with the message rewritten to name the 2048-byte limit (§2.1) and the key that has to shrink. Both mean "an over-long indexed key reached Postgres"; the server's own text is opaque about the cause (`54000` reports an index row size against a btree maximum, `23514` names only the constraint). Neither is retryable. This is a backstop — the Rust guards reject such keys before the write — and `23514` matches on the constraint name so an unrelated future CHECK is not mislabelled as a length problem |
| Any other `sqlx::Error` | `Other` |

Connection loss during a LISTEN reconnect loop is surfaced as `Provider { kind: ConnectionLost }` to affected watchers after the retry budget is exhausted.

## 10. Shutdown Sequence

`PostgresClusterHandle::stop()` follows DESIGN §3.13:

1. Cancel the `CancellationToken` shared by all background tasks (cache reaper, lock reaper, cache-watch LISTEN task, lock release-wake LISTEN task). Await each task's `JoinHandle`. Cancellation also unparks each held lock's guard task promptly, rather than leaving it waiting on a consumer that may never act.
2. Send `CacheWatchEvent::Closed(ClusterError::Shutdown)` to all active watcher channels (dispatched directly against the watch registry before the LISTEN task is awaited, so every watcher observes it prior to `stop()` returning).
3. Drop each dedicated `PgListener` (cancelling its task drops the listener, which closes its socket). No explicit `UNLISTEN *` is issued — dropping the connection ends the session, which is functionally equivalent (a closed backend cannot deliver further notifications).
4. Close the `sqlx::PgPool` under a bounded `POOL_CLOSE_TIMEOUT` (10s — see §11's note on unbounded pool statements).

    **Held lease rows are deliberately left in place, and that is the whole of cluster DESIGN.md §3.19.2: a cluster restart is not a lease event.** This step used to hand back every lock still held, in one `DELETE FROM cluster_lock WHERE <this incarnation's beacon>` followed by a batched `NOTIFY cluster_lock_released` of those names, then close the beacon connection. That was a clean handover while the process holding a lock was the process using it — and it is a fleet-wide revocation the moment locks are brokered, because the rows being deleted belong to other, live consumers.

    So every remaining lease now lapses on its own deadline, renewed in the meantime by whichever holder still owns it, through whichever replica answers next (invariant I7). `PG-LOCK-021` asserts exactly this, and asserts the half that matters: a lease acquired through the stopping handle is still renewable through a handle built *afterwards*. It is the inverse of what that scenario used to assert.

    **The cost, stated:** a name this instance held is taken until its TTL rather than released the moment we let go, so a waiter elsewhere waits out the deadline instead of being woken by a `NOTIFY`. That is the same bound a crashed holder now has (§5.1) and the same one every non-Postgres backend always had.

    Two things fall out of the removal that are worth stating so they are not rediscovered as gaps. There is no longer any **ordering** constraint in this sequence beyond "close the pool last" — the drain needed both the beacon key and the pool live, and that was the reason the beacon was shut down separately from the shared cancellation token. And the orphaned rows the drain also reclaimed (rows with no live guard, left by an acquisition cancelled after its INSERT committed) are now left to the TTL sweep like any other lapsed lease (§5.2).

No remote cleanup is performed on a best-effort basis: held claims and locks lapse via their TTL once the connections drop (`cpt-cf-clst-fr-shutdown-ttl-cleanup`).

## 11. Risks / Trade-offs

**[Risk: LISTEN/NOTIFY does not scale under high concurrent write rates]** NOTIFY acquires a global exclusive lock on commit. Under > ~1000 notifying transactions/sec, this becomes a bottleneck. Mitigation: the cache plugin is not recommended for high-throughput subscriber lease workloads (use Redis cache for those — DESIGN §4.2). Queue overflow aborts the *committing* transaction, so it surfaces as the failing write's `Provider` error (and in the PostgreSQL server logs) — monitor those rather than `cluster_watch_resets_total`, which counts LISTEN connection gaps, not overflow.

**[Risk: a NOTIFY-heavy co-tenant degrades every other deployment on the server]** The costs above are not private to one database. Three of them are cluster-wide properties of the Postgres server, so any two deployments on the same server share them by construction — independent of how their tables, schemas, or channels happen to be arranged:

- **The commit-time queue lock is cluster-wide.** Every notifying transaction takes it regardless of channel or database, so the ~1000 notifying-txn/sec ceiling above is a budget *shared* by every deployment on the server rather than one each.
- **The queue SLRU is cluster-wide, and its tail advances only as fast as the slowest listening backend anywhere on the server.** One wedged listener fills it, and at 100% the queue does not shed load — it **fails the committing transaction** of every `NOTIFY` on the server. The deployment that fills the queue is frequently not the one that first fails.
- **Signal fan-out is per-database.** Postgres does not track which backend listens to which channel in shared memory, so a notification wakes every backend with any active `LISTEN` in that database.

What the plugin does about the part it controls:

- **Neither LISTEN reader loop ever awaits I/O.** A session that stops reading is what pins the queue tail for the whole cluster, so not stalling is the difference between being a victim and being the cause. The two loops buy it differently:
    - The **cache** loop spawns terminal `Reset` broadcasts rather than awaiting up to `TERMINAL_GRACE` per non-draining watcher (`cache::watch::WatchRegistry::dispatch_from_listener`). Spawning costs ordering, so the registry pays for it explicitly: an async mutex serializes every terminal broadcast against `close_all`, and a `closed` latch makes the first `Closed(..)` final. That is what keeps a spawned `Reset` from landing *after* the terminal `Closed` (which the SDK's `CacheWatch` contract forbids) or from emptying the registry so `close_all` finds nothing to close (§10 step 2 / `PG-LIFE-004`). It also gives `stop()` something to wait on, so a detached broadcast can no longer outlive it. A `watch()` arriving after the latch is answered with its terminal event immediately rather than registering into a registry nothing will dispatch to again.
    - The **lock** loop does no work at all on the reader thread: one in-process registry hit to wake local `lock()` waiters for that name, and nothing else. It once did a second thing — nudging the reaper to reconcile locks whose rows another instance's sweep had deleted — which went through two wrong shapes before disappearing entirely: first a detached per-name reclaim task (one pool checkout per matching name, against a default `pool_max_size` of 5, with a `cancel` check read only at spawn time and nothing joining it), then a coalescing signal to a per-wake audit. Neither is needed now: with the lease row as the arbiter there is nothing to reconcile, because whoever deletes the row frees the name for everyone (§5.1).
- **Both sweeps' notifications are batched** — one `pg_notify … FROM unnest(...)` per batch rather than per row, for the lock sweep (`lock::notify::notify_released_many`) and now for cache expiry too (`cache::watch::notify_many`, called once per `cache::reaper::sweep_chunk`). The cache path previously issued one `pg_notify` round-trip per expired key *inside* its chunk transaction, which worked directly against the point of chunking: a chunk's row-lock hold time scaled with the number of expired keys instead of staying flat.
- **The occupancy is monitored before it is fatal.** The lock reaper samples `pg_notification_queue_usage()` once per `lock_reaper_interval_ms`, records `cluster_postgres_notify_queue_usage`, and logs `cluster.provider.notify_queue_high` (WARN) past 25% — well below Postgres's own server-side warning at 50%, because this is the only signal that names the cause rather than a downstream victim. Alert on it. A sampling *failure* reports the full `cluster.provider.error` pair once per run of failures and then keeps counting `cluster_provider_errors_total` alone, so an unreadable queue does not emit one ERROR per interval indefinitely; its resource field is the fixed `pg_notify_queue` rather than `cluster_lock`, which the value has nothing to do with. The matching recovery is logged once as `cluster.provider.notify_queue_readable` — INFO, not WARN, because the end of a fault is not itself actionable and `cluster_provider_errors_total` already carries the exact fault count.

**This risk is about NOTIFY rate, not about co-location.** Several services sharing one database — and one schema — is a normal, supported arrangement: sharing `cluster_cache`/`cluster_lock` means sharing a coordination namespace, which is usually the intent, and a consumer that wants logical separation inside it gets that from the SDK's per-primitive `scoped(prefix)` wrappers rather than from anything in this plugin. Nothing here needs co-tenants to be told apart.

The variable to manage is therefore the *aggregate* notifying-transaction rate the server sees, not how the tenants are laid out. A write-heavy cache tenant is the thing to move: to Redis per the per-primitive-backend guidance below, or to its own Postgres **instance** — the only boundary that genuinely partitions the queue and the commit lock, since a separate database on the same server does not.

**[Retired: hash collision in lock names]** Lock names are no longer hashed at all — the name is the `cluster_lock` primary key, compared as text (§5.1), so two distinct names cannot exclude one another under any circumstances. The `cluster_postgres_lock_active_names` gauge and the `cluster.lock.name_cardinality_high` WARN remain, now purely as a cardinality signal (and as the input to the deferred index decision, §2.1).

**[Risk: `pg_locks` scan cost on the contended path — shipping on measurement]** The acquire predicate's liveness check is a function scan over `pg_lock_status()` with no index, so it is `O(advisory locks in the cluster)`. Three things bound the exposure: the `CASE` short-circuits it off the uncontended path entirely (`PG-SPEC-012`), the subplan is correlated against a single row located by primary key, and contended retries are already rate-limited to roughly four per second per waiter by the NOTIFY-plus-heartbeat design (§5.3). `PG-SPEC-014` records the baseline as an artefact rather than a threshold — on a CI container it measures roughly 0.6 ms at a handful of advisory locks rising to ~2.7 ms at 5000, i.e. the linear scaling the shape predicts, at absolute values far below any plausible lock TTL. **The signal to watch** is `cluster_lock_op_duration_seconds{op="try_lock"}` p99 read against `cluster_postgres_lock_active_names`; note the histogram carries no `result` dimension (deliberately, to mirror the CAS-based default backend's signal set), so contended and uncontended acquires share one distribution and a rise is diluted. **The pre-designed exit**, should it ever look bad: skip the liveness check for rows renewed recently (`WHEN cluster_lock.acquired_at > now() - $staleness THEN false`), paying the scan only for the suspicious set. Correctness is unaffected because skipping is strictly conservative — it declines to steal, never steals wrongly — at the cost of making crash detection `min($staleness, TTL)` rather than immediate.

**[Risk: PgBouncer transaction mode mis-configuration]** Silent mis-behaviour if an operator uses transaction-mode PgBouncer without the `pgbouncer_transaction_mode: true` config flag. Lock and cache operations themselves are fine — every one is a single statement on the pool — but the `LISTEN` connections are not: transaction pooling would detach their session-scoped subscriptions, leaving cache watchers and blocked `lock()` callers permanently unwoken (see §5.4). Narrower than it was, since the beacon's advisory lock was the sharper edge here. Mitigation: the startup validation flag; documentation.

**[Trade-off: prefix_watch is polling-based]** `watch_prefix` is serviced by `PollingPrefixWatch`, not a native LISTEN/NOTIFY subscription. This means prefix watch events have a latency of up to the poll interval (default 5s) and the poll cost is N `get` calls per interval. Use cases that require sub-second prefix-change propagation should use a backend with native prefix watch (etcd, NATS).

**[Retired: all lock operations serialize on one session]** They no longer do. Every lock statement runs on the write pool, so lock throughput is bounded by pool width like everything else, and the previous escape hatch (a set of sessions with lock-name-hash affinity) is moot. The property that motivated the single session — a held lock costing no pool connection — is unchanged and now stronger: a held lock costs no connection at all.

**[Trade-off: a crashed holder's lock lingers until its TTL]** This is the cost of removing the liveness beacon (§5.1, §5.2), and it replaces a trade-off this section used to record in the opposite direction ("losing the beacon invalidates every lock on the instance"). Where the beacon returned a dead holder's locks to the fleet in milliseconds — bounded by how fast Postgres noticed the connection was gone — reclamation is now bounded only by the lease TTL the holder chose, in **every** profile.

What is bought for it is uniformity, and it is worth the cost: a consumer behaves identically wherever it runs (Goal 2), a cluster replica can be restarted, upgraded or rescheduled without revoking the fleet's locks, and any replica serves any lease operation (invariant I7). Keeping the beacon for in-process acquisitions and dropping it for brokered ones would have meant two timings for the same code and the same config — a class of bug that reproduces in only one deployment shape.

The failure mode this removes is also worth naming, because it was real: one beacon per instance meant one blast radius, so a single connection blip made *every* lock that instance held stealable at once, and a ping overrunning its statement timeout was read as a loss — which made runtime starvation a way to lose every lock on the instance without the database having done anything wrong. Nothing can now invalidate a lease for a reason local to the holding process.

**Operationally: keep lock TTLs tight.** The TTL is a per-acquisition parameter on the trait, so recovery promptness is under caller control rather than operator control — there is no knob here to tune, deliberately. A consumer whose critical section is short should not ask for a long lease. ADR-012 records the decision, both rejected alternatives, and this cost; `PG-LOCK-023` holds the resulting timing in both directions.

**[Risk: pool statements are not bounded client-side]** The **write pool** has no client-side statement bound: `pool_acquire_timeout` covers checkout, but statement execution afterwards does not time out, and `sqlx` supplies no read timeout. Against a server that freezes *after* a successful checkout, any pool statement — a reaper sweep, a `renew`, a cache operation — can block indefinitely, and where that statement is inside a background task, `stop()` blocks on its join.

This is pre-existing and not specific to the lock half (it applies equally to every cache operation), which is why it is recorded here rather than fixed as part of the session refactor. The practical bound today comes from `pool_acquire_timeout` arithmetic: a frozen server normally fails the *next* checkout at the `before_acquire` hook, which is the path `PG-LOCK-019` exercises. That is an accident of timing, not a guarantee — do not read `PG-LOCK-019` as proof that `stop()` is bounded in general.

Two of its sharper edges are closed. `PgPool::close()` waits for **every** checked-out connection to come back, and the per-lock guard tasks are spawned detached — a guard parked in a `renew`'s pool I/O is neither preemptible by the shutdown token nor joined anywhere, so an unbounded `close()` relocated the stall out of the joins this section tells operators to budget for and into a step with no budget at all. It is now bounded by `POOL_CLOSE_TIMEOUT` (10s), which is safe because `close()` marks the pool closed *before* it starts waiting: giving up leaves the pool closed and any straggler connection closed when its holder returns it, and logs `cluster.lock.pool_close_timeout`. Separately, the second edge this used to record — a supervisor's `timeout(D, handle.stop())` giving up mid-shutdown leaking the beacon task and its off-pool backend for the life of the process, keeping that beacon *granted* so the fleet went on treating abandoned rows as live — is retired with the beacon itself: there is no off-pool connection left to leak. Both handle `Drop`s still cancel the shared token before their diagnostic panic/warn, so a dropped `stop()` future still unwinds the background tasks.

What remains open is the general case: a client-side bound on pool *statements*. Until then, a deployment that needs a hard shutdown ceiling should still enforce it at the supervisor level.

**[Retired: same-instance exclusion enforced in-process]** Two acquisitions from the same instance now race exactly as two instances do — on the row lock of the conflicting tuple (§5.1) — so Postgres is the authority for both, and no in-process registry participates in exclusion at all. `PG-LOCK-008` (20 concurrent local callers) and `PG-LOCK-016` (two instances) are deliberately kept as separate scenarios even though they exercise the same mechanism now: that they *do* is the claim worth holding both halves to.

**[Trade-off: a holder is no longer told when its lock is reclaimed]** Reclamation used to route through the owning instance, so the owner necessarily noticed and logged it. A successor now steals the row directly and the previous holder learns only at its next `renew` — no behavioural difference for the consumer (`LockExpired` either way), but an operator loses a signal that fired without anyone having to ask for it. Deliberately not replaced: reinstating it would mean reintroducing a per-process registry of held names (removed with the beacon, §5.1) plus an indexed `SELECT` over it on every reaper wake — the class of query this design removed, for a signal with no current consumer.

**[Trade-off: `synchronous_commit = on` enforced, no `off` mode]** The plugin enforces `synchronous_commit = on` on every connection (§3.4) and offers no `EventuallyConsistent`/weak-consistency mode. Operators who need `off`'s write-latency benefit and can tolerate its durability trade-off (risk of losing the last few commits on crash) cannot get it from this plugin — that use case belongs on a backend designed for it. Enforcement is via `after_connect` + `before_acquire` hooks (re-asserted on every checkout), which now covers every durability-relevant write including the `cluster_lock` rows. There is no longer any connection outside that at all: with the beacon removed the lock opens no off-pool connection whatsoever (§3.4). The residual window this risk used to record is retired rather than accepted.

**[Risk: async replication is warn-only, not enforced]** ADR-009 requires synchronous streaming replication for Postgres leader/lock safety under failover, but §3.6's `replication_mode` check only warns (`cluster.provider.replication_async`) when it detects or is told the topology is async — it never fails startup. An operator who ignores or doesn't monitor that log line can run indefinitely on an async-replicated, failover-unsafe topology. This is a deliberate choice (topology isn't always confidently detectable, and some deployments legitimately don't need HA), not an oversight — but it means this is an operational monitoring dependency, not a guarantee enforced by the plugin itself; pair the WARN log with an alert, not just a dashboard.

**[Design choice: no read-path cache]** `get` is always read-through to Postgres (§4.3) — the plugin deliberately does not layer an in-process read cache in front of it. An in-process cache here would be local to each service instance, not shared across a fleet: at N instances it multiplies rather than amortizes correctness risk (each instance's cache would independently race NOTIFY-driven invalidation against concurrent reads, so different instances could transiently observe different values for the same key), while doing nothing to relieve the actual write-side bottleneck above (NOTIFY volume is driven by writers, not readers). It would also risk silently reaching the leader-election primitive that rides on this same cache backend (§6) specifically *because* it declares `Linearizable` consistency — caching those reads would undermine the reason this backend was chosen for them. The intended pattern is per-primitive backend selection: route a given primitive to the backend suited to its access pattern (e.g. Redis for a hot, staleness-tolerant application cache; this plugin for Postgres-backed locks/coordination), rather than asking one backend to be good at everything.

## 12. Open Questions

| Question | Owner | Target Resolution | Recommendation |
|---|---|---|---|
| Credstore-backed credential resolution for the connection string | Postgres plugin owner + Platform OOP deployment design | Future iteration, once the OOP/credstore wiring contract (`docs/arch/toolkit-oop/DESIGN.md` §Platform Host Composition; parent cluster `DESIGN.md:41`) is committed | Decided for now: `connection_string` uses `${VAR}` / `${VAR:-default}` env-var expansion (`toolkit_utils::var_expand` via `#[derive(toolkit_macros::ExpandVars)]` + `#[expand_vars]`, §7), the same mechanism `libs/toolkit-db` uses for DB passwords/DSNs — no `secret_ref` field is exposed by this plugin's config in the meantime. When the credstore path is eventually added, reuse the wiring crate's existing `BackendBinding.secret_ref: Option<SecretRef>` (`cluster/src/config.rs:83`) rather than reintroducing a plugin-local field of the same name at a different layer — that duplication is exactly what was removed here |

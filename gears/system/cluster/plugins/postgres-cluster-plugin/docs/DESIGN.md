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
  - [5.1 The Lease Row and the Liveness Beacon](#51-the-lease-row-and-the-liveness-beacon)
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

`cf-postgres-cluster-plugin` is the Postgres backend plugin for the cluster gear. It provides a native `ClusterCacheBackend` over a `sqlx::PgPool` and a native `DistributedLockBackend` over a `cluster_lock` lease row, with a single per-instance advisory lock retained purely as a liveness beacon (§5.1). Leader election and service discovery are derived from the SDK default backends over the Postgres cache — no additional tables or connections are required for those two primitives.

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
| `DistributedLockBackend` | Native — the `cluster_lock` lease row as sole arbiter, plus one per-instance advisory lock as a liveness beacon (§5.1). Independently routable via `lock: { provider: postgres }` (§3.5), with its own pool/config — not required to be paired with the postgres cache provider | `linearizable: true` | — |
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
    name             TEXT        NOT NULL,
    holder_id        UUID        NOT NULL,
    acquired_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at       TIMESTAMPTZ NOT NULL,
    holder_beacon_hi INT4        NOT NULL,
    holder_beacon_lo INT4        NOT NULL,
    PRIMARY KEY (name),
    CONSTRAINT cluster_lock_name_len_check CHECK (octet_length(name) <= 2048),
    CONSTRAINT cluster_lock_beacon_nonneg_check
        CHECK (holder_beacon_hi >= 0 AND holder_beacon_lo >= 0)
);

CREATE INDEX cluster_lock_expires_idx ON cluster_lock (expires_at);
```

This row **is** the lock (§5.1) — not metadata beside one. `expires_at` is the lock's absolute deadline, computed as `now() + ttl` on the **database** clock at insert and at every renew (PGR-C2, exactly as for `cluster_cache.expires_at`) — not a raw `acquired_at`/`ttl_ms` pair the TTL reaper re-derives for every row on every tick. A derived deadline could not be indexed at all: `timestamptz + interval` is `STABLE`, not `IMMUTABLE` (its result depends on the session `TimeZone`), so Postgres rejects an expression index on `acquired_at + ttl_ms * interval '1ms'`, and `now()` may not appear in a partial-index predicate either — leaving every sweep a guaranteed sequential scan with a per-row interval multiply. Storing the deadline makes the sweep an indexed `WHERE expires_at <= now()` and lets the reaper read `min(expires_at)` index-only, to wake when the next lock is actually due rather than polling blindly (§5.2). The index is unconditional rather than partial like the cache's: a lock TTL is mandatory (`DistributedLockBackend` takes a `Duration`, not a `Ttl`), so there is no `NULL` subset to exclude. `acquired_at` is restamped on every renew and is otherwise diagnostic; the only query that filters on it is the orphan sweep's fence (§5.2), which needs it to tell a row abandoned an interval ago from one written since. (§6's pre-designed `pg_locks` cost fallback would give it a second, load-bearing role; it is not built.) `holder_id` is a random UUID generated at acquire time; `renew()`, `release()`, and the reaper all guard on it to prevent a foreign or stale holder from renewing, releasing, or reclaiming another's lock. It is a native `UUID` column (and a `uuid::Uuid`, not a `String`, in Rust): the only writer is `try_acquire`'s `Uuid::new_v4()`, so the native type costs 16 bytes instead of 36, compares as bytes rather than as a collated string in every fenced query, and makes the invariant the column's own rather than a convention.

`holder_beacon_hi`/`holder_beacon_lo` are the two `int4` halves of the liveness beacon vouching for the row: the single per-incarnation advisory lock the holding instance took at startup on a dedicated connection (§5.1). They are what makes ownership *checkable by anyone* rather than only by the row's writer — the acquire predicate joins them against `pg_locks`, so a row whose beacon is no longer granted is stealable the instant Postgres notices the holder's connection is gone, without waiting out `expires_at`. Nothing renews or maintains them; the beacon is released only by its connection closing, which is precisely the event they exist to detect.

Two `int4` halves rather than one `bigint`, `NOT NULL`, and non-negative, all for the predicate's sake: `pg_locks` exposes the two-argument advisory key as `classid`/`objid`, which are `oid` (unsigned), so keeping both halves non-negative makes the comparison a plain cast with no sign reinterpretation, and `NOT NULL` keeps the predicate free of a `NULL` branch. The columns were declared in `0002_cluster_lock.sql` itself rather than added by a follow-on migration exactly so that no schema version ever exists in which a row can lack a beacon.

There is deliberately **no index** on `(holder_beacon_hi, holder_beacon_lo)`, though three statements filter on that pair (the orphan sweep, the shutdown drain, and the post-reconnect cleanup — §5.2, §10). The table holds only *active* locks, so at the cardinality `cluster_postgres_lock_active_names` reports a sequential scan is trivial while the index would be pure write amplification on the acquire path. Revisit against that gauge, not up front.

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
      beacon.rs     — Beacon: the one dedicated liveness-beacon connection
      mod.rs        — PostgresLock (DistributedLockBackend impl); PostgresLockPlugin, builder,
                       handle (standalone lock-only construction, §3.5)
      reaper.rs     — cluster_lock TTL sweep + beacon-scoped orphan sweep
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
- **A long-lived, owned connection for the liveness beacon** (§3.3, §5.1): the beacon's whole meaning is that one specific socket's death releases it, so the plugin must own that connection outright for its lifetime (`lock/beacon.rs`) rather than borrowing one per statement. `DatabaseConnection`'s only own-a-connection primitive is a transaction, and abusing a long-lived transaction for this collides with the PgBouncer-transaction-mode incompatibility this plugin already rejects at startup (§5.4). The acquire predicate also joins `pg_locks` inside its own `WHERE`, which has no ORM equivalent either.
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
3. Establishes the liveness beacon outside the pool (`.await`ed, §3.3) and opens the
   dedicated LISTEN connections (`.await`ed).
4. Spawns the cache TTL reaper, the lock TTL reaper, the beacon task, and
   the LISTEN fan-out tasks.
5. Returns the handle. By the time `build_and_start` resolves, the schema
   exists and both the beacon and the LISTEN connection are live — there is
   no readiness gate or background-init race for callers to reason about, unlike
   a design built around a synchronous builder. A failure at any of these steps
   tears down whatever the earlier ones already started (including the beacon,
   which does not observe the shared shutdown token and so is ended by its own
   `BeaconHandle::shutdown`) rather than detaching it.

`stop`:
1. Cancels all `CancellationToken`s; awaits background tasks.
2. Sends `CacheWatchEvent::Closed(ClusterError::Shutdown)` to all active watchers.
3. Drops each dedicated `PgListener` — awaiting the cancelled LISTEN tasks in
   step 1 is what drops them, so the LISTEN connections are already closed by the
   time step 4 runs (§10 step 3).
4. Hands back held locks, then closes the beacon, then the pool — in that order
   (§10 step 4). The beacon closes *after* the drain, never before: the drain
   reads the beacon key and needs the pool, so both must still be live when it
   runs.
5. Sets `self.stopped = true` as the last step — graceful shutdown completed, so the `Drop` guard above must not fire.

### 3.3 Connection Pool Split

| Connection type | Purpose | Pool |
|---|---|---|
| Write pool (`PgPool`, default 5 connections) | All cache reads/writes, **every** `cluster_lock` statement (acquire, renew, release, both sweeps, the drain), all `pg_notify`, migrations | `sqlx::PgPool` |
| Cache-watch LISTEN connection (1 dedicated, combined plugin only) | Receives all `NOTIFY cluster_cache_changes` events; never used for queries | A dedicated `sqlx::PgListener`, outside the pool |
| Lock release-wake LISTEN connection (1 dedicated) | Receives all `NOTIFY cluster_lock_released` events, feeding the in-process `ReleaseWaiters` registry that wakes blocked `lock()` callers (§5.3) | A dedicated `sqlx::PgListener`, outside the pool |
| **Liveness beacon (1 dedicated)** | Holds this instance's single per-incarnation advisory lock, and pings itself once a second. Carries no lock traffic whatsoever: no lock name, no `holder_id`, no row, no write of any kind | A dedicated `sqlx::PgConnection` (`lock/beacon.rs`), outside the pool |

The beacon is **not** a lock (§5.1). It is a crash-triggered tombstone: one statement establishes it, no statement ever maintains it, and the server deletes it the instant that connection dies. Every other instance can read it in SQL (`pg_locks`), which is what lets the acquire predicate decide "is this row's holder still alive?" atomically with the acquire itself rather than in a second round-trip that would have to be raced.

A held lock therefore consumes **no connection at all** — not a pooled one, and not a share of anything per-lock. The number of simultaneously held locks is bounded by `cluster_lock` cardinality (§8's `lock_name_cardinality_warn_threshold`), not by `pool_max_size` and not by Postgres's shared lock-manager table. This is why there is no "size the pool for your concurrent locks" advisory.

Three properties of the beacon are load-bearing and enforced elsewhere in this document:

- **It is never released explicitly** (§5.1) — not on release, not on expiry, not at shutdown. Releasing it while the process runs asserts that this instance is dead, and every lock it holds becomes stealable on sight. Only closing the connection ends it, and at shutdown that happens anyway.
- **Never the blocking `pg_advisory_lock`** (§5.3), on the beacon or anywhere else. A blocking form would park a task inside Postgres waiting for a key nobody can hand over.
- **Its ping and its reconnect are bounded client-side** (`beacon::STATEMENT_TIMEOUT`, 5s; `CONNECT_TIMEOUT`, 10s), and both are raced against the shutdown token. `sqlx` applies no read or connect timeout of its own, and a server-side `statement_timeout` is useless for the case that matters, since the peer that would enforce it is the one that stopped answering. Overrunning is read as a lost beacon and handled exactly like one: the connection is discarded rather than reused, because a timed-out statement leaves the wire protocol in an indeterminate state. `PG-LOCK-019` is the regression test.

**Total connection count.** Neither the LISTEN connections nor the beacon live in the `PgPool` (`sqlx::PgListener` owns its own connection and cannot adopt a `PoolConnection`; the beacon must outlive any checkout, and its whole meaning is tied to one socket's lifetime), so an instance's real steady-state connection count is `pool_max_size + 3` for the combined `PostgresClusterPlugin` (cache-watch + lock release-wake + beacon) and `pool_max_size + 2` for the standalone `PostgresLockPlugin` (release-wake + beacon — no cache half, so no cache-watch connection). That total does not move with how many locks are held.

### 3.4 synchronous_commit Enforcement

Per ADR-009 (`docs/ADR/009-leader-election-backend-safety.md`), this plugin **enforces** `synchronous_commit = on` on every connection it uses — it does not support running with `synchronous_commit = off`, and does not offer an `EventuallyConsistent` mode. `consistency()` unconditionally returns `CacheConsistency::Linearizable` (§4.5); there is no code path that downgrades it. `synchronous_commit = on` is Postgres's own default, so this is "enforce the safe default," not an unusual imposition — the case being closed off is an operator (or a co-tenant on a shared database/role) explicitly setting it to `off` for write-latency, which this plugin's lock and leader-election guarantees cannot tolerate.

Enforcement happens at two points in the connection lifecycle, using `sqlx::PgPoolOptions` hooks:

1. **`after_connect`** — runs `SET synchronous_commit = on` once when a new physical connection is established. Covers the common case (role/database default is `off`, or a session-level `ALTER ROLE ... SET synchronous_commit = off` applies at login).
2. **`before_acquire`** — re-runs `SET synchronous_commit = on` every time a connection is checked out of the pool for use, whether for a cache operation or a lock acquire. This closes the window ADR-009 flags: `synchronous_commit` is `USERSET` scope, so it can be mutated mid-session by anything sharing the connection (a misbehaving statement, a pooler-level session variable reset, `ALTER ROLE` applied after the connection was opened). Re-asserting on every checkout means a mutation can only affect the *current* checkout, never a later one.

**No residual gap.** The pool hooks cover every connection the pool owns, and that is now *every statement this plugin issues against its own tables* — the `cluster_lock` INSERT/UPDATE/DELETEs ride the pool exactly like cache writes do, so they get `before_acquire` re-assertion on every checkout. The only long-lived connection outside the pool that the lock opens is the liveness beacon, which **writes nothing at all**: no row, no `pg_notify`, one `pg_try_advisory_lock` at establishment and a ping thereafter. It therefore has no durability setting to maintain, and needs neither the assertion nor the interval re-assertion the previous lock session carried. The residual risk DESIGN §11 used to record for that session is retired rather than accepted.

`PG-LOCK-009` asserts the override against a database whose own default is `off`; `PG-SPEC-005` asserts the correction on the checkout *after* an external mid-session flip, with `pool_max_size: 1` so the connection handed back is provably the same one.

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
| Liveness beacon (§3.3) | 1 | 1 — the lock primitive is the whole plugin here, so it is not optional |
| Background tasks | Cache TTL reaper, lock TTL reaper, beacon task, cache-watch LISTEN task, lock release-wake LISTEN task | Lock TTL reaper, beacon task, lock release-wake LISTEN task |
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

`features().prefix_watch` is `false`, so the capability resolver rejects `CacheCapability::PrefixWatch` at startup for this backend. The SDK-default service-discovery backend auto-selects `PollingPrefixWatch` when `prefix_watch == false` (see §6).

**Empty / unrecognized payload — Reset.** The listen_task interprets `payload.is_empty()` (or any payload not matching `<event>:<key>`) as a `Reset` signal, broadcasts `CacheWatchEvent::Reset` to every active watcher, and clears all watcher subscriptions (consumers must resubscribe). This matches ADR-003's overflow mapping for Postgres. It is the fallback for a bare `NOTIFY` from an external writer or a future format — **not** the NOTIFY-queue-overflow path: overflow aborts the committing *producer* transaction with an error and delivers no notification. Overflow does not disconnect the listener or emit a `Reset`; it surfaces to the *writer* as that write's `Provider` error and in the PostgreSQL server logs, not through this LISTEN-side recovery.

**Connection loss — Reset.** If the dedicated LISTEN connection drops, the listen_task attempts reconnect with exponential backoff. On successful reconnect, it broadcasts `CacheWatchEvent::Reset` before resuming event delivery, signalling that consumers may have missed events during the gap. If reconnect fails beyond the configured retry limit, it broadcasts `CacheWatchEvent::Closed(ClusterError::Provider { kind: ConnectionLost, .. })` and exits.

The Postgres cache is read-through: every `get` hits the database directly. There is no in-process read cache — consumers with hot-key, high-read, staleness-tolerant workloads should route that primitive to a backend built for it (e.g. Redis) rather than expect this plugin to double as a fast local cache; see §11 for the rationale.

### 4.4 scan_prefix

`scan_prefix(prefix)` is implemented via `LIKE prefix%`. The plugin escapes `%`, `_`, and `\` in the caller's prefix before appending `%`, so wildcard characters in `prefix` are matched literally rather than being interpreted by `LIKE`. This is used by `PollingPrefixWatch` to enumerate keys for diffing. Performance degrades with keyspace size; the partial index on `expires_at` does not help here. High-volume prefix scans should use a backend with native prefix watch (Redis, NATS, etcd).

### 4.5 Consistency Declaration

`consistency()` returns `CacheConsistency::Linearizable`. All cache operations run at Postgres's default `READ COMMITTED` isolation level, which provides linearizability for single-row operations (the only kind the cache uses). The CAS path uses an `UPDATE … WHERE version = $expected`, which is an atomic compare-and-set at the row level regardless of isolation level. Under `READ COMMITTED`, concurrent updates do not produce write skew on single rows.

## 5. Distributed Lock Implementation

### 5.1 The Lease Row and the Liveness Beacon

A lock is held **iff** a `cluster_lock` row exists whose `expires_at` is in the future *and* whose recorded beacon is still granted in `pg_locks`. The row is the sole arbiter of ownership. Every acquire, renew, and release is a single statement against the write pool, with no session affinity and no in-process state that is load-bearing for exclusion.

#### How mutual exclusion works

Three mechanisms cooperate inside the acquire statement, and the primary key does the least work of the three:

1. **`PRIMARY KEY (name)` detects the conflict.** It guarantees at most one row per lock name, giving `ON CONFLICT` something to fire on. It decides nothing.
2. **The row lock serializes.** On conflict Postgres takes an exclusive lock on the conflicting tuple, so a competing transaction holding it makes us *block* until it commits or aborts. This is the serialization point.
3. **The `WHERE` decides.** After taking the row lock, Postgres re-reads the **latest committed version** of the row and evaluates the predicate against it — not against the snapshot the statement started with.

Step 3 is what makes it correct: two acquirers cannot both observe the lock as free, because the loser re-evaluates against the winner's already-committed state. `RETURNING` is the answer — a row means acquired (whether by insert or by steal), zero rows means contended, with no third case. Two tasks in the *same* process race exactly as two instances do, which is why no in-process claim registry is needed to arbitrate them.

**This requires `READ COMMITTED`, and the plugin asserts it at startup** (`pg_setup::assert_read_committed`, §3.2). Step 3's re-read is `READ COMMITTED` behaviour; under `REPEATABLE READ` or `SERIALIZABLE` the transaction snapshot cannot advance, so instead of re-evaluating, Postgres raises SQLSTATE `40001` and the caller would have to retry. The check lives in shared startup validation rather than in the lock module because the cache's `put_if_absent` — and so leader-election failover via `CasBasedLeaderElectionBackend::claim` — already depended on exactly the same idiom, unguarded. Asserting rather than *enforcing* (one `SET SESSION CHARACTERISTICS` in `after_connect` would do it) is deliberate: silently overriding an isolation level an operator set on purpose hides a mismatch that failing fast surfaces. `PG-SPEC-011` covers both directions.

Three variants look equivalent and are not: a `SELECT` to check followed by an `INSERT` is a check-then-act race; letting the primary key's unique violation *be* the contention signal cannot express "steal if expired", making a lapsed lock permanently unacquirable; and `SELECT … FOR UPDATE` then `UPDATE` needs an explicit transaction and locks nothing when no row exists yet, so two first-time acquirers both proceed and one takes a unique violation.

#### The liveness beacon

At startup, on one dedicated connection outside the pool (§3.3), the instance picks a random per-incarnation key and takes it:

```sql
SELECT pg_try_advisory_lock($1, $2);   -- (beacon_hi, beacon_lo), both non-negative int4
```

`pg_try_advisory_lock`, never the blocking form. Nobody else holds a freshly random 62-bit key, so `false` means a collision: draw another and retry.

The beacon is not a lock. It is a **crash-triggered tombstone**, and an advisory lock is the only Postgres primitive that is all four of: established by one statement (no schema, no row, no WAL, no write ever again); readable from any other session in SQL, and therefore joinable inside the acquire statement's own predicate; **deleted by the server the instant the session ends**, with nothing having to maintain it; and unfiltered by privilege, unlike `pg_stat_activity`, which nulls most columns for other roles' sessions. The third property is the whole point — it is what returns a crashed holder's locks to the fleet without waiting out their TTL, and the one property that cannot be rebuilt in application code without reintroducing a heartbeat, a TTL on that heartbeat, and a reaper for it.

The key being ours to choose is what makes per-incarnation keying free, and that is load-bearing: every row carrying this key was provably written by this process, on this connection. The orphan sweep, the shutdown drain, and the post-reconnect cleanup all rest on it and need no other fence.

**The beacon is never released explicitly — an invariant, not an omission.** Not when a lock is released, not when one expires, not at shutdown. Releasing it while the process is still running asserts that this instance is dead, and every lock it holds becomes stealable immediately. Closing the connection is the only correct way for it to end.

**Acquire fails outright when there is no live beacon**, with `ClusterError::Provider { ConnectionLost }`, and never a row write. Stamping a row with a dead incarnation's key — or with none — would hand the caller a guard for a lock every other instance can steal on sight, and the consumer would not find out until its next `renew`. `lock()` classifies that error as transient and retries it inside the caller's budget, so a blip still resolves into a successful acquisition rather than a spurious failure.

#### SQL contract per operation

**Acquire** — one statement, any pool connection:

```sql
INSERT INTO cluster_lock (name, holder_id, acquired_at, expires_at,
                          holder_beacon_hi, holder_beacon_lo)
VALUES ($1, $2, now(), now() + ($3::bigint * interval '1 millisecond'), $4, $5)
ON CONFLICT (name) DO UPDATE
   SET holder_id        = EXCLUDED.holder_id,
       acquired_at      = EXCLUDED.acquired_at,
       expires_at       = EXCLUDED.expires_at,
       holder_beacon_hi = EXCLUDED.holder_beacon_hi,
       holder_beacon_lo = EXCLUDED.holder_beacon_lo
 WHERE CASE WHEN cluster_lock.expires_at <= now() THEN true
            ELSE NOT EXISTS (
                   SELECT 1 FROM pg_locks
                    WHERE locktype = 'advisory' AND objsubid = 2 AND granted
                      AND classid = cluster_lock.holder_beacon_hi::oid
                      AND objid   = cluster_lock.holder_beacon_lo::oid)
       END
RETURNING 1;
```

**`CASE`, not `OR`.** SQL does not guarantee left-to-right evaluation of `OR` operands, and this needs the cheap indexed comparison to short-circuit the `pg_locks` scan off the uncontended path — `pg_locks` is a function scan over `pg_lock_status()` with no index. `PG-SPEC-012` holds that to `EXPLAIN ANALYZE` rather than taking it on trust, and notes one subtlety worth knowing when reading such a plan: Postgres emits the correlated `NOT EXISTS` as a *pair* of alternatives (`SubPlan 1 or hashed SubPlan 2`) and runs whichever it picks, so "never executed" appears against the unchosen one even on the contended path.

**Renew** — authoritative against a single truth, no probe:

```sql
UPDATE cluster_lock
   SET acquired_at = now(),
       expires_at  = now() + ($1::bigint * interval '1 millisecond')
 WHERE name = $2 AND holder_id = $3
   AND expires_at > now()
   AND holder_beacon_hi = $4 AND holder_beacon_lo = $5
RETURNING 1;
```

Zero rows is `ClusterError::LockExpired`, whichever fence failed. The `holder_id` fence (PGR-L1) guards against a **successor**; `expires_at > now()` refuses to resurrect a lease the fleet is already entitled to treat as free; and the beacon fence guards against **ourselves** — if this instance's beacon has been replaced since the acquisition, the row carries a dead key that anyone can steal, and reporting a healthy renewal for it is the one way this design could silently lose mutual exclusion.

**Release** — one statement: a `DELETE … WHERE name = $1 AND holder_id = $2` and the `pg_notify` in a single data-modifying CTE, so releasing costs one pool checkout and the wake is atomic with the row's disappearance. The `holder_id` fence is sufficient on its own: a lock stolen after a beacon loss carries the successor's `holder_id` and will not match.

**`pg_advisory_unlock` is not called anywhere in this plugin** — the single sharpest way to state the design, and a useful invariant to check any change against.

Be exact about what that does and does not mean, since "lock" names two different things here. Releasing a lock still means **deleting its row**, and five paths do that: `release`, the TTL sweep, the orphan sweep (§5.2), the shutdown drain (§10), and the beacon's post-reconnect hand-over (§5.2). What is gone is the *advisory-lock* release — nothing per-lock is `pg_advisory_lock`ed, so there is no session-scoped unlock to pair with it. The one advisory lock this plugin takes is the per-instance beacon, released only by its connection closing.

The consequence is the point: a row delete is something **any** instance can perform, whereas an advisory unlock could only ever be issued by the session that took it. An expired or unvouched row is therefore stealable by the acquire predicate itself, evaluated by whoever asks — no reclaim step, and no reason reclamation has to route back to the instance that held the lock. That is what lets a crashed *or merely wedged* holder's lock be taken by anyone rather than only by a healthy reaper on the owning instance (`PG-LOCK-014`).

#### The one surviving in-process registry

`local_holders` (`DashMap<String, Uuid>`, name to `holder_id`) records the locks this process currently has a live guard for. It is **not** authoritative for exclusion and is named so no future reader mistakes it for it: `renew` and `release` fence in SQL, the reaper does not need to know which locks are ours, and `cluster_postgres_lock_active_names` is table-derived rather than `len()` of this map.

It survives for exactly one consumer — §5.2's orphan sweep, which must distinguish a row with a live local guard from a row whose acquirer went away. That is the one question the database cannot answer about itself, and the only reason any local registry remains.

### 5.2 TTL Enforcement, Beacon Loss, and Garbage Collection

`expires_at` is the lease deadline, computed in SQL against the **database** clock at insert and at every renew (§2.1). Reclamation happens on two independent paths, and only the first is load-bearing for exclusion:

1. **Any acquirer's own predicate.** An expired row, or one whose beacon has vanished, is taken in the acquiring statement itself (§5.1). No sweep has to have run, and no instance has to cooperate. This is the whole guarantee.
2. **The background reaper**, which is garbage collection plus a promptness optimisation: it deletes expired rows so the table does not grow, and NOTIFYs their names so blocked waiters wake instead of sitting out a heartbeat. A sweep that never runs costs table growth and slower wake-ups, never a double-hold.

    Each sweep deletes in bounded batches (`DELETE ... WHERE name IN (SELECT name ... ORDER BY expires_at LIMIT n FOR UPDATE SKIP LOCKED)`), looping until a batch comes back short. `SKIP LOCKED` keeps the per-instance reapers from serializing behind each other, and `cancel` is re-checked between batches so shutdown is not held up mid-backlog.

    **Wake schedule.** After each sweep the reaper sleeps until the earlier of the next metrics tick (`lock_reaper_interval_ms`) and the next row's deadline, read as `SELECT extract(epoch FROM (min(expires_at) - now())), now()` — an index-only read, with the subtraction done in Postgres so the delay never depends on this instance's wall clock, and the `now()` doubling as the orphan sweep's fence below. The interval is the *cap*: it keeps `cluster_postgres_lock_active_names` and the cardinality WARN on their configured cadence, and only these interval-boundary wakes do the gauge work. `min(expires_at)` only *shortens* an individual sleep.

A sleep is computed from the table as it looked at wake time, so on its own that shortening would miss a lock whose entire lifetime fits inside one sleep (TTL ≲ `lock_reaper_interval_ms`). `try_acquire` and `renew` therefore signal the reaper (an in-process `tokio::sync::Notify`) once their write is committed — but **only when the TTL they wrote is shorter than `lock_reaper_interval_ms`**, which is exactly that condition. The signal is in-process only, and that is sufficient rather than partial: the sweep is promptness only, so a hint no other instance hears costs at most a waiter's heartbeat. Both the expiry-driven and the signalled wake are floored at 100 ms (or at `lock_reaper_interval_ms` when that is shorter), so many staggered deadlines — or a burst of acquisitions — coalesce into one wake instead of one each. A **lost** signal costs at most one late sweep; a **spurious** one is not symmetric, which is why the gating is not merely an optimisation. `Notify` holds a single permit, so signalling on every write keeps the `notified()` branch permanently ready and collapses every subsequent sleep to the floor — an instance renewing a couple of hundred leases a second would run a full iteration every 100 ms instead of every interval, roughly fifty times the intended database load, permanently, on every instance in the fleet. Expiry is deliberately **not** bucketed into coarse slices: for a lock the TTL is the crash safety net, so rounding deadlines up to a shared boundary would let a stale lock block waiters for up to a full bucket past its TTL.

#### Beacon loss means losing every lock

One beacon per instance means one blast radius, deliberately: a connection blip has one predictable outcome rather than a per-lock recovery path to reason about. When the beacon connection drops:

- Other instances see the beacon absent from `pg_locks` and may reclaim this instance's rows immediately. Correct — the instance can no longer prove it is alive.
- The instance purges `local_holders`, so nothing local advertises a lock it cannot defend.
- After reconnect the key differs, so `renew`'s beacon fence can never match a pre-disconnect row. Those locks are gone permanently rather than resurfacing.
- Consumers learn at their next `renew`, which returns `LockExpired`. That is the only channel `LockGuard` offers — the SDK has no asynchronous lost-lock signal, by design.
- Once reconnected, the instance runs §10's drain statement **against its previous key**, deleting those rows and batch-NOTIFYing their names. Identical SQL, different parameter, and safely fenced for free: the dead key only ever matched rows this instance wrote, and any already stolen now carry the successor's beacon. A courtesy rather than a requirement — the rows are unvouched and stealable either way — but it hands names over on a NOTIFY instead of making waiters discover them by retry.

**Detecting the loss requires a ping, and this is the one place the design does not get a fence for free.** An idle `PgConnection` is not polled, so a beacon whose backend died goes unnoticed *locally* until something uses that connection — and nothing ever does. Without local detection the semantics would quietly weaken to "you remain the holder until someone with a live beacon takes it from you": our own `renew` would keep matching its row and succeeding, because the row still carries our `holder_id` and our now-dead key. No mutual exclusion is violated — the moment another instance steals, the `holder_id` fence makes the next `renew` fail — but the instance would go on believing it holds a lock it can no longer defend, for as long as nobody else wants it. So the beacon task pings its own connection once a second and treats a failed ping as loss: purge, reconnect, new key. A fixed cadence rather than `lock_reaper_interval_ms` (default 5s, and an operator may set it far higher), because this bounds how long an instance can be wrong about what it holds. `PG-LOCK-013` asserts all three halves, including the negative — that a lock is *not* silently retained across the loss when nobody else contends for it.

The remaining race is safe in the direction that matters: a beacon can be read as *alive* moments before it dies (conservative — we decline to steal, and the TTL still bounds it), but a genuinely granted beacon is always visible in `pg_locks`, so a live holder is never robbed.

#### The real bound on "immediate"

Sub-TTL recovery is bounded by how quickly Postgres notices the client is gone, not by anything in this design. A clean process exit or socket close releases the beacon at once. A hard kill or a partition that leaves the TCP connection half-open leaves the backend blocked in `recv`, still holding the beacon, until keepalives fire. The honest statement of the guarantee is therefore **"immediate on clean disconnect, keepalive-bounded otherwise, TTL-bounded in the worst case."**

Tightening that bound is a **server-side** lever, not a client-side one. The beacon sets `tcp_keepalives_idle = 5`, `tcp_keepalives_interval = 2`, `tcp_keepalives_count = 3` on its own session at establishment (the GUCs are `USERSET`): ≈11s worst case, typically 5–7s, for one packet every 5s on one connection per instance. Client-side `keepalives_*` would be the wrong instrument — those detect a dead *server*, which the ping already does faster; what matters here is Postgres noticing that **we** are gone. Best-effort and never fatal: the GUCs are unsupported on some platforms, and a failed `SET` costs recovery promptness with the TTL still bounding it, so it logs at DEBUG and continues. That is deliberately the opposite of how `synchronous_commit` is treated (§3.4). Fixed constants rather than config knobs, because recovery promptness is *already* under caller control through the lock TTL, which is a per-acquisition parameter on the trait — an operator wanting faster reclamation should shorten the TTL rather than tune socket timers. `tcp_user_timeout` is rejected on platform support: it is a tighter bound, but setting a non-zero value where it is unsupported is an error rather than a no-op.

#### Reclaiming orphaned rows

Acquire is a single statement, so there is no compensating unlock to issue if the caller goes away. A `try_acquire` future dropped after its INSERT committed — `lock()`'s per-attempt timeout elapsing mid-acquire, a cancelled consumer task, a runtime shutting down — leaves a row this instance owns with **no local guard**. This is routine rather than exotic, and it is the one case the previous design handled better.

Its severity is worth stating plainly: because the row is unexpired *and* vouched for by a live beacon, nothing in the fleet will steal it — including this instance, whose own next acquire of that name reads its own orphan as a live holder. The name is wedged for both sides until the TTL.

**Detection is exact.** The beacon key is fresh per incarnation, so every row bearing it was written by this process, in this incarnation. Any such row whose `holder_id` is not in `local_holders` is an orphan — no heuristics, no `pg_locks` read, no key-by-key reconciliation:

```sql
DELETE FROM cluster_lock
 WHERE holder_beacon_hi = $1 AND holder_beacon_lo = $2
   AND holder_id <> ALL($3::uuid[])
   AND acquired_at < $4
RETURNING name;
```

`$3` is `local_holders`' value set — `O(locks held by this instance)`. An empty array is correct and needs no special case (`<> ALL('{}')` is true). The reclaimed names are then NOTIFYed in one batch, so waiters wake instead of sitting out the TTL.

**`$4` is the fence that keeps a live acquisition safe.** Without it the sweep races every acquisition in flight: the known-`holder_id` set is snapshotted in Rust *before* the DELETE executes, so a row committing in between would be read as an orphan and deleted out from under its own guard. Rather than reintroduce a per-acquisition guard, the fence is a database `now()` captured at the **previous interval-boundary reaper wake**, so a row is only ever deleted if it was already unregistered one full interval earlier. The fence deliberately does *not* advance on expiry-driven wakes: a younger fence makes more rows eligible, which is the wrong direction, while a staler one only delays cleanup. The first wake after startup needs no special case — every row bearing a fresh beacon was written within the current interval, so the fence exempts them all.

**Residual.** An acquisition that straddles two interval wakes — a committed INSERT more than one `lock_reaper_interval_ms` before its `local_holders` registration *on the same task* — could still be swept. That requires pathological runtime starvation between one statement returning and one `DashMap` insert, and the consequence is a spurious `LockExpired` for that acquisition, not a double-hold: the row is gone, so exclusion is never violated. If that residual ever proves real, the exact alternative is a set of in-flight `holder_id`s taken before the INSERT and dropped after registration — recommended against unless it does, since it puts a cost on every acquisition to guard against garbage collection rather than against a correctness failure. `PG-LOCK-015`/`017`/`018` cover the sweep's reclamation, its selectivity, and the fence respectively.

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
- `Provider { ConnectionLost }` — the budget ran out while this instance still had no live beacon (§5.1) or could not reach the pool. A transient gap is retried inside the caller's budget (the beacon reconnects with a 200ms..5s capped backoff), which is what carries a `lock()` through a Postgres failover; but if it never clears, the caller is told *that* rather than being handed the `LockTimeout` ordinary contention produces. Retrying is deliberately not given a shorter give-up budget: failover commonly takes 10–30s, and cutting the retry short to improve an error code would trade a real availability property for a cosmetic one.
- `LockTimeout` — the budget ran out while Postgres was answering normally and saying the lock was held. This is genuine contention, and only this case reports it.

The wait does **not** LISTEN on the acquiring connection. `sqlx`'s `PgListener` owns its own single connection and has no public way to adopt an already-checked-out `PoolConnection`, so instead a single **dedicated** `cluster_lock_released` LISTEN connection (opened at `build_and_start`, present in both the combined and standalone plugins — §3.3) runs a fan-out task that `notify()`s the in-process `ReleaseWaiters` registry; each blocked `lock()` caller registers a waiter there and is woken when a `NOTIFY cluster_lock_released` for its name arrives. The 250 ms heartbeat sleep is a safety net against a missed notification (registration racing an already-fired `NOTIFY`, or the listen task momentarily reconnecting): a lost wake only costs latency up to the heartbeat interval, never correctness — the loop always re-attempts the acquire statement itself as the source of truth. A waiter that gives up (timeout or heartbeat-driven re-acquire) deregisters itself from the registry on drop, so no stale waiter accumulates.

This avoids busy-polling: waiters wake promptly when a holder explicitly releases. The TTL sweep, the orphan sweep, the shutdown drain, and the beacon's post-reconnect cleanup all NOTIFY the names they reclaim as well, each in one batched statement.

### 5.4 PgBouncer Constraint

**Narrower than it once was, but not gone.** Every lock *operation* is now a single statement on the pool (§5.1), which transaction-mode pooling would serve perfectly well; the constraint no longer touches acquire, renew, release, or either sweep. What still needs session affinity is the pair of things this plugin opens outside the pool:

- **The liveness beacon.** Its advisory lock lives on a *server* session, and transaction pooling does not pin a client to one across transactions. Returning the connection releases nothing — it strands the beacon on a pooled server session, or hands it to whichever client is next given that session. The consequence is worse than under the old design: a beacon released while the process still runs asserts to the entire fleet that this instance is dead, so every lock it holds becomes stealable on sight.
- **The two `LISTEN` connections**, whose subscriptions have exactly the same session affinity.

Both are opened directly rather than through the pool, so in practice this guards an operator who has put PgBouncer in front of the DSN itself:

- If `pgbouncer_transaction_mode: true` is set in config, `build_and_start` returns `Err(ClusterError::InvalidConfig { … })` naming the beacon and the LISTEN subscriptions.
- Operators using PgBouncer must either use session pooling mode for the cluster plugin's connection string, or use a different lock backend.

### 5.5 Inspecting Locks (operators)

`cluster_lock` is the supported inspection surface. `pg_locks` is not, and was a strictly worse one anyway: it only ever exposed the two halves of a name hash, which is irreversible, so identifying the lock behind a row meant enumerating candidate names and hashing each.

```sql
SELECT name, holder_id, holder_beacon_hi, holder_beacon_lo,
       acquired_at, expires_at, expires_at - now() AS remaining
  FROM cluster_lock
 WHERE expires_at > now()
 ORDER BY name;
```

One limit to be clear about: `holder_beacon_*` identifies the holding **incarnation**, not a human-meaningful instance. A random per-incarnation key is not resolvable to a pod, host, or process on its own. It is greppable, though — the beacon logs its key at INFO when established (`cluster.lock.beacon_established`, §8), so a key read out of the table leads back to that instance's logs. Deliberately cheaper than a `holder_instance` column duplicating identity already present in log context.

A row whose `expires_at` is still in the future is not necessarily *held*: its beacon may be gone, in which case the next acquirer takes it. To check that directly:

```sql
SELECT l.name,
       EXISTS (SELECT 1 FROM pg_locks p
                WHERE p.locktype = 'advisory' AND p.objsubid = 2 AND p.granted
                  AND p.classid = l.holder_beacon_hi::oid
                  AND p.objid   = l.holder_beacon_lo::oid) AS holder_alive
  FROM cluster_lock l
 WHERE l.expires_at > now();
```

## 6. Leader Election and Service Discovery

Both primitives use SDK defaults over the Postgres cache backend.

**Leader election** — `CasBasedLeaderElectionBackend::new(Arc::clone(&cache))`. The cache backend is `Linearizable`, so the consistency guard passes. `LeaderElectionFeatures::linearizable == true`.

**Service discovery** — `CacheBasedServiceDiscoveryBackend::new(Arc::clone(&cache))`. The cache backend declares `prefix_watch: false`. The service-discovery default backend detects this when opening its topology watch (`ensure_maintainer`/`watch`) and falls back to `PollingPrefixWatch`, using `scan_prefix` to enumerate keys under the `svc/` prefix at each polling interval; `discover` additionally reconciles from a fresh `scan_prefix` sweep on each call over a polling cache, so it reflects current backend truth rather than lagging the poll interval. The interval is configurable via the backend's `with_prefix_watch_polling` (default 5s). The operator-set `sd_poll_interval_ms` reaches it through the omit-default wiring path: the wiring reads that key from the cache binding's options (single-sourced as `cluster_sdk::provider::SD_POLL_INTERVAL_MS_OPTION`) and threads it into `with_prefix_watch_polling`, so a profile tunes its own staleness tolerance rather than inheriting the 5s default. Omitting the key — or setting it to zero — keeps that default. `ServiceDiscoveryFeatures::metadata_pushdown == false`.

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

    /// TTL reaper interval for cluster_lock — upper bound on the reaper's sleep
    /// and the cadence of its gauge; an imminent expires_at shortens an
    /// individual sleep (§5.2). Default: 5s.
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
    /// Default: 1000 (see DESIGN §8/§11 — a cardinality signal, and the
    /// input to the deferred beacon-index decision, §2.1).
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
This is the operational counterpart to the `pg_locks` scan-cost risk
documented in §11: that scan is `O(advisory locks in the cluster)` and is paid
only on contended acquires, so this gauge is the load proxy a Grafana
panel/alert reads `cluster_lock_op_duration_seconds{op="try_lock"}` p99
against. It is also the input to the deferred beacon-index decision (§2.1). It is a plain count, not a per-name breakdown — lock names
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
beacon and garbage-collection events below. It has no leadership transitions of
its own to report (leader election is the SDK default over this plugin's cache,
and emits `cluster.leader.transition` itself).

**Beacon and garbage-collection events** (all plugin-local, all carrying the
beacon key / affected `lock` as log *fields*, never as metric labels):

| Event | Level | Meaning |
|---|---|---|
| `cluster.lock.beacon_established` | INFO, once per incarnation | The instance took its liveness beacon, at the stated `beacon_hi`/`beacon_lo`, `backend_pid`, and `epoch`. **This is the line that makes a row's `holder_beacon_*` traceable back to an instance** (§5.5); an operator reading the table greps for it |
| `cluster.lock.beacon_lost` | WARN | The once-per-second ping failed, so this instance can no longer prove it is alive: every lock it held is now stealable by the fleet, `lost_locks` reports how many local guards were purged, and acquisition fails until a fresh beacon is established (§5.2). Paired with a `cluster_provider_errors_total{op="lock_beacon_lost"}` increment. The signal to alert on |
| `cluster.lock.beacon_rows_handed_over` | INFO | After reconnecting, the instance deleted and announced the rows written by its *previous* incarnation, so waiters take those names now rather than on their own retry (§5.2). `handed_over` is the count |
| `cluster.lock.beacon_keepalive_unsupported` | DEBUG | A `tcp_keepalives_*` `SET` was refused by the platform (§5.2). Crash detection falls back to the platform default, still TTL-bounded — deliberately not a warning |
| `cluster.lock.orphan_rows_reclaimed` | WARN | The reaper's orphan sweep deleted rows this instance wrote but holds no guard for — acquisitions cancelled after their row committed (§5.2). Each was wedging its name for the whole fleet until its TTL. A steady stream means `lock()` timeouts are landing mid-acquire often enough to be worth widening |
| `cluster.lock.drain_incomplete` | WARN | `stop()` could not delete or announce this instance's rows on the way out (§10). They stop being vouched for the moment the beacon closes a step later, so the fleet reacquires those names by retry rather than by the release NOTIFY — a promptness cost, not a correctness one |

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
4. Hand back every lock still held, in **one statement** — `DELETE FROM cluster_lock WHERE holder_beacon_hi = $1 AND holder_beacon_lo = $2 RETURNING name`, followed by a batched `NOTIFY cluster_lock_released` of those names — then close the beacon connection, then close the `sqlx::PgPool` under a bounded `POOL_CLOSE_TIMEOUT` (10s — see §11's note on unbounded pool statements).

    **The beacon columns fence the drain for free**, which is what collapses what used to be a multi-pass, fixpoint-looping, ordering-sensitive procedure into a single `DELETE`. The key is per-incarnation, so the statement can only ever match rows *this instance* wrote; a row whose lock lapsed and was re-acquired elsewhere now carries the successor's beacon and is skipped — exactly what matching `holder_id` name-by-name had to do by hand. There is no delete-before-unlock ordering invariant any more, because there is no unlock; there is no fixpoint loop, because a straggler acquisition that commits its row *after* the DELETE costs nothing (the beacon closes moments later, leaving that row unvouched, so the next acquirer takes the name on its own heartbeat retry rather than waiting out the TTL).

    It is also strictly more correct than what it replaced, in one further way: it reclaims **orphaned** rows — rows with no live local guard, left by an acquisition cancelled after its INSERT committed — which a drain that iterates a local map cannot see at all. `PG-LOCK-021` asserts both halves: three held locks and one orphan, all gone after a clean `stop()`.

    The order is deliberate and the beacon is shut down **separately from the shared token** to enforce it: the drain reads the beacon key and needs the pool, so both must still be live when it runs. Releasing the beacon before the drain would be worse than premature — it would assert to the whole fleet that this instance is dead while it still held locks. The NOTIFY goes out on a connection the drain **detaches from the pool and closes explicitly**, because a `PoolConnection` is returned asynchronously: one released microseconds before `pool.close()` can still be in flight when that close returns, leaving a live backend behind that nothing subsequently closes (`PG-LIFE-003` catches exactly this). A failed drain is logged as `cluster.lock.drain_incomplete` and costs only promptness — those rows stop being vouched for when the beacon connection closes a step later.

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

**[Risk: beacon key collision]** Two live instances drawing the same 62-bit beacon key would mean one instance's beacon vouches for the other's dead rows, degrading those rows to TTL-bounded reclamation. It does *not* break mutual exclusion: the `holder_id` fence is unaffected, and a vouched-for row is only ever *harder* to steal. `pg_try_advisory_lock` returning `false` detects a collision at establishment and redraws, up to 8 times. Documented as a degradation mode, not guarded further.

**[Risk: `pg_locks` scan cost on the contended path — shipping on measurement]** The acquire predicate's liveness check is a function scan over `pg_lock_status()` with no index, so it is `O(advisory locks in the cluster)`. Three things bound the exposure: the `CASE` short-circuits it off the uncontended path entirely (`PG-SPEC-012`), the subplan is correlated against a single row located by primary key, and contended retries are already rate-limited to roughly four per second per waiter by the NOTIFY-plus-heartbeat design (§5.3). `PG-SPEC-014` records the baseline as an artefact rather than a threshold — on a CI container it measures roughly 0.6 ms at a handful of advisory locks rising to ~2.7 ms at 5000, i.e. the linear scaling the shape predicts, at absolute values far below any plausible lock TTL. **The signal to watch** is `cluster_lock_op_duration_seconds{op="try_lock"}` p99 read against `cluster_postgres_lock_active_names`; note the histogram carries no `result` dimension (deliberately, to mirror the CAS-based default backend's signal set), so contended and uncontended acquires share one distribution and a rise is diluted. **The pre-designed exit**, should it ever look bad: skip the liveness check for rows renewed recently (`WHEN cluster_lock.acquired_at > now() - $staleness THEN false`), paying the scan only for the suspicious set. Correctness is unaffected because skipping is strictly conservative — it declines to steal, never steals wrongly — at the cost of making crash detection `min($staleness, TTL)` rather than immediate.

**[Risk: PgBouncer transaction mode mis-configuration]** Silent mis-behaviour if an operator uses transaction-mode PgBouncer without the `pgbouncer_transaction_mode: true` config flag. Lock operations themselves are fine now, but the beacon is not: transaction pooling would release its advisory lock between transactions, which asserts to the fleet that this instance is dead while it still holds live locks (see §5.4). Mitigation: the startup validation flag; documentation.

**[Trade-off: prefix_watch is polling-based]** `watch_prefix` is serviced by `PollingPrefixWatch`, not a native LISTEN/NOTIFY subscription. This means prefix watch events have a latency of up to the poll interval (default 5s) and the poll cost is N `get` calls per interval. Service discovery use cases that require sub-second topology change propagation should use a backend with native prefix watch (etcd, NATS).

**[Retired: all lock operations serialize on one session]** They no longer do. Every lock statement runs on the write pool, so lock throughput is bounded by pool width like everything else, and the previous escape hatch (a set of sessions with lock-name-hash affinity) is moot. The property that motivated the single session — a held lock costing no pool connection — is unchanged and now stronger: a held lock costs no connection at all.

**[Trade-off: losing the beacon invalidates every lock on the instance]** One beacon means one blast radius. Where the pinned-connection model lost one lock per dropped connection, losing the beacon makes every `cluster_lock` row that instance holds stealable by the fleet at once (§5.1) — the rows survive, but nothing vouches for them any more. The failure is *detected*, not silent, and within a bounded time: the beacon task's 1s ping (`beacon::PING_INTERVAL`) plus its `beacon::STATEMENT_TIMEOUT` puts a ceiling on how long the instance can be wrong about what it holds, and the `holder_beacon_hi`/`holder_beacon_lo` fence — compared by key, so a reconnected beacon under a fresh key does not resurrect the old rows — turns every subsequent `renew` into `LockExpired` and leaves a `release` with no row of its own to delete. On detection the beacon task purges `local_holders`, logs `cluster.lock.beacon_lost` with the casualty count, and emits `cluster_provider_errors_total{op="lock_beacon_lost"}`; acquisition fails with a retryable `Provider { ConnectionLost }` until the beacon is re-established. But a consumer mid-critical-section still learns only at its next `renew`, since `LockGuard` has no asynchronous lost-lock signal. That is the same exposure ADR-002's no-remote-I/O-in-the-critical-section rule already governs, now with a wider fan-out per incident. Monitor `cluster.lock.beacon_lost` and `cluster_provider_errors_total{op="lock_beacon_lost"}`.

Note that a ping overrunning `beacon::STATEMENT_TIMEOUT` (§3.3) is read as a lost connection and carries the same blast radius, which makes runtime starvation a (remote) way to lose every lock on the instance without the database having done anything wrong. The bound is set at ~1000x the expected latency of a single-round-trip built-in on a dedicated connection precisely to keep that improbable, and the failure is detected and fenced rather than silent. There is no separate signal for it: a timed-out ping surfaces as `cluster.lock.beacon_lost` like any other loss, so `beacon_lost` firing without a matching backend outage is the signal that the bound is too tight for the deployment.

**[Risk: pool statements are not bounded client-side]** §3.3 bounds the beacon's ping and its reconnect, because an unresponsive round-trip there would wedge `stop()`. The **write pool** has no equivalent bound: `pool_acquire_timeout` covers checkout, but statement execution afterwards does not time out, and `sqlx` supplies no read timeout. Against a server that freezes *after* a successful checkout, any pool statement — a reaper sweep, a `renew`, a cache operation — can block indefinitely, and where that statement is inside a background task, `stop()` blocks on its join.

This is pre-existing and not specific to the lock half (it applies equally to every cache operation), which is why it is recorded here rather than fixed as part of the session refactor. The practical bound today comes from `pool_acquire_timeout` arithmetic: a frozen server normally fails the *next* checkout at the `before_acquire` hook, which is the path `PG-LOCK-019` exercises. That is an accident of timing, not a guarantee — do not read `PG-LOCK-019` as proof that `stop()` is bounded in general.

Two of its sharper edges are closed. `PgPool::close()` waits for **every** checked-out connection to come back, and the per-lock guard tasks are spawned detached — a guard parked in a `renew`'s pool I/O is neither preemptible by the shutdown token nor joined anywhere, so an unbounded `close()` relocated the stall out of the joins this section tells operators to budget for and into a step with no budget at all. It is now bounded by `POOL_CLOSE_TIMEOUT` (10s), which is safe because `close()` marks the pool closed *before* it starts waiting: giving up leaves the pool closed and any straggler connection closed when its holder returns it, and logs `cluster.lock.pool_close_timeout`. Separately, `BeaconHandle` has a `Drop` that cancels its token and aborts its task, so a supervisor's `timeout(D, handle.stop())` giving up mid-shutdown no longer leaks the beacon task and its off-pool backend for the life of the process — which is precisely the failure mode this section's own supervisor-level advice would otherwise have caused. (Leaking it would also keep this instance's beacon *granted*, so the fleet would go on treating its abandoned rows as live until the process exited.) Both handle `Drop`s also cancel the shared token before their diagnostic panic/warn, so a dropped `stop()` future still unwinds the background tasks.

What remains open is the general case: a client-side bound on pool *statements*. Until then, a deployment that needs a hard shutdown ceiling should still enforce it at the supervisor level.

**[Retired: same-instance exclusion enforced in-process]** Two acquisitions from the same instance now race exactly as two instances do — on the row lock of the conflicting tuple (§5.1) — so Postgres is the authority for both, and no in-process registry participates in exclusion at all. `PG-LOCK-008` (20 concurrent local callers) and `PG-LOCK-016` (two instances) are deliberately kept as separate scenarios even though they exercise the same mechanism now: that they *do* is the claim worth holding both halves to.

**[Trade-off: a holder is no longer told when its lock is reclaimed]** Reclamation used to route through the owning instance, so the owner necessarily noticed and logged it. A successor now steals the row directly and the previous holder learns only at its next `renew` — no behavioural difference for the consumer (`LockExpired` either way), but an operator loses a signal that fired without anyone having to ask for it. Deliberately not replaced: reinstating it means an indexed `SELECT` over `local_holders`' names on every reaper wake, which is the class of query this design removed, for a signal with no current consumer.

**[Trade-off: `synchronous_commit = on` enforced, no `off` mode]** The plugin enforces `synchronous_commit = on` on every connection (§3.4) and offers no `EventuallyConsistent`/weak-consistency mode. Operators who need `off`'s write-latency benefit and can tolerate its durability trade-off (risk of losing the last few commits on crash) cannot get it from this plugin — that use case belongs on a backend designed for it. Enforcement is via `after_connect` + `before_acquire` hooks (re-asserted on every checkout), which now covers every durability-relevant write including the `cluster_lock` rows. There is no longer any connection outside that: the one long-lived connection the lock opens is the beacon, which writes nothing at all and so has no durability setting to maintain (§3.4). The residual window this risk used to record is retired rather than accepted.

**[Risk: async replication is warn-only, not enforced]** ADR-009 requires synchronous streaming replication for Postgres leader/lock safety under failover, but §3.6's `replication_mode` check only warns (`cluster.provider.replication_async`) when it detects or is told the topology is async — it never fails startup. An operator who ignores or doesn't monitor that log line can run indefinitely on an async-replicated, failover-unsafe topology. This is a deliberate choice (topology isn't always confidently detectable, and some deployments legitimately don't need HA), not an oversight — but it means this is an operational monitoring dependency, not a guarantee enforced by the plugin itself; pair the WARN log with an alert, not just a dashboard.

**[Design choice: no read-path cache]** `get` is always read-through to Postgres (§4.3) — the plugin deliberately does not layer an in-process read cache in front of it. An in-process cache here would be local to each service instance, not shared across a fleet: at N instances it multiplies rather than amortizes correctness risk (each instance's cache would independently race NOTIFY-driven invalidation against concurrent reads, so different instances could transiently observe different values for the same key), while doing nothing to relieve the actual write-side bottleneck above (NOTIFY volume is driven by writers, not readers). It would also risk silently reaching the leader-election and service-discovery primitives that ride on this same cache backend (§6) specifically *because* it declares `Linearizable` consistency — caching those reads would undermine the reason this backend was chosen for them. The intended pattern is per-primitive backend selection: route a given primitive to the backend suited to its access pattern (e.g. Redis for a hot, staleness-tolerant application cache; this plugin for Postgres-backed locks/coordination), rather than asking one backend to be good at everything.

## 12. Open Questions

| Question | Owner | Target Resolution | Recommendation |
|---|---|---|---|
| Credstore-backed credential resolution for the connection string | Postgres plugin owner + Platform OOP deployment design | Future iteration, once the OOP/credstore wiring contract (`docs/arch/toolkit-oop/DESIGN.md` §Platform Host Composition; parent cluster `DESIGN.md:41`) is committed | Decided for now: `connection_string` uses `${VAR}` / `${VAR:-default}` env-var expansion (`toolkit_utils::var_expand` via `#[derive(toolkit_macros::ExpandVars)]` + `#[expand_vars]`, §7), the same mechanism `libs/toolkit-db` uses for DB passwords/DSNs — no `secret_ref` field is exposed by this plugin's config in the meantime. When the credstore path is eventually added, reuse the wiring crate's existing `BackendBinding.secret_ref: Option<SecretRef>` (`cluster/src/config.rs:83`) rather than reintroducing a plugin-local field of the same name at a different layer — that duplication is exactly what was removed here |

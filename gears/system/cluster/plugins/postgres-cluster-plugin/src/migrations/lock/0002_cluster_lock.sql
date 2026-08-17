-- cluster_lock: the store-owned lease row (DESIGN.md §2.1,
-- DESIGN-DEPLOYABLE-GEAR §5.8.1, ADR-012).
--
-- This migration is applied via its own `Migrator` (embedded from this
-- `migrations/lock/` subdirectory, separately from `migrations/cache/`), run
-- by both the combined `PostgresClusterPlugin` (cache + lock) and the
-- standalone `PostgresLockPlugin` (DESIGN.md §3.5) — either path only ever
-- runs the migrations its own tables need, so a lock-only deployment never
-- creates `cluster_cache`. Both `Migrator`s share the database's single
-- `_sqlx_migrations` tracking table, so each must be constructed with
-- `.set_ignore_missing(true)` — otherwise a lock-only `Migrator` (which only
-- knows about this file) fails validation the moment it sees the *other*
-- plugin's already-applied `0001_cluster_cache.sql` version recorded there.
--
-- `expires_at` is the lease's **absolute** deadline, computed as `now() + ttl` on
-- the *database* clock at insert and at every renew — not a raw
-- `acquired_at`/`ttl_ms` pair the TTL reaper (DESIGN.md §5.2) re-derives for
-- every row on every tick. A derived deadline cannot be indexed at all:
-- `timestamptz + interval` is `STABLE`, not `IMMUTABLE` (its result depends on
-- the session `TimeZone`), so Postgres rejects an expression index on
-- `acquired_at + ttl_ms * interval '1ms'`, and `now()` may not appear in a
-- partial-index predicate either — leaving the sweep a guaranteed sequential
-- scan with a per-row interval multiply. Storing the deadline makes that sweep
-- an indexed `WHERE expires_at <= now()`, and lets the reaper read
-- `min(expires_at)` (index-only) to wake when the next lease is actually due
-- instead of polling blindly. Same shape as `cluster_cache.expires_at`
-- (`0001_cluster_cache.sql`), and computed off the database clock for the same
-- reason (PGR-C2): a service instance's skewed wall clock must not be able to
-- make a lease expire early or linger.
--
-- **`expires_at` is the only liveness authority.** There is no second mechanism
-- vouching for a row, which is what makes any replica able to serve any lease
-- operation and a broker restart revoke nothing (invariant I7,
-- DESIGN-DEPLOYABLE-GEAR §5.8.2). An earlier revision carried two extra `int4`
-- columns holding the halves of a per-incarnation advisory key, which the acquire
-- predicate joined against `pg_locks` -- a liveness beacon, so that a
-- crashed holder's lock became stealable before its TTL. Sound while the process
-- holding the beacon was the process using the lock; unsound the moment locks are
-- brokered, because the broker's beacon would vouch for locks held by other, live
-- consumers and its restart would revoke the fleet's locks. Removed with the
-- acquire path's only unindexed scan, and its columns with it; the price, stated
-- rather than buried, is that a crashed holder's lock now lingers until its TTL in
-- every profile (ADR-012, Risk 5). Keep lock TTLs tight.
--
-- The index is unconditional, not partial like the cache's — a lease TTL is
-- mandatory, so there is no `NULL` subset to exclude.
--
-- `acquired_at` is kept for diagnostics only ("how long has this been held?"
-- when reading the table by hand); no query filters on it.
--
-- `owner` and `fence` are the lease token, and together they are the whole of the
-- authority over the row (§5.8.1): `renew` and `release` are conditional writes
-- predicated on `(name, owner, fence)` and on nothing any process remembers,
-- which is what lets a lease acquired through one replica be renewed through
-- another that never saw the acquire.
--
-- * `owner` is the holder's identity — a caller-supplied `ClientId` for a
--   brokered acquisition (§5.4), or a freshly minted UUID per in-process
--   `try_lock`/`lock` so two guards held concurrently in one process are distinct
--   owners and neither can renew or release the other's lease. `TEXT`, not the
--   `UUID` the superseded `holder_id` column used: a `ClientId` is an opaque
--   caller string, so the column cannot be narrower than what §5.4 admits. The
--   comparison is equality-only against a row already located by primary key, so
--   the collation-aware string compare costs nothing measurable next to the
--   16-byte one it replaces — and `fence` does most of the discriminating anyway.
--
-- * `fence` is per-`name` and strictly increases on every acquisition, including
--   a steal-on-expiry (`fence = cluster_lock.fence + 1` in the acquire's
--   `ON CONFLICT DO UPDATE`, read off the row's own current value rather than
--   bound by the acquirer). That is what makes "steal on expiry" safe: a stale
--   holder's `renew`/`release` fail their predicate rather than silently
--   succeeding against a lease someone else now owns. `BIGINT` starting at 1, so
--   0 stays available to name the absence of a claim; the positive CHECK is the
--   backstop for the `FIRST_FENCE` the Rust side writes.
--
-- **The fence is retained past the lease, but not past a release** (item `L3`).
-- A lapsed row is left in place for `fence_retention_ms` (default one hour) and
-- only then swept, so the counter survives the lapse: the reaper's predicate is
-- `expires_at <= now() - retention` while acquire's is still `expires_at <= now()`,
-- which is what lets the next acquirer steal a retained row **in place** at
-- `fence + 1` rather than insert a fresh one at 1. No schema change was needed for
-- that — `now() - interval` is a runtime constant, so the sweep is still the same
-- `cluster_lock_expires_idx` range scan (§5.8.1).
--
-- `release` still DELETEs, and that is the residual: an explicit release drops the
-- fence with the row, so the *next* acquisition of that name starts at 1 again.
-- The window covers a lease that **lapsed**, which is the case a stale holder can
-- actually be in — a holder that released knows it did. Closing the release half
-- would mean writing a tombstone instead of deleting, which changes
-- release-by-absence (`Ok` for "nothing there") and turns the waiter-visible
-- delete into an update; ADR-012 records it as deliberately out of scope.
--
-- Two consequences worth knowing before reading the reaper. Reaping is no longer
-- what wakes a waiter promptly — the row is deleted a window *after* anyone could
-- have taken the name, so `lock()`'s 250 ms heartbeat is the bound now. And row
-- count is no longer held-lock count: `cluster_postgres_lock_active_names` filters
-- on `expires_at > now()`, or it would report every name used in the last hour.
--
-- `name` is the PRIMARY KEY, so — exactly as with `cluster_cache.key`, see
-- `0001_cluster_cache.sql` — it lands in a btree bound by the ~2704 byte
-- index-tuple limit. `validate_lock_name` rejects an over-long name in Rust
-- before any lease state is mutated; this CHECK is the backstop. Keep the bound
-- in sync with `limits::MAX_INDEXED_KEY_BYTES`.
--
-- No index on `(owner, fence)`, deliberately. Nothing filters on either without
-- also filtering on `name`, which is the primary key, so every lease predicate is
-- already a single-row lookup and an index on the token halves would be pure write
-- amplification on the acquire path.
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

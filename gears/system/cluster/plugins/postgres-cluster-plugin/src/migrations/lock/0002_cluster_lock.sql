-- cluster_lock: metadata alongside the native pg_advisory_lock (DESIGN.md §2.1).
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
-- `expires_at` is the lock's **absolute** deadline, computed as `now() + ttl` on
-- the *database* clock at insert and at every renew — not a raw
-- `acquired_at`/`ttl_ms` pair the TTL reaper (DESIGN.md §5.2) re-derives for
-- every row on every tick. A derived deadline cannot be indexed at all:
-- `timestamptz + interval` is `STABLE`, not `IMMUTABLE` (its result depends on
-- the session `TimeZone`), so Postgres rejects an expression index on
-- `acquired_at + ttl_ms * interval '1ms'`, and `now()` may not appear in a
-- partial-index predicate either — leaving the sweep a guaranteed sequential
-- scan with a per-row interval multiply. Storing the deadline makes that sweep
-- an indexed `WHERE expires_at <= now()`, and lets the reaper read
-- `min(expires_at)` (index-only) to wake when the next lock is actually due
-- instead of polling blindly. Same shape as `cluster_cache.expires_at`
-- (`0001_cluster_cache.sql`), and computed off the database clock for the same
-- reason (PGR-C2): a service instance's skewed wall clock must not be able to
-- make a lock expire early or linger. The index is unconditional, not partial
-- like the cache's — a lock TTL is mandatory, so there is no `NULL` subset to
-- exclude.
--
-- `acquired_at` is kept for diagnostics only ("how long has this been held?"
-- when reading the table by hand); no query filters on it.
--
-- `holder_id` is a random UUID generated at acquire time and used as an
-- end-to-end ownership fence (PGR-L1): `renew()`, `release()`, and the reaper
-- all match on it, so a stale guard whose lock lapsed and was re-acquired by a
-- newer holder cannot renew, release, or reclaim the successor's live lock.
-- Native `UUID`, not `TEXT`: the only writer is `try_acquire`'s
-- `Uuid::new_v4()` (`lock/mod.rs`), so every value is a UUID by construction.
-- The native type stores it in a fixed 16 bytes instead of a 36-byte string,
-- makes the `holder_id = $n` fence in every `renew`/`release`/reaper query a
-- 16-byte comparison rather than a collation-aware string one, and makes the
-- contract the column's own rather than an implicit convention.
--
-- `holder_beacon_hi`/`holder_beacon_lo` are the two `int4` halves of the
-- liveness beacon vouching for this row (DESIGN.md §5.1): the single
-- per-incarnation advisory lock the holding instance took at startup on a
-- dedicated connection. They are what makes a row's ownership *checkable* by
-- anyone rather than only by its writer — the acquire predicate joins them
-- against `pg_locks`, so a row whose beacon is no longer granted is stealable
-- the instant Postgres notices the holder's connection is gone, without waiting
-- out `expires_at`. Nothing renews or maintains them: the beacon is released
-- only by its connection closing, which is precisely the event they exist to
-- detect.
--
-- Two `int4` halves rather than one `bigint`, and `NOT NULL` with a
-- non-negative CHECK, both for the predicate's sake. `pg_locks` exposes the
-- two-argument advisory key as `classid`/`objid`, which are `oid` (unsigned) —
-- keeping both halves non-negative makes the comparison a plain
-- `classid = holder_beacon_hi::oid` with no sign reinterpretation, and `NOT
-- NULL` keeps the predicate free of a `NULL` branch. The columns are declared
-- here rather than added by a follow-on migration exactly so that no schema
-- version ever exists in which a row can lack a beacon.
--
-- `name` is the PRIMARY KEY, so — exactly as with `cluster_cache.key`, see
-- `0001_cluster_cache.sql` — it lands in a btree bound by the ~2704 byte
-- index-tuple limit. `validate_lock_name` rejects an over-long name in Rust
-- before any lock state is mutated; this CHECK is the backstop. Keep the bound
-- in sync with `limits::MAX_INDEXED_KEY_BYTES`.
--
-- No index on `(holder_beacon_hi, holder_beacon_lo)`, deliberately. Three
-- statements filter on that pair — the reaper's orphan sweep, the shutdown
-- drain, and the post-reconnect cleanup — but the table holds only *active*
-- locks, so at the cardinality `cluster_postgres_lock_active_names` reports a
-- sequential scan is trivial and the index would be pure write amplification on
-- the acquire path. Revisit against that gauge, not up front.
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

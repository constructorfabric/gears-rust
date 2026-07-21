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
-- `ttl_ms` lets the lock TTL reaper (DESIGN.md §5.2) identify expired locks.
-- `holder_id` is a random UUID generated at acquire time and used as an
-- end-to-end ownership fence (PGR-L1): `renew()`, `release()`, and the reaper
-- all match on it, so a stale guard whose lock lapsed and was re-acquired by a
-- newer holder cannot renew, release, or reclaim the successor's live lock.
CREATE TABLE cluster_lock (
    name        TEXT        NOT NULL,
    holder_id   TEXT        NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ttl_ms      BIGINT      NOT NULL,
    PRIMARY KEY (name)
);

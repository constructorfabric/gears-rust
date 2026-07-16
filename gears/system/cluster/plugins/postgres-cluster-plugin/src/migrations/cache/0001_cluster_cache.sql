-- cluster_cache: the native ClusterCacheBackend store (DESIGN.md §2.1).
--
-- `version` starts at 1 on first insert and increments by 1 on every
-- successful write (including CAS) — see DESIGN.md §2.2 for the version
-- semantics this plugin follows (matching cluster_sdk::cache::CacheEntry).
--
-- `expires_at IS NULL` means no TTL. The partial index makes the TTL reaper's
-- sweep (DESIGN.md §4.2) efficient without touching indefinite entries.
CREATE TABLE cluster_cache (
    key        TEXT        NOT NULL,
    value      BYTEA       NOT NULL,
    version    BIGINT      NOT NULL DEFAULT 1,
    expires_at TIMESTAMPTZ,
    PRIMARY KEY (key)
);

CREATE INDEX cluster_cache_expires_idx ON cluster_cache (expires_at)
    WHERE expires_at IS NOT NULL;

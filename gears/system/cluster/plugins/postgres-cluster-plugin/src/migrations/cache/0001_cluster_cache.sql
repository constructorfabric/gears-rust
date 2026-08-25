-- cluster_cache: the native ClusterCacheBackend store (DESIGN.md §2.1).
--
-- `version` starts at 1 on first insert and increments by 1 on every
-- successful write (including CAS) — see DESIGN.md §2.2 for the version
-- semantics this plugin follows (matching cluster_sdk::cache::CacheEntry).
--
-- `expires_at IS NULL` means no TTL. The partial index makes the TTL reaper's
-- sweep (DESIGN.md §4.2) efficient without touching indefinite entries.
--
-- `key` is the PRIMARY KEY, so it lands in a btree and is bound by the ~2704
-- byte index-tuple limit — past that an INSERT fails outright with SQLSTATE
-- 54000. `limits::MAX_INDEXED_KEY_BYTES` rejects an over-long key in Rust
-- before the write; this CHECK is the backstop for anything reaching the table
-- another way (psql, a future code path), turning a mid-write server error
-- into a named constraint violation. Keep the bound in sync with that Rust
-- constant. `octet_length`, not `length`: the limit is on bytes, and a
-- multi-byte key has more of them than it has characters.
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

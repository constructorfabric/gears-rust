-- Created:  2026-05-11 by Constructor Tech

-- ── GTS type path domain ─────────────────────────────────────────────────────
-- GTS type identifier: single or chained, always ends with ~ (schema, not instance).
-- Format: gts.<vendor>.<package>.<namespace>.<type>.v<MAJOR>[.<MINOR>][~<segment>]*~
-- Spec:   https://github.com/GlobalTypeSystem/gts-spec
CREATE DOMAIN gts_type_path AS TEXT
    CHECK (
        LENGTH(VALUE) <= 1024
        AND VALUE ~ '^gts\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.v(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*))?(?:~[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.v(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*))?)*~$'
    );

-- ── Event Log (built-in DB storage backend) ──────────────────────────────────
-- This is the schema for the built-in DB storage backend specifically. Other backends
-- (Kafka, S3, file) have their own native storage layouts; this DDL is internal to
-- the DB backend's `persist` / `query` implementation.
--
-- No `UNIQUE (topic, partition, offset)` constraint — backend offset assignment
-- combined with single-writer-via-outbox order preservation guarantees uniqueness
-- without a global cross-segment unique index. The PRIMARY KEY on `id` (UUID,
-- client-provided) catches accidental duplicate event submissions.
--
-- Note: `tenant_id` is present for authorization filtering (queries are tenant-scoped
-- via SecureConn) but is NOT part of any uniqueness or sequencing key — topic GTS
-- identifiers are globally unique by namespace.
CREATE TABLE evbk_event (
    id              UUID        NOT NULL,
    tenant_id       UUID        NOT NULL,    -- publisher's tenant; authz scope, not sequencing scope
    topic           TEXT        NOT NULL,
    partition       INTEGER     NOT NULL,
    type            TEXT        NOT NULL,
    producer_id     UUID,                    -- chained / monotonic modes only; null in stateless
    previous        BIGINT,                  -- chained mode only; null otherwise
    sequence        BIGINT,                  -- producer-set chain (chained / monotonic); null in stateless
    "offset"        BIGINT      NOT NULL,    -- backend-assigned, monotonic per (topic, partition); consumer-visible
    offset_time     TIMESTAMP   NOT NULL,
    source          TEXT        NOT NULL,
    subject         TEXT        NOT NULL,
    subject_type    TEXT        NOT NULL,
    occurred_at     TIMESTAMP   NOT NULL,
    created_at      TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    trace_parent    TEXT,
    data            JSONB       NOT NULL,

    PRIMARY KEY (id)
);

CREATE INDEX idx_evbk_event_topic_partition_offset ON evbk_event (topic, partition, "offset");


-- ── Consumer Group Registry ──────────────────────────────────────────────────
-- Persistent registry row — outlives every subscription, every delivery shard
-- restart, every cache transition. Lives in the broker's persistent DB alongside
-- `evbk_topic` and `evbk_event_type`, NOT in the cache (which would re-introduce
-- the silent-collision bug after a cache wipe).
--
-- Two creation paths, one table — each path exclusive to one shape:
--  * `POST /v1/consumer_groups`  — anonymous-only (`id` is broker-minted ~<uuid>;
--    request body MUST NOT carry `id`); `tenant_id` and `owner_principal` come
--    from SecurityContext at create time. JOIN authz is owner-tenant equality.
--  * `types_registry` upsert     — named-only. At startup the broker reads
--    `types_registry` for instances of `gts.cf.core.events.consumer_group.v1~`
--    and upserts each into this table with `kind='named'`. Idempotent. JOIN authz
--    is explicit `:consume` grant on the concrete GTS instance via the PEP.
CREATE TABLE evbk_consumer_group (
    id                gts_type_path NOT NULL,    -- full GTS identifier (named: gts.cf.core.events.consumer_group.v1~vendor.foo.v1; anonymous: ~{uuid})
    tenant_id         UUID          NOT NULL,    -- owner tenant (from SecurityContext at create)
    owner_principal   TEXT          NOT NULL,    -- creator principal (from SecurityContext at create)
    kind              TEXT          NOT NULL,    -- 'named' | 'anonymous' (derived from id-instance shape, stored for fast filtering)
    description       TEXT,
    created_at        TIMESTAMP     NOT NULL DEFAULT CURRENT_TIMESTAMP,

    PRIMARY KEY (id)
);

CREATE INDEX idx_evbk_consumer_group_tenant ON evbk_consumer_group (tenant_id);


-- ── Producer Registration ────────────────────────────────────────────────────
-- Producer registration row: principal binding + declared mode + client_agent
-- diagnostic hint. `producer_id` is broker-minted at POST /v1/producers and
-- bound to `owner_principal`. `mode` is immutable for the lifetime of the row
-- (chained / monotonic only — stateless does not register).
-- `client_agent` is informational (RFC 9110 User-Agent grammar, ASCII, 1–256
-- bytes), surfaced in logs and metric labels alongside producer_id; it does
-- NOT participate in dedup / authz / ownership decisions.
-- The Reaper purges rows whose `last_seen_at` is older than the platform-wide
-- producer-registration TTL (default P30D). Cascade-deletes the producer's
-- evbk_producer_state rows on purge.
CREATE TABLE evbk_producer (
    producer_id     UUID        PRIMARY KEY,
    owner_principal TEXT        NOT NULL,
    mode            TEXT        NOT NULL CHECK (mode IN ('chained', 'monotonic')),
    client_agent    TEXT        NOT NULL,
    created_at      TIMESTAMP   NOT NULL,
    last_seen_at    TIMESTAMP   NOT NULL
);

-- ── Producer Chain State ─────────────────────────────────────────────────────
-- Producer chain state keyed by (producer_id, topic, partition) — global, not
-- narrowed by tenant. `last_sequence` tracks the highest producer-set
-- `meta.sequence` accepted from this producer for this (topic, partition). The
-- Reaper worker cleans up records where `last_seen_at` is older than the
-- topic's `retention` (capped at P14D). Cascade-deleted when the parent
-- evbk_producer registration row is aged out.
--
-- Chain check at ingest (chained mode): on incoming event with
--   meta.{previous, sequence} the broker verifies
--   meta.previous == state.last_sequence AND
--   meta.sequence > state.last_sequence.
--     Match              → accept and update last_sequence = meta.sequence.
--     Previous mismatch  → 412 SequenceViolation (recover via
--                          GET /v1/producers/{producer_id}/cursors).
--     Duplicate          → return original event with 200 OK.
--
-- Monotonicity check (monotonic mode, no `meta.previous`): just
--   meta.sequence > state.last_sequence. Gaps allowed in MVP; future
--   enforcement settable at registration.
--
-- Concurrent updates to a single (producer_id, topic, partition) row act as the
-- fencing mechanism — exactly one writer advances `last_sequence` at a time.
CREATE TABLE evbk_producer_state (
    producer_id     UUID        NOT NULL REFERENCES evbk_producer(producer_id) ON DELETE CASCADE,
    topic           TEXT        NOT NULL,
    partition       INTEGER     NOT NULL,
    last_sequence   BIGINT      NOT NULL DEFAULT 0,
    last_seen_at    TIMESTAMP   NOT NULL,

    PRIMARY KEY (producer_id, topic, partition)
);

CREATE INDEX idx_evbk_producer_state_last_seen ON evbk_producer_state (last_seen_at);


-- ── Pending DDL (not yet specified in DESIGN.md) ─────────────────────────────
-- The following tables are listed in DESIGN.md §3.7 "Core Tables" inventory but
-- their CREATE TABLE statements are not yet written into the design. They will
-- be added when their schemas are finalized and documented in DESIGN.md:
--
--   * evbk_topic         — Topic definitions. PK: id (GTS string).
--   * evbk_event_type    — Event type definitions. PK: id (GTS string); FK to evbk_topic.
--   * evbk_segment       — Topic storage segments. PK: (topic, partition, segment_id).
--
-- Subscription, cursor, and group-state are intentionally NOT in this file — they
-- live in the ClusterCapabilities-backed cache (see DESIGN.md §3.2 Subscription
-- Resolution Cache / GroupState Cache).

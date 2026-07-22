-- TimescaleDB Usage Collector storage backend — base schema.
CREATE EXTENSION IF NOT EXISTS timescaledb;

CREATE TABLE IF NOT EXISTS usage_type_catalog (
    gts_id          text PRIMARY KEY,
    kind            text NOT NULL CHECK (kind IN ('counter', 'gauge')),
    metadata_fields text[] NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS usage_records (
    uuid            uuid        NOT NULL,
    tenant_id       uuid        NOT NULL,
    gts_id          text        NOT NULL,
    value           numeric     NOT NULL,
    created_at      timestamptz NOT NULL,
    resource_id     text        NOT NULL,
    resource_type   text        NOT NULL,
    subject_id      text,
    subject_type    text,
    idempotency_key text        NOT NULL,
    corrects_id     uuid,
    status          text        NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
    metadata        jsonb       NOT NULL DEFAULT '{}'::jsonb,
    ingested_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (uuid, created_at),
    -- Dedup authority: `INSERT … ON CONFLICT (tenant_id, gts_id, idempotency_key,
    -- created_at) DO NOTHING` serializes concurrent same-key ingest and decides
    -- insert-vs-absorb-vs-conflict (record_store.rs). A hypertable UNIQUE must
    -- include the partition column (`created_at`), so the dedup identity is the
    -- 4-tuple `(tenant_id, gts_id, idempotency_key, created_at)` — the canonical
    -- dedup key per ADR-0014, not a divergence from the SPI. The plugin's
    -- remaining divergence is retention-bounded key preservation (the key
    -- becomes reusable once its chunk is dropped, vs. the SPI's permanent
    -- preservation); see DESIGN.md §2.2.
    CONSTRAINT usage_records_dedup_uniq
        UNIQUE (tenant_id, gts_id, idempotency_key, created_at),
    CONSTRAINT usage_records_gts_id_fk
        FOREIGN KEY (gts_id) REFERENCES usage_type_catalog (gts_id) ON DELETE RESTRICT
);

SELECT create_hypertable('usage_records', 'created_at', if_not_exists => TRUE);

CREATE INDEX IF NOT EXISTS usage_records_tenant_gts_time_idx
    ON usage_records (tenant_id, gts_id, created_at DESC);
CREATE INDEX IF NOT EXISTS usage_records_tenant_time_idx
    ON usage_records (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS usage_records_corrects_id_idx
    ON usage_records (corrects_id) WHERE corrects_id IS NOT NULL;

//! Usage capture and normalization's tables (feature 04; DESIGN §3.7): the source checkpoints,
//! the usage records with their raw capture, normalized and contribution-state columns, the
//! capture rejects and the source losses. The attribution and counted-coordinate columns of
//! `bss_rating__usage_record` and its window and deferred-state indexes are feature 05's.
//!
//! The checkpoint and capture-reject tables are platform-scoped: a source spans tenants. A
//! quantity is the entry's canonical decimal text (T-D-84), checked against the collector's
//! pattern on PostgreSQL and in code on SQLite (which has no regular expressions); every other
//! check is the same on both.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS bss_rating__source_checkpoint (
        source_id text PRIMARY KEY,
        source_kind text NOT NULL,
        source_definition jsonb NOT NULL,
        cursor text NULL,
        applied_seq bigint NULL,
        state text NOT NULL,
        updated_at timestamptz NOT NULL,
        config_generation bigint NOT NULL DEFAULT 0,
        CONSTRAINT ck_bss_rating__source_checkpoint__kind CHECK (source_kind IN
            ('usage', 'segments', 'period_facts')),
        CONSTRAINT ck_bss_rating__source_checkpoint__state CHECK (state IN
            ('active', 'draining', 'retired')),
        CONSTRAINT ck_bss_rating__source_checkpoint__cursor CHECK (cursor IS NULL
            OR length(cursor) BETWEEN 1 AND 4096),
        CONSTRAINT ck_bss_rating__source_checkpoint__generation CHECK (config_generation >= 0)
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__usage_record (
        tenant_id uuid NOT NULL,
        usage_record_id uuid NOT NULL,
        source_id text NOT NULL,
        raw_entry jsonb NOT NULL,
        raw_sha256 text NOT NULL,
        raw_digest_version smallint NOT NULL,
        captured_at timestamptz NOT NULL,
        gts_type text NULL,
        idempotency_key text NULL,
        resource_type text NULL,
        resource_id text NULL,
        subject_ref jsonb NULL,
        interval_start timestamptz NULL,
        interval_end timestamptz NULL,
        quantity text NULL,
        metadata jsonb NULL,
        entry_type text NULL,
        invalidates_id uuid NULL,
        reason_code text NULL,
        origin text NULL,
        accepted_at timestamptz NULL,
        contribution_state text NOT NULL,
        reject_reason text NULL,
        withdrawn_by uuid NULL,
        withdrawn_at timestamptz NULL,
        PRIMARY KEY (tenant_id, usage_record_id),
        CONSTRAINT ck_bss_rating__usage_record__digest CHECK (raw_sha256 ~ '^[0-9a-f]{64}$'),
        CONSTRAINT ck_bss_rating__usage_record__digest_version CHECK (raw_digest_version >= 1),
        CONSTRAINT ck_bss_rating__usage_record__state CHECK (contribution_state IN
            ('captured', 'rejected', 'awaiting_attribution', 'awaiting_spec',
             'boundary_split_required', 'quarantined', 'counted', 'withdrawn', 'applied')),
        CONSTRAINT ck_bss_rating__usage_record__reject_reason CHECK (reject_reason IS NULL
            OR reject_reason IN ('usage_malformed', 'interval_missing', 'usage_natural_key_conflict')),
        CONSTRAINT ck_bss_rating__usage_record__rejected_has_reason CHECK
            (contribution_state <> 'rejected' OR reject_reason IS NOT NULL),
        CONSTRAINT ck_bss_rating__usage_record__withdrawn_terminal CHECK
            (withdrawn_by IS NULL OR contribution_state = 'withdrawn'),
        CONSTRAINT ck_bss_rating__usage_record__withdrawn_by CHECK
            ((contribution_state = 'withdrawn') = (withdrawn_by IS NOT NULL)),
        CONSTRAINT ck_bss_rating__usage_record__withdrawn_at CHECK
            ((withdrawn_by IS NULL) = (withdrawn_at IS NULL)),
        CONSTRAINT ck_bss_rating__usage_record__entry_type CHECK (entry_type IS NULL
            OR entry_type IN ('record', 'invalidation')),
        CONSTRAINT ck_bss_rating__usage_record__invalidates CHECK
            ((entry_type = 'invalidation') = (invalidates_id IS NOT NULL)),
        CONSTRAINT ck_bss_rating__usage_record__applied CHECK
            (contribution_state <> 'applied' OR entry_type = 'invalidation'),
        CONSTRAINT ck_bss_rating__usage_record__origin CHECK (origin IS NULL
            OR origin IN ('live', 'backfill')),
        CONSTRAINT ck_bss_rating__usage_record__quantity CHECK (quantity IS NULL
            OR quantity ~ '^-?(0|[1-9][0-9]{0,27})(\.[0-9]{1,28})?$'),
        CONSTRAINT ck_bss_rating__usage_record__interval CHECK (interval_end >= interval_start),
        CONSTRAINT ck_bss_rating__usage_record__normalized CHECK (
            (gts_type IS NULL) = (entry_type IS NULL)
            AND (gts_type IS NULL) = (quantity IS NULL)
            AND (gts_type IS NULL) = (interval_start IS NULL)
            AND (gts_type IS NULL) = (accepted_at IS NULL))
    )",
    // The collector's dedup identity: a safety net once normalized (12 §4.3).
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__usage_record__natural_key
        ON bss_rating__usage_record (tenant_id, gts_type, idempotency_key, interval_start,
            interval_end, entry_type)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__usage_record__invalidates
        ON bss_rating__usage_record (tenant_id, invalidates_id) WHERE entry_type = 'invalidation'",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__usage_record__resource
        ON bss_rating__usage_record (tenant_id, resource_id, interval_start)",
    r"CREATE TABLE IF NOT EXISTS bss_rating__usage_capture_reject (
        source_id text NOT NULL,
        raw_sha256 text NOT NULL,
        raw_entry jsonb NOT NULL,
        captured_at timestamptz NOT NULL,
        reason text NOT NULL,
        loss_id uuid NULL,
        PRIMARY KEY (source_id, raw_sha256),
        CONSTRAINT ck_bss_rating__usage_capture_reject__digest CHECK
            (raw_sha256 ~ '^[0-9a-f]{64}$'),
        CONSTRAINT ck_bss_rating__usage_capture_reject__reason CHECK (reason IN
            ('unidentifiable', 'outside_subscription')),
        CONSTRAINT ck_bss_rating__usage_capture_reject__loss CHECK
            ((reason = 'unidentifiable') = (loss_id IS NOT NULL))
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__source_loss (
        tenant_id uuid NOT NULL,
        loss_id uuid NOT NULL,
        source_id text NOT NULL,
        tenant_scope jsonb NOT NULL,
        kind text NOT NULL,
        gts_type_ids jsonb NOT NULL,
        detected_at timestamptz NOT NULL,
        last_good_cursor text NULL,
        last_good_at timestamptz NULL,
        restart_cursor text NULL,
        state text NOT NULL,
        tainted_days jsonb NOT NULL,
        resolution text NULL,
        evidence_ref text NULL,
        resolved_by uuid NULL,
        approved_by uuid NULL,
        resolved_at timestamptz NULL,
        PRIMARY KEY (tenant_id, loss_id),
        CONSTRAINT ck_bss_rating__source_loss__kind CHECK (kind IN
            ('cursor_beyond_retention', 'unidentifiable_entry', 'scope_widened_beyond_retention')),
        CONSTRAINT ck_bss_rating__source_loss__state CHECK (state IN
            ('open', 'narrowed', 'pending_approval', 'resolved')),
        CONSTRAINT ck_bss_rating__source_loss__resolution CHECK (resolution IS NULL
            OR resolution IN ('repaired', 'accepted_loss')),
        CONSTRAINT ck_bss_rating__source_loss__resolved CHECK
            ((state = 'resolved') = (resolved_at IS NOT NULL)),
        CONSTRAINT ck_bss_rating__source_loss__two_person CHECK (approved_by IS NULL
            OR resolved_by IS NULL OR approved_by <> resolved_by)
    )",
    // One not-resolved row per source, kind and last good cursor: a re-detected loss reuses it.
    // The cursor is hashed, as a 4 KiB token can exceed a btree row.
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__source_loss__open
        ON bss_rating__source_loss (tenant_id, source_id, kind,
            md5(COALESCE(last_good_cursor, ''))) WHERE state <> 'resolved'",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__source_loss__source
        ON bss_rating__source_loss (tenant_id, source_id) WHERE state <> 'resolved'",
];

const SQLITE_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS bss_rating__source_checkpoint (
        source_id text PRIMARY KEY,
        source_kind text NOT NULL,
        source_definition text NOT NULL,
        cursor text NULL,
        applied_seq integer NULL,
        state text NOT NULL,
        updated_at text NOT NULL,
        config_generation integer NOT NULL DEFAULT 0,
        CONSTRAINT ck_bss_rating__source_checkpoint__kind CHECK (source_kind IN
            ('usage', 'segments', 'period_facts')),
        CONSTRAINT ck_bss_rating__source_checkpoint__state CHECK (state IN
            ('active', 'draining', 'retired')),
        CONSTRAINT ck_bss_rating__source_checkpoint__cursor CHECK (cursor IS NULL
            OR length(cursor) BETWEEN 1 AND 4096),
        CONSTRAINT ck_bss_rating__source_checkpoint__generation CHECK (config_generation >= 0)
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__usage_record (
        tenant_id text NOT NULL,
        usage_record_id text NOT NULL,
        source_id text NOT NULL,
        raw_entry text NOT NULL,
        raw_sha256 text NOT NULL,
        raw_digest_version integer NOT NULL,
        captured_at text NOT NULL,
        gts_type text NULL,
        idempotency_key text NULL,
        resource_type text NULL,
        resource_id text NULL,
        subject_ref text NULL,
        interval_start text NULL,
        interval_end text NULL,
        quantity text NULL,
        metadata text NULL,
        entry_type text NULL,
        invalidates_id text NULL,
        reason_code text NULL,
        origin text NULL,
        accepted_at text NULL,
        contribution_state text NOT NULL,
        reject_reason text NULL,
        withdrawn_by text NULL,
        withdrawn_at text NULL,
        PRIMARY KEY (tenant_id, usage_record_id),
        CONSTRAINT ck_bss_rating__usage_record__digest CHECK (length(raw_sha256) = 64),
        CONSTRAINT ck_bss_rating__usage_record__digest_version CHECK (raw_digest_version >= 1),
        CONSTRAINT ck_bss_rating__usage_record__state CHECK (contribution_state IN
            ('captured', 'rejected', 'awaiting_attribution', 'awaiting_spec',
             'boundary_split_required', 'quarantined', 'counted', 'withdrawn', 'applied')),
        CONSTRAINT ck_bss_rating__usage_record__reject_reason CHECK (reject_reason IS NULL
            OR reject_reason IN ('usage_malformed', 'interval_missing', 'usage_natural_key_conflict')),
        CONSTRAINT ck_bss_rating__usage_record__rejected_has_reason CHECK
            (contribution_state <> 'rejected' OR reject_reason IS NOT NULL),
        CONSTRAINT ck_bss_rating__usage_record__withdrawn_terminal CHECK
            (withdrawn_by IS NULL OR contribution_state = 'withdrawn'),
        CONSTRAINT ck_bss_rating__usage_record__withdrawn_by CHECK
            ((contribution_state = 'withdrawn') = (withdrawn_by IS NOT NULL)),
        CONSTRAINT ck_bss_rating__usage_record__withdrawn_at CHECK
            ((withdrawn_by IS NULL) = (withdrawn_at IS NULL)),
        CONSTRAINT ck_bss_rating__usage_record__entry_type CHECK (entry_type IS NULL
            OR entry_type IN ('record', 'invalidation')),
        CONSTRAINT ck_bss_rating__usage_record__invalidates CHECK
            ((entry_type = 'invalidation') = (invalidates_id IS NOT NULL)),
        CONSTRAINT ck_bss_rating__usage_record__applied CHECK
            (contribution_state <> 'applied' OR entry_type = 'invalidation'),
        CONSTRAINT ck_bss_rating__usage_record__origin CHECK (origin IS NULL
            OR origin IN ('live', 'backfill')),
        CONSTRAINT ck_bss_rating__usage_record__interval CHECK (interval_end >= interval_start),
        CONSTRAINT ck_bss_rating__usage_record__normalized CHECK (
            (gts_type IS NULL) = (entry_type IS NULL)
            AND (gts_type IS NULL) = (quantity IS NULL)
            AND (gts_type IS NULL) = (interval_start IS NULL)
            AND (gts_type IS NULL) = (accepted_at IS NULL))
    )",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__usage_record__natural_key
        ON bss_rating__usage_record (tenant_id, gts_type, idempotency_key, interval_start,
            interval_end, entry_type)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__usage_record__invalidates
        ON bss_rating__usage_record (tenant_id, invalidates_id) WHERE entry_type = 'invalidation'",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__usage_record__resource
        ON bss_rating__usage_record (tenant_id, resource_id, interval_start)",
    r"CREATE TABLE IF NOT EXISTS bss_rating__usage_capture_reject (
        source_id text NOT NULL,
        raw_sha256 text NOT NULL,
        raw_entry text NOT NULL,
        captured_at text NOT NULL,
        reason text NOT NULL,
        loss_id text NULL,
        PRIMARY KEY (source_id, raw_sha256),
        CONSTRAINT ck_bss_rating__usage_capture_reject__digest CHECK (length(raw_sha256) = 64),
        CONSTRAINT ck_bss_rating__usage_capture_reject__reason CHECK (reason IN
            ('unidentifiable', 'outside_subscription')),
        CONSTRAINT ck_bss_rating__usage_capture_reject__loss CHECK
            ((reason = 'unidentifiable') = (loss_id IS NOT NULL))
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__source_loss (
        tenant_id text NOT NULL,
        loss_id text NOT NULL,
        source_id text NOT NULL,
        tenant_scope text NOT NULL,
        kind text NOT NULL,
        gts_type_ids text NOT NULL,
        detected_at text NOT NULL,
        last_good_cursor text NULL,
        last_good_at text NULL,
        restart_cursor text NULL,
        state text NOT NULL,
        tainted_days text NOT NULL,
        resolution text NULL,
        evidence_ref text NULL,
        resolved_by text NULL,
        approved_by text NULL,
        resolved_at text NULL,
        PRIMARY KEY (tenant_id, loss_id),
        CONSTRAINT ck_bss_rating__source_loss__kind CHECK (kind IN
            ('cursor_beyond_retention', 'unidentifiable_entry', 'scope_widened_beyond_retention')),
        CONSTRAINT ck_bss_rating__source_loss__state CHECK (state IN
            ('open', 'narrowed', 'pending_approval', 'resolved')),
        CONSTRAINT ck_bss_rating__source_loss__resolution CHECK (resolution IS NULL
            OR resolution IN ('repaired', 'accepted_loss')),
        CONSTRAINT ck_bss_rating__source_loss__resolved CHECK
            ((state = 'resolved') = (resolved_at IS NOT NULL)),
        CONSTRAINT ck_bss_rating__source_loss__two_person CHECK (approved_by IS NULL
            OR resolved_by IS NULL OR approved_by <> resolved_by)
    )",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__source_loss__open
        ON bss_rating__source_loss (tenant_id, source_id, kind, COALESCE(last_good_cursor, ''))
        WHERE state <> 'resolved'",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__source_loss__source
        ON bss_rating__source_loss (tenant_id, source_id) WHERE state <> 'resolved'",
];

const DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS bss_rating__source_loss",
    "DROP TABLE IF EXISTS bss_rating__usage_capture_reject",
    "DROP TABLE IF EXISTS bss_rating__usage_record",
    "DROP TABLE IF EXISTS bss_rating__source_checkpoint",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let dialects = super::Dialects {
            postgres: PG_UP,
            sqlite: SQLITE_UP,
        };
        super::exec_backend(self.name(), manager, dialects).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, super::Dialects::both(DOWN)).await
    }
}

//! Fact intake's tables (feature 06 §5.1; DESIGN §3.7; DECISIONS T-D-87): the fact versions and
//! their heads, the child windows, the usage schedules, the usage policy projection, the pricing
//! answers and questions, and the derived declarations. The finalization-policy tables and the
//! scheduler's columns' use are feature 06b's; the roll-up columns of the fact head are feature
//! 09's.
//!
//! The inbox gains two quarantine reasons — `instant_not_whole_second` and `payload_invalid`, a
//! payload its adapter refuses (DESIGN 09 §4.1) — so its reason check is widened: an `ALTER` on
//! PostgreSQL, a table rebuild on SQLite, which cannot alter a check.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    "ALTER TABLE bss_rating__inbox DROP CONSTRAINT IF EXISTS ck_bss_rating__inbox__quarantine_reason",
    r"ALTER TABLE bss_rating__inbox ADD CONSTRAINT ck_bss_rating__inbox__quarantine_reason
        CHECK (quarantine_reason IS NULL OR quarantine_reason IN ('digest_conflict',
            'unsupported_schema_version', 'instant_not_whole_second', 'payload_invalid'))",
    r"CREATE TABLE IF NOT EXISTS bss_rating__fact (
        tenant_id uuid NOT NULL,
        fact_id uuid NOT NULL,
        fact_version bigint NOT NULL,
        entry_id uuid NOT NULL,
        kind text NOT NULL,
        obligation text NOT NULL,
        predecessor_version bigint NULL,
        subscription_id uuid NOT NULL,
        sub_line_key text NOT NULL,
        item_id uuid NOT NULL,
        billing_group_id uuid NOT NULL,
        seller_tenant_id uuid NOT NULL,
        payer_tenant_id uuid NOT NULL,
        resource_tenant_id uuid NOT NULL,
        currency text NOT NULL,
        period_start timestamptz NOT NULL,
        period_end timestamptz NOT NULL,
        served_from timestamptz NOT NULL,
        served_to timestamptz NOT NULL,
        timing text NOT NULL,
        occurrence_id text NULL,
        billing_terms jsonb NOT NULL,
        suspensions jsonb NOT NULL,
        timeline jsonb NOT NULL,
        accepted_terms jsonb NOT NULL,
        producer_digest text NOT NULL,
        payload_digest text NOT NULL,
        payload jsonb NOT NULL,
        received_at timestamptz NOT NULL,
        PRIMARY KEY (tenant_id, fact_id, fact_version),
        CONSTRAINT ck_bss_rating__fact__kind CHECK (kind IN ('recurring', 'usage', 'one_time')),
        CONSTRAINT ck_bss_rating__fact__obligation CHECK (obligation IN ('active', 'zeroed')),
        CONSTRAINT ck_bss_rating__fact__timing CHECK (timing IN ('advance', 'arrears')),
        CONSTRAINT ck_bss_rating__fact__version CHECK (fact_version >= 1),
        CONSTRAINT ck_bss_rating__fact__period CHECK (period_start < period_end),
        CONSTRAINT ck_bss_rating__fact__served CHECK (period_start <= served_from
            AND served_from <= served_to AND served_to <= period_end),
        CONSTRAINT ck_bss_rating__fact__seller CHECK (seller_tenant_id = tenant_id),
        CONSTRAINT ck_bss_rating__fact__producer_digest CHECK (producer_digest ~ '^[0-9a-f]{64}$'),
        CONSTRAINT ck_bss_rating__fact__digest CHECK (payload_digest ~ '^[0-9a-f]{64}$')
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__fact_head (
        tenant_id uuid NOT NULL,
        fact_id uuid NOT NULL,
        shard smallint NOT NULL,
        kind text NOT NULL,
        current_version bigint NOT NULL,
        binding_state text NOT NULL,
        binding_reason text NULL,
        bound_version bigint NULL,
        binding_next_check_at timestamptz NULL,
        expected_manifest_digest text NULL,
        rollup_due_at timestamptz NULL,
        currency text NOT NULL,
        received_at timestamptz NOT NULL,
        updated_at timestamptz NOT NULL,
        PRIMARY KEY (tenant_id, fact_id),
        CONSTRAINT ck_bss_rating__fact_head__shard CHECK (shard >= 0),
        CONSTRAINT ck_bss_rating__fact_head__kind CHECK (kind IN ('recurring', 'usage', 'one_time')),
        CONSTRAINT ck_bss_rating__fact_head__state CHECK (binding_state IN
            ('pending', 'bound', 'failed')),
        CONSTRAINT ck_bss_rating__fact_head__reason CHECK ((binding_state = 'bound')
            = (binding_reason IS NULL) AND (binding_reason IS NULL OR binding_reason IN
            ('derived_declaration_unavailable', 'derived_declaration_conflict'))),
        CONSTRAINT ck_bss_rating__fact_head__retry CHECK (binding_next_check_at IS NULL
            OR binding_state = 'pending'),
        CONSTRAINT ck_bss_rating__fact_head__bound CHECK (binding_state = 'bound'
            OR (expected_manifest_digest IS NULL AND rollup_due_at IS NULL)),
        CONSTRAINT ck_bss_rating__fact_head__manifest CHECK (expected_manifest_digest IS NULL
            OR expected_manifest_digest ~ '^[0-9a-f]{64}$')
    )",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__fact_head__rollup
        ON bss_rating__fact_head (shard, rollup_due_at) WHERE rollup_due_at IS NOT NULL",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__fact_head__binding
        ON bss_rating__fact_head (shard, binding_next_check_at) WHERE binding_state = 'pending'",
    r"CREATE TABLE IF NOT EXISTS bss_rating__child_window (
        child_id uuid PRIMARY KEY,
        tenant_id uuid NOT NULL,
        fact_id uuid NOT NULL,
        shard smallint NOT NULL,
        child_kind text NOT NULL,
        subscription_id uuid NOT NULL,
        item_id uuid NOT NULL,
        dimension_value text NULL,
        aggregation_key jsonb NOT NULL,
        agg_key_digest text NOT NULL,
        window_group_id uuid NULL,
        window_start timestamptz NOT NULL,
        window_end timestamptz NOT NULL,
        served_from timestamptz NOT NULL,
        served_to timestamptz NOT NULL,
        status text NOT NULL,
        pending_reason text NULL,
        failure_reason text NULL,
        next_check_at timestamptz NULL,
        input_generation bigint NOT NULL,
        current_revision bigint NULL,
        current_covered_generation bigint NULL,
        current_input_digest text NULL,
        binding_of_record text NULL,
        binding_resolves jsonb NULL,
        engine_of_record integer NULL,
        requested_rebind boolean NOT NULL,
        requested_engine integer NULL,
        requested_run_id uuid NULL,
        provisional_outcome jsonb NULL,
        last_reason_code text NULL,
        queued_at timestamptz NULL,
        created_at timestamptz NOT NULL,
        updated_at timestamptz NOT NULL,
        CONSTRAINT ck_bss_rating__child_window__kind CHECK (child_kind IN
            ('usage_window', 'period_line', 'one_time')),
        CONSTRAINT ck_bss_rating__child_window__status CHECK (status IN
            ('pending', 'provisional', 'final', 'final_stale', 'failed')),
        CONSTRAINT ck_bss_rating__child_window__failure CHECK (failure_reason IS NULL
            OR status = 'failed'),
        CONSTRAINT ck_bss_rating__child_window__window CHECK (window_start < window_end
            AND window_start <= served_from AND served_from <= served_to
            AND served_to <= window_end),
        CONSTRAINT ck_bss_rating__child_window__generation CHECK (input_generation >= 0),
        CONSTRAINT ck_bss_rating__child_window__shard CHECK (shard >= 0),
        CONSTRAINT ck_bss_rating__child_window__agg CHECK (agg_key_digest ~ '^[0-9a-f]{64}$')
    )",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__child_window__window_key
        ON bss_rating__child_window (tenant_id, fact_id, window_start, agg_key_digest)",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__child_window__due
        ON bss_rating__child_window (shard, next_check_at)
        WHERE status IN ('pending', 'provisional', 'final_stale') AND next_check_at IS NOT NULL",
    r"CREATE TABLE IF NOT EXISTS bss_rating__window_schedule (
        tenant_id uuid NOT NULL,
        fact_id uuid NOT NULL,
        item_id uuid NOT NULL,
        shard smallint NOT NULL,
        state text NOT NULL,
        fact_version bigint NOT NULL,
        window_policy text NOT NULL,
        usage_policy_id uuid NOT NULL,
        usage_policy_version bigint NOT NULL,
        usage_policy_digest text NOT NULL,
        finalization_policy_id uuid NULL,
        finalization_policy_version bigint NULL,
        finalization_policy_owner_scope text NULL,
        delay_seconds bigint NULL,
        evidence_mode text NULL,
        next_due_window_start timestamptz NULL,
        next_due_at timestamptz NOT NULL,
        expected_count bigint NOT NULL,
        scheduler_version bigint NOT NULL,
        updated_at timestamptz NOT NULL,
        PRIMARY KEY (tenant_id, fact_id, item_id),
        CONSTRAINT ck_bss_rating__window_schedule__state CHECK (state IN
            ('policy_pending', 'scheduling', 'complete')),
        CONSTRAINT ck_bss_rating__window_schedule__window CHECK (window_policy IN
            ('billing_cycle', 'calendar_hour')),
        CONSTRAINT ck_bss_rating__window_schedule__pinned CHECK ((state = 'policy_pending')
            = (finalization_policy_id IS NULL)),
        CONSTRAINT ck_bss_rating__window_schedule__policy CHECK (
            (finalization_policy_id IS NULL) = (finalization_policy_version IS NULL)
            AND (finalization_policy_id IS NULL) = (finalization_policy_owner_scope IS NULL)
            AND (finalization_policy_id IS NULL) = (delay_seconds IS NULL)
            AND (finalization_policy_id IS NULL) = (evidence_mode IS NULL)),
        CONSTRAINT ck_bss_rating__window_schedule__counts CHECK (fact_version >= 1
            AND expected_count >= 0 AND scheduler_version >= 0 AND shard >= 0)
    )",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__window_schedule__due
        ON bss_rating__window_schedule (shard, next_due_at) WHERE state <> 'complete'",
    r"CREATE TABLE IF NOT EXISTS bss_rating__usage_policy_projection (
        tenant_id uuid NOT NULL,
        subscription_id uuid NOT NULL,
        item_id uuid NOT NULL,
        valid_from timestamptz NOT NULL,
        fact_version bigint NOT NULL,
        valid_to timestamptz NOT NULL,
        fact_id uuid NOT NULL,
        sub_line_key text NOT NULL,
        policy_id uuid NOT NULL,
        policy_version bigint NOT NULL,
        policy_digest text NOT NULL,
        rating_window text NOT NULL,
        aggregation_scope text NOT NULL,
        meter text NOT NULL,
        inputs jsonb NOT NULL,
        binding_digest text NOT NULL,
        created_at timestamptz NOT NULL,
        PRIMARY KEY (tenant_id, subscription_id, item_id, valid_from, fact_version),
        CONSTRAINT ck_bss_rating__usage_policy_projection__valid CHECK (valid_from < valid_to),
        CONSTRAINT ck_bss_rating__usage_policy_projection__window CHECK (rating_window IN
            ('billing_cycle', 'calendar_hour')),
        CONSTRAINT ck_bss_rating__usage_policy_projection__scope CHECK (aggregation_scope IN
            ('subscription_line', 'resource')),
        CONSTRAINT ck_bss_rating__usage_policy_projection__digest CHECK
            (binding_digest ~ '^[0-9a-f]{64}$')
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__pricing_binding (
        tenant_id uuid NOT NULL,
        binding_digest text NOT NULL,
        plan_id uuid NOT NULL,
        revision_id uuid NOT NULL,
        cells jsonb NOT NULL,
        first_fetched_at timestamptz NOT NULL,
        PRIMARY KEY (tenant_id, binding_digest),
        CONSTRAINT ck_bss_rating__pricing_binding__digest CHECK (binding_digest ~ '^[0-9a-f]{64}$')
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__pricing_resolve (
        tenant_id uuid NOT NULL,
        query_digest text NOT NULL,
        fetched_on date NOT NULL,
        catalog_tenant_id uuid NOT NULL,
        revision_id uuid NOT NULL,
        resolve_date date NOT NULL,
        item_id uuid NULL,
        pins jsonb NOT NULL,
        binding_digest text NOT NULL,
        fetched_at timestamptz NOT NULL,
        PRIMARY KEY (tenant_id, query_digest, fetched_on),
        CONSTRAINT ck_bss_rating__pricing_resolve__digests CHECK (query_digest ~ '^[0-9a-f]{64}$'
            AND binding_digest ~ '^[0-9a-f]{64}$')
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__derived_declaration (
        tenant_id uuid NOT NULL,
        code text NOT NULL,
        version bigint NOT NULL,
        digest text NOT NULL,
        declaration jsonb NOT NULL,
        fetched_at timestamptz NOT NULL,
        PRIMARY KEY (tenant_id, code, version),
        CONSTRAINT ck_bss_rating__derived_declaration__version CHECK (version >= 1),
        CONSTRAINT ck_bss_rating__derived_declaration__digest CHECK (digest ~ '^[0-9a-f]{64}$')
    )",
];

/// SQLite cannot alter a check, so the inbox is rebuilt with the widened reason check and its
/// rows copied; every other statement is PostgreSQL's with SQLite's types.
const SQLITE_UP: &[&str] = &[
    r"CREATE TABLE bss_rating__inbox_next (
        entry_id text PRIMARY KEY,
        tenant_id text NOT NULL,
        source text NOT NULL,
        business_id text NOT NULL,
        version integer NOT NULL,
        payload_digest text NOT NULL,
        payload text NOT NULL,
        received_via text NOT NULL,
        state text NOT NULL,
        quarantine_reason text NULL,
        conflict_of text NULL,
        received_at text NOT NULL,
        applied_at text NULL,
        CONSTRAINT ck_bss_rating__inbox__source CHECK (source IN
            ('fact', 'segment', 'scope', 'coverage', 'subscription_version', 'billing_hint')),
        CONSTRAINT ck_bss_rating__inbox__received_via CHECK (received_via IN ('pull', 'event')),
        CONSTRAINT ck_bss_rating__inbox__state CHECK (state IN
            ('accepted', 'applied', 'quarantined', 'discarded')),
        CONSTRAINT ck_bss_rating__inbox__version CHECK (version >= 0),
        CONSTRAINT ck_bss_rating__inbox__business_id CHECK (length(business_id) BETWEEN 1 AND 255),
        CONSTRAINT ck_bss_rating__inbox__digest CHECK (length(payload_digest) = 64),
        CONSTRAINT ck_bss_rating__inbox__quarantine_reason CHECK (quarantine_reason IS NULL
            OR quarantine_reason IN ('digest_conflict', 'unsupported_schema_version',
                'instant_not_whole_second', 'payload_invalid')),
        CONSTRAINT ck_bss_rating__inbox__quarantined_has_reason CHECK (state <> 'quarantined'
            OR quarantine_reason IS NOT NULL),
        CONSTRAINT ck_bss_rating__inbox__conflict CHECK (
            (COALESCE(quarantine_reason, '') = 'digest_conflict') = (conflict_of IS NOT NULL)),
        CONSTRAINT ck_bss_rating__inbox__applied_at CHECK ((state = 'applied') = (applied_at IS NOT NULL))
    )",
    r"INSERT INTO bss_rating__inbox_next (entry_id, tenant_id, source, business_id, version,
        payload_digest, payload, received_via, state, quarantine_reason, conflict_of, received_at,
        applied_at)
        SELECT entry_id, tenant_id, source, business_id, version, payload_digest, payload,
            received_via, state, quarantine_reason, conflict_of, received_at, applied_at
        FROM bss_rating__inbox",
    "DROP TABLE bss_rating__inbox",
    "ALTER TABLE bss_rating__inbox_next RENAME TO bss_rating__inbox",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__inbox__key_digest
        ON bss_rating__inbox (tenant_id, source, business_id, version, payload_digest)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__inbox__primary
        ON bss_rating__inbox (tenant_id, source, business_id, version) WHERE conflict_of IS NULL",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__inbox__quarantined
        ON bss_rating__inbox (tenant_id, received_at) WHERE state = 'quarantined'",
    r"CREATE TABLE IF NOT EXISTS bss_rating__fact (
        tenant_id text NOT NULL,
        fact_id text NOT NULL,
        fact_version integer NOT NULL,
        entry_id text NOT NULL,
        kind text NOT NULL,
        obligation text NOT NULL,
        predecessor_version integer NULL,
        subscription_id text NOT NULL,
        sub_line_key text NOT NULL,
        item_id text NOT NULL,
        billing_group_id text NOT NULL,
        seller_tenant_id text NOT NULL,
        payer_tenant_id text NOT NULL,
        resource_tenant_id text NOT NULL,
        currency text NOT NULL,
        period_start text NOT NULL,
        period_end text NOT NULL,
        served_from text NOT NULL,
        served_to text NOT NULL,
        timing text NOT NULL,
        occurrence_id text NULL,
        billing_terms text NOT NULL,
        suspensions text NOT NULL,
        timeline text NOT NULL,
        accepted_terms text NOT NULL,
        producer_digest text NOT NULL,
        payload_digest text NOT NULL,
        payload text NOT NULL,
        received_at text NOT NULL,
        PRIMARY KEY (tenant_id, fact_id, fact_version),
        CONSTRAINT ck_bss_rating__fact__kind CHECK (kind IN ('recurring', 'usage', 'one_time')),
        CONSTRAINT ck_bss_rating__fact__obligation CHECK (obligation IN ('active', 'zeroed')),
        CONSTRAINT ck_bss_rating__fact__timing CHECK (timing IN ('advance', 'arrears')),
        CONSTRAINT ck_bss_rating__fact__version CHECK (fact_version >= 1),
        CONSTRAINT ck_bss_rating__fact__seller CHECK (seller_tenant_id = tenant_id),
        CONSTRAINT ck_bss_rating__fact__producer_digest CHECK (length(producer_digest) = 64),
        CONSTRAINT ck_bss_rating__fact__digest CHECK (length(payload_digest) = 64)
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__fact_head (
        tenant_id text NOT NULL,
        fact_id text NOT NULL,
        shard integer NOT NULL,
        kind text NOT NULL,
        current_version integer NOT NULL,
        binding_state text NOT NULL,
        binding_reason text NULL,
        bound_version integer NULL,
        binding_next_check_at text NULL,
        expected_manifest_digest text NULL,
        rollup_due_at text NULL,
        currency text NOT NULL,
        received_at text NOT NULL,
        updated_at text NOT NULL,
        PRIMARY KEY (tenant_id, fact_id),
        CONSTRAINT ck_bss_rating__fact_head__shard CHECK (shard >= 0),
        CONSTRAINT ck_bss_rating__fact_head__kind CHECK (kind IN ('recurring', 'usage', 'one_time')),
        CONSTRAINT ck_bss_rating__fact_head__state CHECK (binding_state IN
            ('pending', 'bound', 'failed')),
        CONSTRAINT ck_bss_rating__fact_head__reason CHECK ((binding_state = 'bound')
            = (binding_reason IS NULL) AND (binding_reason IS NULL OR binding_reason IN
            ('derived_declaration_unavailable', 'derived_declaration_conflict'))),
        CONSTRAINT ck_bss_rating__fact_head__retry CHECK (binding_next_check_at IS NULL
            OR binding_state = 'pending'),
        CONSTRAINT ck_bss_rating__fact_head__bound CHECK (binding_state = 'bound'
            OR (expected_manifest_digest IS NULL AND rollup_due_at IS NULL))
    )",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__fact_head__rollup
        ON bss_rating__fact_head (shard, rollup_due_at) WHERE rollup_due_at IS NOT NULL",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__fact_head__binding
        ON bss_rating__fact_head (shard, binding_next_check_at) WHERE binding_state = 'pending'",
    r"CREATE TABLE IF NOT EXISTS bss_rating__child_window (
        child_id text PRIMARY KEY,
        tenant_id text NOT NULL,
        fact_id text NOT NULL,
        shard integer NOT NULL,
        child_kind text NOT NULL,
        subscription_id text NOT NULL,
        item_id text NOT NULL,
        dimension_value text NULL,
        aggregation_key text NOT NULL,
        agg_key_digest text NOT NULL,
        window_group_id text NULL,
        window_start text NOT NULL,
        window_end text NOT NULL,
        served_from text NOT NULL,
        served_to text NOT NULL,
        status text NOT NULL,
        pending_reason text NULL,
        failure_reason text NULL,
        next_check_at text NULL,
        input_generation integer NOT NULL,
        current_revision integer NULL,
        current_covered_generation integer NULL,
        current_input_digest text NULL,
        binding_of_record text NULL,
        binding_resolves text NULL,
        engine_of_record integer NULL,
        requested_rebind integer NOT NULL,
        requested_engine integer NULL,
        requested_run_id text NULL,
        provisional_outcome text NULL,
        last_reason_code text NULL,
        queued_at text NULL,
        created_at text NOT NULL,
        updated_at text NOT NULL,
        CONSTRAINT ck_bss_rating__child_window__kind CHECK (child_kind IN
            ('usage_window', 'period_line', 'one_time')),
        CONSTRAINT ck_bss_rating__child_window__status CHECK (status IN
            ('pending', 'provisional', 'final', 'final_stale', 'failed')),
        CONSTRAINT ck_bss_rating__child_window__failure CHECK (failure_reason IS NULL
            OR status = 'failed'),
        CONSTRAINT ck_bss_rating__child_window__generation CHECK (input_generation >= 0),
        CONSTRAINT ck_bss_rating__child_window__shard CHECK (shard >= 0),
        CONSTRAINT ck_bss_rating__child_window__agg CHECK (length(agg_key_digest) = 64)
    )",
    r"CREATE UNIQUE INDEX IF NOT EXISTS uq_bss_rating__child_window__window_key
        ON bss_rating__child_window (tenant_id, fact_id, window_start, agg_key_digest)",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__child_window__due
        ON bss_rating__child_window (shard, next_check_at)
        WHERE status IN ('pending', 'provisional', 'final_stale') AND next_check_at IS NOT NULL",
    r"CREATE TABLE IF NOT EXISTS bss_rating__window_schedule (
        tenant_id text NOT NULL,
        fact_id text NOT NULL,
        item_id text NOT NULL,
        shard integer NOT NULL,
        state text NOT NULL,
        fact_version integer NOT NULL,
        window_policy text NOT NULL,
        usage_policy_id text NOT NULL,
        usage_policy_version integer NOT NULL,
        usage_policy_digest text NOT NULL,
        finalization_policy_id text NULL,
        finalization_policy_version integer NULL,
        finalization_policy_owner_scope text NULL,
        delay_seconds integer NULL,
        evidence_mode text NULL,
        next_due_window_start text NULL,
        next_due_at text NOT NULL,
        expected_count integer NOT NULL,
        scheduler_version integer NOT NULL,
        updated_at text NOT NULL,
        PRIMARY KEY (tenant_id, fact_id, item_id),
        CONSTRAINT ck_bss_rating__window_schedule__state CHECK (state IN
            ('policy_pending', 'scheduling', 'complete')),
        CONSTRAINT ck_bss_rating__window_schedule__window CHECK (window_policy IN
            ('billing_cycle', 'calendar_hour')),
        CONSTRAINT ck_bss_rating__window_schedule__pinned CHECK ((state = 'policy_pending')
            = (finalization_policy_id IS NULL)),
        CONSTRAINT ck_bss_rating__window_schedule__counts CHECK (fact_version >= 1
            AND expected_count >= 0 AND scheduler_version >= 0 AND shard >= 0)
    )",
    r"CREATE INDEX IF NOT EXISTS idx_bss_rating__window_schedule__due
        ON bss_rating__window_schedule (shard, next_due_at) WHERE state <> 'complete'",
    r"CREATE TABLE IF NOT EXISTS bss_rating__usage_policy_projection (
        tenant_id text NOT NULL,
        subscription_id text NOT NULL,
        item_id text NOT NULL,
        valid_from text NOT NULL,
        fact_version integer NOT NULL,
        valid_to text NOT NULL,
        fact_id text NOT NULL,
        sub_line_key text NOT NULL,
        policy_id text NOT NULL,
        policy_version integer NOT NULL,
        policy_digest text NOT NULL,
        rating_window text NOT NULL,
        aggregation_scope text NOT NULL,
        meter text NOT NULL,
        inputs text NOT NULL,
        binding_digest text NOT NULL,
        created_at text NOT NULL,
        PRIMARY KEY (tenant_id, subscription_id, item_id, valid_from, fact_version),
        CONSTRAINT ck_bss_rating__usage_policy_projection__window CHECK (rating_window IN
            ('billing_cycle', 'calendar_hour')),
        CONSTRAINT ck_bss_rating__usage_policy_projection__scope CHECK (aggregation_scope IN
            ('subscription_line', 'resource'))
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__pricing_binding (
        tenant_id text NOT NULL,
        binding_digest text NOT NULL,
        plan_id text NOT NULL,
        revision_id text NOT NULL,
        cells text NOT NULL,
        first_fetched_at text NOT NULL,
        PRIMARY KEY (tenant_id, binding_digest),
        CONSTRAINT ck_bss_rating__pricing_binding__digest CHECK (length(binding_digest) = 64)
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__pricing_resolve (
        tenant_id text NOT NULL,
        query_digest text NOT NULL,
        fetched_on text NOT NULL,
        catalog_tenant_id text NOT NULL,
        revision_id text NOT NULL,
        resolve_date text NOT NULL,
        item_id text NULL,
        pins text NOT NULL,
        binding_digest text NOT NULL,
        fetched_at text NOT NULL,
        PRIMARY KEY (tenant_id, query_digest, fetched_on),
        CONSTRAINT ck_bss_rating__pricing_resolve__digests CHECK (length(query_digest) = 64
            AND length(binding_digest) = 64)
    )",
    r"CREATE TABLE IF NOT EXISTS bss_rating__derived_declaration (
        tenant_id text NOT NULL,
        code text NOT NULL,
        version integer NOT NULL,
        digest text NOT NULL,
        declaration text NOT NULL,
        fetched_at text NOT NULL,
        PRIMARY KEY (tenant_id, code, version),
        CONSTRAINT ck_bss_rating__derived_declaration__version CHECK (version >= 1),
        CONSTRAINT ck_bss_rating__derived_declaration__digest CHECK (length(digest) = 64)
    )",
];

const DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS bss_rating__derived_declaration",
    "DROP TABLE IF EXISTS bss_rating__pricing_resolve",
    "DROP TABLE IF EXISTS bss_rating__pricing_binding",
    "DROP TABLE IF EXISTS bss_rating__usage_policy_projection",
    "DROP TABLE IF EXISTS bss_rating__window_schedule",
    "DROP TABLE IF EXISTS bss_rating__child_window",
    "DROP TABLE IF EXISTS bss_rating__fact_head",
    "DROP TABLE IF EXISTS bss_rating__fact",
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

    /// Drops fact intake's tables. The inbox keeps its widened reason check: an entry quarantined
    /// for a refused payload must stay readable.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, super::Dialects::both(DOWN)).await
    }
}
